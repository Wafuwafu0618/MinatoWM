//! Animation settings. Every animation kind refers to a named cubic bezier
//! curve, so one curve definition gives the whole desktop a shared feel.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Control points `[x1, y1, x2, y2]` of a CSS-style cubic bezier from
/// (0,0) to (1,1).
pub type CurvePoints = [f32; 4];

/// A named easing curve:
/// * a bezier `[x1, y1, x2, y2]`,
/// * a spring fitted to the animation's duration, `{ "spring": damping }`
///   (1.0 = no overshoot, lower = bouncier),
/// * a physical spring in Apple's model, `{ "apple_spring": { "duration":
///   0.5, "bounce": 0.0 } }` (SwiftUI / CASpringAnimation semantics). Its
///   length comes from the physics, so the animation's `duration_ms` is
///   ignored, and an interrupted animation keeps its velocity.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CurveSpec {
    Bezier(CurvePoints),
    Spring { spring: f32 },
    AppleSpring { apple_spring: AppleSpring },
}

/// Apple's spring parameters (WWDC23 "Animate with springs"): with mass 1,
/// stiffness = (2π / duration)² and damping ratio = 1 − bounce.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AppleSpring {
    /// Perceptual duration in seconds.
    pub duration: f32,
    /// 0 = no overshoot (critically damped), up to ~0.9 = very bouncy.
    #[serde(default)]
    pub bounce: f32,
}

impl CurveSpec {
    /// Whether the curve decides its own length (ignores `duration_ms`).
    pub fn is_physical(&self) -> bool {
        matches!(self, CurveSpec::AppleSpring { .. })
    }
}

/// Built-in curves, always available even if `curves` omits them.
pub const BUILTIN_CURVES: &[(&str, CurveSpec)] = &[
    // Apple's `.smooth` spring, used for the Spaces-style workspace slide.
    (
        "macos_spaces",
        CurveSpec::AppleSpring {
            apple_spring: AppleSpring {
                duration: 0.5,
                bounce: 0.0,
            },
        },
    ),
    // Critically damped spring: fast start, settles without overshoot.
    ("apple", CurveSpec::Spring { spring: 1.0 }),
    // A touch of overshoot.
    ("apple_bouncy", CurveSpec::Spring { spring: 0.8 }),
    ("linear", CurveSpec::Bezier([0.0, 0.0, 1.0, 1.0])),
    ("smooth", CurveSpec::Bezier([0.25, 0.1, 0.25, 1.0])),
    ("ease_out", CurveSpec::Bezier([0.0, 0.0, 0.2, 1.0])),
    ("ease_in_out", CurveSpec::Bezier([0.42, 0.0, 0.58, 1.0])),
    ("spaces", CurveSpec::Bezier([0.2, 0.9, 0.1, 1.0])),
];

pub const FALLBACK_CURVE: &str = "apple";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStyle {
    /// Push horizontally: higher workspaces come in from the right.
    #[default]
    Slide,
    /// Push vertically: higher workspaces come in from below.
    SlideVertical,
    /// Cross-fade in place.
    Fade,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutAnimation {
    pub enabled: bool,
    pub duration_ms: u32,
    /// Time the frozen picture stays fully opaque while apps repaint at
    /// their new size, before the fade starts.
    pub hold_ms: u32,
    pub curve: String,
}

impl Default for LayoutAnimation {
    fn default() -> Self {
        Self {
            enabled: true,
            duration_ms: 200,
            hold_ms: 16,
            curve: "apple".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceAnimation {
    pub enabled: bool,
    pub duration_ms: u32,
    pub curve: String,
    pub style: WorkspaceStyle,
}

impl Default for WorkspaceAnimation {
    fn default() -> Self {
        Self {
            enabled: true,
            duration_ms: 380,
            curve: "macos_spaces".into(),
            style: WorkspaceStyle::Slide,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BorderAnimation {
    pub enabled: bool,
    pub duration_ms: u32,
    pub curve: String,
}

impl Default for BorderAnimation {
    fn default() -> Self {
        Self {
            enabled: true,
            duration_ms: 120,
            curve: "apple".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnimationConfig {
    /// Master switch.
    pub enabled: bool,
    /// Extra or overridden named curves.
    pub curves: BTreeMap<String, CurveSpec>,
    /// Swaps, resizes, windows opening and closing: a cross-fade.
    pub layout: LayoutAnimation,
    pub workspace: WorkspaceAnimation,
    /// Focus border colour transitions.
    pub border: BorderAnimation,
}

impl Default for AnimationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            curves: BTreeMap::new(),
            layout: LayoutAnimation::default(),
            workspace: WorkspaceAnimation::default(),
            border: BorderAnimation::default(),
        }
    }
}

impl AnimationConfig {
    /// Resolves a curve name: user curves first, then built-ins, then the
    /// fallback curve.
    pub fn curve(&self, name: &str) -> CurveSpec {
        self.try_curve(name)
            .or_else(|| self.try_curve(FALLBACK_CURVE))
            .unwrap_or(CurveSpec::Spring { spring: 1.0 })
    }

    fn try_curve(&self, name: &str) -> Option<CurveSpec> {
        self.curves.get(name).copied().or_else(|| {
            BUILTIN_CURVES
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, p)| *p)
        })
    }

    /// All curve names, built-ins first.
    pub fn curve_names(&self) -> Vec<String> {
        let mut names: Vec<String> = BUILTIN_CURVES.iter().map(|(n, _)| n.to_string()).collect();
        for n in self.curves.keys() {
            if !names.contains(n) {
                names.push(n.clone());
            }
        }
        names
    }

    pub fn validate(&self, errors: &mut Vec<String>) {
        for (name, spec) in &self.curves {
            match spec {
                CurveSpec::Bezier(p) => {
                    if !(0.0..=1.0).contains(&p[0]) || !(0.0..=1.0).contains(&p[2]) {
                        errors.push(format!("curve {name:?}: x1 and x2 must be between 0 and 1"));
                    }
                    if p.iter().any(|v| !v.is_finite()) {
                        errors.push(format!("curve {name:?} has a non-finite value"));
                    }
                }
                CurveSpec::Spring { spring } => {
                    if !(0.1..=1.0).contains(spring) {
                        errors.push(format!("curve {name:?}: spring damping must be between 0.1 and 1.0"));
                    }
                }
                CurveSpec::AppleSpring { apple_spring: s } => {
                    if !(0.05..=5.0).contains(&s.duration) || !(0.0..=0.9).contains(&s.bounce) {
                        errors.push(format!(
                            "curve {name:?}: apple_spring needs duration 0.05–5 s and bounce 0–0.9"
                        ));
                    }
                }
            }
        }
        for (kind, name) in [
            ("layout", &self.layout.curve),
            ("workspace", &self.workspace.curve),
            ("border", &self.border.curve),
        ] {
            if self.try_curve(name).is_none() {
                errors.push(format!(
                    "animations.{kind}: unknown curve {name:?}, using {FALLBACK_CURVE:?}"
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_builtin_user_and_fallback() {
        let mut a = AnimationConfig::default();
        assert_eq!(a.curve("linear"), CurveSpec::Bezier([0.0, 0.0, 1.0, 1.0]));
        a.curves.insert("snappy".into(), CurveSpec::Bezier([0.1, 0.8, 0.2, 1.0]));
        a.curves.insert("linear".into(), CurveSpec::Spring { spring: 0.7 });
        assert_eq!(a.curve("snappy"), CurveSpec::Bezier([0.1, 0.8, 0.2, 1.0]));
        assert_eq!(a.curve("linear"), CurveSpec::Spring { spring: 0.7 });
        assert_eq!(a.curve("nope"), a.curve(FALLBACK_CURVE));
        assert!(a.curve_names().contains(&"snappy".to_string()));
    }

    #[test]
    fn validation() {
        let mut a = AnimationConfig::default();
        let mut errors = Vec::new();
        a.validate(&mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        a.layout.curve = "missing".into();
        a.curves.insert("bad".into(), CurveSpec::Bezier([1.5, 0.0, 0.5, 1.0]));
        a.curves.insert("wobbly".into(), CurveSpec::Spring { spring: 0.0 });
        a.validate(&mut errors);
        assert_eq!(errors.len(), 3, "{errors:?}");
    }

    #[test]
    fn parses_json() {
        let a: AnimationConfig = serde_json::from_str(
            r#"{ "curves": { "mine": [0.3, 0.0, 0.2, 1.0], "boing": { "spring": 0.6 }, "mac": { "apple_spring": { "duration": 0.4, "bounce": 0.1 } } },
                 "workspace": { "style": "slide_vertical", "curve": "mine" },
                 "border": { "enabled": false } }"#,
        )
        .unwrap();
        assert_eq!(a.workspace.style, WorkspaceStyle::SlideVertical);
        assert_eq!(a.workspace.duration_ms, 380);
        assert!(!a.border.enabled);
        assert!(a.layout.enabled);
        assert_eq!(a.curves["boing"], CurveSpec::Spring { spring: 0.6 });
        assert_eq!(
            a.curves["mac"],
            CurveSpec::AppleSpring { apple_spring: AppleSpring { duration: 0.4, bounce: 0.1 } }
        );
        assert!(a.curves["mac"].is_physical());
        assert_eq!(a.curves["mine"], CurveSpec::Bezier([0.3, 0.0, 0.2, 1.0]));
    }
}
