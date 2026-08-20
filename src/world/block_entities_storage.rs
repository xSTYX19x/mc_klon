use std::collections::HashMap;

use super::block_entity_types::BlockEntity;
use super::{CHUNK_SIZE, ChunkCoord};

pub(crate) type LocalKey = u32;

pub(crate) fn pack_local(x: u32, y: u32, z: u32) -> LocalKey {
    debug_assert!(x < CHUNK_SIZE as u32);
    debug_assert!(y < CHUNK_SIZE as u32);
    debug_assert!(z < CHUNK_SIZE as u32);
    (x & 0x3ff) | ((y & 0x3ff) << 10) | ((z & 0x3ff) << 20)
}

pub(crate) struct ChunkBlockEntities {
    map: HashMap<LocalKey, Box<dyn BlockEntity>>,
}

impl ChunkBlockEntities {
    pub(crate) fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub(crate) fn insert(&mut self, local: LocalKey, be: Box<dyn BlockEntity>) {
        self.map.insert(local, be);
    }

    pub(crate) fn remove(&mut self, local: LocalKey) -> Option<Box<dyn BlockEntity>> {
        self.map.remove(&local)
    }

    pub(crate) fn get_mut(&mut self, local: LocalKey) -> Option<&mut Box<dyn BlockEntity>> {
        self.map.get_mut(&local)
    }

    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub(crate) fn local_keys(&self) -> impl Iterator<Item = LocalKey> + '_ {
        self.map.keys().copied()
    }
}

pub(crate) struct BlockEntitiesStore {
    chunks: HashMap<ChunkCoord, ChunkBlockEntities>,
}

impl BlockEntitiesStore {
    pub(crate) fn new() -> Self {
        Self {
            chunks: HashMap::new(),
        }
    }

    pub(crate) fn insert(&mut self, chunk: ChunkCoord, local: LocalKey, be: Box<dyn BlockEntity>) {
        self.chunks
            .entry(chunk)
            .or_insert_with(ChunkBlockEntities::new)
            .insert(local, be);
    }

    pub(crate) fn remove(
        &mut self,
        chunk: ChunkCoord,
        local: LocalKey,
    ) -> Option<Box<dyn BlockEntity>> {
        let removed = self
            .chunks
            .get_mut(&chunk)
            .and_then(|entries| entries.remove(local));
        let remove_chunk = self
            .chunks
            .get(&chunk)
            .is_some_and(ChunkBlockEntities::is_empty);
        if remove_chunk {
            self.chunks.remove(&chunk);
        }
        removed
    }

    pub(crate) fn get_mut(
        &mut self,
        chunk: ChunkCoord,
        local: LocalKey,
    ) -> Option<&mut Box<dyn BlockEntity>> {
        self.chunks
            .get_mut(&chunk)
            .and_then(|entries| entries.get_mut(local))
    }

    pub(crate) fn total_count(&self) -> usize {
        self.chunks.values().map(ChunkBlockEntities::len).sum()
    }

    pub(crate) fn collect_tick_targets_in_active_columns(
        &self,
        active: &dyn Fn(i32, i32) -> bool,
        out: &mut Vec<(ChunkCoord, LocalKey)>,
    ) -> usize {
        out.clear();
        let mut active_chunks = 0usize;
        for (coord, chunk_entries) in &self.chunks {
            if !active(coord.x, coord.z) {
                continue;
            }
            active_chunks += 1;
            out.extend(chunk_entries.local_keys().map(|local| (*coord, local)));
        }
        active_chunks
    }

    pub(crate) fn retain_loaded_chunks(&mut self, is_loaded: impl Fn(ChunkCoord) -> bool) {
        self.chunks.retain(|coord, _| is_loaded(*coord));
    }
}
