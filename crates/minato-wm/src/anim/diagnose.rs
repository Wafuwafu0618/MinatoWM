//! Diagnostics for cursor handling in captures (see `examples/cursor_check.rs`).

use minato_layout::Rect;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BOX,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

use super::duplication::Duplicator;
use super::show_system_cursor;

fn flush(n: usize) {
    for _ in 0..n {
        unsafe {
            let _ = windows::Win32::Graphics::Dwm::DwmFlush();
        }
    }
}

/// Reads `area` of the duplicated desktop back to the CPU.
fn grab(dup: &mut Duplicator, d3d: &ID3D11Device, ctx: &ID3D11DeviceContext, area: Rect) -> Option<Vec<u8>> {
    let (tex, x, y) = dup.capture(ctx, area)?;
    unsafe {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: area.w as u32,
            Height: area.h as u32,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging: Option<ID3D11Texture2D> = None;
        d3d.CreateTexture2D(&desc, None, Some(&mut staging)).ok()?;
        let staging = staging?;
        let src = D3D11_BOX {
            left: x as u32,
            top: y as u32,
            front: 0,
            right: (x + area.w) as u32,
            bottom: (y + area.h) as u32,
            back: 1,
        };
        ctx.CopySubresourceRegion(&staging, 0, 0, 0, 0, &tex, 0, Some(&src));
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).ok()?;
        let mut out = Vec::with_capacity((area.w * area.h * 4) as usize);
        for row in 0..area.h as usize {
            let p = (mapped.pData as *const u8).add(row * mapped.RowPitch as usize);
            out.extend_from_slice(std::slice::from_raw_parts(p, area.w as usize * 4));
        }
        ctx.Unmap(&staging, 0);
        Some(out)
    }
}

fn differing(a: &[u8], b: &[u8]) -> usize {
    a.chunks(4).zip(b.chunks(4)).filter(|(x, y)| x[..3] != y[..3]).count()
}

/// Checks, on the real desktop, whether duplicated frames contain the
/// cursor and whether hiding it with the Magnification API works and is
/// undone. Returns a human-readable report.
pub fn cursor_report() -> String {
    let mut report = String::new();
    let (cx, cy) = crate::win32::cursor_pos();
    let Some(mon) = crate::win32::monitors().into_iter().find(|m| m.rect.contains_point(cx, cy)) else {
        return "cursor is on no monitor".into();
    };
    // A box around the cursor, inside the monitor.
    let size = 128;
    let x = (cx - size / 2).clamp(mon.rect.x, mon.rect.right() - size);
    let y = (cy - size / 2).clamp(mon.rect.y, mon.rect.bottom() - size);
    let area = Rect::new(x, y, size, size);
    report += &format!("cursor at ({cx},{cy}) on {}, sampling {area:?}\n", mon.device);

    let mut d3d = None;
    let mut ctx = None;
    unsafe {
        if D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut d3d),
            None,
            Some(&mut ctx),
        )
        .is_err()
        {
            return report + "D3D11 device creation failed";
        }
    }
    let (d3d, ctx): (ID3D11Device, ID3D11DeviceContext) = (d3d.unwrap(), ctx.unwrap());
    let mut dup = Duplicator::new(&d3d);
    let mag = unsafe { windows_sys::Win32::UI::Magnification::MagInitialize() != 0 };
    report += &format!("MagInitialize: {mag}\n");

    flush(3);
    // Warm up: the first frames after DuplicateOutput can be stale.
    for _ in 0..3 {
        if grab(&mut dup, &d3d, &ctx, area).is_none() {
            return report + "duplication unavailable for this monitor (GDI path)";
        }
        flush(2);
    }
    let Some(shown) = grab(&mut dup, &d3d, &ctx, area) else {
        return report + "duplication unavailable for this monitor (GDI path)";
    };
    flush(3);
    let shown2 = grab(&mut dup, &d3d, &ctx, area).unwrap_or_default();
    let zero = shown.chunks(4).filter(|p| p[..3] == [0, 0, 0]).count();
    report += &format!("baseline noise (two grabs, cursor shown): {} px; black px: {zero}
", differing(&shown, &shown2));
    show_system_cursor(false);
    flush(3);
    let hidden = grab(&mut dup, &d3d, &ctx, area).unwrap_or_default();
    show_system_cursor(true);
    flush(3);
    let again = grab(&mut dup, &d3d, &ctx, area).unwrap_or_default();

    let d_hide = differing(&shown, &hidden);
    let d_again = differing(&shown, &again);
    report += &format!("hidden vs shown-again: {} px
", differing(&hidden, &again));
    report += &format!("pixels changed by hiding the cursor: {d_hide}\n");
    report += &format!("pixels still changed after showing it again: {d_again}\n");
    report += match (d_hide > 0, d_again == 0) {
        (true, true) => "=> the cursor IS baked into captures; hiding works and is undone",
        (true, false) => "=> the cursor is baked into captures, but showing it again did NOT restore it",
        (false, _) => "=> the cursor is NOT in captures; hiding it during capture is unnecessary",
    };
    report
}
