# Projektidee und Zielbild

## Ziel

Das Projekt rekonstruiert fruehe Minecraft-Versionen schrittweise, ohne die Engine-Qualitaet zu opfern.
Der Fokus ist:

- historische Spielgefuehl-Naehe (Classic/Pre-Classic)
- performante Runtime fuer grosse Sichtweiten
- saubere Erweiterbarkeit fuer neue Inhalte

## Warum Rust + wgpu

- hohe Laufzeitperformance und kontrollierbares Memory-Verhalten
- sichere Parallelisierung fuer Chunkgen/Meshing/Lighting/Storage
- portable moderne Grafik-API ueber `wgpu`

## Architekturprinzipien

1. Rendern, Simulation und Storage als getrennte Systeme mit klaren Budgets
2. Chunk/Subchunk (16x16x16) als zentrale Daten- und Recheneinheit
3. datengetriebene Registries statt starrer Hardcodierung in Hot Paths
4. stabile Frametimes wichtiger als Peak-FPS

## Ist-Stand (2026-03-01)

Die Basis ist umgesetzt:

- Chunk-Streaming mit Worker-Pipelines
- getrennte Render-/Simulation-Distanzen
- fixe Sim-Ticks (20 TPS) fuer Sim-Systeme
- Save/Load mit `world.meta` + Manifest + Region-Dateien
- Worldgen mit Presets, Carvers, Lakes/Springs

Details stehen in `docs/01_status_und_aktueller_stand.md`.
