# Aktueller Stand (Code-Realitaet)

Stand: 2026-03-01

## Toolchain / Dependencies

- Rust Edition 2024
- `winit = 0.30`
- `wgpu = 28`
- `pollster = 0.4`
- `glam = 0.32`
- `bytemuck = 1.16`
- `image = 0.25`
- `anyhow = 1`
- `fastnoise-lite = 1`
- `serde` / `serde_json`
- `rayon`
- `crossbeam-channel`
- `flate2`

## Projektstruktur (wichtige Module)

- `src/app/mod.rs`: App-Loop, Input, Game-State (`InGame`, `Paused`, `GeneratingWorld`)
- `src/render/state.rs`: Render-/Streaming-/Storage-Orchestrierung, Overlay, Budgets
- `src/world/mod.rs`: World-Daten, Chunks, Lighting-Basis, Registry/Manifest-Mapping
- `src/world/*_system.rs`: Sim-Systeme (`fluids`, `block_entities`, `random_ticks`)
- `src/meshing/*`: Greedy/Plants/Liquids + Memory-Pooling
- `src/worldgen/*`: Terrain, Density, Surface Rules, Carvers
- `src/storage/*`: `world.meta`, Region-I/O, Codec, Save/Load-Worker

## Implementierte Systeme

### Rendering / GPU

- Layered Rendering (`Opaque`, `Cutout`, `Transparent`) mit Depth Buffer
- Frustum-Culling fuer Rendering und Meshing-Dispatch
- Far-LOD-Pfad (aktivierbar), separate LOD-Streaming-/Upload-Budgets
- Fog-Varianten (Default/Water/Lava), Wasser-Overlay, Textur-Animationen (`.mcmeta`)
- UI/Overlays: Crosshair, Block-Preview, Pause-Menue, Generation-Screen, Debug-Overlay
- Debug-Views fuer Depth/Culling/AO/Lighting

### Gameplay / Input / UI

- FPS-Kamera, Cursor-Capture, Hotbar (`1..9` + Mausrad)
- Block abbauen/platzieren mit Repeat-Cooldown
- Pause-Menue (`ESC`) mit Save/Generate/Exit
- Neue Welt generieren (`N`) mit Progress-Overlay
- Render Distance (`F`) und Simulation Distance (`G`) separat schaltbar

### Bewegung / Physik

- Walking + Flight (Double-Space Toggle)
- Sprint inkl. Double-Tap-Forward-Latch
- Kollision + Step-Up
- Swim/Wading mit fluid-abh. Verhalten

### Simulation

- Fester Sim-Tick (20 TPS) mit Catchup-Limit pro Frame
- Sim-System-Framework (`SimSystem`) mit aktiven Sim-Spalten
- Systeme:
  - FluidsSystem
  - BlockEntitiesSystem
  - RandomTicksSystem
- Aktive Sim-Spalten aus Simulation Distance plus aktivierten Ticking Areas

### Blocks / Meshing

- Erweiterte Blockpalette inkl. Bedrock, Wasser/Lava (still/flowing), Sand, Obsidian
- Blockmodelle: Cube, CrossPlant, Liquid
- Greedy Meshing fuer Solid-Layer
- Spezifischer Liquid-Mesh-Pfad
- Thread-local MeshingPool inkl. Liquid-Caches

### Fluids

- Wasser- und Lava-Scheduler mit separaten Tickraten
- Rebuild aus Weltbloecken beim Load/Reset
- Mixing-Regeln (Obsidian/Cobblestone/Stone)
- Umfangreiche Unit-Tests in `src/world/fluids.rs`

### World / Chunks / Lighting

- Chunk-Groesse: `16x16x16`
- Vertikaler Bereich aktuell `0..255` (`min_y=0`, `max_y=255`)
- Skylight/Blocklight-Rebuild und asynchrone Lighting-Worker
- Remesh/Relight-Trigger bei Block- und Fluid-Aenderungen

### Worldgen

- Presets: `Flatish`, `Rd161348Like`, `Hilly`
- Seeded Terrain ueber Terrain/Density/Pipeline-Bausteine
- Surface Rules, Caves, Ravines
- Water/Lava Lakes + Water Springs im Chunkgen-Worker
- Sea-Level in `WorldConfig`/`world.meta` (Default 64)

### Save / Load

- Aktives Save-Layout:
  - `save/world.meta`
  - `save/manifest.json`
  - `save/region/*.mca`
- Atomare Writes fuer Meta + Manifest
- Region-Storage mit Zlib-komprimierten Spaltenpayloads
- Async Save-Writer + Async Load-Worker
- Autosave-Sweep mit Budget pro Frame
- Legacy `save/world.bin` weiterhin im Code, aber nicht als Primaerpfad

## Performance / Architektur-Status

- Worker-Pools: Chunkgen, Meshing, Lighting, Storage
- Budgetierte Stage-Loops im Streaming (Dispatch, Upload, Chunkgen-Apply, Saved-Load)
- Backpressure-Mechanismen und Coalescing fuer Save/Fluid/Mesh
- Perf-Trace/CSV/Overlay-Instrumentierung vorhanden

## Offene Punkte (bereinigt)

### Hoch

1. Boundary-Ocean-Visualisierung ausserhalb der harten Weltgrenze
2. Legacy-Worldgen-Pfade in `world/mod.rs` gegen aktive Worker-Pipeline weiter konsolidieren
3. Save-/Manifest-Doku auf Migrationsfaelle (groessere Block-Registries) erweitern

### Mittel

1. Biome-/Material-Layering ueber aktuelle Surface Rules hinaus
2. Worldgen-Tuning pro Preset mit reproduzierbaren Snapshot-Tests
3. Ticking-Areas auch ueber persistente/benutzerseitige Konfiguration steuerbar machen

### Niedrig

1. Historische Paritaetsfeatures (z.B. Humans/Mob-Stub)
2. Weitere Profiling-Repro-Szenen und Benchmark-Protokolle
