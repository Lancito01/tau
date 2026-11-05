//! Progress cards in the corner of the screen. Never takes focus.
use crate::gfx::*;
use crate::gfx::SUCCESS;
use crate::job::Job;
use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

const CARD_W: f32 = 340.0;
const CARD_H: f32 = 70.0;
const GAP: f32 = 8.0;
const MARGIN: f32 = 16.0;
const WM_APP_ADD: u32 = WM_APP + 20;
const DONE_SECS: f32 = 3.5;
const ERROR_SECS: f32 = 9.0;

static JOBS: Mutex<Vec<Arc<Job>>> = Mutex::new(Vec::new());
static HWND_ADDR: AtomicIsize = AtomicIsize::new(0);

struct Hud { gfx: Gfx, surface: Option<Surface>, x: i32, y: i32, scale: f32 }
thread_local! { static H: RefCell<Option<Hud>> = const { RefCell::new(None) }; }

pub fn add(job: Arc<Job>) {
    JOBS.lock().unwrap().push(job);
    let h = HWND_ADDR.load(Ordering::Relaxed);
    if h != 0 { unsafe { let _ = PostMessageW(Some(HWND(h as *mut _)), WM_APP_ADD, WPARAM(0), LPARAM(0)); } }
}

pub fn remove(job: &Arc<Job>) { JOBS.lock().unwrap().retain(|j| !Arc::ptr_eq(j, job)); }

pub fn busy() -> bool { JOBS.lock().unwrap().iter().any(|j| !j.finished.load(Ordering::Relaxed)) }

fn status(j: &Job) -> (String, u32) {
    let done = j.done.load(Ordering::Relaxed);
    let errs = j.errors.lock().unwrap();
    if !j.finished.load(Ordering::Relaxed) {
        if j.cancelled() { return ("Stopping".into(), MUTED); }
        return (format!("{done} of {} saved", j.total), MUTED);
    }
    if j.cancelled() { return ("Cancelled".into(), MUTED); }
    if let Some(e) = errs.first() {
        let ok = j.total.saturating_sub(errs.len());
        let prefix = if j.total > 1 { format!("{ok} of {} saved. ", j.total) } else { String::new() };
        return (format!("{prefix}{e}"), DANGER);
    }
    if let Some(n) = j.note.lock().unwrap().as_ref() { return (n.clone(), SUCCESS); }
    let name = j.last_output.lock().unwrap().as_ref().and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));
    match name { Some(n) => (format!("Saved {n}. Click to show."), SUCCESS), None => ("Done".into(), SUCCESS) }
}

fn paint(hwnd: HWND) {
    H.with(|h| {
        let mut b = h.borrow_mut();
        let Some(hud) = b.as_mut() else { return };
        let jobs = JOBS.lock().unwrap().clone();
        if jobs.is_empty() { unsafe { let _ = ShowWindow(hwnd, SW_HIDE); let _ = KillTimer(Some(hwnd), 1); } return; }
        let p = cursor_pos();
        let wa = work_area_at(p);
        let scale = dpi_scale_at(p);
        let cards = jobs.len().min(4) as f32;
        let w = ((CARD_W + MARGIN * 2.0) * scale) as i32;
        let h_px = ((CARD_H * cards + GAP * (cards - 1.0) + MARGIN * 2.0) * scale) as i32;
        let stale = hud.surface.as_ref().is_none_or(|s| s.w != w || s.h != h_px);
        if stale { hud.surface = Surface::new(&hud.gfx, w, h_px).ok(); }
        hud.scale = scale;
        hud.x = wa.right - w;
        hud.y = wa.bottom - h_px;
        let Some(s) = hud.surface.as_ref() else { return };
        unsafe {
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), hud.x, hud.y, w, h_px, SWP_NOACTIVATE);
            s.rt.BeginDraw();
            s.rt.Clear(Some(&rgba(0, 0.0)));
            s.rt.SetTransform(&windows_numerics::Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: 0.0, M32: 0.0 });
            let rt: ID2D1RenderTarget = s.rt.cast().unwrap();
            let title = hud.gfx.format("Segoe UI", 13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING).unwrap();
            let sub = hud.gfx.format("Segoe UI", 11.5, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING).unwrap();
            let _ = sub.SetTrimming(&DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 }, None);
            let icon = hud.gfx.format("Segoe MDL2 Assets", 10.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER).unwrap();
            let brush = |c: u32, a: f32| rt.CreateSolidColorBrush(&rgba(c, a), None).unwrap();
            for (i, j) in jobs.iter().take(4).enumerate() {
                let y = MARGIN + i as f32 * (CARD_H + GAP);
                let r = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: MARGIN, top: y, right: MARGIN + CARD_W, bottom: y + CARD_H }, radiusX: 12.0, radiusY: 12.0 };
                rt.FillRoundedRectangle(&r, &brush(BG, 0.97));
                rt.DrawRoundedRectangle(&r, &brush(BORDER, 1.0), 1.0, None);
                let (msg, col) = status(j);
                let tx = wide(&j.title);
                rt.DrawText(&tx, &title, &D2D_RECT_F { left: MARGIN + 14.0, top: y + 8.0, right: MARGIN + CARD_W - 34.0, bottom: y + 28.0 }, &brush(TEXT, 1.0), D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
                let mx = wide(&msg);
                rt.DrawText(&mx, &sub, &D2D_RECT_F { left: MARGIN + 14.0, top: y + 28.0, right: MARGIN + CARD_W - 14.0, bottom: y + 46.0 }, &brush(col, 1.0), D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
                let cx = wide("\u{E711}");
                rt.DrawText(&cx, &icon, &D2D_RECT_F { left: MARGIN + CARD_W - 30.0, top: y + 8.0, right: MARGIN + CARD_W - 10.0, bottom: y + 28.0 }, &brush(MUTED, 1.0), D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                let bar_y = y + 54.0;
                let (l, rr) = (MARGIN + 14.0, MARGIN + CARD_W - 14.0);
                let track = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: l, top: bar_y, right: rr, bottom: bar_y + 4.0 }, radiusX: 2.0, radiusY: 2.0 };
                rt.FillRoundedRectangle(&track, &brush(SURFACE, 1.0));
                let f = if j.finished.load(Ordering::Relaxed) { 1.0 } else { j.progress() };
                let fill = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: l, top: bar_y, right: l + (rr - l) * f, bottom: bar_y + 4.0 }, radiusX: 2.0, radiusY: 2.0 };
                rt.FillRoundedRectangle(&fill, &brush(if col == DANGER { DANGER } else { ACCENT }, 1.0));
            }
            let _ = s.rt.EndDraw(None, None);
            s.present(hwnd, hud.x, hud.y);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    });
}

fn prune() {
    JOBS.lock().unwrap().retain(|j| {
        let at = j.finished_at.lock().unwrap();
        let Some(t) = *at else { return true };
        let keep = if j.errors.lock().unwrap().is_empty() { DONE_SECS } else { ERROR_SECS };
        t.elapsed().as_secs_f32() < keep
    });
}

fn click(lp: LPARAM) {
    let (x, y) = ((lp.0 & 0xFFFF) as i16 as f32, ((lp.0 >> 16) & 0xFFFF) as i16 as f32);
    let scale = H.with(|h| h.borrow().as_ref().map(|s| s.scale)).unwrap_or(1.0);
    let (x, y) = (x / scale, y / scale);
    let i = ((y - MARGIN) / (CARD_H + GAP)).floor();
    if i < 0.0 { return; }
    let mut jobs = JOBS.lock().unwrap();
    let Some(j) = jobs.get(i as usize).cloned() else { return };
    let in_card_y = y - MARGIN - i * (CARD_H + GAP);
    if x > MARGIN + CARD_W - 34.0 && in_card_y < 30.0 {
        if j.finished.load(Ordering::Relaxed) { jobs.remove(i as usize); } else { j.cancel.store(true, Ordering::Relaxed); }
    } else if j.finished.load(Ordering::Relaxed) {
        if let Some(p) = j.last_output.lock().unwrap().clone() {
            crate::actions::reveal(&p);
            jobs.remove(i as usize);
        }
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_APP_ADD => { SetTimer(Some(hwnd), 1, 66, None); paint(hwnd); LRESULT(0) }
            WM_TIMER => { prune(); paint(hwnd); LRESULT(0) }
            WM_LBUTTONUP => { click(lp); paint(hwnd); LRESULT(0) }
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

pub fn init() -> Result<()> {
    unsafe {
        let inst = GetModuleHandleW(None)?;
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: inst.into(), lpszClassName: w!("OrreryHud"), hCursor: LoadCursorW(None, IDC_ARROW)?, ..Default::default() };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, w!("OrreryHud"), w!("Orrery progress"), WS_POPUP, 0, 0, 10, 10, None, None, Some(inst.into()), None)?;
        H.with(|h| *h.borrow_mut() = Gfx::new().ok().map(|gfx| Hud { gfx, surface: None, x: 0, y: 0, scale: 1.0 }));
        HWND_ADDR.store(hwnd.0 as isize, Ordering::Relaxed);
        Ok(())
    }
}
