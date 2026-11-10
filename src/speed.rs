//! Change speed window: the video plays at the speed chosen, so a speed can be judged by watching it. Custom speeds
//! sit on a logarithmic slider, so halving and doubling are the same distance apart and slow speeds get as fine a
//! control as fast ones.
use crate::form;
use crate::gfx::*;
use crate::image::Bmp;
use crate::player::{self, Player};
use crate::timeline::{fmt_time, snap, timecode};
use crate::ui::{self, glyph, id, r, Align, Page, Style, Ui, FOOT_H, PAD, R, TITLE_H};
use std::sync::{Arc, Mutex};
use windows::Win32::UI::Input::KeyboardAndMouse::*;

const SIDE: f32 = 340.0;
const SCRUB_H: f32 = 56.0;
pub const PRESETS: [f64; 7] = [0.25, 0.5, 0.75, 1.25, 1.5, 2.0, 4.0];
const DEFAULT: usize = 5;
/// The Custom slider's ends, as powers of two in hundredths: 0.1x to 10x. A step is 0.7% either way.
const LOG_MIN: f32 = -333.0;
const LOG_MAX: f32 = 333.0;

/// Fetches the picture at a time. Called off the UI thread.
pub type Loader = Arc<dyn Fn(f64) -> Result<Bmp, String> + Send + Sync>;

/// "2x", "0.75x", "1.23x".
pub fn label(f: f64) -> String {
    let s = format!("{f:.2}");
    format!("{}x", s.trim_end_matches('0').trim_end_matches('.'))
}

/// A slider step as a speed, to three significant figures so it reads cleanly.
fn from_step(v: f32) -> f64 {
    let f = 2f64.powf(v as f64 / 100.0);
    let digits = 2 - f.log10().floor() as i32;
    let m = 10f64.powi(digits);
    ((f * m).round() / m).clamp(0.1, 10.0)
}

fn to_step(f: f64) -> f32 { ((f.max(0.001).log2() * 100.0).round() as f32).clamp(LOG_MIN, LOG_MAX) }

struct Window {
    sub: String,
    dur: f64,
    fps: f64,
    play: player::Source,
    load: Loader,
    sel: usize,
    /// The Custom slider, in hundredths of a doubling.
    step: f32,
    pos: f32,
    /// The picture on show: a still while paused, the playing frame while playing.
    frame: Bmp,
    frame_gen: u64,
    /// The slider position the still on show is from, and the one being fetched.
    still_at: f32,
    fetching: Option<f32>,
    fetched: Arc<Mutex<Option<(f32, Result<Bmp, String>)>>>,
    player: Option<Player>,
    /// The speed the player was started at.
    playing_at: f64,
    /// Playing was paused by dragging the slider, and goes on once it is let go.
    resume: bool,
    start: (f32, f32),
    result: Option<bool>,
}

impl Window {
    fn speed(&self) -> f64 { PRESETS.get(self.sel).copied().unwrap_or_else(|| from_step(self.step)) }
    fn is_custom(&self) -> bool { self.sel >= PRESETS.len() }
    fn time(&self) -> f64 {
        let frame = if self.fps > 0.0 { 1.0 / self.fps } else { 0.04 };
        snap(self.pos as f64 / 1000.0 * self.dur, self.fps).min((self.dur - frame * 2.0).max(0.0))
    }
    fn pos_of(&self, t: f64) -> f32 { (t / self.dur.max(0.001) * 1000.0).clamp(0.0, 1000.0) as f32 }

    fn show(&mut self, ui: &mut Ui, b: Bmp) {
        ui.forget_image(id(&format!("sp.frame{}", self.frame_gen)));
        self.frame_gen += 1;
        self.frame = b;
    }

    fn toggle_play(&mut self) {
        if self.player.take().is_some() { return; }
        let from = player::start_from(self.time(), 0.0, self.dur, self.fps);
        self.pos = self.pos_of(from);
        self.start_player(from);
    }

    fn start_player(&mut self, from: f64) {
        let speed = self.speed();
        self.player = Some(Player::start_at_speed(&self.play, from, self.dur, (self.frame.w, self.frame.h), speed));
        self.playing_at = speed;
    }

    /// Playing frames, or the still at the slider while paused.
    fn update_picture(&mut self, ui: &mut Ui) {
        if let Some(pl) = self.player.as_mut() {
            let next = pl.tick();
            let (t, ended) = (pl.time(), pl.ended());
            if let Some(b) = next { self.show(ui, b); }
            self.pos = self.pos_of(t);
            self.still_at = self.pos;
            ui.want_anim = true;
            if ended { self.player = None; }
            // A new speed takes over where playing has got to, once the slider is let go.
            else if self.speed() != self.playing_at && !ui.down { self.start_player(t); }
            return;
        }
        let done = self.fetched.lock().unwrap().take();
        if let Some((at, res)) = done {
            self.fetching = None;
            if let Ok(b) = res { self.show(ui, b); }
            self.still_at = at;
        }
        if self.still_at != self.pos && self.fetching.is_none() {
            // Only the newest position is fetched, so dragging never queues up work.
            let (load, slot, at, t) = (self.load.clone(), self.fetched.clone(), self.pos, self.time());
            self.fetching = Some(at);
            std::thread::spawn(move || {
                crate::job::com_init();
                let res = load(t);
                *slot.lock().unwrap() = Some((at, res));
            });
        }
        if self.fetching.is_some() { ui.want_anim = true; }
    }

    fn stage(&mut self, ui: &mut Ui, q: R) {
        ui.fill(q, 0.0, 0x050506, 1.0);
        let view = q.inset(10.0);
        ui.fill(view, 8.0, 0x0A0B0C, 1.0);
        let inner = view.inset(1.0);
        let b = &self.frame;
        let s = (inner.w / b.w as f32).min(inner.h / b.h as f32);
        let (w, h) = (b.w as f32 * s, b.h as f32 * s);
        ui.image(id(&format!("sp.frame{}", self.frame_gen)), b, r(inner.x + (inner.w - w) / 2.0, inner.y + (inner.h - h) / 2.0, w, h));
        let chip = if self.player.is_some() { format!("Playing at {}, without sound", label(self.playing_at)) } else { "Press Space to play at this speed".to_string() };
        let cw = ui.measure(&chip, 12.0, 600) + 18.0;
        let c = r(view.x + 8.0, view.y + 8.0, cw, 24.0);
        ui.fill(c, 6.0, 0, 0.62);
        ui.text(&chip, c, 12.0, 600, TEXT, Align::Center);
    }

    fn scrubber(&mut self, ui: &mut Ui, q: R) {
        ui.fill(q, 0.0, SURFACE, 0.6);
        ui.fill(r(q.x, q.y, q.w, 1.0), 0.0, BORDER, 0.7);
        let g = if self.player.is_some() || self.resume { glyph::PAUSE } else { glyph::PLAY };
        if ui.icon_button("sp.play", g, r(q.x + PAD - 6.0, q.y + (q.h - 36.0) / 2.0, 36.0, 36.0), 13.0, false) {
            if self.resume { self.resume = false; } else { self.toggle_play(); }
        }
        let bw = 42.0;
        let before = self.pos;
        ui.slider("sp.scrub", r(q.x + PAD - 9.0 + bw, q.y + 14.0, q.w - PAD * 2.0 - 132.0 + 18.0 - bw, 28.0), 0.0, 1000.0, 0.0, &mut self.pos);
        // Dragging while playing holds it at the slider, and it plays on from wherever it is let go.
        if self.pos != before && self.player.take().is_some() { self.resume = true; }
        if self.resume && !ui.down { self.resume = false; let from = player::start_from(self.time(), 0.0, self.dur, self.fps); self.start_player(from); }
        let label = format!("{} / {}", timecode(self.time(), self.fps), timecode(self.dur, self.fps));
        ui.text(&label, r(q.right() - PAD - 124.0, q.y, 124.0, q.h), 12.0, 500, MUTED, Align::Right);
    }

    fn sidebar(&mut self, ui: &mut Ui, q: R) {
        ui.fill(q, 0.0, SURFACE, 0.6);
        ui.fill(r(q.x, q.y, 1.0, q.h), 0.0, BORDER, 0.7);
        let (x, w) = (q.x + PAD, q.w - PAD * 2.0);
        let mut y = q.y + 14.0;
        ui.text("Speed", r(x, y, w, 18.0), 12.5, 500, MUTED, Align::Left);
        y += 22.0;
        let rows: Vec<String> = PRESETS.iter().map(|f| label(*f)).chain(["Custom".to_string()]).collect();
        let before = self.sel;
        ui.select("sp.level", r(x, y, w, 38.0), &rows, &mut self.sel, true);
        // Custom starts from the speed that was showing.
        if self.sel != before && self.is_custom() { self.step = to_step(PRESETS.get(before).copied().unwrap_or(1.0)); }
        y += 50.0;
        if self.is_custom() {
            ui.text("Custom speed", r(x, y, w, 18.0), 12.5, 500, MUTED, Align::Left);
            ui.text(&label(self.speed()), r(x, y, w, 18.0), 12.5, 600, TEXT, Align::Right);
            ui.slider("sp.custom", r(x - 9.0, y + 18.0, w + 18.0, 28.0), LOG_MIN, LOG_MAX, 0.0, &mut self.step);
            let ends = r(x, y + 46.0, w, 16.0);
            ui.text("0.1x", ends, 11.0, 500, FAINT, Align::Left);
            ui.text("1x", ends, 11.0, 500, FAINT, Align::Center);
            ui.text("10x", ends, 11.0, 500, FAINT, Align::Right);
            y += 70.0;
            let tip = "Each step is the same share of the speed, so slow speeds are as fine to set as fast ones. Arrow keys nudge it, double-click returns to 1x.";
            let h = ui.wrapped_height(tip, 12.0, 400, w);
            ui.wrapped(tip, r(x, y, w, h), 12.0, 400, MUTED);
            y += h + 14.0;
        }
        let f = self.speed();
        let detail = format!("{} long now, {} at {}. Every frame is kept, and the sound keeps its pitch.", fmt_time(self.dur), fmt_time(self.dur / f), label(f));
        let h = ui.wrapped_height(&detail, 12.0, 400, w);
        ui.wrapped(&detail, r(x, y, w, h), 12.0, 400, MUTED);
    }
}

impl Page for Window {
    fn size(&self) -> (f32, f32) { self.start }
    fn resizable(&self) -> bool { true }
    fn done(&self) -> bool { self.result.is_some() }
    fn on_close(&mut self) { self.result = Some(false); }
    fn frame(&mut self, ui: &mut Ui) {
        let (w, h) = (ui.w, ui.h);
        ui.maximize_button(w - 80.0);
        let sub = self.sub.clone();
        if ui.titlebar("Change speed", &sub) { self.result = Some(false); }
        ui.divider(TITLE_H, 0.0, w);
        let body = h - TITLE_H - FOOT_H - 1.0;
        self.sidebar(ui, r(w - SIDE, TITLE_H + 1.0, SIDE - 1.0, body));
        if ui.key(VK_SPACE) { if self.resume { self.resume = false; } else { self.toggle_play(); } }
        self.scrubber(ui, r(1.0, TITLE_H + 1.0 + body - SCRUB_H, w - SIDE - 1.0, SCRUB_H));
        self.update_picture(ui);
        self.stage(ui, r(1.0, TITLE_H + 1.0, w - SIDE - 1.0, body - SCRUB_H));
        let fy = h - FOOT_H;
        ui.divider(fy, 0.0, w);
        ui.text("The preview plays without sound. The saved file keeps it.", r(PAD, fy, w - 320.0, FOOT_H), 12.5, 400, MUTED, Align::Left);
        let by = fy + (FOOT_H - 38.0) / 2.0;
        let primary = r(w - PAD - 120.0, by, 120.0, 38.0);
        if ui.button("sp.cancel", "Cancel", r(primary.x - 96.0, by, 88.0, 38.0), Style::Ghost, true) { self.result = Some(false); }
        if ui.button("sp.ok", "Apply", primary, Style::Primary, true) { self.result = Some(true); }
        if ui.key(VK_ESCAPE) { self.result = Some(false); }
        if ui.key(VK_RETURN) { self.result = Some(true); }
    }
}

/// Opens the window and blocks. Returns the speed chosen, or None on cancel.
pub fn pick(first: Bmp, at: f64, dur: f64, fps: f64, play: player::Source, load: Loader) -> Option<f64> {
    let p = cursor_pos();
    let (wa, s) = (work_area_at(p), dpi_scale_at(p));
    let start = (((wa.right - wa.left) as f32 / s * 0.8).max(980.0), ((wa.bottom - wa.top) as f32 / s * 0.8).max(620.0));
    let pos = (at / dur.max(0.001) * 1000.0) as f32;
    let win = Window {
        sub: form::context(), dur, fps, play, load, sel: DEFAULT, step: 0.0, pos, frame: first, frame_gen: 0, still_at: pos,
        fetching: None, fetched: Arc::new(Mutex::new(None)), player: None, playing_at: 1.0, resume: false, start, result: None,
    };
    let w = ui::run_modal(win, "Change speed", None)?;
    (w.result == Some(true)).then(|| w.speed())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slider_steps_read_as_clean_speeds() {
        assert_eq!(from_step(0.0), 1.0);
        assert_eq!(from_step(100.0), 2.0);
        assert_eq!(from_step(-100.0), 0.5);
        assert_eq!(from_step(14.0), 1.1);
        assert_eq!(from_step(LOG_MIN), 0.1);
        assert_eq!(from_step(LOG_MAX), 10.0);
    }
    #[test]
    fn steps_are_finer_at_slow_speeds() {
        let slow = from_step(-300.0) - from_step(-301.0);
        let fast = from_step(301.0) - from_step(300.0);
        assert!(slow > 0.0 && slow < fast / 20.0);
    }
    #[test]
    fn presets_round_trip_through_the_slider() {
        for f in PRESETS { assert!((from_step(to_step(f)) - f).abs() / f < 0.01, "{f}"); }
    }
    #[test]
    fn labels_drop_trailing_zeros() {
        assert_eq!(label(2.0), "2x");
        assert_eq!(label(0.75), "0.75x");
        assert_eq!(label(1.1), "1.1x");
    }
}
