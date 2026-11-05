//! The MinatoWM settings window.
//!
//! The view edits a working copy of the [`Config`] and writes it to
//! `config.json` shortly after every change. It never talks to the window
//! manager about configuration directly: the file watcher picks up the
//! write and applies it, exactly as with a hand edit.

pub mod bar;
mod catalog;
mod pages;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use crossbeam_channel::{Receiver, Sender};
use gpui_kit::component::color_picker::{ColorPickerEvent, ColorPickerState};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::select::{SelectEvent, SelectState};
use gpui_kit::component::IndexPath;
use gpui_kit::prelude::*;
use gpui_kit::*;
use minato_config::{Color, Config};
use minato_wm::{hotkey, Snapshot, WmMsg};

pub(crate) type StrSelect = SelectState<Vec<SharedString>>;

/// Everything the settings window needs from the rest of the app.
#[derive(Clone)]
pub struct UiContext {
    pub config_path: PathBuf,
    /// The last config the watcher loaded, to notice hand edits.
    pub latest_config: Arc<ArcSwap<Config>>,
    pub snapshot: Arc<ArcSwap<Snapshot>>,
    pub wm: Sender<WmMsg>,
}

/// Opens the settings window.
pub fn open_settings(ctx: UiContext, cx: &mut App) -> anyhow::Result<AnyWindowHandle> {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(860.), px(680.)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("MinatoWM 設定".into()),
            ..Default::default()
        }),
        window_min_size: Some(size(px(640.), px(420.))),
        ..Default::default()
    };
    let (handle, _) = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| SettingsView::new(ctx, window, cx))
    })?;
    Ok(handle)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordTarget {
    New,
    Row(usize),
}

pub(crate) struct WorkspaceRow {
    pub name: Entity<InputState>,
    pub layout: Entity<StrSelect>,
}

pub(crate) struct Widgets {
    // General
    pub outer_gap: Entity<InputState>,
    pub inner_gap: Entity<InputState>,
    pub resize_delta: Entity<InputState>,
    pub primary_ratio: Entity<InputState>,
    pub default_layout: Entity<StrSelect>,
    // Workspaces
    pub workspaces: Vec<WorkspaceRow>,
    // Hotkeys
    pub new_action: Entity<StrSelect>,
    // Rules
    pub rule_field: Entity<StrSelect>,
    pub rule_kind: Entity<StrSelect>,
    pub rule_pattern: Entity<InputState>,
    pub rule_action: Entity<StrSelect>,
    // Border
    pub border_focused: Entity<ColorPickerState>,
    pub border_unfocused: Entity<ColorPickerState>,
    pub border_monocle: Entity<ColorPickerState>,
    // Animations
    pub anim: AnimWidgets,
    // Status bar
    pub bar_height: Entity<InputState>,
    pub bar_clock: Entity<InputState>,
    /// "Add module" pickers for the left / center / right sections.
    pub bar_add: [Entity<StrSelect>; 3],
}

pub(crate) struct AnimWidgets {
    pub layout_duration: Entity<InputState>,
    pub layout_hold: Entity<InputState>,
    pub layout_curve: Entity<StrSelect>,
    pub ws_duration: Entity<InputState>,
    pub ws_curve: Entity<StrSelect>,
    pub ws_style: Entity<StrSelect>,
    pub border_duration: Entity<InputState>,
    pub border_curve: Entity<StrSelect>,
}

pub(crate) const WS_STYLES: [(&str, minato_config::WorkspaceStyle); 3] = [
    ("横スライド", minato_config::WorkspaceStyle::Slide),
    ("縦スライド", minato_config::WorkspaceStyle::SlideVertical),
    ("フェード", minato_config::WorkspaceStyle::Fade),
];

pub struct SettingsView {
    pub(crate) ctx: UiContext,
    pub(crate) cfg: Config,
    pub(crate) tab: usize,
    /// Which monitor's workspaces the Workspaces page edits.
    pub(crate) ws_monitor: usize,
    pub(crate) snapshot: Arc<Snapshot>,
    seen_config: Arc<Config>,
    save_task: Option<Task<()>>,
    pub(crate) save_error: Option<String>,
    pub(crate) recording: Option<RecordTarget>,
    pub(crate) new_keys: String,
    capture_tx: Sender<String>,
    capture_rx: Receiver<String>,
    pub(crate) w: Option<Widgets>,
    subs: Vec<Subscription>,
    _poll: Task<()>,
}

pub(crate) fn to_hsla(c: Color) -> Hsla {
    rgb(c.to_rgb_u32()).into()
}

fn from_hsla(h: Hsla) -> Color {
    let c: Rgba = h.into();
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color::rgb(byte(c.r), byte(c.g), byte(c.b))
}

impl SettingsView {
    fn new(ctx: UiContext, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let seen_config = ctx.latest_config.load_full();
        let (capture_tx, capture_rx) = crossbeam_channel::unbounded();
        let poll = cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            if this.update_in(cx, |this, window, cx| this.poll(window, cx)).is_err() {
                break;
            }
        });
        let mut view = Self {
            cfg: (*seen_config).clone(),
            snapshot: ctx.snapshot.load_full(),
            ctx,
            tab: 0,
            ws_monitor: 0,
            seen_config,
            save_task: None,
            save_error: None,
            recording: None,
            new_keys: String::new(),
            capture_tx,
            capture_rx,
            w: None,
            subs: Vec::new(),
            _poll: poll,
        };
        view.rebuild(window, cx);
        view
    }

    /// Picks up hotkey captures, WM state changes and external config edits.
    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while let Ok(keys) = self.capture_rx.try_recv() {
            match self.recording.take() {
                Some(RecordTarget::Row(i)) => {
                    if let Some(hk) = self.cfg.hotkeys.get_mut(i) {
                        hk.keys = keys;
                        self.changed(cx);
                    }
                }
                Some(RecordTarget::New) => self.new_keys = keys,
                None => {}
            }
            cx.notify();
        }

        let snap = self.ctx.snapshot.load_full();
        if !Arc::ptr_eq(&snap, &self.snapshot) {
            // Only the header and the workspace / rule / status pages show
            // WM state; skip re-rendering other pages on every focus change.
            let header_changed =
                snap.paused != self.snapshot.paused || snap.config_errors != self.snapshot.config_errors;
            self.snapshot = snap;
            if header_changed || matches!(self.tab, 1 | 3 | 7) {
                cx.notify();
            }
        }

        let latest = self.ctx.latest_config.load_full();
        if !Arc::ptr_eq(&latest, &self.seen_config) {
            self.seen_config = latest.clone();
            // Our own saves come back identical; only adopt real outside edits.
            if *latest != self.cfg && self.save_task.is_none() {
                self.cfg = (*latest).clone();
                self.rebuild(window, cx);
                cx.notify();
            }
        }
    }

    /// Call after every edit of `self.cfg`: saves it after a short pause
    /// so typing doesn't write the file on every keystroke.
    pub(crate) fn changed(&mut self, cx: &mut Context<Self>) {
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(300)).await;
            let _ = this.update(cx, |this, cx| {
                this.save_task = None;
                this.save_error = minato_config::save(&this.ctx.config_path, &this.cfg)
                    .err()
                    .map(|e| format!("{e:#}"));
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn send(&self, action: minato_config::Action) {
        let _ = self.ctx.wm.send(WmMsg::Action(action));
    }

    pub(crate) fn start_recording(&mut self, target: RecordTarget, cx: &mut Context<Self>) {
        if self.recording == Some(target) {
            hotkey::cancel_capture();
            self.recording = None;
        } else {
            let tx = self.capture_tx.clone();
            hotkey::capture_next(move |keys| {
                let _ = tx.send(keys);
            });
            self.recording = Some(target);
        }
        cx.notify();
    }

    // ----- widget construction ------------------------------------------------

    fn input(
        &mut self,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
        apply: impl Fn(&mut Config, &str) -> bool + 'static,
    ) -> Entity<InputState> {
        let state = cx.new(|cx| InputState::new(window, cx).default_value(value));
        self.subs.push(cx.subscribe_in(&state, window, move |this, state, ev: &InputEvent, _w, cx| {
            if matches!(ev, InputEvent::Change) {
                let v = state.read(cx).value().to_string();
                if apply(&mut this.cfg, v.trim()) {
                    this.changed(cx);
                }
            }
        }));
        state
    }

    pub(crate) fn select(
        &mut self,
        options: Vec<SharedString>,
        selected: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_pick: impl Fn(&mut Self, usize, &mut Context<Self>) + 'static,
    ) -> Entity<StrSelect> {
        let items = options.clone();
        let state = cx.new(|cx| SelectState::new(items, Some(IndexPath::new(selected)), window, cx));
        self.subs.push(cx.subscribe_in(
            &state,
            window,
            move |this, _s, ev: &SelectEvent<Vec<SharedString>>, _w, cx| {
                if let SelectEvent::Confirm(Some(v)) = ev {
                    if let Some(i) = options.iter().position(|o| o == v) {
                        on_pick(this, i, cx);
                        cx.notify();
                    }
                }
            },
        ));
        state
    }

    fn color(
        &mut self,
        value: Color,
        window: &mut Window,
        cx: &mut Context<Self>,
        apply: impl Fn(&mut Config, Color) + 'static,
    ) -> Entity<ColorPickerState> {
        let state = cx.new(|cx| ColorPickerState::new(window, cx).default_value(to_hsla(value)));
        self.subs.push(cx.subscribe_in(&state, window, move |this, _s, ev: &ColorPickerEvent, _w, cx| {
            let ColorPickerEvent::Change(Some(h)) = ev else {
                return;
            };
            apply(&mut this.cfg, from_hsla(*h));
            this.changed(cx);
        }));
        state
    }

    /// Recreates every stateful widget from `self.cfg`.
    pub(crate) fn rebuild(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use minato_layout::LayoutKind;
        self.subs.clear();
        let cfg = self.cfg.clone();

        let outer_gap = self.input(cfg.gaps.outer.to_string(), window, cx, |c, v| {
            v.parse().map(|n: i32| c.gaps.outer = n.max(0)).is_ok()
        });
        let inner_gap = self.input(cfg.gaps.inner.to_string(), window, cx, |c, v| {
            v.parse().map(|n: i32| c.gaps.inner = n.max(0)).is_ok()
        });
        let resize_delta = self.input(cfg.resize_delta.to_string(), window, cx, |c, v| {
            v.parse().map(|n: i32| c.resize_delta = n.clamp(1, 2000)).is_ok()
        });
        let primary_ratio = self.input(format!("{:.2}", cfg.primary_ratio), window, cx, |c, v| {
            match v.parse::<f32>() {
                Ok(r) if (0.05..=0.95).contains(&r) => {
                    c.primary_ratio = r;
                    true
                }
                _ => false,
            }
        });
        let layouts: Vec<SharedString> = LayoutKind::ALL.iter().map(|l| l.label().into()).collect();
        let current = LayoutKind::ALL.iter().position(|l| *l == cfg.default_layout).unwrap_or(0);
        let default_layout = self.select(layouts, current, window, cx, |this, i, cx| {
            this.cfg.default_layout = LayoutKind::ALL[i];
            this.changed(cx);
        });

        let workspaces = self.build_workspace_rows(window, cx);

        let catalog = catalog::actions();
        let names: Vec<SharedString> = catalog.iter().map(|a| a.describe().into()).collect();
        let new_action = self.select(names, 0, window, cx, |_, _, _| {});

        let rule_field = self.select(catalog::RULE_FIELDS.iter().map(|s| (*s).into()).collect(), 0, window, cx, |_, _, _| {});
        let rule_kind = self.select(catalog::RULE_KINDS.iter().map(|s| (*s).into()).collect(), 0, window, cx, |_, _, _| {});
        let rule_pattern = cx.new(|cx| InputState::new(window, cx).placeholder("例: Taskmgr.exe"));
        let rule_action = self.select(catalog::rule_actions().into_iter().map(|(n, _)| n.into()).collect(), 0, window, cx, |_, _, _| {});

        let border_focused = self.color(cfg.border.focused, window, cx, |c, v| c.border.focused = v);
        let border_unfocused = self.color(
            cfg.border.unfocused.unwrap_or(Color::rgb(0x44, 0x44, 0x4c)),
            window,
            cx,
            |c, v| c.border.unfocused = Some(v),
        );
        let border_monocle = self.color(
            cfg.border.monocle.unwrap_or(cfg.border.focused),
            window,
            cx,
            |c, v| c.border.monocle = Some(v),
        );

        let anim = self.build_animation_widgets(window, cx);

        let bar_height = self.input(cfg.bar.height.to_string(), window, cx, |c, v| {
            use minato_config::BarConfig;
            match v.parse::<u32>() {
                Ok(n) if (BarConfig::MIN_HEIGHT..=BarConfig::MAX_HEIGHT).contains(&n) => {
                    c.bar.height = n;
                    true
                }
                _ => false,
            }
        });
        let bar_clock = self.input(catalog::bar_clock_format(&cfg.bar), window, cx, |c, v| {
            let format = catalog::clock_format_value(v);
            let b = &mut c.bar;
            for m in b.left.iter_mut().chain(b.center.iter_mut()).chain(b.right.iter_mut()) {
                if let minato_config::BarModule::Clock { format: f } = m {
                    *f = format.clone();
                }
            }
            true
        });
        let module_names: Vec<SharedString> =
            catalog::bar_modules().iter().map(|m| catalog::bar_module_label(m).into()).collect();
        let bar_add = [0, 1, 2].map(|_| self.select(module_names.clone(), 0, window, cx, |_, _, _| {}));

        self.w = Some(Widgets {
            anim,
            bar_height,
            bar_clock,
            bar_add,
            outer_gap,
            inner_gap,
            resize_delta,
            primary_ratio,
            default_layout,
            workspaces,
            new_action,
            rule_field,
            rule_kind,
            rule_pattern,
            rule_action,
            border_focused,
            border_unfocused,
            border_monocle,
        });
    }

    fn build_animation_widgets(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnimWidgets {
        let a = self.cfg.animations.clone();
        let names = a.curve_names();
        let options: Vec<SharedString> = names.iter().map(|n| SharedString::from(n.clone())).collect();
        let pos = |name: &str| names.iter().position(|n| n == name).unwrap_or(0);
        let (layout_pos, ws_pos, border_pos) = (pos(&a.layout.curve), pos(&a.workspace.curve), pos(&a.border.curve));
        let ms = |v: &str| v.parse::<u32>().ok().filter(|n| *n <= 3000);

        let layout_duration = self.input(a.layout.duration_ms.to_string(), window, cx, move |c, v| {
            ms(v).map(|n| c.animations.layout.duration_ms = n).is_some()
        });
        let layout_hold = self.input(a.layout.hold_ms.to_string(), window, cx, move |c, v| {
            ms(v).map(|n| c.animations.layout.hold_ms = n).is_some()
        });
        let n1 = names.clone();
        let layout_curve = self.select(options.clone(), layout_pos, window, cx, move |this, i, cx| {
            this.cfg.animations.layout.curve = n1[i].clone();
            this.changed(cx);
        });

        let ws_duration = self.input(a.workspace.duration_ms.to_string(), window, cx, move |c, v| {
            ms(v).map(|n| c.animations.workspace.duration_ms = n).is_some()
        });
        let n2 = names.clone();
        let ws_curve = self.select(options.clone(), ws_pos, window, cx, move |this, i, cx| {
            this.cfg.animations.workspace.curve = n2[i].clone();
            this.changed(cx);
        });
        let style_idx = WS_STYLES.iter().position(|(_, s)| *s == a.workspace.style).unwrap_or(0);
        let ws_style = self.select(
            WS_STYLES.iter().map(|(n, _)| SharedString::from(*n)).collect(),
            style_idx,
            window,
            cx,
            |this, i, cx| {
                this.cfg.animations.workspace.style = WS_STYLES[i].1;
                this.changed(cx);
            },
        );

        let border_duration = self.input(a.border.duration_ms.to_string(), window, cx, move |c, v| {
            ms(v).map(|n| c.animations.border.duration_ms = n).is_some()
        });
        let n3 = names;
        let border_curve = self.select(options, border_pos, window, cx, move |this, i, cx| {
            this.cfg.animations.border.curve = n3[i].clone();
            this.changed(cx);
        });

        AnimWidgets {
            layout_duration,
            layout_hold,
            layout_curve,
            ws_duration,
            ws_curve,
            ws_style,
            border_duration,
            border_curve,
        }
    }

    /// Makes sure `cfg.monitors` has an explicit entry for `ws_monitor`, so
    /// edits don't silently change the fallback used by other monitors.
    pub(crate) fn ensure_monitor_entry(&mut self) {
        while self.cfg.monitors.len() <= self.ws_monitor {
            let next = self.cfg.monitor(self.cfg.monitors.len());
            self.cfg.monitors.push(next);
        }
    }

    pub(crate) fn build_workspace_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<WorkspaceRow> {
        use minato_layout::LayoutKind;
        let mi = self.ws_monitor;
        self.ensure_monitor_entry();
        let list = self.cfg.monitor(mi).workspaces;
        let mut options: Vec<SharedString> = vec!["既定のレイアウト".into()];
        options.extend(LayoutKind::ALL.iter().map(|l| SharedString::from(l.label())));

        list.iter()
            .enumerate()
            .map(|(wi, ws)| {
                let name = self.input(ws.name.clone(), window, cx, move |c, v| {
                    // `ensure_monitor_entry` ran before any row was shown.
                    match c.monitors.get_mut(mi).and_then(|m| m.workspaces.get_mut(wi)) {
                        Some(w) => {
                            w.name = v.to_string();
                            true
                        }
                        None => false,
                    }
                });
                let selected = ws
                    .layout
                    .and_then(|l| LayoutKind::ALL.iter().position(|k| *k == l))
                    .map_or(0, |i| i + 1);
                let layout = self.select(options.clone(), selected, window, cx, move |this, i, cx| {
                    this.ensure_monitor_entry();
                    if let Some(w) = this.cfg.monitors.get_mut(mi).and_then(|m| m.workspaces.get_mut(wi)) {
                        w.layout = (i > 0).then(|| LayoutKind::ALL[i - 1]);
                        this.changed(cx);
                    }
                });
                WorkspaceRow { name, layout }
            })
            .collect()
    }

    pub(crate) fn refresh_workspace_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Simplest correct approach: rebuild everything (subscriptions are
        // keyed to the widgets they observe).
        self.ensure_monitor_entry();
        self.rebuild(window, cx);
        cx.notify();
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        pages::render(self, window, cx)
    }
}
