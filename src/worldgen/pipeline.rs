use std::cell::RefCell;
use std::sync::Arc;

use crate::blocks::BlockId;
use crate::world::{CHUNK_SIZE, Chunk, ChunkCoord};

use super::TerrainSampler;
use super::density::cache::{DensityChunkCache, heightmap2d_value};
use super::density::{DensityFn, EvalContext};
use super::surface_rules::{SurfaceContext, SurfaceRules};

const CHUNK_VOLUME: usize = (CHUNK_SIZE as usize) * (CHUNK_SIZE as usize) * (CHUNK_SIZE as usize);
const COLUMN_COUNT: usize = (CHUNK_SIZE as usize) * (CHUNK_SIZE as usize);
const CHUNK_Y_STRIDE: usize = (CHUNK_SIZE as usize) * (CHUNK_SIZE as usize);

thread_local! {
    static COLUMN_HEIGHTMAP_SCRATCH: RefCell<[f32; COLUMN_COUNT]> =
        RefCell::new([0.0_f32; COLUMN_COUNT]);
}

#[derive(Clone)]
enum DensityFastPath {
    HeightmapMinusY {
        sampler: Arc<TerrainSampler>,
        max_y: i32,
        smooth: bool,
    },
}

#[derive(Clone)]
pub struct WorldgenPipeline {
    pub density_root: DensityFn,
    pub surface_rules: SurfaceRules,
    pub sea_level: i32,
    pub min_y: i32,
    pub max_y: i32,
    density_fast_path: Option<DensityFastPath>,
}

impl WorldgenPipeline {
    pub fn new(
        density_root: DensityFn,
        surface_rules: SurfaceRules,
        sea_level: i32,
        min_y: i32,
        max_y: i32,
    ) -> Self {
        let density_fast_path = Self::detect_density_fast_path(&density_root);
        Self {
            density_root,
            surface_rules,
            sea_level,
            min_y,
            max_y,
            density_fast_path,
        }
    }

    fn detect_density_fast_path(root: &DensityFn) -> Option<DensityFastPath> {
        let DensityFn::Add(a, b) = root else {
            return None;
        };
        let left_heightmap = Self::extract_heightmap_2d(a);
        let right_heightmap = Self::extract_heightmap_2d(b);
        let left_is_neg_y = Self::is_mul_negative_one_y(a);
        let right_is_neg_y = Self::is_mul_negative_one_y(b);

        if let (Some((sampler, max_y, smooth)), true) = (left_heightmap, right_is_neg_y) {
            return Some(DensityFastPath::HeightmapMinusY {
                sampler,
                max_y,
                smooth,
            });
        }
        if let (Some((sampler, max_y, smooth)), true) = (right_heightmap, left_is_neg_y) {
            return Some(DensityFastPath::HeightmapMinusY {
                sampler,
                max_y,
                smooth,
            });
        }
        None
    }

    fn extract_heightmap_2d(node: &DensityFn) -> Option<(Arc<TerrainSampler>, i32, bool)> {
        match node {
            DensityFn::Cache2D { input, .. } => Self::extract_heightmap_2d(input),
            DensityFn::Heightmap2D {
                sampler,
                max_y,
                smooth,
                ..
            } => Some((Arc::clone(sampler), *max_y, *smooth)),
            _ => None,
        }
    }

    fn is_mul_negative_one_y(node: &DensityFn) -> bool {
        let DensityFn::Mul(a, b) = node else {
            return false;
        };
        matches!((&**a, &**b), (DensityFn::Constant(v), DensityFn::Y) if *v == -1.0)
            || matches!((&**a, &**b), (DensityFn::Y, DensityFn::Constant(v)) if *v == -1.0)
    }

    pub fn generate_chunk(&self, seed: u64, coord: ChunkCoord, out: &mut Chunk) {
        out.blocks.make_flat_mut().fill(BlockId::Air);
        Arc::make_mut(&mut out.block_data).fill(0);

        let chunk_world_y0 = coord.y * CHUNK_SIZE;
        let chunk_world_y1 = chunk_world_y0 + CHUNK_SIZE - 1;
        if chunk_world_y1 < self.min_y || chunk_world_y0 > self.max_y {
            out.recompute_cached_block_classification();
            return;
        }

        let mut fallback_ctx = None;
        let density_cache = match &self.density_fast_path {
            Some(DensityFastPath::HeightmapMinusY {
                sampler,
                max_y,
                smooth,
            }) => DensityChunkCache::build_heightmap_minus_y(coord, sampler, *max_y, *smooth),
            None => {
                let mut ctx = EvalContext::new(seed);
                ctx.init_for_chunk(coord.x, coord.y, coord.z);
                let cache = DensityChunkCache::build(&self.density_root, &mut ctx, coord);
                fallback_ctx = Some(ctx);
                cache
            }
        };
        let mut solid_mask = [false; CHUNK_VOLUME];
        let mut top_local_per_column = [-1_i32; COLUMN_COUNT];

        for local_z in 0..CHUNK_SIZE {
            for local_x in 0..CHUNK_SIZE {
                let column_idx = (local_x + local_z * CHUNK_SIZE) as usize;
                let mut local_top = -1_i32;
                let column_base_idx = (local_x + local_z * CHUNK_SIZE) as usize;

                for local_y in 0..CHUNK_SIZE {
                    let world_y = chunk_world_y0 + local_y;
                    if world_y < self.min_y || world_y > self.max_y {
                        continue;
                    }
                    if density_cache.sample(local_x, local_y, local_z) <= 0.0 {
                        continue;
                    }
                    let idx = column_base_idx + (local_y as usize) * CHUNK_Y_STRIDE;
                    solid_mask[idx] = true;
                    local_top = local_y;
                }
                top_local_per_column[column_idx] = local_top;
            }
        }

        let mut top_y_per_column = [0_i32; COLUMN_COUNT];
        match &self.density_fast_path {
            Some(DensityFastPath::HeightmapMinusY {
                sampler,
                max_y,
                smooth,
            }) => {
                COLUMN_HEIGHTMAP_SCRATCH.with(|scratch| {
                    let mut heightmap_per_column = scratch.borrow_mut();
                    for local_z in 0..CHUNK_SIZE {
                        let world_z = coord.z * CHUNK_SIZE + local_z;
                        for local_x in 0..CHUNK_SIZE {
                            let world_x = coord.x * CHUNK_SIZE + local_x;
                            let column_idx = (local_x + local_z * CHUNK_SIZE) as usize;
                            let height_f =
                                heightmap2d_value(sampler, *max_y, *smooth, world_x, world_z);
                            heightmap_per_column[column_idx] = height_f;
                            let mut candidate = height_f.floor() as i32;
                            if height_f + (-1.0 * candidate as f32) <= 0.0 {
                                candidate -= 1;
                            }
                            top_y_per_column[column_idx] = candidate.clamp(self.min_y, self.max_y);
                        }
                    }
                });
            }
            None => {
                let ctx = fallback_ctx
                    .as_mut()
                    .expect("fallback context must exist when no fast path is active");
                for local_z in 0..CHUNK_SIZE {
                    let world_z = coord.z * CHUNK_SIZE + local_z;
                    for local_x in 0..CHUNK_SIZE {
                        let world_x = coord.x * CHUNK_SIZE + local_x;
                        let column_idx = (local_x + local_z * CHUNK_SIZE) as usize;
                        let height_f = self.density_root.eval(ctx, world_x, 0, world_z);
                        let mut candidate = height_f.floor() as i32;
                        if self.density_root.eval(ctx, world_x, candidate, world_z) <= 0.0 {
                            candidate -= 1;
                        }
                        top_y_per_column[column_idx] = candidate.clamp(self.min_y, self.max_y);
                    }
                }
            }
        }

        let out_blocks = out.blocks.make_flat_mut();
        for local_z in 0..CHUNK_SIZE {
            let world_z = coord.z * CHUNK_SIZE + local_z;
            for local_x in 0..CHUNK_SIZE {
                let world_x = coord.x * CHUNK_SIZE + local_x;
                let column_idx = (local_x + local_z * CHUNK_SIZE) as usize;
                let local_top = top_local_per_column[column_idx];
                if local_top < 0 {
                    continue;
                }

                let top_y = top_y_per_column[column_idx];
                let near_water = (top_y - self.sea_level).abs() <= self.surface_rules.shore_band;
                let surface_ctx = SurfaceContext {
                    x: world_x,
                    z: world_z,
                    top_y,
                    sea_level: self.sea_level,
                    near_water,
                };

                let column_base_idx = (local_x + local_z * CHUNK_SIZE) as usize;
                for local_y in 0..CHUNK_SIZE {
                    let idx = column_base_idx + (local_y as usize) * CHUNK_Y_STRIDE;
                    if !solid_mask[idx] {
                        continue;
                    }
                    let world_y = chunk_world_y0 + local_y;
                    out_blocks[idx] = self.surface_rules.block_for_solid(world_y, surface_ctx);
                }
            }
        }
        out.recompute_cached_block_classification();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::config::WorldConfig;
    use crate::worldgen::{WorldGenConfig, WorldgenPreset};

    fn build_fast_density_root(terrain_sampler: Arc<TerrainSampler>, max_y: i32) -> DensityFn {
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

    fn build_equivalent_fallback_density_root(
        terrain_sampler: Arc<TerrainSampler>,
        max_y: i32,
    ) -> DensityFn {
        DensityFn::Add(
            Box::new(DensityFn::Mul(
                Box::new(DensityFn::Constant(1.0)),
                Box::new(DensityFn::Cache2D {
                    node_id: 1002,
                    input: Box::new(DensityFn::Heightmap2D {
                        node_id: 1001,
                        sampler: terrain_sampler,
                        max_y,
                        smooth: true,
                    }),
                }),
            )),
            Box::new(DensityFn::Mul(
                Box::new(DensityFn::Constant(-1.0)),
                Box::new(DensityFn::Y),
            )),
        )
    }

    #[test]
    fn fast_path_matches_equivalent_fallback_output() {
        let world_cfg = WorldConfig::default();
        let sampler = Arc::new(TerrainSampler::new(WorldGenConfig::from_preset(
            132_211,
            WorldgenPreset::Rd161348Like,
        )));
        let surface_rules = SurfaceRules {
            dirt_depth: 3,
            shore_band: 1,
        };
        let seed = world_cfg.seed;
        let fast_pipeline = WorldgenPipeline::new(
            build_fast_density_root(Arc::clone(&sampler), world_cfg.max_y),
            surface_rules,
            world_cfg.sea_level,
            world_cfg.min_y,
            world_cfg.max_y,
        );
        let fallback_pipeline = WorldgenPipeline::new(
            build_equivalent_fallback_density_root(Arc::clone(&sampler), world_cfg.max_y),
            surface_rules,
            world_cfg.sea_level,
            world_cfg.min_y,
            world_cfg.max_y,
        );

        let coords = [
            ChunkCoord { x: 0, y: 0, z: 0 },
            ChunkCoord { x: 3, y: 4, z: -2 },
            ChunkCoord { x: -7, y: 2, z: 5 },
            ChunkCoord { x: 9, y: 8, z: 9 },
        ];
        for coord in coords {
            let mut fast_chunk = Chunk::new_empty();
            fast_pipeline.generate_chunk(seed, coord, &mut fast_chunk);

            let mut fallback_chunk = Chunk::new_empty();
            fallback_pipeline.generate_chunk(seed, coord, &mut fallback_chunk);

            assert_eq!(
                fast_chunk.blocks.to_vec(),
                fallback_chunk.blocks.to_vec(),
                "block mismatch at {:?}",
                coord
            );
            assert_eq!(
                &*fast_chunk.block_data, &*fallback_chunk.block_data,
                "block-data mismatch at {:?}",
                coord
            );
        }
    }
}
