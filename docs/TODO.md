# TODO

Status: 2026-08-19

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
- align world lighting/exposure checks with the meshing occlusion-shape rules (lighting still gates on a single per-block `blocks_skylight` bool, not `OcclusionShape`/`FaceCoverage`)
- add debug visualization for face coverage, baked AO classes, and preview cache behavior
- consider GPU-driven/indirect submission only if a draw-call bottleneck is measured and proven
- consider more advanced occlusion approaches only after measurement
- fix main-menu panorama tilt: rotation is composed as `Mat3::from_rotation_y(t * yaw_speed) * Mat3::from_rotation_x(pitch)` (`src/render/state.rs:18492-18493`) with a nonzero `MENU_PANORAMA_PITCH_RAD` (`state.rs:1240`) — the pitch term makes the sky horizon look tilted as it yaws; either zero the pitch const or apply it only around the eye's local right axis after yaw so the horizon stays level through the full rotation
- fix chunk-lighting dropouts ("dark chunks"): likely a cross-boundary light-propagation gap between `src/render/lighting_worker.rs` and the per-chunk meshing pass — check whether skylight/blocklight BFS seeds from neighbor chunk edges correctly when a chunk finishes generating after its neighbor already meshed (stale neighbor snapshot vs. re-mesh-on-neighbor-light-change); add a regression test that generates a 3x3 chunk area and asserts no chunk has an all-zero light buffer adjacent to lit neighbors
- replace the paused-menu's static background with a blurred, frozen last frame: capture the last rendered frame into a texture on pause-enter, run a small two-pass (or single dual-filter) gaussian blur post-process (candidate: extend `src/render/depth.rs`/render graph with a blur pass, sampled once and cached, not per-frame) and draw the pause UI on top of that instead of whatever background `GameState::Paused` currently renders (`src/app/mod.rs:112`, label wiring `src/render/state.rs:1303`)

## Worldgen

### High priority
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
- [x] GUI scale option in den Graphics-Settings *(umgesetzt, wartet auf Verifikation)*
- add a base FOV setting decoupled from the dynamic sprint/flight FOV kick: `fov_y_radians` (`src/camera.rs:232`, default 70° at `camera.rs:244`) has no user-facing setting; the existing dynamic effect (`FOV_EFFECT_SCALE`, `FOV_EFFECT_SCALE_FLIGHT`, `FOV_RESPONSE_RATE`, `FOV_MIN/MAX_DEG` at `app/mod.rs:38-43`, eased in `app/mod.rs:2689-2726`) multiplies off of `base_fov_y_radians` — add a `Settings` field for the user's base FOV and feed it into `base_fov_y_radians` as the resting value that sprint/flight easing multiplies from, so the setting stays the anchor and the dynamic kick keeps working unchanged
- add a screenshot action (default `F2`, rebindable): nothing named "screenshot" exists in the codebase yet — add a keybind entry alongside the other rebindable actions in `src/settings.rs`/`src/input.rs`, and on trigger copy the current swapchain/backbuffer texture to a `Vec<u8>` (wgpu `copy_texture_to_buffer` + async map) and encode to PNG (already have `image`/PNG decode via `src/texture.rs`; add an encode path or the `image` crate's encoder) into a `screenshots/` folder next to saves
- add a third-person camera toggle (default `F5`, rebindable): no `third_person`/camera-mode concept exists yet — only the first-person camera in `src/camera.rs`; add a `CameraMode` enum (FirstPerson/ThirdPersonBack/ThirdPersonFront), a rebindable action wired the same way as the screenshot key, and an orbit offset behind the player's eye position with a collision raycast to pull the camera in when it would clip through terrain
- support multiple selectable texture packs: `TextureRegistry` (`src/texture_registry.rs:46`) currently loads PNGs from one fixed `assets_root: PathBuf` folder — generalize to a list of available packs (scan a `resourcepacks/` directory), add a `Settings` field for the active pack, and support hot-reload/restart-to-apply when switching in the Graphics options screen
- support texture packs as `.zip` archives, not just folders: pack loading in `texture_registry.rs`/`src/texture.rs` (`resolve_path`, `load_png_rgba`) assumes a flat filesystem folder — add a `zip` crate dependency and an abstraction (e.g. a small `PackSource` trait with folder and zip implementations) so `resolve_path`/PNG loads read through either a real directory or an in-memory zip archive index, needed before per-pack switching (above) is worth exposing to users

## Gameplay

- wire hotbar digit keys `1..9` to slot selection and decide whether they become rebindable actions (`hotbar_slot_for_key` in `src/input.rs` exists but is never called)
- continue block-shape gameplay polish so interaction boxes stay close to visible geometry for future non-cube blocks
- decide how spawn persistence and first-view direction should affect startup readiness/target selection
- add a hand swing animation on break/place: the first-person arm/held-item model already exists and is tunable (`arm_model_rotation`, `arm_tweak.model_rot_*_rad`, held-item pose consts, `hand_anchor_local` around `src/render/state.rs:3467-3499`), but it's a static pose — add a short timed swing (punch arc on break, a lighter jab on place) driven by a timer set on the mine/place action, eased back to the resting `arm_tweak` pose, reusing the existing rotation composition instead of a new animation system

## Performance

- document reproducible startup and streaming benchmark scenes as a baseline
- measure startup warmup and first-playable-frame timing under fixed scenes
- measure ore-pass, preview-cache, and loading-screen warmup costs before deeper optimization work
- measure preview cache growth and decide whether eviction/prewarming is needed
- continue consolidating legacy worldgen/storage paths that still duplicate logic
- preset-specific worldgen tuning with snapshot tests
- refine additional upload/dispatch metrics in the debug overlay
- complete ticking-area workflows (radius/flags/UX) beyond the shortcut baseline
- reduce stalling/lag when new chunks generate: generation is already off the main thread (`src/render/chunkgen_worker.rs`, `src/render/mesh_worker.rs`, `src/render/lighting_worker.rs` all spawn plain `std::thread` workers and hand results back via crossbeam channels — this is not a "put it on a thread" problem), so the lag most likely comes from how much work is dispatched per tick and how results are drained/uploaded on the main thread; profile chunks-generated-per-frame vs. GPU upload batching (mesh buffer creation, `render/mesh_gpu.rs`) under movement, and tune worker pool size / per-tick chunk budget / upload batching before assuming more parallelism is needed
- note re: "multithreading" — already present via `std::thread` + crossbeam-channel workers per `src/render/*_worker.rs`; no `rayon` usage found in those workers, worth deciding whether worker-pool-per-stage (current) or a shared rayon pool is the intended long-term model before adding more parallel stages

## Docs / Cleanup

- keep README, status docs, backlog docs, and worldgen TODOs updated after feature landings
- convert important contracts into short code comments near their source modules
- remove stale assumptions whenever a system stops being experimental

## Historical / Classic Parity

- humans/mob stub for early Classic feel
- fine-tuning of individual UI/gameplay parity details
