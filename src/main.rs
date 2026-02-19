mod texture;
mod texture_registry;
mod worldgen;

use bytemuck::{Pod, Zeroable};
use glam::{IVec3, Mat4, Vec3};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};
use texture::TextureArray;
use texture_registry::TextureRegistry;
use wgpu::SurfaceError;
use wgpu::util::DeviceExt;
use winit::{
    application::ApplicationHandler,
    event::{DeviceEvent, ElementState, KeyEvent, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{CursorGrabMode, Window, WindowId},
};
use worldgen::{DEFAULT_WORLDGEN_PRESET, TerrainSampler, WorldGenConfig, WorldgenPreset};

// glam::perspective_rh erzeugt OpenGL-Clipspace-Z in [-1..1].
// wgpu erwartet Z in [0..1], daher explizite Konvertierung.
const OPENGL_TO_WGPU_MATRIX: Mat4 = Mat4::from_cols_array(&[
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.5, 1.0,
]);

/// ============================
/// Kamera (Position + yaw/pitch)
/// ============================
/// - yaw: links/rechts um Welt-Y-Achse
/// - pitch: hoch/runter um Kamera-X-Achse
///
/// Welt-Konvention:
/// - +Y = oben
/// - Wir starten so, dass "vorwärts" zunächst in -Z zeigt (typisch für RH setups)
struct Camera {
    position: Vec3,
    yaw: f32,
    pitch: f32,

    fov_y_radians: f32,
    z_near: f32,
    z_far: f32,
}

impl Camera {
    fn new(position: Vec3) -> Self {
        Self {
            position,
            yaw: (-90.0_f32).to_radians(),
            pitch: 0.0,
            fov_y_radians: 70.0_f32.to_radians(),
            z_near: 0.1,
            z_far: 500.0,
        }
    }

    /// Vorwärtsrichtung aus yaw/pitch.
    /// Das ist die Blickrichtung der Kamera.
    fn forward(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        Vec3::new(cy * cp, sp, sy * cp).normalize()
    }

    /// Rechtsrichtung relativ zur Blickrichtung
    fn right(&self) -> Vec3 {
        // RH: right = forward x world_up
        self.forward().cross(Vec3::Y).normalize()
    }

    /// Vorwärtsrichtung nur auf der XZ-Ebene (Pitch ignoriert).
    fn forward_flat(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        Vec3::new(cy, 0.0, sy).normalize()
    }

    /// Rechtsrichtung nur auf der XZ-Ebene (Pitch ignoriert).
    fn right_flat(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        Vec3::new(-sy, 0.0, cy).normalize()
    }

    fn view_matrix(&self) -> Mat4 {
        Mat4::look_at_rh(self.position, self.position + self.forward(), Vec3::Y)
    }

    fn proj_matrix(&self, aspect: f32) -> Mat4 {
        Mat4::perspective_rh(self.fov_y_radians, aspect, self.z_near, self.z_far)
    }
}

impl Default for Camera {
    fn default() -> Self {
        Camera::new(Vec3::new(128.0, 60.0, 220.0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MoveMode {
    Flight,
    Walking,
}

/// Player-Transform in Weltkoordinaten.
/// `feet_position` beschreibt die Unterkante des Players.
#[derive(Clone, Copy, Debug)]
struct Player {
    feet_position: Vec3,
    mode: MoveMode,
    _velocity_y: f32,
    on_ground: bool,
}

impl Player {
    fn from_eye_position(eye_position: Vec3) -> Self {
        Self {
            feet_position: eye_position - Vec3::Y * EYE_HEIGHT,
            mode: MoveMode::Flight,
            _velocity_y: 0.0,
            on_ground: false,
        }
    }

    fn eye_position(self) -> Vec3 {
        self.feet_position + Vec3::Y * EYE_HEIGHT
    }
}

/// =======================================
/// Input / Controller (WASD + Maus + ESC)
/// =======================================
/// Hier passiert:
/// - Flight-WASD bewegt auf XZ (yaw-basiert, pitch-unabhängig)
/// - Space/Shift bewegt entlang Welt-Y
/// - Maus-Deltas verändern yaw/pitch
struct CameraController {
    move_forward: bool,
    move_backward: bool,
    move_left: bool,
    move_right: bool,
    move_up: bool,
    move_down: bool,

    jump_requested: bool,
    mouse_sens: f32,

    mouse_captured: bool,
}

impl Default for CameraController {
    fn default() -> Self {
        Self::new()
    }
}

impl CameraController {
    fn new() -> Self {
        Self {
            move_forward: false,
            move_backward: false,
            move_left: false,
            move_right: false,
            move_up: false,
            move_down: false,
            jump_requested: false,
            mouse_sens: 0.002,
            mouse_captured: false,
        }
    }

    fn process_key(&mut self, event: &KeyEvent) {
        let pressed = event.state == ElementState::Pressed;
        let PhysicalKey::Code(code) = event.physical_key else {
            return;
        };

        match code {
            KeyCode::KeyW => self.move_forward = pressed,
            KeyCode::KeyS => self.move_backward = pressed,
            KeyCode::KeyA => self.move_left = pressed,
            KeyCode::KeyD => self.move_right = pressed,
            KeyCode::Space => self.move_up = pressed,
            KeyCode::ShiftLeft | KeyCode::ShiftRight => self.move_down = pressed,
            _ => {}
        }
    }

    /// Mausbewegung (raw delta) -> yaw/pitch
    fn process_mouse_motion(&mut self, camera: &mut Camera, dx: f64, dy: f64) {
        if !self.mouse_captured {
            return;
        }

        camera.yaw += (dx as f32) * self.mouse_sens;
        camera.pitch -= (dy as f32) * self.mouse_sens;

        // Clamp pitch, damit du nicht über Kopf flipst.
        let limit = 89.0_f32.to_radians();
        camera.pitch = camera.pitch.clamp(-limit, limit);
    }

    fn horizontal_move_input(&self, camera: &Camera) -> Vec3 {
        let forward_flat = camera.forward_flat();
        let right_flat = camera.right_flat();
        let mut horizontal = Vec3::ZERO;
        if self.move_forward {
            horizontal += forward_flat;
        }
        if self.move_backward {
            horizontal -= forward_flat;
        }
        if self.move_right {
            horizontal += right_flat;
        }
        if self.move_left {
            horizontal -= right_flat;
        }
        horizontal
    }

    /// Liefert Flight-Delta pro Frame (dt in Sekunden):
    /// - WASD bewegt nur auf XZ (Pitch-unabhängig)
    /// - Space/Shift steuern Höhe entlang Welt-Y
    fn flight_move_delta(&self, camera: &Camera, dt: f32) -> Vec3 {
        let horizontal_input = self.horizontal_move_input(camera);
        let up = Vec3::Y;

        let mut vertical_input = Vec3::ZERO;
        if self.move_up {
            vertical_input += up;
        }
        if self.move_down {
            vertical_input -= up;
        }

        let desired_move_dir = horizontal_input + vertical_input;
        if desired_move_dir.length_squared() > 0.0 {
            desired_move_dir.normalize() * FLIGHT_SPEED * dt
        } else {
            Vec3::ZERO
        }
    }

    fn consume_jump_requested(&mut self) -> bool {
        let requested = self.jump_requested;
        self.jump_requested = false;
        requested
    }

    fn request_jump(&mut self) {
        self.jump_requested = true;
    }

    fn clear_jump_requested(&mut self) {
        self.jump_requested = false;
    }

    /// Maus grabben/ungrabben (FPS Look)
    fn set_mouse_capture(&mut self, window: &Window, capture: bool) {
        self.mouse_captured = capture;

        if capture {
            window.set_cursor_visible(false);
            let _ = window
                .set_cursor_grab(CursorGrabMode::Locked)
                .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined));
        } else {
            let _ = window.set_cursor_grab(CursorGrabMode::None);
            window.set_cursor_visible(true);
        }
    }
}

fn hotbar_slot_for_key(code: KeyCode) -> Option<usize> {
    match code {
        KeyCode::Digit1 => Some(0),
        KeyCode::Digit2 => Some(1),
        KeyCode::Digit3 => Some(2),
        KeyCode::Digit4 => Some(3),
        KeyCode::Digit5 => Some(4),
        KeyCode::Digit6 => Some(5),
        KeyCode::Digit7 => Some(6),
        KeyCode::Digit8 => Some(7),
        KeyCode::Digit9 => Some(8),
        _ => None,
    }
}

/// =======================================
/// GPU-Datenstrukturen: Vertex + Uniform
/// =======================================

/// Vertex-Layout für den Würfel:
/// - position: 3 floats
/// - uv: 2 floats
/// - tex_layer: layer im Texture-Array (pro Face)
/// - color: 3 floats
/// - tint_kind: 0 = kein Tint, 1 = Grass-Top-Tint
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct Vertex {
    position: [f32; 3],
    uv: [f32; 2],
    tex_layer: u32,
    color: [f32; 3],
    tint_kind: u32,
}

impl Vertex {
    fn layout() -> wgpu::VertexBufferLayout<'static> {
        use std::mem;
        wgpu::VertexBufferLayout {
            array_stride: mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                // location 0: position
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x3,
                },
                // location 1: uv
                wgpu::VertexAttribute {
                    offset: mem::size_of::<[f32; 3]>() as wgpu::BufferAddress,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x2,
                },
                // location 2: texture layer index
                wgpu::VertexAttribute {
                    offset: (mem::size_of::<[f32; 3]>() + mem::size_of::<[f32; 2]>())
                        as wgpu::BufferAddress,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Uint32,
                },
                // location 3: color
                wgpu::VertexAttribute {
                    offset: (mem::size_of::<[f32; 3]>()
                        + mem::size_of::<[f32; 2]>()
                        + mem::size_of::<u32>()) as wgpu::BufferAddress,
                    shader_location: 3,
                    format: wgpu::VertexFormat::Float32x3,
                },
                // location 4: tint kind
                wgpu::VertexAttribute {
                    offset: (mem::size_of::<[f32; 3]>()
                        + mem::size_of::<[f32; 2]>()
                        + mem::size_of::<u32>()
                        + mem::size_of::<[f32; 3]>())
                        as wgpu::BufferAddress,
                    shader_location: 4,
                    format: wgpu::VertexFormat::Uint32,
                },
            ],
        }
    }
}

const CHUNK_SIZE: i32 = 16;
const WORLD_XZ: i32 = 256;
const WORLD_MAX_Y: i32 = 64;
const WORLD_GRASS_Y: i32 = 43;
const WORLD_CHUNKS_XZ: i32 = WORLD_XZ / CHUNK_SIZE;
const WORLD_HEIGHT: i32 = WORLD_MAX_Y + 1;
const USE_GREEDY_MESHING: bool = true;
const RAYCAST_MAX_DISTANCE: f32 = 8.0;
const HIGHLIGHT_FACE_OFFSET: f32 = 0.0015;
const HIGHLIGHT_BLINK_MS: u128 = 250;
const CROSSHAIR_THICKNESS_PX: f32 = 4.0;
const CROSSHAIR_HALF_LEN_PX: f32 = 14.0;
const PREVIEW_SIZE_PX: f32 = 44.0;
const PREVIEW_MARGIN_TOP_PX: f32 = 20.0;
const PREVIEW_MARGIN_RIGHT_PX: f32 = 20.0;
const PREVIEW_ACCENT_STRIPE_PX: f32 = 10.0;
const PREVIEW_FLIP_HORIZONTAL: bool = true;
const PREVIEW_FLIP_VERTICAL: bool = false;
const FOG_START_DISTANCE: f32 = 40.0;
const FOG_END_DISTANCE: f32 = 180.0;
const FOG_DARKEN_STRENGTH: f32 = 0.22;
const PLAYER_RADIUS: f32 = 0.3;
const PLAYER_HEIGHT: f32 = 1.8;
const EYE_HEIGHT: f32 = 1.62;
const COLLISION_EPS: f32 = 1e-4;
const COLLISION_SUBSTEP: f32 = 0.1;
const GRAVITY: f32 = 32.0;
const JUMP_HEIGHT: f32 = 1.1;
const SPEED_MULT: f32 = 1.10;
const WALK_SPEED: f32 = 4.3 * SPEED_MULT;
const FLIGHT_SPEED: f32 = 10.0 * SPEED_MULT;
const DOUBLE_SPACE_WINDOW: f32 = 0.20;
const GROUND_CHECK_DISTANCE: f32 = 0.08;
const WORLDGEN_SEED: u32 = 132_211;
const DEFAULT_PRESET: WorldgenPreset = DEFAULT_WORLDGEN_PRESET;
const ASSET_ROOT: &str = "assets/minecraft";
const TEX_STONE: &str = "minecraft:textures/block/stone.png";
const TEX_GRASS_TOP: &str = "minecraft:textures/block/grass_block_top.png";
const TEX_GRASS_SIDE: &str = "minecraft:textures/block/grass_block_side.png";
const TEX_DIRT: &str = "minecraft:textures/block/dirt.png";
const TEX_SAPLING: &str = "minecraft:textures/block/oak_sapling.png";
const TEX_OAK_PLANKS: &str = "minecraft:textures/block/oak_planks.png";
const REQUIRED_FACE_TEXTURE_KEYS: [&str; 5] = [
    TEX_GRASS_TOP,
    TEX_GRASS_SIDE,
    TEX_DIRT,
    TEX_OAK_PLANKS,
    TEX_SAPLING,
];
const TEXTURE_SELF_TEST_KEYS: [&str; 3] = [
    "minecraft:textures/block/acacia_log.png",
    "minecraft:textures/item/apple.png",
    "minecraft:textures/block/torch.png",
];
const PARTICLE_SPAWN_COUNT: usize = 10;
const PARTICLE_MAX_COUNT: usize = 2000;
const PARTICLE_QUAD_SIZE: f32 = 0.11;
const PARTICLE_GRAVITY: f32 = 12.0;
const PARTICLE_SPEED_MIN: f32 = 1.8;
const PARTICLE_SPEED_MAX: f32 = 4.6;
const PARTICLE_LIFE_MIN: f32 = 0.35;
const PARTICLE_LIFE_MAX: f32 = 0.80;
const TINT_NONE: u32 = 0;
const TINT_GRASS_TOP: u32 = 1;
const UI_MODE_SOLID: u32 = 0;
const UI_MODE_TEXTURED: u32 = 1;
const SAVE_PATH: &str = "save/world.bin";
const SAVE_MAGIC: [u8; 4] = *b"MCW0";
const SAVE_VERSION: u16 = 13;

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartupWorldMode {
    LoadIfPresentElseGenerate,
    AlwaysRegenerate,
}

const STARTUP_WORLD_MODE: StartupWorldMode = StartupWorldMode::LoadIfPresentElseGenerate;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
enum BlockId {
    Air = 0,
    Grass = 1,
    Stone = 2,
    Sapling = 3,
    Dirt = 4,
    OakPlanks = 5,
}

const HOTBAR: [BlockId; 9] = [
    BlockId::Stone,
    BlockId::Dirt,
    BlockId::Grass,
    BlockId::OakPlanks,
    BlockId::Dirt,
    BlockId::Sapling,
    BlockId::Grass,
    BlockId::Stone,
    BlockId::Dirt,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Face {
    Up,
    Down,
    North,
    South,
    East,
    West,
}

type TextureKey = &'static str;

impl Face {
    const fn as_usize(self) -> usize {
        match self {
            Face::Up => 0,
            Face::Down => 1,
            Face::North => 2,
            Face::South => 3,
            Face::East => 4,
            Face::West => 5,
        }
    }

    const fn normal_i32(self) -> [i32; 3] {
        match self {
            Face::Up => [0, 1, 0],
            Face::Down => [0, -1, 0],
            Face::North => [0, 0, -1],
            Face::South => [0, 0, 1],
            Face::East => [1, 0, 0],
            Face::West => [-1, 0, 0],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockModel {
    Cube,
    CrossPlant,
}

#[derive(Clone, Copy, Debug)]
struct BlockDef {
    model: BlockModel,
    textures: [TextureKey; 6],
    plant_texture: Option<TextureKey>,
    solid: bool,
}

impl BlockDef {
    const fn cube_all(key: TextureKey, solid: bool) -> Self {
        Self {
            model: BlockModel::Cube,
            textures: [key; 6],
            plant_texture: None,
            solid,
        }
    }

    const fn grass(top: TextureKey, side: TextureKey, bottom: TextureKey, solid: bool) -> Self {
        let mut textures = [side; 6];
        textures[Face::Up.as_usize()] = top;
        textures[Face::Down.as_usize()] = bottom;
        Self {
            model: BlockModel::Cube,
            textures,
            plant_texture: None,
            solid,
        }
    }

    const fn plant_cross(key: TextureKey) -> Self {
        Self {
            model: BlockModel::CrossPlant,
            textures: [key; 6],
            plant_texture: Some(key),
            solid: false,
        }
    }

    fn texture_for(&self, face: Face) -> TextureKey {
        match self.model {
            BlockModel::Cube => self.textures[face.as_usize()],
            BlockModel::CrossPlant => self
                .plant_texture
                .expect("CrossPlant BlockDef missing plant texture"),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct BlockTextureLayers {
    up: u32,
    down: u32,
    north: u32,
    south: u32,
    east: u32,
    west: u32,
}

impl BlockTextureLayers {
    fn layer_for_face(self, face: Face) -> u32 {
        match face {
            Face::Up => self.up,
            Face::Down => self.down,
            Face::North => self.north,
            Face::South => self.south,
            Face::East => self.east,
            Face::West => self.west,
        }
    }
}

/// BlockRegistry:
/// - zentraler Ort für Blockdefinitionen
/// - jeder Block bekommt Texturen pro Face
struct BlockRegistry {
    defs: HashMap<BlockId, BlockDef>,
}

impl BlockRegistry {
    fn new() -> Self {
        let mut defs = HashMap::new();

        // Stone: überall gleiche Textur.
        defs.insert(BlockId::Stone, BlockDef::cube_all(TEX_STONE, true));
        defs.insert(BlockId::Dirt, BlockDef::cube_all(TEX_DIRT, true));

        // Grass: oben grass_top, unten dirt, Seiten grass_side.
        defs.insert(
            BlockId::Grass,
            BlockDef::grass(TEX_GRASS_TOP, TEX_GRASS_SIDE, TEX_DIRT, true),
        );

        defs.insert(BlockId::OakPlanks, BlockDef::cube_all(TEX_OAK_PLANKS, true));

        defs.insert(BlockId::Sapling, BlockDef::plant_cross(TEX_SAPLING));

        Self { defs }
    }

    fn def(&self, block: BlockId) -> BlockDef {
        *self
            .defs
            .get(&block)
            .unwrap_or_else(|| panic!("missing BlockDef registration for {:?}", block))
    }

    fn texture_key_for(&self, block: BlockId, face: Face) -> TextureKey {
        self.def(block).texture_for(face)
    }

    fn model_for(&self, block: BlockId) -> BlockModel {
        self.def(block).model
    }

    fn solid_for(&self, block: BlockId) -> bool {
        self.def(block).solid
    }

    fn collect_texture_keys(&self) -> Vec<String> {
        let mut keys: HashSet<String> = HashSet::new();
        for def in self.defs.values() {
            match def.model {
                BlockModel::Cube => {
                    for key in def.textures {
                        keys.insert(key.to_string());
                    }
                }
                BlockModel::CrossPlant => {
                    if let Some(key) = def.plant_texture {
                        keys.insert(key.to_string());
                    }
                }
            }
        }
        let mut collected: Vec<String> = keys.into_iter().collect();
        collected.sort();
        collected
    }
}

#[derive(Clone, Copy, Debug)]
struct BlockTextureInfo {
    model: BlockModel,
    cube_layers: BlockTextureLayers,
    plant_layer: Option<u32>,
    solid: bool,
}

struct BlockTextureLookup {
    by_block: HashMap<BlockId, BlockTextureInfo>,
    missing_layer: u32,
}

impl BlockTextureLookup {
    fn new(
        block_registry: &BlockRegistry,
        key_to_layer: &HashMap<String, u32>,
        missing_layer: u32,
    ) -> Self {
        let mut warned_missing_keys: HashSet<&'static str> = HashSet::new();

        let mut by_block = HashMap::new();
        for (&block, &textures) in &block_registry.defs {
            let mut resolve = |key: &'static str| -> u32 {
                if let Some(layer) = key_to_layer.get(key).copied() {
                    layer
                } else {
                    if warned_missing_keys.insert(key) {
                        eprintln!(
                            "[warn] missing texture key '{}' in atlas mapping; using missing-texture layer",
                            key
                        );
                    }
                    missing_layer
                }
            };
            let cube_layers = BlockTextureLayers {
                up: resolve(textures.texture_for(Face::Up)),
                down: resolve(textures.texture_for(Face::Down)),
                north: resolve(textures.texture_for(Face::North)),
                south: resolve(textures.texture_for(Face::South)),
                east: resolve(textures.texture_for(Face::East)),
                west: resolve(textures.texture_for(Face::West)),
            };
            let plant_layer = textures.plant_texture.map(resolve);
            by_block.insert(
                block,
                BlockTextureInfo {
                    model: textures.model,
                    cube_layers,
                    plant_layer,
                    solid: textures.solid,
                },
            );
        }

        Self {
            by_block,
            missing_layer,
        }
    }

    fn layer_for(&self, block: BlockId, face: Face) -> u32 {
        self.by_block
            .get(&block)
            .copied()
            .map_or(self.missing_layer, |info| match info.model {
                BlockModel::Cube => info.cube_layers.layer_for_face(face),
                BlockModel::CrossPlant => info.plant_layer.unwrap_or(self.missing_layer),
            })
    }

    fn model_for(&self, block: BlockId) -> BlockModel {
        self.by_block
            .get(&block)
            .map_or(BlockModel::Cube, |info| info.model)
    }

    fn is_solid_for(&self, block: BlockId) -> bool {
        self.by_block.get(&block).is_some_and(|info| info.solid)
    }

    fn plant_layer_for(&self, block: BlockId) -> Option<u32> {
        self.by_block.get(&block).and_then(|info| info.plant_layer)
    }
}

/// Erzeugt eine auffällige Magenta/Schwarz-Checker-Textur als Missing-Texture-Layer.
fn make_missing_texture_rgba(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = vec![0_u8; (width as usize) * (height as usize) * 4];
    let check = 4_u32;
    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            let odd = ((x / check) + (y / check)) % 2 == 0;
            let (r, g, b) = if odd {
                (255_u8, 0_u8, 255_u8)
            } else {
                (0_u8, 0_u8, 0_u8)
            };
            rgba[idx] = r;
            rgba[idx + 1] = g;
            rgba[idx + 2] = b;
            rgba[idx + 3] = 255;
        }
    }
    rgba
}

/// UV-Basis pro Face:
/// - U läuft nach rechts, wenn man von außen auf die Face schaut.
/// - V läuft nach unten (V=0 ist die obere Texturkante).
fn face_uv_basis(face: Face) -> ([i32; 3], [i32; 3]) {
    match face {
        Face::North => ([1, 0, 0], [0, -1, 0]),
        Face::South => ([-1, 0, 0], [0, -1, 0]),
        Face::West => ([0, 0, 1], [0, -1, 0]),
        Face::East => ([0, 0, -1], [0, -1, 0]),
        Face::Up => ([1, 0, 0], [0, 0, 1]),
        Face::Down => ([1, 0, 0], [0, 0, -1]),
    }
}

fn project_axis(p: [i32; 3], axis: [i32; 3]) -> i32 {
    p[0] * axis[0] + p[1] * axis[1] + p[2] * axis[2]
}

/// Berechnet UVs aus der Face-Basis für beliebige Quad-Ecken.
/// Damit bleiben Side-Texturen auf North/South/East/West konsistent orientiert
/// und Greedy-Quads behalten Tiling über die UV-Range 0..N.
fn face_uvs_for_quad(face: Face, corners: [[i32; 3]; 4]) -> [[f32; 2]; 4] {
    let (u_axis, v_axis) = face_uv_basis(face);
    let mut uvs = [[0.0; 2]; 4];
    let mut projected = [(0_i32, 0_i32); 4];
    let mut u_min = i32::MAX;
    let mut v_min = i32::MAX;

    for (i, corner) in corners.iter().enumerate() {
        let u = project_axis(*corner, u_axis);
        let v = project_axis(*corner, v_axis);
        projected[i] = (u, v);
        u_min = u_min.min(u);
        v_min = v_min.min(v);
    }

    for (i, (u, v)) in projected.iter().enumerate() {
        uvs[i] = [(*u - u_min) as f32, (*v - v_min) as f32];
    }

    uvs
}

fn tint_kind_for_face(block: BlockId, face: Face) -> u32 {
    if block == BlockId::Grass && face == Face::Up {
        TINT_GRASS_TOP
    } else {
        TINT_NONE
    }
}

impl BlockId {
    /// Physische Solidität (Collision).
    fn is_solid(self) -> bool {
        matches!(
            self,
            BlockId::Grass | BlockId::Stone | BlockId::Dirt | BlockId::OakPlanks
        )
    }

    /// Ob der Block als Voxel-Zelle vorhanden ist (Meshing/Raycast).
    fn is_voxel_filled(self) -> bool {
        self != BlockId::Air
    }

    fn color(self) -> [f32; 3] {
        match self {
            BlockId::Air => [0.0, 0.0, 0.0], // wird nicht gerendert
            BlockId::Grass => [0.35, 0.75, 0.25],
            BlockId::Stone => [0.55, 0.55, 0.58],
            BlockId::Sapling => [0.28, 0.88, 0.36],
            BlockId::Dirt => [0.49, 0.34, 0.20],
            BlockId::OakPlanks => [0.68, 0.56, 0.34],
        }
    }

    fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(BlockId::Air),
            1 => Some(BlockId::Grass),
            2 => Some(BlockId::Stone),
            3 => Some(BlockId::Sapling),
            4 => Some(BlockId::Dirt),
            5 => Some(BlockId::OakPlanks),
            _ => None,
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            BlockId::Air => "Air",
            BlockId::Grass => "Grass",
            BlockId::Stone => "Stone",
            BlockId::Sapling => "Sapling",
            BlockId::Dirt => "Dirt",
            BlockId::OakPlanks => "Oak Planks",
        }
    }
}

impl Default for BlockId {
    fn default() -> Self {
        BlockId::Stone
    }
}

struct Chunk {
    blocks: Vec<BlockId>,
}

impl Chunk {
    fn new_empty() -> Self {
        Self {
            blocks: vec![BlockId::Air; (CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE) as usize],
        }
    }

    fn index(x: i32, y: i32, z: i32) -> usize {
        // Flatten: x + z*SIZE + y*SIZE*SIZE
        (x + z * CHUNK_SIZE + y * CHUNK_SIZE * CHUNK_SIZE) as usize
    }

    fn in_bounds(x: i32, y: i32, z: i32) -> bool {
        (0..CHUNK_SIZE).contains(&x) && (0..CHUNK_SIZE).contains(&y) && (0..CHUNK_SIZE).contains(&z)
    }

    fn get(&self, x: i32, y: i32, z: i32) -> BlockId {
        if Self::in_bounds(x, y, z) {
            self.blocks[Self::index(x, y, z)]
        } else {
            // Außerhalb des Chunks behandeln wir als Air (damit Außenflächen sichtbar werden)
            BlockId::Air
        }
    }

    fn set(&mut self, x: i32, y: i32, z: i32, id: BlockId) {
        if Self::in_bounds(x, y, z) {
            let i = Self::index(x, y, z);
            self.blocks[i] = id;
        }
    }

    fn is_empty(&self) -> bool {
        self.blocks.iter().all(|&b| !b.is_voxel_filled())
    }
}

#[derive(Hash, Eq, PartialEq, Copy, Clone, Debug)]
struct ChunkCoord {
    x: i32,
    y: i32,
    z: i32,
}

type LocalCoord = IVec3;

struct World {
    chunks: HashMap<ChunkCoord, Chunk>,
    terrain: TerrainSampler,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetBlockError {
    OutOfBounds,
}

impl World {
    fn new() -> Self {
        let cfg = WorldGenConfig::from_preset(WORLDGEN_SEED, DEFAULT_PRESET);
        Self {
            chunks: HashMap::new(),
            terrain: TerrainSampler::new(cfg),
        }
    }

    /// Harte World Border:
    /// - X/Z nur in [0..WORLD_XZ-1]
    /// - Y nur in [0..WORLD_MAX_Y]
    fn in_bounds(world_pos: IVec3) -> bool {
        (0..WORLD_XZ).contains(&world_pos.x)
            && (0..WORLD_XZ).contains(&world_pos.z)
            && (0..=WORLD_MAX_Y).contains(&world_pos.y)
    }

    /// Mappt eine Welt-Blockposition auf die Chunk-Koordinate.
    /// `div_euclid` ist wichtig, damit negative Koordinaten korrekt in Chunks fallen.
    fn world_to_chunk_coord(world_pos: IVec3) -> ChunkCoord {
        ChunkCoord {
            x: world_pos.x.div_euclid(CHUNK_SIZE),
            y: world_pos.y.div_euclid(CHUNK_SIZE),
            z: world_pos.z.div_euclid(CHUNK_SIZE),
        }
    }

    /// Mappt eine Welt-Blockposition auf lokale Chunk-Koordinaten im Bereich [0..CHUNK_SIZE-1].
    /// `rem_euclid` liefert auch für negative Weltkoordinaten positive lokale Offsets.
    fn world_to_local(world_pos: IVec3) -> LocalCoord {
        IVec3::new(
            world_pos.x.rem_euclid(CHUNK_SIZE),
            world_pos.y.rem_euclid(CHUNK_SIZE),
            world_pos.z.rem_euclid(CHUNK_SIZE),
        )
    }

    /// Liest einen Block an Weltkoordinate; fehlt der Chunk, behandeln wir ihn als Air.
    fn get_block(&self, world_pos: IVec3) -> BlockId {
        if !Self::in_bounds(world_pos) {
            return BlockId::Air;
        }

        let chunk_coord = Self::world_to_chunk_coord(world_pos);
        let local = Self::world_to_local(world_pos);

        self.chunks
            .get(&chunk_coord)
            .map_or(BlockId::Air, |chunk| chunk.get(local.x, local.y, local.z))
    }

    /// Collision-Query:
    /// - Out-of-bounds ist SOLID, damit die World-Border physisch blockt.
    fn is_solid_block_at(&self, wx: i32, wy: i32, wz: i32) -> bool {
        let pos = IVec3::new(wx, wy, wz);
        if !Self::in_bounds(pos) {
            return true;
        }
        self.get_block(pos).is_solid()
    }

    /// Schreibt einen Block an Weltkoordinate; fehlende Chunks werden bei Bedarf erzeugt.
    /// Außerhalb der Border wird nichts geändert und ein Fehler zurückgegeben.
    fn set_block(&mut self, world_pos: IVec3, id: BlockId) -> Result<(), SetBlockError> {
        if !Self::in_bounds(world_pos) {
            return Err(SetBlockError::OutOfBounds);
        }

        let chunk_coord = Self::world_to_chunk_coord(world_pos);
        let local = Self::world_to_local(world_pos);

        if id == BlockId::Air {
            let mut remove_chunk = false;
            if let Some(chunk) = self.chunks.get_mut(&chunk_coord) {
                chunk.set(local.x, local.y, local.z, id);
                remove_chunk = chunk.is_empty();
            }
            if remove_chunk {
                self.chunks.remove(&chunk_coord);
            }
            return Ok(());
        }

        let chunk = self
            .chunks
            .entry(chunk_coord)
            .or_insert_with(Chunk::new_empty);
        chunk.set(local.x, local.y, local.z, id);
        Ok(())
    }

    fn terrain_height_at(&self, world_x: i32, world_z: i32) -> i32 {
        self.terrain.height_at(world_x, world_z, WORLD_MAX_Y)
    }

    fn height_index(x: i32, z: i32) -> usize {
        (z * WORLD_XZ + x) as usize
    }

    fn smooth_heightmap(heights: &[i32], passes: usize) -> Vec<i32> {
        let mut current = heights.to_vec();
        for _ in 0..passes {
            let mut next = current.clone();
            for z in 0..WORLD_XZ {
                for x in 0..WORLD_XZ {
                    let mut sum = 0_i32;
                    let mut count = 0_i32;
                    for dz in -1..=1 {
                        for dx in -1..=1 {
                            let sx = (x + dx).clamp(0, WORLD_XZ - 1);
                            let sz = (z + dz).clamp(0, WORLD_XZ - 1);
                            sum += current[Self::height_index(sx, sz)];
                            count += 1;
                        }
                    }
                    let avg = (sum as f32) / (count as f32);
                    let idx = Self::height_index(x, z);
                    // Mildes Smoothing gegen 1-Block-Zacken, ohne große Hügel plattzumachen.
                    let current_h = current[idx] as f32;
                    let blend = if (current_h - avg).abs() > 3.0 {
                        0.08
                    } else {
                        0.15
                    };
                    next[idx] = (current_h * (1.0 - blend) + avg * blend).round() as i32;
                }
            }
            current = next;
        }
        current
    }

    /// Noise-World:
    /// - y > surface: Air
    /// - y == surface: Grass
    /// - y in [surface-dirt_depth, surface): Dirt
    /// - darunter: Stone
    fn generate_noise_world(&mut self) {
        self.chunks.clear();
        let dirt_depth = self.terrain.config().dirt_depth.max(1);
        let mut raw_heights = vec![0_i32; (WORLD_XZ * WORLD_XZ) as usize];

        for z in 0..WORLD_XZ {
            for x in 0..WORLD_XZ {
                // Wichtig gegen Chunk-Seams: ausschließlich globale Weltkoordinaten verwenden.
                raw_heights[Self::height_index(x, z)] =
                    self.terrain_height_at(x, z).clamp(0, WORLD_MAX_Y);
            }
        }
        // Ein leichter Smoothing-Pass reduziert 1-Block-Stufen sichtbar,
        // ohne die übergeordneten Hügelformen zu verlieren.
        let smoothed_heights = Self::smooth_heightmap(&raw_heights, 1);

        let mut min_height = i32::MAX;
        let mut max_height = i32::MIN;
        for z in 0..WORLD_XZ {
            for x in 0..WORLD_XZ {
                let surface_y = smoothed_heights[Self::height_index(x, z)].clamp(0, WORLD_MAX_Y);
                min_height = min_height.min(surface_y);
                max_height = max_height.max(surface_y);
                for y in 0..=surface_y {
                    let id = if y == surface_y {
                        BlockId::Grass
                    } else if y >= surface_y - dirt_depth {
                        BlockId::Dirt
                    } else {
                        BlockId::Stone
                    };
                    let _ = self.set_block(IVec3::new(x, y, z), id);
                }
            }
        }

        println!(
            "worldgen noise seed={} height={}..{} dirt_depth={}",
            self.terrain.config().seed,
            min_height,
            max_height,
            dirt_depth
        );
    }

    /// Dateiformat:
    /// - Magic (4 Byte), Version (u16), WORLD_XZ (u32), WORLD_MAX_Y (u32)
    /// - Danach lineares Blockarray (u8) für bounded world: y-major, dann z, dann x.
    /// Bounded world erlaubt dieses einfache, robuste Layout ohne Chunk-Metadaten.
    fn save_to_file(&self, path: &str) -> io::Result<()> {
        let save_path = Path::new(path);
        if let Some(parent) = save_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let block_count = (WORLD_XZ * WORLD_XZ * WORLD_HEIGHT) as usize;
        let mut bytes = Vec::with_capacity(4 + 2 + 4 + 4 + block_count);
        bytes.extend_from_slice(&SAVE_MAGIC);
        bytes.extend_from_slice(&SAVE_VERSION.to_le_bytes());
        bytes.extend_from_slice(&(WORLD_XZ as u32).to_le_bytes());
        bytes.extend_from_slice(&(WORLD_MAX_Y as u32).to_le_bytes());

        for y in 0..WORLD_HEIGHT {
            for z in 0..WORLD_XZ {
                for x in 0..WORLD_XZ {
                    let id = self.get_block(IVec3::new(x, y, z));
                    bytes.push(id as u8);
                }
            }
        }

        let tmp_path = save_path.with_extension("bin.tmp");
        fs::write(&tmp_path, &bytes)?;
        fs::rename(tmp_path, save_path)?;
        Ok(())
    }

    fn load_from_file(path: &str) -> io::Result<Option<Self>> {
        let save_path = Path::new(path);
        if !save_path.exists() {
            return Ok(None);
        }

        let bytes = fs::read(save_path)?;
        let header_len = 4 + 2 + 4 + 4;
        if bytes.len() < header_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "save file too small",
            ));
        }

        let file_magic = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if file_magic != SAVE_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "invalid save magic: expected {:?}, got {:?}",
                    SAVE_MAGIC, file_magic
                ),
            ));
        }

        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        if version != SAVE_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unsupported save version: expected {}, got {}",
                    SAVE_VERSION, version
                ),
            ));
        }

        let file_world_xz = u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]) as i32;
        let file_world_max_y =
            u32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]) as i32;
        if file_world_xz != WORLD_XZ || file_world_max_y != WORLD_MAX_Y {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "save dimensions mismatch: expected WORLD_XZ={}, WORLD_MAX_Y={}, got WORLD_XZ={}, WORLD_MAX_Y={}",
                    WORLD_XZ, WORLD_MAX_Y, file_world_xz, file_world_max_y
                ),
            ));
        }

        let expected_block_count = (WORLD_XZ * WORLD_XZ * WORLD_HEIGHT) as usize;
        let expected_total_len = header_len + expected_block_count;
        if bytes.len() != expected_total_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "save payload size mismatch: expected {} bytes, got {} bytes",
                    expected_total_len,
                    bytes.len()
                ),
            ));
        }

        let mut world = World::new();
        let mut index = header_len;
        for y in 0..WORLD_HEIGHT {
            for z in 0..WORLD_XZ {
                for x in 0..WORLD_XZ {
                    let raw = bytes[index];
                    index += 1;
                    let id = BlockId::from_u8(raw).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "invalid block id in save")
                    })?;
                    if id != BlockId::Air {
                        let _ = world.set_block(IVec3::new(x, y, z), id);
                    }
                }
            }
        }

        Ok(Some(world))
    }
}

/// Baut ein Mesh (Vertices + Indices) aus einem Chunk.
/// Naives Meshing: Jede sichtbare Face -> 1 Quad -> 2 Triangles.
///
/// “Sichtbar” heißt: Nachbarblock in dieser Richtung ist nicht solid.
fn build_chunk_mesh(chunk: &Chunk, texture_lookup: &BlockTextureLookup) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices: Vec<Vertex> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // 6 Richtungen: (dx,dy,dz)
    // plus pro Richtung die 4 Eckpunkte des Quads (in lokaler Blockspace)
    //
    // Wir bauen pro Face 4 Vertices in CCW-Order (FrontFace CCW).
    // Koordinaten sind im Blockspace: ein Block ist [0..1] in x/y/z,
    // dann addieren wir Blockposition (x,y,z).
    let faces: [(Face, [[i32; 3]; 4]); 6] = [
        // +Z
        (Face::South, [[0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]]),
        // -Z
        (Face::North, [[1, 0, 0], [0, 0, 0], [0, 1, 0], [1, 1, 0]]),
        // +X
        (Face::East, [[1, 0, 1], [1, 0, 0], [1, 1, 0], [1, 1, 1]]),
        // -X
        (Face::West, [[0, 0, 0], [0, 0, 1], [0, 1, 1], [0, 1, 0]]),
        // +Y
        (Face::Up, [[0, 1, 1], [1, 1, 1], [1, 1, 0], [0, 1, 0]]),
        // -Y
        (Face::Down, [[0, 0, 0], [1, 0, 0], [1, 0, 1], [0, 0, 1]]),
    ];

    for y in 0..CHUNK_SIZE {
        for z in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                let id = chunk.get(x, y, z);
                if !id.is_voxel_filled() {
                    continue;
                }
                if texture_lookup.model_for(id) != BlockModel::Cube {
                    continue;
                }

                let color = [1.0, 1.0, 1.0];

                for (face, quad) in faces {
                    let normal = face.normal_i32();
                    let nx = x + normal[0];
                    let ny = y + normal[1];
                    let nz = z + normal[2];
                    let neighbor = chunk.get(nx, ny, nz);

                    // Face sichtbar, wenn Nachbar nicht solid ist
                    if neighbor.is_voxel_filled()
                        && texture_lookup.model_for(neighbor) == BlockModel::Cube
                    {
                        continue;
                    }

                    let base_index = vertices.len() as u32;
                    let tex_layer = texture_lookup.layer_for(id, face);
                    let tint_kind = tint_kind_for_face(id, face);
                    let quad_world = [
                        [x + quad[0][0], y + quad[0][1], z + quad[0][2]],
                        [x + quad[1][0], y + quad[1][1], z + quad[1][2]],
                        [x + quad[2][0], y + quad[2][1], z + quad[2][2]],
                        [x + quad[3][0], y + quad[3][1], z + quad[3][2]],
                    ];
                    let quad_uvs = face_uvs_for_quad(face, quad_world);

                    // 4 Vertices fürs Quad
                    for (corner_index, p) in quad.iter().enumerate() {
                        vertices.push(Vertex {
                            position: [(x + p[0]) as f32, (y + p[1]) as f32, (z + p[2]) as f32],
                            uv: quad_uvs[corner_index],
                            tex_layer,
                            color,
                            tint_kind,
                        });
                    }

                    // 2 Triangles: (0,1,2) und (0,2,3)
                    indices.extend_from_slice(&[
                        base_index + 0,
                        base_index + 1,
                        base_index + 2,
                        base_index + 0,
                        base_index + 2,
                        base_index + 3,
                    ]);
                }
            }
        }
    }

    append_cross_plants_in_chunk(
        chunk,
        [0.0, 0.0, 0.0],
        texture_lookup,
        &mut vertices,
        &mut indices,
    );

    (vertices, indices)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MaskCell {
    block: BlockId,
    normal_positive: bool,
}

/// Baut ein Chunk-Mesh in World-Space.
/// `build_chunk_mesh` liefert lokale Positionen [0..CHUNK_SIZE]; wir addieren den Chunk-Offset,
/// damit jeder Chunk an seiner Weltposition `(cx, cy, cz) * CHUNK_SIZE` gerendert wird.
fn build_chunk_mesh_with_offset(
    chunk: &Chunk,
    chunk_offset: [f32; 3],
    texture_lookup: &BlockTextureLookup,
) -> (Vec<Vertex>, Vec<u32>) {
    let (mut vertices, indices) = build_chunk_mesh(chunk, texture_lookup);

    for v in &mut vertices {
        v.position[0] += chunk_offset[0];
        v.position[1] += chunk_offset[1];
        v.position[2] += chunk_offset[2];
    }

    (vertices, indices)
}

/// Greedy-Meshing pro Achse:
/// - Wir bauen pro Slice eine 2D-Maske mit sichtbaren Faces (inkl. Richtung + BlockId).
/// - In der Maske werden gleichartige, benachbarte Einträge zu maximalen Rechtecken gemerged.
/// - Pro Rechteck entsteht genau ein Quad.
fn build_chunk_mesh_greedy(
    chunk: &Chunk,
    chunk_offset: [f32; 3],
    texture_lookup: &BlockTextureLookup,
) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices: Vec<Vertex> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    let mut mask: Vec<Option<MaskCell>> = vec![None; (CHUNK_SIZE * CHUNK_SIZE) as usize];
    let mut x = [0_i32; 3];
    for d in 0..3 {
        let u = (d + 1) % 3;
        let v = (d + 2) % 3;
        let mut q = [0_i32; 3];
        q[d] = 1;

        for slice in -1..CHUNK_SIZE {
            x[d] = slice;

            // Maskenaufbau:
            // Ein Eintrag ist genau dann gesetzt, wenn zwischen a/b eine sichtbare Fläche liegt.
            // Gespeichert wird BlockId + Flächennormalen-Richtung (positiv/negativ entlang Achse d).
            let mut n = 0usize;
            for j in 0..CHUNK_SIZE {
                x[v] = j;
                for i in 0..CHUNK_SIZE {
                    x[u] = i;

                    let a = if slice >= 0 {
                        chunk.get(x[0], x[1], x[2])
                    } else {
                        BlockId::Air
                    };
                    let b = if slice < CHUNK_SIZE - 1 {
                        chunk.get(x[0] + q[0], x[1] + q[1], x[2] + q[2])
                    } else {
                        BlockId::Air
                    };
                    let a_cube =
                        a.is_voxel_filled() && texture_lookup.model_for(a) == BlockModel::Cube;
                    let b_cube =
                        b.is_voxel_filled() && texture_lookup.model_for(b) == BlockModel::Cube;

                    mask[n] = match (a_cube, b_cube) {
                        (true, false) => Some(MaskCell {
                            block: a,
                            normal_positive: true,
                        }),
                        (false, true) => Some(MaskCell {
                            block: b,
                            normal_positive: false,
                        }),
                        _ => None,
                    };
                    n += 1;
                }
            }

            // Wir emittieren Faces auf der Slice-Grenzfläche x[d] = slice + 1.
            x[d] = slice + 1;

            let mut j = 0_i32;
            while j < CHUNK_SIZE {
                let mut i = 0_i32;
                while i < CHUNK_SIZE {
                    let idx = (j * CHUNK_SIZE + i) as usize;
                    let Some(cell) = mask[idx] else {
                        i += 1;
                        continue;
                    };

                    // Rechteckbreite w finden.
                    let mut w = 1_i32;
                    while i + w < CHUNK_SIZE {
                        if mask[(j * CHUNK_SIZE + (i + w)) as usize] != Some(cell) {
                            break;
                        }
                        w += 1;
                    }

                    // Rechteckhöhe h finden.
                    let mut h = 1_i32;
                    'height: while j + h < CHUNK_SIZE {
                        for k in 0..w {
                            if mask[((j + h) * CHUNK_SIZE + (i + k)) as usize] != Some(cell) {
                                break 'height;
                            }
                        }
                        h += 1;
                    }

                    let mut du = [0_i32; 3];
                    let mut dv = [0_i32; 3];
                    du[u] = w;
                    dv[v] = h;

                    let origin = {
                        x[u] = i;
                        x[v] = j;
                        x
                    };

                    let p = |px: [i32; 3]| -> [f32; 3] {
                        [
                            (px[0] as f32) + chunk_offset[0],
                            (px[1] as f32) + chunk_offset[1],
                            (px[2] as f32) + chunk_offset[2],
                        ]
                    };

                    let o = origin;
                    let p0 = o;
                    let p1 = [o[0] + du[0], o[1] + du[1], o[2] + du[2]];
                    let p2 = [
                        o[0] + du[0] + dv[0],
                        o[1] + du[1] + dv[1],
                        o[2] + du[2] + dv[2],
                    ];
                    let p3 = [o[0] + dv[0], o[1] + dv[1], o[2] + dv[2]];

                    let color = [1.0, 1.0, 1.0];
                    let base = vertices.len() as u32;
                    let face = match (d, cell.normal_positive) {
                        (0, true) => Face::East,
                        (0, false) => Face::West,
                        (1, true) => Face::Up,
                        (1, false) => Face::Down,
                        (2, true) => Face::South,
                        (2, false) => Face::North,
                        _ => Face::Up,
                    };
                    let tex_layer = texture_lookup.layer_for(cell.block, face);
                    let tint_kind = tint_kind_for_face(cell.block, face);
                    let quad_uvs = face_uvs_for_quad(face, [p0, p1, p2, p3]);

                    // UV-Tiling für gemergte Faces:
                    // statt (0..1) über das ganze Rechteck zu stretchen, nutzen wir
                    // die Face-Basis und die tatsächliche Quad-Ausdehnung (0..u_len, 0..v_len).
                    // Mit Repeat-Sampler wird die Blocktextur pro Block gekachelt.
                    let uv0 = quad_uvs[0];
                    let uv1 = quad_uvs[1];
                    let uv2 = quad_uvs[2];
                    let uv3 = quad_uvs[3];

                    // CCW-Winding:
                    // +Normal: p0,p1,p2,p3
                    // -Normal: p0,p3,p2,p1 (invertiert, damit FrontFace::Ccw konsistent bleibt)
                    let quad = if cell.normal_positive {
                        [(p0, uv0), (p1, uv1), (p2, uv2), (p3, uv3)]
                    } else {
                        [(p0, uv0), (p3, uv3), (p2, uv2), (p1, uv1)]
                    };

                    for (qp, uv) in quad {
                        vertices.push(Vertex {
                            position: p(qp),
                            uv,
                            tex_layer,
                            color,
                            tint_kind,
                        });
                    }
                    indices.extend_from_slice(&[
                        base,
                        base + 1,
                        base + 2,
                        base,
                        base + 2,
                        base + 3,
                    ]);

                    // Die gemergte Region in der Maske leeren, damit sie nicht erneut verarbeitet wird.
                    for dy in 0..h {
                        for dx in 0..w {
                            mask[((j + dy) * CHUNK_SIZE + (i + dx)) as usize] = None;
                        }
                    }

                    i += w;
                }
                j += 1;
            }
        }
    }

    append_cross_plants_in_chunk(
        chunk,
        chunk_offset,
        texture_lookup,
        &mut vertices,
        &mut indices,
    );

    (vertices, indices)
}

fn append_cross_plants_in_chunk(
    chunk: &Chunk,
    chunk_offset: [f32; 3],
    texture_lookup: &BlockTextureLookup,
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
) {
    for y in 0..CHUNK_SIZE {
        for z in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                let id = chunk.get(x, y, z);
                if !id.is_voxel_filled() || texture_lookup.model_for(id) != BlockModel::CrossPlant {
                    continue;
                }
                let tex_layer = texture_lookup
                    .plant_layer_for(id)
                    .unwrap_or(texture_lookup.layer_for(id, Face::North));
                let base = [
                    x as f32 + chunk_offset[0],
                    y as f32 + chunk_offset[1],
                    z as f32 + chunk_offset[2],
                ];
                push_cross_plant(base, tex_layer, vertices, indices);
            }
        }
    }
}

fn push_cross_plant(
    base: [f32; 3],
    tex_layer: u32,
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
) {
    let x = base[0];
    let y = base[1];
    let z = base[2];
    let color = [1.0, 1.0, 1.0];
    let uv0 = [0.0, 1.0];
    let uv1 = [1.0, 1.0];
    let uv2 = [1.0, 0.0];
    let uv3 = [0.0, 0.0];

    let quad_a = [
        [x + 0.0, y + 0.0, z + 0.0],
        [x + 1.0, y + 0.0, z + 1.0],
        [x + 1.0, y + 1.0, z + 1.0],
        [x + 0.0, y + 1.0, z + 0.0],
    ];
    push_double_sided_quad(
        quad_a,
        [uv0, uv1, uv2, uv3],
        tex_layer,
        color,
        vertices,
        indices,
    );

    let quad_b = [
        [x + 1.0, y + 0.0, z + 0.0],
        [x + 0.0, y + 0.0, z + 1.0],
        [x + 0.0, y + 1.0, z + 1.0],
        [x + 1.0, y + 1.0, z + 0.0],
    ];
    push_double_sided_quad(
        quad_b,
        [uv0, uv1, uv2, uv3],
        tex_layer,
        color,
        vertices,
        indices,
    );
}

fn push_double_sided_quad(
    positions: [[f32; 3]; 4],
    uvs: [[f32; 2]; 4],
    tex_layer: u32,
    color: [f32; 3],
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
) {
    let base = vertices.len() as u32;
    for i in 0..4 {
        vertices.push(Vertex {
            position: positions[i],
            uv: uvs[i],
            tex_layer,
            color,
            tint_kind: TINT_NONE,
        });
    }
    // Front
    indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    // Back
    indices.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
}

fn chunk_offset(coord: ChunkCoord) -> [f32; 3] {
    [
        (coord.x * CHUNK_SIZE) as f32,
        (coord.y * CHUNK_SIZE) as f32,
        (coord.z * CHUNK_SIZE) as f32,
    ]
}

#[derive(Default, Clone, Copy, Debug)]
struct CollisionFlags {
    hit_x: bool,
    hit_z: bool,
    hit_y_up: bool,
    hit_y_down: bool,
}

fn player_aabb(feet_pos: Vec3) -> (Vec3, Vec3) {
    (
        feet_pos + Vec3::new(-PLAYER_RADIUS, 0.0, -PLAYER_RADIUS),
        feet_pos + Vec3::new(PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_RADIUS),
    )
}

/// Prüft, ob eine AABB irgendeine solide Voxelzelle überlappt.
fn aabb_collides(world: &World, aabb_min: Vec3, aabb_max: Vec3) -> bool {
    let min_x = aabb_min.x.floor() as i32;
    let min_y = aabb_min.y.floor() as i32;
    let min_z = aabb_min.z.floor() as i32;
    let max_x = (aabb_max.x - COLLISION_EPS).floor() as i32;
    let max_y = (aabb_max.y - COLLISION_EPS).floor() as i32;
    let max_z = (aabb_max.z - COLLISION_EPS).floor() as i32;

    for y in min_y..=max_y {
        for z in min_z..=max_z {
            for x in min_x..=max_x {
                if world.is_solid_block_at(x, y, z) {
                    return true;
                }
            }
        }
    }
    false
}

fn move_axis_with_collisions(
    world: &World,
    mut pos: Vec3,
    axis: usize,
    delta_axis: f32,
    hit_flag: &mut bool,
) -> Vec3 {
    if delta_axis.abs() <= f32::EPSILON {
        return pos;
    }

    let steps = ((delta_axis.abs() / COLLISION_SUBSTEP).ceil().max(1.0)) as i32;
    let step = delta_axis / (steps as f32);

    for _ in 0..steps {
        let mut test_pos = pos;
        test_pos[axis] += step;
        let (test_min, test_max) = player_aabb(test_pos);
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
            let (mid_min, mid_max) = player_aabb(mid_pos);
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

/// Shared Bewegung mit Axis-Separation:
/// X -> Z -> Y, damit diagonales Sliding an Wänden robust bleibt.
fn move_with_collisions(world: &World, feet_pos: Vec3, delta: Vec3) -> (Vec3, CollisionFlags) {
    let mut pos = feet_pos;
    let mut flags = CollisionFlags::default();
    let mut hit_y = false;

    pos = move_axis_with_collisions(world, pos, 0, delta.x, &mut flags.hit_x);
    pos = move_axis_with_collisions(world, pos, 2, delta.z, &mut flags.hit_z);
    pos = move_axis_with_collisions(world, pos, 1, delta.y, &mut hit_y);
    if hit_y {
        if delta.y > 0.0 {
            flags.hit_y_up = true;
        } else if delta.y < 0.0 {
            flags.hit_y_down = true;
        }
    }

    (pos, flags)
}

fn jump_velocity() -> f32 {
    (2.0 * GRAVITY * JUMP_HEIGHT).sqrt()
}

/// Korrigiert kleine Bodenabstände und liefert den Ground-State.
fn snap_to_ground(world: &World, feet_pos: Vec3) -> (Vec3, bool) {
    let (snapped_pos, flags) =
        move_with_collisions(world, feet_pos, Vec3::new(0.0, -GROUND_CHECK_DISTANCE, 0.0));
    if flags.hit_y_down {
        (snapped_pos, true)
    } else {
        (feet_pos, false)
    }
}

fn update_flight_movement(
    world: &World,
    camera: &Camera,
    controller: &mut CameraController,
    player: &mut Player,
    last_space_tap: &mut Option<Instant>,
    dt: f32,
) {
    let desired_delta = controller.flight_move_delta(camera, dt);
    let (new_feet, collision) = move_with_collisions(world, player.feet_position, desired_delta);
    player.feet_position = new_feet;
    player._velocity_y = 0.0;
    controller.clear_jump_requested();

    // Auto-exit nur bei echter Bodenberührung während negativer Y-Bewegung.
    if desired_delta.y < 0.0 && collision.hit_y_down {
        player.mode = MoveMode::Walking;
        player.on_ground = true;
        *last_space_tap = None;
    } else {
        player.on_ground = collision.hit_y_down;
    }
}

#[derive(Clone, Copy, Debug)]
struct RaycastHit {
    hit_block_pos: IVec3,
    hit_normal: IVec3,
}

impl RaycastHit {
    fn place_pos(self) -> IVec3 {
        self.hit_block_pos + self.hit_normal
    }
}

/// Voxel-DDA Raycast:
/// Wir traversieren Zellen entlang des Rays über Grid-Grenzen (nicht per kleinem Raymarching),
/// damit der erste getroffene Block robust bestimmt wird.
fn raycast_world(
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
        if world.get_block(cell).is_voxel_filled() {
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

/// Erzeugt ein Quad genau auf der getroffenen Face.
/// Die Vertices werden minimal entlang der Face-Normale verschoben,
/// damit das Overlay nicht mit der Blockface z-fightet.
fn build_face_highlight_mesh(
    hit_block_pos: IVec3,
    hit_normal: IVec3,
    face_offset: f32,
) -> (Vec<Vertex>, Vec<u32>) {
    let base = Vec3::new(
        hit_block_pos.x as f32,
        hit_block_pos.y as f32,
        hit_block_pos.z as f32,
    );
    let n = Vec3::new(
        hit_normal.x as f32,
        hit_normal.y as f32,
        hit_normal.z as f32,
    );

    let (origin, u, v) = match (hit_normal.x, hit_normal.y, hit_normal.z) {
        (1, 0, 0) => (
            base + Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
        ),
        (-1, 0, 0) => (base, Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.0, 1.0, 0.0)),
        (0, 1, 0) => (
            base + Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(1.0, 0.0, 0.0),
        ),
        (0, -1, 0) => (base, Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0)),
        (0, 0, 1) => (
            base + Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        ),
        (0, 0, -1) => (base, Vec3::new(0.0, 1.0, 0.0), Vec3::new(1.0, 0.0, 0.0)),
        _ => (base, Vec3::X, Vec3::Y),
    };

    let p0 = origin + n * face_offset;
    let p1 = origin + u + n * face_offset;
    let p2 = origin + u + v + n * face_offset;
    let p3 = origin + v + n * face_offset;

    let white = [1.0, 1.0, 1.0];
    let vertices = vec![
        Vertex {
            position: p0.to_array(),
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: TINT_NONE,
            color: white,
        },
        Vertex {
            position: p1.to_array(),
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: TINT_NONE,
            color: white,
        },
        Vertex {
            position: p2.to_array(),
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: TINT_NONE,
            color: white,
        },
        Vertex {
            position: p3.to_array(),
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: TINT_NONE,
            color: white,
        },
    ];
    let indices = vec![0, 1, 2, 0, 2, 3];

    (vertices, indices)
}

/// Baut ein plus-förmiges Crosshair in Screen-Space (NDC).
/// Die Dicke entsteht durch zwei Rechtecke (horizontal + vertikal), nicht durch 1px-Linien.
fn build_crosshair_mesh(view_width: u32, view_height: u32) -> (Vec<Vertex>, Vec<u32>) {
    // NDC reicht von -1..1. Wir modellieren Pixel-Offsets relativ zur Screen-Mitte:
    // delta_x_ndc = (delta_px / width) * 2
    // delta_y_ndc = (delta_px / height) * 2
    let px_to_ndc_x = 2.0 / (view_width.max(1) as f32);
    let px_to_ndc_y = 2.0 / (view_height.max(1) as f32);

    let half_len_x = CROSSHAIR_HALF_LEN_PX * px_to_ndc_x;
    let half_len_y = CROSSHAIR_HALF_LEN_PX * px_to_ndc_y;
    let half_thick_x = (CROSSHAIR_THICKNESS_PX * 0.5) * px_to_ndc_x;
    let half_thick_y = (CROSSHAIR_THICKNESS_PX * 0.5) * px_to_ndc_y;

    let white = [1.0, 1.0, 1.0];

    // Horizontaler Balken um den Screen-Mittelpunkt.
    let h0 = [-half_len_x, -half_thick_y, 0.0];
    let h1 = [half_len_x, -half_thick_y, 0.0];
    let h2 = [half_len_x, half_thick_y, 0.0];
    let h3 = [-half_len_x, half_thick_y, 0.0];

    // Vertikaler Balken um den Screen-Mittelpunkt.
    let v0 = [-half_thick_x, -half_len_y, 0.0];
    let v1 = [half_thick_x, -half_len_y, 0.0];
    let v2 = [half_thick_x, half_len_y, 0.0];
    let v3 = [-half_thick_x, half_len_y, 0.0];

    let vertices = vec![
        Vertex {
            position: h0,
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: UI_MODE_SOLID,
            color: white,
        },
        Vertex {
            position: h1,
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: UI_MODE_SOLID,
            color: white,
        },
        Vertex {
            position: h2,
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: UI_MODE_SOLID,
            color: white,
        },
        Vertex {
            position: h3,
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: UI_MODE_SOLID,
            color: white,
        },
        Vertex {
            position: v0,
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: UI_MODE_SOLID,
            color: white,
        },
        Vertex {
            position: v1,
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: UI_MODE_SOLID,
            color: white,
        },
        Vertex {
            position: v2,
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: UI_MODE_SOLID,
            color: white,
        },
        Vertex {
            position: v3,
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: UI_MODE_SOLID,
            color: white,
        },
    ];

    let indices = vec![
        0, 1, 2, 0, 2, 3, // horizontal
        4, 5, 6, 4, 6, 7, // vertikal
    ];

    (vertices, indices)
}

/// Preview-Icon oben rechts in NDC.
/// Wir erzeugen ein Quadrat plus Akzent-Streifen. "Flipped" wird über UV-Mirroring umgesetzt.
fn build_block_preview_mesh(
    view_width: u32,
    view_height: u32,
    texture_layer: u32,
    flipped_horizontal: bool,
    flipped_vertical: bool,
) -> (Vec<Vertex>, Vec<u32>) {
    let px_to_ndc_x = 2.0 / (view_width.max(1) as f32);
    let px_to_ndc_y = 2.0 / (view_height.max(1) as f32);

    let half_w = (PREVIEW_SIZE_PX * 0.5) * px_to_ndc_x;
    let half_h = (PREVIEW_SIZE_PX * 0.5) * px_to_ndc_y;
    let stripe_w = PREVIEW_ACCENT_STRIPE_PX * px_to_ndc_x;

    // Oben rechts in NDC: rechter/oberer Rand = +1.
    let center_x = 1.0 - (PREVIEW_MARGIN_RIGHT_PX * px_to_ndc_x) - half_w;
    let center_y = 1.0 - (PREVIEW_MARGIN_TOP_PX * px_to_ndc_y) - half_h;

    let mut local = vec![
        // Haupt-Quad
        [-half_w, -half_h, 0.0],
        [half_w, -half_h, 0.0],
        [half_w, half_h, 0.0],
        [-half_w, half_h, 0.0],
        // Akzent-Streifen (initial links)
        [-half_w, -half_h, 0.0],
        [-half_w + stripe_w, -half_h, 0.0],
        [-half_w + stripe_w, half_h, 0.0],
        [-half_w, half_h, 0.0],
    ];

    for p in &mut local {
        p[0] += center_x;
        p[1] += center_y;
    }

    let stripe_uv_span = (PREVIEW_ACCENT_STRIPE_PX / PREVIEW_SIZE_PX).clamp(0.0, 1.0);
    let uv = |u: f32, v: f32| -> [f32; 2] {
        let u_flipped = if flipped_horizontal { 1.0 - u } else { u };
        let v_flipped = if flipped_vertical { 1.0 - v } else { v };
        [u_flipped, v_flipped]
    };

    let base = [1.0, 1.0, 1.0];
    let accent = [0.72, 0.72, 0.72];

    let vertices = vec![
        Vertex {
            position: local[0],
            uv: uv(0.0, 0.0),
            tex_layer: texture_layer,
            tint_kind: UI_MODE_TEXTURED,
            color: base,
        },
        Vertex {
            position: local[1],
            uv: uv(1.0, 0.0),
            tex_layer: texture_layer,
            tint_kind: UI_MODE_TEXTURED,
            color: base,
        },
        Vertex {
            position: local[2],
            uv: uv(1.0, 1.0),
            tex_layer: texture_layer,
            tint_kind: UI_MODE_TEXTURED,
            color: base,
        },
        Vertex {
            position: local[3],
            uv: uv(0.0, 1.0),
            tex_layer: texture_layer,
            tint_kind: UI_MODE_TEXTURED,
            color: base,
        },
        Vertex {
            position: local[4],
            uv: uv(0.0, 0.0),
            tex_layer: texture_layer,
            tint_kind: UI_MODE_TEXTURED,
            color: accent,
        },
        Vertex {
            position: local[5],
            uv: uv(stripe_uv_span, 0.0),
            tex_layer: texture_layer,
            tint_kind: UI_MODE_TEXTURED,
            color: accent,
        },
        Vertex {
            position: local[6],
            uv: uv(stripe_uv_span, 1.0),
            tex_layer: texture_layer,
            tint_kind: UI_MODE_TEXTURED,
            color: accent,
        },
        Vertex {
            position: local[7],
            uv: uv(0.0, 1.0),
            tex_layer: texture_layer,
            tint_kind: UI_MODE_TEXTURED,
            color: accent,
        },
    ];

    let indices = vec![
        0, 1, 2, 0, 2, 3, // Haupt-Quad
        4, 5, 6, 4, 6, 7, // Streifen
    ];

    (vertices, indices)
}

/// Uniform: eine 4x4 Matrix (ViewProjection)
/// Diese Matrix wird im Vertex-Shader auf jede Position angewendet.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 3],
    _pad0: f32,
    fog_params: [f32; 4], // x=start, y=end, z=darken_strength, w=reserved
}

impl CameraUniform {
    fn new() -> Self {
        Self {
            view_proj: Mat4::IDENTITY.to_cols_array_2d(),
            camera_pos: [0.0, 0.0, 0.0],
            _pad0: 0.0,
            fog_params: [
                FOG_START_DISTANCE,
                FOG_END_DISTANCE,
                FOG_DARKEN_STRENGTH,
                0.0,
            ],
        }
    }

    fn update(&mut self, camera: &Camera, aspect: f32) {
        let view = camera.view_matrix();
        let proj = camera.proj_matrix(aspect);

        // Wichtig: Reihenfolge = OPENGL_TO_WGPU * proj * view (erst View, dann Projection).
        let view_proj = OPENGL_TO_WGPU_MATRIX * proj * view;

        if cfg!(debug_assertions) {
            // Mini-Regressionscheck: Punkt vor der Kamera muss im erwarteten Clipspace landen.
            let front_world = camera.position + camera.forward() * 2.0;
            let clip = view_proj * front_world.extend(1.0);
            let z_ndc = clip.z / clip.w;
            debug_assert!(
                clip.w > 0.0 && (0.0..=1.0).contains(&z_ndc),
                "camera clip regression: w={}, z_ndc={}",
                clip.w,
                z_ndc
            );
        }

        self.view_proj = view_proj.to_cols_array_2d();
        self.camera_pos = camera.position.to_array();
        self.fog_params = [
            FOG_START_DISTANCE,
            FOG_END_DISTANCE,
            FOG_DARKEN_STRENGTH,
            0.0,
        ];
    }
}

/// ============================
/// Shader (WGSL) als String
/// ============================
/// World-Shader:
/// - group(0): Textur + Sampler
/// - group(1): Kamera-Uniform (view_proj + fog)
/// - sampled PNGs laufen in sRGB (`Rgba8UnormSrgb`), daher stimmen die Farben.
const WORLD_SHADER_WGSL: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
    camera_pos: vec3<f32>,
    _pad0: f32,
    fog_params: vec4<f32>,
};

@group(0) @binding(0)
var block_tex: texture_2d_array<f32>;
@group(0) @binding(1)
var block_sampler: sampler;
@group(1) @binding(0)
var<uniform> camera: Camera;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) tex_layer: u32,
    @location(3) color: vec3<f32>,
    @location(4) tint_kind: u32,
};

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) tex_layer: u32,
    @location(3) world_pos: vec3<f32>,
    @location(4) @interpolate(flat) tint_kind: u32,
};

@vertex
fn vs_main(input: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_pos = camera.view_proj * vec4<f32>(input.position, 1.0);
    out.color = input.color;
    out.uv = input.uv;
    out.tex_layer = input.tex_layer;
    out.world_pos = input.position;
    out.tint_kind = input.tint_kind;
    return out;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    let GRASS_TINT: vec3<f32> = vec3<f32>(124.0 / 255.0, 189.0 / 255.0, 107.0 / 255.0);
    let sampled = textureSample(block_tex, block_sampler, input.uv, i32(input.tex_layer));
    if (sampled.a < 0.5) {
        discard;
    }
    var base_color = sampled.rgb * input.color;
    if (input.tint_kind == 1u) {
        base_color = base_color * GRASS_TINT;
    }

    // Distanz-basiertes Darkening im Fragment ist robust, weil wir pro Pixel arbeiten.
    // Wir nutzen echte World-Distanz zur Kamera statt nur clip-depth.
    let distance = distance(input.world_pos, camera.camera_pos);
    let fog_start = camera.fog_params.x;
    let fog_end = camera.fog_params.y;
    let darken_strength = camera.fog_params.z;

    let fog_factor = clamp(
        (distance - fog_start) / max(fog_end - fog_start, 0.0001),
        0.0,
        1.0
    );
    let darken = 1.0 - darken_strength * fog_factor;
    return vec4<f32>(base_color * darken, sampled.a);
}
"#;

/// Farbbasierter 3D-Shader für Highlight/Partikel (ohne Textur-Sampling).
const COLOR_SHADER_WGSL: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
    camera_pos: vec3<f32>,
    _pad0: f32,
    fog_params: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) tex_layer: u32,
    @location(3) color: vec3<f32>,
};

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) world_pos: vec3<f32>,
};

@vertex
fn vs_main(input: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_pos = camera.view_proj * vec4<f32>(input.position, 1.0);
    out.color = input.color;
    out.world_pos = input.position;
    return out;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    let distance = distance(input.world_pos, camera.camera_pos);
    let fog_start = camera.fog_params.x;
    let fog_end = camera.fog_params.y;
    let darken_strength = camera.fog_params.z;

    let fog_factor = clamp(
        (distance - fog_start) / max(fog_end - fog_start, 0.0001),
        0.0,
        1.0
    );
    let darken = 1.0 - darken_strength * fog_factor;
    return vec4<f32>(input.color * darken, 1.0);
}
"#;

const UI_SHADER_WGSL: &str = r#"
@group(0) @binding(0)
var block_tex: texture_2d_array<f32>;
@group(0) @binding(1)
var block_sampler: sampler;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) tex_layer: u32,
    @location(3) color: vec3<f32>,
    @location(4) mode: u32,
};

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) tex_layer: u32,
    @location(3) @interpolate(flat) mode: u32,
};

@vertex
fn vs_main(input: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_pos = vec4<f32>(input.position, 1.0);
    out.color = input.color;
    out.uv = input.uv;
    out.tex_layer = input.tex_layer;
    out.mode = input.mode;
    return out;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    if (input.mode == 1u) {
        let sampled = textureSample(block_tex, block_sampler, input.uv, i32(input.tex_layer));
        return vec4<f32>(sampled.rgb * input.color, sampled.a);
    }
    return vec4<f32>(input.color, 1.0);
}
"#;

/// ============================
/// Depth Texture helper
/// ============================
struct DepthTexture {
    view: wgpu::TextureView,
    format: wgpu::TextureFormat,
}

impl DepthTexture {
    fn create(device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) -> Self {
        // Typisches Depth Format
        let format = wgpu::TextureFormat::Depth24Plus;

        let desc = wgpu::TextureDescriptor {
            label: Some("Depth Texture"),
            size: wgpu::Extent3d {
                width: config.width,
                height: config.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        };

        let tex = device.create_texture(&desc);
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());

        Self { view, format }
    }
}

struct ChunkMesh {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
}

struct Particle {
    pos: Vec3,
    vel: Vec3,
    life: f32,
    color: [f32; 3],
}

/// ============================
/// Render State (wgpu)
/// ============================
struct State {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,

    // Pipeline stuff
    render_pipeline_back_depth: wgpu::RenderPipeline,
    render_pipeline_no_cull_depth: wgpu::RenderPipeline,
    render_pipeline_back_no_depth: wgpu::RenderPipeline,
    render_pipeline_no_cull_no_depth: wgpu::RenderPipeline,
    highlight_pipeline: wgpu::RenderPipeline,
    ui_pipeline: wgpu::RenderPipeline,
    debug_backface_culling: bool,
    debug_depth_test: bool,
    world: World,
    chunk_meshes: HashMap<ChunkCoord, ChunkMesh>,
    highlight_mesh: Option<ChunkMesh>,
    crosshair_mesh: ChunkMesh,
    preview_mesh: ChunkMesh,
    preview_block: BlockId,
    _texture_registry: TextureRegistry,
    _world_texture_array: TextureArray,
    world_texture_bind_group: wgpu::BindGroup,
    block_texture_lookup: BlockTextureLookup,
    particles: Vec<Particle>,
    particle_rng: u32,

    // Camera uniform
    camera_uniform: CameraUniform,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,

    // Depth buffer
    depth: DepthTexture,
}

impl State {
    fn create_world_pipeline(
        device: &wgpu::Device,
        config: &wgpu::SurfaceConfiguration,
        world_shader: &wgpu::ShaderModule,
        world_pipeline_layout: &wgpu::PipelineLayout,
        depth_format: wgpu::TextureFormat,
        cull_mode: Option<wgpu::Face>,
        depth_test_enabled: bool,
    ) -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Cube Pipeline"),
            layout: Some(world_pipeline_layout),
            vertex: wgpu::VertexState {
                module: world_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Vertex::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: world_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                // Meshing liefert outward faces in CCW, daher passt Backface-Culling.
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode,
                front_face: wgpu::FrontFace::Ccw,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: depth_format,
                depth_write_enabled: depth_test_enabled,
                depth_compare: if depth_test_enabled {
                    // LessEqual ist robuster gegen Precision-/Coplanar-Randfälle als Less.
                    wgpu::CompareFunction::LessEqual
                } else {
                    wgpu::CompareFunction::Always
                },
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        })
    }

    fn world_pipeline(&self) -> &wgpu::RenderPipeline {
        match (self.debug_backface_culling, self.debug_depth_test) {
            (true, true) => &self.render_pipeline_back_depth,
            (false, true) => &self.render_pipeline_no_cull_depth,
            (true, false) => &self.render_pipeline_back_no_depth,
            (false, false) => &self.render_pipeline_no_cull_no_depth,
        }
    }

    fn toggle_debug_culling(&mut self) {
        self.debug_backface_culling = !self.debug_backface_culling;
        println!(
            "debug render: backface_culling={}, depth_test={}",
            self.debug_backface_culling, self.debug_depth_test
        );
    }

    fn toggle_debug_depth_test(&mut self) {
        self.debug_depth_test = !self.debug_depth_test;
        println!(
            "debug render: backface_culling={}, depth_test={}",
            self.debug_backface_culling, self.debug_depth_test
        );
    }

    async fn new(window: &Window) -> Self {
        let size = window.inner_size();

        let instance = wgpu::Instance::default();

        let surface_temp = instance
            .create_surface(window)
            .expect("Failed to create wgpu surface from window");

        // Surface lifetime workaround (weil surface an window hängt)
        let surface: wgpu::Surface<'static> = unsafe {
            std::mem::transmute::<wgpu::Surface<'_>, wgpu::Surface<'static>>(surface_temp)
        };

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("No suitable GPU adapters found");

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .expect("Failed to create device");

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        // -------- Camera uniform (Buffer + BindGroup) --------
        let camera_uniform = CameraUniform::new();
        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Camera Buffer"),
            contents: bytemuck::bytes_of(&camera_uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        // BindGroupLayout: beschreibt, was der Shader erwartet (Uniform Buffer an binding(0))
        let camera_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Camera BindGroupLayout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    // Camera-Uniform wird in VS (view_proj) und FS (distance darkening) gelesen.
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Camera BindGroup"),
            layout: &camera_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        // Texture-Array BindGroupLayout: binding0=texture_2d_array, binding1=sampler
        let texture_bind_group_layout = TextureArray::create_bind_group_layout(&device);

        // Blockdefinitionen mit Face-spezifischen Texturen.
        let block_registry = BlockRegistry::new();
        debug_assert_eq!(
            block_registry.texture_key_for(BlockId::Grass, Face::Up),
            TEX_GRASS_TOP
        );
        debug_assert_eq!(
            block_registry.texture_key_for(BlockId::Grass, Face::Down),
            TEX_DIRT
        );
        debug_assert_eq!(
            block_registry.texture_key_for(BlockId::Grass, Face::East),
            TEX_GRASS_SIDE
        );
        debug_assert_eq!(
            block_registry.texture_key_for(BlockId::Stone, Face::North),
            TEX_STONE
        );
        debug_assert_eq!(
            block_registry.texture_key_for(BlockId::OakPlanks, Face::Up),
            TEX_OAK_PLANKS
        );
        debug_assert_eq!(
            block_registry.texture_key_for(BlockId::Sapling, Face::North),
            TEX_SAPLING
        );
        debug_assert_eq!(
            block_registry.model_for(BlockId::Sapling),
            BlockModel::CrossPlant
        );
        debug_assert!(!block_registry.solid_for(BlockId::Sapling));
        debug_assert!(block_registry.solid_for(BlockId::OakPlanks));
        println!(
            "block texture self-check ok: grass(top={}, side={}, bottom={}), stone(all={}), oak_planks(all={}), sapling(cross={})",
            TEX_GRASS_TOP, TEX_GRASS_SIDE, TEX_DIRT, TEX_STONE, TEX_OAK_PLANKS, TEX_SAPLING
        );

        // Registry nutzt Mojang-Keys relativ zu assets/minecraft/.
        let mut texture_registry = TextureRegistry::new(ASSET_ROOT);
        for key in TEXTURE_SELF_TEST_KEYS {
            match texture_registry.get_or_load(key) {
                Ok(_) => println!("texture self-test ok: {}", key),
                Err(err) => eprintln!("texture self-test failed for {}: {}", key, err),
            }
        }

        let block_texture_keys = block_registry.collect_texture_keys();
        for required_key in REQUIRED_FACE_TEXTURE_KEYS {
            assert!(
                block_texture_keys.iter().any(|key| key == required_key),
                "required block texture key '{}' is not registered in BlockRegistry",
                required_key
            );
        }

        // Wir suchen zuerst die erste erfolgreich geladene Textur, um die Layer-Größe festzulegen.
        // Jeder Key aus der BlockRegistry muss ladbar sein, sonst starten wir nicht.
        let mut layer_size: Option<(u32, u32)> = None;
        for key in &block_texture_keys {
            let handle = texture_registry.get_or_load(key.as_str()).unwrap_or_else(|err| {
                panic!(
                    "[error] failed to load block texture key '{}' referenced by BlockRegistry: {}",
                    key, err
                )
            });
            if layer_size.is_none() {
                let image = texture_registry
                    .get(&handle)
                    .unwrap_or_else(|| panic!("missing cached texture image for key '{}'", key));
                layer_size = Some((image.width, image.height));
            }
        }

        let (layer_width, layer_height) = layer_size.unwrap_or((16, 16));
        let missing_rgba = make_missing_texture_rgba(layer_width, layer_height);
        let mut key_to_layer: HashMap<String, u32> = HashMap::new();
        let mut texture_layers_rgba: Vec<Vec<u8>> =
            Vec::with_capacity(block_texture_keys.len() + 1);

        // Layer 0 ist absichtlich "missing texture", damit fehlende/kaputte Keys niemals still auf Stone fallen.
        let missing_layer = texture_layers_rgba.len() as u32;
        texture_layers_rgba.push(missing_rgba.clone());

        for key in &block_texture_keys {
            let layer_index = texture_layers_rgba.len() as u32;
            let handle = texture_registry.get_or_load(key.as_str()).unwrap_or_else(|err| {
                panic!(
                    "[error] failed to load block texture key '{}' referenced by BlockRegistry: {}",
                    key, err
                )
            });
            let image = texture_registry
                .get(&handle)
                .unwrap_or_else(|| panic!("missing cached texture image for key '{}'", key));
            if image.width != layer_width || image.height != layer_height {
                panic!(
                    "block texture '{}' has size {}x{}, expected {}x{}",
                    key, image.width, image.height, layer_width, layer_height
                );
            }
            let rgba = image.rgba.clone();
            key_to_layer.insert(key.clone(), layer_index);
            texture_layers_rgba.push(rgba);
        }

        // Sicherheitscheck: Grass Top/Side/Bottom müssen echte Layer haben.
        for required_key in REQUIRED_FACE_TEXTURE_KEYS {
            let layer = key_to_layer
                .get(required_key)
                .copied()
                .unwrap_or(missing_layer);
            assert!(
                layer != missing_layer,
                "required block texture key '{}' resolved to missing-texture layer",
                required_key
            );
        }

        let world_texture_array = TextureArray::from_rgba8_layers(
            &device,
            &queue,
            &texture_layers_rgba,
            layer_width,
            layer_height,
            "World Texture Array",
        )
        .unwrap_or_else(|err| panic!("failed to build world texture array: {}", err));
        let world_texture_bind_group = world_texture_array.create_bind_group(
            &device,
            &texture_bind_group_layout,
            "World Texture BindGroup",
        );
        let block_texture_lookup =
            BlockTextureLookup::new(&block_registry, &key_to_layer, missing_layer);
        debug_assert_eq!(
            block_texture_lookup.model_for(BlockId::Sapling),
            BlockModel::CrossPlant
        );
        debug_assert!(!block_texture_lookup.is_solid_for(BlockId::Sapling));

        // Self-Check auf Layer-Ebene:
        // Grass muss pro Face unterschiedliche Layer erlauben (top/side/bottom),
        // Stone/Cobble bleibt auf allen Faces gleich.
        let grass_top_layer = block_texture_lookup.layer_for(BlockId::Grass, Face::Up);
        let grass_side_layer = block_texture_lookup.layer_for(BlockId::Grass, Face::North);
        let grass_bottom_layer = block_texture_lookup.layer_for(BlockId::Grass, Face::Down);
        let stone_top_layer = block_texture_lookup.layer_for(BlockId::Stone, Face::Up);
        let stone_side_layer = block_texture_lookup.layer_for(BlockId::Stone, Face::East);
        debug_assert_ne!(grass_top_layer, grass_side_layer);
        debug_assert_ne!(grass_side_layer, grass_bottom_layer);
        debug_assert_eq!(stone_top_layer, stone_side_layer);
        println!(
            "block texture layer self-check ok: grass(top={}, side={}, bottom={}), stone(layer={})",
            grass_top_layer, grass_side_layer, grass_bottom_layer, stone_top_layer
        );

        // -------- Shader + Pipeline --------
        let world_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("World Shader"),
            source: wgpu::ShaderSource::Wgsl(WORLD_SHADER_WGSL.into()),
        });
        let color_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Color Shader"),
            source: wgpu::ShaderSource::Wgsl(COLOR_SHADER_WGSL.into()),
        });

        // World-Pipeline verwendet Textur + Kamera.
        let world_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("World Pipeline Layout"),
                bind_group_layouts: &[&texture_bind_group_layout, &camera_bind_group_layout],
                immediate_size: 0,
            });
        // Color-Pipeline (Highlight/Partikel) braucht nur die Kamera.
        let color_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Color Pipeline Layout"),
                bind_group_layouts: &[&camera_bind_group_layout],
                immediate_size: 0,
            });

        // Depth texture für korrekte 3D-Sichtbarkeit (Faces hintereinander)
        let depth = DepthTexture::create(&device, &config);

        let render_pipeline_back_depth = Self::create_world_pipeline(
            &device,
            &config,
            &world_shader,
            &world_pipeline_layout,
            depth.format,
            Some(wgpu::Face::Back),
            true,
        );
        let render_pipeline_no_cull_depth = Self::create_world_pipeline(
            &device,
            &config,
            &world_shader,
            &world_pipeline_layout,
            depth.format,
            None,
            true,
        );
        let render_pipeline_back_no_depth = Self::create_world_pipeline(
            &device,
            &config,
            &world_shader,
            &world_pipeline_layout,
            depth.format,
            Some(wgpu::Face::Back),
            false,
        );
        let render_pipeline_no_cull_no_depth = Self::create_world_pipeline(
            &device,
            &config,
            &world_shader,
            &world_pipeline_layout,
            depth.format,
            None,
            false,
        );

        // Highlight-Overlay Pipeline:
        // eigene Pipeline ohne Backface-Culling, damit die Face-Markierung zuverlässig sichtbar bleibt.
        let highlight_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Highlight Pipeline"),
            layout: Some(&color_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &color_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Vertex::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &color_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                front_face: wgpu::FrontFace::Ccw,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: depth.format,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // 2D-UI Pipeline in NDC: unabhängig von Kamera/View-Matrix.
        let ui_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("UI Shader"),
            source: wgpu::ShaderSource::Wgsl(UI_SHADER_WGSL.into()),
        });
        let ui_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("UI Pipeline Layout"),
            bind_group_layouts: &[&texture_bind_group_layout],
            immediate_size: 0,
        });
        let ui_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("UI Pipeline"),
            layout: Some(&ui_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &ui_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Vertex::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &ui_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            // Gleicher Depth-Format wie der aktive RenderPass, sonst Pipeline/Pass inkompatibel.
            // UI schreibt keine Depth-Werte und besteht den Test immer, damit es als Overlay sichtbar bleibt.
            depth_stencil: Some(wgpu::DepthStencilState {
                format: depth.format,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let (crosshair_vertices, crosshair_indices) =
            build_crosshair_mesh(config.width, config.height);
        let crosshair_vertex_buffer =
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Crosshair Vertex Buffer"),
                contents: bytemuck::cast_slice(&crosshair_vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let crosshair_index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Crosshair Index Buffer"),
            contents: bytemuck::cast_slice(&crosshair_indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let crosshair_mesh = ChunkMesh {
            vertex_buffer: crosshair_vertex_buffer,
            index_buffer: crosshair_index_buffer,
            index_count: crosshair_indices.len() as u32,
        };

        let preview_block = BlockId::default();
        let preview_layer = block_texture_lookup.layer_for(preview_block, Face::North);
        let (preview_vertices, preview_indices) = build_block_preview_mesh(
            config.width,
            config.height,
            preview_layer,
            PREVIEW_FLIP_HORIZONTAL,
            PREVIEW_FLIP_VERTICAL,
        );
        let preview_vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Preview Vertex Buffer"),
            contents: bytemuck::cast_slice(&preview_vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let preview_index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Preview Index Buffer"),
            contents: bytemuck::cast_slice(&preview_indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let preview_mesh = ChunkMesh {
            vertex_buffer: preview_vertex_buffer,
            index_buffer: preview_index_buffer,
            index_count: preview_indices.len() as u32,
        };

        debug_assert_eq!(WORLD_XZ % CHUNK_SIZE, 0);

        let (mut world, generated_default_world) = match STARTUP_WORLD_MODE {
            StartupWorldMode::AlwaysRegenerate => {
                println!("startup mode: AlwaysRegenerate");
                let mut generated = World::new();
                generated.generate_noise_world();
                (generated, true)
            }
            StartupWorldMode::LoadIfPresentElseGenerate => match World::load_from_file(SAVE_PATH) {
                Ok(Some(loaded)) => {
                    println!(
                        "startup mode: LoadIfPresentElseGenerate -> loaded {}",
                        SAVE_PATH
                    );
                    (loaded, false)
                }
                Ok(None) => {
                    println!(
                        "startup mode: LoadIfPresentElseGenerate -> no save found, generating world"
                    );
                    let mut generated = World::new();
                    generated.generate_noise_world();
                    (generated, true)
                }
                Err(err) => {
                    eprintln!(
                        "startup mode: LoadIfPresentElseGenerate -> failed to load save ({}), generating world",
                        err
                    );
                    let mut generated = World::new();
                    generated.generate_noise_world();
                    (generated, true)
                }
            },
        };
        if generated_default_world {
            // Diese Invariante gilt nur direkt nach der Generator-Ausführung.
            // Bei geladenen Saves sind Blöcke absichtlich veränderbar.
            let h0 = world.terrain_height_at(0, 0).clamp(0, WORLD_MAX_Y);
            debug_assert_eq!(world.get_block(IVec3::new(0, h0, 0)), BlockId::Grass);
            if h0 > 0 {
                debug_assert_eq!(world.get_block(IVec3::new(0, h0 - 1, 0)), BlockId::Dirt);
            }
        }
        debug_assert_eq!(
            world.get_block(IVec3::new(-1, WORLD_GRASS_Y, 0)),
            BlockId::Air
        );
        debug_assert_eq!(
            world.set_block(IVec3::new(WORLD_XZ, WORLD_GRASS_Y, 0), BlockId::Grass),
            Err(SetBlockError::OutOfBounds)
        );

        // Für jeden Chunk ein eigenes GPU-Mesh.
        // X/Z decken exakt die 256x256 Weltfläche ab: cx,cz in [0..15] bei CHUNK_SIZE=16.
        // Y-Chunks werden bis WORLD_MAX_Y abgedeckt.
        let mut chunk_meshes: HashMap<ChunkCoord, ChunkMesh> = HashMap::new();
        let max_chunk_y = WORLD_MAX_Y.div_euclid(CHUNK_SIZE);
        for cz in 0..WORLD_CHUNKS_XZ {
            for cx in 0..WORLD_CHUNKS_XZ {
                for cy in 0..=max_chunk_y {
                    let coord = ChunkCoord {
                        x: cx,
                        y: cy,
                        z: cz,
                    };

                    let Some(chunk) = world.chunks.get(&coord) else {
                        continue;
                    };

                    let offset = chunk_offset(coord);
                    let (naive_vertices, naive_indices) =
                        build_chunk_mesh_with_offset(chunk, offset, &block_texture_lookup);
                    let (vertices, indices) = if USE_GREEDY_MESHING {
                        build_chunk_mesh_greedy(chunk, offset, &block_texture_lookup)
                    } else {
                        (naive_vertices.clone(), naive_indices.clone())
                    };

                    println!(
                        "chunk ({},{},{}): naive v/i = {}/{}, greedy v/i = {}/{}, mode={}",
                        coord.x,
                        coord.y,
                        coord.z,
                        naive_vertices.len(),
                        naive_indices.len(),
                        vertices.len(),
                        indices.len(),
                        if USE_GREEDY_MESHING {
                            "greedy"
                        } else {
                            "naive"
                        }
                    );

                    let vertex_buffer =
                        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("Chunk Vertex Buffer"),
                            contents: bytemuck::cast_slice(&vertices),
                            usage: wgpu::BufferUsages::VERTEX,
                        });
                    let index_buffer =
                        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("Chunk Index Buffer"),
                            contents: bytemuck::cast_slice(&indices),
                            usage: wgpu::BufferUsages::INDEX,
                        });

                    chunk_meshes.insert(
                        coord,
                        ChunkMesh {
                            vertex_buffer,
                            index_buffer,
                            index_count: indices.len() as u32,
                        },
                    );
                }
            }
        }

        Self {
            surface,
            device,
            queue,
            config,

            render_pipeline_back_depth,
            render_pipeline_no_cull_depth,
            render_pipeline_back_no_depth,
            render_pipeline_no_cull_no_depth,
            highlight_pipeline,
            ui_pipeline,
            debug_backface_culling: true,
            debug_depth_test: true,
            world,
            chunk_meshes,
            highlight_mesh: None,
            crosshair_mesh,
            preview_mesh,
            preview_block,
            _texture_registry: texture_registry,
            _world_texture_array: world_texture_array,
            world_texture_bind_group,
            block_texture_lookup,
            particles: Vec::new(),
            particle_rng: 0x7A11_9F3D,

            camera_uniform,
            camera_buffer,
            camera_bind_group,

            depth,
        }
    }

    fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        self.config.width = new_size.width;
        self.config.height = new_size.height;
        self.surface.configure(&self.device, &self.config);

        // Depth muss die gleiche Größe wie der Framebuffer haben
        self.depth = DepthTexture::create(&self.device, &self.config);
        self.rebuild_crosshair_mesh();
        self.rebuild_preview_mesh(self.preview_block);
    }

    fn rebuild_crosshair_mesh(&mut self) {
        let (vertices, indices) = build_crosshair_mesh(self.config.width, self.config.height);
        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Crosshair Vertex Buffer"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Crosshair Index Buffer"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        self.crosshair_mesh = ChunkMesh {
            vertex_buffer,
            index_buffer,
            index_count: indices.len() as u32,
        };
    }

    fn rebuild_preview_mesh(&mut self, block: BlockId) {
        let preview_layer = self.block_texture_lookup.layer_for(block, Face::North);
        let (vertices, indices) = build_block_preview_mesh(
            self.config.width,
            self.config.height,
            preview_layer,
            PREVIEW_FLIP_HORIZONTAL,
            PREVIEW_FLIP_VERTICAL,
        );
        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Preview Vertex Buffer"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Preview Index Buffer"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        self.preview_mesh = ChunkMesh {
            vertex_buffer,
            index_buffer,
            index_count: indices.len() as u32,
        };
        self.preview_block = block;
    }

    fn save_world(&self) {
        match self.world.save_to_file(SAVE_PATH) {
            Ok(()) => println!("world saved to {}", SAVE_PATH),
            Err(err) => eprintln!("failed to save world: {}", err),
        }
    }

    fn next_particle_rand(&mut self) -> f32 {
        // xorshift32: einfacher deterministischer PRNG ohne externe Crates.
        let mut x = self.particle_rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.particle_rng = x.max(1);
        (self.particle_rng as f32) / (u32::MAX as f32)
    }

    fn spawn_block_particles(&mut self, block_pos: IVec3, block: BlockId) {
        if !block.is_voxel_filled() {
            return;
        }

        let center = Vec3::new(
            block_pos.x as f32 + 0.5,
            block_pos.y as f32 + 0.5,
            block_pos.z as f32 + 0.5,
        );
        let base = block.color();

        for _ in 0..PARTICLE_SPAWN_COUNT {
            let offset = Vec3::new(
                (self.next_particle_rand() * 2.0 - 1.0) * 0.35,
                self.next_particle_rand() * 0.70 - 0.20,
                (self.next_particle_rand() * 2.0 - 1.0) * 0.35,
            );

            let mut dir = Vec3::new(
                self.next_particle_rand() * 2.0 - 1.0,
                self.next_particle_rand() * 1.35 + 0.25,
                self.next_particle_rand() * 2.0 - 1.0,
            );
            if dir.length_squared() <= 1e-6 {
                dir = Vec3::Y;
            }

            let speed = PARTICLE_SPEED_MIN
                + self.next_particle_rand() * (PARTICLE_SPEED_MAX - PARTICLE_SPEED_MIN);
            let life = PARTICLE_LIFE_MIN
                + self.next_particle_rand() * (PARTICLE_LIFE_MAX - PARTICLE_LIFE_MIN);
            let tint = 0.7 + self.next_particle_rand() * 0.5;
            let color = [
                (base[0] * tint).clamp(0.0, 1.0),
                (base[1] * tint).clamp(0.0, 1.0),
                (base[2] * tint).clamp(0.0, 1.0),
            ];

            self.particles.push(Particle {
                pos: center + offset,
                vel: dir.normalize() * speed,
                life,
                color,
            });
        }

        if self.particles.len() > PARTICLE_MAX_COUNT {
            let overflow = self.particles.len() - PARTICLE_MAX_COUNT;
            self.particles.drain(0..overflow);
        }
    }

    fn update_particles(&mut self, dt: f32) {
        for p in &mut self.particles {
            p.pos += p.vel * dt;
            p.vel.y -= PARTICLE_GRAVITY * dt;
            p.life -= dt;
        }
        self.particles.retain(|p| p.life > 0.0);
    }

    fn build_particle_mesh(&self, camera: &Camera) -> (Vec<Vertex>, Vec<u32>) {
        if self.particles.is_empty() {
            return (Vec::new(), Vec::new());
        }

        // Partikel bleiben in WORLD SPACE gespeichert. Die Kamera wirkt nur über view_proj.
        // Dadurch "folgen" sie korrekt beim Bewegen, statt am Screen zu kleben.
        let mut right = camera.right();
        if right.length_squared() <= 1e-6 {
            right = Vec3::X;
        }
        right = right.normalize() * (PARTICLE_QUAD_SIZE * 0.5);

        let mut up = right.cross(camera.forward());
        if up.length_squared() <= 1e-6 {
            up = Vec3::Y;
        } else {
            up = up.normalize();
        }
        up *= PARTICLE_QUAD_SIZE * 0.5;

        let mut vertices = Vec::with_capacity(self.particles.len() * 4);
        let mut indices = Vec::with_capacity(self.particles.len() * 6);

        for p in &self.particles {
            let p0 = p.pos - right - up;
            let p1 = p.pos + right - up;
            let p2 = p.pos + right + up;
            let p3 = p.pos - right + up;

            let base = vertices.len() as u32;
            vertices.push(Vertex {
                position: p0.to_array(),
                uv: [0.0, 0.0],
                tex_layer: 0,
                tint_kind: TINT_NONE,
                color: p.color,
            });
            vertices.push(Vertex {
                position: p1.to_array(),
                uv: [0.0, 0.0],
                tex_layer: 0,
                tint_kind: TINT_NONE,
                color: p.color,
            });
            vertices.push(Vertex {
                position: p2.to_array(),
                uv: [0.0, 0.0],
                tex_layer: 0,
                tint_kind: TINT_NONE,
                color: p.color,
            });
            vertices.push(Vertex {
                position: p3.to_array(),
                uv: [0.0, 0.0],
                tex_layer: 0,
                tint_kind: TINT_NONE,
                color: p.color,
            });
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }

        (vertices, indices)
    }

    fn rebuild_chunk_mesh(&mut self, coord: ChunkCoord) {
        let Some(chunk) = self.world.chunks.get(&coord) else {
            self.chunk_meshes.remove(&coord);
            return;
        };

        let offset = chunk_offset(coord);
        let (vertices, indices) = if USE_GREEDY_MESHING {
            build_chunk_mesh_greedy(chunk, offset, &self.block_texture_lookup)
        } else {
            build_chunk_mesh_with_offset(chunk, offset, &self.block_texture_lookup)
        };

        if indices.is_empty() {
            self.chunk_meshes.remove(&coord);
            return;
        }

        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Chunk Vertex Buffer"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Chunk Index Buffer"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });

        self.chunk_meshes.insert(
            coord,
            ChunkMesh {
                vertex_buffer,
                index_buffer,
                index_count: indices.len() as u32,
            },
        );
    }

    /// Nach einem Block-Change reicht nicht nur der eigene Chunk:
    /// Liegt der Block auf einer Chunk-Grenze, ändern sich sichtbare Faces im Nachbarchunk.
    fn remesh_after_block_change(&mut self, world_pos: IVec3) {
        let coord = World::world_to_chunk_coord(world_pos);
        let local = World::world_to_local(world_pos);

        let mut affected = vec![coord];
        let push_unique = |coords: &mut Vec<ChunkCoord>, candidate: ChunkCoord| {
            if !coords.contains(&candidate) {
                coords.push(candidate);
            }
        };

        if local.x == 0 {
            push_unique(
                &mut affected,
                ChunkCoord {
                    x: coord.x - 1,
                    y: coord.y,
                    z: coord.z,
                },
            );
        }
        if local.x == CHUNK_SIZE - 1 {
            push_unique(
                &mut affected,
                ChunkCoord {
                    x: coord.x + 1,
                    y: coord.y,
                    z: coord.z,
                },
            );
        }
        if local.y == 0 {
            push_unique(
                &mut affected,
                ChunkCoord {
                    x: coord.x,
                    y: coord.y - 1,
                    z: coord.z,
                },
            );
        }
        if local.y == CHUNK_SIZE - 1 {
            push_unique(
                &mut affected,
                ChunkCoord {
                    x: coord.x,
                    y: coord.y + 1,
                    z: coord.z,
                },
            );
        }
        if local.z == 0 {
            push_unique(
                &mut affected,
                ChunkCoord {
                    x: coord.x,
                    y: coord.y,
                    z: coord.z - 1,
                },
            );
        }
        if local.z == CHUNK_SIZE - 1 {
            push_unique(
                &mut affected,
                ChunkCoord {
                    x: coord.x,
                    y: coord.y,
                    z: coord.z + 1,
                },
            );
        }

        for c in affected {
            self.rebuild_chunk_mesh(c);
        }
    }

    fn destroy_block_at_camera_ray(&mut self, camera: &Camera) {
        let Some(hit) = raycast_world(
            &self.world,
            camera.position,
            camera.forward(),
            RAYCAST_MAX_DISTANCE,
        ) else {
            return;
        };

        let destroyed_block = self.world.get_block(hit.hit_block_pos);
        if cfg!(debug_assertions) {
            println!(
                "[debug] destroy: block={:?} pos=({}, {}, {})",
                destroyed_block, hit.hit_block_pos.x, hit.hit_block_pos.y, hit.hit_block_pos.z
            );
        }
        if self
            .world
            .set_block(hit.hit_block_pos, BlockId::Air)
            .is_ok()
        {
            self.remesh_after_block_change(hit.hit_block_pos);
            self.spawn_block_particles(hit.hit_block_pos, destroyed_block);
        }
    }

    fn place_block_at_camera_ray(&mut self, camera: &Camera, selected_block: BlockId) {
        let Some(hit) = raycast_world(
            &self.world,
            camera.position,
            camera.forward(),
            RAYCAST_MAX_DISTANCE,
        ) else {
            return;
        };

        let place_pos = hit.place_pos();
        if cfg!(debug_assertions) {
            println!(
                "[debug] place: block={:?} pos=({}, {}, {})",
                selected_block, place_pos.x, place_pos.y, place_pos.z
            );
        }

        if self.world.set_block(place_pos, selected_block).is_ok() {
            self.remesh_after_block_change(place_pos);
            // Konsistent zum gesetzten Blocktyp: gleiche BlockId für visuelles Feedback.
            self.spawn_block_particles(place_pos, selected_block);
        }
    }

    fn update_face_highlight(&mut self, camera: &Camera, blink_visible: bool) {
        if !blink_visible {
            self.highlight_mesh = None;
            return;
        }

        let Some(hit) = raycast_world(
            &self.world,
            camera.position,
            camera.forward(),
            RAYCAST_MAX_DISTANCE,
        ) else {
            self.highlight_mesh = None;
            return;
        };

        if hit.hit_normal == IVec3::ZERO {
            self.highlight_mesh = None;
            return;
        }

        let (vertices, indices) =
            build_face_highlight_mesh(hit.hit_block_pos, hit.hit_normal, HIGHLIGHT_FACE_OFFSET);
        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Highlight Vertex Buffer"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Highlight Index Buffer"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });

        self.highlight_mesh = Some(ChunkMesh {
            vertex_buffer,
            index_buffer,
            index_count: indices.len() as u32,
        });
    }

    /// Update: Kamera-Uniform in GPU-Buffer schreiben
    fn update_camera_uniform(&mut self, camera: &Camera) {
        let aspect = (self.config.width as f32 / self.config.height as f32).max(0.0001);
        self.camera_uniform.update(camera, aspect);

        // COPY_DST erlaubt uns, pro Frame per queue.write_buffer zu aktualisieren.
        self.queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::bytes_of(&self.camera_uniform),
        );
    }

    fn render(&mut self, camera: &Camera) -> Result<(), SurfaceError> {
        let frame = self.surface.get_current_texture()?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let particle_mesh = if self.particles.is_empty() {
            None
        } else {
            let (vertices, indices) = self.build_particle_mesh(camera);
            if indices.is_empty() {
                None
            } else {
                let vertex_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("Particle Vertex Buffer"),
                            contents: bytemuck::cast_slice(&vertices),
                            usage: wgpu::BufferUsages::VERTEX,
                        });
                let index_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("Particle Index Buffer"),
                            contents: bytemuck::cast_slice(&indices),
                            usage: wgpu::BufferUsages::INDEX,
                        });
                Some(ChunkMesh {
                    vertex_buffer,
                    index_buffer,
                    index_count: indices.len() as u32,
                })
            }
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Render Encoder"),
            });

        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Main Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.08,
                            g: 0.10,
                            b: 0.14,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth.view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });

            // Erst Welt zeichnen.
            rpass.set_pipeline(self.world_pipeline());
            rpass.set_bind_group(0, &self.world_texture_bind_group, &[]);
            rpass.set_bind_group(1, &self.camera_bind_group, &[]);
            for mesh in self.chunk_meshes.values() {
                rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                rpass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
            }

            // Partikel sind World-Space Billboards und nutzen dieselbe Kamera-Uniform.
            // Keine zusätzliche Kamera-Offset-Logik: view_proj transformiert sie korrekt.
            if let Some(mesh) = &particle_mesh {
                rpass.set_pipeline(&self.highlight_pipeline);
                rpass.set_bind_group(0, &self.camera_bind_group, &[]);
                rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                rpass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
            }

            // Dann das Face-Highlight als Overlay.
            if let Some(mesh) = &self.highlight_mesh {
                rpass.set_pipeline(&self.highlight_pipeline);
                rpass.set_bind_group(0, &self.camera_bind_group, &[]);
                rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                rpass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
            }

            // 2D-UI in NDC: Crosshair bleibt stabil in der Screen-Mitte.
            rpass.set_pipeline(&self.ui_pipeline);
            rpass.set_bind_group(0, &self.world_texture_bind_group, &[]);
            rpass.set_vertex_buffer(0, self.crosshair_mesh.vertex_buffer.slice(..));
            rpass.set_index_buffer(
                self.crosshair_mesh.index_buffer.slice(..),
                wgpu::IndexFormat::Uint32,
            );
            rpass.draw_indexed(0..self.crosshair_mesh.index_count, 0, 0..1);

            rpass.set_vertex_buffer(0, self.preview_mesh.vertex_buffer.slice(..));
            rpass.set_index_buffer(
                self.preview_mesh.index_buffer.slice(..),
                wgpu::IndexFormat::Uint32,
            );
            rpass.draw_indexed(0..self.preview_mesh.index_count, 0, 0..1);
        }

        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }
}

/// ============================
/// App: Fenster + State + Input + Kamera + Timing
/// ============================
struct App {
    window: Option<Window>,
    state: Option<State>,

    player: Player,
    camera: Camera,
    controller: CameraController,
    selected_slot: usize,
    last_space_tap: Option<Instant>,

    last_frame: Option<Instant>,
    app_start: Option<Instant>,
    fps_last_instant: Option<Instant>,
    fps_frame_count: u32,
    fps_value: u32,
}

impl Default for App {
    fn default() -> Self {
        let camera = Camera::default();
        let player = Player::from_eye_position(camera.position);
        Self {
            window: None,
            state: None,
            player,
            camera,
            controller: CameraController::new(),
            selected_slot: 0,
            last_space_tap: None,
            last_frame: None,
            app_start: None,
            fps_last_instant: None,
            fps_frame_count: 0,
            fps_value: 0,
        }
    }
}

impl App {
    fn double_space_window() -> Duration {
        Duration::from_secs_f32(DOUBLE_SPACE_WINDOW)
    }

    fn selected_block(&self) -> BlockId {
        HOTBAR[self.selected_slot]
    }

    fn set_selected_slot(&mut self, selected_slot: usize) -> bool {
        if selected_slot >= HOTBAR.len() || self.selected_slot == selected_slot {
            return false;
        }
        self.selected_slot = selected_slot;
        true
    }

    fn clear_double_space_state(&mut self) {
        self.last_space_tap = None;
    }

    fn toggle_move_mode(&mut self) {
        self.player.mode = match self.player.mode {
            MoveMode::Flight => {
                self.player._velocity_y = 0.0;
                let (snapped, grounded) = if let Some(state) = self.state.as_ref() {
                    snap_to_ground(&state.world, self.player.feet_position)
                } else {
                    (self.player.feet_position, false)
                };
                self.player.feet_position = snapped;
                self.player.on_ground = grounded;
                self.controller.clear_jump_requested();
                MoveMode::Walking
            }
            MoveMode::Walking => {
                self.player._velocity_y = 0.0;
                self.player.on_ground = false;
                self.controller.clear_jump_requested();
                MoveMode::Flight
            }
        };
        self.clear_double_space_state();
        println!(
            "Move mode: {}",
            match self.player.mode {
                MoveMode::Flight => "Flight",
                MoveMode::Walking => "Walking",
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
        if self.player.mode == MoveMode::Walking && self.player.on_ground {
            self.controller.request_jump();
        }
        false
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = event_loop
            .create_window(
                Window::default_attributes().with_title("Minecraft Klon – Würfel + Kamera"),
            )
            .expect("Failed to create window");

        window.set_cursor_visible(true);

        let mut state = pollster::block_on(State::new(&window));
        state.rebuild_preview_mesh(self.selected_block());

        // Spawn in Weltmitte, leicht über Terrain, dann auf Boden aufsetzen.
        let spawn_x = WORLD_XZ / 2;
        let spawn_z = WORLD_XZ / 2;
        let terrain_y = state.world.terrain_height_at(spawn_x, spawn_z) as f32;
        let spawn_feet = Vec3::new(spawn_x as f32 + 0.5, terrain_y + 2.0, spawn_z as f32 + 0.5);
        let (settled_feet, settle_flags) =
            move_with_collisions(&state.world, spawn_feet, Vec3::new(0.0, -4.0, 0.0));
        self.player.feet_position = settled_feet;
        self.player.mode = MoveMode::Flight;
        self.player._velocity_y = 0.0;
        self.player.on_ground = settle_flags.hit_y_down;
        self.camera.position = self.player.eye_position();

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
                if let Some(state) = self.state.as_ref() {
                    state.save_world();
                }
                event_loop.exit();
            }

            WindowEvent::Resized(size) => {
                if let Some(state) = self.state.as_mut() {
                    state.resize(size);
                }
            }

            // Tastatur: WASD/Space/Shift, Double-Space toggelt Flight/Walking, ESC toggelt Mouse Capture
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(KeyCode::Escape) = event.physical_key {
                    if event.state == ElementState::Pressed {
                        let new_state = !self.controller.mouse_captured;
                        if let Some(window) = self.window.as_ref() {
                            self.controller.set_mouse_capture(window, new_state);
                        }
                    }
                }

                let mut consume_space_for_controller = false;
                if event.state == ElementState::Pressed {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        if code == KeyCode::Space && !event.repeat {
                            consume_space_for_controller =
                                self.handle_space_pressed(Instant::now());
                        }

                        if code == KeyCode::F3 && !event.repeat {
                            if let Some(state) = self.state.as_mut() {
                                state.toggle_debug_culling();
                            }
                        }

                        if code == KeyCode::F4 && !event.repeat {
                            if let Some(state) = self.state.as_mut() {
                                state.toggle_debug_depth_test();
                            }
                        }

                        if matches!(code, KeyCode::Enter | KeyCode::NumpadEnter) {
                            if let Some(state) = self.state.as_ref() {
                                state.save_world();
                            }
                        }

                        if !event.repeat {
                            if let Some(next_slot) = hotbar_slot_for_key(code) {
                                if self.set_selected_slot(next_slot) {
                                    let selected = self.selected_block();
                                    println!("Selected block: {}", selected.display_name());
                                    if let Some(state) = self.state.as_mut() {
                                        state.rebuild_preview_mesh(selected);
                                    }
                                }
                            }
                        }
                    }
                }

                if !(consume_space_for_controller
                    && matches!(event.physical_key, PhysicalKey::Code(KeyCode::Space)))
                {
                    self.controller.process_key(&event);
                }
            }

            // rd-132211 Mapping:
            // LMB = platzieren, RMB = abbauen.
            WindowEvent::MouseInput { state, button, .. } => {
                if state == ElementState::Pressed {
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
                                state.place_block_at_camera_ray(&self.camera, selected_block);
                            }
                            MouseButton::Right => {
                                state.destroy_block_at_camera_ray(&self.camera);
                            }
                            _ => {}
                        }
                    }
                }
            }

            // Redraw -> Update (dt, camera), Uniform schreiben, Render
            WindowEvent::RedrawRequested => {
                // dt in Sekunden (Zeit seit letztem Frame)
                let now = Instant::now();
                let dt = self
                    .last_frame
                    .replace(now)
                    .map(|prev| (now - prev).as_secs_f32())
                    .unwrap_or(0.016);
                let current_window_size = self
                    .window
                    .as_ref()
                    .map(|window| window.inner_size())
                    .unwrap_or_default();

                if let Some(state) = self.state.as_mut() {
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
                        }
                        MoveMode::Walking => {
                            // Horizontal (yaw-basiert, ohne pitch).
                            let horizontal_input =
                                self.controller.horizontal_move_input(&self.camera);
                            let desired_h = if horizontal_input.length_squared() > 0.0 {
                                horizontal_input.normalize()
                            } else {
                                Vec3::ZERO
                            };
                            let delta_h = desired_h * WALK_SPEED * dt;

                            if self.player.on_ground {
                                if self.controller.consume_jump_requested() {
                                    self.player._velocity_y = jump_velocity();
                                    self.player.on_ground = false;
                                } else {
                                    self.player._velocity_y = 0.0;
                                }
                            } else {
                                self.player._velocity_y -= GRAVITY * dt;
                                self.controller.clear_jump_requested();
                            }

                            let delta_y = self.player._velocity_y * dt;
                            let desired_delta = Vec3::new(delta_h.x, delta_y, delta_h.z);
                            let (new_feet, collision) = move_with_collisions(
                                &state.world,
                                self.player.feet_position,
                                desired_delta,
                            );
                            self.player.feet_position = new_feet;

                            if collision.hit_y_down {
                                self.player.on_ground = true;
                                self.player._velocity_y = 0.0;
                            } else if collision.hit_y_up {
                                self.player._velocity_y = 0.0;
                                self.player.on_ground = false;
                            } else {
                                if delta_y.abs() <= f32::EPSILON {
                                    let (snapped, grounded) =
                                        snap_to_ground(&state.world, self.player.feet_position);
                                    self.player.feet_position = snapped;
                                    self.player.on_ground = grounded;
                                } else {
                                    self.player.on_ground = false;
                                }
                            }
                        }
                    }
                    self.camera.position = self.player.eye_position();

                    // ViewProjection in Uniform schreiben
                    state.update_camera_uniform(&self.camera);
                    state.update_particles(dt);
                    let blink_visible = self
                        .app_start
                        .map(|start| ((now - start).as_millis() / HIGHLIGHT_BLINK_MS) % 2 == 0)
                        .unwrap_or(true);
                    state.update_face_highlight(&self.camera, blink_visible);

                    // Rendern
                    match state.render(&self.camera) {
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
                        if let Some(window) = self.window.as_ref() {
                            window.set_title(&format!("Minecraft Klon - {} FPS", self.fps_value));
                        }
                    }
                } else {
                    self.fps_last_instant = Some(now);
                }

                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }

            _ => {}
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    run()
}

/// Kleiner Wrapper, damit wir wgpu::util::DeviceExt sauber importieren können.
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = App::default();
    event_loop.run_app(&mut app)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const EPS: f32 = 1e-6;

    #[test]
    fn grass_blockdef_has_correct_face_textures() {
        let registry = BlockRegistry::new();
        assert_eq!(
            registry.texture_key_for(BlockId::Grass, Face::Up),
            TEX_GRASS_TOP
        );
        assert_eq!(
            registry.texture_key_for(BlockId::Grass, Face::Down),
            TEX_DIRT
        );
        assert_eq!(
            registry.texture_key_for(BlockId::Grass, Face::North),
            TEX_GRASS_SIDE
        );
        assert_eq!(
            registry.texture_key_for(BlockId::Grass, Face::South),
            TEX_GRASS_SIDE
        );
        assert_eq!(
            registry.texture_key_for(BlockId::Grass, Face::East),
            TEX_GRASS_SIDE
        );
        assert_eq!(
            registry.texture_key_for(BlockId::Grass, Face::West),
            TEX_GRASS_SIDE
        );
    }

    #[test]
    fn oak_planks_blockdef_has_uniform_face_textures() {
        let registry = BlockRegistry::new();
        for face in [
            Face::Up,
            Face::Down,
            Face::North,
            Face::South,
            Face::East,
            Face::West,
        ] {
            assert_eq!(
                registry.texture_key_for(BlockId::OakPlanks, face),
                TEX_OAK_PLANKS
            );
        }
    }

    #[test]
    fn sapling_uses_cross_plant_model_and_oak_sapling_texture() {
        let registry = BlockRegistry::new();
        assert_eq!(registry.model_for(BlockId::Sapling), BlockModel::CrossPlant);
        assert_eq!(
            registry.texture_key_for(BlockId::Sapling, Face::North),
            TEX_SAPLING
        );
        assert_eq!(TEX_SAPLING, "minecraft:textures/block/oak_sapling.png");
        assert!(!registry.solid_for(BlockId::Sapling));
    }

    #[test]
    fn sapling_cross_plant_mesh_has_two_double_sided_quads() {
        let mut chunk = Chunk::new_empty();
        chunk.set(1, 1, 1, BlockId::Sapling);

        let registry = BlockRegistry::new();
        let mut key_to_layer = HashMap::new();
        for (i, key) in registry.collect_texture_keys().iter().enumerate() {
            key_to_layer.insert(key.clone(), (i as u32) + 1);
        }
        let lookup = BlockTextureLookup::new(&registry, &key_to_layer, 0);
        let (vertices, indices) = build_chunk_mesh_greedy(&chunk, [0.0, 0.0, 0.0], &lookup);

        assert_eq!(vertices.len(), 8);
        assert_eq!(indices.len(), 24);
    }

    #[test]
    fn flight_forward_does_not_change_height_with_pitch() {
        let mut pitched_camera = Camera::new(Vec3::new(128.0, 50.0, 128.0));
        pitched_camera.pitch = 45.0_f32.to_radians();
        let mut player = Player::from_eye_position(pitched_camera.position);
        let world = World::new();
        let start_feet = player.feet_position;

        let mut controller = CameraController::new();
        controller.move_forward = true;
        let delta = controller.flight_move_delta(&pitched_camera, 1.0);
        let (new_feet, _) = move_with_collisions(&world, player.feet_position, delta);
        player.feet_position = new_feet;

        assert!((delta.y).abs() < EPS);
        assert!((player.feet_position.y - start_feet.y).abs() < 1e-4);
        assert!(player.feet_position.z < start_feet.z);
    }

    #[test]
    fn flight_space_and_shift_control_vertical_only() {
        let mut camera = Camera::new(Vec3::new(128.0, 50.0, 128.0));
        camera.pitch = 45.0_f32.to_radians();
        let mut player = Player::from_eye_position(camera.position);
        let world = World::new();

        let mut controller = CameraController::new();
        controller.move_up = true;
        let up_delta = controller.flight_move_delta(&camera, 1.0);
        let (after_up, _) = move_with_collisions(&world, player.feet_position, up_delta);
        player.feet_position = after_up;
        let y_after_up = player.feet_position.y;
        assert!(up_delta.x.abs() < EPS);
        assert!(up_delta.z.abs() < EPS);
        assert!(y_after_up > (camera.position.y - EYE_HEIGHT));

        controller.move_up = false;
        controller.move_down = true;
        let down_delta = controller.flight_move_delta(&camera, 1.0);
        let (after_down, _) = move_with_collisions(&world, player.feet_position, down_delta);
        player.feet_position = after_down;

        assert!(down_delta.x.abs() < EPS);
        assert!(down_delta.z.abs() < EPS);
        assert!(player.feet_position.y < y_after_up);
    }

    #[test]
    fn collision_flags_distinguish_downward_and_upward_y_collisions() {
        let mut world = World::new();
        let _ = world.set_block(IVec3::new(32, 9, 32), BlockId::Stone);
        let _ = world.set_block(IVec3::new(32, 12, 32), BlockId::Stone);
        let start = Vec3::new(32.5, 10.0, 32.5);

        let (_, down_flags) = move_with_collisions(&world, start, Vec3::new(0.0, -2.0, 0.0));
        assert!(down_flags.hit_y_down);
        assert!(!down_flags.hit_y_up);

        let (_, up_flags) = move_with_collisions(&world, start, Vec3::new(0.0, 2.0, 0.0));
        assert!(up_flags.hit_y_up);
        assert!(!up_flags.hit_y_down);
    }

    #[test]
    fn flight_auto_exits_to_walking_on_downward_ground_collision() {
        let mut world = World::new();
        let _ = world.set_block(IVec3::new(32, 9, 32), BlockId::Stone);

        let mut player = Player::from_eye_position(Vec3::new(32.5, 11.62, 32.5));
        player.mode = MoveMode::Flight;
        let camera = Camera::new(player.eye_position());
        let mut controller = CameraController::new();
        controller.move_down = true;
        let mut last_space_tap = Some(Instant::now());

        update_flight_movement(
            &world,
            &camera,
            &mut controller,
            &mut player,
            &mut last_space_tap,
            0.2,
        );

        assert_eq!(player.mode, MoveMode::Walking);
        assert!(player.on_ground);
        assert!(player._velocity_y.abs() < EPS);
        assert!(last_space_tap.is_none());
    }

    #[test]
    fn flight_side_or_ceiling_collision_does_not_auto_exit() {
        // Seitenkontakt in Flight darf keinen Modewechsel auslösen.
        let mut side_world = World::new();
        let _ = side_world.set_block(IVec3::new(32, 10, 31), BlockId::Stone);
        let mut side_player = Player::from_eye_position(Vec3::new(32.5, 11.62, 32.5));
        side_player.mode = MoveMode::Flight;
        let side_camera = Camera::new(side_player.eye_position());
        let mut side_controller = CameraController::new();
        side_controller.move_forward = true;
        let mut side_last_tap = Some(Instant::now());
        let side_delta = side_controller.flight_move_delta(&side_camera, 0.2);
        let (_, side_flags) =
            move_with_collisions(&side_world, side_player.feet_position, side_delta);
        assert!(side_flags.hit_x || side_flags.hit_z);
        assert!(!side_flags.hit_y_down);
        update_flight_movement(
            &side_world,
            &side_camera,
            &mut side_controller,
            &mut side_player,
            &mut side_last_tap,
            0.2,
        );
        assert_eq!(side_player.mode, MoveMode::Flight);

        // Kollision nach +Y (Decke) darf ebenfalls keinen Modewechsel auslösen.
        let mut ceiling_world = World::new();
        let _ = ceiling_world.set_block(IVec3::new(32, 12, 32), BlockId::Stone);
        let mut ceiling_player = Player::from_eye_position(Vec3::new(32.5, 11.62, 32.5));
        ceiling_player.mode = MoveMode::Flight;
        let ceiling_camera = Camera::new(ceiling_player.eye_position());
        let mut ceiling_controller = CameraController::new();
        ceiling_controller.move_up = true;
        let mut ceiling_last_tap = Some(Instant::now());
        let ceiling_delta = ceiling_controller.flight_move_delta(&ceiling_camera, 0.2);
        let (_, ceiling_flags) =
            move_with_collisions(&ceiling_world, ceiling_player.feet_position, ceiling_delta);
        assert!(ceiling_flags.hit_y_up);
        assert!(!ceiling_flags.hit_y_down);
        update_flight_movement(
            &ceiling_world,
            &ceiling_camera,
            &mut ceiling_controller,
            &mut ceiling_player,
            &mut ceiling_last_tap,
            0.2,
        );
        assert_eq!(ceiling_player.mode, MoveMode::Flight);
    }

    #[test]
    fn movement_collides_and_slides_against_solid_voxels() {
        let mut world = World::new();
        let _ = world.set_block(IVec3::new(130, 20, 128), BlockId::Stone);

        let start = Vec3::new(128.0, 20.0, 128.0);
        let delta = Vec3::new(4.0, 0.0, 1.5);
        let (end, flags) = move_with_collisions(&world, start, delta);

        assert!(flags.hit_x);
        assert!(!flags.hit_z);
        // Block bei x=130 belegt [130..131); AABB.max.x muss davor bleiben.
        assert!(end.x + PLAYER_RADIUS <= 130.0 + 1e-3);
        assert!(end.z > start.z);
    }

    #[test]
    fn out_of_bounds_counts_as_solid_for_collisions() {
        let world = World::new();
        assert!(world.is_solid_block_at(-1, 10, 10));
        assert!(world.is_solid_block_at(10, -1, 10));
        assert!(world.is_solid_block_at(10, 10, WORLD_XZ));
    }

    #[test]
    fn sapling_is_not_collision_solid() {
        assert!(!BlockId::Sapling.is_solid());
    }

    #[test]
    fn oak_planks_is_collision_solid() {
        assert!(BlockId::OakPlanks.is_solid());
    }

    #[test]
    fn double_space_toggles_flight_to_walking() {
        let mut app = App::default();
        app.player.mode = MoveMode::Flight;
        let t0 = Instant::now();
        assert!(!app.handle_space_pressed(t0));
        assert!(app.handle_space_pressed(t0 + Duration::from_millis(100)));
        assert_eq!(app.player.mode, MoveMode::Walking);
    }

    #[test]
    fn double_space_in_walking_toggles_on_second_press_without_extra_jump() {
        let mut app = App::default();
        app.player.mode = MoveMode::Walking;
        app.player.on_ground = true;
        let t0 = Instant::now();
        assert!(!app.handle_space_pressed(t0));
        // First press keeps normal jump behavior.
        assert!(app.controller.consume_jump_requested());
        assert!(app.handle_space_pressed(t0 + Duration::from_millis(100)));
        assert_eq!(app.player.mode, MoveMode::Flight);
        // Second press only toggles; no additional jump request.
        assert!(!app.controller.consume_jump_requested());
    }

    #[test]
    fn single_space_in_walking_triggers_immediate_jump_request() {
        let mut app = App::default();
        app.player.mode = MoveMode::Walking;
        app.player.on_ground = true;
        let t0 = Instant::now();
        assert!(!app.handle_space_pressed(t0));
        assert!(app.controller.consume_jump_requested());
    }

    #[test]
    fn digit6_maps_to_slot6_and_slot6_is_sapling() {
        assert_eq!(hotbar_slot_for_key(KeyCode::Digit6), Some(5));
        assert_eq!(HOTBAR[5], BlockId::Sapling);
        assert!(HOTBAR.contains(&BlockId::OakPlanks));
    }

    #[test]
    fn preview_uv_flip_mirrors_horizontally_without_moving_quad() {
        let (normal, _) = build_block_preview_mesh(1280, 720, 0, false, false);
        let (flipped_x, _) = build_block_preview_mesh(1280, 720, 0, true, false);

        // Flipped Preview bleibt an gleicher Position, spiegelt aber U.
        assert_eq!(normal[0].position, flipped_x[0].position);
        assert!((normal[0].uv[0] - 0.0).abs() < EPS);
        assert!((flipped_x[0].uv[0] - 1.0).abs() < EPS);
        assert!((normal[1].uv[0] - 1.0).abs() < EPS);
        assert!((flipped_x[1].uv[0] - 0.0).abs() < EPS);
        assert!((normal[0].uv[1] - flipped_x[0].uv[1]).abs() < EPS);
    }

    #[test]
    fn load_reports_expected_and_got_on_dimension_mismatch() {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "mc_klon_dim_mismatch_{}_{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time before unix epoch")
                .as_nanos()
        );
        path.push(unique);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&SAVE_MAGIC);
        bytes.extend_from_slice(&SAVE_VERSION.to_le_bytes());
        bytes.extend_from_slice(&((WORLD_XZ + 1) as u32).to_le_bytes());
        bytes.extend_from_slice(&(WORLD_MAX_Y as u32).to_le_bytes());
        std::fs::write(&path, &bytes).expect("failed to write temp save");

        let err = match World::load_from_file(path.to_str().expect("non-utf8 temp path")) {
            Ok(_) => panic!("load should fail for dimensions mismatch"),
            Err(err) => err,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("save dimensions mismatch"),
            "unexpected error message: {}",
            msg
        );
        assert!(
            msg.contains("expected WORLD_XZ"),
            "expected details missing: {}",
            msg
        );
        assert!(msg.contains("got WORLD_XZ"), "got details missing: {}", msg);

        let _ = std::fs::remove_file(path);
    }
}
