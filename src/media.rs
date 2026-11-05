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

pub fn duration(p: &Path) -> f64 {
    let Some(probe) = ffprobe() else { return 0.0 };
    let a = s(&["-v", "error", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1", &p.display().to_string()]);
    run(None, &probe, &a, None).ok().and_then(|o| o.trim().parse().ok()).unwrap_or(0.0)
}

/// What is in the file. Empty when ffprobe is missing or cannot read it.
pub fn probe(p: &Path) -> quality::Probe {
    let Some(exe) = ffprobe() else { return Default::default() };
    run(None, &exe, &quality::probe_args(&p.display().to_string()), None).map(|o| quality::parse_probe(&o)).unwrap_or_default()
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
    let ff = crate::ffmpeg().ok_or("Needs ffmpeg")?;
    let mut a = s(&["-y", "-hide_banner", "-nostdin", "-progress", "pipe:1", "-nostats"]);
    a.extend_from_slice(before);
    for i in inputs { a.push("-i".into()); a.push(i.display().to_string()); }
    a.extend_from_slice(args);
    if let Some(m) = quality::muxer_for(&crate::catalog::ext_of(out)) { a.push("-f".into()); a.push(m.into()); }
    a.push(out.display().to_string());
    let cb = |line: &str| {
        if let Some(v) = line.strip_prefix("out_time_us=") {
            if let (Ok(us), true) = (v.parse::<f64>(), total > 0.0) { job.set_frac((us / 1e6 / total) as f32); }
        }
    };
    run(Some(job), &ff, &a, Some(&cb)).map(|_| ())
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

pub fn convert(job: &Job, input: &Path, target: &str) -> Result<PathBuf, String> {
    attempts(job, input, "", target, &quality::convert_attempts(&probe(input), target))
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
