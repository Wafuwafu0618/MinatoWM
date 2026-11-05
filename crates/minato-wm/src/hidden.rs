//! Crash safety for hidden windows.
//!
//! Windows on background workspaces are hidden with `SW_HIDE`. If MinatoWM
//! dies without restoring them they would stay invisible, so the set is
//! mirrored in memory (for the panic hook) and on disk (for the next start).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::win32::Hwnd;

/// `(hwnd, pid)`: the pid guards against a recycled handle that now
/// belongs to some unrelated, intentionally hidden window.
static MIRROR: Mutex<Vec<(isize, u32)>> = Mutex::new(Vec::new());

fn path() -> PathBuf {
    minato_config::config_dir().join("hidden.json")
}

pub fn persist(hidden: &HashSet<Hwnd>) {
    let list: Vec<(isize, u32)> = hidden.iter().map(|h| (h.0, h.process_id())).collect();
    if let Ok(mut m) = MIRROR.lock() {
        m.clone_from(&list);
    }
    let p = path();
    let result = if list.is_empty() {
        match std::fs::remove_file(&p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        std::fs::create_dir_all(p.parent().unwrap())
            .and_then(|_| std::fs::write(&p, serde_json::to_string(&list).unwrap()))
    };
    if let Err(e) = result {
        log::warn!("could not persist hidden windows: {e}");
    }
}

/// Shows every window in the in-memory mirror. Safe to call from a panic hook.
pub fn restore_from_mirror() {
    let list = match MIRROR.lock() {
        Ok(m) => m.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    for (h, pid) in list {
        let h = Hwnd(h);
        if h.exists() && h.process_id() == pid {
            h.show();
        }
    }
}

/// Shows windows a previous, crashed instance left hidden.
pub fn restore_leftovers() {
    let p = path();
    let Ok(text) = std::fs::read_to_string(&p) else {
        return;
    };
    let list: Vec<(isize, u32)> = serde_json::from_str(&text).unwrap_or_default();
    let mut restored = 0;
    for (h, pid) in list {
        let h = Hwnd(h);
        if h.exists() && !h.is_visible() && h.process_id() == pid {
            h.show();
            restored += 1;
        }
    }
    if restored > 0 {
        log::warn!("restored {restored} window(s) hidden by a previous run");
    }
    let _ = std::fs::remove_file(p);
}
