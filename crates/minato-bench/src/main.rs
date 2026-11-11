//! Benchmarks a running MinatoWM (started with `MINATO_CONTROL=1`).
//!
//! ```text
//! minato-bench run <scenario.json>... [--out result.json]
//! minato-bench ctl '<json request>'
//! ```
//!
//! A scenario opens its own test windows on spare workspaces, drives the WM
//! through the control pipe, and records how long each step took: how long
//! the WM thread was busy, until the test windows stopped changing, and
//! until the animation finished, plus the WM's own per-batch breakdown.

mod pipe;
mod report;
mod testwin;

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use minato_config::Action;
use minato_layout::Rect;
use minato_wm::win32::Hwnd;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use pipe::Client;

#[derive(Debug, Deserialize)]
struct Scenario {
    name: String,
    /// Workspaces the scenario uses; they must hold no other windows.
    workspaces: Vec<usize>,
    steps: Vec<Step>,
}

fn default_interval() -> u64 {
    700
}

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Step {
    /// How slowly the test windows react from now on.
    Delay {
        #[serde(default)]
        paint_ms: u32,
        #[serde(default)]
        size_ms: u32,
    },
    Action {
        action: Action,
        label: Option<String>,
    },
    /// Shows a workspace on the scenario's monitor.
    Workspace {
        index: usize,
        label: Option<String>,
    },
    /// Opens `count` test windows one at a time.
    Open {
        count: usize,
        label: Option<String>,
    },
    /// Closes the `count` most recently opened test windows one at a time.
    Close {
        count: usize,
        label: Option<String>,
    },
    CloseAll,
    /// Sends `actions` in turn, `repeat` times in total. With `wait`, each
    /// one is measured to completion before the next; without, they are
    /// sent every `interval_ms` regardless (key mashing).
    Cycle {
        #[serde(default)]
        actions: Vec<Action>,
        /// Workspaces of the scenario's monitor, as an alternative to
        /// `actions`.
        #[serde(default)]
        workspaces: Vec<usize>,
        repeat: usize,
        #[serde(default = "default_interval")]
        interval_ms: u64,
        #[serde(default = "yes")]
        wait: bool,
        /// Simulate a hotkey: "press Alt" this long before each command.
        warm_up_ms: Option<u64>,
        label: Option<String>,
    },
    Sleep {
        ms: u64,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct Sample {
    pub label: String,
    pub i: usize,
    /// Time the WM took to handle the request (only for actions).
    pub wm_ms: Option<f64>,
    /// Until the test windows stopped moving, appearing and repainting.
    pub settle_ms: f64,
    /// Until the first animation of the step started moving.
    pub anim_start_ms: Option<f64>,
    /// Until the last animation of the step finished.
    pub anim_end_ms: Option<f64>,
    pub batches: Vec<Value>,
    pub anims: Vec<Value>,
}

/// How long nothing may change before a step counts as settled.
const QUIET: Duration = Duration::from_millis(150);
const SETTLE_TIMEOUT: Duration = Duration::from_secs(4);

/// One thing to ask the WM.
enum Command {
    Action(Action),
    Workspace(usize),
}

struct Runner {
    client: Client,
    /// The monitor everything happens on (index as the WM counts them).
    monitor: usize,
    epoch: Instant,
    windows: Vec<Hwnd>,
    opened: usize,
    area: Rect,
    samples: Vec<Sample>,
}

impl Runner {
    fn now(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64() * 1000.0
    }

    fn action(&mut self, action: &Action) -> Result<f64> {
        self.command(&Command::Action(*action))
    }

    fn command(&mut self, cmd: &Command) -> Result<f64> {
        let req = match cmd {
            Command::Action(a) => json!({"cmd": "action", "action": a}),
            Command::Workspace(w) => {
                json!({"cmd": "focus_workspace_on", "monitor": self.monitor, "workspace": w})
            }
        };
        let resp = self.client.call(req)?;
        Ok(resp["wm_ms"].as_f64().unwrap_or(0.0))
    }

    /// Waits until no test window changed position, visibility or paint
    /// for [`QUIET`]; returns when the last change happened (our clock).
    fn wait_settle(&self, t0: f64) -> f64 {
        let state = |ws: &[Hwnd]| -> Vec<(Option<Rect>, bool)> {
            ws.iter().map(|h| (h.frame_rect(), h.is_visible())).collect()
        };
        let mut last = state(&self.windows);
        let mut last_change = t0;
        let start = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(2));
            let now = self.now();
            let cur = state(&self.windows);
            if cur != last {
                last = cur;
                last_change = now;
            }
            let latest = last_change.max(testwin::last_activity_ms());
            if now - latest >= QUIET.as_secs_f64() * 1000.0 || start.elapsed() > SETTLE_TIMEOUT {
                return latest.max(t0);
            }
        }
    }

    /// Settles, waits for animations, and collects the WM's records since
    /// the last collection.
    fn finish(&mut self, label: &Option<String>, i: usize, t0: f64, wm_ms: Option<f64>) -> Result<()> {
        let settled = self.wait_settle(t0);
        self.client.call(json!({"cmd": "wait_idle", "timeout_ms": 5000}))?;
        let perf = self.client.call(json!({"cmd": "perf", "clear": true}))?;
        let Some(label) = label else {
            return Ok(());
        };
        let anims: Vec<Value> = perf["anims"].as_array().cloned().unwrap_or_default();
        let anim_end_ms = anims
            .iter()
            .filter_map(|a| a["t_ms"].as_f64())
            .map(|t| self.client.to_local(t) - t0)
            .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))));
        let anim_start_ms = anims
            .iter()
            .filter_map(|a| a["start_ms"].as_f64())
            .map(|t| self.client.to_local(t) - t0)
            .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.min(v))));
        self.samples.push(Sample {
            label: label.clone(),
            i,
            wm_ms,
            settle_ms: settled - t0,
            anim_start_ms,
            anim_end_ms,
            batches: perf["batches"].as_array().cloned().unwrap_or_default(),
            anims,
        });
        Ok(())
    }

    fn step(&mut self, step: &Step) -> Result<()> {
        match step {
            Step::Delay { paint_ms, size_ms } => {
                testwin::PAINT_MS.store(*paint_ms, std::sync::atomic::Ordering::Relaxed);
                testwin::SIZE_MS.store(*size_ms, std::sync::atomic::Ordering::Relaxed);
            }
            Step::Action { action, label } => {
                let t0 = self.now();
                let wm = self.action(action)?;
                self.finish(label, 0, t0, Some(wm))?;
            }
            Step::Workspace { index, label } => {
                let t0 = self.now();
                let wm = self.command(&Command::Workspace(*index))?;
                self.finish(label, 0, t0, Some(wm))?;
            }
            Step::Open { count, label } => {
                for i in 0..*count {
                    let t0 = self.now();
                    self.opened += 1;
                    let h = testwin::spawn(self.opened, self.area);
                    self.windows.push(h);
                    self.finish(label, i, t0, None)?;
                }
            }
            Step::Close { count, label } => {
                for i in 0..*count {
                    let Some(h) = self.windows.pop() else { break };
                    let t0 = self.now();
                    h.close();
                    self.finish(label, i, t0, None)?;
                }
            }
            Step::CloseAll => self.close_all(),
            Step::Cycle {
                actions,
                workspaces,
                repeat,
                interval_ms,
                wait,
                warm_up_ms,
                label,
            } => {
                let cmds: Vec<Command> = actions
                    .iter()
                    .map(|a| Command::Action(*a))
                    .chain(workspaces.iter().map(|w| Command::Workspace(*w)))
                    .collect();
                if cmds.is_empty() {
                    bail!("cycle without actions or workspaces");
                }
                let interval = Duration::from_millis(*interval_ms);
                let burst_start = self.now();
                let mut wm_total = 0.0;
                for i in 0..*repeat {
                    let started = Instant::now();
                    if let Some(ms) = warm_up_ms {
                        self.client.call(json!({"cmd": "warm_up"}))?;
                        std::thread::sleep(Duration::from_millis(*ms));
                    }
                    let t0 = self.now();
                    let wm = self.command(&cmds[i % cmds.len()])?;
                    if *wait {
                        self.finish(label, i, t0, Some(wm))?;
                    } else {
                        wm_total += wm;
                    }
                    if let Some(rest) = interval.checked_sub(started.elapsed()) {
                        std::thread::sleep(rest);
                    }
                }
                if !*wait {
                    // One sample for the whole burst.
                    let avg = wm_total / *repeat as f64;
                    self.finish(label, 0, burst_start, Some(avg))?;
                }
            }
            Step::Sleep { ms } => std::thread::sleep(Duration::from_millis(*ms)),
        }
        Ok(())
    }

    fn close_all(&mut self) {
        for h in self.windows.drain(..) {
            h.close();
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// Index and data of the focused monitor.
fn focused_monitor(snapshot: &Value) -> Result<(usize, &Value)> {
    let ms = snapshot["monitors"].as_array().context("the WM reports no monitors")?;
    let i = ms.iter().position(|m| m["focused"] == true).unwrap_or(0);
    Ok((i, ms.get(i).context("the WM reports no monitors")?))
}

fn run_scenario(path: &str, epoch: Instant) -> Result<(String, Vec<Sample>)> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let scenario: Scenario = serde_json::from_str(&text).with_context(|| format!("parsing {path}"))?;
    let mut client = Client::connect(epoch)?;
    let snap = client.call(json!({"cmd": "snapshot"}))?["snapshot"].clone();
    let (mi, monitor) = focused_monitor(&snap)?;
    // Where every monitor was, to put everything back afterwards.
    let original: Vec<usize> = snap["monitors"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| m["active_workspace"].as_u64().unwrap_or(0) as usize)
        .collect();
    let wa = &monitor["work_area"];
    let area = Rect::new(
        wa["x"].as_i64().unwrap_or(0) as i32,
        wa["y"].as_i64().unwrap_or(0) as i32,
        wa["w"].as_i64().unwrap_or(1280) as i32,
        wa["h"].as_i64().unwrap_or(720) as i32,
    );
    // Never move the user's own windows around.
    for &wi in &scenario.workspaces {
        let busy = monitor["workspaces"][wi]["windows"].as_array().is_some_and(|w| !w.is_empty());
        if busy {
            bail!("workspace {} is not empty; the scenario needs it to itself", wi + 1);
        }
    }
    if snap["paused"] == true {
        bail!("MinatoWM is paused");
    }
    eprintln!("== {} ({path}) on monitor {}", scenario.name, mi + 1);
    client.call(json!({"cmd": "perf", "clear": true}))?;
    let mut runner = Runner {
        client,
        monitor: mi,
        epoch,
        windows: Vec::new(),
        opened: 0,
        area,
        samples: Vec::new(),
    };
    let mut result = Ok(());
    for step in &scenario.steps {
        if let Err(e) = runner.step(step) {
            result = Err(e);
            break;
        }
    }
    // Always clean up: close our windows and go back where the user was.
    runner.close_all();
    for (m, ws) in original.iter().enumerate() {
        let _ = runner
            .client
            .call(json!({"cmd": "focus_workspace_on", "monitor": m, "workspace": ws}));
    }
    let _ = runner.action(&Action::FocusMonitor { index: mi });
    let _ = runner.client.call(json!({"cmd": "wait_idle"}));
    result?;
    Ok((scenario.name, runner.samples))
}

fn main() -> Result<()> {
    minato_wm::win32::set_dpi_awareness();
    let epoch = testwin::epoch();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("ctl") => {
            let req: Value = serde_json::from_str(args.get(1).context("ctl needs a JSON request")?)?;
            let mut client = Client::connect(epoch)?;
            println!("{}", serde_json::to_string_pretty(&client.request(&req)?)?);
        }
        Some("run") => {
            let mut files = Vec::new();
            let mut out = None;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                if a == "--out" {
                    out = it.next().cloned();
                } else {
                    files.push(a.clone());
                }
            }
            if files.is_empty() {
                bail!("run needs at least one scenario file");
            }
            let mut all = Vec::new();
            for f in &files {
                // One failing scenario shouldn't lose the others' results.
                match run_scenario(f, epoch) {
                    Ok(r) => all.push(r),
                    Err(e) => eprintln!("{f}: {e:#}"),
                }
            }
            let doc = report::document(&all);
            println!("{}", report::markdown(&doc));
            if let Some(out) = out {
                std::fs::write(&out, serde_json::to_string_pretty(&doc)?)?;
                eprintln!("wrote {out}");
            }
        }
        _ => {
            eprintln!("usage: minato-bench run <scenario.json>... [--out result.json]");
            eprintln!("       minato-bench ctl '<json request>'");
        }
    }
    Ok(())
}
