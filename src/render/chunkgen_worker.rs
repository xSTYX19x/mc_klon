use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select};
use glam::IVec3;

use crate::blocks::BlockId;
use crate::meshing::{CpuMeshData, FULLBRIGHT_LIGHT_PACKED, Vertex};
use crate::perf::trace as perf_trace;
use crate::world::config::WorldConfig;
use crate::world::fluids::{
    FluidKind, fluid_data_is_source_origin, fluid_data_is_worldgen_static, fluid_data_make_static,
    fluid_kind,
};
use crate::world::{CHUNK_SIZE, Chunk, ChunkCoord, WORLD_MAX_Y, block_data_arc, blocks_arc};
use crate::worldgen::{
    ChunkGenerator, ColumnBuffer, GenContext, TerrainGenerator, TerrainSampler, WorldGenConfig,
    WorldGenerator, WorldgenPreset, carvers::Carvers, ores::apply_ores_to_column,
};

const WATER_FEATURE_CHANCE_PER_CHUNK_256: u32 = 24;
const WATER_FEATURE_RIVER_RATIO_256: u32 = 128;
const LAVA_LAKE_CHANCE_PER_CHUNK_256: u32 = 6;
const WATER_LAKE_R_MIN: i32 = 3;
const WATER_LAKE_R_MAX: i32 = 7;
const WATER_LAKE_MAX_HEIGHT_SPAN: i32 = 4;
const WATER_LAKE_SLOPE_SAMPLE_MARGIN: i32 = 2;
const WATER_LAKE_AIR_CLEARANCE: i32 = 3;
const WATER_LAKE_SURFACE_OFFSET_MIN: i32 = 1;
const WATER_LAKE_SURFACE_OFFSET_MAX: i32 = 3;
const LAVA_LAKE_R_MIN: i32 = 2;
const LAVA_LAKE_R_MAX: i32 = 5;
const LAKE_DEPTH_MIN: i32 = 3;
const LAKE_DEPTH_MAX: i32 = 7;
const SHORELINE_WATER_RADIUS: i32 = 2;
const SHORELINE_MAX_SLOPE: i32 = 2;
const SHORELINE_MAX_TOP_ABOVE_WATER: i32 = 2;
const SHORELINE_MAX_WATER_ABOVE_TOP: i32 = 1;
const SHORELINE_FILL_DEPTH: i32 = 3;
const WATER_SPRING_MIN_SURFACE_Y: i32 = 30;
const WATER_SPRING_PATH_MIN: i32 = 8;
const WATER_SPRING_PATH_MAX: i32 = 24;
const WATER_RIVER_SOURCE_MIN_ABOVE_SEA: i32 = 6;
const WATER_RIVER_SOURCE_MAX_ABOVE_SEA: i32 = 28;
const WATER_RIVER_SOURCE_MAX_SLOPE: i32 = 2;
const WATER_RIVER_SOURCE_SLOPE_SAMPLE_RADIUS: i32 = 3;
const WATER_RIVER_SOURCE_MAX_HEIGHT_SPAN: i32 = 4;
const WATER_RIVER_PATH_MAX_SLOPE: i32 = 3;
const WATER_RIVER_MAX_UPHILL_STEP: i32 = 1;
const WATER_RIVER_STOP_ABOVE_SEA: i32 = 1;
const WATER_RIVER_MIN_PATH_LEN: usize = 8;
const WATER_RIVER_MEANDER_BIAS_TURN_CHANCE_256: u32 = 72;
const TREE_ATTEMPTS_PER_CHUNK: i32 = 6;
const TREE_CHANCE_PER_ATTEMPT_256: u32 = 132;
const TREE_MAX_PER_CHUNK: usize = 3;
const TREE_XZ_MARGIN: i32 = 3;
const TREE_MAX_LOCAL_SLOPE: i32 = 2;
const TREE_WATER_AVOID_RADIUS: i32 = 3;
const TREE_SURFACE_SCAN_DEPTH: i32 = 24;
const TREE_SALT: u64 = 0x5D71_3C8A;
const FLOWER_SALT: u64 = 0x8F4B_23D1;
const FLOWER_PATCH_ATTEMPTS_PER_CHUNK: i32 = 2;
const FLOWER_PATCH_RADIUS: i32 = 3;
const FLOWER_PATCH_PLACEMENTS_MIN: i32 = 1;
const FLOWER_PATCH_PLACEMENTS_MAX: i32 = 4;
const FLOWER_SURFACE_SCAN_DEPTH: i32 = 24;
const DANDELION_CHANCE_PER_FLOWER_256: u32 = 152;
const MUSHROOM_SALT: u64 = 0x91AE_77C3;
const MUSHROOM_DARK_PROBES_PER_CHUNK: i32 = 28;
const MUSHROOM_SURFACE_SCAN_DEPTH: i32 = 48;
const MUSHROOM_MIN_BELOW_SURFACE: i32 = 3;
const MUSHROOM_CEILING_MAX_DISTANCE: i32 = 3;
const MUSHROOM_BROWN_PATCH_CHANCE_256: u32 = 64; // 1/4
const MUSHROOM_RED_PATCH_CHANCE_256: u32 = 32; // 1/8
const MUSHROOM_PATCH_RADIUS: i32 = 4;
const WINTER_MAX_SNOW_Y: i32 = WORLD_MAX_Y - 1;
const MUSHROOM_PATCH_PLACEMENTS_MIN: i32 = 5;
const MUSHROOM_PATCH_PLACEMENTS_MAX: i32 = 12;
const LOD_GRID_CELLS_PER_CHUNK: i32 = 4;
const GENERATED_COLUMN_CACHE_MAX_ENTRIES: usize = 384;
const GENERATED_COLUMN_HISTORY_MAX_ENTRIES: usize = 4096;
// Real far LOD is driven by dedicated `LodColumnOnly` jobs. This flag only controls
// whether full chunkgen opportunistically piggybacks an extra far-LOD mesh on section results.
const ENABLE_FAR_LOD_PIGGYBACK_ON_FULL_CHUNK_RESULTS: bool = false;
const ENABLE_WORLD_FLUID_FEATURES: bool = true;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ColumnCoordKey {
    x: i32,
    z: i32,
}

impl ColumnCoordKey {
    const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ColumnGenerationDescriptor {
    seed: u64,
    sea_level: i32,
    min_y: i32,
    max_y: i32,
    chunk_size: i32,
    boundary_ocean: bool,
    boundary_radius_chunks: i32,
    preset: WorldgenPreset,
}

impl ColumnGenerationDescriptor {
    fn new(world_cfg: WorldConfig, preset: WorldgenPreset) -> Self {
        Self {
            seed: world_cfg.seed,
            sea_level: world_cfg.sea_level,
            min_y: world_cfg.min_y,
            max_y: world_cfg.max_y,
            chunk_size: world_cfg.chunk_size,
            boundary_ocean: world_cfg.boundary_ocean,
            boundary_radius_chunks: world_cfg.boundary_radius_chunks,
            preset,
        }
    }

    fn min_chunk_y(self) -> i32 {
        self.min_y.div_euclid(CHUNK_SIZE)
    }

    fn max_chunk_y(self) -> i32 {
        self.max_y.div_euclid(CHUNK_SIZE)
    }

    fn section_count(self) -> usize {
        usize::try_from((self.max_chunk_y() - self.min_chunk_y() + 1).max(0)).unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ColumnCacheKey {
    coord: ColumnCoordKey,
    descriptor: ColumnGenerationDescriptor,
}

impl ColumnCacheKey {
    fn new(coord: ColumnCoordKey, world_cfg: WorldConfig, preset: WorldgenPreset) -> Self {
        Self {
            coord,
            descriptor: ColumnGenerationDescriptor::new(world_cfg, preset),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }

    fn chance_256(&mut self, threshold: u32) -> bool {
        (self.next_u32() & 255) < threshold.min(256)
    }

    fn range_i32(&mut self, lo: i32, hi: i32) -> i32 {
        debug_assert!(lo <= hi);
        let span = (hi - lo + 1).max(1) as u32;
        lo + (self.next_u32() % span) as i32
    }
}

#[derive(Clone, Copy, Debug)]
struct LakeCandidate {
    cx: i32,
    cy: i32,
    cz: i32,
    radius: i32,
    depth: i32,
    shape_seed: u64,
    fill: BlockId,
}

#[derive(Clone, Debug)]
struct WaterSpring {
    source_x: i32,
    source_y: i32,
    source_z: i32,
    shape_seed: u64,
    path: Vec<(i32, i32, i32)>,
}

#[derive(Clone, Debug)]
enum WaterFeature {
    Lake(LakeCandidate),
    Spring(WaterSpring),
}

#[derive(Clone, Copy, Debug)]
enum TreePreset {
    SmallOak,
    TallOak,
    WideCanopyOak,
    BushTree,
    GreatOak,
    MonumentOak,
}

#[derive(Clone, Copy, Debug)]
struct TreeCandidate {
    local_x: i32,
    local_z: i32,
    ground_y: i32,
    trunk_height: i32,
    preset: TreePreset,
}

fn worldgen_seed32(seed: u64) -> u32 {
    let mixed = seed ^ (seed >> 32);
    (mixed as u32).wrapping_mul(0x9E37_79B9)
}

fn preset_seed_mix(preset: WorldgenPreset) -> u64 {
    match preset {
        WorldgenPreset::Flatish => 0xA1F2_63D7_8BC9_1023,
        WorldgenPreset::ClassicFlatter => 0x2C7E_51A9_DB40_8F65,
        WorldgenPreset::Rd161348Like => 0x6D45_9A73_E20B_5C11,
        WorldgenPreset::Hilly => 0xF41B_0CE8_77D2_394B,
    }
}

fn lake_rng(
    seed: u64,
    preset: WorldgenPreset,
    chunk_x: i32,
    chunk_z: i32,
    salt: u64,
) -> SplitMix64 {
    let mut mixed = (worldgen_seed32(seed) as u64) ^ preset_seed_mix(preset) ^ salt;
    mixed ^= (chunk_x as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    mixed ^= (chunk_z as i64 as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    SplitMix64::new(mixed ^ 0x94D0_49BB_1331_11EB)
}

fn hash_mix64(mut value: u64) -> u64 {
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

fn signed_noise_2d(seed: u64, x: i32, z: i32, salt: u64) -> f32 {
    let mut key = seed ^ salt;
    key ^= (x as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    key ^= (z as i64 as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let hash = hash_mix64(key);
    let unit = ((hash >> 11) as f64) * (1.0 / ((1_u64 << 53) as f64));
    (unit as f32) * 2.0 - 1.0
}

fn local_max_slope(terrain: &TerrainSampler, world_x: i32, world_z: i32) -> i32 {
    let center = terrain.height_at(world_x, world_z, WORLD_MAX_Y);
    let mut slope = 0;
    for (dx, dz) in [
        (1, 0),
        (-1, 0),
        (0, 1),
        (0, -1),
        (1, 1),
        (1, -1),
        (-1, 1),
        (-1, -1),
    ] {
        let h = terrain.height_at(world_x + dx, world_z + dz, WORLD_MAX_Y);
        slope = slope.max((h - center).abs());
    }
    slope
}

fn lake_local_depth(lake: LakeCandidate, world_x: i32, world_z: i32) -> Option<i32> {
    let dx = world_x - lake.cx;
    let dz = world_z - lake.cz;
    let dist2 = (dx * dx + dz * dz) as f32;
    let mut radius = lake.radius as f32;
    if lake.fill == BlockId::WaterStill {
        let edge_noise = signed_noise_2d(lake.shape_seed, world_x, world_z, 0x7C9E_1284);
        radius *= (1.0 + edge_noise * 0.25).clamp(0.72, 1.35);
    }
    let radius_sq = radius * radius;
    if dist2 > radius_sq {
        return None;
    }
    let t = (dist2 / radius_sq).clamp(0.0, 1.0);
    let mut depth_factor = (1.0 - t).sqrt();
    if lake.fill == BlockId::WaterStill {
        let cove_noise = signed_noise_2d(lake.shape_seed, world_x, world_z, 0x34C1_EF2A);
        depth_factor *= (1.0 + cove_noise * 0.18).clamp(0.72, 1.22);
    }
    let local_depth = (depth_factor * lake.depth as f32).ceil() as i32;
    (local_depth > 0).then_some(local_depth)
}

fn terrain_height_span_in_radius(
    terrain: &TerrainSampler,
    world_x: i32,
    world_z: i32,
    radius: i32,
) -> i32 {
    let mut min_h = i32::MAX;
    let mut max_h = i32::MIN;
    let radius_sq = radius * radius;
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            if dx * dx + dz * dz > radius_sq {
                continue;
            }
            let h = terrain.height_at(world_x + dx, world_z + dz, WORLD_MAX_Y);
            min_h = min_h.min(h);
            max_h = max_h.max(h);
        }
    }
    max_h.saturating_sub(min_h)
}

fn make_water_lake_candidate(
    source_chunk_x: i32,
    source_chunk_z: i32,
    terrain: &TerrainSampler,
    _sea_level: i32,
    rng: &mut SplitMix64,
) -> Option<LakeCandidate> {
    let wx = source_chunk_x * CHUNK_SIZE + rng.range_i32(0, CHUNK_SIZE - 1);
    let wz = source_chunk_z * CHUNK_SIZE + rng.range_i32(0, CHUNK_SIZE - 1);
    let radius = rng.range_i32(WATER_LAKE_R_MIN, WATER_LAKE_R_MAX);
    let slope_sample_radius = radius + WATER_LAKE_SLOPE_SAMPLE_MARGIN;
    let height_span = terrain_height_span_in_radius(terrain, wx, wz, slope_sample_radius);
    if height_span > WATER_LAKE_MAX_HEIGHT_SPAN {
        return None;
    }

    let surface_y = terrain.height_at(wx, wz, WORLD_MAX_Y);
    let lake_y = (surface_y
        - rng.range_i32(WATER_LAKE_SURFACE_OFFSET_MIN, WATER_LAKE_SURFACE_OFFSET_MAX))
    .clamp(6, WORLD_MAX_Y - 2);
    if lake_y >= surface_y {
        return None;
    }

    Some(LakeCandidate {
        cx: wx,
        cy: lake_y,
        cz: wz,
        radius,
        depth: rng.range_i32(LAKE_DEPTH_MIN, LAKE_DEPTH_MAX),
        shape_seed: rng.next_u64(),
        fill: BlockId::WaterStill,
    })
}

fn make_lava_lake_candidate(
    seed: u64,
    preset: WorldgenPreset,
    source_chunk_x: i32,
    source_chunk_z: i32,
    terrain: &TerrainSampler,
    sea_level: i32,
) -> Option<LakeCandidate> {
    let mut rng = lake_rng(seed, preset, source_chunk_x, source_chunk_z, 0x4D77_E102);
    if !rng.chance_256(LAVA_LAKE_CHANCE_PER_CHUNK_256) {
        return None;
    }

    let wx = source_chunk_x * CHUNK_SIZE + rng.range_i32(0, CHUNK_SIZE - 1);
    let wz = source_chunk_z * CHUNK_SIZE + rng.range_i32(0, CHUNK_SIZE - 1);
    let surface_y = terrain.height_at(wx, wz, WORLD_MAX_Y);
    let lava_max_y = (sea_level - 2).min(WORLD_MAX_Y - 2).max(8);
    let lava_y = (surface_y - rng.range_i32(10, 18)).clamp(8, lava_max_y);
    if lava_y >= surface_y - 4 {
        return None;
    }

    Some(LakeCandidate {
        cx: wx,
        cy: lava_y,
        cz: wz,
        radius: rng.range_i32(LAVA_LAKE_R_MIN, LAVA_LAKE_R_MAX),
        depth: rng.range_i32(LAKE_DEPTH_MIN, LAKE_DEPTH_MAX),
        shape_seed: rng.next_u64(),
        fill: BlockId::LavaStill,
    })
}

fn collect_nearby_water_features(
    world_cfg: &WorldConfig,
    preset: WorldgenPreset,
    chunk_x: i32,
    chunk_z: i32,
    terrain: &TerrainSampler,
) -> (Vec<LakeCandidate>, Vec<WaterSpring>, Vec<LakeCandidate>) {
    let mut water = Vec::with_capacity(16);
    let mut springs = Vec::with_capacity(8);
    let mut lava = Vec::with_capacity(4);
    for source_z in (chunk_z - 3)..=(chunk_z + 3) {
        for source_x in (chunk_x - 3)..=(chunk_x + 3) {
            if let Some(feature) = choose_water_feature_for_source(
                world_cfg.seed,
                preset,
                source_x,
                source_z,
                terrain,
                world_cfg.sea_level(),
            ) {
                match feature {
                    WaterFeature::Lake(candidate) => water.push(candidate),
                    WaterFeature::Spring(spring) => springs.push(spring),
                }
            }
            if let Some(candidate) = make_lava_lake_candidate(
                world_cfg.seed,
                preset,
                source_x,
                source_z,
                terrain,
                world_cfg.sea_level(),
            ) {
                lava.push(candidate);
            }
        }
    }
    (water, springs, lava)
}

#[derive(Clone, Debug, Default)]
struct NearbyWaterFeatures {
    water_lakes: Vec<LakeCandidate>,
    springs: Vec<WaterSpring>,
    lava_lakes: Vec<LakeCandidate>,
}

fn make_water_spring_candidate(
    source_chunk_x: i32,
    source_chunk_z: i32,
    terrain: &TerrainSampler,
    sea_level: i32,
    rng: &mut SplitMix64,
) -> Option<WaterSpring> {
    let source_x = source_chunk_x * CHUNK_SIZE + rng.range_i32(0, CHUNK_SIZE - 1);
    let source_z = source_chunk_z * CHUNK_SIZE + rng.range_i32(0, CHUNK_SIZE - 1);
    let surface_y = terrain.height_at(source_x, source_z, WORLD_MAX_Y);
    if surface_y < WATER_SPRING_MIN_SURFACE_Y {
        return None;
    }
    if surface_y < sea_level + WATER_RIVER_SOURCE_MIN_ABOVE_SEA
        || surface_y > sea_level + WATER_RIVER_SOURCE_MAX_ABOVE_SEA
    {
        return None;
    }
    if local_max_slope(terrain, source_x, source_z) > WATER_RIVER_SOURCE_MAX_SLOPE {
        return None;
    }
    let source_height_span = terrain_height_span_in_radius(
        terrain,
        source_x,
        source_z,
        WATER_RIVER_SOURCE_SLOPE_SAMPLE_RADIUS,
    );
    if source_height_span > WATER_RIVER_SOURCE_MAX_HEIGHT_SPAN {
        return None;
    }
    let source_y = (surface_y - 1).clamp(2, WORLD_MAX_Y - 2);

    let mut x = source_x;
    let mut z = source_z;
    let mut surface = surface_y;
    let mut y = source_y;
    let path_len = rng.range_i32(WATER_SPRING_PATH_MIN, WATER_SPRING_PATH_MAX);
    let mut path = Vec::with_capacity(path_len as usize);
    let mut prev_dir = (0_i32, 0_i32);
    let mut meander_bias = match rng.range_i32(0, 3) {
        0 => (1, 0),
        1 => (-1, 0),
        2 => (0, 1),
        _ => (0, -1),
    };

    for _ in 0..path_len {
        if rng.chance_256(WATER_RIVER_MEANDER_BIAS_TURN_CHANCE_256) {
            meander_bias = (meander_bias.1, -meander_bias.0);
        }

        let mut best: Option<(i32, i32, i32, i32)> = None;
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            let nx = x + dx;
            let nz = z + dz;
            let n_surface = terrain.height_at(nx, nz, WORLD_MAX_Y);
            let uphill_surface = (n_surface - surface).max(0);
            if uphill_surface > WATER_RIVER_MAX_UPHILL_STEP {
                continue;
            }
            if local_max_slope(terrain, nx, nz) > WATER_RIVER_PATH_MAX_SLOPE {
                continue;
            }

            let candidate_bed = (n_surface - 1).clamp(1, WORLD_MAX_Y - 2).min(y);
            let downhill = (y - candidate_bed).max(0);
            let reverse_penalty = if prev_dir != (0, 0) && (dx, dz) == (-prev_dir.0, -prev_dir.1) {
                24
            } else {
                0
            };
            let turn_penalty = if prev_dir != (0, 0) && (dx, dz) != prev_dir {
                2
            } else {
                0
            };
            let meander_bonus = if (dx, dz) == meander_bias { -2 } else { 0 };

            // Keep rivers trending downhill while retaining mild horizontal meander.
            let mut score = n_surface * 4 + uphill_surface * 14 + turn_penalty + reverse_penalty;
            score += rng.range_i32(0, 2);
            score += meander_bonus;
            score -= downhill * 4;
            if candidate_bed <= sea_level + WATER_RIVER_STOP_ABOVE_SEA {
                score -= 4;
            }

            let replace = best
                .as_ref()
                .is_none_or(|(_, _, _, best_score)| score < *best_score);
            if replace {
                best = Some((nx, nz, n_surface, score));
            }
        }
        let Some((nx, nz, n_surface, _)) = best else {
            break;
        };

        let bed_y = (n_surface - 1).clamp(1, WORLD_MAX_Y - 2);
        y = y.min(bed_y);
        surface = n_surface;
        if y <= 1 {
            break;
        }

        prev_dir = (nx - x, nz - z);
        x = nx;
        z = nz;
        path.push((x, y, z));
        if y <= sea_level + WATER_RIVER_STOP_ABOVE_SEA {
            break;
        }
    }

    if path.len() < WATER_RIVER_MIN_PATH_LEN {
        return None;
    }

    Some(WaterSpring {
        source_x,
        source_y,
        source_z,
        shape_seed: rng.next_u64(),
        path,
    })
}

fn choose_water_feature_for_source(
    seed: u64,
    preset: WorldgenPreset,
    source_chunk_x: i32,
    source_chunk_z: i32,
    terrain: &TerrainSampler,
    sea_level: i32,
) -> Option<WaterFeature> {
    let mut rng = lake_rng(seed, preset, source_chunk_x, source_chunk_z, 0x34A0_719C);
    if !rng.chance_256(WATER_FEATURE_CHANCE_PER_CHUNK_256) {
        return None;
    }

    let prefer_river = rng.chance_256(WATER_FEATURE_RIVER_RATIO_256);
    if prefer_river {
        if let Some(spring) = make_water_spring_candidate(
            source_chunk_x,
            source_chunk_z,
            terrain,
            sea_level,
            &mut rng,
        ) {
            return Some(WaterFeature::Spring(spring));
        }
        if let Some(lake) =
            make_water_lake_candidate(source_chunk_x, source_chunk_z, terrain, sea_level, &mut rng)
        {
            return Some(WaterFeature::Lake(lake));
        }
    } else {
        if let Some(lake) =
            make_water_lake_candidate(source_chunk_x, source_chunk_z, terrain, sea_level, &mut rng)
        {
            return Some(WaterFeature::Lake(lake));
        }
        if let Some(spring) = make_water_spring_candidate(
            source_chunk_x,
            source_chunk_z,
            terrain,
            sea_level,
            &mut rng,
        ) {
            return Some(WaterFeature::Spring(spring));
        }
    }
    None
}

fn is_fluid(block: BlockId) -> bool {
    matches!(
        block,
        BlockId::WaterStill | BlockId::WaterFlowing | BlockId::LavaStill | BlockId::LavaFlowing
    )
}

fn apply_water_lake_boundary_seal_to_chunk(
    chunk: &mut Chunk,
    coord: ChunkCoord,
    lake: LakeCandidate,
) {
    let chunk_x0 = coord.x * CHUNK_SIZE;
    let chunk_x1 = chunk_x0 + CHUNK_SIZE - 1;
    let chunk_y0 = coord.y * CHUNK_SIZE;
    let chunk_y1 = chunk_y0 + CHUNK_SIZE - 1;
    let chunk_z0 = coord.z * CHUNK_SIZE;
    let chunk_z1 = chunk_z0 + CHUNK_SIZE - 1;

    for local_z in 0..CHUNK_SIZE {
        let world_z = chunk_z0 + local_z;
        for local_x in 0..CHUNK_SIZE {
            let world_x = chunk_x0 + local_x;
            let Some(local_depth) = lake_local_depth(lake, world_x, world_z) else {
                continue;
            };
            let boundary_column = [(1, 0), (-1, 0), (0, 1), (0, -1)]
                .into_iter()
                .any(|(dx, dz)| lake_local_depth(lake, world_x + dx, world_z + dz).is_none());
            if !boundary_column {
                continue;
            }

            let y_min = (lake.cy - local_depth).max(1);
            let y_max = lake.cy.min(WORLD_MAX_Y);
            if y_min > chunk_y1 || y_max < chunk_y0 {
                continue;
            }
            let write_y_min = y_min.max(chunk_y0);
            let write_y_max = y_max.min(chunk_y1);
            for world_y in write_y_min..=write_y_max {
                let seal_block = if world_y >= lake.cy - 1 {
                    BlockId::Dirt
                } else {
                    BlockId::Stone
                };
                for (dx, dy, dz) in [(1, 0, 0), (-1, 0, 0), (0, 0, 1), (0, 0, -1), (0, -1, 0)] {
                    let nx = world_x + dx;
                    let ny = world_y + dy;
                    let nz = world_z + dz;
                    if ny < 1 || ny > lake.cy || ny > WORLD_MAX_Y {
                        continue;
                    }
                    if nx < chunk_x0
                        || nx > chunk_x1
                        || ny < chunk_y0
                        || ny > chunk_y1
                        || nz < chunk_z0
                        || nz > chunk_z1
                    {
                        continue;
                    }
                    let nlx = nx - chunk_x0;
                    let nly = ny - chunk_y0;
                    let nlz = nz - chunk_z0;
                    if chunk.get(nlx, nly, nlz) == BlockId::Air {
                        chunk.set(nlx, nly, nlz, seal_block);
                        chunk.set_data(nlx, nly, nlz, 0);
                    }
                }
            }
        }
    }
}

fn apply_lake_carve_and_fill_to_chunk(
    chunk: &mut Chunk,
    coord: ChunkCoord,
    terrain: &TerrainSampler,
    lake: LakeCandidate,
) {
    let chunk_y0 = coord.y * CHUNK_SIZE;
    let chunk_y1 = chunk_y0 + CHUNK_SIZE - 1;
    let chunk_x0 = coord.x * CHUNK_SIZE;
    let chunk_z0 = coord.z * CHUNK_SIZE;
    let is_water_lake = lake.fill == BlockId::WaterStill;

    for local_z in 0..CHUNK_SIZE {
        let world_z = chunk_z0 + local_z;
        for local_x in 0..CHUNK_SIZE {
            let world_x = chunk_x0 + local_x;
            let Some(local_depth) = lake_local_depth(lake, world_x, world_z) else {
                continue;
            };
            let y_min = (lake.cy - local_depth).max(1);
            let y_max = lake.cy.min(WORLD_MAX_Y);
            if y_min > chunk_y1 || y_max < chunk_y0 {
                continue;
            }
            let write_y_min = y_min.max(chunk_y0);
            let write_y_max = y_max.min(chunk_y1);
            for world_y in write_y_min..=write_y_max {
                let local_y = world_y - chunk_y0;
                chunk.set(local_x, local_y, local_z, lake.fill);
                let data = match lake.fill {
                    BlockId::WaterStill | BlockId::LavaStill => fluid_data_make_static(0, false),
                    _ => 0,
                };
                chunk.set_data(local_x, local_y, local_z, data);
            }

            if is_water_lake {
                let surface_y = terrain.height_at(world_x, world_z, WORLD_MAX_Y);
                let air_y_min = (lake.cy + 1).clamp(1, WORLD_MAX_Y);
                let air_y_max = (lake.cy + WATER_LAKE_AIR_CLEARANCE)
                    .min(surface_y)
                    .clamp(1, WORLD_MAX_Y);
                if air_y_min > chunk_y1 || air_y_max < chunk_y0 || air_y_min > air_y_max {
                    continue;
                }
                let write_air_min = air_y_min.max(chunk_y0);
                let write_air_max = air_y_max.min(chunk_y1);
                for world_y in write_air_min..=write_air_max {
                    let local_y = world_y - chunk_y0;
                    chunk.set(local_x, local_y, local_z, BlockId::Air);
                    chunk.set_data(local_x, local_y, local_z, 0);
                }
            }
        }
    }

    if is_water_lake {
        apply_water_lake_boundary_seal_to_chunk(chunk, coord, lake);
    }
}

fn apply_spring_channel_to_chunk(chunk: &mut Chunk, coord: ChunkCoord, spring: &WaterSpring) {
    let chunk_x0 = coord.x * CHUNK_SIZE;
    let chunk_x1 = chunk_x0 + CHUNK_SIZE - 1;
    let chunk_y0 = coord.y * CHUNK_SIZE;
    let chunk_y1 = chunk_y0 + CHUNK_SIZE - 1;
    let chunk_z0 = coord.z * CHUNK_SIZE;
    let chunk_z1 = chunk_z0 + CHUNK_SIZE - 1;

    let mut last_bed_y = spring.source_y;
    for (x, raw_y, z) in std::iter::once((spring.source_x, spring.source_y, spring.source_z))
        .chain(spring.path.iter().copied())
    {
        let bed_y = raw_y.min(last_bed_y).clamp(2, WORLD_MAX_Y - 2);
        last_bed_y = bed_y;

        let width_noise = signed_noise_2d(spring.shape_seed, x, z, 0x67D2_A119);
        let radius = if width_noise > 0.42 { 2 } else { 1 };
        let depth = if width_noise < -0.45 { 2 } else { 1 };
        let floor_y = (bed_y - depth + 1).max(1);
        let ceiling_y = (bed_y + 1).min(WORLD_MAX_Y);

        for dz in -radius..=radius {
            for dx in -radius..=radius {
                if dx * dx + dz * dz > radius * radius + (radius - 1) {
                    continue;
                }
                let wx = x + dx;
                let wz = z + dz;
                if wx < chunk_x0 || wx > chunk_x1 || wz < chunk_z0 || wz > chunk_z1 {
                    continue;
                }
                let lx = wx - chunk_x0;
                let lz = wz - chunk_z0;

                for wy in floor_y..=ceiling_y {
                    if wy < chunk_y0 || wy > chunk_y1 {
                        continue;
                    }
                    let ly = wy - chunk_y0;
                    if wy <= bed_y {
                        chunk.set(lx, ly, lz, BlockId::WaterStill);
                        chunk.set_data(lx, ly, lz, fluid_data_make_static(0, false));
                    } else {
                        chunk.set(lx, ly, lz, BlockId::Air);
                        chunk.set_data(lx, ly, lz, 0);
                    }
                }

                let bottom_y = floor_y - 1;
                if (chunk_y0..=chunk_y1).contains(&bottom_y) {
                    let ly = bottom_y - chunk_y0;
                    let bottom = chunk.get(lx, ly, lz);
                    if is_fluid(bottom) || !bottom.is_solid() {
                        chunk.set(lx, ly, lz, BlockId::Dirt);
                        chunk.set_data(lx, ly, lz, 0);
                    }
                }
            }
        }

        // Seal channel boundary so carved rivers do not immediately leak into open caves.
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                if dx * dx + dz * dz > radius * radius + (radius - 1) {
                    continue;
                }
                let boundary_cell =
                    [(1, 0), (-1, 0), (0, 1), (0, -1)]
                        .into_iter()
                        .any(|(ox, oz)| {
                            let ndx = dx + ox;
                            let ndz = dz + oz;
                            ndx * ndx + ndz * ndz > radius * radius + (radius - 1)
                        });
                if !boundary_cell {
                    continue;
                }

                let wx = x + dx;
                let wz = z + dz;
                if wx < chunk_x0 || wx > chunk_x1 || wz < chunk_z0 || wz > chunk_z1 {
                    continue;
                }
                for wy in floor_y..=bed_y {
                    for (ox, oy, oz) in [(1, 0, 0), (-1, 0, 0), (0, 0, 1), (0, 0, -1), (0, -1, 0)] {
                        let nx = wx + ox;
                        let ny = wy + oy;
                        let nz = wz + oz;
                        if nx < chunk_x0
                            || nx > chunk_x1
                            || nz < chunk_z0
                            || nz > chunk_z1
                            || ny < chunk_y0
                            || ny > chunk_y1
                            || ny <= 0
                        {
                            continue;
                        }
                        if ny > bed_y {
                            continue;
                        }
                        let nlx = nx - chunk_x0;
                        let nly = ny - chunk_y0;
                        let nlz = nz - chunk_z0;
                        if chunk.get(nlx, nly, nlz) == BlockId::Air {
                            let seal_block = if ny >= bed_y - 1 {
                                BlockId::Dirt
                            } else {
                                BlockId::Stone
                            };
                            chunk.set(nlx, nly, nlz, seal_block);
                            chunk.set_data(nlx, nly, nlz, 0);
                        }
                    }
                }
            }
        }

        let bank_radius = radius + 1;
        let bank_y = (bed_y + 1).clamp(1, WORLD_MAX_Y);
        for dz in -bank_radius..=bank_radius {
            for dx in -bank_radius..=bank_radius {
                if dx.abs().max(dz.abs()) != bank_radius {
                    continue;
                }
                let wx = x + dx;
                let wz = z + dz;
                if wx < chunk_x0 || wx > chunk_x1 || wz < chunk_z0 || wz > chunk_z1 {
                    continue;
                }

                for depth in 0..=1 {
                    let wy = bank_y - depth;
                    if wy < chunk_y0 || wy > chunk_y1 || wy <= 0 {
                        continue;
                    }
                    let lx = wx - chunk_x0;
                    let ly = wy - chunk_y0;
                    let lz = wz - chunk_z0;
                    let block = chunk.get(lx, ly, lz);
                    if !block.is_solid() || is_fluid(block) {
                        let bank_block = if wy >= bed_y {
                            BlockId::Dirt
                        } else {
                            BlockId::Stone
                        };
                        chunk.set(lx, ly, lz, bank_block);
                        chunk.set_data(lx, ly, lz, 0);
                    }
                }
            }
        }
    }
}

fn apply_streaming_lakes_and_fluids(
    chunk: &mut Chunk,
    coord: ChunkCoord,
    terrain_sampler: &TerrainSampler,
    nearby_water_features: &NearbyWaterFeatures,
) {
    for &lake in &nearby_water_features.water_lakes {
        apply_lake_carve_and_fill_to_chunk(chunk, coord, terrain_sampler, lake);
    }
    for &lake in &nearby_water_features.lava_lakes {
        apply_lake_carve_and_fill_to_chunk(chunk, coord, terrain_sampler, lake);
    }

    for spring in &nearby_water_features.springs {
        apply_spring_channel_to_chunk(chunk, coord, spring);
    }
}

fn apply_post_fixes_to_chunk(chunk: &mut Chunk) {
    for local_y in 0..(CHUNK_SIZE - 1) {
        for local_z in 0..CHUNK_SIZE {
            for local_x in 0..CHUNK_SIZE {
                if chunk.get(local_x, local_y, local_z) != BlockId::Grass {
                    continue;
                }
                let above = chunk.get(local_x, local_y + 1, local_z);
                if above == BlockId::WaterStill || above == BlockId::WaterFlowing {
                    chunk.set(local_x, local_y, local_z, BlockId::Dirt);
                    chunk.set_data(local_x, local_y, local_z, 0);
                }
            }
        }
    }
}

fn apply_surface_dirt_to_grass_in_column(column: &mut ColumnBuffer) {
    for local_z in 0..CHUNK_SIZE {
        for local_x in 0..CHUNK_SIZE {
            for y in (1..=WORLD_MAX_Y).rev() {
                let block = column.get_block(local_x, y, local_z);
                if block == BlockId::Air {
                    continue;
                }
                if is_fluid(block) {
                    break;
                }
                if block == BlockId::Dirt
                    && column.get_block(local_x, y + 1, local_z) == BlockId::Air
                {
                    column.set_block(local_x, y, local_z, BlockId::Grass);
                    column.set_data(local_x, y, local_z, 0);
                }
                break;
            }
        }
    }
}

#[inline]
fn column_xz_index(local_x: i32, local_z: i32) -> usize {
    (local_z * CHUNK_SIZE + local_x) as usize
}

#[inline]
fn column_get_block(column: &ColumnBuffer, local_x: i32, y: i32, local_z: i32) -> BlockId {
    column.get_block(local_x, y, local_z)
}

#[inline]
fn column_set_block(column: &mut ColumnBuffer, local_x: i32, y: i32, local_z: i32, block: BlockId) {
    column.set_block(local_x, y, local_z, block);
    column.set_data(local_x, y, local_z, 0);
}

fn column_top_solid_y(column: &ColumnBuffer, local_x: i32, local_z: i32) -> Option<i32> {
    for y in (1..=WORLD_MAX_Y).rev() {
        let block = column_get_block(column, local_x, y, local_z);
        if block != BlockId::Air && !is_fluid(block) {
            return Some(y);
        }
    }
    None
}

fn column_water_surface_y(column: &ColumnBuffer, local_x: i32, local_z: i32) -> Option<i32> {
    for y in (1..=WORLD_MAX_Y).rev() {
        let block = column_get_block(column, local_x, y, local_z);
        if !is_water_block(block) {
            continue;
        }
        let above = column_get_block(column, local_x, y + 1, local_z);
        if !is_water_block(above) {
            return Some(y);
        }
    }
    None
}

#[inline]
fn count_sand_placement_in_chunk(shoreline_sand_by_chunk: &mut [u32], y: i32) {
    let chunk_y = y.div_euclid(CHUNK_SIZE);
    if let Some(entry) = shoreline_sand_by_chunk.get_mut(chunk_y as usize) {
        *entry = entry.saturating_add(1);
    }
}

fn place_shoreline_sand_block(
    column: &mut ColumnBuffer,
    shoreline_sand_by_chunk: &mut [u32],
    local_x: i32,
    y: i32,
    local_z: i32,
) {
    if column_get_block(column, local_x, y, local_z) == BlockId::Sand {
        return;
    }
    column_set_block(column, local_x, y, local_z, BlockId::Sand);
    count_sand_placement_in_chunk(shoreline_sand_by_chunk, y);
}

fn apply_shoreline_pass_to_column(column: &mut ColumnBuffer) -> Vec<u32> {
    let cache_len = (CHUNK_SIZE * CHUNK_SIZE) as usize;
    let mut top_y_cache = vec![None::<i32>; cache_len];
    let mut water_surface_cache = vec![None::<i32>; cache_len];
    let mut shoreline_sand_by_chunk = vec![0u32; ColumnBuffer::chunk_count_y() as usize];

    for local_z in 0..CHUNK_SIZE {
        for local_x in 0..CHUNK_SIZE {
            let idx = column_xz_index(local_x, local_z);
            top_y_cache[idx] = column_top_solid_y(column, local_x, local_z);
            water_surface_cache[idx] = column_water_surface_y(column, local_x, local_z);
        }
    }

    for local_z in 0..CHUNK_SIZE {
        for local_x in 0..CHUNK_SIZE {
            let idx = column_xz_index(local_x, local_z);
            let Some(top_y) = top_y_cache[idx] else {
                continue;
            };
            if top_y <= 1 || top_y >= WORLD_MAX_Y {
                continue;
            }

            let top_block = column_get_block(column, local_x, top_y, local_z);
            if !matches!(top_block, BlockId::Grass | BlockId::Dirt) {
                continue;
            }

            let above = column_get_block(column, local_x, top_y + 1, local_z);
            if !(above == BlockId::Air || is_water_block(above)) {
                continue;
            }

            let below = column_get_block(column, local_x, top_y - 1, local_z);
            if !below.is_solid() {
                continue;
            }

            let mut max_delta = 0;
            let mut slope_valid = true;
            for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                let nx = local_x + dx;
                let nz = local_z + dz;
                if !(0..CHUNK_SIZE).contains(&nx) || !(0..CHUNK_SIZE).contains(&nz) {
                    slope_valid = false;
                    break;
                }
                let Some(neighbor_top_y) = top_y_cache[column_xz_index(nx, nz)] else {
                    slope_valid = false;
                    break;
                };
                max_delta = max_delta.max((neighbor_top_y - top_y).abs());
            }
            if !slope_valid || max_delta > SHORELINE_MAX_SLOPE {
                continue;
            }

            let mut nearest_water_y: Option<i32> = None;
            let mut best_abs_delta = i32::MAX;
            for dz in -SHORELINE_WATER_RADIUS..=SHORELINE_WATER_RADIUS {
                for dx in -SHORELINE_WATER_RADIUS..=SHORELINE_WATER_RADIUS {
                    let nx = local_x + dx;
                    let nz = local_z + dz;
                    if !(0..CHUNK_SIZE).contains(&nx) || !(0..CHUNK_SIZE).contains(&nz) {
                        continue;
                    }
                    let Some(water_y) = water_surface_cache[column_xz_index(nx, nz)] else {
                        continue;
                    };
                    if water_y < top_y - SHORELINE_MAX_TOP_ABOVE_WATER
                        || water_y > top_y + SHORELINE_MAX_WATER_ABOVE_TOP
                    {
                        continue;
                    }
                    let abs_delta = (top_y - water_y).abs();
                    if abs_delta < best_abs_delta {
                        best_abs_delta = abs_delta;
                        nearest_water_y = Some(water_y);
                    }
                }
            }

            let Some(nearest_water_y) = nearest_water_y else {
                continue;
            };
            let top_above_water = top_y - nearest_water_y;
            if !(0..=SHORELINE_MAX_TOP_ABOVE_WATER).contains(&top_above_water) {
                continue;
            }

            place_shoreline_sand_block(
                column,
                &mut shoreline_sand_by_chunk,
                local_x,
                top_y,
                local_z,
            );

            for depth in 1..=SHORELINE_FILL_DEPTH {
                let y = top_y - depth;
                if y <= 0 {
                    break;
                }
                if column_get_block(column, local_x, y, local_z) != BlockId::Dirt {
                    break;
                }
                let support = column_get_block(column, local_x, y - 1, local_z);
                if !support.is_solid() {
                    break;
                }
                place_shoreline_sand_block(
                    column,
                    &mut shoreline_sand_by_chunk,
                    local_x,
                    y,
                    local_z,
                );
            }
        }
    }

    shoreline_sand_by_chunk
}

fn remove_unsupported_sand_in_column(column: &mut ColumnBuffer) {
    for y in 1..=WORLD_MAX_Y {
        for local_z in 0..CHUNK_SIZE {
            for local_x in 0..CHUNK_SIZE {
                if column_get_block(column, local_x, y, local_z) != BlockId::Sand {
                    continue;
                }
                let below = column_get_block(column, local_x, y - 1, local_z);
                if below.is_solid() {
                    continue;
                }
                column_set_block(column, local_x, y, local_z, BlockId::Air);
            }
        }
    }
}

fn choose_tree_preset(rng: &mut SplitMix64) -> (TreePreset, i32) {
    match rng.range_i32(0, 9) {
        0 | 1 => (TreePreset::SmallOak, 4),
        2 | 3 => (TreePreset::TallOak, rng.range_i32(6, 7)),
        4 | 5 => (TreePreset::WideCanopyOak, 5),
        6 | 7 => (TreePreset::BushTree, rng.range_i32(2, 3)),
        8 => (TreePreset::GreatOak, rng.range_i32(8, 10)),
        _ => (TreePreset::MonumentOak, rng.range_i32(9, 11)),
    }
}

fn is_tree_ground(block: BlockId) -> bool {
    matches!(block, BlockId::Grass | BlockId::Dirt)
}

fn is_water_block(block: BlockId) -> bool {
    matches!(block, BlockId::WaterStill | BlockId::WaterFlowing)
}

fn can_place_leaf_over(block: BlockId) -> bool {
    matches!(block, BlockId::Air | BlockId::OakLeaves)
}

fn find_tree_ground_y(
    column: &ColumnBuffer,
    local_x: i32,
    local_z: i32,
    surface_y: i32,
) -> Option<i32> {
    let start_y = (surface_y + 2).clamp(1, WORLD_MAX_Y - 2);
    let quick_min = (start_y - TREE_SURFACE_SCAN_DEPTH).max(1);

    for y in (quick_min..=start_y).rev() {
        let ground = column.get_block(local_x, y, local_z);
        let above = column.get_block(local_x, y + 1, local_z);
        if is_tree_ground(ground) && above == BlockId::Air {
            return Some(y);
        }
    }

    for y in (1..quick_min).rev() {
        let ground = column.get_block(local_x, y, local_z);
        let above = column.get_block(local_x, y + 1, local_z);
        if is_tree_ground(ground) && above == BlockId::Air {
            return Some(y);
        }
    }

    None
}

fn has_nearby_water_for_tree(
    column: &ColumnBuffer,
    local_x: i32,
    ground_y: i32,
    local_z: i32,
) -> bool {
    let y0 = (ground_y - 2).max(1);
    let y1 = (ground_y + 2).min(WORLD_MAX_Y);

    for dz in -TREE_WATER_AVOID_RADIUS..=TREE_WATER_AVOID_RADIUS {
        for dx in -TREE_WATER_AVOID_RADIUS..=TREE_WATER_AVOID_RADIUS {
            let nx = local_x + dx;
            let nz = local_z + dz;
            if !(0..CHUNK_SIZE).contains(&nx) || !(0..CHUNK_SIZE).contains(&nz) {
                return true;
            }
            for y in y0..=y1 {
                if is_water_block(column.get_block(nx, y, nz)) {
                    return true;
                }
            }
        }
    }

    false
}

fn place_leaf_if_air_or_leaf(column: &mut ColumnBuffer, x: i32, y: i32, z: i32) {
    if !ColumnBuffer::in_bounds(x, y, z) {
        return;
    }
    let current = column.get_block(x, y, z);
    if can_place_leaf_over(current) {
        column.set_block(x, y, z, BlockId::OakLeaves);
        column.set_data(x, y, z, 0);
    }
}

fn place_leaf_layer(
    column: &mut ColumnBuffer,
    cx: i32,
    cy: i32,
    cz: i32,
    radius: i32,
    cut_corners: bool,
) {
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            if cut_corners && dx.abs() == radius && dz.abs() == radius {
                continue;
            }
            place_leaf_if_air_or_leaf(column, cx + dx, cy, cz + dz);
        }
    }
}

fn place_tree_candidate_to_column(column: &mut ColumnBuffer, candidate: TreeCandidate) -> bool {
    let cx = candidate.local_x;
    let cz = candidate.local_z;
    let base_y = candidate.ground_y + 1;
    let top_y = base_y + candidate.trunk_height - 1;
    if top_y >= WORLD_MAX_Y - 1 {
        return false;
    }

    for y in base_y..=top_y {
        if !ColumnBuffer::in_bounds(cx, y, cz) || column.get_block(cx, y, cz) != BlockId::Air {
            return false;
        }
    }

    for y in base_y..=top_y {
        column.set_block(cx, y, cz, BlockId::OakLog);
        column.set_data(cx, y, cz, 0);
    }

    match candidate.preset {
        TreePreset::SmallOak => {
            place_leaf_layer(column, cx, top_y - 2, cz, 1, false);
            place_leaf_layer(column, cx, top_y - 1, cz, 1, false);
            place_leaf_layer(column, cx, top_y, cz, 1, true);
            place_leaf_if_air_or_leaf(column, cx, top_y + 1, cz);
        }
        TreePreset::TallOak => {
            place_leaf_layer(column, cx, top_y - 3, cz, 1, false);
            place_leaf_layer(column, cx, top_y - 2, cz, 2, true);
            place_leaf_layer(column, cx, top_y - 1, cz, 2, false);
            place_leaf_layer(column, cx, top_y, cz, 2, true);
            place_leaf_layer(column, cx, top_y + 1, cz, 1, false);
            place_leaf_if_air_or_leaf(column, cx, top_y + 2, cz);
        }
        TreePreset::WideCanopyOak => {
            place_leaf_layer(column, cx, top_y - 2, cz, 1, false);
            place_leaf_layer(column, cx, top_y - 1, cz, 2, true);
            place_leaf_layer(column, cx, top_y, cz, 2, false);
            place_leaf_layer(column, cx, top_y + 1, cz, 1, true);
            for (dx, dz) in [(2, 0), (-2, 0), (0, 2), (0, -2)] {
                place_leaf_if_air_or_leaf(column, cx + dx, top_y - 1, cz + dz);
                place_leaf_if_air_or_leaf(column, cx + dx, top_y - 2, cz + dz);
            }
        }
        TreePreset::BushTree => {
            place_leaf_layer(column, cx, top_y - 1, cz, 1, false);
            place_leaf_layer(column, cx, top_y, cz, 1, false);
            place_leaf_layer(column, cx, top_y + 1, cz, 1, true);
        }
        TreePreset::GreatOak => {
            place_leaf_layer(column, cx, top_y - 4, cz, 2, true);
            place_leaf_layer(column, cx, top_y - 3, cz, 3, true);
            place_leaf_layer(column, cx, top_y - 2, cz, 3, false);
            place_leaf_layer(column, cx, top_y - 1, cz, 3, true);
            place_leaf_layer(column, cx, top_y, cz, 2, true);
            place_leaf_layer(column, cx, top_y + 1, cz, 1, false);
            place_leaf_if_air_or_leaf(column, cx, top_y + 2, cz);
            for (dx, dz) in [(3, 0), (-3, 0), (0, 3), (0, -3)] {
                place_leaf_if_air_or_leaf(column, cx + dx, top_y - 2, cz + dz);
                place_leaf_if_air_or_leaf(column, cx + dx, top_y - 3, cz + dz);
            }
        }
        TreePreset::MonumentOak => {
            place_leaf_layer(column, cx, top_y - 5, cz, 2, false);
            place_leaf_layer(column, cx, top_y - 4, cz, 3, true);
            place_leaf_layer(column, cx, top_y - 3, cz, 4, true);
            place_leaf_layer(column, cx, top_y - 2, cz, 4, false);
            place_leaf_layer(column, cx, top_y - 1, cz, 3, true);
            place_leaf_layer(column, cx, top_y, cz, 2, true);
            place_leaf_layer(column, cx, top_y + 1, cz, 1, true);
            place_leaf_if_air_or_leaf(column, cx, top_y + 2, cz);
            for (dx, dz) in [(4, 0), (-4, 0), (0, 4), (0, -4)] {
                place_leaf_if_air_or_leaf(column, cx + dx, top_y - 2, cz + dz);
                place_leaf_if_air_or_leaf(column, cx + dx, top_y - 3, cz + dz);
            }
        }
    }

    true
}

fn apply_trees_to_column(
    column: &mut ColumnBuffer,
    world_cfg: &WorldConfig,
    preset: WorldgenPreset,
    chunk_x: i32,
    chunk_z: i32,
    terrain_sampler: &TerrainSampler,
) {
    let mut rng = lake_rng(world_cfg.seed, preset, chunk_x, chunk_z, TREE_SALT);
    let mut planted = 0usize;

    for _ in 0..TREE_ATTEMPTS_PER_CHUNK {
        if planted >= TREE_MAX_PER_CHUNK {
            break;
        }
        if !rng.chance_256(TREE_CHANCE_PER_ATTEMPT_256) {
            continue;
        }

        let local_x = rng.range_i32(TREE_XZ_MARGIN, CHUNK_SIZE - 1 - TREE_XZ_MARGIN);
        let local_z = rng.range_i32(TREE_XZ_MARGIN, CHUNK_SIZE - 1 - TREE_XZ_MARGIN);
        let world_x = chunk_x * CHUNK_SIZE + local_x;
        let world_z = chunk_z * CHUNK_SIZE + local_z;

        if local_max_slope(terrain_sampler, world_x, world_z) > TREE_MAX_LOCAL_SLOPE {
            continue;
        }

        let surface_y = terrain_sampler.height_at(world_x, world_z, WORLD_MAX_Y);
        let Some(ground_y) = find_tree_ground_y(column, local_x, local_z, surface_y) else {
            continue;
        };
        if has_nearby_water_for_tree(column, local_x, ground_y, local_z) {
            continue;
        }

        let (preset_kind, trunk_height) = choose_tree_preset(&mut rng);
        let candidate = TreeCandidate {
            local_x,
            local_z,
            ground_y,
            trunk_height,
            preset: preset_kind,
        };
        if place_tree_candidate_to_column(column, candidate) {
            planted += 1;
        }
    }
}

#[inline]
fn is_flower_ground(block: BlockId) -> bool {
    matches!(block, BlockId::Grass | BlockId::Dirt)
}

fn has_sky_access(column: &ColumnBuffer, local_x: i32, y: i32, local_z: i32) -> bool {
    let start_y = (y + 1).clamp(1, WORLD_MAX_Y);
    for scan_y in start_y..=WORLD_MAX_Y {
        if column.get_block(local_x, scan_y, local_z) != BlockId::Air {
            return false;
        }
    }
    true
}

fn find_flower_ground_y(
    column: &ColumnBuffer,
    local_x: i32,
    local_z: i32,
    surface_y: i32,
) -> Option<i32> {
    let start_y = (surface_y + 2).clamp(1, WORLD_MAX_Y - 2);
    let min_y = (start_y - FLOWER_SURFACE_SCAN_DEPTH).max(1);
    for y in (min_y..=start_y).rev() {
        let ground = column.get_block(local_x, y, local_z);
        let above = column.get_block(local_x, y + 1, local_z);
        if is_flower_ground(ground) && above == BlockId::Air {
            return Some(y);
        }
    }
    None
}

fn can_place_flower_at(column: &ColumnBuffer, local_x: i32, y: i32, local_z: i32) -> bool {
    if !ColumnBuffer::in_bounds(local_x, y, local_z)
        || !ColumnBuffer::in_bounds(local_x, y + 1, local_z)
    {
        return false;
    }
    let ground = column.get_block(local_x, y, local_z);
    let at = column.get_block(local_x, y + 1, local_z);
    if !is_flower_ground(ground) || at != BlockId::Air {
        return false;
    }
    has_sky_access(column, local_x, y + 1, local_z)
}

fn apply_flowers_to_column(
    column: &mut ColumnBuffer,
    world_cfg: &WorldConfig,
    preset: WorldgenPreset,
    chunk_x: i32,
    chunk_z: i32,
    terrain_sampler: &TerrainSampler,
) {
    let mut rng = lake_rng(world_cfg.seed, preset, chunk_x, chunk_z, FLOWER_SALT);
    for _ in 0..FLOWER_PATCH_ATTEMPTS_PER_CHUNK {
        let center_x = rng.range_i32(0, CHUNK_SIZE - 1);
        let center_z = rng.range_i32(0, CHUNK_SIZE - 1);
        let placements =
            rng.range_i32(FLOWER_PATCH_PLACEMENTS_MIN, FLOWER_PATCH_PLACEMENTS_MAX) as usize;
        for _ in 0..placements {
            let local_x = center_x + rng.range_i32(-FLOWER_PATCH_RADIUS, FLOWER_PATCH_RADIUS);
            let local_z = center_z + rng.range_i32(-FLOWER_PATCH_RADIUS, FLOWER_PATCH_RADIUS);
            if !(0..CHUNK_SIZE).contains(&local_x) || !(0..CHUNK_SIZE).contains(&local_z) {
                continue;
            }
            let world_x = chunk_x * CHUNK_SIZE + local_x;
            let world_z = chunk_z * CHUNK_SIZE + local_z;
            let surface_y = terrain_sampler.height_at(world_x, world_z, WORLD_MAX_Y);
            let Some(ground_y) = find_flower_ground_y(column, local_x, local_z, surface_y) else {
                continue;
            };
            if !can_place_flower_at(column, local_x, ground_y, local_z) {
                continue;
            }
            let flower = if rng.chance_256(DANDELION_CHANCE_PER_FLOWER_256) {
                BlockId::Dandelion
            } else {
                BlockId::Poppy
            };
            column_set_block(column, local_x, ground_y + 1, local_z, flower);
        }
    }
}

fn is_mushroom_floor(block: BlockId) -> bool {
    block.is_solid()
        && !is_fluid(block)
        && !matches!(
            block,
            BlockId::OakLeaves
                | BlockId::OakLog
                | BlockId::Dandelion
                | BlockId::Poppy
                | BlockId::BrownMushroom
                | BlockId::RedMushroom
        )
}

fn find_dark_mushroom_ground_y(
    column: &ColumnBuffer,
    local_x: i32,
    local_z: i32,
    surface_y: i32,
) -> Option<i32> {
    let start_y = (surface_y - 1).clamp(2, WORLD_MAX_Y - 2);
    let min_y = (start_y - MUSHROOM_SURFACE_SCAN_DEPTH).max(1);
    for ground_y in (min_y..=start_y).rev() {
        let candidate_y = ground_y + 1;
        if surface_y - candidate_y < MUSHROOM_MIN_BELOW_SURFACE {
            continue;
        }
        let floor = column.get_block(local_x, ground_y, local_z);
        if !is_mushroom_floor(floor) {
            continue;
        }
        let at = column.get_block(local_x, candidate_y, local_z);
        if at != BlockId::Air {
            continue;
        }
        let mut ceiling_close = false;
        for dy in 1..=MUSHROOM_CEILING_MAX_DISTANCE {
            let check_y = candidate_y + dy;
            if check_y > WORLD_MAX_Y {
                break;
            }
            let above = column.get_block(local_x, check_y, local_z);
            if above.is_solid() && !is_fluid(above) {
                ceiling_close = true;
                break;
            }
        }
        if !ceiling_close {
            continue;
        }
        if has_sky_access(column, local_x, candidate_y, local_z) {
            continue;
        }
        return Some(ground_y);
    }
    None
}

fn collect_dark_mushroom_starts(
    column: &ColumnBuffer,
    chunk_x: i32,
    chunk_z: i32,
    terrain_sampler: &TerrainSampler,
    rng: &mut SplitMix64,
) -> Vec<(i32, i32, i32)> {
    let mut starts = Vec::with_capacity(MUSHROOM_DARK_PROBES_PER_CHUNK as usize);
    for _ in 0..MUSHROOM_DARK_PROBES_PER_CHUNK {
        let local_x = rng.range_i32(0, CHUNK_SIZE - 1);
        let local_z = rng.range_i32(0, CHUNK_SIZE - 1);
        let world_x = chunk_x * CHUNK_SIZE + local_x;
        let world_z = chunk_z * CHUNK_SIZE + local_z;
        let surface_y = terrain_sampler.height_at(world_x, world_z, WORLD_MAX_Y);
        let Some(ground_y) = find_dark_mushroom_ground_y(column, local_x, local_z, surface_y)
        else {
            continue;
        };
        let candidate = (local_x, ground_y + 1, local_z);
        if !starts.contains(&candidate) {
            starts.push(candidate);
        }
    }
    starts
}

fn place_mushroom_patch(
    column: &mut ColumnBuffer,
    chunk_x: i32,
    chunk_z: i32,
    terrain_sampler: &TerrainSampler,
    rng: &mut SplitMix64,
    dark_starts: &[(i32, i32, i32)],
    mushroom_block: BlockId,
) {
    if dark_starts.is_empty() {
        return;
    }
    let start = dark_starts[rng.range_i32(0, dark_starts.len() as i32 - 1) as usize];
    let center_x = start.0;
    let center_z = start.2;
    let attempts =
        rng.range_i32(MUSHROOM_PATCH_PLACEMENTS_MIN, MUSHROOM_PATCH_PLACEMENTS_MAX) as usize;
    for _ in 0..attempts {
        let local_x = center_x + rng.range_i32(-MUSHROOM_PATCH_RADIUS, MUSHROOM_PATCH_RADIUS);
        let local_z = center_z + rng.range_i32(-MUSHROOM_PATCH_RADIUS, MUSHROOM_PATCH_RADIUS);
        if !(0..CHUNK_SIZE).contains(&local_x) || !(0..CHUNK_SIZE).contains(&local_z) {
            continue;
        }
        let world_x = chunk_x * CHUNK_SIZE + local_x;
        let world_z = chunk_z * CHUNK_SIZE + local_z;
        let surface_y = terrain_sampler.height_at(world_x, world_z, WORLD_MAX_Y);
        let Some(ground_y) = find_dark_mushroom_ground_y(column, local_x, local_z, surface_y)
        else {
            continue;
        };
        if column.get_block(local_x, ground_y + 1, local_z) != BlockId::Air {
            continue;
        }
        column_set_block(column, local_x, ground_y + 1, local_z, mushroom_block);
    }
}

fn apply_mushrooms_to_column(
    column: &mut ColumnBuffer,
    world_cfg: &WorldConfig,
    preset: WorldgenPreset,
    chunk_x: i32,
    chunk_z: i32,
    terrain_sampler: &TerrainSampler,
) {
    let mut rng = lake_rng(world_cfg.seed, preset, chunk_x, chunk_z, MUSHROOM_SALT);
    let dark_starts =
        collect_dark_mushroom_starts(column, chunk_x, chunk_z, terrain_sampler, &mut rng);
    if dark_starts.is_empty() {
        return;
    }
    if rng.chance_256(MUSHROOM_BROWN_PATCH_CHANCE_256) {
        place_mushroom_patch(
            column,
            chunk_x,
            chunk_z,
            terrain_sampler,
            &mut rng,
            &dark_starts,
            BlockId::BrownMushroom,
        );
    }
    if rng.chance_256(MUSHROOM_RED_PATCH_CHANCE_256) {
        place_mushroom_patch(
            column,
            chunk_x,
            chunk_z,
            terrain_sampler,
            &mut rng,
            &dark_starts,
            BlockId::RedMushroom,
        );
    }
}

fn apply_foliage_to_column(
    column: &mut ColumnBuffer,
    world_cfg: &WorldConfig,
    preset: WorldgenPreset,
    chunk_x: i32,
    chunk_z: i32,
    terrain_sampler: &TerrainSampler,
) {
    apply_flowers_to_column(column, world_cfg, preset, chunk_x, chunk_z, terrain_sampler);
    apply_mushrooms_to_column(column, world_cfg, preset, chunk_x, chunk_z, terrain_sampler);
}

#[inline]
fn winter_surface_supports_snow(block: BlockId) -> bool {
    block.is_solid()
        && !matches!(
            block,
            BlockId::Air
                | BlockId::WaterStill
                | BlockId::WaterFlowing
                | BlockId::LavaStill
                | BlockId::LavaFlowing
                | BlockId::Ice
                | BlockId::Snow
        )
}

#[derive(Clone, Copy, Debug, Default)]
struct WinterColumnPassStats {
    snow_layers: u32,
    ice_blocks: u32,
}

fn apply_winter_surface_to_column(column: &mut ColumnBuffer) -> WinterColumnPassStats {
    let mut stats = WinterColumnPassStats::default();
    for local_z in 0..CHUNK_SIZE {
        for local_x in 0..CHUNK_SIZE {
            for y in (1..=WINTER_MAX_SNOW_Y).rev() {
                let block = column.get_block(local_x, y, local_z);
                if block == BlockId::Air {
                    continue;
                }
                let above = column.get_block(local_x, y + 1, local_z);
                if matches!(block, BlockId::WaterStill | BlockId::WaterFlowing)
                    && above == BlockId::Air
                {
                    column_set_block(column, local_x, y, local_z, BlockId::Ice);
                    stats.ice_blocks = stats.ice_blocks.saturating_add(1);
                    break;
                }
                if winter_surface_supports_snow(block) && above == BlockId::Air {
                    column_set_block(column, local_x, y + 1, local_z, BlockId::Snow);
                    stats.snow_layers = stats.snow_layers.saturating_add(1);
                }
                break;
            }
        }
    }
    stats
}

struct StreamingWorldColumnGenerator {
    preset: WorldgenPreset,
}

impl StreamingWorldColumnGenerator {
    fn new(preset: WorldgenPreset) -> Self {
        Self { preset }
    }

    fn generate_column_with_debug_stats(
        &self,
        cx: i32,
        cz: i32,
        out: &mut ColumnBuffer,
        ctx: &mut GenContext,
    ) -> (
        Vec<u32>,
        ColumnGenerationStageTimings,
        WinterColumnPassStats,
    ) {
        let cfg = WorldConfig { ..ctx.world_cfg };
        let min_chunk_y = cfg.min_y.div_euclid(CHUNK_SIZE);
        let max_chunk_y = cfg.max_y.div_euclid(CHUNK_SIZE);
        let section_count = (max_chunk_y - min_chunk_y + 1).max(1);
        let terrain_cfg = WorldGenConfig::from_preset_with_sea_level(
            worldgen_seed32(cfg.seed),
            self.preset,
            cfg.sea_level(),
        );
        let terrain_sampler = TerrainSampler::new(terrain_cfg);
        let (water_lakes, springs, lava_lakes) =
            collect_nearby_water_features(&cfg, self.preset, cx, cz, &terrain_sampler);
        let nearby_water_features = NearbyWaterFeatures {
            water_lakes,
            springs,
            lava_lakes,
        };
        let mut stage_timings = ColumnGenerationStageTimings::default();
        let mut winter_stats = WinterColumnPassStats::default();

        // 1) Base terrain fill
        ctx.report_progress("Base terrain fill", 0, 7);
        let stage_started = Instant::now();
        let terrain_generator = TerrainGenerator::new(cfg, self.preset);
        for chunk_y in min_chunk_y..=max_chunk_y {
            let mut section = Chunk::new_empty();
            terrain_generator.generate_chunk(
                ChunkCoord {
                    x: cx,
                    y: chunk_y,
                    z: cz,
                },
                &mut section,
            );
            out.write_chunk_section(chunk_y, &section);
        }
        stage_timings.base_terrain_fill = stage_started.elapsed();

        // 2) Caves / carvers
        ctx.report_progress("Caves", 1, 7);
        let stage_started = Instant::now();
        Carvers::carve_column(ctx.seed, cx, cz, out, cfg.sea_level);
        stage_timings.carvers = stage_started.elapsed();

        // 3) Ores
        ctx.report_progress("Ores", 2, 7);
        let stage_started = Instant::now();
        apply_ores_to_column(out, cfg, self.preset, cx, cz);
        stage_timings.ores = stage_started.elapsed();

        // 4) Lakes/springs and 5) post-fixes per section
        ctx.report_progress("Lakes & springs", 3, 7);
        let stage_started = Instant::now();
        for (idx, chunk_y) in (min_chunk_y..=max_chunk_y).enumerate() {
            let mut section = out.extract_chunk(chunk_y);
            let coord = ChunkCoord {
                x: cx,
                y: chunk_y,
                z: cz,
            };

            // 3) Water/lava lakes and springs
            apply_streaming_lakes_and_fluids(
                &mut section,
                coord,
                &terrain_sampler,
                &nearby_water_features,
            );

            // 4) Post-fixes
            apply_post_fixes_to_chunk(&mut section);
            out.write_chunk_section(chunk_y, &section);

            if idx + 1 == section_count as usize {
                ctx.report_progress("Post-fixes", 4, 7);
            }
        }
        stage_timings.water_and_post_fixes = stage_started.elapsed();

        let stage_started = Instant::now();
        apply_surface_dirt_to_grass_in_column(out);
        let shoreline_sand_by_chunk = apply_shoreline_pass_to_column(out);
        remove_unsupported_sand_in_column(out);
        stage_timings.shoreline = stage_started.elapsed();
        ctx.report_progress("Shoreline", 5, 7);

        // 6) Trees (column-level feature pass)
        let stage_started = Instant::now();
        apply_trees_to_column(out, &cfg, self.preset, cx, cz, &terrain_sampler);
        stage_timings.trees = stage_started.elapsed();
        ctx.report_progress("Trees", 6, 7);
        // 7) Foliage (flowers + mushrooms)
        let stage_started = Instant::now();
        apply_foliage_to_column(out, &cfg, self.preset, cx, cz, &terrain_sampler);
        stage_timings.foliage = stage_started.elapsed();
        ctx.report_progress("Foliage", 7, 7);
        if cfg.environment().enables_winter_surface_rules() {
            let stage_started = Instant::now();
            winter_stats = apply_winter_surface_to_column(out);
            stage_timings.winter_surface = stage_started.elapsed();
        }
        (shoreline_sand_by_chunk, stage_timings, winter_stats)
    }
}

impl WorldGenerator for StreamingWorldColumnGenerator {
    fn generate_column(&self, cx: i32, cz: i32, out: &mut ColumnBuffer, ctx: &mut GenContext) {
        let _ = self.generate_column_with_debug_stats(cx, cz, out, ctx);
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ColumnGenerationStageTimings {
    base_terrain_fill: Duration,
    carvers: Duration,
    ores: Duration,
    water_and_post_fixes: Duration,
    shoreline: Duration,
    trees: Duration,
    foliage: Duration,
    winter_surface: Duration,
    section_snapshot: Duration,
}

impl ColumnGenerationStageTimings {
    fn instrumentation_fields(self) -> String {
        format!(
            "base_ms={:.3} carvers_ms={:.3} ores_ms={:.3} fluid_ms={:.3} shoreline_ms={:.3} trees_ms={:.3} foliage_ms={:.3} winter_ms={:.3} snapshot_ms={:.3}",
            self.base_terrain_fill.as_secs_f64() * 1000.0,
            self.carvers.as_secs_f64() * 1000.0,
            self.ores.as_secs_f64() * 1000.0,
            self.water_and_post_fixes.as_secs_f64() * 1000.0,
            self.shoreline.as_secs_f64() * 1000.0,
            self.trees.as_secs_f64() * 1000.0,
            self.foliage.as_secs_f64() * 1000.0,
            self.winter_surface.as_secs_f64() * 1000.0,
            self.section_snapshot.as_secs_f64() * 1000.0,
        )
    }
}

#[derive(Clone, Debug)]
struct GeneratedSectionSnapshot {
    blocks: Arc<Vec<BlockId>>,
    block_data: Arc<Vec<u8>>,
    filled_voxel_count: u16,
    solid_voxel_count: u16,
    worldgen_debug: GeneratedWorldgenDebugStats,
    fluid_bootstrap: Arc<Vec<GeneratedFluidBootstrapCell>>,
}

impl GeneratedSectionSnapshot {
    fn materialize_chunk(&self) -> Chunk {
        Chunk::from_generated_parts(
            self.blocks.clone(),
            self.block_data.clone(),
            self.filled_voxel_count,
            self.solid_voxel_count,
        )
    }
}

// Full terrain/features are generated once per X/Z column and then reused for
// any requested Y section. Section snapshots are cached in ready-to-serve form
// so cache hits avoid rescanning the full column for stats/bootstrap work.
#[derive(Clone, Debug)]
struct GeneratedColumn {
    coord: ColumnCoordKey,
    descriptor: ColumnGenerationDescriptor,
    min_chunk_y: i32,
    max_chunk_y: i32,
    sections: Vec<GeneratedSectionSnapshot>,
    stage_timings: ColumnGenerationStageTimings,
}

impl GeneratedColumn {
    fn new(
        coord: ColumnCoordKey,
        descriptor: ColumnGenerationDescriptor,
        sections: Vec<GeneratedSectionSnapshot>,
        stage_timings: ColumnGenerationStageTimings,
    ) -> Self {
        Self {
            coord,
            descriptor,
            min_chunk_y: descriptor.min_chunk_y(),
            max_chunk_y: descriptor.max_chunk_y(),
            sections,
            stage_timings,
        }
    }

    fn contains_chunk_y(&self, chunk_y: i32) -> bool {
        (self.min_chunk_y..=self.max_chunk_y).contains(&chunk_y)
    }

    fn section_index(&self, chunk_y: i32) -> Option<usize> {
        if !self.contains_chunk_y(chunk_y) {
            return None;
        }
        usize::try_from(chunk_y - self.min_chunk_y).ok()
    }

    fn extract_chunk(
        &self,
        chunk_y: i32,
    ) -> (
        Chunk,
        GeneratedWorldgenDebugStats,
        Vec<GeneratedFluidBootstrapCell>,
    ) {
        debug_assert_eq!(self.sections.len(), self.descriptor.section_count());
        let Some(snapshot) = self
            .section_index(chunk_y)
            .and_then(|idx| self.sections.get(idx))
        else {
            return (
                Chunk::new_empty(),
                GeneratedWorldgenDebugStats::default(),
                Vec::new(),
            );
        };
        (
            snapshot.materialize_chunk(),
            snapshot.worldgen_debug,
            snapshot.fluid_bootstrap.as_ref().clone(),
        )
    }
}

fn generate_streaming_column_buffer(
    world_cfg: WorldConfig,
    preset: WorldgenPreset,
    chunk_x: i32,
    chunk_z: i32,
) -> (
    ColumnBuffer,
    Vec<u32>,
    ColumnGenerationStageTimings,
    WinterColumnPassStats,
) {
    let mut ctx = GenContext::new(world_cfg.seed, world_cfg);
    let generator = StreamingWorldColumnGenerator::new(preset);
    let mut column = ColumnBuffer::new();
    let (shoreline_sand_by_chunk, stage_timings, winter_stats) =
        generator.generate_column_with_debug_stats(chunk_x, chunk_z, &mut column, &mut ctx);
    (column, shoreline_sand_by_chunk, stage_timings, winter_stats)
}

fn generate_streaming_column(
    world_cfg: WorldConfig,
    preset: WorldgenPreset,
    chunk_x: i32,
    chunk_z: i32,
) -> GeneratedColumn {
    let coord = ColumnCoordKey::new(chunk_x, chunk_z);
    let descriptor = ColumnGenerationDescriptor::new(world_cfg, preset);
    let (column, shoreline_sand_by_chunk, mut stage_timings, winter_stats) =
        generate_streaming_column_buffer(world_cfg, preset, chunk_x, chunk_z);
    let snapshot_started = Instant::now();
    let sections = build_generated_section_snapshots(
        coord,
        descriptor,
        &column,
        &shoreline_sand_by_chunk,
        winter_stats,
    );
    stage_timings.section_snapshot = snapshot_started.elapsed();
    GeneratedColumn::new(coord, descriptor, sections, stage_timings)
}

#[cfg(test)]
pub(crate) fn generate_streaming_column_for_test(
    world_cfg: WorldConfig,
    preset: WorldgenPreset,
    chunk_x: i32,
    chunk_z: i32,
) -> ColumnBuffer {
    let (column, _, _, _) = generate_streaming_column_buffer(world_cfg, preset, chunk_x, chunk_z);
    column
}

fn build_generated_section_snapshots(
    coord: ColumnCoordKey,
    descriptor: ColumnGenerationDescriptor,
    column: &ColumnBuffer,
    shoreline_sand_by_chunk: &[u32],
    _winter_stats: WinterColumnPassStats,
) -> Vec<GeneratedSectionSnapshot> {
    let section_voxel_count = (CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE) as usize;
    let layer_area = (CHUNK_SIZE * CHUNK_SIZE) as usize;
    let mut sections = Vec::with_capacity(descriptor.section_count());

    for (section_idx, chunk_y) in (descriptor.min_chunk_y()..=descriptor.max_chunk_y()).enumerate()
    {
        let base_world_y = chunk_y * CHUNK_SIZE;
        let mut blocks = Vec::with_capacity(section_voxel_count);
        let mut block_data = Vec::with_capacity(section_voxel_count);

        for local_y in 0..CHUNK_SIZE {
            let world_y = base_world_y + local_y;
            let src_start = (world_y as usize) * layer_area;
            let src_end = src_start + layer_area;
            blocks.extend_from_slice(&column.blocks[src_start..src_end]);
            block_data.extend_from_slice(&column.data[src_start..src_end]);
        }

        let shoreline_sand_blocks = shoreline_sand_by_chunk
            .get(section_idx)
            .copied()
            .unwrap_or(0);
        let (filled_voxel_count, solid_voxel_count, worldgen_debug, fluid_bootstrap) =
            analyze_generated_section_buffers(
                &blocks,
                &block_data,
                shoreline_sand_blocks,
                ChunkCoord {
                    x: coord.x,
                    y: chunk_y,
                    z: coord.z,
                },
            );
        sections.push(GeneratedSectionSnapshot {
            blocks: blocks_arc(blocks),
            block_data: block_data_arc(block_data),
            filled_voxel_count,
            solid_voxel_count,
            worldgen_debug,
            fluid_bootstrap: Arc::new(fluid_bootstrap),
        });
    }

    sections
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColumnAcquireDisposition {
    CacheHit,
    CacheMissGenerated,
    InflightDedupHit,
}

impl ColumnAcquireDisposition {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CacheHit => "cache_hit",
            Self::CacheMissGenerated => "cache_miss_generated",
            Self::InflightDedupHit => "inflight_dedup_hit",
        }
    }
}

#[derive(Clone)]
struct ColumnLease {
    column: Arc<GeneratedColumn>,
    disposition: ColumnAcquireDisposition,
}

struct InflightColumnEntry {
    ready: Mutex<Option<Arc<GeneratedColumn>>>,
    wake_all: Condvar,
}

impl InflightColumnEntry {
    fn new() -> Self {
        Self {
            ready: Mutex::new(None),
            wake_all: Condvar::new(),
        }
    }
}

struct GeneratedColumnCache {
    entries: HashMap<ColumnCacheKey, Arc<GeneratedColumn>>,
    lru: VecDeque<ColumnCacheKey>,
    max_entries: usize,
}

impl GeneratedColumnCache {
    fn new(max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            lru: VecDeque::new(),
            max_entries: max_entries.max(1),
        }
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn get(&mut self, key: &ColumnCacheKey) -> Option<Arc<GeneratedColumn>> {
        let entry = self.entries.get(key).cloned()?;
        self.touch(*key);
        Some(entry)
    }

    fn insert(&mut self, key: ColumnCacheKey, column: Arc<GeneratedColumn>) {
        self.entries.insert(key, column);
        self.touch(key);
        while self.entries.len() > self.max_entries {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }

    fn touch(&mut self, key: ColumnCacheKey) {
        if let Some(pos) = self.lru.iter().position(|entry| *entry == key) {
            self.lru.remove(pos);
        }
        self.lru.push_back(key);
    }
}

struct RecentGeneratedColumnHistory {
    entries: HashSet<ColumnCacheKey>,
    lru: VecDeque<ColumnCacheKey>,
    max_entries: usize,
}

impl RecentGeneratedColumnHistory {
    fn new(max_entries: usize) -> Self {
        Self {
            entries: HashSet::new(),
            lru: VecDeque::new(),
            max_entries: max_entries.max(1),
        }
    }

    fn record_generation(&mut self, key: ColumnCacheKey) -> bool {
        let regenerated = self.entries.contains(&key);
        if !regenerated {
            self.entries.insert(key);
        }
        if let Some(pos) = self.lru.iter().position(|entry| *entry == key) {
            self.lru.remove(pos);
        }
        self.lru.push_back(key);
        while self.entries.len() > self.max_entries {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
        regenerated
    }
}

#[derive(Default)]
struct GeneratedColumnCoordinatorMetrics {
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
    inflight_dedup_hits: AtomicU64,
    repeated_requests_avoided: AtomicU64,
    full_column_generations: AtomicU64,
    unique_columns_generated: AtomicU64,
    regenerated_columns: AtomicU64,
    section_extractions: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct GeneratedColumnCoordinatorSnapshot {
    cache_hits: u64,
    cache_misses: u64,
    inflight_dedup_hits: u64,
    repeated_requests_avoided: u64,
    full_column_generations: u64,
    unique_columns_generated: u64,
    regenerated_columns: u64,
    section_extractions: u64,
}

impl GeneratedColumnCoordinatorMetrics {
    fn snapshot(&self) -> GeneratedColumnCoordinatorSnapshot {
        GeneratedColumnCoordinatorSnapshot {
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.cache_misses.load(Ordering::Relaxed),
            inflight_dedup_hits: self.inflight_dedup_hits.load(Ordering::Relaxed),
            repeated_requests_avoided: self.repeated_requests_avoided.load(Ordering::Relaxed),
            full_column_generations: self.full_column_generations.load(Ordering::Relaxed),
            unique_columns_generated: self.unique_columns_generated.load(Ordering::Relaxed),
            regenerated_columns: self.regenerated_columns.load(Ordering::Relaxed),
            section_extractions: self.section_extractions.load(Ordering::Relaxed),
        }
    }
}

// Worker threads coordinate through this shared layer so cache hits avoid any
// heavy worldgen work, and concurrent misses for the same column collapse into
// one in-flight generation that the rest wait on.
struct GeneratedColumnCoordinator {
    cache: Mutex<GeneratedColumnCache>,
    inflight: Mutex<HashMap<ColumnCacheKey, Arc<InflightColumnEntry>>>,
    history: Mutex<RecentGeneratedColumnHistory>,
    metrics: GeneratedColumnCoordinatorMetrics,
}

impl GeneratedColumnCoordinator {
    fn new(max_entries: usize) -> Self {
        Self {
            cache: Mutex::new(GeneratedColumnCache::new(max_entries)),
            inflight: Mutex::new(HashMap::new()),
            history: Mutex::new(RecentGeneratedColumnHistory::new(
                GENERATED_COLUMN_HISTORY_MAX_ENTRIES.max(max_entries * 4),
            )),
            metrics: GeneratedColumnCoordinatorMetrics::default(),
        }
    }

    fn get_or_generate(
        &self,
        world_cfg: WorldConfig,
        preset: WorldgenPreset,
        chunk_x: i32,
        chunk_z: i32,
    ) -> ColumnLease {
        self.get_or_generate_with(world_cfg, preset, chunk_x, chunk_z, || {
            generate_streaming_column(world_cfg, preset, chunk_x, chunk_z)
        })
    }

    fn get_or_generate_with<F>(
        &self,
        world_cfg: WorldConfig,
        preset: WorldgenPreset,
        chunk_x: i32,
        chunk_z: i32,
        generate: F,
    ) -> ColumnLease
    where
        F: FnOnce() -> GeneratedColumn,
    {
        let key = ColumnCacheKey::new(ColumnCoordKey::new(chunk_x, chunk_z), world_cfg, preset);

        if let Some(column) = {
            let mut cache = self.cache.lock().expect("generated column cache poisoned");
            cache.get(&key)
        } {
            self.metrics.cache_hits.fetch_add(1, Ordering::Relaxed);
            self.metrics
                .repeated_requests_avoided
                .fetch_add(1, Ordering::Relaxed);
            let cache_entries = self.cache_entry_count();
            perf_trace::instant_event(
                "chunkgen_worker.column_cache",
                &self.instrumentation_counts("cache_hit", key, cache_entries),
            );
            return ColumnLease {
                column,
                disposition: ColumnAcquireDisposition::CacheHit,
            };
        }

        let (entry, should_generate) = {
            let mut inflight = self
                .inflight
                .lock()
                .expect("generated column inflight poisoned");
            if let Some(entry) = inflight.get(&key).cloned() {
                self.metrics
                    .inflight_dedup_hits
                    .fetch_add(1, Ordering::Relaxed);
                self.metrics
                    .repeated_requests_avoided
                    .fetch_add(1, Ordering::Relaxed);
                let cache_entries = self.cache_entry_count();
                perf_trace::instant_event(
                    "chunkgen_worker.column_inflight",
                    &self.instrumentation_counts("dedup_hit", key, cache_entries),
                );
                (entry, false)
            } else {
                let entry = Arc::new(InflightColumnEntry::new());
                inflight.insert(key, entry.clone());
                self.metrics.cache_misses.fetch_add(1, Ordering::Relaxed);
                let cache_entries = self.cache_entry_count();
                perf_trace::instant_event(
                    "chunkgen_worker.column_cache",
                    &self.instrumentation_counts("cache_miss", key, cache_entries),
                );
                (entry, true)
            }
        };

        if should_generate {
            let started = Instant::now();
            let column = Arc::new(generate());
            let regenerated = self
                .history
                .lock()
                .expect("generated column history poisoned")
                .record_generation(key);
            if regenerated {
                self.metrics
                    .regenerated_columns
                    .fetch_add(1, Ordering::Relaxed);
            } else {
                self.metrics
                    .unique_columns_generated
                    .fetch_add(1, Ordering::Relaxed);
            }
            let cache_entries = {
                let mut cache = self.cache.lock().expect("generated column cache poisoned");
                cache.insert(key, column.clone());
                cache.len()
            };

            {
                let mut ready = entry
                    .ready
                    .lock()
                    .expect("generated column inflight entry poisoned");
                *ready = Some(column.clone());
                entry.wake_all.notify_all();
            }

            self.inflight
                .lock()
                .expect("generated column inflight poisoned")
                .remove(&key);
            self.metrics
                .full_column_generations
                .fetch_add(1, Ordering::Relaxed);
            perf_trace::record_duration(
                "chunkgen_worker.full_column",
                started.elapsed(),
                &format!(
                    "{} regenerated={} {}",
                    self.instrumentation_counts("generated", key, cache_entries),
                    regenerated,
                    column.stage_timings.instrumentation_fields()
                ),
            );

            return ColumnLease {
                column,
                disposition: ColumnAcquireDisposition::CacheMissGenerated,
            };
        }

        let mut ready = entry
            .ready
            .lock()
            .expect("generated column inflight entry poisoned");
        while ready.is_none() {
            ready = entry
                .wake_all
                .wait(ready)
                .expect("generated column inflight wait poisoned");
        }
        ColumnLease {
            column: ready
                .as_ref()
                .expect("generated column should be ready after wait")
                .clone(),
            disposition: ColumnAcquireDisposition::InflightDedupHit,
        }
    }

    fn record_section_extraction(
        &self,
        coord: ChunkCoord,
        disposition: ColumnAcquireDisposition,
        extraction_time: Duration,
    ) {
        self.metrics
            .section_extractions
            .fetch_add(1, Ordering::Relaxed);
        let cache_entries = self.cache_entry_count();
        perf_trace::record_duration(
            "chunkgen_worker.section_extract",
            extraction_time,
            &format!(
                "coord=({}, {}, {}) column_state={} {}",
                coord.x,
                coord.y,
                coord.z,
                disposition.as_str(),
                self.instrumentation_suffix(cache_entries)
            ),
        );
    }

    #[cfg(test)]
    fn metrics_snapshot(&self) -> GeneratedColumnCoordinatorSnapshot {
        self.metrics.snapshot()
    }

    fn cache_entry_count(&self) -> usize {
        self.cache
            .lock()
            .expect("generated column cache poisoned")
            .len()
    }

    fn instrumentation_counts(
        &self,
        event: &str,
        key: ColumnCacheKey,
        cache_entries: usize,
    ) -> String {
        format!(
            "event={} coord=({}, {}) preset={:?} min_chunk_y={} max_chunk_y={} {}",
            event,
            key.coord.x,
            key.coord.z,
            key.descriptor.preset,
            key.descriptor.min_chunk_y(),
            key.descriptor.max_chunk_y(),
            self.instrumentation_suffix(cache_entries)
        )
    }

    fn instrumentation_suffix(&self, cache_entries: usize) -> String {
        let snapshot = self.metrics.snapshot();
        let cache_lookups = snapshot.cache_hits + snapshot.cache_misses;
        let cache_hit_rate = if cache_lookups == 0 {
            0.0
        } else {
            snapshot.cache_hits as f64 / cache_lookups as f64
        };
        let dedup_attempts = snapshot.cache_misses + snapshot.inflight_dedup_hits;
        let inflight_dedup_rate = if dedup_attempts == 0 {
            0.0
        } else {
            snapshot.inflight_dedup_hits as f64 / dedup_attempts as f64
        };
        format!(
            "cache_entries={} cache_hits={} cache_misses={} cache_hit_rate={:.3} inflight_dedup_hits={} inflight_dedup_rate={:.3} repeated_requests_avoided={} full_column_generations={} unique_columns_generated={} regenerated_columns={} section_extractions={}",
            cache_entries,
            snapshot.cache_hits,
            snapshot.cache_misses,
            cache_hit_rate,
            snapshot.inflight_dedup_hits,
            inflight_dedup_rate,
            snapshot.repeated_requests_avoided,
            snapshot.full_column_generations,
            snapshot.unique_columns_generated,
            snapshot.regenerated_columns,
            snapshot.section_extractions
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::thread;
    use std::time::Duration;

    use super::{
        CHUNK_SIZE, ColumnAcquireDisposition, ColumnCoordKey, ColumnGenerationDescriptor,
        ColumnGenerationStageTimings, GeneratedColumn, GeneratedColumnCoordinator, WORLD_MAX_Y,
        WinterColumnPassStats, WorldConfig, apply_winter_surface_to_column,
        build_generated_section_snapshots, generate_streaming_column_for_test,
    };
    use crate::blocks::BlockId;
    use crate::world::WorldEnvironment;
    use crate::world::fluids::{fluid_data_is_source_origin, fluid_data_is_worldgen_static};
    use crate::worldgen::ColumnBuffer;
    use crate::worldgen::WorldgenPreset;

    fn empty_generated_column(
        coord: ColumnCoordKey,
        descriptor: ColumnGenerationDescriptor,
    ) -> GeneratedColumn {
        let column = ColumnBuffer::new();
        GeneratedColumn {
            coord,
            descriptor,
            min_chunk_y: descriptor.min_chunk_y(),
            max_chunk_y: descriptor.max_chunk_y(),
            sections: build_generated_section_snapshots(
                coord,
                descriptor,
                &column,
                &vec![0; descriptor.section_count()],
                WinterColumnPassStats::default(),
            ),
            stage_timings: ColumnGenerationStageTimings::default(),
        }
    }

    #[test]
    fn generated_column_extracts_sections_relative_to_min_chunk_y() {
        let descriptor = ColumnGenerationDescriptor::new(
            WorldConfig {
                min_y: 32,
                max_y: 63,
                ..WorldConfig::default()
            },
            WorldgenPreset::ClassicFlatter,
        );
        let mut column = ColumnBuffer::new();
        column.set_block(0, 32, 0, BlockId::Stone);
        column.set_block(0, 48, 0, BlockId::Dirt);
        let coord = ColumnCoordKey::new(3, -2);
        let generated = GeneratedColumn {
            coord,
            descriptor,
            min_chunk_y: descriptor.min_chunk_y(),
            max_chunk_y: descriptor.max_chunk_y(),
            sections: build_generated_section_snapshots(
                coord,
                descriptor,
                &column,
                &[7, 11],
                WinterColumnPassStats::default(),
            ),
            stage_timings: ColumnGenerationStageTimings::default(),
        };

        let (low, low_stats, _) = generated.extract_chunk(2);
        let (high, high_stats, _) = generated.extract_chunk(3);

        assert_eq!(low.get(0, 0, 0), BlockId::Stone);
        assert_eq!(high.get(0, 0, 0), BlockId::Dirt);
        assert_eq!(low_stats.shoreline_sand_blocks, 7);
        assert_eq!(high_stats.shoreline_sand_blocks, 11);
    }

    #[test]
    fn column_coordinator_reuses_cached_columns() {
        let coordinator = GeneratedColumnCoordinator::new(4);
        let cfg = WorldConfig::default();
        let preset = WorldgenPreset::ClassicFlatter;
        let generate_calls = AtomicUsize::new(0);
        let coord = ColumnCoordKey::new(5, -1);
        let descriptor = ColumnGenerationDescriptor::new(cfg, preset);

        let first = coordinator.get_or_generate_with(cfg, preset, coord.x, coord.z, || {
            generate_calls.fetch_add(1, Ordering::SeqCst);
            empty_generated_column(coord, descriptor)
        });
        let second = coordinator.get_or_generate_with(cfg, preset, coord.x, coord.z, || {
            generate_calls.fetch_add(1, Ordering::SeqCst);
            empty_generated_column(coord, descriptor)
        });

        assert_eq!(
            first.disposition,
            ColumnAcquireDisposition::CacheMissGenerated
        );
        assert_eq!(second.disposition, ColumnAcquireDisposition::CacheHit);
        assert!(Arc::ptr_eq(&first.column, &second.column));
        assert_eq!(generate_calls.load(Ordering::SeqCst), 1);

        let snapshot = coordinator.metrics_snapshot();
        assert_eq!(snapshot.cache_hits, 1);
        assert_eq!(snapshot.cache_misses, 1);
        assert_eq!(snapshot.inflight_dedup_hits, 0);
        assert_eq!(snapshot.full_column_generations, 1);
        assert_eq!(snapshot.repeated_requests_avoided, 1);
        assert_eq!(snapshot.unique_columns_generated, 1);
        assert_eq!(snapshot.regenerated_columns, 0);
    }

    #[test]
    fn column_coordinator_deduplicates_inflight_columns() {
        let coordinator = Arc::new(GeneratedColumnCoordinator::new(4));
        let cfg = WorldConfig::default();
        let preset = WorldgenPreset::ClassicFlatter;
        let coord = ColumnCoordKey::new(9, 4);
        let descriptor = ColumnGenerationDescriptor::new(cfg, preset);
        let generate_calls = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();

        for _ in 0..6 {
            let coordinator = coordinator.clone();
            let generate_calls = generate_calls.clone();
            handles.push(thread::spawn(move || {
                coordinator.get_or_generate_with(cfg, preset, coord.x, coord.z, || {
                    generate_calls.fetch_add(1, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(25));
                    empty_generated_column(coord, descriptor)
                })
            }));
        }

        let mut generated = 0usize;
        let mut deduped = 0usize;
        for handle in handles {
            let lease = handle.join().expect("column dedup test thread panicked");
            match lease.disposition {
                ColumnAcquireDisposition::CacheMissGenerated => generated += 1,
                ColumnAcquireDisposition::InflightDedupHit => deduped += 1,
                ColumnAcquireDisposition::CacheHit => {}
            }
        }

        assert_eq!(generated, 1);
        assert_eq!(deduped, 5);
        assert_eq!(generate_calls.load(Ordering::SeqCst), 1);

        let snapshot = coordinator.metrics_snapshot();
        assert_eq!(snapshot.cache_hits, 0);
        assert_eq!(snapshot.cache_misses, 1);
        assert_eq!(snapshot.inflight_dedup_hits, 5);
        assert_eq!(snapshot.full_column_generations, 1);
        assert_eq!(snapshot.repeated_requests_avoided, 5);
        assert_eq!(snapshot.unique_columns_generated, 1);
    }

    fn tree_root_positions(column: &crate::worldgen::ColumnBuffer) -> Vec<(i32, i32, i32)> {
        let mut roots = Vec::new();
        for y in 1..256 {
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    if column.get_block(x, y, z) != BlockId::OakLog {
                        continue;
                    }
                    if column.get_block(x, y - 1, z) == BlockId::OakLog {
                        continue;
                    }
                    roots.push((x, y, z));
                }
            }
        }
        roots
    }

    fn count_block(column: &ColumnBuffer, target: BlockId) -> u32 {
        let mut count = 0u32;
        for y in 0..=WORLD_MAX_Y {
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    if column.get_block(x, y, z) == target {
                        count = count.saturating_add(1);
                    }
                }
            }
        }
        count
    }

    #[test]
    fn winter_surface_pass_places_snow_and_freezes_exposed_water() {
        let mut column = ColumnBuffer::new();
        column.set_block(3, 64, 3, BlockId::Grass);
        column.set_block(7, 62, 8, BlockId::WaterStill);

        let stats = apply_winter_surface_to_column(&mut column);

        assert_eq!(column.get_block(3, 65, 3), BlockId::Snow);
        assert_eq!(column.get_block(7, 62, 8), BlockId::Ice);
        assert_eq!(stats.snow_layers, 1);
        assert_eq!(stats.ice_blocks, 1);
    }

    #[test]
    fn trees_are_deterministic_for_same_seed_and_column() {
        let cfg = WorldConfig {
            seed: 0x5EED_BAAD_F00D_CAFE,
            ..WorldConfig::default()
        };
        let a = generate_streaming_column_for_test(cfg, WorldgenPreset::ClassicFlatter, 7, -4);
        let b = generate_streaming_column_for_test(cfg, WorldgenPreset::ClassicFlatter, 7, -4);
        assert_eq!(a.blocks, b.blocks);
        assert_eq!(a.data, b.data);
    }

    #[test]
    fn winter_worldgen_adds_surface_snow_over_temperate_generation() {
        let temperate_cfg = WorldConfig {
            seed: 0xA110_1004_55AA_7711,
            environment: WorldEnvironment::Temperate,
            ..WorldConfig::default()
        };
        let winter_cfg = WorldConfig {
            environment: WorldEnvironment::Winter,
            ..temperate_cfg
        };
        let preset = WorldgenPreset::ClassicFlatter;
        let mut winter_snow = 0u32;
        let mut temperate_snow = 0u32;

        for cz in -2..=2 {
            for cx in -2..=2 {
                let temperate = generate_streaming_column_for_test(temperate_cfg, preset, cx, cz);
                let winter = generate_streaming_column_for_test(winter_cfg, preset, cx, cz);
                temperate_snow =
                    temperate_snow.saturating_add(count_block(&temperate, BlockId::Snow));
                winter_snow = winter_snow.saturating_add(count_block(&winter, BlockId::Snow));
            }
        }

        assert_eq!(temperate_snow, 0);
        assert!(
            winter_snow > 0,
            "winter worldgen did not place any snow layers"
        );
    }

    #[test]
    fn tree_roots_spawn_on_grass_or_dirt_only() {
        let cfg = WorldConfig {
            seed: 0x1A2B_3C4D_5E6F_7788,
            ..WorldConfig::default()
        };
        let preset = WorldgenPreset::ClassicFlatter;
        let mut found = None;
        for ring in 0..=24 {
            for cz in -ring..=ring {
                for cx in -ring..=ring {
                    let column = generate_streaming_column_for_test(cfg, preset, cx, cz);
                    let roots = tree_root_positions(&column);
                    if roots.is_empty() {
                        continue;
                    }
                    found = Some((column, roots, cx, cz));
                    break;
                }
                if found.is_some() {
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }

        let (column, roots, cx, cz) = found.expect("no trees found in search area");
        for (x, y, z) in roots {
            let below = column.get_block(x, y - 1, z);
            assert!(
                matches!(below, BlockId::Grass | BlockId::Dirt),
                "tree root in column ({cx},{cz}) has invalid ground at ({x},{y},{z}): {:?}",
                below
            );
            assert_ne!(
                below,
                BlockId::Sand,
                "tree root in column ({cx},{cz}) spawned on sand at ({x},{y},{z})"
            );
        }
    }

    #[test]
    fn top_surface_dirt_gets_promoted_to_grass() {
        let cfg = WorldConfig {
            seed: 0xC0FF_EE12_3499_ABCD,
            ..WorldConfig::default()
        };
        let preset = WorldgenPreset::ClassicFlatter;

        for cz in -2..=2 {
            for cx in -2..=2 {
                let column = generate_streaming_column_for_test(cfg, preset, cx, cz);
                for z in 0..CHUNK_SIZE {
                    for x in 0..CHUNK_SIZE {
                        for y in (1..=255).rev() {
                            let block = column.get_block(x, y, z);
                            if block == BlockId::Air {
                                continue;
                            }
                            if matches!(
                                block,
                                BlockId::WaterStill
                                    | BlockId::WaterFlowing
                                    | BlockId::LavaStill
                                    | BlockId::LavaFlowing
                            ) {
                                break;
                            }
                            assert_ne!(
                                block,
                                BlockId::Dirt,
                                "top block remained dirt at column ({cx},{cz}) local=({x},{z}) y={y}"
                            );
                            break;
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn shoreline_and_banks_do_not_create_floating_sand() {
        let cfg = WorldConfig {
            seed: 0xBADA_55EE_D15C_0DED,
            ..WorldConfig::default()
        };
        let preset = WorldgenPreset::ClassicFlatter;

        for cz in -4..=4 {
            for cx in -4..=4 {
                let column = generate_streaming_column_for_test(cfg, preset, cx, cz);
                for y in 1..=255 {
                    for z in 0..CHUNK_SIZE {
                        for x in 0..CHUNK_SIZE {
                            if column.get_block(x, y, z) != BlockId::Sand {
                                continue;
                            }
                            let below = column.get_block(x, y - 1, z);
                            assert!(
                                below.is_solid(),
                                "floating sand in column ({cx},{cz}) at local=({x},{y},{z}) over {:?}",
                                below
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn worldgen_water_regression_guards_hold_for_fixed_seed() {
        const WATER_COLUMN_LIMIT: u32 = 40;
        let cfg = WorldConfig {
            seed: 0xA11C_E5EE_DA7A_1122,
            ..WorldConfig::default()
        };
        let preset = WorldgenPreset::ClassicFlatter;
        let mut worldgen_water_blocks = 0u32;
        let mut worldgen_source_blocks = 0u32;

        for cz in -4..=4 {
            for cx in -4..=4 {
                let column = generate_streaming_column_for_test(cfg, preset, cx, cz);
                for z in 0..CHUNK_SIZE {
                    for x in 0..CHUNK_SIZE {
                        let mut water_in_vertical_column = 0u32;
                        for y in 1..=255 {
                            let block = column.get_block(x, y, z);
                            if block == BlockId::Sand {
                                let below = column.get_block(x, y - 1, z);
                                assert!(
                                    below.is_solid(),
                                    "floating sand in regression test at chunk ({cx},{cz}) local=({x},{y},{z}) over {:?}",
                                    below
                                );
                            }
                            if !matches!(block, BlockId::WaterStill | BlockId::WaterFlowing) {
                                continue;
                            }
                            water_in_vertical_column = water_in_vertical_column.saturating_add(1);
                            let data = column.get_data(x, y, z);
                            if fluid_data_is_worldgen_static(data) {
                                worldgen_water_blocks = worldgen_water_blocks.saturating_add(1);
                            }
                            if fluid_data_is_source_origin(data) {
                                worldgen_source_blocks = worldgen_source_blocks.saturating_add(1);
                            }
                        }
                        assert!(
                            water_in_vertical_column <= WATER_COLUMN_LIMIT,
                            "water sheet regression at chunk ({cx},{cz}) local=({x},{z}) height_count={} limit={WATER_COLUMN_LIMIT}",
                            water_in_vertical_column
                        );
                    }
                }
            }
        }

        assert!(
            worldgen_water_blocks > 0,
            "regression test sampled no worldgen static water; seed/area no longer exercises water features"
        );
        assert_eq!(
            worldgen_source_blocks, 0,
            "worldgen introduced {} water source-origin cells; expected 0 to avoid runaway floods",
            worldgen_source_blocks
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct LodColumnCoord {
    pub(crate) x: i32,
    pub(crate) z: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChunkGenRequestKind {
    FullChunk,
    WarmupColumnOnly,
    LodColumnOnly,
}

fn lod_color_for_height(y: i32) -> [f32; 3] {
    let t = (y as f32 / WORLD_MAX_Y.max(1) as f32).clamp(0.0, 1.0);
    [0.24 + 0.30 * t, 0.28 + 0.45 * t, 0.18 + 0.18 * t]
}

fn build_lod_mesh_for_column(
    world_cfg: &WorldConfig,
    preset: WorldgenPreset,
    x: i32,
    z: i32,
) -> CpuMeshData {
    let terrain_cfg = WorldGenConfig::from_preset_with_sea_level(
        worldgen_seed32(world_cfg.seed),
        preset,
        world_cfg.sea_level(),
    );
    let terrain_sampler = TerrainSampler::new(terrain_cfg);
    let mut mesh = CpuMeshData::default();
    let cells = LOD_GRID_CELLS_PER_CHUNK.max(1);
    let verts_per_row = (cells + 1) as usize;
    mesh.vertices.reserve(verts_per_row * verts_per_row);
    mesh.indices.reserve((cells * cells * 6) as usize);

    let base_x = x * CHUNK_SIZE;
    let base_z = z * CHUNK_SIZE;
    for gz in 0..=cells {
        let zf = gz as f32 / cells as f32;
        let sample_z = base_z + (gz * CHUNK_SIZE).div_euclid(cells);
        for gx in 0..=cells {
            let xf = gx as f32 / cells as f32;
            let sample_x = base_x + (gx * CHUNK_SIZE).div_euclid(cells);
            let surface_y = terrain_sampler
                .height_at(sample_x, sample_z, WORLD_MAX_Y)
                .clamp(0, WORLD_MAX_Y);
            let world_x = base_x as f32 + xf * CHUNK_SIZE as f32;
            let world_z = base_z as f32 + zf * CHUNK_SIZE as f32;
            mesh.vertices.push(Vertex {
                position: [world_x, surface_y as f32 + 1.0, world_z],
                uv: [xf, zf],
                tex_layer: 0,
                color: lod_color_for_height(surface_y),
                tint_kind: 0,
                light_packed: FULLBRIGHT_LIGHT_PACKED,
            });
        }
    }

    let idx = |ix: i32, iz: i32| -> u32 { (iz as u32) * (cells as u32 + 1) + ix as u32 };
    for zc in 0..cells {
        for xc in 0..cells {
            let i00 = idx(xc, zc);
            let i10 = idx(xc + 1, zc);
            let i01 = idx(xc, zc + 1);
            let i11 = idx(xc + 1, zc + 1);
            mesh.indices
                .extend_from_slice(&[i00, i01, i10, i10, i01, i11]);
        }
    }

    mesh
}

pub(crate) struct ChunkGenJob {
    pub(crate) coord: ChunkCoord,
    pub(crate) preset: WorldgenPreset,
    pub(crate) world_cfg: WorldConfig,
    pub(crate) request_kind: ChunkGenRequestKind,
    pub(crate) epoch: u64,
}

pub(crate) struct ChunkGenResult {
    pub(crate) coord: ChunkCoord,
    pub(crate) chunk: Option<Chunk>,
    pub(crate) fluid_bootstrap: Vec<GeneratedFluidBootstrapCell>,
    pub(crate) worldgen_debug: GeneratedWorldgenDebugStats,
    pub(crate) lod_column: Option<LodColumnCoord>,
    pub(crate) lod_mesh: Option<CpuMeshData>,
    pub(crate) epoch: u64,
    pub(crate) request_kind: ChunkGenRequestKind,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct GeneratedWorldgenDebugStats {
    pub(crate) worldgen_water_blocks: u32,
    pub(crate) worldgen_water_source_blocks: u32,
    pub(crate) shoreline_sand_blocks: u32,
    pub(crate) winter_snow_blocks: u32,
    pub(crate) winter_ice_blocks: u32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct GeneratedFluidBootstrapCell {
    pub(crate) pos: IVec3,
    pub(crate) kind: FluidKind,
}

fn generated_fluid_cell_needs_bootstrap_local(
    blocks: &[BlockId],
    local_x: i32,
    local_y: i32,
    local_z: i32,
    kind: FluidKind,
    is_source: bool,
) -> bool {
    if local_y <= 0 {
        return true;
    }
    if fluid_kind(blocks[Chunk::index(local_x, local_y - 1, local_z)]) != Some(kind) {
        return true;
    }

    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        let nx = local_x + dx;
        let nz = local_z + dz;
        if !(0..CHUNK_SIZE).contains(&nx) || !(0..CHUNK_SIZE).contains(&nz) {
            return true;
        }
        if fluid_kind(blocks[Chunk::index(nx, local_y, nz)]) != Some(kind) {
            return true;
        }
    }

    if is_source {
        return false;
    }

    if local_y >= CHUNK_SIZE - 1 {
        return true;
    }
    fluid_kind(blocks[Chunk::index(local_x, local_y + 1, local_z)]) != Some(kind)
}

fn analyze_generated_section_buffers(
    blocks: &[BlockId],
    block_data: &[u8],
    shoreline_sand_blocks: u32,
    coord: ChunkCoord,
) -> (
    u16,
    u16,
    GeneratedWorldgenDebugStats,
    Vec<GeneratedFluidBootstrapCell>,
) {
    debug_assert_eq!(
        blocks.len(),
        (CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE) as usize
    );
    debug_assert_eq!(block_data.len(), blocks.len());
    let chunk_x0 = coord.x * CHUNK_SIZE;
    let chunk_y0 = coord.y * CHUNK_SIZE;
    let chunk_z0 = coord.z * CHUNK_SIZE;
    let mut out = Vec::with_capacity(32);
    let mut filled_voxel_count = 0u16;
    let mut solid_voxel_count = 0u16;
    let mut worldgen_water_blocks = 0u32;
    let mut worldgen_water_source_blocks = 0u32;
    let mut winter_snow_blocks = 0u32;
    let mut winter_ice_blocks = 0u32;

    for local_y in 0..CHUNK_SIZE {
        for local_z in 0..CHUNK_SIZE {
            for local_x in 0..CHUNK_SIZE {
                let index = Chunk::index(local_x, local_y, local_z);
                let block = blocks[index];
                if block.is_voxel_filled() {
                    filled_voxel_count = filled_voxel_count.saturating_add(1);
                }
                if block.is_solid() {
                    solid_voxel_count = solid_voxel_count.saturating_add(1);
                }
                if block == BlockId::Snow {
                    winter_snow_blocks = winter_snow_blocks.saturating_add(1);
                } else if block == BlockId::Ice {
                    winter_ice_blocks = winter_ice_blocks.saturating_add(1);
                }
                let Some(kind) = fluid_kind(block) else {
                    continue;
                };
                let data = block_data[index];
                if is_water_block(block) {
                    if fluid_data_is_worldgen_static(data) {
                        worldgen_water_blocks = worldgen_water_blocks.saturating_add(1);
                    }
                    if fluid_data_is_source_origin(data) {
                        worldgen_water_source_blocks =
                            worldgen_water_source_blocks.saturating_add(1);
                    }
                }
                if fluid_data_is_worldgen_static(data) {
                    continue;
                }
                let is_source = fluid_data_is_source_origin(data);
                if !generated_fluid_cell_needs_bootstrap_local(
                    blocks, local_x, local_y, local_z, kind, is_source,
                ) {
                    continue;
                }
                out.push(GeneratedFluidBootstrapCell {
                    pos: IVec3::new(chunk_x0 + local_x, chunk_y0 + local_y, chunk_z0 + local_z),
                    kind,
                });
            }
        }
    }

    (
        filled_voxel_count,
        solid_voxel_count,
        GeneratedWorldgenDebugStats {
            worldgen_water_blocks,
            worldgen_water_source_blocks,
            shoreline_sand_blocks,
            winter_snow_blocks,
            winter_ice_blocks,
        },
        out,
    )
}

pub(crate) struct ChunkGenWorkerPool {
    shutdown_tx: Sender<()>,
    workers: Vec<thread::JoinHandle<()>>,
}

impl ChunkGenWorkerPool {
    pub(crate) fn new(job_rx: Receiver<ChunkGenJob>, result_tx: Sender<ChunkGenResult>) -> Self {
        let worker_count = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .max(1);
        let generated_column_coordinator = Arc::new(GeneratedColumnCoordinator::new(
            GENERATED_COLUMN_CACHE_MAX_ENTRIES,
        ));
        let (shutdown_tx, shutdown_rx) = crossbeam_channel::unbounded();
        let mut workers = Vec::with_capacity(worker_count);

        for worker_index in 0..worker_count {
            let job_rx = job_rx.clone();
            let result_tx = result_tx.clone();
            let shutdown_rx = shutdown_rx.clone();
            let generated_column_coordinator = generated_column_coordinator.clone();
            let builder = thread::Builder::new().name(format!("chunkgen-worker-{worker_index}"));
            workers.push(builder.spawn(move || {
                loop {
                    select! {
                        recv(shutdown_rx) -> _ => break,
                        recv(job_rx) -> msg => {
                            let Ok(job) = msg else {
                                break;
                            };
                            let started = std::time::Instant::now();

                            match job.request_kind {
                                ChunkGenRequestKind::FullChunk => {
                                    let column_lease = generated_column_coordinator.get_or_generate(
                                        job.world_cfg,
                                        job.preset,
                                        job.coord.x,
                                        job.coord.z,
                                    );
                                    let section_started = Instant::now();
                                    let (mut chunk, worldgen_debug, fluid_bootstrap) =
                                        column_lease.column.extract_chunk(job.coord.y);
                                    debug_assert_eq!(
                                        column_lease.column.coord,
                                        ColumnCoordKey::new(job.coord.x, job.coord.z)
                                    );
                                    generated_column_coordinator.record_section_extraction(
                                        job.coord,
                                        column_lease.disposition,
                                        section_started.elapsed(),
                                    );
                                    if !ENABLE_WORLD_FLUID_FEATURES {
                                        apply_post_fixes_to_chunk(&mut chunk);
                                    }
                                    let fluid_bootstrap_count = fluid_bootstrap.len();
                                    let (lod_column, lod_mesh) = if ENABLE_FAR_LOD_PIGGYBACK_ON_FULL_CHUNK_RESULTS
                                        && job.coord.y == 0
                                    {
                                        let coord = LodColumnCoord {
                                            x: job.coord.x,
                                            z: job.coord.z,
                                        };
                                        (
                                            Some(coord),
                                            Some(build_lod_mesh_for_column(
                                                &job.world_cfg,
                                                job.preset,
                                                coord.x,
                                                coord.z,
                                            )),
                                        )
                                    } else {
                                        (None, None)
                                    };
                                    if result_tx.send(ChunkGenResult {
                                        coord: job.coord,
                                        chunk: Some(chunk),
                                        fluid_bootstrap,
                                        worldgen_debug,
                                        lod_column,
                                        lod_mesh,
                                        epoch: job.epoch,
                                        request_kind: ChunkGenRequestKind::FullChunk,
                                    }).is_err() {
                                        break;
                                    }
                                    perf_trace::record_duration(
                                        "chunkgen_worker.full_chunk",
                                        started.elapsed(),
                                        &format!(
                                            "coord=({}, {}, {}) column_state={} lod_generated={} bootstrap_cells={} worldgen_water_blocks={} shoreline_sand_blocks={}",
                                            job.coord.x,
                                            job.coord.y,
                                            job.coord.z,
                                            column_lease.disposition.as_str(),
                                            ENABLE_FAR_LOD_PIGGYBACK_ON_FULL_CHUNK_RESULTS
                                                && job.coord.y == 0,
                                            fluid_bootstrap_count,
                                            worldgen_debug.worldgen_water_blocks,
                                            worldgen_debug.shoreline_sand_blocks
                                        ),
                                    );
                                }
                                ChunkGenRequestKind::WarmupColumnOnly => {
                                    let column_lease = generated_column_coordinator.get_or_generate(
                                        job.world_cfg,
                                        job.preset,
                                        job.coord.x,
                                        job.coord.z,
                                    );
                                    if result_tx.send(ChunkGenResult {
                                        coord: job.coord,
                                        chunk: None,
                                        fluid_bootstrap: Vec::new(),
                                        worldgen_debug: GeneratedWorldgenDebugStats::default(),
                                        lod_column: None,
                                        lod_mesh: None,
                                        epoch: job.epoch,
                                        request_kind: ChunkGenRequestKind::WarmupColumnOnly,
                                    }).is_err() {
                                        break;
                                    }
                                    perf_trace::record_duration(
                                        "chunkgen_worker.warmup_column",
                                        started.elapsed(),
                                        &format!(
                                            "coord=({}, {}) column_state={}",
                                            job.coord.x,
                                            job.coord.z,
                                            column_lease.disposition.as_str(),
                                        ),
                                    );
                                }
                                ChunkGenRequestKind::LodColumnOnly => {
                                    let lod_column = LodColumnCoord {
                                        x: job.coord.x,
                                        z: job.coord.z,
                                    };
                                    let lod_mesh = Some(build_lod_mesh_for_column(
                                        &job.world_cfg,
                                        job.preset,
                                        lod_column.x,
                                        lod_column.z,
                                    ));
                                    if result_tx.send(ChunkGenResult {
                                        coord: job.coord,
                                        chunk: None,
                                        fluid_bootstrap: Vec::new(),
                                        worldgen_debug: GeneratedWorldgenDebugStats::default(),
                                        lod_column: Some(lod_column),
                                        lod_mesh,
                                        epoch: job.epoch,
                                        request_kind: ChunkGenRequestKind::LodColumnOnly,
                                    }).is_err() {
                                        break;
                                    }
                                    perf_trace::record_duration(
                                        "chunkgen_worker.lod_only",
                                        started.elapsed(),
                                        &format!("coord=({}, {})", lod_column.x, lod_column.z),
                                    );
                                }
                            }
                        }
                    }
                }
            }).expect("failed to spawn chunkgen worker"));
        }

        Self {
            shutdown_tx,
            workers,
        }
    }
}

impl Drop for ChunkGenWorkerPool {
    fn drop(&mut self) {
        for _ in 0..self.workers.len() {
            let _ = self.shutdown_tx.send(());
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
