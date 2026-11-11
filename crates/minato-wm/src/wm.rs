//! The window manager itself. Owned by a single thread; everything arrives
//! as a [`WmMsg`] and is handled to completion before the next one.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use minato_config::{
    Action, Color, Config, CycleDirection, FlipAxis, RuleAction, RuleSet, WorkspaceStyle,
};
use minato_layout::{neighbor, Arrangement, Direction, LayoutKind, LayoutParams, Rect, Sizing};

use crate::anim::curve::Easing;
use crate::anim::{Animator, Play};
use crate::{hidden, perf};
use crate::hotkey::{self, Keymap};
use crate::snapshot::{ForegroundSnapshot, MonitorSnapshot, Snapshot, WindowSnapshot, WorkspaceSnapshot};
use crate::state::{Container, Monitor, Ring, Slot, Workspace};
use crate::win32::{self, Hwnd, MonitorInfo};
use crate::winevent::WinEvent;
use crate::{UiRequest, WmMsg};

/// Workspaces auto-created by index never go beyond this.
const MAX_WORKSPACES: usize = 32;
/// How long a close freeze waits for the window to disappear.
const CLOSE_WAIT: Duration = Duration::from_millis(400);

fn ms_u64(v: u32) -> Duration {
    Duration::from_millis(v as u64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Freeze {
    Layout,
    Workspace { from: usize, to: usize },
}

/// Whether a non-desktop window covers the whole monitor (games, videos):
/// an overlay on top of those would be intrusive.
fn fullscreen_on(info: &MonitorInfo) -> bool {
    let Some(fg) = Hwnd::foreground() else {
        return false;
    };
    if matches!(fg.class().as_str(), "Progman" | "WorkerW") {
        return false;
    }
    fg.window_rect().is_some_and(|r| {
        r.x <= info.rect.x && r.y <= info.rect.y && r.right() >= info.rect.right() && r.bottom() >= info.rect.bottom()
    })
}

/// Edge moves smaller than this are rounding noise, not a drag.
const DRAG_EDGE_MIN: i32 = 3;

/// The edges that moved between the tile `old` and the window's frame
/// `new` after a drag, each with how far it moved outward (negative =
/// inward). Empty when the size is unchanged, i.e. the window was moved
/// rather than resized.
fn drag_edges(old: Rect, new: Rect) -> Vec<(Direction, i32)> {
    if (new.w - old.w).abs() < DRAG_EDGE_MIN && (new.h - old.h).abs() < DRAG_EDGE_MIN {
        return Vec::new();
    }
    [
        (Direction::Left, old.x - new.x),
        (Direction::Right, new.right() - old.right()),
        (Direction::Up, old.y - new.y),
        (Direction::Down, new.bottom() - old.bottom()),
    ]
    .into_iter()
    .filter(|(_, d)| d.abs() >= DRAG_EDGE_MIN)
    .collect()
}

fn bar_height(cfg: &Config, info: &MonitorInfo) -> i32 {
    cfg.bar.reserved_px(info.dpi).min(info.work_area.h / 2)
}

fn usable_area(cfg: &Config, info: &MonitorInfo) -> Rect {
    info.work_area.inset_sides(0, bar_height(cfg, info), 0, 0)
}

pub struct WindowManager {
    cfg: Arc<Config>,
    rules: RuleSet,
    monitors: Ring<Monitor>,
    /// Windows hidden by us (other workspaces, background stack members).
    hidden: HashSet<Hwnd>,
    hidden_dirty: bool,
    /// Frame colour last applied to each window.
    borders: HashMap<Hwnd, Option<Color>>,
    paused: bool,
    config_errors: Vec<String>,
    snapshot: Arc<ArcSwap<Snapshot>>,
    ui: Box<dyn Fn(UiRequest) + Send>,
    pub(crate) quit: bool,
    anim: Animator,
    /// Set once the initial layout is done; nothing animates before.
    ready: bool,
    /// Monitors covered by a frozen picture during the current batch.
    frozen: HashMap<usize, Freeze>,
    /// Rect each tiled window was last placed at.
    applied: HashMap<Hwnd, Rect>,
    /// A window being closed by `Action::Close`, for the close fade.
    closing: Option<(usize, Hwnd, Instant)>,
    last_layout_freeze: HashMap<usize, Instant>,
    /// Executable name and class of each window in the snapshot. Neither
    /// changes during a window's life, and querying the executable means
    /// opening its process: too slow to repeat for every window each batch.
    identities: HashMap<Hwnd, (String, String)>,
    /// Control-pipe requests to answer once the current batch is done.
    pub(crate) control_replies: Vec<crossbeam_channel::Sender<Option<crate::perf::BatchPerf>>>,
}

fn make_workspaces(cfg: &Config, monitor: usize) -> Ring<Workspace> {
    let mc = cfg.monitor(monitor);
    let mut list: Vec<Workspace> = mc
        .workspaces
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let name = if w.name.is_empty() { (i + 1).to_string() } else { w.name.clone() };
            Workspace::new(name, w.layout.unwrap_or(cfg.default_layout), w.flip)
        })
        .collect();
    if list.is_empty() {
        list.push(Workspace::new("1", cfg.default_layout, Default::default()));
    }
    Ring::from_vec(list)
}

impl WindowManager {
    pub fn new(
        cfg: Arc<Config>,
        snapshot: Arc<ArcSwap<Snapshot>>,
        ui: Box<dyn Fn(UiRequest) + Send>,
        anim: Animator,
    ) -> Self {
        let monitors = win32::monitors()
            .into_iter()
            .enumerate()
            .map(|(i, info)| Monitor {
                info,
                workspaces: make_workspaces(&cfg, i),
            })
            .collect();
        let mut wm = Self {
            rules: RuleSet::compile_lossy(&cfg.rules),
            cfg: cfg.clone(),
            monitors: Ring::from_vec(monitors),
            hidden: HashSet::new(),
            hidden_dirty: false,
            borders: HashMap::new(),
            paused: false,
            config_errors: Vec::new(),
            snapshot,
            ui,
            quit: false,
            anim,
            ready: false,
            frozen: HashMap::new(),
            applied: HashMap::new(),
            closing: None,
            last_layout_freeze: HashMap::new(),
            identities: HashMap::new(),
            control_replies: Vec::new(),
        };
        wm.config_errors = wm.apply_keymap(&cfg);
        wm.config_errors.extend(cfg.validate());
        wm
    }

    /// Adopts the windows that already exist and lays them out.
    pub fn init(&mut self) {
        // Bottom of the z-order first, so the topmost window ends up focused.
        for hwnd in win32::top_level_windows().into_iter().rev() {
            self.manage(hwnd);
        }
        self.retile_all();
        if let Some(fg) = Hwnd::foreground() {
            self.sync_focus(fg);
        }
        self.after_batch();
        self.ready = true;
    }

    pub fn handle(&mut self, msg: WmMsg) {
        match msg {
            WmMsg::Event(ev, hwnd) => {
                if !self.paused {
                    self.on_event(ev, hwnd);
                }
            }
            WmMsg::Action(action) => {
                if !self.paused || action == Action::TogglePause || action == Action::Quit {
                    self.on_action(action);
                }
            }
            WmMsg::FocusWorkspaceOn { monitor, workspace } => {
                if !self.paused && monitor < self.monitors.len() {
                    self.focus_workspace(monitor, workspace);
                }
            }
            WmMsg::Config(cfg) => self.apply_config(cfg),
            WmMsg::ConfigError(e) => {
                self.config_errors = vec![e];
            }
            WmMsg::Control { op, reply } => {
                match op {
                    crate::ControlOp::Ping => {}
                    // Quitting is not something to do remotely.
                    crate::ControlOp::Action(action) => {
                        if action != Action::Quit && (!self.paused || action == Action::TogglePause) {
                            self.on_action(action);
                        }
                    }
                    crate::ControlOp::FocusWorkspaceOn { monitor, workspace } => {
                        if !self.paused && monitor < self.monitors.len() {
                            self.focus_workspace(monitor, workspace);
                        }
                    }
                }
                self.control_replies.push(reply);
            }
        }
    }

    /// Housekeeping after a burst of messages: borders, crash-recovery
    /// file, and the snapshot for the UI.
    pub fn after_batch(&mut self) {
        self.release_frozen();
        self.update_borders();
        if self.hidden_dirty {
            self.hidden_dirty = false;
            hidden::persist(&self.hidden);
        }
        self.publish_snapshot();
    }

    /// Puts every window back on screen and restores frame colours.
    pub fn shutdown(&mut self) {
        let hidden: Vec<_> = self.hidden.drain().collect();
        for h in hidden {
            h.show();
        }
        if self.anim.available() {
            // Let in-flight border fades finish before resetting colours.
            std::thread::sleep(Duration::from_millis(
                self.cfg.animations.border.duration_ms as u64 + 50,
            ));
        }
        for (h, _) in self.borders.drain() {
            h.set_border_color(None);
        }
        hidden::persist(&HashSet::new());
    }

    pub fn notify_quit(&self) {
        (self.ui)(UiRequest::Quit);
    }

    // ----- lookup helpers -------------------------------------------------

    fn locate(&self, hwnd: Hwnd) -> Option<(usize, usize, Slot)> {
        for (mi, m) in self.monitors.iter().enumerate() {
            for (wi, w) in m.workspaces.iter().enumerate() {
                if let Some(slot) = w.find(hwnd) {
                    return Some((mi, wi, slot));
                }
            }
        }
        None
    }

    fn monitor_index(&self, handle: isize) -> Option<usize> {
        self.monitors.position(|m| m.info.handle == handle)
    }

    fn focused_monitor(&self) -> usize {
        self.monitors.focused_idx()
    }

    fn ws(&self, mi: usize) -> &Workspace {
        self.monitors.get(mi).unwrap().workspace()
    }

    fn ws_mut(&mut self, mi: usize) -> &mut Workspace {
        self.monitors.get_mut(mi).unwrap().workspace_mut()
    }

    fn ensure_workspace(&mut self, mi: usize, wi: usize) -> bool {
        if wi >= MAX_WORKSPACES {
            return false;
        }
        let layout = self.cfg.default_layout;
        let m = self.monitors.get_mut(mi).unwrap();
        while m.workspaces.len() <= wi {
            let name = (m.workspaces.len() + 1).to_string();
            m.workspaces.push(Workspace::new(name, layout, Default::default()));
        }
        true
    }

    /// The work area minus the status bar: where windows go and what
    /// animations cover.
    fn usable_area(&self, mi: usize) -> Rect {
        usable_area(&self.cfg, &self.monitors.get(mi).unwrap().info)
    }

    fn layout_area(&self, mi: usize) -> Rect {
        self.usable_area(mi).inset(self.cfg.gaps.outer)
    }

    fn layout_params(&self, ws: &Workspace) -> LayoutParams {
        LayoutParams {
            primary_ratio: self.cfg.primary_ratio,
            inner_gap: self.cfg.gaps.inner,
            flip: ws.flip,
        }
    }

    /// The managed window the user is working with: the foreground window
    /// if we manage it and it is on a visible workspace, else the focused
    /// tile of the focused monitor.
    fn current(&self) -> Option<(usize, Slot, Hwnd)> {
        if let Some(fg) = Hwnd::foreground() {
            if let Some((mi, wi, slot)) = self.locate(fg) {
                if self.monitors.get(mi).unwrap().workspaces.focused_idx() == wi {
                    return Some((mi, slot, fg));
                }
            }
        }
        let mi = self.focused_monitor();
        let ws = self.ws(mi);
        let ci = ws.containers.focused_idx();
        let c = ws.containers.get(ci)?;
        let wi = c.windows.focused_idx();
        Some((mi, Slot::Tiled { container: ci, window: wi }, c.focused_window()?))
    }

    // ----- hide / show ----------------------------------------------------

    fn hide(&mut self, hwnd: Hwnd) {
        if self.hidden.insert(hwnd) {
            self.hidden_dirty = true;
            perf::time(perf::Op::Hide, || hwnd.hide());
        } else if hwnd.is_visible() {
            // Hidden by us before, but the app showed itself again.
            perf::time(perf::Op::Hide, || hwnd.hide());
        }
    }

    fn show(&mut self, hwnd: Hwnd) {
        if self.hidden.remove(&hwnd) {
            self.hidden_dirty = true;
            perf::time(perf::Op::Show, || hwnd.show());
        }
    }

    fn focus_window(&mut self, hwnd: Hwnd) {
        perf::time(perf::Op::Focus, || hwnd.focus());
        if self.cfg.mouse_follows_focus {
            if let Some(r) = hwnd.frame_rect() {
                let (x, y) = r.center();
                win32::set_cursor_pos(x, y);
            }
        }
    }

    // ----- managing windows -----------------------------------------------

    /// Starts managing `hwnd` if it is an eligible app window. Returns the
    /// monitor it was added to.
    fn manage(&mut self, hwnd: Hwnd) -> Option<usize> {
        if self.locate(hwnd).is_some() || self.hidden.contains(&hwnd) || !hwnd.is_app_window() {
            return None;
        }
        let rule = self.rules.evaluate(&hwnd.info());
        if rule == Some(RuleAction::Ignore) {
            return None;
        }
        let mi = self
            .monitor_index(hwnd.monitor())
            .unwrap_or_else(|| self.focused_monitor());
        let active = self.monitors.get(mi).unwrap().workspaces.focused_idx();
        let wi = match rule {
            Some(RuleAction::Workspace(n)) if self.ensure_workspace(mi, n) => n,
            _ => active,
        };
        let ws = self.monitors.get_mut(mi).unwrap().workspaces.get_mut(wi).unwrap();
        if rule == Some(RuleAction::Float) {
            ws.floating.push(hwnd);
        } else {
            ws.add_tiled(hwnd);
        }
        log::debug!("managing {hwnd:?} {:?} on monitor {mi} workspace {wi}", hwnd.title());
        if wi != active {
            self.hide(hwnd);
            self.invalidate_cache(mi, wi);
        }
        Some(mi)
    }

    /// Stops managing `hwnd`. Returns the monitor it was on.
    fn unmanage(&mut self, hwnd: Hwnd) -> Option<usize> {
        let (mi, wi, _) = self.locate(hwnd)?;
        self.monitors
            .get_mut(mi)
            .unwrap()
            .workspaces
            .get_mut(wi)
            .unwrap()
            .remove(hwnd);
        if self.hidden.remove(&hwnd) {
            self.hidden_dirty = true;
        }
        self.borders.remove(&hwnd);
        self.applied.remove(&hwnd);
        if self.monitors.get(mi).unwrap().workspaces.focused_idx() != wi {
            self.invalidate_cache(mi, wi);
        }
        Some(mi)
    }

    // ----- layout -----------------------------------------------------------

    pub fn retile_all(&mut self) {
        for mi in 0..self.monitors.len() {
            self.retile(mi);
        }
    }

    /// Lays out the active workspace of monitor `mi`: positions and shows
    /// its visible windows, hides background stack members.
    fn retile(&mut self, mi: usize) {
        // Safety net for windows whose destruction we missed: a dead
        // handle would keep an empty tile.
        let dead: Vec<Hwnd> = self.ws(mi).windows().filter(|h| !h.exists()).collect();
        for h in dead {
            self.unmanage(h);
        }
        let area = self.layout_area(mi);
        let params = self.layout_params(self.ws(mi));
        let ws = self.ws_mut(mi);
        let n = ws.containers.len();
        let rects = if ws.monocle {
            vec![area; n]
        } else {
            minato_layout::calculate(ws.layout, area, n, &params, &ws.overrides)
        };
        ws.last_rects = rects.clone();

        let visible: HashSet<Hwnd> = ws.visible_windows().into_iter().collect();
        let mut place = Vec::new();
        let mut hide = Vec::new();
        for (i, c) in ws.containers.iter().enumerate() {
            for h in c.windows.iter() {
                if visible.contains(h) {
                    place.push((*h, rects[i]));
                } else {
                    hide.push(*h);
                }
            }
        }
        let floating = ws.floating.clone();

        // Only animate when something on screen will actually change;
        // retile runs on many events that turn out to be no-ops.
        let changes = place
            .iter()
            .any(|(h, r)| self.applied.get(h) != Some(r) || self.hidden.contains(h))
            || hide.iter().any(|h| !self.hidden.contains(h))
            || floating.iter().any(|h| self.hidden.contains(h));
        if changes {
            self.freeze(mi, Freeze::Layout);
        }

        for (h, r) in place {
            self.show(h);
            perf::time(perf::Op::SetPosition, || h.set_position(r));
            self.applied.insert(h, r);
        }
        for h in floating {
            self.show(h);
        }
        for h in hide {
            self.hide(h);
        }
    }

    // ----- events -----------------------------------------------------------

    fn on_event(&mut self, ev: WinEvent, hwnd: Hwnd) {
        match ev {
            WinEvent::Show | WinEvent::Uncloak | WinEvent::MinimizeEnd | WinEvent::NameChange => {
                if self.hidden.contains(&hwnd) {
                    return;
                }
                if let Some(mi) = self.manage(hwnd) {
                    self.retile(mi);
                }
            }
            WinEvent::Hide => {
                // Events are asynchronous: only trust them if the window is
                // still hidden now and we didn't hide it ourselves.
                if !self.hidden.contains(&hwnd) && !hwnd.is_visible() {
                    if let Some(mi) = self.unmanage(hwnd) {
                        self.retile(mi);
                    }
                }
            }
            WinEvent::MinimizeStart => {
                if !self.hidden.contains(&hwnd) && hwnd.is_minimized() {
                    if let Some(mi) = self.unmanage(hwnd) {
                        self.retile(mi);
                    }
                }
            }
            WinEvent::Destroy => {
                // The event can arrive while the handle is still valid. A
                // window being destroyed is no longer visible, though; and
                // one we hid (background workspace) never sends the Hide
                // that would otherwise have unmanaged it.
                if !hwnd.exists() || !hwnd.is_visible() {
                    if let Some(mi) = self.unmanage(hwnd) {
                        self.retile(mi);
                    }
                }
            }
            WinEvent::Foreground => {
                self.refresh_monitors();
                if self.locate(hwnd).is_none() {
                    if let Some(mi) = self.manage(hwnd) {
                        self.retile(mi);
                    }
                }
                self.sync_focus(hwnd);
            }
            WinEvent::MoveSizeEnd => self.on_move_end(hwnd),
            WinEvent::MoveSizeStart | WinEvent::Cloak => {}
        }
    }

    /// Makes our focus state follow the real foreground window.
    fn sync_focus(&mut self, hwnd: Hwnd) {
        let Some((mi, wi, _)) = self.locate(hwnd) else {
            return;
        };
        self.monitors.focus(mi);
        if self.monitors.get(mi).unwrap().workspaces.focused_idx() != wi {
            // Something activated a window on a background workspace
            // (e.g. a notification click): switch to it.
            self.switch_workspace(mi, wi);
        }
        let ws = self.ws_mut(mi);
        let before = ws.focused_tiled_window();
        ws.focus_window(hwnd);
        let changed_stack = before != ws.focused_tiled_window()
            && matches!(ws.find(hwnd), Some(Slot::Tiled { window, .. }) if window > 0 || ws.monocle);
        if changed_stack {
            self.retile(mi);
        }
    }

    /// A window was dragged: drop it on another tile to swap, or on another
    /// monitor to move it there.
    fn on_move_end(&mut self, hwnd: Hwnd) {
        let Some((mi, wi, slot)) = self.locate(hwnd) else {
            return;
        };
        if self.monitors.get(mi).unwrap().workspaces.focused_idx() != wi {
            return;
        }
        let (x, y) = win32::cursor_pos();
        let target = self
            .monitor_index(win32::monitor_at_point(x, y))
            .unwrap_or(mi);

        match slot {
            Slot::Floating(i) => {
                if target != mi {
                    self.ws_mut(mi).floating.remove(i);
                    self.ws_mut(target).floating.push(hwnd);
                }
            }
            Slot::Tiled { container, .. } => {
                if target != mi {
                    let c = self.ws_mut(mi).containers.remove(container).unwrap();
                    let tws = self.ws_mut(target);
                    let at = tws
                        .last_rects
                        .iter()
                        .position(|r| r.contains_point(x, y))
                        .unwrap_or(tws.containers.len());
                    tws.containers.insert(at, c);
                    tws.containers.focus(at);
                    self.monitors.focus(target);
                    self.retile(target);
                } else if self.apply_drag_resize(mi, container, hwnd) {
                    // Edges were dragged: the neighbours follow (retile below).
                } else {
                    let ws = self.ws_mut(mi);
                    if let Some(j) = ws.last_rects.iter().position(|r| r.contains_point(x, y)) {
                        if j != container {
                            ws.containers.swap(container, j);
                            ws.containers.focus(j);
                        }
                    }
                }
                self.retile(mi);
            }
        }
    }

    /// If the user resized tiled window `hwnd` (container `container` of
    /// monitor `mi`) with the mouse, turns every edge it moved into a split
    /// ratio change, so the neighbouring tiles grow or shrink to match.
    /// Returns `false` when the size didn't change (a plain move).
    fn apply_drag_resize(&mut self, mi: usize, container: usize, hwnd: Hwnd) -> bool {
        let Some(new) = hwnd.frame_rect() else {
            return false;
        };
        let Some(old) = self
            .applied
            .get(&hwnd)
            .copied()
            .or_else(|| self.ws(mi).last_rects.get(container).copied())
        else {
            return false;
        };
        let edges = drag_edges(old, new);
        if edges.is_empty() {
            return false;
        }
        let area = self.layout_area(mi);
        let params = self.layout_params(self.ws(mi));
        let ws = self.ws_mut(mi);
        if ws.monocle || container >= ws.containers.len() {
            // Nothing to share space with: snap back.
            return true;
        }
        for (dir, delta) in edges {
            // Recompute between edges: an outer split changes the extent
            // the inner ones are measured against.
            let arr = Arrangement::new(ws.layout, area, ws.containers.len(), &params, &ws.overrides);
            arr.resize(container, dir, Sizing::Increase, delta, &mut ws.overrides);
        }
        true
    }

    /// Adds monitors that appeared, folds removed ones into the first
    /// monitor, and refreshes work areas (taskbar moves, resolution changes).
    fn refresh_monitors(&mut self) {
        let current = win32::monitors();
        let known: Vec<&MonitorInfo> = self.monitors.iter().map(|m| &m.info).collect();
        if current.len() == known.len() && current.iter().zip(&known).all(|(a, b)| a == *b) {
            return;
        }
        log::info!("monitor configuration changed: {} monitor(s)", current.len());
        // Cached pictures and pending freezes refer to the old geometry.
        self.anim.clear_cache();
        self.frozen.clear();

        let mut old: Vec<Monitor> = Vec::new();
        while let Some(m) = self.monitors.remove(0) {
            old.push(m);
        }
        let mut next = Vec::new();
        for (i, info) in current.into_iter().enumerate() {
            let existing = old
                .iter()
                .position(|m| m.info.device == info.device || m.info.handle == info.handle);
            let workspaces = match existing {
                Some(p) => old.remove(p).workspaces,
                None => make_workspaces(&self.cfg, i),
            };
            next.push(Monitor { info, workspaces });
        }
        if next.is_empty() {
            // No monitors (e.g. mid-reconnect): keep the old state.
            self.monitors = Ring::from_vec(old);
            return;
        }
        // Move windows of disconnected monitors to the first one.
        for orphan in old {
            for (wi, ws) in orphan.workspaces.iter().enumerate() {
                let target = &mut next[0].workspaces;
                while target.len() <= wi {
                    let n = target.len() + 1;
                    target.push(Workspace::new(n.to_string(), self.cfg.default_layout, Default::default()));
                }
                let t = target.get_mut(wi).unwrap();
                for c in ws.containers.iter() {
                    t.containers.push(c.clone());
                }
                t.floating.extend(&ws.floating);
            }
        }
        self.monitors = Ring::from_vec(next);
        // Windows that moved onto a background workspace must be hidden,
        // ones on the active workspace shown.
        for mi in 0..self.monitors.len() {
            let m = self.monitors.get(mi).unwrap();
            let active = m.workspaces.focused_idx();
            let bg: Vec<Hwnd> = m
                .workspaces
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != active)
                .flat_map(|(_, w)| w.windows().collect::<Vec<_>>())
                .collect();
            for h in bg {
                self.hide(h);
            }
        }
        self.retile_all();
    }

    // ----- workspaces -------------------------------------------------------

    /// Shows workspace `wi` on monitor `mi` and hides the previous one.
    fn switch_workspace(&mut self, mi: usize, wi: usize) {
        if !self.ensure_workspace(mi, wi) {
            return;
        }
        let from = self.monitors.get(mi).unwrap().workspaces.focused_idx();
        if from == wi {
            return;
        }
        self.freeze(mi, Freeze::Workspace { from, to: wi });
        let m = self.monitors.get_mut(mi).unwrap();
        let old: Vec<Hwnd> = m.workspace().windows().collect();
        m.workspaces.focus(wi);
        self.retile(mi);
        for h in old {
            self.hide(h);
        }
    }

    fn focus_workspace(&mut self, mi: usize, wi: usize) {
        self.switch_workspace(mi, wi);
        self.monitors.focus(mi);
        self.focus_active(mi);
        perf::time(perf::Op::Focus, win32::nudge_cursor);
    }

    /// Focuses the focused window of monitor `mi`'s active workspace, or the
    /// desktop if it is empty.
    fn focus_active(&mut self, mi: usize) {
        let ws = self.ws(mi);
        let target = ws.focused_tiled_window().or_else(|| ws.floating.first().copied());
        match target {
            Some(h) => self.focus_window(h),
            None => {
                if let Some(d) = Hwnd::desktop() {
                    d.focus();
                }
                if self.cfg.mouse_follows_focus {
                    let (x, y) = self.monitors.get(mi).unwrap().info.work_area.center();
                    win32::set_cursor_pos(x, y);
                }
            }
        }
    }

    /// Takes `hwnd` out of monitor `mi`'s active workspace and puts it on
    /// workspace `wi` of monitor `to`, keeping it tiled or floating.
    fn transfer(&mut self, hwnd: Hwnd, mi: usize, to: usize, wi: usize) {
        if !self.ensure_workspace(to, wi) {
            return;
        }
        let Some(slot) = self.ws_mut(mi).remove(hwnd) else {
            return;
        };
        let target_active = self.monitors.get(to).unwrap().workspaces.focused_idx();
        let ws = self.monitors.get_mut(to).unwrap().workspaces.get_mut(wi).unwrap();
        match slot {
            Slot::Floating(_) => ws.floating.push(hwnd),
            Slot::Tiled { .. } => ws.add_tiled(hwnd),
        }
        if wi != target_active {
            self.hide(hwnd);
            self.invalidate_cache(to, wi);
        }
        self.retile(mi);
        if to != mi {
            self.retile(to);
        }
    }

    // ----- actions ----------------------------------------------------------

    fn on_action(&mut self, action: Action) {
        log::debug!("action {action:?}");
        match action {
            Action::Focus { dir } => self.focus_direction(dir),
            Action::Move { dir } => self.move_direction(dir),
            Action::Stack { dir } => {
                let mi = self.focused_monitor();
                let ws = self.ws(mi);
                let i = ws.containers.focused_idx();
                if let Some(j) = neighbor(&ws.last_rects, i, dir) {
                    self.ws_mut(mi).stack_into(i, j);
                    self.retile(mi);
                }
            }
            Action::Unstack => {
                let mi = self.focused_monitor();
                if self.ws_mut(mi).unstack_focused() {
                    self.retile(mi);
                }
            }
            Action::CycleStack { dir } => {
                let mi = self.focused_monitor();
                let ws = self.ws_mut(mi);
                let Some(c) = ws.containers.focused_mut() else {
                    return;
                };
                let n = c.windows.len();
                if n < 2 {
                    return;
                }
                let i = c.windows.focused_idx();
                let next = match dir {
                    CycleDirection::Next => (i + 1) % n,
                    CycleDirection::Previous => (i + n - 1) % n,
                };
                c.windows.focus(next);
                let h = c.focused_window();
                self.retile(mi);
                if let Some(h) = h {
                    self.focus_window(h);
                }
            }
            Action::Promote => {
                let mi = self.focused_monitor();
                let ws = self.ws_mut(mi);
                let i = ws.containers.focused_idx();
                let other = if i == 0 { 1 } else { i };
                if ws.containers.len() > 1 {
                    ws.containers.swap(0, other);
                    ws.containers.focus(0);
                    self.retile(mi);
                }
            }
            Action::Resize { dir, sizing } => {
                let mi = self.focused_monitor();
                let area = self.layout_area(mi);
                let params = self.layout_params(self.ws(mi));
                let delta = self.cfg.resize_delta;
                let ws = self.ws_mut(mi);
                if ws.monocle || ws.containers.is_empty() {
                    return;
                }
                let arr = Arrangement::new(ws.layout, area, ws.containers.len(), &params, &ws.overrides);
                let i = ws.containers.focused_idx();
                if arr.resize(i, dir, sizing, delta, &mut ws.overrides) {
                    self.retile(mi);
                }
            }
            Action::ToggleFloat => self.toggle_float(),
            Action::ToggleMonocle => {
                let mi = self.focused_monitor();
                let ws = self.ws_mut(mi);
                ws.monocle = !ws.monocle;
                self.retile(mi);
            }
            Action::ChangeLayout { layout } => self.set_layout(|_| layout),
            Action::CycleLayout { dir } => self.set_layout(|l| match dir {
                CycleDirection::Next => l.next(),
                CycleDirection::Previous => l.previous(),
            }),
            Action::FlipLayout { axis } => {
                let mi = self.focused_monitor();
                let ws = self.ws_mut(mi);
                match axis {
                    FlipAxis::Horizontal => ws.flip.horizontal = !ws.flip.horizontal,
                    FlipAxis::Vertical => ws.flip.vertical = !ws.flip.vertical,
                }
                self.retile(mi);
            }
            Action::FocusWorkspace { index } => {
                let mi = self.focused_monitor();
                self.focus_workspace(mi, index);
            }
            Action::MoveToWorkspace { index } | Action::SendToWorkspace { index } => {
                let Some((mi, _, hwnd)) = self.current() else {
                    return;
                };
                if self.monitors.get(mi).unwrap().workspaces.focused_idx() == index {
                    return;
                }
                self.transfer(hwnd, mi, mi, index);
                if matches!(action, Action::MoveToWorkspace { .. }) {
                    self.switch_workspace(mi, index);
                    self.ws_mut(mi).focus_window(hwnd);
                    self.focus_window(hwnd);
                } else {
                    self.focus_active(mi);
                }
            }
            Action::FocusMonitor { index } => {
                if index < self.monitors.len() {
                    self.monitors.focus(index);
                    self.focus_active(index);
                }
            }
            Action::MoveToMonitor { index } => {
                let Some((mi, _, hwnd)) = self.current() else {
                    return;
                };
                if index < self.monitors.len() && index != mi {
                    let wi = self.monitors.get(index).unwrap().workspaces.focused_idx();
                    self.transfer(hwnd, mi, index, wi);
                    self.monitors.focus(index);
                    self.ws_mut(index).focus_window(hwnd);
                    self.focus_window(hwnd);
                }
            }
            Action::Retile => {
                self.refresh_monitors();
                self.retile_all();
            }
            Action::Close => {
                if let Some(fg) = Hwnd::foreground() {
                    // Freeze with the window still visible, so that once it
                    // is gone the picture fades it out.
                    if let Some((mi, _, _)) = self.locate(fg) {
                        self.freeze(mi, Freeze::Layout);
                        self.closing = Some((mi, fg, Instant::now()));
                    }
                    fg.close();
                }
            }
            Action::TogglePause => {
                self.paused = !self.paused;
                hotkey::set_paused(self.paused);
                log::info!("paused: {}", self.paused);
                if !self.paused {
                    self.reconcile();
                }
            }
            Action::OpenSettings => (self.ui)(UiRequest::OpenSettings),
            Action::Quit => self.quit = true,
        }
    }

    fn set_layout(&mut self, f: impl FnOnce(LayoutKind) -> LayoutKind) {
        let mi = self.focused_monitor();
        let ws = self.ws_mut(mi);
        let next = f(ws.layout);
        if next != ws.layout {
            ws.layout = next;
            ws.overrides.clear();
        }
        ws.monocle = false;
        self.retile(mi);
    }

    fn focus_direction(&mut self, dir: Direction) {
        let mi = self.focused_monitor();
        let ws = self.ws(mi);
        let i = ws.containers.focused_idx();
        let n = ws.containers.len();
        let target = if ws.monocle && n > 1 {
            Some(match dir {
                Direction::Left | Direction::Up => (i + n - 1) % n,
                Direction::Right | Direction::Down => (i + 1) % n,
            })
        } else {
            neighbor(&ws.last_rects, i, dir)
        };
        match target {
            Some(j) => {
                let ws = self.ws_mut(mi);
                ws.containers.focus(j);
                let monocle = ws.monocle;
                let h = ws.focused_tiled_window();
                if monocle {
                    self.retile(mi);
                }
                if let Some(h) = h {
                    self.focus_window(h);
                }
            }
            None => {
                if let Some(mj) = self.monitor_in_direction(mi, dir) {
                    self.monitors.focus(mj);
                    self.focus_active(mj);
                }
            }
        }
    }

    fn move_direction(&mut self, dir: Direction) {
        let mi = self.focused_monitor();
        let ws = self.ws(mi);
        if ws.containers.is_empty() {
            return;
        }
        let i = ws.containers.focused_idx();
        if let Some(j) = neighbor(&ws.last_rects, i, dir).filter(|_| !ws.monocle) {
            let ws = self.ws_mut(mi);
            ws.containers.swap(i, j);
            ws.containers.focus(j);
            self.retile(mi);
            return;
        }
        let Some(mj) = self.monitor_in_direction(mi, dir) else {
            return;
        };
        let c: Container = self.ws_mut(mi).containers.remove(i).unwrap();
        let h = c.focused_window();
        let tws = self.ws_mut(mj);
        // Enter on the side we came from.
        let at = match dir {
            Direction::Right | Direction::Down => 0,
            Direction::Left | Direction::Up => tws.containers.len(),
        };
        tws.containers.insert(at, c);
        tws.containers.focus(at);
        self.monitors.focus(mj);
        self.retile(mi);
        self.retile(mj);
        if let Some(h) = h {
            self.focus_window(h);
        }
    }

    fn monitor_in_direction(&self, mi: usize, dir: Direction) -> Option<usize> {
        let rects: Vec<Rect> = self.monitors.iter().map(|m| m.info.rect).collect();
        neighbor(&rects, mi, dir)
    }

    fn toggle_float(&mut self) {
        let Some((mi, slot, hwnd)) = self.current() else {
            return;
        };
        let area = self.layout_area(mi);
        let ws = self.ws_mut(mi);
        match slot {
            Slot::Tiled { .. } => {
                ws.remove(hwnd);
                ws.floating.push(hwnd);
                // Float centred at two thirds of the work area.
                let (w, h) = (area.w * 2 / 3, area.h * 2 / 3);
                let r = Rect::new(area.x + (area.w - w) / 2, area.y + (area.h - h) / 2, w, h);
                self.retile(mi);
                hwnd.set_position(r);
            }
            Slot::Floating(i) => {
                ws.floating.remove(i);
                ws.add_tiled(hwnd);
                self.retile(mi);
            }
        }
        self.focus_window(hwnd);
    }

    /// Re-syncs with reality after a pause: drop windows that vanished,
    /// adopt ones that appeared.
    fn reconcile(&mut self) {
        let all: Vec<Hwnd> = self
            .monitors
            .iter()
            .flat_map(|m| m.workspaces.iter().flat_map(|w| w.windows().collect::<Vec<_>>()))
            .collect();
        for h in all {
            if !h.exists() || (!self.hidden.contains(&h) && (!h.is_visible() || h.is_minimized())) {
                self.unmanage(h);
            }
        }
        for h in win32::top_level_windows().into_iter().rev() {
            self.manage(h);
        }
        self.refresh_monitors();
        self.retile_all();
    }

    // ----- config -------------------------------------------------------------

    fn apply_keymap(&self, cfg: &Config) -> Vec<String> {
        let (keymap, errors) = Keymap::build(&cfg.hotkeys);
        hotkey::set_keymap(keymap);
        errors
    }

    fn apply_config(&mut self, cfg: Arc<Config>) {
        log::info!("applying configuration");
        let old = std::mem::replace(&mut self.cfg, cfg.clone());
        self.rules = RuleSet::compile_lossy(&cfg.rules);
        self.config_errors = cfg.validate();
        self.config_errors.extend(self.apply_keymap(&cfg));

        for mi in 0..self.monitors.len() {
            let new_mc = cfg.monitor(mi);
            let old_mc = old.monitor(mi);
            let resolve = |c: &Config, w: Option<&minato_config::WorkspaceConfig>| {
                w.map(|w| (w.layout.unwrap_or(c.default_layout), w.flip))
                    .unwrap_or((c.default_layout, Default::default()))
            };
            for (wi, wc) in new_mc.workspaces.iter().enumerate() {
                self.ensure_workspace(mi, wi);
                let new = resolve(&cfg, Some(wc));
                let before = resolve(&old, old_mc.workspaces.get(wi));
                let ws = self.monitors.get_mut(mi).unwrap().workspaces.get_mut(wi).unwrap();
                ws.name = if wc.name.is_empty() { (wi + 1).to_string() } else { wc.name.clone() };
                if new != before {
                    (ws.layout, ws.flip) = new;
                    ws.overrides.clear();
                }
            }
            // Drop trailing workspaces the config no longer lists, as long
            // as they are empty and not on screen.
            let keep = new_mc.workspaces.len().max(1);
            let m = self.monitors.get_mut(mi).unwrap();
            while m.workspaces.len() > keep
                && m.workspaces.focused_idx() < m.workspaces.len() - 1
                && m.workspaces.get(m.workspaces.len() - 1).is_some_and(Workspace::is_empty)
            {
                let len = m.workspaces.len();
                m.workspaces.truncate(len - 1);
            }
        }

        // Re-evaluate rules for managed windows: newly ignored windows are
        // released, float rules move tiles to floating and vice versa.
        let managed: Vec<(usize, usize, Slot, Hwnd)> = self
            .monitors
            .iter()
            .enumerate()
            .flat_map(|(mi, m)| {
                m.workspaces.iter().enumerate().flat_map(move |(wi, w)| {
                    w.windows().map(move |h| (mi, wi, w.find(h).unwrap(), h)).collect::<Vec<_>>()
                })
            })
            .collect();
        let old_rules = RuleSet::compile_lossy(&old.rules);
        for (mi, wi, slot, h) in managed {
            let info = h.info();
            let rule = self.rules.evaluate(&info);
            if rule == Some(RuleAction::Ignore) {
                let was_hidden = self.hidden.contains(&h);
                self.unmanage(h);
                if was_hidden {
                    h.show();
                }
                h.set_border_color(None);
                continue;
            }
            let ws = self.monitors.get_mut(mi).unwrap().workspaces.get_mut(wi).unwrap();
            match slot {
                Slot::Tiled { .. } if rule == Some(RuleAction::Float) => {
                    ws.remove(h);
                    ws.floating.push(h);
                }
                // Only re-tile windows a rule floated; ones floated by hand
                // keep floating.
                Slot::Floating(_)
                    if rule != Some(RuleAction::Float)
                        && old_rules.evaluate(&info) == Some(RuleAction::Float) =>
                {
                    ws.remove(h);
                    ws.add_tiled(h);
                }
                _ => {}
            }
        }

        if !cfg.border.enabled {
            for (h, _) in self.borders.drain() {
                h.set_border_color(None);
            }
        } else {
            // Force colours to be re-applied with the new palette.
            self.borders.clear();
        }
        // Gaps or layouts may have changed: cached pictures are stale.
        self.anim.clear_cache();
        self.retile_all();
    }

    // ----- borders & snapshot -----------------------------------------------

    fn update_borders(&mut self) {
        if !self.cfg.border.enabled {
            return;
        }
        let fg = Hwnd::foreground();
        let b = &self.cfg.border;
        let mut wanted: Vec<(Hwnd, Option<Color>)> = Vec::new();
        for m in self.monitors.iter() {
            let ws = m.workspace();
            for h in ws.visible_windows() {
                let color = if Some(h) == fg {
                    Some(if ws.monocle { b.monocle.unwrap_or(b.focused) } else { b.focused })
                } else {
                    b.unfocused
                };
                wanted.push((h, color));
            }
        }
        let a = &self.cfg.animations;
        let fade = (self.animations_on() && a.border.enabled).then(|| {
            (
                Duration::from_millis(a.border.duration_ms as u64),
                Easing::from_spec(a.curve(&a.border.curve)),
            )
        });
        for (h, color) in wanted {
            match self.borders.get(&h) {
                Some(prev) if *prev == color => continue,
                // A window we coloured before: blend from its old colour.
                Some(prev) if fade.is_some() => {
                    let (duration, curve) = fade.unwrap();
                    self.anim.border(h, *prev, color, duration, curve);
                }
                _ => h.set_border_color(color),
            }
            self.borders.insert(h, color);
        }
    }

    // ----- animations ---------------------------------------------------------

    fn animations_on(&self) -> bool {
        self.ready
            && !self.paused
            && self.cfg.animations.enabled
            && self.anim.available()
            && win32::system_animations_enabled()
    }

    /// Covers monitor `mi` with a picture of what it shows right now, before
    /// anything on it changes. At most once per batch; released in
    /// [`Self::after_batch`].
    fn freeze(&mut self, mi: usize, kind: Freeze) {
        if let Some(existing) = self.frozen.get_mut(&mi) {
            match (*existing, kind) {
                // A second switch in the same batch: the first one may
                // already be sliding (early play), so continue from there
                // like a switch mid-slide.
                (Freeze::Workspace { .. }, Freeze::Workspace { .. }) => {
                    self.frozen.remove(&mi);
                }
                // Already frozen this batch: the picture predates every
                // change. A workspace switch wins over a layout change.
                (_, Freeze::Workspace { .. }) => {
                    *existing = kind;
                    return;
                }
                _ => return,
            }
        }
        if !self.animations_on() {
            return;
        }
        let a = &self.cfg.animations;
        let wanted = match kind {
            Freeze::Layout => a.layout.enabled,
            Freeze::Workspace { .. } => a.workspace.enabled,
        };
        if !wanted {
            return;
        }
        let info = self.monitors.get(mi).unwrap().info.clone();
        if fullscreen_on(&info) {
            return;
        }
        if kind == Freeze::Layout {
            // A layout fade is still running here (e.g. a held resize key):
            // don't re-freeze, let the running fade reveal the newest state.
            let busy = ms_u64(a.layout.hold_ms + a.layout.duration_ms);
            if self.last_layout_freeze.get(&mi).is_some_and(|t| t.elapsed() < busy) {
                return;
            }
            self.last_layout_freeze.insert(mi, Instant::now());
        }
        // Leaving a workspace: keep its picture for sliding back in later.
        let cache_as = match kind {
            Freeze::Workspace { from, .. } => Some((info.handle, from)),
            Freeze::Layout => None,
        };
        let area = usable_area(&self.cfg, &info);
        let waited = Instant::now();
        let frozen = self.anim.freeze(info.handle, area, cache_as);
        perf::freeze_waited(waited.elapsed());
        if frozen {
            self.frozen.insert(mi, kind);
            // A slide to a workspace whose picture is cached can start
            // now: until it ends only pictures are visible, so the window
            // shuffling below doesn't have to finish first.
            if let Some(play) = self.play_for(kind, info.handle, true) {
                self.anim.play(info.handle, play);
            }
        }
    }

    /// Starts the animations for every monitor frozen during this batch.
    fn release_frozen(&mut self) {
        // Keep a close freeze until the window is actually gone (bounded),
        // otherwise the fade would reveal it still sitting there.
        let waiting = match self.closing {
            Some((mi, h, since)) if h.exists() && since.elapsed() < CLOSE_WAIT => Some(mi),
            _ => {
                self.closing = None;
                None
            }
        };
        let frozen: Vec<(usize, Freeze)> = self.frozen.iter().map(|(k, v)| (*k, *v)).collect();
        for (mi, kind) in frozen {
            if Some(mi) == waiting {
                continue;
            }
            self.frozen.remove(&mi);
            let Some(m) = self.monitors.get(mi) else {
                continue;
            };
            let key = m.info.handle;
            if let Some(play) = self.play_for(kind, key, false) {
                self.anim.play(key, play);
            }
        }
    }

    /// The animation that releases a freeze of `kind` on monitor `key`.
    /// `early`: for before the windows have moved, which only a slide can
    /// start with (see `Play::Slide::only_if_cached`).
    fn play_for(&self, kind: Freeze, key: isize, early: bool) -> Option<Play> {
        let a = &self.cfg.animations;
        let ms = |v: u32| Duration::from_millis(v as u64);
        let play = match kind {
            Freeze::Layout => Play::Fade {
                hold: ms(a.layout.hold_ms),
                duration: ms(a.layout.duration_ms),
                curve: Easing::from_spec(a.curve(&a.layout.curve)),
            },
            Freeze::Workspace { from, to } => {
                let curve = Easing::from_spec(a.curve(&a.workspace.curve));
                let duration = ms(a.workspace.duration_ms);
                let sign = if to > from { 1 } else { -1 };
                match a.workspace.style {
                    WorkspaceStyle::Fade => Play::Fade {
                        hold: ms(a.layout.hold_ms),
                        duration,
                        curve,
                    },
                    style => Play::Slide {
                        incoming: Some((key, to)),
                        dir: if style == WorkspaceStyle::SlideVertical { (0, sign) } else { (sign, 0) },
                        duration,
                        curve,
                        only_if_cached: early,
                    },
                }
            }
        };
        (!early || matches!(play, Play::Slide { .. })).then_some(play)
    }

    /// Forgets the picture of a background workspace whose contents changed.
    fn invalidate_cache(&mut self, mi: usize, wi: usize) {
        if let Some(m) = self.monitors.get(mi) {
            self.anim.invalidate((m.info.handle, wi));
        }
    }

    /// Publishes the state for the UI, unless nothing changed: every new
    /// snapshot makes the status bars redraw.
    fn publish_snapshot(&mut self) {
        let fg = Hwnd::foreground();
        let mut identities = std::mem::take(&mut self.identities);
        let mut seen = HashSet::new();
        let mut identity = |h: Hwnd| {
            seen.insert(h);
            identities
                .entry(h)
                .or_insert_with(|| {
                    let info = h.info();
                    (info.exe, info.class)
                })
                .clone()
        };
        let monitors: Vec<MonitorSnapshot> = self
            .monitors
            .iter()
            .enumerate()
            .map(|(mi, m)| MonitorSnapshot {
                handle: m.info.handle,
                device: m.info.device.clone(),
                rect: m.info.rect,
                work_area: m.info.work_area,
                dpi: m.info.dpi,
                bar: self.cfg.bar.enabled.then(|| {
                    let wa = m.info.work_area;
                    Rect::new(wa.x, wa.y, wa.w, bar_height(&self.cfg, &m.info))
                }),
                fullscreen: fullscreen_on(&m.info),
                focused: mi == self.focused_monitor(),
                active_workspace: m.workspaces.focused_idx(),
                workspaces: m
                    .workspaces
                    .iter()
                    .map(|w| WorkspaceSnapshot {
                        name: w.name.clone(),
                        layout: w.layout,
                        monocle: w.monocle,
                        windows: w
                            .windows()
                            .map(|h| {
                                let (exe, class) = identity(h);
                                WindowSnapshot {
                                    hwnd: h.0,
                                    title: h.title(),
                                    exe,
                                    class,
                                    floating: matches!(w.find(h), Some(Slot::Floating(_))),
                                    focused: Some(h) == fg,
                                }
                            })
                            .collect(),
                    })
                    .collect(),
            })
            .collect();
        let foreground = fg.map(|h| ForegroundSnapshot {
            monitor: h.monitor(),
            title: h.title(),
            exe: identity(h).0,
        });
        identities.retain(|h, _| seen.contains(h));
        crate::anim::set_warm_up_allowed(!monitors.iter().any(|m| m.fullscreen));
        self.identities = identities;
        let snapshot = Snapshot {
            paused: self.paused,
            config_errors: self.config_errors.clone(),
            monitors,
            foreground,
        };
        if **self.snapshot.load() != snapshot {
            self.snapshot.store(Arc::new(snapshot));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_edges_detects_resized_sides() {
        let old = Rect::new(0, 0, 1000, 800);
        // Moved, not resized.
        assert!(drag_edges(old, Rect::new(50, 40, 1000, 800)).is_empty());
        // Rounding noise.
        assert!(drag_edges(old, Rect::new(1, 0, 999, 801)).is_empty());
        // Right edge dragged out, bottom edge dragged in.
        assert_eq!(
            drag_edges(old, Rect::new(0, 0, 1200, 700)),
            vec![(Direction::Right, 200), (Direction::Down, -100)]
        );
        // Left edge dragged out.
        assert_eq!(drag_edges(old, Rect::new(-150, 0, 1150, 800)), vec![(Direction::Left, 150)]);
    }
}
