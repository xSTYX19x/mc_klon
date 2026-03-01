# Save System Region Format v1

Stand: 2026-03-01

Diese Spezifikation beschreibt den aktiven Save-Pfad.

## Scope

Persistiert werden:

- Blockinhalte pro Chunk-Sektion (`blocks`, `block_data`)
- World-Parameter (`seed`, `preset`, `sea_level`, `min_y`, `max_y`, `chunk_size`)
- Block-ID-Mapping-Kontext ueber World Manifest

Nicht persistiert (runtime-only):

- Lighting-Buffer (werden nach Load neu aufgebaut)
- Runtime-Revisionszaehler

## Dateilayout

- Save-Root: `save/`
- Meta: `save/world.meta`
- Manifest: `save/manifest.json`
- Regionen: `save/region/r.<rx>.<rz>.mca`

## `world.meta` (v1)

Pflichtfelder:

- `magic = "MKWM"`
- `format_version = 1`
- `world_seed` (`u64`)
- `preset` (`flatish | rd161348_like | hilly`)
- `sea_level`, `min_y`, `max_y`, `chunk_size`
- `block_id_version = 1`

Validierung ist strikt (`deny_unknown_fields` in Wire-Typ).

## `manifest.json` (v1)

Pflichtfelder:

- `format_version`
- `game_version`
- `content_signature`
- `blocks[]` mit `(id, key)`

Funktion:

- Mapping zwischen gespeicherten IDs und Runtime-Blockdefinitionen
- Fallback auf Missing-Block, wenn Manifest-Key im aktuellen Runtime-Registry fehlt

## Region Container (`.mca`)

Minecraft-aehnlicher Container:

- 8 KiB Header (Location + Timestamp Tables)
- 4 KiB Sektoren
- Chunk-Spalten als Blob mit Kompressionsbyte `2` (zlib)

## Column Payload (`MKCL`, v1)

- Header mit Koordinaten/Hoehenparametern
- sektionierte Daten (`cy`-Records)
- CRC32 ueber unkomprimierte Payload-Daten

Sektionen enthalten je 4096 Werte fuer:

- `blocks`
- `data`

Encoding pro Array:

- `Single(value)` oder
- `PaletteBitpacked`

## Wichtige Invariante (aktuell)

On-disk `blocks` speichern aktuell **Manifest-ID als `u8`**.
Damit muss jede benutzte Manifest-ID in `0..=255` liegen.
Groessere Content-Sets erfordern ein Format-Upgrade.

## Crash-Safety

- `world.meta`/`manifest.json` atomar via tmp + rename + sync
- Region Writes append-or-relocate, Header erst nach erfolgreichem Datenwrite aktualisieren

## Legacy-Grenze

`save/world.bin` gilt als Legacy. Der aktive Runtime-Pfad ist `world.meta` + `manifest.json` + `region/*.mca`.
