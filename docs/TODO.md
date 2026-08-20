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

- extend the occlusion/AO/selection contract to non-cube shapes: stairs, panes, fences, torches, rails
- align world lighting/exposure checks with the meshing occlusion-shape rules: lighting still gates on the per-block `blocks_skylight` bool instead of `OcclusionShape`/`FaceCoverage`, and the two already disagree — `grass_block` occludes faces and AO but lets skylight through (`blocks/mod.rs:694`), `oak_leaves` stops skylight but contributes nothing to occlusion (`:1017`), `stone_slab` likewise (`:1057`); the unknown-block fallback splits too (`Decorative` occlusion vs. spec-derived skylight). Derive the predicate from `shape.boundary_coverage(Face::Down) == FaceCoverage::Full` with explicit overrides for glass/water, then drop the bool. Needs a visual pass — it changes terrain lighting everywhere. Guard with a test asserting agreement for every block plus a whitelist
- add debug visualization for face coverage, baked AO classes, and preview cache behavior
- stop greedy meshing from merging across AO discontinuities: AO is sampled only at the four corners of a merged quad, so occluders inside a long run are ignored and AO becomes a linear ramp across the span (`meshing/greedy.rs:1219-1240`). Including the packed AO values in `MaskCell` equality stops the merge at AO edges — more correct, but more quads, so measure
- confirm or rule out boundary-ocean plane aliasing: `push_ocean_quad` builds UVs from world coordinates over a ring reaching the world border (`state.rs:2783-2833`), and with `Nearest` filtering and no mipmaps an unbounded UV range on a screen-covering plane moirés badly. Never reproduced, only reasoned about — toggle the boundary ocean off while an artifact is on screen to settle it
- consider GPU-driven/indirect submission only if a draw-call bottleneck is measured and proven
- consider more advanced occlusion approaches only after measurement
- fix main-menu panorama tilt: rotation is composed as `Mat3::from_rotation_y(t * yaw_speed) * Mat3::from_rotation_x(pitch)` (`src/render/state.rs:18492-18493`) with a nonzero `MENU_PANORAMA_PITCH_RAD` (`state.rs:1240`) — the pitch term makes the sky horizon look tilted as it yaws; either zero the pitch const or apply it only around the eye's local right axis after yaw so the horizon stays level through the full rotation
- add the dark-chunk regression test that is still missing: generate a 3x3 chunk area and assert no chunk keeps an all-zero light buffer next to lit neighbours, and that a chunk reaching `VisualReady` actually gets its mesh rebuilt. The original cause was **not** the suspected cross-boundary BFS gap — it was provisional light seeded to sky=0 plus a lighting-only remesh parked until `Finalized` (a state edge chunks never reach while the player moves); both are fixed, see `07_performance_rendering_audit.md` §2.1/§2.3/§2.4. Without the test the parking condition can silently regress
- replace the paused-menu's static background with a blurred, frozen last frame: capture the last rendered frame into a texture on pause-enter, run a small two-pass (or single dual-filter) gaussian blur post-process (candidate: extend `src/render/depth.rs`/render graph with a blur pass, sampled once and cached, not per-frame) and draw the pause UI on top of that instead of whatever background `GameState::Paused` currently renders (`src/app/mod.rs:112`, label wiring `src/render/state.rs:1303`)

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

## Storage / Content Foundation

- keep V1/V2 read-compatibility explicit; expand save/load tests for signature drift, unknown blocks, remap cases
- if measurements justify it: design a more compact v2.1/v3 column storage layout (less string overhead than the current name-palette, forward-compatible)
- measure and, if necessary, reduce V2 storage overhead for block-name palettes
- migrate in-memory chunk storage from `BlockId`/`block_data` `Vec`s to `BlockStateId` + separate dirty flags, then optionally palette/bitpack at runtime too

## UI / UX

- keep expanding the options UI without reintroducing scattered hard-coded runtime settings
- [x] Texture-Packs als `.zip` *(umgesetzt, wartet auf Verifikation)*

## Gameplay

- wire hotbar digit keys `1..9` to slot selection and decide whether they become rebindable actions (`hotbar_slot_for_key` in `src/input.rs` exists but is never called)
- continue block-shape gameplay polish so interaction boxes stay close to visible geometry for future non-cube blocks
- decide how spawn persistence and first-view direction should affect startup readiness/target selection
- add a hand swing animation on break/place: the first-person arm/held-item model already exists and is tunable (`arm_model_rotation`, `arm_tweak.model_rot_*_rad`, held-item pose consts, `hand_anchor_local` around `src/render/state.rs:3467-3499`), but it's a static pose — add a short timed swing (punch arc on break, a lighter jab on place) driven by a timer set on the mine/place action, eased back to the resting `arm_tweak` pose, reusing the existing rotation composition instead of a new animation system

## Performance

- finish the streaming-lag hunt — the open half of the reported symptom. Measured after the fixes in `07_performance_rendering_audit.md`: streaming frames are median **10.6 ms / p90 53.1 ms**, and `state.process_dirty_chunk_meshes` alone is **mean 12.9 ms**, ~60 % of a 21 ms frame. Chunk apply is no longer the cost (104 ms → 0.51 ms per applied chunk). The p90 spikes are what feels laggy, and they never cross the 200 ms stall threshold, so no stall report ever contained them. Open: where the remaining ~9 ms/frame inside `process_dirty_chunk_meshes` goes. The `gpu.csv` breakdown columns answer this from one normal play session — read them before changing anything else here
- document reproducible startup and streaming benchmark scenes as a baseline
- measure startup warmup and first-playable-frame timing under fixed scenes
- measure ore-pass, preview-cache, and loading-screen warmup costs before deeper optimization work
- measure preview cache growth and decide whether eviction/prewarming is needed
- continue consolidating legacy worldgen/storage paths that still duplicate logic
- preset-specific worldgen tuning with snapshot tests
- refine additional upload/dispatch metrics in the debug overlay
- give the stall ring buffer a useful window: it is capped at `RING_MAX_EVENTS = 8192`, which under streaming load covers only ~900 ms instead of the intended 10 s, so whatever caused a stall has usually been evicted before the report is written. Either raise the cap or cut per-frame event volume
- the stall summary's coarse `tops=[…]` list aggregates by subsystem counter, so cost can be attributed to the wrong system (unload-triggered lighting showed up as `lighting=188.0ms`). An `unload` bucket was added; check the remaining buckets for the same aliasing
- complete ticking-area workflows (radius/flags/UX) beyond the shortcut baseline
- mesh-job priority inversion (`state.rs:5678`): the mesh job channel is `unbounded` and workers are FIFO, so dispatch order freezes priority at enqueue time and far chunks get meshed ahead of near ones (`inflight(mesh=148)`, upload waits up to 413 ms in older logs). The dirty queue is already reprioritized every frame, so the fix is to cap in-flight jobs and keep the rest in the reorderable queue. Needs measurement first: a wrong in-flight limit trades stutter for starvation
- chunk mesh GPU suballocation (`render/mesh_gpu.rs:196-260`): up to 6 `wgpu::Buffer` per chunk, `grown_capacity` rounds to `next_power_of_two` (up to 2x VRAM waste). Suballocate layers from a persistent arena, keep the existing reuse-on-fit path
- move incremental lighting onto the existing `LightingWorkerPool`: it is spawned but idle (`light_rx=0` in every captured log) while incremental lighting runs on the main thread in 1.5 ms slices. This is the structural half of the lighting backlog problem; the budgets were only raised. Touches world-state/snapshot consistency, so it is a concurrency change, not tuning
- step the chunk-removal lighting like the added path: `lighting_on_chunks_removed` is one-shot, so a single batch can still overshoot its budget (~8-19 ms per chunk). `begin_incremental_region_lighting_task` + `step_incremental_lighting_task` already provide the bounded variant
- stop a single item from blowing a stage budget: every stage loop checks `if iterations > 0 && elapsed >= budget`, which is right for guaranteeing progress but leaves no ceiling on one item. Add an aggregated watchdog that reports when an item exceeds the whole stage budget
- note re: "multithreading" — already present via `std::thread` + crossbeam-channel workers per `src/render/*_worker.rs`; no `rayon` usage found in those workers, worth deciding whether worker-pool-per-stage (current) or a shared rayon pool is the intended long-term model before adding more parallel stages

## Docs / Cleanup

- keep README, status docs, backlog docs, and worldgen TODOs updated after feature landings
- convert important contracts into short code comments near their source modules
- remove stale assumptions whenever a system stops being experimental
- make `cargo clippy --all-targets -- -D warnings` (the lint command in `CLAUDE.md`) usable again: the one hard clippy error is gone, but ~102 `never used` warnings remain, so `-D warnings` still fails. Needs a decision per item — bulk-deleting would likely remove scaffolding for in-flight work, bulk-`#[allow]` would hide future real findings

## Historical / Classic Parity

- humans/mob stub for early Classic feel
- fine-tuning of individual UI/gameplay parity details
