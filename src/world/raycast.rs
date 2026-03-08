use glam::{IVec3, Vec3};

use crate::blocks::BlockId;

use super::World;

pub(crate) const RAYCAST_MAX_DISTANCE: f32 = 8.0;

pub(crate) fn blocks_raycast_targeting(block: BlockId) -> bool {
    block.is_voxel_filled() && !super::fluids::is_water(block)
}

pub(crate) struct RaycastHit {
    pub(crate) hit_block_pos: IVec3,
    pub(crate) hit_normal: IVec3,
    pub(crate) hit_bounds_min: Vec3,
    pub(crate) hit_bounds_max: Vec3,
}

impl RaycastHit {
    pub(crate) fn place_pos(self) -> IVec3 {
        self.hit_block_pos + self.hit_normal
    }
}

#[inline(always)]
fn axis_normal(axis: usize, min_plane: bool) -> IVec3 {
    match (axis, min_plane) {
        (0, true) => IVec3::new(-1, 0, 0),
        (0, false) => IVec3::new(1, 0, 0),
        (1, true) => IVec3::new(0, -1, 0),
        (1, false) => IVec3::new(0, 1, 0),
        (2, true) => IVec3::new(0, 0, -1),
        (2, false) => IVec3::new(0, 0, 1),
        _ => IVec3::ZERO,
    }
}

fn ray_intersect_aabb(
    origin: Vec3,
    direction: Vec3,
    aabb_min: Vec3,
    aabb_max: Vec3,
) -> Option<(f32, IVec3)> {
    let mut t_enter = f32::NEG_INFINITY;
    let mut t_exit = f32::INFINITY;
    let mut enter_normal = IVec3::ZERO;
    let mut exit_normal = IVec3::ZERO;

    for axis in 0..3 {
        let origin_axis = origin[axis];
        let dir_axis = direction[axis];
        let min_axis = aabb_min[axis];
        let max_axis = aabb_max[axis];

        if dir_axis.abs() <= f32::EPSILON {
            if origin_axis < min_axis || origin_axis > max_axis {
                return None;
            }
            continue;
        }

        let inv_dir = 1.0 / dir_axis;
        let t_to_min = (min_axis - origin_axis) * inv_dir;
        let t_to_max = (max_axis - origin_axis) * inv_dir;
        let (t_near, near_normal, t_far, far_normal) = if t_to_min <= t_to_max {
            (
                t_to_min,
                axis_normal(axis, true),
                t_to_max,
                axis_normal(axis, false),
            )
        } else {
            (
                t_to_max,
                axis_normal(axis, false),
                t_to_min,
                axis_normal(axis, true),
            )
        };

        if t_near > t_enter {
            t_enter = t_near;
            enter_normal = near_normal;
        }
        if t_far < t_exit {
            t_exit = t_far;
            exit_normal = far_normal;
        }
        if t_enter > t_exit {
            return None;
        }
    }

    if t_exit < 0.0 {
        return None;
    }
    if t_enter >= 0.0 {
        Some((t_enter, enter_normal))
    } else {
        Some((t_exit, exit_normal))
    }
}

pub(crate) fn raycast_world(
    world: &World,
    origin: Vec3,
    direction: Vec3,
    max_distance: f32,
) -> Option<RaycastHit> {
    if direction.length_squared() <= f32::EPSILON {
        return None;
    }

    let dir = direction.normalize();
    let mut cell = IVec3::new(
        origin.x.floor() as i32,
        origin.y.floor() as i32,
        origin.z.floor() as i32,
    );

    let step = IVec3::new(
        if dir.x > 0.0 {
            1
        } else if dir.x < 0.0 {
            -1
        } else {
            0
        },
        if dir.y > 0.0 {
            1
        } else if dir.y < 0.0 {
            -1
        } else {
            0
        },
        if dir.z > 0.0 {
            1
        } else if dir.z < 0.0 {
            -1
        } else {
            0
        },
    );

    let t_delta = Vec3::new(
        if step.x != 0 {
            1.0 / dir.x.abs()
        } else {
            f32::INFINITY
        },
        if step.y != 0 {
            1.0 / dir.y.abs()
        } else {
            f32::INFINITY
        },
        if step.z != 0 {
            1.0 / dir.z.abs()
        } else {
            f32::INFINITY
        },
    );

    let mut t_max = Vec3::new(
        if step.x > 0 {
            ((cell.x + 1) as f32 - origin.x) / dir.x
        } else if step.x < 0 {
            (cell.x as f32 - origin.x) / dir.x
        } else {
            f32::INFINITY
        },
        if step.y > 0 {
            ((cell.y + 1) as f32 - origin.y) / dir.y
        } else if step.y < 0 {
            (cell.y as f32 - origin.y) / dir.y
        } else {
            f32::INFINITY
        },
        if step.z > 0 {
            ((cell.z + 1) as f32 - origin.z) / dir.z
        } else if step.z < 0 {
            (cell.z as f32 - origin.z) / dir.z
        } else {
            f32::INFINITY
        },
    );

    let mut t = 0.0_f32;
    const RAY_HIT_EPS: f32 = 1e-5;

    while t <= max_distance {
        let block = world.get_block(cell);
        if blocks_raycast_targeting(block) {
            let t_next = t_max.x.min(t_max.y.min(t_max.z));
            let shape = block.raycast_shape();
            if let Some((shape_min, shape_max)) = shape.world_bounds(cell) {
                if let Some((t_hit, shape_hit_normal)) =
                    ray_intersect_aabb(origin, dir, shape_min, shape_max)
                {
                    if t_hit + RAY_HIT_EPS >= t
                        && t_hit <= t_next + RAY_HIT_EPS
                        && t_hit <= max_distance + RAY_HIT_EPS
                    {
                        return Some(RaycastHit {
                            hit_block_pos: cell,
                            hit_normal: shape_hit_normal,
                            hit_bounds_min: shape_min,
                            hit_bounds_max: shape_max,
                        });
                    }
                }
            }
        }

        if t_max.x < t_max.y {
            if t_max.x < t_max.z {
                cell.x += step.x;
                t = t_max.x;
                t_max.x += t_delta.x;
            } else {
                cell.z += step.z;
                t = t_max.z;
                t_max.z += t_delta.z;
            }
        } else if t_max.y < t_max.z {
            cell.y += step.y;
            t = t_max.y;
            t_max.y += t_delta.y;
        } else {
            cell.z += step.z;
            t = t_max.z;
            t_max.z += t_delta.z;
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::BlockId;
    use crate::world::World;

    #[test]
    fn slab_does_not_raycast_hit_when_ray_only_crosses_upper_half() {
        let mut world = World::new();
        let slab_pos = IVec3::new(0, 10, 0);
        let _ = world.set_block(slab_pos, BlockId::StoneSlab);

        let hit = raycast_world(&world, Vec3::new(-1.0, 10.75, 0.5), Vec3::X, 4.0);
        assert!(hit.is_none());
    }

    #[test]
    fn slab_raycast_hits_lower_half_from_side() {
        let mut world = World::new();
        let slab_pos = IVec3::new(0, 10, 0);
        let _ = world.set_block(slab_pos, BlockId::StoneSlab);

        let hit = raycast_world(&world, Vec3::new(-1.0, 10.25, 0.5), Vec3::X, 4.0)
            .expect("expected slab hit");
        assert_eq!(hit.hit_block_pos, slab_pos);
        assert_eq!(hit.hit_normal, IVec3::new(-1, 0, 0));
    }

    #[test]
    fn slab_raycast_from_above_reports_up_normal() {
        let mut world = World::new();
        let slab_pos = IVec3::new(0, 10, 0);
        let _ = world.set_block(slab_pos, BlockId::StoneSlab);

        let hit = raycast_world(&world, Vec3::new(0.5, 12.0, 0.5), -Vec3::Y, 5.0)
            .expect("expected slab top hit");
        assert_eq!(hit.hit_block_pos, slab_pos);
        assert_eq!(hit.hit_normal, IVec3::new(0, 1, 0));
    }

    #[test]
    fn cross_plant_raycast_uses_small_selection_box() {
        let mut world = World::new();
        let plant_pos = IVec3::new(0, 10, 0);
        let _ = world.set_block(plant_pos, BlockId::Dandelion);

        let miss = raycast_world(&world, Vec3::new(-1.0, 10.5, 0.02), Vec3::X, 4.0);
        assert!(miss.is_none());

        let hit = raycast_world(&world, Vec3::new(-1.0, 10.5, 0.50), Vec3::X, 4.0)
            .expect("expected plant hit");
        assert_eq!(hit.hit_block_pos, plant_pos);
        assert_eq!(hit.hit_normal, IVec3::new(-1, 0, 0));
        assert_eq!(hit.hit_bounds_min, Vec3::new(1.0 / 16.0, 10.0, 1.0 / 16.0));
        assert_eq!(
            hit.hit_bounds_max,
            Vec3::new(15.0 / 16.0, 10.0 + 15.0 / 16.0, 15.0 / 16.0)
        );
    }
}
