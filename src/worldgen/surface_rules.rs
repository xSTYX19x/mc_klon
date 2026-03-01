use crate::blocks::BlockId;

#[derive(Clone, Copy, Debug)]
pub struct SurfaceContext {
    pub x: i32,
    pub z: i32,
    pub top_y: i32,
    pub sea_level: i32,
    pub near_water: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct SurfaceRules {
    pub dirt_depth: i32,
    pub shore_band: i32,
}

impl Default for SurfaceRules {
    fn default() -> Self {
        Self {
            dirt_depth: 3,
            shore_band: 1,
        }
    }
}

impl SurfaceRules {
    pub fn block_for_solid(&self, y: i32, ctx: SurfaceContext) -> BlockId {
        if y == 0 {
            return BlockId::Bedrock;
        }

        let dirt_depth = self.dirt_depth.max(1);
        let shoreline = ctx.near_water || (ctx.top_y - ctx.sea_level).abs() <= self.shore_band;
        let _ = (ctx.x, ctx.z);

        if y == ctx.top_y {
            if !shoreline {
                BlockId::Grass
            } else {
                BlockId::Dirt
            }
        } else if y >= ctx.top_y - dirt_depth {
            BlockId::Dirt
        } else {
            BlockId::Stone
        }
    }
}
