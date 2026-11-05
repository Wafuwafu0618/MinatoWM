//! Thin, safe wrappers over the Win32 calls the window manager needs.
//!
//! Window handles are stored as `isize` so they are `Send` and hashable;
//! every call converts back to the raw pointer type at the FFI boundary.

mod monitor;
pub mod sys;

pub use monitor::{monitor_at_point, monitors, MonitorInfo};

use std::ffi::c_void;
use std::mem::size_of;

use minato_config::{Color, WindowInfo};
use minato_layout::Rect;
use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CLOAKED,
    DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows_sys::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONEAREST};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetCursorPos, GetForegroundWindow, GetShellWindow, GetWindow,
    GetWindowLongPtrW,
    GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindow, IsWindowVisible, IsZoomed, PostMessageW, SetCursorPos, SetForegroundWindow,
    SetWindowPos, ShowWindow, GWL_EXSTYLE, GWL_STYLE, GW_OWNER, SWP_ASYNCWINDOWPOS,
    SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER, SW_HIDE, SW_RESTORE, SW_SHOWNOACTIVATE,
    WM_CLOSE, WS_CAPTION, WS_CHILD, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
    WS_THICKFRAME,
};

/// `DWMWA_COLOR_DEFAULT`: reset a DWM colour attribute to the system value.
const DWM_COLOR_DEFAULT: u32 = 0xFFFF_FFFF;

/// Window classes that are never application windows.
const IGNORED_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Windows.UI.Core.CoreWindow",
    "XamlExplorerHostIslandWindow",
    "ForegroundStaging",
    "MultitaskingViewFrame",
    "TaskListThumbnailWnd",
    "Chrome_RenderWidgetHostHWND",
    "NotifyIconOverflowWindow",
    "TopLevelWindowForOverflowXamlIsland",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Hwnd(pub isize);

fn to_rect(r: RECT) -> Rect {
    Rect::from_edges(r.left, r.top, r.right, r.bottom)
}

fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

impl Hwnd {
    pub fn raw(self) -> HWND {
        self.0 as HWND
    }

    pub fn from_raw(h: HWND) -> Option<Self> {
        (!h.is_null()).then_some(Self(h as isize))
    }

    pub fn foreground() -> Option<Self> {
        Self::from_raw(unsafe { GetForegroundWindow() })
    }

    /// The desktop (shell) window; focusing it leaves no app focused.
    pub fn desktop() -> Option<Self> {
        Self::from_raw(unsafe { GetShellWindow() })
    }

    pub fn exists(self) -> bool {
        unsafe { IsWindow(self.raw()) != 0 }
    }

    pub fn is_visible(self) -> bool {
        unsafe { IsWindowVisible(self.raw()) != 0 }
    }

    pub fn is_minimized(self) -> bool {
        unsafe { IsIconic(self.raw()) != 0 }
    }

    pub fn is_maximized(self) -> bool {
        unsafe { IsZoomed(self.raw()) != 0 }
    }

    pub fn is_cloaked(self) -> bool {
        let mut cloaked: u32 = 0;
        let hr = unsafe {
            DwmGetWindowAttribute(
                self.raw(),
                DWMWA_CLOAKED as u32,
                &mut cloaked as *mut u32 as *mut c_void,
                size_of::<u32>() as u32,
            )
        };
        hr >= 0 && cloaked != 0
    }

    pub fn style(self) -> u32 {
        unsafe { GetWindowLongPtrW(self.raw(), GWL_STYLE) as u32 }
    }

    pub fn ex_style(self) -> u32 {
        unsafe { GetWindowLongPtrW(self.raw(), GWL_EXSTYLE) as u32 }
    }

    pub fn owner(self) -> Option<Hwnd> {
        Self::from_raw(unsafe { GetWindow(self.raw(), GW_OWNER) })
    }

    pub fn title(self) -> String {
        unsafe {
            let len = GetWindowTextLengthW(self.raw());
            if len <= 0 {
                return String::new();
            }
            let mut buf = vec![0u16; len as usize + 1];
            let n = GetWindowTextW(self.raw(), buf.as_mut_ptr(), buf.len() as i32);
            String::from_utf16_lossy(&buf[..n.max(0) as usize])
        }
    }

    pub fn class(self) -> String {
        let mut buf = [0u16; 256];
        let n = unsafe { GetClassNameW(self.raw(), buf.as_mut_ptr(), buf.len() as i32) };
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }

    pub fn process_id(self) -> u32 {
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(self.raw(), &mut pid) };
        pid
    }

    /// Full path of the owning process's executable, or empty if it can't
    /// be queried (e.g. an elevated process).
    pub fn exe_path(self) -> String {
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, self.process_id());
            if process.is_null() {
                return String::new();
            }
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
            CloseHandle(process);
            if ok == 0 {
                return String::new();
            }
            String::from_utf16_lossy(&buf[..len as usize])
        }
    }

    pub fn info(self) -> WindowInfo {
        let path = self.exe_path();
        let exe = path.rsplit(['\\', '/']).next().unwrap_or_default().to_string();
        WindowInfo {
            exe,
            path,
            class: self.class(),
            title: self.title(),
        }
    }

    /// The outer window rect, including the invisible resize borders.
    pub fn window_rect(self) -> Option<Rect> {
        let mut r: RECT = unsafe { std::mem::zeroed() };
        (unsafe { GetWindowRect(self.raw(), &mut r) } != 0).then(|| to_rect(r))
    }

    /// The visible frame as DWM draws it, without the invisible borders.
    pub fn frame_rect(self) -> Option<Rect> {
        let mut r: RECT = unsafe { std::mem::zeroed() };
        let hr = unsafe {
            DwmGetWindowAttribute(
                self.raw(),
                DWMWA_EXTENDED_FRAME_BOUNDS as u32,
                &mut r as *mut RECT as *mut c_void,
                size_of::<RECT>() as u32,
            )
        };
        if hr >= 0 {
            Some(to_rect(r))
        } else {
            self.window_rect()
        }
    }

    pub fn monitor(self) -> isize {
        unsafe { MonitorFromWindow(self.raw(), MONITOR_DEFAULTTONEAREST) as isize }
    }

    /// Whether this looks like a normal top-level application window that
    /// should be tiled or floated. Rules are applied separately.
    pub fn is_app_window(self) -> bool {
        if !self.exists() || !self.is_visible() || self.is_minimized() || self.is_cloaked() {
            return false;
        }
        if self.owner().is_some() || self.process_id() == unsafe { GetCurrentProcessId() } {
            return false;
        }
        let style = self.style();
        let ex = self.ex_style();
        if style & WS_CHILD != 0 {
            return false;
        }
        let has_frame = style & WS_CAPTION == WS_CAPTION || style & WS_THICKFRAME != 0;
        if !has_frame || ex & (WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT) != 0 {
            return false;
        }
        if self.title().trim().is_empty() {
            return false;
        }
        let class = self.class();
        if IGNORED_CLASSES.contains(&class.as_str()) {
            return false;
        }
        matches!(self.frame_rect(), Some(r) if r.w >= 32 && r.h >= 32)
    }

    /// Places the window so its *visible* frame matches `target`.
    pub fn set_position(self, target: Rect) {
        if self.is_maximized() || self.is_minimized() {
            unsafe { ShowWindow(self.raw(), SW_RESTORE) };
        }
        let (Some(outer), Some(frame)) = (self.window_rect(), self.frame_rect()) else {
            return;
        };
        if frame == target {
            return;
        }
        // Invisible borders around the visible frame.
        let left = frame.x - outer.x;
        let top = frame.y - outer.y;
        let right = outer.right() - frame.right();
        let bottom = outer.bottom() - frame.bottom();
        unsafe {
            SetWindowPos(
                self.raw(),
                std::ptr::null_mut(),
                target.x - left,
                target.y - top,
                target.w + left + right,
                target.h + top + bottom,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS,
            );
        }
    }

    /// Styles one of our status bar windows: clicks don't activate it,
    /// it stays out of Alt+Tab, and DWM draws no rounded corners or frame.
    pub fn set_bar_style(self) {
        use windows_sys::Win32::Graphics::Dwm::{
            DWMWA_COLOR_NONE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW;
        let ex = self.ex_style() | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW;
        unsafe {
            SetWindowLongPtrW(self.raw(), GWL_EXSTYLE, ex as isize);
            let corner = DWMWCP_DONOTROUND;
            DwmSetWindowAttribute(
                self.raw(),
                DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                &corner as *const _ as *const c_void,
                size_of_val(&corner) as u32,
            );
            let none = DWMWA_COLOR_NONE;
            DwmSetWindowAttribute(
                self.raw(),
                DWMWA_BORDER_COLOR as u32,
                &none as *const _ as *const c_void,
                size_of_val(&none) as u32,
            );
        }
    }

    pub fn hide(self) {
        unsafe { ShowWindow(self.raw(), SW_HIDE) };
    }

    pub fn show(self) {
        unsafe { ShowWindow(self.raw(), SW_SHOWNOACTIVATE) };
    }

    /// Politely asks the window to close, like clicking its X button.
    pub fn close(self) {
        unsafe { PostMessageW(self.raw(), WM_CLOSE, 0, 0) };
    }

    pub fn focus(self) {
        unsafe {
            if self.is_minimized() {
                ShowWindow(self.raw(), SW_RESTORE);
            }
            if GetForegroundWindow() == self.raw() {
                return;
            }
            // Windows only lets the process that received the last input
            // event change the foreground window. A zero-length mouse move
            // makes that us.
            let input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: 0,
                        dwFlags: MOUSEEVENTF_MOVE,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            SendInput(1, &input, size_of::<INPUT>() as i32);
            SetForegroundWindow(self.raw());
        }
    }

    /// Sets the Windows 11 DWM frame colour, `None` restores the default.
    pub fn set_border_color(self, color: Option<Color>) {
        let value = color.map_or(DWM_COLOR_DEFAULT, Color::to_colorref);
        unsafe {
            DwmSetWindowAttribute(
                self.raw(),
                DWMWA_BORDER_COLOR as u32,
                &value as *const u32 as *const c_void,
                size_of::<u32>() as u32,
            );
        }
    }
}

/// All top-level windows in z-order (topmost first).
pub fn top_level_windows() -> Vec<Hwnd> {
    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> i32 {
        let out = &mut *(lparam as *mut Vec<Hwnd>);
        out.push(Hwnd(hwnd as isize));
        1
    }
    let mut out: Vec<Hwnd> = Vec::new();
    unsafe { EnumWindows(Some(collect), &mut out as *mut Vec<Hwnd> as LPARAM) };
    out
}

pub fn cursor_pos() -> (i32, i32) {
    let mut p = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut p) };
    (p.x, p.y)
}

pub fn set_cursor_pos(x: i32, y: i32) {
    unsafe { SetCursorPos(x, y) };
}

/// Re-shows a cursor hidden by "Hide pointer while typing": that setting
/// keeps the pointer hidden until the mouse moves, and the hotkey that
/// switched workspaces counts as typing. Moving the cursor one pixel and
/// straight back counts as mouse movement without visibly moving it.
pub fn nudge_cursor() {
    let (x, y) = cursor_pos();
    let away = if monitor_at_point(x + 1, y) == monitor_at_point(x, y) { x + 1 } else { x - 1 };
    set_cursor_pos(away, y);
    set_cursor_pos(x, y);
}

/// The Windows "Animation effects" accessibility setting.
pub fn system_animations_enabled() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION,
    };
    let mut on: i32 = 1;
    let ok = unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, &mut on as *mut i32 as *mut c_void, 0)
    };
    ok == 0 || on != 0
}

/// Makes the process per-monitor DPI aware (v2) so every coordinate we see
/// and set is in physical pixels. Must run before any window is created.
pub fn set_dpi_awareness() {
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}
