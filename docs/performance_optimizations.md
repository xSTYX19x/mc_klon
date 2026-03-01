# Performance Optimizations

Stand: 2026-03-01

## Bereits umgesetzt

- Meshing Memory Pool (thread-local Reuse)
- Frustum-Culling fuer Render und Mesh-Dispatch
- Worker-Pipelines fuer Chunkgen, Meshing, Lighting, Storage
- Stage-Budgets und Burst-Modus im Streaming
- Far-LOD Streaming/Upload mit separaten Budgets
- Fixe Simulationsticks (20 TPS) mit Catchup-Limit
- Save/Load asynchron mit Queue-Coalescing und Autosave-Budgetierung
- Fluid-Mesh-Coalescing gegen Thrashing

## Hohe Prioritaet

1. Reproduzierbare Profiling-Szenen als Baseline dokumentieren
2. Worldgen-Pfadkonsolidierung (reduziert doppelte Arbeit/Branching)
3. Save-/Manifest-Migration fuer groessere Blockmengen vorbereiten

## Mittlere Prioritaet

1. Preset-spezifisches Worldgen-Tuning mit Snapshot-Tests
2. Ticking-Area-Workflows (Persistenz, Bedienung) vervollstaendigen
3. Weitere Upload-/Dispatch-Metriken im Overlay verfeinern

## Niedrige Prioritaet

1. GPU-driven Submission nur bei nachgewiesenem Drawcall-Bottleneck
2. Erweiterte Occlusion-Ansatzpunkte nur nach Messung

## Messung vor Optimierung

Mindestens erfassen:

- Phase-Zeiten (`chunkgen`, `meshing`, `lighting`, `upload`, `autosave`)
- Queue-Backlogs und Inflight-Zaehler
- Anzahl geaenderter Chunks pro Sim-/Fluid-Schritt
- Vorher/Nachher-Vergleich unter gleicher Szene und Settings
