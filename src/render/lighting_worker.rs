use std::collections::HashMap;
use std::thread;

use crossbeam_channel::{Receiver, Sender, select};

use crate::perf::trace as perf_trace;
use crate::world::{ChunkCoord, SnapshotWorldLighting, rebuild_lighting_for_snapshot};

pub(crate) struct LightingJob {
    pub(crate) generation: u64,
    pub(crate) snapshot: SnapshotWorldLighting,
}

pub(crate) struct LightingResult {
    pub(crate) generation: u64,
    pub(crate) chunk_lights: HashMap<ChunkCoord, Vec<u8>>,
}

pub(crate) struct LightingWorkerPool {
    shutdown_tx: Sender<()>,
    workers: Vec<thread::JoinHandle<()>>,
}

impl LightingWorkerPool {
    pub(crate) fn new(job_rx: Receiver<LightingJob>, result_tx: Sender<LightingResult>) -> Self {
        let worker_count = 1;
        let (shutdown_tx, shutdown_rx) = crossbeam_channel::unbounded();
        let mut workers = Vec::with_capacity(worker_count);

        for worker_index in 0..worker_count {
            let job_rx = job_rx.clone();
            let result_tx = result_tx.clone();
            let shutdown_rx = shutdown_rx.clone();
            let builder = thread::Builder::new().name(format!("lighting-worker-{worker_index}"));
            workers.push(builder.spawn(move || {
                loop {
                    select! {
                        recv(shutdown_rx) -> _ => break,
                        recv(job_rx) -> msg => {
                            let Ok(job) = msg else {
                                break;
                            };
                            let started = std::time::Instant::now();
                            let mut snapshot = job.snapshot;
                            rebuild_lighting_for_snapshot(&mut snapshot);
                            let chunk_lights = snapshot.into_light_buffers();
                            let changed_buffers = chunk_lights.len();
                            if result_tx
                                .send(LightingResult {
                                    generation: job.generation,
                                    chunk_lights,
                                })
                                .is_err()
                            {
                                break;
                            }
                            perf_trace::record_duration(
                                "lighting_worker.job",
                                started.elapsed(),
                                &format!("generation={} chunks={}", job.generation, changed_buffers),
                            );
                        }
                    }
                }
            }).expect("failed to spawn lighting worker"));
        }

        Self {
            shutdown_tx,
            workers,
        }
    }
}

impl Drop for LightingWorkerPool {
    fn drop(&mut self) {
        for _ in 0..self.workers.len() {
            let _ = self.shutdown_tx.send(());
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
