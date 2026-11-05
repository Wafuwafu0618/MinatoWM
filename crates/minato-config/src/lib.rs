//! MinatoWM configuration: the JSON schema, its defaults, rule matching,
//! and loading / saving / watching `config.json`.
//!
//! The JSON file is the single source of truth. The settings UI writes it,
//! and the window manager only ever applies what the watcher reads back, so
//! hand edits and UI edits behave identically.

mod action;
mod animation;
mod bar;
mod color;
mod io;
mod rules;

pub use action::{Action, CycleDirection, FlipAxis};
pub use animation::{
    AnimationConfig, AppleSpring, BorderAnimation, CurvePoints, CurveSpec, LayoutAnimation, WorkspaceAnimation,
    WorkspaceStyle, BUILTIN_CURVES,
};
pub use bar::{BarConfig, BarModule};
pub use color::Color;
pub use io::{config_dir, config_path, load, load_or_create, save, ConfigWatcher};
pub use minato_layout::{Direction, Flip, LayoutKind, Sizing};
pub use rules::{MatchKind, Matcher, Rule, RuleAction, RuleMatch, RuleSet, WindowInfo};

use serde::{Deserialize, Serialize};

pub const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub gaps: Gaps,
    /// Layout for workspaces that don't set their own.
    pub default_layout: LayoutKind,
    /// Share of the area given to the primary window in stack layouts.
    pub primary_ratio: f32,
    /// Pixels moved per resize action.
    pub resize_delta: i32,
    /// Move the cursor to the centre of a window focused by a hotkey.
    pub mouse_follows_focus: bool,
    /// Per-monitor workspaces, by monitor index (left to right).
    /// Monitors without an entry use the last entry.
    pub monitors: Vec<MonitorConfig>,
    pub border: BorderConfig,
    pub animations: AnimationConfig,
    pub bar: BarConfig,
    pub rules: Vec<Rule>,
    pub hotkeys: Vec<Hotkey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Gaps {
    /// Space between windows and the monitor's work area edges.
    pub outer: i32,
    /// Space between adjacent windows.
    pub inner: i32,
}

impl Default for Gaps {
    fn default() -> Self {
        Self {
            outer: 8,
            inner: 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MonitorConfig {
    pub workspaces: Vec<WorkspaceConfig>,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            workspaces: (1..=5).map(|i| WorkspaceConfig::named(i.to_string())).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceConfig {
    pub name: String,
    /// Overrides `default_layout` for this workspace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<LayoutKind>,
    #[serde(skip_serializing_if = "is_default_flip")]
    pub flip: Flip,
}

fn is_default_flip(f: &Flip) -> bool {
    *f == Flip::default()
}

impl WorkspaceConfig {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            layout: None,
            flip: Flip::default(),
        }
    }
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self::named("")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BorderConfig {
    pub enabled: bool,
    /// Frame colour of the focused tiled window.
    pub focused: Color,
    /// Frame colour of other managed windows; `null` keeps the system colour.
    pub unfocused: Option<Color>,
    /// Frame colour of the focused window while it is in monocle mode.
    pub monocle: Option<Color>,
}

impl Default for BorderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            focused: Color::rgb(0x7a, 0xa2, 0xf7),
            unfocused: None,
            monocle: Some(Color::rgb(0xbb, 0x9a, 0xf7)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hotkey {
    /// Keys joined by `+`, modifiers first: `"alt+shift+h"`, `"win+1"`.
    pub keys: String,
    pub action: Action,
}

impl Hotkey {
    pub fn new(keys: impl Into<String>, action: Action) -> Self {
        Self {
            keys: keys.into(),
            action,
        }
    }
}

impl Config {
    /// The workspace list for monitor `index`.
    pub fn monitor(&self, index: usize) -> MonitorConfig {
        self.monitors
            .get(index)
            .or(self.monitors.last())
            .cloned()
            .unwrap_or_default()
    }

    /// Problems that make parts of the config unusable. An empty list means
    /// everything will be applied.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if let Err(e) = RuleSet::compile(&self.rules) {
            errors.push(e.to_string());
        }
        if !(0.05..=0.95).contains(&self.primary_ratio) {
            errors.push(format!(
                "primary_ratio {} must be between 0.05 and 0.95",
                self.primary_ratio
            ));
        }
        if self.gaps.inner < 0 || self.gaps.outer < 0 {
            errors.push("gaps must not be negative".into());
        }
        self.animations.validate(&mut errors);
        if !(BarConfig::MIN_HEIGHT..=BarConfig::MAX_HEIGHT).contains(&self.bar.height) {
            errors.push(format!(
                "bar.height {} must be between {} and {}",
                self.bar.height,
                BarConfig::MIN_HEIGHT,
                BarConfig::MAX_HEIGHT
            ));
        }
        errors
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            gaps: Gaps::default(),
            default_layout: LayoutKind::Bsp,
            primary_ratio: 0.5,
            resize_delta: 50,
            mouse_follows_focus: false,
            monitors: vec![MonitorConfig::default()],
            border: BorderConfig::default(),
            animations: AnimationConfig::default(),
            bar: BarConfig::default(),
            rules: rules::default_rules(),
            hotkeys: default_hotkeys(),
        }
    }
}

/// Vim-style defaults on Alt, mirroring common tiling WM setups.
pub fn default_hotkeys() -> Vec<Hotkey> {
    use Action::*;
    use Direction::*;

    let dirs = [("h", Left), ("j", Down), ("k", Up), ("l", Right)];
    let mut keys = Vec::new();
    for (k, d) in dirs {
        keys.push(Hotkey::new(format!("alt+{k}"), Focus { dir: d }));
        keys.push(Hotkey::new(format!("alt+shift+{k}"), Move { dir: d }));
        keys.push(Hotkey::new(
            format!("alt+ctrl+{k}"),
            Resize {
                dir: d,
                sizing: Sizing::Increase,
            },
        ));
        keys.push(Hotkey::new(
            format!("alt+ctrl+shift+{k}"),
            Resize {
                dir: d,
                sizing: Sizing::Decrease,
            },
        ));
    }
    for (k, d) in [("left", Left), ("down", Down), ("up", Up), ("right", Right)] {
        keys.push(Hotkey::new(format!("alt+{k}"), Stack { dir: d }));
    }
    keys.extend([
        Hotkey::new("alt+semicolon", Unstack),
        Hotkey::new(
            "alt+open_bracket",
            CycleStack {
                dir: CycleDirection::Previous,
            },
        ),
        Hotkey::new(
            "alt+close_bracket",
            CycleStack {
                dir: CycleDirection::Next,
            },
        ),
        Hotkey::new("alt+enter", Promote),
        Hotkey::new("alt+t", ToggleFloat),
        Hotkey::new("alt+f", ToggleMonocle),
        Hotkey::new(
            "alt+space",
            CycleLayout {
                dir: CycleDirection::Next,
            },
        ),
        Hotkey::new(
            "alt+x",
            FlipLayout {
                axis: FlipAxis::Horizontal,
            },
        ),
        Hotkey::new(
            "alt+y",
            FlipLayout {
                axis: FlipAxis::Vertical,
            },
        ),
        Hotkey::new("alt+shift+r", Retile),
        Hotkey::new("alt+q", Close),
        Hotkey::new("alt+shift+p", TogglePause),
        Hotkey::new("alt+shift+o", OpenSettings),
    ]);
    for i in 1..=9 {
        keys.push(Hotkey::new(format!("alt+{i}"), FocusWorkspace { index: i - 1 }));
        keys.push(Hotkey::new(
            format!("alt+shift+{i}"),
            MoveToWorkspace { index: i - 1 },
        ));
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_round_trips() {
        let cfg = Config::default();
        let json = serde_json::to_string_pretty(&cfg).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
        assert!(cfg.validate().is_empty(), "{:?}", cfg.validate());
    }

    #[test]
    fn partial_file_uses_defaults() {
        let cfg: Config = serde_json::from_str(r#"{ "gaps": { "inner": 20 } }"#).unwrap();
        assert_eq!(cfg.gaps.inner, 20);
        assert_eq!(cfg.gaps.outer, 8);
        assert_eq!(cfg.default_layout, LayoutKind::Bsp);
        assert!(!cfg.hotkeys.is_empty());
    }

    #[test]
    fn monitor_fallback() {
        let mut cfg = Config {
            monitors: vec![
                MonitorConfig {
                    workspaces: vec![WorkspaceConfig::named("a")],
                },
                MonitorConfig {
                    workspaces: vec![WorkspaceConfig::named("b")],
                },
            ],
            ..Config::default()
        };
        assert_eq!(cfg.monitor(0).workspaces[0].name, "a");
        assert_eq!(cfg.monitor(5).workspaces[0].name, "b");
        cfg.monitors.clear();
        assert_eq!(cfg.monitor(0).workspaces.len(), 5);
    }

    #[test]
    fn schema_example_parses() {
        let json = r##"{
            "version": 1,
            "gaps": { "outer": 4, "inner": 6 },
            "default_layout": "vertical_stack",
            "monitors": [{ "workspaces": [
                { "name": "web", "layout": "columns" },
                { "name": "code", "flip": { "horizontal": true } }
            ] }],
            "border": { "enabled": false, "focused": "#ff0000", "unfocused": null },
            "rules": [
                { "match": { "exe": "Taskmgr.exe" }, "action": "float" },
                { "match": { "title": { "regex": "^Picture" } }, "action": "ignore" },
                { "match": { "exe": { "contains": "steam" } }, "action": { "workspace": 3 } }
            ],
            "hotkeys": [
                { "keys": "win+h", "action": { "type": "focus", "dir": "left" } },
                { "keys": "win+2", "action": { "type": "focus_workspace", "index": 1 } },
                { "keys": "win+l", "action": { "type": "change_layout", "layout": "grid" } }
            ]
        }"##;
        let cfg: Config = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.default_layout, LayoutKind::VerticalStack);
        assert_eq!(cfg.monitors[0].workspaces[0].layout, Some(LayoutKind::Columns));
        assert!(cfg.monitors[0].workspaces[1].flip.horizontal);
        assert_eq!(cfg.border.focused, Color::rgb(255, 0, 0));
        assert_eq!(cfg.rules[2].action, RuleAction::Workspace(3));
        assert_eq!(cfg.hotkeys[0].action, Action::Focus { dir: Direction::Left });
        assert!(cfg.validate().is_empty());
    }

    #[test]
    fn validation_reports_bad_regex() {
        let mut cfg = Config::default();
        cfg.rules.push(Rule {
            matcher: RuleMatch {
                title: Some(Matcher::Kind(MatchKind::Regex("(".into()))),
                ..Default::default()
            },
            action: RuleAction::Float,
        });
        assert_eq!(cfg.validate().len(), 1);
    }
}
