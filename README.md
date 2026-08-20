# mc_klon

Rust/WGPU Minecraft-like voxel engine with a Classic/Pre-Classic feature order and a modern implementation strategy.

## Direction

The project does not treat early-era features as permission for throwaway code.
Gameplay and content are still introduced in a roughly historical order, but rendering, UI, worldgen, storage, and settings are built toward modern contracts so they can scale without a later rewrite.

## Current State

- Rendering with `wgpu`, layered world passes, fog, overlays, sky cycle, and debug views
- Deterministic terrain shading contract with explicit render/collision/raycast/occlusion shapes
- Startup loading screen that prepares the first playable area before entering gameplay
- Persistent settings in `save/settings.json` with gameplay, graphics, and control bindings
- Main menu, pause menu, and options screens using textured Minecraft-style widget buttons
- 3D cached hotbar/inventory block previews for cubes, slabs, and cross-plants
- Smaller plant selection boxes for flowers, mushrooms, saplings, and similar decorative cross-plants
- Chunk streaming through worker pipelines for chunkgen, meshing, lighting, and save/load
- Worldgen with presets, terrain, carvers, lakes/springs, foliage, and a dedicated ore feature pass
- Save/load with `save/world.meta`, `save/manifest.json`, `save/ticking_areas.bin`, `save/region/*.mca`
- Simulation with fixed tick rate (20 TPS), random ticks, fluids, and separate simulation distance

## Build / Run

- `cargo run`
- `cargo run -- --migrate-save-v1-to-v2`
- `cargo test`
- `cargo fmt`
- `cargo clippy`

## Important Controls

- `WASD`: move
- Mouse: look
- first mouse click in the window: enable mouse capture
- `LMB` / `RMB`: break / place block
- `Mouse Wheel`: hotbar selection
- `1`-`9`: select hotbar slot directly (fixed, not rebindable)
- `B`: inventory
- `T`: chat
- `Space`: jump / move upward (depending on mode)
- `Shift`: descend (while flying) / context-dependent movement
- `Ctrl`: hold sprint (in addition to double-tap `W`)
- Double `Space`: walking <-> flight
- `ESC`: pause / close menus
- `N`: generate a new world
- `Enter`: save world
- `K`: ticking-area editor (radius / flags)
- `F2`: screenshot -> `screenshots/<YYYY-MM-DD_HH-MM-SS>.png`
- `F5`: cycle camera perspective (first person / third person back / front)
- `F`: cycle render distance
- `G`: cycle simulation distance
- `F3`: debug overlay
- `F4`: depth-test debug toggle
- `F6`: far LOD on/off
- `F7`: ambient occlusion debug toggle
- `F8`: cycle lighting debug view
- `F9`: fluid debug line under the crosshair
- `F10`: perf summary toggle
- `F11`: precipitation toggle
- `Ctrl+T`: create a ticking area at the current chunk position
- `Ctrl+Shift+T`: remove the nearest ticking area

Rebindable actions (movement, jump, sneak, sprint, inventory, chat, ticking areas,
screenshot, camera perspective, pause) can be changed under Options -> Controls.

Note: hotbar selection through number keys `1..9` is still not wired.

## Runtime Defaults

- Player start mode: `Flight` (toggle via double-space)
- Double-tap windows: `Space=0.24s`, `W=0.30s`
- Default worldgen preset: `Rd161348Like`
- Random tick speed: `3` samples per active chunk and sim tick
- Settings file: `save/settings.json`

## Resource Packs

- `resourcepacks/` is created next to `save/`, `logs/` and `screenshots/` on first launch
- Drop a pack in as a folder or a `.zip`, then pick it under Options -> Video / Graphics
- Both layouts are detected: `textures/...` and `assets/minecraft/textures/...` (folder or archive root)
- A pack is an overlay: anything it does not ship falls back to the bundled `assets/minecraft`
- `.mcmeta` animation sidecars are read from the same pack as their texture
- Textures are uploaded into GPU arrays at startup, so switching packs takes effect after a restart

## Save System

- Metadata: `save/world.meta`
- Content manifest: `save/manifest.json`
- Ticking areas: `save/ticking_areas.bin`
- Chunk columns: `save/region/r.<rx>.<rz>.mca`
- Persists world seed, preset, height parameters, sea level, block data, and registry context
- Client settings are stored separately in `save/settings.json`
- Runtime lighting is rebuilt after load

Note: legacy `save/world.bin` still exists in code as an older path, but it is not the active runtime storage path.

## Relevant Documentation

- `docs/00_idee_und_ziel.md`: target vision and principles
- `docs/01_status_und_aktueller_stand.md`: current technical status and open work
- `docs/05_gameplay_feature_backlog_1_21_11_alignment.md`: prioritized backlog grouped by subsystem
- `docs/06_runtime_contracts_and_pipelines.md`: current shading, loading, settings, and ore-generation contracts
- `docs/todos_worldgen.md`: worldgen-specific next steps
- `docs/performance_optimizations.md`: implemented and open performance work
- `docs/save_region_format_v1.md`: storage format notes
- `docs/03_java_bedrock_takeaways.md`: architecture north star
- `docs/04_content_foundation.md`: content/data foundation
