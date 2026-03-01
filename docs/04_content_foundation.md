# Content Foundation (datengetrieben, migrationsfaehig)

Stand: 2026-03-01

## Bereits vorhanden

- Runtime BlockRegistry
- Save-Manifest (`save/manifest.json`) mit stabilen IDs und Content-Signatur
- Mapper zwischen Manifest-IDs und Runtime-Blocks
- Save-Codec pro Sektion mit kanonischem Encoding (`Single` / `PaletteBitpacked`)

## Aktuelle Grenzen

- On-disk Block-IDs sind aktuell `u8` (Manifest-ID muss in `0..=255` bleiben)
- In-Memory-Chunk nutzt weiterhin `BlockId` + `block_data`
- Kein vollstaendiger datengetriebener BlockState-Stack (Properties/State-Explosion)

## Zielbild

1. Authoring ueber Keys (`namespace:name`), Runtime ueber kompakte IDs
2. BlockState-zentrierte Runtime-Entscheidungen (Render, Collision, Light)
3. versionierte Content-/Worldgen-Vertraege fuer reproduzierbare Welten

## Migrationspfad

### Schritt 1

- Manifest- und Registry-Vertrag stabil halten
- Save/Load-Tests fuer Signature-Drift, Unknown-Blocks, Remap-Faelle ausbauen

### Schritt 2

- Storage-ID auf `u16` vorbereiten (oder duales Format) fuer groessere Content-Sets
- Format-Version entsprechend erhoehen

### Schritt 3

- In-Memory auf BlockStateId + getrennte Dirty-Flags migrieren
- Danach optional Palette/Bitpacking auch zur Runtime nutzen

## Definition of Done

- Neue Content-Pakete koennen ohne Hot-Path-Umbau integriert werden
- Save bleibt lesbar oder klar migrierbar
- Debug-Ausgaben zeigen Manifest-/Versionierungsstatus eindeutig
