//! Upload a frozen desktop image once, then scale its tiles on a hardware Direct2D target.
//! No capture sessions or foreign-process code injection are involved.
use std::collections::HashSet;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

pub struct Renderer {
    target: ID2D1HwndRenderTarget,
    bitmap: ID2D1Bitmap,
}

impl Renderer {
    pub unsafe fn new(hwnd: HWND, pixels: &[u8], width: u32, height: u32) -> Option<Self> {
        let len = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        if width == 0 || height == 0 || pixels.len() != len {
            return None;
        }
        let factory: ID2D1Factory =
            D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ok()?;
        let format = D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_IGNORE,
        };
        let target = factory
            .CreateHwndRenderTarget(
                &D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: D2D1_RENDER_TARGET_TYPE_HARDWARE,
                    pixelFormat: format,
                    dpiX: 96.0,
                    dpiY: 96.0,
                    ..Default::default()
                },
                &D2D1_HWND_RENDER_TARGET_PROPERTIES {
                    hwnd,
                    pixelSize: D2D_SIZE_U { width, height },
                    presentOptions: D2D1_PRESENT_OPTIONS_IMMEDIATELY,
                },
            )
            .ok()?;
        let bitmap = target
            .CreateBitmap(
                D2D_SIZE_U { width, height },
                Some(pixels.as_ptr().cast()),
                width * 4,
                &D2D1_BITMAP_PROPERTIES {
                    pixelFormat: format,
                    dpiX: 96.0,
                    dpiY: 96.0,
                },
            )
            .ok()?;
        Some(Self { target, bitmap })
    }

    pub unsafe fn draw(
        &self,
        area: &RECT,
        sources: &[(isize, RECT)],
        tiles: &[(isize, RECT)],
        entering: &HashSet<isize>,
        progress: f32,
    ) -> bool {
        self.target.BeginDraw();
        self.target.Clear(Some(&D2D1_COLOR_F {
            r: 19.0 / 255.0,
            g: 23.0 / 255.0,
            b: 27.0 / 255.0,
            a: 1.0,
        }));
        for (h, dest) in tiles {
            if let Some((_, source)) = sources.iter().find(|(source, _)| source == h) {
                let local = |r: &RECT| D2D_RECT_F {
                    left: (r.left - area.left) as f32,
                    top: (r.top - area.top) as f32,
                    right: (r.right - area.left) as f32,
                    bottom: (r.bottom - area.top) as f32,
                };
                self.target.DrawBitmap(
                    &self.bitmap,
                    Some(&local(dest)),
                    if entering.contains(h) { progress } else { 1.0 },
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    Some(&local(source)),
                );
            }
        }
        self.target.EndDraw(None, None).is_ok()
    }
}
