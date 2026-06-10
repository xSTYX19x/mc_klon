use std::collections::{HashMap, HashSet};
use std::fmt;

use glam::{IVec3, Vec3};

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
pub(crate) const TEX_BRICKS: &str = "minecraft:textures/block/bricks.png";
pub(crate) const TEX_OAK_LOG_SIDE: &str = "minecraft:textures/block/oak_log.png";
pub(crate) const TEX_OAK_LOG_TOP: &str = "minecraft:textures/block/oak_log_top.png";
pub(crate) const TEX_OAK_LEAVES: &str = "minecraft:textures/block/oak_leaves.png";
pub(crate) const TEX_GLASS: &str = "minecraft:textures/block/glass.png";
pub(crate) const TEX_ICE: &str = "minecraft:textures/block/ice.png";
pub(crate) const TEX_SNOW: &str = "minecraft:textures/block/snow.png";
pub(crate) const TEX_STONE_SLAB_TOP: &str = "minecraft:textures/block/smooth_stone.png";
pub(crate) const TEX_STONE_SLAB_SIDE: &str = "minecraft:textures/block/smooth_stone_slab_side.png";
pub(crate) const TEX_MOSSY_COBBLESTONE: &str = "minecraft:textures/block/mossy_cobblestone.png";
pub(crate) const TEX_GRAVEL: &str = "minecraft:textures/block/gravel.png";
pub(crate) const TEX_SPONGE: &str = "minecraft:textures/block/sponge.png";
pub(crate) const TEX_DANDELION: &str = "minecraft:textures/block/dandelion.png";
pub(crate) const TEX_POPPY: &str = "minecraft:textures/block/poppy.png";
pub(crate) const TEX_BROWN_MUSHROOM: &str = "minecraft:textures/block/brown_mushroom.png";
pub(crate) const TEX_RED_MUSHROOM: &str = "minecraft:textures/block/red_mushroom.png";
pub(crate) const TEX_COAL_ORE: &str = "minecraft:textures/block/coal_ore.png";
pub(crate) const TEX_IRON_ORE: &str = "minecraft:textures/block/iron_ore.png";
pub(crate) const TEX_GOLD_ORE: &str = "minecraft:textures/block/gold_ore.png";
pub(crate) const TEX_IRON_BLOCK: &str = "minecraft:textures/block/iron_block.png";
pub(crate) const TEX_GOLD_BLOCK: &str = "minecraft:textures/block/gold_block.png";
pub(crate) const TEX_BOOKSHELF_SIDE: &str = "minecraft:textures/block/bookshelf.png";
pub(crate) const TEX_TNT_TOP: &str = "minecraft:textures/block/tnt_top.png";
pub(crate) const TEX_TNT_SIDE: &str = "minecraft:textures/block/tnt_side.png";
pub(crate) const TEX_TNT_BOTTOM: &str = "minecraft:textures/block/tnt_bottom.png";
pub(crate) const TEX_WHITE_WOOL: &str = "minecraft:textures/block/white_wool.png";
pub(crate) const TEX_ORANGE_WOOL: &str = "minecraft:textures/block/orange_wool.png";
pub(crate) const TEX_MAGENTA_WOOL: &str = "minecraft:textures/block/magenta_wool.png";
pub(crate) const TEX_LIGHT_BLUE_WOOL: &str = "minecraft:textures/block/light_blue_wool.png";
pub(crate) const TEX_YELLOW_WOOL: &str = "minecraft:textures/block/yellow_wool.png";
pub(crate) const TEX_LIME_WOOL: &str = "minecraft:textures/block/lime_wool.png";
pub(crate) const TEX_PINK_WOOL: &str = "minecraft:textures/block/pink_wool.png";
pub(crate) const TEX_GRAY_WOOL: &str = "minecraft:textures/block/gray_wool.png";
pub(crate) const TEX_LIGHT_GRAY_WOOL: &str = "minecraft:textures/block/light_gray_wool.png";
pub(crate) const TEX_CYAN_WOOL: &str = "minecraft:textures/block/cyan_wool.png";
pub(crate) const TEX_PURPLE_WOOL: &str = "minecraft:textures/block/purple_wool.png";
pub(crate) const TEX_BLUE_WOOL: &str = "minecraft:textures/block/blue_wool.png";
pub(crate) const TEX_BROWN_WOOL: &str = "minecraft:textures/block/brown_wool.png";
pub(crate) const TEX_GREEN_WOOL: &str = "minecraft:textures/block/green_wool.png";
pub(crate) const TEX_RED_WOOL: &str = "minecraft:textures/block/red_wool.png";
pub(crate) const TEX_BLACK_WOOL: &str = "minecraft:textures/block/black_wool.png";
pub(crate) const CORE_FURNACE_STUB_BLOCK_ENTITY_KEY: &str = "core:furnace_stub";
pub(crate) const CORE_MISSING_BLOCK_KEY: &str = "core:missing";

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
    Bricks = 14,
    OakLog = 15,
    OakLeaves = 16,
    Glass = 17,
    StoneSlab = 18,
    MossyCobblestone = 19,
    Gravel = 20,
    Sponge = 21,
    Dandelion = 22,
    Poppy = 23,
    BrownMushroom = 24,
    RedMushroom = 25,
    CoalOre = 26,
    IronOre = 27,
    GoldOre = 28,
    IronBlock = 29,
    GoldBlock = 30,
    Bookshelf = 31,
    Tnt = 32,
    WhiteWool = 33,
    OrangeWool = 34,
    MagentaWool = 35,
    LightBlueWool = 36,
    YellowWool = 37,
    LimeWool = 38,
    PinkWool = 39,
    GrayWool = 40,
    LightGrayWool = 41,
    CyanWool = 42,
    PurpleWool = 43,
    BlueWool = 44,
    BrownWool = 45,
    GreenWool = 46,
    RedWool = 47,
    BlackWool = 48,
    Ice = 49,
    Snow = 50,
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

pub(crate) const CLASSIC_PALETTE_ORDER: [BlockId; 43] = [
    // Row 1
    BlockId::Stone,
    BlockId::Cobblestone,
    BlockId::Bricks,
    BlockId::Dirt,
    BlockId::Grass,
    BlockId::OakPlanks,
    BlockId::OakLog,
    BlockId::OakLeaves,
    BlockId::Glass,
    BlockId::StoneSlab,
    // Row 2
    BlockId::MossyCobblestone,
    BlockId::Sapling,
    BlockId::Dandelion,
    BlockId::Poppy,
    BlockId::BrownMushroom,
    BlockId::RedMushroom,
    BlockId::Sand,
    BlockId::Gravel,
    BlockId::Sponge,
    // Rows 3-4
    BlockId::WhiteWool,
    BlockId::OrangeWool,
    BlockId::MagentaWool,
    BlockId::LightBlueWool,
    BlockId::YellowWool,
    BlockId::LimeWool,
    BlockId::PinkWool,
    BlockId::GrayWool,
    BlockId::LightGrayWool,
    BlockId::CyanWool,
    BlockId::PurpleWool,
    BlockId::BlueWool,
    BlockId::BrownWool,
    BlockId::GreenWool,
    BlockId::RedWool,
    BlockId::BlackWool,
    // Row 5
    BlockId::GoldOre,
    BlockId::IronOre,
    BlockId::CoalOre,
    BlockId::GoldBlock,
    BlockId::IronBlock,
    BlockId::Bookshelf,
    BlockId::Tnt,
    BlockId::Obsidian,
];

pub(crate) const CLASSIC_PALETTE: &[BlockId] = &CLASSIC_PALETTE_ORDER;

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

    #[inline(always)]
    pub(crate) const fn opposite(self) -> Self {
        match self {
            Face::Up => Face::Down,
            Face::Down => Face::Up,
            Face::North => Face::South,
            Face::South => Face::North,
            Face::East => Face::West,
            Face::West => Face::East,
        }
    }
}

/// Render shape only. This drives mesh generation and texture addressing, but not
/// collision, raycast/selection, or occlusion semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockModel {
    Cube,
    CrossPlant,
    SlabBottom,
    LayerBottom { height_px: u8 },
    Liquid { kind: FluidKind },
}

/// Physics/collision shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BlockAabb {
    min: [u8; 3],
    max: [u8; 3],
}

impl BlockAabb {
    pub(crate) const fn from_pixels(min: [u8; 3], max: [u8; 3]) -> Self {
        Self { min, max }
    }

    pub(crate) fn world_bounds(self, cell: IVec3) -> (Vec3, Vec3) {
        let base = Vec3::new(cell.x as f32, cell.y as f32, cell.z as f32);
        let min = base
            + Vec3::new(
                self.min[0] as f32 / 16.0,
                self.min[1] as f32 / 16.0,
                self.min[2] as f32 / 16.0,
            );
        let max = base
            + Vec3::new(
                self.max[0] as f32 / 16.0,
                self.max[1] as f32 / 16.0,
                self.max[2] as f32 / 16.0,
            );
        (min, max)
    }
}

const FULL_CUBE_AABB: BlockAabb = BlockAabb::from_pixels([0, 0, 0], [16, 16, 16]);
const LOWER_SLAB_AABB: BlockAabb = BlockAabb::from_pixels([0, 0, 0], [16, 8, 16]);
const SNOW_LAYER_AABB: BlockAabb = BlockAabb::from_pixels([0, 0, 0], [16, 2, 16]);
// Shared plant selection box: slightly inset from the block edges and one pixel shy of full height.
const CROSS_PLANT_SELECTION_AABB: BlockAabb = BlockAabb::from_pixels([1, 0, 1], [15, 15, 15]);

/// Physics/collision or selection/raycast shape. Collision and selection intentionally
/// stay separate at the metadata layer even though they reuse the same AABB primitive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CollisionShape {
    Empty,
    FullCube,
    SlabBottom,
    Aabb(BlockAabb),
}

impl CollisionShape {
    pub(crate) const fn local_bounds(self) -> Option<BlockAabb> {
        match self {
            CollisionShape::Empty => None,
            CollisionShape::FullCube => Some(FULL_CUBE_AABB),
            CollisionShape::SlabBottom => Some(LOWER_SLAB_AABB),
            CollisionShape::Aabb(bounds) => Some(bounds),
        }
    }

    pub(crate) fn world_bounds(self, cell: IVec3) -> Option<(Vec3, Vec3)> {
        self.local_bounds().map(|bounds| bounds.world_bounds(cell))
    }
}

/// Boundary coverage on a shared cell face. Used for deterministic face visibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FaceCoverage {
    None,
    LowerHalf,
    UpperHalf,
    Full,
}

impl FaceCoverage {
    #[inline(always)]
    pub(crate) const fn covers(self, requested: Self) -> bool {
        match requested {
            FaceCoverage::None => true,
            FaceCoverage::LowerHalf => {
                matches!(self, FaceCoverage::LowerHalf | FaceCoverage::Full)
            }
            FaceCoverage::UpperHalf => {
                matches!(self, FaceCoverage::UpperHalf | FaceCoverage::Full)
            }
            FaceCoverage::Full => matches!(self, FaceCoverage::Full),
        }
    }
}

/// Occlusion shape only. AO and face visibility operate on this contract instead of
/// generic solidity/skylight flags, so partial blocks can define precise behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OcclusionShape {
    Empty,
    FullCube,
    SlabLower,
    SlabUpper,
    CrossPlant,
    Decorative,
}

impl OcclusionShape {
    #[inline(always)]
    pub(crate) const fn boundary_coverage(self, face: Face) -> FaceCoverage {
        match self {
            OcclusionShape::Empty | OcclusionShape::CrossPlant | OcclusionShape::Decorative => {
                FaceCoverage::None
            }
            OcclusionShape::FullCube => FaceCoverage::Full,
            OcclusionShape::SlabLower => match face {
                Face::Down => FaceCoverage::Full,
                Face::Up => FaceCoverage::None,
                Face::North | Face::South | Face::East | Face::West => FaceCoverage::LowerHalf,
            },
            OcclusionShape::SlabUpper => match face {
                Face::Down => FaceCoverage::None,
                Face::Up => FaceCoverage::Full,
                Face::North | Face::South | Face::East | Face::West => FaceCoverage::UpperHalf,
            },
        }
    }

    #[inline]
    pub(crate) fn occludes_ao_sample(self, local_point: [f32; 3]) -> bool {
        match self {
            OcclusionShape::Empty | OcclusionShape::CrossPlant | OcclusionShape::Decorative => {
                false
            }
            OcclusionShape::FullCube => {
                point_in_aabb(local_point, [0.0, 0.0, 0.0], [1.0, 1.0, 1.0])
            }
            OcclusionShape::SlabLower => {
                point_in_aabb(local_point, [0.0, 0.0, 0.0], [1.0, 0.5, 1.0])
            }
            OcclusionShape::SlabUpper => {
                point_in_aabb(local_point, [0.0, 0.5, 0.0], [1.0, 1.0, 1.0])
            }
        }
    }
}

#[inline]
fn point_in_aabb(local_point: [f32; 3], min: [f32; 3], max: [f32; 3]) -> bool {
    const EPS: f32 = 1e-4;
    (min[0] - EPS..=max[0] + EPS).contains(&local_point[0])
        && (min[1] - EPS..=max[1] + EPS).contains(&local_point[1])
        && (min[2] - EPS..=max[2] + EPS).contains(&local_point[2])
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockModelSpec {
    None,
    CubeAll {
        all: TextureKey,
    },
    CubeTopSideBottom {
        top: TextureKey,
        side: TextureKey,
        bottom: TextureKey,
    },
    LayerBottomTopSideBottom {
        height_px: u8,
        top: TextureKey,
        side: TextureKey,
        bottom: TextureKey,
    },
    GrassTopSideBottom {
        top: TextureKey,
        side: TextureKey,
        bottom: TextureKey,
    },
    PlantCross {
        texture: TextureKey,
    },
    SlabBottomTopSideBottom {
        top: TextureKey,
        side: TextureKey,
        bottom: TextureKey,
    },
    LiquidTopSideBottom {
        kind: FluidKind,
        top: TextureKey,
        side: TextureKey,
        bottom: TextureKey,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FaceTintSpec {
    None,
    All(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CullGroupSpec {
    None,
    SelfBlock,
    Water,
    Lava,
}

impl CullGroupSpec {
    const fn resolve(self, block: BlockId) -> CullGroup {
        match self {
            CullGroupSpec::None => CullGroup::None,
            CullGroupSpec::SelfBlock => CullGroup::Block(block),
            CullGroupSpec::Water => CullGroup::Water,
            CullGroupSpec::Lava => CullGroup::Lava,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct BlockSpec {
    id: u8,
    variant: BlockId,
    key: &'static str,
    key_aliases: &'static [&'static str],
    display_name: &'static str,
    model: BlockModelSpec,
    render_layer: RenderLayer,
    cull_group: CullGroupSpec,
    face_occluding: bool,
    solid: bool,
    blocks_skylight: bool,
    breakable: bool,
    emitted_light: u8,
    particle_color: [f32; 3],
    face_tint: FaceTintSpec,
    block_entity: Option<&'static str>,
    random_tick: Option<RandomTickFn>,
}

impl BlockSpec {
    const fn collision_shape(self) -> CollisionShape {
        if !self.solid {
            return CollisionShape::Empty;
        }
        match self.model {
            BlockModelSpec::SlabBottomTopSideBottom { .. } => CollisionShape::SlabBottom,
            BlockModelSpec::LayerBottomTopSideBottom { height_px, .. } => {
                if height_px == 2 {
                    CollisionShape::Aabb(SNOW_LAYER_AABB)
                } else {
                    CollisionShape::Aabb(BlockAabb::from_pixels([0, 0, 0], [16, height_px, 16]))
                }
            }
            _ => CollisionShape::FullCube,
        }
    }

    const fn raycast_shape(self) -> CollisionShape {
        match self.model {
            BlockModelSpec::None => CollisionShape::Empty,
            BlockModelSpec::SlabBottomTopSideBottom { .. } => CollisionShape::SlabBottom,
            BlockModelSpec::LayerBottomTopSideBottom { height_px, .. } => {
                if height_px == 2 {
                    CollisionShape::Aabb(SNOW_LAYER_AABB)
                } else {
                    CollisionShape::Aabb(BlockAabb::from_pixels([0, 0, 0], [16, height_px, 16]))
                }
            }
            BlockModelSpec::PlantCross { .. } => CollisionShape::Aabb(CROSS_PLANT_SELECTION_AABB),
            _ => CollisionShape::FullCube,
        }
    }

    const fn occlusion_shape(self) -> OcclusionShape {
        if !self.face_occluding {
            return match self.model {
                BlockModelSpec::None => OcclusionShape::Empty,
                BlockModelSpec::PlantCross { .. } => OcclusionShape::CrossPlant,
                _ => OcclusionShape::Decorative,
            };
        }
        match self.model {
            BlockModelSpec::None => OcclusionShape::Empty,
            BlockModelSpec::SlabBottomTopSideBottom { .. } => OcclusionShape::SlabLower,
            _ => OcclusionShape::FullCube,
        }
    }

    fn block_def(self) -> Option<BlockDef> {
        let cull_group = self.cull_group.resolve(self.variant);
        let mut def = match self.model {
            BlockModelSpec::None => return None,
            BlockModelSpec::CubeAll { all } => BlockDef::cube_all(
                all,
                self.solid,
                self.breakable,
                self.face_occluding,
                self.render_layer,
                cull_group,
            ),
            BlockModelSpec::CubeTopSideBottom { top, side, bottom } => {
                BlockDef::cube_top_side_bottom(
                    top,
                    side,
                    bottom,
                    self.solid,
                    self.breakable,
                    self.face_occluding,
                    self.render_layer,
                    cull_group,
                )
            }
            BlockModelSpec::GrassTopSideBottom { top, side, bottom } => BlockDef::grass(
                top,
                side,
                bottom,
                self.solid,
                self.breakable,
                self.face_occluding,
                self.render_layer,
                cull_group,
            ),
            BlockModelSpec::PlantCross { texture } => BlockDef::plant_cross(texture),
            BlockModelSpec::SlabBottomTopSideBottom { top, side, bottom } => {
                BlockDef::slab_bottom_top_side_bottom(
                    top,
                    side,
                    bottom,
                    self.solid,
                    self.breakable,
                    self.face_occluding,
                    self.render_layer,
                    cull_group,
                )
            }
            BlockModelSpec::LayerBottomTopSideBottom {
                height_px,
                top,
                side,
                bottom,
            } => BlockDef::layer_bottom_top_side_bottom(
                height_px,
                top,
                side,
                bottom,
                self.solid,
                self.breakable,
                self.face_occluding,
                self.render_layer,
                cull_group,
            ),
            BlockModelSpec::LiquidTopSideBottom {
                kind,
                top,
                side,
                bottom,
            } => BlockDef::liquid_top_side_bottom(
                kind,
                top,
                side,
                bottom,
                self.breakable,
                self.render_layer,
                cull_group,
            ),
        };

        def = def.with_occlusion_shape(self.occlusion_shape());

        match self.face_tint {
            FaceTintSpec::None => {}
            FaceTintSpec::All(tint_kind) => {
                def = def.with_tint_all(tint_kind);
            }
        }

        if let Some(key) = self.block_entity {
            def = def.with_block_entity(key);
        }
        if let Some(random_tick) = self.random_tick {
            def = def.with_random_tick(random_tick);
        }
        Some(def)
    }
}

const AIR_KEY_ALIASES: [&str; 1] = ["core:air"];
const GRASS_KEY_ALIASES: [&str; 1] = ["core:grass"];
const STONE_KEY_ALIASES: [&str; 1] = ["core:stone"];
const DIRT_KEY_ALIASES: [&str; 1] = ["core:dirt"];
const SAPLING_KEY_ALIASES: [&str; 1] = ["core:sapling"];
const OAK_PLANKS_KEY_ALIASES: [&str; 1] = ["core:oak_planks"];
const BEDROCK_KEY_ALIASES: [&str; 1] = ["core:bedrock"];
const WATER_STILL_KEY_ALIASES: [&str; 1] = ["core:water_still"];
const WATER_FLOWING_KEY_ALIASES: [&str; 1] = ["core:water_flowing"];
const LAVA_STILL_KEY_ALIASES: [&str; 1] = ["core:lava_still"];
const LAVA_FLOWING_KEY_ALIASES: [&str; 1] = ["core:lava_flowing"];
const COBBLESTONE_KEY_ALIASES: [&str; 1] = ["core:cobblestone"];
const OBSIDIAN_KEY_ALIASES: [&str; 1] = ["core:obsidian"];
const SAND_KEY_ALIASES: [&str; 1] = ["core:sand"];

const BLOCK_SPECS: [BlockSpec; 51] = [
    BlockSpec {
        id: 0,
        variant: BlockId::Air,
        key: "minecraft:air",
        key_aliases: &AIR_KEY_ALIASES,
        display_name: "Air",
        model: BlockModelSpec::None,
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::None,
        face_occluding: false,
        solid: false,
        blocks_skylight: false,
        breakable: false,
        emitted_light: 0,
        particle_color: [0.0, 0.0, 0.0],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 1,
        variant: BlockId::Grass,
        key: "minecraft:grass_block",
        key_aliases: &GRASS_KEY_ALIASES,
        display_name: "Grass",
        model: BlockModelSpec::GrassTopSideBottom {
            top: TEX_GRASS_TOP,
            side: TEX_GRASS_SIDE,
            bottom: TEX_DIRT,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: false,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.35, 0.75, 0.25],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: Some(grass_random_tick),
    },
    BlockSpec {
        id: 2,
        variant: BlockId::Stone,
        key: "minecraft:stone",
        key_aliases: &STONE_KEY_ALIASES,
        display_name: "Stone",
        model: BlockModelSpec::CubeTopSideBottom {
            top: TEX_STONE,
            side: TEX_STONE,
            bottom: TEX_STONE,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.55, 0.55, 0.58],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 3,
        variant: BlockId::Dirt,
        key: "minecraft:dirt",
        key_aliases: &DIRT_KEY_ALIASES,
        display_name: "Dirt",
        model: BlockModelSpec::CubeAll { all: TEX_DIRT },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.49, 0.34, 0.20],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 4,
        variant: BlockId::Sapling,
        key: "minecraft:oak_sapling",
        key_aliases: &SAPLING_KEY_ALIASES,
        display_name: "Sapling",
        model: BlockModelSpec::PlantCross {
            texture: TEX_SAPLING,
        },
        render_layer: RenderLayer::Cutout,
        cull_group: CullGroupSpec::None,
        face_occluding: false,
        solid: false,
        blocks_skylight: false,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.28, 0.88, 0.36],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: Some(sapling_random_tick),
    },
    BlockSpec {
        id: 5,
        variant: BlockId::OakPlanks,
        key: "minecraft:oak_planks",
        key_aliases: &OAK_PLANKS_KEY_ALIASES,
        display_name: "Oak Planks",
        model: BlockModelSpec::CubeAll {
            all: TEX_OAK_PLANKS,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.68, 0.56, 0.34],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 6,
        variant: BlockId::Bedrock,
        key: "minecraft:bedrock",
        key_aliases: &BEDROCK_KEY_ALIASES,
        display_name: "Bedrock",
        model: BlockModelSpec::CubeAll { all: TEX_BEDROCK },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: false,
        emitted_light: 0,
        particle_color: [0.32, 0.32, 0.34],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 7,
        variant: BlockId::WaterStill,
        key: "minecraft:water",
        key_aliases: &WATER_STILL_KEY_ALIASES,
        display_name: "Water (Still)",
        model: BlockModelSpec::LiquidTopSideBottom {
            kind: FluidKind::Water,
            top: TEX_WATER_STILL,
            side: TEX_WATER_STILL,
            bottom: TEX_WATER_STILL,
        },
        render_layer: RenderLayer::Transparent,
        cull_group: CullGroupSpec::Water,
        face_occluding: false,
        solid: false,
        blocks_skylight: false,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.26, 0.48, 0.95],
        face_tint: FaceTintSpec::All(TINT_WATER),
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 8,
        variant: BlockId::WaterFlowing,
        key: "minecraft:flowing_water",
        key_aliases: &WATER_FLOWING_KEY_ALIASES,
        display_name: "Water (Flowing)",
        model: BlockModelSpec::LiquidTopSideBottom {
            kind: FluidKind::Water,
            top: TEX_WATER_STILL,
            side: TEX_WATER_FLOW,
            bottom: TEX_WATER_STILL,
        },
        render_layer: RenderLayer::Transparent,
        cull_group: CullGroupSpec::Water,
        face_occluding: false,
        solid: false,
        blocks_skylight: false,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.26, 0.48, 0.95],
        face_tint: FaceTintSpec::All(TINT_WATER),
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 9,
        variant: BlockId::LavaStill,
        key: "minecraft:lava",
        key_aliases: &LAVA_STILL_KEY_ALIASES,
        display_name: "Lava (Still)",
        model: BlockModelSpec::LiquidTopSideBottom {
            kind: FluidKind::Lava,
            top: TEX_LAVA_STILL,
            side: TEX_LAVA_STILL,
            bottom: TEX_LAVA_STILL,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::Lava,
        face_occluding: false,
        solid: false,
        blocks_skylight: false,
        breakable: false,
        emitted_light: 15,
        particle_color: [0.94, 0.44, 0.10],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 10,
        variant: BlockId::LavaFlowing,
        key: "minecraft:flowing_lava",
        key_aliases: &LAVA_FLOWING_KEY_ALIASES,
        display_name: "Lava (Flowing)",
        model: BlockModelSpec::LiquidTopSideBottom {
            kind: FluidKind::Lava,
            top: TEX_LAVA_STILL,
            side: TEX_LAVA_FLOW,
            bottom: TEX_LAVA_STILL,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::Lava,
        face_occluding: false,
        solid: false,
        blocks_skylight: false,
        breakable: false,
        emitted_light: 15,
        particle_color: [0.94, 0.44, 0.10],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 11,
        variant: BlockId::Cobblestone,
        key: "minecraft:cobblestone",
        key_aliases: &COBBLESTONE_KEY_ALIASES,
        display_name: "Cobblestone",
        model: BlockModelSpec::CubeAll {
            all: TEX_COBBLESTONE,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.48, 0.48, 0.50],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 12,
        variant: BlockId::Obsidian,
        key: "minecraft:obsidian",
        key_aliases: &OBSIDIAN_KEY_ALIASES,
        display_name: "Obsidian",
        model: BlockModelSpec::CubeAll { all: TEX_OBSIDIAN },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.16, 0.10, 0.22],
        face_tint: FaceTintSpec::None,
        block_entity: Some(CORE_FURNACE_STUB_BLOCK_ENTITY_KEY),
        random_tick: None,
    },
    BlockSpec {
        id: 13,
        variant: BlockId::Sand,
        key: "minecraft:sand",
        key_aliases: &SAND_KEY_ALIASES,
        display_name: "Sand",
        model: BlockModelSpec::CubeAll { all: TEX_SAND },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.85, 0.80, 0.55],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 14,
        variant: BlockId::Bricks,
        key: "minecraft:bricks",
        key_aliases: &[],
        display_name: "Bricks",
        model: BlockModelSpec::CubeAll { all: TEX_BRICKS },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.66, 0.31, 0.25],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 15,
        variant: BlockId::OakLog,
        key: "minecraft:oak_log",
        key_aliases: &[],
        display_name: "Oak Log",
        model: BlockModelSpec::CubeTopSideBottom {
            top: TEX_OAK_LOG_TOP,
            side: TEX_OAK_LOG_SIDE,
            bottom: TEX_OAK_LOG_TOP,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.55, 0.42, 0.26],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 16,
        variant: BlockId::OakLeaves,
        key: "minecraft:oak_leaves",
        key_aliases: &[],
        display_name: "Oak Leaves",
        model: BlockModelSpec::CubeAll {
            all: TEX_OAK_LEAVES,
        },
        render_layer: RenderLayer::Cutout,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: false,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.24, 0.56, 0.22],
        face_tint: FaceTintSpec::All(TINT_GRASS_TOP),
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 17,
        variant: BlockId::Glass,
        key: "minecraft:glass",
        key_aliases: &[],
        display_name: "Glass",
        model: BlockModelSpec::CubeAll { all: TEX_GLASS },
        render_layer: RenderLayer::Transparent,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: false,
        solid: true,
        blocks_skylight: false,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.82, 0.90, 0.92],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 18,
        variant: BlockId::StoneSlab,
        key: "minecraft:stone_slab",
        key_aliases: &[],
        display_name: "Stone Slab",
        model: BlockModelSpec::SlabBottomTopSideBottom {
            top: TEX_STONE_SLAB_TOP,
            side: TEX_STONE_SLAB_SIDE,
            bottom: TEX_STONE_SLAB_TOP,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: false,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.70, 0.70, 0.70],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 19,
        variant: BlockId::MossyCobblestone,
        key: "minecraft:mossy_cobblestone",
        key_aliases: &[],
        display_name: "Mossy Cobblestone",
        model: BlockModelSpec::CubeAll {
            all: TEX_MOSSY_COBBLESTONE,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.41, 0.47, 0.35],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 20,
        variant: BlockId::Gravel,
        key: "minecraft:gravel",
        key_aliases: &[],
        display_name: "Gravel",
        model: BlockModelSpec::CubeAll { all: TEX_GRAVEL },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.55, 0.54, 0.52],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 21,
        variant: BlockId::Sponge,
        key: "minecraft:sponge",
        key_aliases: &[],
        display_name: "Sponge",
        model: BlockModelSpec::CubeAll { all: TEX_SPONGE },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.86, 0.80, 0.27],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 22,
        variant: BlockId::Dandelion,
        key: "minecraft:dandelion",
        key_aliases: &[],
        display_name: "Dandelion",
        model: BlockModelSpec::PlantCross {
            texture: TEX_DANDELION,
        },
        render_layer: RenderLayer::Cutout,
        cull_group: CullGroupSpec::None,
        face_occluding: false,
        solid: false,
        blocks_skylight: false,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.97, 0.84, 0.12],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 23,
        variant: BlockId::Poppy,
        key: "minecraft:poppy",
        key_aliases: &[],
        display_name: "Poppy",
        model: BlockModelSpec::PlantCross { texture: TEX_POPPY },
        render_layer: RenderLayer::Cutout,
        cull_group: CullGroupSpec::None,
        face_occluding: false,
        solid: false,
        blocks_skylight: false,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.82, 0.18, 0.19],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 24,
        variant: BlockId::BrownMushroom,
        key: "minecraft:brown_mushroom",
        key_aliases: &[],
        display_name: "Brown Mushroom",
        model: BlockModelSpec::PlantCross {
            texture: TEX_BROWN_MUSHROOM,
        },
        render_layer: RenderLayer::Cutout,
        cull_group: CullGroupSpec::None,
        face_occluding: false,
        solid: false,
        blocks_skylight: false,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.52, 0.36, 0.25],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 25,
        variant: BlockId::RedMushroom,
        key: "minecraft:red_mushroom",
        key_aliases: &[],
        display_name: "Red Mushroom",
        model: BlockModelSpec::PlantCross {
            texture: TEX_RED_MUSHROOM,
        },
        render_layer: RenderLayer::Cutout,
        cull_group: CullGroupSpec::None,
        face_occluding: false,
        solid: false,
        blocks_skylight: false,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.80, 0.23, 0.21],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 26,
        variant: BlockId::CoalOre,
        key: "minecraft:coal_ore",
        key_aliases: &[],
        display_name: "Coal Ore",
        model: BlockModelSpec::CubeAll { all: TEX_COAL_ORE },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.43, 0.43, 0.43],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 27,
        variant: BlockId::IronOre,
        key: "minecraft:iron_ore",
        key_aliases: &[],
        display_name: "Iron Ore",
        model: BlockModelSpec::CubeAll { all: TEX_IRON_ORE },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.73, 0.62, 0.52],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 28,
        variant: BlockId::GoldOre,
        key: "minecraft:gold_ore",
        key_aliases: &[],
        display_name: "Gold Ore",
        model: BlockModelSpec::CubeAll { all: TEX_GOLD_ORE },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.82, 0.71, 0.32],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 29,
        variant: BlockId::IronBlock,
        key: "minecraft:iron_block",
        key_aliases: &[],
        display_name: "Iron Block",
        model: BlockModelSpec::CubeAll {
            all: TEX_IRON_BLOCK,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.88, 0.88, 0.90],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 30,
        variant: BlockId::GoldBlock,
        key: "minecraft:gold_block",
        key_aliases: &[],
        display_name: "Gold Block",
        model: BlockModelSpec::CubeAll {
            all: TEX_GOLD_BLOCK,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.96, 0.84, 0.27],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 31,
        variant: BlockId::Bookshelf,
        key: "minecraft:bookshelf",
        key_aliases: &[],
        display_name: "Bookshelf",
        model: BlockModelSpec::CubeTopSideBottom {
            top: TEX_OAK_PLANKS,
            side: TEX_BOOKSHELF_SIDE,
            bottom: TEX_OAK_PLANKS,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.61, 0.43, 0.28],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 32,
        variant: BlockId::Tnt,
        key: "minecraft:tnt",
        key_aliases: &[],
        display_name: "TNT",
        model: BlockModelSpec::CubeTopSideBottom {
            top: TEX_TNT_TOP,
            side: TEX_TNT_SIDE,
            bottom: TEX_TNT_BOTTOM,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.83, 0.28, 0.24],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 33,
        variant: BlockId::WhiteWool,
        key: "minecraft:white_wool",
        key_aliases: &[],
        display_name: "White Wool",
        model: BlockModelSpec::CubeAll {
            all: TEX_WHITE_WOOL,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.93, 0.93, 0.93],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 34,
        variant: BlockId::OrangeWool,
        key: "minecraft:orange_wool",
        key_aliases: &[],
        display_name: "Orange Wool",
        model: BlockModelSpec::CubeAll {
            all: TEX_ORANGE_WOOL,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.86, 0.50, 0.20],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 35,
        variant: BlockId::MagentaWool,
        key: "minecraft:magenta_wool",
        key_aliases: &[],
        display_name: "Magenta Wool",
        model: BlockModelSpec::CubeAll {
            all: TEX_MAGENTA_WOOL,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.75, 0.35, 0.73],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 36,
        variant: BlockId::LightBlueWool,
        key: "minecraft:light_blue_wool",
        key_aliases: &[],
        display_name: "Light Blue Wool",
        model: BlockModelSpec::CubeAll {
            all: TEX_LIGHT_BLUE_WOOL,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.45, 0.62, 0.86],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 37,
        variant: BlockId::YellowWool,
        key: "minecraft:yellow_wool",
        key_aliases: &[],
        display_name: "Yellow Wool",
        model: BlockModelSpec::CubeAll {
            all: TEX_YELLOW_WOOL,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.90, 0.84, 0.25],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 38,
        variant: BlockId::LimeWool,
        key: "minecraft:lime_wool",
        key_aliases: &[],
        display_name: "Lime Wool",
        model: BlockModelSpec::CubeAll { all: TEX_LIME_WOOL },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.50, 0.78, 0.28],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 39,
        variant: BlockId::PinkWool,
        key: "minecraft:pink_wool",
        key_aliases: &[],
        display_name: "Pink Wool",
        model: BlockModelSpec::CubeAll { all: TEX_PINK_WOOL },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.94, 0.60, 0.70],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 40,
        variant: BlockId::GrayWool,
        key: "minecraft:gray_wool",
        key_aliases: &[],
        display_name: "Gray Wool",
        model: BlockModelSpec::CubeAll { all: TEX_GRAY_WOOL },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.33, 0.36, 0.38],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 41,
        variant: BlockId::LightGrayWool,
        key: "minecraft:light_gray_wool",
        key_aliases: &[],
        display_name: "Light Gray Wool",
        model: BlockModelSpec::CubeAll {
            all: TEX_LIGHT_GRAY_WOOL,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.61, 0.61, 0.61],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 42,
        variant: BlockId::CyanWool,
        key: "minecraft:cyan_wool",
        key_aliases: &[],
        display_name: "Cyan Wool",
        model: BlockModelSpec::CubeAll { all: TEX_CYAN_WOOL },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.27, 0.56, 0.61],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 43,
        variant: BlockId::PurpleWool,
        key: "minecraft:purple_wool",
        key_aliases: &[],
        display_name: "Purple Wool",
        model: BlockModelSpec::CubeAll {
            all: TEX_PURPLE_WOOL,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.50, 0.31, 0.70],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 44,
        variant: BlockId::BlueWool,
        key: "minecraft:blue_wool",
        key_aliases: &[],
        display_name: "Blue Wool",
        model: BlockModelSpec::CubeAll { all: TEX_BLUE_WOOL },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.26, 0.33, 0.69],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 45,
        variant: BlockId::BrownWool,
        key: "minecraft:brown_wool",
        key_aliases: &[],
        display_name: "Brown Wool",
        model: BlockModelSpec::CubeAll {
            all: TEX_BROWN_WOOL,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.46, 0.33, 0.22],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 46,
        variant: BlockId::GreenWool,
        key: "minecraft:green_wool",
        key_aliases: &[],
        display_name: "Green Wool",
        model: BlockModelSpec::CubeAll {
            all: TEX_GREEN_WOOL,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.37, 0.52, 0.19],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 47,
        variant: BlockId::RedWool,
        key: "minecraft:red_wool",
        key_aliases: &[],
        display_name: "Red Wool",
        model: BlockModelSpec::CubeAll { all: TEX_RED_WOOL },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.67, 0.23, 0.23],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 48,
        variant: BlockId::BlackWool,
        key: "minecraft:black_wool",
        key_aliases: &[],
        display_name: "Black Wool",
        model: BlockModelSpec::CubeAll {
            all: TEX_BLACK_WOOL,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: true,
        solid: true,
        blocks_skylight: true,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.13, 0.13, 0.13],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 49,
        variant: BlockId::Ice,
        key: "minecraft:ice",
        key_aliases: &[],
        display_name: "Ice",
        model: BlockModelSpec::CubeAll { all: TEX_ICE },
        render_layer: RenderLayer::Transparent,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: false,
        solid: true,
        blocks_skylight: false,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.74, 0.86, 0.96],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
    BlockSpec {
        id: 50,
        variant: BlockId::Snow,
        key: "minecraft:snow",
        key_aliases: &[],
        display_name: "Snow",
        model: BlockModelSpec::LayerBottomTopSideBottom {
            height_px: 2,
            top: TEX_SNOW,
            side: TEX_SNOW,
            bottom: TEX_SNOW,
        },
        render_layer: RenderLayer::Opaque,
        cull_group: CullGroupSpec::SelfBlock,
        face_occluding: false,
        solid: true,
        blocks_skylight: false,
        breakable: true,
        emitted_light: 0,
        particle_color: [0.96, 0.97, 1.0],
        face_tint: FaceTintSpec::None,
        block_entity: None,
        random_tick: None,
    },
];

#[inline(always)]
fn block_spec_for(block: BlockId) -> &'static BlockSpec {
    let raw = block.to_u8() as usize;
    let spec = &BLOCK_SPECS[raw];
    debug_assert_eq!(spec.variant, block);
    debug_assert_eq!(spec.id, block.to_u8());
    spec
}

#[inline(always)]
fn block_spec_from_u8(raw: u8) -> Option<&'static BlockSpec> {
    let spec = BLOCK_SPECS.get(raw as usize)?;
    if spec.id == raw { Some(spec) } else { None }
}

fn namespaced_parts_match_str(namespace: &str, name: &str, expected: &str) -> bool {
    let Some((expected_namespace, expected_name)) = expected.split_once(':') else {
        return false;
    };
    namespace == expected_namespace && name == expected_name
}

fn block_from_key_parts(namespace: &str, name: &str) -> Option<BlockId> {
    for spec in BLOCK_SPECS {
        if namespaced_parts_match_str(namespace, name, spec.key) {
            return Some(spec.variant);
        }
        for alias in spec.key_aliases {
            if namespaced_parts_match_str(namespace, name, alias) {
                return Some(spec.variant);
            }
        }
    }

    if namespaced_parts_match_str(namespace, name, CORE_MISSING_BLOCK_KEY)
        || namespaced_parts_match_str(namespace, name, "minecraft:missing")
    {
        return Some(BlockId::Air);
    }

    None
}

#[derive(Clone, Copy, Debug)]
struct BlockDef {
    model: BlockModel,
    textures: [TextureKey; 6],
    face_tint_kinds: [u32; 6],
    plant_texture: Option<TextureKey>,
    occlusion_shape: OcclusionShape,
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
            occlusion_shape: OcclusionShape::FullCube,
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
            occlusion_shape: OcclusionShape::FullCube,
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

    const fn slab_bottom_top_side_bottom(
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
            model: BlockModel::SlabBottom,
            textures,
            face_tint_kinds: [TINT_NONE; 6],
            plant_texture: None,
            occlusion_shape: OcclusionShape::SlabLower,
            block_entity: None,
            random_tick: None,
            solid,
            breakable,
            face_occluding,
            render_layer,
            cull_group,
        }
    }

    const fn layer_bottom_top_side_bottom(
        height_px: u8,
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
            model: BlockModel::LayerBottom { height_px },
            textures,
            face_tint_kinds: [TINT_NONE; 6],
            plant_texture: None,
            occlusion_shape: OcclusionShape::Decorative,
            block_entity: None,
            random_tick: None,
            solid,
            breakable,
            face_occluding,
            render_layer,
            cull_group,
        }
    }

    const fn plant_cross(key: TextureKey) -> Self {
        Self {
            model: BlockModel::CrossPlant,
            textures: [key; 6],
            face_tint_kinds: [TINT_NONE; 6],
            plant_texture: Some(key),
            occlusion_shape: OcclusionShape::CrossPlant,
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
            occlusion_shape: OcclusionShape::Decorative,
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

    const fn with_occlusion_shape(mut self, occlusion_shape: OcclusionShape) -> Self {
        self.occlusion_shape = occlusion_shape;
        self
    }

    const fn with_tint_all(mut self, tint_kind: u32) -> Self {
        self.face_tint_kinds = [tint_kind; 6];
        self
    }

    fn texture_for(&self, face: Face) -> TextureKey {
        match self.model {
            BlockModel::Cube
            | BlockModel::SlabBottom
            | BlockModel::LayerBottom { .. }
            | BlockModel::Liquid { .. } => self.textures[face.as_usize()],
            BlockModel::CrossPlant => self
                .plant_texture
                .expect("CrossPlant BlockDef missing plant texture"),
        }
    }

    fn tint_kind_for(&self, face: Face) -> u32 {
        match self.model {
            BlockModel::Cube
            | BlockModel::SlabBottom
            | BlockModel::LayerBottom { .. }
            | BlockModel::Liquid { .. } => self.face_tint_kinds[face.as_usize()],
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
        let mut defs = HashMap::with_capacity(BLOCK_SPECS.len());
        for spec in BLOCK_SPECS {
            if let Some(def) = spec.block_def() {
                defs.insert(spec.variant, def);
            }
        }

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
        block_spec_for(block).block_entity
    }

    pub(crate) fn random_tick_for(&self, block: BlockId) -> Option<RandomTickFn> {
        block_spec_for(block).random_tick
    }

    pub(crate) fn has_random_tick(&self, block: BlockId) -> bool {
        self.random_tick_for(block).is_some()
    }

    pub(crate) fn registered_block_keys(&self) -> Vec<NamespacedKey> {
        let mut keys: Vec<NamespacedKey> = BLOCK_SPECS
            .iter()
            .filter_map(|spec| NamespacedKey::parse(spec.key))
            .collect();
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

    pub(crate) fn resolve_fast(&self, name: &str) -> Option<BlockId> {
        let raw = name.trim();
        if raw.is_empty() {
            return None;
        }
        if let Some((namespace, name)) = raw.split_once(':') {
            let namespace = namespace.trim();
            let name = name.trim();
            if namespace.is_empty() || name.is_empty() {
                return None;
            }
            return block_from_key_parts(namespace, name);
        }
        block_from_key_parts("core", raw)
    }

    pub(crate) fn collect_texture_keys(&self) -> Vec<String> {
        let mut keys: HashSet<String> = HashSet::new();
        for def in self.defs.values() {
            match def.model {
                BlockModel::Cube
                | BlockModel::SlabBottom
                | BlockModel::LayerBottom { .. }
                | BlockModel::Liquid { .. } => {
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

    pub(crate) fn has_def_for(&self, block: BlockId) -> bool {
        self.defs.contains_key(&block)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BlockTextureInfo {
    pub(crate) model: BlockModel,
    pub(crate) cube_layers: BlockTextureLayers,
    pub(crate) face_tint_kinds: [u32; 6],
    pub(crate) plant_layer: Option<u32>,
    pub(crate) occlusion_shape: OcclusionShape,
    pub(crate) solid: bool,
    pub(crate) breakable: bool,
    pub(crate) face_occluding: bool,
    pub(crate) render_layer: RenderLayer,
    pub(crate) cull_group: CullGroup,
}

pub(crate) struct BlockTextureLookup {
    by_block: [BlockTextureInfo; 256],
    missing_layer: u32,
}

impl BlockTextureLookup {
    pub(crate) fn new(
        block_registry: &BlockRegistry,
        key_to_layer: &HashMap<String, u32>,
        missing_layer: u32,
    ) -> Self {
        let mut warned_missing_keys: HashSet<&'static str> = HashSet::new();

        let missing_info = BlockTextureInfo {
            model: BlockModel::Cube,
            cube_layers: BlockTextureLayers {
                up: missing_layer,
                down: missing_layer,
                north: missing_layer,
                south: missing_layer,
                east: missing_layer,
                west: missing_layer,
            },
            face_tint_kinds: [TINT_NONE; 6],
            plant_layer: None,
            occlusion_shape: OcclusionShape::Decorative,
            solid: false,
            breakable: false,
            face_occluding: false,
            render_layer: RenderLayer::Opaque,
            cull_group: CullGroup::None,
        };
        let mut by_block = std::array::from_fn(|_| missing_info);
        for (&block, &textures) in &block_registry.defs {
            let mut resolve = |key: &'static str| -> u32 {
                if let Some(layer) = key_to_layer.get(key).copied() {
                    layer
                } else {
                    if warned_missing_keys.insert(key) {
                        crate::console_error!(
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
            by_block[block.to_u8() as usize] = BlockTextureInfo {
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
                occlusion_shape: textures.occlusion_shape,
                solid: textures.solid,
                breakable: textures.breakable,
                face_occluding: textures.face_occluding,
                render_layer: textures.render_layer,
                cull_group: textures.cull_group,
            };
        }

        Self {
            by_block,
            missing_layer,
        }
    }

    #[inline(always)]
    pub(crate) fn info_for(&self, block: BlockId) -> BlockTextureInfo {
        self.by_block[block.to_u8() as usize]
    }

    #[inline(always)]
    pub(crate) fn layer_for_info(&self, info: BlockTextureInfo, face: Face) -> u32 {
        match info.model {
            BlockModel::Cube
            | BlockModel::SlabBottom
            | BlockModel::LayerBottom { .. }
            | BlockModel::Liquid { .. } => info.cube_layers.layer_for_face(face),
            BlockModel::CrossPlant => info.plant_layer.unwrap_or(self.missing_layer),
        }
    }

    #[inline(always)]
    pub(crate) fn tint_kind_for_info(&self, info: BlockTextureInfo, face: Face) -> u32 {
        match info.model {
            BlockModel::Cube
            | BlockModel::SlabBottom
            | BlockModel::LayerBottom { .. }
            | BlockModel::Liquid { .. } => info.face_tint_kinds[face.as_usize()],
            BlockModel::CrossPlant => TINT_NONE,
        }
    }

    pub(crate) fn layer_for(&self, block: BlockId, face: Face) -> u32 {
        self.layer_for_info(self.info_for(block), face)
    }

    pub(crate) fn tint_kind_for(&self, block: BlockId, face: Face) -> u32 {
        self.tint_kind_for_info(self.info_for(block), face)
    }

    pub(crate) fn model_for(&self, block: BlockId) -> BlockModel {
        self.info_for(block).model
    }

    pub(crate) fn is_solid_for(&self, block: BlockId) -> bool {
        self.info_for(block).solid
    }

    pub(crate) fn is_breakable_for(&self, block: BlockId) -> bool {
        self.info_for(block).breakable
    }

    pub(crate) fn render_layer_for(&self, block: BlockId) -> RenderLayer {
        self.info_for(block).render_layer
    }

    pub(crate) fn occlusion_shape_for(&self, block: BlockId) -> OcclusionShape {
        self.info_for(block).occlusion_shape
    }

    pub(crate) fn face_occluding_for(&self, block: BlockId) -> bool {
        self.info_for(block).face_occluding
    }

    pub(crate) fn cull_group_for(&self, block: BlockId) -> CullGroup {
        self.info_for(block).cull_group
    }

    pub(crate) fn plant_layer_for(&self, block: BlockId) -> Option<u32> {
        self.info_for(block).plant_layer
    }
}

impl BlockId {
    pub(crate) fn collision_shape(self) -> CollisionShape {
        block_spec_for(self).collision_shape()
    }

    pub(crate) fn raycast_shape(self) -> CollisionShape {
        block_spec_for(self).raycast_shape()
    }

    /// Visibility/AO occlusion shape. This is intentionally separate from collision,
    /// raycast selection, and render shape.
    pub(crate) fn occlusion_shape(self) -> OcclusionShape {
        block_spec_for(self).occlusion_shape()
    }

    /// Physische Solidität (Collision).
    pub(crate) fn is_solid(self) -> bool {
        block_spec_for(self).solid
    }

    /// Ob der Block als Voxel-Zelle vorhanden ist (Meshing/Raycast).
    #[inline(always)]
    pub(crate) fn is_voxel_filled(self) -> bool {
        self != BlockId::Air
    }

    #[inline]
    pub(crate) fn blocks_skylight(self) -> bool {
        block_spec_for(self).blocks_skylight
    }

    pub(crate) fn emitted_light(self) -> u8 {
        block_spec_for(self).emitted_light
    }

    pub(crate) fn color(self) -> [f32; 3] {
        block_spec_for(self).particle_color
    }

    pub(crate) fn to_u8(self) -> u8 {
        self as u8
    }

    pub(crate) fn from_u8(value: u8) -> Result<Self, InvalidBlockId> {
        block_spec_from_u8(value)
            .map(|spec| spec.variant)
            .ok_or(InvalidBlockId { raw: value })
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
        block_spec_for(self).display_name
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
    let above = pos + IVec3::Y;
    if !World::in_bounds(above)
        || world
            .get_chunk(World::world_to_chunk_coord(above))
            .is_none()
    {
        return;
    }

    let above_block = world.get_block(above);
    if above_block.blocks_skylight()
        || matches!(
            above_block,
            BlockId::WaterStill | BlockId::WaterFlowing | BlockId::LavaStill | BlockId::LavaFlowing
        )
    {
        let _ = world.set_block_and_data_loaded_only(pos, BlockId::Dirt, 0);
        return;
    }

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

#[derive(Clone, Copy, Debug)]
enum SaplingTreePreset {
    SmallOak,
    TallOak,
    WideCanopyOak,
    BushTree,
    GreatOak,
    MonumentOak,
}

#[derive(Clone, Copy, Debug)]
struct SaplingTreeShape {
    preset: SaplingTreePreset,
    trunk_height: i32,
}

pub(crate) fn sapling_stage(data: u8) -> u8 {
    data & 1
}

pub(crate) fn sapling_set_stage(data: u8, stage: u8) -> u8 {
    (data & !1) | (stage & 1)
}

fn sapling_rng_seed(world_seed: u64, pos: IVec3, tick_index: u64) -> u64 {
    let mut mixed = world_seed ^ tick_index.wrapping_mul(0xD6E8_FEB8_6659_FD93);
    mixed ^= (pos.x as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    mixed ^= (pos.y as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    mixed ^= (pos.z as i64 as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
    mixed
}

fn sapling_rng_next(seed: &mut u64) -> u32 {
    *seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *seed;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (z ^ (z >> 31)) as u32
}

fn choose_sapling_tree_shape(seed: &mut u64) -> SaplingTreeShape {
    let roll = sapling_rng_next(seed) % 100;
    let preset = if roll < 20 {
        SaplingTreePreset::SmallOak
    } else if roll < 40 {
        SaplingTreePreset::TallOak
    } else if roll < 60 {
        SaplingTreePreset::WideCanopyOak
    } else if roll < 80 {
        SaplingTreePreset::BushTree
    } else if roll < 90 {
        SaplingTreePreset::GreatOak
    } else {
        SaplingTreePreset::MonumentOak
    };
    let trunk_height = match preset {
        SaplingTreePreset::SmallOak => 4,
        SaplingTreePreset::TallOak => 6 + (sapling_rng_next(seed) % 2) as i32,
        SaplingTreePreset::WideCanopyOak => 5,
        SaplingTreePreset::BushTree => 2 + (sapling_rng_next(seed) % 2) as i32,
        SaplingTreePreset::GreatOak => 8 + (sapling_rng_next(seed) % 3) as i32,
        SaplingTreePreset::MonumentOak => 9 + (sapling_rng_next(seed) % 3) as i32,
    };
    SaplingTreeShape {
        preset,
        trunk_height,
    }
}

fn push_leaf_layer(
    out: &mut HashSet<IVec3>,
    center_x: i32,
    center_y: i32,
    center_z: i32,
    radius: i32,
    cut_corners: bool,
) {
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            if cut_corners && dx.abs() == radius && dz.abs() == radius {
                continue;
            }
            out.insert(IVec3::new(center_x + dx, center_y, center_z + dz));
        }
    }
}

fn sapling_tree_occupied_positions(
    base_pos: IVec3,
    shape: SaplingTreeShape,
) -> (Vec<IVec3>, Vec<IVec3>) {
    let mut trunk = Vec::with_capacity(shape.trunk_height as usize + 1);
    let mut leaves = HashSet::with_capacity(192);
    let top_y = base_pos.y + shape.trunk_height - 1;

    for y in base_pos.y..=top_y {
        trunk.push(IVec3::new(base_pos.x, y, base_pos.z));
    }

    match shape.preset {
        SaplingTreePreset::SmallOak => {
            push_leaf_layer(&mut leaves, base_pos.x, top_y - 2, base_pos.z, 1, false);
            push_leaf_layer(&mut leaves, base_pos.x, top_y - 1, base_pos.z, 1, false);
            push_leaf_layer(&mut leaves, base_pos.x, top_y, base_pos.z, 1, true);
            leaves.insert(IVec3::new(base_pos.x, top_y + 1, base_pos.z));
        }
        SaplingTreePreset::TallOak => {
            push_leaf_layer(&mut leaves, base_pos.x, top_y - 3, base_pos.z, 1, false);
            push_leaf_layer(&mut leaves, base_pos.x, top_y - 2, base_pos.z, 2, true);
            push_leaf_layer(&mut leaves, base_pos.x, top_y - 1, base_pos.z, 2, false);
            push_leaf_layer(&mut leaves, base_pos.x, top_y, base_pos.z, 2, true);
            push_leaf_layer(&mut leaves, base_pos.x, top_y + 1, base_pos.z, 1, false);
            leaves.insert(IVec3::new(base_pos.x, top_y + 2, base_pos.z));
        }
        SaplingTreePreset::WideCanopyOak => {
            push_leaf_layer(&mut leaves, base_pos.x, top_y - 2, base_pos.z, 1, false);
            push_leaf_layer(&mut leaves, base_pos.x, top_y - 1, base_pos.z, 2, true);
            push_leaf_layer(&mut leaves, base_pos.x, top_y, base_pos.z, 2, false);
            push_leaf_layer(&mut leaves, base_pos.x, top_y + 1, base_pos.z, 1, true);
            for (dx, dz) in [(2, 0), (-2, 0), (0, 2), (0, -2)] {
                leaves.insert(IVec3::new(base_pos.x + dx, top_y - 1, base_pos.z + dz));
                leaves.insert(IVec3::new(base_pos.x + dx, top_y - 2, base_pos.z + dz));
            }
        }
        SaplingTreePreset::BushTree => {
            push_leaf_layer(&mut leaves, base_pos.x, top_y - 1, base_pos.z, 1, false);
            push_leaf_layer(&mut leaves, base_pos.x, top_y, base_pos.z, 1, false);
            push_leaf_layer(&mut leaves, base_pos.x, top_y + 1, base_pos.z, 1, true);
        }
        SaplingTreePreset::GreatOak => {
            push_leaf_layer(&mut leaves, base_pos.x, top_y - 4, base_pos.z, 2, true);
            push_leaf_layer(&mut leaves, base_pos.x, top_y - 3, base_pos.z, 3, true);
            push_leaf_layer(&mut leaves, base_pos.x, top_y - 2, base_pos.z, 3, false);
            push_leaf_layer(&mut leaves, base_pos.x, top_y - 1, base_pos.z, 3, true);
            push_leaf_layer(&mut leaves, base_pos.x, top_y, base_pos.z, 2, true);
            push_leaf_layer(&mut leaves, base_pos.x, top_y + 1, base_pos.z, 1, false);
            leaves.insert(IVec3::new(base_pos.x, top_y + 2, base_pos.z));
            for (dx, dz) in [(3, 0), (-3, 0), (0, 3), (0, -3)] {
                leaves.insert(IVec3::new(base_pos.x + dx, top_y - 2, base_pos.z + dz));
                leaves.insert(IVec3::new(base_pos.x + dx, top_y - 3, base_pos.z + dz));
            }
        }
        SaplingTreePreset::MonumentOak => {
            push_leaf_layer(&mut leaves, base_pos.x, top_y - 5, base_pos.z, 2, false);
            push_leaf_layer(&mut leaves, base_pos.x, top_y - 4, base_pos.z, 3, true);
            push_leaf_layer(&mut leaves, base_pos.x, top_y - 3, base_pos.z, 4, true);
            push_leaf_layer(&mut leaves, base_pos.x, top_y - 2, base_pos.z, 4, false);
            push_leaf_layer(&mut leaves, base_pos.x, top_y - 1, base_pos.z, 3, true);
            push_leaf_layer(&mut leaves, base_pos.x, top_y, base_pos.z, 2, true);
            push_leaf_layer(&mut leaves, base_pos.x, top_y + 1, base_pos.z, 1, true);
            leaves.insert(IVec3::new(base_pos.x, top_y + 2, base_pos.z));
            for (dx, dz) in [(4, 0), (-4, 0), (0, 4), (0, -4)] {
                leaves.insert(IVec3::new(base_pos.x + dx, top_y - 2, base_pos.z + dz));
                leaves.insert(IVec3::new(base_pos.x + dx, top_y - 3, base_pos.z + dz));
            }
        }
    }

    (trunk, leaves.into_iter().collect())
}

fn can_replace_trunk_block(block: BlockId) -> bool {
    matches!(
        block,
        BlockId::Air
            | BlockId::OakLeaves
            | BlockId::Snow
            | BlockId::Sapling
            | BlockId::Dandelion
            | BlockId::Poppy
            | BlockId::BrownMushroom
            | BlockId::RedMushroom
    )
}

fn can_replace_leaf_block(block: BlockId) -> bool {
    matches!(
        block,
        BlockId::Air
            | BlockId::OakLeaves
            | BlockId::Snow
            | BlockId::Sapling
            | BlockId::Dandelion
            | BlockId::Poppy
            | BlockId::BrownMushroom
            | BlockId::RedMushroom
    )
}

fn can_grow_sapling_tree(world: &World, trunk: &[IVec3], leaves: &[IVec3]) -> bool {
    let trunk_set: HashSet<IVec3> = trunk.iter().copied().collect();

    for pos in trunk {
        if !World::in_bounds(*pos) || !world.is_chunk_loaded_at(*pos) {
            return false;
        }
        if !can_replace_trunk_block(world.get_block(*pos)) {
            return false;
        }
    }

    for pos in leaves {
        if trunk_set.contains(pos) {
            continue;
        }
        if !World::in_bounds(*pos) || !world.is_chunk_loaded_at(*pos) {
            return false;
        }
        if !can_replace_leaf_block(world.get_block(*pos)) {
            return false;
        }
    }

    true
}

fn place_sapling_tree(world: &mut World, sapling_pos: IVec3, trunk: &[IVec3], leaves: &[IVec3]) {
    let trunk_set: HashSet<IVec3> = trunk.iter().copied().collect();

    for pos in trunk {
        let _ = world.set_block_and_data_loaded_only(*pos, BlockId::OakLog, 0);
    }
    for pos in leaves {
        if trunk_set.contains(pos) {
            continue;
        }
        let current = world.get_block(*pos);
        if can_replace_leaf_block(current) {
            let _ = world.set_block_and_data_loaded_only(*pos, BlockId::OakLeaves, 0);
        }
    }

    let below = sapling_pos - IVec3::Y;
    if World::in_bounds(below)
        && world.is_chunk_loaded_at(below)
        && world.get_block(below) == BlockId::Grass
    {
        let _ = world.set_block_and_data_loaded_only(below, BlockId::Dirt, 0);
    }
}

fn sapling_random_tick(world: &mut World, pos: IVec3, _state: BlockStateId, tick_index: u64) {
    if !World::in_bounds(pos) || world.get_block(pos) != BlockId::Sapling {
        return;
    }

    let light_probe = pos + IVec3::Y;
    if !World::in_bounds(light_probe)
        || !world.is_chunk_loaded_at(pos)
        || !world.is_chunk_loaded_at(light_probe)
    {
        return;
    }

    let light = world.get_light(light_probe);
    let effective_light = light.sky.max(light.block);
    if effective_light < 9 {
        return;
    }

    let mut seed = sapling_rng_seed(world.seed(), pos, tick_index);
    if sapling_rng_next(&mut seed) % 7 != 0 {
        return;
    }

    let data = world.get_block_data(pos);
    let stage = sapling_stage(data);
    if stage == 0 {
        let next_data = sapling_set_stage(data, 1);
        let _ = world.set_block_and_data_loaded_only(pos, BlockId::Sapling, next_data);
        return;
    }

    let shape = choose_sapling_tree_shape(&mut seed);
    let (trunk_positions, leaf_positions) = sapling_tree_occupied_positions(pos, shape);
    if !can_grow_sapling_tree(world, &trunk_positions, &leaf_positions) {
        return;
    }
    place_sapling_tree(world, pos, &trunk_positions, &leaf_positions);
}

pub(crate) fn block_entity_key_str_for_block(block: BlockId) -> Option<&'static str> {
    block_spec_for(block).block_entity
}

pub(crate) fn block_entity_key_for_block(block: BlockId) -> Option<NamespacedKey> {
    block_entity_key_str_for_block(block).and_then(NamespacedKey::parse)
}

pub(crate) fn block_has_block_entity(block: BlockId) -> bool {
    block_entity_key_str_for_block(block).is_some()
}

pub(crate) fn block_key_str_for_block(block: BlockId) -> &'static str {
    block_spec_for(block).key
}

pub(crate) fn block_key_for_block(block: BlockId) -> NamespacedKey {
    NamespacedKey::parse(block_key_str_for_block(block))
        .expect("invalid hardcoded runtime block key")
}

pub(crate) fn block_from_key(key: &NamespacedKey) -> Option<BlockId> {
    block_from_key_parts(&key.namespace, &key.name)
}

pub(crate) fn debug_assert_block_blueprint_integrity(block_registry: &BlockRegistry) {
    if !cfg!(debug_assertions) {
        return;
    }

    let mut seen_ids = HashSet::new();
    let mut seen_keys = HashSet::new();

    for spec in BLOCK_SPECS {
        debug_assert!(
            seen_ids.insert(spec.id),
            "duplicate block blueprint runtime id {}",
            spec.id
        );
        debug_assert!(
            seen_keys.insert(spec.key),
            "duplicate block blueprint key {}",
            spec.key
        );

        let roundtrip = BlockId::from_u8(spec.id)
            .unwrap_or_else(|_| panic!("block id {} from blueprint is not loadable", spec.id));
        debug_assert_eq!(
            roundtrip, spec.variant,
            "block id {} resolves to {:?}, expected {:?}",
            spec.id, roundtrip, spec.variant
        );
        debug_assert_eq!(
            roundtrip.to_u8(),
            spec.id,
            "block {:?} roundtrip id mismatch",
            roundtrip
        );

        if spec.variant != BlockId::Air {
            debug_assert!(
                block_registry.has_def_for(spec.variant),
                "missing BlockDef for {:?}",
                spec.variant
            );
        }
    }

    let registered_keys: HashSet<String> = block_registry
        .registered_block_keys()
        .into_iter()
        .map(|key| key.to_string())
        .collect();
    for spec in BLOCK_SPECS {
        debug_assert!(
            registered_keys.contains(spec.key),
            "registered block keys missing {}",
            spec.key
        );
    }
}

#[cfg(test)]
mod tests {
    use glam::IVec3;

    use super::{BlockId, OcclusionShape, grass_random_tick};
    use crate::world::World;

    #[test]
    fn stone_slab_keeps_partial_occlusion_without_blocking_skylight() {
        assert_eq!(
            BlockId::StoneSlab.occlusion_shape(),
            OcclusionShape::SlabLower
        );
        assert!(!BlockId::StoneSlab.blocks_skylight());
    }

    #[test]
    fn grass_turns_to_dirt_when_covered_by_block() {
        let mut world = World::new();
        let pos = IVec3::new(0, 64, 0);
        let above = pos + IVec3::Y;
        world.set_block(pos, BlockId::Grass).unwrap();
        world.set_block(above, BlockId::Stone).unwrap();

        grass_random_tick(&mut world, pos, BlockId::Grass, 0);

        assert_eq!(world.get_block(pos), BlockId::Dirt);
    }
}
