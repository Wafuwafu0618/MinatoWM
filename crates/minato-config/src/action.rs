use minato_layout::{Direction, LayoutKind, Sizing};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CycleDirection {
    Next,
    Previous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlipAxis {
    Horizontal,
    Vertical,
}

/// Something a hotkey (or the tray / settings UI) can ask the WM to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Focus the neighbouring window, crossing to the next monitor at the edge.
    Focus { dir: Direction },
    /// Swap the focused window with its neighbour, or move it to the next monitor.
    Move { dir: Direction },
    /// Put the focused window into the neighbouring container's stack.
    Stack { dir: Direction },
    /// Take the focused window out of its stack into its own container.
    Unstack,
    /// Show the next / previous window of the focused stack.
    CycleStack { dir: CycleDirection },
    /// Swap the focused container with the primary (first) one.
    Promote,
    /// Move one edge of the focused window.
    Resize { dir: Direction, sizing: Sizing },
    ToggleFloat,
    ToggleMonocle,
    ChangeLayout { layout: LayoutKind },
    CycleLayout { dir: CycleDirection },
    FlipLayout { axis: FlipAxis },
    FocusWorkspace { index: usize },
    /// Move the focused window to a workspace and follow it.
    MoveToWorkspace { index: usize },
    /// Move the focused window to a workspace and stay where you are.
    SendToWorkspace { index: usize },
    FocusMonitor { index: usize },
    MoveToMonitor { index: usize },
    /// Re-apply the layout of every visible workspace.
    Retile,
    /// Close the focused window.
    Close,
    /// Stop tiling and hotkeys (except this one) until toggled again.
    TogglePause,
    OpenSettings,
    Quit,
}

impl Action {
    /// A short human-readable description, used by the settings UI.
    pub fn describe(&self) -> String {
        let dir = |d: &Direction| format!("{d:?}").to_lowercase();
        match self {
            Action::Focus { dir: d } => format!("Focus {}", dir(d)),
            Action::Move { dir: d } => format!("Move {}", dir(d)),
            Action::Stack { dir: d } => format!("Stack {}", dir(d)),
            Action::Unstack => "Unstack".into(),
            Action::CycleStack { dir } => format!("Cycle stack {dir:?}").to_lowercase(),
            Action::Promote => "Promote".into(),
            Action::Resize { dir: d, sizing } => {
                format!("Resize {} edge ({sizing:?})", dir(d)).to_lowercase()
            }
            Action::ToggleFloat => "Toggle float".into(),
            Action::ToggleMonocle => "Toggle monocle".into(),
            Action::ChangeLayout { layout } => format!("Layout: {}", layout.label()),
            Action::CycleLayout { dir } => format!("Cycle layout {dir:?}").to_lowercase(),
            Action::FlipLayout { axis } => format!("Flip {axis:?}").to_lowercase(),
            Action::FocusWorkspace { index } => format!("Focus workspace {}", index + 1),
            Action::MoveToWorkspace { index } => format!("Move to workspace {}", index + 1),
            Action::SendToWorkspace { index } => format!("Send to workspace {}", index + 1),
            Action::FocusMonitor { index } => format!("Focus monitor {}", index + 1),
            Action::MoveToMonitor { index } => format!("Move to monitor {}", index + 1),
            Action::Retile => "Retile".into(),
            Action::Close => "Close window".into(),
            Action::TogglePause => "Toggle pause".into(),
            Action::OpenSettings => "Open settings".into(),
            Action::Quit => "Quit MinatoWM".into(),
        }
    }
}
