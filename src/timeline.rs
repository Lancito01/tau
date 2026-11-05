//! Picks points or ranges in time over a filmstrip or waveform. Shared by trim, split, snapshot and bleep. Video also
//! gets a viewport showing the exact frame a marker sits on, and its times count frames rather than hundredths.
use crate::form;
use crate::gfx::*;
use crate::image::Bmp;
use crate::ui::{self, glyph, id, r, Align, Page, Style, Ui, FOOT_H, PAD, R, TITLE_H};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    /// Exactly two marks: what to keep.
    Range,
    /// Any number of marks: cut points or snapshots.
    Points,
    /// Marks read as pairs: the ranges to act on.
    Ranges,
}

const WIDTH: f32 = 900.0;
/// Everything except the strip and the viewport: title, hint, labels, chips, footer.
const CHROME: f32 = 240.0;
const STRIP_H: f32 = 110.0;
/// With a viewport the strip only has to show where things are.
const STRIP_H_VIDEO: f32 = 72.0;
const VIEW_GAP: f32 = 22.0;
const VIEW_MAX: f32 = 360.0;
const VIEW_MIN: f32 = 180.0;
const GRAB_PX: f64 = 8.0;
const CACHE_FRAMES: usize = 32;

pub fn fmt_time(t: f64) -> String {
    let t = t.max(0.0);
    let (h, m, s) = ((t / 3600.0) as u32, ((t % 3600.0) / 60.0) as u32, t % 60.0);
    if h > 0 { format!("{h}:{m:02}:{s:05.2}") } else { format!("{m}:{s:05.2}") }
}

/// Whether a rate can be counted in frames. A file reporting nothing, or nonsense, keeps decimals.
fn usable_fps(fps: f64) -> bool { fps > 0.0 && fps <= 240.0 }

/// Minutes, seconds and frames: 1:05:12 is frame 12 of the 65th second, counting up to the frame rate. Without one,
/// as for audio, it is the time in decimals.
pub fn timecode(t: f64, fps: f64) -> String {
    if !usable_fps(fps) { return fmt_time(t); }
    let n = (t.max(0.0) * fps).round();
    // Half a frame in hand, so that a rate like 29.97 still starts each second on frame 0.
    let secs = ((n + 0.5) / fps).floor();
    let ff = (n - (secs * fps - 0.5).ceil()).max(0.0) as u32;
    let secs = secs as u32;
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}:{ff:02}") } else { format!("{m}:{s:02}:{ff:02}") }
}

/// The start of the frame nearest to `t`. Times stay as they are when there are no frames to land on.
pub fn snap(t: f64, fps: f64) -> f64 {
    if !usable_fps(fps) { return t; }
    (t * fps).round() / fps
}

/// "29.97 fps", "30 fps".
fn rate_label(fps: f64) -> String {
    let s = format!("{fps:.2}");
    format!("{} fps", s.trim_end_matches('0').trim_end_matches('.'))
}

/// The mark nearest to x if close enough to grab.
pub fn nearest(marks: &[f64], t: f64, tolerance: f64) -> Option<usize> {
    marks.iter().enumerate()
        .map(|(i, m)| (i, (m - t).abs()))
        .filter(|(_, d)| *d <= tolerance)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// Spans of the strip that are shaded as selected, as start and end times.
pub fn spans(mode: Mode, marks: &[f64]) -> Vec<(f64, f64)> {
    let mut m = marks.to_vec();
    m.sort_by(f64::total_cmp);
    match mode {
        Mode::Points => vec![],
        Mode::Range | Mode::Ranges => m.chunks_exact(2).map(|c| (c[0], c[1])).collect(),
    }
}

/// The mark with no partner yet, when marks read as pairs.
fn lone(mode: Mode, marks: &[f64]) -> Option<f64> {
    if mode != Mode::Ranges || marks.len() % 2 == 0 { return None; }
    let mut m = marks.to_vec();
    m.sort_by(f64::total_cmp);
    m.last().copied()
}

/// Removes one range when the time falls in it or on its edge, otherwise the one mark near it.
fn remove_at(mode: Mode, marks: &mut Vec<f64>, t: f64, tolerance: f64) -> bool {
    if mode == Mode::Range { return false; }
    if mode == Mode::Ranges {
        if let Some((a, b)) = spans(mode, marks).into_iter().find(|(a, b)| t >= a - tolerance && t <= b + tolerance) {
            marks.retain(|m| *m != a && *m != b);
            return true;
        }
    }
    match nearest(marks, t, tolerance) { Some(i) => { marks.remove(i); true } None => false }
}

/// Fetches the picture at a time. Called off the UI thread.
pub type FrameLoader = Arc<dyn Fn(f64) -> Result<Bmp, String> + Send + Sync>;

/// The frame on show, fetched in the background. The newest request wins, so dragging never queues up work.
struct Viewport {
    load: FrameLoader,
    /// The frame on screen, and the time it is from.
    shown: Option<(i64, f64)>,
    /// A frame is being fetched.
    loading: Option<i64>,
    slot: Arc<Mutex<Option<(i64, f64, Result<Bmp, String>)>>>,
    frames: HashMap<i64, Bmp>,
    order: Vec<i64>,
    failed: bool,
}

/// What the viewport is showing and why.
struct Focus { t: f64, label: String }

struct Timeline {
    title: String,
    sub: String,
    mode: Mode,
    strip: Option<Bmp>,
    dur: f64,
    fps: f64,
    marks: Vec<f64>,
    undo: Vec<Vec<f64>>,
    drag: Option<usize>,
    /// The marker the viewport and the frame-step keys act on.
    sel: Option<usize>,
    hover: Option<f64>,
    view: Option<Viewport>,
    view_h: f32,
    ok: String,
    result: Option<bool>,
}

impl Timeline {
    fn new(title: &str, sub: &str, strip: Option<Bmp>, dur: f64, fps: f64, mode: Mode, ok: &str, load: Option<FrameLoader>) -> Timeline {
        let marks = if mode == Mode::Range { vec![0.0, dur] } else { vec![] };
        let view = load.map(|load| Viewport { load, shown: None, loading: None, slot: Arc::new(Mutex::new(None)), frames: HashMap::new(), order: vec![], failed: false });
        // A small screen gets a smaller viewport, not a window that runs off it.
        let at = cursor_pos();
        let wa = work_area_at(at);
        let room = (wa.bottom - wa.top) as f32 / dpi_scale_at(at) - 80.0 - CHROME - STRIP_H_VIDEO - VIEW_GAP;
        Timeline {
            title: title.into(), sub: sub.into(), mode, strip, dur, fps, marks, undo: vec![], drag: None,
            sel: if mode == Mode::Range { Some(0) } else { None }, hover: None, view, view_h: room.clamp(VIEW_MIN, VIEW_MAX),
            ok: ok.into(), result: None,
        }
    }
    fn fmt(&self, t: f64) -> String { timecode(t, self.fps) }
    fn strip_h(&self) -> f32 { if self.view.is_some() { STRIP_H_VIDEO } else { STRIP_H } }
    fn view_space(&self) -> f32 { if self.view.is_some() { self.view_h + VIEW_GAP } else { 0.0 } }
    fn strip_y(&self) -> f32 { TITLE_H + 34.0 + self.view_space() }
    fn height(&self) -> f32 { CHROME + self.strip_h() + self.view_space() }
    fn frame_len(&self) -> f64 { if usable_fps(self.fps) { 1.0 / self.fps } else { 0.0 } }

    /// The last time a picture exists at. The end of a file is just after its last frame.
    fn last_frame(&self) -> f64 { snap((self.dur - self.frame_len()).max(0.0), self.fps) }

    /// Where the pointer means a mark to go, on a frame and inside the file. A trim's end may sit after the last frame,
    /// so that it can keep it. A cut or a snapshot has to land on a frame that exists.
    fn snapped(&self, t: f64) -> f64 {
        let at = snap(t, self.fps).clamp(0.0, self.dur);
        if self.mode == Mode::Points { at.min(self.last_frame()) } else { at }
    }

    fn ready(&self) -> bool {
        match self.mode {
            Mode::Range => (self.marks[0] - self.marks[1]).abs() > 0.01,
            Mode::Points => !self.marks.is_empty(),
            Mode::Ranges => self.marks.len() >= 2,
        }
    }
    fn hint(&self) -> &'static str {
        match self.mode {
            Mode::Range => "Drag the two grips around the part to keep.",
            Mode::Points => "Click to add a marker. Drag to move, right-click to remove.",
            Mode::Ranges if lone(self.mode, &self.marks).is_some() => "Now click where this range ends.",
            Mode::Ranges => "Click where a range starts, then where it ends. Drag to adjust.",
        }
    }
    fn readout(&self) -> String {
        let total = self.fmt(self.dur);
        let total = if usable_fps(self.fps) { format!("{total}  ·  {}", rate_label(self.fps)) } else { total };
        match self.mode {
            Mode::Range => {
                let (a, b) = (self.marks[0].min(self.marks[1]), self.marks[0].max(self.marks[1]));
                format!("Keep {} of {total}", self.fmt(b - a))
            }
            Mode::Points => match self.marks.len() { 0 => total, 1 => format!("1 marker  ·  {total}"), n => format!("{n} markers  ·  {total}") },
            Mode::Ranges => {
                let s: f64 = spans(self.mode, &self.marks).iter().map(|(a, b)| b - a).sum();
                format!("{} marked of {total}", self.fmt(s))
            }
        }
    }

    /// What the viewport should show: the marker being dragged, then wherever the pointer is on the strip, then the
    /// selected marker, then the start.
    fn focus(&self) -> Focus {
        let name = |i: usize| match self.mode {
            Mode::Range => if self.marks[i] <= self.marks[1 - i] { "Start".to_string() } else { "End".to_string() },
            _ => {
                let mut sorted = self.marks.clone();
                sorted.sort_by(f64::total_cmp);
                format!("Marker {}", sorted.iter().position(|m| *m == self.marks[i]).unwrap_or(i) + 1)
            }
        };
        let mark = self.drag.or(self.sel).filter(|i| *i < self.marks.len());
        let (t, label) = match (self.drag, self.hover, mark) {
            (Some(_), _, Some(i)) => (self.marks[i], name(i)),
            (_, Some(t), _) => (t, "Pointer".to_string()),
            (_, _, Some(i)) => (self.marks[i], name(i)),
            _ => (0.0, "First frame".to_string()),
        };
        Focus { t: t.min(self.last_frame()), label }
    }

    fn viewport(&mut self, ui: &mut Ui, q: R) {
        let focus = self.focus();
        let fps = self.fps;
        let index = |t: f64| if usable_fps(fps) { (t * fps).round() as i64 } else { (t * 1000.0) as i64 };
        let want = index(focus.t);
        let Some(v) = self.view.as_mut() else { return };

        if let Some((n, t, res)) = v.slot.lock().unwrap().take() {
            v.loading = None;
            match res {
                Ok(b) => {
                    v.failed = false;
                    if v.order.len() >= CACHE_FRAMES {
                        let old = v.order.remove(0);
                        v.frames.remove(&old);
                        ui.forget_image(id(&format!("tl.view{old}")));
                    }
                    v.frames.insert(n, b);
                    v.order.push(n);
                    v.shown = Some((n, t));
                }
                Err(_) => v.failed = n == want,
            }
        }
        if v.frames.contains_key(&want) {
            let t = if want == v.shown.map(|s| s.0).unwrap_or(i64::MIN) { v.shown.map(|s| s.1).unwrap_or(focus.t) } else { focus.t };
            v.shown = Some((want, t));
            v.failed = false;
        } else if v.loading.is_none() && !v.failed {
            v.loading = Some(want);
            let (load, slot, t) = (v.load.clone(), v.slot.clone(), focus.t);
            std::thread::spawn(move || {
                crate::job::com_init();
                let res = load(t);
                *slot.lock().unwrap() = Some((want, t, res));
            });
        }
        let waiting = v.loading.is_some();
        if waiting { ui.want_anim = true; }

        ui.fill(q, 10.0, 0x0A0B0C, 1.0);
        let shown = v.shown.and_then(|(n, t)| v.frames.get(&n).map(|b| (n, t, b)));
        let mut pic = q;
        if let Some((n, _, b)) = shown {
            let s = (q.w / b.w as f32).min(q.h / b.h as f32);
            let (w, h) = (b.w as f32 * s, b.h as f32 * s);
            pic = r(q.x + (q.w - w) / 2.0, q.y + (q.h - h) / 2.0, w, h);
            ui.image(id(&format!("tl.view{n}")), b, pic);
        } else if !v.failed {
            ui.text("Loading frame", q, 12.5, 400, FAINT, Align::Center);
        }
        if v.failed { ui.text("Could not read this frame", q, 12.5, 400, FAINT, Align::Center); }

        // A picture still on its way must not pass for the one asked for: the label says what is really on screen.
        let on_screen = v.shown.map(|s| s.0) == Some(want);
        let stamp = if on_screen || shown.is_none() { format!("{}  ·  {}", focus.label, timecode(focus.t, fps)) } else { format!("Loading {}", timecode(focus.t, fps)) };
        if !on_screen && shown.is_some() { ui.fill(pic, 0.0, 0, 0.35); }
        let tw = ui.measure(&stamp, 12.0, 600) + 22.0;
        let pill = r(pic.x + (pic.w - tw) / 2.0, pic.bottom() - 34.0, tw, 26.0);
        ui.fill(pill, 13.0, 0, 0.72);
        ui.text(&stamp, pill, 12.0, 600, TEXT, Align::Center);

        // One frame back and forward, either side of the picture.
        let can_step = self.sel.is_some_and(|i| i < self.marks.len()) && usable_fps(fps);
        let by = q.y + q.h / 2.0 - 22.0;
        let back = ui.icon_button_if("tl.back", 0xE76B, r(q.x + 4.0, by, 44.0, 44.0), 15.0, can_step);
        let fwd = ui.icon_button_if("tl.next", 0xE76C, r(q.right() - 48.0, by, 44.0, 44.0), 15.0, can_step);
        if back { self.step_frame(-1.0); }
        if fwd { self.step_frame(1.0); }
    }

    /// Moves the selected marker one frame along.
    fn step_frame(&mut self, by: f64) {
        let Some(i) = self.sel.filter(|i| *i < self.marks.len()) else { return };
        let moved = self.snapped(self.marks[i] + by * self.frame_len());
        if moved != self.marks[i] {
            self.undo.push(self.marks.clone());
            self.marks[i] = moved;
        }
    }

    fn strip_area(&mut self, ui: &mut Ui, q: R) {
        ui.fill(q, 0.0, SURFACE, 1.0);
        match &self.strip {
            Some(b) => ui.image(id("timeline.strip"), b, q),
            None => ui.text("No preview for this file", q, 12.5, 400, FAINT, Align::Center),
        }
        let dur = self.dur.max(0.001);
        let xof = |t: f64| q.x + (t / dur) as f32 * q.w;
        let per_px = dur / q.w as f64;
        let band = r(q.x - 8.0, q.y - 8.0, q.w + 16.0, q.h + 16.0);
        let (hover, held, _) = ui.hit(id("timeline.area"), band);
        let raw = (((ui.mouse.0 - q.x) as f64) * per_px).clamp(0.0, dur);
        let at = self.snapped(raw);

        if hover && ui.pressed {
            self.undo.push(self.marks.clone());
            let hit = nearest(&self.marks, raw, per_px * GRAB_PX);
            self.drag = match (hit, self.mode) {
                (Some(i), _) => Some(i),
                (None, Mode::Range) => nearest(&self.marks, raw, f64::MAX),
                (None, _) => { self.marks.push(at); Some(self.marks.len() - 1) }
            };
            if let Some(i) = self.drag { self.marks[i] = at; self.sel = Some(i); }
        }
        if held { if let Some(i) = self.drag { self.marks[i] = at; } }
        if ui.released && self.drag.take().is_some() && self.undo.last() == Some(&self.marks) { self.undo.pop(); }
        if hover && ui.rpressed {
            let before = self.marks.clone();
            if remove_at(self.mode, &mut self.marks, raw, per_px * GRAB_PX * 2.0) { self.undo.push(before); self.sel = None; }
        }
        let on_mark = nearest(&self.marks, raw, per_px * GRAB_PX).is_some();
        if hover { ui.cursor = if on_mark || self.drag.is_some() { IDC_SIZEWE } else { IDC_ARROW }; }
        self.hover = if hover && self.drag.is_none() && !on_mark { Some(at) } else { None };

        match self.mode {
            Mode::Range => {
                let (a, b) = (self.marks[0].min(self.marks[1]), self.marks[0].max(self.marks[1]));
                if a > 0.0 { ui.fill(r(q.x, q.y, xof(a) - q.x, q.h), 0.0, 0, 0.62); }
                if b < dur { ui.fill(r(xof(b), q.y, q.right() - xof(b), q.h), 0.0, 0, 0.62); }
                ui.fill(r(xof(a), q.y - 3.0, xof(b) - xof(a), 3.0), 0.0, ACCENT, 1.0);
                ui.fill(r(xof(a), q.bottom(), xof(b) - xof(a), 3.0), 0.0, ACCENT, 1.0);
            }
            Mode::Ranges => {
                for (a, b) in spans(self.mode, &self.marks) {
                    ui.fill(r(xof(a), q.y, xof(b) - xof(a), q.h), 0.0, ACCENT, 0.22);
                    ui.fill(r(xof(a), q.y - 3.0, xof(b) - xof(a), 3.0), 0.0, ACCENT, 1.0);
                    ui.fill(r(xof(a), q.bottom(), xof(b) - xof(a), 3.0), 0.0, ACCENT, 1.0);
                }
            }
            Mode::Points => {}
        }
        ui.round_corners(q, 10.0, BG);

        if hover && self.drag.is_none() && !on_mark {
            ui.fill(r(ui.mouse.0.clamp(q.x, q.right()) - 0.5, q.y, 1.0, q.h), 0.0, TEXT, 0.6);
        }
        if hover { ui.tooltip = Some((self.fmt(at), (ui.mouse.0.clamp(q.x, q.right()), q.y - 6.0))); }

        for (i, m) in self.marks.clone().iter().enumerate() {
            let x = xof(*m);
            let active = self.drag == Some(i) || (self.view.is_some() && self.sel == Some(i));
            match self.mode {
                Mode::Points => {
                    ui.fill(r(x - 1.0, q.y - 4.0, 2.0, q.h + 8.0), 0.0, if active { TEXT } else { ACCENT }, 1.0);
                    ui.dot(x, q.y - 7.0, 5.5, if active { TEXT } else { ACCENT }, 1.0);
                }
                _ => {
                    let grip = r(x - 4.0, q.y + q.h / 2.0 - 19.0, 8.0, 38.0);
                    ui.fill(r(x - 1.0, q.y - 3.0, 2.0, q.h + 6.0), 0.0, ACCENT, 1.0);
                    ui.fill(grip, 4.0, if active { TEXT } else { ACCENT }, 1.0);
                    ui.fill(r(x - 0.75, grip.y + 11.0, 1.5, 16.0), 0.5, ON_ACCENT, 0.7);
                }
            }
            let tw = 64.0;
            let lx = (x - tw / 2.0).clamp(q.x, q.right() - tw);
            ui.text(&self.fmt(*m), r(lx, q.bottom() + 6.0, tw, 16.0), 11.0, 500, if active { TEXT } else { MUTED }, Align::Center);
        }
    }

    /// The marks as a row of chips with their own remove buttons. Clicking one shows it in the viewport.
    fn chips(&mut self, ui: &mut Ui, y: f32) {
        let mut x = PAD;
        let max_x = WIDTH - PAD - 150.0;
        let mut remove: Option<Vec<f64>> = None;
        let items: Vec<(String, Vec<f64>)> = match self.mode {
            Mode::Range => vec![],
            Mode::Points => { let mut m = self.marks.clone(); m.sort_by(f64::total_cmp); m.iter().map(|t| (self.fmt(*t), vec![*t])).collect() }
            Mode::Ranges => {
                let mut v: Vec<(String, Vec<f64>)> = spans(self.mode, &self.marks).iter().map(|(a, b)| (format!("{} to {}", self.fmt(*a), self.fmt(*b)), vec![*a, *b])).collect();
                if let Some(t) = lone(self.mode, &self.marks) { v.push((format!("{} to ...", self.fmt(t)), vec![t])); }
                v
            }
        };
        let selected_time = self.sel.and_then(|i| self.marks.get(i)).copied();
        let mut hidden = 0;
        let mut pick = None;
        for (i, (label, values)) in items.iter().enumerate() {
            let w = ui.measure(label, 12.0, 500) + 44.0;
            if x + w > max_x { hidden = items.len() - i; break; }
            let q = r(x, y, w, 30.0);
            let close = r(q.right() - 27.0, q.y + 4.0, 22.0, 22.0);
            // The remove button comes first, so a press on it is not also a press on the chip.
            if ui.icon_button(&format!("tl.chip{i}"), glyph::CLOSE, close, 8.0, false) { remove = Some(values.clone()); }
            let (hov, _, clicked) = ui.hit(id(&format!("tl.pick{i}")), q);
            if clicked && self.view.is_some() { pick = values.first().copied(); }
            ui.fill(q, 15.0, RAISED, 1.0);
            if self.view.is_some() && hov { ui.fill(q, 15.0, TEXT, 0.05); }
            if self.view.is_some() && selected_time.is_some_and(|t| values.contains(&t)) { ui.stroke(q, 15.0, ACCENT, 0.8, 1.0); }
            ui.text(label, r(q.x + 12.0, q.y, w - 36.0, 30.0), 12.0, 500, TEXT, Align::Left);
            ui.icon(glyph::CLOSE, close, 8.0, MUTED, 1.0);
            x += w + 6.0;
        }
        if hidden > 0 { ui.text(&format!("+{hidden} more"), r(x + 4.0, y, 70.0, 30.0), 12.0, 400, MUTED, Align::Left); }
        if let Some(t) = pick { self.sel = self.marks.iter().position(|m| *m == t); }
        if let Some(vals) = remove {
            self.undo.push(self.marks.clone());
            self.marks.retain(|m| !vals.contains(m));
            self.sel = None;
        }

        // Reset and undo, on the right of the same row.
        let can_undo = !self.undo.is_empty();
        if ui.icon_button_if("tl.undo", glyph::UNDO, r(WIDTH - PAD - 30.0, y, 30.0, 30.0), 13.0, can_undo) { self.step_back(); }
        if self.mode == Mode::Range {
            let full = self.marks.iter().cloned().fold(f64::MAX, f64::min) <= 0.0 && self.marks.iter().cloned().fold(0.0, f64::max) >= self.dur;
            if ui.button("tl.reset", "Reset", r(WIDTH - PAD - 106.0, y, 70.0, 30.0), Style::Ghost, !full) {
                self.undo.push(self.marks.clone());
                self.marks = vec![0.0, self.dur];
            }
        } else if ui.button("tl.clear", "Clear all", r(WIDTH - PAD - 116.0, y, 80.0, 30.0), Style::Ghost, !self.marks.is_empty()) {
            self.undo.push(self.marks.clone());
            self.marks.clear();
            self.sel = None;
        }
    }
    fn step_back(&mut self) {
        if let Some(m) = self.undo.pop() {
            self.marks = m;
            if self.sel.is_some_and(|i| i >= self.marks.len()) { self.sel = None; }
        }
    }
}

impl Page for Timeline {
    fn size(&self) -> (f32, f32) { (WIDTH, self.height()) }
    fn done(&self) -> bool { self.result.is_some() }
    fn on_close(&mut self) { self.result = Some(false); }
    fn frame(&mut self, ui: &mut Ui) {
        if ui.titlebar(&self.title.clone(), &self.sub.clone()) { self.result = Some(false); }
        ui.text(self.hint(), r(PAD, TITLE_H, WIDTH / 2.0 + 60.0, 26.0), 12.5, 400, MUTED, Align::Left);
        let read = self.readout();
        ui.text(&read, r(WIDTH / 2.0 - 60.0, TITLE_H, WIDTH / 2.0 + 60.0 - PAD, 26.0), 12.5, 600, TEXT, Align::Right);
        let (strip_y, strip_h, height) = (self.strip_y(), self.strip_h(), self.height());
        // The strip first, as the viewport follows what the pointer is doing on it.
        self.strip_area(ui, r(PAD, strip_y, WIDTH - PAD * 2.0, strip_h));
        if self.view.is_some() {
            self.viewport(ui, r(PAD, TITLE_H + 30.0, WIDTH - PAD * 2.0, self.view_h));
            if ui.key(VK_LEFT) { self.step_frame(-1.0); }
            if ui.key(VK_RIGHT) { self.step_frame(1.0); }
        }
        self.chips(ui, strip_y + strip_h + 30.0);
        let fy = height - FOOT_H;
        ui.divider(fy, 0.0, WIDTH);
        let by = fy + (FOOT_H - 38.0) / 2.0;
        let primary = r(WIDTH - PAD - 120.0, by, 120.0, 38.0);
        let ready = self.ready();
        if ui.button("tl.cancel", "Cancel", r(primary.x - 96.0, by, 88.0, 38.0), Style::Ghost, true) { self.result = Some(false); }
        if ui.button("tl.ok", &self.ok.clone(), primary, Style::Primary, ready) { self.result = Some(true); }
        if ui.key(VK_ESCAPE) { self.result = Some(false); }
        if ui.key(VK_RETURN) && ready { self.result = Some(true); }
        if ui.ctrl_key(VK_Z).is_some() { self.step_back(); }
    }
}

/// Opens the picker and blocks while pumping messages. Returns the sorted marks. `fps` of 0 means times stay in decimals,
/// and a `load` adds the viewport.
pub fn pick(title: &str, strip: Option<Bmp>, dur: f64, fps: f64, mode: Mode, ok: &str, load: Option<FrameLoader>) -> Option<Vec<f64>> {
    let t = ui::run_modal(Timeline::new(title, &form::context(), strip, dur, fps, mode, ok, load), title, None)?;
    if t.result != Some(true) { return None; }
    let mut m = t.marks;
    m.sort_by(f64::total_cmp);
    Some(m)
}

/// A picker as a PNG with sample marks, for design review. A frame adds the viewport, already showing it.
pub fn render(mode: Mode, strip: Option<Bmp>, marks: Vec<f64>, fps: f64, frame: Option<Bmp>, out: &std::path::Path) -> Result<(), String> {
    let (title, ok) = match mode { Mode::Range => ("Trim", "Trim"), Mode::Points => ("Split", "Split"), Mode::Ranges => ("Bleep", "Bleep") };
    let load: Option<FrameLoader> = frame.as_ref().map(|_| -> FrameLoader { Arc::new(|_| Err(String::new())) });
    let mut t = Timeline::new(title, "interview take 3.mp4", strip, 192.0, fps, mode, ok, load);
    t.marks = marks;
    if !t.marks.is_empty() { t.sel = Some(0); }
    t.view_h = VIEW_MAX;
    if let (Some(f), Some(v)) = (frame, t.view.as_mut()) {
        let n = t.marks.first().map(|m| (m * fps).round() as i64).unwrap_or(0);
        v.frames.insert(n, f);
        v.order.push(n);
        v.shown = Some((n, n as f64 / fps.max(1.0)));
    }
    ui::render_png(&mut t, 1.0, None, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn time_format() {
        assert_eq!(fmt_time(5.5), "0:05.50");
        assert_eq!(fmt_time(65.0), "1:05.00");
        assert_eq!(fmt_time(3725.0), "1:02:05.00");
    }
    #[test]
    fn timecode_counts_frames_up_to_the_rate() {
        assert_eq!(timecode(0.0, 30.0), "0:00:00");
        assert_eq!(timecode(1.5, 30.0), "0:01:15");
        assert_eq!(timecode(65.0 + 12.0 / 24.0, 24.0), "1:05:12");
        assert_eq!(timecode(59.0 / 60.0, 60.0), "0:00:59");
        assert_eq!(timecode(1.0, 60.0), "0:01:00");
        assert_eq!(timecode(3725.5, 30.0), "1:02:05:15");
        assert_eq!(timecode(5.5, 0.0), "0:05.50", "audio has no frames to count");
    }
    #[test]
    fn timecode_never_shows_a_frame_past_the_rate() {
        for fps in [24.0f64, 25.0, 30.0, 60.0, 24000.0 / 1001.0, 30000.0 / 1001.0, 60000.0 / 1001.0] {
            let top = fps.ceil() as u32;
            let mut last = String::new();
            for n in 0..(fps as u32 * 130) {
                let t = n as f64 / fps;
                let s = timecode(t, fps);
                let ff: u32 = s.rsplit(':').next().unwrap().parse().unwrap();
                assert!(ff < top, "{s} at {fps}");
                assert_ne!(s, last, "every frame has its own label at {fps}");
                last = s;
            }
        }
    }
    #[test]
    fn marks_land_on_frames() {
        assert_eq!(snap(1.01, 30.0), 1.0);
        assert_eq!(snap(1.02, 30.0), 31.0 / 30.0);
        assert_eq!(snap(1.2345, 0.0), 1.2345);
    }
    #[test]
    fn rate_reads_cleanly() {
        assert_eq!(rate_label(30.0), "30 fps");
        assert_eq!(rate_label(30000.0 / 1001.0), "29.97 fps");
    }
    #[test]
    fn grabs_only_within_tolerance() {
        assert_eq!(nearest(&[1.0, 5.0], 1.1, 0.5), Some(0));
        assert_eq!(nearest(&[1.0, 5.0], 3.0, 0.5), None);
    }
    #[test]
    fn range_pairs_sort_and_ignore_a_lone_mark() {
        assert_eq!(spans(Mode::Ranges, &[5.0, 1.0, 9.0]), vec![(1.0, 5.0)]);
        assert!(spans(Mode::Points, &[1.0, 2.0]).is_empty());
    }
    #[test]
    fn right_click_removes_a_whole_range() {
        let mut m = vec![1.0, 5.0, 8.0, 9.0];
        assert!(remove_at(Mode::Ranges, &mut m, 3.0, 0.1));
        assert_eq!(m, vec![8.0, 9.0]);
    }
    #[test]
    fn right_click_removes_one_marker() {
        let mut m = vec![1.0, 5.0];
        assert!(remove_at(Mode::Points, &mut m, 5.05, 0.1));
        assert_eq!(m, vec![1.0]);
        assert!(!remove_at(Mode::Points, &mut m, 5.05, 0.1));
    }
    #[test]
    fn trim_keeps_both_ends() {
        let mut m = vec![1.0, 5.0];
        assert!(!remove_at(Mode::Range, &mut m, 1.0, 0.1));
        assert_eq!(m.len(), 2);
    }
    #[test]
    fn lone_mark_is_the_last_when_pairs_are_odd() {
        assert_eq!(lone(Mode::Ranges, &[5.0, 1.0, 9.0]), Some(9.0));
        assert_eq!(lone(Mode::Ranges, &[5.0, 1.0]), None);
        assert_eq!(lone(Mode::Points, &[1.0]), None);
    }
}
