use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3, Vec4};

use crate::config::render::{
    FOG_COLOR_BLEND_DEFAULT, FOG_COLOR_DEFAULT, FOG_DARKEN_STRENGTH, FOG_END_DISTANCE,
    FOG_START_DISTANCE, LAVA_FOG_COLOR, LAVA_FOG_COLOR_BLEND, LAVA_FOG_DARKEN_STRENGTH,
    LAVA_FOG_END_DISTANCE, LAVA_FOG_START_DISTANCE, WATER_FOG_COLOR, WATER_FOG_COLOR_BLEND,
    WATER_FOG_DARKEN_STRENGTH, WATER_FOG_END_DISTANCE, WATER_FOG_START_DISTANCE,
};
use crate::world::fluids::FluidKind;

// glam::perspective_rh erzeugt OpenGL-Clipspace-Z in [-1..1].
// wgpu erwartet Z in [0..1], daher explizite Konvertierung.
const OPENGL_TO_WGPU_MATRIX: Mat4 = Mat4::from_cols_array(&[
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.5, 1.0,
]);

#[derive(Clone, Copy, Debug)]
pub(crate) struct Frustum {
    pub(crate) planes: [Vec4; 6],
}

fn mat4_row(m: Mat4, row: usize) -> Vec4 {
    match row {
        0 => Vec4::new(m.x_axis.x, m.y_axis.x, m.z_axis.x, m.w_axis.x),
        1 => Vec4::new(m.x_axis.y, m.y_axis.y, m.z_axis.y, m.w_axis.y),
        2 => Vec4::new(m.x_axis.z, m.y_axis.z, m.z_axis.z, m.w_axis.z),
        3 => Vec4::new(m.x_axis.w, m.y_axis.w, m.z_axis.w, m.w_axis.w),
        _ => unreachable!("row must be in 0..=3"),
    }
}

fn normalize_plane(plane: Vec4) -> Vec4 {
    let normal = plane.truncate();
    let len = normal.length();
    if len > 0.0 { plane / len } else { plane }
}

impl Frustum {
    pub(crate) fn from_view_proj(view_proj: Mat4) -> Self {
        // D3D/wgpu clip-space: x,y in [-w,+w], z in [0,+w]
        let r0 = mat4_row(view_proj, 0);
        let r1 = mat4_row(view_proj, 1);
        let r2 = mat4_row(view_proj, 2);
        let r3 = mat4_row(view_proj, 3);

        let planes = [
            normalize_plane(r3 + r0), // left
            normalize_plane(r3 - r0), // right
            normalize_plane(r3 + r1), // bottom
            normalize_plane(r3 - r1), // top
            normalize_plane(r2),      // near
            normalize_plane(r3 - r2), // far
        ];
        Self { planes }
    }
}

pub(crate) fn build_view_proj_wgpu(camera: &Camera, aspect: f32) -> Mat4 {
    let view = camera.view_matrix();
    let proj = camera.proj_matrix(aspect);
    OPENGL_TO_WGPU_MATRIX * proj * view
}

pub(crate) fn sphere_in_frustum(planes: &[Vec4; 6], center: Vec3, radius: f32) -> bool {
    let c = center.extend(1.0);
    for plane in planes {
        if plane.dot(c) < -radius {
            return false;
        }
    }
    true
}

pub(crate) fn aabb_in_frustum(planes: &[Vec4; 6], min: Vec3, max: Vec3) -> bool {
    for plane in planes {
        let normal = plane.truncate();
        let d = plane.w;

        // Get the point of the AABB that is furthest along the plane normal direction
        let p = Vec3::new(
            if normal.x >= 0.0 { max.x } else { min.x },
            if normal.y >= 0.0 { max.y } else { min.y },
            if normal.z >= 0.0 { max.z } else { min.z },
        );

        // If this furthest point is behind the plane, AABB is completely outside
        if normal.dot(p) + d < 0.0 {
            return false;
        }
    }
    true
}

pub(crate) fn chunk_in_frustum(frustum: &Frustum, chunk_coord: crate::world::ChunkCoord) -> bool {
    const CHUNK_SIZE: f32 = crate::world::CHUNK_SIZE as f32;

    let min = Vec3::new(
        chunk_coord.x as f32 * CHUNK_SIZE,
        chunk_coord.y as f32 * CHUNK_SIZE,
        chunk_coord.z as f32 * CHUNK_SIZE,
    );
    let max = min + Vec3::splat(CHUNK_SIZE);

    aabb_in_frustum(&frustum.planes, min, max)
}

/// Welt-Konvention:
/// - +Y = oben
/// - Startvorwaertsrichtung in -Z (typisch fuer RH setups)
pub(crate) struct Camera {
    pub(crate) position: Vec3,
    pub(crate) yaw: f32,
    pub(crate) pitch: f32,

    pub(crate) fov_y_radians: f32,
    pub(crate) z_near: f32,
    pub(crate) z_far: f32,
}

impl Camera {
    pub(crate) fn new(position: Vec3) -> Self {
        Self {
            position,
            yaw: (-90.0_f32).to_radians(),
            pitch: 0.0,
            fov_y_radians: 70.0_f32.to_radians(),
            z_near: 0.1,
            z_far: 500.0,
        }
    }

    pub(crate) fn forward(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        Vec3::new(cy * cp, sp, sy * cp).normalize()
    }

    pub(crate) fn right(&self) -> Vec3 {
        self.forward().cross(Vec3::Y).normalize()
    }

    pub(crate) fn forward_flat(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        Vec3::new(cy, 0.0, sy).normalize()
    }

    pub(crate) fn right_flat(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        Vec3::new(-sy, 0.0, cy).normalize()
    }

    pub(crate) fn view_matrix(&self) -> Mat4 {
        Mat4::look_at_rh(self.position, self.position + self.forward(), Vec3::Y)
    }

    pub(crate) fn proj_matrix(&self, aspect: f32) -> Mat4 {
        Mat4::perspective_rh(self.fov_y_radians, aspect, self.z_near, self.z_far)
    }
}

impl Default for Camera {
    fn default() -> Self {
        Camera::new(Vec3::new(128.0, 60.0, 220.0))
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct CameraUniform {
    pub(crate) view_proj: [[f32; 4]; 4],
    pub(crate) camera_pos: [f32; 3],
    pub(crate) _pad0: f32,
    pub(crate) fog_params: [f32; 4], // x=start, y=end, z=darken_strength, w=color_blend
    pub(crate) fog_color: [f32; 4],  // rgb=fog color, a=reserved
    pub(crate) sky_params: [f32; 4], // x=sky_brightness, yzw=reserved
}

impl CameraUniform {
    pub(crate) fn new() -> Self {
        Self {
            view_proj: Mat4::IDENTITY.to_cols_array_2d(),
            camera_pos: [0.0, 0.0, 0.0],
            _pad0: 0.0,
            fog_params: [
                FOG_START_DISTANCE,
                FOG_END_DISTANCE,
                FOG_DARKEN_STRENGTH,
                FOG_COLOR_BLEND_DEFAULT,
            ],
            fog_color: [
                FOG_COLOR_DEFAULT[0],
                FOG_COLOR_DEFAULT[1],
                FOG_COLOR_DEFAULT[2],
                0.0,
            ],
            sky_params: [1.0, 0.0, 0.0, 0.0],
        }
    }

    pub(crate) fn update(
        &mut self,
        camera: &Camera,
        aspect: f32,
        underwater_kind: Option<FluidKind>,
        overlay_alpha: f32,
        render_distance_fog: Option<(f32, f32)>,
    ) {
        let view_proj = build_view_proj_wgpu(camera, aspect);

        if cfg!(debug_assertions) {
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
        let (mut fog_start, mut fog_end, darken, fog_color, fog_blend) = match underwater_kind {
            Some(FluidKind::Water) => (
                WATER_FOG_START_DISTANCE,
                WATER_FOG_END_DISTANCE,
                WATER_FOG_DARKEN_STRENGTH,
                WATER_FOG_COLOR,
                WATER_FOG_COLOR_BLEND,
            ),
            Some(FluidKind::Lava) => (
                LAVA_FOG_START_DISTANCE,
                LAVA_FOG_END_DISTANCE,
                LAVA_FOG_DARKEN_STRENGTH,
                LAVA_FOG_COLOR,
                LAVA_FOG_COLOR_BLEND,
            ),
            None => (
                FOG_START_DISTANCE,
                FOG_END_DISTANCE,
                FOG_DARKEN_STRENGTH,
                FOG_COLOR_DEFAULT,
                FOG_COLOR_BLEND_DEFAULT,
            ),
        };
        if underwater_kind.is_none() {
            if let Some((start, end)) = render_distance_fog {
                fog_start = start;
                fog_end = end;
            }
        }
        self.fog_params = [fog_start, fog_end, darken, fog_blend];
        self.fog_color = [fog_color[0], fog_color[1], fog_color[2], overlay_alpha];
        self.sky_params = [1.0, 0.0, 0.0, 0.0];
    }
}
