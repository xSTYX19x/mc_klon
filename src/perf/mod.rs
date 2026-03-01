pub(crate) mod allocation_tracker;
pub(crate) mod trace;

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use winit::window::Window;

pub(crate) use allocation_tracker::{ALLOCATION_TRACKER, AllocationStats, AllocationTracker};

const EMA_ALPHA: f32 = 0.10;
const REPORT_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug)]
pub enum PerfCat {
    Movement,
    ChunkStream,
    Fluids,
    TexAnim,
    Uniforms,
    Particles,
    Highlight,
    Render,
}

impl PerfCat {
    pub const N: usize = 8;
    const ORDER: [PerfCat; PerfCat::N] = [
        PerfCat::Movement,
        PerfCat::ChunkStream,
        PerfCat::Fluids,
        PerfCat::TexAnim,
        PerfCat::Uniforms,
        PerfCat::Particles,
        PerfCat::Highlight,
        PerfCat::Render,
    ];

    pub const fn idx(self) -> usize {
        match self {
            PerfCat::Movement => 0,
            PerfCat::ChunkStream => 1,
            PerfCat::Fluids => 2,
            PerfCat::TexAnim => 3,
            PerfCat::Uniforms => 4,
            PerfCat::Particles => 5,
            PerfCat::Highlight => 6,
            PerfCat::Render => 7,
        }
    }

    pub const fn short(self) -> &'static str {
        match self {
            PerfCat::Movement => "mov",
            PerfCat::ChunkStream => "chunk",
            PerfCat::Fluids => "fluid",
            PerfCat::TexAnim => "tex",
            PerfCat::Uniforms => "ubo",
            PerfCat::Particles => "part",
            PerfCat::Highlight => "hl",
            PerfCat::Render => "rend",
        }
    }
}

pub struct PerfScope<'a> {
    stats: &'a mut PerfStats,
    cat: PerfCat,
    start: Instant,
}

impl Drop for PerfScope<'_> {
    fn drop(&mut self) {
        let ms = self.start.elapsed().as_secs_f32() * 1000.0;
        self.stats.frame_cat_accum[self.cat.idx()] += ms;
    }
}

pub struct PerfStats {
    enabled: bool,
    ema_frame_ms: f32,
    ema_cat_ms: [f32; PerfCat::N],
    frame_start: Instant,
    frame_cat_accum: [f32; PerfCat::N],
    last_report: Instant,
    extra_suffix: String,
}

impl PerfStats {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            enabled: true,
            ema_frame_ms: 0.0,
            ema_cat_ms: [0.0; PerfCat::N],
            frame_start: now,
            frame_cat_accum: [0.0; PerfCat::N],
            last_report: now,
            extra_suffix: String::new(),
        }
    }

    pub fn toggle(&mut self) {
        self.enabled = !self.enabled;
    }

    pub fn begin_frame(&mut self) {
        self.frame_start = Instant::now();
        self.frame_cat_accum = [0.0; PerfCat::N];
    }

    pub fn end_frame(&mut self) -> f32 {
        let frame_ms = self.frame_start.elapsed().as_secs_f32() * 1000.0;
        self.ema_frame_ms = ema_step(self.ema_frame_ms, frame_ms);
        for i in 0..PerfCat::N {
            self.ema_cat_ms[i] = ema_step(self.ema_cat_ms[i], self.frame_cat_accum[i]);
        }
        frame_ms
    }

    pub fn scope(&mut self, cat: PerfCat) -> PerfScope<'_> {
        PerfScope {
            stats: self,
            cat,
            start: Instant::now(),
        }
    }

    pub fn set_extra_suffix(&mut self, s: String) {
        self.extra_suffix = s;
    }

    pub fn format_title_text(&self, fps: u32) -> String {
        let mut title = String::with_capacity(256);
        let _ = write!(
            &mut title,
            "Minecraft Klon - {} FPS | frame {:.1}ms",
            fps, self.ema_frame_ms
        );

        if self.enabled {
            let denom = self.ema_frame_ms.max(0.001);
            for cat in PerfCat::ORDER {
                let ms = self.ema_cat_ms[cat.idx()];
                let pct = (ms / denom) * 100.0;
                let _ = write!(&mut title, " | {} {:.2}ms({:.0}%)", cat.short(), ms, pct);
            }
        }
        if !self.extra_suffix.is_empty() {
            let _ = write!(&mut title, " | {}", self.extra_suffix);
        }

        title
    }

    pub fn maybe_update_title(&mut self, window: &Window, fps: u32) {
        let now = Instant::now();
        if now.duration_since(self.last_report) < REPORT_INTERVAL {
            return;
        }
        self.last_report = now;

        let title = self.format_title_text(fps);
        window.set_title(&title);
    }
}

fn ema_step(prev: f32, sample: f32) -> f32 {
    prev.mul_add(1.0 - EMA_ALPHA, sample * EMA_ALPHA)
}
