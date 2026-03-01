use std::sync::Arc;

use crate::world::config::WorldConfig;
use crate::world::{Chunk, ChunkCoord};

use super::density::DensityFn;
use super::pipeline::WorldgenPipeline;
use super::surface_rules::SurfaceRules;
use super::{ChunkGenerator, TerrainSampler, WorldGenConfig, WorldgenPreset};

pub struct TerrainGenerator {
    world_cfg: WorldConfig,
    pipeline: WorldgenPipeline,
    terrain_sampler: Arc<TerrainSampler>,
}

impl TerrainGenerator {
    pub fn new(world_cfg: WorldConfig, preset: WorldgenPreset) -> Self {
        let terrain_cfg = WorldGenConfig::from_preset_with_sea_level(
            Self::seed32_from_u64(world_cfg.seed),
            preset,
            world_cfg.sea_level(),
        );
        let terrain_sampler = Arc::new(TerrainSampler::new(terrain_cfg));
        let density_root =
            Self::build_terrain_density_root(Arc::clone(&terrain_sampler), world_cfg.max_y);
        let surface_rules = SurfaceRules {
            dirt_depth: terrain_cfg.dirt_depth.max(1),
            shore_band: 1,
        };
        let pipeline = WorldgenPipeline::new(
            density_root,
            surface_rules,
            world_cfg.sea_level,
            world_cfg.min_y,
            world_cfg.max_y,
        );

        Self {
            world_cfg,
            pipeline,
            terrain_sampler,
        }
    }

    fn seed32_from_u64(seed: u64) -> u32 {
        let mixed = seed ^ (seed >> 32);
        (mixed as u32).wrapping_mul(0x9E37_79B9)
    }

    fn build_terrain_density_root(terrain_sampler: Arc<TerrainSampler>, max_y: i32) -> DensityFn {
        DensityFn::Add(
            Box::new(DensityFn::Cache2D {
                node_id: 1002,
                input: Box::new(DensityFn::Heightmap2D {
                    node_id: 1001,
                    sampler: terrain_sampler,
                    max_y,
                    smooth: true,
                }),
            }),
            Box::new(DensityFn::Mul(
                Box::new(DensityFn::Constant(-1.0)),
                Box::new(DensityFn::Y),
            )),
        )
    }
}

impl ChunkGenerator for TerrainGenerator {
    fn generate_chunk(&self, coord: ChunkCoord, out: &mut Chunk) {
        self.pipeline
            .generate_chunk(self.world_cfg.seed, coord, out);
    }

    fn estimate_surface_height(&self, world_x: i32, world_z: i32, world_max_y: i32) -> i32 {
        self.terrain_sampler
            .height_at(world_x, world_z, world_max_y)
            .clamp(
                self.world_cfg.min_y.min(self.world_cfg.max_y),
                self.world_cfg.max_y.max(self.world_cfg.min_y),
            )
    }
}
