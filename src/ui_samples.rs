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
        "background" => {
            let names: Vec<String> = crate::fade::DIRECTIONS.iter().map(|(n, _)| n.to_string()).collect();
            let fields = [
                Field::Colors("Colours. Two or more fade in this order", vec!["#2563EB".to_string(), "#E11D48".to_string(), "#F2EFE8".to_string()]),
                Field::ChoiceIfMany(0, "Fade direction", names, 0),
                Field::Slider("Margin (% of the longest side)", 0, 50, 8),
                Field::Slider("Corner radius (% of the shortest side)", 0, 50, 12),
            ];
            let base = sample_photo(800, 500);
            form::render_with_preview("Add background", "holiday.jpg", &fields, "Apply", Box::new(move |v| {
                let stops: Vec<[u8; 4]> = v[0].split(',').map(crate::image::parse_color).collect();
                Some(crate::tools::background_preview(&base, &stops, crate::fade::direction_named(&v[1]), v[2].parse().unwrap_or(0), v[3].parse().unwrap_or(0)))
            }), out)
        }
        "metadata" => {
            use crate::metadata::{Info, Section};
            let rows = |r: &[(&str, &str)]| r.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<Vec<_>>();
            let info = Info { editable: vec![], sections: vec![
                Section { title: "Summary".into(), rows: rows(&[("Format", "JPG"), ("Dimensions", "6000 x 4000"), ("Camera make", "Canon"), ("Camera model", "EOS R6"), ("Date taken", "2024:06:14 18:22:07"), ("Location", "-34.60370, -58.38160")]) },
                Section { title: "File".into(), rows: rows(&[("Name", "holiday.jpg"), ("Size", "6.2 MB (6500000 bytes)")]) },
                Section { title: "Image".into(), rows: rows(&[("Camera make", "Canon"), ("Orientation", "Upright")]) },
                Section { title: "Exposure".into(), rows: rows(&[("Exposure time", "1/250"), ("F-number", "2.8"), ("ISO", "400")]) },
                Section { title: "GPS".into(), rows: rows(&[("Latitude", "34, 36, 13.3")]) },
                Section { title: "XMP".into(), rows: rows(&[("Creator tool", "Lightroom")]) },
            ] };
            crate::metadata::render("holiday.jpg", info, &[], out)
        }
        "metadata-open" => {
            use crate::metadata::{Info, Section};
            let rows = |r: &[(&str, &str)]| r.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<Vec<_>>();
            let info = Info { editable: vec![], sections: vec![
                Section { title: "Summary".into(), rows: rows(&[("Format", "QuickTime / MOV"), ("Length", "3:12.04"), ("Video", "h264, 1920x1080, 29.97 fps")]) },
                Section { title: "File".into(), rows: rows(&[("Name", "interview take 3.mp4"), ("Folder", "C:\\Users\\Someone\\Videos\\A folder with quite a long name that has to wrap somewhere"), ("Size", "84.0 MB")]) },
                Section { title: "Video 1".into(), rows: rows(&[("Codec name", "h264"), ("Profile", "High"), ("Width", "1920"), ("Height", "1080"), ("Pix fmt", "yuv420p"), ("Color space", "bt709"), ("Language", "eng")]) },
                Section { title: "Audio 1".into(), rows: rows(&[("Codec name", "aac")]) },
            ] };
            crate::metadata::render("interview take 3.mp4", info, &[1, 2], out)
        }
        "background-one" => {
            let names: Vec<String> = crate::fade::DIRECTIONS.iter().map(|(n, _)| n.to_string()).collect();
            let fields = [
                Field::Colors("Colours. Two or more fade in this order", vec!["#F2EFE8".to_string()]),
                Field::ChoiceIfMany(0, "Fade direction", names, 0),
                Field::Slider("Margin (% of the longest side)", 0, 50, 5),
                Field::Slider("Corner radius (% of the shortest side)", 0, 50, 0),
            ];
            let base = sample_photo(800, 500);
            form::render_with_preview("Add background", "holiday.jpg", &fields, "Apply", Box::new(move |v| {
                let stops: Vec<[u8; 4]> = v[0].split(',').map(crate::image::parse_color).collect();
                Some(crate::tools::background_preview(&base, &stops, crate::fade::direction_named(&v[1]), v[2].parse().unwrap_or(0), v[3].parse().unwrap_or(0)))
            }), out)
        }
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
            Shape { fx: Fx { feather: 12.0, ..Fx::default() }, ..Shape::new(Tool::Ellipse, (0.55, 0.62), (0.8, 0.8), 0xFF3B30) },
            Shape { fx: Fx { blur: true, strength: 50.0, feather: 20.0, ..Fx::default() }, ..Shape::new(Tool::Ellipse, (0.4, 0.1), (0.56, 0.3), 0) },
        ], 2, None, out),
        "redact-video" => canvas::render(Mode::Redact, photo(), vec![Shape::rect(0.55, 0.62, 0.8, 0.7)], 0, Some(192.0), out),
        "crop" => canvas::render(Mode::Crop, photo(), vec![Shape::rect(0.15, 0.1, 0.85, 0.8)], 0, None, out),
        "annotate" => canvas::render(Mode::Annotate, photo(), vec![
            Shape { fx: Fx { fill: 25.0, ..Fx::default() }, ..Shape::new(Tool::Rect, (0.1, 0.15), (0.42, 0.5), 0xFF3B30) },
            Shape { bend: Some((0.68, 0.52)), ..Shape::new(Tool::Arrow, (0.75, 0.8), (0.5, 0.45), 0xFFCC00) },
            Shape { text: "Sunset here".into(), fx: Fx { fill: 60.0, ..Fx::default() }, ..Shape::new(Tool::Text, (0.5, 0.2), (0.5, 0.2), 0xFFFFFF) },
        ], 1, None, out),
        "annotate-video" => canvas::render(Mode::Annotate, photo(), vec![
            Shape { fx: Fx { width: 1.5, ..Fx::default() }, ..Shape::new(Tool::Ellipse, (0.62, 0.15), (0.84, 0.42), 0xFFCC00) },
        ], 0, Some(192.0), out),
        "timeline-range" => timeline::render(timeline::Mode::Range, Some(sample_strip()), vec![24.0, 140.0], 30.0, Some(sample_photo(800, 450)), out),
        "timeline-points" => timeline::render(timeline::Mode::Points, Some(sample_strip()), vec![30.0, 95.0, 150.0], 29.97, Some(sample_photo(800, 450)), out),
        "timeline-ranges" => timeline::render(timeline::Mode::Ranges, Some(sample_strip()), vec![12.0, 30.0, 70.0, 88.0, 150.0], 0.0, None, out),
        "photo" => crate::photo::render(photo(), out),
        "compress" | "compress-custom" => {
            use crate::preview::{Control, Preset};
            let base = photo();
            let level = |name: &str, values: Vec<i32>| Preset { name: name.to_string(), values };
            let options = |o: &[(&str, i32)]| o.iter().map(|(n, v)| (n.to_string(), *v)).collect::<Vec<_>>();
            let spec = crate::preview::Spec {
                title: "Compress video".into(), ok: "Compress".into(),
                presets: vec![
                    level("Very high quality", vec![94, 0, 0, 0]), level("High quality", vec![80, 0, 256, 0]),
                    level("Medium quality", vec![66, 1080, 160, 0]), level("Low quality, 720p", vec![54, 720, 96, 0]),
                ],
                default: 1,
                controls: vec![
                    Control::Slider { label: "Picture quality".into(), min: 0, max: 100, unit: "%" },
                    Control::Choice { label: "Resolution".into(), options: options(&[("Keep size", 0), ("720p", 720), ("480p", 480)]) },
                    Control::Choice { label: "Audio".into(), options: options(&[("Keep as it is", 0), ("96 kbps", 96), ("8 kbps", 8)]) },
                    Control::Choice { label: "Frame rate".into(), options: options(&[("Keep rate", 0), ("15 fps", 15)]) },
                ],
                custom_start: vec![40, 0, 0, 0], original_bytes: 84_000_000, dur: 192.0, fps: 29.97, visual: true, note: String::new(),
                picture: std::sync::Arc::new(move |_, _| Ok(base.clone())),
                size: std::sync::Arc::new(|v| Ok((v[0] as u64).pow(2) * 8000)),
                play: None,
            };
            crate::preview::render(spec, if name == "compress" { 1 } else { 4 }, out)
        }
        _ => Err(format!("No sample called {name}")),
    }
}
