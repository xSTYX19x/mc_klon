use std::ops::RangeInclusive;
use std::sync::Arc;

use crate::world::config::WorldConfig;
use crate::world::{Chunk, ChunkCoord};

use super::carvers::SeaGuard;
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
    pub(crate) fn generate_column(
        &self,
        chunk_x: i32,
        chunk_z: i32,
        chunk_ys: RangeInclusive<i32>,
        scratch: &mut Chunk,
        emit: impl FnMut(i32, &Chunk),
    ) -> SeaGuard {
        self.pipeline
            .generate_column(chunk_x, chunk_z, chunk_ys, scratch, emit)
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
    use crate::world::fluids::fluid_data_make_static;
    use crate::worldgen::carvers::{EDGE_NEG_X, EDGE_NEG_Z, EDGE_POS_X, EDGE_POS_Z};

    const SEA_SEEDS: [u64; 3] = [132_211, 7, 31_337];

    /// Chunk columns whose heightmap dips below sea level, deepest first. The
    /// presets rarely go that low, so this scans a wide square coarsely.
    fn sea_chunks(generator: &TerrainGenerator, sea_level: i32) -> Vec<(i32, i32)> {
        let sampler = generator.terrain_sampler();
        let mut found = Vec::new();
        for cz in -48..=48 {
            for cx in -48..=48 {
                let min_height = (0..4)
                    .map(|i| {
                        let x = cx * 16 + (i % 2) * 8 + 4;
                        let z = cz * 16 + (i / 2) * 8 + 4;
                        sampler.height_at(x, z, 255)
                    })
                    .min()
                    .unwrap();
                if min_height < sea_level - 2 {
                    found.push((min_height, cx, cz));
                }
            }
        }
        found.sort();
        found.into_iter().map(|(_, cx, cz)| (cx, cz)).collect()
    }

    /// Terrain phase only: (blocks, data) of a whole column, indexed
    /// `x + z * 16 + y * 256`, plus the column's sea guard.
    fn terrain_column(
        generator: &TerrainGenerator,
        cx: i32,
        cz: i32,
    ) -> (Vec<BlockId>, Vec<u8>, SeaGuard) {
        let mut scratch = Chunk::new_empty();
        let mut blocks = Vec::new();
        let mut data = Vec::new();
        let guard = generator.generate_column(cx, cz, 0..=15, &mut scratch, |_, section| {
            blocks.extend(section.blocks.to_vec());
            data.extend(section.block_data.iter().copied());
        });
        (blocks, data, guard)
    }

    fn cell(x: i32, y: i32, z: i32) -> usize {
        (x + z * 16 + y * 256) as usize
    }

    /// "Meere": every empty cell below sea level is static sea water, nothing
    /// above it is, and the surface under the sea is never grass.
    #[test]
    fn sea_floods_everything_below_sea_level() {
        for seed in SEA_SEEDS {
            let cfg = WorldConfig {
                seed,
                ..WorldConfig::default()
            };
            let generator = TerrainGenerator::new(cfg, WorldgenPreset::Hilly);
            let (cx, cz) = sea_chunks(&generator, cfg.sea_level)[0];
            let (blocks, data, _) = terrain_column(&generator, cx, cz);
            let mut water = 0;
            for z in 0..16 {
                for x in 0..16 {
                    for y in 1..256 {
                        let block = blocks[cell(x, y, z)];
                        if y < cfg.sea_level {
                            assert_ne!(
                                block,
                                BlockId::Air,
                                "seed {seed}: dry cell below sea level"
                            );
                        }
                        if block != BlockId::WaterStill {
                            continue;
                        }
                        water += 1;
                        assert!(y < cfg.sea_level, "seed {seed}: water above sea level");
                        assert_eq!(data[cell(x, y, z)], fluid_data_make_static(0, false));
                        let below = blocks[cell(x, y - 1, z)];
                        assert_ne!(below, BlockId::Grass, "seed {seed}: grass under the sea");
                    }
                }
            }
            assert!(water > 0, "seed {seed}: chunk ({cx}, {cz}) has no sea");
        }
    }

    /// The neighbour's cell across an edge, for edge cell `i`.
    type NeighbourCell = fn(i32) -> (i32, i32);

    /// The carvers trust the sea guard about water they cannot see across the
    /// column edge; it must never deny water that the neighbour really has.
    #[test]
    fn sea_guard_never_denies_the_neighbours_water() {
        let mut checked_water = 0;
        for seed in SEA_SEEDS {
            let cfg = WorldConfig {
                seed,
                ..WorldConfig::default()
            };
            let generator = TerrainGenerator::new(cfg, WorldgenPreset::Hilly);
            for (cx, cz) in sea_chunks(&generator, cfg.sea_level).into_iter().take(3) {
                let (_, _, guard) = terrain_column(&generator, cx, cz);
                // (edge, neighbour offset, neighbour cell for edge cell i)
                let edges: [(usize, (i32, i32), NeighbourCell); 4] = [
                    (EDGE_NEG_X, (-1, 0), |i| (15, i)),
                    (EDGE_POS_X, (1, 0), |i| (0, i)),
                    (EDGE_NEG_Z, (0, -1), |i| (i, 15)),
                    (EDGE_POS_Z, (0, 1), |i| (i, 0)),
                ];
                for (edge, (dx, dz), neighbour_cell) in edges {
                    let (neighbour, _, _) = terrain_column(&generator, cx + dx, cz + dz);
                    for i in 0..16 {
                        let (nx, nz) = neighbour_cell(i);
                        for y in 1..cfg.sea_level {
                            if neighbour[cell(nx, y, nz)] != BlockId::WaterStill {
                                continue;
                            }
                            checked_water += 1;
                            assert!(
                                guard.may_hold_water(edge, i, y),
                                "seed {seed} chunk ({cx}, {cz}) edge {edge} cell {i}: water at y {y} denied"
                            );
                        }
                    }
                }
            }
        }
        assert!(checked_water > 0, "no sea crossed a chunk edge");
    }

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
