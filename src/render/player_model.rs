//! Full player skin model for the third-person views.
//!
//! Classic 64x64 Steve layout, built from six boxes (head, body, two arms, two
//! legs). One model pixel is 1/16 of a block, so the model is 32 px = 2 blocks
//! tall, matching vanilla proportions.
//!
//! The mesh is produced in *world space* directly: it is rebuilt every frame
//! anyway (the player moves and the limbs swing), so a per-frame model matrix
//! would buy nothing.

use glam::Vec3;

use crate::config::render::UI_MODE_TEXTURED;
use crate::meshing::Vertex;

/// Skin texture is a square 64x64 sheet.
const SKIN_PX: f32 = 64.0;
/// Model pixel -> world blocks.
const PX: f32 = 1.0 / 16.0;

const HEAD_SIZE: f32 = 8.0;
const BODY_W: f32 = 8.0;
const BODY_H: f32 = 12.0;
const BODY_D: f32 = 4.0;
const LIMB_W: f32 = 4.0;
const LIMB_H: f32 = 12.0;
const LIMB_D: f32 = 4.0;
/// Body top / shoulder line / hip line, in model pixels above the feet.
const BODY_TOP_PX: f32 = 24.0;
const SHOULDER_PX: f32 = 22.0;
const HIP_PX: f32 = 12.0;
/// How far the upper body drops when sneaking.
const SNEAK_DROP_PX: f32 = 3.0;

/// Peak limb swing at full walking speed.
const LIMB_SWING_MAX_RAD: f32 = 0.9;
/// Head turn limits relative to the body, so the neck cannot spin around.
const HEAD_YAW_CLAMP_RAD: f32 = 1.3;
const HEAD_PITCH_CLAMP_RAD: f32 = 1.4;

/// Everything the model needs for one frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PlayerModelView {
    /// World position of the player's feet.
    pub feet: Vec3,
    /// Direction the body faces, in the same convention as `Camera::yaw`.
    pub body_yaw: f32,
    /// Where the player is looking; the head turns towards it.
    pub look_yaw: f32,
    pub look_pitch: f32,
    /// Walk cycle phase in radians, shared with the view bobbing stride.
    pub stride_phase: f32,
    /// 0..1 blend of the limb swing, so standing still means a still model.
    pub stride_intensity: f32,
    /// Whether the player is sneaking (upper body drops).
    pub sneaking: bool,
    /// Per-vertex brightness baked from the world light at the player's
    /// position; the overlay shader has no lighting of its own.
    pub brightness: f32,
    pub skin_layer: u32,
}

/// The six faces of one skin box, as texel rects on the 64x64 sheet.
#[derive(Clone, Copy)]
struct BoxUv {
    /// x, y of the box's texture "cross" origin.
    origin: [f32; 2],
    /// Box size in model pixels (w, h, d).
    size: [f32; 3],
}

impl BoxUv {
    /// Vanilla unwraps a box as: top/bottom on the upper row (offset by depth),
    /// then right/front/left/back side by side on the row below.
    fn faces(self) -> SkinFaceUvs {
        let [ox, oy] = self.origin;
        let [w, h, d] = self.size;
        SkinFaceUvs {
            top: rect(ox + d, oy, w, d),
            bottom: rect(ox + d + w, oy, w, d),
            right: rect(ox, oy + d, d, h),
            front: rect(ox + d, oy + d, w, h),
            left: rect(ox + d + w, oy + d, d, h),
            back: rect(ox + d + w + d, oy + d, w, h),
        }
    }
}

/// Texel rect on the skin sheet: `(uv_min, uv_max)`.
type UvRect = ([f32; 2], [f32; 2]);

#[derive(Clone, Copy)]
struct SkinFaceUvs {
    top: UvRect,
    bottom: UvRect,
    right: UvRect,
    front: UvRect,
    left: UvRect,
    back: UvRect,
}

/// Half a texel of inset, so neighbouring skin regions never bleed in.
fn rect(x: f32, y: f32, w: f32, h: f32) -> UvRect {
    let inset = 0.5 / SKIN_PX;
    (
        [
            (x / SKIN_PX + inset).clamp(0.0, 1.0),
            (y / SKIN_PX + inset).clamp(0.0, 1.0),
        ],
        [
            ((x + w) / SKIN_PX - inset).clamp(0.0, 1.0),
            ((y + h) / SKIN_PX - inset).clamp(0.0, 1.0),
        ],
    )
}

fn uv_quad(uv: UvRect) -> [[f32; 2]; 4] {
    let (min, max) = uv;
    [
        [min[0], max[1]],
        [max[0], max[1]],
        [max[0], min[1]],
        [min[0], min[1]],
    ]
}

/// Where the model's local axes point in the world.
///
/// Derived from the engine's own convention (`Camera::forward_flat` /
/// `right_flat`) so the model faces exactly where the player looks:
/// local `-Z` is forward, local `+X` is the player's right.
#[derive(Clone, Copy)]
struct BodyBasis {
    forward: Vec3,
    right: Vec3,
}

impl BodyBasis {
    fn from_yaw(body_yaw: f32) -> Self {
        let (sin_yaw, cos_yaw) = body_yaw.sin_cos();
        Self {
            forward: Vec3::new(cos_yaw, 0.0, sin_yaw),
            right: Vec3::new(-sin_yaw, 0.0, cos_yaw),
        }
    }

    /// Model pixels -> world blocks.
    fn to_world(self, model_px: Vec3, feet: Vec3) -> Vec3 {
        let scaled = model_px * PX;
        feet + self.right * scaled.x + Vec3::Y * scaled.y - self.forward * scaled.z
    }
}

/// A box placed in the model's local frame: `pivot` is the point it rotates
/// around, `min`/`max` are its corners relative to that pivot (model pixels).
#[derive(Clone, Copy)]
struct ModelBox {
    pivot: Vec3,
    min: Vec3,
    max: Vec3,
    uv: BoxUv,
    /// Rotation around the pivot's local X axis (limb swing), in radians.
    pitch: f32,
    /// Extra yaw around the pivot (head turn), in radians.
    yaw: f32,
}

pub(crate) fn build_player_model_mesh(view: PlayerModelView) -> (Vec<Vertex>, Vec<u32>) {
    let swing = limb_swing(view);
    let sneak_drop = if view.sneaking { -SNEAK_DROP_PX } else { 0.0 };

    // Head tracking is relative to the body, clamped so it cannot spin around.
    let head_yaw =
        wrap_pi(view.look_yaw - view.body_yaw).clamp(-HEAD_YAW_CLAMP_RAD, HEAD_YAW_CLAMP_RAD);
    let head_pitch = view
        .look_pitch
        .clamp(-HEAD_PITCH_CLAMP_RAD, HEAD_PITCH_CLAMP_RAD);

    // Vertical layout matches vanilla: legs 0..12, body 12..24, head 24..32,
    // shoulders at 22 — 32 px total, i.e. exactly two blocks.
    let boxes = [
        // Head pivots on the neck, so looking up/down rotates it in place.
        ModelBox {
            pivot: Vec3::new(0.0, BODY_TOP_PX + sneak_drop, 0.0),
            min: Vec3::new(-HEAD_SIZE * 0.5, 0.0, -HEAD_SIZE * 0.5),
            max: Vec3::new(HEAD_SIZE * 0.5, HEAD_SIZE, HEAD_SIZE * 0.5),
            uv: BoxUv {
                origin: [0.0, 0.0],
                size: [HEAD_SIZE, HEAD_SIZE, HEAD_SIZE],
            },
            pitch: head_pitch,
            yaw: head_yaw,
        },
        ModelBox {
            pivot: Vec3::new(0.0, BODY_TOP_PX + sneak_drop, 0.0),
            min: Vec3::new(-BODY_W * 0.5, -BODY_H, -BODY_D * 0.5),
            max: Vec3::new(BODY_W * 0.5, 0.0, BODY_D * 0.5),
            uv: BoxUv {
                origin: [16.0, 16.0],
                size: [BODY_W, BODY_H, BODY_D],
            },
            pitch: 0.0,
            yaw: 0.0,
        },
        // Right arm hangs off the +X shoulder, flush with the body edge.
        ModelBox {
            pivot: Vec3::new(BODY_W * 0.5, SHOULDER_PX + sneak_drop, 0.0),
            min: Vec3::new(0.0, -LIMB_H, -LIMB_D * 0.5),
            max: Vec3::new(LIMB_W, 0.0, LIMB_D * 0.5),
            uv: BoxUv {
                origin: [40.0, 16.0],
                size: [LIMB_W, LIMB_H, LIMB_D],
            },
            pitch: swing,
            yaw: 0.0,
        },
        // Left arm, swinging opposite the right one.
        ModelBox {
            pivot: Vec3::new(-BODY_W * 0.5, SHOULDER_PX + sneak_drop, 0.0),
            min: Vec3::new(-LIMB_W, -LIMB_H, -LIMB_D * 0.5),
            max: Vec3::new(0.0, 0.0, LIMB_D * 0.5),
            uv: BoxUv {
                origin: [32.0, 48.0],
                size: [LIMB_W, LIMB_H, LIMB_D],
            },
            pitch: -swing,
            yaw: 0.0,
        },
        // Legs pivot at the hips and keep their feet on the ground when sneaking.
        ModelBox {
            pivot: Vec3::new(LIMB_W * 0.5, HIP_PX, 0.0),
            min: Vec3::new(-LIMB_W * 0.5, -LIMB_H, -LIMB_D * 0.5),
            max: Vec3::new(LIMB_W * 0.5, 0.0, LIMB_D * 0.5),
            uv: BoxUv {
                origin: [0.0, 16.0],
                size: [LIMB_W, LIMB_H, LIMB_D],
            },
            pitch: -swing,
            yaw: 0.0,
        },
        ModelBox {
            pivot: Vec3::new(-LIMB_W * 0.5, HIP_PX, 0.0),
            min: Vec3::new(-LIMB_W * 0.5, -LIMB_H, -LIMB_D * 0.5),
            max: Vec3::new(LIMB_W * 0.5, 0.0, LIMB_D * 0.5),
            uv: BoxUv {
                origin: [16.0, 48.0],
                size: [LIMB_W, LIMB_H, LIMB_D],
            },
            pitch: swing,
            yaw: 0.0,
        },
    ];

    let mut vertices = Vec::with_capacity(boxes.len() * 24);
    let mut indices = Vec::with_capacity(boxes.len() * 36);
    let basis = BodyBasis::from_yaw(view.body_yaw);
    for model_box in boxes {
        append_box(&mut vertices, &mut indices, model_box, view, basis);
    }
    (vertices, indices)
}

/// Darkest the model gets in an unlit cave; matches the terrain's floor closely
/// enough that the player never turns fully black.
const MIN_BRIGHTNESS: f32 = 0.2;

/// Folds a packed light value (high nibble sky, low nibble block) and the
/// current sky brightness into a single vertex shade.
///
/// The overlay pipeline the model shares with the first-person arm has no
/// lighting stage, so this is baked on the CPU instead. It approximates the
/// terrain shader rather than reproducing it: no fog, no per-face shading.
pub(crate) fn brightness_from_light(light_packed: u8, sky_brightness: f32) -> f32 {
    let sky = f32::from((light_packed >> 4) & 0xF) / 15.0;
    let block = f32::from(light_packed & 0xF) / 15.0;
    let lit = (sky * sky_brightness.clamp(0.0, 1.0)).max(block);
    MIN_BRIGHTNESS + (1.0 - MIN_BRIGHTNESS) * lit.clamp(0.0, 1.0)
}

/// Limb swing angle for this frame. Scales with how hard the player is walking,
/// so a standing player is perfectly still.
fn limb_swing(view: PlayerModelView) -> f32 {
    view.stride_phase.sin() * LIMB_SWING_MAX_RAD * view.stride_intensity.clamp(0.0, 1.0)
}

fn wrap_pi(angle: f32) -> f32 {
    if !angle.is_finite() {
        return 0.0;
    }
    (angle + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
}

fn append_box(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    model_box: ModelBox,
    view: PlayerModelView,
    basis: BodyBasis,
) {
    let uvs = model_box.uv.faces();
    let (min, max) = (model_box.min, model_box.max);
    let shade = view.brightness.clamp(0.0, 1.0);

    let corner = |x: f32, y: f32, z: f32| {
        let local = rotate_local(Vec3::new(x, y, z), model_box.pitch, model_box.yaw);
        basis
            .to_world(local + model_box.pivot, view.feet)
            .to_array()
    };

    let (x0, y0, z0) = (min.x, min.y, min.z);
    let (x1, y1, z1) = (max.x, max.y, max.z);

    // Local -Z is the facing direction, so the skin's "front" cell goes on the
    // min-Z face; +X is the player's right, so "right" goes on the max-X face.
    // Winding is counter-clockwise seen from outside the box.
    let faces: [([[f32; 3]; 4], UvRect); 6] = [
        (
            [
                corner(x1, y0, z0),
                corner(x0, y0, z0),
                corner(x0, y1, z0),
                corner(x1, y1, z0),
            ],
            uvs.front,
        ),
        (
            [
                corner(x0, y0, z1),
                corner(x1, y0, z1),
                corner(x1, y1, z1),
                corner(x0, y1, z1),
            ],
            uvs.back,
        ),
        (
            [
                corner(x1, y0, z1),
                corner(x1, y0, z0),
                corner(x1, y1, z0),
                corner(x1, y1, z1),
            ],
            uvs.right,
        ),
        (
            [
                corner(x0, y0, z0),
                corner(x0, y0, z1),
                corner(x0, y1, z1),
                corner(x0, y1, z0),
            ],
            uvs.left,
        ),
        (
            [
                corner(x0, y1, z0),
                corner(x1, y1, z0),
                corner(x1, y1, z1),
                corner(x0, y1, z1),
            ],
            uvs.top,
        ),
        (
            [
                corner(x0, y0, z1),
                corner(x1, y0, z1),
                corner(x1, y0, z0),
                corner(x0, y0, z0),
            ],
            uvs.bottom,
        ),
    ];

    for (corners, uv) in faces {
        let base = vertices.len() as u32;
        let quad = uv_quad(uv);
        for i in 0..4 {
            vertices.push(Vertex {
                position: corners[i],
                uv: quad[i],
                tex_layer: view.skin_layer,
                // The 3D overlay shader reuses this field as its draw mode;
                // anything but `UI_MODE_TEXTURED` renders flat white.
                tint_kind: UI_MODE_TEXTURED,
                color: [shade, shade, shade],
                light_packed: 0,
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

/// Rotates a corner around its box pivot: pitch around X (limb swing), then yaw
/// around Y (head turn).
fn rotate_local(local: Vec3, pitch: f32, yaw: f32) -> Vec3 {
    let (sp, cp) = pitch.sin_cos();
    let pitched = Vec3::new(
        local.x,
        local.y * cp - local.z * sp,
        local.y * sp + local.z * cp,
    );
    if yaw == 0.0 {
        return pitched;
    }
    let (sy, cy) = yaw.sin_cos();
    Vec3::new(
        pitched.x * cy + pitched.z * sy,
        pitched.y,
        -pitched.x * sy + pitched.z * cy,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> PlayerModelView {
        PlayerModelView {
            feet: Vec3::new(3.0, 64.0, -7.0),
            body_yaw: 0.0,
            look_yaw: 0.0,
            look_pitch: 0.0,
            stride_phase: 0.0,
            stride_intensity: 0.0,
            sneaking: false,
            brightness: 1.0,
            skin_layer: 2,
        }
    }

    fn bounds(vertices: &[Vertex], axis: usize) -> (f32, f32) {
        vertices.iter().fold((f32::MAX, f32::MIN), |(lo, hi), v| {
            (lo.min(v.position[axis]), hi.max(v.position[axis]))
        })
    }

    #[test]
    fn builds_six_boxes() {
        let (vertices, indices) = build_player_model_mesh(view());
        assert_eq!(vertices.len(), 6 * 24, "six boxes with four verts per face");
        assert_eq!(indices.len(), 6 * 36);
        assert!(vertices.iter().all(|v| v.tex_layer == 2));
    }

    #[test]
    fn model_stands_on_the_feet_position_and_is_two_blocks_tall() {
        let view = view();
        let (vertices, _) = build_player_model_mesh(view);
        let (min_y, max_y) = bounds(&vertices, 1);
        assert!((min_y - view.feet.y).abs() < 1e-4, "feet at {min_y}");
        assert!(
            (max_y - (view.feet.y + 2.0)).abs() < 1e-4,
            "head top at {max_y}"
        );
    }

    /// The model must face where the camera looks. `Camera::forward_flat` is
    /// `(cos yaw, 0, sin yaw)`, so the chest (local -Z) has to land on that side.
    #[test]
    fn body_faces_the_camera_yaw() {
        for yaw in [0.0_f32, 0.9, 2.5, -1.7, std::f32::consts::PI] {
            let mut view = view();
            view.body_yaw = yaw;
            view.look_yaw = yaw;
            let basis = BodyBasis::from_yaw(yaw);
            let forward_flat = Vec3::new(yaw.cos(), 0.0, yaw.sin());
            assert!(
                (basis.forward - forward_flat).length() < 1e-5,
                "basis forward mismatch at yaw {yaw}"
            );

            // Chest centre, one model pixel in front of the body's front face.
            let chest = basis.to_world(Vec3::new(0.0, 18.0, -BODY_D * 0.5), view.feet);
            let offset = chest - view.feet;
            let horizontal = Vec3::new(offset.x, 0.0, offset.z).normalize();
            assert!(
                (horizontal - forward_flat).length() < 1e-4,
                "chest points {horizontal:?} instead of {forward_flat:?} at yaw {yaw}"
            );
        }
    }

    /// `Camera::right_flat` is `(-sin yaw, 0, cos yaw)`; the right arm must be
    /// on that side, or the skin's arms end up mirrored.
    #[test]
    fn right_arm_is_on_the_players_right() {
        let yaw = 0.6_f32;
        let mut view = view();
        view.body_yaw = yaw;
        let basis = BodyBasis::from_yaw(yaw);
        let right_flat = Vec3::new(-yaw.sin(), 0.0, yaw.cos());
        assert!((basis.right - right_flat).length() < 1e-5);

        // Centre of the right arm box, taken from the model layout above.
        let arm = basis.to_world(
            Vec3::new(BODY_W * 0.5 + LIMB_W * 0.5, SHOULDER_PX - LIMB_H * 0.5, 0.0),
            view.feet,
        );
        let offset = arm - view.feet;
        assert!(
            Vec3::new(offset.x, 0.0, offset.z).dot(right_flat) > 0.0,
            "right arm ended up on the left"
        );
    }

    #[test]
    fn body_yaw_rotates_the_model_around_the_feet() {
        let mut turned = view();
        turned.body_yaw = std::f32::consts::FRAC_PI_2;
        let (vertices, _) = build_player_model_mesh(turned);
        let (min_x, max_x) = bounds(&vertices, 0);
        let center_x = (min_x + max_x) * 0.5;
        assert!(
            (center_x - turned.feet.x).abs() < 1e-4,
            "center at {center_x}"
        );
    }

    #[test]
    fn standing_still_means_no_limb_swing() {
        let mut moving = view();
        moving.stride_phase = 1.0;
        moving.stride_intensity = 0.0;
        assert!(limb_swing(moving).abs() < 1e-6);

        moving.stride_intensity = 1.0;
        assert!(limb_swing(moving).abs() > 0.1);
    }

    #[test]
    fn sneaking_lowers_the_upper_body_but_not_the_feet() {
        let mut sneaking = view();
        sneaking.sneaking = true;
        let (upright, _) = build_player_model_mesh(view());
        let (crouched, _) = build_player_model_mesh(sneaking);

        let (upright_min, upright_max) = bounds(&upright, 1);
        let (crouched_min, crouched_max) = bounds(&crouched, 1);
        assert!((upright_min - crouched_min).abs() < 1e-4, "feet moved");
        assert!(crouched_max < upright_max, "head did not drop");
        assert!((upright_max - crouched_max - SNEAK_DROP_PX * PX).abs() < 1e-4);
    }

    /// Regression: the model rendered flat white because `tint_kind` doubles as
    /// the overlay shader's draw mode and `UI_MODE_SOLID` (0) ignores the texture.
    #[test]
    fn vertices_select_the_textured_overlay_mode() {
        let (vertices, _) = build_player_model_mesh(view());
        assert!(vertices.iter().all(|v| v.tint_kind == UI_MODE_TEXTURED));
    }

    #[test]
    fn brightness_tracks_the_world_light() {
        // Unlit cave: floored, never fully black.
        let dark = brightness_from_light(0x00, 1.0);
        assert!((dark - MIN_BRIGHTNESS).abs() < 1e-6);
        // Full sky light at noon.
        assert!((brightness_from_light(0xF0, 1.0) - 1.0).abs() < 1e-6);
        // The same spot at night is dim, because sky light is scaled...
        assert!(brightness_from_light(0xF0, 0.0) < dark + 1e-6);
        // ...but a torch (block light) stays bright regardless of time of day.
        assert!((brightness_from_light(0x0F, 0.0) - 1.0).abs() < 1e-6);
    }

    /// Regression: the head pitched against the mouse. Looking up (positive
    /// camera pitch) has to raise the front of the head, not lower it.
    #[test]
    fn head_pitches_with_the_look_direction() {
        let front_height = |look_pitch: f32| {
            let mut view = view();
            view.look_pitch = look_pitch;
            let (vertices, _) = build_player_model_mesh(view);
            // The head is the first box; its front face is the first quad.
            vertices[..4]
                .iter()
                .map(|v| v.position[1])
                .fold(f32::MIN, f32::max)
        };

        let level = front_height(0.0);
        let looking_up = front_height(0.6);
        let looking_down = front_height(-0.6);
        assert!(looking_up > level, "looking up did not raise the face");
        assert!(looking_down < level, "looking down did not lower the face");
    }

    #[test]
    fn head_turn_is_clamped_relative_to_the_body() {
        assert!((wrap_pi(std::f32::consts::TAU + 0.25) - 0.25).abs() < 1e-5);
        assert!((wrap_pi(-std::f32::consts::TAU - 0.25) + 0.25).abs() < 1e-5);
        // A look direction fully behind the body clamps instead of wrapping the neck.
        let behind =
            wrap_pi(std::f32::consts::PI - 0.01).clamp(-HEAD_YAW_CLAMP_RAD, HEAD_YAW_CLAMP_RAD);
        assert!((behind - HEAD_YAW_CLAMP_RAD).abs() < 1e-5);
    }
}
