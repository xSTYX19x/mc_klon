mod caves;
mod ravine;

use glam::IVec3;

use crate::blocks::BlockId;
use crate::world::{CHUNK_SIZE, Chunk, ChunkCoord, World};

pub(crate) use caves::CaveCarver;
pub(crate) use ravine::RavineCarver;

pub(crate) struct Carvers;

impl Carvers {
    pub(crate) const RANGE: i32 = 8;

    pub(crate) fn carve_column(
        world_seed: u64,
        target_chunk_x: i32,
        target_chunk_z: i32,
        column: &mut ChunkColumn,
        _sea_level: i32,
    ) {
        CaveCarver::carve_target_column(world_seed, target_chunk_x, target_chunk_z, column);
        RavineCarver::carve_target_column(world_seed, target_chunk_x, target_chunk_z, column);
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ChunkColumn {
    pub(crate) blocks: Vec<BlockId>,
    pub(crate) data: Vec<u8>,
}

impl ChunkColumn {
    pub(crate) const WIDTH: i32 = 16;
    pub(crate) const HEIGHT: i32 = 256;
    pub(crate) const DEPTH: i32 = 16;
    const LAYER_AREA: usize = (Self::WIDTH as usize) * (Self::DEPTH as usize);
    const VOLUME: usize = Self::LAYER_AREA * (Self::HEIGHT as usize);

    pub(crate) fn new() -> Self {
        Self {
            blocks: vec![BlockId::Air; Self::VOLUME],
            data: vec![0; Self::VOLUME],
        }
    }

    #[inline]
    pub(crate) fn index(x: i32, y: i32, z: i32) -> usize {
        debug_assert!((0..Self::WIDTH).contains(&x));
        debug_assert!((0..Self::HEIGHT).contains(&y));
        debug_assert!((0..Self::DEPTH).contains(&z));
        (x as usize) + (z as usize) * 16 + (y as usize) * 16 * 16
    }

    #[inline]
    pub(crate) fn in_bounds(x: i32, y: i32, z: i32) -> bool {
        (0..Self::WIDTH).contains(&x)
            && (0..Self::HEIGHT).contains(&y)
            && (0..Self::DEPTH).contains(&z)
    }

    #[inline]
    pub(crate) fn get_block(&self, x: i32, y: i32, z: i32) -> BlockId {
        if !Self::in_bounds(x, y, z) {
            return BlockId::Air;
        }
        self.blocks[Self::index(x, y, z)]
    }

    #[inline]
    pub(crate) fn set_block(&mut self, x: i32, y: i32, z: i32, block: BlockId) {
        if Self::in_bounds(x, y, z) {
            let idx = Self::index(x, y, z);
            self.blocks[idx] = block;
        }
    }

    #[inline]
    pub(crate) fn get_data(&self, x: i32, y: i32, z: i32) -> u8 {
        if !Self::in_bounds(x, y, z) {
            return 0;
        }
        self.data[Self::index(x, y, z)]
    }

    #[inline]
    pub(crate) fn set_data(&mut self, x: i32, y: i32, z: i32, value: u8) {
        if Self::in_bounds(x, y, z) {
            let idx = Self::index(x, y, z);
            self.data[idx] = value;
        }
    }

    pub(crate) fn write_chunk_section(&mut self, chunk_y: i32, chunk: &Chunk) {
        if !(0..Self::chunk_count_y()).contains(&chunk_y) {
            return;
        }

        let base_world_y = chunk_y * CHUNK_SIZE;
        for local_y in 0..CHUNK_SIZE {
            let world_y = base_world_y + local_y;
            let dst_start = (world_y as usize) * Self::LAYER_AREA;
            let dst_end = dst_start + Self::LAYER_AREA;
            let src_start = (local_y as usize) * Self::LAYER_AREA;
            let src_end = src_start + Self::LAYER_AREA;
            self.blocks[dst_start..dst_end].copy_from_slice(&chunk.blocks[src_start..src_end]);
            self.data[dst_start..dst_end].copy_from_slice(&chunk.block_data[src_start..src_end]);
        }
    }

    pub(crate) fn extract_chunk(&self, chunk_y: i32) -> Chunk {
        let mut chunk = Chunk::new_empty();
        if !(0..Self::chunk_count_y()).contains(&chunk_y) {
            return chunk;
        }
        let blocks = std::sync::Arc::make_mut(&mut chunk.blocks);
        let data = std::sync::Arc::make_mut(&mut chunk.block_data);

        let base_world_y = chunk_y * CHUNK_SIZE;
        for local_y in 0..CHUNK_SIZE {
            let world_y = base_world_y + local_y;
            let src_start = (world_y as usize) * Self::LAYER_AREA;
            let src_end = src_start + Self::LAYER_AREA;
            let dst_start = (local_y as usize) * Self::LAYER_AREA;
            let dst_end = dst_start + Self::LAYER_AREA;
            blocks[dst_start..dst_end].copy_from_slice(&self.blocks[src_start..src_end]);
            data[dst_start..dst_end].copy_from_slice(&self.data[src_start..src_end]);
        }
        chunk.recompute_cached_block_classification();
        chunk
    }

    pub(crate) fn chunk_count_y() -> i32 {
        Self::HEIGHT / CHUNK_SIZE
    }

    pub(crate) fn from_world_column(world: &World, chunk_x: i32, chunk_z: i32) -> Self {
        let mut column = Self::new();
        let world_x0 = chunk_x * CHUNK_SIZE;
        let world_z0 = chunk_z * CHUNK_SIZE;

        for y in 0..Self::HEIGHT {
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    let wx = world_x0 + x;
                    let wz = world_z0 + z;
                    let pos = IVec3::new(wx, y, wz);
                    column.set_block(x, y, z, world.get_block(pos));
                    column.set_data(x, y, z, world.get_block_data(pos));
                }
            }
        }

        column
    }

    pub(crate) fn write_back_into_world(&self, world: &mut World, chunk_x: i32, chunk_z: i32) {
        for chunk_y in 0..Self::chunk_count_y() {
            let coord = ChunkCoord {
                x: chunk_x,
                y: chunk_y,
                z: chunk_z,
            };
            let chunk = self.extract_chunk(chunk_y);
            if chunk.is_empty() {
                world.chunks.remove(&coord);
            } else {
                world.chunks.insert(coord, chunk);
            }
        }
    }
}
