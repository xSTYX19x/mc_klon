# Worldgen TODOs

Stand: 2026-03-01

## Bereits vorhanden

- Presets (`Flatish`, `Rd161348Like`, `Hilly`)
- Seeded Terrain + Density/Surface Rules
- Carvers (Caves/Ravines)
- Lakes/Springs im Chunkgen-Worker
- Sea-Level in `WorldConfig` und `world.meta`
- Lava-Lake-Hoehenbegrenzung unterhalb Waterline

## High Priority

- [ ] Legacy-/Altpfade in `src/world/mod.rs` gegen aktive Worker-Pipeline weiter konsolidieren
- [ ] Sea-Level/Waterline Source-of-Truth durchgaengig absichern (keine Drift zwischen alten Konstanten und Config)
- [ ] Boundary-Ocean-Visualisierung fuer harte Weltgrenzen umsetzen
- [ ] Worldgen-Phasen und Preset-Unterschiede klarer dokumentieren (welcher Pfad produziert was)

## Medium Priority

- [ ] Biome-/Material-Layering ueber aktuelle Surface Rules hinaus
- [ ] Preset-spezifisches Tuning fuer Caves/Ravines/Lakes/Springs
- [ ] Reproduzierbare Worldgen-Regression-Suite mit Snapshot-Vergleichen erweitern
- [ ] Mehr Worldgen-Metriken im Debug-Overlay sichtbar machen

## Low Priority

- [ ] Explizite Entscheidung und Doku zu langfristiger Weltgroesse (hart begrenzt vs. unendlich)
- [ ] Historische Paritaetsfeatures (Humans/Mob-Stub) vorbereiten
- [ ] Vergleichsscreenshots pro Preset in die Doku aufnehmen
