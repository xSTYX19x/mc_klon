# Documentation

Open work lives in one place: [TODO.md](TODO.md) (German). When an item ships it
is deleted there; the documents below describe what exists.

## Overview

| Document | Contents |
|---|---|
| [vision.md](vision.md) | Goal, principles and the feature order the project follows |
| [status.md](status.md) | Current technical state of the code |

## Architecture

| Document | Contents |
|---|---|
| [architecture/runtime-contracts.md](architecture/runtime-contracts.md) | Shading/occlusion shapes, loading, settings and ore-generation contracts |
| [architecture/save-format.md](architecture/save-format.md) | Region file and column payload format (v2 active, v1 readable) |
| [architecture/content-foundation.md](architecture/content-foundation.md) | Block registry, manifest and migration-friendly content data |
| [architecture/meshing-memory-pool.md](architecture/meshing-memory-pool.md) | Thread-local buffer reuse in the meshers |
| [architecture/java-bedrock-takeaways.md](architecture/java-bedrock-takeaways.md) | Performance principles borrowed from established voxel engines |

## Reference

| Document | Contents |
|---|---|
| [reference/keybinds.md](reference/keybinds.md) | All key and mouse bindings by context |
| [reference/commands.md](reference/commands.md) | Chat commands; kept in sync by a test in `src/commands/mod.rs` |

## Planning

| Document | Contents |
|---|---|
| [planning/gameplay-backlog.md](planning/gameplay-backlog.md) | Gameplay, UI and rendering backlog by subsystem |
| [planning/worldgen.md](planning/worldgen.md) | World generation state and phase order |
| [planning/roadmap-c0.0.13a_03.md](planning/roadmap-c0.0.13a_03.md) | Historical roadmap up to the c0.0.13a_03 milestone |
| [planning/feature-reference-a1.0.17_04.md](planning/feature-reference-a1.0.17_04.md) | Feature overview of the a1.0.17_04 milestone used for gap analysis, with Minecraft Wiki sources (German) |

## Performance

| Document | Contents |
|---|---|
| [performance/optimizations.md](performance/optimizations.md) | Implemented optimizations and the reference measurement |
| [performance/audit-2026-08.md](performance/audit-2026-08.md) | Performance and rendering-correctness audit with fix log (German) |
