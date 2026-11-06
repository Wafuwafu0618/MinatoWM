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
/// The most an animation advances per frame (two frames at 60 Hz).
const MAX_FRAME_STEP: Duration = Duration::from_millis(34);
/// How long the overlay stays up after the last animation.
const OVERLAY_LINGER: Duration = Duration::from_secs(2);
/// Crossfade from a slid-in picture to the live windows.
const SETTLE_FADE: Duration = Duration::from_millis(90);

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
    /// `clock` is the animation's own time: it advances by at most
    /// [`MAX_FRAME_STEP`] per frame, so a dropped frame delays the motion
    /// instead of making it jump.
    Playing { clock: Duration, last: Instant, anim: Anim },
}

enum Anim {
    /// Hold the pictures, then fade them out.
    Fade {
        hold: Duration,
        duration: Duration,
        curve: Easing,
    },
    /// Move the strip from shift `from` to `to`. `v0`: starting velocity
    /// (progress per second) for physical springs.
    Slide {
        from: (f32, f32),
        to: (f32, f32),
        duration: Duration,
        curve: Easing,
        v0: f32,
    },
}

/// One picture of the strip, at `base` when the strip is not shifted.
struct Tile {
    layer: Layer,
    base: (f32, f32),
    /// The workspace it shows, if it is a whole picture of one.
    ws: Option<CacheKey>,
}

/// What covers one monitor: a strip of workspace pictures that slides as a
/// whole. A switch during a slide extends the strip instead of capturing
/// the half-finished picture, so every workspace stays one intact picture
/// and the motion simply continues.
struct Scene {
    group: Group,
    area: Rect,
    tiles: Vec<Tile>,
    /// Current offset of the whole strip.
    shift: (f32, f32),
    /// Strip position of the workspace being shown (or slid to).
    cursor: (f32, f32),
    /// Strip velocity in pixels per second, carried into the next slide.
    velocity: (f32, f32),
    /// Crossfading from the slid-in picture to the live windows.
    settling: bool,
    state: State,
}

impl Scene {
    fn place(&self) {
        place(&self.tiles, self.shift);
    }

    fn sliding(&self) -> bool {
        matches!(self.state, State::Playing { anim: Anim::Slide { .. }, .. })
    }

    /// Whether the strip has a picture where the workspace at `cursor` is.
    fn covers_cursor(&self) -> bool {
        let (w, h) = (self.area.w as f32, self.area.h as f32);
        self.tiles
            .iter()
            .any(|t| (t.base.0 - self.cursor.0).abs() < w && (t.base.1 - self.cursor.1).abs() < h)
    }
}

fn place(tiles: &[Tile], shift: (f32, f32)) {
    for t in tiles {
        t.layer.set_offset(t.base.0 + shift.0, t.base.1 + shift.1);
    }
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
        let mut last_busy = Instant::now();
        loop {
            let busy = !self.scenes.is_empty() || !self.borders.is_empty();
            if busy {
                last_busy = Instant::now();
            } else {
                // Keep the (empty, click-through) overlay up for a while:
                // a freshly shown window can take an extra frame to appear,
                // which the next freeze would otherwise have to wait out.
                let linger = OVERLAY_LINGER.saturating_sub(last_busy.elapsed());
                if linger.is_zero() {
                    self.comp.hide();
                }
                let wait = if self.comp.shown() { linger } else { Duration::from_millis(250) };
                // Idle: wait for work, servicing the overlay window meanwhile.
                match rx.recv_timeout(wait.max(Duration::from_millis(1))) {
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
            Msg::Play { key, play } => self.play(key, play),
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

    fn play(&mut self, key: isize, play: Play) {
        let Some(scene) = self.scenes.get_mut(&key) else {
            return;
        };
        if !matches!(scene.state, State::Frozen { .. }) {
            // Already released by the timeout, or a layout change that
            // arrived mid-slide (the slide keeps going).
            return;
        }
        let anim = match play {
            Play::Fade { hold, duration, curve } => {
                scene.velocity = (0.0, 0.0);
                Anim::Fade { hold, duration, curve }
            }
            Play::Slide {
                incoming,
                dir,
                duration,
                curve,
            } => {
                let (w, h) = (scene.area.w as f32, scene.area.h as f32);
                let target = (scene.cursor.0 + dir.0 as f32 * w, scene.cursor.1 + dir.1 as f32 * h);
                let near = |b: (f32, f32)| (b.0 - target.0).abs() < 0.5 && (b.1 - target.1).abs() < 0.5;
                // Going back to a workspace still in the strip: reuse it.
                let reuse = incoming.is_some() && scene.tiles.iter().any(|t| t.ws == incoming && near(t.base));
                if !reuse {
                    // Whatever sits in the target's spot shows something else.
                    let comp = &self.comp;
                    let group = &scene.group;
                    scene.tiles.retain(|t| {
                        let overlaps = (t.base.0 - target.0).abs() < w && (t.base.1 - target.1).abs() < h;
                        if overlaps {
                            comp.remove_layer(group, &t.layer);
                        }
                        !overlaps
                    });
                    if let Some(ck) = incoming {
                        if let Some((surface, cw, chh)) = self.cache.get(&ck) {
                            if *cw == scene.area.w && *chh == scene.area.h {
                                match self.comp.layer_from_surface(&scene.group, surface.clone()) {
                                    Ok(layer) => scene.tiles.push(Tile {
                                        layer,
                                        base: target,
                                        ws: Some(ck),
                                    }),
                                    Err(e) => log::warn!("incoming layer failed: {e}"),
                                }
                            }
                        }
                    }
                }
                let from = scene.shift;
                let to = (-target.0, -target.1);
                let (dx, dy) = (to.0 - from.0, to.1 - from.1);
                let dist = (dx * dx + dy * dy).sqrt();
                // Keep the momentum of an interrupted slide (negative if it
                // was heading the other way).
                let v0 = if dist > 1.0 {
                    (scene.velocity.0 * dx + scene.velocity.1 * dy) / (dist * dist)
                } else {
                    0.0
                };
                scene.cursor = target;
                scene.place();
                Anim::Slide {
                    from,
                    to,
                    duration,
                    curve,
                    v0,
                }
            }
        };
        scene.state = State::Playing {
            clock: Duration::ZERO,
            last: Instant::now(),
            anim,
        };
    }

    /// Captures `sub` (relative to `area`) of the screen as a layer of `group`.
    fn capture_layer(&mut self, group: &Group, area: Rect, sub: Rect) -> Option<Layer> {
        let abs = Rect::new(area.x + sub.x, area.y + sub.y, sub.w, sub.h);
        // GPU path first (never contains the cursor); GDI if this area isn't
        // duplicable.
        let layer = match self.dup.capture(&self.comp.ctx, abs) {
            Some((tex, x, y)) => self
                .comp
                .layer_from_texture(group, &tex, x, y, abs.w as u32, abs.h as u32),
            None => self.comp.layer(group, &capture::capture(abs)?),
        };
        layer.map_err(|e| log::warn!("capture failed: {e}")).ok()
    }

    fn freeze(&mut self, key: isize, area: Rect, cache_as: Option<CacheKey>) -> bool {
        let started = Instant::now();
        if self.scenes.get(&key).is_some_and(|s| s.sliding() && s.area == area) {
            if cache_as.is_some() {
                self.interrupt_slide(key);
            }
            // A layout change mid-slide is simply covered by the slide.
            log::debug!("continued slide in {:?}", started.elapsed());
            return true;
        }
        let group = match self.comp.group(area) {
            Ok(g) => g,
            Err(e) => {
                log::warn!("freeze failed: {e}");
                return false;
            }
        };
        let Some(out) = self.capture_layer(&group, area, Rect::new(0, 0, area.w, area.h)) else {
            self.comp.remove_group(&group);
            return false;
        };
        // The new picture already shows any half-finished animation (the
        // overlay is part of the captured desktop), so the old scene can
        // simply be replaced. Mid layout-fade that picture is an old layout,
        // though: not something to slide back to later.
        let old = self.scenes.remove(&key);
        let trustworthy = old.as_ref().is_none_or(|s| s.settling);
        if let Some(old) = &old {
            self.comp.remove_group(&old.group);
        }
        if let Some(ck) = cache_as {
            self.cache.remove(&ck);
            self.cache_order.retain(|k| *k != ck);
            if trustworthy {
                self.cache.insert(ck, (out.surface.clone(), area.w, area.h));
                self.cache_order.push(ck);
                while self.cache_order.len() > CACHE_ENTRIES {
                    let oldest = self.cache_order.remove(0);
                    self.cache.remove(&oldest);
                }
            }
        }
        self.scenes.insert(
            key,
            Scene {
                group,
                area,
                tiles: vec![Tile {
                    layer: out,
                    base: (0.0, 0.0),
                    ws: cache_as,
                }],
                shift: (0.0, 0.0),
                cursor: (0.0, 0.0),
                velocity: (0.0, 0.0),
                settling: false,
                state: State::Frozen { since: Instant::now() },
            },
        );
        let fresh = self.comp.show();
        self.comp.commit();
        unsafe {
            let _ = windows::Win32::Graphics::Dwm::DwmFlush();
            if fresh {
                // A window that just appeared may miss the first composition.
                let _ = windows::Win32::Graphics::Dwm::DwmFlush();
            }
        }
        log::debug!("froze {}x{} in {:?}", area.w, area.h, started.elapsed());
        true
    }

    /// Stops the strip where it is until the next `play` continues it. If
    /// the workspace being slid in had no picture, the live windows were
    /// showing through; they are captured now, before the WM hides them.
    fn interrupt_slide(&mut self, key: isize) {
        let Some(scene) = self.scenes.get(&key) else {
            return;
        };
        let area = scene.area;
        let gap = (!scene.covers_cursor())
            .then(|| {
                let x0 = (scene.cursor.0 + scene.shift.0).round() as i32;
                let y0 = (scene.cursor.1 + scene.shift.1).round() as i32;
                let (l, t) = (x0.max(0), y0.max(0));
                let (r, b) = ((x0 + area.w).min(area.w), (y0 + area.h).min(area.h));
                (r > l && b > t).then(|| Rect::new(l, t, r - l, b - t))
            })
            .flatten();
        if let Some(sub) = gap {
            let group = scene.group.clone();
            if let Some(layer) = self.capture_layer(&group, area, sub) {
                let scene = self.scenes.get_mut(&key).unwrap();
                let base = (sub.x as f32 - scene.shift.0, sub.y as f32 - scene.shift.1);
                layer.set_offset(sub.x as f32, sub.y as f32);
                scene.tiles.push(Tile { layer, base, ws: None });
            }
        }
        let scene = self.scenes.get_mut(&key).unwrap();
        scene.state = State::Frozen { since: Instant::now() };
        self.comp.commit();
    }

    /// Advances every animation by one frame and drops finished ones.
    fn step(&mut self) {
        let now = Instant::now();
        let mut done = Vec::new();
        for (key, scene) in self.scenes.iter_mut() {
            if let State::Frozen { since } = scene.state {
                if now - since > FREEZE_TIMEOUT {
                    scene.velocity = (0.0, 0.0);
                    scene.state = State::Playing {
                        clock: Duration::ZERO,
                        last: now,
                        anim: Anim::Fade {
                            hold: Duration::ZERO,
                            duration: Duration::from_millis(150),
                            curve: Easing::LINEAR,
                        },
                    };
                }
            }
            let State::Playing { clock, last, anim } = &mut scene.state else {
                continue;
            };
            *clock += now.saturating_duration_since(*last).min(MAX_FRAME_STEP);
            *last = now;
            let t = *clock;
            let mut slid_in = false;
            let finished = match anim {
                Anim::Fade { hold, duration, curve } => {
                    let s = curve.sample(t.saturating_sub(*hold).as_secs_f32(), duration.as_secs_f32(), 0.0);
                    for tile in &scene.tiles {
                        tile.layer.set_opacity(1.0 - s.value);
                    }
                    t >= *hold && s.done
                }
                Anim::Slide {
                    from,
                    to,
                    duration,
                    curve,
                    v0,
                } => {
                    let s = curve.sample(t.as_secs_f32(), duration.as_secs_f32(), *v0);
                    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
                    scene.shift = (from.0 + dx * s.value, from.1 + dy * s.value);
                    scene.velocity = (dx * s.velocity, dy * s.velocity);
                    place(&scene.tiles, scene.shift);
                    slid_in = s.done;
                    false
                }
            };
            if slid_in {
                if scene.covers_cursor() {
                    // The picture may be a little out of date: crossfade to
                    // the live windows instead of swapping in one frame.
                    scene.velocity = (0.0, 0.0);
                    scene.settling = true;
                    scene.state = State::Playing {
                        clock: Duration::ZERO,
                        last: now,
                        anim: Anim::Fade {
                            hold: Duration::ZERO,
                            duration: SETTLE_FADE,
                            curve: Easing::LINEAR,
                        },
                    };
                } else {
                    done.push(*key);
                }
            } else if finished {
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
