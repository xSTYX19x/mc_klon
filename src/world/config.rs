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

    #[test]
    fn sea_level_comes_from_world_config_for_lakes_and_lava_constraints() {
        let cfg = WorldConfig {
            seed: 0x0BAD_5EED_CAFE_BABE,
            sea_level: 80,
            ..WorldConfig::default()
        };
        let preset = WorldgenPreset::Rd161348Like;

        let mut chosen: Option<(i32, i32, i32, i32)> = None;
        for ring in 0..=20 {
            for cz in -ring..=ring {
                for cx in -ring..=ring {
                    let (max_water_y, max_lava_y) = column_feature_extrema(cfg, preset, cx, cz);
                    if let (Some(w), Some(l)) = (max_water_y, max_lava_y) {
                        chosen = Some((cx, cz, w, l));
                        break;
                    }
                }
                if chosen.is_some() {
                    break;
                }
            }
            if chosen.is_some() {
                break;
            }
        }

        let (cx, cz, max_water_y, max_lava_y) = chosen
            .unwrap_or_else(|| panic!("no test column with water + lava features found for seed"));
        assert!(
            max_water_y <= cfg.sea_level() - 1,
            "water feature above configured sea level in column ({cx},{cz}): water_y={}, sea_level={}",
            max_water_y,
            cfg.sea_level()
        );
        assert!(
            max_lava_y <= cfg.sea_level() - 2,
            "lava feature violated sea-level constraint in column ({cx},{cz}): lava_y={}, sea_level={}",
            max_lava_y,
            cfg.sea_level()
        );
    }
}
