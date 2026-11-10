//! Every tool on the Tools wheel. `ask` collects options through the UI; `exec` does the work from plain
//! string options, so the command line and tests reach exactly the same code as the wheel.
use crate::canvas::{self, Shape};
use crate::catalog::{classify, ext_of, Cat};
use crate::form::{self, choice, text, Field};
use crate::image::{self, Bmp, SaveOpts};
use crate::job::*;
use crate::media::{self, s as sv};
use crate::preview::{self, Control, Preset};
use crate::quality;
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
fn marks_str(m: &[f64]) -> String { m.iter().map(|t| format!("{t:.6}")).collect::<Vec<_>>().join(",") }

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

fn frame_at(input: &Path, t: f64) -> Result<Bmp, String> { still_at(input, t, None) }

/// A frame, through a filter when it would not look right as a picture without one.
fn still_at(input: &Path, t: f64, filter: Option<&str>) -> Result<Bmp, String> {
    // A file's length can run past its last picture when the sound goes on longer. The last picture is the nearest then.
    still_exactly(input, t, filter).or_else(|e| if t > 0.0 { still_exactly(input, (t - END_MARGIN).max(0.0), filter) } else { Err(e) })
}

const END_MARGIN: f64 = 0.25;

fn still_exactly(input: &Path, t: f64, filter: Option<&str>) -> Result<Bmp, String> {
    let ff = crate::ffmpeg().ok_or("Needs ffmpeg")?;
    let tmp = TempDir::new()?;
    let png = tmp.file("f.png");
    let vf = filter.map(|f| vec!["-vf".to_string(), f.to_string()]).unwrap_or_default();
    let (before, after) = quality::still_seek(&ext_of(input), t);
    // The PNG only carries the picture across and is read straight back, so it is stored uncompressed: the same pixels,
    // without the time it takes to compress a large frame.
    run(None, &ff, &sv(&["-y", "-hide_banner"]).into_iter().chain(before).chain(sv(&["-i", &input.display().to_string()])).chain(after).chain(vf).chain(sv(&["-frames:v", "1", "-compression_level", "0", &png.display().to_string()])).collect::<Vec<_>>(), None)?;
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
    // Each frame is its own ffmpeg, so they are fetched side by side rather than one after another.
    let tiles: Vec<Option<Bmp>> = std::thread::scope(|sc| {
        let handles: Vec<_> = (0..STRIP_FRAMES).map(|i| sc.spawn(move || {
            com_init();
            let at = dur * (i as f64 + 0.5) / STRIP_FRAMES as f64;
            frame_at(input, at).and_then(|f| f.scaled(160, 90)).ok()
        })).collect();
        handles.into_iter().map(|h| h.join().ok().flatten()).collect()
    });
    for (i, f) in tiles.iter().enumerate() {
        if let Some(f) = f { out.blit(f, (i as u32 * 160) as i32, 0); }
    }
    Ok(out)
}

/// Frames per second of a video, 0 for anything without pictures.
fn frame_rate(p: &Path) -> f64 {
    media::probe(p).video.as_ref().map(|v| v.fps).unwrap_or(0.0)
}

fn pick_times(title: &str, input: &Path, mode: timeline::Mode, ok: &str) -> Option<Vec<f64>> {
    let p = input.to_path_buf();
    let r = pump_job("Reading file", move |_| {
        let pr = media::probe(&p);
        let dur = pr.duration;
        if dur <= 0.0 { return Err("Could not read the length of this file".to_string()); }
        Ok((dur, pr.video, strip(&p, dur).ok()))
    });
    let (dur, video, st) = match r { Ok(v) => v, Err(e) => { fail(title, e); return None; } };
    let fps = video.as_ref().map(|v| v.fps).unwrap_or(0.0);
    let filter = video.as_ref().and_then(quality::still_filter);
    // Only pictures have a frame to look at.
    let load: Option<timeline::FrameLoader> = (classify(input) == Cat::Video).then(|| {
        let p = input.to_path_buf();
        Arc::new(move |t| still_at(&p, t, filter.as_deref()).and_then(thumbnail)) as timeline::FrameLoader
    });
    timeline::pick(title, st, dur, fps, mode, ok, load)
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
        Ok((frame_at(&p, 1f64.min(dur / 2.0))?, dur, frame_rate(&p)))
    });
    let (first, dur, fps) = match loaded { Ok(v) => v, Err(e) => { fail(title, e); return None; } };
    let p = input.to_path_buf();
    let source = canvas::FrameSource { dur, fps, pages: 0, load: Arc::new(move |t| frame_at(&p, t)) };
    canvas::edit_video(title, first, mode, ok, source).map(|v| canvas::encode_all(&v))
}

/// A PDF's redaction editor, a page at a time. Gives "page<TAB>shape" lines for the pages that have marks.
fn pick_pdf_redactions(input: &Path) -> Option<String> {
    const LONG: u32 = 1600;
    let p = input.to_path_buf();
    let loaded = pump_job("Reading file", move |_| Ok((crate::pdf::page_count(&p)?, crate::pdf::page_preview(&p, 0, LONG)?)));
    let (count, first) = match loaded { Ok(v) => v, Err(e) => { fail("Redact", e); return None; } };
    let p = input.to_path_buf();
    let pages = canvas::redact_pages("Redact", first, count, "Redact", Arc::new(move |i| crate::pdf::page_preview(&p, i, LONG)))?;
    Some(pages.iter().enumerate().flat_map(|(i, v)| v.iter().map(move |s| format!("{i}\t{}", s.encode()))).collect::<Vec<_>>().join("\n"))
}

/// The marks of each page from `pick_pdf_redactions`, in page order.
fn decode_pages(s: &str) -> Vec<(u32, Vec<Shape>)> {
    let mut out: std::collections::BTreeMap<u32, Vec<Shape>> = Default::default();
    for line in s.lines() {
        let Some((n, rest)) = line.split_once('\t') else { continue };
        let (Ok(n), Some(sh)) = (n.parse::<u32>(), Shape::decode(rest)) else { continue };
        out.entry(n).or_default().push(sh);
    }
    out.into_iter().collect()
}

/// Faces and words found in the picture, offered as starting redactions the user can remove one by one.
fn suggest(p: &Path, b: &Bmp) -> Vec<Shape> {
    crate::detect::regions(p, b.w, b.h).unwrap_or_default().into_iter().map(|(x, y, w, h)| Shape::rect(x, y, x + w, y + h)).collect()
}

// ---- what each compress level would give ----

const THUMB_MAX: u32 = 900;

/// A copy small enough to redraw while a slider moves.
fn thumbnail(b: Bmp) -> Result<Bmp, String> {
    let long = b.w.max(b.h);
    if long <= THUMB_MAX { return Ok(b); }
    let f = THUMB_MAX as f64 / long as f64;
    b.scaled(((b.w as f64 * f) as u32).max(1), ((b.h as f64 * f) as u32).max(1))
}

fn file_size(p: &Path) -> u64 { std::fs::metadata(p).map(|m| m.len()).unwrap_or(0) }

/// "Keep" and every limit under what the file already has, largest first. A limit above it would do nothing.
fn limits_under(keep: &str, source: u32, limits: &[u32], label: impl Fn(u32) -> String) -> Vec<(String, i32)> {
    std::iter::once((keep.to_string(), 0)).chain(limits.iter().filter(|l| source == 0 || **l < source).map(|l| (label(*l), *l as i32))).collect()
}

/// The name of a level, with the size it shrinks to when it shrinks the picture.
fn level_name(name: &str, shrink: i32, suffix: &str) -> String {
    if shrink > 0 { format!("{name}, {shrink}{suffix}") } else { name.to_string() }
}

/// (name, picture quality, tallest picture, audio allowance in kbps). Audio stays as it is until the level is modest,
/// and only the bottom levels ask much of it.
const VIDEO_LEVELS: [(&str, i32, u32, i32); 5] = [
    ("Very high quality", 94, 0, 0),
    ("High quality", 80, 0, 256),
    ("Medium quality", 66, 1080, 160),
    ("Low quality", 54, 720, 96),
    ("Very low quality", 43, 480, 64),
];
const AUDIO_ALLOWANCES: [i32; 11] = [320, 256, 192, 160, 128, 96, 64, 48, 32, 16, 8];

fn squeeze_of(v: &[i32]) -> quality::Squeeze {
    let some = |x: i32| (x > 0).then_some(x as u32);
    quality::Squeeze { crf: quality::crf_for(v[0]), height: some(v[1]), audio_kbps: some(v[2]), fps: some(v[3]) }
}

/// Encodes a stretch of the video exactly as the tool will.
fn encode_sample(input: &Path, args: &[String], start: f64, len: f64, out: &Path) -> Result<(), String> {
    let ff = crate::ffmpeg().ok_or("Needs ffmpeg")?;
    let mut a = sv(&["-y", "-hide_banner", "-nostdin", "-ss", &format!("{start:.3}"), "-t", &format!("{len:.3}"), "-i", &input.display().to_string()]);
    a.extend_from_slice(args);
    a.push(out.display().to_string());
    run(None, &ff, &a, None).map(|_| ())
}

/// The frame at a moment comes from a second of video encoded around it, so it is a typical frame that has been
/// predicted from others and not the unusually clean one a stream starts with. The size comes from two short pieces,
/// scaled up to the whole length.
fn video_spec(input: &Path) -> Result<preview::Spec, String> {
    let pr = media::probe(input);
    let dur = media::duration(input);
    if dur <= 0.0 { return Err("Could not read the length of this file".to_string()); }
    let (height, fps) = pr.video.as_ref().map(|v| (v.display_size().1, v.fps)).unwrap_or((0, 0.0));
    let heights = limits_under("Keep size", height, &[2160, 1440, 1080, 720, 480, 360, 240, 144], |h| format!("{h}p"));
    let rates = limits_under("Keep rate", fps.ceil() as u32, &[60, 30, 24, 15, 10, 5], |r| format!("{r} fps"));
    let audio: Vec<(String, i32)> = if pr.audio.is_empty() { vec![("No audio in this file".to_string(), 0)] } else {
        std::iter::once(("Keep as it is".to_string(), 0)).chain(AUDIO_ALLOWANCES.iter().map(|k| (format!("{k} kbps"), *k))).collect()
    };
    let presets = VIDEO_LEVELS.iter().map(|(name, q, h, a)| {
        let h = if height == 0 || *h < height { *h as i32 } else { 0 };
        Preset { name: level_name(name, h, "p"), values: vec![*q, h, if pr.audio.is_empty() { 0 } else { *a }, 0] }
    }).collect();
    let controls = vec![
        Control::Slider { label: "Picture quality".into(), min: 0, max: 100, unit: "%" },
        Control::Choice { label: "Resolution".into(), options: heights },
        Control::Choice { label: "Audio".into(), options: audio },
        Control::Choice { label: "Frame rate".into(), options: rates },
    ];
    let still = pr.video.as_ref().and_then(quality::still_filter);
    let (p, probe) = (input.to_path_buf(), pr.clone());
    let picture: preview::Picture = Arc::new(move |values, t| {
        let Some(values) = values else { return still_at(&p, t, still.as_deref()) };
        let args = quality::compress_video_args(&probe, &squeeze_of(values));
        let start = (t - 0.6).max(0.0);
        let tmp = TempDir::new()?;
        let out = tmp.file("frame.mp4");
        encode_sample(&p, &args, start, 1.0, &out)?;
        // What the encode left may still have pixels that are not square, and it may be shorter than asked for.
        let filter = media::probe(&out).video.as_ref().and_then(quality::still_filter);
        still_at(&out, t - start, filter.as_deref()).or_else(|_| still_at(&out, 0.0, filter.as_deref()))
    });
    let (p, probe) = (input.to_path_buf(), pr);
    let size: preview::Size = Arc::new(move |values| {
        let len = 2f64.min(dur / 3.0).max(0.1);
        let starts = if dur > len * 3.0 { vec![dur * 0.3, dur * 0.7] } else { vec![0.0] };
        let args = quality::compress_video_args(&probe, &squeeze_of(values));
        let tmp = TempDir::new()?;
        // The samples are separate encodes, so they run side by side.
        let sizes: Vec<Result<u64, String>> = std::thread::scope(|sc| {
            let handles: Vec<_> = starts.iter().enumerate().map(|(i, start)| {
                let (p, args, out) = (&p, &args, tmp.file(&format!("s{i}.mp4")));
                sc.spawn(move || { encode_sample(p, args, *start, len, &out)?; Ok(file_size(&out)) })
            }).collect();
            handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("Could not measure".into()))).collect()
        });
        let bytes = sizes.into_iter().sum::<Result<u64, String>>()?;
        Ok((bytes as f64 / (starts.len() as f64 * len) * dur) as u64)
    });
    Ok(preview::Spec {
        title: "Compress video".into(), ok: "Compress".into(), presets, default: 1, controls,
        custom_start: vec![40, 0, 0, 0], original_bytes: file_size(input), dur, fps, visual: true, note: String::new(), picture, size,
    })
}

const IMAGE_LEVELS: [(&str, i32, u32); 5] = [
    ("Very high quality", 90, 0),
    ("High quality", 80, 0),
    ("Medium quality", 65, 0),
    ("Low quality", 45, 1920),
    ("Very low quality", 25, 1280),
];

/// The picture no longer than `max` on its longest side. Zero, or a picture already within it, stays as it is.
fn shrink_to(b: &Bmp, max: u32) -> Result<Bmp, String> {
    let long = b.w.max(b.h);
    if max == 0 || long <= max { return Ok(b.clone()); }
    let f = max as f64 / long as f64;
    b.scaled(((b.w as f64 * f) as u32).max(1), ((b.h as f64 * f) as u32).max(1))
}

/// An image compressed for real at each level, since a picture takes no time.
fn image_spec(input: &Path) -> Result<preview::Spec, String> {
    let source = Arc::new(image::load(input)?);
    let long = source.w.max(source.h);
    let presets = IMAGE_LEVELS.iter().map(|(name, q, m)| {
        let m = if *m < long { *m as i32 } else { 0 };
        Preset { name: level_name(name, m, " px"), values: vec![*q, m] }
    }).collect();
    let sides = limits_under("Keep size", long, &[3840, 2560, 1920, 1280, 800, 480, 240, 120], |m| format!("{m} px"));
    let encode = {
        let (input, source) = (input.to_path_buf(), source.clone());
        move |values: &[i32]| -> Result<(TempDir, PathBuf), String> {
            let b = shrink_to(&source, values[1] as u32)?;
            let t = TempDir::new()?;
            let out = t.file(&format!("o.{}", compress_ext(&ext_of(&input), b.has_alpha())));
            image::save(&b, &out, &SaveOpts { quality: values[0] as f32 / 100.0, fit: None, ..Default::default() })?;
            Ok((t, out))
        }
    };
    let enc = encode.clone();
    let original = source.clone();
    let picture: preview::Picture = Arc::new(move |values, _| match values {
        None => Ok((*original).clone()),
        Some(v) => { let (_t, out) = enc(v)?; image::load(&out) }
    });
    let size: preview::Size = Arc::new(move |v| { let (_t, out) = encode(v)?; Ok(file_size(&out)) });
    let note = if source.has_alpha() { "This picture has transparency, so it stays a PNG. Only a smaller size saves space.".to_string() } else { String::new() };
    Ok(preview::Spec {
        title: "Compress image".into(), ok: "Compress".into(), presets, default: 1,
        controls: vec![
            Control::Slider { label: "Picture quality".into(), min: 1, max: 99, unit: "%" },
            Control::Choice { label: "Longest side".into(), options: sides },
        ],
        custom_start: vec![40, 0], original_bytes: file_size(input), dur: 0.0, fps: 0.0, visual: true, note, picture, size,
    })
}

/// A PDF's first page in each form. Tidying is really done for its size, and a rebuild is scaled up from that one page.
fn pdf_spec(input: &Path) -> Result<preview::Spec, String> {
    let pages = crate::pdf::page_count(input)? as u64;
    let p = input.to_path_buf();
    let picture: preview::Picture = Arc::new(move |values, _| match values.map(|v| v[0]) {
        None | Some(0) => Ok(crate::pdf::first_page(&p, 200.0, false)?.0),
        Some(dpi) => Ok(crate::pdf::first_page(&p, dpi as f64, true)?.0),
    });
    let p = input.to_path_buf();
    let size: preview::Size = Arc::new(move |v| match v[0] {
        0 => crate::pdf::tidied_size(&p),
        dpi => Ok(crate::pdf::first_page(&p, dpi as f64, true)?.1 * pages),
    });
    let level = |name: &str, dpi| Preset { name: name.to_string(), values: vec![dpi] };
    Ok(preview::Spec {
        title: "Compress PDF".into(), ok: "Compress".into(), presets: vec![level("Tidy only", 0), level("Scan at 150 DPI", 150), level("Scan at 96 DPI", 96)], default: 0,
        controls: vec![Control::Slider { label: "Scan resolution".into(), min: 20, max: 300, unit: " DPI" }],
        custom_start: vec![72], original_bytes: file_size(input), dur: 0.0, fps: 0.0, visual: true,
        note: "Tidying keeps the text. A scan turns every page into a picture, so its text can no longer be selected.".into(), picture, size,
    })
}

/// A sound file has nothing to look at, so the view is its waveform and each level is its bitrate.
fn audio_spec(input: &Path) -> Result<preview::Spec, String> {
    let dur = media::duration(input);
    let wave = Arc::new(strip(input, dur));
    let picture: preview::Picture = Arc::new(move |_, _| (*wave).clone());
    let size: preview::Size = Arc::new(move |v| if dur > 0.0 { Ok((v[0] as f64 * 125.0 * dur) as u64) } else { Err("Could not read the length of this file".into()) });
    let level = |name: &str, k: i32| Preset { name: format!("{name}, {k} kbps"), values: vec![k] };
    Ok(preview::Spec {
        title: "Compress audio".into(), ok: "Compress".into(),
        presets: vec![level("High", 192), level("Good", 128), level("Medium", 96), level("Low", 64)], default: 1,
        controls: vec![Control::Slider { label: "Bitrate".into(), min: 8, max: 320, unit: " kbps" }],
        custom_start: vec![64], original_bytes: file_size(input), dur: 0.0, fps: 0.0, visual: false, note: String::new(), picture, size,
    })
}

/// Reads what the window needs off the UI thread, then shows it.
fn compress_choice(title: &str, input: &Path, make: fn(&Path) -> Result<preview::Spec, String>) -> Option<Vec<i32>> {
    let p = input.to_path_buf();
    match pump_job("Reading file", move |_| make(&p)) {
        Ok(spec) => preview::show(spec),
        Err(e) => { fail(title, e); None }
    }
}

// ---- options UI ----

/// Asks for what the tool needs. `None` means the user cancelled.
fn ask(id: &str, paths: &[PathBuf]) -> Option<Opts> {
    let first = paths.first()?;
    Some(match id {
        "img.compress" => {
            let v = compress_choice("Compress image", first, image_spec)?;
            opts(&[("quality", v[0].to_string()), ("max", v[1].to_string())])
        }
        "img.metadata" => metadata_flow("Image metadata", paths, |p| crate::metadata::read_image(p), "Remove everything else")?,
        "img.edit" => {
            let p = first.clone();
            let v = match pump_job("Reading file", move |_| image::load(&p)) {
                Ok(b) => crate::photo::show(b, paths.len())?,
                Err(_) => form::show("Edit photo", &[Field::Slider("Brightness", -100, 100, 0), Field::Slider("Contrast", -100, 100, 0), Field::Slider("Saturation", -100, 100, 0), choice("Rotate", &["None", "90 right", "90 left", "180"], 0), choice("Flip", &["None", "Horizontal", "Vertical"], 0)], "Apply")?,
            };
            opts(&[("brightness", v[0].clone()), ("contrast", v[1].clone()), ("saturation", v[2].clone()), ("rotate", v[3].clone()), ("flip", v[4].clone())])
        }
        "img.background" => {
            let fields = |margin: i32| {
                let names: Vec<String> = crate::fade::DIRECTIONS.iter().map(|(n, _)| n.to_string()).collect();
                vec![
                    Field::Colors("Colours. Two or more fade in this order", vec!["#FFFFFF".to_string()]),
                    Field::ChoiceIfMany(0, "Fade direction", names, 0),
                    Field::Slider("Margin (% of the longest side)", 0, 50, margin),
                    Field::Slider("Corner radius (% of the shortest side)", 0, 50, 0),
                ]
            };
            let p = first.clone();
            let v = match pump_job("Reading file", move |_| image::load(&p).and_then(thumbnail)) {
                // An opaque picture has nowhere for the colour to show except around it.
                Ok(b) => form::show_with_preview("Add background", &fields(if b.has_alpha() { 0 } else { 5 }), "Apply", Box::new(move |v| {
                    Some(background_preview(&b, &stops_of(v.first()?), crate::fade::direction_named(v.get(1)?), v.get(2)?.trim().parse().unwrap_or(0), v.get(3)?.trim().parse().unwrap_or(0)))
                }))?,
                Err(_) => form::show("Add background", &fields(5), "Apply")?,
            };
            opts(&[("colors", v[0].clone()), ("direction", v[1].clone()), ("margin", v[2].clone()), ("radius", v[3].clone())])
        }
        "img.collage" => {
            let fields = [choice("Columns", &["Auto", "2", "3", "4", "5"], 0), Field::Slider("Gap (pixels)", 0, 80, 12), Field::Color("Background", "#FFFFFF".into())];
            let ps = paths.to_vec();
            let loaded = pump_job("Reading files", move |_| {
                let mut out = vec![];
                let mut scale = 1f64;
                for p in &ps {
                    let b = image::load(p)?;
                    let long = b.w.max(b.h);
                    let t = thumbnail(b)?;
                    scale = scale.min(t.w.max(t.h) as f64 / long as f64);
                    out.push(t);
                }
                Ok((out, scale))
            });
            let v = match loaded {
                Ok((thumbs, scale)) => form::show_with_preview("Collage", &fields, "Create", Box::new(move |v| {
                    let cols = v.first()?.parse().unwrap_or_else(|_| auto_columns(thumbs.len()));
                    let gap = (v.get(1)?.trim().parse::<f64>().unwrap_or(12.0) * scale) as u32;
                    collage(&thumbs, cols, gap, image::parse_color(v.get(2)?)).ok()
                }))?,
                Err(_) => form::show("Collage", &fields, "Create")?,
            };
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
        "vid.annotate" => {
            let sh = pick_shapes_video("Annotate video", first, canvas::Mode::Annotate, "Save")?;
            opts(&[("shapes", sh)])
        }
        "vid.compress" => {
            let v = compress_choice("Compress video", first, video_spec)?;
            opts(&[("quality", v[0].to_string()), ("height", v[1].to_string()), ("audio", v[2].to_string()), ("fps", v[3].to_string())])
        }
        "vid.metadata" | "aud.metadata" => metadata_flow("Metadata", paths, |p| crate::metadata::read_media(p), "Remove everything else")?,
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
            let v = compress_choice("Compress audio", first, audio_spec)?;
            opts(&[("bitrate", v[0].to_string())])
        }
        "aud.channels" => {
            let v = form::show("Channels", &[choice("Output", &["Mono", "Stereo from mono", "Swap left and right", "Left only", "Right only"], 0)], "Apply")?;
            opts(&[("mode", v[0].clone())])
        }
        "pdf.compress" => {
            let v = compress_choice("Compress PDF", first, pdf_spec)?;
            opts(&[("dpi", if v[0] > 0 { v[0].to_string() } else { String::new() })])
        }
        "pdf.metadata" => metadata_flow("PDF metadata", paths, |p| crate::metadata::read_pdf(p), "Remove everything else")?,
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
    if id == "pdf.redact" {
        // Each document needs its own marks too.
        for p in paths {
            form::set_context(p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
            let Some(pages) = pick_pdf_redactions(&p) else { return };
            exec(id, vec![p], opts(&[("pages", pages)]));
        }
        return;
    }
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

/// A quality from 0 to 100, or one of the names the command line has always taken.
fn quality_percent(s: &str) -> i32 {
    match s { "Smaller file" => 60, "Higher quality" => 90, _ => s.trim().parse().unwrap_or(78) }
}
pub fn quality_of(s: &str) -> f32 { quality_percent(s).clamp(0, 99) as f32 / 100.0 }
fn crf_of(s: &str) -> u32 {
    match s { "Smaller file" => 28, "Higher quality" => 18, _ => s.trim().parse().map(quality::crf_for).unwrap_or(23) }
}

/// Output format for compressed audio: the same one if it is lossy and can go that low, otherwise one that can.
fn compressed_audio_ext(input_ext: &str, kbps: u32) -> String {
    match input_ext {
        "wma" if kbps < quality::WMA_FLOOR_KBPS => "m4a".to_string(),
        e @ ("mp3" | "m4a" | "ogg" | "opus" | "aac" | "wma") => e.to_string(),
        _ => "mp3".to_string(),
    }
}

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
    // Tiles fit the largest picture, so nothing is shrunk to make the sheet.
    let sizes: Vec<(u32, u32)> = imgs.iter().map(|i| (i.w, i.h)).collect();
    let (tw, th) = quality::collage_tile(&sizes, cols as u32, rows as u32, gap);
    let mut out = Bmp::new(cols as u32 * (tw + gap) + gap, rows as u32 * (th + gap) + gap, bg);
    for (i, im) in imgs.iter().enumerate() {
        let f = (tw as f64 / im.w as f64).min(th as f64 / im.h as f64).min(1.0);
        let (nw, nh) = (((im.w as f64 * f) as u32).max(1), ((im.h as f64 * f) as u32).max(1));
        let scaled;
        let sc = if (nw, nh) == (im.w, im.h) { im } else { scaled = im.scaled(nw, nh)?; &scaled };
        let (c, r) = ((i % cols) as u32, (i / cols) as u32);
        let x = gap + c * (tw + gap) + (tw - nw) / 2;
        let y = gap + r * (th + gap) + (th - nh) / 2;
        out.blit(sc, x as i32, y as i32);
    }
    Ok(out)
}

pub fn auto_columns(n: usize) -> usize { (n as f64).sqrt().ceil() as usize }

fn stops_of(colors: &str) -> Vec<[u8; 4]> { colors.split(',').map(image::parse_color).collect() }

/// The colours show through transparent pixels and around the picture, fading across the whole canvas when there are
/// several. `margin` is a percentage of the longest side and `radius` of the shortest, and the picture itself is copied
/// across without being resampled.
pub fn with_background(src: &Bmp, stops: &[[u8; 4]], direction: crate::fade::Direction, margin: u32, radius: u32) -> Bmp {
    let m = (src.w.max(src.h) as u64 * margin.min(100) as u64 / 100) as u32;
    let (w, h) = (src.w + m * 2, src.h + m * 2);
    let mut out = Bmp { w, h, px: crate::fade::paint(w, h, stops, direction) };
    let rad = src.w.min(src.h) as f32 * radius.min(50) as f32 / 100.0;
    if rad > 0.0 {
        let mut round = src.clone();
        round_corners(&mut round, rad);
        out.blit(&round, m as i32, m as i32);
    } else {
        out.blit(src, m as i32, m as i32);
    }
    out
}

/// Fades the corners out with a smooth edge. Everything past the curves stays exactly as it was.
fn round_corners(b: &mut Bmp, rad: f32) {
    let span = rad.ceil() as u32;
    for y in 0..b.h {
        // Only the rows and columns within a radius of a corner can change.
        if y >= span && y + span < b.h { continue; }
        let dy = rad - (y.min(b.h - 1 - y) as f32 + 0.5);
        for x in (0..b.w).filter(|x| *x < span || *x + span >= b.w) {
            let dx = rad - (x.min(b.w - 1 - x) as f32 + 0.5);
            if dx <= 0.0 || dy <= 0.0 { continue; }
            let cover = (rad - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
            let a = ((y * b.w + x) * 4 + 3) as usize;
            b.px[a] = (b.px[a] as f32 * cover).round() as u8;
        }
    }
}

/// The share of the longest side the preview always leaves room for.
const PREVIEW_ROOM: u32 = 15;

/// The result on a clear canvas big enough for a small margin, so the picture holds still while the colour grows around
/// it, and only shrinks to fit once a margin outgrows the room.
pub fn background_preview(src: &Bmp, stops: &[[u8; 4]], direction: crate::fade::Direction, margin: u32, radius: u32) -> Bmp {
    let done = with_background(src, stops, direction, margin, radius);
    let room = (src.w.max(src.h) as u64 * PREVIEW_ROOM as u64 / 100) as u32;
    let (w, h) = (src.w + room * 2, src.h + room * 2);
    if done.w >= w { return done; }
    let mut canvas = Bmp::new(w, h, [0, 0, 0, 0]);
    canvas.blit(&done, ((w - done.w) / 2) as i32, ((h - done.h) / 2) as i32);
    canvas
}

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

/// Tags to change. The streams themselves are copied by `quality::copy_attempts`.
fn metadata_args(o: &Opts) -> Vec<String> {
    let mut a = vec![];
    if get(o, "strip", "0") == "1" { a.extend(sv(&["-map_metadata", "-1"])); }
    // Only what was changed gets here. An empty one was cleared, and that is how ffmpeg is told to remove it.
    for k in ["title", "artist", "comment"] {
        if let Some(v) = o.get(k) { a.push("-metadata".into()); a.push(format!("{k}={}", v.trim())); }
    }
    a
}

/// Shows what a file holds, and only on Edit asks what to change. Cancelling the edit comes back to the view. Returns
/// only what changed, so that several files keep whatever else makes each of them different.
fn metadata_flow(title: &str, paths: &[PathBuf], read: impl Fn(&Path) -> Result<crate::metadata::Info, String> + Send + 'static, strip_label: &'static str) -> Option<Opts> {
    let first = paths.first()?.clone();
    let mut info = match pump_job("Reading file", move |_| read(&first)) {
        Ok(i) => i,
        Err(e) => { fail(title, e); return None; }
    };
    let sub = if paths.len() > 1 { format!("{}, first of {} files", form::context(), paths.len()) } else { form::context() };
    loop {
        let (back, edit) = crate::metadata::show(title, &sub, info);
        info = back;
        if !edit { return None; }
        let mut fields: Vec<Field> = info.editable.iter().map(|e| text(e.label, &e.value)).collect();
        if fields.is_empty() { fields.push(Field::Note("Details in this format can be removed, but not changed here.".into())); }
        fields.push(Field::Check(strip_label, false));
        let Some(v) = form::show(title, &fields, "Save") else { continue };
        let strip = v.last().cloned().unwrap_or_default();
        let mut out: Opts = info.editable.iter().zip(&v).filter(|(e, new)| new.trim() != e.value.trim()).map(|(e, new)| (e.key.to_string(), new.clone())).collect();
        if strip == "1" {
            // Whatever stays has to be written back, as the old copy is going.
            for (e, new) in info.editable.iter().zip(&v) { if !new.trim().is_empty() { out.insert(e.key.to_string(), new.clone()); } }
            out.insert("strip".to_string(), strip);
        }
        if !out.is_empty() { return Some(out); }
    }
}

const NO_LOSSLESS: &str = "Could not read this video's format, so it cannot be edited without loss. Put ffprobe.exe next to ffmpeg.exe";

fn tell(j: &Job, msg: String) { if j.total == 1 { *j.note.lock().unwrap() = Some(msg); } }

/// An edit that must leave every other pixel as it is. A GIF has no lossless path through a filter, since its palette
/// is rebuilt, but keeps as many colours per frame as the format holds.
fn lossless_edit(j: &Job, p: &Path, suffix: &str, filter: Option<&str>, audio: Option<&str>, drop_timed: bool) -> Result<PathBuf, String> {
    let e = ext_of(p);
    if e == "gif" { return media::transform(j, p, suffix, None, quality::gif_edit_args(filter)); }
    let l = quality::lossless_args(&media::probe(p), &e, filter, audio, drop_timed).ok_or(NO_LOSSLESS)?;
    if l.ext != e { tell(j, format!("Saved as {} to keep it lossless", l.ext.to_ascii_uppercase())); }
    media::transform(j, p, suffix, Some(&l.ext), l.args)
}

/// Cuts a range of video exactly. Copies the original bits when the range allows that, otherwise encodes losslessly.
fn cut_video(j: &Job, p: &Path, a: f64, b: f64, out: impl Fn(&str) -> PathBuf) -> Result<PathBuf, String> {
    let e = ext_of(p);
    let (seek, from) = quality::cut_seek(&e, a, b);
    let with_seek = |args: &[String]| -> Vec<String> { from.iter().chain(args).cloned().collect() };
    let total = b - a;
    if e == "gif" {
        let o = out("gif");
        return atomically(&o, |tmp| media::ffmpeg_from(j, &seek, &[p], &with_seek(&quality::gif_edit_args(None)), tmp, total));
    }
    let pr = media::probe(p);
    let fps = pr.video.as_ref().map(|v| v.fps).unwrap_or(0.0);
    // Where seeking is loose, only the very start is certain to be a keyframe.
    let startable = if quality::seeks_loosely(&e) { a <= 0.0 } else { media::is_keyframe(p, a, fps) };
    if quality::trim_can_copy(startable, b, media::duration(p), fps) {
        let o = out(&e);
        for args in quality::copy_attempts(&[], &quality::trim_copy_tail()) {
            match atomically(&o, |tmp| media::ffmpeg_from(j, &seek, &[p], &with_seek(&args), tmp, total)) {
                Ok(r) => return Ok(r),
                Err(x) if j.cancelled() => return Err(x),
                Err(_) => {}
            }
        }
    }
    let l = quality::lossless_args(&pr, &e, None, None, false).ok_or(NO_LOSSLESS)?;
    if l.ext != e { tell(j, format!("Saved as {} to keep it lossless", l.ext.to_ascii_uppercase())); }
    let o = out(&l.ext);
    atomically(&o, |tmp| media::ffmpeg_from(j, &seek, &[p], &with_seek(&l.args), tmp, total))
}

/// Two clips from the same encode join without touching a frame.
fn join_by_copy(j: &Job, ps: &[PathBuf], total: f64) -> Option<PathBuf> {
    let t = TempDir::new().ok()?;
    let list = t.file("list.txt");
    let names: Vec<String> = ps.iter().map(|p| p.display().to_string()).collect();
    std::fs::write(&list, quality::concat_list(&names)).ok()?;
    let out = sibling(&ps[0], " joined", &ext_of(&ps[0]));
    atomically(&out, |tmp| media::ffmpeg_from(j, &sv(&["-f", "concat", "-safe", "0"]), &[list.as_path()], &sv(&["-map", "0", "-c", "copy", "-map_metadata", "0"]), tmp, total)).ok()
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
            let res = save_new(&b, sibling(p, " compressed", &ext), &SaveOpts { quality: quality_of(get(&o, "quality", "")), fit: None, ..Default::default() });
            if resized { res } else { only_if_smaller(j, p, res) }
        }),
        "img.metadata" => each("Saving metadata", paths, move |j, p| {
            let e = ext_of(p);
            let tags: Vec<(&str, &str)> = ["title", "artist", "copyright"].iter().filter_map(|k| o.get(*k).map(|v| (*k, v.trim()))).collect();
            // Asked for nothing in particular, it means taking everything off.
            let strip = get(&o, "strip", "0") == "1" || tags.is_empty();
            let out = sibling(p, if tags.is_empty() { " clean" } else { " edited" }, &e);
            // JPEG and PNG change without being re-encoded.
            let original = std::fs::read(p).map_err(|x| x.to_string())?;
            let base = if strip { image::strip_metadata(&e, &original) } else { Some(original) };
            if let Some(base) = base {
                j.set_frac(0.5);
                let done = if tags.is_empty() { base } else { crate::metadata::write_image_tags(&e, &base, &tags)? };
                return atomically(&out, |tmp| std::fs::write(tmp, &done).map_err(|x| x.to_string()));
            }
            // Decoding and saving again carries no metadata across, only the colour profile.
            let n = image::load_native(p)?;
            atomically(&out, |tmp| image::save_native(&n, tmp, &SaveOpts::default()))
        }),
        "img.edit" => each("Editing", paths, move |j, p| {
            let mut a = vec!["-vf".to_string(), edit_filter(&o).join(","), "-frames:v".to_string(), "1".to_string(), "-update".to_string(), "1".to_string()];
            a.extend(quality::image_encode_args(&ext_of(p)));
            media::transform(j, p, " edited", None, a)
        }),
        "img.background" => each("Adding background", paths, move |j, p| {
            let src = image::load(p)?;
            let margin: u32 = get(&o, "margin", "0").trim().parse().unwrap_or(0);
            let radius: u32 = get(&o, "radius", "0").trim().parse().unwrap_or(0);
            // The result would be the same picture under a new name.
            if margin == 0 && radius == 0 && !src.has_alpha() {
                tell(j, "No transparent areas to fill. Add a margin or round the corners.".to_string());
                return Ok(p.to_path_buf());
            }
            let colors = get(&o, "colors", get(&o, "color", "#FFFFFF"));
            let bg = with_background(&src, &stops_of(colors), crate::fade::direction_named(get(&o, "direction", "")), margin, radius);
            save_new(&bg, sibling(p, " background", "png"), &SaveOpts::default())
        }),
        "img.crop" => each("Cropping", paths, move |_, p| {
            let sh = shapes.first().ok_or("Nothing selected to crop")?;
            let n = image::load_native(p)?;
            let (fw, fh) = n.size();
            let (x, y, w, h) = crop_px(sh, fw, fh, false);
            let c = n.cropped(x, y, w, h)?;
            let out = sibling(p, " cropped", &ext_of(p));
            atomically(&out, |tmp| image::save_native(&c, tmp, &SaveOpts::default()))
        }),
        "img.redact" => each("Redacting", paths, move |j, p| {
            let n = image::load_native(p)?;
            let (fw, fh) = n.size();
            n.apply(&canvas::marks_for(&shapes, fw, fh, false))?;
            // Saving a JPEG again would change every pixel, so anything lossy becomes PNG. Metadata does not
            // carry over, and with it goes any embedded thumbnail of the unredacted picture.
            let e = ext_of(p);
            let keep = quality::lossless_image_ext(&e);
            if keep != e { tell(j, format!("Saved as {} so nothing else changes", keep.to_ascii_uppercase())); }
            let out = sibling(p, " redacted", keep);
            atomically(&out, |tmp| image::save_native(&n, tmp, &SaveOpts::default()))
        }),
        "img.annotate" => each("Annotating", paths, move |_, p| {
            let mut b = image::load(p)?;
            canvas::apply_annotations(&mut b, &shapes)?;
            save_new(&b, sibling(p, " annotated", &ext_of(p)), &SaveOpts::default())
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
            let pr = media::probe(p);
            let positive = |k: &str| get(&o, k, "").parse::<u32>().ok().filter(|n| *n > 0);
            let h = positive("height");
            let a = quality::compress_video_args(&pr, &quality::Squeeze { crf: crf_of(get(&o, "quality", "")), height: h, audio_kbps: positive("audio"), fps: positive("fps") });
            let resized = match (h, &pr.video) { (Some(h), Some(v)) => v.display_size().1 > h, _ => false };
            let res = media::transform(j, p, " compressed", Some("mp4"), a);
            if resized { res } else { only_if_smaller(j, p, res) }
        }),
        "vid.metadata" | "aud.metadata" => each("Saving metadata", paths, move |j, p| {
            media::attempts(j, p, " edited", &ext_of(p), &quality::copy_attempts(&[], &metadata_args(&o)))
        }),
        "vid.mute" => each("Muting", paths, |j, p| {
            let sets = [sv(&["-map", "0", "-map", "-0:a", "-c", "copy"]), sv(&["-map", "0:v", "-map", "0:s?", "-c", "copy"])];
            media::attempts(j, p, " muted", &ext_of(p), &sets)
        }),
        "vid.speed" => each("Changing speed", paths, move |j, p| {
            let f: f64 = get(&o, "speed", "2").parse().map_err(|_| "Bad speed")?;
            if f <= 0.0 { return Err("Bad speed".into()); }
            // Every frame stays, however fast, and subtitles and chapters would no longer line up.
            let filter = format!("setpts=PTS/{f}");
            let tempo = media::atempo_chain(f);
            lossless_edit(j, p, &format!(" {f}x"), Some(&filter), Some(&tempo), true)
        }),
        "vid.crop" => each("Cropping", paths, move |j, p| {
            let sh = shapes.first().ok_or("Nothing selected to crop")?;
            let (w, h) = media::size(p).ok_or("Could not read the video size")?;
            let (x, y, cw, ch) = crop_px(sh, w, h, true);
            lossless_edit(j, p, " cropped", Some(&format!("crop={cw}:{ch}:{x}:{y}")), None, false)
        }),
        "vid.redact" => each("Redacting", paths, move |j, p| {
            if shapes.is_empty() { return Err("Nothing marked to redact".into()); }
            if ext_of(p) == "gif" {
                let bytes = std::fs::read(p).map_err(|e| e.to_string())?;
                let (w, h) = crate::gif::screen_size(&bytes).ok_or("Could not read this GIF")?;
                let marks = canvas::marks_for(&shapes, w, h, false);
                // Hard solid marks go in place, so palettes, timing and everything outside them stay as they are.
                if marks.iter().all(|m| m.is_hard_solid()) {
                    let clean = crate::gif::redact(&bytes, &marks)?;
                    let out = sibling(p, " redacted", "gif");
                    return atomically(&out, |tmp| std::fs::write(tmp, &clean).map_err(|e| e.to_string()));
                }
                // A blur or feather needs colours the palette may not have, so each frame gets a new one.
                let g = quality::redact_graph("bgra", "", true, w, h, &marks).ok_or(NO_LOSSLESS)?;
                return media::transform(j, p, " redacted", None, quality::gif_edit_args(Some(&format!("format=gbrap,{g}"))));
            }
            let pr = media::probe(p);
            let v = pr.video.as_ref().ok_or(NO_LOSSLESS)?;
            let (w, h) = v.display_size();
            let marks = canvas::marks_for(&shapes, w, h, true);
            let f = quality::redact_graph(&v.pix_fmt, &v.color_space, v.full_range, w, h, &marks).ok_or(NO_LOSSLESS)?;
            let e = ext_of(p);
            let l = quality::redact_args(&pr, &e, &f).ok_or(NO_LOSSLESS)?;
            if l.ext != e { tell(j, format!("Saved as {} to keep it lossless", l.ext.to_ascii_uppercase())); }
            media::transform(j, p, " redacted", Some(&l.ext), l.args)
        }),
        "vid.annotate" => each("Annotating", paths, move |j, p| {
            if shapes.is_empty() { return Err("Nothing drawn to add".into()); }
            // The annotations are drawn once, at the size the video shows at, into a clear picture laid over every frame.
            let (w, h) = media::size(p).ok_or("Could not read the video size")?;
            let layer = Bmp { w, h, px: canvas::annotation_layer(w, h, &shapes)? };
            let t = TempDir::new()?;
            let png = t.file("annotations.png");
            image::save(&layer, &png, &SaveOpts::default())?;
            let e = ext_of(p);
            let (ext, args) = if e == "gif" { (e.clone(), quality::gif_edit_args(None)) } else {
                let l = quality::lossless_args(&media::probe(p), &e, None, None, false).ok_or(NO_LOSSLESS)?;
                if l.ext != e { tell(j, format!("Saved as {} to keep it lossless", l.ext.to_ascii_uppercase())); }
                (l.ext, l.args)
            };
            let args = quality::overlay_args(&args).ok_or(NO_LOSSLESS)?;
            let total = media::duration(p);
            atomically(&sibling(p, " annotated", &ext), |tmp| media::ffmpeg(j, &[p, png.as_path()], &args, tmp, total))
        }),
        "vid.trim" | "aud.trim" => each("Trimming", paths, move |j, p| {
            let (a, b) = match marks[..] { [a, b, ..] => (a, b), _ => return Err("Needs a start and an end".into()) };
            if classify(p) == Cat::Audio {
                let e = ext_of(p);
                let out = sibling(p, " trimmed", &e);
                let args = quality::audio_trim_args(&media::probe(p), &e);
                return atomically(&out, |tmp| media::ffmpeg_from(j, &quality::seek_args(a, b), &[p], &args, tmp, b - a));
            }
            cut_video(j, p, a, b, |e| sibling(p, " trimmed", e))
        }),
        "vid.split" => each("Splitting", paths, move |j, p| {
            let dur = media::duration(p);
            let dir = sibling_dir(p, " parts")?;
            let stem = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
            let mut cuts = vec![0.0];
            cuts.extend(marks.iter().copied().filter(|m| *m > 0.0 && *m < dur));
            cuts.push(dur);
            for (i, w) in cuts.windows(2).enumerate() {
                if j.cancelled() { return Err("Cancelled".into()); }
                j.set_frac(i as f32 / (cuts.len() - 1) as f32);
                cut_video(j, p, w[0], w[1], |e| dir.join(format!("{stem} part {}.{e}", i + 1)))?;
            }
            Ok(dir)
        }),
        "vid.snapshot" => each("Saving snapshots", paths, move |j, p| {
            let ff = crate::ffmpeg().ok_or("Needs ffmpeg")?;
            let dir = if marks.len() > 1 { Some(sibling_dir(p, " snapshots")?) } else { None };
            let mut last = PathBuf::new();
            let filter = media::probe(p).video.as_ref().and_then(quality::still_filter);
            let vf = filter.map(|f| vec!["-vf".to_string(), f]).unwrap_or_default();
            for (i, t) in marks.iter().enumerate() {
                j.set_frac(i as f32 / marks.len() as f32);
                last = match &dir { Some(d) => d.join(format!("frame-{:03}.png", i + 1)), None => sibling(p, " snapshot", "png") };
                for at in [*t, (*t - END_MARGIN).max(0.0)] {
                    let (before, after) = quality::still_seek(&ext_of(p), at);
                    run(Some(j), &ff, &sv(&["-y", "-hide_banner"]).into_iter().chain(before).chain(sv(&["-i", &p.display().to_string()])).chain(after).chain(vf.clone()).chain(sv(&["-frames:v", "1", "-sws_flags", "lanczos+accurate_rnd+full_chroma_int", &last.display().to_string()])).collect::<Vec<_>>(), None)?;
                    if last.exists() { break; }
                }
            }
            if !last.exists() { return Err("No picture could be read at that moment".into()); }
            Ok(dir.unwrap_or(last))
        }),
        "vid.join" => together("Joining videos", paths, |j, ps| {
            let probes: Vec<quality::Probe> = ps.iter().map(|p| media::probe(p)).collect();
            let exts: Vec<String> = ps.iter().map(|p| ext_of(p)).collect();
            let total: f64 = ps.iter().map(|p| media::duration(p)).sum();
            if quality::can_concat_copy(&probes, &exts) {
                if let Some(done) = join_by_copy(j, ps, total) { return Ok(done); }
                if j.cancelled() { return Err("Cancelled".into()); }
            }
            // Otherwise the richest clip sets the format, so nothing is shrunk, slowed or flattened to match the first.
            let spec = quality::join_spec(&probes);
            let silent: Vec<Option<f64>> = ps.iter().zip(&probes).map(|(p, pr)| if pr.audio.is_empty() { Some(media::duration(p)) } else { None }).collect();
            let (ext, tail) = quality::join_output_args(&spec, &probes);
            let mut a = vec!["-filter_complex".to_string(), quality::join_filter(&spec, &silent)];
            a.extend(tail);
            let inputs: Vec<&Path> = ps.iter().map(PathBuf::as_path).collect();
            let out = sibling(&ps[0], " joined", &ext);
            atomically(&out, |tmp| media::ffmpeg(j, &inputs, &a, tmp, total))
        }),

        "aud.compress" => each("Compressing", paths, move |j, p| {
            let kbps = get(&o, "bitrate", "128").trim().parse().unwrap_or(128);
            let ext = compressed_audio_ext(&ext_of(p), kbps);
            let info = media::probe(p).audio.first().cloned().unwrap_or_default();
            let mut a = sv(&["-vn", "-b:a", &format!("{kbps}k")]);
            a.extend(quality::audio_squeeze_args(":a", &ext, &info, kbps));
            only_if_smaller(j, p, media::transform(j, p, " compressed", Some(&ext), a))
        }),
        "aud.normalize" => each("Normalizing", paths, |j, p| {
            media::transform(j, p, " normalized", None, quality::audio_edit_args(&media::probe(p), &ext_of(p), "loudnorm=I=-16:TP=-1.5:LRA=11"))
        }),
        "aud.visualize" => each("Rendering video", paths, |j, p| {
            let mut a = sv(&["-filter_complex", "[0:a]showwaves=s=1280x720:mode=cline:rate=30:colors=0xC8F135,format=yuv420p[v]", "-map", "[v]", "-c:v", "libx264", "-preset", "slow", "-crf", quality::CONVERT_X264_CRF, "-shortest"]);
            a.extend(quality::audio_target_args(&media::probe(p), "mp4", false));
            media::transform(j, p, " visualized", Some("mp4"), a)
        }),
        "aud.channels" => each("Changing channels", paths, move |j, p| {
            media::transform(j, p, " channels", None, quality::audio_edit_args(&media::probe(p), &ext_of(p), channels_filter(get(&o, "mode", "Mono"))))
        }),
        "aud.bleep" => each("Bleeping", paths, move |j, p| {
            let ranges: Vec<(f64, f64)> = marks.chunks_exact(2).map(|c| (c[0], c[1])).collect();
            if ranges.is_empty() { return Err("Mark at least one range".into()); }
            let pr = media::probe(p);
            let rate = pr.audio.first().map(|a| a.rate).filter(|r| *r > 0).unwrap_or(48000);
            let mut a = vec!["-filter_complex".to_string(), media::bleep_filter(&ranges, rate), "-map".to_string(), "[out]".to_string()];
            a.extend(quality::audio_graph_args(&pr, &ext_of(p)));
            media::transform(j, p, " bleeped", None, a)
        }),

        "pdf.compress" => each("Compressing", paths, move |j, p| only_if_smaller(j, p, crate::pdf::compress(j, p, get(&o, "dpi", "").parse().ok()))),
        "pdf.metadata" => each("Saving metadata", paths, move |_, p| {
            // Only what was changed gets here. An empty one was cleared.
            let f: Vec<(&str, String)> = [("Title", "title"), ("Author", "author"), ("Subject", "subject")].iter().filter_map(|(name, k)| o.get(*k).map(|v| (*name, v.clone()))).collect();
            crate::pdf::set_metadata(p, &f, get(&o, "strip", "0") == "1")
        }),
        "pdf.split" => each("Splitting", paths, |j, p| crate::pdf::split(j, p)),
        "pdf.redact" => each("Redacting", paths, move |j, p| {
            let marked = decode_pages(get(&o, "pages", ""));
            let n = marked.len();
            let out = crate::pdf::redact(j, p, &marked)?;
            tell(j, format!("{n} redacted {} saved as {}, so nothing under a mark survives", if n == 1 { "page" } else { "pages" }, if n == 1 { "an image" } else { "images" }));
            Ok(out)
        }),
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
    fn compressed_audio_keeps_its_format_while_the_format_can_go_that_low() {
        assert_eq!(compressed_audio_ext("wma", 64), "wma");
        assert_eq!(compressed_audio_ext("wma", 8), "m4a");
        assert_eq!(compressed_audio_ext("opus", 8), "opus");
        assert_eq!(compressed_audio_ext("wav", 128), "mp3");
    }
    #[test]
    fn levels_are_read_as_percentages_and_old_names_still_work() {
        assert_eq!(quality_of("45"), 0.45);
        assert_eq!(quality_of("Higher quality"), 0.9);
        assert_eq!(crf_of("80"), 23);
        assert_eq!(crf_of("Smaller file"), 28);
    }
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
        assert_eq!(c.w, 2 * 110 + 10, "tiles fit the largest picture");
        assert_eq!(c.h, 2 * 110 + 10);
    }
    #[test]
    fn background_shows_around_an_opaque_picture_and_through_a_clear_one() {
        let red = [0, 0, 255, 255];
        let opaque = Bmp::new(100, 50, [10, 20, 30, 255]);
        let dir = crate::fade::DIRECTIONS[0].1;
        let plain = with_background(&opaque, &[red], dir, 0, 0);
        assert_eq!(plain.px, opaque.px, "with no margin an opaque picture has nowhere to show it");
        let framed = with_background(&opaque, &[red], dir, 10, 0);
        assert_eq!((framed.w, framed.h), (120, 70));
        assert_eq!(&framed.px[..4], &red, "corner is the colour");
        let at = ((10 * framed.w + 10) * 4) as usize;
        assert_eq!(&framed.px[at..at + 4], &[10, 20, 30, 255], "picture starts past the margin, unchanged");
        let clear = with_background(&Bmp::new(4, 4, [9, 9, 9, 0]), &[red], dir, 0, 0);
        assert_eq!(&clear.px[..4], &red, "transparent pixels fill in");
        let blue = [255, 0, 0, 255];
        let faded = with_background(&opaque, &[red, blue], dir, 10, 0);
        assert_eq!(&faded.px[..4], &red, "first colour at the top left");
        assert_eq!(&faded.px[faded.px.len() - 4..], &blue, "last at the bottom right");
        let at = ((10 * faded.w + 10) * 4) as usize;
        assert_eq!(&faded.px[at..at + 4], &[10, 20, 30, 255], "the picture is untouched by the fade");
        assert_eq!(stops_of("#FF0000,#0000FF"), vec![red, blue]);
    }
    #[test]
    fn rounded_corners_show_the_colour_and_leave_the_rest_alone() {
        let red = [0, 0, 255, 255];
        let pic = [10, 20, 30, 255];
        let dir = crate::fade::DIRECTIONS[0].1;
        let round = with_background(&Bmp::new(100, 100, pic), &[red], dir, 0, 20);
        assert_eq!((round.w, round.h), (100, 100), "no margin, so the size stays");
        for (x, y) in [(0u32, 0u32), (99, 0), (0, 99), (99, 99)] {
            let at = ((y * 100 + x) * 4) as usize;
            assert_eq!(&round.px[at..at + 4], &red, "corner {x},{y} is the colour");
        }
        for (x, y) in [(50u32, 0u32), (0, 50), (99, 50), (50, 99), (50, 50), (20, 20)] {
            let at = ((y * 100 + x) * 4) as usize;
            assert_eq!(&round.px[at..at + 4], &pic, "{x},{y} is untouched");
        }
        let full = with_background(&Bmp::new(100, 100, pic), &[red], dir, 0, 50);
        let at = ((10 * 100 + 10) * 4) as usize;
        assert_eq!(&full.px[at..at + 4], &red, "at 50% the picture is a circle");
    }
    #[test]
    fn preview_holds_the_picture_still_until_the_margin_outgrows_its_room() {
        let dir = crate::fade::DIRECTIONS[0].1;
        let pic = Bmp::new(100, 50, [10, 20, 30, 255]);
        let size = |m| { let b = background_preview(&pic, &[[0, 0, 255, 255]], dir, m, 0); (b.w, b.h) };
        assert_eq!(size(0), (130, 80));
        assert_eq!(size(10), (130, 80), "same canvas, so the picture does not change size");
        assert_eq!(size(15), (130, 80));
        assert_eq!(size(30), (160, 110), "past the room the whole result fits instead");
    }
    #[test]
    fn columns_are_square_ish() { assert_eq!(auto_columns(4), 2); assert_eq!(auto_columns(5), 3); }
}
