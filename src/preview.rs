//! Compress window: one view of the file as it would come out, and a dropdown of levels that each say what they cost
//! and what they come to. Pictures and sizes are real encodes with the settings the tool will use, made in the
//! background, so what shows is what the result will look like.
use crate::form;
use crate::gfx::*;
use crate::image::Bmp;
use crate::timeline::{snap, timecode};
use crate::ui::{self, id, r, Align, Page, Style, Ui, FOOT_H, PAD, R, TITLE_H};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const SIDE: f32 = 340.0;
const SCRUB_H: f32 = 56.0;
const ZOOMS: [&str; 3] = ["Fit", "100%", "200%"];
/// Pictures kept for going back to. A level or a moment seen a moment ago comes back at once.
const KEEP_PICTURES: usize = 24;

/// One setting of the Custom level. A value means the same thing in every level, so levels can be handed to `Custom`.
pub enum Control {
    Slider { label: String, min: i32, max: i32, unit: &'static str },
    Choice { label: String, options: Vec<(String, i32)> },
}

/// A ready-made level: what it is called, and a value for each control.
pub struct Preset {
    pub name: String,
    pub values: Vec<i32>,
}

/// The file as it would come out. `None` asks for the original. The time is ignored by anything without frames.
pub type Picture = Arc<dyn Fn(Option<&[i32]>, f64) -> Result<Bmp, String> + Send + Sync>;
/// The expected size in bytes of the whole file at these values.
pub type Size = Arc<dyn Fn(&[i32]) -> Result<u64, String> + Send + Sync>;

pub struct Spec {
    pub title: String,
    pub ok: String,
    pub presets: Vec<Preset>,
    pub default: usize,
    pub controls: Vec<Control>,
    /// Where Custom starts for a control whose level value it cannot show, such as "no scan at all" on a DPI slider.
    pub custom_start: Vec<i32>,
    pub original_bytes: u64,
    /// Length of a video. Zero leaves the scrubber out.
    pub dur: f64,
    pub fps: f64,
    /// False when the picture does not change with the level, as with a waveform: nothing to zoom or compare.
    pub visual: bool,
    /// Something true of this file at every level.
    pub note: String,
    pub picture: Picture,
    pub size: Size,
}

pub fn format_bytes(n: u64) -> String {
    let v = n as f64;
    if v >= 1e9 { format!("{:.2} GB", v / 1e9) } else if v >= 1e6 { format!("{:.1} MB", v / 1e6) } else if v >= 1e3 { format!("{:.0} KB", v / 1e3) } else { format!("{n} B") }
}

/// What a dropdown row says it comes to.
fn size_label(size: Option<&Result<u64, String>>, original: u64) -> String {
    match size {
        Some(Ok(b)) if *b >= original && original > 0 => format!("{}, no saving", format_bytes(*b)),
        Some(Ok(b)) => format!("about {}", format_bytes(*b)),
        Some(Err(_)) => "size unknown".to_string(),
        None => "working out size".to_string(),
    }
}

/// The sentence under the dropdown.
fn size_detail(size: Option<&Result<u64, String>>, original: u64) -> String {
    match size {
        Some(Ok(b)) if *b >= original && original > 0 => format!("About {}, which saves nothing, so the original would stay.", format_bytes(*b)),
        Some(Ok(b)) if original > 0 => format!("About {}, {}% smaller than the {} original.", format_bytes(*b), 100 - b * 100 / original, format_bytes(original)),
        Some(Ok(b)) => format!("About {}.", format_bytes(*b)),
        Some(Err(e)) => e.clone(),
        None => "Encoding samples to work out the size.".to_string(),
    }
}

/// The slider's millesimal position as a time, kept far enough from the end that a frame exists there.
fn time_at(pos: f32, dur: f64, fps: f64) -> f64 {
    if dur <= 0.0 { return 0.0; }
    let frame = if fps > 0.0 { 1.0 / fps } else { 0.04 };
    snap(pos as f64 / 1000.0 * dur, fps).min((dur - frame * 2.0).max(0.0))
}

type Key = (Vec<i32>, i64);

fn key_of(values: &[i32], t: f64) -> Key { (values.to_vec(), (t * 1000.0).round() as i64) }

/// What the window and its two background workers share. An empty value list means the original.
struct Shared {
    alive: AtomicBool,
    want: Mutex<(Vec<i32>, f64)>,
    sizes: Mutex<HashMap<Vec<i32>, Result<u64, String>>>,
    pictures: Mutex<HashMap<Key, Arc<Result<Bmp, String>>>>,
}

impl Shared {
    fn new() -> Arc<Shared> {
        Arc::new(Shared { alive: AtomicBool::new(true), want: Mutex::new((vec![], 0.0)), sizes: Mutex::new(HashMap::new()), pictures: Mutex::new(HashMap::new()) })
    }
    fn picture(&self, k: &Key) -> Option<Arc<Result<Bmp, String>>> { self.pictures.lock().unwrap().get(k).cloned() }
    fn size(&self, v: &[i32]) -> Option<Result<u64, String>> { self.sizes.lock().unwrap().get(v).cloned() }
    fn store_picture(&self, k: Key, res: Result<Bmp, String>) {
        let mut all = self.pictures.lock().unwrap();
        if all.len() >= KEEP_PICTURES { all.clear(); }
        all.insert(k, Arc::new(res));
    }
}

/// Sizes first for what is being looked at, then for every level, so the dropdown fills in by itself.
fn size_worker(shared: Arc<Shared>, size: Size, presets: Vec<Vec<i32>>) {
    crate::job::com_init();
    while shared.alive.load(Ordering::SeqCst) {
        let current = shared.want.lock().unwrap().0.clone();
        let next = std::iter::once(current).chain(presets.iter().cloned()).find(|v| !v.is_empty() && shared.size(v).is_none());
        match next {
            Some(v) => { let res = size(&v); shared.sizes.lock().unwrap().insert(v, res); }
            None => std::thread::sleep(std::time::Duration::from_millis(40)),
        }
    }
}

/// The moment being looked at. The original comes first because it takes no encoding, so scrubbing keeps up even
/// while the compressed frame is still being made.
fn picture_worker(shared: Arc<Shared>, picture: Picture) {
    crate::job::com_init();
    while shared.alive.load(Ordering::SeqCst) {
        let (values, t) = shared.want.lock().unwrap().clone();
        let original = key_of(&[], t);
        let wanted = key_of(&values, t);
        if shared.picture(&original).is_none() {
            let res = picture(None, t);
            shared.store_picture(original, res);
        } else if !values.is_empty() && shared.picture(&wanted).is_none() {
            let res = picture(Some(&values), t);
            shared.store_picture(wanted, res);
        } else {
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
    }
}

struct Preview {
    spec: Spec,
    sub: String,
    sel: usize,
    /// The Custom values as the controls show them, and as they stood when last let go.
    custom: Vec<i32>,
    settled: Vec<i32>,
    pos: f32,
    zoom: usize,
    pan: (f32, f32),
    drag: Option<((f32, f32), (f32, f32))>,
    /// True while the "see the original" button is held.
    comparing: bool,
    shown: Option<u64>,
    shared: Arc<Shared>,
    started: bool,
    result: Option<bool>,
    start: (f32, f32),
}

impl Preview {
    fn new(spec: Spec, sub: String) -> Preview {
        let at = cursor_pos();
        let (wa, s) = (work_area_at(at), dpi_scale_at(at));
        let start = (((wa.right - wa.left) as f32 / s * 0.8).max(980.0), ((wa.bottom - wa.top) as f32 / s * 0.8).max(620.0));
        let sel = spec.default;
        let custom = spec.custom_start.clone();
        Preview { sub, sel, settled: custom.clone(), custom, pos: 300.0, zoom: 0, pan: (0.5, 0.5), drag: None, comparing: false, shown: None, shared: Shared::new(), started: false, result: None, start, spec }
    }

    fn is_custom(&self) -> bool { self.sel >= self.spec.presets.len() }

    /// The values of the level in use.
    fn values(&self) -> Vec<i32> { self.spec.presets.get(self.sel).map(|p| p.values.clone()).unwrap_or_else(|| self.settled.clone()) }

    fn time(&self) -> f64 { time_at(self.pos, self.spec.dur, self.spec.fps) }

    fn start_workers(&mut self) {
        if self.started { return; }
        self.started = true;
        let presets: Vec<Vec<i32>> = self.spec.presets.iter().map(|p| p.values.clone()).collect();
        let (a, size) = (self.shared.clone(), self.spec.size.clone());
        std::thread::spawn(move || size_worker(a, size, presets));
        let (b, picture) = (self.shared.clone(), self.spec.picture.clone());
        std::thread::spawn(move || picture_worker(b, picture));
    }

    /// Custom starts from the level that was showing, so it is a step away from something the user has already judged.
    fn seed_custom(&mut self, from: usize) {
        let Some(p) = self.spec.presets.get(from) else { return };
        for (i, c) in self.spec.controls.iter().enumerate() {
            let v = p.values[i];
            let fits = match c {
                Control::Slider { min, max, .. } => v >= *min && v <= *max,
                Control::Choice { options, .. } => options.iter().any(|o| o.1 == v),
            };
            self.custom[i] = if fits { v } else { self.spec.custom_start[i] };
        }
        self.settled = self.custom.clone();
    }

    fn stage(&mut self, ui: &mut Ui, q: R) {
        ui.fill(q, 0.0, 0x050506, 1.0);
        let view = q.inset(10.0);
        ui.fill(view, 8.0, 0x0A0B0C, 1.0);
        let values = self.values();
        let t = self.time();
        let original_key = key_of(&[], t);
        let original = self.shared.picture(&original_key);
        let wanted = if self.comparing || !self.spec.visual { original_key.clone() } else { key_of(&values, t) };
        let exact = self.shared.picture(&wanted);
        // Until this frame is encoded the original stands in, and the label says so.
        let (key, picture) = match (&exact, &original) {
            (Some(p), _) => (wanted.clone(), Some(p.clone())),
            (None, Some(p)) => (original_key.clone(), Some(p.clone())),
            _ => (wanted.clone(), None),
        };
        let encoding = exact.is_none();

        // Every level draws at the original's size, so a smaller encode shows as soft as it will really look.
        let base = original.as_ref().and_then(|p| p.as_ref().as_ref().ok().map(|b| (b.w, b.h)));
        let scale_of = |bw: u32, bh: u32, zoom: usize, q: R| match zoom { 1 => 1.0, 2 => 2.0, _ => (q.w / bw as f32).min(q.h / bh as f32) };
        if self.zoom != 0 {
            if ui.pressed && view.has(ui.mouse) { self.drag = Some((ui.mouse, self.pan)); }
            if let (Some((from, pan)), true, Some((bw, bh))) = (self.drag, ui.down, base) {
                let s = scale_of(bw, bh, self.zoom, view);
                self.pan = ((pan.0 - (ui.mouse.0 - from.0) / (bw as f32 * s)).clamp(0.0, 1.0), (pan.1 - (ui.mouse.1 - from.1) / (bh as f32 * s)).clamp(0.0, 1.0));
            }
            if view.has(ui.mouse) { ui.cursor = IDC_SIZEALL; }
        }
        if !ui.down { self.drag = None; }

        let inner = view.inset(1.0);
        let mut failure = None;
        match picture.as_ref().map(|p| p.as_ref()) {
            Some(Ok(bmp)) => {
                let (bw, bh) = base.unwrap_or((bmp.w, bmp.h));
                let s = scale_of(bw, bh, self.zoom, inner);
                let (dw, dh) = (bw as f32 * s, bh as f32 * s);
                let (dx, dy) = if self.zoom == 0 { (inner.x + (inner.w - dw) / 2.0, inner.y + (inner.h - dh) / 2.0) } else {
                    (inner.x + inner.w / 2.0 - self.pan.0 * dw, inner.y + inner.h / 2.0 - self.pan.1 * dh)
                };
                let k = id(&format!("pv.{:?}", key));
                if let Some(old) = self.shown.filter(|o| *o != k) { ui.forget_image(old); }
                self.shown = Some(k);
                ui.clip(inner);
                ui.image(k, bmp, r(dx, dy, dw, dh));
                ui.unclip();
            }
            Some(Err(e)) => failure = Some(e.clone()),
            None => { ui.text("Reading the file", view, 12.5, 400, MUTED, Align::Center); }
        }
        if let Some(e) = failure { ui.wrapped(&e, view.inset(40.0), 12.0, 400, DANGER); }

        let label = if !self.spec.visual { String::new() } else if self.comparing { "Original".to_string() } else if encoding { "Original, until this frame is encoded".to_string() } else { self.level_name() };
        if !label.is_empty() {
            let w = ui.measure(&label, 12.0, 600) + 18.0;
            let chip = r(view.x + 8.0, view.y + 8.0, w, 24.0);
            ui.fill(chip, 6.0, 0, 0.62);
            ui.text(&label, chip, 12.0, 600, TEXT, Align::Center);
        }
        if encoding || self.shared.size(&values).is_none() || self.spec.presets.iter().any(|p| self.shared.size(&p.values).is_none()) { ui.want_anim = true; }
    }

    fn level_name(&self) -> String { self.spec.presets.get(self.sel).map(|p| p.name.clone()).unwrap_or_else(|| "Custom".to_string()) }

    fn scrubber(&mut self, ui: &mut Ui, q: R) {
        ui.fill(q, 0.0, SURFACE, 0.6);
        ui.fill(r(q.x, q.y, q.w, 1.0), 0.0, BORDER, 0.7);
        ui.slider("pv.scrub", r(q.x + PAD - 9.0, q.y + 14.0, q.w - PAD * 2.0 - 132.0 + 18.0, 28.0), 0.0, 1000.0, 0.0, &mut self.pos);
        let label = format!("{} / {}", timecode(self.time(), self.spec.fps), timecode(self.spec.dur, self.spec.fps));
        ui.text(&label, r(q.right() - PAD - 124.0, q.y, 124.0, q.h), 12.0, 500, MUTED, Align::Right);
    }

    fn sidebar(&mut self, ui: &mut Ui, q: R) {
        ui.fill(q, 0.0, SURFACE, 0.6);
        ui.fill(r(q.x, q.y, 1.0, q.h), 0.0, BORDER, 0.7);
        let (x, w) = (q.x + PAD, q.w - PAD * 2.0);
        let mut y = q.y + 14.0;
        ui.text("Compression", r(x, y, w, 18.0), 12.5, 500, MUTED, Align::Left);
        y += 22.0;
        let original = self.spec.original_bytes;
        let mut rows: Vec<String> = self.spec.presets.iter().map(|p| format!("{}, {}", p.name, size_label(self.shared.size(&p.values).as_ref(), original))).collect();
        let custom_size = self.shared.size(&self.settled);
        rows.push(if self.is_custom() { format!("Custom, {}", size_label(custom_size.as_ref(), original)) } else { "Custom".to_string() });
        let before = self.sel;
        ui.select("pv.level", r(x, y, w, 38.0), &rows, &mut self.sel, true);
        if self.sel != before && self.is_custom() { self.seed_custom(before); }
        y += 46.0;
        let detail = size_detail(self.shared.size(&self.values()).as_ref(), original);
        let h = ui.wrapped_height(&detail, 12.0, 400, w);
        ui.wrapped(&detail, r(x, y, w, h), 12.0, 400, MUTED);
        y += h + 10.0;
        if !self.spec.note.is_empty() {
            let note = self.spec.note.clone();
            let h = ui.wrapped_height(&note, 12.0, 400, w);
            ui.wrapped(&note, r(x, y, w, h), 12.0, 400, MUTED);
            y += h + 10.0;
        }
        if self.is_custom() { y = self.custom_controls(ui, x, y, w); }
        if self.spec.visual {
            y += 4.0;
            ui.text("Zoom", r(x, y, w, 18.0), 12.5, 500, MUTED, Align::Left);
            let zooms: Vec<String> = ZOOMS.iter().map(|s| s.to_string()).collect();
            ui.segmented("pv.zoom", r(x, y + 22.0, w, 34.0), &zooms, &mut self.zoom);
            y += 68.0;
            let b = r(x, y, w, 36.0);
            let (hover, held, _) = ui.hit(id("pv.compare"), b);
            self.comparing = held;
            ui.fill(b, 9.0, TEXT, if held { 0.14 } else if hover { 0.08 } else { 0.04 });
            ui.stroke(b, 9.0, TEXT, 0.09, 1.0);
            ui.text("Hold to see the original", b, 12.5, 500, TEXT, Align::Center);
        }
    }

    fn custom_controls(&mut self, ui: &mut Ui, x: f32, mut y: f32, w: f32) -> f32 {
        let warning = "These go well past the levels above and can lose a lot of detail. The original file is not changed.";
        let h = ui.wrapped_height(warning, 12.0, 400, w);
        ui.wrapped(warning, r(x, y, w, h), 12.0, 400, MUTED);
        y += h + 10.0;
        for i in 0..self.spec.controls.len() {
            let row_key = format!("pv.custom{i}");
            match &self.spec.controls[i] {
                Control::Slider { label, min, max, unit } => {
                    let (label, min, max) = (label.clone(), *min as f32, *max as f32);
                    let value = format!("{}{}", self.custom[i], unit);
                    ui.text(&label, r(x, y, w, 18.0), 12.5, 500, MUTED, Align::Left);
                    ui.text(&value, r(x, y, w, 18.0), 12.5, 500, TEXT, Align::Right);
                    let mut v = self.custom[i] as f32;
                    ui.slider(&row_key, r(x - 9.0, y + 18.0, w + 18.0, 28.0), min, max, min, &mut v);
                    self.custom[i] = v as i32;
                    y += 50.0;
                }
                Control::Choice { label, options } => {
                    let label = label.clone();
                    let names: Vec<String> = options.iter().map(|o| o.0.clone()).collect();
                    let values: Vec<i32> = options.iter().map(|o| o.1).collect();
                    let mut at = values.iter().position(|v| *v == self.custom[i]).unwrap_or(0);
                    ui.text(&label, r(x, y, w, 18.0), 12.5, 500, MUTED, Align::Left);
                    ui.select(&row_key, r(x, y + 20.0, w, 34.0), &names, &mut at, true);
                    self.custom[i] = values.get(at).copied().unwrap_or(0);
                    y += 62.0;
                }
            }
        }
        // A level is worth encoding only once the slider has stopped.
        if !ui.down { self.settled = self.custom.clone(); }
        y
    }
}

impl Page for Preview {
    fn size(&self) -> (f32, f32) { self.start }
    fn resizable(&self) -> bool { true }
    fn done(&self) -> bool { self.result.is_some() }
    fn on_close(&mut self) { self.result = Some(false); }
    fn frame(&mut self, ui: &mut Ui) {
        let (w, h) = (ui.w, ui.h);
        ui.maximize_button(w - 80.0);
        let (title, sub) = (self.spec.title.clone(), self.sub.clone());
        if ui.titlebar(&title, &sub) { self.result = Some(false); }
        ui.divider(TITLE_H, 0.0, w);
        let body = h - TITLE_H - FOOT_H - 1.0;
        self.sidebar(ui, r(w - SIDE, TITLE_H + 1.0, SIDE - 1.0, body));
        let scrub = if self.spec.dur > 0.0 { SCRUB_H } else { 0.0 };
        if scrub > 0.0 { self.scrubber(ui, r(1.0, TITLE_H + 1.0 + body - scrub, w - SIDE - 1.0, scrub)); }
        *self.shared.want.lock().unwrap() = (self.values(), self.time());
        self.start_workers();
        self.stage(ui, r(1.0, TITLE_H + 1.0, w - SIDE - 1.0, body - scrub));
        let fy = h - FOOT_H;
        ui.divider(fy, 0.0, w);
        let hint = if !self.spec.visual { "Sizes are worked out from the length of the file, so the final size can differ a little." } else { "Each view is a real encode. Sizes are estimated from samples, so the final size can differ." };
        ui.text(hint, r(PAD, fy, w - 320.0, FOOT_H), 12.5, 400, MUTED, Align::Left);
        let by = fy + (FOOT_H - 38.0) / 2.0;
        let primary = r(w - PAD - 120.0, by, 120.0, 38.0);
        if ui.button("pv.cancel", "Cancel", r(primary.x - 96.0, by, 88.0, 38.0), Style::Ghost, true) { self.result = Some(false); }
        let ok = self.spec.ok.clone();
        if ui.button("pv.ok", &ok, primary, Style::Primary, true) { self.result = Some(true); }
        if ui.key(VK_ESCAPE) { self.result = Some(false); }
        if ui.key(VK_RETURN) { self.result = Some(true); }
    }
}

impl Drop for Preview {
    /// Stops the workers after whatever they are encoding now.
    fn drop(&mut self) { self.shared.alive.store(false, Ordering::SeqCst); }
}

/// Shows the window and blocks. Returns the values of the chosen level, or None on cancel.
pub fn show(spec: Spec) -> Option<Vec<i32>> {
    let title = spec.title.clone();
    let mut p = ui::run_modal(Preview::new(spec, form::context()), &title, None)?;
    if p.result != Some(true) { return None; }
    p.settled = p.custom.clone();
    Some(p.values())
}

/// The window as a PNG, with everything worked out, for design review. `level` past the presets opens Custom.
pub fn render(spec: Spec, level: usize, out: &std::path::Path) -> Result<(), String> {
    let mut p = Preview::new(spec, "interview take 3.mp4".into());
    p.start = (1180.0, 720.0);
    p.started = true;
    let last = p.spec.presets.len() - 1;
    if level > last { p.seed_custom(last); }
    p.sel = level;
    let t = p.time();
    p.shared.store_picture(key_of(&[], t), (p.spec.picture)(None, t));
    let values = p.values();
    p.shared.store_picture(key_of(&values, t), (p.spec.picture)(Some(&values), t));
    let all: Vec<Vec<i32>> = p.spec.presets.iter().map(|x| x.values.clone()).chain([values]).collect();
    for v in all {
        let size = (p.spec.size)(&v);
        p.shared.sizes.lock().unwrap().insert(v, size);
    }
    ui::render_png(&mut p, 1.0, None, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn labels_say_what_a_level_comes_to() {
        assert_eq!(size_label(Some(&Ok(12_000_000)), 84_000_000), "about 12.0 MB");
        assert_eq!(size_label(Some(&Ok(90_000_000)), 84_000_000), "90.0 MB, no saving");
        assert_eq!(size_label(None, 84_000_000), "working out size");
        assert!(size_detail(Some(&Ok(21_000_000)), 84_000_000).contains("75% smaller"));
    }
    #[test]
    fn a_frame_exists_at_every_slider_position() {
        assert!(time_at(1000.0, 10.0, 25.0) <= 10.0 - 2.0 / 25.0 + 1e-9);
        assert_eq!(time_at(500.0, 0.0, 0.0), 0.0);
        assert_eq!(time_at(1000.0, 0.05, 25.0), 0.0, "shorter than two frames");
    }
}
