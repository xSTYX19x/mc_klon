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

Current initial ore table (coal, iron, gold), split per preset:
`FLATISH_ORE_TABLE`, `CLASSIC_FLATTER_ORE_TABLE`, `RD161348_LIKE_ORE_TABLE`,
`HILLY_ORE_TABLE` in `worldgen/ores.rs`.

A preset changes *where* ore sits, not *how much* of it a world holds. An ore's
height band only yields ore where it intersects rock, so a band anchored above a
preset's terrain envelope wastes its samples in air: with the previous single
shared table, coal came out **1.93x** apart across the four presets (1777 for
`Rd161348Like` vs 3428 for `Hilly`, 25 columns, seed 132211) while iron and gold,
whose bands sit fully inside rock everywhere, stayed within 1.25x.

Each table therefore carries its preset's terrain envelope as the band cap plus
the attempt count that re-normalises the density. Measured spread afterwards:
coal 1.17-1.23x, iron 1.18-1.30x, gold 1.24-1.41x over two seeds. Guarded by
`worldgen::ores::tests::ore_density_stays_within_tolerance_across_presets`
(tolerance 1.6x) and `..::every_preset_gets_its_own_ore_table`.

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
