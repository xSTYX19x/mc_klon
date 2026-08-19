# 07 — Performance & Rendering-Correctness Audit

**Datum:** 2026-08-19 · **Basis der Analyse:** Commit `8f7634d` (branch `main`, clean)
**Verifikation der Analyse:** `cargo check` → exit 0.

> **Umsetzungsstand.** Der Report wurde read-only erstellt; Fixes kamen danach. Erledigt sind §1.1,
> §1.2 (Commit `188fdb8`) sowie §2.1, §2.3, §2.4 und die Durchsatz-Anpassung aus §2.2.
> Details in [Umsetzungsstand](#umsetzungsstand). Die Messwerte im Report stammen durchweg aus
> Läufen **vor** diesen Fixes und sind entsprechend zu lesen.

Untersuchte Symptome (Priorität wie gemeldet):

1. **CHUNK-GENERATION-LAG** — Spiel läuft flüssig, bis neue Chunks generiert werden müssen (z. B. beim Bewegen); dann unspielbar laggy.
2. **LIGHTING/SHADER-DROPOUTS** — einzelne Chunks rendern unbeleuchtet: entweder komplett dunkel (wie Nacht) oder ganz ohne Shading (flach schwarz / rohe Texturen ohne Schatten/AO).
3. **SHADER-ARTEFAKTE** — gelegentlich 2D schwarze vertikale Säulen/Streifen am Bildschirm.

Datenbasis: vollständiges Lesen der Streaming-/Lighting-/Meshing-/Upload-Pfade plus Auswertung aller 10 vorhandenen `logs/run_*.stall.txt` und der zugehörigen `*.trace.json` / `*.gpu.csv`.

Jeder Befund ist markiert als **CONFIRMED** (Code- **und** Log-Evidenz) oder **SUSPECTED** (Messung nötig).

---

## Inhaltsverzeichnis

1. [Top Suspects](#1-top-suspects)
2. [Symptom 1 — Chunk-Streaming-Lag](#2-symptom-1--chunk-streaming-lag)
3. [Symptom 2 — Lighting/Shading-Dropouts](#3-symptom-2--lightingshading-dropouts)
4. [Symptom 3 — Schwarze vertikale Säulen](#4-symptom-3--schwarze-vertikale-säulen)
5. [Allgemeine Schwachstellen](#5-allgemeine-schwachstellen)
6. [Nächste Messungen](#6-nächste-messungen)
7. [Umsetzungsstand](#umsetzungsstand)
8. [Anhang A — Rohdaten aus den Stall-Reports](#anhang-a--rohdaten-aus-den-stall-reports)
9. [Anhang B — Was explizit geprüft und ausgeschlossen wurde](#anhang-b--was-explizit-geprüft-und-ausgeschlossen-wurde)
10. [Anhang C — Relevante Konstanten](#anhang-c--relevante-konstanten)

---

## 1. TOP SUSPECTS

| # | Befund | Severity | Symptom | Analyse | Umsetzung |
|---|--------|----------|---------|---------|-----------|
| 1 | Immer aktives Chrome-Tracing: JSON-Serialisierung + Datei-I/O unter **einem globalen Mutex**, geteilt von Main-Thread und allen Workern | critical | 1 | CONFIRMED | ✅ §1.1 |
| 2 | `refresh_worldgen_warmup_tracking()` baut das komplette Startup-Section-Set pro Drain neu → O(n²) über den Startup | critical | 1 | CONFIRMED | ✅ §1.2 |
| 3 | Provisorisches Licht unterhalb Y=64 ist **komplett 0**, und der Lighting-Backlog wird nie abgebaut (190–218 Jobs dauerhaft) | critical | 2, 3 | CONFIRMED | ✅ §2.1 + §2.2 |
| 4 | `mesh_should_await_lighting_finalization` parkt den korrigierenden Remesh bis `Finalized` — was Rand-Chunks u. U. nie erreichen | high | 2, 3 | CONFIRMED | ✅ §2.3 + §2.4 |
| 5 | `create_buffer_init` an 17 Stellen in `render()` (davon 11 ohne Cache-Gate) + `queue.write_buffer` pro Chunk-Layer ohne Suballocation | high | 1 | CONFIRMED (Zahl korrigiert) | ✅ Overlays behoben, Chunk-Suballocation offen |

### Kernzahlen aus den eigenen Logs

| Log | frame | Evidenz |
|-----|-------|---------|
| `run_1781101083_106` | 356.1 ms | `chunkgen pure_ms=312.9` für **3** applied chunks (≈104 ms/Chunk) |
| `run_1779800309_641` | 241.5 ms | `chunkgen pure_ms=201.4`, `lighting split_jobs=190, backlog=190` |
| `run_1773085075_183` | **9697.5 ms** | `sched[pend=8566,heap=15575]`, view=32 |
| `run_1787146900_888` | 288.4 ms | `mesh wait_avg/max=181.02/413.20 ms`, `mesh_up_q=47`, `lighting backlog=194` |
| `run_1773071920_600` | 253.9 ms | `lighting=237.5 ms`, aber `proc.rebuild_world_lighting_if_dirty=3.2 ms` |

Die letzte Zeile ist der entscheidende Hinweis auf #1: Die Zeit liegt **innerhalb** des Lighting-Scopes, aber **außerhalb** jedes Sub-Scopes. Sie wird in der Instrumentierung selbst verbraucht.

---

## 2. SYMPTOM 1 — Chunk-Streaming-Lag

### 1.1 Tracing ist bedingungslos aktiv und schreibt hunderte MB synchron

**Severity:** critical · **Likelihood Symptom 1:** sehr hoch · **Status:** CONFIRMED

**Evidenz:** `src/main.rs:34` ruft `perf::trace::trace_init()` — ohne Feature-Gate, ohne Env-Var, ohne Release-Guard.

`src/perf/trace.rs:237-268` (`record_duration`) macht pro Event:

1. `thread_label_and_tid()` → `String`-Allokation + FNV-Hash (`trace.rs:89-99`)
2. `tag.to_string()` + `counts.to_string()` → 2 weitere Allokationen
3. `json!{…}` → ein `serde_json::Value`-Map (weitere Allokationen)
4. `rt.ring.lock()` → globaler Mutex #1
5. `rt.writer.lock()` → **globaler Mutex #2, gehalten über `serde_json::to_writer` in einen `BufWriter<File>`**

175 Call-Sites insgesamt; 89 davon übergeben `&format!(...)`. Das `format!` wird **vor** dem `TRACE_RUNTIME.get()`-Early-Return ausgewertet — die Formatierungskosten fallen also selbst dann an, wenn das Tracing deaktiviert wäre.

**Mechanismus:** Jeder Worker-Thread und der Main-Thread serialisieren auf einem Mutex, der über Disk-I/O gehalten wird. Das Trace-Volumen ist proportional zur Chunk-Pipeline-Aktivität — die Contention tritt daher exakt dann auf, wenn Chunks streamen. Das ist die direkte Erklärung für „läuft gut, bis neue Chunks generiert werden".

Konkrete Hot Spots:

- `pump_chunkgen_results` emittiert **8** `record_duration_ms(..., &format!(...))` pro appliziertem Chunk (`src/render/state.rs:12945-13005`).
- `mesh_worker` emittiert **3 große** pro Job, der letzte mit 21 Format-Argumenten (`src/render/mesh_worker.rs:83-165`) — auf **jedem** Mesh-Worker-Thread.
- `pump_chunkgen_apply_followups` emittiert ein großes `format!` **pro Step** (`src/render/state.rs:12644-12660`).
- `finalize_chunk_lighting_state` emittiert bis zu 3 Events pro Chunk (`src/render/state.rs:15613-15646`).

**Smoking Gun:** `logs/run_1787146900_888.trace.json` = **558 MB** (Lauf von heute). Weitere: 134 MB, 113 MB, 113 MB, 98 MB, 95 MB, 69 MB. Das rekonziliert `pure_ms=312.9` mit `apply_chunk.total=0.789 ms` im Ring-Buffer: Die eigentliche Arbeit pro Chunk ist schnell, das Tracing drumherum nicht.

**Zusätzlich pro Frame:** `src/perf/trace.rs:311-324` (`append_gpu_csv_line`) macht `path.exists()` (stat-Syscall) + `OpenOptions::open` + ungepuffertes `writeln!` + close — **jeden Frame**, auf dem Render-Thread (`src/render/state.rs:19689`).

**Fix:**

- Gesamtes Runtime hinter ein `perf_trace`-Cargo-Feature (default aus) **und** eine Env-Var legen.
- Alle `record_*`-Funktionen auf `impl FnOnce() -> String` (oder ein `&[(&str, Value)]`-Slice) umstellen, damit `format!` bei deaktiviertem Tracing nie ausgeführt wird.
- Serialisierung vom Hot Path nehmen: kleines POD-Event in einen SPSC-Ring pushen, ein dedizierter Writer-Thread macht JSON + I/O.
- GPU-CSV einmal beim Init öffnen und das Handle behalten.

**Billigste Messung:** `return Ok(())` an den Anfang von `trace_init` (`src/perf/trace.rs:141`), Release-Build, gleiche Route fliegen. **Erwartung:** `chunkgen pure_ms` fällt von ~300 ms auf einstellig. Zweitbeste: `ls -la logs/*.trace.json` nach 60 s Session — geschriebene MB/s sind der Contention-Proxy.

---

### 1.2 `refresh_worldgen_warmup_tracking` ist O(target_sections) pro Aufruf

**Severity:** critical · **Likelihood Symptom 1:** sehr hoch (Startup + große View-Distance) · **Status:** CONFIRMED

**Evidenz:** `src/render/state.rs:11040-11073`. Pro Aufruf:

- klont `target_sections` in einen frischen `Vec`
- klont `generated_columns` (ein `HashSet`)
- baut **drei** neue `HashSet`s in Größe `target_sections`, mit 3 Hash-Lookups pro Section
  (`chunk_is_generation_satisfied`, `startup_section_has_completed_mesh`, `startup_section_has_uploaded_mesh`)
- allokiert bedingungslos einen `format!`-Phase-String und `overlay_title().to_string()` (`state.rs:11101-11117`)

**Aufrufer:**

- `src/render/state.rs:13021` — jedes `pump_chunkgen_results` mit `drained > 0`
- `src/render/state.rs:17587` — jedes `pump_mesh_results_with_limits` mit `processed > 0`
- zusätzlich die Startup-Burst-Schleife

→ mehrfach pro Frame.

**Mechanismus:** Bei View-Distance 32 umfasst `target_sections` tausende Koordinaten. Ein kompletter Rescan pro gedraintem Batch macht den Startup quadratisch. Erklärt direkt `run_1773085075_183`: `frame=9697.5 ms`, `proc.pump_chunkgen_results=51.9 ms`, `sched[pend=8566,heap=15575]`.

**Fix:** Die drei Sets inkrementell pflegen. `mark_startup_section_applied` (`src/render/state.rs:11009`) existiert bereits — dort einfügen; Äquivalente für Mesh-Complete und Upload-Complete ergänzen; den Full-Rescan streichen. Den Progress-String nur neu bauen, wenn sich `progress_permille` tatsächlich ändert.

**Messung:** Ein `frame_perf`-Feld mit `target_sections.len() × Aufrufzahl` pro Frame, in die bestehende Stall-Zeile aufnehmen. Kein neues Subsystem nötig.

---

### 1.3 Synchroner Spin-Wait auf Chunk-Generierung im Main-Thread

**Severity:** ~~high~~ → **low** · **Likelihood Symptom 1:** gering · **Status:** CONFIRMED (Severity nachträglich korrigiert)

> **Korrektur (nach Prüfung des Aufrufpfads).** Die ursprüngliche Einstufung als „Kandidat für den
> 9.7-Sekunden-Frame" war falsch. Der Aufrufpfad ist einmalig, nicht wiederkehrend — siehe unten.

**Evidenz:** `src/render/state.rs:12732-12759` — `ensure_column_loaded_or_generated` läuft bis zu **512 Iterationen**, jede mit Generierungs-Requests, `pump_chunkgen_results()` und `std::thread::yield_now()`. Blockiert den Main-Thread, bis der Worker liefert.

**Aufrufpfad:** `src/app/mod.rs:1515` in `world_spawn_settled_feet_near` ← `src/app/mod.rs:2423`. Das läuft **genau einmal**, beim Übergang `GeneratingWorld` → `InGame`, also während des Ladebildschirms — nicht im Gameplay und nicht beim Streaming. Die Suche deckt bis zu (2·8+1)² = 289 Spalten ab, die aber überwiegend schon vom Startup-Warmup generiert sind; gespinnt wird nur für Spalten außerhalb des Warmup-Radius.

**Mechanismus:** Kann den Übergangs-Frame verlängern, erklärt aber **nicht** die gemeldete Lag beim Bewegen. Die 9.7-s-Frames aus `run_1773085075_183` gehen auf §1.1/§1.2 zurück.

**Fix:** Aufrufer soll eine noch nicht fertige Column tolerieren (`Poll::Pending` zurückgeben, nächsten Frame erneut versuchen) statt zu spinnen.

**Verwandt:** `save_world` (`src/render/state.rs:14147-14172`) hat dasselbe Muster mit 4096 Spins. Dort akzeptabel (expliziter Save), sollte aber nie auf einem Gameplay-Frame laufen.

**Messung:** `perf_trace::instant_event` bei Entry/Exit mit Spin-Count — oder in bestehenden Traces nach `frame.total`-Spikes suchen, die mit dem Aufrufer korrelieren.

---

### 1.4 Mesh-Upload-Queue verhungert unter Last (Priority Inversion)

**Severity:** high · **Likelihood Symptom 1:** hoch · **Status:** CONFIRMED

**Evidenz aus den Logs:**

- `run_1787146900_888`: `mesh(wait_avg/max_ms=181.02/413.20, up_q=46=>47, uploads=30)`
- `run_1781101083_106`: `up_q=39=>62`
- `run_1773087313_576`: `up_q=17=>56`
- `run_1773086032_185`: `up_q=0=>49`

Die Queue wächst schneller, als `STREAM_MESH_UPLOAD_BUDGET_MS` sie abbaut (`src/render/state.rs:10513-10549`).

Der Mesh-**Job**-Channel ist `unbounded` (`src/render/state.rs:5678`, `9692`) mit FIFO-Workern und **ohne Priorität**. `inflight(mesh=101..148)` bedeutet: ferne Chunks werden vor nahen gemesht und landen dann in der Upload-Queue.

**Mechanismus:** Priority Inversion. `reprioritize_mesh_upload_queue` sortiert die *Upload*-Queue — zu diesem Zeitpunkt ist die CPU-Meshing-Arbeit für die falschen Chunks aber schon verbraucht.

**Fix:** In-Flight-Mesh-Jobs deckeln (bounded Channel plus eine umsortierbare Pending-Queue), sodass die Dispatch-Reihenfolge die aktuelle Kamera-Priorität widerspiegelt statt der Enqueue-Reihenfolge. Den bestehenden `MeshUploadPriorityKey` für die Upload-Stufe beibehalten.

**Messung:** Bereits instrumentiert — `mesh_upload.queue_wait`-Events mit `near_visible=true` beobachten. Wenn Near-Visible-Wartezeiten >100 ms bleiben, während ferne Chunks hochgeladen werden, ist die Inversion bestätigt.

---

### 1.5 `console_error!` = ungepuffertes `eprintln!` in Hot Paths

**Severity:** medium · **Status:** CONFIRMED

**Evidenz:** `src/console.rs:9-13` — `console_error!` expandiert zu `eprintln!`. stderr ist line-buffered: ein `write(2)`-Syscall pro Aufruf, plus globaler stderr-Lock. 104 Call-Sites insgesamt.

Kritische Stellen:

- `src/render/state.rs:17604-17617` — feuert bei **jedem** Mesh-Pump, der einen Buffer erzeugt hat, also praktisch jeden Frame während des Streamings.
- `src/world/mod.rs:2425-2454` — **sechs** `eprintln!` pro `rebuild_lighting_for_snapshot`-Aufruf, ausgeführt auf einem *Lighting-Worker-Thread*, in Konkurrenz mit dem Main-Thread um den stderr-Lock.
- `src/render/state.rs:16609-16615` — pro Lighting-Rebuild-Request.

**Fix:** Über einen level-gegateten Logger mit gepuffertem Sink routen; die Per-Frame-Mesh-Upload-Zeile auf ein periodisches Aggregat reduzieren.

---

### 1.6 Trace-`format!`-Allokationen, die auch ohne Arbeit laufen

**Severity:** medium · **Status:** CONFIRMED

`src/render/state.rs:17401-17408`:

```rust
let _upload_scope = perf_trace::scope("render.upload.meshes").with_counts(format!(
    "queue={} inflight={} urgent_near={} budget_ms={:.2} speed={:.2}", …));
```

Allokiert einen `String` **jeden Frame**, unabhängig davon, ob überhaupt ein Upload stattfindet. Gleiches Muster im Follow-Up-Pump (`src/render/state.rs:12644-12660`). Wird durch Fix 1.1 mit erledigt.

---

### 1.7 Budgets werden geprüft, aber ein einzelnes Item kann sie sprengen

**Severity:** medium · **Status:** CONFIRMED

Alle Stage-Loops nutzen das Muster:

```rust
if iterations > 0 && elapsed >= budget { break; }
```

(z. B. `src/render/state.rs:12795`, `17413`, `16906ff`)

Das garantiert ≥1 Item pro Frame — korrekt so. Es gibt aber **keine Obergrenze für die Kosten eines einzelnen Items**, weshalb `budget_ms=1.25` und `pure_ms=312.9` in denselben Logs koexistieren.

Nach Fix 1.1 und 1.2 sollten die Per-Item-Kosten kollabieren. Falls nicht: Watchdog ergänzen, der (aggregiert) loggt, wenn ein Item das gesamte Stage-Budget überschreitet.

---

### 1.8 Was hier **nicht** das Problem ist

- **Keine blockierenden GPU-Waits.** Kein `device.poll(Maintain::Wait)`, kein synchrones Buffer-Mapping im Render-Pfad. Die Audit-Notiz in `src/perf/trace.rs:163` stimmt.
- **Keine 16³-Array-Kopien zwischen Stages.** `snapshot_neighborhood` (`src/render/state.rs:14376-14401`) nutzt durchgehend `Arc::clone`.
- **Keine geteilten Mutexes zwischen Main-Thread und Workern** außer dem Trace-Runtime. Die Pipeline ist echtes Message-Passing. Genau deshalb ist der Trace-Mutex so schädlich: Er ist der **einzige** Cross-Thread-Serialisierungspunkt — und er wird über Disk-I/O gehalten.
- **Worldgen ist deterministisch** — keine `HashMap`/`HashSet`-Iteration irgendwo in `src/worldgen/`.
- **Kein `unsafe` im gesamten Crate.**
- **Far-LOD ist inaktiv.** Jeder Stall-Report zeigt `lod=0, lod_dc=0, lod_mem_mb=0.00, lod_req=0, lod_results=0` — trotz `far_lod=true`. Der Far-LOD-Pfad trägt zu keinem der Symptome bei, tut aber auch seinen Job nicht. Separater Untersuchungspunkt.

---

## 3. SYMPTOM 2 — Lighting/Shading-Dropouts

Der Mechanismus ist eine Kette aus drei Dingen. Zusammen erklären sie sowohl „komplett dunkel wie Nacht" als auch „gar kein Shading".

### 2.1 Provisorisches Licht unterhalb Y=64 ist komplett 0

**Severity:** critical · **Likelihood Symptom 2:** sehr hoch · **Status:** CONFIRMED

**Evidenz:** `src/render/state.rs:16776-16798`:

```rust
let sky_fill = if world_y >= PENDING_LIGHTING_SKY_HEURISTIC_MIN_Y { 15 } else { 0 };
…
let sky = if block.blocks_skylight() { 0 } else { sky_fill };
light[idx] = LightLevel::new(sky, block.emitted_light()).pack();
```

mit `PENDING_LIGHTING_SKY_HEURISTIC_MIN_Y = 64` (`src/render/state.rs:1190`).

**Mechanismus:** Jede Chunk-Section, die vollständig unterhalb Welt-Y=64 liegt, wird mit `sky=0, block=emitted_light` geseedet. Sie wird in die Welt eingefügt, als `GEOMETRY`-dirty markiert, gemesht und hochgeladen — mit diesem Nulllicht in jeden Vertex eingebacken.

Im Shader (`src/render/shaders.rs:84-89`) hat `light_f` einen Floor von `MIN_WORLD_BRIGHTNESS = 0.045`, und `shade` hat einen Floor von `face_shade × MIN_AO = 0.5 × 0.5 = 0.25` (`src/meshing/greedy.rs:481-497`).

Nettohelligkeit ≈ **0.011** — optisch reines Schwarz, aber nicht literal 0.

**Wichtig:** Der Occlusion-Shape-Contract ist intakt. Das Problem sind die *Lichtdaten*, nicht AO oder Face-Visibility.

**Der Halo ist ebenfalls in Ordnung:** `fill_default_light` schreibt `0xF0` (sky=15) für fehlende Nachbarn (`src/meshing/neighborhood.rs:172-190`). Ein fehlender Nachbar macht einen Chunk also *heller*, nicht dunkler. Die Dunkelheit kommt ausschließlich aus dem eigenen Buffer des Center-Chunks.

**Fix:**

- Provisorisches Skylight aus dem tatsächlichen Heightmap/`sky_blocker_top` der Column seeden statt aus einer flachen Y=64-Konstante, damit sub-64-Sections, die real Himmelskontakt haben (Höhlen, Klippen, Ozeane), von Anfang an beleuchtet sind.
- Billigerer Zwischenfix: Meshes von Chunks, die noch `Provisional` und unterhalb der Heuristik-Y liegen, **nicht hochladen** — dann bleibt der Chunk unsichtbar statt schwarz. `mesh_upload_lighting_rank` (`src/render/state.rs:10409`) ranked das bereits, gated aber nicht.

**Messung:** Die `lighting.chunk_state`-Trace-Events tragen bereits `state=provisional|visual_ready|finalized` pro Koordinate. Gegen `mesh_upload.queue_wait` für dieselbe Koordinate kreuzen: Jede Koordinate, die im Zustand `provisional` und unterhalb Y=64 hochgeladen wird, ist ein garantiert schwarzer Chunk.

---

### 2.2 Der Lighting-Backlog wird nie abgebaut

**Severity:** critical · **Likelihood Symptom 2:** sehr hoch · **Status:** CONFIRMED

**Evidenz:** `lighting backlog=218 / 194 / 190 / 176` — dauerhaft, über vier separate Läufe, bei `steps=2, step_max_ms=1.50`.

**Durchsatzrechnung** aus den Konstanten (`src/render/state.rs:1184-1189`):

| Konstante | Wert |
|-----------|------|
| `STREAM_CHUNK_LIGHTING_MAX_PRIMARY_CHUNKS_PER_JOB` | **3** |
| `STREAM_CHUNK_LIGHTING_MAX_REGION_CHUNKS_PER_JOB` | 14 |
| `STREAM_CHUNK_LIGHTING_MAX_VOXELS_PER_STEP` | 48 000 |
| `STREAM_CHUNK_LIGHTING_STEP_BUDGET_MS` | 1.5 |
| `STREAM_CHUNK_LIGHTING_FRAME_BUDGET_MS` | **3.0** |
| `STREAM_CHUNK_LIGHTING_MAX_STEPS_PER_FRAME` | **4** (real gemessen: 2) |

Ein Job umfasst also max. 3 Primary-Chunks, braucht aber bei einer Region von bis zu 14 Chunks mehrere 48k-Voxel-Steps. Bei effektiv 2 Steps/Frame retired die Pipeline deutlich **unter einem Job (≈3 Chunks) pro Frame**, während Streaming bei view=32 weit mehr einreiht.

Ein 200-Job-Backlog braucht 200+ Frames ≈ 3–4 Sekunden zum Abbau — und solange man sich weiterbewegt, holt er nie auf. Jeder Chunk in diesem Backlog zeigt in der Zwischenzeit das provisorische Licht aus 2.1.

**Fix:**

- `STREAM_CHUNK_LIGHTING_MAX_PRIMARY_CHUNKS_PER_JOB` und das Frame-Budget anheben, sobald 1.1/1.2 Main-Thread-Zeit freigemacht haben.
- Strukturell: Inkrementelles Lighting auf den bestehenden `LightingWorkerPool` verlagern. Der ist derzeit nahezu idle (`light_rx=0` in **jedem** Log), und sein eigener Kommentar (`src/render/lighting_worker.rs:26-27`) sagt, dass Bulk-Rebuild nur noch ein Fallback ist. Der Worker-Pool existiert und wird nicht benutzt — die Arbeit läuft stattdessen synchron auf dem Main-Thread in 1.5-ms-Scheiben.

**Messung:** Steht bereits in der Stall-Zeile. `lighting backlog` über einen 30-s-Flug plotten — monotoner Anstieg bestätigt.

---

### 2.3 Der korrigierende Remesh wird unbegrenzt geparkt

**Severity:** high · **Likelihood Symptom 2:** hoch · **Status:** CONFIRMED

**Evidenz:** `src/render/state.rs:14729-14742`:

```rust
fn mesh_should_await_lighting_finalization(&self, coord, reasons) -> bool {
    if !reasons.is_lighting_only() || !self.chunk_meshes.contains_key(&coord) {
        return false;
    }
    self.chunk_lighting_progress.get(&coord)
        .is_some_and(|p| p.state != ChunkLightingReadiness::Finalized)
}
```

Eine `LIGHTING`-only Dirty-Markierung auf einem Chunk, der bereits ein Mesh hat, wandert nach `entry.pending` mit Zustand `AwaitingLightingFinalization`. Freigegeben wird sie **ausschließlich** durch `maybe_release_mesh_after_lighting_finalization` (`src/render/state.rs:14869`), was `Finalized` voraussetzt.

`Finalized` wird an genau **einer** Stelle gesetzt — `finalize_chunk_lighting_state` (`src/render/state.rs:15590-15650`) — und nur wenn `chunk_lighting_boundary_complete(coord)` gilt (`src/render/state.rs:15567-15588`):

```rust
for neighbor in Self::neighbor_chunk_coords(coord) {
    if neighbor.y < min_chunk_y || neighbor.y > max_chunk_y { continue; }
    if self.world.get_chunk(neighbor).is_none() { return false; }
    if self.pending_chunk_added_lighting_coords.contains(&neighbor) { return false; }
}
```

→ **alle sechs Nachbarn geladen und keiner pending.** Jeder Chunk am Rand der geladenen Region scheitert daran und bleibt `VisualReady`.

**Mechanismus:** Chunk C wird dunkel hochgeladen (2.1). Sein Lighting-Job läuft irgendwann und markiert ihn `LIGHTING`-dirty — aber der Remesh wird geparkt, weil ein Nachbar noch nicht geladen ist. Er bleibt dunkel.

Es *gibt* einen Re-Evaluierungspfad: Ein späterer Job für Nachbar N nimmt C in sein Candidate-Set auf (`src/render/state.rs:15700-15724`). Der feuert aber nur, wenn so ein Job tatsächlich läuft — und bei Backlog 200 (2.2) passiert das oft nicht in nützlicher Zeit.

**Fix:** Nicht auf `Finalized` gaten. `VisualReady` ist per Definition gut genug zum Anzeigen — den geparkten Remesh schon bei `VisualReady` freigeben und einen späteren Job verfeinern lassen. Falls das Parken der Koaleszenz dienen soll: mit einem Frame-Deadline begrenzen statt mit einer unbegrenzten Zustandsbedingung.

**Messung:** Chunks mit `mesh_schedule`-Zustand `AwaitingLightingFinalization` zählen und in der Stall-Zeile neben `dirty[g,l,b]` ausgeben. Jeder von 0 verschiedene Steady-State-Wert ist ein gestrandeter Chunk. (Einzige neue Sonde, die ich empfehle — ca. 4 Zeilen.)

---

### 2.4 Startup-deferred Lighting kann stranden

**Severity:** medium · **Status:** CONFIRMED

**Evidenz:** Warmup-Chunks werden in `startup_deferred_chunk_added_lighting_coords` gelegt — **ganz ohne Lighting-Job** (`src/render/state.rs:12934-12940`):

```rust
if self.should_defer_startup_chunk_added_lighting(coord) {
    self.note_chunk_lighting_provisional(&[coord], "startup_chunk_apply");
    self.note_chunk_lighting_visual_ready(&[coord], "startup_chunk_apply");
    self.startup_deferred_chunk_added_lighting_coords.insert(coord);
}
```

Das Set wird nur von `maybe_enqueue_startup_deferred_final_lighting` (`src/render/state.rs:15881`) geleert, was `startup_final_lighting_batch_ready()` voraussetzt. Und das gibt **sofort `false`** zurück, wenn `world_generation_task` `None` ist (`src/render/state.rs:15554-15557`).

**Zeitfenster:**

- Drain feuert bei **85 %** applizierter Sections (`STARTUP_FINAL_LIGHTING_TRIGGER_PERCENT`, `src/render/state.rs:1178`)
- Task wird bei **100 %** gecleart (`src/render/state.rs:14131`)

Chunks, die in diesem 85→100 %-Fenster appliziert werden, landen erneut im Set und stranden dann, wenn der Task auf `None` geht: dauerhaft `VisualReady` mit dem provisorischen Licht aus 2.1 — und 2.3 blockiert danach ihren Remesh für immer.

Der einzige weitere Drain ist `src/render/state.rs:5885` (Session-Shutdown), der das Set nur *löscht*, ohne die Chunks zu beleuchten.

**Fix:** Das Deferred-Set bedingungslos drainen, wenn `world_generation_task` auf `None` übergeht — **vor** dem Clearen.

---

### 2.5 `blocks_skylight` vs. `OcclusionShape` — echte Contract-Verletzungen

**Severity:** high · **Likelihood Symptom 2:** mittel–hoch · **Status:** CONFIRMED

| Block | `face_occluding` | abgeleitete `OcclusionShape` | `blocks_skylight` | Widerspruch |
|-------|------------------|------------------------------|-------------------|-------------|
| `minecraft:grass_block` (`src/blocks/mod.rs:694`) | `true` (:704) | **FullCube** | **`false`** (:706) | verdeckt Faces + AO, aber Skylight geht komplett durch |
| `minecraft:oak_leaves` (`src/blocks/mod.rs:1017`) | `false` (:1025) | **Decorative** (coverage `None`) | **`true`** (:1027) | stoppt Skylight vollständig, trägt aber **nichts** zu AO/Occlusion bei |
| `minecraft:stone_slab` (`src/blocks/mod.rs:1057`) | `true` | SlabLower (`Down`=Full) | `false` | vermutlich Absicht, aber inkonsistent zu `boundary_coverage` |

**Mechanismus (Gras):** `sky_blocker_top` wird aus `blocks_skylight()` gebaut (`src/world/mod.rs:1638`), die Grasoberfläche ist also **kein** Blocker. Skylight seedet einen Block tiefer — und entscheidend: Sky-Propagation hat **keine Abschwächung nach unten**:

```rust
let next = if dir == IVec3::new(0, -1, 0) { level } else { level.saturating_sub(1) };
```

(`src/world/mod.rs:2366-2371`)

Überall, wo Gras über Luft sitzt (Überhänge, Höhleneingänge, Klippen), flutet Skylight in voller Stärke hindurch. Das produziert falsche Beleuchtung **und** bläht die BFS-Queue auf — konsistent mit `voxels=75662, boundary=2979` in `run_1773071920_600`.

**Mechanismus (Blätter):** Bäume stempeln harte dunkle Skylight-Schatten ohne passendes AO. Das liest sich als „flach dunkle" Flecken statt als schattierte Geometrie — passt zur Beschreibung „rohe Texturen ohne Schatten/AO".

**Dritter Divergenzpfad:** Der Fallback für unbekannte Blöcke ist `OcclusionShape::Decorative` (`src/blocks/mod.rs:2207`), während `blocks_skylight()` weiterhin aus `block_spec_for` kommt (`src/blocks/mod.rs:2360`).

**Fix:** Skylight-Blocking aus `OcclusionShape` ableiten statt als unabhängiges Bool zu speichern. Natürliches Prädikat:

```rust
shape.boundary_coverage(Face::Down) == FaceCoverage::Full
```

mit expliziter Per-Block-Override nur dort, wo Transluzenz wirklich gewollt ist (Glas, Wasser). Danach das `blocks_skylight`-Feld löschen.

Falls das zu groß ist: `#[test]`, der für **jeden** Block die Übereinstimmung asserted, mit Whitelist für die gewollten Ausnahmen.

**Messung:** Der Lighting-Debug-View existiert bereits — `cycle_debug_lighting_view` Modus 3 rendert rohes Skylight (`src/render/shaders.rs:99-101`). In Modus 3 unter einen Grasüberhang fliegen; sichtbares Skylight darunter bestätigt das Leck.

---

### 2.6 Kein Fallback-/Incomplete-Pipeline-Pfad

**Status:** ausgeschlossen. Es existiert **kein** Codepfad, der Welt-Geometrie mit einer Default-Pipeline oder einer unvollständigen Bind-Group zeichnet. „Gar kein Shading" ist 2.1 — Chunks, deren eingebackene `shade`/`light`-Werte uniform sind, rendern als flache rohe Textur.

---

## 4. SYMPTOM 3 — Schwarze vertikale Säulen

### Wahrscheinlichste Erklärung: Symptom 2 in Column-Form

**Status:** SUSPECTED (hohe Konfidenz)

Die Lighting-Pipeline ist durchgängig **pro X/Z-Column** organisiert:

- `skylight_seed_columns` ist gekeyt mit `(chunk_x, chunk_z)` (`src/world/mod.rs:2250`)
- `SkylightSeedColumn` hält eine 256-Einträge-Blocker-Tabelle pro Column
- Generierung läuft pro Column (`src/render/chunkgen_worker.rs:1912-1920`)
- Dirty-Tracking läuft pro Column (`mark_column_dirty_for_chunk`)

→ Wenn Lighting für eine Column ausfällt oder stockt, ist **jede Section dieser Column** betroffen — über die volle Welthöhe.

Ein Stapel aus Sections über die volle Höhe, der mit ≈0.011 Helligkeit rendert (§2.1), ist auf dem Bildschirm exakt eine **schwarze vertikale Säule ohne inneres Shading-Detail**. Das deckt sich präzise mit der Beschreibung, inklusive der „2D"-Flachheit (uniforme Dunkelheit über die ganze Säule = keine Tiefenhinweise). Auch „gelegentlich" passt: Die Säule erscheint, wenn das Lighting einer Column verhungert, und verschwindet, wenn der Backlog sie endlich erreicht.

**Diskriminierende Messung:** Wenn eine Säule auftaucht, den Lighting-Debug-Cycle auf Modus 3 (rohes Skylight) schalten.

- Säule wird zu einer uniform schwarzen Column im Skylight-View → es ist §2.1/§2.2, und deren Fix behebt auch das.
- Säule **verschwindet** in Modus 3 (Skylight ist okay, Geometrie ist falsch) → echter Geometrie-Bug, siehe Kandidaten unten.

---

### 3.1 Boundary-Ocean-Plane-Aliasing

**Severity:** medium · **Status:** SUSPECTED

`src/render/state.rs:2783-2833` baut UVs als `world_x * BOUNDARY_OCEAN_UV_TILE_PER_BLOCK` über einen Ring, der bis zur Weltgrenze reicht. Mit `Nearest`-Filterung und ohne Mipmaps produziert ein unbegrenzter UV-Bereich auf einer bildschirmfüllenden Ebene starkes Moiré.

Taucht in den Traces auf: `render.draw.transparent … boundary_ocean=1`.

Liest sich normalerweise als horizontale Bänderung Richtung Horizont, nicht als vertikale Säulen — aber ausschließenswert, indem man den Boundary-Ocean abschaltet, während eine Säule sichtbar ist.

---

### 3.2 Greedy-AO-Interpolation über gemergte Spans

**Severity:** low · **Status:** CONFIRMED

AO wird nur an den vier Ecken eines gemergten Quads gesampelt (`src/meshing/greedy.rs:1219-1240`). Occluder im Inneren eines langen gemergten Runs werden ignoriert; AO wird zu einer linearen Rampe über die gesamte Spanne.

`MaskCell`-Gleichheit steuert das Mergen — wenn sie die AO-Werte nicht enthält, mergen benachbarte Zellen mit unterschiedlicher Occlusion trotzdem.

Ergebnis: sichtbare Bänderung/Streifen entlang gemergter Runs. Kosmetisch, aber eine echte Quelle für „streifige" Shading-Artefakte.

**Fix:** Die 4 gepackten AO-Werte in die `MaskCell`-Gleichheit aufnehmen, damit das Mergen an AO-Diskontinuitäten stoppt (Standardpraxis), zum Preis von etwas mehr Quads.

---

### 3.3 Erreichbarer `f32::clamp`-Panic in der Precipitation

**Severity:** low · **Status:** CONFIRMED

`src/render/state.rs:18418-18420`:

```rust
let top_y = (center_y + top_offset - phase * vertical_drop)
    .clamp(terrain_y + min_length, max_y);
let bottom_y = (terrain_y.max(center_y - bottom_offset)).clamp(min_y, top_y - 0.25);
```

`f32::clamp` **panict**, wenn `min > max`.

- Wenn `terrain_y + min_length > max_y` (Terrain nahe Weltdecke) → erster Aufruf panict.
- Wenn `top_y - 0.25 < min_y` → zweiter Aufruf panict.

Keine Säulen-Ursache, aber ein Crash, der auf die richtige Terrainhöhe wartet.

**Fix:** `.max(a).min(b)` statt `.clamp(a, b)`, oder die Grenzen vorher selbst clampen.

---

## 5. ALLGEMEINE SCHWACHSTELLEN

| Sev | Ort | Problem | Fix |
|-----|-----|---------|-----|
| critical | `src/perf/trace.rs:237-268`, `src/main.rs:34` | Bedingungsloses Tracing; globaler Mutex über JSON + Datei-I/O gehalten; 558 MB/Session | Feature-Gate; lazy `FnOnce`-Counts; dedizierter Writer-Thread |
| critical | `src/render/state.rs:11040` | Kompletter Startup-Set-Rebuild + 2 Collection-Clones + `format!` pro Aufruf, mehrfach pro Frame | Sets inkrementell pflegen |
| high | `src/render/state.rs:12732` | 512-Iterationen-Main-Thread-Spin, wartet auf Chunkgen-Worker | Aufrufer async-tolerant machen |
| high | `src/render/state.rs:18898-19242` | `create_buffer_init` an 17 Stellen in `render()` — **11 davon ohne Cache-Gate**, siehe Korrektur in §5.1 | Persistente Grow-on-Demand-Buffer + `write_buffer`; verletzt `.claude/rules/render.md` |
| high | `src/render/mesh_gpu.rs:196-260` | Pro-Chunk-pro-Layer `wgpu::Buffer` (bis zu 6/Chunk), keine Suballocation; `grown_capacity` = `next_power_of_two` → bis zu 2× VRAM-Verschwendung | Layer aus einer großen persistenten Arena suballozieren; bestehenden Reuse-on-Fit-Pfad behalten |
| high | `src/render/state.rs:5678` | Mesh-Job-Channel `unbounded` + FIFO-Worker → ferne Chunks verhungern nahe (`inflight(mesh=148)`) | Bounded Channel + umsortierbare Pending-Queue |
| medium | `src/console.rs:9-13`; `src/world/mod.rs:2425-2454`; `src/render/state.rs:17604` | `eprintln!` (ungepuffert, globaler Lock) pro Mesh-Pump und ×6 pro Lighting-Job, aus Worker-Threads | Level-gegateter, gepufferter Logger |
| ~~medium~~ → **kein Befund** | `src/meshing/pool.rs:63-70` | `take_mesh_data` allokiert zwar ≈410 KB pro Aufruf, wird aber **nur in Tests** aufgerufen (`pool.rs`, `pool_regression_test.rs`) — kein Produktionspfad | keine Maßnahme; Einstufung war falsch |
| medium | `src/meshing/pool.rs:152-158` | `reserve_if_hint` ruft `Vec::reserve` (additiv) mit monoton wachsenden Hints aus `update_hint` → Kapazität ratcht auf den größten je gemeshten Chunk hoch | ✅ behoben: `reserve_exact` nur auf den Fehlbetrag, Hint klingt jetzt ab |
| medium | `src/render/state.rs:18874` | `transparent_chunks = visible_chunks.clone()` + zweiter Full-Sort, jeden Frame | ✅ behoben: `visible_chunks.iter().rev()`, Clone und Sort entfallen |
| medium | `src/render/state.rs:18898-18906` | Partikel-Vertex-/Index-Buffer jeden Frame neu gebaut | ✅ behoben mit §5 (`frame_mesh_slots`) |
| medium | `Cargo.toml` | Feature `allocation_tracking` wird in `src/meshing/liquids.rs:10` und `:653` benutzt, ist aber **nirgends deklariert** → `cargo build --features allocation_tracking` schlägt fehl; der Tracker ist unerreichbar | `[features]\nallocation_tracking = []` ergänzen |
| medium | `src/render/state.rs:18418` | Erreichbarer `f32::clamp`-Panic (min > max) in der Precipitation | ✅ behoben: `.max().min()` |
| low | `src/meshing/neighborhood.rs:60-64` | `resize()` gefolgt von `fill()` — drei redundante Full-Fills à 5832 Elemente pro Mesh-Job | ✅ behoben: `clear()` + `resize()`, ein Durchlauf statt zwei |
| low | `src/meshing/neighborhood.rs:176-182` | Toter Zweig: `if world_y > WORLD_MAX_Y { 0xF0 } else { 0xF0 }` | ✅ behoben: zusammengefasst, `0xF0` als benannte Konstante `FULL_SKYLIGHT_PACKED`; `< 0` → `0` ist beabsichtigt (Weltboden) |
| low | `src/render/mesh_gpu.rs:216` | `format!("{label_prefix} Vertex Buffer")` allokiert einen `String` pro Buffer-Erzeugung | ✅ behoben: Label nur noch unter `debug_assertions`, sonst `None` |
| ~~low~~ → **teilweise falsch** | `src/render/state.rs:10496-10510` | `count_urgent_near_mesh_uploads` scannt die Upload-Queue einmal pro Pump — **nicht** zweimal: die beiden Aufrufstellen sind Alternativzweige derselben `if let`. Bei Queue-Längen von 13–62 unkritisch | zurückgestellt |
| medium | `src/render/state.rs:16157-16177` | **`pump_removed_lighting_batches` bricht unbedingt nach dem ersten Batch ab** — siehe §D.1 | Entscheiden: `if let` (Verhalten festschreiben) oder `break` entfernen (Budget wirken lassen) |
| medium | Build/CI | **`cargo clippy --all-targets -- -D warnings` schlägt fehl** — siehe §D.2 | Lint aus §D.1 auflösen, dann Warning-Schulden abtragen |
| — | gesamtes Crate | **Null `unsafe`-Blöcke.** `unwrap`/`expect`-Dichte niedrig und auf Init-/Test-Pfade konzentriert (state.rs: 5, greedy.rs: 12, world/mod.rs: 19, region_file.rs: 14, chunkgen_worker.rs: 13) | keine Maßnahme |

### 5.1 — Korrektur: „17 `create_buffer_init` pro Frame"

Die ursprüngliche Formulierung zählte **Code-Stellen**, nicht Ausführungen pro Frame. Präzisiert:

| Mesh | Cache-Gate | pro Frame? |
|------|-----------|-----------|
| Inventory-, Chat-, Hotbar-Overlay (6 Stellen) | ja (`*OverlayCacheKey`) | nur bei Änderung |
| Particle (2) | nein | nur wenn Partikel existieren |
| Precipitation (1) | nein | nur bei Niederschlag |
| Generating-Overlay (2) | nein | nur im Ladebildschirm |
| Menu-Overlay (2) | nein | nur im Menü |
| Debug-Overlay (2) | nein | wenn Debug-HUD an (Text ändert sich ohnehin jeden Frame) |
| First-Person-Overlay (2) | nein | **jeden Gameplay-Frame** |

Im normalen Gameplay laufen also **2–7** Buffer-Erzeugungen pro Frame, nicht 17. Das First-Person-Overlay ist der einzige bedingungslose Fall. Die Einstufung *high* bleibt: Buffer-Erzeugung plus Freigabe am Frame-Ende erzeugt genau den Jitter, den das Designziel „stabile Frame-Zeiten" ausschließt — aber die Größenordnung war überzeichnet.

### D.1 — `pump_removed_lighting_batches` verarbeitet nur einen Batch pro Frame

**Severity:** medium · **Status:** CONFIRMED · **Gefunden:** beim Umsetzen von §2.2

`src/render/state.rs:16157-16177`. Die Schleife sieht aus wie ein budgetierter Pump, endet aber mit einem **unbedingten `break`**:

```rust
while let Some(batch) = self.pending_removed_lighting_batches.pop_front() {
    if started.elapsed() >= frame_budget {
        self.pending_removed_lighting_batches.push_front(batch);
        break;
    }
    // … genau ein Batch verarbeiten …
    break;          // ← unbedingt
}
```

Das `while let` ist damit faktisch ein `if let`: pro Frame wird **ein** Batch abgearbeitet, unabhängig vom Budget. Die Budget-Prüfung greift nur, wenn die Zeit schon beim Eintritt überschritten war.

Clippy meldet das als `error: this loop never actually loops` — der einzige echte Lint-Error im Crate (siehe §D.2).

**Warum das lange folgenlos blieb:** die Queue war fast immer leer, weil der Unload-Pfad seine Lighting-Reparatur gar nicht einreihte, sondern synchron erledigte — siehe §D.3. Damit war der Pump nur für Saved-Load-Batches zuständig und ein Batch pro Frame reichte.

**Status: ✅ behoben.** Mit §D.3 füllt der Unload-Pfad die Queue jetzt tatsächlich (bis zu `STREAM_EVICT_CHUNKS_PER_FRAME` = 24 Batches pro Frame), also *muss* sie auch abgebaut werden dürfen. Das `break` ist entfernt, `STREAM_CHUNK_LIGHTING_REMOVED_FRAME_BUDGET_MS` ist damit ein echtes Budget. Nebeneffekt: der einzige Clippy-**Error** im Crate ist weg (siehe §D.2).

### D.3 — Chunk-Entladung repariert Lighting synchron und unbudgetiert

**Severity:** high · **Status:** CONFIRMED · **Gefunden:** in `logs/run_1787160799_221` (Lauf nach den §1.1/§1.2-Fixes) · **Status: ✅ behoben**

Gemessen:

```
frame=215.4ms  tops=[lighting=188.0ms,...]
  proc.unload_chunks_outside_keep_radius = 188.6 ms
  lighting(voxels=165857, inc_chunks=10, backlog=673)
```

`unload_chunks_outside_keep_radius` (`src/render/state.rs`) budgetiert seine Evict-Schleife sauber (`STREAM_EVICT_CHUNKS_PER_FRAME` = 24, `STREAM_EVICT_BUDGET_MS` = 1.5). **Nach** der Schleife rief es aber `run_incremental_chunk_removed_lighting(&removed_chunk_coords, …)` — synchron, für alle in diesem Frame entfernten Chunks auf einmal, ohne jedes Budget. Ein Frame verbrachte dort 188.6 ms und besuchte 165 857 Voxel.

**Nicht neu:** derselbe Peak steht mit **192.9 ms** schon im Vorlauf `run_1787146900_888`, also vor allen Fixes. Er war nur unsichtbar, weil der Tracing-Overhead (§1.1) alles überlagerte. Keine Regression — ein freigelegter Altbestand.

**Fix:** `enqueue_removed_lighting_batches(…)` statt des synchronen Aufrufs. Damit landet die Arbeit in genau der Queue, die `pump_removed_lighting_batches` unter Budget abbaut — die Infrastruktur existierte bereits, der Unload-Pfad umging sie nur. Zwingend dazu gehört §D.1 (das `break` musste weg). Die Readiness-Einträge der evakuierten Chunks werden jetzt direkt beim Evict entfernt, statt auf das verzögerte Lighting zu warten.

**Wirkung:** die Spitze fällt von 188 ms auf ~8–19 ms pro Frame (Batch-Größe ist `SAVED_LOAD_APPLY_LIGHTING_BATCH_PER_STEP` = 1 Chunk, gemessene Kosten ≈ 8–19 ms pro Chunk).

**Was noch offen ist:** das Budget wird *vor* jedem Batch geprüft, ein einzelner Batch kann es also weiterhin überschreiten. Wirklich gedeckelt wäre die Removal-Reparatur erst, wenn sie wie der Added-Pfad über `begin_incremental_region_lighting_task` + `step_incremental_lighting_task` läuft (dort greifen `MAX_VOXELS_PER_STEP` und `STEP_BUDGET_MS` innerhalb eines Jobs). `lighting_on_chunks_removed` ist bisher One-Shot.

### D.2 — Der dokumentierte Lint-Befehl schlägt fehl

**Severity:** medium · **Status:** CONFIRMED

`CLAUDE.md` nennt `cargo clippy --all-targets -- -D warnings` als Lint-Kommando. Das schlägt aktuell fehl, und zwar aus zwei unabhängigen Gründen:

1. ~~**Ein echter Lint-Error:** `this loop never actually loops` (§D.1). Der bricht den Build schon ohne `-D warnings`.~~ → **✅ behoben** mit §D.1. `cargo clippy --all-targets` läuft jetzt fehlerfrei durch (0 Errors, vorher 3).
2. **Warning-Schulden:** ~109 Warnungen im Crate (`cargo check`), unter Clippy ~205. Ganz überwiegend `never used` für Konstanten, Felder und Methoden (`src/perf/allocation_tracker.rs` komplett, `stall_file_path`, `gpu_csv_path`, `note_gpu_wait`, diverse UI-Konstanten). Mit `-D warnings` ist jede davon fatal. **Noch offen.**

Beides war vorbestehend und unabhängig von den Fixes — gegengeprüft gegen `8f7634d`: dort identische Fehlerzahl.

**Rest-Fix:** die Dead-Code-Warnungen in einem eigenen Durchgang abräumen (löschen oder `#[allow(dead_code)]` mit Begründung dort, wo bewusst Vorrat gehalten wird — z. B. `allocation_tracker`, das erst mit dem jetzt deklarierten Feature nutzbar wird). Solange das offen ist, ist `-D warnings` in `CLAUDE.md` irreführend.

### D.4 — Die oberste `tops`-Kategorie hatte keinen Unload-Bucket

**Severity:** low (Instrumentierung) · **Status:** CONFIRMED · **Status: ✅ behoben**

`format_stall_report` baut zwei Listen: eine grobe Kategorienliste (`tops=[…]`) und eine feine `proc.*`-Liste. Die feine Liste enthielt `proc.unload_chunks_outside_keep_radius` von Anfang an und hat den 188-ms-Posten auch korrekt ausgewiesen. Der **groben** Liste fehlte ein Unload-Bucket — und weil die vom Unload ausgelöste Removal-Reparatur in `rebuild_world_lighting_ms` landet, las sich der Frame als `tops=[lighting=188.0ms]`. Das zeigt auf das falsche Subsystem.

**Fix:** eigener `unload`-Bucket in der Kategorienliste, und `take(3)` → `take(4)`, weil die Liste jetzt acht Einträge hat.

> **Korrektur zu einer früheren Aussage von mir:** ich hatte behauptet, die `tops`-Liste führe Unload „gar nicht auf". Das stimmt so nicht — die `proc.*`-Liste tut es. Der eigentliche Grund, warum das Audit den Posten übersah: ich habe mich auf die `summary:`-Zeilen verlassen, die nur die Zähler *des einen* auslösenden Frames enthalten. Die 192.9-ms-Spitze im Vorlauf lag in einem anderen Frame und stand nur im Ring-Buffer. Für künftige Analysen: immer auch den Ring-Buffer nach den größten Scopes durchsuchen, nicht nur die Summary lesen.

---

## 6. NÄCHSTE MESSUNGEN

Sortiert nach Erkenntnis pro Minute. Die ersten beiden brauchen **keinen** neuen Code.

### 1. Tracing abschalten, neu messen ⭐ zuerst

`return Ok(())` an den Anfang von `trace_init` (`src/perf/trace.rs:141`), Release neu bauen, dieselbe Route fliegen.

**Erwartung:** Wenn #1 die dominante Ursache ist, fällt `chunkgen pure_ms` von ~300 ms auf einstellig.

Dieses eine Experiment trennt „Instrumentierungs-Overhead" von „echter Arbeit" — und sollte **vor allem anderen** laufen, weil jede andere Zahl in den vorhandenen Logs davon kontaminiert ist.

### 2. `lighting backlog` über einen 30-s-Flug plotten

`backlog=` aus den Stall-Summaries greppen, oder ein `console_info!` pro Sekunde.

**Erwartung:** monotoner Anstieg. Bestätigt §2.2 und liefert den nötigen Durchsatz-Multiplikator.

### 3. Lighting-State gegen Uploads kreuzen

Beide Events liegen bereits in `logs/run_*.trace.json`:

```
jq -r 'select(.name=="lighting.chunk_state" or .name=="mesh_upload.queue_wait") | …'
```

Menge der Koordinaten bilden, die im Zustand `provisional` hochgeladen wurden.

**Erwartung:** große Menge, stark gewichtet auf `y < 4` (Welt-Y < 64). Das ist die Schwarz-Chunk-Population. Gruppierung nach `(x, z)` zeigt, ob sie in Columns clustert — der direkte Test der Säulen-Hypothese aus §4.

### 4. Skylight-Debug-View an einem Grasüberhang

Auf Lighting-Debug-Modus 3 schalten, unter einen Überhang schauen.

**Erwartung:** Skylight, wo Schatten sein sollte → bestätigt §2.5.

### 5. `AwaitingLightingFinalization`-Zählung

Ein neuer `frame_perf`-Zähler, in der bestehenden Stall-Zeile neben `dirty[g,l,b]` ausgegeben.

**Erwartung:** von 0 verschiedener Steady-State-Wert = gestrandete Chunks (§2.3).

Die **einzige** neue Sonde, die ich empfehle — ca. 4 Zeilen.

### 6. GPU-Buffer-Erzeugungsrate

`mesh_gpu_buffers_created` wird bereits in `frame_perf` getrackt, steht aber nicht in der Stall-Summary. Ergänzen.

**Erwartung:** sollte im Steady State gegen 0 gehen. Falls nicht, greift der Reuse-Pfad in `upload_world_cpu_mesh_to_gpu_reuse` nicht, und die Arena-Arbeit (§5) wird dringend.

---

### Caveat zu den vorhandenen Logs

Die neueste Aufzeichnung (`run_1787146900_888`, heute) und die beiden aus Mai/Juni sind zeitlich deutlich nach dem März-Batch — zeigen aber **dieselben** Backlog- und Wartezeit-Signaturen. Das sind also langjährige Zustände, keine Regressionen.

Außerdem wurden mehrere Stall-Reports bei `menu=1` / `in_game=0` aufgenommen (z. B. `run_1773082954_380`), einige Frame-Zeiten dort sind also Menü-Frames, keine Gameplay-Frames. Alle oben zitierten `chunkgen pure_ms`- und `lighting backlog`-Werte stammen aus `in_game`/Streaming-Frames.

---

## UMSETZUNGSSTAND

Der Report oben ist die Analyse auf `8f7634d`. Diese Tabelle hält nach, was davon umgesetzt ist.

| § | Befund | Status | Wo |
|---|--------|--------|-----|
| §1.1 | Tracing bedingungslos aktiv, globaler Mutex über Datei-I/O | ✅ behoben | `188fdb8` |
| §1.2 | `refresh_worldgen_warmup_tracking` O(n·m) pro Drain | ✅ behoben | `188fdb8` |
| §2.1 | Provisorisches Licht unter Y=64 komplett 0 | ✅ behoben | working tree |
| §2.2 | Lighting-Backlog wird nie abgebaut | 🟡 Durchsatz angehoben | working tree |
| §2.3 | Remesh geparkt bis `Finalized` | ✅ behoben | working tree |
| §2.4 | Startup-deferred-Set strandet | ✅ behoben | working tree |
| §5 | Per-Frame-GPU-Buffer für Overlays/Partikel/Precipitation | ✅ behoben | working tree |
| §1.5 | `eprintln!` in Hot Paths | ✅ behoben | working tree |
| §1.3 | 512-Iterationen-Spin im Main-Thread | ✅ behoben | working tree |
| §5 (klein) | Precipitation-Panic, `transparent_chunks`-Clone, Pool-Ratchet, Neighborhood-Fills, Buffer-Labels | ✅ behoben | working tree |
| §1.4 | Mesh-Job-Priority-Inversion | offen (bewusst) | |
| §2.5 | `blocks_skylight` vs. `OcclusionShape` | offen (bewusst) | |
| §5 (Rest) | Chunk-Mesh-Suballocation statt Buffer pro Layer | offen (bewusst) | |
| §2.2 (Rest) | Lighting auf den Worker-Pool | offen (bewusst) | |
| §D.3 (Rest) | Removal-Lighting stufen | offen (bewusst) | |
| §D.1 | `pump_removed_lighting_batches` bricht nach einem Batch ab | ✅ behoben | working tree |
| §D.2 | Lint-Befehl aus `CLAUDE.md` schlägt fehl | 🟡 Clippy-Errors weg, Warning-Schulden offen | working tree |
| §D.3 | Chunk-Entladung repariert Lighting synchron (188 ms) | ✅ behoben | working tree |
| §D.4 | `tops`-Kategorienliste ohne Unload-Bucket | ✅ behoben | working tree |

### Was konkret geändert wurde

**§1.1 — Tracing.** Drei Stufen über `MC_KLON_TRACE=off|ring|full`; Cargo-Feature `perf_trace` verschiebt nur den Default, die Env-Var gewinnt. Neuer Default `ring`: Stall-Reports und GPU-CSV bleiben, der JSON-Eventstream ist opt-in. JSON-Serialisierung und Datei-I/O laufen im Thread `perf-trace-writer`, Übergabe per `bounded(16384)` + `try_send` — Producer blockieren nie, Überlauf wird gezählt und beim Shutdown gemeldet. GPU-CSV-Handle wird einmal geöffnet statt pro Frame `stat`+`open`+`close`. Thread-Label in einem `thread_local` gecacht. 4 Tests auf der Level-Logik.

**§1.2 — Warmup-Tracking.** Der Kostentreiber war schlimmer als analysiert: `startup_section_has_completed_mesh` machte pro Section einen Linearscan über `mesh_upload_queue`, also O(n·m). Jetzt einmal ein `pending_uploads`-Set → O(n+m). Rescan max. einmal pro Frame (Reset in `begin_frame_profiling`, **nicht** über `frame_index` — der steht während `Generating` still). `generated_columns`-Clone-Roundtrip entfernt, Sets werden wiederverwendet, Phase-String nur bei geänderten `stats`.

**§2.1 — Provisorisches Licht.** Statt `world_y >= 64` jetzt die Worldgen-Oberflächenschätzung dieser Chunk-Spalte (`estimated_surface_height_for_column_cached`, ein gecachtes Sample pro Spalte) als Startwert, dann pro Spalte von oben nach unten: Skylight endet am ersten Blocker, wie beim echten BFS-Seed. Korrigiert beide Fehlrichtungen — begrabene Luftblasen über Y=64 werden nicht mehr hell, freiliegendes Terrain unter Y=64 nicht mehr schwarz. `PENDING_LIGHTING_SKY_HEURISTIC_MIN_Y` entfernt.

**§2.2 — Durchsatz.** `FRAME_BUDGET_MS 3.0 / STEP_BUDGET_MS 1.5` ergab exakt 2 Steps/Frame — genau der Messwert aus den Logs, während `MAX_STEPS_PER_FRAME = 4` nie band. Frame-Budget 3.0 → 6.0, Step-Cap 4 → 8, Job-Granularität 3 → 6 Primary / 14 → 24 Region. `pump_removed_lighting_batches` teilte sich die Konstante und hätte das Worst-Case-Lighting pro Frame verdoppelt → eigene Konstante, Verhalten unverändert. **Der strukturelle Fix steht weiter aus:** der `LightingWorkerPool` ist nach wie vor idle (`light_rx=0`), das inkrementelle Lighting läuft weiter auf dem Main-Thread.

**§2.3 — Parking.** `mesh_should_await_lighting_finalization` parkt nur noch bei `Provisional` statt bis `Finalized`. Die Freigabe musste zusätzlich überhaupt erst auslösen: `maybe_release_mesh_after_lighting_finalization` → `…_after_lighting_progress`, aufgerufen bei **beiden** Vorwärtsübergängen sowie in `note_chunk_lighting_visual_ready`. Logik als freie Funktion `lighting_state_defers_remesh` extrahiert, 3 Regressionstests.

**§2.4 — Stranden.** `startup_final_lighting_batch_ready()` liefert `true` statt `false`, wenn kein Worldgen-Task mehr existiert, plus expliziter Drain direkt nach `world_generation_task = None`.

**§5 — Per-Frame-GPU-Buffer.** Neues `frame_mesh_slots: HashMap<&'static str, ChunkMesh>` auf `State` plus `upload_frame_mesh` / `clear_frame_mesh` / `frame_mesh_to_draw`. Die Uploads laufen über das vorhandene `upload_cpu_mesh_to_gpu_reuse`, das nur bei Kapazitätsmangel neu alloziert und sonst `write_buffer` benutzt. Umgestellt: Particle, Generating-, Menu-, Debug- und First-Person-Overlay (10 Stellen). Precipitation ist vertex-only (Line-List) und hat ein eigenes `precipitation_vertex_buffer` mit Grow-on-Demand (1 Stelle). Ein leeres Mesh setzt `index_count = 0` und behält die Allokation, statt sie freizugeben. Die drei cache-gated Overlays blieben unangetastet — sie erzeugen ohnehin nur bei Key-Änderung Buffer.

Eine Falle dabei: `showing_pause_menu` wurde aus `menu_overlay_mesh.is_some()` abgeleitet, also aus der Existenz des gerade gebauten Mesh. Da das Mesh jetzt persistent ist, wird die Flag vor dem Konsumieren aus `menu_overlay.is_some()` genommen.

**§1.5 — `eprintln!` in Hot Paths.** Zwei Stellen mit Log-Evidenz:
- `rebuild_lighting_for_snapshot` (`src/world/mod.rs`) emittierte **sechs** `eprintln!` pro Aufruf, auf einem Lighting-Worker-Thread, jedes mit prozessweitem stderr-Lock und eigenem `write(2)`. Jetzt ein einzelnes `record_duration_ms`-Event mit denselben Zahlen — kostet nichts, wenn Tracing aus ist.
- Die Mesh-Upload-Zeile feuerte bei `created_buffers > 0`, also während des Streamings praktisch jeden Frame. Die Bedingung ist jetzt rein zeitbasiert; die Buffer-Zahl steht ohnehin in `frame_perf.mesh_gpu_buffers_created` und damit im Stall-Report.

**§1.3 — Spawn-Spin.** `ensure_column_loaded_or_generated` busy-spinnte mit `yield_now()` über bis zu 512 Iterationen und verbrannte dabei einen Kern, den die Worldgen-Worker gebrauchen konnten. Jetzt `sleep(250µs)` zwischen den Polls und eine Wanduhr-Schranke (5 s) statt einer Iterationszahl — eine hängende Spalte gibt nach vorhersagbarer Zeit auf und loggt das, statt still abzubrechen. Blockierend bleibt es bewusst: es läuft einmal während des Ladebildschirms, und die Spawn-Platzierung braucht die Spalte.

**§5 (kleine Posten).**
- Erreichbarer `f32::clamp`-Panic in der Precipitation (`clamp` panict bei min > max; beide Grenzen kommen aus der Terrainhöhe) → `.max().min()`.
- `transparent_chunks = visible_chunks.clone()` plus zweiter Full-Sort pro Frame → die Transparent-Reihenfolge ist exakt die Umkehrung, also `visible_chunks.iter().rev()`. Clone und Sort entfallen ersatzlos.
- `reserve_if_hint` rief `Vec::reserve` (additiv auf die Länge) mit absoluten Hints, `update_hint` ratchte nur nach oben → jetzt `reserve_exact` auf den Fehlbetrag, und der Hint klingt ab, statt sich auf dem größten je gemeshten Chunk festzusetzen.
- `neighborhood.rs`: `resize()` + `fill()` lief zweimal über alle 5832 Elemente → `clear()` + `resize()`. Der tote Zweig in `fill_default_light` ist zusammengefasst, `0xF0` heißt jetzt `FULL_SKYLIGHT_PACKED`.
- Buffer-Labels in `mesh_gpu.rs` werden nur noch unter `debug_assertions` formatiert.

Pool-Regressionstests (`pool_regression_test.rs`, von `.claude/rules/meshing.md` gefordert) laufen durch.

**§D.2 (Teil) — Warning-Schulden.** `allocation_tracker` wurde bedingungslos kompiliert, aber nur unter `#[cfg(feature = "allocation_tracking")]` benutzt — jetzt ist das Modul selbst hinter dem Feature. 109 → 102 Warnungen. Nebenbei erstmals verifiziert, dass `cargo check --features allocation_tracking` überhaupt baut (das Feature war bis zum Cargo.toml-Fix gar nicht deklariert).

### Messlauf nach §1.1/§1.2 — `logs/run_1787160799_221`

Erster Lauf mit dem neuen Tracing-Default. Belege:

**Kein `trace.json`.** Der Vorlauf `run_1787146900_888` schrieb 558 MB, dieser Lauf null. Nur `stall.txt` + `gpu.csv`. §1.1 ist aktiv.

**Chunk-Apply auf dem Main-Thread** (`pure_ms / applied` aus den `summary:`-Zeilen):

| Lauf | applied | pure_ms | pro Chunk |
|---|---|---|---|
| `run_1773085075_183` | 1 | 100.9 | 100.9 ms |
| `run_1781101083_106` | 3 | 312.9 | 104.3 ms |
| `run_1779800309_641` | 17 | 201.4 | 11.8 ms |
| `run_1773087313_576` | 3 | 20.9 | 7.0 ms |
| `run_1773082954_380` | 9 | 21.5 | 2.4 ms |
| **`run_1787160799_221`** | **25** | **12.7** | **0.51 ms** |

Der neue Lauf appliziert 25 Chunks in 12.7 ms, wo die alten 1–17 Chunks für 17–313 ms brauchten. Faktor 5–200 je nach Vergleichslauf.

**`cpu_render_ms`, session-weit** (`gpu.csv`, 10 018 bzw. 205 064 Frames): max **158.4 → 57.8 ms**, mean 5.23 → 4.46 ms. Der Median ist minimal schlechter (3.61 → 3.91 ms) — bei unterschiedlichen Sessions und Spielsituationen Rauschen, kein Signal.

**Nicht belastbar:** ein Vergleich von `frame.total` aus den Ring-Buffern. Der Ring erfasst per Konstruktion nur die 10 Sekunden **um einen Stall herum**, in beiden Läufen aus verschiedenen Szenarien. Daraus lässt sich weder Verbesserung noch Verschlechterung ableiten.

**Neuer Hauptverursacher:** §D.3 (Unload-Lighting, 188.6 ms) — vorbestehend, vorher vom Tracing-Overhead verdeckt.

**Nebenbefund:** `lighting.chunk_finalized` meldet Latenzen von **100–113 Sekunden** von `provisional` bis `finalized`. Keine Frame-Kosten, aber ein Beleg dafür, wie weit die Finalisierung hinterherhinkt. Dass die Chunks trotzdem nicht dunkel bleiben, liegt am §2.3-Fix — sie werden schon bei `VisualReady` neu gemesht.

### Bewusst offen gelassen

Diese Punkte sind nicht vergessen, sondern zurückgestellt — jeder aus einem konkreten Grund:

| § | Warum nicht jetzt |
|---|---|
| §1.4 Mesh-Job-Priority-Inversion | Erfordert bounded Channel + umsortierbare Pending-Queue. Ein falsch gesetztes In-Flight-Limit kann Starvation oder Deadlock erzeugen; das gehört gemessen, nicht geraten. Die Messgrundlage (`mesh wait_avg/max`) stammt noch von vor allen Fixes. |
| §5 (Rest) Chunk-Mesh-Suballocation | Arena-Allokator für bis zu 6 Buffer pro Chunk. Größerer Umbau in `mesh_gpu.rs` mit Lebensdauer-/Fragmentierungsfragen; ohne Messung des Ist-Zustands nicht sinnvoll priorisierbar. |
| §2.2 (Rest) Lighting auf den Worker-Pool | Verschiebt inkrementelles Lighting vom Main-Thread auf den (heute idlen) `LightingWorkerPool`. Betrifft die Konsistenz zwischen Weltzustand und Lighting-Snapshot — ein Nebenläufigkeitsproblem, kein Tuning. |
| §D.3 (Rest) Removal-Lighting stufen | `lighting_on_chunks_removed` müsste auf die Step-API umgestellt werden. Die akute Spitze (188 ms) ist bereits weg; der Rest ist Feinschliff. |
| §2.5 `blocks_skylight` vs. `OcclusionShape` | Ändert sichtbar die Beleuchtung von Gras, Blättern und Slabs im gesamten Terrain. Ohne visuelle Prüfung im laufenden Spiel nicht verantwortbar. Der abgesicherte Zwischenschritt wäre ein Test, der die Übereinstimmung für jeden Block asserted, mit Whitelist für gewollte Ausnahmen. |
| §3.2 Greedy-AO über gemergte Spans | `MaskCell`-Gleichheit um die AO-Werte erweitern stoppt das Mergen an AO-Kanten — korrekter, aber mehr Quads. Sichtbare und Performance-Auswirkung, gehört gemessen. |
| §D.2 (Rest) Dead-Code-Warnungen | ~100 `never used`-Warnungen. Massenhaft löschen würde vermutlich Vorrat für laufende Arbeit entfernen, massenhaft `#[allow]` würde künftige echte Funde verstecken. Braucht eine Entscheidung pro Posten. |

### Testlage

`cargo test`: 116 passed, 1 failed. Der Fehlschlag ist `world::config::tests::sea_level_comes_from_world_config_for_lakes_and_lava_constraints` und **vorbestehend** — gegen `8f7634d` mit identischer Meldung gegengeprüft (`water_y=84, sea_level=80`). Er ist unabhängig von allen hier beschriebenen Änderungen und noch offen.

---

## ANHANG A — Rohdaten aus den Stall-Reports

Summary-Zeilen aller 10 Reports, auf die relevanten Felder reduziert.

| Log | frame (ms) | tops | chunkgen pure_ms / applied | mesh wait avg/max | mesh up_q | lighting backlog | view | inflight (mesh/gen) |
|-----|-----------|------|---------------------------|-------------------|-----------|------------------|------|---------------------|
| `run_1773071920_600` | 253.9 | lighting=237.5 | 0.0 / 0 | 0.27/0.61 | 0→0 | **218** | 4 | 13 / 42 |
| `run_1773082954_380` | 254.3 | chunkgen=21.5 | 21.5 / 9 | 0.00/0.00 | 0→24 | 0 | 12 | 148 / 124 |
| `run_1773085075_183` | **9697.5** | chunkgen=101.0 | 100.9 / 1 | — | 0→0 | 0 | 32 | 54 / 120 |
| `run_1773085692_815` | 357.0 | mesh=15.0 | 0.0 / 0 | 34.49/**352.95** | 2→16 | **194** | 32 | 88 / 60 |
| `run_1773086032_185` | 261.1 | chunkgen=17.2 | 17.2 / 5 | 0.00/0.00 | 0→49 | 0 | 32 | 142 / 123 |
| `run_1773087313_576` | 309.8 | chunkgen=21.0 | 20.9 / 3 | 0.00/0.00 | 17→56 | 0 | 32 | 124 / 125 |
| `run_1773254795_598` | 241.9 | chunkgen=39.9 | 39.8 / 0 | 0.00/0.00 | 0→22 | 0 | 32 | 37 / 128 |
| `run_1779800309_641` | 241.5 | chunkgen=201.5 | **201.4** / 17 | 0.00/0.00 | 0→8 | **176** | 32 | 40 / 84 |
| `run_1781101083_106` | 356.1 | chunkgen=313.0 | **312.9** / **3** | 0.00/0.00 | 39→62 | **190** | 32 | 68 / 86 |
| `run_1787146900_888` | 288.4 | mesh=17.6 | 0.0 / 0 | **181.02**/**413.20** | 46→47 | **194** | 12 | 101 / 60 |

**Bemerkenswert:**

- `lod=0, lod_dc=0, lod_mem_mb=0.00` in **allen** 10 Reports, trotz `far_lod=true`.
- `light_rx=0` in **allen** 10 Reports — der `LightingWorkerPool` bekommt nie Ergebnisse zurück, weil er nie Jobs bekommt.
- `sched[pend=8566,heap=15575]` im 9.7-s-Frame: der Priority-Heap ist um Faktor ~2 gegenüber dem Pending-Set aufgebläht.
- `sidefx_q=876=>831` (`run_1773085075_183`): Block-Edit-Side-Effects stauen sich ebenfalls auf.

### Trace-Dateigrößen

| Datei | Größe | Datum |
|-------|-------|-------|
| `run_1787146900_888.trace.json` | **558 MB** | Aug 19 16:02 |
| `run_1773082954_380.trace.json` | 134 MB | Mär 9 20:05 |
| `run_1779800309_641.trace.json` | 113 MB | Mai 26 15:00 |
| `run_1781101083_106.trace.json` | 113 MB | Jun 10 16:19 |
| `run_1773071920_600.trace.json` | 113 MB | Mär 9 17:00 |
| `run_1773086032_185.trace.json` | 98 MB | Mär 9 20:55 |
| `run_1773085692_815.trace.json` | 95 MB | Mär 9 20:49 |
| `run_1773254795_598.trace.json` | 69 MB | Mär 11 19:47 |
| `run_1773085075_183.trace.json` | 40 MB | Mär 9 20:40 |
| `run_1773083368_598.trace.json` | 37 MB | Mär 9 20:11 |

---

## ANHANG B — Was explizit geprüft und ausgeschlossen wurde

Diese Dinge sind **nicht** das Problem — hier muss niemand mehr suchen.

### Geometrie / Meshing

| Geprüft | Ergebnis | Beleg |
|---------|----------|-------|
| Greedy-Mask-Staleness | sauber — `mask.clear(); mask.resize(256, None)` pro Invocation, `n += 1` beschreibt alle 256 Zellen pro Slice neu | `greedy.rs:1024-1026`, `:1122`, `:1610` |
| Merge-Bounds / Row-Wraparound | korrekt — `while i + w < CHUNK_SIZE` / `while j + h < CHUNK_SIZE` | `greedy.rs:1139-1153` |
| Winding vs. Backface-Culling | korrekt — für alle drei Achsen ergibt `p0→p1→p2` ein `du × dv` gleich der positiven Face-Normale, passend zu `front_face: Ccw`; der Negativ-Zweig invertiert richtig | `greedy.rs:1206-1210`, `state.rs:7795` |
| Index-Base / geteilte Buffer | korrekt — jeder Layer besitzt eigene Buffer; `base = mesh.vertices.len()` wird pro Quad gegen den richtigen Layer genommen. Es gibt **keine** Shared-Buffer-Offset-Arithmetik | `greedy.rs:1197` |
| Negative / überlaufende UVs | ausgeschlossen — `face_uvs_for_quad` normalisiert durch Subtraktion des Per-Quad-Minimums, UVs sind immer ≥ 0; Max ist 16.0 → `pack_uv` clampt 65536→65535, Fehler = 1/4096 Texel | `greedy.rs:55-114`, `meshing/mod.rs:63-69` |
| Liquid-Corner-Cache-Staleness | sauber — **beide** Pools (`MeshingPool::liquid_pool` via `pool.reset()`, und der separate `LIQUID_POOL`-thread_local) rufen `clear_for_reuse()` vor jedem Job; Einträge sind mit Weltposition gekeyt | `pool.rs:196`, `liquids.rs:631-641` |
| Near-/Far-LOD-Overlap (Z-Fighting) | ausgeschlossen — beide nutzen denselben kreisförmigen Test `dx²+dz² ≤ r²` mit `lod_near_radius == near_mesh_radius_chunks()`, und LOD überspringt `dist2 <= r²`. Die Ringe sind **exakt disjunkt**. Zudem hat Far-LOD in allen Logs null Geometrie produziert | `state.rs:11333-11355`, `:18817-18828` |
| Vertex-Layout/Stride-Mismatch | korrekt — `WorldVertex::layout()` deckt sich Feld für Feld mit der WGSL-`VsIn`-Deklaration: Offsets 0/12/16/20, Formate `Float32x3`/`Uint16x2`/`Uint32`/`Uint8x4` ↔ `vec3<f32>`/`vec2<u32>`/`u32`/`vec4<u32>` | `mesh_gpu.rs:8-39`, `shaders.rs:27-32` |
| Mipmap-/`fract()`-Seams | nicht anwendbar — `mip_level_count: 1`, alles `Nearest`, `AddressMode::Repeat`. Das `fract()` im Animated-Texture-Zweig kann ohne Mips keine LOD-Artefakte erzeugen | `texture.rs:167-176`, `shaders.rs:66-68` |
| Padded-Index-Overflow in AO/Light | in Bounds — der 1-Zellen-Halo (18³) reicht exakt: Center-Block padded 1..16, `outer` → 0..17, `side1`/`side2`/`corner` bleiben ≤ 17 | `greedy.rs:645-694`, `neighborhood.rs:191-200` |
| AO treibt Face auf Schwarz | ausgeschlossen — `validated_ao_factor` clampt auf `MIN_AO = 0.5`; `face_shade` min. 0.5 → `shade` ≥ 0.25 | `greedy.rs:481-497`, `:353-360` |

### Threading / I/O

| Geprüft | Ergebnis |
|---------|----------|
| Blockierende GPU-Waits | keine — kein `device.poll(Maintain::Wait)`, kein sync. Buffer-Mapping im Render-Pfad |
| Geteilte Mutexes Main↔Worker | keine, **außer** dem Trace-Runtime. Die Pipeline ist echtes Message-Passing |
| 16³-Block-Array-Kopien zwischen Stages | keine — `snapshot_neighborhood` nutzt durchgehend `Arc::clone` |
| Spin-Loops / Busy-Waiting in Workern | keine — alle Worker nutzen `crossbeam_channel::select!` mit Shutdown-Kanal, blockierend |
| Verpasste Wakeups | keine — `select!` über `job_rx` + `shutdown_rx`, sauberes `Drop` mit Join |
| Worldgen-Determinismus | sauber — **keine** `HashMap`/`HashSet`-Iteration in `src/worldgen/` |
| `unsafe` | **null** Blöcke im gesamten Crate |
| Blockierendes I/O im Main-Loop | nur `settings.rs:301` (`fs::write`, atomarer Settings-Save, selten) und die Trace-/CSV-Pfade aus §1.1 |

---

## ANHANG C — Relevante Konstanten

Alle in `src/render/state.rs`, Zeilen 1083–1218.

### Chunkgen-Apply

```
CHUNKGEN_APPLY_RESULTS_PER_FRAME_BASE        = 4
CHUNKGEN_APPLY_RESULTS_PER_FRAME_BURST       = 8
CHUNKGEN_APPLY_RESULTS_PER_FRAME_MIN         = 1
CHUNKGEN_APPLY_CPU_TARGET_MS                 = 8.0
CHUNKGEN_APPLY_CPU_HARD_STOP_MS              = 12.0
CHUNKGEN_APPLY_FOLLOWUP_BUDGET_MS            = 1.5
CHUNKGEN_APPLY_FOLLOWUP_STEPS_PER_FRAME      = 8
CHUNKGEN_REQUESTS_PER_FRAME                  = 48
MAX_INFLIGHT_CHUNKGEN                        = 128
CHUNKGEN_JOB_QUEUE_CAPACITY                  = 256   (bounded)
CHUNKGEN_RESULT_QUEUE_CAPACITY               = 256   (bounded)
```

### Mesh

```
MESH_RESULTS_DRAIN_PER_FRAME                 = 64
MESH_UPLOAD_NEAR_PRIORITY_RADIUS_CHUNKS      = 2
MESH_UPLOAD_IMMEDIATE_RADIUS_CHUNKS          = 1
MESH_UPLOAD_LOW_SPEED_BUDGET_MS              = 5.5
MESH_UPLOAD_WARMUP_BUDGET_MS                 = 10.0
MESH_FRUSTUM_DEFER_FRAMES                    = 24
MESH_MISSING_SAFETY_SCAN_PERIOD_FRAMES       = 30
                                             (mesh_jobs / mesh_results: unbounded)
```

### Lighting — der Engpass

```
LIGHTING_RESULTS_PER_FRAME                        = 2
LIGHTING_CHANGED_CHUNK_DIRTY_APPLY_PER_FRAME      = 64
LIGHTING_CHANGED_CHUNK_DIRTY_APPLY_BUDGET_MS      = 0.5
LIGHTING_REBUILD_DEBOUNCE_MS                      = 75
STREAM_CHUNK_LIGHTING_MAX_PRIMARY_CHUNKS_PER_JOB  = 3      ← §2.2
STREAM_CHUNK_LIGHTING_MAX_REGION_CHUNKS_PER_JOB   = 14
STREAM_CHUNK_LIGHTING_MAX_VOXELS_PER_STEP         = 48_000
STREAM_CHUNK_LIGHTING_STEP_BUDGET_MS              = 1.5
STREAM_CHUNK_LIGHTING_FRAME_BUDGET_MS             = 3.0    ← §2.2
STREAM_CHUNK_LIGHTING_MAX_STEPS_PER_FRAME         = 4      ← real: 2
PENDING_LIGHTING_SKY_HEURISTIC_MIN_Y              = 64     ← §2.1
STARTUP_FINAL_LIGHTING_TRIGGER_PERCENT            = 85     ← §2.4
```

### Startup

```
STARTUP_STREAMING_BURST_FRAME_BUDGET_MS      = 20.0
STARTUP_CHUNKGEN_APPLY_BUDGET_MS             = 6.0
STARTUP_CHUNKGEN_APPLY_RESULTS_PER_PASS      = 48
STARTUP_MESH_DISPATCH_BUDGET_MS              = 8.0
STARTUP_MESH_UPLOAD_BUDGET_MS                = 10.0
```

### Stream / Evict

```
STREAM_STAGE_LOOP_SAFETY_CAP                 = 4096
STREAM_PRUNE_MESHES_PER_FRAME                = 128
STREAM_PRUNE_MESH_BUDGET_MS                  = 1.0
STREAM_UNLOAD_SCAN_COORDS_PER_FRAME          = 256
STREAM_EVICT_CHUNKS_PER_FRAME                = 24
STREAM_EVICT_BUDGET_MS                       = 1.5
STREAM_KEEP_RADIUS_MARGIN_CHUNKS             = 2
VERTICAL_VIEW_RADIUS_CHUNKS                  = 4
```

---

## Empfohlene Reihenfolge der Umsetzung

1. ~~**§1.1 Tracing gaten**~~ — ✅ erledigt.
2. ~~**§1.2 `refresh_worldgen_warmup_tracking` inkrementell**~~ — ✅ erledigt.
3. ~~**§2.3 Parking auf `VisualReady` lösen**~~ — ✅ erledigt.
4. ~~**§2.1 Provisorisches Skylight aus Heightmap**~~ — ✅ erledigt (Worldgen-Schätzung statt Heightmap).
5. ~~**§5 GPU-Buffer pro Frame**~~ — ✅ erledigt für Overlays/Partikel/Precipitation.
6. ~~**§1.5 `eprintln!` aus den Hot Paths**~~ — ✅ erledigt.
7. ~~**§6.1 Messung nachholen**~~ — ✅ erledigt, siehe [Messlauf](#messlauf-nach-1112--logsrun_1787160799_221).
8. ~~**§D.3 Unload-Lighting budgetieren**~~ + ~~**§D.1**~~ + ~~**§D.4**~~ — ✅ erledigt.
9. **Messlauf wiederholen** — nach §D.3/§5/§1.5 erneut messen, bevor weiter getunt wird.
10. **§D.3 (Rest) Removal-Lighting stufen** — `lighting_on_chunks_removed` ist One-Shot; erst über die Step-API ist ein einzelner Batch wirklich gedeckelt.
11. **§2.2 Lighting auf den Worker-Pool** — strukturell; bisher nur die Budgets angehoben, der Pool ist weiter idle.
12. **§1.4 Mesh-Job-Priority-Inversion** — größerer Umbau (bounded Channel + umsortierbare Pending-Queue), vorher messen.
13. **§5 (Rest) Chunk-Mesh-Suballocation** — bis zu 6 Buffer pro Chunk, `next_power_of_two` verschwendet bis zu 2× VRAM.
14. **§2.5 `blocks_skylight` aus `OcclusionShape` ableiten** — Contract-Bereinigung, braucht visuelle Prüfung.
15. **§D.2 (Rest) Warning-Schulden** — damit `-D warnings` wieder benutzbar wird.
16. **§1.3 Spin-Loop** — niedrige Priorität, läuft nur einmal im Ladebildschirm.
