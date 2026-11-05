//! Sample data behind `--render-ui`, so every window can be looked at without clicking through to it.
use crate::canvas::{self, Fx, Mode, Shape, Tool};
use crate::form::{self, choice, text, Field};
use crate::image::Bmp;
use crate::timeline;
use crate::ui::{self, glyph, Menu, MenuItem};
use std::path::{Path, PathBuf};

/// A soft landscape, so previews look like a picture and not a test card.
fn sample_photo(w: u32, h: u32) -> Bmp {
    let mut b = Bmp::new(w, h, [0, 0, 0, 255]);
    for y in 0..h {
        for x in 0..w {
            let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
            let hill = 0.62 + 0.10 * (u * 7.0).sin() + 0.04 * (u * 19.0).sin();
            let sun = ((u - 0.72).powi(2) * 3.0 + (v - 0.28).powi(2) * 5.0).sqrt();
            let (mut re, mut gr, mut bl) = (60.0 + 150.0 * v, 110.0 + 90.0 * v, 190.0 - 30.0 * v);
            if sun < 0.12 { re = 250.0; gr = 225.0; bl = 170.0; }
            if v > hill { let d = (v - hill) * 2.0; re = 34.0 + 40.0 * d; gr = 70.0 + 30.0 * d; bl = 62.0; }
            let i = ((y * w + x) * 4) as usize;
            b.px[i] = bl as u8;
            b.px[i + 1] = gr as u8;
            b.px[i + 2] = re as u8;
        }
    }
    b
}

fn sample_strip() -> Bmp {
    let mut out = Bmp::new(1920, 90, [0x12, 0x10, 0x0F, 255]);
    for i in 0..12 {
        let mut f = sample_photo(160, 90);
        for p in f.px.chunks_exact_mut(4) { let k = 0.7 + 0.05 * i as f32; p[0] = (p[0] as f32 * k) as u8; p[1] = (p[1] as f32 * k) as u8; }
        out.blit(&f, i * 160, 0);
    }
    out
}

pub fn render(name: &str, out: &Path) -> Result<(), String> {
    let photo = || sample_photo(1600, 1000);
    match name {
        "form" => form::render("Compress video", "interview take 3.mp4", &[
            choice("Quality", &["Balanced", "Smaller file", "Higher quality"], 0),
            choice("Height", &["Keep size", "1080", "720", "480"], 0),
        ], "Compress", out),
        "form-long" => form::render("PDF metadata", "contract.pdf", &[
            text("Title", "Service agreement"), text("Author", ""), text("Subject", ""),
            Field::Check("Remove all existing metadata first", true),
        ], "Save", out),
        "form-color" => form::render("Collage", "6 files", &[
            choice("Columns", &["Auto", "2", "3", "4", "5"], 0),
            Field::Slider("Gap (pixels)", 0, 80, 12),
            Field::Color("Background", "#FFFFFF".into()),
        ], "Create", out),
        "settings" => crate::settings::render(out),
        "menu" => {
            let items = vec![MenuItem::new("Settings", glyph::SETTINGS), MenuItem::new("Check for updates", glyph::SYNC), MenuItem::separator(), MenuItem::new("Quit Tau", glyph::POWER)];
            ui::render_png(&mut Menu::new(230.0, items, None, Some(format!("Tau {}", crate::VERSION))), 1.0, Some((100.0, 90.0)), out)
        }
        "hud" => crate::hud::render(out),
        "shelf" => crate::shelf::render(["C:\\a\\holiday.jpg", "C:\\a\\interview take 3.mp4", "C:\\a\\voice note.m4a", "C:\\a\\contract.pdf", "C:\\a\\Reports"].iter().map(PathBuf::from).collect(), out),
        "shelf-empty" => crate::shelf::render(vec![], out),
        "redact" => canvas::render(Mode::Redact, photo(), vec![
            Shape::rect(0.12, 0.2, 0.3, 0.32),
            Shape { tool: Tool::Ellipse, a: (0.55, 0.62), b: (0.8, 0.8), color: 0xFF3B30, text: String::new(), fx: Fx { feather: 12.0, ..Fx::default() } },
            Shape { tool: Tool::Ellipse, a: (0.4, 0.1), b: (0.56, 0.3), color: 0, text: String::new(), fx: Fx { blur: true, strength: 50.0, feather: 20.0, width: 1.0 } },
        ], 2, None, out),
        "redact-video" => canvas::render(Mode::Redact, photo(), vec![Shape::rect(0.55, 0.62, 0.8, 0.7)], 0, Some(192.0), out),
        "crop" => canvas::render(Mode::Crop, photo(), vec![Shape::rect(0.15, 0.1, 0.85, 0.8)], 0, None, out),
        "annotate" => canvas::render(Mode::Annotate, photo(), vec![
            Shape { tool: Tool::Rect, a: (0.1, 0.15), b: (0.42, 0.5), color: 0xFF3B30, text: String::new(), fx: Fx::default() },
            Shape { tool: Tool::Arrow, a: (0.7, 0.75), b: (0.5, 0.45), color: 0xFFCC00, text: String::new(), fx: Fx::default() },
            Shape { tool: Tool::Text, a: (0.5, 0.2), b: (0.5, 0.2), color: 0xFFFFFF, text: "Sunset here".into(), fx: Fx::default() },
        ], 1, None, out),
        "timeline-range" => timeline::render(timeline::Mode::Range, Some(sample_strip()), vec![24.0, 140.0], out),
        "timeline-points" => timeline::render(timeline::Mode::Points, Some(sample_strip()), vec![30.0, 95.0, 150.0], out),
        "timeline-ranges" => timeline::render(timeline::Mode::Ranges, Some(sample_strip()), vec![12.0, 30.0, 70.0, 88.0, 150.0], out),
        "photo" => crate::photo::render(photo(), out),
        "compress" => {
            let base = photo();
            let spec = crate::preview::Spec {
                title: "Compress video".into(), ok: "Compress".into(),
                levels: ["Smaller file", "Balanced", "Higher quality"].iter().map(|s| s.to_string()).collect(), default: 1,
                option_label: "Height".into(), options: ["Keep size", "1080", "720"].iter().map(|s| s.to_string()).collect(),
                sampler: std::sync::Arc::new(move |i, _| Ok((base.clone(), [84_000_000, 12_000_000, 26_000_000, 51_000_000][i]))),
            };
            crate::preview::render(spec, out)
        }
        _ => Err(format!("No sample called {name}")),
    }
}
