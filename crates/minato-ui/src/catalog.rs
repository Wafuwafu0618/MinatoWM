//! Choices offered by the settings UI.

use minato_config::{Action, CycleDirection, Direction, FlipAxis, LayoutKind, RuleAction, Sizing};

/// Every action the hotkey editor can bind, in display order.
pub fn actions() -> Vec<Action> {
    use Action::*;
    let mut out = Vec::new();
    for dir in Direction::ALL {
        out.push(Focus { dir });
    }
    for dir in Direction::ALL {
        out.push(Move { dir });
    }
    for dir in Direction::ALL {
        out.push(Stack { dir });
    }
    out.extend([
        Unstack,
        CycleStack { dir: CycleDirection::Next },
        CycleStack { dir: CycleDirection::Previous },
        Promote,
    ]);
    for sizing in [Sizing::Increase, Sizing::Decrease] {
        for dir in Direction::ALL {
            out.push(Resize { dir, sizing });
        }
    }
    out.extend([
        ToggleFloat,
        ToggleMonocle,
        CycleLayout { dir: CycleDirection::Next },
        CycleLayout { dir: CycleDirection::Previous },
        FlipLayout { axis: FlipAxis::Horizontal },
        FlipLayout { axis: FlipAxis::Vertical },
    ]);
    for layout in LayoutKind::ALL {
        out.push(ChangeLayout { layout });
    }
    for index in 0..9 {
        out.push(FocusWorkspace { index });
    }
    for index in 0..9 {
        out.push(MoveToWorkspace { index });
    }
    for index in 0..9 {
        out.push(SendToWorkspace { index });
    }
    for index in 0..3 {
        out.push(FocusMonitor { index });
    }
    for index in 0..3 {
        out.push(MoveToMonitor { index });
    }
    out.extend([Retile, Close, TogglePause, OpenSettings, Quit]);
    out
}

pub const RULE_FIELDS: &[&str] = &["exe", "class", "title", "path"];
pub const RULE_KINDS: &[&str] = &["完全一致", "含む", "前方一致", "後方一致", "正規表現"];

pub fn rule_actions() -> Vec<(String, RuleAction)> {
    let mut out = vec![
        ("フロート".to_string(), RuleAction::Float),
        ("無視".to_string(), RuleAction::Ignore),
    ];
    for i in 0..9 {
        out.push((format!("ワークスペース {}", i + 1), RuleAction::Workspace(i)));
    }
    out
}

/// Status bar modules the settings UI can add, in display order.
pub fn bar_modules() -> Vec<minato_config::BarModule> {
    use minato_config::BarModule::*;
    vec![Workspaces, Layout, Title, Clock { format: None }, Cpu, Memory]
}

pub fn bar_module_label(m: &minato_config::BarModule) -> String {
    use minato_config::BarModule::*;
    match m {
        Workspaces => "ワークスペース".into(),
        Layout => "レイアウト".into(),
        Title => "ウィンドウ名".into(),
        Clock { .. } => "時計".into(),
        Cpu => "CPU".into(),
        Memory => "メモリ".into(),
        Text { text } => format!("テキスト「{text}」"),
    }
}

/// The clock format the bar uses (the first clock module's), for display.
pub fn bar_clock_format(bar: &minato_config::BarConfig) -> String {
    bar.left
        .iter()
        .chain(&bar.center)
        .chain(&bar.right)
        .find_map(|m| match m {
            minato_config::BarModule::Clock { format } => format.clone(),
            _ => None,
        })
        .unwrap_or_else(|| minato_config::BarModule::DEFAULT_CLOCK.to_string())
}

/// `None` for the default format, so the JSON stays short.
pub fn clock_format_value(v: &str) -> Option<String> {
    let v = v.trim();
    (!v.is_empty() && v != minato_config::BarModule::DEFAULT_CLOCK).then(|| v.to_string())
}
