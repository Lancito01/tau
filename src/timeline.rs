//! Picks points or ranges in time over a filmstrip or waveform. Shared by trim, split, snapshot and bleep.
use crate::form;
use crate::gfx::*;
use crate::image::Bmp;
use crate::ui::{self, glyph, id, r, Align, Page, Style, Ui, FOOT_H, PAD, R, TITLE_H};
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
const HEIGHT: f32 = 376.0;
const STRIP_H: f32 = 110.0;
const STRIP_Y: f32 = TITLE_H + 34.0;
const GRAB_PX: f64 = 8.0;

pub fn fmt_time(t: f64) -> String {
    let t = t.max(0.0);
    let (h, m, s) = ((t / 3600.0) as u32, ((t % 3600.0) / 60.0) as u32, t % 60.0);
    if h > 0 { format!("{h}:{m:02}:{s:05.2}") } else { format!("{m}:{s:05.2}") }
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

struct Timeline {
    title: String,
    sub: String,
    mode: Mode,
    strip: Option<Bmp>,
    dur: f64,
    marks: Vec<f64>,
    undo: Vec<Vec<f64>>,
    drag: Option<usize>,
    ok: String,
    result: Option<bool>,
}

impl Timeline {
    fn new(title: &str, sub: &str, strip: Option<Bmp>, dur: f64, mode: Mode, ok: &str) -> Timeline {
        let marks = if mode == Mode::Range { vec![0.0, dur] } else { vec![] };
        Timeline { title: title.into(), sub: sub.into(), mode, strip, dur, marks, undo: vec![], drag: None, ok: ok.into(), result: None }
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
        let total = fmt_time(self.dur);
        match self.mode {
            Mode::Range => {
                let (a, b) = (self.marks[0].min(self.marks[1]), self.marks[0].max(self.marks[1]));
                format!("Keep {} of {total}", fmt_time(b - a))
            }
            Mode::Points => match self.marks.len() { 0 => total, 1 => format!("1 marker  ·  {total}"), n => format!("{n} markers  ·  {total}") },
            Mode::Ranges => {
                let s: f64 = spans(self.mode, &self.marks).iter().map(|(a, b)| b - a).sum();
                format!("{} marked of {total}", fmt_time(s))
            }
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
        let at = (((ui.mouse.0 - q.x) as f64) * per_px).clamp(0.0, dur);

        if hover && ui.pressed {
            self.undo.push(self.marks.clone());
            let hit = nearest(&self.marks, at, per_px * GRAB_PX);
            self.drag = match (hit, self.mode) {
                (Some(i), _) => Some(i),
                (None, Mode::Range) => nearest(&self.marks, at, f64::MAX),
                (None, _) => { self.marks.push(at); Some(self.marks.len() - 1) }
            };
            if let Some(i) = self.drag { self.marks[i] = at; }
        }
        if held { if let Some(i) = self.drag { self.marks[i] = at; } }
        if ui.released && self.drag.take().is_some() && self.undo.last() == Some(&self.marks) { self.undo.pop(); }
        if hover && ui.rpressed {
            let before = self.marks.clone();
            if remove_at(self.mode, &mut self.marks, at, per_px * GRAB_PX * 2.0) { self.undo.push(before); }
        }
        let on_mark = nearest(&self.marks, at, per_px * GRAB_PX).is_some();
        if hover { ui.cursor = if on_mark || self.drag.is_some() { IDC_SIZEWE } else { IDC_ARROW }; }

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
        if hover { ui.tooltip = Some((fmt_time(at), (ui.mouse.0.clamp(q.x, q.right()), q.y - 6.0))); }

        for (i, m) in self.marks.clone().iter().enumerate() {
            let x = xof(*m);
            let active = self.drag == Some(i);
            match self.mode {
                Mode::Points => {
                    ui.fill(r(x - 1.0, q.y - 4.0, 2.0, q.h + 8.0), 0.0, ACCENT, 1.0);
                    ui.dot(x, q.y - 7.0, 5.5, ACCENT, 1.0);
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
            ui.text(&fmt_time(*m), r(lx, q.bottom() + 6.0, tw, 16.0), 11.0, 500, if active { TEXT } else { MUTED }, Align::Center);
        }
    }

    /// The marks as a row of chips with their own remove buttons.
    fn chips(&mut self, ui: &mut Ui, y: f32) {
        let mut x = PAD;
        let max_x = WIDTH - PAD - 150.0;
        let mut remove: Option<Vec<f64>> = None;
        let items: Vec<(String, Vec<f64>)> = match self.mode {
            Mode::Range => vec![],
            Mode::Points => { let mut m = self.marks.clone(); m.sort_by(f64::total_cmp); m.iter().map(|t| (fmt_time(*t), vec![*t])).collect() }
            Mode::Ranges => {
                let mut v: Vec<(String, Vec<f64>)> = spans(self.mode, &self.marks).iter().map(|(a, b)| (format!("{} to {}", fmt_time(*a), fmt_time(*b)), vec![*a, *b])).collect();
                if let Some(t) = lone(self.mode, &self.marks) { v.push((format!("{} to ...", fmt_time(t)), vec![t])); }
                v
            }
        };
        let mut hidden = 0;
        for (i, (label, values)) in items.iter().enumerate() {
            let w = ui.measure(label, 12.0, 500) + 44.0;
            if x + w > max_x { hidden = items.len() - i; break; }
            let q = r(x, y, w, 30.0);
            ui.fill(q, 15.0, RAISED, 1.0);
            ui.text(label, r(q.x + 12.0, q.y, w - 36.0, 30.0), 12.0, 500, TEXT, Align::Left);
            if ui.icon_button(&format!("tl.chip{i}"), glyph::CLOSE, r(q.right() - 27.0, q.y + 4.0, 22.0, 22.0), 8.0, false) { remove = Some(values.clone()); }
            x += w + 6.0;
        }
        if hidden > 0 { ui.text(&format!("+{hidden} more"), r(x + 4.0, y, 70.0, 30.0), 12.0, 400, MUTED, Align::Left); }
        if let Some(vals) = remove {
            self.undo.push(self.marks.clone());
            self.marks.retain(|m| !vals.contains(m));
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
        }
    }
    fn step_back(&mut self) { if let Some(m) = self.undo.pop() { self.marks = m; } }
}

impl Page for Timeline {
    fn size(&self) -> (f32, f32) { (WIDTH, HEIGHT) }
    fn done(&self) -> bool { self.result.is_some() }
    fn on_close(&mut self) { self.result = Some(false); }
    fn frame(&mut self, ui: &mut Ui) {
        if ui.titlebar(&self.title.clone(), &self.sub.clone()) { self.result = Some(false); }
        ui.text(self.hint(), r(PAD, TITLE_H, WIDTH / 2.0 + 60.0, 26.0), 12.5, 400, MUTED, Align::Left);
        let read = self.readout();
        ui.text(&read, r(WIDTH / 2.0, TITLE_H, WIDTH / 2.0 - PAD, 26.0), 12.5, 600, TEXT, Align::Right);
        self.strip_area(ui, r(PAD, STRIP_Y, WIDTH - PAD * 2.0, STRIP_H));
        self.chips(ui, STRIP_Y + STRIP_H + 30.0);
        let fy = HEIGHT - FOOT_H;
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

/// Opens the picker and blocks while pumping messages. Returns the sorted marks.
pub fn pick(title: &str, strip: Option<Bmp>, dur: f64, mode: Mode, ok: &str) -> Option<Vec<f64>> {
    let t = ui::run_modal(Timeline::new(title, &form::context(), strip, dur, mode, ok), title, None)?;
    if t.result != Some(true) { return None; }
    let mut m = t.marks;
    m.sort_by(f64::total_cmp);
    Some(m)
}

/// A picker as a PNG with sample marks, for design review.
pub fn render(mode: Mode, strip: Option<Bmp>, marks: Vec<f64>, out: &std::path::Path) -> Result<(), String> {
    let (title, ok) = match mode { Mode::Range => ("Trim", "Trim"), Mode::Points => ("Split", "Split"), Mode::Ranges => ("Bleep", "Bleep") };
    let mut t = Timeline::new(title, "interview take 3.mp4", strip, 192.0, mode, ok);
    t.marks = marks;
    ui::render_png(&mut t, 1.0, Some((430.0, STRIP_Y + 50.0)), out)
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
