use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use glam::{IVec3, Vec3};
use wgpu::SurfaceError;
use winit::{
    application::ApplicationHandler,
    event::{DeviceEvent, ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowId},
};

use crate::blocks::BlockId;
use crate::camera::Camera;
use crate::config::app::DOUBLE_SPACE_WINDOW;
use crate::config::player::PLAYER_TUNING_MINECRAFT;
use crate::config::render::{HIGHLIGHT_BLINK_MS, RenderDistance, SimulationDistance};
use crate::input::CameraController;
use crate::perf::trace as perf_trace;
use crate::perf::{PerfCat, PerfStats};
use crate::player::*;
use crate::render::State;
use crate::render::state::{
    DebugOverlay, GenerationOverlay, PauseOverlay, WorldGenParams, WorldGenRequest,
    pause_menu_item_at_pos_px,
};
use crate::world::{FluidKind, World};

const BLOCK_ACTION_REPEAT_COOLDOWN_TICKS: f32 = 4.0;
const DOUBLE_FORWARD_TAP_WINDOW: f32 = 0.25;
const FOV_EFFECT_SCALE: f32 = 1.0;
const FOV_EFFECT_SCALE_FLIGHT: f32 = 0.18;
const FOV_FLIGHT_MAX_MULTIPLIER: f32 = 1.05;
const FOV_RESPONSE_RATE: f32 = 10.0;
const FOV_MIN_DEG: f32 = 30.0;
const FOV_MAX_DEG: f32 = 150.0;
const BLOCK_CYCLE: [BlockId; 10] = [
    BlockId::Stone,
    BlockId::Cobblestone,
    BlockId::Dirt,
    BlockId::Grass,
    BlockId::Sand,
    BlockId::OakPlanks,
    BlockId::Obsidian,
    BlockId::WaterStill,
    BlockId::Sapling,
    BlockId::LavaStill,
];
const MOUSE_WHEEL_PIXEL_STEP: f32 = 40.0;
const PAUSE_MENU_ITEMS: [&str; 4] = [
    "Return to game",
    "Generate new level",
    "Save level",
    "Exit game",
];

enum GameState {
    InGame,
    GeneratingWorld { progress: f32, phase: String },
    Paused { selected: usize },
}

struct App {
    state: Option<State>,
    window: Option<Arc<Window>>,

    player: Player,
    camera: Camera,
    controller: CameraController,
    selected_slot: usize,
    render_distance: RenderDistance,
    simulation_distance: SimulationDistance,
    game_state: GameState,
    last_space_tap: Option<Instant>,
    last_forward_tap: Option<Instant>,
    base_fov_y_radians: f32,
    cursor_pos: (f32, f32),
    mouse_wheel_accum_y: f32,
    show_debug_overlay: bool,

    last_frame: Option<Instant>,
    app_start: Option<Instant>,
    fps_last_instant: Option<Instant>,
    fps_frame_count: u32,
    fps_value: u32,
    perf: PerfStats,
    break_held: bool,
    place_held: bool,
    break_cooldown_seconds: f32,
    place_cooldown_seconds: f32,
}

impl Default for App {
    fn default() -> Self {
        let camera = Camera::default();
        let base_fov_y_radians = camera.fov_y_radians;
        let player = Player::from_eye_position(camera.position);
        Self {
            window: None,
            state: None,
            player,
            camera,
            controller: CameraController::new(),
            selected_slot: 0,
            render_distance: RenderDistance::default(),
            simulation_distance: SimulationDistance::default(),
            game_state: GameState::InGame,
            last_space_tap: None,
            last_forward_tap: None,
            base_fov_y_radians,
            cursor_pos: (0.0, 0.0),
            mouse_wheel_accum_y: 0.0,
            show_debug_overlay: false,
            last_frame: None,
            app_start: None,
            fps_last_instant: None,
            fps_frame_count: 0,
            fps_value: 0,
            perf: PerfStats::new(),
            break_held: false,
            place_held: false,
            break_cooldown_seconds: 0.0,
            place_cooldown_seconds: 0.0,
        }
    }
}

impl App {
    fn block_action_repeat_cooldown_seconds() -> f32 {
        BLOCK_ACTION_REPEAT_COOLDOWN_TICKS / PLAYER_TUNING_MINECRAFT.mc_ticks_per_second
    }

    fn double_space_window() -> Duration {
        Duration::from_secs_f32(DOUBLE_SPACE_WINDOW)
    }

    fn double_forward_window() -> Duration {
        Duration::from_secs_f32(DOUBLE_FORWARD_TAP_WINDOW)
    }

    fn selected_block(&self) -> BlockId {
        BLOCK_CYCLE[self.selected_slot]
    }

    fn set_selected_slot(&mut self, selected_slot: usize) -> bool {
        if selected_slot >= BLOCK_CYCLE.len() || self.selected_slot == selected_slot {
            return false;
        }
        self.selected_slot = selected_slot;
        true
    }

    fn cycle_selected_block(&mut self, steps: i32) {
        let count = BLOCK_CYCLE.len();
        if count == 0 || steps == 0 {
            return;
        }
        let next = (self.selected_slot as i32 + steps).rem_euclid(count as i32) as usize;
        if self.set_selected_slot(next) {
            let selected = self.selected_block();
            println!("Selected block: {}", selected.display_name());
            if let Some(state) = self.state.as_mut() {
                state.rebuild_preview_mesh(selected);
            }
        }
    }

    fn toggle_debug_overlay(&mut self) {
        self.show_debug_overlay = !self.show_debug_overlay;
        println!(
            "debug overlay: {}",
            if self.show_debug_overlay { "on" } else { "off" }
        );
    }

    fn clear_double_space_state(&mut self) {
        self.last_space_tap = None;
    }

    fn is_generating_world(&self) -> bool {
        matches!(self.game_state, GameState::GeneratingWorld { .. })
    }

    fn is_paused(&self) -> bool {
        matches!(self.game_state, GameState::Paused { .. })
    }

    fn is_in_game(&self) -> bool {
        matches!(self.game_state, GameState::InGame)
    }

    fn clear_action_state(&mut self) {
        self.break_held = false;
        self.place_held = false;
        self.break_cooldown_seconds = 0.0;
        self.place_cooldown_seconds = 0.0;
        self.last_forward_tap = None;
        self.controller.clear_inputs();
    }

    fn enter_pause(&mut self) {
        self.game_state = GameState::Paused { selected: 0 };
        self.clear_action_state();
        if let Some(window) = self.window.as_ref() {
            self.controller.set_mouse_capture(window, false);
        }
    }

    fn leave_pause(&mut self) {
        self.game_state = GameState::InGame;
        self.clear_action_state();
        if let Some(window) = self.window.as_ref() {
            self.controller.set_mouse_capture(window, true);
        }
    }

    fn move_pause_selection(&mut self, delta: i32) {
        let count = PAUSE_MENU_ITEMS.len();
        if count == 0 {
            return;
        }
        if let GameState::Paused { selected } = &mut self.game_state {
            let next = (*selected as i32 + delta).rem_euclid(count as i32) as usize;
            *selected = next;
        }
    }

    fn start_new_level_generation(&mut self, request: WorldGenRequest) -> bool {
        let started = if let Some(state) = self.state.as_mut() {
            state.begin_new_world(request.seed, request.params.preset);
            state.is_generating_new_level()
        } else {
            false
        };

        if started {
            self.game_state = GameState::GeneratingWorld {
                progress: 0.0,
                phase: "Clearing".to_string(),
            };
            self.clear_action_state();
            if let Some(window) = self.window.as_ref() {
                self.controller.set_mouse_capture(window, false);
            }
        }

        started
    }

    fn activate_pause_menu_selected(&mut self, event_loop: &ActiveEventLoop) {
        let selected = match &self.game_state {
            GameState::Paused { selected } => *selected,
            _ => return,
        };

        match selected {
            0 => self.leave_pause(),
            1 => {
                let request = self.make_worldgen_request();
                if !self.start_new_level_generation(request) {
                    println!("new level generation is already running");
                }
            }
            2 => {
                if let Some(state) = self.state.as_mut() {
                    state.save_world();
                    println!("Saved.");
                }
            }
            3 => {
                if let Some(state) = self.state.as_mut() {
                    state.save_world();
                }
                event_loop.exit();
            }
            _ => {}
        }
    }

    fn pause_menu_item_at_cursor(&self) -> Option<usize> {
        let window = self.window.as_ref()?;
        let size = window.inner_size();
        pause_menu_item_at_pos_px(
            size.width,
            size.height,
            PAUSE_MENU_ITEMS.len(),
            self.cursor_pos.0,
            self.cursor_pos.1,
        )
    }

    fn build_debug_overlay_lines(&self) -> Vec<String> {
        let mut lines = Vec::with_capacity(18);
        for segment in self.perf.format_title_text(self.fps_value).split(" | ") {
            let s = segment.trim();
            if !s.is_empty() {
                lines.push(s.to_string());
            }
        }

        let pos = self.camera.position;
        let world_pos = IVec3::new(
            pos.x.floor() as i32,
            pos.y.floor() as i32,
            pos.z.floor() as i32,
        );
        let chunk = World::world_to_chunk_coord(world_pos);
        lines.push(format!("XYZ {:.1} {:.1} {:.1}", pos.x, pos.y, pos.z));
        lines.push(format!("Chunk {} {} {}", chunk.x, chunk.y, chunk.z));
        lines.push(format!(
            "Render distance: {} ({} chunks)",
            self.render_distance.label(),
            self.render_distance.view_distance_chunks()
        ));
        lines.push(format!(
            "Simulation distance: {} ({} chunks)",
            self.simulation_distance.label(),
            self.simulation_distance
                .distance_chunks()
                .min(self.render_distance.view_distance_chunks())
        ));
        lines.push(format!(
            "Selected block: {} ({}/{})",
            self.selected_block().display_name(),
            self.selected_slot + 1,
            BLOCK_CYCLE.len()
        ));
        if let Some(state) = self.state.as_ref() {
            lines.push(state.simulation_debug_line());
            lines.push(state.ticking_areas_debug_line());
            lines.push(state.sim_systems_debug_line());
            lines.push(state.block_entities_debug_line());
            lines.push(state.random_ticks_debug_line());
            lines.push(state.world_manifest_debug_line());
            lines.push(format!(
                "Lighting step (CPU): {:.3} ms",
                state.lighting_step_last_ms()
            ));
            lines.push(state.lighting_pending_debug_line());
        }

        lines
    }

    fn world_spawn_settled_feet(state: &mut State) -> (Vec3, bool) {
        let spawn_x = 0;
        let spawn_z = 0;
        state.ensure_column_loaded_or_generated(spawn_x, spawn_z);
        let terrain_y = state.world.terrain_height_at(spawn_x, spawn_z) as f32;
        let spawn_feet = Vec3::new(spawn_x as f32 + 0.5, terrain_y + 2.0, spawn_z as f32 + 0.5);
        Self::settle_spawn_feet(&state.world, spawn_feet)
    }

    fn settle_spawn_feet(world: &World, spawn_feet: Vec3) -> (Vec3, bool) {
        let (settled_feet, settle_flags) =
            move_with_collisions(world, spawn_feet, Vec3::new(0.0, -4.0, 0.0));
        (settled_feet, settle_flags.hit_y_down)
    }

    fn apply_settled_spawn(&mut self, settled_feet: Vec3, on_ground: bool) {
        self.player.feet_position = settled_feet;
        self.player.mode = MoveMode::Flight;
        self.player._velocity_y = 0.0;
        self.player.walk_velocity = Vec3::ZERO;
        self.player.swim_velocity = Vec3::ZERO;
        self.player.bob_phase = 0.0;
        self.player.bob_anchor_y = settled_feet.y;
        self.player.was_at_surface = false;
        self.player.sprint_swim_active = false;
        self.player.is_swim_prone = false;
        self.player.jump_hold_cooldown_seconds = 0.0;
        self.player.on_ground = on_ground;
        self.player.time_since_grounded = if self.player.on_ground {
            0.0
        } else {
            f32::INFINITY
        };
        self.controller.clear_jump_requested();
        self.clear_double_space_state();
        self.camera.position = self.player.eye_position();
    }

    fn place_player_at_world_spawn(&mut self, state: &mut State) {
        let (settled_feet, on_ground) = Self::world_spawn_settled_feet(state);
        self.apply_settled_spawn(settled_feet, on_ground);
    }

    fn make_worldgen_request(&self) -> WorldGenRequest {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let nanos = now.as_nanos() as u64;
        WorldGenRequest {
            seed: nanos ^ (nanos.rotate_left(23)),
            params: WorldGenParams::default(),
        }
    }

    fn toggle_move_mode(&mut self) {
        self.player.mode = match self.player.mode {
            MoveMode::Flight => {
                self.player._velocity_y = 0.0;
                self.player.walk_velocity = Vec3::ZERO;
                self.player.swim_velocity = Vec3::ZERO;
                self.player.bob_phase = 0.0;
                self.player.was_at_surface = false;
                self.player.sprint_swim_active = false;
                self.player.jump_hold_cooldown_seconds = 0.0;
                let (snapped, grounded) = if let Some(state) = self.state.as_ref() {
                    snap_to_ground_with_height(
                        &state.world,
                        self.player.feet_position,
                        self.player.collision_height(),
                    )
                } else {
                    (self.player.feet_position, false)
                };
                self.player.feet_position = snapped;
                self.player.on_ground = grounded;
                self.player.time_since_grounded = if grounded { 0.0 } else { f32::INFINITY };
                self.controller.clear_jump_requested();
                MoveMode::Walk
            }
            MoveMode::Walk | MoveMode::Swim { .. } => {
                self.player._velocity_y = 0.0;
                self.player.walk_velocity = Vec3::ZERO;
                self.player.swim_velocity = Vec3::ZERO;
                self.player.bob_phase = 0.0;
                self.player.was_at_surface = false;
                self.player.sprint_swim_active = false;
                self.player.is_swim_prone = false;
                self.player.jump_hold_cooldown_seconds = 0.0;
                self.player.on_ground = false;
                self.player.time_since_grounded = f32::INFINITY;
                self.controller.clear_jump_requested();
                MoveMode::Flight
            }
        };
        self.clear_double_space_state();
        println!(
            "Move mode: {}",
            match self.player.mode {
                MoveMode::Flight => "Flight",
                MoveMode::Walk => "Walk",
                MoveMode::Swim {
                    kind: FluidKind::Water,
                } => "Swim (Water)",
                MoveMode::Swim {
                    kind: FluidKind::Lava,
                } => "Swim (Lava)",
            }
        );
        self.camera.position = self.player.eye_position();
    }

    /// Returns true if this key press was consumed by a double-space toggle.
    fn handle_space_pressed(&mut self, now: Instant) -> bool {
        let is_double_tap = self
            .last_space_tap
            .is_some_and(|last| now.saturating_duration_since(last) <= Self::double_space_window());
        if is_double_tap {
            self.last_space_tap = None;
            self.toggle_move_mode();
            return true;
        }

        self.last_space_tap = Some(now);
        if matches!(self.player.mode, MoveMode::Walk | MoveMode::Swim { .. })
            && self.player.on_ground
        {
            self.controller.request_jump();
        }
        false
    }

    fn handle_forward_pressed(&mut self, now: Instant) {
        let is_double_tap = self.last_forward_tap.is_some_and(|last| {
            now.saturating_duration_since(last) <= Self::double_forward_window()
        });
        if is_double_tap {
            self.controller.set_sprint_latched(true);
        }
        self.last_forward_tap = Some(now);
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let _trace_scope = perf_trace::scope("app.resumed");
        let window = event_loop
            .create_window(
                Window::default_attributes().with_title("Minecraft Klon – Würfel + Kamera"),
            )
            .expect("Failed to create window");
        let window = Arc::new(window);

        window.set_cursor_visible(true);

        let mut state = pollster::block_on(State::new(window.clone()));
        state.rebuild_preview_mesh(self.selected_block());

        self.place_player_at_world_spawn(&mut state);
        state.set_render_distance(self.render_distance, &self.camera);
        state.set_simulation_distance(self.simulation_distance, &self.camera);
        self.game_state = GameState::InGame;

        self.window = Some(window);
        self.state = Some(state);
        self.last_frame = Some(Instant::now());
        self.app_start = Some(Instant::now());
        self.fps_last_instant = Some(Instant::now());
        self.fps_frame_count = 0;
        self.fps_value = 0;
    }

    /// Raw device events: ideal für FPS-look (MouseMotion liefert deltas)
    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: winit::event::DeviceId,
        event: DeviceEvent,
    ) {
        match event {
            DeviceEvent::MouseMotion { delta: (dx, dy) } => {
                self.controller
                    .process_mouse_motion(&mut self.camera, dx, dy);
            }
            _ => {}
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(window_id) = self.window.as_ref().map(|window| window.id()) else {
            return;
        };
        if window_id != id {
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                if let Some(state) = self.state.as_mut() {
                    state.save_world();
                }
                event_loop.exit();
            }

            WindowEvent::Resized(size) => {
                if let Some(state) = self.state.as_mut() {
                    state.resize(size);
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = (position.x as f32, position.y as f32);
                if self.is_paused() {
                    if let Some(index) = self.pause_menu_item_at_cursor() {
                        if let GameState::Paused { selected } = &mut self.game_state {
                            *selected = index;
                        }
                    }
                }
            }

            // Tastatur: WASD/Space/Shift, Double-Space toggelt Flight/Walking, ESC öffnet/schließt Menü
            WindowEvent::KeyboardInput { event, .. } => {
                if matches!(event.physical_key, PhysicalKey::Code(KeyCode::Escape))
                    && event.state == ElementState::Pressed
                {
                    if self.is_generating_world() {
                        return;
                    }
                    if self.is_paused() {
                        self.leave_pause();
                    } else {
                        self.enter_pause();
                    }
                    return;
                }

                let generating = self.is_generating_world();
                let paused = self.is_paused();
                if paused {
                    if event.state == ElementState::Pressed {
                        if let PhysicalKey::Code(code) = event.physical_key {
                            if code == KeyCode::F3 && !event.repeat {
                                self.toggle_debug_overlay();
                                return;
                            }
                            if event.repeat {
                                return;
                            }
                            match code {
                                KeyCode::ArrowUp | KeyCode::KeyW => self.move_pause_selection(-1),
                                KeyCode::ArrowDown | KeyCode::KeyS => self.move_pause_selection(1),
                                KeyCode::Enter | KeyCode::NumpadEnter | KeyCode::Space => {
                                    self.activate_pause_menu_selected(event_loop)
                                }
                                _ => {}
                            }
                        }
                    }
                    return;
                }

                if !generating {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        match (code, event.state) {
                            (KeyCode::KeyW, ElementState::Pressed) if !event.repeat => {
                                self.handle_forward_pressed(Instant::now());
                            }
                            (KeyCode::KeyW, ElementState::Released) => {
                                self.controller.set_sprint_latched(false);
                            }
                            (KeyCode::KeyS, ElementState::Pressed)
                            | (KeyCode::ShiftLeft, ElementState::Pressed)
                            | (KeyCode::ShiftRight, ElementState::Pressed) => {
                                self.controller.set_sprint_latched(false);
                            }
                            _ => {}
                        }
                    }
                }
                if event.state == ElementState::Pressed {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        if !generating
                            && code == KeyCode::KeyT
                            && !event.repeat
                            && self.controller.sprint_held
                        {
                            if let Some(state) = self.state.as_mut() {
                                let chunk = World::world_to_chunk_coord(IVec3::new(
                                    self.camera.position.x.floor() as i32,
                                    self.camera.position.y.floor() as i32,
                                    self.camera.position.z.floor() as i32,
                                ));
                                if self.controller.move_down {
                                    match state.delete_nearest_ticking_area(chunk.x, chunk.z) {
                                        Some(id) => println!(
                                            "Ticking area removed: id={} near chunk ({}, {})",
                                            id, chunk.x, chunk.z
                                        ),
                                        None => println!("No ticking area to remove."),
                                    }
                                } else {
                                    let id = state.create_ticking_area_radius(chunk.x, chunk.z, 4);
                                    println!(
                                        "Ticking area created: id={} center=({}, {}) radius=4",
                                        id, chunk.x, chunk.z
                                    );
                                }
                                state.update_simulation_activation(&self.camera);
                            }
                            return;
                        }

                        if code == KeyCode::KeyN && !event.repeat {
                            if generating {
                                println!("new level generation is already running");
                            } else {
                                let request = self.make_worldgen_request();
                                self.start_new_level_generation(request);
                            }
                        }

                        if !generating && code == KeyCode::Space && !event.repeat {
                            self.handle_space_pressed(Instant::now());
                        }

                        if code == KeyCode::F3 && !event.repeat {
                            self.toggle_debug_overlay();
                        }

                        if !generating && code == KeyCode::F5 && !event.repeat {
                            if let Some(state) = self.state.as_mut() {
                                state.toggle_debug_culling();
                            }
                        }

                        if !generating && code == KeyCode::F4 && !event.repeat {
                            if let Some(state) = self.state.as_mut() {
                                state.toggle_debug_depth_test();
                            }
                        }

                        if !generating && code == KeyCode::F6 && !event.repeat {
                            if let Some(state) = self.state.as_mut() {
                                state.toggle_far_lod_enabled();
                            }
                        }

                        if !generating && code == KeyCode::F7 && !event.repeat {
                            if let Some(state) = self.state.as_mut() {
                                state.toggle_debug_disable_ao();
                            }
                        }

                        if !generating && code == KeyCode::F8 && !event.repeat {
                            if let Some(state) = self.state.as_mut() {
                                state.cycle_debug_lighting_view();
                            }
                        }

                        if !generating && code == KeyCode::KeyF && !event.repeat {
                            self.render_distance = self.render_distance.next();
                            if let Some(state) = self.state.as_mut() {
                                state.set_render_distance(self.render_distance, &self.camera);
                            }
                            println!(
                                "Render distance: {} ({} chunks)",
                                self.render_distance.label(),
                                self.render_distance.view_distance_chunks()
                            );
                        }

                        if !generating && code == KeyCode::KeyG && !event.repeat {
                            self.simulation_distance = self.simulation_distance.next();
                            if let Some(state) = self.state.as_mut() {
                                state.set_simulation_distance(
                                    self.simulation_distance,
                                    &self.camera,
                                );
                            }
                            println!(
                                "Simulation distance: {} ({} chunks)",
                                self.simulation_distance.label(),
                                self.simulation_distance
                                    .distance_chunks()
                                    .min(self.render_distance.view_distance_chunks())
                            );
                        }

                        if !generating && matches!(code, KeyCode::Enter | KeyCode::NumpadEnter) {
                            if let Some(state) = self.state.as_mut() {
                                state.save_world();
                            }
                        }
                    }
                }

                if !generating {
                    self.controller.process_key(&event);
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                if self.is_generating_world() || self.is_paused() {
                    return;
                }

                let mut steps = 0_i32;
                match delta {
                    MouseScrollDelta::LineDelta(_, y) => {
                        if y > 0.0 {
                            steps = -1;
                        } else if y < 0.0 {
                            steps = 1;
                        }
                    }
                    MouseScrollDelta::PixelDelta(p) => {
                        self.mouse_wheel_accum_y += p.y as f32;
                        while self.mouse_wheel_accum_y >= MOUSE_WHEEL_PIXEL_STEP {
                            steps -= 1;
                            self.mouse_wheel_accum_y -= MOUSE_WHEEL_PIXEL_STEP;
                        }
                        while self.mouse_wheel_accum_y <= -MOUSE_WHEEL_PIXEL_STEP {
                            steps += 1;
                            self.mouse_wheel_accum_y += MOUSE_WHEEL_PIXEL_STEP;
                        }
                    }
                }

                if steps != 0 {
                    self.cycle_selected_block(steps);
                }
            }

            // Minecraft-übliches Mapping:
            // LMB = abbauen, RMB = platzieren.
            WindowEvent::MouseInput {
                state: button_state,
                button,
                ..
            } => {
                if self.is_generating_world() {
                    return;
                }

                if self.is_paused() {
                    if button_state == ElementState::Pressed && button == MouseButton::Left {
                        if let Some(index) = self.pause_menu_item_at_cursor() {
                            if let GameState::Paused { selected } = &mut self.game_state {
                                *selected = index;
                            }
                            self.activate_pause_menu_selected(event_loop);
                        }
                    }
                    return;
                }

                if button_state == ElementState::Pressed {
                    if !self.controller.mouse_captured {
                        if let Some(window) = self.window.as_ref() {
                            self.controller.set_mouse_capture(window, true);
                        }
                        return;
                    }

                    let selected_block = self.selected_block();
                    if let Some(state) = self.state.as_mut() {
                        match button {
                            MouseButton::Left => {
                                self.break_held = true;
                                state.destroy_block_at_camera_ray(&self.camera);
                                self.break_cooldown_seconds =
                                    Self::block_action_repeat_cooldown_seconds();
                            }
                            MouseButton::Right => {
                                self.place_held = true;
                                state.place_block_at_camera_ray(
                                    &self.camera,
                                    selected_block,
                                    self.player.feet_position,
                                );
                                self.place_cooldown_seconds =
                                    Self::block_action_repeat_cooldown_seconds();
                            }
                            _ => {}
                        }
                    }
                } else if button_state == ElementState::Released {
                    match button {
                        MouseButton::Left => {
                            self.break_held = false;
                        }
                        MouseButton::Right => {
                            self.place_held = false;
                        }
                        _ => {}
                    }
                }
            }

            // Redraw -> Update (dt, camera), Uniform schreiben, Render
            WindowEvent::RedrawRequested => {
                let _frame_trace = perf_trace::scope("app.frame");
                // dt in Sekunden (Zeit seit letztem Frame)
                let now = Instant::now();
                let dt = self
                    .last_frame
                    .replace(now)
                    .map(|prev| (now - prev).as_secs_f32())
                    .unwrap_or(0.016);
                self.perf.begin_frame();
                let current_window_size = self
                    .window
                    .as_ref()
                    .map(|window| window.inner_size())
                    .unwrap_or_default();
                let selected_block = self.selected_block();
                let mut completed_spawn: Option<Vec3> = None;

                if let Some(state) = self.state.as_mut() {
                    if let GameState::GeneratingWorld { progress, phase } = &mut self.game_state {
                        while let Some(update) = state.poll_worldgen_progress() {
                            *progress = update.progress01.clamp(0.0, 1.0);
                            *phase = update.phase;
                        }
                        completed_spawn = state.try_finish_new_level_generation(&self.camera);
                    }
                }

                if let Some(spawn_feet) = completed_spawn {
                    self.clear_action_state();
                    let spawn_data = self
                        .state
                        .as_ref()
                        .map(|state| Self::settle_spawn_feet(&state.world, spawn_feet));
                    if let Some((settled_feet, on_ground)) = spawn_data {
                        self.apply_settled_spawn(settled_feet, on_ground);
                    }
                    if let Some(state) = self.state.as_mut() {
                        state.set_render_distance(self.render_distance, &self.camera);
                    }
                    self.game_state = GameState::InGame;
                }

                if self.is_generating_world()
                    && self
                        .state
                        .as_ref()
                        .is_some_and(|state| !state.is_generating_new_level())
                {
                    self.game_state = GameState::InGame;
                }

                if self.is_generating_world() {
                    self.clear_action_state();
                }

                let in_game = self.is_in_game();
                let debug_lines_owned = if self.show_debug_overlay && in_game {
                    Some(self.build_debug_overlay_lines())
                } else {
                    None
                };
                let debug_line_refs = debug_lines_owned
                    .as_ref()
                    .map(|lines| lines.iter().map(|line| line.as_str()).collect::<Vec<_>>());
                if let Some(state) = self.state.as_mut() {
                    let _update_trace = perf_trace::scope("app.update");
                    state.begin_frame_profiling(dt * 1000.0);
                    state.world_time.advance(dt);

                    if in_game {
                        let dt_non_negative = dt.max(0.0);
                        {
                            let _perf_scope = self.perf.scope(PerfCat::Movement);
                            if !matches!(self.player.mode, MoveMode::Walk)
                                || !self.controller.move_forward
                                || self.controller.move_backward
                                || self.controller.move_down
                            {
                                self.controller.set_sprint_latched(false);
                            }
                            let fluid_state_before =
                                PlayerFluidState::sample(&state.world, &self.player);
                            apply_swim_mode_transition(&mut self.player, &fluid_state_before);
                            update_player_pose_for_fluids(
                                &state.world,
                                &self.controller,
                                &mut self.player,
                                &fluid_state_before,
                            );

                            match self.player.mode {
                                MoveMode::Flight => {
                                    update_flight_movement(
                                        &state.world,
                                        &self.camera,
                                        &mut self.controller,
                                        &mut self.player,
                                        &mut self.last_space_tap,
                                        dt,
                                    );
                                    update_grounded_timer(&mut self.player, dt);
                                }
                                MoveMode::Swim { kind } => {
                                    update_swim_movement(
                                        &state.world,
                                        &self.camera,
                                        &mut self.controller,
                                        &mut self.player,
                                        kind,
                                        &fluid_state_before,
                                        dt,
                                    );
                                    update_grounded_timer(&mut self.player, dt);
                                }
                                MoveMode::Walk => {
                                    self.player.swim_velocity = Vec3::ZERO;
                                    update_walk_movement(
                                        &state.world,
                                        &self.camera,
                                        &mut self.controller,
                                        &mut self.player,
                                        &fluid_state_before,
                                        dt,
                                    );
                                    update_grounded_timer(&mut self.player, dt);
                                }
                            }

                            let fluid_state_after_move =
                                PlayerFluidState::sample(&state.world, &self.player);
                            apply_swim_mode_transition(&mut self.player, &fluid_state_after_move);
                            update_player_pose_for_fluids(
                                &state.world,
                                &self.controller,
                                &mut self.player,
                                &fluid_state_after_move,
                            );
                            self.camera.position = self.player.eye_position();

                            self.break_cooldown_seconds =
                                (self.break_cooldown_seconds - dt_non_negative).max(0.0);
                            self.place_cooldown_seconds =
                                (self.place_cooldown_seconds - dt_non_negative).max(0.0);
                            if self.controller.mouse_captured {
                                if self.break_held && self.break_cooldown_seconds <= f32::EPSILON {
                                    state.destroy_block_at_camera_ray(&self.camera);
                                    self.break_cooldown_seconds =
                                        Self::block_action_repeat_cooldown_seconds();
                                }
                                if self.place_held && self.place_cooldown_seconds <= f32::EPSILON {
                                    state.place_block_at_camera_ray(
                                        &self.camera,
                                        selected_block,
                                        self.player.feet_position,
                                    );
                                    self.place_cooldown_seconds =
                                        Self::block_action_repeat_cooldown_seconds();
                                }
                            } else {
                                self.break_held = false;
                                self.place_held = false;
                            }
                        }

                        {
                            let _perf_scope = self.perf.scope(PerfCat::Fluids);
                            state.update_simulation_activation(&self.camera);
                            state.update_fluid_simulation(dt_non_negative);
                        }
                        self.perf.set_extra_suffix(state.fluid_debug_suffix());
                        {
                            let _perf_scope = self.perf.scope(PerfCat::Particles);
                            state.update_particles(dt_non_negative);
                        }
                        let blink_visible = self
                            .app_start
                            .map(|start| ((now - start).as_millis() / HIGHLIGHT_BLINK_MS) % 2 == 0)
                            .unwrap_or(true);
                        {
                            let _perf_scope = self.perf.scope(PerfCat::Highlight);
                            state.update_face_highlight(&self.camera, blink_visible);
                        }
                    } else {
                        let _perf_scope = self.perf.scope(PerfCat::Highlight);
                        state.clear_targeting_visuals();
                    }

                    {
                        let _perf_scope = self.perf.scope(PerfCat::ChunkStream);
                        state.process_dirty_chunk_meshes(&self.camera);
                    }
                    state.pump_mesh_results(&self.camera);
                    {
                        let _perf_scope = self.perf.scope(PerfCat::TexAnim);
                        state.update_texture_animations(dt);
                    }

                    let target_fov = if !in_game {
                        self.base_fov_y_radians
                    } else {
                        let mode = self.player.mode;
                        let current_speed = match mode {
                            MoveMode::Walk => Vec3::new(
                                self.player.walk_velocity.x,
                                0.0,
                                self.player.walk_velocity.z,
                            )
                            .length(),
                            MoveMode::Swim { .. } => self.player.swim_velocity.length(),
                            MoveMode::Flight => Vec3::new(
                                self.player.walk_velocity.x,
                                0.0,
                                self.player.walk_velocity.z,
                            )
                            .length(),
                        };
                        let base_walk_speed = PLAYER_TUNING_MINECRAFT.walk_speed.max(0.001);
                        let ratio = (current_speed / base_walk_speed).max(1.0);
                        let mut m = (ratio + 1.0) * 0.5;
                        m = match mode {
                            MoveMode::Flight => {
                                let flight_m = 1.0 + (m - 1.0) * FOV_EFFECT_SCALE_FLIGHT;
                                flight_m.min(FOV_FLIGHT_MAX_MULTIPLIER)
                            }
                            _ => 1.0 + (m - 1.0) * FOV_EFFECT_SCALE,
                        };
                        self.base_fov_y_radians * m
                    };
                    let fov_alpha = 1.0 - (-FOV_RESPONSE_RATE * dt.max(0.0)).exp();
                    self.camera.fov_y_radians +=
                        (target_fov - self.camera.fov_y_radians) * fov_alpha;
                    self.camera.fov_y_radians = self
                        .camera
                        .fov_y_radians
                        .clamp(FOV_MIN_DEG.to_radians(), FOV_MAX_DEG.to_radians());

                    let fluid_state_after = PlayerFluidState::sample(&state.world, &self.player);
                    let underwater_kind = fluid_state_after.underwater_kind;
                    let overlay_alpha = underwater_overlay_alpha(
                        &state.world,
                        self.camera.position,
                        underwater_kind,
                    );
                    let world_time = state.world_time;
                    {
                        let _perf_scope = self.perf.scope(PerfCat::Uniforms);
                        state.update_camera_uniform(
                            &self.camera,
                            underwater_kind,
                            overlay_alpha,
                            &world_time,
                        );
                    }

                    let generation_overlay_owned = match &self.game_state {
                        GameState::GeneratingWorld { progress, phase } => {
                            Some((*progress, phase.clone()))
                        }
                        GameState::InGame | GameState::Paused { .. } => None,
                    };
                    let generation_overlay =
                        generation_overlay_owned.as_ref().map(|(progress, phase)| {
                            GenerationOverlay {
                                progress01: *progress,
                                phase: phase.as_str(),
                            }
                        });
                    let pause_overlay = match &self.game_state {
                        GameState::Paused { selected } => Some(PauseOverlay {
                            selected: *selected,
                            items: &PAUSE_MENU_ITEMS,
                        }),
                        _ => None,
                    };
                    let debug_overlay = debug_line_refs.as_ref().map(|lines| DebugOverlay {
                        lines: lines.as_slice(),
                    });

                    let render_result = {
                        let _render_trace = perf_trace::scope("app.render");
                        let _perf_scope = self.perf.scope(PerfCat::Render);
                        state.render(
                            &self.camera,
                            underwater_kind,
                            generation_overlay,
                            pause_overlay,
                            debug_overlay,
                        )
                    };
                    match render_result {
                        Ok(_) => {}
                        Err(SurfaceError::Lost) => state.resize(current_window_size),
                        Err(SurfaceError::OutOfMemory) => event_loop.exit(),
                        Err(_) => {}
                    }
                }

                // Kontinuierliches Rendern wie in Games
                self.fps_frame_count = self.fps_frame_count.saturating_add(1);
                if let Some(last) = self.fps_last_instant {
                    let elapsed = now - last;
                    if elapsed.as_secs_f32() >= 1.0 {
                        self.fps_value =
                            (self.fps_frame_count as f32 / elapsed.as_secs_f32()).round() as u32;
                        self.fps_frame_count = 0;
                        self.fps_last_instant = Some(now);
                    }
                }
                let frame_ms = self.perf.end_frame();
                if self
                    .state
                    .as_ref()
                    .is_some_and(|state| state.should_report_stall(frame_ms))
                {
                    if let Some(state) = self.state.as_ref() {
                        let summary = state.format_stall_report(frame_ms);
                        eprintln!("{summary}");
                        let _ = perf_trace::write_stall_report(&summary);
                    }
                }
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }

            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

pub(crate) fn run() -> Result<(), Box<dyn std::error::Error>> {
    let _trace_scope = perf_trace::scope("app.run");
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::default();
    event_loop.run_app(&mut app)?;
    Ok(())
}
