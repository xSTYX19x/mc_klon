# Save System Region Format (v2 Active, v1 Compatible)

Status: 2026-03-04

This specification describes the active save path.

## Scope

Persisted:

- block contents per chunk section (`blocks`, `block_data`)
- world parameters (`seed`, `preset`, `sea_level`, `min_y`, `max_y`, `chunk_size`)
- boundary parameters (`boundary_ocean`, `boundary_radius_chunks`)
- save/manifest context (`save_format_version`, `block_id_version`, `block_registry_hash`)
- ticking areas (`save/ticking_areas.bin`)

Not persisted (runtime-only):

- lighting buffers (rebuilt after load)
- runtime revision counters

## File Layout

- Save root: `save/`
- Meta: `save/world.meta`
- Manifest: `save/manifest.json`
- Ticking areas: `save/ticking_areas.bin`
- Regions: `save/region/r.<rx>.<rz>.mca`

## `world.meta` (format_version = 1)

Required fields:

- `magic = "MKWM"`
- `format_version = 1`
- `world_seed` (`u64`)
- `preset` (`flatish | rd161348_like | hilly`)
- `sea_level`, `min_y`, `max_y`, `chunk_size`
- `boundary_ocean`, `boundary_radius_chunks`
- `block_id_version = 1`
- `save_format_version` (`1` or `2`, actively produced: `2`)

Optional:

- `block_registry_hash` (manifest signature for drift detection)
- `display_name` (user-facing world name; falls back to the save directory name)

Validation is strict (`deny_unknown_fields` in the wire type). Optional fields are
additive: older saves load unchanged, but a `world.meta` written by a newer build
is rejected by builds that predate the field, so `format_version` stays at `1`
(bumping it would invalidate every existing save, since `validate` requires an
exact match).

Important validation rules:

- `chunk_size == 16`
- `min_y <= max_y`
- height range section-aligned (`min_y % 16 == 0`, `max_y % 16 == 15`)
- `sea_level` within `[min_y, max_y]`
- `boundary_radius_chunks >= 0`
- `save_format_version` only `1` or `2`

## `manifest.json` (v1)

Required fields:

- `format_version`
- `game_version`
- `content_signature`
- `blocks[]` with `(id, key)`

Function:

- registry/version context for save-load and diagnostics
- mapping fallbacks for legacy v1 columns (manifest ID -> runtime block)

## `ticking_areas.bin` (format_version = 1)

The historical filename is `.bin`, but the content is JSON.

Required fields:

- `format_version = 1`
- `areas[]` with:
  - `id`
  - `cx`, `cz`
  - `radius_chunks`
  - `flags_bits`

Semantics:

- `radius_chunks` is clamped to `0..32` on load/write
- `flags_bits` currently uses:
  - Bit 0: `KEEP_LOADED`
  - Bit 1: `TICK_FLUIDS`
  - Bit 2: `TICK_LIGHTING`
  - Bit 3: `TICK_ENTITIES`

Write strategy: atomic via tmp + rename.

## Region Container (`.mca`)

Minecraft-like container:

- 8 KiB header (location + timestamp tables)
- 4 KiB sectors
- chunk columns as blobs with compression byte `2` (zlib)
- region mapping via Euclidean division (`32x32` chunks per region), negative coordinates are mapped correctly

## Column Payload (`MKCL`)

### Active: `format_version = 2`

- header with coordinates/height parameters
- sectioned data (`cy` records)
- CRC32 over uncompressed payload data
- `blocks` as `NamePaletteBitpacked`:
  - palette of block keys (`String`)
  - bitpacked indices
- `data` still as `EncodedArray` (`Single` or `PaletteBitpacked`, `u8`)

### Legacy: `format_version = 1` (read-only compatible)

- `blocks` as manifest-based `u8` values
- can be migrated via CLI:
  - `cargo run -- --migrate-save-v1-to-v2`

## Important Invariants (Current)

- active v2 columns have **no** `u8` limit for `blocks` (string palette instead of manifest `u8`)
- legacy v1 columns remain readable (compatibility on load)
- `world.meta.save_format_version` must be `1` or `2`

## Crash Safety

- `world.meta` and `manifest.json`: atomic via tmp + rename + file/directory sync
- `ticking_areas.bin`: atomic via tmp + rename
- region writes append-or-relocate, update header only after a successful data write

## Legacy Boundary

`save/world.bin` is considered legacy. The active runtime path is `world.meta` + `manifest.json` + `ticking_areas.bin` + `region/*.mca`.
