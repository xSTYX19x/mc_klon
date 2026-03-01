use glam::IVec3;

use crate::blocks::BlockId;

use super::{Chunk, ChunkCoord, World};

pub(crate) trait BlockAccess {
    fn get_block(&self, pos: IVec3) -> BlockId;
    fn get_block_data(&self, pos: IVec3) -> u8;
    fn get_light_packed(&self, pos: IVec3) -> u8;
    fn get_chunk(&self, coord: ChunkCoord) -> Option<&Chunk>;
}

impl BlockAccess for World {
    fn get_block(&self, pos: IVec3) -> BlockId {
        World::get_block(self, pos)
    }

    fn get_block_data(&self, pos: IVec3) -> u8 {
        World::get_block_data(self, pos)
    }

    fn get_light_packed(&self, pos: IVec3) -> u8 {
        World::get_light_packed(self, pos)
    }

    fn get_chunk(&self, coord: ChunkCoord) -> Option<&Chunk> {
        self.chunks.get(&coord)
    }
}
