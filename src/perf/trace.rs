use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;

const RING_RETENTION: Duration = Duration::from_secs(10);
const RING_MAX_EVENTS: usize = 8192;

static TRACE_RUNTIME: OnceLock<TraceRuntime> = OnceLock::new();

#[derive(Clone, Debug)]
struct RingEvent {
    t_ms: f64,
    thread: String,
    tag: String,
    duration_ms: f64,
    counts: String,
}

struct TraceWriter {
    file: BufWriter<File>,
    first_event: bool,
}

impl TraceWriter {
    fn new(path: &PathBuf) -> io::Result<Self> {
        let file = File::create(path)?;
        let mut file = BufWriter::new(file);
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

struct TraceRuntime {
    run_label: String,
    trace_path: PathBuf,
    stall_path: PathBuf,
    gpu_csv_path: PathBuf,
    started_at: Instant,
    writer: Mutex<TraceWriter>,
    ring: Mutex<VecDeque<RingEvent>>,
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

    fn pid(&self) -> u32 {
        std::process::id()
    }

    fn thread_label_and_tid() -> (String, u64) {
        let label = std::thread::current()
            .name()
            .map(str::to_string)
            .unwrap_or_else(|| format!("tid:{:?}", std::thread::current().id()));
        let mut h = 1469598103934665603u64;
        for b in label.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(1099511628211);
        }
        (label, h)
    }

    fn push_ring(&self, event: RingEvent) {
        let mut ring = self.ring.lock().expect("trace ring poisoned");
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

pub(crate) struct TraceScope {
    tag: String,
    started_at: Instant,
    counts: Option<String>,
}

impl TraceScope {
    pub(crate) fn with_counts(mut self, counts: impl Into<String>) -> Self {
        self.counts = Some(counts.into());
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
    let (run_label, trace_path, stall_path, gpu_csv_path) = TraceRuntime::make_paths()?;
    let writer = TraceWriter::new(&trace_path)?;
    let rt = TraceRuntime {
        run_label,
        trace_path,
        stall_path,
        gpu_csv_path,
        started_at: Instant::now(),
        writer: Mutex::new(writer),
        ring: Mutex::new(VecDeque::with_capacity(2048)),
    };
    let _ = TRACE_RUNTIME.set(rt);
    instant_event(
        "trace.init",
        &format!("trace_file={}", trace_file_path().display()),
    );
    instant_event(
        "gpu.wait.audit",
        "No explicit device.poll(Maintain::Wait) / blocking GPU waits found in current instrumented codepath",
    );
    Ok(())
}

pub(crate) fn trace_shutdown() {
    if let Some(rt) = TRACE_RUNTIME.get() {
        instant_event("trace.shutdown", "");
        if let Ok(mut writer) = rt.writer.lock() {
            let _ = writer.finish();
        }
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

pub(crate) fn instant_event(tag: &str, counts: &str) {
    let Some(rt) = TRACE_RUNTIME.get() else {
        return;
    };
    let (thread, tid) = TraceRuntime::thread_label_and_tid();
    let ts_us = rt.now_us();
    rt.push_ring(RingEvent {
        t_ms: ts_us as f64 / 1000.0,
        thread: thread.clone(),
        tag: tag.to_string(),
        duration_ms: 0.0,
        counts: counts.to_string(),
    });
    let event = json!({
        "name": tag,
        "cat": "instant",
        "ph": "i",
        "s": "t",
        "ts": ts_us,
        "pid": rt.pid(),
        "tid": tid,
        "args": {
            "counts": counts,
            "thread": thread,
        }
    });
    if let Ok(mut writer) = rt.writer.lock() {
        let _ = writer.write_event(event);
    }
}

pub(crate) fn record_duration(tag: &str, dur: Duration, counts: &str) {
    let Some(rt) = TRACE_RUNTIME.get() else {
        return;
    };
    let (thread, tid) = TraceRuntime::thread_label_and_tid();
    let ts_end_us = rt.now_us();
    let dur_us = (dur.as_secs_f64() * 1_000_000.0) as u64;
    let ts_start_us = ts_end_us.saturating_sub(dur_us);
    rt.push_ring(RingEvent {
        t_ms: ts_end_us as f64 / 1000.0,
        thread: thread.clone(),
        tag: tag.to_string(),
        duration_ms: dur.as_secs_f64() * 1000.0,
        counts: counts.to_string(),
    });
    let event = json!({
        "name": tag,
        "cat": "cpu",
        "ph": "X",
        "ts": ts_start_us,
        "dur": dur_us,
        "pid": rt.pid(),
        "tid": tid,
        "args": {
            "counts": counts,
            "thread": thread,
        }
    });
    if let Ok(mut writer) = rt.writer.lock() {
        let _ = writer.write_event(event);
    }
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
    writeln!(file, "trace_file: {}", rt.trace_path.display())?;
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
    let exists = rt.gpu_csv_path.exists();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&rt.gpu_csv_path)?;
    if !exists {
        writeln!(file, "{header}")?;
    }
    writeln!(file, "{line}")?;
    Ok(())
}

pub(crate) fn note_gpu_wait(reason: &str) {
    instant_event("gpu.wait", reason);
}
