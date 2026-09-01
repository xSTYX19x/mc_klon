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

## Sea Level Contract

`WorldConfig::sea_level` is a worldgen *input*, not a ceiling for water:

- **lava** is bounded by it — `make_lava_lake_candidate` clamps every lava lake
  to `sea_level - 2`
- **springs/rivers** deliberately start *above* it
  (`WATER_RIVER_SOURCE_MIN/MAX_ABOVE_SEA` = `sea_level + 6 .. + 28`) and descend
  until `sea_level + 1`
- **water lakes** follow the local terrain surface and take no sea-level
  argument at all; a pond on a hillside above sea level is intended

Guarded by
`world::config::tests::sea_level_from_world_config_bounds_lava_while_springs_may_start_above_it`,
which runs two sea levels so the lava bound cannot be met by a constant.

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
