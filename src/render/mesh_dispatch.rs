//! How many mesh jobs may be outstanding at the workers at once.
//!
//! The job channel is unbounded and the workers are FIFO, so whatever order the
//! dispatcher sends in is frozen at enqueue time. With 148 jobs in flight (as
//! measured), a chunk that became near-and-visible *this* frame waits behind
//! ~140 chunks that were near two seconds ago — the dirty queue is
//! reprioritized every frame, but only for the part that has not been sent yet.
//! Capping how much may sit at the workers keeps the reorderable part large,
//! which is where priority still means something.
//!
//! The cap is deliberately *not* a fixed number: too low starves the pool, too
//! high restores the inversion. Sizing it off the worker count keeps every
//! worker fed (each has a job plus at least one queued behind it) on any machine.

/// Jobs allowed at the workers per worker thread during normal play.
///
/// Three, not two: the dispatcher runs once per frame, so a worker that empties
/// its share early would idle until the next frame. Three keeps the pool fed
/// while the cap stays far below the per-frame rate the upload budget can
/// absorb — uploads must remain the limiter, not dispatch.
const JOBS_PER_WORKER: usize = 3;
/// During warmup, throughput outranks ordering: the loading screen is up and
/// nothing is being looked at yet, so the pool is allowed a deeper queue.
const WARMUP_JOBS_PER_WORKER: usize = 8;
/// Floor for machines that report a tiny parallelism, so dispatch cannot grind
/// to a near-halt on a single-core report.
const MIN_INFLIGHT_JOBS: usize = 4;

/// Upper bound on dispatched-but-unfinished mesh jobs.
pub(crate) fn mesh_inflight_job_cap(worker_count: usize, warmup: bool) -> usize {
    let per_worker = if warmup {
        WARMUP_JOBS_PER_WORKER
    } else {
        JOBS_PER_WORKER
    };
    worker_count
        .max(1)
        .saturating_mul(per_worker)
        .max(MIN_INFLIGHT_JOBS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_worker_gets_a_job_plus_one_queued_behind_it() {
        // The starvation guard: a cap below the worker count would idle threads.
        for workers in 1..=32 {
            let cap = mesh_inflight_job_cap(workers, false);
            assert!(
                cap >= workers,
                "cap {cap} would leave workers idle at {workers} threads"
            );
            assert!(cap >= MIN_INFLIGHT_JOBS);
        }
        assert_eq!(mesh_inflight_job_cap(8, false), 24);
    }

    #[test]
    fn the_cap_stays_far_below_the_measured_inversion() {
        // `inflight(mesh=148)` with upload waits up to 413 ms is what this fixes.
        assert!(mesh_inflight_job_cap(16, false) < 148);
    }

    #[test]
    fn warmup_is_allowed_a_deeper_queue() {
        assert!(mesh_inflight_job_cap(8, true) > mesh_inflight_job_cap(8, false));
    }

    #[test]
    fn a_degenerate_worker_count_still_dispatches() {
        assert_eq!(mesh_inflight_job_cap(0, false), MIN_INFLIGHT_JOBS);
        assert_eq!(mesh_inflight_job_cap(1, false), MIN_INFLIGHT_JOBS);
    }
}
