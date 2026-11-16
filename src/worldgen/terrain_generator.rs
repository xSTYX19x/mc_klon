use std::ops::RangeInclusive;
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

    /// The sampler behind this generator's heightmap, for callers that need
    /// the same terrain heights (lakes, springs, trees) without building a
    /// second one.
    pub fn terrain_sampler(&self) -> &TerrainSampler {
        &self.terrain_sampler
    }

    /// Generates sections `chunk_ys` of one column into `scratch`, handing
    /// each to `emit`; see `WorldgenPipeline::generate_column`.
    pub fn generate_column(
        &self,
        chunk_x: i32,
        chunk_z: i32,
        chunk_ys: RangeInclusive<i32>,
        scratch: &mut Chunk,
        emit: impl FnMut(i32, &Chunk),
    ) {
        self.pipeline
            .generate_column(chunk_x, chunk_z, chunk_ys, scratch, emit);
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
        self.pipeline.generate_chunk(coord, out);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::BlockId;

    /// The column path computes the y-independent terrain once and skips
    /// all-air sections; it must produce exactly what per-section generation
    /// produces.
    #[test]
    fn column_generation_matches_per_section_generation() {
        let cfg = WorldConfig {
            seed: 0x0C01_DE11_5EC7_1011,
            ..WorldConfig::default()
        };
        let chunk_ys = cfg.min_y.div_euclid(16)..=cfg.max_y.div_euclid(16);
        for preset in [
            WorldgenPreset::Flatish,
            WorldgenPreset::ClassicFlatter,
            WorldgenPreset::Rd161348Like,
            WorldgenPreset::Hilly,
        ] {
            let generator = TerrainGenerator::new(cfg, preset);
            for (cx, cz) in [(0, 0), (-7, 3), (25, -18)] {
                let mut scratch = Chunk::new_empty();
                let mut sections = Vec::new();
                generator.generate_column(cx, cz, chunk_ys.clone(), &mut scratch, |cy, section| {
                    sections.push((cy, section.blocks.to_vec(), section.block_data.to_vec()));
                });
                assert_eq!(sections.len(), chunk_ys.clone().count());
                let all_air = sections
                    .iter()
                    .filter(|(_, blocks, _)| blocks.iter().all(|&b| b == BlockId::Air))
                    .count();
                assert!(all_air > 0, "{preset:?}: no all-air section to skip");

                for (cy, blocks, data) in sections {
                    let mut single = Chunk::new_empty();
                    generator.generate_chunk(
                        ChunkCoord {
                            x: cx,
                            y: cy,
                            z: cz,
                        },
                        &mut single,
                    );
                    assert_eq!(
                        blocks,
                        single.blocks.to_vec(),
                        "{preset:?} ({cx}, {cy}, {cz}): blocks differ"
                    );
                    assert_eq!(
                        data, *single.block_data,
                        "{preset:?} ({cx}, {cy}, {cz}): data differs"
                    );
                }
            }
        }
    }
}
