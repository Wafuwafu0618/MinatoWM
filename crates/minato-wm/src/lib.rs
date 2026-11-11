//! MinatoWM's window manager core.
//!
//! Threads:
//! * `wm` owns all state and handles [`WmMsg`]s one at a time,
//! * `winevent` pumps the WinEvent hook and forwards window events,
//! * `hotkey` pumps the low-level keyboard hook and forwards actions.

pub mod anim;
mod hidden;
pub mod hotkey;
pub mod perf;
mod snapshot;
mod state;
pub mod win32;
mod winevent;
mod wm;

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use arc_swap::ArcSwap;
use crossbeam_channel::Sender;
use minato_config::{Action, Config};

pub use snapshot::{ForegroundSnapshot, MonitorSnapshot, Snapshot, WindowSnapshot, WorkspaceSnapshot};
pub use winevent::WinEvent;

pub enum WmMsg {
    Event(WinEvent, win32::Hwnd),
    Action(Action),
    /// Show workspace `workspace` on monitor `monitor` (status bar clicks).
    FocusWorkspaceOn { monitor: usize, workspace: usize },
    /// A new valid config was loaded.
    Config(Arc<Config>),
    /// The config file changed but could not be parsed.
    ConfigError(String),
    /// From the control pipe: run `op`, then reply with the timing of the
    /// batch it ran in once that batch is fully done.
    Control {
        op: ControlOp,
        reply: Sender<Option<perf::BatchPerf>>,
    },
}

/// What a control-pipe request asks the WM thread to do.
#[derive(Debug, Clone)]
pub enum ControlOp {
    Ping,
    Action(Action),
    FocusWorkspaceOn { monitor: usize, workspace: usize },
}

impl WmMsg {
    fn describe(&self) -> String {
        match self {
            WmMsg::Event(ev, _) => format!("event:{ev:?}"),
            WmMsg::Action(a) => format!("action:{a:?}"),
            WmMsg::FocusWorkspaceOn { workspace, .. } => format!("bar:workspace {workspace}"),
            WmMsg::Config(_) => "config".into(),
            WmMsg::ConfigError(_) => "config_error".into(),
            WmMsg::Control { op, .. } => format!("control:{op:?}"),
        }
    }
}

/// Requests from the WM to the application shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiRequest {
    OpenSettings,
    /// The WM restored all windows and stopped; the app should exit.
    Quit,
}

pub struct WmHandle {
    tx: Sender<WmMsg>,
    pub snapshot: Arc<ArcSwap<Snapshot>>,
    thread: Option<JoinHandle<()>>,
}

impl WmHandle {
    pub fn send(&self, msg: WmMsg) {
        let _ = self.tx.send(msg);
    }

    pub fn action(&self, action: Action) {
        self.send(WmMsg::Action(action));
    }

    pub fn sender(&self) -> Sender<WmMsg> {
        self.tx.clone()
    }

    /// Asks the WM to restore every window and stop, then waits for it.
    pub fn stop(&mut self) {
        self.action(Action::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Starts the hooks and the WM thread. Must be called once, after
/// [`win32::set_dpi_awareness`].
pub fn start(cfg: Config, on_ui: impl Fn(UiRequest) + Send + 'static) -> anyhow::Result<WmHandle> {
    hidden::restore_leftovers();

    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        hidden::restore_from_mirror();
        prev(info);
    }));

    let (tx, rx) = crossbeam_channel::unbounded();
    hotkey::start(tx.clone(), hotkey::Keymap::default())?;
    winevent::start(tx.clone())?;

    let snapshot = Arc::new(ArcSwap::from_pointee(Snapshot::default()));
    let snap = snapshot.clone();
    let cfg = Arc::new(cfg);
    let thread = std::thread::Builder::new().name("wm".into()).spawn(move || {
        let animator = anim::Animator::start();
        let mut wm = wm::WindowManager::new(cfg, snap, Box::new(on_ui), animator);
        wm.init();
        'outer: while let Ok(msg) = rx.recv() {
            let started = Instant::now();
            perf::batch_begin(|| msg.describe());
            perf::batch_message();
            wm.handle(msg);
            // Coalesce bursts (e.g. an app opening several windows).
            while !wm.quit {
                match rx.try_recv() {
                    Ok(msg) => {
                        perf::batch_message();
                        wm.handle(msg);
                    }
                    Err(_) => break,
                }
            }
            if wm.quit {
                break 'outer;
            }
            let after = Instant::now();
            wm.after_batch();
            let record = perf::batch_end(started.elapsed(), after.elapsed());
            for reply in wm.control_replies.drain(..) {
                let _ = reply.send(record.clone());
            }
        }
        wm.shutdown();
        wm.notify_quit();
    })?;

    Ok(WmHandle {
        tx,
        snapshot,
        thread: Some(thread),
    })
}
