# Feature-Referenz: Alpha v1.0.17_04

Stand der Java Edition am Ende der Alpha-1.0-Reihe, als Vorlage für den
Content-Abgleich in [TODO.md](../TODO.md). Eigene Zusammenfassung; die
Angaben sind gegen die Versionsseiten des [Minecraft Wiki](https://minecraft.wiki)
geprüft (Quellen am Ende). Steht bei einem Punkt eine Version, ist das die
Alpha-Version, in der er dazukam.

- **Veröffentlicht:** 23. August 2010, letzter Patch von Alpha v1.0.17
  (20. August 2010, "Seecret Friday 8"). Der Patch selbst behebt nur ein
  Steckenbleiben des Spielers.
- **Nachfolger:** Alpha v1.1.0. Biome und Nether kommen erst mit v1.2.0.

## 1. Welt und Speicherformat

| Thema | Stand |
|---|---|
| Chunk | 16 × 16 × 128 Blöcke, Y 0 bis 127 |
| Welt | unendlich generiert; ab Koordinate 12.550.824 setzen die Far Lands ein (Noise-Überlauf) |
| Speicherformat | *Alpha Level Format*: `level.dat` (Seed, Spawn, Zeit, Spieler) plus eine Datei pro Chunk (`c.<x>.<z>.dat`, Base36) in zwei Ebenen Unterordnern; alles NBT, GZip-komprimiert. Regionsdateien kommen erst in Beta 1.3 |
| `session.lock` | Zeitstempel; wer zuletzt schreibt, besitzt die Welt, andere Instanzen geben auf |

## 2. Weltgenerierung

| Thema | Stand |
|---|---|
| Biome | keine. Der Generator kennt nur Geländeformen; echte Biome erst in v1.2.0 |
| Winterwelten (v1.0.4) | 25 % Chance bei der Welterstellung (`SnowCovered` in `level.dat`): Schneefall, dünne Schneeschicht auf allem unter freiem Himmel, die bei Schneefall wächst; offenes Wasser friert zu Eis, außer an Kiesstränden |
| Ozeane | Wasser unterhalb des Meeresspiegels (vermutlich Y=64; Beta 1.8 senkt ihn um einen Block). Ozeane sind Geländeteile, kein eigenes Biom; Boden aus Erde, Sand und Ton |
| Strände | Sand- und Kiesstrände; Kiesstrände entfallen erst mit Beta 1.8 |
| Höhlen | verzweigte Höhlensysteme; Schluchten gibt es noch nicht (Beta 1.8) |
| Untergrund | Kies- und Erdtaschen, erzartig verteilt (seit Infdev) |
| Erze | Kohle, Eisen, Gold, Diamant, Redstone (v1.0.1). Gold und Diamant seit v1.0.1 doppelt so häufig. Kein Lapis (Beta 1.2) |
| Dungeons | Monsterräume mit Spawner und Truhen. Seit v1.0.14 doppelt so häufig. Inhalt u. a. Schwarzpulver, Faden, Weizen, Brot, Eisen, Eimer, Sattel, Redstone, selten Goldapfel, Schallplatten (v1.0.14) |
| Bäume | normale Eiche und "Fancy"-Eiche (v1.0.6 wieder da, seltener) |
| Pflanzen | gelbe und rote Blumen; Pilze überall, im Sonnenlicht fallen sie mit der Zeit ab (v1.0.1) |
| Kaktus (v1.0.6) | nur auf Sand, ohne direkte Nachbarblöcke, bis 3 hoch (v1.0.14), Kontaktschaden |
| Zuckerrohr (v1.0.11) | damals "Reeds", an Stränden, bis 3 hoch |
| Ton (v1.0.11) | in Wassernähe |

## 3. Blöcke

- **Natur:** Stein, Gras, Erde, Bruchstein, bemooster Bruchstein, Grundgestein,
  Sand, Kies, Ton, Obsidian (seit v1.0.14 TNT-fest), Stamm, Laub, Setzling,
  Blumen, Pilze, Kaktus, Zuckerrohr, Wasser, Lava, Feuer
- **Erze und Erzblöcke:** Kohle-, Eisen-, Gold-, Diamant-, Redstone-Erz;
  Eisen-, Gold-, Diamantblock
- **Schnee und Eis (v1.0.4):** Schneeschicht (noch nicht abbaubar), Schneeblock
  (aus 4 Schneebällen), Eis (rutschig, schmilzt an Feuer/Lava)
- **Bau:** Bretter, Glas, Ziegel, Bücherregal (seit v1.0.11 herstellbar, droppt
  nichts), Wolle (**nur weiß**), Steinstufe, Holz- und Bruchsteintreppe,
  Leiter, Schild, Holztür, Zaun (v1.0.17)
- **Funktion:** Werkbank, Ofen, Truhe, Jukebox (v1.0.14), Monster-Spawner,
  TNT, Ackerboden und Weizenpflanze, Schiene
- **Redstone (v1.0.1):** Redstone-Staub, Redstone-Fackel, Hebel, Steinknopf,
  Druckplatte aus Stein (nur Spieler/Mobs) und aus Holz (alle Entities),
  Eisentür (nur per Redstone)
- **Sonderfall:** Schwamm existiert als Block, wird aber nicht generiert

**Zaun (v1.0.17):** nur auf festen Blöcken platzierbar, verbindet sich nicht
mit festen Blöcken, auf ihm lässt sich nichts platzieren. Neue Welten starten
mit 64 Zäunen im Inventar.

**Noch nicht vorhanden:** Kürbis, Netherrack, Glowstone, Seelensand (v1.2.0),
Lapis, Sandstein, Notenblock, Werfer, farbige Wolle (Beta 1.2), Bett (Beta 1.3).

## 4. Items

- **Werkzeuge:** Spitzhacke, Axt, Schaufel, Hacke, Schwert in Holz, Stein,
  Eisen, Gold, Diamant
- **Rüstung:** Leder (seit v1.0.8 aus Leder statt Stoff), Eisen, Gold, Diamant
- **Fernkampf:** Bogen ohne Aufladen (Schuss sofort, Aufladen erst Beta 1.8),
  Pfeil (Spitze seit v1.0.14 aus Feuerstein)
- **Rohstoffe:** Kohle, Diamant, Eisen- und Goldbarren, Redstone, Stock,
  Faden, Feder, Schwarzpulver, Feuerstein, Leder (v1.0.8), Tonklumpen und
  Ziegel (v1.0.11), Zuckerrohr, Papier, Buch (v1.0.11), Schleimball
  (v1.0.11, noch ohne Verwendung), Schneeball (stapelt bis 16)
- **Nahrung** (heilt sofort, keine Hungerleiste): Apfel, Goldapfel, Brot,
  rohes und gebratenes Schweinefleisch, Pilzsuppe. Äpfel fallen noch nicht aus
  Laub (erst Java 1.1)
- **Landwirtschaft:** Samen entstehen beim Bearbeiten von Gras mit der Hacke;
  Weizen wird auf Ackerboden angebaut
- **Sonstiges:** Eimer (Wasser, Lava, Milch; Milch seit v1.0.11 von Kühen,
  leerbar seit v1.0.17), Feuerzeug, Schüssel, Gemälde, Sattel, Lore, Lore mit
  Truhe und mit Ofen (v1.0.14), Boot (v1.0.6), Ei (v1.0.14, ohne Funktion,
  nicht werfbar), Schallplatten "13" und "cat" (v1.0.14)

**Noch nicht vorhanden:** Kompass (v1.1.0), Angel und Fisch (v1.1.1), Uhr
(v1.2.0), Farbstoffe, Knochen, Zucker, Kuchen (Beta 1.2), Schere (Beta 1.7).

## 5. Mobs

| Mob | seit | Drop | Verhalten |
|---|---|---|---|
| Schwein | vor Alpha | 0–2 rohes Schweinefleisch | mit Sattel reitbar, aber **nicht steuerbar** (erst Java 1.4.2) |
| Schaf | vor Alpha | Wolle (weiß) beim Schlagen | Scheren erst Beta 1.7 |
| Kuh | v1.0.8 | 0–2 Leder | melkbar mit Eimer (v1.0.11); Rindfleisch erst Beta 1.8 |
| Huhn | v1.0.14 | 0–2 Federn | legt alle 5–10 min ein Ei; seit v1.0.17 kein Fallschaden, gleitet |
| Zombie | vor Alpha | 0–2 Federn | brennt im Sonnenlicht |
| Skelett | vor Alpha | Pfeile | brennt im Sonnenlicht; Knochen erst Beta 1.2 |
| Spinne | vor Alpha | 0–2 Faden | bei Tageslicht neutral; Wandklettern erst Beta 1.2 |
| Creeper | vor Alpha | Schwarzpulver | explodiert in Spielernähe |
| Schleim | v1.0.11 | Schleimball (kleinste Größe) | drei Größen, teilt sich beim Tod; nur unter Y=32. Seit v1.0.14 spawnt er durch einen Rechenfehler praktisch nicht mehr natürlich |
| Spinnenreiter | v1.0.17 | | Skelett reitet auf Spinne |

## 6. Spielmechanik

- **Gesundheit:** 10 Herzen, Essen heilt sofort. Kein Hunger, kein Sprinten
  (beides Beta 1.8), kein Schleichen (Alpha v1.1.1).
- **Laub:** zerfällt nicht (seit v1.0.6); Setzling-Drop 5 %.
- **Fahrzeuge:** Boot zerbricht an Land oder bei Kollision mit Land (v1.0.6).
  Sitzhaltung beim Reiten (v1.0.4); seit v1.0.14 sind Booster-Konstruktionen
  möglich, die Ofen-Lore fährt mit Kohle.
- **Redstone:** Staub kennt nur an oder aus; abgestufte Signalstärke mit
  sichtbarer Abdunklung erst in Beta 1.3. Fackeln invertieren. Ob Reichweite
  und Fackel-Burnout schon wie später begrenzt waren, ist nicht belegt. Die
  Eisentür reagiert nur auf Redstone, die Holztür auf Klick.
- **Lava** setzt Blöcke in Brand (v1.0.15).
- **Fluide:** Wasser und Lava fließen dynamisch (seit Infdev).
- **Arm-Schwingen:** seit v1.0.15 im Multiplayer und in der Third-Person-Ansicht
  sichtbar; seit v1.0.17 auch beim Rechtsklick auf benutzbare Blöcke.

## 7. Oberfläche und Audio

- **Inventar:** 40 Slots (heute 36); auch der Crafting-Slot kann Items halten.
  Crafting 2 × 2 im Inventar, 3 × 3 an der Werkbank.
- **Tasten:** F1 blendet HUD und Arm aus, F1 + F2 speichert einen Screenshot.
- **Third Person:** gehaltene Items erscheinen stark vergrößert; seit v1.0.17
  ist die Kamera wieder zentriert.
- **Titelbildschirm:** animiertes Logo aus Stein (v1.0.4), Splash-Texte.
- **Musik:** seit v1.0.14 wieder aktiv, mit drei neuen Stücken von C418;
  dazu Höhlen- und Mob-Geräusche.

## 8. Multiplayer

Für den Abgleich ausgeklammert, nur zur Einordnung:

- Survival-Multiplayer seit v1.0.15 (Server 0.1.0), Verbindung per IP.
- Server-Operatoren und Befehle wie `/op`, `/deop`, `/kick`, `/ban`,
  `/pardon`, `/pardon-ip`, `/home`, `/stop` seit v1.0.16.
- Seit v1.0.17 gibt es Hühner, Schleime und Schweine sowie den Tag-Nacht-Zyklus
  auch im Multiplayer.

## Korrekturen gegenüber der früheren Fassung

Die vorige Version dieser Datei enthielt Angaben, die laut Wiki nicht auf
a1.0.17_04 zutreffen. Wo [TODO.md](../TODO.md) daraus Punkte übernommen hat,
gilt diese Fassung:

- Kein Lapis, kein Notenblock, keine Farbstoffe, Wolle nur weiß (alles Beta 1.2).
- Keine Schluchten (Beta 1.8); Flüsse sind nicht belegt. Kies liegt an
  Stränden und in Taschen unter Tage, nicht als Schichten in Höhlen.
- Redstone-Staub ist nur an oder aus, keine 15 Signalstufen.
- Die Far Lands beginnen bei 12.550.824.
- Weizen ist anbaubar (seit Indev), nicht nur ein Drop.
- Schafe lassen Wolle beim Schlagen fallen; sie regenerieren keine Wolle und
  sind nicht scherbar. Kühe droppen nur Leder, Hühner nur Federn.
- Schweine sind reitbar, aber nicht steuerbar.
- Äpfel fallen nicht aus Laub.
- Eier sind nicht werfbar.
- Sprinten kam mit Beta 1.8, Schleichen mit Alpha v1.1.1 (nicht "Alpha 1.2.0").
- Multiplayer startete mit v1.0.15, nicht v1.0.10.
- Chunkdateien sind GZip-komprimiert, nicht zlib.

## Quellen

Minecraft Wiki, abgerufen am 28.10.2026. Die Wiki-Inhalte stehen unter
[CC BY-NC-SA 3.0](https://creativecommons.org/licenses/by-nc-sa/3.0/); diese
Datei übernimmt keinen Text, nur Fakten.

- Versionsseiten: [Alpha](https://minecraft.wiki/w/Java_Edition_Alpha),
  [v1.0.1](https://minecraft.wiki/w/Java_Edition_Alpha_v1.0.1),
  [v1.0.4](https://minecraft.wiki/w/Java_Edition_Alpha_v1.0.4),
  [v1.0.6](https://minecraft.wiki/w/Java_Edition_Alpha_v1.0.6),
  [v1.0.8](https://minecraft.wiki/w/Java_Edition_Alpha_v1.0.8),
  [v1.0.11](https://minecraft.wiki/w/Java_Edition_Alpha_v1.0.11),
  [v1.0.14](https://minecraft.wiki/w/Java_Edition_Alpha_v1.0.14),
  [v1.0.15](https://minecraft.wiki/w/Java_Edition_Alpha_v1.0.15),
  [v1.0.16](https://minecraft.wiki/w/Java_Edition_Alpha_v1.0.16),
  [v1.0.17](https://minecraft.wiki/w/Java_Edition_Alpha_v1.0.17),
  [v1.0.17_04](https://minecraft.wiki/w/Java_Edition_Alpha_v1.0.17_04),
  [Infdev](https://minecraft.wiki/w/Java_Edition_Infdev)
- Format und Welt: [Alpha level format](https://minecraft.wiki/w/Java_Edition_Alpha_level_format),
  [Far Lands](https://minecraft.wiki/w/Far_Lands),
  [Monster Room](https://minecraft.wiki/w/Monster_Room),
  [Ravine](https://minecraft.wiki/w/Ravine),
  [Ocean](https://minecraft.wiki/w/Ocean),
  [Gravel](https://minecraft.wiki/w/Gravel),
  [Lapis Lazuli Ore](https://minecraft.wiki/w/Lapis_Lazuli_Ore)
- Blöcke und Items: [Oak Fence](https://minecraft.wiki/w/Oak_Fence),
  [Wool](https://minecraft.wiki/w/Wool),
  [Note Block](https://minecraft.wiki/w/Note_Block),
  [Wheat Seeds](https://minecraft.wiki/w/Wheat_Seeds),
  [Apple](https://minecraft.wiki/w/Apple),
  [Egg](https://minecraft.wiki/w/Egg),
  [Bow](https://minecraft.wiki/w/Bow),
  [Redstone Dust](https://minecraft.wiki/w/Redstone_Dust)
- Mobs und Mechanik: [Pig](https://minecraft.wiki/w/Pig),
  [Cow](https://minecraft.wiki/w/Cow),
  [Zombie](https://minecraft.wiki/w/Zombie),
  [Skeleton](https://minecraft.wiki/w/Skeleton),
  [Spider](https://minecraft.wiki/w/Spider),
  [Sneaking](https://minecraft.wiki/w/Sneaking),
  [Sprinting](https://minecraft.wiki/w/Sprinting)
