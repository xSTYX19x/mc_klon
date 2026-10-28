# Memory Pool Implementation (Meshing)

Status: 2026-03-04

## Goal

Reduce meshing allocations per job in order to stabilize frame times during heavy remeshing.

## Implemented Approach

- Thread-local `MeshingPool` (`src/meshing/pool.rs`)
- Reuse of:
  - vertex/index buffers
  - greedy mask scratch
  - `LiquidMeshingPool` (corner cache)
  - temporary arrays
- `with_meshing_pool(...)` resets contents only and keeps capacities

## Liquid Meshing

- `LiquidMeshingPool` in `src/meshing/liquids.rs`
- `append_liquids_in_chunk_pooled(...)` uses the pooled path
- fallback without an external pool also uses thread-local reuse

## Instrumentation

- `src/perf/allocation_tracker.rs` provides optional allocation tracking
- can be hooked into `liquids.rs` via the `allocation_tracking` feature gate

## Expected Effect

- fewer short-lived allocations in the meshing hot path
- less heap churn in fluid-/greedy-heavy scenes
- more stable frame times under streaming load

## Relevant Files

- `src/meshing/pool.rs`
- `src/meshing/greedy.rs`
- `src/meshing/liquids.rs`
- `src/perf/allocation_tracker.rs`
