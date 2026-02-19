# mc_klon

## Texture Assets

This project resolves Mojang-style texture keys relative to:

- `assets/minecraft/`

Examples:

- `textures/block/stone.png`
- `textures/block/acacia_log.png`
- `textures/item/apple.png`

So the concrete file on disk is:

- `assets/minecraft/textures/block/stone.png`

## Current Default Texture

World chunk geometry uses a single default texture key:

- `textures/block/stone.png`

This is loaded at startup through the `TextureRegistry` and bound in the world render pipeline.

## Adding New Textures

1. Put your PNG into the Mojang-style tree under `assets/minecraft/`.
2. Use the key (without leading slash), e.g. `textures/block/my_block.png`.
3. Load it via `TextureRegistry::get_or_load(...)`.

Missing files produce a clear error containing both the texture key and the resolved filesystem path.
