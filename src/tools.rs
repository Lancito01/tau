//! Every tool on the Tools wheel. `ask` collects options through the UI; `exec` does the work from plain
//! string options, so the command line and tests reach exactly the same code as the wheel.
use crate::canvas::{self, Shape};
use crate::catalog::{classify, ext_of, Cat};
use crate::form::{self, choice, text, Field};
use crate::image::{self, Bmp, SaveOpts};
use crate::job::*;
use crate::media::{self, s as sv};
use crate::timeline;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub type Opts = HashMap<String, String>;

fn opts(pairs: &[(&str, String)]) -> Opts { pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect() }
fn get<'a>(o: &'a Opts, k: &str, d: &'a str) -> &'a str { o.get(k).map(String::as_str).unwrap_or(d) }

pub fn parse_marks(s: &str) -> Vec<f64> {
    let mut v: Vec<f64> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    v.sort_by(f64::total_cmp);
    v
}
fn marks_str(m: &[f64]) -> String { m.iter().map(|t| format!("{t:.3}")).collect::<Vec<_>>().join(",") }

/// Runs slow setup off the UI thread and keeps the message loop alive meanwhile, so the keyboard hook never stalls input.
pub fn pump_job<T: Send + 'static>(title: &str, f: impl FnOnce(&Job) -> Result<T, String> + Send + 'static) -> Result<T, String> {
    use windows::Win32::UI::WindowsAndMessaging::*;
    let slot: Arc<Mutex<Option<Result<T, String>>>> = Arc::new(Mutex::new(None));
    let once = Mutex::new(Some(f));
    let out = slot.clone();
    let job = spawn(title.to_string(), vec![()], move |j, _| {
        if let Some(f) = once.lock().unwrap().take() { *out.lock().unwrap() = Some(f(j)); }
        Ok(PathBuf::new())
    });
    while !job.finished.load(std::sync::atomic::Ordering::Relaxed) {
        unsafe {
            let mut m = MSG::default();
            while PeekMessageW(&mut m, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&m);
                DispatchMessageW(&m);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(12));
    }
    crate::hud::remove(&job);
    let r = slot.lock().unwrap().take();
    r.unwrap_or_else(|| Err("Cancelled".into()))
}

pub fn fail(title: &str, msg: String) {
    spawn(title.to_string(), vec![()], move |_, _| Err(msg.clone()));
}

// ---- preview material for the editors ----

fn frame_at(input: &Path, t: f64) -> Result<Bmp, String> {
    let ff = crate::ffmpeg().ok_or("Needs ffmpeg")?;
    let tmp = TempDir::new()?;
    let png = tmp.file("f.png");
    run(None, &ff, &sv(&["-y", "-hide_banner", "-ss", &format!("{t:.3}"), "-i", &input.display().to_string(), "-frames:v", "1", &png.display().to_string()]), None)?;
    image::load(&png)
}

const STRIP_FRAMES: u32 = 12;

/// Video: a few seeked frames, not a full decode, so a long film opens quickly. Audio: a waveform image.
fn strip(input: &Path, dur: f64) -> Result<Bmp, String> {
    let ff = crate::ffmpeg().ok_or("Needs ffmpeg")?;
    let tmp = TempDir::new()?;
    if classify(input) == Cat::Audio {
        let png = tmp.file("w.png");
        run(None, &ff, &sv(&["-y", "-hide_banner", "-i", &input.display().to_string(), "-filter_complex", "aformat=channel_layouts=mono,showwavespic=s=1400x220:colors=0xC8F135", "-frames:v", "1", &png.display().to_string()]), None)?;
        let mut b = image::load(&png)?;
        for p in b.px.chunks_exact_mut(4) {
            if p[3] == 0 { p.copy_from_slice(&[0x1E, 0x1B, 0x19, 255]); }
        }
        return Ok(b);
    }
    let mut out = Bmp::new(STRIP_FRAMES * 160, 90, [0x12, 0x10, 0x0F, 255]);
    for i in 0..STRIP_FRAMES {
        let at = dur * (i as f64 + 0.5) / STRIP_FRAMES as f64;
        if let Ok(f) = frame_at(input, at).and_then(|f| f.scaled(160, 90)) { out.blit(&f, (i * 160) as i32, 0); }
    }
    Ok(out)
}

fn pick_times(title: &str, input: &Path, mode: timeline::Mode, ok: &str) -> Option<Vec<f64>> {
    let p = input.to_path_buf();
    let r = pump_job("Reading file", move |_| {
        let dur = media::duration(&p);
        if dur <= 0.0 { return Err("Could not read the length of this file".to_string()); }
        Ok((dur, strip(&p, dur).ok()))
    });
    match r {
        Ok((dur, st)) => timeline::pick(title, st, dur, mode, ok),
        Err(e) => { fail(title, e); None }
    }
}

fn pick_shapes(title: &str, bmp: Result<Bmp, String>, mode: canvas::Mode, init: Vec<Shape>, ok: &str) -> Option<String> {
    match bmp {
        Ok(b) => canvas::edit(title, b, mode, init, ok).map(|v| canvas::encode_all(&v)),
        Err(e) => { fail(title, e); None }
    }
}

/// The video editors look at one frame, with a scrubber to find the right one.
fn pick_shapes_video(title: &str, input: &Path, mode: canvas::Mode, ok: &str) -> Option<String> {
    let p = input.to_path_buf();
    let loaded = pump_job("Reading file", move |_| {
        let dur = media::duration(&p);
        Ok((frame_at(&p, 1f64.min(dur / 2.0))?, dur))
    });
    let (first, dur) = match loaded { Ok(v) => v, Err(e) => { fail(title, e); return None; } };
    let p = input.to_path_buf();
    let source = canvas::FrameSource { dur, load: Arc::new(move |t| frame_at(&p, t)) };
    canvas::edit_video(title, first, mode, ok, source).map(|v| canvas::encode_all(&v))
}

/// Faces and words found in the picture, offered as starting redactions the user can remove one by one.
fn suggest(p: &Path, b: &Bmp) -> Vec<Shape> {
    crate::detect::regions(p, b.w, b.h).unwrap_or_default().into_iter().map(|(x, y, w, h)| Shape::rect(x, y, x + w, y + h)).collect()
}

// ---- options UI ----

/// Asks for what the tool needs. `None` means the user cancelled.
fn ask(id: &str, paths: &[PathBuf]) -> Option<Opts> {
    let first = paths.first()?;
    Some(match id {
        "img.compress" => {
            let v = form::show("Compress image", &[choice("Quality", &["Balanced", "Smaller file", "Higher quality"], 0), choice("Longest side", &["Keep size", "3840", "2560", "1920", "1280"], 0)], "Compress")?;
            opts(&[("quality", v[0].clone()), ("max", v[1].clone())])
        }
        "img.metadata" => {
            form::show("Image metadata", &[Field::Note("Saves a copy with camera, location and other metadata removed.".into())], "Remove")?;
            Opts::new()
        }
        "img.edit" => {
            let p = first.clone();
            let v = match pump_job("Reading file", move |_| image::load(&p)) {
                Ok(b) => crate::photo::show(b, paths.len())?,
                Err(_) => form::show("Edit photo", &[Field::Slider("Brightness", -100, 100, 0), Field::Slider("Contrast", -100, 100, 0), Field::Slider("Saturation", -100, 100, 0), choice("Rotate", &["None", "90 right", "90 left", "180"], 0), choice("Flip", &["None", "Horizontal", "Vertical"], 0)], "Apply")?,
            };
            opts(&[("brightness", v[0].clone()), ("contrast", v[1].clone()), ("saturation", v[2].clone()), ("rotate", v[3].clone()), ("flip", v[4].clone())])
        }
        "img.background" => {
            let v = form::show("Add background", &[Field::Color("Colour", "#FFFFFF".into())], "Apply")?;
            opts(&[("color", v[0].clone())])
        }
        "img.collage" => {
            let v = form::show("Collage", &[choice("Columns", &["Auto", "2", "3", "4", "5"], 0), Field::Slider("Gap (pixels)", 0, 80, 12), Field::Color("Background", "#FFFFFF".into())], "Create")?;
            opts(&[("columns", v[0].clone()), ("gap", v[1].clone()), ("color", v[2].clone())])
        }
        "img.crop" => {
            let p = first.clone();
            let b = pump_job("Reading file", move |_| image::load(&p));
            let sh = pick_shapes("Crop", b, canvas::Mode::Crop, vec![], "Crop")?;
            opts(&[("shapes", sh)])
        }
        "vid.crop" => {
            let sh = pick_shapes_video("Crop video", first, canvas::Mode::Crop, "Crop")?;
            opts(&[("shapes", sh)])
        }
        "vid.redact" => {
            let sh = pick_shapes_video("Redact video", first, canvas::Mode::Redact, "Redact")?;
            opts(&[("shapes", sh)])
        }
        "vid.compress" => {
            let v = form::show("Compress video", &[choice("Quality", &["Balanced", "Smaller file", "Higher quality"], 0), choice("Height", &["Keep size", "1080", "720", "480"], 0)], "Compress")?;
            opts(&[("quality", v[0].clone()), ("height", v[1].clone())])
        }
        "vid.metadata" | "aud.metadata" => {
            let v = form::show("Metadata", &[text("Title", ""), text("Artist or author", ""), text("Comment", ""), Field::Check("Remove everything else", false)], "Save")?;
            opts(&[("title", v[0].clone()), ("artist", v[1].clone()), ("comment", v[2].clone()), ("strip", v[3].clone())])
        }
        "vid.speed" => {
            let v = form::show("Change speed", &[choice("Speed", &["0.25x", "0.5x", "0.75x", "1.25x", "1.5x", "2x", "4x"], 5)], "Apply")?;
            opts(&[("speed", v[0].trim_end_matches('x').to_string())])
        }
        "vid.trim" | "aud.trim" => {
            let m = pick_times("Trim", first, timeline::Mode::Range, "Trim")?;
            opts(&[("marks", marks_str(&m))])
        }
        "vid.split" => {
            let m = pick_times("Split", first, timeline::Mode::Points, "Split")?;
            if m.is_empty() { return None; }
            opts(&[("marks", marks_str(&m))])
        }
        "vid.snapshot" => {
            let m = pick_times("Snapshot", first, timeline::Mode::Points, "Save")?;
            if m.is_empty() { return None; }
            opts(&[("marks", marks_str(&m))])
        }
        "aud.bleep" => {
            let m = pick_times("Bleep", first, timeline::Mode::Ranges, "Bleep")?;
            if m.len() < 2 { return None; }
            opts(&[("marks", marks_str(&m))])
        }
        "aud.compress" => {
            let v = form::show("Compress audio", &[choice("Bitrate", &["64 kbps", "96 kbps", "128 kbps", "192 kbps"], 2)], "Compress")?;
            opts(&[("bitrate", v[0].split(' ').next().unwrap_or("128").to_string())])
        }
        "aud.channels" => {
            let v = form::show("Channels", &[choice("Output", &["Mono", "Stereo from mono", "Swap left and right", "Left only", "Right only"], 0)], "Apply")?;
            opts(&[("mode", v[0].clone())])
        }
        "pdf.compress" => {
            let v = form::show("Compress PDF", &[choice("Method", &["Tidy structure (keeps text selectable)", "Rebuild at 150 DPI (text becomes images)", "Rebuild at 96 DPI (text becomes images)"], 0)], "Compress")?;
            let dpi = if v[0].contains("150") { "150" } else if v[0].contains("96") { "96" } else { "" };
            opts(&[("dpi", dpi.to_string())])
        }
        "pdf.metadata" => {
            let v = form::show("PDF metadata", &[text("Title", ""), text("Author", ""), text("Subject", ""), Field::Check("Remove all existing metadata first", false)], "Save")?;
            opts(&[("title", v[0].clone()), ("author", v[1].clone()), ("subject", v[2].clone()), ("strip", v[3].clone())])
        }
        "pdf.organize" => {
            let p = first.clone();
            let n = pump_job("Reading file", move |_| crate::pdf::page_count(&p)).unwrap_or(0);
            let v = form::show("Organize pages", &[Field::Note(format!("This PDF has {n} pages. List them in the order you want.\nLeave a page out to remove it. 5r90 rotates page 5. 2x3 repeats page 2 three times.")), text("Order", &format!("1-{n}"))], "Save")?;
            opts(&[("order", v[1].clone())])
        }
        _ => Opts::new(),
    })
}

/// From the wheel.
pub fn run_ui(id: &str, paths: Vec<PathBuf>) {
    form::set_context(match paths.as_slice() {
        [one] => one.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        many => format!("{} files", many.len()),
    });
    if matches!(id, "img.annotate" | "img.redact") {
        // Each picture needs its own marks, so these open one editor per file.
        for p in paths {
            let pp = p.clone();
            let loaded = pump_job("Reading file", move |_| { let b = image::load(&pp)?; let s = suggest(&pp, &b); Ok((b, s)) });
            let (bmp, found) = match loaded { Ok((b, f)) => (Ok(b), f), Err(e) => (Err(e), vec![]) };
            form::set_context(p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
            let (mode, init, title, ok) = if id == "img.redact" {
                (canvas::Mode::Redact, found, "Redact", "Redact")
            } else {
                (canvas::Mode::Annotate, vec![], "Annotate", "Save")
            };
            let Some(sh) = pick_shapes(title, bmp, mode, init, ok) else { return };
            exec(id, vec![p], opts(&[("shapes", sh)]));
        }
        return;
    }
    if let Some(o) = ask(id, &paths) { exec(id, paths, o); }
}

// ---- the work ----

fn each(title: &str, paths: Vec<PathBuf>, f: impl Fn(&Job, &Path) -> Result<PathBuf, String> + Send + 'static) -> Arc<Job> {
    spawn(title.to_string(), paths, move |j, p: &PathBuf| f(j, p))
}

/// All inputs to one output.
fn together(title: &str, paths: Vec<PathBuf>, f: impl Fn(&Job, &[PathBuf]) -> Result<PathBuf, String> + Send + 'static) -> Arc<Job> {
    spawn(title.to_string(), vec![paths], move |j, p: &Vec<PathBuf>| f(j, p))
}

/// A compressor must not leave a bigger file behind: keep the original and say so.
fn only_if_smaller(j: &Job, input: &Path, res: Result<PathBuf, String>) -> Result<PathBuf, String> {
    let out = res?;
    let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    if size(&out) < size(input) { return Ok(out); }
    let _ = std::fs::remove_file(&out);
    if j.total == 1 { *j.note.lock().unwrap() = Some("Already as small as it can get".into()); }
    Ok(input.to_path_buf())
}

pub fn quality_of(s: &str) -> f32 { match s { "Smaller file" => 0.6, "Higher quality" => 0.9, _ => 0.78 } }
fn crf_of(s: &str) -> &'static str { match s { "Smaller file" => "30", "Higher quality" => "20", _ => "25" } }

/// Output format for compression: opaque lossless formats become JPEG, since that is where the saving is.
pub fn compress_ext(input_ext: &str, has_alpha: bool) -> String {
    match input_ext {
        "jpg" | "jpeg" | "webp" | "avif" | "heic" | "heif" => input_ext.to_string(),
        _ if has_alpha => "png".to_string(),
        _ => "jpg".to_string(),
    }
}

/// Pixel crop from fractions. Even sizes, because most video codecs require them.
pub fn crop_px(shape: &Shape, w: u32, h: u32, even: bool) -> (u32, u32, u32, u32) {
    let (x, y, cw, ch) = shape.bounds();
    let mut px = ((x * w as f64) as u32, (y * h as f64) as u32, (cw * w as f64) as u32, (ch * h as f64) as u32);
    if even { px = (px.0 & !1, px.1 & !1, (px.2 & !1).max(2), (px.3 & !1).max(2)); }
    px.0 = px.0.min(w - 1);
    px.1 = px.1.min(h - 1);
    px.2 = px.2.clamp(1, w - px.0);
    px.3 = px.3.clamp(1, h - px.1);
    px
}

pub fn collage(imgs: &[Bmp], cols: usize, gap: u32, bg: [u8; 4]) -> Result<Bmp, String> {
    if imgs.is_empty() { return Err("No images".into()); }
    let cols = cols.clamp(1, imgs.len());
    let rows = imgs.len().div_ceil(cols);
    let tile: u32 = 800;
    let (tw, th) = (tile, tile * 3 / 4);
    let mut out = Bmp::new(cols as u32 * (tw + gap) + gap, rows as u32 * (th + gap) + gap, bg);
    for (i, im) in imgs.iter().enumerate() {
        let f = (tw as f64 / im.w as f64).min(th as f64 / im.h as f64);
        let (nw, nh) = (((im.w as f64 * f) as u32).max(1), ((im.h as f64 * f) as u32).max(1));
        let sc = im.scaled(nw, nh)?;
        let (c, r) = ((i % cols) as u32, (i / cols) as u32);
        let x = gap + c * (tw + gap) + (tw - nw) / 2;
        let y = gap + r * (th + gap) + (th - nh) / 2;
        out.blit(&sc, x as i32, y as i32);
    }
    Ok(out)
}

pub fn auto_columns(n: usize) -> usize { (n as f64).sqrt().ceil() as usize }

fn save_new(b: &Bmp, out: PathBuf, o: &SaveOpts) -> Result<PathBuf, String> {
    atomically(&out, |tmp| image::save(b, tmp, o))
}

fn edit_filter(o: &Opts) -> Vec<String> {
    let n = |k: &str| get(o, k, "0").trim().parse::<f64>().unwrap_or(0.0).clamp(-100.0, 100.0);
    // eq goes through YUV and shifts colours by a step even at neutral settings, so leave it out then.
    let mut f = vec![];
    if n("brightness") != 0.0 || n("contrast") != 0.0 || n("saturation") != 0.0 {
        f.push(format!("eq=brightness={:.3}:contrast={:.3}:saturation={:.3}", n("brightness") / 200.0, 1.0 + n("contrast") / 100.0, 1.0 + n("saturation") / 100.0));
    }
    match get(o, "rotate", "None") { "90 right" => f.push("transpose=1".into()), "90 left" => f.push("transpose=2".into()), "180" => f.push("hflip,vflip".into()), _ => {} }
    match get(o, "flip", "None") { "Horizontal" => f.push("hflip".into()), "Vertical" => f.push("vflip".into()), _ => {} }
    if f.is_empty() { f.push("null".into()); }
    f
}

fn metadata_args(o: &Opts) -> Vec<String> {
    let mut a = vec![];
    if get(o, "strip", "0") == "1" { a.extend(sv(&["-map_metadata", "-1"])); }
    for k in ["title", "artist", "comment"] {
        let v = get(o, k, "").trim();
        if !v.is_empty() { a.push("-metadata".into()); a.push(format!("{k}={v}")); }
    }
    a.extend(sv(&["-c", "copy"]));
    a
}

fn trim_args(cat: Cat, a: f64, b: f64, ext: &str) -> Vec<String> {
    let mut v = sv(&["-ss", &format!("{a:.3}"), "-to", &format!("{b:.3}")]);
    if cat == Cat::Video {
        // Re-encoding cuts on the exact frame; stream copy could only cut at keyframes.
        v.extend(sv(&["-c:v", "libx264", "-crf", "18", "-preset", "veryfast", "-pix_fmt", "yuv420p"]));
        if ext == "webm" { v = sv(&["-ss", &format!("{a:.3}"), "-to", &format!("{b:.3}"), "-c:v", "libvpx-vp9", "-crf", "30", "-b:v", "0"]); }
    }
    v
}

pub fn channels_filter(mode: &str) -> &'static str {
    match mode {
        "Stereo from mono" => "pan=stereo|c0=c0|c1=c0",
        "Swap left and right" => "pan=stereo|c0=c1|c1=c0",
        "Left only" => "pan=mono|c0=c0",
        "Right only" => "pan=mono|c0=c1",
        _ => "pan=mono|c0=0.5*c0+0.5*c1",
    }
}

pub fn join_filter(sizes: (u32, u32), audio: &[Option<f64>]) -> String {
    let (w, h) = (sizes.0 & !1, sizes.1 & !1);
    let mut f = String::new();
    let mut tail = String::new();
    for (i, a) in audio.iter().enumerate() {
        f.push_str(&format!("[{i}:v]scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30,format=yuv420p[v{i}];"));
        match a {
            // A clip with no sound still needs a silent track of its own length, or concat fails.
            Some(d) => f.push_str(&format!("anullsrc=r=48000:cl=stereo,atrim=duration={d:.3}[a{i}];")),
            None => f.push_str(&format!("[{i}:a]aformat=sample_rates=48000:channel_layouts=stereo[a{i}];")),
        }
        tail.push_str(&format!("[v{i}][a{i}]"));
    }
    format!("{f}{tail}concat=n={}:v=1:a=1[v][a]", audio.len())
}

/// Runs a tool. Returns the job so callers can wait on it.
pub fn exec(id: &str, paths: Vec<PathBuf>, o: Opts) -> Arc<Job> {
    let shapes = canvas::decode_all(get(&o, "shapes", ""));
    let marks = parse_marks(get(&o, "marks", ""));
    match id {
        "img.compress" => each("Compressing", paths, move |j, p| {
            let mut b = image::load(p)?;
            j.set_frac(0.4);
            let mut resized = false;
            if let Ok(max) = get(&o, "max", "").parse::<u32>() {
                let long = b.w.max(b.h);
                if long > max { let f = max as f64 / long as f64; b = b.scaled((b.w as f64 * f) as u32, (b.h as f64 * f) as u32)?; resized = true; }
            }
            let ext = compress_ext(&ext_of(p), b.has_alpha());
            let res = save_new(&b, sibling(p, " compressed", &ext), &SaveOpts { quality: quality_of(get(&o, "quality", "")) });
            if resized { res } else { only_if_smaller(j, p, res) }
        }),
        "img.metadata" => each("Removing metadata", paths, |j, p| {
            let e = ext_of(p);
            let out = sibling(p, " clean", &e);
            // JPEG and PNG lose it without being re-encoded.
            if let Some(clean) = std::fs::read(p).ok().and_then(|b| image::strip_metadata(&e, &b)) {
                j.set_frac(0.5);
                return atomically(&out, |tmp| std::fs::write(tmp, &clean).map_err(|x| x.to_string()));
            }
            // Decoding and saving again carries no metadata across.
            let b = image::load(p)?;
            save_new(&b, sibling(p, " clean", &e), &SaveOpts { quality: 0.95 })
        }),
        "img.edit" => each("Editing", paths, move |j, p| {
            media::transform(j, p, " edited", None, vec!["-vf".into(), edit_filter(&o).join(","), "-frames:v".into(), "1".into(), "-update".into(), "1".into()])
        }),
        "img.background" => each("Adding background", paths, move |_, p| {
            let src = image::load(p)?;
            let mut bg = Bmp::new(src.w, src.h, image::parse_color(get(&o, "color", "#FFFFFF")));
            bg.blit(&src, 0, 0);
            save_new(&bg, sibling(p, " background", "png"), &SaveOpts::default())
        }),
        "img.crop" => each("Cropping", paths, move |_, p| {
            let sh = shapes.first().ok_or("Nothing selected to crop")?;
            let b = image::load(p)?;
            let (x, y, w, h) = crop_px(sh, b.w, b.h, false);
            save_new(&b.crop(x, y, w, h), sibling(p, " cropped", &ext_of(p)), &SaveOpts::default())
        }),
        "img.redact" => each("Redacting", paths, move |_, p| {
            let mut b = image::load(p)?;
            for sh in &shapes {
                let (x, y, w, h) = crop_px(sh, b.w, b.h, false);
                // Solid black, not blur: nothing remains to be recovered.
                b.fill_rect(x as i32, y as i32, w as i32, h as i32, [0, 0, 0, 255]);
            }
            save_new(&b, sibling(p, " redacted", &ext_of(p)), &SaveOpts { quality: 0.95 })
        }),
        "img.annotate" => each("Annotating", paths, move |_, p| {
            let mut b = image::load(p)?;
            canvas::apply_annotations(&mut b, &shapes)?;
            save_new(&b, sibling(p, " annotated", &ext_of(p)), &SaveOpts { quality: 0.95 })
        }),
        "img.collage" => together("Creating collage", paths, move |j, ps| {
            let mut imgs = vec![];
            for (i, p) in ps.iter().enumerate() {
                if j.cancelled() { return Err("Cancelled".into()); }
                j.set_frac(i as f32 / ps.len() as f32 * 0.8);
                imgs.push(image::load(p)?);
            }
            let cols = get(&o, "columns", "Auto").parse().unwrap_or_else(|_| auto_columns(imgs.len()));
            let gap = get(&o, "gap", "12").trim().parse().unwrap_or(12);
            let c = collage(&imgs, cols, gap, image::parse_color(get(&o, "color", "#FFFFFF")))?;
            save_new(&c, sibling(&ps[0], " collage", "png"), &SaveOpts::default())
        }),

        "vid.compress" => each("Compressing", paths, move |j, p| {
            let mut a = sv(&["-c:v", "libx264", "-preset", "medium", "-crf", crf_of(get(&o, "quality", "")), "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "128k", "-movflags", "+faststart"]);
            let mut resized = false;
            if let Ok(h) = get(&o, "height", "").parse::<u32>() {
                a.push("-vf".into());
                a.push(format!("scale=-2:'min({h},ih)'"));
                resized = media::size(p).is_some_and(|(_, sh)| sh > h);
            }
            let res = media::transform(j, p, " compressed", Some("mp4"), a);
            if resized { res } else { only_if_smaller(j, p, res) }
        }),
        "vid.metadata" | "aud.metadata" => each("Saving metadata", paths, move |j, p| media::transform(j, p, " edited", None, metadata_args(&o))),
        "vid.mute" => each("Muting", paths, |j, p| media::transform(j, p, " muted", None, sv(&["-an", "-c:v", "copy"]))),
        "vid.speed" => each("Changing speed", paths, move |j, p| {
            let f: f64 = get(&o, "speed", "2").parse().map_err(|_| "Bad speed")?;
            if f <= 0.0 { return Err("Bad speed".into()); }
            let mut a = vec!["-vf".to_string(), format!("setpts=PTS/{f}")];
            if media::has_audio(p) { a.push("-af".into()); a.push(media::atempo_chain(f)); } else { a.push("-an".into()); }
            media::transform(j, p, &format!(" {f}x"), None, a)
        }),
        "vid.crop" => each("Cropping", paths, move |j, p| {
            let sh = shapes.first().ok_or("Nothing selected to crop")?;
            let (w, h) = media::size(p).ok_or("Could not read the video size")?;
            let (x, y, cw, ch) = crop_px(sh, w, h, true);
            media::transform(j, p, " cropped", None, vec!["-vf".into(), format!("crop={cw}:{ch}:{x}:{y}"), "-c:a".into(), "copy".into()])
        }),
        "vid.redact" => each("Redacting", paths, move |j, p| {
            let (w, h) = media::size(p).ok_or("Could not read the video size")?;
            let f: Vec<String> = shapes.iter().map(|sh| { let (x, y, cw, ch) = crop_px(sh, w, h, false); format!("drawbox=x={x}:y={y}:w={cw}:h={ch}:color=black:t=fill") }).collect();
            if f.is_empty() { return Err("Nothing marked to redact".into()); }
            media::transform(j, p, " redacted", None, vec!["-vf".into(), f.join(","), "-c:a".into(), "copy".into()])
        }),
        "vid.trim" | "aud.trim" => each("Trimming", paths, move |j, p| {
            let (a, b) = match marks[..] { [a, b, ..] => (a, b), _ => return Err("Needs a start and an end".into()) };
            let e = ext_of(p);
            let out = sibling(p, " trimmed", &e);
            let args = trim_args(classify(p), a, b, &e);
            atomically(&out, |tmp| media::ffmpeg(j, &[p], &args, tmp, b))
        }),
        "vid.split" => each("Splitting", paths, move |j, p| {
            let dur = media::duration(p);
            let dir = sibling_dir(p, " parts")?;
            let e = ext_of(p);
            let stem = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
            let mut cuts = vec![0.0];
            cuts.extend(marks.iter().copied().filter(|m| *m > 0.0 && *m < dur));
            cuts.push(dur);
            for (i, w) in cuts.windows(2).enumerate() {
                if j.cancelled() { return Err("Cancelled".into()); }
                j.set_frac(i as f32 / (cuts.len() - 1) as f32);
                let out = dir.join(format!("{stem} part {}.{e}", i + 1));
                media::ffmpeg(j, &[p], &trim_args(Cat::Video, w[0], w[1], &e), &out, 0.0)?;
            }
            Ok(dir)
        }),
        "vid.snapshot" => each("Saving snapshots", paths, move |j, p| {
            let ff = crate::ffmpeg().ok_or("Needs ffmpeg")?;
            let dir = if marks.len() > 1 { Some(sibling_dir(p, " snapshots")?) } else { None };
            let mut last = PathBuf::new();
            for (i, t) in marks.iter().enumerate() {
                j.set_frac(i as f32 / marks.len() as f32);
                last = match &dir { Some(d) => d.join(format!("frame-{:03}.png", i + 1)), None => sibling(p, " snapshot", "png") };
                run(Some(j), &ff, &sv(&["-y", "-hide_banner", "-ss", &format!("{t:.3}"), "-i", &p.display().to_string(), "-frames:v", "1", &last.display().to_string()]), None)?;
            }
            Ok(dir.unwrap_or(last))
        }),
        "vid.join" => together("Joining videos", paths, |j, ps| {
            let sz = media::size(&ps[0]).ok_or("Could not read the video size")?;
            let audio: Vec<Option<f64>> = ps.iter().map(|p| if media::has_audio(p) { None } else { Some(media::duration(p)) }).collect();
            let total: f64 = ps.iter().map(|p| media::duration(p)).sum();
            let inputs: Vec<&Path> = ps.iter().map(PathBuf::as_path).collect();
            let a = vec!["-filter_complex".into(), join_filter(sz, &audio), "-map".into(), "[v]".into(), "-map".into(), "[a]".into(), "-c:v".into(), "libx264".into(), "-crf".into(), "20".into(), "-c:a".into(), "aac".into()];
            let out = sibling(&ps[0], " joined", "mp4");
            atomically(&out, |tmp| media::ffmpeg(j, &inputs, &a, tmp, total))
        }),

        "aud.compress" => each("Compressing", paths, move |j, p| {
            let ext = match ext_of(p).as_str() { e @ ("mp3" | "m4a" | "ogg" | "opus" | "aac" | "wma") => e.to_string(), _ => "mp3".to_string() };
            only_if_smaller(j, p, media::transform(j, p, " compressed", Some(&ext), vec!["-vn".into(), "-b:a".into(), format!("{}k", get(&o, "bitrate", "128"))]))
        }),
        "aud.normalize" => each("Normalizing", paths, |j, p| media::transform(j, p, " normalized", None, sv(&["-af", "loudnorm=I=-16:TP=-1.5:LRA=11"]))),
        "aud.visualize" => each("Rendering video", paths, |j, p| {
            let a = sv(&["-filter_complex", "[0:a]showwaves=s=1280x720:mode=cline:rate=30:colors=0xC8F135,format=yuv420p[v]", "-map", "[v]", "-map", "0:a", "-c:v", "libx264", "-preset", "veryfast", "-c:a", "aac", "-shortest"]);
            media::transform(j, p, " visualized", Some("mp4"), a)
        }),
        "aud.channels" => each("Changing channels", paths, move |j, p| media::transform(j, p, " channels", None, vec!["-af".into(), channels_filter(get(&o, "mode", "Mono")).into()])),
        "aud.bleep" => each("Bleeping", paths, move |j, p| {
            let ranges: Vec<(f64, f64)> = marks.chunks_exact(2).map(|c| (c[0], c[1])).collect();
            if ranges.is_empty() { return Err("Mark at least one range".into()); }
            media::transform(j, p, " bleeped", None, vec!["-filter_complex".into(), media::bleep_filter(&ranges), "-map".into(), "[out]".into()])
        }),

        "pdf.compress" => each("Compressing", paths, move |j, p| only_if_smaller(j, p, crate::pdf::compress(j, p, get(&o, "dpi", "").parse().ok()))),
        "pdf.metadata" => each("Saving metadata", paths, move |_, p| {
            let f = [("Title", get(&o, "title", "").to_string()), ("Author", get(&o, "author", "").to_string()), ("Subject", get(&o, "subject", "").to_string())];
            crate::pdf::set_metadata(p, &f, get(&o, "strip", "0") == "1")
        }),
        "pdf.split" => each("Splitting", paths, |j, p| crate::pdf::split(j, p)),
        "pdf.organize" => each("Organizing", paths, move |_, p| {
            let doc = crate::pdf::load_doc(p)?;
            let order = crate::pdf::parse_order(get(&o, "order", ""), doc.get_pages().len() as u32)?;
            if order.is_empty() { return Err("No pages listed".into()); }
            let picks: Vec<(usize, u32, i64)> = order.into_iter().map(|(n, r)| (0, n, r)).collect();
            let out = sibling(p, " organized", "pdf");
            atomically(&out, |tmp| crate::pdf::assemble(std::slice::from_ref(&doc), &picks, tmp))
        }),
        "pdf.merge" => together("Merging", paths, |j, ps| {
            let out = sibling(&ps[0], " merged", "pdf");
            atomically(&out, |tmp| crate::pdf::merge(j, ps, tmp))
        }),
        "arc.extract" => each("Extracting", paths, |j, p| crate::docs::extract(j, p)),
        other => { let msg = format!("Unknown tool {other}"); spawn("Tool".to_string(), vec![()], move |_, _| Err(msg.clone())) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn marks_parse_sorted() { assert_eq!(parse_marks("5, 1.5,x,3"), vec![1.5, 3.0, 5.0]); }
    #[test]
    fn compression_only_changes_format_when_it_helps() {
        assert_eq!(compress_ext("png", false), "jpg");
        assert_eq!(compress_ext("png", true), "png");
        assert_eq!(compress_ext("jpeg", false), "jpeg");
        assert_eq!(compress_ext("bmp", false), "jpg");
    }
    #[test]
    fn video_crop_is_even_and_inside() {
        let (x, y, w, h) = crop_px(&Shape::rect(0.1, 0.1, 0.9, 1.0), 1921, 1081, true);
        assert_eq!((x % 2, y % 2, w % 2, h % 2), (0, 0, 0, 0));
        assert!(x + w <= 1921 && y + h <= 1081);
    }
    #[test]
    fn crop_never_empty_at_the_edge() {
        let (x, _, w, _) = crop_px(&Shape::rect(1.0, 0.0, 1.0, 1.0), 100, 100, false);
        assert!(w >= 1 && x + w <= 100);
    }
    #[test]
    fn collage_fits_grid() {
        let imgs = vec![Bmp::new(100, 50, [0, 0, 255, 255]), Bmp::new(50, 100, [0, 255, 0, 255]), Bmp::new(10, 10, [255, 0, 0, 255])];
        crate::job::com_init();
        let c = collage(&imgs, 2, 10, [255, 255, 255, 255]).unwrap();
        assert_eq!(c.w, 2 * 810 + 10);
        assert_eq!(c.h, 2 * 610 + 10);
    }
    #[test]
    fn columns_are_square_ish() { assert_eq!(auto_columns(4), 2); assert_eq!(auto_columns(5), 3); }
    #[test]
    fn join_fills_silence_for_clips_without_sound() {
        let f = join_filter((1280, 720), &[None, Some(2.5)]);
        assert!(f.contains("[0:a]aformat") && f.contains("anullsrc") && f.contains("duration=2.500") && f.ends_with("concat=n=2:v=1:a=1[v][a]"));
    }
}
