use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use winit::keyboard::KeyCode;

use crate::config::render::{
    BASE_FOV_DEFAULT_DEG, BASE_FOV_MAX_DEG, BASE_FOV_MIN_DEG, GuiScale, RenderDistance,
};

const SETTINGS_DIR: &str = "save";
const SETTINGS_FILE_NAME: &str = "settings.json";

#[derive(Clone, Debug)]
pub(crate) struct AppSettings {
    pub(crate) gameplay: GameplaySettings,
    pub(crate) graphics: GraphicsSettings,
    pub(crate) controls: InputBindings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct GameplaySettings {
    pub(crate) view_bobbing: bool,
    /// Resting field of view in degrees; the sprint/flight kick multiplies off it.
    pub(crate) base_fov_deg: u32,
}

impl Default for GameplaySettings {
    fn default() -> Self {
        Self {
            view_bobbing: true,
            base_fov_deg: BASE_FOV_DEFAULT_DEG,
        }
    }
}

impl GameplaySettings {
    pub(crate) fn base_fov_radians(self) -> f32 {
        (self.base_fov_deg as f32).to_radians()
    }

    /// Clamps values that came from a hand-edited settings file.
    fn normalized(mut self) -> Self {
        self.base_fov_deg = self.base_fov_deg.clamp(BASE_FOV_MIN_DEG, BASE_FOV_MAX_DEG);
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct GraphicsSettings {
    pub(crate) render_distance: RenderDistance,
    pub(crate) vsync: bool,
    pub(crate) backface_culling: bool,
    pub(crate) gui_scale: GuiScale,
    /// Directory name under `resourcepacks/`; `None` uses the bundled assets.
    pub(crate) texture_pack: Option<String>,
}

impl Default for GraphicsSettings {
    fn default() -> Self {
        Self {
            render_distance: RenderDistance::default(),
            vsync: true,
            backface_culling: true,
            gui_scale: GuiScale::default(),
            texture_pack: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(crate) enum InputAction {
    MoveForward,
    MoveBackward,
    MoveLeft,
    MoveRight,
    Jump,
    Sneak,
    Sprint,
    OpenInventory,
    OpenChat,
    OpenTickingAreas,
    Screenshot,
    ToggleCameraMode,
    PauseMenu,
}

impl InputAction {
    pub(crate) const ALL: [Self; 13] = [
        Self::MoveForward,
        Self::MoveBackward,
        Self::MoveLeft,
        Self::MoveRight,
        Self::Jump,
        Self::Sneak,
        Self::Sprint,
        Self::OpenInventory,
        Self::OpenChat,
        Self::OpenTickingAreas,
        Self::Screenshot,
        Self::ToggleCameraMode,
        Self::PauseMenu,
    ];

    const fn index(self) -> usize {
        match self {
            Self::MoveForward => 0,
            Self::MoveBackward => 1,
            Self::MoveLeft => 2,
            Self::MoveRight => 3,
            Self::Jump => 4,
            Self::Sneak => 5,
            Self::Sprint => 6,
            Self::OpenInventory => 7,
            Self::OpenChat => 8,
            Self::OpenTickingAreas => 9,
            Self::Screenshot => 10,
            Self::ToggleCameraMode => 11,
            Self::PauseMenu => 12,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::MoveForward => "Move forward",
            Self::MoveBackward => "Move backward",
            Self::MoveLeft => "Move left",
            Self::MoveRight => "Move right",
            Self::Jump => "Jump",
            Self::Sneak => "Sneak",
            Self::Sprint => "Sprint",
            Self::OpenInventory => "Inventory",
            Self::OpenChat => "Chat",
            Self::OpenTickingAreas => "Ticking areas",
            Self::Screenshot => "Screenshot",
            Self::ToggleCameraMode => "Camera perspective",
            Self::PauseMenu => "Pause menu",
        }
    }

    pub(crate) const fn default_key(self) -> KeyCode {
        match self {
            Self::MoveForward => KeyCode::KeyW,
            Self::MoveBackward => KeyCode::KeyS,
            Self::MoveLeft => KeyCode::KeyA,
            Self::MoveRight => KeyCode::KeyD,
            Self::Jump => KeyCode::Space,
            Self::Sneak => KeyCode::ShiftLeft,
            Self::Sprint => KeyCode::ControlLeft,
            Self::OpenInventory => KeyCode::KeyB,
            Self::OpenChat => KeyCode::KeyT,
            Self::OpenTickingAreas => KeyCode::KeyK,
            Self::Screenshot => KeyCode::F2,
            Self::ToggleCameraMode => KeyCode::F5,
            Self::PauseMenu => KeyCode::Escape,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InputBindings {
    keys: [KeyCode; InputAction::ALL.len()],
}

impl Default for InputBindings {
    fn default() -> Self {
        let mut keys = [KeyCode::Escape; InputAction::ALL.len()];
        for action in InputAction::ALL {
            keys[action.index()] = action.default_key();
        }
        Self { keys }
    }
}

impl InputBindings {
    pub(crate) fn key_for(&self, action: InputAction) -> KeyCode {
        self.keys[action.index()]
    }

    pub(crate) fn action_for_key(&self, key: KeyCode) -> Option<InputAction> {
        InputAction::ALL
            .into_iter()
            .find(|action| self.key_for(*action) == key)
    }

    pub(crate) fn set_key_swapping(
        &mut self,
        action: InputAction,
        new_key: KeyCode,
    ) -> Option<InputAction> {
        let current_key = self.key_for(action);
        let conflict = self
            .action_for_key(new_key)
            .filter(|other| *other != action);
        self.keys[action.index()] = new_key;
        if let Some(other) = conflict {
            self.keys[other.index()] = current_key;
        }
        conflict
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct AppSettingsFile {
    gameplay: GameplaySettings,
    graphics: GraphicsSettings,
    controls: SavedInputBindings,
}

impl Default for AppSettingsFile {
    fn default() -> Self {
        Self {
            gameplay: GameplaySettings::default(),
            graphics: GraphicsSettings::default(),
            controls: SavedInputBindings::from_runtime(&InputBindings::default()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SavedInputBindings {
    move_forward: String,
    move_backward: String,
    move_left: String,
    move_right: String,
    jump: String,
    sneak: String,
    sprint: String,
    open_inventory: String,
    open_chat: String,
    open_ticking_areas: String,
    screenshot: String,
    toggle_camera_mode: String,
    pause_menu: String,
}

impl Default for SavedInputBindings {
    fn default() -> Self {
        Self::from_runtime(&InputBindings::default())
    }
}

impl SavedInputBindings {
    fn from_runtime(bindings: &InputBindings) -> Self {
        Self {
            move_forward: keycode_config_name(bindings.key_for(InputAction::MoveForward)),
            move_backward: keycode_config_name(bindings.key_for(InputAction::MoveBackward)),
            move_left: keycode_config_name(bindings.key_for(InputAction::MoveLeft)),
            move_right: keycode_config_name(bindings.key_for(InputAction::MoveRight)),
            jump: keycode_config_name(bindings.key_for(InputAction::Jump)),
            sneak: keycode_config_name(bindings.key_for(InputAction::Sneak)),
            sprint: keycode_config_name(bindings.key_for(InputAction::Sprint)),
            open_inventory: keycode_config_name(bindings.key_for(InputAction::OpenInventory)),
            open_chat: keycode_config_name(bindings.key_for(InputAction::OpenChat)),
            open_ticking_areas: keycode_config_name(
                bindings.key_for(InputAction::OpenTickingAreas),
            ),
            screenshot: keycode_config_name(bindings.key_for(InputAction::Screenshot)),
            toggle_camera_mode: keycode_config_name(
                bindings.key_for(InputAction::ToggleCameraMode),
            ),
            pause_menu: keycode_config_name(bindings.key_for(InputAction::PauseMenu)),
        }
    }

    fn into_runtime(self) -> InputBindings {
        let defaults = InputBindings::default();
        let mut bindings = defaults.clone();
        let entries = [
            (InputAction::MoveForward, self.move_forward),
            (InputAction::MoveBackward, self.move_backward),
            (InputAction::MoveLeft, self.move_left),
            (InputAction::MoveRight, self.move_right),
            (InputAction::Jump, self.jump),
            (InputAction::Sneak, self.sneak),
            (InputAction::Sprint, self.sprint),
            (InputAction::OpenInventory, self.open_inventory),
            (InputAction::OpenChat, self.open_chat),
            (InputAction::OpenTickingAreas, self.open_ticking_areas),
            (InputAction::Screenshot, self.screenshot),
            (InputAction::ToggleCameraMode, self.toggle_camera_mode),
            (InputAction::PauseMenu, self.pause_menu),
        ];
        for (action, raw) in entries {
            let key = parse_keycode_name(&raw).unwrap_or_else(|| defaults.key_for(action));
            bindings.set_key_swapping(action, key);
        }
        bindings
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        let file = AppSettingsFile::default();
        Self {
            gameplay: file.gameplay,
            graphics: file.graphics,
            controls: file.controls.into_runtime(),
        }
    }
}

/// Groups of settings that can be reset to their `Default` values as a unit.
/// Keeping the scopes here (instead of in the menu code) means new settings only
/// have to be added to the matching `Default` impl to take part in reset flows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SettingsScope {
    Gameplay,
    Graphics,
    Controls,
    All,
}

impl SettingsScope {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Gameplay => "gameplay settings",
            Self::Graphics => "graphics settings",
            Self::Controls => "controls",
            Self::All => "all settings",
        }
    }
}

impl AppSettings {
    pub(crate) fn is_default(&self, scope: SettingsScope) -> bool {
        match scope {
            SettingsScope::Gameplay => self.gameplay == GameplaySettings::default(),
            SettingsScope::Graphics => self.graphics == GraphicsSettings::default(),
            SettingsScope::Controls => self.controls == InputBindings::default(),
            SettingsScope::All => {
                self.is_default(SettingsScope::Gameplay)
                    && self.is_default(SettingsScope::Graphics)
                    && self.is_default(SettingsScope::Controls)
            }
        }
    }

    pub(crate) fn reset(&mut self, scope: SettingsScope) {
        match scope {
            SettingsScope::Gameplay => self.gameplay = GameplaySettings::default(),
            SettingsScope::Graphics => self.graphics = GraphicsSettings::default(),
            SettingsScope::Controls => self.controls = InputBindings::default(),
            SettingsScope::All => *self = Self::default(),
        }
    }
}

impl AppSettings {
    pub(crate) fn load() -> io::Result<Self> {
        let path = settings_file_path();
        match fs::read(&path) {
            Ok(bytes) => {
                let file: AppSettingsFile = serde_json::from_slice(&bytes).map_err(|err| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("failed to parse {}: {err}", path.display()),
                    )
                })?;
                Ok(Self {
                    gameplay: file.gameplay.normalized(),
                    graphics: file.graphics,
                    controls: file.controls.into_runtime(),
                })
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err),
        }
    }

    pub(crate) fn save(&self) -> io::Result<()> {
        let path = settings_file_path();
        let file = AppSettingsFile {
            gameplay: self.gameplay,
            graphics: self.graphics.clone(),
            controls: SavedInputBindings::from_runtime(&self.controls),
        };
        save_settings_atomic(&path, &file)
    }
}

pub(crate) fn settings_file_path() -> PathBuf {
    PathBuf::from(SETTINGS_DIR).join(SETTINGS_FILE_NAME)
}

fn save_settings_atomic(path: &Path, file: &AppSettingsFile) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut bytes = serde_json::to_vec_pretty(file)
        .map_err(|err| io::Error::other(format!("failed to serialize settings: {err}")))?;
    bytes.push(b'\n');
    let tmp_path = tmp_path_for(path)?;
    fs::write(&tmp_path, &bytes)?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

fn tmp_path_for(path: &Path) -> io::Result<PathBuf> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid settings path: {}", path.display()),
            )
        })?;
    Ok(path.with_file_name(format!("{file_name}.tmp")))
}

pub(crate) fn keycode_label(code: KeyCode) -> String {
    match code {
        KeyCode::KeyA => "A".to_string(),
        KeyCode::KeyB => "B".to_string(),
        KeyCode::KeyC => "C".to_string(),
        KeyCode::KeyD => "D".to_string(),
        KeyCode::KeyE => "E".to_string(),
        KeyCode::KeyF => "F".to_string(),
        KeyCode::KeyG => "G".to_string(),
        KeyCode::KeyH => "H".to_string(),
        KeyCode::KeyI => "I".to_string(),
        KeyCode::KeyJ => "J".to_string(),
        KeyCode::KeyK => "K".to_string(),
        KeyCode::KeyL => "L".to_string(),
        KeyCode::KeyM => "M".to_string(),
        KeyCode::KeyN => "N".to_string(),
        KeyCode::KeyO => "O".to_string(),
        KeyCode::KeyP => "P".to_string(),
        KeyCode::KeyQ => "Q".to_string(),
        KeyCode::KeyR => "R".to_string(),
        KeyCode::KeyS => "S".to_string(),
        KeyCode::KeyT => "T".to_string(),
        KeyCode::KeyU => "U".to_string(),
        KeyCode::KeyV => "V".to_string(),
        KeyCode::KeyW => "W".to_string(),
        KeyCode::KeyX => "X".to_string(),
        KeyCode::KeyY => "Y".to_string(),
        KeyCode::KeyZ => "Z".to_string(),
        KeyCode::Digit0 => "0".to_string(),
        KeyCode::Digit1 => "1".to_string(),
        KeyCode::Digit2 => "2".to_string(),
        KeyCode::Digit3 => "3".to_string(),
        KeyCode::Digit4 => "4".to_string(),
        KeyCode::Digit5 => "5".to_string(),
        KeyCode::Digit6 => "6".to_string(),
        KeyCode::Digit7 => "7".to_string(),
        KeyCode::Digit8 => "8".to_string(),
        KeyCode::Digit9 => "9".to_string(),
        KeyCode::ArrowUp => "Up Arrow".to_string(),
        KeyCode::ArrowDown => "Down Arrow".to_string(),
        KeyCode::ArrowLeft => "Left Arrow".to_string(),
        KeyCode::ArrowRight => "Right Arrow".to_string(),
        KeyCode::Space => "Space".to_string(),
        KeyCode::Escape => "Escape".to_string(),
        KeyCode::Enter => "Enter".to_string(),
        KeyCode::NumpadEnter => "Numpad Enter".to_string(),
        KeyCode::Tab => "Tab".to_string(),
        KeyCode::Backspace => "Backspace".to_string(),
        KeyCode::Delete => "Delete".to_string(),
        KeyCode::Home => "Home".to_string(),
        KeyCode::End => "End".to_string(),
        KeyCode::PageUp => "Page Up".to_string(),
        KeyCode::PageDown => "Page Down".to_string(),
        KeyCode::ShiftLeft => "Left Shift".to_string(),
        KeyCode::ShiftRight => "Right Shift".to_string(),
        KeyCode::ControlLeft => "Left Ctrl".to_string(),
        KeyCode::ControlRight => "Right Ctrl".to_string(),
        KeyCode::AltLeft => "Left Alt".to_string(),
        KeyCode::AltRight => "Right Alt".to_string(),
        KeyCode::BracketLeft => "[".to_string(),
        KeyCode::BracketRight => "]".to_string(),
        KeyCode::Minus => "-".to_string(),
        KeyCode::Equal => "=".to_string(),
        KeyCode::Comma => ",".to_string(),
        KeyCode::Period => ".".to_string(),
        KeyCode::Slash => "/".to_string(),
        KeyCode::Backslash => "\\".to_string(),
        KeyCode::Semicolon => ";".to_string(),
        KeyCode::Quote => "'".to_string(),
        KeyCode::Backquote => "`".to_string(),
        KeyCode::F1 => "F1".to_string(),
        KeyCode::F2 => "F2".to_string(),
        KeyCode::F3 => "F3".to_string(),
        KeyCode::F4 => "F4".to_string(),
        KeyCode::F5 => "F5".to_string(),
        KeyCode::F6 => "F6".to_string(),
        KeyCode::F7 => "F7".to_string(),
        KeyCode::F8 => "F8".to_string(),
        KeyCode::F9 => "F9".to_string(),
        KeyCode::F10 => "F10".to_string(),
        KeyCode::F11 => "F11".to_string(),
        KeyCode::F12 => "F12".to_string(),
        other => format!("{other:?}"),
    }
}

fn keycode_config_name(code: KeyCode) -> String {
    format!("{code:?}")
}

fn parse_keycode_name(raw: &str) -> Option<KeyCode> {
    Some(match raw {
        "KeyA" => KeyCode::KeyA,
        "KeyB" => KeyCode::KeyB,
        "KeyC" => KeyCode::KeyC,
        "KeyD" => KeyCode::KeyD,
        "KeyE" => KeyCode::KeyE,
        "KeyF" => KeyCode::KeyF,
        "KeyG" => KeyCode::KeyG,
        "KeyH" => KeyCode::KeyH,
        "KeyI" => KeyCode::KeyI,
        "KeyJ" => KeyCode::KeyJ,
        "KeyK" => KeyCode::KeyK,
        "KeyL" => KeyCode::KeyL,
        "KeyM" => KeyCode::KeyM,
        "KeyN" => KeyCode::KeyN,
        "KeyO" => KeyCode::KeyO,
        "KeyP" => KeyCode::KeyP,
        "KeyQ" => KeyCode::KeyQ,
        "KeyR" => KeyCode::KeyR,
        "KeyS" => KeyCode::KeyS,
        "KeyT" => KeyCode::KeyT,
        "KeyU" => KeyCode::KeyU,
        "KeyV" => KeyCode::KeyV,
        "KeyW" => KeyCode::KeyW,
        "KeyX" => KeyCode::KeyX,
        "KeyY" => KeyCode::KeyY,
        "KeyZ" => KeyCode::KeyZ,
        "Digit0" => KeyCode::Digit0,
        "Digit1" => KeyCode::Digit1,
        "Digit2" => KeyCode::Digit2,
        "Digit3" => KeyCode::Digit3,
        "Digit4" => KeyCode::Digit4,
        "Digit5" => KeyCode::Digit5,
        "Digit6" => KeyCode::Digit6,
        "Digit7" => KeyCode::Digit7,
        "Digit8" => KeyCode::Digit8,
        "Digit9" => KeyCode::Digit9,
        "ArrowUp" => KeyCode::ArrowUp,
        "ArrowDown" => KeyCode::ArrowDown,
        "ArrowLeft" => KeyCode::ArrowLeft,
        "ArrowRight" => KeyCode::ArrowRight,
        "Space" => KeyCode::Space,
        "Escape" => KeyCode::Escape,
        "Enter" => KeyCode::Enter,
        "NumpadEnter" => KeyCode::NumpadEnter,
        "Tab" => KeyCode::Tab,
        "Backspace" => KeyCode::Backspace,
        "Delete" => KeyCode::Delete,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        "ShiftLeft" => KeyCode::ShiftLeft,
        "ShiftRight" => KeyCode::ShiftRight,
        "ControlLeft" => KeyCode::ControlLeft,
        "ControlRight" => KeyCode::ControlRight,
        "AltLeft" => KeyCode::AltLeft,
        "AltRight" => KeyCode::AltRight,
        "BracketLeft" => KeyCode::BracketLeft,
        "BracketRight" => KeyCode::BracketRight,
        "Minus" => KeyCode::Minus,
        "Equal" => KeyCode::Equal,
        "Comma" => KeyCode::Comma,
        "Period" => KeyCode::Period,
        "Slash" => KeyCode::Slash,
        "Backslash" => KeyCode::Backslash,
        "Semicolon" => KeyCode::Semicolon,
        "Quote" => KeyCode::Quote,
        "Backquote" => KeyCode::Backquote,
        "F1" => KeyCode::F1,
        "F2" => KeyCode::F2,
        "F3" => KeyCode::F3,
        "F4" => KeyCode::F4,
        "F5" => KeyCode::F5,
        "F6" => KeyCode::F6,
        "F7" => KeyCode::F7,
        "F8" => KeyCode::F8,
        "F9" => KeyCode::F9,
        "F10" => KeyCode::F10,
        "F11" => KeyCode::F11,
        "F12" => KeyCode::F12,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::{GameplaySettings, InputAction, InputBindings};
    use crate::config::render::{BASE_FOV_MAX_DEG, BASE_FOV_MIN_DEG};

    #[test]
    fn clamps_hand_edited_base_fov() {
        let too_low = GameplaySettings {
            base_fov_deg: 1,
            ..GameplaySettings::default()
        }
        .normalized();
        assert_eq!(too_low.base_fov_deg, BASE_FOV_MIN_DEG);

        let too_high = GameplaySettings {
            base_fov_deg: 9000,
            ..GameplaySettings::default()
        }
        .normalized();
        assert_eq!(too_high.base_fov_deg, BASE_FOV_MAX_DEG);

        let in_range = GameplaySettings {
            base_fov_deg: 72,
            ..GameplaySettings::default()
        }
        .normalized();
        assert_eq!(in_range.base_fov_deg, 72);
    }

    #[test]
    fn default_base_fov_matches_camera_default() {
        let radians = GameplaySettings::default().base_fov_radians();
        assert!((radians - crate::camera::Camera::default().fov_y_radians).abs() < 1e-5);
    }

    #[test]
    fn rebinding_swaps_the_conflicting_action() {
        let mut bindings = InputBindings::default();
        let jump_key = bindings.key_for(InputAction::Jump);
        let sneak_key = bindings.key_for(InputAction::Sneak);

        let conflict = bindings.set_key_swapping(InputAction::Jump, sneak_key);
        assert_eq!(conflict, Some(InputAction::Sneak));
        assert_eq!(bindings.key_for(InputAction::Jump), sneak_key);
        assert_eq!(bindings.key_for(InputAction::Sneak), jump_key);
    }
}
