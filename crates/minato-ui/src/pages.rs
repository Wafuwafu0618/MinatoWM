//! Rendering of the settings pages.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::color_picker::ColorPicker;
use gpui_kit::component::input::Input;
use gpui_kit::component::select::Select;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::TabBar;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use minato_config::{Hotkey, MatchKind, Matcher, Rule, RuleAction, RuleMatch, WorkspaceConfig};

use crate::{catalog, RecordTarget, SettingsView};

const TABS: [&str; 8] = [
    "一般",
    "ワークスペース",
    "ホットキー",
    "ルール",
    "ボーダー",
    "アニメーション",
    "バー",
    "状態",
];

pub fn render(v: &mut SettingsView, window: &mut Window, cx: &mut Context<SettingsView>) -> impl IntoElement {
    let theme = cx.theme();
    let (bg, fg, muted, danger) = (theme.background, theme.foreground, theme.muted_foreground, theme.danger);

    let status: SharedString = if let Some(e) = &v.save_error {
        format!("保存に失敗しました: {e}").into()
    } else if let Some(e) = v.snapshot.config_errors.first() {
        format!("設定エラー: {e}").into()
    } else if v.snapshot.paused {
        "一時停止中".into()
    } else {
        "変更は自動保存され、すぐに反映されます".into()
    };
    let status_is_error = v.save_error.is_some() || !v.snapshot.config_errors.is_empty();

    let page = match v.tab {
        0 => general(v, cx).into_any_element(),
        1 => workspaces(v, cx).into_any_element(),
        2 => hotkeys(v, cx).into_any_element(),
        3 => rules(v, cx).into_any_element(),
        4 => border(v, cx).into_any_element(),
        5 => animations(v, cx).into_any_element(),
        6 => bar(v, cx).into_any_element(),
        _ => status_page(v, cx).into_any_element(),
    };
    let _ = window;

    v_flex()
        .size_full()
        .bg(bg)
        .text_color(fg)
        .child(
            h_flex()
                .px_4()
                .pt_3()
                .pb_2()
                .justify_between()
                .items_center()
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("MinatoWM"))
                .child(
                    div()
                        .text_sm()
                        .text_color(if status_is_error { danger } else { muted })
                        .child(status),
                ),
        )
        .child(
            div().px_4().child(
                TabBar::new("tabs")
                    .underline()
                    .selected_index(v.tab)
                    .on_click(cx.listener(|this, i: &usize, _, cx| {
                        this.tab = *i;
                        cx.notify();
                    }))
                    .children(TABS),
            ),
        )
        .child(if v.tab == 2 {
            // The hotkey page scrolls its own (virtualized) list.
            div().flex_1().min_h_0().p_4().child(page).into_any_element()
        } else {
            div()
                .id("page")
                .flex_1()
                .overflow_y_scroll()
                .p_4()
                .child(page)
                .into_any_element()
        })
}

fn row(label: impl Into<SharedString>, control: impl IntoElement) -> impl IntoElement {
    h_flex()
        .gap_4()
        .items_center()
        .min_h(px(36.))
        .child(div().w(px(220.)).flex_none().child(label.into()))
        .child(div().flex_1().child(control))
}

fn section(title: &str) -> impl IntoElement {
    div()
        .pt_4()
        .pb_1()
        .font_weight(FontWeight::SEMIBOLD)
        .child(SharedString::from(title.to_string()))
}

fn hint(text: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    div().text_sm().text_color(cx.theme().muted_foreground).child(text.into())
}

// ----- General ------------------------------------------------------------------

fn general(v: &mut SettingsView, cx: &mut Context<SettingsView>) -> impl IntoElement {
    let w = v.w.as_ref().unwrap();
    v_flex()
        .gap_2()
        .child(section("タイリング"))
        .child(row("既定のレイアウト", div().w(px(260.)).child(Select::new(&w.default_layout))))
        .child(row("外側の余白 (px)", div().w(px(120.)).child(Input::new(&w.outer_gap))))
        .child(row("ウィンドウ間の余白 (px)", div().w(px(120.)).child(Input::new(&w.inner_gap))))
        .child(row("メイン領域の比率 (0.05–0.95)", div().w(px(120.)).child(Input::new(&w.primary_ratio))))
        .child(row("リサイズ量 (px)", div().w(px(120.)).child(Input::new(&w.resize_delta))))
        .child(section("フォーカス"))
        .child(row(
            "フォーカス移動時にマウスも移動",
            Switch::new("mff")
                .checked(v.cfg.mouse_follows_focus)
                .on_click(cx.listener(|this, on: &bool, _, cx| {
                    this.cfg.mouse_follows_focus = *on;
                    this.changed(cx);
                })),
        ))
        .child(hint(
            format!("設定ファイル: {}", v.ctx.config_path.display()),
            cx,
        ))
}

// ----- Workspaces ---------------------------------------------------------------

fn workspaces(v: &mut SettingsView, cx: &mut Context<SettingsView>) -> impl IntoElement {
    let monitor_count = v.snapshot.monitors.len().max(v.cfg.monitors.len()).max(1);
    let mi = v.ws_monitor;
    let rows = &v.w.as_ref().unwrap().workspaces;

    let monitors = h_flex().gap_2().children((0..monitor_count).map(|i| {
        let label = v
            .snapshot
            .monitors
            .get(i)
            .map(|m| format!("モニタ {} ({}×{})", i + 1, m.rect.w, m.rect.h))
            .unwrap_or_else(|| format!("モニタ {}", i + 1));
        Button::new(("mon", i))
            .label(label)
            .small()
            .when(i == mi, |b| b.primary())
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.ws_monitor = i;
                this.refresh_workspace_rows(window, cx);
            }))
    }));

    let list = v_flex().gap_2().children(rows.iter().enumerate().map(|(wi, r)| {
        h_flex()
            .gap_3()
            .items_center()
            .child(div().w(px(28.)).child(format!("{}", wi + 1)))
            .child(div().w(px(200.)).child(Input::new(&r.name)))
            .child(div().w(px(260.)).child(Select::new(&r.layout)))
            .child(
                Button::new(("ws-del", wi))
                    .label("削除")
                    .small()
                    .ghost()
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.ensure_monitor_entry();
                        let list = &mut this.cfg.monitors[this.ws_monitor].workspaces;
                        if list.len() > 1 {
                            list.remove(wi);
                            this.changed(cx);
                            this.refresh_workspace_rows(window, cx);
                        }
                    })),
            )
    }));

    v_flex()
        .gap_3()
        .child(monitors)
        .child(hint(
            "設定のないモニタは最後のモニタの設定を使います。Alt+数字キーで切り替えます。",
            cx,
        ))
        .child(list)
        .child(
            Button::new("ws-add")
                .label("ワークスペースを追加")
                .small()
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.ensure_monitor_entry();
                    let list = &mut this.cfg.monitors[this.ws_monitor].workspaces;
                    list.push(WorkspaceConfig::named((list.len() + 1).to_string()));
                    this.changed(cx);
                    this.refresh_workspace_rows(window, cx);
                })),
        )
}

// ----- Hotkeys ------------------------------------------------------------------

fn hotkeys(v: &mut SettingsView, cx: &mut Context<SettingsView>) -> impl IntoElement {
    let w = v.w.as_ref().unwrap();
    let recording_new = v.recording == Some(RecordTarget::New);
    let new_label = if recording_new {
        "キーを押してください… (Esc で取消)".to_string()
    } else if v.new_keys.is_empty() {
        "キーを記録".to_string()
    } else {
        v.new_keys.clone()
    };

    let form = h_flex()
        .gap_2()
        .items_center()
        .child(
            Button::new("hk-record")
                .label(new_label)
                .small()
                .when(recording_new, |b| b.primary())
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.start_recording(RecordTarget::New, cx);
                })),
        )
        .child(div().w(px(300.)).child(Select::new(&w.new_action)))
        .child(
            Button::new("hk-add")
                .label("追加")
                .small()
                .primary()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    if this.new_keys.is_empty() {
                        return;
                    }
                    let w = this.w.as_ref().unwrap();
                    let idx = w.new_action.read(cx).selected_index(cx).map(|p| p.row).unwrap_or(0);
                    let action = catalog::actions()[idx];
                    let keys = std::mem::take(&mut this.new_keys);
                    this.cfg.hotkeys.retain(|h| !h.keys.eq_ignore_ascii_case(&keys));
                    this.cfg.hotkeys.push(Hotkey::new(keys, action));
                    this.changed(cx);
                })),
        );

    // Virtualized: only the rows in view are built, so long lists stay cheap.
    let list = uniform_list(
        "hk-list",
        v.cfg.hotkeys.len(),
        cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
            range
                .filter_map(|i| {
                    let hk = this.cfg.hotkeys.get(i)?;
                    let recording = this.recording == Some(RecordTarget::Row(i));
                    Some(
                        h_flex()
                            .h(px(32.))
                            .gap_3()
                            .items_center()
                            .child(
                                div().w(px(220.)).flex_none().child(
                                    Button::new(("hk-keys", i))
                                        .label(if recording {
                                            "キーを押してください…".to_string()
                                        } else {
                                            hk.keys.clone()
                                        })
                                        .small()
                                        .outline()
                                        .when(recording, |b| b.primary())
                                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            this.start_recording(RecordTarget::Row(i), cx);
                                        })),
                                ),
                            )
                            .child(div().flex_1().truncate().child(hk.action.describe())),
                    )
                })
                .collect::<Vec<_>>()
        }),
    )
    .flex_1()
    .min_h_0();

    v_flex()
        .size_full()
        .gap_3()
        .child(form)
        .child(hint(
            "キー欄をクリックして押したキーで再設定できます。同じキーの割り当ては上書きされます。",
            cx,
        ))
        .child(list)
        .child(
            Button::new("hk-reset")
                .label("既定に戻す")
                .small()
                .ghost()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.cfg.hotkeys = minato_config::default_hotkeys();
                    this.changed(cx);
                })),
        )
}

// ----- Rules --------------------------------------------------------------------

fn describe_rule(r: &Rule) -> String {
    let mut parts = Vec::new();
    for (name, m) in [
        ("exe", &r.matcher.exe),
        ("path", &r.matcher.path),
        ("class", &r.matcher.class),
        ("title", &r.matcher.title),
    ] {
        if let Some(m) = m {
            let kind = match m.kind() {
                MatchKind::Exact(_) => "=",
                MatchKind::Contains(_) => "含む",
                MatchKind::Prefix(_) => "で始まる",
                MatchKind::Suffix(_) => "で終わる",
                MatchKind::Regex(_) => "~",
            };
            parts.push(format!("{name} {kind} \"{}\"", m.pattern()));
        }
    }
    let action = match r.action {
        RuleAction::Float => "フロート".to_string(),
        RuleAction::Ignore => "無視".to_string(),
        RuleAction::Workspace(i) => format!("ワークスペース {}", i + 1),
    };
    format!("{}  →  {action}", parts.join(" かつ "))
}

fn exe_rule(exe: &str, action: RuleAction) -> Rule {
    Rule {
        matcher: RuleMatch {
            exe: Some(Matcher::Exact(exe.to_string())),
            ..Default::default()
        },
        action,
    }
}

fn rules(v: &mut SettingsView, cx: &mut Context<SettingsView>) -> impl IntoElement {
    let w = v.w.as_ref().unwrap();
    let form = h_flex()
        .gap_2()
        .items_center()
        .child(div().w(px(110.)).child(Select::new(&w.rule_field)))
        .child(div().w(px(130.)).child(Select::new(&w.rule_kind)))
        .child(div().w(px(220.)).child(Input::new(&w.rule_pattern)))
        .child(div().w(px(170.)).child(Select::new(&w.rule_action)))
        .child(
            Button::new("rule-add")
                .label("追加")
                .small()
                .primary()
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    let w = this.w.as_ref().unwrap();
                    let pattern = w.rule_pattern.read(cx).value().trim().to_string();
                    if pattern.is_empty() {
                        return;
                    }
                    let pick = |s: &gpui_kit::Entity<crate::StrSelect>, cx: &App| {
                        s.read(cx).selected_index(cx).map(|p| p.row).unwrap_or(0)
                    };
                    let field = pick(&w.rule_field, cx);
                    let kind = pick(&w.rule_kind, cx);
                    let action = catalog::rule_actions()[pick(&w.rule_action, cx)].1;
                    let m = Matcher::Kind(match kind {
                        1 => MatchKind::Contains(pattern),
                        2 => MatchKind::Prefix(pattern),
                        3 => MatchKind::Suffix(pattern),
                        4 => MatchKind::Regex(pattern),
                        _ => MatchKind::Exact(pattern),
                    });
                    let mut matcher = RuleMatch::default();
                    match field {
                        1 => matcher.class = Some(m),
                        2 => matcher.title = Some(m),
                        3 => matcher.path = Some(m),
                        _ => matcher.exe = Some(m),
                    }
                    this.cfg.rules.insert(0, Rule { matcher, action });
                    this.changed(cx);
                    let state = this.w.as_ref().unwrap().rule_pattern.clone();
                    state.update(cx, |s, cx| s.set_value("", window, cx));
                })),
        );

    let list = v_flex().gap_1().children(v.cfg.rules.iter().enumerate().map(|(i, r)| {
        h_flex()
            .gap_3()
            .items_center()
            .child(div().flex_1().child(describe_rule(r)))
            .child(
                Button::new(("rule-del", i))
                    .label("削除")
                    .small()
                    .ghost()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if i < this.cfg.rules.len() {
                            this.cfg.rules.remove(i);
                            this.changed(cx);
                        }
                    })),
            )
    }));

    let mut seen = std::collections::HashSet::new();
    let open: Vec<(String, String)> = v
        .snapshot
        .monitors
        .iter()
        .flat_map(|m| m.workspaces.iter().flat_map(|w| w.windows.iter()))
        .filter(|w| !w.exe.is_empty() && seen.insert(w.exe.to_lowercase()))
        .map(|w| (w.exe.clone(), w.title.clone()))
        .collect();
    let open_list = v_flex().gap_1().children(open.into_iter().enumerate().map(|(i, (exe, title))| {
        let float_exe = exe.clone();
        let ignore_exe = exe.clone();
        h_flex()
            .gap_3()
            .items_center()
            .child(div().w(px(200.)).child(exe))
            .child(div().flex_1().text_sm().text_color(cx.theme().muted_foreground).child(title))
            .child(
                Button::new(("open-float", i))
                    .label("フロート")
                    .small()
                    .outline()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.cfg.rules.insert(0, exe_rule(&float_exe, RuleAction::Float));
                        this.changed(cx);
                    })),
            )
            .child(
                Button::new(("open-ignore", i))
                    .label("無視")
                    .small()
                    .outline()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.cfg.rules.insert(0, exe_rule(&ignore_exe, RuleAction::Ignore));
                        this.changed(cx);
                    })),
            )
    }));

    v_flex()
        .gap_3()
        .child(section("新しいルール"))
        .child(form)
        .child(hint("ルールは上から順に評価され、最初に一致したものが使われます。", cx))
        .child(section("ルール一覧"))
        .child(list)
        .child(section("管理中のアプリから追加"))
        .child(open_list)
}

// ----- Border -------------------------------------------------------------------

fn border(v: &mut SettingsView, cx: &mut Context<SettingsView>) -> impl IntoElement {
    let w = v.w.as_ref().unwrap();
    let b = &v.cfg.border;
    v_flex()
        .gap_2()
        .child(row(
            "フォーカス枠を表示",
            Switch::new("border-on")
                .checked(b.enabled)
                .on_click(cx.listener(|this, on: &bool, _, cx| {
                    this.cfg.border.enabled = *on;
                    this.changed(cx);
                })),
        ))
        .child(row("フォーカス中の枠の色", ColorPicker::new(&w.border_focused)))
        .child(row(
            "非フォーカスの枠に色を付ける",
            Switch::new("border-unf")
                .checked(b.unfocused.is_some())
                .on_click(cx.listener(|this, on: &bool, _, cx| {
                    this.cfg.border.unfocused = if *on {
                        Some(minato_config::Color::rgb(0x44, 0x44, 0x4c))
                    } else {
                        None
                    };
                    this.changed(cx);
                })),
        ))
        .when(b.unfocused.is_some(), |el| {
            el.child(row("非フォーカスの枠の色", ColorPicker::new(&w.border_unfocused)))
        })
        .child(row("モノクル時の枠の色", ColorPicker::new(&w.border_monocle)))
        .child(hint(
            "Windows 11 のウィンドウ枠の色 (DWM) を変更します。Windows 10 では効果がありません。",
            cx,
        ))
}

// ----- Status bar ---------------------------------------------------------------

const BAR_SECTIONS: [&str; 3] = ["左", "中央", "右"];

/// The module list of bar section `i` (0 = left, 1 = center, 2 = right).
fn bar_section(cfg: &mut minato_config::Config, i: usize) -> &mut Vec<minato_config::BarModule> {
    match i {
        0 => &mut cfg.bar.left,
        1 => &mut cfg.bar.center,
        _ => &mut cfg.bar.right,
    }
}

fn bar(v: &mut SettingsView, cx: &mut Context<SettingsView>) -> impl IntoElement {
    let w = v.w.as_ref().unwrap();
    let mut page = v_flex()
        .gap_2()
        .child(row(
            "ステータスバーを表示",
            Switch::new("bar-on")
                .checked(v.cfg.bar.enabled)
                .on_click(cx.listener(|this, on: &bool, _, cx| {
                    this.cfg.bar.enabled = *on;
                    this.changed(cx);
                })),
        ))
        .child(row("高さ (px, 16–64)", div().w(px(120.)).child(Input::new(&w.bar_height))))
        .child(row("時計の書式", div().w(px(260.)).child(Input::new(&w.bar_clock))))
        .child(hint("%Y 年 / %m 月 / %d 日 / %H 時 / %M 分 / %S 秒 / %a 曜日", cx));

    let mut cfg = v.cfg.clone();
    for (si, title) in BAR_SECTIONS.into_iter().enumerate() {
        let modules = bar_section(&mut cfg, si).clone();
        let len = modules.len();
        let chips = h_flex().gap_3().flex_wrap().children(modules.iter().enumerate().map(|(mi, m)| {
            let id = si * 100 + mi;
            h_flex()
                .items_center()
                .child(
                    Button::new(("bar-l", id))
                        .label("‹")
                        .xsmall()
                        .ghost()
                        .disabled(mi == 0)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            let list = bar_section(&mut this.cfg, si);
                            if mi > 0 && mi < list.len() {
                                list.swap(mi - 1, mi);
                                this.changed(cx);
                            }
                        })),
                )
                .child(div().px_1().child(catalog::bar_module_label(m)))
                .child(
                    Button::new(("bar-r", id))
                        .label("›")
                        .xsmall()
                        .ghost()
                        .disabled(mi + 1 >= len)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            let list = bar_section(&mut this.cfg, si);
                            if mi + 1 < list.len() {
                                list.swap(mi, mi + 1);
                                this.changed(cx);
                            }
                        })),
                )
                .child(
                    Button::new(("bar-x", id))
                        .label("×")
                        .xsmall()
                        .ghost()
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            let list = bar_section(&mut this.cfg, si);
                            if mi < list.len() {
                                list.remove(mi);
                                this.changed(cx);
                            }
                        })),
                )
        }));
        let add = h_flex()
            .gap_2()
            .items_center()
            .child(div().w(px(200.)).child(Select::new(&w.bar_add[si])))
            .child(
                Button::new(("bar-add", si))
                    .label("追加")
                    .small()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let w = this.w.as_ref().unwrap();
                        let idx = w.bar_add[si].read(cx).selected_index(cx).map(|p| p.row).unwrap_or(0);
                        let mut module = catalog::bar_modules()[idx].clone();
                        if let minato_config::BarModule::Clock { format } = &mut module {
                            // Use the format the clock field shows.
                            *format = catalog::clock_format_value(&w.bar_clock.read(cx).value());
                        }
                        bar_section(&mut this.cfg, si).push(module);
                        this.changed(cx);
                    })),
            );
        page = page
            .child(section(title))
            .child(if len == 0 { hint("(なし)", cx).into_any_element() } else { chips.into_any_element() })
            .child(add);
    }
    page.child(
        div().pt_4().child(
            Button::new("bar-reset")
                .label("既定に戻す")
                .small()
                .ghost()
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.cfg.bar = minato_config::BarConfig::default();
                    this.changed(cx);
                    this.rebuild(window, cx);
                })),
        ),
    )
    .child(hint(
        "バーはタスクバーを除いた画面の上端に表示され、ウィンドウはその下に並びます。ワークスペース名をクリックすると切り替わります。全画面のアプリが前面にある間は隠れます。",
        cx,
    ))
}

// ----- Status -------------------------------------------------------------------

fn status_page(v: &mut SettingsView, cx: &mut Context<SettingsView>) -> impl IntoElement {
    let snap = v.snapshot.clone();
    let danger = cx.theme().danger;
    let muted = cx.theme().muted_foreground;

    let buttons = h_flex()
        .gap_2()
        .child(
            Button::new("st-retile")
                .label("再タイル")
                .small()
                .on_click(cx.listener(|this, _: &ClickEvent, _, _| {
                    this.send(minato_config::Action::Retile);
                })),
        )
        .child(
            Button::new("st-pause")
                .label(if snap.paused { "再開" } else { "一時停止" })
                .small()
                .on_click(cx.listener(|this, _: &ClickEvent, _, _| {
                    this.send(minato_config::Action::TogglePause);
                })),
        )
        .child(
            Button::new("st-open")
                .label("config.json を開く")
                .small()
                .on_click(cx.listener(|this, _: &ClickEvent, _, _| {
                    let _ = std::process::Command::new("explorer").arg(&this.ctx.config_path).spawn();
                })),
        )
        .child(
            Button::new("st-quit")
                .label("MinatoWM を終了")
                .small()
                .danger()
                .on_click(cx.listener(|this, _: &ClickEvent, _, _| {
                    this.send(minato_config::Action::Quit);
                })),
        );

    let errors = v_flex()
        .gap_1()
        .children(snap.config_errors.iter().map(|e| div().text_color(danger).child(e.clone())));

    let monitors = v_flex().gap_3().children(snap.monitors.iter().enumerate().map(|(mi, m)| {
        v_flex()
            .gap_1()
            .child(
                div().font_weight(FontWeight::SEMIBOLD).child(format!(
                    "モニタ {} {}  {}×{}{}",
                    mi + 1,
                    m.device,
                    m.rect.w,
                    m.rect.h,
                    if m.focused { "  (フォーカス中)" } else { "" }
                )),
            )
            .children(m.workspaces.iter().enumerate().map(|(wi, ws)| {
                let active = wi == m.active_workspace;
                v_flex()
                    .pl_4()
                    .child(
                        div()
                            .when(!active, |d| d.text_color(muted))
                            .child(format!(
                                "{} {}  [{}{}]  {} 枚",
                                if active { "▶" } else { "  " },
                                ws.name,
                                ws.layout.label(),
                                if ws.monocle { ", モノクル" } else { "" },
                                ws.windows.len()
                            )),
                    )
                    .children(ws.windows.iter().map(|w| {
                        div().pl_6().text_sm().text_color(muted).child(format!(
                            "{}{} — {}{}",
                            if w.focused { "● " } else { "" },
                            w.exe,
                            w.title,
                            if w.floating { "  (フロート)" } else { "" }
                        ))
                    }))
            }))
    }));

    v_flex()
        .gap_3()
        .child(buttons)
        .child(errors)
        .child(monitors)
}

// ----- Animations ---------------------------------------------------------------

fn animations(v: &mut SettingsView, cx: &mut Context<SettingsView>) -> impl IntoElement {
    let w = &v.w.as_ref().unwrap().anim;
    let a = &v.cfg.animations;
    let narrow = |state: &Entity<gpui_kit::component::input::InputState>| div().w(px(120.)).child(Input::new(state));
    let wide = |state: &Entity<crate::StrSelect>| div().w(px(220.)).child(Select::new(state));

    v_flex()
        .gap_2()
        .child(row(
            "アニメーションを有効にする",
            Switch::new("anim-on")
                .checked(a.enabled)
                .on_click(cx.listener(|this, on: &bool, _, cx| {
                    this.cfg.animations.enabled = *on;
                    this.changed(cx);
                })),
        ))
        .child(hint(
            "画面を一瞬「凍結」した画像を動かして、配置の変化を滑らかに見せます。Windows の「アニメーション効果」がオフのときは無効になります。",
            cx,
        ))
        .when(a.enabled, |el| {
            el.child(section("配置の変化 (入れ替え・リサイズ・開閉)"))
                .child(row(
                    "クロスフェード",
                    Switch::new("anim-layout")
                        .checked(a.layout.enabled)
                        .on_click(cx.listener(|this, on: &bool, _, cx| {
                            this.cfg.animations.layout.enabled = *on;
                            this.changed(cx);
                        })),
                ))
                .child(row("時間 (ms)", narrow(&w.layout_duration)))
                .child(row("再描画を待つ時間 (ms)", narrow(&w.layout_hold)))
                .child(row("カーブ", wide(&w.layout_curve)))
                .child(section("ワークスペース切替"))
                .child(row(
                    "アニメーション",
                    Switch::new("anim-ws")
                        .checked(a.workspace.enabled)
                        .on_click(cx.listener(|this, on: &bool, _, cx| {
                            this.cfg.animations.workspace.enabled = *on;
                            this.changed(cx);
                        })),
                ))
                .child(row("スタイル", wide(&w.ws_style)))
                .child(row("カーブ", wide(&w.ws_curve)))
                .map(|el| {
                    if a.curve(&a.workspace.curve).is_physical() {
                        el.child(hint(
                            "macos_spaces は Apple のバネ (duration 0.5 / bounce 0) です。長さは物理演算で決まるので時間の設定は使いません。連打すると勢いを引き継ぎます。",
                            cx,
                        ))
                    } else {
                        el.child(row("時間 (ms)", narrow(&w.ws_duration)))
                    }
                })
                .child(section("フォーカス枠"))
                .child(row(
                    "色の遷移",
                    Switch::new("anim-border")
                        .checked(a.border.enabled)
                        .on_click(cx.listener(|this, on: &bool, _, cx| {
                            this.cfg.animations.border.enabled = *on;
                            this.changed(cx);
                        })),
                ))
                .child(row("時間 (ms)", narrow(&w.border_duration)))
                .child(row("カーブ", wide(&w.border_curve)))
                .child(hint(
                    "apple はバネ (Apple の UI と同じ動き)。config.json の animations.curves に [x1, y1, x2, y2] (CSS の cubic-bezier) か {\"spring\": 減衰率} で独自のカーブを追加できます。",
                    cx,
                ))
                .child(
                    Button::new("anim-try")
                        .label("試す (再タイル)")
                        .small()
                        .on_click(cx.listener(|this, _: &ClickEvent, _, _| {
                            this.send(minato_config::Action::Retile);
                        })),
                )
        })
}
