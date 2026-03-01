use glam::{IVec3, Vec3};
use std::collections::HashMap;

use crate::blocks::{BlockModel, BlockTextureLookup, Face};
use crate::world::{BlockAccess, CHUNK_SIZE, ChunkCoord};

use super::greedy::push_textured_quad;
use super::{CpuMeshData, LayeredChunkGeometry, chunk_world_origin};

#[cfg(feature = "allocation_tracking")]
use crate::perf::ALLOCATION_TRACKER;

/// Unified corner cache entry that stores all corner height data for a position
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CornerCacheEntry {
    /// Water surface corner heights
    pub water_surface: [f32; 4],
    /// Lava surface corner heights
    pub lava_surface: [f32; 4],
    /// Water wall corner heights
    pub water_wall: [f32; 4],
    /// Lava wall corner heights
    pub lava_wall: [f32; 4],
    /// Validity flags for each field to avoid recalculation
    pub valid_flags: u8, // bit 0: water_surface, bit 1: lava_surface, bit 2: water_wall, bit 3: lava_wall
}

impl CornerCacheEntry {
    const WATER_SURFACE_VALID: u8 = 0b0001;
    const LAVA_SURFACE_VALID: u8 = 0b0010;
    const WATER_WALL_VALID: u8 = 0b0100;
    const LAVA_WALL_VALID: u8 = 0b1000;

    #[inline]
    pub fn is_water_surface_valid(&self) -> bool {
        self.valid_flags & Self::WATER_SURFACE_VALID != 0
    }

    #[inline]
    pub fn is_lava_surface_valid(&self) -> bool {
        self.valid_flags & Self::LAVA_SURFACE_VALID != 0
    }

    #[inline]
    pub fn is_water_wall_valid(&self) -> bool {
        self.valid_flags & Self::WATER_WALL_VALID != 0
    }

    #[inline]
    pub fn is_lava_wall_valid(&self) -> bool {
        self.valid_flags & Self::LAVA_WALL_VALID != 0
    }

    #[inline]
    pub fn set_water_surface(&mut self, heights: [f32; 4]) {
        self.water_surface = heights;
        self.valid_flags |= Self::WATER_SURFACE_VALID;
    }

    #[inline]
    pub fn set_lava_surface(&mut self, heights: [f32; 4]) {
        self.lava_surface = heights;
        self.valid_flags |= Self::LAVA_SURFACE_VALID;
    }

    #[inline]
    pub fn set_water_wall(&mut self, heights: [f32; 4]) {
        self.water_wall = heights;
        self.valid_flags |= Self::WATER_WALL_VALID;
    }

    #[inline]
    pub fn set_lava_wall(&mut self, heights: [f32; 4]) {
        self.lava_wall = heights;
        self.valid_flags |= Self::LAVA_WALL_VALID;
    }
}

/// Memory pool for liquid meshing to avoid repeated allocations
/// Stores pre-allocated HashMap caches that can be reused across meshing calls
#[derive(Default)]
pub(crate) struct LiquidMeshingPool {
    /// Unified corner cache that reduces HashMap lookups and memory fragmentation
    pub(crate) corner_cache: HashMap<IVec3, CornerCacheEntry>,
}

impl LiquidMeshingPool {
    /// Create a new pool with pre-allocated capacity
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            corner_cache: HashMap::with_capacity(capacity),
        }
    }

    /// Clear all caches for reuse while retaining allocated capacity
    pub(crate) fn clear_for_reuse(&mut self) {
        self.corner_cache.clear();
    }

    /// Get allocation statistics for performance tracking
    pub(crate) fn allocation_stats(&self) -> LiquidPoolStats {
        // Count entries with each type of valid data
        let mut water_surface_used = 0;
        let mut lava_surface_used = 0;
        let mut water_wall_used = 0;
        let mut lava_wall_used = 0;

        for entry in self.corner_cache.values() {
            if entry.is_water_surface_valid() {
                water_surface_used += 1;
            }
            if entry.is_lava_surface_valid() {
                lava_surface_used += 1;
            }
            if entry.is_water_wall_valid() {
                water_wall_used += 1;
            }
            if entry.is_lava_wall_valid() {
                lava_wall_used += 1;
            }
        }

        LiquidPoolStats {
            total_capacity: self.corner_cache.capacity(),
            total_entries: self.corner_cache.len(),
            water_surface_used,
            lava_surface_used,
            water_wall_used,
            lava_wall_used,
        }
    }
}

/// Statistics for tracking memory pool usage
#[derive(Debug, Clone)]
pub(crate) struct LiquidPoolStats {
    pub(crate) total_capacity: usize,
    pub(crate) total_entries: usize,
    pub(crate) water_surface_used: usize,
    pub(crate) lava_surface_used: usize,
    pub(crate) water_wall_used: usize,
    pub(crate) lava_wall_used: usize,
}

impl LiquidPoolStats {
    pub(crate) fn total_capacity(&self) -> usize {
        self.total_capacity
    }

    pub(crate) fn total_used(&self) -> usize {
        self.water_surface_used
            + self.lava_surface_used
            + self.water_wall_used
            + self.lava_wall_used
    }
}

use crate::world::fluids::{
    FluidKind, fluid_data_is_falling, fluid_data_level, fluid_kind, is_source, make_flowing,
    make_still,
};

pub(crate) const LIQUID_SOURCE_HEIGHT: f32 = 1.0;
pub(crate) const LIQUID_EPS: f32 = 0.001;

fn height_from_level(level: u8) -> f32 {
    const MIN_HEIGHT: f32 = 1.0 / 8.0;
    ((8.0 - level as f32) / 8.0).clamp(MIN_HEIGHT, LIQUID_SOURCE_HEIGHT)
}

#[cfg(test)]
pub(crate) fn fluid_surface_height(kind: FluidKind, level: u8, is_falling: bool) -> f32 {
    if is_falling {
        return LIQUID_SOURCE_HEIGHT;
    }
    let _ = kind;
    height_from_level(level)
}

fn is_same_fluid<A: BlockAccess + ?Sized>(world: &A, pos: IVec3, kind: FluidKind) -> bool {
    fluid_kind(world.get_block(pos)) == Some(kind)
}

fn is_covered<A: BlockAccess + ?Sized>(world: &A, pos: IVec3, kind: FluidKind) -> bool {
    is_same_fluid(world, pos + IVec3::new(0, 1, 0), kind)
}

pub(crate) fn surface_height<A: BlockAccess + ?Sized>(
    world: &A,
    pos: IVec3,
    kind: FluidKind,
) -> f32 {
    if !is_same_fluid(world, pos, kind) {
        return 0.0;
    }
    if is_covered(world, pos, kind) {
        return LIQUID_SOURCE_HEIGHT;
    }
    let data = world.get_block_data(pos);
    if fluid_data_is_falling(data) {
        return LIQUID_SOURCE_HEIGHT;
    }
    let level = fluid_data_level(data);
    let _ = kind;
    height_from_level(level)
}

pub(crate) fn wall_height<A: BlockAccess + ?Sized>(world: &A, pos: IVec3, kind: FluidKind) -> f32 {
    if !is_same_fluid(world, pos, kind) {
        return 0.0;
    }
    if is_covered(world, pos, kind) {
        return 1.0;
    }
    surface_height(world, pos, kind)
}

#[cfg(test)]
pub(crate) fn effective_cell_height<A: BlockAccess + ?Sized>(
    world: &A,
    pos: IVec3,
    kind: FluidKind,
) -> f32 {
    wall_height(world, pos, kind)
}

pub(super) fn fluid_surface_height_at<A: BlockAccess + ?Sized>(
    world: &A,
    pos: IVec3,
    kind: FluidKind,
) -> f32 {
    surface_height(world, pos, kind)
}

fn surface_corner_heights_for_cell<A: BlockAccess + ?Sized>(
    world: &A,
    pos: IVec3,
    kind: FluidKind,
) -> [f32; 4] {
    [
        corner_height_with_sampler(
            world,
            pos,
            kind,
            0,
            0,
            sample_surface_height::<A>,
            LIQUID_SOURCE_HEIGHT,
        ), // NW
        corner_height_with_sampler(
            world,
            pos,
            kind,
            1,
            0,
            sample_surface_height::<A>,
            LIQUID_SOURCE_HEIGHT,
        ), // NE
        corner_height_with_sampler(
            world,
            pos,
            kind,
            0,
            1,
            sample_surface_height::<A>,
            LIQUID_SOURCE_HEIGHT,
        ), // SW
        corner_height_with_sampler(
            world,
            pos,
            kind,
            1,
            1,
            sample_surface_height::<A>,
            LIQUID_SOURCE_HEIGHT,
        ), // SE
    ]
}

fn wall_corner_heights_for_cell<A: BlockAccess + ?Sized>(
    world: &A,
    pos: IVec3,
    kind: FluidKind,
) -> [f32; 4] {
    [
        corner_height_with_sampler(world, pos, kind, 0, 0, sample_wall_height::<A>, 1.0), // NW
        corner_height_with_sampler(world, pos, kind, 1, 0, sample_wall_height::<A>, 1.0), // NE
        corner_height_with_sampler(world, pos, kind, 0, 1, sample_wall_height::<A>, 1.0), // SW
        corner_height_with_sampler(world, pos, kind, 1, 1, sample_wall_height::<A>, 1.0), // SE
    ]
}

fn bilerp(h_nw: f32, h_ne: f32, h_sw: f32, h_se: f32, lx: f32, lz: f32) -> f32 {
    let h_n = h_nw + (h_ne - h_nw) * lx;
    let h_s = h_sw + (h_se - h_sw) * lx;
    h_n + (h_s - h_n) * lz
}

pub(crate) fn liquid_height_at_point<A: BlockAccess + ?Sized>(
    world: &A,
    kind: FluidKind,
    p_world: Vec3,
) -> Option<f32> {
    let mut cell = IVec3::new(
        p_world.x.floor() as i32,
        p_world.y.floor() as i32,
        p_world.z.floor() as i32,
    );
    if fluid_kind(world.get_block(cell)) != Some(kind) {
        return None;
    }

    while fluid_kind(world.get_block(cell + IVec3::Y)) == Some(kind) {
        cell += IVec3::Y;
    }

    let lx = (p_world.x - cell.x as f32).clamp(0.0, 1.0);
    let lz = (p_world.z - cell.z as f32).clamp(0.0, 1.0);
    let [h_nw, h_ne, h_sw, h_se] = surface_corner_heights_for_cell(world, cell, kind);
    let h = bilerp(h_nw, h_ne, h_sw, h_se, lx, lz);
    Some(cell.y as f32 + h)
}

pub(crate) fn liquid_fill_height_at_point(
    world: &impl BlockAccess,
    kind: FluidKind,
    p_world: Vec3,
) -> Option<f32> {
    let cell = IVec3::new(
        p_world.x.floor() as i32,
        p_world.y.floor() as i32,
        p_world.z.floor() as i32,
    );
    if fluid_kind(world.get_block(cell)) != Some(kind) {
        return None;
    }

    let lx = (p_world.x - cell.x as f32).clamp(0.0, 1.0);
    let lz = (p_world.z - cell.z as f32).clamp(0.0, 1.0);
    let [h_nw, h_ne, h_sw, h_se] = wall_corner_heights_for_cell(world, cell, kind);
    let h = bilerp(h_nw, h_ne, h_sw, h_se, lx, lz);
    Some(cell.y as f32 + h)
}

pub(crate) fn fluid_kind_at_point(world: &impl BlockAccess, p_world: Vec3) -> Option<FluidKind> {
    let cell = IVec3::new(
        p_world.x.floor() as i32,
        p_world.y.floor() as i32,
        p_world.z.floor() as i32,
    );
    let kind = fluid_kind(world.get_block(cell))?;
    let fill_height = liquid_fill_height_at_point(world, kind, p_world)?;
    if p_world.y < fill_height - 1e-4 {
        Some(kind)
    } else {
        None
    }
}

fn sample_surface_height<A: BlockAccess + ?Sized>(
    world: &A,
    pos: IVec3,
    kind: FluidKind,
) -> Option<f32> {
    let h = surface_height(world, pos, kind);
    if h <= LIQUID_EPS { None } else { Some(h) }
}

fn sample_wall_height<A: BlockAccess + ?Sized>(
    world: &A,
    pos: IVec3,
    kind: FluidKind,
) -> Option<f32> {
    let h = wall_height(world, pos, kind);
    if h <= LIQUID_EPS { None } else { Some(h) }
}

fn corner_height_with_sampler<A, F>(
    world: &A,
    pos: IVec3,
    kind: FluidKind,
    dx: i32,
    dz: i32,
    sampler: F,
    max_h: f32,
) -> f32
where
    A: BlockAccess + ?Sized,
    F: Fn(&A, IVec3, FluidKind) -> Option<f32>,
{
    let sx = if dx == 0 { -1 } else { 1 };
    let sz = if dz == 0 { -1 } else { 1 };
    let samples = [
        pos,
        pos + IVec3::new(sx, 0, 0),
        pos + IVec3::new(0, 0, sz),
        pos + IVec3::new(sx, 0, sz),
    ];

    let mut weighted_sum = 0.0_f32;
    let mut weight_total = 0.0_f32;
    for sample in samples {
        let Some(h) = sampler(world, sample, kind) else {
            continue;
        };
        let weight = if h >= 0.875 * LIQUID_SOURCE_HEIGHT {
            10.0
        } else {
            1.0
        };
        weighted_sum += h * weight;
        weight_total += weight;
    }
    if weight_total <= LIQUID_EPS {
        return 0.0;
    }
    (weighted_sum / weight_total).clamp(0.0, max_h)
}

fn surface_corner_heights_for_cell_cached<A: BlockAccess + ?Sized>(
    world: &A,
    pos: IVec3,
    kind: FluidKind,
    cache: &mut HashMap<IVec3, CornerCacheEntry>,
) -> [f32; 4] {
    let entry = cache.entry(pos).or_default();

    match kind {
        FluidKind::Water => {
            if entry.is_water_surface_valid() {
                return entry.water_surface;
            }
            let heights = surface_corner_heights_for_cell(world, pos, kind);
            entry.set_water_surface(heights);
            heights
        }
        FluidKind::Lava => {
            if entry.is_lava_surface_valid() {
                return entry.lava_surface;
            }
            let heights = surface_corner_heights_for_cell(world, pos, kind);
            entry.set_lava_surface(heights);
            heights
        }
    }
}

fn wall_corner_heights_for_cell_cached<A: BlockAccess + ?Sized>(
    world: &A,
    pos: IVec3,
    kind: FluidKind,
    cache: &mut HashMap<IVec3, CornerCacheEntry>,
) -> [f32; 4] {
    let entry = cache.entry(pos).or_default();

    match kind {
        FluidKind::Water => {
            if entry.is_water_wall_valid() {
                return entry.water_wall;
            }
            let heights = wall_corner_heights_for_cell(world, pos, kind);
            entry.set_water_wall(heights);
            heights
        }
        FluidKind::Lava => {
            if entry.is_lava_wall_valid() {
                return entry.lava_wall;
            }
            let heights = wall_corner_heights_for_cell(world, pos, kind);
            entry.set_lava_wall(heights);
            heights
        }
    }
}

fn edge_heights(corners: [f32; 4], face: Face) -> (f32, f32) {
    match face {
        Face::North => (corners[0], corners[1]),
        Face::South => (corners[2], corners[3]),
        Face::West => (corners[0], corners[2]),
        Face::East => (corners[1], corners[3]),
        _ => (0.0, 0.0),
    }
}

fn side_v(local_y: f32) -> f32 {
    (local_y / LIQUID_SOURCE_HEIGHT).clamp(0.0, 1.0)
}

fn liquid_side_v(kind: FluidKind, local_y: f32) -> f32 {
    let v = side_v(local_y);
    if kind == FluidKind::Lava { 1.0 - v } else { v }
}

fn emit_liquid_side(
    mesh: &mut CpuMeshData,
    kind: FluidKind,
    face: Face,
    wx: f32,
    wy: f32,
    wz: f32,
    bottom_left: f32,
    bottom_right: f32,
    top_left: f32,
    top_right: f32,
    tex_layer: u32,
    tint_kind: u32,
) {
    let (positions, uvs) = match face {
        Face::South => (
            [
                [wx + 0.0, wy + bottom_left, wz + 1.0],
                [wx + 1.0, wy + bottom_right, wz + 1.0],
                [wx + 1.0, wy + top_right, wz + 1.0],
                [wx + 0.0, wy + top_left, wz + 1.0],
            ],
            [
                [0.0, liquid_side_v(kind, bottom_left)],
                [1.0, liquid_side_v(kind, bottom_right)],
                [1.0, liquid_side_v(kind, top_right)],
                [0.0, liquid_side_v(kind, top_left)],
            ],
        ),
        Face::North => (
            [
                [wx + 1.0, wy + bottom_right, wz + 0.0],
                [wx + 0.0, wy + bottom_left, wz + 0.0],
                [wx + 0.0, wy + top_left, wz + 0.0],
                [wx + 1.0, wy + top_right, wz + 0.0],
            ],
            [
                [1.0, liquid_side_v(kind, bottom_right)],
                [0.0, liquid_side_v(kind, bottom_left)],
                [0.0, liquid_side_v(kind, top_left)],
                [1.0, liquid_side_v(kind, top_right)],
            ],
        ),
        Face::West => (
            [
                [wx + 0.0, wy + bottom_left, wz + 0.0],
                [wx + 0.0, wy + bottom_right, wz + 1.0],
                [wx + 0.0, wy + top_right, wz + 1.0],
                [wx + 0.0, wy + top_left, wz + 0.0],
            ],
            [
                [0.0, liquid_side_v(kind, bottom_left)],
                [1.0, liquid_side_v(kind, bottom_right)],
                [1.0, liquid_side_v(kind, top_right)],
                [0.0, liquid_side_v(kind, top_left)],
            ],
        ),
        Face::East => (
            [
                [wx + 1.0, wy + bottom_right, wz + 1.0],
                [wx + 1.0, wy + bottom_left, wz + 0.0],
                [wx + 1.0, wy + top_left, wz + 0.0],
                [wx + 1.0, wy + top_right, wz + 1.0],
            ],
            [
                [1.0, liquid_side_v(kind, bottom_right)],
                [0.0, liquid_side_v(kind, bottom_left)],
                [0.0, liquid_side_v(kind, top_left)],
                [1.0, liquid_side_v(kind, top_right)],
            ],
        ),
        _ => return,
    };
    push_textured_quad(mesh, positions, uvs, tex_layer, tint_kind);
}

pub(super) fn append_liquids_in_chunk_pooled(
    world: &impl BlockAccess,
    coord: ChunkCoord,
    chunk_offset: [f32; 3],
    texture_lookup: &BlockTextureLookup,
    layered: &mut LayeredChunkGeometry,
    pool_opt: Option<&mut super::pool::MeshingPool>,
) {
    if let Some(pool) = pool_opt {
        // Use the unified pool's liquid pool
        let Some(chunk) = world.get_chunk(coord) else {
            return;
        };
        let chunk_origin = chunk_world_origin(coord);

        pool.liquid_pool.clear_for_reuse();
        append_liquids_in_chunk_with_pool(
            world,
            coord,
            chunk,
            chunk_origin,
            chunk_offset,
            texture_lookup,
            layered,
            &mut pool.liquid_pool,
        );
    } else {
        append_liquids_in_chunk(world, coord, chunk_offset, texture_lookup, layered);
    }
}

pub(super) fn append_liquids_in_chunk(
    world: &impl BlockAccess,
    coord: ChunkCoord,
    chunk_offset: [f32; 3],
    texture_lookup: &BlockTextureLookup,
    layered: &mut LayeredChunkGeometry,
) {
    let Some(chunk) = world.get_chunk(coord) else {
        return;
    };
    let chunk_origin = chunk_world_origin(coord);

    // BEFORE: 4 HashMap allocations per meshing call
    // let mut water_surface_corner_cache: HashMap<IVec3, [f32; 4]> = HashMap::new();
    // let mut lava_surface_corner_cache: HashMap<IVec3, [f32; 4]> = HashMap::new();
    // let mut water_wall_corner_cache: HashMap<IVec3, [f32; 4]> = HashMap::new();
    // let mut lava_wall_corner_cache: HashMap<IVec3, [f32; 4]> = HashMap::new();

    // AFTER: Use thread-local pool to reuse allocations
    thread_local! {
        static LIQUID_POOL: std::cell::RefCell<LiquidMeshingPool> = std::cell::RefCell::new(
            LiquidMeshingPool::with_capacity(64) // Pre-allocate for typical chunk liquid content
        );
    }

    LIQUID_POOL.with(|pool| {
        let mut pool = pool.borrow_mut();
        pool.clear_for_reuse(); // Clear contents but keep allocated memory

        append_liquids_in_chunk_with_pool(
            world,
            coord,
            chunk,
            chunk_origin,
            chunk_offset,
            texture_lookup,
            layered,
            &mut pool,
        );

        #[cfg(feature = "allocation_tracking")]
        ALLOCATION_TRACKER.track_mesh_job();
    });
}

/// Internal implementation using the memory pool
fn append_liquids_in_chunk_with_pool(
    world: &impl BlockAccess,
    _coord: ChunkCoord,
    chunk: &crate::world::Chunk,
    chunk_origin: IVec3,
    chunk_offset: [f32; 3],
    texture_lookup: &BlockTextureLookup,
    layered: &mut LayeredChunkGeometry,
    pool: &mut LiquidMeshingPool,
) {
    for y in 0..CHUNK_SIZE {
        for z in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                let id = chunk.get(x, y, z);
                let BlockModel::Liquid { kind } = texture_lookup.model_for(id) else {
                    continue;
                };

                let world_pos = chunk_origin + IVec3::new(x, y, z);
                let covered = is_covered(world, world_pos, kind);
                let surface_corners = surface_corner_heights_for_cell_cached(
                    world,
                    world_pos,
                    kind,
                    &mut pool.corner_cache,
                );
                let [h_nw_surface, h_ne_surface, h_sw_surface, h_se_surface] = surface_corners;
                let surface_h_max = h_nw_surface
                    .max(h_ne_surface)
                    .max(h_sw_surface)
                    .max(h_se_surface);
                if !covered && surface_h_max <= LIQUID_EPS {
                    continue;
                }
                let wall_corners = wall_corner_heights_for_cell_cached(
                    world,
                    world_pos,
                    kind,
                    &mut pool.corner_cache,
                );

                let data = world.get_block_data(world_pos);
                let render_layer = texture_lookup.render_layer_for(id);
                let mesh = layered.mesh_mut(render_layer);
                let still_block = make_still(kind);
                let side_block = if is_source(id, data) {
                    still_block
                } else {
                    make_flowing(kind)
                };
                let wx = x as f32 + chunk_offset[0];
                let wy = y as f32 + chunk_offset[1];
                let wz = z as f32 + chunk_offset[2];

                if !covered {
                    push_textured_quad(
                        mesh,
                        [
                            [wx + 0.0, wy + h_sw_surface, wz + 1.0], // SW
                            [wx + 1.0, wy + h_se_surface, wz + 1.0], // SE
                            [wx + 1.0, wy + h_ne_surface, wz + 0.0], // NE
                            [wx + 0.0, wy + h_nw_surface, wz + 0.0], // NW
                        ],
                        [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
                        texture_lookup.layer_for(still_block, Face::Up),
                        texture_lookup.tint_kind_for(still_block, Face::Up),
                    );
                }

                for (face, dir) in [
                    (Face::South, IVec3::new(0, 0, 1)),
                    (Face::North, IVec3::new(0, 0, -1)),
                    (Face::East, IVec3::new(1, 0, 0)),
                    (Face::West, IVec3::new(-1, 0, 0)),
                ] {
                    let neighbor_pos = world_pos + dir;
                    let neighbor = world.get_block(neighbor_pos);
                    let (our_left, our_right) = edge_heights(wall_corners, face);
                    if our_left.max(our_right) <= LIQUID_EPS {
                        continue;
                    }

                    let mut bottom_left = 0.0_f32;
                    let mut bottom_right = 0.0_f32;
                    let emit_side = if fluid_kind(neighbor) == Some(kind) {
                        let neighbor_corners = wall_corner_heights_for_cell_cached(
                            world,
                            neighbor_pos,
                            kind,
                            &mut pool.corner_cache,
                        );
                        let opposite_face = match face {
                            Face::North => Face::South,
                            Face::South => Face::North,
                            Face::West => Face::East,
                            Face::East => Face::West,
                            _ => Face::North,
                        };
                        let (neighbor_left, neighbor_right) =
                            edge_heights(neighbor_corners, opposite_face);
                        bottom_left = neighbor_left.clamp(0.0, our_left);
                        bottom_right = neighbor_right.clamp(0.0, our_right);
                        !(neighbor_left >= our_left - LIQUID_EPS
                            && neighbor_right >= our_right - LIQUID_EPS)
                    } else {
                        !neighbor.is_voxel_filled() || !texture_lookup.face_occluding_for(neighbor)
                    };

                    if !emit_side
                        || our_left <= bottom_left + LIQUID_EPS
                            && our_right <= bottom_right + LIQUID_EPS
                    {
                        continue;
                    }

                    emit_liquid_side(
                        mesh,
                        kind,
                        face,
                        wx,
                        wy,
                        wz,
                        bottom_left,
                        bottom_right,
                        our_left,
                        our_right,
                        texture_lookup.layer_for(side_block, face),
                        texture_lookup.tint_kind_for(side_block, face),
                    );
                }
            }
        }
    }
}
