//! What a redaction mark is, and painting one into a picture's own samples. No Win32, so the saved result and the
//! editor's preview run the same code, and `tests/fidelity.rs` can check it.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fill {
    /// RGB.
    Solid([u8; 3]),
    /// Gaussian blur with this sigma, in pixels.
    Blur(f64),
}

/// One mark in pixels. Feather reaches outward, so everything marked stays fully covered, however soft the edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mark { pub x: u32, pub y: u32, pub w: u32, pub h: u32, pub ellipse: bool, pub fill: Fill, pub feather: u32 }

impl Mark {
    pub fn is_hard_solid(&self) -> bool { self.feather == 0 && matches!(self.fill, Fill::Solid(_)) }

    /// How far outside the shape a pixel is, 0 when inside. Measured from the pixel's centre.
    fn outside(&self, px: f64, py: f64) -> f64 {
        let (x0, y0, x1, y1) = (self.x as f64, self.y as f64, (self.x + self.w) as f64, (self.y + self.h) as f64);
        let (cx, cy) = (px + 0.5, py + 0.5);
        let boxed = (x0 - cx).max(cx - x1).max(0.0).hypot((y0 - cy).max(cy - y1).max(0.0));
        if !self.ellipse { return boxed; }
        let (rx, ry) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
        let (dx, dy) = (cx - x0 - rx, cy - y0 - ry);
        let k = (dx / rx).powi(2) + (dy / ry).powi(2);
        if k <= 1.0 { return 0.0; }
        // First-order distance to the curve. It can never be less than the distance to the bounding box, which is what
        // keeps the feather inside `reach`.
        ((k - 1.0) / (2.0 * (dx / (rx * rx)).hypot(dy / (ry * ry)))).max(boxed)
    }

    /// 1 inside the shape, easing to 0 across the feather, and exactly 0 beyond it.
    pub fn coverage(&self, px: u32, py: u32) -> f32 {
        let d = self.outside(px as f64, py as f64);
        if d <= 0.0 { return 1.0; }
        if self.feather == 0 || d >= self.feather as f64 { return 0.0; }
        let t = 1.0 - d / self.feather as f64;
        (t * t * (3.0 - 2.0 * t)) as f32
    }

    /// The pixels this mark can touch, as x0, y0, x1, y1, clamped to the picture.
    pub fn reach(&self, w: u32, h: u32) -> (u32, u32, u32, u32) {
        (self.x.saturating_sub(self.feather), self.y.saturating_sub(self.feather),
         (self.x + self.w + self.feather).min(w), (self.y + self.h + self.feather).min(h))
    }
}

/// How samples sit in a buffer. `rgb` holds the sample index of red, green and blue; grey repeats one index.
#[derive(Clone, Copy, Debug)]
pub struct Layout { pub w: u32, pub h: u32, pub stride: usize, pub sample_bytes: usize, pub channels: usize, pub rgb: [usize; 3], pub alpha: Option<usize> }

impl Layout {
    fn max(&self) -> f32 { if self.sample_bytes == 1 { 255.0 } else { 65535.0 } }
    fn at(&self, x: u32, y: u32, c: usize) -> usize { y as usize * self.stride + (x as usize * self.channels + c) * self.sample_bytes }
    fn read(&self, b: &[u8], i: usize) -> f32 {
        if self.sample_bytes == 1 { b[i] as f32 } else { u16::from_le_bytes([b[i], b[i + 1]]) as f32 }
    }
    fn write(&self, b: &mut [u8], i: usize, v: f32) {
        let v = v.round().clamp(0.0, self.max());
        if self.sample_bytes == 1 { b[i] = v as u8; } else { b[i..i + 2].copy_from_slice(&(v as u16).to_le_bytes()); }
    }
    fn is_grey(&self) -> bool { self.rgb[0] == self.rgb[1] && self.rgb[1] == self.rgb[2] }
}

fn kernel(sigma: f64) -> Vec<f32> {
    let radius = (sigma * 3.0).ceil().max(1.0) as i32;
    let raw: Vec<f64> = (-radius..=radius).map(|i| (-(i as f64 * i as f64) / (2.0 * sigma * sigma)).exp()).collect();
    let sum: f64 = raw.iter().sum();
    raw.into_iter().map(|v| (v / sum) as f32).collect()
}

/// A blurred copy of one channel of a region. Pixels past the picture's edge repeat the last one.
fn blur(src: &[f32], w: usize, h: usize, k: &[f32]) -> Vec<f32> {
    let radius = (k.len() / 2) as i32;
    let mut across = vec![0f32; w * h];
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        for x in 0..w {
            across[y * w + x] = k.iter().enumerate().map(|(i, g)| g * row[(x as i32 + i as i32 - radius).clamp(0, w as i32 - 1) as usize]).sum();
        }
    }
    let mut out = vec![0f32; w * h];
    for x in 0..w {
        for y in 0..h {
            out[y * w + x] = k.iter().enumerate().map(|(i, g)| g * across[(y as i32 + i as i32 - radius).clamp(0, h as i32 - 1) as usize * w + x]).sum();
        }
    }
    out
}

/// Paints the marks in order. A sample is only written where a mark covers it, so the rest stay bit for bit.
pub fn apply(buf: &mut [u8], l: &Layout, marks: &[Mark]) {
    apply_within(buf, l, marks, (0, 0, l.w, l.h));
}

/// A part of a picture as x0, y0, x1, y1.
pub type Area = (u32, u32, u32, u32);

fn blur_pad(sigma: f64) -> u32 { (sigma.max(0.5) * 3.0).ceil() as u32 }

fn grown(a: Area, by: u32, w: u32, h: u32) -> Area {
    (a.0.saturating_sub(by), a.1.saturating_sub(by), (a.2 + by).min(w), (a.3 + by).min(h))
}

fn overlap(a: Area, b: Area) -> Option<Area> {
    let o = (a.0.max(b.0), a.1.max(b.1), a.2.min(b.2), a.3.min(b.3));
    (o.0 < o.2 && o.1 < o.3).then_some(o)
}

/// What going from one list of marks to another means for a painted picture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Change {
    /// The only pixels whose final value can differ.
    pub shown: Area,
    /// What has to be painted again to get `shown` exactly right, since a blur reads what lies around it.
    pub work: Area,
    /// A blur is part of it, which is slow.
    pub blur: bool,
}

/// `None` when the two lists paint the same picture.
pub fn change_between(old: &[Mark], new: &[Mark], w: u32, h: u32) -> Option<Change> {
    let mut shown: Option<Area> = None;
    for i in 0..old.len().max(new.len()) {
        if old.get(i) == new.get(i) { continue; }
        for m in [old.get(i), new.get(i)].into_iter().flatten() {
            let a = m.reach(w, h);
            shown = Some(shown.map_or(a, |s| (s.0.min(a.0), s.1.min(a.1), s.2.max(a.2), s.3.max(a.3))));
        }
    }
    let mut shown = shown?;
    let (mut margin, mut blur) = (0, false);
    // A blur spreads a change by its own radius, and each later one spreads that again.
    for m in new {
        let Fill::Blur(sigma) = m.fill else { continue };
        let pad = blur_pad(sigma);
        if let Some(o) = overlap(grown(shown, pad, w, h), m.reach(w, h)) {
            shown = (shown.0.min(o.0), shown.1.min(o.1), shown.2.max(o.2), shown.3.max(o.3));
            margin += pad;
            blur = true;
        }
    }
    Some(Change { shown, work: grown(shown, margin, w, h), blur })
}

/// The same, writing and reading only inside `clip`. Everything further than the blur radii from the edge of `clip` comes
/// out exactly as painting the whole picture would.
pub fn apply_within(buf: &mut [u8], l: &Layout, marks: &[Mark], clip: Area) {
    for m in marks {
        let Some((x0, y0, x1, y1)) = overlap(m.reach(l.w, l.h), clip) else { continue };
        match m.fill {
            Fill::Solid(rgb) => {
                let scale = l.max() / 255.0;
                let wanted: Vec<f32> = (0..l.channels).map(|c| {
                    if Some(c) == l.alpha { l.max() }
                    else if l.is_grey() { (0.299 * rgb[0] as f32 + 0.587 * rgb[1] as f32 + 0.114 * rgb[2] as f32) * scale }
                    else { l.rgb.iter().position(|r| *r == c).map(|i| rgb[i] as f32 * scale).unwrap_or(0.0) }
                }).collect();
                for y in y0..y1 {
                    for x in x0..x1 {
                        let a = m.coverage(x, y);
                        if a <= 0.0 { continue; }
                        for (c, want) in wanted.iter().enumerate() {
                            let i = l.at(x, y, c);
                            let old = l.read(buf, i);
                            l.write(buf, i, old + (want - old) * a);
                        }
                    }
                }
            }
            Fill::Blur(sigma) => {
                // The blur gathers from past the mark, so nothing sharp can survive at its edge.
                let sigma = sigma.max(0.5);
                let pad = blur_pad(sigma);
                let (rx0, ry0, rx1, ry1) = (x0.saturating_sub(pad).max(clip.0), y0.saturating_sub(pad).max(clip.1), (x1 + pad).min(clip.2), (y1 + pad).min(clip.3));
                let (rw, rh) = ((rx1 - rx0) as usize, (ry1 - ry0) as usize);
                let k = kernel(sigma);
                for c in 0..l.channels {
                    let mut src = Vec::with_capacity(rw * rh);
                    for y in ry0..ry1 { for x in rx0..rx1 { src.push(l.read(buf, l.at(x, y, c))); } }
                    let soft = blur(&src, rw, rh, &k);
                    for y in y0..y1 {
                        for x in x0..x1 {
                            let a = m.coverage(x, y);
                            if a <= 0.0 { continue; }
                            let at = (y - ry0) as usize * rw + (x - rx0) as usize;
                            l.write(buf, l.at(x, y, c), src[at] + (soft[at] - src[at]) * a);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn painting_only_the_change_matches_painting_everything() {
        let (w, h) = (160, 120);
        let (base, l) = bgra(w, h);
        let solid = |x, y, feather| Mark { x, y, w: 30, h: 24, ellipse: false, fill: Fill::Solid([200, 30, 40]), feather };
        let soft = |x, y| Mark { x, y, w: 40, h: 30, ellipse: true, fill: Fill::Blur(4.0), feather: 3 };
        let cases: Vec<(Vec<Mark>, Vec<Mark>)> = vec![
            (vec![], vec![solid(10, 10, 0)]),
            (vec![solid(10, 10, 0)], vec![solid(14, 12, 0)]),
            (vec![solid(10, 10, 6), solid(80, 60, 0)], vec![solid(10, 10, 12), solid(80, 60, 0)]),
            (vec![soft(30, 20), solid(60, 30, 0)], vec![soft(30, 20), solid(50, 34, 0)]),
            (vec![solid(40, 30, 0), soft(30, 20)], vec![solid(44, 30, 0), soft(30, 20)]),
            (vec![solid(40, 30, 0), soft(30, 20), soft(50, 30)], vec![solid(44, 30, 0), soft(30, 20), soft(50, 30)]),
            (vec![solid(10, 10, 0), soft(90, 70)], vec![soft(90, 70)]),
        ];
        for (old, new) in cases {
            let mut shown = base.clone();
            apply(&mut shown, &l, &old);
            let mut whole = base.clone();
            apply(&mut whole, &l, &new);
            let c = change_between(&old, &new, w, h).expect("these lists differ");
            // A scratch copy with nothing painted in the work area, as the editor keeps one.
            let mut scratch = base.clone();
            apply_within(&mut scratch, &l, &new, c.work);
            for y in c.shown.1..c.shown.3 {
                for x in c.shown.0..c.shown.2 {
                    let i = ((y * w + x) * 4) as usize;
                    shown[i..i + 4].copy_from_slice(&scratch[i..i + 4]);
                }
            }
            assert_eq!(shown, whole, "{old:?} to {new:?}");
        }
        assert_eq!(change_between(&[solid(1, 1, 0)], &[solid(1, 1, 0)], w, h), None);
        let far = change_between(&[soft(100, 80)], &[soft(100, 80), solid(2, 2, 0)], w, h).unwrap();
        assert!(!far.blur, "a blur nowhere near the change costs nothing");
        assert!(change_between(&[soft(30, 20), solid(60, 30, 0)], &[soft(30, 20), solid(50, 34, 0)], w, h).unwrap().blur);
    }

    fn bgra(w: u32, h: u32) -> (Vec<u8>, Layout) {
        let mut b = vec![0u8; (w * h * 4) as usize];
        for y in 0..h { for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            b[i] = ((x * 7 + y * 3) % 256) as u8;
            b[i + 1] = ((x * 13) % 256) as u8;
            b[i + 2] = ((y * 11) % 256) as u8;
            b[i + 3] = 255;
        } }
        (b, Layout { w, h, stride: (w * 4) as usize, sample_bytes: 1, channels: 4, rgb: [2, 1, 0], alpha: Some(3) })
    }
    fn mark(ellipse: bool, fill: Fill, feather: u32) -> Mark { Mark { x: 40, y: 30, w: 40, h: 20, ellipse, fill, feather } }
    fn px<'a>(b: &'a [u8], l: &Layout, x: u32, y: u32) -> &'a [u8] { let i = l.at(x, y, 0); &b[i..i + l.channels * l.sample_bytes] }

    fn outside_is_untouched(m: Mark, before: &[u8], after: &[u8], l: &Layout) {
        let (x0, y0, x1, y1) = m.reach(l.w, l.h);
        for y in 0..l.h { for x in 0..l.w {
            if x < x0 || x >= x1 || y < y0 || y >= y1 { assert_eq!(px(before, l, x, y), px(after, l, x, y), "{x},{y}"); }
        } }
    }

    #[test]
    fn solid_box_paints_its_colour_and_nothing_else() {
        let (mut b, l) = bgra(120, 80);
        let before = b.clone();
        let m = mark(false, Fill::Solid([200, 100, 50]), 0);
        apply(&mut b, &l, &[m]);
        assert_eq!(px(&b, &l, 40, 30), &[50, 100, 200, 255]);
        assert_eq!(px(&b, &l, 79, 49), &[50, 100, 200, 255]);
        assert_eq!(px(&b, &l, 39, 30), px(&before, &l, 39, 30));
        assert_eq!(px(&b, &l, 80, 30), px(&before, &l, 80, 30));
        outside_is_untouched(m, &before, &b, &l);
    }
    #[test]
    fn ellipse_leaves_the_corners_of_its_box_alone() {
        let (mut b, l) = bgra(120, 80);
        let before = b.clone();
        apply(&mut b, &l, &[mark(true, Fill::Solid([0, 0, 0]), 0)]);
        assert_eq!(px(&b, &l, 60, 40), &[0, 0, 0, 255]);
        assert_eq!(px(&b, &l, 40, 30), px(&before, &l, 40, 30), "corner");
        assert_eq!(px(&b, &l, 79, 49), px(&before, &l, 79, 49), "corner");
    }
    #[test]
    fn feather_keeps_the_mark_covered_and_fades_outside_it() {
        let (mut b, l) = bgra(120, 80);
        let before = b.clone();
        let m = mark(false, Fill::Solid([0, 0, 0]), 10);
        apply(&mut b, &l, &[m]);
        assert_eq!(px(&b, &l, 40, 30), &[0, 0, 0, 255], "the marked area is fully covered right to its edge");
        assert!(m.coverage(35, 40) > m.coverage(32, 40) && m.coverage(32, 40) > 0.0);
        assert_eq!(m.coverage(29, 40), 0.0);
        assert_eq!(px(&b, &l, 29, 40), px(&before, &l, 29, 40));
        outside_is_untouched(m, &before, &b, &l);
    }
    #[test]
    fn a_long_ellipse_feathers_evenly_and_stays_in_reach() {
        let m = Mark { x: 100, y: 100, w: 120, h: 20, ellipse: true, fill: Fill::Solid([0, 0, 0]), feather: 16 };
        // Past each tip and past each side, by the same distance from the edge, about the same coverage.
        let (along, across) = (m.coverage(220 + 8, 110), m.coverage(160, 120 + 8));
        assert!((along - across).abs() < 0.15, "{along} vs {across}");
        for y in 0..240 { for x in 0..320 {
            let (x0, y0, x1, y1) = m.reach(320, 240);
            if x < x0 || x >= x1 || y < y0 || y >= y1 { assert_eq!(m.coverage(x, y), 0.0, "{x},{y}"); }
        } }
    }
    #[test]
    fn blur_smooths_inside_and_is_exact_outside() {
        let (mut b, l) = bgra(120, 80);
        let before = b.clone();
        let m = mark(false, Fill::Blur(6.0), 5);
        apply(&mut b, &l, &[m]);
        outside_is_untouched(m, &before, &b, &l);
        // Neighbouring samples differ far less once blurred.
        let step = |d: &[u8]| (41..79).map(|x| (px(d, &l, x, 40)[1] as i32 - px(d, &l, x + 1, 40)[1] as i32).abs()).sum::<i32>();
        assert!(step(&b) * 5 < step(&before), "{} vs {}", step(&b), step(&before));
        assert_eq!(px(&b, &l, 60, 40)[3], 255, "alpha stays");
    }
    #[test]
    fn sixteen_bit_keeps_its_depth() {
        let (w, h) = (64u32, 48u32);
        let mut b = vec![0u8; (w * h * 6) as usize];
        for (i, v) in b.chunks_exact_mut(2).enumerate() { v.copy_from_slice(&((i as u32 * 257 % 65535) as u16).to_le_bytes()); }
        let l = Layout { w, h, stride: (w * 6) as usize, sample_bytes: 2, channels: 3, rgb: [0, 1, 2], alpha: None };
        let before = b.clone();
        let m = Mark { x: 10, y: 10, w: 20, h: 10, ellipse: false, fill: Fill::Solid([255, 0, 128]), feather: 0 };
        apply(&mut b, &l, &[m]);
        let got: Vec<u16> = px(&b, &l, 15, 15).chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(got, vec![65535, 0, 32896]);
        outside_is_untouched(m, &before, &b, &l);
    }
    #[test]
    fn grey_takes_the_luma_of_the_colour() {
        let mut b = vec![90u8; 100];
        let l = Layout { w: 10, h: 10, stride: 10, sample_bytes: 1, channels: 1, rgb: [0, 0, 0], alpha: None };
        apply(&mut b, &l, &[Mark { x: 0, y: 0, w: 10, h: 10, ellipse: false, fill: Fill::Solid([255, 255, 255]), feather: 0 }]);
        assert!(b.iter().all(|v| *v == 255));
    }
    #[test]
    fn marks_off_the_edge_do_not_panic() {
        let (mut b, l) = bgra(50, 50);
        apply(&mut b, &l, &[Mark { x: 45, y: 45, w: 20, h: 20, ellipse: true, fill: Fill::Blur(4.0), feather: 8 }, Mark { x: 60, y: 60, w: 5, h: 5, ellipse: false, fill: Fill::Solid([1, 2, 3]), feather: 0 }]);
    }
}
