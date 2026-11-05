//! System-wide WinEvent hook. Runs on its own thread with a message pump;
//! the callback only translates events and forwards them to the WM thread.

use std::sync::OnceLock;

use crossbeam_channel::Sender;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, TranslateMessage, CHILDID_SELF, EVENT_OBJECT_CLOAKED,
    EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_SHOW,
    EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND,
    EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZESTART, MSG,
    OBJID_WINDOW, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

use crate::win32::Hwnd;
use crate::WmMsg;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WinEvent {
    Show,
    Hide,
    Destroy,
    Cloak,
    Uncloak,
    Foreground,
    MinimizeStart,
    MinimizeEnd,
    MoveSizeStart,
    MoveSizeEnd,
    NameChange,
}

impl WinEvent {
    fn from_raw(event: u32) -> Option<Self> {
        Some(match event {
            EVENT_OBJECT_SHOW => Self::Show,
            EVENT_OBJECT_HIDE => Self::Hide,
            EVENT_OBJECT_DESTROY => Self::Destroy,
            EVENT_OBJECT_CLOAKED => Self::Cloak,
            EVENT_OBJECT_UNCLOAKED => Self::Uncloak,
            EVENT_SYSTEM_FOREGROUND => Self::Foreground,
            EVENT_SYSTEM_MINIMIZESTART => Self::MinimizeStart,
            EVENT_SYSTEM_MINIMIZEEND => Self::MinimizeEnd,
            EVENT_SYSTEM_MOVESIZESTART => Self::MoveSizeStart,
            EVENT_SYSTEM_MOVESIZEEND => Self::MoveSizeEnd,
            EVENT_OBJECT_NAMECHANGE => Self::NameChange,
            _ => return None,
        })
    }
}

static SENDER: OnceLock<Sender<WmMsg>> = OnceLock::new();

unsafe extern "system" fn callback(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if id_object != OBJID_WINDOW || id_child != CHILDID_SELF as i32 {
        return;
    }
    let (Some(hwnd), Some(event)) = (Hwnd::from_raw(hwnd), WinEvent::from_raw(event)) else {
        return;
    };
    if let Some(tx) = SENDER.get() {
        let _ = tx.send(WmMsg::Event(event, hwnd));
    }
}

pub fn start(tx: Sender<WmMsg>) -> anyhow::Result<()> {
    SENDER
        .set(tx)
        .map_err(|_| anyhow::anyhow!("winevent hook already started"))?;
    std::thread::Builder::new()
        .name("winevent".into())
        .spawn(|| unsafe {
            let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
            // Two narrow ranges instead of everything: skips the very noisy
            // location-change and caret events in between.
            let ranges = [
                (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND),
                (EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE),
                (EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_NAMECHANGE),
                (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
            ];
            for (min, max) in ranges {
                let hook = SetWinEventHook(min, max, std::ptr::null_mut(), Some(callback), 0, 0, flags);
                if hook.is_null() {
                    log::error!("SetWinEventHook({min:#x}..{max:#x}) failed");
                }
            }
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })?;
    Ok(())
}
