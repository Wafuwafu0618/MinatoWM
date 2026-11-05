//! The status bar: one always-on-top, non-activating window per monitor,
//! placed where the WM reserves space (`MonitorSnapshot::bar`).
//!
//! Like the settings window, the bar only reads the published [`Snapshot`]
//! and config, and talks to the WM through [`WmMsg`]s.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::{h_flex, ActiveTheme};
use gpui_kit::prelude::*;
use gpui_kit::*;
use minato_config::{BarModule, Config};
use minato_layout::Rect;
use minato_wm::win32::sys::{self, CpuTimes, LocalTime};
use minato_wm::win32::Hwnd;
use minato_wm::{MonitorSnapshot, Snapshot, WmMsg};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::{to_hsla, UiContext};

const POLL: Duration = Duration::from_millis(100);
const STATS_EVERY: Duration = Duration::from_secs(2);

/// Where a bar window sits; a change means it is closed and reopened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Placement {
    monitor: isize,
    rect: Rect,
    dpi: u32,
}

impl Placement {
    fn of(m: &MonitorSnapshot) -> Option<Self> {
        m.bar.filter(|r| r.w > 0 && r.h > 0).map(|rect| Self {
            monitor: m.handle,
            rect,
            dpi: m.dpi,
        })
    }
}

/// The open bar windows. Call [`Bars::sync`] periodically on the main
/// thread; it opens, moves and closes bars to match the WM's monitors.
#[derive(Default)]
pub struct Bars {
    open: Vec<(Placement, WindowHandle<BarView>)>,
    seen: Option<Arc<Snapshot>>,
}

impl Bars {
    pub fn sync(&mut self, ui: &UiContext, cx: &mut App) {
        let snap = ui.snapshot.load_full();
        if self.seen.as_ref().is_some_and(|s| Arc::ptr_eq(s, &snap)) {
            return;
        }
        self.seen = Some(snap.clone());
        // Before the WM has published anything there are no monitors yet.
        if snap.monitors.is_empty() {
            return;
        }
        let wanted: Vec<Placement> = snap.monitors.iter().filter_map(Placement::of).collect();
        let current: Vec<Placement> = self.open.iter().map(|(p, _)| *p).collect();
        if wanted == current {
            return;
        }
        self.close_all(cx);
        for p in wanted {
            match open_bar(ui.clone(), p, cx) {
                Ok(h) => self.open.push((p, h)),
                Err(e) => log::error!("opening status bar: {e:#}"),
            }
        }
    }

    pub fn close_all(&mut self, cx: &mut App) {
        for (_, h) in self.open.drain(..) {
            let _ = h.update(cx, |_, window, _| window.remove_window());
        }
    }
}

fn open_bar(ui: UiContext, p: Placement, cx: &mut App) -> anyhow::Result<WindowHandle<BarView>> {
    // GPUI positions in logical pixels of the target display; the exact
    // physical rect is applied right after creation.
    let scale = p.dpi as f32 / 96.0;
    let r = p.rect;
    let bounds = Bounds {
        origin: point(px(r.x as f32 / scale), px(r.y as f32 / scale)),
        size: size(px(r.w as f32 / scale), px(r.h as f32 / scale)),
    };
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        display_id: Some(DisplayId::new(p.monitor as u64)),
        ..Default::default()
    };
    let handle = cx.open_window(options, |window, cx| cx.new(|cx| BarView::new(ui, p.monitor, window, cx)))?;
    handle.update(cx, |view, window, _| {
        if let Some(hwnd) = hwnd_of(window) {
            hwnd.set_bar_style();
            hwnd.set_position(r);
            view.hwnd = Some(hwnd);
        }
    })?;
    Ok(handle)
}

fn hwnd_of(window: &Window) -> Option<Hwnd> {
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(Hwnd(h.hwnd.get())),
        _ => None,
    }
}

pub struct BarView {
    ctx: UiContext,
    monitor: isize,
    hwnd: Option<Hwnd>,
    hidden: bool,
    snapshot: Arc<Snapshot>,
    cfg: Arc<Config>,
    time: LocalTime,
    cpu_sample: CpuTimes,
    cpu: f32,
    mem: (u64, u64),
    stats_at: Instant,
    _poll: Task<()>,
}

impl BarView {
    fn new(ctx: UiContext, monitor: isize, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(POLL).await;
            if this.update(cx, |this, cx| this.poll(cx)).is_err() {
                break;
            }
        });
        Self {
            snapshot: ctx.snapshot.load_full(),
            cfg: ctx.latest_config.load_full(),
            ctx,
            monitor,
            hwnd: None,
            hidden: false,
            time: sys::local_time(),
            cpu_sample: CpuTimes::now(),
            cpu: 0.0,
            mem: sys::memory(),
            stats_at: Instant::now(),
            _poll: poll,
        }
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        let mut dirty = false;

        let snap = self.ctx.snapshot.load_full();
        if !Arc::ptr_eq(&snap, &self.snapshot) {
            self.snapshot = snap;
            dirty = true;
        }
        // Maximizing doesn't reach the WM (it skips location-change events),
        // so this is checked on every tick rather than per snapshot.
        let hide = self.monitor_snapshot().is_some_and(|m| m.fullscreen) || self.maximized_here();
        if hide != self.hidden {
            self.hidden = hide;
            if let Some(h) = self.hwnd {
                if hide {
                    h.hide();
                } else {
                    h.show();
                    dirty = true;
                }
            }
        }
        let cfg = self.ctx.latest_config.load_full();
        if !Arc::ptr_eq(&cfg, &self.cfg) {
            self.cfg = cfg;
            dirty = true;
        }
        let time = sys::local_time();
        if time != self.time {
            self.time = time;
            dirty = true;
        }
        if self.stats_at.elapsed() >= STATS_EVERY {
            let now = CpuTimes::now();
            self.cpu = now.usage_since(&self.cpu_sample);
            self.cpu_sample = now;
            self.mem = sys::memory();
            self.stats_at = Instant::now();
            dirty = true;
        }
        if dirty && !self.hidden {
            cx.notify();
        }
    }

    /// A maximized window on this monitor would have its top edge under
    /// the bar: the foreground window, or any window of the shown workspace.
    fn maximized_here(&self) -> bool {
        let shown = |h: Hwnd| h.is_maximized() && h.is_visible() && !h.is_cloaked() && h.monitor() == self.monitor;
        if Hwnd::foreground().is_some_and(shown) {
            return true;
        }
        self.monitor_snapshot()
            .and_then(|m| m.workspaces.get(m.active_workspace))
            .is_some_and(|ws| ws.windows.iter().any(|w| shown(Hwnd(w.hwnd))))
    }

    fn monitor_index(&self) -> Option<usize> {
        self.snapshot.monitors.iter().position(|m| m.handle == self.monitor)
    }

    fn monitor_snapshot(&self) -> Option<&MonitorSnapshot> {
        self.monitor_index().map(|i| &self.snapshot.monitors[i])
    }

    fn section(&self, modules: &[BarModule], cx: &mut Context<Self>) -> Div {
        let mut row = h_flex().gap_3().items_center().min_w_0();
        for m in modules {
            row = row.child(self.module(m, cx));
        }
        row
    }

    fn module(&self, module: &BarModule, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let text = |s: String| div().whitespace_nowrap().child(s).into_any_element();
        match module {
            BarModule::Workspaces => self.workspaces(cx).into_any_element(),
            BarModule::Layout => {
                let Some(m) = self.monitor_snapshot() else {
                    return div().into_any_element();
                };
                let Some(ws) = m.workspaces.get(m.active_workspace) else {
                    return div().into_any_element();
                };
                let label = if ws.monocle {
                    format!("{} [M]", ws.layout.label())
                } else {
                    ws.layout.label().to_string()
                };
                div().whitespace_nowrap().text_color(muted).child(label).into_any_element()
            }
            BarModule::Title => {
                let title = self
                    .snapshot
                    .foreground
                    .as_ref()
                    .filter(|f| f.monitor == self.monitor)
                    .map(|f| f.title.clone())
                    .unwrap_or_default();
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(title)
                    .into_any_element()
            }
            BarModule::Clock { format } => {
                text(format_time(format.as_deref().unwrap_or(BarModule::DEFAULT_CLOCK), &self.time))
            }
            BarModule::Cpu => text(format!("CPU {:>3.0}%", self.cpu * 100.0)),
            BarModule::Memory => {
                let (used, total) = self.mem;
                let gib = |b: u64| b as f64 / (1u64 << 30) as f64;
                text(format!("MEM {:.1}/{:.0}G", gib(used), gib(total)))
            }
            BarModule::Text { text: t } => text(t.clone()),
        }
    }

    fn workspaces(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, muted) = (theme.foreground, theme.muted_foreground);
        let accent = to_hsla(self.cfg.border.focused);
        let on_accent = theme.background;
        let mut row = h_flex().gap_1().items_center();
        let (Some(mi), Some(m)) = (self.monitor_index(), self.monitor_snapshot()) else {
            return row;
        };
        for (wi, ws) in m.workspaces.iter().enumerate() {
            let active = wi == m.active_workspace;
            let empty = ws.windows.is_empty();
            let wm = self.ctx.wm.clone();
            row = row.child(
                div()
                    .id(("ws", wi))
                    .px_2()
                    .rounded_md()
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .when(active, |d| d.bg(accent).text_color(on_accent))
                    .when(!active, |d| d.text_color(if empty { muted } else { fg }))
                    .when(!active, |d| d.hover(|s| s.bg(muted.opacity(0.2))))
                    .child(ws.name.clone())
                    .on_click(move |_, _, _| {
                        let _ = wm.send(WmMsg::FocusWorkspaceOn {
                            monitor: mi,
                            workspace: wi,
                        });
                    }),
            );
        }
        row
    }
}

impl Render for BarView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bar = self.cfg.bar.clone();
        let theme = cx.theme();
        let (bg, fg, warn) = (theme.background, theme.foreground, theme.danger);
        let paused = self.snapshot.paused;
        h_flex()
            .size_full()
            .px_3()
            .gap_3()
            .items_center()
            .bg(bg)
            .text_color(fg)
            .text_sm()
            .child(
                self.section(&bar.left, cx)
                    .flex_1()
                    .flex_basis(px(0.))
                    .when(paused, |d| d.child(div().text_color(warn).child("一時停止中"))),
            )
            .child(self.section(&bar.center, cx).flex_initial().justify_center().max_w(relative(0.5)))
            .child(self.section(&bar.right, cx).flex_1().flex_basis(px(0.)).justify_end())
    }
}

/// strftime subset: `%Y %m %d %H %M %S %a %%`. `%a` is the Japanese
/// weekday (日 月 火 ...).
pub fn format_time(fmt: &str, t: &LocalTime) -> String {
    const WEEKDAYS: [&str; 7] = ["日", "月", "火", "水", "木", "金", "土"];
    let mut out = String::new();
    let mut chars = fmt.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('Y') => out.push_str(&t.year.to_string()),
            Some('m') => out.push_str(&format!("{:02}", t.month)),
            Some('d') => out.push_str(&format!("{:02}", t.day)),
            Some('H') => out.push_str(&format!("{:02}", t.hour)),
            Some('M') => out.push_str(&format!("{:02}", t.minute)),
            Some('S') => out.push_str(&format!("{:02}", t.second)),
            Some('a') => out.push_str(WEEKDAYS.get(t.weekday as usize).copied().unwrap_or("?")),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[std::prelude::v1::test] // `gpui_kit::*` brings its own `test` macro
    fn formats_time() {
        let t = LocalTime {
            year: 2026,
            month: 11,
            day: 5,
            weekday: 4,
            hour: 9,
            minute: 3,
            second: 7,
        };
        assert_eq!(format_time(BarModule::DEFAULT_CLOCK, &t), "11/05 (木) 09:03");
        assert_eq!(format_time("%Y-%m-%d %H:%M:%S %% %q", &t), "2026-11-05 09:03:07 % %q");
    }
}
