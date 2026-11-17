# Worldgen TODOs

Status: 2026-03-07

## Already Present

- presets (`Flatish`, `ClassicFlatter`, `Rd161348Like`, `Hilly`)
- seeded terrain + density/surface rules
- sea: everything below `sea_level` above the terrain is static water
- carvers (caves/ravines), kept out of water
- lakes/springs in the chunkgen worker
- shoreline post-processing: lake sand rims, Alpha sand/gravel beaches, clay
- sea level in `WorldConfig` and `world.meta`
- boundary-ocean visualization for hard world borders (preset-dependent)
- foliage pass (flowers, sugar cane, cacti, mushrooms)
- ore generation as its own deterministic feature pass

## Current Ore Feature Contract

The ore pass is intentionally modern in structure even if the content set is still early.

Current guarantees:

- dedicated feature pass after base terrain and carvers
- per-ore config for replacement, vein size, attempts, height distribution, exposure policy, and salt
- deterministic generation for a world seed and preset
- seam-stable cross-border veins by sampling neighboring source chunks (per
  ore, as far as its vein can reach; veins that miss the column skip their
  step maths but still draw their RNG values)

Current ore table, split per preset: coal, iron, gold, redstone, diamond, then
dirt and gravel pockets, in `FLATISH_ORE_TABLE`, `CLASSIC_FLATTER_ORE_TABLE`,
`RD161348_LIKE_ORE_TABLE`, `HILLY_ORE_TABLE` in `worldgen/ores.rs`.

Redstone, diamond and the pockets take Alpha 1.0.17's counts and vein sizes
(8x7, 1x7, 20x32, 10x32 per chunk, stone only). Their heights are mapped, not
copied: the pockets span bottom to the envelope top (coal's cap), and the deep
band is a quarter of the preset's envelope floor, as Alpha's `y` 0..15 is a
quarter of the rock below its lowest land. Alpha places the pockets before the
ores; here they come last so the calibrated coal/iron/gold placement is
unchanged.

A preset changes *where* ore sits, not *how much* of it a world holds. An ore's
height band only yields ore where it intersects rock, so a band anchored above a
preset's terrain envelope wastes its samples in air: with the previous single
shared table, coal came out **1.93x** apart across the four presets (1777 for
`Rd161348Like` vs 3428 for `Hilly`, 25 columns, seed 132211) while iron and gold,
whose bands sit fully inside rock everywhere, stayed within 1.25x.

Each table therefore carries its preset's terrain envelope as the band cap plus
the attempt count that re-normalises the density. Measured spread afterwards:
coal 1.17-1.23x, iron 1.18-1.30x, gold 1.24-1.41x, redstone 1.07-1.10x, diamond
1.21-1.49x (sampling noise), dirt 1.11x, gravel 1.14-1.20x over two seeds.
Guarded by
`worldgen::ores::tests::ore_density_stays_within_tolerance_across_presets`
(tolerance 1.6x) and `..::every_preset_gets_its_own_ore_table`.

## Priorities

See "Worldgen" in `docs/TODO.md`.

## Sea Level Contract

`WorldConfig::sea_level` is a worldgen *input*, not a ceiling for water:

- **the sea** fills every cell below it (`y < sea_level`, so the surface plane
  is at `sea_level`, like the boundary-ocean ring) that the terrain leaves
  empty, during the terrain phase, as static water
  (`fluid_data_make_static(0, false)`) that the fluid bootstrap leaves
  dormant; the surface under it is dirt (sand, gravel or clay after the
  shoreline phase), never grass
- **beaches** use Alpha's band `sea_level - 4 ..= sea_level + 1`

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

The order in `StreamingWorldColumnGenerator` (`render/chunkgen_worker.rs`):

1. terrain (`WorldgenPipeline::generate_column`): density → stone, then the
   surface rules (bedrock at y 0, grass/dirt top, dirt layer; dirt instead of
   grass below sea level), then the sea fill below `sea_level`. It also hands
   the carvers a `SeaGuard`: where sea water can sit across each column edge.
2. carvers (caves, ravines): besides Alpha's per-step water abort, a cell with
   water above or beside it is never carved (`carvers::WaterGuard`), across
   column edges via the `SeaGuard`, so sea water never hangs over air or
   stands beside it
3. ores (incl. dirt/gravel pockets)
4. lakes/springs + post-fixes (per section): a water lake below sea level
   keeps the sea in its air clearance; grass under water becomes dirt
5. shoreline/material cleanup: exposed dirt → grass, sand rims around lakes
   (water above `sea_level` only), Alpha beaches (sand vs. the rarer gravel by
   two low-frequency noises; a gravel beach loses its surface block), clay
   patches in sand under water, unsupported sand removed
6. trees
7. foliage/decorations: flowers; sugar cane at water edges and cacti on dry sand
   (temperate worlds only), each block only where its survival rule
   (`blocks::plants`) holds inside the column; mushrooms only where a backward
   light search shows the lit level stays at or below `MUSHROOM_MAX_LIGHT`
   (borders count as possibly lit from the neighbor column), so their random
   tick removes nothing after loading
8. winter surface (winter worlds only): snow layers, ice on exposed water
   except water touching gravel (Alpha's gravel beaches stay liquid) and water
   a light source in the column would melt again (ice melts above block light
   11)
9. grass settle (always last): grass under a skylight blocker or a fluid becomes dirt, using the same rule as the grass random tick (`blocks::grass_survives_under`), so freshly generated terrain causes no random-tick edits after loading

If future passes are added, they should fit into this ordered pipeline instead of bypassing it.

## Test / Acceptance Ideas

See "Worldgen > Test/acceptance ideas" in `docs/TODO.md`.
