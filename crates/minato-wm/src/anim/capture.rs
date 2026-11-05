//! Grabs what is currently on screen. Reading the screen DC returns the
//! DWM-composed desktop, so DirectX / Chromium content and our own overlay
//! (mid-animation) are included.

use std::time::Instant;

use minato_layout::Rect;
use windows_sys::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, SRCCOPY,
};

/// A captured BGRA image, top-down rows, `w * 4` bytes per row.
pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub pixels: Vec<u8>,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Frame({}x{})", self.w, self.h)
    }
}

/// Captures `area` (screen coordinates, physical pixels).
pub fn capture(area: Rect) -> Option<Frame> {
    if area.w <= 0 || area.h <= 0 {
        return None;
    }
    let started = Instant::now();
    unsafe {
        let screen = GetDC(std::ptr::null_mut());
        if screen.is_null() {
            return None;
        }
        let mem = CreateCompatibleDC(screen);
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: area.w,
            biHeight: -area.h, // negative: top-down rows
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(mem, &info, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        let mut result = None;
        if !dib.is_null() && !bits.is_null() {
            let old = SelectObject(mem, dib);
            if BitBlt(mem, 0, 0, area.w, area.h, screen, area.x, area.y, SRCCOPY | CAPTUREBLT) != 0 {
                let len = area.w as usize * area.h as usize * 4;
                let pixels = std::slice::from_raw_parts(bits as *const u8, len).to_vec();
                result = Some(Frame {
                    w: area.w as u32,
                    h: area.h as u32,
                    pixels,
                });
            }
            SelectObject(mem, old);
            DeleteObject(dib);
        }
        DeleteDC(mem);
        ReleaseDC(std::ptr::null_mut(), screen);
        log::debug!("captured {}x{} in {:?}", area.w, area.h, started.elapsed());
        result
    }
}
