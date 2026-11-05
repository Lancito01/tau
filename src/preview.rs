//! Compare window: the same part of a file at every compression level, side by side with the original, each with its
//! expected size. Samples are real encodes with the settings the tool will use, made in the background, so what shows
//! is what the result will look like.
use crate::form;
use crate::gfx::*;
use crate::image::Bmp;
use crate::ui::{self, id, r, Align, Page, Style, Ui, FOOT_H, PAD, R, TITLE_H};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const SIDE: f32 = 290.0;
const ZOOMS: [&str; 3] = ["Fit", "100%", "200%"];

/// Makes one tile: (entry index, option index) gives the picture and its size in bytes. Entry 0 is the original.
pub type Sampler = Arc<dyn Fn(usize, usize) -> Result<(Bmp, u64), String> + Send + Sync>;

pub struct Spec {
    pub title: String,
    pub ok: String,
    /// The levels to offer. The original comes before them on its own tile.
    pub levels: Vec<String>,
    pub default: usize,
    pub option_label: String,
    pub options: Vec<String>,
    pub sampler: Sampler,
}

pub fn format_bytes(n: u64) -> String {
    let v = n as f64;
    if v >= 1e9 { format!("{:.2} GB", v / 1e9) } else if v >= 1e6 { format!("{:.1} MB", v / 1e6) } else if v >= 1e3 { format!("{:.0} KB", v / 1e3) } else { format!("{n} B") }
}

type Tile = Option<Result<(Bmp, u64), String>>;

struct Preview {
    spec: Spec,
    sub: String,
    sel: usize,
    opt: usize,
    running_opt: Option<usize>,
    zoom: usize,
    pan: (f32, f32),
    drag: Option<((f32, f32), (f32, f32))>,
    tiles: Arc<Mutex<Vec<Tile>>>,
    generation: Arc<AtomicU64>,
    result: Option<bool>,
    start: (f32, f32),
}

impl Preview {
    fn new(spec: Spec, sub: String) -> Preview {
        let n = spec.levels.len() + 1;
        let at = cursor_pos();
        let (wa, s) = (work_area_at(at), dpi_scale_at(at));
        let start = (((wa.right - wa.left) as f32 / s * 0.8).max(980.0), ((wa.bottom - wa.top) as f32 / s * 0.8).max(620.0));
        let sel = spec.default;
        Preview { spec, sub, sel, opt: 0, running_opt: None, zoom: 1, pan: (0.5, 0.5), drag: None, tiles: Arc::new(Mutex::new((0..n).map(|_| None).collect())), generation: Arc::new(AtomicU64::new(0)), result: None, start }
    }

    /// Starts over for the current option. Runs the level the user is looking at first, and stops early if the option changes again.
    fn sample(&mut self, ui: &mut Ui) {
        if self.running_opt == Some(self.opt) { return; }
        let old = self.generation.fetch_add(1, Ordering::SeqCst);
        let now = old + 1;
        let n = self.spec.levels.len() + 1;
        for i in 0..n { ui.forget_image(id(&format!("pv.{old}.{i}"))); }
        *self.tiles.lock().unwrap() = (0..n).map(|_| None).collect();
        self.running_opt = Some(self.opt);
        let (sampler, tiles, generation, opt) = (self.spec.sampler.clone(), self.tiles.clone(), self.generation.clone(), self.opt);
        let first = self.sel + 1;
        std::thread::spawn(move || {
            crate::job::com_init();
            let mut order = vec![0, first];
            order.extend((1..n).filter(|i| *i != first));
            for i in order {
                if generation.load(Ordering::SeqCst) != now { return; }
                let made = sampler(i, opt);
                if generation.load(Ordering::SeqCst) != now { return; }
                tiles.lock().unwrap()[i] = Some(made);
            }
        });
    }

    fn name(&self, i: usize) -> String { if i == 0 { "Original".to_string() } else { self.spec.levels[i - 1].clone() } }

    fn draw_tile(&mut self, ui: &mut Ui, i: usize, q: R, base: Option<(u32, u32)>) {
        ui.fill(q, 8.0, 0x0A0B0C, 1.0);
        let now = self.generation.load(Ordering::SeqCst);
        let mut size = None;
        let mut failure = None;
        let mut pending = false;
        {
            let tiles = self.tiles.lock().unwrap();
            match &tiles[i] {
                Some(Ok((bmp, bytes))) => {
                    size = Some(*bytes);
                    // Every tile draws at the original's size, so a smaller encode shows as soft as it will really look.
                    let (bw, bh) = base.unwrap_or((bmp.w, bmp.h));
                    let inner = q.inset(1.0);
                    let s = match self.zoom { 1 => 1.0, 2 => 2.0, _ => (inner.w / bw as f32).min(inner.h / bh as f32) };
                    let (dw, dh) = (bw as f32 * s, bh as f32 * s);
                    let (dx, dy) = if self.zoom == 0 { (inner.x + (inner.w - dw) / 2.0, inner.y + (inner.h - dh) / 2.0) } else {
                        (inner.x + inner.w / 2.0 - self.pan.0 * dw, inner.y + inner.h / 2.0 - self.pan.1 * dh)
                    };
                    ui.clip(inner);
                    ui.image(id(&format!("pv.{now}.{i}")), bmp, r(dx, dy, dw, dh));
                    ui.unclip();
                }
                Some(Err(e)) => failure = Some(e.clone()),
                None => pending = true,
            }
        }
        let label = self.name(i);
        let w = ui.measure(&label, 12.0, 600) + 18.0;
        let chip = r(q.x + 8.0, q.y + 8.0, w, 24.0);
        ui.fill(chip, 6.0, 0, 0.62);
        ui.text(&label, chip.inset(0.0), 12.0, 600, TEXT, Align::Center);
        if let Some(b) = size {
            let t = if i == 0 { format_bytes(b) } else { format!("about {}", format_bytes(b)) };
            let tw = ui.measure(&t, 11.5, 500) + 16.0;
            let c = r(q.right() - tw - 8.0, q.y + 8.0, tw, 24.0);
            ui.fill(c, 6.0, 0, 0.62);
            ui.text(&t, c, 11.5, 500, MUTED, Align::Center);
        }
        if pending {
            ui.want_anim = true;
            ui.text("Encoding a sample", q, 12.5, 400, MUTED, Align::Center);
        }
        if let Some(e) = failure { ui.wrapped(&e, q.inset(40.0), 12.0, 400, DANGER); }
        if i == self.sel + 1 { ui.stroke(q, 8.0, ACCENT, 1.0, 2.0); }
        if i > 0 {
            let (hover, _, clicked) = ui.hit(id(&format!("pv.tile{i}")), q);
            if clicked && self.drag.is_none() { self.sel = i - 1; }
            if hover && self.sel + 1 != i { ui.stroke(q, 8.0, TEXT, 0.25, 1.0); }
        }
    }

    fn stage(&mut self, ui: &mut Ui, q: R) {
        ui.fill(q, 0.0, 0x050506, 1.0);
        let n = self.spec.levels.len() + 1;
        let cols = if n <= 2 { n } else { 2 };
        let rows = n.div_ceil(cols);
        let gap = 8.0;
        let area = q.inset(10.0);
        let (tw, th) = ((area.w - gap * (cols - 1) as f32) / cols as f32, (area.h - gap * (rows - 1) as f32) / rows as f32);
        let base = self.tiles.lock().unwrap().iter().flatten().find_map(|t| t.as_ref().ok().map(|(b, _)| (b.w, b.h)));
        let base = self.tiles.lock().unwrap()[0].as_ref().and_then(|t| t.as_ref().ok().map(|(b, _)| (b.w, b.h))).or(base);

        // Dragging any tile moves them all, so the same spot stays in view everywhere.
        if self.zoom != 0 {
            if ui.pressed && q.has(ui.mouse) { self.drag = Some((ui.mouse, self.pan)); }
            if let (Some((from, pan)), true, Some((bw, bh))) = (self.drag, ui.down, base) {
                let s = if self.zoom == 2 { 2.0 } else { 1.0 };
                self.pan = ((pan.0 - (ui.mouse.0 - from.0) / (bw as f32 * s)).clamp(0.0, 1.0), (pan.1 - (ui.mouse.1 - from.1) / (bh as f32 * s)).clamp(0.0, 1.0));
            }
            if q.has(ui.mouse) { ui.cursor = IDC_SIZEALL; }
        }
        let moved = self.drag.is_some_and(|(from, _)| (ui.mouse.0 - from.0).abs() + (ui.mouse.1 - from.1).abs() > 4.0);
        let was = self.drag;
        // A press that hardly moved is a click on a tile.
        if !moved { self.drag = None; }
        for i in 0..n {
            let (c, row) = (i % cols, i / cols);
            self.draw_tile(ui, i, r(area.x + c as f32 * (tw + gap), area.y + row as f32 * (th + gap), tw, th), base);
        }
        self.drag = was;
        if !ui.down { self.drag = None; }
    }

    fn sidebar(&mut self, ui: &mut Ui, q: R) {
        ui.fill(q, 0.0, SURFACE, 0.6);
        ui.fill(r(q.x, q.y, 1.0, q.h), 0.0, BORDER, 0.7);
        let (x, w) = (q.x + PAD, q.w - PAD * 2.0);
        let mut y = q.y + 14.0;
        let original = self.tiles.lock().unwrap()[0].as_ref().and_then(|t| t.as_ref().ok().map(|(_, b)| *b));
        ui.text("Level", r(x, y, w, 18.0), 12.5, 500, MUTED, Align::Left);
        y += 24.0;
        for l in 0..self.spec.levels.len() {
            let row = r(x, y, w, 54.0);
            let (hover, _, clicked) = ui.hit(id(&format!("pv.level{l}")), row);
            if clicked { self.sel = l; }
            let on = self.sel == l;
            ui.fill(row, 10.0, TEXT, if on { 0.10 } else if hover { 0.06 } else { 0.02 });
            if on { ui.stroke(row, 10.0, ACCENT, 0.8, 1.0); }
            let name = self.spec.levels[l].clone();
            ui.text(&name, r(row.x + 12.0, row.y + 6.0, row.w - 24.0, 20.0), 13.0, 600, TEXT, Align::Left);
            let size = self.tiles.lock().unwrap()[l + 1].as_ref().and_then(|t| t.as_ref().ok().map(|(_, b)| *b));
            let detail = match (size, original) {
                (Some(b), Some(o)) if b >= o => format!("About {}, no saving, so the original stays", format_bytes(b)),
                (Some(b), Some(o)) => format!("About {}, {}% smaller", format_bytes(b), 100 - b * 100 / o.max(1)),
                (Some(b), None) => format!("About {}", format_bytes(b)),
                _ => "Working it out".to_string(),
            };
            ui.text(&detail, r(row.x + 12.0, row.y + 27.0, row.w - 24.0, 18.0), 11.5, 400, MUTED, Align::Left);
            y += 60.0;
        }
        y += 8.0;
        if !self.spec.options.is_empty() {
            ui.text(&self.spec.option_label.clone(), r(x, y, w, 18.0), 12.5, 500, MUTED, Align::Left);
            let opts = self.spec.options.clone();
            ui.select("pv.option", r(x, y + 22.0, w, 38.0), &opts, &mut self.opt, true);
            y += 72.0;
        }
        ui.text("Zoom", r(x, y, w, 18.0), 12.5, 500, MUTED, Align::Left);
        let zooms: Vec<String> = ZOOMS.iter().map(|s| s.to_string()).collect();
        ui.segmented("pv.zoom", r(x, y + 22.0, w, 34.0), &zooms, &mut self.zoom);
        y += 70.0;
        ui.wrapped("Each tile is a real encode of a sample from this file. Sizes are estimated from those samples, so the final size can differ.", r(x, y, w, 72.0), 11.5, 400, MUTED);
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
        self.sample(ui);
        self.stage(ui, r(1.0, TITLE_H + 1.0, w - SIDE - 1.0, body));
        let fy = h - FOOT_H;
        ui.divider(fy, 0.0, w);
        ui.text("Click a tile to choose it. Drag to look around.", r(PAD, fy, w - 320.0, FOOT_H), 12.5, 400, MUTED, Align::Left);
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
    /// Stops the sampling thread after its current tile.
    fn drop(&mut self) { self.generation.fetch_add(1, Ordering::SeqCst); }
}

/// Shows the window and blocks. Returns the chosen level and option, or None on cancel.
pub fn show(spec: Spec) -> Option<(String, String)> {
    let title = spec.title.clone();
    let p = ui::run_modal(Preview::new(spec, form::context()), &title, None)?;
    if p.result != Some(true) { return None; }
    Some((p.spec.levels[p.sel].clone(), p.spec.options.get(p.opt).cloned().unwrap_or_default()))
}

/// The window as a PNG, with every tile filled, for design review.
pub fn render(spec: Spec, out: &std::path::Path) -> Result<(), String> {
    let n = spec.levels.len() + 1;
    let sampler = spec.sampler.clone();
    let mut p = Preview::new(spec, "interview take 3.mp4".into());
    p.start = (1180.0, 720.0);
    p.running_opt = Some(0);
    *p.tiles.lock().unwrap() = (0..n).map(|i| Some(sampler(i, 0))).collect();
    ui::render_png(&mut p, 1.0, None, out)
}
