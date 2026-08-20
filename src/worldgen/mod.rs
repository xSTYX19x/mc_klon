pub mod carvers;
pub mod density;
pub mod java_random;
pub mod noise;
pub mod ores;
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

pub(crate) struct GenContext {
    pub seed: u64,
    pub world_cfg: WorldConfig,
    pub progress_hook: Option<Box<dyn FnMut(GenerationProgress) + Send>>,
}

impl GenContext {
    pub fn new(seed: u64, world_cfg: WorldConfig) -> Self {
        Self {
            seed,
            world_cfg,
            progress_hook: None,
        }
    }

    pub fn report_progress(&mut self, phase: &'static str, done: u32, total: u32) {
        if let Some(hook) = self.progress_hook.as_mut() {
            hook(GenerationProgress { phase, done, total });
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ClassicLevelSize {
    Small,
    Normal,
    Huge,
}

impl ClassicLevelSize {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Small => "Small",
            Self::Normal => "Normal",
            Self::Huge => "Huge",
        }
    }

    pub(crate) const fn xz_extent_blocks(self) -> i32 {
        match self {
            Self::Small => 128,
            Self::Normal => 256,
            Self::Huge => 512,
        }
    }

    pub(crate) const fn boundary_radius_chunks(self) -> i32 {
        self.xz_extent_blocks() / 32
    }

    pub(crate) fn infer_from_boundary_radius_chunks(radius_chunks: i32) -> Option<Self> {
        match radius_chunks {
            4 => Some(Self::Small),
            8 => Some(Self::Normal),
            16 => Some(Self::Huge),
            _ => None,
        }
    }
}

pub use terrain::{DEFAULT_WORLDGEN_PRESET, TerrainSampler, WorldGenConfig, WorldgenPreset};
pub use terrain_generator::TerrainGenerator;

#[cfg(test)]
mod tests {
    use super::ClassicLevelSize;

    #[test]
    fn classic_level_sizes_match_expected_chunk_boundaries() {
        assert_eq!(ClassicLevelSize::Small.xz_extent_blocks(), 128);
        assert_eq!(ClassicLevelSize::Small.boundary_radius_chunks(), 4);
        assert_eq!(ClassicLevelSize::Normal.xz_extent_blocks(), 256);
        assert_eq!(ClassicLevelSize::Normal.boundary_radius_chunks(), 8);
        assert_eq!(ClassicLevelSize::Huge.xz_extent_blocks(), 512);
        assert_eq!(ClassicLevelSize::Huge.boundary_radius_chunks(), 16);
        assert_eq!(
            ClassicLevelSize::infer_from_boundary_radius_chunks(8),
            Some(ClassicLevelSize::Normal)
        );
    }
}
