use glam::IVec3;

use crate::blocks::block_entity_key_for_block;
use crate::registry::NamespacedKey;
use crate::sim::{ActiveSimColumns, ColumnCoord, SimSystem};
use crate::world::block_entities_storage::{BlockEntitiesStore, LocalKey, pack_local};
use crate::world::block_entity_types::{BlockEntityRegistry, register_core_block_entity_types};
use crate::world::{BlockEntityBlockChange, ChunkCoord, World};

pub(crate) struct BlockEntitiesSystem {
    store: BlockEntitiesStore,
    registry: BlockEntityRegistry,
    pending_block_changes: Vec<BlockEntityBlockChange>,
    tick_targets: Vec<(ChunkCoord, LocalKey)>,
    active_chunk_count: usize,
}

impl BlockEntitiesSystem {
    pub(crate) fn new() -> Self {
        let mut registry = BlockEntityRegistry::new();
        register_core_block_entity_types(&mut registry);
        Self {
            store: BlockEntitiesStore::new(),
            registry,
            pending_block_changes: Vec::new(),
            tick_targets: Vec::new(),
            active_chunk_count: 0,
        }
    }

    pub(crate) fn debug_counts(&self) -> (usize, usize) {
        (self.store.total_count(), self.active_chunk_count)
    }

    pub(crate) fn reset(&mut self, world: &mut World) {
        self.store = BlockEntitiesStore::new();
        self.tick_targets.clear();
        world.drain_block_entity_block_changes(&mut self.pending_block_changes);
        self.pending_block_changes.clear();
        self.active_chunk_count = 0;
    }

    fn process_world_block_changes(&mut self, world: &mut World) {
        world.drain_block_entity_block_changes(&mut self.pending_block_changes);
        if self.pending_block_changes.is_empty() {
            return;
        }

        while let Some(change) = self.pending_block_changes.pop() {
            self.apply_block_change(change);
        }
    }

    fn apply_block_change(&mut self, change: BlockEntityBlockChange) {
        let old_key = block_entity_key_for_block(change.old_block);
        let new_key = block_entity_key_for_block(change.new_block);
        if old_key == new_key {
            return;
        }

        let chunk = World::world_to_chunk_coord(change.pos);
        let local = World::world_to_local(change.pos);
        let local_key = pack_local(local.x as u32, local.y as u32, local.z as u32);

        if old_key.is_some() {
            let _ = self.store.remove(chunk, local_key);
        }

        if let Some(key) = new_key {
            self.insert_new_block_entity(chunk, local_key, change.pos, &key);
        }
    }

    fn insert_new_block_entity(
        &mut self,
        chunk: ChunkCoord,
        local_key: LocalKey,
        world_pos: IVec3,
        key: &NamespacedKey,
    ) {
        let Some(type_id) = self.registry.get_id(key) else {
            return;
        };
        let Some(ty) = self.registry.get_type(type_id) else {
            return;
        };
        let entity = (ty.create)(world_pos);
        debug_assert_eq!(entity.type_key(), &ty.key);
        self.store.insert(chunk, local_key, entity);
    }
}

impl SimSystem for BlockEntitiesSystem {
    fn name(&self) -> &'static str {
        "block_entities"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn on_columns_entered(
        &mut self,
        _world: &mut World,
        _cols: &[ColumnCoord],
        _active: ActiveSimColumns<'_>,
    ) {
    }

    fn on_columns_exited(
        &mut self,
        _world: &mut World,
        _cols: &[ColumnCoord],
        _active: ActiveSimColumns<'_>,
    ) {
    }

    fn fixed_tick(
        &mut self,
        world: &mut World,
        tick_dt: f32,
        tick_index: u64,
        active: ActiveSimColumns<'_>,
    ) {
        self.process_world_block_changes(world);
        self.store
            .retain_loaded_chunks(|coord| world.chunks.contains_key(&coord));
        self.active_chunk_count = self
            .store
            .collect_tick_targets_in_active_columns(active.is_active, &mut self.tick_targets);

        for (chunk, local_key) in self.tick_targets.iter().copied() {
            let Some(entity) = self.store.get_mut(chunk, local_key) else {
                continue;
            };
            entity.tick(world, tick_dt, tick_index);
        }
    }
}
