use super::noise::NoiseSampler;

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorldgenPreset {
    Flatish,
    ClassicFlatter,
    Rd161348Like,
    Hilly,
}

pub const DEFAULT_WORLDGEN_PRESET: WorldgenPreset = WorldgenPreset::Rd161348Like;

/// Konfiguration fuer 2D-Heightmap Terrain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldGenConfig {
    pub seed: u32,
    pub sea_level: i32,
    pub base_height: i32,
    pub height_amp: f32,
    pub freq: f32,
    pub octaves: u8,
    pub lacunarity: f32,
    pub gain: f32,
    pub dirt_depth: i32,
    pub warp_freq: f32,
    pub warp_strength: f32,
    pub cliff_freq: f32,
    pub cliff_amp: f32,
    pub local_hill_freq_mul: f32,
    pub local_hill_weight: f32,
    pub ridge_weight: f32,
    pub valley_floor_offset: i32,
}

impl WorldGenConfig {
    pub fn from_preset(seed: u32, preset: WorldgenPreset) -> Self {
        Self::from_preset_with_sea_level(seed, preset, 64)
    }

    pub fn from_preset_with_sea_level(seed: u32, preset: WorldgenPreset, sea_level: i32) -> Self {
        match preset {
            WorldgenPreset::Flatish => Self {
                seed,
                sea_level,
                base_height: 78,
                height_amp: 14.0,
                freq: 0.0094,
                octaves: 4,
                lacunarity: 2.0,
                gain: 0.50,
                dirt_depth: 3,
                warp_freq: 0.0070,
                warp_strength: 5.2,
                cliff_freq: 0.028,
                cliff_amp: 0.0,
                local_hill_freq_mul: 2.9,
                local_hill_weight: 0.46,
                ridge_weight: 0.20,
                valley_floor_offset: 12,
            },
            WorldgenPreset::ClassicFlatter => Self {
                seed,
                sea_level,
                // Etwas flacher als rd161348_like, aber weiterhin mit klarer Silhouette.
                base_height: 84,
                height_amp: 20.0,
                freq: 0.0104,
                octaves: 4,
                lacunarity: 2.0,
                gain: 0.52,
                dirt_depth: 3,
                warp_freq: 0.0078,
                warp_strength: 6.6,
                cliff_freq: 0.034,
                // Deutlich weniger steile Cliffs.
                cliff_amp: 2.8,
                local_hill_freq_mul: 2.35,
                local_hill_weight: 0.35,
                ridge_weight: 0.14,
                valley_floor_offset: 10,
            },
            WorldgenPreset::Rd161348Like => Self {
                seed,
                sea_level,
                // Mehr Headroom für aktive Cliffs ohne dauerhaftes Top-Clipping.
                base_height: 86,
                height_amp: 26.0,
                freq: 0.0108,
                octaves: 4,
                lacunarity: 2.0,
                gain: 0.52,
                dirt_depth: 3,
                // Etwas stärkeres Warp gegen lineare Rampen bei steileren Profilen.
                warp_freq: 0.0078,
                warp_strength: 7.2,
                cliff_freq: 0.034,
                // Aktiv: selektiv häufigere Steilhänge.
                cliff_amp: 9.8,
                local_hill_freq_mul: 2.9,
                local_hill_weight: 0.46,
                ridge_weight: 0.20,
                valley_floor_offset: 12,
            },
            WorldgenPreset::Hilly => Self {
                seed,
                sea_level,
                base_height: 90,
                height_amp: 34.0,
                freq: 0.0118,
                octaves: 5,
                lacunarity: 2.0,
                gain: 0.54,
                dirt_depth: 3,
                warp_freq: 0.0082,
                warp_strength: 8.0,
                cliff_freq: 0.038,
                cliff_amp: 13.0,
                local_hill_freq_mul: 2.9,
                local_hill_weight: 0.46,
                ridge_weight: 0.20,
                valley_floor_offset: 12,
            },
        }
    }
}

impl Default for WorldGenConfig {
    fn default() -> Self {
        Self::from_preset(132_211, DEFAULT_WORLDGEN_PRESET)
    }
}

/// TerrainSampler mappt (world_x, world_z) deterministisch auf eine Oberflaechenhoehe.
/// Es werden immer Weltkoordinaten verwendet, damit Chunk-Grenzen nahtlos bleiben.
pub struct TerrainSampler {
    cfg: WorldGenConfig,
    noise: NoiseSampler,
}

impl TerrainSampler {
    pub fn new(cfg: WorldGenConfig) -> Self {
        Self {
            noise: NoiseSampler::new(cfg.seed),
            cfg,
        }
    }

    pub fn config(&self) -> &WorldGenConfig {
        &self.cfg
    }

    pub fn height_at(&self, world_x: i32, world_z: i32, world_max_y: i32) -> i32 {
        let x = world_x as f32;
        let z = world_z as f32;
        let base_height = self.cfg.base_height as f32;

        // Domain-Warping:
        // niedrige Frequenz verschiebt die Sample-Positionen und bricht
        // lineare/terrassierte Muster auf.
        let warp_x = self
            .noise
            .fbm2(x + 1450.0, z - 3020.0, self.cfg.warp_freq, 3, 2.0, 0.5)
            * self.cfg.warp_strength;
        let warp_z = self
            .noise
            .fbm2(x - 2710.0, z + 910.0, self.cfg.warp_freq, 3, 2.0, 0.5)
            * self.cfg.warp_strength;
        let wx = x + warp_x;
        let wz = z + warp_z;

        // 1) Macro-Noise (breite Hügel),
        // 2) sehr tieffrequenter Trend (wird teilweise abgezogen),
        // damit keine einseitige Großrampe über große Kartenanteile entsteht.
        let macro_raw = self.noise.fbm2(
            wx,
            wz,
            self.cfg.freq * 0.82,
            self.cfg.octaves,
            self.cfg.lacunarity,
            self.cfg.gain,
        );
        let macro_trend =
            self.noise
                .fbm2(wx - 6000.0, wz + 2200.0, self.cfg.freq * 0.23, 2, 2.0, 0.5);
        let macro_hills = (macro_raw - macro_trend * 0.72).clamp(-1.0, 1.0);

        // Lokale Hügelstruktur.
        let local_hills = self.noise.fbm2(
            wx + 4096.0,
            wz - 2048.0,
            self.cfg.freq * self.cfg.local_hill_freq_mul,
            4,
            self.cfg.lacunarity,
            0.55,
        );
        // Ridge-Term bringt Rücken und Täler ohne gleichmäßige Terrassen.
        let ridge_src = self.noise.fbm2(
            wx - 8192.0,
            wz + 4096.0,
            self.cfg.freq * 2.0,
            3,
            self.cfg.lacunarity,
            0.56,
        );
        let ridges = 1.0 - ridge_src.abs();
        // Feindetail, klein gehalten.
        let detail = self.noise.fbm2(
            wx + 1337.0,
            wz + 7331.0,
            self.cfg.freq * 4.0,
            2,
            self.cfg.lacunarity,
            0.56,
        );

        // Mischfeld + nichtlineare Schärfung:
        // ergibt erkennbare Kuppen statt nur weicher Schrägen.
        let hill_field = macro_hills * 0.70
            + local_hills * self.cfg.local_hill_weight
            + (ridges * 2.0 - 1.0) * self.cfg.ridge_weight;
        let hill_shaped = hill_field * 0.78 + hill_field.powi(3) * 0.16;

        let mut height =
            base_height + hill_shaped * self.cfg.height_amp + detail * (self.cfg.height_amp * 0.07);

        // Sea-level bleibt Referenzwert gegen zu tiefe Täler.
        height = height.max((self.cfg.sea_level - self.cfg.valley_floor_offset) as f32);

        if self.cfg.cliff_amp > 0.0 {
            // Optionaler Cliff-Effekt; standardmäßig deaktiviert.
            let cliff_src = self
                .noise
                .cliff2(wx + 333.0, wz - 777.0, self.cfg.cliff_freq)
                .abs();
            let cliff_mask = ((cliff_src - 0.72) / 0.28).clamp(0.0, 1.0).powf(2.4);
            let cliff_drive = hill_field.abs().max(local_hills.abs());
            height += cliff_mask * cliff_drive * self.cfg.cliff_amp;
        }

        // Leichte Kompression der Extreme, damit die Silhouette natürlich bleibt.
        let delta = height - base_height;
        height = base_height + delta * 0.96;

        height.round().clamp(0.0, world_max_y as f32) as i32
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_WORLDGEN_PRESET, TerrainSampler, WorldGenConfig, WorldgenPreset};

    fn gradient_stats(sampler: &TerrainSampler, size: i32, max_y: i32) -> (f32, f32) {
        let mut sum_abs_grad = 0.0_f32;
        let mut grad_samples = 0usize;
        let mut steep_edges = 0usize;

        for z in 0..size {
            for x in 0..size {
                let h = sampler.height_at(x, z, max_y);
                if x + 1 < size {
                    let hx = sampler.height_at(x + 1, z, max_y);
                    let d = (hx - h).abs();
                    sum_abs_grad += d as f32;
                    if d >= 2 {
                        steep_edges += 1;
                    }
                    grad_samples += 1;
                }
                if z + 1 < size {
                    let hz = sampler.height_at(x, z + 1, max_y);
                    let d = (hz - h).abs();
                    sum_abs_grad += d as f32;
                    if d >= 2 {
                        steep_edges += 1;
                    }
                    grad_samples += 1;
                }
            }
        }

        let avg_abs_grad = sum_abs_grad / (grad_samples as f32);
        let steep_ratio = (steep_edges as f32) / (grad_samples as f32);
        (avg_abs_grad, steep_ratio)
    }

    #[test]
    fn default_profile_has_hills_without_top_clipping() {
        let sampler = TerrainSampler::new(WorldGenConfig::default());

        let mut min_h = i32::MAX;
        let mut max_h = i32::MIN;
        let mut clipped_top = 0usize;
        let mut sum_abs_dx = 0.0_f32;
        let mut sum_abs_dz = 0.0_f32;
        let mut grad_samples = 0usize;

        const SIZE: i32 = 256;
        const MAX_Y: i32 = 255;

        for z in 0..SIZE {
            for x in 0..SIZE {
                let h = sampler.height_at(x, z, MAX_Y);
                min_h = min_h.min(h);
                max_h = max_h.max(h);
                if h == MAX_Y {
                    clipped_top += 1;
                }

                if x + 1 < SIZE {
                    let hx = sampler.height_at(x + 1, z, MAX_Y);
                    sum_abs_dx += (hx - h).abs() as f32;
                    grad_samples += 1;
                }
                if z + 1 < SIZE {
                    let hz = sampler.height_at(x, z + 1, MAX_Y);
                    sum_abs_dz += (hz - h).abs() as f32;
                    grad_samples += 1;
                }
            }
        }

        let range = max_h - min_h;
        let clipped_ratio = clipped_top as f32 / ((SIZE * SIZE) as f32);
        let avg_abs_grad = (sum_abs_dx + sum_abs_dz) / (grad_samples as f32);

        assert!(
            min_h >= 52,
            "terrain dips too far below sea-level band unexpectedly: min_h={}",
            min_h
        );
        assert!(
            max_h >= 72,
            "terrain should commonly sit above sea level (64): max_h={}",
            max_h
        );
        assert!(
            range >= 16,
            "height range too small for visible hills: {}",
            range
        );
        assert!(
            clipped_ratio <= 0.01,
            "too much top clipping/plateau: {:.3}",
            clipped_ratio
        );
        assert!(
            avg_abs_grad >= 0.35,
            "terrain too ramp-like/flat on average: {:.3}",
            avg_abs_grad
        );
    }

    #[test]
    fn default_profile_uses_rd161348_like_preset() {
        let expected = WorldGenConfig::from_preset(132_211, DEFAULT_WORLDGEN_PRESET);
        assert_eq!(expected, WorldGenConfig::default());
        assert_eq!(DEFAULT_WORLDGEN_PRESET, WorldgenPreset::Rd161348Like);
    }

    #[test]
    fn rd161348_like_preset_enables_active_cliffs() {
        let cfg = WorldGenConfig::from_preset(1234, WorldgenPreset::Rd161348Like);
        assert!(cfg.cliff_amp >= 7.0, "cliff_amp too low: {}", cfg.cliff_amp);
        assert!(
            cfg.cliff_amp <= 14.0,
            "cliff_amp too high: {}",
            cfg.cliff_amp
        );
        assert!(
            cfg.warp_strength >= 6.5,
            "warp_strength should stay high enough against ramps: {}",
            cfg.warp_strength
        );
    }

    #[test]
    fn rd161348_like_is_steeper_than_flatish() {
        const SIZE: i32 = 192;
        const MAX_Y: i32 = 255;

        let flat = TerrainSampler::new(WorldGenConfig::from_preset(
            132_211,
            WorldgenPreset::Flatish,
        ));
        let rd = TerrainSampler::new(WorldGenConfig::from_preset(
            132_211,
            WorldgenPreset::Rd161348Like,
        ));
        let (flat_grad, flat_steep) = gradient_stats(&flat, SIZE, MAX_Y);
        let (rd_grad, rd_steep) = gradient_stats(&rd, SIZE, MAX_Y);

        assert!(
            rd_grad >= flat_grad + 0.06,
            "rd161348_like not steeper enough (avg grad flat={:.3}, rd={:.3})",
            flat_grad,
            rd_grad
        );
        assert!(
            rd_steep > flat_steep,
            "rd161348_like not producing more steep edges (flat={:.3}, rd={:.3})",
            flat_steep,
            rd_steep
        );
    }

    #[test]
    fn classic_flatter_is_less_steep_than_rd161348_like() {
        const SIZE: i32 = 192;
        const MAX_Y: i32 = 255;

        let flatter = TerrainSampler::new(WorldGenConfig::from_preset(
            132_211,
            WorldgenPreset::ClassicFlatter,
        ));
        let rd = TerrainSampler::new(WorldGenConfig::from_preset(
            132_211,
            WorldgenPreset::Rd161348Like,
        ));
        let (flatter_grad, flatter_steep) = gradient_stats(&flatter, SIZE, MAX_Y);
        let (rd_grad, rd_steep) = gradient_stats(&rd, SIZE, MAX_Y);

        assert!(
            rd_grad >= flatter_grad + 0.04,
            "classic_flatter not flatter enough (avg grad flatter={:.3}, rd={:.3})",
            flatter_grad,
            rd_grad
        );
        assert!(
            rd_steep > flatter_steep,
            "classic_flatter did not reduce steep edges (flatter={:.3}, rd={:.3})",
            flatter_steep,
            rd_steep
        );
    }

    #[test]
    fn classic_flatter_has_relief_without_top_clipping() {
        const SIZE: i32 = 192;
        const MAX_Y: i32 = 255;

        let sampler = TerrainSampler::new(WorldGenConfig::from_preset(
            132_211,
            WorldgenPreset::ClassicFlatter,
        ));
        let mut min_h = i32::MAX;
        let mut max_h = i32::MIN;
        let mut clipped_top = 0usize;

        for z in 0..SIZE {
            for x in 0..SIZE {
                let h = sampler.height_at(x, z, MAX_Y);
                min_h = min_h.min(h);
                max_h = max_h.max(h);
                if h == MAX_Y {
                    clipped_top += 1;
                }
            }
        }

        let range = max_h - min_h;
        let clipped_ratio = clipped_top as f32 / ((SIZE * SIZE) as f32);

        assert!(
            range >= 14,
            "classic_flatter lost too much macro variation: range={}",
            range
        );
        assert!(
            clipped_ratio <= 0.01,
            "classic_flatter has too much top clipping: {:.3}",
            clipped_ratio
        );
    }
}
