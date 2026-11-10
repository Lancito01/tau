//! Audio and video through ffmpeg.
use crate::job::*;
use crate::quality;
use std::path::{Path, PathBuf};

pub fn s(v: &[&str]) -> Vec<String> { v.iter().map(|x| x.to_string()).collect() }

pub fn ffprobe() -> Option<PathBuf> {
    let ff = crate::ffmpeg()?;
    let probe = ff.with_file_name("ffprobe.exe");
    Some(if probe.exists() { probe } else { PathBuf::from("ffprobe") })
}

pub fn duration(p: &Path) -> f64 { probe(p).duration }

/// What is in the file. Empty when ffprobe is missing or cannot read it.
/// Starting ffprobe costs far more than anything it reads, and a single operation asks several times, so an answer is
/// kept for as long as the file has the same size and modified time.
pub fn probe(p: &Path) -> quality::Probe {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    type Key = (PathBuf, u64, Option<std::time::SystemTime>);
    static CACHE: OnceLock<Mutex<HashMap<Key, quality::Probe>>> = OnceLock::new();
    let Some(exe) = ffprobe() else { return Default::default() };
    let key = std::fs::metadata(p).ok().map(|m| (p.to_path_buf(), m.len(), m.modified().ok()));
    let cache = CACHE.get_or_init(Default::default);
    if let Some(k) = &key {
        if let Some(hit) = cache.lock().unwrap().get(k) { return hit.clone(); }
    }
    let Ok(out) = run(None, &exe, &quality::probe_args(&p.display().to_string()), None) else { return Default::default() };
    let found = quality::parse_probe(&out);
    if let Some(k) = key {
        let mut c = cache.lock().unwrap();
        if c.len() > 256 { c.clear(); }
        c.insert(k, found.clone());
    }
    found
}

/// True when the frame nearest `t` is a keyframe.
pub fn is_keyframe(p: &Path, t: f64, fps: f64) -> bool {
    let Some(exe) = ffprobe() else { return false };
    run(None, &exe, &quality::keyframe_probe_args(&p.display().to_string(), t), None).is_ok_and(|o| quality::is_keyframe_at(&o, t, fps))
}

/// Runs ffmpeg with progress reported on the job. `args` excludes the leading -i and the output.
pub fn ffmpeg(job: &Job, inputs: &[&Path], args: &[String], out: &Path, total: f64) -> Result<(), String> {
    ffmpeg_from(job, &[], inputs, args, out, total)
}

/// Same, with options that go before the first input, such as a seek.
pub fn ffmpeg_from(job: &Job, before: &[String], inputs: &[&Path], args: &[String], out: &Path, total: f64) -> Result<(), String> {
    if crate::catalog::ext_of(out) == "gif" && inputs.len() == 1 {
        match gif_in_parts(job, before, inputs[0], args, out, total) {
            Ok(()) => return Ok(()),
            Err(e) if job.cancelled() => return Err(e),
            // Anything unexpected goes the plain way, which gives the same file.
            Err(_) => job.set_frac(0.0),
        }
    }
    let on_time = |t: f64| if total > 0.0 { job.set_frac((t / total) as f32) };
    ffmpeg_timed(job, before, inputs, args, out, &on_time)
}

/// Runs ffmpeg, reporting each output time in seconds as it goes.
fn ffmpeg_timed(job: &Job, before: &[String], inputs: &[&Path], args: &[String], out: &Path, on_time: &(dyn Fn(f64) + Sync)) -> Result<(), String> {
    let ff = crate::ffmpeg().ok_or("Needs ffmpeg")?;
    let mut a = s(&["-y", "-hide_banner", "-nostdin", "-progress", "pipe:1", "-nostats"]);
    a.extend_from_slice(before);
    for i in inputs { a.push("-i".into()); a.push(i.display().to_string()); }
    a.extend_from_slice(args);
    if let Some(m) = quality::muxer_for(&crate::catalog::ext_of(out)) { a.push("-f".into()); a.push(m.into()); }
    a.push(out.display().to_string());
    let cb = |line: &str| {
        if let Some(Ok(us)) = line.strip_prefix("out_time_us=").map(str::parse::<f64>) { on_time(us / 1e6); }
    };
    run(Some(job), &ff, &a, Some(&cb)).map(|_| ())
}

/// Shortest stretch worth its own ffmpeg. Starting one costs a fraction of a second, and past eight parts the time
/// saved is lost to starting them.
const GIF_PART_MIN: f64 = 0.5;

/// A GIF's palettes are built frame by frame on one core, by far the slowest part of making one. Each frame's palette
/// depends on that frame alone, so stretches of it are encoded side by side and joined. Every part decodes from the
/// same start with the same filters, so each frame and its delay come out as one pass would make them.
fn gif_in_parts(job: &Job, before: &[String], input: &Path, args: &[String], out: &Path, total: f64) -> Result<(), String> {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(8);
    let n = cores.min((total / GIF_PART_MIN) as usize);
    if n < 2 { return Err("Too short to split".into()); }
    let cut = |k: usize| total * k as f64 / n as f64;
    let parts = (0..n).map(|k| {
        let from = (k > 0).then(|| cut(k));
        let to = (k + 1 < n).then(|| cut(k + 1));
        quality::gif_part_args(args, from, to).ok_or_else(|| "This GIF cannot be made in parts".to_string())
    }).collect::<Result<Vec<_>, _>>()?;
    let tmp = TempDir::new()?;
    let files: Vec<PathBuf> = (0..n).map(|k| tmp.file(&format!("part{k}.gif"))).collect();
    let fracs = std::sync::Mutex::new(vec![0f32; n]);
    let results: Vec<Result<(), String>> = std::thread::scope(|sc| {
        let handles: Vec<_> = parts.iter().zip(&files).enumerate().map(|(k, (a, f))| {
            let fracs = &fracs;
            sc.spawn(move || {
                let (lo, hi) = (cut(k), cut(k + 1));
                let on_time = move |t: f64| {
                    let mut v = fracs.lock().unwrap();
                    v[k] = ((t - lo) / (hi - lo)).clamp(0.0, 1.0) as f32;
                    job.set_frac(v.iter().sum::<f32>() / v.len() as f32);
                };
                ffmpeg_timed(job, before, &[input], a, f, &on_time)
            })
        }).collect();
        handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("A GIF part failed".into()))).collect()
    });
    for r in results { r?; }
    let bytes = files.iter().map(|f| std::fs::read(f).map_err(|e| e.to_string())).collect::<Result<Vec<_>, _>>()?;
    let joined = crate::gif::splice(&bytes)?;
    if crate::gif::frame_count(&joined)? == 0 { return Err("No frames".into()); }
    std::fs::write(out, joined).map_err(|e| e.to_string())
}

/// Runs each set of arguments until one works, best first. Only the last failure is worth reporting.
pub fn attempts(job: &Job, input: &Path, suffix: &str, ext: &str, sets: &[Vec<String>]) -> Result<PathBuf, String> {
    let out = sibling(input, suffix, ext);
    let total = duration(input);
    let mut last = "Nothing to do".to_string();
    for args in sets {
        match atomically(&out, |tmp| ffmpeg(job, &[input], args, tmp, total)) {
            Ok(p) => return Ok(p),
            Err(e) if job.cancelled() => return Err(e),
            Err(e) => last = e,
        }
    }
    Err(last)
}

pub fn convert(job: &Job, input: &Path, target: &str, speed: quality::Speed) -> Result<PathBuf, String> {
    attempts(job, input, "", target, &quality::convert_attempts(&probe(input), target, speed))
}

/// One input to one output beside it, with extra arguments.
pub fn transform(job: &Job, input: &Path, suffix: &str, ext: Option<&str>, args: Vec<String>) -> Result<PathBuf, String> {
    let e = ext.map(str::to_string).unwrap_or_else(|| crate::catalog::ext_of(input));
    attempts(job, input, suffix, &e, &[args])
}

/// atempo only accepts 0.5-2.0 per stage, so larger factors are chained.
pub fn atempo_chain(mut f: f64) -> String {
    let mut parts = vec![];
    while f > 2.0 { parts.push("atempo=2.0".to_string()); f /= 2.0; }
    while f < 0.5 { parts.push("atempo=0.5".to_string()); f /= 0.5; }
    parts.push(format!("atempo={f:.4}"));
    parts.join(",")
}

/// Mutes the given ranges and lays a tone over them.
pub fn bleep_filter(ranges: &[(f64, f64)], rate: u32) -> String {
    let cond = |rs: &[(f64, f64)]| rs.iter().map(|(a, b)| format!("between(t,{a:.3},{b:.3})")).collect::<Vec<_>>().join("+");
    let c = cond(ranges);
    format!("[0:a]volume=enable='{c}':volume=0[m];sine=f=1000:r={rate},volume=0.25,volume=enable='not({c})':volume=0[t];[m][t]amix=inputs=2:duration=first:normalize=0[out]")
}

/// Width and height of the first video stream as it is shown, which is what filters work on.
/// Phone video is stored sideways with a rotation tag, so the stored size would misplace every crop and redaction.
pub fn size(p: &Path) -> Option<(u32, u32)> {
    let v = probe(p).video?;
    (v.width > 0 && v.height > 0).then(|| v.display_size())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn atempo_chains_outside_range() {
        assert_eq!(atempo_chain(4.0).matches("atempo").count(), 2);
        assert_eq!(atempo_chain(1.5).matches("atempo").count(), 1);
        assert_eq!(atempo_chain(0.25).matches("atempo").count(), 2);
    }
    #[test]
    fn bleep_covers_each_range() { assert!(bleep_filter(&[(1.0, 2.0), (3.0, 4.0)], 48000).contains("between(t,3.000,4.000)")); }
}
