Feature-Übersicht von Alpha v1.0.17_04

Die Version Alpha v1.0.17_04 (veröffentlicht am 23. August 2010) markiert das Ende der Seecret Friday Updates rund um Update 8.
1. Weltgenerierung & Chunk-System
Chunk-Format & Speichern

    Chunk-Größe: 16×16×128 Blöcke (Y=0 bis 127).

    Speicherformat: Alpha-Level-Format (NBT-basiert, komprimiert mit zlib) .

    Weltgrenzen: Theoretisch ±32 Millionen Blöcke; Far Lands bei ±12.550.820 mit Mesh-Verzerrungen.

    Session-Lock: session.lock verhindert, dass zwei Instanzen dieselbe Welt gleichzeitig speichern.

Terrain & Noise

    Generator: Perlin/Simplex-Noise für Höhenkarte, Höhlen (3D-Noise), Erzgänge.

    Biom-Vorläufer: Keine echten Biomes, aber „Winter-Maps" (25% Chance bei Welterstellung) mit Schneebedeckung und zufrierendem Wasser.

    Strukturen:

        Dungeons: 7×7×7 Raum mit Monster-Spawner und 2 Truhen.

        Bäume: Eichen (klein und groß/fancy), Setzlinge mit 1/20 Drop-Chance.

        Erze: Kohle, Eisen, Gold, Diamant, Redstone, Lapis (noch nicht als Erzblock, nur als Item).

        Gewässer: Ozeane, Seen, Flüsse; Kiesstrände mit teilweise nicht gefrorenem Wasser in Winterwelten.

        Höhlen: Große, verzweigte Höhlensysteme, teilweise mit Lavaseen.

        Schluchten: Tief eingeschnittene Canyons (durch Generierungsfehler/Edge-Cases) .

Vegetation & Dekoration

    Pflanzen:

        Bäume: Eichen (Standard und „Fancy"-Variante).

        Blumen: Gelbe und rote Blumen in kleinen Patches.

        Pilze: Rote und braune Pilze im Schatten (auch auf Gras).

        Zuckerrohr: An Stränden und Flussufern (damals „Schilf").

        Kakteen: Nur auf Sand, maximal 3 Blöcke hoch, verursachen Kontakt-Schaden.

        Schnee: Dünne Schneeschicht auf Blöcken in Winterwelten; Schneebedeckung wächst bei Schneefall.

    Oberflächenmaterialien:

        Ton: In der Nähe von Wasser generiert, droppt 4 Tonbällchen.

        Sand & Kies: An Stränden, in Wüsten-Patches und als Schichten in Höhlen .

2. Blöcke (vollständige Liste)
Naturblöcke

    Grundgestein: Bedrock (unzerstörbar, Y=0) .

    Steinvarianten: Stein, Kopfsteinpflaster (Cobblestone), Bruchstein-Stufen, Bruchstein-Treppen.

    Erde & Gras: Erde, Grasblock (grüne Textur in Alpha) .

    Sand & Kies: Sand, Kies (fällt bei fehlender Unterstützung) .

    Erzblöcke: Kohle-, Eisen-, Gold-, Diamant-, Redstone-Erz (visuell als Erz im Stein).

    Holz: Eichenstamm, Eichenbretter, Eichenlaub (verrottet nicht mehr).

    Pflanzen: Setzling, Blumen (gelb/rot), Pilz (rot/braun), Zuckerrohr, Kaktus.

    Schnee & Eis: Schneeblock, Schneedecke (Layer), Eis (rutschig).

    Ton: Tonblock.

    Obsidian: TNT-resistent (seit a1.0.14).

    Sonder: Bücherregal, Glas, Schwamm (noch nicht natürlich generierend, nur im Code) .

Funktionsblöcke

    Werkbank: 3×3 Crafting .

    Ofen: Brennt Erze, Sand zu Glas, Ton zu Ziegel, Nahrungsmittel.

    Kiste: Einzel- und Doppelkiste (seit a1.0.14) .

    Jukebox: Spielt Musikplatten (13, Cat).

    Redstone-Komponenten:

        Redstone-Staub, Redstone-Fackel, Redstone-Erz.

        Hebel, Steinknopf, Holzdruckplatte, Steindruckplatte.

        Eisentür, Holztür (öffnen/schließen per Rechtsklick/Redstone).

    Transport: Schiene, Lore (Minecart), Ofen-Lore, Truhen-Lore.

    Sonstige:

        Fackel (Beleuchtung, Monster-Spawn-Prävention) .

        Leiter (Klettern).

        Tür (Holz/Eisen).

        Schild (Text, kein Redstone-Signal).

        Treppe & Stufe (Holz/Stein).

        Zaun (neu in a1.0.17, 1,5 Blöcke Kollisionshöhe) .

        Notenblock (noch nicht funktional spielbar, nur als Block) .

3. Items & Werkzeuge
Werkzeuge & Waffen

    Werkzeugstufen: Holz, Stein, Eisen, Gold, Diamant (Spitzhacke, Axt, Schaufel, Hacke, Schwert) .

    Haltbarkeit: Diamant-Werkzeuge seit a1.0.3: 1025 Uses (vorher 513).

    Rüstung: Leder, Eisen, Gold, Diamant (Helm, Brust, Hose, Stiefel); Kettenrüstung im Code, aber nicht craftbar .

    Bogen & Pfeile: Bogen (unendlich viele Pfeile in Alpha), Pfeil (Flint + Stock + Feder).

Ressourcen & Materialien

    Rohstoffe: Kohle, Eisenbarren, Goldbarren, Diamant, Redstone, Tonbällchen, Ziegel (gebrannter Ton).

    Baumaterialien: Bretter, Zaun (neu in a1.0.17), Glas, Bücherregal, Wolle (8 Farben) .

    Nahrung:

        Äpfel (Drop von Eichenlaub, noch nicht 100%) .

        Brot (3 Weizen) .

        Rohes/gebratenes Schweinefleisch .

        Pilzsuppe (2 Pilze + Schüssel) .

        Weizen & Samen (noch nicht als Farm-Pflanze, nur als Drop/Item) .

    Sonstige Items:

        Eimer (Wasser, Lava, Milch) .

        Schüssel .

        TNT (1 TNT + 4 Sand/Kies) .

        Feuerzeug (Flint & Steel) .

        Sattel (nur in Dungeon-Truhen, zum Reiten von Schweinen).

        Musikplatten (13, Cat).

        Schneeball (werfbar, kein Schaden).

        Ei (werfbar, kein Spawn) .

        Farbe (Dye, 8 Farben) .

        Papier & Buch (für Bücherregal, noch kein Buch-Item als solches).

        Slimeball (Drop von kleinen Slimes, noch ohne Verwendung).

4. Mobs & Entitäten
Passive Mobs

    Schwein: Aufsattelbar, Drop: rohes Schweinefleisch.

    Schaf: Wolle regeneriert, Drop: Wolle + Fleisch.

    Kuh: Melkbar (Milcheimer), Drop: rohes Rindfleisch + Leder.

    Huhn: Drop: Federn, rohes Huhn, Eier (legen zufällig).

Hostile Mobs

    Zombie: Nahkampf, Drop: Federn (Bug, später Fleisch).

    Skelett: Fernkampf mit Bogen, Drop: Pfeile, Knochen (noch nicht implementiert als Item) .

    Spinne: Klettert, nachts aktiv, Tag neutral .

    Creeper: Explodiert bei Annäherung .

    Slime: Teilt sich in kleinere Slimes (Frieden: keine großen Slimes).

    Spider Jockey: Skelett reitet Spinne (neu in a1.0.17) .

Nicht-Mob-Entitäten

    Lore: Normale Lore, Ofen-Lore, Truhen-Lore.

    Boot: Wasserfahrzeug, zerbricht an Land.

    Drop-Item: Alle Items als Entities im Boden .

    Pfeil: Flugprojektil .

    TNT: Explodiert nach Zündung .

    Erfahrungskugeln: Noch nicht vorhanden (kamen erst in Beta) .

5. Gameplay-Mechaniken
Gesundheit & Nahrung

    Gesundheit: 20 HP (10 Herzen), Heilung durch Essen (sofort, keine Hungerleiste) .

    Schadenquellen: Fall, Ertrinken, Lava, Feuer, Erstickung (in Blöcken), Mob-Angriffe, TNT-Explosion, Kaktus-Kontakt.

Bewegung & Physik

    Bewegung: Gehen, Springen (max. 2 Blöcke horizontal seit a1.0.4), Schwimmen.

    Sprint & Schleichen: Nicht vorhanden (kamen erst in Alpha 1.2.0) .

    Reiten:

        Schwein: Mit Sattel reiten (Steuerung ohne Karotte, nur Richtung).

        Lore: Mit Sitz-Animation (seit a1.0.4).

        Boot: Rechtsklick zum Einsteigen.

    Fluid-Physik: Wasser und Lava fließen mit einfacher Ausbreitungslogik (Quellenblöcke, fließende Blöcke).

    Sand/Kies: Fallen bei fehlender Unterstützung .

Interaktion & Crafting

    Crafting: 2×2 im Inventar, 3×3 an Werkbank .

    Smelting: Ofen mit Brennstoff (Kohle, Holz, Lava-Eimer).

    Block-Platzierung: Rechtsklick, Schilder benötigen keinen freien Block darüber (seit a1.0.5).

    Block-Abbau: Linksklick, Werkzeug-Effizienz, Haltbarkeit .

    Türen: Öffnen/Schließen per Rechtsklick, Redstone-Signal.

    Kisten: Öffnen/Schließen, Items lagern .

    Jukebox: Rechtsklick mit Platte zum Abspielen.

Redstone & Logik

    Redstone-Staub: Überträgt Signal bis 15 Blöcke.

    Redstone-Fackel: Inverter (Signal umkehren).

    Hebel, Knopf, Druckplatte: Signalquellen.

    Eisentür: Öffnet/Schließt mit Redstone.

    Schiebetüren & Kolben: Noch nicht vorhanden (kamen später) .

6. Multiplayer & Server
SMP (Survival Multiplayer)

    Einführung: a1.0.10 (erste SMP-Unterstützung, Mojang-Server, Invite-only).

    Server-Features:

        /op, /deop, /pardon, /pardon-ip, /tp (seit a1.0.16/0.1.1/0.1.2) .

        Server-seitige Render-Distanz, Max-Player-Capacity, Chat-Word-Wrap.

        Session-Lock für Weltspeicher.

    Multiplayer-spezifische Features:

        Hühner, Slime, Schwein in SMP (seit a1.0.17) .

        Tag-Nacht-Zyklus in SMP (seit a1.0.17) .

        Arm-Schwing-Animation in SMP/Third-Person (seit a1.0.15) .

7. Audio, UI & Sonstiges
Audio

    Musik: C418-Tracks (piano1.ogg, piano2.ogg, piano3.ogg, calm1.ogg, calm2.ogg, calm3.ogg, hal1.ogg, hal2.ogg, hal3.ogg, hal4.ogg).

    Ambiente: Höhlen-Geräusche, Schrittgeräusche, Mob-Sounds (hostile Mobs seit a1.0.3).

    Musik: Wiedereingeführt in a1.0.14 nach Entfernung in Infdev.

UI & Menü

    Titelbildschirm: Animiertes Logo (seit a1.0.4), Stein-Textur statt Cobblestone.

    Splashes: Zahlreiche Splash-Texte (z. B. „Seecret Friday update!", „Do not distribute!", „1K IN 24h" in a1.0.13_01).

    Inventar: 40 Slots (36 Inventar + 4 Crafting/Rüstung), keine Hungerleiste .

    HUD: F1 für HUD-Ausblendung, F2 für Screenshots (seit Alpha allgemein) .

    Pause-Menü: Dunklerer Hintergrund (seit a1.0.9).

Technische Features

    Java-Version: Java SE 5 (Minimum).

    Protocol-Version: 12 (a1.0.14), 13 (a1.0.15), 14 (a1.0.16), 1 (a1.0.17).

    Server-Versionen: 0.1.0 (a1.0.15), 0.1.1 (a1.0.16), 0.1.2 (a1.0.16_01), 0.1.4 (a1.0.17) .

8. Bekannte Bugs & Limitationen (für den Abgleich relevant)

    Slime-Spawn: Natürliches Spawnen durch Berechnungsfehler de facto deaktiviert (a1.0.14).

    Score-Anzeige: Todesbild zeigt „Score: &e0" (Bug bis 1.0.0).

    Dritte-Person-Items: Items in Third-Person riesig dargestellt (a1.0.6).

    Arm-Animation: Arm-Schwingen nur in Multiplayer/Third-Person sichtbar (a1.0.15) .

    Fehlende Features (im Vergleich zu später):

        Kein Hunger, kein Sprint, kein Schleichen .

        Kein Nether, keine Betten, keine Biomes (im modernen Sinn) .

        Kein Zaubertisch, kein Verzaubern, kein Amboss .