use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, TrySendError};
use serde_json::json;

const RING_RETENTION: Duration = Duration::from_secs(10);
const RING_MAX_EVENTS: usize = 8192;

/// Bounded hand-off to the writer thread. Producers never block: once this is
/// full the event is dropped and counted, so a slow disk can throttle the trace
/// but never the frame.
const WRITER_QUEUE_CAPACITY: usize = 16384;

static TRACE_RUNTIME: OnceLock<TraceRuntime> = OnceLock::new();

/// How much instrumentation is live this run.
///
/// The JSON event stream is the expensive tier: it used to run unconditionally
/// and produced >500 MB per session, serialized under a single global mutex that
/// every worker thread contended on. It is opt-in now. The in-memory ring stays
/// on by default because stall reports are built from it and it is bounded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TraceLevel {
    /// No instrumentation at all, no `logs/` directory.
    Off,
    /// Ring buffer + stall reports + GPU CSV. No JSON event stream.
    Ring,
    /// Everything, with JSON serialization on a dedicated writer thread.
    Full,
}

impl TraceLevel {
    /// The cargo feature only moves the default; the env var always wins so a
    /// release build can still be asked for a full trace when diagnosing.
    fn compiled_default() -> Self {
        if cfg!(feature = "perf_trace") {
            Self::Full
        } else {
            Self::Ring
        }
    }

    fn parse(raw: Option<&str>, default: Self) -> Self {
        let Some(raw) = raw else {
            return default;
        };
        match raw.trim().to_ascii_lowercase().as_str() {
            "0" | "off" | "none" | "false" => Self::Off,
            "1" | "ring" | "stall" | "true" => Self::Ring,
            "2" | "full" | "json" | "all" => Self::Full,
            _ => default,
        }
    }

    fn from_env() -> Self {
        Self::parse(
            std::env::var("MC_KLON_TRACE").ok().as_deref(),
            Self::compiled_default(),
        )
    }
}

#[derive(Clone, Debug)]
struct RingEvent {
    t_ms: f64,
    thread: String,
    tag: String,
    duration_ms: f64,
    counts: String,
}

/// One event on its way to the writer thread. Owns its strings so the producer
/// never has to wait for serialization.
struct WriterEvent {
    tag: String,
    counts: String,
    thread: String,
    tid: u64,
    ts_us: u64,
    /// `None` marks an instant event, `Some` a complete (duration) event.
    dur_us: Option<u64>,
}

enum WriterMsg {
    Event(WriterEvent),
    Shutdown,
}

struct TraceWriter {
    file: BufWriter<File>,
    first_event: bool,
}

impl TraceWriter {
    fn new(path: &PathBuf) -> io::Result<Self> {
        let file = File::create(path)?;
        let mut file = BufWriter::with_capacity(1 << 20, file);
        file.write_all(b"{\"traceEvents\":[\n")?;
        Ok(Self {
            file,
            first_event: true,
        })
    }

    fn write_event(&mut self, event: serde_json::Value) -> io::Result<()> {
        if !self.first_event {
            self.file.write_all(b",\n")?;
        }
        self.first_event = false;
        serde_json::to_writer(&mut self.file, &event)
            .map_err(|e| io::Error::other(format!("trace json write failed: {e}")))?;
        Ok(())
    }

    fn finish(&mut self) -> io::Result<()> {
        self.file.write_all(b"\n]}\n")?;
        self.file.flush()
    }
}

fn run_writer_thread(mut writer: TraceWriter, rx: Receiver<WriterMsg>, pid: u32) {
    while let Ok(msg) = rx.recv() {
        let event = match msg {
            WriterMsg::Shutdown => break,
            WriterMsg::Event(event) => event,
        };
        let value = match event.dur_us {
            Some(dur_us) => json!({
                "name": event.tag,
                "cat": "cpu",
                "ph": "X",
                "ts": event.ts_us.saturating_sub(dur_us),
                "dur": dur_us,
                "pid": pid,
                "tid": event.tid,
                "args": { "counts": event.counts, "thread": event.thread },
            }),
            None => json!({
                "name": event.tag,
                "cat": "instant",
                "ph": "i",
                "s": "t",
                "ts": event.ts_us,
                "pid": pid,
                "tid": event.tid,
                "args": { "counts": event.counts, "thread": event.thread },
            }),
        };
        if writer.write_event(value).is_err() {
            break;
        }
    }
    let _ = writer.finish();
}

struct TraceRuntime {
    level: TraceLevel,
    run_label: String,
    trace_path: PathBuf,
    stall_path: PathBuf,
    gpu_csv_path: PathBuf,
    started_at: Instant,
    /// `None` unless the JSON stream is enabled. A plain field (not a mutex) so
    /// the hot path never takes a lock to reach it.
    event_tx: Option<Sender<WriterMsg>>,
    writer_join: Mutex<Option<thread::JoinHandle<()>>>,
    dropped_events: AtomicU64,
    ring: Mutex<VecDeque<RingEvent>>,
    /// Held open for the whole run; the old code re-opened (and stat'd) this file
    /// once per frame on the render thread.
    gpu_csv: Mutex<Option<BufWriter<File>>>,
}

impl TraceRuntime {
    fn make_paths() -> io::Result<(String, PathBuf, PathBuf, PathBuf)> {
        fs::create_dir_all("logs")?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0));
        let secs = now.as_secs();
        let ms = now.subsec_millis();
        let run_label = format!("run_{secs}_{ms:03}");
        Ok((
            run_label.clone(),
            PathBuf::from("logs").join(format!("{run_label}.trace.json")),
            PathBuf::from("logs").join(format!("{run_label}.stall.txt")),
            PathBuf::from("logs").join(format!("{run_label}.gpu.csv")),
        ))
    }

    fn now_us(&self) -> u64 {
        (self.started_at.elapsed().as_secs_f64() * 1_000_000.0) as u64
    }

    fn push_ring(&self, event: RingEvent) {
        let Ok(mut ring) = self.ring.lock() else {
            return;
        };
        ring.push_back(event);
        let cutoff_ms = self.started_at.elapsed().as_secs_f64() * 1000.0
            - RING_RETENTION.as_secs_f64() * 1000.0;
        while ring
            .front()
            .is_some_and(|e| e.t_ms < cutoff_ms || ring.len() > RING_MAX_EVENTS)
        {
            ring.pop_front();
        }
    }
}

thread_local! {
    /// `thread::current().name()` plus an FNV hash used to be recomputed for every
    /// single event. It cannot change for the life of a thread, so cache it.
    static THREAD_IDENTITY: RefCell<Option<(String, u64)>> = const { RefCell::new(None) };
}

fn thread_label_and_tid() -> (String, u64) {
    THREAD_IDENTITY.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell.is_none() {
            let label = std::thread::current()
                .name()
                .map(str::to_string)
                .unwrap_or_else(|| format!("tid:{:?}", std::thread::current().id()));
            let mut h = 1469598103934665603u64;
            for b in label.as_bytes() {
                h ^= *b as u64;
                h = h.wrapping_mul(1099511628211);
            }
            *cell = Some((label, h));
        }
        let (label, tid) = cell.as_ref().expect("thread identity just initialized");
        (label.clone(), *tid)
    })
}

pub(crate) struct TraceScope {
    tag: String,
    started_at: Instant,
    counts: Option<String>,
}

impl TraceScope {
    /// Builds the counts string only if the event will actually be recorded.
    ///
    /// The eager `with_counts(format!(…))` form evaluated its argument before the
    /// call, so several per-frame scopes allocated a `String` on every frame even
    /// with tracing off — gating the runtime alone does not help there.
    pub(crate) fn with_counts(mut self, counts: impl FnOnce() -> String) -> Self {
        if TRACE_RUNTIME.get().is_some() {
            self.counts = Some(counts());
        }
        self
    }
}

impl Drop for TraceScope {
    fn drop(&mut self) {
        record_duration(
            &self.tag,
            self.started_at.elapsed(),
            self.counts.as_deref().unwrap_or_default(),
        );
    }
}

pub(crate) fn trace_init() -> io::Result<()> {
    if TRACE_RUNTIME.get().is_some() {
        return Ok(());
    }
    let level = TraceLevel::from_env();
    if level == TraceLevel::Off {
        return Ok(());
    }

    let (run_label, trace_path, stall_path, gpu_csv_path) = TraceRuntime::make_paths()?;
    let pid = std::process::id();

    let (event_tx, writer_join) = if level == TraceLevel::Full {
        let writer = TraceWriter::new(&trace_path)?;
        let (tx, rx) = crossbeam_channel::bounded::<WriterMsg>(WRITER_QUEUE_CAPACITY);
        let handle = thread::Builder::new()
            .name("perf-trace-writer".to_string())
            .spawn(move || run_writer_thread(writer, rx, pid))?;
        (Some(tx), Some(handle))
    } else {
        (None, None)
    };

    let rt = TraceRuntime {
        level,
        run_label,
        trace_path,
        stall_path,
        gpu_csv_path,
        started_at: Instant::now(),
        event_tx,
        writer_join: Mutex::new(writer_join),
        dropped_events: AtomicU64::new(0),
        ring: Mutex::new(VecDeque::with_capacity(RING_MAX_EVENTS.min(2048))),
        gpu_csv: Mutex::new(None),
    };
    let _ = TRACE_RUNTIME.set(rt);

    instant_event(
        "trace.init",
        &format!(
            "level={level:?} trace_file={} json_stream={}",
            trace_file_path().display(),
            level == TraceLevel::Full
        ),
    );
    instant_event(
        "gpu.wait.audit",
        "No explicit device.poll(Maintain::Wait) / blocking GPU waits found in current instrumented codepath",
    );
    Ok(())
}

pub(crate) fn trace_shutdown() {
    let Some(rt) = TRACE_RUNTIME.get() else {
        return;
    };
    let dropped = rt.dropped_events.load(Ordering::Relaxed);
    if dropped > 0 {
        instant_event("trace.shutdown", &format!("dropped_events={dropped}"));
        crate::console_error!(
            "[perf][trace] writer queue overflowed: {dropped} events dropped (trace is incomplete)"
        );
    } else {
        instant_event("trace.shutdown", "");
    }

    if let Some(tx) = rt.event_tx.as_ref() {
        let _ = tx.send(WriterMsg::Shutdown);
    }
    if let Ok(mut join) = rt.writer_join.lock()
        && let Some(handle) = join.take()
    {
        let _ = handle.join();
    }
    if let Ok(mut csv) = rt.gpu_csv.lock()
        && let Some(file) = csv.as_mut()
    {
        let _ = file.flush();
    }
}

pub(crate) fn trace_file_path() -> PathBuf {
    TRACE_RUNTIME
        .get()
        .map(|rt| rt.trace_path.clone())
        .unwrap_or_else(|| PathBuf::from("logs/run_unknown.trace.json"))
}

pub(crate) fn stall_file_path() -> PathBuf {
    TRACE_RUNTIME
        .get()
        .map(|rt| rt.stall_path.clone())
        .unwrap_or_else(|| PathBuf::from("logs/run_unknown.stall.txt"))
}

pub(crate) fn gpu_csv_path() -> PathBuf {
    TRACE_RUNTIME
        .get()
        .map(|rt| rt.gpu_csv_path.clone())
        .unwrap_or_else(|| PathBuf::from("logs/run_unknown.gpu.csv"))
}

pub(crate) fn scope(tag: impl Into<String>) -> TraceScope {
    TraceScope {
        tag: tag.into(),
        started_at: Instant::now(),
        counts: None,
    }
}

/// Single emit path for both event kinds. Does the ring push inline (bounded,
/// no I/O) and hands JSON serialization to the writer thread.
fn emit(rt: &TraceRuntime, tag: &str, counts: &str, ts_us: u64, dur_us: Option<u64>) {
    let (thread, tid) = thread_label_and_tid();

    let Some(tx) = rt.event_tx.as_ref() else {
        // Ring-only: hand the strings straight over, no clones.
        rt.push_ring(RingEvent {
            t_ms: ts_us as f64 / 1000.0,
            thread,
            tag: tag.to_string(),
            duration_ms: dur_us.unwrap_or(0) as f64 / 1000.0,
            counts: counts.to_string(),
        });
        return;
    };

    rt.push_ring(RingEvent {
        t_ms: ts_us as f64 / 1000.0,
        thread: thread.clone(),
        tag: tag.to_string(),
        duration_ms: dur_us.unwrap_or(0) as f64 / 1000.0,
        counts: counts.to_string(),
    });

    let event = WriterEvent {
        tag: tag.to_string(),
        counts: counts.to_string(),
        thread,
        tid,
        ts_us,
        dur_us,
    };
    match tx.try_send(WriterMsg::Event(event)) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            rt.dropped_events.fetch_add(1, Ordering::Relaxed);
        }
        Err(TrySendError::Disconnected(_)) => {}
    }
}

pub(crate) fn instant_event(tag: &str, counts: &str) {
    let Some(rt) = TRACE_RUNTIME.get() else {
        return;
    };
    let ts_us = rt.now_us();
    emit(rt, tag, counts, ts_us, None);
}

pub(crate) fn record_duration(tag: &str, dur: Duration, counts: &str) {
    let Some(rt) = TRACE_RUNTIME.get() else {
        return;
    };
    let ts_end_us = rt.now_us();
    let dur_us = (dur.as_secs_f64() * 1_000_000.0) as u64;
    emit(rt, tag, counts, ts_end_us, Some(dur_us));
}

pub(crate) fn record_duration_ms(tag: &str, dur_ms: f32, counts: &str) {
    record_duration(
        tag,
        Duration::from_secs_f64((dur_ms.max(0.0) as f64) / 1000.0),
        counts,
    );
}

pub(crate) fn write_stall_report(stall_summary: &str) -> io::Result<()> {
    let Some(rt) = TRACE_RUNTIME.get() else {
        return Ok(());
    };
    let ring_snapshot = {
        let ring = rt.ring.lock().expect("trace ring poisoned");
        ring.iter().cloned().collect::<Vec<_>>()
    };
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&rt.stall_path)?;
    writeln!(file, "==== STALL REPORT {} ====", rt.run_label)?;
    writeln!(file, "summary: {stall_summary}")?;
    if rt.level == TraceLevel::Full {
        writeln!(file, "trace_file: {}", rt.trace_path.display())?;
    } else {
        writeln!(
            file,
            "trace_file: <disabled> (set MC_KLON_TRACE=full for the JSON event stream)"
        )?;
    }
    writeln!(file, "gpu_csv: {}", rt.gpu_csv_path.display())?;
    writeln!(
        file,
        "-- ring-buffer (last {:.1}s) --",
        RING_RETENTION.as_secs_f32()
    )?;
    for e in ring_snapshot {
        writeln!(
            file,
            "t={:9.3}ms thread={} tag={} dur={:8.3}ms {}",
            e.t_ms, e.thread, e.tag, e.duration_ms, e.counts
        )?;
    }
    writeln!(file)?;
    instant_event("stall.report_written", stall_summary);
    Ok(())
}

pub(crate) fn append_gpu_csv_line(header: &str, line: &str) -> io::Result<()> {
    let Some(rt) = TRACE_RUNTIME.get() else {
        return Ok(());
    };
    let mut slot = match rt.gpu_csv.lock() {
        Ok(slot) => slot,
        Err(_) => return Ok(()),
    };
    if slot.is_none() {
        let existed = rt.gpu_csv_path.exists();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&rt.gpu_csv_path)?;
        let mut file = BufWriter::new(file);
        if !existed {
            writeln!(file, "{header}")?;
        }
        *slot = Some(file);
    }
    let file = slot.as_mut().expect("gpu csv writer just initialized");
    writeln!(file, "{line}")
}

pub(crate) fn note_gpu_wait(reason: &str) {
    instant_event("gpu.wait", reason);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_env_keeps_the_compiled_default() {
        assert_eq!(
            TraceLevel::parse(None, TraceLevel::Ring),
            TraceLevel::Ring,
            "an unset MC_KLON_TRACE must not turn the JSON stream on"
        );
        assert_eq!(TraceLevel::parse(None, TraceLevel::Full), TraceLevel::Full);
    }

    #[test]
    fn json_stream_stays_opt_in_by_default() {
        // The regression this guards: the JSON event stream used to be
        // unconditional and wrote >500 MB per session from the hot path.
        assert_ne!(TraceLevel::compiled_default(), TraceLevel::Full);
    }

    #[test]
    fn env_var_overrides_in_both_directions() {
        for raw in ["0", "off", "none", "false", " OFF "] {
            assert_eq!(
                TraceLevel::parse(Some(raw), TraceLevel::Full),
                TraceLevel::Off,
                "{raw:?} should disable tracing even when the build defaults to full"
            );
        }
        for raw in ["2", "full", "json", "all", " Full "] {
            assert_eq!(
                TraceLevel::parse(Some(raw), TraceLevel::Ring),
                TraceLevel::Full,
                "{raw:?} should enable the JSON stream on a default build"
            );
        }
        for raw in ["1", "ring", "stall", "true"] {
            assert_eq!(
                TraceLevel::parse(Some(raw), TraceLevel::Off),
                TraceLevel::Ring
            );
        }
    }

    #[test]
    fn unrecognized_value_falls_back_to_the_default() {
        assert_eq!(
            TraceLevel::parse(Some("verbose"), TraceLevel::Ring),
            TraceLevel::Ring
        );
        assert_eq!(
            TraceLevel::parse(Some(""), TraceLevel::Ring),
            TraceLevel::Ring
        );
    }
}
