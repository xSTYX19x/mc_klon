use std::collections::HashMap;

use glam::IVec3;

use crate::registry::NamespacedKey;

use super::World;

pub(crate) type BlockEntityTypeId = u32;

pub(crate) struct BlockEntityType {
    pub(crate) key: NamespacedKey,
    pub(crate) create: fn(pos: IVec3) -> Box<dyn BlockEntity>,
}

pub(crate) trait BlockEntity {
    fn type_key(&self) -> &NamespacedKey;
    fn tick(&mut self, world: &mut World, tick_dt: f32, tick_index: u64);
}

pub(crate) struct BlockEntityRegistry {
    by_key: HashMap<NamespacedKey, BlockEntityTypeId>,
    types: Vec<BlockEntityType>,
}

impl BlockEntityRegistry {
    pub(crate) fn new() -> Self {
        Self {
            by_key: HashMap::new(),
            types: Vec::new(),
        }
    }

    pub(crate) fn register(
        &mut self,
        key: NamespacedKey,
        create: fn(pos: IVec3) -> Box<dyn BlockEntity>,
    ) -> BlockEntityTypeId {
        if let Some(existing) = self.by_key.get(&key).copied() {
            return existing;
        }

        let id = self.types.len() as BlockEntityTypeId;
        self.by_key.insert(key.clone(), id);
        self.types.push(BlockEntityType { key, create });
        id
    }

    pub(crate) fn get_id(&self, key: &NamespacedKey) -> Option<BlockEntityTypeId> {
        self.by_key.get(key).copied()
    }

    pub(crate) fn get_type(&self, id: BlockEntityTypeId) -> Option<&BlockEntityType> {
        self.types.get(id as usize)
    }
}

const FURNACE_STUB_KEY_STR: &str = "core:furnace_stub";

struct FurnaceStubBlockEntity {
    type_key: NamespacedKey,
    _pos: IVec3,
    tick_counter: u64,
}

impl FurnaceStubBlockEntity {
    fn new(pos: IVec3) -> Self {
        Self {
            type_key: NamespacedKey::parse(FURNACE_STUB_KEY_STR)
                .expect("invalid hardcoded block entity key"),
            _pos: pos,
            tick_counter: 0,
        }
    }
}

impl BlockEntity for FurnaceStubBlockEntity {
    fn type_key(&self) -> &NamespacedKey {
        &self.type_key
    }

    fn tick(&mut self, _world: &mut World, _tick_dt: f32, _tick_index: u64) {
        self.tick_counter = self.tick_counter.wrapping_add(1);
    }
}

fn create_furnace_stub(pos: IVec3) -> Box<dyn BlockEntity> {
    Box::new(FurnaceStubBlockEntity::new(pos))
}

pub(crate) fn register_core_block_entity_types(registry: &mut BlockEntityRegistry) {
    let key = NamespacedKey::parse(FURNACE_STUB_KEY_STR).expect("invalid hardcoded BE key");
    registry.register(key, create_furnace_stub);
}
