use std::f32::consts::PI;

use crate::blocks::BlockId;
use crate::util::rng::{Rng64, hash64};
use crate::world::CHUNK_SIZE;
use crate::world::config::WorldConfig;

use super::{ColumnBuffer, WorldgenPreset};

const ORE_SOURCE_X_MIX: u64 = 0x9E37_79B9_7F4A_7C15;
const ORE_SOURCE_Z_MIX: u64 = 0xBF58_476D_1CE4_E5B9;
const ORE_SEED_FINAL_MIX: u64 = 0x94D0_49BB_1331_11EB;

const REPLACE_STONE: [BlockId; 1] = [BlockId::Stone];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OreReplaceRule {
    AnyOf(&'static [BlockId]),
}

impl OreReplaceRule {
    fn matches(self, block: BlockId) -> bool {
        match self {
            Self::AnyOf(candidates) => candidates.contains(&block),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeightAnchor {
    Absolute(i32),
    AboveBottom(i32),
    BelowTop(i32),
}

impl HeightAnchor {
    fn resolve(self, world_cfg: WorldConfig) -> i32 {
        match self {
            Self::Absolute(value) => value,
            Self::AboveBottom(offset) => world_cfg.min_y().saturating_add(offset),
            Self::BelowTop(offset) => world_cfg.max_y().saturating_sub(offset),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OreHeightDistribution {
    Uniform {
        min: HeightAnchor,
        max: HeightAnchor,
    },
    Triangle {
        min: HeightAnchor,
        peak: HeightAnchor,
        max: HeightAnchor,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExposedOrePolicy {
    Allow,
    HiddenOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OreFeatureConfig {
    pub(crate) name: &'static str,
    pub(crate) ore_block: BlockId,
    pub(crate) replace_rule: OreReplaceRule,
    pub(crate) vein_size: u32,
    pub(crate) attempts_per_chunk: u32,
    pub(crate) chance_per_attempt_256: u32,
    pub(crate) height_distribution: OreHeightDistribution,
    pub(crate) exposed_policy: ExposedOrePolicy,
    pub(crate) salt: u64,
}

impl OreFeatureConfig {
    fn source_radius_chunks(self) -> i32 {
        (((self.vein_size as i32) + CHUNK_SIZE - 1) / CHUNK_SIZE).max(1)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct OreFeatureProfile {
    pub(crate) entries: &'static [OreFeatureConfig],
}

impl OreFeatureProfile {
    fn source_radius_chunks(self) -> i32 {
        self.entries
            .iter()
            .map(|entry| entry.source_radius_chunks())
            .max()
            .unwrap_or(0)
    }
}

const DEFAULT_ORE_TABLE: [OreFeatureConfig; 3] = [
    OreFeatureConfig {
        name: "coal_ore_upper",
        ore_block: BlockId::CoalOre,
        replace_rule: OreReplaceRule::AnyOf(&REPLACE_STONE),
        vein_size: 12,
        attempts_per_chunk: 18,
        chance_per_attempt_256: 192,
        height_distribution: OreHeightDistribution::Triangle {
            min: HeightAnchor::AboveBottom(0),
            peak: HeightAnchor::Absolute(96),
            max: HeightAnchor::BelowTop(48),
        },
        exposed_policy: ExposedOrePolicy::Allow,
        salt: 0x24C8_52FA_03D9_6A11,
    },
    OreFeatureConfig {
        name: "iron_ore_main",
        ore_block: BlockId::IronOre,
        replace_rule: OreReplaceRule::AnyOf(&REPLACE_STONE),
        vein_size: 9,
        attempts_per_chunk: 14,
        chance_per_attempt_256: 160,
        height_distribution: OreHeightDistribution::Triangle {
            min: HeightAnchor::AboveBottom(0),
            peak: HeightAnchor::Absolute(40),
            max: HeightAnchor::Absolute(136),
        },
        exposed_policy: ExposedOrePolicy::Allow,
        salt: 0x79E4_6AD1_5D36_BC23,
    },
    OreFeatureConfig {
        name: "gold_ore_lower",
        ore_block: BlockId::GoldOre,
        replace_rule: OreReplaceRule::AnyOf(&REPLACE_STONE),
        vein_size: 8,
        attempts_per_chunk: 6,
        chance_per_attempt_256: 104,
        height_distribution: OreHeightDistribution::Triangle {
            min: HeightAnchor::AboveBottom(0),
            peak: HeightAnchor::Absolute(18),
            max: HeightAnchor::Absolute(64),
        },
        exposed_policy: ExposedOrePolicy::Allow,
        salt: 0xA6F2_3D11_97CC_4E7B,
    },
];

pub(crate) fn ore_profile_for_preset(_preset: WorldgenPreset) -> OreFeatureProfile {
    OreFeatureProfile {
        entries: &DEFAULT_ORE_TABLE,
    }
}

pub(crate) fn apply_ores_to_column(
    column: &mut ColumnBuffer,
    world_cfg: WorldConfig,
    preset: WorldgenPreset,
    target_chunk_x: i32,
    target_chunk_z: i32,
) {
    let profile = ore_profile_for_preset(preset);
    let source_radius = profile.source_radius_chunks();
    for ore in profile.entries {
        for source_chunk_z in (target_chunk_z - source_radius)..=(target_chunk_z + source_radius) {
            for source_chunk_x in
                (target_chunk_x - source_radius)..=(target_chunk_x + source_radius)
            {
                apply_ore_from_source_chunk(
                    column,
                    world_cfg,
                    preset,
                    target_chunk_x,
                    target_chunk_z,
                    source_chunk_x,
                    source_chunk_z,
                    *ore,
                );
            }
        }
    }
}

fn apply_ore_from_source_chunk(
    column: &mut ColumnBuffer,
    world_cfg: WorldConfig,
    preset: WorldgenPreset,
    target_chunk_x: i32,
    target_chunk_z: i32,
    source_chunk_x: i32,
    source_chunk_z: i32,
    ore: OreFeatureConfig,
) {
    let mut rng = ore_rng(
        world_cfg.seed,
        preset,
        source_chunk_x,
        source_chunk_z,
        ore.salt,
    );
    for _ in 0..ore.attempts_per_chunk {
        if !chance_256(&mut rng, ore.chance_per_attempt_256) {
            continue;
        }
        let origin_x = source_chunk_x * CHUNK_SIZE + range_i32(&mut rng, 0, CHUNK_SIZE - 1);
        let origin_z = source_chunk_z * CHUNK_SIZE + range_i32(&mut rng, 0, CHUNK_SIZE - 1);
        let Some(origin_y) = ore.height_distribution.sample_y(world_cfg, &mut rng) else {
            continue;
        };
        carve_segmented_vein(
            column,
            world_cfg,
            target_chunk_x,
            target_chunk_z,
            origin_x,
            origin_y,
            origin_z,
            ore,
            &mut rng,
        );
    }
}

fn carve_segmented_vein(
    column: &mut ColumnBuffer,
    world_cfg: WorldConfig,
    target_chunk_x: i32,
    target_chunk_z: i32,
    origin_x: i32,
    origin_y: i32,
    origin_z: i32,
    ore: OreFeatureConfig,
    rng: &mut Rng64,
) {
    let angle = next_f32(rng) * PI;
    let horizontal_half_span = ore.vein_size as f32 / 8.0;
    let dx = angle.sin() * horizontal_half_span;
    let dz = angle.cos() * horizontal_half_span;
    let start_x = origin_x as f32 + dx;
    let end_x = origin_x as f32 - dx;
    let start_z = origin_z as f32 + dz;
    let end_z = origin_z as f32 - dz;
    let start_y = origin_y as f32 + range_f32(rng, -2.0, 2.0);
    let end_y = origin_y as f32 + range_f32(rng, -2.0, 2.0);
    let vein_steps = ore.vein_size.max(1) as i32;
    let size_scale = ore.vein_size as f32 / 16.0;
    let target_min_x = target_chunk_x * CHUNK_SIZE;
    let target_max_x = target_min_x + CHUNK_SIZE - 1;
    let target_min_z = target_chunk_z * CHUNK_SIZE;
    let target_max_z = target_min_z + CHUNK_SIZE - 1;

    for step in 0..vein_steps {
        let t = if vein_steps <= 1 {
            0.0
        } else {
            step as f32 / (vein_steps - 1) as f32
        };
        let center_x = lerp(start_x, end_x, t);
        let center_y = lerp(start_y, end_y, t);
        let center_z = lerp(start_z, end_z, t);
        let radius = (((t * PI).sin() + 1.0) * (next_f32(rng) * size_scale + 1.0)) * 0.5;
        if radius <= 0.0 {
            continue;
        }

        let min_x = (center_x - radius).floor() as i32;
        let max_x = (center_x + radius).floor() as i32;
        let min_y = (center_y - radius).floor() as i32;
        let max_y = (center_y + radius).floor() as i32;
        let min_z = (center_z - radius).floor() as i32;
        let max_z = (center_z + radius).floor() as i32;
        if max_x < target_min_x
            || min_x > target_max_x
            || max_z < target_min_z
            || min_z > target_max_z
        {
            continue;
        }

        for world_x in min_x..=max_x {
            let norm_x = ((world_x as f32 + 0.5) - center_x) / radius;
            let norm_x_sq = norm_x * norm_x;
            if norm_x_sq >= 1.0 {
                continue;
            }
            for world_y in min_y..=max_y {
                let norm_y = ((world_y as f32 + 0.5) - center_y) / radius;
                let norm_xy_sq = norm_x_sq + norm_y * norm_y;
                if norm_xy_sq >= 1.0 {
                    continue;
                }
                for world_z in min_z..=max_z {
                    let norm_z = ((world_z as f32 + 0.5) - center_z) / radius;
                    if norm_xy_sq + norm_z * norm_z >= 1.0 {
                        continue;
                    }
                    try_place_ore(
                        column,
                        world_cfg,
                        target_chunk_x,
                        target_chunk_z,
                        world_x,
                        world_y,
                        world_z,
                        ore,
                    );
                }
            }
        }
    }
}

fn try_place_ore(
    column: &mut ColumnBuffer,
    world_cfg: WorldConfig,
    target_chunk_x: i32,
    target_chunk_z: i32,
    world_x: i32,
    world_y: i32,
    world_z: i32,
    ore: OreFeatureConfig,
) {
    let Some((local_x, local_y, local_z)) =
        world_to_local_target(target_chunk_x, target_chunk_z, world_x, world_y, world_z)
    else {
        return;
    };
    if world_y < clamped_min_y(world_cfg) || world_y > clamped_max_y(world_cfg) {
        return;
    }
    let current = column.get_block(local_x, local_y, local_z);
    if !ore.replace_rule.matches(current) {
        return;
    }
    if ore.exposed_policy == ExposedOrePolicy::HiddenOnly
        && block_is_exposed_to_air(column, local_x, local_y, local_z)
    {
        return;
    }
    column.set_block(local_x, local_y, local_z, ore.ore_block);
}

fn world_to_local_target(
    target_chunk_x: i32,
    target_chunk_z: i32,
    world_x: i32,
    world_y: i32,
    world_z: i32,
) -> Option<(i32, i32, i32)> {
    let local_x = world_x - target_chunk_x * CHUNK_SIZE;
    let local_z = world_z - target_chunk_z * CHUNK_SIZE;
    if !(0..CHUNK_SIZE).contains(&local_x)
        || !(0..CHUNK_SIZE).contains(&local_z)
        || !(0..ColumnBuffer::HEIGHT).contains(&world_y)
    {
        return None;
    }
    Some((local_x, world_y, local_z))
}

fn block_is_exposed_to_air(column: &ColumnBuffer, x: i32, y: i32, z: i32) -> bool {
    for (dx, dy, dz) in [
        (1, 0, 0),
        (-1, 0, 0),
        (0, 1, 0),
        (0, -1, 0),
        (0, 0, 1),
        (0, 0, -1),
    ] {
        let neighbor = column.get_block(x + dx, y + dy, z + dz);
        if neighbor == BlockId::Air {
            return true;
        }
    }
    false
}

impl OreHeightDistribution {
    fn sample_y(self, world_cfg: WorldConfig, rng: &mut Rng64) -> Option<i32> {
        match self {
            Self::Uniform { min, max } => {
                let min_y = clamp_world_y(world_cfg, min.resolve(world_cfg));
                let max_y = clamp_world_y(world_cfg, max.resolve(world_cfg));
                if min_y > max_y {
                    return None;
                }
                Some(range_i32(rng, min_y, max_y))
            }
            Self::Triangle { min, peak, max } => {
                let min_y = clamp_world_y(world_cfg, min.resolve(world_cfg));
                let peak_y = clamp_world_y(world_cfg, peak.resolve(world_cfg));
                let max_y = clamp_world_y(world_cfg, max.resolve(world_cfg));
                if min_y > max_y {
                    return None;
                }
                let peak_y = peak_y.clamp(min_y, max_y);
                if min_y == max_y {
                    return Some(min_y);
                }
                let min_f = min_y as f32;
                let max_f = max_y as f32;
                let peak_f = peak_y as f32;
                let unit = next_f32(rng);
                let span = (max_f - min_f).max(1.0);
                let peak_ratio = ((peak_f - min_f) / span).clamp(0.0, 1.0);
                let sampled = if unit < peak_ratio {
                    min_f + (unit * span * (peak_f - min_f).max(0.0)).sqrt()
                } else {
                    max_f - ((1.0 - unit) * span * (max_f - peak_f).max(0.0)).sqrt()
                };
                Some(sampled.round() as i32)
            }
        }
    }
}

fn ore_rng(
    world_seed: u64,
    preset: WorldgenPreset,
    source_chunk_x: i32,
    source_chunk_z: i32,
    salt: u64,
) -> Rng64 {
    let mut mixed = world_seed ^ preset_seed_mix(preset) ^ salt;
    mixed ^= (source_chunk_x as i64 as u64).wrapping_mul(ORE_SOURCE_X_MIX);
    mixed ^= (source_chunk_z as i64 as u64).wrapping_mul(ORE_SOURCE_Z_MIX);
    Rng64::new(hash64(mixed ^ ORE_SEED_FINAL_MIX))
}

fn preset_seed_mix(preset: WorldgenPreset) -> u64 {
    match preset {
        WorldgenPreset::Flatish => 0xA1F2_63D7_8BC9_1023,
        WorldgenPreset::ClassicFlatter => 0x2C7E_51A9_DB40_8F65,
        WorldgenPreset::Rd161348Like => 0x6D45_9A73_E20B_5C11,
        WorldgenPreset::Hilly => 0xF41B_0CE8_77D2_394B,
    }
}

fn clamped_min_y(world_cfg: WorldConfig) -> i32 {
    clamp_world_y(world_cfg, world_cfg.min_y())
}

fn clamped_max_y(world_cfg: WorldConfig) -> i32 {
    clamp_world_y(world_cfg, world_cfg.max_y())
}

fn clamp_world_y(world_cfg: WorldConfig, value: i32) -> i32 {
    value
        .clamp(world_cfg.min_y(), world_cfg.max_y())
        .clamp(0, ColumnBuffer::HEIGHT - 1)
}

fn chance_256(rng: &mut Rng64, threshold: u32) -> bool {
    (rng.next_u32() & 255) < threshold.min(256)
}

fn range_i32(rng: &mut Rng64, lo: i32, hi: i32) -> i32 {
    debug_assert!(lo <= hi);
    let span = (hi - lo + 1).max(1) as u32;
    lo + (rng.next_u32() % span) as i32
}

fn next_f32(rng: &mut Rng64) -> f32 {
    ((rng.next_u32() >> 8) as f32) * (1.0 / ((1u32 << 24) as f32))
}

fn range_f32(rng: &mut Rng64, lo: f32, hi: f32) -> f32 {
    lo + (hi - lo) * next_f32(rng)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stone_column() -> ColumnBuffer {
        let mut column = ColumnBuffer::new();
        for y in 0..ColumnBuffer::HEIGHT {
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    column.set_block(x, y, z, BlockId::Stone);
                }
            }
        }
        column
    }

    #[test]
    fn ore_generation_is_deterministic_for_same_column() {
        let world_cfg = WorldConfig::default();
        let preset = WorldgenPreset::Rd161348Like;
        let mut a = stone_column();
        let mut b = stone_column();

        apply_ores_to_column(&mut a, world_cfg, preset, 3, -2);
        apply_ores_to_column(&mut b, world_cfg, preset, 3, -2);

        assert_eq!(a.blocks, b.blocks);
    }

    #[test]
    fn triangular_height_distribution_respects_world_bounds() {
        let world_cfg = WorldConfig {
            min_y: 0,
            max_y: 63,
            ..WorldConfig::default()
        };
        let distribution = OreHeightDistribution::Triangle {
            min: HeightAnchor::AboveBottom(0),
            peak: HeightAnchor::Absolute(24),
            max: HeightAnchor::BelowTop(0),
        };
        let mut rng = Rng64::new(0xAABBCCDD);

        for _ in 0..256 {
            let y = distribution
                .sample_y(world_cfg, &mut rng)
                .expect("expected valid ore height sample");
            assert!((world_cfg.min_y()..=world_cfg.max_y()).contains(&y));
        }
    }

    #[test]
    fn segmented_vein_can_reach_into_neighbor_chunk_space() {
        let world_cfg = WorldConfig::default();
        let ore = OreFeatureConfig {
            name: "test_spill",
            ore_block: BlockId::CoalOre,
            replace_rule: OreReplaceRule::AnyOf(&REPLACE_STONE),
            vein_size: 24,
            attempts_per_chunk: 1,
            chance_per_attempt_256: 256,
            height_distribution: OreHeightDistribution::Uniform {
                min: HeightAnchor::Absolute(32),
                max: HeightAnchor::Absolute(32),
            },
            exposed_policy: ExposedOrePolicy::Allow,
            salt: 1,
        };
        let mut column = stone_column();
        let mut rng = Rng64::new(0xC0FFEE);

        carve_segmented_vein(&mut column, world_cfg, 0, 0, 16, 32, 8, ore, &mut rng);

        let spilled = (0..ColumnBuffer::HEIGHT).any(|y| {
            (0..CHUNK_SIZE).any(|z| column.get_block(CHUNK_SIZE - 1, y, z) == BlockId::CoalOre)
        });
        assert!(
            spilled,
            "expected vein from neighboring chunk space to reach target column edge"
        );
    }
}
