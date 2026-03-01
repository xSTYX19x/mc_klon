use std::sync::Arc;

use crate::blocks::BlockId;

use super::CHUNK_SIZE;

#[derive(Debug)]
pub(crate) struct Chunk {
    pub(crate) blocks: Arc<Vec<BlockId>>,
    pub(crate) block_data: Arc<Vec<u8>>,
    pub(crate) light: Arc<Vec<u8>>, // packed nibbles: high=sky, low=block
    pub(crate) revision: u64,
    filled_voxel_count: u16,
    solid_voxel_count: u16,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LightLevel {
    pub(crate) sky: u8,
    pub(crate) block: u8,
}

impl LightLevel {
    pub(crate) const fn new(sky: u8, block: u8) -> Self {
        Self {
            sky: sky & 0x0F,
            block: block & 0x0F,
        }
    }

    pub(crate) const fn pack(self) -> u8 {
        ((self.sky & 0x0F) << 4) | (self.block & 0x0F)
    }

    pub(crate) const fn from_packed(packed: u8) -> Self {
        Self {
            sky: (packed >> 4) & 0x0F,
            block: packed & 0x0F,
        }
    }
}

impl Chunk {
    const BLOCK_COUNT: usize = (CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE) as usize;

    pub(crate) fn new_empty() -> Self {
        Self {
            blocks: Arc::new(vec![BlockId::Air; Self::BLOCK_COUNT]),
            block_data: Arc::new(vec![0; Self::BLOCK_COUNT]),
            light: Arc::new(vec![0; Self::BLOCK_COUNT]),
            revision: 0,
            filled_voxel_count: 0,
            solid_voxel_count: 0,
        }
    }

    pub(crate) fn from_parts(
        blocks: Arc<Vec<BlockId>>,
        block_data: Arc<Vec<u8>>,
        light: Arc<Vec<u8>>,
        revision: u64,
    ) -> Self {
        debug_assert_eq!(blocks.len(), Self::BLOCK_COUNT);
        debug_assert_eq!(block_data.len(), Self::BLOCK_COUNT);
        debug_assert_eq!(light.len(), Self::BLOCK_COUNT);
        let mut chunk = Self {
            blocks,
            block_data,
            light,
            revision,
            filled_voxel_count: 0,
            solid_voxel_count: 0,
        };
        chunk.recompute_cached_block_classification();
        chunk
    }

    pub(crate) fn from_parts_readonly(
        blocks: Arc<Vec<BlockId>>,
        block_data: Arc<Vec<u8>>,
        light: Arc<Vec<u8>>,
        revision: u64,
    ) -> Self {
        debug_assert_eq!(blocks.len(), Self::BLOCK_COUNT);
        debug_assert_eq!(block_data.len(), Self::BLOCK_COUNT);
        debug_assert_eq!(light.len(), Self::BLOCK_COUNT);
        Self {
            blocks,
            block_data,
            light,
            revision,
            filled_voxel_count: 0,
            solid_voxel_count: 0,
        }
    }

    pub(crate) fn index(x: i32, y: i32, z: i32) -> usize {
        (x + z * CHUNK_SIZE + y * CHUNK_SIZE * CHUNK_SIZE) as usize
    }

    pub(crate) fn in_bounds(x: i32, y: i32, z: i32) -> bool {
        (0..CHUNK_SIZE).contains(&x) && (0..CHUNK_SIZE).contains(&y) && (0..CHUNK_SIZE).contains(&z)
    }

    pub(crate) fn get(&self, x: i32, y: i32, z: i32) -> BlockId {
        if Self::in_bounds(x, y, z) {
            self.blocks[Self::index(x, y, z)]
        } else {
            BlockId::Air
        }
    }

    pub(crate) fn set(&mut self, x: i32, y: i32, z: i32, id: BlockId) {
        if Self::in_bounds(x, y, z) {
            let i = Self::index(x, y, z);
            let blocks = Arc::make_mut(&mut self.blocks);
            let old = blocks[i];
            if old != id {
                blocks[i] = id;
                let old_filled = old.is_voxel_filled() as i32;
                let new_filled = id.is_voxel_filled() as i32;
                let old_solid = old.is_solid() as i32;
                let new_solid = id.is_solid() as i32;
                self.filled_voxel_count = (self.filled_voxel_count as i32 + new_filled - old_filled)
                    .clamp(0, Self::BLOCK_COUNT as i32)
                    as u16;
                self.solid_voxel_count = (self.solid_voxel_count as i32 + new_solid - old_solid)
                    .clamp(0, Self::BLOCK_COUNT as i32)
                    as u16;
                self.revision = self.revision.wrapping_add(1);
            }
        }
    }

    pub(crate) fn get_data(&self, x: i32, y: i32, z: i32) -> u8 {
        if Self::in_bounds(x, y, z) {
            self.block_data[Self::index(x, y, z)]
        } else {
            0
        }
    }

    pub(crate) fn set_data(&mut self, x: i32, y: i32, z: i32, data: u8) {
        if Self::in_bounds(x, y, z) {
            let i = Self::index(x, y, z);
            let block_data = Arc::make_mut(&mut self.block_data);
            if block_data[i] != data {
                block_data[i] = data;
                self.revision = self.revision.wrapping_add(1);
            }
        }
    }

    pub(crate) fn get_light_packed(&self, x: i32, y: i32, z: i32) -> u8 {
        if Self::in_bounds(x, y, z) {
            self.light[Self::index(x, y, z)]
        } else {
            0
        }
    }

    pub(crate) fn get_light(&self, x: i32, y: i32, z: i32) -> LightLevel {
        LightLevel::from_packed(self.get_light_packed(x, y, z))
    }

    pub(crate) fn set_light_packed(&mut self, x: i32, y: i32, z: i32, packed: u8) {
        if Self::in_bounds(x, y, z) {
            let i = Self::index(x, y, z);
            let packed = packed & 0xFF;
            let light = Arc::make_mut(&mut self.light);
            if light[i] != packed {
                light[i] = packed;
                self.revision = self.revision.wrapping_add(1);
            }
        }
    }

    pub(crate) fn set_light(&mut self, x: i32, y: i32, z: i32, light: LightLevel) {
        self.set_light_packed(x, y, z, light.pack());
    }

    pub(crate) fn clear_light(&mut self) {
        let mut changed = false;
        for v in Arc::make_mut(&mut self.light).iter_mut() {
            if *v != 0 {
                *v = 0;
                changed = true;
            }
        }
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.filled_voxel_count == 0
    }

    pub(crate) fn is_full_solid(&self) -> bool {
        self.solid_voxel_count as usize == Self::BLOCK_COUNT
    }

    pub(crate) fn recompute_cached_block_classification(&mut self) {
        let mut filled = 0u16;
        let mut solid = 0u16;
        for &block in self.blocks.iter() {
            if block.is_voxel_filled() {
                filled = filled.saturating_add(1);
            }
            if block.is_solid() {
                solid = solid.saturating_add(1);
            }
        }
        self.filled_voxel_count = filled;
        self.solid_voxel_count = solid;
    }
}

#[derive(Hash, Eq, PartialEq, Copy, Clone, Debug)]
pub(crate) struct ChunkCoord {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) z: i32,
}
