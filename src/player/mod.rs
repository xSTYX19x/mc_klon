use glam::{IVec3, Vec3};
use std::time::Instant;

use crate::camera::Camera;
use crate::config::player::{
    COLLISION_EPS, COLLISION_SUBSTEP, EYE_HEIGHT, FLUID_BOB_IMPULSE_PER_TICK,
    FLUID_DIVE_ACCEL_PER_TICK, FLUID_GRAVITY_PER_TICK, FLUID_INPUT_ACCEL_PER_TICK,
    FLUID_JUMP_IMPULSE_PER_TICK, FLUID_LEDGE_ASSIST_STEP_CHECK, FLUID_LEDGE_POP_VY_PER_TICK,
    FLUID_SURFACE_CONTACT_EPS, FLUID_SWIM_SPEED_ATTR_BASE, GROUND_CHECK_DISTANCE,
    JUMP_HOLD_COOLDOWN_SECONDS, LAVA_FLUID_DRAG_PER_TICK, LAVA_SWIM_SPEED_CAP, PLAYER_HEIGHT,
    PLAYER_RADIUS, PLAYER_TUNING_MINECRAFT, STEP_EPS, STEP_GROUNDED_GRACE_SECONDS, STEP_HEIGHT,
    SWIM_ENTER_DEPTH, SWIM_POSE_EYE_HEIGHT, SWIM_POSE_HEIGHT, WADING_ACCEL_MULT, WADING_EXTRA_DRAG,
    WADING_SPEED_MULT, WATER_FLUID_DRAG_PER_TICK, WATER_SPRINT_SWIM_SPEED_CAP,
    WATER_SWIM_SPEED_CAP,
};
use crate::config::render::{WATER_OVERLAY_ALPHA_MAX, WATER_OVERLAY_ALPHA_MIN};
use crate::input::CameraController;
use crate::meshing::{fluid_kind_at_point, liquid_height_at_point};
use crate::world::fluids::{flow_velocity_at_point, fluid_kind};
use crate::world::{FluidKind, World};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MoveMode {
    Flight,
    Walk,
    Swim { kind: FluidKind },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlayerMode {
    Survival,
    Creative,
}

/// Player-Transform in Weltkoordinaten.
/// `feet_position` beschreibt die Unterkante des Players.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Player {
    pub(crate) feet_position: Vec3,
    pub(crate) mode: MoveMode,
    pub(crate) player_mode: PlayerMode,
    pub(crate) _velocity_y: f32,
    pub(crate) walk_velocity: Vec3,
    pub(crate) swim_velocity: Vec3,
    pub(crate) bob_phase: f32,
    pub(crate) bob_anchor_y: f32,
    pub(crate) was_at_surface: bool,
    pub(crate) time_since_grounded: f32,
    pub(crate) on_ground: bool,
    pub(crate) sprint_swim_active: bool,
    pub(crate) is_swim_prone: bool,
    pub(crate) food_level: u8,
    pub(crate) has_blindness: bool,
    pub(crate) jump_hold_cooldown_seconds: f32,
}

impl Player {
    pub(crate) fn from_eye_position(eye_position: Vec3) -> Self {
        Self {
            feet_position: eye_position - Vec3::Y * EYE_HEIGHT,
            mode: MoveMode::Flight,
            player_mode: PlayerMode::Creative,
            _velocity_y: 0.0,
            walk_velocity: Vec3::ZERO,
            swim_velocity: Vec3::ZERO,
            bob_phase: 0.0,
            bob_anchor_y: eye_position.y - EYE_HEIGHT,
            was_at_surface: false,
            time_since_grounded: f32::INFINITY,
            on_ground: false,
            sprint_swim_active: false,
            is_swim_prone: false,
            food_level: 20,
            has_blindness: false,
            jump_hold_cooldown_seconds: 0.0,
        }
    }

    pub(crate) fn collision_height(self) -> f32 {
        if self.is_swim_prone {
            SWIM_POSE_HEIGHT
        } else {
            PLAYER_HEIGHT
        }
    }

    pub(crate) fn eye_height(self) -> f32 {
        if self.is_swim_prone {
            SWIM_POSE_EYE_HEIGHT
        } else {
            EYE_HEIGHT
        }
    }

    pub(crate) fn eye_position(self) -> Vec3 {
        self.feet_position + Vec3::Y * self.eye_height()
    }

    pub(crate) fn can_sprint_swim(self) -> bool {
        self.food_level > 6 && !self.has_blindness
    }
}

/// =======================================
/// Input / Controller (WASD + Maus + ESC)
/// =======================================
/// Hier passiert:
/// - Flight-WASD bewegt auf XZ (yaw-basiert, pitch-unabhängig)
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CollisionFlags {
    pub(crate) hit_x: bool,
    pub(crate) hit_z: bool,
    pub(crate) hit_y_up: bool,
    pub(crate) hit_y_down: bool,
}

pub(crate) fn player_aabb_with_height(feet_pos: Vec3, height: f32) -> (Vec3, Vec3) {
    (
        feet_pos + Vec3::new(-PLAYER_RADIUS, 0.0, -PLAYER_RADIUS),
        feet_pos + Vec3::new(PLAYER_RADIUS, height.max(0.0), PLAYER_RADIUS),
    )
}

#[allow(dead_code)]
pub(crate) fn player_aabb(feet_pos: Vec3) -> (Vec3, Vec3) {
    player_aabb_with_height(feet_pos, PLAYER_HEIGHT)
}

#[inline(always)]
fn aabb_overlaps(min_a: Vec3, max_a: Vec3, min_b: Vec3, max_b: Vec3) -> bool {
    min_a.x < max_b.x
        && max_a.x > min_b.x
        && min_a.y < max_b.y
        && max_a.y > min_b.y
        && min_a.z < max_b.z
        && max_a.z > min_b.z
}

/// Prüft, ob eine AABB irgendeine solide Voxelzelle überlappt.
pub(crate) fn aabb_collides(world: &World, aabb_min: Vec3, aabb_max: Vec3) -> bool {
    let min_x = aabb_min.x.floor() as i32;
    let min_y = aabb_min.y.floor() as i32;
    let min_z = aabb_min.z.floor() as i32;
    let max_x = (aabb_max.x - COLLISION_EPS).floor() as i32;
    let max_y = (aabb_max.y - COLLISION_EPS).floor() as i32;
    let max_z = (aabb_max.z - COLLISION_EPS).floor() as i32;

    for y in min_y..=max_y {
        for z in min_z..=max_z {
            for x in min_x..=max_x {
                let block_pos = IVec3::new(x, y, z);
                if !World::in_bounds(block_pos) {
                    return true;
                }

                let block = world.get_block(block_pos);
                let shape = block.collision_shape();
                let Some((block_min, block_max)) = shape.world_bounds(block_pos) else {
                    continue;
                };
                if aabb_overlaps(aabb_min, aabb_max, block_min, block_max) {
                    return true;
                }
            }
        }
    }
    false
}

pub(crate) fn move_axis_with_collisions_for_height(
    world: &World,
    mut pos: Vec3,
    axis: usize,
    delta_axis: f32,
    hit_flag: &mut bool,
    player_height: f32,
) -> Vec3 {
    if delta_axis.abs() <= f32::EPSILON {
        return pos;
    }

    let steps = ((delta_axis.abs() / COLLISION_SUBSTEP).ceil().max(1.0)) as i32;
    let step = delta_axis / (steps as f32);

    for _ in 0..steps {
        let mut test_pos = pos;
        test_pos[axis] += step;
        let (test_min, test_max) = player_aabb_with_height(test_pos, player_height);
        if !aabb_collides(world, test_min, test_max) {
            pos = test_pos;
            continue;
        }

        // Kollision innerhalb des aktuellen Substeps:
        // wir suchen die maximal noch erlaubte Teilstrecke per Binärsuche.
        let mut lo = 0.0_f32;
        let mut hi = 1.0_f32;
        for _ in 0..10 {
            let mid = (lo + hi) * 0.5;
            let mut mid_pos = pos;
            mid_pos[axis] += step * mid;
            let (mid_min, mid_max) = player_aabb_with_height(mid_pos, player_height);
            if aabb_collides(world, mid_min, mid_max) {
                hi = mid;
            } else {
                lo = mid;
            }
        }

        pos[axis] += step * lo;
        *hit_flag = true;
        break;
    }

    pos
}

#[allow(dead_code)]
pub(crate) fn move_axis_with_collisions(
    world: &World,
    pos: Vec3,
    axis: usize,
    delta_axis: f32,
    hit_flag: &mut bool,
) -> Vec3 {
    move_axis_with_collisions_for_height(world, pos, axis, delta_axis, hit_flag, PLAYER_HEIGHT)
}

/// Shared Bewegung mit Axis-Separation:
/// X -> Z -> Y, damit diagonales Sliding an Wänden robust bleibt.
pub(crate) fn move_with_collisions_for_height(
    world: &World,
    feet_pos: Vec3,
    delta: Vec3,
    player_height: f32,
) -> (Vec3, CollisionFlags) {
    let mut pos = feet_pos;
    let mut flags = CollisionFlags::default();
    let mut hit_y = false;

    pos = move_axis_with_collisions_for_height(
        world,
        pos,
        0,
        delta.x,
        &mut flags.hit_x,
        player_height,
    );
    pos = move_axis_with_collisions_for_height(
        world,
        pos,
        2,
        delta.z,
        &mut flags.hit_z,
        player_height,
    );
    pos = move_axis_with_collisions_for_height(world, pos, 1, delta.y, &mut hit_y, player_height);
    if hit_y {
        if delta.y > 0.0 {
            flags.hit_y_up = true;
        } else if delta.y < 0.0 {
            flags.hit_y_down = true;
        }
    }

    (pos, flags)
}

pub(crate) fn move_with_collisions(
    world: &World,
    feet_pos: Vec3,
    delta: Vec3,
) -> (Vec3, CollisionFlags) {
    move_with_collisions_for_height(world, feet_pos, delta, PLAYER_HEIGHT)
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CharacterMoveResult {
    pub(crate) feet_pos: Vec3,
    pub(crate) collision: CollisionFlags,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) stepped_up: bool,
}

pub(crate) fn horizontal_progress_squared(start: Vec3, end: Vec3) -> f32 {
    let delta = Vec3::new(end.x - start.x, 0.0, end.z - start.z);
    delta.length_squared()
}

pub(crate) fn move_horizontal_with_step_up_with_height(
    world: &World,
    feet_pos: Vec3,
    delta_xz: Vec3,
    allow_step_up: bool,
    player_height: f32,
) -> CharacterMoveResult {
    let (flat_pos, flat_flags) =
        move_with_collisions_for_height(world, feet_pos, delta_xz, player_height);
    let base_result = CharacterMoveResult {
        feet_pos: flat_pos,
        collision: flat_flags,
        stepped_up: false,
    };

    if delta_xz.x.abs() <= f32::EPSILON && delta_xz.z.abs() <= f32::EPSILON {
        return base_result;
    }
    if !(flat_flags.hit_x || flat_flags.hit_z) {
        return base_result;
    }
    if !allow_step_up {
        return base_result;
    }

    let (after_up, up_flags) = move_with_collisions_for_height(
        world,
        feet_pos,
        Vec3::new(0.0, STEP_HEIGHT, 0.0),
        player_height,
    );
    if up_flags.hit_y_up || after_up.y < feet_pos.y + STEP_HEIGHT - STEP_EPS {
        return base_result;
    }

    let (after_step_xz, step_xz_flags) =
        move_with_collisions_for_height(world, after_up, delta_xz, player_height);
    let normal_progress = horizontal_progress_squared(feet_pos, flat_pos);
    let stepped_progress = horizontal_progress_squared(feet_pos, after_step_xz);
    if stepped_progress <= normal_progress + STEP_EPS * STEP_EPS {
        return base_result;
    }

    let (after_down, down_flags) = move_with_collisions_for_height(
        world,
        after_step_xz,
        Vec3::new(0.0, -STEP_HEIGHT, 0.0),
        player_height,
    );

    CharacterMoveResult {
        feet_pos: after_down,
        collision: CollisionFlags {
            hit_x: step_xz_flags.hit_x,
            hit_z: step_xz_flags.hit_z,
            hit_y_up: up_flags.hit_y_up,
            hit_y_down: down_flags.hit_y_down,
        },
        stepped_up: true,
    }
}

#[allow(dead_code)]
pub(crate) fn move_horizontal_with_step_up(
    world: &World,
    feet_pos: Vec3,
    delta_xz: Vec3,
    allow_step_up: bool,
) -> CharacterMoveResult {
    move_horizontal_with_step_up_with_height(
        world,
        feet_pos,
        delta_xz,
        allow_step_up,
        PLAYER_HEIGHT,
    )
}

#[cfg(test)]
pub(crate) fn move_character_with_step_up(
    world: &World,
    feet_pos: Vec3,
    delta: Vec3,
) -> CharacterMoveResult {
    let mut result = move_horizontal_with_step_up_with_height(
        world,
        feet_pos,
        Vec3::new(delta.x, 0.0, delta.z),
        true,
        PLAYER_HEIGHT,
    );
    let (after_vertical, vertical_flags) = move_with_collisions_for_height(
        world,
        result.feet_pos,
        Vec3::new(0.0, delta.y, 0.0),
        PLAYER_HEIGHT,
    );
    result.feet_pos = after_vertical;
    result.collision.hit_y_up |= vertical_flags.hit_y_up;
    result.collision.hit_y_down |= vertical_flags.hit_y_down;
    result
}

#[allow(dead_code)]
pub(crate) fn move_character_with_conditional_step_up(
    world: &World,
    feet_pos: Vec3,
    delta: Vec3,
    allow_step_up: bool,
) -> CharacterMoveResult {
    move_character_with_conditional_step_up_with_height(
        world,
        feet_pos,
        delta,
        allow_step_up,
        PLAYER_HEIGHT,
    )
}

pub(crate) fn move_character_with_conditional_step_up_with_height(
    world: &World,
    feet_pos: Vec3,
    delta: Vec3,
    allow_step_up: bool,
    player_height: f32,
) -> CharacterMoveResult {
    let mut result = move_horizontal_with_step_up_with_height(
        world,
        feet_pos,
        Vec3::new(delta.x, 0.0, delta.z),
        allow_step_up,
        player_height,
    );
    let (after_vertical, vertical_flags) = move_with_collisions_for_height(
        world,
        result.feet_pos,
        Vec3::new(0.0, delta.y, 0.0),
        player_height,
    );
    result.feet_pos = after_vertical;
    result.collision.hit_y_up |= vertical_flags.hit_y_up;
    result.collision.hit_y_down |= vertical_flags.hit_y_down;
    result
}

pub(crate) fn can_attempt_step_up(player: &Player, vertical_velocity: f32) -> bool {
    vertical_velocity <= 0.0
        && (player.on_ground || player.time_since_grounded < STEP_GROUNDED_GRACE_SECONDS)
}

pub(crate) fn update_grounded_timer(player: &mut Player, dt: f32) {
    if player.on_ground {
        player.time_since_grounded = 0.0;
    } else {
        player.time_since_grounded += dt.max(0.0);
    }
}

pub(crate) fn jump_velocity() -> f32 {
    PLAYER_TUNING_MINECRAFT.jump_initial_vy_per_tick * PLAYER_TUNING_MINECRAFT.mc_ticks_per_second
}

/// Korrigiert kleine Bodenabstände und liefert den Ground-State.
pub(crate) fn snap_to_ground_with_height(
    world: &World,
    feet_pos: Vec3,
    player_height: f32,
) -> (Vec3, bool) {
    let (snapped_pos, flags) = move_with_collisions_for_height(
        world,
        feet_pos,
        Vec3::new(0.0, -GROUND_CHECK_DISTANCE, 0.0),
        player_height,
    );
    if flags.hit_y_down {
        (snapped_pos, true)
    } else {
        (feet_pos, false)
    }
}

#[allow(dead_code)]
pub(crate) fn snap_to_ground(world: &World, feet_pos: Vec3) -> (Vec3, bool) {
    snap_to_ground_with_height(world, feet_pos, PLAYER_HEIGHT)
}

fn exp_approach(current: f32, target: f32, rate: f32, dt: f32) -> f32 {
    let alpha = 1.0 - (-rate.max(0.0) * dt.max(0.0)).exp();
    current + (target - current) * alpha
}

fn integrate_air_vertical_motion(vy_blocks_per_sec: f32, dt: f32) -> (f32, f32) {
    let tuning = PLAYER_TUNING_MINECRAFT;
    let ticks = (dt.max(0.0) * tuning.mc_ticks_per_second).max(0.0);
    if ticks <= f32::EPSILON {
        return (0.0, vy_blocks_per_sec);
    }

    let a = tuning.air_drag_per_tick;
    let g = tuning.gravity_per_tick;
    let c = -a * g;
    let v0 = vy_blocks_per_sec / tuning.mc_ticks_per_second;

    let one_minus_a = 1.0 - a;
    if one_minus_a.abs() <= 1e-6 {
        let v_n = v0 + c * ticks;
        let displacement = v0 * ticks + 0.5 * c * ticks * ticks;
        return (displacement, v_n * tuning.mc_ticks_per_second);
    }

    let a_pow_n = a.powf(ticks);
    let geom = (1.0 - a_pow_n) / one_minus_a;
    let v_n = a_pow_n * v0 + c * geom;
    // Vanilla-like arc integration: y += vy, then vy = (vy - g) * drag.
    let displacement = v0 * geom + (c / one_minus_a) * (ticks - geom);
    (displacement, v_n * tuning.mc_ticks_per_second)
}

fn walk_speed_cap(fluid_state: &PlayerFluidState, controller: &CameraController) -> f32 {
    let tuning = PLAYER_TUNING_MINECRAFT;
    let sneak_requested = controller.move_down;
    let sprint_requested = controller.sprint
        && controller.move_forward
        && !controller.move_backward
        && !sneak_requested;

    let mut speed = if tuning.sneak_enabled && sneak_requested {
        tuning.sneak_speed
    } else if tuning.sprint_enabled && sprint_requested {
        tuning.sprint_speed
    } else {
        tuning.walk_speed
    };
    if fluid_state.wading_kind().is_some() {
        speed *= WADING_SPEED_MULT;
    }
    speed
}

fn flight_speed_cap(controller: &CameraController) -> f32 {
    let tuning = PLAYER_TUNING_MINECRAFT;
    let sprint_requested = controller.sprint;
    if tuning.sprint_enabled && sprint_requested {
        tuning.fly_speed_sprint
    } else {
        tuning.fly_speed
    }
}

fn sneak_move_keeps_ground_contact(
    world: &World,
    feet_pos: Vec3,
    delta_xz: Vec3,
    player_height: f32,
) -> bool {
    let moved =
        move_horizontal_with_step_up_with_height(world, feet_pos, delta_xz, true, player_height);
    let (_, grounded) = snap_to_ground_with_height(world, moved.feet_pos, player_height);
    grounded
}

fn clamp_sneak_axis_x(world: &World, feet_pos: Vec3, delta_x: f32, player_height: f32) -> f32 {
    if delta_x.abs() <= f32::EPSILON {
        return 0.0;
    }

    if sneak_move_keeps_ground_contact(world, feet_pos, Vec3::new(delta_x, 0.0, 0.0), player_height)
    {
        return delta_x;
    }

    let sign = delta_x.signum();
    let mut lo = 0.0_f32;
    let mut hi = delta_x.abs();
    for _ in 0..8 {
        let mid = (lo + hi) * 0.5;
        if sneak_move_keeps_ground_contact(
            world,
            feet_pos,
            Vec3::new(sign * mid, 0.0, 0.0),
            player_height,
        ) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    sign * lo
}

fn clamp_sneak_axis_z(world: &World, feet_pos: Vec3, delta_z: f32, player_height: f32) -> f32 {
    if delta_z.abs() <= f32::EPSILON {
        return 0.0;
    }

    if sneak_move_keeps_ground_contact(world, feet_pos, Vec3::new(0.0, 0.0, delta_z), player_height)
    {
        return delta_z;
    }

    let sign = delta_z.signum();
    let mut lo = 0.0_f32;
    let mut hi = delta_z.abs();
    for _ in 0..8 {
        let mid = (lo + hi) * 0.5;
        if sneak_move_keeps_ground_contact(
            world,
            feet_pos,
            Vec3::new(0.0, 0.0, sign * mid),
            player_height,
        ) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    sign * lo
}

fn clamp_sneak_edge_delta(
    world: &World,
    feet_pos: Vec3,
    delta_xz: Vec3,
    player_height: f32,
) -> Vec3 {
    if delta_xz.x.abs() <= f32::EPSILON && delta_xz.z.abs() <= f32::EPSILON {
        return delta_xz;
    }

    if sneak_move_keeps_ground_contact(world, feet_pos, delta_xz, player_height) {
        return delta_xz;
    }

    let safe_x = clamp_sneak_axis_x(world, feet_pos, delta_xz.x, player_height);
    let safe_z = clamp_sneak_axis_z(world, feet_pos, delta_xz.z, player_height);
    let safe_delta = Vec3::new(safe_x, 0.0, safe_z);

    if sneak_move_keeps_ground_contact(world, feet_pos, safe_delta, player_height) {
        return safe_delta;
    }

    let mut lo = 0.0_f32;
    let mut hi = 1.0_f32;
    for _ in 0..8 {
        let mid = (lo + hi) * 0.5;
        let test = safe_delta * mid;
        if sneak_move_keeps_ground_contact(world, feet_pos, test, player_height) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    safe_delta * lo
}

pub(crate) fn update_walk_movement(
    world: &World,
    camera: &Camera,
    controller: &mut CameraController,
    player: &mut Player,
    fluid_state: &PlayerFluidState,
    dt: f32,
) {
    let dt_non_negative = dt.max(0.0);
    if controller.move_up {
        player.jump_hold_cooldown_seconds =
            (player.jump_hold_cooldown_seconds - dt_non_negative).max(0.0);
    } else {
        // Releasing jump resets the hold-repeat delay like in vanilla.
        player.jump_hold_cooldown_seconds = 0.0;
    }

    if player.on_ground {
        let jump_pressed = controller.consume_jump_requested();
        let jump_from_hold =
            controller.move_up && player.jump_hold_cooldown_seconds <= f32::EPSILON;
        if jump_pressed || jump_from_hold {
            // Vanilla jump is a direct set, not additive impulse.
            player._velocity_y = jump_velocity();
            player.on_ground = false;
            if controller.move_up {
                player.jump_hold_cooldown_seconds = JUMP_HOLD_COOLDOWN_SECONDS;
            }
        } else {
            player._velocity_y = 0.0;
        }
    } else {
        controller.clear_jump_requested();
    }

    let horizontal_input = controller.horizontal_move_input(camera);
    let wish_dir = if horizontal_input.length_squared() > 0.0 {
        horizontal_input.normalize()
    } else {
        Vec3::ZERO
    };

    let target_speed = walk_speed_cap(fluid_state, controller);
    let target_horizontal = Vec3::new(wish_dir.x, 0.0, wish_dir.z) * target_speed;

    let mut accel_rate = if player.on_ground {
        PLAYER_TUNING_MINECRAFT.walk_accel_ground
    } else {
        PLAYER_TUNING_MINECRAFT.walk_accel_air
    };
    let mut decel_rate = if player.on_ground {
        PLAYER_TUNING_MINECRAFT.walk_decel_ground
    } else {
        PLAYER_TUNING_MINECRAFT.walk_decel_air
    };
    if fluid_state.wading_kind().is_some() {
        accel_rate *= WADING_ACCEL_MULT;
        decel_rate += WADING_EXTRA_DRAG;
    }
    let smoothing_rate = if wish_dir.length_squared() > 0.0 {
        accel_rate
    } else {
        decel_rate
    };

    player.walk_velocity.x = exp_approach(
        player.walk_velocity.x,
        target_horizontal.x,
        smoothing_rate,
        dt,
    );
    player.walk_velocity.z = exp_approach(
        player.walk_velocity.z,
        target_horizontal.z,
        smoothing_rate,
        dt,
    );

    let mut delta_h = Vec3::new(player.walk_velocity.x, 0.0, player.walk_velocity.z) * dt.max(0.0);
    if fluid_state.depth_for_kind(FluidKind::Water) > 0.0 {
        let flow_probe =
            player.feet_position + Vec3::new(0.0, player.collision_height() * 0.35, 0.0);
        let flow = flow_velocity_at_point(world, FluidKind::Water, flow_probe);
        delta_h += Vec3::new(flow.x, 0.0, flow.z) * dt.max(0.0);
    }
    let sneak_edge_guard = PLAYER_TUNING_MINECRAFT.sneak_enabled
        && controller.move_down
        && player.on_ground
        && !controller.move_up;
    if sneak_edge_guard {
        delta_h = clamp_sneak_edge_delta(
            world,
            player.feet_position,
            delta_h,
            player.collision_height(),
        );
    }
    let (delta_y, new_vy) = if player.on_ground {
        (0.0, player._velocity_y)
    } else {
        integrate_air_vertical_motion(player._velocity_y, dt)
    };
    player._velocity_y = new_vy;

    let desired_delta = Vec3::new(delta_h.x, delta_y, delta_h.z);
    let move_result = move_character_with_conditional_step_up_with_height(
        world,
        player.feet_position,
        desired_delta,
        can_attempt_step_up(player, player._velocity_y),
        player.collision_height(),
    );
    player.feet_position = move_result.feet_pos;
    let collision = move_result.collision;

    if collision.hit_x {
        player.walk_velocity.x = 0.0;
    }
    if collision.hit_z {
        player.walk_velocity.z = 0.0;
    }

    if collision.hit_y_down {
        player.on_ground = true;
        player._velocity_y = 0.0;
    } else if collision.hit_y_up {
        if player._velocity_y > 0.0 {
            player._velocity_y = 0.0;
        }
        player.on_ground = false;
    } else if delta_y.abs() <= f32::EPSILON {
        let (snapped, grounded) =
            snap_to_ground_with_height(world, player.feet_position, player.collision_height());
        player.feet_position = snapped;
        player.on_ground = grounded;
    } else {
        player.on_ground = false;
    }
}

pub(crate) fn update_flight_movement(
    world: &World,
    camera: &Camera,
    controller: &mut CameraController,
    player: &mut Player,
    last_space_tap: &mut Option<Instant>,
    dt: f32,
) {
    let horizontal_input = controller.horizontal_move_input(camera);
    let vertical_input = match (controller.move_up, controller.move_down) {
        (true, false) => 1.0,
        (false, true) => -1.0,
        _ => 0.0,
    };
    let desired_dir = Vec3::new(horizontal_input.x, vertical_input, horizontal_input.z);
    let wish_dir = if desired_dir.length_squared() > 0.0 {
        desired_dir.normalize()
    } else {
        Vec3::ZERO
    };
    let target_velocity = wish_dir * flight_speed_cap(controller);
    let rate = PLAYER_TUNING_MINECRAFT.flight_accel;
    player.walk_velocity.x = exp_approach(player.walk_velocity.x, target_velocity.x, rate, dt);
    player.walk_velocity.y = exp_approach(player.walk_velocity.y, target_velocity.y, rate, dt);
    player.walk_velocity.z = exp_approach(player.walk_velocity.z, target_velocity.z, rate, dt);

    let desired_delta = player.walk_velocity * dt.max(0.0);
    let (new_feet, collision) = move_with_collisions_for_height(
        world,
        player.feet_position,
        desired_delta,
        player.collision_height(),
    );
    player.feet_position = new_feet;
    player._velocity_y = 0.0;
    controller.clear_jump_requested();
    player.swim_velocity = Vec3::ZERO;
    if collision.hit_x {
        player.walk_velocity.x = 0.0;
    }
    if collision.hit_y_up || collision.hit_y_down {
        player.walk_velocity.y = 0.0;
    }
    if collision.hit_z {
        player.walk_velocity.z = 0.0;
    }

    // Auto-exit nur bei echter Bodenberührung während negativer Y-Bewegung.
    if desired_delta.y < 0.0 && collision.hit_y_down {
        player.mode = MoveMode::Walk;
        player.swim_velocity = Vec3::ZERO;
        player.on_ground = true;
        *last_space_tap = None;
    } else {
        player.on_ground = collision.hit_y_down;
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PlayerFluidState {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) body_kind: Option<FluidKind>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) in_water_body: bool,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) in_lava_body: bool,
    pub(crate) underwater_kind: Option<FluidKind>,
    pub(crate) eyes_in_water: bool,
    pub(crate) eyes_in_lava: bool,
    pub(crate) surface_contact_water: bool,
    pub(crate) surface_contact_lava: bool,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) water_surface_y: Option<f32>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) lava_surface_y: Option<f32>,
    pub(crate) water_depth: f32,
    pub(crate) lava_depth: f32,
    pub(crate) depth_kind: Option<FluidKind>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) surface_y: Option<f32>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) submerged_depth: f32,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) at_surface: bool,
}

fn can_fit_player_height(world: &World, feet_pos: Vec3, height: f32) -> bool {
    let (aabb_min, aabb_max) = player_aabb_with_height(feet_pos, height);
    !aabb_collides(world, aabb_min, aabb_max)
}

fn aabb_intersects_fluid_kind(
    world: &World,
    aabb_min: Vec3,
    aabb_max: Vec3,
    kind: FluidKind,
) -> bool {
    let min_x = aabb_min.x.floor() as i32;
    let min_y = aabb_min.y.floor() as i32;
    let min_z = aabb_min.z.floor() as i32;
    let max_x = (aabb_max.x - COLLISION_EPS).floor() as i32;
    let max_y = (aabb_max.y - COLLISION_EPS).floor() as i32;
    let max_z = (aabb_max.z - COLLISION_EPS).floor() as i32;

    for y in min_y..=max_y {
        for z in min_z..=max_z {
            for x in min_x..=max_x {
                let cell = IVec3::new(x, y, z);
                if !World::in_bounds(cell) {
                    continue;
                }
                if fluid_kind(world.get_block(cell)) != Some(kind) {
                    continue;
                }
                let probe = Vec3::new(x as f32 + 0.5, y as f32 + 0.001, z as f32 + 0.5);
                let Some(surface_y) = liquid_height_at_point(world, kind, probe) else {
                    continue;
                };
                let fluid_min_y = y as f32;
                let fluid_max_y = surface_y.clamp(fluid_min_y, fluid_min_y + 1.0);
                if fluid_max_y > aabb_min.y + COLLISION_EPS
                    && fluid_min_y < aabb_max.y - COLLISION_EPS
                {
                    return true;
                }
            }
        }
    }
    false
}

impl PlayerFluidState {
    #[allow(dead_code)]
    pub(crate) fn surface_for_kind(&self, kind: FluidKind) -> Option<f32> {
        match kind {
            FluidKind::Water => self.water_surface_y,
            FluidKind::Lava => self.lava_surface_y,
        }
    }

    pub(crate) fn depth_for_kind(&self, kind: FluidKind) -> f32 {
        match kind {
            FluidKind::Water => self.water_depth,
            FluidKind::Lava => self.lava_depth,
        }
    }

    pub(crate) fn in_body_for_kind(&self, kind: FluidKind) -> bool {
        match kind {
            FluidKind::Water => self.in_water_body,
            FluidKind::Lava => self.in_lava_body,
        }
    }

    pub(crate) fn eyes_in_for_kind(&self, kind: FluidKind) -> bool {
        match kind {
            FluidKind::Water => self.eyes_in_water,
            FluidKind::Lava => self.eyes_in_lava,
        }
    }

    pub(crate) fn surface_contact_for_kind(&self, kind: FluidKind) -> bool {
        match kind {
            FluidKind::Water => self.surface_contact_water,
            FluidKind::Lava => self.surface_contact_lava,
        }
    }

    pub(crate) fn best_kind_for_swim_enter(&self) -> Option<FluidKind> {
        if self.in_water_body {
            Some(FluidKind::Water)
        } else if self.in_lava_body {
            Some(FluidKind::Lava)
        } else {
            None
        }
    }

    pub(crate) fn wading_kind(&self) -> Option<FluidKind> {
        self.depth_kind.filter(|&kind| {
            let depth = self.depth_for_kind(kind);
            depth > 0.0 && depth < SWIM_ENTER_DEPTH
        })
    }

    pub(crate) fn sample(world: &World, player: &Player) -> Self {
        let waist_point =
            player.feet_position + Vec3::new(0.0, player.collision_height() * 0.5, 0.0);
        let eye_point = player.eye_position();
        let eye_kind = fluid_kind_at_point(world, eye_point);
        let (aabb_min, aabb_max) =
            player_aabb_with_height(player.feet_position, player.collision_height());
        let in_water_body = aabb_intersects_fluid_kind(world, aabb_min, aabb_max, FluidKind::Water);
        let in_lava_body = aabb_intersects_fluid_kind(world, aabb_min, aabb_max, FluidKind::Lava);
        let body_kind = if in_water_body {
            Some(FluidKind::Water)
        } else if in_lava_body {
            Some(FluidKind::Lava)
        } else {
            None
        };
        let surface_probe = player.feet_position + Vec3::new(0.0, 0.02, 0.0);
        let water_surface_y = liquid_height_at_point(world, FluidKind::Water, surface_probe)
            .or_else(|| liquid_height_at_point(world, FluidKind::Water, waist_point))
            .or_else(|| liquid_height_at_point(world, FluidKind::Water, eye_point));
        let lava_surface_y = liquid_height_at_point(world, FluidKind::Lava, surface_probe)
            .or_else(|| liquid_height_at_point(world, FluidKind::Lava, waist_point))
            .or_else(|| liquid_height_at_point(world, FluidKind::Lava, eye_point));

        let water_depth = water_surface_y
            .map(|surface| surface - player.feet_position.y)
            .unwrap_or(f32::NEG_INFINITY);
        let lava_depth = lava_surface_y
            .map(|surface| surface - player.feet_position.y)
            .unwrap_or(f32::NEG_INFINITY);

        let depth_kind = if water_depth >= lava_depth {
            (water_depth > 0.0).then_some(FluidKind::Water)
        } else {
            (lava_depth > 0.0).then_some(FluidKind::Lava)
        };
        let surface_y = depth_kind.and_then(|kind| match kind {
            FluidKind::Water => water_surface_y,
            FluidKind::Lava => lava_surface_y,
        });
        let submerged_depth = depth_kind
            .map(|kind| match kind {
                FluidKind::Water => water_depth,
                FluidKind::Lava => lava_depth,
            })
            .unwrap_or(f32::NEG_INFINITY);
        let eyes_in_water = eye_kind == Some(FluidKind::Water);
        let eyes_in_lava = eye_kind == Some(FluidKind::Lava);
        let surface_contact_water = water_surface_y
            .map(|surface| (surface - eye_point.y).abs() <= FLUID_SURFACE_CONTACT_EPS)
            .unwrap_or(false);
        let surface_contact_lava = lava_surface_y
            .map(|surface| (surface - eye_point.y).abs() <= FLUID_SURFACE_CONTACT_EPS)
            .unwrap_or(false);
        let at_surface = surface_y
            .map(|surface| (surface - eye_point.y).abs() <= FLUID_SURFACE_CONTACT_EPS)
            .unwrap_or(false);

        Self {
            body_kind,
            in_water_body,
            in_lava_body,
            underwater_kind: eye_kind,
            eyes_in_water,
            eyes_in_lava,
            surface_contact_water,
            surface_contact_lava,
            water_surface_y,
            lava_surface_y,
            water_depth,
            lava_depth,
            depth_kind,
            surface_y,
            submerged_depth,
            at_surface,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FluidMovementParams {
    pub(crate) input_accel_per_tick: f32,
    pub(crate) gravity_per_tick: f32,
    pub(crate) jump_impulse_per_tick: f32,
    pub(crate) dive_impulse_per_tick: f32,
    pub(crate) drag_per_tick: f32,
    pub(crate) speed_cap: f32,
    pub(crate) sprint_swim_speed_cap: Option<f32>,
    pub(crate) ledge_pop_vy_per_tick: f32,
}

pub(crate) fn fluid_movement_profile(kind: FluidKind) -> FluidMovementParams {
    match kind {
        FluidKind::Water => FluidMovementParams {
            input_accel_per_tick: FLUID_INPUT_ACCEL_PER_TICK,
            gravity_per_tick: FLUID_GRAVITY_PER_TICK,
            jump_impulse_per_tick: FLUID_JUMP_IMPULSE_PER_TICK,
            dive_impulse_per_tick: FLUID_DIVE_ACCEL_PER_TICK,
            drag_per_tick: WATER_FLUID_DRAG_PER_TICK,
            speed_cap: WATER_SWIM_SPEED_CAP,
            sprint_swim_speed_cap: Some(WATER_SPRINT_SWIM_SPEED_CAP),
            ledge_pop_vy_per_tick: FLUID_LEDGE_POP_VY_PER_TICK,
        },
        FluidKind::Lava => FluidMovementParams {
            input_accel_per_tick: FLUID_INPUT_ACCEL_PER_TICK,
            gravity_per_tick: FLUID_GRAVITY_PER_TICK,
            jump_impulse_per_tick: FLUID_JUMP_IMPULSE_PER_TICK,
            dive_impulse_per_tick: FLUID_DIVE_ACCEL_PER_TICK,
            drag_per_tick: LAVA_FLUID_DRAG_PER_TICK,
            speed_cap: LAVA_SWIM_SPEED_CAP,
            sprint_swim_speed_cap: None,
            ledge_pop_vy_per_tick: FLUID_LEDGE_POP_VY_PER_TICK,
        },
    }
}

fn current_speed_cap(player: &Player, kind: FluidKind, params: FluidMovementParams) -> f32 {
    if kind == FluidKind::Water && player.sprint_swim_active {
        params.sprint_swim_speed_cap.unwrap_or(params.speed_cap)
    } else {
        params.speed_cap
    }
}

fn can_fluid_ledge_pop(world: &World, feet_pos: Vec3, delta_xz: Vec3, player_height: f32) -> bool {
    let move_xz = Vec3::new(delta_xz.x, 0.0, delta_xz.z);
    if move_xz.length_squared() <= f32::EPSILON {
        return false;
    }

    let (flat_pos, flat_flags) =
        move_with_collisions_for_height(world, feet_pos, move_xz, player_height);
    if !(flat_flags.hit_x || flat_flags.hit_z) {
        return false;
    }

    let (after_up, up_flags) = move_with_collisions_for_height(
        world,
        feet_pos,
        Vec3::new(0.0, FLUID_LEDGE_ASSIST_STEP_CHECK, 0.0),
        player_height,
    );
    if up_flags.hit_y_up || after_up.y < feet_pos.y + FLUID_LEDGE_ASSIST_STEP_CHECK - STEP_EPS {
        return false;
    }

    let (after_step_xz, _) =
        move_with_collisions_for_height(world, after_up, move_xz, player_height);
    horizontal_progress_squared(feet_pos, after_step_xz)
        > horizontal_progress_squared(feet_pos, flat_pos) + STEP_EPS * STEP_EPS
}

pub(crate) fn update_player_pose_for_fluids(
    world: &World,
    controller: &CameraController,
    player: &mut Player,
    fluid_state: &PlayerFluidState,
) {
    let forced_prone = !can_fit_player_height(world, player.feet_position, PLAYER_HEIGHT);
    let sprint_swim_requested = fluid_state.in_water_body
        && controller.sprint
        && player.can_sprint_swim()
        && (fluid_state.eyes_in_water || forced_prone);

    player.is_swim_prone = forced_prone || sprint_swim_requested;
    player.sprint_swim_active = sprint_swim_requested;
}

pub(crate) fn update_swim_movement(
    world: &World,
    camera: &Camera,
    controller: &mut CameraController,
    player: &mut Player,
    kind: FluidKind,
    fluid_state: &PlayerFluidState,
    dt: f32,
) {
    update_player_pose_for_fluids(world, controller, player, fluid_state);
    let fluid_state = PlayerFluidState::sample(world, player);
    let params = fluid_movement_profile(kind);
    let ticks = (dt.max(0.0) * PLAYER_TUNING_MINECRAFT.mc_ticks_per_second).max(0.0);
    if ticks <= f32::EPSILON {
        return;
    }

    let horizontal_input = controller.horizontal_move_input(camera);
    let wish_dir = if horizontal_input.length_squared() > 0.0 {
        horizontal_input.normalize()
    } else {
        Vec3::ZERO
    };
    let mut vel_tick = player.swim_velocity / PLAYER_TUNING_MINECRAFT.mc_ticks_per_second;
    let mut remaining_ticks = ticks;
    while remaining_ticks > 0.0 {
        let sub_tick = remaining_ticks.min(1.0);
        remaining_ticks -= sub_tick;

        vel_tick += Vec3::new(wish_dir.x, 0.0, wish_dir.z) * params.input_accel_per_tick * sub_tick;

        if controller.move_up {
            vel_tick.y +=
                params.jump_impulse_per_tick * FLUID_SWIM_SPEED_ATTR_BASE.max(0.0) * sub_tick;
        }
        if controller.move_down {
            vel_tick.y -= params.dive_impulse_per_tick * sub_tick;
        }
        vel_tick.y -= params.gravity_per_tick * sub_tick;

        if kind == FluidKind::Water {
            let surface_contact = fluid_state.surface_contact_for_kind(kind);
            let eyes_in = fluid_state.eyes_in_for_kind(kind);

            if surface_contact {
                if !player.was_at_surface {
                    player.bob_phase = 0.0;
                }
                player.was_at_surface = true;
            } else {
                player.was_at_surface = false;
            }

            if surface_contact && controller.move_up {
                vel_tick.y = vel_tick.y.max(0.0);
                player.bob_phase += sub_tick * std::f32::consts::TAU * 0.5;
                vel_tick.y += player.bob_phase.sin() * FLUID_BOB_IMPULSE_PER_TICK;
            } else if controller.sprint && !eyes_in && fluid_state.in_body_for_kind(kind) {
                // Surface skimming while sprinting should stay close to waterline.
                vel_tick.y = vel_tick.y.max(-FLUID_BOB_IMPULSE_PER_TICK);
            }
        } else {
            player.was_at_surface = false;
        }

        let drag = params.drag_per_tick.powf(sub_tick);
        vel_tick *= drag;

        let speed_cap = current_speed_cap(player, kind, params);
        let mut horizontal = Vec3::new(vel_tick.x, 0.0, vel_tick.z);
        let speed = horizontal.length();
        if speed > speed_cap {
            horizontal = horizontal / speed * speed_cap;
            vel_tick.x = horizontal.x;
            vel_tick.z = horizontal.z;
        }
    }

    player.swim_velocity = vel_tick * PLAYER_TUNING_MINECRAFT.mc_ticks_per_second;
    let desired_delta = player.swim_velocity * dt.max(0.0);
    let allow_step_up = can_attempt_step_up(player, player.swim_velocity.y);
    let move_result = move_character_with_conditional_step_up_with_height(
        world,
        player.feet_position,
        desired_delta,
        allow_step_up,
        player.collision_height(),
    );
    player.feet_position = move_result.feet_pos;
    let collision = move_result.collision;
    if collision.hit_x {
        player.swim_velocity.x = 0.0;
    }
    if collision.hit_z {
        player.swim_velocity.z = 0.0;
    }
    if collision.hit_y_up && player.swim_velocity.y > 0.0 {
        player.swim_velocity.y = 0.0;
    }
    if collision.hit_y_down && player.swim_velocity.y < 0.0 {
        player.swim_velocity.y = 0.0;
    }

    let horizontal_colliding = collision.hit_x || collision.hit_z;
    if horizontal_colliding && fluid_state.in_body_for_kind(kind) {
        let ledge_delta =
            if desired_delta.x.abs() > f32::EPSILON || desired_delta.z.abs() > f32::EPSILON {
                desired_delta
            } else {
                Vec3::new(wish_dir.x, 0.0, wish_dir.z) * 0.1
            };
        if can_fluid_ledge_pop(
            world,
            player.feet_position,
            ledge_delta,
            player.collision_height(),
        ) {
            player.swim_velocity.y = player
                .swim_velocity
                .y
                .max(params.ledge_pop_vy_per_tick * PLAYER_TUNING_MINECRAFT.mc_ticks_per_second);
            player.on_ground = false;
        }
    }

    player.on_ground = collision.hit_y_down;
    controller.clear_jump_requested();
    player._velocity_y = player.swim_velocity.y;
}

pub(crate) fn apply_swim_mode_transition(player: &mut Player, fluid_state: &PlayerFluidState) {
    if matches!(player.mode, MoveMode::Flight) {
        return;
    }

    player.mode = match player.mode {
        MoveMode::Walk => {
            let Some(kind) = fluid_state.best_kind_for_swim_enter() else {
                return;
            };
            player.walk_velocity = Vec3::ZERO;
            player.swim_velocity = Vec3::ZERO;
            player._velocity_y = 0.0;
            player.bob_phase = 0.0;
            player.bob_anchor_y = player.feet_position.y;
            player.was_at_surface = false;
            player.on_ground = false;
            player.sprint_swim_active = false;
            player.jump_hold_cooldown_seconds = 0.0;
            MoveMode::Swim { kind }
        }
        MoveMode::Swim { kind } => {
            if fluid_state.in_body_for_kind(kind) {
                MoveMode::Swim { kind }
            } else if let Some(next_kind) = fluid_state.best_kind_for_swim_enter() {
                player.sprint_swim_active = false;
                player.jump_hold_cooldown_seconds = 0.0;
                MoveMode::Swim { kind: next_kind }
            } else {
                player.walk_velocity = Vec3::ZERO;
                player.swim_velocity = Vec3::ZERO;
                player._velocity_y = 0.0;
                player.bob_phase = 0.0;
                player.was_at_surface = false;
                player.sprint_swim_active = false;
                player.jump_hold_cooldown_seconds = 0.0;
                MoveMode::Walk
            }
        }
        MoveMode::Flight => MoveMode::Flight,
    };
}

pub(crate) fn underwater_overlay_alpha(
    world: &World,
    camera_eye: Vec3,
    underwater_kind: Option<FluidKind>,
) -> f32 {
    if underwater_kind != Some(FluidKind::Water) {
        return 0.0;
    }
    let surface_y =
        liquid_height_at_point(world, FluidKind::Water, camera_eye).unwrap_or(camera_eye.y);
    let depth = (surface_y - camera_eye.y).clamp(0.0, 1.0);
    WATER_OVERLAY_ALPHA_MIN + (WATER_OVERLAY_ALPHA_MAX - WATER_OVERLAY_ALPHA_MIN) * depth
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::BlockId;
    use crate::input::CameraController;

    fn make_single_block_platform_world() -> (World, IVec3) {
        let mut world = World::new();
        let block = IVec3::new(64, 20, 64);
        let _ = world.set_block(block, BlockId::Stone);
        (world, block)
    }

    fn make_current_test_water_world() -> (World, Vec3) {
        let mut world = World::new();
        let origin = IVec3::new(80, 20, 80);

        for z in (origin.z - 1)..=(origin.z + 1) {
            for x in (origin.x - 1)..=(origin.x + 2) {
                let _ = world.set_block(IVec3::new(x, origin.y - 1, z), BlockId::Stone);
            }
        }

        let _ = world.set_block_and_data(origin, BlockId::WaterStill, 0);
        let _ = world.set_block(origin + IVec3::Y, BlockId::WaterStill);
        let east = origin + IVec3::X;
        let _ = world.set_block_and_data(east, BlockId::WaterStill, 0);
        let _ = world.set_block(east + IVec3::Y, BlockId::WaterStill);

        let eye = Vec3::new(
            origin.x as f32 + 0.5,
            origin.y as f32 + EYE_HEIGHT,
            origin.z as f32 + 0.5,
        );
        (world, eye)
    }

    #[test]
    fn mc_jump_velocity_constant_matches() {
        let vy_tick = jump_velocity() / PLAYER_TUNING_MINECRAFT.mc_ticks_per_second;
        assert!((vy_tick - 0.42).abs() < 1e-5);
    }

    #[test]
    fn mc_jump_arc_height_is_vanilla_like() {
        let mut vy = jump_velocity();
        let mut y = 0.0_f32;
        let mut max_y = y;
        let dt = 1.0 / PLAYER_TUNING_MINECRAFT.mc_ticks_per_second;

        for _ in 0..200 {
            let (dy, next_vy) = integrate_air_vertical_motion(vy, dt);
            y += dy;
            vy = next_vy;
            if y > max_y {
                max_y = y;
            }
        }

        assert!(max_y > 1.20 && max_y < 1.30, "max_y={max_y}");
    }

    #[test]
    fn mc_terminal_velocity_matches_expected_range() {
        let dt = 1.0 / PLAYER_TUNING_MINECRAFT.mc_ticks_per_second;
        let mut vy = 0.0_f32;

        for _ in 0..800 {
            let (_, next_vy) = integrate_air_vertical_motion(vy, dt);
            vy = next_vy;
        }

        let vy_tick = vy / PLAYER_TUNING_MINECRAFT.mc_ticks_per_second;
        assert!(vy_tick < -3.85 && vy_tick > -4.00, "vy_tick={vy_tick}");
    }

    #[test]
    fn player_geometry_matches_vanilla_size_targets() {
        assert!((PLAYER_RADIUS * 2.0 - 0.6).abs() < 1e-6);
        assert!((PLAYER_HEIGHT - 1.8).abs() < 1e-6);
        assert!((EYE_HEIGHT - 1.62).abs() < 1e-6);
        assert!((STEP_HEIGHT - 0.6).abs() < 1e-6);
    }

    #[test]
    fn movement_speed_targets_match_minecraft_values() {
        assert!((PLAYER_TUNING_MINECRAFT.walk_speed - 4.317).abs() < 1e-6);
        assert!((PLAYER_TUNING_MINECRAFT.sprint_speed - 5.612).abs() < 1e-6);
        assert!((PLAYER_TUNING_MINECRAFT.sneak_speed - 1.295).abs() < 1e-6);
        assert!((PLAYER_TUNING_MINECRAFT.fly_speed - 10.92).abs() < 1e-6);
        assert!((PLAYER_TUNING_MINECRAFT.fly_speed_sprint - 21.6).abs() < 1e-6);
    }

    #[test]
    fn jump_hold_repeat_delay_matches_vanilla_ten_ticks() {
        assert!((JUMP_HOLD_COOLDOWN_SECONDS - 0.5).abs() < 1e-6);
    }

    #[test]
    fn slab_collision_only_blocks_lower_half() {
        let mut world = World::new();
        let slab = IVec3::new(20, 20, 20);
        let _ = world.set_block(slab, BlockId::StoneSlab);

        let upper_min = Vec3::new(20.1, 20.60, 20.1);
        let upper_max = Vec3::new(20.9, 20.95, 20.9);
        assert!(!aabb_collides(&world, upper_min, upper_max));

        let lower_min = Vec3::new(20.1, 20.10, 20.1);
        let lower_max = Vec3::new(20.9, 20.40, 20.9);
        assert!(aabb_collides(&world, lower_min, lower_max));
    }

    #[test]
    fn step_up_can_climb_onto_bottom_slab() {
        let mut world = World::new();
        let _ = world.set_block(IVec3::new(0, 0, 0), BlockId::Stone);
        let _ = world.set_block(IVec3::new(1, 0, 0), BlockId::Stone);
        let _ = world.set_block(IVec3::new(1, 1, 0), BlockId::StoneSlab);

        let start_feet = Vec3::new(0.5, 1.0, 0.5);
        let moved = move_character_with_step_up(&world, start_feet, Vec3::new(1.0, 0.0, 0.0));

        assert!(
            moved.feet_pos.y > 1.45 && moved.feet_pos.y < 1.55,
            "expected slab top y~=1.5, got {}",
            moved.feet_pos.y
        );
    }

    #[test]
    fn fluid_state_detects_aabb_body_intersection() {
        let mut world = World::new();
        let fluid_cell = IVec3::new(24, 20, 24);
        let _ = world.set_block_and_data(fluid_cell, BlockId::WaterStill, 0);
        let _ = world.set_block(fluid_cell + IVec3::Y, BlockId::WaterStill);

        let player_eye = Vec3::new(fluid_cell.x as f32 + 0.5, 21.2, fluid_cell.z as f32 + 0.5);
        let player = Player::from_eye_position(player_eye);
        let state = PlayerFluidState::sample(&world, &player);

        assert!(state.in_water_body);
        assert!(state.eyes_in_water);
        assert!(!state.in_lava_body);
    }

    #[test]
    fn sprint_swim_pose_only_activates_in_water() {
        let mut controller = CameraController::new();
        controller.sprint = true;

        let mut world_water = World::new();
        let water = IVec3::new(32, 20, 32);
        let _ = world_water.set_block(water, BlockId::WaterStill);
        let _ = world_water.set_block(water + IVec3::Y, BlockId::WaterStill);

        let mut player_water = Player::from_eye_position(Vec3::new(32.5, 21.2, 32.5));
        let water_state = PlayerFluidState::sample(&world_water, &player_water);
        update_player_pose_for_fluids(&world_water, &controller, &mut player_water, &water_state);
        assert!(player_water.sprint_swim_active);
        assert!(player_water.is_swim_prone);

        let mut world_lava = World::new();
        let lava = IVec3::new(40, 20, 40);
        let _ = world_lava.set_block(lava, BlockId::LavaStill);
        let _ = world_lava.set_block(lava + IVec3::Y, BlockId::LavaStill);

        let mut player_lava = Player::from_eye_position(Vec3::new(40.5, 21.2, 40.5));
        let lava_state = PlayerFluidState::sample(&world_lava, &player_lava);
        update_player_pose_for_fluids(&world_lava, &controller, &mut player_lava, &lava_state);
        assert!(!player_lava.sprint_swim_active);
    }

    #[test]
    fn fluid_ledge_pop_detects_climb_out_path() {
        let mut world = World::new();
        let water = IVec3::new(48, 20, 48);
        let shore = water + IVec3::new(1, 0, 0);
        let _ = world.set_block(water + IVec3::new(0, -1, 0), BlockId::Stone);
        let _ = world.set_block_and_data(water, BlockId::WaterStill, 0);
        let _ = world.set_block(shore, BlockId::Stone);
        let _ = world.set_block(shore + IVec3::new(0, -1, 0), BlockId::Stone);

        let feet = Vec3::new(
            water.x as f32 + 0.5,
            water.y as f32 + 0.58,
            water.z as f32 + 0.5,
        );
        let can_pop = can_fluid_ledge_pop(&world, feet, Vec3::new(0.7, 0.0, 0.0), PLAYER_HEIGHT);
        assert!(can_pop);
    }

    #[test]
    fn flowing_water_does_not_push_player_in_flight_mode() {
        let (world, eye) = make_current_test_water_world();
        let mut player = Player::from_eye_position(eye);
        player.mode = MoveMode::Flight;
        let start = player.feet_position;
        let mut controller = CameraController::new();
        let camera = Camera::default();
        let mut last_space_tap = None;

        for _ in 0..60 {
            update_flight_movement(
                &world,
                &camera,
                &mut controller,
                &mut player,
                &mut last_space_tap,
                1.0 / 60.0,
            );
        }

        let moved = Vec3::new(
            player.feet_position.x - start.x,
            0.0,
            player.feet_position.z - start.z,
        );
        assert!(
            moved.length_squared() <= 1e-6,
            "player drifted in flight from water current: delta={:?}",
            moved
        );
    }

    #[test]
    fn flowing_water_pushes_player_in_walk_mode() {
        let (world, eye) = make_current_test_water_world();
        let mut player = Player::from_eye_position(eye);
        player.mode = MoveMode::Walk;
        player.on_ground = true;
        let start_x = player.feet_position.x;
        let mut controller = CameraController::new();
        let camera = Camera::default();

        for _ in 0..60 {
            let fluid_state = PlayerFluidState::sample(&world, &player);
            update_walk_movement(
                &world,
                &camera,
                &mut controller,
                &mut player,
                &fluid_state,
                1.0 / 60.0,
            );
        }

        assert!(
            player.feet_position.x < start_x - 0.05,
            "expected walk mode to be pushed by flowing water: start_x={start_x} end_x={}",
            player.feet_position.x
        );
    }

    #[test]
    fn sneak_prevents_falling_off_block_edge() {
        let (world, block) = make_single_block_platform_world();
        let mut player = Player::from_eye_position(Vec3::new(
            block.x as f32 + 0.5,
            block.y as f32 + 1.0 + EYE_HEIGHT,
            block.z as f32 + 0.5,
        ));
        player.mode = MoveMode::Walk;
        player.on_ground = true;

        let camera = Camera::default();
        let mut controller = CameraController::new();
        controller.move_right = true;
        controller.move_down = true;

        for _ in 0..180 {
            let fluid_state = PlayerFluidState::sample(&world, &player);
            update_walk_movement(
                &world,
                &camera,
                &mut controller,
                &mut player,
                &fluid_state,
                1.0 / 60.0,
            );
        }

        assert!(
            player.on_ground,
            "sneaking should keep player grounded at the edge"
        );
        assert!(
            player.feet_position.y >= block.y as f32 + 0.99,
            "player dropped while sneaking: y={}",
            player.feet_position.y
        );
    }

    #[test]
    fn walking_without_sneak_can_fall_off_block_edge() {
        let (world, block) = make_single_block_platform_world();
        let mut player = Player::from_eye_position(Vec3::new(
            block.x as f32 + 0.5,
            block.y as f32 + 1.0 + EYE_HEIGHT,
            block.z as f32 + 0.5,
        ));
        player.mode = MoveMode::Walk;
        player.on_ground = true;

        let camera = Camera::default();
        let mut controller = CameraController::new();
        controller.move_right = true;

        for _ in 0..180 {
            let fluid_state = PlayerFluidState::sample(&world, &player);
            update_walk_movement(
                &world,
                &camera,
                &mut controller,
                &mut player,
                &fluid_state,
                1.0 / 60.0,
            );
        }

        assert!(
            !player.on_ground || player.feet_position.y < block.y as f32 + 0.99,
            "without sneaking the player should be able to step off and fall"
        );
    }
}
