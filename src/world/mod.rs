use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use glam::{IVec2, IVec3};

use crate::blocks::{BlockId, BlockRegistry, block_has_block_entity};
use crate::world::ticking_areas::{
    TICKING_AREA_MAX_RADIUS_CHUNKS, TickingArea, TickingFlags,
};
use crate::worldgen::{ChunkGenerator, DEFAULT_WORLDGEN_PRESET, TerrainGenerator, WorldgenPreset};

pub(crate) mod block_access;
pub(crate) mod block_entities_storage;
pub(crate) mod block_entities_system;
pub(crate) mod block_entity_types;
pub(crate) mod chunk;
pub(crate) mod config;
pub(crate) mod fluids;
pub(crate) mod fluids_system;
pub(crate) mod manifest;
pub(crate) mod random_ticks_system;
pub(crate) mod raycast;
pub(crate) mod ticking_areas;
pub(crate) mod time;

pub(crate) use block_access::BlockAccess;
pub(crate) use chunk::{Chunk, ChunkCoord, LightLevel};
pub(crate) use config::WorldConfig;
pub(crate) use fluids::FluidKind;
use fluids::fluid_kind;
use manifest::{IdMapper, WorldManifest};

pub(crate) const CHUNK_SIZE: i32 = 16;
pub(crate) const WORLD_XZ: i32 = 256;
pub(crate) const WORLD_MAX_Y: i32 = 255;
pub(crate) const WORLD_CHUNKS_XZ: i32 = WORLD_XZ / CHUNK_SIZE;
pub(crate) const WORLD_HEIGHT: i32 = WORLD_MAX_Y + 1;
pub(crate) const SAVE_PATH: &str = "save/world.bin";
const SAVE_MAGIC: [u8; 4] = *b"MCW0";
const SAVE_VERSION: u16 = 15;
const SAVE_VERSION_LEGACY_V14: u16 = 14;
const SAVE_VERSION_LEGACY_V13: u16 = 13;

type LocalCoord = IVec3;

pub(crate) struct World {
    pub(crate) chunks: HashMap<ChunkCoord, Chunk>,
    light_engine: LightEngine,
    cfg: WorldConfig,
    preset: WorldgenPreset,
    generator: Box<dyn ChunkGenerator + Send + Sync>,
    pending_block_entity_block_changes: Vec<BlockEntityBlockChange>,
    ticking_areas: Vec<TickingArea>,
    next_ticking_area_id: u32,
    manifest: WorldManifest,
    id_mapper: IdMapper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetBlockError {
    OutOfBounds,
    ChunkNotLoaded,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BlockEntityBlockChange {
    pub(crate) pos: IVec3,
    pub(crate) old_block: BlockId,
    pub(crate) new_block: BlockId,
}

#[derive(Clone)]
pub(crate) struct SnapshotLightingChunk {
    pub(crate) blocks: Arc<Vec<BlockId>>,
    pub(crate) block_data: Arc<Vec<u8>>,
    pub(crate) light: Vec<u8>,
    // Per local (x,z) column: highest local y that blocks skylight, or -1 if none.
    pub(crate) sky_blocker_top: [i16; 256],
}

#[derive(Clone, Default)]
pub(crate) struct SnapshotWorldLighting {
    pub(crate) chunks: HashMap<ChunkCoord, SnapshotLightingChunk>,
}

impl SnapshotWorldLighting {
    pub(crate) fn loaded_chunk_count(&self) -> usize {
        self.chunks.len()
    }

    pub(crate) fn xz_column_count(&self) -> usize {
        let mut xz_chunk_columns: HashSet<(i32, i32)> = HashSet::with_capacity(self.chunks.len());
        for coord in self.chunks.keys() {
            xz_chunk_columns.insert((coord.x, coord.z));
        }
        xz_chunk_columns.len()
    }

    pub(crate) fn into_light_buffers(self) -> HashMap<ChunkCoord, Vec<u8>> {
        let mut out = HashMap::with_capacity(self.chunks.len());
        for (coord, chunk) in self.chunks {
            out.insert(coord, chunk.light);
        }
        out
    }

    fn get_block(&self, world_pos: IVec3) -> BlockId {
        if !World::in_bounds(world_pos) {
            return BlockId::Air;
        }

        let chunk_coord = World::world_to_chunk_coord(world_pos);
        let local = World::world_to_local(world_pos);
        self.chunks.get(&chunk_coord).map_or(BlockId::Air, |chunk| {
            chunk.blocks[Chunk::index(local.x, local.y, local.z)]
        })
    }

    fn get_light_packed(&self, world_pos: IVec3) -> u8 {
        if world_pos.y > WORLD_MAX_Y {
            return LightLevel::new(15, 0).pack();
        }
        if world_pos.y < 0 {
            return 0;
        }

        let chunk_coord = World::world_to_chunk_coord(world_pos);
        let local = World::world_to_local(world_pos);
        self.chunks.get(&chunk_coord).map_or_else(
            || {
                if world_pos.y >= 0 {
                    LightLevel::new(15, 0).pack()
                } else {
                    0
                }
            },
            |chunk| chunk.light[Chunk::index(local.x, local.y, local.z)],
        )
    }

    fn get_light(&self, world_pos: IVec3) -> LightLevel {
        LightLevel::from_packed(self.get_light_packed(world_pos))
    }

    fn set_light_packed(&mut self, world_pos: IVec3, packed: u8) {
        if !World::in_bounds(world_pos) {
            return;
        }
        let chunk_coord = World::world_to_chunk_coord(world_pos);
        let local = World::world_to_local(world_pos);
        if let Some(chunk) = self.chunks.get_mut(&chunk_coord) {
            let idx = Chunk::index(local.x, local.y, local.z);
            chunk.light[idx] = packed;
        }
    }
}

fn snapshot_chunk_column_index(lx: i32, lz: i32) -> usize {
    (lx + lz * CHUNK_SIZE) as usize
}

fn build_snapshot_sky_blocker_top(blocks: &[BlockId]) -> [i16; 256] {
    let mut out = [-1i16; 256];
    for lz in 0..CHUNK_SIZE {
        for lx in 0..CHUNK_SIZE {
            let col_idx = snapshot_chunk_column_index(lx, lz);
            for ly in (0..CHUNK_SIZE).rev() {
                if blocks[Chunk::index(lx, ly, lz)].blocks_skylight() {
                    out[col_idx] = ly as i16;
                    break;
                }
            }
        }
    }
    out
}

#[derive(Default)]
struct LightEngine {
    sky_queue: VecDeque<IVec3>,
    block_queue: VecDeque<IVec3>,
    affected_chunks: HashSet<ChunkCoord>,
    region_scratch: HashSet<ChunkCoord>,
}

impl LightEngine {
    fn on_chunk_added(&mut self, world: &mut World, coord: ChunkCoord) -> Vec<ChunkCoord> {
        if !world.chunks.contains_key(&coord) {
            return Vec::new();
        }
        self.region_scratch.clear();
        self.insert_chunk_and_neighbors(world, coord);
        self.recompute_region_and_propagate(world)
    }

    fn on_chunk_removed(
        &mut self,
        world: &mut World,
        removed_coord: ChunkCoord,
    ) -> Vec<ChunkCoord> {
        self.region_scratch.clear();
        for neighbor in [
            ChunkCoord {
                x: removed_coord.x + 1,
                y: removed_coord.y,
                z: removed_coord.z,
            },
            ChunkCoord {
                x: removed_coord.x - 1,
                y: removed_coord.y,
                z: removed_coord.z,
            },
            ChunkCoord {
                x: removed_coord.x,
                y: removed_coord.y + 1,
                z: removed_coord.z,
            },
            ChunkCoord {
                x: removed_coord.x,
                y: removed_coord.y - 1,
                z: removed_coord.z,
            },
            ChunkCoord {
                x: removed_coord.x,
                y: removed_coord.y,
                z: removed_coord.z + 1,
            },
            ChunkCoord {
                x: removed_coord.x,
                y: removed_coord.y,
                z: removed_coord.z - 1,
            },
        ] {
            if world.chunks.contains_key(&neighbor) {
                self.region_scratch.insert(neighbor);
            }
        }
        self.recompute_region_and_propagate(world)
    }

    fn on_block_changed(
        &mut self,
        world: &mut World,
        pos: IVec3,
        _old: BlockId,
        _new: BlockId,
    ) -> Vec<ChunkCoord> {
        let base = World::world_to_chunk_coord(pos);
        self.region_scratch.clear();
        self.insert_chunk_and_neighbors(world, base);

        let local = World::world_to_local(pos);
        if local.x == 0 {
            self.insert_chunk_and_neighbors(
                world,
                ChunkCoord {
                    x: base.x - 1,
                    y: base.y,
                    z: base.z,
                },
            );
        }
        if local.x == CHUNK_SIZE - 1 {
            self.insert_chunk_and_neighbors(
                world,
                ChunkCoord {
                    x: base.x + 1,
                    y: base.y,
                    z: base.z,
                },
            );
        }
        if local.y == 0 {
            self.insert_chunk_and_neighbors(
                world,
                ChunkCoord {
                    x: base.x,
                    y: base.y - 1,
                    z: base.z,
                },
            );
        }
        if local.y == CHUNK_SIZE - 1 {
            self.insert_chunk_and_neighbors(
                world,
                ChunkCoord {
                    x: base.x,
                    y: base.y + 1,
                    z: base.z,
                },
            );
        }
        if local.z == 0 {
            self.insert_chunk_and_neighbors(
                world,
                ChunkCoord {
                    x: base.x,
                    y: base.y,
                    z: base.z - 1,
                },
            );
        }
        if local.z == CHUNK_SIZE - 1 {
            self.insert_chunk_and_neighbors(
                world,
                ChunkCoord {
                    x: base.x,
                    y: base.y,
                    z: base.z + 1,
                },
            );
        }

        self.recompute_region_and_propagate(world)
    }

    fn insert_chunk_and_neighbors(&mut self, world: &World, coord: ChunkCoord) {
        if !world.chunks.contains_key(&coord) {
            return;
        }
        self.region_scratch.insert(coord);
        for neighbor in [
            ChunkCoord {
                x: coord.x + 1,
                y: coord.y,
                z: coord.z,
            },
            ChunkCoord {
                x: coord.x - 1,
                y: coord.y,
                z: coord.z,
            },
            ChunkCoord {
                x: coord.x,
                y: coord.y + 1,
                z: coord.z,
            },
            ChunkCoord {
                x: coord.x,
                y: coord.y - 1,
                z: coord.z,
            },
            ChunkCoord {
                x: coord.x,
                y: coord.y,
                z: coord.z + 1,
            },
            ChunkCoord {
                x: coord.x,
                y: coord.y,
                z: coord.z - 1,
            },
        ] {
            if world.chunks.contains_key(&neighbor) {
                self.region_scratch.insert(neighbor);
            }
        }
    }

    fn recompute_region_and_propagate(&mut self, world: &mut World) -> Vec<ChunkCoord> {
        if self.region_scratch.is_empty() {
            return Vec::new();
        }
        self.sky_queue.clear();
        self.block_queue.clear();
        self.affected_chunks.clear();

        let region_coords: Vec<ChunkCoord> = self.region_scratch.iter().copied().collect();
        for coord in &region_coords {
            self.clear_chunk_light(world, *coord);
        }
        for coord in &region_coords {
            self.seed_chunk_skylight(world, *coord);
            self.seed_chunk_blocklight(world, *coord);
        }
        self.seed_from_boundary_neighbors(world, &region_coords);

        self.propagate_sky(world);
        self.propagate_block(world);

        let affected: Vec<ChunkCoord> = self.affected_chunks.iter().copied().collect();
        self.region_scratch.clear();
        self.affected_chunks.clear();
        affected
    }

    fn clear_chunk_light(&mut self, world: &mut World, coord: ChunkCoord) {
        let Some(chunk) = world.chunks.get_mut(&coord) else {
            return;
        };
        let mut changed = false;
        for v in Arc::make_mut(&mut chunk.light).iter_mut() {
            if *v != 0 {
                *v = 0;
                changed = true;
            }
        }
        if changed {
            chunk.revision = chunk.revision.wrapping_add(1);
            self.affected_chunks.insert(coord);
        }
    }

    fn seed_chunk_skylight(&mut self, world: &mut World, coord: ChunkCoord) {
        if !world.chunks.contains_key(&coord) {
            return;
        }
        let base_x = coord.x * CHUNK_SIZE;
        let base_y = coord.y * CHUNK_SIZE;
        let base_z = coord.z * CHUNK_SIZE;
        let chunk_top_y = base_y + CHUNK_SIZE - 1;
        let scan_hi = WORLD_MAX_Y;

        for lz in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                let wx = base_x + lx;
                let wz = base_z + lz;
                let mut sky_reaches_chunk = true;
                let mut y = scan_hi;
                while y > chunk_top_y {
                    let pos = IVec3::new(wx, y, wz);
                    let scan_coord = World::world_to_chunk_coord(pos);
                    if !world.chunks.contains_key(&scan_coord) {
                        sky_reaches_chunk = true;
                        break;
                    }
                    if world.get_block(pos).blocks_skylight() {
                        sky_reaches_chunk = false;
                        break;
                    }
                    y -= 1;
                }
                if !sky_reaches_chunk {
                    continue;
                }

                for ly in (0..CHUNK_SIZE).rev() {
                    let pos = IVec3::new(wx, base_y + ly, wz);
                    if world.get_block(pos).blocks_skylight() {
                        break;
                    }
                    if self.try_set_light_component(world, pos, true, 15) {
                        self.sky_queue.push_back(pos);
                    }
                }
            }
        }
    }

    fn seed_chunk_blocklight(&mut self, world: &mut World, coord: ChunkCoord) {
        let Some(chunk) = world.chunks.get(&coord) else {
            return;
        };
        let origin = IVec3::new(
            coord.x * CHUNK_SIZE,
            coord.y * CHUNK_SIZE,
            coord.z * CHUNK_SIZE,
        );
        let mut emitted_positions = Vec::new();
        for y in 0..CHUNK_SIZE {
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    let emitted = chunk.get(x, y, z).emitted_light();
                    if emitted > 0 {
                        emitted_positions.push((origin + IVec3::new(x, y, z), emitted));
                    }
                }
            }
        }
        for (pos, emitted) in emitted_positions {
            if self.try_set_light_component(world, pos, false, emitted) {
                self.block_queue.push_back(pos);
            }
        }
    }

    fn seed_from_boundary_neighbors(&mut self, world: &World, region_coords: &[ChunkCoord]) {
        for &coord in region_coords {
            let base = IVec3::new(
                coord.x * CHUNK_SIZE,
                coord.y * CHUNK_SIZE,
                coord.z * CHUNK_SIZE,
            );

            for ly in 0..CHUNK_SIZE {
                for lz in 0..CHUNK_SIZE {
                    let pos_neg_x = base + IVec3::new(0, ly, lz);
                    let src_neg_x = pos_neg_x + IVec3::new(-1, 0, 0);
                    self.enqueue_existing_light_if_loaded(world, src_neg_x);

                    let pos_pos_x = base + IVec3::new(CHUNK_SIZE - 1, ly, lz);
                    let src_pos_x = pos_pos_x + IVec3::new(1, 0, 0);
                    self.enqueue_existing_light_if_loaded(world, src_pos_x);
                }
            }

            for ly in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    let pos_neg_z = base + IVec3::new(lx, ly, 0);
                    let src_neg_z = pos_neg_z + IVec3::new(0, 0, -1);
                    self.enqueue_existing_light_if_loaded(world, src_neg_z);

                    let pos_pos_z = base + IVec3::new(lx, ly, CHUNK_SIZE - 1);
                    let src_pos_z = pos_pos_z + IVec3::new(0, 0, 1);
                    self.enqueue_existing_light_if_loaded(world, src_pos_z);
                }
            }

            for lz in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    let pos_neg_y = base + IVec3::new(lx, 0, lz);
                    let src_neg_y = pos_neg_y + IVec3::new(0, -1, 0);
                    self.enqueue_existing_light_if_loaded(world, src_neg_y);

                    let pos_pos_y = base + IVec3::new(lx, CHUNK_SIZE - 1, lz);
                    let src_pos_y = pos_pos_y + IVec3::new(0, 1, 0);
                    self.enqueue_existing_light_if_loaded(world, src_pos_y);
                }
            }
        }
    }

    fn enqueue_existing_light_if_loaded(&mut self, world: &World, pos: IVec3) {
        if pos.y < 0 || pos.y > WORLD_MAX_Y {
            return;
        }
        let coord = World::world_to_chunk_coord(pos);
        if !world.chunks.contains_key(&coord) {
            return;
        }
        let light = world.get_light(pos);
        if light.sky > 0 {
            self.sky_queue.push_back(pos);
        }
        if light.block > 0 {
            self.block_queue.push_back(pos);
        }
    }

    fn propagate_sky(&mut self, world: &mut World) {
        let neighbor_dirs = [
            IVec3::new(1, 0, 0),
            IVec3::new(-1, 0, 0),
            IVec3::new(0, 1, 0),
            IVec3::new(0, -1, 0),
            IVec3::new(0, 0, 1),
            IVec3::new(0, 0, -1),
        ];
        while let Some(pos) = self.sky_queue.pop_front() {
            let level = world.get_light(pos).sky;
            if level == 0 {
                continue;
            }
            for dir in &neighbor_dirs {
                let npos = pos + *dir;
                if npos.y < 0 || npos.y > WORLD_MAX_Y {
                    continue;
                }
                if world.get_block(npos).blocks_skylight() {
                    continue;
                }
                let next = if *dir == IVec3::new(0, -1, 0) {
                    level
                } else {
                    level.saturating_sub(1)
                };
                if next == 0 {
                    continue;
                }
                if self.try_set_light_component(world, npos, true, next) {
                    self.sky_queue.push_back(npos);
                }
            }
        }
    }

    fn propagate_block(&mut self, world: &mut World) {
        let neighbor_dirs = [
            IVec3::new(1, 0, 0),
            IVec3::new(-1, 0, 0),
            IVec3::new(0, 1, 0),
            IVec3::new(0, -1, 0),
            IVec3::new(0, 0, 1),
            IVec3::new(0, 0, -1),
        ];
        while let Some(pos) = self.block_queue.pop_front() {
            let level = world.get_light(pos).block;
            if level <= 1 {
                continue;
            }
            for dir in &neighbor_dirs {
                let npos = pos + *dir;
                if npos.y < 0 || npos.y > WORLD_MAX_Y {
                    continue;
                }
                if world.get_block(npos).blocks_skylight() {
                    continue;
                }
                let next = level.saturating_sub(1);
                if next == 0 {
                    continue;
                }
                if self.try_set_light_component(world, npos, false, next) {
                    self.block_queue.push_back(npos);
                }
            }
        }
    }

    fn try_set_light_component(
        &mut self,
        world: &mut World,
        pos: IVec3,
        sky: bool,
        new_level: u8,
    ) -> bool {
        if !World::in_bounds(pos) {
            return false;
        }
        let coord = World::world_to_chunk_coord(pos);
        let local = World::world_to_local(pos);
        let Some(chunk) = world.chunks.get_mut(&coord) else {
            return false;
        };
        let idx = Chunk::index(local.x, local.y, local.z);
        let light = Arc::make_mut(&mut chunk.light);
        let current = LightLevel::from_packed(light[idx]);
        let (next_sky, next_block, changed) = if sky {
            if current.sky >= new_level {
                (current.sky, current.block, false)
            } else {
                (new_level, current.block, true)
            }
        } else if current.block >= new_level {
            (current.sky, current.block, false)
        } else {
            (current.sky, new_level, true)
        };
        if !changed {
            return false;
        }
        light[idx] = LightLevel::new(next_sky, next_block).pack();
        chunk.revision = chunk.revision.wrapping_add(1);
        self.affected_chunks.insert(coord);
        true
    }
}

pub(crate) fn rebuild_lighting_for_snapshot(snapshot: &mut SnapshotWorldLighting) {
    if snapshot.chunks.is_empty() {
        return;
    }

    let rebuild_started = Instant::now();
    let loaded_chunk_count = snapshot.chunks.len();

    let clear_light_started = Instant::now();
    for chunk in snapshot.chunks.values_mut() {
        chunk.light.fill(0);
    }
    let clear_light_ms = clear_light_started.elapsed().as_secs_f64() * 1000.0;

    let mut sky_queue: VecDeque<IVec3> = VecDeque::new();
    let mut block_queue: VecDeque<IVec3> = VecDeque::new();
    let mut skylight_seed_sky_queue_peak = 0usize;
    let mut sky_propagation_queue_peak = 0usize;
    let mut block_seed_queue_peak = 0usize;
    let mut block_propagation_queue_peak = 0usize;

    let skylight_seed_started = Instant::now();
    struct SkylightSeedColumn {
        top_loaded_chunk_y: i32,
        top_blocker_world_y: [i32; 256],
    }

    let mut skylight_seed_columns: HashMap<(i32, i32), SkylightSeedColumn> =
        HashMap::with_capacity(snapshot.chunks.len());
    for (&coord, chunk) in &snapshot.chunks {
        let entry = skylight_seed_columns
            .entry((coord.x, coord.z))
            .or_insert_with(|| SkylightSeedColumn {
                top_loaded_chunk_y: coord.y,
                top_blocker_world_y: [i32::MIN; 256],
            });
        if coord.y > entry.top_loaded_chunk_y {
            entry.top_loaded_chunk_y = coord.y;
        }

        let chunk_base_y = coord.y * CHUNK_SIZE;
        for (idx, &local_top) in chunk.sky_blocker_top.iter().enumerate() {
            if local_top < 0 {
                continue;
            }
            let blocker_world_y = chunk_base_y + local_top as i32;
            if blocker_world_y > entry.top_blocker_world_y[idx] {
                entry.top_blocker_world_y[idx] = blocker_world_y;
            }
        }
    }
    let xz_column_count = skylight_seed_columns.len();

    for ((chunk_x, chunk_z), seed_column) in skylight_seed_columns {
        let base_x = chunk_x * CHUNK_SIZE;
        let base_z = chunk_z * CHUNK_SIZE;
        for lz in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                let wx = base_x + lx;
                let wz = base_z + lz;
                let col_idx = snapshot_chunk_column_index(lx, lz);
                let seed_y = if seed_column.top_blocker_world_y[col_idx] == i32::MIN {
                    seed_column.top_loaded_chunk_y * CHUNK_SIZE + (CHUNK_SIZE - 1)
                } else {
                    seed_column.top_blocker_world_y[col_idx] + 1
                };
                if !(0..=WORLD_MAX_Y).contains(&seed_y) {
                    continue;
                }
                let pos = IVec3::new(wx, seed_y, wz);
                let seed_coord = World::world_to_chunk_coord(pos);
                if !snapshot.chunks.contains_key(&seed_coord) {
                    continue;
                }
                if snapshot.get_block(pos).blocks_skylight() {
                    continue;
                }
                let current = snapshot.get_light(pos);
                if current.sky < 15 {
                    snapshot.set_light_packed(pos, LightLevel::new(15, current.block).pack());
                    sky_queue.push_back(pos);
                    skylight_seed_sky_queue_peak =
                        skylight_seed_sky_queue_peak.max(sky_queue.len());
                }
            }
        }
    }
    let skylight_seed_ms = skylight_seed_started.elapsed().as_secs_f64() * 1000.0;

    let block_seed_started = Instant::now();
    let loaded_coords: Vec<ChunkCoord> = snapshot.chunks.keys().copied().collect();
    for coord in loaded_coords {
        let origin = IVec3::new(
            coord.x * CHUNK_SIZE,
            coord.y * CHUNK_SIZE,
            coord.z * CHUNK_SIZE,
        );
        for y in 0..CHUNK_SIZE {
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    let pos = origin + IVec3::new(x, y, z);
                    let emitted = snapshot.get_block(pos).emitted_light();
                    if emitted == 0 {
                        continue;
                    }
                    let current = snapshot.get_light(pos);
                    if current.block < emitted {
                        snapshot
                            .set_light_packed(pos, LightLevel::new(current.sky, emitted).pack());
                        block_queue.push_back(pos);
                        block_seed_queue_peak = block_seed_queue_peak.max(block_queue.len());
                    }
                }
            }
        }
    }
    let block_seed_ms = block_seed_started.elapsed().as_secs_f64() * 1000.0;

    let neighbor_dirs = [
        IVec3::new(1, 0, 0),
        IVec3::new(-1, 0, 0),
        IVec3::new(0, 1, 0),
        IVec3::new(0, -1, 0),
        IVec3::new(0, 0, 1),
        IVec3::new(0, 0, -1),
    ];

    let sky_propagation_started = Instant::now();
    sky_propagation_queue_peak = sky_propagation_queue_peak.max(sky_queue.len());
    while let Some(pos) = sky_queue.pop_front() {
        let level = snapshot.get_light(pos).sky;
        if level == 0 {
            continue;
        }
        for dir in &neighbor_dirs {
            let dir = *dir;
            let npos = pos + dir;
            if npos.y < 0 || npos.y > WORLD_MAX_Y {
                continue;
            }
            if snapshot.get_block(npos).blocks_skylight() {
                continue;
            }
            let next = if dir == IVec3::new(0, -1, 0) {
                level
            } else {
                level.saturating_sub(1)
            };
            if next == 0 {
                continue;
            }
            let ncoord = World::world_to_chunk_coord(npos);
            if !snapshot.chunks.contains_key(&ncoord) {
                continue;
            }
            let current = snapshot.get_light(npos);
            if current.sky >= next {
                continue;
            }
            snapshot.set_light_packed(npos, LightLevel::new(next, current.block).pack());
            sky_queue.push_back(npos);
            sky_propagation_queue_peak = sky_propagation_queue_peak.max(sky_queue.len());
        }
    }
    let sky_propagation_ms = sky_propagation_started.elapsed().as_secs_f64() * 1000.0;

    let block_propagation_started = Instant::now();
    block_propagation_queue_peak = block_propagation_queue_peak.max(block_queue.len());
    while let Some(pos) = block_queue.pop_front() {
        let level = snapshot.get_light(pos).block;
        if level <= 1 {
            continue;
        }
        for dir in &neighbor_dirs {
            let dir = *dir;
            let npos = pos + dir;
            if npos.y < 0 || npos.y > WORLD_MAX_Y {
                continue;
            }
            if snapshot.get_block(npos).blocks_skylight() {
                continue;
            }
            let next = level.saturating_sub(1);
            if next == 0 {
                continue;
            }
            let ncoord = World::world_to_chunk_coord(npos);
            if !snapshot.chunks.contains_key(&ncoord) {
                continue;
            }
            let current = snapshot.get_light(npos);
            if current.block >= next {
                continue;
            }
            snapshot.set_light_packed(npos, LightLevel::new(current.sky, next).pack());
            block_queue.push_back(npos);
            block_propagation_queue_peak = block_propagation_queue_peak.max(block_queue.len());
        }
    }
    let block_propagation_ms = block_propagation_started.elapsed().as_secs_f64() * 1000.0;
    let total_ms = rebuild_started.elapsed().as_secs_f64() * 1000.0;

    eprintln!(
        "[perf][lighting][world] loaded_chunks={} xz_columns={} total_ms={:.2}",
        loaded_chunk_count, xz_column_count, total_ms
    );
    eprintln!(
        "[perf][lighting][world] phase=clear_light ms={:.2} sky_q_peak=0 block_q_peak=0",
        clear_light_ms
    );
    eprintln!(
        "[perf][lighting][world] phase=skylight_seed ms={:.2} sky_q_peak={} block_q_peak=0",
        skylight_seed_ms, skylight_seed_sky_queue_peak
    );
    eprintln!(
        "[perf][lighting][world] phase=block_seed ms={:.2} sky_q_peak=0 block_q_peak={}",
        block_seed_ms, block_seed_queue_peak
    );
    eprintln!(
        "[perf][lighting][world] phase=sky_propagation ms={:.2} sky_q_peak={} block_q_peak=0",
        sky_propagation_ms, sky_propagation_queue_peak
    );
    eprintln!(
        "[perf][lighting][world] phase=block_propagation ms={:.2} sky_q_peak=0 block_q_peak={}",
        block_propagation_ms, block_propagation_queue_peak
    );
}

impl World {
    fn new_with_world_config_and_preset(cfg: WorldConfig, preset: WorldgenPreset) -> Self {
        debug_assert_eq!(cfg.chunk_size, CHUNK_SIZE);
        let generator = Box::new(TerrainGenerator::new(cfg, preset));
        let block_registry = BlockRegistry::new();
        let manifest = WorldManifest::build_from_registry(&block_registry);
        let id_mapper = IdMapper::from_manifest_and_registry(&manifest, &block_registry);
        Self {
            chunks: HashMap::new(),
            light_engine: LightEngine::default(),
            cfg,
            preset,
            generator,
            pending_block_entity_block_changes: Vec::new(),
            ticking_areas: Vec::new(),
            next_ticking_area_id: 1,
            manifest,
            id_mapper,
        }
    }

    pub(crate) fn new() -> Self {
        Self::new_with_world_config_and_preset(WorldConfig::default(), DEFAULT_WORLDGEN_PRESET)
    }

    pub(crate) fn new_with_seed_and_preset(seed: u64, preset: WorldgenPreset) -> Self {
        let mut cfg = WorldConfig::default();
        cfg.seed = seed;
        Self::new_with_world_config_and_preset(cfg, preset)
    }

    pub(crate) fn in_bounds(world_pos: IVec3) -> bool {
        (0..=WORLD_MAX_Y).contains(&world_pos.y)
    }

    pub(crate) fn configure_generator(&mut self, seed: u64, preset: WorldgenPreset) {
        self.cfg.seed = seed;
        self.preset = preset;
        self.generator = Box::new(TerrainGenerator::new(self.cfg, preset));
    }

    pub(crate) fn configure_world_config(&mut self, cfg: WorldConfig, preset: WorldgenPreset) {
        self.cfg = cfg;
        self.preset = preset;
        self.generator = Box::new(TerrainGenerator::new(self.cfg, preset));
    }

    pub(crate) fn config(&self) -> WorldConfig {
        self.cfg
    }

    pub(crate) fn seed(&self) -> u64 {
        self.cfg.seed
    }

    pub(crate) fn sea_level(&self) -> i32 {
        self.cfg.sea_level()
    }

    pub(crate) fn min_y(&self) -> i32 {
        self.cfg.min_y()
    }

    pub(crate) fn max_y(&self) -> i32 {
        self.cfg.max_y()
    }

    pub(crate) fn chunk_size(&self) -> i32 {
        self.cfg.chunk_size
    }

    pub(crate) fn set_manifest_and_mapper(&mut self, manifest: WorldManifest, mapper: IdMapper) {
        self.manifest = manifest;
        self.id_mapper = mapper;
    }

    pub(crate) fn ticking_areas(&self) -> &[TickingArea] {
        &self.ticking_areas
    }

    pub(crate) fn replace_ticking_areas(&mut self, mut areas: Vec<TickingArea>) {
        for area in &mut areas {
            *area = area.normalized();
        }
        areas.sort_by_key(|a| a.id);
        areas.dedup_by_key(|a| a.id);
        self.next_ticking_area_id = areas
            .iter()
            .map(|a| a.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .max(1);
        self.ticking_areas = areas;
    }

    pub(crate) fn create_ticking_area(
        &mut self,
        center: IVec2,
        radius_chunks: i32,
        flags: TickingFlags,
    ) -> TickingArea {
        let id = self.next_ticking_area_id.max(1);
        self.next_ticking_area_id = id.saturating_add(1).max(1);
        let area = TickingArea {
            id,
            center,
            radius_chunks: radius_chunks.clamp(0, TICKING_AREA_MAX_RADIUS_CHUNKS),
            flags,
        };
        self.ticking_areas.push(area);
        area
    }

    pub(crate) fn remove_ticking_area(&mut self, id: u32) -> bool {
        let before = self.ticking_areas.len();
        self.ticking_areas.retain(|area| area.id != id);
        self.ticking_areas.len() != before
    }

    pub(crate) fn remove_nearest_ticking_area(&mut self, center: IVec2) -> Option<TickingArea> {
        let mut best: Option<(usize, i64)> = None;
        for (idx, area) in self.ticking_areas.iter().enumerate() {
            let dx = i64::from(area.center.x - center.x);
            let dz = i64::from(area.center.y - center.y);
            let dist2 = dx * dx + dz * dz;
            let replace = best.as_ref().is_none_or(|(_, best_dist2)| dist2 < *best_dist2);
            if replace {
                best = Some((idx, dist2));
            }
        }
        best.map(|(idx, _)| self.ticking_areas.remove(idx))
    }

    pub(crate) fn manifest(&self) -> &WorldManifest {
        &self.manifest
    }

    pub(crate) fn manifest_missing_blocks_count(&self) -> usize {
        self.id_mapper.missing_blocks_count()
    }

    pub(crate) fn manifest_missing_block_keys(&self) -> &[String] {
        self.id_mapper.missing_block_keys()
    }

    pub(crate) fn encode_runtime_block_for_storage_u8(&self, block: BlockId) -> io::Result<u8> {
        let manifest_id = self.id_mapper.encode_runtime_block(block);
        u8::try_from(manifest_id).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "manifest id {} for runtime block {:?} exceeds u8 storage range",
                    manifest_id, block
                ),
            )
        })
    }

    pub(crate) fn decode_manifest_block_id(&self, manifest_id: u32) -> BlockId {
        self.id_mapper.decode_manifest_id(manifest_id)
    }

    pub(crate) fn runtime_to_manifest_lut(&self) -> [u32; 256] {
        self.id_mapper.runtime_to_manifest_lut()
    }

    pub(crate) fn min_chunk_y(&self) -> i32 {
        self.cfg.min_y.div_euclid(CHUNK_SIZE)
    }

    pub(crate) fn max_chunk_y(&self) -> i32 {
        self.cfg.max_y.div_euclid(CHUNK_SIZE)
    }

    pub(crate) fn world_to_chunk_coord(world_pos: IVec3) -> ChunkCoord {
        ChunkCoord {
            x: world_pos.x.div_euclid(CHUNK_SIZE),
            y: world_pos.y.div_euclid(CHUNK_SIZE),
            z: world_pos.z.div_euclid(CHUNK_SIZE),
        }
    }

    pub(crate) fn world_to_local(world_pos: IVec3) -> LocalCoord {
        IVec3::new(
            world_pos.x.rem_euclid(CHUNK_SIZE),
            world_pos.y.rem_euclid(CHUNK_SIZE),
            world_pos.z.rem_euclid(CHUNK_SIZE),
        )
    }

    pub(crate) fn get_block(&self, world_pos: IVec3) -> BlockId {
        if !Self::in_bounds(world_pos) {
            return BlockId::Air;
        }

        let chunk_coord = Self::world_to_chunk_coord(world_pos);
        let local = Self::world_to_local(world_pos);
        self.chunks
            .get(&chunk_coord)
            .map_or(BlockId::Air, |chunk| chunk.get(local.x, local.y, local.z))
    }

    pub(crate) fn is_chunk_loaded_at(&self, world_pos: IVec3) -> bool {
        if !Self::in_bounds(world_pos) {
            return false;
        }
        let chunk_coord = Self::world_to_chunk_coord(world_pos);
        self.chunks.contains_key(&chunk_coord)
    }

    pub(crate) fn get_block_data(&self, world_pos: IVec3) -> u8 {
        if !Self::in_bounds(world_pos) {
            return 0;
        }

        let chunk_coord = Self::world_to_chunk_coord(world_pos);
        let local = Self::world_to_local(world_pos);
        self.chunks
            .get(&chunk_coord)
            .map_or(0, |chunk| chunk.get_data(local.x, local.y, local.z))
    }

    pub(crate) fn get_light_packed(&self, world_pos: IVec3) -> u8 {
        if world_pos.y > WORLD_MAX_Y {
            return LightLevel::new(15, 0).pack();
        }
        if world_pos.y < 0 {
            return 0;
        }

        let chunk_coord = Self::world_to_chunk_coord(world_pos);
        let local = Self::world_to_local(world_pos);
        self.chunks.get(&chunk_coord).map_or_else(
            || {
                if world_pos.y >= 0 {
                    LightLevel::new(15, 0).pack()
                } else {
                    0
                }
            },
            |chunk| chunk.get_light_packed(local.x, local.y, local.z),
        )
    }

    pub(crate) fn get_light(&self, world_pos: IVec3) -> LightLevel {
        LightLevel::from_packed(self.get_light_packed(world_pos))
    }

    pub(crate) fn set_light_packed(&mut self, world_pos: IVec3, packed: u8) {
        if !Self::in_bounds(world_pos) {
            return;
        }
        let chunk_coord = Self::world_to_chunk_coord(world_pos);
        let local = Self::world_to_local(world_pos);
        if let Some(chunk) = self.chunks.get_mut(&chunk_coord) {
            chunk.set_light_packed(local.x, local.y, local.z, packed);
        }
    }

    pub(crate) fn get_chunk(&self, coord: ChunkCoord) -> Option<&Chunk> {
        self.chunks.get(&coord)
    }

    pub(crate) fn build_lighting_snapshot(&self) -> SnapshotWorldLighting {
        let mut chunks = HashMap::with_capacity(self.chunks.len());
        for (&coord, chunk) in &self.chunks {
            let sky_blocker_top = build_snapshot_sky_blocker_top(chunk.blocks.as_slice());
            chunks.insert(
                coord,
                SnapshotLightingChunk {
                    blocks: Arc::clone(&chunk.blocks),
                    block_data: Arc::clone(&chunk.block_data),
                    light: vec![0; chunk.light.len()],
                    sky_blocker_top,
                },
            );
        }
        SnapshotWorldLighting { chunks }
    }

    pub(crate) fn lighting_on_chunk_added(&mut self, coord: ChunkCoord) -> Vec<ChunkCoord> {
        let mut engine = std::mem::take(&mut self.light_engine);
        let affected = engine.on_chunk_added(self, coord);
        self.light_engine = engine;
        affected
    }

    pub(crate) fn lighting_on_chunk_removed(&mut self, coord: ChunkCoord) -> Vec<ChunkCoord> {
        let mut engine = std::mem::take(&mut self.light_engine);
        let affected = engine.on_chunk_removed(self, coord);
        self.light_engine = engine;
        affected
    }

    pub(crate) fn lighting_on_block_changed(
        &mut self,
        pos: IVec3,
        old: BlockId,
        new: BlockId,
    ) -> Vec<ChunkCoord> {
        let mut engine = std::mem::take(&mut self.light_engine);
        let affected = engine.on_block_changed(self, pos, old, new);
        self.light_engine = engine;
        affected
    }

    pub(crate) fn rebuild_loaded_lighting(&mut self) {
        if self.chunks.is_empty() {
            return;
        }
        let mut snapshot = self.build_lighting_snapshot();
        rebuild_lighting_for_snapshot(&mut snapshot);
        for (coord, snapshot_chunk) in snapshot.chunks {
            let Some(chunk) = self.chunks.get_mut(&coord) else {
                continue;
            };
            if chunk.light.as_slice() != snapshot_chunk.light.as_slice() {
                chunk.light = Arc::new(snapshot_chunk.light);
                chunk.revision = chunk.revision.wrapping_add(1);
            }
        }
    }

    pub(crate) fn is_solid_block_at(&self, wx: i32, wy: i32, wz: i32) -> bool {
        let pos = IVec3::new(wx, wy, wz);
        if !Self::in_bounds(pos) {
            return true;
        }
        self.get_block(pos).is_solid()
    }

    #[cfg(test)]
    pub(crate) fn ensure_column_generated(&mut self, world_x: i32, world_z: i32) {
        let chunk_x = world_x.div_euclid(CHUNK_SIZE);
        let chunk_z = world_z.div_euclid(CHUNK_SIZE);
        for chunk_y in self.min_chunk_y()..=self.max_chunk_y() {
            let coord = ChunkCoord {
                x: chunk_x,
                y: chunk_y,
                z: chunk_z,
            };
            if self.chunks.contains_key(&coord) {
                continue;
            }
            let mut chunk = Chunk::new_empty();
            self.generator.generate_chunk(coord, &mut chunk);
            if !chunk.is_empty() {
                self.chunks.insert(coord, chunk);
            }
        }
    }

    pub(crate) fn set_block(&mut self, world_pos: IVec3, id: BlockId) -> Result<(), SetBlockError> {
        self.set_block_and_data(world_pos, id, 0)
    }

    pub(crate) fn set_block_and_data(
        &mut self,
        world_pos: IVec3,
        id: BlockId,
        data: u8,
    ) -> Result<(), SetBlockError> {
        if !Self::in_bounds(world_pos) {
            return Err(SetBlockError::OutOfBounds);
        }

        let chunk_coord = Self::world_to_chunk_coord(world_pos);
        let local = Self::world_to_local(world_pos);
        let old_block = self
            .chunks
            .get(&chunk_coord)
            .map_or(BlockId::Air, |chunk| chunk.get(local.x, local.y, local.z));

        if id == BlockId::Air {
            let mut remove_chunk = false;
            if let Some(chunk) = self.chunks.get_mut(&chunk_coord) {
                chunk.set(local.x, local.y, local.z, id);
                chunk.set_data(local.x, local.y, local.z, 0);
                remove_chunk = chunk.is_empty();
            }
            if remove_chunk {
                self.chunks.remove(&chunk_coord);
            }
            if old_block != id && (block_has_block_entity(old_block) || block_has_block_entity(id))
            {
                self.pending_block_entity_block_changes
                    .push(BlockEntityBlockChange {
                        pos: world_pos,
                        old_block,
                        new_block: id,
                    });
            }
            return Ok(());
        }

        let chunk = self
            .chunks
            .entry(chunk_coord)
            .or_insert_with(Chunk::new_empty);
        chunk.set(local.x, local.y, local.z, id);
        chunk.set_data(local.x, local.y, local.z, data);
        if old_block != id && (block_has_block_entity(old_block) || block_has_block_entity(id)) {
            self.pending_block_entity_block_changes
                .push(BlockEntityBlockChange {
                    pos: world_pos,
                    old_block,
                    new_block: id,
                });
        }
        Ok(())
    }

    pub(crate) fn set_block_and_data_loaded_only(
        &mut self,
        world_pos: IVec3,
        id: BlockId,
        data: u8,
    ) -> Result<(), SetBlockError> {
        if !Self::in_bounds(world_pos) {
            return Err(SetBlockError::OutOfBounds);
        }

        let chunk_coord = Self::world_to_chunk_coord(world_pos);
        if id != BlockId::Air && !self.chunks.contains_key(&chunk_coord) {
            return Err(SetBlockError::ChunkNotLoaded);
        }

        let local = Self::world_to_local(world_pos);
        let old_block = self
            .chunks
            .get(&chunk_coord)
            .map_or(BlockId::Air, |chunk| chunk.get(local.x, local.y, local.z));

        if id == BlockId::Air {
            let mut remove_chunk = false;
            if let Some(chunk) = self.chunks.get_mut(&chunk_coord) {
                chunk.set(local.x, local.y, local.z, id);
                chunk.set_data(local.x, local.y, local.z, 0);
                remove_chunk = chunk.is_empty();
            }
            if remove_chunk {
                self.chunks.remove(&chunk_coord);
            }
            if old_block != id && (block_has_block_entity(old_block) || block_has_block_entity(id))
            {
                self.pending_block_entity_block_changes
                    .push(BlockEntityBlockChange {
                        pos: world_pos,
                        old_block,
                        new_block: id,
                    });
            }
            return Ok(());
        }

        let Some(chunk) = self.chunks.get_mut(&chunk_coord) else {
            return Err(SetBlockError::ChunkNotLoaded);
        };
        chunk.set(local.x, local.y, local.z, id);
        chunk.set_data(local.x, local.y, local.z, data);
        if old_block != id && (block_has_block_entity(old_block) || block_has_block_entity(id)) {
            self.pending_block_entity_block_changes
                .push(BlockEntityBlockChange {
                    pos: world_pos,
                    old_block,
                    new_block: id,
                });
        }
        Ok(())
    }

    pub(crate) fn drain_block_entity_block_changes(
        &mut self,
        out: &mut Vec<BlockEntityBlockChange>,
    ) {
        out.append(&mut self.pending_block_entity_block_changes);
    }

    pub(crate) fn terrain_height_at(&self, world_x: i32, world_z: i32) -> i32 {
        let min_y = self.cfg.min_y.min(self.cfg.max_y);
        let max_y = self.cfg.max_y.max(self.cfg.min_y);
        for y in (min_y..=max_y).rev() {
            if self.get_block(IVec3::new(world_x, y, world_z)).is_solid() {
                return y;
            }
        }
        min_y
    }

    pub(crate) fn estimated_surface_height_at(&self, world_x: i32, world_z: i32) -> i32 {
        let min_y = self.cfg.min_y.min(self.cfg.max_y);
        let max_y = self.cfg.max_y.max(self.cfg.min_y);
        self.generator
            .estimate_surface_height(world_x, world_z, max_y)
            .clamp(min_y, max_y)
    }

    pub(crate) fn save_to_file(&self, path: &str) -> io::Result<()> {
        let save_path = Path::new(path);
        if let Some(parent) = save_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let block_count = (WORLD_XZ * WORLD_XZ * WORLD_HEIGHT) as usize;
        let mut bytes = Vec::with_capacity(4 + 2 + 4 + 4 + block_count + block_count);
        bytes.extend_from_slice(&SAVE_MAGIC);
        bytes.extend_from_slice(&SAVE_VERSION.to_le_bytes());
        bytes.extend_from_slice(&(WORLD_XZ as u32).to_le_bytes());
        bytes.extend_from_slice(&(WORLD_MAX_Y as u32).to_le_bytes());

        for y in 0..WORLD_HEIGHT {
            for z in 0..WORLD_XZ {
                for x in 0..WORLD_XZ {
                    let id = self.get_block(IVec3::new(x, y, z));
                    bytes.push(id.to_u8());
                }
            }
        }
        for y in 0..WORLD_HEIGHT {
            for z in 0..WORLD_XZ {
                for x in 0..WORLD_XZ {
                    let pos = IVec3::new(x, y, z);
                    let data = if fluid_kind(self.get_block(pos)).is_some() {
                        self.get_block_data(pos)
                    } else {
                        0
                    };
                    bytes.push(data);
                }
            }
        }

        let tmp_path = save_path.with_extension("bin.tmp");
        fs::write(&tmp_path, &bytes)?;
        fs::rename(tmp_path, save_path)?;
        Ok(())
    }

    pub(crate) fn load_from_file(path: &str) -> io::Result<Option<Self>> {
        let save_path = Path::new(path);
        if !save_path.exists() {
            return Ok(None);
        }

        let bytes = fs::read(save_path)?;
        let header_len = 4 + 2 + 4 + 4;
        if bytes.len() < header_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "save file too small",
            ));
        }

        let file_magic = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if file_magic != SAVE_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "invalid save magic: expected {:?}, got {:?}",
                    SAVE_MAGIC, file_magic
                ),
            ));
        }

        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        if version != SAVE_VERSION
            && version != SAVE_VERSION_LEGACY_V14
            && version != SAVE_VERSION_LEGACY_V13
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unsupported save version: expected {}, {} or {}, got {}",
                    SAVE_VERSION_LEGACY_V13, SAVE_VERSION_LEGACY_V14, SAVE_VERSION, version
                ),
            ));
        }

        let file_world_xz = u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]) as i32;
        let file_world_max_y =
            u32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]) as i32;
        if file_world_xz != WORLD_XZ || file_world_max_y != WORLD_MAX_Y {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "save dimensions mismatch: expected WORLD_XZ={}, WORLD_MAX_Y={}, got WORLD_XZ={}, WORLD_MAX_Y={}",
                    WORLD_XZ, WORLD_MAX_Y, file_world_xz, file_world_max_y
                ),
            ));
        }

        let expected_block_count = (WORLD_XZ * WORLD_XZ * WORLD_HEIGHT) as usize;
        let expected_total_len_blocks_only = header_len + expected_block_count;
        let expected_total_len_with_data = expected_total_len_blocks_only + expected_block_count;
        let has_block_data = bytes.len() == expected_total_len_with_data;
        if bytes.len() != expected_total_len_blocks_only && !has_block_data {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "save payload size mismatch: expected {} or {} bytes, got {} bytes",
                    expected_total_len_blocks_only,
                    expected_total_len_with_data,
                    bytes.len()
                ),
            ));
        }

        let mut world = World::new();
        let mut blocks_index = header_len;
        let mut block_data_index = expected_total_len_blocks_only;
        for y in 0..WORLD_HEIGHT {
            for z in 0..WORLD_XZ {
                for x in 0..WORLD_XZ {
                    let raw = bytes[blocks_index];
                    blocks_index += 1;
                    let id = if version == SAVE_VERSION_LEGACY_V13 {
                        BlockId::from_legacy_v13_u8(raw)
                    } else {
                        BlockId::from_u8(raw)
                    }
                    .map_err(|err| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("invalid block id in save: {} (version {})", err, version),
                        )
                    })?;
                    let raw_data = if has_block_data {
                        let value = bytes[block_data_index];
                        block_data_index += 1;
                        value
                    } else {
                        0
                    };
                    let data = if fluid_kind(id).is_some() {
                        raw_data
                    } else {
                        0
                    };
                    if id != BlockId::Air {
                        let _ = world.set_block_and_data(IVec3::new(x, y, z), id, data);
                    }
                }
            }
        }

        Ok(Some(world))
    }
}
