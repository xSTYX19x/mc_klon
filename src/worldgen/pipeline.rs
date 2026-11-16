use std::ops::RangeInclusive;
use std::sync::Arc;

use crate::blocks::BlockId;
use crate::world::{CHUNK_SIZE, Chunk, ChunkCoord};

use super::TerrainSampler;
use super::density::cache::{DensityChunkCache, GRID_COLUMNS, smoothed_height};
use super::density::{DensityFn, EvalContext};
use super::surface_rules::{SurfaceContext, SurfaceRules};

const CHUNK_VOLUME: usize = (CHUNK_SIZE as usize) * (CHUNK_SIZE as usize) * (CHUNK_SIZE as usize);
const COLUMN_COUNT: usize = (CHUNK_SIZE as usize) * (CHUNK_SIZE as usize);
const CHUNK_Y_STRIDE: usize = (CHUNK_SIZE as usize) * (CHUNK_SIZE as usize);
/// Raw-height samples per axis for one column: x/z from -1 to 17.
const RAW_HEIGHT_AXIS: usize = CHUNK_SIZE as usize + 3;

/// The y-independent part of terrain generation for one chunk column: the
/// smoothed heightmap at every block column (for the surface rules) and at the
/// density-grid points. It is the same for all sections of the column.
struct ColumnTerrain {
    top_y: [i32; COLUMN_COUNT],
    grid_heights: [f32; GRID_COLUMNS],
    grid_max_height: f32,
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

    /// The y-independent part of generating chunk column (`chunk_x`,
    /// `chunk_z`), or `None` when the density tree has no heightmap fast path
    /// and every section has to evaluate the tree itself.
    fn column_terrain(&self, chunk_x: i32, chunk_z: i32) -> Option<ColumnTerrain> {
        let Some(DensityFastPath::HeightmapMinusY {
            sampler,
            max_y,
            smooth,
        }) = &self.density_fast_path
        else {
            return None;
        };
        // Raw heights over the column plus a one-block margin (the 3×3
        // smoothing) and the density-grid points on its far edge (x/z = 16):
        // each is sampled once instead of up to nine times per section.
        let world_x0 = chunk_x * CHUNK_SIZE - 1;
        let world_z0 = chunk_z * CHUNK_SIZE - 1;
        let mut raw = [0_i32; RAW_HEIGHT_AXIS * RAW_HEIGHT_AXIS];
        for (i, raw_height) in raw.iter_mut().enumerate() {
            let world_x = world_x0 + (i % RAW_HEIGHT_AXIS) as i32;
            let world_z = world_z0 + (i / RAW_HEIGHT_AXIS) as i32;
            *raw_height = sampler.height_at(world_x, world_z, *max_y);
        }
        let raw_height = |world_x: i32, world_z: i32| {
            raw[(world_x - world_x0) as usize + (world_z - world_z0) as usize * RAW_HEIGHT_AXIS]
        };
        let height_at = |world_x, world_z| smoothed_height(raw_height, *smooth, world_x, world_z);

        let mut top_y = [0_i32; COLUMN_COUNT];
        for local_z in 0..CHUNK_SIZE {
            let world_z = chunk_z * CHUNK_SIZE + local_z;
            for local_x in 0..CHUNK_SIZE {
                let world_x = chunk_x * CHUNK_SIZE + local_x;
                let height_f = height_at(world_x, world_z);
                let mut candidate = height_f.floor() as i32;
                if height_f + (-1.0 * candidate as f32) <= 0.0 {
                    candidate -= 1;
                }
                top_y[(local_x + local_z * CHUNK_SIZE) as usize] =
                    candidate.clamp(self.min_y, self.max_y);
            }
        }
        let grid_heights = DensityChunkCache::grid_heights(chunk_x, chunk_z, height_at);
        let grid_max_height = grid_heights
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        Some(ColumnTerrain {
            top_y,
            grid_heights,
            grid_max_height,
        })
    }

    /// One section on its own; see `ChunkGenerator::generate_chunk`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn generate_chunk(&self, coord: ChunkCoord, out: &mut Chunk) {
        let column = self.column_terrain(coord.x, coord.z);
        self.generate_section(coord, out, column.as_ref());
    }

    /// Generates sections `chunk_ys` of one column, one after another into
    /// `scratch`, and hands each to `emit`. The y-independent terrain (the
    /// smoothed heightmap) is computed once for the column instead of once per
    /// section; the output is identical to calling `generate_chunk` per section.
    pub fn generate_column(
        &self,
        chunk_x: i32,
        chunk_z: i32,
        chunk_ys: RangeInclusive<i32>,
        scratch: &mut Chunk,
        mut emit: impl FnMut(i32, &Chunk),
    ) {
        let column = self.column_terrain(chunk_x, chunk_z);
        for chunk_y in chunk_ys {
            let coord = ChunkCoord {
                x: chunk_x,
                y: chunk_y,
                z: chunk_z,
            };
            self.generate_section(coord, scratch, column.as_ref());
            emit(chunk_y, scratch);
        }
    }

    fn generate_section(&self, coord: ChunkCoord, out: &mut Chunk, column: Option<&ColumnTerrain>) {
        out.blocks.make_flat_mut().fill(BlockId::Air);
        Arc::make_mut(&mut out.block_data).fill(0);

        let chunk_world_y0 = coord.y * CHUNK_SIZE;
        let chunk_world_y1 = chunk_world_y0 + CHUNK_SIZE - 1;
        if chunk_world_y1 < self.min_y || chunk_world_y0 > self.max_y {
            out.recompute_cached_block_classification();
            return;
        }

        let mut fallback_ctx = None;
        let density_cache = match column {
            Some(column) => {
                // Every grid value is at most `grid_max_height - chunk_world_y0`.
                // Below zero, trilinear interpolation cannot reach a positive
                // density anywhere in the section: it is all air.
                if column.grid_max_height - (chunk_world_y0 as f32) < 0.0 {
                    out.recompute_cached_block_classification();
                    return;
                }
                DensityChunkCache::from_grid_heights(chunk_world_y0, &column.grid_heights)
            }
            None => {
                let mut ctx = EvalContext::new();
                ctx.init_for_chunk(coord.x, coord.z);
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

        let top_y_per_column = match column {
            Some(column) => column.top_y,
            None => {
                let ctx = fallback_ctx
                    .as_mut()
                    .expect("fallback context must exist when no fast path is active");
                let mut top_y_per_column = [0_i32; COLUMN_COUNT];
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
                top_y_per_column
            }
        };

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
            fast_pipeline.generate_chunk(coord, &mut fast_chunk);

            let mut fallback_chunk = Chunk::new_empty();
            fallback_pipeline.generate_chunk(coord, &mut fallback_chunk);

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
