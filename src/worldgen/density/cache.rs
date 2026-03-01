use std::cell::RefCell;

use crate::world::{CHUNK_SIZE, ChunkCoord};
use crate::worldgen::TerrainSampler;

use super::{DensityFn, EvalContext};

pub const DENSITY_SAMPLE_STEP: i32 = 4;
const GRID_AXIS: usize = (CHUNK_SIZE as usize / DENSITY_SAMPLE_STEP as usize) + 1;
const GRID_LEN: usize = GRID_AXIS * GRID_AXIS * GRID_AXIS;

thread_local! {
    static HEIGHTMAP_GRID_SCRATCH: RefCell<[f32; GRID_AXIS * GRID_AXIS]> =
        RefCell::new([0.0_f32; GRID_AXIS * GRID_AXIS]);
}

#[derive(Clone)]
pub struct DensityChunkCache {
    chunk_world_x0: i32,
    chunk_world_y0: i32,
    chunk_world_z0: i32,
    grid: [f32; GRID_LEN],
}

impl DensityChunkCache {
    pub fn build(density_root: &DensityFn, ctx: &mut EvalContext, coord: ChunkCoord) -> Self {
        let chunk_world_x0 = coord.x * CHUNK_SIZE;
        let chunk_world_y0 = coord.y * CHUNK_SIZE;
        let chunk_world_z0 = coord.z * CHUNK_SIZE;

        let mut grid = [0.0_f32; GRID_LEN];
        for gz in 0..GRID_AXIS {
            let world_z = chunk_world_z0 + (gz as i32) * DENSITY_SAMPLE_STEP;
            for gy in 0..GRID_AXIS {
                let world_y = chunk_world_y0 + (gy as i32) * DENSITY_SAMPLE_STEP;
                for gx in 0..GRID_AXIS {
                    let world_x = chunk_world_x0 + (gx as i32) * DENSITY_SAMPLE_STEP;
                    grid[Self::grid_index(gx, gy, gz)] =
                        density_root.eval(ctx, world_x, world_y, world_z);
                }
            }
        }

        Self {
            chunk_world_x0,
            chunk_world_y0,
            chunk_world_z0,
            grid,
        }
    }

    pub fn build_heightmap_minus_y(
        coord: ChunkCoord,
        sampler: &TerrainSampler,
        max_y: i32,
        smooth: bool,
    ) -> Self {
        let chunk_world_x0 = coord.x * CHUNK_SIZE;
        let chunk_world_y0 = coord.y * CHUNK_SIZE;
        let chunk_world_z0 = coord.z * CHUNK_SIZE;

        let mut grid = [0.0_f32; GRID_LEN];
        // Cache per sampled (gx,gz) heightmap value and reuse over gy, matching
        // the old Cache2D behavior while avoiding enum dispatch in the hot loop.
        HEIGHTMAP_GRID_SCRATCH.with(|scratch| {
            let mut sampled_heightmap = scratch.borrow_mut();
            for gz in 0..GRID_AXIS {
                let world_z = chunk_world_z0 + (gz as i32) * DENSITY_SAMPLE_STEP;
                for gy in 0..GRID_AXIS {
                    let world_y = chunk_world_y0 + (gy as i32) * DENSITY_SAMPLE_STEP;
                    for gx in 0..GRID_AXIS {
                        let world_x = chunk_world_x0 + (gx as i32) * DENSITY_SAMPLE_STEP;
                        let column_idx = gx + gz * GRID_AXIS;
                        if gy == 0 {
                            sampled_heightmap[column_idx] =
                                heightmap2d_value(sampler, max_y, smooth, world_x, world_z);
                        }
                        let heightmap = sampled_heightmap[column_idx];
                        grid[Self::grid_index(gx, gy, gz)] = heightmap + (-1.0 * world_y as f32);
                    }
                }
            }
        });

        Self {
            chunk_world_x0,
            chunk_world_y0,
            chunk_world_z0,
            grid,
        }
    }

    pub fn sample(&self, lx: i32, ly: i32, lz: i32) -> f32 {
        debug_assert!((0..CHUNK_SIZE).contains(&lx));
        debug_assert!((0..CHUNK_SIZE).contains(&ly));
        debug_assert!((0..CHUNK_SIZE).contains(&lz));

        let gx0 = (lx / DENSITY_SAMPLE_STEP) as usize;
        let gy0 = (ly / DENSITY_SAMPLE_STEP) as usize;
        let gz0 = (lz / DENSITY_SAMPLE_STEP) as usize;
        let gx1 = gx0 + 1;
        let gy1 = gy0 + 1;
        let gz1 = gz0 + 1;

        let tx = (lx.rem_euclid(DENSITY_SAMPLE_STEP) as f32) / (DENSITY_SAMPLE_STEP as f32);
        let ty = (ly.rem_euclid(DENSITY_SAMPLE_STEP) as f32) / (DENSITY_SAMPLE_STEP as f32);
        let tz = (lz.rem_euclid(DENSITY_SAMPLE_STEP) as f32) / (DENSITY_SAMPLE_STEP as f32);

        let c000 = self.grid[Self::grid_index(gx0, gy0, gz0)];
        let c100 = self.grid[Self::grid_index(gx1, gy0, gz0)];
        let c010 = self.grid[Self::grid_index(gx0, gy1, gz0)];
        let c110 = self.grid[Self::grid_index(gx1, gy1, gz0)];
        let c001 = self.grid[Self::grid_index(gx0, gy0, gz1)];
        let c101 = self.grid[Self::grid_index(gx1, gy0, gz1)];
        let c011 = self.grid[Self::grid_index(gx0, gy1, gz1)];
        let c111 = self.grid[Self::grid_index(gx1, gy1, gz1)];

        let x00 = Self::lerp(c000, c100, tx);
        let x10 = Self::lerp(c010, c110, tx);
        let x01 = Self::lerp(c001, c101, tx);
        let x11 = Self::lerp(c011, c111, tx);
        let y0 = Self::lerp(x00, x10, ty);
        let y1 = Self::lerp(x01, x11, ty);
        Self::lerp(y0, y1, tz)
    }

    pub fn contains_world_pos(&self, world_x: i32, world_y: i32, world_z: i32) -> bool {
        (self.chunk_world_x0..self.chunk_world_x0 + CHUNK_SIZE).contains(&world_x)
            && (self.chunk_world_y0..self.chunk_world_y0 + CHUNK_SIZE).contains(&world_y)
            && (self.chunk_world_z0..self.chunk_world_z0 + CHUNK_SIZE).contains(&world_z)
    }

    pub fn sample_world_pos(&self, world_x: i32, world_y: i32, world_z: i32) -> f32 {
        self.sample(
            world_x - self.chunk_world_x0,
            world_y - self.chunk_world_y0,
            world_z - self.chunk_world_z0,
        )
    }

    #[inline]
    fn grid_index(gx: usize, gy: usize, gz: usize) -> usize {
        gx + gy * GRID_AXIS + gz * GRID_AXIS * GRID_AXIS
    }

    #[inline]
    fn lerp(a: f32, b: f32, t: f32) -> f32 {
        a + (b - a) * t
    }
}

#[inline]
pub fn heightmap2d_value(
    sampler: &TerrainSampler,
    max_y: i32,
    smooth: bool,
    world_x: i32,
    world_z: i32,
) -> f32 {
    if smooth {
        let current = sampler.height_at(world_x, world_z, max_y) as f32;
        let mut sum = 0.0_f32;
        let mut count = 0.0_f32;
        for dz in -1..=1 {
            for dx in -1..=1 {
                sum += sampler.height_at(world_x + dx, world_z + dz, max_y) as f32;
                count += 1.0;
            }
        }
        let avg = sum / count.max(1.0);
        let blend = if (current - avg).abs() > 3.0 {
            0.08
        } else {
            0.15
        };
        current * (1.0 - blend) + avg * blend
    } else {
        sampler.height_at(world_x, world_z, max_y) as f32
    }
}
