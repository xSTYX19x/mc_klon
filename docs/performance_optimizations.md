# Performance Optimizations

Status: 2026-03-07

## Already Implemented

- Meshing memory pool (thread-local reuse)
- Frustum culling for render and mesh dispatch
- Worker pipelines for chunkgen, meshing, lighting, storage
- Stage budgets and burst mode in streaming
- Far-LOD streaming/upload with separate budgets
- Fixed simulation ticks (20 TPS) with catchup limit
- Save/load asynchronous with queue coalescing and autosave budgeting
- Fluid mesh coalescing against thrashing
- Runtime trace output to `logs/run_<ts>_<ms>.*`
- Startup loading flow now uses the same warmup/overlay path as `Generate new level`
- Hotbar/inventory 3D block previews use cached preview meshes instead of rebuilding slot content every frame

## High Priority

1. Document reproducible profiling scenes as a baseline
2. Continue consolidating the worldgen path (reduces duplicate work/branching)
3. Measure and, if necessary, reduce V2 storage overhead for block-name palettes
4. Measure startup warmup and first-playable-frame timing under fixed scenes

## Medium Priority

1. Preset-specific worldgen tuning with snapshot tests
2. Complete ticking-area workflows (radius/flags/UX) beyond the shortcut baseline
3. Refine additional upload/dispatch metrics in the overlay
4. Measure preview cache growth and decide whether eviction/prewarming is needed

## Low Priority

1. GPU-driven submission only if a draw-call bottleneck is proven
2. More advanced occlusion approaches only after measurement

## Measure Before Optimizing

At minimum capture:

- phase times (`chunkgen`, `meshing`, `lighting`, `upload`, `autosave`)
- queue backlogs and inflight counters
- number of changed chunks per simulation/fluid step
- time until "first playable frame"
- time until "warm spawn complete" (near render distance around spawn filled stably)
- before/after comparison under the same scene and settings

## Boot / Loading-Screen Notes

The boot path now already follows the loading-screen design:

1. load window + GPU + minimal UI resources
2. warm the world asynchronously
3. prepare the critical spawn-area chunk set
4. enter gameplay only after the startup readiness threshold is reached

Still worth measuring:

- first-playable-frame latency
- total warm-start time for the critical startup set
- whether separate startup-vs-runtime budgets would improve perceived readiness

## Practical Artifacts

- `logs/run_*.trace.json`: Chrome/Perfetto-compatible CPU trace
- `logs/run_*.gpu.csv`: GPU timing CSV (per frame/event)
- `logs/run_*.stall.txt`: stall report with ring buffer of recent events
- current stall-report threshold in code: `200 ms`
