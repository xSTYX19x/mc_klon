pub(crate) mod app {
    pub(crate) const DOUBLE_SPACE_WINDOW: f32 = 0.20;
}

pub(crate) mod assets {
    pub(crate) const ASSET_ROOT: &str = "assets/minecraft";
}

pub(crate) mod player {
    #[derive(Clone, Copy, Debug)]
    pub(crate) struct PlayerTuningMinecraft {
        pub(crate) mc_ticks_per_second: f32,
        pub(crate) walk_speed: f32,
        pub(crate) sprint_speed: f32,
        pub(crate) sneak_speed: f32,
        pub(crate) fly_speed: f32,
        pub(crate) fly_speed_sprint: f32,
        pub(crate) jump_initial_vy_per_tick: f32,
        pub(crate) gravity_per_tick: f32,
        pub(crate) air_drag_per_tick: f32,
        pub(crate) walk_accel_ground: f32,
        pub(crate) walk_accel_air: f32,
        pub(crate) walk_decel_ground: f32,
        pub(crate) walk_decel_air: f32,
        pub(crate) flight_accel: f32,
        pub(crate) sprint_enabled: bool,
        pub(crate) sneak_enabled: bool,
        pub(crate) player_width: f32,
        pub(crate) player_height: f32,
        pub(crate) eye_height: f32,
        pub(crate) step_height: f32,
    }

    pub(crate) const PLAYER_TUNING_MINECRAFT: PlayerTuningMinecraft = PlayerTuningMinecraft {
        mc_ticks_per_second: 20.0,
        walk_speed: 4.317,
        sprint_speed: 5.612,
        sneak_speed: 1.295,
        fly_speed: 10.92,
        fly_speed_sprint: 21.6,
        jump_initial_vy_per_tick: 0.42,
        gravity_per_tick: 0.08,
        air_drag_per_tick: 0.98,
        walk_accel_ground: 28.0,
        walk_accel_air: 9.0,
        walk_decel_ground: 18.0,
        walk_decel_air: 3.0,
        flight_accel: 16.0,
        sprint_enabled: true,
        sneak_enabled: true,
        player_width: 0.6,
        player_height: 1.8,
        eye_height: 1.62,
        step_height: 0.6,
    };

    pub(crate) const PLAYER_RADIUS: f32 = PLAYER_TUNING_MINECRAFT.player_width * 0.5;
    pub(crate) const PLAYER_HEIGHT: f32 = PLAYER_TUNING_MINECRAFT.player_height;
    pub(crate) const EYE_HEIGHT: f32 = PLAYER_TUNING_MINECRAFT.eye_height;
    pub(crate) const SWIM_POSE_HEIGHT: f32 = 0.6;
    pub(crate) const SWIM_POSE_EYE_HEIGHT: f32 = 0.4;
    pub(crate) const COLLISION_EPS: f32 = 1e-4;
    pub(crate) const COLLISION_SUBSTEP: f32 = 0.1;
    #[allow(dead_code)]
    pub(crate) const GRAVITY: f32 = PLAYER_TUNING_MINECRAFT.gravity_per_tick
        * PLAYER_TUNING_MINECRAFT.mc_ticks_per_second
        * PLAYER_TUNING_MINECRAFT.mc_ticks_per_second;
    pub(crate) const WADING_SPEED_MULT: f32 = 0.7;
    pub(crate) const WADING_ACCEL_MULT: f32 = 0.8;
    pub(crate) const WADING_EXTRA_DRAG: f32 = 2.0;
    pub(crate) const SWIM_ENTER_DEPTH: f32 = 0.9;
    #[allow(dead_code)]
    pub(crate) const SWIM_EXIT_DEPTH: f32 = 0.6;
    #[allow(dead_code)]
    pub(crate) const SWIM_BOB_SURFACE_BAND_EYE: f32 = 0.4;
    #[allow(dead_code)]
    pub(crate) const SWIM_BOBBING_AMP: f32 = 0.5;
    #[allow(dead_code)]
    pub(crate) const SWIM_BOB_K: f32 = 18.0;
    #[allow(dead_code)]
    pub(crate) const SWIM_BOB_D: f32 = 8.0;
    #[allow(dead_code)]
    pub(crate) const SWIM_WATER_SINK_RATE: f32 = 0.10;
    #[allow(dead_code)]
    pub(crate) const SWIM_LAVA_SINK_RATE: f32 = 0.05;
    #[allow(dead_code)]
    pub(crate) const SWIM_LIQUID_GRAVITY_MULT: f32 = 0.35;
    #[allow(dead_code)]
    pub(crate) const SWIM_BUOYANCY_MULT: f32 = 0.6;
    #[allow(dead_code)]
    pub(crate) const SWIM_WATER_MAX_SPEED: f32 = 4.5;
    #[allow(dead_code)]
    pub(crate) const SWIM_WATER_ACCEL: f32 = 18.0;
    #[allow(dead_code)]
    pub(crate) const SWIM_WATER_DRAG_XZ: f32 = 6.0;
    #[allow(dead_code)]
    pub(crate) const SWIM_WATER_DRAG_Y: f32 = 4.0;
    #[allow(dead_code)]
    pub(crate) const SWIM_WATER_BOB_FREQ: f32 = 1.2;
    #[allow(dead_code)]
    pub(crate) const SWIM_LAVA_MAX_SPEED: f32 = SWIM_WATER_MAX_SPEED * 0.5;
    #[allow(dead_code)]
    pub(crate) const SWIM_LAVA_ACCEL: f32 = SWIM_WATER_ACCEL * 0.6;
    #[allow(dead_code)]
    pub(crate) const SWIM_LAVA_DRAG_XZ: f32 = 10.0;
    #[allow(dead_code)]
    pub(crate) const SWIM_LAVA_DRAG_Y: f32 = 8.0;
    #[allow(dead_code)]
    pub(crate) const SWIM_LAVA_BOB_FREQ: f32 = 0.8;
    pub(crate) const FLUID_SURFACE_CONTACT_EPS: f32 = 0.08;
    pub(crate) const FLUID_INPUT_ACCEL_PER_TICK: f32 = 0.02;
    pub(crate) const FLUID_GRAVITY_PER_TICK: f32 = 0.02;
    pub(crate) const FLUID_JUMP_IMPULSE_PER_TICK: f32 = 0.04;
    pub(crate) const FLUID_LEDGE_POP_VY_PER_TICK: f32 = 0.3;
    pub(crate) const FLUID_SWIM_SPEED_ATTR_BASE: f32 = 1.0;
    pub(crate) const WATER_FLUID_DRAG_PER_TICK: f32 = 0.8;
    pub(crate) const LAVA_FLUID_DRAG_PER_TICK: f32 = 0.5;
    pub(crate) const WATER_SWIM_SPEED_CAP: f32 = 2.20;
    pub(crate) const WATER_SPRINT_SWIM_SPEED_CAP: f32 = PLAYER_TUNING_MINECRAFT.sprint_speed;
    pub(crate) const LAVA_SWIM_SPEED_CAP: f32 = 1.10;
    pub(crate) const FLUID_BOB_IMPULSE_PER_TICK: f32 = 0.003;
    pub(crate) const FLUID_LEDGE_ASSIST_STEP_CHECK: f32 = 0.6;
    pub(crate) const FLUID_DIVE_ACCEL_PER_TICK: f32 = 0.04;
    pub(crate) const GROUND_CHECK_DISTANCE: f32 = 0.08;
    pub(crate) const STEP_HEIGHT: f32 = PLAYER_TUNING_MINECRAFT.step_height;
    pub(crate) const STEP_EPS: f32 = 0.001;
    pub(crate) const STEP_GROUNDED_GRACE_SECONDS: f32 = 0.10;
    pub(crate) const JUMP_HOLD_REPEAT_TICKS: f32 = 10.0;
    pub(crate) const JUMP_HOLD_COOLDOWN_SECONDS: f32 =
        JUMP_HOLD_REPEAT_TICKS / PLAYER_TUNING_MINECRAFT.mc_ticks_per_second;
    pub(crate) const VIEW_BOB_STRIDE_LENGTH_BLOCKS: f32 = 0.62;
    pub(crate) const VIEW_BOB_SPEED_MULTIPLIER: f32 = 0.80;
    pub(crate) const VIEW_BOB_VERTICAL_AMPLITUDE: f32 = 0.0;
    pub(crate) const VIEW_BOB_HORIZONTAL_AMPLITUDE: f32 = 0.012;
    pub(crate) const VIEW_BOB_PITCH_AMPLITUDE_RAD: f32 = 0.0;
    pub(crate) const VIEW_BOB_ROLL_AMPLITUDE_RAD: f32 = 0.0;
    pub(crate) const VIEW_BOB_MAX_SPEED_MULTIPLIER: f32 = 1.30;
    pub(crate) const VIEW_BOB_MIN_SPEED: f32 = 0.10;
    pub(crate) const VIEW_BOB_INTENSITY_RESPONSE: f32 = 12.0;
    pub(crate) const VIEW_BOB_OFFSET_RESPONSE: f32 = 16.0;
    pub(crate) const VIEW_BOB_ROLL_RESPONSE: f32 = 14.0;
}

pub(crate) mod render {
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub(crate) enum RenderDistance {
        Short,
        Normal,
        Far,
        Extreme,
        Ultra,
        Insane,
    }

    /// User-selectable resting field of view, in degrees. The dynamic sprint /
    /// flight kick multiplies off this value, so it stays the anchor the camera
    /// eases back to when standing still.
    pub(crate) const BASE_FOV_MIN_DEG: u32 = 30;
    pub(crate) const BASE_FOV_MAX_DEG: u32 = 110;
    pub(crate) const BASE_FOV_STEP_DEG: u32 = 5;
    pub(crate) const BASE_FOV_DEFAULT_DEG: u32 = 70;

    /// Base FOV as a 0..1 slider fill.
    pub(crate) fn base_fov_fill01(deg: u32) -> f32 {
        let span = (BASE_FOV_MAX_DEG - BASE_FOV_MIN_DEG) as f32;
        ((deg as f32 - BASE_FOV_MIN_DEG as f32) / span).clamp(0.0, 1.0)
    }

    /// Inverse of [`base_fov_fill01`], snapped to `BASE_FOV_STEP_DEG` so dragging
    /// lands on the same values the arrow keys produce.
    pub(crate) fn base_fov_from_fill01(fill01: f32) -> u32 {
        let span = (BASE_FOV_MAX_DEG - BASE_FOV_MIN_DEG) as f32;
        let step = BASE_FOV_STEP_DEG.max(1) as f32;
        let raw = fill01.clamp(0.0, 1.0) * span;
        let snapped = (raw / step).round() * step + BASE_FOV_MIN_DEG as f32;
        (snapped.round() as i32).clamp(BASE_FOV_MIN_DEG as i32, BASE_FOV_MAX_DEG as i32) as u32
    }

    /// Multiplier applied on top of the per-element UI scales (hotbar, inventory,
    /// menu buttons, text). `Normal` is 1.0, i.e. the scales the code already had.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub(crate) enum GuiScale {
        Tiny,
        Small,
        #[default]
        Normal,
        Large,
        Huge,
    }

    impl GuiScale {
        pub(crate) const ALL: [Self; 5] = [
            Self::Tiny,
            Self::Small,
            Self::Normal,
            Self::Large,
            Self::Huge,
        ];

        pub(crate) fn label(self) -> &'static str {
            match self {
                Self::Tiny => "Tiny",
                Self::Small => "Small",
                Self::Normal => "Normal",
                Self::Large => "Large",
                Self::Huge => "Huge",
            }
        }

        pub(crate) fn factor(self) -> f32 {
            match self {
                Self::Tiny => 0.5,
                Self::Small => 0.75,
                Self::Normal => 1.0,
                Self::Large => 1.35,
                Self::Huge => 1.75,
            }
        }
    }

    impl Default for RenderDistance {
        fn default() -> Self {
            Self::Normal
        }
    }

    impl RenderDistance {
        pub(crate) const ALL: [Self; 6] = [
            Self::Short,
            Self::Normal,
            Self::Far,
            Self::Extreme,
            Self::Ultra,
            Self::Insane,
        ];

        pub(crate) fn next(self) -> Self {
            match self {
                Self::Short => Self::Normal,
                Self::Normal => Self::Far,
                Self::Far => Self::Extreme,
                Self::Extreme => Self::Ultra,
                Self::Ultra => Self::Insane,
                Self::Insane => Self::Short,
            }
        }

        pub(crate) fn label(self) -> &'static str {
            match self {
                Self::Short => "Short",
                Self::Normal => "Normal",
                Self::Far => "Far",
                Self::Extreme => "Extreme",
                Self::Ultra => "Ultra",
                Self::Insane => "Insane",
            }
        }

        pub(crate) fn view_distance_chunks(self) -> i32 {
            match self {
                Self::Short => 4,
                Self::Normal => 8,
                Self::Far => 12,
                Self::Extreme => 16,
                Self::Ultra => 24,
                Self::Insane => 32,
            }
        }

        pub(crate) fn far_lod_requests_per_frame(self) -> usize {
            match self {
                Self::Short => 0,
                Self::Normal => 0,
                Self::Far => 1,
                Self::Extreme => 3,
                Self::Ultra => 5,
                Self::Insane => 8,
            }
        }

        pub(crate) fn far_lod_gpu_uploads_per_frame(self) -> usize {
            match self {
                Self::Short => 0,
                Self::Normal => 0,
                Self::Far => 1,
                Self::Extreme => 1,
                Self::Ultra => 2,
                Self::Insane => 4,
            }
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum SimulationDistance {
        Short,
        Normal,
        Far,
        Extreme,
    }

    impl Default for SimulationDistance {
        fn default() -> Self {
            Self::Normal
        }
    }

    impl SimulationDistance {
        pub(crate) fn next(self) -> Self {
            match self {
                Self::Short => Self::Normal,
                Self::Normal => Self::Far,
                Self::Far => Self::Extreme,
                Self::Extreme => Self::Short,
            }
        }

        pub(crate) fn label(self) -> &'static str {
            match self {
                Self::Short => "Short",
                Self::Normal => "Normal",
                Self::Far => "Far",
                Self::Extreme => "Extreme",
            }
        }

        pub(crate) fn distance_chunks(self) -> i32 {
            match self {
                Self::Short => 4,
                Self::Normal => 6,
                Self::Far => 8,
                Self::Extreme => 12,
            }
        }
    }

    pub(crate) const USE_GREEDY_MESHING: bool = true;
    pub(crate) const STREAM_DISPATCH_BUDGET_MS: f32 = 2.0;
    pub(crate) const STREAM_MESH_UPLOAD_BUDGET_MS: f32 = 3.0;
    pub(crate) const STREAM_CHUNKGEN_APPLY_BUDGET_MS: f32 = 3.0;
    pub(crate) const STREAM_BURST_DISPATCH_BUDGET_MS: f32 = 8.0;
    pub(crate) const STREAM_BURST_MESH_UPLOAD_BUDGET_MS: f32 = 8.0;
    pub(crate) const STREAM_BURST_CHUNKGEN_APPLY_BUDGET_MS: f32 = 8.0;
    pub(crate) const STREAM_BURST_MAX_TOTAL_BUDGET_MS: f32 = 25.0;
    pub(crate) const STREAM_BURST_MISSING_MESHES_THRESHOLD: usize = 24;
    pub(crate) const STREAM_BURST_NEAR_RADIUS_CHUNKS: i32 = 4;
    pub(crate) const NEAR_MESH_RADIUS_CHUNKS: i32 = 8;
    pub(crate) const HIGHLIGHT_FACE_OFFSET: f32 = 0.0015;
    pub(crate) const HIGHLIGHT_BLINK_MS: u128 = 250;
    pub(crate) const CROSSHAIR_THICKNESS_PX: f32 = 4.0;
    pub(crate) const CROSSHAIR_HALF_LEN_PX: f32 = 14.0;

    pub(crate) const FOG_START_DISTANCE: f32 = 40.0;
    pub(crate) const FOG_END_DISTANCE: f32 = 180.0;
    pub(crate) const FOG_RENDERDIST_START_RATIO: f32 = 0.55;
    pub(crate) const FOG_RENDERDIST_END_MARGIN_BLOCKS: f32 = 4.0;
    pub(crate) const FOG_DARKEN_STRENGTH: f32 = 0.22;
    pub(crate) const FOG_COLOR_DEFAULT: [f32; 3] = [0.08, 0.10, 0.14];
    pub(crate) const FOG_COLOR_BLEND_DEFAULT: f32 = 1.0;
    pub(crate) const WATER_FOG_START_DISTANCE: f32 = 0.5;
    pub(crate) const WATER_FOG_END_DISTANCE: f32 = 18.0;
    pub(crate) const WATER_FOG_DARKEN_STRENGTH: f32 = 0.62;
    pub(crate) const WATER_FOG_COLOR: [f32; 3] = [0.22, 0.43, 0.68];
    pub(crate) const WATER_FOG_COLOR_BLEND: f32 = 0.76;
    pub(crate) const LAVA_FOG_START_DISTANCE: f32 = 0.25;
    pub(crate) const LAVA_FOG_END_DISTANCE: f32 = 9.0;
    pub(crate) const LAVA_FOG_DARKEN_STRENGTH: f32 = 0.78;
    pub(crate) const LAVA_FOG_COLOR: [f32; 3] = [0.82, 0.28, 0.08];
    pub(crate) const LAVA_FOG_COLOR_BLEND: f32 = 0.84;

    pub(crate) const PARTICLE_SPAWN_COUNT: usize = 10;
    pub(crate) const PARTICLE_MAX_COUNT: usize = 2000;
    pub(crate) const PARTICLE_QUAD_SIZE: f32 = 0.11;
    pub(crate) const PARTICLE_GRAVITY: f32 = 12.0;
    pub(crate) const PARTICLE_SPEED_MIN: f32 = 1.8;
    pub(crate) const PARTICLE_SPEED_MAX: f32 = 4.6;
    pub(crate) const PARTICLE_LIFE_MIN: f32 = 0.35;
    pub(crate) const PARTICLE_LIFE_MAX: f32 = 0.80;

    pub(crate) const WATER_OVERLAY_TINT: [f32; 3] = [0.85, 0.92, 1.0];
    pub(crate) const WATER_OVERLAY_UV_TILE: f32 = 4.0;
    pub(crate) const WATER_OVERLAY_ALPHA_MIN: f32 = 0.15;
    pub(crate) const WATER_OVERLAY_ALPHA_MAX: f32 = 0.45;

    pub(crate) const ANIM_FLAG_ANIMATED: u32 = 1;
    pub(crate) const DEFAULT_FRAMETIME_TICKS: u32 = 1;
    pub(crate) const DEFAULT_WATER_FRAMETIME_TICKS: u32 = 2;
    pub(crate) const DEFAULT_LAVA_FRAMETIME_TICKS: u32 = 3;
    pub(crate) const UI_MODE_SOLID: u32 = 0;
    pub(crate) const UI_MODE_TEXTURED: u32 = 1;
    pub(crate) const UI_MODE_WATER_OVERLAY: u32 = 2;
    pub(crate) const UI_MODE_SOLID_ALPHA: u32 = 3;
    pub(crate) const MINECRAFT_TICK_SECONDS: f32 = 1.0 / 20.0;
}

pub(crate) mod sim {
    pub(crate) const RANDOM_TICK_SPEED_DEFAULT: u32 = 3;
}

#[cfg(test)]
mod tests {
    use super::render::RenderDistance;
    use super::render::{
        BASE_FOV_DEFAULT_DEG, BASE_FOV_MAX_DEG, BASE_FOV_MIN_DEG, BASE_FOV_STEP_DEG,
        base_fov_fill01, base_fov_from_fill01,
    };

    #[test]
    fn render_distance_extends_to_thirty_two_chunks() {
        assert_eq!(RenderDistance::Insane.view_distance_chunks(), 32);
        assert_eq!(RenderDistance::Ultra.view_distance_chunks(), 24);
    }

    #[test]
    fn far_lod_budgets_scale_with_render_distance() {
        assert_eq!(RenderDistance::Normal.far_lod_requests_per_frame(), 0);
        assert!(RenderDistance::Extreme.far_lod_requests_per_frame() > 0);
        assert!(
            RenderDistance::Insane.far_lod_requests_per_frame()
                > RenderDistance::Ultra.far_lod_requests_per_frame()
        );
        assert!(
            RenderDistance::Insane.far_lod_gpu_uploads_per_frame()
                >= RenderDistance::Ultra.far_lod_gpu_uploads_per_frame()
        );
    }

    #[test]
    fn fov_slider_ends_hit_the_limits() {
        assert_eq!(base_fov_from_fill01(0.0), BASE_FOV_MIN_DEG);
        assert_eq!(base_fov_from_fill01(1.0), BASE_FOV_MAX_DEG);
        // Out-of-range drags clamp instead of wrapping.
        assert_eq!(base_fov_from_fill01(-3.0), BASE_FOV_MIN_DEG);
        assert_eq!(base_fov_from_fill01(9.0), BASE_FOV_MAX_DEG);
        assert!((base_fov_fill01(BASE_FOV_MIN_DEG) - 0.0).abs() < 1e-6);
        assert!((base_fov_fill01(BASE_FOV_MAX_DEG) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn fov_slider_snaps_to_the_step_size() {
        for fill in 0..=100 {
            let deg = base_fov_from_fill01(fill as f32 / 100.0);
            assert_eq!(
                (deg - BASE_FOV_MIN_DEG) % BASE_FOV_STEP_DEG,
                0,
                "fill {fill} produced off-step {deg}"
            );
            assert!((BASE_FOV_MIN_DEG..=BASE_FOV_MAX_DEG).contains(&deg));
        }
    }

    #[test]
    fn fov_slider_roundtrips_every_step() {
        let mut deg = BASE_FOV_MIN_DEG;
        while deg <= BASE_FOV_MAX_DEG {
            assert_eq!(base_fov_from_fill01(base_fov_fill01(deg)), deg);
            deg += BASE_FOV_STEP_DEG;
        }
        assert_eq!(
            base_fov_from_fill01(base_fov_fill01(BASE_FOV_DEFAULT_DEG)),
            BASE_FOV_DEFAULT_DEG
        );
    }
}
