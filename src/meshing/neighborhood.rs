use std::sync::Arc;

use glam::IVec3;

use crate::blocks::BlockId;
use crate::world::{BlockAccess, CHUNK_SIZE, Chunk, ChunkCoord};

#[derive(Clone)]
pub(crate) struct ChunkSnapshot {
    pub(crate) blocks: Arc<Vec<BlockId>>,
    pub(crate) block_data: Arc<Vec<u8>>,
    pub(crate) light: Arc<Vec<u8>>,
}

#[derive(Clone)]
pub(crate) struct NeighborhoodSnapshot {
    pub(crate) center: ChunkSnapshot,
    // Order: +X, -X, +Y, -Y, +Z, -Z.
    pub(crate) neighbors: [Option<ChunkSnapshot>; 6],
}

/// Sky=15, block=0. What an unknown neighbour cell defaults to.
const FULL_SKYLIGHT_PACKED: u8 = 0xF0;

pub(crate) const PADDED_CHUNK_SIZE: i32 = CHUNK_SIZE + 2;
pub(crate) const PADDED_CHUNK_STRIDE_Z: usize = PADDED_CHUNK_SIZE as usize;
pub(crate) const PADDED_CHUNK_STRIDE_Y: usize = PADDED_CHUNK_STRIDE_Z * PADDED_CHUNK_STRIDE_Z;
pub(crate) const PADDED_CHUNK_VOLUME: usize = PADDED_CHUNK_STRIDE_Y * PADDED_CHUNK_STRIDE_Z;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MeshingNeighborhoodBuildStats {
    pub(crate) copied_voxels: u32,
    pub(crate) allocations_avoided: u32,
}

#[derive(Default)]
pub(crate) struct MeshingNeighborhoodScratch {
    blocks: Vec<BlockId>,
    block_data: Vec<u8>,
    light: Vec<u8>,
}

impl MeshingNeighborhoodScratch {
    pub(crate) fn new() -> Self {
        Self {
            blocks: Vec::with_capacity(PADDED_CHUNK_VOLUME),
            block_data: Vec::with_capacity(PADDED_CHUNK_VOLUME),
            light: Vec::with_capacity(PADDED_CHUNK_VOLUME),
        }
    }

    pub(crate) fn build<'a>(
        &'a mut self,
        origin_coord: ChunkCoord,
        snapshot: NeighborhoodSnapshot,
    ) -> (MeshingNeighborhood<'a>, MeshingNeighborhoodBuildStats) {
        let mut stats = MeshingNeighborhoodBuildStats::default();
        stats.allocations_avoided += ensure_capacity(&mut self.blocks, PADDED_CHUNK_VOLUME);
        stats.allocations_avoided += ensure_capacity(&mut self.block_data, PADDED_CHUNK_VOLUME);
        stats.allocations_avoided += ensure_capacity(&mut self.light, PADDED_CHUNK_VOLUME);

        // `clear` + `resize` fills the whole (now empty) buffer in one pass; the
        // previous `resize` followed by `fill` walked all 5832 elements twice.
        self.blocks.clear();
        self.blocks.resize(PADDED_CHUNK_VOLUME, BlockId::Air);
        self.block_data.clear();
        self.block_data.resize(PADDED_CHUNK_VOLUME, 0);
        self.light.resize(PADDED_CHUNK_VOLUME, 0);
        fill_default_light(
            &mut self.light,
            origin_coord.y * CHUNK_SIZE,
            PADDED_CHUNK_STRIDE_Y,
        );

        copy_full_chunk(
            &snapshot.center,
            0,
            0,
            0,
            &mut self.blocks,
            &mut self.block_data,
            &mut self.light,
        );
        stats.copied_voxels += (CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE) as u32;

        if let Some(neighbor) = snapshot.neighbors[0].as_ref() {
            copy_x_face(
                neighbor,
                0,
                CHUNK_SIZE,
                &mut self.blocks,
                &mut self.block_data,
                &mut self.light,
            );
            stats.copied_voxels += (CHUNK_SIZE * CHUNK_SIZE) as u32;
        }
        if let Some(neighbor) = snapshot.neighbors[1].as_ref() {
            copy_x_face(
                neighbor,
                CHUNK_SIZE - 1,
                -1,
                &mut self.blocks,
                &mut self.block_data,
                &mut self.light,
            );
            stats.copied_voxels += (CHUNK_SIZE * CHUNK_SIZE) as u32;
        }
        if let Some(neighbor) = snapshot.neighbors[2].as_ref() {
            copy_y_face(
                neighbor,
                0,
                CHUNK_SIZE,
                &mut self.blocks,
                &mut self.block_data,
                &mut self.light,
            );
            stats.copied_voxels += (CHUNK_SIZE * CHUNK_SIZE) as u32;
        }
        if let Some(neighbor) = snapshot.neighbors[3].as_ref() {
            copy_y_face(
                neighbor,
                CHUNK_SIZE - 1,
                -1,
                &mut self.blocks,
                &mut self.block_data,
                &mut self.light,
            );
            stats.copied_voxels += (CHUNK_SIZE * CHUNK_SIZE) as u32;
        }
        if let Some(neighbor) = snapshot.neighbors[4].as_ref() {
            copy_z_face(
                neighbor,
                0,
                CHUNK_SIZE,
                &mut self.blocks,
                &mut self.block_data,
                &mut self.light,
            );
            stats.copied_voxels += (CHUNK_SIZE * CHUNK_SIZE) as u32;
        }
        if let Some(neighbor) = snapshot.neighbors[5].as_ref() {
            copy_z_face(
                neighbor,
                CHUNK_SIZE - 1,
                -1,
                &mut self.blocks,
                &mut self.block_data,
                &mut self.light,
            );
            stats.copied_voxels += (CHUNK_SIZE * CHUNK_SIZE) as u32;
        }

        let center_chunk = Chunk::from_parts_readonly(
            snapshot.center.blocks,
            snapshot.center.block_data,
            snapshot.center.light,
            0,
        );

        (
            MeshingNeighborhood {
                origin_coord,
                origin_world: IVec3::new(
                    origin_coord.x * CHUNK_SIZE,
                    origin_coord.y * CHUNK_SIZE,
                    origin_coord.z * CHUNK_SIZE,
                ),
                center_chunk,
                blocks: &self.blocks,
                block_data: &self.block_data,
                light: &self.light,
            },
            stats,
        )
    }
}

#[inline(always)]
fn ensure_capacity<T>(buffer: &mut Vec<T>, needed: usize) -> u32 {
    if buffer.capacity() >= needed {
        1
    } else {
        buffer.reserve(needed.saturating_sub(buffer.capacity()));
        0
    }
}

#[inline]
fn fill_default_light(light: &mut [u8], origin_world_y: i32, y_stride: usize) {
    for padded_y in 0..PADDED_CHUNK_SIZE as usize {
        let world_y = origin_world_y + padded_y as i32 - 1;
        // Below the world floor there is no sky; everywhere else a missing
        // neighbour defaults to full skylight so an absent chunk brightens the
        // halo rather than baking a dark seam into the mesh.
        let fill = if world_y < 0 { 0 } else { FULL_SKYLIGHT_PACKED };
        let start = padded_y * y_stride;
        let end = start + y_stride;
        light[start..end].fill(fill);
    }
}

#[inline(always)]
fn padded_index(local_x: i32, local_y: i32, local_z: i32) -> usize {
    debug_assert!((-1..=CHUNK_SIZE).contains(&local_x));
    debug_assert!((-1..=CHUNK_SIZE).contains(&local_y));
    debug_assert!((-1..=CHUNK_SIZE).contains(&local_z));
    let px = (local_x + 1) as usize;
    let py = (local_y + 1) as usize;
    let pz = (local_z + 1) as usize;
    px + pz * PADDED_CHUNK_STRIDE_Z + py * PADDED_CHUNK_STRIDE_Y
}

fn copy_full_chunk(
    snapshot: &ChunkSnapshot,
    dst_x: i32,
    dst_y: i32,
    dst_z: i32,
    blocks: &mut [BlockId],
    block_data: &mut [u8],
    light: &mut [u8],
) {
    for y in 0..CHUNK_SIZE {
        for z in 0..CHUNK_SIZE {
            let src = Chunk::index(0, y, z);
            let dst = padded_index(dst_x, dst_y + y, dst_z + z);
            let src_end = src + CHUNK_SIZE as usize;
            let dst_end = dst + CHUNK_SIZE as usize;
            blocks[dst..dst_end].clone_from_slice(&snapshot.blocks[src..src_end]);
            block_data[dst..dst_end].copy_from_slice(&snapshot.block_data[src..src_end]);
            light[dst..dst_end].copy_from_slice(&snapshot.light[src..src_end]);
        }
    }
}

fn copy_x_face(
    snapshot: &ChunkSnapshot,
    src_x: i32,
    dst_x: i32,
    blocks: &mut [BlockId],
    block_data: &mut [u8],
    light: &mut [u8],
) {
    for y in 0..CHUNK_SIZE {
        for z in 0..CHUNK_SIZE {
            let src = Chunk::index(src_x, y, z);
            let dst = padded_index(dst_x, y, z);
            blocks[dst] = snapshot.blocks[src];
            block_data[dst] = snapshot.block_data[src];
            light[dst] = snapshot.light[src];
        }
    }
}

fn copy_y_face(
    snapshot: &ChunkSnapshot,
    src_y: i32,
    dst_y: i32,
    blocks: &mut [BlockId],
    block_data: &mut [u8],
    light: &mut [u8],
) {
    for z in 0..CHUNK_SIZE {
        let src = Chunk::index(0, src_y, z);
        let dst = padded_index(0, dst_y, z);
        let src_end = src + CHUNK_SIZE as usize;
        let dst_end = dst + CHUNK_SIZE as usize;
        blocks[dst..dst_end].clone_from_slice(&snapshot.blocks[src..src_end]);
        block_data[dst..dst_end].copy_from_slice(&snapshot.block_data[src..src_end]);
        light[dst..dst_end].copy_from_slice(&snapshot.light[src..src_end]);
    }
}

fn copy_z_face(
    snapshot: &ChunkSnapshot,
    src_z: i32,
    dst_z: i32,
    blocks: &mut [BlockId],
    block_data: &mut [u8],
    light: &mut [u8],
) {
    for y in 0..CHUNK_SIZE {
        let src = Chunk::index(0, y, src_z);
        let dst = padded_index(0, y, dst_z);
        let src_end = src + CHUNK_SIZE as usize;
        let dst_end = dst + CHUNK_SIZE as usize;
        blocks[dst..dst_end].clone_from_slice(&snapshot.blocks[src..src_end]);
        block_data[dst..dst_end].copy_from_slice(&snapshot.block_data[src..src_end]);
        light[dst..dst_end].copy_from_slice(&snapshot.light[src..src_end]);
    }
}

// The volume stores the center chunk at local coordinates 0..15 and a one-cell
// halo at -1 / 16 on each axis. That keeps every hot-path read in a single,
// dense array lookup while preserving the old snapshot semantics for missing
// diagonal chunks by leaving those cells at their default Air / fallback light.
pub(crate) struct MeshingNeighborhood<'a> {
    origin_coord: ChunkCoord,
    origin_world: IVec3,
    center_chunk: Chunk,
    blocks: &'a [BlockId],
    block_data: &'a [u8],
    light: &'a [u8],
}

impl<'a> MeshingNeighborhood<'a> {
    #[inline(always)]
    pub(crate) fn center_chunk(&self) -> &Chunk {
        &self.center_chunk
    }

    #[inline(always)]
    pub(crate) fn block_local(&self, local_x: i32, local_y: i32, local_z: i32) -> BlockId {
        self.blocks[padded_index(local_x, local_y, local_z)]
    }

    #[inline(always)]
    pub(crate) fn local_padded_index(local_x: i32, local_y: i32, local_z: i32) -> usize {
        padded_index(local_x, local_y, local_z)
    }

    #[inline(always)]
    pub(crate) fn block_at_index(&self, index: usize) -> BlockId {
        self.blocks[index]
    }

    #[inline(always)]
    pub(crate) fn block_data_local(&self, local_x: i32, local_y: i32, local_z: i32) -> u8 {
        self.block_data[padded_index(local_x, local_y, local_z)]
    }

    #[inline(always)]
    pub(crate) fn light_local(&self, local_x: i32, local_y: i32, local_z: i32) -> u8 {
        self.light[padded_index(local_x, local_y, local_z)]
    }

    #[inline(always)]
    pub(crate) fn light_at_index(&self, index: usize) -> u8 {
        self.light[index]
    }

    #[inline(always)]
    fn in_halo(local: IVec3) -> bool {
        (-1..=CHUNK_SIZE).contains(&local.x)
            && (-1..=CHUNK_SIZE).contains(&local.y)
            && (-1..=CHUNK_SIZE).contains(&local.z)
    }

    #[inline(always)]
    fn default_light_for_world_y(world_y: i32) -> u8 {
        if world_y < 0 { 0 } else { FULL_SKYLIGHT_PACKED }
    }
}

impl BlockAccess for MeshingNeighborhood<'_> {
    fn get_block(&self, pos: IVec3) -> BlockId {
        let local = pos - self.origin_world;
        if Self::in_halo(local) {
            self.block_local(local.x, local.y, local.z)
        } else {
            BlockId::Air
        }
    }

    fn get_block_data(&self, pos: IVec3) -> u8 {
        let local = pos - self.origin_world;
        if Self::in_halo(local) {
            self.block_data_local(local.x, local.y, local.z)
        } else {
            0
        }
    }

    fn get_light_packed(&self, pos: IVec3) -> u8 {
        let local = pos - self.origin_world;
        if Self::in_halo(local) {
            self.light_local(local.x, local.y, local.z)
        } else {
            Self::default_light_for_world_y(pos.y)
        }
    }

    fn get_chunk(&self, coord: ChunkCoord) -> Option<&Chunk> {
        if coord == self.origin_coord {
            Some(&self.center_chunk)
        } else {
            None
        }
    }
}
