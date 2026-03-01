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
}

impl RaycastHit {
    pub(crate) fn place_pos(self) -> IVec3 {
        self.hit_block_pos + self.hit_normal
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
    let mut hit_normal = IVec3::ZERO;

    while t <= max_distance {
        if blocks_raycast_targeting(world.get_block(cell)) {
            return Some(RaycastHit {
                hit_block_pos: cell,
                hit_normal,
            });
        }

        if t_max.x < t_max.y {
            if t_max.x < t_max.z {
                cell.x += step.x;
                t = t_max.x;
                t_max.x += t_delta.x;
                hit_normal = IVec3::new(-step.x, 0, 0);
            } else {
                cell.z += step.z;
                t = t_max.z;
                t_max.z += t_delta.z;
                hit_normal = IVec3::new(0, 0, -step.z);
            }
        } else if t_max.y < t_max.z {
            cell.y += step.y;
            t = t_max.y;
            t_max.y += t_delta.y;
            hit_normal = IVec3::new(0, -step.y, 0);
        } else {
            cell.z += step.z;
            t = t_max.z;
            t_max.z += t_delta.z;
            hit_normal = IVec3::new(0, 0, -step.z);
        }
    }

    None
}
