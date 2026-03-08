# Gameplay / UI / Rendering Backlog with Modern Alignment

Status: 2026-03-07

## Principle

Features may come from older Minecraft eras, but the implementation target is a modern-quality system contract.
That means the backlog is organized around foundation reuse and extension, not around one-off historical hacks.

## Recently Completed Foundation

- deterministic shading/occlusion contract for cubes, slabs, plants, and decorative blocks
- startup loading screen with prioritized spawn-area preparation
- persistent settings and options UI
- classic first-person view bobbing tied to movement and settings
- smaller plant selection boxes for cross-plant blocks
- 3D hotbar/inventory previews backed by cached preview meshes
- textured menu buttons using GUI widget assets
- data-driven ore feature pass in worldgen

## Active Remaining Work

### Rendering

1. extend the occlusion, AO, and selection contracts to stairs, panes, fences, torches, rails, and similar non-cube blocks
2. align world lighting and exposure checks with the newer occlusion-shape rules used by meshing
3. add stronger debug tooling for baked shade/AO inputs and shape coverage

### Worldgen

1. split ore profiles by preset and tune them independently
2. add richer vertical distributions and more ore families once the balance targets are defined
3. introduce biome/material-aware replacement rules instead of stone-only rules
4. expand deterministic tests with seam, density, and startup-readiness sampling

### UI / UX

1. move any remaining legacy icon paths onto the same 3D preview renderer
2. improve options UX with reset/default flows, validation feedback, and more settings without scattering logic
3. add an in-game ticking-area editor instead of relying on shortcuts only

### Gameplay

1. wire hotbar digit keys and decide whether they become rebindable actions
2. keep future interaction boxes close to visible geometry for non-cube blocks
3. refine startup spawn target selection and first-view prioritization

### Performance

1. measure startup warmup, ore pass, and preview-cache costs under reproducible scenes
2. continue removing duplicate legacy paths in worldgen and storage
3. only consider deeper rendering optimization after those measurements exist

### Docs / Cleanup

1. keep status, backlog, worldgen TODOs, and control docs synchronized after each subsystem change
2. convert important contracts into short code comments near their source modules
3. remove stale assumptions whenever a system stops being experimental

## What Is No Longer Backlog

The following items were previously listed as upcoming and are now part of the implemented baseline:

- slab-adjacent shading cleanup and AO contract refactor
- startup boot flow behind the loading screen
- settings backbone with options menus and persistence
- bobbing option and final-camera bobbing layer
- smaller plant hit/selection boxes
- 3D hotbar/inventory previews
- textured Minecraft-style menu buttons
- modern-style ore generation pass

## Documentation Rule

After any feature in this file moves from backlog to implementation, update at least:

- `README.md`
- `docs/01_status_und_aktueller_stand.md`
- `docs/06_runtime_contracts_and_pipelines.md`
- `docs/todos_worldgen.md` if worldgen is involved
- `docs/keybinds-index-markdown.md` if inputs/UI changed
