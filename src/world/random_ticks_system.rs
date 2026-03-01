use glam::IVec3;

use crate::blocks::BlockRegistry;
use crate::sim::{ActiveSimColumns, ColumnCoord, SimSystem};
use crate::util::rng::{Rng64, hash64};
use crate::world::{CHUNK_SIZE, ChunkCoord, World};

pub(crate) struct RandomTicksSystem {
    pub(crate) ticks_per_chunk: u32,
    block_registry: BlockRegistry,
    active_chunks: Vec<ChunkCoord>,
    sampled_last_tick: usize,
    callbacks_last_tick: usize,
}

impl RandomTicksSystem {
    pub(crate) fn new(ticks_per_chunk: u32) -> Self {
        Self {
            ticks_per_chunk,
            block_registry: BlockRegistry::new(),
            active_chunks: Vec::new(),
            sampled_last_tick: 0,
            callbacks_last_tick: 0,
        }
    }

    pub(crate) fn debug_counters(&self) -> (u32, usize, usize) {
        (
            self.ticks_per_chunk,
            self.sampled_last_tick,
            self.callbacks_last_tick,
        )
    }

    fn chunk_tick_seed(world_seed: u64, coord: ChunkCoord, tick_index: u64) -> u64 {
        let mut seed = world_seed;
        seed ^= (coord.x as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        seed ^= (coord.y as i64 as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        seed ^= (coord.z as i64 as u64).wrapping_mul(0x94D0_49BB_1331_11EB);
        seed ^= tick_index.wrapping_mul(0xD6E8_FEB8_6659_FD93);
        hash64(seed)
    }
}

impl SimSystem for RandomTicksSystem {
    fn name(&self) -> &'static str {
        "random_ticks"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn on_columns_entered(
        &mut self,
        _world: &mut World,
        _cols: &[ColumnCoord],
        _active: ActiveSimColumns<'_>,
    ) {
    }

    fn on_columns_exited(
        &mut self,
        _world: &mut World,
        _cols: &[ColumnCoord],
        _active: ActiveSimColumns<'_>,
    ) {
    }

    fn fixed_tick(
        &mut self,
        world: &mut World,
        _tick_dt: f32,
        tick_index: u64,
        active: ActiveSimColumns<'_>,
    ) {
        self.sampled_last_tick = 0;
        self.callbacks_last_tick = 0;
        if self.ticks_per_chunk == 0 {
            return;
        }

        self.active_chunks.clear();
        self.active_chunks.extend(
            world
                .chunks
                .keys()
                .filter(|coord| (active.is_active)(coord.x, coord.z))
                .copied(),
        );

        let world_seed = world.seed();
        for coord in self.active_chunks.iter().copied() {
            let mut rng = Rng64::new(Self::chunk_tick_seed(world_seed, coord, tick_index));
            let chunk_x0 = coord.x * CHUNK_SIZE;
            let chunk_y0 = coord.y * CHUNK_SIZE;
            let chunk_z0 = coord.z * CHUNK_SIZE;
            for _ in 0..self.ticks_per_chunk {
                let local_x = (rng.next_u32() % CHUNK_SIZE as u32) as i32;
                let local_y = (rng.next_u32() % CHUNK_SIZE as u32) as i32;
                let local_z = (rng.next_u32() % CHUNK_SIZE as u32) as i32;
                let world_pos =
                    IVec3::new(chunk_x0 + local_x, chunk_y0 + local_y, chunk_z0 + local_z);

                let state = world.get_block(world_pos);
                if let Some(random_tick) = self.block_registry.random_tick_for(state) {
                    random_tick(world, world_pos, state, tick_index);
                    self.callbacks_last_tick += 1;
                }
                self.sampled_last_tick += 1;
            }
        }
    }
}
