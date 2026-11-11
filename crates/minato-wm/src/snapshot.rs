//! Read-only view of the WM state, published for the settings UI.

use minato_layout::{LayoutKind, Rect};
use serde::Serialize;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Snapshot {
    pub paused: bool,
    /// Problems found in the current config (bad regex, unknown keys, ...).
    pub config_errors: Vec<String>,
    pub monitors: Vec<MonitorSnapshot>,
    /// The foreground window, whether or not it is managed.
    pub foreground: Option<ForegroundSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ForegroundSnapshot {
    /// `HMONITOR` of the monitor the window is on.
    pub monitor: isize,
    pub title: String,
    pub exe: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MonitorSnapshot {
    /// `HMONITOR`.
    pub handle: isize,
    pub device: String,
    pub rect: Rect,
    pub work_area: Rect,
    /// Effective DPI (96 = 100%).
    pub dpi: u32,
    /// Where the status bar goes (physical pixels); `None` when disabled.
    pub bar: Option<Rect>,
    /// A full-screen window (game, video) is in front on this monitor.
    pub fullscreen: bool,
    pub focused: bool,
    pub active_workspace: usize,
    pub workspaces: Vec<WorkspaceSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkspaceSnapshot {
    pub name: String,
    pub layout: LayoutKind,
    pub monocle: bool,
    pub windows: Vec<WindowSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WindowSnapshot {
    pub hwnd: isize,
    pub title: String,
    pub exe: String,
    pub class: String,
    pub floating: bool,
    pub focused: bool,
}
