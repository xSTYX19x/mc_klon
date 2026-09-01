use super::{CHUNK_SIZE, WorldEnvironment};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WorldConfig {
    pub(crate) seed: u64,
    pub(crate) sea_level: i32,
    pub(crate) min_y: i32,
    pub(crate) max_y: i32,
    pub(crate) chunk_size: i32,
    pub(crate) boundary_ocean: bool,
    pub(crate) boundary_radius_chunks: i32,
    pub(crate) environment: WorldEnvironment,
}

impl WorldConfig {
    pub(crate) fn sea_level(&self) -> i32 {
        self.sea_level
    }

    pub(crate) fn min_y(&self) -> i32 {
        self.min_y
    }

    pub(crate) fn max_y(&self) -> i32 {
        self.max_y
    }

    pub(crate) fn boundary_ocean(&self) -> bool {
        self.boundary_ocean
    }

    pub(crate) fn boundary_radius_chunks(&self) -> i32 {
        self.boundary_radius_chunks.max(0)
    }

    pub(crate) fn environment(&self) -> WorldEnvironment {
        self.environment
    }
}

impl Default for WorldConfig {
    fn default() -> Self {
        Self {
            seed: 132_211,
            sea_level: 64,
            min_y: 0,
            max_y: 255,
            chunk_size: CHUNK_SIZE,
            boundary_ocean: false,
            boundary_radius_chunks: 128,
            environment: WorldEnvironment::Temperate,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::WorldConfig;
    use crate::blocks::BlockId;
    use crate::render::chunkgen_worker::generate_streaming_column_for_test;
    use crate::worldgen::WorldgenPreset;

    fn column_feature_extrema(
        cfg: WorldConfig,
        preset: WorldgenPreset,
        cx: i32,
        cz: i32,
    ) -> (Option<i32>, Option<i32>) {
        let column = generate_streaming_column_for_test(cfg, preset, cx, cz);
        let mut max_water_y = None;
        let mut max_lava_y = None;
        for y in cfg.min_y()..=cfg.max_y() {
            for z in 0..16 {
                for x in 0..16 {
                    match column.get_block(x, y, z) {
                        BlockId::WaterStill | BlockId::WaterFlowing => {
                            max_water_y = Some(max_water_y.map_or(y, |v: i32| v.max(y)));
                        }
                        BlockId::LavaStill | BlockId::LavaFlowing => {
                            max_lava_y = Some(max_lava_y.map_or(y, |v: i32| v.max(y)));
                        }
                        _ => {}
                    }
                }
            }
        }
        (max_water_y, max_lava_y)
    }

    /// Scans a fixed square of columns and returns the highest water / lava
    /// block found anywhere in it.
    fn grid_feature_extrema(
        cfg: WorldConfig,
        preset: WorldgenPreset,
        ring: i32,
    ) -> (Option<i32>, Option<i32>) {
        let mut max_water_y = None;
        let mut max_lava_y = None;
        for cz in -ring..=ring {
            for cx in -ring..=ring {
                let (water, lava) = column_feature_extrema(cfg, preset, cx, cz);
                if let Some(w) = water {
                    max_water_y = Some(max_water_y.map_or(w, |v: i32| v.max(w)));
                }
                if let Some(l) = lava {
                    max_lava_y = Some(max_lava_y.map_or(l, |v: i32| v.max(l)));
                }
            }
        }
        (max_water_y, max_lava_y)
    }

    /// `sea_level` is a worldgen *input*, not a ceiling for water. Springs are
    /// deliberately sourced on hillsides above it (`WATER_RIVER_SOURCE_MIN/MAX_ABOVE_SEA`
    /// in `render::chunkgen_worker`) and water lakes follow the terrain surface,
    /// so water above sea level is generated on purpose. What sea level does
    /// bound is lava: `make_lava_lake_candidate` clamps every lava lake to
    /// `sea_level - 2`.
    ///
    /// The test runs two sea levels so the bound cannot be satisfied by a
    /// hard-coded constant: the high-sea run must place lava above the low-sea
    /// run's ceiling, which only holds if the value is threaded through from
    /// `WorldConfig`.
    #[test]
    fn sea_level_from_world_config_bounds_lava_while_springs_may_start_above_it() {
        const LOW_SEA_LEVEL: i32 = 40;
        const HIGH_SEA_LEVEL: i32 = 80;
        const RING: i32 = 4;

        let preset = WorldgenPreset::Rd161348Like;
        let base = WorldConfig {
            seed: 0x0BAD_5EED_CAFE_BABE,
            ..WorldConfig::default()
        };

        let mut observed = Vec::new();
        for sea_level in [LOW_SEA_LEVEL, HIGH_SEA_LEVEL] {
            let cfg = WorldConfig { sea_level, ..base };
            let (max_water_y, max_lava_y) = grid_feature_extrema(cfg, preset, RING);
            let max_lava_y = max_lava_y.unwrap_or_else(|| {
                panic!("no lava feature within ring {RING} for sea_level={sea_level}")
            });
            assert!(
                max_lava_y <= sea_level - 2,
                "lava feature above the sea-level ceiling: lava_y={max_lava_y}, sea_level={sea_level}"
            );
            observed.push((sea_level, max_water_y, max_lava_y));
        }

        let (_, _, low_lava_y) = observed[0];
        let (_, high_water_y, high_lava_y) = observed[1];

        assert!(
            high_lava_y > low_lava_y,
            "lava ceiling does not track sea_level: sea {LOW_SEA_LEVEL} -> lava_y={low_lava_y}, \
             sea {HIGH_SEA_LEVEL} -> lava_y={high_lava_y}"
        );

        // Springs/lakes above sea level are intended; if this ever stops holding,
        // water placement was clamped to sea level and the surface features are gone.
        let high_water_y = high_water_y.expect("no water feature within scanned ring");
        assert!(
            high_water_y > HIGH_SEA_LEVEL,
            "no water generated above sea level any more: water_y={high_water_y}, \
             sea_level={HIGH_SEA_LEVEL}"
        );
    }
}
