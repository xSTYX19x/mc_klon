# mc_klon

Rust/WGPU-Minecraft-Klon mit Fokus auf fruehe Classic-/Pre-Classic-Rekonstruktion und sauberer, performanter Architektur.

## Aktueller Stand

- Rendering mit `wgpu` inkl. Layern (`Opaque`, `Cutout`, `Transparent`), Depth Buffer, Fog und Overlays
- Chunk-basiertes Voxel-Rendering mit Greedy Meshing, Plant- und Liquid-Meshing
- Fluids: Water/Lava-Simulation, Mixing (Stone/Cobblestone/Obsidian), Fluid-Mesh-Coalescing
- Bewegung: Walking/Flight/Swim/Wading, Sprint-Logik, Kollisionen, Step-Up
- Worldgen: Seed + Presets, Terrain-Noise, Surface Rules, Carvers (Caves/Ravines), Lakes/Springs
- Streaming ueber Worker-Pipelines (Chunkgen, Meshing, Lighting, Save/Load)
- Save/Load mit `save/world.meta`, `save/manifest.json`, `save/region/*.mca`
- Simulation mit fixer Tickrate (20 TPS) und separater Simulation Distance

## Build / Run

- `cargo run`
- `cargo test`
- `cargo fmt`
- `cargo clippy`

## Wichtige Controls

- `WASD`: Bewegen
- Maus: Look
- `LMB` / `RMB`: Block abbauen / platzieren
- `1`..`9` oder Mausrad: Block-Auswahl
- `Space`: Springen / aufsteigen (je nach Modus)
- `Shift`: Absteigen (im Flug) / kontextabhaengig Bewegung
- Doppelt `Space`: Walking <-> Flight
- `ESC`: Pause-Menue oeffnen/schliessen
- `N`: Neue Welt generieren (mit Generation-Overlay)
- `Enter`: Welt speichern
- `F`: Render Distance zyklisch (`Short`, `Normal`, `Far`, `Extreme`)
- `G`: Simulation Distance zyklisch (`Short`, `Normal`, `Far`, `Extreme`)
- `F3`: Debug-Overlay
- `F4`: Depth-Test Debug-Toggle
- `F5`: Frustum-Culling Debug-Toggle
- `F6`: Far-LOD an/aus
- `F7`: Ambient-Occlusion Debug-Toggle
- `F8`: Lighting-Debug-View zyklisch

## Save-System (aktuell)

- Metadaten: `save/world.meta` (JSON, atomarer Write)
- Content-Manifest: `save/manifest.json` (atomarer Write)
- Chunk-Spalten: `save/region/r.<rx>.<rz>.mca`
- Persistiert u.a.:
  - Blockdaten pro Sektion (`blocks`, `block_data`)
  - World Seed, Preset, Hoehenparameter, Sea-Level
  - Block-ID-Mapping-Version + Manifest-Signatur-Kontext
- Laufzeitdaten wie Lighting werden nach Load neu aufgebaut

Hinweis: Legacy-`save/world.bin` existiert noch als Altpfad im Code, ist aber nicht der aktive Runtime-Storage-Pfad.

## Relevante Doku

- `docs/01_status_und_aktueller_stand.md`: technischer Ist-Stand
- `docs/todos_worldgen.md`: offene Worldgen-Aufgaben
- `docs/performance_optimizations.md`: umgesetzte und offene Performance-Themen
- `docs/save_region_format_v1.md`: Storage-Format-Spezifikation
- `docs/03_java_bedrock_takeaways.md`: Architekturentscheidungen (North Star)
- `docs/04_content_foundation.md`: datengetriebene Content-Foundation
