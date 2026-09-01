# TODO

Status: 2026-09-01

Single source of truth for open/not-yet-done work. Previously this list was
duplicated across `01_status_und_aktueller_stand.md`,
`02_expanded_roadmap_to_c0.0.13a_03.md`, `03_java_bedrock_takeaways.md`,
`04_content_foundation.md`, `05_gameplay_feature_backlog_1_21_11_alignment.md`,
`performance_optimizations.md`, and `todos_worldgen.md`. Those files now point
here for open work; keep their "Already Implemented" / status sections
separate from this file.

When an item ships, delete it from here instead of marking it done.

## Rendering

- extend the occlusion/AO/selection contract to non-cube shapes: stairs, panes, fences, torches, rails. **Not a rendering fix — a content feature with a prerequisite.** Every one of these needs per-block orientation (stairs 8 states, rails 10, fences 4 connection bits), and the engine has no block-state system: `BlockStateId` is a plain alias for `BlockId`. The 8 bits of per-voxel `block_data` exist but are owned by fluid levels today, so the first step is deciding whether geometry state shares them or gets its own storage. After that each shape needs a `BlockModelSpec` variant, per-block (non-greedy) meshing, an `OcclusionShape`, a `CollisionShape`, textures, placement logic and an inventory entry. Meanwhile the contract itself is guarded: `blocks::interaction_shape_tests::every_model_states_its_raycast_shape` matches exhaustively on `BlockModelSpec`, so a new non-cube model fails to compile until its shapes are stated
- **visual pass still owed** on the skylight change: `blocks_skylight` is gone, replaced by `SkylightSpec` deriving from `OcclusionShape::boundary_coverage(Face::Down)` with two named exceptions (leaves block without occluding, glass is explicitly transparent to light). This fixed two real leaks — `grass_block` and `stone_slab` were full-coverage shapes that passed skylight straight through — so anything under grass or a slab is now correctly dark where it used to be lit. Terrain lighting changed everywhere; walk a cave mouth, an overhang and a slab roof before trusting it. Guarded by `blocks::tests::skylight_agrees_with_the_occlusion_shape_except_for_named_blocks` and `world::tests::opaque_surface_blocks_no_longer_leak_skylight_downwards`
- decide whether AO-aware greedy merging is worth its cost: implemented behind `MeshingPool::ao_aware_merge` and **measured** (`cargo test --release meshing::greedy::greedy_ao_merge_report -- --nocapture`) at **+36 % quads and +34 % meshing time** on hilly terrain. Default is off, because meshing is already the largest slice of a streaming frame. This is a visual-quality call, not a technical one — flip the flag, look at long walls and cave mouths, and decide
- settle the boundary-ocean aliasing question with the F12 toggle: the analysis is now recorded at `push_ocean_quad` (one texel per block, minification only past ~2000 blocks, and the boundary fog fades the ring out over the same span — the one open case is a camera sitting almost exactly at sea level, where the plane is edge-on). Toggle the ring off while an artifact is on screen; if it stays, the ring is not the cause
- consider GPU-driven/indirect submission only if a draw-call bottleneck is measured and proven. The numbers are now on screen (F3: terrain + LOD draw calls, visible chunks, CPU render ms) and per frame in `gpu.csv` next to `cpu_render_ms`. The gate: if render time does not track draw-call count, indirect submission buys nothing. Everything measured so far points at CPU meshing instead — `process_dirty_chunk_meshes` is ~60 % of a streaming frame
- consider more advanced occlusion approaches only after measurement. Same gate and same instrumentation as the item above; frustum-culling counters are already tracked (`frustum_culling_stats`). Nothing to do until a capture shows submission or overdraw dominating a frame

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

**Das ist der Blocker für ca. 60 % der Liste unten** (Treppen, Zaun, Tür,
Leiter, Fackel, Schiene, Hebel, Knopf, Druckplatte, Redstone-Staub, Schild,
Kaktus/Zuckerrohr-Alter, Schneehöhe). Der Punkt existiert bereits unter
"Rendering" als Formen-/Occlusion-Problem — hier steht die Speicher- und
Contract-Seite, weil daran alle Content-Punkte hängen.

Befund im Code:

- `BlockStateId` ist heute ein reiner Alias auf `BlockId` (`blocks/mod.rs:80`),
  trägt also keinen Zustand.
- `Chunk::block_data` ist bereits **ein Byte pro Voxel**
  (`world/chunk.rs:10`), wird bereits mitgespeichert (`storage/chunk_codec.rs`,
  `COLUMN_PAYLOAD_FORMAT_VERSION = 2`) **und liegt dem Mesher schon im
  gepolsterten Nachbarschaftspuffer vor** (`meshing/neighborhood.rs:11,170`).
- Die Fluid-Bits belegen davon nur `0x0F` (Level), `0x20`, `0x40`, `0x80`
  (`world/fluids.rs:174-177`) — und das **ausschließlich für Fluid-Blöcke**.
  Bit `0x10` ist frei, für Nicht-Fluid-Blöcke ist das ganze Byte frei.

Effizienteste Umsetzung: **kein neuer Speicher, keine Formatänderung.**
`block_data` per Blocktyp umdeuten statt einen zweiten Array anzulegen.

1. Ein Modul `blocks/state.rs` mit typisierten Sichten auf das Byte:
   `Facing4`/`Facing6` (2-3 Bit), `Half` (1 Bit, Slab/Treppe/Tür oben/unten),
   `Open`/`Powered` (je 1 Bit), `RailShape` (4 Bit), `RedstonePower` (4 Bit),
   `Age` (4 Bit für Kaktus/Zuckerrohr/Setzling), `SnowLayers` (3 Bit).
   Jeweils `fn from_data(u8) -> Self` / `fn to_data(self, u8) -> u8` analog zu
   den `fluid_data_*`-Helfern — gleiche Form, damit der Stil konsistent bleibt.
2. `BlockSpec` bekommt ein Feld `state: StateSpec` (Enum: `None`,
   `Facing4 { … }`, `Rail`, `Door`, `Redstone`, `Age { max }`, …). Damit steht
   pro Block an genau einer Stelle, wer die Bits besitzt; ein Test in der Art
   von `every_model_states_its_raycast_shape` kann erzwingen, dass jeder
   `StateSpec` disjunkt zu `FluidKind`-Nutzung ist.
3. `BlockModelSpec` bekommt die neuen Varianten (`Stairs`, `Fence`, `Torch`,
   `Ladder`, `Door`, `Rail`, `FlatOnFloor`, `Sign`). Nicht-Würfel-Modelle
   laufen über den **Nicht-Greedy-Pfad** in `meshing/` — der Greedy-Merger darf
   sie gar nicht erst sehen, sonst verschmelzen unterschiedliche Orientierungen.
   Praktisch: `meshing/plants.rs` ist die vorhandene Vorlage für
   „pro Voxel, eigene Geometrie" und sollte zu einem generischen
   `meshing/models.rs` verallgemeinert werden statt kopiert zu werden.
4. `set_block` braucht eine `set_block_state(pos, block, data)`-Variante, die
   `block_data` mitschreibt und dieselbe Dirty-/Lighting-Invalidierung auslöst.
   Der bestehende Fluid-Pfad zeigt schon, dass das geht.

Reihenfolge: erst 1+2 (reine Datenschicht, testbar ohne Rendering), dann 3 pro
Formfamilie. Jede Formfamilie ist danach ein isolierter, kleiner Change.

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

- Sand/Kies fallen bei fehlender Unterstützung. Heute ohne jede Logik. Effizient
  ohne Falling-Block-Entity: über den vorhandenen **Random-Tick-Pfad**
  (`RandomTickFn`, `blocks/mod.rs:81`, `with_random_tick` `:2007`) plus einen
  Nachbar-Update-Trigger beim Block-Edit — der Block „rutscht" pro Tick eine
  Position nach unten. Optisch schlechter als eine echte Fallanimation, aber
  ohne Entity-System sofort machbar; die Entity-Variante gehört an den
  Entity-Punkt unten.
- TNT: existiert nur als Deko-Block (kein einziger Treffer außerhalb von
  `blocks/mod.rs`). Braucht Zündung + Explosionsradius. Die Explosion selbst ist
  ein Batch-`set_block` über eine Kugel plus Raycast-Dämpfung; wichtig für die
  Performance: **einmal** dirty markieren pro betroffenem Chunk, nicht pro Block
  — der bestehende Block-Edit-Pfad in `render/state.rs` markiert pro Edit.
- Schwamm: existiert nur als Deko-Block. Wasser im 5×5×5 entfernen beim Setzen;
  das ist ein einzelner Aufruf gegen `FluidSystem::enqueue_with_neighbors`.
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

Heute gibt es **keine Entities außer dem Spieler und Block-Entities**. Es gibt
kein Entity-Register, keine Bewegungs-/AI-Systeme, kein Spawning.

Voraussetzungen, die im Code schon stehen und benutzt werden sollten:

- `SimSystem` (`sim/mod.rs`) mit `fixed_tick`, `on_columns_entered/exited` und
  `ActiveSimColumns` — genau der richtige Aufhänger. Registrierung über
  `State::register_sim_system` (`render/state.rs:14522`); `FluidsSystem`,
  `BlockEntitiesSystem` und `RandomTicksSystem` sind die Vorlagen.
- Kollision/Bewegung sind **nicht** an den Spieler gebunden:
  `move_with_collisions_for_height`, `move_character_with_step_up`,
  `snap_to_ground_with_height` (`player/mod.rs:215-400`) nehmen bereits eine
  freie Höhe entgegen. Mob-Bewegung kann dieselben Funktionen benutzen.
- `build_player_model_mesh` (`render/player_model.rs:168`) ist bereits ein
  generischer Box-Modell-Builder mit UV-Rects und Limb-Swing. Mob-Modelle sind
  dieselbe Datenstruktur mit anderen Box-Listen — nicht neu schreiben.

Arbeitsschritte:

1. `src/entities/mod.rs`: `EntityId`, `EntityKind`, ein
   `struct Entity { pos, velocity, yaw, pitch, health, on_ground, … }` und ein
   `EntityStore` mit **Spatial-Index nach `ColumnCoord`** — sonst wird
   „welche Entities sind in aktiven Spalten" pro Tick zu einem linearen Scan
   über alles. `ActiveSimColumns::is_active` ist die vorhandene Abfrage dafür.
2. `EntitiesSystem: SimSystem` mit Bewegung/Schwerkraft/Kollision, danach erst
   AI. Entladen von Spalten entfernt Entities über `on_columns_exited`.
3. Rendering: **ein** Instanz-/Batch-Draw für alle Mobs, nicht ein Draw pro
   Entity. Die Modell-Meshes pro `EntityKind` einmal bauen und cachen, wie es
   der Block-Preview-Cache schon macht.
4. **Persistenz ist ein Blocker**, siehe Storage-Punkt unten.

Inhalt:

- Passiv: Schwein (satteln/reiten), Schaf (Wolle regeneriert, scherbar), Kuh
  (melkbar), Huhn (legt Eier).
- Feindlich: Zombie, Skelett (Fernkampf), Spinne (klettert, tagsüber neutral),
  Creeper (Explosion), Slime (teilt sich), Spider Jockey.
- Spawning: lichtabhängig, nachts, in einem Ring um den Spieler, Cap pro
  geladener Spalte. Der Lichtwert liegt bereits pro Voxel vor
  (`Chunk::light`, gepackte Nibbles) — die Spawn-Prüfung ist damit ein
  direkter Lookup, kein neuer Pass.
- Nicht-Mob-Entities: Drop-Item (Voraussetzung für „Abbau droppt"), Pfeil, TNT
  (gezündet), Lore + Schiene, Ofen-/Truhen-Lore, Boot.
- Bewusst **nicht** enthalten (Vorlage: kamen erst in Beta): Erfahrungskugeln.

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
- Bewegung: Flug vs. Gehen (steht schon)
- Schaden/Tod: aus vs. an
- Bedrock: abbaubar vs. nicht (steht schon)
- Mob-Aggression: ignoriert den Spieler vs. greift an

Umsetzung: ein `struct ModeRules` mit `const fn for_mode(PlayerMode) -> Self`
in `player/mod.rs`, in dem genau diese Booleans stehen. Jede Fallunterscheidung
liest daraus, statt `matches!(mode, PlayerMode::Creative)` erneut zu schreiben —
dieses Muster steht heute schon zweimal im Code und würde sonst zweistellig oft.

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

Heute gibt es nur Abbauen und Platzieren (`place_block_at_camera_ray`,
`render/state.rs:19233`). Türen,
Kisten, Werkbank, Ofen, Jukebox, Hebel und Knopf brauchen einen echten
Rechtsklick-Handler pro Block, der *vor* dem Platzieren greift.

Umsetzung: ein Feld `on_use: Option<UseFn>` in `BlockSpec` — dieselbe Bauform
wie das schon vorhandene `random_tick: Option<RandomTickFn>`
(`blocks/mod.rs:514`, `with_random_tick` `:2007`), damit die Dispatch-Tabelle
einheitlich bleibt. Der Rechtsklick-Pfad fragt zuerst `on_use` des getroffenen
Blocks; nur wenn das `None` liefert oder ablehnt, wird platziert. Sneak-Halten
überspringt `on_use` (Vorlage-Verhalten für "auf eine Kiste bauen").

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
`block_data`).

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
- add neighbor-aware exposed-air checks so `HiddenOnly` ores are trustworthy at chunk borders
- expand worldgen regression tests with seam, density, and height-distribution sampling
- continue consolidating legacy/older worldgen entry points against the active worker pipeline
- document preset differences and worldgen phase order where contributors will find it quickly

### Medium priority
- add more ore families once replacement/height targets are defined
- introduce biome/material-aware replacement rules beyond plain stone
- add richer height models if balancing needs more than uniform/triangle distributions
- surface more worldgen metrics in the debug overlay
- prepare the worldgen config path for taller worlds without redoing ore anchors again

### Low priority
- decide and document long-term world size expectations
- add comparison screenshots/seed references per preset
- revisit historical parity content only after modern feature passes are stable
- improve water pond/lake generation, which currently reads as bare sand+water clumps rather than believable small ponds: the logic actually lives outside `worldgen/carvers/` in `src/render/chunkgen_worker.rs` (`LakeCandidate` at `chunkgen_worker.rs:192`, `make_water_lake_candidate` at `:342`, boundary sealing `apply_water_lake_boundary_seal_to_chunk` at `:639`, carve/fill `apply_lake_carve_and_fill_to_chunk` at `:707`, size bounds `WATER_LAKE_R_MIN/MAX` at `:29-35`); consider moving this into a proper `worldgen/carvers/ponds.rs` module alongside `ravine.rs`/`caves.rs`, and improving the shape (organic/noise-perturbed basin outline instead of a filled sphere/blob) plus a proper sand-shore rim (graded sand band around the water edge instead of sand and water occupying the same footprint)

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

## UI / UX

- keep expanding the options UI without reintroducing scattered hard-coded runtime settings

### Aus dem a1.0.17_04-Abgleich

- Hotbar-Zifferntasten `1..9` auf die Slotauswahl legen. Steht seit 2026-03-07
  als Caveat in `01_status_und_aktueller_stand.md` und ist die kleinste offene
  Lücke im heutigen Inventar. Achtung beim Anlegen neuer Aktionen:
  `InputAction::ALL` ist ein Array fester Länge mit handgepflegtem `index()`
  (`settings.rs:92-120`) — beides muss mitwachsen, sonst bricht der
  Settings-Serializer still
- HUD-Ausblenden auf F1. F2 (Screenshot) und F3 (Debug-Overlay) gibt es, F1
  nicht; die Tastenbehandlung für F10-F12 in `app/mod.rs:3389-3429` ist die
  Vorlage
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

What was verified in the code before writing this item:

- **The per-stage budgets are additive and none of them knows the frame
  deadline.** Every stage is a separate constant and every stage may spend its
  full budget in the same frame: chunkgen apply 3.0
  (`STREAM_CHUNKGEN_APPLY_BUDGET_MS`, `config/mod.rs:343`), apply follow-ups 1.5,
  saved-column load apply 2.0, streaming lighting 6.0, removal lighting 3.0,
  block-edit lighting 1.5, changed-chunk dirty apply 0.5, mesh dispatch 2.0,
  mesh upload 3.0 (5.5 low-speed, 10.0 warmup), prune 1.0, evict 1.5, block-edit
  side effects 1.5, fluid sim 3.0. That sums to **~30 ms before `render` even
  starts** — a 60 Hz frame is 16.6 ms. The engine is currently *specified* to
  drop to ~30 fps whenever every stage has work, which is the definition of
  "chunks are generating".
- **Burst mode widens the budget exactly when the frame is already heavy.**
  `update_stream_burst_mode_for_camera` (`state.rs:11228`) turns burst on when
  meshes near the camera are missing, and `stream_budget_for_stage_ms`
  (`state.rs:11245`) then hands out up to `STREAM_BURST_MAX_TOTAL_BUDGET_MS =
  25.0` across three stages alone. That shared cap is the right shape — it is
  just capped too high, applies only in burst, and covers only 3 of ~12 stages.
- **There is no frame-time feedback anywhere.** No budget in `render/state.rs`
  or `app/mod.rs` reads a measured frame time; all of them are compile-time
  constants. Nothing widens the budget on a cheap frame or narrows it after an
  overrun.
- **Three worker pools each spawn `available_parallelism()` threads**
  (`mesh_worker.rs:45`, `chunkgen_worker.rs:3162`, `lighting_worker.rs:28`), all
  at default OS priority. On an 8-thread CPU that is up to 24 runnable workers
  competing with the one thread that has to simulate *and* render. A main thread
  that respects its budget perfectly still loses wall-clock time here, so budget
  work alone cannot close this.
- **The world can only be mutated on the frame thread.** `World.chunks` is a
  plain `HashMap<ChunkCoord, Chunk>` (`world/mod.rs:51`) owned by `State`
  (`state.rs:5532`), so applying a generated chunk needs `&mut World` inline.
  This is the structural reason apply, lighting hooks and fluid bootstrap cannot
  move off the frame thread today.
- **GPU resources are created inside the frame.** `upload_cpu_mesh_to_gpu_reuse`
  (`mesh_gpu.rs:203`) calls `device.create_buffer` + `queue.write_buffer` on the
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
   `state.rs:10689`), subtract the measured cost of the parts that are not
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
- chunk mesh GPU suballocation (`render/mesh_gpu.rs:196-260`): up to 6 `wgpu::Buffer` per chunk, `grown_capacity` rounds to `next_power_of_two` (up to 2x VRAM waste). Suballocate layers from a persistent arena, keep the existing reuse-on-fit path
- move incremental lighting onto the existing `LightingWorkerPool`: it is spawned but idle (`light_rx=0` in every captured log) while incremental lighting runs on the main thread in 1.5 ms slices. This is the structural half of the lighting backlog problem; the budgets were only raised. Touches world-state/snapshot consistency, so it is a concurrency change, not tuning
- ~~step the chunk-removal lighting like the added path~~ **done**: `pump_removed_lighting_batches` no longer calls the one-shot `lighting_on_chunks_removed`. A batch becomes a `RemovedLightingJob` holding an `IncrementalLightingTask` (`World::begin_incremental_chunk_removed_lighting_task`, sharing its neighbor-region walk with `LightEngine::on_chunks_removed` so both relight the same region) and is stepped under the same voxel/wall-clock limits as the added path, resuming across frames. Backlog counters and the teardown path count the in-flight job via `removed_lighting_backlog_len()`
- ~~stop a single item from blowing a stage budget~~ **done** (reporting half): `perf/stage_budget.rs` aggregates per frame which single items cost a stage its whole budget (count, worst item + label, summed overrun), reported worst-first into the trace ring every frame, into the console at most every 5 s, and appended to the stall report as `stage_budget_overruns[…]`. Wired into the three lighting step loops and the chunk-mesh upload loop — i.e. only where the per-item duration was already measured, so no new timing calls on hot paths. Splitting an offending item stays a per-stage fix; the watchdog only makes it visible
- note re: "multithreading" — already present via `std::thread` + crossbeam-channel workers per `src/render/*_worker.rs`; no `rayon` usage found in those workers, worth deciding whether worker-pool-per-stage (current) or a shared rayon pool is the intended long-term model before adding more parallel stages

## Docs / Cleanup

- keep README, status docs, backlog docs, and worldgen TODOs updated after feature landings. Storage / Content Foundation and Gameplay are closed; their two rejected designs (a compact v2.1/v3 column layout, and merging blocks + block_data into a `BlockStateId`) are recorded with their measurements next to the code, in `storage/chunk_codec.rs` and `world/chunk.rs`, rather than lingering here as open work
- convert important contracts into short code comments near their source modules
- remove stale assumptions whenever a system stops being experimental
- make `cargo clippy --all-targets -- -D warnings` (the lint command in `CLAUDE.md`) usable again: the one hard clippy error is gone, but ~102 `never used` warnings remain, so `-D warnings` still fails. Needs a decision per item — bulk-deleting would likely remove scaffolding for in-flight work, bulk-`#[allow]` would hide future real findings

## Historical / Classic Parity

- humans/mob stub for early Classic feel
- fine-tuning of individual UI/gameplay parity details
