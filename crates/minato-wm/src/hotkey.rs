//! Built-in hotkeys: a low-level keyboard hook on its own thread.
//!
//! The hook callback must return quickly (Windows silently removes hooks
//! that take longer than ~300ms), so it only looks the chord up and sends
//! the action to the WM thread.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use arc_swap::ArcSwap;
use crossbeam_channel::Sender;
use minato_config::{Action, Hotkey};
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage,
    KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN,
    WM_SYSKEYUP,
};

use crate::WmMsg;

pub const MOD_ALT: u8 = 1;
pub const MOD_CTRL: u8 = 2;
pub const MOD_SHIFT: u8 = 4;
pub const MOD_WIN: u8 = 8;

/// An unassigned virtual key, injected to stop a swallowed Alt/Win combo
/// from opening the menu bar or the Start menu on release.
const VK_DUMMY: u16 = 0xE8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    pub mods: u8,
    pub vk: u16,
}

const MODIFIER_NAMES: &[(&str, u8)] = &[
    ("alt", MOD_ALT),
    ("ctrl", MOD_CTRL),
    ("control", MOD_CTRL),
    ("shift", MOD_SHIFT),
    ("win", MOD_WIN),
    ("super", MOD_WIN),
];

/// Non-alphanumeric key names. The first name for a code is the canonical
/// one used when formatting.
const KEY_NAMES: &[(&str, u16)] = &[
    ("enter", 0x0D),
    ("return", 0x0D),
    ("space", 0x20),
    ("tab", 0x09),
    ("escape", 0x1B),
    ("esc", 0x1B),
    ("backspace", 0x08),
    ("delete", 0x2E),
    ("insert", 0x2D),
    ("home", 0x24),
    ("end", 0x23),
    ("page_up", 0x21),
    ("page_down", 0x22),
    ("left", 0x25),
    ("up", 0x26),
    ("right", 0x27),
    ("down", 0x28),
    ("semicolon", 0xBA),
    ("equal", 0xBB),
    ("comma", 0xBC),
    ("minus", 0xBD),
    ("period", 0xBE),
    ("slash", 0xBF),
    ("backtick", 0xC0),
    ("open_bracket", 0xDB),
    ("backslash", 0xDC),
    ("close_bracket", 0xDD),
    ("quote", 0xDE),
    ("print_screen", 0x2C),
    ("pause", 0x13),
];

fn is_modifier_vk(vk: u16) -> bool {
    matches!(vk, 0x10..=0x12 | 0x5B | 0x5C | 0xA0..=0xA5)
}

fn key_from_name(name: &str) -> Option<u16> {
    let name = name.to_ascii_lowercase();
    let bytes = name.as_bytes();
    if bytes.len() == 1 && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit()) {
        return Some(bytes[0].to_ascii_uppercase() as u16);
    }
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<u16>().ok()) {
        if (1..=24).contains(&n) {
            return Some(0x70 + n - 1);
        }
    }
    if let Some(n) = name.strip_prefix("numpad").and_then(|n| n.parse::<u16>().ok()) {
        if n <= 9 {
            return Some(0x60 + n);
        }
    }
    KEY_NAMES.iter().find(|(n, _)| *n == name).map(|(_, vk)| *vk)
}

fn key_name(vk: u16) -> String {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => ((vk as u8) as char).to_ascii_lowercase().to_string(),
        0x70..=0x87 => format!("f{}", vk - 0x70 + 1),
        0x60..=0x69 => format!("numpad{}", vk - 0x60),
        _ => KEY_NAMES
            .iter()
            .find(|(_, v)| *v == vk)
            .map(|(n, _)| n.to_string())
            .unwrap_or_else(|| format!("vk{vk:#04x}")),
    }
}

impl Chord {
    /// Parses `"alt+shift+h"`. Modifiers may come in any order; exactly one
    /// non-modifier key is required.
    pub fn parse(s: &str) -> Result<Self, String> {
        let mut mods = 0;
        let mut vk = None;
        for part in s.split('+').map(str::trim) {
            if part.is_empty() {
                return Err(format!("{s:?}: empty key"));
            }
            let lower = part.to_ascii_lowercase();
            if let Some((_, m)) = MODIFIER_NAMES.iter().find(|(n, _)| *n == lower) {
                mods |= m;
            } else if vk.is_some() {
                return Err(format!("{s:?}: more than one non-modifier key"));
            } else {
                vk = Some(key_from_name(part).ok_or_else(|| format!("{s:?}: unknown key {part:?}"))?);
            }
        }
        let vk = vk.ok_or_else(|| format!("{s:?}: no key besides modifiers"))?;
        Ok(Self { mods, vk })
    }

    pub fn format(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for (name, bit) in [("win", MOD_WIN), ("ctrl", MOD_CTRL), ("alt", MOD_ALT), ("shift", MOD_SHIFT)] {
            if self.mods & bit != 0 {
                parts.push(name.into());
            }
        }
        parts.push(key_name(self.vk));
        parts.join("+")
    }
}

#[derive(Default)]
pub struct Keymap {
    bindings: HashMap<Chord, Action>,
}

impl Keymap {
    /// Builds a keymap, skipping (and reporting) unparsable or duplicate keys.
    pub fn build(hotkeys: &[Hotkey]) -> (Self, Vec<String>) {
        let mut bindings = HashMap::new();
        let mut errors = Vec::new();
        for hk in hotkeys {
            match Chord::parse(&hk.keys) {
                Ok(chord) => {
                    if bindings.insert(chord, hk.action).is_some() {
                        errors.push(format!("{:?} is bound more than once; the last binding wins", hk.keys));
                    }
                }
                Err(e) => errors.push(e),
            }
        }
        (Self { bindings }, errors)
    }

    pub fn get(&self, chord: &Chord) -> Option<Action> {
        self.bindings.get(chord).copied()
    }
}

type CaptureFn = Box<dyn FnOnce(String) + Send>;

struct HookState {
    tx: Sender<WmMsg>,
    keymap: ArcSwap<Keymap>,
    paused: AtomicBool,
    capture: Mutex<Option<CaptureFn>>,
}

static STATE: OnceLock<HookState> = OnceLock::new();

thread_local! {
    /// Keys whose key-down we swallowed; their key-up is swallowed too.
    static SWALLOWED: std::cell::RefCell<HashSet<u16>> = Default::default();
}

pub fn set_keymap(keymap: Keymap) {
    if let Some(s) = STATE.get() {
        s.keymap.store(Arc::new(keymap));
    }
}

/// While paused only `TogglePause` bindings fire.
pub fn set_paused(paused: bool) {
    if let Some(s) = STATE.get() {
        s.paused.store(paused, Ordering::Relaxed);
    }
}

/// Swallows the next key chord and hands its formatted form (e.g.
/// `"alt+shift+h"`) to `f` instead of running its binding. Used by the
/// settings UI to record hotkeys. Escape cancels without calling `f`.
pub fn capture_next(f: impl FnOnce(String) + Send + 'static) {
    if let Some(s) = STATE.get() {
        *s.capture.lock().unwrap() = Some(Box::new(f));
    }
}

pub fn cancel_capture() {
    if let Some(s) = STATE.get() {
        s.capture.lock().unwrap().take();
    }
}

fn key_down(vk: u16) -> bool {
    unsafe { GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0 }
}

fn current_mods() -> u8 {
    let mut m = 0;
    if key_down(0x12) {
        m |= MOD_ALT;
    }
    if key_down(0x11) {
        m |= MOD_CTRL;
    }
    if key_down(0x10) {
        m |= MOD_SHIFT;
    }
    if key_down(0x5B) || key_down(0x5C) {
        m |= MOD_WIN;
    }
    m
}

fn inject_dummy() {
    let key = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_DUMMY,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [key(0), key(KEYEVENTF_KEYUP)];
    unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
}

/// Decides whether the key-down is ours. Returns `true` to swallow it.
fn handle_key_down(state: &HookState, vk: u16) -> bool {
    let chord = Chord {
        mods: current_mods(),
        vk,
    };

    if let Ok(mut capture) = state.capture.try_lock() {
        if let Some(f) = capture.take() {
            if !(vk == 0x1B && chord.mods == 0) {
                f(chord.format());
            }
            return true;
        }
    }

    let Some(action) = state.keymap.load().get(&chord) else {
        return false;
    };
    if state.paused.load(Ordering::Relaxed) && action != Action::TogglePause {
        return false;
    }
    let _ = state.tx.send(WmMsg::Action(action));
    if chord.mods & (MOD_ALT | MOD_WIN) != 0 {
        inject_dummy();
    }
    true
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        if let Some(state) = STATE.get() {
            let info = &*(lparam as *const KBDLLHOOKSTRUCT);
            let vk = info.vkCode as u16;
            let injected = info.flags & LLKHF_INJECTED != 0;
            if !injected && !is_modifier_vk(vk) {
                let msg = wparam as u32;
                let swallow = match msg {
                    WM_KEYDOWN | WM_SYSKEYDOWN => {
                        let ours = handle_key_down(state, vk);
                        SWALLOWED.with(|s| {
                            let mut s = s.borrow_mut();
                            if ours {
                                s.insert(vk);
                            } else {
                                s.remove(&vk);
                            }
                        });
                        ours
                    }
                    WM_KEYUP | WM_SYSKEYUP => SWALLOWED.with(|s| s.borrow_mut().remove(&vk)),
                    _ => false,
                };
                if swallow {
                    return 1;
                }
            }
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}

pub fn start(tx: Sender<WmMsg>, keymap: Keymap) -> anyhow::Result<()> {
    STATE
        .set(HookState {
            tx,
            keymap: ArcSwap::from_pointee(keymap),
            paused: AtomicBool::new(false),
            capture: Mutex::new(None),
        })
        .map_err(|_| anyhow::anyhow!("hotkey hook already started"))?;

    std::thread::Builder::new()
        .name("hotkey".into())
        .spawn(|| unsafe {
            let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), GetModuleHandleW(std::ptr::null()), 0);
            if hook.is_null() {
                log::error!("SetWindowsHookExW(WH_KEYBOARD_LL) failed");
                return;
            }
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_chords() {
        assert_eq!(
            Chord::parse("alt+shift+h").unwrap(),
            Chord {
                mods: MOD_ALT | MOD_SHIFT,
                vk: 0x48
            }
        );
        assert_eq!(Chord::parse("Win + 1").unwrap(), Chord { mods: MOD_WIN, vk: 0x31 });
        assert_eq!(Chord::parse("ctrl+f12").unwrap().vk, 0x7B);
        assert_eq!(Chord::parse("alt+open_bracket").unwrap().vk, 0xDB);
        assert_eq!(Chord::parse("f5").unwrap(), Chord { mods: 0, vk: 0x74 });
        assert!(Chord::parse("alt+shift").is_err());
        assert!(Chord::parse("alt+h+j").is_err());
        assert!(Chord::parse("alt+nope").is_err());
        assert!(Chord::parse("alt++h").is_err());
    }

    #[test]
    fn format_round_trips() {
        for s in ["win+ctrl+alt+shift+h", "alt+enter", "f1", "alt+numpad3", "win+semicolon"] {
            assert_eq!(Chord::parse(s).unwrap().format(), s);
        }
        assert_eq!(Chord::parse("shift+alt+H").unwrap().format(), "alt+shift+h");
    }

    #[test]
    fn default_hotkeys_all_parse() {
        let (map, errors) = Keymap::build(&minato_config::default_hotkeys());
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            map.get(&Chord::parse("alt+h").unwrap()),
            Some(Action::Focus {
                dir: minato_layout::Direction::Left
            })
        );
    }
}
