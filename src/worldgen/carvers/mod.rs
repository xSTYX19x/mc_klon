mod caves;
mod ravine;

use crate::blocks::BlockId;
use crate::world::{CHUNK_SIZE, Chunk};

pub(crate) use caves::CaveCarver;
pub(crate) use ravine::RavineCarver;

pub(crate) struct Carvers;

impl Carvers {
    pub(crate) fn carve_column(
        world_seed: u64,
        target_chunk_x: i32,
        target_chunk_z: i32,
        column: &mut ChunkColumn,
        sea: &SeaGuard,
    ) {
        let water = WaterGuard::new(column, sea);
        let mut target = CarveTarget {
            column,
            water: &water,
        };
        CaveCarver::carve_target_column(world_seed, target_chunk_x, target_chunk_z, &mut target);
        RavineCarver::carve_target_column(world_seed, target_chunk_x, target_chunk_z, &mut target);
    }
}

/// The column being carved and the guard that keeps carving out of water.
pub(crate) struct CarveTarget<'a> {
    pub(crate) column: &'a mut ChunkColumn,
    pub(crate) water: &'a WaterGuard<'a>,
}

/// Chunk edges, in `SeaGuard::lowest_water_y` order.
pub(crate) const EDGE_NEG_X: usize = 0;
pub(crate) const EDGE_POS_X: usize = 1;
pub(crate) const EDGE_NEG_Z: usize = 2;
pub(crate) const EDGE_POS_Z: usize = 3;

/// Where sea water can sit just across this column's edges. The terrain phase
/// floods everything below `sea_level` above the terrain, but carvers run per
/// column and cannot see the neighbour's water, so the terrain phase hands them
/// this bound instead.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SeaGuard {
    sea_level: i32,
    /// Per edge and edge cell (z along the x edges, x along the z edges): the
    /// lowest y at which the cell across the edge may hold sea water. At or
    /// above `sea_level` means it never does.
    lowest_water_y: [[i32; CHUNK_SIZE as usize]; 4],
    /// Whether the column itself may hold sea water; `false` lets the carvers
    /// skip looking for it.
    column_may_hold_water: bool,
}

impl SeaGuard {
    /// Built from conservative bounds on the neighbours' terrain tops (see
    /// `WorldgenPipeline::sea_guard`): lower by at least a block, never higher.
    pub(crate) fn new(
        sea_level: i32,
        lowest_water_y: [[i32; CHUNK_SIZE as usize]; 4],
        column_may_hold_water: bool,
    ) -> Self {
        Self {
            sea_level,
            lowest_water_y,
            column_may_hold_water,
        }
    }

    /// No knowledge of the neighbours: assumes no water across the edges. Only
    /// for density trees without the heightmap fast path, which no preset uses.
    pub(crate) fn without_neighbours(sea_level: i32) -> Self {
        Self::new(sea_level, [[sea_level; CHUNK_SIZE as usize]; 4], true)
    }

    /// Whether the cell across `edge` from edge cell `edge_cell` may be sea
    /// water at height `y`.
    pub(crate) fn may_hold_water(&self, edge: usize, edge_cell: i32, y: i32) -> bool {
        y < self.sea_level && y >= self.lowest_water_y[edge][edge_cell as usize]
    }

    fn any_edge_may_hold_water(&self) -> bool {
        self.lowest_water_y
            .iter()
            .flatten()
            .any(|&lowest| lowest < self.sea_level)
    }
}

/// Keeps carvers from opening water to air: a cell with water directly above it
/// or beside it is left alone, so sea water always keeps its floor and walls.
pub(crate) struct WaterGuard<'a> {
    sea: &'a SeaGuard,
    /// Columns of this chunk holding water when carving starts. Carving never
    /// replaces water, so this stays true for the whole carve.
    wet_columns: [bool; (CHUNK_SIZE * CHUNK_SIZE) as usize],
    active: bool,
}

impl<'a> WaterGuard<'a> {
    fn new(column: &ChunkColumn, sea: &'a SeaGuard) -> Self {
        let mut wet_columns = [false; (CHUNK_SIZE * CHUNK_SIZE) as usize];
        // Only the sea exists before the lake phase, and it lies below sea level.
        let scan_top = if sea.column_may_hold_water {
            sea.sea_level.clamp(0, ChunkColumn::HEIGHT)
        } else {
            0
        };
        for z in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                wet_columns[(x + z * CHUNK_SIZE) as usize] =
                    (0..scan_top).any(|y| is_water(column.get_block(x, y, z)));
            }
        }
        let active = wet_columns.contains(&true) || sea.any_edge_may_hold_water();
        Self {
            sea,
            wet_columns,
            active,
        }
    }

    /// Whether turning (`x`, `y`, `z`) into air or lava would put water next to
    /// it or leave water above it unsupported.
    pub(crate) fn borders_water(&self, column: &ChunkColumn, x: i32, y: i32, z: i32) -> bool {
        if !self.active {
            return false;
        }
        if self.is_wet(column, x, y + 1, z) {
            return true;
        }
        for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
            let (nx, nz) = (x + dx, z + dz);
            let across = if nx < 0 {
                Some((EDGE_NEG_X, nz))
            } else if nx >= CHUNK_SIZE {
                Some((EDGE_POS_X, nz))
            } else if nz < 0 {
                Some((EDGE_NEG_Z, nx))
            } else if nz >= CHUNK_SIZE {
                Some((EDGE_POS_Z, nx))
            } else {
                None
            };
            let wet = match across {
                Some((edge, edge_cell)) => self.sea.may_hold_water(edge, edge_cell, y),
                None => self.is_wet(column, nx, y, nz),
            };
            if wet {
                return true;
            }
        }
        false
    }

    fn is_wet(&self, column: &ChunkColumn, x: i32, y: i32, z: i32) -> bool {
        self.wet_columns[(x + z * CHUNK_SIZE) as usize] && is_water(column.get_block(x, y, z))
    }
}

#[inline]
fn is_water(block: BlockId) -> bool {
    matches!(block, BlockId::WaterStill | BlockId::WaterFlowing)
}

#[derive(Clone, Debug)]
pub(crate) struct ChunkColumn {
    pub(crate) blocks: Vec<BlockId>,
    pub(crate) data: Vec<u8>,
}

impl ChunkColumn {
    pub(crate) const WIDTH: i32 = 16;
    pub(crate) const HEIGHT: i32 = 256;
    pub(crate) const DEPTH: i32 = 16;
    const LAYER_AREA: usize = (Self::WIDTH as usize) * (Self::DEPTH as usize);
    const VOLUME: usize = Self::LAYER_AREA * (Self::HEIGHT as usize);

    pub(crate) fn new() -> Self {
        Self {
            blocks: vec![BlockId::Air; Self::VOLUME],
            data: vec![0; Self::VOLUME],
        }
    }

    #[inline]
    pub(crate) fn index(x: i32, y: i32, z: i32) -> usize {
        debug_assert!((0..Self::WIDTH).contains(&x));
        debug_assert!((0..Self::HEIGHT).contains(&y));
        debug_assert!((0..Self::DEPTH).contains(&z));
        (x as usize) + (z as usize) * 16 + (y as usize) * 16 * 16
    }

    #[inline]
    pub(crate) fn in_bounds(x: i32, y: i32, z: i32) -> bool {
        (0..Self::WIDTH).contains(&x)
            && (0..Self::HEIGHT).contains(&y)
            && (0..Self::DEPTH).contains(&z)
    }

    #[inline]
    pub(crate) fn get_block(&self, x: i32, y: i32, z: i32) -> BlockId {
        if !Self::in_bounds(x, y, z) {
            return BlockId::Air;
        }
        self.blocks[Self::index(x, y, z)]
    }

    #[inline]
    pub(crate) fn set_block(&mut self, x: i32, y: i32, z: i32, block: BlockId) {
        if Self::in_bounds(x, y, z) {
            let idx = Self::index(x, y, z);
            self.blocks[idx] = block;
        }
    }

    #[inline]
    pub(crate) fn get_data(&self, x: i32, y: i32, z: i32) -> u8 {
        if !Self::in_bounds(x, y, z) {
            return 0;
        }
        self.data[Self::index(x, y, z)]
    }

    #[inline]
    pub(crate) fn set_data(&mut self, x: i32, y: i32, z: i32, value: u8) {
        if Self::in_bounds(x, y, z) {
            let idx = Self::index(x, y, z);
            self.data[idx] = value;
        }
    }

    pub(crate) fn write_chunk_section(&mut self, chunk_y: i32, chunk: &Chunk) {
        if !(0..Self::chunk_count_y()).contains(&chunk_y) {
            return;
        }

        let base_world_y = chunk_y * CHUNK_SIZE;
        for local_y in 0..CHUNK_SIZE {
            let world_y = base_world_y + local_y;
            let dst_start = (world_y as usize) * Self::LAYER_AREA;
            let dst_end = dst_start + Self::LAYER_AREA;
            let src_start = (local_y as usize) * Self::LAYER_AREA;
            let src_end = src_start + Self::LAYER_AREA;
            chunk
                .blocks
                .copy_range_into(src_start, &mut self.blocks[dst_start..dst_end]);
            self.data[dst_start..dst_end].copy_from_slice(&chunk.block_data[src_start..src_end]);
        }
    }

    pub(crate) fn extract_chunk(&self, chunk_y: i32) -> Chunk {
        let mut chunk = Chunk::new_empty();
        if !(0..Self::chunk_count_y()).contains(&chunk_y) {
            return chunk;
        }
        let blocks = chunk.blocks.make_flat_mut();
        let data = std::sync::Arc::make_mut(&mut chunk.block_data);

        let base_world_y = chunk_y * CHUNK_SIZE;
        for local_y in 0..CHUNK_SIZE {
            let world_y = base_world_y + local_y;
            let src_start = (world_y as usize) * Self::LAYER_AREA;
            let src_end = src_start + Self::LAYER_AREA;
            let dst_start = (local_y as usize) * Self::LAYER_AREA;
            let dst_end = dst_start + Self::LAYER_AREA;
            blocks[dst_start..dst_end].copy_from_slice(&self.blocks[src_start..src_end]);
            data[dst_start..dst_end].copy_from_slice(&self.data[src_start..src_end]);
        }
        chunk.recompute_cached_block_classification();
        chunk
    }

    pub(crate) fn chunk_count_y() -> i32 {
        Self::HEIGHT / CHUNK_SIZE
    }
}
