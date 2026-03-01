use std::f32::consts::{PI, TAU};

use crate::blocks::BlockId;
use crate::world::fluids::fluid_data_make;
use crate::worldgen::java_random::JavaRandom;

use super::ChunkColumn;

pub(crate) struct RavineCarver;

impl RavineCarver {
    const RANGE: i32 = 8;

    pub(crate) fn carve_target_column(
        world_seed: u64,
        target_chunk_x: i32,
        target_chunk_z: i32,
        column: &mut ChunkColumn,
    ) {
        let world_seed_i64 = world_seed as i64;
        let mut rand = JavaRandom::new(world_seed_i64);
        let j = rand.next_long();
        let k = rand.next_long();

        for source_chunk_x in (target_chunk_x - Self::RANGE)..=(target_chunk_x + Self::RANGE) {
            for source_chunk_z in (target_chunk_z - Self::RANGE)..=(target_chunk_z + Self::RANGE) {
                let j1 = (source_chunk_x as i64).wrapping_mul(j);
                let k1 = (source_chunk_z as i64).wrapping_mul(k);
                rand.set_seed(j1 ^ k1 ^ world_seed_i64);
                Self::recursive_generate(
                    &mut rand,
                    source_chunk_x,
                    source_chunk_z,
                    target_chunk_x,
                    target_chunk_z,
                    column,
                );
            }
        }
    }

    fn recursive_generate(
        rand: &mut JavaRandom,
        source_chunk_x: i32,
        source_chunk_z: i32,
        target_chunk_x: i32,
        target_chunk_z: i32,
        column: &mut ChunkColumn,
    ) {
        if rand.next_int_bound(50) != 0 {
            return;
        }

        let x = (source_chunk_x * 16 + rand.next_int_bound(16)) as f64;
        let y_outer = rand.next_int_bound(40) + 8;
        let y = (rand.next_int_bound(y_outer) + 20) as f64;
        let z = (source_chunk_z * 16 + rand.next_int_bound(16)) as f64;
        let yaw = rand.next_float() * TAU;
        let pitch = ((rand.next_float() - 0.5_f32) * 2.0_f32) / 8.0_f32;
        let size = (rand.next_float() * 2.0_f32 + rand.next_float()) * 2.0_f32;

        Self::add_tunnel(
            rand.next_long(),
            target_chunk_x,
            target_chunk_z,
            column,
            TunnelParams {
                x,
                y,
                z,
                size,
                yaw,
                pitch,
                start_step: 0,
                max_steps: 0,
                vertical_scale: 3.0,
            },
        );
    }

    fn add_tunnel(
        seed: i64,
        target_chunk_x: i32,
        target_chunk_z: i32,
        column: &mut ChunkColumn,
        mut params: TunnelParams,
    ) {
        let mut random = JavaRandom::new(seed);
        let chunk_center_x = (target_chunk_x * 16 + 8) as f64;
        let chunk_center_z = (target_chunk_z * 16 + 8) as f64;
        let mut yaw_vel = 0.0_f32;
        let mut pitch_vel = 0.0_f32;

        if params.max_steps <= 0 {
            let i = Self::RANGE * 16 - 16;
            params.max_steps = i - random.next_int_bound(i / 4);
        }

        let mut started_from_room = false;
        if params.start_step == -1 {
            params.start_step = params.max_steps / 2;
            started_from_room = true;
        }

        let mut rs = [0.0_f32; 256];
        let mut f2 = 1.0_f32;
        for (j, entry) in rs.iter_mut().enumerate() {
            if j == 0 || random.next_int_bound(3) == 0 {
                f2 = 1.0_f32 + random.next_float() * random.next_float();
            }
            *entry = f2 * f2;
        }

        let mut step = params.start_step;
        while step < params.max_steps {
            let mut radius_xz = 1.5_f64
                + ((step as f32 * PI) / params.max_steps as f32).sin() as f64 * params.size as f64;
            let mut radius_y = radius_xz * params.vertical_scale;
            radius_xz *= (random.next_float() * 0.25_f32 + 0.75_f32) as f64;
            radius_y *= (random.next_float() * 0.25_f32 + 0.75_f32) as f64;

            let pitch_cos = params.pitch.cos();
            let pitch_sin = params.pitch.sin();
            params.x += (params.yaw.cos() * pitch_cos) as f64;
            params.y += pitch_sin as f64;
            params.z += (params.yaw.sin() * pitch_cos) as f64;

            params.pitch *= 0.7_f32;
            params.pitch += pitch_vel * 0.05_f32;
            params.yaw += yaw_vel * 0.05_f32;
            pitch_vel *= 0.8_f32;
            yaw_vel *= 0.5_f32;
            pitch_vel +=
                (random.next_float() - random.next_float()) * random.next_float() * 2.0_f32;
            yaw_vel += (random.next_float() - random.next_float()) * random.next_float() * 4.0_f32;

            if started_from_room || random.next_int_bound(4) != 0 {
                let dx = params.x - chunk_center_x;
                let dz = params.z - chunk_center_z;
                let remaining = (params.max_steps - step) as f64;
                let max_dist = (params.size + 2.0_f32 + 16.0_f32) as f64;
                if dx * dx + dz * dz - remaining * remaining > max_dist * max_dist {
                    return;
                }

                if params.x < chunk_center_x - 16.0_f64 - radius_xz * 2.0_f64
                    || params.z < chunk_center_z - 16.0_f64 - radius_xz * 2.0_f64
                    || params.x > chunk_center_x + 16.0_f64 + radius_xz * 2.0_f64
                    || params.z > chunk_center_z + 16.0_f64 + radius_xz * 2.0_f64
                {
                    step += 1;
                    if started_from_room {
                        break;
                    }
                    continue;
                }

                let chunk_x0 = target_chunk_x * 16;
                let chunk_z0 = target_chunk_z * 16;

                let mut x_min = floor_i32(params.x - radius_xz) - chunk_x0 - 1;
                let mut x_max = floor_i32(params.x + radius_xz) - chunk_x0 + 1;
                let mut y_min = floor_i32(params.y - radius_y) - 1;
                let mut y_max = floor_i32(params.y + radius_y) + 1;
                let mut z_min = floor_i32(params.z - radius_xz) - chunk_z0 - 1;
                let mut z_max = floor_i32(params.z + radius_xz) - chunk_z0 + 1;

                x_min = x_min.clamp(0, 16);
                x_max = x_max.clamp(0, 16);
                y_min = y_min.clamp(1, 248);
                y_max = y_max.clamp(1, 248);
                z_min = z_min.clamp(0, 16);
                z_max = z_max.clamp(0, 16);

                if x_min >= x_max || y_min >= y_max || z_min >= z_max {
                    step += 1;
                    if started_from_room {
                        break;
                    }
                    continue;
                }

                if Self::scan_water_abort(column, x_min, x_max, y_min, y_max, z_min, z_max) {
                    step += 1;
                    if started_from_room {
                        break;
                    }
                    continue;
                }

                for local_x in x_min..x_max {
                    let x_dist = (((chunk_x0 + local_x) as f64 + 0.5_f64) - params.x) / radius_xz;
                    let x_dist_sq = x_dist * x_dist;

                    for local_z in z_min..z_max {
                        let z_dist =
                            (((chunk_z0 + local_z) as f64 + 0.5_f64) - params.z) / radius_xz;
                        let z_dist_sq = z_dist * z_dist;
                        if x_dist_sq + z_dist_sq >= 1.0_f64 {
                            continue;
                        }

                        let mut carved_grass = false;
                        for world_y in (y_min..y_max).rev() {
                            let y_dist = ((world_y as f64 + 0.5_f64) - params.y) / radius_y;
                            let shape = (x_dist_sq + z_dist_sq) * rs[(world_y - 1) as usize] as f64
                                + (y_dist * y_dist) / 6.0_f64;
                            if shape >= 1.0_f64 {
                                continue;
                            }

                            let current = column.get_block(local_x, world_y, local_z);
                            if current == BlockId::Grass {
                                carved_grass = true;
                            }
                            if !is_replaceable(current) {
                                continue;
                            }

                            if world_y - 1 < 10 {
                                column.set_block(local_x, world_y, local_z, BlockId::LavaStill);
                                column.set_data(
                                    local_x,
                                    world_y,
                                    local_z,
                                    fluid_data_make(0, false, true),
                                );
                            } else {
                                column.set_block(local_x, world_y, local_z, BlockId::Air);
                                column.set_data(local_x, world_y, local_z, 0);
                                if carved_grass && world_y > 0 {
                                    let below_y = world_y - 1;
                                    if column.get_block(local_x, below_y, local_z) == BlockId::Dirt
                                    {
                                        column.set_block(local_x, below_y, local_z, BlockId::Grass);
                                    }
                                }
                            }
                        }
                    }
                }
            }

            step += 1;
            if started_from_room {
                break;
            }
        }
    }

    fn scan_water_abort(
        column: &ChunkColumn,
        x_min: i32,
        x_max: i32,
        y_min: i32,
        y_max: i32,
        z_min: i32,
        z_max: i32,
    ) -> bool {
        for local_x in x_min..x_max {
            for local_z in z_min..z_max {
                let mut scan_y = y_max + 1;
                while scan_y >= y_min - 1 {
                    if (0..ChunkColumn::HEIGHT).contains(&scan_y) {
                        let block = column.get_block(local_x, scan_y, local_z);
                        if is_water(block) {
                            return true;
                        }
                    }

                    if scan_y != y_min - 1
                        && local_x != x_min
                        && local_x != x_max - 1
                        && local_z != z_min
                        && local_z != z_max - 1
                    {
                        scan_y = y_min;
                    }
                    scan_y -= 1;
                }
            }
        }
        false
    }
}

#[derive(Clone, Copy)]
struct TunnelParams {
    x: f64,
    y: f64,
    z: f64,
    size: f32,
    yaw: f32,
    pitch: f32,
    start_step: i32,
    max_steps: i32,
    vertical_scale: f64,
}

#[inline]
fn floor_i32(v: f64) -> i32 {
    v.floor() as i32
}

#[inline]
fn is_water(block: BlockId) -> bool {
    matches!(block, BlockId::WaterStill | BlockId::WaterFlowing)
}

#[inline]
fn is_replaceable(block: BlockId) -> bool {
    matches!(block, BlockId::Stone | BlockId::Dirt | BlockId::Grass)
}
