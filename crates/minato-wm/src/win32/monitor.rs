use std::mem::size_of;

use minato_layout::Rect;
use windows_sys::Win32::Foundation::{LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, HDC, HMONITOR, MONITORINFO,
    MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};

use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

use super::{to_rect, wide_to_string};

fn monitor_dpi(h: HMONITOR) -> u32 {
    let (mut x, mut y) = (0u32, 0u32);
    let ok = unsafe { GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut x, &mut y) } == 0;
    if ok && x > 0 {
        x
    } else {
        96
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorInfo {
    pub handle: isize,
    /// Device name such as `\\.\DISPLAY1`; stable across reconnects.
    pub device: String,
    pub rect: Rect,
    /// The monitor minus the taskbar and other app bars.
    pub work_area: Rect,
    /// Effective DPI (96 = 100% scaling).
    pub dpi: u32,
}

/// All monitors, sorted left to right, then top to bottom.
pub fn monitors() -> Vec<MonitorInfo> {
    unsafe extern "system" fn collect(h: HMONITOR, _: HDC, _: *mut RECT, lparam: LPARAM) -> i32 {
        let out = &mut *(lparam as *mut Vec<MonitorInfo>);
        let mut info: MONITORINFOEXW = std::mem::zeroed();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(h, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) != 0 {
            out.push(MonitorInfo {
                handle: h as isize,
                device: wide_to_string(&info.szDevice),
                rect: to_rect(info.monitorInfo.rcMonitor),
                work_area: to_rect(info.monitorInfo.rcWork),
                dpi: monitor_dpi(h),
            });
        }
        1
    }
    let mut out: Vec<MonitorInfo> = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(collect),
            &mut out as *mut Vec<MonitorInfo> as LPARAM,
        )
    };
    out.sort_by_key(|m| (m.rect.x, m.rect.y));
    out
}

pub fn monitor_at_point(x: i32, y: i32) -> isize {
    unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) as isize }
}
