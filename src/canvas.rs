//! Shared image editor window: crop, redact and annotate. Coordinates are fractions of the image,
//! so one result can apply across a batch or to a video of any resolution.
use crate::gfx::{wide, wz};
use crate::image::Bmp;
use std::cell::RefCell;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode { Crop, Redact, Annotate }

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tool { Rect, Arrow, Text }

#[derive(Clone, PartialEq, Debug)]
pub struct Shape { pub tool: Tool, pub a: (f64, f64), pub b: (f64, f64), pub color: u32, pub text: String }

impl Shape {
    pub fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Shape { Shape { tool: Tool::Rect, a: (x0, y0), b: (x1, y1), color: 0, text: String::new() } }
    /// Normalised (x, y, w, h) fractions.
    pub fn bounds(&self) -> (f64, f64, f64, f64) {
        let (x0, x1) = (self.a.0.min(self.b.0).clamp(0.0, 1.0), self.a.0.max(self.b.0).clamp(0.0, 1.0));
        let (y0, y1) = (self.a.1.min(self.b.1).clamp(0.0, 1.0), self.a.1.max(self.b.1).clamp(0.0, 1.0));
        (x0, y0, x1 - x0, y1 - y0)
    }
    pub fn encode(&self) -> String {
        let t = match self.tool { Tool::Rect => "rect", Tool::Arrow => "arrow", Tool::Text => "text" };
        format!("{t}\t{}\t{}\t{}\t{}\t{:06X}\t{}", self.a.0, self.a.1, self.b.0, self.b.1, self.color, self.text.replace(['\t', '\n'], " "))
    }
    pub fn decode(s: &str) -> Option<Shape> {
        let f: Vec<&str> = s.split('\t').collect();
        let n = |i: usize| f.get(i)?.parse::<f64>().ok();
        Some(Shape {
            tool: match *f.first()? { "rect" => Tool::Rect, "arrow" => Tool::Arrow, "text" => Tool::Text, _ => return None },
            a: (n(1)?, n(2)?), b: (n(3)?, n(4)?),
            color: u32::from_str_radix(f.get(5)?, 16).ok()?,
            text: f.get(6).unwrap_or(&"").to_string(),
        })
    }
}

pub fn encode_all(v: &[Shape]) -> String { v.iter().map(Shape::encode).collect::<Vec<_>>().join("\n") }
pub fn decode_all(s: &str) -> Vec<Shape> { s.lines().filter_map(Shape::decode).collect() }

const COLORS: [u32; 5] = [0xFF3B30, 0xFFCC00, 0x34C759, 0x0A84FF, 0xFFFFFF];
const ID_SAVE: usize = 101;
const ID_CANCEL: usize = 102;
const ID_RECT: usize = 103;
const ID_ARROW: usize = 104;
const ID_TEXT: usize = 105;
const ID_COLOR: usize = 106;
const ID_UNDO: usize = 107;
const BAR: i32 = 48;

fn colorref(c: u32) -> COLORREF { COLORREF(((c >> 16) & 255) | (c & 0xFF00) | ((c & 255) << 16)) }

struct State {
    mode: Mode,
    tool: Tool,
    color: usize,
    shapes: Vec<Shape>,
    drag: Option<Shape>,
    bmp: Bmp,
    done: Option<bool>,
}
thread_local! { static C: RefCell<Option<State>> = const { RefCell::new(None) }; }

fn bmi(w: u32, h: u32) -> BITMAPINFO {
    BITMAPINFO { bmiHeader: BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w as i32, biHeight: -(h as i32),
        biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default()
    }, ..Default::default() }
}

/// Image rectangle inside the client area, fitted and centred.
fn view(rc: &RECT, bw: u32, bh: u32, bar: i32) -> (i32, i32, i32, i32) {
    let (aw, ah) = ((rc.right - 16).max(1) as f64, (rc.bottom - bar - 16).max(1) as f64);
    let s = (aw / bw as f64).min(ah / bh as f64);
    let (w, h) = ((bw as f64 * s) as i32, (bh as f64 * s) as i32);
    ((rc.right - w) / 2, (rc.bottom - bar - h) / 2, w.max(1), h.max(1))
}

/// Draws shapes into a DC where the image occupies (ox, oy, w, h).
unsafe fn draw_shapes(dc: HDC, shapes: &[Shape], ox: i32, oy: i32, w: i32, h: i32, redact: bool) {
    let map = |p: (f64, f64)| (ox + (p.0 * w as f64) as i32, oy + (p.1 * h as f64) as i32);
    let thick = (w.max(h) / 220).max(3);
    let null = GetStockObject(NULL_BRUSH);
    for s in shapes {
        let (ax, ay) = map(s.a);
        let (bx, by) = map(s.b);
        let pen = CreatePen(PS_SOLID, thick, colorref(s.color));
        let old = SelectObject(dc, pen.into());
        match s.tool {
            Tool::Rect if redact => {
                let br = CreateSolidBrush(COLORREF(0));
                let _ = FillRect(dc, &RECT { left: ax.min(bx), top: ay.min(by), right: ax.max(bx), bottom: ay.max(by) }, br);
                let _ = DeleteObject(br.into());
            }
            Tool::Rect => {
                let ob = SelectObject(dc, null);
                let _ = Rectangle(dc, ax, ay, bx, by);
                SelectObject(dc, ob);
            }
            Tool::Arrow => {
                let _ = MoveToEx(dc, ax, ay, None);
                let _ = LineTo(dc, bx, by);
                let ang = ((ay - by) as f64).atan2((ax - bx) as f64);
                let head = (thick * 5) as f64;
                for d in [-0.45f64, 0.45] {
                    let _ = MoveToEx(dc, bx, by, None);
                    let _ = LineTo(dc, bx + ((ang + d).cos() * head) as i32, by + ((ang + d).sin() * head) as i32);
                }
            }
            Tool::Text => {
                let font = CreateFontW(-(thick * 8), 0, 0, 0, 600, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, ANTIALIASED_QUALITY, 0, w!("Segoe UI"));
                let of = SelectObject(dc, font.into());
                SetBkMode(dc, TRANSPARENT);
                SetTextColor(dc, colorref(s.color));
                let _ = TextOutW(dc, ax, ay, &wide(&s.text));
                SelectObject(dc, of);
                let _ = DeleteObject(font.into());
            }
        }
        SelectObject(dc, old);
        let _ = DeleteObject(pen.into());
    }
}

/// Burns annotations into the pixels at full resolution.
pub fn apply_annotations(b: &mut Bmp, shapes: &[Shape]) -> std::result::Result<(), String> {
    unsafe {
        let dc = CreateCompatibleDC(None);
        let info = bmi(b.w, b.h);
        let mut bits = std::ptr::null_mut();
        let dib = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0).map_err(|e| e.to_string())?;
        let old = SelectObject(dc, dib.into());
        let n = (b.w * b.h * 4) as usize;
        std::ptr::copy_nonoverlapping(b.px.as_ptr(), bits as *mut u8, n);
        draw_shapes(dc, shapes, 0, 0, b.w as i32, b.h as i32, false);
        let _ = GdiFlush();
        let out = std::slice::from_raw_parts(bits as *const u8, n);
        // GDI clears alpha on everything it touches: restore it where the colour changed.
        for (d, s) in b.px.chunks_exact_mut(4).zip(out.chunks_exact(4)) {
            if d[..3] != s[..3] { d[..3].copy_from_slice(&s[..3]); d[3] = 255; }
        }
        SelectObject(dc, old);
        let _ = DeleteObject(dib.into());
        let _ = DeleteDC(dc);
    }
    Ok(())
}

fn hit_fraction(hwnd: HWND, x: i32, y: i32) -> (f64, f64) {
    unsafe {
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let bar = bar_px(hwnd);
        C.with(|c| {
            let c = c.borrow();
            let st = c.as_ref().unwrap();
            let (ox, oy, w, h) = view(&rc, st.bmp.w, st.bmp.h, bar);
            (((x - ox) as f64 / w as f64).clamp(0.0, 1.0), ((y - oy) as f64 / h as f64).clamp(0.0, 1.0))
        })
    }
}

fn bar_px(hwnd: HWND) -> i32 { unsafe { BAR * GetDpiForWindow(hwnd) as i32 / 96 } }

fn paint(hwnd: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let mem = CreateCompatibleDC(Some(dc));
        let bm = CreateCompatibleBitmap(dc, rc.right.max(1), rc.bottom.max(1));
        let old = SelectObject(mem, bm.into());
        let bg = CreateSolidBrush(colorref(crate::gfx::BG));
        FillRect(mem, &rc, bg);
        let _ = DeleteObject(bg.into());
        let bar = bar_px(hwnd);
        C.with(|c| {
            let c = c.borrow();
            let Some(st) = c.as_ref() else { return };
            let (ox, oy, w, h) = view(&rc, st.bmp.w, st.bmp.h, bar);
            let info = bmi(st.bmp.w, st.bmp.h);
            SetStretchBltMode(mem, HALFTONE);
            StretchDIBits(mem, ox, oy, w, h, 0, 0, st.bmp.w as i32, st.bmp.h as i32, Some(st.bmp.px.as_ptr() as *const _), &info, DIB_RGB_COLORS, SRCCOPY);
            let redact = st.mode == Mode::Redact;
            draw_shapes(mem, &st.shapes, ox, oy, w, h, redact);
            if let Some(d) = &st.drag { draw_shapes(mem, std::slice::from_ref(d), ox, oy, w, h, redact); }
        });
        let _ = BitBlt(dc, 0, 0, rc.right, rc.bottom, Some(mem), 0, 0, SRCCOPY);
        SelectObject(mem, old);
        let _ = DeleteObject(bm.into());
        let _ = DeleteDC(mem);
        let _ = EndPaint(hwnd, &ps);
    }
}

fn set_done(v: bool) { C.with(|c| if let Some(s) = c.borrow_mut().as_mut() { s.done = Some(v); }); }

fn prompt_text() -> Option<String> {
    let v = crate::form::show("Add text", &[crate::form::text("Text", "")], "Add")?;
    let t = v[0].trim().to_string();
    (!t.is_empty()).then_some(t)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let (x, y) = ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32);
        match msg {
            WM_PAINT => { paint(hwnd); LRESULT(0) }
            WM_ERASEBKGND => LRESULT(1),
            WM_SIZE => { layout(hwnd); let _ = InvalidateRect(Some(hwnd), None, false); LRESULT(0) }
            WM_COMMAND => {
                match wp.0 & 0xFFFF {
                    ID_SAVE => set_done(true),
                    ID_CANCEL => set_done(false),
                    ID_RECT => C.with(|c| c.borrow_mut().as_mut().unwrap().tool = Tool::Rect),
                    ID_ARROW => C.with(|c| c.borrow_mut().as_mut().unwrap().tool = Tool::Arrow),
                    ID_TEXT => C.with(|c| c.borrow_mut().as_mut().unwrap().tool = Tool::Text),
                    ID_COLOR => C.with(|c| { let mut b = c.borrow_mut(); let s = b.as_mut().unwrap(); s.color = (s.color + 1) % COLORS.len(); }),
                    ID_UNDO => C.with(|c| { c.borrow_mut().as_mut().unwrap().shapes.pop(); }),
                    _ => {}
                }
                let _ = SetFocus(Some(hwnd));
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let p = hit_fraction(hwnd, x, y);
                let tool = C.with(|c| c.borrow().as_ref().unwrap().tool);
                if tool == Tool::Text {
                    if let Some(t) = prompt_text() {
                        C.with(|c| { let mut b = c.borrow_mut(); let s = b.as_mut().unwrap(); let color = COLORS[s.color]; s.shapes.push(Shape { tool, a: p, b: p, color, text: t }); });
                    }
                } else {
                    let _ = SetCapture(hwnd);
                    C.with(|c| { let mut b = c.borrow_mut(); let s = b.as_mut().unwrap(); let color = COLORS[s.color]; s.drag = Some(Shape { tool, a: p, b: p, color, text: String::new() }); });
                }
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let p = hit_fraction(hwnd, x, y);
                let moved = C.with(|c| { let mut b = c.borrow_mut(); let s = b.as_mut().unwrap(); if let Some(d) = s.drag.as_mut() { d.b = p; true } else { false } });
                if moved { let _ = InvalidateRect(Some(hwnd), None, false); }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let _ = ReleaseCapture();
                C.with(|c| {
                    let mut b = c.borrow_mut();
                    let s = b.as_mut().unwrap();
                    if let Some(d) = s.drag.take() {
                        let (_, _, w, h) = d.bounds();
                        if w.max(h) > 0.005 {
                            if s.mode == Mode::Crop { s.shapes.clear(); }
                            s.shapes.push(d);
                        }
                    }
                });
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }
            WM_KEYDOWN => {
                if wp.0 == VK_ESCAPE.0 as usize { set_done(false); }
                if wp.0 == VK_RETURN.0 as usize { set_done(true); }
                LRESULT(0)
            }
            WM_CLOSE => { set_done(false); LRESULT(0) }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn layout(hwnd: HWND) {
    unsafe {
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let dpi = GetDpiForWindow(hwnd) as i32;
        let px = |v: i32| v * dpi / 96;
        let y = rc.bottom - px(BAR) + px(9);
        let place = |id: usize, x: i32, w: i32| {
            if let Ok(c) = GetDlgItem(Some(hwnd), id as i32) { let _ = MoveWindow(c, x, y, w, px(30), true); }
        };
        place(ID_SAVE, rc.right - px(106), px(96));
        place(ID_CANCEL, rc.right - px(206), px(92));
        let mut x = px(10);
        for id in [ID_RECT, ID_ARROW, ID_TEXT, ID_COLOR, ID_UNDO] { place(id, x, px(72)); x += px(78); }
    }
}

/// Opens the editor and blocks, pumping messages, until saved or cancelled.
pub fn edit(title: &str, bmp: Bmp, mode: Mode, initial: Vec<Shape>, save_label: &str) -> Option<Vec<Shape>> {
    unsafe {
        let inst = GetModuleHandleW(None).ok()?;
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: inst.into(), lpszClassName: w!("OrreryCanvas"), hCursor: LoadCursorW(None, IDC_CROSS).ok()?, ..Default::default() };
        RegisterClassW(&wc);
        let wa = crate::gfx::work_area_at(crate::gfx::cursor_pos());
        let (ww, wh) = ((wa.right - wa.left) * 3 / 4, (wa.bottom - wa.top) * 3 / 4);
        let t = wz(title);
        C.with(|c| *c.borrow_mut() = Some(State { mode, tool: Tool::Rect, color: 0, shapes: initial, drag: None, bmp, done: None }));
        let hwnd = CreateWindowExW(WS_EX_TOPMOST, w!("OrreryCanvas"), PCWSTR(t.as_ptr()), WS_OVERLAPPEDWINDOW,
            wa.left + (wa.right - wa.left - ww) / 2, wa.top + (wa.bottom - wa.top - wh) / 2, ww, wh, None, None, Some(inst.into()), None).ok()?;
        let font = CreateFontW(-(14 * GetDpiForWindow(hwnd) as i32 / 96), 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
        let button = |label: &str, id: usize| {
            let l = wz(label);
            if let Ok(b) = CreateWindowExW(WINDOW_EX_STYLE(0), w!("BUTTON"), PCWSTR(l.as_ptr()), WS_CHILD | WS_VISIBLE, 0, 0, 10, 10, Some(hwnd), Some(HMENU(id as *mut _)), Some(inst.into()), None) {
                SendMessageW(b, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            }
        };
        button(save_label, ID_SAVE);
        button("Cancel", ID_CANCEL);
        if mode == Mode::Annotate {
            for (l, id) in [("Box", ID_RECT), ("Arrow", ID_ARROW), ("Text", ID_TEXT), ("Colour", ID_COLOR), ("Undo", ID_UNDO)] { button(l, id); }
        } else if mode == Mode::Redact {
            button("Undo", ID_UNDO);
        }
        layout(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        let mut msg = MSG::default();
        let mut res = None;
        while res.is_none() && GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            res = C.with(|c| c.borrow().as_ref().and_then(|s| s.done));
        }
        let _ = DestroyWindow(hwnd);
        let _ = DeleteObject(font.into());
        let st = C.with(|c| c.borrow_mut().take())?;
        (res == Some(true)).then_some(st.shapes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shapes_round_trip() {
        let v = vec![Shape::rect(0.1, 0.2, 0.3, 0.4), Shape { tool: Tool::Text, a: (0.5, 0.5), b: (0.5, 0.5), color: 0xFF3B30, text: "hi\tthere".into() }];
        let back = decode_all(&encode_all(&v));
        assert_eq!(back[0], v[0]);
        assert_eq!(back[1].text, "hi there");
    }
    #[test]
    fn bounds_normalise_and_clamp() {
        let (x, y, w, h) = Shape::rect(0.8, 1.5, 0.2, 0.5).bounds();
        assert!((x - 0.2).abs() < 1e-9 && (y - 0.5).abs() < 1e-9 && (w - 0.6).abs() < 1e-9 && (h - 0.5).abs() < 1e-9);
    }
    #[test]
    fn annotation_changes_pixels() {
        let mut b = Bmp::new(200, 200, [255, 255, 255, 255]);
        apply_annotations(&mut b, &[Shape { tool: Tool::Rect, a: (0.1, 0.1), b: (0.9, 0.9), color: 0xFF0000, text: String::new() }]).unwrap();
        assert!(b.px.chunks_exact(4).any(|p| p[2] == 255 && p[0] == 0 && p[3] == 255));
    }
}
