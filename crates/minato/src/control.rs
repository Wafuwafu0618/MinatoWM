//! Control pipe for automation and benchmarks (`minato-bench`).
//!
//! Only started when `MINATO_CONTROL=1` is set. Speaks newline-delimited
//! JSON on `\\.\pipe\MinatoWM.Control`, one request and one response per
//! line:
//!
//! ```text
//! {"cmd":"action","action":{"type":"focus_workspace","index":7}}
//! {"cmd":"focus_workspace_on","monitor":0,"workspace":7}
//! {"cmd":"ping"}
//! {"cmd":"snapshot"}
//! {"cmd":"perf","clear":true}
//! {"cmd":"wait_idle","timeout_ms":3000}
//! ```

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::FromRawHandle;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use crossbeam_channel::Sender;
use minato_config::Action;
use minato_wm::{perf, ControlOp, Snapshot, WmMsg};
use serde::Deserialize;
use serde_json::{json, Value};
use windows_sys::Win32::Foundation::{GetLastError, ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_WAIT,
};

pub const PIPE_NAME: &str = r"\\.\pipe\MinatoWM.Control";
const WM_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Request {
    Action { action: Action },
    /// Shows `workspace` on monitor `monitor` (indices), focusing it.
    FocusWorkspaceOn { monitor: usize, workspace: usize },
    Ping,
    /// What pressing Alt/Win does: get the animation overlay ready.
    WarmUp,
    Snapshot,
    Perf {
        #[serde(default)]
        clear: bool,
    },
    WaitIdle {
        #[serde(default = "default_idle_timeout")]
        timeout_ms: u64,
    },
}

fn default_idle_timeout() -> u64 {
    3000
}

/// Whether `MINATO_CONTROL=1` asks for the control pipe.
pub fn requested() -> bool {
    std::env::var("MINATO_CONTROL").is_ok_and(|v| v == "1")
}

pub fn start(wm: Sender<WmMsg>, snapshot: Arc<ArcSwap<Snapshot>>) {
    perf::enable();
    let spawned = std::thread::Builder::new().name("control".into()).spawn(move || loop {
        match accept() {
            Some(pipe) => serve(pipe, &wm, &snapshot),
            None => std::thread::sleep(Duration::from_secs(1)),
        }
    });
    match spawned {
        Ok(_) => log::info!("control pipe listening on {PIPE_NAME}"),
        Err(e) => log::error!("control pipe: {e}"),
    }
}

/// Waits for the next client.
fn accept() -> Option<File> {
    let name: Vec<u16> = PIPE_NAME.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let h = CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            64 * 1024,
            64 * 1024,
            0,
            std::ptr::null(),
        );
        if h == INVALID_HANDLE_VALUE {
            log::warn!("CreateNamedPipe failed: {}", GetLastError());
            return None;
        }
        // The file owns (and eventually closes) the handle.
        let file = File::from_raw_handle(h as _);
        if ConnectNamedPipe(h, std::ptr::null_mut()) == 0 && GetLastError() != ERROR_PIPE_CONNECTED {
            return None;
        }
        Some(file)
    }
}

fn serve(pipe: File, wm: &Sender<WmMsg>, snapshot: &ArcSwap<Snapshot>) {
    let Ok(mut out) = pipe.try_clone() else { return };
    let reader = BufReader::new(pipe);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(req) => handle(req, wm, snapshot),
            Err(e) => json!({ "ok": false, "error": format!("bad request: {e}") }),
        };
        let mut text = response.to_string();
        text.push('\n');
        if out.write_all(text.as_bytes()).and_then(|_| out.flush()).is_err() {
            break;
        }
    }
}

/// Sends `action` (or a no-op) through the WM and waits for its batch.
fn round_trip(wm: &Sender<WmMsg>, op: ControlOp) -> Value {
    let (reply, rx) = crossbeam_channel::bounded(1);
    let sent = Instant::now();
    if wm.send(WmMsg::Control { op, reply }).is_err() {
        return json!({ "ok": false, "error": "window manager stopped" });
    }
    match rx.recv_timeout(WM_REPLY_TIMEOUT) {
        Ok(batch) => json!({
            "ok": true,
            "wm_ms": sent.elapsed().as_secs_f64() * 1000.0,
            // The clock perf records use, to line them up with the client's.
            "now_ms": perf::now_ms(),
            "batch": batch,
        }),
        Err(_) => json!({ "ok": false, "error": "window manager did not answer" }),
    }
}

fn handle(req: Request, wm: &Sender<WmMsg>, snapshot: &ArcSwap<Snapshot>) -> Value {
    match req {
        Request::Action { action } => round_trip(wm, ControlOp::Action(action)),
        Request::FocusWorkspaceOn { monitor, workspace } => {
            round_trip(wm, ControlOp::FocusWorkspaceOn { monitor, workspace })
        }
        Request::Ping => round_trip(wm, ControlOp::Ping),
        Request::WarmUp => {
            minato_wm::anim::warm_up();
            json!({ "ok": true })
        }
        Request::Snapshot => json!({ "ok": true, "snapshot": **snapshot.load() }),
        Request::Perf { clear } => {
            let (batches, anims) = perf::dump(clear);
            json!({ "ok": true, "batches": batches, "anims": anims })
        }
        Request::WaitIdle { timeout_ms } => {
            let start = Instant::now();
            let limit = Duration::from_millis(timeout_ms);
            loop {
                // The WM must have caught up too: its batch may start the
                // next animation.
                let ping = round_trip(wm, ControlOp::Ping);
                if ping["ok"] != true {
                    return ping;
                }
                if !perf::anim_busy() {
                    return json!({ "ok": true, "waited_ms": start.elapsed().as_secs_f64() * 1000.0 });
                }
                if start.elapsed() > limit {
                    return json!({ "ok": false, "error": "still animating", "waited_ms": timeout_ms });
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_requests() {
        let r: Request =
            serde_json::from_str(r#"{"cmd":"action","action":{"type":"focus_workspace","index":7}}"#).unwrap();
        assert!(matches!(r, Request::Action { action: Action::FocusWorkspace { index: 7 } }));
        let r: Request = serde_json::from_str(r#"{"cmd":"perf"}"#).unwrap();
        assert!(matches!(r, Request::Perf { clear: false }));
        let r: Request = serde_json::from_str(r#"{"cmd":"wait_idle"}"#).unwrap();
        assert!(matches!(r, Request::WaitIdle { timeout_ms: 3000 }));
        assert!(serde_json::from_str::<Request>(r#"{"cmd":"nope"}"#).is_err());
    }
}
