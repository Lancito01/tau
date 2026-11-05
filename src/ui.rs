//! Tau's window kit. Every window is a layered Direct2D surface with an immediate-mode page drawn into it,
//! so rounded corners, soft shadows and motion look the same on Windows 10 and 11.
use crate::gfx::*;
use crate::image::Bmp;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const TITLE_H: f32 = 56.0;
pub const FOOT_H: f32 = 68.0;
pub const PAD: f32 = 20.0;
pub const SHADOW: f32 = 28.0;
const CARD_R: f32 = 16.0;
const RESIZE_BAND: f32 = 7.0;
const WM_APP_REFRESH: u32 = WM_APP + 40;
const WM_APP_DESTROY: u32 = WM_APP + 41;

pub mod glyph {
    pub const CLOSE: u16 = 0xE711;
    pub const CHECK: u16 = 0xE73E;
    pub const UNDO: u16 = 0xE7A7;
    pub const REDO: u16 = 0xE7A6;
    pub const PIN: u16 = 0xE718;
    pub const PINNED: u16 = 0xE840;
    pub const SETTINGS: u16 = 0xE713;
    pub const SYNC: u16 = 0xE895;
    pub const POWER: u16 = 0xE7E8;
    pub const MAXIMIZE: u16 = 0xE922;
    pub const RESTORE: u16 = 0xE923;
    pub const CHEVRON: u16 = 0xE70D;
    pub const FILE: u16 = 0xE8A5;
    pub const FOLDER: u16 = 0xE8B7;
    pub const PHOTO: u16 = 0xEB9F;
    pub const VIDEO: u16 = 0xE714;
    pub const MUSIC: u16 = 0xE8D6;
    pub const WARNING: u16 = 0xE7BA;
    pub const ADD: u16 = 0xE710;
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct R { pub x: f32, pub y: f32, pub w: f32, pub h: f32 }
pub fn r(x: f32, y: f32, w: f32, h: f32) -> R { R { x, y, w, h } }
impl R {
    pub fn has(&self, p: (f32, f32)) -> bool { p.0 >= self.x && p.1 >= self.y && p.0 < self.x + self.w && p.1 < self.y + self.h }
    pub fn right(&self) -> f32 { self.x + self.w }
    pub fn bottom(&self) -> f32 { self.y + self.h }
    pub fn inset(&self, d: f32) -> R { r(self.x + d, self.y + d, self.w - d * 2.0, self.h - d * 2.0) }
    fn d2(&self) -> D2D_RECT_F { D2D_RECT_F { left: self.x, top: self.y, right: self.right(), bottom: self.bottom() } }
}

pub fn id(s: &str) -> u64 { s.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3)) }

#[derive(Clone, Copy, PartialEq)]
pub enum Style { Primary, Secondary, Ghost, Danger }

#[derive(Clone, Copy, PartialEq)]
pub enum Align { Left, Center, Right }

#[derive(Clone, Copy)]
pub struct Key { pub vk: VIRTUAL_KEY, pub ctrl: bool, pub shift: bool }

/// A page draws itself and reacts to input in one call. It runs on every event.
pub trait Page {
    /// Card size in DIPs. Resizable pages give their starting size here.
    fn size(&self) -> (f32, f32);
    fn frame(&mut self, ui: &mut Ui);
    /// Runs after the frame, outside drawing, for anything that opens another window. True asks for a repaint.
    fn after(&mut self) -> bool { false }
    fn done(&self) -> bool { false }
    fn on_close(&mut self) {}
    fn on_deactivate(&mut self) {}
    fn resizable(&self) -> bool { false }
    /// False for pages that draw their own cards.
    fn card(&self) -> bool { true }
    /// Screen position in pixels, given the window's outer size in pixels.
    fn place(&mut self, _w: i32, _h: i32) -> Option<POINT> { None }
    fn message(&mut self, _hwnd: HWND, _msg: u32, _wp: WPARAM, _lp: LPARAM) -> Option<LRESULT> { None }
}

struct PopupReq { id: u64, anchor: R, items: Vec<MenuItem>, sel: Option<usize> }

pub struct Ui {
    pub gfx: Gfx,
    rt: Option<ID2D1RenderTarget>,
    brush: Option<ID2D1SolidColorBrush>,
    round: Option<ID2D1StrokeStyle>,
    formats: HashMap<(u32, i32, u8, bool), IDWriteTextFormat>,
    anims: HashMap<u64, f32>,
    last: std::time::Instant,
    pub dt: f32,
    pub w: f32,
    pub h: f32,
    pub mouse: (f32, f32),
    pub down: bool,
    pub pressed: bool,
    pub released: bool,
    pub rpressed: bool,
    pub double: bool,
    pub scroll: f32,
    keys: Vec<Key>,
    chars: Vec<u16>,
    pub active: u64,
    pub focus: u64,
    order: Vec<u64>,
    tab: i32,
    carets: HashMap<u64, (usize, usize, f32)>,
    bitmaps: HashMap<u64, ID2D1Bitmap>,
    popup: Option<PopupReq>,
    popup_result: Option<(u64, usize)>,
    pub want_anim: bool,
    want_drag: bool,
    want_max: bool,
    pub maximized: bool,
    pub cursor: PCWSTR,
    pub tooltip: Option<(String, (f32, f32))>,
}

impl Ui {
    fn new() -> Result<Ui> {
        let gfx = Gfx::new()?;
        let round = round_stroke(&gfx).ok();
        Ok(Ui {
            gfx, rt: None, brush: None, round, formats: HashMap::new(), anims: HashMap::new(), last: std::time::Instant::now(), dt: 0.016,
            w: 0.0, h: 0.0, mouse: (-1000.0, -1000.0), down: false, pressed: false, released: false, rpressed: false, double: false, scroll: 0.0,
            keys: vec![], chars: vec![], active: 0, focus: 0, order: vec![], tab: 0, carets: HashMap::new(), bitmaps: HashMap::new(),
            popup: None, popup_result: None, want_anim: false, want_drag: false, want_max: false, maximized: false, cursor: IDC_ARROW, tooltip: None,
        })
    }

    pub fn rt(&self) -> &ID2D1RenderTarget { self.rt.as_ref().unwrap() }
    fn b(&self, c: u32, a: f32) -> &ID2D1SolidColorBrush {
        let b = self.brush.as_ref().unwrap();
        unsafe { b.SetColor(&rgba(c, a)); }
        b
    }

    // ---- drawing ----

    pub fn fill(&self, q: R, rad: f32, c: u32, a: f32) {
        unsafe {
            if rad <= 0.0 { self.rt().FillRectangle(&q.d2(), self.b(c, a)); }
            else { self.rt().FillRoundedRectangle(&rrect(q.x, q.y, q.w, q.h, rad), self.b(c, a)); }
        }
    }
    pub fn stroke(&self, q: R, rad: f32, c: u32, a: f32, w: f32) {
        unsafe {
            let i = w / 2.0;
            self.rt().DrawRoundedRectangle(&rrect(q.x + i, q.y + i, q.w - w, q.h - w, rad), self.b(c, a), w, None);
        }
    }
    pub fn line(&self, a: (f32, f32), b: (f32, f32), c: u32, alpha: f32, w: f32) {
        unsafe { self.rt().DrawLine(pt(a.0, a.1), pt(b.0, b.1), self.b(c, alpha), w, self.round.as_ref()); }
    }
    pub fn dot(&self, x: f32, y: f32, rad: f32, c: u32, a: f32) {
        unsafe { self.rt().FillEllipse(&ellipse(x, y, rad), self.b(c, a)); }
    }
    pub fn ring(&self, x: f32, y: f32, rad: f32, c: u32, a: f32, w: f32) {
        unsafe { self.rt().DrawEllipse(&ellipse(x, y, rad), self.b(c, a), w, None); }
    }
    pub fn arc(&self, x: f32, y: f32, rad: f32, from: f32, sweep: f32, c: u32, a: f32, w: f32) {
        if let Ok(g) = crate::gfx::arc(&self.gfx, x, y, rad, from, sweep) {
            unsafe { self.rt().DrawGeometry(&g, self.b(c, a), w, self.round.as_ref()); }
        }
    }
    pub fn clip(&self, q: R) { unsafe { self.rt().PushAxisAlignedClip(&q.d2(), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE); } }
    pub fn unclip(&self) { unsafe { self.rt().PopAxisAlignedClip(); } }
    /// Rounds the corners of a picture drawn at `q` by painting the background over them.
    pub fn round_corners(&self, q: R, rad: f32, bg: u32) {
        self.stroke(q.inset(-4.0), rad + 4.0, bg, 1.0, 4.0);
    }
    /// A floating card with its soft shadow.
    pub fn card(&self, q: R, rad: f32) {
        for i in 0..12 {
            let g = 12.0 - i as f32;
            self.fill(r(q.x - g, q.y - g + 6.0, q.w + g * 2.0, q.h + g * 2.0), rad + g, 0, 0.018 + i as f32 * 0.004);
        }
        self.fill(q, rad, BG, 1.0);
        self.stroke(q, rad, 0xFFFFFF, 0.09, 1.0);
    }

    fn format(&mut self, font: &str, size: f32, weight: i32, al: Align, wrap: bool) -> Option<IDWriteTextFormat> {
        let key = ((size * 10.0) as u32 + if font == ICONS { 100000 } else { 0 }, weight, al as u8, wrap);
        if let Some(f) = self.formats.get(&key) { return Some(f.clone()); }
        let align = match al { Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING, Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER, Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING };
        let f = self.gfx.format(font, size, DWRITE_FONT_WEIGHT(weight), align).ok()?;
        unsafe {
            if wrap {
                let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
                let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
            } else if let Ok(sign) = self.gfx.dw.CreateEllipsisTrimmingSign(&f) {
                let _ = f.SetTrimming(&DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 }, &sign);
            }
        }
        self.formats.insert(key, f.clone());
        Some(f)
    }

    /// One line, vertically centred in `q`, ellipsised if it does not fit.
    pub fn text(&mut self, s: &str, q: R, size: f32, weight: i32, c: u32, al: Align) {
        let Some(f) = self.format(FONT, size, weight, al, false) else { return };
        let t = wide(s);
        unsafe { self.rt().DrawText(&t, &f, &q.d2(), self.b(c, 1.0), D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL); }
    }
    pub fn wrapped(&mut self, s: &str, q: R, size: f32, weight: i32, c: u32) {
        let Some(f) = self.format(FONT, size, weight, Align::Left, true) else { return };
        let t = wide(s);
        unsafe { self.rt().DrawText(&t, &f, &q.d2(), self.b(c, 1.0), D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL); }
    }
    pub fn icon(&mut self, g: u16, q: R, size: f32, c: u32, a: f32) {
        let Some(f) = self.format(ICONS, size, 400, Align::Center, false) else { return };
        unsafe { self.rt().DrawText(&[g], &f, &q.d2(), self.b(c, a), D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL); }
    }
    pub fn measure(&mut self, s: &str, size: f32, weight: i32) -> f32 {
        let Some(f) = self.format(FONT, size, weight, Align::Left, true) else { return 0.0 };
        let t = wide(s);
        unsafe {
            let Ok(l) = self.gfx.dw.CreateTextLayout(&t, &f, 4000.0, 100.0) else { return 0.0 };
            let mut m = DWRITE_TEXT_METRICS::default();
            let _ = l.GetMetrics(&mut m);
            m.widthIncludingTrailingWhitespace
        }
    }
    pub fn wrapped_height(&mut self, s: &str, size: f32, weight: i32, width: f32) -> f32 {
        let Some(f) = self.format(FONT, size, weight, Align::Left, true) else { return 0.0 };
        let t = wide(s);
        unsafe {
            let Ok(l) = self.gfx.dw.CreateTextLayout(&t, &f, width, 2000.0) else { return 0.0 };
            let mut m = DWRITE_TEXT_METRICS::default();
            let _ = l.GetMetrics(&mut m);
            m.height
        }
    }

    /// Draws a picture, keeping one GPU copy per `key`.
    pub fn image(&mut self, key: u64, bmp: &Bmp, dest: R) {
        if !self.bitmaps.contains_key(&key) {
            let mut px = bmp.px.clone();
            for p in px.chunks_exact_mut(4) {
                if p[3] != 255 { let a = p[3] as u32; for v in &mut p[..3] { *v = (*v as u32 * a / 255) as u8; } }
            }
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0, dpiY: 96.0,
            };
            let made = unsafe { self.rt().CreateBitmap(D2D_SIZE_U { width: bmp.w, height: bmp.h }, Some(px.as_ptr() as *const _), bmp.w * 4, &props) };
            if let Ok(b) = made { self.bitmaps.insert(key, b); }
        }
        if let Some(b) = self.bitmaps.get(&key) {
            unsafe { self.rt().DrawBitmap(b, Some(&dest.d2()), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None); }
        }
    }
    pub fn forget_image(&mut self, key: u64) { self.bitmaps.remove(&key); }

    // ---- input ----

    pub fn key(&mut self, vk: VIRTUAL_KEY) -> bool {
        if let Some(i) = self.keys.iter().position(|k| k.vk == vk && !k.ctrl) { self.keys.remove(i); true } else { false }
    }
    /// Ctrl plus a key. The answer says whether Shift was held too.
    pub fn ctrl_key(&mut self, vk: VIRTUAL_KEY) -> Option<bool> {
        let i = self.keys.iter().position(|k| k.vk == vk && k.ctrl)?;
        Some(self.keys.remove(i).shift)
    }

    /// Smoothly moves a stored value towards `target`.
    pub fn anim(&mut self, key: u64, target: f32, speed: f32) -> f32 {
        let dt = self.dt;
        let v = self.anims.entry(key).or_insert(target);
        *v += (target - *v) * (1.0 - (-speed * dt).exp());
        if (*v - target).abs() < 0.004 { *v = target; } else { self.want_anim = true; }
        *v
    }

    /// Standard hit logic: (hovered, held, clicked).
    pub fn hit(&mut self, key: u64, q: R) -> (bool, bool, bool) {
        let inside = q.has(self.mouse);
        let hover = inside && (self.active == 0 || self.active == key);
        if hover && self.pressed { self.active = key; self.focus = 0; }
        let held = self.active == key && self.down;
        let clicked = self.released && self.active == key && inside;
        (hover, held, clicked)
    }
    fn focusable(&mut self, key: u64) -> bool { self.order.push(key); self.focus == key }
    fn focus_ring(&self, q: R, rad: f32) { self.stroke(q.inset(-2.0), rad + 2.0, ACCENT, 0.8, 1.5); }

    // ---- widgets ----

    /// Logo, title, optional subtitle, close button. Dragging the empty bar moves the window. True when closed.
    pub fn titlebar(&mut self, title: &str, sub: &str) -> bool {
        let w = self.w;
        let _ = draw_logo(&self.gfx, self.rt(), PAD, 17.0, 22.0);
        if sub.is_empty() {
            self.text(title, r(PAD + 32.0, 0.0, w - PAD * 2.0 - 120.0, TITLE_H), 14.0, 600, TEXT, Align::Left);
        } else {
            self.text(title, r(PAD + 32.0, 9.0, w - PAD * 2.0 - 120.0, 20.0), 14.0, 600, TEXT, Align::Left);
            self.text(sub, r(PAD + 32.0, 28.0, w - PAD * 2.0 - 120.0, 18.0), 11.5, 400, MUTED, Align::Left);
        }
        let closed = self.icon_button("ui.close", glyph::CLOSE, r(w - 44.0, 12.0, 32.0, 32.0), 10.0, false);
        if self.pressed && self.active == 0 && r(0.0, 0.0, w, TITLE_H).has(self.mouse) { self.want_drag = true; }
        closed
    }
    pub fn maximize_button(&mut self, x: f32) {
        let g = if self.maximized { glyph::RESTORE } else { glyph::MAXIMIZE };
        if self.icon_button("ui.max", g, r(x, 12.0, 32.0, 32.0), 10.0, false) { self.want_max = true; }
    }
    pub fn divider(&self, y: f32, x0: f32, x1: f32) { self.fill(r(x0, y, x1 - x0, 1.0), 0.0, BORDER, 0.7); }

    pub fn button(&mut self, key: &str, label: &str, q: R, style: Style, enabled: bool) -> bool { self.icon_label_button(key, 0, label, q, style, enabled) }
    pub fn icon_label_button(&mut self, key: &str, g: u16, label: &str, q: R, style: Style, enabled: bool) -> bool {
        let k = id(key);
        let focused = self.focusable(k) && enabled;
        let (hover, held, clicked) = if enabled { self.hit(k, q) } else { (false, false, false) };
        let hv = self.anim(k, if held { 1.0 } else if hover { 0.6 } else { 0.0 }, 22.0);
        let (bg, ba, fg) = match style {
            Style::Primary => (ACCENT, if enabled { 1.0 } else { 0.22 }, if enabled { ON_ACCENT } else { MUTED }),
            Style::Secondary => (RAISED, 1.0, if enabled { TEXT } else { FAINT }),
            Style::Ghost => (TEXT, 0.09 * hv, if enabled { MUTED } else { FAINT }),
            Style::Danger => (DANGER, 0.14 + 0.14 * hv, DANGER),
        };
        self.fill(q, 9.0, bg, ba);
        if style == Style::Primary || style == Style::Secondary { self.fill(q, 9.0, 0xFFFFFF, 0.14 * hv); }
        if style == Style::Secondary { self.stroke(q, 9.0, TEXT, 0.06, 1.0); }
        let w = if style == Style::Primary { 600 } else { 500 };
        if g != 0 {
            let tw = self.measure(label, 13.0, w);
            let gap = if label.is_empty() { 0.0 } else { 7.0 };
            let x = q.x + (q.w - tw - 14.0 - gap) / 2.0;
            self.icon(g, r(x, q.y, 14.0, q.h), 12.0, fg, 1.0);
            self.text(label, r(x + 14.0 + gap, q.y, tw + 2.0, q.h), 13.0, w, fg, Align::Left);
        } else {
            self.text(label, q, 13.0, w, fg, Align::Center);
        }
        if focused { self.focus_ring(q, 9.0); }
        clicked || (focused && self.key(VK_SPACE))
    }
    pub fn icon_button(&mut self, key: &str, g: u16, q: R, size: f32, on: bool) -> bool {
        let k = id(key);
        let (hover, held, clicked) = self.hit(k, q);
        let hv = self.anim(k, if held { 1.0 } else if hover { 0.7 } else { 0.0 }, 22.0);
        if on { self.fill(q, 8.0, ACCENT, 1.0); } else { self.fill(q, 8.0, TEXT, 0.10 * hv); }
        self.icon(g, q, size, if on { ON_ACCENT } else if hover { TEXT } else { MUTED }, 1.0);
        clicked
    }
    /// An icon button that shows when it has nothing to do.
    pub fn icon_button_if(&mut self, key: &str, g: u16, q: R, size: f32, enabled: bool) -> bool {
        if enabled { return self.icon_button(key, g, q, size, false); }
        self.icon(g, q, size, FAINT, 0.6);
        false
    }

    pub fn toggle(&mut self, key: &str, q: R, v: &mut bool) {
        let k = id(key);
        let focused = self.focusable(k);
        let (_, _, clicked) = self.hit(k, q);
        if clicked || (focused && self.key(VK_SPACE)) { *v = !*v; }
        let t = self.anim(k, if *v { 1.0 } else { 0.0 }, 20.0);
        let sw = r(q.right() - 40.0, q.y + (q.h - 22.0) / 2.0, 40.0, 22.0);
        self.fill(sw, 11.0, RAISED, 1.0);
        self.fill(sw, 11.0, ACCENT, t);
        self.dot(sw.x + 11.0 + 18.0 * t, sw.y + 11.0, 8.0, mix(TEXT, ON_ACCENT, t), 1.0);
        if focused { self.focus_ring(sw, 11.0); }
    }

    pub fn segmented(&mut self, key: &str, q: R, opts: &[String], sel: &mut usize) {
        let n = opts.len().max(1);
        let k = id(key);
        let focused = self.focusable(k);
        if focused {
            if self.key(VK_LEFT) && *sel > 0 { *sel -= 1; }
            if self.key(VK_RIGHT) && *sel + 1 < n { *sel += 1; }
        }
        self.fill(q, 10.0, SURFACE, 1.0);
        self.stroke(q, 10.0, TEXT, 0.06, 1.0);
        let cw = (q.w - 6.0) / n as f32;
        for i in 0..opts.len() {
            let c = r(q.x + 3.0 + cw * i as f32, q.y + 3.0, cw, q.h - 6.0);
            let (_, _, clicked) = self.hit(id(&format!("{key}/{i}")), c);
            if clicked { *sel = i; self.focus = k; }
        }
        let x = self.anim(k, *sel as f32, 24.0);
        let thumb = r(q.x + 3.0 + cw * x, q.y + 3.0, cw, q.h - 6.0);
        self.fill(thumb, 8.0, RAISED, 1.0);
        self.stroke(thumb, 8.0, TEXT, 0.08, 1.0);
        for (i, o) in opts.iter().enumerate() {
            let c = r(q.x + 3.0 + cw * i as f32, q.y + 3.0, cw, q.h - 6.0);
            let on = i == *sel;
            self.text(o, c.inset(2.0), 12.5, if on { 600 } else { 400 }, if on { TEXT } else { MUTED }, Align::Center);
        }
        if focused { self.focus_ring(q, 10.0); }
    }

    pub fn select(&mut self, key: &str, q: R, opts: &[String], sel: &mut usize, enabled: bool) {
        let k = id(key);
        let focused = self.focusable(k) && enabled;
        if let Some((pid, i)) = self.popup_result { if pid == k { *sel = i; self.popup_result = None; } }
        let (hover, _, clicked) = if enabled { self.hit(k, q) } else { (false, false, false) };
        let open = clicked || (focused && (self.key(VK_SPACE) || self.key(VK_RETURN)));
        let hv = self.anim(k, if hover { 1.0 } else { 0.0 }, 22.0);
        self.fill(q, 9.0, SURFACE, 1.0);
        self.fill(q, 9.0, TEXT, 0.05 * hv);
        self.stroke(q, 9.0, TEXT, 0.09, 1.0);
        let label = opts.get(*sel).cloned().unwrap_or_default();
        self.text(&label, r(q.x + 12.0, q.y, q.w - 38.0, q.h), 13.0, 400, if enabled { TEXT } else { FAINT }, Align::Left);
        self.icon(glyph::CHEVRON, r(q.right() - 30.0, q.y, 20.0, q.h), 9.0, MUTED, if enabled { 1.0 } else { 0.5 });
        if focused { self.focus_ring(q, 9.0); }
        if open {
            let items = opts.iter().map(|o| MenuItem::new(o, 0)).collect();
            self.popup = Some(PopupReq { id: k, anchor: q, items, sel: Some(*sel) });
        }
    }

    /// `origin` is where the filled part starts: 0 for bipolar values, `min` otherwise. Double-click returns to it.
    pub fn slider(&mut self, key: &str, q: R, min: f32, max: f32, origin: f32, v: &mut f32) {
        let k = id(key);
        let focused = self.focusable(k);
        let (hover, held, _) = self.hit(k, q);
        if held {
            let f = ((self.mouse.0 - q.x - 9.0) / (q.w - 18.0)).clamp(0.0, 1.0);
            *v = (min + (max - min) * f).round();
            self.focus = k;
        }
        if self.double && hover { *v = origin; }
        if focused {
            if self.key(VK_LEFT) { *v = (*v - 1.0).max(min); }
            if self.key(VK_RIGHT) { *v = (*v + 1.0).min(max); }
        }
        let cy = q.y + q.h / 2.0;
        let xof = |t: f32| q.x + 9.0 + (t - min) / (max - min) * (q.w - 18.0);
        self.fill(r(q.x + 9.0, cy - 2.0, q.w - 18.0, 4.0), 2.0, RAISED, 1.0);
        let (a, b) = (xof(origin.min(*v)), xof(origin.max(*v)));
        self.fill(r(a, cy - 2.0, b - a, 4.0), 2.0, ACCENT, 1.0);
        let g = self.anim(k, if held { 1.0 } else if hover { 0.5 } else { 0.0 }, 22.0);
        self.dot(xof(*v), cy, 7.0 + 8.0 * g, ACCENT, 0.16 * g);
        self.dot(xof(*v), cy, 7.0, TEXT, 1.0);
        if focused { self.ring(xof(*v), cy, 10.0, ACCENT, 0.8, 1.5); }
    }

    pub fn text_input(&mut self, key: &str, q: R, v: &mut String, placeholder: &str) {
        let k = id(key);
        self.focusable(k);
        let (hover, held, _) = self.hit(k, q);
        if hover { self.cursor = IDC_IBEAM; }
        let mut text: Vec<u16> = v.encode_utf16().collect();
        let (mut caret, mut anchor, mut off) = self.carets.get(&k).copied().unwrap_or((text.len(), text.len(), 0.0));
        caret = caret.min(text.len());
        anchor = anchor.min(text.len());
        let inner = r(q.x + 12.0, q.y, q.w - 24.0, q.h);
        let Some(fmt) = self.format(FONT, 13.0, 400, Align::Left, false) else { return };
        let dw = self.gfx.dw.clone();
        let layout_of = |t: &[u16]| unsafe { dw.CreateTextLayout(t, &fmt, 8000.0, q.h).ok() };
        let hit_x = |l: &Option<IDWriteTextLayout>, x: f32| -> usize {
            let Some(l) = l else { return 0 };
            let (mut trail, mut inside) = (BOOL(0), BOOL(0));
            let mut m = DWRITE_HIT_TEST_METRICS::default();
            unsafe { let _ = l.HitTestPoint(x, 0.0, &mut trail, &mut inside, &mut m); }
            (m.textPosition as i32 + trail.0) as usize
        };
        let before = layout_of(&text);
        if self.pressed && hover { self.focus = k; caret = hit_x(&before, self.mouse.0 - inner.x + off); anchor = caret; }
        else if held { caret = hit_x(&before, self.mouse.0 - inner.x + off); }
        let focused = self.focus == k;
        if focused {
            let keys = std::mem::take(&mut self.keys);
            let mut rest = vec![];
            for e in keys {
                let (lo, hi) = (caret.min(anchor), caret.max(anchor));
                match e.vk {
                    VK_LEFT => { caret = if e.ctrl { 0 } else { caret.saturating_sub(1) }; if !e.shift { anchor = caret; } }
                    VK_RIGHT => { caret = if e.ctrl { text.len() } else { (caret + 1).min(text.len()) }; if !e.shift { anchor = caret; } }
                    VK_HOME => { caret = 0; if !e.shift { anchor = 0; } }
                    VK_END => { caret = text.len(); if !e.shift { anchor = caret; } }
                    VK_BACK => { if lo != hi { text.drain(lo..hi); caret = lo; } else if caret > 0 { text.remove(caret - 1); caret -= 1; } anchor = caret; }
                    VK_DELETE => { if lo != hi { text.drain(lo..hi); caret = lo; } else if caret < text.len() { text.remove(caret); } anchor = caret; }
                    VK_A if e.ctrl => { anchor = 0; caret = text.len(); }
                    VK_C if e.ctrl => { if lo != hi { clip_set(&text[lo..hi]); } }
                    VK_X if e.ctrl => { if lo != hi { clip_set(&text[lo..hi]); text.drain(lo..hi); caret = lo; anchor = caret; } }
                    VK_V if e.ctrl => {
                        let ins: Vec<u16> = clip_get().into_iter().filter(|c| *c >= 32).collect();
                        text.splice(lo..hi, ins.iter().copied());
                        caret = lo + ins.len();
                        anchor = caret;
                    }
                    _ => rest.push(e),
                }
            }
            self.keys = rest;
            for c in std::mem::take(&mut self.chars) {
                if c >= 32 && c != 127 {
                    let (lo, hi) = (caret.min(anchor), caret.max(anchor));
                    text.splice(lo..hi, [c]);
                    caret = lo + 1;
                    anchor = caret;
                }
            }
        }
        *v = String::from_utf16_lossy(&text);
        let layout = layout_of(&text);
        let cx_of = |i: usize| -> f32 {
            let Some(l) = &layout else { return 0.0 };
            let (mut x, mut y) = (0f32, 0f32);
            let mut m = DWRITE_HIT_TEST_METRICS::default();
            unsafe { let _ = l.HitTestTextPosition(i as u32, false, &mut x, &mut y, &mut m); }
            x
        };
        let cx = cx_of(caret);
        if cx - off > inner.w { off = cx - inner.w + 2.0; }
        if cx - off < 0.0 { off = cx; }
        self.carets.insert(k, (caret, anchor, off));
        let hv = self.anim(k, if focused { 1.0 } else if hover { 0.4 } else { 0.0 }, 22.0);
        self.fill(q, 9.0, SURFACE, 1.0);
        self.stroke(q, 9.0, mix(0x3A3D42, ACCENT, hv), 1.0, 1.0);
        self.clip(inner);
        if focused && caret != anchor {
            let (a, b) = (cx_of(caret.min(anchor)), cx_of(caret.max(anchor)));
            self.fill(r(inner.x + a - off, q.y + 7.0, b - a, q.h - 14.0), 2.0, ACCENT, 0.32);
        }
        if text.is_empty() {
            self.text(placeholder, inner, 13.0, 400, FAINT, Align::Left);
        } else if let Some(l) = &layout {
            unsafe { self.rt().DrawTextLayout(pt(inner.x - off, q.y), l, self.b(TEXT, 1.0), D2D1_DRAW_TEXT_OPTIONS_NONE); }
        }
        if focused { self.fill(r(inner.x + cx - off, q.y + 8.0, 1.5, q.h - 16.0), 0.0, ACCENT, 1.0); }
        self.unclip();
    }

    /// Scrollable body: clips to `q` and returns the offset to apply. Call `unclip` after drawing the rows.
    pub fn scroll_area(&mut self, key: &str, q: R, content: f32) -> f32 {
        let k = id(key) ^ 0x55;
        let max = (content - q.h).max(0.0);
        let cur = self.anims.get(&k).copied().unwrap_or(0.0);
        let next = if q.has(self.mouse) { (cur - self.scroll * 48.0).clamp(0.0, max) } else { cur.clamp(0.0, max) };
        self.anims.insert(k, next);
        self.clip(q);
        next
    }

    fn begin(&mut self) {
        let now = std::time::Instant::now();
        self.dt = (now - self.last).as_secs_f32().clamp(0.001, 0.05);
        self.last = now;
        self.want_anim = false;
        self.cursor = IDC_ARROW;
        self.tooltip = None;
        if self.tab != 0 && !self.order.is_empty() {
            let n = self.order.len() as i32;
            let i = self.order.iter().position(|k| *k == self.focus).map(|i| i as i32);
            self.focus = self.order[match i { Some(i) => (i + self.tab).rem_euclid(n), None => if self.tab > 0 { 0 } else { n - 1 } } as usize];
        }
        self.tab = 0;
        self.order.clear();
    }

    fn end(&mut self) {
        if let Some((s, p)) = self.tooltip.take() {
            let tw = self.measure(&s, 12.0, 500) + 16.0;
            let x = (p.0 - tw / 2.0).clamp(4.0, (self.w - tw - 4.0).max(4.0));
            let q = r(x, p.1 - 30.0, tw, 24.0);
            self.fill(q, 7.0, RAISED, 1.0);
            self.stroke(q, 7.0, TEXT, 0.1, 1.0);
            self.text(&s, q, 12.0, 500, TEXT, Align::Center);
        }
        if self.released { self.active = 0; }
        self.clear_events();
    }

    fn clear_events(&mut self) {
        self.pressed = false; self.released = false; self.rpressed = false; self.double = false;
        self.scroll = 0.0;
        self.keys.clear();
        self.chars.clear();
    }
}

pub fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |s: u32| { let (x, y) = (((a >> s) & 255) as f32, ((b >> s) & 255) as f32); ((x + (y - x) * t) as u32) << s };
    ch(16) | ch(8) | ch(0)
}

fn clip_set(s: &[u16]) {
    unsafe {
        if OpenClipboard(None).is_err() { return; }
        let _ = EmptyClipboard();
        if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, (s.len() + 1) * 2) {
            let p = GlobalLock(h) as *mut u16;
            std::ptr::copy_nonoverlapping(s.as_ptr(), p, s.len());
            *p.add(s.len()) = 0;
            let _ = GlobalUnlock(h);
            let _ = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(h.0)));
        }
        let _ = CloseClipboard();
    }
}

fn clip_get() -> Vec<u16> {
    unsafe {
        if OpenClipboard(None).is_err() { return vec![]; }
        let mut out = vec![];
        if let Ok(h) = GetClipboardData(CF_UNICODETEXT.0 as u32) {
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if !p.is_null() {
                let mut i = 0;
                while *p.add(i) != 0 { out.push(*p.add(i)); i += 1; }
                let _ = GlobalUnlock(g);
            }
        }
        let _ = CloseClipboard();
        out
    }
}

// ---- host window ----

struct Host {
    hwnd: HWND,
    ui: Ui,
    page: Box<dyn Page>,
    surface: Option<Surface>,
    scale: f32,
    size: (f32, f32),
    busy: bool,
    modal: bool,
    restore: RECT,
    tracking: bool,
}

fn card_bg(ui: &Ui, w: f32, h: f32, maximized: bool) {
    if maximized { ui.fill(r(0.0, 0.0, w, h), 0.0, BG, 1.0); return; }
    for i in 0..14 {
        let g = 14.0 - i as f32;
        ui.fill(r(-g, -g + 8.0, w + g * 2.0, h + g * 2.0), CARD_R + g, 0, 0.018 + i as f32 * 0.0035);
    }
    ui.fill(r(0.0, 0.0, w, h), CARD_R, BG, 1.0);
    ui.stroke(r(0.0, 0.0, w, h), CARD_R, 0xFFFFFF, 0.09, 1.0);
}

fn margin(h: &Host) -> f32 { if h.ui.maximized { 0.0 } else { SHADOW } }

impl Host {
    fn outer_px(&self) -> (i32, i32) {
        let m = margin(self) * 2.0;
        (((self.size.0 + m) * self.scale).ceil() as i32, ((self.size.1 + m) * self.scale).ceil() as i32)
    }

    fn pass(&mut self) {
        let (w, h) = self.outer_px();
        if self.surface.as_ref().is_none_or(|s| s.w != w || s.h != h) {
            self.surface = Surface::new(&self.ui.gfx, w, h).ok();
            self.ui.rt = self.surface.as_ref().and_then(|s| s.rt.cast().ok());
            self.ui.brush = self.ui.rt.as_ref().and_then(|t| unsafe { t.CreateSolidColorBrush(&rgba(0, 1.0), None).ok() });
            self.ui.bitmaps.clear();
        }
        let m = margin(self);
        let card = self.page.card();
        let Some(s) = self.surface.as_ref() else { return };
        self.ui.w = self.size.0;
        self.ui.h = self.size.1;
        unsafe {
            s.rt.BeginDraw();
            s.rt.Clear(Some(&rgba(0, 0.0)));
            let sc = self.scale;
            s.rt.SetTransform(&windows_numerics::Matrix3x2 { M11: sc, M12: 0.0, M21: 0.0, M22: sc, M31: m * sc, M32: m * sc });
            self.ui.begin();
            if card { card_bg(&self.ui, self.size.0, self.size.1, self.ui.maximized); }
            self.page.frame(&mut self.ui);
            self.ui.end();
            let _ = s.rt.EndDraw(None, None);
        }
    }

    fn paint(&mut self) {
        if self.busy { return; }
        self.busy = true;
        let had_input = self.ui.pressed || self.ui.released || self.ui.rpressed || self.ui.double || !self.ui.keys.is_empty() || !self.ui.chars.is_empty() || self.ui.tab != 0;
        if !self.page.resizable() { self.size = self.page.size(); }
        self.pass();
        // The first pass reacts to input, the second shows the result in the same present.
        let mut again = had_input;
        if !self.page.resizable() && self.page.size() != self.size { self.size = self.page.size(); again = true; }
        if again { self.pass(); }
        let (w, h) = self.outer_px();
        let placed = self.page.place(w, h);
        unsafe {
            let mut wr = RECT::default();
            let _ = GetWindowRect(self.hwnd, &mut wr);
            let (x, y) = placed.map(|p| (p.x, p.y)).unwrap_or((wr.left, wr.top));
            if let Some(s) = &self.surface { s.present(self.hwnd, x, y); }
            if self.ui.want_anim { SetTimer(Some(self.hwnd), 1, 16, None); } else { let _ = KillTimer(Some(self.hwnd), 1); }
        }
        self.after_frame();
        self.busy = false;
        if self.page.done() && !self.modal { unsafe { let _ = PostMessageW(Some(self.hwnd), WM_APP_DESTROY, WPARAM(0), LPARAM(0)); } }
    }

    fn repaint_nested(&mut self) {
        self.busy = false;
        self.paint();
        self.busy = true;
    }

    fn after_frame(&mut self) {
        unsafe {
            if self.ui.want_drag {
                self.ui.want_drag = false;
                if !self.ui.maximized {
                    self.ui.active = 0;
                    self.ui.down = false;
                    let _ = ReleaseCapture();
                    SendMessageW(self.hwnd, WM_NCLBUTTONDOWN, Some(WPARAM(HTCAPTION as usize)), Some(LPARAM(0)));
                }
            }
            if self.ui.want_max {
                self.ui.want_max = false;
                self.ui.active = 0;
                let wa = work_area_at(cursor_pos());
                if self.ui.maximized {
                    self.ui.maximized = false;
                    let q = self.restore;
                    self.size = ((q.right - q.left) as f32 / self.scale - SHADOW * 2.0, (q.bottom - q.top) as f32 / self.scale - SHADOW * 2.0);
                    let _ = SetWindowPos(self.hwnd, None, q.left, q.top, q.right - q.left, q.bottom - q.top, SWP_NOZORDER);
                } else {
                    let _ = GetWindowRect(self.hwnd, &mut self.restore);
                    self.ui.maximized = true;
                    self.size = ((wa.right - wa.left) as f32 / self.scale, (wa.bottom - wa.top) as f32 / self.scale);
                    let _ = SetWindowPos(self.hwnd, None, wa.left, wa.top, wa.right - wa.left, wa.bottom - wa.top, SWP_NOZORDER);
                }
                self.repaint_nested();
            }
            if let Some(p) = self.ui.popup.take() {
                let mut wr = RECT::default();
                let _ = GetWindowRect(self.hwnd, &mut wr);
                let m = margin(self);
                let sc = self.scale;
                let (x, y) = (wr.left as f32 + (m + p.anchor.x) * sc, wr.top as f32 + (m + p.anchor.y) * sc);
                self.ui.active = 0;
                self.ui.down = false;
                let _ = ReleaseCapture();
                let chosen = menu(POINT { x: x as i32, y: (y + (p.anchor.h + 4.0) * sc) as i32 }, y as i32, p.anchor.w, p.items, p.sel);
                let _ = SetForegroundWindow(self.hwnd);
                let _ = SetFocus(Some(self.hwnd));
                self.ui.clear_events();
                if let Some(i) = chosen { self.ui.popup_result = Some((p.id, i)); self.ui.focus = p.id; }
                self.repaint_nested();
            }
            if self.page.after() {
                self.ui.active = 0;
                self.ui.down = false;
                self.ui.clear_events();
                if self.modal {
                    let _ = SetForegroundWindow(self.hwnd);
                    let _ = SetFocus(Some(self.hwnd));
                }
                self.repaint_nested();
            }
        }
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCALCSIZE { return LRESULT(0); }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Host;
        if ptr.is_null() { return DefWindowProcW(hwnd, msg, wp, lp); }
        let h = &mut *ptr;
        if let Some(res) = h.page.message(hwnd, msg, wp, lp) { return res; }
        let (px, py) = ((lp.0 & 0xFFFF) as i16 as f32, ((lp.0 >> 16) & 0xFFFF) as i16 as f32);
        let to_log = |h: &Host, x: f32, y: f32| { let m = margin(h); (x / h.scale - m, y / h.scale - m) };
        match msg {
            WM_NCHITTEST => {
                if h.page.resizable() && !h.ui.maximized {
                    let mut wr = RECT::default();
                    let _ = GetWindowRect(hwnd, &mut wr);
                    let (x, y) = to_log(h, px - wr.left as f32, py - wr.top as f32);
                    let (l, t, rr, b) = (x.abs() < RESIZE_BAND, y.abs() < RESIZE_BAND, (x - h.size.0).abs() < RESIZE_BAND, (y - h.size.1).abs() < RESIZE_BAND);
                    let ht = match (l, t, rr, b) {
                        (true, true, _, _) => HTTOPLEFT, (_, true, true, _) => HTTOPRIGHT, (true, _, _, true) => HTBOTTOMLEFT, (_, _, true, true) => HTBOTTOMRIGHT,
                        (true, ..) => HTLEFT, (_, true, ..) => HTTOP, (_, _, true, _) => HTRIGHT, (_, _, _, true) => HTBOTTOM, _ => HTCLIENT,
                    };
                    return LRESULT(ht as isize);
                }
                LRESULT(HTCLIENT as isize)
            }
            WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
                if let Ok(c) = LoadCursorW(None, h.ui.cursor) { SetCursor(Some(c)); }
                LRESULT(1)
            }
            WM_GETMINMAXINFO if h.page.resizable() => {
                let mm = &mut *(lp.0 as *mut MINMAXINFO);
                mm.ptMinTrackSize = POINT { x: (780.0 * h.scale) as i32, y: (520.0 * h.scale) as i32 };
                LRESULT(0)
            }
            WM_SIZE => {
                if h.page.resizable() && !h.busy {
                    let (w, ht) = ((lp.0 & 0xFFFF) as f32, ((lp.0 >> 16) & 0xFFFF) as f32);
                    let m = margin(h) * 2.0;
                    let n = (w / h.scale - m, ht / h.scale - m);
                    if n.0 > 10.0 && (n.0 - h.size.0).abs() + (n.1 - h.size.1).abs() > 0.5 { h.size = n; h.paint(); }
                }
                LRESULT(0)
            }
            WM_DPICHANGED => {
                h.scale = (wp.0 & 0xFFFF) as f32 / 96.0;
                let q = &*(lp.0 as *const RECT);
                let _ = SetWindowPos(hwnd, None, q.left, q.top, q.right - q.left, q.bottom - q.top, SWP_NOZORDER | SWP_NOACTIVATE);
                h.paint();
                LRESULT(0)
            }
            WM_TIMER | WM_APP_REFRESH => { h.paint(); LRESULT(0) }
            WM_APP_DESTROY => { let _ = DestroyWindow(hwnd); LRESULT(0) }
            WM_MOUSEMOVE => {
                h.ui.mouse = to_log(h, px, py);
                if !h.tracking {
                    let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    let _ = TrackMouseEvent(&mut t);
                    h.tracking = true;
                }
                h.paint();
                LRESULT(0)
            }
            WM_MOUSELEAVE => { h.tracking = false; if !h.ui.down { h.ui.mouse = (-1000.0, -1000.0); h.paint(); } LRESULT(0) }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                h.ui.mouse = to_log(h, px, py);
                h.ui.down = true;
                h.ui.pressed = true;
                h.ui.double = msg == WM_LBUTTONDBLCLK;
                SetCapture(hwnd);
                h.paint();
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                h.ui.mouse = to_log(h, px, py);
                h.ui.down = false;
                h.ui.released = true;
                let _ = ReleaseCapture();
                h.paint();
                LRESULT(0)
            }
            WM_RBUTTONDOWN => { h.ui.mouse = to_log(h, px, py); h.ui.rpressed = true; h.paint(); LRESULT(0) }
            WM_MOUSEWHEEL => { h.ui.scroll += ((wp.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0; h.paint(); LRESULT(0) }
            WM_KEYDOWN => {
                let vk = VIRTUAL_KEY(wp.0 as u16);
                let shift = GetKeyState(VK_SHIFT.0 as i32) < 0;
                if vk == VK_TAB { h.ui.tab = if shift { -1 } else { 1 }; }
                else { h.ui.keys.push(Key { vk, ctrl: GetKeyState(VK_CONTROL.0 as i32) < 0, shift }); }
                h.paint();
                LRESULT(0)
            }
            WM_CHAR => { h.ui.chars.push(wp.0 as u16); h.paint(); LRESULT(0) }
            WM_CLOSE => { if !h.busy { h.page.on_close(); h.paint(); } LRESULT(0) }
            WM_ACTIVATE => {
                // Deactivation arrives as a sent message, so wake the modal loop to notice.
                if (wp.0 & 0xFFFF) == 0 && !h.busy { h.page.on_deactivate(); let _ = PostMessageW(Some(hwnd), WM_APP_REFRESH, WPARAM(0), LPARAM(0)); }
                LRESULT(0)
            }
            WM_NCDESTROY => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(ptr));
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn register() -> Option<HINSTANCE> {
    unsafe {
        let inst: HINSTANCE = GetModuleHandleW(None).ok()?.into();
        let wc = WNDCLASSW {
            style: CS_DBLCLKS, lpfnWndProc: Some(wndproc), hInstance: inst, lpszClassName: w!("TauWindow"),
            hIcon: LoadIconW(Some(inst), PCWSTR(1 as *const u16)).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW).ok()?, ..Default::default()
        };
        RegisterClassW(&wc);
        Some(inst)
    }
}

/// `at` of None centres the window on the screen with the cursor.
fn create(page: Box<dyn Page>, title: &str, at: Option<POINT>, ex: WINDOW_EX_STYLE, modal: bool) -> Option<HWND> {
    unsafe {
        let inst = register()?;
        let t = wz(title);
        let style = if page.resizable() { WS_POPUP | WS_THICKFRAME } else { WS_POPUP };
        let hwnd = CreateWindowExW(WS_EX_LAYERED | ex, w!("TauWindow"), PCWSTR(t.as_ptr()), style, 0, 0, 10, 10, None, None, Some(inst), None).ok()?;
        let target = at.unwrap_or(cursor_pos());
        let scale = dpi_scale_at(target);
        let size = page.size();
        let host = Box::new(Host { hwnd, ui: Ui::new().ok()?, page, surface: None, scale, size, busy: false, modal, restore: RECT::default(), tracking: false });
        let raw = Box::into_raw(host);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize);
        let h = &mut *raw;
        // A throwaway frame lets a page measure its text and settle its height before it is seen.
        h.busy = true;
        h.pass();
        h.busy = false;
        if !h.page.resizable() { h.size = h.page.size(); }
        let (w, ph) = h.outer_px();
        let wa = work_area_at(target);
        let p = at.unwrap_or(POINT { x: wa.left + (wa.right - wa.left - w) / 2, y: wa.top + (wa.bottom - wa.top - ph) / 2 });
        let _ = SetWindowPos(hwnd, None, p.x, p.y, w, ph, SWP_NOZORDER | SWP_NOACTIVATE);
        h.paint();
        Some(hwnd)
    }
}

/// Lets the caller keep the page after the window closes. A nested window can deliver messages to
/// the outer one while the outer page is borrowed, so those are skipped instead of panicking.
struct Shared<P>(Rc<RefCell<P>>);
impl<P: Page> Page for Shared<P> {
    fn size(&self) -> (f32, f32) { self.0.borrow().size() }
    fn frame(&mut self, ui: &mut Ui) { self.0.borrow_mut().frame(ui) }
    fn after(&mut self) -> bool { self.0.borrow_mut().after() }
    fn done(&self) -> bool { self.0.try_borrow().is_ok_and(|p| p.done()) }
    fn on_close(&mut self) { if let Ok(mut p) = self.0.try_borrow_mut() { p.on_close(); } }
    fn on_deactivate(&mut self) { if let Ok(mut p) = self.0.try_borrow_mut() { p.on_deactivate(); } }
    fn resizable(&self) -> bool { self.0.try_borrow().is_ok_and(|p| p.resizable()) }
    fn card(&self) -> bool { self.0.borrow().card() }
    fn place(&mut self, w: i32, h: i32) -> Option<POINT> { self.0.borrow_mut().place(w, h) }
    fn message(&mut self, a: HWND, b: u32, c: WPARAM, d: LPARAM) -> Option<LRESULT> {
        self.0.try_borrow_mut().ok().and_then(|mut p| p.message(a, b, c, d))
    }
}

/// Shows a page and blocks, pumping messages, until it says it is done.
pub fn run_modal<P: Page + 'static>(page: P, title: &str, at: Option<POINT>) -> Option<P> {
    let shared = Rc::new(RefCell::new(page));
    let hwnd = create(Box::new(Shared(shared.clone())), title, at, WS_EX_TOPMOST, true)?;
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        let mut m = MSG::default();
        while !shared.try_borrow().map(|p| p.done()).unwrap_or(false) && GetMessageW(&mut m, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&m);
            DispatchMessageW(&m);
        }
        let _ = DestroyWindow(hwnd);
    }
    Rc::try_unwrap(shared).ok().map(RefCell::into_inner)
}

/// Shows a page that lives on its own, without taking focus.
pub fn open<P: Page + 'static>(page: P, title: &str, at: POINT, ex: WINDOW_EX_STYLE) -> Option<HWND> {
    let hwnd = create(Box::new(page), title, Some(at), ex, false)?;
    unsafe { let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE); }
    Some(hwnd)
}
pub fn refresh(hwnd: HWND) { unsafe { let _ = PostMessageW(Some(hwnd), WM_APP_REFRESH, WPARAM(0), LPARAM(0)); } }

/// Draws a page to a PNG, for design review. `mouse` puts the pointer somewhere to show hover states.
pub fn render_png<P: Page>(page: &mut P, scale: f32, mouse: Option<(f32, f32)>, out: &std::path::Path) -> std::result::Result<(), String> {
    let mut ui = Ui::new().map_err(|e| e.to_string())?;
    let mut surf: Option<Surface> = None;
    // Enough frames for measured sizes and animations to settle.
    for _ in 0..30 {
        let (cw, ch) = page.size();
        let (w, h) = (((cw + SHADOW * 2.0) * scale).ceil() as i32, ((ch + SHADOW * 2.0) * scale).ceil() as i32);
        if surf.as_ref().is_none_or(|s| s.w != w || s.h != h) {
            surf = Some(Surface::new(&ui.gfx, w, h).map_err(|e| e.to_string())?);
            ui.rt = surf.as_ref().and_then(|s| s.rt.cast().ok());
            ui.brush = ui.rt.as_ref().and_then(|t| unsafe { t.CreateSolidColorBrush(&rgba(0, 1.0), None).ok() });
            ui.bitmaps.clear();
        }
        let s = surf.as_ref().unwrap();
        ui.mouse = mouse.unwrap_or((-1000.0, -1000.0));
        ui.w = cw;
        ui.h = ch;
        unsafe {
            s.rt.BeginDraw();
            s.rt.Clear(Some(&rgba(0x3A3D44, 1.0)));
            s.rt.SetTransform(&windows_numerics::Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: SHADOW * scale, M32: SHADOW * scale });
            ui.begin();
            ui.dt = 0.05;
            if page.card() { card_bg(&ui, cw, ch, false); }
            page.frame(&mut ui);
            ui.end();
            let _ = s.rt.EndDraw(None, None);
        }
    }
    let s = surf.unwrap();
    crate::image::write_rgba_png(&s.to_rgba(), s.w as u32, s.h as u32, out)
}

// ---- pop-up menu, for the tray and for dropdowns ----

#[derive(Clone)]
pub struct MenuItem { pub label: String, pub icon: u16, pub sep: bool }
impl MenuItem {
    pub fn new(l: &str, icon: u16) -> MenuItem { MenuItem { label: l.to_string(), icon, sep: false } }
    pub fn separator() -> MenuItem { MenuItem { label: String::new(), icon: 0, sep: true } }
}

const MENU_ROW: f32 = 34.0;

pub struct Menu { width: f32, items: Vec<MenuItem>, sel: Option<usize>, header: Option<String>, cur: Option<usize>, last_mouse: (f32, f32), pub chosen: Option<usize>, closed: bool }

impl Menu {
    pub fn new(width: f32, items: Vec<MenuItem>, sel: Option<usize>, header: Option<String>) -> Menu {
        Menu { width, items, sel, header, cur: sel, last_mouse: (-1.0, -1.0), chosen: None, closed: false }
    }
    fn head(&self) -> f32 { if self.header.is_some() { 48.0 } else { 6.0 } }
    fn step(&mut self, d: i32) {
        let n = self.items.len() as i32;
        let mut i = self.cur.map(|c| c as i32).unwrap_or(if d > 0 { -1 } else { n });
        for _ in 0..n {
            i = (i + d).rem_euclid(n);
            if !self.items[i as usize].sep { self.cur = Some(i as usize); return; }
        }
    }
}

impl Page for Menu {
    fn size(&self) -> (f32, f32) {
        let body: f32 = self.items.iter().map(|i| if i.sep { 9.0 } else { MENU_ROW }).sum();
        (self.width, self.head() + body + 6.0)
    }
    fn done(&self) -> bool { self.closed }
    fn on_close(&mut self) { self.closed = true; }
    fn on_deactivate(&mut self) { self.closed = true; }
    fn frame(&mut self, ui: &mut Ui) {
        if ui.key(VK_ESCAPE) { self.closed = true; }
        if ui.key(VK_DOWN) { self.step(1); }
        if ui.key(VK_UP) { self.step(-1); }
        if (ui.key(VK_RETURN) || ui.key(VK_SPACE)) && self.cur.is_some() { self.chosen = self.cur; self.closed = true; }
        if let Some(h) = self.header.clone() {
            let _ = draw_logo(&ui.gfx, ui.rt(), 14.0, 13.0, 22.0);
            ui.text(&h, r(44.0, 0.0, self.width - 56.0, 47.0), 13.0, 600, TEXT, Align::Left);
            ui.divider(47.0, 0.0, self.width);
        }
        let moved = ui.mouse != self.last_mouse;
        self.last_mouse = ui.mouse;
        let mut y = self.head();
        let any_icons = self.items.iter().any(|i| i.icon != 0);
        for (i, it) in self.items.clone().iter().enumerate() {
            if it.sep { ui.divider(y + 4.0, 0.0, self.width); y += 9.0; continue; }
            let q = r(6.0, y, self.width - 12.0, MENU_ROW);
            if q.has(ui.mouse) && moved { self.cur = Some(i); }
            if q.has(ui.mouse) && ui.released { self.chosen = Some(i); self.closed = true; }
            let on = self.cur == Some(i);
            if on { ui.fill(q, 8.0, TEXT, 0.09); }
            let tx = if any_icons {
                ui.icon(it.icon, r(q.x + 8.0, q.y, 20.0, q.h), 13.0, if on { TEXT } else { MUTED }, 1.0);
                q.x + 36.0
            } else { q.x + 10.0 };
            ui.text(&it.label, r(tx, q.y, q.right() - tx - 26.0, q.h), 13.0, 400, TEXT, Align::Left);
            if self.sel == Some(i) { ui.icon(glyph::CHECK, r(q.right() - 26.0, q.y, 20.0, q.h), 11.0, ACCENT, 1.0); }
            y += MENU_ROW;
        }
    }
}

/// Opens at `at` in pixels. If it would run off the bottom it opens upward from `flip_y`.
pub fn menu(at: POINT, flip_y: i32, width: f32, items: Vec<MenuItem>, sel: Option<usize>) -> Option<usize> {
    show_menu(Menu::new(width.max(120.0), items, sel, None), at, flip_y)
}

pub fn show_menu(page: Menu, at: POINT, flip_y: i32) -> Option<usize> {
    let scale = dpi_scale_at(at);
    let (w, h) = page.size();
    let (pw, ph) = (((w + SHADOW * 2.0) * scale) as i32, ((h + SHADOW * 2.0) * scale) as i32);
    let wa = work_area_at(at);
    let m = (SHADOW * scale) as i32;
    let x = (at.x - m).clamp(wa.left - m, (wa.right + m - pw).max(wa.left - m));
    let y = if at.y + ph - m > wa.bottom { (flip_y - ph + m).max(wa.top - m) } else { at.y - m };
    run_modal(page, "Tau menu", Some(POINT { x, y })).and_then(|p| p.chosen)
}
