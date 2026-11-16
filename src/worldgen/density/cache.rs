use crate::world::{CHUNK_SIZE, ChunkCoord};

use super::{DensityFn, EvalContext};

pub const DENSITY_SAMPLE_STEP: i32 = 4;
const GRID_AXIS: usize = (CHUNK_SIZE as usize / DENSITY_SAMPLE_STEP as usize) + 1;
const GRID_LEN: usize = GRID_AXIS * GRID_AXIS * GRID_AXIS;
/// Density-grid points per horizontal layer (5×5 for 16-wide chunks).
pub const GRID_COLUMNS: usize = GRID_AXIS * GRID_AXIS;

#[derive(Clone)]
pub struct DensityChunkCache {
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

        Self { grid }
    }

    /// The heightmap (`height_at(world_x, world_z)`) at the density-grid
    /// points of chunk column (`chunk_x`, `chunk_z`). It does not depend on y,
    /// so a column computes it once and builds every section's cache from it
    /// (`from_grid_heights`).
    pub fn grid_heights(
        chunk_x: i32,
        chunk_z: i32,
        height_at: impl Fn(i32, i32) -> f32,
    ) -> [f32; GRID_COLUMNS] {
        let chunk_world_x0 = chunk_x * CHUNK_SIZE;
        let chunk_world_z0 = chunk_z * CHUNK_SIZE;
        let mut heights = [0.0_f32; GRID_COLUMNS];
        for gz in 0..GRID_AXIS {
            let world_z = chunk_world_z0 + (gz as i32) * DENSITY_SAMPLE_STEP;
            for gx in 0..GRID_AXIS {
                let world_x = chunk_world_x0 + (gx as i32) * DENSITY_SAMPLE_STEP;
                heights[gx + gz * GRID_AXIS] = height_at(world_x, world_z);
            }
        }
        heights
    }

    /// Density grid of the "heightmap minus y" terrain for the section
    /// starting at `chunk_world_y0`, from that column's `grid_heights`.
    pub fn from_grid_heights(chunk_world_y0: i32, heights: &[f32; GRID_COLUMNS]) -> Self {
        let mut grid = [0.0_f32; GRID_LEN];
        for gz in 0..GRID_AXIS {
            for gy in 0..GRID_AXIS {
                let world_y = chunk_world_y0 + (gy as i32) * DENSITY_SAMPLE_STEP;
                for gx in 0..GRID_AXIS {
                    let heightmap = heights[gx + gz * GRID_AXIS];
                    grid[Self::grid_index(gx, gy, gz)] = heightmap + (-1.0 * world_y as f32);
                }
            }
        }
        Self { grid }
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

    #[inline]
    fn grid_index(gx: usize, gy: usize, gz: usize) -> usize {
        gx + gy * GRID_AXIS + gz * GRID_AXIS * GRID_AXIS
    }

    #[inline]
    fn lerp(a: f32, b: f32, t: f32) -> f32 {
        a + (b - a) * t
    }
}

/// The heightmap value at (`world_x`, `world_z`) from raw heights. With
/// `smooth`, it blends the height with its 3×3 neighbourhood. A caller that
/// already holds the raw heights of an area (a chunk column) passes a lookup
/// instead of resampling; the arithmetic is the same either way, so the
/// result is bit-identical.
#[inline]
pub fn smoothed_height(
    raw_height: impl Fn(i32, i32) -> i32,
    smooth: bool,
    world_x: i32,
    world_z: i32,
) -> f32 {
    if smooth {
        let current = raw_height(world_x, world_z) as f32;
        let mut sum = 0.0_f32;
        let mut count = 0.0_f32;
        for dz in -1..=1 {
            for dx in -1..=1 {
                sum += raw_height(world_x + dx, world_z + dz) as f32;
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
        raw_height(world_x, world_z) as f32
    }
}
