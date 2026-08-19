# Worldgen TODOs

Status: 2026-03-07

## Already Present

- presets (`Flatish`, `ClassicFlatter`, `Rd161348Like`, `Hilly`)
- seeded terrain + density/surface rules
- carvers (caves/ravines)
- lakes/springs in the chunkgen worker
- shoreline post-processing
- sea level in `WorldConfig` and `world.meta`
- boundary-ocean visualization for hard world borders (preset-dependent)
- foliage pass (flowers/mushrooms)
- ore generation as its own deterministic feature pass

## Current Ore Feature Contract

The ore pass is intentionally modern in structure even if the content set is still early.

Current guarantees:

- dedicated feature pass after base terrain and carvers
- per-ore config for replacement, vein size, attempts, height distribution, exposure policy, and salt
- deterministic generation for a world seed and preset
- seam-stable cross-border veins by sampling neighboring source chunks

Current initial ore table:

- coal
- iron
- gold

## Priorities

See "Worldgen" in `docs/TODO.md`.

## Worldgen Phase Order

The current target order is:

1. terrain/base stone
2. carvers
3. ores
4. lakes/springs + post-fixes
5. shoreline/material cleanup
6. trees
7. foliage/decorations

If future passes are added, they should fit into this ordered pipeline instead of bypassing it.

## Test / Acceptance Ideas

See "Worldgen > Test/acceptance ideas" in `docs/TODO.md`.
