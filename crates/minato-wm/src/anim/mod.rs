//! Freeze-frame animations.
//!
//! Before the WM changes what a monitor shows, it asks the animation thread
//! to capture the monitor and cover it with that picture ([`Animator::freeze`]).
//! Underneath, windows are moved / shown / hidden instantly. Then the picture
//! is animated away ([`Animator::play`]): faded out for layout changes, pushed
//! aside for workspace switches. Apps never have to animate themselves, so a
//! slow app can't make the animation stutter, and a new change mid-animation
//! simply captures the half-finished picture and continues from there.
//!
//! Capture uses DXGI Desktop Duplication (GPU only) and falls back to GDI.

pub mod capture;
mod compositor;
pub mod curve;
mod duplication;
mod diagnose;

pub use diagnose::cursor_report;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use minato_config::Color;
use minato_layout::Rect;
use windows::Win32::Graphics::DirectComposition::IDCompositionSurface;

pub use capture::Frame;
use compositor::{Compositor, Group, Layer};
use curve::Easing;
use duplication::Duplicator;

use crate::win32::Hwnd;

/// A frozen picture that never gets a `play` is released after this long.
const FREEZE_TIMEOUT: Duration = Duration::from_millis(1200);
/// How long `freeze` waits for the picture to be on screen.
const FREEZE_ACK_TIMEOUT: Duration = Duration::from_millis(150);
/// Stand-in for "system default" when blending border colours.
const DEFAULT_BORDER: Color = Color::rgb(0x5a, 0x5a, 0x5a);
/// Cached workspace pictures kept on the GPU (a 4K picture is ~37 MB).
const CACHE_ENTRIES: usize = 6;

/// Identifies a cached picture: (monitor handle, workspace index).
pub type CacheKey = (isize, usize);

#[derive(Debug, Clone)]
pub enum Play {
    /// Hold the picture, then fade it out.
    Fade {
        hold: Duration,
        duration: Duration,
        curve: Easing,
    },
    /// Push the picture off towards `-dir` while the cached picture
    /// `incoming` (if present) comes in from `+dir`. `dir` is a unit vector
    /// such as `(1, 0)`.
    Slide {
        incoming: Option<CacheKey>,
        dir: (i32, i32),
        duration: Duration,
        curve: Easing,
    },
}

enum Msg {
    Freeze {
        key: isize,
        area: Rect,
        cache_as: Option<CacheKey>,
        ack: Sender<bool>,
    },
    Play {
        key: isize,
        play: Play,
    },
    Invalidate(CacheKey),
    ClearCache,
    Border {
        hwnd: Hwnd,
        from: Option<Color>,
        to: Option<Color>,
        duration: Duration,
        curve: Easing,
    },
}

/// Handle to the animation thread. Cheap to clone; all methods are no-ops
/// if the overlay could not be created.
#[derive(Clone)]
pub struct Animator {
    tx: Option<Sender<Msg>>,
}

impl Animator {
    pub fn start() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (ready_tx, ready_rx) = crossbeam_channel::bounded(1);
        let spawned = std::thread::Builder::new().name("anim".into()).spawn(move || {
            let compositor = match Compositor::new() {
                Ok(c) => {
                    let _ = ready_tx.send(true);
                    c
                }
                Err(e) => {
                    log::error!("animations disabled: could not create the overlay: {e}");
                    let _ = ready_tx.send(false);
                    return;
                }
            };
            Engine::new(compositor).run(rx);
        });
        let ok = spawned.is_ok() && ready_rx.recv_timeout(Duration::from_secs(5)).unwrap_or(false);
        Self { tx: ok.then_some(tx) }
    }

    /// An animator that does nothing.
    pub fn disabled() -> Self {
        Self { tx: None }
    }

    pub fn available(&self) -> bool {
        self.tx.is_some()
    }

    /// Captures `area` and covers it with the picture, returning `true` once
    /// the picture is on screen. `key` identifies the monitor. With
    /// `cache_as`, the picture is also kept for a later slide-in.
    pub fn freeze(&self, key: isize, area: Rect, cache_as: Option<CacheKey>) -> bool {
        let Some(tx) = &self.tx else {
            return false;
        };
        let (ack, ack_rx) = crossbeam_channel::bounded(1);
        if tx.send(Msg::Freeze { key, area, cache_as, ack }).is_err() {
            return false;
        }
        ack_rx.recv_timeout(FREEZE_ACK_TIMEOUT).unwrap_or_else(|_| {
            log::debug!("freeze ack timed out");
            // The picture may still show up; release it normally.
            true
        })
    }

    pub fn play(&self, key: isize, play: Play) {
        self.send(Msg::Play { key, play });
    }

    /// Forgets a cached picture whose workspace changed.
    pub fn invalidate(&self, key: CacheKey) {
        self.send(Msg::Invalidate(key));
    }

    pub fn clear_cache(&self) {
        self.send(Msg::ClearCache);
    }

    pub fn border(&self, hwnd: Hwnd, from: Option<Color>, to: Option<Color>, duration: Duration, curve: Easing) {
        match &self.tx {
            Some(_) => self.send(Msg::Border {
                hwnd,
                from,
                to,
                duration,
                curve,
            }),
            None => hwnd.set_border_color(to),
        }
    }

    fn send(&self, msg: Msg) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(msg);
        }
    }
}

enum State {
    Frozen { since: Instant },
    /// `v0`: starting velocity (progress per second) for physical springs.
    Playing { start: Instant, play: Play, v0: f32 },
}

struct Scene {
    group: Group,
    area: Rect,
    out: Layer,
    incoming: Option<Layer>,
    state: State,
    /// Direction and velocity of the slide currently playing.
    motion: Option<((i32, i32), f32)>,
    /// Motion of the slide this scene interrupted, handed to its own slide
    /// so rapid switches keep their momentum.
    carry: Option<((i32, i32), f32)>,
}

struct BorderFade {
    from: Color,
    to: Color,
    to_default: bool,
    start: Instant,
    duration: Duration,
    curve: Easing,
    current: Color,
}

fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    Color::rgb(l(a.r, b.r), l(a.g, b.g), l(a.b, b.b))
}

struct Engine {
    comp: Compositor,
    dup: Duplicator,
    scenes: HashMap<isize, Scene>,
    borders: HashMap<Hwnd, BorderFade>,
    /// Pictures of background workspaces with the size they were taken at.
    cache: HashMap<CacheKey, (IDCompositionSurface, i32, i32)>,
    cache_order: Vec<CacheKey>,
}

/// Shows or hides the system cursor for every app (Magnification API).
/// Only used by the `cursor_check` diagnostic: Desktop Duplication frames
/// never contain the cursor, so captures don't need it hidden.
pub fn show_system_cursor(show: bool) {
    unsafe {
        windows_sys::Win32::UI::Magnification::MagShowSystemCursor(show as i32);
    }
}

impl Engine {
    fn new(comp: Compositor) -> Self {
        let dup = Duplicator::new(&comp.d3d);
        Self {
            comp,
            dup,
            scenes: HashMap::new(),
            borders: HashMap::new(),
            cache: HashMap::new(),
            cache_order: Vec::new(),
        }
    }

    fn run(mut self, rx: Receiver<Msg>) {
        loop {
            let busy = !self.scenes.is_empty() || !self.borders.is_empty();
            if !busy {
                self.comp.hide();
                // Idle: wait for work, servicing the overlay window meanwhile.
                match rx.recv_timeout(Duration::from_millis(250)) {
                    Ok(msg) => self.handle(msg),
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        self.comp.pump();
                        continue;
                    }
                    Err(_) => return,
                }
            }
            while let Ok(msg) = rx.try_recv() {
                self.handle(msg);
            }
            self.comp.pump();
            self.step();
            self.comp.commit();
            // Pace with the compositor (one call per refresh).
            unsafe {
                if windows::Win32::Graphics::Dwm::DwmFlush().is_err() {
                    std::thread::sleep(Duration::from_millis(8));
                }
            }
        }
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Freeze { key, area, cache_as, ack } => {
                let ok = self.freeze(key, area, cache_as);
                let _ = ack.send(ok);
            }
            Msg::Play { key, play } => {
                let Some(scene) = self.scenes.get_mut(&key) else {
                    return;
                };
                if !matches!(scene.state, State::Frozen { .. }) {
                    // Already released by the timeout.
                    return;
                }
                if let Play::Slide { incoming: Some(ck), dir, .. } = &play {
                    if let Some((surface, w, h)) = self.cache.get(ck) {
                        if *w == scene.area.w && *h == scene.area.h {
                            match self.comp.layer_from_surface(&scene.group, surface.clone()) {
                                Ok(layer) => {
                                    layer.set_offset((dir.0 * w) as f32, (dir.1 * h) as f32);
                                    scene.incoming = Some(layer);
                                }
                                Err(e) => log::warn!("incoming layer failed: {e}"),
                            }
                        }
                    }
                }
                // Continue an interrupted slide: same direction keeps the speed,
                // the opposite direction starts out moving backwards.
                let v0 = match (&play, scene.carry) {
                    (Play::Slide { dir, .. }, Some((old, vel))) => vel * (old.0 * dir.0 + old.1 * dir.1) as f32,
                    _ => 0.0,
                };
                scene.state = State::Playing {
                    start: Instant::now(),
                    play,
                    v0,
                };
            }
            Msg::Invalidate(ck) => {
                self.cache.remove(&ck);
                self.cache_order.retain(|k| *k != ck);
            }
            Msg::ClearCache => {
                self.cache.clear();
                self.cache_order.clear();
            }
            Msg::Border {
                hwnd,
                from,
                to,
                duration,
                curve,
            } => {
                // Retarget from wherever an in-flight fade currently is.
                let from = self
                    .borders
                    .get(&hwnd)
                    .map(|b| b.current)
                    .unwrap_or(from.unwrap_or(DEFAULT_BORDER));
                self.borders.insert(
                    hwnd,
                    BorderFade {
                        from,
                        to: to.unwrap_or(DEFAULT_BORDER),
                        to_default: to.is_none(),
                        start: Instant::now(),
                        duration,
                        curve,
                        current: from,
                    },
                );
            }
        }
    }

    fn freeze(&mut self, key: isize, area: Rect, cache_as: Option<CacheKey>) -> bool {
        let started = Instant::now();
        let group = match self.comp.group(area) {
            Ok(g) => g,
            Err(e) => {
                log::warn!("freeze failed: {e}");
                return false;
            }
        };
        // GPU path first (never contains the cursor); GDI if this area isn't
        // duplicable.
        let captured = self.dup.capture(&self.comp.ctx, area);
        let frame = if captured.is_none() { capture::capture(area) } else { None };
        let layer = match captured {
            Some((tex, x, y)) => self
                .comp
                .layer_from_texture(&group, &tex, x, y, area.w as u32, area.h as u32),
            None => match frame {
                Some(frame) => self.comp.layer(&group, &frame),
                None => {
                    self.comp.remove_group(&group);
                    return false;
                }
            },
        };
        let out = match layer {
            Ok(l) => l,
            Err(e) => {
                log::warn!("freeze failed: {e}");
                self.comp.remove_group(&group);
                return false;
            }
        };
        if let Some(ck) = cache_as {
            self.cache.insert(ck, (out.surface.clone(), area.w, area.h));
            self.cache_order.retain(|k| *k != ck);
            self.cache_order.push(ck);
            while self.cache_order.len() > CACHE_ENTRIES {
                let oldest = self.cache_order.remove(0);
                self.cache.remove(&oldest);
            }
        }
        // The new picture already shows any half-finished animation (the
        // overlay is part of the captured desktop), so the old scene can
        // simply be replaced.
        let mut carry = None;
        if let Some(old) = self.scenes.remove(&key) {
            carry = old.motion;
            self.comp.remove_group(&old.group);
        }
        self.scenes.insert(
            key,
            Scene {
                group,
                area,
                out,
                incoming: None,
                state: State::Frozen { since: Instant::now() },
                motion: None,
                carry,
            },
        );
        self.comp.show();
        self.comp.commit();
        unsafe {
            let _ = windows::Win32::Graphics::Dwm::DwmFlush();
        }
        log::debug!("froze {}x{} in {:?}", area.w, area.h, started.elapsed());
        true
    }

    /// Advances every animation by one frame and drops finished ones.
    fn step(&mut self) {
        let now = Instant::now();
        let mut done = Vec::new();
        for (key, scene) in self.scenes.iter_mut() {
            if let State::Frozen { since } = scene.state {
                if now - since > FREEZE_TIMEOUT {
                    scene.state = State::Playing {
                        start: now,
                        play: Play::Fade {
                            hold: Duration::ZERO,
                            duration: Duration::from_millis(150),
                            curve: Easing::LINEAR,
                        },
                        v0: 0.0,
                    };
                }
            }
            let State::Playing { start, play, v0 } = &scene.state else {
                continue;
            };
            let elapsed = now - *start;
            let finished = match play {
                Play::Fade { hold, duration, curve } => {
                    let s = curve.sample(elapsed.saturating_sub(*hold).as_secs_f32(), duration.as_secs_f32(), 0.0);
                    scene.out.set_opacity(1.0 - s.value);
                    elapsed >= *hold && s.done
                }
                Play::Slide { dir, duration, curve, .. } => {
                    let s = curve.sample(elapsed.as_secs_f32(), duration.as_secs_f32(), *v0);
                    let p = s.value;
                    scene.motion = Some((*dir, s.velocity));
                    let (w, h) = (scene.area.w as f32, scene.area.h as f32);
                    let (dx, dy) = (dir.0 as f32, dir.1 as f32);
                    scene.out.set_offset(-dx * w * p, -dy * h * p);
                    if let Some(inc) = &scene.incoming {
                        inc.set_offset(dx * w * (1.0 - p), dy * h * (1.0 - p));
                    }
                    s.done
                }
            };
            if finished {
                done.push(*key);
            }
        }
        for key in done {
            if let Some(scene) = self.scenes.remove(&key) {
                self.comp.remove_group(&scene.group);
            }
        }

        let mut finished = Vec::new();
        for (hwnd, b) in self.borders.iter_mut() {
            let t = progress(now - b.start, b.duration);
            b.current = lerp_color(b.from, b.to, b.curve.eval(t).clamp(0.0, 1.0));
            if t >= 1.0 {
                hwnd.set_border_color(if b.to_default { None } else { Some(b.to) });
                finished.push(*hwnd);
            } else {
                hwnd.set_border_color(Some(b.current));
            }
        }
        for h in finished {
            self.borders.remove(&h);
        }
    }
}

fn progress(elapsed: Duration, total: Duration) -> f32 {
    if total.is_zero() {
        1.0
    } else {
        (elapsed.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_lerp() {
        let a = Color::rgb(0, 100, 200);
        let b = Color::rgb(100, 100, 0);
        assert_eq!(lerp_color(a, b, 0.0), a);
        assert_eq!(lerp_color(a, b, 1.0), b);
        assert_eq!(lerp_color(a, b, 0.5), Color::rgb(50, 100, 100));
    }

    #[test]
    fn progress_clamps() {
        let s = Duration::from_millis(100);
        assert_eq!(progress(Duration::ZERO, s), 0.0);
        assert_eq!(progress(Duration::from_millis(50), s), 0.5);
        assert_eq!(progress(Duration::from_millis(500), s), 1.0);
        assert_eq!(progress(Duration::from_millis(5), Duration::ZERO), 1.0);
    }
}
