# TODO

Status: 2026-09-28 (Audit gegen Code-Stand `81757f3`)

Single source of truth for open/not-yet-done work. Previously this list was
duplicated across `01_status_und_aktueller_stand.md`,
`02_expanded_roadmap_to_c0.0.13a_03.md`, `03_java_bedrock_takeaways.md`,
`04_content_foundation.md`, `05_gameplay_feature_backlog_1_21_11_alignment.md`,
`performance_optimizations.md`, and `todos_worldgen.md`. Those files now point
here for open work; keep their "Already Implemented" / status sections
separate from this file.

When an item ships, delete it from here instead of marking it done.

**Audit 2026-09-28 — Lesehilfe.** Diese Runde hat jeden Punkt gegen den Code
geprüft (`cargo build --release`, `cargo clippy --all-targets` und alle 249
Tests grün). Abweichend von der Regel oben sind nachweislich fertige Punkte
ausnahmsweise mit `~~…~~ **done**` plus Beleg markiert statt gelöscht (dieselbe
Markierung wie schon unter "Performance"), damit der Abgleich nachvollziehbar
bleibt; veraltete Punkte tragen `**veraltet (2026-09-28)**` mit Begründung.
Beides beim nächsten Aufräumen löschen. Reine Zeilen- oder Faktenkorrekturen
an sonst gültigen Punkten stehen inline als *Korrektur 2026-09-28*.

Neue und neu formulierte Punkte folgen dem Schema **Ist → Ziel → Vorbild →
Vorgehen → Dateien → Abhängigkeiten → Priorität/Aufwand/Impact → Akzeptanz**.
Priorität P1 (als Nächstes) … P4 (Ausblick); Aufwand S (≤ 1 Tag), M (2-5 Tage),
L (1-3 Wochen), XL (> 3 Wochen).

Annahmen dieser Runde (vom Maintainer bestätigt): Zielsystem Mittelklasse-GPU
mit 6-8 Kernen, Sichtweite 12-16 Chunks bei 60 FPS; reines Singleplayer, keine
Vanilla-Kompatibilität; neue Blöcke per deklarativem Rust-Makro plus
Modell-Dateien (kein Laufzeit-Modding); Biome nach modernem Multi-Noise-3D-
Vorbild; Welten sollen migrierbar bleiben, große Umbauten an Worldgen oder
Block-States haben aber Vorrang vor Save-Kompatibilität.

## Architektur / Zukunftsfähigkeit

Ergebnis des Architektur-Audits 2026-09-28: Was muss sich ändern, damit
Block-States, Nicht-Würfel-Blöcke, Interaktion, Entities, Biome und Wetter
ohne späteren Umbau hineinpassen? Die Punkte stehen dort, wo sie thematisch
hingehören; diese Liste gibt die **Reihenfolge** vor, weil jeder Schritt den
nächsten billiger macht:

1. Welt-Änderungsjournal (unten) — behebt einen echten Bug und ist die
   Voraussetzung für alles, was die Welt außerhalb von Spieler-Edits ändert.
2. Block-Registry per Makro, `BlockId` → `u16` vorbereiten (unten).
3. Block-State-Datenschicht (→ "Content-Abgleich / Querschnitts-Voraussetzung:
   Block-States").
4. Blockmodelle und Voxel-Shapes (→ "Rendering", erster Punkt).
5. Block-Verhalten: Interaktions-Hooks (→ "Survival-Gameplay / Interaktion")
   plus Neighbor-Updates und Scheduled Ticks (unten).
6. Spiellogik aus `State` lösen, deterministischer Tick, Interpolation (unten)
   — Voraussetzung für Entities.
7. Entity-Fundament (→ "Content-Abgleich / Entities & Mobs").
8. Column-Payload v3 für Block-Entities, Entities und Biome (→ "Storage").
9. Biome und Multi-Noise-Terrain (→ "Worldgen / Biome & Multi-Noise-Terrain").
10. Wetter und Himmelslicht (unten).

- **Welt-Änderungsjournal: jede Block-Änderung läuft durch dieselbe Pipeline**
  - *Ist:* Die Nebenwirkungen eines Edits (Remesh-Dirty, Licht-Reparatur,
    Fluid-Notify, Sand-Check, Save-Dirty) hängen an
    `State::enqueue_pending_block_edit` (`render/state.rs:17464`), das nur
    Spieler-Edits (`:19192`, `:19356`), der Winter-Tick
    (`try_apply_environment_block_change`, `:18770-18786`) und Fluid-Positionen
    (`:18952-18963`) erreichen. `World::set_block*` (`world/mod.rs:3219-3336`)
    selbst meldet nichts. **Bug:** Random Ticks laufen als `SimSystem` direkt
    gegen `World` (`world/random_ticks_system.rs:111`) — Gras-Ausbreitung
    (`blocks/mod.rs:2439`, `:2475`) und Setzlingswachstum
    (`place_sapling_tree`, `:2699-2716`) ändern Blöcke, ohne dass Mesh, Licht
    oder Save-Dirty davon erfahren. Ein gewachsener Baum bleibt unsichtbar, bis
    der Chunk aus anderem Grund neu gemesht wird, und geht beim Entladen
    verloren, wenn die Spalte nicht aus anderem Grund in `dirty_columns` steht
    (`render/state.rs:14156`).
  - *Ziel:* Genau ein Weg, auf dem Block-Änderungen Nebenwirkungen auslösen,
    egal ob Spieler, Sim-System, Explosion, Befehl oder Entity sie verursacht.
  - *Vorbild:* Vanilla `Level.setBlock` mit Update-Flags (Nachbarn
    benachrichtigen, neu rendern, dirty markieren). Ohne Client/Server reicht
    ein Journal, das der Frame-Thread leert. Rust-Übertragung: ein
    `Vec<BlockChange { pos, old, new, old_data, new_data }>` in `World`, das
    alle Setter füllen — kein Callback, kein Trait-Objekt.
  - *Vorgehen:* 1) `World::changes: Vec<BlockChange>` plus
    `drain_block_changes(&mut Vec<_>)`; alle `set_block*`-Varianten pushen
    (ersetzt nebenbei `pending_block_entity_block_changes`,
    `world/mod.rs:3250-3334`). 2) `State` leert das Journal nach
    `fixed_sim_tick` und nach jedem Spieler-Edit und speist
    `enqueue_pending_block_edit`. 3) Die direkten Aufrufe an den heutigen
    Stellen entfallen, damit nichts doppelt läuft. 4) Batch-Pfad für viele
    Änderungen auf einmal (TNT, `/fill`, Baum): Dirty-Marken koalesziert
    `pending_block_edit_dirty_chunks` schon pro Chunk (`render/state.rs:5473`,
    Flush `:16368-16402`); Lighting-Klassifikation und Fluid-Notify laufen aber
    pro Position (`:17565-17590`) und brauchen für Batches eine Region-Variante.
  - *Dateien:* `world/mod.rs`, `world/random_ticks_system.rs`,
    `world/block_entities_system.rs`, `render/state.rs` (Edit-Pipeline
    `:17464-17647`).
  - *Abhängigkeiten:* keine. Voraussetzung für Neighbor-Updates/Scheduled
    Ticks, TNT, Redstone und Entities, die Blöcke ändern.
  - *Priorität/Aufwand/Impact:* **P1**, M, hoch (Bugfix + Fundament).
  - *Akzeptanz:* Test ohne GPU: ein Random Tick lässt einen Setzling wachsen →
    das Journal enthält alle Stamm-/Laubpositionen. Im Spiel: Setzling bei
    hochgedrehtem `RANDOM_TICK_SPEED` wächst sichtbar, und der Baum ist nach
    Entladen/Neuladen der Spalte noch da.

- **Block-Registry per Makro, `BlockId` auf `u16` vorbereiten**
  - *Ist:* Ein neuer Block braucht Handarbeit an vier Stellen: Enum-Variante
    mit explizitem Diskriminanten (`blocks/mod.rs:83-137`, `#[repr(u8)]`),
    Zeile in `BLOCK_SPECS: [BlockSpec; 51]` (`:700`), Eintrag in
    `CLASSIC_PALETTE_ORDER: [BlockId; 43]` (`:139`), und beide Array-Längen
    müssen mitwachsen. `repr(u8)` deckelt die Registry bei **256 Blocktypen**;
    mehrere Tabellen hängen an dieser Zahl (`by_block: [BlockTextureInfo; 256]`,
    `:2199`; `runtime_to_manifest_lut: [u32; 256]`, `world/manifest.rs:129`).
    Der Vanilla-Umfang liegt bei über 1000 Blocktypen. Daneben baut
    `BlockRegistry::new()` bei jedem Aufruf eine `HashMap<BlockId, BlockDef>`
    (`blocks/mod.rs:2071-2085`) — auch pro gespeicherter/geladener Spalte
    (`storage/chunk_codec.rs:304`, `:343`, `:672`) und einmal im
    `RandomTicksSystem`, das `defs` gar nicht liest
    (`world/random_ticks_system.rs:20`). Positiv: das Speicherformat speichert
    Blocknamen, nicht IDs (`chunk_codec.rs:110-117`) — die Laufzeit-ID ist frei
    änderbar, ohne Saves zu brechen.
  - *Ziel:* Ein Block = ein Eintrag an genau einer Stelle; Laufzeit-IDs dicht,
    aus der Deklarationsreihenfolge vergeben; Blockmodelle und
    Blockstate-Varianten als Dateien (siehe "Rendering").
  - *Vorbild:* Vanilla `Registry` + `BlockBehaviour.Properties` (Builder);
    FerriteCore (Property-Tabellen einmal pro Block statt pro State).
    Rust-Übertragung: ein `macro_rules!`-Makro `blocks! { stone => {…}, … }`,
    das Enum, `BLOCK_SPECS` und `BLOCK_COUNT` erzeugt. Eine Laufzeit-Registry
    ist unnötig, weil es kein Laufzeit-Modding gibt.
  - *Vorgehen:* 1) Makro einführen, das aus einer Liste Enum + Tabelle + Länge
    generiert; die 51 Einträge umziehen, Diskriminanten bleiben identisch
    (`debug_assert_block_blueprint_integrity`, `blocks/mod.rs:2785`).
    2) Palette-Reihenfolge als Feld `creative_order: Option<u16>` statt
    zweitem Array. 3) `BlockRegistry` auf eine `&'static`-Sicht über die
    Tabelle reduzieren (keine HashMap, kein `new()` pro Spalte). 4) `repr(u8)`
    → `repr(u16)` erst, wenn mehr als ~200 Blöcke absehbar sind: kostet 4 KiB
    mehr pro `ChunkBlocks::Flat`-Sektion (`world/chunk.rs:332-338`) und im
    Mesher-Halo (`meshing/neighborhood.rs:37-50`); paletted/uniform Sektionen
    und das Speicherformat bleiben gleich. Vorher den Anteil Flat-Sektionen
    messen (siehe Performance "Editierte Sektionen wieder kompaktieren").
  - *Dateien:* `blocks/mod.rs`, `registry/mod.rs`, `world/manifest.rs`,
    `storage/chunk_codec.rs`, `world/random_ticks_system.rs`; Items
    (`src/items/`, siehe "Items, Inventar & Crafting") in derselben Bauform.
  - *Abhängigkeiten:* keine. Vor der Block-State-Schicht und vor den
    a1.0.17-Blöcken erledigen, damit diese nicht mehr von Hand in feste Arrays
    müssen.
  - *Priorität/Aufwand/Impact:* **P2**, M, hoch für das Content-Tempo.
  - *Akzeptanz:* Neuer Testblock = eine Makrozeile + Textur, kein Edit an
    Array-Längen; `cargo test` grün; ein Save von vorher lädt unverändert.

- **Block-Verhalten: Neighbor-Updates und Scheduled Ticks für Blöcke**
  - *Ist:* Es gibt nur Random Ticks (`RandomTickFn`, `blocks/mod.rs:81`; zwei
    Nutzer: Gras `:741`, Setzling `:804`). Ein generischer
    Neighbor-Update-Mechanismus fehlt (Suche nach
    `neighbor_update|on_neighbor|scheduled_tick` leer); die nächsten
    Verwandten sind fluid-spezifisch (`notify_fluid_on_block_change`,
    `render/state.rs:18729`; `mark_transforming_with_neighbors`,
    `world/fluids.rs:526-538`) plus der Sand-Check (`render/state.rs:17470-17536`).
    Scheduled Ticks gibt es nur für Fluids, dort aber schon als gutes
    Timing-Wheel (`FluidScheduler`, `world/fluids.rs:190`, `:210-217`, 512
    Buckets, Dedupe über `scheduled_due`).
  - *Ziel:* Fackeln fallen ab, wenn ihr Träger verschwindet; Kies fällt;
    Zuckerrohr und Kaktus prüfen ihren Untergrund; Redstone und Türen reagieren
    auf Nachbarn; Blöcke können sich "in N Ticks" selbst wecken — alles über
    dieselbe Tabelle wie `random_tick`.
  - *Vorbild:* Vanilla `neighborChanged`/`updateShape` und `LevelTicks<Block>`
    (geplante Ticks mit Priorität); Lithium für die Tick-Queue-Struktur.
    Rust-Übertragung: Funktionszeiger in `BlockSpec` (wie `random_tick`
    heute), kein Trait-Objekt pro Block.
  - *Vorgehen:* 1) `BlockSpec` bekommt `on_neighbor_changed:
    Option<NeighborFn>` und `on_scheduled_tick: Option<ScheduledTickFn>`, beide
    mit Signatur `fn(&mut World, IVec3, BlockState, …)`. 2) `FluidScheduler`
    zu einem generischen `TickScheduler<K>` verallgemeinern und eine Instanz
    für Blöcke anlegen; die Fluids behalten ihre. 3) Ein
    `BlockUpdatesSystem: SimSystem` leert pro Tick das Welt-Journal
    (Punkt oben), ruft für die 6 Nachbarn jeder Änderung `on_neighbor_changed`
    auf (Tiefenlimit gegen Kettenreaktionen, Vanilla: 512) und arbeitet fällige
    Scheduled Ticks mit Budget ab. 4) Sand-Check aus `State` hierher umziehen
    und mit Kies zusammenführen (`falls: bool` in `BlockSpec`). 5) Pruning nach
    aktiven Spalten wie beim Fluid-Scheduler.
  - *Dateien:* `blocks/mod.rs`, `world/fluids.rs` (Scheduler extrahieren),
    neues `world/block_updates_system.rs`, `render/state.rs` (Sand-Pfad).
  - *Abhängigkeiten:* Welt-Änderungsjournal; Block-States (Fackel-Orientierung,
    Zuckerrohr-Alter). Basis für "Redstone & Logik".
  - *Priorität/Aufwand/Impact:* **P2**, M-L, hoch (Voraussetzung für rund 20
    Inhaltspunkte).
  - *Akzeptanz:* Tests ohne GPU: Block unter Sand entfernen → Sand fällt, Kies
    ebenso; ein Block mit `schedule(pos, 10)` tickt genau in Tick +10; eine
    Kettenreaktion bricht am Tiefenlimit ab.

- **Spiellogik aus `State` lösen: `GameSim`, deterministischer Tick,
  Render-Interpolation**
  - *Ist:* `State` (`render/state.rs:5435-5754`, Datei mit 21 067 Zeilen)
    besitzt zugleich GPU-Ressourcen, die Welt (`:5464`), die Sim-Systeme
    (`:5590-5600`), Wetter, Partikel, Zeit, Streaming und Spielregeln
    (Abbauen/Platzieren `:19103-19410`); nichts davon ist ohne Fenster/GPU
    testbar (`State::new`, `:8935`). Der Sim-Tick läuft versteckt in
    `update_fluid_simulation` (`:18926-18971`): der Akkumulator ist auf 2 Ticks
    geklemmt, dazu kommt ein 3-ms-Wall-Clock-Budget (`:18945`,
    `FLUID_SIM_BUDGET_MS_PER_FRAME`, `:1316`). Überschüssige Zeit wird
    verworfen — die Sim läuft unter Last langsamer statt nachzuholen, und wie
    viele Ticks laufen, hängt von der Wanduhr ab. Spielerbewegung läuft pro
    Frame mit variablem, ungeklemmtem `dt` (`app/mod.rs:3771-3776`,
    `:3949-3985`); Interpolation zwischen Tick-Zuständen gibt es nicht (Suche
    nach `interpolat|prev_pos|partial_tick` leer). Positiv: `World`,
    `player/`, `sim/` und `worldgen/` importieren kein wgpu und sind schon
    heute isoliert testbar.
  - *Ziel:* Ein GPU-freies `GameSim` (Welt, Sim-Systeme, Zeit, Wetter,
    Spielregeln, Logik der Edit-Pipeline), das mit festem 20-TPS-Takt
    deterministisch tickt und headless testbar ist; `State` rendert nur noch
    und interpoliert Entities mit `alpha = accum / tick_len`. Kein
    Client/Server-Modell — nur eine saubere Grenze, damit `GameSim` später
    optional auf einem eigenen Thread laufen kann.
  - *Vorbild:* Vanilla `MinecraftServer.tickServer` + `partialTick` im
    Renderer ("Fix Your Timestep"). Übertragung: im Singleplayer braucht es
    keine Serialisierung zwischen den Hälften; `GameSim` übergibt dem Renderer
    das Welt-Journal und Entity-Snapshots (vorherige + aktuelle Position).
  - *Vorgehen:* 1) Tick-Treiber aus `update_fluid_simulation` in einen eigenen
    `SimClock` ziehen; der Budget-Abbruch bleibt nur als Notbremse, mit Zähler
    "verlorene Ticks" im F3-Overlay. 2) `destroy/place/pick_block_at_camera_ray`
    in ein GPU-freies Modul (`gameplay/`) verschieben, das auf `&mut World` +
    Raycast arbeitet und das Journal füllt. 3) Wetter, Zeit, Sim-Systeme und
    Edit-Pipeline-Logik in `GameSim` bündeln; `State` hält `GameSim` als Feld.
    4) Spieler vorerst pro Frame lassen (Spielgefühl), aber die Positionen, die
    Mobs und Kollision sehen, pro Tick festhalten. 5) Headless-Testharness:
    `GameSim::new(seed)` + `tick(n)`.
  - *Dateien:* `render/state.rs`, `app/mod.rs`, neues `src/game/` (oder
    Erweiterung von `sim/`), `player/mod.rs`.
  - *Abhängigkeiten:* Welt-Änderungsjournal. Voraussetzung für das
    Entity-Fundament (Interpolation) und für Sim-Logik auf einem eigenen
    Thread; eng verwandt mit "Decouple frame rate from chunk streaming".
  - *Priorität/Aufwand/Impact:* **P2**, L (schrittweise), hoch (Testbarkeit,
    Entities).
  - *Akzeptanz:* Headless-Test: gleicher Seed + gleiche Eingaben + 1000 Ticks
    → bitgleiche Welt; F3 zeigt im Normalbetrieb 0 verlorene Ticks;
    `render/state.rs` enthält keine Spielregel mehr (keine `PlayerMode`-Prüfung
    außerhalb reiner Parameterweitergabe).

- **Wetter und Himmelslicht**
  - *Ist:* Niederschlag ist ein manueller Schalter (F11 →
    `toggle_precipitation`, `app/mod.rs:3389-3393`) und beim Laden nur für
    Winterwelten an (`reset_weather_for_environment`,
    `render/state.rs:6239-6243`). Regen färbt Himmel und Nebel
    (`:19546-19549`, `:19819-19824`), dunkelt das **Terrain-Himmelslicht aber
    nicht** ab: `sky_params[0]` kommt ungefiltert aus
    `world_time.sky_brightness` (`:19807`). Kein Gewitter, kein Blitz, kein
    Wetterzyklus, kein Niederschlag je Biom; Wetter- und Zeitzustand werden
    nicht gespeichert (siehe "Storage: Weltzeit und Wetterzustand").
  - *Ziel:* Wetterzyklus mit zufälligen Dauern (klar/Regen/Gewitter),
    Niederschlagsart je Biom und Höhe (Regen/Schnee/keiner), Abdunklung des
    Himmelslichts im Shader, Blitz als kurzer Lichtblitz (als Entity später).
  - *Vorbild:* Vanilla `rainTime`/`thunderTime` in den Level-Daten,
    `Level.getSkyDarken` (Regen und Gewitter dunkeln das Himmelslicht ab),
    `Biome.getPrecipitationAt`. Direkt übertragbar; die Abdunklung ist ein
    Uniform-Faktor, kein Relight.
  - *Vorgehen:* 1) `WeatherState { rain_ticks_left, thunder_ticks_left,
    raining, thundering }` in `GameSim`, tick-getrieben und seed-basiert.
    2) `sky_params[0]` in `update_camera_uniform` (`render/state.rs:19786`)
    mit Regen-/Gewitterstärke skalieren. 3) Niederschlagsart je Spalte aus dem
    Biom (bis Biome existieren: aus `WorldEnvironment`,
    `world/environment.rs:29-34`). 4) Schneefall in Winterwelten an den
    Wetterzustand koppeln statt Dauer-an.
  - *Dateien:* `render/state.rs` (Wetter `:6228-6272`, Uniform `:19786`),
    `world/environment.rs`, `app/mod.rs`, `storage/world_meta.rs`.
  - *Abhängigkeiten:* Biome (für Niederschlag je Biom; mit `WorldEnvironment`
    als Stellvertreter vorher machbar); "Storage: Weltzeit und Wetterzustand".
  - *Priorität/Aufwand/Impact:* **P3**, M, mittel.
  - *Akzeptanz:* Bei Regen sinkt die Terrainhelligkeit sichtbar (F8
    Lighting-View); Wetter wechselt ohne Eingabe; der Zustand übersteht
    Speichern/Laden.

## Rendering

- **Blockmodelle und Voxel-Shapes für Nicht-Vollblöcke** (neu formuliert
  2026-09-28; ersetzt "extend the occlusion/AO/selection contract to non-cube
  shapes: stairs, panes, fences, torches, rails", dessen Inhalt hier vollständig
  aufgeht — die dort offene Frage "teilt Geometrie-State die 8 Bit von
  `block_data`?" ist unter "Block-States" beantwortet: ja)
  - *Ist:* **Kein Rendering-Fix, sondern ein Content-Feature mit
    Voraussetzung.** Geometrie ist ein geschlossenes Enum mit 8 Varianten
    (`BlockModelSpec`, `blocks/mod.rs:413-449`); Texturen unterscheiden nur
    oben/Seite/unten (`BlockDef.textures: [TextureKey; 6]`, `:1807`). Es gibt
    keine Cuboid-Elemente, keine Per-Face-UVs, kein `cullface`, keine Rotation,
    keine Multipart-Modelle. Shapes sind **genau eine AABB** pro Block in
    1/16-Pixeln (`BlockAabb`, `:253-257`; `CollisionShape`, `:290-296`).
    Raycast-/Outline-Shape (`raycast_shape`, `:556-570`; Outline
    `render/state.rs:19762-19783`) und `OcclusionShape` (`:341-348`;
    `SlabUpper` wird nie erzeugt, Compiler-Warnung `never constructed`) sind
    schon getrennt — der Contract steht, nur die Datenform ist zu schmal.
    Treppen brauchen 2 Boxen, Zäune eine 1,5 Blöcke hohe Kollision, Scheiben
    und Zäune Verbindungen zu Nachbarn. Kollision prüft nur Zellen, die die
    Spieler-AABB überlappt (`aabb_collides`, `player/mod.rs:143-145`) — eine
    Box, die aus dem Block darunter herausragt (Zaun), würde übersehen. Der
    Raycast testet schon pro AABB (`world/raycast.rs:192-212`), gibt aber den
    Trefferpunkt nicht zurück (`t_hit` wird bei `:197` berechnet und
    verworfen). Einen Nicht-Greedy-Pfad gibt es nur für Kreuzpflanzen
    (`meshing/plants.rs`). Der Contract ist bewacht:
    `blocks::interaction_shape_tests::every_model_states_its_raycast_shape`
    (`blocks/mod.rs:2962`) matcht erschöpfend auf `BlockModelSpec`.
  - *Ziel:* Jede Blockform (Stufe, Treppe, Zaun, Scheibe, Tür, Fackel, Leiter,
    Schiene, Schild, Schneeschicht) ist ein Modell aus Daten plus getrennte
    Kollisions-, Outline- und Occlusion-Shapes pro Block-State. Greedy-Meshing
    bleibt für Vollwürfel, alles andere läuft über gebackene Modell-Quads.
  - *Vorbild:* Vanilla-Blockmodelle (`elements` mit `from`/`to`, `faces` mit
    `uv`/`texture`/`cullface`, `rotation`), Blockstate-`variants` und
    `multipart`, `VoxelShape` (getrennt: Kollision, Outline, Occlusion). Format
    frei: RON oder JSON im Asset-/Resourcepack-Pfad, beim Start zu "Baked
    Quads" pro State kompiliert. Übertragung: Shapes als
    `&'static [BlockAabb]` statt Java-Objektgraph; Occlusion pro Fläche als
    4×4-Bitmaske (`u16`) statt exakter Flächenvergleiche — das reicht für alle
    Vanilla-Formen und ist im Mesher ein einziges AND.
  - *Vorgehen:* 1) Shapes verbreitern: `CollisionShape::Boxes(&'static
    [BlockAabb])`, `BlockAabb` mit `i8`-Koordinaten (−16…32) für Überstände;
    `aabb_collides` prüft zusätzlich die Zellreihe unter der AABB.
    2) `RaycastHit` um Trefferpunkt und Fläche erweitern (für den
    Platzier-Kontext, siehe "Interaktion"). 3) Modellformat + Loader über den
    `assets.rs`-Pfad, damit Resourcepacks Modelle überschreiben können, und ein
    Bake-Schritt Modell × State → Quad-Liste mit `cullface`, gecacht pro State.
    4) `meshing/plants.rs` zu `meshing/models.rs` verallgemeinern statt es zu
    kopieren (pro Voxel, gebackene Quads, Culling per `cullface` gegen die
    Occlusion-Maske des Nachbarn); der Greedy-Merger sieht nur Vollwürfel,
    sonst verschmelzen unterschiedliche Orientierungen. 5) Multipart/Connected:
    Verbindungsbits beim Meshen aus den Nachbarn **ableiten** statt speichern
    (Zaun, Scheibe, Redstone-Seiten) — spart State-Bits. 6) AO für Modell-Quads
    nur auf Flächen, die bündig auf der Blockgrenze liegen. 7) Pro Formfamilie
    Textur, Platzierlogik und Inventareintrag der Blöcke aus "Blöcke (Inhalt)"
    nachziehen.
  - *Dateien:* `blocks/mod.rs`, neue `blocks/model.rs` und `blocks/shape.rs`,
    `meshing/plants.rs` → `meshing/models.rs`, `meshing/greedy.rs`
    (`should_emit_cube_face_local_fast`, `:698-729`), `player/mod.rs`,
    `world/raycast.rs`, `assets.rs`, `render/state.rs` (Outline, Item-Preview).
  - *Abhängigkeiten:* Block-State-Datenschicht (Orientierung), Registry-Makro
    (Modellreferenz pro Block). Blockiert Treppe, Zaun, Tür, Fackel, Leiter,
    Schiene, Schild.
  - *Priorität/Aufwand/Impact:* **P2**, L, sehr hoch (öffnet rund 60 % der
    Content-Liste).
  - *Akzeptanz:* Treppe in 8 Orientierungen, Zaun mit Verbindungen und
    1,5er-Kollision, Glasscheibe und Wandfackel rendern korrekt; die Outline
    folgt der Form; der Spieler kann nicht über Zäune springen; bestehende
    Vollwürfel-Meshes bleiben unverändert (gleiche Quad-Zahl im
    `greedy_ao_merge_report`).
- **visual pass still owed** on the skylight change: `blocks_skylight` is gone, replaced by `SkylightSpec` deriving from `OcclusionShape::boundary_coverage(Face::Down)` with two named exceptions (leaves block without occluding, glass is explicitly transparent to light). This fixed two real leaks — `grass_block` and `stone_slab` were full-coverage shapes that passed skylight straight through — so anything under grass or a slab is now correctly dark where it used to be lit. Terrain lighting changed everywhere; walk a cave mouth, an overhang and a slab roof before trusting it. Guarded by `blocks::tests::skylight_agrees_with_the_occlusion_shape_except_for_named_blocks` and `world::tests::opaque_surface_blocks_no_longer_leak_skylight_downwards`
- decide whether AO-aware greedy merging is worth its cost: implemented behind `MeshingPool::ao_aware_merge` and **measured** (`cargo test --release meshing::greedy::greedy_ao_merge_report -- --nocapture`) at **+36 % quads and +34 % meshing time** on hilly terrain. Default is off, because meshing is already the largest slice of a streaming frame. This is a visual-quality call, not a technical one — flip the flag, look at long walls and cave mouths, and decide
- settle the boundary-ocean aliasing question with the F12 toggle: the analysis is now recorded at `push_ocean_quad` (one texel per block, minification only past ~2000 blocks, and the boundary fog fades the ring out over the same span — the one open case is a camera sitting almost exactly at sea level, where the plane is edge-on). Toggle the ring off while an artifact is on screen; if it stays, the ring is not the cause
- consider GPU-driven/indirect submission only if a draw-call bottleneck is measured and proven. The numbers are now on screen (F3: terrain + LOD draw calls, visible chunks, CPU render ms) and per frame in `gpu.csv` next to `cpu_render_ms`. The gate: if render time does not track draw-call count, indirect submission buys nothing. Everything measured so far points at CPU meshing instead — `process_dirty_chunk_meshes` is ~60 % of a streaming frame
- **Section-Visibility-Graph ("Cave Culling") vor weiteren Occlusion-Ansätzen**
  (neu formuliert 2026-09-28; der bisherige Punkt "consider more advanced
  occlusion approaches only after measurement" geht hier auf, sein Mess-Gate
  bleibt)
  - *Ist:* Pro Frame wird die sichtbare Liste aus **allen** `chunk_meshes` neu
    gebaut (Radius-Test, `sphere_in_frustum`, Sortierung;
    `render/state.rs:19897-19959`), danach ein `draw_indexed` pro Sektion und
    Layer mit eigenem Vertex-/Index-Buffer-Bind (`:20406-20489`). Einzige
    Verdeckungsprüfung: eine vollsolide Sektion mit 6 vollsoliden Nachbarn wird
    gar nicht gemesht (`chunk_is_fully_occluded_full_solid`, `:15944-15956`).
    Keine Occlusion-Queries, kein Hi-Z, kein Sichtbarkeitsgraph. *Korrektur:*
    Die Frustum-Zähler heißen `frustum_culled_chunks_total` /
    `meshes_skipped_due_to_frustum` (`:5616-5617`), nicht
    `frustum_culling_stats`.
  - *Ziel:* Unter Tage und hinter Bergen nur Sektionen zeichnen, die vom
    Kamerastandort durch Luft/transparente Blöcke erreichbar sind — weniger
    Draw-Calls, weniger Overdraw, als Nebeneffekt eine natürliche
    Front-to-Back-Reihenfolge.
  - *Vorbild:* Vanilla `VisGraph` / Section-Occlusion-Graph (Sodium nutzt
    dieselbe Idee): beim Meshen pro Sektion ein Flood-Fill über die 4096
    Zellen, Ergebnis = 15-Bit-Menge "Fläche A ist mit Fläche B verbunden"; beim
    Rendern BFS von der Kamerasektion über verbundene Flächen, nur in
    Blickrichtung. Vollständig übertragbar, reine CPU-Arbeit auf dem
    Mesh-Worker.
  - *Vorgehen:* 1) Zuerst messen: F3 zeigt Terrain-Draw-Calls und sichtbare
    Chunks, `gpu.csv` enthält `cpu_render_ms`; eine Höhlen- und eine
    Oberflächen-Szene mit RD 12 und 16 aufnehmen. 2) `VisibilitySet(u16)` im
    Mesh-Worker berechnen und mit dem Mesh-Ergebnis zurückgeben (Opazität aus
    `OcclusionShape::boundary_coverage`). 3) Sichtbare Liste per BFS statt
    Vollscan bauen, Frustum-Test pro besuchter Sektion. 4) Ungemeshte Sektionen
    gelten als "alles verbunden".
  - *Dateien:* `meshing/mod.rs`, `render/mesh_worker.rs`, `render/state.rs`
    (Sichtliste), `camera.rs`.
  - *Abhängigkeiten:* keine; profitiert von Blockmodellen (Opazität pro
    Fläche). Vor "GPU-driven/indirect submission" angehen, weil es dieselbe
    Metrik billiger senkt.
  - *Priorität/Aufwand/Impact:* **P3**, M; bei RD 12-16 unter Tage hoch, an
    der Oberfläche gering.
  - *Akzeptanz:* In der Höhlen-Szene sinken sichtbare Sektionen und Draw-Calls
    um mindestens 50 %, ohne Löcher beim schnellen Drehen; `cpu_render_ms`
    sinkt oder bleibt gleich; die Oberflächen-Szene wird nicht langsamer.

- **Mesher-Korrektheit: echtes Licht für Flüssigkeiten/Pflanzen, Halo mit
  Kanten- und Ecknachbarn**
  - *Ist:* (a) Flüssigkeits- und Pflanzen-Quads schreiben immer
    `FULLBRIGHT_LIGHT_PACKED` und `shade: 255` (`meshing/greedy.rs:1844-1854`,
    `meshing/plants.rs:84-86`) — Wasser, Blumen und Pilze in Höhlen leuchten
    mit vollem Himmelslicht. (b) Der Mesher-Halo kopiert nur die 6
    Flächennachbarn; die 12 Kanten- und 8 Eckregionen bleiben Luft mit
    Default-Licht (`meshing/neighborhood.rs:298-301`; Snapshot mit
    `neighbors: [Option<ChunkSnapshot>; 6]`, `:9-20`, gebaut in
    `render/state.rs:15332-15358`). AO-Ecksamples an Sektionskanten sehen
    dadurch Luft statt des verdeckenden Blocks → hellere Linien entlang der
    Kanten. Passend dazu invalidiert ein Edit nie Diagonal-Sektionen
    (`classify_block_edit_mesh_invalidation`, `render/state.rs:15562-15655`).
  - *Ziel:* Alle Geometrie folgt demselben Licht-/AO-Modell; keine Artefakte an
    Sektionsgrenzen.
  - *Vorbild:* Vanilla `RenderChunkRegion` bzw. Sodium `WorldSlice` (der Mesher
    bekommt einen 3×3×3-Sektionsblock). Direkt übertragbar: die Snapshots sind
    `Arc`-Klone und kosten fast nichts.
  - *Vorgehen:* 1) Flüssigkeiten nehmen das Licht der Zelle vor der Fläche
    (wie `cube_face_vertex_shade_and_light_local_fast`), Lava behält ihr
    `emitted_light`; Pflanzen nehmen das Licht ihrer eigenen Zelle.
    2) Snapshot auf 26 Nachbarn erweitern, Halo-Kanten (12 × 16 Zellen) und
    Ecken (8 Zellen) kopieren. 3) Ein Edit an einer Sektionskante oder -ecke
    markiert auch die betroffenen Diagonal-Sektionen dirty, wenn sich deren
    AO-Samples ändern.
  - *Dateien:* `meshing/greedy.rs`, `meshing/liquids.rs`, `meshing/plants.rs`,
    `meshing/neighborhood.rs`, `render/state.rs`.
  - *Abhängigkeiten:* keine. Vor "Smooth Lighting" erledigen.
  - *Priorität/Aufwand/Impact:* **P1** (a: S, b: M), mittel, gut sichtbar.
  - *Akzeptanz:* Wasser in einer lichtlosen Höhle ist dunkel; eine Blocktreppe
    über eine Sektionsecke zeigt durchgehende AO ohne helle Kante; die
    Mesh-Zeit pro Job (`mesh_worker`-Trace) steigt um weniger als 5 %.

- **Smooth Lighting: Lichtverläufe über gemergte Quads — Entscheidung "Licht im
  Vertex" vs. "Licht-Volumen"**
  - *Ist:* `light_packed` wird mit `@interpolate(flat)` übergeben
    (`render/shaders.rs:41`), also ein Lichtwert pro Dreieck vom Provoking
    Vertex. Der Greedy-Merge vergleicht nur Block und Normalenrichtung
    (`MaskCell`, `meshing/greedy.rs:1172`, `:1182`); Licht fließt nicht in die
    Merge-Entscheidung. Ein langer Wandstreifen bekommt damit einen einzigen
    Lichtwert, und seine zwei Dreiecke können sichtbar verschieden hell sein.
    Heute fällt das kaum auf, weil fast nur Himmelslicht existiert; mit der
    Fackel (siehe "Blöcke") wird es sofort sichtbar. Zugleich erzwingt jede
    Lichtänderung einen Remesh (siehe Performance "Lighting-bedingte
    Remeshes").
  - *Ziel:* Sanfte Lichtverläufe (Vanilla "Smooth Lighting"), ohne den
    Greedy-Gewinn aufzugeben; idealerweise Lichtänderungen ohne Remesh.
  - *Vorbild:* Option A — Vanilla/Sodium: Licht pro Vertex als Mittel der 4
    angrenzenden Zellen, Merge nur bei gleichen Eck-Lichtwerten (analog zu
    `ao_aware_merge`, dort gemessen +36 % Quads). Option B — eigener Ansatz,
    nicht aus Vanilla: Licht als 3D-Textur bzw. Storage-Buffer pro Sektion auf
    der GPU; der Fragment-Shader sampelt an `world_pos + normal * 0.5`,
    bilinear in der Flächenebene. Das Mesh enthält dann kein Licht mehr, ein
    Lichtupdate ist ein 4-KiB-Upload statt eines Remeshes.
  - *Vorgehen:* 1) Zuerst den Anteil der Remeshes mit Grund `lighting` messen
    (Performance-Punkt unten). 2) Prototyp B auf einem Branch:
    `R8Uint`-Volumen 18³ pro Sektion in der Arena aus "Chunk-Mesh-GPU-
    Suballokation", Shader-Sampling, AO bleibt im Vertex. 3) A gegen B
    vergleichen: Quad-Anzahl, Mesh-Zeit, Remeshes/s beim Streamen,
    GPU-Frametime. 4) Entscheidung dokumentieren, die Verliererlösung löschen.
  - *Dateien:* `render/shaders.rs`, `meshing/greedy.rs`, `meshing/mod.rs`
    (`WorldVertex`), `render/mesh_gpu.rs`, `render/state.rs`.
  - *Abhängigkeiten:* "Mesher-Korrektheit" (Halo); spätestens mit der Fackel;
    Option B hängt an "Chunk-Mesh-GPU-Suballokation".
  - *Priorität/Aufwand/Impact:* **P2**, L, hoch (Optik; bei B zusätzlich
    deutlich weniger Remeshes).
  - *Akzeptanz:* Eine Fackel an einer langen Wand zeigt einen stufenlosen
    Lichtkegel ohne Dreiecksnaht; bei B fallen Remeshes mit Grund `lighting`
    beim Streamen auf nahe 0.

- **Transluzente Geometrie innerhalb einer Sektion sortieren**
  - *Ist:* Transparente Sektionen werden nur als Ganzes back-to-front nach
    Sektionsmittelpunkt gezeichnet (`render/state.rs:20471-20474`); innerhalb
    einer Sektion gibt es keine Sortierung (kein `sort` in `src/meshing`), und
    Depth-Write ist aus (`:9748-9757`). Überlappende Wasser- und Eisflächen
    derselben Sektion mischen in Mesh-Reihenfolge — sichtbar, sobald man durch
    zwei Wasseroberflächen oder durch Eis auf Wasser schaut. Mit gefärbtem Glas
    wird das zum Regelfall.
  - *Ziel:* Korrekte Überblendung transparenter Flächen aus jeder
    Blickrichtung.
  - *Vorbild:* Sodium "Translucency Sorting": Quads pro Sektion mit Mittelpunkt
    ablegen, den Indexpuffer auf dem Worker neu sortieren, sobald sich die
    Kamera relativ zur Sektion über einen Schwellwert bewegt hat; Sektionen mit
    nur koplanaren Flächen (flache Wasseroberfläche) brauchen nie ein Resort.
    Ohne Einschränkung übertragbar.
  - *Vorgehen:* 1) Transparente Quads mit Zentrum im Mesh-Ergebnis ablegen.
    2) Sort-Job-Typ im Mesh-Worker (nur Indizes), ausgelöst bei einer
    Kamerabewegung von mehr als 1 Block relativ zur Sektion und nur für nahe
    Sektionen (≤ 4 Chunks). 3) Beim Meshen klassifizieren, ob eine Sektion nie
    sortiert werden muss.
  - *Dateien:* `meshing/greedy.rs`, `meshing/liquids.rs`,
    `render/mesh_worker.rs`, `render/state.rs`.
  - *Abhängigkeiten:* keine; einfacher nach dem geteilten Quad-Indexpuffer.
  - *Priorität/Aufwand/Impact:* **P3**, M, mittel (Optik); steigt mit
    gefärbtem Glas.
  - *Akzeptanz:* Blick durch zwei gestaffelte Wasseroberflächen und durch Eis
    auf Wasser ist aus 8 Richtungen fehlerfrei; Sort-Jobs brauchen beim
    Herumfliegen weniger als 5 % der Mesh-Worker-Zeit.

- **Mipmaps für das Block-Textur-Array**
  - *Ist:* Das Textur-Array hat `mip_level_count: 1` und filtert durchgehend
    Nearest (`texture.rs:128`, `:170-172`, `:183`). Ab mittlerer Distanz
    flimmert Terrain (Moiré auf Gras und Sand), und jeder Zugriff liest die
    volle Auflösung — ungünstig für den Textur-Cache bei RD 12-16.
  - *Ziel:* Ruhiges Bild auf Distanz, bessere Cache-Trefferrate.
  - *Vorbild:* Vanilla (Mipmap-Stufen 0-4, alpha-bewusstes Downsampling für
    Cutout-Texturen). Übertragung: Mip-Kette beim Laden auf der CPU erzeugen
    (Box-Filter; für Cutout-Texturen die Alpha-Coverage erhalten),
    `mipmap_filter` im Sampler.
  - *Vorgehen:* 1) Mip-Generierung in `from_rgba8_layers` (`texture.rs:90`),
    Stufenzahl aus der Kachelgröße. 2) Sampler mit `mipmap_filter`.
    3) Cutout-Layer (Laub, Pflanzen): Alpha pro Stufe so skalieren, dass die
    Coverage gegen den `discard`-Schwellwert 0,5 (`render/shaders.rs:72-73`)
    gleich bleibt. 4) Option "Mipmap Levels" in `settings.rs`. 5) Animierte
    Texturen (`render/texture_anim.rs`) mitziehen.
  - *Dateien:* `texture.rs`, `render/state.rs` (Array-Aufbau `:9317-9360`),
    `render/texture_anim.rs`, `settings.rs`.
  - *Abhängigkeiten:* keine.
  - *Priorität/Aufwand/Impact:* **P3**, S-M, mittel (Optik, leichte
    GPU-Entlastung).
  - *Akzeptanz:* Kein Moiré auf einer Grasebene bei RD 16
    (Screenshot-Vergleich); Laub dünnt auf Distanz nicht sichtbar aus;
    GPU-Frametime in `gpu.csv` gleich oder besser.

- **Innenflächen gleichartiger transparenter Blöcke cullen (Glas, Eis),
  optional Laub ("More Culling")**
  - *Ist:* Laub, Glas und Eis haben `face_occluding: false`
    (`blocks/mod.rs:1054`, `:1073`, `:1725`) und damit
    `OcclusionShape::Decorative` ohne Flächenabdeckung (`:352-356`); der
    Same-Cull-Group-Zweig in `should_emit_cube_face_local_fast`
    (`meshing/greedy.rs:713-722`) greift deshalb nie. Zwischen zwei Glasblöcken
    entstehen beide Innenflächen (sichtbare Innenkanten in Glaswänden),
    Laubkronen erzeugen alle Innenflächen. Eine Fast/Fancy-Einstellung gibt es
    nicht.
  - *Ziel:* Glas und Eis wie in Vanilla (Fläche zum gleichen Block entfällt);
    Laub wahlweise "fancy" (heute) oder "culled".
  - *Vorbild:* Vanilla `HalfTransparentBlock.skipRendering` (gleicher Block →
    Fläche weg); More Culling "Leaves Culling" (Laubflächen entfallen, wenn der
    Nachbar Laub ist). Eine Regel im Face-Culling, direkt übertragbar.
  - *Vorgehen:* 1) Regel "gleicher Block und gleiche `cull_group` → Fläche weg"
    für Glas und Eis. 2) Option `leaves_culling: Fancy | Culled` in
    `settings.rs`. 3) Quads vorher/nachher mit dem Greedy-Report auf einer
    Wald-Szene zählen.
  - *Dateien:* `meshing/greedy.rs`, `blocks/mod.rs`, `settings.rs`.
  - *Abhängigkeiten:* keine; bei Blockmodellen in die `cullface`-Logik
    übernehmen.
  - *Priorität/Aufwand/Impact:* **P3**, S, niedrig bis mittel.
  - *Akzeptanz:* Glaswand ohne Innenkanten; die Wald-Szene hat mit "Culled"
    messbar weniger Quads bei gleicher Silhouette.

- **Kompaktes Terrain-Vertex-Format** (Ausblick, nur nach GPU-Messung)
  - *Ist:* `WorldVertex` hat 24 Byte (`meshing/mod.rs:41-51`): Position als
    `f32 × 3` in Weltkoordinaten (Chunk-Offset eingebacken), UV `u16 × 2`,
    `tex_layer: u32`, je ein `u8` für Shade, Tint, Licht und Padding. Indizes
    sind durchgehend `u32` (`meshing/mod.rs:81`, `:90`).
  - *Ziel:* 16 Byte pro Vertex (−33 % Vertex-Bandbreite und VRAM).
  - *Vorbild:* Sodium "compact vertex format": Position als `u16 × 3`
    Fixed-Point relativ zum Sektionsursprung, den Ursprung liefern
    Instanz-/Storage-Daten pro Draw. Übertragbar; wgpu unterstützt die nötigen
    `Uint16`/`Unorm16`-Attribute.
  - *Vorgehen:* 1) Nur angehen, wenn `gpu.csv` zeigt, dass GPU-Zeit (nicht
    CPU-Zeit) den Frame begrenzt. 2) Position relativ + Sektionsursprung per
    Storage-Buffer-Index; `tex_layer` als `u16`. 3) Mit dem geteilten
    Quad-Indexpuffer kombinieren.
  - *Dateien:* `meshing/mod.rs`, `render/mesh_gpu.rs`, `render/shaders.rs`,
    alle Mesher.
  - *Abhängigkeiten:* "Chunk-Mesh-GPU-Suballokation" (Performance).
  - *Priorität/Aufwand/Impact:* **P4**, M, niedrig auf dem Zielsystem (alle
    bisherigen Messungen zeigen CPU-Grenzen, keine GPU-Grenzen).
  - *Akzeptanz:* VRAM für Terrain-Meshes −30 %, GPU-Frametime gleich oder
    besser, keine Präzisionsartefakte.

## Content-Abgleich gegen Alpha v1.0.17_04

Quelle: `docs/a1.0.17_04.md`. Aufgenommen sind nur Punkte, die im Code **fehlen**
oder gegenüber dem heutigen Stand **erweitert** werden müssen; alles bereits
Implementierte (Chunk-/Region-Storage, Terrain+Carver+Erz-Pässe, Bäume, Blumen,
Pilze, Winterwelten, Wasser/Lava-Fluid-Sim, Tag/Nacht, Third-Person-Modell,
Niederschlag, Screenshots, Menüs/Options/Settings, Ticking-Areas) steht nicht
mehr hier. Multiplayer/SMP (Abschnitt 6 der Featureliste) ist explizit
ausgeklammert.

**Grundregel bei Widerspruch: das vorhandene Feature gewinnt.** Wo der Code
über a1.0.17_04 hinaus ist, ist das ein bewusster Vorgriff auf eine spätere
Version — nichts davon wird zurückgebaut, um die Vorlage exakt zu treffen. Die
Featureliste liefert den *Inhalt*, der noch fehlt, nicht die Obergrenze.
Betroffen sind heute: Weltgrenzen **0..255** statt 128 (`WorldConfig::min_y/
max_y`, `world/config.rs:45-48`), **Sprint/Sneak** (`InputAction::Sprint`/
`Sneak`, in der Vorlage erst ab Alpha 1.2.0), `Player::food_level`
(`player/mod.rs:52`, gated heute `can_sprint_swim`, `:100`), die sechs
Baum-Presets statt zwei Eichenformen, und `WorldEnvironment` als expliziter
Weltparameter statt einer reinen Winter-Zufallsflagge.

### Querschnitts-Voraussetzung: Block-States

*Neu formuliert 2026-09-28.* Befund und Kernvorschlag vom Stand 2026-09-01
("`block_data` per Blocktyp umdeuten — kein neuer Speicher, keine
Formatänderung") bleiben; ergänzt um die Architekturfragen, die er offen ließ:
typisierter State-Wert, abgeleitete vs. gespeicherte Properties,
Waterlogging, Grenze von 256 Blocktypen.

**Das ist der Blocker für ca. 60 % der Liste unten** (Treppen, Zaun, Tür,
Leiter, Fackel, Schiene, Hebel, Knopf, Druckplatte, Redstone-Staub, Schild,
Kaktus/Zuckerrohr-Alter, Schneehöhe). Die Form-/Occlusion-Seite steht unter
"Rendering / Blockmodelle und Voxel-Shapes"; hier steht die Speicher- und
Contract-Seite, weil daran alle Content-Punkte hängen.

- *Ist:*
  - `BlockStateId` ist ein reiner Alias auf `BlockId` (`blocks/mod.rs:80`),
    trägt also keinen Zustand; `RandomTickFn` bekommt ihn trotzdem schon als
    Parameter (`:81`).
  - `Chunk::block_data` ist bereits **ein Byte pro Voxel** als
    `Arc<Vec<u8>>` (`world/chunk.rs:10`), Copy-on-Write über `Arc::make_mut`
    (`:163-172`), für unveränderte Sektionen ein geteilter Null-Puffer
    (`:455-462`). Es wird mitgespeichert (`storage/chunk_codec.rs`,
    `COLUMN_PAYLOAD_FORMAT_VERSION = 2`, eigene u8-Palette `:120-127`) **und
    liegt dem Mesher im gepolsterten Nachbarschaftspuffer vor**
    (`meshing/neighborhood.rs:11`, `:170`).
  - Belegung heute: Fluids nutzen `0x0F` (Level), `0x20`, `0x40`, `0x80`
    (`world/fluids.rs:174-177`), **nur für Fluid-Blöcke**; Bit `0x10` ist
    frei. Der Setzling nutzt Bit 0 als Wachstumsstufe
    (`sapling_stage`/`sapling_set_stage`, `blocks/mod.rs:2495-2501`) — der
    erste Nicht-Fluid-Nutzer und der Beleg, dass die Umdeutung pro Blocktyp
    funktioniert. Es gibt aber keine typisierte Sicht und keine zentrale
    Stelle, die festhält, welcher Block welche Bits besitzt
    (`blocks/state.rs` existiert nicht).
  - Kein Setter schreibt einen State mit Nebenwirkungen: es gibt
    `set_block_and_data` / `set_block_and_data_loaded_only`
    (`world/mod.rs:3223`, `:3279`), aber keine `set_block_state`-Variante, und
    Platzieren setzt für Nicht-Fluids immer `0` (`render/state.rs:19316-19325`).
  - Die Ablehnung "`blocks` + `block_data` zu einer `u16`-State-ID
    verschmelzen" (`world/chunk.rs:328-330`: spart nichts, ~150 Call-Sites)
    gilt weiter für die **Speicherung**, aber nicht für die **API** — dort
    fehlt ein Wert, der Block und Daten gemeinsam trägt.
- *Ziel:* Jeder Block deklariert seine Properties einmal; alle Systeme
  (Mesher, Shapes, Interaktion, Speicher, Debug-Anzeige) lesen sie über einen
  typisierten `BlockState`-Wert. Die Speicherform (heute `BlockId` + Byte)
  bleibt ein Detail von `Chunk` und kann später ohne API-Bruch wechseln.
- *Vorbild:* Vanilla `StateDefinition`/`Property<T>`; FerriteCore
  (Property-Tabellen pro Block statt pro State, keine State-Objekte pro Voxel).
  Übertragung ohne Objektgraph: ein `Copy`-Wert `BlockState { id: BlockId,
  data: u8 }` plus pro Block ein statisches Schema `&'static [PropertySpec]`
  (Name, Bit-Offset, Bit-Breite, Werteliste). Das Byte reicht für alle
  Vanilla-Formen, **wenn verbindungsabhängige Properties beim Meshen
  abgeleitet statt gespeichert werden** (Treppen-`shape`, Zaun-/Scheiben-/
  Mauer-Verbindungen, Redstone-Seiten, Notenblock-Instrument). Gespeichert
  wird nur, was der Spieler setzt oder was sich über die Zeit ändert: Tür =
  `facing` (2) + `half` (1) + `hinge` (1) + `open` (1) + `powered` (1) = 6 Bit;
  Treppe = `facing` (2) + `half` (1) + `waterlogged` (1) = 4 Bit; Schiene = 10
  Formen (4 Bit) + `powered` (1); Redstone-Staub = `power` (4); Alter/Stufen
  ≤ 4 Bit.
- *Vorgehen:*
  1. `blocks/state.rs`: `BlockState { id, data }`, `PropertySpec` und
     typisierte Sichten `Facing4`/`Facing6` (2-3 Bit), `Half` (1 Bit,
     Slab/Treppe/Tür oben/unten), `Open`/`Powered` (je 1 Bit), `RailShape`
     (4 Bit), `RedstonePower` (4 Bit), `Age { max }` (4 Bit für
     Kaktus/Zuckerrohr/Setzling), `SnowLayers` (3 Bit). Jeweils
     `fn get(BlockState) -> T` / `fn with(BlockState, T) -> BlockState` analog
     zu den `fluid_data_*`-Helfern, damit der Stil konsistent bleibt.
     Setzling-Bit und Fluid-Bits als erste Schemata eintragen.
  2. `BlockSpec` bekommt `properties: &'static [PropertySpec]` (über das
     Registry-Makro aus "Architektur"). Damit steht pro Block an genau einer
     Stelle, wer die Bits besitzt. Ein Test in der Art von
     `every_model_states_its_raycast_shape` erzwingt: Bit-Bereiche pro Block
     sind disjunkt und passen in 8 Bit, Fluid-Blöcke deklarieren genau die
     Fluid-Bits.
  3. Den `BlockStateId`-Alias durch `BlockState` ersetzen, zuerst in den
     Funktionszeiger-Signaturen (`RandomTickFn`, künftige Hooks) und in den
     Shape-/Modell-Abfragen (`collision_shape`, `raycast_shape`,
     `occlusion_shape` nehmen einen State statt einer ID).
  4. `World::set_block_state(pos, BlockState)` als Schreibweg, der über das
     Welt-Änderungsjournal ("Architektur") dieselbe Dirty-/Lighting-
     Invalidierung auslöst; `set_block` bleibt als Kurzform mit dem
     Default-State. Der bestehende Fluid-Pfad zeigt, dass das geht.
  5. **Waterlogging:** ein gespeichertes Bit `waterlogged` in jedem Block, der
     es unterstützt; der Fluid-Pfad liest es als "Quelle, still". Keine zweite
     Fluid-Schicht — Vanilla erlaubt in gefluteten Blöcken auch nur Quellen.
  6. Speicherformat bleibt v2 (Blockname + Daten-Byte). Palette-Namen der Form
     `name[prop=wert,…]` nur, wenn ein späterer Umbau das erzwingt (siehe
     "Storage: Versionierung & Migrationskette").
  7. Neue `BlockModelSpec`-Varianten (`Stairs`, `Fence`, `Torch`, `Ladder`,
     `Door`, `Rail`, `FlatOnFloor`, `Sign`) entstehen pro Formfamilie unter
     "Rendering / Blockmodelle und Voxel-Shapes"; Nicht-Würfel laufen dort über
     den Modell-Pfad, den der Greedy-Merger nie sieht.
- *Dateien:* neues `blocks/state.rs`, `blocks/mod.rs`, `world/mod.rs`,
  `world/fluids.rs`, `render/state.rs` (Platzieren), `meshing/*`.
- *Abhängigkeiten:* Registry-Makro (Schema-Deklaration), Welt-Änderungsjournal
  (Nebenwirkungen). Blockiert Blockmodelle, Interaktion und Redstone.
- *Priorität/Aufwand/Impact:* **P2**; Schritte 1-4 M, danach pro Formfamilie
  S; Impact sehr hoch.
- *Akzeptanz:* Schema-Test grün; Setzling und Fluids laufen über die neuen
  Sichten ohne Verhaltensänderung (bestehende Fluid-Tests grün); das
  F3-Overlay zeigt für den anvisierten Block `name[prop=wert]`; ein Save von
  vorher lädt bitgleich.

Reihenfolge: erst 1+2 (reine Datenschicht, testbar ohne Rendering), dann 3+4,
dann die Blockmodelle pro Formfamilie. Jede Formfamilie ist danach ein
isolierter, kleiner Change.

### Blöcke (Inhalt)

Heute registriert: 51 IDs (`BlockId`, `blocks/mod.rs:85-137`). Fehlend gegenüber
der Vorlage. Der Blocktextur-Bestand deckt fast alles ab
(`assets/minecraft/textures/block/`, 2113 Dateien inkl. Normal-/Specular-
Varianten): `torch`, `ladder`, `clay`, `cactus_*`, `sugar_cane`, `diamond_ore`,
`redstone_ore`, `lapis_ore`, `crafting_table_*`, `furnace_*`, `jukebox_*`,
`note_block`, `spawner`, `rail`, `lever`, `redstone_dust_*`, `oak_door_*` sind
alle vorhanden — für diese Blöcke fehlt also nur die `BlockSpec`-Zeile. Die
**Kiste** hat im modernen Layout keine Blocktextur, sondern eine Entity-Textur;
`textures/entity/chest/` liegt inzwischen vor (`normal`, `normal_left`,
`normal_right`, `trapped*`, `ender`), das Kistenmodell muss also als
Entity-artiges Modell mit eigener Textur gebaut werden, nicht als Würfel aus dem
Blockatlas. **Zaun** und **Knopf** haben keine eigene Textur und leihen sich
Planken bzw. Stein — beim Modellbau bewusst so umsetzen.

**Ohne neue Blockform (billig, reine Tabellen-Einträge in `BLOCK_SPECS`
(`blocks/mod.rs:700`) plus `CLASSIC_PALETTE_ORDER` (`:139`)):** beides sind
Arrays fester Länge (`[BlockSpec; 51]`, `[BlockId; 43]`), die Zahl im Typ muss
also je Block mitwachsen.

- Diamanterz, Redstone-Erz, Diamantblock, Lapis-Erz. Erz-Seite: neue Einträge in
  den vier Tabellen in `worldgen/ores.rs` (`FLATISH_/CLASSIC_FLATTER_/
  RD161348_LIKE_/HILLY_ORE_TABLE`, `ores.rs:183-216`) — der Pass ist bereits
  datengetrieben, es kommen nur Zeilen dazu (auch hier feste Arraylänge,
  heute `[OreFeatureConfig; 3]`). Achtung: der Test
  `ore_density_stays_within_tolerance_across_presets` (Toleranz 1.6x) gilt auch
  für neue Erze, die Höhenbänder müssen also pro Preset an der Terrain-Hülle
  ausgerichtet werden — genau wie bei Kohle beschrieben in `todos_worldgen.md`.
- Tonblock (Clay), Schneeblock (der heutige `BlockId::Snow` ist die 2px-**Decke**,
  nicht der Vollblock), Werkbank, Ofen (an/aus als zwei Texturen-Sätze),
  Notenblock, Jukebox, Kiste.
- Redstone-Erz mit Leucht-Zustand: `emitted_light` ist in `BlockSpec` schon da
  (heute nur Lava = 15, `blocks/mod.rs:912,936`), also nur ein zweiter Spec.

**Mit neuer Blockform (hängt an der Block-State-Voraussetzung oben):**

- Fackel (Wand/Boden, `emitted_light: 14`) — der wichtigste Einzelblock der
  Liste, weil ohne ihn Beleuchtung reines Skylight bleibt und der komplette
  Block-Light-Pfad im Spiel nie benutzt wird.
- Treppe (Bruchstein/Holz), Stufe oben/unten (`SlabUpper` ist in
  `OcclusionShape` bereits vorgesehen, `blocks/mod.rs:363` — es fehlt nur der
  Block dazu), Zaun (4 Verbindungsbits, 1.5 Blöcke Kollisionshöhe → braucht
  eine `CollisionShape::Aabb` höher als der Voxel, prüfen ob
  `move_with_collisions` das trägt), Leiter, Holz-/Eisentür (2 Blöcke hoch,
  `Half`+`Facing4`+`Open`), Schild (Boden/Wand + Textinhalt → Block-Entity),
  Kaktus (Kollisionsschaden, nur auf Sand), Zuckerrohr, Schneedecke mit
  Höhenstufen 1-8.

**Verhalten für schon vorhandene Blöcke:**

- **Kies fällt wie Sand; Fallen in das Block-Update-System umziehen**
  (neu formuliert 2026-09-28 — der alte Befund "Sand/Kies fallen … heute ohne
  jede Logik" stimmt für Sand nicht mehr)
  - *Ist:* Sand fällt bereits: jeder Edit stößt einen gebudgeteten Check an
    (`enqueue_pending_sand_gravity_check`, `render/state.rs:17470-17482`,
    ausgelöst `:17578`); `try_drop_sand_block` (`:17862-17914`) setzt den Block
    **sofort** auf die Landeposition (erster `is_solid()`-Block darunter), ohne
    Fall-Entity, mit Fluid-Notify, Dirty- und Licht-Reparatur für beide Zellen.
    Die Bedingung ist fest auf `BlockId::Sand` verdrahtet (`:17863`) — Kies
    fällt nicht. Der Pfad lebt in `State`, nicht in der Welt-/Sim-Schicht, und
    nutzt weder den Random-Tick-Pfad noch einen generischen Nachbar-Trigger
    (den es nicht gibt).
  - *Ziel:* Sand und Kies (später z. B. Beton-Pulver) fallen über eine
    Blockeigenschaft; mit dem Entity-System optional als sichtbare Fall-Entity.
  - *Vorbild:* Vanilla `FallingBlock` (Scheduled Tick nach einem
    Nachbar-Update, dann `FallingBlockEntity`). Der Sofort-Teleport ist eine
    bewusste Zwischenstufe ohne Entity: optisch schlechter als eine
    Fallanimation, aber ohne Entity-System machbar.
  - *Vorgehen:* 1) Sofort: `falls: bool` in `BlockSpec`, Bedingung in
    `try_drop_sand_block` darauf umstellen, Kies setzen. 2) Mit
    "Architektur / Block-Verhalten: Neighbor-Updates und Scheduled Ticks" den
    Check dorthin verschieben (2 Ticks Verzögerung wie in Vanilla). 3) Mit dem
    Entity-System als Fall-Entity rendern (gehört an den Entity-Punkt unten).
  - *Dateien:* `blocks/mod.rs`, `render/state.rs` (`:17470-17536`,
    `:17862-17914`).
  - *Abhängigkeiten:* Schritt 1 keine; Schritt 2 Block-Update-System;
    Schritt 3 Entity-Fundament.
  - *Priorität/Aufwand/Impact:* **P2** (Schritt 1: S), niedrig bis mittel.
  - *Akzeptanz:* Kies unter einem entfernten Block fällt; das Sand-Verhalten
    bleibt unverändert.
- TNT: existiert nur als Deko-Block (kein einziger Treffer außerhalb von
  `blocks/mod.rs`). Braucht Zündung + Explosionsradius. Die Explosion selbst ist
  ein Batch-`set_block` über eine Kugel plus Raycast-Dämpfung; wichtig für die
  Performance: **einmal** dirty markieren pro betroffenem Chunk, nicht pro Block
  — der bestehende Block-Edit-Pfad in `render/state.rs` markiert pro Edit.
  *Korrektur 2026-09-28:* Dirty-Marken werden inzwischen pro Chunk koalesziert
  (`pending_block_edit_dirty_chunks`, `render/state.rs:5473`, Flush
  `:16368-16402`); pro Position laufen aber weiterhin Lighting-Klassifikation,
  Fluid-Notify, Sand-Check und Partikel (`:17565-17590`). Für eine Explosion
  braucht es die Batch-Variante aus "Architektur / Welt-Änderungsjournal",
  Schritt 4.
- Schwamm: existiert nur als Deko-Block. Wasser im 5×5×5 entfernen beim Setzen;
  das ist ein einzelner Aufruf gegen `FluidSystem::enqueue_with_neighbors`.
  *Korrektur 2026-09-28:* die Funktion existiert (`world/fluids.rs:556`), ist
  aber derzeit ungenutzt (Compiler-Warnung `never used`) — beim Clippy-Aufräumen
  nicht löschen.
- Obsidian ist vorhanden, muss aber explosionsresistent sein, sobald TNT lebt.

### Items, Inventar & Crafting

Heute gibt es **kein Item-System**: das Inventar ist eine Kreativ-Palette über
`BlockId` (`GameState::Inventory { carried_block: Option<BlockId> }`,
`app/mod.rs:224-232`). Es gibt keine `ItemId`, keinen `ItemStack`, keine
Stackgrößen, keine Haltbarkeit.

Das ist der größte Einzelblock der Liste und Voraussetzung für Crafting,
Smelting, Werkzeuge, Rüstung, Nahrung, Kisten und Drops.

Effiziente Reihenfolge:

1. `src/items/mod.rs` als Spiegel von `blocks/mod.rs`: `ItemId` als `#[repr(u16)]`
   Enum plus `ITEM_SPECS`-Tabelle (`key`, `display_name`, `texture`,
   `max_stack`, `durability: Option<u16>`, `tool: Option<ToolSpec>`,
   `food: Option<u8>`). Genau dieselbe Bauform wie `BlockSpec` — dann greift die
   vorhandene Registry-/NamespacedKey-Infrastruktur ohne Anpassung.
2. Block↔Item-Brücke: `fn item_for_block(BlockId) -> Option<ItemId>` und
   `fn block_for_item(ItemId) -> Option<BlockId>`. Beides `const fn` über
   dieselbe Tabelle, damit der Platzierungspfad in `render/state.rs` nur an
   einer Stelle umgestellt werden muss.
3. `ItemStack { item: ItemId, count: u8, damage: u16 }` und ein
   `Inventory { slots: [Option<ItemStack>; 36], armor: [Option<ItemStack>; 4], … }`
   in `src/player/inventory.rs`.
4. Erst danach die UI umbauen: der `GameState::Inventory`-Zweig behält seine
   Drag-/Carry-Mechanik (`press_origin`, `carried_block`), tauscht aber
   `BlockId` gegen `ItemStack`. Der 3D-Preview-Cache für Blöcke bleibt, für
   flache Items kommt ein Sprite-Pfad daneben — `ItemPreviewDraw`
   (`render/state.rs:3004`) ist der Anknüpfungspunkt.

**Assets: erledigt bis auf einen kleinen Rest.**
`assets/minecraft/textures/item/` enthält jetzt 462 Dateien, dazu
`textures/entity/chest/` und `textures/entity/signs/`. Werkzeuge, Rüstung,
Barren, Nahrung, Eimer, Bogen/Pfeil, Musikplatten, Buch/Papier, `slime_ball`,
`clay_ball`, `brick` und `wheat_seeds` sind da. Noch offen und vor der
Item-UI zu beschaffen: `feather`, `wheat`, `bowl`, `saddle`, `snowball` sowie
15 der 16 Farbstoffe (nur `yellow_dye.png` liegt vor) — für a1.0.17 werden
davon 8 gebraucht.

Inhalt danach, in der Reihenfolge der Abhängigkeit:

- Werkzeugstufen Holz/Stein/Eisen/Gold/Diamant × Spitzhacke/Axt/Schaufel/Hacke/
  Schwert; Diamant 1025 Uses.
- Werkzeug-Effizienz und Haltbarkeit als *Daten* (`tool_class`, `tier`,
  `durability`) gehören in die `ITEM_SPECS`-Tabelle; die *Wirkung* (Abbauzeit,
  Verschleiß) steht unter "Survival-Gameplay", weil sie nur dort greift.
- Drops beim Abbau (inkl. Setzling 1/20 aus Laub, Apfel aus Laub,
  Ton→4 Tonbällchen, Kies→Feuerstein-Chance). Ohne Entity-System zunächst
  direkt ins Inventar; die Item-Entity kommt am Entity-Punkt.
- Crafting: 2×2 im Inventar, 3×3 an der Werkbank. Rezepte als deklarative
  Tabelle (shaped/shapeless) in `src/items/recipes.rs`; Matching über eine
  normalisierte 3×3-Maske, damit shaped-Rezepte verschiebungsinvariant sind.
- Smelting: Ofen als Block-Entity mit `input`/`fuel`/`output` und Brennwerten
  pro Item. Der Ofen-Stub existiert bereits als Registrierung
  (`CORE_FURNACE_STUB_BLOCK_ENTITY_KEY`, `blocks/mod.rs:66`) — dort andocken.
  *Korrektur 2026-09-28:* der Stub hängt heute testweise an **Obsidian**
  (`blocks/mod.rs:979`) und zählt nur einen Zähler hoch
  (`world/block_entity_types.rs:58-89`); beim Anlegen des Ofens umhängen.
- Rüstung (Leder/Eisen/Gold/Diamant, 4 Slots), Bogen + Pfeile.
- Nahrung: Apfel, Brot, rohes/gebratenes Schwein, Pilzsuppe. In dieser Ära heilt
  Essen **sofort** und es gibt keine Hungerleiste. `Player::food_level`
  (`player/mod.rs:52`) bleibt trotzdem stehen — es ist heute die Bedingung für
  `can_sprint_swim` (`:100`) und damit ein vorhandenes Feature, kein toter Rest.
  Essen setzt es auf 20 zurück und heilt zusätzlich sofort; die Hungerleiste als
  *UI* kommt erst mit der Version, in der sie hingehört.
- Eimer (Wasser/Lava/Milch): setzt/entfernt Quellblöcke über den vorhandenen
  `FluidSystem`-Pfad; Milch braucht die Kuh.
- Restliche Items: Schüssel, Feuerzeug, Sattel, Musikplatten (13, Cat),
  Schneeball (werfbar, kein Schaden), Ei (werfbar, kein Spawn), Farbe (8),
  Papier, Buch, Slimeball, Ziegel, Weizen+Samen (nur als Drop/Item, **keine**
  Farmpflanze in dieser Version), Wolle (8 Farben als Item — die 16
  Wollblöcke sind bereits da).

### Entities & Mobs

*Neu formuliert 2026-09-28:* Inhalt und Arbeitsschritte vom Stand 2026-09-01
bleiben vollständig erhalten; ergänzt um Tick/Interpolation, Generations-IDs,
KI-Goals, Pathfinding, Spawning-Regeln und Rendering, korrigiert beim
Player-Modell.

- *Ist:* Es gibt **keine Entities außer dem Spieler und Block-Entities**: kein
  Entity-Register, keine Bewegungs-/AI-Systeme, kein Spawning (Suche nach
  `EntityId|mob|entities:` leer; nur das Flag `TickingFlags::TICK_ENTITIES`,
  `world/ticking_areas.rs:20`). Voraussetzungen, die im Code schon stehen und
  benutzt werden sollten:
  - `SimSystem` (`sim/mod.rs:15-42`) mit `fixed_tick`,
    `on_columns_entered/exited` und `ActiveSimColumns` — genau der richtige
    Aufhänger. Registrierung über `State::register_sim_system`
    (`render/state.rs:14522`); `FluidsSystem`, `BlockEntitiesSystem` und
    `RandomTicksSystem` sind die Vorlagen.
  - Kollision/Bewegung sind **nicht** an den Spieler gebunden:
    `move_with_collisions_for_height` (`player/mod.rs:215-251`),
    `move_character_with_conditional_step_up_with_height` (`:357-381`) und
    `snap_to_ground_with_height` (`:401-417`) nehmen eine freie Höhe entgegen;
    Mob-Bewegung kann dieselben Funktionen benutzen. *Korrektur:*
    `move_character_with_step_up` (`:332-355`) ist nur `#[cfg(test)]`.
    Kollidiert wird gegen die `CollisionShape` pro Block (`aabb_collides`,
    `:135-163`), nicht nur gegen Vollvoxel.
  - *Korrektur:* `build_player_model_mesh` (`render/player_model.rs:168`) ist
    **kein** generischer Box-Modell-Builder, sondern verdrahtet sechs
    Steve-Boxen mit 64×64-Skin-Layout (`:175-260`). Generisch sind nur die
    privaten Helfer `ModelBox` (`:157`), `append_box` (`:293`) und
    `rotate_local` (`:395`) — diese verallgemeinern statt neu zu schreiben. Das
    Mesh wird jeden Frame in Weltkoordinaten neu gebaut und hochgeladen
    (`render/state.rs:20221-20229`).
  - Der Lichtwert liegt pro Voxel vor (`Chunk::light`, gepackte Nibbles,
    `world/chunk.rs:11`) — die Spawn-Prüfung ist ein direkter Lookup, kein
    neuer Pass.
- *Ziel:* Passive und feindliche Mobs, Drop-Items, Projektile und Fahrzeuge auf
  einem gemeinsamen Fundament, das mit 100 Entities in RD 12-16 unter 1 ms
  Tick-Zeit bleibt und flüssig gerendert wird.
- *Vorbild:* Vanilla `Entity`/`Mob` mit `GoalSelector` (priorisierte Goals:
  Schwimmen, Fliehen, Umherlaufen, Angreifen), `PathNavigation` +
  `WalkNodeEvaluator` (A* über Blockzellen), `NaturalSpawner` (Cap pro
  Kategorie, Licht, Abstand zum Spieler); Lithium (Entity-Kollision nur gegen
  Blöcke in der AABB-Umgebung); Entity Culling (Frustum + Distanz, Raycast-
  Verdeckung als spätere Option). Übertragung: **kein** ECS-Crate nötig. Ein
  `EntityStore` mit Generations-IDs (Slot-Map) und einem `enum EntityKind`-
  Datenteil reicht und passt zur vorhandenen `SimSystem`-Architektur; ein ECS
  lohnt erst, wenn viele Komponenten-Kombinationen entstehen.
- *Vorgehen:*
  1. `src/entities/mod.rs`: `EntityId { index: u32, generation: u32 }`,
     `EntityKind`, `struct Entity { pos, prev_pos, velocity, yaw, pitch,
     health, on_ground, … }` und ein `EntityStore` mit **Spatial-Index nach
     `ColumnCoord`** — sonst wird "welche Entities sind in aktiven Spalten" pro
     Tick zu einem linearen Scan über alles. `ActiveSimColumns::is_active` ist
     die vorhandene Abfrage dafür.
  2. `EntitiesSystem: SimSystem` mit Bewegung/Schwerkraft/Kollision (die
     Funktionen oben) im festen 20-TPS-Takt, danach erst AI. `prev_pos` wird
     vor jedem Tick gesichert, der Renderer interpoliert mit `alpha` (siehe
     "Architektur / Spiellogik aus State lösen"). Entladen von Spalten
     entfernt Entities über `on_columns_exited` — **nach** dem Einsammeln für
     den Save (siehe Storage v3, Schritt 6).
  3. KI: `Goal`-Trait mit `can_use`/`tick`/Priorität, pro Mob-Art eine
     statische Goal-Liste. Pathfinding als A* über Blockzellen mit Knotenlimit
     (z. B. 400) und Budget pro Tick; Pfade cachen, neu planen nur bei
     Zieländerung oder Blockade.
  4. Spawning: lichtabhängig, nachts, in einem Ring um den Spieler, Cap pro
     geladener Spalte und pro Kategorie (passiv/feindlich); Spawn-Tabellen
     später pro Biom (siehe "Worldgen / Biome & Multi-Noise-Terrain").
  5. Rendering: Box-Modelle aus Daten (die Helfer aus `player_model.rs` zu
     einem `BoxModel`-Builder verallgemeinern, der Spieler wird erster Nutzer).
     Modell-Meshes pro `EntityKind` einmal bauen und cachen, wie es der
     Block-Preview-Cache schon macht; pro Frame nur Instanzdaten (Transform,
     Limb-Swing, Licht). **Ein** Instanz-/Batch-Draw pro Mob-Art, nicht ein
     Draw pro Entity; Frustum- und Distanz-Culling vor dem Instanz-Upload.
  6. Raycast für Nahkampf: separater Test gegen Entity-AABBs in derselben
     Reichweite, das nähere Ergebnis gewinnt (siehe "Kampf").
  7. **Persistenz ist ein Blocker**, siehe Storage-Punkt unten.
- *Inhalt:*
  - Passiv: Schwein (satteln/reiten), Schaf (Wolle regeneriert, scherbar), Kuh
    (melkbar), Huhn (legt Eier).
  - Feindlich: Zombie, Skelett (Fernkampf), Spinne (klettert, tagsüber
    neutral), Creeper (Explosion), Slime (teilt sich), Spider Jockey.
  - Nicht-Mob-Entities: Drop-Item (Voraussetzung für "Abbau droppt"), Pfeil,
    TNT (gezündet), Lore + Schiene, Ofen-/Truhen-Lore, Boot, später Fall-Block
    (siehe Sand/Kies).
  - Bewusst **nicht** enthalten (Vorlage: kamen erst in Beta):
    Erfahrungskugeln.
- *Dateien:* neues `src/entities/`, `sim/mod.rs`, `player/mod.rs`,
  `render/player_model.rs` → `render/box_model.rs`, `render/state.rs`
  (Registrierung, Render-Pass), `world/raycast.rs`, `storage/chunk_codec.rs`.
- *Abhängigkeiten:* "Architektur / Spiellogik aus State lösen" (Tick +
  Interpolation), Welt-Änderungsjournal (Creeper, blockverändernde Mobs),
  Storage v3 (Persistenz), Biome (Spawn-Tabellen, optional).
- *Priorität/Aufwand/Impact:* Schritte 1, 2 und 5 **P2** (L); KI, Pathfinding
  und Spawning **P3** (L); Impact sehr hoch.
- *Akzeptanz:* 100 Schweine in geladenen Spalten: der Trace-Scope
  `sim.fixed_tick.entities` bleibt unter 1 ms pro Tick, ein Draw-Call pro
  Mob-Art, flüssige Bewegung auf einem 144-Hz-Monitor (Interpolation); Mobs
  überstehen Entladen/Neuladen der Spalte; ein Zombie findet einen Weg um eine
  5 Blöcke breite Wand.

### Survival-Gameplay

Ist-Stand: `PlayerMode::Survival` existiert und wird per `/gamemode` gesetzt,
**Standard ist aber `PlayerMode::Creative`** (`player/mod.rs:62`). Der Modus
unterscheidet heute genau zwei Dinge: den Bewegungsmodus (Survival →
`MoveMode::Walk` + Boden-Snap, `app/mod.rs:2899`) und ob Bedrock abbaubar ist
(`render/state.rs:19139`). Es gibt keine Gesundheit, keinen Schaden, keinen
Tod, keine Abbauzeit und keinen Ressourcenverbrauch. Survival ist damit heute
"Creative ohne Fliegen", nicht ein eigener Spielmodus.

Alles Folgende ist **modusgegated**: Creative behält Sofort-Abbau, Flug,
unbegrenzte Blöcke, keinen Schaden. Der Ort dafür ist nicht verstreut — es gibt
bereits einen `player_mode`-Parameter, der bis in den Block-Edit-Pfad
durchgereicht wird (`destroy_block_at_camera_ray(&camera, player_mode)`,
`render/state.rs:19103`, aufgerufen `app/mod.rs:3702` und `:4017`). Diese
Signatur ist der richtige Angriffspunkt; sie sollte **nicht** durch ein globales
"is_survival"-Flag ersetzt werden.

#### Modus-Kontrakt

Zuerst festschreiben, was der Modus überhaupt gatet, bevor Einzelfeatures
gebaut werden — sonst wandert die Fallunterscheidung in jeden Call-Site:

- Abbau: sofort (Creative) vs. Härte/Werkzeug (Survival)
- Platzieren: unbegrenzt vs. verbraucht Stack
- Drops: keine vs. Item-Entity/Inventar
- Bewegung: Flug vs. Gehen (steht nur halb — *Korrektur 2026-09-28:*
  `/gamemode survival` setzt `MoveMode::Walk` (`app/mod.rs:2899-2901`), aber
  Doppel-Leertaste schaltet ohne Moduscheck zurück auf Flug:
  `handle_space_pressed` (`:2945`) → `toggle_move_mode` (`:2823`))
- Schaden/Tod: aus vs. an
- Bedrock: abbaubar vs. nicht (steht schon)
- Mob-Aggression: ignoriert den Spieler vs. greift an

Umsetzung: ein `struct ModeRules` mit `const fn for_mode(PlayerMode) -> Self`
in `player/mod.rs`, in dem genau diese Booleans stehen. Jede Fallunterscheidung
liest daraus, statt `matches!(mode, PlayerMode::Creative)` erneut zu schreiben —
dieses Muster steht heute schon zweimal im Code und würde sonst zweistellig oft.
(*Korrektur 2026-09-28:* es steht einmal im Code, `render/state.rs:19139`;
Platzieren ist gar nicht modusgegated.)

#### Block-Abbau: Härte, Werkzeug-Effizienz, Haltbarkeit

**Fehlt komplett** — der Abbau ist sofortig. Der Wiederholungspfad ist aber
schon genau richtig gebaut und muss nur gefüllt werden: `App::break_held` +
`break_cooldown_seconds` (`app/mod.rs:417-419`) treiben in der Frame-Schleife
(`app/mod.rs:4011-4023`) wiederholtes Abbauen mit einer festen Wartezeit aus
`block_action_repeat_cooldown_seconds()` (`:479`).

- `BlockSpec` bekommt `hardness: f32` und `tool_class: ToolClass`
  (`None`/`Pickaxe`/`Axe`/`Shovel`/`Shears`) plus `requires_tier: Option<Tier>`.
  Beides sind konstante Tabellenfelder wie `breakable` heute — kein neuer
  Speicher pro Voxel.
- In Survival ersetzt ein **Fortschrittszähler pro Ziel** die feste Wartezeit:
  `struct BreakProgress { target: IVec3, progress: f32 }`. Zurücksetzen, sobald
  der Raycast ein anderes Ziel liefert oder die Maustaste losgelassen wird —
  sonst kann man durch Zielwechsel weiterbohren. Der Raycast-Treffer liegt schon
  vor (`world/raycast.rs`, `hit.hit_block_pos`).
- Fortschrittsanzeige: `assets/minecraft/textures/block/destroy_stage_0..9.png`
  liegen bereits im Pack (10 Stufen). Als zusätzliches Overlay-Quad auf den
  Flächen des Zielblocks zeichnen — die Selektionsbox-Geometrie existiert
  bereits und liefert dieselben Flächen.
- Haltbarkeit: pro erfolgreichem Abbau ein Punkt Verschleiß auf dem gehaltenen
  Werkzeug; bei 0 verschwindet das Item. Diamant 1025 Uses (Vorlage,
  seit a1.0.3).
- Creative bleibt sofort: `ModeRules::instant_break`.

#### Gesundheit & Schaden

- 20 HP (10 Herzen), Regeneration in dieser Ära **nur** durch Essen (sofortiger
  Heileffekt). Keine Hungerleiste — siehe Grundregel oben zu `food_level`.
- Schadensquellen der Vorlage: Fall, Ertrinken, Lava, Feuer, Ersticken in
  Blöcken, Mob-Nahkampf, TNT-Explosion, Kaktus-Kontakt.
- **Fallschaden**: der Landepunkt steht exakt fest — `collision.hit_y_down` in
  `update_walk_movement` (`player/mod.rs:710-713`) setzt `on_ground = true` und
  nullt `_velocity_y`. Unmittelbar davor ist `_velocity_y` die Aufprallgeschwin-
  digkeit. Die Vorlage rechnet aber über die **Fallstrecke**, nicht über die
  Geschwindigkeit; ein `fall_start_y: f32` im `Player` (ein `f32`, gesetzt beim
  Verlassen des Bodens, ausgewertet an derselben Stelle) trifft die Originalformel
  (`Schaden = floor(Fallhöhe) - 3`) direkt. Wasser/Leiter setzen ihn zurück.
- **Ertrinken**: `PlayerFluidState` (`player/mod.rs:787`) kennt bereits
  `eyes_in_for_kind(FluidKind::Water)`. Luftvorrat als Tick-Zähler dazu; unter 0
  ein Herz pro Sekunde. Die Luftblasen-Anzeige gehört ins HUD.
- **Lava/Feuer**: `in_body_for_kind(FluidKind::Lava)` liegt ebenfalls vor. Feuer
  als Zustand am Spieler mit Restdauer, nicht als Weltblock — der Feuerblock
  selbst ist nicht Teil dieser Liste.
- **Ersticken**: Kopfzelle solide → Schaden. `aabb_collides`
  (`player/mod.rs:135`) beantwortet das direkt.
- **Kaktus**: Kontaktschaden über die AABB-Überlappung, sobald der Block
  existiert (siehe "Blöcke").
- Alle Quellen laufen über **eine** Funktion `apply_damage(&mut Player, amount,
  DamageSource)` mit gemeinsamer Unverwundbarkeitszeit (Vorlage: 0.5 s), sonst
  addieren sich Lava und Feuer pro Tick doppelt.

#### Tod & Respawn

- Todesbildschirm mit Respawn-Knopf (neuer `GameState`-Zweig, siehe UI/UX).
- Respawn setzt auf `PlayerSpawn` aus `storage/world_meta.rs` — wird bereits
  geladen (`render/state.rs:6421`) und ist damit schon persistent.
- Beim Tod fällt das Inventar als Item-Entities zu Boden (hängt am
  Entity-System) — bis dahin ersatzweise leeren.
- Die Vorlage zeigt auf dem Todesbildschirm "Score: &e0"; das ist ein Bug der
  Vorlage und wird nicht nachgebaut (siehe "Nicht übernommen").

#### Kampf

- Nahkampf: Linksklick auf ein Entity statt auf einen Block. Der Raycast muss
  dafür Entities mittreffen — heute testet `world/raycast.rs` nur Voxel. Am
  günstigsten als zweiter, separater Test gegen die Entity-AABBs **in derselben
  Reichweite**, dann das nähere Ergebnis gewinnt; kein Umbau des Voxel-Raycasts.
- Schadenswerte pro Werkzeugstufe, Rüstungsreduktion über die 4 Rüstungsslots.
- Fernkampf: Bogen (in dieser Ära **unendlich viele Pfeile**) und Pfeil als
  Projektil-Entity.
- Knockback beim Treffer in beide Richtungen.

#### Interaktion ("Benutzen")

*Neu formuliert 2026-09-28:* der bisherige Vorschlag (`on_use` in `BlockSpec`,
Sneak überspringt) bleibt, ergänzt um Platzier-Kontext und Abbau-Hooks.
Weltseitige Reaktionen (Nachbar-Updates, geplante Ticks) stehen unter
"Architektur / Block-Verhalten".

- *Ist:* Es gibt nur Abbauen und Platzieren. Rechtsklick platziert immer
  (`app/mod.rs:3714-3726`, `:4027-4038` → `place_block_at_camera_ray`,
  `render/state.rs:19233`). Der Platzierpfad bekommt Kamera, gewählten Block
  und Fußposition, aber **keinen** Kontext: kein `player_mode`, keinen
  Trefferpunkt, keine getroffene Fläche außer implizit über `hit.place_pos()`
  (`:19281`), keine Blickrichtung; `block_data` ist für Nicht-Fluids immer 0
  (`:19316-19325`). `RaycastHit` enthält Position, Normale und Box
  (`world/raycast.rs:13-18`), aber nicht den Trefferpunkt (`t_hit` wird bei
  `:197` berechnet und verworfen). Abbauen kennt nur `breakable: bool`
  (`blocks/mod.rs:509`) und die Bedrock-Sonderregel
  (`render/state.rs:19138-19142`). Kein `on_use`, kein `hardness` (Suche leer).
- *Ziel:* Türen, Kisten, Werkbank, Ofen, Jukebox, Hebel und Knopf reagieren auf
  einen echten Rechtsklick-Handler pro Block, der *vor* dem Platzieren greift;
  orientierte Blöcke (Treppe, Stufe oben/unten, Wandfackel, Tür-Scharnier,
  Leiter) wählen ihren State aus dem Platzier-Kontext; Blöcke können beim
  Entfernen reagieren (zweite Tür-Hälfte, Kiste droppt Inhalt).
- *Vorbild:* Vanilla `Block.use`, `getStateForPlacement(BlockPlaceContext)`
  (Klickfläche, Trefferpunkt, Blickrichtung, Sneak), `playerWillDestroy`/
  `onRemove`. Übertragung: Funktionszeiger in `BlockSpec` — dieselbe Bauform wie
  das vorhandene `random_tick: Option<RandomTickFn>` (`blocks/mod.rs:514`,
  `with_random_tick` `:2007`), damit die Dispatch-Tabelle einheitlich bleibt;
  kein Trait-Objekt pro Block.
- *Vorgehen:*
  1. `RaycastHit` um `hit_point: Vec3` erweitern.
  2. `struct PlaceContext { hit, face, hit_point_local, look_dir, yaw,
     sneaking, player_mode }` und `on_place: Option<fn(&World, &PlaceContext,
     BlockId) -> Option<BlockState>>` (liefert den State oder lehnt ab).
  3. `on_use: Option<UseFn>`: Der Rechtsklick-Pfad fragt zuerst `on_use` des
     getroffenen Blocks; nur wenn das `None` liefert oder ablehnt, wird
     platziert. Sneak-Halten überspringt `on_use` (Vorlage-Verhalten für "auf
     eine Kiste bauen").
  4. `on_removed: Option<RemovedFn>` für Mehrblock-Strukturen und
     Block-Entities mit Inhalt.
  5. Die Hooks werden aus dem GPU-freien Gameplay-Modul aufgerufen (siehe
     "Architektur / Spiellogik aus State lösen"), nicht aus `render/state.rs`.
- *Dateien:* `blocks/mod.rs`, `world/raycast.rs`, `render/state.rs`
  (`:19103-19410`), `app/mod.rs` (Klickpfad).
- *Abhängigkeiten:* Block-States (State als Rückgabewert),
  Welt-Änderungsjournal; Kisten und Ofen zusätzlich Item-System und Storage v3.
- *Priorität/Aufwand/Impact:* **P2**, M, hoch (Voraussetzung für alle
  interaktiven Blöcke).
- *Akzeptanz:* Test ohne GPU: eine Stufe, auf die obere Hälfte einer
  Seitenfläche geklickt, bekommt `half=top`; eine Tür öffnet und schließt per
  Rechtsklick, aber nicht beim Sneak-Klick; eine Fackel an der Wand übernimmt
  die Flächenrichtung.

#### Reiten

Schwein (mit Sattel, in dieser Ära nur Richtungssteuerung ohne Karotte), Lore
(mit Sitz-Animation seit a1.0.4), Boot (Rechtsklick zum Einsteigen, zerbricht an
Land). Hängt vollständig am Entity-System. Die Kamera muss dafür einem Entity
statt `player.feet_position` folgen — `sync_camera_to_player_eye`
(`app/mod.rs:1068`) ist die eine Stelle, die das entscheidet.

#### Bewusst abweichend

Sprint und Schleichen gibt es in der Vorlage nicht (kamen erst in Alpha 1.2.0),
im Code aber schon (`InputAction::Sprint`/`Sneak`, `sprint_swim_active`). Nach
der Grundregel oben bleiben beide. Sneak bekommt mit Survival zusätzlich seine
klassische Aufgabe: nicht über Kanten laufen, und `on_use` überspringen.

### Redstone & Logik

Komplett nicht vorhanden. Hängt an Block-States (Signalstärke = 4 Bit in
`block_data`). *Ergänzung 2026-09-28:* und an "Architektur / Block-Verhalten:
Neighbor-Updates und Scheduled Ticks" — die dort vorgeschlagene generische
Tick-Queue ist die verallgemeinerte Fassung der unten genannten Fluid-Queue.

- Redstone-Staub mit Signalstärke 0-15 und Ausbreitung über 15 Blöcke.
- Redstone-Fackel als Inverter (mit Burnout-Verhalten der Ära).
- Signalquellen: Hebel, Steinknopf, Holz-/Steindruckplatte.
- Verbraucher: Eisentür, Holztür, TNT-Zündung, Notenblock.
- Umsetzung: **kein** Tick-pro-Block-Scan. Ein `RedstoneSystem: SimSystem` mit
  einer Update-Queue nach dem Muster von `FluidSystem::schedule_fluid_update`
  (`world/fluids.rs:467`) — dort ist die verzögerte, positionsbasierte
  Update-Queue inklusive Pruning nach Keep-Radius und aktiven Spalten schon
  gebaut und bewährt. Redstone hat exakt dieselbe Zugriffsform (lokaler
  Nachbarschafts-Neuberechnung mit Tick-Verzögerung), also die Struktur
  übernehmen statt neu erfinden.
- Bewusst nicht enthalten: Kolben, Schiebetüren (kamen später).

### Storage: Column-Payload v3 für Block-Entities und Entities

**Eigener, vorgezogener TODO — harter Blocker für Kisten, Öfen, Schilder,
Jukebox, Notenblock, Spawner und jedes Mob/Drop-Item.** Ohne ihn ist jeder
Inhalt mit Zustand nach einem Chunk-Unload weg, und das fällt erst beim Testen
auf, nicht beim Bauen.

#### Befund

- `encode_blob` (`storage/chunk_codec.rs:460`) schreibt Header, Sektionen
  (Blöcke + `block_data`) und CRC32 — sonst nichts. Weder `block_entit` noch
  `entities` kommt in der Datei vor.
- Block-Entities werden beim Laden aus der Blocktabelle neu erzeugt
  (`BlockSpec::block_entity`), haben also heute per Definition keinen Zustand,
  den man verlieren könnte. Genau das ändert sich mit der ersten Kiste.
  *Korrektur 2026-09-28:* Sie werden beim Laden **nicht** neu erzeugt. Sie
  entstehen nur über `BlockEntityBlockChange` aus den `World`-Settern
  (`world/mod.rs:3250-3334`); `BlockEntitiesSystem::on_columns_entered` ist
  leer (`world/block_entities_system.rs:106-112`), und Chunks aus Worldgen oder
  Save werden direkt in `World.chunks` eingefügt. Eine geladene oder generierte
  Block-Entity-Zelle hat also gar keine Block-Entity. Für v3 deshalb:
  fehlende Block-Entities nach dem Laden aus der Blocktabelle nachziehen (in
  `on_columns_entered` die Sektionen nach Block-Entity-Blöcken scannen);
  geladene Payloads haben Vorrang.
- Der Weg ist im Dateikopf von `chunk_codec.rs:23-24` schon vorgezeichnet:
  "Adding a v3 means bumping `COLUMN_PAYLOAD_FORMAT_VERSION` and keeping both
  older branches in `decode_column`."

#### Formatvorschlag v3

Der entscheidende Punkt: **die Sektionen bleiben bitgleich v2.** v3 hängt nur
einen Column-Trailer zwischen der letzten Sektion und der CRC32 an. Damit ist
der teure, gemessene Teil des Formats (Blockindizes, 92.4 % der Nutzlast, siehe
die Ablehnungsnotiz in `chunk_codec.rs:26-35`) unverändert und die
v2→v3-Migration ist reines Anhängen.

Der Erweiterungspunkt existiert schon: das `u16`-Feld **column flags** direkt
hinter der Version ist heute `COLUMN_FLAGS_V2 = 0` (`chunk_codec.rs:51`) und
wird beim Lesen auf exakt 0 geprüft (`:521-523`).

```
Header      … unverändert (magic, version=3, flags, 8×i32, section_count, reserved0)
Sections    … unverändert v2-Kodierung
[Trailer]   … nur wenn flags & COLUMN_FLAG_HAS_TRAILER
CRC32       … über alles davor, unverändert
```

Trailer-Layout (little endian, wie der Rest):

```
u16 trailer_version            (1)
u16 trailer_reserved           (muss 0 sein)
u16 type_key_count             Palette zuerst, damit ein Durchlauf reicht
  je Eintrag: u16 len + UTF-8 bytes   (NamespacedKey als Name, nicht als ID)
u32 block_entity_count
  je Eintrag:
    i32  cy                    (Sektion)
    u16  local                 (gepackter lokaler Index, wie pack_local)
    u16  type_key_index        (Index in die Palette oben)
    u32  payload_len
    u8[] payload
u32 entity_count
  je Eintrag:
    u16  type_key_index
    u32  payload_len
    u8[] payload
```

Begründungen, die beim Umsetzen nicht wegdiskutiert werden sollten:

- **Namen statt numerischer IDs** für `type_key`, aus demselben Grund, aus dem
  v2 Blocknamen speichert: selbstbeschreibend, ein unbekannter Typ degradiert
  sauber (Block-Entity wird verworfen und aus der Blocktabelle neu erzeugt,
  Entity wird verworfen) statt still auf einen fremden Typ zu zeigen.
- **Eine Palette pro Column**, nicht pro Eintrag: es gibt wenige Typen und viele
  Einträge. Das ist die *umgekehrte* Entscheidung zur Blockpalette, wo eine
  Column-Palette gemessen 18 % schlechter war — dort kostete das breitere
  `bits_per_entry`, hier gibt es kein bitpacking.
- **Flag statt neuer Pflichtsektion**: eine Column ohne Block-Entities und ohne
  Entities schreibt weiterhin exakt die v2-Bytes plus einer Version. Terrain,
  also die große Mehrheit aller Columns, wird nicht größer.

#### Arbeitsschritte

1. `COLUMN_PAYLOAD_FORMAT_VERSION` 2 → 3 in `storage/format.rs:13`.
   `COLUMN_FLAGS_HAS_TRAILER: u16 = 0x0001` daneben. Die Flag-Prüfung in
   `decode_column` (`:521`) von "muss 0 sein" auf "unbekannte Bits müssen 0
   sein" ändern — sonst ist der nächste Erweiterungsschritt wieder ein
   Versionsbump.
2. v1- und v2-Zweig in `decode_column` unangetastet lassen. Der Decoder trägt
   die Fallunterscheidung schon (`:512-517`); es kommt ein dritter Wert dazu,
   kein Umbau.
3. `BlockEntity`-Trait (`world/block_entity_types.rs:16`) um
   `fn save(&self) -> Vec<u8>` und einen `load`-Pfad erweitern. `BlockEntityType`
   (`:11`) bekommt neben `create` ein
   `load: fn(pos: IVec3, &[u8]) -> Option<Box<dyn BlockEntity>>`. Serde ist
   bereits Abhängigkeit, und das Regionformat komprimiert ohnehin mit zlib —
   ein kompaktes Serde-Format reicht, es braucht kein eigenes Binärlayout pro
   Typ.
4. **`ColumnSaveSnapshot` erweitern — und hier liegt die Performance-Falle.**
   Der Snapshot wird auf dem Frame-Thread gebaut
   (`build_column_save_snapshot`, `render/state.rs:6695`) und nutzt bewusst nur
   billige Operationen: `Arc::clone` für `block_data`, ein Clone der paletted
   Blocks. `Box<dyn BlockEntity>` lässt sich nicht billig klonen, also **nicht**
   im Frame serialisieren. Zwei gangbare Wege:
   - jede Block-Entity hält ihre zuletzt serialisierte Form als
     `Option<Arc<[u8]>>` und invalidiert sie beim Mutieren; der Snapshot nimmt
     den `Arc` — das ist die gleiche Bauform wie `block_data` heute, und
     Block-Entities ändern sich selten relativ zur Speicherfrequenz;
   - oder der Save-Worker bekommt eine eigene, per-Column gesperrte Kopie.
     Teurer und führt Sperren auf einem Pfad ein, der heute keine hat.
   Der erste Weg ist der, der zum bestehenden Code passt.
5. `apply_column` (`chunk_codec.rs:736`) muss den Trailer wieder in
   `BlockEntitiesStore` (`world/block_entities_storage.rs:51`) einspielen —
   **nach** dem Setzen der Blöcke, sonst erzeugt die Blocktabelle die
   Block-Entity erneut und überschreibt den geladenen Zustand.
6. Entities gehören pro *Column*, nicht pro Sektion: sie bewegen sich über
   Sektionsgrenzen, und der Save-/Load-Pfad arbeitet ohnehin spaltenweise.
   Beim Unload einer Spalte müssen sie **vor** dem Verwerfen eingesammelt
   werden — `on_columns_exited` im künftigen `EntitiesSystem` ist der Haken
   dafür, aber die Reihenfolge gegenüber dem Save muss explizit festgelegt
   werden, sonst speichert man eine bereits geleerte Spalte.
7. *Ergänzung 2026-09-28:* Der Trailer ist auch der Ort für **Biomdaten**
   (Multi-Noise-3D, 4×4×4 pro Sektion = 64 Einträge; Namens-Palette pro Column
   wie bei `type_key`, bitgepackte Indizes je Sektion) — so bleiben die
   Sektionen bitgleich v2. Empfehlung deshalb: den Trailer von Anfang an als
   Liste getaggter Blöcke anlegen (`u16 tag, u32 len, payload`; unbekannte Tags
   werden übersprungen) statt als feste Folge Block-Entities → Entities. Sonst
   ist jeder weitere Column-Datensatz (Biome, Heightmaps, geplante Ticks,
   eventuell Licht) wieder ein Formatbruch.

#### Tests, die es dafür geben muss

- Round-Trip: Column mit Block-Entities und Entities encoden → decoden →
  gleiche Werte. `chunk_codec.rs` hat bereits eine Round-Trip-Testfamilie, in
  die das gehört.
- **v2-Blob bleibt lesbar** und ergibt eine Column ohne Trailer. Der
  v1-Fixture-Encoder `encode_blob_v1` (`chunk_codec.rs:1474`) ist die Bauform,
  die es dafür in v2 auch braucht.
- Unbekannter `type_key` degradiert, statt zu scheitern.
- CRC32 deckt den Trailer mit ab (Byte-Flip im Trailer muss auffallen);
  `decode_column_rejects_header_signature_drift` (`chunk_codec.rs:1621`) ist die
  Vorlage, und seine Fall-Tabelle muss um das Flag-Feld erweitert werden, sobald
  "flags != 0" nicht mehr pauschal abgelehnt wird.

### Storage: Weltzeit und Wetterzustand in `world.meta`

- *Ist:* `WorldTime` (`world/time.rs`) wird nicht gespeichert — `world_time`
  und `ticks_total` kommen nur in `world/time.rs`, `commands/mod.rs` und
  `render/state.rs` vor (Feld `:5707`), in `storage/` gar nicht. Jede Welt
  startet nach dem Laden wieder mit der Default-Tageszeit
  (`world/time.rs:21`); dasselbe gilt für Wetter (siehe "Architektur /
  Wetter") und den Sim-Tick-Zähler `sim_tick_index` (`render/state.rs:5591`),
  aus dem die Random-Tick-Seeds abgeleitet werden
  (`world/random_ticks_system.rs:35-41`).
- *Ziel:* Tageszeit, Tick-Zähler und Wetterzustand überstehen Speichern/Laden.
- *Vorbild:* Vanilla `level.dat` (`DayTime`, `Time`, `rainTime`,
  `thunderTime`). Direkt übertragbar.
- *Vorgehen:* 1) Optionale Felder mit `#[serde(default)]` in `WorldMetaWire`
  (`storage/world_meta.rs:98-124`): ältere Metas bleiben lesbar, weil fehlende
  Felder Defaults bekommen; wegen `deny_unknown_fields` lehnt aber eine
  **ältere** Programmversion neue Metas ab — so lassen und im Fehlertext
  benennen. 2) Beim Autosave und beim Beenden schreiben. 3) `sim_tick_index`
  mitspeichern, damit sich Random-Tick-Seeds nach dem Laden nicht wiederholen.
- *Dateien:* `storage/world_meta.rs`, `render/state.rs`, `world/time.rs`.
- *Abhängigkeiten:* keine.
- *Priorität/Aufwand/Impact:* **P2**, S, mittel (die Vorlage speichert die
  Zeit ebenfalls).
- *Akzeptanz:* Welt am Abend speichern und neu laden → gleiche Tageszeit;
  Round-Trip-Test für `world.meta` mit und ohne die neuen Felder.

### Storage: Versionierung & Migrationskette (DataFixer-light)

- *Ist:* Versionen werden überall **geprüft und abgelehnt**, nicht
  hochgezogen: `world.meta` verlangt exakt `format_version = 1` und
  `block_id_version = 1` (`storage/world_meta.rs:320-338`), `manifest.json`
  exakt seine Version (`world/manifest.rs:83-88`), der Column-Decoder kennt v1
  und v2 fest verdrahtet (`storage/chunk_codec.rs:512-519`). Die einzige
  Migration ist der CLI-Pfad v1→v2 (`storage/mod.rs:43-121`,
  `--migrate-save-v1-to-v2`, `main.rs:24-25`), der `world.meta` nicht anfasst;
  sonst werden v1-Spalten beim nächsten Speichern still als v2 geschrieben.
  Positiv: Blocknamen statt IDs (`chunk_codec.rs:110-117`) und `key_aliases`
  für umbenannte Blöcke (`blocks/mod.rs:501`, `:685-698`) — das ist bereits
  ein Mini-DataFixer für Blocknamen.
- *Ziel:* Jede Formatänderung ist ein kleiner, getesteter Schritt vN → vN+1;
  beim Laden läuft die Kette automatisch, geschrieben wird immer die neueste
  Version. Vorgabe des Maintainers: Migrierbarkeit ist gewünscht, **große
  Umbauten an Worldgen oder Block-States gehen aber vor** — ein solcher Umbau
  darf alte Welten mit klarer Fehlermeldung ablehnen, der Migrationsschritt
  wird nachgereicht, wenn er billig ist.
- *Vorbild:* Vanilla DataFixerUpper — nur die Idee (versionierte
  Transformationsschritte), nicht die Typ-Maschinerie. Übertragung:
  `world.meta` als `serde_json::Value` einlesen, Schritte als `fn(&mut Value)`,
  danach in den typisierten Wire-Struct. Column-Payloads werden in die
  In-Memory-Form dekodiert (das macht `decode_column` für v1 schon) und neu
  kodiert.
- *Vorgehen:* 1) `storage/migrate.rs` mit einer Tabelle `[(from_version, fn)]`
  je Dateiart (meta, manifest, column). 2) Exakte Versionsprüfungen ersetzen:
  kleiner oder gleich der aktuellen Version → Kette ausführen; größer →
  verständliche Fehlermeldung. 3) Pro Schritt ein Fixture (kleiner Save im
  Repo, Bauform wie `encode_blob_v1`, `chunk_codec.rs:1474`) und ein
  Round-Trip-Test. 4) Block- und State-Umbenennungen laufen über `key_aliases`
  (bzw. deren State-Erweiterung) statt über Code-Schritte.
- *Dateien:* neues `storage/migrate.rs`, `storage/world_meta.rs`,
  `world/manifest.rs`, `storage/chunk_codec.rs`, `storage/mod.rs`.
- *Abhängigkeiten:* keine; vor dem nächsten Format-Bump (v3) billiger als
  danach.
- *Priorität/Aufwand/Impact:* **P3**, M, mittel (laut Maintainer nachrangig
  gegenüber dem Entwicklungstempo).
- *Akzeptanz:* Ein v1-Save aus dem Fixture lädt ohne CLI-Flag; ein Save mit
  Zukunftsversion wird mit klarer Meldung abgelehnt; jeder Schritt ist durch
  ein Fixture in `cargo test` abgedeckt.

### Audio

Komplett nicht vorhanden — es gibt **keine Audio-Abhängigkeit** in `Cargo.toml`
und keinen Ton im gesamten Repo.

- Erst die Grundsatzentscheidung: Crate wählen (`rodio` ist der pragmatische
  Default, `kira` wenn Mixing/Fading gebraucht wird) und ein `src/audio/mod.rs`
  mit einer Handvoll Aufrufen (`play_at(pos, key)`, `play_music(key)`)
  aufsetzen. Vorher lohnt kein Inhalts-TODO.
- Laden über den vorhandenen `AssetSource`/Resourcepack-Pfad (`src/assets.rs`),
  also `assets/minecraft/sounds/...` — der Ordner existiert heute nicht.
- Inhalt danach: Schritt-Geräusche pro Material (`BlockSpec` braucht dafür ein
  `sound_group`-Feld), Block-Abbau/-Platzieren, Höhlen-Ambiente, Mob-Sounds,
  Hintergrundmusik mit zufälligen Pausen, Musikplatten über die Jukebox.
- Positionsabhängige Lautstärke/Panning braucht nichts Neues: Kamera-Position
  und -Ausrichtung liegen in `src/camera.rs` vor.

### Nicht übernommen (bewusst)

Aus der Vorlage ausdrücklich **nicht** auf die Liste genommen:

- Abschnitt 6 (Multiplayer/SMP) komplett — auf Wunsch ausgeklammert.
- Far Lands bei ±12.550.820 mit Mesh-Verzerrungen: ein Bug der Vorlage, kein
  Feature. Der harte Weltrand mit Boundary-Ocean (`WorldConfig::boundary_ocean`)
  ist die hier gewählte Lösung.
- `session.lock`: die Vorlage braucht das gegen zwei Java-Instanzen; hier gibt
  es einen asynchronen Save-Writer mit einem Prozess. Aufnehmen erst, wenn
  mehrere Instanzen auf dasselbe Save zeigen können.
- Die Bug-Liste aus Abschnitt 8 (Score-Anzeige „Score: &e0", riesige Items in
  Third-Person, Zombie droppt Federn, faktisch deaktiviertes Slime-Spawning) —
  das sind Fehler der Vorlage, nicht nachzubauen.
- Java-Version und Protokollversionen (Abschnitt 7 „Technische Features") sind
  für diese Codebasis gegenstandslos.

## Worldgen

### High priority
- **Worldgen-Regressionstests: Nähte, Reihenfolge-Unabhängigkeit,
  Höhenverteilung** (neu formuliert 2026-09-28; bisher "expand worldgen
  regression tests with seam, density, and height-distribution sampling")
  - ~~Dichte-Stichprobe pro Erz über mehrere Seeds~~ **done**:
    `worldgen::ores::tests::ore_density_stays_within_tolerance_across_presets`
    (`worldgen/ores.rs:668-699`, 2 Seeds, 4×4 Spalten, Toleranz 1,6×) plus
    `every_preset_gets_its_own_ore_table` (`:605-622`)
  - ~~Determinismus gleicher Seed → gleiche Spalte~~ **done**:
    `trees_are_deterministic_for_same_seed_and_column`
    (`render/chunkgen_worker.rs:2701`) vergleicht die komplette Spalte (Blöcke
    + Daten) nach allen Phasen; dazu `ore_generation_is_deterministic_for_
    same_column` (`ores.rs:563-574`)
  - *Ist (offen):* Kein Test vergleicht Randblöcke benachbarter Spalten (die
    Suche nach `seam` findet nur Meshing), keiner prüft, dass die
    Generierungsreihenfolge egal ist, keiner misst die Höhenverteilung der
    **ganzen** Pipeline (die `terrain.rs`-Tests prüfen nur den
    `TerrainSampler`, `worldgen/terrain.rs:282-433`). Bekannte Nahtquellen,
    die solche Tests absichern müssen: Seen-Abdichtung, Ufer- und Quell-Pässe
    überspringen alles außerhalb der Spalte (`chunkgen_worker.rs:688-696`,
    `:799-801`, `:1090-1112`), die Seen-Abdichtung zusätzlich Sektionsgrenzen
    (`:1806-1825`); Baumkronen werden an der Spalte abgeschnitten (`:1267`),
    obwohl `MonumentOak` ±4 statt `TREE_XZ_MARGIN = 3` braucht (`:61`,
    `:1361`, `:1367`).
  - *Ziel:* Jede Nahtregression fällt im Test auf, nicht im Spiel.
  - *Vorbild:* Vanilla-GameTests für Features; hier als normale `cargo test`s
    über `generate_streaming_column_for_test` (`chunkgen_worker.rs:2018-2027`).
  - *Vorgehen:* 1) Reihenfolge-Test: 3×3 Spalten in zwei Reihenfolgen mit
    frischem Koordinator → bitgleich. 2) Naht-Test: an jeder Grenze der 3×3
    zählen, wie oft Wasser über Luft an der Grenze endet und wie oft Laub
    abrupt abbricht; bekannte Fälle als Ist-Wert einfrieren. 3) Höhenhistogramm
    der fertigen Spalten pro Preset als Snapshot mit Toleranz.
  - *Dateien:* `render/chunkgen_worker.rs` (Tests), `worldgen/terrain.rs`.
  - *Abhängigkeiten:* keine; wertvoll vor Multi-Noise und vor dem Umzug der
    Phasen nach `worldgen/`.
  - *Priorität/Aufwand/Impact:* **P2**, M, mittel.
  - *Akzeptanz:* drei neue Tests grün; der `MonumentOak`-Überhang ist behoben
    oder als bekannter Wert eingefroren.
- **Worldgen-Einstiegspunkte konsolidieren — ohne den Density-Baum zu
  löschen** (neu formuliert 2026-09-28; bisher "continue consolidating
  legacy/older worldgen entry points against the active worker pipeline")
  - *Ist:* Aktiv ist nur `StreamingWorldColumnGenerator::generate_column_with_
    debug_stats` (`render/chunkgen_worker.rs:1742-1855`). `worldgen/` liefert
    daraus nur die Basis-Terrain-Phase (`TerrainGenerator` → `WorldgenPipeline`,
    `worldgen/terrain_generator.rs:11-83`, `worldgen/pipeline.rs:106-236`); alle
    weiteren Phasen (Seen, Ufer, Bäume, Blumen, Winter) leben in `render/`.
    `World::generator` dient zur Laufzeit nur `estimated_surface_height_at`
    (`world/mod.rs:3355-3362`), `generate_chunk` nur dem Test-Pfad
    `ensure_column_generated` (`:3197-3215`). Der generische `DensityFn`-Baum
    (`worldgen/density/mod.rs:94-141`) läuft zur Laufzeit nie, weil
    `detect_density_fast_path` (`pipeline.rs:59-83`) den einzigen gebauten Baum
    erkennt. Doppelte Helfer: `worldgen_seed32` (`chunkgen_worker.rs:236-239`)
    = `seed32_from_u64` (`terrain_generator.rs:46-49`); `preset_seed_mix`
    zweimal (`chunkgen_worker.rs:241-248`, `ores.rs:501-508`); `hash_mix64`
    (`chunkgen_worker.rs:263-267`) = `util::rng::hash64` (`util/rng.rs:24-30`);
    Heightmap-Glättung zweimal (`density/mod.rs:107-126`,
    `density/cache.rs:126-153`). `SplitMix64` (`chunkgen_worker.rs:158-189`)
    nimmt die unteren 32 Bit, `Rng64` die oberen (`util/rng.rs:19-21`) — nicht
    austauschbar, ohne Welten zu verändern. Tote Zweige hinter
    `ENABLE_WORLD_FLUID_FEATURES` / `ENABLE_FAR_LOD_PIGGYBACK_ON_FULL_CHUNK_
    RESULTS` (`chunkgen_worker.rs:89-90`, `:3205-3224`).
  - *Ziel:* Alle Phasen in `worldgen/` (Phasen-Module wie `carvers/`), ein
    Einstiegspunkt, keine doppelten Hash-/Seed-Helfer; `render/` enthält nur
    noch Worker-Plumbing.
  - *Vorbild:* Vanilla `ChunkGenerator` mit `ChunkStatus`-Phasen. Übertragung:
    eine `ColumnPipeline` mit fester Phasenliste in der Reihenfolge von
    `docs/todos_worldgen.md`.
  - *Vorgehen:* 1) Phasen einzeln nach `worldgen/` verschieben (reine
    Verschiebung; der Determinismus-Test muss bitgleich bleiben), Seen zuerst —
    das deckt `worldgen/carvers/ponds.rs` aus "Low priority" ab. 2) Helfer
    zusammenlegen, wo sie bitgleich sind; `SplitMix64` bewusst behalten und
    dokumentieren. 3) **`DensityFn` und `DensityChunkCache` behalten**: sie
    sind das Gerüst für den Multi-Noise-Router (siehe "Biome &
    Multi-Noise-Terrain"), keine Altlast. 4) `World::generator` auf einen
    `SurfaceEstimator` reduzieren.
  - *Dateien:* `render/chunkgen_worker.rs`, `worldgen/*`, `world/mod.rs`,
    `util/rng.rs`.
  - *Abhängigkeiten:* Regressionstests (oben) zuerst.
  - *Priorität/Aufwand/Impact:* **P2**, M, mittel (Voraussetzung für
    Multi-Noise; verkleinert `chunkgen_worker.rs` mit 3340 Zeilen).
  - *Akzeptanz:* Determinismus-Test bitgleich; `render/chunkgen_worker.rs`
    enthält keine Terrain-/Feature-Logik mehr; keine doppelten Seed-Helfer.
- **Preset-Unterschiede und Phasenreihenfolge dokumentieren** (neu formuliert
  2026-09-28; bisher "document preset differences and worldgen phase order
  where contributors will find it quickly")
  - *Ist:* Die Phasenliste steht in `docs/todos_worldgen.md:65-77` und in
    `docs/06_runtime_contracts_and_pipelines.md` ("Ore Generation Pipeline"),
    ist gegenüber dem Code (`render/chunkgen_worker.rs:1742-1855`) aber
    unvollständig: Surface-Rules laufen *innerhalb* der Terrain-Phase
    (`worldgen/pipeline.rs:203-234`), Phase 5 enthält eine Dirt→Gras-Promotion
    und eine Sand-Bereinigung (`chunkgen_worker.rs:961`, `:1171`), es gibt eine
    Winterphase 8 (`:1849-1853`) und **keine Meeresfüllung** (siehe
    a1.0.17-Abgleich unten). Preset-Parameter stehen nur im Code
    (`worldgen/terrain.rs:40-124`). `README.md:77` nennt `Rd161348Like` als
    Default, der Code `ClassicFlatter` (`worldgen/terrain.rs:11`).
  - *Ziel:* Ein Contributor findet Phasen, Presets und deren Unterschiede an
    einer Stelle.
  - *Vorbild:* — (Doku).
  - *Vorgehen:* 1) "Worldgen Phase Order" in `docs/todos_worldgen.md` an den
    Code angleichen. 2) Tabelle Preset × (base_height, height_amp, cliff_amp,
    octaves, Erz-Hülle). 3) README korrigieren. 4) Beim Umzug nach `worldgen/`
    die Liste als Doc-Kommentar an die Phasenliste hängen.
  - *Dateien:* `docs/todos_worldgen.md`, `README.md`, `worldgen/terrain.rs`.
  - *Abhängigkeiten:* keine.
  - *Priorität/Aufwand/Impact:* **P2**, S, niedrig.
  - *Akzeptanz:* Die Doku nennt alle 8 Phasen und den richtigen Default.

### Medium priority
- **Nahtsichere `HiddenOnly`-Prüfung für Erze** (neu formuliert 2026-09-28;
  bisher High: "add neighbor-aware exposed-air checks so `HiddenOnly` ores are
  trustworthy at chunk borders")
  - *Ist:* `block_is_exposed_to_air` (`worldgen/ores.rs:432-447`, aufgerufen
    `:406-410`) fragt `column.get_block`, und `ChunkColumn::get_block` liefert
    außerhalb der Spalte `Air` (`worldgen/carvers/mod.rs:61-64`) — an x/z = 0
    oder 15 gilt jedes Erz als freiliegend und wird verworfen. Zusätzlich läuft
    der Erz-Pass vor Seen, Quellen und Bäumen, die Erz später freilegen können.
    **Heute nutzt aber keine Tabelle `HiddenOnly`**: alle Erze stehen auf
    `ExposedOrePolicy::Allow` (`ores.rs:139`, `:157`, `:177`).
  - *Ziel:* Wenn ein Erz versteckt sein soll, gilt das auch an Spaltengrenzen
    und nach späteren Phasen.
  - *Vorbild:* Vanilla (ab 1.18) `OreFeature` mit
    `discard_chance_on_air_exposure`, ausgewertet gegen die schon generierte
    Umgebung.
  - *Vorgehen:* 1) Nachbar-Luft am Spaltenrand deterministisch rekonstruieren
    wie die Carver (die Quellchunks im Radius werden ohnehin gesampelt), statt
    nur die eigene Spalte zu fragen. 2) Alternativ die Prüfung als Nachpass nach
    Seen/Quellen, ebenfalls mit Rand-Rekonstruktion. 3) Test: Erz an x = 15
    neben festem Nachbar-Stein bzw. fester Nachbar-Luft.
  - *Dateien:* `worldgen/ores.rs`, `worldgen/carvers/mod.rs`.
  - *Abhängigkeiten:* erst relevant, wenn ein Erz `HiddenOnly` bekommt (die
    a1.0.17-Erze sind in der Vorlage nicht versteckt).
  - *Priorität/Aufwand/Impact:* **P3** (herabgestuft, weil ohne Nutzer), M,
    niedrig.
  - *Akzeptanz:* Test oben grün; `ore_density_stays_within_tolerance_across_
    presets` bleibt grün.
- add more ore families once replacement/height targets are defined
- introduce biome/material-aware replacement rules beyond plain stone
- add richer height models if balancing needs more than uniform/triangle distributions
- surface more worldgen metrics in the debug overlay
- **Höhere Welten vorbereiten, ohne Erz-Anker neu zu setzen** (neu formuliert
  2026-09-28; bisher "prepare the worldgen config path for taller worlds without
  redoing ore anchors again")
  - *Ist:* `WorldConfig` hat `min_y`/`max_y` (`world/config.rs:7-8`, Default
    0/255), aber `World::in_bounds` prüft fest gegen `WORLD_MAX_Y = 255`
    (`world/mod.rs:34`, `:2702-2704`), ebenso die Sand-Gravitation
    (`render/state.rs:17479`) und Worldgen-Konstanten (`WINTER_MAX_SNOW_Y`,
    `chunkgen_worker.rs:81`; `lava_max_y`, `:396`). Erz-Höhen sind absolute
    Werte pro Preset (`worldgen/ores.rs:183-214`). Ein Multi-Noise-Preset
    (siehe unten) wird eher −64…320 wollen.
  - *Ziel:* Weltgrenzen kommen ausschließlich aus `WorldConfig`; Erz-Bänder
    lassen sich relativ zu Boden, Decke oder Meeresspiegel definieren.
  - *Vorbild:* Vanilla `DimensionType` (`min_y`, `height`) und `VerticalAnchor`
    (`absolute`, `above_bottom`, `below_top`) — die Anker-Varianten existieren
    teilweise schon (`HeightAnchor::BelowTop`, `ores.rs:33-38`, derzeit
    `#[allow(dead_code)]`).
  - *Vorgehen:* 1) Alle `WORLD_MAX_Y`-Verwendungen durch `cfg`-Werte ersetzen.
    2) Heightmaps, `BlockAabb` und Sektionsindex auf negative Y prüfen
    (`ChunkCoord.y` ist schon `i32`). 3) Erz-Anker auf `above_bottom` /
    `below_top` / `relative_to_sea` umstellen. 4) Test mit `min_y = -64`,
    `max_y = 319`.
  - *Dateien:* `world/mod.rs`, `world/config.rs`, `render/state.rs`,
    `render/chunkgen_worker.rs`, `worldgen/ores.rs`.
  - *Abhängigkeiten:* keine; vor Multi-Noise.
  - *Priorität/Aufwand/Impact:* **P3**, M, mittel.
  - *Akzeptanz:* Eine Welt mit −64…319 generiert, speichert und lädt; die
    Suche nach `WORLD_MAX_Y` außerhalb der Konfiguration ist leer.

### Low priority
- decide and document long-term world size expectations
- add comparison screenshots/seed references per preset
- revisit historical parity content only after modern feature passes are stable
- improve water pond/lake generation, which currently reads as bare sand+water clumps rather than believable small ponds: the logic actually lives outside `worldgen/carvers/` in `src/render/chunkgen_worker.rs` (`LakeCandidate` at `chunkgen_worker.rs:191`, `make_water_lake_candidate` at `:346`, boundary sealing `apply_water_lake_boundary_seal_to_chunk` at `:642`, carve/fill `apply_lake_carve_and_fill_to_chunk` at `:710`, size bounds `WATER_LAKE_R_MIN/MAX` at `:29-30`; *Zeilen aktualisiert 2026-09-28*, der Umzug ist auch Schritt 1 von "Worldgen-Einstiegspunkte konsolidieren"); consider moving this into a proper `worldgen/carvers/ponds.rs` module alongside `ravine.rs`/`caves.rs`, and improving the shape (organic/noise-perturbed basin outline instead of a filled sphere/blob) plus a proper sand-shore rim (graded sand band around the water edge instead of sand and water occupying the same footprint)

### Test/acceptance ideas
- keep deterministic tests for same-seed/same-column equality
- add a visual/manual checklist for chunk-border seams
- measure that startup/chunkgen stay within acceptable warmup budgets after ore tuning

### Aus dem a1.0.17_04-Abgleich

Alles hier gehört in die dokumentierte Phasenreihenfolge aus
`docs/todos_worldgen.md` ("Worldgen Phase Order"), nicht an ihr vorbei.

- **Dungeons**: 7x7x7-Raum aus Bruchstein/moosigem Bruchstein mit Spawner und
  2 Truhen. Neue Phase *nach* den Carvern und *vor* Erzen, weil der Raum die
  Erzplatzierung nicht nachträglich zerschneiden soll. Kritisch: der Raum ist
  größer als eine Chunk-Kante und muss deterministisch aus einem
  Anker-Chunk erzeugt werden — genau das Muster, das der Erz-Pass für
  nahtstabile Adern schon fährt (Nachbar-Quellchunks sampeln,
  `worldgen/ores.rs`). Kein neuer Nachbearbeitungspass, sondern dieselbe
  Technik. Blockiert zusätzlich durch Spawner (Block-Entity) und Truhen
  (Inhalt) — der Raum selbst kann aber vorher stehen
- **Ton**: Klumpen in Wassernähe. Passt in die vorhandene Shoreline-/
  Material-Cleanup-Phase (5), nicht in einen eigenen Pass —
  `apply_shoreline_pass_to_column` (`chunkgen_worker.rs:1046`) kennt die Wassernähe
  bereits
- **Zuckerrohr** an Strand- und Flussufern, **Kaktus** auf Sand (max. 3 hoch).
  Beides in die Foliage-Phase (7) neben Blumen/Pilze; `apply_flowers_to_column`
  (`chunkgen_worker.rs:1474`) ist die direkte Vorlage inklusive
  Untergrundprüfung und Patch-Radius. Beide brauchen erst die Blöcke
- **Kiesstrände** und **Sand-/Kiesschichten in Höhlen** aus der Vorlage. Sand
  und Kies existieren als Blöcke; es fehlt die Platzierungsregel in den
  Surface-Rules bzw. als Nachbearbeitung der Carver-Hohlräume
- **Winterwelten als Welterstellungs-Option mit 25 % Chance**.
  `WorldEnvironment::Winter` und der Schnee-/Eis-Pass
  (`apply_winter_surface_to_column`, `chunkgen_worker.rs:1705`) existieren
  bereits und werden aus `world.meta` gelesen — offen ist nur die zufällige
  Auswahl bei "Generate new level" statt einer expliziten Einstellung
- **Schneefall lässt die Schneedecke wachsen**. Braucht den geschichteten
  Schneeblock (Block-States) und einen Random-Tick, der nur bei aktivem
  `PrecipitationKind::Snow` greift. Der Random-Tick-Pfad ist da
  (`RandomTicksSystem`), der Niederschlagszustand auch
  (`State::active_precipitation_kind`, `render/state.rs:6228`)
- **Nicht gefrorenes Wasser an Kiesstränden** in Winterwelten — eine Ausnahme in
  `winter_surface_supports_snow` (`chunkgen_worker.rs:1685`), sobald Kiesstrände
  existieren
- **Erzfamilien Diamant, Redstone, Lapis** in die vier Preset-Tabellen in
  `worldgen/ores.rs`. Deckt den bereits offenen Punkt "add more ore families
  once replacement/height targets are defined" oben inhaltlich ab: die
  Vorlage liefert die fehlende Zielvorgabe
- **Meere: Wasserfüllung bis `sea_level`** (neu 2026-09-28)
  - *Ist:* Es gibt **keinen** Meeres-/Ozean-Füllschritt: `sea_level` begrenzt
    nur Lava-Seen und Quellstarts (`render/chunkgen_worker.rs:396`,
    `:473-474`), die Surface-Rules setzen am Meeresspiegel lediglich Erde statt
    Gras (`worldgen/surface_rules.rs:34-42`). Terrain unterhalb von
    `sea_level` (möglich z. B. bei `Hilly`: `base_height` 90, `height_amp` 34,
    `worldgen/terrain.rs:40-124`) bleibt eine trockene Senke. Die Vorlage hat
    Ozeane.
  - *Ziel:* Alle Luft über dem Terrain unterhalb von `sea_level` ist Wasser
    (unter Tage durch die Carver begrenzt); Strände und Kiesstrände an der
    Küste.
  - *Vorbild:* Vanilla bis 1.17 ("unter dem Meeresspiegel ist Wasser"), ab 1.18
    Aquifere. Übertragung: im heutigen Preset-Modell ein Füllschritt in der
    Terrain-Phase; im Multi-Noise-Preset übernehmen Aquifere.
  - *Vorgehen:* 1) In `WorldgenPipeline::generate_chunk`
    (`worldgen/pipeline.rs:203-234`) Luft ≤ `sea_level` oberhalb der
    Oberfläche mit `WaterStill` + `fluid_data_make_static` füllen. 2) Carver
    schneiden nicht ins Meer (die Wasser-Prüfung existiert,
    `worldgen/carvers/caves.rs:411`, `worldgen/carvers/ravine.rs:307`).
    3) Ufer-Pass (`chunkgen_worker.rs:1046`) auf echte Küsten anwenden und die
    Kiesstrände oben dort andocken. 4) Fluid-Bootstrap für große statische
    Wasserflächen beim Laden prüfen.
  - *Dateien:* `worldgen/pipeline.rs`, `render/chunkgen_worker.rs`,
    `worldgen/carvers/*`.
  - *Abhängigkeiten:* keine; im Multi-Noise-Preset durch Aquifere ersetzt.
  - *Priorität/Aufwand/Impact:* **P2**, S-M, hoch (Weltbild der Vorlage).
  - *Akzeptanz:* Senken unter `sea_level` sind geflutet, keine schwebenden
    Wasserflächen; die Fluid-Debug-Zähler (F3) zeigen beim Laden keine
    Update-Welle.

### Biome & Multi-Noise-Terrain

Neu 2026-09-28. Zielbild laut Maintainer: Biome nach dem modernen
Multi-Noise-3D-Ansatz. Die vier heutigen Presets bleiben als "klassische
Welttypen" wählbar (Grundregel: das vorhandene Feature gewinnt).

- **Multi-Noise-Terrain als neuer Preset (Noise-Router nach 1.18-Vorbild)**
  - *Ist:* Terrain ist eine 2D-Heightmap minus Y
    (`Add(Cache2D(Heightmap2D), Mul(-1, Y))`,
    `worldgen/terrain_generator.rs:51-67`). Der `DensityFn`-Baum
    (`worldgen/density/mod.rs:94-141`) und der 5×5×5-Grid-Cache mit
    trilinearer Interpolation (`density/cache.rs:8-20`, `:80-112`) existieren
    aber schon. Keine Überhänge außer durch Carver, keine Klimaparameter,
    keine Aquifere; die Carver ignorieren den Preset
    (`worldgen/carvers/caves.rs:20-29`).
  - *Ziel:* Ein neuer Default-Preset, dessen Terrain aus Klimaparametern
    entsteht (Kontinentalität, Erosion, Peaks & Valleys, Tiefe) und der dieselben
    Parameter an die Biomwahl weitergibt.
  - *Vorbild:* Vanilla 1.18+ `NoiseRouter` / `DensityFunction` (`continents`,
    `erosion`, `ridges`, spline-basierte `offset`/`factor`/`jaggedness`,
    `final_density`), `NoiseChunk` mit `cache_2d`/`flat_cache`/`interpolated`
    (Zellgröße 4×8×4), `Aquifer`, Noise-Höhlen (Cheese/Spaghetti/Noodle).
    Übertragung: `DensityFn` bleibt ein Enum-Baum wie heute (kein Trait-Objekt
    pro Knoten), Splines als kleine Tabellen; 2D-Knoten pro Spalte cachen
    (siehe Performance "2D-Terrain-Noise pro Spalte").
  - *Vorgehen:* 1) `DensityFn` um die fehlenden Knoten erweitern (`Spline`,
    `Clamp`, `RangeChoice`, `Interpolated`, `FlatCache`), die
    Fast-Path-Erkennung beibehalten. 2) Klimanoise (`temperature`,
    `vegetation`, `continents`, `erosion`, `weirdness`, `depth`) mit festen
    Salzen aus dem Seed. 3) Preset `MultiNoise` in `WorldgenPreset`
    (`worldgen/terrain.rs:3-9`), zunächst ohne Aquifere und mit den bestehenden
    Carvern. 4) Aquifere (lokaler Wasser-/Lava-Spiegel pro Zelle) statt der
    heutigen Seen-Heuristik unter Tage. 5) Noise-Höhlen als zusätzliche
    Dichteterme. 6) Seen, Ufer und Bäume auf "Oberfläche aus der Dichte" statt
    "aus der Heightmap" umstellen (`estimated_surface_height_at` braucht einen
    Ersatz).
  - *Dateien:* `worldgen/density/*`, `worldgen/terrain.rs`,
    `worldgen/pipeline.rs`, `worldgen/noise.rs`, Worldgen-Phasen (nach der
    Konsolidierung in `worldgen/`).
  - *Abhängigkeiten:* "Worldgen-Einstiegspunkte konsolidieren",
    Regressionstests, "Höhere Welten vorbereiten", Performance
    "2D-Terrain-Noise pro Spalte".
  - *Priorität/Aufwand/Impact:* **P3**, XL, sehr hoch (Grundlage für Biome,
    Meere, Gebirge).
  - *Akzeptanz:* Der Preset erzeugt über mehrere Seeds Ozeane, Küsten, Hügel und
    Gebirge; Chunkgen-Zeit pro Spalte (`ColumnGenerationStageTimings`,
    `render/chunkgen_worker.rs:1859`) höchstens doppelt so hoch wie bei den
    heutigen Presets; Determinismus- und Nahttests grün.

- **Biom-Registry und 3D-Biomdaten je 4×4×4, gespeichert und im Mesher
  verfügbar**
  - *Ist:* Es gibt kein Biom-System (Suche nach
    `biome|temperature|humidity|climate` findet nur einen Kommentar,
    `render/state.rs:10246`, und einen Debug-String, `:19012`).
    `WorldEnvironment { Temperate, Winter }` (`world/environment.rs:1-5`) ist
    ein weltweiter Schalter. Der Column-Cache-Key der Chunkgen enthält ihn
    nicht (`ColumnGenerationDescriptor`, `render/chunkgen_worker.rs:105-114`) —
    derzeit harmlos, weil die Worker beim Weltwechsel neu starten
    (`render/state.rs:6205`), mit Biomen/Klima aber ein Cache-Fehler.
  - *Ziel:* Jede 4×4×4-Zelle hat ein Biom (Höhlenbiome möglich). Biome tragen
    Klima (Temperatur, Niederschlag), Farben, Oberflächenregeln, Features
    (Bäume, Blumen) und Spawn-Tabellen; `Winter` wird ein Klima-Override bzw.
    eine Biomauswahl.
  - *Vorbild:* Vanilla 1.18 `Climate.ParameterList` (Biom = Punkt im
    6-D-Parameterraum, Auswahl per nächstem Nachbarn), `PalettedContainer` für
    Biome (64 Einträge pro Sektion), `BiomeManager` (Zoom/Fuzzing auf
    Blockauflösung). Übertragung: Biome über dasselbe Registry-Makro wie Blöcke;
    in Viertel-Auflösung reicht eine lineare Suche über ~60 Punkte (bei Bedarf
    ein beim Start gebauter k-d-Baum).
  - *Vorgehen:* 1) `BiomeId` + `BIOME_SPECS` per Makro, zunächst 5-8 Biome
    (Ozean, Strand, Ebene, Wald, Wüste, Taiga/Schnee, Gebirge). 2) Pro Sektion
    `biomes: [BiomeId; 64]` (bzw. paletted) in `Chunk`; Worldgen füllt es aus
    dem Klima-Sampling. 3) Speicherung im Column-Trailer (Storage v3,
    Ergänzung Schritt 7). 4) Surface-Rules (`worldgen/surface_rules.rs`) und
    Foliage-Phase biomabhängig machen. 5) Biom ins F3-Overlay und in den
    Mesher-Snapshot (für die Färbung). 6) Winterwelten auf "nur kalte Biome"
    abbilden; `environment` bzw. die Klima-Parameter in den Column-Cache-Key.
  - *Dateien:* neues `src/biomes/`, `world/chunk.rs`, `worldgen/*`,
    `storage/chunk_codec.rs`, `meshing/neighborhood.rs`,
    `world/environment.rs`, `render/chunkgen_worker.rs`.
  - *Abhängigkeiten:* Registry-Makro, Multi-Noise (für die Parameter; ein
    einfacher 2-Parameter-Stellvertreter erlaubt einen früheren Start),
    Storage v3.
  - *Priorität/Aufwand/Impact:* **P3**, L, sehr hoch (Wetter, Spawning und
    Färbung hängen daran).
  - *Akzeptanz:* F3 zeigt das Biom unter dem Spieler; Biome überstehen
    Speichern/Laden; der Oberflächenblock wechselt an Biomgrenzen (Sand in der
    Wüste).

- **Gras-, Laub- und Wasserfärbung je Biom mit Blending**
  - *Ist:* Die Färbung ist eine Konstante im Shader (`GRASS_TINT`,
    `WATER_TINT`, `render/shaders.rs:60-61`, Verzweigung `:78-83`), gesteuert
    über ein `tint_kind`-Byte im Vertex (`meshing/mod.rs:41-51`, `TINT_*`
    `blocks/mod.rs:76-78`). Im Asset-Bestand fehlen Colormaps
    (`textures/colormap/`).
  - *Ziel:* Farbe pro Vertex aus den umliegenden Biomen gemischt (weiche
    Übergänge), Colormaps per Resourcepack überschreibbar.
  - *Vorbild:* Vanilla `BiomeColors` + `ColorResolver`, Option "Biome Blend"
    (Radius 0-7), Colormaps `grass.png`/`foliage.png`, indiziert über
    Temperatur × Niederschlag. Übertragung: Blending im Mesher auf dem Worker
    aus einer 2D-Biomkarte der Nachbarschaft; das Ergebnis landet als RGB im
    Vertex.
  - *Vorgehen:* 1) Der Vertex bekommt eine gepackte Tint-Farbe (`u32` RGBA8)
    statt oder zusätzlich zu `tint_kind` — mit dem kompakten Vertex-Format
    abstimmen. 2) Colormaps über `assets.rs` laden; fehlen sie, gelten die
    Konstanten wie heute. 3) Der Mesher mischt pro Vertex über die Blend-Box.
    4) Option "Biome Blend" in den Einstellungen.
  - *Dateien:* `meshing/*`, `render/shaders.rs`, `assets.rs`, `settings.rs`,
    `blocks/mod.rs`.
  - *Abhängigkeiten:* Biom-Registry und 3D-Biomdaten; Mesher-Halo mit 26
    Nachbarn (Rendering "Mesher-Korrektheit").
  - *Priorität/Aufwand/Impact:* **P3**, M, hoch (gut sichtbar).
  - *Akzeptanz:* Stufenloser Farbübergang von Wald zu Wüste; die Mesh-Zeit pro
    Job steigt bei Blend-Radius 2 um weniger als 10 %.

## UI / UX

- keep expanding the options UI without reintroducing scattered hard-coded runtime settings

### Aus dem a1.0.17_04-Abgleich

- ~~Hotbar-Zifferntasten `1..9` auf die Slotauswahl legen~~ **done**
  (verifiziert 2026-09-28): `hotbar_slot_for_key` (`input.rs:157-170`) bildet
  `Digit1`…`Digit9` auf die Slots ab, ausgewertet in `app/mod.rs:3257-3267`
  *nach* den gebundenen Aktionen (wer eine Aktion auf eine Zifferntaste legt,
  gewinnt) und nur im Spiel ohne offenes Menü/Inventar. Bewusst nicht als
  `InputAction` umgesetzt, sodass `InputAction::ALL`/`index()`
  (`settings.rs:92-124`) nicht wachsen mussten. Der Caveat in
  `01_status_und_aktueller_stand.md` ist damit gegenstandslos. Die Warnung
  von damals gilt weiter für jede *neue* Aktion (z. B. F1): `InputAction::ALL`
  ist ein Array fester Länge mit handgepflegtem `index()` — beides muss
  mitwachsen, sonst bricht der Settings-Serializer still
- HUD-Ausblenden auf F1. F2 (Screenshot) und F3 (Debug-Overlay) gibt es, F1
  nicht; die Tastenbehandlung für F10-F12 in `app/mod.rs:3389-3429` ist die
  Vorlage
- **F3-Overlay nennt "(F5)" als Wettertaste, belegt ist F11** (neu 2026-09-28)
  - *Ist:* `app/mod.rs:2313` beschriftet die Wetterzeile mit "(F5)"; F5 ist
    `ToggleCameraMode` (`settings.rs:144-160`), der Wetterschalter liegt auf
    F11 (`app/mod.rs:3389-3393`).
  - *Ziel/Vorbild:* Beschriftung stimmt mit der wirksamen Taste überein
    (Vanilla leitet Tastennamen aus der Belegung ab).
  - *Vorgehen:* Text korrigieren; die Debug-Tasten F3-F12 sind nicht
    umbelegbar, eine Ableitung aus der Belegung lohnt erst, wenn sie es werden.
  - *Dateien:* `app/mod.rs`. *Abhängigkeiten:* keine.
  - *Priorität/Aufwand/Impact:* **P3**, S, niedrig.
  - *Akzeptanz:* F3 nennt F11.
- Überlebens-HUD: Herzleiste und Rüstungsleiste (**keine** Hungerleiste, die
  Vorlage hat keine). Hängt an "Survival-Gameplay"
- Todesbildschirm mit Respawn-Knopf. `GameState` (`app/mod.rs:203-233`) nimmt
  einen weiteren Zweig auf; der Pause-Backdrop (`render/pause_backdrop.rs`) ist
  als abgedunkelter Hintergrund wiederverwendbar
- Survival-Inventar-Layout mit 2x2-Crafting und 4 Rüstungsslots. Die Textur ist
  bereits eingebunden und wird gewählt (`InventoryAtlasSource::SurvivalInventory`,
  `render/state.rs:2071-2112`) — es fehlt der Inhalt, nicht das Asset
  (40 Slots gesamt: 36 Inventar + 4)
- Werkbank-Screen (3x3), Kisten-Screen (einzel/doppelt), Ofen-Screen. Alle drei
  brauchen erst das Item-System und die Block-Entity-Persistenz
- Titelbildschirm: animiertes Logo und Splash-Texte. Beides gibt es nicht
  (kein Treffer für `splash`/`logo` im Code). Splashes als einfache Textdatei
  über `AssetSource::read_to_string` laden, damit ein Resourcepack sie
  überschreiben kann
- neue Chat-Kommandos für die Survival-Ära (`/give`, `/kill`, `/summon`) erst
  anlegen, wenn Items bzw. Entities existieren. `COMMAND_DOCS`
  (`commands/mod.rs:13`) und `docs/commands-index-markdown.md` werden vom Test
  `commands_doc_lists_every_registered_command` gegeneinander geprüft — beide
  müssen im selben Commit wachsen

## Performance

- **Referenzmessung 2026-08-20: Chunk-Streaming ~50 FPS (vorher 10-20).** Details und Ursachenkette in `docs/07_performance_rendering_audit.md` → "Nachtrag 2026-08-20". Vor weiterem Streaming-Tuning einen frischen `stall.txt` + `gpu.csv` aufnehmen — alle Messwerte unten stammen von *vor* dieser Runde

### Decouple frame rate from chunk streaming

Frame time must stop tracking streaming activity. Today it does, and not by
accident — the current design permits it. Do **not** "fix" this by lowering the
existing constants: they were raised deliberately, and shrinking them trades
stutter for slow streaming. The fix is to stop letting the stages decide their
cost independently of the frame deadline.

What was verified in the code before writing this item (*Zeilenangaben
aktualisiert 2026-09-28*):

- **The per-stage budgets are additive and none of them knows the frame
  deadline.** Every stage is a separate constant and every stage may spend its
  full budget in the same frame: chunkgen apply 3.0
  (`STREAM_CHUNKGEN_APPLY_BUDGET_MS`, `config/mod.rs:290`), apply follow-ups 1.5,
  saved-column load apply 2.0, streaming lighting 6.0, removal lighting 3.0,
  block-edit lighting 1.5, changed-chunk dirty apply 0.5, mesh dispatch 2.0,
  mesh upload 3.0 (5.5 low-speed, 10.0 warmup), prune 1.0, evict 1.5, block-edit
  side effects 1.5, fluid sim 3.0. That sums to **~30 ms before `render` even
  starts** — a 60 Hz frame is 16.6 ms. The engine is currently *specified* to
  drop to ~30 fps whenever every stage has work, which is the definition of
  "chunks are generating".
- **Burst mode widens the budget exactly when the frame is already heavy.**
  `update_stream_burst_mode_for_camera` (`state.rs:11206`) turns burst on when
  meshes near the camera are missing, and `stream_budget_for_stage_ms`
  (`state.rs:11223`) then hands out up to `STREAM_BURST_MAX_TOTAL_BUDGET_MS =
  25.0` across three stages alone. That shared cap is the right shape — it is
  just capped too high, applies only in burst, and covers only 3 of ~12 stages.
- **There is no frame-time feedback anywhere.** No budget in `render/state.rs`
  or `app/mod.rs` reads a measured frame time; all of them are compile-time
  constants. Nothing widens the budget on a cheap frame or narrows it after an
  overrun.
- **Three worker pools each spawn `available_parallelism()` threads**
  (`mesh_worker.rs:51-54`, `chunkgen_worker.rs:3159-3162`,
  `lighting_worker.rs:28-31`), all at default OS priority. On an 8-thread CPU
  that is up to 24 runnable workers competing with the one thread that has to
  simulate *and* render. *Korrektur 2026-09-28:* Die Lighting-Worker bekommen
  nie Arbeit (siehe "`LightingWorkerPool`: anschließen oder entfernen") und
  blockieren im Channel-`select!`; lauffähig konkurrieren also bis zu 2 × N
  Worker (Chunkgen + Mesh) plus Storage-Reader/-Writer — bei 8 Threads 16,
  nicht 24. Chunkgen-Worker können sich zusätzlich gegenseitig im
  Column-Koordinator blockieren (Mutex + Condvar,
  `chunkgen_worker.rs:2389-2398`). A main thread that respects its budget
  perfectly still loses wall-clock time here, so budget work alone cannot close
  this.
- **The world can only be mutated on the frame thread.** `World.chunks` is a
  plain `HashMap<ChunkCoord, Chunk>` (`world/mod.rs:39`) owned by `State`
  (`state.rs:5464`), so applying a generated chunk needs `&mut World` inline.
  This is the structural reason apply, lighting hooks and fluid bootstrap cannot
  move off the frame thread today.
- **GPU resources are created inside the frame.** `upload_cpu_mesh_to_gpu_reuse`
  (`mesh_gpu.rs:202`; Terrain-Pfad `upload_world_cpu_mesh_to_gpu_reuse`, `:290`) calls `device.create_buffer` + `queue.write_buffer` on the
  frame thread; buffer creation is a driver allocation whose cost lands *after*
  the budget check that admitted it.

Work items, in the order they should be attempted:

1. **Bound per-item overshoot first** (already listed below as "stop a single
   item from blowing a stage budget"). *Stand 2026-08-20: der Watchdog meldet
   solche Items jetzt aggregiert pro Frame (`src/perf/stage_budget.rs`), und der
   gemessene Worst Case (Removal-Lighting, 8-19 ms pro Chunk) ist gestuft und
   damit weg. Das Muster selbst bleibt: jede Stage kann weiterhin um einen
   begrenzten Step überziehen.* A frame-level budget is only as tight as
   its worst single item, and the measured worst items are far above any frame
   budget: removal lighting was 8-19 ms *per chunk* while chunk apply is
   0.51 ms. Until one item cannot exceed its stage, a frame controller cannot
   hold a deadline.
2. **Replace the independent constants with one frame-level admission
   controller.** Derive the frame's total streaming allowance from the actual
   deadline (present mode is `Fifo` by default, see `preferred_present_mode`,
   `state.rs:10667`), subtract the measured cost of the parts that are not
   optional (sim, render, present), and hand each stage a *share of what
   remains* instead of its own constant. Generalise `stream_budget_for_stage_ms`
   into that single allowance and route every stage through it. Give each stage
   a guaranteed floor so streaming cannot starve — the existing
   `if iterations > 0 && elapsed >= budget` pattern already guarantees one item
   of progress per stage, so the floor is free.

   **Teilweise umgesetzt (2026-08-20), bisher nur für Lighting:**
   `LightingFrameBudget` (`src/render/lighting_budget.rs`) ersetzt die drei
   privaten Lighting-Budgets (1.5 + 6.0 + 3.0 = 10.5 ms) durch **ein** geteiltes
   7-ms-Budget, vergeben in Prioritätsreihenfolge Block-Edit → Chunk-Add →
   Chunk-Remove. Jede Stage behält ihren alten Wert als Cap, hält eine Reserve
   für die Stages hinter sich zurück und hat einen Starvation-Floor von 0.25 ms.
   Das ist die Form, die dieser Punkt für *alle* Stages verlangt — die Vorlage
   steht, die Allowance ist aber weiterhin eine Konstante statt aus dem Frame-
   Deadline abgeleitet (das bleibt Punkt 3). Offen für die nächste Messung: ob
   7.0 ms der richtige Wert ist und ob der Removal-Backlog unter Dauer-Unload
   mit dem Floor auskommt.
3. **Close the feedback loop.** Feed the previous frames' measured
   `frame_dt_ms` (already written per frame to `gpu.csv`) into the allowance as
   an EMA: shrink after overruns, grow while frames sit under the deadline.
   This is what makes the result hold on hardware that was never measured, and
   it is why the constants must go rather than be retuned.
4. **Measure and fix CPU oversubscription.** Size the three pools against one
   shared thread total (or a single shared pool) instead of each taking
   `available_parallelism()`, and/or drop worker priority below the frame
   thread. Gate on measurement: run a fixed scene, compare per-stage main-thread
   scope times with pools saturated vs. capped. If the main thread's own scope
   times inflate while its item counts stay constant, the loss is OS scheduling
   and no budget change will recover it.
5. **Move GPU buffer creation out of the frame.** Do the arena suballocation
   item below first (it removes most allocations outright); afterwards decide
   whether the remaining create+write should move to an upload thread — wgpu 28
   `Device`/`Queue` are cheap to clone and `Send`, so a worker can hand finished
   `ChunkMesh` values to the frame thread.
6. **Only then consider making world mutation concurrent** (per-chunk locks, or
   a double-buffered insert queue swapped once per frame). This is the large,
   risky change; steps 1-5 must fail to close the gap before it is justified.

Related items already listed below that are part of this problem, not separate
from it: the mesh-job priority inversion, the mesh GPU suballocation, moving
incremental lighting onto the idle `LightingWorkerPool`, stepping chunk-removal
lighting, and the per-item stage watchdog.

Acceptance: a fixed scene flown at constant speed through ungenerated terrain,
with `gpu.csv` captured for the whole run. The metric is **p99 `frame_dt_ms`
while streaming vs. p99 while standing still in already-loaded terrain** — an
average hides exactly the spikes that are the complaint. Record applied
chunks/second in the same run, otherwise a "fix" that simply streams slower
looks like a win. The startup burst path stays exempt: it runs behind the
loading screen, where throughput legitimately outranks frame time.

### Other performance work

- finish the streaming-lag hunt — the open half of the reported symptom. Measured after the fixes in `07_performance_rendering_audit.md`: streaming frames are median **10.6 ms / p90 53.1 ms**, and `state.process_dirty_chunk_meshes` alone is **mean 12.9 ms**, ~60 % of a 21 ms frame. Chunk apply is no longer the cost (104 ms → 0.51 ms per applied chunk). The p90 spikes are what feels laggy, and they never cross the 200 ms stall threshold, so no stall report ever contained them. Open: where the remaining ~9 ms/frame inside `process_dirty_chunk_meshes` goes. The `gpu.csv` breakdown columns answer this from one normal play session — read them before changing anything else here
- document reproducible startup and streaming benchmark scenes as a baseline
- measure startup warmup and first-playable-frame timing under fixed scenes
- measure ore-pass, preview-cache, and loading-screen warmup costs before deeper optimization work
- measure preview cache growth and decide whether eviction/prewarming is needed
- continue consolidating legacy worldgen/storage paths that still duplicate logic
- preset-specific worldgen tuning with snapshot tests
- refine additional upload/dispatch metrics in the debug overlay
- ~~give the stall ring buffer a useful window~~ **done**: the flat `RING_MAX_EVENTS = 8192` cap is gone. `perf/trace.rs` now evicts by retention time with a 16 MB byte budget as the backstop (`MC_KLON_TRACE_RING_MB` overrides), which holds >10 s at the measured ~9.1 k events/s. Thread labels are `Arc<str>` (one allocation per thread instead of a `String` clone per event) and `push_ring` no longer re-reads the clock. The stall report header prints the **actual** covered span, event count and `dropped_by_budget`, so a truncated window is no longer indistinguishable from a complete one
- ~~the stall summary's coarse `tops=[…]` list aggregates by subsystem counter~~ **done**: die verbleibenden Buckets wurden geprüft. Gefunden: `stream` (`enqueue_chunks_within_view_ms`) enthält die Saved-Column-Requests, die `saved_load` bereits zählt, und `chunkgen`/`unload` können inline ausgelöste Lighting-Zeit enthalten. `render/stall_attribution.rs` baut die Liste jetzt aus **exklusiver** Zeit (Bucket meldet, was er an andere Buckets verrechnet hat) und hängt `attrib_overflow=+X.Xms/Y.Yms` an, wenn die Buckets mehr beanspruchen als die Stage, die sie enthält — d. h. noch nicht deklarierte Verschachtelung fällt künftig auf, statt das Ranking still zu verzerren. Das Sekundenfenster (`push_frame`) rechnet nach derselben Regel
- complete ticking-area workflows (radius/flags/UX) beyond the shortcut baseline
- ~~mesh-job priority inversion~~ **done**: `render/mesh_dispatch.rs` deckelt die Jobs, die gleichzeitig bei den Mesh-Workern liegen, auf `worker_count * 3` (Warmup: `* 8`, Minimum 4) — der Rest bleibt in der jeden Frame umsortierten Dirty-Queue, wo Priorität noch etwas bedeutet. Gezählt wird bewusst nicht `inflight.len()` (das enthält auch fertige Ergebnisse in der Upload-Queue, die ohnehin pro Frame umsortiert wird), sondern ein eigener Zähler `mesh_jobs_outstanding`: hochgezählt beim Senden, heruntergezählt beim Empfangen des Ergebnisses. Der Starvation-Einwand ist damit adressiert statt geraten: der Cap skaliert mit den Worker-Threads, jeder Worker hat also einen Job plus mindestens einen dahinter, und das Drain-Limit (64/Frame) liegt weit über dem Cap. Stall-Report zeigt `inflight_jobs=n/cap,cap_hits=k`. Offen für die Messung: ob 3 Jobs pro Worker der richtige Faktor ist — zu niedrig lässt Worker leerlaufen, zu hoch stellt die Inversion wieder her
- **Chunk-Mesh-GPU-Suballokation plus geteilter Quad-Indexpuffer** (neu
  formuliert 2026-09-28; der bisherige Punkt "chunk mesh GPU suballocation" ist
  vollständig enthalten)
  - *Ist:* Bis zu 6 `wgpu::Buffer` pro Sektion (3 Layer × Vertex + Index,
    `render/mesh_gpu.rs:96-109`); `grown_capacity` rundet auf
    `next_power_of_two` mit Minimum 256 (`:154-156`) — bis zu 2× VRAM-
    Verschnitt; `create_buffer` im Frame, sobald die Daten nicht mehr passen
    (`:307-363`, Legacy-Pfad `:219-276`). Die Indizes sind `u32` und bei Quads
    immer dasselbe Muster (0,1,2,2,3,0 + 4k), werden aber pro Sektion erzeugt,
    hochgeladen und gebunden. CPU-Geometrie wird nach dem Upload verworfen statt
    recycelt.
  - *Ziel:* Wenige große Buffer (Arena) statt Tausender kleiner; kein
    Index-Upload für Opaque/Cutout; keine Buffer-Erzeugung mehr im Frame.
  - *Vorbild:* Sodium (Buffer-Arena pro Region aus mehreren Sektionen,
    geteilter Quad-Indexpuffer). Übertragbar: wgpu erlaubt `draw_indexed` mit
    `base_vertex` in einem gemeinsamen Buffer; die Arena ist ein einfacher
    Free-List-Allokator auf der CPU-Seite.
  - *Vorgehen:* 1) Ein statischer, einmal hochgeladener Quad-Indexpuffer für
    Opaque/Cutout; Transparent behält eigene Indizes (wegen Sortierung).
    2) Vertex-Arena pro Layer mit Free-List in Größenklassen; den vorhandenen
    Reuse-on-Fit-Pfad beibehalten. 3) Draws per `base_vertex` + `index_count`
    in die gemeinsamen Buffer; Bind-Wechsel nur noch pro Arena. 4) Messen:
    Buffer-Erzeugungen pro Sekunde (Audit §6.6), VRAM, `cpu_render_ms`.
  - *Dateien:* `render/mesh_gpu.rs`, `render/state.rs` (Draw-Loops
    `:20406-20489`, Upload `:18478-18592`), `meshing/mod.rs`.
  - *Abhängigkeiten:* Voraussetzung für "GPU-driven/indirect submission",
    "Kompaktes Terrain-Vertex-Format" und Option B von "Smooth Lighting";
    Schritt 5 des Decouple-Punkts.
  - *Priorität/Aufwand/Impact:* **P3**, M, mittel (weniger Stalls durch
    Buffer-Erzeugung, halb so viele Buffer, halber Index-Speicher).
  - *Akzeptanz:* Beim Streamen nahezu keine `create_buffer`-Aufrufe mehr; VRAM
    für Terrain sinkt; der Stage-Budget-Watchdog meldet keine Upload-Overruns
    durch Buffer-Erzeugung.
- **`LightingWorkerPool`: anschließen oder entfernen** (neu formuliert
  2026-09-28; enthält den bisherigen Punkt "move incremental lighting onto the
  existing `LightingWorkerPool`")
  - *Ist:* Der Pool spawnt `available_parallelism()` Threads
    (`render/lighting_worker.rs:28-31`) und bekäme volle Snapshots aller Chunks
    (`world/mod.rs:2928-2942`), wird aber **nie gefüttert**:
    `schedule_lighting_rebuild_if_needed` kehrt sofort zurück, solange
    `lighting_request_pending` false ist (`render/state.rs:17742`), und das
    Flag wird nur im Fehlerzweig *innerhalb* dieser Funktion auf true gesetzt
    (`:17768`; initial false `:10599`, Reset `:6136`, `:17774`). Der Pfad ist
    also toter Code, nicht "idle" (daher `light_rx=0` in jedem Log).
    Inkrementelles Lighting läuft komplett auf dem Frame-Thread, inzwischen
    unter dem geteilten 7-ms-Budget (`render/lighting_budget.rs`,
    `LIGHTING_FRAME_BUDGET_MS`, `render/state.rs:1274`) statt in
    1,5-ms-Scheiben. Das ist die strukturelle Hälfte des Lighting-Backlogs; die
    Budgets wurden nur umverteilt.
  - *Ziel:* Lichtberechnung ohne Frame-Thread-Kosten — oder, falls das vorerst
    nicht kommt, keine N toten Threads und kein toter Code.
  - *Vorbild:* Starlight/ScalableLux rechnen Licht auf eigenen Threads.
    Übertragung ohne Sperren: der Worker bekommt eine Region aus
    `Arc`-Snapshots (Blöcke + aktuelles Licht, wie der Mesher) und liefert neue
    Licht-Arrays zurück; der Frame-Thread übernimmt sie nur, wenn die
    Chunk-Revisionen noch stimmen (dasselbe Muster wie bei Mesh-Ergebnissen,
    `render/state.rs:18515-18564`). Das berührt die Konsistenz von
    Weltzustand und Snapshots — eine Nebenläufigkeitsänderung, kein Tuning.
  - *Vorgehen:* 1) Sofort (a): Pool und toten Pfad entfernen. 2) Später (b),
    nach "Lighting nach Starlight-Vorbild": Region-Job = Chunk + 6 Nachbarn als
    Snapshots, Ergebnis = Licht-Arrays + wirklich geänderte Chunks,
    Revisions-Check beim Übernehmen; Block-Edit-Reparaturen bleiben auf dem
    Frame-Thread (klein, latenzkritisch).
  - *Dateien:* `render/lighting_worker.rs`, `render/state.rs`
    (`:17671-17782`), `world/mod.rs`.
  - *Abhängigkeiten:* (b) hängt an "Lighting nach Starlight-Vorbild" und an
    Schritt 4 des Decouple-Punkts (Thread-Budget).
  - *Priorität/Aufwand/Impact:* (a) **P2**, S, niedrig bis mittel;
    (b) **P3**, L, hoch.
  - *Akzeptanz:* (a) keine `lighting-worker-*`-Threads mehr, Tests grün;
    (b) der `lighting`-Bucket im Stall-Report bleibt beim Streamen unter
    1 ms/Frame, und ein Vergleichstest Worker- vs. Frame-Ergebnis ist
    bitgleich.
- ~~step the chunk-removal lighting like the added path~~ **done**: `pump_removed_lighting_batches` no longer calls the one-shot `lighting_on_chunks_removed`. A batch becomes a `RemovedLightingJob` holding an `IncrementalLightingTask` (`World::begin_incremental_chunk_removed_lighting_task`, sharing its neighbor-region walk with `LightEngine::on_chunks_removed` so both relight the same region) and is stepped under the same voxel/wall-clock limits as the added path, resuming across frames. Backlog counters and the teardown path count the in-flight job via `removed_lighting_backlog_len()`
- ~~stop a single item from blowing a stage budget~~ **done** (reporting half): `perf/stage_budget.rs` aggregates per frame which single items cost a stage its whole budget (count, worst item + label, summed overrun), reported worst-first into the trace ring every frame, into the console at most every 5 s, and appended to the stall report as `stage_budget_overruns[…]`. Wired into the three lighting step loops and the chunk-mesh upload loop — i.e. only where the per-item duration was already measured, so no new timing calls on hot paths. Splitting an offending item stays a per-stage fix; the watchdog only makes it visible
- **Thread-Modell festlegen: Pools pro Stage oder ein geteilter Pool** (neu
  formuliert 2026-09-28; enthält den bisherigen Hinweis "note re:
  multithreading")
  - *Ist:* Worker sind `std::thread` + crossbeam-channel pro Stage in
    `src/render/*_worker.rs`: Chunkgen, Mesh und Lighting mit je
    `available_parallelism()` Threads (`render/chunkgen_worker.rs:3159-3162`,
    `render/mesh_worker.rs:51-54`, `render/lighting_worker.rs:28-31`), dazu je
    ein Storage-Reader und -Writer (`storage/mod.rs:393`, `:496`) und optional
    der Trace-Writer. `rayon = "1.10"` steht in `Cargo.toml:26`, wird aber
    **nirgends** benutzt (Suche nach `rayon|par_iter` leer). Keine
    Thread-Prioritäten.
  - *Ziel:* Vor weiteren parallelen Stages entscheiden, welches Modell
    langfristig gilt; eine feste Obergrenze lauffähiger Worker (Kerne minus 1
    für den Frame-Thread) mit Vorrang für sichtbare Arbeit.
  - *Vorbild:* C2ME und Sodium: ein Worker-Pool mit priorisierter Job-Queue
    statt Pools pro Stage. Übertragung: entweder `rayon::ThreadPool` mit einer
    eigenen Prioritäts-Queue davor oder die bestehenden Pools mit einem
    gemeinsamen Thread-Budget. Java-Spezifisches (ForkJoin-Tuning) ist nicht
    relevant.
  - *Vorgehen:* 1) Mit Schritt 4 des Decouple-Punkts messen (gesättigt vs.
    gedeckelt). 2) Bei Befund ein gemeinsames Thread-Budget, z. B. Chunkgen
    `max(1, (N-1)/2)`, Mesh den Rest; Lighting-Pool entfernen (siehe oben).
    3) `rayon` entweder als geteilten Pool einsetzen oder aus `Cargo.toml`
    streichen.
  - *Dateien:* `Cargo.toml`, `render/*_worker.rs`, `render/state.rs`
    (`create_runtime_worker_services`, `:5917`).
  - *Abhängigkeiten:* Schritt 4 des Decouple-Punkts.
  - *Priorität/Aufwand/Impact:* **P2**, S (Deckel) bis M (geteilter Pool),
    mittel.
  - *Akzeptanz:* p99 `frame_dt_ms` beim Streamen sinkt oder bleibt bei gleichem
    Durchsatz (Chunks/s) — Messmethode wie im Decouple-Punkt.
- **Lighting-bedingte Remeshes nur bei tatsächlich geändertem Licht** (neu
  2026-09-28)
  - *Ist:* `commit_incremental_lighting_result` (`render/state.rs:16593`)
    markiert **jeden** betroffenen Chunk als lighting-dirty
    (`queue_lighting_changed_chunk_dirty`, `:16626-16627`) und zusätzlich jeden
    geladenen Nachbarn außerhalb dieser Menge als boundary-dirty
    (`:16628-16637`). Die Chunk-Add-Aufgabe umfasst Chunk + 6 Nachbarn und
    setzt deren Licht zuerst auf 0 (`recompute_region_and_propagate`,
    `world/mod.rs:2117`); ob sich die Endwerte geändert haben, prüft niemand.
    Pro gestreamter Sektion entstehen so bis zu 7 Lighting-Remeshes plus ein
    Ring Boundary-Remeshes. Bei gemessenem `process_dirty_chunk_meshes` ≈ 60 %
    eines Streaming-Frames (Punkt "finish the streaming-lag hunt") liegt hier
    ein naheliegender Anteil — **gemessen ist er nicht**.
  - *Ziel:* Remesh nur für Chunks, deren Licht sich im Inneren (für den eigenen
    Mesh) bzw. in der Randschicht (für Nachbar-Meshes) wirklich geändert hat.
  - *Vorbild:* Starlight und Vanilla melden aus der Propagation nur Sektionen
    mit echten Wertänderungen. Ohne Einschränkung übertragbar.
  - *Vorgehen:* 1) Messen: Remeshes pro Sekunde nach Grund (Labels
    `lighting`/`boundary`/`geometry` aus `MeshDirtyReasons`,
    `render/state.rs:518-535`) als Spalten in `gpu.csv`, Flug-Szene aus dem
    Decouple-Punkt. 2) Vor der Task pro Chunk einen Hash (oder eine Kopie) der
    Licht-Arrays halten und danach vergleichen; die 6 Randschichten
    (je 256 Byte) getrennt vergleichen. 3) Nur geänderte Chunks
    lighting-dirty, Nachbarn nur bei geänderter Randschicht boundary-dirty.
  - *Dateien:* `render/state.rs` (`:16593-16665`), `world/mod.rs`
    (`IncrementalLightingTask`, `:1337-1354`).
  - *Abhängigkeiten:* keine; wird mit Option B von "Smooth Lighting" hinfällig.
  - *Priorität/Aufwand/Impact:* **P1**, S-M, potenziell hoch (erst messen,
    dann fixen).
  - *Akzeptanz:* In der Flug-Szene sinken die Remeshes pro Sekunde mit Grund
    `lighting`/`boundary` deutlich und der Mittelwert von
    `process_dirty_chunk_meshes` sinkt; kein sichtbarer Lichtfehler
    (Screenshot-Vergleich an Höhlenmund und Überhang).
- **Lighting nach Starlight-Vorbild: Kantenpropagation statt Region-Clear,
  Decrease-BFS fürs Entfernen** (neu 2026-09-28)
  - *Ist:* Sky- und Blocklicht liegen als Nibbles in einem Byte pro Voxel
    (`world/chunk.rs:11`) und werden in getrennten FIFO-BFS-Durchläufen
    propagiert (`LightEngine`, `world/mod.rs:1916-1922`; `propagate_sky`
    `:2343`, `propagate_block` `:2381`) — die Grundform stimmt. Aber: Chunk
    hinzufügen heißt, die Region aus Chunk + 6 Nachbarn zu **löschen**, neu zu
    seeden und komplett zu propagieren (`recompute_region_and_propagate`,
    `:2117`; inkrementell identisch, `IncrementalLightingTask`,
    `:1326-1354`). Jede Sektion wird beim Streamen so bis zu 7-mal neu belichtet
    (einmal selbst, einmal pro Nachbar). Block-Edits nutzen Relaxation
    (`LocalizedLightingRepairTask`, `:877-884`; `recompute_*_at`,
    `:1084-1124`): Licht entfernen konvergiert nur schrittweise, statt in einem
    Durchgang zu löschen. Licht wird nicht gespeichert
    (`storage/chunk_codec.rs:731`), geladene Spalten werden also ebenfalls
    vollständig neu belichtet.
  - *Ziel:* Pro neuer Sektion nur deren eigenes Licht plus der Austausch über
    die Grenzflächen; Entfernen in einem Durchgang; Kosten proportional zur
    Änderung statt zur Region.
  - *Vorbild:* Starlight/ScalableLux: getrennte Increase- und Decrease-Queues
    (Decrease löscht und sammelt Randquellen, Increase propagiert neu),
    Skylight-Seed aus der Heightmap, beim Laden nur Kantenaustausch mit schon
    belichteten Nachbarn, "initialisiert"-Flag pro Sektion. Direkt übertragbar;
    die Byte-Packung Sky/Block bleibt.
  - *Vorgehen:* 1) Messen: Voxel-Schritte und ms pro hinzugefügter Sektion
    (Zähler `lighting_incremental_chunks_touched`, `render/state.rs:16609`) auf
    der Flug-Szene. 2) Decrease-/Increase-BFS für Block-Edits (ersetzt die
    Relaxation). 3) Chunk-Add: Sektion lokal seeden (Sky aus der Heightmap,
    siehe nächster Punkt), dann nur die Randzellen der Nachbarn als Quellen
    einspeisen — kein Löschen der Nachbarn. 4) Chunk-Remove: Decrease an den
    Grenzflächen statt Region-Neuberechnung. 5) Vergleichstest neues Ergebnis
    vs. One-Shot-Ergebnis auf festen Szenen (diese Testform existiert seit dem
    Removal-Stepping).
  - *Dateien:* `world/mod.rs` (LightEngine, Tasks), `render/state.rs` (Pumps
    `:17785-17809`), `render/lighting_budget.rs`.
  - *Abhängigkeiten:* "Persistente Heightmaps"; ergänzt "Lighting-bedingte
    Remeshes"; Grundlage für Option (b) beim `LightingWorkerPool`.
  - *Priorität/Aufwand/Impact:* **P2**, L, hoch (Lighting ist laut Audit der
    Engpass beim Streamen, `docs/07_performance_rendering_audit.md`, Anhang C).
  - *Akzeptanz:* Voxel-Schritte pro hinzugefügter Sektion sinken um mindestens
    50 %; der `lighting`-Bucket im Stall-Report sinkt; Vergleichstests
    bitgleich; eine entfernte Fackel ist innerhalb eines Frame-Budgets dunkel.
- **Persistente Heightmaps pro Spalte; Skylight-Seed ohne Vollscan** (neu
  2026-09-28)
  - *Ist:* Es gibt keine gepflegte Heightmap (die Suche nach
    `heightmap|highest` findet nur Worldgen). `build_skylight_seed_columns_
    for_region` (`world/mod.rs:1925-1964`) iteriert pro Lighting-Task über
    **alle** geladenen Chunks (`for (&coord, chunk) in &world.chunks`,
    `:1935`) und scannt jede passende Sektion komplett
    (`build_snapshot_sky_blocker_top`). `terrain_height_at` scannt die Spalte
    bei jedem Aufruf von oben (`:3345-3354`). Bei RD 16 sind über 10 000
    Sektionen geladen.
  - *Ziel:* O(1)-Abfrage "höchster lichtblockierender Block" und "höchster
    fester Block" pro (x, z) — für Skylight-Seeds, Niederschlag, Schnee-Tick,
    Spawning und Random-Tick-Auswahl.
  - *Vorbild:* Vanilla `Heightmap` (`MOTION_BLOCKING`, `WORLD_SURFACE`,
    `OCEAN_FLOOR`) mit 256 Einträgen pro Spalte, gepflegt in `setBlock`.
    Übertragung: `ColumnHeightmaps { sky_blocker: [i16; 256], surface:
    [i16; 256] }` in einer Map neben `World.chunks`, gepflegt über das
    Welt-Änderungsjournal.
  - *Vorgehen:* 1) Sofort: in `build_skylight_seed_columns_for_region` statt
    über alle Chunks über die benötigten Spalten × Sektions-Y-Bereich mit
    direktem `get` iterieren (16 Lookups pro Spalte). 2) Heightmap-Typ
    einführen und beim Einfügen einer Sektion sowie bei Block-Änderungen
    pflegen. 3) Skylight-Seed, `terrain_height_at`, Winter-Tick und
    Niederschlag darauf umstellen. 4) Optional speichern (getaggter Block im
    Column-Trailer).
  - *Dateien:* `world/mod.rs`, `render/state.rs` (Winter-Tick `:18789`,
    Niederschlag `:19428`).
  - *Abhängigkeiten:* Schritt 1 keine; ab Schritt 2 Welt-Änderungsjournal.
  - *Priorität/Aufwand/Impact:* Schritt 1 **P1**, S; Schritte 2-4 **P2**, M;
    Impact mittel, wächst mit der Sichtweite.
  - *Akzeptanz:* Der Trace-Scope der Region-Task-Erzeugung ist unabhängig von
    der Zahl geladener Chunks (Vergleich RD 8 vs. RD 16); das Lichtergebnis ist
    bitgleich (bestehende Lighting-Tests).
- **Worldgen: 2D-Terrain-Noise pro Spalte statt pro Sektion** (neu 2026-09-28)
  - *Ist:* Die Basis-Terrain-Phase ruft `TerrainGenerator::generate_chunk` für
    jede der 16 Sektionen einer Spalte auf (`render/chunkgen_worker.rs:
    1776-1788`), und jeder Aufruf berechnet die komplette 16×16-Heightmap neu
    (`worldgen/pipeline.rs:157-181`; geglättet 10 `height_at` pro Spalte,
    `density/cache.rs:133-142`) plus 25 Samples für das Density-Grid
    (`:66-68`) — unabhängig von der Sektionshöhe, auch für reine
    Luftsektionen. Das sind rund 45 000 `height_at`-Aufrufe pro Spalte mit je
    ~21-23 Fbm-Oktaven (`worldgen/terrain.rs:156-234`), wo ein Sechzehntel
    reichen würde. Außerdem pro Spalte zwei `TerrainSampler`
    (`chunkgen_worker.rs:1762`, `:1776`) und eine Neuberechnung aller Seen und
    Quellen der 7×7 Quellchunks ohne spaltenübergreifenden Cache (`:413-451`).
  - *Ziel:* Jede 2D-Größe einmal pro Spalte; Luft- und Vollstein-Sektionen
    ohne Per-Voxel-Arbeit.
  - *Vorbild:* Vanilla `NoiseChunk` (`cache_2d`/`flat_cache` für y-unabhängige
    Dichteknoten), C2ME (Noise-Caches). Direkt übertragbar und Voraussetzung
    für Multi-Noise, wo die 2D-Knoten (Kontinentalität, Erosion) noch teurer
    sind.
  - *Vorgehen:* 1) Messen: `ColumnGenerationStageTimings::base_terrain_fill`
    (`chunkgen_worker.rs:1859`) über 200 Spalten, vorher und nachher.
    2) `WorldgenPipeline` bekommt `generate_column(cx, cz, out)`, das die
    Heightmap einmal füllt und die Sektionen daraus schreibt; Sektionen
    oberhalb der Spalten-Maximalhöhe werden direkt `ChunkBlocks::Uniform(Air)`.
    3) Einen `TerrainSampler` pro Spalte teilen. 4) Seen-/Quell-Kandidaten pro
    Quellchunk in einem kleinen LRU cachen (wie den Column-Cache, `:85`).
  - *Dateien:* `worldgen/pipeline.rs`, `worldgen/terrain_generator.rs`,
    `worldgen/density/cache.rs`, `render/chunkgen_worker.rs`.
  - *Abhängigkeiten:* keine; vor "Multi-Noise-Terrain".
  - *Priorität/Aufwand/Impact:* **P1**, S-M, hoch für den Chunkgen-Durchsatz
    (Worker-Zeit, nicht Frame-Zeit — wirkt über weniger CPU-Konkurrenz und
    schneller gefüllte Sichtweite).
  - *Akzeptanz:* `base_terrain_fill` pro Spalte sinkt deutlich (erwartet: mehr
    als 5×); der Determinismus-Test `trees_are_deterministic_for_same_seed_
    and_column` bleibt bitgleich.
- **Chunkgen-Jobs spaltenweise statt sektionsweise** (neu 2026-09-28)
  - *Ist:* Pro Sektion ein `ChunkGenJob` (`render/state.rs:12378-12400`); der
    Worker erzeugt die ganze Spalte, legt sie in einen LRU-Cache (384 Spalten,
    `chunkgen_worker.rs:85`, `:2119-2161`) und extrahiert eine Sektion. Andere
    Worker, die dieselbe Spalte anfragen, warten per Condvar (`:2389-2398`);
    die LRU-Pflege ist O(n) unter Mutex (`VecDeque::position`, `:2156`,
    `:2183`); jede Extraktion klont `fluid_bootstrap` (`:1972`).
  - *Ziel:* Keine blockierten Worker, weniger Channel-Nachrichten und
    Lock-Verkehr.
  - *Vorbild:* C2ME (Scheduling nach Spalte/Region, keine gegenseitig
    wartenden Worker). Übertragbar.
  - *Vorgehen:* 1) Messen: Anzahl und Dauer der Condvar-Waits pro Sekunde
    (Trace-Zähler). 2) Job = Spalte, Ergebnis = alle Sektionen + Bootstrap; der
    Main-Thread wendet weiter sektionsweise unter dem bestehenden Apply-Budget
    an. 3) Den Koordinator-Cache auf "Warmup/LOD teilen eine Spalte"
    reduzieren.
  - *Dateien:* `render/chunkgen_worker.rs`, `render/state.rs`
    (`request_chunk_generation`, `pump_chunkgen_results` `:13639`).
  - *Abhängigkeiten:* "2D-Terrain-Noise pro Spalte".
  - *Priorität/Aufwand/Impact:* **P3**, M, mittel — nur, wenn die Messung
    relevante Wartezeiten zeigt.
  - *Akzeptanz:* Condvar-Waits nahe 0; Chunks/s beim Streamen gleich oder
    höher.
- **Schnellerer Hasher und chunk-lokale Lookups in heißen Pfaden** (neu
  2026-09-28)
  - *Ist:* Alle Maps und Sets nutzen std-SipHash (weder `ahash` noch
    `rustc-hash` im Projekt). `World::get_block` macht pro Aufruf `div_euclid`
    und einen Hash-Lookup in `HashMap<ChunkCoord, Chunk>` (`world/mod.rs:39`,
    `:2855-2865`), ebenso `get_light_packed` (`:2887`). Heiße Nutzer:
    Block-Edit-Reparatur, Fluid-Sim, Spieler-Kollision, Sand-Check, Random
    Ticks. Auch der Corner-Cache der Flüssigkeiten ist eine SipHash-
    `HashMap<IVec3, _>` pro Zelle (`meshing/liquids.rs:14-85`).
  - *Ziel:* Weniger Zeit in Hashing und Lookups, ohne Strukturumbau.
  - *Vorbild:* Lithium (Block-Lookups über gecachte Sektionsreferenzen), rustc
    (`FxHash` für kleine Integer-Keys). In Rust idiomatisch: `rustc-hash` oder
    `ahash` als `BuildHasher`, dazu ein `WorldRegion`-Cache (3×3×3
    Chunk-Referenzen) für Nachbarschafts-Algorithmen.
  - *Vorgehen:* 1) Messen mit `cargo flamegraph` bzw. `perf record` auf der
    Flug-Szene und einer Fluid-Szene: Anteil von Hashing/`HashMap::get`.
    2) Bei Befund `type FastMap<K, V> = HashMap<K, V, FxBuildHasher>` für
    `World.chunks`, Lighting- und Dirty-Sets. 3) `WorldRegion` für
    Lighting-Reparatur und Fluid-Nachbarschaft. 4) Den Liquid-Corner-Cache auf
    ein dichtes 17×17-Grid pro Sektionsebene umstellen.
  - *Dateien:* `Cargo.toml`, `world/mod.rs`, `world/fluids.rs`,
    `meshing/liquids.rs`, `render/state.rs`.
  - *Abhängigkeiten:* keine.
  - *Priorität/Aufwand/Impact:* **P2**, S-M, mittel (die Messung entscheidet).
  - *Akzeptanz:* Hash-Anteil im Flamegraph halbiert; in der Fluid-Szene sinkt
    `sim.fixed_tick.fluids` im Trace.
- **Release-Profil: LTO und Codegen-Units** (neu 2026-09-28)
  - *Ist:* `Cargo.toml` hat kein `[profile.release]` — Default-Codegen-Units
    und kein LTO über Crate-Grenzen (wgpu, glam, fastnoise-lite, flate2).
    `#[inline]` ist in den Hot Paths ungleich verteilt (`meshing/` 52×,
    `worldgen/noise.rs` und `worldgen/terrain*.rs` 0×; `Chunk::index`/`get`
    nicht inline, `world/chunk.rs:117`, `:125`) — mit einer Codegen-Unit und
    LTO entscheidet das der Compiler ohne Handpflege.
  - *Ziel:* Durchsatzgewinn in Meshing, Worldgen und Noise ohne Codeänderung.
  - *Vorbild:* Rust-Standardpraxis, kein Minecraft-Pendant.
  - *Vorgehen:* 1) `[profile.release] lto = "thin"`, `codegen-units = 1`;
    `lto = "fat"` vergleichen; `panic = "abort"` nur, wenn nichts auf
    `catch_unwind` setzt. 2) Für schnellere Iteration
    `[profile.dev.package."*"] opt-level = 3`. 3) Messen: Greedy-Report
    (`cargo test --release meshing::greedy::greedy_ao_merge_report --
    --nocapture`), `base_terrain_fill`, Release-Build-Zeit.
  - *Dateien:* `Cargo.toml`.
  - *Abhängigkeiten:* keine.
  - *Priorität/Aufwand/Impact:* **P2**, S, mittel (typisch 5-15 %, zählen nur
    gemessene Zahlen).
  - *Akzeptanz:* Meshing- und Worldgen-Messwerte schneller bei akzeptabler
    Release-Build-Zeit.
- **Rest der Optimierungsrunde abschließen (Pools, SIMD, Allocation-Tracking)**
  (neu 2026-09-28; Verifikation der Runde aus
  `docs/performance_optimizations.md` und `docs/memory_pool_implementation.md`)
  - *Ist — umgesetzt, nicht erneut anfassen:* Grid statt HashMap im
    Density-Cache (`worldgen/density/cache.rs:8-20`); Frustum-Culling vor dem
    Meshing (Defer um 24 Frames, `render/state.rs:18064-18076`; Zähler
    `:5616-5617`); keine `.collect()` in Per-Voxel-Schleifen von Meshing und
    Worldgen; thread-lokaler `MeshingPool` für Greedy-Masken und den
    Liquid-Corner-Cache (`meshing/pool.rs:199-214`, `liquids.rs:524-542`);
    vereinheitlichter Corner-Cache für Flüssigkeiten (`liquids.rs:14-85`).
  - *Ist — unvollständig:* (1) Die Ausgabe-Geometrie ist pro Job ein frisches
    `LayeredChunkGeometry` (`meshing/greedy.rs:987`, `:821`), reserviert für
    **alle** Layer unabhängig von `requested_layers` (`pool.rs:96-126`), und
    wird nach dem Upload verworfen. (2) Die Pool-Felder
    `vertices`/`indices`/`temp_*` (≈ 400 KB pro Thread, `pool.rs:46-53`) nutzt
    die Produktion nicht; `take_mesh_data` und `MeshingPoolStats` sind tot
    (Compiler-Warnungen `pool.rs:70`, `:194`), und `pool_regression_test.rs`
    testet nur diesen toten Pfad. (3) SIMD-UVs: `face_uvs_for_quad_simd` und
    `batch_face_uvs_for_quads_simd` (`greedy.rs:55-153`) rufen nur Tests auf;
    die Produktion nutzt `face_uvs_for_quad_scalar` (`:115-135`), und
    `push_textured_quad_simd` (`:1831-1889`) enthält kein SIMD. Bewertung:
    4 UV-Paare pro Quad lohnen kein SIMD — löschen statt anschließen.
    (4) `allocation_tracking` zählt in Produktion nichts: der einzige Aufruf
    steht im nicht-gepoolten Liquid-Pfad (`liquids.rs:597-598`), den kein
    Produktionsaufrufer erreicht (`:543-551`); es gibt keinen
    `GlobalAlloc`-Hook. (5) Drei eager `format!` pro Mesh-Job auch bei
    abgeschaltetem Tracing (`render/mesh_worker.rs:92`, `:104`, `:143`), ebenso
    im Chunkgen-Koordinator (`chunkgen_worker.rs:2294-2421`). (6) Textur-Laden:
    noch mehrere `String`- und RGBA-Klone (`render/state.rs:9154-9335`,
    `texture_registry.rs:62-71`) — nur beim Start, daher niedrig.
  - *Ziel:* Die Runde ist entweder vollständig oder bewusst zurückgebaut;
    Allokationen pro Mesh-Job sind messbar.
  - *Vorbild:* Sodium (Mesh-Puffer gehen an die Worker zurück).
  - *Vorgehen:* 1) `allocation_tracking` zu einem zählenden
    `GlobalAlloc`-Wrapper umbauen (Allokationen/Bytes pro Thread) und die
    Zähler pro Mesh-Job ausgeben. 2) Messen. 3) Geometrie nur für angeforderte
    Layer reservieren, Kapazität aus der letzten Mesh-Größe der Sektion
    schätzen, Vecs nach dem Upload per Channel an den Worker-Pool zurückgeben.
    4) Tote Pool-Felder, SIMD-UV-Code und den alten Regressionstest entfernen
    bzw. auf den Greedy-/Liquid-Pfad umschreiben. 5) `format!` hinter eine
    Tracing-Abfrage bzw. in Closures (`with_counts`-Muster).
  - *Dateien:* `meshing/pool.rs`, `meshing/greedy.rs`, `meshing/liquids.rs`,
    `meshing/pool_regression_test.rs`, `perf/allocation_tracker.rs`,
    `render/mesh_worker.rs`, `render/chunkgen_worker.rs`.
  - *Abhängigkeiten:* keine; (4) deckt sich mit dem Clippy-Punkt unter
    "Docs / Cleanup".
  - *Priorität/Aufwand/Impact:* **P3**, S-M, niedrig bis mittel.
  - *Akzeptanz:* Im Steady State weniger als 5 Allokationen pro Mesh-Job
    (gemessen); keine `never used`-Warnungen mehr in `meshing/`.
- **Random Ticks: Sektionen ohne tickbare Blöcke überspringen** (neu
  2026-09-28)
  - *Ist:* `RandomTicksSystem` iteriert pro Tick über alle Keys von
    `world.chunks` (`world/random_ticks_system.rs:88-94`) und würfelt 3
    Positionen pro aktiver Sektion (`RANDOM_TICK_SPEED_DEFAULT`,
    `config/mod.rs:346`), auch in Sektionen ohne einen einzigen Block mit
    `random_tick`.
  - *Ziel:* Kosten proportional zu tickbaren Blöcken statt zu geladenen
    Sektionen.
  - *Vorbild:* Vanilla `LevelChunkSection.isRandomlyTicking` (Zähler tickbarer
    Blöcke pro Sektion, gepflegt in `setBlockState`). Direkt übertragbar —
    `Chunk` pflegt schon `filled_voxel_count`/`solid_voxel_count`
    (`world/chunk.rs:13-14`).
  - *Vorgehen:* 1) `random_tickable_count: u16` in `Chunk`, gepflegt in
    `Chunk::set` und bei `compact()`. 2) Sektionen mit 0 überspringen.
    3) Aktive Sektionen als Liste pflegen statt voll zu iterieren.
  - *Dateien:* `world/chunk.rs`, `world/random_ticks_system.rs`.
  - *Abhängigkeiten:* keine.
  - *Priorität/Aufwand/Impact:* **P4**, S, niedrig (steigt mit weiteren
    tickenden Blöcken wie Pflanzen, Eis, Feuer).
  - *Akzeptanz:* Die `random_ticks`-Trace-Zeit pro Tick sinkt bei gleicher
    Callback-Zahl (Zähler `random_ticks_debug_counters`,
    `render/state.rs:14590`).
- **Editierte Sektionen wieder kompaktieren** (neu 2026-09-28)
  - *Ist:* Die erste Änderung wandelt eine paletted/uniform Sektion dauerhaft
    in `ChunkBlocks::Flat` (4096 Byte; `make_flat_mut`,
    `world/chunk.rs:403-411`); paletted sind gemessen ≈ 403 Byte im Mittel
    (`:322-326`). Mit `BlockId: u16` (siehe "Architektur") verdoppelt sich Flat
    auf 8 KiB.
  - *Ziel:* Geringer Speicher auch in bebauten Gebieten.
  - *Vorbild:* Vanilla `PalettedContainer` (bleibt paletted, die Palette
    wächst); FerriteCore (Speicher zählt). Übertragung: Flat als schnelle
    Arbeitsform behalten, aber beim Speichern, beim Entladen oder nach Edit-Ruhe
    das vorhandene `compact()` (`:344`) aufrufen.
  - *Vorgehen:* 1) Messen: Anteil Flat-Sektionen nach 10 Minuten Bauen.
    2) `compact()` im Save-Snapshot-Pfad (auf dem Worker) oder nach Edit-Ruhe.
  - *Dateien:* `world/chunk.rs`, `render/state.rs`.
  - *Abhängigkeiten:* sinnvoll zusammen mit `u16`-BlockId.
  - *Priorität/Aufwand/Impact:* **P4**, S, niedrig.
  - *Akzeptanz:* Nach Edit-Ruhe fällt der Flat-Anteil auf nahe 0.
- **Licht mitspeichern? — Entscheidung per Messung** (neu 2026-09-28)
  - *Ist:* Der Codec speichert kein Licht (`ColumnSaveSectionSnapshot`,
    `storage/chunk_codec.rs:69-74`); geladene Sektionen bekommen ein frisches
    Null-Licht-Array (`:731`) und durchlaufen denselben Region-Relight wie
    frisch generierte.
  - *Ziel:* Geladene Welten ohne vollen Relight, falls das messbar zählt.
  - *Vorbild:* Vanilla und Starlight speichern Licht (Starlight mit eigener
    Versionsmarke, um nach Algorithmusänderungen neu zu rechnen). Übertragung:
    roh 4 KiB pro Sektion, zlib komprimiert Licht sehr gut.
  - *Vorgehen:* 1) Messen: Anteil Lighting an der Ladezeit gespeicherter
    Spalten (Stall-Attribution `saved_load` vs. `lighting`) und der
    Dateigrößen-Zuwachs mit Licht. 2) Falls lohnend: Licht als getaggter Block
    im Column-Trailer (Storage v3) mit Algorithmus-Version; bei
    Versionswechsel verwerfen und neu rechnen.
  - *Dateien:* `storage/chunk_codec.rs`, `render/state.rs`.
  - *Abhängigkeiten:* Storage v3 (getaggter Trailer); erst nach "Lighting nach
    Starlight-Vorbild", sonst speichert man Werte eines Algorithmus, der sich
    noch ändert.
  - *Priorität/Aufwand/Impact:* **P4**, S (Messung) bzw. M (Umsetzung),
    Impact unbekannt.
  - *Akzeptanz:* Entscheidung mit Zahlen dokumentiert.
- **Bewertet und bewusst nicht als eigener Punkt aufgenommen** (Audit
  2026-09-28, damit diese Konzepte nicht erneut geprüft werden)
  - *Palettierte, bitgepackte Sektionen:* im Speicher schon vorhanden
    (`ChunkBlocks::Uniform/Paletted/Flat`, `world/chunk.rs:332-338`; leere
    Sektionen werden gar nicht gespeichert). `docs/03_java_bedrock_takeaways.md`
    führt das noch als PARTIAL — veraltet. Rest siehe "Editierte Sektionen
    wieder kompaktieren".
  - *Texture Arrays statt Atlas:* umgesetzt (`TextureViewDimension::D2Array`,
    `texture.rs:37`); Greedy-Tiling über `AddressMode::Repeat` ohne
    Atlas-Bleeding (`texture.rs:162-169`).
  - *Dirty-Flags nur für betroffene Sektionen:* umgesetzt, inklusive
    Face-Signatur-Vergleich für Nachbarn (`render/state.rs:15562-15655`);
    Lücke nur bei Diagonalen, siehe Rendering "Mesher-Korrektheit".
  - *Scheduled Ticks als Timing-Wheel:* für Fluids umgesetzt
    (`world/fluids.rs:190-253`); Verallgemeinerung unter "Architektur /
    Block-Verhalten".
  - *ImmediatelyFast (HUD-/Text-Batching):* pro Overlay existiert schon ein
    Mesh (Bitmap-Font, `render/state.rs:4103`); die UI verursacht etwa 10-20
    Draws pro Frame plus einen pro sichtbarem Item-Preview (`:20701-20706`).
    Gegenüber Tausenden Terrain-Draws irrelevant; erst mit Messung aufnehmen.
  - *Entity Culling:* ohne Entities gegenstandslos; Frustum- und
    Distanz-Culling stehen im Entity-Punkt.
  - *Distant Horizons / LOD:* Far-LOD existiert (F6;
    `render/chunkgen_worker.rs:2938-2992`, heightmap-basiert, Vertexfarbe aus
    der Höhe). Ausblick erst nach Multi-Noise: LOD dann aus der Dichtefunktion
    statt aus dem `TerrainSampler`, mit Biomfarbe.
  - *Java-Spezifisches:* GC-Druck, Boxing, Mixins und die Deduplizierung von
    `BlockState`-Objekten (FerriteCore) haben kein Rust-Pendant — Block-States
    sind hier `Copy`-Werte (siehe Block-States).
  - *SIMD für UVs:* lohnt nicht, siehe "Rest der Optimierungsrunde".

## Docs / Cleanup

- keep README, status docs, backlog docs, and worldgen TODOs updated after feature landings. Storage / Content Foundation and Gameplay are closed; their two rejected designs (a compact v2.1/v3 column layout, and merging blocks + block_data into a `BlockStateId`) are recorded with their measurements next to the code, in `storage/chunk_codec.rs` and `world/chunk.rs`, rather than lingering here as open work
- convert important contracts into short code comments near their source modules
- remove stale assumptions whenever a system stops being experimental
- **`cargo clippy --all-targets -- -D warnings` (der Lint-Befehl in
  `CLAUDE.md`) wieder grün machen** (neu formuliert 2026-09-28; der bisherige
  Punkt ist enthalten)
  - *Ist (gemessen 2026-09-28):* Der harte Clippy-Fehler ist weg, aber
    `cargo clippy --all-targets` meldet **120** Warnungen: **26** Dead-Code-
    Warnungen (`never used`/`never constructed`/`never read`) und rund 94
    Stil-Lints — 34× `too_many_arguments` (bis 14 Parameter), 26×
    `collapsible_if`, 6× `manual_is_multiple_of`, 4× `derivable_impls` u. a.;
    49 davon behebt `cargo clippy --fix` automatisch. Die frühere Angabe
    "~102 never used" stimmt nicht mehr. Dead Code im Einzelnen:
    Mesher-Altpfade (`build_chunk_mesh_layered_naive*`,
    `build_chunk_mesh_greedy_layered*`, `meshing/greedy.rs:1340-1563`),
    SIMD-UVs (`:55`, `:139`), `push_textured_quad_scalar` (`:1892`),
    `should_emit_cube_face` (`:175`), `cube_face_vertex_shade_and_light`
    (`:602`), `LOWER_SLAB` (`:375`), Pool-Reste (`meshing/pool.rs:70`, `:194`),
    `fluid_surface_height_at` (`meshing/liquids.rs:171`), `SlabUpper`
    (`blocks/mod.rs:345`), `relative_asset_path_from_key`
    (`render/state.rs:2247`), `storage/region_file.rs:18`/`:136`,
    `world/mod.rs:766`/`:1981`/`:2696`, `enqueue_with_neighbors`
    (`world/fluids.rs:556`), `world/manifest.rs:129`/`:242`,
    `worldgen/carvers/mod.rs:77`.
  - *Ziel:* Der Lint-Befehl ist ein brauchbares Gate.
  - *Vorbild:* — (Hygiene).
  - *Vorgehen:* 1) Pro Dead-Code-Eintrag entscheiden — pauschales Löschen
    würde Gerüste für laufende Arbeit entfernen, pauschales `#[allow]` künftige
    echte Funde verstecken: löschen (Mesher-Altpfade, SIMD-UVs, Pool-Reste,
    siehe Performance "Rest der Optimierungsrunde") oder behalten mit
    `#[allow(dead_code)]` plus Verweis auf den TODO-Punkt, der ihn braucht
    (`SlabUpper` → Blockmodelle, `enqueue_with_neighbors` → Schwamm,
    `on_chunks_removed`/`on_block_changes` → Lighting-Vergleichstests).
    2) Stil-Lints mit `cargo clippy --fix`; `too_many_arguments` pro Funktion
    über einen Parameter-Struct oder ein gezieltes `#[allow]`. 3) Danach
    `-D warnings` in die Doku-Checkliste aufnehmen.
  - *Dateien:* siehe Liste.
  - *Abhängigkeiten:* keine; vor größeren Umbauten, damit neue Warnungen
    auffallen.
  - *Priorität/Aufwand/Impact:* **P2**, S-M, mittel.
  - *Akzeptanz:* `cargo clippy --all-targets -- -D warnings` endet mit Exit 0.
- **Veraltete Aussagen in Status-Dokumenten** (neu 2026-09-28)
  - *Ist:* `docs/03_java_bedrock_takeaways.md` führt "Palette/bitpacking as
    runtime storage" als PARTIAL (im Speicher längst paletted,
    `world/chunk.rs:332-338`) und "Lighting as a separate asynchronous path"
    als DONE (der Lighting-Worker ist toter Code, siehe Performance
    "`LightingWorkerPool`"). `README.md:77` nennt den falschen Default-Preset
    (siehe Worldgen "Preset-Unterschiede dokumentieren"). `CLAUDE.md` nennt
    `cargo test --lib world::raycast::` als Befehl für eine Testdatei; das
    Crate hat kein Library-Target (`error: no library targets found in
    package`), richtig ist `cargo test world::raycast::`.
  - *Ziel/Vorgehen:* Die drei Stellen korrigieren; bei künftigen Audits
    Status-Matrizen gegen den Code prüfen statt fortschreiben.
  - *Dateien:* `docs/03_java_bedrock_takeaways.md`, `README.md`, `CLAUDE.md`.
  - *Abhängigkeiten:* keine.
  - *Priorität/Aufwand/Impact:* **P3**, S, niedrig.
  - *Akzeptanz:* Die Aussagen stimmen mit dem Code überein.

## Historical / Classic Parity

- humans/mob stub for early Classic feel
- fine-tuning of individual UI/gameplay parity details
