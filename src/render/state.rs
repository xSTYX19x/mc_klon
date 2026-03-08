use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::f32::consts::PI;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytemuck::{Pod, Zeroable};
use crossbeam_channel::{
    Receiver as CrossbeamReceiver, Sender as CrossbeamSender, TrySendError as CrossbeamTrySendError,
};
use glam::{IVec3, Mat3, Mat4, Vec3};
use wgpu::SurfaceError;
use wgpu::util::DeviceExt;
use winit::window::Window;

use super::chunkgen_worker::{
    ChunkGenJob, ChunkGenRequestKind, ChunkGenResult, ChunkGenWorkerPool,
    GeneratedFluidBootstrapCell, GeneratedWorldgenDebugStats, LodColumnCoord,
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
    BlockId, BlockModel, BlockRegistry, BlockTextureLookup, Face, REQUIRED_OVERLAY_TEXTURE_KEYS,
    RenderLayer, TEX_BEDROCK, TEX_DIRT, TEX_GRASS_SIDE, TEX_GRASS_TOP, TEX_OAK_PLANKS, TEX_SAPLING,
    TEX_STONE, TEX_WATER_OVERLAY, TEXTURE_SELF_TEST_KEYS, TINT_NONE,
    debug_assert_block_blueprint_integrity,
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
    SimulationDistance, UI_MODE_SOLID, UI_MODE_SOLID_ALPHA, UI_MODE_TEXTURED,
    UI_MODE_WATER_OVERLAY, USE_GREEDY_MESHING, WATER_OVERLAY_TINT, WATER_OVERLAY_UV_TILE,
};
use crate::config::sim::RANDOM_TICK_SPEED_DEFAULT;
use crate::meshing::{CpuMeshData, FULLBRIGHT_LIGHT_PACKED, Vertex};
use crate::perf::trace as perf_trace;
use crate::player::PlayerMode;
use crate::sim::{ActiveSimColumns, ColumnCoord, SimSystem};
use crate::storage::chunk_codec::{
    ColumnBlob, ColumnSaveCompletion, ColumnSaveSectionSnapshot, ColumnSaveSnapshot,
    decode_chunk_from_section_blob, decode_column, validate_column_for_world,
};
use crate::storage::format::{
    SAVE_ROOT_DIR, SECTION_BLOCK_COUNT, TICKING_AREAS_FILE_NAME, WORLD_MANIFEST_FILE_NAME,
    WORLD_META_FILE_NAME,
};
use crate::storage::world_meta::{self, WorldMeta};
use crate::storage::{
    RegionStorage, StorageColumnLoadStatus, StorageColumnSaveResult, StorageLoadWorker,
    StorageWorker, WorldStorage,
};
use crate::texture::{TextureArray, load_png_rgba};
use crate::texture_registry::TextureRegistry;
use crate::world::block_entities_system::BlockEntitiesSystem;
use crate::world::fluids::{
    FLUID_FIXED_STEP_SECONDS, FLUID_MAX_TICKS_PER_FRAME, FluidDebugCounts, FluidKind,
    fluid_data_is_falling, fluid_data_is_source_origin, fluid_data_is_worldgen_static,
    fluid_data_level, fluid_data_make, fluid_kind, make_still,
};
use crate::world::fluids_system::FluidsSystem;
use crate::world::manifest::{IdMapper, WorldManifest};
use crate::world::random_ticks_system::RandomTicksSystem;
use crate::world::raycast::{RAYCAST_MAX_DISTANCE, raycast_world};
use crate::world::ticking_areas::{self, TickingArea, TickingFlags};
use crate::world::time::WorldTime;
use crate::world::{
    CHUNK_SIZE, Chunk, ChunkCoord, IncrementalLightingResult, IncrementalLightingStepLimits,
    IncrementalLightingTask, LightLevel, LightingBlockChange, SetBlockError, WORLD_MAX_Y, World,
    WorldConfig,
};
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeferredEditKind {
    PlaceBlock,
    PlaceFluid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChunkOrigin {
    Generated,
    Loaded,
    UserOnly,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DeferredBlockEdit {
    pub(crate) world_pos: IVec3,
    pub(crate) block: BlockId,
    pub(crate) data: u8,
    pub(crate) frame_index: u64,
    pub(crate) kind: DeferredEditKind,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ApplyEditsStats {
    pub(crate) total: usize,
    pub(crate) applied: usize,
    pub(crate) applied_fluid: usize,
    pub(crate) skipped_out_of_bounds: usize,
    pub(crate) skipped_conflict: usize,
    pub(crate) skipped_unchanged: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SavedLoadApplyPhase {
    PrepareSections,
    ApplyColumn,
    ReplayDeferredEdits,
    FinalizeSections,
    LightingRemoved,
    LightingAdded,
    EnqueueSecondaryWork,
    MarkBoundaryDirty,
    Finish,
}

impl SavedLoadApplyPhase {
    fn label(self) -> &'static str {
        match self {
            Self::PrepareSections => "prepare",
            Self::ApplyColumn => "commit",
            Self::ReplayDeferredEdits => "deferred",
            Self::FinalizeSections => "finalize",
            Self::LightingRemoved => "light_remove",
            Self::LightingAdded => "light_add",
            Self::EnqueueSecondaryWork => "secondary",
            Self::MarkBoundaryDirty => "boundary",
            Self::Finish => "finish",
        }
    }
}

#[derive(Debug, Default)]
struct SavedLoadApplyTaskStats {
    prepare_ms: f32,
    commit_ms: f32,
    deferred_ms: f32,
    finalize_ms: f32,
    lighting_ms: f32,
    secondary_ms: f32,
    sections_prepared: usize,
    sections_finalized: usize,
    deferred_batches: usize,
    deferred_total: usize,
    deferred_applied: usize,
    deferred_applied_fluid: usize,
    deferred_chunks_touched: usize,
    removed_lighting_batches: usize,
    added_lighting_batches: usize,
    fluid_enqueues: usize,
    geometry_dirty_marks: usize,
    boundary_dirty_marks: usize,
    yields: usize,
    resumes: usize,
}

#[derive(Debug)]
struct SavedLoadApplyTask {
    cx: i32,
    cz: i32,
    min_section_y: i32,
    max_section_y: i32,
    section_presence_mask: Vec<bool>,
    present_sections: Vec<ChunkCoord>,
    finalized_present_sections: Vec<ChunkCoord>,
    previously_loaded_sections: Vec<ChunkCoord>,
    removed_sections: Vec<ChunkCoord>,
    boundary_dirty_chunks: Vec<ChunkCoord>,
    boundary_dirty_seen: HashSet<ChunkCoord>,
    blob: Option<ColumnBlob>,
    phase: SavedLoadApplyPhase,
    prepare_cursor: i32,
    commit_remove_cursor: i32,
    commit_section_cursor: usize,
    commit_validated: bool,
    deferred_cursor: usize,
    finalize_cursor: i32,
    removed_lighting_cursor: usize,
    added_lighting_cursor: usize,
    secondary_cursor: usize,
    boundary_cursor: usize,
    loaded_any_chunk: bool,
    started_at: Instant,
    stats: SavedLoadApplyTaskStats,
}

impl SavedLoadApplyTask {
    fn new(cx: i32, cz: i32, column: ColumnBlob) -> Self {
        let min_section_y = column.min_section_y;
        let max_section_y = column.max_section_y;
        let section_span = (max_section_y - min_section_y + 1).max(0) as usize;
        let mut section_presence_mask = vec![false; section_span];
        let mut present_sections = Vec::with_capacity(column.sections.len());
        for section in &column.sections {
            let idx = (section.cy - min_section_y) as usize;
            if let Some(slot) = section_presence_mask.get_mut(idx) {
                *slot = true;
            }
            present_sections.push(ChunkCoord {
                x: cx,
                y: section.cy,
                z: cz,
            });
        }

        Self {
            cx,
            cz,
            min_section_y,
            max_section_y,
            section_presence_mask,
            present_sections,
            finalized_present_sections: Vec::new(),
            previously_loaded_sections: Vec::new(),
            removed_sections: Vec::new(),
            boundary_dirty_chunks: Vec::new(),
            boundary_dirty_seen: HashSet::new(),
            blob: Some(column),
            phase: SavedLoadApplyPhase::PrepareSections,
            prepare_cursor: min_section_y,
            commit_remove_cursor: min_section_y,
            commit_section_cursor: 0,
            commit_validated: false,
            deferred_cursor: 0,
            finalize_cursor: min_section_y,
            removed_lighting_cursor: 0,
            added_lighting_cursor: 0,
            secondary_cursor: 0,
            boundary_cursor: 0,
            loaded_any_chunk: false,
            started_at: Instant::now(),
            stats: SavedLoadApplyTaskStats::default(),
        }
    }

    fn phase_label(&self) -> &'static str {
        self.phase.label()
    }
}

enum SavedLoadApplyTaskOutcome {
    InProgress,
    Completed,
    Failed(String),
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
enum ChunkLightingReadiness {
    Provisional,
    VisualReady,
    Finalized,
}

impl ChunkLightingReadiness {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Provisional => "provisional",
            Self::VisualReady => "visual_ready",
            Self::Finalized => "finalized",
        }
    }
}

#[derive(Clone, Debug)]
struct ChunkLightingProgress {
    state: ChunkLightingReadiness,
    started_at: Instant,
    visually_acceptable_at: Option<Instant>,
    finalized_at: Option<Instant>,
}

#[derive(Debug)]
struct ChunkAddedLightingJob {
    original_batch_id: u64,
    split_job_index: usize,
    split_job_count: usize,
    source_primary_count: usize,
    primary_coords: Vec<ChunkCoord>,
    task: IncrementalLightingTask,
    enqueued_at: Instant,
    cpu_ms_accum: f32,
}

#[derive(Debug)]
struct RemovedLightingBatch {
    coords: Vec<ChunkCoord>,
    reason: &'static str,
    enqueued_at: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChunkGenApplyFollowUpPhase {
    BoundaryDirty,
    Lighting,
    Fluids,
    Finish,
}

impl ChunkGenApplyFollowUpPhase {
    const fn label(self) -> &'static str {
        match self {
            Self::BoundaryDirty => "boundary",
            Self::Lighting => "lighting",
            Self::Fluids => "fluids",
            Self::Finish => "finish",
        }
    }
}

#[derive(Debug)]
struct ChunkGenApplyFollowUpTask {
    coord: ChunkCoord,
    removed_chunk: bool,
    neighbor_cursor: usize,
    fluid_bootstrap: Vec<GeneratedFluidBootstrapCell>,
    fluid_bootstrap_cursor: usize,
    needs_chunk_fluid_scan: bool,
    fluid_scan_cursor: usize,
    phase: ChunkGenApplyFollowUpPhase,
    enqueued_at: Instant,
}

impl ChunkGenApplyFollowUpTask {
    fn new(
        coord: ChunkCoord,
        removed_chunk: bool,
        fluid_bootstrap: Vec<GeneratedFluidBootstrapCell>,
        needs_chunk_fluid_scan: bool,
    ) -> Self {
        Self {
            coord,
            removed_chunk,
            neighbor_cursor: 0,
            fluid_bootstrap,
            fluid_bootstrap_cursor: 0,
            needs_chunk_fluid_scan,
            fluid_scan_cursor: 0,
            phase: ChunkGenApplyFollowUpPhase::BoundaryDirty,
            enqueued_at: Instant::now(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ChunkGenDrainLimits {
    max_results: usize,
    budget_ms: f32,
    cpu_ms: f32,
    lighting_backlog: usize,
    mesh_backlog: usize,
    followup_backlog: usize,
    saved_load_backlog: usize,
    overshoot: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct MeshDirtyReasons(u8);

impl MeshDirtyReasons {
    const NONE: Self = Self(0);
    const GEOMETRY: Self = Self(1 << 0);
    const LIGHTING: Self = Self(1 << 1);
    const BOUNDARY: Self = Self(1 << 2);
    const DEBUG: Self = Self(1 << 3);

    const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    const fn is_empty(self) -> bool {
        self.0 == 0
    }

    const fn is_lighting_only(self) -> bool {
        self.0 != 0 && (self.0 & !Self::LIGHTING.0) == 0
    }

    const fn bits(self) -> u8 {
        self.0
    }

    fn labels(self) -> &'static str {
        match self.0 {
            0 => "none",
            x if x == Self::GEOMETRY.0 => "geometry",
            x if x == Self::LIGHTING.0 => "lighting",
            x if x == Self::BOUNDARY.0 => "boundary",
            x if x == Self::DEBUG.0 => "debug",
            x if x == (Self::GEOMETRY.0 | Self::BOUNDARY.0) => "geometry|boundary",
            x if x == (Self::GEOMETRY.0 | Self::LIGHTING.0) => "geometry|lighting",
            x if x == (Self::BOUNDARY.0 | Self::LIGHTING.0) => "boundary|lighting",
            x if x == (Self::DEBUG.0 | Self::LIGHTING.0) => "debug|lighting",
            x if x == (Self::GEOMETRY.0 | Self::DEBUG.0) => "geometry|debug",
            x if x == (Self::BOUNDARY.0 | Self::DEBUG.0) => "boundary|debug",
            _ => "mixed",
        }
    }
}

impl std::ops::BitOr for MeshDirtyReasons {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for MeshDirtyReasons {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChunkMeshState {
    CleanReady,
    DirtyGeometry,
    DirtyLighting,
    DirtyBoundary,
    DirtyMixed,
    InFlight,
    AwaitingLightingFinalization,
}

impl ChunkMeshState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CleanReady => "clean",
            Self::DirtyGeometry => "dirty_geometry",
            Self::DirtyLighting => "dirty_lighting",
            Self::DirtyBoundary => "dirty_boundary",
            Self::DirtyMixed => "dirty_mixed",
            Self::InFlight => "inflight",
            Self::AwaitingLightingFinalization => "await_lighting_final",
        }
    }
}

// Dirty reasons stay attached to a chunk across queueing and in-flight work so
// repeated invalidations collapse into one follow-up remesh instead of spawning duplicates.
#[derive(Clone, Copy, Debug)]
struct ChunkMeshSchedule {
    state: ChunkMeshState,
    queued_reasons: MeshDirtyReasons,
    pending_reasons: MeshDirtyReasons,
    inflight_reasons: MeshDirtyReasons,
}

impl Default for ChunkMeshSchedule {
    fn default() -> Self {
        Self {
            state: ChunkMeshState::CleanReady,
            queued_reasons: MeshDirtyReasons::NONE,
            pending_reasons: MeshDirtyReasons::NONE,
            inflight_reasons: MeshDirtyReasons::NONE,
        }
    }
}

const fn startup_mesh_upload_complete(
    mesh_state: Option<ChunkMeshState>,
    has_uploaded_gpu_mesh: bool,
) -> bool {
    match mesh_state {
        Some(ChunkMeshState::CleanReady | ChunkMeshState::AwaitingLightingFinalization) => true,
        Some(_) => false,
        None => has_uploaded_gpu_mesh,
    }
}

#[cfg(test)]
mod startup_mesh_tracking_tests {
    use super::{ChunkMeshState, startup_mesh_upload_complete};

    #[test]
    fn ready_mesh_state_counts_as_uploaded_without_gpu_geometry() {
        assert!(startup_mesh_upload_complete(
            Some(ChunkMeshState::CleanReady),
            false
        ));
        assert!(startup_mesh_upload_complete(
            Some(ChunkMeshState::AwaitingLightingFinalization),
            false
        ));
    }

    #[test]
    fn dirty_mesh_state_still_blocks_startup_completion() {
        assert!(!startup_mesh_upload_complete(
            Some(ChunkMeshState::DirtyGeometry),
            false
        ));
        assert!(!startup_mesh_upload_complete(
            Some(ChunkMeshState::InFlight),
            false
        ));
    }

    #[test]
    fn uploaded_gpu_mesh_without_schedule_entry_still_counts() {
        assert!(startup_mesh_upload_complete(None, true));
        assert!(!startup_mesh_upload_complete(None, false));
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct WorldgenFeatureDebugCounts {
    chunks_applied: u64,
    total_worldgen_water_blocks: u64,
    total_worldgen_water_source_blocks: u64,
    total_shoreline_sand_blocks: u64,
    last_worldgen_water_blocks: u32,
    last_worldgen_water_source_blocks: u32,
    last_shoreline_sand_blocks: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StreamPriorityKey {
    startup_rank: u8,
    dist2: i32,
    visibility_score: u32,
    horizon_penalty_rank: u8,
    view_alignment_rank: u8,
    y_band_rank: u8,
    age: u32,
}

impl StreamPriorityKey {
    fn cmp_ascending(&self, other: &Self) -> Ordering {
        self.startup_rank
            .cmp(&other.startup_rank)
            .then_with(|| self.dist2.cmp(&other.dist2))
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
    epoch: u64,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ChunkgenPriorityContextStamp {
    center: (i32, i32, i32),
    radius: i32,
    forward_bucket: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StreamHorizonCacheKey {
    camera_chunk: (i32, i32, i32),
    forward_bucket: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MeshUploadPriorityKey {
    startup_rank: u8,
    near_rank: u8,
    visibility_score: u8,
    mesh_presence_rank: u8,
    lighting_rank: u8,
    view_alignment_rank: u8,
    dist2: i32,
    age: u32,
}

impl MeshUploadPriorityKey {
    fn cmp_ascending(&self, other: &Self) -> Ordering {
        self.startup_rank
            .cmp(&other.startup_rank)
            .then_with(|| self.near_rank.cmp(&other.near_rank))
            .then_with(|| self.visibility_score.cmp(&other.visibility_score))
            .then_with(|| self.mesh_presence_rank.cmp(&other.mesh_presence_rank))
            .then_with(|| self.lighting_rank.cmp(&other.lighting_rank))
            .then_with(|| self.view_alignment_rank.cmp(&other.view_alignment_rank))
            .then_with(|| self.dist2.cmp(&other.dist2))
            .then_with(|| self.age.cmp(&other.age))
    }
}

#[derive(Clone, Copy, Debug)]
struct ChunkGenApplyFollowupLimits {
    budget_ms: f32,
    phase_budget_ms: f32,
    steps_per_pump: usize,
    neighbors_per_step: usize,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MeshPumpProgress {
    drained_results: usize,
    uploaded_meshes: usize,
}

impl MeshPumpProgress {
    fn did_work(self) -> bool {
        self.drained_results > 0 || self.uploaded_meshes > 0
    }
}

// Mesh results enter a short-lived upload queue so we can reprioritize GPU work
// for chunks nearest to the player before the actual buffer uploads happen.
struct QueuedMeshUpload {
    result: MeshResult,
    queued_at: Instant,
}

#[derive(Clone, Debug, Default)]
struct ColumnStreamPlan {
    tier0_y: Vec<i32>,
    tier1_y: Vec<i32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
struct StreamPlanningWorldKey {
    seed: u64,
    preset: WorldgenPreset,
    sea_level: i32,
    min_y: i32,
    max_y: i32,
    chunk_size: i32,
    boundary_ocean: bool,
    boundary_radius_chunks: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
struct SurfaceHeightCacheKey {
    column_x: i32,
    column_z: i32,
    world: StreamPlanningWorldKey,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
struct ColumnStreamPlanCacheKey {
    surface: SurfaceHeightCacheKey,
    center_chunk_y: i32,
    min_chunk_y: i32,
    max_chunk_y: i32,
    startup_mode: bool,
    prefer_camera_band: bool,
    secondary_extra: i32,
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
    crate::console_error!(
        "BLOCK_EDIT_SLOW kind={} ms={:.3} top={} top_ms={:.3} coord={}",
        kind,
        total_ms,
        top,
        top_ms,
        coord_str
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

#[derive(Clone, Copy, Debug)]
struct OutlineTarget {
    min: Vec3,
    max: Vec3,
}

fn build_outline_line_vertices(
    min: Vec3,
    max: Vec3,
) -> [OutlineVertex; BLOCK_OUTLINE_VERTEX_COUNT as usize] {
    let min = min - Vec3::splat(BLOCK_OUTLINE_EPS);
    let max = max + Vec3::splat(BLOCK_OUTLINE_EPS);
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
const HUD_ITEM_PREVIEW_INSET_PX: f32 = 4.0;
const INVENTORY_ITEM_PREVIEW_INSET_PX: f32 = 4.0;
const PAUSE_MENU_BUTTON_WIDTH_PX: f32 = 440.0;
const PAUSE_MENU_BUTTON_HEIGHT_PX: f32 = 40.0;
const PAUSE_MENU_BUTTON_SPACING_PX: f32 = 10.0;
const MENU_BUTTON_TEXTURE_KEY: &str = "minecraft:textures/gui/sprites/widget/button.png";
const MENU_BUTTON_HIGHLIGHTED_TEXTURE_KEY: &str =
    "minecraft:textures/gui/sprites/widget/button_highlighted.png";
const MENU_BUTTON_DISABLED_TEXTURE_KEY: &str =
    "minecraft:textures/gui/sprites/widget/button_disabled.png";
const MENU_BUTTON_LOGICAL_WIDTH_PX: u32 = 200;
const MENU_BUTTON_LOGICAL_HEIGHT_PX: u32 = 20;
const MENU_BUTTON_NINE_SLICE_BORDER_PX: f32 = 2.0;
const MENU_BUTTON_TEXT_NORMAL: [f32; 3] = [0.88, 0.88, 0.88];
const MENU_BUTTON_TEXT_HOVERED: [f32; 3] = [1.0, 1.0, 0.64];
const MENU_BUTTON_TEXT_DISABLED: [f32; 3] = [0.62, 0.62, 0.62];
const MENU_BUTTON_PRESSED_TINT: [f32; 3] = [0.92, 0.92, 0.92];
const HUD_HOTBAR_SLOT_COUNT: usize = 9;
const HUD_GUI_WIDGETS_KEY: &str = "minecraft:textures/gui/widgets.png";
const HUD_GUI_HOTBAR_KEY: &str = "minecraft:textures/gui/sprites/hud/hotbar.png";
const HUD_GUI_HOTBAR_SELECTION_KEY: &str =
    "minecraft:textures/gui/sprites/hud/hotbar_selection.png";
const INVENTORY_GUI_CREATIVE_KEY: &str =
    "minecraft:textures/gui/container/creative_inventory/tab_items.png";
const INVENTORY_GUI_SURVIVAL_KEY: &str = "minecraft:textures/gui/container/inventory.png";
const SKIN_TEXTURE_KEY_ROOT: &str = "minecraft:skin.png";
const SKIN_TEXTURE_KEY_ENTITY: &str = "minecraft:textures/entity/skin.png";
const HUD_HOTBAR_BG_RECT_CLASSIC: GuiRect = GuiRect {
    x: 0,
    y: 0,
    w: 182,
    h: 22,
};
const HUD_HOTBAR_SELECTION_RECT_CLASSIC: GuiRect = GuiRect {
    x: 0,
    y: 22,
    w: 24,
    h: 24,
};
const HUD_HOTBAR_SLOT_STRIDE_PX: f32 = 20.0;
const HUD_HOTBAR_UI_SCALE: f32 = 3.0;
const HUD_HOTBAR_MARGIN_BOTTOM_PX: f32 = 4.0;
const INVENTORY_GUI_SCALE: f32 = 3.0;
const INVENTORY_SLOT_COUNT: usize = 9;
const INVENTORY_SLOT_SIZE_PX: f32 = 18.0;
const INVENTORY_SLOT_STRIDE_PX: f32 = 18.0;
const INVENTORY_SELECTION_SIZE_FACTOR: f32 = 24.0 / 18.0;
const FIRST_PERSON_ARM_ATLAS_SIZE_PX: f32 = 64.0;
const FIRST_PERSON_ARM_WIDTH: f32 = 4.0 / 16.0;
const FIRST_PERSON_ARM_HEIGHT: f32 = 12.0 / 16.0;
const FIRST_PERSON_ARM_DEPTH: f32 = 4.0 / 16.0;
const FIRST_PERSON_ARM_FORWARD_OFFSET: f32 = 0.58;
const FIRST_PERSON_ARM_SIDE_OFFSET: f32 = 0.45;
const FIRST_PERSON_ARM_DOWN_OFFSET: f32 = 0.60;
const FIRST_PERSON_ARM_ROLL_RAD: f32 = 0.0_f32.to_radians();
const FIRST_PERSON_ARM_DIR_FORWARD_WEIGHT: f32 = 1.0;
const FIRST_PERSON_ARM_DIR_INWARD_WEIGHT: f32 = 0.30;
const FIRST_PERSON_ARM_DIR_DOWN_WEIGHT: f32 = 0.42;
const FIRST_PERSON_ARM_SCALE: f32 = 0.73;
const FIRST_PERSON_ARM_MODEL_ROT_X_RAD: f32 = -137.0_f32.to_radians();
const FIRST_PERSON_ARM_MODEL_ROT_Y_RAD: f32 = -23.0_f32.to_radians();
const FIRST_PERSON_ARM_MODEL_ROT_Z_RAD: f32 = 29.0_f32.to_radians();
const FIRST_PERSON_HELD_ITEM_SCALE: f32 = 0.35;
const FIRST_PERSON_HELD_ITEM_SIDE_OFFSET: f32 = 0.0;
const FIRST_PERSON_HELD_ITEM_DOWN_OFFSET: f32 = 0.0;
const FIRST_PERSON_HELD_ITEM_FORWARD_OFFSET: f32 = 0.0;
const FIRST_PERSON_HELD_ITEM_CAMERA_FORWARD_BIAS: f32 = 0.0;
const FIRST_PERSON_HELD_ITEM_GRIP_TO_CENTER_UP: f32 = 0.3;
const FIRST_PERSON_HELD_ITEM_GRIP_TO_CENTER_FORWARD: f32 = 1.5;
const FIRST_PERSON_HELD_ITEM_YAW_RAD: f32 = -35.0_f32.to_radians();
const FIRST_PERSON_HELD_ITEM_PITCH_RAD: f32 = 0.0_f32.to_radians();
pub(crate) const BLOCK_SELECT_COLUMNS: usize = 9;
const BLOCK_SELECT_CELL_SIZE_PX: f32 = 44.0;
const BLOCK_SELECT_CELL_GAP_PX: f32 = 8.0;
const BLOCK_SELECT_PANEL_PAD_PX: f32 = 14.0;
const BLOCK_SELECT_PANEL_BORDER: [f32; 3] = [0.12, 0.12, 0.12];
const BLOCK_SELECT_PANEL_BG: [f32; 3] = [0.20, 0.20, 0.20];
const BLOCK_SELECT_CELL_BORDER: [f32; 3] = [0.10, 0.10, 0.10];
const BLOCK_SELECT_CELL_BG: [f32; 3] = [0.27, 0.27, 0.27];
const BLOCK_SELECT_CELL_SELECTED: [f32; 3] = [0.55, 0.55, 0.55];
const COORDS_TEXT_SCALE: f32 = 1.5;
const DEBUG_OVERLAY_TEXT_SCALE: f32 = 2.5;
const CHAT_OVERLAY_TEXT_SCALE: f32 = 2.35;
const CHAT_OVERLAY_PAD_X: f32 = 8.0;
const CHAT_OVERLAY_PAD_BOTTOM: f32 = 86.0;
const CHAT_OVERLAY_LINE_GAP: f32 = 2.0;
const CHAT_OVERLAY_VISIBLE_SECONDS: f32 = 8.0;
const CHAT_OVERLAY_FADE_SECONDS: f32 = 4.0;
const CHAT_OVERLAY_BG_PAD_X: f32 = 4.0;
const CHAT_OVERLAY_BG_PAD_Y: f32 = 2.0;
const CHAT_OVERLAY_HISTORY_BG_COLOR: [f32; 3] = [0.22, 0.22, 0.22];
const CHAT_OVERLAY_HISTORY_BG_ALPHA: f32 = 0.12;
const CHAT_OVERLAY_INPUT_BG_COLOR: [f32; 3] = [0.72, 0.72, 0.72];
const CHAT_OVERLAY_INPUT_BG_ALPHA: f32 = 0.32;
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
const SAVED_LOAD_APPLY_PHASE_BUDGET_MS: f32 = 0.35;
const SAVED_LOAD_APPLY_STEP_CAP_PER_FRAME: usize = 8;
const SAVED_LOAD_APPLY_COMPLETIONS_PER_FRAME_CAP: usize = 2;
const SAVED_LOAD_APPLY_SECTION_BATCH_PER_STEP: usize = 4;
const SAVED_LOAD_APPLY_COMMIT_SECTION_BATCH_PER_STEP: usize = 1;
const SAVED_LOAD_APPLY_DEFERRED_EDITS_PER_BATCH: usize = 64;
const SAVED_LOAD_APPLY_DEFERRED_BATCHES_PER_STEP: usize = 2;
const SAVED_LOAD_APPLY_LIGHTING_BATCH_PER_STEP: usize = 1;
const SAVED_LOAD_APPLY_SECONDARY_BATCH_PER_STEP: usize = 4;
const SAVED_LOAD_APPLY_BOUNDARY_BATCH_PER_STEP: usize = 24;
const SAVED_LOAD_MISS_CACHE_TTL: Duration = Duration::from_secs(60);
const STREAM_STAGE_LOOP_SAFETY_CAP: usize = 4096;
const CHUNKGEN_APPLY_RESULTS_PER_FRAME_BASE: usize = 4;
const CHUNKGEN_APPLY_RESULTS_PER_FRAME_BURST: usize = 8;
const CHUNKGEN_APPLY_RESULTS_PER_FRAME_MIN: usize = 1;
const CHUNKGEN_APPLY_CPU_TARGET_MS: f32 = 8.0;
const CHUNKGEN_APPLY_CPU_HARD_STOP_MS: f32 = 12.0;
const CHUNKGEN_APPLY_BACKPRESSURE_LIGHTING_THRESHOLD: usize = 24;
const CHUNKGEN_APPLY_BACKPRESSURE_MESH_THRESHOLD: usize = 192;
const CHUNKGEN_APPLY_BACKPRESSURE_FOLLOWUP_THRESHOLD: usize = 24;
const CHUNKGEN_APPLY_FOLLOWUP_BUDGET_MS: f32 = 1.5;
const CHUNKGEN_APPLY_FOLLOWUP_PHASE_BUDGET_MS: f32 = 0.35;
const CHUNKGEN_APPLY_FOLLOWUP_STEPS_PER_FRAME: usize = 8;
const CHUNKGEN_APPLY_FOLLOWUP_NEIGHBORS_PER_STEP: usize = 2;
const CHUNKGEN_APPLY_FOLLOWUP_FLUID_BOOTSTRAP_BATCH: usize = 32;
const CHUNKGEN_APPLY_FOLLOWUP_FLUID_SCAN_BATCH: usize = 128;
const STARTUP_STREAMING_BURST_FRAME_BUDGET_MS: f32 = 20.0;
const STARTUP_STREAMING_BURST_INTERNAL_PASSES: usize = 4;
const STARTUP_CHUNKGEN_APPLY_RESULTS_PER_PASS: usize = 48;
const STARTUP_CHUNKGEN_APPLY_BUDGET_MS: f32 = 6.0;
const STARTUP_CHUNKGEN_APPLY_FOLLOWUP_BUDGET_MS: f32 = 4.0;
const STARTUP_CHUNKGEN_APPLY_FOLLOWUP_PHASE_BUDGET_MS: f32 = 0.75;
const STARTUP_CHUNKGEN_APPLY_FOLLOWUP_STEPS_PER_PASS: usize = 48;
const STARTUP_CHUNKGEN_APPLY_FOLLOWUP_NEIGHBORS_PER_STEP: usize = 6;
const CHUNKGEN_REQUESTS_PER_FRAME: usize = 48;
const TICKING_AREA_STREAM_BUDGET_PER_FRAME: usize = 24;
const CHUNKGEN_REQUESTS_PER_FRAME_BACKPRESSURE_MIN: usize = 8;
const CHUNKGEN_BACKPRESSURE_DIRTY_COLUMNS_THRESHOLD: usize = 200;
const CHUNKGEN_BACKPRESSURE_PENDING_UNLOAD_COLUMNS_THRESHOLD: usize = 16;
const CHUNKGEN_PRIORITY_HEAP_REBUILD_PERIOD_FRAMES: u64 = 12;
const CHUNKGEN_PENDING_COLUMN_SCAN_PER_FRAME: usize = 48;
const CHUNKGEN_PENDING_PRIORITY_REFRESH_PER_FRAME: usize = 96;
const CHUNKGEN_PENDING_NEAR_REFRESH_OFFSETS_PER_FRAME: usize = 24;
const CHUNKGEN_PENDING_HEAP_REBUILD_MIN_SIZE: usize = 128;
const CHUNKGEN_PENDING_HEAP_REBUILD_BLOAT_FACTOR: usize = 4;
const STREAM_PRIORITY_VIEW_ALIGNMENT_BUCKETS: u8 = 8;
const STREAM_PRIORITY_FORWARD_BUCKETS: u8 = 16;
const STREAM_PRIORITY_REORDER_MIN_BACKLOG: usize = 24;
const STREAM_HORIZON_REUSE_MAX_FRAMES: u64 = 8;
const STREAM_MISSING_MESH_SCAN_COLUMNS_ON_CENTER_CHANGE: usize = 48;
const MESH_UPLOAD_NEAR_PRIORITY_RADIUS_CHUNKS: i32 = 2;
const MESH_UPLOAD_IMMEDIATE_RADIUS_CHUNKS: i32 = 1;
const MESH_UPLOAD_LOW_SPEED_THRESHOLD_BLOCKS_PER_SEC: f32 = 6.0;
const MESH_UPLOAD_LOW_SPEED_BUDGET_MS: f32 = 5.5;
const MESH_UPLOAD_WARMUP_BUDGET_MS: f32 = 10.0;
const MESH_UPLOAD_NEAR_URGENT_THRESHOLD: usize = 1;
const MESH_FRUSTUM_DEFER_FRAMES: u64 = 24;
const MESH_MISSING_SAFETY_SCAN_PERIOD_FRAMES: u64 = 30;
const STREAM_HORIZON_SECTOR_COUNT: usize = 64;
const STREAM_HORIZON_OCCLUDED_MARGIN_RAD: f32 = 0.034_906_585;
const STREAM_HORIZON_PENALTY_STEP_RAD: f32 = 0.087_266_46;
const STREAM_HORIZON_MAX_PENALTY_RANK: u8 = 6;
const MAX_INFLIGHT_CHUNKGEN: usize = 128;
const MAX_INFLIGHT_LOD_CHUNKGEN: usize = 64;
const MAX_INFLIGHT_TOTAL_CHUNKGEN_WITH_LOD: usize =
    MAX_INFLIGHT_CHUNKGEN + MAX_INFLIGHT_LOD_CHUNKGEN;
const CHUNKGEN_JOB_QUEUE_CAPACITY: usize = 256;
const CHUNKGEN_RESULT_QUEUE_CAPACITY: usize = 256;
const CHUNKGEN_APPLY_LOG_THRESHOLD_MS: f32 = 8.0;
const MESH_RESULTS_DRAIN_PER_FRAME: usize = 64;
const STARTUP_MESH_RESULTS_DRAIN_PER_PASS: usize = 256;
const STARTUP_MESH_DISPATCH_BUDGET_MS: f32 = 8.0;
const STARTUP_MESH_UPLOAD_BUDGET_MS: f32 = 10.0;
const STARTUP_FINAL_LIGHTING_TRIGGER_PERCENT: usize = 85;
const STORAGE_SAVE_RESULTS_PER_FRAME: usize = 32;
const LIGHTING_RESULTS_PER_FRAME: usize = 2;
const LIGHTING_CHANGED_CHUNK_DIRTY_APPLY_PER_FRAME: usize = 64;
const LIGHTING_CHANGED_CHUNK_DIRTY_APPLY_BUDGET_MS: f32 = 0.5;
const LIGHTING_REBUILD_DEBOUNCE_MS: u64 = 75;
const STREAM_CHUNK_LIGHTING_MAX_PRIMARY_CHUNKS_PER_JOB: usize = 3;
const STREAM_CHUNK_LIGHTING_MAX_REGION_CHUNKS_PER_JOB: usize = 14;
const STREAM_CHUNK_LIGHTING_MAX_VOXELS_PER_STEP: u64 = 48_000;
const STREAM_CHUNK_LIGHTING_STEP_BUDGET_MS: f32 = 1.5;
const STREAM_CHUNK_LIGHTING_FRAME_BUDGET_MS: f32 = 3.0;
const STREAM_CHUNK_LIGHTING_MAX_STEPS_PER_FRAME: usize = 4;
const PENDING_LIGHTING_SKY_HEURISTIC_MIN_Y: i32 = 64;
const MESH_UPLOAD_LOG_THRESHOLD: Duration = Duration::from_millis(4);
const BLOCK_EDIT_SIDEFX_BUDGET_MS: f32 = 1.5;
const BLOCK_EDIT_SIDEFX_MAX_EDITS_PER_FRAME: usize = 8;
const BLOCK_EDIT_PARTICLES_MAX_SPAWN_PER_FRAME: usize = 64;
const BLOCK_EDIT_SAND_SCAN_STEPS_PER_FRAME: usize = 32;
const DEFERRED_EDITS_PER_CHUNK_CAP: usize = 4096;
const DEFERRED_EDITS_GLOBAL_CAP: usize = 200_000;
const PLACEHOLDER_SALVAGE_NON_AIR_CAP: usize = 20_000;
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
const STARTUP_COLUMN_WARMUP_REQUESTS_PER_FRAME: usize = 24;
const AUTOSAVE_DYNAMIC_DIRTY_COLUMNS_PER_STEP: usize = 50;
const AUTOSAVE_DYNAMIC_MAX_COLUMNS_PER_FRAME: usize = 8;
const SKY_SUN_QUAD_SIZE: f32 = 30.0;
const SKY_MOON_QUAD_SIZE: f32 = 20.0;
const SKY_SUN_DISTANCE: f32 = 100.0;
const SKY_MOON_DISTANCE: f32 = 100.0;
const SKY_STAR_COUNT: usize = 192;
const SKY_STAR_DISTANCE: f32 = 180.0;
const SKY_STAR_SIZE_MIN: f32 = 0.45;
const SKY_STAR_SIZE_MAX: f32 = 1.15;
const SKY_STAR_BRIGHTNESS_MIN: f32 = 0.55;
const SKY_STAR_BRIGHTNESS_MAX: f32 = 1.0;
const SKY_BASE_OBJECT_VERTEX_COUNT: usize = 8;
const SKY_BASE_OBJECT_INDEX_COUNT: u32 = 12;
const SKY_OBJECT_VERTEX_COUNT: usize = SKY_BASE_OBJECT_VERTEX_COUNT + SKY_STAR_COUNT * 4;
const SKY_OBJECT_INDEX_COUNT: u32 = SKY_BASE_OBJECT_INDEX_COUNT + (SKY_STAR_COUNT as u32) * 6;
const MENU_PANORAMA_FACE_COUNT: usize = 6;
const MENU_PANORAMA_VERTEX_COUNT: usize = MENU_PANORAMA_FACE_COUNT * 4;
const MENU_PANORAMA_INDEX_COUNT: u32 = (MENU_PANORAMA_FACE_COUNT * 6) as u32;
const MENU_PANORAMA_CUBE_SIZE: f32 = 80.0;
const MENU_PANORAMA_PITCH_RAD: f32 = -12.0_f32.to_radians();
const MENU_PANORAMA_YAW_SPEED_RAD_PER_SEC: f32 = 0.06;
const SKY_TEXTURE_MODE_DEFAULT: u32 = 0;
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
    pub(crate) chunkgen_apply_pure_ms: f32,
    pub(crate) chunkgen_apply_followup_ms: f32,
    pub(crate) chunkgen_apply_followup_backlog_before: usize,
    pub(crate) chunkgen_apply_followup_backlog_after: usize,
    pub(crate) chunkgen_apply_followup_steps: usize,
    pub(crate) chunkgen_apply_followup_completed: usize,
    pub(crate) chunkgen_apply_followup_max_step_ms: f32,
    pub(crate) chunkgen_apply_followup_tasks_queued: usize,
    pub(crate) chunkgen_apply_lighting_add_queued: usize,
    pub(crate) chunkgen_apply_lighting_remove_queued: usize,
    pub(crate) chunkgen_apply_fluid_bootstrap_cells_queued: usize,
    pub(crate) chunkgen_apply_deferred_fluid_scans_queued: usize,
    pub(crate) chunkgen_apply_boundary_dirty_marks: usize,
    pub(crate) chunkgen_apply_cpu_target_overshoots: usize,
    pub(crate) stream_scheduler_pending_peak: usize,
    pub(crate) stream_scheduler_heap_peak: usize,
    pub(crate) stream_scheduler_heap_rebuild_items: usize,
    pub(crate) stream_scheduler_heap_rebuild_ms: f32,
    pub(crate) stream_scheduler_reprioritized_items: usize,
    pub(crate) stream_scheduler_frustum_cache_hits: usize,
    pub(crate) stream_scheduler_frustum_cache_misses: usize,
    pub(crate) stream_scheduler_horizon_cache_hits: usize,
    pub(crate) stream_scheduler_horizon_cache_misses: usize,
    pub(crate) stream_scheduler_frustum_refresh_ms: f32,
    pub(crate) stream_scheduler_horizon_refresh_ms: f32,
    pub(crate) deferred_edits_pending_total: usize,
    pub(crate) deferred_edits_applied: usize,
    pub(crate) deferred_edits_chunks_touched: usize,
    pub(crate) deferred_edits_dropped_due_to_caps: usize,
    pub(crate) placeholder_salvage_chunks: usize,
    pub(crate) placeholder_salvage_edits_enqueued: usize,
    pub(crate) placeholder_salvage_skipped_dense: usize,

    pub(crate) mesh_results_drained: usize,
    pub(crate) mesh_results_backlog_before: usize,
    pub(crate) mesh_results_backlog_after: usize,
    pub(crate) mesh_upload_queue_before: usize,
    pub(crate) mesh_upload_queue_after: usize,
    pub(crate) mesh_uploads_processed: usize,
    pub(crate) mesh_gpu_buffers_created: u32,
    pub(crate) mesh_gpu_reused_writes: u32,
    pub(crate) mesh_upload_wait_samples: usize,
    pub(crate) mesh_upload_wait_total_ms: f32,
    pub(crate) mesh_upload_wait_max_ms: f32,
    pub(crate) mesh_upload_near_first_visible_count: usize,
    pub(crate) mesh_upload_near_first_visible_total_ms: f32,
    pub(crate) mesh_upload_near_first_visible_max_ms: f32,
    pub(crate) mesh_upload_near_final_ready_count: usize,
    pub(crate) mesh_upload_near_final_ready_total_ms: f32,
    pub(crate) mesh_upload_near_final_ready_max_ms: f32,
    pub(crate) mesh_upload_finalized_upgrades: usize,
    pub(crate) mesh_dirty_geometry_events: usize,
    pub(crate) mesh_dirty_lighting_events: usize,
    pub(crate) mesh_dirty_boundary_events: usize,
    pub(crate) mesh_dirty_coalesced_events: usize,
    pub(crate) mesh_avoided_remeshes: usize,
    pub(crate) mesh_boundary_dispatches: usize,
    pub(crate) mesh_lighting_dispatches: usize,
    pub(crate) mesh_upload_budget_boost_ms: f32,

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
    pub(crate) lighting_incremental_jobs: usize,
    pub(crate) lighting_incremental_primary_chunks: usize,
    pub(crate) lighting_incremental_region_chunks: usize,
    pub(crate) lighting_incremental_steps: usize,
    pub(crate) lighting_incremental_split_jobs: usize,
    pub(crate) lighting_incremental_backlog_depth: usize,
    pub(crate) lighting_incremental_step_total_ms: f32,
    pub(crate) lighting_incremental_step_max_ms: f32,
    pub(crate) lighting_incremental_voxels_visited: u64,
    pub(crate) lighting_incremental_chunks_touched: usize,
    pub(crate) lighting_incremental_boundary_propagations: u64,

    pub(crate) saved_column_load_attempts: usize,
    pub(crate) saved_column_load_hits: usize,
    pub(crate) saved_column_load_misses: usize,
    pub(crate) saved_load_results_drained: usize,
    pub(crate) saved_load_results_applied: usize,
    pub(crate) saved_load_results_discarded: usize,
    pub(crate) saved_load_results_failed: usize,
    pub(crate) saved_load_results_backlog_before: usize,
    pub(crate) saved_load_results_backlog_after: usize,
    pub(crate) saved_load_apply_queue_before: usize,
    pub(crate) saved_load_apply_queue_after: usize,
    pub(crate) saved_load_apply_steps: usize,
    pub(crate) saved_load_apply_completed: usize,
    pub(crate) saved_load_apply_resumed: usize,
    pub(crate) saved_load_apply_yielded: usize,
    pub(crate) saved_load_sections_prepared: usize,
    pub(crate) saved_load_sections_finalized: usize,
    pub(crate) saved_load_deferred_batches: usize,
    pub(crate) saved_load_deferred_edits_applied: usize,
    pub(crate) saved_load_chunks_touched: usize,
    pub(crate) saved_load_boundary_marks: usize,
    pub(crate) saved_load_completed_columns_per_sec: f32,
    pub(crate) saved_load_apply_prepare_ms: f32,
    pub(crate) saved_load_apply_commit_ms: f32,
    pub(crate) saved_load_apply_deferred_ms: f32,
    pub(crate) saved_load_apply_finalize_ms: f32,
    pub(crate) saved_load_apply_lighting_ms: f32,
    pub(crate) saved_load_apply_secondary_ms: f32,
    pub(crate) saved_load_apply_prepare_step_max_ms: f32,
    pub(crate) saved_load_apply_commit_step_max_ms: f32,
    pub(crate) saved_load_apply_deferred_step_max_ms: f32,
    pub(crate) saved_load_apply_finalize_step_max_ms: f32,
    pub(crate) saved_load_apply_lighting_step_max_ms: f32,
    pub(crate) saved_load_apply_secondary_step_max_ms: f32,
    pub(crate) saved_load_apply_worst_task_ms: f32,

    pub(crate) far_lod_requests_queued: usize,
    pub(crate) far_lod_requests_suppressed_cpu_budget: bool,
    pub(crate) far_lod_requests_suppressed_backlog: bool,
    pub(crate) far_lod_center_changes: usize,
    pub(crate) far_lod_visible_columns: usize,
    pub(crate) far_lod_drawcalls: usize,
    pub(crate) near_visible_chunks: usize,
    pub(crate) near_mesh_memory_bytes: u64,
    pub(crate) far_lod_memory_bytes: u64,
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
            chunkgen_apply_pure_ms: 0.0,
            chunkgen_apply_followup_ms: 0.0,
            chunkgen_apply_followup_backlog_before: 0,
            chunkgen_apply_followup_backlog_after: 0,
            chunkgen_apply_followup_steps: 0,
            chunkgen_apply_followup_completed: 0,
            chunkgen_apply_followup_max_step_ms: 0.0,
            chunkgen_apply_followup_tasks_queued: 0,
            chunkgen_apply_lighting_add_queued: 0,
            chunkgen_apply_lighting_remove_queued: 0,
            chunkgen_apply_fluid_bootstrap_cells_queued: 0,
            chunkgen_apply_deferred_fluid_scans_queued: 0,
            chunkgen_apply_boundary_dirty_marks: 0,
            chunkgen_apply_cpu_target_overshoots: 0,
            stream_scheduler_pending_peak: 0,
            stream_scheduler_heap_peak: 0,
            stream_scheduler_heap_rebuild_items: 0,
            stream_scheduler_heap_rebuild_ms: 0.0,
            stream_scheduler_reprioritized_items: 0,
            stream_scheduler_frustum_cache_hits: 0,
            stream_scheduler_frustum_cache_misses: 0,
            stream_scheduler_horizon_cache_hits: 0,
            stream_scheduler_horizon_cache_misses: 0,
            stream_scheduler_frustum_refresh_ms: 0.0,
            stream_scheduler_horizon_refresh_ms: 0.0,
            deferred_edits_pending_total: 0,
            deferred_edits_applied: 0,
            deferred_edits_chunks_touched: 0,
            deferred_edits_dropped_due_to_caps: 0,
            placeholder_salvage_chunks: 0,
            placeholder_salvage_edits_enqueued: 0,
            placeholder_salvage_skipped_dense: 0,
            mesh_results_drained: 0,
            mesh_results_backlog_before: 0,
            mesh_results_backlog_after: 0,
            mesh_upload_queue_before: 0,
            mesh_upload_queue_after: 0,
            mesh_uploads_processed: 0,
            mesh_gpu_buffers_created: 0,
            mesh_gpu_reused_writes: 0,
            mesh_upload_wait_samples: 0,
            mesh_upload_wait_total_ms: 0.0,
            mesh_upload_wait_max_ms: 0.0,
            mesh_upload_near_first_visible_count: 0,
            mesh_upload_near_first_visible_total_ms: 0.0,
            mesh_upload_near_first_visible_max_ms: 0.0,
            mesh_upload_near_final_ready_count: 0,
            mesh_upload_near_final_ready_total_ms: 0.0,
            mesh_upload_near_final_ready_max_ms: 0.0,
            mesh_upload_finalized_upgrades: 0,
            mesh_dirty_geometry_events: 0,
            mesh_dirty_lighting_events: 0,
            mesh_dirty_boundary_events: 0,
            mesh_dirty_coalesced_events: 0,
            mesh_avoided_remeshes: 0,
            mesh_boundary_dispatches: 0,
            mesh_lighting_dispatches: 0,
            mesh_upload_budget_boost_ms: 0.0,
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
            lighting_incremental_jobs: 0,
            lighting_incremental_primary_chunks: 0,
            lighting_incremental_region_chunks: 0,
            lighting_incremental_steps: 0,
            lighting_incremental_split_jobs: 0,
            lighting_incremental_backlog_depth: 0,
            lighting_incremental_step_total_ms: 0.0,
            lighting_incremental_step_max_ms: 0.0,
            lighting_incremental_voxels_visited: 0,
            lighting_incremental_chunks_touched: 0,
            lighting_incremental_boundary_propagations: 0,
            saved_column_load_attempts: 0,
            saved_column_load_hits: 0,
            saved_column_load_misses: 0,
            saved_load_results_drained: 0,
            saved_load_results_applied: 0,
            saved_load_results_discarded: 0,
            saved_load_results_failed: 0,
            saved_load_results_backlog_before: 0,
            saved_load_results_backlog_after: 0,
            saved_load_apply_queue_before: 0,
            saved_load_apply_queue_after: 0,
            saved_load_apply_steps: 0,
            saved_load_apply_completed: 0,
            saved_load_apply_resumed: 0,
            saved_load_apply_yielded: 0,
            saved_load_sections_prepared: 0,
            saved_load_sections_finalized: 0,
            saved_load_deferred_batches: 0,
            saved_load_deferred_edits_applied: 0,
            saved_load_chunks_touched: 0,
            saved_load_boundary_marks: 0,
            saved_load_completed_columns_per_sec: 0.0,
            saved_load_apply_prepare_ms: 0.0,
            saved_load_apply_commit_ms: 0.0,
            saved_load_apply_deferred_ms: 0.0,
            saved_load_apply_finalize_ms: 0.0,
            saved_load_apply_lighting_ms: 0.0,
            saved_load_apply_secondary_ms: 0.0,
            saved_load_apply_prepare_step_max_ms: 0.0,
            saved_load_apply_commit_step_max_ms: 0.0,
            saved_load_apply_deferred_step_max_ms: 0.0,
            saved_load_apply_finalize_step_max_ms: 0.0,
            saved_load_apply_lighting_step_max_ms: 0.0,
            saved_load_apply_secondary_step_max_ms: 0.0,
            saved_load_apply_worst_task_ms: 0.0,
            far_lod_requests_queued: 0,
            far_lod_requests_suppressed_cpu_budget: false,
            far_lod_requests_suppressed_backlog: false,
            far_lod_center_changes: 0,
            far_lod_visible_columns: 0,
            far_lod_drawcalls: 0,
            near_visible_chunks: 0,
            near_mesh_memory_bytes: 0,
            far_lod_memory_bytes: 0,
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

#[derive(Clone, Debug)]
struct PerfSummaryWindow {
    started_at: Instant,
    frames: u32,
    max_frame_ms: f32,
    sum_process_dirty_chunk_meshes_ms: f32,
    sum_chunkgen_ms: f32,
    sum_mesh_ms: f32,
    sum_storage_ms: f32,
    sum_lighting_ms: f32,
    sum_stream_ms: f32,
    sum_saved_load_ms: f32,
    sum_mesh_upload_wait_ms: f32,
    sum_mesh_upload_wait_samples: usize,
    sum_mesh_upload_near_first_visible_ms: f32,
    sum_mesh_upload_near_first_visible_count: usize,
    sum_mesh_upload_near_final_ready_ms: f32,
    sum_mesh_upload_near_final_ready_count: usize,
    sum_mesh_upload_finalized_upgrades: usize,
}

impl PerfSummaryWindow {
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            frames: 0,
            max_frame_ms: 0.0,
            sum_process_dirty_chunk_meshes_ms: 0.0,
            sum_chunkgen_ms: 0.0,
            sum_mesh_ms: 0.0,
            sum_storage_ms: 0.0,
            sum_lighting_ms: 0.0,
            sum_stream_ms: 0.0,
            sum_saved_load_ms: 0.0,
            sum_mesh_upload_wait_ms: 0.0,
            sum_mesh_upload_wait_samples: 0,
            sum_mesh_upload_near_first_visible_ms: 0.0,
            sum_mesh_upload_near_first_visible_count: 0,
            sum_mesh_upload_near_final_ready_ms: 0.0,
            sum_mesh_upload_near_final_ready_count: 0,
            sum_mesh_upload_finalized_upgrades: 0,
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn push_frame(&mut self, perf: &StateFramePerf) {
        self.frames = self.frames.saturating_add(1);
        self.max_frame_ms = self.max_frame_ms.max(perf.frame_dt_ms);
        self.sum_process_dirty_chunk_meshes_ms += perf.process_dirty_chunk_meshes_ms;
        self.sum_chunkgen_ms += perf.pump_chunkgen_results_ms;
        self.sum_mesh_ms += perf.pump_mesh_results_ms;
        self.sum_storage_ms += perf.pump_storage_save_results_ms + perf.autosave_total_ms;
        self.sum_lighting_ms += perf.rebuild_world_lighting_ms;
        self.sum_stream_ms += perf.enqueue_chunks_within_view_ms;
        self.sum_saved_load_ms += perf.ensure_saved_column_load_ms;
        self.sum_mesh_upload_wait_ms += perf.mesh_upload_wait_total_ms;
        self.sum_mesh_upload_wait_samples += perf.mesh_upload_wait_samples;
        self.sum_mesh_upload_near_first_visible_ms += perf.mesh_upload_near_first_visible_total_ms;
        self.sum_mesh_upload_near_first_visible_count += perf.mesh_upload_near_first_visible_count;
        self.sum_mesh_upload_near_final_ready_ms += perf.mesh_upload_near_final_ready_total_ms;
        self.sum_mesh_upload_near_final_ready_count += perf.mesh_upload_near_final_ready_count;
        self.sum_mesh_upload_finalized_upgrades += perf.mesh_upload_finalized_upgrades;
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

fn centered_uv_rect_4(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> [f32; 4] {
    let off_x = (dst_w.saturating_sub(src_w)) / 2;
    let off_y = (dst_h.saturating_sub(src_h)) / 2;
    [
        off_x as f32 / dst_w.max(1) as f32,
        off_y as f32 / dst_h.max(1) as f32,
        (off_x + src_w) as f32 / dst_w.max(1) as f32,
        (off_y + src_h) as f32 / dst_h.max(1) as f32,
    ]
}

fn apply_black_alpha_mask(rgba: &mut [u8]) {
    for pixel in rgba.chunks_exact_mut(4) {
        let max_rgb = pixel[0].max(pixel[1]).max(pixel[2]) as u16;
        pixel[3] = ((pixel[3] as u16 * max_rgb) / 255) as u8;
    }
}

#[derive(Clone, Copy, Debug)]
struct SkyStar {
    direction: Vec3,
    size: f32,
    brightness: f32,
}

fn generate_sky_stars() -> Vec<SkyStar> {
    let mut state = 0xD1CE_BA5E_F00D_1234_u64;
    let mut stars = Vec::with_capacity(SKY_STAR_COUNT);
    let next_unit = |state: &mut u64| {
        *state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((*state >> 40) as u32) as f32 / (u32::MAX as f32)
    };

    while stars.len() < SKY_STAR_COUNT {
        let z = lerp(-1.0, 1.0, next_unit(&mut state));
        let azimuth = next_unit(&mut state) * std::f32::consts::TAU;
        let radius = (1.0 - z * z).max(0.0).sqrt();
        let direction = Vec3::new(radius * azimuth.cos(), z, radius * azimuth.sin());
        if direction.length_squared() < 0.99 || direction.y.abs() < 0.08 {
            continue;
        }
        let size = lerp(SKY_STAR_SIZE_MIN, SKY_STAR_SIZE_MAX, next_unit(&mut state));
        let brightness = lerp(
            SKY_STAR_BRIGHTNESS_MIN,
            SKY_STAR_BRIGHTNESS_MAX,
            next_unit(&mut state),
        );
        stars.push(SkyStar {
            direction: direction.normalize(),
            size,
            brightness,
        });
    }

    stars
}

#[cfg(test)]
mod sky_texture_tests {
    use super::apply_black_alpha_mask;

    #[test]
    fn black_pixels_become_transparent_while_colored_halo_stays_partial() {
        let mut rgba = vec![
            0, 0, 0, 255, // black background
            32, 32, 64, 255, // dim halo
            255, 240, 128, 255, // bright core
        ];

        apply_black_alpha_mask(&mut rgba);

        assert_eq!(rgba[3], 0, "pure black should turn fully transparent");
        assert_eq!(rgba[7], 64, "dim halo should keep partial alpha");
        assert_eq!(rgba[11], 255, "bright core should stay opaque");
    }
}

fn nontransparent_bounds_rgba(rgba: &[u8], width: u32, height: u32) -> Option<GuiRect> {
    if width == 0 || height == 0 {
        return None;
    }
    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    let mut found = false;

    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4 + 3) as usize;
            if idx >= rgba.len() || rgba[idx] == 0 {
                continue;
            }
            found = true;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }

    if !found {
        return None;
    }
    Some(GuiRect {
        x: min_x,
        y: min_y,
        w: max_x - min_x + 1,
        h: max_y - min_y + 1,
    })
}

impl HudAtlasSource {
    fn texture_keys(self) -> &'static [&'static str] {
        match self {
            Self::ClassicWidgets => &[HUD_GUI_WIDGETS_KEY],
            Self::ModernSprites => &[HUD_GUI_HOTBAR_KEY, HUD_GUI_HOTBAR_SELECTION_KEY],
        }
    }
}

impl InventoryAtlasSource {
    fn texture_key(self) -> &'static str {
        match self {
            Self::CreativeTabItems => INVENTORY_GUI_CREATIVE_KEY,
            Self::SurvivalInventory => INVENTORY_GUI_SURVIVAL_KEY,
        }
    }

    fn logical_size_px(self) -> [u32; 2] {
        match self {
            Self::CreativeTabItems => [195, 136],
            Self::SurvivalInventory => [176, 166],
        }
    }

    fn palette_columns(self) -> usize {
        9
    }

    fn palette_rows(self) -> usize {
        5
    }

    fn grid_origin_px(self) -> [f32; 2] {
        match self {
            Self::CreativeTabItems => [9.0, 18.0],
            Self::SurvivalInventory => [7.0, 17.0],
        }
    }

    fn hotbar_origin_px(self) -> [f32; 2] {
        match self {
            Self::CreativeTabItems => [9.0, 113.0],
            Self::SurvivalInventory => [7.0, 143.0],
        }
    }

    fn preview_rect_px(self) -> GuiRect {
        match self {
            Self::CreativeTabItems => GuiRect {
                x: 173,
                y: 3,
                w: 18,
                h: 18,
            },
            Self::SurvivalInventory => GuiRect {
                x: 151,
                y: 16,
                w: 18,
                h: 18,
            },
        }
    }
}

fn pick_hud_atlas_source(texture_registry: &mut TextureRegistry) -> HudAtlasSource {
    if texture_registry.get_or_load(HUD_GUI_WIDGETS_KEY).is_ok() {
        return HudAtlasSource::ClassicWidgets;
    }

    let modern_hotbar = texture_registry.get_or_load(HUD_GUI_HOTBAR_KEY).is_ok();
    let modern_selection = texture_registry
        .get_or_load(HUD_GUI_HOTBAR_SELECTION_KEY)
        .is_ok();
    if modern_hotbar && modern_selection {
        return HudAtlasSource::ModernSprites;
    }

    panic!(
        "missing required hotbar GUI textures: expected '{}' OR ('{}' and '{}')",
        HUD_GUI_WIDGETS_KEY, HUD_GUI_HOTBAR_KEY, HUD_GUI_HOTBAR_SELECTION_KEY
    );
}

fn pick_inventory_atlas_source(texture_registry: &mut TextureRegistry) -> InventoryAtlasSource {
    let candidates = [
        InventoryAtlasSource::CreativeTabItems,
        InventoryAtlasSource::SurvivalInventory,
    ];
    for candidate in candidates {
        if texture_registry
            .get_or_load(candidate.texture_key())
            .is_ok()
        {
            return candidate;
        }
    }

    panic!(
        "missing required inventory GUI textures: expected '{}' or '{}'",
        INVENTORY_GUI_CREATIVE_KEY, INVENTORY_GUI_SURVIVAL_KEY
    );
}

fn pick_skin_texture_key(texture_registry: &mut TextureRegistry) -> &'static str {
    for key in [SKIN_TEXTURE_KEY_ROOT, SKIN_TEXTURE_KEY_ENTITY] {
        if texture_registry.get_or_load(key).is_ok() {
            return key;
        }
    }

    panic!(
        "missing required skin texture: expected '{}' or '{}'",
        SKIN_TEXTURE_KEY_ROOT, SKIN_TEXTURE_KEY_ENTITY
    );
}

fn hud_uv_for_rect(layout: &HudTextureLayout, rect: GuiRect) -> ([f32; 2], [f32; 2]) {
    assert!(
        rect.x + rect.w <= layout.content_w && rect.y + rect.h <= layout.content_h,
        "HUD rect out of bounds: rect=({},{} {}x{}), content={}x{}",
        rect.x,
        rect.y,
        rect.w,
        rect.h,
        layout.content_w,
        layout.content_h
    );

    let x0 = layout.content_offset_x + rect.x;
    let y0 = layout.content_offset_y + rect.y;
    let x1 = x0 + rect.w;
    let y1 = y0 + rect.h;
    let frame = layout.frame_size as f32;
    (
        [x0 as f32 / frame, y0 as f32 / frame],
        [x1 as f32 / frame, y1 as f32 / frame],
    )
}

fn centered_uv_rect(
    layer_width: u32,
    layer_height: u32,
    content_width: u32,
    content_height: u32,
) -> ([f32; 2], [f32; 2]) {
    let offset_x = (layer_width.saturating_sub(content_width)) / 2;
    let offset_y = (layer_height.saturating_sub(content_height)) / 2;
    (
        [
            offset_x as f32 / layer_width.max(1) as f32,
            offset_y as f32 / layer_height.max(1) as f32,
        ],
        [
            (offset_x + content_width) as f32 / layer_width.max(1) as f32,
            (offset_y + content_height) as f32 / layer_height.max(1) as f32,
        ],
    )
}

fn load_gui_sprite_logical_rgba(
    asset_root: &str,
    texture_key: &str,
    logical_width: u32,
    logical_height: u32,
) -> io::Result<(u32, u32, Vec<u8>)> {
    let rel_path = relative_asset_path_from_key(texture_key);
    let sprite_path = Path::new(asset_root).join(rel_path);
    let (src_w, src_h, src_rgba) =
        load_png_rgba(&sprite_path).map_err(|err| io::Error::other(err.to_string()))?;
    let scale_x = (src_w / logical_width.max(1)).max(1);
    let scale_y = (src_h / logical_height.max(1)).max(1);
    let scale = scale_x.min(scale_y).max(1);
    let dst_w = (src_w / scale).max(1);
    let dst_h = (src_h / scale).max(1);
    let dst_rgba = if scale > 1 {
        resize_rgba_nearest(&src_rgba, src_w, src_h, dst_w, dst_h)
    } else {
        src_rgba
    };
    Ok((dst_w, dst_h, dst_rgba))
}

fn relative_asset_path_from_key(texture_key: &str) -> &str {
    let without_namespace = texture_key
        .strip_prefix("minecraft:")
        .unwrap_or(texture_key);
    without_namespace
        .strip_prefix('/')
        .unwrap_or(without_namespace)
}

#[cfg(test)]
mod asset_path_tests {
    use super::relative_asset_path_from_key;

    #[test]
    fn strips_minecraft_namespace_from_gui_asset_keys() {
        assert_eq!(
            relative_asset_path_from_key("minecraft:textures/gui/sprites/widget/button.png"),
            "textures/gui/sprites/widget/button.png"
        );
    }

    #[test]
    fn strips_optional_leading_slash_without_restoring_namespace() {
        assert_eq!(
            relative_asset_path_from_key("/textures/gui/sprites/widget/button.png"),
            "textures/gui/sprites/widget/button.png"
        );
    }
}

fn build_hud_atlas(
    source: HudAtlasSource,
    key_to_layer: &HashMap<String, u32>,
    texture_layouts: &HashMap<String, HudTextureLayout>,
) -> HudAtlas {
    match source {
        HudAtlasSource::ClassicWidgets => {
            let key = HUD_GUI_WIDGETS_KEY.to_string();
            let hotbar_layer = *key_to_layer
                .get(&key)
                .unwrap_or_else(|| panic!("HUD atlas key '{}' missing layer mapping", key));
            let widgets_layout = texture_layouts
                .get(&key)
                .unwrap_or_else(|| panic!("HUD atlas key '{}' missing texture layout", key));
            let (hotbar_uv_min, hotbar_uv_max) =
                hud_uv_for_rect(widgets_layout, HUD_HOTBAR_BG_RECT_CLASSIC);
            let (selection_uv_min, selection_uv_max) =
                hud_uv_for_rect(widgets_layout, HUD_HOTBAR_SELECTION_RECT_CLASSIC);
            let hotbar_w = HUD_HOTBAR_BG_RECT_CLASSIC.w as f32;
            let hotbar_h = HUD_HOTBAR_BG_RECT_CLASSIC.h as f32;
            let slot_center_start_x =
                ((hotbar_w - HUD_HOTBAR_SLOT_STRIDE_PX * HUD_HOTBAR_SLOT_COUNT as f32) * 0.5)
                    + HUD_HOTBAR_SLOT_STRIDE_PX * 0.5;
            HudAtlas {
                hotbar_layer,
                selection_layer: hotbar_layer,
                hotbar_uv_min,
                hotbar_uv_max,
                selection_uv_min,
                selection_uv_max,
                hotbar_size_px: [hotbar_w, hotbar_h],
                selection_size_px: [
                    HUD_HOTBAR_SELECTION_RECT_CLASSIC.w as f32,
                    HUD_HOTBAR_SELECTION_RECT_CLASSIC.h as f32,
                ],
                slot_center_start_x_px: slot_center_start_x,
                slot_center_y_px: hotbar_h * 0.5,
                slot_stride_px: HUD_HOTBAR_SLOT_STRIDE_PX,
            }
        }
        HudAtlasSource::ModernSprites => {
            let hotbar_key = HUD_GUI_HOTBAR_KEY.to_string();
            let selection_key = HUD_GUI_HOTBAR_SELECTION_KEY.to_string();
            let hotbar_layer = *key_to_layer
                .get(&hotbar_key)
                .unwrap_or_else(|| panic!("HUD atlas key '{}' missing layer mapping", hotbar_key));
            let selection_layer = *key_to_layer.get(&selection_key).unwrap_or_else(|| {
                panic!("HUD atlas key '{}' missing layer mapping", selection_key)
            });
            let hotbar_layout = texture_layouts
                .get(&hotbar_key)
                .unwrap_or_else(|| panic!("HUD atlas key '{}' missing texture layout", hotbar_key));
            let selection_layout = texture_layouts.get(&selection_key).unwrap_or_else(|| {
                panic!("HUD atlas key '{}' missing texture layout", selection_key)
            });

            let modern_scale_x = (hotbar_layout.content_w / HUD_HOTBAR_BG_RECT_CLASSIC.w).max(1);
            let modern_scale_y = (hotbar_layout.content_h / HUD_HOTBAR_BG_RECT_CLASSIC.h).max(1);
            let modern_scale = modern_scale_x.min(modern_scale_y).max(1);
            let hotbar_logical_w = hotbar_layout.content_w as f32 / modern_scale as f32;
            let hotbar_logical_h = hotbar_layout.content_h as f32 / modern_scale as f32;
            let selection_logical_w = selection_layout.content_w as f32 / modern_scale as f32;
            let selection_logical_h = selection_layout.content_h as f32 / modern_scale as f32;
            let slot_center_start_x = ((hotbar_logical_w
                - HUD_HOTBAR_SLOT_STRIDE_PX * HUD_HOTBAR_SLOT_COUNT as f32)
                * 0.5)
                + HUD_HOTBAR_SLOT_STRIDE_PX * 0.5;

            let (hotbar_uv_min, hotbar_uv_max) = hud_uv_for_rect(
                hotbar_layout,
                GuiRect {
                    x: 0,
                    y: 0,
                    w: hotbar_layout.content_w,
                    h: hotbar_layout.content_h,
                },
            );
            let (selection_uv_min, selection_uv_max) = hud_uv_for_rect(
                selection_layout,
                GuiRect {
                    x: 0,
                    y: 0,
                    w: selection_layout.content_w,
                    h: selection_layout.content_h,
                },
            );
            HudAtlas {
                hotbar_layer,
                selection_layer,
                hotbar_uv_min,
                hotbar_uv_max,
                selection_uv_min,
                selection_uv_max,
                hotbar_size_px: [hotbar_logical_w, hotbar_logical_h],
                selection_size_px: [selection_logical_w, selection_logical_h],
                slot_center_start_x_px: slot_center_start_x,
                slot_center_y_px: hotbar_logical_h * 0.5,
                slot_stride_px: HUD_HOTBAR_SLOT_STRIDE_PX,
            }
        }
    }
}

fn build_inventory_atlas(
    source: InventoryAtlasSource,
    key_to_layer: &HashMap<String, u32>,
    texture_layouts: &HashMap<String, HudTextureLayout>,
) -> InventoryAtlas {
    let key = source.texture_key().to_string();
    let background_layer = *key_to_layer
        .get(&key)
        .unwrap_or_else(|| panic!("inventory atlas key '{}' missing layer mapping", key));
    let texture_layout = texture_layouts
        .get(&key)
        .unwrap_or_else(|| panic!("inventory atlas key '{}' missing texture layout", key));

    let source_size = source.logical_size_px();
    let scale_x = (texture_layout.content_w / source_size[0]).max(1);
    let scale_y = (texture_layout.content_h / source_size[1]).max(1);
    let scale = scale_x.min(scale_y).max(1);

    let (background_uv_min, background_uv_max) = hud_uv_for_rect(
        texture_layout,
        GuiRect {
            x: 0,
            y: 0,
            w: texture_layout.content_w,
            h: texture_layout.content_h,
        },
    );

    InventoryAtlas {
        background_layer,
        background_uv_min,
        background_uv_max,
        background_size_px: [
            texture_layout.content_w as f32 / scale as f32,
            texture_layout.content_h as f32 / scale as f32,
        ],
        grid_origin_px: source.grid_origin_px(),
        hotbar_origin_px: source.hotbar_origin_px(),
        slot_size_px: INVENTORY_SLOT_SIZE_PX,
        slot_stride_px: INVENTORY_SLOT_STRIDE_PX,
        palette_columns: source.palette_columns(),
        palette_rows: source.palette_rows(),
        preview_rect_px: source.preview_rect_px(),
    }
}

fn build_menu_button_atlas(
    key_to_layer: &HashMap<String, u32>,
    worldgen_ui_layer_width: u32,
    worldgen_ui_layer_height: u32,
    normal_size: [u32; 2],
    highlighted_size: [u32; 2],
    disabled_size: [u32; 2],
) -> MenuButtonAtlas {
    let normal_key = MENU_BUTTON_TEXTURE_KEY.to_string();
    let highlighted_key = MENU_BUTTON_HIGHLIGHTED_TEXTURE_KEY.to_string();
    let disabled_key = MENU_BUTTON_DISABLED_TEXTURE_KEY.to_string();
    let normal_layer = *key_to_layer
        .get(&normal_key)
        .unwrap_or_else(|| panic!("menu button key '{}' missing layer mapping", normal_key));
    let highlighted_layer = *key_to_layer.get(&highlighted_key).unwrap_or_else(|| {
        panic!(
            "menu button key '{}' missing layer mapping",
            highlighted_key
        )
    });
    let disabled_layer = *key_to_layer
        .get(&disabled_key)
        .unwrap_or_else(|| panic!("menu button key '{}' missing layer mapping", disabled_key));

    let make_sprite = |layer: u32, size: [u32; 2]| {
        let (uv_min, uv_max) = centered_uv_rect(
            worldgen_ui_layer_width,
            worldgen_ui_layer_height,
            size[0],
            size[1],
        );
        UiSprite {
            layer,
            uv_min,
            uv_max,
            logical_size_px: [size[0] as f32, size[1] as f32],
        }
    };

    MenuButtonAtlas {
        normal: make_sprite(normal_layer, normal_size),
        highlighted: make_sprite(highlighted_layer, highlighted_size),
        disabled: make_sprite(disabled_layer, disabled_size),
        nine_slice_border_px: MENU_BUTTON_NINE_SLICE_BORDER_PX,
    }
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
    pub title: String,
    pub phase: String,
    pub progress01: f32,
    pub stats: WorldLoadStageStats,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WorldLoadStageStats {
    pub generated_columns: usize,
    pub target_columns: usize,
    pub applied_sections: usize,
    pub meshed_sections: usize,
    pub uploaded_sections: usize,
    pub target_sections: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct GenerationOverlay<'a> {
    pub title: &'a str,
    pub phase: &'a str,
    pub progress01: f32,
    pub stats: WorldLoadStageStats,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MenuOverlay<'a> {
    pub title: &'a str,
    pub selected: usize,
    pub pressed: Option<usize>,
    pub items: &'a [&'a str],
    pub enabled: Option<&'a [bool]>,
    pub footer: Option<&'a str>,
    pub show_panorama: bool,
}

#[derive(Clone, Copy, Debug)]
struct GuiRect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

#[derive(Clone, Copy, Debug)]
struct HudTextureLayout {
    frame_size: u32,
    content_offset_x: u32,
    content_offset_y: u32,
    content_w: u32,
    content_h: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HudAtlasSource {
    ClassicWidgets,
    ModernSprites,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InventoryAtlasSource {
    CreativeTabItems,
    SurvivalInventory,
}

#[derive(Clone, Copy, Debug)]
struct HudAtlas {
    hotbar_layer: u32,
    selection_layer: u32,
    hotbar_uv_min: [f32; 2],
    hotbar_uv_max: [f32; 2],
    selection_uv_min: [f32; 2],
    selection_uv_max: [f32; 2],
    hotbar_size_px: [f32; 2],
    selection_size_px: [f32; 2],
    slot_center_start_x_px: f32,
    slot_center_y_px: f32,
    slot_stride_px: f32,
}

#[derive(Clone, Copy, Debug)]
struct InventoryAtlas {
    background_layer: u32,
    background_uv_min: [f32; 2],
    background_uv_max: [f32; 2],
    background_size_px: [f32; 2],
    grid_origin_px: [f32; 2],
    hotbar_origin_px: [f32; 2],
    slot_size_px: f32,
    slot_stride_px: f32,
    palette_columns: usize,
    palette_rows: usize,
    preview_rect_px: GuiRect,
}

#[derive(Clone, Copy, Debug)]
struct UiSprite {
    layer: u32,
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    logical_size_px: [f32; 2],
}

#[derive(Clone, Copy, Debug)]
struct MenuButtonAtlas {
    normal: UiSprite,
    highlighted: UiSprite,
    disabled: UiSprite,
    nine_slice_border_px: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MenuButtonVisualState {
    Normal,
    Hovered,
    Pressed,
    Disabled,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BlockSelectOverlay<'a> {
    pub selected: usize,
    pub blocks: &'a [BlockId],
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct InventoryOverlay<'a> {
    pub palette_blocks: &'a [BlockId],
    pub selected_palette: usize,
    pub selected_hotbar_slot: usize,
    pub page: usize,
    pub hovered_palette: Option<usize>,
    pub hovered_hotbar_slot: Option<usize>,
    pub dragged_block: Option<BlockId>,
    pub cursor_px: [f32; 2],
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DebugOverlay<'a> {
    pub lines: &'a [&'a str],
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ChatOverlayLine<'a> {
    pub text: &'a str,
    pub age_seconds: f32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ChatOverlay<'a> {
    pub recent_lines: &'a [ChatOverlayLine<'a>],
    pub chat_open: bool,
    pub chat_input: &'a str,
    pub chat_input_cursor: usize,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct HitchCounters {
    pub loaded_chunks: usize,
    pub mesh_jobs_queued: usize,
    pub lighting_dirty_chunks: usize,
    pub storage_loads_inflight: usize,
    pub active_sim_columns: usize,
}

#[derive(Clone, Copy, Debug)]
struct FirstPersonArmTweak {
    scale: f32,
    forward_offset: f32,
    side_offset: f32,
    down_offset: f32,
    roll_rad: f32,
    model_rot_x_rad: f32,
    model_rot_y_rad: f32,
    model_rot_z_rad: f32,
}

impl Default for FirstPersonArmTweak {
    fn default() -> Self {
        Self {
            scale: FIRST_PERSON_ARM_SCALE,
            forward_offset: FIRST_PERSON_ARM_FORWARD_OFFSET,
            side_offset: FIRST_PERSON_ARM_SIDE_OFFSET,
            down_offset: FIRST_PERSON_ARM_DOWN_OFFSET,
            roll_rad: FIRST_PERSON_ARM_ROLL_RAD,
            model_rot_x_rad: FIRST_PERSON_ARM_MODEL_ROT_X_RAD,
            model_rot_y_rad: FIRST_PERSON_ARM_MODEL_ROT_Y_RAD,
            model_rot_z_rad: FIRST_PERSON_ARM_MODEL_ROT_Z_RAD,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BlockSelectOverlayCacheKey {
    view_width: u32,
    view_height: u32,
    selected: usize,
    block_count: usize,
    block_ptr: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HotbarOverlayCacheKey {
    view_width: u32,
    view_height: u32,
    selected_slot: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InventoryOverlayCacheKey {
    view_width: u32,
    view_height: u32,
    selected_palette: usize,
    selected_hotbar_slot: usize,
    hovered_palette: Option<usize>,
    hovered_hotbar_slot: Option<usize>,
    page: usize,
    palette_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ChatOverlayCacheKey {
    view_width: u32,
    view_height: u32,
    signature: u64,
}

struct WorldGenerationTask {
    kind: WorldGenerationKind,
    warmup_center_x: i32,
    warmup_center_z: i32,
    startup_plan: StartupPlan,
    pending_progress: Option<WorldGenProgress>,
    last_reported_phase: String,
    last_reported_progress_permille: u16,
    last_reported_stats: WorldLoadStageStats,
    started_at: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorldGenerationKind {
    StartupLoad,
    NewWorld,
}

#[derive(Clone, Copy, Debug)]
struct ItemPreviewDraw {
    block: BlockId,
    viewport_x: u32,
    viewport_y: u32,
    viewport_w: u32,
    viewport_h: u32,
}

impl WorldGenerationKind {
    const fn overlay_title(self) -> &'static str {
        match self {
            Self::StartupLoad => "Loading world",
            Self::NewWorld => "Generating level",
        }
    }
}

struct StartupPlan {
    radius_chunks: i32,
    target_columns: HashSet<ColumnCoord>,
    target_sections: HashSet<ChunkCoord>,
    column_request_order: Vec<ColumnCoord>,
    column_request_cursor: usize,
    columns_inflight: HashSet<ColumnCoord>,
    generated_columns: HashSet<ColumnCoord>,
    applied_sections: HashSet<ChunkCoord>,
    meshed_sections: HashSet<ChunkCoord>,
    uploaded_sections: HashSet<ChunkCoord>,
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
        BlockModel::SlabBottom => {
            let faces = [
                (Face::South, [[0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]]),
                (Face::North, [[1, 0, 0], [0, 0, 0], [0, 1, 0], [1, 1, 0]]),
                (Face::East, [[1, 0, 1], [1, 0, 0], [1, 1, 0], [1, 1, 1]]),
                (Face::West, [[0, 0, 0], [0, 0, 1], [0, 1, 1], [0, 1, 0]]),
                (Face::Up, [[0, 1, 1], [1, 1, 1], [1, 1, 0], [0, 1, 0]]),
                (Face::Down, [[0, 0, 0], [1, 0, 0], [1, 0, 1], [0, 0, 1]]),
            ];
            for (face, corners) in faces {
                let corners_f32 = corners.map(|p| {
                    [
                        p[0] as f32 - 0.5,
                        -0.5 + (p[1] as f32 * 0.5),
                        p[2] as f32 - 0.5,
                    ]
                });
                let mut quad_uvs = preview_face_uvs_for_quad(face, corners);
                if matches!(face, Face::North | Face::South | Face::East | Face::West) {
                    for uv in &mut quad_uvs {
                        uv[1] *= 0.5;
                    }
                }
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

fn uv_rect_px(x: f32, y: f32, w: f32, h: f32, atlas_w: f32, atlas_h: f32) -> ([f32; 2], [f32; 2]) {
    let inset_u = 0.5 / atlas_w.max(1.0);
    let inset_v = 0.5 / atlas_h.max(1.0);
    let u0 = (x / atlas_w + inset_u).clamp(0.0, 1.0);
    let v0 = (y / atlas_h + inset_v).clamp(0.0, 1.0);
    let u1 = ((x + w) / atlas_w - inset_u).clamp(0.0, 1.0);
    let v1 = ((y + h) / atlas_h - inset_v).clamp(0.0, 1.0);
    ([u0, v0], [u1, v1])
}

fn uv_quad_from_rect(uv_min: [f32; 2], uv_max: [f32; 2]) -> [[f32; 2]; 4] {
    [
        [uv_min[0], uv_max[1]],
        [uv_max[0], uv_max[1]],
        [uv_max[0], uv_min[1]],
        [uv_min[0], uv_min[1]],
    ]
}

fn build_first_person_arm_mesh(skin_layer: u32) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(256);
    let mut indices = Vec::with_capacity(384);
    let w = FIRST_PERSON_ARM_WIDTH;
    let h = FIRST_PERSON_ARM_HEIGHT;
    let d = FIRST_PERSON_ARM_DEPTH;
    let x0 = -w * 0.5;
    let x1 = w * 0.5;
    let y0 = -h;
    let y1 = 0.0;
    let z0 = -d * 0.5;
    let z1 = d * 0.5;

    // Steve 64x64 right-arm layout:
    // x=40..44 right, 44..48 front, 48..52 left, 52..56 back; y=16..20 top, 20..32 side rows.
    let (front_uv_min, front_uv_max) = uv_rect_px(
        44.0,
        20.0,
        4.0,
        12.0,
        FIRST_PERSON_ARM_ATLAS_SIZE_PX,
        FIRST_PERSON_ARM_ATLAS_SIZE_PX,
    );
    let (back_uv_min, back_uv_max) = uv_rect_px(
        52.0,
        20.0,
        4.0,
        12.0,
        FIRST_PERSON_ARM_ATLAS_SIZE_PX,
        FIRST_PERSON_ARM_ATLAS_SIZE_PX,
    );
    let (left_uv_min, left_uv_max) = uv_rect_px(
        48.0,
        20.0,
        4.0,
        12.0,
        FIRST_PERSON_ARM_ATLAS_SIZE_PX,
        FIRST_PERSON_ARM_ATLAS_SIZE_PX,
    );
    let (right_uv_min, right_uv_max) = uv_rect_px(
        40.0,
        20.0,
        4.0,
        12.0,
        FIRST_PERSON_ARM_ATLAS_SIZE_PX,
        FIRST_PERSON_ARM_ATLAS_SIZE_PX,
    );
    let (top_uv_min, top_uv_max) = uv_rect_px(
        44.0,
        16.0,
        4.0,
        4.0,
        FIRST_PERSON_ARM_ATLAS_SIZE_PX,
        FIRST_PERSON_ARM_ATLAS_SIZE_PX,
    );
    let (bottom_uv_min, bottom_uv_max) = uv_rect_px(
        48.0,
        16.0,
        4.0,
        4.0,
        FIRST_PERSON_ARM_ATLAS_SIZE_PX,
        FIRST_PERSON_ARM_ATLAS_SIZE_PX,
    );

    append_preview_textured_quad(
        &mut vertices,
        &mut indices,
        [[x0, y0, z1], [x1, y0, z1], [x1, y1, z1], [x0, y1, z1]],
        uv_quad_from_rect(front_uv_min, front_uv_max),
        skin_layer,
        TINT_NONE,
        FULLBRIGHT_LIGHT_PACKED,
    );
    append_preview_textured_quad(
        &mut vertices,
        &mut indices,
        [[x1, y0, z0], [x0, y0, z0], [x0, y1, z0], [x1, y1, z0]],
        uv_quad_from_rect(back_uv_min, back_uv_max),
        skin_layer,
        TINT_NONE,
        FULLBRIGHT_LIGHT_PACKED,
    );
    append_preview_textured_quad(
        &mut vertices,
        &mut indices,
        [[x1, y0, z1], [x1, y0, z0], [x1, y1, z0], [x1, y1, z1]],
        uv_quad_from_rect(right_uv_min, right_uv_max),
        skin_layer,
        TINT_NONE,
        FULLBRIGHT_LIGHT_PACKED,
    );
    append_preview_textured_quad(
        &mut vertices,
        &mut indices,
        [[x0, y0, z0], [x0, y0, z1], [x0, y1, z1], [x0, y1, z0]],
        uv_quad_from_rect(left_uv_min, left_uv_max),
        skin_layer,
        TINT_NONE,
        FULLBRIGHT_LIGHT_PACKED,
    );
    append_preview_textured_quad(
        &mut vertices,
        &mut indices,
        [[x0, y1, z1], [x1, y1, z1], [x1, y1, z0], [x0, y1, z0]],
        uv_quad_from_rect(top_uv_min, top_uv_max),
        skin_layer,
        TINT_NONE,
        FULLBRIGHT_LIGHT_PACKED,
    );
    append_preview_textured_quad(
        &mut vertices,
        &mut indices,
        [[x0, y0, z0], [x1, y0, z0], [x1, y0, z1], [x0, y0, z1]],
        uv_quad_from_rect(bottom_uv_min, bottom_uv_max),
        skin_layer,
        TINT_NONE,
        FULLBRIGHT_LIGHT_PACKED,
    );

    (vertices, indices)
}

fn append_overlay_mesh(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    mut add_vertices: Vec<Vertex>,
    mut add_indices: Vec<u32>,
) {
    let base = vertices.len() as u32;
    for index in &mut add_indices {
        *index += base;
    }
    vertices.append(&mut add_vertices);
    indices.append(&mut add_indices);
}

fn transform_overlay_vertices(vertices: &mut [Vertex], basis: Mat3, origin: Vec3, scale: f32) {
    for vertex in vertices {
        let local = Vec3::new(vertex.position[0], vertex.position[1], vertex.position[2]) * scale;
        let world = origin + basis * local;
        vertex.position = world.to_array();
    }
}

fn build_first_person_overlay_mesh(
    camera: &Camera,
    texture_lookup: &BlockTextureLookup,
    skin_layer: u32,
    arm_tweak: FirstPersonArmTweak,
    held_block: Option<BlockId>,
) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(1024);
    let mut indices = Vec::with_capacity(1536);
    let forward = camera.forward();
    let right = camera.right();
    let up = right.cross(forward).normalize();

    let arm_direction = (forward * FIRST_PERSON_ARM_DIR_FORWARD_WEIGHT
        - right * FIRST_PERSON_ARM_DIR_INWARD_WEIGHT
        - up * FIRST_PERSON_ARM_DIR_DOWN_WEIGHT)
        .normalize();
    let arm_side = up.cross(arm_direction).normalize();
    let arm_up = arm_direction.cross(arm_side).normalize();
    let arm_basis = Mat3::from_cols(arm_side, arm_up, arm_direction)
        * Mat3::from_rotation_z(arm_tweak.roll_rad);
    let arm_origin =
        camera.position + forward * arm_tweak.forward_offset + right * arm_tweak.side_offset
            - up * arm_tweak.down_offset;

    let (mut arm_vertices, arm_indices) = build_first_person_arm_mesh(skin_layer);
    for vertex in &mut arm_vertices {
        vertex.tint_kind = UI_MODE_TEXTURED;
    }
    // Stable application order: X first, then Y, then Z (Rz * Ry * Rx with column vectors).
    let arm_model_rotation = Mat3::from_rotation_z(arm_tweak.model_rot_z_rad)
        * Mat3::from_rotation_y(arm_tweak.model_rot_y_rad)
        * Mat3::from_rotation_x(arm_tweak.model_rot_x_rad);
    for vertex in &mut arm_vertices {
        let rotated = arm_model_rotation
            * Vec3::new(vertex.position[0], vertex.position[1], vertex.position[2]);
        vertex.position = rotated.to_array();
    }
    transform_overlay_vertices(&mut arm_vertices, arm_basis, arm_origin, arm_tweak.scale);
    append_overlay_mesh(&mut vertices, &mut indices, arm_vertices, arm_indices);

    if let Some(block) = held_block {
        let (mut held_vertices, held_indices) = build_block_preview_mesh_3d(block, texture_lookup);
        for vertex in &mut held_vertices {
            vertex.tint_kind = UI_MODE_TEXTURED;
        }
        // Held item lives in the hand frame and keeps a stable orientation relative to that frame.
        let held_pose = Mat3::from_rotation_y(FIRST_PERSON_HELD_ITEM_YAW_RAD)
            * Mat3::from_rotation_x(FIRST_PERSON_HELD_ITEM_PITCH_RAD);
        let held_basis = arm_basis * held_pose;

        // Anchor at the upper/front hand tip (corner), not inside the hand volume.
        let hand_anchor_local = arm_model_rotation
            * Vec3::new(
                FIRST_PERSON_ARM_WIDTH * 0.5,
                0.0,
                FIRST_PERSON_ARM_DEPTH * 0.5,
            );
        let hand_anchor_world = arm_origin + arm_basis * (hand_anchor_local * arm_tweak.scale);
        // Shift item center so the hand anchor touches the block's back-top edge.
        let grip_to_center = Vec3::new(
            0.0,
            FIRST_PERSON_HELD_ITEM_GRIP_TO_CENTER_UP * FIRST_PERSON_HELD_ITEM_SCALE,
            FIRST_PERSON_HELD_ITEM_GRIP_TO_CENTER_FORWARD * FIRST_PERSON_HELD_ITEM_SCALE,
        );
        let held_origin = hand_anchor_world
            + held_basis * grip_to_center
            + held_basis
                * Vec3::new(
                    FIRST_PERSON_HELD_ITEM_SIDE_OFFSET,
                    FIRST_PERSON_HELD_ITEM_DOWN_OFFSET,
                    FIRST_PERSON_HELD_ITEM_FORWARD_OFFSET,
                )
            + forward * FIRST_PERSON_HELD_ITEM_CAMERA_FORWARD_BIAS;
        transform_overlay_vertices(
            &mut held_vertices,
            held_basis,
            held_origin,
            FIRST_PERSON_HELD_ITEM_SCALE,
        );
        append_overlay_mesh(&mut vertices, &mut indices, held_vertices, held_indices);
    }

    (vertices, indices)
}

fn preview_rect_px_from_center(
    view_width: u32,
    view_height: u32,
    center_x: f32,
    center_y: f32,
    size_px: f32,
) -> Option<(u32, u32, u32, u32)> {
    let half = size_px.max(1.0) * 0.5;
    let x0 = (center_x - half).max(0.0).round() as u32;
    let y0 = (center_y - half).max(0.0).round() as u32;
    let x1 = (center_x + half).min(view_width as f32).round() as u32;
    let y1 = (center_y + half).min(view_height as f32).round() as u32;
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some((x0, y0, x1 - x0, y1 - y0))
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

fn append_ui_solid_alpha_quad(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    min: [f32; 2],
    max: [f32; 2],
    color: [f32; 3],
    alpha: f32,
) {
    let alpha_byte = (alpha.clamp(0.0, 1.0) * 255.0).round() as u32;
    let base = vertices.len() as u32;
    let z = 0.0;
    vertices.push(Vertex {
        position: [min[0], min[1], z],
        uv: [0.0, 0.0],
        tex_layer: 0,
        tint_kind: UI_MODE_SOLID_ALPHA,
        light_packed: alpha_byte,
        color,
    });
    vertices.push(Vertex {
        position: [max[0], min[1], z],
        uv: [0.0, 0.0],
        tex_layer: 0,
        tint_kind: UI_MODE_SOLID_ALPHA,
        light_packed: alpha_byte,
        color,
    });
    vertices.push(Vertex {
        position: [max[0], max[1], z],
        uv: [0.0, 0.0],
        tex_layer: 0,
        tint_kind: UI_MODE_SOLID_ALPHA,
        light_packed: alpha_byte,
        color,
    });
    vertices.push(Vertex {
        position: [min[0], max[1], z],
        uv: [0.0, 0.0],
        tex_layer: 0,
        tint_kind: UI_MODE_SOLID_ALPHA,
        light_packed: alpha_byte,
        color,
    });
    indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
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

fn append_ui_solid_alpha_quad_px(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    view_width: u32,
    view_height: u32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: [f32; 3],
    alpha: f32,
) {
    let min = [px_to_ndc_x(view_width, x), px_to_ndc_y(view_height, y + h)];
    let max = [px_to_ndc_x(view_width, x + w), px_to_ndc_y(view_height, y)];
    append_ui_solid_alpha_quad(vertices, indices, min, max, color, alpha);
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

fn append_ui_textured_nine_slice_px(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    view_width: u32,
    view_height: u32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    sprite: UiSprite,
    border_px: f32,
    color: [f32; 3],
) {
    let left = border_px.min(w * 0.5).max(0.0);
    let right = border_px.min(w * 0.5).max(0.0);
    let top = border_px.min(h * 0.5).max(0.0);
    let bottom = border_px.min(h * 0.5).max(0.0);
    let x1 = (x + left).min(x + w);
    let x2 = (x + w - right).max(x1);
    let y1 = (y + top).min(y + h);
    let y2 = (y + h - bottom).max(y1);
    let xs = [x, x1, x2, x + w];
    let ys = [y, y1, y2, y + h];

    let u_span = sprite.uv_max[0] - sprite.uv_min[0];
    let v_span = sprite.uv_max[1] - sprite.uv_min[1];
    let u1 = sprite.uv_min[0] + u_span * (left / sprite.logical_size_px[0].max(1.0));
    let u2 = sprite.uv_max[0] - u_span * (right / sprite.logical_size_px[0].max(1.0));
    let v1 = sprite.uv_min[1] + v_span * (top / sprite.logical_size_px[1].max(1.0));
    let v2 = sprite.uv_max[1] - v_span * (bottom / sprite.logical_size_px[1].max(1.0));
    let us = [sprite.uv_min[0], u1, u2, sprite.uv_max[0]];
    let vs = [sprite.uv_min[1], v1, v2, sprite.uv_max[1]];

    for row in 0..3 {
        let quad_h = ys[row + 1] - ys[row];
        if quad_h <= 0.0 {
            continue;
        }
        for col in 0..3 {
            let quad_w = xs[col + 1] - xs[col];
            if quad_w <= 0.0 {
                continue;
            }
            append_ui_textured_quad_px(
                vertices,
                indices,
                view_width,
                view_height,
                xs[col],
                ys[row],
                quad_w,
                quad_h,
                [us[col], vs[row]],
                [us[col + 1], vs[row + 1]],
                sprite.layer,
                color,
            );
        }
    }
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

fn append_bitmap_text_with_shadow_color(
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
    let shadow = [color[0] * 0.25, color[1] * 0.25, color[2] * 0.25];
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
        shadow,
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
        color,
    );
}

fn build_generating_screen_mesh(
    view_width: u32,
    view_height: u32,
    dirt_layer: u32,
    font: &BitmapFont,
    title: &str,
    progress01: f32,
    phase: &str,
    stats: WorldLoadStageStats,
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

    let title_x = cx - bitmap_text_width_px(font, title, WORLDGEN_FONT_SCALE) * 0.5;
    let title_y = cy - 56.0;
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
    let phase_y = cy - 20.0;
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
    let bar_y = cy + 8.0;
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

    let detail_scale = 1.0;
    let detail_lines = [
        format!(
            "Critical columns: {}/{}",
            stats.generated_columns, stats.target_columns
        ),
        format!(
            "Critical chunks generated: {}/{}",
            stats.applied_sections, stats.target_sections
        ),
        format!(
            "Critical chunks meshed: {}/{}",
            stats.meshed_sections, stats.target_sections
        ),
        format!(
            "Critical chunks uploaded: {}/{}",
            stats.uploaded_sections, stats.target_sections
        ),
    ];
    let mut detail_y = bar_y + 18.0;
    for line in &detail_lines {
        let line_x = cx - bitmap_text_width_px(font, line, detail_scale) * 0.5;
        append_bitmap_text_with_shadow(
            &mut vertices,
            &mut indices,
            view_width,
            view_height,
            font,
            line,
            line_x,
            detail_y,
            detail_scale,
        );
        detail_y += 12.0;
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

#[derive(Clone, Copy, Debug)]
struct InventoryLayoutPx {
    scale: f32,
    background_x: f32,
    background_y: f32,
    background_w: f32,
    background_h: f32,
    palette_grid_x: f32,
    palette_grid_y: f32,
    hotbar_row_x: f32,
    hotbar_row_y: f32,
    slot_size: f32,
    slot_stride: f32,
    preview_x: f32,
    preview_y: f32,
    preview_w: f32,
    preview_h: f32,
}

fn inventory_page_size(atlas: &InventoryAtlas) -> usize {
    atlas.palette_columns.max(1) * atlas.palette_rows.max(1)
}

fn inventory_page_count(item_count: usize, atlas: &InventoryAtlas) -> usize {
    if item_count == 0 {
        return 1;
    }
    item_count
        .div_ceil(inventory_page_size(atlas).max(1))
        .max(1)
}

fn inventory_layout_px(
    view_width: u32,
    view_height: u32,
    atlas: &InventoryAtlas,
) -> Option<InventoryLayoutPx> {
    if atlas.background_size_px[0] <= 0.0 || atlas.background_size_px[1] <= 0.0 {
        return None;
    }

    let max_scale_x = (view_width as f32 / atlas.background_size_px[0]).floor();
    let max_scale_y = (view_height as f32 / atlas.background_size_px[1]).floor();
    let max_scale = max_scale_x.min(max_scale_y);
    let scale = if max_scale >= 1.0 {
        INVENTORY_GUI_SCALE.min(max_scale).max(1.0)
    } else {
        (view_width as f32 / atlas.background_size_px[0])
            .min(view_height as f32 / atlas.background_size_px[1])
            .max(0.5)
    };

    let background_w = atlas.background_size_px[0] * scale;
    let background_h = atlas.background_size_px[1] * scale;
    let background_x = ((view_width as f32 - background_w) * 0.5).round();
    let background_y = ((view_height as f32 - background_h) * 0.5).round();
    let slot_size = (atlas.slot_size_px * scale).round();
    let slot_stride = (atlas.slot_stride_px * scale).round();

    Some(InventoryLayoutPx {
        scale,
        background_x,
        background_y,
        background_w,
        background_h,
        palette_grid_x: background_x + atlas.grid_origin_px[0] * scale,
        palette_grid_y: background_y + atlas.grid_origin_px[1] * scale,
        hotbar_row_x: background_x + atlas.hotbar_origin_px[0] * scale,
        hotbar_row_y: background_y + atlas.hotbar_origin_px[1] * scale,
        slot_size,
        slot_stride,
        preview_x: background_x + atlas.preview_rect_px.x as f32 * scale,
        preview_y: background_y + atlas.preview_rect_px.y as f32 * scale,
        preview_w: (atlas.preview_rect_px.w as f32 * scale).max(1.0).round(),
        preview_h: (atlas.preview_rect_px.h as f32 * scale).max(1.0).round(),
    })
}

fn inventory_hotbar_slot_at_pos_px_internal(
    view_width: u32,
    view_height: u32,
    atlas: &InventoryAtlas,
    x: f32,
    y: f32,
) -> Option<usize> {
    let layout = inventory_layout_px(view_width, view_height, atlas)?;
    for slot in 0..INVENTORY_SLOT_COUNT {
        let slot_x = layout.hotbar_row_x + slot as f32 * layout.slot_stride;
        let slot_y = layout.hotbar_row_y;
        if x >= slot_x
            && x <= slot_x + layout.slot_size
            && y >= slot_y
            && y <= slot_y + layout.slot_size
        {
            return Some(slot);
        }
    }
    None
}

fn inventory_palette_item_at_pos_px_internal(
    view_width: u32,
    view_height: u32,
    atlas: &InventoryAtlas,
    item_count: usize,
    page: usize,
    x: f32,
    y: f32,
) -> Option<usize> {
    if item_count == 0 {
        return None;
    }
    let layout = inventory_layout_px(view_width, view_height, atlas)?;
    let page_size = inventory_page_size(atlas).max(1);
    let page_count = inventory_page_count(item_count, atlas);
    let page = page.min(page_count.saturating_sub(1));
    let page_start = page * page_size;
    let page_end = (page_start + page_size).min(item_count);

    for index in page_start..page_end {
        let local = index - page_start;
        let row = local / atlas.palette_columns.max(1);
        let col = local % atlas.palette_columns.max(1);
        let slot_x = layout.palette_grid_x + col as f32 * layout.slot_stride;
        let slot_y = layout.palette_grid_y + row as f32 * layout.slot_stride;
        if x >= slot_x
            && x <= slot_x + layout.slot_size
            && y >= slot_y
            && y <= slot_y + layout.slot_size
        {
            return Some(index);
        }
    }

    None
}

fn inventory_preview_viewport_rect_px(
    view_width: u32,
    view_height: u32,
    atlas: &InventoryAtlas,
) -> Option<(u32, u32, u32, u32)> {
    let layout = inventory_layout_px(view_width, view_height, atlas)?;
    let x = layout.preview_x.max(0.0).round() as u32;
    let y = layout.preview_y.max(0.0).round() as u32;
    let w = layout
        .preview_w
        .max(1.0)
        .round()
        .min((view_width as f32 - x as f32).max(1.0)) as u32;
    let h = layout
        .preview_h
        .max(1.0)
        .round()
        .min((view_height as f32 - y as f32).max(1.0)) as u32;
    Some((x, y, w.max(1), h.max(1)))
}

#[derive(Clone, Copy, Debug)]
struct BlockSelectLayout {
    panel_x: f32,
    panel_y: f32,
    panel_w: f32,
    panel_h: f32,
    grid_x: f32,
    grid_y: f32,
}

fn block_select_layout_px(
    view_width: u32,
    view_height: u32,
    item_count: usize,
) -> Option<BlockSelectLayout> {
    if item_count == 0 {
        return None;
    }
    let columns = BLOCK_SELECT_COLUMNS.max(1);
    let rows = item_count.div_ceil(columns);
    let grid_w = columns as f32 * BLOCK_SELECT_CELL_SIZE_PX
        + (columns.saturating_sub(1)) as f32 * BLOCK_SELECT_CELL_GAP_PX;
    let grid_h = rows as f32 * BLOCK_SELECT_CELL_SIZE_PX
        + (rows.saturating_sub(1)) as f32 * BLOCK_SELECT_CELL_GAP_PX;
    let panel_w = grid_w + BLOCK_SELECT_PANEL_PAD_PX * 2.0;
    let panel_h = grid_h + BLOCK_SELECT_PANEL_PAD_PX * 2.0;
    let panel_x = view_width as f32 * 0.5 - panel_w * 0.5;
    let panel_y = view_height as f32 * 0.5 - panel_h * 0.5;

    Some(BlockSelectLayout {
        panel_x,
        panel_y,
        panel_w,
        panel_h,
        grid_x: panel_x + BLOCK_SELECT_PANEL_PAD_PX,
        grid_y: panel_y + BLOCK_SELECT_PANEL_PAD_PX,
    })
}

fn block_select_cell_rect_px(
    view_width: u32,
    view_height: u32,
    item_count: usize,
    index: usize,
) -> Option<(f32, f32, f32, f32)> {
    if item_count == 0 || index >= item_count {
        return None;
    }
    let layout = block_select_layout_px(view_width, view_height, item_count)?;
    let columns = BLOCK_SELECT_COLUMNS.max(1);
    let row = index / columns;
    let col = index % columns;
    let x = layout.grid_x + col as f32 * (BLOCK_SELECT_CELL_SIZE_PX + BLOCK_SELECT_CELL_GAP_PX);
    let y = layout.grid_y + row as f32 * (BLOCK_SELECT_CELL_SIZE_PX + BLOCK_SELECT_CELL_GAP_PX);
    Some((x, y, BLOCK_SELECT_CELL_SIZE_PX, BLOCK_SELECT_CELL_SIZE_PX))
}

pub(crate) fn block_select_item_at_pos_px(
    view_width: u32,
    view_height: u32,
    item_count: usize,
    x: f32,
    y: f32,
) -> Option<usize> {
    for index in 0..item_count {
        let Some((bx, by, bw, bh)) =
            block_select_cell_rect_px(view_width, view_height, item_count, index)
        else {
            continue;
        };
        if x >= bx && x <= bx + bw && y >= by && y <= by + bh {
            return Some(index);
        }
    }
    None
}

fn build_block_select_overlay_mesh(
    view_width: u32,
    view_height: u32,
    texture_lookup: &BlockTextureLookup,
    selected: usize,
    blocks: &[BlockId],
) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(8192);
    let mut indices = Vec::with_capacity(12288);

    let Some(layout) = block_select_layout_px(view_width, view_height, blocks.len()) else {
        return (vertices, indices);
    };

    append_ui_solid_quad_px(
        &mut vertices,
        &mut indices,
        view_width,
        view_height,
        layout.panel_x - 1.0,
        layout.panel_y - 1.0,
        layout.panel_w + 2.0,
        layout.panel_h + 2.0,
        BLOCK_SELECT_PANEL_BORDER,
    );
    append_ui_solid_quad_px(
        &mut vertices,
        &mut indices,
        view_width,
        view_height,
        layout.panel_x,
        layout.panel_y,
        layout.panel_w,
        layout.panel_h,
        BLOCK_SELECT_PANEL_BG,
    );

    let selected_index = selected.min(blocks.len().saturating_sub(1));
    for (index, block) in blocks.iter().copied().enumerate() {
        let Some((x, y, w, h)) =
            block_select_cell_rect_px(view_width, view_height, blocks.len(), index)
        else {
            continue;
        };
        append_ui_solid_quad_px(
            &mut vertices,
            &mut indices,
            view_width,
            view_height,
            x,
            y,
            w,
            h,
            BLOCK_SELECT_CELL_BORDER,
        );
        append_ui_solid_quad_px(
            &mut vertices,
            &mut indices,
            view_width,
            view_height,
            x + 1.0,
            y + 1.0,
            w - 2.0,
            h - 2.0,
            if index == selected_index {
                BLOCK_SELECT_CELL_SELECTED
            } else {
                BLOCK_SELECT_CELL_BG
            },
        );

        let icon_pad = 5.0;
        let icon_size = (w - icon_pad * 2.0).max(1.0);
        let icon_face = if matches!(texture_lookup.model_for(block), BlockModel::CrossPlant) {
            Face::North
        } else {
            Face::Up
        };
        let icon_layer = texture_lookup.layer_for(block, icon_face);
        append_ui_textured_quad_px(
            &mut vertices,
            &mut indices,
            view_width,
            view_height,
            x + icon_pad,
            y + icon_pad,
            icon_size,
            icon_size,
            [0.0, 0.0],
            [1.0, 1.0],
            icon_layer,
            [1.0, 1.0, 1.0],
        );
    }

    (vertices, indices)
}

fn collect_hotbar_item_preview_draws(
    view_width: u32,
    view_height: u32,
    hud_atlas: &HudAtlas,
    hotbar: &[Option<BlockId>; HUD_HOTBAR_SLOT_COUNT],
) -> Vec<ItemPreviewDraw> {
    let mut draws = Vec::with_capacity(HUD_HOTBAR_SLOT_COUNT);
    let scale = HUD_HOTBAR_UI_SCALE.max(1.0);
    let hotbar_w = hud_atlas.hotbar_size_px[0] * scale;
    let hotbar_h = hud_atlas.hotbar_size_px[1] * scale;
    let hotbar_x = ((view_width as f32 - hotbar_w) * 0.5).round();
    let hotbar_y = (view_height as f32 - hotbar_h - HUD_HOTBAR_MARGIN_BOTTOM_PX * scale).round();
    let preview_size = ((hud_atlas.selection_size_px[0].min(hud_atlas.selection_size_px[1])
        - HUD_ITEM_PREVIEW_INSET_PX)
        * scale)
        .max(1.0);

    for (slot_index, block) in hotbar.iter().enumerate() {
        let Some(block) = block else {
            continue;
        };
        let center_x = hotbar_x
            + (hud_atlas.slot_center_start_x_px + slot_index as f32 * hud_atlas.slot_stride_px)
                * scale;
        let center_y = hotbar_y + hud_atlas.slot_center_y_px * scale;
        if let Some((x, y, w, h)) =
            preview_rect_px_from_center(view_width, view_height, center_x, center_y, preview_size)
        {
            draws.push(ItemPreviewDraw {
                block: *block,
                viewport_x: x,
                viewport_y: y,
                viewport_w: w,
                viewport_h: h,
            });
        }
    }

    draws
}

fn build_hotbar_overlay_mesh(
    view_width: u32,
    view_height: u32,
    hud_atlas: &HudAtlas,
    selected_slot: usize,
) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(2048);
    let mut indices = Vec::with_capacity(3072);

    let scale = HUD_HOTBAR_UI_SCALE.max(1.0);
    let hotbar_w = hud_atlas.hotbar_size_px[0] * scale;
    let hotbar_h = hud_atlas.hotbar_size_px[1] * scale;
    let hotbar_x = ((view_width as f32 - hotbar_w) * 0.5).round();
    let hotbar_y = (view_height as f32 - hotbar_h - HUD_HOTBAR_MARGIN_BOTTOM_PX * scale).round();

    append_ui_textured_quad_px(
        &mut vertices,
        &mut indices,
        view_width,
        view_height,
        hotbar_x,
        hotbar_y,
        hotbar_w,
        hotbar_h,
        hud_atlas.hotbar_uv_min,
        hud_atlas.hotbar_uv_max,
        hud_atlas.hotbar_layer,
        [1.0, 1.0, 1.0],
    );

    let selected = selected_slot.min(HUD_HOTBAR_SLOT_COUNT.saturating_sub(1));
    let selected_center_x = hotbar_x
        + (hud_atlas.slot_center_start_x_px + selected as f32 * hud_atlas.slot_stride_px) * scale;
    let selected_center_y = hotbar_y + hud_atlas.slot_center_y_px * scale;
    let selection_w = hud_atlas.selection_size_px[0] * scale;
    let selection_h = hud_atlas.selection_size_px[1] * scale;
    let selection_x = (selected_center_x - selection_w * 0.5).round();
    let selection_y = (selected_center_y - selection_h * 0.5).round();
    append_ui_textured_quad_px(
        &mut vertices,
        &mut indices,
        view_width,
        view_height,
        selection_x,
        selection_y,
        selection_w,
        selection_h,
        hud_atlas.selection_uv_min,
        hud_atlas.selection_uv_max,
        hud_atlas.selection_layer,
        [1.0, 1.0, 1.0],
    );

    (vertices, indices)
}

fn build_inventory_overlay_mesh(
    view_width: u32,
    view_height: u32,
    hud_atlas: &HudAtlas,
    inventory_atlas: &InventoryAtlas,
    overlay: InventoryOverlay<'_>,
) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(8192);
    let mut indices = Vec::with_capacity(12288);

    let Some(layout) = inventory_layout_px(view_width, view_height, inventory_atlas) else {
        return (vertices, indices);
    };

    append_ui_textured_quad_px(
        &mut vertices,
        &mut indices,
        view_width,
        view_height,
        layout.background_x,
        layout.background_y,
        layout.background_w,
        layout.background_h,
        inventory_atlas.background_uv_min,
        inventory_atlas.background_uv_max,
        inventory_atlas.background_layer,
        [1.0, 1.0, 1.0],
    );

    let selection_w = (layout.slot_size * INVENTORY_SELECTION_SIZE_FACTOR).round();
    let selection_h = (layout.slot_size * INVENTORY_SELECTION_SIZE_FACTOR).round();
    let mut draw_hotbar_highlight = |slot: usize, color: [f32; 3]| {
        let slot = slot.min(INVENTORY_SLOT_COUNT.saturating_sub(1));
        let center_x =
            layout.hotbar_row_x + slot as f32 * layout.slot_stride + layout.slot_size * 0.5;
        let center_y = layout.hotbar_row_y + layout.slot_size * 0.5;
        append_ui_textured_quad_px(
            &mut vertices,
            &mut indices,
            view_width,
            view_height,
            (center_x - selection_w * 0.5).round(),
            (center_y - selection_h * 0.5).round(),
            selection_w,
            selection_h,
            hud_atlas.selection_uv_min,
            hud_atlas.selection_uv_max,
            hud_atlas.selection_layer,
            color,
        );
    };

    if overlay.dragged_block.is_some()
        && let Some(hovered_slot) = overlay.hovered_hotbar_slot
    {
        draw_hotbar_highlight(hovered_slot, [0.85, 0.85, 0.85]);
    }
    let selected_hotbar_slot = overlay
        .selected_hotbar_slot
        .min(INVENTORY_SLOT_COUNT.saturating_sub(1));
    draw_hotbar_highlight(selected_hotbar_slot, [1.0, 1.0, 1.0]);

    let page_size = inventory_page_size(inventory_atlas).max(1);
    let page_count = inventory_page_count(overlay.palette_blocks.len(), inventory_atlas);
    let page = overlay.page.min(page_count.saturating_sub(1));
    let page_start = page * page_size;
    let page_end = (page_start + page_size).min(overlay.palette_blocks.len());

    let mut draw_palette_highlight = |global_index: usize, color: [f32; 3]| {
        if global_index < page_start || global_index >= page_end {
            return;
        }
        let local = global_index - page_start;
        let row = local / inventory_atlas.palette_columns.max(1);
        let col = local % inventory_atlas.palette_columns.max(1);
        let center_x =
            layout.palette_grid_x + col as f32 * layout.slot_stride + layout.slot_size * 0.5;
        let center_y =
            layout.palette_grid_y + row as f32 * layout.slot_stride + layout.slot_size * 0.5;
        append_ui_textured_quad_px(
            &mut vertices,
            &mut indices,
            view_width,
            view_height,
            (center_x - selection_w * 0.5).round(),
            (center_y - selection_h * 0.5).round(),
            selection_w,
            selection_h,
            hud_atlas.selection_uv_min,
            hud_atlas.selection_uv_max,
            hud_atlas.selection_layer,
            color,
        );
    };

    if let Some(hovered) = overlay.hovered_palette {
        draw_palette_highlight(hovered, [0.9, 0.9, 0.9]);
    }
    draw_palette_highlight(overlay.selected_palette, [1.0, 1.0, 1.0]);

    (vertices, indices)
}

fn collect_inventory_item_preview_draws(
    view_width: u32,
    view_height: u32,
    inventory_atlas: &InventoryAtlas,
    overlay: InventoryOverlay<'_>,
    hotbar: &[Option<BlockId>; INVENTORY_SLOT_COUNT],
) -> Vec<ItemPreviewDraw> {
    let Some(layout) = inventory_layout_px(view_width, view_height, inventory_atlas) else {
        return Vec::new();
    };
    let mut draws =
        Vec::with_capacity(INVENTORY_SLOT_COUNT + inventory_page_size(inventory_atlas).max(1) + 1);
    let preview_size = (layout.slot_size - INVENTORY_ITEM_PREVIEW_INSET_PX * layout.scale).max(1.0);

    for (slot_index, block) in hotbar.iter().enumerate() {
        let Some(block) = block else {
            continue;
        };
        let center_x =
            layout.hotbar_row_x + slot_index as f32 * layout.slot_stride + layout.slot_size * 0.5;
        let center_y = layout.hotbar_row_y + layout.slot_size * 0.5;
        if let Some((x, y, w, h)) =
            preview_rect_px_from_center(view_width, view_height, center_x, center_y, preview_size)
        {
            draws.push(ItemPreviewDraw {
                block: *block,
                viewport_x: x,
                viewport_y: y,
                viewport_w: w,
                viewport_h: h,
            });
        }
    }

    let page_size = inventory_page_size(inventory_atlas).max(1);
    let page_count = inventory_page_count(overlay.palette_blocks.len(), inventory_atlas);
    let page = overlay.page.min(page_count.saturating_sub(1));
    let page_start = page * page_size;
    let page_end = (page_start + page_size).min(overlay.palette_blocks.len());

    for index in page_start..page_end {
        let block = overlay.palette_blocks[index];
        let local = index - page_start;
        let row = local / inventory_atlas.palette_columns.max(1);
        let col = local % inventory_atlas.palette_columns.max(1);
        let center_x =
            layout.palette_grid_x + col as f32 * layout.slot_stride + layout.slot_size * 0.5;
        let center_y =
            layout.palette_grid_y + row as f32 * layout.slot_stride + layout.slot_size * 0.5;
        if let Some((x, y, w, h)) =
            preview_rect_px_from_center(view_width, view_height, center_x, center_y, preview_size)
        {
            draws.push(ItemPreviewDraw {
                block,
                viewport_x: x,
                viewport_y: y,
                viewport_w: w,
                viewport_h: h,
            });
        }
    }

    if let Some(dragged_block) = overlay.dragged_block {
        let drag_center_x = overlay.cursor_px[0] + preview_size * 0.75;
        let drag_center_y = overlay.cursor_px[1] + preview_size * 0.75;
        if let Some((x, y, w, h)) = preview_rect_px_from_center(
            view_width,
            view_height,
            drag_center_x,
            drag_center_y,
            preview_size,
        ) {
            draws.push(ItemPreviewDraw {
                block: dragged_block,
                viewport_x: x,
                viewport_y: y,
                viewport_w: w,
                viewport_h: h,
            });
        }
    }

    draws
}

fn menu_button_visual_state(
    index: usize,
    selected_index: usize,
    pressed_index: Option<usize>,
    enabled: bool,
) -> MenuButtonVisualState {
    if !enabled {
        MenuButtonVisualState::Disabled
    } else if pressed_index.is_some_and(|pressed| pressed == index && pressed == selected_index) {
        MenuButtonVisualState::Pressed
    } else if index == selected_index {
        MenuButtonVisualState::Hovered
    } else {
        MenuButtonVisualState::Normal
    }
}

fn append_menu_button_widget(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    view_width: u32,
    view_height: u32,
    font: &BitmapFont,
    button_atlas: &MenuButtonAtlas,
    label: &str,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    state: MenuButtonVisualState,
    text_scale: f32,
) {
    let (sprite, tint, text_color, text_offset_y) = match state {
        MenuButtonVisualState::Normal => (
            button_atlas.normal,
            [1.0, 1.0, 1.0],
            MENU_BUTTON_TEXT_NORMAL,
            0.0,
        ),
        MenuButtonVisualState::Hovered => (
            button_atlas.highlighted,
            [1.0, 1.0, 1.0],
            MENU_BUTTON_TEXT_HOVERED,
            0.0,
        ),
        MenuButtonVisualState::Pressed => (
            button_atlas.highlighted,
            MENU_BUTTON_PRESSED_TINT,
            MENU_BUTTON_TEXT_HOVERED,
            1.0,
        ),
        MenuButtonVisualState::Disabled => (
            button_atlas.disabled,
            [1.0, 1.0, 1.0],
            MENU_BUTTON_TEXT_DISABLED,
            0.0,
        ),
    };

    append_ui_textured_nine_slice_px(
        vertices,
        indices,
        view_width,
        view_height,
        x,
        y,
        w,
        h,
        sprite,
        button_atlas.nine_slice_border_px,
        tint,
    );

    let glyph_h = font.glyph_h as f32 * text_scale;
    let label_x = x + (w - bitmap_text_width_px(font, label, text_scale)) * 0.5;
    let label_y = y + (h - glyph_h) * 0.5 + text_offset_y;
    append_bitmap_text_with_shadow_color(
        vertices,
        indices,
        view_width,
        view_height,
        font,
        label,
        label_x,
        label_y,
        text_scale,
        text_color,
    );
}

fn build_menu_overlay_mesh(
    view_width: u32,
    view_height: u32,
    dirt_layer: u32,
    font: &BitmapFont,
    button_atlas: &MenuButtonAtlas,
    show_panorama: bool,
    title: &str,
    selected: usize,
    pressed: Option<usize>,
    items: &[&str],
    enabled: Option<&[bool]>,
    footer: Option<&str>,
) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(4096);
    let mut indices = Vec::with_capacity(6144);

    if !show_panorama {
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
    }

    let cx = view_width as f32 * 0.5;
    let cy = view_height as f32 * 0.5;

    let text_scale = WORLDGEN_FONT_SCALE;
    let glyph_h = font.glyph_h as f32 * text_scale;
    let title_x = cx - bitmap_text_width_px(font, title, WORLDGEN_FONT_SCALE) * 0.5;
    let title_y = pause_menu_button_rect_px(view_width, view_height, items.len(), 0)
        .map(|(_, button_y, _, _)| button_y - glyph_h - 18.0)
        .unwrap_or(cy - 64.0);
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
        let is_enabled = enabled
            .and_then(|flags| flags.get(index))
            .copied()
            .unwrap_or(true);
        append_menu_button_widget(
            &mut vertices,
            &mut indices,
            view_width,
            view_height,
            font,
            button_atlas,
            label,
            button_x,
            y,
            button_w,
            button_h,
            menu_button_visual_state(index, selected_index, pressed, is_enabled),
            text_scale,
        );
    }

    if let Some(footer) = footer.filter(|text| !text.trim().is_empty()) {
        let footer_scale = 1.0;
        let footer_x = cx - bitmap_text_width_px(font, footer, footer_scale) * 0.5;
        let footer_y = pause_menu_button_rect_px(
            view_width,
            view_height,
            items.len(),
            items.len().saturating_sub(1),
        )
        .map(|(_, button_y, _, button_h)| button_y + button_h + 18.0)
        .unwrap_or(cy + 64.0);
        append_bitmap_text_with_shadow(
            &mut vertices,
            &mut indices,
            view_width,
            view_height,
            font,
            footer,
            footer_x,
            footer_y,
            footer_scale,
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

fn chat_cursor_boundary(text: &str, cursor: usize) -> usize {
    let cursor = cursor.min(text.len());
    if text.is_char_boundary(cursor) {
        return cursor;
    }
    text.char_indices()
        .map(|(idx, _)| idx)
        .take_while(|idx| *idx < cursor)
        .last()
        .unwrap_or(0)
}

fn chat_line_alpha(chat_open: bool, age_seconds: f32) -> f32 {
    if chat_open {
        return 1.0;
    }
    if age_seconds <= CHAT_OVERLAY_VISIBLE_SECONDS {
        return 1.0;
    }
    let t =
        ((age_seconds - CHAT_OVERLAY_VISIBLE_SECONDS) / CHAT_OVERLAY_FADE_SECONDS).clamp(0.0, 1.0);
    1.0 - t
}

fn append_chat_line_background(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    view_width: u32,
    view_height: u32,
    font: &BitmapFont,
    text: &str,
    baseline_y: f32,
    scale: f32,
    color: [f32; 3],
    alpha: f32,
) {
    let alpha = alpha.clamp(0.0, 1.0);
    if alpha <= 0.01 {
        return;
    }
    let box_w = bitmap_text_width_px(font, text, scale) + CHAT_OVERLAY_BG_PAD_X * 2.0;
    let box_h = font.glyph_h as f32 * scale + CHAT_OVERLAY_BG_PAD_Y * 2.0;
    append_ui_solid_alpha_quad_px(
        vertices,
        indices,
        view_width,
        view_height,
        CHAT_OVERLAY_PAD_X - CHAT_OVERLAY_BG_PAD_X,
        baseline_y - CHAT_OVERLAY_BG_PAD_Y,
        box_w,
        box_h,
        color,
        alpha,
    );
}

fn build_chat_overlay_mesh(
    view_width: u32,
    view_height: u32,
    font: &BitmapFont,
    overlay: ChatOverlay<'_>,
) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(4096);
    let mut indices = Vec::with_capacity(6144);
    let scale = CHAT_OVERLAY_TEXT_SCALE;
    let line_h = font.glyph_h as f32 * scale + CHAT_OVERLAY_LINE_GAP;
    let mut y = view_height as f32 - CHAT_OVERLAY_PAD_BOTTOM - font.glyph_h as f32 * scale;

    if overlay.chat_open {
        let cursor = chat_cursor_boundary(overlay.chat_input, overlay.chat_input_cursor);
        let mut input_line = format!("> {}", overlay.chat_input);
        input_line.insert(2 + cursor, '|');
        append_chat_line_background(
            &mut vertices,
            &mut indices,
            view_width,
            view_height,
            font,
            &input_line,
            y,
            scale,
            CHAT_OVERLAY_INPUT_BG_COLOR,
            CHAT_OVERLAY_INPUT_BG_ALPHA,
        );
        append_bitmap_text_with_shadow_color(
            &mut vertices,
            &mut indices,
            view_width,
            view_height,
            font,
            &input_line,
            CHAT_OVERLAY_PAD_X,
            y,
            scale,
            [1.0, 1.0, 1.0],
        );
        y -= line_h;
    }

    for line in overlay.recent_lines.iter().rev() {
        let alpha = chat_line_alpha(overlay.chat_open, line.age_seconds);
        if alpha <= 0.01 {
            continue;
        }
        append_chat_line_background(
            &mut vertices,
            &mut indices,
            view_width,
            view_height,
            font,
            line.text,
            y,
            scale,
            CHAT_OVERLAY_HISTORY_BG_COLOR,
            CHAT_OVERLAY_HISTORY_BG_ALPHA * alpha,
        );
        let base_color = if line.text.starts_with("Error:") {
            [1.0, 0.45, 0.45]
        } else {
            [1.0, 1.0, 1.0]
        };
        let brightness = 0.35 + 0.65 * alpha;
        let color = [
            (base_color[0] * brightness).clamp(0.0, 1.0),
            (base_color[1] * brightness).clamp(0.0, 1.0),
            (base_color[2] * brightness).clamp(0.0, 1.0),
        ];
        append_bitmap_text_with_shadow_color(
            &mut vertices,
            &mut indices,
            view_width,
            view_height,
            font,
            line.text,
            CHAT_OVERLAY_PAD_X,
            y,
            scale,
            color,
        );
        y -= line_h;
    }

    (vertices, indices)
}

fn chat_overlay_signature(overlay: ChatOverlay<'_>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    overlay.chat_open.hash(&mut hasher);
    overlay.chat_input.hash(&mut hasher);
    overlay.chat_input_cursor.hash(&mut hasher);
    overlay.recent_lines.len().hash(&mut hasher);
    for line in overlay.recent_lines {
        line.text.hash(&mut hasher);
        let fade_bucket = if overlay.chat_open {
            0_u16
        } else {
            (line.age_seconds.max(0.0) * 4.0).floor() as u16
        };
        fade_bucket.hash(&mut hasher);
    }
    hasher.finish()
}

/// ============================
/// Render State (wgpu)
/// ============================
pub(crate) struct State {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    supported_present_modes: Vec<wgpu::PresentMode>,

    // Pipeline stuff
    render_pipeline_back_depth: wgpu::RenderPipeline,
    render_pipeline_no_cull_depth: wgpu::RenderPipeline,
    render_pipeline_back_no_depth: wgpu::RenderPipeline,
    render_pipeline_no_cull_no_depth: wgpu::RenderPipeline,
    render_pipeline_transparent: wgpu::RenderPipeline,
    render_pipeline_transparent_no_depth: wgpu::RenderPipeline,
    render_pipeline_lod: wgpu::RenderPipeline,
    sky_pipeline: wgpu::RenderPipeline,
    highlight_pipeline: wgpu::RenderPipeline,
    outline_pipeline: wgpu::RenderPipeline,
    ui_pipeline: wgpu::RenderPipeline,
    ui_3d_overlay_pipeline: wgpu::RenderPipeline,
    backface_culling_enabled: bool,
    debug_depth_test: bool,
    pub(crate) world: World,
    zero_light_chunk_data: Arc<Vec<u8>>,
    chunk_meshes: HashMap<ChunkCoord, LayeredChunkMesh>,
    full_solid_mesh_skip_set: HashSet<ChunkCoord>,
    lod_meshes: HashMap<LodColumnCoord, ChunkMesh>,
    dirty_chunk_queue: VecDeque<ChunkCoord>,
    dirty_chunk_set: HashSet<ChunkCoord>,
    mesh_schedule: HashMap<ChunkCoord, ChunkMeshSchedule>,
    deferred_mesh_until_frame: HashMap<ChunkCoord, u64>,
    pending_block_edits: VecDeque<PendingBlockEdit>,
    pending_block_edit_dirty_chunks: HashSet<ChunkCoord>,
    pending_block_edit_dirty_columns: HashSet<(i32, i32)>,
    deferred_block_edits: HashMap<ChunkCoord, Vec<DeferredBlockEdit>>,
    deferred_block_edits_total: usize,
    pending_sand_gravity_checks: VecDeque<PendingSandGravityCheck>,
    pending_sand_gravity_check_set: HashSet<IVec3>,
    highlight_mesh: Option<ChunkMesh>,
    outline_target: Option<OutlineTarget>,
    outline_vertex_buffer: wgpu::Buffer,
    crosshair_mesh: ChunkMesh,
    preview_mesh_cache: HashMap<BlockId, ChunkMesh>,
    hotbar_overlay_mesh: Option<ChunkMesh>,
    hotbar_overlay_cache: Option<HotbarOverlayCacheKey>,
    chat_overlay_mesh: Option<ChunkMesh>,
    chat_overlay_cache: Option<ChatOverlayCacheKey>,
    inventory_overlay_mesh: Option<ChunkMesh>,
    inventory_overlay_cache: Option<InventoryOverlayCacheKey>,
    block_select_overlay_mesh: Option<ChunkMesh>,
    block_select_overlay_cache: Option<BlockSelectOverlayCacheKey>,
    _texture_registry: TextureRegistry,
    _world_texture_array: TextureArray,
    _sky_texture_array: TextureArray,
    _worldgen_ui_texture_array: TextureArray,
    water_overlay_mesh: ChunkMesh,
    boundary_ocean_mesh: Option<ChunkMesh>,
    sky_object_vertex_buffer: wgpu::Buffer,
    sky_object_index_buffer: wgpu::Buffer,
    menu_panorama_vertex_buffer: wgpu::Buffer,
    menu_panorama_index_buffer: wgpu::Buffer,
    world_texture_bind_group: wgpu::BindGroup,
    sky_texture_bind_group: wgpu::BindGroup,
    worldgen_ui_texture_bind_group: wgpu::BindGroup,
    _sky_anim_buffer: wgpu::Buffer,
    _worldgen_ui_anim_buffer: wgpu::Buffer,
    worldgen_dirt_layer: u32,
    sky_sun_layer: u32,
    sky_moon_layer: u32,
    sky_star_layer: u32,
    menu_panorama_layers: [u32; MENU_PANORAMA_FACE_COUNT],
    sky_sun_uv_rect: [f32; 4],
    menu_panorama_uv_rects: [[f32; 4]; MENU_PANORAMA_FACE_COUNT],
    moon_phase_atlas_size: [u32; 2],
    sky_stars: Vec<SkyStar>,
    worldgen_font: BitmapFont,
    hud_atlas: HudAtlas,
    inventory_atlas: InventoryAtlas,
    menu_button_atlas: MenuButtonAtlas,
    first_person_skin_layer: u32,
    first_person_arm_tweak: FirstPersonArmTweak,
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
    chunk_origin: HashMap<ChunkCoord, ChunkOrigin>,
    mesh_upload_queue: VecDeque<QueuedMeshUpload>,
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
    sim_system_last_frame_ms: Vec<f32>,
    sim_system_last_frame_ticks: Vec<u32>,
    fluid_debug_counts: FluidDebugCounts,
    worldgen_feature_debug_counts: WorldgenFeatureDebugCounts,
    chunk_lighting_progress: HashMap<ChunkCoord, ChunkLightingProgress>,
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
    uploaded_final_lighting_chunks: HashSet<ChunkCoord>,
    last_stream_center: Option<(i32, i32, i32)>,
    last_stream_radius: i32,
    radius_offsets_cache: Vec<(i32, i32)>,
    lod_ring_offsets_cache: Vec<(i32, i32)>,
    lod_request_cursor: usize,
    chunkgen_pending_set: HashSet<ChunkCoord>,
    chunkgen_pending_enqueued_frame: HashMap<ChunkCoord, u64>,
    chunkgen_pending_priority: HashMap<ChunkCoord, (u64, StreamPriorityKey)>,
    chunkgen_pending_round_robin: VecDeque<ChunkCoord>,
    chunkgen_pending_heap: BinaryHeap<ChunkPriorityHeapEntry>,
    chunkgen_pending_scan_cursor: usize,
    chunkgen_pending_refresh_cursor: usize,
    chunkgen_heap_valid_for_center: Option<(i32, i32, i32)>,
    chunkgen_heap_valid_for_radius: i32,
    chunkgen_heap_last_rebuild_frame: u64,
    chunkgen_priority_epoch: u64,
    chunkgen_priority_context_stamp: Option<ChunkgenPriorityContextStamp>,
    chunkgen_stream_epoch: u64,
    last_lod_stream_center: Option<(i32, i32)>,
    far_lod_enabled: bool,
    far_lod_requests_per_frame: usize,
    far_lod_gpu_uploads_per_frame: usize,
    pending_lod_mesh_uploads: VecDeque<(LodColumnCoord, CpuMeshData)>,
    stream_last_camera_pos: Option<Vec3>,
    stream_camera_speed_blocks_per_sec: f32,
    stream_last_camera_y: Option<f32>,
    stream_recent_descend_frames: u32,
    stream_recent_mining_frames: u32,
    stream_horizon_sector_max_elev: [f32; STREAM_HORIZON_SECTOR_COUNT],
    stream_horizon_has_data: bool,
    stream_horizon_built_frame: u64,
    stream_horizon_cache_key: Option<StreamHorizonCacheKey>,
    stream_horizon_mesh_count: usize,
    stream_horizon_seen_columns: HashSet<(i32, i32)>,
    stream_surface_height_cache: HashMap<SurfaceHeightCacheKey, i32>,
    stream_column_plan_cache: HashMap<ColumnStreamPlanCacheKey, ColumnStreamPlan>,
    stream_scheduler_cached_frame: u64,
    stream_scheduler_cached_frustum: Option<Frustum>,
    stream_scheduler_cached_forward_xz: Option<Vec3>,
    stream_scheduler_cached_forward_bucket: u8,
    mesh_debug_epoch: u32,
    lod_cached_near_radius: i32,
    lod_cached_far_radius: i32,
    frame_perf: StateFramePerf,
    debug_perf_summary_enabled: bool,
    perf_summary_window: PerfSummaryWindow,
    last_lighting_step_ms: f32,
    lighting_job_inflight: bool,
    lighting_generation: u64,
    lighting_request_pending: bool,
    lighting_rebuild_not_before: Option<Instant>,
    pending_chunkgen_apply_followups: VecDeque<ChunkGenApplyFollowUpTask>,
    pending_removed_lighting_batches: VecDeque<RemovedLightingBatch>,
    pending_chunk_added_lighting_jobs: VecDeque<ChunkAddedLightingJob>,
    pending_chunk_added_lighting_coords: HashSet<ChunkCoord>,
    startup_deferred_chunk_added_lighting_coords: HashSet<ChunkCoord>,
    next_chunk_added_lighting_batch_id: u64,
    pending_lighting_dirty_chunks: VecDeque<ChunkCoord>,
    pending_lighting_dirty_set: HashSet<ChunkCoord>,
    debug_disable_ao: bool,
    debug_lighting_view: LightingDebugView,
    menu_panorama_started_at: Instant,
    pub(crate) world_time: WorldTime,
    save_root: PathBuf,
    storage_reader: RegionStorage,
    storage_loader: StorageLoadWorker,
    storage_writer: StorageWorker,
    saved_revision: HashMap<ChunkCoord, u64>,
    saved_column_missing_cache: HashMap<(i32, i32), Instant>,
    saved_column_loads_inflight: HashSet<(i32, i32)>,
    saved_loads_paused: bool,
    startup_saved_load_bypass_logged: bool,
    // Saved columns are decoded off-thread, then applied through this staged queue so one column
    // cannot monopolize the main thread with deferred edits, lighting hooks, and dirty fan-out.
    saved_load_apply_queue: VecDeque<SavedLoadApplyTask>,
    saved_load_apply_active: Option<SavedLoadApplyTask>,
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
    fn default_save_root() -> PathBuf {
        PathBuf::from(SAVE_ROOT_DIR)
    }

    fn active_world_pointer_path() -> PathBuf {
        Self::default_save_root().join("current_world.txt")
    }

    fn resolve_startup_save_root() -> PathBuf {
        let default_root = Self::default_save_root();
        let pointer_path = Self::active_world_pointer_path();
        let Ok(raw) = fs::read_to_string(&pointer_path) else {
            return default_root;
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed == "." {
            default_root
        } else {
            let relative = Path::new(trimmed);
            if relative.is_absolute() {
                relative.to_path_buf()
            } else {
                default_root.join(relative)
            }
        }
    }

    fn persist_active_save_root(save_root: &Path) -> io::Result<()> {
        let pointer_path = Self::active_world_pointer_path();
        if let Some(parent) = pointer_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let root_value = save_root
            .strip_prefix(Self::default_save_root())
            .ok()
            .filter(|relative| !relative.as_os_str().is_empty())
            .map(|relative| relative.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".".to_string());
        let tmp_path = pointer_path.with_extension("tmp");
        fs::write(&tmp_path, format!("{root_value}\n"))?;
        fs::rename(&tmp_path, &pointer_path)?;
        Ok(())
    }

    fn allocate_new_world_save_root(seed: u64) -> PathBuf {
        let worlds_root = Self::default_save_root().join("worlds");
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        for attempt in 0..1024u32 {
            let suffix = if attempt == 0 {
                String::new()
            } else {
                format!("-{attempt}")
            };
            let candidate = worlds_root.join(format!("world-{now_ms:013}-{seed:016x}{suffix}"));
            if !candidate.exists() {
                return candidate;
            }
        }
        worlds_root.join(format!("world-{now_ms:013}-{seed:016x}-fallback"))
    }

    fn world_meta_path_for(save_root: &Path) -> PathBuf {
        save_root.join(WORLD_META_FILE_NAME)
    }

    fn world_manifest_path_for(save_root: &Path) -> PathBuf {
        save_root.join(WORLD_MANIFEST_FILE_NAME)
    }

    fn ticking_areas_path_for(save_root: &Path) -> PathBuf {
        save_root.join(TICKING_AREAS_FILE_NAME)
    }

    fn world_meta_path(&self) -> PathBuf {
        Self::world_meta_path_for(&self.save_root)
    }

    fn world_manifest_path(&self) -> PathBuf {
        Self::world_manifest_path_for(&self.save_root)
    }

    fn ticking_areas_path(&self) -> PathBuf {
        Self::ticking_areas_path_for(&self.save_root)
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

    fn load_ticking_areas_into_world(save_root: &Path, world: &mut World) {
        let path = Self::ticking_areas_path_for(save_root);
        match ticking_areas::load(&path) {
            Ok(Some(areas)) => {
                let count = areas.len();
                world.replace_ticking_areas(areas);
                crate::console_info!("ticking areas: loaded {} from {}", count, path.display());
            }
            Ok(None) => {}
            Err(err) => {
                crate::console_error!("ticking areas: failed to load {}: {}", path.display(), err);
            }
        }
    }

    fn save_ticking_areas(&self) -> io::Result<()> {
        ticking_areas::save_atomic(&self.ticking_areas_path(), self.world.ticking_areas())
    }

    fn create_storage_services(
        save_root: &Path,
    ) -> io::Result<(RegionStorage, StorageLoadWorker, StorageWorker)> {
        let root = save_root.to_path_buf();
        let storage_reader = RegionStorage::new(root.clone(), 64);
        let storage_loader = StorageLoadWorker::start(root.clone())?;
        let storage_writer = StorageWorker::start(root)?;
        Ok((storage_reader, storage_loader, storage_writer))
    }

    fn switch_save_root(&mut self, save_root: PathBuf) {
        let (storage_reader, storage_loader, storage_writer) =
            Self::create_storage_services(&save_root).unwrap_or_else(|err| {
                panic!(
                    "failed to start storage services for {}: {}",
                    save_root.display(),
                    err
                )
            });
        self.save_root = save_root.clone();
        self.storage_reader = storage_reader;
        self.storage_loader = storage_loader;
        self.storage_writer = storage_writer;
        if let Err(err) = Self::persist_active_save_root(&save_root) {
            crate::console_error!(
                "[storage] failed to persist active save root {}: {}",
                save_root.display(),
                err
            );
        }
    }

    fn saved_loads_suspended(&self) -> bool {
        self.saved_loads_paused || self.world_generation_task.is_some()
    }

    fn clear_saved_load_apply_backlog(&mut self) -> usize {
        let mut cleared = self.saved_load_apply_queue.len();
        self.saved_load_apply_queue.clear();
        if self.saved_load_apply_active.take().is_some() {
            cleared += 1;
        }
        cleared
    }

    fn discard_ready_saved_load_results(&mut self) -> usize {
        let mut discarded = 0usize;
        while let Some(result) = self.storage_loader.try_recv_result() {
            discarded += 1;
            self.saved_column_loads_inflight
                .remove(&(result.cx, result.cz));
        }
        discarded
    }

    fn drain_saved_load_pipeline_for_startup_transition(&mut self) {
        let started = Instant::now();
        let mut discarded_results = self.discard_ready_saved_load_results();
        let mut cleared_apply = self.clear_saved_load_apply_backlog();

        while self.storage_loader.queue_len_approx() > 0
            || self.storage_loader.result_queue_len_approx() > 0
            || !self.saved_column_loads_inflight.is_empty()
        {
            discarded_results += self.discard_ready_saved_load_results();
            cleared_apply += self.clear_saved_load_apply_backlog();
            std::thread::yield_now();
        }

        discarded_results += self.discard_ready_saved_load_results();
        cleared_apply += self.clear_saved_load_apply_backlog();

        if discarded_results > 0 || cleared_apply > 0 {
            crate::console_info!(
                "[storage][saved_load] startup transition drained results={} apply_tasks={} in {:.2}ms",
                discarded_results,
                cleared_apply,
                started.elapsed().as_secs_f64() * 1000.0
            );
        }
    }

    fn log_startup_saved_load_bypass_once(&mut self, source: &str) {
        if self.world_generation_task.is_some() && !self.startup_saved_load_bypass_logged {
            self.startup_saved_load_bypass_logged = true;
            crate::console_info!(
                "[storage][saved_load] startup bypass active via {} (save_root={})",
                source,
                self.save_root.display()
            );
        }
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
        let layer = self
            .block_texture_lookup
            .layer_for(BlockId::WaterStill, Face::Up);
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
                crate::console_error!("[storage][save] failed to queue column ({cx},{cz}): {err}");
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
                    crate::console_error!(
                        "[storage][save] async save failed for column ({},{}): {}",
                        completion.cx,
                        completion.cz,
                        err
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
            crate::console_error!(
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
        if self.saved_loads_suspended() {
            return false;
        }
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
            if self.chunk_is_generation_satisfied(coord) || self.chunkgen_inflight.contains(&coord)
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
            crate::console_error!(
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
                crate::console_error!("[storage][load] failed to queue column ({cx},{cz}): {err}");
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
        if self.saved_loads_suspended() {
            return 0;
        }
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

    fn saved_load_apply_backlog_len(&self) -> usize {
        self.saved_load_apply_queue.len() + usize::from(self.saved_load_apply_active.is_some())
    }

    fn saved_load_apply_should_yield(
        outer_started: Instant,
        phase_started: Instant,
        unbounded: bool,
    ) -> bool {
        if unbounded {
            return false;
        }
        outer_started.elapsed().as_secs_f32() * 1000.0 >= SAVED_LOAD_APPLY_BUDGET_MS
            || phase_started.elapsed().as_secs_f32() * 1000.0 >= SAVED_LOAD_APPLY_PHASE_BUDGET_MS
    }

    fn record_saved_load_phase_step_ms(&mut self, phase: SavedLoadApplyPhase, step_ms: f32) {
        match phase {
            SavedLoadApplyPhase::PrepareSections => {
                self.frame_perf.saved_load_apply_prepare_step_max_ms = self
                    .frame_perf
                    .saved_load_apply_prepare_step_max_ms
                    .max(step_ms);
            }
            SavedLoadApplyPhase::ApplyColumn => {
                self.frame_perf.saved_load_apply_commit_step_max_ms = self
                    .frame_perf
                    .saved_load_apply_commit_step_max_ms
                    .max(step_ms);
            }
            SavedLoadApplyPhase::ReplayDeferredEdits => {
                self.frame_perf.saved_load_apply_deferred_step_max_ms = self
                    .frame_perf
                    .saved_load_apply_deferred_step_max_ms
                    .max(step_ms);
            }
            SavedLoadApplyPhase::FinalizeSections => {
                self.frame_perf.saved_load_apply_finalize_step_max_ms = self
                    .frame_perf
                    .saved_load_apply_finalize_step_max_ms
                    .max(step_ms);
            }
            SavedLoadApplyPhase::LightingRemoved | SavedLoadApplyPhase::LightingAdded => {
                self.frame_perf.saved_load_apply_lighting_step_max_ms = self
                    .frame_perf
                    .saved_load_apply_lighting_step_max_ms
                    .max(step_ms);
            }
            SavedLoadApplyPhase::EnqueueSecondaryWork | SavedLoadApplyPhase::MarkBoundaryDirty => {
                self.frame_perf.saved_load_apply_secondary_step_max_ms = self
                    .frame_perf
                    .saved_load_apply_secondary_step_max_ms
                    .max(step_ms);
            }
            SavedLoadApplyPhase::Finish => {}
        }
    }

    fn enqueue_removed_lighting_batches(&mut self, coords: &[ChunkCoord], reason: &'static str) {
        if coords.is_empty() {
            return;
        }
        for chunk in coords.chunks(SAVED_LOAD_APPLY_LIGHTING_BATCH_PER_STEP) {
            self.pending_removed_lighting_batches
                .push_back(RemovedLightingBatch {
                    coords: chunk.to_vec(),
                    reason,
                    enqueued_at: Instant::now(),
                });
        }
    }

    fn make_saved_load_apply_task(
        &self,
        cx: i32,
        cz: i32,
        column: ColumnBlob,
    ) -> SavedLoadApplyTask {
        SavedLoadApplyTask::new(cx, cz, column)
    }

    fn take_deferred_edits_batch_for_chunk(
        &mut self,
        coord: ChunkCoord,
        max_edits: usize,
    ) -> Vec<DeferredBlockEdit> {
        let Some(edits) = self.deferred_block_edits.get_mut(&coord) else {
            return Vec::new();
        };
        edits.sort_by_key(Self::deferred_edit_order_key);
        let take = max_edits.min(edits.len());
        if take == 0 {
            return Vec::new();
        }
        let remaining = edits.split_off(take);
        let batch = std::mem::replace(edits, remaining);
        self.deferred_block_edits_total =
            self.deferred_block_edits_total.saturating_sub(batch.len());
        if self
            .deferred_block_edits
            .get(&coord)
            .is_some_and(|remaining| remaining.is_empty())
        {
            self.deferred_block_edits.remove(&coord);
        }
        self.frame_perf.deferred_edits_pending_total = self.deferred_block_edits_total;
        batch
    }

    fn restore_deferred_edits_batch_for_chunk(
        &mut self,
        coord: ChunkCoord,
        mut edits: Vec<DeferredBlockEdit>,
    ) {
        if edits.is_empty() {
            return;
        }
        self.deferred_block_edits_total += edits.len();
        self.deferred_block_edits
            .entry(coord)
            .or_default()
            .append(&mut edits);
        self.frame_perf.deferred_edits_pending_total = self.deferred_block_edits_total;
    }

    fn apply_deferred_edits_batch_to_chunk(
        coord: ChunkCoord,
        chunk: &mut Chunk,
        edits: Vec<DeferredBlockEdit>,
    ) -> ApplyEditsStats {
        let mut stats = ApplyEditsStats::default();
        for edit in edits {
            stats.total += 1;
            if !World::in_bounds(edit.world_pos) {
                stats.skipped_out_of_bounds += 1;
                continue;
            }
            if World::world_to_chunk_coord(edit.world_pos) != coord {
                stats.skipped_conflict += 1;
                continue;
            }
            let local = World::world_to_local(edit.world_pos);
            if !Chunk::in_bounds(local.x, local.y, local.z) {
                stats.skipped_out_of_bounds += 1;
                continue;
            }

            let (block, data) = Self::canonicalize_deferred_edit(edit);
            let prev_block = chunk.get(local.x, local.y, local.z);
            let prev_data = chunk.get_data(local.x, local.y, local.z);
            if prev_block == block && prev_data == data {
                stats.skipped_unchanged += 1;
                continue;
            }

            chunk.set(local.x, local.y, local.z, block);
            chunk.set_data(local.x, local.y, local.z, data);
            stats.applied += 1;
            if fluid_kind(block).is_some() {
                stats.applied_fluid += 1;
            }
        }
        stats
    }

    fn apply_deferred_edits_batch_to_loaded_world_chunk(
        &mut self,
        coord: ChunkCoord,
        max_edits: usize,
    ) -> ApplyEditsStats {
        if !self.has_deferred_edits(coord) {
            return ApplyEditsStats::default();
        }
        let started = Instant::now();
        let edits = self.take_deferred_edits_batch_for_chunk(coord, max_edits);
        if edits.is_empty() {
            return ApplyEditsStats::default();
        }

        let Some(mut chunk) = self.world.chunks.remove(&coord) else {
            self.restore_deferred_edits_batch_for_chunk(coord, edits);
            return ApplyEditsStats::default();
        };
        let stats = Self::apply_deferred_edits_batch_to_chunk(coord, &mut chunk, edits);
        let edits_remaining = self.has_deferred_edits(coord);
        self.full_solid_mesh_skip_set.remove(&coord);
        if !edits_remaining && chunk.is_empty() {
            self.generated_empty_chunks.insert(coord);
            self.chunk_origin.remove(&coord);
        } else {
            self.generated_empty_chunks.remove(&coord);
            self.world.chunks.insert(coord, chunk);
            self.mark_chunk_user_only_if_unknown(coord);
        }

        self.frame_perf.deferred_edits_applied += stats.applied;
        self.frame_perf.deferred_edits_chunks_touched += 1;
        self.frame_perf.deferred_edits_pending_total = self.deferred_block_edits_total;
        let apply_ms = started.elapsed().as_secs_f32() * 1000.0;
        perf_trace::record_duration_ms(
            "deferred_edits.apply_batch_to_chunk",
            apply_ms,
            &format!(
                "coord=({}, {}, {}) total={} applied={} applied_fluid={} skip_oob={} skip_conflict={} skip_unchanged={} remaining={} pending_total={}",
                coord.x,
                coord.y,
                coord.z,
                stats.total,
                stats.applied,
                stats.applied_fluid,
                stats.skipped_out_of_bounds,
                stats.skipped_conflict,
                stats.skipped_unchanged,
                usize::from(edits_remaining),
                self.deferred_block_edits_total
            ),
        );
        stats
    }

    fn advance_saved_load_apply_task_step(
        &mut self,
        task: &mut SavedLoadApplyTask,
        outer_started: Instant,
        unbounded: bool,
    ) -> SavedLoadApplyTaskOutcome {
        match task.phase {
            SavedLoadApplyPhase::PrepareSections => {
                let phase_started = Instant::now();
                let mut prepared = 0usize;
                while task.prepare_cursor <= task.max_section_y
                    && prepared < SAVED_LOAD_APPLY_SECTION_BATCH_PER_STEP
                {
                    let coord = ChunkCoord {
                        x: task.cx,
                        y: task.prepare_cursor,
                        z: task.cz,
                    };
                    if self.world.get_chunk(coord).is_some() {
                        task.previously_loaded_sections.push(coord);
                    }
                    self.salvage_untrusted_chunk_for_generation(coord);
                    self.chunk_origin.remove(&coord);
                    task.prepare_cursor += 1;
                    prepared += 1;
                    if Self::saved_load_apply_should_yield(outer_started, phase_started, unbounded)
                    {
                        break;
                    }
                }
                let step_ms = phase_started.elapsed().as_secs_f32() * 1000.0;
                task.stats.prepare_ms += step_ms;
                task.stats.sections_prepared += prepared;
                self.frame_perf.saved_load_sections_prepared += prepared;
                self.frame_perf.saved_load_apply_prepare_ms += step_ms;
                self.record_saved_load_phase_step_ms(task.phase, step_ms);
                perf_trace::record_duration_ms(
                    "saved_load.apply_prepare_sections",
                    step_ms,
                    &format!(
                        "cx={} cz={} sections={} cursor={}..={} phase={} inner_budget_ms={:.2}",
                        task.cx,
                        task.cz,
                        prepared,
                        task.prepare_cursor,
                        task.max_section_y,
                        task.phase_label(),
                        SAVED_LOAD_APPLY_PHASE_BUDGET_MS
                    ),
                );
                if task.prepare_cursor > task.max_section_y {
                    task.phase = SavedLoadApplyPhase::ApplyColumn;
                }
                SavedLoadApplyTaskOutcome::InProgress
            }
            SavedLoadApplyPhase::ApplyColumn => {
                let phase_started = Instant::now();
                let Some(blob) = task.blob.as_ref() else {
                    return SavedLoadApplyTaskOutcome::Failed(format!(
                        "saved-load task missing blob for column ({},{})",
                        task.cx, task.cz
                    ));
                };
                if !task.commit_validated {
                    if let Err(err) = validate_column_for_world(&self.world, blob) {
                        return SavedLoadApplyTaskOutcome::Failed(format!(
                            "saved-load apply failed for column ({},{}): {}",
                            task.cx, task.cz, err
                        ));
                    }
                    task.commit_validated = true;
                }

                let mut removed = 0usize;
                while task.commit_remove_cursor <= blob.max_section_y
                    && removed < SAVED_LOAD_APPLY_SECTION_BATCH_PER_STEP
                {
                    self.world.chunks.remove(&ChunkCoord {
                        x: task.cx,
                        y: task.commit_remove_cursor,
                        z: task.cz,
                    });
                    task.commit_remove_cursor += 1;
                    removed += 1;
                    if Self::saved_load_apply_should_yield(outer_started, phase_started, unbounded)
                    {
                        break;
                    }
                }

                let mut applied = 0usize;
                while task.commit_remove_cursor > blob.max_section_y
                    && task.commit_section_cursor < blob.sections.len()
                    && applied < SAVED_LOAD_APPLY_COMMIT_SECTION_BATCH_PER_STEP
                {
                    let section = &blob.sections[task.commit_section_cursor];
                    let chunk = match decode_chunk_from_section_blob(&self.world, blob, section) {
                        Ok(chunk) => chunk,
                        Err(err) => {
                            return SavedLoadApplyTaskOutcome::Failed(format!(
                                "saved-load apply failed for column ({},{}): {}",
                                task.cx, task.cz, err
                            ));
                        }
                    };
                    if let Some(chunk) = chunk {
                        self.world.chunks.insert(
                            ChunkCoord {
                                x: task.cx,
                                y: section.cy,
                                z: task.cz,
                            },
                            chunk,
                        );
                    }
                    task.commit_section_cursor += 1;
                    applied += 1;
                    if Self::saved_load_apply_should_yield(outer_started, phase_started, unbounded)
                    {
                        break;
                    }
                }

                if task.commit_remove_cursor > blob.max_section_y
                    && task.commit_section_cursor >= blob.sections.len()
                {
                    self.saved_column_missing_cache.remove(&(task.cx, task.cz));
                    task.phase = SavedLoadApplyPhase::ReplayDeferredEdits;
                }

                let step_ms = phase_started.elapsed().as_secs_f32() * 1000.0;
                task.stats.commit_ms += step_ms;
                self.frame_perf.saved_load_apply_commit_ms += step_ms;
                self.record_saved_load_phase_step_ms(task.phase, step_ms);
                perf_trace::record_duration_ms(
                    "saved_load.apply_commit_column",
                    step_ms,
                    &format!(
                        "cx={} cz={} removed={} remove_cursor={} applied={} section_cursor={}/{} inner_budget_ms={:.2}",
                        task.cx,
                        task.cz,
                        removed,
                        task.commit_remove_cursor,
                        applied,
                        task.commit_section_cursor,
                        blob.sections.len(),
                        SAVED_LOAD_APPLY_PHASE_BUDGET_MS
                    ),
                );
                SavedLoadApplyTaskOutcome::InProgress
            }
            SavedLoadApplyPhase::ReplayDeferredEdits => {
                if task.deferred_cursor >= task.present_sections.len() {
                    task.phase = SavedLoadApplyPhase::FinalizeSections;
                    return SavedLoadApplyTaskOutcome::InProgress;
                }
                let phase_started = Instant::now();
                let mut batches = 0usize;
                let mut chunks_touched = 0usize;
                while task.deferred_cursor < task.present_sections.len()
                    && batches < SAVED_LOAD_APPLY_DEFERRED_BATCHES_PER_STEP
                {
                    let coord = task.present_sections[task.deferred_cursor];
                    let stats = self.apply_deferred_edits_batch_to_loaded_world_chunk(
                        coord,
                        SAVED_LOAD_APPLY_DEFERRED_EDITS_PER_BATCH,
                    );
                    if stats.total > 0 {
                        chunks_touched += 1;
                        task.stats.deferred_batches += 1;
                        task.stats.deferred_chunks_touched += 1;
                        task.stats.deferred_total += stats.total;
                        task.stats.deferred_applied += stats.applied;
                        task.stats.deferred_applied_fluid += stats.applied_fluid;
                        self.frame_perf.saved_load_deferred_batches += 1;
                        self.frame_perf.saved_load_deferred_edits_applied += stats.applied;
                        self.frame_perf.saved_load_chunks_touched += 1;
                    }
                    if !self.has_deferred_edits(coord) {
                        task.deferred_cursor += 1;
                    }
                    batches += 1;
                    if Self::saved_load_apply_should_yield(outer_started, phase_started, unbounded)
                    {
                        break;
                    }
                }
                let step_ms = phase_started.elapsed().as_secs_f32() * 1000.0;
                task.stats.deferred_ms += step_ms;
                self.frame_perf.saved_load_apply_deferred_ms += step_ms;
                self.record_saved_load_phase_step_ms(task.phase, step_ms);
                perf_trace::record_duration_ms(
                    "saved_load.apply_deferred_edits",
                    step_ms,
                    &format!(
                        "cx={} cz={} batches={} chunks={} cursor={}/{} total={} applied={} applied_fluid={} inner_budget_ms={:.2}",
                        task.cx,
                        task.cz,
                        batches,
                        chunks_touched,
                        task.deferred_cursor,
                        task.present_sections.len(),
                        task.stats.deferred_total,
                        task.stats.deferred_applied,
                        task.stats.deferred_applied_fluid,
                        SAVED_LOAD_APPLY_PHASE_BUDGET_MS
                    ),
                );
                if task.deferred_cursor >= task.present_sections.len() {
                    task.phase = SavedLoadApplyPhase::FinalizeSections;
                }
                SavedLoadApplyTaskOutcome::InProgress
            }
            SavedLoadApplyPhase::FinalizeSections => {
                let phase_started = Instant::now();
                let mut finalized = 0usize;
                while task.finalize_cursor <= task.max_section_y
                    && finalized < SAVED_LOAD_APPLY_SECTION_BATCH_PER_STEP
                {
                    let coord = ChunkCoord {
                        x: task.cx,
                        y: task.finalize_cursor,
                        z: task.cz,
                    };
                    self.full_solid_mesh_skip_set.remove(&coord);
                    self.sync_generated_empty_chunk_marker(coord);
                    let idx = (task.finalize_cursor - task.min_section_y) as usize;
                    let expected = task
                        .section_presence_mask
                        .get(idx)
                        .copied()
                        .unwrap_or(false);
                    if expected && self.world.get_chunk(coord).is_some() {
                        self.chunk_origin.insert(coord, ChunkOrigin::Loaded);
                        task.finalized_present_sections.push(coord);
                        task.loaded_any_chunk = true;
                    } else {
                        self.chunk_origin.remove(&coord);
                    }
                    task.finalize_cursor += 1;
                    finalized += 1;
                    if Self::saved_load_apply_should_yield(outer_started, phase_started, unbounded)
                    {
                        break;
                    }
                }
                let step_ms = phase_started.elapsed().as_secs_f32() * 1000.0;
                task.stats.finalize_ms += step_ms;
                task.stats.sections_finalized += finalized;
                self.frame_perf.saved_load_sections_finalized += finalized;
                self.frame_perf.saved_load_apply_finalize_ms += step_ms;
                self.record_saved_load_phase_step_ms(task.phase, step_ms);
                perf_trace::record_duration_ms(
                    "saved_load.apply_finalize_sections",
                    step_ms,
                    &format!(
                        "cx={} cz={} sections={} cursor={}..={} present={} inner_budget_ms={:.2}",
                        task.cx,
                        task.cz,
                        finalized,
                        task.finalize_cursor,
                        task.max_section_y,
                        task.finalized_present_sections.len(),
                        SAVED_LOAD_APPLY_PHASE_BUDGET_MS
                    ),
                );
                if task.finalize_cursor > task.max_section_y {
                    task.removed_sections = task
                        .previously_loaded_sections
                        .iter()
                        .copied()
                        .filter(|coord| self.world.get_chunk(*coord).is_none())
                        .collect();
                    task.phase = SavedLoadApplyPhase::LightingRemoved;
                }
                SavedLoadApplyTaskOutcome::InProgress
            }
            SavedLoadApplyPhase::LightingRemoved => {
                if task.removed_lighting_cursor >= task.removed_sections.len() {
                    task.phase = SavedLoadApplyPhase::LightingAdded;
                    return SavedLoadApplyTaskOutcome::InProgress;
                }
                let phase_started = Instant::now();
                let end = (task.removed_lighting_cursor + SAVED_LOAD_APPLY_LIGHTING_BATCH_PER_STEP)
                    .min(task.removed_sections.len());
                let batch = task.removed_sections[task.removed_lighting_cursor..end].to_vec();
                self.enqueue_removed_lighting_batches(&batch, "saved_load_remove");
                let step_ms = phase_started.elapsed().as_secs_f32() * 1000.0;
                task.stats.lighting_ms += step_ms;
                task.stats.removed_lighting_batches += 1;
                self.frame_perf.saved_load_apply_lighting_ms += step_ms;
                self.record_saved_load_phase_step_ms(task.phase, step_ms);
                perf_trace::record_duration_ms(
                    "saved_load.apply_lighting_removed",
                    step_ms,
                    &format!(
                        "cx={} cz={} batch={} cursor={}..{} lighting_backlog={} inner_budget_ms={:.2}",
                        task.cx,
                        task.cz,
                        batch.len(),
                        task.removed_lighting_cursor,
                        end,
                        self.pending_removed_lighting_batches.len(),
                        SAVED_LOAD_APPLY_PHASE_BUDGET_MS
                    ),
                );
                task.removed_lighting_cursor = end;
                if task.removed_lighting_cursor >= task.removed_sections.len() {
                    task.phase = SavedLoadApplyPhase::LightingAdded;
                }
                SavedLoadApplyTaskOutcome::InProgress
            }
            SavedLoadApplyPhase::LightingAdded => {
                if task.added_lighting_cursor >= task.finalized_present_sections.len() {
                    task.phase = SavedLoadApplyPhase::EnqueueSecondaryWork;
                    return SavedLoadApplyTaskOutcome::InProgress;
                }
                let phase_started = Instant::now();
                let end = (task.added_lighting_cursor + SAVED_LOAD_APPLY_LIGHTING_BATCH_PER_STEP)
                    .min(task.finalized_present_sections.len());
                let batch =
                    task.finalized_present_sections[task.added_lighting_cursor..end].to_vec();
                self.enqueue_chunk_stream_added_lighting(&batch, "saved_load_apply");
                let step_ms = phase_started.elapsed().as_secs_f32() * 1000.0;
                task.stats.lighting_ms += step_ms;
                task.stats.added_lighting_batches += 1;
                self.frame_perf.saved_load_apply_lighting_ms += step_ms;
                self.record_saved_load_phase_step_ms(task.phase, step_ms);
                perf_trace::record_duration_ms(
                    "saved_load.apply_lighting_added",
                    step_ms,
                    &format!(
                        "cx={} cz={} batch={} cursor={}..{} lighting_backlog={} inner_budget_ms={:.2}",
                        task.cx,
                        task.cz,
                        batch.len(),
                        task.added_lighting_cursor,
                        end,
                        self.pending_chunk_added_lighting_jobs.len(),
                        SAVED_LOAD_APPLY_PHASE_BUDGET_MS
                    ),
                );
                task.added_lighting_cursor = end;
                if task.added_lighting_cursor >= task.finalized_present_sections.len() {
                    task.phase = SavedLoadApplyPhase::EnqueueSecondaryWork;
                }
                SavedLoadApplyTaskOutcome::InProgress
            }
            SavedLoadApplyPhase::EnqueueSecondaryWork => {
                if task.secondary_cursor >= task.finalized_present_sections.len() {
                    task.phase = SavedLoadApplyPhase::MarkBoundaryDirty;
                    return SavedLoadApplyTaskOutcome::InProgress;
                }
                let phase_started = Instant::now();
                let mut processed = 0usize;
                while task.secondary_cursor < task.finalized_present_sections.len()
                    && processed < SAVED_LOAD_APPLY_SECONDARY_BATCH_PER_STEP
                {
                    let coord = task.finalized_present_sections[task.secondary_cursor];
                    self.enqueue_generated_fluids_for_chunk(coord);
                    self.mark_chunk_dirty_with_reasons(coord, MeshDirtyReasons::GEOMETRY);
                    task.stats.fluid_enqueues += 1;
                    task.stats.geometry_dirty_marks += 1;
                    for neighbor in Self::neighbor_chunk_coords(coord) {
                        if task.boundary_dirty_seen.insert(neighbor) {
                            task.boundary_dirty_chunks.push(neighbor);
                        }
                    }
                    task.secondary_cursor += 1;
                    processed += 1;
                    if Self::saved_load_apply_should_yield(outer_started, phase_started, unbounded)
                    {
                        break;
                    }
                }
                let step_ms = phase_started.elapsed().as_secs_f32() * 1000.0;
                task.stats.secondary_ms += step_ms;
                self.frame_perf.saved_load_apply_secondary_ms += step_ms;
                self.record_saved_load_phase_step_ms(task.phase, step_ms);
                perf_trace::record_duration_ms(
                    "saved_load.apply_secondary",
                    step_ms,
                    &format!(
                        "cx={} cz={} chunks={} cursor={}/{} boundary_pending={} inner_budget_ms={:.2}",
                        task.cx,
                        task.cz,
                        processed,
                        task.secondary_cursor,
                        task.finalized_present_sections.len(),
                        task.boundary_dirty_chunks
                            .len()
                            .saturating_sub(task.boundary_cursor),
                        SAVED_LOAD_APPLY_PHASE_BUDGET_MS
                    ),
                );
                if task.secondary_cursor >= task.finalized_present_sections.len() {
                    task.phase = SavedLoadApplyPhase::MarkBoundaryDirty;
                }
                SavedLoadApplyTaskOutcome::InProgress
            }
            SavedLoadApplyPhase::MarkBoundaryDirty => {
                if task.boundary_cursor >= task.boundary_dirty_chunks.len() {
                    task.phase = SavedLoadApplyPhase::Finish;
                    return SavedLoadApplyTaskOutcome::InProgress;
                }
                let phase_started = Instant::now();
                let mut count = 0usize;
                while task.boundary_cursor < task.boundary_dirty_chunks.len()
                    && count < SAVED_LOAD_APPLY_BOUNDARY_BATCH_PER_STEP
                {
                    self.mark_chunk_boundary_dirty(
                        task.boundary_dirty_chunks[task.boundary_cursor],
                    );
                    task.boundary_cursor += 1;
                    count += 1;
                    if Self::saved_load_apply_should_yield(outer_started, phase_started, unbounded)
                    {
                        break;
                    }
                }
                let step_ms = phase_started.elapsed().as_secs_f32() * 1000.0;
                task.stats.secondary_ms += step_ms;
                task.stats.boundary_dirty_marks += count;
                self.frame_perf.saved_load_boundary_marks += count;
                self.frame_perf.saved_load_apply_secondary_ms += step_ms;
                self.record_saved_load_phase_step_ms(task.phase, step_ms);
                perf_trace::record_duration_ms(
                    "saved_load.apply_boundary_dirty",
                    step_ms,
                    &format!(
                        "cx={} cz={} marks={} cursor={}/{} inner_budget_ms={:.2}",
                        task.cx,
                        task.cz,
                        count,
                        task.boundary_cursor,
                        task.boundary_dirty_chunks.len(),
                        SAVED_LOAD_APPLY_PHASE_BUDGET_MS
                    ),
                );
                if task.boundary_cursor >= task.boundary_dirty_chunks.len() {
                    task.phase = SavedLoadApplyPhase::Finish;
                }
                SavedLoadApplyTaskOutcome::InProgress
            }
            SavedLoadApplyPhase::Finish => {
                self.mark_column_saved_revisions(task.cx, task.cz);
                self.clear_dirty_column_tracking(task.cx, task.cz);
                SavedLoadApplyTaskOutcome::Completed
            }
        }
    }

    fn finish_saved_load_apply_task_success(&mut self, task: SavedLoadApplyTask) {
        let total_ms = task.started_at.elapsed().as_secs_f32() * 1000.0;
        self.frame_perf.saved_column_load_hits += 1;
        self.frame_perf.saved_load_results_applied += 1;
        self.frame_perf.saved_load_apply_completed += 1;
        self.frame_perf.saved_load_apply_worst_task_ms =
            self.frame_perf.saved_load_apply_worst_task_ms.max(total_ms);
        perf_trace::record_duration_ms(
            "saved_load.column_finalized",
            total_ms,
            &format!(
                "cx={} cz={} phase={} sections_prepared={} sections_finalized={} deferred_batches={} deferred_total={} deferred_applied={} deferred_fluid={} removed_batches={} added_batches={} fluid_calls={} geometry_marks={} boundary_marks={} yields={} resumes={} loaded_any={} backlog={}",
                task.cx,
                task.cz,
                task.phase_label(),
                task.stats.sections_prepared,
                task.stats.sections_finalized,
                task.stats.deferred_batches,
                task.stats.deferred_total,
                task.stats.deferred_applied,
                task.stats.deferred_applied_fluid,
                task.stats.removed_lighting_batches,
                task.stats.added_lighting_batches,
                task.stats.fluid_enqueues,
                task.stats.geometry_dirty_marks,
                task.stats.boundary_dirty_marks,
                task.stats.yields,
                task.stats.resumes,
                task.loaded_any_chunk,
                self.saved_load_apply_backlog_len()
            ),
        );
        if total_ms >= 8.0 {
            crate::console_error!(
                "[perf][saved_apply] col=({}, {}) total_ms={:.2} prep={:.2} commit={:.2} deferred={:.2} finalize={:.2} lighting={:.2} secondary={:.2} sections={}=>{} edits={} chunks={} boundary={} yields={} resumes={} backlog={}",
                task.cx,
                task.cz,
                total_ms,
                task.stats.prepare_ms,
                task.stats.commit_ms,
                task.stats.deferred_ms,
                task.stats.finalize_ms,
                task.stats.lighting_ms,
                task.stats.secondary_ms,
                task.stats.sections_prepared,
                task.stats.sections_finalized,
                task.stats.deferred_applied,
                task.stats.deferred_chunks_touched,
                task.stats.boundary_dirty_marks,
                task.stats.yields,
                task.stats.resumes,
                self.saved_load_apply_backlog_len()
            );
        }
    }

    fn finish_saved_load_apply_task_failed(&mut self, task: SavedLoadApplyTask, err: String) {
        crate::console_error!(
            "[storage][load] staged apply failed for column ({},{}): {}",
            task.cx,
            task.cz,
            err
        );
    }

    fn run_saved_load_apply_to_completion(&mut self, mut task: SavedLoadApplyTask) -> bool {
        let started = Instant::now();
        loop {
            match self.advance_saved_load_apply_task_step(&mut task, started, true) {
                SavedLoadApplyTaskOutcome::InProgress => {
                    task.stats.yields += 1;
                }
                SavedLoadApplyTaskOutcome::Completed => {
                    self.finish_saved_load_apply_task_success(task);
                    return true;
                }
                SavedLoadApplyTaskOutcome::Failed(err) => {
                    self.frame_perf.saved_load_results_failed += 1;
                    self.finish_saved_load_apply_task_failed(task, err);
                    return false;
                }
            }
        }
    }

    fn pump_storage_load_results(&mut self) {
        let _trace_scope = perf_trace::scope("state.pump_storage_load_results");
        if self.saved_loads_suspended() {
            let discarded_results = self.discard_ready_saved_load_results();
            let cleared_apply = self.clear_saved_load_apply_backlog();
            if self.world_generation_task.is_some() {
                self.log_startup_saved_load_bypass_once("pump_storage_load_results");
            }
            if discarded_results > 0 || cleared_apply > 0 {
                crate::console_info!(
                    "[storage][saved_load] suspended: discarded results={} apply_tasks={} (save_root={})",
                    discarded_results,
                    cleared_apply,
                    self.save_root.display()
                );
            }
            return;
        }
        let started = Instant::now();
        let backlog_before = self.storage_loader.result_queue_len_approx();
        let apply_queue_before = self.saved_load_apply_backlog_len();
        self.frame_perf.saved_load_results_backlog_before = self
            .frame_perf
            .saved_load_results_backlog_before
            .max(backlog_before);
        self.frame_perf.saved_load_apply_queue_before = self
            .frame_perf
            .saved_load_apply_queue_before
            .max(apply_queue_before);

        let mut drained = 0usize;
        let mut misses = 0usize;
        let mut failed = 0usize;

        while drained < SAVED_LOAD_RESULTS_APPLY_PER_FRAME {
            let Some(result) = self.storage_loader.try_recv_result() else {
                break;
            };
            drained += 1;
            self.saved_column_loads_inflight
                .remove(&(result.cx, result.cz));
            match result.status {
                StorageColumnLoadStatus::Hit(blob) => {
                    self.saved_load_apply_queue
                        .push_back(self.make_saved_load_apply_task(result.cx, result.cz, blob));
                }
                StorageColumnLoadStatus::Miss => {
                    self.frame_perf.saved_column_load_misses += 1;
                    self.saved_column_missing_cache
                        .insert((result.cx, result.cz), Instant::now());
                    misses += 1;
                }
                StorageColumnLoadStatus::Error(err) => {
                    self.saved_column_missing_cache
                        .remove(&(result.cx, result.cz));
                    failed += 1;
                    crate::console_error!(
                        "[storage][load] async load failed for column ({},{}): {}",
                        result.cx,
                        result.cz,
                        err
                    );
                }
            }
        }

        let backlog_after_drain = self.storage_loader.result_queue_len_approx();
        let apply_queue_after_drain = self.saved_load_apply_backlog_len();
        self.frame_perf.saved_load_results_drained += drained;
        self.frame_perf.saved_load_apply_queue_before = self
            .frame_perf
            .saved_load_apply_queue_before
            .max(apply_queue_after_drain);

        let apply_started = Instant::now();
        let mut steps = 0usize;
        let mut completed = 0usize;
        let mut budget_hit = false;
        let mut cap_hit = false;
        while steps < SAVED_LOAD_APPLY_STEP_CAP_PER_FRAME {
            if apply_started.elapsed().as_secs_f32() * 1000.0 >= SAVED_LOAD_APPLY_BUDGET_MS {
                budget_hit = true;
                break;
            }
            if self.saved_load_apply_active.is_none() {
                let Some(task) = self.saved_load_apply_queue.pop_front() else {
                    break;
                };
                self.saved_load_apply_active = Some(task);
            }
            let Some(mut task) = self.saved_load_apply_active.take() else {
                break;
            };
            if task.stats.yields > 0 {
                task.stats.resumes += 1;
                self.frame_perf.saved_load_apply_resumed += 1;
            }
            steps += 1;
            self.frame_perf.saved_load_apply_steps += 1;
            match self.advance_saved_load_apply_task_step(&mut task, apply_started, false) {
                SavedLoadApplyTaskOutcome::InProgress => {
                    task.stats.yields += 1;
                    self.frame_perf.saved_load_apply_yielded += 1;
                    self.saved_load_apply_active = Some(task);
                }
                SavedLoadApplyTaskOutcome::Completed => {
                    completed += 1;
                    self.finish_saved_load_apply_task_success(task);
                    if completed >= SAVED_LOAD_APPLY_COMPLETIONS_PER_FRAME_CAP
                        && (self.saved_load_apply_active.is_some()
                            || !self.saved_load_apply_queue.is_empty())
                    {
                        cap_hit = true;
                        break;
                    }
                }
                SavedLoadApplyTaskOutcome::Failed(err) => {
                    failed += 1;
                    self.finish_saved_load_apply_task_failed(task, err);
                }
            }
        }
        if steps >= SAVED_LOAD_APPLY_STEP_CAP_PER_FRAME
            && (self.saved_load_apply_active.is_some() || !self.saved_load_apply_queue.is_empty())
        {
            cap_hit = true;
        }
        let apply_ms = apply_started.elapsed().as_secs_f32() * 1000.0;
        self.frame_perf.saved_load_completed_columns_per_sec = if apply_ms > 0.0 {
            completed as f32 * 1000.0 / apply_ms
        } else {
            0.0
        };
        let backlog_after = self.storage_loader.result_queue_len_approx();
        let apply_queue_after = self.saved_load_apply_backlog_len();
        self.frame_perf.saved_load_results_failed += failed;
        self.frame_perf.saved_load_results_backlog_after = self
            .frame_perf
            .saved_load_results_backlog_after
            .max(backlog_after);
        self.frame_perf.saved_load_apply_queue_after = self
            .frame_perf
            .saved_load_apply_queue_after
            .max(apply_queue_after);

        if drained > 0 || steps > 0 || misses > 0 || failed > 0 {
            let active_phase = self
                .saved_load_apply_active
                .as_ref()
                .map(|task| task.phase_label())
                .unwrap_or("idle");
            perf_trace::record_duration_ms(
                "saved_load.apply_to_world",
                apply_ms,
                &format!(
                    "drained={} steps={} completed={} completed_per_sec={:.2} miss={} fail={} discarded={} budget_hit={} cap_hit={} budget_ms={:.1} inner_budget_ms={:.2} step_cap={} done_cap={} active_phase={} rx_q={}=>{}=>{} apply_q={}=>{}=>{} lighting_q[remove={},add={}] sections={}=>{} edits={} chunks={} boundary={} yields={} resumes={} phase_ms[p={:.2},c={:.2},d={:.2},f={:.2},l={:.2},s={:.2}] step_max_ms[p={:.2},c={:.2},d={:.2},f={:.2},l={:.2},s={:.2}] worst_ms={:.2}",
                    drained,
                    steps,
                    completed,
                    self.frame_perf.saved_load_completed_columns_per_sec,
                    misses,
                    failed,
                    0,
                    budget_hit,
                    cap_hit,
                    SAVED_LOAD_APPLY_BUDGET_MS,
                    SAVED_LOAD_APPLY_PHASE_BUDGET_MS,
                    SAVED_LOAD_APPLY_STEP_CAP_PER_FRAME,
                    SAVED_LOAD_APPLY_COMPLETIONS_PER_FRAME_CAP,
                    active_phase,
                    backlog_before,
                    backlog_after_drain,
                    backlog_after,
                    apply_queue_before,
                    apply_queue_after_drain,
                    apply_queue_after,
                    self.pending_removed_lighting_batches.len(),
                    self.pending_chunk_added_lighting_jobs.len(),
                    self.frame_perf.saved_load_sections_prepared,
                    self.frame_perf.saved_load_sections_finalized,
                    self.frame_perf.saved_load_deferred_edits_applied,
                    self.frame_perf.saved_load_chunks_touched,
                    self.frame_perf.saved_load_boundary_marks,
                    self.frame_perf.saved_load_apply_yielded,
                    self.frame_perf.saved_load_apply_resumed,
                    self.frame_perf.saved_load_apply_prepare_ms,
                    self.frame_perf.saved_load_apply_commit_ms,
                    self.frame_perf.saved_load_apply_deferred_ms,
                    self.frame_perf.saved_load_apply_finalize_ms,
                    self.frame_perf.saved_load_apply_lighting_ms,
                    self.frame_perf.saved_load_apply_secondary_ms,
                    self.frame_perf.saved_load_apply_prepare_step_max_ms,
                    self.frame_perf.saved_load_apply_commit_step_max_ms,
                    self.frame_perf.saved_load_apply_deferred_step_max_ms,
                    self.frame_perf.saved_load_apply_finalize_step_max_ms,
                    self.frame_perf.saved_load_apply_lighting_step_max_ms,
                    self.frame_perf.saved_load_apply_secondary_step_max_ms,
                    self.frame_perf.saved_load_apply_worst_task_ms
                ),
            );
        }
        self.frame_perf.ensure_saved_column_load_ms += started.elapsed().as_secs_f32() * 1000.0;
    }

    fn try_load_saved_column_if_present(&mut self, cx: i32, cz: i32) -> bool {
        if self.saved_loads_suspended() {
            return false;
        }
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
            if self.chunk_is_generation_satisfied(coord) || self.chunkgen_inflight.contains(&coord)
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
                let task = self.make_saved_load_apply_task(cx, cz, blob);
                let ok = self.run_saved_load_apply_to_completion(task);
                self.frame_perf.ensure_saved_column_load_ms +=
                    load_started.elapsed().as_secs_f32() * 1000.0;
                ok
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
        match (self.backface_culling_enabled, self.debug_depth_test) {
            (true, true) => &self.render_pipeline_back_depth,
            (false, true) => &self.render_pipeline_no_cull_depth,
            (true, false) => &self.render_pipeline_back_no_depth,
            (false, false) => &self.render_pipeline_no_cull_no_depth,
        }
    }

    fn cutout_world_pipeline(&self) -> &wgpu::RenderPipeline {
        if self.debug_depth_test {
            &self.render_pipeline_no_cull_depth
        } else {
            &self.render_pipeline_no_cull_no_depth
        }
    }

    pub(crate) fn set_backface_culling_enabled(&mut self, enabled: bool) {
        self.backface_culling_enabled = enabled;
    }

    pub(crate) fn toggle_debug_depth_test(&mut self) {
        self.debug_depth_test = !self.debug_depth_test;
        crate::console_info!(
            "debug render: backface_culling={}, depth_test={}",
            self.backface_culling_enabled,
            self.debug_depth_test
        );
    }

    fn queue_debug_remesh_for_loaded_chunks(&mut self) {
        let coords: Vec<ChunkCoord> = self.world.chunks.keys().copied().collect();
        for coord in coords {
            self.mark_chunk_debug_dirty(coord);
        }
    }

    pub(crate) fn toggle_debug_disable_ao(&mut self) {
        self.debug_disable_ao = !self.debug_disable_ao;
        crate::meshing::greedy::set_debug_disable_ao(self.debug_disable_ao);
        self.mesh_debug_epoch = self.mesh_debug_epoch.wrapping_add(1);
        self.queue_debug_remesh_for_loaded_chunks();
        crate::console_info!(
            "debug lighting: ao_disabled={} (remeshing loaded chunks, mesh_debug_epoch={})",
            self.debug_disable_ao,
            self.mesh_debug_epoch
        );
    }

    pub(crate) fn cycle_debug_lighting_view(&mut self) {
        self.debug_lighting_view = self.debug_lighting_view.next();
        crate::console_info!(
            "debug lighting: view={} (normal/fullbright/ao_only/skylight_heatmap/blocklight_heatmap)",
            self.debug_lighting_view.label()
        );
    }

    pub(crate) fn toggle_debug_perf_summary(&mut self) {
        self.debug_perf_summary_enabled = !self.debug_perf_summary_enabled;
        self.perf_summary_window.reset();
        crate::console_info!(
            "debug perf summary: {}",
            if self.debug_perf_summary_enabled {
                "on (1s rolling)"
            } else {
                "off"
            }
        );
    }

    fn maybe_log_perf_summary(&mut self) {
        if !self.debug_perf_summary_enabled {
            self.perf_summary_window.reset();
            return;
        }
        self.perf_summary_window.push_frame(&self.frame_perf);
        if self.perf_summary_window.started_at.elapsed() < RENDER_STATS_REPORT_INTERVAL {
            return;
        }
        let window = std::mem::replace(&mut self.perf_summary_window, PerfSummaryWindow::new());
        if window.frames == 0 {
            return;
        }

        let mut top_subsystems = [
            (
                "process_dirty_meshes",
                window.sum_process_dirty_chunk_meshes_ms,
            ),
            ("chunkgen", window.sum_chunkgen_ms),
            ("mesh", window.sum_mesh_ms),
            ("storage", window.sum_storage_ms),
            ("lighting", window.sum_lighting_ms),
            ("stream", window.sum_stream_ms),
            ("saved_load", window.sum_saved_load_ms),
        ];
        top_subsystems.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let top = top_subsystems
            .iter()
            .take(5)
            .filter(|(_, ms)| *ms > 0.05)
            .map(|(name, ms)| format!("{name}={ms:.1}ms"))
            .collect::<Vec<_>>()
            .join(",");
        let avg_mesh_wait_ms = if window.sum_mesh_upload_wait_samples > 0 {
            window.sum_mesh_upload_wait_ms / window.sum_mesh_upload_wait_samples as f32
        } else {
            0.0
        };
        let avg_near_first_visible_ms = if window.sum_mesh_upload_near_first_visible_count > 0 {
            window.sum_mesh_upload_near_first_visible_ms
                / window.sum_mesh_upload_near_first_visible_count as f32
        } else {
            0.0
        };
        let avg_near_final_ready_ms = if window.sum_mesh_upload_near_final_ready_count > 0 {
            window.sum_mesh_upload_near_final_ready_ms
                / window.sum_mesh_upload_near_final_ready_count as f32
        } else {
            0.0
        };

        crate::console_error!(
            "[perf][summary][1s] frames={} max_frame_ms={:.1} top=[{}] ready(wait_avg_ms={:.2},near_first={}@{:.2}ms,near_final={}@{:.2}ms,final_upgrades={}) coverage(near={},lod={},lod_dc={},near_mem_mb={:.2},lod_mem_mb={:.2}) inflight(mesh={},chunkgen={},lod={},saved_load={}) q(mesh_up={},dirty={},mesh_rx={},chunkgen_rx={},light_rx={},saved_apply={},saved_ack_rx={},saved_req_q={}) saved(work[sec={}=>{},edits={},chunks={},boundary={},steps={},done={},yield={},resume={},cps={:.1}],phase_ms=[{:.1}/{:.1}/{:.1}/{:.1}/{:.1}/{:.1}],step_max=[{:.1}/{:.1}/{:.1}/{:.1}/{:.1}/{:.1}],worst_ms={:.1})",
            window.frames,
            window.max_frame_ms,
            top,
            avg_mesh_wait_ms,
            window.sum_mesh_upload_near_first_visible_count,
            avg_near_first_visible_ms,
            window.sum_mesh_upload_near_final_ready_count,
            avg_near_final_ready_ms,
            window.sum_mesh_upload_finalized_upgrades,
            self.frame_perf.near_visible_chunks,
            self.frame_perf.far_lod_visible_columns,
            self.frame_perf.far_lod_drawcalls,
            self.frame_perf.near_mesh_memory_bytes as f64 / (1024.0 * 1024.0),
            self.frame_perf.far_lod_memory_bytes as f64 / (1024.0 * 1024.0),
            self.inflight.len(),
            self.chunkgen_inflight.len(),
            self.lod_chunkgen_inflight.len(),
            self.saved_column_loads_inflight.len(),
            self.mesh_upload_queue.len(),
            self.dirty_chunk_queue.len(),
            self.mesh_results.len(),
            self.chunkgen_results.len(),
            self.lighting_results.len(),
            self.saved_load_apply_backlog_len(),
            self.storage_loader.result_queue_len_approx(),
            self.storage_loader.queue_len_approx(),
            self.frame_perf.saved_load_sections_prepared,
            self.frame_perf.saved_load_sections_finalized,
            self.frame_perf.saved_load_deferred_edits_applied,
            self.frame_perf.saved_load_chunks_touched,
            self.frame_perf.saved_load_boundary_marks,
            self.frame_perf.saved_load_apply_steps,
            self.frame_perf.saved_load_apply_completed,
            self.frame_perf.saved_load_apply_yielded,
            self.frame_perf.saved_load_apply_resumed,
            self.frame_perf.saved_load_completed_columns_per_sec,
            self.frame_perf.saved_load_apply_prepare_ms,
            self.frame_perf.saved_load_apply_commit_ms,
            self.frame_perf.saved_load_apply_deferred_ms,
            self.frame_perf.saved_load_apply_finalize_ms,
            self.frame_perf.saved_load_apply_lighting_ms,
            self.frame_perf.saved_load_apply_secondary_ms,
            self.frame_perf.saved_load_apply_prepare_step_max_ms,
            self.frame_perf.saved_load_apply_commit_step_max_ms,
            self.frame_perf.saved_load_apply_deferred_step_max_ms,
            self.frame_perf.saved_load_apply_finalize_step_max_ms,
            self.frame_perf.saved_load_apply_lighting_step_max_ms,
            self.frame_perf.saved_load_apply_secondary_step_max_ms,
            self.frame_perf.saved_load_apply_worst_task_ms,
        );
    }

    pub(crate) fn begin_frame_profiling(&mut self, frame_dt_ms: f32) {
        self.frame_perf.begin_frame(frame_dt_ms);
        self.frame_perf.deferred_edits_pending_total = self.deferred_block_edits_total;
        if self.sim_system_last_frame_ms.len() != self.sim_systems.len() {
            self.sim_system_last_frame_ms
                .resize(self.sim_systems.len(), 0.0);
        } else {
            self.sim_system_last_frame_ms.fill(0.0);
        }
        if self.sim_system_last_frame_ticks.len() != self.sim_systems.len() {
            self.sim_system_last_frame_ticks
                .resize(self.sim_systems.len(), 0);
        } else {
            self.sim_system_last_frame_ticks.fill(0);
        }
    }

    pub(crate) fn toggle_far_lod_enabled(&mut self) {
        self.far_lod_enabled = !self.far_lod_enabled;
        if !self.far_lod_enabled {
            self.lod_meshes.clear();
            self.pending_lod_mesh_uploads.clear();
            self.lod_chunkgen_inflight.clear();
            self.lod_ring_offsets_cache.clear();
            self.lod_request_cursor = 0;
        } else {
            self.last_lod_stream_center = None;
            self.lod_request_cursor = 0;
        }
        crate::console_info!(
            "far LOD: enabled={} requests_per_frame={} gpu_uploads_per_frame={} pending_uploads={}",
            self.far_lod_enabled,
            self.far_lod_requests_per_frame,
            self.far_lod_gpu_uploads_per_frame,
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
            "[perf][stall] frame={:.1}ms tops=[{}] process_dirty_chunk_meshes(ms={:.1},tops=[{}]) chunkgen(ms={:.1},drain={},q={}=>{},applied={},pure_ms={:.1},sidefx_ms={:.1},sidefx_q={}=>{},queued={},done={},light[a={},r={}],fluid_cells={},fluid_scans={},overshoot={},sched[pend={},heap={},reprio={},rebuild={}@{:.1}ms,fcache={}/{}@{:.1}ms,hcache={}/{}@{:.1}ms],lod_results={},lod_req={},lod_up={},lod_up_q={},lod_gpu_ms={:.1}) mesh(ms={:.1},drain={},q={}=>{},up_q={}=>{},uploads={},gpu_up_ms={:.1},wait_avg/max_ms={:.2}/{:.2},near_first={}@{:.2}/{:.2}ms,near_final={}@{:.2}/{:.2}ms,final_upgrades={},budget_boost_ms={:.2},dirty[g={},l={},b={}],coalesced={},avoided={},dispatch[b={},l={}]) storage(ms={:.1},drain={},q={}=>{},ok={},fail={},autosave_q={}) lighting(ms={:.1},drain={},q={}=>{},changed={},inc_jobs={},steps={},primary={},region={},inc_chunks={},voxels={},boundary={},split_jobs={},step_max_ms={:.2},backlog={}) stream(ms={:.1},far_lod={},lod_center={:?},lod_center_changes={},lod_cpu_skip={},lod_backlog_skip={}) coverage(near={},lod={},lod_dc={},near_mem_mb={:.2},lod_mem_mb={:.2}) saved_load(ms={:.1},attempts={},hits={},misses={},drain={},applied={},discarded={},failed={},rx_q={}=>{},apply_q={}=>{},work[sec={}=>{},edits={},chunks={},boundary={},steps={},done={},yield={},resume={},cps={:.1}],phase_ms=[{:.1}/{:.1}/{:.1}/{:.1}/{:.1}/{:.1}],step_max=[{:.1}/{:.1}/{:.1}/{:.1}/{:.1}/{:.1}],worst_ms={:.1}) deferred(pending={},applied={},chunks_touched={},dropped_caps={}) salvage(chunks={},edits={},skipped_dense={}) view={} inflight(mesh={},chunkgen={},lod={}) backlogs(mesh_rx={},chunkgen_rx={},light_rx={},save_rx={},save_ack_rx={},mesh_up_q={},lod_up_q={})",
            frame_dt_ms,
            top_scopes,
            self.frame_perf.process_dirty_chunk_meshes_ms,
            top_proc_scopes,
            self.frame_perf.pump_chunkgen_results_ms,
            self.frame_perf.chunkgen_results_drained,
            self.frame_perf.chunkgen_results_backlog_before,
            self.frame_perf.chunkgen_results_backlog_after,
            self.frame_perf.chunkgen_chunks_applied,
            self.frame_perf.chunkgen_apply_pure_ms,
            self.frame_perf.chunkgen_apply_followup_ms,
            self.frame_perf.chunkgen_apply_followup_backlog_before,
            self.frame_perf.chunkgen_apply_followup_backlog_after,
            self.frame_perf.chunkgen_apply_followup_tasks_queued,
            self.frame_perf.chunkgen_apply_followup_completed,
            self.frame_perf.chunkgen_apply_lighting_add_queued,
            self.frame_perf.chunkgen_apply_lighting_remove_queued,
            self.frame_perf.chunkgen_apply_fluid_bootstrap_cells_queued,
            self.frame_perf.chunkgen_apply_deferred_fluid_scans_queued,
            self.frame_perf.chunkgen_apply_cpu_target_overshoots,
            self.frame_perf.stream_scheduler_pending_peak,
            self.frame_perf.stream_scheduler_heap_peak,
            self.frame_perf.stream_scheduler_reprioritized_items,
            self.frame_perf.stream_scheduler_heap_rebuild_items,
            self.frame_perf.stream_scheduler_heap_rebuild_ms,
            self.frame_perf.stream_scheduler_frustum_cache_hits,
            self.frame_perf.stream_scheduler_frustum_cache_misses,
            self.frame_perf.stream_scheduler_frustum_refresh_ms,
            self.frame_perf.stream_scheduler_horizon_cache_hits,
            self.frame_perf.stream_scheduler_horizon_cache_misses,
            self.frame_perf.stream_scheduler_horizon_refresh_ms,
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
            if self.frame_perf.mesh_upload_wait_samples > 0 {
                self.frame_perf.mesh_upload_wait_total_ms
                    / self.frame_perf.mesh_upload_wait_samples as f32
            } else {
                0.0
            },
            self.frame_perf.mesh_upload_wait_max_ms,
            self.frame_perf.mesh_upload_near_first_visible_count,
            if self.frame_perf.mesh_upload_near_first_visible_count > 0 {
                self.frame_perf.mesh_upload_near_first_visible_total_ms
                    / self.frame_perf.mesh_upload_near_first_visible_count as f32
            } else {
                0.0
            },
            self.frame_perf.mesh_upload_near_first_visible_max_ms,
            self.frame_perf.mesh_upload_near_final_ready_count,
            if self.frame_perf.mesh_upload_near_final_ready_count > 0 {
                self.frame_perf.mesh_upload_near_final_ready_total_ms
                    / self.frame_perf.mesh_upload_near_final_ready_count as f32
            } else {
                0.0
            },
            self.frame_perf.mesh_upload_near_final_ready_max_ms,
            self.frame_perf.mesh_upload_finalized_upgrades,
            self.frame_perf.mesh_upload_budget_boost_ms,
            self.frame_perf.mesh_dirty_geometry_events,
            self.frame_perf.mesh_dirty_lighting_events,
            self.frame_perf.mesh_dirty_boundary_events,
            self.frame_perf.mesh_dirty_coalesced_events,
            self.frame_perf.mesh_avoided_remeshes,
            self.frame_perf.mesh_boundary_dispatches,
            self.frame_perf.mesh_lighting_dispatches,
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
            self.frame_perf.lighting_incremental_jobs,
            self.frame_perf.lighting_incremental_steps,
            self.frame_perf.lighting_incremental_primary_chunks,
            self.frame_perf.lighting_incremental_region_chunks,
            self.frame_perf.lighting_incremental_chunks_touched,
            self.frame_perf.lighting_incremental_voxels_visited,
            self.frame_perf.lighting_incremental_boundary_propagations,
            self.frame_perf.lighting_incremental_split_jobs,
            self.frame_perf.lighting_incremental_step_max_ms,
            self.frame_perf.lighting_incremental_backlog_depth,
            self.frame_perf.enqueue_chunks_within_view_ms,
            self.far_lod_enabled,
            self.last_lod_stream_center,
            self.frame_perf.far_lod_center_changes,
            self.frame_perf.far_lod_requests_suppressed_cpu_budget,
            self.frame_perf.far_lod_requests_suppressed_backlog,
            self.frame_perf.near_visible_chunks,
            self.frame_perf.far_lod_visible_columns,
            self.frame_perf.far_lod_drawcalls,
            self.frame_perf.near_mesh_memory_bytes as f64 / (1024.0 * 1024.0),
            self.frame_perf.far_lod_memory_bytes as f64 / (1024.0 * 1024.0),
            self.frame_perf.ensure_saved_column_load_ms,
            self.frame_perf.saved_column_load_attempts,
            self.frame_perf.saved_column_load_hits,
            self.frame_perf.saved_column_load_misses,
            self.frame_perf.saved_load_results_drained,
            self.frame_perf.saved_load_results_applied,
            self.frame_perf.saved_load_results_discarded,
            self.frame_perf.saved_load_results_failed,
            self.frame_perf.saved_load_results_backlog_before,
            self.frame_perf.saved_load_results_backlog_after,
            self.frame_perf.saved_load_apply_queue_before,
            self.frame_perf.saved_load_apply_queue_after,
            self.frame_perf.saved_load_sections_prepared,
            self.frame_perf.saved_load_sections_finalized,
            self.frame_perf.saved_load_deferred_edits_applied,
            self.frame_perf.saved_load_chunks_touched,
            self.frame_perf.saved_load_boundary_marks,
            self.frame_perf.saved_load_apply_steps,
            self.frame_perf.saved_load_apply_completed,
            self.frame_perf.saved_load_apply_yielded,
            self.frame_perf.saved_load_apply_resumed,
            self.frame_perf.saved_load_completed_columns_per_sec,
            self.frame_perf.saved_load_apply_prepare_ms,
            self.frame_perf.saved_load_apply_commit_ms,
            self.frame_perf.saved_load_apply_deferred_ms,
            self.frame_perf.saved_load_apply_finalize_ms,
            self.frame_perf.saved_load_apply_lighting_ms,
            self.frame_perf.saved_load_apply_secondary_ms,
            self.frame_perf.saved_load_apply_prepare_step_max_ms,
            self.frame_perf.saved_load_apply_commit_step_max_ms,
            self.frame_perf.saved_load_apply_deferred_step_max_ms,
            self.frame_perf.saved_load_apply_finalize_step_max_ms,
            self.frame_perf.saved_load_apply_lighting_step_max_ms,
            self.frame_perf.saved_load_apply_secondary_step_max_ms,
            self.frame_perf.saved_load_apply_worst_task_ms,
            self.frame_perf.deferred_edits_pending_total,
            self.frame_perf.deferred_edits_applied,
            self.frame_perf.deferred_edits_chunks_touched,
            self.frame_perf.deferred_edits_dropped_due_to_caps,
            self.frame_perf.placeholder_salvage_chunks,
            self.frame_perf.placeholder_salvage_edits_enqueued,
            self.frame_perf.placeholder_salvage_skipped_dense,
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
            crate::console_error!("[wgpu][error] uncaptured: {err}");
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
        debug_assert_block_blueprint_integrity(&block_registry);
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
        crate::console_info!(
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
                Ok(_) => crate::console_info!("texture self-test ok: {}", key),
                Err(err) => crate::console_error!("texture self-test failed for {}: {}", key, err),
            }
        }
        let hud_atlas_source = pick_hud_atlas_source(&mut texture_registry);
        let inventory_atlas_source = pick_inventory_atlas_source(&mut texture_registry);
        let inventory_gui_key = inventory_atlas_source.texture_key();
        let skin_texture_key = pick_skin_texture_key(&mut texture_registry);
        let hud_modern_sprite_scale = if hud_atlas_source == HudAtlasSource::ModernSprites {
            let handle = texture_registry
                .get_or_load(HUD_GUI_HOTBAR_KEY)
                .unwrap_or_else(|err| panic!("failed to load HUD hotbar texture: {}", err));
            let image = texture_registry.get(&handle).unwrap_or_else(|| {
                panic!(
                    "missing cached HUD hotbar image for '{}'",
                    HUD_GUI_HOTBAR_KEY
                )
            });
            let sx = image.width / HUD_HOTBAR_BG_RECT_CLASSIC.w;
            let sy = image.height / HUD_HOTBAR_BG_RECT_CLASSIC.h;
            sx.min(sy).max(1)
        } else {
            1
        };

        let block_texture_keys = block_registry.collect_texture_keys();
        assert!(
            !block_texture_keys.is_empty(),
            "BlockRegistry did not provide any texture keys"
        );

        let mut all_texture_keys = block_texture_keys.clone();
        for required_overlay in REQUIRED_OVERLAY_TEXTURE_KEYS {
            if !all_texture_keys.iter().any(|k| k == required_overlay) {
                all_texture_keys.push(required_overlay.to_string());
            }
        }
        for hud_key in hud_atlas_source.texture_keys() {
            if !all_texture_keys.iter().any(|k| k == hud_key) {
                all_texture_keys.push((*hud_key).to_string());
            }
        }
        if !all_texture_keys.iter().any(|k| k == inventory_gui_key) {
            all_texture_keys.push(inventory_gui_key.to_string());
        }
        if !all_texture_keys.iter().any(|k| k == skin_texture_key) {
            all_texture_keys.push(skin_texture_key.to_string());
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
        let mut ui_texture_layouts: HashMap<String, HudTextureLayout> = HashMap::new();
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
            let is_hud_texture = hud_atlas_source
                .texture_keys()
                .iter()
                .any(|candidate| key == candidate);
            let is_inventory_texture = key == inventory_gui_key;
            let is_ui_layout_texture = is_hud_texture || is_inventory_texture;
            let (sheet_frames, frames_rgba, frame_size, texture_layout) = if is_ui_layout_texture {
                let (content_w, content_h, content_rgba, content_bounds) = if is_hud_texture
                    && hud_atlas_source == HudAtlasSource::ModernSprites
                    && hud_modern_sprite_scale > 1
                {
                    let w = (image.width / hud_modern_sprite_scale).max(1);
                    let h = (image.height / hud_modern_sprite_scale).max(1);
                    let rgba = resize_rgba_nearest(&image.rgba, image.width, image.height, w, h);
                    (w, h, rgba.clone(), GuiRect { x: 0, y: 0, w, h })
                } else if is_inventory_texture {
                    let source_size = inventory_atlas_source.logical_size_px();
                    let bounds_raw =
                        nontransparent_bounds_rgba(&image.rgba, image.width, image.height)
                            .unwrap_or(GuiRect {
                                x: 0,
                                y: 0,
                                w: image.width,
                                h: image.height,
                            });
                    let scale_x = (bounds_raw.w / source_size[0]).max(1);
                    let scale_y = (bounds_raw.h / source_size[1]).max(1);
                    let scale = scale_x.min(scale_y).max(1);
                    let w = (image.width / scale).max(1);
                    let h = (image.height / scale).max(1);
                    let rgba = if scale > 1 {
                        resize_rgba_nearest(&image.rgba, image.width, image.height, w, h)
                    } else {
                        image.rgba.clone()
                    };
                    (
                        w,
                        h,
                        rgba.clone(),
                        nontransparent_bounds_rgba(&rgba, w, h).unwrap_or(GuiRect {
                            x: 0,
                            y: 0,
                            w,
                            h,
                        }),
                    )
                } else {
                    let w = image.width;
                    let h = image.height;
                    (w, h, image.rgba.clone(), GuiRect { x: 0, y: 0, w, h })
                };

                if content_w != content_h {
                    let padded_size = content_w.max(content_h).max(1);
                    let offset_x = (padded_size - content_w) / 2 + content_bounds.x;
                    let offset_y = (padded_size - content_h) / 2 + content_bounds.y;
                    (
                        1,
                        vec![pad_rgba_center(
                            &content_rgba,
                            content_w,
                            content_h,
                            padded_size,
                            padded_size,
                        )],
                        padded_size,
                        Some(HudTextureLayout {
                            frame_size: padded_size,
                            content_offset_x: offset_x,
                            content_offset_y: offset_y,
                            content_w: content_bounds.w,
                            content_h: content_bounds.h,
                        }),
                    )
                } else {
                    (
                        1,
                        vec![content_rgba],
                        content_w,
                        Some(HudTextureLayout {
                            frame_size: content_w,
                            content_offset_x: content_bounds.x,
                            content_offset_y: content_bounds.y,
                            content_w: content_bounds.w,
                            content_h: content_bounds.h,
                        }),
                    )
                }
            } else {
                let (sheet_frames, frames_rgba, frame_size) =
                    decode_texture_frames(image.width, image.height, &image.rgba);
                (sheet_frames, frames_rgba, frame_size, None)
            };
            max_frame_size = max_frame_size.max(frame_size);
            let anim_meta = parse_anim_meta_for_texture(key, sheet_frames);
            debug_assert_eq!(anim_meta.axis, AnimAxis::Vertical);
            if let Some(layout) = texture_layout {
                ui_texture_layouts.insert(key.clone(), layout);
            }
            loaded_textures.push(LoadedTexture {
                key: key.clone(),
                frame_size,
                frames_rgba,
                anim_meta,
            });
        }
        let texture_load_duration = texture_load_start.elapsed();
        crate::console_info!(
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
        crate::console_info!(
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
        for required_key in &block_texture_keys {
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
        let first_person_skin_layer = key_to_layer
            .get(skin_texture_key)
            .copied()
            .unwrap_or_else(|| panic!("required skin texture key '{}' missing", skin_texture_key));
        let hud_atlas = build_hud_atlas(hud_atlas_source, &key_to_layer, &ui_texture_layouts);
        let inventory_atlas =
            build_inventory_atlas(inventory_atlas_source, &key_to_layer, &ui_texture_layouts);

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
        let (sun_w, sun_h, mut sun_rgba) = load_png_rgba(&sun_path)
            .unwrap_or_else(|err| panic!("failed to load sky sun texture: {}", err));
        let moon_path = format!("{ASSET_ROOT}/textures/environment/moon_phases.png");
        let (moon_w, moon_h, mut moon_rgba) = load_png_rgba(&moon_path)
            .unwrap_or_else(|err| panic!("failed to load sky moon texture: {}", err));
        let panorama_paths = std::array::from_fn::<_, MENU_PANORAMA_FACE_COUNT, _>(|index| {
            format!("{ASSET_ROOT}/textures/gui/title/background/panorama_{index}.png")
        });
        let panorama_faces = panorama_paths.map(|path| {
            load_png_rgba(&path)
                .unwrap_or_else(|err| panic!("failed to load menu panorama texture: {}", err))
        });
        apply_black_alpha_mask(&mut sun_rgba);
        apply_black_alpha_mask(&mut moon_rgba);
        let sky_tex_w = panorama_faces
            .iter()
            .fold(sun_w.max(moon_w).max(16), |max_dim, (w, _, _)| {
                max_dim.max(*w)
            });
        let sky_tex_h = panorama_faces
            .iter()
            .fold(sun_h.max(moon_h).max(16), |max_dim, (_, h, _)| {
                max_dim.max(*h)
            });
        let sky_sun_layer = 0_u32;
        let sky_moon_layer = 1_u32;
        let sky_star_layer = 2_u32;
        let menu_panorama_layers = std::array::from_fn(|index| 3_u32 + index as u32);
        let sky_sun_uv_rect = centered_uv_rect_4(sun_w, sun_h, sky_tex_w, sky_tex_h);
        let menu_panorama_uv_rects = std::array::from_fn(|index| {
            let (w, h, _) = &panorama_faces[index];
            centered_uv_rect_4(*w, *h, sky_tex_w, sky_tex_h)
        });
        let white_sky_layer = vec![255u8; (sky_tex_w as usize) * (sky_tex_h as usize) * 4];
        let mut sky_layers = vec![
            pad_rgba_center(&sun_rgba, sun_w, sun_h, sky_tex_w, sky_tex_h),
            pad_rgba_center(&moon_rgba, moon_w, moon_h, sky_tex_w, sky_tex_h),
            white_sky_layer,
        ];
        for (w, h, rgba) in &panorama_faces {
            sky_layers.push(pad_rgba_center(rgba, *w, *h, sky_tex_w, sky_tex_h));
        }
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
        let (menu_button_w, menu_button_h, menu_button_rgba) = load_gui_sprite_logical_rgba(
            ASSET_ROOT,
            MENU_BUTTON_TEXTURE_KEY,
            MENU_BUTTON_LOGICAL_WIDTH_PX,
            MENU_BUTTON_LOGICAL_HEIGHT_PX,
        )
        .unwrap_or_else(|err| panic!("failed to load menu button texture: {}", err));
        let (menu_button_highlighted_w, menu_button_highlighted_h, menu_button_highlighted_rgba) =
            load_gui_sprite_logical_rgba(
                ASSET_ROOT,
                MENU_BUTTON_HIGHLIGHTED_TEXTURE_KEY,
                MENU_BUTTON_LOGICAL_WIDTH_PX,
                MENU_BUTTON_LOGICAL_HEIGHT_PX,
            )
            .unwrap_or_else(|err| {
                panic!("failed to load highlighted menu button texture: {}", err)
            });
        let (menu_button_disabled_w, menu_button_disabled_h, menu_button_disabled_rgba) =
            load_gui_sprite_logical_rgba(
                ASSET_ROOT,
                MENU_BUTTON_DISABLED_TEXTURE_KEY,
                MENU_BUTTON_LOGICAL_WIDTH_PX,
                MENU_BUTTON_LOGICAL_HEIGHT_PX,
            )
            .unwrap_or_else(|err| panic!("failed to load disabled menu button texture: {}", err));
        let worldgen_ui_size = dirt_w
            .max(dirt_h)
            .max(font_w)
            .max(font_h)
            .max(menu_button_w)
            .max(menu_button_h)
            .max(menu_button_highlighted_w)
            .max(menu_button_highlighted_h)
            .max(menu_button_disabled_w)
            .max(menu_button_disabled_h)
            .max(16);
        let worldgen_dirt_layer = 0_u32;
        let worldgen_font_layer = 1_u32;
        let worldgen_menu_button_layer = 2_u32;
        let worldgen_menu_button_highlighted_layer = 3_u32;
        let worldgen_menu_button_disabled_layer = 4_u32;
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
            pad_rgba_center(
                &menu_button_rgba,
                menu_button_w,
                menu_button_h,
                worldgen_ui_size,
                worldgen_ui_size,
            ),
            pad_rgba_center(
                &menu_button_highlighted_rgba,
                menu_button_highlighted_w,
                menu_button_highlighted_h,
                worldgen_ui_size,
                worldgen_ui_size,
            ),
            pad_rgba_center(
                &menu_button_disabled_rgba,
                menu_button_disabled_w,
                menu_button_disabled_h,
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
        let mut worldgen_ui_key_to_layer = HashMap::with_capacity(5);
        worldgen_ui_key_to_layer.insert(
            MENU_BUTTON_TEXTURE_KEY.to_string(),
            worldgen_menu_button_layer,
        );
        worldgen_ui_key_to_layer.insert(
            MENU_BUTTON_HIGHLIGHTED_TEXTURE_KEY.to_string(),
            worldgen_menu_button_highlighted_layer,
        );
        worldgen_ui_key_to_layer.insert(
            MENU_BUTTON_DISABLED_TEXTURE_KEY.to_string(),
            worldgen_menu_button_disabled_layer,
        );
        let menu_button_atlas = build_menu_button_atlas(
            &worldgen_ui_key_to_layer,
            worldgen_ui_size,
            worldgen_ui_size,
            [menu_button_w, menu_button_h],
            [menu_button_highlighted_w, menu_button_highlighted_h],
            [menu_button_disabled_w, menu_button_disabled_h],
        );

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
        crate::console_info!(
            "block texture layer self-check ok: grass(top={}, side={}, bottom={}), stone(layer={})",
            grass_top_layer,
            grass_side_layer,
            grass_bottom_layer,
            stone_top_layer
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
        let render_pipeline_transparent_no_depth = Self::create_world_pipeline(
            &device,
            &config,
            &world_shader,
            &world_pipeline_layout,
            depth.format,
            Some(wgpu::Face::Back),
            Some(wgpu::BlendState::ALPHA_BLENDING),
            wgpu::CompareFunction::Always,
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
        let ui_3d_overlay_pipeline = Self::create_render_pipeline_traced(
            &device,
            &wgpu::RenderPipelineDescriptor {
                label: Some("UI 3D Overlay Pipeline"),
                layout: Some(&ui_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &ui_shader,
                    entry_point: Some("vs_main_3d_overlay"),
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
        crate::console_info!(
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
        let mut sky_object_indices = Vec::with_capacity(SKY_OBJECT_INDEX_COUNT as usize);
        for quad_index in 0..(2 + SKY_STAR_COUNT) {
            let base = (quad_index * 4) as u32;
            sky_object_indices.extend_from_slice(&[
                base,
                base + 1,
                base + 2,
                base,
                base + 2,
                base + 3,
            ]);
        }
        let sky_object_index_buffer =
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Sky Objects Index Buffer"),
                contents: bytemuck::cast_slice(&sky_object_indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        let sky_stars = generate_sky_stars();
        let menu_panorama_vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Menu Panorama Vertex Buffer"),
            size: (std::mem::size_of::<Vertex>() as u64) * MENU_PANORAMA_VERTEX_COUNT as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let menu_panorama_indices: [u32; MENU_PANORAMA_INDEX_COUNT as usize] = [
            0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7, 8, 9, 10, 8, 10, 11, 12, 13, 14, 12, 14, 15, 16,
            17, 18, 16, 18, 19, 20, 21, 22, 20, 22, 23,
        ];
        let menu_panorama_index_buffer =
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Menu Panorama Index Buffer"),
                contents: bytemuck::cast_slice(&menu_panorama_indices),
                usage: wgpu::BufferUsages::INDEX,
            });

        let mut world = match STARTUP_WORLD_MODE {
            StartupWorldMode::AlwaysRegenerate | StartupWorldMode::LoadIfPresentElseGenerate => {
                World::new()
            }
        };
        let save_root = Self::resolve_startup_save_root();
        let world_meta_path = Self::world_meta_path_for(&save_root);
        let mut startup_worldgen_preset = DEFAULT_WORLDGEN_PRESET;
        match world_meta::load(&world_meta_path) {
            Ok(Some(meta)) => {
                let cfg = Self::world_config_from_meta(&meta);
                world.configure_world_config(cfg, meta.preset);
                startup_worldgen_preset = meta.preset;
                crate::console_info!(
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
                crate::console_info!(
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
        Self::load_ticking_areas_into_world(&save_root, &mut world);

        let world_manifest_path = Self::world_manifest_path_for(&save_root);
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
            crate::console_error!(
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
        crate::console_info!(
            "startup fluid rebuild: {:.2}ms",
            fluid_rebuild_started.elapsed().as_secs_f64() * 1000.0
        );

        let (storage_reader, storage_loader, storage_writer) =
            Self::create_storage_services(&save_root)
                .unwrap_or_else(|err| panic!("failed to start storage services: {}", err));

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

        crate::console_info!(
            "[perf][startup] State::new setup before first frame took: {:.2}ms",
            startup_total_start.elapsed().as_secs_f64() * 1000.0
        );

        let mut state = Self {
            surface,
            device,
            queue,
            config,
            supported_present_modes: caps.present_modes.clone(),

            render_pipeline_back_depth,
            render_pipeline_no_cull_depth,
            render_pipeline_back_no_depth,
            render_pipeline_no_cull_no_depth,
            render_pipeline_transparent,
            render_pipeline_transparent_no_depth,
            render_pipeline_lod,
            sky_pipeline,
            highlight_pipeline,
            outline_pipeline,
            ui_pipeline,
            ui_3d_overlay_pipeline,
            backface_culling_enabled: true,
            debug_depth_test: true,
            world,
            zero_light_chunk_data,
            chunk_meshes,
            full_solid_mesh_skip_set: HashSet::new(),
            lod_meshes,
            dirty_chunk_queue,
            dirty_chunk_set,
            mesh_schedule: HashMap::new(),
            deferred_mesh_until_frame: HashMap::new(),
            pending_block_edits: VecDeque::new(),
            pending_block_edit_dirty_chunks: HashSet::new(),
            pending_block_edit_dirty_columns: HashSet::new(),
            deferred_block_edits: HashMap::new(),
            deferred_block_edits_total: 0,
            pending_sand_gravity_checks: VecDeque::new(),
            pending_sand_gravity_check_set: HashSet::new(),
            highlight_mesh: None,
            outline_target: None,
            outline_vertex_buffer,
            crosshair_mesh,
            preview_mesh_cache: HashMap::new(),
            hotbar_overlay_mesh: None,
            hotbar_overlay_cache: None,
            chat_overlay_mesh: None,
            chat_overlay_cache: None,
            inventory_overlay_mesh: None,
            inventory_overlay_cache: None,
            block_select_overlay_mesh: None,
            block_select_overlay_cache: None,
            _texture_registry: texture_registry,
            _world_texture_array: world_texture_array,
            _sky_texture_array: sky_texture_array,
            _worldgen_ui_texture_array: worldgen_ui_texture_array,
            water_overlay_mesh,
            boundary_ocean_mesh: None,
            sky_object_vertex_buffer,
            sky_object_index_buffer,
            menu_panorama_vertex_buffer,
            menu_panorama_index_buffer,
            world_texture_bind_group,
            sky_texture_bind_group,
            worldgen_ui_texture_bind_group,
            _sky_anim_buffer: sky_anim_buffer,
            _worldgen_ui_anim_buffer: worldgen_ui_anim_buffer,
            worldgen_dirt_layer,
            sky_sun_layer,
            sky_moon_layer,
            sky_star_layer,
            menu_panorama_layers,
            sky_sun_uv_rect,
            menu_panorama_uv_rects,
            moon_phase_atlas_size: [sky_tex_w, sky_tex_h],
            sky_stars,
            worldgen_font,
            hud_atlas,
            inventory_atlas,
            menu_button_atlas,
            first_person_skin_layer,
            first_person_arm_tweak: FirstPersonArmTweak::default(),
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
            chunk_origin: HashMap::new(),
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
            sim_system_last_frame_ms: Vec::new(),
            sim_system_last_frame_ticks: Vec::new(),
            fluid_debug_counts: FluidDebugCounts::default(),
            worldgen_feature_debug_counts: WorldgenFeatureDebugCounts::default(),
            chunk_lighting_progress: HashMap::new(),
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
            uploaded_final_lighting_chunks: HashSet::new(),
            last_stream_center: None,
            last_stream_radius: -1,
            radius_offsets_cache: Vec::with_capacity(4096),
            lod_ring_offsets_cache: Vec::with_capacity(1024),
            lod_request_cursor: 0,
            chunkgen_pending_set: HashSet::new(),
            chunkgen_pending_enqueued_frame: HashMap::new(),
            chunkgen_pending_priority: HashMap::new(),
            chunkgen_pending_round_robin: VecDeque::new(),
            chunkgen_pending_heap: BinaryHeap::new(),
            chunkgen_pending_scan_cursor: 0,
            chunkgen_pending_refresh_cursor: 0,
            chunkgen_heap_valid_for_center: None,
            chunkgen_heap_valid_for_radius: -1,
            chunkgen_heap_last_rebuild_frame: 0,
            chunkgen_priority_epoch: 1,
            chunkgen_priority_context_stamp: None,
            chunkgen_stream_epoch: 1,
            last_lod_stream_center: None,
            far_lod_enabled: ENABLE_FAR_LOD_DEFAULT,
            far_lod_requests_per_frame: render_distance.far_lod_requests_per_frame(),
            far_lod_gpu_uploads_per_frame: render_distance.far_lod_gpu_uploads_per_frame(),
            pending_lod_mesh_uploads: VecDeque::new(),
            stream_last_camera_pos: None,
            stream_camera_speed_blocks_per_sec: 0.0,
            stream_last_camera_y: None,
            stream_recent_descend_frames: 0,
            stream_recent_mining_frames: 0,
            stream_horizon_sector_max_elev: [f32::NEG_INFINITY; STREAM_HORIZON_SECTOR_COUNT],
            stream_horizon_has_data: false,
            stream_horizon_built_frame: u64::MAX,
            stream_horizon_cache_key: None,
            stream_horizon_mesh_count: 0,
            stream_horizon_seen_columns: HashSet::with_capacity(4096),
            stream_surface_height_cache: HashMap::with_capacity(4096),
            stream_column_plan_cache: HashMap::with_capacity(4096),
            stream_scheduler_cached_frame: u64::MAX,
            stream_scheduler_cached_frustum: None,
            stream_scheduler_cached_forward_xz: None,
            stream_scheduler_cached_forward_bucket: 0,
            mesh_debug_epoch: 0,
            lod_cached_near_radius: -1,
            lod_cached_far_radius: -1,
            frame_perf: StateFramePerf::default(),
            debug_perf_summary_enabled: false,
            perf_summary_window: PerfSummaryWindow::new(),
            last_lighting_step_ms: 0.0,
            lighting_job_inflight: false,
            lighting_generation: 0,
            lighting_request_pending: false,
            lighting_rebuild_not_before: None,
            pending_chunkgen_apply_followups: VecDeque::new(),
            pending_removed_lighting_batches: VecDeque::new(),
            pending_chunk_added_lighting_jobs: VecDeque::new(),
            pending_chunk_added_lighting_coords: HashSet::new(),
            startup_deferred_chunk_added_lighting_coords: HashSet::new(),
            next_chunk_added_lighting_batch_id: 1,
            pending_lighting_dirty_chunks: VecDeque::new(),
            pending_lighting_dirty_set: HashSet::new(),
            debug_disable_ao: false,
            debug_lighting_view: LightingDebugView::Normal,
            menu_panorama_started_at: Instant::now(),
            world_time: WorldTime::default(),
            save_root,
            storage_reader,
            storage_loader,
            storage_writer,
            saved_revision: HashMap::new(),
            saved_column_missing_cache: HashMap::new(),
            saved_column_loads_inflight: HashSet::new(),
            saved_loads_paused: false,
            startup_saved_load_bypass_logged: false,
            saved_load_apply_queue: VecDeque::new(),
            saved_load_apply_active: None,
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

    fn preferred_present_mode(&self, vsync_enabled: bool) -> wgpu::PresentMode {
        if vsync_enabled {
            for candidate in [
                wgpu::PresentMode::Fifo,
                wgpu::PresentMode::AutoVsync,
                wgpu::PresentMode::FifoRelaxed,
            ] {
                if self.supported_present_modes.contains(&candidate) {
                    return candidate;
                }
            }
        } else {
            for candidate in [
                wgpu::PresentMode::Immediate,
                wgpu::PresentMode::Mailbox,
                wgpu::PresentMode::AutoNoVsync,
            ] {
                if self.supported_present_modes.contains(&candidate) {
                    return candidate;
                }
            }
        }
        self.supported_present_modes
            .first()
            .copied()
            .unwrap_or(wgpu::PresentMode::Fifo)
    }

    pub(crate) fn set_vsync_enabled(&mut self, vsync_enabled: bool) {
        let next_mode = self.preferred_present_mode(vsync_enabled);
        if self.config.present_mode == next_mode {
            return;
        }
        self.config.present_mode = next_mode;
        self.surface.configure(&self.device, &self.config);
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

    fn ensure_preview_mesh_cached(&mut self, block: BlockId) {
        if self.preview_mesh_cache.contains_key(&block) {
            return;
        }
        let (vertices, indices) = build_block_preview_mesh_3d(block, &self.block_texture_lookup);
        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Cached Preview Vertex Buffer"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Cached Preview Index Buffer"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        self.preview_mesh_cache.insert(
            block,
            ChunkMesh {
                vertex_buffer,
                index_buffer,
                index_count: indices.len() as u32,
                vertex_capacity_bytes: std::mem::size_of_val(vertices.as_slice()) as u64,
                index_capacity_bytes: std::mem::size_of_val(indices.as_slice()) as u64,
            },
        );
    }

    fn preview_pipeline_for_block(&self, block: BlockId) -> &wgpu::RenderPipeline {
        let info = self.block_texture_lookup.info_for(block);
        if info.render_layer == RenderLayer::Transparent {
            &self.render_pipeline_transparent_no_depth
        } else if matches!(info.model, BlockModel::CrossPlant) {
            &self.render_pipeline_no_cull_no_depth
        } else {
            &self.render_pipeline_back_no_depth
        }
    }

    fn draw_item_preview<'a>(&'a self, rpass: &mut wgpu::RenderPass<'a>, draw: ItemPreviewDraw) {
        let Some(mesh) = self.preview_mesh_cache.get(&draw.block) else {
            return;
        };
        rpass.set_viewport(
            draw.viewport_x as f32,
            draw.viewport_y as f32,
            draw.viewport_w as f32,
            draw.viewport_h as f32,
            0.0,
            1.0,
        );
        rpass.set_scissor_rect(
            draw.viewport_x,
            draw.viewport_y,
            draw.viewport_w,
            draw.viewport_h,
        );
        rpass.set_pipeline(self.preview_pipeline_for_block(draw.block));
        rpass.set_bind_group(0, &self.world_texture_bind_group, &[]);
        rpass.set_bind_group(1, &self.preview_camera_bind_group, &[]);
        rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
        rpass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
        rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
    }

    pub(crate) fn rebuild_preview_mesh(&mut self, block: BlockId) {
        self.ensure_preview_mesh_cached(block);
    }

    pub(crate) fn inventory_palette_page_count(&self, item_count: usize) -> usize {
        inventory_page_count(item_count, &self.inventory_atlas)
    }

    pub(crate) fn inventory_clamp_palette_page(&self, page: i32, item_count: usize) -> usize {
        if page <= 0 {
            return 0;
        }
        (page as usize).min(
            self.inventory_palette_page_count(item_count)
                .saturating_sub(1),
        )
    }

    pub(crate) fn inventory_palette_item_at_pos_px(
        &self,
        item_count: usize,
        page: usize,
        x: f32,
        y: f32,
    ) -> Option<usize> {
        inventory_palette_item_at_pos_px_internal(
            self.config.width,
            self.config.height,
            &self.inventory_atlas,
            item_count,
            page,
            x,
            y,
        )
    }

    pub(crate) fn inventory_hotbar_slot_at_pos_px(&self, x: f32, y: f32) -> Option<usize> {
        inventory_hotbar_slot_at_pos_px_internal(
            self.config.width,
            self.config.height,
            &self.inventory_atlas,
            x,
            y,
        )
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

    fn stream_planning_world_key(&self) -> StreamPlanningWorldKey {
        let cfg = self.world.config();
        StreamPlanningWorldKey {
            seed: cfg.seed,
            preset: self.worldgen_preset,
            sea_level: cfg.sea_level,
            min_y: cfg.min_y,
            max_y: cfg.max_y,
            chunk_size: cfg.chunk_size,
            boundary_ocean: cfg.boundary_ocean,
            boundary_radius_chunks: cfg.boundary_radius_chunks,
        }
    }

    fn surface_height_cache_key(
        &self,
        column_chunk_x: i32,
        column_chunk_z: i32,
    ) -> SurfaceHeightCacheKey {
        SurfaceHeightCacheKey {
            column_x: column_chunk_x,
            column_z: column_chunk_z,
            world: self.stream_planning_world_key(),
        }
    }

    fn invalidate_stream_planning_cache(&mut self, reason: &str) {
        if self.stream_surface_height_cache.is_empty() && self.stream_column_plan_cache.is_empty() {
            return;
        }
        perf_trace::instant_event(
            "stream_planning.cache_invalidate",
            &format!(
                "reason={} surface_entries={} plan_entries={}",
                reason,
                self.stream_surface_height_cache.len(),
                self.stream_column_plan_cache.len()
            ),
        );
        self.stream_surface_height_cache.clear();
        self.stream_column_plan_cache.clear();
    }

    fn estimated_surface_height_for_column_cached(
        &mut self,
        column_chunk_x: i32,
        column_chunk_z: i32,
    ) -> i32 {
        let key = self.surface_height_cache_key(column_chunk_x, column_chunk_z);
        if let Some(&surface_y) = self.stream_surface_height_cache.get(&key) {
            return surface_y;
        }

        let sample_world_x = column_chunk_x * CHUNK_SIZE + CHUNK_SIZE / 2;
        let sample_world_z = column_chunk_z * CHUNK_SIZE + CHUNK_SIZE / 2;
        let surface_y = self
            .world
            .estimated_surface_height_at(sample_world_x, sample_world_z);
        self.stream_surface_height_cache.insert(key, surface_y);
        surface_y
    }

    fn update_stream_motion_hints_for_camera(&mut self, camera: &Camera) {
        let dt_s = (self.frame_perf.frame_dt_ms.max(0.001) / 1000.0).max(1e-4);
        if let Some(prev_pos) = self.stream_last_camera_pos {
            let delta = camera.position - prev_pos;
            self.stream_camera_speed_blocks_per_sec = delta.length() / dt_s;
        } else {
            self.stream_camera_speed_blocks_per_sec = 0.0;
        }
        self.stream_last_camera_pos = Some(camera.position);

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

    fn chunkgen_apply_followup_limits(&self) -> ChunkGenApplyFollowupLimits {
        if self.world_generation_task.is_some() {
            ChunkGenApplyFollowupLimits {
                budget_ms: STARTUP_CHUNKGEN_APPLY_FOLLOWUP_BUDGET_MS,
                phase_budget_ms: STARTUP_CHUNKGEN_APPLY_FOLLOWUP_PHASE_BUDGET_MS,
                steps_per_pump: STARTUP_CHUNKGEN_APPLY_FOLLOWUP_STEPS_PER_PASS,
                neighbors_per_step: STARTUP_CHUNKGEN_APPLY_FOLLOWUP_NEIGHBORS_PER_STEP,
            }
        } else {
            ChunkGenApplyFollowupLimits {
                budget_ms: CHUNKGEN_APPLY_FOLLOWUP_BUDGET_MS,
                phase_budget_ms: CHUNKGEN_APPLY_FOLLOWUP_PHASE_BUDGET_MS,
                steps_per_pump: CHUNKGEN_APPLY_FOLLOWUP_STEPS_PER_FRAME,
                neighbors_per_step: CHUNKGEN_APPLY_FOLLOWUP_NEIGHBORS_PER_STEP,
            }
        }
    }

    fn mesh_upload_lighting_rank(&self, coord: ChunkCoord) -> u8 {
        match self.chunk_lighting_progress.get(&coord).map(|p| p.state) {
            Some(ChunkLightingReadiness::Finalized) => 0,
            Some(ChunkLightingReadiness::VisualReady) => 1,
            Some(ChunkLightingReadiness::Provisional) => 2,
            None => 1,
        }
    }

    fn mesh_upload_near_rank(
        coord: ChunkCoord,
        center_chunk_x: i32,
        center_chunk_y: i32,
        center_chunk_z: i32,
    ) -> u8 {
        let dx = coord.x - center_chunk_x;
        let dy = coord.y - center_chunk_y;
        let dz = coord.z - center_chunk_z;
        let dist2 = dx * dx + dy * dy + dz * dz;
        if dist2 <= MESH_UPLOAD_IMMEDIATE_RADIUS_CHUNKS * MESH_UPLOAD_IMMEDIATE_RADIUS_CHUNKS {
            0
        } else if dist2
            <= MESH_UPLOAD_NEAR_PRIORITY_RADIUS_CHUNKS * MESH_UPLOAD_NEAR_PRIORITY_RADIUS_CHUNKS
        {
            1
        } else {
            2
        }
    }

    fn mesh_upload_age_rank(queued_at: Instant, now: Instant) -> u32 {
        let waited_ms = now.saturating_duration_since(queued_at).as_millis();
        u32::MAX.saturating_sub(waited_ms.min(u32::MAX as u128) as u32)
    }

    fn mesh_upload_priority_key_for_result(
        &self,
        queued: &QueuedMeshUpload,
        center_chunk_x: i32,
        center_chunk_y: i32,
        center_chunk_z: i32,
        frustum: &Frustum,
        forward_xz: Option<Vec3>,
        now: Instant,
    ) -> MeshUploadPriorityKey {
        let coord = queued.result.coord;
        let dx = coord.x - center_chunk_x;
        let dy = coord.y - center_chunk_y;
        let dz = coord.z - center_chunk_z;
        MeshUploadPriorityKey {
            startup_rank: u8::from(!self.is_worldgen_warmup_coord(coord)),
            near_rank: Self::mesh_upload_near_rank(
                coord,
                center_chunk_x,
                center_chunk_y,
                center_chunk_z,
            ),
            visibility_score: Self::chunk_visibility_score_for_frustum(frustum, coord) as u8,
            mesh_presence_rank: u8::from(self.chunk_meshes.contains_key(&coord)),
            lighting_rank: self.mesh_upload_lighting_rank(coord),
            view_alignment_rank: Self::stream_view_alignment_rank(
                coord,
                center_chunk_x,
                center_chunk_z,
                forward_xz,
            ),
            dist2: dx * dx + dy * dy + dz * dz,
            age: Self::mesh_upload_age_rank(queued.queued_at, now),
        }
    }

    fn mesh_upload_is_near_urgent(
        &self,
        coord: ChunkCoord,
        center_chunk_x: i32,
        center_chunk_y: i32,
        center_chunk_z: i32,
        frustum: &Frustum,
    ) -> bool {
        Self::mesh_upload_near_rank(coord, center_chunk_x, center_chunk_y, center_chunk_z) <= 1
            && Self::chunk_visibility_score_for_frustum(frustum, coord) == 0
    }

    fn count_urgent_near_mesh_uploads(
        &self,
        center_chunk_x: i32,
        center_chunk_y: i32,
        center_chunk_z: i32,
        frustum: &Frustum,
    ) -> usize {
        self.mesh_upload_queue
            .iter()
            .filter(|entry| {
                self.mesh_upload_is_near_urgent(
                    entry.result.coord,
                    center_chunk_x,
                    center_chunk_y,
                    center_chunk_z,
                    frustum,
                )
            })
            .count()
    }

    fn mesh_upload_budget_for_camera(
        &mut self,
        camera: &Camera,
        frustum: &Frustum,
    ) -> (f32, usize) {
        let (center_chunk_x, center_chunk_y, center_chunk_z) = Self::camera_chunk_coords(camera);
        let urgent_near = self.count_urgent_near_mesh_uploads(
            center_chunk_x,
            center_chunk_y,
            center_chunk_z,
            frustum,
        );
        let mut budget_ms = self.stream_budget_for_stage_ms(
            STREAM_MESH_UPLOAD_BUDGET_MS,
            STREAM_BURST_MESH_UPLOAD_BUDGET_MS,
        );
        let base_budget = budget_ms;
        let low_speed = self.stream_camera_speed_blocks_per_sec
            <= MESH_UPLOAD_LOW_SPEED_THRESHOLD_BLOCKS_PER_SEC;
        if urgent_near >= MESH_UPLOAD_NEAR_URGENT_THRESHOLD {
            let requested = if self.world_generation_task.is_some() {
                MESH_UPLOAD_WARMUP_BUDGET_MS
            } else if low_speed {
                MESH_UPLOAD_LOW_SPEED_BUDGET_MS
            } else {
                budget_ms
            };
            if requested > budget_ms {
                let remaining = (STREAM_BURST_MAX_TOTAL_BUDGET_MS
                    - self.stream_burst_budget_used_ms())
                .max(budget_ms);
                budget_ms = requested.min(remaining);
            }
        }
        self.frame_perf.mesh_upload_budget_boost_ms += (budget_ms - base_budget).max(0.0);
        (budget_ms, urgent_near)
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

    fn stream_estimated_surface_height_for_coord(&mut self, coord: ChunkCoord) -> i32 {
        self.estimated_surface_height_for_column_cached(coord.x, coord.z)
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
        let key = StreamHorizonCacheKey {
            camera_chunk: Self::camera_chunk_coords(camera),
            forward_bucket: Self::stream_forward_bucket(camera),
        };
        let mesh_count = self.chunk_meshes.len();
        if self.stream_horizon_cache_key == Some(key)
            && self.stream_horizon_mesh_count == mesh_count
            && self
                .frame_index
                .saturating_sub(self.stream_horizon_built_frame)
                <= STREAM_HORIZON_REUSE_MAX_FRAMES
        {
            self.frame_perf.stream_scheduler_horizon_cache_hits += 1;
            return;
        }

        let started = Instant::now();
        self.stream_horizon_sector_max_elev.fill(f32::NEG_INFINITY);
        self.stream_horizon_seen_columns.clear();

        let chunk_mesh_coords = self.chunk_meshes.keys().copied().collect::<Vec<_>>();
        let mut has_data = false;

        for coord in chunk_mesh_coords {
            if !self.stream_horizon_seen_columns.insert((coord.x, coord.z)) {
                continue;
            }

            let surface_y = self.estimated_surface_height_for_column_cached(coord.x, coord.z);
            let surface_chunk_y = surface_y
                .div_euclid(CHUNK_SIZE)
                .clamp(self.world.min_chunk_y(), self.world.max_chunk_y());
            let surface_coord = ChunkCoord {
                x: coord.x,
                y: surface_chunk_y,
                z: coord.z,
            };
            if !self.chunk_meshes.contains_key(&surface_coord)
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
            if elevation_angle > self.stream_horizon_sector_max_elev[sector] {
                self.stream_horizon_sector_max_elev[sector] = elevation_angle;
                has_data = true;
            }
        }

        self.stream_horizon_has_data = has_data;
        self.stream_horizon_built_frame = self.frame_index;
        self.stream_horizon_cache_key = Some(key);
        self.stream_horizon_mesh_count = mesh_count;
        self.frame_perf.stream_scheduler_horizon_cache_misses += 1;
        self.frame_perf.stream_scheduler_horizon_refresh_ms +=
            started.elapsed().as_secs_f32() * 1000.0;
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
        if !self.stream_horizon_has_data {
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

    fn stream_forward_bucket(camera: &Camera) -> u8 {
        let Some(forward) = Self::camera_forward_xz(camera) else {
            return 0;
        };
        let azimuth = forward.z.atan2(forward.x);
        let normalized = (azimuth + PI) / (2.0 * PI);
        let idx = (normalized * STREAM_PRIORITY_FORWARD_BUCKETS as f32).floor() as i32;
        idx.rem_euclid(STREAM_PRIORITY_FORWARD_BUCKETS as i32) as u8
    }

    fn chunkgen_priority_context_stamp(
        center_chunk_x: i32,
        center_chunk_y: i32,
        center_chunk_z: i32,
        radius: i32,
        camera: Option<&Camera>,
    ) -> ChunkgenPriorityContextStamp {
        ChunkgenPriorityContextStamp {
            center: (center_chunk_x, center_chunk_y, center_chunk_z),
            radius,
            forward_bucket: camera.map(Self::stream_forward_bucket).unwrap_or(0),
        }
    }

    fn cached_stream_scheduler_priority_context(
        &mut self,
        camera: Option<&Camera>,
    ) -> (Option<Frustum>, Option<Vec3>, u8) {
        let Some(camera) = camera else {
            return (None, None, 0);
        };
        if self.stream_scheduler_cached_frame == self.frame_index
            && self.stream_scheduler_cached_frustum.is_some()
        {
            self.frame_perf.stream_scheduler_frustum_cache_hits += 1;
            return (
                self.stream_scheduler_cached_frustum,
                self.stream_scheduler_cached_forward_xz,
                self.stream_scheduler_cached_forward_bucket,
            );
        }

        let started = Instant::now();
        let frustum = self.build_frustum_for_camera(camera);
        let refresh_ms = started.elapsed().as_secs_f32() * 1000.0;
        let forward_xz = Self::camera_forward_xz(camera);
        let forward_bucket = Self::stream_forward_bucket(camera);
        self.stream_scheduler_cached_frame = self.frame_index;
        self.stream_scheduler_cached_frustum = Some(frustum);
        self.stream_scheduler_cached_forward_xz = forward_xz;
        self.stream_scheduler_cached_forward_bucket = forward_bucket;
        self.frame_perf.stream_scheduler_frustum_cache_misses += 1;
        self.frame_perf.stream_scheduler_frustum_refresh_ms += refresh_ms;
        (Some(frustum), forward_xz, forward_bucket)
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
        &mut self,
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
            startup_rank: u8::from(!self.is_worldgen_warmup_coord(coord)),
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
        self.chunkgen_pending_priority.clear();
        self.chunkgen_pending_round_robin.clear();
        self.chunkgen_pending_heap.clear();
        self.chunkgen_pending_scan_cursor = 0;
        self.chunkgen_pending_refresh_cursor = 0;
        self.chunkgen_heap_valid_for_center = None;
        self.chunkgen_heap_valid_for_radius = -1;
        self.chunkgen_priority_context_stamp = None;
    }

    fn startup_radius_chunks(&self) -> i32 {
        self.view_distance_chunks
            .max(self.near_mesh_radius_chunks())
            .max(0)
    }

    fn startup_column_order(
        &self,
        center_chunk_x: i32,
        center_chunk_z: i32,
        radius: i32,
    ) -> Vec<ColumnCoord> {
        let radius_sq = radius * radius;
        let mut cols = Vec::new();
        cols.reserve(((radius * 2 + 1) * (radius * 2 + 1)) as usize);
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                if dx * dx + dz * dz > radius_sq {
                    continue;
                }
                cols.push(ColumnCoord {
                    x: center_chunk_x + dx,
                    z: center_chunk_z + dz,
                });
            }
        }
        cols.sort_by_key(|coord| {
            let dx = coord.x - center_chunk_x;
            let dz = coord.z - center_chunk_z;
            (dx.abs() + dz.abs(), dx * dx + dz * dz)
        });
        cols
    }

    fn build_startup_column_stream_plan(
        &mut self,
        column_chunk_x: i32,
        center_chunk_y: i32,
        column_chunk_z: i32,
        min_chunk_y: i32,
        max_chunk_y: i32,
    ) -> ColumnStreamPlan {
        self.build_column_stream_plan_for_mode(
            column_chunk_x,
            center_chunk_y,
            column_chunk_z,
            min_chunk_y,
            max_chunk_y,
            true,
        )
    }

    fn build_worldgen_warmup_coords(
        &mut self,
        center_chunk_x: i32,
        center_chunk_y: i32,
        center_chunk_z: i32,
    ) -> HashSet<ChunkCoord> {
        let mut coords = HashSet::new();
        let min_chunk_y = self.world.min_chunk_y();
        let max_chunk_y = self.world.max_chunk_y();
        let radius = self.startup_radius_chunks();
        for col in self.startup_column_order(center_chunk_x, center_chunk_z, radius) {
            let plan = self.build_startup_column_stream_plan(
                col.x,
                center_chunk_y,
                col.z,
                min_chunk_y,
                max_chunk_y,
            );
            for cy in plan.tier0_y.into_iter().chain(plan.tier1_y.into_iter()) {
                coords.insert(ChunkCoord {
                    x: col.x,
                    y: cy,
                    z: col.z,
                });
            }
        }
        coords
    }

    fn build_worldgen_warmup_columns(
        &self,
        center_chunk_x: i32,
        center_chunk_z: i32,
    ) -> HashSet<ColumnCoord> {
        self.startup_column_order(center_chunk_x, center_chunk_z, self.startup_radius_chunks())
            .into_iter()
            .collect()
    }

    fn is_worldgen_warmup_coord(&self, coord: ChunkCoord) -> bool {
        self.world_generation_task
            .as_ref()
            .is_some_and(|task| task.startup_plan.target_sections.contains(&coord))
    }

    fn mark_startup_column_generated(&mut self, col: ColumnCoord) {
        let Some(task) = self.world_generation_task.as_mut() else {
            return;
        };
        task.startup_plan.columns_inflight.remove(&col);
        if task.startup_plan.target_columns.contains(&col) {
            task.startup_plan.generated_columns.insert(col);
        }
    }

    fn mark_startup_section_applied(&mut self, coord: ChunkCoord) {
        let Some(task) = self.world_generation_task.as_mut() else {
            return;
        };
        if task.startup_plan.target_sections.contains(&coord) {
            task.startup_plan.applied_sections.insert(coord);
        }
    }

    fn startup_section_has_uploaded_mesh(&self, coord: ChunkCoord) -> bool {
        if self.generated_empty_chunks.contains(&coord)
            || self.full_solid_mesh_skip_set.contains(&coord)
        {
            return true;
        }
        startup_mesh_upload_complete(
            self.mesh_schedule.get(&coord).map(|entry| entry.state),
            self.chunk_meshes.contains_key(&coord),
        )
    }

    fn startup_section_has_completed_mesh(&self, coord: ChunkCoord) -> bool {
        if self.startup_section_has_uploaded_mesh(coord) {
            return true;
        }
        let current_revision = self.world.get_chunk(coord).map(|chunk| chunk.revision);
        self.mesh_upload_queue.iter().any(|queued| {
            queued.result.coord == coord && current_revision == Some(queued.result.revision)
        })
    }

    fn refresh_worldgen_warmup_tracking(&mut self) {
        let Some(task_ref) = self.world_generation_task.as_ref() else {
            return;
        };
        let target_sections: Vec<ChunkCoord> = task_ref
            .startup_plan
            .target_sections
            .iter()
            .copied()
            .collect();
        let generated_columns = task_ref.startup_plan.generated_columns.clone();

        let mut applied_sections = HashSet::with_capacity(target_sections.len());
        let mut meshed_sections = HashSet::with_capacity(target_sections.len());
        let mut uploaded_sections = HashSet::with_capacity(target_sections.len());
        for coord in target_sections {
            if self.chunk_is_generation_satisfied(coord) {
                applied_sections.insert(coord);
            }
            if self.startup_section_has_completed_mesh(coord) {
                meshed_sections.insert(coord);
            }
            if self.startup_section_has_uploaded_mesh(coord) {
                uploaded_sections.insert(coord);
            }
        }

        let Some(task) = self.world_generation_task.as_mut() else {
            return;
        };
        task.startup_plan.generated_columns = generated_columns;
        task.startup_plan.applied_sections = applied_sections;
        task.startup_plan.meshed_sections = meshed_sections;
        task.startup_plan.uploaded_sections = uploaded_sections;

        let target_columns = task.startup_plan.target_columns.len().max(1);
        let target_sections = task.startup_plan.target_sections.len().max(1);
        let stats = WorldLoadStageStats {
            generated_columns: task.startup_plan.generated_columns.len(),
            target_columns,
            applied_sections: task.startup_plan.applied_sections.len(),
            meshed_sections: task.startup_plan.meshed_sections.len(),
            uploaded_sections: task.startup_plan.uploaded_sections.len(),
            target_sections,
        };
        let generated_progress =
            (stats.generated_columns as f32 / target_columns as f32).clamp(0.0, 1.0);
        let applied_progress =
            (stats.applied_sections as f32 / target_sections as f32).clamp(0.0, 1.0);
        let meshed_progress =
            (stats.meshed_sections as f32 / target_sections as f32).clamp(0.0, 1.0);
        let uploaded_progress =
            (stats.uploaded_sections as f32 / target_sections as f32).clamp(0.0, 1.0);
        let progress01 = (generated_progress * 0.35
            + applied_progress * 0.20
            + meshed_progress * 0.25
            + uploaded_progress * 0.20)
            .clamp(0.0, 1.0);
        let phase = if stats.generated_columns < target_columns {
            format!(
                "Generating startup columns {}/{}",
                stats.generated_columns, target_columns
            )
        } else if stats.applied_sections < target_sections {
            format!(
                "Applying startup bands {}/{}",
                stats.applied_sections, target_sections
            )
        } else if stats.meshed_sections < target_sections {
            format!(
                "Meshing startup bands {}/{}",
                stats.meshed_sections, target_sections
            )
        } else {
            format!(
                "Uploading startup meshes {}/{}",
                stats.uploaded_sections, target_sections
            )
        };
        let progress01 = progress01.clamp(0.0, 1.0);
        let progress_permille = (progress01 * 1000.0).round().clamp(0.0, 1000.0) as u16;
        if task.last_reported_phase != phase
            || task.last_reported_progress_permille != progress_permille
            || task.last_reported_stats != stats
        {
            task.pending_progress = Some(WorldGenProgress {
                title: task.kind.overlay_title().to_string(),
                phase: phase.clone(),
                progress01,
                stats,
            });
            task.last_reported_phase = phase;
            task.last_reported_progress_permille = progress_permille;
            task.last_reported_stats = stats;
        }
        self.maybe_enqueue_startup_deferred_final_lighting();
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

    fn build_column_stream_plan_for_mode(
        &mut self,
        column_chunk_x: i32,
        camera_chunk_y: i32,
        column_chunk_z: i32,
        min_chunk_y: i32,
        max_chunk_y: i32,
        startup_mode: bool,
    ) -> ColumnStreamPlan {
        let surface_key = self.surface_height_cache_key(column_chunk_x, column_chunk_z);
        let surface_y =
            self.estimated_surface_height_for_column_cached(column_chunk_x, column_chunk_z);
        let camera_world_y = camera_chunk_y * CHUNK_SIZE + CHUNK_SIZE / 2;
        let prefer_camera_band = if startup_mode {
            camera_world_y < surface_y - SURFACE_STREAM_UNDERGROUND_TRIGGER_BLOCKS
        } else {
            let underground =
                camera_world_y < surface_y - SURFACE_STREAM_UNDERGROUND_TRIGGER_BLOCKS;
            underground || self.stream_prefer_camera_band()
        };
        let secondary_extra = if startup_mode {
            0
        } else {
            self.surface_stream_secondary_extra_chunks()
        };
        let key = ColumnStreamPlanCacheKey {
            surface: surface_key,
            center_chunk_y: camera_chunk_y,
            min_chunk_y,
            max_chunk_y,
            startup_mode,
            prefer_camera_band,
            secondary_extra,
        };
        if let Some(plan) = self.stream_column_plan_cache.get(&key) {
            return plan.clone();
        }

        let surface_chunk_y = surface_y
            .div_euclid(CHUNK_SIZE)
            .clamp(min_chunk_y, max_chunk_y);
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

        let plan = ColumnStreamPlan { tier0_y, tier1_y };
        self.stream_column_plan_cache.insert(key, plan.clone());
        plan
    }

    fn build_column_stream_plan(
        &mut self,
        column_chunk_x: i32,
        camera_chunk_y: i32,
        column_chunk_z: i32,
        min_chunk_y: i32,
        max_chunk_y: i32,
    ) -> ColumnStreamPlan {
        self.build_column_stream_plan_for_mode(
            column_chunk_x,
            camera_chunk_y,
            column_chunk_z,
            min_chunk_y,
            max_chunk_y,
            self.world_generation_task.is_some(),
        )
    }

    fn column_stream_chunk_y_order(
        &mut self,
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

    fn chunk_mesh_gpu_bytes(mesh: &ChunkMesh) -> u64 {
        mesh.vertex_capacity_bytes + mesh.index_capacity_bytes
    }

    fn layered_chunk_mesh_gpu_bytes(mesh: &LayeredChunkMesh) -> u64 {
        mesh.opaque
            .as_ref()
            .map(Self::chunk_mesh_gpu_bytes)
            .unwrap_or(0)
            + mesh
                .cutout
                .as_ref()
                .map(Self::chunk_mesh_gpu_bytes)
                .unwrap_or(0)
            + mesh
                .transparent
                .as_ref()
                .map(Self::chunk_mesh_gpu_bytes)
                .unwrap_or(0)
    }

    fn total_near_mesh_memory_bytes(&self) -> u64 {
        self.chunk_meshes
            .values()
            .map(Self::layered_chunk_mesh_gpu_bytes)
            .sum()
    }

    fn total_lod_mesh_memory_bytes(&self) -> u64 {
        self.lod_meshes
            .values()
            .map(Self::chunk_mesh_gpu_bytes)
            .sum()
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

    fn chunk_is_trusted_generated_or_loaded(&self, coord: ChunkCoord) -> bool {
        matches!(
            self.chunk_origin.get(&coord),
            Some(ChunkOrigin::Generated | ChunkOrigin::Loaded)
        )
    }

    fn chunk_is_generation_satisfied(&self, coord: ChunkCoord) -> bool {
        self.generated_empty_chunks.contains(&coord)
            || self.chunk_is_trusted_generated_or_loaded(coord)
    }

    fn mark_chunk_user_only_if_unknown(&mut self, coord: ChunkCoord) {
        self.chunk_origin
            .entry(coord)
            .or_insert(ChunkOrigin::UserOnly);
    }

    fn salvage_untrusted_chunk_for_generation(&mut self, coord: ChunkCoord) {
        if self.chunk_is_trusted_generated_or_loaded(coord) {
            return;
        }
        if let Some(origin) = self.chunk_origin.get(&coord)
            && *origin == ChunkOrigin::UserOnly
        {
            // Continue to salvage user-only chunks just like unknown placeholders.
        }

        let Some(chunk) = self.world.chunks.remove(&coord) else {
            self.chunk_origin.remove(&coord);
            return;
        };
        self.chunk_origin.remove(&coord);
        self.generated_empty_chunks.remove(&coord);
        self.full_solid_mesh_skip_set.remove(&coord);
        self.chunk_meshes.remove(&coord);
        self.uploaded_final_lighting_chunks.remove(&coord);
        self.dirty_chunk_set.remove(&coord);
        self.inflight.remove(&coord);
        self.mesh_schedule.remove(&coord);
        self.fluid_mesh_coalesce.remove(&coord);
        self.fluid_dirty_due_to_fluids.remove(&coord);
        self.saved_revision.remove(&coord);

        let base_x = coord.x * CHUNK_SIZE;
        let base_y = coord.y * CHUNK_SIZE;
        let base_z = coord.z * CHUNK_SIZE;
        let mut non_air: usize = 0;
        let mut salvaged_edits: Vec<DeferredBlockEdit> = Vec::new();
        salvaged_edits.reserve((CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE) as usize);

        for local_y in 0..CHUNK_SIZE {
            for local_z in 0..CHUNK_SIZE {
                for local_x in 0..CHUNK_SIZE {
                    let block = chunk.get(local_x, local_y, local_z);
                    if block == BlockId::Air {
                        continue;
                    }
                    non_air += 1;
                    if non_air > PLACEHOLDER_SALVAGE_NON_AIR_CAP {
                        self.frame_perf.placeholder_salvage_skipped_dense += 1;
                        crate::console_error!(
                            "[warn][stream] placeholder salvage skipped for chunk ({}, {}, {}): non_air={} exceeds cap={}",
                            coord.x,
                            coord.y,
                            coord.z,
                            non_air,
                            PLACEHOLDER_SALVAGE_NON_AIR_CAP
                        );
                        for neighbor in Self::neighbor_chunk_coords(coord) {
                            self.mark_chunk_boundary_dirty(neighbor);
                        }
                        return;
                    }

                    let world_pos =
                        IVec3::new(base_x + local_x, base_y + local_y, base_z + local_z);
                    let data = chunk.get_data(local_x, local_y, local_z);
                    let kind = if fluid_kind(block).is_some() {
                        DeferredEditKind::PlaceFluid
                    } else {
                        DeferredEditKind::PlaceBlock
                    };
                    salvaged_edits.push(DeferredBlockEdit {
                        world_pos,
                        block,
                        data,
                        frame_index: self.frame_index,
                        kind,
                    });
                }
            }
        }

        let salvaged_count = salvaged_edits.len();
        for edit in salvaged_edits {
            self.defer_block_edit(edit);
        }
        if salvaged_count > 0 {
            self.frame_perf.placeholder_salvage_chunks += 1;
            self.frame_perf.placeholder_salvage_edits_enqueued += salvaged_count;
        }
        for neighbor in Self::neighbor_chunk_coords(coord) {
            self.mark_chunk_boundary_dirty(neighbor);
        }
    }

    fn request_chunk_generation(&mut self, coord: ChunkCoord, preset: WorldgenPreset) -> bool {
        if self.chunk_is_generation_satisfied(coord) || self.chunkgen_inflight.contains(&coord) {
            return false;
        }
        if self.chunkgen_inflight.len() >= MAX_INFLIGHT_CHUNKGEN {
            return false;
        }

        if self.world.get_chunk(coord).is_some() {
            self.salvage_untrusted_chunk_for_generation(coord);
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

    fn column_has_chunkgen_inflight(&self, cx: i32, cz: i32) -> bool {
        for cy in self.world.min_chunk_y()..=self.world.max_chunk_y() {
            if self.chunkgen_inflight.contains(&ChunkCoord {
                x: cx,
                y: cy,
                z: cz,
            }) {
                return true;
            }
        }
        false
    }

    fn request_startup_column_generation(
        &mut self,
        col: ColumnCoord,
        preset: WorldgenPreset,
    ) -> bool {
        let Some(task) = self.world_generation_task.as_ref() else {
            return false;
        };
        if task.startup_plan.generated_columns.contains(&col)
            || task.startup_plan.columns_inflight.contains(&col)
            || self.column_has_chunkgen_inflight(col.x, col.z)
        {
            return false;
        }
        let startup_inflight = task.startup_plan.columns_inflight.len();
        if self.chunkgen_inflight.len() + startup_inflight >= MAX_INFLIGHT_CHUNKGEN {
            return false;
        }

        let job_coord = ChunkCoord {
            x: col.x,
            y: self.world.min_chunk_y(),
            z: col.z,
        };
        let world_cfg = self.world.config();
        match self.chunkgen_jobs.try_send(ChunkGenJob {
            coord: job_coord,
            preset,
            world_cfg,
            request_kind: ChunkGenRequestKind::WarmupColumnOnly,
            epoch: self.chunkgen_stream_epoch,
        }) {
            Ok(()) => {
                if let Some(task) = self.world_generation_task.as_mut() {
                    task.startup_plan.columns_inflight.insert(col);
                }
                true
            }
            Err(CrossbeamTrySendError::Full(_)) => false,
            Err(CrossbeamTrySendError::Disconnected(_)) => false,
        }
    }

    fn queue_startup_column_generation_with_budget(&mut self, budget_columns: usize) -> usize {
        if budget_columns == 0 {
            return 0;
        }
        let Some(task) = self.world_generation_task.as_ref() else {
            return 0;
        };
        if task.startup_plan.column_request_order.is_empty() {
            return 0;
        }

        let order_len = task.startup_plan.column_request_order.len();
        let mut queued = 0usize;
        let mut scanned = 0usize;
        let preset = self.worldgen_preset;
        while queued < budget_columns && scanned < order_len && self.world_generation_task.is_some()
        {
            let idx = {
                let task = self
                    .world_generation_task
                    .as_mut()
                    .expect("startup task missing");
                let idx = task.startup_plan.column_request_cursor % order_len;
                task.startup_plan.column_request_cursor =
                    (task.startup_plan.column_request_cursor + 1) % order_len;
                idx
            };
            let col = {
                let task = self
                    .world_generation_task
                    .as_ref()
                    .expect("startup task missing");
                task.startup_plan.column_request_order[idx]
            };
            scanned += 1;
            if self.request_startup_column_generation(col, preset) {
                queued += 1;
            }
        }
        queued
    }

    fn request_lod_column_generation(
        &mut self,
        coord: LodColumnCoord,
        preset: WorldgenPreset,
    ) -> bool {
        // Reserve independent inflight headroom for LOD-only work so distant coverage can
        // progress even while near chunk streaming is saturating the full-chunk budget.
        if self.view_distance_chunks <= self.near_mesh_radius_chunks() || !self.far_lod_enabled {
            return false;
        }
        if self.lod_meshes.contains_key(&coord) || self.lod_chunkgen_inflight.contains(&coord) {
            return false;
        }
        let total_inflight = self.lod_chunkgen_inflight.len() + self.chunkgen_inflight.len();
        if self.lod_chunkgen_inflight.len() >= MAX_INFLIGHT_LOD_CHUNKGEN
            || total_inflight >= MAX_INFLIGHT_TOTAL_CHUNKGEN_WITH_LOD
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
        let center = (center_chunk_x, center_chunk_y, center_chunk_z);
        if self.chunkgen_heap_valid_for_center != Some(center)
            || self.chunkgen_heap_valid_for_radius != radius
        {
            self.chunkgen_pending_scan_cursor = 0;
        }
        let min_chunk_y = self.world.min_chunk_y();
        let max_chunk_y = self.world.max_chunk_y();
        let scan_limit =
            CHUNKGEN_PENDING_COLUMN_SCAN_PER_FRAME.min(self.radius_offsets_cache.len());
        let mut scanned = 0usize;
        while scanned < scan_limit {
            let idx = self.chunkgen_pending_scan_cursor % self.radius_offsets_cache.len();
            self.chunkgen_pending_scan_cursor =
                (self.chunkgen_pending_scan_cursor + 1) % self.radius_offsets_cache.len();
            let (dx, dz) = self.radius_offsets_cache[idx];
            let x = center_chunk_x + dx;
            let z = center_chunk_z + dz;
            let plan =
                self.build_column_stream_plan(x, center_chunk_y, z, min_chunk_y, max_chunk_y);
            for cy in plan.tier0_y.into_iter().chain(plan.tier1_y.into_iter()) {
                let coord = ChunkCoord { x, y: cy, z };
                if self.chunk_is_generation_satisfied(coord)
                    || self.chunkgen_inflight.contains(&coord)
                {
                    continue;
                }
                self.insert_chunkgen_pending_coord(coord);
            }
            scanned += 1;
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
        let scheduler_started = Instant::now();
        let reprioritized_before = self.frame_perf.stream_scheduler_reprioritized_items;
        let frustum_hits_before = self.frame_perf.stream_scheduler_frustum_cache_hits;
        let frustum_misses_before = self.frame_perf.stream_scheduler_frustum_cache_misses;
        let horizon_hits_before = self.frame_perf.stream_scheduler_horizon_cache_hits;
        let horizon_misses_before = self.frame_perf.stream_scheduler_horizon_cache_misses;
        let frustum_ms_before = self.frame_perf.stream_scheduler_frustum_refresh_ms;
        let horizon_ms_before = self.frame_perf.stream_scheduler_horizon_refresh_ms;

        self.enqueue_chunkgen_candidates_in_radius(
            center_chunk_x,
            center_chunk_y,
            center_chunk_z,
            radius,
        );

        let center = (center_chunk_x, center_chunk_y, center_chunk_z);
        let (frustum, forward_xz, _) = self.cached_stream_scheduler_priority_context(camera);
        if let (Some(camera), Some(frustum)) = (camera, frustum.as_ref()) {
            self.refresh_stream_horizon_map(camera, frustum);
        }
        let context_stamp = Self::chunkgen_priority_context_stamp(
            center_chunk_x,
            center_chunk_y,
            center_chunk_z,
            radius,
            camera,
        );
        if self.chunkgen_priority_context_stamp != Some(context_stamp) {
            self.chunkgen_priority_epoch = self.chunkgen_priority_epoch.wrapping_add(1).max(1);
            self.chunkgen_priority_context_stamp = Some(context_stamp);
            self.chunkgen_pending_refresh_cursor = 0;
        }

        let near_refresh_limit =
            CHUNKGEN_PENDING_NEAR_REFRESH_OFFSETS_PER_FRAME.min(self.radius_offsets_cache.len());
        let min_chunk_y = self.world.min_chunk_y();
        let max_chunk_y = self.world.max_chunk_y();
        let mut near_refreshed = 0usize;
        while near_refreshed < near_refresh_limit {
            let idx = self.chunkgen_pending_refresh_cursor % self.radius_offsets_cache.len();
            self.chunkgen_pending_refresh_cursor =
                (self.chunkgen_pending_refresh_cursor + 1) % self.radius_offsets_cache.len();
            let (dx, dz) = self.radius_offsets_cache[idx];
            let x = center_chunk_x + dx;
            let z = center_chunk_z + dz;
            let plan =
                self.build_column_stream_plan(x, center_chunk_y, z, min_chunk_y, max_chunk_y);
            for cy in plan.tier0_y.into_iter().chain(plan.tier1_y.into_iter()) {
                let coord = ChunkCoord { x, y: cy, z };
                let _ = self.refresh_chunkgen_pending_coord_priority(
                    coord,
                    center_chunk_x,
                    center_chunk_y,
                    center_chunk_z,
                    camera,
                    frustum.as_ref(),
                    forward_xz,
                );
            }
            near_refreshed += 1;
        }

        let mut refreshed = 0usize;
        while refreshed < CHUNKGEN_PENDING_PRIORITY_REFRESH_PER_FRAME {
            let Some(coord) = self.chunkgen_pending_round_robin.pop_front() else {
                break;
            };
            if !self.chunkgen_pending_set.contains(&coord) {
                continue;
            }
            if !Self::is_chunk_within_radius_xz(coord, center_chunk_x, center_chunk_z, radius)
                || self.chunk_is_generation_satisfied(coord)
                || self.chunkgen_inflight.contains(&coord)
            {
                self.remove_chunkgen_pending_coord(coord);
                continue;
            }
            let _ = self.refresh_chunkgen_pending_coord_priority(
                coord,
                center_chunk_x,
                center_chunk_y,
                center_chunk_z,
                camera,
                frustum.as_ref(),
                forward_xz,
            );
            self.chunkgen_pending_round_robin.push_back(coord);
            refreshed += 1;
        }

        let periodic_rebuild = self
            .frame_index
            .saturating_sub(self.chunkgen_heap_last_rebuild_frame)
            >= REBUILD_PERIOD;
        let mut rebuild_items_this_call = 0usize;
        let mut rebuild_ms_this_call = 0.0f32;
        if (periodic_rebuild
            || (self.chunkgen_pending_heap.len()
                > self
                    .chunkgen_pending_set
                    .len()
                    .saturating_mul(CHUNKGEN_PENDING_HEAP_REBUILD_BLOAT_FACTOR)))
            && self.chunkgen_pending_set.len() >= CHUNKGEN_PENDING_HEAP_REBUILD_MIN_SIZE
        {
            let rebuild_started = Instant::now();
            let mut rebuilt = BinaryHeap::with_capacity(self.chunkgen_pending_priority.len());
            let mut rebuild_items = 0usize;
            for (&coord, &(epoch, key)) in &self.chunkgen_pending_priority {
                if !self.chunkgen_pending_set.contains(&coord) {
                    continue;
                }
                rebuilt.push(ChunkPriorityHeapEntry { coord, key, epoch });
                rebuild_items += 1;
            }
            self.chunkgen_pending_heap = rebuilt;
            let rebuild_ms = rebuild_started.elapsed().as_secs_f32() * 1000.0;
            self.chunkgen_heap_last_rebuild_frame = self.frame_index;
            self.frame_perf.stream_scheduler_heap_rebuild_items += rebuild_items;
            self.frame_perf.stream_scheduler_heap_rebuild_ms += rebuild_ms;
            rebuild_items_this_call = rebuild_items;
            rebuild_ms_this_call = rebuild_ms;
        }
        self.frame_perf.stream_scheduler_pending_peak = self
            .frame_perf
            .stream_scheduler_pending_peak
            .max(self.chunkgen_pending_set.len());
        self.frame_perf.stream_scheduler_heap_peak = self
            .frame_perf
            .stream_scheduler_heap_peak
            .max(self.chunkgen_pending_heap.len());

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
            let current = self.chunkgen_pending_priority.get(&coord).copied();
            let Some((current_epoch, current_key)) = current else {
                let _ = self.refresh_chunkgen_pending_coord_priority(
                    coord,
                    center_chunk_x,
                    center_chunk_y,
                    center_chunk_z,
                    camera,
                    frustum.as_ref(),
                    forward_xz,
                );
                continue;
            };
            if current_epoch != self.chunkgen_priority_epoch
                || entry.epoch != current_epoch
                || entry.key != current_key
            {
                let _ = self.refresh_chunkgen_pending_coord_priority(
                    coord,
                    center_chunk_x,
                    center_chunk_y,
                    center_chunk_z,
                    camera,
                    frustum.as_ref(),
                    forward_xz,
                );
                continue;
            }
            if self.chunk_is_generation_satisfied(coord) || self.chunkgen_inflight.contains(&coord)
            {
                self.remove_chunkgen_pending_coord(coord);
                continue;
            }
            if self.request_chunk_generation(coord, preset) {
                queued += 1;
                self.remove_chunkgen_pending_coord(coord);
            } else {
                break;
            }
        }
        self.chunkgen_heap_valid_for_center = Some(center);
        self.chunkgen_heap_valid_for_radius = radius;
        let scheduler_ms = scheduler_started.elapsed().as_secs_f32() * 1000.0;
        perf_trace::record_duration_ms(
            "state.queue_chunkgen_requests",
            scheduler_ms,
            &format!(
                "queued={} pending={} heap={} inflight={} job_q={} reprioritized={} rebuild_items={} rebuild_ms={:.2} frustum_cache[h={},m={}] horizon_cache[h={},m={}] frustum_ms={:.2} horizon_ms={:.2}",
                queued,
                self.chunkgen_pending_set.len(),
                self.chunkgen_pending_heap.len(),
                self.chunkgen_inflight.len(),
                self.chunkgen_jobs.len(),
                self.frame_perf
                    .stream_scheduler_reprioritized_items
                    .saturating_sub(reprioritized_before),
                rebuild_items_this_call,
                rebuild_ms_this_call,
                self.frame_perf
                    .stream_scheduler_frustum_cache_hits
                    .saturating_sub(frustum_hits_before),
                self.frame_perf
                    .stream_scheduler_frustum_cache_misses
                    .saturating_sub(frustum_misses_before),
                self.frame_perf
                    .stream_scheduler_horizon_cache_hits
                    .saturating_sub(horizon_hits_before),
                self.frame_perf
                    .stream_scheduler_horizon_cache_misses
                    .saturating_sub(horizon_misses_before),
                self.frame_perf.stream_scheduler_frustum_refresh_ms - frustum_ms_before,
                self.frame_perf.stream_scheduler_horizon_refresh_ms - horizon_ms_before
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
        if self.world_generation_task.is_some() {
            let startup_column_budget =
                STARTUP_COLUMN_WARMUP_REQUESTS_PER_FRAME.min(budget.saturating_sub(1).max(1));
            let queued_columns =
                self.queue_startup_column_generation_with_budget(startup_column_budget);
            budget = budget.saturating_sub(queued_columns);
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
            crate::console_info!(
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
        if far_radius <= near_radius || self.far_lod_requests_per_frame == 0 {
            self.lod_meshes.clear();
            self.pending_lod_mesh_uploads.clear();
            self.lod_chunkgen_inflight.clear();
            self.last_lod_stream_center = None;
            self.lod_ring_offsets_cache.clear();
            self.lod_request_cursor = 0;
            return;
        }
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
                crate::console_error!(
                    "[perf][lod] center_changed from=({}, {}) to=({}, {}) near={} far={} cursor_reset",
                    prev.0,
                    prev.1,
                    center.0,
                    center.1,
                    near_radius,
                    far_radius
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
                "center=({}, {}) queued={} inflight_lod={} cursor={}/{} near={} far={} req_budget={}",
                center_chunk_x,
                center_chunk_z,
                queued,
                self.lod_chunkgen_inflight.len(),
                self.lod_request_cursor,
                self.lod_ring_offsets_cache.len(),
                near_radius,
                far_radius,
                self.far_lod_requests_per_frame
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
        while uploads < self.far_lod_gpu_uploads_per_frame {
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
                        if fluid_data_is_worldgen_static(data) {
                            continue;
                        }
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

    fn accumulate_worldgen_feature_debug_stats(&mut self, stats: GeneratedWorldgenDebugStats) {
        self.worldgen_feature_debug_counts.chunks_applied = self
            .worldgen_feature_debug_counts
            .chunks_applied
            .saturating_add(1);
        self.worldgen_feature_debug_counts
            .total_worldgen_water_blocks = self
            .worldgen_feature_debug_counts
            .total_worldgen_water_blocks
            .saturating_add(stats.worldgen_water_blocks as u64);
        self.worldgen_feature_debug_counts
            .total_worldgen_water_source_blocks = self
            .worldgen_feature_debug_counts
            .total_worldgen_water_source_blocks
            .saturating_add(stats.worldgen_water_source_blocks as u64);
        self.worldgen_feature_debug_counts
            .total_shoreline_sand_blocks = self
            .worldgen_feature_debug_counts
            .total_shoreline_sand_blocks
            .saturating_add(stats.shoreline_sand_blocks as u64);
        self.worldgen_feature_debug_counts
            .last_worldgen_water_blocks = stats.worldgen_water_blocks;
        self.worldgen_feature_debug_counts
            .last_worldgen_water_source_blocks = stats.worldgen_water_source_blocks;
        self.worldgen_feature_debug_counts
            .last_shoreline_sand_blocks = stats.shoreline_sand_blocks;
    }

    fn chunkgen_apply_followup_backlog_len(&self) -> usize {
        self.pending_chunkgen_apply_followups.len()
    }

    fn chunkgen_apply_lighting_backlog_len(&self) -> usize {
        self.pending_chunk_added_lighting_jobs.len()
            + self.pending_removed_lighting_batches.len()
            + self.pending_lighting_dirty_chunks.len()
            + self.lighting_results.len()
            + usize::from(self.lighting_job_inflight)
            + usize::from(self.lighting_request_pending)
    }

    fn chunkgen_apply_mesh_backlog_len(&self) -> usize {
        self.dirty_chunk_queue.len()
            + self.inflight.len()
            + self.mesh_results.len()
            + self.mesh_upload_queue.len()
    }

    fn chunkgen_apply_drain_limits(&mut self) -> ChunkGenDrainLimits {
        let cpu_ms = self.frame_perf.elapsed_ms_since_frame_start();
        let lighting_backlog = self.chunkgen_apply_lighting_backlog_len();
        let mesh_backlog = self.chunkgen_apply_mesh_backlog_len();
        let followup_backlog = self.chunkgen_apply_followup_backlog_len();
        let saved_load_backlog = self.saved_load_apply_backlog_len();
        if self.world_generation_task.is_some() {
            let limits = ChunkGenDrainLimits {
                max_results: STARTUP_CHUNKGEN_APPLY_RESULTS_PER_PASS,
                budget_ms: STARTUP_CHUNKGEN_APPLY_BUDGET_MS,
                cpu_ms,
                lighting_backlog,
                mesh_backlog,
                followup_backlog,
                saved_load_backlog,
                overshoot: false,
            };
            perf_trace::instant_event(
                "chunkgen.apply_startup_budget",
                &format!(
                    "cpu_ms={:.2} max_results={} budget_ms={:.2} lighting_backlog={} mesh_backlog={} followup_backlog={} saved_load_backlog={}",
                    limits.cpu_ms,
                    limits.max_results,
                    limits.budget_ms,
                    limits.lighting_backlog,
                    limits.mesh_backlog,
                    limits.followup_backlog,
                    limits.saved_load_backlog
                ),
            );
            return limits;
        }
        let mut max_results = if self.frame_perf.stream_burst_mode_active {
            CHUNKGEN_APPLY_RESULTS_PER_FRAME_BURST
        } else {
            CHUNKGEN_APPLY_RESULTS_PER_FRAME_BASE
        };
        let mut budget_ms = self.stream_budget_for_stage_ms(
            STREAM_CHUNKGEN_APPLY_BUDGET_MS,
            STREAM_BURST_CHUNKGEN_APPLY_BUDGET_MS,
        );
        let overshoot = cpu_ms >= CHUNKGEN_APPLY_CPU_TARGET_MS;
        if overshoot {
            self.frame_perf.chunkgen_apply_cpu_target_overshoots += 1;
            max_results = CHUNKGEN_APPLY_RESULTS_PER_FRAME_MIN;
            budget_ms = budget_ms.min(0.75);
        }
        if cpu_ms >= CHUNKGEN_APPLY_CPU_HARD_STOP_MS {
            max_results = 0;
            budget_ms = 0.0;
        }
        if lighting_backlog >= CHUNKGEN_APPLY_BACKPRESSURE_LIGHTING_THRESHOLD {
            max_results = max_results.min(2);
            budget_ms = budget_ms.min(1.25);
        }
        if mesh_backlog >= CHUNKGEN_APPLY_BACKPRESSURE_MESH_THRESHOLD {
            max_results = max_results.min(2);
            budget_ms = budget_ms.min(1.25);
        }
        if followup_backlog >= CHUNKGEN_APPLY_BACKPRESSURE_FOLLOWUP_THRESHOLD {
            max_results = max_results.min(2);
            budget_ms = budget_ms.min(1.0);
        }
        if saved_load_backlog > 0 {
            max_results = max_results.min(1);
            budget_ms = budget_ms.min(0.75);
        }
        let limits = ChunkGenDrainLimits {
            max_results,
            budget_ms: budget_ms.max(0.0),
            cpu_ms,
            lighting_backlog,
            mesh_backlog,
            followup_backlog,
            saved_load_backlog,
            overshoot,
        };
        if limits.overshoot
            || limits.saved_load_backlog > 0
            || limits.lighting_backlog >= CHUNKGEN_APPLY_BACKPRESSURE_LIGHTING_THRESHOLD
            || limits.mesh_backlog >= CHUNKGEN_APPLY_BACKPRESSURE_MESH_THRESHOLD
            || limits.followup_backlog >= CHUNKGEN_APPLY_BACKPRESSURE_FOLLOWUP_THRESHOLD
        {
            perf_trace::instant_event(
                "chunkgen.apply_backpressure",
                &format!(
                    "cpu_ms={:.2} max_results={} budget_ms={:.2} lighting_backlog={} mesh_backlog={} followup_backlog={} saved_load_backlog={} overshoot={}",
                    limits.cpu_ms,
                    limits.max_results,
                    limits.budget_ms,
                    limits.lighting_backlog,
                    limits.mesh_backlog,
                    limits.followup_backlog,
                    limits.saved_load_backlog,
                    limits.overshoot as u8
                ),
            );
        }
        limits
    }

    fn enqueue_chunkgen_apply_followup(&mut self, task: ChunkGenApplyFollowUpTask) {
        self.frame_perf.chunkgen_apply_followup_tasks_queued += 1;
        self.frame_perf.chunkgen_apply_lighting_add_queued += usize::from(!task.removed_chunk);
        self.frame_perf.chunkgen_apply_lighting_remove_queued += usize::from(task.removed_chunk);
        self.frame_perf.chunkgen_apply_fluid_bootstrap_cells_queued += task.fluid_bootstrap.len();
        self.frame_perf.chunkgen_apply_deferred_fluid_scans_queued +=
            usize::from(task.needs_chunk_fluid_scan);
        self.pending_chunkgen_apply_followups.push_back(task);
    }

    fn chunkgen_apply_followup_has_current_state(&self, task: &ChunkGenApplyFollowUpTask) -> bool {
        if task.removed_chunk {
            self.world.get_chunk(task.coord).is_none()
        } else {
            self.world.get_chunk(task.coord).is_some()
                && !self.generated_empty_chunks.contains(&task.coord)
                && self.chunk_origin.get(&task.coord) == Some(&ChunkOrigin::Generated)
        }
    }

    fn chunk_local_coords_from_index(index: usize) -> (i32, i32, i32) {
        let idx = index as i32;
        let plane = CHUNK_SIZE * CHUNK_SIZE;
        let y = idx / plane;
        let rem = idx % plane;
        let z = rem / CHUNK_SIZE;
        let x = rem % CHUNK_SIZE;
        (x, y, z)
    }

    fn advance_chunkgen_apply_followup_task_step(
        &mut self,
        task: &mut ChunkGenApplyFollowUpTask,
        outer_started: Instant,
        limits: ChunkGenApplyFollowupLimits,
    ) -> bool {
        if !self.chunkgen_apply_followup_has_current_state(task) {
            task.phase = ChunkGenApplyFollowUpPhase::Finish;
            return true;
        }

        loop {
            let phase_started = Instant::now();
            match task.phase {
                ChunkGenApplyFollowUpPhase::BoundaryDirty => {
                    let neighbors = Self::neighbor_chunk_coords(task.coord);
                    let mut processed = 0usize;
                    while task.neighbor_cursor < neighbors.len()
                        && processed < limits.neighbors_per_step
                    {
                        let neighbor = neighbors[task.neighbor_cursor];
                        task.neighbor_cursor += 1;
                        processed += 1;
                        if self.world.get_chunk(neighbor).is_some()
                            && self.chunk_meshes.contains_key(&neighbor)
                        {
                            self.mark_chunk_boundary_dirty(neighbor);
                            self.frame_perf.chunkgen_apply_boundary_dirty_marks += 1;
                        }
                        if self.full_solid_mesh_skip_set.contains(&neighbor) {
                            self.mark_chunk_boundary_dirty(neighbor);
                            self.frame_perf.chunkgen_apply_boundary_dirty_marks += 1;
                        }
                        if outer_started.elapsed().as_secs_f32() * 1000.0 >= limits.budget_ms
                            || phase_started.elapsed().as_secs_f32() * 1000.0
                                >= limits.phase_budget_ms
                        {
                            return false;
                        }
                    }
                    if task.neighbor_cursor < neighbors.len() {
                        return false;
                    }
                    task.phase = ChunkGenApplyFollowUpPhase::Lighting;
                }
                ChunkGenApplyFollowUpPhase::Lighting => {
                    if task.removed_chunk {
                        self.enqueue_removed_lighting_batches(&[task.coord], "chunk_stream_remove");
                    } else if self.should_defer_startup_chunk_added_lighting(task.coord) {
                        self.startup_deferred_chunk_added_lighting_coords
                            .insert(task.coord);
                        perf_trace::instant_event(
                            "lighting.startup_deferred",
                            &format!(
                                "coord=({}, {}, {}) deferred_pending={} startup_active=1",
                                task.coord.x,
                                task.coord.y,
                                task.coord.z,
                                self.startup_deferred_chunk_added_lighting_coords.len()
                            ),
                        );
                    } else {
                        self.enqueue_chunk_stream_added_lighting(&[task.coord], "chunk_stream_add");
                    }
                    task.phase = ChunkGenApplyFollowUpPhase::Fluids;
                }
                ChunkGenApplyFollowUpPhase::Fluids => {
                    if task.removed_chunk || !self.is_sim_column_active(task.coord.x, task.coord.z)
                    {
                        task.phase = ChunkGenApplyFollowUpPhase::Finish;
                        continue;
                    }
                    if task.fluid_bootstrap_cursor < task.fluid_bootstrap.len() {
                        let end = (task.fluid_bootstrap_cursor
                            + CHUNKGEN_APPLY_FOLLOWUP_FLUID_BOOTSTRAP_BATCH)
                            .min(task.fluid_bootstrap.len());
                        let cells = &task.fluid_bootstrap[task.fluid_bootstrap_cursor..end];
                        let _ = self.with_fluids_system(|fluids| {
                            for cell in cells {
                                let delay = fluids.tick_rate_for_kind(cell.kind);
                                fluids.schedule_fluid_update(cell.pos, cell.kind, delay);
                            }
                        });
                        task.fluid_bootstrap_cursor = end;
                        return false;
                    }
                    let total_voxels = (CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE) as usize;
                    if task.needs_chunk_fluid_scan && task.fluid_scan_cursor < total_voxels {
                        let mut to_schedule = Vec::<(IVec3, FluidKind)>::with_capacity(
                            CHUNKGEN_APPLY_FOLLOWUP_FLUID_SCAN_BATCH,
                        );
                        if let Some(chunk) = self.world.get_chunk(task.coord) {
                            let chunk_x0 = task.coord.x * CHUNK_SIZE;
                            let chunk_y0 = task.coord.y * CHUNK_SIZE;
                            let chunk_z0 = task.coord.z * CHUNK_SIZE;
                            let end = (task.fluid_scan_cursor
                                + CHUNKGEN_APPLY_FOLLOWUP_FLUID_SCAN_BATCH)
                                .min(total_voxels);
                            while task.fluid_scan_cursor < end {
                                let idx = task.fluid_scan_cursor;
                                task.fluid_scan_cursor += 1;
                                let (local_x, local_y, local_z) =
                                    Self::chunk_local_coords_from_index(idx);
                                let block = chunk.get(local_x, local_y, local_z);
                                let Some(kind) = fluid_kind(block) else {
                                    continue;
                                };
                                let data = chunk.get_data(local_x, local_y, local_z);
                                if fluid_data_is_worldgen_static(data) {
                                    continue;
                                }
                                let pos = IVec3::new(
                                    chunk_x0 + local_x,
                                    chunk_y0 + local_y,
                                    chunk_z0 + local_z,
                                );
                                let is_source = fluid_data_is_source_origin(data);
                                if !Self::generated_fluid_cell_needs_bootstrap(
                                    &self.world,
                                    pos,
                                    kind,
                                    is_source,
                                ) {
                                    continue;
                                }
                                to_schedule.push((pos, kind));
                            }
                        } else {
                            task.fluid_scan_cursor = total_voxels;
                        }
                        if !to_schedule.is_empty() {
                            let _ = self.with_fluids_system(|fluids| {
                                for (pos, kind) in to_schedule {
                                    let delay = fluids.tick_rate_for_kind(kind);
                                    fluids.schedule_fluid_update(pos, kind, delay);
                                }
                            });
                        }
                        return false;
                    }
                    task.phase = ChunkGenApplyFollowUpPhase::Finish;
                }
                ChunkGenApplyFollowUpPhase::Finish => return true,
            }
        }
    }

    fn pump_chunkgen_apply_followups(&mut self) -> usize {
        let _trace_scope = perf_trace::scope("state.pump_chunkgen_apply_followups");
        let started = Instant::now();
        let limits = self.chunkgen_apply_followup_limits();
        let backlog_before = self.pending_chunkgen_apply_followups.len();
        self.frame_perf.chunkgen_apply_followup_backlog_before = self
            .frame_perf
            .chunkgen_apply_followup_backlog_before
            .max(backlog_before);
        let mut steps = 0usize;

        while steps < limits.steps_per_pump
            && !self.pending_chunkgen_apply_followups.is_empty()
            && started.elapsed().as_secs_f32() * 1000.0 < limits.budget_ms
        {
            let Some(mut task) = self.pending_chunkgen_apply_followups.pop_front() else {
                break;
            };
            let total_voxels = (CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE) as usize;
            let phase_before = task.phase.label();
            let step_started = Instant::now();
            let complete =
                self.advance_chunkgen_apply_followup_task_step(&mut task, started, limits);
            let step_ms = step_started.elapsed().as_secs_f32() * 1000.0;
            steps += 1;
            self.frame_perf.chunkgen_apply_followup_steps += 1;
            self.frame_perf.chunkgen_apply_followup_max_step_ms = self
                .frame_perf
                .chunkgen_apply_followup_max_step_ms
                .max(step_ms);
            perf_trace::record_duration_ms(
                "chunkgen.apply_followup_step",
                step_ms,
                &format!(
                    "coord=({}, {}, {}) removed={} phase={} next_phase={} boundary_cursor={} fluid_bootstrap={}/{} fluid_scan={}/{} complete={} latency_ms={:.2} backlog={}",
                    task.coord.x,
                    task.coord.y,
                    task.coord.z,
                    task.removed_chunk as u8,
                    phase_before,
                    task.phase.label(),
                    task.neighbor_cursor,
                    task.fluid_bootstrap_cursor,
                    task.fluid_bootstrap.len(),
                    task.fluid_scan_cursor,
                    total_voxels,
                    complete as u8,
                    task.enqueued_at.elapsed().as_secs_f32() * 1000.0,
                    self.pending_chunkgen_apply_followups.len() + usize::from(!complete)
                ),
            );
            if complete {
                self.frame_perf.chunkgen_apply_followup_completed += 1;
            } else {
                self.pending_chunkgen_apply_followups.push_back(task);
            }
        }

        let total_ms = started.elapsed().as_secs_f32() * 1000.0;
        self.frame_perf.chunkgen_apply_followup_ms += total_ms;
        self.frame_perf.chunkgen_apply_followup_backlog_after = self
            .frame_perf
            .chunkgen_apply_followup_backlog_after
            .max(self.pending_chunkgen_apply_followups.len());
        perf_trace::record_duration_ms(
            "chunkgen.apply_followups",
            total_ms,
            &format!(
                "steps={} backlog={}=>{} lighting_q={} mesh_q={} saved_load_q={} budget_ms={:.2} step_max_ms={:.2}",
                steps,
                backlog_before,
                self.pending_chunkgen_apply_followups.len(),
                self.chunkgen_apply_lighting_backlog_len(),
                self.chunkgen_apply_mesh_backlog_len(),
                self.saved_load_apply_backlog_len(),
                limits.budget_ms,
                self.frame_perf.chunkgen_apply_followup_max_step_ms
            ),
        );
        steps
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
                !self.chunk_is_generation_satisfied(coord)
            }) {
                let preset = self.worldgen_preset;
                for y in min_chunk_y..=max_chunk_y {
                    let coord = ChunkCoord { x: cx, y, z: cz };
                    if self.chunk_is_generation_satisfied(coord) {
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

    fn pump_chunkgen_results(&mut self) -> usize {
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

        let drain_limits = self.chunkgen_apply_drain_limits();
        let mut drained = 0usize;
        let mut stale_discarded = 0usize;
        let mut apply_prof = ChunkApplyProfileAgg::default();
        let mut budget_hit = false;
        self.frame_perf.stream_chunkgen_apply_budget_ms = self
            .frame_perf
            .stream_chunkgen_apply_budget_ms
            .max(drain_limits.budget_ms);
        let apply_loop_started = Instant::now();
        while drained < STREAM_STAGE_LOOP_SAFETY_CAP && drained < drain_limits.max_results {
            if drain_limits.budget_ms <= 0.0 {
                break;
            }
            if drained > 0
                && apply_loop_started.elapsed().as_secs_f32() * 1000.0 >= drain_limits.budget_ms
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
                match result.request_kind {
                    ChunkGenRequestKind::FullChunk => {
                        self.chunkgen_inflight.remove(&result.coord);
                    }
                    ChunkGenRequestKind::WarmupColumnOnly => {
                        if let Some(task) = self.world_generation_task.as_mut() {
                            task.startup_plan.columns_inflight.remove(&ColumnCoord {
                                x: result.coord.x,
                                z: result.coord.z,
                            });
                        }
                    }
                    ChunkGenRequestKind::LodColumnOnly => {}
                }
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

            if matches!(
                result.request_kind,
                ChunkGenRequestKind::FullChunk | ChunkGenRequestKind::WarmupColumnOnly
            ) {
                self.mark_startup_column_generated(ColumnCoord {
                    x: result.coord.x,
                    z: result.coord.z,
                });
            }

            if matches!(result.request_kind, ChunkGenRequestKind::WarmupColumnOnly) {
                continue;
            }

            let Some(mut chunk) = result.chunk else {
                continue;
            };
            self.accumulate_worldgen_feature_debug_stats(result.worldgen_debug);

            let chunk_apply_started = Instant::now();
            let coord = result.coord;
            if matches!(result.request_kind, ChunkGenRequestKind::FullChunk) {
                self.chunkgen_inflight.remove(&result.coord);
            }
            let deferred_stats = self.apply_deferred_edits_to_chunk(coord, &mut chunk);
            let was_mesh_present = self.chunk_meshes.contains_key(&coord);
            self.frame_perf.chunkgen_chunks_applied += 1;
            let mut insert_ms = 0.0f32;
            let heightmap_ms = 0.0f32;
            let lighting_hooks_ms = 0.0f32;
            let mut mark_mesh_dirty_ms = 0.0f32;
            let fluid_bootstrap_ms = 0.0f32;
            let meshing_sync_ms = 0.0f32;
            let gpu_upload_sync_ms = 0.0f32;

            if chunk.is_empty() {
                self.frame_perf.chunkgen_empty_chunks_applied += 1;
                let insert_started = Instant::now();
                let removed_chunk = self.world.chunks.remove(&coord).is_some();
                self.chunk_origin.remove(&coord);
                self.full_solid_mesh_skip_set.remove(&coord);
                self.uploaded_final_lighting_chunks.remove(&coord);
                self.chunk_lighting_progress.remove(&coord);
                self.startup_deferred_chunk_added_lighting_coords
                    .remove(&coord);
                insert_ms += insert_started.elapsed().as_secs_f32() * 1000.0;
                if removed_chunk {
                    let mark_started = Instant::now();
                    self.mark_column_dirty_for_chunk(coord);
                    mark_mesh_dirty_ms += mark_started.elapsed().as_secs_f32() * 1000.0;
                }
                self.generated_empty_chunks.insert(coord);
                if removed_chunk {
                    let mark_started = Instant::now();
                    if was_mesh_present {
                        self.chunk_meshes.remove(&coord);
                    }
                    mark_mesh_dirty_ms += mark_started.elapsed().as_secs_f32() * 1000.0;
                    self.enqueue_chunkgen_apply_followup(ChunkGenApplyFollowUpTask::new(
                        coord,
                        true,
                        Vec::new(),
                        false,
                    ));
                }
                self.mark_startup_section_applied(coord);
            } else {
                let insert_started = Instant::now();
                self.generated_empty_chunks.remove(&coord);
                self.full_solid_mesh_skip_set.remove(&coord);
                self.seed_pending_lighting_for_chunk(coord, &mut chunk);
                self.world.chunks.insert(coord, chunk);
                self.chunk_origin.insert(coord, ChunkOrigin::Generated);
                insert_ms += insert_started.elapsed().as_secs_f32() * 1000.0;

                let mark_started = Instant::now();
                self.mark_column_dirty_for_chunk(coord);
                self.mark_chunk_dirty_with_reasons(coord, MeshDirtyReasons::GEOMETRY);
                mark_mesh_dirty_ms += mark_started.elapsed().as_secs_f32() * 1000.0;
                if self.should_defer_startup_chunk_added_lighting(coord) {
                    self.note_chunk_lighting_provisional(&[coord], "startup_chunk_apply");
                    self.note_chunk_lighting_visual_ready(&[coord], "startup_chunk_apply");
                    self.startup_deferred_chunk_added_lighting_coords
                        .insert(coord);
                }
                self.enqueue_chunkgen_apply_followup(ChunkGenApplyFollowUpTask::new(
                    coord,
                    false,
                    result.fluid_bootstrap,
                    deferred_stats.applied_fluid > 0,
                ));
                self.mark_startup_section_applied(coord);
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
        if drained >= STREAM_STAGE_LOOP_SAFETY_CAP
            || (drain_limits.max_results > 0 && drained >= drain_limits.max_results)
        {
            budget_hit = true;
        }
        if self.world_generation_task.is_some() && drained > 0 {
            self.refresh_worldgen_warmup_tracking();
        }
        let pure_apply_ms = apply_loop_started.elapsed().as_secs_f32() * 1000.0;
        self.frame_perf.stream_chunkgen_apply_used_ms += pure_apply_ms;
        self.frame_perf.chunkgen_apply_pure_ms += pure_apply_ms;
        self.frame_perf.chunkgen_results_drained += drained;
        self.frame_perf.chunkgen_results_backlog_after = self
            .frame_perf
            .chunkgen_results_backlog_after
            .max(self.chunkgen_results.len());
        self.pump_pending_lod_mesh_uploads();
        let dur_ms = started.elapsed().as_secs_f32() * 1000.0;
        self.frame_perf.pump_chunkgen_results_ms += dur_ms;
        perf_trace::record_duration_ms(
            "state.pump_chunkgen_results",
            dur_ms,
            &format!(
                "drained={} chunk_applied={} lod_results={} stale_discarded={} budget_hit={} pure_ms={:.2} apply_avg_ms={:.2} apply_max_ms={:.2} slowest={:?} drain_cap={} cpu_ms={:.2} lighting_backlog={} mesh_backlog={} followup_backlog={} saved_load_backlog={} followup_q={} overshoot={} lod_upload_q={}",
                drained,
                self.frame_perf.chunkgen_chunks_applied,
                self.frame_perf.chunkgen_lod_results_applied,
                stale_discarded,
                budget_hit,
                pure_apply_ms,
                if apply_prof.applied_chunks > 0 {
                    apply_prof.total_chunk_ms / apply_prof.applied_chunks as f32
                } else {
                    0.0
                },
                apply_prof.max_chunk_ms,
                apply_prof.slowest_coord,
                drain_limits.max_results,
                drain_limits.cpu_ms,
                drain_limits.lighting_backlog,
                drain_limits.mesh_backlog,
                drain_limits.followup_backlog,
                drain_limits.saved_load_backlog,
                self.pending_chunkgen_apply_followups.len(),
                drain_limits.overshoot as u8,
                self.pending_lod_mesh_uploads.len()
            ),
        );
        if dur_ms >= CHUNKGEN_APPLY_LOG_THRESHOLD_MS
            || apply_prof.max_chunk_ms >= CHUNKGEN_APPLY_LOG_THRESHOLD_MS
        {
            crate::console_error!(
                "[perf][chunkgen_apply] frame_ms={:.1} drained={} applied={} stale={} budget_hit={} pure_ms={:.1} avg_ms={:.2} max_ms={:.2} slowest={:?} sub(insert={:.1} heightmap={:.1} lighting_hooks={:.1} mesh_dirty={:.1} fluid_bootstrap={:.1} meshing_sync={:.1} gpu_sync={:.1}) followup(q={},queued={},light_add={},light_remove={},fluid_cells={},fluid_scans={}) pressure(cpu_ms={:.2},drain_cap={},lighting_backlog={},mesh_backlog={},saved_load_q={},overshoot={}) inflight={} job_q={} result_q={}",
                dur_ms,
                drained,
                apply_prof.applied_chunks,
                stale_discarded,
                budget_hit,
                pure_apply_ms,
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
                self.pending_chunkgen_apply_followups.len(),
                self.frame_perf.chunkgen_apply_followup_tasks_queued,
                self.frame_perf.chunkgen_apply_lighting_add_queued,
                self.frame_perf.chunkgen_apply_lighting_remove_queued,
                self.frame_perf.chunkgen_apply_fluid_bootstrap_cells_queued,
                self.frame_perf.chunkgen_apply_deferred_fluid_scans_queued,
                drain_limits.cpu_ms,
                drain_limits.max_results,
                drain_limits.lighting_backlog,
                drain_limits.mesh_backlog,
                drain_limits.saved_load_backlog,
                drain_limits.overshoot as u8,
                self.chunkgen_inflight.len(),
                self.chunkgen_jobs.len(),
                self.chunkgen_results.len(),
            );
        }
        drained
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
                task.startup_plan.radius_chunks,
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
                self.uploaded_final_lighting_chunks.remove(&coord);
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
                    task.startup_plan.radius_chunks,
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
        let mut removed_chunk_coords = Vec::new();

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
            self.uploaded_final_lighting_chunks.remove(&coord);
            self.full_solid_mesh_skip_set.remove(&coord);
            self.generated_empty_chunks.remove(&coord);
            self.chunk_origin.remove(&coord);
            self.chunkgen_inflight.remove(&coord);
            self.inflight.remove(&coord);
            self.dirty_chunk_set.remove(&coord);
            self.mesh_schedule.remove(&coord);
            self.fluid_mesh_coalesce.remove(&coord);
            self.fluid_dirty_due_to_fluids.remove(&coord);
            self.saved_revision.remove(&coord);

            if removed_chunk || had_mesh {
                for neighbor in Self::neighbor_chunk_coords(coord) {
                    self.mark_chunk_boundary_dirty(neighbor);
                }
            }
            if removed_chunk {
                removed_chunks += 1;
                removed_any_loaded_chunk = true;
                removed_chunk_coords.push(coord);
            }
        }

        if removed_any_loaded_chunk {
            self.run_incremental_chunk_removed_lighting(&removed_chunk_coords, "stream_unload");
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
        max_columns: usize,
    ) {
        if self.radius_offsets_cache.is_empty() || max_columns == 0 {
            return;
        }

        let min_chunk_y = self.world.min_chunk_y();
        let max_chunk_y = self.world.max_chunk_y();

        for idx in 0..max_columns.min(self.radius_offsets_cache.len()) {
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

    fn reprioritize_mesh_upload_queue(&mut self, camera: &Camera, frustum: &Frustum) {
        if self.mesh_upload_queue.len() < 2 {
            return;
        }
        let (center_chunk_x, center_chunk_y, center_chunk_z) = Self::camera_chunk_coords(camera);
        let forward_xz = Self::camera_forward_xz(camera);
        let now = Instant::now();

        let drained: Vec<QueuedMeshUpload> = self.mesh_upload_queue.drain(..).collect();
        let mut staged = Vec::with_capacity(drained.len());
        for queued in drained {
            let key = self.mesh_upload_priority_key_for_result(
                &queued,
                center_chunk_x,
                center_chunk_y,
                center_chunk_z,
                frustum,
                forward_xz,
                now,
            );
            staged.push((queued, key));
        }
        staged.sort_by(|a, b| {
            a.1.cmp_ascending(&b.1)
                .then_with(|| a.0.result.coord.x.cmp(&b.0.result.coord.x))
                .then_with(|| a.0.result.coord.y.cmp(&b.0.result.coord.y))
                .then_with(|| a.0.result.coord.z.cmp(&b.0.result.coord.z))
        });
        self.mesh_upload_queue
            .extend(staged.into_iter().map(|(queued, _)| queued));
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
                    task.startup_plan.radius_chunks,
                )
            } else {
                let (x, y, z) = Self::camera_chunk_coords(camera);
                (x, y, z, self.near_mesh_radius_chunks())
            };
        let center = (chunk_x, chunk_y, chunk_z);
        let center_changed = self.last_stream_center != Some(center);
        let radius_changed = self.last_stream_radius != radius;
        if center_changed || radius_changed {
            self.invalidate_stream_planning_cache(if radius_changed {
                "stream_center_or_radius_changed"
            } else {
                "stream_center_changed"
            });
            if radius_changed {
                self.rebuild_radius_offsets_cache(radius);
            }
            self.saved_load_request_cursor = 0;
            self.queue_missing_meshes_in_radius(
                chunk_x,
                chunk_y,
                chunk_z,
                STREAM_MISSING_MESH_SCAN_COLUMNS_ON_CENTER_CHANGE,
            );
            self.last_stream_center = Some(center);
            self.last_stream_radius = radius;
        }
        let ensure_started = Instant::now();
        self.ensure_chunks_in_radius(chunk_x, chunk_y, chunk_z, radius, Some(camera));
        let ensure_ms = ensure_started.elapsed().as_secs_f32() * 1000.0;
        let saved_started = Instant::now();
        let saved_load_queued = if self.saved_loads_suspended() {
            if self.world_generation_task.is_some() {
                self.log_startup_saved_load_bypass_once("enqueue_chunks_within_view");
            }
            0
        } else {
            self.queue_saved_column_load_requests_for_radius(
                chunk_x,
                chunk_z,
                SAVED_LOAD_REQUESTS_PER_FRAME,
            )
        };
        let saved_load_ms = saved_started.elapsed().as_secs_f32() * 1000.0;
        if !self.saved_loads_suspended() {
            self.report_saved_column_request_rate_if_due();
        }
        let mut lod_ms = 0.0f32;
        if self.far_lod_enabled && self.world_generation_task.is_none() {
            let lod_started = Instant::now();
            let (lod_cx, lod_cz) =
                self.stable_lod_stream_center_from_camera(camera, chunk_x, chunk_z);
            self.update_lod_streaming(lod_cx, lod_cz);
            lod_ms = lod_started.elapsed().as_secs_f32() * 1000.0;
        }
        let dur_ms = started.elapsed().as_secs_f32() * 1000.0;
        self.frame_perf.enqueue_chunks_within_view_ms += dur_ms;
        perf_trace::record_duration_ms(
            "state.enqueue_chunks_within_view",
            dur_ms,
            &format!(
                "center_changed={} radius_changed={} ensure_ms={:.2} saved_ms={:.2} lod_ms={:.2} pending={} heap={} heap_rebuild={}@{:.2}ms reprioritized={} frustum_cache[h={},m={}] horizon_cache[h={},m={}] frustum_ms={:.2} horizon_ms={:.2} saved_load_q={} far_lod={} inflight_chunkgen={} inflight_mesh={} saved_load_inflight={} saved_load_q_backlog={} saved_load_ack_backlog={}",
                center_changed,
                radius_changed,
                ensure_ms,
                saved_load_ms,
                lod_ms,
                self.chunkgen_pending_set.len(),
                self.chunkgen_pending_heap.len(),
                self.frame_perf.stream_scheduler_heap_rebuild_items,
                self.frame_perf.stream_scheduler_heap_rebuild_ms,
                self.frame_perf.stream_scheduler_reprioritized_items,
                self.frame_perf.stream_scheduler_frustum_cache_hits,
                self.frame_perf.stream_scheduler_frustum_cache_misses,
                self.frame_perf.stream_scheduler_horizon_cache_hits,
                self.frame_perf.stream_scheduler_horizon_cache_misses,
                self.frame_perf.stream_scheduler_frustum_refresh_ms,
                self.frame_perf.stream_scheduler_horizon_refresh_ms,
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
        self.far_lod_requests_per_frame = self.render_distance.far_lod_requests_per_frame();
        self.far_lod_gpu_uploads_per_frame = self.render_distance.far_lod_gpu_uploads_per_frame();
        if self.view_distance_chunks <= self.near_mesh_radius_chunks() {
            self.lod_meshes.clear();
            self.pending_lod_mesh_uploads.clear();
            self.lod_chunkgen_inflight.clear();
            self.last_lod_stream_center = None;
            self.lod_ring_offsets_cache.clear();
            self.lod_request_cursor = 0;
        }
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
        self.mesh_schedule.retain(|coord, _| {
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
        self.sim_system_last_frame_ms.push(0.0);
        self.sim_system_last_frame_ticks.push(0);
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
            crate::console_error!("[ticking_areas] failed to persist after create: {err}");
        }
        self.last_sim_center = None;
        area.id
    }

    pub(crate) fn delete_nearest_ticking_area(&mut self, cx: i32, cz: i32) -> Option<u32> {
        let removed = self
            .world
            .remove_nearest_ticking_area(glam::IVec2::new(cx, cz))?;
        if let Err(err) = self.save_ticking_areas() {
            crate::console_error!("[ticking_areas] failed to persist after delete: {err}");
        }
        self.last_sim_center = None;
        Some(removed.id)
    }

    fn for_each_ticking_area_column(&self, area: &TickingArea, mut f: impl FnMut(ColumnCoord)) {
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
            let coord = ChunkCoord {
                x: cx,
                y: cy,
                z: cz,
            };
            if self.chunk_is_generation_satisfied(coord) {
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

    pub(crate) fn is_world_startup_loading(&self) -> bool {
        self.world_generation_task.is_some()
    }

    fn startup_spawn_chunk_center(&self) -> (i32, i32) {
        // Spawn policy is currently origin-centered; both startup warmup and final spawn
        // settlement use the same focus so the first playable frame lands in prepared terrain.
        (0, 0)
    }

    fn begin_world_startup_task(
        &mut self,
        kind: WorldGenerationKind,
        warmup_center_x: i32,
        warmup_center_z: i32,
    ) {
        if self.world_generation_task.is_some() {
            return;
        }

        let center_chunk_y = (self.world.sea_level() + 16).div_euclid(CHUNK_SIZE);
        let warmup_columns = self.build_worldgen_warmup_columns(warmup_center_x, warmup_center_z);
        let warmup_coords =
            self.build_worldgen_warmup_coords(warmup_center_x, center_chunk_y, warmup_center_z);
        let startup_radius = self.startup_radius_chunks();
        let startup_column_order =
            self.startup_column_order(warmup_center_x, warmup_center_z, startup_radius);
        let warmup_target = warmup_coords.len().max(1);
        self.world_generation_task = Some(WorldGenerationTask {
            kind,
            warmup_center_x,
            warmup_center_z,
            startup_plan: StartupPlan {
                radius_chunks: startup_radius,
                target_columns: warmup_columns,
                target_sections: warmup_coords,
                column_request_order: startup_column_order,
                column_request_cursor: 0,
                columns_inflight: HashSet::new(),
                generated_columns: HashSet::new(),
                applied_sections: HashSet::new(),
                meshed_sections: HashSet::new(),
                uploaded_sections: HashSet::new(),
            },
            pending_progress: None,
            last_reported_phase: String::new(),
            last_reported_progress_permille: u16::MAX,
            last_reported_stats: WorldLoadStageStats::default(),
            started_at: Instant::now(),
        });
        self.refresh_worldgen_warmup_tracking();

        self.ensure_chunks_in_radius(
            warmup_center_x,
            center_chunk_y,
            warmup_center_z,
            startup_radius,
            None,
        );
        self.last_stream_center = Some((warmup_center_x, center_chunk_y, warmup_center_z));
        self.last_stream_radius = startup_radius;
        crate::console_info!(
            "{}: started startup pipeline (startup_radius={}, target_columns={}, target_sections={}, save_root={})",
            kind.overlay_title(),
            startup_radius,
            self.world_generation_task
                .as_ref()
                .map(|task| task.startup_plan.target_columns.len())
                .unwrap_or(0),
            warmup_target,
            self.save_root.display()
        );
    }

    pub(crate) fn begin_startup_loading(&mut self) {
        let (warmup_center_x, warmup_center_z) = self.startup_spawn_chunk_center();
        self.begin_world_startup_task(
            WorldGenerationKind::StartupLoad,
            warmup_center_x,
            warmup_center_z,
        );
    }

    pub(crate) fn begin_new_world(&mut self, seed: u64, preset: WorldgenPreset) {
        if self.world_generation_task.is_some() {
            return;
        }
        self.saved_loads_paused = true;
        self.startup_saved_load_bypass_logged = false;
        if let Err(err) = self.storage_writer.flush() {
            crate::console_error!(
                "[storage][save] flush before new level generation failed: {err}"
            );
        }
        self.pump_storage_save_results();
        self.drain_saved_load_pipeline_for_startup_transition();
        self.save_epoch = self.save_epoch.wrapping_add(1);

        let previous_save_root = self.save_root.clone();
        let new_save_root = Self::allocate_new_world_save_root(seed);
        self.switch_save_root(new_save_root.clone());
        crate::console_info!(
            "[storage][saved_load] startup namespace reset {} -> {}; saved-load bypass armed",
            previous_save_root.display(),
            new_save_root.display()
        );

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
        let world_meta_path = self.world_meta_path();
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
        let world_manifest_path = self.world_manifest_path();
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
            crate::console_error!(
                "failed to write {} after selecting new world seed/preset: {}",
                self.ticking_areas_path().display(),
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
        self.mesh_schedule.clear();
        self.chunkgen_inflight.clear();
        self.lod_chunkgen_inflight.clear();
        self.generated_empty_chunks.clear();
        self.chunk_origin.clear();
        self.deferred_block_edits.clear();
        self.deferred_block_edits_total = 0;
        self.mesh_upload_queue.clear();
        while self.mesh_results.try_recv().is_ok() {}
        while self.chunkgen_results.try_recv().is_ok() {}
        while self.lighting_results.try_recv().is_ok() {}
        self.discard_ready_saved_load_results();
        self.clear_saved_load_apply_backlog();
        self.highlight_mesh = None;
        self.outline_target = None;
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
        self.worldgen_feature_debug_counts = WorldgenFeatureDebugCounts::default();
        self.chunk_lighting_progress.clear();
        self.uploaded_final_lighting_chunks.clear();
        self.pending_chunkgen_apply_followups.clear();
        self.pending_removed_lighting_batches.clear();
        self.pending_chunk_added_lighting_jobs.clear();
        self.pending_chunk_added_lighting_coords.clear();
        self.startup_deferred_chunk_added_lighting_coords.clear();
        self.next_chunk_added_lighting_batch_id = 1;
        self.frame_index = 0;
        self.frustum_culled_chunks_total = 0;
        self.meshes_skipped_due_to_frustum = 0;
        self.fluid_mesh_coalesce.clear();
        self.fluid_dirty_due_to_fluids.clear();
        self.stream_last_camera_pos = None;
        self.stream_camera_speed_blocks_per_sec = 0.0;
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
        self.chunkgen_pending_priority.clear();
        self.chunkgen_pending_round_robin.clear();
        self.chunkgen_pending_heap.clear();
        self.chunkgen_pending_scan_cursor = 0;
        self.chunkgen_pending_refresh_cursor = 0;
        self.chunkgen_heap_valid_for_center = None;
        self.chunkgen_heap_valid_for_radius = -1;
        self.chunkgen_heap_last_rebuild_frame = 0;
        self.chunkgen_priority_epoch = 1;
        self.chunkgen_priority_context_stamp = None;
        self.chunkgen_stream_epoch = self.chunkgen_stream_epoch.wrapping_add(1);
        self.stream_horizon_has_data = false;
        self.stream_horizon_built_frame = u64::MAX;
        self.stream_horizon_cache_key = None;
        self.stream_horizon_mesh_count = 0;
        self.stream_horizon_seen_columns.clear();
        self.invalidate_stream_planning_cache("begin_new_world");
        self.stream_scheduler_cached_frame = u64::MAX;
        self.stream_scheduler_cached_frustum = None;
        self.stream_scheduler_cached_forward_xz = None;
        self.stream_scheduler_cached_forward_bucket = 0;
        self.lod_cached_near_radius = -1;
        self.lod_cached_far_radius = -1;
        self.lighting_job_inflight = false;
        self.lighting_request_pending = false;
        self.lighting_rebuild_not_before = None;
        self.pending_chunkgen_apply_followups.clear();
        self.pending_removed_lighting_batches.clear();
        self.pending_chunk_added_lighting_jobs.clear();
        self.pending_chunk_added_lighting_coords.clear();
        self.startup_deferred_chunk_added_lighting_coords.clear();
        self.pending_lighting_dirty_chunks.clear();
        self.pending_lighting_dirty_set.clear();
        self.saved_revision.clear();
        self.saved_column_missing_cache.clear();
        self.saved_column_loads_inflight.clear();
        self.saved_loads_paused = true;
        self.startup_saved_load_bypass_logged = false;
        self.saved_load_request_cursor = 0;
        self.saved_load_requests_in_window = 0;
        self.saved_load_requests_window_started = Instant::now();
        self.perf_summary_window.reset();
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

        let (warmup_center_x, warmup_center_z) = self.startup_spawn_chunk_center();
        self.begin_world_startup_task(
            WorldGenerationKind::NewWorld,
            warmup_center_x,
            warmup_center_z,
        );
        crate::console_info!(
            "new level generation: seed={} preset={:?} warmup_center=({}, {})",
            seed,
            preset,
            warmup_center_x,
            warmup_center_z
        );
    }

    pub(crate) fn poll_worldgen_progress(&mut self) -> Option<WorldGenProgress> {
        if let Some(task) = self.world_generation_task.as_ref() {
            let center_chunk_y = (self.world.sea_level() + 16).div_euclid(CHUNK_SIZE);
            self.ensure_chunks_in_radius(
                task.warmup_center_x,
                center_chunk_y,
                task.warmup_center_z,
                task.startup_plan.radius_chunks,
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

    pub(crate) fn try_finish_world_startup_loading(&mut self, camera: &Camera) -> Option<Vec3> {
        if let Some(task) = self.world_generation_task.as_ref() {
            let center_chunk_y = (self.world.sea_level() + 16).div_euclid(CHUNK_SIZE);
            self.ensure_chunks_in_radius(
                task.warmup_center_x,
                center_chunk_y,
                task.warmup_center_z,
                task.startup_plan.radius_chunks,
                None,
            );
        }
        self.pump_chunkgen_results();
        self.pump_storage_load_results();
        self.pump_lighting_results();
        self.refresh_worldgen_warmup_tracking();
        let task = self.world_generation_task.as_ref()?;
        if task.startup_plan.generated_columns.len() < task.startup_plan.target_columns.len()
            || task.startup_plan.applied_sections.len() < task.startup_plan.target_sections.len()
            || task.startup_plan.meshed_sections.len() < task.startup_plan.target_sections.len()
            || task.startup_plan.uploaded_sections.len() < task.startup_plan.target_sections.len()
        {
            return None;
        }

        crate::console_info!(
            "[perf][startup] startup area ready (columns={} sections_applied={} sections_meshed={} sections_uploaded={} target_columns={} target_sections={}) after {:.2}ms",
            task.startup_plan.generated_columns.len(),
            task.startup_plan.applied_sections.len(),
            task.startup_plan.meshed_sections.len(),
            task.startup_plan.uploaded_sections.len(),
            task.startup_plan.target_columns.len(),
            task.startup_plan.target_sections.len(),
            task.started_at.elapsed().as_secs_f64() * 1000.0
        );
        let spawn_x = task.warmup_center_x * CHUNK_SIZE;
        let spawn_z = task.warmup_center_z * CHUNK_SIZE;
        let terrain_y = self.world.terrain_height_at(spawn_x, spawn_z) as f32;
        let spawn = Vec3::new(spawn_x as f32 + 0.5, terrain_y + 2.0, spawn_z as f32 + 0.5);

        self.world_generation_task = None;
        self.saved_loads_paused = false;
        self.startup_saved_load_bypass_logged = false;
        self.enqueue_chunks_within_view(camera);
        Some(spawn)
    }

    pub(crate) fn save_world(&mut self) {
        let started = Instant::now();
        if let Err(err) = self.save_ticking_areas() {
            crate::console_error!("world save: failed to save ticking areas: {err}");
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
                    crate::console_error!(
                        "world save failed after queuing {} dirty columns: {}",
                        queued_total,
                        err
                    );
                    return;
                }
                self.pump_storage_save_results();
            } else if queued_total == 0 {
                break;
            }
            spins += 1;
            if spins > 4096 {
                crate::console_error!(
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
                crate::console_info!(
                    "world save: flushed {} dirty loaded columns in {:.2}ms",
                    queued_total,
                    started.elapsed().as_secs_f64() * 1000.0
                );
            }
            Err(err) => {
                self.saved_revision.clear();
                crate::console_error!(
                    "world save failed after queuing {} dirty columns: {}",
                    queued_total,
                    err
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

    fn mesh_state_from_reasons(reasons: MeshDirtyReasons) -> ChunkMeshState {
        if reasons.is_empty() {
            ChunkMeshState::CleanReady
        } else if reasons == MeshDirtyReasons::GEOMETRY
            || reasons == (MeshDirtyReasons::GEOMETRY | MeshDirtyReasons::DEBUG)
            || reasons == MeshDirtyReasons::DEBUG
        {
            ChunkMeshState::DirtyGeometry
        } else if reasons == MeshDirtyReasons::LIGHTING {
            ChunkMeshState::DirtyLighting
        } else if reasons == MeshDirtyReasons::BOUNDARY {
            ChunkMeshState::DirtyBoundary
        } else {
            ChunkMeshState::DirtyMixed
        }
    }

    fn note_mesh_dirty_reason_metrics(&mut self, reasons: MeshDirtyReasons) {
        if reasons.contains(MeshDirtyReasons::GEOMETRY) || reasons.contains(MeshDirtyReasons::DEBUG)
        {
            self.frame_perf.mesh_dirty_geometry_events += 1;
        }
        if reasons.contains(MeshDirtyReasons::LIGHTING) {
            self.frame_perf.mesh_dirty_lighting_events += 1;
        }
        if reasons.contains(MeshDirtyReasons::BOUNDARY) {
            self.frame_perf.mesh_dirty_boundary_events += 1;
        }
    }

    fn mesh_should_await_lighting_finalization(
        &self,
        coord: ChunkCoord,
        reasons: MeshDirtyReasons,
    ) -> bool {
        if !reasons.is_lighting_only() || !self.chunk_meshes.contains_key(&coord) {
            return false;
        }
        self.chunk_lighting_progress
            .get(&coord)
            .is_some_and(|progress| progress.state != ChunkLightingReadiness::Finalized)
    }

    fn enqueue_dirty_chunk_coord(&mut self, coord: ChunkCoord) {
        if self.dirty_chunk_set.insert(coord) {
            self.dirty_chunk_queue.push_back(coord);
        }
    }

    fn requeue_existing_dirty_chunk(&mut self, coord: ChunkCoord) {
        if self
            .mesh_schedule
            .get(&coord)
            .is_some_and(|entry| !entry.queued_reasons.is_empty())
        {
            self.enqueue_dirty_chunk_coord(coord);
        }
    }

    fn mark_chunk_dirty_with_reasons(&mut self, coord: ChunkCoord, reasons: MeshDirtyReasons) {
        if reasons.is_empty() {
            return;
        }
        self.note_mesh_dirty_reason_metrics(reasons);

        let should_await_lighting = self.mesh_should_await_lighting_finalization(coord, reasons);
        let mut enqueue_now = false;
        let mut coalesced = false;
        let mut avoided = false;
        {
            let entry = self.mesh_schedule.entry(coord).or_default();
            match entry.state {
                ChunkMeshState::CleanReady => {
                    if should_await_lighting {
                        entry.pending_reasons |= reasons;
                        entry.state = ChunkMeshState::AwaitingLightingFinalization;
                        avoided = true;
                    } else {
                        let before = entry.queued_reasons;
                        entry.queued_reasons |= reasons;
                        entry.state = Self::mesh_state_from_reasons(entry.queued_reasons);
                        enqueue_now = true;
                        coalesced = !before.is_empty();
                    }
                }
                ChunkMeshState::DirtyGeometry
                | ChunkMeshState::DirtyLighting
                | ChunkMeshState::DirtyBoundary
                | ChunkMeshState::DirtyMixed => {
                    let before = entry.queued_reasons;
                    entry.queued_reasons |= reasons;
                    entry.state = Self::mesh_state_from_reasons(entry.queued_reasons);
                    enqueue_now = true;
                    coalesced =
                        entry.queued_reasons != before || self.dirty_chunk_set.contains(&coord);
                }
                ChunkMeshState::InFlight => {
                    let before = entry.pending_reasons;
                    entry.pending_reasons |= reasons;
                    coalesced =
                        entry.pending_reasons != before || self.inflight.contains_key(&coord);
                    avoided = true;
                }
                ChunkMeshState::AwaitingLightingFinalization => {
                    let before = entry.pending_reasons;
                    entry.pending_reasons |= reasons;
                    if !should_await_lighting {
                        entry.queued_reasons |= entry.pending_reasons;
                        entry.pending_reasons = MeshDirtyReasons::NONE;
                        entry.state = Self::mesh_state_from_reasons(entry.queued_reasons);
                        enqueue_now = true;
                    } else {
                        entry.state = ChunkMeshState::AwaitingLightingFinalization;
                        avoided = true;
                    }
                    coalesced = entry.pending_reasons != before || avoided;
                }
            }
        }

        if coalesced {
            self.frame_perf.mesh_dirty_coalesced_events += 1;
        }
        if avoided {
            self.frame_perf.mesh_avoided_remeshes += 1;
        }
        if enqueue_now {
            self.enqueue_dirty_chunk_coord(coord);
        }
    }

    pub(crate) fn mark_chunk_dirty(&mut self, coord: ChunkCoord) {
        self.mark_chunk_dirty_with_reasons(coord, MeshDirtyReasons::GEOMETRY);
    }

    fn mark_chunk_lighting_dirty(&mut self, coord: ChunkCoord) {
        self.mark_chunk_dirty_with_reasons(coord, MeshDirtyReasons::LIGHTING);
    }

    fn mark_chunk_boundary_dirty(&mut self, coord: ChunkCoord) {
        self.mark_chunk_dirty_with_reasons(coord, MeshDirtyReasons::BOUNDARY);
    }

    fn mark_chunk_debug_dirty(&mut self, coord: ChunkCoord) {
        self.mark_chunk_dirty_with_reasons(coord, MeshDirtyReasons::DEBUG);
    }

    fn maybe_release_mesh_after_lighting_finalization(&mut self, coord: ChunkCoord) {
        let should_release = self.mesh_schedule.get(&coord).is_some_and(|entry| {
            entry.state == ChunkMeshState::AwaitingLightingFinalization
                && !entry.pending_reasons.is_empty()
        }) && !self
            .mesh_should_await_lighting_finalization(coord, MeshDirtyReasons::LIGHTING);
        if !should_release {
            return;
        }

        let queued_reasons = {
            let entry = self.mesh_schedule.entry(coord).or_default();
            entry.queued_reasons |= entry.pending_reasons;
            entry.pending_reasons = MeshDirtyReasons::NONE;
            entry.state = Self::mesh_state_from_reasons(entry.queued_reasons);
            entry.queued_reasons
        };
        if !queued_reasons.is_empty() {
            self.enqueue_dirty_chunk_coord(coord);
        }
    }

    fn begin_mesh_dispatch(&mut self, coord: ChunkCoord, revision: u64) -> MeshDirtyReasons {
        let reasons = {
            let entry = self.mesh_schedule.entry(coord).or_default();
            let reasons = if entry.queued_reasons.is_empty() {
                MeshDirtyReasons::GEOMETRY
            } else {
                entry.queued_reasons
            };
            entry.queued_reasons = MeshDirtyReasons::NONE;
            entry.inflight_reasons = reasons;
            entry.state = ChunkMeshState::InFlight;
            reasons
        };
        self.inflight.insert(coord, revision);
        if reasons.contains(MeshDirtyReasons::BOUNDARY) {
            self.frame_perf.mesh_boundary_dispatches += 1;
        }
        if reasons.contains(MeshDirtyReasons::LIGHTING) {
            self.frame_perf.mesh_lighting_dispatches += 1;
        }
        reasons
    }

    fn finish_mesh_dispatch(
        &mut self,
        coord: ChunkCoord,
        requeue_reasons: Option<MeshDirtyReasons>,
    ) {
        let mut next_reasons = {
            let entry = self.mesh_schedule.entry(coord).or_default();
            let mut next_reasons = requeue_reasons.unwrap_or(entry.pending_reasons);
            if requeue_reasons.is_some() {
                next_reasons |= entry.pending_reasons;
            }
            entry.pending_reasons = MeshDirtyReasons::NONE;
            entry.inflight_reasons = MeshDirtyReasons::NONE;
            next_reasons
        };
        let should_await_lighting = if next_reasons.is_empty() {
            false
        } else {
            self.mesh_should_await_lighting_finalization(coord, next_reasons)
        };

        let mut enqueue_now = false;
        {
            let entry = self.mesh_schedule.entry(coord).or_default();
            if next_reasons.is_empty() {
                entry.queued_reasons = MeshDirtyReasons::NONE;
                entry.state = ChunkMeshState::CleanReady;
            } else if should_await_lighting {
                entry.queued_reasons = MeshDirtyReasons::NONE;
                entry.pending_reasons = next_reasons;
                entry.state = ChunkMeshState::AwaitingLightingFinalization;
                self.frame_perf.mesh_avoided_remeshes += 1;
            } else {
                entry.queued_reasons |= next_reasons;
                next_reasons = entry.queued_reasons;
                entry.state = Self::mesh_state_from_reasons(next_reasons);
                enqueue_now = true;
            }
        }
        if enqueue_now {
            self.enqueue_dirty_chunk_coord(coord);
        }
    }

    fn mark_neighbors_dirty_if_meshed(&mut self, coord: ChunkCoord) {
        for neighbor in Self::neighbor_chunk_coords(coord) {
            if self.world.get_chunk(neighbor).is_some() && self.chunk_meshes.contains_key(&neighbor)
            {
                self.mark_chunk_boundary_dirty(neighbor);
            }
        }
    }

    fn mark_skipped_full_solid_neighbors_dirty(&mut self, coord: ChunkCoord) {
        for neighbor in Self::neighbor_chunk_coords(coord) {
            if self.full_solid_mesh_skip_set.contains(&neighbor) {
                self.mark_chunk_boundary_dirty(neighbor);
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

    fn deferred_edit_order_key(edit: &DeferredBlockEdit) -> (u64, i32, i32, i32) {
        (
            edit.frame_index,
            edit.world_pos.x,
            edit.world_pos.y,
            edit.world_pos.z,
        )
    }

    fn canonicalize_deferred_edit(edit: DeferredBlockEdit) -> (BlockId, u8) {
        match edit.kind {
            DeferredEditKind::PlaceFluid | DeferredEditKind::PlaceBlock => {
                if let Some(kind) = fluid_kind(edit.block) {
                    (make_still(kind), fluid_data_make(0, false, true))
                } else {
                    (edit.block, edit.data)
                }
            }
        }
    }

    fn enqueue_chunkgen_candidate_for_coord(&mut self, coord: ChunkCoord) {
        if self.chunk_is_generation_satisfied(coord) || self.chunkgen_inflight.contains(&coord) {
            return;
        }
        self.insert_chunkgen_pending_coord(coord);
    }

    fn insert_chunkgen_pending_coord(&mut self, coord: ChunkCoord) -> bool {
        if !self.chunkgen_pending_set.insert(coord) {
            return false;
        }
        self.chunkgen_pending_enqueued_frame
            .insert(coord, self.frame_index);
        self.chunkgen_pending_round_robin.push_back(coord);
        self.frame_perf.stream_scheduler_pending_peak = self
            .frame_perf
            .stream_scheduler_pending_peak
            .max(self.chunkgen_pending_set.len());
        true
    }

    fn remove_chunkgen_pending_coord(&mut self, coord: ChunkCoord) {
        self.chunkgen_pending_set.remove(&coord);
        self.chunkgen_pending_enqueued_frame.remove(&coord);
        self.chunkgen_pending_priority.remove(&coord);
    }

    fn refresh_chunkgen_pending_coord_priority(
        &mut self,
        coord: ChunkCoord,
        center_chunk_x: i32,
        center_chunk_y: i32,
        center_chunk_z: i32,
        camera: Option<&Camera>,
        frustum: Option<&Frustum>,
        forward_xz: Option<Vec3>,
    ) -> bool {
        if !self.chunkgen_pending_set.contains(&coord)
            || self.chunk_is_generation_satisfied(coord)
            || self.chunkgen_inflight.contains(&coord)
        {
            self.remove_chunkgen_pending_coord(coord);
            return false;
        }
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
            frustum,
            forward_xz,
            age,
        );
        self.chunkgen_pending_priority
            .insert(coord, (self.chunkgen_priority_epoch, key));
        self.chunkgen_pending_heap.push(ChunkPriorityHeapEntry {
            coord,
            key,
            epoch: self.chunkgen_priority_epoch,
        });
        self.frame_perf.stream_scheduler_reprioritized_items += 1;
        self.frame_perf.stream_scheduler_heap_peak = self
            .frame_perf
            .stream_scheduler_heap_peak
            .max(self.chunkgen_pending_heap.len());
        true
    }

    fn evict_oldest_deferred_edit_global(&mut self) -> bool {
        let mut oldest: Option<(ChunkCoord, usize, (u64, i32, i32, i32, i32, i32, i32))> = None;
        for (&coord, edits) in &self.deferred_block_edits {
            let Some((idx, edit)) = edits
                .iter()
                .enumerate()
                .min_by_key(|(_, edit)| Self::deferred_edit_order_key(edit))
            else {
                continue;
            };
            let key = (
                edit.frame_index,
                edit.world_pos.x,
                edit.world_pos.y,
                edit.world_pos.z,
                coord.x,
                coord.y,
                coord.z,
            );
            if oldest
                .as_ref()
                .map(|(_, _, best_key)| key < *best_key)
                .unwrap_or(true)
            {
                oldest = Some((coord, idx, key));
            }
        }

        let Some((coord, idx, _)) = oldest else {
            return false;
        };

        let mut remove_coord_entry = false;
        if let Some(edits) = self.deferred_block_edits.get_mut(&coord) {
            if idx < edits.len() {
                edits.swap_remove(idx);
                self.deferred_block_edits_total = self.deferred_block_edits_total.saturating_sub(1);
            }
            remove_coord_entry = edits.is_empty();
        }
        if remove_coord_entry {
            self.deferred_block_edits.remove(&coord);
        }
        true
    }

    fn materialize_known_empty_chunk_for_edits(&mut self, coord: ChunkCoord) -> bool {
        if !self.generated_empty_chunks.contains(&coord) || !self.has_deferred_edits(coord) {
            return false;
        }

        let mut chunk = Chunk::new_empty();
        let stats = self.apply_deferred_edits_to_chunk(coord, &mut chunk);
        if chunk.is_empty() {
            return false;
        }

        self.generated_empty_chunks.remove(&coord);
        self.full_solid_mesh_skip_set.remove(&coord);
        self.seed_pending_lighting_for_chunk(coord, &mut chunk);
        self.world.chunks.insert(coord, chunk);
        self.chunk_origin.insert(coord, ChunkOrigin::Generated);
        self.mark_column_dirty_for_chunk(coord);
        self.mark_chunk_dirty(coord);
        self.mark_neighbors_dirty_if_meshed(coord);
        self.mark_skipped_full_solid_neighbors_dirty(coord);
        if stats.applied_fluid > 0 && self.is_sim_column_active(coord.x, coord.z) {
            self.enqueue_generated_fluids_for_chunk(coord);
        }
        self.run_incremental_chunk_added_lighting(&[coord], "materialize_empty_chunk");
        true
    }

    fn ensure_chunk_available_for_deferred_edit(&mut self, coord: ChunkCoord) {
        if self.chunk_is_generation_satisfied(coord) && self.world.get_chunk(coord).is_some() {
            return;
        }
        if self.generated_empty_chunks.contains(&coord) {
            let _ = self.materialize_known_empty_chunk_for_edits(coord);
            return;
        }
        let queued_saved = self.try_queue_saved_column_load_request(coord.x, coord.z);
        if self.chunkgen_inflight.contains(&coord) {
            return;
        }
        if !queued_saved
            && !self
                .saved_column_loads_inflight
                .contains(&(coord.x, coord.z))
        {
            let preset = self.worldgen_preset;
            if !self.request_chunk_generation(coord, preset) {
                self.enqueue_chunkgen_candidate_for_coord(coord);
            }
        } else {
            // Let streaming request chunkgen after save-load settles.
            self.enqueue_chunkgen_candidate_for_coord(coord);
        }
    }

    fn apply_deferred_edits_to_loaded_world_chunk(&mut self, coord: ChunkCoord) -> ApplyEditsStats {
        if !self.has_deferred_edits(coord) {
            return ApplyEditsStats::default();
        }
        let Some(mut chunk) = self.world.chunks.remove(&coord) else {
            return ApplyEditsStats::default();
        };
        let stats = self.apply_deferred_edits_to_chunk(coord, &mut chunk);
        self.full_solid_mesh_skip_set.remove(&coord);
        if chunk.is_empty() {
            self.generated_empty_chunks.insert(coord);
            self.chunk_origin.remove(&coord);
        } else {
            self.generated_empty_chunks.remove(&coord);
            self.world.chunks.insert(coord, chunk);
            self.mark_chunk_user_only_if_unknown(coord);
        }
        stats
    }

    pub(crate) fn defer_block_edit(&mut self, edit: DeferredBlockEdit) {
        if !World::in_bounds(edit.world_pos) {
            return;
        }
        let coord = World::world_to_chunk_coord(edit.world_pos);

        let mut removed_old = 0usize;
        let mut dropped_due_to_caps = 0usize;
        {
            let edits = self.deferred_block_edits.entry(coord).or_default();
            if let Some(existing) = edits
                .iter_mut()
                .find(|existing| existing.world_pos == edit.world_pos)
            {
                if edit.frame_index >= existing.frame_index {
                    *existing = edit;
                }
                return;
            }

            if edits.len() >= DEFERRED_EDITS_PER_CHUNK_CAP
                && let Some((idx, _)) = edits
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, edit)| Self::deferred_edit_order_key(edit))
            {
                edits.swap_remove(idx);
                removed_old = 1;
                dropped_due_to_caps += 1;
            }

            edits.push(edit);
        }

        self.deferred_block_edits_total = self
            .deferred_block_edits_total
            .saturating_add(1)
            .saturating_sub(removed_old);

        while self.deferred_block_edits_total > DEFERRED_EDITS_GLOBAL_CAP {
            if !self.evict_oldest_deferred_edit_global() {
                break;
            }
            dropped_due_to_caps += 1;
        }

        if dropped_due_to_caps > 0 {
            self.frame_perf.deferred_edits_dropped_due_to_caps += dropped_due_to_caps;
        }
        self.frame_perf.deferred_edits_pending_total = self.deferred_block_edits_total;
    }

    pub(crate) fn has_deferred_edits(&self, coord: ChunkCoord) -> bool {
        self.deferred_block_edits
            .get(&coord)
            .is_some_and(|edits| !edits.is_empty())
    }

    pub(crate) fn drain_deferred_edits_for_chunk(
        &mut self,
        coord: ChunkCoord,
    ) -> Vec<DeferredBlockEdit> {
        let Some(mut edits) = self.deferred_block_edits.remove(&coord) else {
            return Vec::new();
        };
        self.deferred_block_edits_total =
            self.deferred_block_edits_total.saturating_sub(edits.len());
        edits.sort_by_key(Self::deferred_edit_order_key);
        edits
    }

    pub(crate) fn apply_deferred_edits_to_chunk(
        &mut self,
        coord: ChunkCoord,
        chunk: &mut Chunk,
    ) -> ApplyEditsStats {
        let started = Instant::now();
        let edits = self.drain_deferred_edits_for_chunk(coord);
        if edits.is_empty() {
            self.frame_perf.deferred_edits_pending_total = self.deferred_block_edits_total;
            return ApplyEditsStats::default();
        }

        let mut stats = ApplyEditsStats::default();
        for edit in edits {
            stats.total += 1;
            if !World::in_bounds(edit.world_pos) {
                stats.skipped_out_of_bounds += 1;
                continue;
            }
            if World::world_to_chunk_coord(edit.world_pos) != coord {
                stats.skipped_conflict += 1;
                continue;
            }
            let local = World::world_to_local(edit.world_pos);
            if !Chunk::in_bounds(local.x, local.y, local.z) {
                stats.skipped_out_of_bounds += 1;
                continue;
            }

            let (block, data) = Self::canonicalize_deferred_edit(edit);
            let prev_block = chunk.get(local.x, local.y, local.z);
            let prev_data = chunk.get_data(local.x, local.y, local.z);
            if prev_block == block && prev_data == data {
                stats.skipped_unchanged += 1;
                continue;
            }

            chunk.set(local.x, local.y, local.z, block);
            chunk.set_data(local.x, local.y, local.z, data);
            stats.applied += 1;
            if fluid_kind(block).is_some() {
                stats.applied_fluid += 1;
            }
        }
        self.frame_perf.deferred_edits_applied += stats.applied;
        self.frame_perf.deferred_edits_chunks_touched += 1;
        self.frame_perf.deferred_edits_pending_total = self.deferred_block_edits_total;
        let apply_ms = started.elapsed().as_secs_f32() * 1000.0;
        perf_trace::record_duration_ms(
            "deferred_edits.apply_to_chunk",
            apply_ms,
            &format!(
                "coord=({}, {}, {}) total={} applied={} applied_fluid={} skip_oob={} skip_conflict={} skip_unchanged={} pending_total={}",
                coord.x,
                coord.y,
                coord.z,
                stats.total,
                stats.applied,
                stats.applied_fluid,
                stats.skipped_out_of_bounds,
                stats.skipped_conflict,
                stats.skipped_unchanged,
                self.deferred_block_edits_total
            ),
        );
        stats
    }

    fn trigger_fluid_mesh_rebuild(&mut self, coord: ChunkCoord) {
        let was_already_dirty = self.dirty_chunk_set.contains(&coord);
        self.mark_chunk_dirty_with_reasons(coord, MeshDirtyReasons::GEOMETRY);
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
        for (index, coord) in affected.iter().copied().enumerate() {
            if index == 0 {
                self.mark_chunk_dirty_with_reasons(coord, MeshDirtyReasons::GEOMETRY);
            } else {
                self.mark_chunk_boundary_dirty(coord);
            }
            self.mark_column_dirty_for_chunk(coord);
        }
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
        (dirty_chunk_count, dirty_column_count, any_dirty)
    }

    fn note_chunk_lighting_provisional(&mut self, coords: &[ChunkCoord], reason: &str) {
        let now = Instant::now();
        for &coord in coords {
            if self.world.get_chunk(coord).is_none() {
                continue;
            }
            self.uploaded_final_lighting_chunks.remove(&coord);
            self.chunk_lighting_progress.insert(
                coord,
                ChunkLightingProgress {
                    state: ChunkLightingReadiness::Provisional,
                    started_at: now,
                    visually_acceptable_at: None,
                    finalized_at: None,
                },
            );
            perf_trace::instant_event(
                "lighting.chunk_provisional",
                &format!(
                    "coord=({}, {}, {}) reason={} state={}",
                    coord.x,
                    coord.y,
                    coord.z,
                    reason,
                    ChunkLightingReadiness::Provisional.as_str()
                ),
            );
        }
    }

    fn note_chunk_lighting_visual_ready(&mut self, coords: &[ChunkCoord], reason: &str) {
        let now = Instant::now();
        for &coord in coords {
            if self.world.get_chunk(coord).is_none() {
                continue;
            }
            self.uploaded_final_lighting_chunks.remove(&coord);
            let progress = self
                .chunk_lighting_progress
                .entry(coord)
                .or_insert_with(|| ChunkLightingProgress {
                    state: ChunkLightingReadiness::Provisional,
                    started_at: now,
                    visually_acceptable_at: None,
                    finalized_at: None,
                });
            if progress.state == ChunkLightingReadiness::Finalized {
                continue;
            }
            progress.state = ChunkLightingReadiness::VisualReady;
            if progress.visually_acceptable_at.is_none() {
                progress.visually_acceptable_at = Some(now);
                perf_trace::record_duration(
                    "lighting.chunk_visually_acceptable",
                    now.saturating_duration_since(progress.started_at),
                    &format!(
                        "coord=({}, {}, {}) reason={} state={}",
                        coord.x,
                        coord.y,
                        coord.z,
                        reason,
                        ChunkLightingReadiness::VisualReady.as_str()
                    ),
                );
            }
            perf_trace::instant_event(
                "lighting.chunk_state",
                &format!(
                    "coord=({}, {}, {}) reason={} state={}",
                    coord.x,
                    coord.y,
                    coord.z,
                    reason,
                    ChunkLightingReadiness::VisualReady.as_str()
                ),
            );
        }
    }

    fn should_defer_startup_chunk_added_lighting(&self, coord: ChunkCoord) -> bool {
        self.world_generation_task.is_some() && self.is_worldgen_warmup_coord(coord)
    }

    fn startup_final_lighting_batch_ready(&self) -> bool {
        let Some(task) = self.world_generation_task.as_ref() else {
            return false;
        };
        let target_sections = task.startup_plan.target_sections.len();
        if target_sections == 0 {
            return false;
        }
        let applied_sections = task.startup_plan.applied_sections.len();
        applied_sections.saturating_mul(100)
            >= target_sections.saturating_mul(STARTUP_FINAL_LIGHTING_TRIGGER_PERCENT)
    }

    fn chunk_lighting_boundary_complete(&self, coord: ChunkCoord) -> bool {
        if self.world.get_chunk(coord).is_none() {
            return false;
        }
        if self.pending_chunk_added_lighting_coords.contains(&coord) {
            return false;
        }
        let min_chunk_y = self.world.min_chunk_y();
        let max_chunk_y = self.world.max_chunk_y();
        for neighbor in Self::neighbor_chunk_coords(coord) {
            if neighbor.y < min_chunk_y || neighbor.y > max_chunk_y {
                continue;
            }
            if self.world.get_chunk(neighbor).is_none() {
                return false;
            }
            if self.pending_chunk_added_lighting_coords.contains(&neighbor) {
                return false;
            }
        }
        true
    }

    fn finalize_chunk_lighting_state(&mut self, coord: ChunkCoord, reason: &str, now: Instant) {
        if self.world.get_chunk(coord).is_none() {
            self.chunk_lighting_progress.remove(&coord);
            self.uploaded_final_lighting_chunks.remove(&coord);
            return;
        }

        let next_state = if self.chunk_lighting_boundary_complete(coord) {
            ChunkLightingReadiness::Finalized
        } else {
            ChunkLightingReadiness::VisualReady
        };
        let progress = self
            .chunk_lighting_progress
            .entry(coord)
            .or_insert_with(|| ChunkLightingProgress {
                state: ChunkLightingReadiness::Provisional,
                started_at: now,
                visually_acceptable_at: None,
                finalized_at: None,
            });
        progress.state = next_state;
        if progress.visually_acceptable_at.is_none() {
            progress.visually_acceptable_at = Some(now);
            perf_trace::record_duration(
                "lighting.chunk_visually_acceptable",
                now.saturating_duration_since(progress.started_at),
                &format!(
                    "coord=({}, {}, {}) reason={} state={}",
                    coord.x,
                    coord.y,
                    coord.z,
                    reason,
                    next_state.as_str()
                ),
            );
        }
        if next_state == ChunkLightingReadiness::Finalized && progress.finalized_at.is_none() {
            progress.finalized_at = Some(now);
            perf_trace::record_duration(
                "lighting.chunk_finalized",
                now.saturating_duration_since(progress.started_at),
                &format!(
                    "coord=({}, {}, {}) reason={}",
                    coord.x, coord.y, coord.z, reason
                ),
            );
        }
        perf_trace::instant_event(
            "lighting.chunk_state",
            &format!(
                "coord=({}, {}, {}) reason={} state={}",
                coord.x,
                coord.y,
                coord.z,
                reason,
                next_state.as_str()
            ),
        );
        if next_state == ChunkLightingReadiness::Finalized {
            self.maybe_release_mesh_after_lighting_finalization(coord);
        }
    }

    fn commit_incremental_lighting_result(
        &mut self,
        reason: &str,
        primary_coords: &[ChunkCoord],
        removed_coords: &[ChunkCoord],
        result: IncrementalLightingResult,
        job_ms: f32,
    ) {
        self.frame_perf.rebuild_world_lighting_ms += job_ms;
        self.frame_perf.lighting_incremental_jobs += 1;
        self.frame_perf.lighting_incremental_primary_chunks += primary_coords.len();
        self.frame_perf.lighting_incremental_region_chunks += result.stats.region_chunks;
        self.frame_perf.lighting_incremental_voxels_visited = self
            .frame_perf
            .lighting_incremental_voxels_visited
            .saturating_add(result.stats.voxels_visited);
        self.frame_perf.lighting_incremental_chunks_touched += result.stats.affected_chunks;
        self.frame_perf.lighting_incremental_boundary_propagations = self
            .frame_perf
            .lighting_incremental_boundary_propagations
            .saturating_add(result.stats.boundary_propagations);
        self.frame_perf.lighting_chunks_changed += result.stats.affected_chunks;

        for &coord in removed_coords {
            self.chunk_lighting_progress.remove(&coord);
            self.uploaded_final_lighting_chunks.remove(&coord);
        }
        let affected_chunk_set = result
            .affected_chunks
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        let mut boundary_neighbor_chunks = HashSet::new();
        for &coord in &result.affected_chunks {
            self.queue_lighting_changed_chunk_dirty(coord);
            for neighbor in Self::neighbor_chunk_coords(coord) {
                if !affected_chunk_set.contains(&neighbor)
                    && self.world.get_chunk(neighbor).is_some()
                {
                    boundary_neighbor_chunks.insert(neighbor);
                }
            }
        }
        for coord in boundary_neighbor_chunks.iter().copied() {
            self.mark_chunk_boundary_dirty(coord);
        }

        let now = Instant::now();
        let mut state_candidates: HashSet<ChunkCoord> = HashSet::new();
        for &coord in primary_coords {
            state_candidates.insert(coord);
            for neighbor in Self::neighbor_chunk_coords(coord) {
                state_candidates.insert(neighbor);
            }
        }
        for &coord in removed_coords {
            for neighbor in Self::neighbor_chunk_coords(coord) {
                state_candidates.insert(neighbor);
            }
        }
        for &coord in &result.affected_chunks {
            state_candidates.insert(coord);
        }
        for &coord in &boundary_neighbor_chunks {
            state_candidates.insert(coord);
        }
        for &coord in removed_coords {
            state_candidates.remove(&coord);
        }
        for coord in state_candidates {
            self.finalize_chunk_lighting_state(coord, reason, now);
        }

        perf_trace::record_duration(
            "lighting.incremental_job",
            Duration::from_secs_f32(job_ms.max(0.0) / 1000.0),
            &format!(
                "reason={} primary={} removed={} affected={} region_chunks={} voxels_visited={} changed_voxels={} boundary_props={} sky_visits={} block_visits={} chunk_clears={} sky_columns={} emitters={}",
                reason,
                primary_coords.len(),
                removed_coords.len(),
                result.stats.affected_chunks,
                result.stats.region_chunks,
                result.stats.voxels_visited,
                result.stats.changed_voxels,
                result.stats.boundary_propagations,
                result.stats.sky_queue_visits,
                result.stats.block_queue_visits,
                result.stats.chunk_clears,
                result.stats.sky_columns_seeded,
                result.stats.block_emitters_seeded
            ),
        );
    }

    fn chunk_stream_lighting_center(&self) -> ChunkCoord {
        let (x, y, z) = self.last_stream_center.unwrap_or((0, 0, 0));
        ChunkCoord { x, y, z }
    }

    fn collect_chunk_added_lighting_region(&self, coords: &[ChunkCoord]) -> HashSet<ChunkCoord> {
        let mut region = HashSet::with_capacity(coords.len() * 3);
        for &coord in coords {
            if self.world.get_chunk(coord).is_none() {
                continue;
            }
            region.insert(coord);
            for neighbor in Self::neighbor_chunk_coords(coord) {
                if self.world.get_chunk(neighbor).is_some() {
                    region.insert(neighbor);
                }
            }
        }
        region
    }

    fn enqueue_chunk_stream_added_lighting_internal(
        &mut self,
        coords: &[ChunkCoord],
        reason: &'static str,
        reset_to_provisional: bool,
    ) {
        let mut active_coords = coords
            .iter()
            .copied()
            .filter(|coord| {
                self.world.get_chunk(*coord).is_some()
                    && !self.pending_chunk_added_lighting_coords.contains(coord)
            })
            .collect::<Vec<_>>();
        if active_coords.is_empty() {
            return;
        }

        let center = self.chunk_stream_lighting_center();
        active_coords.sort_by_key(|coord| {
            let dx = coord.x - center.x;
            let dy = coord.y - center.y;
            let dz = coord.z - center.z;
            (dx * dx + dy * dy + dz * dz, coord.x, coord.y, coord.z)
        });
        if reset_to_provisional {
            self.note_chunk_lighting_provisional(&active_coords, reason);
        }

        let original_batch_id = self.next_chunk_added_lighting_batch_id;
        self.next_chunk_added_lighting_batch_id = self
            .next_chunk_added_lighting_batch_id
            .wrapping_add(1)
            .max(1);
        let source_primary_count = active_coords.len();
        let mut split_jobs = Vec::<(Vec<ChunkCoord>, Vec<ChunkCoord>)>::new();
        let mut current_primary = Vec::<ChunkCoord>::new();
        let mut current_region = HashSet::<ChunkCoord>::new();

        for coord in active_coords {
            let candidate_region = self.collect_chunk_added_lighting_region(&[coord]);
            let mut merged_region_len = current_region.len();
            for candidate in &candidate_region {
                if !current_region.contains(candidate) {
                    merged_region_len += 1;
                }
            }
            if !current_primary.is_empty()
                && (current_primary.len() >= STREAM_CHUNK_LIGHTING_MAX_PRIMARY_CHUNKS_PER_JOB
                    || merged_region_len > STREAM_CHUNK_LIGHTING_MAX_REGION_CHUNKS_PER_JOB)
            {
                split_jobs.push((
                    std::mem::take(&mut current_primary),
                    current_region.drain().collect(),
                ));
            }
            current_primary.push(coord);
            current_region.extend(candidate_region);
        }
        if !current_primary.is_empty() {
            split_jobs.push((current_primary, current_region.into_iter().collect()));
        }

        let split_job_count = split_jobs.len();
        self.frame_perf.lighting_incremental_split_jobs += split_job_count;
        for (split_index, (primary_coords, region_coords)) in split_jobs.into_iter().enumerate() {
            for &coord in &primary_coords {
                self.pending_chunk_added_lighting_coords.insert(coord);
            }
            let Some(task) = self
                .world
                .begin_incremental_region_lighting_task(region_coords)
            else {
                for coord in primary_coords {
                    self.pending_chunk_added_lighting_coords.remove(&coord);
                }
                continue;
            };
            self.pending_chunk_added_lighting_jobs
                .push_back(ChunkAddedLightingJob {
                    original_batch_id,
                    split_job_index: split_index + 1,
                    split_job_count,
                    source_primary_count,
                    primary_coords,
                    task,
                    enqueued_at: Instant::now(),
                    cpu_ms_accum: 0.0,
                });
        }
        self.frame_perf.lighting_incremental_backlog_depth = self
            .frame_perf
            .lighting_incremental_backlog_depth
            .max(self.pending_chunk_added_lighting_jobs.len());
        perf_trace::instant_event(
            "lighting.chunk_stream_batch_split",
            &format!(
                "reason={} batch={} primary={} split_jobs={} backlog={}",
                reason,
                original_batch_id,
                source_primary_count,
                split_job_count,
                self.pending_chunk_added_lighting_jobs.len()
            ),
        );
    }

    fn enqueue_chunk_stream_added_lighting(&mut self, coords: &[ChunkCoord], reason: &'static str) {
        self.enqueue_chunk_stream_added_lighting_internal(coords, reason, true);
    }

    fn maybe_enqueue_startup_deferred_final_lighting(&mut self) {
        if self.startup_deferred_chunk_added_lighting_coords.is_empty()
            || !self.startup_final_lighting_batch_ready()
        {
            return;
        }

        let mut coords = self
            .startup_deferred_chunk_added_lighting_coords
            .drain()
            .filter(|coord| self.world.get_chunk(*coord).is_some())
            .collect::<Vec<_>>();
        if coords.is_empty() {
            return;
        }

        let center = self.chunk_stream_lighting_center();
        coords.sort_by_key(|coord| {
            let dx = coord.x - center.x;
            let dy = coord.y - center.y;
            let dz = coord.z - center.z;
            (dx * dx + dy * dy + dz * dz, coord.x, coord.y, coord.z)
        });
        let target_sections = self
            .world_generation_task
            .as_ref()
            .map(|task| task.startup_plan.target_sections.len())
            .unwrap_or(0);
        let applied_sections = self
            .world_generation_task
            .as_ref()
            .map(|task| task.startup_plan.applied_sections.len())
            .unwrap_or(0);
        perf_trace::instant_event(
            "lighting.startup_batch_enqueue",
            &format!(
                "coords={} applied_sections={}/{} backlog={} trigger={}%",
                coords.len(),
                applied_sections,
                target_sections,
                self.pending_chunk_added_lighting_jobs.len(),
                STARTUP_FINAL_LIGHTING_TRIGGER_PERCENT
            ),
        );
        crate::console_info!(
            "[perf][startup][lighting] enqueue deferred final lighting batch: coords={} applied_sections={}/{} backlog={}",
            coords.len(),
            applied_sections,
            target_sections,
            self.pending_chunk_added_lighting_jobs.len()
        );
        self.enqueue_chunk_stream_added_lighting_internal(
            &coords,
            "startup_chunk_stream_finalize",
            false,
        );
    }

    fn pump_chunk_added_lighting_jobs(&mut self) {
        let _trace_scope = perf_trace::scope("render.lighting.step");
        let started = Instant::now();
        let frame_budget =
            Duration::from_secs_f32((STREAM_CHUNK_LIGHTING_FRAME_BUDGET_MS / 1000.0).max(0.0));
        let step_limits = IncrementalLightingStepLimits {
            max_voxels_visited: STREAM_CHUNK_LIGHTING_MAX_VOXELS_PER_STEP,
            max_wall_clock: Duration::from_secs_f32(
                (STREAM_CHUNK_LIGHTING_STEP_BUDGET_MS / 1000.0).max(0.0),
            ),
        };
        let mut steps = 0usize;
        self.frame_perf.lighting_incremental_backlog_depth = self
            .frame_perf
            .lighting_incremental_backlog_depth
            .max(self.pending_chunk_added_lighting_jobs.len());

        while steps < STREAM_CHUNK_LIGHTING_MAX_STEPS_PER_FRAME
            && !self.pending_chunk_added_lighting_jobs.is_empty()
            && started.elapsed() < frame_budget
        {
            let Some(mut job) = self.pending_chunk_added_lighting_jobs.pop_front() else {
                break;
            };
            let step_started = Instant::now();
            let step_result = self
                .world
                .step_incremental_lighting_task(&mut job.task, step_limits);
            let step_ms = step_started.elapsed().as_secs_f32() * 1000.0;
            job.cpu_ms_accum += step_ms;
            steps += 1;
            self.last_lighting_step_ms = step_ms;
            self.frame_perf.lighting_incremental_steps += 1;
            self.frame_perf.lighting_incremental_step_total_ms += step_ms;
            self.frame_perf.lighting_incremental_step_max_ms = self
                .frame_perf
                .lighting_incremental_step_max_ms
                .max(step_ms);
            self.frame_perf.lighting_incremental_backlog_depth = self
                .frame_perf
                .lighting_incremental_backlog_depth
                .max(self.pending_chunk_added_lighting_jobs.len() + 1);
            perf_trace::record_duration_ms(
                "lighting.incremental_step",
                step_ms,
                &format!(
                    "reason=chunk_stream_add batch={} split={}/{} source_primary={} primary={} region_chunks={} voxels_visited={} affected={} boundary_props={} backlog={} q_pending={} latency_ms={:.2} complete={}",
                    job.original_batch_id,
                    job.split_job_index,
                    job.split_job_count,
                    job.source_primary_count,
                    job.primary_coords.len(),
                    job.task.region_chunk_count(),
                    step_result.stats.voxels_visited,
                    step_result.stats.affected_chunks,
                    step_result.stats.boundary_propagations,
                    self.pending_chunk_added_lighting_jobs.len() + 1,
                    job.task.pending_queue_len(),
                    job.enqueued_at.elapsed().as_secs_f32() * 1000.0,
                    step_result.complete as u8
                ),
            );

            if step_result.complete {
                for &coord in &job.primary_coords {
                    self.pending_chunk_added_lighting_coords.remove(&coord);
                }
                let result = job.task.into_result();
                self.commit_incremental_lighting_result(
                    "chunk_stream_add",
                    &job.primary_coords,
                    &[],
                    result,
                    job.cpu_ms_accum,
                );
            } else {
                self.pending_chunk_added_lighting_jobs.push_front(job);
            }
        }

        let step_total_ms = started.elapsed().as_secs_f32() * 1000.0;
        perf_trace::record_duration_ms(
            "render.lighting.step",
            step_total_ms,
            &format!(
                "pending_dirty={} req_pending={} inflight={} result_q={} stream_backlog={} remove_backlog={} step_count={} step_max_ms={:.3}",
                self.pending_lighting_dirty_chunks.len(),
                self.lighting_request_pending as u8,
                self.lighting_job_inflight as u8,
                self.lighting_results.len(),
                self.pending_chunk_added_lighting_jobs.len(),
                self.pending_removed_lighting_batches.len(),
                steps,
                self.frame_perf.lighting_incremental_step_max_ms
            ),
        );
    }

    fn pump_removed_lighting_batches(&mut self) {
        let started = Instant::now();
        let frame_budget =
            Duration::from_secs_f32((STREAM_CHUNK_LIGHTING_FRAME_BUDGET_MS / 1000.0).max(0.0));
        while let Some(batch) = self.pending_removed_lighting_batches.pop_front() {
            if started.elapsed() >= frame_budget {
                self.pending_removed_lighting_batches.push_front(batch);
                break;
            }
            let apply_started = Instant::now();
            self.run_incremental_chunk_removed_lighting(&batch.coords, batch.reason);
            let step_ms = apply_started.elapsed().as_secs_f32() * 1000.0;
            self.last_lighting_step_ms = self.last_lighting_step_ms.max(step_ms);
            perf_trace::record_duration_ms(
                "lighting.incremental_remove_step",
                step_ms,
                &format!(
                    "reason={} chunks={} backlog={} latency_ms={:.2}",
                    batch.reason,
                    batch.coords.len(),
                    self.pending_removed_lighting_batches.len(),
                    batch.enqueued_at.elapsed().as_secs_f32() * 1000.0
                ),
            );
            break;
        }
    }

    fn run_incremental_chunk_added_lighting(&mut self, coords: &[ChunkCoord], reason: &str) {
        let active_coords: Vec<ChunkCoord> = coords
            .iter()
            .copied()
            .filter(|coord| self.world.get_chunk(*coord).is_some())
            .collect();
        if active_coords.is_empty() {
            return;
        }
        self.note_chunk_lighting_provisional(&active_coords, reason);
        let started = Instant::now();
        let result = self.world.lighting_on_chunks_added(&active_coords);
        self.commit_incremental_lighting_result(
            reason,
            &active_coords,
            &[],
            result,
            started.elapsed().as_secs_f32() * 1000.0,
        );
    }

    fn run_incremental_chunk_removed_lighting(&mut self, coords: &[ChunkCoord], reason: &str) {
        if coords.is_empty() {
            return;
        }
        let started = Instant::now();
        let result = self.world.lighting_on_chunks_removed(coords);
        self.commit_incremental_lighting_result(
            reason,
            &[],
            coords,
            result,
            started.elapsed().as_secs_f32() * 1000.0,
        );
    }

    fn run_incremental_block_change_lighting(
        &mut self,
        changes: &[LightingBlockChange],
        reason: &str,
    ) {
        if changes.is_empty() {
            return;
        }
        let primary_coords: Vec<ChunkCoord> = changes
            .iter()
            .map(|change| World::world_to_chunk_coord(change.pos))
            .collect();
        let started = Instant::now();
        let result = self.world.lighting_on_block_changes(changes);
        self.commit_incremental_lighting_result(
            reason,
            &primary_coords,
            &[],
            result,
            started.elapsed().as_secs_f32() * 1000.0,
        );
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
        let mut lighting_changes = Vec::with_capacity(BLOCK_EDIT_SIDEFX_MAX_EDITS_PER_FRAME);
        let mut edits_remaining_before = self.pending_block_edits.len();
        let mut sand_remaining_before = self.pending_sand_gravity_checks.len();

        while edits_processed < BLOCK_EDIT_SIDEFX_MAX_EDITS_PER_FRAME && started.elapsed() < budget
        {
            let Some(edit) = self.pending_block_edits.pop_front() else {
                break;
            };

            edits_processed += 1;
            let _ = self.stage_block_edit_dirty_after_block_change(edit.pos);
            lighting_changes.push(LightingBlockChange {
                pos: edit.pos,
                old: edit.old,
                new: edit.new,
            });

            self.notify_fluid_on_block_change(edit.pos, edit.old, edit.new);
            fluid_notifies += 1;

            self.enqueue_pending_sand_gravity_check(edit.pos);

            // Spawn particles only for block destruction, not placement.
            if edit.new == BlockId::Air
                && particles_spawned + PARTICLE_SPAWN_COUNT
                    <= BLOCK_EDIT_PARTICLES_MAX_SPAWN_PER_FRAME
            {
                let spawned = self.spawn_block_particles(edit.pos, edit.old);
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

        let (dirty_chunks_flushed, dirty_columns_flushed, dirty_marks_flushed) =
            self.flush_pending_block_edit_dirty_marks();
        self.run_incremental_block_change_lighting(&lighting_changes, "block_sidefx");

        let ms = started.elapsed().as_secs_f32() * 1000.0;
        edits_remaining_before = edits_remaining_before.saturating_sub(edits_processed);
        sand_remaining_before = sand_remaining_before.saturating_sub(sand_checks_processed);
        perf_trace::record_duration_ms(
            "proc.process_pending_block_edit_sidefx.work",
            ms,
            &format!(
                "edits_proc={} edits_q_before={} edits_q_after={} fluids={} particles_spawned={} particle_events={} sand_checks_proc={} sand_q_before={} sand_q_after={} sand_steps={} sand_scanned={} sand_dropped={} dirty_chunks_flush={} dirty_cols_flush={} dirty_marks_flushed={}",
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
                dirty_marks_flushed as u8
            ),
        );
    }

    fn request_lighting_rebuild(&mut self) {
        // Retained as a fallback for explicit bulk rebuilds; ordinary streaming
        // now uses immediate incremental lighting hooks instead.
        let was_pending = self.lighting_request_pending;
        if self.lighting_job_inflight || !self.lighting_request_pending {
            self.lighting_generation = self.lighting_generation.wrapping_add(1);
        }
        self.lighting_request_pending = true;
        self.lighting_rebuild_not_before =
            Some(Instant::now() + Duration::from_millis(LIGHTING_REBUILD_DEBOUNCE_MS));
        if !was_pending {
            crate::console_error!(
                "[perf][lighting][async] requested gen={} debounce_ms={} inflight={}",
                self.lighting_generation,
                LIGHTING_REBUILD_DEBOUNCE_MS,
                self.lighting_job_inflight
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
            self.mark_chunk_lighting_dirty(coord);
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
                crate::console_error!(
                    "[perf][lighting][async] discard stale result gen={} current_gen={}",
                    result.generation,
                    self.lighting_generation
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

            crate::console_error!(
                "[perf][lighting][async] applied gen={} changed_chunks={}",
                result.generation,
                changed_count
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
            crate::console_error!("[warn] lighting worker channel closed; keeping request pending");
            self.lighting_request_pending = true;
            self.lighting_job_inflight = false;
            return;
        }

        self.lighting_job_inflight = true;
        self.lighting_request_pending = false;
        self.lighting_rebuild_not_before = None;
        crate::console_error!(
            "[perf][lighting][async] queued gen={} loaded_chunks={} xz_columns={} snapshot_ms={:.2}",
            generation,
            loaded_chunk_count,
            xz_column_count,
            snapshot_ms
        );
    }

    fn rebuild_world_lighting_if_dirty(&mut self) {
        let _trace_scope = perf_trace::scope("state.rebuild_world_lighting_if_dirty");
        self.pump_lighting_results();
        self.pump_removed_lighting_batches();
        self.pump_chunk_added_lighting_jobs();
        self.pump_pending_lighting_dirty_chunk_marks();
        self.schedule_lighting_rebuild_if_needed();
    }

    fn sync_generated_empty_chunk_marker(&mut self, coord: ChunkCoord) {
        if self.world.get_chunk(coord).is_none() {
            self.generated_empty_chunks.insert(coord);
            self.chunk_origin.remove(&coord);
            if self.has_deferred_edits(coord) {
                let _ = self.materialize_known_empty_chunk_for_edits(coord);
            }
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
        if self
            .world
            .set_block_and_data_loaded_only(pos, BlockId::Air, 0)
            .is_err()
        {
            return false;
        }
        if self
            .world
            .set_block_and_data_loaded_only(landing, BlockId::Sand, 0)
            .is_err()
        {
            let _ = self
                .world
                .set_block_and_data_loaded_only(pos, BlockId::Sand, 0);
            return false;
        }
        self.mark_chunk_user_only_if_unknown(World::world_to_chunk_coord(pos));
        self.mark_chunk_user_only_if_unknown(World::world_to_chunk_coord(landing));
        self.sync_generated_empty_chunk_marker(World::world_to_chunk_coord(pos));
        self.sync_generated_empty_chunk_marker(World::world_to_chunk_coord(landing));
        self.notify_fluid_on_block_change(pos, BlockId::Sand, BlockId::Air);
        let _ = self.stage_block_edit_dirty_after_block_change(pos);
        self.run_incremental_block_change_lighting(
            &[LightingBlockChange {
                pos,
                old: BlockId::Sand,
                new: BlockId::Air,
            }],
            "sand_gravity_source",
        );
        self.notify_fluid_on_block_change(landing, replaced, BlockId::Sand);
        let _ = self.stage_block_edit_dirty_after_block_change(landing);
        self.run_incremental_block_change_lighting(
            &[LightingBlockChange {
                pos: landing,
                old: replaced,
                new: BlockId::Sand,
            }],
            "sand_gravity_landing",
        );
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

    fn dispatch_dirty_chunk_meshes_pass(
        &mut self,
        camera: &Camera,
        budget_override_ms: Option<f32>,
    ) -> usize {
        let _trace_scope = perf_trace::scope("state.dispatch_dirty_chunk_meshes_pass");
        let (center_chunk_x, center_chunk_z, active_radius) =
            if let Some(task) = self.world_generation_task.as_ref() {
                (
                    task.warmup_center_x,
                    task.warmup_center_z,
                    task.startup_plan.radius_chunks,
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
        let mut dispatched_jobs = 0usize;

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
            self.queue_missing_meshes_in_radius(
                center_chunk_x,
                center_chunk_y,
                center_chunk_z,
                self.radius_offsets_cache.len(),
            );
        }
        self.reprioritize_dirty_chunk_queue(
            center_chunk_x,
            center_chunk_y,
            center_chunk_z,
            &frustum,
            camera,
        );
        let dispatch_budget_ms = budget_override_ms.unwrap_or_else(|| {
            self.stream_budget_for_stage_ms(
                STREAM_DISPATCH_BUDGET_MS,
                STREAM_BURST_DISPATCH_BUDGET_MS,
            )
        });
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
                        self.requeue_existing_dirty_chunk(coord);
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
                    self.uploaded_final_lighting_chunks.remove(&coord);
                    continue;
                }

                let force_warmup_mesh = self.is_worldgen_warmup_coord(coord);

                if !force_warmup_mesh && !chunk_in_frustum(&frustum, coord) {
                    self.fluid_dirty_due_to_fluids.remove(&coord);
                    self.frustum_culled_chunks_total += 1;
                    self.meshes_skipped_due_to_frustum += 1;
                    self.deferred_mesh_until_frame.insert(
                        coord,
                        self.frame_index.saturating_add(MESH_FRUSTUM_DEFER_FRAMES),
                    );
                    let started = Instant::now();
                    self.requeue_existing_dirty_chunk(coord);
                    dispatch_requeue_dirty_ms += started.elapsed().as_secs_f32() * 1000.0;
                    continue;
                }

                if self.inflight.contains_key(&coord) {
                    let started = Instant::now();
                    self.requeue_existing_dirty_chunk(coord);
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
                    self.uploaded_final_lighting_chunks.remove(&coord);
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
                    self.uploaded_final_lighting_chunks.remove(&coord);
                    self.full_solid_mesh_skip_set.remove(&coord);
                    if had_mesh {
                        let started = Instant::now();
                        for neighbor in Self::neighbor_chunk_coords(coord) {
                            self.mark_chunk_boundary_dirty(neighbor);
                        }
                        dispatch_requeue_dirty_ms += started.elapsed().as_secs_f32() * 1000.0;
                    }
                    self.mesh_schedule.remove(&coord);
                    continue;
                };

                let dispatch_reasons = self.begin_mesh_dispatch(coord, revision);
                perf_trace::instant_event(
                    "mesh.dispatch",
                    &format!(
                        "coord=({}, {}, {}) revision={} state={} reasons={} reason_bits={} queue_len={} inflight={}",
                        coord.x,
                        coord.y,
                        coord.z,
                        revision,
                        ChunkMeshState::InFlight.as_str(),
                        dispatch_reasons.labels(),
                        dispatch_reasons.bits(),
                        self.dirty_chunk_queue.len(),
                        self.inflight.len()
                    ),
                );
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
                    dispatched_jobs += 1;
                    self.fluid_dirty_due_to_fluids.remove(&coord);
                    self.deferred_mesh_until_frame.remove(&coord);
                    if let Some(entry) = self.fluid_mesh_coalesce.get_mut(&coord) {
                        entry.pending_dirty = false;
                    }
                } else {
                    let started = Instant::now();
                    self.inflight.remove(&coord);
                    self.finish_mesh_dispatch(coord, Some(dispatch_reasons));
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
        dispatched_jobs
    }

    fn run_startup_streaming_burst(&mut self, camera: &Camera) {
        if self.world_generation_task.is_none() {
            return;
        }
        let _trace_scope = perf_trace::scope("state.run_startup_streaming_burst");
        let started = Instant::now();
        let mut passes = 0usize;
        let mut total_chunkgen_drained = 0usize;
        let mut total_followup_steps = 0usize;
        let mut total_dispatched = 0usize;
        let mut total_mesh_result_drained = 0usize;
        let mut total_mesh_uploads = 0usize;

        while passes < STARTUP_STREAMING_BURST_INTERNAL_PASSES {
            let elapsed_ms = started.elapsed().as_secs_f32() * 1000.0;
            let remaining_ms = (STARTUP_STREAMING_BURST_FRAME_BUDGET_MS - elapsed_ms).max(0.0);
            if remaining_ms <= 0.0 {
                break;
            }

            passes += 1;
            let chunkgen_drained = self.pump_chunkgen_results();
            let followup_steps = self.pump_chunkgen_apply_followups();
            let dispatch_budget_ms = STARTUP_MESH_DISPATCH_BUDGET_MS.min(remaining_ms);
            let dispatched = if dispatch_budget_ms > 0.0 {
                self.dispatch_dirty_chunk_meshes_pass(camera, Some(dispatch_budget_ms))
            } else {
                0
            };
            let elapsed_ms = started.elapsed().as_secs_f32() * 1000.0;
            let remaining_ms = (STARTUP_STREAMING_BURST_FRAME_BUDGET_MS - elapsed_ms).max(0.0);
            let mesh_progress = if remaining_ms > 0.0 {
                self.pump_mesh_results_with_limits(
                    camera,
                    STARTUP_MESH_RESULTS_DRAIN_PER_PASS,
                    Some(STARTUP_MESH_UPLOAD_BUDGET_MS.min(remaining_ms)),
                )
            } else {
                MeshPumpProgress::default()
            };

            total_chunkgen_drained += chunkgen_drained;
            total_followup_steps += followup_steps;
            total_dispatched += dispatched;
            total_mesh_result_drained += mesh_progress.drained_results;
            total_mesh_uploads += mesh_progress.uploaded_meshes;

            if chunkgen_drained == 0
                && followup_steps == 0
                && dispatched == 0
                && !mesh_progress.did_work()
            {
                break;
            }
        }

        perf_trace::instant_event(
            "startup_streaming.burst",
            &format!(
                "passes={} elapsed_ms={:.2} chunkgen_drained={} followup_steps={} dispatched={} mesh_results_drained={} mesh_uploads={}",
                passes,
                started.elapsed().as_secs_f32() * 1000.0,
                total_chunkgen_drained,
                total_followup_steps,
                total_dispatched,
                total_mesh_result_drained,
                total_mesh_uploads
            ),
        );
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
            let _scope = perf_trace::scope("proc.pump_chunkgen_apply_followups");
            let started = Instant::now();
            self.pump_chunkgen_apply_followups();
            let ms = started.elapsed().as_secs_f32() * 1000.0;
            perf_trace::record_duration_ms("proc.pump_chunkgen_apply_followups", ms, "");
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
            let _stream_scope = perf_trace::scope("render.streaming.ensure_columns").with_counts(
                format!(
                    "view_chunks={} inflight_chunkgen={} inflight_mesh={} saved_inflight={} saved_q={}",
                    self.view_distance_chunks,
                    self.chunkgen_inflight.len(),
                    self.inflight.len(),
                    self.saved_column_loads_inflight.len(),
                    self.storage_loader.queue_len_approx()
                ),
            );
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
            let _lighting_scope = perf_trace::scope("render.lighting.step").with_counts(format!(
                "pending_dirty={} req_pending={} inflight={} result_q={} stream_backlog={} remove_backlog={}",
                self.pending_lighting_dirty_chunks.len(),
                self.lighting_request_pending as u8,
                self.lighting_job_inflight as u8,
                self.lighting_results.len(),
                self.pending_chunk_added_lighting_jobs.len(),
                self.pending_removed_lighting_batches.len()
            ));
            self.rebuild_world_lighting_if_dirty();
            let ms = started.elapsed().as_secs_f32() * 1000.0;
            self.last_lighting_step_ms = ms;
            self.frame_perf.proc_rebuild_world_lighting_if_dirty_ms += ms;
            perf_trace::record_duration_ms("proc.rebuild_world_lighting_if_dirty", ms, "");
        }
        self.dispatch_dirty_chunk_meshes_pass(camera, None);
        self.run_startup_streaming_burst(camera);

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

    fn pump_mesh_results_with_limits(
        &mut self,
        camera: &Camera,
        result_drain_limit: usize,
        budget_override_ms: Option<f32>,
    ) -> MeshPumpProgress {
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
        while drained < result_drain_limit {
            let Ok(result) = self.mesh_results.try_recv() else {
                break;
            };
            self.mesh_upload_queue.push_back(QueuedMeshUpload {
                result,
                queued_at: Instant::now(),
            });
            drained += 1;
        }
        let frustum = self.build_frustum_for_camera(camera);
        self.reprioritize_mesh_upload_queue(camera, &frustum);
        self.frame_perf.mesh_results_drained += drained;
        self.frame_perf.mesh_results_backlog_after = self
            .frame_perf
            .mesh_results_backlog_after
            .max(self.mesh_results.len());

        let mut processed = 0usize;
        let mut apply_mesh_time = Duration::ZERO;
        let mut upload_stats = MeshUploadStats::default();
        let (mesh_upload_budget_ms, urgent_near_uploads) =
            if let Some(budget_ms) = budget_override_ms {
                let (center_chunk_x, center_chunk_y, center_chunk_z) =
                    Self::camera_chunk_coords(camera);
                (
                    budget_ms,
                    self.count_urgent_near_mesh_uploads(
                        center_chunk_x,
                        center_chunk_y,
                        center_chunk_z,
                        &frustum,
                    ),
                )
            } else {
                self.mesh_upload_budget_for_camera(camera, &frustum)
            };
        self.frame_perf.stream_mesh_upload_budget_ms = self
            .frame_perf
            .stream_mesh_upload_budget_ms
            .max(mesh_upload_budget_ms);
        let mesh_upload_loop_started = Instant::now();
        let mut upload_loop_iterations = 0usize;
        if urgent_near_uploads > 0 || self.frame_perf.mesh_upload_budget_boost_ms > 0.0 {
            perf_trace::instant_event(
                "mesh_upload.budget",
                &format!(
                    "queue={} urgent_near={} budget_ms={:.2} boost_ms={:.2} speed={:.2} warmup={}",
                    self.mesh_upload_queue.len(),
                    urgent_near_uploads,
                    mesh_upload_budget_ms,
                    self.frame_perf.mesh_upload_budget_boost_ms,
                    self.stream_camera_speed_blocks_per_sec,
                    self.world_generation_task.is_some()
                ),
            );
        }
        let _upload_scope = perf_trace::scope("render.upload.meshes").with_counts(format!(
            "queue={} inflight={} urgent_near={} budget_ms={:.2} speed={:.2}",
            self.mesh_upload_queue.len(),
            self.inflight.len(),
            urgent_near_uploads,
            mesh_upload_budget_ms,
            self.stream_camera_speed_blocks_per_sec
        ));

        let (center_chunk_x, center_chunk_y, center_chunk_z) = Self::camera_chunk_coords(camera);
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
            let Some(queued) = self.mesh_upload_queue.pop_front() else {
                break;
            };
            processed += 1;
            let wait_dur = queued.queued_at.elapsed();
            let wait_ms = wait_dur.as_secs_f32() * 1000.0;
            self.frame_perf.mesh_upload_wait_samples += 1;
            self.frame_perf.mesh_upload_wait_total_ms += wait_ms;
            self.frame_perf.mesh_upload_wait_max_ms =
                self.frame_perf.mesh_upload_wait_max_ms.max(wait_ms);
            let result = queued.result;
            let is_near_priority = Self::mesh_upload_near_rank(
                result.coord,
                center_chunk_x,
                center_chunk_y,
                center_chunk_z,
            ) <= 1;
            let is_near_visible = is_near_priority
                && self.mesh_upload_is_near_urgent(
                    result.coord,
                    center_chunk_x,
                    center_chunk_y,
                    center_chunk_z,
                    &frustum,
                );

            if result.mesh_debug_epoch != self.mesh_debug_epoch {
                if self.inflight.get(&result.coord).copied() == Some(result.revision) {
                    self.inflight.remove(&result.coord);
                    let retry_reasons = self
                        .mesh_schedule
                        .get(&result.coord)
                        .map(|entry| entry.inflight_reasons)
                        .unwrap_or(MeshDirtyReasons::GEOMETRY);
                    self.finish_mesh_dispatch(result.coord, Some(retry_reasons));
                }
                continue;
            }

            let expected_revision = match self.inflight.remove(&result.coord) {
                Some(revision) => revision,
                None => continue,
            };
            if expected_revision != result.revision {
                let retry_reasons = self
                    .mesh_schedule
                    .get(&result.coord)
                    .map(|entry| entry.inflight_reasons)
                    .unwrap_or(MeshDirtyReasons::GEOMETRY);
                self.finish_mesh_dispatch(result.coord, Some(retry_reasons));
                continue;
            }

            let current_revision = self
                .world
                .get_chunk(result.coord)
                .map(|chunk| chunk.revision);
            if current_revision != Some(result.revision) {
                let retry_reasons = self
                    .mesh_schedule
                    .get(&result.coord)
                    .map(|entry| entry.inflight_reasons)
                    .unwrap_or(MeshDirtyReasons::GEOMETRY);
                self.finish_mesh_dispatch(result.coord, Some(retry_reasons));
                continue;
            }

            let had_mesh = self.chunk_meshes.contains_key(&result.coord);
            let lighting_progress = self.chunk_lighting_progress.get(&result.coord).cloned();
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
            self.finish_mesh_dispatch(result.coord, None);
            if !has_mesh {
                self.uploaded_final_lighting_chunks.remove(&result.coord);
            }

            perf_trace::record_duration(
                "mesh_upload.queue_wait",
                wait_dur,
                &format!(
                    "coord=({}, {}, {}) near_visible={} had_mesh={} has_mesh={}",
                    result.coord.x,
                    result.coord.y,
                    result.coord.z,
                    is_near_visible,
                    had_mesh,
                    has_mesh
                ),
            );

            if !had_mesh && has_mesh && is_near_visible {
                self.frame_perf.mesh_upload_near_first_visible_count += 1;
                self.frame_perf.mesh_upload_near_first_visible_total_ms += wait_ms;
                self.frame_perf.mesh_upload_near_first_visible_max_ms = self
                    .frame_perf
                    .mesh_upload_near_first_visible_max_ms
                    .max(wait_ms);
                perf_trace::record_duration(
                    "mesh_upload.near_first_visible",
                    wait_dur,
                    &format!(
                        "coord=({}, {}, {}) lighting={}",
                        result.coord.x,
                        result.coord.y,
                        result.coord.z,
                        lighting_progress
                            .as_ref()
                            .map(|p| p.state.as_str())
                            .unwrap_or("unknown")
                    ),
                );
            }

            let lighting_finalized = lighting_progress
                .as_ref()
                .is_some_and(|progress| progress.state == ChunkLightingReadiness::Finalized);
            if has_mesh
                && lighting_finalized
                && self.uploaded_final_lighting_chunks.insert(result.coord)
            {
                let final_ready_dur = lighting_progress
                    .as_ref()
                    .map(|progress| Instant::now().saturating_duration_since(progress.started_at))
                    .unwrap_or(wait_dur);
                let final_ready_ms = final_ready_dur.as_secs_f32() * 1000.0;
                if is_near_visible {
                    self.frame_perf.mesh_upload_near_final_ready_count += 1;
                    self.frame_perf.mesh_upload_near_final_ready_total_ms += final_ready_ms;
                    self.frame_perf.mesh_upload_near_final_ready_max_ms = self
                        .frame_perf
                        .mesh_upload_near_final_ready_max_ms
                        .max(final_ready_ms);
                    perf_trace::record_duration(
                        "mesh_upload.near_final_ready",
                        final_ready_dur,
                        &format!(
                            "coord=({}, {}, {}) wait_ms={:.2}",
                            result.coord.x, result.coord.y, result.coord.z, wait_ms
                        ),
                    );
                }
                if had_mesh {
                    self.frame_perf.mesh_upload_finalized_upgrades += 1;
                }
            }

            if had_mesh != has_mesh {
                for neighbor in Self::neighbor_chunk_coords(result.coord) {
                    self.mark_chunk_boundary_dirty(neighbor);
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
            crate::console_error!(
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
        MeshPumpProgress {
            drained_results: drained,
            uploaded_meshes: processed,
        }
    }

    pub(crate) fn pump_mesh_results(&mut self, camera: &Camera) -> MeshPumpProgress {
        self.pump_mesh_results_with_limits(camera, MESH_RESULTS_DRAIN_PER_FRAME, None)
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
        let active_cols = self.active_sim_columns.len();
        if self.sim_system_last_frame_ms.len() < self.sim_systems.len() {
            self.sim_system_last_frame_ms
                .resize(self.sim_systems.len(), 0.0);
        }
        if self.sim_system_last_frame_ticks.len() < self.sim_systems.len() {
            self.sim_system_last_frame_ticks
                .resize(self.sim_systems.len(), 0);
        }
        for idx in 0..self.sim_systems.len() {
            let system_name = self.sim_systems[idx].name();
            let started = Instant::now();
            let _scope =
                perf_trace::scope(format!("sim.fixed_tick.{system_name}")).with_counts(format!(
                    "tick={} active_cols={} dt_ms={:.3}",
                    tick_index,
                    active_cols,
                    tick_dt * 1000.0
                ));
            self.sim_systems[idx].fixed_tick(
                &mut self.world,
                tick_dt,
                tick_index,
                ActiveSimColumns {
                    is_active: active.is_active,
                },
            );
            let elapsed_ms = started.elapsed().as_secs_f32() * 1000.0;
            self.sim_system_last_frame_ms[idx] += elapsed_ms;
            self.sim_system_last_frame_ticks[idx] =
                self.sim_system_last_frame_ticks[idx].saturating_add(1);
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

    pub(crate) fn worldgen_features_debug_line(&self) -> String {
        let c = self.worldgen_feature_debug_counts;
        format!(
            "Worldgen: chunks={} last W/src/sand={}/{}/{} total W/src/sand={}/{}/{}",
            c.chunks_applied,
            c.last_worldgen_water_blocks,
            c.last_worldgen_water_source_blocks,
            c.last_shoreline_sand_blocks,
            c.total_worldgen_water_blocks,
            c.total_worldgen_water_source_blocks,
            c.total_shoreline_sand_blocks
        )
    }

    pub(crate) fn fluid_debug_under_crosshair_line(&self, camera: &Camera) -> String {
        let Some(hit) = raycast_world(
            &self.world,
            camera.position,
            camera.forward(),
            RAYCAST_MAX_DISTANCE,
        ) else {
            return "Crosshair fluid: no hit".to_string();
        };
        let pos = hit.hit_block_pos;
        let block = self.world.get_block(pos);
        let data = self.world.get_block_data(pos);
        let Some(kind) = fluid_kind(block) else {
            return format!(
                "Crosshair fluid: block={} pos=({}, {}, {}) data=0x{data:02X} (not fluid)",
                block.display_name(),
                pos.x,
                pos.y,
                pos.z
            );
        };
        format!(
            "Crosshair fluid: kind={kind:?} pos=({}, {}, {}) level={} falling={} source_origin={} worldgen_static={} data=0x{data:02X}",
            pos.x,
            pos.y,
            pos.z,
            fluid_data_level(data),
            fluid_data_is_falling(data),
            fluid_data_is_source_origin(data),
            fluid_data_is_worldgen_static(data)
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

    pub(crate) fn destroy_block_at_camera_ray(&mut self, camera: &Camera, player_mode: PlayerMode) {
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
        let is_breakable = if destroyed_block == BlockId::Bedrock {
            matches!(player_mode, PlayerMode::Creative)
        } else {
            self.block_texture_lookup.is_breakable_for(destroyed_block)
        };
        if !is_breakable {
            maybe_log_slow_block_edit(
                "destroy",
                edit_started.elapsed().as_secs_f32() * 1000.0,
                slow_coord,
                timings,
            );
            return;
        }
        if cfg!(debug_assertions) {
            crate::console_info!(
                "[debug] destroy: block={:?} pos=({}, {}, {})",
                destroyed_block,
                hit.hit_block_pos.x,
                hit.hit_block_pos.y,
                hit.hit_block_pos.z
            );
        }

        let chunk_coord = World::world_to_chunk_coord(hit.hit_block_pos);
        let chunk_existed_before = self.world.get_chunk(chunk_coord).is_some();
        let set_ok = {
            let _scope = perf_trace::scope("block_edit.set_block");
            let started = Instant::now();
            let result =
                self.world
                    .set_block_and_data_loaded_only(hit.hit_block_pos, BlockId::Air, 0);
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
            self.mark_chunk_user_only_if_unknown(chunk_coord);
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

    pub(crate) fn pick_block_at_camera_ray(&self, camera: &Camera) -> Option<BlockId> {
        let hit = raycast_world(
            &self.world,
            camera.position,
            camera.forward(),
            RAYCAST_MAX_DISTANCE,
        )?;
        Some(self.world.get_block(hit.hit_block_pos))
    }

    fn block_intersects_player_hitbox(
        block_pos: IVec3,
        block: BlockId,
        player_feet_pos: Vec3,
    ) -> bool {
        let player_min = player_feet_pos + Vec3::new(-PLAYER_RADIUS, 0.0, -PLAYER_RADIUS);
        let player_max = player_feet_pos + Vec3::new(PLAYER_RADIUS, PLAYER_HEIGHT, PLAYER_RADIUS);

        let Some((block_min, block_max)) = block.collision_shape().world_bounds(block_pos) else {
            return false;
        };

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
        if Self::block_intersects_player_hitbox(place_pos, selected_block, player_feet_pos) {
            maybe_log_slow_block_edit(
                "place",
                edit_started.elapsed().as_secs_f32() * 1000.0,
                slow_coord,
                timings,
            );
            return;
        }
        if cfg!(debug_assertions) {
            crate::console_info!(
                "[debug] place: block={:?} pos=({}, {}, {})",
                selected_block,
                place_pos.x,
                place_pos.y,
                place_pos.z
            );
        }

        let (block_to_place, block_data, deferred_kind) =
            if let Some(kind) = fluid_kind(selected_block) {
                (
                    make_still(kind),
                    fluid_data_make(0, false, true),
                    DeferredEditKind::PlaceFluid,
                )
            } else {
                (selected_block, 0, DeferredEditKind::PlaceBlock)
            };
        let chunk_coord = World::world_to_chunk_coord(place_pos);
        let chunk_existed_before = self.world.get_chunk(chunk_coord).is_some();
        let set_result = {
            let _scope = perf_trace::scope("block_edit.set_block");
            let started = Instant::now();
            let result =
                self.world
                    .set_block_and_data_loaded_only(place_pos, block_to_place, block_data);
            timings.set_block_ms = started.elapsed().as_secs_f32() * 1000.0;
            result
        };
        match set_result {
            Ok(()) => {
                let chunk_existed_after = self.world.get_chunk(chunk_coord).is_some();
                let chunk_removed = chunk_existed_before && !chunk_existed_after;
                let chunk_created = !chunk_existed_before && chunk_existed_after;
                perf_trace::record_duration_ms(
                    "block_edit.set_block",
                    timings.set_block_ms,
                    &format!(
                        "kind=place ok=1 chunk=({}, {}, {}) created={} removed={}",
                        chunk_coord.x,
                        chunk_coord.y,
                        chunk_coord.z,
                        chunk_created as u8,
                        chunk_removed as u8
                    ),
                );
                self.mark_chunk_user_only_if_unknown(chunk_coord);
                self.sync_generated_empty_chunk_marker(chunk_coord);
                self.enqueue_pending_block_edit(place_pos, target_block, block_to_place);
            }
            Err(SetBlockError::ChunkNotLoaded) => {
                self.defer_block_edit(DeferredBlockEdit {
                    world_pos: place_pos,
                    block: block_to_place,
                    data: block_data,
                    frame_index: self.frame_index,
                    kind: deferred_kind,
                });
                if self.generated_empty_chunks.contains(&chunk_coord)
                    && self.has_deferred_edits(chunk_coord)
                {
                    let _ = self.materialize_known_empty_chunk_for_edits(chunk_coord);
                } else {
                    self.ensure_chunk_available_for_deferred_edit(chunk_coord);
                }
                let chunk_available_after = self.world.get_chunk(chunk_coord).is_some();
                perf_trace::record_duration_ms(
                    "block_edit.set_block",
                    timings.set_block_ms,
                    &format!(
                        "kind=place_deferred ok=1 chunk=({}, {}, {}) created={} removed=0",
                        chunk_coord.x, chunk_coord.y, chunk_coord.z, chunk_available_after as u8
                    ),
                );
            }
            Err(SetBlockError::OutOfBounds) => {
                perf_trace::record_duration_ms(
                    "block_edit.set_block",
                    timings.set_block_ms,
                    &format!(
                        "kind=place ok=0 chunk=({}, {}, {}) err=out_of_bounds",
                        chunk_coord.x, chunk_coord.y, chunk_coord.z
                    ),
                );
                maybe_log_slow_block_edit(
                    "place",
                    edit_started.elapsed().as_secs_f32() * 1000.0,
                    slow_coord,
                    timings,
                );
                return;
            }
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
        self.outline_target = None;
    }

    fn sky_clear_color_rgb(&self) -> [f32; 3] {
        let t = self.world_time.sky_brightness.clamp(0.0, 1.0);
        let day = [0.52, 0.72, 1.00];
        let night = [0.0, 0.0, 0.0];
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

    fn write_menu_panorama_vertices(&mut self, camera: &Camera) {
        let t = self.menu_panorama_started_at.elapsed().as_secs_f32();
        let panorama_rot = Mat3::from_rotation_y(t * MENU_PANORAMA_YAW_SPEED_RAD_PER_SEC)
            * Mat3::from_rotation_x(MENU_PANORAMA_PITCH_RAD);
        let base_right = camera.right_flat();
        let base_forward = camera.forward_flat();
        let base_up = Vec3::Y;
        let camera_pos = camera.position;
        let half_size = MENU_PANORAMA_CUBE_SIZE * 0.5;
        let face_bases = [
            (Vec3::Z, Vec3::X, Vec3::Y),
            (Vec3::X, -Vec3::Z, Vec3::Y),
            (-Vec3::Z, -Vec3::X, Vec3::Y),
            (-Vec3::X, Vec3::Z, Vec3::Y),
            (Vec3::Y, Vec3::X, -Vec3::Z),
            (-Vec3::Y, Vec3::X, Vec3::Z),
        ];
        let map_local = |v: Vec3| base_right * v.x + base_up * v.y + base_forward * v.z;
        let mut vertices = [Vertex {
            position: [0.0, 0.0, 0.0],
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: SKY_TEXTURE_MODE_DEFAULT,
            light_packed: FULLBRIGHT_LIGHT_PACKED,
            color: [1.0, 1.0, 1.0],
        }; MENU_PANORAMA_VERTEX_COUNT];

        for face_index in 0..MENU_PANORAMA_FACE_COUNT {
            let uv = self.menu_panorama_uv_rects[face_index];
            let quad_uvs = [
                [uv[0], uv[3]],
                [uv[2], uv[3]],
                [uv[2], uv[1]],
                [uv[0], uv[1]],
            ];
            let (face_center, face_right, face_up) = face_bases[face_index];
            let center = camera_pos + map_local(panorama_rot * (face_center * half_size));
            let right = map_local(panorama_rot * (face_right * half_size));
            let up = map_local(panorama_rot * (face_up * half_size));
            let quad = [
                center - right - up,
                center + right - up,
                center + right + up,
                center - right + up,
            ];
            for vertex_index in 0..4 {
                vertices[face_index * 4 + vertex_index] = Vertex {
                    position: quad[vertex_index].to_array(),
                    uv: quad_uvs[vertex_index],
                    tex_layer: self.menu_panorama_layers[face_index],
                    tint_kind: SKY_TEXTURE_MODE_DEFAULT,
                    light_packed: FULLBRIGHT_LIGHT_PACKED,
                    color: [1.0, 1.0, 1.0],
                };
            }
        }

        self.queue.write_buffer(
            &self.menu_panorama_vertex_buffer,
            0,
            bytemuck::cast_slice(&vertices),
        );
    }

    fn write_sky_object_vertices(&mut self, camera: &Camera) {
        let sky_rot = Mat4::from_rotation_y(-std::f32::consts::FRAC_PI_2)
            * Mat4::from_rotation_x(self.world_time.celestial_angle * std::f32::consts::TAU);
        let camera_pos = camera.position;
        let billboard_right = camera.right();
        let billboard_up = billboard_right.cross(camera.forward()).normalize_or_zero();
        let moon_uv = self.moon_phase_uv_rect();
        let sun_color = [self.world_time.sun_brightness; 3];
        let moon_strength = (1.0 - self.world_time.sky_brightness).clamp(0.15, 1.0);
        let moon_color = [moon_strength; 3];
        let star_visibility = (1.0 - self.world_time.sky_brightness)
            .clamp(0.0, 1.0)
            .powf(2.2);

        let mut vertices = [Vertex {
            position: [0.0, 0.0, 0.0],
            uv: [0.0, 0.0],
            tex_layer: 0,
            tint_kind: SKY_TEXTURE_MODE_DEFAULT,
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
            let center =
                camera_pos + (sky_rot * Vec3::new(0.0, local_y, 0.0).extend(1.0)).truncate();
            let local = [
                center - billboard_right * hs - billboard_up * hs,
                center + billboard_right * hs - billboard_up * hs,
                center + billboard_right * hs + billboard_up * hs,
                center - billboard_right * hs + billboard_up * hs,
            ];
            let quad_uvs = [
                [uv[0], uv[3]],
                [uv[2], uv[3]],
                [uv[2], uv[1]],
                [uv[0], uv[1]],
            ];
            for i in 0..4 {
                vertices[base + i] = Vertex {
                    position: local[i].to_array(),
                    uv: quad_uvs[i],
                    tex_layer,
                    tint_kind: SKY_TEXTURE_MODE_DEFAULT,
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

        if star_visibility > 0.001 {
            let star_uvs = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];
            for (star_index, star) in self.sky_stars.iter().enumerate() {
                let rotated_dir = (sky_rot * star.direction.extend(0.0))
                    .truncate()
                    .normalize_or_zero();
                if rotated_dir.length_squared() <= 0.0 {
                    continue;
                }
                let center = camera_pos + rotated_dir * SKY_STAR_DISTANCE;
                let tangent_hint = if rotated_dir.y.abs() > 0.92 {
                    Vec3::X
                } else {
                    Vec3::Y
                };
                let right = rotated_dir.cross(tangent_hint).normalize_or_zero();
                let up = right.cross(rotated_dir).normalize_or_zero();
                let hs = star.size * 0.5;
                let quad = [
                    center - right * hs - up * hs,
                    center + right * hs - up * hs,
                    center + right * hs + up * hs,
                    center - right * hs + up * hs,
                ];
                let color = [star.brightness * star_visibility; 3];
                let base = SKY_BASE_OBJECT_VERTEX_COUNT + star_index * 4;
                for i in 0..4 {
                    vertices[base + i] = Vertex {
                        position: quad[i].to_array(),
                        uv: star_uvs[i],
                        tex_layer: self.sky_star_layer,
                        tint_kind: SKY_TEXTURE_MODE_DEFAULT,
                        light_packed: FULLBRIGHT_LIGHT_PACKED,
                        color,
                    };
                }
            }
        }

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

        self.outline_target = Some(OutlineTarget {
            min: hit.hit_bounds_min,
            max: hit.hit_bounds_max,
        });
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
                let boundary_fog_end =
                    (inner + BOUNDARY_OCEAN_OUTER_PADDING_BLOCKS * 0.55).max(32.0);
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
        menu_overlay: Option<MenuOverlay<'_>>,
        hotbar: &[Option<BlockId>; HUD_HOTBAR_SLOT_COUNT],
        selected_hotbar_slot: usize,
        inventory_overlay: Option<InventoryOverlay<'_>>,
        debug_overlay: Option<DebugOverlay<'_>>,
        chat_overlay: Option<ChatOverlay<'_>>,
    ) -> Result<(), SurfaceError> {
        let _trace_scope = perf_trace::scope("state.render");
        let render_started = Instant::now();
        let frame = self.surface.get_current_texture()?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let showing_generation_screen = generation_overlay.is_some();
        let showing_main_menu_panorama = menu_overlay.is_some_and(|overlay| overlay.show_panorama);
        let draw_world_scene = !showing_generation_screen && !showing_main_menu_panorama;
        let (camera_chunk_x, _, camera_chunk_z) = Self::camera_chunk_coords(camera);
        let mut visible_lod_columns: Vec<(LodColumnCoord, f32)> = Vec::with_capacity(256);
        let mut visible_chunks: Vec<(ChunkCoord, f32)> =
            Vec::with_capacity(self.chunk_meshes.len());
        let mut transparent_chunks = Vec::new();
        if draw_world_scene {
            let aspect = (self.config.width as f32 / self.config.height as f32).max(0.0001);
            let frustum = Frustum::from_view_proj(build_view_proj_wgpu(camera, aspect));
            let near_mesh_radius = self.near_mesh_radius_chunks();
            let (lod_near_radius, lod_far_radius) = self.lod_ring_radii_chunks();
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
            transparent_chunks = visible_chunks.clone();
            transparent_chunks.sort_by(|(coord_a, dist_a), (coord_b, dist_b)| {
                dist_b
                    .total_cmp(dist_a)
                    .then_with(|| coord_b.x.cmp(&coord_a.x))
                    .then_with(|| coord_b.y.cmp(&coord_a.y))
                    .then_with(|| coord_b.z.cmp(&coord_a.z))
            });
        }
        let near_mesh_memory_bytes = self.total_near_mesh_memory_bytes();
        let far_lod_memory_bytes = self.total_lod_mesh_memory_bytes();
        self.frame_perf.near_visible_chunks = visible_chunks.len();
        self.frame_perf.far_lod_visible_columns = visible_lod_columns.len();
        self.frame_perf.near_mesh_memory_bytes = near_mesh_memory_bytes;
        self.frame_perf.far_lod_memory_bytes = far_lod_memory_bytes;
        let mut lod_drawcalls = 0usize;
        let mut world_drawcalls = 0usize;

        let particle_mesh = if !draw_world_scene || self.particles.is_empty() {
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
        let ui_mesh_scope = perf_trace::scope("render.build_ui_meshes").with_counts(format!(
            "gen={} menu={} inv={} debug={} chat={} hotbar_slots={}",
            generation_overlay.is_some() as u8,
            menu_overlay.is_some() as u8,
            inventory_overlay.is_some() as u8,
            debug_overlay.is_some() as u8,
            chat_overlay.is_some() as u8,
            hotbar.len()
        ));
        let generating_overlay_mesh = if let Some(overlay) = generation_overlay {
            let (vertices, indices) = build_generating_screen_mesh(
                self.config.width,
                self.config.height,
                self.worldgen_dirt_layer,
                &self.worldgen_font,
                overlay.title,
                overlay.progress01,
                overlay.phase,
                overlay.stats,
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
        let menu_overlay_mesh = if let Some(overlay) = menu_overlay {
            let (vertices, indices) = build_menu_overlay_mesh(
                self.config.width,
                self.config.height,
                self.worldgen_dirt_layer,
                &self.worldgen_font,
                &self.menu_button_atlas,
                overlay.show_panorama,
                overlay.title,
                overlay.selected,
                overlay.pressed,
                overlay.items,
                overlay.enabled,
                overlay.footer,
            );
            let vertex_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Menu Overlay Vertex Buffer"),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            let index_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Menu Overlay Index Buffer"),
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
        self.block_select_overlay_mesh = None;
        self.block_select_overlay_cache = None;
        let showing_inventory = if let Some(overlay) = inventory_overlay {
            let cache_key = InventoryOverlayCacheKey {
                view_width: self.config.width,
                view_height: self.config.height,
                selected_palette: overlay.selected_palette,
                selected_hotbar_slot: overlay
                    .selected_hotbar_slot
                    .min(INVENTORY_SLOT_COUNT.saturating_sub(1)),
                hovered_palette: overlay.hovered_palette,
                hovered_hotbar_slot: overlay.hovered_hotbar_slot,
                page: overlay.page,
                palette_count: overlay.palette_blocks.len(),
            };
            if self.inventory_overlay_cache != Some(cache_key) {
                let (vertices, indices) = build_inventory_overlay_mesh(
                    self.config.width,
                    self.config.height,
                    &self.hud_atlas,
                    &self.inventory_atlas,
                    overlay,
                );
                let vertex_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("Inventory Overlay Vertex Buffer"),
                            contents: bytemuck::cast_slice(&vertices),
                            usage: wgpu::BufferUsages::VERTEX,
                        });
                let index_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("Inventory Overlay Index Buffer"),
                            contents: bytemuck::cast_slice(&indices),
                            usage: wgpu::BufferUsages::INDEX,
                        });
                self.inventory_overlay_mesh = Some(ChunkMesh {
                    vertex_buffer,
                    index_buffer,
                    index_count: indices.len() as u32,
                    vertex_capacity_bytes: std::mem::size_of_val(vertices.as_slice()) as u64,
                    index_capacity_bytes: std::mem::size_of_val(indices.as_slice()) as u64,
                });
                self.inventory_overlay_cache = Some(cache_key);
            }
            true
        } else {
            self.inventory_overlay_mesh = None;
            self.inventory_overlay_cache = None;
            false
        };
        let showing_pause_menu = menu_overlay_mesh.is_some();
        let allow_gameplay_ui =
            !showing_generation_screen && !showing_pause_menu && !showing_inventory;
        let debug_overlay_mesh = if allow_gameplay_ui {
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
        if allow_gameplay_ui {
            if let Some(overlay) = chat_overlay {
                let has_visible_lines = overlay.chat_open
                    || overlay
                        .recent_lines
                        .iter()
                        .any(|line| chat_line_alpha(false, line.age_seconds) > 0.01);
                if has_visible_lines {
                    let cache_key = ChatOverlayCacheKey {
                        view_width: self.config.width,
                        view_height: self.config.height,
                        signature: chat_overlay_signature(overlay),
                    };
                    if self.chat_overlay_cache != Some(cache_key) {
                        let (vertices, indices) = build_chat_overlay_mesh(
                            self.config.width,
                            self.config.height,
                            &self.worldgen_font,
                            overlay,
                        );
                        let vertex_buffer =
                            self.device
                                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                    label: Some("Chat Overlay Vertex Buffer"),
                                    contents: bytemuck::cast_slice(&vertices),
                                    usage: wgpu::BufferUsages::VERTEX,
                                });
                        let index_buffer =
                            self.device
                                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                    label: Some("Chat Overlay Index Buffer"),
                                    contents: bytemuck::cast_slice(&indices),
                                    usage: wgpu::BufferUsages::INDEX,
                                });
                        self.chat_overlay_mesh = Some(ChunkMesh {
                            vertex_buffer,
                            index_buffer,
                            index_count: indices.len() as u32,
                            vertex_capacity_bytes: std::mem::size_of_val(vertices.as_slice())
                                as u64,
                            index_capacity_bytes: std::mem::size_of_val(indices.as_slice()) as u64,
                        });
                        self.chat_overlay_cache = Some(cache_key);
                    }
                } else {
                    self.chat_overlay_mesh = None;
                    self.chat_overlay_cache = None;
                }
            } else {
                self.chat_overlay_mesh = None;
                self.chat_overlay_cache = None;
            }
        } else {
            self.chat_overlay_mesh = None;
            self.chat_overlay_cache = None;
        }
        let show_boundary_ocean = allow_gameplay_ui && self.world.config().boundary_ocean();
        let show_hotbar = allow_gameplay_ui;
        if show_hotbar {
            let cache_key = HotbarOverlayCacheKey {
                view_width: self.config.width,
                view_height: self.config.height,
                selected_slot: selected_hotbar_slot.min(HUD_HOTBAR_SLOT_COUNT.saturating_sub(1)),
            };
            if self.hotbar_overlay_cache != Some(cache_key) {
                let (vertices, indices) = build_hotbar_overlay_mesh(
                    self.config.width,
                    self.config.height,
                    &self.hud_atlas,
                    selected_hotbar_slot,
                );
                let vertex_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("Hotbar Overlay Vertex Buffer"),
                            contents: bytemuck::cast_slice(&vertices),
                            usage: wgpu::BufferUsages::VERTEX,
                        });
                let index_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("Hotbar Overlay Index Buffer"),
                            contents: bytemuck::cast_slice(&indices),
                            usage: wgpu::BufferUsages::INDEX,
                        });
                self.hotbar_overlay_mesh = Some(ChunkMesh {
                    vertex_buffer,
                    index_buffer,
                    index_count: indices.len() as u32,
                    vertex_capacity_bytes: std::mem::size_of_val(vertices.as_slice()) as u64,
                    index_capacity_bytes: std::mem::size_of_val(indices.as_slice()) as u64,
                });
                self.hotbar_overlay_cache = Some(cache_key);
            }
        } else {
            self.hotbar_overlay_mesh = None;
            self.hotbar_overlay_cache = None;
        }
        let show_first_person_overlay =
            !showing_generation_screen && !showing_pause_menu && !showing_inventory;
        let first_person_overlay_mesh = if show_first_person_overlay {
            let held_block =
                hotbar[selected_hotbar_slot.min(HUD_HOTBAR_SLOT_COUNT.saturating_sub(1))];
            let (vertices, indices) = build_first_person_overlay_mesh(
                camera,
                &self.block_texture_lookup,
                self.first_person_skin_layer,
                self.first_person_arm_tweak,
                held_block,
            );
            if indices.is_empty() {
                None
            } else {
                let vertex_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("First Person Overlay Vertex Buffer"),
                            contents: bytemuck::cast_slice(&vertices),
                            usage: wgpu::BufferUsages::VERTEX,
                        });
                let index_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("First Person Overlay Index Buffer"),
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
        } else {
            None
        };
        let outline_target = if showing_generation_screen || showing_pause_menu || showing_inventory
        {
            None
        } else {
            self.outline_target
        };
        if let Some(target) = outline_target {
            let outline_vertices = build_outline_line_vertices(target.min, target.max);
            self.queue.write_buffer(
                &self.outline_vertex_buffer,
                0,
                bytemuck::cast_slice(&outline_vertices),
            );
        }
        let item_preview_draws = if let Some(overlay) = inventory_overlay {
            collect_inventory_item_preview_draws(
                self.config.width,
                self.config.height,
                &self.inventory_atlas,
                overlay,
                hotbar,
            )
        } else if show_hotbar {
            collect_hotbar_item_preview_draws(
                self.config.width,
                self.config.height,
                &self.hud_atlas,
                hotbar,
            )
        } else {
            Vec::new()
        };
        let inventory_preview_draw = inventory_overlay.and_then(|overlay| {
            overlay
                .hovered_palette
                .and_then(|index| overlay.palette_blocks.get(index).copied())
                .or_else(|| {
                    overlay
                        .palette_blocks
                        .get(overlay.selected_palette)
                        .copied()
                })
                .or_else(|| overlay.palette_blocks.first().copied())
                .and_then(|block| {
                    inventory_preview_viewport_rect_px(
                        self.config.width,
                        self.config.height,
                        &self.inventory_atlas,
                    )
                    .map(|(viewport_x, viewport_y, viewport_w, viewport_h)| {
                        ItemPreviewDraw {
                            block,
                            viewport_x,
                            viewport_y,
                            viewport_w,
                            viewport_h,
                        }
                    })
                })
        });
        if inventory_preview_draw.is_some() || !item_preview_draws.is_empty() {
            let mut preview_blocks = HashSet::new();
            if let Some(draw) = inventory_preview_draw {
                preview_blocks.insert(draw.block);
            }
            for draw in &item_preview_draws {
                preview_blocks.insert(draw.block);
            }
            for block in preview_blocks {
                self.ensure_preview_mesh_cached(block);
            }
            self.update_preview_camera_uniform();
        }
        drop(ui_mesh_scope);
        if showing_main_menu_panorama {
            self.write_menu_panorama_vertices(camera);
        } else if draw_world_scene {
            self.write_sky_object_vertices(camera);
        }

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

            if showing_main_menu_panorama {
                rpass.set_pipeline(&self.sky_pipeline);
                rpass.set_bind_group(0, &self.sky_texture_bind_group, &[]);
                rpass.set_bind_group(1, &self.camera_bind_group, &[]);
                rpass.set_vertex_buffer(0, self.menu_panorama_vertex_buffer.slice(..));
                rpass.set_index_buffer(
                    self.menu_panorama_index_buffer.slice(..),
                    wgpu::IndexFormat::Uint32,
                );
                rpass.draw_indexed(0..MENU_PANORAMA_INDEX_COUNT, 0, 0..1);
            } else if draw_world_scene {
                rpass.set_pipeline(&self.sky_pipeline);
                rpass.set_bind_group(0, &self.sky_texture_bind_group, &[]);
                rpass.set_bind_group(1, &self.camera_bind_group, &[]);
                rpass.set_vertex_buffer(0, self.sky_object_vertex_buffer.slice(..));
                rpass.set_index_buffer(
                    self.sky_object_index_buffer.slice(..),
                    wgpu::IndexFormat::Uint32,
                );
                rpass.draw_indexed(0..SKY_OBJECT_INDEX_COUNT, 0, 0..1);

                {
                    let _opaque_scope =
                        perf_trace::scope("render.draw.opaque").with_counts(format!(
                            "chunks={} lod_cols={} far_lod={}",
                            visible_chunks.len(),
                            visible_lod_columns.len(),
                            self.far_lod_enabled as u8
                        ));
                    if self.far_lod_enabled {
                        // Erst Far-LOD, dann Near-Chunkmeshes.
                        rpass.set_pipeline(&self.render_pipeline_lod);
                        rpass.set_bind_group(0, &self.camera_bind_group, &[]);
                        for (coord, _) in &visible_lod_columns {
                            let Some(mesh) = self.lod_meshes.get(coord) else {
                                continue;
                            };
                            rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                            rpass.set_index_buffer(
                                mesh.index_buffer.slice(..),
                                wgpu::IndexFormat::Uint32,
                            );
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
                            rpass.set_index_buffer(
                                mesh.index_buffer.slice(..),
                                wgpu::IndexFormat::Uint32,
                            );
                            rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
                            world_drawcalls += 1;
                        }
                    }
                }
                {
                    let _cutout_scope = perf_trace::scope("render.draw.cutout")
                        .with_counts(format!("chunks={}", visible_chunks.len()));
                    // Leaves/plants should stay double-sided so canopy layers read naturally
                    // from below even when global backface culling is enabled for solid blocks.
                    rpass.set_pipeline(self.cutout_world_pipeline());
                    rpass.set_bind_group(0, &self.world_texture_bind_group, &[]);
                    rpass.set_bind_group(1, &self.camera_bind_group, &[]);
                    for (coord, _) in &visible_chunks {
                        let Some(layered) = self.chunk_meshes.get(coord) else {
                            continue;
                        };
                        if let Some(mesh) = &layered.cutout {
                            rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                            rpass.set_index_buffer(
                                mesh.index_buffer.slice(..),
                                wgpu::IndexFormat::Uint32,
                            );
                            rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
                            world_drawcalls += 1;
                        }
                    }
                }
                {
                    let _transparent_scope = perf_trace::scope("render.draw.transparent")
                        .with_counts(format!(
                            "chunks={} boundary_ocean={}",
                            transparent_chunks.len(),
                            show_boundary_ocean as u8
                        ));
                    rpass.set_pipeline(&self.render_pipeline_transparent);
                    rpass.set_bind_group(0, &self.world_texture_bind_group, &[]);
                    rpass.set_bind_group(1, &self.camera_bind_group, &[]);
                    if show_boundary_ocean {
                        if let Some(mesh) = self.boundary_ocean_mesh.as_ref() {
                            rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                            rpass.set_index_buffer(
                                mesh.index_buffer.slice(..),
                                wgpu::IndexFormat::Uint32,
                            );
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
                        rpass.set_index_buffer(
                            mesh.index_buffer.slice(..),
                            wgpu::IndexFormat::Uint32,
                        );
                        rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
                        world_drawcalls += 1;
                    }
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

                if outline_target.is_some() {
                    rpass.set_pipeline(&self.outline_pipeline);
                    rpass.set_bind_group(0, &self.camera_bind_group, &[]);
                    rpass.set_vertex_buffer(0, self.outline_vertex_buffer.slice(..));
                    rpass.draw(0..BLOCK_OUTLINE_VERTEX_COUNT, 0..1);
                }
            }
        }

        if let Some(mesh) = first_person_overlay_mesh.as_ref() {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("First Person Overlay Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
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
            rpass.set_pipeline(&self.ui_3d_overlay_pipeline);
            rpass.set_bind_group(0, &self.world_texture_bind_group, &[]);
            rpass.set_bind_group(1, &self.camera_bind_group, &[]);
            rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
            rpass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
            world_drawcalls += 1;
        }

        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("UI Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth.view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });

            // 2D-UI in NDC: Crosshair bleibt stabil in der Screen-Mitte.
            rpass.set_pipeline(&self.ui_pipeline);
            rpass.set_bind_group(1, &self.camera_bind_group, &[]);

            if underwater_kind == Some(FluidKind::Water)
                && !showing_generation_screen
                && !showing_pause_menu
                && !showing_inventory
            {
                rpass.set_bind_group(0, &self.world_texture_bind_group, &[]);
                rpass.set_vertex_buffer(0, self.water_overlay_mesh.vertex_buffer.slice(..));
                rpass.set_index_buffer(
                    self.water_overlay_mesh.index_buffer.slice(..),
                    wgpu::IndexFormat::Uint32,
                );
                rpass.draw_indexed(0..self.water_overlay_mesh.index_count, 0, 0..1);
            }

            if !showing_generation_screen && !showing_pause_menu && !showing_inventory {
                rpass.set_bind_group(0, &self.world_texture_bind_group, &[]);
                rpass.set_vertex_buffer(0, self.crosshair_mesh.vertex_buffer.slice(..));
                rpass.set_index_buffer(
                    self.crosshair_mesh.index_buffer.slice(..),
                    wgpu::IndexFormat::Uint32,
                );
                rpass.draw_indexed(0..self.crosshair_mesh.index_count, 0, 0..1);
            }
            if show_hotbar && let Some(mesh) = self.hotbar_overlay_mesh.as_ref() {
                rpass.set_bind_group(0, &self.world_texture_bind_group, &[]);
                rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                rpass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
            }

            if let Some(mesh) = &generating_overlay_mesh {
                rpass.set_bind_group(0, &self.worldgen_ui_texture_bind_group, &[]);
                rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                rpass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
            } else if let Some(mesh) = &menu_overlay_mesh {
                rpass.set_bind_group(0, &self.worldgen_ui_texture_bind_group, &[]);
                rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                rpass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
            } else if let Some(mesh) = self.inventory_overlay_mesh.as_ref() {
                rpass.set_bind_group(0, &self.world_texture_bind_group, &[]);
                rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                rpass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
            }

            if let Some(draw) = inventory_preview_draw {
                self.draw_item_preview(&mut rpass, draw);
            }
            for draw in &item_preview_draws {
                self.draw_item_preview(&mut rpass, *draw);
            }
            if inventory_preview_draw.is_some() || !item_preview_draws.is_empty() {
                rpass.set_viewport(
                    0.0,
                    0.0,
                    self.config.width as f32,
                    self.config.height as f32,
                    0.0,
                    1.0,
                );
                rpass.set_scissor_rect(0, 0, self.config.width, self.config.height);
                rpass.set_pipeline(&self.ui_pipeline);
                rpass.set_bind_group(1, &self.camera_bind_group, &[]);
            }

            if let Some(mesh) = &debug_overlay_mesh {
                rpass.set_bind_group(0, &self.worldgen_ui_texture_bind_group, &[]);
                rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                rpass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
            }
            if let Some(mesh) = self.chat_overlay_mesh.as_ref() {
                rpass.set_bind_group(0, &self.worldgen_ui_texture_bind_group, &[]);
                rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                rpass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
            }
        }

        self.queue.submit(Some(encoder.finish()));
        frame.present();
        self.maybe_log_perf_summary();
        let render_ms = render_started.elapsed().as_secs_f32() * 1000.0;
        self.frame_perf.far_lod_drawcalls = lod_drawcalls;
        perf_trace::record_duration_ms(
            "state.render.total",
            render_ms,
            &format!(
                "visible_chunks={} visible_lod={} world_drawcalls={} lod_drawcalls={} near_mem_mb={:.2} lod_mem_mb={:.2}",
                visible_chunks.len(),
                visible_lod_columns.len(),
                world_drawcalls,
                lod_drawcalls,
                near_mesh_memory_bytes as f64 / (1024.0 * 1024.0),
                far_lod_memory_bytes as f64 / (1024.0 * 1024.0)
            ),
        );
        let _ = perf_trace::append_gpu_csv_line(
            "frame_index,cpu_render_ms,gpu_timestamps_supported,terrain_drawcalls,lod_drawcalls,visible_chunks,visible_lod,near_mesh_mem_bytes,lod_mesh_mem_bytes",
            &format!(
                "{},{:.3},false,{},{},{},{},{},{}",
                self.frame_index,
                render_ms,
                world_drawcalls,
                lod_drawcalls,
                visible_chunks.len(),
                visible_lod_columns.len(),
                near_mesh_memory_bytes,
                far_lod_memory_bytes
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

    pub(crate) fn deferred_edit_waiting_debug_line(&self) -> Option<String> {
        if self.deferred_block_edits_total == 0 {
            return None;
        }

        let waiting_coord = self.deferred_block_edits.iter().find_map(|(coord, edits)| {
            if edits.is_empty() || self.world.get_chunk(*coord).is_some() {
                None
            } else {
                Some(*coord)
            }
        });

        if let Some(coord) = waiting_coord {
            Some(format!(
                "Deferred edit: waiting for chunk ({},{},{})",
                coord.x, coord.y, coord.z
            ))
        } else {
            Some(format!(
                "Deferred edits pending: {}",
                self.deferred_block_edits_total
            ))
        }
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
                    let loaded = self.chunk_is_generation_satisfied(coord);
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
        let mut slowest = self
            .sim_systems
            .iter()
            .enumerate()
            .map(|(idx, sys)| {
                (
                    sys.name(),
                    self.sim_system_last_frame_ms
                        .get(idx)
                        .copied()
                        .unwrap_or(0.0),
                    self.sim_system_last_frame_ticks
                        .get(idx)
                        .copied()
                        .unwrap_or(0),
                )
            })
            .collect::<Vec<_>>();
        slowest.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
        let slowest_summary = slowest
            .iter()
            .take(3)
            .filter(|(_, ms, _)| *ms > 0.05)
            .map(|(name, ms, ticks)| format!("{name}={ms:.2}ms/{ticks}t"))
            .collect::<Vec<_>>()
            .join(", ");
        let blocked_water = self.fluid_debug_counts.unloaded_writes.0;
        let blocked_lava = self.fluid_debug_counts.unloaded_writes.1;
        if let Some(len) = self.fluids_bootstrap_queue_len() {
            format!(
                "SimSystems active: {names} | Fluids bootstrap_queue={len} blocked_unloaded W/L={blocked_water}/{blocked_lava}{}",
                if slowest_summary.is_empty() {
                    String::new()
                } else {
                    format!(" | Slowest(last): {slowest_summary}")
                }
            )
        } else {
            format!(
                "SimSystems active: {names} | Fluids blocked_unloaded W/L={blocked_water}/{blocked_lava}{}",
                if slowest_summary.is_empty() {
                    String::new()
                } else {
                    format!(" | Slowest(last): {slowest_summary}")
                }
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
        let provisional = self
            .chunk_lighting_progress
            .values()
            .filter(|progress| progress.state == ChunkLightingReadiness::Provisional)
            .count();
        let visual_ready = self
            .chunk_lighting_progress
            .values()
            .filter(|progress| progress.state == ChunkLightingReadiness::VisualReady)
            .count();
        let finalized = self
            .chunk_lighting_progress
            .values()
            .filter(|progress| progress.state == ChunkLightingReadiness::Finalized)
            .count();
        let pending = self.lighting_request_pending
            || self.lighting_job_inflight
            || !self.pending_lighting_dirty_chunks.is_empty()
            || provisional > 0
            || visual_ready > 0
            || !self.pending_chunk_added_lighting_jobs.is_empty()
            || !self.pending_removed_lighting_batches.is_empty();
        let debounce_ms_left = self
            .lighting_rebuild_not_before
            .map(|t| t.saturating_duration_since(now).as_millis())
            .unwrap_or(0);
        format!(
            "Lighting Pending: {} (prov={} visual={} final={} req={} inflight={} dirty_q={} stream_q={} remove_q={} result_q={} debounce={}ms)",
            if pending { "yes" } else { "no" },
            provisional,
            visual_ready,
            finalized,
            self.lighting_request_pending,
            self.lighting_job_inflight,
            self.pending_lighting_dirty_chunks.len(),
            self.pending_chunk_added_lighting_jobs.len(),
            self.pending_removed_lighting_batches.len(),
            self.lighting_results.len(),
            debounce_ms_left
        )
    }

    pub(crate) fn hitch_counters(&self) -> HitchCounters {
        HitchCounters {
            loaded_chunks: self.chunk_meshes.len(),
            mesh_jobs_queued: self.dirty_chunk_queue.len()
                + self.inflight.len()
                + self.mesh_results.len()
                + self.mesh_upload_queue.len(),
            lighting_dirty_chunks: self.pending_lighting_dirty_chunks.len()
                + self.pending_chunk_added_lighting_jobs.len()
                + self.pending_removed_lighting_batches.len(),
            storage_loads_inflight: self.saved_column_loads_inflight.len()
                + self.storage_loader.queue_len_approx(),
            active_sim_columns: self.active_sim_columns.len(),
        }
    }
}
