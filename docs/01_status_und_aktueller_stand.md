# docs/01_status_und_aktueller_stand.md

## Aktueller Stand (Code-Realität)

### Toolchain / Setup
- Rust (Edition 2024)
- `winit = "0.30"`
- `wgpu = "28"`
- `pollster = "0.4"`
- `glam = "0.32"`
- `bytemuck` mit `derive`
- `image = "0.25"`
- `anyhow = "1"`
- `fastnoise-lite = "1"`

### Projektstruktur
- `src/main.rs`: App-Loop, Input, Kamera, Movement, World, Meshing, Rendering, Save/Load
- `src/texture.rs`: PNG Loader + `TextureArray` (`texture_2d_array`)
- `src/texture_registry.rs`: Mojang-Key-basierter Asset-Loader (`minecraft:...`)
- `src/worldgen/noise.rs`: deterministischer NoiseSampler
- `src/worldgen/terrain.rs`: TerrainSampler + Presets (`WorldgenPreset`)

---

## Implementierte Systeme

### Rendering (wgpu)
- 3D Pipeline mit Depth Buffer
- robuste Projektion (`OPENGL_TO_WGPU_MATRIX` + `perspective_rh`)
- Chunk-Rendering mit `u32` Indices (`Uint32`)
- Textur-Rendering über `texture_2d_array<f32>` + Nearest/Repeat Sampler
- World-Fragmentshader mit Alpha-Cutout (`discard` bei `alpha < 0.5`) für Cutout-Texturen
- 2D UI Pipeline für Crosshair + Block-Preview
- UI-Preview mit Alpha-Blending
- Distanz-Darkening/Fog im Fragmentshader
- Debug-Toggles:
  - `F3`: Culling an/aus
  - `F4`: Depth-Test an/aus

### Kamera + Input
- FPS-Kamera mit Maus-Look
- ESC toggelt Mouse Capture
- kontinuierlicher Render-Loop (`request_redraw`)
- FPS-Counter im Window-Title
- Hotbar-Selektion per `Digit1..Digit9`
  - Slot 6 (`Digit6`) ist fest `Sapling`

### Bewegung / Modi
- Zwei Modi: `Flight` und `Walking`
- Double-Space (nur echte Press-Transitions, kein Repeat) toggelt Modus beim 2. Press innerhalb `DOUBLE_SPACE_WINDOW = 0.20s`
- Walking:
  - Gravity aktiv
  - Jump auf erster Space-Eingabe sofort (wenn `on_ground`)
- Flight:
  - freie Bewegung (WASD horizontal, Space hoch, Shift runter)
  - Gravity aus (`velocity_y = 0`)
  - Auto-Exit zu Walking bei Bodenberührung **nur** bei Downward-Collision (`delta.y < 0 && hit_y_down`)
  - kein Auto-Exit bei Seitenkollision (`hit_x/hit_z`) oder Deckenkollision (`hit_y_up`)

### Voxel / World / Chunks
- Chunkgröße: `16x16x16`
- Weltgröße: `WORLD_XZ = 256`, `WORLD_MAX_Y = 64`
- Blocksystem:
  - `Air`
  - `Grass`
  - `Stone`
  - `Dirt`
  - `Sapling`
  - `OakPlanks`
- Harte World Border (Out-of-bounds kollidiert als solid)
- World-Koordinaten-Mapping via `div_euclid`/`rem_euclid`

### Blockmodelle / Meshing
- `BlockModel::Cube` und `BlockModel::CrossPlant`
- Greedy Meshing für Cube-Flächen (UV-Tiling/Repeat, kein Stretching)
- Sapling als Plant-Modell:
  - 2 gekreuzte vertikale Quads (X-Form)
  - doppelseitige Indizes (Culling-kompatibel)
  - kein Greedy-Merge für Pflanzen
- Sapling ist nicht-solid
- Oak Planks ist solid und auf allen 6 Faces gleich texturiert

### Textursystem
- Pro-Block/pro-Face Texturzuordnung
- Grass: Top/Side/Bottom getrennt
- Oak Planks:
  - `minecraft:textures/block/oak_planks.png`
- Sapling:
  - `minecraft:textures/block/oak_sapling.png`
- Pflicht-Keys werden beim Start geprüft (kein silent Fallback)
- Missing-Texture-Layer als defensiver GPU-Fallback vorhanden

### Interaktion
- DDA-Raycast aus Kamerarichtung
- LMB platzieren, RMB abbauen
- Source-of-Truth für Auswahl: `selected_block()` (Hotbar)
  - Placement nutzt `selected_block()`
  - Preview nutzt `selected_block()`
  - Place-Particles nutzen `selected_block()`
  - Break-Particles nutzen den tatsächlich abgebauten Block
- Chunk-Remesh nach Blockänderung inkl. Nachbar-Chunks an Grenzen
- Blinkendes Face-Highlight

### UI
- Crosshair als zwei dicke Balken (kein 1px-Linienkreuz)
- Block-Preview oben rechts
- Preview UV horizontal gespiegelt (`u -> 1-u`)

### Save/Load
- Binärformat `save/world.bin` mit Magic + Version + Dimensionsheader + Blockarray
- Enter speichert, `CloseRequested` speichert
- Startup-Policy via Enum:
  - `StartupWorldMode::LoadIfPresentElseGenerate` (Default)
  - `StartupWorldMode::AlwaysRegenerate`
- Bei inkompatiblem Save (Magic/Version/Dimensionen): klare Fehlermeldung + Fallback auf Generate

---

## Worldgen-Stand

- Preset-System vorhanden:
  - `Flatish`
  - `Rd161348Like`
  - `Hilly`
- Default-Preset: `Rd161348Like`
- 2D Heightmap-Worldgen mit Layering pro Säule:
  - Oberfläche `Grass`
  - darunter `Dirt`
  - darunter `Stone`
  - darüber `Air`
- Noise-Mischung in `src/worldgen/terrain.rs`:
  - Macro/Local/Ridge/Detail
  - Domain Warp
  - selektive Cliff-Maske
- Cliff-Effekt im rd161348-ähnlichen Preset aktiv (nicht global überall)
- Zusätzliche Kanten-Glättung:
  - `smooth_heightmap(..., 1)` in `World::generate_noise_world`
- Terrain-Tests in `src/worldgen/terrain.rs` bleiben grün

---

## Aktueller Fokus / Offene Punkte
1. Optionales weiteres Tuning der rd161348-Parameter für gewünschte Cliff-Dichte pro Seed.
2. Optionales UI-Feedback für aktiven Movement-Mode (`Flight`/`Walking`).
3. Optionales Preset-Toggling zur Laufzeit (derzeit Fokus auf stabilen Default).
