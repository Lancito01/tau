//! Shared image editor window: crop, redact and annotate. Coordinates are fractions of the image,
//! so one result can apply across a batch or to a video of any resolution.
use crate::form;
use crate::gfx::{cursor_pos, dpi_scale_at, wide, work_area_at, ACCENT, BORDER, MUTED, ON_ACCENT, RAISED, SURFACE, TEXT};
use crate::image::Bmp;
use crate::timeline::{snap, timecode};
use crate::ui::{self, glyph, id, r, Align, Page, Style, Ui, FOOT_H, PAD, R, TITLE_H};
use std::sync::{Arc, Mutex};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode { Crop, Redact, Annotate }

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tool { Rect, Arrow, Text, Ellipse }

/// How one shape looks, kept per shape.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Fx {
    /// A redaction that blurs instead of covering in `color`.
    pub blur: bool,
    /// Blur strength, 1 to 100.
    pub strength: f32,
    /// Softness of a redaction's edge, 0 to 100.
    pub feather: f32,
    /// Multiplies the line width and text size of an annotation.
    pub width: f32,
}
impl Default for Fx { fn default() -> Fx { Fx { blur: false, strength: 40.0, feather: 0.0, width: 1.0 } } }

#[derive(Clone, PartialEq, Debug)]
pub struct Shape { pub tool: Tool, pub a: (f64, f64), pub b: (f64, f64), pub color: u32, pub text: String, pub fx: Fx }

/// Marks in pixels for a picture of this size. Strength and feather scale with the picture, so the preview, a video
/// frame and the full-size result all look alike. `even` grows boxes to whole chroma samples, for video.
pub fn marks_for(shapes: &[Shape], w: u32, h: u32, even: bool) -> Vec<crate::redact::Mark> {
    use crate::redact::{Fill, Mark};
    let long = w.max(h) as f64;
    shapes.iter().map(|s| {
        let (fx, fy, fw, fh) = s.bounds();
        // Rounded outward at every edge, so that what was marked is covered even when it falls between pixels.
        let (x, y) = (((fx * w as f64).floor() as u32).min(w.saturating_sub(1)), ((fy * h as f64).floor() as u32).min(h.saturating_sub(1)));
        let (x1, y1) = (((fx + fw) * w as f64).ceil() as u32, ((fy + fh) * h as f64).ceil() as u32);
        let (mw, mh) = (x1.saturating_sub(x).clamp(1, w - x), y1.saturating_sub(y).clamp(1, h - y));
        let (x, y, mw, mh) = if even { crate::quality::redact_box(x, y, mw, mh, w, h) } else { (x, y, mw, mh) };
        let fill = if s.fx.blur { Fill::Blur(s.fx.strength as f64 / 100.0 * 0.04 * long) } else {
            Fill::Solid([(s.color >> 16) as u8, (s.color >> 8) as u8, s.color as u8])
        };
        Mark { x, y, w: mw, h: mh, ellipse: s.tool == Tool::Ellipse, fill, feather: (s.fx.feather as f64 / 100.0 * 0.1 * long) as u32 }
    }).collect()
}

impl Shape {
    pub fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Shape { Shape { tool: Tool::Rect, a: (x0, y0), b: (x1, y1), color: 0, text: String::new(), fx: Fx::default() } }
    /// Normalised (x, y, w, h) fractions.
    pub fn bounds(&self) -> (f64, f64, f64, f64) {
        let (x0, x1) = (self.a.0.min(self.b.0).clamp(0.0, 1.0), self.a.0.max(self.b.0).clamp(0.0, 1.0));
        let (y0, y1) = (self.a.1.min(self.b.1).clamp(0.0, 1.0), self.a.1.max(self.b.1).clamp(0.0, 1.0));
        (x0, y0, x1 - x0, y1 - y0)
    }
    pub fn encode(&self) -> String {
        let t = match self.tool { Tool::Rect => "rect", Tool::Arrow => "arrow", Tool::Text => "text", Tool::Ellipse => "ellipse" };
        format!("{t}\t{}\t{}\t{}\t{}\t{:06X}\t{}\t{}\t{}\t{}\t{}", self.a.0, self.a.1, self.b.0, self.b.1, self.color, self.text.replace(['\t', '\n'], " "),
            self.fx.blur as u8, self.fx.strength, self.fx.feather, self.fx.width)
    }
    pub fn decode(s: &str) -> Option<Shape> {
        let f: Vec<&str> = s.split('\t').collect();
        let n = |i: usize| f.get(i)?.parse::<f64>().ok();
        Some(Shape {
            tool: match *f.first()? { "rect" => Tool::Rect, "arrow" => Tool::Arrow, "text" => Tool::Text, "ellipse" => Tool::Ellipse, _ => return None },
            a: (n(1)?, n(2)?), b: (n(3)?, n(4)?),
            color: u32::from_str_radix(f.get(5)?, 16).ok()?,
            text: f.get(6).unwrap_or(&"").to_string(),
            fx: {
                let d = Fx::default();
                let v = |i: usize, or: f32| f.get(i).and_then(|x| x.parse().ok()).unwrap_or(or);
                Fx { blur: f.get(7) == Some(&"1"), strength: v(8, d.strength), feather: v(9, d.feather), width: v(10, d.width) }
            },
        })
    }
}

pub fn encode_all(v: &[Shape]) -> String { v.iter().map(Shape::encode).collect::<Vec<_>>().join("\n") }
pub fn decode_all(s: &str) -> Vec<Shape> { s.lines().filter_map(Shape::decode).collect() }

const COLORS: [u32; 5] = [0xFF3B30, 0xFFCC00, 0x34C759, 0x0A84FF, 0xFFFFFF];
const REDACT_COLORS: [u32; 5] = [0x000000, 0xFFFFFF, 0xFF3B30, 0x34C759, 0x0A84FF];

fn colorref(c: u32) -> COLORREF { COLORREF(((c >> 16) & 255) | (c & 0xFF00) | ((c & 255) << 16)) }

fn bmi(w: u32, h: u32) -> BITMAPINFO {
    BITMAPINFO { bmiHeader: BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w as i32, biHeight: -(h as i32),
        biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default()
    }, ..Default::default() }
}

/// Stroke width for an image of this size, in image pixels.
fn thickness(w: u32, h: u32) -> i32 { (w.max(h) as i32 / 220).max(3) }

/// Draws shapes into a DC where the image occupies (ox, oy, w, h).
unsafe fn draw_shapes(dc: HDC, shapes: &[Shape], ox: i32, oy: i32, w: i32, h: i32) {
    let map = |p: (f64, f64)| (ox + (p.0 * w as f64) as i32, oy + (p.1 * h as f64) as i32);
    let base = thickness(w as u32, h as u32);
    let null = GetStockObject(NULL_BRUSH);
    for s in shapes {
        let thick = ((base as f32 * s.fx.width) as i32).max(1);
        let (ax, ay) = map(s.a);
        let (bx, by) = map(s.b);
        let pen = CreatePen(PS_SOLID, thick, colorref(s.color));
        let old = SelectObject(dc, pen.into());
        match s.tool {
            Tool::Rect => {
                let ob = SelectObject(dc, null);
                let _ = Rectangle(dc, ax, ay, bx, by);
                SelectObject(dc, ob);
            }
            Tool::Ellipse => {
                let ob = SelectObject(dc, null);
                let _ = Ellipse(dc, ax, ay, bx, by);
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
        draw_shapes(dc, shapes, 0, 0, b.w as i32, b.h as i32);
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

// ---- geometry, kept apart from the window so it can be tested ----

type P = (f64, f64);
const ASPECTS: [Option<f64>; 5] = [None, Some(1.0), Some(4.0 / 3.0), Some(16.0 / 9.0), Some(9.0 / 16.0)];
const ASPECT_NAMES: [&str; 5] = ["Free", "1:1", "4:3", "16:9", "9:16"];

/// Where the corner being dragged lands so the box keeps its ratio and stays on the image.
pub fn constrain(anchor: P, moving: P, ratio: f64, iw: f64, ih: f64) -> P {
    let (dx, dy) = (moving.0 - anchor.0, moving.1 - anchor.1);
    let (sx, sy) = (if dx < 0.0 { -1.0 } else { 1.0 }, if dy < 0.0 { -1.0 } else { 1.0 });
    let (mut w, mut h) = (dx.abs() * iw, dy.abs() * ih);
    if w / ratio > h { h = w / ratio; } else { w = h * ratio; }
    let room_w = (if sx > 0.0 { 1.0 - anchor.0 } else { anchor.0 }) * iw;
    let room_h = (if sy > 0.0 { 1.0 - anchor.1 } else { anchor.1 }) * ih;
    if w > room_w { w = room_w; h = w / ratio; }
    if h > room_h { h = room_h; w = h * ratio; }
    (anchor.0 + sx * w / iw, anchor.1 + sy * h / ih)
}

/// The largest box of this ratio centred inside the given one.
pub fn fit_aspect(rect: (f64, f64, f64, f64), ratio: f64, iw: f64, ih: f64) -> Shape {
    let (x, y, w, h) = rect;
    let (mut pw, mut ph) = (w * iw, h * ih);
    if pw / ph > ratio { pw = ph * ratio; } else { ph = pw / ratio; }
    let (fw, fh) = (pw / iw, ph / ih);
    let (x0, y0) = (x + (w - fw) / 2.0, y + (h - fh) / 2.0);
    Shape::rect(x0, y0, x0 + fw, y0 + fh)
}

/// Grab points as (which, point). 0-3 are the corners clockwise from top left, 4-7 the edges
/// from the top, and 8 and 9 the ends of an arrow.
fn handles(s: &Shape, edges: bool) -> Vec<(u8, P)> {
    match s.tool {
        Tool::Text => vec![],
        Tool::Arrow => vec![(8, s.a), (9, s.b)],
        Tool::Rect | Tool::Ellipse => {
            let (x, y, w, h) = s.bounds();
            let (x1, y1) = (x + w, y + h);
            let mut v = vec![(0, (x, y)), (1, (x1, y)), (2, (x1, y1)), (3, (x, y1))];
            if edges { v.extend([(4, (x + w / 2.0, y)), (5, (x1, y + h / 2.0)), (6, (x + w / 2.0, y1)), (7, (x, y + h / 2.0))]); }
            v
        }
    }
}

/// The shape after dragging one of its grab points to p. The opposite side stays put.
fn apply_handle(orig: &Shape, which: u8, p: P, aspect: Option<(f64, f64, f64)>) -> Shape {
    let mut s = orig.clone();
    let (x, y, w, h) = orig.bounds();
    let (x1, y1) = (x + w, y + h);
    match which {
        0..=3 => {
            let anchor = [(x1, y1), (x, y1), (x, y), (x1, y)][which as usize];
            s.a = anchor;
            s.b = match aspect { Some((ratio, iw, ih)) => constrain(anchor, p, ratio, iw, ih), None => p };
        }
        4 => { s.a = (x, p.1); s.b = (x1, y1); }
        5 => { s.a = (x, y); s.b = (p.0, y1); }
        6 => { s.a = (x, y); s.b = (x1, p.1); }
        7 => { s.a = (p.0, y); s.b = (x1, y1); }
        8 => s.a = p,
        _ => s.b = p,
    }
    s
}

/// The shape moved by a delta, stopping at the edges of the image.
fn moved(orig: &Shape, d: P) -> Shape {
    let (x0, x1) = (orig.a.0.min(orig.b.0), orig.a.0.max(orig.b.0));
    let (y0, y1) = (orig.a.1.min(orig.b.1), orig.a.1.max(orig.b.1));
    let (dx, dy) = (d.0.clamp(-x0, 1.0 - x1), d.1.clamp(-y0, 1.0 - y1));
    let mut s = orig.clone();
    s.a = (s.a.0 + dx, s.a.1 + dy);
    s.b = (s.b.0 + dx, s.b.1 + dy);
    s
}

fn seg_distance(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = dx * dx + dy * dy;
    let t = if len > 0.0 { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len).clamp(0.0, 1.0) } else { 0.0 };
    ((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt()
}

/// True when the point, given as a fraction, is on the shape. vw and vh are the on-screen image
/// size in pixels, em the on-screen text size.
fn on_shape(s: &Shape, p: P, vw: f64, vh: f64, em: f64) -> bool {
    let px = |q: P| (q.0 * vw, q.1 * vh);
    let m = px(p);
    match s.tool {
        Tool::Rect => {
            let (x, y, w, h) = s.bounds();
            p.0 >= x && p.0 <= x + w && p.1 >= y && p.1 <= y + h
        }
        Tool::Ellipse => {
            let (x, y, w, h) = s.bounds();
            let (rx, ry) = ((w / 2.0).max(1e-6), (h / 2.0).max(1e-6));
            ((p.0 - x - rx) / rx).powi(2) + ((p.1 - y - ry) / ry).powi(2) <= 1.0
        }
        Tool::Arrow => seg_distance(m, px(s.a), px(s.b)) <= 8.0,
        Tool::Text => {
            let a = px(s.a);
            let em = em * s.fx.width as f64;
            m.0 >= a.0 && m.0 <= a.0 + s.text.chars().count() as f64 * em * 0.56 && m.1 >= a.1 && m.1 <= a.1 + em * 1.35
        }
    }
}

/// The topmost shape under the point.
fn shape_at(items: &[Item], p: P, vw: f64, vh: f64, em: f64) -> Option<usize> {
    items.iter().rposition(|i| on_shape(&i.shape, p, vw, vh, em))
}

// ---- the window ----

const SIDE: f32 = 272.0;
const SCRUB_H: f32 = 56.0;
const ROW: f32 = 46.0;
const GRAB: f32 = 10.0;
const PREVIEW_MAX: u32 = 2400;
/// Redacting repaints the picture as marks move, so it works on a copy no bigger than a window can show.
const REDACT_PREVIEW_MAX: u32 = 1600;
/// What a selected redaction's row adds for its own settings.
const ROW_FEATHER: f32 = 48.0;
const STAGE_BG: u32 = 0x0A0B0C;

/// Lets the video editors fetch other frames while the window is open.
pub struct FrameSource {
    pub dur: f64,
    pub fps: f64,
    pub load: Arc<dyn Fn(f64) -> std::result::Result<Bmp, String> + Send + Sync>,
}

struct Scrub {
    source: FrameSource,
    pos: f32,
    requested: f32,
    loading: bool,
    slot: Arc<Mutex<Option<std::result::Result<Bmp, String>>>>,
    error: Option<String>,
}

#[derive(Clone, PartialEq)]
struct Item { shape: Shape, detected: bool }

enum Drag {
    New(Shape),
    Move { i: usize, start: P, orig: Shape },
    Handle { i: usize, which: u8, orig: Shape },
}

struct Editor {
    title: String,
    sub: String,
    mode: Mode,
    ok: String,
    preview: Bmp,
    full: (u32, u32),
    items: Vec<Item>,
    undo: Vec<Vec<Item>>,
    redo: Vec<Vec<Item>>,
    before: Option<Vec<Item>>,
    sel: Option<usize>,
    hover_row: Option<usize>,
    drag: Option<Drag>,
    tool: usize,
    /// Colour and look of the next shape. Changing them also changes the selected one.
    pen: u32,
    fx: Fx,
    oval: bool,
    pending_text: Option<P>,
    pending_color: bool,
    /// A slider is being dragged and its undo step is already taken.
    slider_open: bool,
    /// The picture with the redactions painted in, by the code that paints the saved file.
    shown: Bmp,
    shown_for: Option<Vec<Shape>>,
    shown_marks: Vec<crate::redact::Mark>,
    /// Where changed areas are painted before being copied into `shown`, kept to avoid a large allocation per change.
    scratch: Vec<u8>,
    shown_gen: u64,
    /// The selection the list last scrolled to.
    list_sel: Option<usize>,
    aspect: usize,
    scrub: Option<Scrub>,
    start_size: (f32, f32),
    result: Option<bool>,
}

fn shrink(b: Bmp, mode: Mode) -> Bmp {
    let long = b.w.max(b.h);
    let max = if mode == Mode::Redact { REDACT_PREVIEW_MAX } else { PREVIEW_MAX };
    if long <= max { return b; }
    let f = max as f64 / long as f64;
    let (w, h) = (((b.w as f64 * f) as u32).max(1), ((b.h as f64 * f) as u32).max(1));
    match b.scaled(w, h) { Ok(s) => s, Err(_) => b }
}

fn size_label(s: &Shape, full: (u32, u32)) -> String {
    let (_, _, w, h) = s.bounds();
    format!("{} × {}", (w * full.0 as f64).round() as u32, (h * full.1 as f64).round() as u32)
}

impl Editor {
    fn new(title: &str, bmp: Bmp, mode: Mode, initial: Vec<Shape>, ok: &str, scrub: Option<FrameSource>) -> Editor {
        let full = (bmp.w, bmp.h);
        let preview = shrink(bmp, mode);
        let mut items: Vec<Item> = initial.into_iter().map(|shape| Item { shape, detected: true }).collect();
        let mut sel = None;
        if mode == Mode::Crop {
            if items.is_empty() { items.push(Item { shape: Shape::rect(0.0, 0.0, 1.0, 1.0), detected: false }); }
            items.truncate(1);
            sel = Some(0);
        }
        let at = cursor_pos();
        let (wa, s) = (work_area_at(at), dpi_scale_at(at));
        let start_size = (((wa.right - wa.left) as f32 / s * 0.75).max(900.0), ((wa.bottom - wa.top) as f32 / s * 0.75).max(600.0));
        let scrub = scrub.filter(|f| f.dur > 0.0).map(|source| Scrub { source, pos: 0.0, requested: 0.0, loading: false, slot: Arc::new(Mutex::new(None)), error: None });
        Editor {
            title: title.into(), sub: form::context(), mode, ok: ok.into(), preview: preview.clone(), full, items,
            undo: vec![], redo: vec![], before: None, sel, hover_row: None, drag: None, tool: 0,
            pen: if mode == Mode::Redact { 0 } else { COLORS[0] }, fx: Fx::default(), oval: false, aspect: 0,
            pending_text: None, pending_color: false, slider_open: false, shown: preview, shown_for: None, shown_marks: vec![], scratch: vec![], shown_gen: 0, list_sel: None, scrub, start_size, result: None,
        }
    }

    fn aspect_info(&self) -> Option<(f64, f64, f64)> { ASPECTS[self.aspect].map(|ratio| (ratio, self.full.0 as f64, self.full.1 as f64)) }

    fn is_full_frame(&self) -> bool {
        let (x, y, w, h) = self.items[0].shape.bounds();
        x <= 0.001 && y <= 0.001 && w >= 0.998 && h >= 0.998
    }

    fn ready(&self) -> bool {
        if self.mode == Mode::Crop { !self.is_full_frame() } else { !self.items.is_empty() }
    }

    fn output(&self) -> Vec<Shape> {
        self.items.iter().map(|i| {
            if self.mode != Mode::Crop { return i.shape.clone(); }
            let (x, y, w, h) = i.shape.bounds();
            Shape::rect(x, y, x + w, y + h)
        }).collect()
    }

    fn checkpoint(&mut self) { self.undo.push(self.items.clone()); self.redo.clear(); }

    fn fix_selection(&mut self) {
        if self.mode == Mode::Crop { self.sel = Some(0); }
        else if self.sel.is_some_and(|i| i >= self.items.len()) { self.sel = None; }
    }
    fn step_undo(&mut self) {
        if let Some(s) = self.undo.pop() { self.redo.push(std::mem::replace(&mut self.items, s)); self.fix_selection(); }
    }
    fn step_redo(&mut self) {
        if let Some(s) = self.redo.pop() { self.undo.push(std::mem::replace(&mut self.items, s)); self.fix_selection(); }
    }

    fn remove(&mut self, i: usize) {
        self.checkpoint();
        self.items.remove(i);
        self.sel = match self.sel { Some(s) if s == i => None, Some(s) if s > i => Some(s - 1), s => s };
    }

    /// The image rectangle on screen, and the text size that matches the saved result.
    fn fit(&self, stage: R) -> (R, f32) {
        let q = stage.inset(16.0);
        let s = (q.w / self.preview.w as f32).min(q.h / self.preview.h as f32);
        let (w, h) = (self.preview.w as f32 * s, self.preview.h as f32 * s);
        let v = r(q.x + (q.w - w) / 2.0, q.y + (q.h - h) / 2.0, w, h);
        let t = thickness(self.full.0, self.full.1) as f32 * v.w / self.full.0 as f32;
        (v, t)
    }

    // ---- stage ----

    fn stage(&mut self, ui: &mut Ui, stage: R) {
        ui.fill(stage, 0.0, STAGE_BG, 1.0);
        let (v, thick) = self.fit(stage);
        let em = (thick * 8.0) as f64;
        let to_frac = |m: (f32, f32)| (((m.0 - v.x) / v.w).clamp(0.0, 1.0) as f64, ((m.1 - v.y) / v.h).clamp(0.0, 1.0) as f64);
        let to_view = |p: P| (v.x + p.0 as f32 * v.w, v.y + p.1 as f32 * v.h);
        let (hover, _, _) = ui.hit(id("ed.stage"), stage);
        let p = to_frac(ui.mouse);
        let edges = self.mode == Mode::Crop && self.aspect == 0;

        let grabbed = self.sel.and_then(|i| {
            let s = &self.items.get(i)?.shape;
            handles(s, edges).into_iter().find(|(_, h)| {
                let q = to_view(*h);
                (q.0 - ui.mouse.0).abs() <= GRAB && (q.1 - ui.mouse.1).abs() <= GRAB
            }).map(|(w, _)| (i, w))
        });
        let under = shape_at(&self.items, p, v.w as f64, v.h as f64, em);
        let selecting = self.mode == Mode::Redact || (self.mode == Mode::Annotate && self.tool == 0);

        if hover && ui.pressed {
            self.before = Some(self.items.clone());
            if let Some((i, which)) = grabbed {
                self.drag = Some(Drag::Handle { i, which, orig: self.items[i].shape.clone() });
            } else {
                match self.mode {
                    Mode::Crop => {
                        self.drag = Some(if under.is_some() {
                            Drag::Move { i: 0, start: p, orig: self.items[0].shape.clone() }
                        } else {
                            Drag::Handle { i: 0, which: 2, orig: Shape::rect(p.0, p.1, p.0, p.1) }
                        });
                    }
                    _ if selecting => {
                        self.sel = under;
                        if let Some(i) = under {
                            self.drag = Some(Drag::Move { i, start: p, orig: self.items[i].shape.clone() });
                        } else if self.mode == Mode::Redact {
                            let tool = if self.oval { Tool::Ellipse } else { Tool::Rect };
                            self.drag = Some(Drag::New(Shape { tool, a: p, b: p, color: self.pen, text: String::new(), fx: self.fx }));
                        }
                    }
                    _ if self.tool == 3 => { self.pending_text = Some(p); self.before = None; }
                    _ => {
                        let tool = match self.tool { 1 => Tool::Rect, 4 => Tool::Ellipse, _ => Tool::Arrow };
                        self.sel = None;
                        self.drag = Some(Drag::New(Shape { tool, a: p, b: p, color: self.pen, text: String::new(), fx: Fx { width: self.fx.width, ..Fx::default() } }));
                    }
                }
            }
        }
        if ui.down {
            let aspect = self.aspect_info().filter(|_| self.mode == Mode::Crop);
            match &mut self.drag {
                Some(Drag::New(s)) => s.b = p,
                Some(Drag::Move { i, start, orig }) => self.items[*i].shape = moved(orig, (p.0 - start.0, p.1 - start.1)),
                Some(Drag::Handle { i, which, orig }) => self.items[*i].shape = apply_handle(orig, *which, p, aspect),
                None => {}
            }
        }
        if ui.released { self.finish_drag(); }
        if hover && ui.rpressed && self.mode != Mode::Crop && self.drag.is_none() {
            if let Some(i) = under { self.remove(i); }
        }

        if hover {
            ui.cursor = match (grabbed.map(|g| g.1), self.drag.is_some()) {
                (Some(0 | 2), _) => IDC_SIZENWSE,
                (Some(1 | 3), _) => IDC_SIZENESW,
                (Some(4 | 6), _) => IDC_SIZENS,
                (Some(5 | 7), _) => IDC_SIZEWE,
                (Some(_), _) => IDC_SIZEALL,
                (None, true) => IDC_CROSS,
                (None, false) if under.is_some() && (selecting || self.mode == Mode::Crop) => IDC_SIZEALL,
                (None, false) if self.mode == Mode::Annotate && self.tool == 0 => IDC_ARROW,
                (None, false) => IDC_CROSS,
            };
        }

        // Paint.
        if self.mode == Mode::Redact {
            self.refresh_shown(ui);
            ui.image(id(&format!("ed.shown{}", self.shown_gen)), &self.shown, v);
        } else {
            ui.image(id("ed.image"), &self.preview, v);
        }
        ui.clip(v);
        if self.mode == Mode::Crop {
            self.paint_crop(ui, v);
        } else {
            for i in 0..self.items.len() {
                let (selected, lit) = (self.sel == Some(i), self.hover_row == Some(i));
                let s = self.items[i].shape.clone();
                self.paint_shape(ui, &s, v, thick, selected, lit);
            }
            if let Some(Drag::New(s)) = &self.drag { let s = s.clone(); self.paint_shape(ui, &s, v, thick, true, false); }
        }
        ui.unclip();
        if self.mode != Mode::Crop {
            if let Some(i) = self.sel.filter(|i| *i < self.items.len()) {
                for (_, h) in handles(&self.items[i].shape, false) {
                    let q = to_view(h);
                    ui.dot(q.0, q.1, 5.5, ON_ACCENT, 1.0);
                    ui.dot(q.0, q.1, 4.0, ACCENT, 1.0);
                }
            }
        }
    }

    fn finish_drag(&mut self) {
        let Some(drag) = self.drag.take() else { return };
        if let Drag::New(s) = drag {
            let (_, _, w, h) = s.bounds();
            if w.max(h) >= 0.005 {
                self.items.push(Item { shape: s, detected: false });
                self.sel = Some(self.items.len() - 1);
            }
        }
        for it in &mut self.items {
            if matches!(it.shape.tool, Tool::Rect | Tool::Ellipse) {
                let (x, y, w, h) = it.shape.bounds();
                it.shape.a = (x, y);
                it.shape.b = (x + w, y + h);
            }
        }
        let Some(before) = self.before.take() else { return };
        if self.mode == Mode::Crop {
            let (_, _, w, h) = self.items[0].shape.bounds();
            if w < 0.005 || h < 0.005 { self.items = before; return; }
        }
        if before != self.items {
            self.undo.push(before);
            self.redo.clear();
        }
    }

    /// Paints the redactions into the preview with the same code as the saved file, redoing only the part that changed.
    /// A mark being drawn shows as it grows. A blur waits for the mouse button, since it is too slow to redo for every
    /// pixel of a drag; the outline follows meanwhile.
    fn refresh_shown(&mut self, ui: &mut Ui) {
        use crate::redact::{apply, apply_within, change_between, Layout};
        let mut now: Vec<Shape> = self.items.iter().map(|i| i.shape.clone()).collect();
        if let Some(Drag::New(s)) = &self.drag { if !s.fx.blur { now.push(s.clone()); } }
        if self.shown_for.as_ref() == Some(&now) { return; }
        let (w, h) = (self.preview.w, self.preview.h);
        let marks = marks_for(&now, w, h, false);
        let l = Layout { w, h, stride: w as usize * 4, sample_bytes: 1, channels: 4, rgb: [2, 1, 0], alpha: Some(3) };
        if self.shown_for.is_none() {
            let mut b = self.preview.clone();
            apply(&mut b.px, &l, &marks);
            ui.forget_image(id(&format!("ed.shown{}", self.shown_gen)));
            self.shown_gen += 1;
            self.shown = b;
        } else if let Some(c) = change_between(&self.shown_marks, &marks, w, h) {
            if ui.down && c.blur { return; }
            if self.scratch.len() != self.preview.px.len() { self.scratch = self.preview.px.clone(); }
            let row = |y: u32, a: (u32, u32, u32, u32)| (y as usize * w as usize + a.0 as usize) * 4..(y as usize * w as usize + a.2 as usize) * 4;
            for y in c.work.1..c.work.3 { self.scratch[row(y, c.work)].copy_from_slice(&self.preview.px[row(y, c.work)]); }
            apply_within(&mut self.scratch, &l, &marks, c.work);
            for y in c.shown.1..c.shown.3 { self.shown.px[row(y, c.shown)].copy_from_slice(&self.scratch[row(y, c.shown)]); }
            ui.update_image(id(&format!("ed.shown{}", self.shown_gen)), &self.shown, c.shown);
        }
        self.shown_for = Some(now);
        self.shown_marks = marks;
    }

    fn paint_shape(&self, ui: &mut Ui, s: &Shape, v: R, thick: f32, selected: bool, lit: bool) {
        let thick = thick * s.fx.width;
        let at = |p: P| (v.x + p.0 as f32 * v.w, v.y + p.1 as f32 * v.h);
        let (a, b) = (at(s.a), at(s.b));
        let (bx, by, bw, bh) = s.bounds();
        let q = r(v.x + bx as f32 * v.w, v.y + by as f32 * v.h, bw as f32 * v.w, bh as f32 * v.h);
        if self.mode == Mode::Redact {
            // The fill itself is already in the picture. This only shows where the mark is.
            if selected || lit || self.drag.is_some() {
                if s.tool == Tool::Ellipse { ui.oval_stroke(q, ACCENT, 1.0, 1.5); } else { ui.stroke(q, 0.0, ACCENT, 1.0, 1.5); }
            }
            return;
        }
        match s.tool {
            Tool::Rect => {
                ui.stroke(q, 0.0, s.color, 1.0, thick);
                if selected || lit { ui.stroke(q.inset(-thick / 2.0 - 2.0), 0.0, ACCENT, 0.9, 1.0); }
            }
            Tool::Ellipse => {
                ui.oval_stroke(q, s.color, 1.0, thick);
                if selected || lit { ui.stroke(q.inset(-thick / 2.0 - 2.0), 0.0, ACCENT, 0.9, 1.0); }
            }
            Tool::Arrow => {
                if lit { ui.line(a, b, ACCENT, 0.5, thick + 4.0); }
                ui.line(a, b, s.color, 1.0, thick);
                let ang = (a.1 - b.1).atan2(a.0 - b.0);
                for d in [-0.45f32, 0.45] {
                    ui.line(b, (b.0 + (ang + d).cos() * thick * 5.0, b.1 + (ang + d).sin() * thick * 5.0), s.color, 1.0, thick);
                }
            }
            Tool::Text => {
                let em = thick * 8.0;
                let w = ui.measure(&s.text, em, 600) + 4.0;
                let tq = r(a.0, a.1, w, em * 1.35);
                ui.text(&s.text, tq, em, 600, s.color, Align::Left);
                if selected || lit { ui.stroke(tq.inset(-3.0), 2.0, ACCENT, 0.9, 1.0); }
            }
        }
    }

    fn paint_crop(&self, ui: &mut Ui, v: R) {
        let (x, y, w, h) = self.items[0].shape.bounds();
        let q = r(v.x + x as f32 * v.w, v.y + y as f32 * v.h, w as f32 * v.w, h as f32 * v.h);
        const DIM: f32 = 0.6;
        ui.fill(r(v.x, v.y, v.w, q.y - v.y), 0.0, 0, DIM);
        ui.fill(r(v.x, q.bottom(), v.w, v.bottom() - q.bottom()), 0.0, 0, DIM);
        ui.fill(r(v.x, q.y, q.x - v.x, q.h), 0.0, 0, DIM);
        ui.fill(r(q.right(), q.y, v.right() - q.right(), q.h), 0.0, 0, DIM);
        for k in [1.0, 2.0] {
            let (tx, ty) = (q.x + q.w * k / 3.0, q.y + q.h * k / 3.0);
            ui.fill(r(tx - 0.5, q.y, 1.0, q.h), 0.0, TEXT, 0.22);
            ui.fill(r(q.x, ty - 0.5, q.w, 1.0), 0.0, TEXT, 0.22);
        }
        ui.stroke(q, 0.0, ACCENT, 1.0, 1.5);
        let edges = self.aspect == 0;
        for (which, p) in handles(&self.items[0].shape, edges) {
            let (hx, hy) = (v.x + p.0 as f32 * v.w, v.y + p.1 as f32 * v.h);
            let (w, h) = match which { 0..=3 => (11.0, 11.0), 4 | 6 => (22.0, 5.0), _ => (5.0, 22.0) };
            let g = r(hx - w / 2.0, hy - h / 2.0, w, h);
            ui.fill(g.inset(-1.0), 3.0, ON_ACCENT, 1.0);
            ui.fill(g, 2.5, ACCENT, 1.0);
        }
    }

    // ---- sidebar ----

    fn sidebar(&mut self, ui: &mut Ui, q: R) {
        ui.fill(q, 0.0, SURFACE, 0.6);
        ui.fill(r(q.x, q.y, 1.0, q.h), 0.0, BORDER, 0.7);
        let (x, w) = (q.x + PAD, q.w - PAD * 2.0);
        let mut y = q.y + 14.0;

        let heading = match self.mode {
            Mode::Crop => "Crop".to_string(),
            Mode::Redact if self.items.is_empty() => "Redactions".to_string(),
            Mode::Redact => format!("Redactions  ·  {}", self.items.len()),
            Mode::Annotate => "Markup".to_string(),
        };
        ui.text(&heading, r(x, y, w - 64.0, 30.0), 13.0, 600, TEXT, Align::Left);
        let (can_undo, can_redo) = (!self.undo.is_empty(), !self.redo.is_empty());
        if ui.icon_button_if("ed.undo", glyph::UNDO, r(x + w - 62.0, y, 30.0, 30.0), 13.0, can_undo) { self.step_undo(); }
        if ui.icon_button_if("ed.redo", glyph::REDO, r(x + w - 30.0, y, 30.0, 30.0), 13.0, can_redo) { self.step_redo(); }
        y += 42.0;

        match self.mode {
            Mode::Crop => {
                ui.text("Aspect ratio", r(x, y, w, 18.0), 12.5, 500, MUTED, Align::Left);
                let names: Vec<String> = ASPECT_NAMES.iter().map(|s| s.to_string()).collect();
                let was = self.aspect;
                ui.segmented("ed.aspect", r(x, y + 22.0, w, 34.0), &names, &mut self.aspect);
                if self.aspect != was {
                    if let Some(ratio) = ASPECTS[self.aspect] {
                        self.checkpoint();
                        self.items[0].shape = fit_aspect(self.items[0].shape.bounds(), ratio, self.full.0 as f64, self.full.1 as f64);
                    }
                }
                y += 72.0;
                ui.fill(r(x, y, w, 58.0), 10.0, RAISED, 0.7);
                ui.text("Selection", r(x + 12.0, y + 8.0, w - 24.0, 18.0), 11.5, 400, MUTED, Align::Left);
                ui.text(&size_label(&self.items[0].shape, self.full), r(x + 12.0, y + 26.0, w - 24.0, 24.0), 15.0, 600, TEXT, Align::Left);
                y += 70.0;
                let full = self.is_full_frame();
                if ui.button("ed.reset", "Reset", r(x, y, w, 34.0), Style::Secondary, !full) {
                    self.checkpoint();
                    self.aspect = 0;
                    self.items[0].shape = Shape::rect(0.0, 0.0, 1.0, 1.0);
                }
            }
            Mode::Annotate => {
                let names: Vec<String> = ["Select", "Box", "Arrow", "Text", "Oval"].iter().map(|s| s.to_string()).collect();
                ui.segmented("ed.tool", r(x, y, w, 34.0), &names, &mut self.tool);
                y += 46.0;
                self.swatches(ui, x, y, &COLORS);
                y += 40.0;
                let sel = self.sel.filter(|s| *s < self.items.len());
                let mut width = sel.map(|i| self.items[i].shape.fx.width).unwrap_or(self.fx.width) * 2.0;
                ui.text("Line and text size", r(x, y, w - 50.0, 18.0), 12.5, 500, MUTED, Align::Left);
                ui.text(&format!("{}x", width / 2.0), r(x + w - 50.0, y, 50.0, 18.0), 12.5, 600, MUTED, Align::Right);
                let was = width;
                ui.slider("ed.width", r(x - 9.0, y + 18.0, w + 18.0, 28.0), 1.0, 10.0, 2.0, &mut width);
                if width != was { self.restyle(|f| f.width = width / 2.0); }
                if !ui.down { self.slider_open = false; }
                y += 52.0;
                self.list(ui, r(q.x, y, q.w - 1.0, q.bottom() - y - 58.0));
                self.clear_button(ui, x, w, q);
            }
            Mode::Redact => {
                y = self.redact_controls(ui, x, w, y);
                self.list(ui, r(q.x, y, q.w - 1.0, q.bottom() - y - 58.0));
                self.clear_button(ui, x, w, q);
            }
        }
    }

    /// Changes the look of the next shape and of the selected one. Dragging a slider makes one undo step.
    fn restyle(&mut self, change: impl Fn(&mut Fx)) {
        change(&mut self.fx);
        if let Some(i) = self.sel.filter(|i| *i < self.items.len()) {
            let mut fx = self.items[i].shape.fx;
            change(&mut fx);
            if fx != self.items[i].shape.fx {
                if !self.slider_open { self.checkpoint(); self.slider_open = true; }
                self.items[i].shape.fx = fx;
            }
        }
    }

    fn recolor(&mut self, c: u32) {
        self.pen = c;
        if let Some(i) = self.sel.filter(|i| *i < self.items.len()) {
            if self.items[i].shape.color != c {
                self.checkpoint();
                self.items[i].shape.color = c;
            }
        }
    }

    /// A row of colours and a button for any other. Shows the selected shape's colour when there is one.
    fn swatches(&mut self, ui: &mut Ui, x: f32, y: f32, colors: &[u32]) {
        let current = self.sel.and_then(|i| self.items.get(i)).map(|i| i.shape.color).unwrap_or(self.pen);
        for (i, c) in colors.iter().enumerate() {
            let cx = x + 14.0 + i as f32 * 36.0;
            let (hov, _, clicked) = ui.hit(id(&format!("ed.col{i}")), r(cx - 14.0, y, 28.0, 28.0));
            if clicked { self.recolor(*c); }
            if current == *c { ui.ring(cx, y + 14.0, 13.0, ACCENT, 1.0, 2.0); }
            else if hov { ui.ring(cx, y + 14.0, 13.0, TEXT, 0.3, 1.5); }
            ui.dot(cx, y + 14.0, 9.0, *c, 1.0);
            ui.ring(cx, y + 14.0, 9.0, TEXT, 0.25, 1.0);
        }
        let cx = x + 14.0 + colors.len() as f32 * 36.0;
        let custom = !colors.contains(&current);
        if custom { ui.dot(cx, y + 14.0, 9.0, current, 1.0); }
        if ui.icon_button("ed.custom", if custom { glyph::CHECK } else { glyph::ADD }, r(cx - 14.0, y, 28.0, 28.0), 11.0, false) { self.pending_color = true; }
    }

    fn redact_controls(&mut self, ui: &mut Ui, x: f32, w: f32, mut y: f32) -> f32 {
        let sel = self.sel.filter(|i| *i < self.items.len());
        let fx = sel.map(|i| self.items[i].shape.fx).unwrap_or(self.fx);
        let is_oval = sel.map(|i| self.items[i].shape.tool == Tool::Ellipse).unwrap_or(self.oval);

        let mut shape = is_oval as usize;
        let names: Vec<String> = ["Box", "Ellipse"].iter().map(|s| s.to_string()).collect();
        ui.segmented("ed.shape", r(x, y, w, 32.0), &names, &mut shape);
        if (shape == 1) != is_oval {
            self.oval = shape == 1;
            if let Some(i) = sel {
                self.checkpoint();
                self.items[i].shape.tool = if shape == 1 { Tool::Ellipse } else { Tool::Rect };
            }
        }
        y += 40.0;

        let mut fill = fx.blur as usize;
        let names: Vec<String> = ["Colour", "Blur"].iter().map(|s| s.to_string()).collect();
        ui.segmented("ed.fill", r(x, y, w, 32.0), &names, &mut fill);
        if (fill == 1) != fx.blur {
            let blur = fill == 1;
            self.slider_open = false;
            self.restyle(|f| f.blur = blur);
        }
        y += 40.0;

        if fx.blur {
            let mut v = fx.strength;
            ui.text("Strength", r(x, y, w - 50.0, 18.0), 12.5, 500, MUTED, Align::Left);
            ui.text(&format!("{}", v as i32), r(x + w - 50.0, y, 50.0, 18.0), 12.5, 600, MUTED, Align::Right);
            ui.slider("ed.strength", r(x - 9.0, y + 18.0, w + 18.0, 28.0), 1.0, 100.0, 40.0, &mut v);
            if v != fx.strength { self.restyle(|f| f.strength = v); }
            y += 50.0;
        } else {
            self.swatches(ui, x, y, &REDACT_COLORS);
            y += 38.0;
        }

        if !ui.down { self.slider_open = false; }

        if fx.blur {
            ui.wrapped("A weak blur can sometimes be reversed. For anything sensitive, use a solid colour.", r(x, y, w, 34.0), 11.5, 400, crate::gfx::DANGER);
            y += 40.0;
        }
        y
    }

    fn clear_button(&mut self, ui: &mut Ui, x: f32, w: f32, q: R) {
        let any = !self.items.is_empty();
        if ui.button("ed.clear", "Clear all", r(x, q.bottom() - 48.0, w, 34.0), Style::Secondary, any) {
            self.checkpoint();
            self.items.clear();
            self.sel = None;
        }
    }

    fn list(&mut self, ui: &mut Ui, q: R) {
        self.hover_row = None;
        if self.items.is_empty() {
            let msg = if self.mode == Mode::Redact { "Nothing marked yet. Drag across the picture to cover an area." } else { "Pick Box, Arrow or Text, then draw on the picture." };
            ui.wrapped(msg, r(q.x + PAD, q.y + 4.0, q.w - PAD * 2.0, 60.0), 12.5, 400, MUTED);
            return;
        }
        // The selected redaction's row opens to hold the settings that belong to that mark alone.
        let redact = self.mode == Mode::Redact;
        let extra = move |sel: Option<usize>, i: usize| if redact && sel == Some(i) { ROW_FEATHER } else { 0.0 };
        let top_of = move |sel: Option<usize>, i: usize| (0..i).map(|n| ROW + extra(sel, n)).sum::<f32>();
        let content = top_of(self.sel, self.items.len());
        if self.sel != self.list_sel {
            self.list_sel = self.sel;
            if let Some(i) = self.sel.filter(|i| *i < self.items.len()) {
                let top = top_of(self.sel, i);
                ui.scroll_into_view("ed.list", q.h, top, top + ROW + extra(self.sel, i));
            }
        }
        let off = ui.scroll_area("ed.list", q, content);
        let real = ui.mouse;
        if !q.has(real) { ui.mouse = (-1000.0, -1000.0); }
        let mut remove = None;
        for i in 0..self.items.len() {
            let row = r(q.x + 10.0, q.y + top_of(self.sel, i) - off, q.w - 20.0, ROW - 4.0 + extra(self.sel, i));
            let hovered = row.has(ui.mouse);
            let x_q = r(row.right() - 32.0, row.y + 8.0, 26.0, 26.0);
            if hovered {
                self.hover_row = Some(i);
                if ui.icon_button(&format!("ed.x{i}"), glyph::CLOSE, x_q, 9.0, false) { remove = Some(i); }
            }
            // Only the top of an open row selects, or its slider would have to fight for the press.
            let (_, _, clicked) = ui.hit(id(&format!("ed.row{i}")), r(row.x, row.y, row.w, ROW - 4.0));
            if clicked { self.sel = Some(i); }
            let selected = self.sel == Some(i);
            let t = ui.anim(id(&format!("ed.hv{i}")), if hovered { 1.0 } else { 0.0 }, 22.0);
            ui.fill(row, 10.0, TEXT, if selected { 0.10 } else { 0.02 + 0.06 * t });
            if selected { ui.stroke(row, 10.0, ACCENT, 0.7, 1.0); }

            let it = &self.items[i];
            let (name, detail) = match (self.mode, it.shape.tool) {
                (Mode::Redact, t) => {
                    let what = if it.detected { "Detected area" } else if t == Tool::Ellipse { "Ellipse" } else { "Box" };
                    (if it.shape.fx.blur { format!("{what}, blurred") } else { what.to_string() }, size_label(&it.shape, self.full))
                }
                (_, Tool::Rect) => ("Box".to_string(), size_label(&it.shape, self.full)),
                (_, Tool::Ellipse) => ("Oval".to_string(), size_label(&it.shape, self.full)),
                (_, Tool::Arrow) => ("Arrow".to_string(), String::new()),
                (_, Tool::Text) => (it.shape.text.clone(), "Text".to_string()),
            };
            if self.mode == Mode::Redact {
                let sw = r(row.x + 10.0, row.y + 12.0, 18.0, 18.0);
                let (ink, alpha) = if it.shape.fx.blur { (TEXT, 0.35) } else { (it.shape.color, 1.0) };
                if it.shape.tool == Tool::Ellipse {
                    ui.oval(sw, ink, alpha);
                    ui.oval_stroke(sw, TEXT, 0.25, 1.0);
                } else {
                    ui.fill(sw, 3.0, ink, alpha);
                    ui.stroke(sw, 3.0, TEXT, 0.25, 1.0);
                }
            } else {
                ui.dot(row.x + 19.0, row.y + row.h / 2.0, 6.0, it.shape.color, 1.0);
            }
            let tw = row.w - 40.0 - 36.0;
            if detail.is_empty() {
                ui.text(&name, r(row.x + 38.0, row.y, tw, ROW - 4.0), 12.5, 500, TEXT, Align::Left);
            } else {
                ui.text(&name, r(row.x + 38.0, row.y + 4.0, tw, 18.0), 12.5, 500, TEXT, Align::Left);
                ui.text(&detail, r(row.x + 38.0, row.y + 21.0, tw, 16.0), 11.0, 400, MUTED, Align::Left);
            }
            if extra(self.sel, i) > 0.0 {
                let was = self.items[i].shape.fx.feather;
                let mut v = was;
                let (fx, fw, fy) = (row.x + 12.0, row.w - 24.0, row.y + ROW - 4.0);
                ui.text("Feather", r(fx, fy, fw - 50.0, 18.0), 12.0, 500, MUTED, Align::Left);
                ui.text(&format!("{}", v as i32), r(fx + fw - 50.0, fy, 50.0, 18.0), 12.0, 600, MUTED, Align::Right);
                ui.slider(&format!("ed.feather{i}"), r(fx - 9.0, fy + 16.0, fw + 18.0, 28.0), 0.0, 100.0, 0.0, &mut v);
                if v != was {
                    if !self.slider_open { self.checkpoint(); self.slider_open = true; }
                    self.items[i].shape.fx.feather = v;
                }
            }
        }
        ui.mouse = real;
        ui.unclip();
        if let Some(i) = remove { self.remove(i); }
    }

    // ---- video ----

    fn scrubber(&mut self, ui: &mut Ui, q: R) {
        let Some(sc) = self.scrub.as_mut() else { return };
        ui.fill(q, 0.0, SURFACE, 0.6);
        ui.fill(r(q.x, q.y, q.w, 1.0), 0.0, BORDER, 0.7);
        let fps = sc.source.fps;
        let t = (sc.pos / 1000.0) as f64 * sc.source.dur;
        ui.slider("ed.scrub", r(q.x + PAD - 9.0, q.y + 14.0, q.w - PAD * 2.0 - 132.0 + 18.0, 28.0), 0.0, 1000.0, 0.0, &mut sc.pos);
        let label = format!("{} / {}", timecode(t, fps), timecode(sc.source.dur, fps));
        ui.text(&label, r(q.right() - PAD - 124.0, q.y, 124.0, q.h), 12.0, 500, MUTED, Align::Right);

        if let Some(res) = sc.slot.lock().unwrap().take() {
            sc.loading = false;
            match res {
                Ok(b) => { sc.error = None; self.preview = shrink(b, self.mode); self.shown_for = None; ui.forget_image(id("ed.image")); }
                Err(e) => sc.error = Some(e),
            }
        }
        if !sc.loading && sc.pos != sc.requested {
            sc.requested = sc.pos;
            sc.loading = true;
            let (load, slot, at) = (sc.source.load.clone(), sc.slot.clone(), snap((sc.pos / 1000.0) as f64 * sc.source.dur, fps));
            std::thread::spawn(move || {
                crate::job::com_init();
                let res = load(at);
                *slot.lock().unwrap() = Some(res);
            });
        }
        if sc.loading { ui.want_anim = true; }
    }

    // ---- footer and keys ----

    fn footer(&mut self, ui: &mut Ui, w: f32, h: f32) {
        let fy = h - FOOT_H;
        ui.divider(fy, 0.0, w);
        let video = self.scrub.is_some();
        let err = self.scrub.as_ref().and_then(|s| s.error.as_ref()).map(|_| "Could not read that frame. Pick another point in the video.");
        let hint = err.unwrap_or(match self.mode {
            Mode::Crop if video => "Applies to the whole video. Drag the handles, or drag outside to start over.",
            Mode::Crop => "Drag the handles, or drag outside the box to start over.",
            Mode::Redact if video => "Applies to the whole video. Click a mark to select and restyle it, right-click to remove it.",
            Mode::Redact => "Click a mark to select and restyle it, right-click to remove it. Delete works too.",
            Mode::Annotate => match self.tool {
                0 => "Click to select, drag to move, right-click to remove.",
                3 => "Click where the text should start.",
                _ => "Drag on the picture to draw.",
            },
        });
        ui.text(hint, r(PAD, fy, w - 300.0, FOOT_H), 12.5, 400, if err.is_some() { crate::gfx::DANGER } else { MUTED }, Align::Left);
        let by = fy + (FOOT_H - 38.0) / 2.0;
        let primary = r(w - PAD - 120.0, by, 120.0, 38.0);
        let ready = self.ready();
        if ui.button("ed.cancel", "Cancel", r(primary.x - 96.0, by, 88.0, 38.0), Style::Ghost, true) { self.result = Some(false); }
        if ui.button("ed.ok", &self.ok, primary, Style::Primary, ready) { self.result = Some(true); }
    }

    fn keys(&mut self, ui: &mut Ui) {
        if ui.key(VK_ESCAPE) {
            if self.mode != Mode::Crop && self.sel.is_some() { self.sel = None; } else { self.result = Some(false); }
        }
        if ui.key(VK_RETURN) && self.ready() { self.result = Some(true); }
        if ui.key(VK_DELETE) && self.mode != Mode::Crop {
            if let Some(i) = self.sel.filter(|i| *i < self.items.len()) { self.remove(i); }
        }
        match ui.ctrl_key(VK_Z) { Some(true) => self.step_redo(), Some(false) => self.step_undo(), None => {} }
        if ui.ctrl_key(VK_Y).is_some() { self.step_redo(); }
    }
}

impl Page for Editor {
    fn size(&self) -> (f32, f32) { self.start_size }
    fn resizable(&self) -> bool { true }
    fn done(&self) -> bool { self.result.is_some() }
    fn on_close(&mut self) { self.result = Some(false); }
    fn frame(&mut self, ui: &mut Ui) {
        let (w, h) = (ui.w, ui.h);
        // Before the title bar, or pressing it would also start moving the window.
        ui.maximize_button(w - 80.0);
        if ui.titlebar(&self.title, &self.sub) { self.result = Some(false); }
        ui.divider(TITLE_H, 0.0, w);
        let body = h - TITLE_H - FOOT_H - 1.0;
        let scrub_h = if self.scrub.is_some() { SCRUB_H } else { 0.0 };
        self.sidebar(ui, r(w - SIDE, TITLE_H + 1.0, SIDE - 1.0, body));
        self.scrubber(ui, r(1.0, h - FOOT_H - scrub_h, w - SIDE - 1.0, scrub_h));
        self.stage(ui, r(1.0, TITLE_H + 1.0, w - SIDE - 1.0, body - scrub_h));
        self.footer(ui, w, h);
        self.keys(ui);
    }
    fn after(&mut self) -> bool {
        if std::mem::take(&mut self.pending_color) {
            let current = self.sel.and_then(|i| self.items.get(i)).map(|i| i.shape.color).unwrap_or(self.pen);
            if let Some(v) = form::show("Custom colour", &[form::Field::Color("Colour", format!("#{current:06X}"))], "Use") {
                let c = crate::image::parse_color(&v[0]);
                self.recolor(((c[2] as u32) << 16) | ((c[1] as u32) << 8) | c[0] as u32);
            }
            return true;
        }
        let Some(p) = self.pending_text.take() else { return false };
        if let Some(v) = form::show("Add text", &[form::text("Text", "")], "Add") {
            let t = v[0].trim().to_string();
            if !t.is_empty() {
                self.checkpoint();
                self.items.push(Item { shape: Shape { tool: Tool::Text, a: p, b: p, color: self.pen, text: t, fx: Fx { width: self.fx.width, ..Fx::default() } }, detected: false });
                self.sel = Some(self.items.len() - 1);
            }
        }
        true
    }
}

fn run(title: &str, bmp: Bmp, mode: Mode, initial: Vec<Shape>, save_label: &str, scrub: Option<FrameSource>) -> Option<Vec<Shape>> {
    let e = ui::run_modal(Editor::new(title, bmp, mode, initial, save_label, scrub), title, None)?;
    (e.result == Some(true)).then(|| e.output())
}

/// Opens the editor and blocks, pumping messages, until saved or cancelled.
pub fn edit(title: &str, bmp: Bmp, mode: Mode, initial: Vec<Shape>, save_label: &str) -> Option<Vec<Shape>> {
    run(title, bmp, mode, initial, save_label, None)
}

/// The same editor with a scrubber, for choosing a frame to work on. The result applies to the whole video.
pub fn edit_video(title: &str, first: Bmp, mode: Mode, save_label: &str, source: FrameSource) -> Option<Vec<Shape>> {
    run(title, first, mode, vec![], save_label, Some(source))
}

/// The editor as a PNG with sample shapes, for design review.
pub fn render(mode: Mode, bmp: Bmp, shapes: Vec<Shape>, sel: usize, video: Option<f64>, out: &std::path::Path) -> std::result::Result<(), String> {
    let title = match (mode, video.is_some()) {
        (Mode::Crop, _) => "Crop",
        (Mode::Redact, false) => "Redact",
        (Mode::Redact, true) => "Redact video",
        (Mode::Annotate, _) => "Annotate",
    };
    let source = video.map(|dur| FrameSource { dur, fps: 30.0, load: Arc::new(|_| Err(String::new())) });
    let mut e = Editor::new(title, bmp, mode, shapes, title, source);
    e.sub = "holiday.jpg".into();
    e.start_size = (1180.0, 720.0);
    e.sel = Some(sel);
    if mode == Mode::Redact && e.items.len() > 1 { e.items[0].detected = false; }
    ui::render_png(&mut e, 1.0, None, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shapes_round_trip() {
        let v = vec![Shape::rect(0.1, 0.2, 0.3, 0.4), Shape { tool: Tool::Text, a: (0.5, 0.5), b: (0.5, 0.5), color: 0xFF3B30, text: "hi\tthere".into(), fx: Fx { blur: true, strength: 12.0, feather: 7.0, width: 2.5 } }];
        let back = decode_all(&encode_all(&v));
        assert_eq!(back[0], v[0]);
        assert_eq!(back[1].text, "hi there");
        assert_eq!(back[1].fx, v[1].fx, "per-shape look survives");
        assert_eq!(Shape::decode("rect\t0.1\t0.2\t0.3\t0.4\t000000\t").unwrap().fx, Fx::default(), "older lines still read");
    }
    #[test]
    fn marks_round_outward_and_stay_on_the_picture() {
        let m = marks_for(&[Shape::rect(0.101, 0.101, 0.2995, 0.2995)], 1000, 1000, false)[0];
        assert_eq!((m.x, m.y, m.w, m.h), (101, 101, 199, 199));
        let m = marks_for(&[Shape::rect(0.9995, 0.9995, 1.0, 1.0)], 1000, 1000, true)[0];
        assert!(m.x + m.w <= 1000 && m.y + m.h <= 1000 && m.w >= 1);
    }
    #[test]
    fn bounds_normalise_and_clamp() {
        let (x, y, w, h) = Shape::rect(0.8, 1.5, 0.2, 0.5).bounds();
        assert!((x - 0.2).abs() < 1e-9 && (y - 0.5).abs() < 1e-9 && (w - 0.6).abs() < 1e-9 && (h - 0.5).abs() < 1e-9);
    }
    #[test]
    fn annotation_changes_pixels() {
        let mut b = Bmp::new(200, 200, [255, 255, 255, 255]);
        apply_annotations(&mut b, &[Shape { tool: Tool::Rect, a: (0.1, 0.1), b: (0.9, 0.9), color: 0xFF0000, text: String::new(), fx: Fx::default() }]).unwrap();
        assert!(b.px.chunks_exact(4).any(|p| p[2] == 255 && p[0] == 0 && p[3] == 255));
    }
    fn ratio_of(a: P, b: P, iw: f64, ih: f64) -> f64 { ((b.0 - a.0) * iw / ((b.1 - a.1) * ih)).abs() }
    #[test]
    fn locked_corner_keeps_the_ratio() {
        let b = constrain((0.1, 0.1), (0.5, 0.2), 16.0 / 9.0, 1600.0, 900.0);
        assert!((ratio_of((0.1, 0.1), b, 1600.0, 900.0) - 16.0 / 9.0).abs() < 1e-6);
    }
    #[test]
    fn locked_corner_stays_on_the_image() {
        for m in [(1.0, 1.0), (0.0, 0.0), (1.0, 0.0), (0.0, 1.0)] {
            let b = constrain((0.7, 0.3), m, 1.0, 1000.0, 500.0);
            assert!((0.0..=1.0).contains(&b.0) && (0.0..=1.0).contains(&b.1), "{b:?}");
            assert!((ratio_of((0.7, 0.3), b, 1000.0, 500.0) - 1.0).abs() < 1e-6);
        }
    }
    #[test]
    fn aspect_fits_inside_and_centres() {
        let s = fit_aspect((0.0, 0.0, 1.0, 1.0), 1.0, 1000.0, 500.0);
        let (x, y, w, h) = s.bounds();
        assert!((w * 1000.0 - h * 500.0).abs() < 1e-6);
        assert!((h - 1.0).abs() < 1e-9 && (x - 0.25).abs() < 1e-9 && y.abs() < 1e-9);
    }
    #[test]
    fn dragging_a_corner_keeps_the_opposite_one() {
        let s = apply_handle(&Shape::rect(0.2, 0.2, 0.6, 0.6), 2, (0.9, 0.8), None);
        assert_eq!((s.a, s.b), ((0.2, 0.2), (0.9, 0.8)));
        let s = apply_handle(&Shape::rect(0.2, 0.2, 0.6, 0.6), 0, (0.9, 0.9), None);
        assert_eq!(s.bounds().0, 0.6);
    }
    #[test]
    fn dragging_an_edge_moves_one_side() {
        let s = apply_handle(&Shape::rect(0.2, 0.2, 0.6, 0.6), 5, (0.8, 0.1), None);
        assert_eq!((s.a, s.b), ((0.2, 0.2), (0.8, 0.6)));
    }
    #[test]
    fn moving_stops_at_the_edge() {
        let s = moved(&Shape::rect(0.2, 0.2, 0.6, 0.6), (0.9, -0.9));
        assert!((s.b.0 - 1.0).abs() < 1e-9 && s.a.1.abs() < 1e-9);
        assert!((s.bounds().2 - 0.4).abs() < 1e-9);
    }
    #[test]
    fn arrows_are_hit_near_the_line_only() {
        let s = Shape { tool: Tool::Arrow, a: (0.1, 0.5), b: (0.9, 0.5), color: 0, text: String::new(), fx: Fx::default() };
        assert!(on_shape(&s, (0.5, 0.505), 1000.0, 1000.0, 20.0));
        assert!(!on_shape(&s, (0.5, 0.6), 1000.0, 1000.0, 20.0));
    }
    #[test]
    fn topmost_shape_wins() {
        let item = |s| Item { shape: s, detected: false };
        let v = vec![item(Shape::rect(0.0, 0.0, 1.0, 1.0)), item(Shape::rect(0.4, 0.4, 0.6, 0.6))];
        assert_eq!(shape_at(&v, (0.5, 0.5), 100.0, 100.0, 10.0), Some(1));
        assert_eq!(shape_at(&v, (0.1, 0.1), 100.0, 100.0, 10.0), Some(0));
        assert_eq!(shape_at(&v[1..], (0.1, 0.1), 100.0, 100.0, 10.0), None);
    }
}
