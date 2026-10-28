# Assets

No textures are distributed with this repository. The engine expects a resource
pack in the Java Edition layout as its built-in fallback:

```
assets/
  minecraft/
    textures/
      block/      stone.png, dirt.png, grass_block_top.png, ...
      item/
      entity/     skin.png (64x64 player skin, used by the third-person model)
      environment/ sun.png, moon_phases.png
      font/       ascii.png
      gui/
      particle/
```

`assets/minecraft/` is ignored by git, so whatever you put there stays local.

## Getting textures

Use a resource pack whose license allows you to use it, or the textures from
your own copy of the game. Copy or extract its `assets/minecraft/textures`
folder to `assets/minecraft/textures` here.

`*.png.mcmeta` animation files next to a texture are picked up as well.

## Resource packs

Additional packs go into `resourcepacks/` (created on first launch) as a folder
or a `.zip`, and are selected under *Options > Video / Graphics*. A pack only
needs the files it changes; everything else falls back to `assets/minecraft/`.
Both `textures/...` and `assets/minecraft/textures/...` layouts are detected.
Textures are uploaded at startup, so switching packs takes effect after a
restart.

If a texture the engine needs is missing from every source, startup fails with
an error naming the missing key.
