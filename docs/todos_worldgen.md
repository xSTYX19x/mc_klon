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

## High Priority

- [ ] Split ore profiles by preset instead of using one shared default profile
- [ ] Add neighbor-aware exposed-air checks so `HiddenOnly` ores are trustworthy at chunk borders
- [ ] Expand worldgen regression tests with seam, density, and height-distribution sampling
- [ ] Continue consolidating legacy/older worldgen entry points against the active worker pipeline
- [ ] Document preset differences and worldgen phase order more explicitly where contributors will find it quickly

## Medium Priority

- [ ] Add more ore families once replacement and height targets are defined clearly
- [ ] Introduce biome/material-aware replacement rules beyond plain stone
- [ ] Add richer height models if balancing needs more than uniform/triangle distributions
- [ ] Make more worldgen metrics visible in the debug overlay
- [ ] Prepare the worldgen config path for taller worlds without redoing ore anchors again

## Low Priority

- [ ] Decide and document long-term world size expectations more explicitly
- [ ] Add comparison screenshots / seed references per preset
- [ ] Revisit historical parity content only after the modern feature passes remain stable

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

- [ ] sample several fixed seeds and assert per-ore counts stay within defined tolerances
- [ ] keep deterministic tests for same-seed same-column equality
- [ ] add a visual/manual checklist for chunk-border seams
- [ ] measure that startup and normal chunkgen remain within acceptable warmup budgets after ore tuning
