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

See `docs/TODO.md` for the current open-work list by subsystem.

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
- `docs/TODO.md` (remove the item)
- `docs/todos_worldgen.md` if worldgen is involved
- `docs/keybinds-index-markdown.md` if inputs/UI changed
