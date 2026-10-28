# mc_klon

A voxel sandbox engine written in Rust on top of `wgpu` and `winit`.

Gameplay features are added roughly in the order early block-building games
introduced them, starting with the Classic era. The engine underneath (rendering,
chunk streaming, world generation, storage and settings) is built against modern,
measured contracts so later content does not force a rewrite.

> **Status:** hobby project, work in progress. Expect breaking changes to the
> save format and settings between commits.

> **Disclaimer:** this is an independent project. It is not affiliated with,
> endorsed by or connected to Mojang Studios or Microsoft. "Minecraft" is a
> trademark of Mojang Synergies AB. No game assets are included in this
> repository.

## Features

- **Rendering:** `wgpu` with opaque/cutout/transparent layers, fog, sky and day
  cycle, precipitation, baked ambient occlusion, far LOD and a set of debug views
- **Chunk pipeline:** worker pools for generation, meshing, lighting and save/load
  with per-frame budgets; greedy meshing plus dedicated plant and liquid meshers
- **World generation:** seeded presets, density/surface rules, caves and ravines,
  lakes and springs, foliage and a separate ore pass
- **Simulation:** fixed 20 TPS tick, random ticks, water/lava flow and mixing,
  ticking areas and a separate simulation distance
- **Storage:** region files (`.mca`) with a palette-based column format, a content
  manifest and a v1 to v2 save migration
- **UI:** main/pause/options menus, world list with rename and delete, inventory
  and hotbar with 3D block previews, chat commands, GUI scale, FOV slider,
  third-person camera and screenshots
- **Resource packs:** folder or `.zip` packs in the Java Edition layout, selected
  in the options menu
- **Profiling:** opt-in trace output, stall reports and per-stage budget reports

## Getting started

### Requirements

- A recent stable Rust toolchain (edition 2024)
- A GPU and driver supported by `wgpu` (Vulkan, Metal or DirectX 12)

### Textures

The repository ships **no textures**. The engine loads them from
`assets/minecraft/textures/...` (Java Edition resource pack layout) and will not
start without them. See [assets/README.md](assets/README.md) for where to put a
pack and which files are needed.

### Build and run

```sh
cargo run --release
```

On first launch the game creates `save/`, `logs/`, `screenshots/` and
`resourcepacks/` in the working directory.

Migrate an old v1 save in `save/` to the current format:

```sh
cargo run --release -- --migrate-save-v1-to-v2
```

### Development

```sh
cargo check
cargo test
cargo test world::raycast::            # tests of one module
cargo clippy --all-targets
cargo fmt
```

`cargo clippy --all-targets -- -D warnings` is the target lint level; there are
still open warnings, tracked in [docs/TODO.md](docs/TODO.md).

#### Profiling

| Variable | Values | Effect |
|---|---|---|
| `MC_KLON_TRACE` | `off`, `ring` (default), `full` | `ring` keeps an in-memory buffer for stall reports, `full` also writes a JSON trace to `logs/` |
| `MC_KLON_TRACE_RING_MB` | number | Memory budget of the in-memory ring buffer (default 16) |

The Cargo feature `perf_trace` makes `full` the default for a build.

## Controls

| Input | Action |
|---|---|
| `W` `A` `S` `D`, mouse | Move, look (click into the window to capture the mouse) |
| `Space` / `Shift` | Jump or fly up / sneak or fly down |
| Double `Space` | Toggle walking and flying |
| `Ctrl` or double `W` | Sprint |
| `LMB` / `RMB` / `MMB` | Break / place / pick block |
| Mouse wheel, `1`-`9` | Select hotbar slot |
| `B` | Inventory |
| `T` | Chat and commands |
| `K` | Ticking-area editor |
| `F2` | Screenshot |
| `F5` | Cycle camera perspective |
| `F3` | Debug overlay |
| `Esc` | Pause menu / close current screen |

Most actions can be rebound under *Options > Controls*. The complete list,
including render and debug toggles, is in
[docs/reference/keybinds.md](docs/reference/keybinds.md); chat commands are in
[docs/reference/commands.md](docs/reference/commands.md).

## Save data

| Path | Contents |
|---|---|
| `save/settings.json` | Client settings and key bindings |
| `save/world.meta` | Seed, preset, height parameters, sea level |
| `save/manifest.json` | Block registry context with stable IDs |
| `save/ticking_areas.bin` | Ticking areas |
| `save/region/r.<x>.<z>.mca` | Chunk columns |

Lighting is rebuilt after loading. The format is described in
[docs/architecture/save-format.md](docs/architecture/save-format.md).

New worlds start in flight mode with the `ClassicFlatter` worldgen preset.

## Project layout

| Path | Contents |
|---|---|
| `src/app/` | Startup, event loop, menus |
| `src/render/` | GPU state, shaders, chunkgen/mesh/lighting workers, frame budgets |
| `src/meshing/` | Greedy, plant and liquid meshing, mesh pools |
| `src/world/` | Chunks, block access, fluids, raycast, time, ticking areas |
| `src/worldgen/` | Terrain, density, carvers, ores, noise |
| `src/storage/` | Region files, column codec, world metadata, migration |
| `src/blocks/`, `src/registry/` | Block definitions and IDs |
| `src/player/`, `src/camera.rs`, `src/input.rs` | Player movement, camera, input mapping |
| `src/sim/`, `src/commands/`, `src/config/` | Simulation tick, chat commands, configuration |
| `src/assets.rs`, `src/texture*.rs` | Resource pack lookup and texture loading |
| `src/perf/` | Tracing, stage budgets, allocation tracking |
| `docs/` | Design notes, formats, planning ([index](docs/README.md)) |

## Documentation

Start with [docs/README.md](docs/README.md). Most documents are in English;
the open-work list ([docs/TODO.md](docs/TODO.md)), the performance audit and the
a1.0.17_04 feature reference are written in German.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
