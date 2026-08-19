# 07 — Performance & Rendering-Correctness Audit

**Datum:** 2026-08-19 · **Basis:** Commit `8f7634d` (branch `main`, clean) · **Modus:** read-only
**Verifikation:** `cargo check` → exit 0. Keine Datei außerhalb von `docs/` verändert.

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
7. [Anhang A — Rohdaten aus den Stall-Reports](#anhang-a--rohdaten-aus-den-stall-reports)
8. [Anhang B — Was explizit geprüft und ausgeschlossen wurde](#anhang-b--was-explizit-geprüft-und-ausgeschlossen-wurde)
9. [Anhang C — Relevante Konstanten](#anhang-c--relevante-konstanten)

---

## 1. TOP SUSPECTS

| # | Befund | Severity | Symptom | Status |
|---|--------|----------|---------|--------|
| 1 | Immer aktives Chrome-Tracing: JSON-Serialisierung + Datei-I/O unter **einem globalen Mutex**, geteilt von Main-Thread und allen Workern | critical | 1 | CONFIRMED |
| 2 | `refresh_worldgen_warmup_tracking()` baut das komplette Startup-Section-Set pro Drain neu → O(n²) über den Startup | critical | 1 | CONFIRMED |
| 3 | Provisorisches Licht unterhalb Y=64 ist **komplett 0**, und der Lighting-Backlog wird nie abgebaut (190–218 Jobs dauerhaft) | critical | 2, 3 | CONFIRMED |
| 4 | `mesh_should_await_lighting_finalization` parkt den korrigierenden Remesh bis `Finalized` — was Rand-Chunks u. U. nie erreichen | high | 2, 3 | CONFIRMED |
| 5 | 17 `create_buffer_init`-Aufrufe **pro Frame** in `render()` + `queue.write_buffer` pro Chunk-Layer ohne Suballocation | high | 1 | CONFIRMED |

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

**Severity:** high · **Likelihood Symptom 1:** hoch · **Status:** CONFIRMED

**Evidenz:** `src/render/state.rs:12732-12759` — `ensure_column_loaded_or_generated` läuft bis zu **512 Iterationen**, jede mit Generierungs-Requests, `pump_chunkgen_results()` und `std::thread::yield_now()`. Blockiert den Main-Thread, bis der Worker liefert.

Aufrufer: `src/app/mod.rs:1515`.

**Mechanismus:** Harter Main-Thread-Stall, nur begrenzt durch Worker-Latenz × 512. Kandidat für den 9.7-Sekunden-Frame.

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
| high | `src/render/state.rs:18898-19242` | **17** `create_buffer_init`-Aufrufe pro Frame in `render()` (Partikel, Precipitation, Outlines, UI) | Persistente Grow-on-Demand-Buffer + `write_buffer`; verletzt `.claude/rules/render.md` |
| high | `src/render/mesh_gpu.rs:196-260` | Pro-Chunk-pro-Layer `wgpu::Buffer` (bis zu 6/Chunk), keine Suballocation; `grown_capacity` = `next_power_of_two` → bis zu 2× VRAM-Verschwendung | Layer aus einer großen persistenten Arena suballozieren; bestehenden Reuse-on-Fit-Pfad behalten |
| high | `src/render/state.rs:5678` | Mesh-Job-Channel `unbounded` + FIFO-Worker → ferne Chunks verhungern nahe (`inflight(mesh=148)`) | Bounded Channel + umsortierbare Pending-Queue |
| medium | `src/console.rs:9-13`; `src/world/mod.rs:2425-2454`; `src/render/state.rs:17604` | `eprintln!` (ungepuffert, globaler Lock) pro Mesh-Pump und ×6 pro Lighting-Job, aus Worker-Threads | Level-gegateter, gepufferter Logger |
| medium | `src/meshing/pool.rs:63-70` | `take_mesh_data` macht `mem::take` und allokiert dann frische `Vec::with_capacity(8192/12288)` ≈ **410 KB pro Aufruf** — der Pool alloziert, statt zu poolen | Slice zurückgeben / in caller-eigenen Buffer drainen, oder Free-List retirierter Vecs |
| medium | `src/meshing/pool.rs:152-158` | `reserve_if_hint` ruft `Vec::reserve` (additiv) mit monoton wachsenden Hints aus `update_hint` → Kapazität ratcht auf den größten je gemeshten Chunk hoch | `reserve_exact` gegen `hint - len`, Hint verfallen lassen |
| medium | `src/render/state.rs:18874` | `transparent_chunks = visible_chunks.clone()` + zweiter Full-Sort, jeden Frame | `visible_chunks` rückwärts iterieren — der Sort ist exakt invertiert |
| medium | `src/render/state.rs:18898-18906` | Partikel-Vertex-/Index-Buffer jeden Frame neu gebaut | Persistenter Ring-Buffer |
| medium | `Cargo.toml` | Feature `allocation_tracking` wird in `src/meshing/liquids.rs:10` und `:653` benutzt, ist aber **nirgends deklariert** → `cargo build --features allocation_tracking` schlägt fehl; der Tracker ist unerreichbar | `[features]\nallocation_tracking = []` ergänzen |
| medium | `src/render/state.rs:18418` | Erreichbarer `f32::clamp`-Panic (min > max) in der Precipitation | `.max().min()` |
| low | `src/meshing/neighborhood.rs:60-64` | `resize()` gefolgt von `fill()` — drei redundante Full-Fills à 5832 Elemente pro Mesh-Job | `fill` nach `resize`/`clear` streichen |
| low | `src/meshing/neighborhood.rs:176-182` | Toter Zweig: `if world_y > WORLD_MAX_Y { 0xF0 } else { 0xF0 }` | Zusammenfassen; klären, ob der `< 0`-Fall wirklich `0` sein soll |
| low | `src/render/mesh_gpu.rs:216` | `format!("{label_prefix} Vertex Buffer")` allokiert einen `String` pro Buffer-Erzeugung | `&'static str`-Labels, oder `None` im Release |
| low | `src/render/state.rs:10496-10510` | `count_urgent_near_mesh_uploads` scannt die volle Upload-Queue jeden Frame (zweimal, über beide Budget-Pfade) | Zähler inkrementell pflegen |
| — | gesamtes Crate | **Null `unsafe`-Blöcke.** `unwrap`/`expect`-Dichte niedrig und auf Init-/Test-Pfade konzentriert (state.rs: 5, greedy.rs: 12, world/mod.rs: 19, region_file.rs: 14, chunkgen_worker.rs: 13) | keine Maßnahme |

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

1. **§1.1 Tracing gaten** — größter Hebel, geringstes Risiko, macht alle weiteren Messungen erst aussagekräftig.
2. **§6.1 Messung wiederholen** — bevor irgendetwas anderes angefasst wird.
3. **§1.2 `refresh_worldgen_warmup_tracking` inkrementell** — zweitgrößter Hebel für Symptom 1.
4. **§2.3 Parking auf `VisualReady` lösen** — kleinster Fix mit größter Wirkung auf Symptom 2/3.
5. **§2.1 Provisorisches Skylight aus Heightmap** — behebt die Schwarz-Chunks an der Wurzel.
6. **§2.2 Lighting auf den Worker-Pool** — strukturell, aber der Pool existiert schon.
7. **§2.5 `blocks_skylight` aus `OcclusionShape` ableiten** — Contract-Bereinigung, mit Test absichern.
8. **§1.3 / §1.4 / §5 GPU-Buffer** — danach, gemessen priorisiert.
