//! Plant rules shared by random ticks, block-update hooks, placement and
//! worldgen: sugar cane and cactus survival and growth, and mushrooms in light.

use glam::IVec3;

use super::placement::PlaceContext;
use super::state::PLANT_AGE;
use super::{BlockId, BlockState, RandomTickContext};
use crate::world::World;
use crate::world::block_updates_system::BlockTicks;
use crate::world::fluids::is_water;

/// Sugar cane and cactus stop growing at this height; worldgen keeps to it
/// too.
pub(crate) const COLUMN_PLANT_MAX_HEIGHT: i32 = 3;
const PLANT_MAX_AGE: u8 = 15;

/// A mushroom survives up to this light level (sky or block light, whichever
/// is higher); brighter, a random tick removes it. Vanilla compares the light
/// after the day-night darkening; here the stored light counts, so the random
/// tick and the worldgen check see the same number.
pub(crate) const MUSHROOM_MAX_LIGHT: u8 = 12;

const HORIZONTAL: [IVec3; 4] = [IVec3::X, IVec3::NEG_X, IVec3::Z, IVec3::NEG_Z];

/// Sugar cane at `pos` stands on cane, or on grass, dirt or sand with water
/// right beside that block. `block_at` answers for any cell; worldgen passes
/// one that reports cells outside its column as stone, so an unknown
/// neighbor never counts as water.
pub(crate) fn sugar_cane_can_stay(block_at: impl Fn(IVec3) -> BlockId, pos: IVec3) -> bool {
    let below = pos - IVec3::Y;
    match block_at(below) {
        BlockId::SugarCane => true,
        BlockId::Grass | BlockId::Dirt | BlockId::Sand => HORIZONTAL
            .iter()
            .any(|&dir| is_water(block_at(below + dir))),
        _ => false,
    }
}

/// A cactus at `pos` stands on sand or cactus, and all four side neighbors
/// are air. Same `block_at` contract as `sugar_cane_can_stay`: stone for an
/// unknown cell keeps worldgen off column borders.
pub(crate) fn cactus_can_stay(block_at: impl Fn(IVec3) -> BlockId, pos: IVec3) -> bool {
    matches!(block_at(pos - IVec3::Y), BlockId::Sand | BlockId::Cactus)
        && HORIZONTAL
            .iter()
            .all(|&dir| block_at(pos + dir) == BlockId::Air)
}

/// Random tick for sugar cane and cactus (Vanilla `BlockReed`/`BlockCactus`):
/// the top block of a column shorter than the cap, with air above, counts its
/// age up; at the maximum a new block grows on top and the age starts over.
/// The new block checks its own footing through the block updates, so a
/// cactus that grows beside a block breaks again.
pub(super) fn column_plant_random_tick(
    world: &mut World,
    pos: IVec3,
    state: BlockState,
    _ctx: &RandomTickContext,
) {
    let above = pos + IVec3::Y;
    if !World::in_bounds(above) || world.get_block(above) != BlockId::Air {
        return;
    }
    let mut height = 1;
    while height < COLUMN_PLANT_MAX_HEIGHT && world.get_block(pos - IVec3::Y * height) == state.id
    {
        height += 1;
    }
    if height >= COLUMN_PLANT_MAX_HEIGHT {
        return;
    }
    let age = PLANT_AGE.get(state);
    if age < PLANT_MAX_AGE {
        let _ = world.set_block_state_loaded_only(pos, PLANT_AGE.with(state, age + 1));
        return;
    }
    if world
        .set_block_state(above, BlockState::default_of(state.id))
        .is_ok()
    {
        let _ = world.set_block_state_loaded_only(pos, PLANT_AGE.with(state, 0));
    }
}

pub(super) fn sugar_cane_neighbor_changed(
    world: &mut World,
    pos: IVec3,
    _state: BlockState,
    _from: IVec3,
    _ticks: &mut BlockTicks,
) {
    if !sugar_cane_can_stay(|p| world.get_block(p), pos) {
        let _ = world.set_block(pos, BlockId::Air);
    }
}

pub(super) fn cactus_neighbor_changed(
    world: &mut World,
    pos: IVec3,
    _state: BlockState,
    _from: IVec3,
    _ticks: &mut BlockTicks,
) {
    if !cactus_can_stay(|p| world.get_block(p), pos) {
        let _ = world.set_block(pos, BlockId::Air);
    }
}

/// Placement refuses a spot the plant could not stay on.
pub(super) fn sugar_cane_on_place(
    world: &World,
    ctx: &PlaceContext,
    block: BlockId,
) -> Option<BlockState> {
    sugar_cane_can_stay(|p| world.get_block(p), ctx.place_pos).then(|| block.into())
}

pub(super) fn cactus_on_place(
    world: &World,
    ctx: &PlaceContext,
    block: BlockId,
) -> Option<BlockState> {
    cactus_can_stay(|p| world.get_block(p), ctx.place_pos).then(|| block.into())
}

pub(super) fn mushroom_random_tick(
    world: &mut World,
    pos: IVec3,
    _state: BlockState,
    _ctx: &RandomTickContext,
) {
    let light = world.get_light(pos);
    if light.sky.max(light.block) > MUSHROOM_MAX_LIGHT {
        let _ = world.set_block(pos, BlockId::Air);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::LightLevel;

    fn set(world: &mut World, pos: IVec3, block: BlockId) {
        world.set_block(pos, block).unwrap();
    }

    fn tick(world: &mut World, pos: IVec3) {
        let state = world.get_block_state(pos);
        column_plant_random_tick(world, pos, state, &RandomTickContext::at(0));
    }

    /// Ticks the top block of the column at `base` until nothing changes for
    /// a full age cycle, and returns the column height.
    fn grow_fully(world: &mut World, base: IVec3, block: BlockId) -> i32 {
        for _ in 0..200 {
            let mut top = base;
            while world.get_block(top + IVec3::Y) == block {
                top += IVec3::Y;
            }
            tick(world, top);
        }
        let mut height = 0;
        while world.get_block(base + IVec3::Y * height) == block {
            height += 1;
        }
        height
    }

    #[test]
    fn sugar_cane_and_cactus_grow_every_sixteen_ticks_up_to_three() {
        let mut world = World::new();
        let cane = IVec3::new(0, 10, 0);
        set(&mut world, cane - IVec3::Y, BlockId::Sand);
        set(&mut world, cane + IVec3::new(1, -1, 0), BlockId::WaterStill);
        set(&mut world, cane, BlockId::SugarCane);
        for _ in 0..PLANT_MAX_AGE {
            tick(&mut world, cane);
        }
        assert_eq!(world.get_block(cane + IVec3::Y), BlockId::Air);
        assert_eq!(PLANT_AGE.get(world.get_block_state(cane)), PLANT_MAX_AGE);
        tick(&mut world, cane);
        assert_eq!(world.get_block(cane + IVec3::Y), BlockId::SugarCane);
        assert_eq!(PLANT_AGE.get(world.get_block_state(cane)), 0);

        assert_eq!(grow_fully(&mut world, cane, BlockId::SugarCane), 3);

        let cactus = IVec3::new(8, 10, 8);
        set(&mut world, cactus - IVec3::Y, BlockId::Sand);
        set(&mut world, cactus, BlockId::Cactus);
        assert_eq!(grow_fully(&mut world, cactus, BlockId::Cactus), 3);
    }

    #[test]
    fn plants_do_not_grow_into_a_block() {
        let mut world = World::new();
        let cactus = IVec3::new(0, 10, 0);
        set(&mut world, cactus - IVec3::Y, BlockId::Sand);
        set(&mut world, cactus, BlockId::Cactus);
        set(&mut world, cactus + IVec3::Y, BlockId::Stone);
        assert_eq!(grow_fully(&mut world, cactus, BlockId::Cactus), 1);
        assert_eq!(PLANT_AGE.get(world.get_block_state(cactus)), 0);
    }

    #[test]
    fn sugar_cane_needs_water_beside_its_ground_or_cane_below() {
        let mut world = World::new();
        let at = |world: &World, pos: IVec3| sugar_cane_can_stay(|p| world.get_block(p), pos);
        let pos = IVec3::new(0, 10, 0);
        for ground in [BlockId::Grass, BlockId::Dirt, BlockId::Sand] {
            set(&mut world, pos - IVec3::Y, ground);
            assert!(!at(&world, pos), "{ground:?} without water");
            set(&mut world, pos + IVec3::new(0, -1, 1), BlockId::WaterFlowing);
            assert!(at(&world, pos), "{ground:?} with water beside it");
            set(&mut world, pos + IVec3::new(0, -1, 1), BlockId::Air);
        }
        // Water beside the cane itself or diagonal to the ground does not count.
        set(&mut world, pos + IVec3::X, BlockId::WaterStill);
        set(&mut world, pos + IVec3::new(1, -1, 1), BlockId::WaterStill);
        assert!(!at(&world, pos));
        set(&mut world, pos - IVec3::Y, BlockId::Stone);
        set(&mut world, pos + IVec3::new(-1, -1, 0), BlockId::WaterStill);
        assert!(!at(&world, pos), "stone is no ground for cane");
        set(&mut world, pos - IVec3::Y, BlockId::SugarCane);
        assert!(at(&world, pos), "cane on cane");
    }

    #[test]
    fn cactus_needs_sand_or_cactus_below_and_air_on_all_sides() {
        let mut world = World::new();
        let at = |world: &World, pos: IVec3| cactus_can_stay(|p| world.get_block(p), pos);
        let pos = IVec3::new(0, 10, 0);
        set(&mut world, pos - IVec3::Y, BlockId::Dirt);
        assert!(!at(&world, pos));
        set(&mut world, pos - IVec3::Y, BlockId::Sand);
        assert!(at(&world, pos));
        for side in [IVec3::X, IVec3::NEG_X, IVec3::Z, IVec3::NEG_Z] {
            for neighbor in [BlockId::Dandelion, BlockId::WaterStill, BlockId::Stone] {
                set(&mut world, pos + side, neighbor);
                assert!(!at(&world, pos), "{neighbor:?} at {side:?}");
                set(&mut world, pos + side, BlockId::Air);
            }
        }
        set(&mut world, pos - IVec3::Y, BlockId::Cactus);
        assert!(at(&world, pos), "cactus on cactus");
    }

    /// Losing its footing breaks the plant, and the column above follows
    /// through the block updates. (Water taken from beside the ground block
    /// sits diagonal to the cane and wakes nothing, as in Vanilla.)
    #[test]
    fn unsupported_plants_break_through_their_neighbor_hook() {
        use crate::sim::{ActiveSimColumns, SimSystem};
        use crate::world::block_updates_system::BlockUpdatesSystem;

        let mut world = World::new();
        let mut system = BlockUpdatesSystem::new();
        let cane = IVec3::new(0, 10, 0);
        set(&mut world, cane - IVec3::Y, BlockId::Grass);
        set(&mut world, cane + IVec3::new(1, -1, 0), BlockId::WaterStill);
        for dy in 0..3 {
            set(&mut world, cane + IVec3::Y * dy, BlockId::SugarCane);
        }
        let cactus = IVec3::new(6, 10, 0);
        set(&mut world, cactus - IVec3::Y, BlockId::Sand);
        set(&mut world, cactus, BlockId::Cactus);
        world.drain_block_changes(&mut Vec::new());

        set(&mut world, cane - IVec3::Y, BlockId::Air);
        set(&mut world, cactus + IVec3::Z, BlockId::Dirt);
        let mut changes = Vec::new();
        world.drain_block_changes(&mut changes);
        system.on_block_changes(&changes);
        system.fixed_tick(&mut world, 0.05, 0, ActiveSimColumns {
            is_active: &|_, _| true,
        });

        for dy in 0..3 {
            assert_eq!(world.get_block(cane + IVec3::Y * dy), BlockId::Air, "cane {dy}");
        }
        assert_eq!(world.get_block(cactus), BlockId::Air);
    }

    #[test]
    fn placement_refuses_spots_the_plant_cannot_stay_on() {
        use super::super::placement::{PlaceContext, placement_state};
        use super::super::Face;
        use glam::Vec3;

        let mut world = World::new();
        let pos = IVec3::new(0, 10, 0);
        set(&mut world, pos - IVec3::Y, BlockId::Sand);
        let ctx = PlaceContext {
            place_pos: pos,
            face: Face::Up,
            hit_point_local: Vec3::new(0.5, 1.0, 0.5),
            yaw: 0.0,
        };
        assert_eq!(placement_state(&world, &ctx, BlockId::SugarCane), None);
        assert!(placement_state(&world, &ctx, BlockId::Cactus).is_some());
        set(&mut world, pos + IVec3::new(0, -1, 1), BlockId::WaterStill);
        assert!(placement_state(&world, &ctx, BlockId::SugarCane).is_some());
        set(&mut world, pos + IVec3::NEG_X, BlockId::Stone);
        assert_eq!(placement_state(&world, &ctx, BlockId::Cactus), None);
    }

    #[test]
    fn mushrooms_break_in_light_above_twelve() {
        let mut world = World::new();
        let pos = IVec3::new(0, 10, 0);
        for mushroom in [BlockId::BrownMushroom, BlockId::RedMushroom] {
            for (sky, block, survives) in [(12, 0, true), (0, 12, true), (13, 0, false), (0, 13, false)]
            {
                set(&mut world, pos, mushroom);
                world.set_light_packed(pos, LightLevel::new(sky, block).pack());
                mushroom_random_tick(&mut world, pos, mushroom.into(), &RandomTickContext::at(0));
                let expected = if survives { mushroom } else { BlockId::Air };
                assert_eq!(world.get_block(pos), expected, "{mushroom:?} sky {sky} block {block}");
            }
        }
    }
}
