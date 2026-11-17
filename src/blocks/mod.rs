use std::collections::{HashMap, HashSet};
use std::fmt;

use glam::{IVec3, Vec3};

use crate::registry::NamespacedKey;
use crate::world::World;
use crate::world::block_updates_system::BlockTicks;
use crate::world::fluids::FluidKind;

mod falling;
pub(crate) mod placement;
pub(crate) mod plants;
pub(crate) mod state;

use placement::{OnPlaceFn, front_toward_placer, slab_on_place};
use plants::{
    cactus_neighbor_changed, cactus_on_place, column_plant_random_tick, mushroom_random_tick,
    sugar_cane_neighbor_changed, sugar_cane_on_place,
};
pub(crate) use state::BlockState;
use state::views::{Facing4, Half};
use state::{
    FLUID_PROPERTIES, FURNACE_FACING, FURNACE_PROPERTIES, PLANT_AGE_PROPERTIES, Property,
    PropertySpec, SAPLING_PROPERTIES, SAPLING_STAGE, SLAB_HALF, SLAB_PROPERTIES, StateForm,
};

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
pub(crate) const TEX_DIAMOND_ORE: &str = "minecraft:textures/block/diamond_ore.png";
pub(crate) const TEX_REDSTONE_ORE: &str = "minecraft:textures/block/redstone_ore.png";
pub(crate) const TEX_DIAMOND_BLOCK: &str = "minecraft:textures/block/diamond_block.png";
pub(crate) const TEX_CLAY: &str = "minecraft:textures/block/clay.png";
pub(crate) const TEX_CRAFTING_TABLE_TOP: &str = "minecraft:textures/block/crafting_table_top.png";
pub(crate) const TEX_CRAFTING_TABLE_SIDE: &str = "minecraft:textures/block/crafting_table_side.png";
pub(crate) const TEX_CRAFTING_TABLE_FRONT: &str =
    "minecraft:textures/block/crafting_table_front.png";
pub(crate) const TEX_FURNACE_TOP: &str = "minecraft:textures/block/furnace_top.png";
pub(crate) const TEX_FURNACE_SIDE: &str = "minecraft:textures/block/furnace_side.png";
pub(crate) const TEX_FURNACE_FRONT: &str = "minecraft:textures/block/furnace_front.png";
pub(crate) const TEX_FURNACE_FRONT_ON: &str = "minecraft:textures/block/furnace_front_on.png";
pub(crate) const TEX_JUKEBOX_TOP: &str = "minecraft:textures/block/jukebox_top.png";
pub(crate) const TEX_JUKEBOX_SIDE: &str = "minecraft:textures/block/jukebox_side.png";
pub(crate) const TEX_SPAWNER: &str = "minecraft:textures/block/spawner.png";
pub(crate) const TEX_SUGAR_CANE: &str = "minecraft:textures/block/sugar_cane.png";
pub(crate) const TEX_CACTUS_TOP: &str = "minecraft:textures/block/cactus_top.png";
pub(crate) const TEX_CACTUS_SIDE: &str = "minecraft:textures/block/cactus_side.png";
pub(crate) const TEX_CACTUS_BOTTOM: &str = "minecraft:textures/block/cactus_bottom.png";
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

/// A block's random-tick hook: the world, the block's position, its current
/// state there, and the tick index.
pub(crate) type RandomTickFn = fn(&mut World, IVec3, BlockState, u64);

/// A block's reaction to a change in a neighboring cell, or in its own cell
/// (`from == pos`, a block arriving — Vanilla `onPlace`): the world, the
/// block's position and state, where the change happened, and the tick queue.
/// Run by `world::block_updates_system`.
pub(crate) type NeighborFn = fn(&mut World, IVec3, BlockState, IVec3, &mut BlockTicks);

/// A tick the block scheduled for itself (`BlockTicks::schedule`): the world,
/// the block's position and state, and the tick queue.
pub(crate) type ScheduledTickFn = fn(&mut World, IVec3, BlockState, &mut BlockTicks);

/// A block's hooks in the block-update system.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BlockBehavior {
    pub(crate) on_neighbor_changed: Option<NeighborFn>,
    pub(crate) on_scheduled_tick: Option<ScheduledTickFn>,
}

/// Declares every block in one place: each `Variant => spec` entry becomes a
/// `BlockId` variant and the matching `BLOCK_SPECS` row, and `BLOCK_COUNT`
/// follows from the list. The runtime id is the entry's position.
///
/// Only ever append: pre-manifest saves store runtime ids raw
/// (`WorldManifest::build_from_registry` keeps them as manifest ids), so moving
/// an entry renumbers blocks in those worlds.
macro_rules! blocks {
    ($($variant:ident => $spec:expr,)+) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub(crate) enum BlockId {
            $($variant,)+
        }

        pub(crate) const BLOCK_COUNT: usize = [$(BlockId::$variant),+].len();

        const BLOCK_SPECS: [BlockSpec; BLOCK_COUNT] =
            [$($spec.with_variant(BlockId::$variant),)+];
    };
}

/// The creative inventory palette: every block with a `creative_order`, sorted
/// by it at compile time.
pub(crate) const CLASSIC_PALETTE: &[BlockId] = &CREATIVE_PALETTE;

const CREATIVE_PALETTE_LEN: usize = {
    let mut len = 0;
    let mut i = 0;
    while i < BLOCK_COUNT {
        if BLOCK_SPECS[i].creative_order.is_some() {
            len += 1;
        }
        i += 1;
    }
    len
};

const CREATIVE_PALETTE: [BlockId; CREATIVE_PALETTE_LEN] = {
    // Insertion sort; two blocks claiming the same slot fail the build.
    let mut blocks = [BlockId::Air; CREATIVE_PALETTE_LEN];
    let mut orders = [0u16; CREATIVE_PALETTE_LEN];
    let mut len = 0;
    let mut i = 0;
    while i < BLOCK_COUNT {
        if let Some(order) = BLOCK_SPECS[i].creative_order {
            let mut slot = len;
            while slot > 0 && orders[slot - 1] > order {
                orders[slot] = orders[slot - 1];
                blocks[slot] = blocks[slot - 1];
                slot -= 1;
            }
            assert!(
                slot == 0 || orders[slot - 1] != order,
                "two blocks share a creative_order"
            );
            orders[slot] = order;
            blocks[slot] = BLOCK_SPECS[i].variant;
            len += 1;
        }
        i += 1;
    }
    blocks
};

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

    /// The face a raycast normal points along.
    pub(crate) const fn from_normal(normal: IVec3) -> Face {
        match (normal.x, normal.y, normal.z) {
            (1, _, _) => Face::East,
            (-1, _, _) => Face::West,
            (_, 1, _) => Face::Up,
            (_, -1, _) => Face::Down,
            (_, _, 1) => Face::South,
            _ => Face::North,
        }
    }

    /// Which face of a model, turned `quarter_turns` clockwise seen from
    /// above, ends up on this side. Up and Down stay where they are.
    #[inline(always)]
    pub(crate) const fn model_face_after_turns(self, quarter_turns: u8) -> Face {
        const RING: [Face; 4] = [Face::North, Face::East, Face::South, Face::West];
        let index = match self {
            Face::North => 0,
            Face::East => 1,
            Face::South => 2,
            Face::West => 3,
            Face::Up | Face::Down => return self,
        };
        RING[(index + 4 - (quarter_turns & 3) as usize) & 3]
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
    /// Full top and bottom faces; the four side faces sit `inset_px` inside
    /// the cell (cactus).
    InsetSides { inset_px: u8 },
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
const UPPER_SLAB_AABB: BlockAabb = BlockAabb::from_pixels([0, 8, 0], [16, 16, 16]);
const SNOW_LAYER_AABB: BlockAabb = BlockAabb::from_pixels([0, 0, 0], [16, 2, 16]);

const fn inset_sides_aabb(inset_px: u8) -> BlockAabb {
    BlockAabb::from_pixels([inset_px, 0, inset_px], [16 - inset_px, 16, 16 - inset_px])
}
// Shared plant selection box: slightly inset from the block edges and one pixel shy of full height.
const CROSS_PLANT_SELECTION_AABB: BlockAabb = BlockAabb::from_pixels([1, 0, 1], [15, 15, 15]);

/// Physics/collision or selection/raycast shape. Collision and selection intentionally
/// stay separate at the metadata layer even though they reuse the same AABB primitive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CollisionShape {
    Empty,
    FullCube,
    SlabBottom,
    SlabTop,
    Aabb(BlockAabb),
}

impl CollisionShape {
    pub(crate) const fn local_bounds(self) -> Option<BlockAabb> {
        match self {
            CollisionShape::Empty => None,
            CollisionShape::FullCube => Some(FULL_CUBE_AABB),
            CollisionShape::SlabBottom => Some(LOWER_SLAB_AABB),
            CollisionShape::SlabTop => Some(UPPER_SLAB_AABB),
            CollisionShape::Aabb(bounds) => Some(bounds),
        }
    }

    /// The same shape in a block whose state puts it in the upper half of
    /// the cell (`StateForm::is_top`).
    pub(crate) const fn moved_to_top_half(self) -> Self {
        match self {
            CollisionShape::SlabBottom => CollisionShape::SlabTop,
            other => other,
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
    /// See `CollisionShape::moved_to_top_half`.
    #[inline(always)]
    pub(crate) const fn moved_to_top_half(self) -> Self {
        match self {
            OcclusionShape::SlabLower => OcclusionShape::SlabUpper,
            other => other,
        }
    }

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
    /// A full cube with its own texture on every face.
    CubeFaces {
        up: TextureKey,
        down: TextureKey,
        north: TextureKey,
        south: TextureKey,
        east: TextureKey,
        west: TextureKey,
    },
    /// `BlockModel::InsetSides`; selected and collided as the inset box.
    InsetSidesTopSideBottom {
        inset_px: u8,
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

/// How a block relates to skylight.
///
/// The default derives from the occlusion shape, so lighting and meshing cannot
/// drift apart the way the old per-block bool let them: `grass_block` was a full
/// opaque cube that let skylight straight through, and `stone_slab` the same.
/// The two named exceptions below are the cases where the shape genuinely
/// cannot express the answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SkylightSpec {
    /// Anything that fully covers its own `Down` boundary stops skylight.
    FromShape,
    /// Stops skylight without occluding faces — leaves shade what grows under
    /// them, but you can see through the gaps.
    Blocks,
    /// Passes skylight even if the shape says otherwise. Glass is
    /// `Decorative`-shaped today, so this is currently belt-and-braces: it keeps
    /// glass transparent to light if it ever becomes face-occluding for culling.
    Transmits,
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
    skylight: SkylightSpec,
    breakable: bool,
    emitted_light: u8,
    particle_color: [f32; 3],
    face_tint: FaceTintSpec,
    block_entity: Option<&'static str>,
    random_tick: Option<RandomTickFn>,
    /// Picks the placed state from the click (`placement::placement_state`);
    /// `None` places the default state.
    on_place: Option<OnPlaceFn>,
    /// Block-update hooks (`BlockId::behavior`).
    on_neighbor_changed: Option<NeighborFn>,
    on_scheduled_tick: Option<ScheduledTickFn>,
    /// Drops onto the first solid block below when unsupported (sand,
    /// gravel). Stands in for both hooks above with the ones in `falling.rs`.
    falls: bool,
    /// Position in the creative palette (`CLASSIC_PALETTE`); `None` keeps the
    /// block out of it.
    creative_order: Option<u16>,
    /// Which bits of the block's data byte mean what (`state::BlockState`).
    properties: &'static [PropertySpec],
    /// Which of those properties change shape or model.
    form: StateForm,
}

/// Builders for `blocks!` entries. `new` is the common case — a solid, opaque,
/// breakable full block that culls against itself — and every deviation is a
/// modifier, so an ordinary block is one line.
impl BlockSpec {
    const fn new(
        key: &'static str,
        display_name: &'static str,
        model: BlockModelSpec,
        particle_color: [f32; 3],
    ) -> Self {
        Self {
            // Placeholders; `blocks!` sets both via `with_variant`.
            id: 0,
            variant: BlockId::Air,
            key,
            key_aliases: &[],
            display_name,
            model,
            render_layer: RenderLayer::Opaque,
            cull_group: CullGroupSpec::SelfBlock,
            face_occluding: true,
            solid: true,
            skylight: SkylightSpec::FromShape,
            breakable: true,
            emitted_light: 0,
            particle_color,
            face_tint: FaceTintSpec::None,
            block_entity: None,
            random_tick: None,
            on_place: None,
            on_neighbor_changed: None,
            on_scheduled_tick: None,
            falls: false,
            creative_order: None,
            properties: &[],
            form: StateForm::FIXED,
        }
    }

    const fn cube(
        key: &'static str,
        display_name: &'static str,
        all: TextureKey,
        particle_color: [f32; 3],
    ) -> Self {
        let model = BlockModelSpec::CubeAll { all };
        Self::new(key, display_name, model, particle_color)
    }

    const fn cube_top_side_bottom(
        key: &'static str,
        display_name: &'static str,
        top: TextureKey,
        side: TextureKey,
        bottom: TextureKey,
        particle_color: [f32; 3],
    ) -> Self {
        let model = BlockModelSpec::CubeTopSideBottom { top, side, bottom };
        Self::new(key, display_name, model, particle_color)
    }

    /// A cube with one `front` face, declared on the south side: the side
    /// face the inventory preview shows. `with_front_facing` turns it in the
    /// world.
    const fn cube_front(
        key: &'static str,
        display_name: &'static str,
        top: TextureKey,
        bottom: TextureKey,
        side: TextureKey,
        front: TextureKey,
        particle_color: [f32; 3],
    ) -> Self {
        let model = BlockModelSpec::CubeFaces {
            up: top,
            down: bottom,
            north: side,
            south: front,
            east: side,
            west: side,
        };
        Self::new(key, display_name, model, particle_color)
    }

    /// A cross-shaped plant: cutout, walk-through, occludes nothing.
    const fn plant(
        key: &'static str,
        display_name: &'static str,
        texture: TextureKey,
        particle_color: [f32; 3],
    ) -> Self {
        let model = BlockModelSpec::PlantCross { texture };
        Self::new(key, display_name, model, particle_color)
            .with_layer(RenderLayer::Cutout)
            .with_cull(CullGroupSpec::None)
            .non_occluding()
            .non_solid()
    }

    const fn with_variant(mut self, variant: BlockId) -> Self {
        self.id = variant as u8;
        self.variant = variant;
        self
    }

    const fn with_aliases(mut self, key_aliases: &'static [&'static str]) -> Self {
        self.key_aliases = key_aliases;
        self
    }

    const fn with_layer(mut self, render_layer: RenderLayer) -> Self {
        self.render_layer = render_layer;
        self
    }

    const fn with_cull(mut self, cull_group: CullGroupSpec) -> Self {
        self.cull_group = cull_group;
        self
    }

    const fn non_occluding(mut self) -> Self {
        self.face_occluding = false;
        self
    }

    const fn non_solid(mut self) -> Self {
        self.solid = false;
        self
    }

    const fn with_skylight(mut self, skylight: SkylightSpec) -> Self {
        self.skylight = skylight;
        self
    }

    const fn unbreakable(mut self) -> Self {
        self.breakable = false;
        self
    }

    const fn with_light(mut self, emitted_light: u8) -> Self {
        self.emitted_light = emitted_light;
        self
    }

    const fn with_tint_all(mut self, tint_kind: u32) -> Self {
        self.face_tint = FaceTintSpec::All(tint_kind);
        self
    }

    const fn with_block_entity(mut self, key: &'static str) -> Self {
        self.block_entity = Some(key);
        self
    }

    const fn with_random_tick(mut self, callback: RandomTickFn) -> Self {
        self.random_tick = Some(callback);
        self
    }

    const fn with_creative_order(mut self, order: u16) -> Self {
        self.creative_order = Some(order);
        self
    }

    const fn with_properties(mut self, properties: &'static [PropertySpec]) -> Self {
        self.properties = properties;
        self
    }

    const fn with_on_place(mut self, on_place: OnPlaceFn) -> Self {
        self.on_place = Some(on_place);
        self
    }

    const fn with_on_neighbor_changed(mut self, hook: NeighborFn) -> Self {
        self.on_neighbor_changed = Some(hook);
        self
    }

    /// Unsupported, the block drops onto the first solid block below.
    const fn falls(mut self) -> Self {
        self.falls = true;
        self
    }

    /// `half=top` moves collision, selection, occlusion and model into the
    /// upper half of the cell. The property must be in `with_properties`.
    const fn with_shape_half(mut self, half: Property<Half>) -> Self {
        self.form = self.form.with_half(half);
        self
    }

    /// The side faces turn so the declared south face points along `facing`.
    /// The property must be in `with_properties`.
    const fn with_front_facing(mut self, facing: Property<Facing4>) -> Self {
        self.form = self.form.with_facing(facing);
        self
    }
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
            BlockModelSpec::InsetSidesTopSideBottom { inset_px, .. } => {
                CollisionShape::Aabb(inset_sides_aabb(inset_px))
            }
            _ => CollisionShape::FullCube,
        }
    }

    /// The box the player aims at, kept close to the block's *visible*
    /// geometry — deliberately separate from `collision_shape` (which may be
    /// empty for a block you can still select) and from `occlusion_shape`.
    ///
    /// The catch-all below is only correct for models that really fill the
    /// cell. `interaction_shape_tests::every_model_states_its_raycast_shape`
    /// matches on `BlockModelSpec` exhaustively, so adding a non-cube model
    /// (stairs, panes, fences, torches, rails) fails to compile until its shape
    /// is stated here rather than silently inheriting `FullCube`.
    /// Whether this block stops skylight from reaching what is below it.
    const fn blocks_skylight(self) -> bool {
        match self.skylight {
            SkylightSpec::Blocks => true,
            SkylightSpec::Transmits => false,
            SkylightSpec::FromShape => matches!(
                self.occlusion_shape().boundary_coverage(Face::Down),
                FaceCoverage::Full
            ),
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
            BlockModelSpec::InsetSidesTopSideBottom { inset_px, .. } => {
                CollisionShape::Aabb(inset_sides_aabb(inset_px))
            }
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
            BlockModelSpec::CubeFaces {
                up,
                down,
                north,
                south,
                east,
                west,
            } => {
                let mut def = BlockDef::cube_all(
                    up,
                    self.solid,
                    self.breakable,
                    self.face_occluding,
                    self.render_layer,
                    cull_group,
                );
                def.textures[Face::Down.as_usize()] = down;
                def.textures[Face::North.as_usize()] = north;
                def.textures[Face::South.as_usize()] = south;
                def.textures[Face::East.as_usize()] = east;
                def.textures[Face::West.as_usize()] = west;
                def
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
            BlockModelSpec::InsetSidesTopSideBottom {
                inset_px,
                top,
                side,
                bottom,
            } => {
                let mut def = BlockDef::cube_top_side_bottom(
                    top,
                    side,
                    bottom,
                    self.solid,
                    self.breakable,
                    self.face_occluding,
                    self.render_layer,
                    cull_group,
                );
                def.model = BlockModel::InsetSides { inset_px };
                def
            }
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

blocks! {
    Air => BlockSpec::new("minecraft:air", "Air", BlockModelSpec::None, [0.0, 0.0, 0.0])
        .with_aliases(&["core:air"])
        .with_cull(CullGroupSpec::None)
        .non_occluding()
        .non_solid()
        .unbreakable(),
    Grass => BlockSpec::new(
        "minecraft:grass_block", "Grass",
        BlockModelSpec::GrassTopSideBottom {
            top: TEX_GRASS_TOP,
            side: TEX_GRASS_SIDE,
            bottom: TEX_DIRT,
        },
        [0.35, 0.75, 0.25],
    )
    .with_aliases(&["core:grass"])
    .with_random_tick(grass_random_tick)
    .with_creative_order(4),
    Stone => BlockSpec::cube_top_side_bottom(
        "minecraft:stone", "Stone", TEX_STONE, TEX_STONE, TEX_STONE, [0.55, 0.55, 0.58],
    )
    .with_aliases(&["core:stone"])
    .with_creative_order(0),
    Dirt => BlockSpec::cube("minecraft:dirt", "Dirt", TEX_DIRT, [0.49, 0.34, 0.20])
        .with_aliases(&["core:dirt"])
        .with_creative_order(3),
    Sapling => BlockSpec::plant("minecraft:oak_sapling", "Sapling", TEX_SAPLING, [0.28, 0.88, 0.36])
        .with_aliases(&["core:sapling"])
        .with_random_tick(sapling_random_tick)
        .with_properties(&SAPLING_PROPERTIES)
        .with_creative_order(11),
    OakPlanks => BlockSpec::cube(
        "minecraft:oak_planks", "Oak Planks", TEX_OAK_PLANKS, [0.68, 0.56, 0.34],
    )
    .with_aliases(&["core:oak_planks"])
    .with_creative_order(5),
    Bedrock => BlockSpec::cube("minecraft:bedrock", "Bedrock", TEX_BEDROCK, [0.32, 0.32, 0.34])
        .with_aliases(&["core:bedrock"])
        .unbreakable(),
    WaterStill => BlockSpec::new(
        "minecraft:water", "Water (Still)",
        BlockModelSpec::LiquidTopSideBottom {
            kind: FluidKind::Water,
            top: TEX_WATER_STILL,
            side: TEX_WATER_STILL,
            bottom: TEX_WATER_STILL,
        },
        [0.26, 0.48, 0.95],
    )
    .with_aliases(&["core:water_still"])
    .with_layer(RenderLayer::Transparent)
    .with_cull(CullGroupSpec::Water)
    .non_occluding()
    .non_solid()
    .with_tint_all(TINT_WATER)
    .with_properties(&FLUID_PROPERTIES),
    WaterFlowing => BlockSpec::new(
        "minecraft:flowing_water", "Water (Flowing)",
        BlockModelSpec::LiquidTopSideBottom {
            kind: FluidKind::Water,
            top: TEX_WATER_STILL,
            side: TEX_WATER_FLOW,
            bottom: TEX_WATER_STILL,
        },
        [0.26, 0.48, 0.95],
    )
    .with_aliases(&["core:water_flowing"])
    .with_layer(RenderLayer::Transparent)
    .with_cull(CullGroupSpec::Water)
    .non_occluding()
    .non_solid()
    .with_tint_all(TINT_WATER)
    .with_properties(&FLUID_PROPERTIES),
    LavaStill => BlockSpec::new(
        "minecraft:lava", "Lava (Still)",
        BlockModelSpec::LiquidTopSideBottom {
            kind: FluidKind::Lava,
            top: TEX_LAVA_STILL,
            side: TEX_LAVA_STILL,
            bottom: TEX_LAVA_STILL,
        },
        [0.94, 0.44, 0.10],
    )
    .with_aliases(&["core:lava_still"])
    .with_cull(CullGroupSpec::Lava)
    .non_occluding()
    .non_solid()
    .unbreakable()
    .with_light(15)
    .with_properties(&FLUID_PROPERTIES),
    LavaFlowing => BlockSpec::new(
        "minecraft:flowing_lava", "Lava (Flowing)",
        BlockModelSpec::LiquidTopSideBottom {
            kind: FluidKind::Lava,
            top: TEX_LAVA_STILL,
            side: TEX_LAVA_FLOW,
            bottom: TEX_LAVA_STILL,
        },
        [0.94, 0.44, 0.10],
    )
    .with_aliases(&["core:lava_flowing"])
    .with_cull(CullGroupSpec::Lava)
    .non_occluding()
    .non_solid()
    .unbreakable()
    .with_light(15)
    .with_properties(&FLUID_PROPERTIES),
    Cobblestone => BlockSpec::cube(
        "minecraft:cobblestone", "Cobblestone", TEX_COBBLESTONE, [0.48, 0.48, 0.50],
    )
    .with_aliases(&["core:cobblestone"])
    .with_creative_order(1),
    Obsidian => BlockSpec::cube("minecraft:obsidian", "Obsidian", TEX_OBSIDIAN, [0.16, 0.10, 0.22])
        .with_aliases(&["core:obsidian"])
        .with_block_entity(CORE_FURNACE_STUB_BLOCK_ENTITY_KEY)
        .with_creative_order(42),
    Sand => BlockSpec::cube("minecraft:sand", "Sand", TEX_SAND, [0.85, 0.80, 0.55])
        .with_aliases(&["core:sand"])
        .falls()
        .with_creative_order(16),
    Bricks => BlockSpec::cube("minecraft:bricks", "Bricks", TEX_BRICKS, [0.66, 0.31, 0.25])
        .with_creative_order(2),
    OakLog => BlockSpec::cube_top_side_bottom(
        "minecraft:oak_log", "Oak Log", TEX_OAK_LOG_TOP, TEX_OAK_LOG_SIDE, TEX_OAK_LOG_TOP,
        [0.55, 0.42, 0.26],
    )
    .with_creative_order(6),
    OakLeaves => BlockSpec::cube(
        "minecraft:oak_leaves", "Oak Leaves", TEX_OAK_LEAVES, [0.24, 0.56, 0.22],
    )
    .with_layer(RenderLayer::Cutout)
    .non_occluding()
    .with_skylight(SkylightSpec::Blocks)
    .with_tint_all(TINT_GRASS_TOP)
    .with_creative_order(7),
    Glass => BlockSpec::cube("minecraft:glass", "Glass", TEX_GLASS, [0.82, 0.90, 0.92])
        .with_layer(RenderLayer::Transparent)
        .non_occluding()
        .with_skylight(SkylightSpec::Transmits)
        .with_creative_order(8),
    StoneSlab => BlockSpec::new(
        "minecraft:stone_slab", "Stone Slab",
        BlockModelSpec::SlabBottomTopSideBottom {
            top: TEX_STONE_SLAB_TOP,
            side: TEX_STONE_SLAB_SIDE,
            bottom: TEX_STONE_SLAB_TOP,
        },
        [0.70, 0.70, 0.70],
    )
    .with_properties(&SLAB_PROPERTIES)
    .with_shape_half(SLAB_HALF)
    .with_on_place(slab_on_place)
    .with_creative_order(9),
    MossyCobblestone => BlockSpec::cube(
        "minecraft:mossy_cobblestone", "Mossy Cobblestone", TEX_MOSSY_COBBLESTONE,
        [0.41, 0.47, 0.35],
    )
    .with_creative_order(10),
    Gravel => BlockSpec::cube("minecraft:gravel", "Gravel", TEX_GRAVEL, [0.55, 0.54, 0.52])
        .falls()
        .with_creative_order(17),
    Sponge => BlockSpec::cube("minecraft:sponge", "Sponge", TEX_SPONGE, [0.86, 0.80, 0.27])
        .with_creative_order(18),
    Dandelion => BlockSpec::plant(
        "minecraft:dandelion", "Dandelion", TEX_DANDELION, [0.97, 0.84, 0.12],
    )
    .with_creative_order(12),
    Poppy => BlockSpec::plant("minecraft:poppy", "Poppy", TEX_POPPY, [0.82, 0.18, 0.19])
        .with_creative_order(13),
    BrownMushroom => BlockSpec::plant(
        "minecraft:brown_mushroom", "Brown Mushroom", TEX_BROWN_MUSHROOM, [0.52, 0.36, 0.25],
    )
    .with_random_tick(mushroom_random_tick)
    .with_creative_order(14),
    RedMushroom => BlockSpec::plant(
        "minecraft:red_mushroom", "Red Mushroom", TEX_RED_MUSHROOM, [0.80, 0.23, 0.21],
    )
    .with_random_tick(mushroom_random_tick)
    .with_creative_order(15),
    CoalOre => BlockSpec::cube("minecraft:coal_ore", "Coal Ore", TEX_COAL_ORE, [0.43, 0.43, 0.43])
        .with_creative_order(37),
    IronOre => BlockSpec::cube("minecraft:iron_ore", "Iron Ore", TEX_IRON_ORE, [0.73, 0.62, 0.52])
        .with_creative_order(36),
    GoldOre => BlockSpec::cube("minecraft:gold_ore", "Gold Ore", TEX_GOLD_ORE, [0.82, 0.71, 0.32])
        .with_creative_order(35),
    IronBlock => BlockSpec::cube(
        "minecraft:iron_block", "Iron Block", TEX_IRON_BLOCK, [0.88, 0.88, 0.90],
    )
    .with_creative_order(39),
    GoldBlock => BlockSpec::cube(
        "minecraft:gold_block", "Gold Block", TEX_GOLD_BLOCK, [0.96, 0.84, 0.27],
    )
    .with_creative_order(38),
    Bookshelf => BlockSpec::cube_top_side_bottom(
        "minecraft:bookshelf", "Bookshelf", TEX_OAK_PLANKS, TEX_BOOKSHELF_SIDE, TEX_OAK_PLANKS,
        [0.61, 0.43, 0.28],
    )
    .with_creative_order(40),
    Tnt => BlockSpec::cube_top_side_bottom(
        "minecraft:tnt", "TNT", TEX_TNT_TOP, TEX_TNT_SIDE, TEX_TNT_BOTTOM, [0.83, 0.28, 0.24],
    )
    .with_creative_order(41),
    WhiteWool => BlockSpec::cube(
        "minecraft:white_wool", "White Wool", TEX_WHITE_WOOL, [0.93, 0.93, 0.93],
    )
    .with_creative_order(19),
    OrangeWool => BlockSpec::cube(
        "minecraft:orange_wool", "Orange Wool", TEX_ORANGE_WOOL, [0.86, 0.50, 0.20],
    )
    .with_creative_order(20),
    MagentaWool => BlockSpec::cube(
        "minecraft:magenta_wool", "Magenta Wool", TEX_MAGENTA_WOOL, [0.75, 0.35, 0.73],
    )
    .with_creative_order(21),
    LightBlueWool => BlockSpec::cube(
        "minecraft:light_blue_wool", "Light Blue Wool", TEX_LIGHT_BLUE_WOOL, [0.45, 0.62, 0.86],
    )
    .with_creative_order(22),
    YellowWool => BlockSpec::cube(
        "minecraft:yellow_wool", "Yellow Wool", TEX_YELLOW_WOOL, [0.90, 0.84, 0.25],
    )
    .with_creative_order(23),
    LimeWool => BlockSpec::cube(
        "minecraft:lime_wool", "Lime Wool", TEX_LIME_WOOL, [0.50, 0.78, 0.28],
    )
    .with_creative_order(24),
    PinkWool => BlockSpec::cube(
        "minecraft:pink_wool", "Pink Wool", TEX_PINK_WOOL, [0.94, 0.60, 0.70],
    )
    .with_creative_order(25),
    GrayWool => BlockSpec::cube(
        "minecraft:gray_wool", "Gray Wool", TEX_GRAY_WOOL, [0.33, 0.36, 0.38],
    )
    .with_creative_order(26),
    LightGrayWool => BlockSpec::cube(
        "minecraft:light_gray_wool", "Light Gray Wool", TEX_LIGHT_GRAY_WOOL, [0.61, 0.61, 0.61],
    )
    .with_creative_order(27),
    CyanWool => BlockSpec::cube(
        "minecraft:cyan_wool", "Cyan Wool", TEX_CYAN_WOOL, [0.27, 0.56, 0.61],
    )
    .with_creative_order(28),
    PurpleWool => BlockSpec::cube(
        "minecraft:purple_wool", "Purple Wool", TEX_PURPLE_WOOL, [0.50, 0.31, 0.70],
    )
    .with_creative_order(29),
    BlueWool => BlockSpec::cube(
        "minecraft:blue_wool", "Blue Wool", TEX_BLUE_WOOL, [0.26, 0.33, 0.69],
    )
    .with_creative_order(30),
    BrownWool => BlockSpec::cube(
        "minecraft:brown_wool", "Brown Wool", TEX_BROWN_WOOL, [0.46, 0.33, 0.22],
    )
    .with_creative_order(31),
    GreenWool => BlockSpec::cube(
        "minecraft:green_wool", "Green Wool", TEX_GREEN_WOOL, [0.37, 0.52, 0.19],
    )
    .with_creative_order(32),
    RedWool => BlockSpec::cube("minecraft:red_wool", "Red Wool", TEX_RED_WOOL, [0.67, 0.23, 0.23])
        .with_creative_order(33),
    BlackWool => BlockSpec::cube(
        "minecraft:black_wool", "Black Wool", TEX_BLACK_WOOL, [0.13, 0.13, 0.13],
    )
    .with_creative_order(34),
    Ice => BlockSpec::cube("minecraft:ice", "Ice", TEX_ICE, [0.74, 0.86, 0.96])
        .with_layer(RenderLayer::Transparent)
        .non_occluding(),
    Snow => BlockSpec::new(
        "minecraft:snow", "Snow",
        BlockModelSpec::LayerBottomTopSideBottom {
            height_px: 2,
            top: TEX_SNOW,
            side: TEX_SNOW,
            bottom: TEX_SNOW,
        },
        [0.96, 0.97, 1.0],
    )
    .non_occluding(),
    DiamondOre => BlockSpec::cube(
        "minecraft:diamond_ore", "Diamond Ore", TEX_DIAMOND_ORE, [0.46, 0.54, 0.54],
    )
    .with_creative_order(43),
    RedstoneOre => BlockSpec::cube(
        "minecraft:redstone_ore", "Redstone Ore", TEX_REDSTONE_ORE, [0.55, 0.43, 0.43],
    )
    .with_creative_order(44),
    LitRedstoneOre => BlockSpec::cube(
        "minecraft:lit_redstone_ore", "Redstone Ore (Lit)", TEX_REDSTONE_ORE, [0.55, 0.43, 0.43],
    )
    .with_light(9)
    .with_creative_order(45),
    DiamondBlock => BlockSpec::cube(
        "minecraft:diamond_block", "Diamond Block", TEX_DIAMOND_BLOCK, [0.38, 0.93, 0.90],
    )
    .with_creative_order(46),
    Clay => BlockSpec::cube("minecraft:clay", "Clay", TEX_CLAY, [0.63, 0.65, 0.70])
        .with_creative_order(47),
    SnowBlock => BlockSpec::cube("minecraft:snow_block", "Snow Block", TEX_SNOW, [0.96, 0.97, 1.0])
        .with_creative_order(48),
    // Fronts on two adjacent faces like vanilla's north/west pair, turned so the
    // inventory preview (south and east faces) shows one front and one side.
    CraftingTable => BlockSpec::new(
        "minecraft:crafting_table", "Crafting Table",
        BlockModelSpec::CubeFaces {
            up: TEX_CRAFTING_TABLE_TOP,
            down: TEX_OAK_PLANKS,
            north: TEX_CRAFTING_TABLE_SIDE,
            south: TEX_CRAFTING_TABLE_FRONT,
            east: TEX_CRAFTING_TABLE_SIDE,
            west: TEX_CRAFTING_TABLE_FRONT,
        },
        [0.55, 0.45, 0.30],
    )
    .with_creative_order(49),
    Furnace => BlockSpec::cube_front(
        "minecraft:furnace", "Furnace", TEX_FURNACE_TOP, TEX_FURNACE_TOP, TEX_FURNACE_SIDE,
        TEX_FURNACE_FRONT, [0.37, 0.37, 0.37],
    )
    .with_properties(&FURNACE_PROPERTIES)
    .with_front_facing(FURNACE_FACING)
    .with_on_place(front_toward_placer)
    .with_creative_order(50),
    LitFurnace => BlockSpec::cube_front(
        "minecraft:lit_furnace", "Furnace (Lit)", TEX_FURNACE_TOP, TEX_FURNACE_TOP,
        TEX_FURNACE_SIDE, TEX_FURNACE_FRONT_ON, [0.37, 0.37, 0.37],
    )
    .with_light(13)
    .with_properties(&FURNACE_PROPERTIES)
    .with_front_facing(FURNACE_FACING)
    .with_on_place(front_toward_placer)
    .with_creative_order(51),
    Jukebox => BlockSpec::cube_top_side_bottom(
        "minecraft:jukebox", "Jukebox", TEX_JUKEBOX_TOP, TEX_JUKEBOX_SIDE, TEX_JUKEBOX_SIDE,
        [0.36, 0.24, 0.17],
    )
    .with_creative_order(52),
    Spawner => BlockSpec::cube(
        "minecraft:spawner", "Monster Spawner", TEX_SPAWNER, [0.14, 0.17, 0.24],
    )
    .with_layer(RenderLayer::Cutout)
    .with_cull(CullGroupSpec::None)
    .non_occluding()
    .with_creative_order(53),
    SugarCane => BlockSpec::plant(
        "minecraft:sugar_cane", "Sugar Cane", TEX_SUGAR_CANE, [0.58, 0.76, 0.39],
    )
    .with_random_tick(column_plant_random_tick)
    .with_on_neighbor_changed(sugar_cane_neighbor_changed)
    .with_on_place(sugar_cane_on_place)
    .with_properties(&PLANT_AGE_PROPERTIES)
    .with_creative_order(54),
    Cactus => BlockSpec::new(
        "minecraft:cactus", "Cactus",
        BlockModelSpec::InsetSidesTopSideBottom {
            inset_px: 1,
            top: TEX_CACTUS_TOP,
            side: TEX_CACTUS_SIDE,
            bottom: TEX_CACTUS_BOTTOM,
        },
        [0.36, 0.53, 0.19],
    )
    .with_layer(RenderLayer::Cutout)
    .with_cull(CullGroupSpec::None)
    .non_occluding()
    .with_random_tick(column_plant_random_tick)
    .with_on_neighbor_changed(cactus_neighbor_changed)
    .with_on_place(cactus_on_place)
    .with_properties(&PLANT_AGE_PROPERTIES)
    .with_creative_order(55),
}

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
            | BlockModel::InsetSides { .. }
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
            | BlockModel::InsetSides { .. }
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

/// Read-only view over `BLOCK_SPECS`. The table is compile-time data, so there
/// is one registry for the whole process (`BlockRegistry::get`) and nothing to
/// build per caller.
pub(crate) struct BlockRegistry {
    specs: &'static [BlockSpec],
}

static BLOCK_REGISTRY: BlockRegistry = BlockRegistry {
    specs: &BLOCK_SPECS,
};

impl BlockRegistry {
    pub(crate) fn get() -> &'static Self {
        &BLOCK_REGISTRY
    }

    fn spec(&self, block: BlockId) -> &'static BlockSpec {
        &self.specs[block.to_u8() as usize]
    }

    fn def(&self, block: BlockId) -> BlockDef {
        self.spec(block)
            .block_def()
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

    pub(crate) fn random_tick_for(&self, block: BlockId) -> Option<RandomTickFn> {
        self.spec(block).random_tick
    }

    pub(crate) fn registered_block_keys(&self) -> Vec<NamespacedKey> {
        let mut keys: Vec<NamespacedKey> = self
            .specs
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
        for def in self.specs.iter().filter_map(|spec| spec.block_def()) {
            match def.model {
                BlockModel::Cube
                | BlockModel::SlabBottom
                | BlockModel::LayerBottom { .. }
                | BlockModel::InsetSides { .. }
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
        self.spec(block).block_def().is_some()
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
    /// Kept apart from `by_block`: one more field pushed `BlockTextureInfo`
    /// past 64 bytes and cost the greedy pass ~12 % (`greedy_ao_merge_report`).
    forms: [StateForm; 256],
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
        let mut forms = [StateForm::FIXED; 256];
        for spec in block_registry.specs {
            let Some(textures) = spec.block_def() else {
                continue;
            };
            let block = spec.variant;
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
            forms[block.to_u8() as usize] = spec.form;
        }

        Self {
            by_block,
            forms,
            missing_layer,
        }
    }

    /// `block`'s occlusion shape in the state whose data byte `data` returns.
    /// Only a shape with a top-half variant reads the form and calls `data`,
    /// so every other block pays one compare on a byte already loaded.
    #[inline(always)]
    pub(crate) fn occlusion_shape_in(
        &self,
        block: BlockId,
        info: BlockTextureInfo,
        data: impl FnOnce() -> u8,
    ) -> OcclusionShape {
        let shape = info.occlusion_shape;
        let top = shape.moved_to_top_half();
        if top != shape && self.is_top_in(block, data) {
            top
        } else {
            shape
        }
    }

    /// Whether `block` sits in the upper half of its cell in that state.
    #[inline(always)]
    pub(crate) fn is_top_in(&self, block: BlockId, data: impl FnOnce() -> u8) -> bool {
        let form = self.forms[block.to_u8() as usize];
        form.has_half() && form.is_top(data())
    }

    /// Quarter turns of `block`'s side faces in that state
    /// (`StateForm::quarter_turns`).
    #[inline(always)]
    pub(crate) fn quarter_turns_in(&self, block: BlockId, data: impl FnOnce() -> u8) -> u8 {
        let form = self.forms[block.to_u8() as usize];
        if form.has_facing() {
            form.quarter_turns(data())
        } else {
            0
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
            | BlockModel::InsetSides { .. }
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
            | BlockModel::InsetSides { .. }
            | BlockModel::Liquid { .. } => info.face_tint_kinds[face.as_usize()],
            BlockModel::CrossPlant => TINT_NONE,
        }
    }

    /// `layer_for_info` for a block whose side faces are turned
    /// `quarter_turns` (`quarter_turns_in`).
    #[inline(always)]
    pub(crate) fn layer_for_info_turned(
        &self,
        info: BlockTextureInfo,
        face: Face,
        quarter_turns: u8,
    ) -> u32 {
        self.layer_for_info(info, face.model_face_after_turns(quarter_turns))
    }

    #[inline(always)]
    pub(crate) fn tint_kind_for_info_turned(
        &self,
        info: BlockTextureInfo,
        face: Face,
        quarter_turns: u8,
    ) -> u32 {
        self.tint_kind_for_info(info, face.model_face_after_turns(quarter_turns))
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
    /// Physische Solidität (Collision).
    pub(crate) fn is_solid(self) -> bool {
        block_spec_for(self).solid
    }

    /// The block's hooks in the block-update system; falling blocks use the
    /// fall hooks.
    pub(crate) fn behavior(self) -> BlockBehavior {
        let spec = block_spec_for(self);
        if spec.falls {
            return falling::FALLING_BEHAVIOR;
        }
        BlockBehavior {
            on_neighbor_changed: spec.on_neighbor_changed,
            on_scheduled_tick: spec.on_scheduled_tick,
        }
    }

    /// Whether the data byte changes this block's shape or model (slab half,
    /// furnace facing), so a data-only change needs a remesh.
    pub(crate) fn data_changes_form(self) -> bool {
        block_spec_for(self).form != StateForm::FIXED
    }

    /// Ob der Block als Voxel-Zelle vorhanden ist (Meshing/Raycast).
    #[inline(always)]
    pub(crate) fn is_voxel_filled(self) -> bool {
        self != BlockId::Air
    }

    #[inline]
    pub(crate) fn blocks_skylight(self) -> bool {
        block_spec_for(self).blocks_skylight()
    }

    pub(crate) fn emitted_light(self) -> u8 {
        block_spec_for(self).emitted_light
    }

    pub(crate) fn has_random_tick(self) -> bool {
        block_spec_for(self).random_tick.is_some()
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

/// Whether grass stays grass with `above` directly on top of it. Shared by the
/// grass random tick and worldgen, so freshly generated terrain is already in
/// the state the random tick would settle it into.
pub(crate) fn grass_survives_under(above: BlockId) -> bool {
    !(above.blocks_skylight()
        || matches!(
            above,
            BlockId::WaterStill | BlockId::WaterFlowing | BlockId::LavaStill | BlockId::LavaFlowing
        ))
}

fn grass_random_tick(world: &mut World, pos: IVec3, _state: BlockState, tick_index: u64) {
    let above = pos + IVec3::Y;
    if !World::in_bounds(above)
        || world
            .get_chunk(World::world_to_chunk_coord(above))
            .is_none()
    {
        return;
    }

    if !grass_survives_under(world.get_block(above)) {
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

fn sapling_random_tick(world: &mut World, pos: IVec3, state: BlockState, tick_index: u64) {
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

    if SAPLING_STAGE.get(state) == 0 {
        let _ = world.set_block_state_loaded_only(pos, SAPLING_STAGE.with(state, 1));
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

    use std::collections::HashMap;

    use super::{
        BlockId, BlockRegistry, CLASSIC_PALETTE, Face, FaceCoverage, OcclusionShape, RenderLayer,
        TEX_CRAFTING_TABLE_FRONT, TEX_FURNACE_FRONT, TEX_FURNACE_FRONT_ON, block_spec_for,
        grass_random_tick, sapling_random_tick,
    };
    use crate::blocks::BlockState;
    use crate::blocks::state::SAPLING_STAGE;
    use crate::world::{CHUNK_SIZE, Chunk, ChunkCoord, World};

    #[test]
    fn only_sand_and_gravel_fall_and_they_use_the_fall_hooks() {
        for spec in super::BLOCK_SPECS.iter() {
            let falls = matches!(spec.variant, BlockId::Sand | BlockId::Gravel);
            assert_eq!(spec.falls, falls, "{}", spec.key);
            if falls {
                let behavior = spec.variant.behavior();
                assert!(behavior.on_neighbor_changed.is_some(), "{}", spec.key);
                assert!(behavior.on_scheduled_tick.is_some(), "{}", spec.key);
            }
        }
    }

    #[test]
    fn stone_slab_occludes_partially_and_stops_skylight() {
        // A lower slab covers the whole bottom of its cell, so a vertical sky
        // ray is stopped even though the sides are only half covered. This used
        // to assert the opposite, which is what let light leak under slabs.
        assert_eq!(
            BlockState::from(BlockId::StoneSlab).occlusion_shape(),
            OcclusionShape::SlabLower
        );
        assert_eq!(
            BlockState::from(BlockId::StoneSlab)
                .occlusion_shape()
                .boundary_coverage(Face::Down),
            FaceCoverage::Full
        );
        assert!(BlockId::StoneSlab.blocks_skylight());
    }

    /// The guard `docs/TODO.md` asked for: lighting and meshing must agree
    /// about every block, and every disagreement must be a named exception.
    #[test]
    fn skylight_agrees_with_the_occlusion_shape_except_for_named_blocks() {
        // Leaves shade without occluding; glass occludes nothing and passes
        // light. Anything else disagreeing means the two models drifted apart.
        let allowed_exceptions = ["minecraft:oak_leaves"];

        for raw in 0u16..=255 {
            let Ok(block) = BlockId::from_u8(raw as u8) else {
                continue;
            };
            let spec = block_spec_for(block);
            let derived_from_shape = spec.occlusion_shape().boundary_coverage(Face::Down)
                == FaceCoverage::Full;
            if block.blocks_skylight() == derived_from_shape {
                continue;
            }
            assert!(
                allowed_exceptions.contains(&spec.key),
                "{} disagrees with its occlusion shape (skylight={}, shape says {}) without being a named exception",
                spec.key,
                block.blocks_skylight(),
                derived_from_shape
            );
        }
    }

    #[test]
    fn full_opaque_cubes_all_stop_skylight() {
        // The `grass_block` regression in one assertion: a block that occludes
        // every face cannot let a sky ray through.
        for raw in 0u16..=255 {
            let Ok(block) = BlockId::from_u8(raw as u8) else {
                continue;
            };
            if BlockState::from(block).occlusion_shape() != OcclusionShape::FullCube {
                continue;
            }
            assert!(
                block.blocks_skylight(),
                "{:?} is a full occluding cube but passes skylight",
                block
            );
        }
    }

    /// The plain a1.0.17 cubes: layer, light, fronts, and their palette slots.
    #[test]
    fn plain_alpha_cubes_render_as_declared() {
        let opaque = [
            BlockId::DiamondOre,
            BlockId::RedstoneOre,
            BlockId::LitRedstoneOre,
            BlockId::DiamondBlock,
            BlockId::Clay,
            BlockId::SnowBlock,
            BlockId::CraftingTable,
            BlockId::Furnace,
            BlockId::LitFurnace,
            BlockId::Jukebox,
        ];
        for block in opaque {
            assert_eq!(block_spec_for(block).render_layer, RenderLayer::Opaque);
            assert_eq!(
                BlockState::from(block).occlusion_shape(),
                OcclusionShape::FullCube
            );
            assert!(block.is_solid() && block.blocks_skylight(), "{block:?}");
        }
        assert_eq!(BlockId::LitRedstoneOre.emitted_light(), 9);
        assert_eq!(BlockId::LitFurnace.emitted_light(), 13);
        assert_eq!(BlockId::RedstoneOre.emitted_light(), 0);

        // Cutout and see-through like glass, but still a solid block.
        let spawner = BlockId::Spawner;
        assert_eq!(block_spec_for(spawner).render_layer, RenderLayer::Cutout);
        assert_eq!(
            BlockState::from(spawner).occlusion_shape(),
            OcclusionShape::Decorative
        );
        assert!(spawner.is_solid() && !spawner.blocks_skylight());

        let registry = BlockRegistry::get();
        let front = |block| registry.texture_key_for(block, Face::South);
        assert_eq!(front(BlockId::Furnace), TEX_FURNACE_FRONT);
        assert_eq!(front(BlockId::LitFurnace), TEX_FURNACE_FRONT_ON);
        assert_eq!(front(BlockId::CraftingTable), TEX_CRAFTING_TABLE_FRONT);

        let mut appended = opaque.to_vec();
        appended.push(spawner);
        assert_eq!(&CLASSIC_PALETTE[43..43 + appended.len()], appended.as_slice());
    }

    #[test]
    fn grass_turns_to_dirt_when_covered_by_block() {
        let mut world = World::new();
        let pos = IVec3::new(0, 64, 0);
        let above = pos + IVec3::Y;
        world.set_block(pos, BlockId::Grass).unwrap();
        world.set_block(above, BlockId::Stone).unwrap();

        grass_random_tick(&mut world, pos, BlockId::Grass.into(), 0);

        assert_eq!(world.get_block(pos), BlockId::Dirt);
    }

    /// `blocks!` numbers blocks by position, and pre-manifest saves store those
    /// numbers raw. Blocks that existed before the macro must keep theirs; new
    /// blocks are appended and need no entry here.
    #[test]
    fn pre_macro_blocks_keep_their_runtime_ids() {
        let pre_macro_keys = "air grass_block stone dirt oak_sapling oak_planks bedrock water \
            flowing_water lava flowing_lava cobblestone obsidian sand bricks oak_log oak_leaves \
            glass stone_slab mossy_cobblestone gravel sponge dandelion poppy brown_mushroom \
            red_mushroom coal_ore iron_ore gold_ore iron_block gold_block bookshelf tnt \
            white_wool orange_wool magenta_wool light_blue_wool yellow_wool lime_wool pink_wool \
            gray_wool light_gray_wool cyan_wool purple_wool blue_wool brown_wool green_wool \
            red_wool black_wool ice snow";
        let mut count = 0;
        for (id, name) in pre_macro_keys.split_whitespace().enumerate() {
            let block = BlockId::from_u8(id as u8).expect("pre-macro id no longer exists");
            let key = format!("minecraft:{name}");
            assert_eq!(block_spec_for(block).key, key, "id {id} moved");
            count += 1;
        }
        assert_eq!(count, 51);
    }

    /// The bug the world change journal fixes: a random tick grew a tree
    /// straight into `World`, and nothing downstream (mesh, light, save) heard
    /// of it. Every block the tree placed must now be in the journal.
    #[test]
    fn sapling_growth_journals_every_tree_block() {
        let mut world = World::new();
        let sapling = IVec3::new(8, 1, 8);
        let ground = sapling - IVec3::Y;
        world.set_block(ground, BlockId::Grass).unwrap();
        world
            .set_block_state(sapling, SAPLING_STAGE.with(BlockId::Sapling.into(), 1))
            .unwrap();
        // Room for the tallest preset, and enough light to grow.
        world
            .chunks
            .insert(ChunkCoord { x: 0, y: 1, z: 0 }, Chunk::new_empty());
        world.set_light_packed(sapling + IVec3::Y, 0xF0);
        let mut journal = Vec::new();
        world.drain_block_changes(&mut journal);
        journal.clear();

        let mut tick = 0u64;
        while world.get_block(sapling) == BlockId::Sapling {
            assert!(tick < 10_000, "the sapling never grew");
            let state = world.get_block_state(sapling);
            sapling_random_tick(&mut world, sapling, state, tick);
            tick += 1;
        }
        world.drain_block_changes(&mut journal);

        let journaled: HashMap<IVec3, BlockId> = journal
            .iter()
            .map(|change| (change.pos, change.new))
            .collect();
        let mut tree_blocks = 0usize;
        for y in 0..2 * CHUNK_SIZE {
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    let pos = IVec3::new(x, y, z);
                    let block = world.get_block(pos);
                    if matches!(block, BlockId::OakLog | BlockId::OakLeaves) {
                        tree_blocks += 1;
                        assert_eq!(
                            journaled.get(&pos),
                            Some(&block),
                            "{block:?} at {pos:?} was placed without a journal entry"
                        );
                    }
                }
            }
        }
        assert!(tree_blocks > 1, "no tree was placed");
        assert_eq!(journaled.get(&ground), Some(&BlockId::Dirt));
        assert_eq!(
            journal.len(),
            tree_blocks + 1,
            "journal holds exactly the tree plus the grass turned to dirt"
        );
    }
}

#[cfg(test)]
mod interaction_shape_tests {
    use super::{
        BlockAabb, BlockId, BlockModelSpec, BlockState, CROSS_PLANT_SELECTION_AABB, CollisionShape,
        FULL_CUBE_AABB, LOWER_SLAB_AABB, SNOW_LAYER_AABB, block_spec_for, inset_sides_aabb,
    };

    /// Whether a model fills its whole cell visually. Exhaustive on purpose:
    /// this is the guard that stops a future non-cube model from quietly
    /// inheriting a full-cube selection box.
    fn model_fills_the_cell(model: BlockModelSpec) -> bool {
        match model {
            BlockModelSpec::None => false,
            BlockModelSpec::CubeAll { .. }
            | BlockModelSpec::CubeTopSideBottom { .. }
            | BlockModelSpec::CubeFaces { .. }
            | BlockModelSpec::GrassTopSideBottom { .. }
            // A liquid *source* fills the cell; flowing levels render lower, but
            // their height lives in block data, which a per-spec shape cannot
            // see. Water is excluded from targeting entirely
            // (`raycast::blocks_raycast_targeting`).
            | BlockModelSpec::LiquidTopSideBottom { .. } => true,
            BlockModelSpec::LayerBottomTopSideBottom { .. }
            | BlockModelSpec::PlantCross { .. }
            | BlockModelSpec::InsetSidesTopSideBottom { .. }
            | BlockModelSpec::SlabBottomTopSideBottom { .. } => false,
        }
    }

    fn all_blocks() -> Vec<BlockId> {
        (0u16..=255)
            .filter_map(|raw| BlockId::from_u8(raw as u8).ok())
            .collect()
    }

    #[test]
    fn every_model_states_its_raycast_shape() {
        for block in all_blocks() {
            let spec = block_spec_for(block);
            let shape = BlockState::from(block).raycast_shape();
            if model_fills_the_cell(spec.model) {
                continue;
            }
            assert!(
                !matches!(shape, CollisionShape::FullCube),
                "{block:?} renders smaller than its cell but is selected as a full cube"
            );
        }
    }

    #[test]
    fn selection_boxes_stay_inside_their_cell() {
        for block in all_blocks() {
            let Some(bounds) = BlockState::from(block).raycast_shape().local_bounds() else {
                continue;
            };
            let (min, max) = bounds.world_bounds(glam::IVec3::ZERO);
            for axis in 0..3 {
                assert!(
                    min[axis] >= 0.0 && max[axis] <= 1.0,
                    "{block:?} selection box leaves its cell on axis {axis}"
                );
                assert!(
                    max[axis] > min[axis],
                    "{block:?} selection box is degenerate on axis {axis}"
                );
            }
        }
    }

    #[test]
    fn partial_height_models_are_selected_at_their_rendered_height() {
        for block in all_blocks() {
            let spec = block_spec_for(block);
            let expected: Option<BlockAabb> = match spec.model {
                BlockModelSpec::None => None,
                BlockModelSpec::SlabBottomTopSideBottom { .. } => Some(LOWER_SLAB_AABB),
                BlockModelSpec::LayerBottomTopSideBottom { height_px, .. } => Some(if height_px == 2
                {
                    SNOW_LAYER_AABB
                } else {
                    BlockAabb::from_pixels([0, 0, 0], [16, height_px, 16])
                }),
                BlockModelSpec::PlantCross { .. } => Some(CROSS_PLANT_SELECTION_AABB),
                BlockModelSpec::InsetSidesTopSideBottom { inset_px, .. } => {
                    Some(inset_sides_aabb(inset_px))
                }
                _ => Some(FULL_CUBE_AABB),
            };
            assert_eq!(
                BlockState::from(block).raycast_shape().local_bounds(),
                expected,
                "{block:?} selection box does not match its rendered height"
            );
        }
    }

    #[test]
    fn empty_models_cannot_be_selected() {
        for block in all_blocks() {
            if matches!(block_spec_for(block).model, BlockModelSpec::None) {
                assert!(
                    BlockState::from(block)
                        .raycast_shape()
                        .local_bounds()
                        .is_none(),
                    "{block:?} has no geometry but offers a selection box"
                );
            }
        }
    }
}
