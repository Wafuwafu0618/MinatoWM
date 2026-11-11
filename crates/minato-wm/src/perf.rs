//! Opt-in timing records for benchmarking (see `minato-bench`).
//!
//! Disabled unless [`enable`] was called; then every WM batch and every
//! animation leaves a record in a bounded buffer that the control pipe
//! hands out. All recording functions are cheap no-ops when disabled.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;

const MAX_RECORDS: usize = 1000;

static ENABLED: AtomicBool = AtomicBool::new(false);
/// Whether the animation thread has anything on screen right now.
static ANIM_BUSY: AtomicBool = AtomicBool::new(false);
static EPOCH: OnceLock<Instant> = OnceLock::new();

static BATCHES: Mutex<VecDeque<BatchPerf>> = Mutex::new(VecDeque::new());
static ANIMS: Mutex<VecDeque<AnimPerf>> = Mutex::new(VecDeque::new());
/// The batch being handled by the WM thread.
static CURRENT: Mutex<Option<BatchPerf>> = Mutex::new(None);
/// Freezes measured on the animation thread, not yet claimed by a batch.
static FREEZES: Mutex<Vec<FreezePerf>> = Mutex::new(Vec::new());

pub fn enable() {
    EPOCH.get_or_init(Instant::now);
    ENABLED.store(true, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub fn set_anim_busy(busy: bool) {
    ANIM_BUSY.store(busy, Ordering::Relaxed);
}

pub fn anim_busy() -> bool {
    ANIM_BUSY.load(Ordering::Relaxed)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Milliseconds since perf was enabled.
pub fn now_ms() -> f64 {
    EPOCH.get().map_or(0.0, |e| ms(e.elapsed()))
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FreezePerf {
    pub total_ms: f64,
    /// Desktop Duplication (`"dup"`), GDI (`"gdi"`), or none (`"none"`:
    /// a running slide was continued instead).
    pub capture: &'static str,
    pub capture_ms: f64,
    pub commit_ms: f64,
    pub flush_ms: f64,
    /// The overlay had been hidden and needed a second flush.
    pub fresh: bool,
    pub w: i32,
    pub h: i32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct OpStat {
    pub count: u32,
    pub ms: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BatchPerf {
    pub t_ms: f64,
    pub trigger: String,
    pub messages: u32,
    pub total_ms: f64,
    /// Time the WM thread waited for freezes to be on screen.
    pub freeze_wait_ms: f64,
    pub freezes: Vec<FreezePerf>,
    pub set_position: OpStat,
    pub show: OpStat,
    pub hide: OpStat,
    /// Focusing a window (and nudging the cursor after a switch).
    pub focus: OpStat,
    pub after_batch_ms: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AnimPerf {
    /// When the animation ended.
    pub t_ms: f64,
    /// When it started moving.
    pub start_ms: f64,
    pub kind: &'static str,
    pub duration_ms: f64,
    pub frames: u32,
    pub max_gap_ms: f64,
    /// Frames that took longer than 1.5 refresh periods.
    pub slow_frames: u32,
    pub refresh_ms: f64,
}

fn push<T>(q: &Mutex<VecDeque<T>>, v: T) {
    if let Ok(mut q) = q.lock() {
        if q.len() >= MAX_RECORDS {
            q.pop_front();
        }
        q.push_back(v);
    }
}

/// Starts recording a WM batch whose first message is `trigger`.
pub fn batch_begin(trigger: impl FnOnce() -> String) {
    if !enabled() {
        return;
    }
    if let Ok(mut c) = CURRENT.lock() {
        *c = Some(BatchPerf {
            t_ms: now_ms(),
            trigger: trigger(),
            ..Default::default()
        });
    }
}

fn with_current(f: impl FnOnce(&mut BatchPerf)) {
    if !enabled() {
        return;
    }
    if let Ok(mut c) = CURRENT.lock() {
        if let Some(b) = c.as_mut() {
            f(b);
        }
    }
}

pub fn batch_message() {
    with_current(|b| b.messages += 1);
}

/// Finishes the current batch. `total` and `after` are its durations.
pub fn batch_end(total: Duration, after: Duration) -> Option<BatchPerf> {
    if !enabled() {
        return None;
    }
    let mut b = CURRENT.lock().ok()?.take()?;
    b.total_ms = ms(total);
    b.after_batch_ms = ms(after);
    push(&BATCHES, b.clone());
    Some(b)
}

#[derive(Clone, Copy)]
pub enum Op {
    SetPosition,
    Show,
    Hide,
    Focus,
}

/// Runs `f`, adding its duration to the current batch under `op`.
pub fn time<R>(op: Op, f: impl FnOnce() -> R) -> R {
    if !enabled() {
        return f();
    }
    let t = Instant::now();
    let r = f();
    let d = ms(t.elapsed());
    with_current(|b| {
        let s = match op {
            Op::SetPosition => &mut b.set_position,
            Op::Show => &mut b.show,
            Op::Hide => &mut b.hide,
            Op::Focus => &mut b.focus,
        };
        s.count += 1;
        s.ms += d;
    });
    r
}

/// Called by the WM thread after waiting `waited` for a freeze.
pub fn freeze_waited(waited: Duration) {
    with_current(|b| {
        b.freeze_wait_ms += ms(waited);
        if let Ok(mut f) = FREEZES.lock() {
            b.freezes.append(&mut f);
        }
    });
}

/// Called by the animation thread before acknowledging a freeze.
pub fn record_freeze(f: FreezePerf) {
    if !enabled() {
        return;
    }
    if let Ok(mut q) = FREEZES.lock() {
        q.push(f);
    }
}

pub fn record_anim(a: AnimPerf) {
    if enabled() {
        push(&ANIMS, a);
    }
}

/// Everything recorded so far, optionally clearing it.
pub fn dump(clear: bool) -> (Vec<BatchPerf>, Vec<AnimPerf>) {
    fn take<T: Clone>(q: &Mutex<VecDeque<T>>, clear: bool) -> Vec<T> {
        let Ok(mut q) = q.lock() else { return Vec::new() };
        if clear {
            q.drain(..).collect()
        } else {
            q.iter().cloned().collect()
        }
    }
    (take(&BATCHES, clear), take(&ANIMS, clear))
}

/// Frame statistics of one animation, kept by the animation thread.
pub struct FrameStats {
    kind: &'static str,
    start: Instant,
    start_ms: f64,
    last: Instant,
    frames: u32,
    max_gap: Duration,
    slow: u32,
    refresh: Duration,
}

impl FrameStats {
    pub fn new(kind: &'static str, refresh: Duration) -> Self {
        let now = Instant::now();
        Self {
            kind,
            start: now,
            start_ms: now_ms(),
            last: now,
            frames: 0,
            max_gap: Duration::ZERO,
            slow: 0,
            refresh,
        }
    }

    pub fn frame(&mut self, now: Instant) {
        let gap = now.saturating_duration_since(self.last);
        self.last = now;
        if self.frames > 0 {
            self.max_gap = self.max_gap.max(gap);
            if gap.as_secs_f64() > self.refresh.as_secs_f64() * 1.5 {
                self.slow += 1;
            }
        }
        self.frames += 1;
    }

    pub fn finish(&self) {
        record_anim(AnimPerf {
            t_ms: now_ms(),
            start_ms: self.start_ms,
            kind: self.kind,
            duration_ms: ms(self.start.elapsed()),
            frames: self.frames,
            max_gap_ms: ms(self.max_gap),
            slow_frames: self.slow,
            refresh_ms: ms(self.refresh),
        });
    }
}
