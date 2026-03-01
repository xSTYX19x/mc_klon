# Memory Pool Implementation (Meshing)

Stand: 2026-03-01

## Ziel

Meshing-Allokationen pro Job reduzieren, um Frametimes bei vielen Remeshes zu stabilisieren.

## Umgesetzter Ansatz

- Thread-local `MeshingPool` (`src/meshing/pool.rs`)
- Reuse von:
  - Vertex-/Index-Buffern
  - Greedy-Mask-Scratch
  - LiquidMeshingPool (Corner-Cache)
  - temporaeren Arrays
- `with_meshing_pool(...)` resetet nur Inhalte, behaelt Kapazitaeten

## Liquid-Meshing

- `LiquidMeshingPool` in `src/meshing/liquids.rs`
- `append_liquids_in_chunk_pooled(...)` nutzt den Pool-Pfad
- Fallback ohne externen Pool nutzt ebenfalls thread-local Reuse

## Instrumentierung

- `src/perf/allocation_tracker.rs` bietet optionales Allocation-Tracking
- In `liquids.rs` ueber Feature-Gate `allocation_tracking` einhaengbar

## Erwarteter Effekt

- Weniger kurzlebige Allokationen im Meshing-Hot-Path
- Weniger Heap-Churn bei Fluid-/Greedy-lastigen Szenen
- stabilere Framezeiten unter Streaming-Last

## Relevante Dateien

- `src/meshing/pool.rs`
- `src/meshing/greedy.rs`
- `src/meshing/liquids.rs`
- `src/perf/allocation_tracker.rs`
