# Java/Bedrock Takeaways (Performance North Star)

Status: 2026-03-04

## Short Summary

Three guidelines remain central for this repo:

1. Render != Simulation != Always-On
2. Streaming as a budgeted pipeline with backpressure
3. Shape the data layout so content can grow

## Status Matrix (Current Code)

| Recommendation | Status | Notes |
| --- | --- | --- |
| Separate render distance vs simulation distance | DONE | Both distances exist, with separate control (`F`/`G`). |
| Fixed timestep for simulation | DONE | Simulation systems run on a fixed tick (20 TPS, catchup limit). |
| Ticking areas / always-on zones | PARTIAL | Persistence exists (`save/ticking_areas.bin`) and there are shortcuts (`Ctrl+T` / `Ctrl+Shift+T`), but no complete in-game editor for radius/flags yet. |
| Chunk lifecycle with budgets/backpressure | DONE | Budgeted streaming stages, inflight/queue control, and coalescing are present. |
| Far LOD for large view distances | DONE | LOD streaming, LOD upload budgets, runtime toggle (`F6`). |
| Lighting as a separate asynchronous path | DONE | Lighting worker + generation-guarded apply path exist. |
| Registry/manifest as content foundation | DONE | `save/manifest.json` + runtime mapper are active. |
| Palette/bitpacking as runtime storage in the chunk | PARTIAL | Save codec has bitpacked arrays; the in-memory chunk is still classic (`BlockId`, `block_data`). |
| GPU-driven/indirect rendering | MISSING | Not yet a target for the near-term roadmap. |

## Next Architecture Steps

See "UI / UX", "Storage / Content Foundation", and "Worldgen" in `docs/TODO.md`.

## Guardrails

- Optimize only with reproducible measurements
- Prioritize frame times, not peak FPS
- No large data-layout refactor without a clear migration and test concept
