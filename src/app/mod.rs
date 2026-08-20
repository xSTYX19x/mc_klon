use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use glam::{IVec3, Vec3};
use wgpu::SurfaceError;
use winit::{
    application::ApplicationHandler,
    event::{DeviceEvent, ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowId},
};

use std::path::PathBuf;

use crate::blocks::{BlockId, CLASSIC_PALETTE};
use crate::camera::{Camera, CameraMode, ViewBobbing};
use crate::commands::{self, Command};
use crate::config::app::DOUBLE_SPACE_WINDOW;
use crate::config::player::PLAYER_TUNING_MINECRAFT;
use crate::config::render::{
    BASE_FOV_MAX_DEG, BASE_FOV_MIN_DEG, BASE_FOV_STEP_DEG, GuiScale, HIGHLIGHT_BLINK_MS,
    RenderDistance, SimulationDistance, base_fov_fill01, base_fov_from_fill01,
};
use crate::input::CameraController;
use crate::perf::trace as perf_trace;
use crate::perf::{PerfCat, PerfStats};
use crate::player::*;
use crate::render::State;
use crate::render::player_model::PlayerModelView;
use crate::render::state::{
    ChatOverlay, ChatOverlayLine, DebugOverlay, GenerationOverlay, HitchCounters, InventoryOverlay,
    MenuOverlay, SavedWorldSummary, WorldGenParams, WorldGenRequest, WorldLoadStageStats,
    menu_slider_fill_at_pos_px, pause_menu_item_at_pos_px,
};
use crate::settings::{AppSettings, InputAction, SettingsScope, keycode_label};
use crate::storage::world_meta;
use crate::world::ticking_areas::{TICKING_AREA_MAX_RADIUS_CHUNKS, TickingArea, TickingFlags};
use crate::world::{FluidKind, World, WorldEnvironment};
use crate::worldgen::ClassicLevelSize;

const BLOCK_ACTION_REPEAT_COOLDOWN_TICKS: f32 = 4.0;
const DOUBLE_FORWARD_TAP_WINDOW: f32 = 0.25;
const DOUBLE_TAP_WINDOW_MULTIPLIER: f32 = 1.20;
const FOV_EFFECT_SCALE: f32 = 1.0;
const FOV_EFFECT_SCALE_FLIGHT: f32 = 0.18;
const FOV_FLIGHT_MAX_MULTIPLIER: f32 = 1.05;
const FOV_RESPONSE_RATE: f32 = 10.0;
const FOV_MIN_DEG: f32 = 30.0;
const THIRD_PERSON_DISTANCE_BLOCKS: f32 = 4.0;
/// Half-size of the box swept along the orbit ray, so the near plane cannot
/// poke through a wall once the camera is pulled in.
const THIRD_PERSON_PROBE_RADIUS: f32 = 0.12;
const FOV_MAX_DEG: f32 = 150.0;
const HOTBAR_SLOT_COUNT: usize = 9;
const SPAWN_SEARCH_RADIUS_BLOCKS: i32 = 8;
const MOUSE_WHEEL_PIXEL_STEP: f32 = 40.0;
const CHAT_MAX_LINES: usize = 64;
const CHAT_VISIBLE_LINES: usize = 16;
const CHAT_SENT_HISTORY_MAX: usize = 64;
const CHAT_PLAYER_NAME: &str = "Player";
const HITCH_LOG_THRESHOLD_MS: f32 = 25.0;
const MAIN_MENU_ITEMS: [&str; 4] = ["Generate new level", "Load level", "Options", "Exit game"];
const PAUSE_MENU_ITEMS: [&str; 6] = [
    "Back to game",
    "Generate new level",
    "Load level",
    "Save level",
    "Options",
    "Exit to main menu",
];
const OPTIONS_ROOT_ITEMS: [&str; 3] = ["General / Gameplay", "Video / Graphics", "Controls"];
const OPTIONS_GAMEPLAY_ROW_FOV: usize = 1;
const TICKING_EDIT_FLAG_ROWS: [(u32, &str); 4] = [
    (TickingFlags::KEEP_LOADED, "Keep loaded"),
    (TickingFlags::TICK_FLUIDS, "Tick fluids"),
    (TickingFlags::TICK_LIGHTING, "Tick lighting"),
    (TickingFlags::TICK_ENTITIES, "Tick entities"),
];
const TICKING_EDIT_ROW_RADIUS: usize = 0;
const TICKING_EDIT_ROW_FLAGS_START: usize = 1;
const TICKING_EDIT_ROW_DELETE: usize = TICKING_EDIT_ROW_FLAGS_START + TICKING_EDIT_FLAG_ROWS.len();
const TICKING_EDIT_ROW_BACK: usize = TICKING_EDIT_ROW_DELETE + 1;
const TICKING_EDIT_ROW_COUNT: usize = TICKING_EDIT_ROW_BACK + 1;

struct ChatLine {
    text: String,
    time: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChatTextInputSource {
    Keyboard,
    Ime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingChatTextDedup {
    source: ChatTextInputSource,
    text: String,
}

#[derive(Clone)]
struct SavedWorldMenuEntry {
    summary: SavedWorldSummary,
    label: String,
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

struct MenuScreenContent {
    title: String,
    items: Vec<String>,
    footer: Option<String>,
    selected: usize,
    enabled: Vec<bool>,
    /// Per row: `Some(fill01)` draws a slider instead of a button.
    sliders: Vec<Option<f32>>,
    show_panorama: bool,
}

impl MenuScreenContent {
    /// Convenience for the screens that have no slider rows at all.
    fn no_sliders(len: usize) -> Vec<Option<f32>> {
        vec![None; len]
    }
}

enum GameState {
    MainMenu {
        selected: usize,
    },
    LoadLevelMenu {
        selected: usize,
        worlds: Vec<SavedWorldMenuEntry>,
    },
    InGame,
    GeneratingWorld {
        title: String,
        progress: f32,
        phase: String,
        stats: WorldLoadStageStats,
    },
    Paused {
        selected: usize,
    },
    Options(OptionsMenuState),
    WorldActions(WorldActionsMenuState),
    TickingAreas(TickingAreaMenuState),
    Inventory {
        selected_palette: usize,
        selected_hotbar_slot: usize,
        scroll: i32,
        dragged_block: Option<BlockId>,
    },
}

/// One node in the menu navigation stack. Carries just enough to re-enter a
/// screen; the screen's live data (world list, area snapshot, ...) is rebuilt on
/// restore so it never goes stale while sitting on the stack.
#[derive(Clone, Eq, PartialEq)]
enum MenuScreen {
    MainMenu,
    PauseMenu,
    LoadLevel,
    WorldActions(PathBuf),
    Options(OptionsScreen),
    TickingAreas(TickingAreaScreen),
}

#[derive(Clone)]
struct MenuStackEntry {
    screen: MenuScreen,
    selected: usize,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum OptionsScreen {
    Root,
    Gameplay,
    Graphics,
    Controls,
}

impl OptionsScreen {
    /// Rows that carry an actual value/sub-screen; every options screen appends a
    /// "reset to defaults" row and a "Back" row after those.
    const fn value_row_count(self) -> usize {
        match self {
            Self::Root => OPTIONS_ROOT_ITEMS.len(),
            Self::Gameplay => 2,
            Self::Graphics => 4,
            Self::Controls => InputAction::ALL.len(),
        }
    }

    const fn reset_scope(self) -> SettingsScope {
        match self {
            Self::Root => SettingsScope::All,
            Self::Gameplay => SettingsScope::Gameplay,
            Self::Graphics => SettingsScope::Graphics,
            Self::Controls => SettingsScope::Controls,
        }
    }

    const fn reset_row(self) -> usize {
        self.value_row_count()
    }

    const fn item_count(self) -> usize {
        self.value_row_count() + 2
    }
}

#[derive(Clone)]
struct OptionsMenuState {
    screen: OptionsScreen,
    selected: usize,
    awaiting_rebind: Option<InputAction>,
    status_line: Option<String>,
    /// Armed by the first activation of a reset row; the second activation performs it.
    pending_reset: Option<SettingsScope>,
}

const WORLD_ACTION_ROW_LOAD: usize = 0;
const WORLD_ACTION_ROW_RENAME: usize = 1;
const WORLD_ACTION_ROW_DELETE: usize = 2;
const WORLD_ACTION_ROW_BACK: usize = 3;
const WORLD_ACTION_ROW_COUNT: usize = 4;

/// Per-world management screen reached from the load-level list.
struct WorldActionsMenuState {
    save_root: PathBuf,
    display_name: String,
    details: String,
    is_active: bool,
    can_delete: bool,
    selected: usize,
    status_line: Option<String>,
    /// Armed by the first activation of the delete row; the second one performs it.
    pending_delete: bool,
    /// `Some` while the rename text box has focus; swallows menu navigation keys.
    rename_input: Option<String>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum TickingAreaScreen {
    List,
    Edit { id: u32 },
}

/// In-game editor for ticking areas. `areas` is a snapshot of the world's list,
/// re-read after every mutation so the rows always match what was persisted.
struct TickingAreaMenuState {
    screen: TickingAreaScreen,
    selected: usize,
    areas: Vec<TickingArea>,
    status_line: Option<String>,
    /// Armed by the first activation of the delete row; the second one performs it.
    pending_delete: Option<u32>,
}

impl TickingAreaMenuState {
    fn area(&self, id: u32) -> Option<&TickingArea> {
        self.areas.iter().find(|area| area.id == id)
    }

    fn item_count(&self) -> usize {
        match self.screen {
            // one row per area, plus "add" and "back"
            TickingAreaScreen::List => self.areas.len() + 2,
            TickingAreaScreen::Edit { .. } => TICKING_EDIT_ROW_COUNT,
        }
    }

    fn add_row(&self) -> usize {
        self.areas.len()
    }
}

fn ticking_flag_summary(flags: TickingFlags) -> String {
    let mut parts = Vec::with_capacity(TICKING_EDIT_FLAG_ROWS.len());
    for (bit, label) in TICKING_EDIT_FLAG_ROWS {
        if flags.bits() & bit != 0 {
            parts.push(label);
        }
    }
    if parts.is_empty() {
        "no flags".to_string()
    } else {
        parts.join(", ")
    }
}

pub(crate) struct App {
    state: Option<State>,
    window: Option<Arc<Window>>,

    player: Player,
    camera: Camera,
    controller: CameraController,
    settings: AppSettings,
    selected_slot: usize,
    hotbar: [Option<BlockId>; HOTBAR_SLOT_COUNT],
    simulation_distance: SimulationDistance,
    view_bobbing: ViewBobbing,
    /// Render viewpoint. Runtime-only, like Minecraft's F5.
    camera_mode: CameraMode,
    game_state: GameState,
    /// Screens below `game_state`, innermost last. Empty means the current
    /// screen is a menu root (main menu, pause menu, ticking-area list).
    nav_stack: Vec<MenuStackEntry>,
    last_space_tap: Option<Instant>,
    last_forward_tap: Option<Instant>,
    base_fov_y_radians: f32,
    cursor_pos: (f32, f32),
    menu_pressed_index: Option<usize>,
    /// Row of the slider currently being dragged with the left mouse button.
    menu_slider_drag: Option<usize>,
    mouse_wheel_accum_y: f32,
    show_debug_overlay: bool,
    chat_lines: VecDeque<ChatLine>,
    chat_open: bool,
    chat_input: String,
    chat_input_cursor: usize,
    chat_sent_history: VecDeque<String>,
    chat_history_cursor: Option<usize>,
    suppress_next_chat_commit: bool,
    pending_chat_text_dedup: Option<PendingChatTextDedup>,

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
        let settings = AppSettings::load().unwrap_or_else(|err| {
            crate::console_error!("failed to load settings: {err}");
            AppSettings::default()
        });
        let mut camera = Camera::default();
        // The setting is the resting FOV the sprint/flight easing multiplies from.
        let base_fov_y_radians = settings.gameplay.base_fov_radians();
        camera.fov_y_radians = base_fov_y_radians;
        let player = Player::from_eye_position(camera.position);
        Self {
            window: None,
            state: None,
            player,
            camera,
            controller: CameraController::new(),
            settings,
            selected_slot: 0,
            hotbar: [None; HOTBAR_SLOT_COUNT],
            simulation_distance: SimulationDistance::default(),
            view_bobbing: ViewBobbing::default(),
            camera_mode: CameraMode::default(),
            game_state: GameState::MainMenu { selected: 0 },
            nav_stack: Vec::new(),
            last_space_tap: None,
            last_forward_tap: None,
            base_fov_y_radians,
            cursor_pos: (0.0, 0.0),
            menu_pressed_index: None,
            menu_slider_drag: None,
            mouse_wheel_accum_y: 0.0,
            show_debug_overlay: false,
            chat_lines: VecDeque::with_capacity(CHAT_MAX_LINES),
            chat_open: false,
            chat_input: String::new(),
            chat_input_cursor: 0,
            chat_sent_history: VecDeque::with_capacity(CHAT_SENT_HISTORY_MAX),
            chat_history_cursor: None,
            suppress_next_chat_commit: false,
            pending_chat_text_dedup: None,
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
        Duration::from_secs_f32(DOUBLE_SPACE_WINDOW * DOUBLE_TAP_WINDOW_MULTIPLIER)
    }

    fn double_forward_window() -> Duration {
        Duration::from_secs_f32(DOUBLE_FORWARD_TAP_WINDOW * DOUBLE_TAP_WINDOW_MULTIPLIER)
    }

    fn selected_block(&self) -> Option<BlockId> {
        self.hotbar[self.selected_slot]
    }

    fn clear_menu_press(&mut self) {
        self.menu_slider_drag = None;
        self.menu_pressed_index = None;
    }

    fn set_selected_slot(&mut self, selected_slot: usize) -> bool {
        if selected_slot >= HOTBAR_SLOT_COUNT || self.selected_slot == selected_slot {
            return false;
        }
        self.selected_slot = selected_slot;
        true
    }

    fn cycle_selected_slot(&mut self, steps: i32) {
        let count = HOTBAR_SLOT_COUNT;
        if count == 0 || steps == 0 {
            return;
        }
        let next = (self.selected_slot as i32 + steps).rem_euclid(count as i32) as usize;
        if self.set_selected_slot(next) {
            let held = self
                .selected_block()
                .map(BlockId::display_name)
                .unwrap_or("Empty");
            crate::console_info!(
                "Selected slot: {}/{} ({held})",
                self.selected_slot + 1,
                HOTBAR_SLOT_COUNT
            );
            if let Some(selected) = self.selected_block()
                && let Some(state) = self.state.as_mut()
            {
                state.rebuild_preview_mesh(selected);
            }
        }
    }

    /// Sets the base FOV from a 0..1 slider position, snapped to the step size.
    fn set_base_fov_from_fill01(&mut self, fill01: f32) -> bool {
        let next = base_fov_from_fill01(fill01);
        if next == self.settings.gameplay.base_fov_deg {
            return false;
        }
        self.settings.gameplay.base_fov_deg = next;
        let status = format!("Field of view: {next}\u{b0}");
        self.apply_settings_to_state();
        self.set_options_status(None, status);
        true
    }

    /// The one place that decides which menu rows render as sliders.
    fn menu_row_is_slider(&self, index: usize) -> bool {
        matches!(&self.game_state, GameState::Options(options)
            if matches!(options.screen, OptionsScreen::Gameplay)
                && index == OPTIONS_GAMEPLAY_ROW_FOV)
    }

    /// Slider fill for the row under the cursor, if that row is a slider.
    fn menu_slider_fill_at_cursor(&self, index: usize) -> Option<f32> {
        if !self.menu_row_is_slider(index) {
            return None;
        }
        let window = self.window.as_ref()?;
        let size = window.inner_size();
        menu_slider_fill_at_pos_px(
            size.width,
            size.height,
            self.menu_item_count(),
            index,
            self.cursor_pos.0,
            self.settings.graphics.gui_scale.factor(),
        )
    }

    /// Applies a slider drag on `index`; returns true when the row is a slider.
    fn drag_menu_slider(&mut self, index: usize) -> bool {
        let Some(fill01) = self.menu_slider_fill_at_cursor(index) else {
            return false;
        };
        self.set_base_fov_from_fill01(fill01);
        true
    }

    fn cycle_camera_mode(&mut self) {
        self.camera_mode = self.camera_mode.next();
        if let Some(state) = self.state.as_mut() {
            state.set_camera_mode(self.camera_mode);
        }
        crate::console_info!("Camera: {}", self.camera_mode.label());
    }

    /// Pose handed to the renderer for the third-person player model. `None` in
    /// first person, where the model would only clip through the near plane.
    ///
    /// Takes its inputs explicitly rather than `&self`, so it can be called
    /// while the render `State` is mutably borrowed.
    fn build_player_model_view(
        camera_mode: CameraMode,
        in_game: bool,
        camera: &Camera,
        player: &Player,
        view_bobbing: ViewBobbing,
        sneak_held: bool,
        skin_layer: u32,
    ) -> Option<PlayerModelView> {
        if camera_mode.is_first_person() || !in_game {
            return None;
        }
        let (stride_phase, stride_intensity) = view_bobbing.stride();
        // The body follows the look direction; a separate turn-in-place
        // animation is out of scope, so body and head share the yaw.
        Some(PlayerModelView {
            feet: player.feet_position,
            body_yaw: camera.yaw,
            look_yaw: camera.yaw,
            look_pitch: camera.pitch,
            stride_phase,
            stride_intensity,
            sneaking: sneak_held && !matches!(player.mode, MoveMode::Flight),
            // Filled in by the renderer, which owns the world light data.
            light_packed: 0,
            skin_layer,
        })
    }

    /// Back to first person whenever a world session ends, so a fresh world
    /// never starts in an orbit view left over from the previous one.
    fn reset_camera_mode(&mut self) {
        self.camera_mode = CameraMode::default();
        if let Some(state) = self.state.as_mut() {
            state.set_camera_mode(self.camera_mode);
        }
    }

    /// The camera the world is rendered from. In third person the eye camera is
    /// orbited away from the player and pulled back in on terrain contact; the
    /// eye camera itself is untouched so targeting stays first-person.
    fn compose_render_camera(mode: CameraMode, state: &State, eye: Camera) -> Camera {
        if mode.is_first_person() {
            return eye;
        }
        let distance = state.third_person_camera_distance(
            eye.position,
            eye.orbit_direction(mode),
            THIRD_PERSON_DISTANCE_BLOCKS,
            THIRD_PERSON_PROBE_RADIUS,
        );
        eye.orbited(mode, distance)
    }

    fn take_screenshot(&mut self) {
        // The capture happens on the next rendered frame; the encoder thread
        // reports the final path (or the failure) to the console.
        let armed = self
            .state
            .as_mut()
            .is_some_and(|state| state.request_screenshot());
        if !armed {
            crate::console_error!(
                "Screenshot unavailable: this GPU/backend cannot read back the window surface."
            );
        }
    }

    fn toggle_debug_overlay(&mut self) {
        self.show_debug_overlay = !self.show_debug_overlay;
        crate::console_info!(
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

    /// The screen `game_state` currently shows, if it is part of the menu stack.
    fn current_menu_screen(&self) -> Option<MenuScreen> {
        match &self.game_state {
            GameState::MainMenu { .. } => Some(MenuScreen::MainMenu),
            GameState::Paused { .. } => Some(MenuScreen::PauseMenu),
            GameState::LoadLevelMenu { .. } => Some(MenuScreen::LoadLevel),
            GameState::Options(options) => Some(MenuScreen::Options(options.screen)),
            GameState::WorldActions(menu) => Some(MenuScreen::WorldActions(menu.save_root.clone())),
            GameState::TickingAreas(menu) => Some(MenuScreen::TickingAreas(menu.screen)),
            _ => None,
        }
    }

    /// The bottom of the navigation stack decides the menu's context (world
    /// loaded or not), which drives the panorama background and pause handling.
    fn menu_root_screen(&self) -> Option<MenuScreen> {
        let current = self.current_menu_screen()?;
        Some(
            self.nav_stack
                .first()
                .map_or(current, |entry| entry.screen.clone()),
        )
    }

    fn is_main_menu(&self) -> bool {
        matches!(self.menu_root_screen(), Some(MenuScreen::MainMenu))
    }

    fn is_paused(&self) -> bool {
        matches!(self.menu_root_screen(), Some(MenuScreen::PauseMenu))
    }

    fn is_options_open(&self) -> bool {
        matches!(self.game_state, GameState::Options(_))
    }

    fn is_ticking_areas_open(&self) -> bool {
        matches!(self.game_state, GameState::TickingAreas(_))
    }

    fn is_menu_open(&self) -> bool {
        self.current_menu_screen().is_some()
    }

    fn is_in_game(&self) -> bool {
        matches!(self.game_state, GameState::InGame)
    }

    fn is_inventory_open(&self) -> bool {
        matches!(self.game_state, GameState::Inventory { .. })
    }

    fn chat_prev_boundary(text: &str, cursor: usize) -> usize {
        let cursor = cursor.min(text.len());
        if cursor == 0 {
            return 0;
        }
        text[..cursor]
            .char_indices()
            .next_back()
            .map(|(idx, _)| idx)
            .unwrap_or(0)
    }

    fn chat_next_boundary(text: &str, cursor: usize) -> usize {
        let cursor = cursor.min(text.len());
        if cursor >= text.len() {
            return text.len();
        }
        let mut chars = text[cursor..].chars();
        if let Some(ch) = chars.next() {
            cursor + ch.len_utf8()
        } else {
            text.len()
        }
    }

    fn open_chat(&mut self) {
        if self.chat_open || !self.is_in_game() {
            return;
        }
        self.chat_open = true;
        self.chat_input.clear();
        self.chat_input_cursor = 0;
        self.chat_history_cursor = None;
        self.suppress_next_chat_commit = true;
        self.pending_chat_text_dedup = None;
        self.clear_action_state();
        if let Some(window) = self.window.as_ref() {
            self.controller.set_mouse_capture(window, false);
            window.set_ime_allowed(true);
        }
    }

    fn suppressed_chat_commit_char(&self) -> Option<char> {
        match self.settings.controls.key_for(InputAction::OpenChat) {
            KeyCode::KeyA => Some('a'),
            KeyCode::KeyB => Some('b'),
            KeyCode::KeyC => Some('c'),
            KeyCode::KeyD => Some('d'),
            KeyCode::KeyE => Some('e'),
            KeyCode::KeyF => Some('f'),
            KeyCode::KeyG => Some('g'),
            KeyCode::KeyH => Some('h'),
            KeyCode::KeyI => Some('i'),
            KeyCode::KeyJ => Some('j'),
            KeyCode::KeyK => Some('k'),
            KeyCode::KeyL => Some('l'),
            KeyCode::KeyM => Some('m'),
            KeyCode::KeyN => Some('n'),
            KeyCode::KeyO => Some('o'),
            KeyCode::KeyP => Some('p'),
            KeyCode::KeyQ => Some('q'),
            KeyCode::KeyR => Some('r'),
            KeyCode::KeyS => Some('s'),
            KeyCode::KeyT => Some('t'),
            KeyCode::KeyU => Some('u'),
            KeyCode::KeyV => Some('v'),
            KeyCode::KeyW => Some('w'),
            KeyCode::KeyX => Some('x'),
            KeyCode::KeyY => Some('y'),
            KeyCode::KeyZ => Some('z'),
            KeyCode::Digit0 => Some('0'),
            KeyCode::Digit1 => Some('1'),
            KeyCode::Digit2 => Some('2'),
            KeyCode::Digit3 => Some('3'),
            KeyCode::Digit4 => Some('4'),
            KeyCode::Digit5 => Some('5'),
            KeyCode::Digit6 => Some('6'),
            KeyCode::Digit7 => Some('7'),
            KeyCode::Digit8 => Some('8'),
            KeyCode::Digit9 => Some('9'),
            _ => None,
        }
    }

    fn reset_chat_state(&mut self) {
        self.chat_open = false;
        self.chat_input.clear();
        self.chat_input_cursor = 0;
        self.chat_history_cursor = None;
        self.suppress_next_chat_commit = false;
        self.pending_chat_text_dedup = None;
        if let Some(window) = self.window.as_ref() {
            window.set_ime_allowed(false);
        }
    }

    fn close_chat(&mut self) {
        if !self.chat_open {
            return;
        }
        self.reset_chat_state();
        self.clear_action_state();
        if self.is_in_game()
            && let Some(window) = self.window.as_ref()
        {
            self.controller.set_mouse_capture(window, true);
        }
    }

    fn push_chat_line(&mut self, text: impl Into<String>) {
        if self.chat_lines.len() >= CHAT_MAX_LINES {
            self.chat_lines.pop_front();
        }
        self.chat_lines.push_back(ChatLine {
            text: text.into(),
            time: Instant::now(),
        });
    }

    fn push_chat_history(&mut self, entry: String) {
        if entry.is_empty() {
            return;
        }
        if self.chat_sent_history.len() >= CHAT_SENT_HISTORY_MAX {
            self.chat_sent_history.pop_front();
        }
        self.chat_sent_history.push_back(entry);
    }

    fn chat_insert_char(&mut self, ch: char) {
        if !self.chat_open || ch.is_control() {
            return;
        }
        self.chat_input.insert(self.chat_input_cursor, ch);
        self.chat_input_cursor += ch.len_utf8();
        self.chat_history_cursor = None;
    }

    fn should_skip_chat_text_duplicate(&mut self, text: &str, source: ChatTextInputSource) -> bool {
        matches!(
            self.pending_chat_text_dedup.take(),
            Some(previous) if previous.source != source && previous.text == text
        )
    }

    fn try_consume_suppressed_chat_text(&mut self, text: &str) -> bool {
        if !self.suppress_next_chat_commit {
            return false;
        }
        self.suppress_next_chat_commit = false;
        self.suppressed_chat_commit_char()
            .is_some_and(|expected| text.eq_ignore_ascii_case(&expected.to_string()))
    }

    fn chat_insert_text(&mut self, text: &str, source: ChatTextInputSource) {
        if !self.chat_open || text.is_empty() {
            return;
        }
        if self.try_consume_suppressed_chat_text(text)
            || self.should_skip_chat_text_duplicate(text, source)
        {
            return;
        }

        let mut inserted = false;
        for ch in text.chars() {
            if ch.is_control() {
                continue;
            }
            self.chat_insert_char(ch);
            inserted = true;
        }

        if inserted {
            self.pending_chat_text_dedup = Some(PendingChatTextDedup {
                source,
                text: text.to_owned(),
            });
        }
    }

    fn chat_move_cursor_left(&mut self) {
        self.chat_input_cursor = Self::chat_prev_boundary(&self.chat_input, self.chat_input_cursor);
    }

    fn chat_move_cursor_right(&mut self) {
        self.chat_input_cursor = Self::chat_next_boundary(&self.chat_input, self.chat_input_cursor);
    }

    fn chat_backspace(&mut self) {
        if !self.chat_open || self.chat_input_cursor == 0 {
            return;
        }
        let prev = Self::chat_prev_boundary(&self.chat_input, self.chat_input_cursor);
        self.chat_input.drain(prev..self.chat_input_cursor);
        self.chat_input_cursor = prev;
        self.chat_history_cursor = None;
    }

    fn chat_delete(&mut self) {
        if !self.chat_open || self.chat_input_cursor >= self.chat_input.len() {
            return;
        }
        let next = Self::chat_next_boundary(&self.chat_input, self.chat_input_cursor);
        self.chat_input.drain(self.chat_input_cursor..next);
        self.chat_history_cursor = None;
    }

    fn chat_history_up(&mut self) {
        if self.chat_sent_history.is_empty() {
            return;
        }
        let next = match self.chat_history_cursor {
            None => self.chat_sent_history.len().saturating_sub(1),
            Some(current) => current.saturating_sub(1),
        };
        self.chat_history_cursor = Some(next);
        if let Some(entry) = self.chat_sent_history.get(next) {
            self.chat_input = entry.clone();
            self.chat_input_cursor = self.chat_input.len();
        }
    }

    fn chat_history_down(&mut self) {
        let Some(current) = self.chat_history_cursor else {
            return;
        };
        let next = current + 1;
        if next < self.chat_sent_history.len() {
            self.chat_history_cursor = Some(next);
            if let Some(entry) = self.chat_sent_history.get(next) {
                self.chat_input = entry.clone();
                self.chat_input_cursor = self.chat_input.len();
            }
            return;
        }

        self.chat_history_cursor = None;
        self.chat_input.clear();
        self.chat_input_cursor = 0;
    }

    fn execute_chat_command(&mut self, line: &str) -> Result<String, String> {
        let command = commands::parse_command(line)?;
        self.execute_parsed_command(command)
    }

    fn execute_parsed_command(&mut self, command: Command) -> Result<String, String> {
        let Some(mut state) = self.state.take() else {
            return Err("world is not ready".to_string());
        };
        let result = commands::execute_command(self, &mut state, command);
        self.state = Some(state);
        result
    }

    fn submit_chat_input(&mut self) {
        if !self.chat_open {
            return;
        }
        let submitted = self.chat_input.trim().to_string();
        self.close_chat();
        if submitted.is_empty() {
            return;
        }
        self.push_chat_history(submitted.clone());

        if submitted.starts_with('/') {
            match self.execute_chat_command(&submitted) {
                Ok(feedback) if !feedback.trim().is_empty() => self.push_chat_line(feedback),
                Ok(_) => {}
                Err(err) => self.push_chat_line(format!("Error: {}", err)),
            }
            return;
        }

        self.push_chat_line(format!("<{}> {}", CHAT_PLAYER_NAME, submitted));
    }

    fn clear_action_state(&mut self) {
        self.break_held = false;
        self.place_held = false;
        self.break_cooldown_seconds = 0.0;
        self.place_cooldown_seconds = 0.0;
        self.last_forward_tap = None;
        self.controller.clear_inputs();
    }

    /// Persists settings; returns a user-facing error line when the write failed
    /// so menu screens can surface it instead of only logging to the console.
    fn persist_settings(&self) -> Option<String> {
        match self.settings.save() {
            Ok(()) => None,
            Err(err) => {
                crate::console_error!("failed to save settings: {err}");
                Some(format!("Could not save settings: {err}"))
            }
        }
    }

    fn apply_settings_to_state(&mut self) {
        // Resting FOV the sprint/flight easing multiplies from; kept here so the
        // options rows and the "reset to defaults" flow both pick it up.
        self.base_fov_y_radians = self.settings.gameplay.base_fov_radians();
        if let Some(state) = self.state.as_mut() {
            state.set_render_distance(self.settings.graphics.render_distance, &self.camera);
            state.set_vsync_enabled(self.settings.graphics.vsync);
            state.set_backface_culling_enabled(self.settings.graphics.backface_culling);
            state.set_gui_scale(self.settings.graphics.gui_scale);
        }
    }

    fn reset_view_bobbing(&mut self) {
        self.view_bobbing.reset();
        self.camera.roll = 0.0;
    }

    fn sync_camera_to_player_eye(&mut self, reset_view_bobbing: bool) {
        self.camera.position = self.player.eye_position();
        self.camera.roll = 0.0;
        if reset_view_bobbing {
            self.reset_view_bobbing();
        }
    }

    fn update_view_bobbing_state(
        camera: &mut Camera,
        view_bobbing: &mut ViewBobbing,
        player: &Player,
        bobbing_enabled: bool,
        dt_seconds: f32,
        allow_bobbing: bool,
    ) {
        let raw_eye = player.eye_position();
        let horizontal_distance = Vec3::new(
            raw_eye.x - camera.position.x,
            0.0,
            raw_eye.z - camera.position.z,
        )
        .length();
        camera.position = raw_eye;
        camera.roll = 0.0;
        let horizontal_speed =
            Vec3::new(player.walk_velocity.x, 0.0, player.walk_velocity.z).length();
        let bobbing_active = allow_bobbing
            && bobbing_enabled
            && matches!(player.mode, MoveMode::Walk)
            && player.on_ground;
        view_bobbing.update(
            dt_seconds,
            horizontal_distance,
            horizontal_speed,
            bobbing_active,
        );
    }

    fn compose_view_camera(
        base_camera: Camera,
        bobbing_enabled: bool,
        view_bobbing: ViewBobbing,
    ) -> Camera {
        if bobbing_enabled {
            base_camera.with_view_bobbing(view_bobbing.sample())
        } else {
            base_camera
        }
    }

    fn current_view_camera(&self) -> Camera {
        Self::compose_view_camera(
            self.camera,
            self.settings.gameplay.view_bobbing,
            self.view_bobbing,
        )
    }

    fn enter_main_menu(&mut self) {
        self.reset_camera_mode();
        self.reset_chat_state();
        self.clear_menu_press();
        self.nav_stack.clear();
        self.game_state = GameState::MainMenu { selected: 0 };
        self.clear_action_state();
        if let Some(window) = self.window.as_ref() {
            self.controller.set_mouse_capture(window, false);
        }
    }

    fn enter_pause(&mut self) {
        self.reset_chat_state();
        self.clear_menu_press();
        self.nav_stack.clear();
        self.game_state = GameState::Paused { selected: 0 };
        self.clear_action_state();
        if let Some(state) = self.state.as_mut() {
            state.set_world_session_paused(true, "pause_menu_opened");
        }
        if let Some(window) = self.window.as_ref() {
            self.controller.set_mouse_capture(window, false);
        }
    }

    fn leave_pause(&mut self) {
        self.clear_menu_press();
        self.nav_stack.clear();
        self.game_state = GameState::InGame;
        self.clear_action_state();
        if let Some(state) = self.state.as_mut() {
            state.set_world_session_paused(false, "pause_menu_closed");
        }
        if let Some(window) = self.window.as_ref() {
            self.controller.set_mouse_capture(window, true);
        }
    }

    fn enter_inventory(&mut self) {
        self.reset_chat_state();
        self.clear_menu_press();
        self.nav_stack.clear();
        let selected_palette = self
            .selected_block()
            .and_then(|block| {
                CLASSIC_PALETTE
                    .iter()
                    .position(|candidate| *candidate == block)
            })
            .unwrap_or(0);
        self.game_state = GameState::Inventory {
            selected_palette,
            selected_hotbar_slot: self.selected_slot.min(HOTBAR_SLOT_COUNT.saturating_sub(1)),
            scroll: 0,
            dragged_block: None,
        };
        self.clear_action_state();
        if let Some(window) = self.window.as_ref() {
            self.controller.set_mouse_capture(window, false);
        }
    }

    fn leave_inventory(&mut self) {
        self.clear_menu_press();
        self.nav_stack.clear();
        self.game_state = GameState::InGame;
        self.clear_action_state();
        if let Some(window) = self.window.as_ref() {
            self.controller.set_mouse_capture(window, true);
        }
    }

    fn saved_world_menu_entries(&self) -> Vec<SavedWorldMenuEntry> {
        self.state
            .as_ref()
            .map(|state| {
                state
                    .list_saved_worlds()
                    .into_iter()
                    .map(|summary| {
                        let active = if summary.is_active { " [active]" } else { "" };
                        SavedWorldMenuEntry {
                            label: format!(
                                "{}{} | {}",
                                summary.display_name, active, summary.details
                            ),
                            summary,
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }

    /// Rebuilds the per-world screen from the on-disk listing so a stale entry
    /// (renamed or deleted elsewhere) cannot offer actions on a missing world.
    fn build_world_actions_state(
        &self,
        save_root: PathBuf,
        selected: usize,
    ) -> WorldActionsMenuState {
        let summary = self.state.as_ref().and_then(|state| {
            state
                .list_saved_worlds()
                .into_iter()
                .find(|world| world.save_root == save_root)
        });
        match summary {
            Some(summary) => {
                let is_active = summary.is_active;
                WorldActionsMenuState {
                    can_delete: !is_active && !State::is_default_save_root(&summary.save_root),
                    display_name: summary.display_name,
                    details: summary.details,
                    is_active,
                    save_root: summary.save_root,
                    selected,
                    status_line: None,
                    pending_delete: false,
                    rename_input: None,
                }
            }
            None => WorldActionsMenuState {
                display_name: save_root
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| save_root.display().to_string()),
                details: String::new(),
                is_active: false,
                can_delete: false,
                save_root,
                selected,
                status_line: Some("This world is no longer available.".to_string()),
                pending_delete: false,
                rename_input: None,
            },
        }
    }

    /// Descends one level: remembers the current screen (and its highlighted
    /// row) on the stack, then shows `screen` with its first row selected.
    fn push_menu_screen(&mut self, screen: MenuScreen) {
        if let Some(current) = self.current_menu_screen() {
            let selected = self.selected_menu_index().unwrap_or(0);
            self.nav_stack.push(MenuStackEntry {
                screen: current,
                selected,
            });
        }
        self.show_menu_screen(MenuStackEntry {
            screen,
            selected: 0,
        });
    }

    /// Goes back exactly one level. Returns false only at a menu root that has
    /// nowhere to go (the main menu).
    fn pop_menu_screen(&mut self) -> bool {
        if let Some(entry) = self.nav_stack.pop() {
            self.show_menu_screen(entry);
            return true;
        }
        match &self.game_state {
            GameState::Paused { .. } => {
                self.leave_pause();
                true
            }
            GameState::TickingAreas(_) => {
                self.leave_ticking_area_menu();
                true
            }
            _ => false,
        }
    }

    fn show_menu_screen(&mut self, entry: MenuStackEntry) {
        self.reset_chat_state();
        self.clear_menu_press();
        self.game_state = match entry.screen {
            MenuScreen::MainMenu => GameState::MainMenu {
                selected: entry.selected,
            },
            MenuScreen::PauseMenu => GameState::Paused {
                selected: entry.selected,
            },
            MenuScreen::LoadLevel => {
                let worlds = self.saved_world_menu_entries();
                GameState::LoadLevelMenu {
                    selected: entry.selected,
                    worlds,
                }
            }
            MenuScreen::Options(screen) => GameState::Options(OptionsMenuState {
                screen,
                selected: entry.selected,
                awaiting_rebind: None,
                status_line: None,
                pending_reset: None,
            }),
            MenuScreen::WorldActions(save_root) => {
                GameState::WorldActions(self.build_world_actions_state(save_root, entry.selected))
            }
            MenuScreen::TickingAreas(screen) => {
                let areas = self
                    .state
                    .as_ref()
                    .map(|state| state.ticking_areas_snapshot())
                    .unwrap_or_default();
                GameState::TickingAreas(TickingAreaMenuState {
                    screen,
                    selected: entry.selected,
                    areas,
                    status_line: None,
                    pending_delete: None,
                })
            }
        };
        let max_index = self.menu_item_count().saturating_sub(1);
        self.set_selected_menu_index(entry.selected.min(max_index));
        self.clear_action_state();
        if let Some(window) = self.window.as_ref() {
            self.controller.set_mouse_capture(window, false);
        }
    }

    fn menu_item_count(&self) -> usize {
        match &self.game_state {
            GameState::MainMenu { .. } => MAIN_MENU_ITEMS.len(),
            GameState::Paused { .. } => PAUSE_MENU_ITEMS.len(),
            GameState::LoadLevelMenu { worlds, .. } => worlds.len().max(1) + 1,
            GameState::Options(options) => options.screen.item_count(),
            GameState::WorldActions(_) => WORLD_ACTION_ROW_COUNT,
            GameState::TickingAreas(menu) => menu.item_count(),
            _ => 0,
        }
    }

    fn selected_menu_index(&self) -> Option<usize> {
        match &self.game_state {
            GameState::MainMenu { selected } => Some(*selected),
            GameState::Paused { selected } => Some(*selected),
            GameState::LoadLevelMenu { selected, .. } => Some(*selected),
            GameState::Options(options) => Some(options.selected),
            GameState::WorldActions(menu) => Some(menu.selected),
            GameState::TickingAreas(menu) => Some(menu.selected),
            _ => None,
        }
    }

    fn set_selected_menu_index(&mut self, selected: usize) {
        match &mut self.game_state {
            GameState::MainMenu { selected: current } => *current = selected,
            GameState::Paused { selected: current } => *current = selected,
            GameState::LoadLevelMenu {
                selected: current, ..
            } => *current = selected,
            GameState::Options(options) => {
                if options.selected != selected {
                    options.pending_reset = None;
                }
                options.selected = selected;
            }
            GameState::WorldActions(menu) => {
                if menu.selected != selected {
                    menu.pending_delete = false;
                }
                menu.selected = selected;
            }
            GameState::TickingAreas(menu) => {
                if menu.selected != selected {
                    menu.pending_delete = None;
                }
                menu.selected = selected;
            }
            _ => {}
        }
    }

    fn clear_pending_reset(&mut self) {
        match &mut self.game_state {
            GameState::Options(options) => options.pending_reset = None,
            GameState::WorldActions(menu) => menu.pending_delete = false,
            GameState::TickingAreas(menu) => menu.pending_delete = None,
            _ => {}
        }
    }

    fn move_menu_selection(&mut self, delta: i32) {
        let count = self.menu_item_count();
        if count == 0 {
            return;
        }
        self.clear_menu_press();
        if let Some(current) = self.selected_menu_index() {
            let next = (current as i32 + delta).rem_euclid(count as i32) as usize;
            self.set_selected_menu_index(next);
        }
        self.clear_pending_reset();
    }

    fn enter_world_loading_screen(&mut self, title: &str, phase: &str) {
        self.reset_camera_mode();
        self.reset_chat_state();
        self.clear_menu_press();
        self.nav_stack.clear();
        self.game_state = GameState::GeneratingWorld {
            title: title.to_string(),
            progress: 0.0,
            phase: phase.to_string(),
            stats: WorldLoadStageStats::default(),
        };
        self.clear_action_state();
        if let Some(window) = self.window.as_ref() {
            self.controller.set_mouse_capture(window, false);
        }
    }

    fn start_new_level_generation(&mut self, request: WorldGenRequest) -> bool {
        let started = if let Some(state) = self.state.as_mut() {
            state.begin_new_world(
                request.seed,
                request.params.preset,
                request.params.classic_size,
                request.params.environment,
            );
            state.is_world_startup_loading()
        } else {
            false
        };

        if started {
            self.enter_world_loading_screen("Generating level", "Clearing world state");
        }

        started
    }

    fn start_new_default_level_generation(&mut self) {
        let request =
            self.make_worldgen_request(ClassicLevelSize::Normal, WorldEnvironment::Temperate);
        if !self.start_new_level_generation(request) {
            crate::console_info!("new level generation is already running");
        }
    }

    fn menu_item_at_cursor(&self) -> Option<usize> {
        let window = self.window.as_ref()?;
        let size = window.inner_size();
        pause_menu_item_at_pos_px(
            size.width,
            size.height,
            self.menu_item_count(),
            self.cursor_pos.0,
            self.cursor_pos.1,
            self.settings.graphics.gui_scale.factor(),
        )
    }

    fn apply_selected_option_step(&mut self, delta: i32) -> bool {
        let mut changed = false;
        if let GameState::Options(options) = &mut self.game_state {
            if options.awaiting_rebind.is_some() {
                return false;
            }
            match options.screen {
                OptionsScreen::Gameplay if options.selected == 0 => {
                    self.settings.gameplay.view_bobbing = !self.settings.gameplay.view_bobbing;
                    options.status_line = Some(format!(
                        "View bobbing {}",
                        if self.settings.gameplay.view_bobbing {
                            "enabled"
                        } else {
                            "disabled"
                        }
                    ));
                    changed = true;
                }
                OptionsScreen::Gameplay if options.selected == 1 => {
                    let current = self.settings.gameplay.base_fov_deg;
                    let step = BASE_FOV_STEP_DEG as i32 * delta;
                    let next = (current as i32 + step)
                        .clamp(BASE_FOV_MIN_DEG as i32, BASE_FOV_MAX_DEG as i32)
                        as u32;
                    if next != current {
                        self.settings.gameplay.base_fov_deg = next;
                        options.status_line = Some(format!("Field of view: {next}°"));
                        changed = true;
                    }
                }
                OptionsScreen::Graphics if options.selected == 0 => {
                    let all = RenderDistance::ALL;
                    let current = self.settings.graphics.render_distance;
                    if let Some(index) = all.iter().position(|candidate| *candidate == current) {
                        let next = (index as i32 + delta).rem_euclid(all.len() as i32) as usize;
                        self.settings.graphics.render_distance = all[next];
                        options.status_line = Some(format!(
                            "Render distance: {} ({} chunks)",
                            self.settings.graphics.render_distance.label(),
                            self.settings
                                .graphics
                                .render_distance
                                .view_distance_chunks()
                        ));
                        changed = true;
                    }
                }
                OptionsScreen::Graphics if options.selected == 1 => {
                    self.settings.graphics.backface_culling =
                        !self.settings.graphics.backface_culling;
                    options.status_line = Some(format!(
                        "Backface culling {}",
                        if self.settings.graphics.backface_culling {
                            "enabled"
                        } else {
                            "disabled"
                        }
                    ));
                    changed = true;
                }
                OptionsScreen::Graphics if options.selected == 3 => {
                    let all = GuiScale::ALL;
                    let current = self.settings.graphics.gui_scale;
                    if let Some(index) = all.iter().position(|candidate| *candidate == current) {
                        let next = (index as i32 + delta).rem_euclid(all.len() as i32) as usize;
                        self.settings.graphics.gui_scale = all[next];
                        options.status_line = Some(format!(
                            "GUI scale: {} ({:.0}%)",
                            all[next].label(),
                            all[next].factor() * 100.0
                        ));
                        changed = true;
                    }
                }
                OptionsScreen::Graphics if options.selected == 2 => {
                    self.settings.graphics.vsync = !self.settings.graphics.vsync;
                    options.status_line = Some(format!(
                        "VSync {}",
                        if self.settings.graphics.vsync {
                            "enabled"
                        } else {
                            "disabled"
                        }
                    ));
                    changed = true;
                }
                _ => {}
            }
        }
        if changed {
            if !self.settings.gameplay.view_bobbing {
                self.reset_view_bobbing();
            }

            if matches!(self.game_state, GameState::Options(_)) {
                let save_error = self.persist_settings();
                self.apply_settings_to_state();
                if let Some(error) = save_error {
                    self.set_options_status(None, error);
                }
            }
        }
        changed
    }

    fn activate_main_menu_selected(&mut self, event_loop: &ActiveEventLoop) {
        let selected = match self.game_state {
            GameState::MainMenu { selected } => selected,
            _ => return,
        };
        match selected {
            0 => self.start_new_default_level_generation(),
            1 => self.push_menu_screen(MenuScreen::LoadLevel),
            2 => self.push_menu_screen(MenuScreen::Options(OptionsScreen::Root)),
            3 => event_loop.exit(),
            _ => {}
        }
    }

    fn activate_pause_menu_selected(&mut self, _event_loop: &ActiveEventLoop) {
        let selected = match &self.game_state {
            GameState::Paused { selected } => *selected,
            _ => return,
        };

        match selected {
            0 => self.leave_pause(),
            1 => self.start_new_default_level_generation(),
            2 => self.push_menu_screen(MenuScreen::LoadLevel),
            3 => {
                if let Some(state) = self.state.as_mut() {
                    state.save_world();
                    crate::console_info!("Saved.");
                }
            }
            4 => self.push_menu_screen(MenuScreen::Options(OptionsScreen::Root)),
            5 => {
                if let Some(state) = self.state.as_mut() {
                    state.save_world();
                    state.shutdown_world_session("exit_to_main_menu");
                }
                self.enter_main_menu();
            }
            _ => {}
        }
    }

    fn selected_saved_world_root(&self) -> Option<PathBuf> {
        match &self.game_state {
            GameState::LoadLevelMenu { selected, worlds } => worlds
                .get(*selected)
                .map(|entry| entry.summary.save_root.clone()),
            _ => None,
        }
    }

    fn activate_load_level_menu_selected(&mut self) {
        let Some(save_root) = self.selected_saved_world_root() else {
            // The "Back" row (and the empty-list placeholder) land here.
            self.pop_menu_screen();
            return;
        };
        self.push_menu_screen(MenuScreen::WorldActions(save_root));
    }

    fn activate_world_action_selected(&mut self) {
        let GameState::WorldActions(menu) = &self.game_state else {
            return;
        };
        if menu.rename_input.is_some() {
            return;
        }
        let selected = menu.selected;
        let save_root = menu.save_root.clone();

        match selected {
            WORLD_ACTION_ROW_LOAD => self.load_selected_world(save_root),
            WORLD_ACTION_ROW_RENAME => {
                if let GameState::WorldActions(menu) = &mut self.game_state {
                    menu.rename_input = Some(menu.display_name.clone());
                    menu.pending_delete = false;
                    menu.status_line =
                        Some("Type a new name. Enter confirms, Escape cancels.".to_string());
                }
            }
            WORLD_ACTION_ROW_DELETE => self.activate_world_delete(save_root),
            _ => {
                self.pop_menu_screen();
            }
        }
    }

    /// Two-step delete, same confirmation pattern as the options reset.
    fn activate_world_delete(&mut self, save_root: PathBuf) {
        let (armed, can_delete, is_active) = match &self.game_state {
            GameState::WorldActions(menu) => (menu.pending_delete, menu.can_delete, menu.is_active),
            _ => return,
        };

        if !can_delete {
            let reason = if is_active {
                "Cannot delete the world that is loaded. Load another world first."
            } else {
                "Cannot delete the main save directory."
            };
            if let GameState::WorldActions(menu) = &mut self.game_state {
                menu.pending_delete = false;
                menu.status_line = Some(reason.to_string());
            }
            return;
        }

        if !armed {
            if let GameState::WorldActions(menu) = &mut self.game_state {
                menu.pending_delete = true;
                menu.status_line =
                    Some("Delete this world permanently? Activate again to confirm.".to_string());
            }
            return;
        }

        let result = self
            .state
            .as_mut()
            .map(|state| state.delete_saved_world(&save_root));
        match result {
            Some(Ok(())) => {
                // Back to the list, which `show_menu_screen` rebuilds from disk.
                self.pop_menu_screen();
                if let GameState::LoadLevelMenu { .. } = &self.game_state {
                    crate::console_info!("Deleted world {}", save_root.display());
                }
            }
            Some(Err(err)) => {
                if let GameState::WorldActions(menu) = &mut self.game_state {
                    menu.pending_delete = false;
                    menu.status_line = Some(format!("Delete failed: {err}"));
                }
            }
            None => {}
        }
    }

    fn commit_world_rename(&mut self) {
        let (save_root, raw) = match &self.game_state {
            GameState::WorldActions(menu) => match &menu.rename_input {
                Some(raw) => (menu.save_root.clone(), raw.clone()),
                None => return,
            },
            _ => return,
        };

        let result = self
            .state
            .as_mut()
            .map(|state| state.rename_saved_world(&save_root, &raw));
        let GameState::WorldActions(menu) = &mut self.game_state else {
            return;
        };
        match result {
            Some(Ok((new_root, moved))) => {
                menu.rename_input = None;
                menu.display_name = raw.trim().to_string();
                menu.save_root = new_root;
                menu.status_line = Some(if moved {
                    format!("Renamed to '{}'.", menu.display_name)
                } else {
                    format!(
                        "Renamed to '{}' (save folder unchanged).",
                        menu.display_name
                    )
                });
            }
            Some(Err(err)) => {
                menu.status_line = Some(format!("Rename failed: {err}"));
            }
            None => {
                menu.rename_input = None;
            }
        }
    }

    /// Keyboard handling while the rename text box has focus. Returns true when
    /// the key was consumed so menu navigation never sees it.
    fn handle_world_rename_key(&mut self, code: Option<KeyCode>, text: Option<&str>) -> bool {
        let GameState::WorldActions(menu) = &mut self.game_state else {
            return false;
        };
        if menu.rename_input.is_none() {
            return false;
        }
        match code {
            Some(KeyCode::Escape) => {
                menu.rename_input = None;
                menu.status_line = Some("Rename cancelled.".to_string());
                return true;
            }
            Some(KeyCode::Enter | KeyCode::NumpadEnter) => {
                self.commit_world_rename();
                return true;
            }
            Some(KeyCode::Backspace) => {
                if let Some(input) = menu.rename_input.as_mut() {
                    input.pop();
                }
                return true;
            }
            _ => {}
        }
        if let Some(text) = text
            && let Some(input) = menu.rename_input.as_mut()
        {
            for ch in text.chars().filter(|ch| !ch.is_control()) {
                if input.chars().count() >= world_meta::WORLD_NAME_MAX_LEN {
                    break;
                }
                input.push(ch);
            }
        }
        // Swallow everything while typing, so Space/WASD do not navigate the menu.
        true
    }

    fn load_selected_world(&mut self, save_root: PathBuf) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match state.begin_load_world(save_root) {
            Ok(()) => self.enter_world_loading_screen("Loading level", "Preparing spawn area"),
            Err(err) => {
                let message = format!("Load failed: {err}");
                crate::console_error!("load level failed: {}", err);
                if let GameState::WorldActions(menu) = &mut self.game_state {
                    menu.status_line = Some(message);
                }
            }
        }
    }

    fn activate_options_selected(&mut self) {
        let GameState::Options(options) = &self.game_state else {
            return;
        };
        if options.awaiting_rebind.is_some() {
            return;
        }
        let screen = options.screen;
        let selected = options.selected;

        if selected == screen.reset_row() {
            self.activate_options_reset_row(screen);
            return;
        }
        // The "Back" row goes exactly one level up, same as Escape.
        if selected > screen.reset_row() {
            self.pop_menu_screen();
            return;
        }

        match screen {
            OptionsScreen::Root => {
                let sub = match selected {
                    0 => OptionsScreen::Gameplay,
                    1 => OptionsScreen::Graphics,
                    _ => OptionsScreen::Controls,
                };
                self.push_menu_screen(MenuScreen::Options(sub));
            }
            OptionsScreen::Gameplay | OptionsScreen::Graphics => {
                self.apply_selected_option_step(1);
            }
            OptionsScreen::Controls => {
                let action = InputAction::ALL[selected];
                if let GameState::Options(state) = &mut self.game_state {
                    state.awaiting_rebind = Some(action);
                    state.pending_reset = None;
                    state.status_line = Some(format!(
                        "Press a key for {}. Escape cancels.",
                        action.label()
                    ));
                }
            }
        }
    }

    /// Two-step reset: the first activation arms a confirmation, the second one
    /// applies `AppSettings::reset` for that screen's scope and persists it.
    fn activate_options_reset_row(&mut self, screen: OptionsScreen) {
        let scope = screen.reset_scope();
        let armed = matches!(
            &self.game_state,
            GameState::Options(options) if options.pending_reset == Some(scope)
        );

        if self.settings.is_default(scope) {
            self.set_options_status(
                None,
                format!("{} already at defaults.", capitalize(scope.label())),
            );
            return;
        }

        if !armed {
            self.set_options_status(
                Some(scope),
                format!(
                    "Reset {} to defaults? Activate again to confirm.",
                    scope.label()
                ),
            );
            return;
        }

        self.settings.reset(scope);
        if !self.settings.gameplay.view_bobbing {
            self.reset_view_bobbing();
        }
        let save_error = self.persist_settings();
        self.apply_settings_to_state();
        let status = save_error
            .unwrap_or_else(|| format!("{} reset to defaults.", capitalize(scope.label())));
        self.set_options_status(None, status);
    }

    fn apply_selected_menu_step(&mut self, delta: i32) -> bool {
        match self.game_state {
            GameState::Options(_) => self.apply_selected_option_step(delta),
            GameState::TickingAreas(_) => self.apply_ticking_area_step(delta),
            _ => false,
        }
    }

    fn player_chunk_xz(&self) -> (i32, i32) {
        let chunk = World::world_to_chunk_coord(IVec3::new(
            self.camera.position.x.floor() as i32,
            self.camera.position.y.floor() as i32,
            self.camera.position.z.floor() as i32,
        ));
        (chunk.x, chunk.z)
    }

    fn open_ticking_area_menu(&mut self) {
        self.reset_chat_state();
        self.clear_menu_press();
        self.nav_stack.clear();
        let areas = self
            .state
            .as_ref()
            .map(|state| state.ticking_areas_snapshot())
            .unwrap_or_default();
        self.game_state = GameState::TickingAreas(TickingAreaMenuState {
            screen: TickingAreaScreen::List,
            selected: 0,
            areas,
            status_line: None,
            pending_delete: None,
        });
        self.clear_action_state();
        if let Some(window) = self.window.as_ref() {
            self.controller.set_mouse_capture(window, false);
        }
    }

    fn leave_ticking_area_menu(&mut self) {
        self.clear_menu_press();
        self.nav_stack.clear();
        self.game_state = GameState::InGame;
        self.clear_action_state();
        if let Some(window) = self.window.as_ref() {
            self.controller.set_mouse_capture(window, true);
        }
    }

    /// Re-reads the persisted list and re-applies the simulation activation so
    /// edits take effect without leaving the editor.
    fn refresh_ticking_areas(&mut self, status: Option<String>) {
        let areas = if let Some(state) = self.state.as_mut() {
            state.update_simulation_activation(&self.camera);
            state.ticking_areas_snapshot()
        } else {
            Vec::new()
        };
        let mut vanished = false;
        if let GameState::TickingAreas(menu) = &mut self.game_state {
            menu.areas = areas;
            menu.pending_delete = None;
            if let Some(status) = status.clone() {
                menu.status_line = Some(status);
            }
            // An edited area can disappear (deleted from the edit screen itself).
            vanished =
                matches!(menu.screen, TickingAreaScreen::Edit { id } if menu.area(id).is_none());
            let max_index = menu.item_count().saturating_sub(1);
            menu.selected = menu.selected.min(max_index);
        }
        if vanished {
            // Leave the dead edit screen via the stack so navigation stays consistent.
            self.pop_menu_screen();
            if let Some(status) = status
                && let GameState::TickingAreas(menu) = &mut self.game_state
            {
                menu.status_line = Some(status);
            }
        }
    }

    /// The area currently being edited, taken from the menu's own snapshot
    /// (kept in sync by `refresh_ticking_areas`), plus the selected row.
    fn selected_ticking_area_edit(&self) -> Option<(TickingArea, usize)> {
        let GameState::TickingAreas(menu) = &self.game_state else {
            return None;
        };
        let TickingAreaScreen::Edit { id } = menu.screen else {
            return None;
        };
        menu.area(id).map(|area| (*area, menu.selected))
    }

    /// Left/right on the edit screen: radius steps by `delta`, flag rows toggle.
    fn apply_ticking_area_step(&mut self, delta: i32) -> bool {
        let Some((area, row)) = self.selected_ticking_area_edit() else {
            return false;
        };
        let id = area.id;

        let (radius, flags, status) = if row == TICKING_EDIT_ROW_RADIUS {
            let radius = (area.radius_chunks + delta).clamp(0, TICKING_AREA_MAX_RADIUS_CHUNKS);
            if radius == area.radius_chunks {
                return false;
            }
            (
                radius,
                area.flags,
                format!("Area #{id} radius: {radius} chunk(s)"),
            )
        } else if (TICKING_EDIT_ROW_FLAGS_START..TICKING_EDIT_ROW_DELETE).contains(&row) {
            let (bit, label) = TICKING_EDIT_FLAG_ROWS[row - TICKING_EDIT_ROW_FLAGS_START];
            let flags = TickingFlags::from_bits(area.flags.bits() ^ bit);
            let enabled = flags.bits() & bit != 0;
            (
                area.radius_chunks,
                flags,
                format!(
                    "Area #{id} {}: {}",
                    label.to_lowercase(),
                    if enabled { "on" } else { "off" }
                ),
            )
        } else {
            return false;
        };

        let applied = self
            .state
            .as_mut()
            .and_then(|state| state.update_ticking_area(id, radius, flags))
            .is_some();
        if !applied {
            self.refresh_ticking_areas(Some(format!("Area #{id} no longer exists.")));
            return false;
        }
        self.refresh_ticking_areas(Some(status));
        true
    }

    fn activate_ticking_area_selected(&mut self) {
        let Some((screen, selected, add_row, area_count)) = (match &self.game_state {
            GameState::TickingAreas(menu) => {
                Some((menu.screen, menu.selected, menu.add_row(), menu.areas.len()))
            }
            _ => None,
        }) else {
            return;
        };

        match screen {
            TickingAreaScreen::List => {
                if selected < area_count {
                    let id = match &self.game_state {
                        GameState::TickingAreas(menu) => menu.areas[selected].id,
                        _ => return,
                    };
                    self.push_menu_screen(MenuScreen::TickingAreas(TickingAreaScreen::Edit { id }));
                } else if selected == add_row {
                    let (cx, cz) = self.player_chunk_xz();
                    let created = self
                        .state
                        .as_mut()
                        .map(|state| state.create_ticking_area(cx, cz, 4, TickingFlags::default()));
                    match created {
                        Some(area) => self.refresh_ticking_areas(Some(format!(
                            "Created area #{} at chunk ({cx}, {cz}), radius {}.",
                            area.id, area.radius_chunks
                        ))),
                        None => self.refresh_ticking_areas(Some("No world loaded.".to_string())),
                    }
                } else {
                    self.pop_menu_screen();
                }
            }
            TickingAreaScreen::Edit { id } => match selected {
                TICKING_EDIT_ROW_DELETE => self.activate_ticking_area_delete(id),
                TICKING_EDIT_ROW_BACK => {
                    self.pop_menu_screen();
                }
                _ => {
                    self.apply_ticking_area_step(1);
                }
            },
        }
    }

    /// Two-step delete, mirroring the options reset confirmation.
    fn activate_ticking_area_delete(&mut self, id: u32) {
        let armed = matches!(
            &self.game_state,
            GameState::TickingAreas(menu) if menu.pending_delete == Some(id)
        );
        if !armed {
            if let GameState::TickingAreas(menu) = &mut self.game_state {
                menu.pending_delete = Some(id);
                menu.status_line = Some(format!("Delete area #{id}? Activate again to confirm."));
            }
            return;
        }

        let deleted = self
            .state
            .as_mut()
            .is_some_and(|state| state.delete_ticking_area(id));
        let status = if deleted {
            format!("Area #{id} deleted.")
        } else {
            format!("Area #{id} no longer exists.")
        };
        // `refresh_ticking_areas` notices the area is gone and pops the edit screen.
        self.refresh_ticking_areas(Some(status));
    }

    fn set_options_status(&mut self, pending_reset: Option<SettingsScope>, status: String) {
        if let GameState::Options(options) = &mut self.game_state {
            options.pending_reset = pending_reset;
            options.status_line = Some(status);
        }
    }

    /// "Back" for every menu screen: first let the current screen swallow it to
    /// cancel a pending confirmation, otherwise pop one level off the stack.
    fn handle_menu_back(&mut self) -> bool {
        if self.cancel_pending_menu_action() {
            return true;
        }
        self.pop_menu_screen()
    }

    /// Escape on an armed rebind/reset/delete cancels that instead of navigating.
    fn cancel_pending_menu_action(&mut self) -> bool {
        match &mut self.game_state {
            GameState::Options(options) => {
                if options.awaiting_rebind.take().is_some() {
                    options.status_line = Some("Rebind cancelled.".to_string());
                    return true;
                }
                if options.pending_reset.take().is_some() {
                    options.status_line = Some("Reset cancelled.".to_string());
                    return true;
                }
                false
            }
            GameState::WorldActions(menu) => {
                if menu.rename_input.take().is_some() {
                    menu.status_line = Some("Rename cancelled.".to_string());
                    return true;
                }
                if menu.pending_delete {
                    menu.pending_delete = false;
                    menu.status_line = Some("Delete cancelled.".to_string());
                    return true;
                }
                false
            }
            GameState::TickingAreas(menu) => {
                if menu.pending_delete.take().is_some() {
                    menu.status_line = Some("Delete cancelled.".to_string());
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    fn handle_rebind_input(&mut self, code: KeyCode) -> bool {
        let action = match &self.game_state {
            GameState::Options(options) => options.awaiting_rebind,
            _ => None,
        };
        let Some(action) = action else {
            return false;
        };

        let conflict = self.settings.controls.set_key_swapping(action, code);
        let save_error = self.persist_settings();
        let status = if let Some(error) = save_error {
            error
        } else if let Some(other) = conflict {
            format!(
                "{} bound to {}. Swapped with {}.",
                action.label(),
                keycode_label(code),
                other.label()
            )
        } else {
            format!("{} bound to {}.", action.label(), keycode_label(code))
        };
        if let GameState::Options(options) = &mut self.game_state {
            options.awaiting_rebind = None;
            options.status_line = Some(status);
        }
        true
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
            self.settings.graphics.render_distance.label(),
            self.settings
                .graphics
                .render_distance
                .view_distance_chunks()
        ));
        lines.push(format!(
            "Simulation distance: {} ({} chunks)",
            self.simulation_distance.label(),
            self.simulation_distance.distance_chunks().min(
                self.settings
                    .graphics
                    .render_distance
                    .view_distance_chunks()
            )
        ));
        lines.push(format!(
            "Selected slot: {}/{}",
            self.selected_slot + 1,
            HOTBAR_SLOT_COUNT
        ));
        lines.push(format!(
            "Held block: {}",
            self.selected_block()
                .map(BlockId::display_name)
                .unwrap_or("Empty")
        ));
        if let Some(state) = self.state.as_ref() {
            lines.push(format!(
                "Weather: {} {} (F5)",
                state
                    .active_precipitation_kind()
                    .map(|kind| kind.label())
                    .unwrap_or_else(|| {
                        state
                            .world_environment()
                            .default_precipitation()
                            .map_or("weather", |kind| kind.label())
                    }),
                if state.precipitation_enabled() {
                    "On"
                } else {
                    "Off"
                }
            ));
            lines.push(state.world_session_debug_line());
            lines.push(state.simulation_debug_line());
            lines.push(state.worldgen_features_debug_line());
            lines.push(state.winter_debug_line());
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
            if let Some(line) = state.deferred_edit_waiting_debug_line() {
                lines.push(line);
            }
        }

        lines
    }

    fn menu_overlay_content(&self) -> Option<MenuScreenContent> {
        match &self.game_state {
            GameState::MainMenu { selected } => Some(MenuScreenContent {
                title: "Minecraft Klon".to_string(),
                items: MAIN_MENU_ITEMS
                    .iter()
                    .map(|item| (*item).to_string())
                    .collect(),
                footer: Some("Classic parity flow: generate, load, options.".to_string()),
                selected: *selected,
                enabled: vec![true; MAIN_MENU_ITEMS.len()],
                sliders: MenuScreenContent::no_sliders(MAIN_MENU_ITEMS.len()),
                show_panorama: true,
            }),
            GameState::LoadLevelMenu { selected, worlds } => {
                let mut items = if worlds.is_empty() {
                    vec!["No saved levels found".to_string()]
                } else {
                    worlds.iter().map(|entry| entry.label.clone()).collect()
                };
                items.push("Back".to_string());
                let mut enabled = vec![!worlds.is_empty(); items.len().saturating_sub(1)];
                enabled.push(true);
                let footer = if *selected < worlds.len() {
                    worlds
                        .get(*selected)
                        .map(|entry| entry.summary.save_root.display().to_string())
                } else {
                    Some("Saved worlds live under save/worlds.".to_string())
                };
                let footer = footer.map(|text| format!("{text}  |  Enter: manage / load"));
                Some(MenuScreenContent {
                    sliders: MenuScreenContent::no_sliders(items.len()),
                    title: "Load level".to_string(),
                    items,
                    footer,
                    selected: *selected,
                    enabled,
                    show_panorama: self.is_main_menu(),
                })
            }
            GameState::Paused { selected } => Some(MenuScreenContent {
                title: "Game menu".to_string(),
                items: PAUSE_MENU_ITEMS
                    .iter()
                    .map(|item| (*item).to_string())
                    .collect(),
                footer: None,
                selected: *selected,
                enabled: vec![true; PAUSE_MENU_ITEMS.len()],
                sliders: MenuScreenContent::no_sliders(PAUSE_MENU_ITEMS.len()),
                show_panorama: false,
            }),
            GameState::Options(options) => {
                let (title, mut items) = match options.screen {
                    OptionsScreen::Root => (
                        "Options".to_string(),
                        OPTIONS_ROOT_ITEMS
                            .iter()
                            .map(|item| (*item).to_string())
                            .collect::<Vec<_>>(),
                    ),
                    OptionsScreen::Gameplay => (
                        "General / Gameplay".to_string(),
                        vec![
                            format!(
                                "View bobbing: {}",
                                if self.settings.gameplay.view_bobbing {
                                    "On"
                                } else {
                                    "Off"
                                }
                            ),
                            format!("Field of view: {}°", self.settings.gameplay.base_fov_deg),
                        ],
                    ),
                    OptionsScreen::Graphics => (
                        "Video / Graphics".to_string(),
                        vec![
                            format!(
                                "Render distance: {} ({} chunks)",
                                self.settings.graphics.render_distance.label(),
                                self.settings
                                    .graphics
                                    .render_distance
                                    .view_distance_chunks()
                            ),
                            format!(
                                "Backface culling: {}",
                                if self.settings.graphics.backface_culling {
                                    "On"
                                } else {
                                    "Off"
                                }
                            ),
                            format!(
                                "VSync: {}",
                                if self.settings.graphics.vsync {
                                    "On"
                                } else {
                                    "Off"
                                }
                            ),
                            format!(
                                "GUI scale: {} ({:.0}%)",
                                self.settings.graphics.gui_scale.label(),
                                self.settings.graphics.gui_scale.factor() * 100.0
                            ),
                        ],
                    ),
                    OptionsScreen::Controls => {
                        let mut items = Vec::with_capacity(InputAction::ALL.len());
                        for action in InputAction::ALL {
                            items.push(format!(
                                "{}: {}",
                                action.label(),
                                keycode_label(self.settings.controls.key_for(action))
                            ));
                        }
                        ("Controls".to_string(), items)
                    }
                };

                let scope = options.screen.reset_scope();
                let at_defaults = self.settings.is_default(scope);
                let armed = options.pending_reset == Some(scope);
                items.push(if armed {
                    format!("Reset {} to defaults - confirm?", scope.label())
                } else {
                    format!("Reset {} to defaults", scope.label())
                });
                items.push("Back".to_string());
                let mut enabled = vec![true; items.len()];
                enabled[options.screen.reset_row()] = !at_defaults;
                let mut sliders = MenuScreenContent::no_sliders(items.len());
                for (index, slot) in sliders.iter_mut().enumerate() {
                    if self.menu_row_is_slider(index) {
                        *slot = Some(base_fov_fill01(self.settings.gameplay.base_fov_deg));
                    }
                }

                let footer = if let Some(action) = options.awaiting_rebind {
                    Some(format!(
                        "Press a key for {}. Escape cancels.",
                        action.label()
                    ))
                } else if let Some(status) = options.status_line.clone() {
                    Some(status)
                } else if at_defaults {
                    Some(format!("{} at defaults.", capitalize(scope.label())))
                } else {
                    None
                };
                Some(MenuScreenContent {
                    title,
                    selected: options.selected,
                    enabled,
                    sliders,
                    items,
                    footer,
                    show_panorama: self.is_main_menu(),
                })
            }
            GameState::WorldActions(menu) => Some(Self::world_actions_menu_content(menu)),
            GameState::TickingAreas(menu) => Some(self.ticking_area_menu_content(menu)),
            _ => None,
        }
    }

    fn world_actions_menu_content(menu: &WorldActionsMenuState) -> MenuScreenContent {
        let mut items = Vec::with_capacity(WORLD_ACTION_ROW_COUNT);
        items.push(if menu.is_active {
            "Load world (already loaded)".to_string()
        } else {
            "Load world".to_string()
        });
        items.push(match &menu.rename_input {
            Some(input) => format!("Name: {input}_"),
            None => format!("Rename: {}", menu.display_name),
        });
        items.push(if menu.pending_delete {
            "Delete world - confirm?".to_string()
        } else {
            "Delete world".to_string()
        });
        items.push("Back".to_string());

        let mut enabled = vec![true; items.len()];
        enabled[WORLD_ACTION_ROW_DELETE] = menu.can_delete;

        let footer = menu.status_line.clone().or_else(|| {
            if menu.details.is_empty() {
                None
            } else {
                Some(format!("{} | {}", menu.details, menu.save_root.display()))
            }
        });

        MenuScreenContent {
            title: menu.display_name.clone(),
            selected: menu.selected,
            enabled,
            sliders: MenuScreenContent::no_sliders(items.len()),
            items,
            footer,
            show_panorama: false,
        }
    }

    fn ticking_area_menu_content(&self, menu: &TickingAreaMenuState) -> MenuScreenContent {
        let (cx, cz) = self.player_chunk_xz();
        match menu.screen {
            TickingAreaScreen::List => {
                let mut items: Vec<String> = menu
                    .areas
                    .iter()
                    .map(|area| {
                        format!(
                            "#{} at ({}, {})  r={}  [{}]",
                            area.id,
                            area.center.x,
                            area.center.y,
                            area.radius_chunks,
                            ticking_flag_summary(area.flags)
                        )
                    })
                    .collect();
                items.push(format!("Add area at current chunk ({cx}, {cz})"));
                items.push("Back to game".to_string());
                let footer = menu.status_line.clone().or_else(|| {
                    Some(if menu.areas.is_empty() {
                        "No ticking areas yet. Areas keep chunks loaded and ticking.".to_string()
                    } else {
                        format!("{} area(s). Enter edits, Escape leaves.", menu.areas.len())
                    })
                });
                MenuScreenContent {
                    title: "Ticking areas".to_string(),
                    enabled: vec![true; items.len()],
                    sliders: MenuScreenContent::no_sliders(items.len()),
                    items,
                    footer,
                    selected: menu.selected,
                    show_panorama: false,
                }
            }
            TickingAreaScreen::Edit { id } => {
                let Some(area) = menu.area(id) else {
                    return MenuScreenContent {
                        title: "Ticking area".to_string(),
                        items: vec!["Back".to_string()],
                        footer: Some(format!("Area #{id} no longer exists.")),
                        selected: 0,
                        enabled: vec![true],
                        sliders: vec![None],
                        show_panorama: false,
                    };
                };
                let mut items = Vec::with_capacity(TICKING_EDIT_ROW_COUNT);
                items.push(format!(
                    "Radius: {} chunk(s)  (max {})",
                    area.radius_chunks, TICKING_AREA_MAX_RADIUS_CHUNKS
                ));
                for (bit, label) in TICKING_EDIT_FLAG_ROWS {
                    items.push(format!(
                        "{label}: {}",
                        if area.flags.bits() & bit != 0 {
                            "On"
                        } else {
                            "Off"
                        }
                    ));
                }
                items.push(if menu.pending_delete == Some(id) {
                    "Delete area - confirm?".to_string()
                } else {
                    "Delete area".to_string()
                });
                items.push("Back".to_string());
                let footer = menu.status_line.clone().or_else(|| {
                    Some(format!(
                        "Area #{id} at chunk ({}, {}). Left/right adjusts, Enter toggles.",
                        area.center.x, area.center.y
                    ))
                });
                MenuScreenContent {
                    title: format!("Ticking area #{id}"),
                    enabled: vec![true; items.len()],
                    sliders: MenuScreenContent::no_sliders(items.len()),
                    items,
                    footer,
                    selected: menu.selected,
                    show_panorama: false,
                }
            }
        }
    }

    fn activate_current_menu_selected(&mut self, event_loop: &ActiveEventLoop) {
        match self.game_state {
            GameState::MainMenu { .. } => self.activate_main_menu_selected(event_loop),
            GameState::Paused { .. } => self.activate_pause_menu_selected(event_loop),
            GameState::LoadLevelMenu { .. } => self.activate_load_level_menu_selected(),
            GameState::Options(_) => self.activate_options_selected(),
            GameState::WorldActions(_) => self.activate_world_action_selected(),
            GameState::TickingAreas(_) => self.activate_ticking_area_selected(),
            _ => {}
        }
    }

    fn spawn_probe_feet_for_column(world: &World, world_x: i32, world_z: i32) -> Vec3 {
        let min_y = world.min_y();
        let max_start_y = (world.max_y() - 2).max(min_y + 2);
        let terrain_y = world.terrain_height_at(world_x, world_z);
        let estimated_y = world.estimated_surface_height_at(world_x, world_z);
        let start_y = (terrain_y.max(estimated_y) + 8).clamp(min_y + 2, max_start_y);
        Vec3::new(world_x as f32 + 0.5, start_y as f32, world_z as f32 + 0.5)
    }

    fn try_surface_spawn_in_column(
        world: &World,
        world_x: i32,
        world_z: i32,
    ) -> Option<(Vec3, bool)> {
        let (settled_feet, on_ground) = Self::settle_spawn_feet(
            world,
            Self::spawn_probe_feet_for_column(world, world_x, world_z),
        );
        if !on_ground {
            return None;
        }

        let min_y = world.min_y();
        let feet_y = settled_feet.y.floor() as i32;
        let ground_y = feet_y - 1;
        if ground_y <= min_y {
            return None;
        }

        let ground_block = world.get_block(IVec3::new(world_x, ground_y, world_z));
        if !ground_block.is_solid() || ground_block == BlockId::Bedrock {
            return None;
        }

        Some((settled_feet, true))
    }

    fn world_spawn_settled_feet_near(
        state: &mut State,
        center_x: i32,
        center_z: i32,
    ) -> (Vec3, bool) {
        let mut fallback: Option<(Vec3, bool)> = None;

        for radius in 0..=SPAWN_SEARCH_RADIUS_BLOCKS {
            for dz in -radius..=radius {
                for dx in -radius..=radius {
                    if radius != 0 && dx.abs() != radius && dz.abs() != radius {
                        continue;
                    }
                    let world_x = center_x + dx;
                    let world_z = center_z + dz;
                    state.ensure_column_loaded_or_generated(world_x, world_z);

                    if let Some(candidate) =
                        Self::try_surface_spawn_in_column(&state.world, world_x, world_z)
                    {
                        return candidate;
                    }

                    if fallback.is_none() {
                        fallback = Some(Self::settle_spawn_feet(
                            &state.world,
                            Self::spawn_probe_feet_for_column(&state.world, world_x, world_z),
                        ));
                    }
                }
            }
        }

        fallback.unwrap_or_else(|| {
            let probe = Self::spawn_probe_feet_for_column(&state.world, center_x, center_z);
            Self::settle_spawn_feet(&state.world, probe)
        })
    }

    fn settle_spawn_feet(world: &World, spawn_feet: Vec3) -> (Vec3, bool) {
        let settle_distance = (world.max_y() - world.min_y() + 4).max(8) as f32;
        let (settled_feet, settle_flags) =
            move_with_collisions(world, spawn_feet, Vec3::new(0.0, -settle_distance, 0.0));
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
        self.sync_camera_to_player_eye(true);
    }

    fn make_worldgen_request(
        &self,
        classic_size: ClassicLevelSize,
        environment: WorldEnvironment,
    ) -> WorldGenRequest {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let nanos = now.as_nanos() as u64;
        WorldGenRequest {
            seed: nanos ^ (nanos.rotate_left(23)),
            params: WorldGenParams {
                classic_size,
                environment,
                ..WorldGenParams::default()
            },
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
        crate::console_info!(
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
        self.sync_camera_to_player_eye(true);
    }

    pub(crate) fn command_set_creative_mode(&mut self) {
        self.player.player_mode = PlayerMode::Creative;
        self.player.mode = MoveMode::Flight;
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
        self.clear_action_state();
        self.clear_double_space_state();
        self.sync_camera_to_player_eye(true);
    }

    pub(crate) fn command_set_survival_mode(&mut self, state: &State) {
        self.player.player_mode = PlayerMode::Survival;
        self.player.mode = MoveMode::Walk;
        self.player._velocity_y = 0.0;
        self.player.walk_velocity = Vec3::ZERO;
        self.player.swim_velocity = Vec3::ZERO;
        self.player.bob_phase = 0.0;
        self.player.was_at_surface = false;
        self.player.sprint_swim_active = false;
        self.player.is_swim_prone = false;
        self.player.jump_hold_cooldown_seconds = 0.0;
        let (snapped, grounded) = snap_to_ground_with_height(
            &state.world,
            self.player.feet_position,
            self.player.collision_height(),
        );
        self.player.feet_position = snapped;
        self.player.on_ground = grounded;
        self.player.time_since_grounded = if grounded { 0.0 } else { f32::INFINITY };
        self.controller.clear_jump_requested();
        self.clear_action_state();
        self.clear_double_space_state();
        self.sync_camera_to_player_eye(true);
    }

    pub(crate) fn command_teleport_feet(&mut self, x: f32, y: Option<f32>, z: f32) {
        let target_y = y.unwrap_or(self.player.feet_position.y);
        self.player.feet_position = Vec3::new(x, target_y, z);
        self.player._velocity_y = 0.0;
        self.player.walk_velocity = Vec3::ZERO;
        self.player.swim_velocity = Vec3::ZERO;
        self.player.bob_phase = 0.0;
        self.player.bob_anchor_y = target_y;
        self.player.jump_hold_cooldown_seconds = 0.0;
        self.player.on_ground = false;
        self.player.time_since_grounded = f32::INFINITY;
        self.controller.clear_jump_requested();
        self.clear_action_state();
        self.sync_camera_to_player_eye(true);
    }

    pub(crate) fn player_feet_position(&self) -> Vec3 {
        self.player.feet_position
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
        if let Some(block) = self.selected_block() {
            state.rebuild_preview_mesh(block);
        }

        state.set_simulation_distance(self.simulation_distance, &self.camera);
        state.set_render_distance(self.settings.graphics.render_distance, &self.camera);
        state.set_vsync_enabled(self.settings.graphics.vsync);
        state.set_backface_culling_enabled(self.settings.graphics.backface_culling);
        state.set_gui_scale(self.settings.graphics.gui_scale);

        self.window = Some(window);
        self.state = Some(state);
        self.enter_main_menu();
        self.last_frame = Some(Instant::now());
        self.app_start = Some(Instant::now());
        self.fps_last_instant = Some(Instant::now());
        self.fps_frame_count = 0;
        self.fps_value = 0;
        self.reset_chat_state();
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
                if let Some(index) = self.menu_slider_drag {
                    // Keep tracking outside the row so the drag does not snap
                    // back when the cursor leaves the widget vertically.
                    self.drag_menu_slider(index);
                } else if self.is_menu_open()
                    && let Some(index) = self.menu_item_at_cursor()
                {
                    self.set_selected_menu_index(index);
                } else if self.is_menu_open() {
                    self.clear_menu_press();
                }
            }

            WindowEvent::Ime(Ime::Commit(text)) => {
                if !self.is_in_game() || text.is_empty() || !self.chat_open {
                    return;
                }
                self.chat_insert_text(&text, ChatTextInputSource::Ime);
            }

            // Tastatur: WASD/Space/Shift, Double-Space toggelt Flight/Walking, ESC öffnet/schließt Menü
            WindowEvent::KeyboardInput { event, .. } => {
                if self.chat_open {
                    let mut handled_chat_key = false;
                    if event.state == ElementState::Pressed
                        && let PhysicalKey::Code(code) = event.physical_key
                    {
                        match code {
                            KeyCode::Escape => {
                                self.close_chat();
                                handled_chat_key = true;
                            }
                            KeyCode::Enter | KeyCode::NumpadEnter => {
                                self.submit_chat_input();
                                handled_chat_key = true;
                            }
                            KeyCode::Backspace => {
                                self.chat_backspace();
                                handled_chat_key = true;
                            }
                            KeyCode::Delete => {
                                self.chat_delete();
                                handled_chat_key = true;
                            }
                            KeyCode::ArrowLeft => {
                                self.chat_move_cursor_left();
                                handled_chat_key = true;
                            }
                            KeyCode::ArrowRight => {
                                self.chat_move_cursor_right();
                                handled_chat_key = true;
                            }
                            KeyCode::ArrowUp => {
                                self.chat_history_up();
                                handled_chat_key = true;
                            }
                            KeyCode::ArrowDown => {
                                self.chat_history_down();
                                handled_chat_key = true;
                            }
                            KeyCode::Home => {
                                self.chat_input_cursor = 0;
                                handled_chat_key = true;
                            }
                            KeyCode::End => {
                                self.chat_input_cursor = self.chat_input.len();
                                handled_chat_key = true;
                            }
                            _ => {}
                        }
                    }
                    if !handled_chat_key && event.state == ElementState::Pressed {
                        if let Some(text) = event.text.as_deref() {
                            self.chat_insert_text(text, ChatTextInputSource::Keyboard);
                        }
                    }
                    return;
                }

                let code = match event.physical_key {
                    PhysicalKey::Code(code) => Some(code),
                    _ => None,
                };
                let generating = self.is_generating_world();
                let inventory_open = self.is_inventory_open();
                let options_open = self.is_options_open();
                let ticking_areas_open = self.is_ticking_areas_open();
                let menu_open = self.is_menu_open();
                let pause_key = self.settings.controls.key_for(InputAction::PauseMenu);
                let inventory_key = self.settings.controls.key_for(InputAction::OpenInventory);
                let chat_key = self.settings.controls.key_for(InputAction::OpenChat);
                let ticking_areas_key = self
                    .settings
                    .controls
                    .key_for(InputAction::OpenTickingAreas);
                let jump_key = self.settings.controls.key_for(InputAction::Jump);
                let forward_key = self.settings.controls.key_for(InputAction::MoveForward);
                let backward_key = self.settings.controls.key_for(InputAction::MoveBackward);
                let sneak_key = self.settings.controls.key_for(InputAction::Sneak);

                if event.state == ElementState::Pressed
                    && matches!(&self.game_state, GameState::WorldActions(menu) if menu.rename_input.is_some())
                    && self.handle_world_rename_key(code, event.text.as_deref())
                {
                    return;
                }

                // Skipped while the Controls screen is capturing a key, so the
                // screenshot key itself stays rebindable.
                let awaiting_rebind = matches!(
                    &self.game_state,
                    GameState::Options(options) if options.awaiting_rebind.is_some()
                );
                if !awaiting_rebind
                    && event.state == ElementState::Pressed
                    && !event.repeat
                    && let Some(code) = code
                    && code == self.settings.controls.key_for(InputAction::Screenshot)
                {
                    self.take_screenshot();
                    return;
                }

                if !awaiting_rebind
                    && event.state == ElementState::Pressed
                    && !event.repeat
                    && let Some(code) = code
                    && code
                        == self
                            .settings
                            .controls
                            .key_for(InputAction::ToggleCameraMode)
                {
                    if self.is_in_game() {
                        self.cycle_camera_mode();
                    }
                    return;
                }

                if options_open
                    && event.state == ElementState::Pressed
                    && !event.repeat
                    && let Some(code) = code
                    && let GameState::Options(options) = &self.game_state
                    && options.awaiting_rebind.is_some()
                {
                    if code == KeyCode::Escape {
                        self.handle_menu_back();
                    } else {
                        self.handle_rebind_input(code);
                    }
                    return;
                }

                if event.state == ElementState::Pressed
                    && let Some(code) = code
                {
                    let pause_pressed = code == pause_key || code == KeyCode::Escape;
                    if pause_pressed {
                        if generating {
                            return;
                        }
                        if inventory_open {
                            self.leave_inventory();
                        } else if menu_open {
                            self.handle_menu_back();
                        } else if self.is_in_game() {
                            self.enter_pause();
                        }
                        return;
                    }

                    if code == inventory_key && !event.repeat {
                        if generating {
                            return;
                        }
                        if inventory_open {
                            self.leave_inventory();
                        } else if self.is_in_game() {
                            self.enter_inventory();
                        }
                        return;
                    }

                    if code == ticking_areas_key && !event.repeat {
                        if generating {
                            return;
                        }
                        if ticking_areas_open {
                            self.leave_ticking_area_menu();
                        } else if inventory_open {
                            self.leave_inventory();
                            self.open_ticking_area_menu();
                        } else if self.is_in_game() {
                            self.open_ticking_area_menu();
                        }
                        return;
                    }
                }

                if menu_open {
                    if event.state == ElementState::Pressed {
                        if let Some(code) = code {
                            if code == KeyCode::F3 && !event.repeat {
                                self.toggle_debug_overlay();
                                return;
                            }
                            if event.repeat {
                                return;
                            }
                            match code {
                                KeyCode::ArrowUp | KeyCode::KeyW => self.move_menu_selection(-1),
                                KeyCode::ArrowDown | KeyCode::KeyS => self.move_menu_selection(1),
                                KeyCode::ArrowLeft | KeyCode::KeyA => {
                                    self.apply_selected_menu_step(-1);
                                }
                                KeyCode::ArrowRight | KeyCode::KeyD => {
                                    self.apply_selected_menu_step(1);
                                }
                                KeyCode::Enter | KeyCode::NumpadEnter | KeyCode::Space => {
                                    self.activate_current_menu_selected(event_loop);
                                }
                                _ => {}
                            }
                        }
                    }
                    return;
                }

                if inventory_open {
                    if event.state == ElementState::Pressed
                        && let PhysicalKey::Code(KeyCode::F3) = event.physical_key
                        && !event.repeat
                    {
                        self.toggle_debug_overlay();
                    }
                    return;
                }

                if !generating {
                    if let Some(code) = code {
                        match (code, event.state) {
                            (code, ElementState::Pressed)
                                if code == forward_key && !event.repeat =>
                            {
                                self.handle_forward_pressed(Instant::now());
                            }
                            (code, ElementState::Released) if code == forward_key => {
                                self.controller.set_sprint_latched(false);
                            }
                            (code, ElementState::Pressed)
                                if code == backward_key || code == sneak_key =>
                            {
                                self.controller.set_sprint_latched(false);
                            }
                            _ => {}
                        }
                    }
                }
                if event.state == ElementState::Pressed {
                    if let Some(code) = code {
                        if !generating && code == chat_key && !event.repeat {
                            if self.controller.sprint_held {
                                if let Some(state) = self.state.as_mut() {
                                    let chunk = World::world_to_chunk_coord(IVec3::new(
                                        self.camera.position.x.floor() as i32,
                                        self.camera.position.y.floor() as i32,
                                        self.camera.position.z.floor() as i32,
                                    ));
                                    if self.controller.move_down {
                                        match state.delete_nearest_ticking_area(chunk.x, chunk.z) {
                                            Some(id) => crate::console_info!(
                                                "Ticking area removed: id={} near chunk ({}, {})",
                                                id,
                                                chunk.x,
                                                chunk.z
                                            ),
                                            None => {
                                                crate::console_info!("No ticking area to remove.")
                                            }
                                        }
                                    } else {
                                        let id =
                                            state.create_ticking_area_radius(chunk.x, chunk.z, 4);
                                        crate::console_info!(
                                            "Ticking area created: id={} center=({}, {}) radius=4",
                                            id,
                                            chunk.x,
                                            chunk.z
                                        );
                                    }
                                    state.update_simulation_activation(&self.camera);
                                }
                            } else {
                                self.open_chat();
                            }
                            return;
                        }

                        if code == KeyCode::KeyN && !event.repeat {
                            self.start_new_default_level_generation();
                        }

                        if !generating && code == jump_key && !event.repeat {
                            self.handle_space_pressed(Instant::now());
                        }

                        if code == KeyCode::F3 && !event.repeat {
                            self.toggle_debug_overlay();
                        }

                        if !generating && code == KeyCode::F4 && !event.repeat {
                            if let Some(state) = self.state.as_mut() {
                                state.toggle_debug_depth_test();
                            }
                        }

                        if !generating && code == KeyCode::F5 && !event.repeat {
                            if let Some(state) = self.state.as_mut() {
                                state.toggle_precipitation();
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

                        if !generating && code == KeyCode::F9 && !event.repeat {
                            if let Some(state) = self.state.as_ref() {
                                let view_camera = self.current_view_camera();
                                crate::console_info!(
                                    "{}",
                                    state.fluid_debug_under_crosshair_line(&view_camera)
                                );
                            }
                        }

                        if !generating && code == KeyCode::F10 && !event.repeat {
                            if let Some(state) = self.state.as_mut() {
                                state.toggle_debug_perf_summary();
                            }
                        }

                        if !generating && code == KeyCode::KeyF && !event.repeat {
                            let all = RenderDistance::ALL;
                            if let Some(index) = all.iter().position(|candidate| {
                                *candidate == self.settings.graphics.render_distance
                            }) {
                                self.settings.graphics.render_distance =
                                    all[(index + 1) % all.len()];
                            }
                            let _ = self.persist_settings();
                            self.apply_settings_to_state();
                            crate::console_info!(
                                "Render distance: {} ({} chunks)",
                                self.settings.graphics.render_distance.label(),
                                self.settings
                                    .graphics
                                    .render_distance
                                    .view_distance_chunks()
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
                            crate::console_info!(
                                "Simulation distance: {} ({} chunks)",
                                self.simulation_distance.label(),
                                self.simulation_distance.distance_chunks().min(
                                    self.settings
                                        .graphics
                                        .render_distance
                                        .view_distance_chunks()
                                )
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
                    self.controller.process_key(&event, &self.settings.controls);
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                if self.is_generating_world() || self.is_menu_open() || self.chat_open {
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
                    if self.is_inventory_open() {
                        let page_count = self
                            .state
                            .as_ref()
                            .map(|state| state.inventory_palette_page_count(CLASSIC_PALETTE.len()))
                            .unwrap_or(1);
                        let max_page = page_count.saturating_sub(1) as i32;
                        if let GameState::Inventory { scroll, .. } = &mut self.game_state {
                            *scroll = (*scroll + steps).clamp(0, max_page);
                        }
                    } else {
                        self.cycle_selected_slot(steps);
                    }
                }
            }

            // Minecraft-übliches Mapping:
            // LMB = abbauen, RMB = platzieren.
            WindowEvent::MouseInput {
                state: button_state,
                button,
                ..
            } => {
                if self.is_generating_world() || self.chat_open {
                    return;
                }

                if self.is_menu_open() {
                    if button == MouseButton::Left {
                        match button_state {
                            ElementState::Pressed => {
                                self.menu_pressed_index = self.menu_item_at_cursor();
                                if let Some(index) = self.menu_pressed_index {
                                    self.set_selected_menu_index(index);
                                    // A slider jumps to the click position and
                                    // keeps following the cursor while held.
                                    if self.drag_menu_slider(index) {
                                        self.menu_slider_drag = Some(index);
                                    }
                                }
                            }
                            ElementState::Released => {
                                let pressed_index = self.menu_pressed_index.take();
                                let dragging_slider = self.menu_slider_drag.take().is_some();
                                if dragging_slider {
                                    // One write per drag, not one per cursor move.
                                    if let Some(error) = self.persist_settings() {
                                        self.set_options_status(None, error);
                                    }
                                }
                                let released_index = self.menu_item_at_cursor();
                                if let Some(index) = released_index {
                                    self.set_selected_menu_index(index);
                                }
                                if !dragging_slider
                                    && let Some(index) = pressed_index
                                    && released_index == Some(index)
                                {
                                    self.activate_current_menu_selected(event_loop);
                                }
                            }
                        }
                    }
                    return;
                }

                if self.is_inventory_open() {
                    let Some(state) = self.state.as_ref() else {
                        return;
                    };
                    let page = if let GameState::Inventory { scroll, .. } = &self.game_state {
                        state.inventory_clamp_palette_page(*scroll, CLASSIC_PALETTE.len())
                    } else {
                        0
                    };
                    let hotbar_hit =
                        state.inventory_hotbar_slot_at_pos_px(self.cursor_pos.0, self.cursor_pos.1);
                    let palette_hit = state.inventory_palette_item_at_pos_px(
                        CLASSIC_PALETTE.len(),
                        page,
                        self.cursor_pos.0,
                        self.cursor_pos.1,
                    );
                    let page_count = state.inventory_palette_page_count(CLASSIC_PALETTE.len());
                    if let GameState::Inventory {
                        selected_palette,
                        selected_hotbar_slot,
                        scroll,
                        dragged_block,
                    } = &mut self.game_state
                    {
                        *scroll = (*scroll).clamp(0, page_count.saturating_sub(1) as i32);
                        match (button, button_state) {
                            (MouseButton::Left, ElementState::Pressed) => {
                                if let Some(slot) = hotbar_hit {
                                    *selected_hotbar_slot = slot;
                                    self.selected_slot = slot;
                                }
                                if let Some(index) = palette_hit {
                                    *selected_palette = index;
                                    *dragged_block = CLASSIC_PALETTE.get(index).copied();
                                }
                            }
                            (MouseButton::Left, ElementState::Released) => {
                                if let Some(block) = dragged_block.take()
                                    && let Some(slot) = hotbar_hit
                                {
                                    self.hotbar[slot] = Some(block);
                                    *selected_hotbar_slot = slot;
                                    self.selected_slot = slot;
                                }
                            }
                            (MouseButton::Right, ElementState::Pressed) => {
                                if let Some(slot) = hotbar_hit {
                                    self.hotbar[slot] = None;
                                }
                            }
                            _ => {}
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
                    let view_camera = Self::compose_view_camera(
                        self.camera,
                        self.settings.gameplay.view_bobbing,
                        self.view_bobbing,
                    );
                    if let Some(state) = self.state.as_mut() {
                        match button {
                            MouseButton::Left => {
                                self.break_held = true;
                                state.destroy_block_at_camera_ray(
                                    &view_camera,
                                    self.player.player_mode,
                                );
                                self.break_cooldown_seconds =
                                    Self::block_action_repeat_cooldown_seconds();
                            }
                            MouseButton::Right => {
                                if let Some(selected_block) = selected_block {
                                    self.place_held = true;
                                    state.place_block_at_camera_ray(
                                        &view_camera,
                                        selected_block,
                                        self.player.feet_position,
                                    );
                                    self.place_cooldown_seconds =
                                        Self::block_action_repeat_cooldown_seconds();
                                }
                            }
                            MouseButton::Middle => {
                                if let Some(picked_block) =
                                    state.pick_block_at_camera_ray(&view_camera)
                                    && picked_block != BlockId::Air
                                {
                                    self.hotbar[self.selected_slot] = Some(picked_block);
                                    crate::console_info!(
                                        "Picked block: {} into slot {}/{}",
                                        picked_block.display_name(),
                                        self.selected_slot + 1,
                                        HOTBAR_SLOT_COUNT
                                    );
                                }
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
                let _frame_total_trace = perf_trace::scope("frame.total").with_counts(|| {
                    format!(
                        "in_game={} paused={} inv={} chat_open={} mouse_captured={}",
                        self.is_in_game() as u8,
                        self.is_paused() as u8,
                        self.is_inventory_open() as u8,
                        self.chat_open as u8,
                        self.controller.mouse_captured as u8
                    )
                });
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
                    if let GameState::GeneratingWorld {
                        title,
                        progress,
                        phase,
                        stats,
                    } = &mut self.game_state
                    {
                        while let Some(update) = state.poll_worldgen_progress() {
                            *title = update.title;
                            *progress = update.progress01.clamp(0.0, 1.0);
                            *phase = update.phase;
                            *stats = update.stats;
                        }
                        completed_spawn = state.try_finish_world_startup_loading(&self.camera);
                    }
                }

                if let Some(spawn_feet) = completed_spawn {
                    self.clear_action_state();
                    let spawn_data = self.state.as_mut().map(|state| {
                        let center_x = spawn_feet.x.floor() as i32;
                        let center_z = spawn_feet.z.floor() as i32;
                        Self::world_spawn_settled_feet_near(state, center_x, center_z)
                    });
                    if let Some((settled_feet, on_ground)) = spawn_data {
                        self.apply_settled_spawn(settled_feet, on_ground);
                    }
                    if let Some(state) = self.state.as_mut() {
                        state.set_render_distance(
                            self.settings.graphics.render_distance,
                            &self.camera,
                        );
                    }
                    self.game_state = GameState::InGame;
                    if let Some(window) = self.window.as_ref() {
                        self.controller.set_mouse_capture(window, true);
                    }
                }

                if self.is_generating_world()
                    && self
                        .state
                        .as_ref()
                        .is_some_and(|state| !state.is_world_startup_loading())
                {
                    self.game_state = GameState::InGame;
                    if let Some(window) = self.window.as_ref() {
                        self.controller.set_mouse_capture(window, true);
                    }
                }

                if self.is_generating_world() {
                    self.clear_action_state();
                }

                let frame_input_trace = perf_trace::scope("frame.input").with_counts(|| {
                    format!(
                        "chat_open={} debug={} wheel_accum={:.2}",
                        self.chat_open as u8,
                        self.show_debug_overlay as u8,
                        self.mouse_wheel_accum_y
                    )
                });
                let in_game = self.is_in_game();
                let gameplay_active = in_game && !self.chat_open;
                let world_processing_active = self
                    .state
                    .as_ref()
                    .is_some_and(|state| state.world_runtime_processing_enabled());
                let debug_lines_owned = if self.show_debug_overlay && in_game {
                    Some(self.build_debug_overlay_lines())
                } else {
                    None
                };
                let debug_line_refs = debug_lines_owned
                    .as_ref()
                    .map(|lines| lines.iter().map(|line| line.as_str()).collect::<Vec<_>>());
                let chat_lines_owned = if in_game {
                    let line_count = self.chat_lines.len().min(CHAT_VISIBLE_LINES);
                    let start = self.chat_lines.len().saturating_sub(line_count);
                    Some(
                        self.chat_lines
                            .iter()
                            .skip(start)
                            .map(|line| {
                                (
                                    line.text.clone(),
                                    now.saturating_duration_since(line.time).as_secs_f32(),
                                )
                            })
                            .collect::<Vec<_>>(),
                    )
                } else {
                    None
                };
                drop(frame_input_trace);
                let menu_overlay_owned = self.menu_overlay_content();
                let menu_item_refs = menu_overlay_owned.as_ref().map(|content| {
                    content
                        .items
                        .iter()
                        .map(|item| item.as_str())
                        .collect::<Vec<_>>()
                });
                let menu_enabled_owned = menu_overlay_owned
                    .as_ref()
                    .map(|content| content.enabled.clone());
                let menu_sliders_owned = menu_overlay_owned
                    .as_ref()
                    .map(|content| content.sliders.clone());
                let menu_open_for_render = self.is_menu_open();
                let show_menu_panorama = menu_overlay_owned
                    .as_ref()
                    .is_some_and(|content| content.show_panorama);
                let inventory_open_for_render = self.is_inventory_open();
                if let Some(state) = self.state.as_mut() {
                    let _update_trace = perf_trace::scope("app.update");
                    let frame_sim_trace = perf_trace::scope("frame.sim").with_counts(|| {
                        format!(
                            "gameplay_active={} world_active={} move_mode={:?} in_game={}",
                            gameplay_active as u8,
                            world_processing_active as u8,
                            self.player.mode,
                            in_game as u8
                        )
                    });
                    if world_processing_active {
                        state.begin_frame_profiling(dt * 1000.0);
                        state.world_time.advance(dt);
                        state.advance_weather(dt);
                    }

                    if gameplay_active {
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
                            Self::update_view_bobbing_state(
                                &mut self.camera,
                                &mut self.view_bobbing,
                                &self.player,
                                self.settings.gameplay.view_bobbing,
                                dt_non_negative,
                                gameplay_active,
                            );

                            self.break_cooldown_seconds =
                                (self.break_cooldown_seconds - dt_non_negative).max(0.0);
                            self.place_cooldown_seconds =
                                (self.place_cooldown_seconds - dt_non_negative).max(0.0);
                            if self.controller.mouse_captured {
                                if self.break_held && self.break_cooldown_seconds <= f32::EPSILON {
                                    let view_camera = Self::compose_view_camera(
                                        self.camera,
                                        self.settings.gameplay.view_bobbing,
                                        self.view_bobbing,
                                    );
                                    state.destroy_block_at_camera_ray(
                                        &view_camera,
                                        self.player.player_mode,
                                    );
                                    self.break_cooldown_seconds =
                                        Self::block_action_repeat_cooldown_seconds();
                                }
                                if self.place_held && self.place_cooldown_seconds <= f32::EPSILON {
                                    if let Some(selected_block) = selected_block {
                                        let view_camera = Self::compose_view_camera(
                                            self.camera,
                                            self.settings.gameplay.view_bobbing,
                                            self.view_bobbing,
                                        );
                                        state.place_block_at_camera_ray(
                                            &view_camera,
                                            selected_block,
                                            self.player.feet_position,
                                        );
                                        self.place_cooldown_seconds =
                                            Self::block_action_repeat_cooldown_seconds();
                                    }
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
                            let view_camera = Self::compose_view_camera(
                                self.camera,
                                self.settings.gameplay.view_bobbing,
                                self.view_bobbing,
                            );
                            state.update_face_highlight(&view_camera, blink_visible);
                        }
                    } else {
                        Self::update_view_bobbing_state(
                            &mut self.camera,
                            &mut self.view_bobbing,
                            &self.player,
                            self.settings.gameplay.view_bobbing,
                            dt.max(0.0),
                            false,
                        );
                        let _perf_scope = self.perf.scope(PerfCat::Highlight);
                        state.clear_targeting_visuals();
                    }

                    if world_processing_active {
                        {
                            let _perf_scope = self.perf.scope(PerfCat::ChunkStream);
                            state.process_dirty_chunk_meshes(&self.camera);
                        }
                        state.pump_mesh_results(&self.camera);
                        {
                            let _perf_scope = self.perf.scope(PerfCat::TexAnim);
                            state.update_texture_animations(dt);
                        }
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
                    let render_camera = if show_menu_panorama {
                        Camera::default()
                    } else {
                        Self::compose_render_camera(
                            self.camera_mode,
                            state,
                            Self::compose_view_camera(
                                self.camera,
                                self.settings.gameplay.view_bobbing,
                                self.view_bobbing,
                            ),
                        )
                    };
                    let player_model_view = Self::build_player_model_view(
                        self.camera_mode,
                        in_game,
                        &self.camera,
                        &self.player,
                        self.view_bobbing,
                        self.controller.move_down,
                        state.player_skin_layer(),
                    );
                    state.set_player_model_view(player_model_view);
                    let world_time = state.world_time;
                    {
                        let _perf_scope = self.perf.scope(PerfCat::Uniforms);
                        state.update_camera_uniform(
                            &render_camera,
                            underwater_kind,
                            overlay_alpha,
                            &world_time,
                        );
                    }

                    let generation_overlay_owned = match &self.game_state {
                        GameState::GeneratingWorld {
                            title,
                            progress,
                            phase,
                            stats,
                        } => Some((title.clone(), *progress, phase.clone(), *stats)),
                        GameState::InGame
                        | GameState::MainMenu { .. }
                        | GameState::LoadLevelMenu { .. }
                        | GameState::Paused { .. }
                        | GameState::Options(_)
                        | GameState::WorldActions(_)
                        | GameState::TickingAreas(_)
                        | GameState::Inventory { .. } => None,
                    };
                    let generation_overlay =
                        generation_overlay_owned
                            .as_ref()
                            .map(|(title, progress, phase, stats)| GenerationOverlay {
                                title: title.as_str(),
                                progress01: *progress,
                                phase: phase.as_str(),
                                stats: *stats,
                            });
                    let menu_overlay = menu_overlay_owned
                        .as_ref()
                        .zip(menu_item_refs.as_ref())
                        .zip(menu_enabled_owned.as_ref())
                        .map(|((content, items), enabled)| MenuOverlay {
                            title: content.title.as_str(),
                            selected: content.selected,
                            pressed: self.menu_pressed_index,
                            items: items.as_slice(),
                            enabled: Some(enabled.as_slice()),
                            sliders: menu_sliders_owned.as_deref(),
                            footer: content.footer.as_deref(),
                            show_panorama: show_menu_panorama,
                        });
                    let inventory_overlay_owned = match &self.game_state {
                        GameState::Inventory {
                            selected_palette,
                            selected_hotbar_slot,
                            scroll,
                            dragged_block,
                        } => {
                            let page =
                                state.inventory_clamp_palette_page(*scroll, CLASSIC_PALETTE.len());
                            let hovered_palette = state.inventory_palette_item_at_pos_px(
                                CLASSIC_PALETTE.len(),
                                page,
                                self.cursor_pos.0,
                                self.cursor_pos.1,
                            );
                            let hovered_hotbar = state.inventory_hotbar_slot_at_pos_px(
                                self.cursor_pos.0,
                                self.cursor_pos.1,
                            );
                            Some((
                                *selected_palette,
                                *selected_hotbar_slot,
                                page,
                                hovered_palette,
                                hovered_hotbar,
                                *dragged_block,
                            ))
                        }
                        _ => None,
                    };
                    let inventory_overlay = inventory_overlay_owned.as_ref().map(
                        |(
                            selected_palette,
                            selected_hotbar_slot,
                            page,
                            hovered_palette,
                            hovered_hotbar,
                            dragged_block,
                        )| {
                            InventoryOverlay {
                                palette_blocks: CLASSIC_PALETTE,
                                selected_palette: (*selected_palette)
                                    .min(CLASSIC_PALETTE.len().saturating_sub(1)),
                                selected_hotbar_slot: (*selected_hotbar_slot)
                                    .min(HOTBAR_SLOT_COUNT.saturating_sub(1)),
                                page: *page,
                                hovered_palette: *hovered_palette,
                                hovered_hotbar_slot: *hovered_hotbar,
                                dragged_block: *dragged_block,
                                cursor_px: [self.cursor_pos.0, self.cursor_pos.1],
                            }
                        },
                    );
                    let debug_overlay = debug_line_refs.as_ref().map(|lines| DebugOverlay {
                        lines: lines.as_slice(),
                    });
                    let chat_line_refs = chat_lines_owned.as_ref().map(|lines| {
                        lines
                            .iter()
                            .map(|(text, age_seconds)| ChatOverlayLine {
                                text: text.as_str(),
                                age_seconds: *age_seconds,
                            })
                            .collect::<Vec<_>>()
                    });
                    let chat_overlay = chat_line_refs.as_ref().map(|lines| ChatOverlay {
                        recent_lines: lines.as_slice(),
                        chat_open: self.chat_open,
                        chat_input: self.chat_input.as_str(),
                        chat_input_cursor: self.chat_input_cursor,
                    });

                    drop(frame_sim_trace);
                    let render_result = {
                        let _render_submit_trace = perf_trace::scope("frame.render.submit")
                            .with_counts(|| {
                                format!(
                                    "debug={} chat_open={} inv={} menu={}",
                                    self.show_debug_overlay as u8,
                                    self.chat_open as u8,
                                    inventory_open_for_render as u8,
                                    menu_open_for_render as u8
                                )
                            });
                        let _render_trace = perf_trace::scope("app.render");
                        let _perf_scope = self.perf.scope(PerfCat::Render);
                        state.render(
                            &render_camera,
                            underwater_kind,
                            generation_overlay,
                            menu_overlay,
                            &self.hotbar,
                            self.selected_slot,
                            inventory_overlay,
                            debug_overlay,
                            chat_overlay,
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
                if frame_ms > HITCH_LOG_THRESHOLD_MS {
                    let counters = self
                        .state
                        .as_ref()
                        .map(State::hitch_counters)
                        .unwrap_or_else(HitchCounters::default);
                    perf_trace::instant_event(
                        "frame.hitch",
                        &format!(
                            "dt_ms={:.2} chunks={} mesh_jobs={} lighting_dirty={} storage_inflight={} active_sim_columns={}",
                            frame_ms,
                            counters.loaded_chunks,
                            counters.mesh_jobs_queued,
                            counters.lighting_dirty_chunks,
                            counters.storage_loads_inflight,
                            counters.active_sim_columns
                        ),
                    );
                }
                if self
                    .state
                    .as_ref()
                    .is_some_and(|state| state.should_report_stall(frame_ms))
                {
                    if let Some(state) = self.state.as_ref() {
                        let summary = state.format_stall_report(frame_ms);
                        crate::console_error!("{summary}");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn open_test_chat() -> App {
        let mut app = App::default();
        app.game_state = GameState::InGame;
        app.open_chat();
        app.suppress_next_chat_commit = false;
        app
    }

    #[test]
    fn chat_accepts_keyboard_text_without_ime_commit() {
        let mut app = open_test_chat();

        app.chat_insert_text("hello", ChatTextInputSource::Keyboard);

        assert_eq!(app.chat_input, "hello");
        assert_eq!(app.chat_input_cursor, "hello".len());
    }

    #[test]
    fn chat_dedups_matching_keyboard_and_ime_text() {
        let mut app = open_test_chat();

        app.chat_insert_text("a", ChatTextInputSource::Keyboard);
        app.chat_insert_text("a", ChatTextInputSource::Ime);

        assert_eq!(app.chat_input, "a");
    }
}
