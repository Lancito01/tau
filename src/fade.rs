//! A fill of one or more colours, fading in order along a direction. No Win32, so `tests/fidelity.rs` can check it.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Direction { Across(f64, f64), FromCentre }

/// What the form offers, each with where the fade runs in fractions of the canvas: first colour at the start of the
/// arrow, last at its end. Diagonals run corner to corner, whatever the shape of the canvas.
pub const DIRECTIONS: [(&str, Direction); 9] = [
    ("Top left to bottom right", Direction::Across(1.0, 1.0)),
    ("Top to bottom", Direction::Across(0.0, 1.0)),
    ("Top right to bottom left", Direction::Across(-1.0, 1.0)),
    ("Left to right", Direction::Across(1.0, 0.0)),
    ("Right to left", Direction::Across(-1.0, 0.0)),
    ("Bottom left to top right", Direction::Across(1.0, -1.0)),
    ("Bottom to top", Direction::Across(0.0, -1.0)),
    ("Bottom right to top left", Direction::Across(-1.0, -1.0)),
    ("Centre to edges", Direction::FromCentre),
];

pub fn direction_named(name: &str) -> Direction {
    DIRECTIONS.iter().find(|(n, _)| *n == name).map(|(_, d)| *d).unwrap_or(DIRECTIONS[0].1)
}

/// How far along the fade a point is, from 0 to 1. `u` and `v` are fractions of the canvas.
fn position(d: Direction, u: f64, v: f64) -> f64 {
    match d {
        Direction::FromCentre => (u - 0.5).hypot(v - 0.5) / 0.5f64.hypot(0.5),
        Direction::Across(dx, dy) => {
            let axis = |t: f64, k: f64| if k > 0.0 { t } else { 1.0 - t };
            let sum = if dx != 0.0 { axis(u, dx) } else { 0.0 } + if dy != 0.0 { axis(v, dy) } else { 0.0 };
            sum / (dx.abs() + dy.abs())
        }
    }
}

/// Threshold from an 8x8 ordered dither, from 0 up to but not including 1. Slow fades would otherwise show as bands in
/// 8-bit. A value that is already whole stays exactly itself, so solid colours and the end colours are not disturbed.
fn dither(x: u32, y: u32) -> f32 {
    // A Bayer matrix: the bits of x and of x xor y, interleaved with the coarsest pair last.
    let (a, b) = (x & 7, (x ^ y) & 7);
    let mut m = 0u32;
    for i in 0..3 { m |= (((b >> i) & 1) << 1 | ((a >> i) & 1)) << (4 - 2 * i); }
    m as f32 / 64.0
}

/// A canvas of straight BGRA pixels. Any channel order works, since each channel is treated alike.
pub fn paint(w: u32, h: u32, stops: &[[u8; 4]], d: Direction) -> Vec<u8> {
    let mut px = vec![0u8; w as usize * h as usize * 4];
    if stops.is_empty() { return px; }
    let (xs, ys) = ((w.max(2) - 1) as f64, (h.max(2) - 1) as f64);
    let spans = (stops.len() - 1) as f64;
    for y in 0..h {
        for x in 0..w {
            let t = position(d, x as f64 / xs, y as f64 / ys).clamp(0.0, 1.0) * spans;
            let i = (t as usize).min(stops.len().saturating_sub(2));
            let f = (t - i as f64) as f32;
            let (a, b) = (stops[i], stops[(i + 1).min(stops.len() - 1)]);
            let at = (y as usize * w as usize + x as usize) * 4;
            let n = dither(x, y);
            for c in 0..4 {
                let v = a[c] as f32 + (b[c] as f32 - a[c] as f32) * f;
                px[at + c] = (v + n).floor().clamp(0.0, 255.0) as u8;
            }
        }
    }
    px
}

#[cfg(test)]
mod tests {
    use super::*;
    const RED: [u8; 4] = [0, 0, 255, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [255, 0, 0, 255];
    fn at(p: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] { let i = ((y * w + x) * 4) as usize; [p[i], p[i + 1], p[i + 2], p[i + 3]] }

    #[test]
    fn diagonal_runs_corner_to_corner_on_any_shape() {
        let p = paint(300, 100, &[RED, BLUE], direction_named("Top left to bottom right"));
        assert_eq!(at(&p, 300, 0, 0), RED);
        assert_eq!(at(&p, 300, 299, 99), BLUE);
        // The other two corners sit halfway.
        for c in [at(&p, 300, 299, 0), at(&p, 300, 0, 99)] { assert!((c[0] as i32 - 127).abs() <= 1 && (c[2] as i32 - 127).abs() <= 1, "{c:?}"); }
    }
    #[test]
    fn every_direction_starts_and_ends_where_it_says() {
        let (w, h) = (64, 48);
        let cases = [
            ("Top to bottom", (10, 0), (10, 47)), ("Bottom to top", (10, 47), (10, 0)),
            ("Left to right", (0, 10), (63, 10)), ("Right to left", (63, 10), (0, 10)),
            ("Top right to bottom left", (63, 0), (0, 47)), ("Bottom left to top right", (0, 47), (63, 0)),
            ("Bottom right to top left", (63, 47), (0, 0)),
        ];
        for (name, start, end) in cases {
            let p = paint(w, h, &[RED, BLUE], direction_named(name));
            assert_eq!(at(&p, w, start.0, start.1), RED, "{name} start");
            assert_eq!(at(&p, w, end.0, end.1), BLUE, "{name} end");
        }
    }
    #[test]
    fn colours_come_in_the_order_given() {
        let p = paint(201, 1, &[RED, GREEN, BLUE], direction_named("Left to right"));
        assert_eq!(at(&p, 201, 0, 0), RED);
        assert_eq!(at(&p, 201, 100, 0), GREEN, "the middle colour lands in the middle");
        assert_eq!(at(&p, 201, 200, 0), BLUE);
        let q = at(&p, 201, 50, 0);
        assert!(q[2] > 100 && q[1] > 100 && q[0] == 0, "red into green first: {q:?}");
        let q = at(&p, 201, 150, 0);
        assert!(q[0] > 100 && q[1] > 100 && q[2] == 0, "then green into blue: {q:?}");
        let backwards = paint(201, 1, &[BLUE, GREEN, RED], direction_named("Left to right"));
        assert_eq!(at(&backwards, 201, 0, 0), BLUE);
    }
    #[test]
    fn many_colours_each_get_their_place() {
        let stops = [RED, GREEN, BLUE, [0, 0, 0, 255], [255, 255, 255, 255]];
        let p = paint(401, 1, &stops, direction_named("Left to right"));
        for (i, s) in stops.iter().enumerate() { assert_eq!(&at(&p, 401, i as u32 * 100, 0), s); }
    }
    #[test]
    fn one_colour_is_exactly_that_colour_everywhere() {
        let c = [12, 200, 77, 255];
        assert!(paint(50, 50, &[c], Direction::FromCentre).chunks_exact(4).all(|p| p == c));
    }
    #[test]
    fn centre_fade_reaches_the_corners() {
        let p = paint(101, 101, &[RED, BLUE], Direction::FromCentre);
        assert_eq!(at(&p, 101, 50, 50), RED);
        assert_eq!(at(&p, 101, 0, 0), BLUE);
        assert_eq!(at(&p, 101, 100, 100), BLUE);
    }
    #[test]
    fn slow_fades_are_dithered_not_banded() {
        // Two levels apart across 1000 pixels would otherwise be three solid bands.
        let (a, b) = ([100, 100, 100, 255], [102, 102, 102, 255]);
        let p = paint(1000, 8, &[a, b], direction_named("Left to right"));
        let mean = |x0: u32| (0..8).flat_map(|y| (0..8).map(move |x| (x0 + x, y))).map(|(x, y)| at(&p, 1000, x, y)[0] as f32).sum::<f32>() / 64.0;
        // A patch's average tracks the true value far closer than whole levels could.
        for x0 in [100u32, 300, 500, 700, 900] {
            let want = 100.0 + 2.0 * (x0 as f32 + 3.5) / 999.0;
            assert!((mean(x0) - want).abs() < 0.08, "at {x0}: {} wanted {want}", mean(x0));
        }
    }
    #[test]
    fn dither_uses_each_threshold_once() {
        let mut seen: Vec<u32> = (0..8).flat_map(|y| (0..8).map(move |x| (dither(x, y) * 64.0) as u32)).collect();
        seen.sort();
        assert_eq!(seen, (0..64).collect::<Vec<_>>());
    }
    #[test]
    fn tiny_and_empty_do_not_panic() {
        assert_eq!(paint(1, 1, &[RED, BLUE], DIRECTIONS[0].1).len(), 4);
        assert_eq!(paint(3, 3, &[], DIRECTIONS[0].1).len(), 36);
    }
}
