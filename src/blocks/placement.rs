//! Placement hooks: a block picks its state from where and how it was placed
//! (`BlockSpec::on_place`, set in `blocks!`). They are plain functions over the
//! world and the click, so they run without a GPU and from tests.

use glam::{IVec3, Vec3};

use super::state::views::{Facing4, Half};
use super::state::{FURNACE_FACING, SLAB_HALF};
use super::{BlockId, BlockState, Face, block_spec_for};
use crate::world::World;
use crate::world::raycast::RaycastHit;

/// Picks the state of `BlockId` placed with this context, or refuses the
/// placement with `None`.
pub(crate) type OnPlaceFn = fn(&World, &PlaceContext, BlockId) -> Option<BlockState>;

/// What a placement knows beyond the block: the click and where the player
/// looks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PlaceContext {
    /// The cell the block goes into.
    pub(crate) place_pos: IVec3,
    /// The face of the clicked block the ray entered through.
    pub(crate) face: Face,
    /// Where the ray hit, relative to the clicked block's minimum corner.
    pub(crate) hit_point_local: Vec3,
    /// Camera yaw in radians, `Camera` convention: 0 looks east (+X), a
    /// quarter turn looks south (+Z).
    pub(crate) yaw: f32,
}

impl PlaceContext {
    pub(crate) fn from_hit(hit: &RaycastHit, yaw: f32) -> Self {
        Self {
            place_pos: hit.place_pos(),
            face: Face::from_normal(hit.hit_normal),
            hit_point_local: hit.hit_point - hit.hit_block_pos.as_vec3(),
            yaw,
        }
    }

    /// The horizontal direction the player looks along, from the yaw alone so
    /// it stays defined when looking straight up or down.
    pub(crate) fn look_facing(&self) -> Facing4 {
        // `Camera::forward_flat` is (cos yaw, 0, sin yaw).
        let (z, x) = self.yaw.sin_cos();
        if x.abs() >= z.abs() {
            if x >= 0.0 { Facing4::East } else { Facing4::West }
        } else if z >= 0.0 {
            Facing4::South
        } else {
            Facing4::North
        }
    }
}

/// The state `block` goes in: its `on_place` hook decides, otherwise the
/// default state. `None` means the block refuses this placement.
pub(crate) fn placement_state(
    world: &World,
    ctx: &PlaceContext,
    block: BlockId,
) -> Option<BlockState> {
    match block_spec_for(block).on_place {
        Some(on_place) => on_place(world, ctx, block),
        None => Some(BlockState::default_of(block)),
    }
}

/// Slabs: clicking a bottom face, or the upper half of a side face, puts the
/// slab in the top half of its cell; a top face or the lower half of a side
/// face puts it in the bottom half. A slab clicked onto a slab's top stays a
/// bottom slab in the cell above; slabs do not merge into a double slab.
pub(super) fn slab_on_place(
    _world: &World,
    ctx: &PlaceContext,
    block: BlockId,
) -> Option<BlockState> {
    let half = match ctx.face {
        Face::Up => Half::Bottom,
        Face::Down => Half::Top,
        Face::North | Face::South | Face::East | Face::West => {
            if ctx.hit_point_local.y > 0.5 {
                Half::Top
            } else {
                Half::Bottom
            }
        }
    };
    Some(SLAB_HALF.with(block.into(), half))
}

/// Furnaces: the front faces the player, against the look direction.
pub(super) fn front_toward_placer(
    _world: &World,
    ctx: &PlaceContext,
    block: BlockId,
) -> Option<BlockState> {
    Some(FURNACE_FACING.with(block.into(), ctx.look_facing().opposite()))
}

#[cfg(test)]
mod tests {
    use std::f32::consts::FRAC_PI_2;

    use super::*;
    use crate::camera::Camera;
    use crate::world::raycast::raycast_world;

    /// Raycasts from `origin` along `direction`, places `block` against what
    /// it hits, and returns the state it would be placed in.
    fn place_by_ray(world: &World, origin: Vec3, direction: Vec3, block: BlockId) -> BlockState {
        let hit = raycast_world(world, origin, direction, 8.0).expect("ray should hit a block");
        let ctx = PlaceContext::from_hit(&hit, 0.0);
        placement_state(world, &ctx, block).expect("placement should be accepted")
    }

    fn world_with_stone_at(pos: IVec3) -> World {
        let mut world = World::new();
        world.set_block(pos, BlockId::Stone).unwrap();
        world
    }

    #[test]
    fn hit_point_lands_on_the_clicked_face() {
        let stone = IVec3::new(0, 10, 0);
        let world = world_with_stone_at(stone);
        let hit = raycast_world(&world, Vec3::new(-2.0, 10.75, 0.25), Vec3::X, 8.0).unwrap();
        let ctx = PlaceContext::from_hit(&hit, 0.0);
        assert_eq!(ctx.face, Face::West);
        assert_eq!(ctx.place_pos, IVec3::new(-1, 10, 0));
        assert!((ctx.hit_point_local - Vec3::new(0.0, 0.75, 0.25)).length() < 1e-5);
    }

    #[test]
    fn slab_half_follows_the_hit_point_on_a_side_face() {
        let world = world_with_stone_at(IVec3::new(0, 10, 0));
        let upper = place_by_ray(&world, Vec3::new(-2.0, 10.75, 0.5), Vec3::X, BlockId::StoneSlab);
        assert_eq!(SLAB_HALF.get(upper), Half::Top);
        let lower = place_by_ray(&world, Vec3::new(-2.0, 10.25, 0.5), Vec3::X, BlockId::StoneSlab);
        assert_eq!(SLAB_HALF.get(lower), Half::Bottom);
    }

    #[test]
    fn slab_on_a_top_face_is_bottom_and_under_a_bottom_face_is_top() {
        let world = world_with_stone_at(IVec3::new(0, 10, 0));
        let on_top = place_by_ray(&world, Vec3::new(0.5, 13.0, 0.5), -Vec3::Y, BlockId::StoneSlab);
        assert_eq!(SLAB_HALF.get(on_top), Half::Bottom);
        let below = place_by_ray(&world, Vec3::new(0.5, 8.0, 0.5), Vec3::Y, BlockId::StoneSlab);
        assert_eq!(SLAB_HALF.get(below), Half::Top);
    }

    /// Clicking the upper half of a top slab's side keeps building in the top
    /// half; its selection box is the upper half of the cell.
    #[test]
    fn top_slab_side_continues_the_top_half() {
        let mut world = World::new();
        let slab = IVec3::new(0, 10, 0);
        world
            .set_block_state(slab, SLAB_HALF.with(BlockId::StoneSlab.into(), Half::Top))
            .unwrap();
        let next = place_by_ray(&world, Vec3::new(-2.0, 10.6, 0.5), Vec3::X, BlockId::StoneSlab);
        assert_eq!(SLAB_HALF.get(next), Half::Top);
        assert!(
            raycast_world(&world, Vec3::new(-2.0, 10.25, 0.5), Vec3::X, 4.0).is_none(),
            "the empty lower half of a top slab is not selectable"
        );
    }

    #[test]
    fn furnace_front_faces_the_player_for_each_yaw() {
        let world = World::new();
        for (yaw, front) in [
            (0.0, Facing4::West),
            (FRAC_PI_2, Facing4::North),
            (2.0 * FRAC_PI_2, Facing4::East),
            (-FRAC_PI_2, Facing4::South),
        ] {
            // The context's yaw means what the camera's does: the expected
            // front points back along the camera's view.
            let camera = Camera {
                yaw,
                ..Camera::new(Vec3::ZERO)
            };
            let front_dir = match front {
                Facing4::North => -Vec3::Z,
                Facing4::South => Vec3::Z,
                Facing4::East => Vec3::X,
                Facing4::West => -Vec3::X,
            };
            assert!(
                camera.forward_flat().dot(front_dir) < -0.99,
                "yaw {yaw}: {front:?} does not face back along the view"
            );
            let ctx = PlaceContext {
                place_pos: IVec3::ZERO,
                face: Face::Up,
                hit_point_local: Vec3::new(0.5, 1.0, 0.5),
                yaw: camera.yaw,
            };
            for block in [BlockId::Furnace, BlockId::LitFurnace] {
                let state = placement_state(&world, &ctx, block).unwrap();
                assert_eq!(FURNACE_FACING.get(state), front, "yaw {yaw}, {block:?}");
            }
        }
    }

    #[test]
    fn blocks_without_a_hook_place_their_default_state() {
        let world = World::new();
        let ctx = PlaceContext {
            place_pos: IVec3::ZERO,
            face: Face::Down,
            hit_point_local: Vec3::new(0.5, 0.0, 0.5),
            yaw: 1.0,
        };
        assert_eq!(
            placement_state(&world, &ctx, BlockId::Stone),
            Some(BlockState::from(BlockId::Stone))
        );
    }
}
