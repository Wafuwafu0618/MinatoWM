//! Status bar settings.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BarConfig {
    pub enabled: bool,
    /// Height in logical pixels (scaled by each monitor's DPI).
    pub height: u32,
    pub left: Vec<BarModule>,
    pub center: Vec<BarModule>,
    pub right: Vec<BarModule>,
}

impl BarConfig {
    pub const MIN_HEIGHT: u32 = 16;
    pub const MAX_HEIGHT: u32 = 64;

    /// The height reserved at the top of each monitor, in physical pixels
    /// for a monitor with the given DPI (96 = 100%).
    pub fn reserved_px(&self, dpi: u32) -> i32 {
        if !self.enabled {
            return 0;
        }
        let h = self.height.clamp(Self::MIN_HEIGHT, Self::MAX_HEIGHT);
        (h as f32 * dpi.max(96) as f32 / 96.0).round() as i32
    }
}

impl Default for BarConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            height: 28,
            left: vec![BarModule::Workspaces, BarModule::Layout],
            center: vec![BarModule::Title],
            right: vec![BarModule::Cpu, BarModule::Memory, BarModule::Clock { format: None }],
        }
    }
}

/// One item of the bar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BarModule {
    /// The monitor's workspaces; click one to switch to it.
    Workspaces,
    /// Layout of the active workspace (and monocle state).
    Layout,
    /// Title of the focused window, on the monitor it is on.
    Title,
    /// Local time. `format` uses `%Y %m %d %H %M %S %a`
    /// (default `%m/%d (%a) %H:%M`).
    Clock {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
    },
    /// Total CPU usage.
    Cpu,
    /// Physical memory in use.
    Memory,
    /// Fixed text.
    Text { text: String },
}

impl BarModule {
    pub const DEFAULT_CLOCK: &'static str = "%m/%d (%a) %H:%M";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_scales_with_dpi() {
        let b = BarConfig::default();
        assert_eq!(b.reserved_px(96), 28);
        assert_eq!(b.reserved_px(144), 42);
        let off = BarConfig {
            enabled: false,
            ..BarConfig::default()
        };
        assert_eq!(off.reserved_px(144), 0);
    }

    #[test]
    fn modules_parse() {
        let b: BarConfig = serde_json::from_str(
            r#"{ "height": 30, "left": [{ "type": "workspaces" }],
                 "right": [{ "type": "clock", "format": "%H:%M:%S" }, { "type": "text", "text": "hi" }] }"#,
        )
        .unwrap();
        assert_eq!(b.height, 30);
        assert_eq!(b.left, vec![BarModule::Workspaces]);
        assert_eq!(
            b.right[0],
            BarModule::Clock {
                format: Some("%H:%M:%S".into())
            }
        );
        // Sections the file omits keep their defaults.
        assert_eq!(b.center, BarConfig::default().center);
    }
}
