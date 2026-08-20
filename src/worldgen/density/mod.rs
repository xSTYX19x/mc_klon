pub mod cache;

use std::sync::Arc;

use crate::worldgen::TerrainSampler;

const GRID_2D_SIZE: usize = 24;
const GRID_2D_OFFSET: i32 = 4;
struct GridCache2D {
    data: Vec<Option<(u32, f32)>>,
    base_x: i32,
    base_z: i32,
}

impl GridCache2D {
    fn new(base_x: i32, base_z: i32) -> Self {
        Self {
            data: vec![None; GRID_2D_SIZE * GRID_2D_SIZE],
            base_x,
            base_z,
        }
    }

    fn get(&self, node_id: u32, x: i32, z: i32) -> Option<f32> {
        let gx = x - self.base_x;
        let gz = z - self.base_z;
        if gx < 0 || gx >= GRID_2D_SIZE as i32 || gz < 0 || gz >= GRID_2D_SIZE as i32 {
            return None;
        }
        let idx = (gx as usize) + (gz as usize) * GRID_2D_SIZE;
        if let Some((cached_node_id, value)) = self.data[idx] {
            if cached_node_id == node_id {
                return Some(value);
            }
        }
        None
    }

    fn insert(&mut self, node_id: u32, x: i32, z: i32, value: f32) -> bool {
        let gx = x - self.base_x;
        let gz = z - self.base_z;
        if gx < 0 || gx >= GRID_2D_SIZE as i32 || gz < 0 || gz >= GRID_2D_SIZE as i32 {
            return false;
        }
        let idx = (gx as usize) + (gz as usize) * GRID_2D_SIZE;
        self.data[idx] = Some((node_id, value));
        true
    }
}

pub struct EvalContext {
    cache_2d: GridCache2D,
    height_cache_2d: GridCache2D,
}

impl EvalContext {
    pub fn new() -> Self {
        Self {
            cache_2d: GridCache2D::new(0, 0),
            height_cache_2d: GridCache2D::new(0, 0),
        }
    }

    pub fn init_for_chunk(&mut self, chunk_x: i32, chunk_z: i32) {
        let chunk_size = crate::world::CHUNK_SIZE;

        let base_x = chunk_x * chunk_size - GRID_2D_OFFSET;
        let base_z = chunk_z * chunk_size - GRID_2D_OFFSET;

        self.cache_2d = GridCache2D::new(base_x, base_z);
        self.height_cache_2d = GridCache2D::new(base_x, base_z);
    }
}

#[derive(Clone)]
pub enum DensityFn {
    Constant(f32),
    Y,
    Heightmap2D {
        node_id: u32,
        sampler: Arc<TerrainSampler>,
        max_y: i32,
        smooth: bool,
    },
    Add(Box<DensityFn>, Box<DensityFn>),
    Mul(Box<DensityFn>, Box<DensityFn>),
    Cache2D {
        node_id: u32,
        input: Box<DensityFn>,
    },
}

impl DensityFn {
    pub fn eval(&self, ctx: &mut EvalContext, x: i32, y: i32, z: i32) -> f32 {
        match self {
            Self::Constant(v) => *v,
            Self::Y => y as f32,
            Self::Heightmap2D {
                node_id,
                sampler,
                max_y,
                smooth,
            } => {
                if let Some(v) = ctx.height_cache_2d.get(*node_id, x, z) {
                    return v;
                }
                let value = if *smooth {
                    let current = sampler.height_at(x, z, *max_y) as f32;
                    let mut sum = 0.0_f32;
                    let mut count = 0.0_f32;
                    for dz in -1..=1 {
                        for dx in -1..=1 {
                            sum += sampler.height_at(x + dx, z + dz, *max_y) as f32;
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
                    sampler.height_at(x, z, *max_y) as f32
                };
                ctx.height_cache_2d.insert(*node_id, x, z, value);
                value
            }
            Self::Add(a, b) => a.eval(ctx, x, y, z) + b.eval(ctx, x, y, z),
            Self::Mul(a, b) => a.eval(ctx, x, y, z) * b.eval(ctx, x, y, z),
            Self::Cache2D { node_id, input } => {
                if let Some(v) = ctx.cache_2d.get(*node_id, x, z) {
                    return v;
                }
                let value = input.eval(ctx, x, y, z);
                ctx.cache_2d.insert(*node_id, x, z, value);
                value
            }
        }
    }
}
