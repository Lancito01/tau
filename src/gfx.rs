//! Direct2D helpers: an offscreen premultiplied surface for layered windows, colours and text.
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

// Tokens from replica/design/tokens.json.
pub const BG: u32 = 0x10111A;
pub const SURFACE: u32 = 0x23253A;
pub const BORDER: u32 = 0x3A3D58;
pub const TEXT: u32 = 0xF3F4FA;
pub const MUTED: u32 = 0xA9ADC4;
pub const ACCENT: u32 = 0x6D4AFF;
pub const ON_ACCENT: u32 = 0xFFFFFF;
pub const DANGER: u32 = 0xFF6B6B;
pub const SUCCESS: u32 = 0x3DD68C;

pub fn rgba(c: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((c >> 16) & 255) as f32 / 255.0,
        g: ((c >> 8) & 255) as f32 / 255.0,
        b: (c & 255) as f32 / 255.0,
        a,
    }
}

pub fn pt(x: f32, y: f32) -> windows_numerics::Vector2 { windows_numerics::Vector2 { X: x, Y: y } }

pub fn ellipse(x: f32, y: f32, r: f32) -> D2D1_ELLIPSE {
    D2D1_ELLIPSE { point: pt(x, y), radiusX: r, radiusY: r }
}

pub fn wide(s: &str) -> Vec<u16> { s.encode_utf16().collect() }
pub fn wz(s: &str) -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() }

pub struct Gfx { pub d2d: ID2D1Factory, pub dw: IDWriteFactory }

impl Gfx {
    pub fn new() -> Result<Gfx> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            Ok(Gfx { d2d, dw })
        }
    }

    pub fn format(&self, family: &str, size: f32, weight: DWRITE_FONT_WEIGHT, align: DWRITE_TEXT_ALIGNMENT) -> Result<IDWriteTextFormat> {
        unsafe {
            let f = self.dw.CreateTextFormat(
                &HSTRING::from(family), None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL,
                size, w!("en-us"),
            )?;
            f.SetTextAlignment(align)?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            Ok(f)
        }
    }
}

/// A memory bitmap that Direct2D draws into and UpdateLayeredWindow presents.
pub struct Surface {
    pub w: i32,
    pub h: i32,
    dc: HDC,
    dib: HBITMAP,
    old: HGDIOBJ,
    pub bits: *mut u32,
    pub rt: ID2D1DCRenderTarget,
}

impl Surface {
    pub fn new(g: &Gfx, w: i32, h: i32) -> Result<Surface> {
        unsafe {
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            ReleaseDC(None, screen);
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let dib = CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let old = SelectObject(dc, dib.into());
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0, dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = g.d2d.CreateDCRenderTarget(&props)?;
            rt.BindDC(dc, &RECT { left: 0, top: 0, right: w, bottom: h })?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            Ok(Surface { w, h, dc, dib, old, bits: bits as *mut u32, rt })
        }
    }

    pub fn fill_raw(&self, px: u32) {
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h) as usize).fill(px); }
    }

    /// Push the bitmap to a layered window at screen position (x, y).
    pub fn present(&self, hwnd: HWND, x: i32, y: i32) {
        unsafe {
            let blend = BLENDFUNCTION { BlendOp: 0, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: 1 };
            let pos = POINT { x, y };
            let size = SIZE { cx: self.w, cy: self.h };
            let src = POINT { x: 0, y: 0 };
            let _ = UpdateLayeredWindow(hwnd, None, Some(&pos), Some(&size), Some(self.dc), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
        }
    }

    /// RGBA8 copy, un-premultiplied, for writing a PNG.
    pub fn to_rgba(&self) -> Vec<u8> {
        let n = (self.w * self.h) as usize;
        let src = unsafe { std::slice::from_raw_parts(self.bits, n) };
        let mut out = Vec::with_capacity(n * 4);
        for &p in src {
            let a = (p >> 24) & 255;
            let un = |v: u32| if a == 0 { 0 } else { (v * 255 / a).min(255) as u8 };
            out.extend_from_slice(&[un((p >> 16) & 255), un((p >> 8) & 255), un(p & 255), a as u8]);
        }
        out
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.dib.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

pub fn cursor_pos() -> POINT {
    let mut p = POINT::default();
    unsafe { let _ = GetCursorPos(&mut p); }
    p
}

pub fn dpi_scale_at(p: POINT) -> f32 {
    use windows::Win32::UI::HiDpi::*;
    unsafe {
        let m = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        let (mut x, mut y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut x, &mut y);
        x as f32 / 96.0
    }
}

pub fn work_area_at(p: POINT) -> RECT {
    unsafe {
        let m = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(m, &mut mi);
        mi.rcWork
    }
}
