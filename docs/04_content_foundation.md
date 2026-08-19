# Content Foundation (Data-Driven, Migration-Friendly)

Status: 2026-03-04

## Already Present

- Runtime `BlockRegistry`
- Save manifest (`save/manifest.json`) with stable IDs and content signature
- Mapper between manifest IDs and runtime blocks (relevant especially for legacy v1 columns)
- Save codec per section with canonical encoding (`Single` / `PaletteBitpacked`)
- Active column write path with v2 name-palette encoding for `blocks`
- CLI migration from legacy v1 to v2 (`--migrate-save-v1-to-v2`)

## Current Limits

- V2 stores block names per section palette (robust, but potentially more storage/IO than numeric IDs)
- The in-memory chunk still uses `BlockId` + `block_data`
- No complete data-driven block-state stack yet (properties/state explosion)

## Target Picture

1. Authoring via keys (`namespace:name`), runtime via compact IDs
2. BlockState-centered runtime decisions (render, collision, light)
3. Versioned content/worldgen contracts for reproducible worlds

## Migration Path

See "Storage / Content Foundation" in `docs/TODO.md`.

## Definition of Done

- New content packs can be integrated without hot-path rewrites
- Saves remain readable or clearly migratable
- Debug output shows manifest/versioning status clearly
