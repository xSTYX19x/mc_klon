# Project Idea and Target Vision

## Goal

The project reconstructs early Minecraft versions step by step without sacrificing engine quality.
The focus is:

- historical gameplay feel (Classic/Pre-Classic) as the order in which features are introduced
- modern, robust system contracts for rendering, UI, streaming, storage, and worldgen
- a high-performance runtime for large view distances and later content expansion
- as little throwaway code as possible: features should not have to be rebuilt completely later

## Guideline as of 2026-03-07

New features are still introduced in historical order, but technically they are aligned with a modern Minecraft-like contract.
That means:

1. no purely historical quick hacks if they would force a complete rewrite later
2. UI/render/worldgen systems get clean data and rendering paths right away
3. visual and gameplay features are attached to a modern "contract" (options, persistence, atlas/UI paths, streaming budgets)
4. foundation first, then polishing, then content fill-in

## Why Rust + wgpu

- high runtime performance and controllable memory behavior
- safe parallelization for chunkgen/meshing/lighting/storage
- portable modern graphics API through `wgpu`

## Architecture Principles

1. rendering, simulation, and storage as separate systems with clear budgets
2. chunk/subchunk (`16x16x16`) as the central data and compute unit
3. data-driven registries instead of rigid hardcoding in hot paths
4. stable frame times matter more than peak FPS
5. visual correctness before cosmetic content: shader/lighting contracts must be stable
6. never build new player options as UI facades only; each one needs a clean runtime path and persistence path

## Current State (2026-03-07)

The current implementation direction is now visible in the codebase, not only in the roadmap:

- chunk streaming with worker pipelines
- separate render/simulation distances
- fixed sim ticks (20 TPS) for simulation systems
- save/load with `world.meta` + manifest + region files
- worldgen with presets, carvers, lakes/springs, foliage, and a dedicated ore feature pass
- startup loading now uses the loading-state path instead of spawning directly into unfinished terrain
- deterministic shading now uses explicit render/collision/raycast/occlusion shape contracts
- settings persistence exists with gameplay, graphics, and controls sections
- 3D preview rendering is now used in hotbar/inventory slots
- menu widgets now use textured GUI assets instead of gray placeholders

## Next Focus

The next phase is no longer about adding the missing foundation pieces above.
It is about extending those contracts without breaking them:

1. extend the shape/occlusion contract to more block families such as stairs, fences, panes, rails, and torches
2. make world lighting and exposure logic as shape-aware as the meshing path
3. deepen startup and streaming polish (saved spawn target, first-view prioritization, better warm-start metrics)
4. expand settings/UI coverage without reintroducing scattered hard-coded state
5. continue worldgen modernization through preset-specific tuning, more ore rules, and biome/material layering

Details are in:

- `docs/status.md`
- `docs/planning/gameplay-backlog.md`
- `docs/architecture/runtime-contracts.md`
- `docs/performance/optimizations.md`
- `docs/planning/worldgen.md`
- `docs/TODO.md`
