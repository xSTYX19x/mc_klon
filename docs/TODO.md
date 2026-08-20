# TODO

Status: 2026-08-20

Single source of truth for open/not-yet-done work. Previously this list was
duplicated across `01_status_und_aktueller_stand.md`,
`02_expanded_roadmap_to_c0.0.13a_03.md`, `03_java_bedrock_takeaways.md`,
`04_content_foundation.md`, `05_gameplay_feature_backlog_1_21_11_alignment.md`,
`performance_optimizations.md`, and `todos_worldgen.md`. Those files now point
here for open work; keep their "Already Implemented" / status sections
separate from this file.

When an item ships, delete it from here instead of marking it done.

## Rendering

- extend the occlusion/AO/selection contract to non-cube shapes: stairs, panes, fences, torches, rails. **Not a rendering fix — a content feature with a prerequisite.** Every one of these needs per-block orientation (stairs 8 states, rails 10, fences 4 connection bits), and the engine has no block-state system: `BlockStateId` is a plain alias for `BlockId`. The 8 bits of per-voxel `block_data` exist but are owned by fluid levels today, so the first step is deciding whether geometry state shares them or gets its own storage. After that each shape needs a `BlockModelSpec` variant, per-block (non-greedy) meshing, an `OcclusionShape`, a `CollisionShape`, textures, placement logic and an inventory entry. Meanwhile the contract itself is guarded: `blocks::interaction_shape_tests::every_model_states_its_raycast_shape` matches exhaustively on `BlockModelSpec`, so a new non-cube model fails to compile until its shapes are stated
- **visual pass still owed** on the skylight change: `blocks_skylight` is gone, replaced by `SkylightSpec` deriving from `OcclusionShape::boundary_coverage(Face::Down)` with two named exceptions (leaves block without occluding, glass is explicitly transparent to light). This fixed two real leaks — `grass_block` and `stone_slab` were full-coverage shapes that passed skylight straight through — so anything under grass or a slab is now correctly dark where it used to be lit. Terrain lighting changed everywhere; walk a cave mouth, an overhang and a slab roof before trusting it. Guarded by `blocks::tests::skylight_agrees_with_the_occlusion_shape_except_for_named_blocks` and `world::tests::opaque_surface_blocks_no_longer_leak_skylight_downwards`
- decide whether AO-aware greedy merging is worth its cost: implemented behind `MeshingPool::ao_aware_merge` and **measured** (`cargo test --release meshing::greedy::greedy_ao_merge_report -- --nocapture`) at **+36 % quads and +34 % meshing time** on hilly terrain. Default is off, because meshing is already the largest slice of a streaming frame. This is a visual-quality call, not a technical one — flip the flag, look at long walls and cave mouths, and decide
- settle the boundary-ocean aliasing question with the F12 toggle: the analysis is now recorded at `push_ocean_quad` (one texel per block, minification only past ~2000 blocks, and the boundary fog fades the ring out over the same span — the one open case is a camera sitting almost exactly at sea level, where the plane is edge-on). Toggle the ring off while an artifact is on screen; if it stays, the ring is not the cause
- consider GPU-driven/indirect submission only if a draw-call bottleneck is measured and proven. The numbers are now on screen (F3: terrain + LOD draw calls, visible chunks, CPU render ms) and per frame in `gpu.csv` next to `cpu_render_ms`. The gate: if render time does not track draw-call count, indirect submission buys nothing. Everything measured so far points at CPU meshing instead — `process_dirty_chunk_meshes` is ~60 % of a streaming frame
- consider more advanced occlusion approaches only after measurement. Same gate and same instrumentation as the item above; frustum-culling counters are already tracked (`frustum_culling_stats`). Nothing to do until a capture shows submission or overdraw dominating a frame

## Worldgen

### High priority
- fix the failing sea-level test: `world::config::tests::sea_level_comes_from_world_config_for_lakes_and_lava_constraints` asserts no water feature sits above the configured sea level and fails with `water_y=84, sea_level=80` in column (-2,-4). It is the only red test in the suite and predates the current performance work (verified against `8f7634d`), so either the lake placement ignores `sea_level` or the invariant is stated too strictly
- split ore profiles by preset instead of one shared default table (`ore_profile_for_preset` currently ignores its preset argument)
- add neighbor-aware exposed-air checks so `HiddenOnly` ores are trustworthy at chunk borders
- expand worldgen regression tests with seam, density, and height-distribution sampling
- continue consolidating legacy/older worldgen entry points against the active worker pipeline
- document preset differences and worldgen phase order where contributors will find it quickly

### Medium priority
- add more ore families once replacement/height targets are defined
- introduce biome/material-aware replacement rules beyond plain stone
- add richer height models if balancing needs more than uniform/triangle distributions
- surface more worldgen metrics in the debug overlay
- prepare the worldgen config path for taller worlds without redoing ore anchors again

### Low priority
- decide and document long-term world size expectations
- add comparison screenshots/seed references per preset
- revisit historical parity content only after modern feature passes are stable
- improve water pond/lake generation, which currently reads as bare sand+water clumps rather than believable small ponds: the logic actually lives outside `worldgen/carvers/` in `src/render/chunkgen_worker.rs` (`LakeCandidate` at `chunkgen_worker.rs:192`, `make_water_lake_candidate` at `:342`, boundary sealing `apply_water_lake_boundary_seal_to_chunk` at `:639`, carve/fill `apply_lake_carve_and_fill_to_chunk` at `:707`, size bounds `WATER_LAKE_R_MIN/MAX` at `:29-35`); consider moving this into a proper `worldgen/carvers/ponds.rs` module alongside `ravine.rs`/`caves.rs`, and improving the shape (organic/noise-perturbed basin outline instead of a filled sphere/blob) plus a proper sand-shore rim (graded sand band around the water edge instead of sand and water occupying the same footprint)

### Test/acceptance ideas
- sample several fixed seeds, assert per-ore counts stay within defined tolerances
- keep deterministic tests for same-seed/same-column equality
- add a visual/manual checklist for chunk-border seams
- measure that startup/chunkgen stay within acceptable warmup budgets after ore tuning

## UI / UX

- keep expanding the options UI without reintroducing scattered hard-coded runtime settings

## Performance

- **Referenzmessung 2026-08-20: Chunk-Streaming ~50 FPS (vorher 10-20).** Details und Ursachenkette in `docs/07_performance_rendering_audit.md` → "Nachtrag 2026-08-20". Vor weiterem Streaming-Tuning einen frischen `stall.txt` + `gpu.csv` aufnehmen — alle Messwerte unten stammen von *vor* dieser Runde

### Decouple frame rate from chunk streaming

Frame time must stop tracking streaming activity. Today it does, and not by
accident — the current design permits it. Do **not** "fix" this by lowering the
existing constants: they were raised deliberately, and shrinking them trades
stutter for slow streaming. The fix is to stop letting the stages decide their
cost independently of the frame deadline.

What was verified in the code before writing this item:

- **The per-stage budgets are additive and none of them knows the frame
  deadline.** Every stage is a separate constant and every stage may spend its
  full budget in the same frame: chunkgen apply 3.0
  (`STREAM_CHUNKGEN_APPLY_BUDGET_MS`, `config/mod.rs:343`), apply follow-ups 1.5,
  saved-column load apply 2.0, streaming lighting 6.0, removal lighting 3.0,
  block-edit lighting 1.5, changed-chunk dirty apply 0.5, mesh dispatch 2.0,
  mesh upload 3.0 (5.5 low-speed, 10.0 warmup), prune 1.0, evict 1.5, block-edit
  side effects 1.5, fluid sim 3.0. That sums to **~30 ms before `render` even
  starts** — a 60 Hz frame is 16.6 ms. The engine is currently *specified* to
  drop to ~30 fps whenever every stage has work, which is the definition of
  "chunks are generating".
- **Burst mode widens the budget exactly when the frame is already heavy.**
  `update_stream_burst_mode_for_camera` (`state.rs:11228`) turns burst on when
  meshes near the camera are missing, and `stream_budget_for_stage_ms`
  (`state.rs:11245`) then hands out up to `STREAM_BURST_MAX_TOTAL_BUDGET_MS =
  25.0` across three stages alone. That shared cap is the right shape — it is
  just capped too high, applies only in burst, and covers only 3 of ~12 stages.
- **There is no frame-time feedback anywhere.** No budget in `render/state.rs`
  or `app/mod.rs` reads a measured frame time; all of them are compile-time
  constants. Nothing widens the budget on a cheap frame or narrows it after an
  overrun.
- **Three worker pools each spawn `available_parallelism()` threads**
  (`mesh_worker.rs:45`, `chunkgen_worker.rs:3162`, `lighting_worker.rs:28`), all
  at default OS priority. On an 8-thread CPU that is up to 24 runnable workers
  competing with the one thread that has to simulate *and* render. A main thread
  that respects its budget perfectly still loses wall-clock time here, so budget
  work alone cannot close this.
- **The world can only be mutated on the frame thread.** `World.chunks` is a
  plain `HashMap<ChunkCoord, Chunk>` (`world/mod.rs:51`) owned by `State`
  (`state.rs:5532`), so applying a generated chunk needs `&mut World` inline.
  This is the structural reason apply, lighting hooks and fluid bootstrap cannot
  move off the frame thread today.
- **GPU resources are created inside the frame.** `upload_cpu_mesh_to_gpu_reuse`
  (`mesh_gpu.rs:203`) calls `device.create_buffer` + `queue.write_buffer` on the
  frame thread; buffer creation is a driver allocation whose cost lands *after*
  the budget check that admitted it.

Work items, in the order they should be attempted:

1. **Bound per-item overshoot first** (already listed below as "stop a single
   item from blowing a stage budget"). *Stand 2026-08-20: der Watchdog meldet
   solche Items jetzt aggregiert pro Frame (`src/perf/stage_budget.rs`), und der
   gemessene Worst Case (Removal-Lighting, 8-19 ms pro Chunk) ist gestuft und
   damit weg. Das Muster selbst bleibt: jede Stage kann weiterhin um einen
   begrenzten Step überziehen.* A frame-level budget is only as tight as
   its worst single item, and the measured worst items are far above any frame
   budget: removal lighting was 8-19 ms *per chunk* while chunk apply is
   0.51 ms. Until one item cannot exceed its stage, a frame controller cannot
   hold a deadline.
2. **Replace the independent constants with one frame-level admission
   controller.** Derive the frame's total streaming allowance from the actual
   deadline (present mode is `Fifo` by default, see `preferred_present_mode`,
   `state.rs:10689`), subtract the measured cost of the parts that are not
   optional (sim, render, present), and hand each stage a *share of what
   remains* instead of its own constant. Generalise `stream_budget_for_stage_ms`
   into that single allowance and route every stage through it. Give each stage
   a guaranteed floor so streaming cannot starve — the existing
   `if iterations > 0 && elapsed >= budget` pattern already guarantees one item
   of progress per stage, so the floor is free.

   **Teilweise umgesetzt (2026-08-20), bisher nur für Lighting:**
   `LightingFrameBudget` (`src/render/lighting_budget.rs`) ersetzt die drei
   privaten Lighting-Budgets (1.5 + 6.0 + 3.0 = 10.5 ms) durch **ein** geteiltes
   7-ms-Budget, vergeben in Prioritätsreihenfolge Block-Edit → Chunk-Add →
   Chunk-Remove. Jede Stage behält ihren alten Wert als Cap, hält eine Reserve
   für die Stages hinter sich zurück und hat einen Starvation-Floor von 0.25 ms.
   Das ist die Form, die dieser Punkt für *alle* Stages verlangt — die Vorlage
   steht, die Allowance ist aber weiterhin eine Konstante statt aus dem Frame-
   Deadline abgeleitet (das bleibt Punkt 3). Offen für die nächste Messung: ob
   7.0 ms der richtige Wert ist und ob der Removal-Backlog unter Dauer-Unload
   mit dem Floor auskommt.
3. **Close the feedback loop.** Feed the previous frames' measured
   `frame_dt_ms` (already written per frame to `gpu.csv`) into the allowance as
   an EMA: shrink after overruns, grow while frames sit under the deadline.
   This is what makes the result hold on hardware that was never measured, and
   it is why the constants must go rather than be retuned.
4. **Measure and fix CPU oversubscription.** Size the three pools against one
   shared thread total (or a single shared pool) instead of each taking
   `available_parallelism()`, and/or drop worker priority below the frame
   thread. Gate on measurement: run a fixed scene, compare per-stage main-thread
   scope times with pools saturated vs. capped. If the main thread's own scope
   times inflate while its item counts stay constant, the loss is OS scheduling
   and no budget change will recover it.
5. **Move GPU buffer creation out of the frame.** Do the arena suballocation
   item below first (it removes most allocations outright); afterwards decide
   whether the remaining create+write should move to an upload thread — wgpu 28
   `Device`/`Queue` are cheap to clone and `Send`, so a worker can hand finished
   `ChunkMesh` values to the frame thread.
6. **Only then consider making world mutation concurrent** (per-chunk locks, or
   a double-buffered insert queue swapped once per frame). This is the large,
   risky change; steps 1-5 must fail to close the gap before it is justified.

Related items already listed below that are part of this problem, not separate
from it: the mesh-job priority inversion, the mesh GPU suballocation, moving
incremental lighting onto the idle `LightingWorkerPool`, stepping chunk-removal
lighting, and the per-item stage watchdog.

Acceptance: a fixed scene flown at constant speed through ungenerated terrain,
with `gpu.csv` captured for the whole run. The metric is **p99 `frame_dt_ms`
while streaming vs. p99 while standing still in already-loaded terrain** — an
average hides exactly the spikes that are the complaint. Record applied
chunks/second in the same run, otherwise a "fix" that simply streams slower
looks like a win. The startup burst path stays exempt: it runs behind the
loading screen, where throughput legitimately outranks frame time.

### Other performance work

- finish the streaming-lag hunt — the open half of the reported symptom. Measured after the fixes in `07_performance_rendering_audit.md`: streaming frames are median **10.6 ms / p90 53.1 ms**, and `state.process_dirty_chunk_meshes` alone is **mean 12.9 ms**, ~60 % of a 21 ms frame. Chunk apply is no longer the cost (104 ms → 0.51 ms per applied chunk). The p90 spikes are what feels laggy, and they never cross the 200 ms stall threshold, so no stall report ever contained them. Open: where the remaining ~9 ms/frame inside `process_dirty_chunk_meshes` goes. The `gpu.csv` breakdown columns answer this from one normal play session — read them before changing anything else here
- document reproducible startup and streaming benchmark scenes as a baseline
- measure startup warmup and first-playable-frame timing under fixed scenes
- measure ore-pass, preview-cache, and loading-screen warmup costs before deeper optimization work
- measure preview cache growth and decide whether eviction/prewarming is needed
- continue consolidating legacy worldgen/storage paths that still duplicate logic
- preset-specific worldgen tuning with snapshot tests
- refine additional upload/dispatch metrics in the debug overlay
- ~~give the stall ring buffer a useful window~~ **done**: the flat `RING_MAX_EVENTS = 8192` cap is gone. `perf/trace.rs` now evicts by retention time with a 16 MB byte budget as the backstop (`MC_KLON_TRACE_RING_MB` overrides), which holds >10 s at the measured ~9.1 k events/s. Thread labels are `Arc<str>` (one allocation per thread instead of a `String` clone per event) and `push_ring` no longer re-reads the clock. The stall report header prints the **actual** covered span, event count and `dropped_by_budget`, so a truncated window is no longer indistinguishable from a complete one
- the stall summary's coarse `tops=[…]` list aggregates by subsystem counter, so cost can be attributed to the wrong system (unload-triggered lighting showed up as `lighting=188.0ms`). An `unload` bucket was added; check the remaining buckets for the same aliasing
- complete ticking-area workflows (radius/flags/UX) beyond the shortcut baseline
- mesh-job priority inversion (`state.rs:5678`): the mesh job channel is `unbounded` and workers are FIFO, so dispatch order freezes priority at enqueue time and far chunks get meshed ahead of near ones (`inflight(mesh=148)`, upload waits up to 413 ms in older logs). The dirty queue is already reprioritized every frame, so the fix is to cap in-flight jobs and keep the rest in the reorderable queue. Needs measurement first: a wrong in-flight limit trades stutter for starvation
- chunk mesh GPU suballocation (`render/mesh_gpu.rs:196-260`): up to 6 `wgpu::Buffer` per chunk, `grown_capacity` rounds to `next_power_of_two` (up to 2x VRAM waste). Suballocate layers from a persistent arena, keep the existing reuse-on-fit path
- move incremental lighting onto the existing `LightingWorkerPool`: it is spawned but idle (`light_rx=0` in every captured log) while incremental lighting runs on the main thread in 1.5 ms slices. This is the structural half of the lighting backlog problem; the budgets were only raised. Touches world-state/snapshot consistency, so it is a concurrency change, not tuning
- ~~step the chunk-removal lighting like the added path~~ **done**: `pump_removed_lighting_batches` no longer calls the one-shot `lighting_on_chunks_removed`. A batch becomes a `RemovedLightingJob` holding an `IncrementalLightingTask` (`World::begin_incremental_chunk_removed_lighting_task`, sharing its neighbor-region walk with `LightEngine::on_chunks_removed` so both relight the same region) and is stepped under the same voxel/wall-clock limits as the added path, resuming across frames. Backlog counters and the teardown path count the in-flight job via `removed_lighting_backlog_len()`
- ~~stop a single item from blowing a stage budget~~ **done** (reporting half): `perf/stage_budget.rs` aggregates per frame which single items cost a stage its whole budget (count, worst item + label, summed overrun), reported worst-first into the trace ring every frame, into the console at most every 5 s, and appended to the stall report as `stage_budget_overruns[…]`. Wired into the three lighting step loops and the chunk-mesh upload loop — i.e. only where the per-item duration was already measured, so no new timing calls on hot paths. Splitting an offending item stays a per-stage fix; the watchdog only makes it visible
- note re: "multithreading" — already present via `std::thread` + crossbeam-channel workers per `src/render/*_worker.rs`; no `rayon` usage found in those workers, worth deciding whether worker-pool-per-stage (current) or a shared rayon pool is the intended long-term model before adding more parallel stages

## Docs / Cleanup

- keep README, status docs, backlog docs, and worldgen TODOs updated after feature landings. Storage / Content Foundation and Gameplay are closed; their two rejected designs (a compact v2.1/v3 column layout, and merging blocks + block_data into a `BlockStateId`) are recorded with their measurements next to the code, in `storage/chunk_codec.rs` and `world/chunk.rs`, rather than lingering here as open work
- convert important contracts into short code comments near their source modules
- remove stale assumptions whenever a system stops being experimental
- make `cargo clippy --all-targets -- -D warnings` (the lint command in `CLAUDE.md`) usable again: the one hard clippy error is gone, but ~102 `never used` warnings remain, so `-D warnings` still fails. Needs a decision per item — bulk-deleting would likely remove scaffolding for in-flight work, bulk-`#[allow]` would hide future real findings

## Historical / Classic Parity

- humans/mob stub for early Classic feel
- fine-tuning of individual UI/gameplay parity details
