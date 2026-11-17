use glam::IVec3;

use crate::blocks::{BlockRegistry, BlockState, RandomTickContext};
use crate::world::PrecipitationKind;
use crate::sim::{ActiveSimColumns, ColumnCoord, SimSystem};
use crate::util::rng::{Rng64, hash64};
use crate::world::{CHUNK_SIZE, ChunkCoord, World};

pub(crate) struct RandomTicksSystem {
    pub(crate) ticks_per_chunk: u32,
    /// Set by `State` before each tick (`set_precipitation`).
    precipitation: Option<PrecipitationKind>,
    active_chunks: Vec<ChunkCoord>,
    sampled_last_tick: usize,
    callbacks_last_tick: usize,
}

impl RandomTicksSystem {
    pub(crate) fn new(ticks_per_chunk: u32) -> Self {
        Self {
            ticks_per_chunk,
            precipitation: None,
            active_chunks: Vec::new(),
            sampled_last_tick: 0,
            callbacks_last_tick: 0,
        }
    }

    /// The precipitation the next ticks see (snow layers grow while it snows).
    pub(crate) fn set_precipitation(&mut self, precipitation: Option<PrecipitationKind>) {
        self.precipitation = precipitation;
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
        let ctx = RandomTickContext {
            tick_index,
            precipitation: self.precipitation,
        };
        for coord in self.active_chunks.iter().copied() {
            // Most sections (stone, air, plain dirt) hold nothing with a random
            // tick, so drawing samples there only costs time. The count is read
            // live: a tick in an earlier section can make this one tickable.
            // Skipping cannot change results, because the RNG is seeded per
            // section and a section without ticking blocks fires no callback.
            if !world
                .chunks
                .get(&coord)
                .is_some_and(|chunk| chunk.has_random_ticking_blocks())
            {
                continue;
            }
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

                let block = world.get_block(world_pos);
                if let Some(random_tick) = BlockRegistry::get().random_tick_for(block) {
                    let state = BlockState::new(block, world.get_block_data(world_pos));
                    random_tick(world, world_pos, state, &ctx);
                    self.callbacks_last_tick += 1;
                }
                self.sampled_last_tick += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::BlockId;
    use crate::world::{BlockChange, Chunk, ChunkBlocks};
    use std::sync::Arc;
    use std::time::Instant;

    const VOXELS: usize = (CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE) as usize;

    /// The full-sampling loop as it was before sections without random-ticking
    /// blocks were skipped: every loaded section draws its samples.
    fn reference_fixed_tick(world: &mut World, ticks_per_chunk: u32, tick_index: u64) {
        let registry = BlockRegistry::get();
        let mut coords: Vec<ChunkCoord> = world.chunks.keys().copied().collect();
        coords.sort_unstable_by_key(|c| (c.x, c.y, c.z));
        let world_seed = world.seed();
        for coord in coords {
            let mut rng = Rng64::new(RandomTicksSystem::chunk_tick_seed(
                world_seed, coord, tick_index,
            ));
            for _ in 0..ticks_per_chunk {
                let local_x = (rng.next_u32() % CHUNK_SIZE as u32) as i32;
                let local_y = (rng.next_u32() % CHUNK_SIZE as u32) as i32;
                let local_z = (rng.next_u32() % CHUNK_SIZE as u32) as i32;
                let pos = IVec3::new(
                    coord.x * CHUNK_SIZE + local_x,
                    coord.y * CHUNK_SIZE + local_y,
                    coord.z * CHUNK_SIZE + local_z,
                );
                let block = world.get_block(pos);
                if let Some(random_tick) = registry.random_tick_for(block) {
                    let state = BlockState::new(block, world.get_block_data(pos));
                    random_tick(world, pos, state, &RandomTickContext::at(tick_index));
                }
            }
        }
    }

    /// Stone everywhere, and one section (0, 4, 0) with a grass/dirt layer
    /// under open air plus grass buried under stone. Only that section has
    /// random-ticking blocks, and nothing it does reaches another section, so
    /// the result cannot depend on the order sections are visited in.
    fn one_live_section_world() -> World {
        let mut world = World::new();
        let data = Arc::new(vec![0u8; VOXELS]);
        let light = Arc::new(vec![0u8; VOXELS]);
        for cx in -2..=2 {
            for cz in -2..=2 {
                for cy in 0..4 {
                    world.chunks.insert(
                        ChunkCoord {
                            x: cx,
                            y: cy,
                            z: cz,
                        },
                        Chunk::from_parts(
                            ChunkBlocks::Uniform(BlockId::Stone),
                            Arc::clone(&data),
                            Arc::clone(&light),
                            0,
                        ),
                    );
                }
            }
        }
        let mut live = vec![BlockId::Air; VOXELS];
        for z in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                for y in 0..8 {
                    live[Chunk::index(x, y, z)] = BlockId::Stone;
                }
                live[Chunk::index(x, 8, z)] = if (x + z) % 3 == 0 {
                    BlockId::Grass
                } else {
                    BlockId::Dirt
                };
            }
        }
        for x in 2..6 {
            live[Chunk::index(x, 6, 2)] = BlockId::Grass;
        }
        world.chunks.insert(
            ChunkCoord { x: 0, y: 4, z: 0 },
            Chunk::from_parts(ChunkBlocks::compact(live), data, light, 0),
        );
        world
    }

    #[test]
    fn skipping_matches_full_sampling_reference() {
        let is_active = |_: i32, _: i32| true;
        let mut system_world = one_live_section_world();
        let mut reference_world = one_live_section_world();
        let mut system = RandomTicksSystem::new(64);
        let (mut got, mut want) = (Vec::<BlockChange>::new(), Vec::<BlockChange>::new());
        for tick in 0..60 {
            system.fixed_tick(
                &mut system_world,
                0.05,
                tick,
                ActiveSimColumns {
                    is_active: &is_active,
                },
            );
            reference_fixed_tick(&mut reference_world, 64, tick);
        }
        system_world.drain_block_changes(&mut got);
        reference_world.drain_block_changes(&mut want);
        assert!(!want.is_empty(), "the reference changed nothing");
        assert_eq!(got, want);
    }

    /// Before/after numbers for `docs/TODO.md` "Random Ticks: Sektionen ohne
    /// tickbare Blöcke überspringen". Run with
    /// `cargo test --release random_tick_cost_report -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing report, run manually in release"]
    fn random_tick_cost_report() {
        let mut world = World::new();
        for cx in -2..=2 {
            for cz in -2..=2 {
                world.ensure_column_generated(cx * CHUNK_SIZE, cz * CHUNK_SIZE);
            }
        }
        let sections = world.chunks.len();
        let is_active = |_: i32, _: i32| true;
        let mut journal = Vec::new();
        for (speed, ticks) in [(3u32, 2000u64), (1000, 20)] {
            let mut system = RandomTicksSystem::new(speed);
            let (mut sampled, mut callbacks, mut changes) = (0usize, 0usize, 0usize);
            let start = Instant::now();
            for tick in 0..ticks {
                system.fixed_tick(
                    &mut world,
                    0.05,
                    tick,
                    ActiveSimColumns {
                        is_active: &is_active,
                    },
                );
                sampled += system.sampled_last_tick;
                callbacks += system.callbacks_last_tick;
                world.drain_block_changes(&mut journal);
                changes += journal.len();
                journal.clear();
            }
            let per_tick_ms = start.elapsed().as_secs_f64() * 1000.0 / ticks as f64;
            println!(
                "speed {speed:4}: {sections} sections, {per_tick_ms:8.3} ms per tick, \
                 {} samples, {} callbacks, {:.2} changes per tick",
                sampled as u64 / ticks,
                callbacks as u64 / ticks,
                changes as f64 / ticks as f64
            );
        }
    }
}
