use std::sync::atomic::{AtomicUsize, Ordering};

/// Global allocation tracking for performance measurement
/// Tracks HashMap allocations in liquid meshing before and after optimization
pub(crate) struct AllocationTracker {
    total_hashmap_allocations: AtomicUsize,
    mesh_jobs_completed: AtomicUsize,
}

impl AllocationTracker {
    pub(crate) const fn new() -> Self {
        Self {
            total_hashmap_allocations: AtomicUsize::new(0),
            mesh_jobs_completed: AtomicUsize::new(0),
        }
    }

    /// Track a HashMap allocation (called in BEFORE implementation)
    pub(crate) fn track_hashmap_allocation(&self) {
        self.total_hashmap_allocations
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Track completion of a mesh job
    pub(crate) fn track_mesh_job(&self) {
        self.mesh_jobs_completed.fetch_add(1, Ordering::Relaxed);
    }

    /// Get current allocation statistics
    pub(crate) fn stats(&self) -> AllocationStats {
        let allocations = self.total_hashmap_allocations.load(Ordering::Relaxed);
        let jobs = self.mesh_jobs_completed.load(Ordering::Relaxed);

        AllocationStats {
            total_hashmap_allocations: allocations,
            mesh_jobs_completed: jobs,
            allocations_per_job: if jobs > 0 {
                allocations as f32 / jobs as f32
            } else {
                0.0
            },
        }
    }

    /// Reset counters for a fresh measurement
    pub(crate) fn reset(&self) {
        self.total_hashmap_allocations.store(0, Ordering::Relaxed);
        self.mesh_jobs_completed.store(0, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AllocationStats {
    pub(crate) total_hashmap_allocations: usize,
    pub(crate) mesh_jobs_completed: usize,
    pub(crate) allocations_per_job: f32,
}

impl AllocationStats {
    /// Compare with expected stats for the AFTER implementation
    pub(crate) fn improvement_vs_baseline(&self) -> AllocationImprovement {
        // BEFORE: 4 HashMap allocations per meshing call (lines 406-409 in original code)
        const BASELINE_ALLOCATIONS_PER_JOB: f32 = 4.0;

        let improvement_factor = if self.allocations_per_job > 0.0 {
            BASELINE_ALLOCATIONS_PER_JOB / self.allocations_per_job
        } else {
            f32::INFINITY // Perfect - no allocations
        };

        let allocations_saved = (BASELINE_ALLOCATIONS_PER_JOB - self.allocations_per_job)
            * self.mesh_jobs_completed as f32;

        AllocationImprovement {
            improvement_factor,
            allocations_saved: allocations_saved as usize,
            baseline_allocations_per_job: BASELINE_ALLOCATIONS_PER_JOB,
            current_allocations_per_job: self.allocations_per_job,
        }
    }
}

#[derive(Debug)]
pub(crate) struct AllocationImprovement {
    pub(crate) improvement_factor: f32,
    pub(crate) allocations_saved: usize,
    pub(crate) baseline_allocations_per_job: f32,
    pub(crate) current_allocations_per_job: f32,
}

/// Global instance for tracking allocations
pub(crate) static ALLOCATION_TRACKER: AllocationTracker = AllocationTracker::new();

/// Macro for tracking HashMap allocations in BEFORE code
#[macro_export]
macro_rules! track_allocation {
    () => {
        crate::perf::allocation_tracker::ALLOCATION_TRACKER.track_hashmap_allocation();
    };
}

/// Macro for tracking mesh job completion
#[macro_export]
macro_rules! track_mesh_job {
    () => {
        crate::perf::allocation_tracker::ALLOCATION_TRACKER.track_mesh_job();
    };
}
