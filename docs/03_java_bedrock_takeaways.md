# Java/Bedrock Takeaways (Performance North Star)

Stand: 2026-03-01

## Kurzfazit

Fuer dieses Repo bleiben drei Leitlinien zentral:

1. Render != Simulation != Always-On
2. Streaming als budgetierte Pipeline mit Backpressure
3. Datenlayout so gestalten, dass Content wachsen kann

## Statusmatrix (Ist-Code)

| Empfehlung | Status | Notes |
| --- | --- | --- |
| Render Distance vs Simulation Distance trennen | DONE | Beide Distanzen existieren, getrennte Steuerung (`F`/`G`). |
| Fixed timestep fuer Simulation | DONE | Sim-Systeme laufen ueber festen Tick (20 TPS, Catchup-Limit). |
| Ticking Areas / Always-On-Zonen | PARTIAL | Infrastruktur vorhanden (`TickingArea` in `render/state.rs`), aber noch kein vollstaendiger User-Workflow/Persistenz. |
| Chunk lifecycle mit Budgets/Backpressure | DONE | Budgetierte Streaming-Stages, Inflight-/Queue-Kontrolle, Coalescing vorhanden. |
| Far-LOD fuer grosse Sichtweiten | DONE | LOD-Streaming, LOD-Upload-Budgets, Runtime-Toggle (`F6`). |
| Lighting als separater asynchroner Pfad | DONE | Lighting-Worker + generation-guarded apply path vorhanden. |
| Registry/Manifest als Content-Basis | DONE | `save/manifest.json` + runtime mapper aktiv. |
| Palette/Bitpacking als Runtime-Storage im Chunk | PARTIAL | Save-Codec hat bitpacked Arrays; In-Memory-Chunk ist noch klassisch (`BlockId`, `block_data`). |
| GPU-driven/Indirect Rendering | MISSING | Noch kein Ziel fuer kurzfristige Roadmap. |

## Naechste Architektur-Schritte

1. Ticking-Areas productisieren (Konfiguration, Persistenz, UX)
2. Manifest-/ID-Migration fuer >256 Storage-IDs designen
3. Worldgen-Pfadkonsolidierung weiter vorantreiben

## Guardrails

- Optimieren nur mit reproduzierbaren Messungen
- Frametimes priorisieren, nicht Peak-FPS
- Kein grosser Datenlayout-Refactor ohne klares Migration- und Testkonzept
