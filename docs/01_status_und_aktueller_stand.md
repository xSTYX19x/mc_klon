# Current State (Code Reality)

Status: 2026-03-07

## Project Direction

The repo is no longer in the phase where the main question is "whether" to build modern contracts.
That direction is now active in the code:

- early-era features are still introduced in a Classic/Pre-Classic order
- rendering, startup flow, settings, UI skinning, and worldgen are implemented against reusable systems
- the current priority is extending those systems cleanly instead of replacing them later

## Important Modules

- `src/app/mod.rs`: app loop, state transitions, menus, startup flow, input routing
- `src/render/state.rs`: renderer, streaming orchestration, overlays, UI rendering, preview draws
- `src/render/chunkgen_worker.rs`: column worldgen feature passes on top of terrain generation
- `src/render/shaders.rs`: world and UI shaders
- `src/settings.rs`: persistent client settings and control bindings
- `src/camera.rs`: gameplay camera plus final render-layer view bobbing
- `src/blocks/mod.rs`: block metadata, shapes, occlusion, selection/collision contracts
- `src/world/raycast.rs`: selection and hit testing
- `src/worldgen/ores.rs`: data-driven ore feature pass
- `src/storage/*`: world metadata, regions, async save/load

## Implemented Systems

### Rendering / Meshing

- layered rendering (`Opaque`, `Cutout`, `Transparent`) with depth buffer
- frustum culling for rendering and mesh dispatch
- far-LOD path with separate streaming/upload budgets
- deterministic terrain shading path:
  - face visibility independent from AO
  - AO/light sampling based on explicit occlusion shapes
  - partial blocks no longer treated as full blockers by default
  - shader receives stable baked vertex shading/light inputs
- debug views for depth/culling/AO/lighting
- 3D preview meshes for block models are reused for UI item previews

### Shapes / Interaction

- separate block concepts for render shape, collision shape, selection/raycast shape, and occlusion shape
- explicit occlusion coverage for full cubes, lower slabs, cross-plants, and decorative non-occluders
- plant-like decorative blocks use smaller centered selection boxes instead of full-block targeting
- decorative cross-plants remain non-colliding and non-occluding

### Startup / Streaming

- startup enters a loading state before gameplay
- first world boot and "Generate new level" share the same startup loading path
- startup prioritizes spawn-area chunk preparation near-to-far
- readiness gating waits for critical startup chunks to be generated/applied/meshed/uploaded before entering gameplay
- world rendering is suppressed during loading instead of drawing an unfinished world behind the overlay

### UI / UX / Settings

- main menu, pause menu, and options menu are active
- menu buttons use textured GUI widget assets with hover/pressed/disabled-capable rendering
- persistent settings file at `save/settings.json`
- settings are separated into:
  - gameplay
  - graphics
  - controls
- current settings surfaced in UI:
  - view bobbing on/off
  - render distance
  - VSync on/off
  - gameplay control rebinding for core actions
- runtime application is live for bobbing, render distance, and VSync
- control rebinding swaps conflicting assignments instead of silently duplicating keys

### Camera / Movement

- walking, flight, swim/wading, sprint logic, collisions, and step-up
- classic first-person walking bobbing implemented as a final camera transform layer
- bobbing is grounded-movement driven, speed-scaled, smooth, and settings-controlled
- raw look direction remains separate from bobbing offset/roll

### Inventory / Hotbar Presentation

- inventory and hotbar blocks render as 3D isometric previews
- cross-plants use crossed geometry in previews instead of flat sprites
- preview meshes are cached by block type instead of rebuilt every frame

### Worldgen

- presets: `Flatish`, `ClassicFlatter`, `Rd161348Like`, `Hilly`
- terrain + density + surface rules
- carvers (caves/ravines)
- lakes/springs and shoreline post-processing
- foliage pass (flowers + mushrooms)
- ore generation as a dedicated deterministic feature pass after carvers
- current ore profile is data-driven per ore type:
  - replacement rule
  - vein size
  - attempts/frequency
  - vertical distribution
  - exposed-air policy
- ore generation is seeded per world/preset/source chunk and samples neighboring source chunks for seam-stable cross-border veins

### Save / Load / Simulation

- active save layout:
  - `save/world.meta`
  - `save/manifest.json`
  - `save/ticking_areas.bin`
  - `save/region/*.mca`
- async save writer + async load worker
- fixed simulation tick (20 TPS)
- simulation distance separated from render distance
- fluids, block entities, and random ticks run through the sim-system path

## Runtime Caveats That Are Still Real

- hotbar digit keys `1..9` are still not wired to slot selection
- world lighting still contains coarser full-block assumptions than the newer meshing occlusion contract
- startup focus is still centered around the current spawn/bootstrap target rather than a richer persisted player-spawn/facing contract
- the project still carries many pre-existing warnings in `cargo check` / `cargo test`

## Current Open Work By Subsystem

### Rendering

1. extend the occlusion/selection contract to more non-cube shapes such as stairs, panes, fences, torches, and rails
2. make lighting and exposure checks use the same shape-aware rules as meshing
3. add better debug visualization for face coverage, baked AO classes, and preview cache behavior

### Worldgen

1. split ore tables by preset/profile instead of one shared default table
2. add richer ore rules: hidden-only exposure, more ore families, and biome/material-aware replacement
3. strengthen deterministic regression coverage with sampled-world statistics and seam checks

### UI / UX

1. keep expanding the options UI without reintroducing scattered hard-coded runtime settings
2. move remaining block-selection/icon paths onto the same 3D preview system
3. add a fuller ticking-area/editor UX instead of shortcut-only management

### Gameplay

1. wire hotbar digit keys and decide whether they belong in the rebindable controls set
2. continue block-shape gameplay polish for future interactables
3. decide how spawn persistence and first-view direction should affect startup readiness

### Performance

1. document reproducible startup and streaming benchmark scenes
2. measure ore-pass, preview-cache, and loading-screen warmup costs before deeper optimization work
3. continue consolidating legacy worldgen/storage paths that still duplicate logic

### Docs / Cleanup

1. keep README, status docs, backlog docs, and worldgen TODOs updated after feature landings
2. reduce stale notes that still describe pre-settings or pre-loading-screen behavior
3. tighten file/module comments around the newer contracts so they are discoverable from code as well as docs
