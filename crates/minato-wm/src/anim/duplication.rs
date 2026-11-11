//! GPU screen capture with DXGI Desktop Duplication.
//!
//! GDI capture of a 4K monitor costs ~50ms of CPU readback, which shows up
//! as input lag. Desktop Duplication hands us the composed desktop as a GPU
//! texture instead; we keep a copy per output and blit regions of it
//! straight into overlay surfaces without the pixels ever touching the CPU.

use minato_layout::Rect;
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_MODE_ROTATION_IDENTITY;
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_LOST,
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};

struct Output {
    output: IDXGIOutput1,
    /// Desktop coordinates covered by this output.
    rect: Rect,
    dup: Option<IDXGIOutputDuplication>,
    /// Our copy of the latest desktop image (duplication frames must be
    /// released quickly, and arrive only when something changed).
    copy: Option<ID3D11Texture2D>,
}

pub struct Duplicator {
    device: ID3D11Device,
    outputs: Vec<Output>,
}

impl Duplicator {
    /// Finds the outputs attached to `device`'s adapter. Outputs on other
    /// adapters or rotated displays are skipped; GDI capture covers them.
    pub fn new(device: &ID3D11Device) -> Self {
        let mut outputs = Vec::new();
        unsafe {
            if let Ok(adapter) = device.cast::<IDXGIDevice>().and_then(|d| d.GetAdapter()) {
                if let Ok(desc) = adapter.GetDesc() {
                    let name = String::from_utf16_lossy(&desc.Description);
                    log::info!("overlay GPU: {}", name.trim_end_matches('\0'));
                }
                let mut i = 0;
                while let Ok(output) = adapter.EnumOutputs(i) {
                    i += 1;
                    let Ok(desc) = output.GetDesc() else { continue };
                    if !desc.AttachedToDesktop.as_bool() || desc.Rotation != DXGI_MODE_ROTATION_IDENTITY {
                        continue;
                    }
                    let Ok(output) = output.cast::<IDXGIOutput1>() else { continue };
                    let r = desc.DesktopCoordinates;
                    outputs.push(Output {
                        output,
                        rect: Rect::from_edges(r.left, r.top, r.right, r.bottom),
                        dup: None,
                        copy: None,
                    });
                }
            }
        }
        log::info!("desktop duplication available for {} output(s)", outputs.len());
        Self {
            device: device.clone(),
            outputs,
        }
    }

    /// The latest image of the output containing `area`, plus `area`'s
    /// position inside that texture. `None` means: use GDI instead.
    pub fn capture(&mut self, ctx: &ID3D11DeviceContext, area: Rect) -> Option<(ID3D11Texture2D, i32, i32)> {
        let idx = self.outputs.iter().position(|o| {
            o.rect.x <= area.x
                && o.rect.y <= area.y
                && o.rect.right() >= area.right()
                && o.rect.bottom() >= area.bottom()
        })?;
        for attempt in 0..2 {
            match self.refresh(ctx, idx) {
                Ok(()) => break,
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST && attempt == 0 => {
                    // Mode change, UAC prompt, fullscreen app...: recreate.
                    self.outputs[idx].dup = None;
                }
                Err(e) => {
                    log::debug!("duplication failed: {e}");
                    self.outputs[idx].dup = None;
                    return None;
                }
            }
        }
        let o = &self.outputs[idx];
        Some((o.copy.clone()?, area.x - o.rect.x, area.y - o.rect.y))
    }

    fn refresh(&mut self, ctx: &ID3D11DeviceContext, idx: usize) -> windows::core::Result<()> {
        let device = self.device.clone();
        let o = &mut self.outputs[idx];
        unsafe {
            if o.dup.is_none() {
                o.dup = Some(o.output.DuplicateOutput(&device)?);
                o.copy = None;
            }
            let dup = o.dup.as_ref().unwrap();
            // Without a copy yet we must wait for the first frame; afterwards
            // a timeout just means nothing changed since our copy.
            let timeout = if o.copy.is_none() { 100 } else { 0 };
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;
            match dup.AcquireNextFrame(timeout, &mut info, &mut resource) {
                Ok(()) => {}
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT && o.copy.is_some() => return Ok(()),
                Err(e) => return Err(e),
            }
            let result = (|| {
                let frame: ID3D11Texture2D = resource.unwrap().cast()?;
                if o.copy.is_none() {
                    let mut desc = D3D11_TEXTURE2D_DESC::default();
                    frame.GetDesc(&mut desc);
                    desc.Usage = D3D11_USAGE_DEFAULT;
                    desc.BindFlags = 0;
                    desc.CPUAccessFlags = 0;
                    desc.MiscFlags = 0;
                    let mut tex = None;
                    device.CreateTexture2D(&desc, None, Some(&mut tex))?;
                    o.copy = tex;
                }
                ctx.CopyResource(o.copy.as_ref().unwrap(), &frame);
                Ok(())
            })();
            let _ = dup.ReleaseFrame();
            result
        }
    }
}
