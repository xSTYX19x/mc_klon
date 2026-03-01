use std::collections::{HashMap, HashSet};
use std::fmt;

use glam::IVec3;

use crate::registry::NamespacedKey;
use crate::world::World;
use crate::world::fluids::FluidKind;

pub(crate) const TEX_STONE: &str = "minecraft:textures/block/stone.png";
pub(crate) const TEX_COBBLESTONE: &str = "minecraft:textures/block/cobblestone.png";
pub(crate) const TEX_OBSIDIAN: &str = "minecraft:textures/block/obsidian.png";
pub(crate) const TEX_GRASS_TOP: &str = "minecraft:textures/block/grass_block_top.png";
pub(crate) const TEX_GRASS_SIDE: &str = "minecraft:textures/block/grass_block_side.png";
pub(crate) const TEX_DIRT: &str = "minecraft:textures/block/dirt.png";
pub(crate) const TEX_SAPLING: &str = "minecraft:textures/block/oak_sapling.png";
pub(crate) const TEX_OAK_PLANKS: &str = "minecraft:textures/block/oak_planks.png";
pub(crate) const TEX_BEDROCK: &str = "minecraft:textures/block/bedrock.png";
pub(crate) const TEX_WATER_STILL: &str = "minecraft:textures/block/water_still.png";
pub(crate) const TEX_WATER_FLOW: &str = "minecraft:textures/block/water_flow.png";
pub(crate) const TEX_LAVA_STILL: &str = "minecraft:textures/block/lava_still.png";
pub(crate) const TEX_LAVA_FLOW: &str = "minecraft:textures/block/lava_flow.png";
pub(crate) const TEX_WATER_OVERLAY: &str = "minecraft:textures/block/water_overlay.png";
pub(crate) const TEX_SAND: &str = "minecraft:textures/block/sand.png";
pub(crate) const CORE_FURNACE_STUB_BLOCK_ENTITY_KEY: &str = "core:furnace_stub";
pub(crate) const CORE_MISSING_BLOCK_KEY: &str = "core:missing";

pub(crate) const REQUIRED_FACE_TEXTURE_KEYS: [&str; 13] = [
    TEX_GRASS_TOP,
    TEX_GRASS_SIDE,
    TEX_DIRT,
    TEX_COBBLESTONE,
    TEX_OBSIDIAN,
    TEX_OAK_PLANKS,
    TEX_SAPLING,
    TEX_BEDROCK,
    TEX_WATER_STILL,
    TEX_WATER_FLOW,
    TEX_LAVA_STILL,
    TEX_LAVA_FLOW,
    TEX_SAND,
];
pub(crate) const REQUIRED_OVERLAY_TEXTURE_KEYS: [&str; 1] = [TEX_WATER_OVERLAY];
pub(crate) const TEXTURE_SELF_TEST_KEYS: [&str; 3] = [
    "minecraft:textures/block/oak_log.png",
    "minecraft:textures/item/apple.png",
    "minecraft:textures/block/torch.png",
];

pub(crate) const TINT_NONE: u32 = 0;
pub(crate) const TINT_GRASS_TOP: u32 = 1;
pub(crate) const TINT_WATER: u32 = 2;

pub(crate) type BlockStateId = BlockId;
pub(crate) type RandomTickFn = fn(&mut World, IVec3, BlockStateId, u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub(crate) enum BlockId {
    Air = 0,
    Grass = 1,
    Stone = 2,
    Dirt = 3,
    Sapling = 4,
    OakPlanks = 5,
    Bedrock = 6,
    WaterStill = 7,
    WaterFlowing = 8,
    LavaStill = 9,
    LavaFlowing = 10,
    Cobblestone = 11,
    Obsidian = 12,
    Sand = 13,
}

pub(crate) const HOTBAR: [BlockId; 9] = [
    BlockId::Stone,
    BlockId::Dirt,
    BlockId::Grass,
    BlockId::OakPlanks,
    BlockId::WaterStill,
    BlockId::Sapling,
    BlockId::WaterFlowing,
    BlockId::LavaStill,
    BlockId::LavaFlowing,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Face {
    Up,
    Down,
    North,
    South,
    East,
    West,
}

pub(crate) type TextureKey = &'static str;

impl Face {
    #[inline(always)]
    pub(crate) const fn as_usize(self) -> usize {
        match self {
            Face::Up => 0,
            Face::Down => 1,
            Face::North => 2,
            Face::South => 3,
            Face::East => 4,
            Face::West => 5,
        }
    }

    #[inline(always)]
    pub(crate) const fn normal_i32(self) -> [i32; 3] {
        match self {
            Face::Up => [0, 1, 0],
            Face::Down => [0, -1, 0],
            Face::North => [0, 0, -1],
            Face::South => [0, 0, 1],
            Face::East => [1, 0, 0],
            Face::West => [-1, 0, 0],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockModel {
    Cube,
    CrossPlant,
    Liquid { kind: FluidKind },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RenderLayer {
    Opaque,
    Cutout,
    Transparent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CullGroup {
    None,
    Block(BlockId),
    Water,
    Lava,
}

#[derive(Clone, Copy, Debug)]
struct BlockDef {
    model: BlockModel,
    textures: [TextureKey; 6],
    face_tint_kinds: [u32; 6],
    plant_texture: Option<TextureKey>,
    block_entity: Option<&'static str>,
    random_tick: Option<RandomTickFn>,
    solid: bool,
    breakable: bool,
    face_occluding: bool,
    render_layer: RenderLayer,
    cull_group: CullGroup,
}

impl BlockDef {
    const fn cube_all(
        key: TextureKey,
        solid: bool,
        breakable: bool,
        face_occluding: bool,
        render_layer: RenderLayer,
        cull_group: CullGroup,
    ) -> Self {
        Self {
            model: BlockModel::Cube,
            textures: [key; 6],
            face_tint_kinds: [TINT_NONE; 6],
            plant_texture: None,
            block_entity: None,
            random_tick: None,
            solid,
            breakable,
            face_occluding,
            render_layer,
            cull_group,
        }
    }

    const fn cube_top_side_bottom(
        top: TextureKey,
        side: TextureKey,
        bottom: TextureKey,
        solid: bool,
        breakable: bool,
        face_occluding: bool,
        render_layer: RenderLayer,
        cull_group: CullGroup,
    ) -> Self {
        let mut textures = [side; 6];
        textures[Face::Up.as_usize()] = top;
        textures[Face::Down.as_usize()] = bottom;
        Self {
            model: BlockModel::Cube,
            textures,
            face_tint_kinds: [TINT_NONE; 6],
            plant_texture: None,
            block_entity: None,
            random_tick: None,
            solid,
            breakable,
            face_occluding,
            render_layer,
            cull_group,
        }
    }

    const fn grass(
        top: TextureKey,
        side: TextureKey,
        bottom: TextureKey,
        solid: bool,
        breakable: bool,
        face_occluding: bool,
        render_layer: RenderLayer,
        cull_group: CullGroup,
    ) -> Self {
        let mut block = Self::cube_top_side_bottom(
            top,
            side,
            bottom,
            solid,
            breakable,
            face_occluding,
            render_layer,
            cull_group,
        );
        block.face_tint_kinds[Face::Up.as_usize()] = TINT_GRASS_TOP;
        block
    }

    const fn plant_cross(key: TextureKey) -> Self {
        Self {
            model: BlockModel::CrossPlant,
            textures: [key; 6],
            face_tint_kinds: [TINT_NONE; 6],
            plant_texture: Some(key),
            block_entity: None,
            random_tick: None,
            solid: false,
            breakable: true,
            face_occluding: false,
            render_layer: RenderLayer::Cutout,
            cull_group: CullGroup::None,
        }
    }

    const fn liquid_top_side_bottom(
        kind: FluidKind,
        top: TextureKey,
        side: TextureKey,
        bottom: TextureKey,
        breakable: bool,
        render_layer: RenderLayer,
        cull_group: CullGroup,
    ) -> Self {
        let mut textures = [side; 6];
        textures[Face::Up.as_usize()] = top;
        textures[Face::Down.as_usize()] = bottom;
        Self {
            model: BlockModel::Liquid { kind },
            textures,
            face_tint_kinds: [TINT_NONE; 6],
            plant_texture: None,
            block_entity: None,
            random_tick: None,
            solid: false,
            breakable,
            face_occluding: false,
            render_layer,
            cull_group,
        }
    }

    const fn with_block_entity(mut self, key: &'static str) -> Self {
        self.block_entity = Some(key);
        self
    }

    const fn with_random_tick(mut self, callback: RandomTickFn) -> Self {
        self.random_tick = Some(callback);
        self
    }

    const fn with_tint_all(mut self, tint_kind: u32) -> Self {
        self.face_tint_kinds = [tint_kind; 6];
        self
    }

    fn texture_for(&self, face: Face) -> TextureKey {
        match self.model {
            BlockModel::Cube | BlockModel::Liquid { .. } => self.textures[face.as_usize()],
            BlockModel::CrossPlant => self
                .plant_texture
                .expect("CrossPlant BlockDef missing plant texture"),
        }
    }

    fn tint_kind_for(&self, face: Face) -> u32 {
        match self.model {
            BlockModel::Cube | BlockModel::Liquid { .. } => self.face_tint_kinds[face.as_usize()],
            BlockModel::CrossPlant => TINT_NONE,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BlockTextureLayers {
    up: u32,
    down: u32,
    north: u32,
    south: u32,
    east: u32,
    west: u32,
}

impl BlockTextureLayers {
    pub(crate) fn layer_for_face(self, face: Face) -> u32 {
        match face {
            Face::Up => self.up,
            Face::Down => self.down,
            Face::North => self.north,
            Face::South => self.south,
            Face::East => self.east,
            Face::West => self.west,
        }
    }
}

/// BlockRegistry:
/// - zentraler Ort für Blockdefinitionen
/// - jeder Block bekommt Texturen pro Face
pub(crate) struct BlockRegistry {
    defs: HashMap<BlockId, BlockDef>,
}

impl BlockRegistry {
    pub(crate) fn new() -> Self {
        let mut defs = HashMap::new();

        // Stone: überall gleiche Textur.
        defs.insert(
            BlockId::Stone,
            BlockDef::cube_all(
                TEX_STONE,
                true,
                true,
                true,
                RenderLayer::Opaque,
                CullGroup::Block(BlockId::Stone),
            ),
        );
        defs.insert(
            BlockId::Dirt,
            BlockDef::cube_all(
                TEX_DIRT,
                true,
                true,
                true,
                RenderLayer::Opaque,
                CullGroup::Block(BlockId::Dirt),
            ),
        );
        defs.insert(
            BlockId::Cobblestone,
            BlockDef::cube_all(
                TEX_COBBLESTONE,
                true,
                true,
                true,
                RenderLayer::Opaque,
                CullGroup::Block(BlockId::Cobblestone),
            ),
        );
        defs.insert(
            BlockId::Obsidian,
            BlockDef::cube_all(
                TEX_OBSIDIAN,
                true,
                true,
                true,
                RenderLayer::Opaque,
                CullGroup::Block(BlockId::Obsidian),
            )
            .with_block_entity(CORE_FURNACE_STUB_BLOCK_ENTITY_KEY),
        );
        defs.insert(
            BlockId::Sand,
            BlockDef::cube_all(
                TEX_SAND,
                true,
                true,
                true,
                RenderLayer::Opaque,
                CullGroup::Block(BlockId::Sand),
            ),
        );

        // Grass: oben grass_top, unten dirt, Seiten grass_side.
        defs.insert(
            BlockId::Grass,
            BlockDef::grass(
                TEX_GRASS_TOP,
                TEX_GRASS_SIDE,
                TEX_DIRT,
                true,
                true,
                true,
                RenderLayer::Opaque,
                CullGroup::Block(BlockId::Grass),
            )
            .with_random_tick(grass_random_tick),
        );

        defs.insert(
            BlockId::OakPlanks,
            BlockDef::cube_all(
                TEX_OAK_PLANKS,
                true,
                true,
                true,
                RenderLayer::Opaque,
                CullGroup::Block(BlockId::OakPlanks),
            ),
        );

        defs.insert(BlockId::Sapling, BlockDef::plant_cross(TEX_SAPLING));
        defs.insert(
            BlockId::Bedrock,
            BlockDef::cube_all(
                TEX_BEDROCK,
                true,
                false,
                true,
                RenderLayer::Opaque,
                CullGroup::Block(BlockId::Bedrock),
            ),
        );

        defs.insert(
            BlockId::WaterStill,
            BlockDef::liquid_top_side_bottom(
                FluidKind::Water,
                TEX_WATER_STILL,
                TEX_WATER_STILL,
                TEX_WATER_STILL,
                true,
                RenderLayer::Transparent,
                CullGroup::Water,
            )
            .with_tint_all(TINT_WATER),
        );
        defs.insert(
            BlockId::WaterFlowing,
            BlockDef::liquid_top_side_bottom(
                FluidKind::Water,
                TEX_WATER_STILL,
                TEX_WATER_FLOW,
                TEX_WATER_STILL,
                true,
                RenderLayer::Transparent,
                CullGroup::Water,
            )
            .with_tint_all(TINT_WATER),
        );
        defs.insert(
            BlockId::LavaStill,
            BlockDef::liquid_top_side_bottom(
                FluidKind::Lava,
                TEX_LAVA_STILL,
                TEX_LAVA_STILL,
                TEX_LAVA_STILL,
                false,
                RenderLayer::Opaque,
                CullGroup::Lava,
            ),
        );
        defs.insert(
            BlockId::LavaFlowing,
            BlockDef::liquid_top_side_bottom(
                FluidKind::Lava,
                TEX_LAVA_STILL,
                TEX_LAVA_FLOW,
                TEX_LAVA_STILL,
                false,
                RenderLayer::Opaque,
                CullGroup::Lava,
            ),
        );

        Self { defs }
    }

    fn def(&self, block: BlockId) -> BlockDef {
        *self
            .defs
            .get(&block)
            .unwrap_or_else(|| panic!("missing BlockDef registration for {:?}", block))
    }

    pub(crate) fn texture_key_for(&self, block: BlockId, face: Face) -> TextureKey {
        self.def(block).texture_for(face)
    }

    pub(crate) fn model_for(&self, block: BlockId) -> BlockModel {
        self.def(block).model
    }

    pub(crate) fn solid_for(&self, block: BlockId) -> bool {
        self.def(block).solid
    }

    pub(crate) fn breakable_for(&self, block: BlockId) -> bool {
        self.def(block).breakable
    }

    pub(crate) fn block_entity_key_for(&self, block: BlockId) -> Option<&'static str> {
        if block == BlockId::Air {
            return None;
        }
        self.def(block).block_entity
    }

    pub(crate) fn random_tick_for(&self, block: BlockId) -> Option<RandomTickFn> {
        if block == BlockId::Air {
            return None;
        }
        self.def(block).random_tick
    }

    pub(crate) fn has_random_tick(&self, block: BlockId) -> bool {
        self.random_tick_for(block).is_some()
    }

    pub(crate) fn registered_block_keys(&self) -> Vec<NamespacedKey> {
        let mut keys: Vec<NamespacedKey> =
            self.defs.keys().copied().map(block_key_for_block).collect();
        if let Some(missing) = NamespacedKey::parse(CORE_MISSING_BLOCK_KEY) {
            keys.push(missing);
        }
        keys.sort_by(|a, b| {
            a.namespace
                .cmp(&b.namespace)
                .then_with(|| a.name.cmp(&b.name))
        });
        keys.dedup();
        keys
    }

    pub(crate) fn block_for_key(&self, key: &NamespacedKey) -> Option<BlockId> {
        block_from_key(key)
    }

    pub(crate) fn name_of(&self, block: BlockId) -> &'static str {
        block_key_str_for_block(block)
    }

    pub(crate) fn resolve(&self, name: &str) -> Option<BlockId> {
        let key = NamespacedKey::parse(name)?;
        self.block_for_key(&key)
    }

    pub(crate) fn collect_texture_keys(&self) -> Vec<String> {
        let mut keys: HashSet<String> = HashSet::new();
        for def in self.defs.values() {
            match def.model {
                BlockModel::Cube | BlockModel::Liquid { .. } => {
                    for key in def.textures {
                        keys.insert(key.to_string());
                    }
                }
                BlockModel::CrossPlant => {
                    if let Some(key) = def.plant_texture {
                        keys.insert(key.to_string());
                    }
                }
            }
        }
        let mut collected: Vec<String> = keys.into_iter().collect();
        collected.sort();
        collected
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BlockTextureInfo {
    model: BlockModel,
    cube_layers: BlockTextureLayers,
    face_tint_kinds: [u32; 6],
    plant_layer: Option<u32>,
    solid: bool,
    breakable: bool,
    face_occluding: bool,
    render_layer: RenderLayer,
    cull_group: CullGroup,
}

pub(crate) struct BlockTextureLookup {
    by_block: HashMap<BlockId, BlockTextureInfo>,
    missing_layer: u32,
}

impl BlockTextureLookup {
    pub(crate) fn new(
        block_registry: &BlockRegistry,
        key_to_layer: &HashMap<String, u32>,
        missing_layer: u32,
    ) -> Self {
        let mut warned_missing_keys: HashSet<&'static str> = HashSet::new();

        let mut by_block = HashMap::new();
        for (&block, &textures) in &block_registry.defs {
            let mut resolve = |key: &'static str| -> u32 {
                if let Some(layer) = key_to_layer.get(key).copied() {
                    layer
                } else {
                    if warned_missing_keys.insert(key) {
                        eprintln!(
                            "[warn] missing texture key '{}' in atlas mapping; using missing-texture layer",
                            key
                        );
                    }
                    missing_layer
                }
            };
            let cube_layers = BlockTextureLayers {
                up: resolve(textures.texture_for(Face::Up)),
                down: resolve(textures.texture_for(Face::Down)),
                north: resolve(textures.texture_for(Face::North)),
                south: resolve(textures.texture_for(Face::South)),
                east: resolve(textures.texture_for(Face::East)),
                west: resolve(textures.texture_for(Face::West)),
            };
            let plant_layer = textures.plant_texture.map(resolve);
            by_block.insert(
                block,
                BlockTextureInfo {
                    model: textures.model,
                    cube_layers,
                    face_tint_kinds: [
                        textures.tint_kind_for(Face::Up),
                        textures.tint_kind_for(Face::Down),
                        textures.tint_kind_for(Face::North),
                        textures.tint_kind_for(Face::South),
                        textures.tint_kind_for(Face::East),
                        textures.tint_kind_for(Face::West),
                    ],
                    plant_layer,
                    solid: textures.solid,
                    breakable: textures.breakable,
                    face_occluding: textures.face_occluding,
                    render_layer: textures.render_layer,
                    cull_group: textures.cull_group,
                },
            );
        }

        Self {
            by_block,
            missing_layer,
        }
    }

    pub(crate) fn layer_for(&self, block: BlockId, face: Face) -> u32 {
        self.by_block
            .get(&block)
            .copied()
            .map_or(self.missing_layer, |info| match info.model {
                BlockModel::Cube | BlockModel::Liquid { .. } => {
                    info.cube_layers.layer_for_face(face)
                }
                BlockModel::CrossPlant => info.plant_layer.unwrap_or(self.missing_layer),
            })
    }

    pub(crate) fn tint_kind_for(&self, block: BlockId, face: Face) -> u32 {
        self.by_block
            .get(&block)
            .map_or(TINT_NONE, |info| match info.model {
                BlockModel::Cube | BlockModel::Liquid { .. } => {
                    info.face_tint_kinds[face.as_usize()]
                }
                BlockModel::CrossPlant => TINT_NONE,
            })
    }

    pub(crate) fn model_for(&self, block: BlockId) -> BlockModel {
        self.by_block
            .get(&block)
            .map_or(BlockModel::Cube, |info| info.model)
    }

    pub(crate) fn is_solid_for(&self, block: BlockId) -> bool {
        self.by_block.get(&block).is_some_and(|info| info.solid)
    }

    pub(crate) fn is_breakable_for(&self, block: BlockId) -> bool {
        self.by_block.get(&block).is_some_and(|info| info.breakable)
    }

    pub(crate) fn render_layer_for(&self, block: BlockId) -> RenderLayer {
        self.by_block
            .get(&block)
            .map_or(RenderLayer::Opaque, |info| info.render_layer)
    }

    pub(crate) fn face_occluding_for(&self, block: BlockId) -> bool {
        self.by_block
            .get(&block)
            .is_some_and(|info| info.face_occluding)
    }

    pub(crate) fn cull_group_for(&self, block: BlockId) -> CullGroup {
        self.by_block
            .get(&block)
            .map_or(CullGroup::None, |info| info.cull_group)
    }

    pub(crate) fn plant_layer_for(&self, block: BlockId) -> Option<u32> {
        self.by_block.get(&block).and_then(|info| info.plant_layer)
    }
}

impl BlockId {
    /// Physische Solidität (Collision).
    pub(crate) fn is_solid(self) -> bool {
        matches!(
            self,
            BlockId::Grass
                | BlockId::Stone
                | BlockId::Dirt
                | BlockId::Cobblestone
                | BlockId::Obsidian
                | BlockId::OakPlanks
                | BlockId::Bedrock
                | BlockId::Sand
        )
    }

    /// Ob der Block als Voxel-Zelle vorhanden ist (Meshing/Raycast).
    #[inline(always)]
    pub(crate) fn is_voxel_filled(self) -> bool {
        self != BlockId::Air
    }

    #[inline]
    pub(crate) fn blocks_skylight(self) -> bool {
        matches!(
            self,
            BlockId::Grass
                | BlockId::Stone
                | BlockId::Dirt
                | BlockId::Cobblestone
                | BlockId::Obsidian
                | BlockId::OakPlanks
                | BlockId::Bedrock
                | BlockId::Sand
        )
    }

    pub(crate) fn emitted_light(self) -> u8 {
        match self {
            BlockId::LavaStill | BlockId::LavaFlowing => 15,
            _ => 0,
        }
    }

    pub(crate) fn color(self) -> [f32; 3] {
        match self {
            BlockId::Air => [0.0, 0.0, 0.0], // wird nicht gerendert
            BlockId::Grass => [0.35, 0.75, 0.25],
            BlockId::Stone => [0.55, 0.55, 0.58],
            BlockId::Dirt => [0.49, 0.34, 0.20],
            BlockId::Cobblestone => [0.48, 0.48, 0.50],
            BlockId::Obsidian => [0.16, 0.10, 0.22],
            BlockId::Sapling => [0.28, 0.88, 0.36],
            BlockId::OakPlanks => [0.68, 0.56, 0.34],
            BlockId::Bedrock => [0.32, 0.32, 0.34],
            BlockId::Sand => [0.85, 0.80, 0.55],
            BlockId::WaterStill | BlockId::WaterFlowing => [0.26, 0.48, 0.95],
            BlockId::LavaStill | BlockId::LavaFlowing => [0.94, 0.44, 0.10],
        }
    }

    pub(crate) fn to_u8(self) -> u8 {
        self as u8
    }

    pub(crate) fn from_u8(value: u8) -> Result<Self, InvalidBlockId> {
        match value {
            0 => Ok(BlockId::Air),
            1 => Ok(BlockId::Grass),
            2 => Ok(BlockId::Stone),
            3 => Ok(BlockId::Dirt),
            4 => Ok(BlockId::Sapling),
            5 => Ok(BlockId::OakPlanks),
            6 => Ok(BlockId::Bedrock),
            7 => Ok(BlockId::WaterStill),
            8 => Ok(BlockId::WaterFlowing),
            9 => Ok(BlockId::LavaStill),
            10 => Ok(BlockId::LavaFlowing),
            11 => Ok(BlockId::Cobblestone),
            12 => Ok(BlockId::Obsidian),
            13 => Ok(BlockId::Sand),
            _ => Err(InvalidBlockId { raw: value }),
        }
    }

    pub(crate) fn from_legacy_v13_u8(value: u8) -> Result<Self, InvalidBlockId> {
        match value {
            0 => Ok(BlockId::Air),
            1 => Ok(BlockId::Grass),
            2 => Ok(BlockId::Stone),
            3 => Ok(BlockId::Sapling),
            4 => Ok(BlockId::Dirt),
            5 => Ok(BlockId::OakPlanks),
            _ => Err(InvalidBlockId { raw: value }),
        }
    }

    pub(crate) fn display_name(self) -> &'static str {
        match self {
            BlockId::Air => "Air",
            BlockId::Grass => "Grass",
            BlockId::Stone => "Stone",
            BlockId::Dirt => "Dirt",
            BlockId::Cobblestone => "Cobblestone",
            BlockId::Obsidian => "Obsidian",
            BlockId::Sapling => "Sapling",
            BlockId::OakPlanks => "Oak Planks",
            BlockId::Bedrock => "Bedrock",
            BlockId::Sand => "Sand",
            BlockId::WaterStill => "Water (Still)",
            BlockId::WaterFlowing => "Water (Flowing)",
            BlockId::LavaStill => "Lava (Still)",
            BlockId::LavaFlowing => "Lava (Flowing)",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InvalidBlockId {
    pub(crate) raw: u8,
}

impl fmt::Display for InvalidBlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown block id {}", self.raw)
    }
}

impl std::error::Error for InvalidBlockId {}

impl Default for BlockId {
    fn default() -> Self {
        BlockId::Stone
    }
}

fn grass_random_tick(world: &mut World, pos: IVec3, _state: BlockStateId, tick_index: u64) {
    // Keep it cheap: probe a single horizontal neighbor and spread grass to dirt if exposed.
    let dirs = [
        IVec3::new(1, 0, 0),
        IVec3::new(-1, 0, 0),
        IVec3::new(0, 0, 1),
        IVec3::new(0, 0, -1),
    ];

    let mut mix = tick_index;
    mix ^= (pos.x as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    mix ^= (pos.y as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    mix ^= (pos.z as i64 as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
    let dir = dirs[(mix as usize) & 3];

    let target = pos + dir;
    let above_target = target + IVec3::Y;
    if !World::in_bounds(target) || !World::in_bounds(above_target) {
        return;
    }

    // Avoid loading/creating new chunks from random ticks.
    if world
        .get_chunk(World::world_to_chunk_coord(target))
        .is_none()
        || world
            .get_chunk(World::world_to_chunk_coord(above_target))
            .is_none()
    {
        return;
    }

    if world.get_block(target) == BlockId::Dirt && world.get_block(above_target) == BlockId::Air {
        let _ = world.set_block(target, BlockId::Grass);
    }
}

pub(crate) fn block_entity_key_str_for_block(block: BlockId) -> Option<&'static str> {
    match block {
        BlockId::Obsidian => Some(CORE_FURNACE_STUB_BLOCK_ENTITY_KEY),
        _ => None,
    }
}

pub(crate) fn block_entity_key_for_block(block: BlockId) -> Option<NamespacedKey> {
    block_entity_key_str_for_block(block).and_then(NamespacedKey::parse)
}

pub(crate) fn block_has_block_entity(block: BlockId) -> bool {
    block_entity_key_str_for_block(block).is_some()
}

pub(crate) fn block_key_str_for_block(block: BlockId) -> &'static str {
    match block {
        BlockId::Air => "minecraft:air",
        BlockId::Grass => "minecraft:grass_block",
        BlockId::Stone => "minecraft:stone",
        BlockId::Dirt => "minecraft:dirt",
        BlockId::Sapling => "minecraft:oak_sapling",
        BlockId::OakPlanks => "minecraft:oak_planks",
        BlockId::Bedrock => "minecraft:bedrock",
        BlockId::WaterStill => "minecraft:water",
        BlockId::WaterFlowing => "minecraft:flowing_water",
        BlockId::LavaStill => "minecraft:lava",
        BlockId::LavaFlowing => "minecraft:flowing_lava",
        BlockId::Cobblestone => "minecraft:cobblestone",
        BlockId::Obsidian => "minecraft:obsidian",
        BlockId::Sand => "minecraft:sand",
    }
}

pub(crate) fn block_key_for_block(block: BlockId) -> NamespacedKey {
    NamespacedKey::parse(block_key_str_for_block(block))
        .expect("invalid hardcoded runtime block key")
}

pub(crate) fn block_from_key(key: &NamespacedKey) -> Option<BlockId> {
    match (key.namespace.as_str(), key.name.as_str()) {
        ("minecraft", "air") | ("core", "air") => Some(BlockId::Air),
        ("minecraft", "grass_block") | ("core", "grass") => Some(BlockId::Grass),
        ("minecraft", "stone") | ("core", "stone") => Some(BlockId::Stone),
        ("minecraft", "dirt") | ("core", "dirt") => Some(BlockId::Dirt),
        ("minecraft", "oak_sapling") | ("core", "sapling") => Some(BlockId::Sapling),
        ("minecraft", "oak_planks") | ("core", "oak_planks") => Some(BlockId::OakPlanks),
        ("minecraft", "bedrock") | ("core", "bedrock") => Some(BlockId::Bedrock),
        ("minecraft", "water") | ("core", "water_still") => Some(BlockId::WaterStill),
        ("minecraft", "flowing_water") | ("core", "water_flowing") => Some(BlockId::WaterFlowing),
        ("minecraft", "lava") | ("core", "lava_still") => Some(BlockId::LavaStill),
        ("minecraft", "flowing_lava") | ("core", "lava_flowing") => Some(BlockId::LavaFlowing),
        ("minecraft", "cobblestone") | ("core", "cobblestone") => Some(BlockId::Cobblestone),
        ("minecraft", "obsidian") | ("core", "obsidian") => Some(BlockId::Obsidian),
        ("minecraft", "sand") | ("core", "sand") => Some(BlockId::Sand),
        ("core", "missing") | ("minecraft", "missing") => Some(BlockId::Air),
        _ => None,
    }
}
