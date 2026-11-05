//! Direct2D helpers: an offscreen premultiplied surface for layered windows, colours and text.
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

// Tokens from replica/design/tokens.json. Neutral graphite with one signal colour, used sparingly.
pub const BG: u32 = 0x0F1012;
pub const SURFACE: u32 = 0x191B1E;
pub const RAISED: u32 = 0x26282C;
pub const BORDER: u32 = 0x33363B;
pub const TEXT: u32 = 0xF5F6F7;
pub const MUTED: u32 = 0xA2A7AF;
pub const FAINT: u32 = 0x6F747C;
pub const ACCENT: u32 = 0xC8F135;
pub const ON_ACCENT: u32 = 0x12140A;
pub const DANGER: u32 = 0xFF6B5E;
pub const SUCCESS: u32 = 0x7EE8A8;
pub const FONT: &str = "Segoe UI";
pub const ICONS: &str = "Segoe MDL2 Assets";

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

pub fn rrect(x: f32, y: f32, w: f32, h: f32, r: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }, radiusX: r, radiusY: r }
}

/// An open arc. Angles are radians clockwise from twelve o'clock, as on the dial.
pub fn arc(g: &Gfx, cx: f32, cy: f32, r: f32, from: f32, sweep: f32) -> Result<ID2D1PathGeometry> {
    let at = |a: f32| pt(cx + a.sin() * r, cy - a.cos() * r);
    let sweep = sweep.clamp(-std::f32::consts::TAU * 0.9999, std::f32::consts::TAU * 0.9999);
    unsafe {
        let geo = g.d2d.CreatePathGeometry()?;
        let sink = geo.Open()?;
        sink.BeginFigure(at(from), D2D1_FIGURE_BEGIN_HOLLOW);
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: at(from + sweep), size: D2D_SIZE_F { width: r, height: r }, rotationAngle: 0.0,
            sweepDirection: if sweep >= 0.0 { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
            arcSize: if sweep.abs() > std::f32::consts::PI { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
        });
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
        sink.Close()?;
        Ok(geo)
    }
}

pub fn round_stroke(g: &Gfx) -> Result<ID2D1StrokeStyle> {
    let props = D2D1_STROKE_STYLE_PROPERTIES {
        startCap: D2D1_CAP_STYLE_ROUND, endCap: D2D1_CAP_STYLE_ROUND, dashCap: D2D1_CAP_STYLE_ROUND,
        lineJoin: D2D1_LINE_JOIN_ROUND, miterLimit: 4.0, dashStyle: D2D1_DASH_STYLE_SOLID, dashOffset: 0.0,
    };
    unsafe { g.d2d.CreateStrokeStyle(&props, None) }
}

/// The mark: a signal disc with a dark tau. Same geometry as the icon in build.rs.
pub fn draw_logo(g: &Gfx, rt: &ID2D1RenderTarget, x: f32, y: f32, size: f32) -> Result<()> {
    let p = |u: f32, v: f32| pt(x + u * size, y + v * size);
    unsafe {
        let disc = rt.CreateSolidColorBrush(&rgba(ACCENT, 1.0), None)?;
        let ink = rt.CreateSolidColorBrush(&rgba(ON_ACCENT, 1.0), None)?;
        rt.FillEllipse(&ellipse(x + size / 2.0, y + size / 2.0, size / 2.0), &disc);
        let geo = g.d2d.CreatePathGeometry()?;
        let sink = geo.Open()?;
        sink.BeginFigure(p(0.27, 0.36), D2D1_FIGURE_BEGIN_HOLLOW);
        sink.AddLine(p(0.75, 0.36));
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
        sink.BeginFigure(p(0.50, 0.36), D2D1_FIGURE_BEGIN_HOLLOW);
        sink.AddLine(p(0.50, 0.60));
        sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT { point1: p(0.50, 0.72), point2: p(0.64, 0.72) });
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
        sink.Close()?;
        rt.DrawGeometry(&geo, &ink, size * 0.09, &round_stroke(g)?);
    }
    Ok(())
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
