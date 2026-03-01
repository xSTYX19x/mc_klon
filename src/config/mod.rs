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
}

pub(crate) mod render {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum RenderDistance {
        Short,
        Normal,
        Far,
        Extreme,
    }

    impl Default for RenderDistance {
        fn default() -> Self {
            Self::Normal
        }
    }

    impl RenderDistance {
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

        pub(crate) fn view_distance_chunks(self) -> i32 {
            match self {
                Self::Short => 4,
                Self::Normal => 8,
                Self::Far => 12,
                Self::Extreme => 16,
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
    pub(crate) const PREVIEW_SIZE_PX: f32 = 44.0;
    pub(crate) const PREVIEW_MARGIN_TOP_PX: f32 = 20.0;
    pub(crate) const PREVIEW_MARGIN_RIGHT_PX: f32 = 20.0;
    pub(crate) const PREVIEW_ACCENT_STRIPE_PX: f32 = 10.0;
    pub(crate) const PREVIEW_FLIP_HORIZONTAL: bool = true;
    pub(crate) const PREVIEW_FLIP_VERTICAL: bool = false;

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
    pub(crate) const MINECRAFT_TICK_SECONDS: f32 = 1.0 / 20.0;
}

pub(crate) mod sim {
    pub(crate) const RANDOM_TICK_SPEED_DEFAULT: u32 = 3;
}
