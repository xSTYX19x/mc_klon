pub mod carvers;
pub mod density;
pub mod java_random;
pub mod noise;
pub mod pipeline;
pub mod surface_rules;
pub mod terrain;
pub mod terrain_generator;

use crate::world::config::WorldConfig;
use crate::world::{Chunk, ChunkCoord};
pub(crate) use carvers::ChunkColumn as ColumnBuffer;

pub trait ChunkGenerator {
    fn generate_chunk(&self, coord: ChunkCoord, out: &mut Chunk);
    fn estimate_surface_height(&self, world_x: i32, world_z: i32, world_max_y: i32) -> i32;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GenerationProgress {
    pub phase: &'static str,
    pub done: u32,
    pub total: u32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FeatureRng {
    state: u64,
}

impl FeatureRng {
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn reseed(&mut self, seed: u64) {
        self.state = seed;
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

pub(crate) struct GenContext {
    pub seed: u64,
    pub world_cfg: WorldConfig,
    pub feature_rng: FeatureRng,
    pub progress_hook: Option<Box<dyn FnMut(GenerationProgress) + Send>>,
}

impl GenContext {
    pub fn new(seed: u64, world_cfg: WorldConfig) -> Self {
        Self {
            seed,
            world_cfg,
            feature_rng: FeatureRng::new(seed),
            progress_hook: None,
        }
    }

    pub fn report_progress(&mut self, phase: &'static str, done: u32, total: u32) {
        if let Some(hook) = self.progress_hook.as_mut() {
            hook(GenerationProgress { phase, done, total });
        }
    }
}

pub(crate) trait WorldGenerator: Send + Sync {
    fn generate_column(
        &self,
        cx: i32,
        cz: i32,
        out: &mut ColumnBuffer,
        ctx: &mut GenContext,
    );
}

pub use density::{DensityFn, EvalContext};
pub use pipeline::WorldgenPipeline;
pub use surface_rules::{SurfaceContext, SurfaceRules};
pub use terrain::{DEFAULT_WORLDGEN_PRESET, TerrainSampler, WorldGenConfig, WorldgenPreset};
pub use terrain_generator::TerrainGenerator;
