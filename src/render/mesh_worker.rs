use std::sync::Arc;
use std::thread;

use crossbeam_channel::{Receiver, Sender, select};

use crate::blocks::BlockTextureLookup;
use crate::meshing::{LayeredChunkGeometry, MeshingNeighborhoodScratch};
use crate::perf::trace as perf_trace;
use crate::world::ChunkCoord;

pub(crate) use crate::meshing::{ChunkSnapshot, NeighborhoodSnapshot};

pub(crate) struct MeshJob {
    pub(crate) coord: ChunkCoord,
    pub(crate) revision: u64,
    pub(crate) mesh_debug_epoch: u32,
    pub(crate) snapshot: NeighborhoodSnapshot,
    pub(crate) use_greedy: bool,
}

pub(crate) struct MeshResult {
    pub(crate) coord: ChunkCoord,
    pub(crate) revision: u64,
    pub(crate) mesh_debug_epoch: u32,
    pub(crate) geometry: Option<LayeredChunkGeometry>,
}

pub(crate) struct MeshWorkerPool {
    shutdown_tx: Sender<()>,
    workers: Vec<thread::JoinHandle<()>>,
}

impl MeshWorkerPool {
    pub(crate) fn new(
        texture_lookup: Arc<BlockTextureLookup>,
        job_rx: Receiver<MeshJob>,
        result_tx: Sender<MeshResult>,
    ) -> Self {
        let worker_count = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .max(1);
        let (shutdown_tx, shutdown_rx) = crossbeam_channel::unbounded();
        let mut workers = Vec::with_capacity(worker_count);

        for worker_index in 0..worker_count {
            let job_rx = job_rx.clone();
            let result_tx = result_tx.clone();
            let shutdown_rx = shutdown_rx.clone();
            let texture_lookup = Arc::clone(&texture_lookup);
            let builder = thread::Builder::new().name(format!("mesh-worker-{worker_index}"));
            workers.push(builder.spawn(move || {
                let mut neighborhood_scratch = MeshingNeighborhoodScratch::new();
                loop {
                    select! {
                        recv(shutdown_rx) -> _ => break,
                        recv(job_rx) -> msg => {
                            let Ok(job) = msg else {
                                break;
                            };
                            let started = std::time::Instant::now();
                            let neighborhood_started = std::time::Instant::now();
                            let (neighborhood, neighborhood_stats) =
                                neighborhood_scratch.build(job.coord, job.snapshot);
                            let neighborhood_elapsed = neighborhood_started.elapsed();
                            let geometry_started = std::time::Instant::now();
                            let (geometry, mesh_stats) = crate::meshing::pool::with_meshing_pool(|pool| {
                                if job.use_greedy {
                                    crate::meshing::greedy::build_chunk_mesh_greedy_layered_dense_pooled(&neighborhood, job.coord, texture_lookup.as_ref(), pool)
                                } else {
                                    crate::meshing::greedy::build_chunk_mesh_layered_naive_dense_pooled(&neighborhood, job.coord, texture_lookup.as_ref(), pool)
                                }
                            });
                            let geometry_elapsed = geometry_started.elapsed();
                            let geometry = if geometry.total_index_count() == 0 {
                                None
                            } else {
                                Some(geometry)
                            };
                            let has_geom = geometry.is_some();

                            perf_trace::record_duration(
                                "mesh_worker.neighborhood_build",
                                neighborhood_elapsed,
                                &format!(
                                    "coord=({}, {}, {}) copied_voxels={} alloc_avoided={}",
                                    job.coord.x,
                                    job.coord.y,
                                    job.coord.z,
                                    neighborhood_stats.copied_voxels,
                                    neighborhood_stats.allocations_avoided
                                ),
                            );
                            perf_trace::record_duration(
                                "mesh_worker.mesh_generate",
                                geometry_elapsed,
                                &format!(
                                    "coord=({}, {}, {}) voxels={} faces_tested={} faces_emitted={} quads={} opaque_quads={} cutout_quads={} transparent_quads={} scratch_reused={} hint_reuses={} growths={}",
                                    job.coord.x,
                                    job.coord.y,
                                    job.coord.z,
                                    mesh_stats.voxels_visited,
                                    mesh_stats.cube_faces_considered,
                                    mesh_stats.cube_faces_emitted,
                                    mesh_stats.quads_emitted,
                                    mesh_stats.opaque_quads,
                                    mesh_stats.cutout_quads,
                                    mesh_stats.transparent_quads,
                                    mesh_stats.scratch_buffers_reused,
                                    mesh_stats.capacity_hints_used,
                                    mesh_stats.geometry_growth_events
                                ),
                            );

                            if result_tx
                                .send(MeshResult {
                                    coord: job.coord,
                                    revision: job.revision,
                                    mesh_debug_epoch: job.mesh_debug_epoch,
                                    geometry,
                                })
                                .is_err()
                            {
                                break;
                            }
                            perf_trace::record_duration(
                                "mesh_worker.job",
                                started.elapsed(),
                                &format!(
                                    "coord=({}, {}, {}) rev={} has_geom={} neighborhood_ms={:.2} mesh_ms={:.2} alloc_avoided={} voxels={} faces_tested={} faces_emitted={} quads={} opaque_quads={} cutout_quads={} transparent_quads={} scratch_reused={} hint_reuses={} growths={}",
                                    job.coord.x,
                                    job.coord.y,
                                    job.coord.z,
                                    job.revision,
                                    has_geom,
                                    neighborhood_elapsed.as_secs_f32() * 1000.0,
                                    geometry_elapsed.as_secs_f32() * 1000.0,
                                    neighborhood_stats.allocations_avoided,
                                    mesh_stats.voxels_visited,
                                    mesh_stats.cube_faces_considered,
                                    mesh_stats.cube_faces_emitted,
                                    mesh_stats.quads_emitted
                                    ,
                                    mesh_stats.opaque_quads,
                                    mesh_stats.cutout_quads,
                                    mesh_stats.transparent_quads,
                                    mesh_stats.scratch_buffers_reused,
                                    mesh_stats.capacity_hints_used,
                                    mesh_stats.geometry_growth_events
                                ),
                            );
                        }
                    }
                }
            }).expect("failed to spawn mesh worker"));
        }

        Self {
            shutdown_tx,
            workers,
        }
    }
}

impl Drop for MeshWorkerPool {
    fn drop(&mut self) {
        for _ in 0..self.workers.len() {
            let _ = self.shutdown_tx.send(());
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use bytemuck::cast_slice;
    use std::collections::HashMap;

    use super::*;
    use crate::blocks::{BlockId, BlockRegistry, BlockTextureLookup};
    use crate::meshing::{
        build_chunk_mesh_greedy_layered, build_chunk_mesh_layered_naive, flatten_layered_geometry,
    };
    use crate::world::{Chunk, World};

    fn test_texture_lookup() -> BlockTextureLookup {
        let registry = BlockRegistry::new();
        let key_to_layer: HashMap<String, u32> = registry
            .collect_texture_keys()
            .into_iter()
            .enumerate()
            .map(|(i, key)| (key, i as u32 + 1))
            .collect();
        BlockTextureLookup::new(&registry, &key_to_layer, 0)
    }

    fn snapshot_for(world: &World, coord: ChunkCoord) -> NeighborhoodSnapshot {
        let center = world
            .get_chunk(coord)
            .expect("missing center chunk for meshing snapshot");
        let center_snapshot = ChunkSnapshot {
            blocks: Arc::clone(&center.blocks),
            block_data: Arc::clone(&center.block_data),
            light: Arc::clone(&center.light),
        };
        let neighbors = [
            ChunkCoord {
                x: coord.x + 1,
                y: coord.y,
                z: coord.z,
            },
            ChunkCoord {
                x: coord.x - 1,
                y: coord.y,
                z: coord.z,
            },
            ChunkCoord {
                x: coord.x,
                y: coord.y + 1,
                z: coord.z,
            },
            ChunkCoord {
                x: coord.x,
                y: coord.y - 1,
                z: coord.z,
            },
            ChunkCoord {
                x: coord.x,
                y: coord.y,
                z: coord.z + 1,
            },
            ChunkCoord {
                x: coord.x,
                y: coord.y,
                z: coord.z - 1,
            },
        ]
        .map(|neighbor_coord| {
            world.get_chunk(neighbor_coord).map(|chunk| ChunkSnapshot {
                blocks: Arc::clone(&chunk.blocks),
                block_data: Arc::clone(&chunk.block_data),
                light: Arc::clone(&chunk.light),
            })
        });
        NeighborhoodSnapshot {
            center: center_snapshot,
            neighbors,
        }
    }

    #[test]
    fn dense_meshing_path_matches_world_mesher() {
        let mut world = World::new();
        let coord = ChunkCoord { x: 0, y: 0, z: 0 };
        let east = ChunkCoord { x: 1, y: 0, z: 0 };
        let up = ChunkCoord { x: 0, y: 1, z: 0 };

        let mut center = Chunk::new_empty();
        center.set(15, 8, 8, BlockId::Stone);
        center.set(15, 9, 8, BlockId::Stone);
        center.set(4, 4, 4, BlockId::Glass);
        center.set(5, 4, 4, BlockId::WaterStill);
        world.chunks.insert(coord, center);

        let mut east_chunk = Chunk::new_empty();
        east_chunk.set(0, 8, 8, BlockId::Stone);
        east_chunk.set(0, 9, 8, BlockId::Stone);
        world.chunks.insert(east, east_chunk);

        let mut up_chunk = Chunk::new_empty();
        up_chunk.set(15, 0, 8, BlockId::Stone);
        world.chunks.insert(up, up_chunk);

        world.rebuild_loaded_lighting();

        let texture_lookup = test_texture_lookup();
        let snapshot = snapshot_for(&world, coord);
        let mut scratch = MeshingNeighborhoodScratch::new();
        let (dense_neighborhood, _) = scratch.build(coord, snapshot);

        let dense_greedy = crate::meshing::pool::with_meshing_pool(|pool| {
            crate::meshing::greedy::build_chunk_mesh_greedy_layered_dense_pooled(
                &dense_neighborhood,
                coord,
                &texture_lookup,
                pool,
            )
            .0
        });
        let dense_naive = crate::meshing::pool::with_meshing_pool(|pool| {
            crate::meshing::greedy::build_chunk_mesh_layered_naive_dense_pooled(
                &dense_neighborhood,
                coord,
                &texture_lookup,
                pool,
            )
            .0
        });

        let world_greedy = build_chunk_mesh_greedy_layered(&world, coord, &texture_lookup);
        let world_naive = build_chunk_mesh_layered_naive(&world, coord, &texture_lookup);

        let (dense_greedy_vertices, dense_greedy_indices) = flatten_layered_geometry(dense_greedy);
        let (world_greedy_vertices, world_greedy_indices) = flatten_layered_geometry(world_greedy);
        assert_eq!(dense_greedy_indices, world_greedy_indices);
        assert_eq!(
            cast_slice::<_, u8>(&dense_greedy_vertices),
            cast_slice::<_, u8>(&world_greedy_vertices)
        );

        let (dense_naive_vertices, dense_naive_indices) = flatten_layered_geometry(dense_naive);
        let (world_naive_vertices, world_naive_indices) = flatten_layered_geometry(world_naive);
        assert_eq!(dense_naive_indices, world_naive_indices);
        assert_eq!(
            cast_slice::<_, u8>(&dense_naive_vertices),
            cast_slice::<_, u8>(&world_naive_vertices)
        );
    }
}
