//! Progress cards in the corner of the screen. Never takes focus.
use crate::gfx::*;
use crate::gfx::SUCCESS;
use crate::job::Job;
use crate::ui::{self, glyph, r, Align, Page, Ui, SHADOW};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const CARD_W: f32 = 340.0;
const CARD_H: f32 = 64.0;
const ERROR_H: f32 = 84.0;
const DETAIL_H: f32 = 34.0;
const GAP: f32 = 8.0;
const MAX_CARDS: usize = 4;
const DONE_SECS: f32 = 3.5;
const ERROR_SECS: f32 = 9.0;
/// Opacity while the pointer is over a card, so what is behind it shows through.
const HOVER_OPACITY: f32 = 0.22;
/// How often the window re-asserts that it is topmost.
const TOPMOST_EVERY: std::time::Duration = std::time::Duration::from_millis(400);

static JOBS: Mutex<Vec<Arc<Job>>> = Mutex::new(Vec::new());
static HWND_ADDR: AtomicIsize = AtomicIsize::new(0);

pub fn add(job: Arc<Job>) {
    JOBS.lock().unwrap().push(job);
    let h = HWND_ADDR.load(Ordering::Relaxed);
    if h != 0 { ui::refresh(HWND(h as *mut _)); }
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
    if let Some(n) = j.note.lock().unwrap().as_ref() { return (n.clone(), MUTED); }
    let name = j.last_output.lock().unwrap().as_ref().and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));
    match name { Some(n) => (format!("Saved {n}"), MUTED), None => ("Done".into(), MUTED) }
}

fn prune() {
    JOBS.lock().unwrap().retain(|j| {
        let at = j.finished_at.lock().unwrap();
        let Some(t) = *at else { return true };
        let keep = if j.errors.lock().unwrap().is_empty() { DONE_SECS } else { ERROR_SECS };
        t.elapsed().as_secs_f32() < keep
    });
}

fn snapshot() -> Vec<Arc<Job>> { JOBS.lock().unwrap().iter().take(MAX_CARDS).cloned().collect() }

fn failed(j: &Job) -> bool { j.finished.load(Ordering::Relaxed) && !j.cancelled() && !j.errors.lock().unwrap().is_empty() }

fn card_height(j: &Job) -> f32 {
    if failed(j) { ERROR_H } else if j.detail.lock().unwrap().is_some() { CARD_H + DETAIL_H } else { CARD_H }
}

struct Hud { shown: bool, preview: bool, topped: Option<std::time::Instant> }

impl Page for Hud {
    fn size(&self) -> (f32, f32) {
        let jobs = snapshot();
        let h: f32 = jobs.iter().map(|j| card_height(j)).sum::<f32>() + GAP * jobs.len().saturating_sub(1) as f32;
        (CARD_W, h.max(CARD_H))
    }
    fn card(&self) -> bool { false }
    fn place(&mut self, w: i32, h: i32) -> Option<POINT> {
        let wa = work_area_at(cursor_pos());
        let tuck = (SHADOW * dpi_scale_at(cursor_pos()) * 0.45) as i32;
        Some(POINT { x: wa.right - w + tuck, y: wa.bottom - h + tuck })
    }
    fn message(&mut self, _hwnd: HWND, msg: u32, _wp: WPARAM, _lp: LPARAM) -> Option<LRESULT> {
        (msg == WM_MOUSEACTIVATE).then_some(LRESULT(MA_NOACTIVATE as isize))
    }
    fn frame(&mut self, ui: &mut Ui) {
        if !self.preview { prune(); }
        let jobs = snapshot();
        if !jobs.is_empty() { ui.want_anim = true; }
        // Clicks fall through the cards to whatever is behind, except on the buttons.
        ui.ghost = !self.preview;
        let mut over_card = false;
        let mut y = 0.0;
        for j in &jobs {
            let finished = j.finished.load(Ordering::Relaxed);
            let is_failed = failed(j);
            let h = card_height(j);
            let q = r(0.0, y, CARD_W, h);
            y += h + GAP;
            ui.card(q, 14.0);

            let key = Arc::as_ptr(j) as usize as u64;
            let hovered = q.has(ui.mouse);
            over_card |= hovered;
            let x_q = r(q.right() - 36.0, q.y + 8.0, 26.0, 26.0);
            let output = if finished { j.last_output.lock().unwrap().clone() } else { None };
            if hovered && ui.icon_button(&format!("hud.x{key}"), glyph::CLOSE, x_q, 9.0, false) {
                if finished { remove(j); } else { j.cancel.store(true, Ordering::Relaxed); }
            }
            if let Some(p) = &output {
                let show_q = r(x_q.x - 30.0, x_q.y, 26.0, 26.0);
                if hovered && ui.icon_button(&format!("hud.show{key}"), glyph::FOLDER, show_q, 11.0, false) {
                    crate::actions::reveal(p);
                    remove(j);
                }
            }

            // Progress ring, or the outcome once it is finished.
            let (cx, cy) = (q.x + 32.0, q.y + CARD_H / 2.0);
            let cancelled = finished && j.cancelled();
            if !finished {
                let f = ui.anim(key ^ 0xF0, j.progress(), 10.0);
                ui.ring(cx, cy, 13.0, TEXT, 0.12, 3.0);
                if f > 0.005 { ui.arc(cx, cy, 13.0, 0.0, f * std::f32::consts::TAU, ACCENT, 1.0, 3.0); }
            } else {
                let (g, c) = if is_failed { (glyph::WARNING, DANGER) } else if cancelled { (glyph::CLOSE, MUTED) } else { (glyph::CHECK, SUCCESS) };
                ui.dot(cx, cy, 15.0, c, 0.14);
                ui.icon(g, r(cx - 15.0, cy - 15.0, 30.0, 30.0), 13.0, c, 1.0);
            }

            let tx = q.x + 58.0;
            let tw = q.w - 58.0 - if output.is_some() { 70.0 } else { 40.0 };
            let (msg, col) = status(j);
            ui.text(&j.title, r(tx, q.y + 11.0, tw, 20.0), 13.0, 600, TEXT, Align::Left);
            if is_failed {
                ui.clip(r(tx, q.y + 31.0, q.w - 58.0 - 14.0, 44.0));
                ui.wrapped(&msg, r(tx, q.y + 31.0, q.w - 58.0 - 14.0, 44.0), 11.5, 400, col);
                ui.unclip();
            } else {
                ui.text(&msg, r(tx, q.y + 31.0, q.w - 58.0 - 14.0, 18.0), 11.5, 400, col, Align::Left);
                if let Some(d) = j.detail.lock().unwrap().as_ref() {
                    ui.wrapped(d, r(tx, q.y + 52.0, q.w - 58.0 - 14.0, DETAIL_H), 11.0, 400, MUTED);
                }
            }
        }
        // Fade while the pointer rests on a card, but not on a button, so the button stays readable.
        let target = if over_card && !ui.hot && !ui.down { HOVER_OPACITY } else { 1.0 };
        ui.opacity = ui.anim(0x0FAC_17, target, 16.0);
    }
    fn after(&mut self) -> bool {
        let want = !JOBS.lock().unwrap().is_empty();
        let h = HWND(HWND_ADDR.load(Ordering::Relaxed) as *mut _);
        if h.is_invalid() { return false; }
        if want != self.shown {
            self.shown = want;
            self.topped = None;
            unsafe { let _ = ShowWindow(h, if want { SW_SHOWNOACTIVATE } else { SW_HIDE }); }
        }
        // WS_EX_TOPMOST alone loses to windows that push themselves forward (Explorer, other
        // topmost windows), so keep putting the cards back on top while they are showing.
        if want && self.topped.is_none_or(|t| t.elapsed() >= TOPMOST_EVERY) {
            self.topped = Some(std::time::Instant::now());
            unsafe { let _ = SetWindowPos(h, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER); }
        }
        false
    }
}

pub fn init() -> Result<()> {
    let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
    let hwnd = ui::open(Hud { shown: false, preview: false, topped: None }, "Tau progress", POINT::default(), ex).ok_or_else(|| Error::new(E_FAIL, "Could not create the progress window"))?;
    unsafe { let _ = ShowWindow(hwnd, SW_HIDE); }
    HWND_ADDR.store(hwnd.0 as isize, Ordering::Relaxed);
    Ok(())
}

/// The cards as a PNG with sample jobs, for design review.
pub fn render(out: &std::path::Path) -> std::result::Result<(), String> {
    let running = Arc::new(Job { title: "Converting to MP4".into(), total: 4, done: 1.into(), frac: Mutex::new(0.55), detail: Mutex::new(Some("The source is lossy, so re-encoding loses a little. Bitrate is matched to the source.".into())), ..Default::default() });
    let saved = Arc::new(Job { title: "Compressing".into(), total: 1, done: 1.into(), finished: true.into(), last_output: Mutex::new(Some("C:\\holiday compressed.jpg".into())), ..Default::default() });
    let bad = Arc::new(Job { title: "Rendering video".into(), total: 1, done: 1.into(), finished: true.into(), errors: Mutex::new(vec!["ffmpeg stopped: the input has no audio stream, so there is nothing to draw a waveform from.".into()]), ..Default::default() });
    { let mut j = JOBS.lock().unwrap(); j.push(running); j.push(saved); j.push(bad); }
    let res = ui::render_png(&mut Hud { shown: true, preview: true, topped: None }, 1.0, Some((200.0, 100.0)), out);
    JOBS.lock().unwrap().clear();
    res
}
