//! Snow layers and ice: snow piles up while it snows, snow placed on snow adds
//! a layer, ice melts near a light source.

use glam::IVec3;

use super::placement::PlaceContext;
use super::state::{FLUID_SOURCE, SNOW_LAYERS};
use super::{BlockId, BlockState, RandomTickContext};
use crate::world::{PrecipitationKind, WORLD_MAX_Y, World};

/// The most layers a snow layer block holds.
pub(crate) const SNOW_MAX_LAYERS: u8 = 8;

/// Snowfall stops adding layers at this height. Alpha a1.0.17 let snow cover
/// grow, but how high it went is not documented (`docs/TODO.md`); 4 layers is
/// half a block.
pub(crate) const SNOW_GROWTH_MAX_LAYERS: u8 = 4;

/// Ice melts at this block light or above (Vanilla: above 11).
pub(crate) const ICE_MELT_MIN_BLOCK_LIGHT: u8 = 12;

/// Snow on snow adds a layer; elsewhere snow starts with one.
/// (`placement::PlaceContext::for_placing` makes a snow layer below the
/// maximum the target when snow is placed on it.)
pub(super) fn snow_on_place(
    world: &World,
    ctx: &PlaceContext,
    block: BlockId,
) -> Option<BlockState> {
    let existing = world.get_block_state(ctx.place_pos);
    let layers = if existing.id == block {
        SNOW_LAYERS.get(existing) + 1
    } else {
        1
    };
    Some(SNOW_LAYERS.with(block.into(), layers.min(SNOW_MAX_LAYERS)))
}

/// While it snows, snow under the open sky gains a layer, up to
/// `SNOW_GROWTH_MAX_LAYERS`.
pub(super) fn snow_random_tick(
    world: &mut World,
    pos: IVec3,
    state: BlockState,
    ctx: &RandomTickContext,
) {
    if ctx.precipitation != Some(PrecipitationKind::Snow) {
        return;
    }
    let layers = SNOW_LAYERS.get(state);
    if layers >= SNOW_GROWTH_MAX_LAYERS || !sees_sky(world, pos) {
        return;
    }
    let _ = world.set_block_state_loaded_only(pos, SNOW_LAYERS.with(state, layers + 1));
}

/// Nothing but air above `pos`, up to the top of the world.
fn sees_sky(world: &World, pos: IVec3) -> bool {
    ((pos.y + 1)..=WORLD_MAX_Y).all(|y| world.get_block(IVec3::new(pos.x, y, pos.z)) == BlockId::Air)
}

/// Ice in block light above 11 melts to a water source (Vanilla `BlockIce`),
/// which the fluid simulation then picks up.
pub(super) fn ice_random_tick(
    world: &mut World,
    pos: IVec3,
    _state: BlockState,
    _ctx: &RandomTickContext,
) {
    if world.get_light(pos).block >= ICE_MELT_MIN_BLOCK_LIGHT {
        let water = FLUID_SOURCE.with(BlockId::WaterStill.into(), true);
        let _ = world.set_block_state_loaded_only(pos, water);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::placement::{PlaceContext, can_place_into, placement_state};
    use crate::world::LightLevel;
    use crate::world::raycast::raycast_world;
    use glam::Vec3;

    fn snowing(tick_index: u64) -> RandomTickContext {
        RandomTickContext {
            tick_index,
            precipitation: Some(PrecipitationKind::Snow),
        }
    }

    fn layers_at(world: &World, pos: IVec3) -> u8 {
        let state = world.get_block_state(pos);
        assert_eq!(state.id, BlockId::Snow);
        SNOW_LAYERS.get(state)
    }

    /// Clicks the top of the block at `pos` from above with `block` in hand
    /// and places it like the game does.
    fn place_from_above(world: &mut World, pos: IVec3, block: BlockId) {
        let origin = pos.as_vec3() + Vec3::new(0.5, 3.0, 0.5);
        let hit = raycast_world(world, origin, -Vec3::Y, 8.0).expect("ray should hit");
        let ctx = PlaceContext::for_placing(world, &hit, 0.0, block);
        assert!(can_place_into(world.get_block_state(ctx.place_pos), block));
        let state = placement_state(world, &ctx, block).expect("placement accepted");
        world.set_block_state(ctx.place_pos, state).unwrap();
    }

    #[test]
    fn snow_placed_on_snow_adds_a_layer_up_to_eight() {
        let mut world = World::new();
        let ground = IVec3::new(0, 10, 0);
        world.set_block(ground, BlockId::Stone).unwrap();
        let snow = ground + IVec3::Y;
        place_from_above(&mut world, ground, BlockId::Snow);
        assert_eq!(layers_at(&world, snow), 1);
        for expected in 2..=SNOW_MAX_LAYERS {
            place_from_above(&mut world, snow, BlockId::Snow);
            assert_eq!(layers_at(&world, snow), expected);
        }
        // A full layer block takes no more: the next snow starts on top.
        place_from_above(&mut world, snow, BlockId::Snow);
        assert_eq!(layers_at(&world, snow), SNOW_MAX_LAYERS);
        assert_eq!(layers_at(&world, snow + IVec3::Y), 1);
        // Other blocks do not merge into snow.
        assert!(!can_place_into(world.get_block_state(snow + IVec3::Y), BlockId::Stone));
    }

    #[test]
    fn snow_height_collision_and_outline_follow_the_layers() {
        for layers in 1..=SNOW_MAX_LAYERS {
            let state = SNOW_LAYERS.with(BlockId::Snow.into(), layers);
            let expected = Some(f32::from(layers) * 2.0 / 16.0);
            let top = |shape: crate::blocks::CollisionShape| {
                shape.world_bounds(IVec3::ZERO).map(|(_, max)| max.y)
            };
            assert_eq!(top(state.collision_shape()), expected, "{layers} collision");
            assert_eq!(top(state.raycast_shape()), expected, "{layers} outline");
        }
    }

    #[test]
    fn snow_grows_only_while_snowing_under_the_sky_up_to_the_cap() {
        let mut world = World::new();
        let snow = IVec3::new(0, 11, 0);
        world.set_block(snow - IVec3::Y, BlockId::Stone).unwrap();
        world.set_block(snow, BlockId::Snow).unwrap();
        let tick = |world: &mut World, ctx: RandomTickContext| {
            let state = world.get_block_state(snow);
            snow_random_tick(world, snow, state, &ctx);
        };

        tick(&mut world, RandomTickContext::at(0));
        let raining = RandomTickContext {
            tick_index: 1,
            precipitation: Some(PrecipitationKind::Rain),
        };
        tick(&mut world, raining);
        assert_eq!(layers_at(&world, snow), 1, "no growth without snowfall");

        for i in 0..20 {
            tick(&mut world, snowing(i));
        }
        assert_eq!(layers_at(&world, snow), SNOW_GROWTH_MAX_LAYERS);

        let covered = IVec3::new(4, 11, 0);
        world.set_block(covered - IVec3::Y, BlockId::Stone).unwrap();
        world.set_block(covered, BlockId::Snow).unwrap();
        world.set_block(covered + IVec3::Y * 20, BlockId::OakLeaves).unwrap();
        let state = world.get_block_state(covered);
        snow_random_tick(&mut world, covered, state, &snowing(0));
        assert_eq!(layers_at(&world, covered), 1, "no growth under a roof");
    }

    #[test]
    fn ice_melts_beside_lava_but_not_in_the_dark() {
        let mut world = World::new();
        let lit = IVec3::new(0, 10, 0);
        let dark = IVec3::new(8, 10, 0);
        world.set_block(lit, BlockId::Ice).unwrap();
        world.set_block(lit + IVec3::X, BlockId::LavaStill).unwrap();
        world.set_block(dark, BlockId::Ice).unwrap();
        let coords: Vec<_> = world.chunks.keys().copied().collect();
        world.lighting_on_chunks_added(&coords);
        assert!(world.get_light(lit).block > 11, "lava lights the ice");

        for pos in [lit, dark] {
            ice_random_tick(&mut world, pos, BlockId::Ice.into(), &RandomTickContext::at(0));
        }
        let melted = world.get_block_state(lit);
        assert_eq!(melted.id, BlockId::WaterStill);
        assert!(FLUID_SOURCE.get(melted), "melted ice is a water source");
        assert_eq!(world.get_block(dark), BlockId::Ice);

        // The threshold itself: 11 stays, 12 melts.
        for (block_light, survives) in [(11, true), (12, false)] {
            world.set_block(dark, BlockId::Ice).unwrap();
            world.set_light_packed(dark, LightLevel::new(15, block_light).pack());
            ice_random_tick(&mut world, dark, BlockId::Ice.into(), &RandomTickContext::at(0));
            assert_eq!(world.get_block(dark) == BlockId::Ice, survives, "block light {block_light}");
        }
    }
}
