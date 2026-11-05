use anyhow::{bail, Context};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

/// How a window property is compared. A plain JSON string means
/// [`MatchKind::Exact`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Matcher {
    Exact(String),
    Kind(MatchKind),
}

/// Comparisons are case-insensitive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    Exact(String),
    Contains(String),
    Prefix(String),
    Suffix(String),
    Regex(String),
}

impl Matcher {
    pub fn kind(&self) -> MatchKind {
        match self {
            Matcher::Exact(s) => MatchKind::Exact(s.clone()),
            Matcher::Kind(k) => k.clone(),
        }
    }

    pub fn pattern(&self) -> &str {
        match self {
            Matcher::Exact(s)
            | Matcher::Kind(
                MatchKind::Exact(s)
                | MatchKind::Contains(s)
                | MatchKind::Prefix(s)
                | MatchKind::Suffix(s)
                | MatchKind::Regex(s),
            ) => s,
        }
    }
}

/// Window properties a rule can match on. Every field that is set must
/// match; a rule with no fields never matches.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleMatch {
    /// Executable file name, e.g. `"Taskmgr.exe"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exe: Option<Matcher>,
    /// Full executable path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<Matcher>,
    /// Win32 window class name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class: Option<Matcher>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<Matcher>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleAction {
    /// Manage the window but never tile it.
    Float,
    /// Leave the window completely alone.
    Ignore,
    /// Tile the window on this workspace index of its monitor.
    Workspace(usize),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    #[serde(rename = "match")]
    pub matcher: RuleMatch,
    pub action: RuleAction,
}

/// What rules are evaluated against.
#[derive(Debug, Clone, Default)]
pub struct WindowInfo {
    pub exe: String,
    pub path: String,
    pub class: String,
    pub title: String,
}

enum Compiled {
    Exact(String),
    Contains(String),
    Prefix(String),
    Suffix(String),
    Regex(Regex),
}

impl Compiled {
    fn new(m: &Matcher) -> anyhow::Result<Self> {
        Ok(match m.kind() {
            MatchKind::Exact(s) => Self::Exact(s.to_lowercase()),
            MatchKind::Contains(s) => Self::Contains(s.to_lowercase()),
            MatchKind::Prefix(s) => Self::Prefix(s.to_lowercase()),
            MatchKind::Suffix(s) => Self::Suffix(s.to_lowercase()),
            MatchKind::Regex(s) => Self::Regex(
                RegexBuilder::new(&s)
                    .case_insensitive(true)
                    .build()
                    .with_context(|| format!("invalid regex {s:?}"))?,
            ),
        })
    }

    fn is_match(&self, value: &str) -> bool {
        match self {
            Compiled::Regex(re) => re.is_match(value),
            other => {
                let v = value.to_lowercase();
                match other {
                    Compiled::Exact(s) => v == *s,
                    Compiled::Contains(s) => v.contains(s.as_str()),
                    Compiled::Prefix(s) => v.starts_with(s.as_str()),
                    Compiled::Suffix(s) => v.ends_with(s.as_str()),
                    Compiled::Regex(_) => unreachable!(),
                }
            }
        }
    }
}

struct CompiledRule {
    exe: Option<Compiled>,
    path: Option<Compiled>,
    class: Option<Compiled>,
    title: Option<Compiled>,
    action: RuleAction,
}

impl CompiledRule {
    fn matches(&self, w: &WindowInfo) -> bool {
        let fields = [
            (&self.exe, &w.exe),
            (&self.path, &w.path),
            (&self.class, &w.class),
            (&self.title, &w.title),
        ];
        let mut any = false;
        for (m, value) in fields {
            if let Some(m) = m {
                if !m.is_match(value) {
                    return false;
                }
                any = true;
            }
        }
        any
    }
}

/// Rules compiled once per config load.
#[derive(Default)]
pub struct RuleSet(Vec<CompiledRule>);

impl RuleSet {
    pub fn compile(rules: &[Rule]) -> anyhow::Result<Self> {
        let opt = |m: &Option<Matcher>| m.as_ref().map(Compiled::new).transpose();
        let mut out = Vec::with_capacity(rules.len());
        for (i, r) in rules.iter().enumerate() {
            let m = &r.matcher;
            if m.exe.is_none() && m.path.is_none() && m.class.is_none() && m.title.is_none() {
                bail!("rule #{} has an empty match", i + 1);
            }
            let ctx = || format!("rule #{}", i + 1);
            out.push(CompiledRule {
                exe: opt(&m.exe).with_context(ctx)?,
                path: opt(&m.path).with_context(ctx)?,
                class: opt(&m.class).with_context(ctx)?,
                title: opt(&m.title).with_context(ctx)?,
                action: r.action,
            });
        }
        Ok(Self(out))
    }

    /// Like [`RuleSet::compile`] but drops rules that fail to compile.
    pub fn compile_lossy(rules: &[Rule]) -> Self {
        Self(
            rules
                .iter()
                .filter_map(|r| RuleSet::compile(std::slice::from_ref(r)).ok())
                .flat_map(|s| s.0)
                .collect(),
        )
    }

    /// The action of the first matching rule.
    pub fn evaluate(&self, w: &WindowInfo) -> Option<RuleAction> {
        self.0.iter().find(|r| r.matches(w)).map(|r| r.action)
    }
}

pub(crate) fn default_rules() -> Vec<Rule> {
    let rule = |matcher, action| Rule { matcher, action };
    vec![
        rule(
            RuleMatch {
                exe: Some(Matcher::Exact("Taskmgr.exe".into())),
                ..Default::default()
            },
            RuleAction::Float,
        ),
        rule(
            RuleMatch {
                // Standard Win32 dialog class.
                class: Some(Matcher::Exact("#32770".into())),
                ..Default::default()
            },
            RuleAction::Float,
        ),
        rule(
            RuleMatch {
                // Explorer's file copy / move progress window.
                class: Some(Matcher::Exact("OperationStatusWindow".into())),
                ..Default::default()
            },
            RuleAction::Float,
        ),
        rule(
            RuleMatch {
                title: Some(Matcher::Kind(MatchKind::Regex(
                    "^(Picture[- ]in[- ]Picture|ピクチャー イン ピクチャー)$".into(),
                ))),
                ..Default::default()
            },
            RuleAction::Ignore,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(exe: &str, class: &str, title: &str) -> WindowInfo {
        WindowInfo {
            exe: exe.into(),
            path: format!("C:\\Apps\\{exe}"),
            class: class.into(),
            title: title.into(),
        }
    }

    #[test]
    fn defaults() {
        let rs = RuleSet::compile(&default_rules()).unwrap();
        assert_eq!(
            rs.evaluate(&info("taskmgr.exe", "TaskManagerWindow", "Task Manager")),
            Some(RuleAction::Float)
        );
        assert_eq!(
            rs.evaluate(&info("firefox.exe", "MozillaDialogClass", "Picture-in-Picture")),
            Some(RuleAction::Ignore)
        );
        assert_eq!(
            rs.evaluate(&info("notepad.exe", "Notepad", "Untitled - Notepad")),
            None
        );
    }

    #[test]
    fn all_fields_must_match() {
        let rules = vec![Rule {
            matcher: RuleMatch {
                exe: Some(Matcher::Exact("code.exe".into())),
                title: Some(Matcher::Kind(MatchKind::Contains("settings".into()))),
                ..Default::default()
            },
            action: RuleAction::Workspace(2),
        }];
        let rs = RuleSet::compile(&rules).unwrap();
        assert_eq!(
            rs.evaluate(&info("Code.exe", "Chrome_WidgetWin_1", "Settings - VS Code")),
            Some(RuleAction::Workspace(2))
        );
        assert_eq!(rs.evaluate(&info("Code.exe", "x", "main.rs")), None);
    }

    #[test]
    fn matcher_kinds() {
        let m = |k: MatchKind, v: &str| Compiled::new(&Matcher::Kind(k)).unwrap().is_match(v);
        assert!(m(MatchKind::Prefix("micro".into()), "Microsoft Edge"));
        assert!(m(MatchKind::Suffix("EDGE".into()), "Microsoft Edge"));
        assert!(!m(MatchKind::Exact("edge".into()), "Microsoft Edge"));
        assert!(m(MatchKind::Regex(r"^\w+ edge$".into()), "Microsoft Edge"));
    }

    #[test]
    fn empty_and_invalid_rules_fail() {
        let empty = vec![Rule {
            matcher: RuleMatch::default(),
            action: RuleAction::Float,
        }];
        assert!(RuleSet::compile(&empty).is_err());
        let mut rules = default_rules();
        rules.insert(
            0,
            Rule {
                matcher: RuleMatch {
                    title: Some(Matcher::Kind(MatchKind::Regex("[".into()))),
                    ..Default::default()
                },
                action: RuleAction::Float,
            },
        );
        assert!(RuleSet::compile(&rules).is_err());
        // Lossy compilation keeps the valid ones.
        let rs = RuleSet::compile_lossy(&rules);
        assert_eq!(
            rs.evaluate(&info("Taskmgr.exe", "", "")),
            Some(RuleAction::Float)
        );
    }
}
