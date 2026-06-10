pub(crate) const WORLD_SHADER_WGSL: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
    camera_pos: vec3<f32>,
    _pad0: f32,
    fog_params: vec4<f32>,
    fog_color: vec4<f32>,
    sky_params: vec4<f32>,
};

struct AnimGpu {
    sheet_frames: u32,
    current_frame: u32,
    flags: u32,
    _pad: u32,
};

@group(0) @binding(0)
var block_tex: texture_2d_array<f32>;
@group(0) @binding(1)
var block_sampler: sampler;
@group(0) @binding(2)
var<storage, read> anim_table: array<AnimGpu>;
@group(1) @binding(0)
var<uniform> camera: Camera;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) uv_raw: vec2<u32>,    // Uint16x2: fixed-point UVs (÷ 4096)
    @location(2) tex_layer: u32,
    @location(3) packed: vec4<u32>,    // Uint8x4: x=shade, y=tint_kind, z=light_packed, w=pad
};

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) tex_layer: u32,
    @location(3) world_pos: vec3<f32>,
    @location(4) @interpolate(flat) tint_kind: u32,
    @location(5) @interpolate(flat) light_packed: u32,
};

@vertex
fn vs_main(input: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_pos = camera.view_proj * vec4<f32>(input.position, 1.0);
    let shade = f32(input.packed.x) / 255.0;
    out.color = vec3<f32>(shade, shade, shade);
    out.uv = vec2<f32>(f32(input.uv_raw.x), f32(input.uv_raw.y)) / 4096.0;
    out.tex_layer = input.tex_layer;
    out.world_pos = input.position;
    out.tint_kind = input.packed.y;
    out.light_packed = input.packed.z;
    return out;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    let GRASS_TINT: vec3<f32> = vec3<f32>(90.0 / 255.0, 200.0 / 255.0, 70.0 / 255.0);
    let WATER_TINT: vec4<f32> = vec4<f32>(63.0 / 255.0, 118.0 / 255.0, 228.0 / 255.0, 0.60);
    let MIN_WORLD_BRIGHTNESS = 0.045;
    let MIN_SKY_BRIGHTNESS = 0.16;
    let anim = anim_table[input.tex_layer];
    let frame = min(anim.current_frame, max(anim.sheet_frames, 1u) - 1u);
    var uv_local = input.uv;
    if (anim.sheet_frames > 1u) {
        uv_local = fract(input.uv);
    }
    let layer = i32(input.tex_layer + frame);
    let sampled = textureSample(block_tex, block_sampler, uv_local, layer);
    if (sampled.a < 0.5) {
        discard;
    }
    // Terrain shading is baked during meshing so the shader only applies texture, tint, and light.
    var base_color = sampled.rgb * input.color;
    var tint_alpha = 1.0;
    if (input.tint_kind == 1u) {
        base_color = base_color * GRASS_TINT;
    } else if (input.tint_kind == 2u) {
        base_color = base_color * WATER_TINT.rgb;
        tint_alpha = WATER_TINT.a;
    }

    let sky_light = f32((input.light_packed >> 4u) & 0xFu) / 15.0;
    let block_light = f32(input.light_packed & 0xFu) / 15.0;
    let lighting_debug_mode = u32(round(clamp(camera.sky_params.y, 0.0, 4.0)));
    let sky_brightness = clamp(camera.sky_params.x, 0.0, 1.0);
    let effective_sky_brightness = mix(MIN_SKY_BRIGHTNESS, 1.0, sky_brightness);
    var light_f = max(block_light, sky_light * effective_sky_brightness);
    light_f = max(light_f, MIN_WORLD_BRIGHTNESS);
    if (lighting_debug_mode == 1u) {
        light_f = 1.0;
    } else if (lighting_debug_mode == 2u) {
        // Vertex grayscale from meshing (face shade * AO). Useful to isolate greedy/AO artifacts.
        base_color = vec3<f32>(input.color.r, input.color.r, input.color.r);
        tint_alpha = 1.0;
        light_f = 1.0;
    } else if (lighting_debug_mode == 3u) {
        base_color = vec3<f32>(sky_light, sky_light, sky_light);
        tint_alpha = 1.0;
    } else if (lighting_debug_mode == 4u) {
        base_color = vec3<f32>(block_light, block_light, block_light);
        tint_alpha = 1.0;
    }
    base_color = base_color * light_f;

    // Distanz-basiertes Darkening im Fragment ist robust, weil wir pro Pixel arbeiten.
    // Wir nutzen echte World-Distanz zur Kamera statt nur clip-depth.
    let distance = distance(input.world_pos, camera.camera_pos);
    let fog_start = camera.fog_params.x;
    let fog_end = camera.fog_params.y;
    let darken_strength = camera.fog_params.z;
    let fog_color_blend = camera.fog_params.w;
    let fog_color = camera.fog_color.rgb;

    let fog_factor = clamp(
        (distance - fog_start) / max(fog_end - fog_start, 0.0001),
        0.0,
        1.0
    );
    if (lighting_debug_mode == 2u || lighting_debug_mode == 3u || lighting_debug_mode == 4u) {
        return vec4<f32>(base_color, sampled.a * tint_alpha);
    }
    let darken = 1.0 - darken_strength * fog_factor;
    let shaded_color = base_color * darken;
    let mixed_color = mix(shaded_color, fog_color, fog_factor * fog_color_blend);
    return vec4<f32>(mixed_color, sampled.a * tint_alpha);
}
"#;

pub(crate) const SKY_SHADER_WGSL: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
    camera_pos: vec3<f32>,
    _pad0: f32,
    fog_params: vec4<f32>,
    fog_color: vec4<f32>,
    sky_params: vec4<f32>,
};

@group(0) @binding(0)
var sky_tex: texture_2d_array<f32>;
@group(0) @binding(1)
var sky_sampler: sampler;
@group(1) @binding(0)
var<uniform> camera: Camera;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) tex_layer: u32,
    @location(3) color: vec3<f32>,
    @location(4) _mode: u32,
};

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) tex_layer: u32,
    @location(2) color: vec3<f32>,
    @location(3) @interpolate(flat) mode: u32,
};

@vertex
fn vs_main(input: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_pos = camera.view_proj * vec4<f32>(input.position, 1.0);
    out.uv = input.uv;
    out.tex_layer = input.tex_layer;
    out.color = input.color;
    out.mode = input._mode;
    return out;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    let sampled = textureSample(sky_tex, sky_sampler, input.uv, i32(input.tex_layer));
    if (sampled.a <= 0.01) {
        discard;
    }
    return vec4<f32>(sampled.rgb * input.color, sampled.a);
}
"#;

/// Farbbasierter 3D-Shader für Highlight/Partikel (ohne Textur-Sampling).
pub(crate) const COLOR_SHADER_WGSL: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
    camera_pos: vec3<f32>,
    _pad0: f32,
    fog_params: vec4<f32>,
    fog_color: vec4<f32>,
    sky_params: vec4<f32>,
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
    let fog_color_blend = camera.fog_params.w;
    let fog_color = camera.fog_color.rgb;

    let fog_factor = clamp(
        (distance - fog_start) / max(fog_end - fog_start, 0.0001),
        0.0,
        1.0
    );
    let darken = 1.0 - darken_strength * fog_factor;
    let shaded = input.color * darken;
    let mixed = mix(shaded, fog_color, fog_factor * fog_color_blend);
    return vec4<f32>(mixed, 1.0);
}
"#;

pub(crate) const UI_SHADER_WGSL: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
    camera_pos: vec3<f32>,
    _pad0: f32,
    fog_params: vec4<f32>,
    fog_color: vec4<f32>,
    sky_params: vec4<f32>,
};

struct AnimGpu {
    sheet_frames: u32,
    current_frame: u32,
    flags: u32,
    _pad: u32,
};

@group(0) @binding(0)
var block_tex: texture_2d_array<f32>;
@group(0) @binding(1)
var block_sampler: sampler;
@group(0) @binding(2)
var<storage, read> anim_table: array<AnimGpu>;
@group(1) @binding(0)
var<uniform> camera: Camera;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) tex_layer: u32,
    @location(3) color: vec3<f32>,
    @location(4) mode: u32,
    @location(5) alpha_or_light: u32,
};

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) tex_layer: u32,
    @location(3) @interpolate(flat) mode: u32,
    @location(4) @interpolate(flat) alpha_or_light: u32,
};

@vertex
fn vs_main(input: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_pos = vec4<f32>(input.position, 1.0);
    out.color = input.color;
    out.uv = input.uv;
    out.tex_layer = input.tex_layer;
    out.mode = input.mode;
    out.alpha_or_light = input.alpha_or_light;
    return out;
}

@vertex
fn vs_main_3d_overlay(input: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_pos = camera.view_proj * vec4<f32>(input.position, 1.0);
    out.color = input.color;
    out.uv = input.uv;
    out.tex_layer = input.tex_layer;
    out.mode = input.mode;
    out.alpha_or_light = input.alpha_or_light;
    return out;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    if (input.mode == 1u) {
        let anim = anim_table[input.tex_layer];
        let frame = min(anim.current_frame, max(anim.sheet_frames, 1u) - 1u);
        var uv_local = input.uv;
        if (anim.sheet_frames > 1u) {
            uv_local = fract(input.uv);
        }
        let sampled = textureSample(block_tex, block_sampler, uv_local, i32(input.tex_layer + frame));
        if (sampled.a < 0.1) {
            discard;
        }
        return vec4<f32>(sampled.rgb * input.color, sampled.a);
    } else if (input.mode == 2u) {
        let sampled = textureSample(block_tex, block_sampler, fract(input.uv), i32(input.tex_layer));
        if (sampled.a < 0.1) {
            discard;
        }
        let overlay_alpha = camera.fog_color.a;
        return vec4<f32>(sampled.rgb * input.color, sampled.a * overlay_alpha);
    } else if (input.mode == 3u) {
        let alpha = clamp(f32(input.alpha_or_light & 0xFFu) / 255.0, 0.0, 1.0);
        return vec4<f32>(input.color, alpha);
    }
    return vec4<f32>(input.color, 1.0);
}
"#;

/// Minimaler 3D-Line-Shader fuer Block-Selection-Outline.
pub(crate) const OUTLINE_SHADER_WGSL: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
    camera_pos: vec3<f32>,
    _pad0: f32,
    fog_params: vec4<f32>,
    fog_color: vec4<f32>,
    sky_params: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

struct VsIn {
    @location(0) position: vec3<f32>,
};

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
};

@vertex
fn vs_main(input: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_pos = camera.view_proj * vec4<f32>(input.position, 1.0);
    return out;
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}
"#;

pub(crate) const RAIN_SHADER_WGSL: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
    camera_pos: vec3<f32>,
    _pad0: f32,
    fog_params: vec4<f32>,
    fog_color: vec4<f32>,
    sky_params: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

struct VsIn {
    @location(0) position: vec3<f32>,
};

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
};

@vertex
fn vs_main(input: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_pos = camera.view_proj * vec4<f32>(input.position, 1.0);
    return out;
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(0.67, 0.74, 0.82, 0.58);
}
"#;

pub(crate) const SNOW_SHADER_WGSL: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
    camera_pos: vec3<f32>,
    _pad0: f32,
    fog_params: vec4<f32>,
    fog_color: vec4<f32>,
    sky_params: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

struct VsIn {
    @location(0) position: vec3<f32>,
};

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
};

@vertex
fn vs_main(input: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_pos = camera.view_proj * vec4<f32>(input.position, 1.0);
    return out;
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(0.95, 0.97, 1.0, 0.76);
}
"#;
