//! Single-instance guard. A second launch signals the first one to open
//! its settings window, then exits.

use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE,
    INFINITE,
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

const MUTEX_NAME: &str = r"Local\MinatoWM.Instance";
const EVENT_NAME: &str = r"Local\MinatoWM.OpenSettings";

pub enum Instance {
    /// We are the only instance. The mutex lives as long as the process.
    Primary,
    /// Another instance is running and has been asked to show its settings.
    Secondary,
}

pub fn acquire() -> Instance {
    unsafe {
        let mutex = CreateMutexW(std::ptr::null(), 0, wide(MUTEX_NAME).as_ptr());
        if !mutex.is_null() && GetLastError() == ERROR_ALREADY_EXISTS {
            let event = OpenEventW(EVENT_MODIFY_STATE, 0, wide(EVENT_NAME).as_ptr());
            if !event.is_null() {
                SetEvent(event);
            }
            return Instance::Secondary;
        }
        // The mutex handle is intentionally never closed: the OS releases it
        // when the process exits, which is exactly the lifetime we want.
        Instance::Primary
    }
}

/// Calls `f` every time another launch asks us to open the settings.
pub fn listen(f: impl Fn() + Send + 'static) {
    std::thread::Builder::new()
        .name("instance".into())
        .spawn(move || unsafe {
            let event = CreateEventW(std::ptr::null(), 0, 0, wide(EVENT_NAME).as_ptr());
            if event.is_null() {
                log::error!("could not create the instance event");
                return;
            }
            loop {
                WaitForSingleObject(event, INFINITE);
                f();
            }
        })
        .expect("spawning instance listener");
}
