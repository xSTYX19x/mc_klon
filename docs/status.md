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
  - liquids and cross plants use world light (own cell / cell in front of the face); lava keeps its emitted block light
- debug views for depth/culling/AO/lighting
- 3D preview meshes for block models are reused for UI item previews

### Shapes / Interaction

- separate block concepts for render shape, collision shape, selection/raycast shape, and occlusion shape
- explicit occlusion coverage for full cubes, lower and upper slabs, cross-plants, and decorative non-occluders
- plant-like decorative blocks use smaller centered selection boxes instead of full-block targeting
- decorative cross-plants remain non-colliding and non-occluding

### Block Registry

- every block is declared once in the `blocks!` macro (`src/blocks/mod.rs`), which generates `BlockId`, the spec table, and `BLOCK_COUNT`; runtime ids follow declaration order and only grow by appending
- block specs are built from defaults (`BlockSpec::cube` / `cube_top_side_bottom` / `plant` plus `with_*` modifiers), so an ordinary new block is a single entry
- full cubes can carry a texture per face (`CubeFaces`); a cube declared with a south front (`BlockSpec::cube_front`) turns with a `facing` property (`with_front_facing`), and the mesher keeps differently turned faces out of one greedy run
- the creative palette is sorted from each block's `creative_order` at compile time
- `BlockRegistry::get()` is a static view over the table; nothing is rebuilt per saved or loaded column
- block states (`blocks/state.rs`): a `BlockState` is a block plus its data byte; each block declares its bit layout in `blocks!` (sapling `stage`, fluid `level`/`static`/`source`/`falling`) and code reads it through typed `Property` views; random-tick hooks, shape queries and `World::set_block_state` take states; F3 shows the targeted state as `name[prop=value]`; the save format is unchanged. `StateForm` marks the properties that change shape or model (`with_shape_half`, `with_front_facing`): the stone slab's `half=top` moves collision, selection, occlusion and mesh into the upper half of the cell, and a data-only change to such a block is remeshed
- placement hooks (`blocks/placement.rs`): the raycast reports its hit point, `PlaceContext` carries the clicked face, the hit point on the clicked block and the camera yaw, and a block's `on_place` picks its placed state or refuses; slabs pick their half from the click (bottom face or upper half of a side face → top), furnaces turn their front toward the player

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
- new worlds are unbounded: the classic size (Small/Normal/Huge) only sets the area generated before play starts; the boundary-ocean ring and its fog limit exist only in worlds saved with `boundary_ocean`
- terrain + density + surface rules
- sea: every cell below `sea_level` that the terrain leaves empty is static water (dirt floor, never grass); carvers never cut next to or under it, also across chunk edges; water lakes below sea level keep the sea
- Alpha beaches in `sea_level - 4 ..= sea_level + 1`: sand or the rarer gravel by two low-frequency noises (a gravel beach sits a block lower), clay patches in sand under water; in winter worlds water touching gravel stays liquid
- streaming generates the base terrain per column: the y-independent heightmap is computed once per column and all-air sections are skipped (`WorldgenPipeline::generate_column`)
- carvers (caves/ravines)
- lakes/springs and shoreline post-processing
- foliage pass (flowers + mushrooms)
- final grass settle pass: no generated grass sits under a skylight blocker or fluid, so random ticks do not rewrite fresh terrain after loading
- ore generation as a dedicated deterministic feature pass after carvers
- ore families: coal, iron, gold, redstone, diamond, plus dirt and gravel pockets underground (Alpha counts, heights mapped per preset)
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
- fluids, block entities, random ticks and block updates run through the sim-system path
- random ticks skip sections without random-ticking blocks (per-section counter, like vanilla `isRandomlyTicking`)
- world change journal: every `World::set_block*` change is journaled and drained into the block-edit pipeline (remesh, light, save-dirty), so random-tick changes like grown trees are rendered and saved
- block updates (`world/block_updates_system.rs`): each drained journal batch also goes to the sim systems (`SimSystem::on_block_changes`, never back to its producer); the block-update system notifies the changed cell and its six neighbors through `on_neighbor_changed` and runs `on_scheduled_tick` from a timing wheel (`TickScheduler<K>`, shared with the fluids), with a chain-depth limit of 512 and a per-tick budget; scheduled ticks and pending updates of columns that leave the sim area are dropped
- plants (`blocks/plants.rs`): sugar cane (cross model) and cactus (`InsetSides` model: full caps, sides inset 1/16, selected and collided as that box) carry `age` and grow by random tick to at most 3 tall; cane stays on cane or on grass/dirt/sand with water beside that block, a cactus on sand or cactus with air on all four sides; otherwise their `on_neighbor_changed` hook breaks them, and placement refuses such spots. Mushrooms are removed by a random tick at light 13 or more (stored sky/block light, no day-night darkening)
- worldgen foliage places sugar cane at water edges and cacti on dry sand above sea level (patch scatter like the flowers, not in winter worlds), each block only where its survival rule holds inside the column; mushrooms only where a backward light search proves the lit level stays at 12 or below, so loading never triggers the mushroom random tick
- winter: "Generate new level" opens a New Level screen whose environment defaults to Random (winter for 1 in 4 seeds, from the seed) with Temperate/Winter selectable; the result goes to `world.meta` as before. Snow layers carry `layers` 1-8 (height, collision, outline and mesh follow it), snow placed on snow adds a layer, and while it snows a random tick adds layers to snow open to the sky up to `SNOW_GROWTH_MAX_LAYERS` (4). Ice is slippery (Vanilla slipperiness 0.98 scales ground acceleration and braking) and melts to a water source above block light 11; runtime freezing needs block light below 10, and the winter worldgen pass leaves water within reach of lava liquid
- sand and gravel fall (`falls` in `BlockSpec`): two ticks after a change next to them they drop onto the first solid block below, at once (no falling entity yet); a column follows block by block

## Runtime Caveats That Are Still Real

- hotbar digit keys `1..9` are still not wired to slot selection
- world lighting still contains coarser full-block assumptions than the newer meshing occlusion contract
- startup focus is still centered around the current spawn/bootstrap target rather than a richer persisted player-spawn/facing contract
- the project still carries many pre-existing warnings in `cargo check` / `cargo test`

## Current Open Work By Subsystem

See `docs/TODO.md` for the current open-work list by subsystem.
