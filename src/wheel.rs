//! The orbit wheel: geometry, drawing, the layered window, and the mid-drag drop target.
use crate::catalog::{self, Entry, Kind};
use crate::gfx::*;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::*;
use windows::Win32::System::SystemServices::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const DISC_R: f32 = 176.0;
pub const ORBIT_R: f32 = 122.0;
pub const NODE_R: f32 = 35.0;
pub const HUB_R: f32 = 52.0;
const PAD: f32 = 14.0;
const OPEN_MS: f32 = 140.0;
const PROBE_MS: u32 = 300;

pub const WM_APP_KEY: u32 = WM_APP + 1; // wparam: Kind index
pub const WM_APP_RUN: u32 = WM_APP + 2;
pub const WM_APP_NAV: u32 = WM_APP + 3; // wparam: 0 prev, 1 next, 2 apply, 3 cancel
const WM_APP_SCROLL: u32 = WM_APP + 4; // wparam: signed count of wheels to move by
const TIMER_PROBE: usize = 1;
const TIMER_ANIM: usize = 2;
const TIMER_MODAL: usize = 3;
const TIMER_NUDGE: usize = 4;
const NUDGE_MS: u32 = 30;
const SCROLL_NOTCH: i32 = WHEEL_DELTA as i32;
const KINDS: [Kind; 3] = [Kind::Convert, Kind::Tools, Kind::Actions];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode { Hidden, Probing, Drag, Modal }

/// Which node a point selects. `None` inside the hub, which is the cancel zone.
/// Node 0 sits at twelve o'clock and the rest run clockwise.
pub fn pick(dx: f32, dy: f32, count: usize) -> Option<usize> {
    if count == 0 || dx * dx + dy * dy < HUB_R * HUB_R { return None; }
    let tau = std::f32::consts::TAU;
    let a = dx.atan2(-dy).rem_euclid(tau);
    let step = tau / count as f32;
    Some(((a + step / 2.0) / step) as usize % count)
}

pub fn node_center(i: usize, count: usize, radius: f32) -> (f32, f32) {
    let a = i as f32 / count as f32 * std::f32::consts::TAU;
    (a.sin() * radius, -a.cos() * radius)
}

/// The wheel `steps` places along from `kind`, wrapping at both ends.
fn cycled(kind: Kind, steps: i32) -> Kind {
    let i = KINDS.iter().position(|k| *k == kind).unwrap_or(0) as i32;
    KINDS[(i + steps).rem_euclid(KINDS.len() as i32) as usize]
}

/// Adds a scroll delta to the running total and returns (whole notches, new total).
/// Positive deltas scroll up. High resolution wheels send fractions of a notch, and turning back drops any leftover.
fn scroll_notches(total: i32, delta: i32) -> (i32, i32) {
    let sum = if total.signum() * delta.signum() < 0 { delta } else { total + delta };
    (sum / SCROLL_NOTCH, sum % SCROLL_NOTCH)
}

pub struct View<'a> {
    pub kind: Kind,
    pub entries: &'a [Entry],
    pub hover: Option<usize>,
    pub open: f32,
    pub count: usize,
}

fn draw_text(rt: &ID2D1RenderTarget, f: &IDWriteTextFormat, s: &str, x: f32, y: f32, w: f32, h: f32, b: &ID2D1SolidColorBrush) {
    let t = wide(s);
    let r = D2D_RECT_F { left: x - w / 2.0, top: y - h / 2.0, right: x + w / 2.0, bottom: y + h / 2.0 };
    unsafe { rt.DrawText(&t, f, &r, b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL); }
}

/// Draws the wheel centred at (cx, cy) in DIPs. Used for the window and for --render-wheel.
pub fn draw(g: &Gfx, rt: &ID2D1RenderTarget, cx: f32, cy: f32, v: &View) -> Result<()> {
    unsafe {
        let ease = 1.0 - (1.0 - v.open).powi(3);
        let brush = |c: u32, a: f32| rt.CreateSolidColorBrush(&rgba(c, a), None);

        // Disc with a soft outer shadow. Every pixel is non-zero alpha so OLE hit-tests the whole disc.
        let stops = [
            D2D1_GRADIENT_STOP { position: 0.0, color: rgba(0, 0.45 * ease) },
            D2D1_GRADIENT_STOP { position: DISC_R / (DISC_R + PAD) - 0.01, color: rgba(0, 0.45 * ease) },
            D2D1_GRADIENT_STOP { position: 1.0, color: rgba(0, 0.0) },
        ];
        let coll = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
        let shadow = rt.CreateRadialGradientBrush(
            &D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES { center: pt(cx, cy), gradientOriginOffset: pt(0.0, 0.0), radiusX: DISC_R + PAD, radiusY: DISC_R + PAD },
            None, &coll,
        )?;
        rt.FillEllipse(&ellipse(cx, cy, DISC_R + PAD), &shadow);
        rt.FillEllipse(&ellipse(cx, cy, DISC_R * (0.82 + 0.18 * ease)), &brush(BG, 0.96 * ease)?);

        // The orbit, drawn as a thin ring, never as wedges.
        let orbit = ORBIT_R * ease;
        rt.DrawEllipse(&ellipse(cx, cy, orbit), &brush(BORDER, ease)?, 1.5, None);

        let n = v.entries.len();
        let label = g.format("Segoe UI", 11.0, DWRITE_FONT_WEIGHT_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER)?;
        let small = g.format("Segoe UI", 9.0, DWRITE_FONT_WEIGHT_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER)?;
        let icon = g.format("Segoe MDL2 Assets", 18.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER)?;

        // Tether and comet for the active node, under the nodes.
        if let Some(h) = v.hover.filter(|&h| h < n) {
            if v.entries[h].blocked.is_none() {
                let (nx, ny) = node_center(h, n, orbit);
                let len = (nx * nx + ny * ny).sqrt().max(1.0);
                let (ux, uy) = (nx / len, ny / len);
                rt.DrawLine(pt(cx + ux * HUB_R, cy + uy * HUB_R), pt(cx + nx, cy + ny), &brush(ACCENT, ease)?, 2.5, None);
                let sweep = std::f32::consts::TAU / n as f32 * 0.42;
                let mid = h as f32 / n as f32 * std::f32::consts::TAU;
                let steps = 18;
                for s in 0..steps {
                    let t0 = s as f32 / steps as f32;
                    let t1 = (s + 1) as f32 / steps as f32;
                    let (a0, a1) = (mid - sweep * t1, mid - sweep * t0);
                    let p0 = pt(cx + a0.sin() * orbit, cy - a0.cos() * orbit);
                    let p1 = pt(cx + a1.sin() * orbit, cy - a1.cos() * orbit);
                    rt.DrawLine(p0, p1, &brush(ACCENT, (1.0 - t0) * 0.9 * ease)?, 3.0, None);
                }
            }
        }

        for (i, e) in v.entries.iter().enumerate() {
            let (dx, dy) = node_center(i, n, orbit);
            let (x, y) = (cx + dx, cy + dy);
            let active = v.hover == Some(i);
            let blocked = e.blocked.is_some();
            let r = NODE_R * ease * if active && !blocked { 1.16 } else { 1.0 };
            let fill = if active && !blocked { ACCENT } else { SURFACE };
            if active && !blocked {
                rt.FillEllipse(&ellipse(x, y, r + 6.0), &brush(ACCENT, 0.28 * ease)?);
            }
            rt.FillEllipse(&ellipse(x, y, r), &brush(fill, if blocked { 0.55 * ease } else { ease })?);
            rt.DrawEllipse(&ellipse(x, y, r), &brush(if active { ACCENT } else { BORDER }, ease)?, 1.0, None);
            let fg = if active && !blocked { ON_ACCENT } else if blocked { MUTED } else { TEXT };
            let fb = brush(fg, ease * if blocked { 0.7 } else { 1.0 })?;
            if e.icon != 0 {
                let glyph = String::from_utf16_lossy(&[e.icon]);
                draw_text(rt, &icon, &glyph, x, y - 8.0, 60.0, 24.0, &fb);
                draw_text(rt, &small, e.label, x, y + 13.0, 68.0, 14.0, &fb);
            } else {
                draw_text(rt, &label, e.label, x, y, 66.0, 22.0, &fb);
            }
        }

        // Hub: the current choice, and which wheel this is.
        rt.FillEllipse(&ellipse(cx, cy, HUB_R), &brush(SURFACE, ease)?);
        rt.DrawEllipse(&ellipse(cx, cy, HUB_R), &brush(BORDER, ease)?, 1.0, None);
        let hub = g.format("Segoe UI", 13.0, DWRITE_FONT_WEIGHT_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER)?;
        let cap = g.format("Segoe UI", 9.5, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER)?;
        let (main, sub): (String, String) = match v.hover.and_then(|h| v.entries.get(h)) {
            Some(e) if e.blocked.is_some() => (e.label.to_string(), e.blocked.unwrap().to_string()),
            Some(e) => (e.label.to_string(), if v.count > 1 { format!("{} files", v.count) } else { v.kind.title().to_string() }),
            None if n == 0 => ("Nothing here".to_string(), "try another wheel".to_string()),
            None => (v.kind.title().to_string(), if v.count > 1 { format!("{} files", v.count) } else { "drop here to cancel".to_string() }),
        };
        draw_text(rt, &hub, &main, cx, cy - 8.0, 96.0, 20.0, &brush(TEXT, ease)?);
        let cap_font = if sub.len() > 14 { g.format("Segoe UI", 8.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER)? } else { cap };
        draw_text(rt, &cap_font, &sub, cx, cy + 10.0, 96.0, 14.0, &brush(MUTED, ease)?);

        // Three pips under the hub text: which of the three wheels is showing.
        for (i, k) in KINDS.iter().enumerate() {
            let x = cx + (i as f32 - 1.0) * 10.0;
            let on = *k == v.kind;
            rt.FillEllipse(&ellipse(x, cy + 28.0, 2.5), &brush(if on { ACCENT } else { BORDER }, ease)?);
        }
        Ok(())
    }
}

// ---- window ----

struct Wheel {
    hwnd: HWND,
    gfx: Gfx,
    surface: Option<Surface>,
    mode: Mode,
    kind: Kind,
    paths: Vec<PathBuf>,
    entries: Vec<Entry>,
    hover: Option<usize>,
    last_mouse: POINT,
    /// Screen position of the window's top-left and of the wheel's centre, in pixels.
    origin: POINT,
    center: POINT,
    scale: f32,
    opened_at: std::time::Instant,
    pending: Option<(Kind, Entry)>,
    helper: Option<IDropTargetHelper>,
    mouse_hook: Option<HHOOK>,
}

/// Scroll not yet adding up to a whole notch.
static SCROLL_TOTAL: AtomicI32 = AtomicI32::new(0);

thread_local! { static W: RefCell<Option<Wheel>> = const { RefCell::new(None) }; }

fn with<R>(f: impl FnOnce(&mut Wheel) -> R) -> Option<R> {
    W.with(|w| w.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(f)))
}

fn side(scale: f32) -> i32 { ((DISC_R + PAD) * 2.0 * scale).ceil() as i32 }

impl Wheel {
    fn place(&mut self, at: POINT) {
        let scale = dpi_scale_at(at);
        let s = side(scale);
        let wa = work_area_at(at);
        // Keep the disc on screen. The centre can move off the cursor at screen edges; picking is by angle so that is fine.
        let x = (at.x - s / 2).clamp(wa.left, (wa.right - s).max(wa.left));
        let y = (at.y - s / 2).clamp(wa.top, (wa.bottom - s).max(wa.top));
        self.origin = POINT { x, y };
        self.center = POINT { x: x + s / 2, y: y + s / 2 };
        if self.scale != scale || self.surface.is_none() {
            self.scale = scale;
            self.surface = Surface::new(&self.gfx, s, s).ok();
        }
    }

    fn set_kind(&mut self, kind: Kind) {
        self.kind = kind;
        self.entries = catalog::entries(kind, &self.paths, &crate::engines());
        self.hover = None;
        self.opened_at = std::time::Instant::now();
        unsafe { SetTimer(Some(self.hwnd), TIMER_ANIM, 8, None); }
    }

    fn paint(&mut self) {
        let Some(s) = self.surface.as_ref() else { return };
        let open = (self.opened_at.elapsed().as_secs_f32() * 1000.0 / OPEN_MS).min(1.0);
        unsafe {
            s.rt.BeginDraw();
            // Never fully clear: a transparent pixel under the cursor is click-through, OLE would see
            // the drag leave mid-animation and the wheel would vanish. 1/255 is invisible but solid.
            s.rt.Clear(Some(&rgba(0, 1.0 / 255.0)));
            s.rt.SetTransform(&windows_numerics::Matrix3x2 { M11: self.scale, M12: 0.0, M21: 0.0, M22: self.scale, M31: 0.0, M32: 0.0 });
            let c = (DISC_R + PAD) as f32;
            let view = View { kind: self.kind, entries: &self.entries, hover: self.hover, open, count: self.paths.len() };
            let rt: ID2D1RenderTarget = s.rt.cast().unwrap();
            let _ = draw(&self.gfx, &rt, c, c, &view);
            let _ = s.rt.EndDraw(None, None);
        }
        s.present(self.hwnd, self.origin.x, self.origin.y);
        if open >= 1.0 { unsafe { let _ = KillTimer(Some(self.hwnd), TIMER_ANIM); } }
    }

    fn hover_at(&mut self, p: POINT) -> bool {
        let dx = (p.x - self.center.x) as f32 / self.scale;
        let dy = (p.y - self.center.y) as f32 / self.scale;
        let h = pick(dx, dy, self.entries.len());
        if h != self.hover { self.hover = h; self.paint(); true } else { false }
    }

    /// A drag owns the mouse, so scrolling can only be seen from a hook. It runs on every mouse
    /// move, so it is only installed while the wheel is up, to keep it from ever slowing the cursor.
    fn hook_mouse(&mut self) {
        if self.mouse_hook.is_some() { return; }
        SCROLL_TOTAL.store(0, Ordering::Relaxed);
        unsafe {
            let inst = GetModuleHandleW(None).unwrap_or_default();
            self.mouse_hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), Some(inst.into()), 0).ok();
        }
    }

    fn hide(&mut self) {
        self.mode = Mode::Hidden;
        unsafe {
            if let Some(h) = self.mouse_hook.take() { let _ = UnhookWindowsHookEx(h); }
            for t in [TIMER_PROBE, TIMER_ANIM, TIMER_MODAL, TIMER_NUDGE] { let _ = KillTimer(Some(self.hwnd), t); }
            // Blank the bitmap first so a later show cannot flash the old wheel.
            if let Some(s) = &self.surface { s.fill_raw(0); s.present(self.hwnd, self.origin.x, self.origin.y); }
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    fn take_active(&mut self) -> bool {
        match self.hover.and_then(|h| self.entries.get(h)).cloned() {
            Some(e) if e.blocked.is_none() => {
                self.pending = Some((self.kind, e));
                unsafe { let _ = PostMessageW(Some(self.hwnd), WM_APP_RUN, WPARAM(0), LPARAM(0)); }
                true
            }
            _ => false,
        }
    }
}

// ---- drop target ----

#[windows_core::implement(IDropTarget)]
struct Target;

fn hdrop_paths(data: &IDataObject) -> Vec<PathBuf> {
    let fmt = FORMATETC { cfFormat: CF_HDROP.0, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
    let mut out = vec![];
    unsafe {
        let Ok(mut med) = data.GetData(&fmt) else { return out };
        let h = HDROP(med.u.hGlobal.0);
        let n = DragQueryFileW(h, 0xFFFF_FFFF, None);
        for i in 0..n {
            let mut buf = vec![0u16; 32768];
            let len = DragQueryFileW(h, i, Some(&mut buf)) as usize;
            out.push(PathBuf::from(String::from_utf16_lossy(&buf[..len])));
        }
        ReleaseStgMedium(&mut med);
    }
    out
}

fn effect_for(hit: bool) -> DROPEFFECT { if hit { DROPEFFECT_COPY } else { DROPEFFECT_NONE } }

impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(&self, data: Ref<'_, IDataObject>, _keys: MODIFIERKEYS_FLAGS, p: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let pos = POINT { x: p.x, y: p.y };
        let data = data.ok()?;
        let paths = hdrop_paths(data);
        let hit = with(|w| {
            if paths.is_empty() || w.mode == Mode::Modal || w.mode == Mode::Hidden { return false; }
            unsafe { let _ = KillTimer(Some(w.hwnd), TIMER_PROBE); }
            if let Some(h) = &w.helper {
                unsafe { let _ = h.DragEnter(w.hwnd, data, &pos as *const POINT, DROPEFFECT_COPY); }
            }
            let first = w.mode == Mode::Probing;
            w.mode = Mode::Drag;
            w.paths = paths.clone();
            if first { w.set_kind(w.kind); }
            w.paint();
            w.hover_at(pos);
            w.hover.and_then(|h| w.entries.get(h)).is_some_and(|e| e.blocked.is_none())
        }).unwrap_or(false);
        unsafe { *effect = effect_for(hit); }
        Ok(())
    }

    fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, p: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let pos = POINT { x: p.x, y: p.y };
        let hit = with(|w| {
            if let Some(h) = &w.helper { unsafe { let _ = h.DragOver(&pos as *const POINT, DROPEFFECT_COPY); } }
            w.hover_at(pos);
            w.hover.and_then(|h| w.entries.get(h)).is_some_and(|e| e.blocked.is_none())
        }).unwrap_or(false);
        unsafe { *effect = effect_for(hit); }
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        with(|w| {
            if let Some(h) = &w.helper { unsafe { let _ = h.DragLeave(); } }
            if w.mode == Mode::Drag { w.hide(); }
        });
        Ok(())
    }

    fn Drop(&self, data: Ref<'_, IDataObject>, _keys: MODIFIERKEYS_FLAGS, p: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let pos = POINT { x: p.x, y: p.y };
        let hit = with(|w| {
            if let (Some(h), Ok(d)) = (&w.helper, data.ok()) { unsafe { let _ = h.Drop(d, &pos as *const POINT, DROPEFFECT_COPY); } }
            w.hover_at(pos);
            let ok = w.take_active();
            w.hide();
            ok
        }).unwrap_or(false);
        // COPY, never MOVE: the source must not delete the original.
        unsafe { *effect = effect_for(hit); }
        Ok(())
    }
}

// ---- window proc and public API ----

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_APP_KEY => {
                let kind = match wp.0 { 0 => Kind::Convert, 1 => Kind::Tools, _ => Kind::Actions };
                on_key(kind);
                LRESULT(0)
            }
            WM_APP_RUN => {
                // Taken out first: dispatch may pump messages and re-enter this window.
                if let Some(((kind, entry), paths)) = with(|w| w.pending.take().map(|p| (p, w.paths.clone()))).flatten() {
                    crate::dispatch(kind, entry.id, paths);
                }
                LRESULT(0)
            }
            WM_APP_NAV => { on_nav(wp.0); LRESULT(0) }
            WM_APP_SCROLL => { on_scroll(wp.0 as isize as i32); LRESULT(0) }
            WM_TIMER => {
                match wp.0 {
                    // Back and forth in separate steps until OLE answers, leaving the cursor where it was.
                    TIMER_NUDGE => {
                        let probing = with(|w| w.mode == Mode::Probing).unwrap_or(false);
                        if probing { nudge(); } else { let _ = KillTimer(Some(hwnd), TIMER_NUDGE); }
                    }
                    TIMER_PROBE => {
                        // No DragEnter arrived: this was not a file drag.
                        with(|w| if w.mode == Mode::Probing { w.hide(); });
                    }
                    TIMER_ANIM => { with(|w| w.paint()); }
                    TIMER_MODAL => {
                        // Modal wheel has no focus; a press elsewhere dismisses it.
                        with(|w| {
                            if w.mode == Mode::Modal && GetAsyncKeyState(VK_LBUTTON.0 as i32) < 0 {
                                let p = cursor_pos();
                                let s = side(w.scale);
                                let inside = p.x >= w.origin.x && p.y >= w.origin.y && p.x < w.origin.x + s && p.y < w.origin.y + s;
                                if !inside { w.hide(); }
                            }
                        });
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                // Windows also sends this when a layered window repaints under a still cursor.
                // Only real movement may replace a selection made with the keyboard.
                with(|w| if w.mode == Mode::Modal {
                    let p = cursor_pos();
                    if p.x != w.last_mouse.x || p.y != w.last_mouse.y { w.last_mouse = p; w.hover_at(p); }
                });
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                with(|w| if w.mode == Mode::Modal {
                    w.hover_at(cursor_pos());
                    if w.take_active() { w.hide(); }
                });
                LRESULT(0)
            }
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn on_key(kind: Kind) {
    with(|w| match w.mode {
        Mode::Hidden => {
            // Probe: a nearly invisible window under the cursor plus a one pixel nudge.
            // If a file drag is running OLE calls DragEnter; if not, the timer hides us silently.
            let at = cursor_pos();
            w.place(at);
            w.kind = kind;
            w.mode = Mode::Probing;
            w.hook_mouse();
            let Some(s) = w.surface.as_ref() else { return };
            s.fill_raw(0x0100_0000);
            unsafe {
                let side = s.w;
                let _ = SetWindowPos(w.hwnd, Some(HWND_TOPMOST), w.origin.x, w.origin.y, side, side, SWP_NOACTIVATE);
                s.present(w.hwnd, w.origin.x, w.origin.y);
                let _ = ShowWindow(w.hwnd, SW_SHOWNOACTIVATE);
                SetTimer(Some(w.hwnd), TIMER_PROBE, PROBE_MS, None);
                SetTimer(Some(w.hwnd), TIMER_NUDGE, NUDGE_MS, None);
                nudge();
            }
        }
        Mode::Probing => { w.kind = kind; }
        Mode::Drag | Mode::Modal => { if w.kind != kind { w.set_kind(kind); w.paint(); } }
    });
}

fn on_nav(cmd: usize) {
    with(|w| {
        if w.mode != Mode::Modal { return; }
        let n = w.entries.len();
        match cmd {
            0 | 1 if n > 0 => {
                w.hover = Some(match (w.hover, cmd) {
                    (None, 0) => n - 1,
                    (None, _) => 0,
                    (Some(h), 0) => (h + n - 1) % n,
                    (Some(h), _) => (h + 1) % n,
                });
                w.paint();
            }
            2 => { if w.take_active() { w.hide(); } }
            3 => w.hide(),
            4 => {
                w.set_kind(cycled(w.kind, 1));
                w.paint();
            }
            _ => {}
        }
    });
}

fn on_scroll(steps: i32) {
    with(|w| match w.mode {
        Mode::Hidden => {}
        Mode::Probing => w.kind = cycled(w.kind, steps),
        Mode::Drag | Mode::Modal => {
            w.set_kind(cycled(w.kind, steps));
            // A drag selects by position alone, so the cursor's node stays selected on the new wheel.
            if w.mode == Mode::Drag { w.hover_at(cursor_pos()); }
            w.paint();
        }
    });
}

/// Runs on the UI thread with a hard time budget, so it only posts a message. Only installed while the wheel is up.
unsafe extern "system" fn mouse_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && wp.0 as u32 == WM_MOUSEWHEEL {
        let delta = ((*(lp.0 as *const MSLLHOOKSTRUCT)).mouseData >> 16) as i16 as i32;
        let (notches, rest) = scroll_notches(SCROLL_TOTAL.load(Ordering::Relaxed), delta);
        SCROLL_TOTAL.store(rest, Ordering::Relaxed);
        if notches != 0 {
            // Scrolling down moves forward through the wheels.
            let hwnd = HWND(crate::WHEEL.load(Ordering::Relaxed) as *mut _);
            let _ = PostMessageW(Some(hwnd), WM_APP_SCROLL, WPARAM(-notches as isize as usize), LPARAM(0));
        }
        // The window under the wheel must not scroll as well.
        return LRESULT(1);
    }
    CallNextHookEx(None, code, wp, lp)
}

/// One pixel per call. A move and its undo in the same SendInput merge into no move at all, and OLE never looks.
unsafe fn nudge() {
    use std::sync::atomic::AtomicBool;
    static BACK: AtomicBool = AtomicBool::new(false);
    let dx = if BACK.fetch_xor(true, Ordering::Relaxed) { -1 } else { 1 };
    let mi = MOUSEINPUT { dx, dy: 0, mouseData: 0, dwFlags: MOUSEEVENTF_MOVE, time: 0, dwExtraInfo: 0 };
    SendInput(&[INPUT { r#type: INPUT_MOUSE, Anonymous: INPUT_0 { mi } }], std::mem::size_of::<INPUT>() as i32);
}

/// Creates the hidden wheel window and registers it as a drop target. Call once on the UI thread.
pub fn init() -> Result<HWND> {
    unsafe {
        let inst = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc), hInstance: inst.into(), lpszClassName: w!("OrreryWheel"),
            hCursor: LoadCursorW(None, IDC_ARROW)?, ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("OrreryWheel"), w!("Orrery"), WS_POPUP, 0, 0, 10, 10, None, None, Some(inst.into()), None,
        )?;
        let target: IDropTarget = Target.into();
        RegisterDragDrop(hwnd, &target)?;
        let helper: Option<IDropTargetHelper> = CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER).ok();
        let wheel = Wheel {
            hwnd, gfx: Gfx::new()?, surface: None, mode: Mode::Hidden, kind: Kind::Convert,
            paths: vec![], entries: vec![], hover: None, last_mouse: POINT::default(), origin: POINT::default(), center: POINT::default(),
            scale: 0.0, opened_at: std::time::Instant::now(), pending: None, helper, mouse_hook: None,
        };
        W.with(|w| *w.borrow_mut() = Some(wheel));
        Ok(hwnd)
    }
}

pub fn mode() -> Mode { with(|w| w.mode).unwrap_or(Mode::Hidden) }

/// Opens the wheel without a drag, for the hotkey and Send To. Keys and mouse clicks drive it.
pub fn open_modal(paths: Vec<PathBuf>, kind: Kind) {
    with(|w| {
        let at = cursor_pos();
        w.place(at);
        w.last_mouse = at;
        w.paths = paths;
        w.mode = Mode::Modal;
        w.set_kind(kind);
        if w.surface.is_none() { w.mode = Mode::Hidden; return; }
        w.hook_mouse();
        let s = side(w.scale);
        unsafe {
            let _ = SetWindowPos(w.hwnd, Some(HWND_TOPMOST), w.origin.x, w.origin.y, s, s, SWP_NOACTIVATE);
            w.paint();
            let _ = ShowWindow(w.hwnd, SW_SHOWNOACTIVATE);
            SetTimer(Some(w.hwnd), TIMER_MODAL, 40, None);
        }
    });
}

/// Draws one wheel to a PNG for design review, with the given node active.
pub fn render_png(kind: Kind, paths: &[PathBuf], hover: Option<usize>, scale: f32, out: &std::path::Path) -> Result<()> {
    let g = Gfx::new()?;
    let s = side(scale);
    let surf = Surface::new(&g, s, s)?;
    let en = catalog::entries(kind, paths, &crate::engines());
    unsafe {
        surf.rt.BeginDraw();
        surf.rt.Clear(Some(&rgba(0, 0.0)));
        surf.rt.SetTransform(&windows_numerics::Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: 0.0, M32: 0.0 });
        let c = DISC_R + PAD;
        let view = View { kind, entries: &en, hover, open: 1.0, count: paths.len() };
        let rt: ID2D1RenderTarget = surf.rt.cast()?;
        draw(&g, &rt, c, c, &view)?;
        surf.rt.EndDraw(None, None)?;
    }
    crate::image::write_rgba_png(&surf.to_rgba(), s as u32, s as u32, out).map_err(|e| Error::new(E_FAIL, e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cycling_wraps_both_ways() {
        assert_eq!(cycled(Kind::Convert, 1), Kind::Tools);
        assert_eq!(cycled(Kind::Actions, 1), Kind::Convert);
        assert_eq!(cycled(Kind::Convert, -1), Kind::Actions);
        assert_eq!(cycled(Kind::Tools, 3), Kind::Tools);
    }
    #[test]
    fn one_notch_is_one_step() {
        assert_eq!(scroll_notches(0, 120), (1, 0));
        assert_eq!(scroll_notches(0, -120), (-1, 0));
        assert_eq!(scroll_notches(0, 360), (3, 0));
    }
    #[test]
    fn fine_scrolling_adds_up_to_a_notch() {
        assert_eq!(scroll_notches(0, 40), (0, 40));
        assert_eq!(scroll_notches(40, 40), (0, 80));
        assert_eq!(scroll_notches(80, 40), (1, 0));
    }
    #[test]
    fn turning_back_drops_the_leftover() {
        assert_eq!(scroll_notches(80, -40), (0, -40));
    }
    #[test]
    fn hub_cancels() { assert_eq!(pick(0.0, 0.0, 6), None); assert_eq!(pick(HUB_R - 1.0, 0.0, 6), None); }
    #[test]
    fn straight_up_is_first_node() { assert_eq!(pick(0.0, -100.0, 6), Some(0)); assert_eq!(pick(0.0, -100.0, 1), Some(0)); }
    #[test]
    fn right_is_clockwise() { assert_eq!(pick(100.0, 0.0, 4), Some(1)); assert_eq!(pick(0.0, 100.0, 4), Some(2)); assert_eq!(pick(-100.0, 0.0, 4), Some(3)); }
    #[test]
    fn boundary_wraps() { assert_eq!(pick(-5.0, -100.0, 4), Some(0)); }
    #[test]
    fn empty_wheel_picks_nothing() { assert_eq!(pick(100.0, 0.0, 0), None); }
    #[test]
    fn node_zero_on_top() { let (x, y) = node_center(0, 7, 100.0); assert!(x.abs() < 0.01 && (y + 100.0).abs() < 0.01); }
}
