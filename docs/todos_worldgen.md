# Worldgen TODOs

Leitmotiv: Minecraft-ähnliche Welten nutzen typischerweise Noise (Perlin/fBm) als Basis statt rein deterministischer Stufenfunktionen.

## Erledigt (aktuell)

1. 2D-Heightmap-Worldgen mit mehrstufiger Noise-Mischung umgesetzt (Macro/Local/Ridge/Detail).
2. Domain-Warping gegen lineare Rampen/Bänder integriert.
3. Interne FastNoise-Frequenz auf `1.0` fixiert (Vermeidung doppelter Frequenzskalierung).
4. Smootheres Terrain-Tuning inkl. leichtem Heightmap-Smoothing-Pass (`smooth_heightmap(..., 1)`).
5. Worldgen-Regressionstest für Höhenbereich, Top-Clipping und Mindestgradient ergänzt.

## Offene Punkte

1. Presets einführen (`flatish`, `rd161348_like`, `hilly`) statt Single-Tuning.
2. Seeded World vollständig reproduzierbar machen (Seed in Save/Config persistieren).
3. Optionales Cliff-Shaping/leichte Erosion als separaten Pass einführen.
4. Biome- und Material-Layering erweitern (z.B. grass/dirt/stone abhängig von Höhe/Biome).
5. Optional: größere XZ-Welt oder Chunk-Streaming für besser sichtbare Terrainformationen.
