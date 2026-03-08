# Runtime Contracts and Pipelines

Status: 2026-03-07

This file is the concise source of truth for the newer system contracts that were recently introduced.

## Rendering / Shading Contract

Rendering no longer relies on a generic "solid block" assumption for everything.
Each block can define separate:

- render shape
- collision shape
- selection/raycast shape
- occlusion shape

Current occlusion classes:

- `FullCube`
- `SlabLower`
- `SlabUpper`
- `CrossPlant`
- `Decorative`
- `Empty`

Rules:

1. face visibility is decided independently from AO
2. AO and side-darkening sample occlusion shape, not generic solidity
3. partial blocks do not act like full blockers unless their occlusion shape explicitly says so
4. the shader receives stable baked per-vertex shade/light inputs instead of inferring from ambiguous mesh state
5. AO alone must not drive faces to full black

Practical result:

- slab-adjacent black-face failures are removed
- plant/decorative blocks do not incorrectly occlude neighbors
- the system is ready for more non-cube blocks without another rewrite

## Startup Loading Flow

The app does not enter gameplay directly on boot anymore.

Current flow:

1. create app/render state
2. enter loading state
3. create/load world state
4. determine startup spawn target
5. enqueue prioritized chunk preparation around spawn
6. process the normal chunk pipeline in startup priority order:
   - terrain generation
   - generated column apply
   - CPU meshing
   - GPU upload
7. enter gameplay only after the critical startup set is ready

Current readiness rule:

- spawn-area critical chunks/sections must be generated, applied, meshed, and uploaded
- startup does not wait for the full outer radius forever
- near-to-far preparation is preferred so the first frame is meaningfully ready

## Settings Contract

Client settings are persistent and separate from transient runtime state.

Storage:

- file: `save/settings.json`

Current sections:

- `gameplay`
- `graphics`
- `controls`

Current options in this pass:

- `view_bobbing`
- `render_distance`
- `vsync`
- core gameplay keybindings

Rules:

1. settings load at startup
2. settings apply to runtime systems through explicit code paths, not UI-only flags
3. settings save when changed
4. control rebinding surfaces conflicts by swapping assignments instead of silently duplicating them

## Ore Generation Pipeline

Ore generation is a dedicated feature pass, not a terrain-noise side effect.

Current pass order in the column generator:

1. base terrain fill
2. carvers
3. ores
4. lakes/springs and post-fixes
5. shoreline
6. trees
7. foliage

Per-ore configuration currently supports:

- replacement rules
- vein size
- attempts/frequency
- height distribution
- exposed-air policy
- per-ore salt

Determinism and seam stability:

- RNG is seeded from world seed + preset + source chunk + ore salt
- ore attempts are sampled from source chunks around the target chunk
- neighboring source chunks can spill veins into the target chunk, which keeps chunk borders stable

## UI Item Preview Contract

Inventory and hotbar items no longer use a flat-only block path for block items.

Rules:

1. block previews reuse block preview meshes instead of building world meshes per frame
2. previews are cached per block type
3. cubes, slabs, and cross-plants share one preview renderer with fixed framing and lighting
4. menu button skinning and item previews should stay reusable across main menu, pause, inventory, and future screens
