//! A click-through, never-focused overlay covering the virtual screen,
//! rendered with DirectComposition. Captured frames become visuals whose
//! offset and opacity change per frame; one `Commit` updates all of them
//! in the same composition frame.

use minato_layout::Rect;
use windows::core::{w, Interface, Result};
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BOX,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionEffectGroup, IDCompositionSurface,
    IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetSystemMetrics, PeekMessageW,
    RegisterClassW, SetWindowPos, ShowWindow, TranslateMessage, HWND_TOPMOST, MSG, PM_REMOVE,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    SWP_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
    HTTRANSPARENT, IDC_ARROW, LoadCursorW, LWA_ALPHA, SetLayeredWindowAttributes, WM_NCHITTEST,
    WS_EX_LAYERED,
};

use super::capture::Frame;

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCHITTEST {
        // Never the target of the mouse.
        return LRESULT(HTTRANSPARENT as isize);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

fn virtual_screen() -> Rect {
    unsafe {
        Rect::new(
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

/// One image on the overlay.
pub struct Layer {
    visual: IDCompositionVisual,
    effect: IDCompositionEffectGroup,
    pub surface: IDCompositionSurface,
}

impl Layer {
    pub fn set_offset(&self, x: f32, y: f32) {
        unsafe {
            // Whole pixels keep the picture crisp.
            let _ = self.visual.SetOffsetX2(x.round());
            let _ = self.visual.SetOffsetY2(y.round());
        }
    }

    pub fn set_opacity(&self, opacity: f32) {
        unsafe {
            let _ = self.effect.SetOpacity2(opacity.clamp(0.0, 1.0));
        }
    }
}

/// A monitor-sized container that clips its layers to the monitor.
pub struct Group {
    visual: IDCompositionVisual,
}

pub struct Compositor {
    hwnd: HWND,
    bounds: Rect,
    pub d3d: ID3D11Device,
    pub ctx: ID3D11DeviceContext,
    dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    root: IDCompositionVisual,
    shown: bool,
}

impl Compositor {
    /// Must be called on the thread that will drive the overlay.
    pub fn new() -> Result<Self> {
        unsafe {
            let instance: HMODULE = GetModuleHandleW(None)?;
            let class = w!("MinatoWM.Overlay");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: instance.into(),
                lpszClassName: class,
                // Fallback only: the overlay should never own the cursor.
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                ..Default::default()
            };
            RegisterClassW(&wc);
            let bounds = virtual_screen();
            let hwnd = CreateWindowExW(
                // LAYERED + TRANSPARENT is what makes a window click-through
                // for other processes (TRANSPARENT alone is not enough).
                WS_EX_NOREDIRECTIONBITMAP
                    | WS_EX_LAYERED
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOPMOST,
                class,
                w!("MinatoWM overlay"),
                WS_POPUP,
                bounds.x,
                bounds.y,
                bounds.w,
                bounds.h,
                None,
                None,
                Some(instance.into()),
                None,
            )?;
            // Layered windows stay invisible until given attributes; full
            // opacity lets the DirectComposition content through.
            SetLayeredWindowAttributes(hwnd, Default::default(), 255, LWA_ALPHA)?;

            let mut d3d = None;
            let mut ctx = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                None,
                Some(&mut ctx),
            )?;
            let d3d: ID3D11Device = d3d.unwrap();
            let ctx: ID3D11DeviceContext = ctx.unwrap();
            let dxgi: IDXGIDevice = d3d.cast()?;
            let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi)?;
            let target = dcomp.CreateTargetForHwnd(hwnd, true)?;
            let root = dcomp.CreateVisual()?;
            target.SetRoot(&root)?;
            dcomp.Commit()?;

            Ok(Self {
                hwnd,
                bounds,
                d3d,
                ctx,
                dcomp,
                _target: target,
                root,
                shown: false,
            })
        }
    }

    /// Keeps the overlay covering the whole virtual screen (monitors may
    /// have been added, removed or rearranged).
    fn fit(&mut self) {
        let b = virtual_screen();
        if b != self.bounds {
            self.bounds = b;
            unsafe {
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), b.x, b.y, b.w, b.h, SWP_NOACTIVATE);
            }
        }
    }

    pub fn show(&mut self) {
        self.fit();
        if !self.shown {
            self.shown = true;
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                let b = self.bounds;
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), b.x, b.y, b.w, b.h, SWP_NOACTIVATE);
            }
        }
    }

    pub fn hide(&mut self) {
        if self.shown {
            self.shown = false;
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }

    /// Services the overlay's message queue.
    pub fn pump(&self) {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    /// A container for one monitor's `area` (screen coordinates).
    pub fn group(&mut self, area: Rect) -> Result<Group> {
        self.fit();
        unsafe {
            let visual = self.dcomp.CreateVisual()?;
            visual.SetOffsetX2((area.x - self.bounds.x) as f32)?;
            visual.SetOffsetY2((area.y - self.bounds.y) as f32)?;
            visual.SetClip2(&D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: area.w as f32,
                bottom: area.h as f32,
            })?;
            self.root.AddVisual(&visual, true, None::<&IDCompositionVisual>)?;
            Ok(Group { visual })
        }
    }

    pub fn remove_group(&self, group: &Group) {
        unsafe {
            let _ = self.root.RemoveVisual(&group.visual);
        }
    }

    /// Uploads `frame` and adds it to `group` on top of existing layers.
    pub fn layer(&self, group: &Group, frame: &Frame) -> Result<Layer> {
        unsafe {
            let surface = self.dcomp.CreateSurface(
                frame.w,
                frame.h,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_ALPHA_MODE_IGNORE,
            )?;
            let mut offset = POINT::default();
            let texture: ID3D11Texture2D = surface.BeginDraw(None::<*const RECT>, &mut offset)?;
            let dst = D3D11_BOX {
                left: offset.x as u32,
                top: offset.y as u32,
                front: 0,
                right: offset.x as u32 + frame.w,
                bottom: offset.y as u32 + frame.h,
                back: 1,
            };
            self.ctx.UpdateSubresource(
                &texture,
                0,
                Some(&dst),
                frame.pixels.as_ptr() as *const _,
                frame.w * 4,
                0,
            );
            surface.EndDraw()?;
            self.layer_from_surface(group, surface)
        }
    }

    /// Copies a `w`x`h` region at (`x`, `y`) of a GPU texture (from the same
    /// device) into a new layer.
    pub fn layer_from_texture(
        &self,
        group: &Group,
        src: &ID3D11Texture2D,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
    ) -> Result<Layer> {
        unsafe {
            let surface = self.dcomp.CreateSurface(w, h, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_ALPHA_MODE_IGNORE)?;
            let mut offset = POINT::default();
            let texture: ID3D11Texture2D = surface.BeginDraw(None::<*const RECT>, &mut offset)?;
            let src_box = D3D11_BOX {
                left: x as u32,
                top: y as u32,
                front: 0,
                right: x as u32 + w,
                bottom: y as u32 + h,
                back: 1,
            };
            self.ctx.CopySubresourceRegion(&texture, 0, offset.x as u32, offset.y as u32, 0, src, 0, Some(&src_box));
            surface.EndDraw()?;
            self.layer_from_surface(group, surface)
        }
    }

    /// Shows an existing surface (e.g. a cached picture) as a new layer.
    pub fn layer_from_surface(&self, group: &Group, surface: IDCompositionSurface) -> Result<Layer> {
        unsafe {
            let visual = self.dcomp.CreateVisual()?;
            visual.SetContent(&surface)?;
            let effect = self.dcomp.CreateEffectGroup()?;
            effect.SetOpacity2(1.0)?;
            visual.SetEffect(&effect)?;
            group.visual.AddVisual(&visual, true, None::<&IDCompositionVisual>)?;
            Ok(Layer { visual, effect, surface })
        }
    }

    pub fn commit(&self) {
        unsafe {
            if let Err(e) = self.dcomp.Commit() {
                log::warn!("DirectComposition commit failed: {e}");
            }
        }
    }
}
