use std::sync::Arc;
use std::thread;

use crossbeam_channel::{Receiver, Sender, select};
use glam::IVec3;

use crate::blocks::{BlockId, BlockTextureLookup};
use crate::meshing::LayeredChunkGeometry;
use crate::perf::trace as perf_trace;
use crate::world::{BlockAccess, CHUNK_SIZE, Chunk, ChunkCoord};

#[derive(Clone)]
pub(crate) struct ChunkSnapshot {
    pub(crate) blocks: Arc<Vec<BlockId>>,
    pub(crate) block_data: Arc<Vec<u8>>,
    pub(crate) light: Arc<Vec<u8>>,
}

#[derive(Clone)]
pub(crate) struct NeighborhoodSnapshot {
    pub(crate) center: ChunkSnapshot,
    // Reihenfolge: +X, -X, +Y, -Y, +Z, -Z
    pub(crate) neighbors: [Option<ChunkSnapshot>; 6],
}

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

pub(crate) struct SnapshotWorld {
    origin_coord: ChunkCoord,
    center: Chunk,
    neighbors: [Option<Chunk>; 6],
}

impl SnapshotWorld {
    pub(crate) fn from_snapshot(origin_coord: ChunkCoord, snapshot: NeighborhoodSnapshot) -> Self {
        Self {
            origin_coord,
            center: Chunk::from_parts_readonly(
                snapshot.center.blocks,
                snapshot.center.block_data,
                snapshot.center.light,
                0,
            ),
            neighbors: snapshot.neighbors.map(|chunk| {
                chunk.map(|c| Chunk::from_parts_readonly(c.blocks, c.block_data, c.light, 0))
            }),
        }
    }

    fn neighbor_coord_for_index(origin: ChunkCoord, index: usize) -> ChunkCoord {
        match index {
            0 => ChunkCoord {
                x: origin.x + 1,
                y: origin.y,
                z: origin.z,
            },
            1 => ChunkCoord {
                x: origin.x - 1,
                y: origin.y,
                z: origin.z,
            },
            2 => ChunkCoord {
                x: origin.x,
                y: origin.y + 1,
                z: origin.z,
            },
            3 => ChunkCoord {
                x: origin.x,
                y: origin.y - 1,
                z: origin.z,
            },
            4 => ChunkCoord {
                x: origin.x,
                y: origin.y,
                z: origin.z + 1,
            },
            5 => ChunkCoord {
                x: origin.x,
                y: origin.y,
                z: origin.z - 1,
            },
            _ => origin,
        }
    }
}

impl BlockAccess for SnapshotWorld {
    fn get_block(&self, pos: IVec3) -> BlockId {
        let coord = ChunkCoord {
            x: pos.x.div_euclid(CHUNK_SIZE),
            y: pos.y.div_euclid(CHUNK_SIZE),
            z: pos.z.div_euclid(CHUNK_SIZE),
        };
        let local = IVec3::new(
            pos.x.rem_euclid(CHUNK_SIZE),
            pos.y.rem_euclid(CHUNK_SIZE),
            pos.z.rem_euclid(CHUNK_SIZE),
        );
        self.get_chunk(coord)
            .map_or(BlockId::Air, |chunk| chunk.get(local.x, local.y, local.z))
    }

    fn get_block_data(&self, pos: IVec3) -> u8 {
        let coord = ChunkCoord {
            x: pos.x.div_euclid(CHUNK_SIZE),
            y: pos.y.div_euclid(CHUNK_SIZE),
            z: pos.z.div_euclid(CHUNK_SIZE),
        };
        let local = IVec3::new(
            pos.x.rem_euclid(CHUNK_SIZE),
            pos.y.rem_euclid(CHUNK_SIZE),
            pos.z.rem_euclid(CHUNK_SIZE),
        );
        self.get_chunk(coord)
            .map_or(0, |chunk| chunk.get_data(local.x, local.y, local.z))
    }

    fn get_light_packed(&self, pos: IVec3) -> u8 {
        if pos.y > crate::world::WORLD_MAX_Y {
            return 0xF0;
        }
        if pos.y < 0 {
            return 0;
        }
        let coord = ChunkCoord {
            x: pos.x.div_euclid(CHUNK_SIZE),
            y: pos.y.div_euclid(CHUNK_SIZE),
            z: pos.z.div_euclid(CHUNK_SIZE),
        };
        let local = IVec3::new(
            pos.x.rem_euclid(CHUNK_SIZE),
            pos.y.rem_euclid(CHUNK_SIZE),
            pos.z.rem_euclid(CHUNK_SIZE),
        );
        self.get_chunk(coord).map_or(0xF0, |chunk| {
            chunk.get_light_packed(local.x, local.y, local.z)
        })
    }

    fn get_chunk(&self, coord: ChunkCoord) -> Option<&Chunk> {
        if coord == self.origin_coord {
            return Some(&self.center);
        }
        for index in 0..self.neighbors.len() {
            if coord == Self::neighbor_coord_for_index(self.origin_coord, index) {
                return self.neighbors[index].as_ref();
            }
        }
        None
    }
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
                loop {
                    select! {
                        recv(shutdown_rx) -> _ => break,
                        recv(job_rx) -> msg => {
                            let Ok(job) = msg else {
                                break;
                            };
                            let started = std::time::Instant::now();

                            let snapshot_world = SnapshotWorld::from_snapshot(job.coord, job.snapshot);
                            let geometry = crate::meshing::pool::with_meshing_pool(|pool| {
                                if job.use_greedy {
                                    crate::meshing::greedy::build_chunk_mesh_greedy_layered_pooled(&snapshot_world, job.coord, texture_lookup.as_ref(), pool)
                                } else {
                                    crate::meshing::greedy::build_chunk_mesh_layered_naive_pooled(&snapshot_world, job.coord, texture_lookup.as_ref(), pool)
                                }
                            });
                            let geometry = if geometry.total_index_count() == 0 {
                                None
                            } else {
                                Some(geometry)
                            };
                            let has_geom = geometry.is_some();

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
                                    "coord=({}, {}, {}) rev={} has_geom={}",
                                    job.coord.x,
                                    job.coord.y,
                                    job.coord.z,
                                    job.revision,
                                    has_geom
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
