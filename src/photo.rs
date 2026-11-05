//! Edit photo with a live preview. Returns the same options the tool always took, so saving is unchanged.
use crate::form;
use crate::gfx::*;
use crate::image::Bmp;
use crate::ui::{self, id, r, Align, Page, Style, Ui, FOOT_H, PAD, TITLE_H};
use windows::Win32::UI::Input::KeyboardAndMouse::*;

const WIDTH: f32 = 880.0;
const HEIGHT: f32 = 560.0;
const SIDE: f32 = 280.0;
const ROTATE: [&str; 4] = ["None", "90 right", "90 left", "180"];
const FLIP: [&str; 3] = ["None", "Horizontal", "Vertical"];
const PREVIEW_MAX: u32 = 640;

/// Same formula as the ffmpeg eq filter used when saving, close enough to judge by eye.
pub fn tone(px: &mut [u8], brightness: f32, contrast: f32, saturation: f32) {
    let (b, c, s) = (brightness / 200.0, 1.0 + contrast / 100.0, 1.0 + saturation / 100.0);
    for p in px.chunks_exact_mut(4) {
        let (bl, gr, re) = (p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0);
        let y = 0.299 * re + 0.587 * gr + 0.114 * bl;
        let y2 = (y - 0.5) * c + 0.5 + b;
        for (v, ch) in [(bl, 0), (gr, 1), (re, 2)] {
            p[ch] = ((y2 + s * (v - y)).clamp(0.0, 1.0) * 255.0) as u8;
        }
    }
}

/// Rotates clockwise by quarter turns, then flips, in that order, as the saved file is processed.
pub fn orient(src: &Bmp, rotate: usize, flip: usize) -> Bmp {
    let (w, h) = (src.w as usize, src.h as usize);
    let swap = rotate == 1 || rotate == 2;
    let (dw, dh) = if swap { (h, w) } else { (w, h) };
    let mut out = Bmp::new(dw as u32, dh as u32, [0, 0, 0, 255]);
    for y in 0..h {
        for x in 0..w {
            let (mut dx, mut dy) = match rotate {
                1 => (h - 1 - y, x),
                2 => (y, w - 1 - x),
                3 => (w - 1 - x, h - 1 - y),
                _ => (x, y),
            };
            if flip == 1 { dx = dw - 1 - dx; }
            if flip == 2 { dy = dh - 1 - dy; }
            let (si, di) = ((y * w + x) * 4, (dy * dw + dx) * 4);
            out.px[di..di + 4].copy_from_slice(&src.px[si..si + 4]);
        }
    }
    out
}

struct Photo {
    sub: String,
    base: Bmp,
    shown: Bmp,
    vals: [f32; 3],
    rotate: usize,
    flip: usize,
    applied: ([i32; 3], usize, usize),
    generation: u64,
    result: Option<bool>,
}

impl Photo {
    fn new(src: Bmp, sub: String) -> Photo {
        let long = src.w.max(src.h);
        let base = if long > PREVIEW_MAX {
            let f = PREVIEW_MAX as f64 / long as f64;
            src.scaled(((src.w as f64 * f) as u32).max(1), ((src.h as f64 * f) as u32).max(1)).unwrap_or(src)
        } else { src };
        Photo { sub, shown: base.clone(), base, vals: [0.0; 3], rotate: 0, flip: 0, applied: ([0; 3], 0, 0), generation: 0, result: None }
    }
    fn output(&self) -> Vec<String> {
        vec![
            format!("{}", self.vals[0] as i32), format!("{}", self.vals[1] as i32), format!("{}", self.vals[2] as i32),
            ROTATE[self.rotate].to_string(), FLIP[self.flip].to_string(),
        ]
    }
    fn refresh(&mut self, ui: &mut Ui) {
        let now = ([self.vals[0] as i32, self.vals[1] as i32, self.vals[2] as i32], self.rotate, self.flip);
        if now == self.applied { return; }
        self.applied = now;
        let mut b = orient(&self.base, self.rotate, self.flip);
        tone(&mut b.px, self.vals[0], self.vals[1], self.vals[2]);
        self.shown = b;
        ui.forget_image(self.key());
        self.generation += 1;
    }
    fn key(&self) -> u64 { id("photo.preview") ^ self.generation }
}

impl Page for Photo {
    fn size(&self) -> (f32, f32) { (WIDTH, HEIGHT) }
    fn done(&self) -> bool { self.result.is_some() }
    fn on_close(&mut self) { self.result = Some(false); }
    fn frame(&mut self, ui: &mut Ui) {
        if ui.titlebar("Edit photo", &self.sub.clone()) { self.result = Some(false); }
        ui.divider(TITLE_H, 0.0, WIDTH);
        let body_h = HEIGHT - TITLE_H - FOOT_H;
        let side_x = WIDTH - SIDE;

        // Controls first, so the preview below already shows this frame's values.
        ui.fill(r(side_x, TITLE_H + 1.0, SIDE - 1.0, body_h - 1.0), 0.0, SURFACE, 0.6);
        ui.fill(r(side_x, TITLE_H + 1.0, 1.0, body_h - 1.0), 0.0, BORDER, 0.7);
        let (x, w) = (side_x + PAD, SIDE - PAD * 2.0);
        let mut y = TITLE_H + 16.0;
        for (i, label) in ["Brightness", "Contrast", "Saturation"].iter().enumerate() {
            let n = self.vals[i] as i32;
            ui.text(label, r(x, y, w - 50.0, 20.0), 13.0, 400, TEXT, Align::Left);
            ui.text(&format!("{}{n}", if n > 0 { "+" } else { "" }), r(x + w - 50.0, y, 50.0, 20.0), 12.5, 600, MUTED, Align::Right);
            ui.slider(&format!("photo.s{i}"), r(x - 9.0, y + 20.0, w + 18.0, 28.0), -100.0, 100.0, 0.0, &mut self.vals[i]);
            y += 58.0;
        }
        let names = |list: &[&str]| -> Vec<String> { list.iter().map(|s| s.to_string()).collect() };
        ui.text("Rotate", r(x, y, w, 18.0), 12.5, 500, MUTED, Align::Left);
        ui.select("photo.rotate", r(x, y + 22.0, w, 38.0), &names(&ROTATE), &mut self.rotate, true);
        y += 72.0;
        ui.text("Flip", r(x, y, w, 18.0), 12.5, 500, MUTED, Align::Left);
        ui.select("photo.flip", r(x, y + 22.0, w, 38.0), &names(&FLIP), &mut self.flip, true);
        y += 74.0;
        let changed = self.vals != [0.0; 3] || self.rotate != 0 || self.flip != 0;
        if ui.button("photo.reset", "Reset", r(x, y, w, 34.0), Style::Secondary, changed) {
            self.vals = [0.0; 3];
            self.rotate = 0;
            self.flip = 0;
        }
        self.refresh(ui);

        // The picture.
        let stage = r(1.0, TITLE_H + 1.0, side_x - 1.0, body_h - 1.0);
        ui.fill(stage, 0.0, 0x0A0B0C, 1.0);
        let inner = stage.inset(18.0);
        let s = (inner.w / self.shown.w as f32).min(inner.h / self.shown.h as f32);
        let (pw, ph) = (self.shown.w as f32 * s, self.shown.h as f32 * s);
        let key = self.key();
        let dest = r(inner.x + (inner.w - pw) / 2.0, inner.y + (inner.h - ph) / 2.0, pw, ph);
        ui.image(key, &self.shown, dest);

        let fy = HEIGHT - FOOT_H;
        ui.divider(fy, 0.0, WIDTH);
        ui.text("Double-click a slider to reset it.", r(PAD, fy, 400.0, FOOT_H), 12.5, 400, MUTED, Align::Left);
        let by = fy + (FOOT_H - 38.0) / 2.0;
        let primary = r(WIDTH - PAD - 120.0, by, 120.0, 38.0);
        if ui.button("photo.cancel", "Cancel", r(primary.x - 96.0, by, 88.0, 38.0), Style::Ghost, true) { self.result = Some(false); }
        if ui.button("photo.ok", "Apply", primary, Style::Primary, true) { self.result = Some(true); }
        if ui.key(VK_ESCAPE) { self.result = Some(false); }
        if ui.key(VK_RETURN) { self.result = Some(true); }
    }
}

/// Shows the editor and blocks. Returns brightness, contrast, saturation, rotate and flip as option strings.
pub fn show(first: Bmp, files: usize) -> Option<Vec<String>> {
    let sub = if files > 1 { format!("Previewing the first of {files} files") } else { form::context() };
    let p = ui::run_modal(Photo::new(first, sub), "Edit photo", None)?;
    (p.result == Some(true)).then(|| p.output())
}

/// The editor as a PNG with sample values, for design review.
pub fn render(sample: Bmp, out: &std::path::Path) -> Result<(), String> {
    let mut p = Photo::new(sample, "holiday.jpg".into());
    p.vals = [12.0, 20.0, -15.0];
    ui::render_png(&mut p, 1.0, None, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn neutral_tone_changes_nothing() {
        let mut px = vec![10, 200, 90, 255, 0, 255, 128, 255];
        let before = px.clone();
        tone(&mut px, 0.0, 0.0, 0.0);
        for (a, b) in px.iter().zip(&before) { assert!((*a as i32 - *b as i32).abs() <= 1); }
    }
    #[test]
    fn no_saturation_makes_grey() {
        let mut px = vec![10, 200, 90, 255];
        tone(&mut px, 0.0, 0.0, -100.0);
        assert!((px[0] as i32 - px[2] as i32).abs() <= 1 && (px[1] as i32 - px[2] as i32).abs() <= 1);
    }
    fn tagged() -> Bmp {
        // Two by one: red on the left, blue on the right.
        let mut b = Bmp::new(2, 1, [0, 0, 0, 255]);
        b.px[..4].copy_from_slice(&[0, 0, 255, 255]);
        b.px[4..8].copy_from_slice(&[255, 0, 0, 255]);
        b
    }
    #[test]
    fn rotating_right_puts_left_on_top() {
        let o = orient(&tagged(), 1, 0);
        assert_eq!((o.w, o.h), (1, 2));
        assert_eq!(o.px[2], 255);
    }
    #[test]
    fn rotating_left_puts_right_on_top() {
        let o = orient(&tagged(), 2, 0);
        assert_eq!((o.w, o.h), (1, 2));
        assert_eq!(o.px[0], 255);
    }
    #[test]
    fn flipping_swaps_sides() {
        let o = orient(&tagged(), 0, 1);
        assert_eq!(o.px[0], 255);
        assert_eq!(orient(&tagged(), 3, 0).px[0], 255);
    }
}
