//! Generates the app icon (no binary assets in the repo) and embeds it with the manifest.
use std::path::Path;

/// One icon image: a dark disc, a thin orbit ring, and a violet node. Premultiply-free BGRA, top-down.
fn draw(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut px = vec![0u8; (size * size * 4) as usize];
    let cover = |d: f32| (0.5 - d).clamp(0.0, 1.0);
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let r = ((fx - s / 2.0).powi(2) + (fy - s / 2.0).powi(2)).sqrt();
            let disc = cover(r - s * 0.48);
            let mut c = [0x1Au8 as f32, 0x11 as f32, 0x10 as f32];
            let ring = cover((r - s * 0.32).abs() - (s * 0.018).max(0.6));
            for (i, v) in [0x6Au8, 0x45, 0x3Au8].iter().enumerate() { c[i] += (*v as f32 - c[i]) * ring; }
            let hub = cover(r - s * 0.11);
            for (i, v) in [0xFAu8, 0xF4, 0xF3].iter().enumerate() { c[i] += (*v as f32 - c[i]) * hub; }
            let (nx, ny) = (s / 2.0 + s * 0.226, s / 2.0 - s * 0.226);
            let node = cover(((fx - nx).powi(2) + (fy - ny).powi(2)).sqrt() - s * 0.115);
            for (i, v) in [0xFFu8, 0x4A, 0x6D].iter().enumerate() { c[i] += (*v as f32 - c[i]) * node; }
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
    println!("cargo:rerun-if-changed=orrery.manifest");
    println!("cargo:rerun-if-env-changed=ORRERY_VERSION");
    println!("cargo:rerun-if-env-changed=GITHUB_REPOSITORY");
    let out = std::env::var("OUT_DIR").unwrap();
    let icon = Path::new(&out).join("orrery.ico");
    let bytes = ico(&[16, 20, 24, 32, 48, 64, 256]);
    std::fs::write(&icon, &bytes).unwrap();
    // Kept for the installer, which needs the same icon.
    std::fs::write(Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("target").join("orrery.ico"), &bytes).ok();
    let manifest = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("orrery.manifest");
    let esc = |p: &Path| p.display().to_string().replace('\\', "\\\\");
    let rc = Path::new(&out).join("orrery.rc");
    std::fs::write(&rc, format!("1 ICON \"{}\"\n1 24 \"{}\"\n", esc(&icon), esc(&manifest))).unwrap();
    embed_resource::compile(&rc, embed_resource::NONE).manifest_required().unwrap();
}
