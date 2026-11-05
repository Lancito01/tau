//! Generates the app icon (no binary assets in the repo) and embeds it with the manifest.
use std::path::Path;

/// Distance from a point to a line segment.
fn seg(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((p.0 - a.0 - dx * t).powi(2) + (p.1 - a.1 - dy * t).powi(2)).sqrt()
}

/// One icon image: a signal disc with a dark tau. Same geometry as gfx::draw_logo. BGRA, top-down.
fn draw(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut px = vec![0u8; (size * size * 4) as usize];
    let cover = |d: f32| (0.5 - d).clamp(0.0, 1.0);
    // Tiny icons get a heavier stroke so the glyph survives the tray.
    let stroke = if size <= 24 { 0.12 } else { 0.09 } * s / 2.0;
    let mut tau = vec![((0.27, 0.36), (0.75, 0.36)), ((0.50, 0.36), (0.50, 0.60))];
    let curve = |t: f32| ((1.0 - t).powi(2) * 0.50 + 2.0 * t * (1.0 - t) * 0.50 + t * t * 0.64, (1.0 - t).powi(2) * 0.60 + 2.0 * t * (1.0 - t) * 0.72 + t * t * 0.72);
    for i in 0..10 { tau.push((curve(i as f32 / 10.0), curve((i + 1) as f32 / 10.0))); }
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let r = ((fx - s / 2.0).powi(2) + (fy - s / 2.0).powi(2)).sqrt();
            let disc = cover(r - s * 0.5 + 0.5);
            let d = tau.iter().map(|&(a, b)| seg((fx, fy), (a.0 * s, a.1 * s), (b.0 * s, b.1 * s))).fold(f32::MAX, f32::min);
            let ink = cover(d - stroke);
            // BGR of #C8F135 and #12140A.
            let c: [f32; 3] = std::array::from_fn(|i| { let (bg, fg) = ([0x35, 0xF1, 0xC8][i] as f32, [0x0A, 0x14, 0x12][i] as f32); bg + (fg - bg) * ink });
            let o = ((y * size + x) * 4) as usize;
            px[o] = c[0] as u8; px[o + 1] = c[1] as u8; px[o + 2] = c[2] as u8;
            px[o + 3] = (disc * 255.0) as u8;
        }
    }
    px
}

/// ICO with uncompressed 32-bit DIB images.
fn ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&n| {
        let px = draw(n);
        let mask = ((n + 31) / 32 * 4 * n) as usize;
        let mut d = Vec::with_capacity(40 + px.len() + mask);
        for v in [40u32, n, n * 2] { d.extend_from_slice(&v.to_le_bytes()); }
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&32u16.to_le_bytes());
        d.extend_from_slice(&[0u8; 24]);
        // DIB rows run bottom-up.
        for row in px.chunks_exact((n * 4) as usize).rev() { d.extend_from_slice(row); }
        d.extend(std::iter::repeat_n(0u8, mask));
        d
    }).collect();
    let mut out = vec![0u8, 0, 1, 0];
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (n, im) in sizes.iter().zip(&images) {
        let b = if *n >= 256 { 0 } else { *n as u8 };
        out.extend_from_slice(&[b, b, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(im.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += im.len() as u32;
    }
    for im in images { out.extend(im); }
    out
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=tau.manifest");
    println!("cargo:rerun-if-env-changed=TAU_VERSION");
    println!("cargo:rerun-if-env-changed=GITHUB_REPOSITORY");
    let out = std::env::var("OUT_DIR").unwrap();
    let icon = Path::new(&out).join("tau.ico");
    let bytes = ico(&[16, 20, 24, 32, 48, 64, 256]);
    std::fs::write(&icon, &bytes).unwrap();
    // Kept for the installer, which needs the same icon.
    std::fs::write(Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("target").join("tau.ico"), &bytes).ok();
    let manifest = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("tau.manifest");
    let esc = |p: &Path| p.display().to_string().replace('\\', "\\\\");
    let rc = Path::new(&out).join("tau.rc");
    std::fs::write(&rc, format!("1 ICON \"{}\"\n1 24 \"{}\"\n", esc(&icon), esc(&manifest))).unwrap();
    embed_resource::compile(&rc, embed_resource::NONE).manifest_required().unwrap();
}
