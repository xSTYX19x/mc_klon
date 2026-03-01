use std::thread;

use crossbeam_channel::{Receiver, Sender, select};
use glam::IVec3;

use crate::blocks::BlockId;
use crate::meshing::{CpuMeshData, FULLBRIGHT_LIGHT_PACKED, Vertex};
use crate::perf::trace as perf_trace;
use crate::world::config::WorldConfig;
use crate::world::fluids::{FluidKind, fluid_data_is_source_origin, fluid_data_make, fluid_kind};
use crate::world::{CHUNK_SIZE, Chunk, ChunkCoord, WORLD_MAX_Y};
use crate::worldgen::{
    ChunkGenerator, ColumnBuffer, GenContext, TerrainGenerator, TerrainSampler, WorldGenConfig,
    WorldGenerator, WorldgenPreset,
    carvers::Carvers,
};

const WATER_FEATURE_CHANCE_PER_CHUNK_256: u32 = 24;
const WATER_FEATURE_RIVER_RATIO_256: u32 = 128;
const LAVA_LAKE_CHANCE_PER_CHUNK_256: u32 = 6;
const WATER_LAKE_R_MIN: i32 = 3;
const WATER_LAKE_R_MAX: i32 = 7;
const LAVA_LAKE_R_MIN: i32 = 2;
const LAVA_LAKE_R_MAX: i32 = 5;
const LAKE_DEPTH_MIN: i32 = 3;
const LAKE_DEPTH_MAX: i32 = 7;
const SHORE_RADIUS: i32 = 2;
const WATER_SPRING_MIN_SURFACE_Y: i32 = 30;
const WATER_SPRING_PATH_MIN: i32 = 8;
const WATER_SPRING_PATH_MAX: i32 = 24;
const LOD_GRID_CELLS_PER_CHUNK: i32 = 4;
const ENABLE_FAR_LOD: bool = false;
const ENABLE_WORLD_FLUID_FEATURES: bool = true;

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
    fill: BlockId,
}

#[derive(Clone, Debug)]
struct WaterSpring {
    source_x: i32,
    source_y: i32,
    source_z: i32,
    path: Vec<(i32, i32, i32)>,
}

#[derive(Clone, Debug)]
enum WaterFeature {
    Lake(LakeCandidate),
    Spring(WaterSpring),
}

fn worldgen_seed32(seed: u64) -> u32 {
    let mixed = seed ^ (seed >> 32);
    (mixed as u32).wrapping_mul(0x9E37_79B9)
}

fn preset_seed_mix(preset: WorldgenPreset) -> u64 {
    match preset {
        WorldgenPreset::Flatish => 0xA1F2_63D7_8BC9_1023,
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

fn lake_local_depth(lake: LakeCandidate, world_x: i32, world_z: i32) -> Option<i32> {
    let dx = world_x - lake.cx;
    let dz = world_z - lake.cz;
    let dist2 = dx * dx + dz * dz;
    let radius_sq = lake.radius * lake.radius;
    if dist2 > radius_sq {
        return None;
    }
    let t = dist2 as f32 / radius_sq as f32;
    let local_depth = ((1.0 - t).sqrt() * lake.depth as f32).ceil() as i32;
    (local_depth > 0).then_some(local_depth)
}

fn make_water_lake_candidate(
    source_chunk_x: i32,
    source_chunk_z: i32,
    terrain: &TerrainSampler,
    sea_level: i32,
    rng: &mut SplitMix64,
) -> Option<LakeCandidate> {
    let wx = source_chunk_x * CHUNK_SIZE + rng.range_i32(0, CHUNK_SIZE - 1);
    let wz = source_chunk_z * CHUNK_SIZE + rng.range_i32(0, CHUNK_SIZE - 1);
    let surface_y = terrain.height_at(wx, wz, WORLD_MAX_Y);
    let lake_cap_y = (sea_level - 1).clamp(6, WORLD_MAX_Y - 2);
    let lake_y = (surface_y - rng.range_i32(2, 5)).clamp(6, lake_cap_y);
    Some(LakeCandidate {
        cx: wx,
        cy: lake_y,
        cz: wz,
        radius: rng.range_i32(WATER_LAKE_R_MIN, WATER_LAKE_R_MAX),
        depth: rng.range_i32(LAKE_DEPTH_MIN, LAKE_DEPTH_MAX),
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
        fill: BlockId::LavaStill,
    })
}

fn collect_nearby_water_features(
    world_cfg: &WorldConfig,
    preset: WorldgenPreset,
    coord: ChunkCoord,
    terrain: &TerrainSampler,
) -> (Vec<LakeCandidate>, Vec<WaterSpring>, Vec<LakeCandidate>) {
    let mut water = Vec::with_capacity(16);
    let mut springs = Vec::with_capacity(8);
    let mut lava = Vec::with_capacity(4);
    for source_z in (coord.z - 3)..=(coord.z + 3) {
        for source_x in (coord.x - 3)..=(coord.x + 3) {
            if let Some(feature) =
                choose_water_feature_for_source(
                    world_cfg.seed,
                    preset,
                    source_x,
                    source_z,
                    terrain,
                    world_cfg.sea_level(),
                )
            {
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
    let source_cap_y = (sea_level - 1).clamp(2, WORLD_MAX_Y - 2);
    let source_y = (surface_y - 1).clamp(2, source_cap_y);

    let mut x = source_x;
    let mut z = source_z;
    let mut y = source_y;
    let path_len = rng.range_i32(WATER_SPRING_PATH_MIN, WATER_SPRING_PATH_MAX);
    let mut path = Vec::with_capacity(path_len as usize);
    let mut prev_dir = (0_i32, 0_i32);

    for _ in 0..path_len {
        let mut best: Option<(i32, i32, i32, i32)> = None;
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            let nx = x + dx;
            let nz = z + dz;
            let n_surface = terrain.height_at(nx, nz, WORLD_MAX_Y);
            let mut score = n_surface;
            if (dx, dz) != prev_dir {
                score += 1;
            }
            score += rng.range_i32(0, 1);

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

        // Keine starken "Bergauf"-Sprünge: Quelle soll bergab/eben auslaufen.
        if n_surface > y + 1 {
            break;
        }

        let bed_y = (n_surface - 1).clamp(1, WORLD_MAX_Y - 2);
        y = y.min(bed_y);
        if y <= 1 {
            break;
        }

        prev_dir = (nx - x, nz - z);
        x = nx;
        z = nz;
        path.push((x, y, z));
    }

    if path.len() < 6 {
        return None;
    }

    Some(WaterSpring {
        source_x,
        source_y,
        source_z,
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
        if let Some(spring) =
            make_water_spring_candidate(source_chunk_x, source_chunk_z, terrain, sea_level, &mut rng)
        {
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
        if let Some(spring) =
            make_water_spring_candidate(source_chunk_x, source_chunk_z, terrain, sea_level, &mut rng)
        {
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

fn block_from_base_terrain(y: i32, surface_y: i32, dirt_depth: i32) -> BlockId {
    if y <= 0 {
        return BlockId::Bedrock;
    }
    if y > surface_y {
        return BlockId::Air;
    }
    if y == surface_y {
        return BlockId::Grass;
    }
    if y >= surface_y - dirt_depth {
        return BlockId::Dirt;
    }
    BlockId::Stone
}

fn block_after_lakes(
    world_x: i32,
    world_y: i32,
    world_z: i32,
    surface_y: i32,
    dirt_depth: i32,
    water_lakes: &[LakeCandidate],
    lava_lakes: &[LakeCandidate],
) -> BlockId {
    let mut has_water = false;
    for &lake in water_lakes {
        let Some(local_depth) = lake_local_depth(lake, world_x, world_z) else {
            continue;
        };
        let y_min = (lake.cy - local_depth).max(1);
        let y_max = lake.cy.min(WORLD_MAX_Y);
        if (y_min..=y_max).contains(&world_y) {
            has_water = true;
            break;
        }
    }

    for &lake in lava_lakes {
        let Some(local_depth) = lake_local_depth(lake, world_x, world_z) else {
            continue;
        };
        let y_min = (lake.cy - local_depth).max(1);
        let y_max = lake.cy.min(WORLD_MAX_Y);
        if (y_min..=y_max).contains(&world_y) {
            return lake.fill;
        }
    }

    if has_water {
        return BlockId::WaterStill;
    }

    block_from_base_terrain(world_y, surface_y, dirt_depth)
}

fn apply_lake_carve_and_fill_to_chunk(chunk: &mut Chunk, coord: ChunkCoord, lake: LakeCandidate) {
    let chunk_y0 = coord.y * CHUNK_SIZE;
    let chunk_y1 = chunk_y0 + CHUNK_SIZE - 1;
    let chunk_x0 = coord.x * CHUNK_SIZE;
    let chunk_z0 = coord.z * CHUNK_SIZE;

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
                    // Variante: Lake-Interior wird als explizite Worldgen-Quelle markiert.
                    // So entstehen keine "stills ohne source_origin"-Sonderfaelle.
                    BlockId::WaterStill | BlockId::LavaStill => fluid_data_make(0, false, true),
                    _ => 0,
                };
                chunk.set_data(local_x, local_y, local_z, data);
            }
        }
    }
}

fn apply_shoreline_pass_to_chunk(
    chunk: &mut Chunk,
    coord: ChunkCoord,
    terrain: &TerrainSampler,
    sea_level: i32,
    dirt_depth: i32,
    water_lakes: &[LakeCandidate],
    lava_lakes: &[LakeCandidate],
) {
    let chunk_x0 = coord.x * CHUNK_SIZE;
    let chunk_y0 = coord.y * CHUNK_SIZE;
    let chunk_y1 = chunk_y0 + CHUNK_SIZE - 1;
    let chunk_z0 = coord.z * CHUNK_SIZE;

    for local_z in 0..CHUNK_SIZE {
        let world_z = chunk_z0 + local_z;
        for local_x in 0..CHUNK_SIZE {
            let world_x = chunk_x0 + local_x;
            let surface_y = terrain.height_at(world_x, world_z, WORLD_MAX_Y);

            let mut top: Option<(i32, BlockId)> = None;
            for world_y in (1..=WORLD_MAX_Y).rev() {
                let block = block_after_lakes(
                    world_x,
                    world_y,
                    world_z,
                    surface_y,
                    dirt_depth,
                    water_lakes,
                    lava_lakes,
                );
                if block == BlockId::Air || is_fluid(block) {
                    continue;
                }
                top = Some((world_y, block));
                break;
            }
            let Some((top_y, top_block)) = top else {
                continue;
            };
            let near_sea_level = (top_y - sea_level).abs() <= SHORE_RADIUS * 4;

            let mut near_water = false;
            'near: for dz in -SHORE_RADIUS..=SHORE_RADIUS {
                for dx in -SHORE_RADIUS..=SHORE_RADIUS {
                    let nx = world_x + dx;
                    let nz = world_z + dz;
                    let neighbor_surface = terrain.height_at(nx, nz, WORLD_MAX_Y);
                    for ny in (top_y - 1)..=(top_y + 1) {
                        if !(1..=WORLD_MAX_Y).contains(&ny) {
                            continue;
                        }
                        let neighbor_block = block_after_lakes(
                            nx,
                            ny,
                            nz,
                            neighbor_surface,
                            dirt_depth,
                            water_lakes,
                            lava_lakes,
                        );
                        if neighbor_block == BlockId::WaterStill {
                            near_water = true;
                            break 'near;
                        }
                    }
                }
            }

            let block_above = if top_y < WORLD_MAX_Y {
                block_after_lakes(
                    world_x,
                    top_y + 1,
                    world_z,
                    surface_y,
                    dirt_depth,
                    water_lakes,
                    lava_lakes,
                )
            } else {
                BlockId::Air
            };
            let water_above = matches!(block_above, BlockId::WaterStill | BlockId::WaterFlowing);

            let mut writes: Vec<(i32, BlockId)> = Vec::with_capacity(4);
            if near_sea_level && near_water && matches!(top_block, BlockId::Grass | BlockId::Dirt)
            {
                writes.push((top_y, BlockId::Sand));
                for depth in 1..=3 {
                    let y = top_y - depth;
                    if y <= 0 {
                        break;
                    }
                    let current = block_after_lakes(
                        world_x,
                        y,
                        world_z,
                        surface_y,
                        dirt_depth,
                        water_lakes,
                        lava_lakes,
                    );
                    if current == BlockId::Dirt {
                        writes.push((y, BlockId::Sand));
                    }
                }
            } else if water_above && top_block == BlockId::Grass {
                writes.push((top_y, BlockId::Dirt));
            }

            for (world_y, block) in writes {
                if world_y < chunk_y0 || world_y > chunk_y1 {
                    continue;
                }
                let local_y = world_y - chunk_y0;
                chunk.set(local_x, local_y, local_z, block);
                chunk.set_data(local_x, local_y, local_z, 0);
            }
        }
    }
}

fn apply_streaming_lakes_and_shores(
    chunk: &mut Chunk,
    coord: ChunkCoord,
    world_cfg: &WorldConfig,
    preset: WorldgenPreset,
) {
    let terrain_cfg = WorldGenConfig::from_preset_with_sea_level(
        worldgen_seed32(world_cfg.seed),
        preset,
        world_cfg.sea_level(),
    );
    let terrain_sampler = TerrainSampler::new(terrain_cfg);
    let dirt_depth = terrain_cfg.dirt_depth.max(1);
    let (water_lakes, springs, lava_lakes) =
        collect_nearby_water_features(world_cfg, preset, coord, &terrain_sampler);

    for &lake in &water_lakes {
        apply_lake_carve_and_fill_to_chunk(chunk, coord, lake);
    }
    for &lake in &lava_lakes {
        apply_lake_carve_and_fill_to_chunk(chunk, coord, lake);
    }

    apply_shoreline_pass_to_chunk(
        chunk,
        coord,
        &terrain_sampler,
        world_cfg.sea_level(),
        dirt_depth,
        &water_lakes,
        &lava_lakes,
    );

    let chunk_x0 = coord.x * CHUNK_SIZE;
    let chunk_y0 = coord.y * CHUNK_SIZE;
    let chunk_y1 = chunk_y0 + CHUNK_SIZE - 1;
    let chunk_z0 = coord.z * CHUNK_SIZE;
    for spring in springs {
        let sx = spring.source_x;
        let sy = spring.source_y;
        let sz = spring.source_z;
        if (chunk_x0..(chunk_x0 + CHUNK_SIZE)).contains(&sx)
            && (chunk_z0..(chunk_z0 + CHUNK_SIZE)).contains(&sz)
            && (chunk_y0..=chunk_y1).contains(&sy)
        {
            let lx = sx - chunk_x0;
            let ly = sy - chunk_y0;
            let lz = sz - chunk_z0;
            chunk.set(lx, ly, lz, BlockId::WaterStill);
            chunk.set_data(lx, ly, lz, fluid_data_make(0, false, true));
            if sy + 1 <= chunk_y1 {
                chunk.set(lx, ly + 1, lz, BlockId::Air);
                chunk.set_data(lx, ly + 1, lz, 0);
            }
        }

        for (x, y, z) in spring.path {
            if !(chunk_x0..(chunk_x0 + CHUNK_SIZE)).contains(&x)
                || !(chunk_z0..(chunk_z0 + CHUNK_SIZE)).contains(&z)
                || !(chunk_y0..=chunk_y1).contains(&y)
            {
                continue;
            }

            let lx = x - chunk_x0;
            let ly = y - chunk_y0;
            let lz = z - chunk_z0;
            chunk.set(lx, ly, lz, BlockId::WaterFlowing);
            chunk.set_data(lx, ly, lz, fluid_data_make(1, false, false));
            if y + 1 <= chunk_y1 {
                chunk.set(lx, ly + 1, lz, BlockId::Air);
                chunk.set_data(lx, ly + 1, lz, 0);
            }
        }
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

struct StreamingWorldColumnGenerator {
    preset: WorldgenPreset,
}

impl StreamingWorldColumnGenerator {
    fn new(preset: WorldgenPreset) -> Self {
        Self { preset }
    }
}

impl WorldGenerator for StreamingWorldColumnGenerator {
    fn generate_column(
        &self,
        cx: i32,
        cz: i32,
        out: &mut ColumnBuffer,
        ctx: &mut GenContext,
    ) {
        let cfg = WorldConfig {
            ..ctx.world_cfg
        };
        let min_chunk_y = cfg.min_y.div_euclid(CHUNK_SIZE);
        let max_chunk_y = cfg.max_y.div_euclid(CHUNK_SIZE);
        let section_count = (max_chunk_y - min_chunk_y + 1).max(1);

        // 1) Base terrain fill
        ctx.report_progress("Base terrain fill", 0, 5);
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

        // 2) Caves / carvers
        ctx.report_progress("Caves", 1, 5);
        Carvers::carve_column(ctx.seed, cx, cz, out, cfg.sea_level);

        // 3-5) Lakes + shoreline + post-fixes
        for (idx, chunk_y) in (min_chunk_y..=max_chunk_y).enumerate() {
            let mut section = out.extract_chunk(chunk_y);
            let coord = ChunkCoord {
                x: cx,
                y: chunk_y,
                z: cz,
            };

            // 3) Water/lava lakes and 4) shoreline
            apply_streaming_lakes_and_shores(&mut section, coord, &cfg, self.preset);

            // 5) Post-fixes
            apply_post_fixes_to_chunk(&mut section);
            out.write_chunk_section(chunk_y, &section);

            if idx + 1 == section_count as usize {
                ctx.report_progress("Post-fixes", 5, 5);
            }
        }
    }
}

fn generate_streaming_chunk(world_cfg: WorldConfig, preset: WorldgenPreset, coord: ChunkCoord) -> Chunk {
    let mut ctx = GenContext::new(world_cfg.seed, world_cfg);
    let generator = StreamingWorldColumnGenerator::new(preset);
    let mut column = ColumnBuffer::new();
    generator.generate_column(coord.x, coord.z, &mut column, &mut ctx);
    column.extract_chunk(coord.y)
}

#[cfg(test)]
pub(crate) fn generate_streaming_column_for_test(
    world_cfg: WorldConfig,
    preset: WorldgenPreset,
    chunk_x: i32,
    chunk_z: i32,
) -> ColumnBuffer {
    let mut ctx = GenContext::new(world_cfg.seed, world_cfg);
    let generator = StreamingWorldColumnGenerator::new(preset);
    let mut column = ColumnBuffer::new();
    generator.generate_column(chunk_x, chunk_z, &mut column, &mut ctx);
    column
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct LodColumnCoord {
    pub(crate) x: i32,
    pub(crate) z: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChunkGenRequestKind {
    FullChunk,
    LodColumnOnly,
}

fn lod_color_for_height(y: i32) -> [f32; 3] {
    let t = (y as f32 / WORLD_MAX_Y.max(1) as f32).clamp(0.0, 1.0);
    [0.24 + 0.30 * t, 0.28 + 0.45 * t, 0.18 + 0.18 * t]
}

fn build_lod_mesh_for_column(world_cfg: &WorldConfig, preset: WorldgenPreset, x: i32, z: i32) -> CpuMeshData {
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
    pub(crate) lod_column: Option<LodColumnCoord>,
    pub(crate) lod_mesh: Option<CpuMeshData>,
    pub(crate) epoch: u64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct GeneratedFluidBootstrapCell {
    pub(crate) pos: IVec3,
    pub(crate) kind: FluidKind,
}

fn generated_fluid_cell_needs_bootstrap_local(
    chunk: &Chunk,
    local_x: i32,
    local_y: i32,
    local_z: i32,
    kind: FluidKind,
    is_source: bool,
) -> bool {
    if local_y <= 0 {
        return true;
    }
    if fluid_kind(chunk.get(local_x, local_y - 1, local_z)) != Some(kind) {
        return true;
    }

    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        let nx = local_x + dx;
        let nz = local_z + dz;
        if !(0..CHUNK_SIZE).contains(&nx) || !(0..CHUNK_SIZE).contains(&nz) {
            return true;
        }
        if fluid_kind(chunk.get(nx, local_y, nz)) != Some(kind) {
            return true;
        }
    }

    if is_source {
        return false;
    }

    if local_y >= CHUNK_SIZE - 1 {
        return true;
    }
    fluid_kind(chunk.get(local_x, local_y + 1, local_z)) != Some(kind)
}

fn collect_generated_fluid_bootstrap_cells(
    chunk: &Chunk,
    coord: ChunkCoord,
) -> Vec<GeneratedFluidBootstrapCell> {
    let chunk_x0 = coord.x * CHUNK_SIZE;
    let chunk_y0 = coord.y * CHUNK_SIZE;
    let chunk_z0 = coord.z * CHUNK_SIZE;
    let mut out = Vec::with_capacity(32);

    for local_y in 0..CHUNK_SIZE {
        for local_z in 0..CHUNK_SIZE {
            for local_x in 0..CHUNK_SIZE {
                let block = chunk.get(local_x, local_y, local_z);
                let Some(kind) = fluid_kind(block) else {
                    continue;
                };
                let data = chunk.get_data(local_x, local_y, local_z);
                let is_source = fluid_data_is_source_origin(data);
                if !generated_fluid_cell_needs_bootstrap_local(
                    chunk, local_x, local_y, local_z, kind, is_source,
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

    out
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
        let (shutdown_tx, shutdown_rx) = crossbeam_channel::unbounded();
        let mut workers = Vec::with_capacity(worker_count);

        for worker_index in 0..worker_count {
            let job_rx = job_rx.clone();
            let result_tx = result_tx.clone();
            let shutdown_rx = shutdown_rx.clone();
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
                                    let mut chunk =
                                        generate_streaming_chunk(job.world_cfg, job.preset, job.coord);
                                    if !ENABLE_WORLD_FLUID_FEATURES {
                                        apply_post_fixes_to_chunk(&mut chunk);
                                    }
                                    let fluid_bootstrap =
                                        collect_generated_fluid_bootstrap_cells(&chunk, job.coord);
                                    let (lod_column, lod_mesh) = if ENABLE_FAR_LOD && job.coord.y == 0 {
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
                                        lod_column,
                                        lod_mesh,
                                        epoch: job.epoch,
                                    }).is_err() {
                                        break;
                                    }
                                    perf_trace::record_duration(
                                        "chunkgen_worker.full_chunk",
                                        started.elapsed(),
                                        &format!(
                                            "coord=({}, {}, {}) lod_generated={}",
                                            job.coord.x,
                                            job.coord.y,
                                            job.coord.z,
                                            ENABLE_FAR_LOD && job.coord.y == 0
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
                                        lod_column: Some(lod_column),
                                        lod_mesh,
                                        epoch: job.epoch,
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
