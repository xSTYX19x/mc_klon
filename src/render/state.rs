use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::f32::consts::PI;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable};
use crossbeam_channel::{
    Receiver as CrossbeamReceiver, Sender as CrossbeamSender, TrySendError as CrossbeamTrySendError,
};
use glam::{IVec3, Mat4, Vec3};
use wgpu::SurfaceError;
use wgpu::util::DeviceExt;
use winit::window::Window;

use super::chunkgen_worker::{
    ChunkGenJob, ChunkGenRequestKind, ChunkGenResult, ChunkGenWorkerPool,
    GeneratedFluidBootstrapCell, LodColumnCoord,
};
use super::depth::DepthTexture;
use super::lighting_worker::{LightingJob, LightingResult, LightingWorkerPool};
use super::mesh_gpu::{
    ChunkMesh, LayeredChunkMesh, MeshUploadStats, upload_cpu_mesh_to_gpu_reuse,
    upload_layered_chunk_mesh_to_gpu_reuse,
};
use super::mesh_worker::{
    ChunkSnapshot, MeshJob, MeshResult, MeshWorkerPool, NeighborhoodSnapshot,
};
use super::shaders::{
    COLOR_SHADER_WGSL, OUTLINE_SHADER_WGSL, SKY_SHADER_WGSL, UI_SHADER_WGSL, WORLD_SHADER_WGSL,
};
use super::texture_anim::{
    AnimAxis, AnimGpu, AnimMeta, AnimState, TextureAnimTrack, decode_texture_frames,
    initial_anim_gpu_for_meta, make_missing_texture_rgba, parse_anim_meta_for_texture,
    resize_rgba_nearest,
};
use crate::blocks::{
    BlockId, BlockModel, BlockRegistry, BlockTextureLookup, Face, REQUIRED_FACE_TEXTURE_KEYS,
    REQUIRED_OVERLAY_TEXTURE_KEYS, TEX_BEDROCK, TEX_DIRT, TEX_GRASS_SIDE, TEX_GRASS_TOP,
    TEX_OAK_PLANKS, TEX_SAPLING, TEX_STONE, TEX_WATER_OVERLAY, TEXTURE_SELF_TEST_KEYS, TINT_NONE,
};
use crate::camera::{
    Camera, CameraUniform, Frustum, build_view_proj_wgpu, chunk_in_frustum, sphere_in_frustum,
};
use crate::config::assets::ASSET_ROOT;
use crate::config::player::{PLAYER_HEIGHT, PLAYER_RADIUS};
use crate::config::render::{
    CROSSHAIR_HALF_LEN_PX, CROSSHAIR_THICKNESS_PX, FOG_RENDERDIST_END_MARGIN_BLOCKS,
    FOG_RENDERDIST_START_RATIO, MINECRAFT_TICK_SECONDS, NEAR_MESH_RADIUS_CHUNKS, PARTICLE_GRAVITY,
    PARTICLE_LIFE_MAX, PARTICLE_LIFE_MIN, PARTICLE_MAX_COUNT, PARTICLE_QUAD_SIZE,
    PARTICLE_SPAWN_COUNT, PARTICLE_SPEED_MAX, PARTICLE_SPEED_MIN, PREVIEW_ACCENT_STRIPE_PX,
    PREVIEW_MARGIN_RIGHT_PX, PREVIEW_MARGIN_TOP_PX, PREVIEW_SIZE_PX, RenderDistance,
    STREAM_BURST_CHUNKGEN_APPLY_BUDGET_MS, STREAM_BURST_DISPATCH_BUDGET_MS,
    STREAM_BURST_MAX_TOTAL_BUDGET_MS, STREAM_BURST_MESH_UPLOAD_BUDGET_MS,
    STREAM_BURST_MISSING_MESHES_THRESHOLD, STREAM_BURST_NEAR_RADIUS_CHUNKS,
    STREAM_CHUNKGEN_APPLY_BUDGET_MS, STREAM_DISPATCH_BUDGET_MS, STREAM_MESH_UPLOAD_BUDGET_MS,
    SimulationDistance, UI_MODE_SOLID, UI_MODE_TEXTURED, UI_MODE_WATER_OVERLAY, USE_GREEDY_MESHING,
    WATER_OVERLAY_TINT, WATER_OVERLAY_UV_TILE,
};
use crate::config::sim::RANDOM_TICK_SPEED_DEFAULT;
use crate::meshing::{CpuMeshData, FULLBRIGHT_LIGHT_PACKED, Vertex};
use crate::perf::trace as perf_trace;
use crate::sim::{ActiveSimColumns, ColumnCoord, SimSystem};
use crate::storage::chunk_codec::{
    ColumnBlob, ColumnSaveCompletion, ColumnSaveSectionSnapshot, ColumnSaveSnapshot, apply_column,
    decode_column,
};
use crate::storage::format::{
    SAVE_ROOT_DIR, SECTION_BLOCK_COUNT, TICKING_AREAS_FILE_NAME, WORLD_MANIFEST_FILE_NAME,
    WORLD_META_FILE_NAME,
};
use crate::storage::world_meta::{self, WorldMeta};
use crate::storage::{
    RegionStorage, StorageColumnLoadResult, StorageColumnLoadStatus, StorageColumnSaveResult,
    StorageLoadWorker, StorageWorker, WorldStorage,
};
use crate::texture::{TextureArray, load_png_rgba};
use crate::texture_registry::TextureRegistry;
use crate::world::block_entities_system::BlockEntitiesSystem;
use crate::world::fluids::{
    FLUID_FIXED_STEP_SECONDS, FLUID_MAX_TICKS_PER_FRAME, FluidDebugCounts, FluidKind,
    fluid_data_is_source_origin, fluid_data_make, fluid_kind, make_still,
};
use crate::world::fluids_system::FluidsSystem;
use crate::world::manifest::{IdMapper, WorldManifest};
use crate::world::random_ticks_system::RandomTicksSystem;
use crate::world::raycast::{RAYCAST_MAX_DISTANCE, raycast_world};
use crate::world::time::WorldTime;
use crate::world::ticking_areas::{self, TickingArea, TickingFlags};
use crate::world::{CHUNK_SIZE, Chunk, ChunkCoord, LightLevel, WORLD_MAX_Y, World, WorldConfig};
use crate::worldgen::{DEFAULT_WORLDGEN_PRESET, WorldgenPreset};

struct Particle {
    pos: Vec3,
    vel: Vec3,
    life: f32,
    color: [f32; 3],
}

#[derive(Clone, Copy, Debug)]
struct PendingBlockEdit {
    pos: IVec3,
    old: BlockId,
    new: BlockId,
}

#[derive(Clone, Copy, Debug)]
struct PendingSandGravityCheck {
    origin: IVec3,
    checked_base: bool,
    next_y: i32,
}

#[derive(Clone, Copy, Debug, Default)]
struct SandGravityResolveStats {
    scanned: usize,
    dropped: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct BlockEditTimings {
    raycast_ms: f32,
    set_block_ms: f32,
    lighting_ms: f32,
    mark_dirty_ms: f32,
    fluid_notify_ms: f32,
    sand_gravity_ms: f32,
    particles_ms: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StreamPriorityKey {
    dist2: i32,
    visibility_score: u32,
    horizon_penalty_rank: u8,
    view_alignment_rank: u8,
    y_band_rank: u8,
    age: u32,
}

impl StreamPriorityKey {
    fn cmp_ascending(&self, other: &Self) -> Ordering {
        self.dist2
            .cmp(&other.dist2)
            .then_with(|| self.visibility_score.cmp(&other.visibility_score))
            .then_with(|| self.horizon_penalty_rank.cmp(&other.horizon_penalty_rank))
            .then_with(|| self.view_alignment_rank.cmp(&other.view_alignment_rank))
            .then_with(|| self.y_band_rank.cmp(&other.y_band_rank))
            .then_with(|| self.age.cmp(&other.age))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ChunkPriorityHeapEntry {
    coord: ChunkCoord,
    key: StreamPriorityKey,
}

impl Ord for ChunkPriorityHeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .key
            .cmp_ascending(&self.key)
            .then_with(|| self.coord.x.cmp(&other.coord.x))
            .then_with(|| self.coord.y.cmp(&other.coord.y))
            .then_with(|| self.coord.z.cmp(&other.coord.z))
    }
}

impl PartialOrd for ChunkPriorityHeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Default)]
struct ColumnStreamPlan {
    tier0_y: Vec<i32>,
    tier1_y: Vec<i32>,
}

impl BlockEditTimings {
    fn top_scope(self) -> (&'static str, f32) {
        let mut top = ("none", 0.0f32);
        for (name, ms) in [
            ("block_edit.raycast", self.raycast_ms),
            ("block_edit.set_block", self.set_block_ms),
            ("block_edit.lighting_on_block_changed", self.lighting_ms),
            (
                "block_edit.mark_dirty_after_block_change",
                self.mark_dirty_ms,
            ),
            (
                "block_edit.notify_fluid_on_block_change",
                self.fluid_notify_ms,
            ),
            ("block_edit.sand_gravity_resolve", self.sand_gravity_ms),
            ("block_edit.spawn_particles", self.particles_ms),
        ] {
            if ms > top.1 {
                top = (name, ms);
            }
        }
        top
    }
}

fn maybe_log_slow_block_edit(
    kind: &str,
    total_ms: f32,
    coord: Option<IVec3>,
    timings: BlockEditTimings,
) {
    if total_ms <= 4.0 {
        return;
    }
    let (top, top_ms) = timings.top_scope();
    let coord_str = coord
        .map(|c| format!("({}, {}, {})", c.x, c.y, c.z))
        .unwrap_or_else(|| "none".to_string());
    eprintln!(
        "BLOCK_EDIT_SLOW kind={} ms={:.3} top={} top_ms={:.3} coord={}",
        kind, total_ms, top, top_ms, coord_str
    );
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct OutlineVertex {
    position: [f32; 3],
}

impl OutlineVertex {
    fn layout() -> wgpu::VertexBufferLayout<'static> {
        use std::mem;

        const ATTRS: [wgpu::VertexAttribute; 1] = [wgpu::VertexAttribute {
            offset: 0,
            shader_location: 0,
            format: wgpu::VertexFormat::Float32x3,
        }];

        wgpu::VertexBufferLayout {
            array_stride: mem::size_of::<OutlineVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &ATTRS,
        }
    }
}

fn build_outline_line_vertices(pos: IVec3) -> [OutlineVertex; BLOCK_OUTLINE_VERTEX_COUNT as usize] {
    let min = Vec3::new(pos.x as f32, pos.y as f32, pos.z as f32) - Vec3::splat(BLOCK_OUTLINE_EPS);
    let max = Vec3::new((pos.x + 1) as f32, (pos.y + 1) as f32, (pos.z + 1) as f32)
        + Vec3::splat(BLOCK_OUTLINE_EPS);
    let corners = [
        [min.x, min.y, min.z],
        [max.x, min.y, min.z],
        [max.x, min.y, max.z],
        [min.x, min.y, max.z],
        [min.x, max.y, min.z],
        [max.x, max.y, min.z],
        [max.x, max.y, max.z],
        [min.x, max.y, max.z],
    ];

    let mut vertices = [OutlineVertex {
        position: [0.0, 0.0, 0.0],
    }; BLOCK_OUTLINE_VERTEX_COUNT as usize];
    let edges = [
        (0usize, 1usize),
        (1, 2),
        (2, 3),
        (3, 0),
        (4, 5),
        (5, 6),
        (6, 7),
        (7, 4),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ];

    for (edge_index, (a, b)) in edges.into_iter().enumerate() {
        let i = edge_index * 2;
        vertices[i] = OutlineVertex {
            position: corners[a],
        };
        vertices[i + 1] = OutlineVertex {
            position: corners[b],
        };
    }

    vertices
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartupWorldMode {
    LoadIfPresentElseGenerate,
    AlwaysRegenerate,
}

const STARTUP_WORLD_MODE: StartupWorldMode = StartupWorldMode::LoadIfPresentElseGenerate;
const WORLDGEN_DIRT_TILE_PX: f32 = 32.0;
const WORLDGEN_FONT_SCALE: f32 = 2.0;
const WORLDGEN_TEXT_SHADOW_OFFSET_PX: f32 = 1.0;
const WORLDGEN_BAR_WIDTH_PX: f32 = 200.0;
const WORLDGEN_BAR_HEIGHT_PX: f32 = 6.0;
const WORLDGEN_BAR_BORDER: [f32; 3] = [0.20, 0.20, 0.20];
const WORLDGEN_BAR_BG: [f32; 3] = [0.50, 0.50, 0.50];
const WORLDGEN_BAR_FILL: [f32; 3] = [0.50, 0.94, 0.13];
const WORLDGEN_TEXT_MAIN: [f32; 3] = [1.0, 1.0, 1.0];
const WORLDGEN_TEXT_SHADOW: [f32; 3] = [0.25, 0.25, 0.25];
const BLOCK_OUTLINE_EPS: f32 = 0.002;
const BLOCK_OUTLINE_VERTEX_COUNT: u32 = 24;
const PREVIEW_3D_CAMERA_YAW_RAD: f32 = -135.0_f32.to_radians();
const PREVIEW_3D_CAMERA_PITCH_RAD: f32 = -35.264_f32.to_radians();
const PREVIEW_3D_CAMERA_POS: [f32; 3] = [2.2, 2.0, 2.2];
const PREVIEW_3D_FOV_Y_RAD: f32 = 32.0_f32.to_radians();
const PREVIEW_3D_SIZE_SCALE: f32 = 1.6;
const PAUSE_MENU_TITLE: &str = "Game menu";
const PAUSE_MENU_BUTTON_WIDTH_PX: f32 = 440.0;
const PAUSE_MENU_BUTTON_HEIGHT_PX: f32 = 40.0;
const PAUSE_MENU_BUTTON_SPACING_PX: f32 = 10.0;
const PAUSE_MENU_BUTTON_BORDER: [f32; 3] = [0.12, 0.12, 0.12];
const PAUSE_MENU_BUTTON_NORMAL: [f32; 3] = [0.28, 0.28, 0.28];
const PAUSE_MENU_BUTTON_SELECTED: [f32; 3] = [0.45, 0.45, 0.45];
const CHUNK_WARMUP_RADIUS: i32 = 2;
const COORDS_TEXT_SCALE: f32 = 1.5;
const DEBUG_OVERLAY_TEXT_SCALE: f32 = 2.5;
const COORDS_PAD_X: f32 = 8.0;
const COORDS_PAD_Y: f32 = 8.0;
const SQRT_3: f32 = 1.732_050_8;
const CHUNK_HALF_EXTENT_F32: f32 = CHUNK_SIZE as f32 * 0.5;
const CHUNK_BOUNDING_SPHERE_RADIUS: f32 = CHUNK_HALF_EXTENT_F32 * SQRT_3;
const ENABLE_FAR_LOD_DEFAULT: bool = true;
const STREAM_KEEP_RADIUS_MARGIN_CHUNKS: i32 = 2;
const VERTICAL_VIEW_RADIUS_CHUNKS: i32 = 4;
const SURFACE_STREAM_PRIMARY_HALF_BAND_CHUNKS: i32 = 1;
const SURFACE_STREAM_SECONDARY_MAX_EXTRA_CHUNKS: i32 = 2;
const SURFACE_STREAM_SECONDARY_EXPAND_PERIOD_FRAMES: u64 = 180;
const SURFACE_STREAM_UNDERGROUND_TRIGGER_BLOCKS: i32 = 16;
const SURFACE_STREAM_DESCENT_TRIGGER_BLOCKS_PER_FRAME: f32 = 0.25;
const SURFACE_STREAM_DESCENT_HOLD_FRAMES: u32 = 48;
const SURFACE_STREAM_MINING_HOLD_FRAMES: u32 = 60;
const LOD_BOUNDING_SPHERE_RADIUS: f32 = 40.0;
const LOD_REQUESTS_PER_FRAME_DEFAULT: usize = 1;
const FAR_LOD_GPU_UPLOADS_PER_FRAME: usize = 1;
const FAR_LOD_CPU_SKIP_AFTER_MS: f32 = 6.0;
const FAR_LOD_MESH_UPLOAD_BACKLOG_SKIP_THRESHOLD: usize = 64;
const LOD_STREAM_CENTER_HYSTERESIS_BLOCKS: i32 = 2;
const RENDER_STATS_REPORT_INTERVAL: Duration = Duration::from_secs(1);
const AUTOSAVE_COLUMNS_PER_FRAME: usize = 2;
const AUTOSAVE_QUEUE_SCAN_BUDGET_PER_FRAME: usize = 16;
const SAVED_LOAD_REQUESTS_PER_FRAME: usize = 2;
const SAVED_LOAD_CANDIDATE_SCAN_BUDGET_PER_FRAME: usize = 64;
const SAVED_LOAD_RESULTS_APPLY_PER_FRAME: usize = 4;
const SAVED_LOAD_APPLY_BUDGET_MS: f32 = 2.0;
const SAVED_LOAD_MISS_CACHE_TTL: Duration = Duration::from_secs(60);
const STREAM_STAGE_LOOP_SAFETY_CAP: usize = 4096;
const CHUNKGEN_REQUESTS_PER_FRAME: usize = 48;
const TICKING_AREA_STREAM_BUDGET_PER_FRAME: usize = 24;
const CHUNKGEN_REQUESTS_PER_FRAME_BACKPRESSURE_MIN: usize = 8;
const CHUNKGEN_BACKPRESSURE_DIRTY_COLUMNS_THRESHOLD: usize = 200;
const CHUNKGEN_BACKPRESSURE_PENDING_UNLOAD_COLUMNS_THRESHOLD: usize = 16;
const CHUNKGEN_PRIORITY_HEAP_REBUILD_PERIOD_FRAMES: u64 = 12;
const STREAM_PRIORITY_VIEW_ALIGNMENT_BUCKETS: u8 = 8;
const STREAM_PRIORITY_REORDER_MIN_BACKLOG: usize = 24;
const MESH_FRUSTUM_DEFER_FRAMES: u64 = 24;
const MESH_MISSING_SAFETY_SCAN_PERIOD_FRAMES: u64 = 30;
const STREAM_HORIZON_SECTOR_COUNT: usize = 64;
const STREAM_HORIZON_OCCLUDED_MARGIN_RAD: f32 = 0.034_906_585;
const STREAM_HORIZON_PENALTY_STEP_RAD: f32 = 0.087_266_46;
const STREAM_HORIZON_MAX_PENALTY_RANK: u8 = 6;
const MAX_INFLIGHT_CHUNKGEN: usize = 128;
const MAX_INFLIGHT_LOD_CHUNKGEN: usize = 64;
const CHUNKGEN_JOB_QUEUE_CAPACITY: usize = 256;
const CHUNKGEN_RESULT_QUEUE_CAPACITY: usize = 256;
const CHUNKGEN_APPLY_LOG_THRESHOLD_MS: f32 = 8.0;
const MESH_RESULTS_DRAIN_PER_FRAME: usize = 64;
const STORAGE_SAVE_RESULTS_PER_FRAME: usize = 32;
const LIGHTING_RESULTS_PER_FRAME: usize = 2;
const LIGHTING_CHANGED_CHUNK_DIRTY_APPLY_PER_FRAME: usize = 64;
const LIGHTING_CHANGED_CHUNK_DIRTY_APPLY_BUDGET_MS: f32 = 0.5;
const LIGHTING_REBUILD_DEBOUNCE_MS: u64 = 75;
const PENDING_LIGHTING_SKY_HEURISTIC_MIN_Y: i32 = 64;
const MESH_UPLOAD_LOG_THRESHOLD: Duration = Duration::from_millis(4);
const BLOCK_EDIT_SIDEFX_BUDGET_MS: f32 = 1.5;
const BLOCK_EDIT_SIDEFX_MAX_EDITS_PER_FRAME: usize = 8;
const BLOCK_EDIT_PARTICLES_MAX_SPAWN_PER_FRAME: usize = 64;
const BLOCK_EDIT_SAND_SCAN_STEPS_PER_FRAME: usize = 32;
const FLUID_SIM_BUDGET_MS_PER_FRAME: f32 = 3.0;
// Mesh coalescing for fluid-driven chunk invalidation: periodic preview updates while active,
// then one final rebuild after the chunk settles.
const FLUID_MESH_ACTIVE_REBUILD_EVERY_STEPS: u8 = 2;
const FLUID_MESH_SETTLE_STEPS: u8 = 3;
const FLUID_MESH_MIN_FRAMES_BETWEEN_DISPATCH: u64 = 2;
const STREAM_PRUNE_MESH_PERIOD_FRAMES: u64 = 10;
const STREAM_PRUNE_MESHES_PER_FRAME: usize = 128;
const STREAM_PRUNE_MESH_BUDGET_MS: f32 = 1.0;
const STREAM_UNLOAD_SCAN_REFRESH_PERIOD_FRAMES: u64 = 12;
const STREAM_UNLOAD_SCAN_COORDS_PER_FRAME: usize = 256;
const STREAM_EVICT_SAVE_COLUMNS_PER_FRAME: usize = 1;
const STREAM_EVICT_COLUMNS_PER_FRAME: usize = 2;
const STREAM_EVICT_CHUNKS_PER_FRAME: usize = 24;
const STREAM_EVICT_BUDGET_MS: f32 = 1.5;
const AUTOSAVE_DYNAMIC_DIRTY_COLUMNS_PER_STEP: usize = 50;
const AUTOSAVE_DYNAMIC_MAX_COLUMNS_PER_FRAME: usize = 8;
const SKY_SUN_QUAD_SIZE: f32 = 30.0;
const SKY_MOON_QUAD_SIZE: f32 = 20.0;
const SKY_SUN_DISTANCE: f32 = 100.0;
const SKY_MOON_DISTANCE: f32 = 100.0;
const SKY_OBJECT_VERTEX_COUNT: usize = 8;
const SKY_OBJECT_INDEX_COUNT: u32 = 12;
const BOUNDARY_OCEAN_OUTER_PADDING_BLOCKS: f32 = 512.0;
const BOUNDARY_OCEAN_UV_TILE_PER_BLOCK: f32 = 1.0 / 16.0;
const STALL_REPORT_THRESHOLD_MS: f32 = 200.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LightingDebugView {
    Normal,
    Fullbright,
    AoOnly,
    SkyLightHeatmap,
    BlockLightHeatmap,
}

impl LightingDebugView {
    fn next(self) -> Self {
        match self {
            Self::Normal => Self::Fullbright,
            Self::Fullbright => Self::AoOnly,
            Self::AoOnly => Self::SkyLightHeatmap,
            Self::SkyLightHeatmap => Self::BlockLightHeatmap,
            Self::BlockLightHeatmap => Self::Normal,
        }
    }

    fn shader_mode_f32(self) -> f32 {
        match self {
            Self::Normal => 0.0,
            Self::Fullbright => 1.0,
            Self::AoOnly => 2.0,
            Self::SkyLightHeatmap => 3.0,
            Self::BlockLightHeatmap => 4.0,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Fullbright => "fullbright",
            Self::AoOnly => "ao_only",
            Self::SkyLightHeatmap => "skylight_heatmap",
            Self::BlockLightHeatmap => "blocklight_heatmap",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct StateFramePerf {
    frame_started_at: Instant,
    pub(crate) frame_dt_ms: f32,

    pub(crate) pump_chunkgen_results_ms: f32,
    pub(crate) pump_mesh_results_ms: f32,
    pub(crate) pump_storage_save_results_ms: f32,
    pub(crate) autosave_total_ms: f32,
    pub(crate) rebuild_world_lighting_ms: f32,
    pub(crate) enqueue_chunks_within_view_ms: f32,
    pub(crate) ensure_saved_column_load_ms: f32,
    pub(crate) gpu_chunk_mesh_upload_ms: f32,
    pub(crate) gpu_far_lod_upload_ms: f32,
    pub(crate) process_dirty_chunk_meshes_ms: f32,
    pub(crate) proc_pump_chunkgen_results_ms: f32,
    pub(crate) proc_pump_storage_load_results_ms: f32,
    pub(crate) proc_unload_chunks_outside_keep_radius_ms: f32,
    pub(crate) proc_prune_chunk_meshes_outside_view_ms: f32,
    pub(crate) proc_enqueue_chunks_within_view_ms: f32,
    pub(crate) proc_rebuild_world_lighting_if_dirty_ms: f32,
    pub(crate) proc_dispatch_mesh_jobs_loop_total_ms: f32,
    pub(crate) proc_autosave_if_due_ms: f32,
    pub(crate) proc_dispatch_frustum_build_ms: f32,
    pub(crate) proc_dispatch_pop_dirty_ms: f32,
    pub(crate) proc_dispatch_snapshot_neighborhood_ms: f32,
    pub(crate) proc_dispatch_send_mesh_job_ms: f32,
    pub(crate) proc_dispatch_requeue_dirty_ms: f32,
    pub(crate) stream_burst_mode_active: bool,
    pub(crate) stream_missing_meshes_near_camera: usize,
    pub(crate) stream_dispatch_budget_ms: f32,
    pub(crate) stream_dispatch_used_ms: f32,
    pub(crate) stream_mesh_upload_budget_ms: f32,
    pub(crate) stream_mesh_upload_used_ms: f32,
    pub(crate) stream_chunkgen_apply_budget_ms: f32,
    pub(crate) stream_chunkgen_apply_used_ms: f32,

    pub(crate) chunkgen_results_drained: usize,
    pub(crate) chunkgen_results_backlog_before: usize,
    pub(crate) chunkgen_results_backlog_after: usize,
    pub(crate) chunkgen_chunks_applied: usize,
    pub(crate) chunkgen_empty_chunks_applied: usize,
    pub(crate) chunkgen_lod_results_applied: usize,
    pub(crate) chunkgen_lod_mesh_results_enqueued: usize,
    pub(crate) chunkgen_lod_mesh_uploads: usize,
    pub(crate) chunkgen_lod_upload_backlog_before: usize,
    pub(crate) chunkgen_lod_upload_backlog_after: usize,

    pub(crate) mesh_results_drained: usize,
    pub(crate) mesh_results_backlog_before: usize,
    pub(crate) mesh_results_backlog_after: usize,
    pub(crate) mesh_upload_queue_before: usize,
    pub(crate) mesh_upload_queue_after: usize,
    pub(crate) mesh_uploads_processed: usize,
    pub(crate) mesh_gpu_buffers_created: u32,
    pub(crate) mesh_gpu_reused_writes: u32,

    pub(crate) storage_save_results_drained: usize,
    pub(crate) storage_save_results_backlog_before: usize,
    pub(crate) storage_save_results_backlog_after: usize,
    pub(crate) storage_save_results_ok: usize,
    pub(crate) storage_save_results_failed: usize,
    pub(crate) autosave_columns_queued: usize,

    pub(crate) lighting_results_drained: usize,
    pub(crate) lighting_results_backlog_before: usize,
    pub(crate) lighting_results_backlog_after: usize,
    pub(crate) lighting_chunks_changed: usize,

    pub(crate) saved_column_load_attempts: usize,
    pub(crate) saved_column_load_hits: usize,
    pub(crate) saved_column_load_misses: usize,

    pub(crate) far_lod_requests_queued: usize,
    pub(crate) far_lod_requests_suppressed_cpu_budget: bool,
    pub(crate) far_lod_requests_suppressed_backlog: bool,
    pub(crate) far_lod_center_changes: usize,
    pub(crate) lod_center_changed_this_frame: Option<((i32, i32), (i32, i32))>,
}

impl Default for StateFramePerf {
    fn default() -> Self {
        Self::new()
    }
}

impl StateFramePerf {
    fn new() -> Self {
        Self {
            frame_started_at: Instant::now(),
            frame_dt_ms: 0.0,
            pump_chunkgen_results_ms: 0.0,
            pump_mesh_results_ms: 0.0,
            pump_storage_save_results_ms: 0.0,
            autosave_total_ms: 0.0,
            rebuild_world_lighting_ms: 0.0,
            enqueue_chunks_within_view_ms: 0.0,
            ensure_saved_column_load_ms: 0.0,
            gpu_chunk_mesh_upload_ms: 0.0,
            gpu_far_lod_upload_ms: 0.0,
            process_dirty_chunk_meshes_ms: 0.0,
            proc_pump_chunkgen_results_ms: 0.0,
            proc_pump_storage_load_results_ms: 0.0,
            proc_unload_chunks_outside_keep_radius_ms: 0.0,
            proc_prune_chunk_meshes_outside_view_ms: 0.0,
            proc_enqueue_chunks_within_view_ms: 0.0,
            proc_rebuild_world_lighting_if_dirty_ms: 0.0,
            proc_dispatch_mesh_jobs_loop_total_ms: 0.0,
            proc_autosave_if_due_ms: 0.0,
            proc_dispatch_frustum_build_ms: 0.0,
            proc_dispatch_pop_dirty_ms: 0.0,
            proc_dispatch_snapshot_neighborhood_ms: 0.0,
            proc_dispatch_send_mesh_job_ms: 0.0,
            proc_dispatch_requeue_dirty_ms: 0.0,
            stream_burst_mode_active: false,
            stream_missing_meshes_near_camera: 0,
            stream_dispatch_budget_ms: 0.0,
            stream_dispatch_used_ms: 0.0,
            stream_mesh_upload_budget_ms: 0.0,
            stream_mesh_upload_used_ms: 0.0,
            stream_chunkgen_apply_budget_ms: 0.0,
            stream_chunkgen_apply_used_ms: 0.0,
            chunkgen_results_drained: 0,
            chunkgen_results_backlog_before: 0,
            chunkgen_results_backlog_after: 0,
            chunkgen_chunks_applied: 0,
            chunkgen_empty_chunks_applied: 0,
            chunkgen_lod_results_applied: 0,
            chunkgen_lod_mesh_results_enqueued: 0,
            chunkgen_lod_mesh_uploads: 0,
            chunkgen_lod_upload_backlog_before: 0,
            chunkgen_lod_upload_backlog_after: 0,
            mesh_results_drained: 0,
            mesh_results_backlog_before: 0,
            mesh_results_backlog_after: 0,
            mesh_upload_queue_before: 0,
            mesh_upload_queue_after: 0,
            mesh_uploads_processed: 0,
            mesh_gpu_buffers_created: 0,
            mesh_gpu_reused_writes: 0,
            storage_save_results_drained: 0,
            storage_save_results_backlog_before: 0,
            storage_save_results_backlog_after: 0,
            storage_save_results_ok: 0,
            storage_save_results_failed: 0,
            autosave_columns_queued: 0,
            lighting_results_drained: 0,
            lighting_results_backlog_before: 0,
            lighting_results_backlog_after: 0,
            lighting_chunks_changed: 0,
            saved_column_load_attempts: 0,
            saved_column_load_hits: 0,
            saved_column_load_misses: 0,
            far_lod_requests_queued: 0,
            far_lod_requests_suppressed_cpu_budget: false,
            far_lod_requests_suppressed_backlog: false,
            far_lod_center_changes: 0,
            lod_center_changed_this_frame: None,
        }
    }

    fn begin_frame(&mut self, frame_dt_ms: f32) {
        *self = Self::new();
        self.frame_dt_ms = frame_dt_ms;
    }

    fn elapsed_ms_since_frame_start(&self) -> f32 {
        self.frame_started_at.elapsed().as_secs_f32() * 1000.0
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        lerp(a[0], b[0], t),
        lerp(a[1], b[1], t),
        lerp(a[2], b[2], t),
    ]
}

fn pad_rgba_center(src: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    debug_assert!(src_w <= dst_w && src_h <= dst_h);
    let mut out = vec![0u8; (dst_w as usize) * (dst_h as usize) * 4];
    let off_x = (dst_w - src_w) / 2;
    let off_y = (dst_h - src_h) / 2;
    for y in 0..src_h {
        for x in 0..src_w {
            let src_i = ((y * src_w + x) * 4) as usize;
            let dx = x + off_x;
            let dy = y + off_y;
            let dst_i = ((dy * dst_w + dx) * 4) as usize;
            out[dst_i..dst_i + 4].copy_from_slice(&src[src_i..src_i + 4]);
        }
    }
    out
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct WorldGenParams {
    pub preset: WorldgenPreset,
}

impl Default for WorldGenParams {
    fn default() -> Self {
        Self {
            preset: DEFAULT_WORLDGEN_PRESET,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct WorldGenRequest {
    pub seed: u64,
    pub params: WorldGenParams,
}

#[derive(Clone, Debug)]
pub(crate) struct WorldGenProgress {
    pub phase: String,
    pub progress01: f32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct GenerationOverlay<'a> {
    pub phase: &'a str,
    pub progress01: f32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PauseOverlay<'a> {
    pub selected: usize,
    pub items: &'a [&'a str],
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DebugOverlay<'a> {
    pub lines: &'a [&'a str],
}

struct WorldGenerationTask {
    warmup_center_x: i32,
    warmup_center_z: i32,
    warmup_columns: HashSet<ColumnCoord>,
    warmup_coords: HashSet<ChunkCoord>,
    warmup_target: usize,
    warmup_column_target: usize,
    warmup_columns_generated_done: HashSet<ColumnCoord>,
    warmup_generated_done: HashSet<ChunkCoord>,
    warmup_meshed_done: HashSet<ChunkCoord>,
    pending_progress: Option<WorldGenProgress>,
    last_reported_phase: String,
    last_reported_progress_permille: u16,
    started_at: Instant,
}

// Per-chunk state for throttling fluid-triggered mesh rebuild dispatches. This reduces
// revision churn / obsolete mesh jobs when fluids update many blocks every tick.
#[derive(Clone, Copy, Debug, Default)]
struct FluidMeshCoalesceEntry {
    changed_this_frame: bool,
    settle_steps_remaining: u8,
    steps_since_last_mesh: u8,
    next_allowed_frame: u64,
    pending_dirty: bool,
}

#[derive(Clone)]
struct BitmapFont {
    texture_layer: u32,
    cell_w: u32,
    cell_h: u32,
    texel_scale_x: u32,
    texel_scale_y: u32,
    glyph_h: u8,
    glyph_widths: [u8; 256],
}

#[allow(dead_code)]
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
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: white,
        },
        Vertex {
            position: p1.to_array(),
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: TINT_NONE,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: white,
        },
        Vertex {
            position: p2.to_array(),
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: TINT_NONE,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: white,
        },
        Vertex {
            position: p3.to_array(),
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: TINT_NONE,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: white,
        },
    ];
    let indices = vec![0, 1, 2, 0, 2, 3];
    (vertices, indices)
}

pub(crate) fn build_fullscreen_overlay_mesh(
    texture_layer: u32,
    tint: [f32; 3],
    uv_tile: f32,
) -> (Vec<Vertex>, Vec<u32>) {
    let vertices = vec![
        Vertex {
            position: [-1.0, -1.0, 0.0],
            uv: [0.0, uv_tile],
            tex_layer: texture_layer,
            tint_kind: UI_MODE_WATER_OVERLAY,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: tint,
        },
        Vertex {
            position: [1.0, -1.0, 0.0],
            uv: [uv_tile, uv_tile],
            tex_layer: texture_layer,
            tint_kind: UI_MODE_WATER_OVERLAY,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: tint,
        },
        Vertex {
            position: [1.0, 1.0, 0.0],
            uv: [uv_tile, 0.0],
            tex_layer: texture_layer,
            tint_kind: UI_MODE_WATER_OVERLAY,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: tint,
        },
        Vertex {
            position: [-1.0, 1.0, 0.0],
            uv: [0.0, 0.0],
            tex_layer: texture_layer,
            tint_kind: UI_MODE_WATER_OVERLAY,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: tint,
        },
    ];
    let indices = vec![0, 1, 2, 0, 2, 3];
    (vertices, indices)
}

fn push_ocean_quad(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    texture_layer: u32,
    tint: [f32; 3],
    x0: f32,
    z0: f32,
    x1: f32,
    z1: f32,
    y: f32,
) {
    let base = vertices.len() as u32;
    let uvx0 = x0 * BOUNDARY_OCEAN_UV_TILE_PER_BLOCK;
    let uvz0 = z0 * BOUNDARY_OCEAN_UV_TILE_PER_BLOCK;
    let uvx1 = x1 * BOUNDARY_OCEAN_UV_TILE_PER_BLOCK;
    let uvz1 = z1 * BOUNDARY_OCEAN_UV_TILE_PER_BLOCK;
    vertices.extend_from_slice(&[
        Vertex {
            position: [x0, y, z0],
            uv: [uvx0, uvz0],
            tex_layer: texture_layer,
            color: tint,
            tint_kind: TINT_NONE,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
        },
        Vertex {
            position: [x1, y, z0],
            uv: [uvx1, uvz0],
            tex_layer: texture_layer,
            color: tint,
            tint_kind: TINT_NONE,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
        },
        Vertex {
            position: [x1, y, z1],
            uv: [uvx1, uvz1],
            tex_layer: texture_layer,
            color: tint,
            tint_kind: TINT_NONE,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
        },
        Vertex {
            position: [x0, y, z1],
            uv: [uvx0, uvz1],
            tex_layer: texture_layer,
            color: tint,
            tint_kind: TINT_NONE,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
        },
    ]);
    indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

fn build_boundary_ocean_ring_mesh(
    texture_layer: u32,
    y: f32,
    inner_radius_blocks: f32,
    outer_radius_blocks: f32,
) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(16);
    let mut indices = Vec::with_capacity(24);
    let tint = [1.0, 1.0, 1.0];
    let inner = inner_radius_blocks.max(0.0);
    let outer = outer_radius_blocks.max(inner + 1.0);

    // North strip
    push_ocean_quad(
        &mut vertices,
        &mut indices,
        texture_layer,
        tint,
        -outer,
        -outer,
        outer,
        -inner,
        y,
    );
    // South strip
    push_ocean_quad(
        &mut vertices,
        &mut indices,
        texture_layer,
        tint,
        -outer,
        inner,
        outer,
        outer,
        y,
    );
    // West strip
    push_ocean_quad(
        &mut vertices,
        &mut indices,
        texture_layer,
        tint,
        -outer,
        -inner,
        -inner,
        inner,
        y,
    );
    // East strip
    push_ocean_quad(
        &mut vertices,
        &mut indices,
        texture_layer,
        tint,
        inner,
        -inner,
        outer,
        inner,
        y,
    );

    (vertices, indices)
}

fn build_crosshair_mesh(view_width: u32, view_height: u32) -> (Vec<Vertex>, Vec<u32>) {
    let px_to_ndc_x = 2.0 / (view_width.max(1) as f32);
    let px_to_ndc_y = 2.0 / (view_height.max(1) as f32);

    let half_len_x = CROSSHAIR_HALF_LEN_PX * px_to_ndc_x;
    let half_len_y = CROSSHAIR_HALF_LEN_PX * px_to_ndc_y;
    let half_thick_x = (CROSSHAIR_THICKNESS_PX * 0.5) * px_to_ndc_x;
    let half_thick_y = (CROSSHAIR_THICKNESS_PX * 0.5) * px_to_ndc_y;

    let white = [1.0, 1.0, 1.0];

    let h0 = [-half_len_x, -half_thick_y, 0.0];
    let h1 = [half_len_x, -half_thick_y, 0.0];
    let h2 = [half_len_x, half_thick_y, 0.0];
    let h3 = [-half_len_x, half_thick_y, 0.0];

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
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: white,
        },
        Vertex {
            position: h1,
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: UI_MODE_SOLID,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: white,
        },
        Vertex {
            position: h2,
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: UI_MODE_SOLID,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: white,
        },
        Vertex {
            position: h3,
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: UI_MODE_SOLID,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: white,
        },
        Vertex {
            position: v0,
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: UI_MODE_SOLID,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: white,
        },
        Vertex {
            position: v1,
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: UI_MODE_SOLID,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: white,
        },
        Vertex {
            position: v2,
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: UI_MODE_SOLID,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: white,
        },
        Vertex {
            position: v3,
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: UI_MODE_SOLID,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: white,
        },
    ];

    let indices = vec![
        0, 1, 2, 0, 2, 3, // horizontal
        4, 5, 6, 4, 6, 7, // vertical
    ];

    (vertices, indices)
}

#[allow(dead_code)]
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

    let center_x = 1.0 - (PREVIEW_MARGIN_RIGHT_PX * px_to_ndc_x) - half_w;
    let center_y = 1.0 - (PREVIEW_MARGIN_TOP_PX * px_to_ndc_y) - half_h;

    let mut local = vec![
        [-half_w, -half_h, 0.0],
        [half_w, -half_h, 0.0],
        [half_w, half_h, 0.0],
        [-half_w, half_h, 0.0],
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
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: base,
        },
        Vertex {
            position: local[1],
            uv: uv(1.0, 0.0),
            tex_layer: texture_layer,
            tint_kind: UI_MODE_TEXTURED,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: base,
        },
        Vertex {
            position: local[2],
            uv: uv(1.0, 1.0),
            tex_layer: texture_layer,
            tint_kind: UI_MODE_TEXTURED,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: base,
        },
        Vertex {
            position: local[3],
            uv: uv(0.0, 1.0),
            tex_layer: texture_layer,
            tint_kind: UI_MODE_TEXTURED,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: base,
        },
        Vertex {
            position: local[4],
            uv: uv(0.0, 0.0),
            tex_layer: texture_layer,
            tint_kind: UI_MODE_TEXTURED,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: accent,
        },
        Vertex {
            position: local[5],
            uv: uv(stripe_uv_span, 0.0),
            tex_layer: texture_layer,
            tint_kind: UI_MODE_TEXTURED,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: accent,
        },
        Vertex {
            position: local[6],
            uv: uv(stripe_uv_span, 1.0),
            tex_layer: texture_layer,
            tint_kind: UI_MODE_TEXTURED,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: accent,
        },
        Vertex {
            position: local[7],
            uv: uv(0.0, 1.0),
            tex_layer: texture_layer,
            tint_kind: UI_MODE_TEXTURED,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: accent,
        },
    ];

    let indices = vec![
        0, 1, 2, 0, 2, 3, // main quad
        4, 5, 6, 4, 6, 7, // accent stripe
    ];

    (vertices, indices)
}

fn append_preview_textured_quad(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    corners: [[f32; 3]; 4],
    uvs: [[f32; 2]; 4],
    tex_layer: u32,
    tint_kind: u32,
    light_packed: u32,
) {
    let base = vertices.len() as u32;
    let color = [1.0, 1.0, 1.0];
    for i in 0..4 {
        vertices.push(Vertex {
            position: corners[i],
            uv: uvs[i],
            tex_layer,
            tint_kind,
            color,
            light_packed,
        });
    }
    indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

fn preview_face_uv_basis(face: Face) -> ([i32; 3], [i32; 3]) {
    match face {
        Face::North => ([1, 0, 0], [0, -1, 0]),
        Face::South => ([-1, 0, 0], [0, -1, 0]),
        Face::West => ([0, 0, 1], [0, -1, 0]),
        Face::East => ([0, 0, -1], [0, -1, 0]),
        Face::Up => ([1, 0, 0], [0, 0, 1]),
        Face::Down => ([1, 0, 0], [0, 0, -1]),
    }
}

fn preview_project_axis(p: [i32; 3], axis: [i32; 3]) -> i32 {
    p[0] * axis[0] + p[1] * axis[1] + p[2] * axis[2]
}

fn preview_face_uvs_for_quad(face: Face, corners: [[i32; 3]; 4]) -> [[f32; 2]; 4] {
    let (u_axis, v_axis) = preview_face_uv_basis(face);
    let mut uvs = [[0.0; 2]; 4];
    let mut projected = [(0_i32, 0_i32); 4];
    let mut u_min = i32::MAX;
    let mut v_min = i32::MAX;

    for (i, corner) in corners.iter().enumerate() {
        let u = preview_project_axis(*corner, u_axis);
        let v = preview_project_axis(*corner, v_axis);
        projected[i] = (u, v);
        u_min = u_min.min(u);
        v_min = v_min.min(v);
    }

    for (i, (u, v)) in projected.iter().enumerate() {
        uvs[i] = [(*u - u_min) as f32, (*v - v_min) as f32];
    }

    uvs
}

fn build_block_preview_mesh_3d(
    block: BlockId,
    block_texture_lookup: &BlockTextureLookup,
) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(64);
    let mut indices = Vec::with_capacity(96);

    match block_texture_lookup.model_for(block) {
        BlockModel::CrossPlant => {
            let layer = block_texture_lookup.layer_for(block, Face::North);
            let quad_uvs = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];
            append_preview_textured_quad(
                &mut vertices,
                &mut indices,
                [
                    [-0.5, -0.5, -0.5],
                    [0.5, -0.5, 0.5],
                    [0.5, 0.5, 0.5],
                    [-0.5, 0.5, -0.5],
                ],
                quad_uvs,
                layer,
                TINT_NONE,
                FULLBRIGHT_LIGHT_PACKED,
            );
            append_preview_textured_quad(
                &mut vertices,
                &mut indices,
                [
                    [0.5, -0.5, -0.5],
                    [-0.5, -0.5, 0.5],
                    [-0.5, 0.5, 0.5],
                    [0.5, 0.5, -0.5],
                ],
                quad_uvs,
                layer,
                TINT_NONE,
                FULLBRIGHT_LIGHT_PACKED,
            );
        }
        BlockModel::Cube | BlockModel::Liquid { .. } => {
            let faces = [
                (Face::South, [[0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]]),
                (Face::North, [[1, 0, 0], [0, 0, 0], [0, 1, 0], [1, 1, 0]]),
                (Face::East, [[1, 0, 1], [1, 0, 0], [1, 1, 0], [1, 1, 1]]),
                (Face::West, [[0, 0, 0], [0, 0, 1], [0, 1, 1], [0, 1, 0]]),
                (Face::Up, [[0, 1, 1], [1, 1, 1], [1, 1, 0], [0, 1, 0]]),
                (Face::Down, [[0, 0, 0], [1, 0, 0], [1, 0, 1], [0, 0, 1]]),
            ];
            for (face, corners) in faces {
                let corners_f32 =
                    corners.map(|p| [p[0] as f32 - 0.5, p[1] as f32 - 0.5, p[2] as f32 - 0.5]);
                let quad_uvs = preview_face_uvs_for_quad(face, corners);
                append_preview_textured_quad(
                    &mut vertices,
                    &mut indices,
                    corners_f32,
                    quad_uvs,
                    block_texture_lookup.layer_for(block, face),
                    block_texture_lookup.tint_kind_for(block, face),
                    FULLBRIGHT_LIGHT_PACKED,
                );
            }
        }
    }

    (vertices, indices)
}

fn preview_viewport_rect_px(view_width: u32, view_height: u32) -> (u32, u32, u32, u32) {
    let preview_size_px = PREVIEW_SIZE_PX * PREVIEW_3D_SIZE_SCALE;
    let max_x = view_width.saturating_sub(1);
    let max_y = view_height.saturating_sub(1);
    let x = (view_width as f32 - PREVIEW_MARGIN_RIGHT_PX - preview_size_px)
        .max(0.0)
        .round() as u32;
    let y = PREVIEW_MARGIN_TOP_PX.max(0.0).round() as u32;
    let x = x.min(max_x);
    let y = y.min(max_y);
    let w = (preview_size_px.max(1.0).round() as u32).min(view_width.saturating_sub(x).max(1));
    let h = (preview_size_px.max(1.0).round() as u32).min(view_height.saturating_sub(y).max(1));
    (x, y, w, h)
}

fn append_ui_quad(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    min: [f32; 2],
    max: [f32; 2],
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    tex_layer: u32,
    mode: u32,
    color: [f32; 3],
) {
    let base = vertices.len() as u32;
    let z = 0.0;
    vertices.push(Vertex {
        position: [min[0], min[1], z],
        uv: [uv_min[0], uv_max[1]],
        tex_layer,
        tint_kind: mode,
        light_packed: FULLBRIGHT_LIGHT_PACKED,
        color,
    });
    vertices.push(Vertex {
        position: [max[0], min[1], z],
        uv: [uv_max[0], uv_max[1]],
        tex_layer,
        tint_kind: mode,
        light_packed: FULLBRIGHT_LIGHT_PACKED,
        color,
    });
    vertices.push(Vertex {
        position: [max[0], max[1], z],
        uv: [uv_max[0], uv_min[1]],
        tex_layer,
        tint_kind: mode,
        light_packed: FULLBRIGHT_LIGHT_PACKED,
        color,
    });
    vertices.push(Vertex {
        position: [min[0], max[1], z],
        uv: [uv_min[0], uv_min[1]],
        tex_layer,
        tint_kind: mode,
        light_packed: FULLBRIGHT_LIGHT_PACKED,
        color,
    });
    indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

fn append_ui_solid_quad(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    min: [f32; 2],
    max: [f32; 2],
    color: [f32; 3],
) {
    append_ui_quad(
        vertices,
        indices,
        min,
        max,
        [0.0, 0.0],
        [0.0, 0.0],
        0,
        UI_MODE_SOLID,
        color,
    );
}

fn px_to_ndc_x(view_width: u32, x: f32) -> f32 {
    (x / view_width.max(1) as f32) * 2.0 - 1.0
}

fn px_to_ndc_y(view_height: u32, y: f32) -> f32 {
    1.0 - (y / view_height.max(1) as f32) * 2.0
}

fn append_ui_solid_quad_px(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    view_width: u32,
    view_height: u32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: [f32; 3],
) {
    let min = [px_to_ndc_x(view_width, x), px_to_ndc_y(view_height, y + h)];
    let max = [px_to_ndc_x(view_width, x + w), px_to_ndc_y(view_height, y)];
    append_ui_solid_quad(vertices, indices, min, max, color);
}

fn append_ui_textured_quad_px(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    view_width: u32,
    view_height: u32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    tex_layer: u32,
    color: [f32; 3],
) {
    let min = [px_to_ndc_x(view_width, x), px_to_ndc_y(view_height, y + h)];
    let max = [px_to_ndc_x(view_width, x + w), px_to_ndc_y(view_height, y)];
    append_ui_quad(
        vertices,
        indices,
        min,
        max,
        uv_min,
        uv_max,
        tex_layer,
        UI_MODE_TEXTURED,
        color,
    );
}

fn build_bitmap_font(texture_layer: u32, rgba: &[u8], width: u32, height: u32) -> BitmapFont {
    let cell_w = (width / 16).max(1);
    let cell_h = (height / 16).max(1);
    // Some packs upscale ascii.png (e.g. 512x512). Keep logical 8x8 glyph metrics.
    let texel_scale_x = (cell_w / 8).max(1);
    let texel_scale_y = (cell_h / 8).max(1);
    let mut glyph_widths = [8_u8; 256];

    for glyph in 0..256_u32 {
        if glyph == b' ' as u32 {
            glyph_widths[glyph as usize] = 4;
            continue;
        }
        let col = glyph % 16;
        let row = glyph / 16;
        let base_x = col * cell_w;
        let base_y = row * cell_h;

        let mut last_nonempty = -1_i32;
        for x in 0..cell_w {
            let mut has_pixel = false;
            for y in 0..cell_h {
                let px = base_x + x;
                let py = base_y + y;
                let alpha_idx = ((py * width + px) * 4 + 3) as usize;
                if alpha_idx < rgba.len() && rgba[alpha_idx] > 0 {
                    has_pixel = true;
                    break;
                }
            }
            if has_pixel {
                last_nonempty = x as i32;
            }
        }

        glyph_widths[glyph as usize] = if last_nonempty >= 0 {
            let logical_last = (last_nonempty as u32 / texel_scale_x) as i32;
            (logical_last + 2).clamp(2, 8) as u8
        } else {
            2
        };
    }

    BitmapFont {
        texture_layer,
        cell_w,
        cell_h,
        texel_scale_x,
        texel_scale_y,
        glyph_h: (cell_h / texel_scale_y).clamp(1, 8) as u8,
        glyph_widths,
    }
}

fn glyph_advance_px(font: &BitmapFont, ch: char, scale: f32) -> f32 {
    let idx = if ch.is_ascii() {
        ch as usize
    } else {
        b'?' as usize
    };
    font.glyph_widths[idx].max(1) as f32 * scale
}

fn bitmap_text_width_px(font: &BitmapFont, text: &str, scale: f32) -> f32 {
    text.chars()
        .map(|ch| glyph_advance_px(font, ch, scale))
        .sum()
}

fn append_bitmap_text(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    view_width: u32,
    view_height: u32,
    font: &BitmapFont,
    text: &str,
    x: f32,
    y: f32,
    scale: f32,
    color: [f32; 3],
) {
    let cell_w = font.cell_w as f32;
    let cell_h = font.cell_h as f32;
    let glyph_h = font.glyph_h as f32 * scale;
    let sampled_glyph_h = font.glyph_h as f32 * font.texel_scale_y as f32;
    let mut cursor_x = x;
    for ch in text.chars() {
        let idx = if ch.is_ascii() { ch as u8 } else { b'?' };
        let glyph_px_w = font.glyph_widths[idx as usize].max(1) as f32;
        let sampled_glyph_w = glyph_px_w * font.texel_scale_x as f32;
        let advance = glyph_px_w * scale;
        if ch == ' ' {
            cursor_x += advance;
            continue;
        }

        let col = (idx % 16) as f32;
        let row = (idx / 16) as f32;
        let uv_min = [col / 16.0, row / 16.0];
        let uv_max = [
            uv_min[0] + (sampled_glyph_w / cell_w) * (1.0 / 16.0),
            uv_min[1] + (sampled_glyph_h / cell_h) * (1.0 / 16.0),
        ];
        append_ui_textured_quad_px(
            vertices,
            indices,
            view_width,
            view_height,
            cursor_x,
            y,
            glyph_px_w * scale,
            glyph_h,
            uv_min,
            uv_max,
            font.texture_layer,
            color,
        );
        cursor_x += advance;
    }
}

fn append_bitmap_text_with_shadow(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    view_width: u32,
    view_height: u32,
    font: &BitmapFont,
    text: &str,
    x: f32,
    y: f32,
    scale: f32,
) {
    append_bitmap_text(
        vertices,
        indices,
        view_width,
        view_height,
        font,
        text,
        x + WORLDGEN_TEXT_SHADOW_OFFSET_PX,
        y + WORLDGEN_TEXT_SHADOW_OFFSET_PX,
        scale,
        WORLDGEN_TEXT_SHADOW,
    );
    append_bitmap_text(
        vertices,
        indices,
        view_width,
        view_height,
        font,
        text,
        x,
        y,
        scale,
        WORLDGEN_TEXT_MAIN,
    );
}

fn build_generating_screen_mesh(
    view_width: u32,
    view_height: u32,
    dirt_layer: u32,
    font: &BitmapFont,
    progress01: f32,
    phase: &str,
) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(2048);
    let mut indices = Vec::with_capacity(3072);

    let dirt_tile_u = (view_width as f32 / WORLDGEN_DIRT_TILE_PX).max(1.0);
    let dirt_tile_v = (view_height as f32 / WORLDGEN_DIRT_TILE_PX).max(1.0);
    append_ui_quad(
        &mut vertices,
        &mut indices,
        [-1.0, -1.0],
        [1.0, 1.0],
        [0.0, 0.0],
        [dirt_tile_u, dirt_tile_v],
        dirt_layer,
        UI_MODE_TEXTURED,
        [1.0, 1.0, 1.0],
    );

    let cx = view_width as f32 * 0.5;
    let cy = view_height as f32 * 0.5;

    let title = "Generating level";
    let title_x = cx - bitmap_text_width_px(font, title, WORLDGEN_FONT_SCALE) * 0.5;
    let title_y = cy - 40.0;
    append_bitmap_text_with_shadow(
        &mut vertices,
        &mut indices,
        view_width,
        view_height,
        font,
        title,
        title_x,
        title_y,
        WORLDGEN_FONT_SCALE,
    );

    let mut phase_text = String::with_capacity(phase.len() + 2);
    if phase.trim().is_empty() {
        phase_text.push_str("Working");
    } else {
        phase_text.push_str(phase.trim());
    }
    if !phase_text.ends_with("..") {
        phase_text.push_str("..");
    }

    let phase_x = cx - bitmap_text_width_px(font, &phase_text, WORLDGEN_FONT_SCALE) * 0.5;
    let phase_y = cy - 8.0;
    append_bitmap_text_with_shadow(
        &mut vertices,
        &mut indices,
        view_width,
        view_height,
        font,
        &phase_text,
        phase_x,
        phase_y,
        WORLDGEN_FONT_SCALE,
    );

    let bar_x = cx - WORLDGEN_BAR_WIDTH_PX * 0.5;
    let bar_y = cy + 18.0;
    append_ui_solid_quad_px(
        &mut vertices,
        &mut indices,
        view_width,
        view_height,
        bar_x - 1.0,
        bar_y - 1.0,
        WORLDGEN_BAR_WIDTH_PX + 2.0,
        WORLDGEN_BAR_HEIGHT_PX + 2.0,
        WORLDGEN_BAR_BORDER,
    );
    append_ui_solid_quad_px(
        &mut vertices,
        &mut indices,
        view_width,
        view_height,
        bar_x,
        bar_y,
        WORLDGEN_BAR_WIDTH_PX,
        WORLDGEN_BAR_HEIGHT_PX,
        WORLDGEN_BAR_BG,
    );
    let fill_w = (WORLDGEN_BAR_WIDTH_PX * progress01.clamp(0.0, 1.0)).round();
    if fill_w > 0.0 {
        append_ui_solid_quad_px(
            &mut vertices,
            &mut indices,
            view_width,
            view_height,
            bar_x,
            bar_y,
            fill_w,
            WORLDGEN_BAR_HEIGHT_PX,
            WORLDGEN_BAR_FILL,
        );
    }

    (vertices, indices)
}

fn pause_menu_button_rect_px(
    view_width: u32,
    view_height: u32,
    item_count: usize,
    index: usize,
) -> Option<(f32, f32, f32, f32)> {
    if item_count == 0 || index >= item_count {
        return None;
    }

    let cx = view_width as f32 * 0.5;
    let cy = view_height as f32 * 0.5;
    let count = item_count as f32;
    let total_h =
        count * PAUSE_MENU_BUTTON_HEIGHT_PX + (count - 1.0).max(0.0) * PAUSE_MENU_BUTTON_SPACING_PX;
    let button_x = cx - PAUSE_MENU_BUTTON_WIDTH_PX * 0.5;
    let start_y = cy - total_h * 0.5 - 2.0;
    let y = start_y + index as f32 * (PAUSE_MENU_BUTTON_HEIGHT_PX + PAUSE_MENU_BUTTON_SPACING_PX);
    Some((
        button_x,
        y,
        PAUSE_MENU_BUTTON_WIDTH_PX,
        PAUSE_MENU_BUTTON_HEIGHT_PX,
    ))
}

pub(crate) fn pause_menu_item_at_pos_px(
    view_width: u32,
    view_height: u32,
    item_count: usize,
    x: f32,
    y: f32,
) -> Option<usize> {
    for index in 0..item_count {
        let Some((bx, by, bw, bh)) =
            pause_menu_button_rect_px(view_width, view_height, item_count, index)
        else {
            continue;
        };
        if x >= bx && x <= bx + bw && y >= by && y <= by + bh {
            return Some(index);
        }
    }
    None
}

fn build_pause_menu_mesh(
    view_width: u32,
    view_height: u32,
    dirt_layer: u32,
    font: &BitmapFont,
    selected: usize,
    items: &[&str],
) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(4096);
    let mut indices = Vec::with_capacity(6144);

    let dirt_tile_u = (view_width as f32 / WORLDGEN_DIRT_TILE_PX).max(1.0);
    let dirt_tile_v = (view_height as f32 / WORLDGEN_DIRT_TILE_PX).max(1.0);
    append_ui_quad(
        &mut vertices,
        &mut indices,
        [-1.0, -1.0],
        [1.0, 1.0],
        [0.0, 0.0],
        [dirt_tile_u, dirt_tile_v],
        dirt_layer,
        UI_MODE_TEXTURED,
        [1.0, 1.0, 1.0],
    );

    let cx = view_width as f32 * 0.5;
    let cy = view_height as f32 * 0.5;

    let text_scale = WORLDGEN_FONT_SCALE;
    let glyph_h = font.glyph_h as f32 * text_scale;
    let title_x = cx - bitmap_text_width_px(font, PAUSE_MENU_TITLE, WORLDGEN_FONT_SCALE) * 0.5;
    let title_y = pause_menu_button_rect_px(view_width, view_height, items.len(), 0)
        .map(|(_, button_y, _, _)| button_y - glyph_h - 18.0)
        .unwrap_or(cy - 64.0);
    append_bitmap_text_with_shadow(
        &mut vertices,
        &mut indices,
        view_width,
        view_height,
        font,
        PAUSE_MENU_TITLE,
        title_x,
        title_y,
        WORLDGEN_FONT_SCALE,
    );

    if items.is_empty() {
        return (vertices, indices);
    }

    let selected_index = selected.min(items.len().saturating_sub(1));

    for (index, label) in items.iter().enumerate() {
        let Some((button_x, y, button_w, button_h)) =
            pause_menu_button_rect_px(view_width, view_height, items.len(), index)
        else {
            continue;
        };
        append_ui_solid_quad_px(
            &mut vertices,
            &mut indices,
            view_width,
            view_height,
            button_x - 1.0,
            y - 1.0,
            button_w + 2.0,
            button_h + 2.0,
            PAUSE_MENU_BUTTON_BORDER,
        );
        append_ui_solid_quad_px(
            &mut vertices,
            &mut indices,
            view_width,
            view_height,
            button_x,
            y,
            button_w,
            button_h,
            if index == selected_index {
                PAUSE_MENU_BUTTON_SELECTED
            } else {
                PAUSE_MENU_BUTTON_NORMAL
            },
        );

        let label_x = cx - bitmap_text_width_px(font, label, text_scale) * 0.5;
        let label_y = y + (button_h - glyph_h) * 0.5;
        append_bitmap_text_with_shadow(
            &mut vertices,
            &mut indices,
            view_width,
            view_height,
            font,
            label,
            label_x,
            label_y,
            text_scale,
        );
    }

    (vertices, indices)
}

fn build_coords_overlay_mesh(
    view_width: u32,
    view_height: u32,
    font: &BitmapFont,
    camera: &Camera,
) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(512);
    let mut indices = Vec::with_capacity(768);

    let world_x = camera.position.x;
    let world_y = camera.position.y;
    let world_z = camera.position.z;
    let chunk = World::world_to_chunk_coord(IVec3::new(
        world_x.floor() as i32,
        world_y.floor() as i32,
        world_z.floor() as i32,
    ));
    let text = format!(
        "XYZ {:.1} {:.1} {:.1}  C {} {} {}",
        world_x, world_y, world_z, chunk.x, chunk.y, chunk.z
    );
    append_bitmap_text_with_shadow(
        &mut vertices,
        &mut indices,
        view_width,
        view_height,
        font,
        &text,
        COORDS_PAD_X,
        COORDS_PAD_Y,
        COORDS_TEXT_SCALE,
    );

    (vertices, indices)
}

fn build_debug_overlay_mesh(
    view_width: u32,
    view_height: u32,
    font: &BitmapFont,
    lines: &[&str],
) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(2048);
    let mut indices = Vec::with_capacity(3072);
    let scale = DEBUG_OVERLAY_TEXT_SCALE;
    let line_h = font.glyph_h as f32 * scale + 6.0;

    for (line_index, line) in lines.iter().enumerate() {
        if line.is_empty() {
            continue;
        }
        append_bitmap_text_with_shadow(
            &mut vertices,
            &mut indices,
            view_width,
            view_height,
            font,
            line,
            COORDS_PAD_X,
            COORDS_PAD_Y + line_index as f32 * line_h,
            scale,
        );
    }

    (vertices, indices)
}

/// ============================
/// Render State (wgpu)
/// ============================
pub(crate) struct State {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,

    // Pipeline stuff
    render_pipeline_back_depth: wgpu::RenderPipeline,
    render_pipeline_no_cull_depth: wgpu::RenderPipeline,
    render_pipeline_back_no_depth: wgpu::RenderPipeline,
    render_pipeline_no_cull_no_depth: wgpu::RenderPipeline,
    render_pipeline_transparent: wgpu::RenderPipeline,
    render_pipeline_lod: wgpu::RenderPipeline,
    sky_pipeline: wgpu::RenderPipeline,
    highlight_pipeline: wgpu::RenderPipeline,
    outline_pipeline: wgpu::RenderPipeline,
    ui_pipeline: wgpu::RenderPipeline,
    debug_backface_culling: bool,
    debug_depth_test: bool,
    pub(crate) world: World,
    zero_light_chunk_data: Arc<Vec<u8>>,
    chunk_meshes: HashMap<ChunkCoord, LayeredChunkMesh>,
    full_solid_mesh_skip_set: HashSet<ChunkCoord>,
    lod_meshes: HashMap<LodColumnCoord, ChunkMesh>,
    dirty_chunk_queue: VecDeque<ChunkCoord>,
    dirty_chunk_set: HashSet<ChunkCoord>,
    deferred_mesh_until_frame: HashMap<ChunkCoord, u64>,
    pending_block_edits: VecDeque<PendingBlockEdit>,
    pending_block_edit_dirty_chunks: HashSet<ChunkCoord>,
    pending_block_edit_dirty_columns: HashSet<(i32, i32)>,
    pending_sand_gravity_checks: VecDeque<PendingSandGravityCheck>,
    pending_sand_gravity_check_set: HashSet<IVec3>,
    highlight_mesh: Option<ChunkMesh>,
    outline_target_block: Option<IVec3>,
    outline_vertex_buffer: wgpu::Buffer,
    crosshair_mesh: ChunkMesh,
    preview_mesh: ChunkMesh,
    preview_block: BlockId,
    _texture_registry: TextureRegistry,
    _world_texture_array: TextureArray,
    _sky_texture_array: TextureArray,
    _worldgen_ui_texture_array: TextureArray,
    water_overlay_mesh: ChunkMesh,
    boundary_ocean_mesh: Option<ChunkMesh>,
    sky_object_vertex_buffer: wgpu::Buffer,
    sky_object_index_buffer: wgpu::Buffer,
    world_texture_bind_group: wgpu::BindGroup,
    sky_texture_bind_group: wgpu::BindGroup,
    worldgen_ui_texture_bind_group: wgpu::BindGroup,
    _sky_anim_buffer: wgpu::Buffer,
    _worldgen_ui_anim_buffer: wgpu::Buffer,
    worldgen_dirt_layer: u32,
    sky_sun_layer: u32,
    sky_moon_layer: u32,
    sky_sun_uv_rect: [f32; 4],
    moon_phase_atlas_size: [u32; 2],
    worldgen_font: BitmapFont,
    block_texture_lookup: Arc<BlockTextureLookup>,
    _mesh_pool: MeshWorkerPool,
    _chunkgen_pool: ChunkGenWorkerPool,
    _lighting_pool: LightingWorkerPool,
    mesh_results: CrossbeamReceiver<MeshResult>,
    chunkgen_results: CrossbeamReceiver<ChunkGenResult>,
    lighting_results: CrossbeamReceiver<LightingResult>,
    mesh_jobs: CrossbeamSender<MeshJob>,
    chunkgen_jobs: CrossbeamSender<ChunkGenJob>,
    lighting_jobs: CrossbeamSender<LightingJob>,
    inflight: HashMap<ChunkCoord, u64>,
    chunkgen_inflight: HashSet<ChunkCoord>,
    lod_chunkgen_inflight: HashSet<LodColumnCoord>,
    generated_empty_chunks: HashSet<ChunkCoord>,
    mesh_upload_queue: VecDeque<MeshResult>,
    texture_anim_tracks: Vec<TextureAnimTrack>,
    texture_anim_gpu: Vec<AnimGpu>,
    texture_anim_accum: f32,
    texture_anim_buffer: wgpu::Buffer,
    fluid_accum: f32,
    sim_tick_index: u64,
    sim_systems: Vec<Box<dyn SimSystem>>,
    fluids_system_index: Option<usize>,
    block_entities_system_index: Option<usize>,
    random_ticks_system_index: Option<usize>,
    fluid_debug_counts: FluidDebugCounts,
    frame_index: u64,
    // Frustum culling performance counters
    frustum_culled_chunks_total: u64,
    meshes_skipped_due_to_frustum: u64,
    // Coalesces fluid-driven mesh rebuilds so active fluids do not thrash chunk meshing every tick.
    fluid_mesh_coalesce: HashMap<ChunkCoord, FluidMeshCoalesceEntry>,
    // Marks queue entries that were enqueued by the fluid coalescer (so dispatch can be throttled).
    fluid_dirty_due_to_fluids: HashSet<ChunkCoord>,
    particles: Vec<Particle>,
    particle_rng: u32,
    render_distance: RenderDistance,
    view_distance_chunks: i32,
    simulation_distance: SimulationDistance,
    simulation_distance_chunks: i32,
    active_sim_columns: HashSet<ColumnCoord>,
    last_sim_center: Option<ColumnCoord>,
    sim_columns_entered: Vec<ColumnCoord>,
    sim_columns_exited: Vec<ColumnCoord>,
    worldgen_preset: WorldgenPreset,
    world_generation_task: Option<WorldGenerationTask>,
    last_stream_center: Option<(i32, i32, i32)>,
    last_stream_radius: i32,
    radius_offsets_cache: Vec<(i32, i32)>,
    lod_ring_offsets_cache: Vec<(i32, i32)>,
    lod_request_cursor: usize,
    chunkgen_pending_set: HashSet<ChunkCoord>,
    chunkgen_pending_enqueued_frame: HashMap<ChunkCoord, u64>,
    chunkgen_pending_heap: BinaryHeap<ChunkPriorityHeapEntry>,
    chunkgen_heap_valid_for_center: Option<(i32, i32, i32)>,
    chunkgen_heap_valid_for_radius: i32,
    chunkgen_heap_last_rebuild_frame: u64,
    chunkgen_stream_epoch: u64,
    last_lod_stream_center: Option<(i32, i32)>,
    far_lod_enabled: bool,
    far_lod_requests_per_frame: usize,
    pending_lod_mesh_uploads: VecDeque<(LodColumnCoord, CpuMeshData)>,
    stream_last_camera_y: Option<f32>,
    stream_recent_descend_frames: u32,
    stream_recent_mining_frames: u32,
    stream_horizon_sector_max_elev: [f32; STREAM_HORIZON_SECTOR_COUNT],
    stream_horizon_has_data: bool,
    stream_horizon_built_frame: u64,
    stream_horizon_seen_columns: HashSet<(i32, i32)>,
    mesh_debug_epoch: u32,
    lod_cached_near_radius: i32,
    lod_cached_far_radius: i32,
    frame_perf: StateFramePerf,
    last_render_stats_report: Instant,
    last_lighting_step_ms: f32,
    lighting_job_inflight: bool,
    lighting_generation: u64,
    lighting_request_pending: bool,
    lighting_rebuild_not_before: Option<Instant>,
    pending_lighting_dirty_chunks: VecDeque<ChunkCoord>,
    pending_lighting_dirty_set: HashSet<ChunkCoord>,
    debug_disable_ao: bool,
    debug_lighting_view: LightingDebugView,
    pub(crate) world_time: WorldTime,
    storage_reader: RegionStorage,
    storage_loader: StorageLoadWorker,
    storage_writer: StorageWorker,
    saved_revision: HashMap<ChunkCoord, u64>,
    saved_column_missing_cache: HashMap<(i32, i32), Instant>,
    saved_column_loads_inflight: HashSet<(i32, i32)>,
    saved_load_request_cursor: usize,
    saved_load_requests_in_window: usize,
    saved_load_requests_window_started: Instant,
    dirty_columns: HashSet<(i32, i32)>,
    dirty_column_queue: VecDeque<(i32, i32)>,
    dirty_column_generation: HashMap<(i32, i32), u64>,
    next_dirty_column_generation: u64,
    save_columns_inflight: HashMap<(i32, i32), u64>,
    pending_unload_columns: HashSet<(i32, i32)>,
    pending_unload_queue: VecDeque<(i32, i32)>,
    pending_unload_chunks: VecDeque<ChunkCoord>,
    unload_scan_keys: Vec<ChunkCoord>,
    unload_scan_cursor: usize,
    unload_scan_last_refresh_frame: u64,
    prune_mesh_keys: Vec<ChunkCoord>,
    prune_mesh_cursor: usize,
    prune_mesh_last_target: Option<(i32, i32, i32)>,
    prune_mesh_last_scan_frame: u64,
    save_epoch: u64,
    autosave_sweep_active: bool,
    last_autosave_at: Instant,
    autosave_interval: Duration,

    // Camera uniform
    camera_uniform: CameraUniform,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    preview_camera_uniform: CameraUniform,
    preview_camera_buffer: wgpu::Buffer,
    preview_camera_bind_group: wgpu::BindGroup,

    // Depth buffer
    depth: DepthTexture,
}

impl State {
    fn world_meta_path() -> PathBuf {
        PathBuf::from(SAVE_ROOT_DIR).join(WORLD_META_FILE_NAME)
    }

    fn world_manifest_path() -> PathBuf {
        PathBuf::from(SAVE_ROOT_DIR).join(WORLD_MANIFEST_FILE_NAME)
    }

    fn ticking_areas_path() -> PathBuf {
        PathBuf::from(SAVE_ROOT_DIR).join(TICKING_AREAS_FILE_NAME)
    }

    fn build_world_meta(world: &World, seed: u64, preset: WorldgenPreset) -> WorldMeta {
        let cfg = world.config();
        let mut meta = WorldMeta::new(
            seed,
            preset,
            world.sea_level(),
            world.min_y(),
            world.max_y(),
            world.chunk_size(),
            cfg.boundary_ocean(),
            cfg.boundary_radius_chunks(),
        )
        .unwrap_or_else(|err| {
            panic!(
                "failed to build world.meta from runtime world config (seed={}, preset={:?}): {}",
                seed, preset, err
            )
        });
        meta.block_registry_hash = Some(world.manifest().content_signature.clone());
        meta
    }

    fn world_config_from_meta(meta: &WorldMeta) -> WorldConfig {
        if meta.chunk_size != CHUNK_SIZE {
            panic!(
                "world.meta is incompatible with this build: expected chunk_size={}, got chunk_size={}",
                CHUNK_SIZE, meta.chunk_size
            );
        }

        WorldConfig {
            seed: meta.world_seed,
            sea_level: meta.sea_level,
            min_y: meta.min_y,
            max_y: meta.max_y,
            chunk_size: meta.chunk_size,
            boundary_ocean: meta.boundary_ocean,
            boundary_radius_chunks: meta.boundary_radius_chunks,
        }
    }

    fn boundary_ocean_defaults_for_preset(preset: WorldgenPreset) -> (bool, i32) {
        match preset {
            WorldgenPreset::Rd161348Like => (true, 128),
            _ => (false, 128),
        }
    }

    fn load_ticking_areas_into_world(world: &mut World) {
        let path = Self::ticking_areas_path();
        match ticking_areas::load(&path) {
            Ok(Some(areas)) => {
                let count = areas.len();
                world.replace_ticking_areas(areas);
                println!(
                    "ticking areas: loaded {} from {}",
                    count,
                    path.display()
                );
            }
            Ok(None) => {}
            Err(err) => {
                eprintln!("ticking areas: failed to load {}: {}", path.display(), err);
            }
        }
    }

    fn save_ticking_areas(&self) -> io::Result<()> {
        ticking_areas::save_atomic(&Self::ticking_areas_path(), self.world.ticking_areas())
    }

    fn rebuild_boundary_ocean_mesh(&mut self) {
        let cfg = self.world.config();
        if !cfg.boundary_ocean() || cfg.boundary_radius_chunks() <= 0 {
            self.boundary_ocean_mesh = None;
            return;
        }
        let inner = cfg.boundary_radius_chunks() as f32 * CHUNK_SIZE as f32;
        let outer = inner + BOUNDARY_OCEAN_OUTER_PADDING_BLOCKS;
        let y = cfg.sea_level() as f32;
        let layer = self.block_texture_lookup.layer_for(BlockId::WaterStill, Face::Up);
        let (vertices, indices) = build_boundary_ocean_ring_mesh(layer, y, inner, outer);
        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Boundary Ocean Vertex Buffer"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Boundary Ocean Index Buffer"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        self.boundary_ocean_mesh = Some(ChunkMesh {
            vertex_buffer,
            index_buffer,
            index_count: indices.len() as u32,
            vertex_capacity_bytes: std::mem::size_of_val(vertices.as_slice()) as u64,
            index_capacity_bytes: std::mem::size_of_val(indices.as_slice()) as u64,
        });
    }

    fn mark_column_saved_revisions(&mut self, cx: i32, cz: i32) {
        for cy in self.world.min_chunk_y()..=self.world.max_chunk_y() {
            let coord = ChunkCoord {
                x: cx,
                y: cy,
                z: cz,
            };
            if let Some(chunk) = self.world.get_chunk(coord) {
                self.saved_revision.insert(coord, chunk.revision);
            } else {
                self.saved_revision.remove(&coord);
            }
        }
    }

    fn apply_saved_column_completion(&mut self, completion: &ColumnSaveCompletion) {
        let mut revisions_by_cy = HashMap::with_capacity(completion.section_revisions.len());
        for &(cy, revision) in &completion.section_revisions {
            revisions_by_cy.insert(cy, revision);
        }
        for cy in completion.min_section_y..=completion.max_section_y {
            let coord = ChunkCoord {
                x: completion.cx,
                y: cy,
                z: completion.cz,
            };
            if let Some(revision) = revisions_by_cy.get(&cy).copied() {
                self.saved_revision.insert(coord, revision);
            } else {
                self.saved_revision.remove(&coord);
            }
        }
    }

    fn mark_column_dirty(&mut self, cx: i32, cz: i32) {
        let key = (cx, cz);
        let generation = self.next_dirty_column_generation;
        self.next_dirty_column_generation =
            self.next_dirty_column_generation.wrapping_add(1).max(1);
        self.dirty_column_generation.insert(key, generation);
        if self.dirty_columns.insert(key) {
            self.dirty_column_queue.push_back(key);
        }
    }

    fn mark_column_dirty_for_chunk(&mut self, coord: ChunkCoord) {
        self.mark_column_dirty(coord.x, coord.z);
    }

    fn requeue_dirty_column(&mut self, cx: i32, cz: i32) {
        if self.dirty_columns.contains(&(cx, cz)) {
            self.dirty_column_queue.push_back((cx, cz));
        }
    }

    fn clear_dirty_column_tracking(&mut self, cx: i32, cz: i32) {
        let key = (cx, cz);
        self.dirty_columns.remove(&key);
        self.dirty_column_generation.remove(&key);
        self.save_columns_inflight.remove(&key);
    }

    fn build_column_save_snapshot(
        &self,
        cx: i32,
        cz: i32,
        dirty_generation: u64,
    ) -> ColumnSaveSnapshot {
        let min_section_y = self.world.min_chunk_y();
        let max_section_y = self.world.max_chunk_y();
        let mut sections = Vec::new();
        for cy in min_section_y..=max_section_y {
            let coord = ChunkCoord {
                x: cx,
                y: cy,
                z: cz,
            };
            let Some(chunk) = self.world.get_chunk(coord) else {
                continue;
            };
            sections.push(ColumnSaveSectionSnapshot {
                cy,
                blocks: Arc::clone(&chunk.blocks),
                data: Arc::clone(&chunk.block_data),
                revision: chunk.revision,
            });
        }
        ColumnSaveSnapshot {
            cx,
            cz,
            min_y: self.world.min_y(),
            max_y: self.world.max_y(),
            sea_level: self.world.sea_level(),
            chunk_size: self.world.chunk_size(),
            min_section_y,
            max_section_y,
            dirty_generation,
            save_epoch: self.save_epoch,
            sections,
        }
    }

    fn queue_save_column_if_dirty(&mut self, cx: i32, cz: i32) -> bool {
        let key = (cx, cz);
        if !self.dirty_columns.contains(&key) || self.save_columns_inflight.contains_key(&key) {
            return false;
        }
        let dirty_generation = self.dirty_column_generation.get(&key).copied().unwrap_or(0);
        let snapshot = self.build_column_save_snapshot(cx, cz, dirty_generation);
        self.saved_column_missing_cache.remove(&(cx, cz));
        match self
            .storage_writer
            .enqueue_encode_and_store_column(snapshot)
        {
            Ok(()) => {
                self.save_columns_inflight.insert(key, dirty_generation);
                true
            }
            Err(err) => {
                self.saved_revision.clear();
                eprintln!("[storage][save] failed to queue column ({cx},{cz}): {err}");
                false
            }
        }
    }

    fn queue_dirty_columns_with_budget(&mut self, budget: usize) -> usize {
        if budget == 0 {
            return 0;
        }
        let mut queued = 0usize;
        let mut scanned = 0usize;
        let scan_budget = AUTOSAVE_QUEUE_SCAN_BUDGET_PER_FRAME.max(budget.saturating_mul(2));
        while queued < budget && scanned < scan_budget {
            let Some((cx, cz)) = self.dirty_column_queue.pop_front() else {
                break;
            };
            scanned += 1;
            if !self.dirty_columns.contains(&(cx, cz)) {
                continue;
            }
            if self.save_columns_inflight.contains_key(&(cx, cz)) {
                self.dirty_column_queue.push_back((cx, cz));
                continue;
            }
            if self.queue_save_column_if_dirty(cx, cz) {
                queued += 1;
            } else if self.dirty_columns.contains(&(cx, cz))
                && !self.save_columns_inflight.contains_key(&(cx, cz))
            {
                self.dirty_column_queue.push_back((cx, cz));
            }
        }
        queued
    }

    fn pump_storage_save_results(&mut self) {
        let _trace_scope = perf_trace::scope("state.pump_storage_save_results");
        let started = Instant::now();
        let backlog_before = self.storage_writer.save_result_queue_len_approx();
        self.frame_perf.storage_save_results_backlog_before = self
            .frame_perf
            .storage_save_results_backlog_before
            .max(backlog_before);
        let mut drained = 0usize;
        while drained < STORAGE_SAVE_RESULTS_PER_FRAME {
            let Some(StorageColumnSaveResult { completion, result }) =
                self.storage_writer.try_recv_save_result()
            else {
                break;
            };
            drained += 1;
            let key = (completion.cx, completion.cz);
            if completion.save_epoch != self.save_epoch {
                continue;
            }
            self.save_columns_inflight.remove(&key);

            match result {
                Ok(()) => {
                    self.frame_perf.storage_save_results_ok += 1;
                    self.apply_saved_column_completion(&completion);
                    self.saved_column_missing_cache.remove(&key);
                    let current_generation = self.dirty_column_generation.get(&key).copied();
                    if self.dirty_columns.contains(&key)
                        && current_generation == Some(completion.dirty_generation)
                    {
                        self.dirty_columns.remove(&key);
                        self.dirty_column_generation.remove(&key);
                    } else if self.dirty_columns.contains(&key) {
                        self.requeue_dirty_column(completion.cx, completion.cz);
                    }
                }
                Err(err) => {
                    self.frame_perf.storage_save_results_failed += 1;
                    self.saved_revision.clear();
                    eprintln!(
                        "[storage][save] async save failed for column ({},{}): {}",
                        completion.cx, completion.cz, err
                    );
                    if self.dirty_columns.contains(&key) {
                        self.requeue_dirty_column(completion.cx, completion.cz);
                    }
                }
            }
        }
        self.frame_perf.storage_save_results_drained += drained;
        self.frame_perf.storage_save_results_backlog_after = self
            .frame_perf
            .storage_save_results_backlog_after
            .max(self.storage_writer.save_result_queue_len_approx());
        let dur_ms = started.elapsed().as_secs_f32() * 1000.0;
        self.frame_perf.pump_storage_save_results_ms += dur_ms;
        perf_trace::record_duration_ms(
            "state.pump_storage_save_results",
            dur_ms,
            &format!(
                "drained={} ok={} fail={} q_before={} q_after={}",
                drained,
                self.frame_perf.storage_save_results_ok,
                self.frame_perf.storage_save_results_failed,
                backlog_before,
                self.storage_writer.save_result_queue_len_approx()
            ),
        );
    }

    fn autosave_if_due(&mut self) {
        let _trace_scope = perf_trace::scope("state.autosave_if_due");
        let started = Instant::now();
        self.pump_storage_save_results();

        if !self.autosave_sweep_active && self.last_autosave_at.elapsed() >= self.autosave_interval
        {
            self.autosave_sweep_active = true;
        }
        if !self.autosave_sweep_active {
            return;
        }

        let dirty_based_budget = (self.dirty_columns.len()
            / AUTOSAVE_DYNAMIC_DIRTY_COLUMNS_PER_STEP)
            .clamp(1, AUTOSAVE_DYNAMIC_MAX_COLUMNS_PER_FRAME);
        let unload_pressure_bonus = (self.pending_unload_columns.len() / 16).clamp(0, 4);
        let mut autosave_budget = dirty_based_budget.max(AUTOSAVE_COLUMNS_PER_FRAME);
        if !self.pending_unload_columns.is_empty() {
            autosave_budget = (autosave_budget + unload_pressure_bonus + 1)
                .clamp(1, AUTOSAVE_DYNAMIC_MAX_COLUMNS_PER_FRAME);
        }

        let queued_now = self.queue_dirty_columns_with_budget(autosave_budget);
        self.frame_perf.autosave_columns_queued += queued_now;
        if queued_now > 0 {
            eprintln!(
                "[storage][autosave] queued_this_frame={} budget={} dirty_columns={} pending_unload_cols={} inflight={} writer_queue_approx={}",
                queued_now,
                autosave_budget,
                self.dirty_columns.len(),
                self.pending_unload_columns.len(),
                self.save_columns_inflight.len(),
                self.storage_writer.queue_len_approx()
            );
        }

        if self.dirty_columns.is_empty() && self.save_columns_inflight.is_empty() {
            self.autosave_sweep_active = false;
            self.last_autosave_at = Instant::now();
        }
        let dur_ms = started.elapsed().as_secs_f32() * 1000.0;
        self.frame_perf.autosave_total_ms += dur_ms;
        perf_trace::record_duration_ms(
            "state.autosave_if_due",
            dur_ms,
            &format!(
                "queued_now={} budget={} dirty={} pending_unload_cols={} inflight={}",
                queued_now,
                autosave_budget,
                self.dirty_columns.len(),
                self.pending_unload_columns.len(),
                self.save_columns_inflight.len()
            ),
        );
    }

    fn saved_column_miss_cache_fresh(&self, cx: i32, cz: i32) -> bool {
        self.saved_column_missing_cache
            .get(&(cx, cz))
            .is_some_and(|t| t.elapsed() < SAVED_LOAD_MISS_CACHE_TTL)
    }

    fn prune_expired_saved_column_miss_cache_entry(&mut self, cx: i32, cz: i32) {
        if self
            .saved_column_missing_cache
            .get(&(cx, cz))
            .is_some_and(|t| t.elapsed() >= SAVED_LOAD_MISS_CACHE_TTL)
        {
            self.saved_column_missing_cache.remove(&(cx, cz));
        }
    }

    fn can_request_saved_column_load(&mut self, cx: i32, cz: i32) -> bool {
        if self.saved_column_loads_inflight.contains(&(cx, cz)) {
            return false;
        }
        self.prune_expired_saved_column_miss_cache_entry(cx, cz);
        if self.saved_column_miss_cache_fresh(cx, cz) {
            return false;
        }
        for cy in self.world.min_chunk_y()..=self.world.max_chunk_y() {
            let coord = ChunkCoord {
                x: cx,
                y: cy,
                z: cz,
            };
            if self.world.get_chunk(coord).is_some()
                || self.generated_empty_chunks.contains(&coord)
                || self.chunkgen_inflight.contains(&coord)
            {
                return false;
            }
        }
        true
    }

    fn note_saved_column_request_queued(&mut self) {
        self.saved_load_requests_in_window += 1;
    }

    fn report_saved_column_request_rate_if_due(&mut self) {
        let elapsed = self.saved_load_requests_window_started.elapsed();
        if elapsed >= Duration::from_secs(1) {
            let rate = self.saved_load_requests_in_window as f32 / elapsed.as_secs_f32().max(0.001);
            eprintln!(
                "[stream][saved_load] req_per_sec={:.1} inflight={} miss_cache={} q={} backlog={}",
                rate,
                self.saved_column_loads_inflight.len(),
                self.saved_column_missing_cache.len(),
                self.storage_loader.queue_len_approx(),
                self.storage_loader.result_queue_len_approx()
            );
            self.saved_load_requests_in_window = 0;
            self.saved_load_requests_window_started = Instant::now();
        }
    }

    fn try_queue_saved_column_load_request(&mut self, cx: i32, cz: i32) -> bool {
        if !self.can_request_saved_column_load(cx, cz) {
            return false;
        }
        match self.storage_loader.enqueue_load_column(cx, cz) {
            Ok(()) => {
                self.saved_column_loads_inflight.insert((cx, cz));
                self.frame_perf.saved_column_load_attempts += 1;
                self.note_saved_column_request_queued();
                true
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => false,
            Err(err) => {
                eprintln!("[storage][load] failed to queue column ({cx},{cz}): {err}");
                false
            }
        }
    }

    fn queue_saved_column_load_requests_for_radius(
        &mut self,
        center_chunk_x: i32,
        center_chunk_z: i32,
        budget: usize,
    ) -> usize {
        if budget == 0 || self.radius_offsets_cache.is_empty() {
            return 0;
        }

        let build_started = Instant::now();
        let mut queued = 0usize;
        let len = self.radius_offsets_cache.len();
        let mut scanned = 0usize;
        let scan_budget = SAVED_LOAD_CANDIDATE_SCAN_BUDGET_PER_FRAME.max(budget);

        while queued < budget && scanned < scan_budget && scanned < len {
            let idx = self.saved_load_request_cursor % len;
            self.saved_load_request_cursor = (self.saved_load_request_cursor + 1) % len;
            scanned += 1;
            let (dx, dz) = self.radius_offsets_cache[idx];
            let cx = center_chunk_x + dx;
            let cz = center_chunk_z + dz;
            if self.try_queue_saved_column_load_request(cx, cz) {
                queued += 1;
            }
        }

        let build_ms = build_started.elapsed().as_secs_f32() * 1000.0;
        self.frame_perf.ensure_saved_column_load_ms += build_ms;
        perf_trace::record_duration_ms(
            "saved_load.build_candidates",
            build_ms,
            &format!(
                "queued={} scanned={} budget={} cursor={} inflight={} miss_cache={}",
                queued,
                scanned,
                budget,
                self.saved_load_request_cursor,
                self.saved_column_loads_inflight.len(),
                self.saved_column_missing_cache.len()
            ),
        );
        queued
    }

    fn apply_loaded_saved_column_blob(&mut self, cx: i32, cz: i32, column: ColumnBlob) -> bool {
        let total_started = Instant::now();
        let apply_column_started = Instant::now();
        let min_section_y = column.min_section_y;
        let max_section_y = column.max_section_y;
        let expected_sections = column
            .sections
            .iter()
            .map(|s| s.cy)
            .collect::<HashSet<_>>();
        if let Err(err) = apply_column(&mut self.world, column) {
            eprintln!("saved-load apply failed for column ({cx},{cz}): {err}");
            return false;
        }
        let present_sections: Vec<ChunkCoord> = (min_section_y..=max_section_y)
            .filter(|cy| expected_sections.contains(cy))
            .filter_map(|cy| {
                let coord = ChunkCoord { x: cx, y: cy, z: cz };
                if self.world.get_chunk(coord).is_some() {
                    Some(coord)
                } else {
                    None
                }
            })
            .collect();
        for cy in min_section_y..=max_section_y {
            self.full_solid_mesh_skip_set.remove(&ChunkCoord { x: cx, y: cy, z: cz });
        }
        let apply_column_ms = apply_column_started.elapsed().as_secs_f32() * 1000.0;
        self.saved_column_missing_cache.remove(&(cx, cz));

        let marker_started = Instant::now();
        let mut marker_count = 0usize;
        for cy in min_section_y..=max_section_y {
            marker_count += 1;
            self.sync_generated_empty_chunk_marker(ChunkCoord {
                x: cx,
                y: cy,
                z: cz,
            });
        }
        let sync_empty_markers_ms = marker_started.elapsed().as_secs_f32() * 1000.0;

        let mut loaded_any_chunk = false;
        let lighting_on_chunk_added_total_ms = 0.0f32;
        let mut enqueue_generated_fluids_total_ms = 0.0f32;
        let mut mark_dirty_total_ms = 0.0f32;
        let lighting_calls = 0usize;
        let lighting_affected_total = 0usize;
        let mut fluid_enqueue_calls = 0usize;
        let mut mark_chunk_dirty_calls = 0usize;
        let section_count = present_sections.len();
        for coord in present_sections {
            loaded_any_chunk = true;
            // Saved-load apply must stay cheap: skip per-section incremental lighting hooks here.
            // The async lighting rebuild (requested once per batch in pump_storage_load_results)
            // will reconcile lighting for all loaded chunks coherently.

            let fluids_started = Instant::now();
            self.enqueue_generated_fluids_for_chunk(coord);
            enqueue_generated_fluids_total_ms += fluids_started.elapsed().as_secs_f32() * 1000.0;
            fluid_enqueue_calls += 1;

            let mark_started = Instant::now();
            mark_chunk_dirty_calls += 1;
            self.mark_chunk_dirty(coord);
            for neighbor in Self::neighbor_chunk_coords(coord) {
                mark_chunk_dirty_calls += 1;
                self.mark_chunk_dirty(neighbor);
            }
            mark_dirty_total_ms += mark_started.elapsed().as_secs_f32() * 1000.0;
        }
        self.mark_column_saved_revisions(cx, cz);
        self.clear_dirty_column_tracking(cx, cz);
        let total_ms = total_started.elapsed().as_secs_f32() * 1000.0;

        let coord_counts = format!("cx={} cz={}", cx, cz);
        perf_trace::record_duration_ms(
            "apply_saved.apply_column",
            apply_column_ms,
            &format!("{coord_counts} sections={section_count}"),
        );
        perf_trace::record_duration_ms(
            "apply_saved.sync_empty_markers",
            sync_empty_markers_ms,
            &format!("{coord_counts} markers={marker_count}"),
        );
        perf_trace::record_duration_ms(
            "apply_saved.lighting_on_chunk_added_total",
            lighting_on_chunk_added_total_ms,
            &format!(
                "{coord_counts} calls={} affected_total={} deferred_async_rebuild=true",
                lighting_calls, lighting_affected_total
            ),
        );
        perf_trace::record_duration_ms(
            "apply_saved.enqueue_generated_fluids_total",
            enqueue_generated_fluids_total_ms,
            &format!("{coord_counts} calls={}", fluid_enqueue_calls),
        );
        perf_trace::record_duration_ms(
            "apply_saved.mark_dirty_total",
            mark_dirty_total_ms,
            &format!(
                "{coord_counts} mark_chunk_dirty_calls={} lighting_affected_total={} section_count={}",
                mark_chunk_dirty_calls, lighting_affected_total, section_count
            ),
        );
        perf_trace::record_duration_ms(
            "apply_saved.column_total",
            total_ms,
            &format!(
                "{coord_counts} sections={} markers={} lighting_calls={} lighting_affected_total={} fluid_calls={} mark_chunk_dirty_calls={} loaded_any={}",
                section_count,
                marker_count,
                lighting_calls,
                lighting_affected_total,
                fluid_enqueue_calls,
                mark_chunk_dirty_calls,
                loaded_any_chunk
            ),
        );
        if total_ms >= 8.0 {
            eprintln!(
                "[perf][saved_apply] col=({}, {}) total_ms={:.2} apply_column={:.2} markers={:.2} lighting_hooks={:.2} fluids={:.2} mark_dirty={:.2} sections={} markers={} lighting_calls={} affected={} fluid_calls={} mark_calls={}",
                cx,
                cz,
                total_ms,
                apply_column_ms,
                sync_empty_markers_ms,
                lighting_on_chunk_added_total_ms,
                enqueue_generated_fluids_total_ms,
                mark_dirty_total_ms,
                section_count,
                marker_count,
                lighting_calls,
                lighting_affected_total,
                fluid_enqueue_calls,
                mark_chunk_dirty_calls,
            );
        }
        loaded_any_chunk
    }

    fn pump_storage_load_results(&mut self) {
        let _trace_scope = perf_trace::scope("state.pump_storage_load_results");
        let started = Instant::now();
        let backlog_before = self.storage_loader.result_queue_len_approx();

        let mut drained = 0usize;
        let mut applied = 0usize;
        let mut misses = 0usize;
        let mut failed = 0usize;
        let mut discarded = 0usize;
        let discard_hits = self.world_generation_task.is_some();
        let apply_started = Instant::now();
        let mut budget_hit = false;
        let mut lighting_rebuild_needed = false;
        while drained < SAVED_LOAD_RESULTS_APPLY_PER_FRAME {
            if drained > 0
                && apply_started.elapsed().as_secs_f32() * 1000.0 >= SAVED_LOAD_APPLY_BUDGET_MS
            {
                budget_hit = true;
                break;
            }
            let Some(StorageColumnLoadResult { cx, cz, status }) =
                self.storage_loader.try_recv_result()
            else {
                break;
            };
            drained += 1;
            self.saved_column_loads_inflight.remove(&(cx, cz));
            match status {
                StorageColumnLoadStatus::Hit(blob) => {
                    if discard_hits {
                        discarded += 1;
                        continue;
                    }
                    self.frame_perf.saved_column_load_hits += 1;
                    if self.apply_loaded_saved_column_blob(cx, cz, blob) {
                        lighting_rebuild_needed = true;
                    }
                    applied += 1;
                }
                StorageColumnLoadStatus::Miss => {
                    self.frame_perf.saved_column_load_misses += 1;
                    self.saved_column_missing_cache
                        .insert((cx, cz), Instant::now());
                    misses += 1;
                }
                StorageColumnLoadStatus::Error(err) => {
                    self.saved_column_missing_cache.remove(&(cx, cz));
                    failed += 1;
                    eprintln!("[storage][load] async load failed for column ({cx},{cz}): {err}");
                }
            }
        }
        if lighting_rebuild_needed {
            self.request_lighting_rebuild();
        }
        let apply_ms = apply_started.elapsed().as_secs_f32() * 1000.0;
        if drained > 0 {
            perf_trace::record_duration_ms(
                "saved_load.apply_to_world",
                apply_ms,
                &format!(
                    "drained={} applied={} miss={} fail={} discarded={} lighting_rebuild={} budget_hit={} budget_ms={:.1} q_before={} q_after={}",
                    drained,
                    applied,
                    misses,
                    failed,
                    discarded,
                    lighting_rebuild_needed,
                    budget_hit,
                    SAVED_LOAD_APPLY_BUDGET_MS,
                    backlog_before,
                    self.storage_loader.result_queue_len_approx()
                ),
            );
        }
        self.frame_perf.ensure_saved_column_load_ms += started.elapsed().as_secs_f32() * 1000.0;
    }

    fn try_load_saved_column_if_present(&mut self, cx: i32, cz: i32) -> bool {
        let load_started = Instant::now();
        self.frame_perf.saved_column_load_attempts += 1;
        self.prune_expired_saved_column_miss_cache_entry(cx, cz);
        if self.saved_column_miss_cache_fresh(cx, cz) {
            self.frame_perf.saved_column_load_misses += 1;
            self.frame_perf.ensure_saved_column_load_ms +=
                load_started.elapsed().as_secs_f32() * 1000.0;
            return false;
        }
        for cy in self.world.min_chunk_y()..=self.world.max_chunk_y() {
            let coord = ChunkCoord {
                x: cx,
                y: cy,
                z: cz,
            };
            if self.world.get_chunk(coord).is_some()
                || self.generated_empty_chunks.contains(&coord)
                || self.chunkgen_inflight.contains(&coord)
            {
                self.frame_perf.saved_column_load_misses += 1;
                self.frame_perf.ensure_saved_column_load_ms +=
                    load_started.elapsed().as_secs_f32() * 1000.0;
                return false;
            }
        }

        match self.storage_reader.load_column(cx, cz) {
            Ok(None) => {
                self.saved_column_missing_cache
                    .insert((cx, cz), Instant::now());
                self.frame_perf.saved_column_load_misses += 1;
                self.frame_perf.ensure_saved_column_load_ms +=
                    load_started.elapsed().as_secs_f32() * 1000.0;
                false
            }
            Ok(Some(bytes)) => {
                let blob = decode_column(&bytes).unwrap_or_else(|err| {
                    panic!(
                        "failed to decode saved column ({}, {}) from region storage: {}",
                        cx, cz, err
                    )
                });
                let present_sections: Vec<ChunkCoord> = blob
                    .sections
                    .iter()
                    .map(|s| ChunkCoord {
                        x: blob.cx,
                        y: s.cy,
                        z: blob.cz,
                    })
                    .collect();
                let min_section_y = blob.min_section_y;
                let max_section_y = blob.max_section_y;
                apply_column(&mut self.world, blob).unwrap_or_else(|err| {
                    panic!(
                        "failed to apply saved column ({}, {}) into world: {}",
                        cx, cz, err
                    )
                });
                self.saved_column_missing_cache.remove(&(cx, cz));

                for cy in min_section_y..=max_section_y {
                    self.sync_generated_empty_chunk_marker(ChunkCoord {
                        x: cx,
                        y: cy,
                        z: cz,
                    });
                }

                let mut loaded_any_chunk = false;
                for coord in present_sections {
                    loaded_any_chunk = true;
                    self.enqueue_generated_fluids_for_chunk(coord);
                    self.mark_chunk_dirty(coord);
                    for neighbor in Self::neighbor_chunk_coords(coord) {
                        self.mark_chunk_dirty(neighbor);
                    }
                }
                if loaded_any_chunk {
                    self.request_lighting_rebuild();
                }
                self.mark_column_saved_revisions(cx, cz);
                self.clear_dirty_column_tracking(cx, cz);
                self.frame_perf.saved_column_load_hits += 1;
                self.frame_perf.ensure_saved_column_load_ms +=
                    load_started.elapsed().as_secs_f32() * 1000.0;

                true
            }
            Err(err) => {
                panic!(
                    "failed to load saved column ({}, {}) from region storage: {}",
                    cx, cz, err
                );
            }
        }
    }

    pub(crate) fn create_world_pipeline(
        device: &wgpu::Device,
        config: &wgpu::SurfaceConfiguration,
        world_shader: &wgpu::ShaderModule,
        world_pipeline_layout: &wgpu::PipelineLayout,
        depth_format: wgpu::TextureFormat,
        cull_mode: Option<wgpu::Face>,
        blend: Option<wgpu::BlendState>,
        depth_compare: wgpu::CompareFunction,
        depth_write_enabled: bool,
    ) -> wgpu::RenderPipeline {
        let started = Instant::now();
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
                    blend,
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
                depth_write_enabled,
                depth_compare,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        perf_trace::record_duration(
            "wgpu.create_render_pipeline",
            started.elapsed(),
            "label=Cube Pipeline",
        );
        pipeline
    }

    fn create_shader_module_traced(
        device: &wgpu::Device,
        label: &'static str,
        source: &'static str,
    ) -> wgpu::ShaderModule {
        let started = Instant::now();
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        perf_trace::record_duration(
            "wgpu.create_shader_module",
            started.elapsed(),
            &format!("label={label}"),
        );
        module
    }

    fn create_render_pipeline_traced(
        device: &wgpu::Device,
        descriptor: &wgpu::RenderPipelineDescriptor<'_>,
    ) -> wgpu::RenderPipeline {
        let label = descriptor.label.unwrap_or("unnamed");
        let started = Instant::now();
        let pipeline = device.create_render_pipeline(descriptor);
        perf_trace::record_duration(
            "wgpu.create_render_pipeline",
            started.elapsed(),
            &format!("label={label}"),
        );
        pipeline
    }

    pub(crate) fn world_pipeline(&self) -> &wgpu::RenderPipeline {
        match (self.debug_backface_culling, self.debug_depth_test) {
            (true, true) => &self.render_pipeline_back_depth,
            (false, true) => &self.render_pipeline_no_cull_depth,
            (true, false) => &self.render_pipeline_back_no_depth,
            (false, false) => &self.render_pipeline_no_cull_no_depth,
        }
    }

    pub(crate) fn toggle_debug_culling(&mut self) {
        self.debug_backface_culling = !self.debug_backface_culling;
        println!(
            "debug render: backface_culling={}, depth_test={}",
            self.debug_backface_culling, self.debug_depth_test
        );
    }

    pub(crate) fn toggle_debug_depth_test(&mut self) {
        self.debug_depth_test = !self.debug_depth_test;
        println!(
            "debug render: backface_culling={}, depth_test={}",
            self.debug_backface_culling, self.debug_depth_test
        );
    }

    fn queue_debug_remesh_for_loaded_chunks(&mut self) {
        let coords: Vec<ChunkCoord> = self.world.chunks.keys().copied().collect();
        for coord in coords {
            self.mark_chunk_dirty(coord);
        }
    }

    pub(crate) fn toggle_debug_disable_ao(&mut self) {
        self.debug_disable_ao = !self.debug_disable_ao;
        crate::meshing::greedy::set_debug_disable_ao(self.debug_disable_ao);
        self.mesh_debug_epoch = self.mesh_debug_epoch.wrapping_add(1);
        self.queue_debug_remesh_for_loaded_chunks();
        println!(
            "debug lighting: ao_disabled={} (remeshing loaded chunks, mesh_debug_epoch={})",
            self.debug_disable_ao, self.mesh_debug_epoch
        );
    }

    pub(crate) fn cycle_debug_lighting_view(&mut self) {
        self.debug_lighting_view = self.debug_lighting_view.next();
        println!(
            "debug lighting: view={} (normal/fullbright/ao_only/skylight_heatmap/blocklight_heatmap)",
            self.debug_lighting_view.label()
        );
    }

    pub(crate) fn begin_frame_profiling(&mut self, frame_dt_ms: f32) {
        self.frame_perf.begin_frame(frame_dt_ms);
    }

    pub(crate) fn toggle_far_lod_enabled(&mut self) {
        self.far_lod_enabled = !self.far_lod_enabled;
        if !self.far_lod_enabled {
            self.pending_lod_mesh_uploads.clear();
            self.lod_chunkgen_inflight.clear();
        } else {
            self.last_lod_stream_center = None;
            self.lod_request_cursor = 0;
        }
        println!(
            "far LOD: enabled={} requests_per_frame={} pending_uploads={}",
            self.far_lod_enabled,
            self.far_lod_requests_per_frame,
            self.pending_lod_mesh_uploads.len()
        );
    }

    pub(crate) fn far_lod_enabled(&self) -> bool {
        self.far_lod_enabled
    }

    pub(crate) fn should_report_stall(&self, frame_dt_ms: f32) -> bool {
        frame_dt_ms > STALL_REPORT_THRESHOLD_MS
    }

    pub(crate) fn format_stall_report(&self, frame_dt_ms: f32) -> String {
        let mut scopes = [
            ("chunkgen", self.frame_perf.pump_chunkgen_results_ms),
            ("mesh", self.frame_perf.pump_mesh_results_ms),
            ("autosave", self.frame_perf.autosave_total_ms),
            ("storage", self.frame_perf.pump_storage_save_results_ms),
            ("lighting", self.frame_perf.rebuild_world_lighting_ms),
            ("stream", self.frame_perf.enqueue_chunks_within_view_ms),
            ("saved_load", self.frame_perf.ensure_saved_column_load_ms),
        ];
        scopes.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let top_scopes = scopes
            .iter()
            .take(3)
            .filter(|(_, ms)| *ms > 0.05)
            .map(|(name, ms)| format!("{name}={ms:.1}ms"))
            .collect::<Vec<_>>()
            .join(",");
        let mut proc_scopes = [
            (
                "proc.pump_chunkgen_results",
                self.frame_perf.proc_pump_chunkgen_results_ms,
            ),
            (
                "proc.pump_storage_load_results",
                self.frame_perf.proc_pump_storage_load_results_ms,
            ),
            (
                "proc.unload_chunks_outside_keep_radius",
                self.frame_perf.proc_unload_chunks_outside_keep_radius_ms,
            ),
            (
                "proc.prune_chunk_meshes_outside_view",
                self.frame_perf.proc_prune_chunk_meshes_outside_view_ms,
            ),
            (
                "proc.enqueue_chunks_within_view",
                self.frame_perf.proc_enqueue_chunks_within_view_ms,
            ),
            (
                "proc.rebuild_world_lighting_if_dirty",
                self.frame_perf.proc_rebuild_world_lighting_if_dirty_ms,
            ),
            (
                "proc.dispatch_mesh_jobs_loop_total",
                self.frame_perf.proc_dispatch_mesh_jobs_loop_total_ms,
            ),
            (
                "proc.autosave_if_due",
                self.frame_perf.proc_autosave_if_due_ms,
            ),
            (
                "proc.dispatch.frustum_build",
                self.frame_perf.proc_dispatch_frustum_build_ms,
            ),
            (
                "proc.dispatch.pop_dirty",
                self.frame_perf.proc_dispatch_pop_dirty_ms,
            ),
            (
                "proc.dispatch.snapshot_neighborhood",
                self.frame_perf.proc_dispatch_snapshot_neighborhood_ms,
            ),
            (
                "proc.dispatch.send_mesh_job",
                self.frame_perf.proc_dispatch_send_mesh_job_ms,
            ),
            (
                "proc.dispatch.requeue_dirty",
                self.frame_perf.proc_dispatch_requeue_dirty_ms,
            ),
        ];
        proc_scopes.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let top_proc_scopes = proc_scopes
            .iter()
            .take(6)
            .filter(|(_, ms)| *ms > 0.05)
            .map(|(name, ms)| format!("{name}={ms:.1}ms"))
            .collect::<Vec<_>>()
            .join(",");

        format!(
            "[perf][stall] frame={:.1}ms tops=[{}] process_dirty_chunk_meshes(ms={:.1},tops=[{}]) chunkgen(ms={:.1},drain={},q={}=>{},applied={},lod_results={},lod_req={},lod_up={},lod_up_q={},lod_gpu_ms={:.1}) mesh(ms={:.1},drain={},q={}=>{},up_q={}=>{},uploads={},gpu_up_ms={:.1}) storage(ms={:.1},drain={},q={}=>{},ok={},fail={},autosave_q={}) lighting(ms={:.1},drain={},q={}=>{},changed={}) stream(ms={:.1},far_lod={},lod_center={:?},lod_center_changes={},lod_cpu_skip={},lod_backlog_skip={}) saved_load(ms={:.1},attempts={},hits={},misses={}) view={} inflight(mesh={},chunkgen={},lod={}) backlogs(mesh_rx={},chunkgen_rx={},light_rx={},save_rx={},save_ack_rx={},mesh_up_q={},lod_up_q={})",
            frame_dt_ms,
            top_scopes,
            self.frame_perf.process_dirty_chunk_meshes_ms,
            top_proc_scopes,
            self.frame_perf.pump_chunkgen_results_ms,
            self.frame_perf.chunkgen_results_drained,
            self.frame_perf.chunkgen_results_backlog_before,
            self.frame_perf.chunkgen_results_backlog_after,
            self.frame_perf.chunkgen_chunks_applied,
            self.frame_perf.chunkgen_lod_results_applied,
            self.frame_perf.far_lod_requests_queued,
            self.frame_perf.chunkgen_lod_mesh_uploads,
            self.frame_perf.chunkgen_lod_upload_backlog_after,
            self.frame_perf.gpu_far_lod_upload_ms,
            self.frame_perf.pump_mesh_results_ms,
            self.frame_perf.mesh_results_drained,
            self.frame_perf.mesh_results_backlog_before,
            self.frame_perf.mesh_results_backlog_after,
            self.frame_perf.mesh_upload_queue_before,
            self.frame_perf.mesh_upload_queue_after,
            self.frame_perf.mesh_uploads_processed,
            self.frame_perf.gpu_chunk_mesh_upload_ms,
            self.frame_perf.pump_storage_save_results_ms + self.frame_perf.autosave_total_ms,
            self.frame_perf.storage_save_results_drained,
            self.frame_perf.storage_save_results_backlog_before,
            self.frame_perf.storage_save_results_backlog_after,
            self.frame_perf.storage_save_results_ok,
            self.frame_perf.storage_save_results_failed,
            self.frame_perf.autosave_columns_queued,
            self.frame_perf.rebuild_world_lighting_ms,
            self.frame_perf.lighting_results_drained,
            self.frame_perf.lighting_results_backlog_before,
            self.frame_perf.lighting_results_backlog_after,
            self.frame_perf.lighting_chunks_changed,
            self.frame_perf.enqueue_chunks_within_view_ms,
            self.far_lod_enabled,
            self.last_lod_stream_center,
            self.frame_perf.far_lod_center_changes,
            self.frame_perf.far_lod_requests_suppressed_cpu_budget,
            self.frame_perf.far_lod_requests_suppressed_backlog,
            self.frame_perf.ensure_saved_column_load_ms,
            self.frame_perf.saved_column_load_attempts,
            self.frame_perf.saved_column_load_hits,
            self.frame_perf.saved_column_load_misses,
            self.view_distance_chunks,
            self.inflight.len(),
            self.chunkgen_inflight.len(),
            self.lod_chunkgen_inflight.len(),
            self.mesh_results.len(),
            self.chunkgen_results.len(),
            self.lighting_results.len(),
            self.storage_writer.queue_len_approx(),
            self.storage_writer.save_result_queue_len_approx(),
            self.mesh_upload_queue.len(),
            self.pending_lod_mesh_uploads.len(),
        )
    }

    pub(crate) async fn new(window: Arc<Window>) -> Self {
        let _trace_scope = perf_trace::scope("state.new");
        let startup_total_start = Instant::now();
        let size = window.inner_size();

        let instance = wgpu::Instance::default();

        let surface = instance
            .create_surface(window.clone())
            .expect("Failed to create wgpu surface from window");

        let adapter_req_started = Instant::now();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("No suitable GPU adapters found");
        perf_trace::record_duration("wgpu.request_adapter", adapter_req_started.elapsed(), "");

        let device_req_started = Instant::now();
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
        perf_trace::record_duration("wgpu.request_device", device_req_started.elapsed(), "");
        perf_trace::instant_event(
            "wgpu.features",
            &format!(
                "adapter_timestamp_query={} device_timestamp_query={}",
                adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY),
                device.features().contains(wgpu::Features::TIMESTAMP_QUERY)
            ),
        );
        #[allow(deprecated)]
        device.on_uncaptured_error(Arc::new(|err: wgpu::Error| {
            eprintln!("[wgpu][error] uncaptured: {err}");
            perf_trace::instant_event("wgpu.uncaptured_error", &err.to_string());
        }));

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
        let preview_camera_uniform = CameraUniform::new();
        let preview_camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Preview Camera Buffer"),
            contents: bytemuck::bytes_of(&preview_camera_uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let preview_camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Preview Camera BindGroup"),
            layout: &camera_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: preview_camera_buffer.as_entire_binding(),
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
            block_registry.texture_key_for(BlockId::Bedrock, Face::Down),
            TEX_BEDROCK
        );
        debug_assert_eq!(
            block_registry.model_for(BlockId::Sapling),
            BlockModel::CrossPlant
        );
        debug_assert_eq!(block_registry.model_for(BlockId::Bedrock), BlockModel::Cube);
        debug_assert!(!block_registry.solid_for(BlockId::Sapling));
        debug_assert!(block_registry.solid_for(BlockId::OakPlanks));
        debug_assert!(!block_registry.breakable_for(BlockId::Bedrock));
        println!(
            "block texture self-check ok: grass(top={}, side={}, bottom={}), stone(all={}), oak_planks(all={}), sapling(cross={}), bedrock(all={})",
            TEX_GRASS_TOP,
            TEX_GRASS_SIDE,
            TEX_DIRT,
            TEX_STONE,
            TEX_OAK_PLANKS,
            TEX_SAPLING,
            TEX_BEDROCK
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

        let mut all_texture_keys = block_texture_keys;
        for required_overlay in REQUIRED_OVERLAY_TEXTURE_KEYS {
            if !all_texture_keys.iter().any(|k| k == required_overlay) {
                all_texture_keys.push(required_overlay.to_string());
            }
        }
        all_texture_keys.sort();
        all_texture_keys.dedup();

        struct LoadedTexture {
            key: String,
            frame_size: u32,
            frames_rgba: Vec<Vec<u8>>,
            anim_meta: AnimMeta,
        }

        let mut loaded_textures: Vec<LoadedTexture> = Vec::with_capacity(256);
        let mut max_frame_size = 16_u32;

        let texture_load_start = std::time::Instant::now();
        for key in &all_texture_keys {
            let handle = texture_registry.get_or_load(key.as_str()).unwrap_or_else(|err| {
                panic!(
                    "[error] failed to load block texture key '{}' referenced by BlockRegistry: {}",
                    key, err
                )
            });
            let image = texture_registry
                .get(&handle)
                .unwrap_or_else(|| panic!("missing cached texture image for key '{}'", key));
            let (sheet_frames, frames_rgba, frame_size) =
                decode_texture_frames(image.width, image.height, &image.rgba);
            max_frame_size = max_frame_size.max(frame_size);
            let anim_meta = parse_anim_meta_for_texture(key, sheet_frames);
            debug_assert_eq!(anim_meta.axis, AnimAxis::Vertical);
            loaded_textures.push(LoadedTexture {
                key: key.clone(),
                frame_size,
                frames_rgba,
                anim_meta,
            });
        }
        let texture_load_duration = texture_load_start.elapsed();
        println!(
            "[perf][startup] texture decode/load took: {:?} for {} textures",
            texture_load_duration,
            loaded_textures.len()
        );
        perf_trace::record_duration(
            "startup.texture_decode_and_load",
            texture_load_duration,
            &format!("textures={}", loaded_textures.len()),
        );

        let layer_width = max_frame_size;
        let layer_height = max_frame_size;
        let missing_rgba = make_missing_texture_rgba(layer_width, layer_height);
        let mut key_to_layer: HashMap<String, u32> = HashMap::new();
        let mut texture_layers_rgba: Vec<Vec<u8>> = Vec::with_capacity(512);
        let mut texture_anim_gpu: Vec<AnimGpu> = Vec::with_capacity(64);
        let mut texture_anim_tracks: Vec<TextureAnimTrack> = Vec::with_capacity(64);

        let atlas_build_start = std::time::Instant::now();
        // Layer 0 ist absichtlich "missing texture", damit fehlende/kaputte Keys niemals still auf Stone fallen.
        let missing_layer = texture_layers_rgba.len() as u32;
        texture_layers_rgba.push(missing_rgba);
        texture_anim_gpu.push(AnimGpu {
            sheet_frames: 1,
            current_frame: 0,
            flags: 0,
            _pad: 0,
        });

        for loaded in &loaded_textures {
            let base_layer = texture_layers_rgba.len() as u32;
            key_to_layer.insert(loaded.key.clone(), base_layer);

            for frame in &loaded.frames_rgba {
                texture_layers_rgba.push(resize_rgba_nearest(
                    frame,
                    loaded.frame_size,
                    loaded.frame_size,
                    layer_width,
                    layer_height,
                ));
                texture_anim_gpu.push(AnimGpu {
                    sheet_frames: 1,
                    current_frame: 0,
                    flags: 0,
                    _pad: 0,
                });
            }

            texture_anim_gpu[base_layer as usize] = initial_anim_gpu_for_meta(&loaded.anim_meta);
            if loaded.anim_meta.sheet_frames > 1 {
                texture_anim_tracks.push(TextureAnimTrack {
                    texture_id: base_layer,
                    meta: loaded.anim_meta.clone(),
                    state: AnimState::default(),
                });
            }
        }
        let atlas_build_duration = atlas_build_start.elapsed();
        println!(
            "[perf][startup] texture atlas build took: {:?} for {} layers",
            atlas_build_duration,
            texture_layers_rgba.len()
        );
        perf_trace::record_duration(
            "startup.texture_atlas_build",
            atlas_build_duration,
            &format!("layers={}", texture_layers_rgba.len()),
        );

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

        for required_overlay in REQUIRED_OVERLAY_TEXTURE_KEYS {
            assert!(
                key_to_layer.contains_key(required_overlay),
                "required overlay texture key '{}' missing",
                required_overlay
            );
        }
        let water_overlay_layer = key_to_layer
            .get(TEX_WATER_OVERLAY)
            .copied()
            .unwrap_or(missing_layer);

        let texture_anim_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Texture Animation Meta Buffer"),
            contents: bytemuck::cast_slice(&texture_anim_gpu),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });

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
            &texture_anim_buffer,
            "World Texture BindGroup",
        );

        let sun_path = format!("{ASSET_ROOT}/textures/environment/sun.png");
        let (sun_w, sun_h, sun_rgba) = load_png_rgba(&sun_path)
            .unwrap_or_else(|err| panic!("failed to load sky sun texture: {}", err));
        let moon_path = format!("{ASSET_ROOT}/textures/environment/moon_phases.png");
        let (moon_w, moon_h, moon_rgba) = load_png_rgba(&moon_path)
            .unwrap_or_else(|err| panic!("failed to load sky moon texture: {}", err));
        let sky_tex_w = sun_w.max(moon_w).max(16);
        let sky_tex_h = sun_h.max(moon_h).max(16);
        let sky_sun_layer = 0_u32;
        let sky_moon_layer = 1_u32;
        let sun_off_x = (sky_tex_w - sun_w) / 2;
        let sun_off_y = (sky_tex_h - sun_h) / 2;
        let sky_sun_uv_rect = [
            sun_off_x as f32 / sky_tex_w as f32,
            sun_off_y as f32 / sky_tex_h as f32,
            (sun_off_x + sun_w) as f32 / sky_tex_w as f32,
            (sun_off_y + sun_h) as f32 / sky_tex_h as f32,
        ];
        let sky_layers = vec![
            pad_rgba_center(&sun_rgba, sun_w, sun_h, sky_tex_w, sky_tex_h),
            pad_rgba_center(&moon_rgba, moon_w, moon_h, sky_tex_w, sky_tex_h),
        ];
        let sky_anim_gpu = vec![
            AnimGpu {
                sheet_frames: 1,
                current_frame: 0,
                flags: 0,
                _pad: 0,
            };
            sky_layers.len()
        ];
        let sky_anim_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Sky Anim Buffer"),
            contents: bytemuck::cast_slice(&sky_anim_gpu),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let sky_texture_array = TextureArray::from_rgba8_layers(
            &device,
            &queue,
            &sky_layers,
            sky_tex_w,
            sky_tex_h,
            "Sky Texture Array",
        )
        .unwrap_or_else(|err| panic!("failed to build sky texture array: {}", err));
        let sky_texture_bind_group = sky_texture_array.create_bind_group(
            &device,
            &texture_bind_group_layout,
            &sky_anim_buffer,
            "Sky Texture BindGroup",
        );

        let dirt_path = format!("{ASSET_ROOT}/textures/block/dirt.png");
        let (dirt_w, dirt_h, dirt_rgba) = load_png_rgba(&dirt_path)
            .unwrap_or_else(|err| panic!("failed to load worldgen dirt texture: {}", err));
        let font_path = format!("{ASSET_ROOT}/textures/font/ascii.png");
        let (font_w, font_h, font_rgba) = load_png_rgba(&font_path)
            .unwrap_or_else(|err| panic!("failed to load worldgen font texture: {}", err));
        let worldgen_ui_size = dirt_w.max(dirt_h).max(font_w).max(font_h).max(16);
        let worldgen_dirt_layer = 0_u32;
        let worldgen_font_layer = 1_u32;
        let worldgen_ui_layers = vec![
            resize_rgba_nearest(
                &dirt_rgba,
                dirt_w,
                dirt_h,
                worldgen_ui_size,
                worldgen_ui_size,
            ),
            resize_rgba_nearest(
                &font_rgba,
                font_w,
                font_h,
                worldgen_ui_size,
                worldgen_ui_size,
            ),
        ];
        let worldgen_ui_anim_gpu = vec![
            AnimGpu {
                sheet_frames: 1,
                current_frame: 0,
                flags: 0,
                _pad: 0,
            };
            worldgen_ui_layers.len()
        ];
        let worldgen_ui_anim_buffer =
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Worldgen UI Anim Buffer"),
                contents: bytemuck::cast_slice(&worldgen_ui_anim_gpu),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let worldgen_ui_texture_array = TextureArray::from_rgba8_layers(
            &device,
            &queue,
            &worldgen_ui_layers,
            worldgen_ui_size,
            worldgen_ui_size,
            "Worldgen UI Texture Array",
        )
        .unwrap_or_else(|err| panic!("failed to build worldgen ui texture array: {}", err));
        let worldgen_ui_texture_bind_group = worldgen_ui_texture_array.create_bind_group(
            &device,
            &texture_bind_group_layout,
            &worldgen_ui_anim_buffer,
            "Worldgen UI Texture BindGroup",
        );
        let worldgen_font = build_bitmap_font(worldgen_font_layer, &font_rgba, font_w, font_h);

        let block_texture_lookup = Arc::new(BlockTextureLookup::new(
            &block_registry,
            &key_to_layer,
            missing_layer,
        ));
        debug_assert_eq!(
            block_texture_lookup.model_for(BlockId::Sapling),
            BlockModel::CrossPlant
        );
        debug_assert!(!block_texture_lookup.is_solid_for(BlockId::Sapling));
        debug_assert!(!block_texture_lookup.is_breakable_for(BlockId::Bedrock));

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
        let pipeline_creation_start = Instant::now();
        let world_shader =
            Self::create_shader_module_traced(&device, "World Shader", WORLD_SHADER_WGSL);
        let color_shader =
            Self::create_shader_module_traced(&device, "Color Shader", COLOR_SHADER_WGSL);
        let sky_shader = Self::create_shader_module_traced(&device, "Sky Shader", SKY_SHADER_WGSL);
        let outline_shader =
            Self::create_shader_module_traced(&device, "Outline Shader", OUTLINE_SHADER_WGSL);

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
        let sky_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Sky Pipeline Layout"),
            bind_group_layouts: &[&texture_bind_group_layout, &camera_bind_group_layout],
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
            Some(wgpu::BlendState::REPLACE),
            wgpu::CompareFunction::LessEqual,
            true,
        );
        let render_pipeline_no_cull_depth = Self::create_world_pipeline(
            &device,
            &config,
            &world_shader,
            &world_pipeline_layout,
            depth.format,
            None,
            Some(wgpu::BlendState::REPLACE),
            wgpu::CompareFunction::LessEqual,
            true,
        );
        let render_pipeline_back_no_depth = Self::create_world_pipeline(
            &device,
            &config,
            &world_shader,
            &world_pipeline_layout,
            depth.format,
            Some(wgpu::Face::Back),
            Some(wgpu::BlendState::REPLACE),
            wgpu::CompareFunction::Always,
            false,
        );
        let render_pipeline_no_cull_no_depth = Self::create_world_pipeline(
            &device,
            &config,
            &world_shader,
            &world_pipeline_layout,
            depth.format,
            None,
            Some(wgpu::BlendState::REPLACE),
            wgpu::CompareFunction::Always,
            false,
        );
        let render_pipeline_transparent = Self::create_world_pipeline(
            &device,
            &config,
            &world_shader,
            &world_pipeline_layout,
            depth.format,
            None,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            wgpu::CompareFunction::LessEqual,
            false,
        );
        let render_pipeline_lod = Self::create_render_pipeline_traced(
            &device,
            &wgpu::RenderPipelineDescriptor {
                label: Some("Far LOD Pipeline"),
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
                    depth_write_enabled: true,
                    depth_compare: wgpu::CompareFunction::LessEqual,
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            },
        );
        let sky_pipeline = Self::create_render_pipeline_traced(
            &device,
            &wgpu::RenderPipelineDescriptor {
                label: Some("Sky Objects Pipeline"),
                layout: Some(&sky_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &sky_shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[Vertex::layout()],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &sky_shader,
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
            },
        );

        // Highlight-Overlay Pipeline:
        // eigene Pipeline ohne Backface-Culling, damit die Face-Markierung zuverlässig sichtbar bleibt.
        let highlight_pipeline = Self::create_render_pipeline_traced(
            &device,
            &wgpu::RenderPipelineDescriptor {
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
            },
        );
        let outline_pipeline = Self::create_render_pipeline_traced(
            &device,
            &wgpu::RenderPipelineDescriptor {
                label: Some("Block Outline Pipeline"),
                layout: Some(&color_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &outline_shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[OutlineVertex::layout()],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &outline_shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: config.format,
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::LineList,
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
            },
        );

        // 2D-UI Pipeline in NDC: unabhängig von Kamera/View-Matrix.
        let ui_shader = Self::create_shader_module_traced(&device, "UI Shader", UI_SHADER_WGSL);
        let ui_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("UI Pipeline Layout"),
            bind_group_layouts: &[&texture_bind_group_layout, &camera_bind_group_layout],
            immediate_size: 0,
        });
        let ui_pipeline = Self::create_render_pipeline_traced(
            &device,
            &wgpu::RenderPipelineDescriptor {
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
            },
        );
        println!(
            "[perf][startup] pipeline creation took: {:.2}ms",
            pipeline_creation_start.elapsed().as_secs_f64() * 1000.0
        );
        perf_trace::record_duration(
            "startup.pipeline_creation_total",
            pipeline_creation_start.elapsed(),
            "",
        );

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
            vertex_capacity_bytes: std::mem::size_of_val(crosshair_vertices.as_slice()) as u64,
            index_capacity_bytes: std::mem::size_of_val(crosshair_indices.as_slice()) as u64,
        };
        let outline_vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Block Outline Vertex Buffer"),
            size: (std::mem::size_of::<OutlineVertex>() as u64) * BLOCK_OUTLINE_VERTEX_COUNT as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let preview_block = BlockId::Stone;
        let (preview_vertices, preview_indices) =
            build_block_preview_mesh_3d(preview_block, &block_texture_lookup);
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
            vertex_capacity_bytes: std::mem::size_of_val(preview_vertices.as_slice()) as u64,
            index_capacity_bytes: std::mem::size_of_val(preview_indices.as_slice()) as u64,
        };

        let (water_overlay_vertices, water_overlay_indices) = build_fullscreen_overlay_mesh(
            water_overlay_layer,
            WATER_OVERLAY_TINT,
            WATER_OVERLAY_UV_TILE,
        );
        let water_overlay_vertex_buffer =
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Water Overlay Vertex Buffer"),
                contents: bytemuck::cast_slice(&water_overlay_vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let water_overlay_index_buffer =
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Water Overlay Index Buffer"),
                contents: bytemuck::cast_slice(&water_overlay_indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        let water_overlay_mesh = ChunkMesh {
            vertex_buffer: water_overlay_vertex_buffer,
            index_buffer: water_overlay_index_buffer,
            index_count: water_overlay_indices.len() as u32,
            vertex_capacity_bytes: std::mem::size_of_val(water_overlay_vertices.as_slice()) as u64,
            index_capacity_bytes: std::mem::size_of_val(water_overlay_indices.as_slice()) as u64,
        };
        let sky_object_vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Sky Objects Vertex Buffer"),
            size: (std::mem::size_of::<Vertex>() as u64) * SKY_OBJECT_VERTEX_COUNT as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sky_object_indices: [u32; SKY_OBJECT_INDEX_COUNT as usize] =
            [0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7];
        let sky_object_index_buffer =
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Sky Objects Index Buffer"),
                contents: bytemuck::cast_slice(&sky_object_indices),
                usage: wgpu::BufferUsages::INDEX,
            });

        let mut world = match STARTUP_WORLD_MODE {
            StartupWorldMode::AlwaysRegenerate | StartupWorldMode::LoadIfPresentElseGenerate => {
                World::new()
            }
        };
        let world_meta_path = Self::world_meta_path();
        let mut startup_worldgen_preset = DEFAULT_WORLDGEN_PRESET;
        match world_meta::load(&world_meta_path) {
            Ok(Some(meta)) => {
                let cfg = Self::world_config_from_meta(&meta);
                world.configure_world_config(cfg, meta.preset);
                startup_worldgen_preset = meta.preset;
                println!(
                    "startup world: loaded {} (seed={}, preset={:?}, sea_level={}, min_y={}, max_y={}, chunk_size={}, boundary_ocean={}, boundary_radius_chunks={})",
                    world_meta_path.display(),
                    meta.world_seed,
                    meta.preset,
                    meta.sea_level,
                    meta.min_y,
                    meta.max_y,
                    meta.chunk_size,
                    meta.boundary_ocean,
                    meta.boundary_radius_chunks
                );
            }
            Ok(None) => {
                let meta = Self::build_world_meta(&world, world.seed(), startup_worldgen_preset);
                if let Err(err) = world_meta::save_atomic(&world_meta_path, &meta) {
                    panic!(
                        "failed to create {} for new world (seed={}, preset={:?}): {}",
                        world_meta_path.display(),
                        meta.world_seed,
                        meta.preset,
                        err
                    );
                }
                println!(
                    "startup world: created {} (seed={}, preset={:?}, sea_level={}, min_y={}, max_y={}, chunk_size={}, boundary_ocean={}, boundary_radius_chunks={})",
                    world_meta_path.display(),
                    meta.world_seed,
                    meta.preset,
                    meta.sea_level,
                    meta.min_y,
                    meta.max_y,
                    meta.chunk_size,
                    meta.boundary_ocean,
                    meta.boundary_radius_chunks
                );
            }
            Err(err) => {
                panic!("failed to load {}: {}", world_meta_path.display(), err);
            }
        }
        Self::load_ticking_areas_into_world(&mut world);

        let world_manifest_path = Self::world_manifest_path();
        let runtime_manifest = WorldManifest::build_from_registry(&block_registry);
        let manifest = if world_manifest_path.exists() {
            match WorldManifest::load(&world_manifest_path) {
                Ok(m) => m,
                Err(err) => {
                    panic!("failed to load {}: {}", world_manifest_path.display(), err);
                }
            }
        } else {
            if let Err(err) = runtime_manifest.save(&world_manifest_path) {
                panic!(
                    "failed to create {} for world manifest: {}",
                    world_manifest_path.display(),
                    err
                );
            }
            runtime_manifest.clone()
        };
        let id_mapper = IdMapper::from_manifest_and_registry(&manifest, &block_registry);
        let signature_changed = manifest.content_signature != runtime_manifest.content_signature;
        let missing_blocks_count = id_mapper.missing_blocks_count();
        if signature_changed || missing_blocks_count > 0 {
            let first_missing = id_mapper
                .missing_block_keys()
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            eprintln!(
                "[world_manifest] signature_changed={} saved={} runtime={} missing_blocks={}{}",
                signature_changed,
                manifest.content_signature,
                runtime_manifest.content_signature,
                missing_blocks_count,
                if first_missing.is_empty() {
                    "".to_string()
                } else {
                    format!(" first_missing=[{}]", first_missing)
                }
            );
        }
        world.set_manifest_and_mapper(manifest, id_mapper);

        let fluid_rebuild_started = std::time::Instant::now();
        let mut fluids_runtime = FluidsSystem::new(crate::world::fluids::FluidSystem::new());
        fluids_runtime.rebuild_from_world(&mut world);
        println!(
            "startup fluid rebuild: {:.2}ms",
            fluid_rebuild_started.elapsed().as_secs_f64() * 1000.0
        );

        let storage_root = PathBuf::from(SAVE_ROOT_DIR);
        let storage_reader = RegionStorage::new(storage_root.clone(), 64);
        let storage_loader = StorageLoadWorker::start(storage_root.clone())
            .unwrap_or_else(|err| panic!("failed to start storage load worker: {}", err));
        let storage_writer = StorageWorker::start(storage_root)
            .unwrap_or_else(|err| panic!("failed to start storage worker: {}", err));

        let (mesh_jobs_tx, mesh_jobs_rx) = crossbeam_channel::unbounded::<MeshJob>();
        let (mesh_results_tx, mesh_results_rx) = crossbeam_channel::unbounded::<MeshResult>();
        let mesh_pool = MeshWorkerPool::new(
            Arc::clone(&block_texture_lookup),
            mesh_jobs_rx,
            mesh_results_tx,
        );
        let (chunkgen_jobs_tx, chunkgen_jobs_rx) =
            crossbeam_channel::bounded::<ChunkGenJob>(CHUNKGEN_JOB_QUEUE_CAPACITY);
        let (chunkgen_results_tx, chunkgen_results_rx) =
            crossbeam_channel::bounded::<ChunkGenResult>(CHUNKGEN_RESULT_QUEUE_CAPACITY);
        let chunkgen_pool = ChunkGenWorkerPool::new(chunkgen_jobs_rx, chunkgen_results_tx);
        let (lighting_jobs_tx, lighting_jobs_rx) = crossbeam_channel::unbounded::<LightingJob>();
        let (lighting_results_tx, lighting_results_rx) =
            crossbeam_channel::unbounded::<LightingResult>();
        let lighting_pool = LightingWorkerPool::new(lighting_jobs_rx, lighting_results_tx);

        let dirty_chunk_queue: VecDeque<ChunkCoord> = VecDeque::new();
        let dirty_chunk_set: HashSet<ChunkCoord> = HashSet::new();

        let chunk_meshes: HashMap<ChunkCoord, LayeredChunkMesh> = HashMap::new();
        let lod_meshes: HashMap<LodColumnCoord, ChunkMesh> = HashMap::new();
        let render_distance = RenderDistance::default();
        let view_distance_chunks = render_distance.view_distance_chunks();
        let simulation_distance = SimulationDistance::default();
        let simulation_distance_chunks = simulation_distance
            .distance_chunks()
            .min(view_distance_chunks);
        let zero_light_chunk_data = Arc::new(vec![0u8; SECTION_BLOCK_COUNT]);

        println!(
            "[perf][startup] State::new setup before first frame took: {:.2}ms",
            startup_total_start.elapsed().as_secs_f64() * 1000.0
        );

        let mut state = Self {
            surface,
            device,
            queue,
            config,

            render_pipeline_back_depth,
            render_pipeline_no_cull_depth,
            render_pipeline_back_no_depth,
            render_pipeline_no_cull_no_depth,
            render_pipeline_transparent,
            render_pipeline_lod,
            sky_pipeline,
            highlight_pipeline,
            outline_pipeline,
            ui_pipeline,
            debug_backface_culling: true,
            debug_depth_test: true,
            world,
            zero_light_chunk_data,
            chunk_meshes,
            full_solid_mesh_skip_set: HashSet::new(),
            lod_meshes,
            dirty_chunk_queue,
            dirty_chunk_set,
            deferred_mesh_until_frame: HashMap::new(),
            pending_block_edits: VecDeque::new(),
            pending_block_edit_dirty_chunks: HashSet::new(),
            pending_block_edit_dirty_columns: HashSet::new(),
            pending_sand_gravity_checks: VecDeque::new(),
            pending_sand_gravity_check_set: HashSet::new(),
            highlight_mesh: None,
            outline_target_block: None,
            outline_vertex_buffer,
            crosshair_mesh,
            preview_mesh,
            preview_block,
            _texture_registry: texture_registry,
            _world_texture_array: world_texture_array,
            _sky_texture_array: sky_texture_array,
            _worldgen_ui_texture_array: worldgen_ui_texture_array,
            water_overlay_mesh,
            boundary_ocean_mesh: None,
            sky_object_vertex_buffer,
            sky_object_index_buffer,
            world_texture_bind_group,
            sky_texture_bind_group,
            worldgen_ui_texture_bind_group,
            _sky_anim_buffer: sky_anim_buffer,
            _worldgen_ui_anim_buffer: worldgen_ui_anim_buffer,
            worldgen_dirt_layer,
            sky_sun_layer,
            sky_moon_layer,
            sky_sun_uv_rect,
            moon_phase_atlas_size: [sky_tex_w, sky_tex_h],
            worldgen_font,
            block_texture_lookup,
            _mesh_pool: mesh_pool,
            _chunkgen_pool: chunkgen_pool,
            _lighting_pool: lighting_pool,
            mesh_results: mesh_results_rx,
            chunkgen_results: chunkgen_results_rx,
            lighting_results: lighting_results_rx,
            mesh_jobs: mesh_jobs_tx,
            chunkgen_jobs: chunkgen_jobs_tx,
            lighting_jobs: lighting_jobs_tx,
            inflight: HashMap::new(),
            chunkgen_inflight: HashSet::new(),
            lod_chunkgen_inflight: HashSet::new(),
            generated_empty_chunks: HashSet::new(),
            mesh_upload_queue: VecDeque::new(),
            texture_anim_tracks,
            texture_anim_gpu,
            texture_anim_accum: 0.0,
            texture_anim_buffer,
            fluid_accum: 0.0,
            sim_tick_index: 0,
            sim_systems: Vec::new(),
            fluids_system_index: None,
            block_entities_system_index: None,
            random_ticks_system_index: None,
            fluid_debug_counts: FluidDebugCounts::default(),
            frame_index: 0,
            frustum_culled_chunks_total: 0,
            meshes_skipped_due_to_frustum: 0,
            fluid_mesh_coalesce: HashMap::new(),
            fluid_dirty_due_to_fluids: HashSet::new(),
            particles: Vec::with_capacity(2048),
            particle_rng: 0x7A11_9F3D,
            render_distance,
            view_distance_chunks,
            simulation_distance,
            simulation_distance_chunks,
            active_sim_columns: HashSet::new(),
            last_sim_center: None,
            sim_columns_entered: Vec::new(),
            sim_columns_exited: Vec::new(),
            worldgen_preset: startup_worldgen_preset,
            world_generation_task: None,
            last_stream_center: None,
            last_stream_radius: -1,
            radius_offsets_cache: Vec::with_capacity(4096),
            lod_ring_offsets_cache: Vec::with_capacity(1024),
            lod_request_cursor: 0,
            chunkgen_pending_set: HashSet::new(),
            chunkgen_pending_enqueued_frame: HashMap::new(),
            chunkgen_pending_heap: BinaryHeap::new(),
            chunkgen_heap_valid_for_center: None,
            chunkgen_heap_valid_for_radius: -1,
            chunkgen_heap_last_rebuild_frame: 0,
            chunkgen_stream_epoch: 1,
            last_lod_stream_center: None,
            far_lod_enabled: ENABLE_FAR_LOD_DEFAULT,
            far_lod_requests_per_frame: LOD_REQUESTS_PER_FRAME_DEFAULT,
            pending_lod_mesh_uploads: VecDeque::new(),
            stream_last_camera_y: None,
            stream_recent_descend_frames: 0,
            stream_recent_mining_frames: 0,
            stream_horizon_sector_max_elev: [f32::NEG_INFINITY; STREAM_HORIZON_SECTOR_COUNT],
            stream_horizon_has_data: false,
            stream_horizon_built_frame: u64::MAX,
            stream_horizon_seen_columns: HashSet::with_capacity(4096),
            mesh_debug_epoch: 0,
            lod_cached_near_radius: -1,
            lod_cached_far_radius: -1,
            frame_perf: StateFramePerf::default(),
            last_render_stats_report: Instant::now(),
            last_lighting_step_ms: 0.0,
            lighting_job_inflight: false,
            lighting_generation: 0,
            lighting_request_pending: false,
            lighting_rebuild_not_before: None,
            pending_lighting_dirty_chunks: VecDeque::new(),
            pending_lighting_dirty_set: HashSet::new(),
            debug_disable_ao: false,
            debug_lighting_view: LightingDebugView::Normal,
            world_time: WorldTime::default(),
            storage_reader,
            storage_loader,
            storage_writer,
            saved_revision: HashMap::new(),
            saved_column_missing_cache: HashMap::new(),
            saved_column_loads_inflight: HashSet::new(),
            saved_load_request_cursor: 0,
            saved_load_requests_in_window: 0,
            saved_load_requests_window_started: Instant::now(),
            dirty_columns: HashSet::new(),
            dirty_column_queue: VecDeque::new(),
            dirty_column_generation: HashMap::new(),
            next_dirty_column_generation: 1,
            save_columns_inflight: HashMap::new(),
            pending_unload_columns: HashSet::new(),
            pending_unload_queue: VecDeque::new(),
            pending_unload_chunks: VecDeque::new(),
            unload_scan_keys: Vec::new(),
            unload_scan_cursor: 0,
            unload_scan_last_refresh_frame: 0,
            prune_mesh_keys: Vec::new(),
            prune_mesh_cursor: 0,
            prune_mesh_last_target: None,
            prune_mesh_last_scan_frame: 0,
            save_epoch: 0,
            autosave_sweep_active: false,
            last_autosave_at: Instant::now(),
            autosave_interval: Duration::from_secs(30),

            camera_uniform,
            camera_buffer,
            camera_bind_group,
            preview_camera_uniform,
            preview_camera_buffer,
            preview_camera_bind_group,

            depth,
        };
        state.register_sim_system(Box::new(fluids_runtime));
        state.register_sim_system(Box::new(BlockEntitiesSystem::new()));
        state.register_sim_system(Box::new(RandomTicksSystem::new(RANDOM_TICK_SPEED_DEFAULT)));
        state.rebuild_boundary_ocean_mesh();
        state
    }

    pub(crate) fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
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

    pub(crate) fn rebuild_crosshair_mesh(&mut self) {
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
            vertex_capacity_bytes: std::mem::size_of_val(vertices.as_slice()) as u64,
            index_capacity_bytes: std::mem::size_of_val(indices.as_slice()) as u64,
        };
    }

    pub(crate) fn rebuild_preview_mesh(&mut self, block: BlockId) {
        let (vertices, indices) = build_block_preview_mesh_3d(block, &self.block_texture_lookup);
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
            vertex_capacity_bytes: std::mem::size_of_val(vertices.as_slice()) as u64,
            index_capacity_bytes: std::mem::size_of_val(indices.as_slice()) as u64,
        };
        self.preview_block = block;
    }

    fn camera_chunk_coords(camera: &Camera) -> (i32, i32, i32) {
        let world_x = camera.position.x.floor() as i32;
        let world_y = camera.position.y.floor() as i32;
        let world_z = camera.position.z.floor() as i32;
        let clamped = IVec3::new(world_x, world_y.clamp(0, WORLD_MAX_Y), world_z);
        let chunk = World::world_to_chunk_coord(clamped);
        (chunk.x, chunk.y, chunk.z)
    }

    fn stable_lod_stream_center_from_camera(
        &self,
        camera: &Camera,
        raw_center_chunk_x: i32,
        raw_center_chunk_z: i32,
    ) -> (i32, i32) {
        let Some((prev_x, prev_z)) = self.last_lod_stream_center else {
            return (raw_center_chunk_x, raw_center_chunk_z);
        };

        let wx = camera.position.x.floor() as i32;
        let wz = camera.position.z.floor() as i32;

        let hysteresis = LOD_STREAM_CENTER_HYSTERESIS_BLOCKS.clamp(0, CHUNK_SIZE / 2);
        let mut stable_x = raw_center_chunk_x;
        let mut stable_z = raw_center_chunk_z;

        if raw_center_chunk_x != prev_x {
            let threshold_x = if raw_center_chunk_x > prev_x {
                (prev_x + 1) * CHUNK_SIZE + hysteresis
            } else {
                prev_x * CHUNK_SIZE - hysteresis
            };
            let crossed = if raw_center_chunk_x > prev_x {
                wx >= threshold_x
            } else {
                wx < threshold_x
            };
            if !crossed {
                stable_x = prev_x;
            }
        }

        if raw_center_chunk_z != prev_z {
            let threshold_z = if raw_center_chunk_z > prev_z {
                (prev_z + 1) * CHUNK_SIZE + hysteresis
            } else {
                prev_z * CHUNK_SIZE - hysteresis
            };
            let crossed = if raw_center_chunk_z > prev_z {
                wz >= threshold_z
            } else {
                wz < threshold_z
            };
            if !crossed {
                stable_z = prev_z;
            }
        }

        (stable_x, stable_z)
    }

    fn count_missing_meshes_near_camera(
        &self,
        center_chunk_x: i32,
        center_chunk_y: i32,
        center_chunk_z: i32,
    ) -> usize {
        let radius = STREAM_BURST_NEAR_RADIUS_CHUNKS.max(1);
        let radius_sq = radius * radius;
        let min_chunk_y = self.world.min_chunk_y();
        let max_chunk_y = self.world.max_chunk_y();
        let vertical_radius = VERTICAL_VIEW_RADIUS_CHUNKS.min(2);
        let cy0 = (center_chunk_y - vertical_radius).max(min_chunk_y);
        let cy1 = (center_chunk_y + vertical_radius).min(max_chunk_y);
        let mut missing = 0usize;

        for dz in -radius..=radius {
            for dx in -radius..=radius {
                if dx * dx + dz * dz > radius_sq {
                    continue;
                }
                let x = center_chunk_x + dx;
                let z = center_chunk_z + dz;
                for cy in cy0..=cy1 {
                    let coord = ChunkCoord { x, y: cy, z };
                    if self.world.get_chunk(coord).is_none()
                        || self.generated_empty_chunks.contains(&coord)
                        || self.chunk_meshes.contains_key(&coord)
                    {
                        continue;
                    }
                    missing += 1;
                    if missing >= STREAM_BURST_MISSING_MESHES_THRESHOLD {
                        return missing;
                    }
                }
            }
        }

        missing
    }

    fn stream_prefer_camera_band(&self) -> bool {
        self.stream_recent_descend_frames > 0 || self.stream_recent_mining_frames > 0
    }

    fn update_stream_motion_hints_for_camera(&mut self, camera: &Camera) {
        if let Some(prev_y) = self.stream_last_camera_y {
            if camera.position.y + SURFACE_STREAM_DESCENT_TRIGGER_BLOCKS_PER_FRAME < prev_y {
                self.stream_recent_descend_frames = SURFACE_STREAM_DESCENT_HOLD_FRAMES;
            } else {
                self.stream_recent_descend_frames =
                    self.stream_recent_descend_frames.saturating_sub(1);
            }
        }
        self.stream_last_camera_y = Some(camera.position.y);
        self.stream_recent_mining_frames = self.stream_recent_mining_frames.saturating_sub(1);
    }

    fn update_stream_burst_mode_for_camera(&mut self, camera: &Camera) {
        self.update_stream_motion_hints_for_camera(camera);
        let (center_chunk_x, center_chunk_y, center_chunk_z) = Self::camera_chunk_coords(camera);
        let missing =
            self.count_missing_meshes_near_camera(center_chunk_x, center_chunk_y, center_chunk_z);
        let burst_active = self.world_generation_task.is_some()
            || missing >= STREAM_BURST_MISSING_MESHES_THRESHOLD;
        self.frame_perf.stream_burst_mode_active = burst_active;
        self.frame_perf.stream_missing_meshes_near_camera = missing;
    }

    fn stream_burst_budget_used_ms(&self) -> f32 {
        self.frame_perf.stream_dispatch_used_ms
            + self.frame_perf.stream_mesh_upload_used_ms
            + self.frame_perf.stream_chunkgen_apply_used_ms
    }

    fn stream_budget_for_stage_ms(&self, base_budget_ms: f32, burst_budget_ms: f32) -> f32 {
        let burst_active =
            self.frame_perf.stream_burst_mode_active || self.world_generation_task.is_some();
        if !burst_active {
            return base_budget_ms.max(0.0);
        }
        let requested = burst_budget_ms.max(0.0);
        let remaining =
            (STREAM_BURST_MAX_TOTAL_BUDGET_MS - self.stream_burst_budget_used_ms()).max(0.0);
        requested.min(remaining)
    }

    fn camera_forward_xz(camera: &Camera) -> Option<Vec3> {
        let mut forward = camera.forward();
        forward.y = 0.0;
        (forward.length_squared() > 1e-6).then(|| forward.normalize())
    }

    fn stream_priority_age_from_enqueued_frame(&self, enqueued_frame: u64) -> u32 {
        let waited = self
            .frame_index
            .saturating_sub(enqueued_frame)
            .min(u32::MAX as u64) as u32;
        u32::MAX - waited
    }

    fn stream_estimated_surface_height_for_coord(&self, coord: ChunkCoord) -> i32 {
        let sample_world_x = coord.x * CHUNK_SIZE + CHUNK_SIZE / 2;
        let sample_world_z = coord.z * CHUNK_SIZE + CHUNK_SIZE / 2;
        self.world
            .estimated_surface_height_at(sample_world_x, sample_world_z)
    }

    fn stream_y_band_rank_for_coord(
        &self,
        coord: ChunkCoord,
        camera_chunk_y: i32,
        surface_y: i32,
    ) -> u8 {
        let surface_chunk_y = surface_y
            .div_euclid(CHUNK_SIZE)
            .clamp(self.world.min_chunk_y(), self.world.max_chunk_y());
        let camera_world_y = camera_chunk_y * CHUNK_SIZE + CHUNK_SIZE / 2;
        let underground = camera_world_y < surface_y - SURFACE_STREAM_UNDERGROUND_TRIGGER_BLOCKS;
        let prefer_camera_band = underground || self.stream_prefer_camera_band();

        let primary_center_y = if prefer_camera_band {
            camera_chunk_y
        } else {
            surface_chunk_y
        };
        let secondary_center_y = if prefer_camera_band {
            surface_chunk_y
        } else {
            camera_chunk_y
        };
        let primary_dist = (coord.y - primary_center_y).abs();
        let secondary_dist = (coord.y - secondary_center_y).abs();
        let base = primary_dist.min(secondary_dist).min(250) as u16;
        let boost = if primary_dist <= SURFACE_STREAM_PRIMARY_HALF_BAND_CHUNKS {
            0
        } else if secondary_dist <= SURFACE_STREAM_PRIMARY_HALF_BAND_CHUNKS {
            2
        } else {
            4
        };
        (boost + base).min(u8::MAX as u16) as u8
    }

    fn stream_horizon_sector_index(dx: f32, dz: f32) -> usize {
        let azimuth = dz.atan2(dx);
        let normalized = (azimuth + PI) / (2.0 * PI);
        let idx = (normalized * STREAM_HORIZON_SECTOR_COUNT as f32).floor() as i32;
        idx.clamp(0, STREAM_HORIZON_SECTOR_COUNT as i32 - 1) as usize
    }

    fn stream_sector_and_elevation_angle_for_column(
        camera: &Camera,
        chunk_x: i32,
        surface_y: i32,
        chunk_z: i32,
    ) -> Option<(usize, f32)> {
        let world_x = (chunk_x * CHUNK_SIZE + CHUNK_SIZE / 2) as f32;
        let world_z = (chunk_z * CHUNK_SIZE + CHUNK_SIZE / 2) as f32;
        let dx = world_x - camera.position.x;
        let dz = world_z - camera.position.z;
        let horizontal_dist = (dx * dx + dz * dz).sqrt();
        if horizontal_dist <= 1e-3 {
            return None;
        }
        let sector = Self::stream_horizon_sector_index(dx, dz);
        let world_y = surface_y as f32 + 0.5;
        let elevation_angle = (world_y - camera.position.y).atan2(horizontal_dist);
        Some((sector, elevation_angle))
    }

    fn refresh_stream_horizon_map(&mut self, camera: &Camera, frustum: &Frustum) {
        if self.stream_horizon_built_frame == self.frame_index {
            return;
        }

        self.stream_horizon_sector_max_elev.fill(f32::NEG_INFINITY);
        self.stream_horizon_seen_columns.clear();

        let chunk_meshes = &self.chunk_meshes;
        let world = &self.world;
        let seen_columns = &mut self.stream_horizon_seen_columns;
        let horizon_map = &mut self.stream_horizon_sector_max_elev;
        let mut has_data = false;

        for &coord in chunk_meshes.keys() {
            if !seen_columns.insert((coord.x, coord.z)) {
                continue;
            }

            let sample_world_x = coord.x * CHUNK_SIZE + CHUNK_SIZE / 2;
            let sample_world_z = coord.z * CHUNK_SIZE + CHUNK_SIZE / 2;
            let surface_y = world.estimated_surface_height_at(sample_world_x, sample_world_z);
            let surface_chunk_y = surface_y
                .div_euclid(CHUNK_SIZE)
                .clamp(world.min_chunk_y(), world.max_chunk_y());
            let surface_coord = ChunkCoord {
                x: coord.x,
                y: surface_chunk_y,
                z: coord.z,
            };
            if !chunk_meshes.contains_key(&surface_coord)
                || !chunk_in_frustum(frustum, surface_coord)
            {
                continue;
            }

            let Some((sector, elevation_angle)) =
                Self::stream_sector_and_elevation_angle_for_column(
                    camera, coord.x, surface_y, coord.z,
                )
            else {
                continue;
            };
            if elevation_angle > horizon_map[sector] {
                horizon_map[sector] = elevation_angle;
                has_data = true;
            }
        }

        self.stream_horizon_has_data = has_data;
        self.stream_horizon_built_frame = self.frame_index;
    }

    fn stream_horizon_penalty_rank_for_coord(
        &self,
        coord: ChunkCoord,
        camera: Option<&Camera>,
        surface_y: i32,
    ) -> u8 {
        let Some(camera) = camera else {
            return 0;
        };
        if self.stream_horizon_built_frame != self.frame_index || !self.stream_horizon_has_data {
            return 0;
        }
        let Some((sector, candidate_elevation)) =
            Self::stream_sector_and_elevation_angle_for_column(camera, coord.x, surface_y, coord.z)
        else {
            return 0;
        };
        let horizon_elevation = self.stream_horizon_sector_max_elev[sector];
        if !horizon_elevation.is_finite() {
            return 0;
        }

        let occluded_by = horizon_elevation - candidate_elevation;
        if occluded_by <= STREAM_HORIZON_OCCLUDED_MARGIN_RAD {
            return 0;
        }

        let steps = ((occluded_by - STREAM_HORIZON_OCCLUDED_MARGIN_RAD)
            / STREAM_HORIZON_PENALTY_STEP_RAD)
            .floor() as i32
            + 1;
        steps.clamp(1, STREAM_HORIZON_MAX_PENALTY_RANK as i32) as u8
    }

    fn stream_view_alignment_rank(
        coord: ChunkCoord,
        center_chunk_x: i32,
        center_chunk_z: i32,
        forward_xz: Option<Vec3>,
    ) -> u8 {
        let Some(forward) = forward_xz else {
            return 0;
        };
        let dx = coord.x - center_chunk_x;
        let dz = coord.z - center_chunk_z;
        if dx == 0 && dz == 0 {
            return 0;
        }
        let mut to = Vec3::new(dx as f32, 0.0, dz as f32);
        if to.length_squared() <= 1e-6 {
            return 0;
        }
        to = to.normalize();
        let dot = forward.dot(to).clamp(-1.0, 1.0);
        let t = ((1.0 - dot) * 0.5).clamp(0.0, 1.0);
        (t * STREAM_PRIORITY_VIEW_ALIGNMENT_BUCKETS as f32).floor() as u8
    }

    fn build_frustum_for_camera(&self, camera: &Camera) -> Frustum {
        let aspect = (self.config.width as f32 / self.config.height as f32).max(0.0001);
        Frustum::from_view_proj(build_view_proj_wgpu(camera, aspect))
    }

    fn chunk_visibility_score_for_frustum(frustum: &Frustum, chunk_coord: ChunkCoord) -> u32 {
        if chunk_in_frustum(frustum, chunk_coord) {
            0
        } else {
            1
        }
    }

    fn chunk_visibility_score(&self, camera: &Camera, chunk_coord: ChunkCoord) -> u32 {
        let frustum = self.build_frustum_for_camera(camera);
        Self::chunk_visibility_score_for_frustum(&frustum, chunk_coord)
    }

    fn stream_priority_key_for_chunk(
        &self,
        coord: ChunkCoord,
        center_chunk_x: i32,
        center_chunk_y: i32,
        center_chunk_z: i32,
        camera: Option<&Camera>,
        frustum: Option<&Frustum>,
        forward_xz: Option<Vec3>,
        age: u32,
    ) -> StreamPriorityKey {
        let dx = coord.x - center_chunk_x;
        let dy = coord.y - center_chunk_y;
        let dz = coord.z - center_chunk_z;
        let dist2 = dx * dx + dy * dy + dz * dz;
        let visibility_score = frustum
            .map(|f| Self::chunk_visibility_score_for_frustum(f, coord))
            .unwrap_or(0);
        let view_alignment_rank =
            Self::stream_view_alignment_rank(coord, center_chunk_x, center_chunk_z, forward_xz);
        let surface_y = self.stream_estimated_surface_height_for_coord(coord);
        let y_band_rank = self.stream_y_band_rank_for_coord(coord, center_chunk_y, surface_y);
        let horizon_penalty_rank =
            self.stream_horizon_penalty_rank_for_coord(coord, camera, surface_y);

        StreamPriorityKey {
            dist2,
            visibility_score,
            horizon_penalty_rank,
            view_alignment_rank,
            y_band_rank,
            age,
        }
    }

    fn clear_chunkgen_pending_requests(&mut self) {
        self.chunkgen_pending_set.clear();
        self.chunkgen_pending_enqueued_frame.clear();
        self.chunkgen_pending_heap.clear();
        self.chunkgen_heap_valid_for_center = None;
        self.chunkgen_heap_valid_for_radius = -1;
    }

    fn build_worldgen_warmup_coords(
        &self,
        center_chunk_x: i32,
        center_chunk_y: i32,
        center_chunk_z: i32,
    ) -> HashSet<ChunkCoord> {
        let mut coords = HashSet::new();
        let min_chunk_y = self.world.min_chunk_y();
        let max_chunk_y = self.world.max_chunk_y();
        let radius_sq = CHUNK_WARMUP_RADIUS * CHUNK_WARMUP_RADIUS;
        for dz in -CHUNK_WARMUP_RADIUS..=CHUNK_WARMUP_RADIUS {
            for dx in -CHUNK_WARMUP_RADIUS..=CHUNK_WARMUP_RADIUS {
                if dx * dx + dz * dz > radius_sq {
                    continue;
                }
                let x = center_chunk_x + dx;
                let z = center_chunk_z + dz;
                let plan =
                    self.build_column_stream_plan(x, center_chunk_y, z, min_chunk_y, max_chunk_y);
                for cy in plan.tier0_y.into_iter().chain(plan.tier1_y.into_iter()) {
                    coords.insert(ChunkCoord { x, y: cy, z });
                }
            }
        }
        coords
    }

    fn build_worldgen_warmup_columns(
        &self,
        center_chunk_x: i32,
        center_chunk_z: i32,
    ) -> HashSet<ColumnCoord> {
        let mut cols = HashSet::new();
        let radius_sq = CHUNK_WARMUP_RADIUS * CHUNK_WARMUP_RADIUS;
        for dz in -CHUNK_WARMUP_RADIUS..=CHUNK_WARMUP_RADIUS {
            for dx in -CHUNK_WARMUP_RADIUS..=CHUNK_WARMUP_RADIUS {
                if dx * dx + dz * dz > radius_sq {
                    continue;
                }
                cols.insert(ColumnCoord {
                    x: center_chunk_x + dx,
                    z: center_chunk_z + dz,
                });
            }
        }
        cols
    }

    fn is_worldgen_warmup_coord(&self, coord: ChunkCoord) -> bool {
        self.world_generation_task
            .as_ref()
            .is_some_and(|task| task.warmup_coords.contains(&coord))
    }

    fn refresh_worldgen_warmup_tracking(&mut self) {
        let Some(task_ref) = self.world_generation_task.as_ref() else {
            return;
        };
        let warmup_coords: Vec<ChunkCoord> = task_ref.warmup_coords.iter().copied().collect();
        let warmup_columns: Vec<ColumnCoord> = task_ref.warmup_columns.iter().copied().collect();

        let mut generated_done = HashSet::with_capacity(warmup_coords.len());
        let mut meshed_done = HashSet::with_capacity(warmup_coords.len());
        for coord in warmup_coords {
            let is_generated = self.generated_empty_chunks.contains(&coord)
                || self.world.get_chunk(coord).is_some();
            if is_generated {
                generated_done.insert(coord);
            }
            let is_meshed_or_empty = self.generated_empty_chunks.contains(&coord)
                || self.chunk_meshes.contains_key(&coord);
            if is_meshed_or_empty {
                meshed_done.insert(coord);
            }
        }
        let mut generated_columns = HashSet::with_capacity(warmup_columns.len());
        for col in warmup_columns {
            let mut all_sections_done = true;
            for y in self.world.min_chunk_y()..=self.world.max_chunk_y() {
                let coord = ChunkCoord {
                    x: col.x,
                    y,
                    z: col.z,
                };
                let is_generated =
                    self.generated_empty_chunks.contains(&coord) || self.world.get_chunk(coord).is_some();
                if !is_generated {
                    all_sections_done = false;
                    break;
                }
            }
            if all_sections_done {
                generated_columns.insert(col);
            }
        }

        let Some(task) = self.world_generation_task.as_mut() else {
            return;
        };
        task.warmup_columns_generated_done = generated_columns;
        task.warmup_generated_done = generated_done;
        task.warmup_meshed_done = meshed_done;

        let target_columns = task.warmup_column_target.max(1);
        let generated_columns_count = task.warmup_columns_generated_done.len();
        let progress01 = (generated_columns_count as f32 / target_columns as f32).clamp(0.0, 1.0);
        let phase = format!(
            "Generating columns {}/{}",
            generated_columns_count, target_columns
        );
        let progress01 = progress01.clamp(0.0, 1.0);
        let progress_permille = (progress01 * 1000.0).round().clamp(0.0, 1000.0) as u16;
        if task.last_reported_phase != phase
            || task.last_reported_progress_permille != progress_permille
        {
            task.pending_progress = Some(WorldGenProgress {
                phase: phase.clone(),
                progress01,
            });
            task.last_reported_phase = phase;
            task.last_reported_progress_permille = progress_permille;
        }
    }

    fn push_chunk_y_band_unique(
        y_order: &mut Vec<i32>,
        center_chunk_y: i32,
        half_band: i32,
        min_chunk_y: i32,
        max_chunk_y: i32,
    ) {
        let clamped_half = half_band.max(0);
        for offset in 0..=clamped_half {
            let up = center_chunk_y + offset;
            if (min_chunk_y..=max_chunk_y).contains(&up) && !y_order.contains(&up) {
                y_order.push(up);
            }
            if offset == 0 {
                continue;
            }
            let down = center_chunk_y - offset;
            if (min_chunk_y..=max_chunk_y).contains(&down) && !y_order.contains(&down) {
                y_order.push(down);
            }
        }
    }

    fn surface_stream_secondary_extra_chunks(&self) -> i32 {
        if self.world_generation_task.is_some() {
            return 0;
        }
        (self.frame_index / SURFACE_STREAM_SECONDARY_EXPAND_PERIOD_FRAMES)
            .min(SURFACE_STREAM_SECONDARY_MAX_EXTRA_CHUNKS as u64) as i32
    }

    fn append_band_into_tier(
        tier: &mut Vec<i32>,
        tier0: &[i32],
        center_chunk_y: i32,
        half_band: i32,
        min_chunk_y: i32,
        max_chunk_y: i32,
    ) {
        let mut staged = Vec::with_capacity(9);
        Self::push_chunk_y_band_unique(
            &mut staged,
            center_chunk_y,
            half_band,
            min_chunk_y,
            max_chunk_y,
        );
        for cy in staged {
            if tier0.contains(&cy) || tier.contains(&cy) {
                continue;
            }
            tier.push(cy);
        }
    }

    fn build_column_stream_plan(
        &self,
        column_chunk_x: i32,
        camera_chunk_y: i32,
        column_chunk_z: i32,
        min_chunk_y: i32,
        max_chunk_y: i32,
    ) -> ColumnStreamPlan {
        let sample_world_x = column_chunk_x * CHUNK_SIZE + CHUNK_SIZE / 2;
        let sample_world_z = column_chunk_z * CHUNK_SIZE + CHUNK_SIZE / 2;
        let surface_y = self
            .world
            .estimated_surface_height_at(sample_world_x, sample_world_z);
        let surface_chunk_y = surface_y
            .div_euclid(CHUNK_SIZE)
            .clamp(min_chunk_y, max_chunk_y);
        let camera_world_y = camera_chunk_y * CHUNK_SIZE + CHUNK_SIZE / 2;
        let underground = camera_world_y < surface_y - SURFACE_STREAM_UNDERGROUND_TRIGGER_BLOCKS;
        let prefer_camera_band = underground || self.stream_prefer_camera_band();
        let secondary_extra = self.surface_stream_secondary_extra_chunks();

        let primary_center_y = if prefer_camera_band {
            camera_chunk_y
        } else {
            surface_chunk_y
        };
        let secondary_center_y = if prefer_camera_band {
            surface_chunk_y
        } else {
            camera_chunk_y
        };

        let mut tier0_y = Vec::with_capacity(3);
        Self::push_chunk_y_band_unique(
            &mut tier0_y,
            primary_center_y,
            SURFACE_STREAM_PRIMARY_HALF_BAND_CHUNKS,
            min_chunk_y,
            max_chunk_y,
        );

        let mut tier1_y = Vec::with_capacity(8);
        // Keep initial spawn focused on 2-3 surface sections; unlock tier-1 once expansion starts,
        // unless camera-band priority is explicitly requested (underground/mining/descent).
        if prefer_camera_band || secondary_extra > 0 {
            Self::append_band_into_tier(
                &mut tier1_y,
                &tier0_y,
                secondary_center_y,
                SURFACE_STREAM_PRIMARY_HALF_BAND_CHUNKS,
                min_chunk_y,
                max_chunk_y,
            );
        }
        if secondary_extra > 0 {
            let expand_half = SURFACE_STREAM_PRIMARY_HALF_BAND_CHUNKS + secondary_extra;
            Self::append_band_into_tier(
                &mut tier1_y,
                &tier0_y,
                primary_center_y,
                expand_half,
                min_chunk_y,
                max_chunk_y,
            );
            if prefer_camera_band {
                Self::append_band_into_tier(
                    &mut tier1_y,
                    &tier0_y,
                    secondary_center_y,
                    expand_half,
                    min_chunk_y,
                    max_chunk_y,
                );
            }
        }

        ColumnStreamPlan { tier0_y, tier1_y }
    }

    fn column_stream_chunk_y_order(
        &self,
        column_chunk_x: i32,
        camera_chunk_y: i32,
        column_chunk_z: i32,
        min_chunk_y: i32,
        max_chunk_y: i32,
    ) -> Vec<i32> {
        let plan = self.build_column_stream_plan(
            column_chunk_x,
            camera_chunk_y,
            column_chunk_z,
            min_chunk_y,
            max_chunk_y,
        );
        let mut y_order = Vec::with_capacity(plan.tier0_y.len() + plan.tier1_y.len());
        y_order.extend(plan.tier0_y);
        y_order.extend(plan.tier1_y);
        y_order
    }

    fn is_chunk_within_radius_xz(
        coord: ChunkCoord,
        center_chunk_x: i32,
        center_chunk_z: i32,
        radius: i32,
    ) -> bool {
        let dx = coord.x - center_chunk_x;
        let dz = coord.z - center_chunk_z;
        dx * dx + dz * dz <= radius * radius
    }

    fn near_mesh_radius_chunks(&self) -> i32 {
        NEAR_MESH_RADIUS_CHUNKS
            .min(self.view_distance_chunks)
            .max(0)
    }

    fn lod_ring_radii_chunks(&self) -> (i32, i32) {
        (
            self.near_mesh_radius_chunks(),
            self.view_distance_chunks.max(0),
        )
    }

    fn rebuild_radius_offsets_cache(&mut self, radius: i32) {
        let radius_sq = radius * radius;
        self.radius_offsets_cache.clear();
        self.radius_offsets_cache
            .reserve(((radius * 2 + 1) * (radius * 2 + 1)) as usize);
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                if dx * dx + dz * dz <= radius_sq {
                    self.radius_offsets_cache.push((dx, dz));
                }
            }
        }
        self.radius_offsets_cache.sort_by_key(|(dx, dz)| {
            let manhattan = dx.abs() + dz.abs();
            let dist2 = dx * dx + dz * dz;
            (manhattan, dist2)
        });
        self.last_stream_radius = radius;
    }

    fn request_chunk_generation(
        &mut self,
        coord: ChunkCoord,
        preset: WorldgenPreset,
    ) -> bool {
        if self.world.get_chunk(coord).is_some()
            || self.generated_empty_chunks.contains(&coord)
            || self.chunkgen_inflight.contains(&coord)
        {
            return false;
        }
        if self.chunkgen_inflight.len() >= MAX_INFLIGHT_CHUNKGEN {
            return false;
        }

        let world_cfg = self.world.config();
        match self.chunkgen_jobs.try_send(ChunkGenJob {
            coord,
            preset,
            world_cfg,
            request_kind: ChunkGenRequestKind::FullChunk,
            epoch: self.chunkgen_stream_epoch,
        }) {
            Ok(()) => {
                self.chunkgen_inflight.insert(coord);
                true
            }
            Err(CrossbeamTrySendError::Full(_)) => false,
            Err(CrossbeamTrySendError::Disconnected(_)) => false,
        }
    }

    fn request_lod_column_generation(
        &mut self,
        coord: LodColumnCoord,
        preset: WorldgenPreset,
    ) -> bool {
        if self.lod_meshes.contains_key(&coord) || self.lod_chunkgen_inflight.contains(&coord) {
            return false;
        }
        if self.lod_chunkgen_inflight.len() >= MAX_INFLIGHT_LOD_CHUNKGEN
            || self.chunkgen_inflight.len() >= MAX_INFLIGHT_CHUNKGEN
        {
            return false;
        }

        let job_coord = ChunkCoord {
            x: coord.x,
            y: self.world.min_chunk_y(),
            z: coord.z,
        };
        let world_cfg = self.world.config();
        match self.chunkgen_jobs.try_send(ChunkGenJob {
            coord: job_coord,
            preset,
            world_cfg,
            request_kind: ChunkGenRequestKind::LodColumnOnly,
            epoch: self.chunkgen_stream_epoch,
        }) {
            Ok(()) => {
                self.lod_chunkgen_inflight.insert(coord);
                true
            }
            Err(CrossbeamTrySendError::Full(_)) => false,
            Err(CrossbeamTrySendError::Disconnected(_)) => false,
        }
    }

    fn enqueue_chunkgen_candidates_in_radius(
        &mut self,
        center_chunk_x: i32,
        center_chunk_y: i32,
        center_chunk_z: i32,
        radius: i32,
    ) {
        if self.last_stream_radius != radius || self.radius_offsets_cache.is_empty() {
            self.rebuild_radius_offsets_cache(radius);
        }
        if self.radius_offsets_cache.is_empty() {
            return;
        }
        let min_chunk_y = self.world.min_chunk_y();
        let max_chunk_y = self.world.max_chunk_y();
        let radius_sq = radius * radius;
        for &(dx, dz) in &self.radius_offsets_cache {
            if dx * dx + dz * dz > radius_sq {
                continue;
            }
            let x = center_chunk_x + dx;
            let z = center_chunk_z + dz;
            let plan =
                self.build_column_stream_plan(x, center_chunk_y, z, min_chunk_y, max_chunk_y);
            for cy in plan.tier0_y.into_iter().chain(plan.tier1_y.into_iter()) {
                let coord = ChunkCoord { x, y: cy, z };
                if self.world.get_chunk(coord).is_some()
                    || self.generated_empty_chunks.contains(&coord)
                    || self.chunkgen_inflight.contains(&coord)
                {
                    continue;
                }
                if self.chunkgen_pending_set.insert(coord) {
                    self.chunkgen_pending_enqueued_frame
                        .insert(coord, self.frame_index);
                }
            }
        }

        let mut stale = Vec::with_capacity(128);
        for &coord in &self.chunkgen_pending_set {
            let dx = coord.x - center_chunk_x;
            let dz = coord.z - center_chunk_z;
            let outside_radius = dx * dx + dz * dz > radius_sq;
            let unavailable = self.world.get_chunk(coord).is_some()
                || self.generated_empty_chunks.contains(&coord)
                || self.chunkgen_inflight.contains(&coord);
            if outside_radius || unavailable {
                stale.push(coord);
            }
        }
        for coord in stale {
            self.chunkgen_pending_set.remove(&coord);
            self.chunkgen_pending_enqueued_frame.remove(&coord);
        }
    }

    fn queue_chunk_generation_in_radius_with_budget(
        &mut self,
        center_chunk_x: i32,
        center_chunk_y: i32,
        center_chunk_z: i32,
        radius: i32,
        budget_chunks: usize,
        camera: Option<&Camera>,
    ) -> usize {
        const REBUILD_PERIOD: u64 = CHUNKGEN_PRIORITY_HEAP_REBUILD_PERIOD_FRAMES;
        if budget_chunks == 0 {
            return 0;
        }

        self.enqueue_chunkgen_candidates_in_radius(
            center_chunk_x,
            center_chunk_y,
            center_chunk_z,
            radius,
        );

        let center = (center_chunk_x, center_chunk_y, center_chunk_z);
        let center_changed = self.chunkgen_heap_valid_for_center != Some(center);
        let radius_changed = self.chunkgen_heap_valid_for_radius != radius;
        let periodic_rebuild = self
            .frame_index
            .saturating_sub(self.chunkgen_heap_last_rebuild_frame)
            >= REBUILD_PERIOD;
        let needs_rebuild = center_changed
            || radius_changed
            || periodic_rebuild
            || self.chunkgen_pending_heap.is_empty();

        if needs_rebuild {
            self.chunkgen_pending_heap.clear();
            self.chunkgen_pending_heap
                .reserve(self.chunkgen_pending_set.len());

            let forward_xz = camera.and_then(Self::camera_forward_xz);
            let frustum = camera.map(|camera| self.build_frustum_for_camera(camera));
            if let (Some(camera), Some(frustum)) = (camera, frustum.as_ref()) {
                self.refresh_stream_horizon_map(camera, frustum);
            }

            for &coord in &self.chunkgen_pending_set {
                let enqueued_frame = self
                    .chunkgen_pending_enqueued_frame
                    .get(&coord)
                    .copied()
                    .unwrap_or(self.frame_index);
                let age = self.stream_priority_age_from_enqueued_frame(enqueued_frame);
                let key = self.stream_priority_key_for_chunk(
                    coord,
                    center_chunk_x,
                    center_chunk_y,
                    center_chunk_z,
                    camera,
                    frustum.as_ref(),
                    forward_xz,
                    age,
                );
                self.chunkgen_pending_heap
                    .push(ChunkPriorityHeapEntry { coord, key });
            }

            self.chunkgen_heap_valid_for_center = Some(center);
            self.chunkgen_heap_valid_for_radius = radius;
            self.chunkgen_heap_last_rebuild_frame = self.frame_index;
        }

        let preset = self.worldgen_preset;
        let mut queued = 0usize;
        while queued < budget_chunks && self.chunkgen_inflight.len() < MAX_INFLIGHT_CHUNKGEN {
            let Some(entry) = self.chunkgen_pending_heap.pop() else {
                break;
            };
            let coord = entry.coord;
            if !self.chunkgen_pending_set.contains(&coord) {
                continue;
            }
            if self.world.get_chunk(coord).is_some()
                || self.generated_empty_chunks.contains(&coord)
                || self.chunkgen_inflight.contains(&coord)
            {
                self.chunkgen_pending_set.remove(&coord);
                self.chunkgen_pending_enqueued_frame.remove(&coord);
                continue;
            }
            if self.request_chunk_generation(coord, preset) {
                queued += 1;
                self.chunkgen_pending_set.remove(&coord);
                self.chunkgen_pending_enqueued_frame.remove(&coord);
            } else {
                break;
            }
        }
        perf_trace::record_duration_ms(
            "state.queue_chunkgen_requests",
            0.0,
            &format!(
                "queued={} pending={} inflight={} job_q={}",
                queued,
                self.chunkgen_pending_set.len(),
                self.chunkgen_inflight.len(),
                self.chunkgen_jobs.len()
            ),
        );
        queued
    }

    fn ensure_chunks_in_radius(
        &mut self,
        center_chunk_x: i32,
        center_chunk_y: i32,
        center_chunk_z: i32,
        radius: i32,
        camera: Option<&Camera>,
    ) {
        let mut budget = CHUNKGEN_REQUESTS_PER_FRAME;
        let unload_pressure =
            self.pending_unload_columns.len() + self.pending_unload_queue.len() / 2;
        if self.dirty_columns.len() >= CHUNKGEN_BACKPRESSURE_DIRTY_COLUMNS_THRESHOLD
            || unload_pressure >= CHUNKGEN_BACKPRESSURE_PENDING_UNLOAD_COLUMNS_THRESHOLD
        {
            budget = budget.min(
                CHUNKGEN_REQUESTS_PER_FRAME_BACKPRESSURE_MIN.max(CHUNKGEN_REQUESTS_PER_FRAME / 4),
            );
        }
        let _ = self.queue_chunk_generation_in_radius_with_budget(
            center_chunk_x,
            center_chunk_y,
            center_chunk_z,
            radius,
            budget,
            camera,
        );

        let mut forced_total = 0usize;
        let mut budget_left = TICKING_AREA_STREAM_BUDGET_PER_FRAME;
        let areas: Vec<TickingArea> = self
            .world
            .ticking_areas()
            .iter()
            .copied()
            .filter(Self::ticking_area_requires_loading)
            .collect();
        for area in areas {
            if budget_left == 0 {
                break;
            }
            let queued = self.queue_chunk_generation_in_radius_with_budget(
                area.center.x,
                center_chunk_y,
                area.center.y,
                area.radius_chunks,
                budget_left,
                camera,
            );
            forced_total += queued;
            budget_left = budget_left.saturating_sub(queued);
        }
        if forced_total > 0 && self.frame_index % 120 == 0 {
            println!(
                "ticking areas forced {} chunkgen request(s) this frame",
                forced_total
            );
        }
    }

    fn rebuild_lod_ring_offsets_cache(&mut self, near_radius: i32, far_radius: i32) {
        self.lod_ring_offsets_cache.clear();
        let far_sq = far_radius * far_radius;
        let near_sq = near_radius * near_radius;
        for dz in -far_radius..=far_radius {
            for dx in -far_radius..=far_radius {
                let dist2 = dx * dx + dz * dz;
                if dist2 > far_sq || dist2 <= near_sq {
                    continue;
                }
                self.lod_ring_offsets_cache.push((dx, dz));
            }
        }
        self.lod_ring_offsets_cache.sort_by_key(|(dx, dz)| {
            let manhattan = dx.abs() + dz.abs();
            let dist2 = dx * dx + dz * dz;
            (manhattan, dist2)
        });
        self.lod_request_cursor = 0;
        self.lod_cached_near_radius = near_radius;
        self.lod_cached_far_radius = far_radius;
    }

    fn prune_lod_meshes_outside_radius(
        &mut self,
        center_chunk_x: i32,
        center_chunk_z: i32,
        radius: i32,
    ) {
        let radius_sq = radius * radius;
        self.lod_meshes.retain(|coord, _| {
            let dx = coord.x - center_chunk_x;
            let dz = coord.z - center_chunk_z;
            dx * dx + dz * dz <= radius_sq
        });
    }

    fn update_lod_streaming(&mut self, center_chunk_x: i32, center_chunk_z: i32) {
        let _trace_scope = perf_trace::scope("state.update_lod_streaming");
        if !self.far_lod_enabled {
            return;
        }

        let (near_radius, far_radius) = self.lod_ring_radii_chunks();
        let center = (center_chunk_x, center_chunk_z);
        let old_center = self.last_lod_stream_center;

        if old_center != Some(center)
            || self.lod_cached_near_radius != near_radius
            || self.lod_cached_far_radius != far_radius
        {
            if let Some(prev) = old_center
                && prev != center
            {
                self.frame_perf.far_lod_center_changes += 1;
                self.frame_perf.lod_center_changed_this_frame = Some((prev, center));
                eprintln!(
                    "[perf][lod] center_changed from=({}, {}) to=({}, {}) near={} far={} cursor_reset",
                    prev.0, prev.1, center.0, center.1, near_radius, far_radius
                );
            }
            self.last_lod_stream_center = Some(center);
            self.rebuild_lod_ring_offsets_cache(near_radius, far_radius);
            self.prune_lod_meshes_outside_radius(center_chunk_x, center_chunk_z, far_radius);
            let far_sq = far_radius * far_radius;
            self.lod_chunkgen_inflight.retain(|coord| {
                let dx = coord.x - center_chunk_x;
                let dz = coord.z - center_chunk_z;
                dx * dx + dz * dz <= far_sq
            });
        }

        let mut request_budget = self.far_lod_requests_per_frame;
        let state_cpu_ms = self.frame_perf.elapsed_ms_since_frame_start();
        if state_cpu_ms >= FAR_LOD_CPU_SKIP_AFTER_MS {
            self.frame_perf.far_lod_requests_suppressed_cpu_budget = true;
            request_budget = 0;
        }
        let backlog_pressure = self.chunkgen_results.len()
            + self.mesh_results.len()
            + self.mesh_upload_queue.len()
            + self.pending_lod_mesh_uploads.len();
        if backlog_pressure > 128 {
            self.frame_perf.far_lod_requests_suppressed_backlog = true;
            request_budget = 0;
        }

        let preset = self.worldgen_preset;
        let mut queued = 0usize;
        while self.lod_request_cursor < self.lod_ring_offsets_cache.len() && queued < request_budget
        {
            let (dx, dz) = self.lod_ring_offsets_cache[self.lod_request_cursor];
            self.lod_request_cursor += 1;
            let coord = LodColumnCoord {
                x: center_chunk_x + dx,
                z: center_chunk_z + dz,
            };
            if self.request_lod_column_generation(coord, preset) {
                queued += 1;
            }
        }
        self.frame_perf.far_lod_requests_queued += queued;
        perf_trace::instant_event(
            "state.update_lod_streaming",
            &format!(
                "center=({}, {}) queued={} inflight_lod={} cursor={}/{}",
                center_chunk_x,
                center_chunk_z,
                queued,
                self.lod_chunkgen_inflight.len(),
                self.lod_request_cursor,
                self.lod_ring_offsets_cache.len()
            ),
        );
    }

    fn pump_pending_lod_mesh_uploads(&mut self) {
        self.frame_perf.chunkgen_lod_upload_backlog_before = self
            .frame_perf
            .chunkgen_lod_upload_backlog_before
            .max(self.pending_lod_mesh_uploads.len());
        if !self.far_lod_enabled {
            self.pending_lod_mesh_uploads.clear();
            self.frame_perf.chunkgen_lod_upload_backlog_after = self
                .frame_perf
                .chunkgen_lod_upload_backlog_after
                .max(self.pending_lod_mesh_uploads.len());
            return;
        }
        if self.frame_perf.elapsed_ms_since_frame_start() >= FAR_LOD_CPU_SKIP_AFTER_MS
            || self.mesh_upload_queue.len() > FAR_LOD_MESH_UPLOAD_BACKLOG_SKIP_THRESHOLD
        {
            self.frame_perf.chunkgen_lod_upload_backlog_after = self
                .frame_perf
                .chunkgen_lod_upload_backlog_after
                .max(self.pending_lod_mesh_uploads.len());
            return;
        }

        let mut uploads = 0usize;
        while uploads < FAR_LOD_GPU_UPLOADS_PER_FRAME {
            let Some((lod_column, lod_mesh_cpu)) = self.pending_lod_mesh_uploads.pop_front() else {
                break;
            };
            let upload_started = Instant::now();
            let existing = self.lod_meshes.remove(&lod_column);
            let (lod_mesh_gpu, stats) = upload_cpu_mesh_to_gpu_reuse(
                &self.device,
                &self.queue,
                "Far LOD",
                lod_mesh_cpu,
                existing,
            );
            self.frame_perf.gpu_far_lod_upload_ms +=
                upload_started.elapsed().as_secs_f32() * 1000.0;
            if let Some(lod_mesh_gpu) = lod_mesh_gpu {
                self.lod_meshes.insert(lod_column, lod_mesh_gpu);
            } else {
                self.lod_meshes.remove(&lod_column);
            }
            self.frame_perf.mesh_gpu_buffers_created += stats.created_buffers;
            self.frame_perf.mesh_gpu_reused_writes += stats.reused_buffer_writes;
            uploads += 1;
        }
        self.frame_perf.chunkgen_lod_mesh_uploads += uploads;
        self.frame_perf.chunkgen_lod_upload_backlog_after = self
            .frame_perf
            .chunkgen_lod_upload_backlog_after
            .max(self.pending_lod_mesh_uploads.len());
    }

    fn enqueue_generated_fluids_for_chunk(&mut self, coord: ChunkCoord) {
        if !self.is_sim_column_active(coord.x, coord.z) {
            return;
        }
        let mut to_schedule: Vec<(IVec3, FluidKind)> = Vec::with_capacity(64);
        if let Some(chunk) = self.world.get_chunk(coord) {
            let chunk_x0 = coord.x * CHUNK_SIZE;
            let chunk_y0 = coord.y * CHUNK_SIZE;
            let chunk_z0 = coord.z * CHUNK_SIZE;
            for local_y in 0..CHUNK_SIZE {
                for local_z in 0..CHUNK_SIZE {
                    for local_x in 0..CHUNK_SIZE {
                        let block = chunk.get(local_x, local_y, local_z);
                        let Some(kind) = fluid_kind(block) else {
                            continue;
                        };
                        let data = chunk.get_data(local_x, local_y, local_z);
                        let pos =
                            IVec3::new(chunk_x0 + local_x, chunk_y0 + local_y, chunk_z0 + local_z);
                        let is_source = fluid_data_is_source_origin(data);
                        let bootstrap = Self::generated_fluid_cell_needs_bootstrap(
                            &self.world,
                            pos,
                            kind,
                            is_source,
                        );
                        if !bootstrap {
                            continue;
                        }
                        to_schedule.push((pos, kind));
                    }
                }
            }
        }

        let _ = self.with_fluids_system(|fluids| {
            for (pos, kind) in to_schedule {
                let delay = fluids.tick_rate_for_kind(kind);
                fluids.schedule_fluid_update(pos, kind, delay);
            }
        });
    }

    fn enqueue_generated_fluids_from_bootstrap_cells(
        &mut self,
        cells: &[GeneratedFluidBootstrapCell],
    ) {
        let _ = self.with_fluids_system(|fluids| {
            for cell in cells {
                let delay = fluids.tick_rate_for_kind(cell.kind);
                fluids.schedule_fluid_update(cell.pos, cell.kind, delay);
            }
        });
    }

    fn generated_fluid_cell_needs_bootstrap(
        world: &World,
        pos: IVec3,
        kind: FluidKind,
        is_source: bool,
    ) -> bool {
        let below = pos + IVec3::new(0, -1, 0);
        if World::in_bounds(below) && fluid_kind(world.get_block(below)) != Some(kind) {
            return true;
        }

        for dir in [
            IVec3::new(1, 0, 0),
            IVec3::new(-1, 0, 0),
            IVec3::new(0, 0, 1),
            IVec3::new(0, 0, -1),
        ] {
            let neighbor = pos + dir;
            if !World::in_bounds(neighbor) {
                continue;
            }
            if fluid_kind(world.get_block(neighbor)) != Some(kind) {
                return true;
            }
        }

        // Keep source interiors dormant; non-source interiors can still wake if exposed.
        if is_source {
            return false;
        }

        // For non-source cells, wake up when vertical context differs (e.g. falling columns).
        let above = pos + IVec3::new(0, 1, 0);
        World::in_bounds(above) && fluid_kind(world.get_block(above)) != Some(kind)
    }

    pub(crate) fn ensure_column_loaded_or_generated(&mut self, world_x: i32, world_z: i32) {
        let cx = world_x.div_euclid(CHUNK_SIZE);
        let cz = world_z.div_euclid(CHUNK_SIZE);
        if !self.try_load_saved_column_if_present(cx, cz) {
            let min_chunk_y = self.world.min_chunk_y();
            let max_chunk_y = self.world.max_chunk_y();
            let mut spins = 0usize;
            while (min_chunk_y..=max_chunk_y).any(|y| {
                let coord = ChunkCoord { x: cx, y, z: cz };
                self.world.get_chunk(coord).is_none() && !self.generated_empty_chunks.contains(&coord)
            }) {
                let preset = self.worldgen_preset;
                for y in min_chunk_y..=max_chunk_y {
                    let coord = ChunkCoord { x: cx, y, z: cz };
                    if self.world.get_chunk(coord).is_some()
                        || self.generated_empty_chunks.contains(&coord)
                    {
                        continue;
                    }
                    let _ = self.request_chunk_generation(coord, preset);
                }
                self.pump_chunkgen_results();
                spins = spins.saturating_add(1);
                if spins > 512 {
                    break;
                }
                std::thread::yield_now();
            }
            self.mark_column_dirty(cx, cz);
        }
    }

    fn pump_chunkgen_results(&mut self) {
        let _trace_scope = perf_trace::scope("state.pump_chunkgen_results");
        let started = Instant::now();
        let backlog_before = self.chunkgen_results.len();
        self.frame_perf.chunkgen_results_backlog_before = self
            .frame_perf
            .chunkgen_results_backlog_before
            .max(backlog_before);
        self.frame_perf.chunkgen_lod_upload_backlog_before = self
            .frame_perf
            .chunkgen_lod_upload_backlog_before
            .max(self.pending_lod_mesh_uploads.len());

        #[derive(Default)]
        struct ChunkApplyProfileAgg {
            applied_chunks: usize,
            total_chunk_ms: f32,
            max_chunk_ms: f32,
            slowest_coord: Option<ChunkCoord>,
            total_insert_ms: f32,
            total_heightmap_ms: f32,
            total_lighting_hooks_ms: f32,
            total_mark_mesh_dirty_ms: f32,
            total_fluid_bootstrap_ms: f32,
            total_meshing_sync_ms: f32,
            total_gpu_upload_sync_ms: f32,
        }

        let mut drained = 0usize;
        let mut chunk_topology_changed = false;
        let mut stale_discarded = 0usize;
        let mut apply_prof = ChunkApplyProfileAgg::default();
        let mut budget_hit = false;
        let chunkgen_apply_budget_ms = self.stream_budget_for_stage_ms(
            STREAM_CHUNKGEN_APPLY_BUDGET_MS,
            STREAM_BURST_CHUNKGEN_APPLY_BUDGET_MS,
        );
        self.frame_perf.stream_chunkgen_apply_budget_ms = self
            .frame_perf
            .stream_chunkgen_apply_budget_ms
            .max(chunkgen_apply_budget_ms);
        let apply_loop_started = Instant::now();
        while drained < STREAM_STAGE_LOOP_SAFETY_CAP {
            if chunkgen_apply_budget_ms <= 0.0 {
                break;
            }
            if drained > 0
                && apply_loop_started.elapsed().as_secs_f32() * 1000.0 >= chunkgen_apply_budget_ms
            {
                budget_hit = true;
                break;
            }
            let Ok(result) = self.chunkgen_results.try_recv() else {
                break;
            };
            drained += 1;

            if result.epoch != self.chunkgen_stream_epoch {
                if let Some(lod_column) = result.lod_column {
                    self.lod_chunkgen_inflight.remove(&lod_column);
                }
                self.chunkgen_inflight.remove(&result.coord);
                stale_discarded += 1;
                continue;
            }

            if let Some(lod_column) = result.lod_column {
                self.lod_chunkgen_inflight.remove(&lod_column);
                self.frame_perf.chunkgen_lod_results_applied += 1;
                if let Some(lod_mesh_cpu) = result.lod_mesh {
                    if self.far_lod_enabled {
                        self.pending_lod_mesh_uploads
                            .push_back((lod_column, lod_mesh_cpu));
                        self.frame_perf.chunkgen_lod_mesh_results_enqueued += 1;
                    } else {
                        self.lod_meshes.remove(&lod_column);
                    }
                } else {
                    self.pending_lod_mesh_uploads
                        .retain(|(coord, _)| *coord != lod_column);
                    self.lod_meshes.remove(&lod_column);
                }
            }

            let Some(mut chunk) = result.chunk else {
                continue;
            };

            let chunk_apply_started = Instant::now();
            let coord = result.coord;
            self.chunkgen_inflight.remove(&result.coord);
            let was_mesh_present = self.chunk_meshes.contains_key(&coord);
            self.frame_perf.chunkgen_chunks_applied += 1;
            let mut insert_ms = 0.0f32;
            let heightmap_ms = 0.0f32;
            let lighting_hooks_ms = 0.0f32;
            let mut mark_mesh_dirty_ms = 0.0f32;
            let mut fluid_bootstrap_ms = 0.0f32;
            let meshing_sync_ms = 0.0f32;
            let gpu_upload_sync_ms = 0.0f32;

            if chunk.is_empty() {
                self.frame_perf.chunkgen_empty_chunks_applied += 1;
                let insert_started = Instant::now();
                let removed_chunk = self.world.chunks.remove(&coord).is_some();
                self.full_solid_mesh_skip_set.remove(&coord);
                insert_ms += insert_started.elapsed().as_secs_f32() * 1000.0;
                if removed_chunk {
                    chunk_topology_changed = true;
                    let mark_started = Instant::now();
                    self.mark_column_dirty_for_chunk(coord);
                    mark_mesh_dirty_ms += mark_started.elapsed().as_secs_f32() * 1000.0;
                }
                self.generated_empty_chunks.insert(coord);
                // Intentionally defer lighting recomputation to the async rebuild pipeline.
                if removed_chunk {
                    let mark_started = Instant::now();
                    if was_mesh_present {
                        self.chunk_meshes.remove(&coord);
                    }
                    self.mark_neighbors_dirty_if_meshed(coord);
                    self.mark_skipped_full_solid_neighbors_dirty(coord);
                    mark_mesh_dirty_ms += mark_started.elapsed().as_secs_f32() * 1000.0;
                }
            } else {
                chunk_topology_changed = true;
                let insert_started = Instant::now();
                self.generated_empty_chunks.remove(&coord);
                self.full_solid_mesh_skip_set.remove(&coord);
                self.seed_pending_lighting_for_chunk(coord, &mut chunk);
                self.world.chunks.insert(coord, chunk);
                insert_ms += insert_started.elapsed().as_secs_f32() * 1000.0;

                let mark_started = Instant::now();
                self.mark_column_dirty_for_chunk(coord);
                mark_mesh_dirty_ms += mark_started.elapsed().as_secs_f32() * 1000.0;

                // Intentionally defer all incremental lighting work to the async rebuild pipeline.

                let fluids_started = Instant::now();
                if self.is_sim_column_active(coord.x, coord.z) {
                    self.enqueue_generated_fluids_from_bootstrap_cells(&result.fluid_bootstrap);
                }
                fluid_bootstrap_ms += fluids_started.elapsed().as_secs_f32() * 1000.0;

                let mark_started = Instant::now();
                self.mark_chunk_dirty(coord);
                self.mark_neighbors_dirty_if_meshed(coord);
                self.mark_skipped_full_solid_neighbors_dirty(coord);
                mark_mesh_dirty_ms += mark_started.elapsed().as_secs_f32() * 1000.0;
            }

            perf_trace::record_duration_ms(
                "apply_chunk.insert_into_world",
                insert_ms,
                &format!("coord=({}, {}, {})", coord.x, coord.y, coord.z),
            );
            // Heightmap updates are implicit in current world/chunk path (no explicit separate step).
            perf_trace::record_duration_ms(
                "apply_chunk.update_heightmap",
                heightmap_ms,
                &format!(
                    "coord=({}, {}, {}) explicit=false",
                    coord.x, coord.y, coord.z
                ),
            );
            perf_trace::record_duration_ms(
                "apply_chunk.update_lighting_hooks",
                lighting_hooks_ms,
                &format!("coord=({}, {}, {})", coord.x, coord.y, coord.z),
            );
            perf_trace::record_duration_ms(
                "apply_chunk.mark_mesh_dirty",
                mark_mesh_dirty_ms,
                &format!("coord=({}, {}, {})", coord.x, coord.y, coord.z),
            );
            perf_trace::record_duration_ms(
                "apply_chunk.enqueue_generated_fluids",
                fluid_bootstrap_ms,
                &format!("coord=({}, {}, {})", coord.x, coord.y, coord.z),
            );
            perf_trace::record_duration_ms(
                "apply_chunk.any_meshing_sync",
                meshing_sync_ms,
                &format!(
                    "coord=({}, {}, {}) happened=false",
                    coord.x, coord.y, coord.z
                ),
            );
            perf_trace::record_duration_ms(
                "apply_chunk.any_gpu_upload_sync",
                gpu_upload_sync_ms,
                &format!(
                    "coord=({}, {}, {}) happened=false",
                    coord.x, coord.y, coord.z
                ),
            );

            let chunk_total_ms = chunk_apply_started.elapsed().as_secs_f32() * 1000.0;
            perf_trace::record_duration_ms(
                "apply_chunk.total",
                chunk_total_ms,
                &format!("coord=({}, {}, {})", coord.x, coord.y, coord.z),
            );
            apply_prof.applied_chunks += 1;
            apply_prof.total_chunk_ms += chunk_total_ms;
            apply_prof.total_insert_ms += insert_ms;
            apply_prof.total_heightmap_ms += heightmap_ms;
            apply_prof.total_lighting_hooks_ms += lighting_hooks_ms;
            apply_prof.total_mark_mesh_dirty_ms += mark_mesh_dirty_ms;
            apply_prof.total_fluid_bootstrap_ms += fluid_bootstrap_ms;
            apply_prof.total_meshing_sync_ms += meshing_sync_ms;
            apply_prof.total_gpu_upload_sync_ms += gpu_upload_sync_ms;
            if chunk_total_ms >= apply_prof.max_chunk_ms {
                apply_prof.max_chunk_ms = chunk_total_ms;
                apply_prof.slowest_coord = Some(coord);
            }
        }
        if drained >= STREAM_STAGE_LOOP_SAFETY_CAP {
            budget_hit = true;
        }
        if self.world_generation_task.is_some() && drained > 0 {
            self.refresh_worldgen_warmup_tracking();
        }
        self.frame_perf.stream_chunkgen_apply_used_ms +=
            apply_loop_started.elapsed().as_secs_f32() * 1000.0;
        self.frame_perf.chunkgen_results_drained += drained;
        self.frame_perf.chunkgen_results_backlog_after = self
            .frame_perf
            .chunkgen_results_backlog_after
            .max(self.chunkgen_results.len());
        if chunk_topology_changed {
            self.request_lighting_rebuild();
        }
        self.pump_pending_lod_mesh_uploads();
        let dur_ms = started.elapsed().as_secs_f32() * 1000.0;
        self.frame_perf.pump_chunkgen_results_ms += dur_ms;
        perf_trace::record_duration_ms(
            "state.pump_chunkgen_results",
            dur_ms,
            &format!(
                "drained={} chunk_applied={} lod_results={} stale_discarded={} budget_hit={} lod_upload_q={} apply_avg_ms={:.2} apply_max_ms={:.2} slowest={:?}",
                drained,
                self.frame_perf.chunkgen_chunks_applied,
                self.frame_perf.chunkgen_lod_results_applied,
                stale_discarded,
                budget_hit,
                self.pending_lod_mesh_uploads.len(),
                if apply_prof.applied_chunks > 0 {
                    apply_prof.total_chunk_ms / apply_prof.applied_chunks as f32
                } else {
                    0.0
                },
                apply_prof.max_chunk_ms,
                apply_prof.slowest_coord
            ),
        );
        if dur_ms >= CHUNKGEN_APPLY_LOG_THRESHOLD_MS
            || apply_prof.max_chunk_ms >= CHUNKGEN_APPLY_LOG_THRESHOLD_MS
        {
            eprintln!(
                "[perf][chunkgen_apply] frame_ms={:.1} drained={} applied={} stale={} budget_hit={} avg_ms={:.2} max_ms={:.2} slowest={:?} sub(insert={:.1} heightmap={:.1} lighting_hooks={:.1} mesh_dirty={:.1} fluid_bootstrap={:.1} meshing_sync={:.1} gpu_sync={:.1}) inflight={} job_q={} result_q={}",
                dur_ms,
                drained,
                apply_prof.applied_chunks,
                stale_discarded,
                budget_hit,
                if apply_prof.applied_chunks > 0 {
                    apply_prof.total_chunk_ms / apply_prof.applied_chunks as f32
                } else {
                    0.0
                },
                apply_prof.max_chunk_ms,
                apply_prof.slowest_coord,
                apply_prof.total_insert_ms,
                apply_prof.total_heightmap_ms,
                apply_prof.total_lighting_hooks_ms,
                apply_prof.total_mark_mesh_dirty_ms,
                apply_prof.total_fluid_bootstrap_ms,
                apply_prof.total_meshing_sync_ms,
                apply_prof.total_gpu_upload_sync_ms,
                self.chunkgen_inflight.len(),
                self.chunkgen_jobs.len(),
                self.chunkgen_results.len(),
            );
        }
    }

    fn enqueue_pending_unload_column(&mut self, cx: i32, cz: i32) {
        if self.pending_unload_columns.insert((cx, cz)) {
            self.pending_unload_queue.push_back((cx, cz));
        }
    }

    fn refresh_unload_scan_keys_if_needed(&mut self) {
        let needs_refresh = self.unload_scan_keys.is_empty()
            || self.unload_scan_cursor >= self.unload_scan_keys.len()
            || self
                .frame_index
                .saturating_sub(self.unload_scan_last_refresh_frame)
                >= STREAM_UNLOAD_SCAN_REFRESH_PERIOD_FRAMES;
        if !needs_refresh {
            return;
        }
        self.unload_scan_keys.clear();
        self.unload_scan_keys
            .extend(self.world.chunks.keys().copied());
        self.unload_scan_keys
            .extend(self.generated_empty_chunks.iter().copied());
        self.unload_scan_cursor = 0;
        self.unload_scan_last_refresh_frame = self.frame_index;
    }

    fn stage_pending_unload_column_chunks(&mut self, cx: i32, cz: i32) {
        self.pending_unload_columns.remove(&(cx, cz));
        for cy in self.world.min_chunk_y()..=self.world.max_chunk_y() {
            self.pending_unload_chunks.push_back(ChunkCoord {
                x: cx,
                y: cy,
                z: cz,
            });
        }
    }

    fn prune_chunk_meshes_outside_view(&mut self, camera: &Camera) {
        let (chunk_x, chunk_z, radius) = if let Some(task) = self.world_generation_task.as_ref() {
            (
                task.warmup_center_x,
                task.warmup_center_z,
                CHUNK_WARMUP_RADIUS,
            )
        } else {
            let (x, _, z) = Self::camera_chunk_coords(camera);
            (x, z, self.near_mesh_radius_chunks())
        };
        let target = (chunk_x, chunk_z, radius);
        let target_changed = self.prune_mesh_last_target != Some(target);
        self.prune_mesh_last_target = Some(target);

        let prune_active = self.prune_mesh_cursor < self.prune_mesh_keys.len();
        let periodic_due = self
            .frame_index
            .saturating_sub(self.prune_mesh_last_scan_frame)
            >= STREAM_PRUNE_MESH_PERIOD_FRAMES;
        if !prune_active {
            if !target_changed && !periodic_due {
                return;
            }
            self.prune_mesh_keys.clear();
            self.prune_mesh_keys
                .extend(self.chunk_meshes.keys().copied());
            self.prune_mesh_cursor = 0;
            self.prune_mesh_last_scan_frame = self.frame_index;
        }
        if self.prune_mesh_cursor >= self.prune_mesh_keys.len() {
            return;
        }

        let started = Instant::now();
        let mut processed = 0usize;
        while self.prune_mesh_cursor < self.prune_mesh_keys.len()
            && processed < STREAM_PRUNE_MESHES_PER_FRAME
            && started.elapsed().as_secs_f32() * 1000.0 < STREAM_PRUNE_MESH_BUDGET_MS
        {
            let coord = self.prune_mesh_keys[self.prune_mesh_cursor];
            self.prune_mesh_cursor += 1;
            processed += 1;
            if !Self::is_chunk_within_radius_xz(coord, chunk_x, chunk_z, radius) {
                self.chunk_meshes.remove(&coord);
            }
        }
        if self.prune_mesh_cursor >= self.prune_mesh_keys.len() {
            self.prune_mesh_keys.clear();
            self.prune_mesh_cursor = 0;
        }
    }

    fn unload_chunks_outside_keep_radius(&mut self, camera: &Camera) {
        let (chunk_x, chunk_z, base_radius) =
            if let Some(task) = self.world_generation_task.as_ref() {
                (
                    task.warmup_center_x,
                    task.warmup_center_z,
                    CHUNK_WARMUP_RADIUS,
                )
            } else {
                let (x, _, z) = Self::camera_chunk_coords(camera);
                (x, z, self.near_mesh_radius_chunks())
            };
        let keep_radius = base_radius + STREAM_KEEP_RADIUS_MARGIN_CHUNKS;
        let keep_radius_sq = keep_radius * keep_radius;
        let ticking_keep_areas: Vec<TickingArea> = self
            .world
            .ticking_areas()
            .iter()
            .copied()
            .filter(Self::ticking_area_requires_loading)
            .collect();
        let in_keep_radius = |coord: ChunkCoord| {
            let dx = coord.x - chunk_x;
            let dz = coord.z - chunk_z;
            dx * dx + dz * dz <= keep_radius_sq
        };
        let column_in_keep = |cx: i32, cz: i32| {
            let dx = cx - chunk_x;
            let dz = cz - chunk_z;
            if dx * dx + dz * dz <= keep_radius_sq {
                return true;
            }
            ticking_keep_areas
                .iter()
                .any(|area| Self::column_in_ticking_area(area, cx, cz))
        };

        self.refresh_unload_scan_keys_if_needed();
        let mut scanned = 0usize;
        while self.unload_scan_cursor < self.unload_scan_keys.len()
            && scanned < STREAM_UNLOAD_SCAN_COORDS_PER_FRAME
        {
            let coord = self.unload_scan_keys[self.unload_scan_cursor];
            self.unload_scan_cursor += 1;
            scanned += 1;
            if !in_keep_radius(coord) {
                self.enqueue_pending_unload_column(coord.x, coord.z);
            }
        }
        if self.unload_scan_cursor >= self.unload_scan_keys.len() {
            self.unload_scan_keys.clear();
            self.unload_scan_cursor = 0;
        }

        let started = Instant::now();
        let mut queued_save_columns = 0usize;
        let mut staged_columns = 0usize;
        let mut removed_chunks = 0usize;
        let mut removed_any_loaded_chunk = false;

        let mut column_passes = 0usize;
        while self.pending_unload_chunks.is_empty()
            && column_passes < self.pending_unload_queue.len().max(1)
            && started.elapsed().as_secs_f32() * 1000.0 < STREAM_EVICT_BUDGET_MS
        {
            let Some((cx, cz)) = self.pending_unload_queue.pop_front() else {
                break;
            };
            column_passes += 1;
            if !self.pending_unload_columns.contains(&(cx, cz)) {
                continue;
            }
            if column_in_keep(cx, cz) {
                self.pending_unload_columns.remove(&(cx, cz));
                continue;
            }
            if self.save_columns_inflight.contains_key(&(cx, cz)) {
                self.pending_unload_queue.push_back((cx, cz));
                continue;
            }
            if self.dirty_columns.contains(&(cx, cz)) {
                if queued_save_columns >= STREAM_EVICT_SAVE_COLUMNS_PER_FRAME {
                    self.pending_unload_queue.push_back((cx, cz));
                    break;
                }
                let queued = self.queue_save_column_if_dirty(cx, cz);
                self.pending_unload_queue.push_back((cx, cz));
                if queued {
                    queued_save_columns += 1;
                }
                continue;
            }
            if staged_columns >= STREAM_EVICT_COLUMNS_PER_FRAME {
                self.pending_unload_queue.push_back((cx, cz));
                break;
            }
            self.stage_pending_unload_column_chunks(cx, cz);
            staged_columns += 1;
        }

        for _ in 0..STREAM_EVICT_CHUNKS_PER_FRAME {
            if started.elapsed().as_secs_f32() * 1000.0 >= STREAM_EVICT_BUDGET_MS {
                break;
            }
            let Some(coord) = self.pending_unload_chunks.pop_front() else {
                break;
            };
            if in_keep_radius(coord) {
                continue;
            }

            let removed_chunk = self.world.chunks.remove(&coord).is_some();
            let had_mesh = self.chunk_meshes.remove(&coord).is_some();
            self.full_solid_mesh_skip_set.remove(&coord);
            self.generated_empty_chunks.remove(&coord);
            self.chunkgen_inflight.remove(&coord);
            self.inflight.remove(&coord);
            self.dirty_chunk_set.remove(&coord);
            self.fluid_mesh_coalesce.remove(&coord);
            self.fluid_dirty_due_to_fluids.remove(&coord);
            self.saved_revision.remove(&coord);

            if removed_chunk || had_mesh {
                for neighbor in Self::neighbor_chunk_coords(coord) {
                    self.mark_chunk_dirty(neighbor);
                }
            }
            if removed_chunk {
                removed_chunks += 1;
                removed_any_loaded_chunk = true;
            }
        }

        if removed_any_loaded_chunk {
            // Do not run incremental lighting hooks during eviction; async rebuild is coalesced/debounced.
            self.request_lighting_rebuild();
        }

        if removed_chunks > 0
            || self
                .frame_index
                .is_multiple_of(STREAM_UNLOAD_SCAN_REFRESH_PERIOD_FRAMES)
        {
            let _ = self.with_fluids_system(|fluids| {
                fluids.prune_outside_keep_radius_xz(chunk_x, chunk_z, keep_radius);
            });
        }
    }

    fn queue_missing_meshes_in_radius(
        &mut self,
        center_chunk_x: i32,
        center_chunk_y: i32,
        center_chunk_z: i32,
    ) {
        if self.radius_offsets_cache.is_empty() {
            return;
        }

        let min_chunk_y = self.world.min_chunk_y();
        let max_chunk_y = self.world.max_chunk_y();

        for idx in 0..self.radius_offsets_cache.len() {
            let (dx, dz) = self.radius_offsets_cache[idx];
            let x = center_chunk_x + dx;
            let z = center_chunk_z + dz;
            let chunk_y_order =
                self.column_stream_chunk_y_order(x, center_chunk_y, z, min_chunk_y, max_chunk_y);
            for cy in chunk_y_order {
                let coord = ChunkCoord { x, y: cy, z };
                if self.world.get_chunk(coord).is_none()
                    || self.chunk_meshes.contains_key(&coord)
                    || self.inflight.contains_key(&coord)
                    || self.dirty_chunk_set.contains(&coord)
                {
                    continue;
                }
                self.mark_chunk_dirty(coord);
            }
        }
    }

    fn reprioritize_dirty_chunk_queue(
        &mut self,
        center_chunk_x: i32,
        center_chunk_y: i32,
        center_chunk_z: i32,
        frustum: &Frustum,
        camera: &Camera,
    ) {
        if self.dirty_chunk_queue.len() < STREAM_PRIORITY_REORDER_MIN_BACKLOG {
            return;
        }
        self.refresh_stream_horizon_map(camera, frustum);
        let forward_xz = Self::camera_forward_xz(camera);
        let drained: Vec<ChunkCoord> = self.dirty_chunk_queue.drain(..).collect();
        let mut staged = Vec::with_capacity(drained.len());
        for (idx, coord) in drained.into_iter().enumerate() {
            let key = self.stream_priority_key_for_chunk(
                coord,
                center_chunk_x,
                center_chunk_y,
                center_chunk_z,
                Some(camera),
                Some(frustum),
                forward_xz,
                idx.min(u32::MAX as usize) as u32,
            );
            staged.push((coord, key));
        }
        staged.sort_by(|a, b| {
            a.1.cmp_ascending(&b.1)
                .then_with(|| a.0.x.cmp(&b.0.x))
                .then_with(|| a.0.y.cmp(&b.0.y))
                .then_with(|| a.0.z.cmp(&b.0.z))
        });
        self.dirty_chunk_queue
            .extend(staged.into_iter().map(|(coord, _)| coord));
    }

    fn reprioritize_mesh_upload_queue(&mut self, camera: &Camera) {
        if self.mesh_upload_queue.len() < STREAM_PRIORITY_REORDER_MIN_BACKLOG {
            return;
        }
        let (center_chunk_x, center_chunk_y, center_chunk_z) = Self::camera_chunk_coords(camera);
        let frustum = self.build_frustum_for_camera(camera);
        self.refresh_stream_horizon_map(camera, &frustum);
        let forward_xz = Self::camera_forward_xz(camera);

        let drained: Vec<MeshResult> = self.mesh_upload_queue.drain(..).collect();
        let mut staged = Vec::with_capacity(drained.len());
        for (idx, result) in drained.into_iter().enumerate() {
            let key = self.stream_priority_key_for_chunk(
                result.coord,
                center_chunk_x,
                center_chunk_y,
                center_chunk_z,
                Some(camera),
                Some(&frustum),
                forward_xz,
                idx.min(u32::MAX as usize) as u32,
            );
            staged.push((result, key));
        }
        staged.sort_by(|a, b| {
            a.1.cmp_ascending(&b.1)
                .then_with(|| a.0.coord.x.cmp(&b.0.coord.x))
                .then_with(|| a.0.coord.y.cmp(&b.0.coord.y))
                .then_with(|| a.0.coord.z.cmp(&b.0.coord.z))
        });
        self.mesh_upload_queue
            .extend(staged.into_iter().map(|(result, _)| result));
    }

    fn enqueue_chunks_within_view(&mut self, camera: &Camera) {
        let _trace_scope = perf_trace::scope("state.enqueue_chunks_within_view");
        let started = Instant::now();
        let (chunk_x, chunk_y, chunk_z, radius) =
            if let Some(task) = self.world_generation_task.as_ref() {
                let center_chunk_y = (self.world.sea_level() + 16).div_euclid(CHUNK_SIZE);
                (
                    task.warmup_center_x,
                    center_chunk_y,
                    task.warmup_center_z,
                    CHUNK_WARMUP_RADIUS,
                )
            } else {
                let (x, y, z) = Self::camera_chunk_coords(camera);
                (x, y, z, self.near_mesh_radius_chunks())
            };
        let center = (chunk_x, chunk_y, chunk_z);
        let center_changed = self.last_stream_center != Some(center);
        let radius_changed = self.last_stream_radius != radius;
        if center_changed || radius_changed {
            if radius_changed {
                self.rebuild_radius_offsets_cache(radius);
            }
            self.chunkgen_stream_epoch = self.chunkgen_stream_epoch.wrapping_add(1);
            self.clear_chunkgen_pending_requests();
            self.saved_load_request_cursor = 0;
            self.queue_missing_meshes_in_radius(chunk_x, chunk_y, chunk_z);
            self.last_stream_center = Some(center);
            self.last_stream_radius = radius;
        }
        self.ensure_chunks_in_radius(chunk_x, chunk_y, chunk_z, radius, Some(camera));
        let saved_load_queued = self.queue_saved_column_load_requests_for_radius(
            chunk_x,
            chunk_z,
            SAVED_LOAD_REQUESTS_PER_FRAME,
        );
        self.report_saved_column_request_rate_if_due();
        if self.far_lod_enabled && self.world_generation_task.is_none() {
            let (lod_cx, lod_cz) =
                self.stable_lod_stream_center_from_camera(camera, chunk_x, chunk_z);
            self.update_lod_streaming(lod_cx, lod_cz);
        }
        let dur_ms = started.elapsed().as_secs_f32() * 1000.0;
        self.frame_perf.enqueue_chunks_within_view_ms += dur_ms;
        perf_trace::record_duration_ms(
            "state.enqueue_chunks_within_view",
            dur_ms,
            &format!(
                "center_changed={} radius_changed={} saved_load_q={} far_lod={} inflight_chunkgen={} inflight_mesh={} saved_load_inflight={} saved_load_q_backlog={} saved_load_ack_backlog={}",
                center_changed,
                radius_changed,
                saved_load_queued,
                self.far_lod_enabled,
                self.chunkgen_inflight.len(),
                self.inflight.len(),
                self.saved_column_loads_inflight.len(),
                self.storage_loader.queue_len_approx(),
                self.storage_loader.result_queue_len_approx()
            ),
        );
    }

    pub(crate) fn set_render_distance(&mut self, render_distance: RenderDistance, camera: &Camera) {
        self.render_distance = render_distance;
        self.view_distance_chunks = self.render_distance.view_distance_chunks();
        let prev_sim_chunks = self.simulation_distance_chunks;
        self.simulation_distance_chunks = self
            .simulation_distance
            .distance_chunks()
            .min(self.view_distance_chunks);
        if self.simulation_distance_chunks != prev_sim_chunks {
            self.last_sim_center = None;
            self.update_simulation_activation(camera);
        }
        let (chunk_x, _, chunk_z) = Self::camera_chunk_coords(camera);
        let radius_sq = self.view_distance_chunks * self.view_distance_chunks;
        self.dirty_chunk_queue.retain(|coord| {
            let dx = coord.x - chunk_x;
            let dz = coord.z - chunk_z;
            dx * dx + dz * dz <= radius_sq
        });
        self.dirty_chunk_set.retain(|coord| {
            let dx = coord.x - chunk_x;
            let dz = coord.z - chunk_z;
            dx * dx + dz * dz <= radius_sq
        });
        self.deferred_mesh_until_frame.retain(|coord, _| {
            let dx = coord.x - chunk_x;
            let dz = coord.z - chunk_z;
            dx * dx + dz * dz <= radius_sq
        });
        self.inflight.retain(|coord, _| {
            let dx = coord.x - chunk_x;
            let dz = coord.z - chunk_z;
            dx * dx + dz * dz <= radius_sq
        });
        self.chunkgen_inflight.retain(|coord| {
            let dx = coord.x - chunk_x;
            let dz = coord.z - chunk_z;
            dx * dx + dz * dz <= radius_sq
        });
        self.prune_chunk_meshes_outside_view(camera);
        self.enqueue_chunks_within_view(camera);
    }

    pub(crate) fn is_sim_column_active(&self, x: i32, z: i32) -> bool {
        self.active_sim_columns.contains(&ColumnCoord { x, z })
    }

    pub(crate) fn register_sim_system(&mut self, sys: Box<dyn SimSystem>) {
        if self.fluids_system_index.is_none() && sys.name() == "fluids" {
            self.fluids_system_index = Some(self.sim_systems.len());
        }
        if self.block_entities_system_index.is_none() && sys.name() == "block_entities" {
            self.block_entities_system_index = Some(self.sim_systems.len());
        }
        if self.random_ticks_system_index.is_none() && sys.name() == "random_ticks" {
            self.random_ticks_system_index = Some(self.sim_systems.len());
        }
        self.sim_systems.push(sys);
    }

    fn with_fluids_system_mut<R>(
        &mut self,
        f: impl FnOnce(&mut FluidsSystem, &mut World) -> R,
    ) -> Option<R> {
        let idx = self.fluids_system_index?;
        if idx >= self.sim_systems.len() {
            return None;
        }
        let (_, right) = self.sim_systems.split_at_mut(idx);
        let sys = right
            .first_mut()?
            .as_any_mut()
            .downcast_mut::<FluidsSystem>()?;
        Some(f(sys, &mut self.world))
    }

    fn with_fluids_system<R>(&mut self, f: impl FnOnce(&mut FluidsSystem) -> R) -> Option<R> {
        let idx = self.fluids_system_index?;
        let sys = self.sim_systems.get_mut(idx)?;
        let fluids = sys.as_any_mut().downcast_mut::<FluidsSystem>()?;
        Some(f(fluids))
    }

    fn with_block_entities_system_mut<R>(
        &mut self,
        f: impl FnOnce(&mut BlockEntitiesSystem, &mut World) -> R,
    ) -> Option<R> {
        let idx = self.block_entities_system_index?;
        if idx >= self.sim_systems.len() {
            return None;
        }
        let (_, right) = self.sim_systems.split_at_mut(idx);
        let sys = right
            .first_mut()?
            .as_any_mut()
            .downcast_mut::<BlockEntitiesSystem>()?;
        Some(f(sys, &mut self.world))
    }

    fn fluids_bootstrap_queue_len(&self) -> Option<usize> {
        let idx = self.fluids_system_index?;
        let sys = self.sim_systems.get(idx)?;
        let fluids = sys.as_any().downcast_ref::<FluidsSystem>()?;
        Some(fluids.bootstrap_queue_len())
    }

    fn block_entities_debug_counts(&self) -> Option<(usize, usize)> {
        let idx = self.block_entities_system_index?;
        let sys = self.sim_systems.get(idx)?;
        let block_entities = sys.as_any().downcast_ref::<BlockEntitiesSystem>()?;
        Some(block_entities.debug_counts())
    }

    fn random_ticks_debug_counters(&self) -> Option<(u32, usize, usize)> {
        let idx = self.random_ticks_system_index?;
        let sys = self.sim_systems.get(idx)?;
        let random_ticks = sys.as_any().downcast_ref::<RandomTicksSystem>()?;
        Some(random_ticks.debug_counters())
    }

    pub(crate) fn create_ticking_area_radius(&mut self, cx: i32, cz: i32, radius: i32) -> u32 {
        let area = self.world.create_ticking_area(
            glam::IVec2::new(cx, cz),
            radius,
            TickingFlags::default(),
        );
        if let Err(err) = self.save_ticking_areas() {
            eprintln!("[ticking_areas] failed to persist after create: {err}");
        }
        self.last_sim_center = None;
        area.id
    }

    pub(crate) fn delete_nearest_ticking_area(&mut self, cx: i32, cz: i32) -> Option<u32> {
        let removed = self
            .world
            .remove_nearest_ticking_area(glam::IVec2::new(cx, cz))?;
        if let Err(err) = self.save_ticking_areas() {
            eprintln!("[ticking_areas] failed to persist after delete: {err}");
        }
        self.last_sim_center = None;
        Some(removed.id)
    }

    fn for_each_ticking_area_column(
        &self,
        area: &TickingArea,
        mut f: impl FnMut(ColumnCoord),
    ) {
        let radius = area.radius_chunks.max(0);
        let radius_sq = i64::from(radius) * i64::from(radius);
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                let dx64 = i64::from(dx);
                let dz64 = i64::from(dz);
                if dx64 * dx64 + dz64 * dz64 > radius_sq {
                    continue;
                }
                f(ColumnCoord {
                    x: area.center.x.saturating_add(dx),
                    z: area.center.y.saturating_add(dz),
                });
            }
        }
    }

    fn merge_ticking_areas_for_simulation(&self, cols: &mut HashSet<ColumnCoord>) {
        for area in self.world.ticking_areas() {
            if !area.flags.any_tick() {
                continue;
            }
            self.for_each_ticking_area_column(area, |col| {
                if self.column_has_loaded_data(col.x, col.z) {
                    cols.insert(col);
                }
            });
        }
    }

    fn ticking_area_requires_loading(area: &TickingArea) -> bool {
        area.flags.keep_loaded() || area.flags.any_tick()
    }

    fn column_in_ticking_area(area: &TickingArea, cx: i32, cz: i32) -> bool {
        let dx = i64::from(cx - area.center.x);
        let dz = i64::from(cz - area.center.y);
        let r = i64::from(area.radius_chunks.max(0));
        dx * dx + dz * dz <= r * r
    }

    fn column_has_loaded_data(&self, cx: i32, cz: i32) -> bool {
        for cy in self.world.min_chunk_y()..=self.world.max_chunk_y() {
            let coord = ChunkCoord { x: cx, y: cy, z: cz };
            if self.world.get_chunk(coord).is_some() || self.generated_empty_chunks.contains(&coord) {
                return true;
            }
        }
        false
    }

    pub(crate) fn update_simulation_activation(&mut self, camera: &Camera) {
        let (entered, exited) = self.recompute_active_sim_columns(camera);
        self.sim_columns_entered = entered;
        self.sim_columns_exited = exited;
    }

    fn recompute_active_sim_columns(
        &mut self,
        camera: &Camera,
    ) -> (Vec<ColumnCoord>, Vec<ColumnCoord>) {
        let (center_x, _, center_z) = Self::camera_chunk_coords(camera);
        let center = ColumnCoord {
            x: center_x,
            z: center_z,
        };
        if self.last_sim_center == Some(center) {
            return (Vec::new(), Vec::new());
        }

        let radius = self.simulation_distance_chunks.max(0);
        let radius_sq = radius * radius;
        let diameter = radius * 2 + 1;
        let mut next_active = HashSet::with_capacity((diameter * diameter) as usize);
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                if dx * dx + dz * dz > radius_sq {
                    continue;
                }
                next_active.insert(ColumnCoord {
                    x: center_x + dx,
                    z: center_z + dz,
                });
            }
        }
        self.merge_ticking_areas_for_simulation(&mut next_active);

        let entered = next_active
            .difference(&self.active_sim_columns)
            .copied()
            .collect::<Vec<_>>();
        let exited = self
            .active_sim_columns
            .difference(&next_active)
            .copied()
            .collect::<Vec<_>>();
        self.active_sim_columns = next_active;
        self.last_sim_center = Some(center);

        if !exited.is_empty() {
            let active_set = self.active_sim_columns.clone();
            let active_fn = |x: i32, z: i32| active_set.contains(&ColumnCoord { x, z });
            for system in self.sim_systems.iter_mut() {
                system.on_columns_exited(
                    &mut self.world,
                    &exited,
                    ActiveSimColumns {
                        is_active: &active_fn,
                    },
                );
            }
        }
        if !entered.is_empty() {
            let active_set = self.active_sim_columns.clone();
            let active_fn = |x: i32, z: i32| active_set.contains(&ColumnCoord { x, z });
            for system in self.sim_systems.iter_mut() {
                system.on_columns_entered(
                    &mut self.world,
                    &entered,
                    ActiveSimColumns {
                        is_active: &active_fn,
                    },
                );
            }
        }

        (entered, exited)
    }

    pub(crate) fn set_simulation_distance(&mut self, sd: SimulationDistance, camera: &Camera) {
        self.simulation_distance = sd;
        self.simulation_distance_chunks = self
            .simulation_distance
            .distance_chunks()
            .min(self.view_distance_chunks);
        self.last_sim_center = None;
        self.update_simulation_activation(camera);
    }

    pub(crate) fn is_generating_new_level(&self) -> bool {
        self.world_generation_task.is_some()
    }

    pub(crate) fn begin_new_world(&mut self, seed: u64, preset: WorldgenPreset) {
        if self.world_generation_task.is_some() {
            return;
        }
        if let Err(err) = self.storage_writer.flush() {
            eprintln!("[storage][save] flush before new level generation failed: {err}");
        }
        self.pump_storage_save_results();
        self.save_epoch = self.save_epoch.wrapping_add(1);

        let mut cfg = self.world.config();
        cfg.seed = seed;
        let (boundary_ocean, boundary_radius_chunks) =
            Self::boundary_ocean_defaults_for_preset(preset);
        cfg.boundary_ocean = boundary_ocean;
        cfg.boundary_radius_chunks = boundary_radius_chunks;
        self.world.configure_world_config(cfg, preset);
        self.rebuild_boundary_ocean_mesh();
        self.world.replace_ticking_areas(Vec::new());
        self.worldgen_preset = preset;
        let world_meta_path = Self::world_meta_path();
        let meta = Self::build_world_meta(&self.world, seed, preset);
        if let Err(err) = world_meta::save_atomic(&world_meta_path, &meta) {
            panic!(
                "failed to write {} after selecting new world seed/preset (seed={}, preset={:?}): {}",
                world_meta_path.display(),
                seed,
                preset,
                err
            );
        }
        let world_manifest_path = Self::world_manifest_path();
        let block_registry = BlockRegistry::new();
        let manifest = WorldManifest::build_from_registry(&block_registry);
        if let Err(err) = manifest.save(&world_manifest_path) {
            panic!(
                "failed to write {} after selecting new world seed/preset (seed={}, preset={:?}): {}",
                world_manifest_path.display(),
                seed,
                preset,
                err
            );
        }
        let id_mapper = IdMapper::from_manifest_and_registry(&manifest, &block_registry);
        self.world.set_manifest_and_mapper(manifest, id_mapper);
        if let Err(err) = self.save_ticking_areas() {
            eprintln!(
                "failed to write {} after selecting new world seed/preset: {}",
                Self::ticking_areas_path().display(),
                err
            );
        }
        self.world.chunks.clear();
        self.chunk_meshes.clear();
        self.full_solid_mesh_skip_set.clear();
        self.lod_meshes.clear();
        self.dirty_chunk_queue.clear();
        self.dirty_chunk_set.clear();
        self.deferred_mesh_until_frame.clear();
        self.inflight.clear();
        self.chunkgen_inflight.clear();
        self.lod_chunkgen_inflight.clear();
        self.generated_empty_chunks.clear();
        self.mesh_upload_queue.clear();
        while self.mesh_results.try_recv().is_ok() {}
        while self.chunkgen_results.try_recv().is_ok() {}
        while self.lighting_results.try_recv().is_ok() {}
        while self.storage_loader.try_recv_result().is_some() {}
        self.highlight_mesh = None;
        self.outline_target_block = None;
        self.particles.clear();
        let _ = self.with_fluids_system(|fluids| {
            fluids.reset();
        });
        let _ = self.with_block_entities_system_mut(|block_entities, world| {
            block_entities.reset(world);
        });
        self.fluid_accum = 0.0;
        self.sim_tick_index = 0;
        self.fluid_debug_counts = FluidDebugCounts::default();
        self.frame_index = 0;
        self.frustum_culled_chunks_total = 0;
        self.meshes_skipped_due_to_frustum = 0;
        self.fluid_mesh_coalesce.clear();
        self.fluid_dirty_due_to_fluids.clear();
        self.stream_last_camera_y = None;
        self.stream_recent_descend_frames = 0;
        self.stream_recent_mining_frames = 0;
        self.last_stream_center = None;
        self.last_stream_radius = -1;
        self.radius_offsets_cache.clear();
        self.last_lod_stream_center = None;
        self.lod_ring_offsets_cache.clear();
        self.lod_request_cursor = 0;
        self.chunkgen_pending_set.clear();
        self.chunkgen_pending_enqueued_frame.clear();
        self.chunkgen_pending_heap.clear();
        self.chunkgen_heap_valid_for_center = None;
        self.chunkgen_heap_valid_for_radius = -1;
        self.chunkgen_heap_last_rebuild_frame = 0;
        self.chunkgen_stream_epoch = self.chunkgen_stream_epoch.wrapping_add(1);
        self.lod_cached_near_radius = -1;
        self.lod_cached_far_radius = -1;
        self.lighting_job_inflight = false;
        self.lighting_request_pending = false;
        self.lighting_rebuild_not_before = None;
        self.pending_lighting_dirty_chunks.clear();
        self.pending_lighting_dirty_set.clear();
        self.saved_revision.clear();
        self.saved_column_missing_cache.clear();
        self.saved_column_loads_inflight.clear();
        self.saved_load_request_cursor = 0;
        self.saved_load_requests_in_window = 0;
        self.saved_load_requests_window_started = Instant::now();
        self.dirty_columns.clear();
        self.dirty_column_queue.clear();
        self.dirty_column_generation.clear();
        self.next_dirty_column_generation = 1;
        self.save_columns_inflight.clear();
        self.pending_unload_columns.clear();
        self.pending_unload_queue.clear();
        self.pending_unload_chunks.clear();
        self.unload_scan_keys.clear();
        self.unload_scan_cursor = 0;
        self.unload_scan_last_refresh_frame = 0;
        self.prune_mesh_keys.clear();
        self.prune_mesh_cursor = 0;
        self.prune_mesh_last_target = None;
        self.prune_mesh_last_scan_frame = 0;
        self.autosave_sweep_active = false;
        self.last_autosave_at = Instant::now();

        let warmup_center_x = 0;
        let warmup_center_z = 0;
        let center_chunk_y = (self.world.sea_level() + 16).div_euclid(CHUNK_SIZE);
        let warmup_columns = self.build_worldgen_warmup_columns(warmup_center_x, warmup_center_z);
        let warmup_coords =
            self.build_worldgen_warmup_coords(warmup_center_x, center_chunk_y, warmup_center_z);
        let warmup_target = warmup_coords.len().max(1);
        self.world_generation_task = Some(WorldGenerationTask {
            warmup_center_x,
            warmup_center_z,
            warmup_columns: warmup_columns.clone(),
            warmup_coords,
            warmup_target,
            warmup_column_target: warmup_columns.len().max(1),
            warmup_columns_generated_done: HashSet::new(),
            warmup_generated_done: HashSet::new(),
            warmup_meshed_done: HashSet::new(),
            pending_progress: None,
            last_reported_phase: String::new(),
            last_reported_progress_permille: u16::MAX,
            started_at: Instant::now(),
        });
        self.refresh_worldgen_warmup_tracking();

        let preferred_y = center_chunk_y;
        self.ensure_chunks_in_radius(
            warmup_center_x,
            preferred_y,
            warmup_center_z,
            CHUNK_WARMUP_RADIUS,
            None,
        );
        self.last_stream_center = Some((warmup_center_x, preferred_y, warmup_center_z));
        self.last_stream_radius = CHUNK_WARMUP_RADIUS;
        println!(
            "new level generation: started streaming warmup (seed={}, preset={:?}, target_chunks={})",
            seed, preset, warmup_target
        );
    }

    pub(crate) fn poll_worldgen_progress(&mut self) -> Option<WorldGenProgress> {
        if let Some(task) = self.world_generation_task.as_ref() {
            let center_chunk_y = (self.world.sea_level() + 16).div_euclid(CHUNK_SIZE);
            self.ensure_chunks_in_radius(
                task.warmup_center_x,
                center_chunk_y,
                task.warmup_center_z,
                CHUNK_WARMUP_RADIUS,
                None,
            );
        }
        self.pump_chunkgen_results();
        self.pump_storage_load_results();
        self.pump_lighting_results();
        self.world_generation_task
            .as_mut()
            .and_then(|task| task.pending_progress.take())
    }

    pub(crate) fn try_finish_new_level_generation(&mut self, camera: &Camera) -> Option<Vec3> {
        if let Some(task) = self.world_generation_task.as_ref() {
            let center_chunk_y = (self.world.sea_level() + 16).div_euclid(CHUNK_SIZE);
            self.ensure_chunks_in_radius(
                task.warmup_center_x,
                center_chunk_y,
                task.warmup_center_z,
                CHUNK_WARMUP_RADIUS,
                None,
            );
        }
        self.pump_chunkgen_results();
        self.pump_storage_load_results();
        self.pump_lighting_results();
        self.refresh_worldgen_warmup_tracking();
        let task = self.world_generation_task.as_ref()?;
        if task.warmup_meshed_done.len() < task.warmup_target {
            return None;
        }

        println!(
            "[perf][startup] initial warmup ready (generated={} meshed={} target={}) after {:.2}ms",
            task.warmup_generated_done.len(),
            task.warmup_meshed_done.len(),
            task.warmup_target,
            task.started_at.elapsed().as_secs_f64() * 1000.0
        );
        let spawn_x = task.warmup_center_x * CHUNK_SIZE;
        let spawn_z = task.warmup_center_z * CHUNK_SIZE;
        let terrain_y = self.world.terrain_height_at(spawn_x, spawn_z) as f32;
        let spawn = Vec3::new(spawn_x as f32 + 0.5, terrain_y + 2.0, spawn_z as f32 + 0.5);

        self.world_generation_task = None;
        self.enqueue_chunks_within_view(camera);
        Some(spawn)
    }

    pub(crate) fn save_world(&mut self) {
        let started = Instant::now();
        if let Err(err) = self.save_ticking_areas() {
            eprintln!("world save: failed to save ticking areas: {err}");
        }
        self.pump_storage_save_results();
        self.autosave_sweep_active = true;
        let mut queued_total = 0usize;
        let mut spins = 0usize;
        while !self.dirty_columns.is_empty() || !self.save_columns_inflight.is_empty() {
            queued_total += self.queue_dirty_columns_with_budget(64);
            if !self.save_columns_inflight.is_empty() {
                if let Err(err) = self.storage_writer.flush() {
                    self.saved_revision.clear();
                    eprintln!(
                        "world save failed after queuing {} dirty columns: {}",
                        queued_total, err
                    );
                    return;
                }
                self.pump_storage_save_results();
            } else if queued_total == 0 {
                break;
            }
            spins += 1;
            if spins > 4096 {
                eprintln!(
                    "world save aborted after {} iterations (dirty_columns={}, inflight={})",
                    spins,
                    self.dirty_columns.len(),
                    self.save_columns_inflight.len()
                );
                break;
            }
        }
        match self.storage_writer.flush() {
            Ok(()) => {
                self.pump_storage_save_results();
                self.autosave_sweep_active = false;
                self.last_autosave_at = Instant::now();
                println!(
                    "world save: flushed {} dirty loaded columns in {:.2}ms",
                    queued_total,
                    started.elapsed().as_secs_f64() * 1000.0
                );
            }
            Err(err) => {
                self.saved_revision.clear();
                eprintln!(
                    "world save failed after queuing {} dirty columns: {}",
                    queued_total, err
                );
            }
        }
    }

    pub(crate) fn next_particle_rand(&mut self) -> f32 {
        // xorshift32: einfacher deterministischer PRNG ohne externe Crates.
        let mut x = self.particle_rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.particle_rng = x.max(1);
        (self.particle_rng as f32) / (u32::MAX as f32)
    }

    pub(crate) fn spawn_block_particles(&mut self, block_pos: IVec3, block: BlockId) -> usize {
        if !block.is_voxel_filled() {
            return 0;
        }

        let center = Vec3::new(
            block_pos.x as f32 + 0.5,
            block_pos.y as f32 + 0.5,
            block_pos.z as f32 + 0.5,
        );
        let base = block.color();

        let mut spawned = 0usize;
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
            spawned += 1;
        }

        if self.particles.len() > PARTICLE_MAX_COUNT {
            let overflow = self.particles.len() - PARTICLE_MAX_COUNT;
            self.particles.drain(0..overflow);
        }
        spawned
    }

    pub(crate) fn update_particles(&mut self, dt: f32) {
        for p in &mut self.particles {
            p.pos += p.vel * dt;
            p.vel.y -= PARTICLE_GRAVITY * dt;
            p.life -= dt;
        }
        self.particles.retain(|p| p.life > 0.0);
    }

    pub(crate) fn build_particle_mesh(&self, camera: &Camera) -> (Vec<Vertex>, Vec<u32>) {
        if self.particles.is_empty() {
            return (Vec::with_capacity(0), Vec::with_capacity(0));
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
                light_packed: FULLBRIGHT_LIGHT_PACKED,
                color: p.color,
            });
            vertices.push(Vertex {
                position: p1.to_array(),
                uv: [0.0, 0.0],
                tex_layer: 0,
                tint_kind: TINT_NONE,
                light_packed: FULLBRIGHT_LIGHT_PACKED,
                color: p.color,
            });
            vertices.push(Vertex {
                position: p2.to_array(),
                uv: [0.0, 0.0],
                tex_layer: 0,
                tint_kind: TINT_NONE,
                light_packed: FULLBRIGHT_LIGHT_PACKED,
                color: p.color,
            });
            vertices.push(Vertex {
                position: p3.to_array(),
                uv: [0.0, 0.0],
                tex_layer: 0,
                tint_kind: TINT_NONE,
                light_packed: FULLBRIGHT_LIGHT_PACKED,
                color: p.color,
            });
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }

        (vertices, indices)
    }

    pub(crate) fn neighbor_chunk_coords(coord: ChunkCoord) -> [ChunkCoord; 6] {
        [
            ChunkCoord {
                x: coord.x + 1,
                y: coord.y,
                z: coord.z,
            },
            ChunkCoord {
                x: coord.x - 1,
                y: coord.y,
                z: coord.z,
            },
            ChunkCoord {
                x: coord.x,
                y: coord.y + 1,
                z: coord.z,
            },
            ChunkCoord {
                x: coord.x,
                y: coord.y - 1,
                z: coord.z,
            },
            ChunkCoord {
                x: coord.x,
                y: coord.y,
                z: coord.z + 1,
            },
            ChunkCoord {
                x: coord.x,
                y: coord.y,
                z: coord.z - 1,
            },
        ]
    }

    fn snapshot_neighborhood(&self, coord: ChunkCoord) -> Option<(NeighborhoodSnapshot, u64)> {
        let center = self.world.get_chunk(coord)?;
        let center_revision = center.revision;
        let center_snapshot = ChunkSnapshot {
            blocks: center.blocks.clone(),
            block_data: center.block_data.clone(),
            light: center.light.clone(),
        };
        let neighbor_coords = Self::neighbor_chunk_coords(coord);
        let neighbors = std::array::from_fn(|index| {
            self.world
                .get_chunk(neighbor_coords[index])
                .map(|chunk| ChunkSnapshot {
                    blocks: chunk.blocks.clone(),
                    block_data: chunk.block_data.clone(),
                    light: chunk.light.clone(),
                })
        });

        Some((
            NeighborhoodSnapshot {
                center: center_snapshot,
                neighbors,
            },
            center_revision,
        ))
    }

    /// Nach einem Block-Change reicht nicht nur der eigene Chunk:
    /// Liegt der Block auf einer Chunk-Grenze, ändern sich sichtbare Faces im Nachbarchunk.
    pub(crate) fn affected_chunks_for_block_change(world_pos: IVec3) -> Vec<ChunkCoord> {
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

        affected
    }

    pub(crate) fn mark_chunk_dirty(&mut self, coord: ChunkCoord) {
        if self.dirty_chunk_set.contains(&coord) {
            return;
        }
        self.dirty_chunk_queue.push_back(coord);
        self.dirty_chunk_set.insert(coord);
    }

    fn mark_neighbors_dirty_if_meshed(&mut self, coord: ChunkCoord) {
        for neighbor in Self::neighbor_chunk_coords(coord) {
            if self.world.get_chunk(neighbor).is_some() && self.chunk_meshes.contains_key(&neighbor)
            {
                self.mark_chunk_dirty(neighbor);
            }
        }
    }

    fn mark_skipped_full_solid_neighbors_dirty(&mut self, coord: ChunkCoord) {
        for neighbor in Self::neighbor_chunk_coords(coord) {
            if self.full_solid_mesh_skip_set.contains(&neighbor) {
                self.mark_chunk_dirty(neighbor);
            }
        }
    }

    fn chunk_is_fully_occluded_full_solid(&self, coord: ChunkCoord) -> bool {
        let Some(chunk) = self.world.get_chunk(coord) else {
            return false;
        };
        if !chunk.is_full_solid() {
            return false;
        }
        Self::neighbor_chunk_coords(coord).iter().all(|neighbor| {
            self.world
                .get_chunk(*neighbor)
                .is_some_and(|chunk| chunk.is_full_solid())
        })
    }

    fn trigger_fluid_mesh_rebuild(&mut self, coord: ChunkCoord) {
        let was_already_dirty = self.dirty_chunk_set.contains(&coord);
        self.mark_chunk_dirty(coord);
        if !was_already_dirty {
            self.fluid_dirty_due_to_fluids.insert(coord);
        }
    }

    fn note_fluid_chunk_changed(&mut self, coord: ChunkCoord) {
        let entry = self.fluid_mesh_coalesce.entry(coord).or_default();
        entry.changed_this_frame = true;
        entry.settle_steps_remaining = FLUID_MESH_SETTLE_STEPS;
        entry.steps_since_last_mesh = entry.steps_since_last_mesh.saturating_add(1);
        entry.pending_dirty = true;
    }

    fn flush_fluid_mesh_coalescer(&mut self) {
        let mut trigger_now = Vec::with_capacity(32);
        let mut remove_entries = Vec::with_capacity(16);

        for (&coord, entry) in self.fluid_mesh_coalesce.iter_mut() {
            if entry.changed_this_frame {
                if entry.steps_since_last_mesh >= FLUID_MESH_ACTIVE_REBUILD_EVERY_STEPS
                    && self.frame_index >= entry.next_allowed_frame
                {
                    entry.steps_since_last_mesh = 0;
                    entry.next_allowed_frame =
                        self.frame_index + FLUID_MESH_MIN_FRAMES_BETWEEN_DISPATCH;
                    entry.pending_dirty = false;
                    trigger_now.push(coord);
                }
            } else {
                if entry.settle_steps_remaining > 0 {
                    entry.settle_steps_remaining -= 1;
                }
                if entry.settle_steps_remaining == 0 {
                    if entry.pending_dirty || entry.steps_since_last_mesh > 0 {
                        entry.steps_since_last_mesh = 0;
                        entry.pending_dirty = false;
                        trigger_now.push(coord);
                    }
                    remove_entries.push(coord);
                }
            }

            entry.changed_this_frame = false;
        }

        for coord in trigger_now {
            self.trigger_fluid_mesh_rebuild(coord);
        }
        for coord in remove_entries {
            self.fluid_mesh_coalesce.remove(&coord);
        }
    }

    pub(crate) fn mark_dirty_after_block_change(&mut self, world_pos: IVec3) -> usize {
        let affected = Self::affected_chunks_for_block_change(world_pos);
        for coord in affected.iter().copied() {
            self.mark_chunk_dirty(coord);
            self.mark_column_dirty_for_chunk(coord);
        }
        self.request_lighting_rebuild();
        affected.len()
    }

    fn stage_block_edit_dirty_after_block_change(&mut self, world_pos: IVec3) -> usize {
        let affected = Self::affected_chunks_for_block_change(world_pos);
        for coord in affected.iter().copied() {
            self.pending_block_edit_dirty_chunks.insert(coord);
            self.pending_block_edit_dirty_columns
                .insert((coord.x, coord.z));
        }
        affected.len()
    }

    fn flush_pending_block_edit_dirty_marks(&mut self) -> (usize, usize, bool) {
        let dirty_chunk_count = self.pending_block_edit_dirty_chunks.len();
        let dirty_column_count = self.pending_block_edit_dirty_columns.len();
        let any_dirty = dirty_chunk_count > 0 || dirty_column_count > 0;
        let dirty_chunks = self
            .pending_block_edit_dirty_chunks
            .drain()
            .collect::<Vec<_>>();
        let dirty_columns = self
            .pending_block_edit_dirty_columns
            .drain()
            .collect::<Vec<_>>();

        for coord in dirty_chunks {
            self.mark_chunk_dirty(coord);
        }
        for (cx, cz) in dirty_columns {
            self.mark_column_dirty(cx, cz);
        }
        if any_dirty {
            self.request_lighting_rebuild();
        }

        (dirty_chunk_count, dirty_column_count, any_dirty)
    }

    fn enqueue_pending_block_edit(&mut self, pos: IVec3, old: BlockId, new: BlockId) {
        self.pending_block_edits
            .push_back(PendingBlockEdit { pos, old, new });
        self.stream_recent_mining_frames = SURFACE_STREAM_MINING_HOLD_FRAMES;
    }

    fn enqueue_pending_sand_gravity_check(&mut self, pos: IVec3) {
        if !World::in_bounds(pos) {
            return;
        }
        if self.pending_sand_gravity_check_set.insert(pos) {
            self.pending_sand_gravity_checks
                .push_back(PendingSandGravityCheck {
                    origin: pos,
                    checked_base: false,
                    next_y: (pos.y + 1).clamp(0, WORLD_MAX_Y),
                });
        }
    }

    fn process_pending_sand_gravity_check(
        &mut self,
        mut check: PendingSandGravityCheck,
        max_steps: usize,
    ) -> (
        PendingSandGravityCheck,
        SandGravityResolveStats,
        usize,
        bool,
    ) {
        let mut stats = SandGravityResolveStats::default();
        let mut steps_used = 0usize;
        if max_steps == 0 || !World::in_bounds(check.origin) {
            return (check, stats, 0, true);
        }

        if !check.checked_base {
            check.checked_base = true;
            if self.world.get_block(check.origin) == BlockId::Sand {
                stats.scanned += 1;
                steps_used += 1;
                if self.try_drop_sand_block(check.origin) {
                    stats.dropped += 1;
                }
                if steps_used >= max_steps {
                    return (check, stats, steps_used, false);
                }
            }
        }

        let x = check.origin.x;
        let z = check.origin.z;
        let mut y = check.next_y.clamp(0, WORLD_MAX_Y);
        while y <= WORLD_MAX_Y {
            let pos = IVec3::new(x, y, z);
            if self.world.get_block(pos) != BlockId::Sand {
                check.next_y = y;
                return (check, stats, steps_used, true);
            }
            stats.scanned += 1;
            steps_used += 1;
            if self.try_drop_sand_block(pos) {
                stats.dropped += 1;
            }
            y += 1;
            check.next_y = y;
            if steps_used >= max_steps {
                return (check, stats, steps_used, y > WORLD_MAX_Y);
            }
        }

        (check, stats, steps_used, true)
    }

    fn process_pending_block_edit_side_effects(&mut self) {
        let _trace_scope = perf_trace::scope("proc.process_pending_block_edit_sidefx.work");
        let started = Instant::now();
        let budget = Duration::from_secs_f32(BLOCK_EDIT_SIDEFX_BUDGET_MS / 1000.0);

        let mut edits_processed = 0usize;
        let mut particles_spawned = 0usize;
        let mut particle_events = 0usize;
        let mut fluid_notifies = 0usize;
        let mut sand_checks_processed = 0usize;
        let mut sand_steps_used = 0usize;
        let mut sand_scanned = 0usize;
        let mut sand_dropped = 0usize;
        let mut edits_remaining_before = self.pending_block_edits.len();
        let mut sand_remaining_before = self.pending_sand_gravity_checks.len();

        while edits_processed < BLOCK_EDIT_SIDEFX_MAX_EDITS_PER_FRAME && started.elapsed() < budget
        {
            let Some(edit) = self.pending_block_edits.pop_front() else {
                break;
            };

            edits_processed += 1;
            let _ = self.stage_block_edit_dirty_after_block_change(edit.pos);

            self.notify_fluid_on_block_change(edit.pos, edit.old, edit.new);
            fluid_notifies += 1;

            self.enqueue_pending_sand_gravity_check(edit.pos);

            let particle_block = if edit.new == BlockId::Air {
                edit.old
            } else {
                edit.new
            };
            if particles_spawned + PARTICLE_SPAWN_COUNT <= BLOCK_EDIT_PARTICLES_MAX_SPAWN_PER_FRAME
            {
                let spawned = self.spawn_block_particles(edit.pos, particle_block);
                if spawned > 0 {
                    particles_spawned += spawned;
                    particle_events += 1;
                }
            }
        }

        while sand_steps_used < BLOCK_EDIT_SAND_SCAN_STEPS_PER_FRAME && started.elapsed() < budget {
            let Some(check) = self.pending_sand_gravity_checks.pop_front() else {
                break;
            };
            self.pending_sand_gravity_check_set.remove(&check.origin);
            sand_checks_processed += 1;

            let remaining_steps = BLOCK_EDIT_SAND_SCAN_STEPS_PER_FRAME - sand_steps_used;
            let (next_check, stats, used_steps, complete) =
                self.process_pending_sand_gravity_check(check, remaining_steps);
            sand_steps_used += used_steps;
            sand_scanned += stats.scanned;
            sand_dropped += stats.dropped;

            if !complete
                && self
                    .pending_sand_gravity_check_set
                    .insert(next_check.origin)
            {
                self.pending_sand_gravity_checks.push_back(next_check);
            }
        }

        let (dirty_chunks_flushed, dirty_columns_flushed, lighting_requested) =
            self.flush_pending_block_edit_dirty_marks();

        let ms = started.elapsed().as_secs_f32() * 1000.0;
        edits_remaining_before = edits_remaining_before.saturating_sub(edits_processed);
        sand_remaining_before = sand_remaining_before.saturating_sub(sand_checks_processed);
        perf_trace::record_duration_ms(
            "proc.process_pending_block_edit_sidefx.work",
            ms,
            &format!(
                "edits_proc={} edits_q_before={} edits_q_after={} fluids={} particles_spawned={} particle_events={} sand_checks_proc={} sand_q_before={} sand_q_after={} sand_steps={} sand_scanned={} sand_dropped={} dirty_chunks_flush={} dirty_cols_flush={} lighting_req={}",
                edits_processed,
                edits_remaining_before + edits_processed,
                self.pending_block_edits.len(),
                fluid_notifies,
                particles_spawned,
                particle_events,
                sand_checks_processed,
                sand_remaining_before + sand_checks_processed,
                self.pending_sand_gravity_checks.len(),
                sand_steps_used,
                sand_scanned,
                sand_dropped,
                dirty_chunks_flushed,
                dirty_columns_flushed,
                lighting_requested as u8
            ),
        );
    }

    fn request_lighting_rebuild(&mut self) {
        let was_pending = self.lighting_request_pending;
        if self.lighting_job_inflight || !self.lighting_request_pending {
            self.lighting_generation = self.lighting_generation.wrapping_add(1);
        }
        self.lighting_request_pending = true;
        self.lighting_rebuild_not_before =
            Some(Instant::now() + Duration::from_millis(LIGHTING_REBUILD_DEBOUNCE_MS));
        if !was_pending {
            eprintln!(
                "[perf][lighting][async] requested gen={} debounce_ms={} inflight={}",
                self.lighting_generation, LIGHTING_REBUILD_DEBOUNCE_MS, self.lighting_job_inflight
            );
        }
    }

    fn queue_lighting_changed_chunk_dirty(&mut self, coord: ChunkCoord) {
        if self.pending_lighting_dirty_set.insert(coord) {
            self.pending_lighting_dirty_chunks.push_back(coord);
        }
    }

    fn pump_pending_lighting_dirty_chunk_marks(&mut self) {
        let started = Instant::now();
        for _ in 0..LIGHTING_CHANGED_CHUNK_DIRTY_APPLY_PER_FRAME {
            if started.elapsed().as_secs_f32() * 1000.0
                >= LIGHTING_CHANGED_CHUNK_DIRTY_APPLY_BUDGET_MS
            {
                break;
            }
            let Some(coord) = self.pending_lighting_dirty_chunks.pop_front() else {
                break;
            };
            self.pending_lighting_dirty_set.remove(&coord);
            self.mark_chunk_dirty(coord);
        }
    }

    fn pump_lighting_results(&mut self) {
        let _trace_scope = perf_trace::scope("state.pump_lighting_results");
        let started = Instant::now();
        let backlog_before = self.lighting_results.len();
        self.frame_perf.lighting_results_backlog_before = self
            .frame_perf
            .lighting_results_backlog_before
            .max(backlog_before);
        let mut drained = 0usize;
        while drained < LIGHTING_RESULTS_PER_FRAME {
            let Ok(result) = self.lighting_results.try_recv() else {
                break;
            };
            drained += 1;
            self.lighting_job_inflight = false;

            if result.generation != self.lighting_generation {
                eprintln!(
                    "[perf][lighting][async] discard stale result gen={} current_gen={}",
                    result.generation, self.lighting_generation
                );
                continue;
            }

            let mut changed_coords = Vec::with_capacity(result.chunk_lights.len());
            for (coord, light) in result.chunk_lights {
                let Some(chunk) = self.world.chunks.get_mut(&coord) else {
                    continue;
                };
                if chunk.light.as_slice() != light.as_slice() {
                    chunk.light = Arc::new(light);
                    chunk.revision = chunk.revision.wrapping_add(1);
                    changed_coords.push(coord);
                }
            }

            let changed_count = changed_coords.len();
            self.frame_perf.lighting_chunks_changed += changed_count;
            for coord in changed_coords {
                self.queue_lighting_changed_chunk_dirty(coord);
            }

            eprintln!(
                "[perf][lighting][async] applied gen={} changed_chunks={}",
                result.generation, changed_count
            );
        }
        self.frame_perf.lighting_results_drained += drained;
        self.frame_perf.lighting_results_backlog_after = self
            .frame_perf
            .lighting_results_backlog_after
            .max(self.lighting_results.len());
        let dur_ms = started.elapsed().as_secs_f32() * 1000.0;
        self.frame_perf.rebuild_world_lighting_ms += dur_ms;
        perf_trace::record_duration_ms(
            "state.pump_lighting_results",
            dur_ms,
            &format!(
                "drained={} changed={} q_before={} q_after={}",
                drained,
                self.frame_perf.lighting_chunks_changed,
                backlog_before,
                self.lighting_results.len()
            ),
        );
    }

    fn schedule_lighting_rebuild_if_needed(&mut self) {
        let _trace_scope = perf_trace::scope("state.schedule_lighting_rebuild_if_needed");
        if !self.lighting_request_pending || self.lighting_job_inflight {
            return;
        }
        if self
            .lighting_rebuild_not_before
            .is_some_and(|not_before| Instant::now() < not_before)
        {
            return;
        }

        let snapshot_started = Instant::now();
        let snapshot = self.world.build_lighting_snapshot();
        let snapshot_ms = snapshot_started.elapsed().as_secs_f64() * 1000.0;
        let generation = self.lighting_generation;
        let loaded_chunk_count = snapshot.loaded_chunk_count();
        let xz_column_count = snapshot.xz_column_count();

        if self
            .lighting_jobs
            .send(LightingJob {
                generation,
                snapshot,
            })
            .is_err()
        {
            eprintln!("[warn] lighting worker channel closed; keeping request pending");
            self.lighting_request_pending = true;
            self.lighting_job_inflight = false;
            return;
        }

        self.lighting_job_inflight = true;
        self.lighting_request_pending = false;
        self.lighting_rebuild_not_before = None;
        eprintln!(
            "[perf][lighting][async] queued gen={} loaded_chunks={} xz_columns={} snapshot_ms={:.2}",
            generation, loaded_chunk_count, xz_column_count, snapshot_ms
        );
    }

    fn rebuild_world_lighting_if_dirty(&mut self) {
        let _trace_scope = perf_trace::scope("state.rebuild_world_lighting_if_dirty");
        self.pump_lighting_results();
        self.pump_pending_lighting_dirty_chunk_marks();
        self.schedule_lighting_rebuild_if_needed();
    }

    fn sync_generated_empty_chunk_marker(&mut self, coord: ChunkCoord) {
        if self.world.get_chunk(coord).is_none() {
            self.generated_empty_chunks.insert(coord);
        } else {
            self.generated_empty_chunks.remove(&coord);
        }
    }

    fn seed_pending_lighting_for_chunk(&self, coord: ChunkCoord, chunk: &mut Chunk) {
        if !chunk.light.iter().all(|&v| v == 0) {
            return;
        }

        let base_y = coord.y * CHUNK_SIZE;
        let light = Arc::make_mut(&mut chunk.light);
        for y in 0..CHUNK_SIZE {
            let world_y = base_y + y;
            let sky_fill = if world_y >= PENDING_LIGHTING_SKY_HEURISTIC_MIN_Y {
                15
            } else {
                0
            };
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    let idx = Chunk::index(x, y, z);
                    let block = chunk.blocks[idx];
                    let sky = if block.blocks_skylight() { 0 } else { sky_fill };
                    light[idx] = LightLevel::new(sky, block.emitted_light()).pack();
                }
            }
        }
    }

    fn try_drop_sand_block(&mut self, pos: IVec3) -> bool {
        if !World::in_bounds(pos) || self.world.get_block(pos) != BlockId::Sand || pos.y <= 0 {
            return false;
        }

        let mut landing = pos;
        while landing.y > 0 {
            let below = landing + IVec3::new(0, -1, 0);
            if self.world.get_block(below).is_solid() {
                break;
            }
            landing = below;
        }

        if landing == pos {
            return false;
        }

        let replaced = self.world.get_block(landing);
        if self.world.set_block_and_data(pos, BlockId::Air, 0).is_err() {
            return false;
        }
        if self
            .world
            .set_block_and_data(landing, BlockId::Sand, 0)
            .is_err()
        {
            let _ = self.world.set_block_and_data(pos, BlockId::Sand, 0);
            return false;
        }
        self.sync_generated_empty_chunk_marker(World::world_to_chunk_coord(pos));
        self.sync_generated_empty_chunk_marker(World::world_to_chunk_coord(landing));
        self.notify_fluid_on_block_change(pos, BlockId::Sand, BlockId::Air);
        let _ = self.stage_block_edit_dirty_after_block_change(pos);
        self.notify_fluid_on_block_change(landing, replaced, BlockId::Sand);
        let _ = self.stage_block_edit_dirty_after_block_change(landing);
        true
    }

    fn resolve_sand_gravity_after_direct_block_update(
        &mut self,
        world_pos: IVec3,
    ) -> SandGravityResolveStats {
        let mut stats = SandGravityResolveStats::default();
        if !World::in_bounds(world_pos) {
            return stats;
        }

        if self.world.get_block(world_pos) == BlockId::Sand {
            stats.scanned += 1;
            if self.try_drop_sand_block(world_pos) {
                stats.dropped += 1;
            }
        }

        let start_y = (world_pos.y + 1).clamp(0, WORLD_MAX_Y);
        for y in start_y..=WORLD_MAX_Y {
            let pos = IVec3::new(world_pos.x, y, world_pos.z);
            if self.world.get_block(pos) != BlockId::Sand {
                break;
            }
            stats.scanned += 1;
            if self.try_drop_sand_block(pos) {
                stats.dropped += 1;
            }
        }
        stats
    }

    pub(crate) fn process_dirty_chunk_meshes(&mut self, camera: &Camera) {
        let _trace_scope = perf_trace::scope("state.process_dirty_chunk_meshes");
        let proc_started = Instant::now();
        self.update_stream_burst_mode_for_camera(camera);

        {
            let _scope = perf_trace::scope("proc.pump_chunkgen_results");
            let started = Instant::now();
            self.pump_chunkgen_results();
            let ms = started.elapsed().as_secs_f32() * 1000.0;
            self.frame_perf.proc_pump_chunkgen_results_ms += ms;
            perf_trace::record_duration_ms("proc.pump_chunkgen_results", ms, "");
        }
        {
            let _scope = perf_trace::scope("proc.pump_storage_load_results");
            let started = Instant::now();
            self.pump_storage_load_results();
            let ms = started.elapsed().as_secs_f32() * 1000.0;
            self.frame_perf.proc_pump_storage_load_results_ms += ms;
            perf_trace::record_duration_ms("proc.pump_storage_load_results", ms, "");
        }
        {
            let _scope = perf_trace::scope("proc.unload_chunks_outside_keep_radius");
            let started = Instant::now();
            self.unload_chunks_outside_keep_radius(camera);
            let ms = started.elapsed().as_secs_f32() * 1000.0;
            self.frame_perf.proc_unload_chunks_outside_keep_radius_ms += ms;
            perf_trace::record_duration_ms("proc.unload_chunks_outside_keep_radius", ms, "");
        }
        {
            let _scope = perf_trace::scope("proc.prune_chunk_meshes_outside_view");
            let started = Instant::now();
            self.prune_chunk_meshes_outside_view(camera);
            let ms = started.elapsed().as_secs_f32() * 1000.0;
            self.frame_perf.proc_prune_chunk_meshes_outside_view_ms += ms;
            perf_trace::record_duration_ms("proc.prune_chunk_meshes_outside_view", ms, "");
        }
        {
            let _scope = perf_trace::scope("proc.enqueue_chunks_within_view");
            let started = Instant::now();
            self.enqueue_chunks_within_view(camera);
            let ms = started.elapsed().as_secs_f32() * 1000.0;
            self.frame_perf.proc_enqueue_chunks_within_view_ms += ms;
            perf_trace::record_duration_ms("proc.enqueue_chunks_within_view", ms, "");
        }
        {
            let _scope = perf_trace::scope("proc.process_pending_block_edit_sidefx");
            let started = Instant::now();
            self.process_pending_block_edit_side_effects();
            let ms = started.elapsed().as_secs_f32() * 1000.0;
            perf_trace::record_duration_ms("proc.process_pending_block_edit_sidefx", ms, "");
        }
        {
            let _scope = perf_trace::scope("proc.rebuild_world_lighting_if_dirty");
            let started = Instant::now();
            self.rebuild_world_lighting_if_dirty();
            let ms = started.elapsed().as_secs_f32() * 1000.0;
            self.last_lighting_step_ms = ms;
            self.frame_perf.proc_rebuild_world_lighting_if_dirty_ms += ms;
            perf_trace::record_duration_ms("proc.rebuild_world_lighting_if_dirty", ms, "");
        }
        let (center_chunk_x, center_chunk_z, active_radius) =
            if let Some(task) = self.world_generation_task.as_ref() {
                (
                    task.warmup_center_x,
                    task.warmup_center_z,
                    CHUNK_WARMUP_RADIUS,
                )
            } else {
                let (x, _, z) = Self::camera_chunk_coords(camera);
                (x, z, self.view_distance_chunks)
            };

        let mut dispatch_frustum_build_ms = 0.0f32;
        let mut dispatch_pop_dirty_ms = 0.0f32;
        let mut dispatch_snapshot_neighborhood_ms = 0.0f32;
        let mut dispatch_send_mesh_job_ms = 0.0f32;
        let mut dispatch_requeue_dirty_ms = 0.0f32;

        let frustum = {
            let _scope = perf_trace::scope("proc.dispatch.frustum_build");
            let started = Instant::now();
            let aspect_ratio = (self.config.width as f32 / self.config.height as f32).max(0.0001);
            let view_proj = build_view_proj_wgpu(camera, aspect_ratio);
            let frustum = Frustum::from_view_proj(view_proj);
            dispatch_frustum_build_ms += started.elapsed().as_secs_f32() * 1000.0;
            frustum
        };
        let center_chunk_y = Self::camera_chunk_coords(camera).1;
        if self
            .frame_index
            .is_multiple_of(MESH_MISSING_SAFETY_SCAN_PERIOD_FRAMES)
        {
            self.queue_missing_meshes_in_radius(center_chunk_x, center_chunk_y, center_chunk_z);
        }
        self.reprioritize_dirty_chunk_queue(
            center_chunk_x,
            center_chunk_y,
            center_chunk_z,
            &frustum,
            camera,
        );
        let dispatch_budget_ms = self
            .stream_budget_for_stage_ms(STREAM_DISPATCH_BUDGET_MS, STREAM_BURST_DISPATCH_BUDGET_MS);
        self.frame_perf.stream_dispatch_budget_ms = self
            .frame_perf
            .stream_dispatch_budget_ms
            .max(dispatch_budget_ms);

        let dispatch_loop_ms = {
            let _scope = perf_trace::scope("proc.dispatch_mesh_jobs_loop_total");
            let started = Instant::now();
            let mut loop_iterations = 0usize;
            while loop_iterations < STREAM_STAGE_LOOP_SAFETY_CAP {
                if dispatch_budget_ms <= 0.0 {
                    break;
                }
                if loop_iterations > 0
                    && started.elapsed().as_secs_f32() * 1000.0 >= dispatch_budget_ms
                {
                    break;
                }
                loop_iterations += 1;
                let pop_started = Instant::now();
                let popped = self.dirty_chunk_queue.pop_front();
                if let Some(coord) = popped {
                    self.dirty_chunk_set.remove(&coord);
                }
                dispatch_pop_dirty_ms += pop_started.elapsed().as_secs_f32() * 1000.0;
                let Some(coord) = popped else {
                    break;
                };

                if let Some(until_frame) = self.deferred_mesh_until_frame.get(&coord).copied() {
                    if self.frame_index < until_frame {
                        let started = Instant::now();
                        self.mark_chunk_dirty(coord);
                        dispatch_requeue_dirty_ms += started.elapsed().as_secs_f32() * 1000.0;
                        continue;
                    }
                    self.deferred_mesh_until_frame.remove(&coord);
                }

                let defer_fluid_dispatch = self.fluid_dirty_due_to_fluids.contains(&coord)
                    && self
                        .fluid_mesh_coalesce
                        .get(&coord)
                        .is_some_and(|entry| self.frame_index < entry.next_allowed_frame);
                if defer_fluid_dispatch {
                    self.fluid_dirty_due_to_fluids.remove(&coord);
                    if let Some(entry) = self.fluid_mesh_coalesce.get_mut(&coord) {
                        entry.pending_dirty = true;
                    }
                    continue;
                }

                if !Self::is_chunk_within_radius_xz(
                    coord,
                    center_chunk_x,
                    center_chunk_z,
                    active_radius,
                ) {
                    self.fluid_dirty_due_to_fluids.remove(&coord);
                    self.deferred_mesh_until_frame.remove(&coord);
                    self.chunk_meshes.remove(&coord);
                    continue;
                }

                let force_warmup_mesh = self.is_worldgen_warmup_coord(coord);

                // Frustum culling: skip chunks outside view frustum
                if !force_warmup_mesh && !chunk_in_frustum(&frustum, coord) {
                    self.fluid_dirty_due_to_fluids.remove(&coord);
                    self.frustum_culled_chunks_total += 1;
                    self.meshes_skipped_due_to_frustum += 1;
                    self.deferred_mesh_until_frame.insert(
                        coord,
                        self.frame_index.saturating_add(MESH_FRUSTUM_DEFER_FRAMES),
                    );
                    let started = Instant::now();
                    self.mark_chunk_dirty(coord);
                    dispatch_requeue_dirty_ms += started.elapsed().as_secs_f32() * 1000.0;
                    continue;
                }

                if self.inflight.contains_key(&coord) {
                    let started = Instant::now();
                    self.mark_chunk_dirty(coord);
                    dispatch_requeue_dirty_ms += started.elapsed().as_secs_f32() * 1000.0;
                    continue;
                }

                if !force_warmup_mesh && self.chunk_is_fully_occluded_full_solid(coord) {
                    self.full_solid_mesh_skip_set.insert(coord);
                    self.fluid_dirty_due_to_fluids.remove(&coord);
                    if let Some(entry) = self.fluid_mesh_coalesce.get_mut(&coord) {
                        entry.pending_dirty = false;
                    }
                    self.chunk_meshes.remove(&coord);
                    continue;
                }
                self.full_solid_mesh_skip_set.remove(&coord);

                let had_mesh = self.chunk_meshes.contains_key(&coord);
                let snapshot_started = Instant::now();
                let snapshot_result = self.snapshot_neighborhood(coord);
                dispatch_snapshot_neighborhood_ms +=
                    snapshot_started.elapsed().as_secs_f32() * 1000.0;
                let Some((snapshot, revision)) = snapshot_result else {
                    self.fluid_dirty_due_to_fluids.remove(&coord);
                    self.chunk_meshes.remove(&coord);
                    self.full_solid_mesh_skip_set.remove(&coord);
                    if had_mesh {
                        let started = Instant::now();
                        for neighbor in Self::neighbor_chunk_coords(coord) {
                            self.mark_chunk_dirty(neighbor);
                        }
                        dispatch_requeue_dirty_ms += started.elapsed().as_secs_f32() * 1000.0;
                    }
                    continue;
                };

                let send_started = Instant::now();
                let send_ok = self
                    .mesh_jobs
                    .send(MeshJob {
                        coord,
                        revision,
                        mesh_debug_epoch: self.mesh_debug_epoch,
                        snapshot,
                        use_greedy: USE_GREEDY_MESHING,
                    })
                    .is_ok();
                dispatch_send_mesh_job_ms += send_started.elapsed().as_secs_f32() * 1000.0;
                if send_ok {
                    self.inflight.insert(coord, revision);
                    self.fluid_dirty_due_to_fluids.remove(&coord);
                    self.deferred_mesh_until_frame.remove(&coord);
                    if let Some(entry) = self.fluid_mesh_coalesce.get_mut(&coord) {
                        entry.pending_dirty = false;
                    }
                } else {
                    let started = Instant::now();
                    self.mark_chunk_dirty(coord);
                    dispatch_requeue_dirty_ms += started.elapsed().as_secs_f32() * 1000.0;
                    break;
                }
            }
            started.elapsed().as_secs_f32() * 1000.0
        };
        self.frame_perf.stream_dispatch_used_ms += dispatch_loop_ms;
        self.frame_perf.proc_dispatch_mesh_jobs_loop_total_ms += dispatch_loop_ms;
        self.frame_perf.proc_dispatch_frustum_build_ms += dispatch_frustum_build_ms;
        self.frame_perf.proc_dispatch_pop_dirty_ms += dispatch_pop_dirty_ms;
        self.frame_perf.proc_dispatch_snapshot_neighborhood_ms += dispatch_snapshot_neighborhood_ms;
        self.frame_perf.proc_dispatch_send_mesh_job_ms += dispatch_send_mesh_job_ms;
        self.frame_perf.proc_dispatch_requeue_dirty_ms += dispatch_requeue_dirty_ms;
        perf_trace::record_duration_ms("proc.dispatch_mesh_jobs_loop_total", dispatch_loop_ms, "");
        perf_trace::record_duration_ms(
            "proc.dispatch.frustum_build",
            dispatch_frustum_build_ms,
            "",
        );
        perf_trace::record_duration_ms("proc.dispatch.pop_dirty", dispatch_pop_dirty_ms, "");
        perf_trace::record_duration_ms(
            "proc.dispatch.snapshot_neighborhood",
            dispatch_snapshot_neighborhood_ms,
            "",
        );
        perf_trace::record_duration_ms(
            "proc.dispatch.send_mesh_job",
            dispatch_send_mesh_job_ms,
            "",
        );
        perf_trace::record_duration_ms(
            "proc.dispatch.requeue_dirty",
            dispatch_requeue_dirty_ms,
            "",
        );

        {
            let _scope = perf_trace::scope("proc.autosave_if_due");
            let started = Instant::now();
            self.autosave_if_due();
            let ms = started.elapsed().as_secs_f32() * 1000.0;
            self.frame_perf.proc_autosave_if_due_ms += ms;
            perf_trace::record_duration_ms("proc.autosave_if_due", ms, "");
        }

        let proc_total_ms = proc_started.elapsed().as_secs_f32() * 1000.0;
        self.frame_perf.process_dirty_chunk_meshes_ms += proc_total_ms;
    }

    pub(crate) fn pump_mesh_results(&mut self, camera: &Camera) {
        let _trace_scope = perf_trace::scope("state.pump_mesh_results");
        let pump_started = Instant::now();
        let mesh_rx_backlog_before = self.mesh_results.len();
        self.frame_perf.mesh_results_backlog_before = self
            .frame_perf
            .mesh_results_backlog_before
            .max(mesh_rx_backlog_before);
        self.frame_perf.mesh_upload_queue_before = self
            .frame_perf
            .mesh_upload_queue_before
            .max(self.mesh_upload_queue.len());

        let mut drained = 0usize;
        while drained < MESH_RESULTS_DRAIN_PER_FRAME {
            let Ok(result) = self.mesh_results.try_recv() else {
                break;
            };
            self.mesh_upload_queue.push_back(result);
            drained += 1;
        }
        self.reprioritize_mesh_upload_queue(camera);
        self.frame_perf.mesh_results_drained += drained;
        self.frame_perf.mesh_results_backlog_after = self
            .frame_perf
            .mesh_results_backlog_after
            .max(self.mesh_results.len());

        let mut processed = 0usize;
        let mut apply_mesh_time = Duration::ZERO;
        let mut upload_stats = MeshUploadStats::default();
        let mesh_upload_budget_ms = self.stream_budget_for_stage_ms(
            STREAM_MESH_UPLOAD_BUDGET_MS,
            STREAM_BURST_MESH_UPLOAD_BUDGET_MS,
        );
        self.frame_perf.stream_mesh_upload_budget_ms = self
            .frame_perf
            .stream_mesh_upload_budget_ms
            .max(mesh_upload_budget_ms);
        let mesh_upload_loop_started = Instant::now();
        let mut upload_loop_iterations = 0usize;

        while upload_loop_iterations < STREAM_STAGE_LOOP_SAFETY_CAP {
            if mesh_upload_budget_ms <= 0.0 {
                break;
            }
            if upload_loop_iterations > 0
                && mesh_upload_loop_started.elapsed().as_secs_f32() * 1000.0
                    >= mesh_upload_budget_ms
            {
                break;
            }
            upload_loop_iterations += 1;
            let Some(result) = self.mesh_upload_queue.pop_front() else {
                break;
            };
            processed += 1;

            if result.mesh_debug_epoch != self.mesh_debug_epoch {
                if self.inflight.get(&result.coord).copied() == Some(result.revision) {
                    self.inflight.remove(&result.coord);
                }
                self.mark_chunk_dirty(result.coord);
                continue;
            }

            let expected_revision = match self.inflight.remove(&result.coord) {
                Some(revision) => revision,
                None => continue,
            };
            if expected_revision != result.revision {
                self.mark_chunk_dirty(result.coord);
                continue;
            }

            let current_revision = self
                .world
                .get_chunk(result.coord)
                .map(|chunk| chunk.revision);
            if current_revision != Some(result.revision) {
                self.mark_chunk_dirty(result.coord);
                continue;
            }

            let had_mesh = self.chunk_meshes.contains_key(&result.coord);
            let has_mesh = if let Some(geometry) = result.geometry {
                let apply_started = Instant::now();
                let existing_mesh = self.chunk_meshes.remove(&result.coord);
                let (new_mesh, stats) = upload_layered_chunk_mesh_to_gpu_reuse(
                    &self.device,
                    &self.queue,
                    geometry,
                    existing_mesh,
                );
                apply_mesh_time += apply_started.elapsed();
                upload_stats.add_assign(stats);
                if let Some(layered_mesh) = new_mesh {
                    self.chunk_meshes.insert(result.coord, layered_mesh);
                    true
                } else {
                    false
                }
            } else {
                self.chunk_meshes.remove(&result.coord);
                false
            };

            if had_mesh != has_mesh {
                for neighbor in Self::neighbor_chunk_coords(result.coord) {
                    self.mark_chunk_dirty(neighbor);
                }
            }
        }
        if self.world_generation_task.is_some() && processed > 0 {
            self.refresh_worldgen_warmup_tracking();
        }
        self.frame_perf.stream_mesh_upload_used_ms +=
            mesh_upload_loop_started.elapsed().as_secs_f32() * 1000.0;

        let total_time = pump_started.elapsed();
        self.frame_perf.mesh_uploads_processed += processed;
        self.frame_perf.mesh_upload_queue_after = self
            .frame_perf
            .mesh_upload_queue_after
            .max(self.mesh_upload_queue.len());
        self.frame_perf.gpu_chunk_mesh_upload_ms += apply_mesh_time.as_secs_f32() * 1000.0;
        self.frame_perf.mesh_gpu_buffers_created += upload_stats.created_buffers;
        self.frame_perf.mesh_gpu_reused_writes += upload_stats.reused_buffer_writes;
        self.frame_perf.pump_mesh_results_ms += total_time.as_secs_f32() * 1000.0;
        if processed > 0
            && (total_time >= MESH_UPLOAD_LOG_THRESHOLD
                || upload_stats.created_buffers > 0
                || upload_stats.buffer_create_time >= Duration::from_millis(1))
        {
            eprintln!(
                "[perf][mesh_upload] processed={} queue_remaining={} total_ms={:.2} apply_ms={:.2} create_ms={:.2} write_ms={:.2} created_buffers={} reused_writes={} bytes_created={} bytes_written={}",
                processed,
                self.mesh_upload_queue.len(),
                total_time.as_secs_f64() * 1000.0,
                apply_mesh_time.as_secs_f64() * 1000.0,
                upload_stats.buffer_create_time.as_secs_f64() * 1000.0,
                upload_stats.buffer_write_time.as_secs_f64() * 1000.0,
                upload_stats.created_buffers,
                upload_stats.reused_buffer_writes,
                upload_stats.bytes_created,
                upload_stats.bytes_written
            );
        }
    }

    pub(crate) fn notify_fluid_on_block_change(
        &mut self,
        world_pos: IVec3,
        old_block: BlockId,
        new_block: BlockId,
    ) {
        if old_block == new_block {
            return;
        }
        let fluid_touched = fluid_kind(old_block).is_some() || fluid_kind(new_block).is_some();
        let solidity_changed = old_block.is_solid() != new_block.is_solid();
        if fluid_touched || solidity_changed {
            let _ = self.with_fluids_system_mut(|fluids, world| {
                fluids.mark_transforming_with_neighbors(world, world_pos);
            });
        }
    }

    pub(crate) fn fixed_sim_tick(&mut self, tick_dt: f32, tick_index: u64) {
        let active_set = &self.active_sim_columns;
        let is_active = |x: i32, z: i32| active_set.contains(&ColumnCoord { x, z });
        let active = ActiveSimColumns {
            is_active: &is_active,
        };
        for system in self.sim_systems.iter_mut() {
            system.fixed_tick(
                &mut self.world,
                tick_dt,
                tick_index,
                ActiveSimColumns {
                    is_active: active.is_active,
                },
            );
        }
    }

    pub(crate) fn update_fluid_simulation(&mut self, dt: f32) {
        self.frame_index = self.frame_index.wrapping_add(1);
        self.fluid_accum += dt.max(0.0);
        let max_catchup = FLUID_FIXED_STEP_SECONDS * FLUID_MAX_TICKS_PER_FRAME as f32;
        if self.fluid_accum > max_catchup {
            self.fluid_accum = max_catchup;
        }

        let mut changed_chunks_this_step: HashSet<ChunkCoord> = HashSet::new();
        let mut changed_positions: Vec<IVec3> = Vec::new();
        let mut steps_this_frame = 0usize;
        let frame_budget = Duration::from_secs_f32(FLUID_SIM_BUDGET_MS_PER_FRAME / 1000.0);
        let started = Instant::now();
        while self.fluid_accum >= FLUID_FIXED_STEP_SECONDS
            && steps_this_frame < FLUID_MAX_TICKS_PER_FRAME
        {
            if steps_this_frame > 0 && started.elapsed() >= frame_budget {
                break;
            }
            let tick_index = self.sim_tick_index;
            self.fixed_sim_tick(FLUID_FIXED_STEP_SECONDS, tick_index);
            self.sim_tick_index = self.sim_tick_index.wrapping_add(1);
            changed_positions.clear();
            let _ = self.with_fluids_system(|fluids| {
                fluids.drain_changed_positions_into(&mut changed_positions);
            });
            changed_chunks_this_step.clear();
            for pos in changed_positions.iter().copied() {
                for coord in Self::affected_chunks_for_block_change(pos) {
                    changed_chunks_this_step.insert(coord);
                }
            }
            for coord in changed_chunks_this_step.iter().copied() {
                self.note_fluid_chunk_changed(coord);
            }
            self.fluid_accum -= FLUID_FIXED_STEP_SECONDS;
            steps_this_frame += 1;
        }
        if let Some(counts) = self.with_fluids_system(|fluids| fluids.debug_counts()) {
            self.fluid_debug_counts = counts;
        }
        self.flush_fluid_mesh_coalescer();
    }

    pub(crate) fn fluid_debug_suffix(&self) -> String {
        let water_ready = self.fluid_debug_counts.water_scheduler.0;
        let water_due = self.fluid_debug_counts.water_scheduler.1;
        let lava_ready = self.fluid_debug_counts.lava_scheduler.0;
        let lava_due = self.fluid_debug_counts.lava_scheduler.1;
        let blocked_water = self.fluid_debug_counts.unloaded_writes.0;
        let blocked_lava = self.fluid_debug_counts.unloaded_writes.1;
        let fluid_mesh_pending = self
            .fluid_mesh_coalesce
            .values()
            .filter(|entry| entry.pending_dirty)
            .count();
        format!(
            "fluid W r/d={water_ready}/{water_due} L r/d={lava_ready}/{lava_due} blocked_unloaded W/L={blocked_water}/{blocked_lava} activeFluidChunks={} fluidMeshPending={fluid_mesh_pending}",
            self.fluid_mesh_coalesce.len()
        )
    }

    pub(crate) fn on_minecraft_anim_tick(&mut self) -> bool {
        let mut any_changed = false;
        for track in &mut self.texture_anim_tracks {
            if track.meta.sheet_frames <= 1 || track.meta.sequence.is_empty() {
                continue;
            }
            debug_assert_eq!(track.meta.axis, AnimAxis::Vertical);
            track.state.accum_ticks = track.state.accum_ticks.saturating_add(1);
            if track.state.accum_ticks < track.meta.frametime_ticks {
                continue;
            }
            track.state.accum_ticks = 0;
            track.state.seq_index =
                (track.state.seq_index + 1) % (track.meta.sequence.len() as u32);
            let next_frame = track.meta.sequence[track.state.seq_index as usize] as u32;
            let entry = &mut self.texture_anim_gpu[track.texture_id as usize];
            if entry.current_frame != next_frame {
                entry.current_frame = next_frame;
                any_changed = true;
            }
        }
        any_changed
    }

    pub(crate) fn update_texture_animations(&mut self, dt: f32) {
        self.texture_anim_accum += dt.max(0.0);
        let mut dirty = false;
        while self.texture_anim_accum >= MINECRAFT_TICK_SECONDS {
            dirty |= self.on_minecraft_anim_tick();
            self.texture_anim_accum -= MINECRAFT_TICK_SECONDS;
        }
        if dirty {
            self.queue.write_buffer(
                &self.texture_anim_buffer,
                0,
                bytemuck::cast_slice(&self.texture_anim_gpu),
            );
        }
    }

    pub(crate) fn destroy_block_at_camera_ray(&mut self, camera: &Camera) {
        let _trace_scope = perf_trace::scope("block_edit.destroy");
        let edit_started = Instant::now();
        let mut timings = BlockEditTimings::default();
        let mut slow_coord = None;

        let hit = {
            let _scope = perf_trace::scope("block_edit.raycast");
            let started = Instant::now();
            let hit = raycast_world(
                &self.world,
                camera.position,
                camera.forward(),
                RAYCAST_MAX_DISTANCE,
            );
            timings.raycast_ms = started.elapsed().as_secs_f32() * 1000.0;
            perf_trace::record_duration_ms(
                "block_edit.raycast",
                timings.raycast_ms,
                &format!("kind=destroy hit={}", hit.is_some() as u8),
            );
            hit
        };
        let Some(hit) = hit else {
            maybe_log_slow_block_edit(
                "destroy",
                edit_started.elapsed().as_secs_f32() * 1000.0,
                slow_coord,
                timings,
            );
            return;
        };
        slow_coord = Some(hit.hit_block_pos);

        let destroyed_block = self.world.get_block(hit.hit_block_pos);
        if !self.block_texture_lookup.is_breakable_for(destroyed_block) {
            maybe_log_slow_block_edit(
                "destroy",
                edit_started.elapsed().as_secs_f32() * 1000.0,
                slow_coord,
                timings,
            );
            return;
        }
        if cfg!(debug_assertions) {
            println!(
                "[debug] destroy: block={:?} pos=({}, {}, {})",
                destroyed_block, hit.hit_block_pos.x, hit.hit_block_pos.y, hit.hit_block_pos.z
            );
        }

        let chunk_coord = World::world_to_chunk_coord(hit.hit_block_pos);
        let chunk_existed_before = self.world.get_chunk(chunk_coord).is_some();
        let set_ok = {
            let _scope = perf_trace::scope("block_edit.set_block");
            let started = Instant::now();
            let result = self.world.set_block(hit.hit_block_pos, BlockId::Air);
            let chunk_existed_after = self.world.get_chunk(chunk_coord).is_some();
            let chunk_removed = chunk_existed_before && !chunk_existed_after;
            let chunk_created = !chunk_existed_before && chunk_existed_after;
            timings.set_block_ms = started.elapsed().as_secs_f32() * 1000.0;
            perf_trace::record_duration_ms(
                "block_edit.set_block",
                timings.set_block_ms,
                &format!(
                    "kind=destroy ok={} chunk=({}, {}, {}) created={} removed={}",
                    result.is_ok() as u8,
                    chunk_coord.x,
                    chunk_coord.y,
                    chunk_coord.z,
                    chunk_created as u8,
                    chunk_removed as u8
                ),
            );
            result.is_ok()
        };
        if set_ok {
            self.sync_generated_empty_chunk_marker(chunk_coord);
            self.enqueue_pending_block_edit(hit.hit_block_pos, destroyed_block, BlockId::Air);
        }

        maybe_log_slow_block_edit(
            "destroy",
            edit_started.elapsed().as_secs_f32() * 1000.0,
            slow_coord,
            timings,
        );
    }

    fn block_intersects_player_hitbox(block_pos: IVec3, player_feet_pos: Vec3) -> bool {
        let player_min = player_feet_pos + Vec3::new(-PLAYER_RADIUS, 0.0, -PLAYER_RADIUS);
        let player_max = player_feet_pos + Vec3::new(PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_RADIUS);

        let block_min = Vec3::new(block_pos.x as f32, block_pos.y as f32, block_pos.z as f32);
        let block_max = block_min + Vec3::ONE;

        block_min.x < player_max.x
            && block_max.x > player_min.x
            && block_min.y < player_max.y
            && block_max.y > player_min.y
            && block_min.z < player_max.z
            && block_max.z > player_min.z
    }

    pub(crate) fn place_block_at_camera_ray(
        &mut self,
        camera: &Camera,
        selected_block: BlockId,
        player_feet_pos: Vec3,
    ) {
        let _trace_scope = perf_trace::scope("block_edit.place");
        let edit_started = Instant::now();
        let mut timings = BlockEditTimings::default();
        let mut slow_coord = None;

        if selected_block == BlockId::Bedrock {
            maybe_log_slow_block_edit(
                "place",
                edit_started.elapsed().as_secs_f32() * 1000.0,
                slow_coord,
                timings,
            );
            return;
        }

        let hit = {
            let _scope = perf_trace::scope("block_edit.raycast");
            let started = Instant::now();
            let hit = raycast_world(
                &self.world,
                camera.position,
                camera.forward(),
                RAYCAST_MAX_DISTANCE,
            );
            timings.raycast_ms = started.elapsed().as_secs_f32() * 1000.0;
            perf_trace::record_duration_ms(
                "block_edit.raycast",
                timings.raycast_ms,
                &format!("kind=place hit={}", hit.is_some() as u8),
            );
            hit
        };
        let Some(hit) = hit else {
            maybe_log_slow_block_edit(
                "place",
                edit_started.elapsed().as_secs_f32() * 1000.0,
                slow_coord,
                timings,
            );
            return;
        };

        let place_pos = hit.place_pos();
        slow_coord = Some(place_pos);
        let target_block = self.world.get_block(place_pos);
        let can_replace_target = matches!(
            target_block,
            BlockId::Air | BlockId::WaterStill | BlockId::WaterFlowing
        );
        if target_block == BlockId::Bedrock || !can_replace_target {
            maybe_log_slow_block_edit(
                "place",
                edit_started.elapsed().as_secs_f32() * 1000.0,
                slow_coord,
                timings,
            );
            return;
        }
        if Self::block_intersects_player_hitbox(place_pos, player_feet_pos) {
            maybe_log_slow_block_edit(
                "place",
                edit_started.elapsed().as_secs_f32() * 1000.0,
                slow_coord,
                timings,
            );
            return;
        }
        if cfg!(debug_assertions) {
            println!(
                "[debug] place: block={:?} pos=({}, {}, {})",
                selected_block, place_pos.x, place_pos.y, place_pos.z
            );
        }

        let (block_to_place, block_data) = if let Some(kind) = fluid_kind(selected_block) {
            (make_still(kind), fluid_data_make(0, false, true))
        } else {
            (selected_block, 0)
        };
        let chunk_coord = World::world_to_chunk_coord(place_pos);
        let chunk_existed_before = self.world.get_chunk(chunk_coord).is_some();
        let set_ok = {
            let _scope = perf_trace::scope("block_edit.set_block");
            let started = Instant::now();
            let result = self
                .world
                .set_block_and_data(place_pos, block_to_place, block_data);
            let chunk_existed_after = self.world.get_chunk(chunk_coord).is_some();
            let chunk_removed = chunk_existed_before && !chunk_existed_after;
            let chunk_created = !chunk_existed_before && chunk_existed_after;
            timings.set_block_ms = started.elapsed().as_secs_f32() * 1000.0;
            perf_trace::record_duration_ms(
                "block_edit.set_block",
                timings.set_block_ms,
                &format!(
                    "kind=place ok={} chunk=({}, {}, {}) created={} removed={}",
                    result.is_ok() as u8,
                    chunk_coord.x,
                    chunk_coord.y,
                    chunk_coord.z,
                    chunk_created as u8,
                    chunk_removed as u8
                ),
            );
            result.is_ok()
        };
        if set_ok {
            self.sync_generated_empty_chunk_marker(chunk_coord);
            self.enqueue_pending_block_edit(place_pos, target_block, block_to_place);
        }

        maybe_log_slow_block_edit(
            "place",
            edit_started.elapsed().as_secs_f32() * 1000.0,
            slow_coord,
            timings,
        );
    }

    pub(crate) fn clear_targeting_visuals(&mut self) {
        self.highlight_mesh = None;
        self.outline_target_block = None;
    }

    fn sky_clear_color_rgb(&self) -> [f32; 3] {
        let t = self.world_time.sky_brightness.clamp(0.0, 1.0);
        let day = [0.52, 0.72, 1.00];
        let night = [0.02, 0.04, 0.10];
        let mut color = lerp3(night, day, t);

        let sun_curve = (self.world_time.celestial_angle * std::f32::consts::TAU)
            .cos()
            .abs();
        let horizon = (1.0 - sun_curve).clamp(0.0, 1.0);
        let twilight = horizon.powf(1.8) * (1.0 - (t * 2.0 - 1.0).abs()).clamp(0.0, 1.0);
        color = lerp3(color, [0.96, 0.52, 0.22], twilight * 0.16);
        color
    }

    fn sky_clear_color(&self) -> wgpu::Color {
        let rgb = self.sky_clear_color_rgb();
        wgpu::Color {
            r: rgb[0] as f64,
            g: rgb[1] as f64,
            b: rgb[2] as f64,
            a: 1.0,
        }
    }

    fn moon_phase_uv_rect(&self) -> [f32; 4] {
        let phase = self.world_time.phase as u32;
        let tile_x = phase % 4;
        let tile_y = phase / 4;
        let moon_w = self.moon_phase_atlas_size[0].max(1) as f32;
        let moon_h = self.moon_phase_atlas_size[1].max(1) as f32;
        let inset_u = 0.5 / moon_w;
        let inset_v = 0.5 / moon_h;
        let u0 = tile_x as f32 / 4.0 + inset_u;
        let u1 = (tile_x + 1) as f32 / 4.0 - inset_u;
        let v0 = tile_y as f32 / 2.0 + inset_v;
        let v1 = (tile_y + 1) as f32 / 2.0 - inset_v;
        [u0, v0, u1, v1]
    }

    fn write_sky_object_vertices(&mut self, camera: &Camera) {
        let sky_rot = Mat4::from_rotation_y(-std::f32::consts::FRAC_PI_2)
            * Mat4::from_rotation_x(self.world_time.celestial_angle * std::f32::consts::TAU);
        let camera_pos = camera.position;
        let moon_uv = self.moon_phase_uv_rect();
        let sun_color = [self.world_time.sun_brightness; 3];
        let moon_strength = (1.0 - self.world_time.sky_brightness).clamp(0.15, 1.0);
        let moon_color = [moon_strength; 3];

        let mut vertices = [Vertex {
            position: [0.0, 0.0, 0.0],
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: TINT_NONE,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: [1.0, 1.0, 1.0],
        }; SKY_OBJECT_VERTEX_COUNT];

        let mut write_quad = |base: usize,
                              size: f32,
                              local_y: f32,
                              tex_layer: u32,
                              uv: [f32; 4],
                              color: [f32; 3]| {
            let hs = size * 0.5;
            let local = [
                Vec3::new(-hs, local_y, -hs),
                Vec3::new(hs, local_y, -hs),
                Vec3::new(hs, local_y, hs),
                Vec3::new(-hs, local_y, hs),
            ];
            let quad_uvs = [
                [uv[0], uv[3]],
                [uv[2], uv[3]],
                [uv[2], uv[1]],
                [uv[0], uv[1]],
            ];
            for i in 0..4 {
                let p = camera_pos + (sky_rot * local[i].extend(1.0)).truncate();
                vertices[base + i] = Vertex {
                    position: p.to_array(),
                    uv: quad_uvs[i],
                    tex_layer,
                    tint_kind: TINT_NONE,
                    light_packed: FULLBRIGHT_LIGHT_PACKED,
                    color,
                };
            }
        };

        write_quad(
            0,
            SKY_SUN_QUAD_SIZE,
            SKY_SUN_DISTANCE,
            self.sky_sun_layer,
            self.sky_sun_uv_rect,
            sun_color,
        );
        write_quad(
            4,
            SKY_MOON_QUAD_SIZE,
            -SKY_MOON_DISTANCE,
            self.sky_moon_layer,
            moon_uv,
            moon_color,
        );

        self.queue.write_buffer(
            &self.sky_object_vertex_buffer,
            0,
            bytemuck::cast_slice(&vertices),
        );
    }

    pub(crate) fn update_face_highlight(&mut self, camera: &Camera, _blink_visible: bool) {
        let Some(hit) = raycast_world(
            &self.world,
            camera.position,
            camera.forward(),
            RAYCAST_MAX_DISTANCE,
        ) else {
            self.clear_targeting_visuals();
            return;
        };

        if hit.hit_normal == IVec3::ZERO {
            self.clear_targeting_visuals();
            return;
        }

        self.outline_target_block = Some(hit.hit_block_pos);
        self.highlight_mesh = None;
    }

    /// Update: Kamera-Uniform in GPU-Buffer schreiben
    pub(crate) fn update_camera_uniform(
        &mut self,
        camera: &Camera,
        underwater_kind: Option<FluidKind>,
        overlay_alpha: f32,
        world_time: &WorldTime,
    ) {
        let aspect = (self.config.width as f32 / self.config.height as f32).max(0.0001);
        let fog_radius_chunks = self.view_distance_chunks;
        let renderdist_end = (fog_radius_chunks as f32 * CHUNK_SIZE as f32
            - FOG_RENDERDIST_END_MARGIN_BLOCKS)
            .max(8.0);
        let renderdist_start =
            (renderdist_end * FOG_RENDERDIST_START_RATIO).clamp(2.0, renderdist_end - 1.0);
        self.camera_uniform.update(
            camera,
            aspect,
            underwater_kind,
            overlay_alpha,
            Some((renderdist_start, renderdist_end)),
        );
        self.camera_uniform.sky_params[0] = world_time.sky_brightness.clamp(0.0, 1.0);
        self.camera_uniform.sky_params[1] = self.debug_lighting_view.shader_mode_f32();
        if underwater_kind.is_none() {
            let sky_rgb = self.sky_clear_color_rgb();
            let mix = lerp(0.18, 0.55, world_time.sky_brightness.clamp(0.0, 1.0));
            self.camera_uniform.fog_color[0] =
                lerp(self.camera_uniform.fog_color[0], sky_rgb[0], mix);
            self.camera_uniform.fog_color[1] =
                lerp(self.camera_uniform.fog_color[1], sky_rgb[1], mix);
            self.camera_uniform.fog_color[2] =
                lerp(self.camera_uniform.fog_color[2], sky_rgb[2], mix);
            self.camera_uniform.fog_params[2] *= lerp(1.20, 0.85, world_time.sky_brightness);

            let cfg = self.world.config();
            if cfg.boundary_ocean() {
                let inner = cfg.boundary_radius_chunks() as f32 * CHUNK_SIZE as f32;
                let boundary_fog_end = (inner + BOUNDARY_OCEAN_OUTER_PADDING_BLOCKS * 0.55).max(32.0);
                let boundary_fog_start = (boundary_fog_end * 0.72).max(8.0);
                self.camera_uniform.fog_params[0] =
                    self.camera_uniform.fog_params[0].min(boundary_fog_start);
                self.camera_uniform.fog_params[1] =
                    self.camera_uniform.fog_params[1].min(boundary_fog_end);
                self.camera_uniform.fog_params[2] *= 1.25;
            }
        }

        // COPY_DST erlaubt uns, pro Frame per queue.write_buffer zu aktualisieren.
        self.queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::bytes_of(&self.camera_uniform),
        );
    }

    fn update_preview_camera_uniform(&mut self) {
        let mut preview_camera = Camera::new(Vec3::new(
            PREVIEW_3D_CAMERA_POS[0],
            PREVIEW_3D_CAMERA_POS[1],
            PREVIEW_3D_CAMERA_POS[2],
        ));
        preview_camera.yaw = PREVIEW_3D_CAMERA_YAW_RAD;
        preview_camera.pitch = PREVIEW_3D_CAMERA_PITCH_RAD;
        preview_camera.fov_y_radians = PREVIEW_3D_FOV_Y_RAD;
        preview_camera.z_near = 0.1;
        preview_camera.z_far = 16.0;

        self.preview_camera_uniform.view_proj =
            build_view_proj_wgpu(&preview_camera, 1.0).to_cols_array_2d();
        self.preview_camera_uniform.camera_pos = preview_camera.position.to_array();
        self.preview_camera_uniform._pad0 = 0.0;
        // Disable fog for the HUD preview.
        self.preview_camera_uniform.fog_params = [10_000.0, 10_001.0, 0.0, 0.0];
        self.preview_camera_uniform.fog_color = [0.0, 0.0, 0.0, 0.0];

        self.queue.write_buffer(
            &self.preview_camera_buffer,
            0,
            bytemuck::bytes_of(&self.preview_camera_uniform),
        );
    }

    pub(crate) fn render(
        &mut self,
        camera: &Camera,
        underwater_kind: Option<FluidKind>,
        generation_overlay: Option<GenerationOverlay<'_>>,
        pause_overlay: Option<PauseOverlay<'_>>,
        debug_overlay: Option<DebugOverlay<'_>>,
    ) -> Result<(), SurfaceError> {
        let _trace_scope = perf_trace::scope("state.render");
        let render_started = Instant::now();
        let frame = self.surface.get_current_texture()?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let aspect = (self.config.width as f32 / self.config.height as f32).max(0.0001);
        let frustum = Frustum::from_view_proj(build_view_proj_wgpu(camera, aspect));
        let (camera_chunk_x, _, camera_chunk_z) = Self::camera_chunk_coords(camera);
        let near_mesh_radius = self.near_mesh_radius_chunks();
        let (lod_near_radius, lod_far_radius) = self.lod_ring_radii_chunks();
        let mut visible_lod_columns: Vec<(LodColumnCoord, f32)> = Vec::with_capacity(256);
        if self.far_lod_enabled {
            let lod_near_radius_sq = lod_near_radius * lod_near_radius;
            let lod_far_radius_sq = lod_far_radius * lod_far_radius;
            visible_lod_columns.reserve(self.lod_meshes.len());
            for coord in self.lod_meshes.keys().copied() {
                let dx = coord.x - camera_chunk_x;
                let dz = coord.z - camera_chunk_z;
                let dist2_chunks = dx * dx + dz * dz;
                if dist2_chunks <= lod_near_radius_sq || dist2_chunks > lod_far_radius_sq {
                    continue;
                }
                let center = Vec3::new(
                    coord.x as f32 * CHUNK_SIZE as f32 + CHUNK_HALF_EXTENT_F32,
                    (WORLD_MAX_Y as f32 + 1.0) * 0.5,
                    coord.z as f32 * CHUNK_SIZE as f32 + CHUNK_HALF_EXTENT_F32,
                );
                if !sphere_in_frustum(&frustum.planes, center, LOD_BOUNDING_SPHERE_RADIUS) {
                    continue;
                }
                visible_lod_columns.push((coord, center.distance_squared(camera.position)));
            }
            visible_lod_columns.sort_by(|(coord_a, dist_a), (coord_b, dist_b)| {
                dist_a
                    .total_cmp(dist_b)
                    .then_with(|| coord_a.x.cmp(&coord_b.x))
                    .then_with(|| coord_a.z.cmp(&coord_b.z))
            });
        }
        let mut visible_chunks: Vec<(ChunkCoord, f32)> =
            Vec::with_capacity(self.chunk_meshes.len());
        for coord in self.chunk_meshes.keys().copied() {
            if !Self::is_chunk_within_radius_xz(
                coord,
                camera_chunk_x,
                camera_chunk_z,
                near_mesh_radius,
            ) {
                continue;
            }
            let center = Vec3::new(
                coord.x as f32 * CHUNK_SIZE as f32 + CHUNK_HALF_EXTENT_F32,
                coord.y as f32 * CHUNK_SIZE as f32 + CHUNK_HALF_EXTENT_F32,
                coord.z as f32 * CHUNK_SIZE as f32 + CHUNK_HALF_EXTENT_F32,
            );
            if !sphere_in_frustum(&frustum.planes, center, CHUNK_BOUNDING_SPHERE_RADIUS) {
                continue;
            }
            visible_chunks.push((coord, center.distance_squared(camera.position)));
        }
        visible_chunks.sort_by(|(coord_a, dist_a), (coord_b, dist_b)| {
            dist_a
                .total_cmp(dist_b)
                .then_with(|| coord_a.x.cmp(&coord_b.x))
                .then_with(|| coord_a.y.cmp(&coord_b.y))
                .then_with(|| coord_a.z.cmp(&coord_b.z))
        });
        let mut transparent_chunks = visible_chunks.clone();
        transparent_chunks.sort_by(|(coord_a, dist_a), (coord_b, dist_b)| {
            dist_b
                .total_cmp(dist_a)
                .then_with(|| coord_b.x.cmp(&coord_a.x))
                .then_with(|| coord_b.y.cmp(&coord_a.y))
                .then_with(|| coord_b.z.cmp(&coord_a.z))
        });
        let mut lod_drawcalls = 0usize;
        let mut world_drawcalls = 0usize;

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
                    vertex_capacity_bytes: std::mem::size_of_val(vertices.as_slice()) as u64,
                    index_capacity_bytes: std::mem::size_of_val(indices.as_slice()) as u64,
                })
            }
        };
        let generating_overlay_mesh = if let Some(overlay) = generation_overlay {
            let (vertices, indices) = build_generating_screen_mesh(
                self.config.width,
                self.config.height,
                self.worldgen_dirt_layer,
                &self.worldgen_font,
                overlay.progress01,
                overlay.phase,
            );
            let vertex_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Generating Overlay Vertex Buffer"),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            let index_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Generating Overlay Index Buffer"),
                    contents: bytemuck::cast_slice(&indices),
                    usage: wgpu::BufferUsages::INDEX,
                });
            Some(ChunkMesh {
                vertex_buffer,
                index_buffer,
                index_count: indices.len() as u32,
                vertex_capacity_bytes: std::mem::size_of_val(vertices.as_slice()) as u64,
                index_capacity_bytes: std::mem::size_of_val(indices.as_slice()) as u64,
            })
        } else {
            None
        };
        let pause_overlay_mesh = if let Some(overlay) = pause_overlay {
            let (vertices, indices) = build_pause_menu_mesh(
                self.config.width,
                self.config.height,
                self.worldgen_dirt_layer,
                &self.worldgen_font,
                overlay.selected,
                overlay.items,
            );
            let vertex_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Pause Overlay Vertex Buffer"),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            let index_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Pause Overlay Index Buffer"),
                    contents: bytemuck::cast_slice(&indices),
                    usage: wgpu::BufferUsages::INDEX,
                });
            Some(ChunkMesh {
                vertex_buffer,
                index_buffer,
                index_count: indices.len() as u32,
                vertex_capacity_bytes: std::mem::size_of_val(vertices.as_slice()) as u64,
                index_capacity_bytes: std::mem::size_of_val(indices.as_slice()) as u64,
            })
        } else {
            None
        };
        let showing_generation_screen = generating_overlay_mesh.is_some();
        let showing_pause_menu = pause_overlay_mesh.is_some();
        let debug_overlay_mesh = if !showing_generation_screen && !showing_pause_menu {
            if let Some(overlay) = debug_overlay {
                let (vertices, indices) = build_debug_overlay_mesh(
                    self.config.width,
                    self.config.height,
                    &self.worldgen_font,
                    overlay.lines,
                );
                let vertex_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("Debug Overlay Vertex Buffer"),
                            contents: bytemuck::cast_slice(&vertices),
                            usage: wgpu::BufferUsages::VERTEX,
                        });
                let index_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("Debug Overlay Index Buffer"),
                            contents: bytemuck::cast_slice(&indices),
                            usage: wgpu::BufferUsages::INDEX,
                        });
                Some(ChunkMesh {
                    vertex_buffer,
                    index_buffer,
                    index_count: indices.len() as u32,
                    vertex_capacity_bytes: std::mem::size_of_val(vertices.as_slice()) as u64,
                    index_capacity_bytes: std::mem::size_of_val(indices.as_slice()) as u64,
                })
            } else {
                None
            }
        } else {
            None
        };
        let show_boundary_ocean =
            !showing_generation_screen && !showing_pause_menu && self.world.config().boundary_ocean();
        let outline_target_block = if showing_generation_screen || showing_pause_menu {
            None
        } else {
            self.outline_target_block
        };
        if let Some(target_block) = outline_target_block {
            let outline_vertices = build_outline_line_vertices(target_block);
            self.queue.write_buffer(
                &self.outline_vertex_buffer,
                0,
                bytemuck::cast_slice(&outline_vertices),
            );
        }
        let show_preview_3d = !showing_generation_screen && !showing_pause_menu;
        if show_preview_3d {
            self.update_preview_camera_uniform();
        }
        self.write_sky_object_vertices(camera);

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
                        load: wgpu::LoadOp::Clear(self.sky_clear_color()),
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

            rpass.set_pipeline(&self.sky_pipeline);
            rpass.set_bind_group(0, &self.sky_texture_bind_group, &[]);
            rpass.set_bind_group(1, &self.camera_bind_group, &[]);
            rpass.set_vertex_buffer(0, self.sky_object_vertex_buffer.slice(..));
            rpass.set_index_buffer(
                self.sky_object_index_buffer.slice(..),
                wgpu::IndexFormat::Uint32,
            );
            rpass.draw_indexed(0..SKY_OBJECT_INDEX_COUNT, 0, 0..1);

            if self.far_lod_enabled {
                // Erst Far-LOD, dann Near-Chunkmeshes.
                rpass.set_pipeline(&self.render_pipeline_lod);
                rpass.set_bind_group(0, &self.camera_bind_group, &[]);
                for (coord, _) in &visible_lod_columns {
                    let Some(mesh) = self.lod_meshes.get(coord) else {
                        continue;
                    };
                    rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                    rpass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                    rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
                    lod_drawcalls += 1;
                }
            }

            // Danach Welt aus echten Chunk-Meshes.
            rpass.set_pipeline(self.world_pipeline());
            rpass.set_bind_group(0, &self.world_texture_bind_group, &[]);
            rpass.set_bind_group(1, &self.camera_bind_group, &[]);
            for (coord, _) in &visible_chunks {
                let Some(layered) = self.chunk_meshes.get(coord) else {
                    continue;
                };
                if let Some(mesh) = &layered.opaque {
                    rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                    rpass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                    rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
                    world_drawcalls += 1;
                }
            }
            for (coord, _) in &visible_chunks {
                let Some(layered) = self.chunk_meshes.get(coord) else {
                    continue;
                };
                if let Some(mesh) = &layered.cutout {
                    rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                    rpass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                    rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
                    world_drawcalls += 1;
                }
            }

            rpass.set_pipeline(&self.render_pipeline_transparent);
            rpass.set_bind_group(0, &self.world_texture_bind_group, &[]);
            rpass.set_bind_group(1, &self.camera_bind_group, &[]);
            if show_boundary_ocean {
                if let Some(mesh) = self.boundary_ocean_mesh.as_ref() {
                    rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                    rpass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                    rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
                    world_drawcalls += 1;
                }
            }
            for (coord, _) in &transparent_chunks {
                let Some(layered) = self.chunk_meshes.get(coord) else {
                    continue;
                };
                let Some(mesh) = layered.transparent.as_ref() else {
                    continue;
                };
                rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                rpass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
                world_drawcalls += 1;
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

            if outline_target_block.is_some() {
                rpass.set_pipeline(&self.outline_pipeline);
                rpass.set_bind_group(0, &self.camera_bind_group, &[]);
                rpass.set_vertex_buffer(0, self.outline_vertex_buffer.slice(..));
                rpass.draw(0..BLOCK_OUTLINE_VERTEX_COUNT, 0..1);
            }

            if show_preview_3d {
                let (vx, vy, vw, vh) =
                    preview_viewport_rect_px(self.config.width, self.config.height);
                rpass.set_viewport(vx as f32, vy as f32, vw as f32, vh as f32, 0.0, 1.0);
                rpass.set_scissor_rect(vx, vy, vw, vh);
                let preview_pipeline = match self.block_texture_lookup.model_for(self.preview_block)
                {
                    BlockModel::CrossPlant => &self.render_pipeline_no_cull_no_depth,
                    BlockModel::Cube | BlockModel::Liquid { .. } => {
                        &self.render_pipeline_back_no_depth
                    }
                };
                rpass.set_pipeline(preview_pipeline);
                rpass.set_bind_group(0, &self.world_texture_bind_group, &[]);
                rpass.set_bind_group(1, &self.preview_camera_bind_group, &[]);
                rpass.set_vertex_buffer(0, self.preview_mesh.vertex_buffer.slice(..));
                rpass.set_index_buffer(
                    self.preview_mesh.index_buffer.slice(..),
                    wgpu::IndexFormat::Uint32,
                );
                rpass.draw_indexed(0..self.preview_mesh.index_count, 0, 0..1);
                rpass.set_viewport(
                    0.0,
                    0.0,
                    self.config.width as f32,
                    self.config.height as f32,
                    0.0,
                    1.0,
                );
                rpass.set_scissor_rect(0, 0, self.config.width, self.config.height);
            }

            // 2D-UI in NDC: Crosshair bleibt stabil in der Screen-Mitte.
            rpass.set_pipeline(&self.ui_pipeline);
            rpass.set_bind_group(1, &self.camera_bind_group, &[]);

            if underwater_kind == Some(FluidKind::Water)
                && !showing_generation_screen
                && !showing_pause_menu
            {
                rpass.set_bind_group(0, &self.world_texture_bind_group, &[]);
                rpass.set_vertex_buffer(0, self.water_overlay_mesh.vertex_buffer.slice(..));
                rpass.set_index_buffer(
                    self.water_overlay_mesh.index_buffer.slice(..),
                    wgpu::IndexFormat::Uint32,
                );
                rpass.draw_indexed(0..self.water_overlay_mesh.index_count, 0, 0..1);
            }

            if !showing_generation_screen && !showing_pause_menu {
                rpass.set_bind_group(0, &self.world_texture_bind_group, &[]);
                rpass.set_vertex_buffer(0, self.crosshair_mesh.vertex_buffer.slice(..));
                rpass.set_index_buffer(
                    self.crosshair_mesh.index_buffer.slice(..),
                    wgpu::IndexFormat::Uint32,
                );
                rpass.draw_indexed(0..self.crosshair_mesh.index_count, 0, 0..1);
            }

            if let Some(mesh) = &generating_overlay_mesh {
                rpass.set_bind_group(0, &self.worldgen_ui_texture_bind_group, &[]);
                rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                rpass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
            } else if let Some(mesh) = &pause_overlay_mesh {
                rpass.set_bind_group(0, &self.worldgen_ui_texture_bind_group, &[]);
                rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                rpass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
            } else if let Some(mesh) = &debug_overlay_mesh {
                rpass.set_bind_group(0, &self.worldgen_ui_texture_bind_group, &[]);
                rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                rpass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
            }
        }

        self.queue.submit(Some(encoder.finish()));
        frame.present();
        if self.last_render_stats_report.elapsed() >= RENDER_STATS_REPORT_INTERVAL {
            println!(
                "render stats: visible_chunks={} visible_lod={} world_drawcalls={} lod_drawcalls={}",
                visible_chunks.len(),
                visible_lod_columns.len(),
                world_drawcalls,
                lod_drawcalls
            );
            eprintln!(
                "[stream][summary] burst={} missing_meshes_near_camera={} mesh_upload_queue_len={} dirty_chunk_queue_len={} chunkgen_results_len={} inflight_mesh={} inflight_chunkgen={} inflight_lod={} budget_dispatch_ms={:.2}/{:.2} budget_mesh_upload_ms={:.2}/{:.2} budget_chunkgen_apply_ms={:.2}/{:.2}",
                self.frame_perf.stream_burst_mode_active || self.world_generation_task.is_some(),
                self.frame_perf.stream_missing_meshes_near_camera,
                self.mesh_upload_queue.len(),
                self.dirty_chunk_queue.len(),
                self.chunkgen_results.len(),
                self.inflight.len(),
                self.chunkgen_inflight.len(),
                self.lod_chunkgen_inflight.len(),
                self.frame_perf.stream_dispatch_used_ms,
                self.frame_perf.stream_dispatch_budget_ms,
                self.frame_perf.stream_mesh_upload_used_ms,
                self.frame_perf.stream_mesh_upload_budget_ms,
                self.frame_perf.stream_chunkgen_apply_used_ms,
                self.frame_perf.stream_chunkgen_apply_budget_ms,
            );
            self.last_render_stats_report = Instant::now();
        }
        let render_ms = render_started.elapsed().as_secs_f32() * 1000.0;
        perf_trace::record_duration_ms(
            "state.render.total",
            render_ms,
            &format!(
                "visible_chunks={} visible_lod={} world_drawcalls={} lod_drawcalls={}",
                visible_chunks.len(),
                visible_lod_columns.len(),
                world_drawcalls,
                lod_drawcalls
            ),
        );
        let _ = perf_trace::append_gpu_csv_line(
            "frame_index,cpu_render_ms,gpu_timestamps_supported,terrain_drawcalls,lod_drawcalls",
            &format!(
                "{},{:.3},false,{},{}",
                self.frame_index, render_ms, world_drawcalls, lod_drawcalls
            ),
        );
        Ok(())
    }

    pub(crate) fn frustum_culling_stats(&self) -> (u64, u64) {
        (
            self.frustum_culled_chunks_total,
            self.meshes_skipped_due_to_frustum,
        )
    }

    pub(crate) fn lighting_step_last_ms(&self) -> f32 {
        self.last_lighting_step_ms
    }

    pub(crate) fn simulation_debug_line(&self) -> String {
        let entered = self.sim_columns_entered.len();
        let exited = self.sim_columns_exited.len();
        let ticking_areas = self.world.ticking_areas();
        let total_ticking_areas = ticking_areas.len();
        let enabled_ticking_areas = ticking_areas
            .iter()
            .filter(|area| area.flags.any_tick())
            .count();
        format!(
            "SimDist: {} ({}) active_cols={} entered={} exited={} ticking_areas={}/{}",
            self.simulation_distance.label(),
            self.simulation_distance_chunks,
            self.active_sim_columns.len(),
            entered,
            exited,
            enabled_ticking_areas,
            total_ticking_areas
        )
    }

    pub(crate) fn ticking_areas_debug_line(&self) -> String {
        let areas = self.world.ticking_areas();
        let mut unique_cols: HashSet<ColumnCoord> = HashSet::new();
        for area in areas {
            if !Self::ticking_area_requires_loading(area) {
                continue;
            }
            self.for_each_ticking_area_column(area, |col| {
                if !unique_cols.insert(col) {
                    return;
                }
                let mut fully_loaded = true;
                for cy in self.world.min_chunk_y()..=self.world.max_chunk_y() {
                    let coord = ChunkCoord {
                        x: col.x,
                        y: cy,
                        z: col.z,
                    };
                    let loaded = self.world.get_chunk(coord).is_some()
                        || self.generated_empty_chunks.contains(&coord);
                    if !loaded {
                        fully_loaded = false;
                        break;
                    }
                }
                if !fully_loaded {
                    unique_cols.remove(&col);
                }
            });
        }
        let loaded_cols = unique_cols.len();
        format!(
            "Ticking Areas: {} (loaded columns: {})",
            areas.len(),
            loaded_cols
        )
    }

    pub(crate) fn sim_systems_debug_line(&self) -> String {
        if self.sim_systems.is_empty() {
            return "SimSystems active: none".to_string();
        }
        let names = self
            .sim_systems
            .iter()
            .map(|sys| sys.name())
            .collect::<Vec<_>>()
            .join(", ");
        let blocked_water = self.fluid_debug_counts.unloaded_writes.0;
        let blocked_lava = self.fluid_debug_counts.unloaded_writes.1;
        if let Some(len) = self.fluids_bootstrap_queue_len() {
            format!(
                "SimSystems active: {names} | Fluids bootstrap_queue={len} blocked_unloaded W/L={blocked_water}/{blocked_lava}"
            )
        } else {
            format!(
                "SimSystems active: {names} | Fluids blocked_unloaded W/L={blocked_water}/{blocked_lava}"
            )
        }
    }

    pub(crate) fn block_entities_debug_line(&self) -> String {
        if let Some((total, active_chunks)) = self.block_entities_debug_counts() {
            return format!("BlockEntities: total={total} active_chunks={active_chunks}");
        }
        "BlockEntities: total=0 active_chunks=0".to_string()
    }

    pub(crate) fn random_ticks_debug_line(&self) -> String {
        if let Some((speed, sampled, callbacks)) = self.random_ticks_debug_counters() {
            return format!("RandomTicks: speed={speed} sampled={sampled} callbacks={callbacks}");
        }
        "RandomTicks: speed=0 sampled=0 callbacks=0".to_string()
    }

    pub(crate) fn world_manifest_debug_line(&self) -> String {
        let manifest = self.world.manifest();
        let missing_preview = self
            .world
            .manifest_missing_block_keys()
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "WorldManifest: v{} signature={} missing={}{}",
            manifest.format_version,
            manifest.short_signature(),
            self.world.manifest_missing_blocks_count(),
            if missing_preview.is_empty() {
                "".to_string()
            } else {
                format!(" [{}]", missing_preview)
            }
        )
    }

    pub(crate) fn lighting_pending_debug_line(&self) -> String {
        let now = Instant::now();
        let pending = self.lighting_request_pending
            || self.lighting_job_inflight
            || !self.pending_lighting_dirty_chunks.is_empty();
        let debounce_ms_left = self
            .lighting_rebuild_not_before
            .map(|t| t.saturating_duration_since(now).as_millis())
            .unwrap_or(0);
        format!(
            "Lighting Pending: {} (req={} inflight={} dirty_q={} result_q={} debounce={}ms)",
            if pending { "yes" } else { "no" },
            self.lighting_request_pending,
            self.lighting_job_inflight,
            self.pending_lighting_dirty_chunks.len(),
            self.lighting_results.len(),
            debounce_ms_left
        )
    }
}
