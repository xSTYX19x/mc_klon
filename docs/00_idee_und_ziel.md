# docs/00_idee_und_ziel.md

## Projektidee: Minecraft-Klon als Versions-Rekonstruktion

Ziel ist es, die Entwicklung von Minecraft **Version für Version** nachzucoden. Wir starten mit der frühesten praktikablen Spiel-Loop/3D-Rendering-Basis und arbeiten uns iterativ vor:
- erst: Fenster + Renderloop
- dann: Input + Kamera
- dann: erster 3D-Drawcall
- dann: Chunk-basierte Voxel-Welt (Meshing)
- später: mehrere Chunks, World-Management, Block-Interaktion, Texturen/Atlas, Terrain-Gen, Save/Load, Optimierungen (Culling, Greedy Meshing, Multithreading)

### Warum Rust?
- **Performance** nahe C/C++ (kein GC), gut für datenorientierte Engine-Teile (Chunks, Mesh-Build)
- **Speicher- & Thread-Sicherheit**: Chunk-Meshing/Streaming auf Worker-Threads wird stabiler
- **Tooling**: Cargo, rustfmt, clippy, einfache Dependency-Verwaltung
- **wgpu** als modernes Rendering-Backend (Vulkan/Metal/DX12) und stabiler Cross-Platform-Ansatz

## Engine-Architektur (Zielbild)

### Kernsysteme
1. **App/Window Loop (winit)**
   - Event-driven, `run_app`
   - erzeugt Window, dispatcht Events (Resize/Input/Redraw)

2. **Rendering (wgpu)**
   - Initialisierung: Instance → Surface → Adapter → Device/Queue
   - Pipeline: WGSL Shader + RenderPipeline
   - Pro Frame: Acquire Swapchain Texture → RenderPass → Present
   - Depth Buffer für 3D

3. **Inputsystem**
   - Tasten-States (WASD, Space, Shift)
   - Maus-Deltas (Raw Motion) für FPS-Look
   - Cursor capture togglen (ESC, optional Linksklick)

4. **Kamera**
   - Position + yaw/pitch (Euler)
   - `view = look_at`, `proj = perspective`
   - `view_proj = proj * view` als Uniform in den Shader

5. **Voxel/Chunks**
   - Chunk-Größe: 16×16×16 (Start)
   - BlockId: Air/Dirt/Stone
   - Meshing: “sichtbare Faces” → Quads → 2 Triangles → Vertex/Index Buffers
   - 1 Drawcall pro Chunk (Startpunkt)

## Entwicklungsstrategie / Regeln
- Erst korrekt & sichtbar, dann optimieren.
- Datenorientiert: flache Arrays für Blocks, kompakte Mesh-Buffers.
- Kleine Schritte, immer lauffähig halten.
- Später: Module/Crates aufteilen (render/input/world).

## Roadmap (nah)
1) Chunk-Mesh sichtbar (Flat World + Säule) ✅/in Arbeit
2) Mehrere Chunks (Grid) und World-Koordinaten
3) Block-Interaktion: Raycast + Setzen/Entfernen + Remesh
4) Texturen + UVs + Atlas
5) Terrain-Gen (Noise)
6) Optimierungen: Greedy Meshing, Frustum Culling, Multithread Mesh-Build
