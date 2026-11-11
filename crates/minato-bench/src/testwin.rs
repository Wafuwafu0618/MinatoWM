//! Plain top-level test windows, each on its own UI thread, that can be
//! told to react slowly (like a heavy app on a weak CPU).

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use minato_layout::Rect;
use minato_wm::win32::Hwnd;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, PAINTSTRUCT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetWindowLongPtrW, LoadCursorW, PostQuitMessage, RegisterClassW, SetWindowLongPtrW, TranslateMessage,
    GWLP_USERDATA, IDC_ARROW, MSG, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_PAINT, WM_WINDOWPOSCHANGED,
    WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

/// Extra time spent in every WM_PAINT.
pub static PAINT_MS: AtomicU32 = AtomicU32::new(0);
/// Extra time spent in every WM_WINDOWPOSCHANGED (a move or resize).
pub static SIZE_MS: AtomicU32 = AtomicU32::new(0);
/// When any test window last finished painting or reacting to a move,
/// in microseconds since [`epoch`].
static LAST_ACTIVITY_US: AtomicU64 = AtomicU64::new(0);

static EPOCH: OnceLock<Instant> = OnceLock::new();

pub fn epoch() -> Instant {
    *EPOCH.get_or_init(Instant::now)
}

fn mark_activity() {
    LAST_ACTIVITY_US.store(epoch().elapsed().as_micros() as u64, Ordering::Relaxed);
}

/// Milliseconds since [`epoch`] of the last test-window activity.
pub fn last_activity_ms() -> f64 {
    LAST_ACTIVITY_US.load(Ordering::Relaxed) as f64 / 1000.0
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

const PALETTE: [u32; 6] = [0x00a0_6040, 0x0040_8060, 0x0060_40a0, 0x0030_7090, 0x0090_5030, 0x0050_5050];

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_WINDOWPOSCHANGED => {
            let ms = SIZE_MS.load(Ordering::Relaxed);
            if ms > 0 {
                std::thread::sleep(Duration::from_millis(ms as u64));
            }
            let r = DefWindowProcW(hwnd, msg, wp, lp);
            mark_activity();
            r
        }
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            let ms = PAINT_MS.load(Ordering::Relaxed);
            if ms > 0 {
                std::thread::sleep(Duration::from_millis(ms as u64));
            }
            let mut rc: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut rc);
            let brush = CreateSolidBrush(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as u32);
            FillRect(dc, &rc, brush);
            DeleteObject(brush);
            EndPaint(hwnd, &ps);
            mark_activity();
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn class_name() -> &'static [u16] {
    static CLASS: OnceLock<Vec<u16>> = OnceLock::new();
    CLASS.get_or_init(|| {
        let name = wide("MinatoBench.TestWindow");
        unsafe {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: GetModuleHandleW(std::ptr::null()),
                lpszClassName: name.as_ptr(),
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                ..std::mem::zeroed()
            };
            RegisterClassW(&wc);
        }
        name
    })
}

/// Opens test window number `n` inside `area` and returns once it exists.
pub fn spawn(n: usize, area: Rect) -> Hwnd {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || unsafe {
        let title = wide(&format!("minato-bench #{n}"));
        let (w, h) = ((area.w / 2).max(320), (area.h / 2).max(240));
        let hwnd = CreateWindowExW(
            0,
            class_name().as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            area.x + (area.w - w) / 2,
            area.y + (area.h - h) / 2,
            w,
            h,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        );
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, PALETTE[n % PALETTE.len()] as isize);
        let _ = tx.send(hwnd as isize);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
    Hwnd(rx.recv().expect("test window thread died"))
}
