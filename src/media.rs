//! Audio and video through ffmpeg.
use crate::job::*;
use std::path::{Path, PathBuf};

pub fn s(v: &[&str]) -> Vec<String> { v.iter().map(|x| x.to_string()).collect() }

pub fn duration(p: &Path) -> f64 {
    let Some(ff) = crate::ffmpeg() else { return 0.0 };
    let probe = ff.with_file_name("ffprobe.exe");
    let probe = if probe.exists() { probe } else { PathBuf::from("ffprobe") };
    let a = s(&["-v", "error", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1", &p.display().to_string()]);
    run(None, &probe, &a, None).ok().and_then(|o| o.trim().parse().ok()).unwrap_or(0.0)
}

/// Runs ffmpeg with progress reported on the job. `args` excludes the leading -i and the output.
pub fn ffmpeg(job: &Job, inputs: &[&Path], args: &[String], out: &Path, total: f64) -> Result<(), String> {
    let ff = crate::ffmpeg().ok_or("Needs ffmpeg")?;
    let mut a = s(&["-y", "-hide_banner", "-nostdin", "-progress", "pipe:1", "-nostats"]);
    for i in inputs { a.push("-i".into()); a.push(i.display().to_string()); }
    a.extend_from_slice(args);
    a.push(out.display().to_string());
    let cb = |line: &str| {
        if let Some(v) = line.strip_prefix("out_time_us=") {
            if let (Ok(us), true) = (v.parse::<f64>(), total > 0.0) { job.set_frac((us / 1e6 / total) as f32); }
        }
    };
    run(Some(job), &ff, &a, Some(&cb)).map(|_| ())
}

pub fn convert_args(target: &str) -> Vec<String> {
    let v: &[&str] = match target {
        "mp4" => &["-c:v", "libx264", "-preset", "medium", "-crf", "20", "-pix_fmt", "yuv420p", "-vf", "scale=trunc(iw/2)*2:trunc(ih/2)*2", "-c:a", "aac", "-b:a", "192k", "-movflags", "+faststart"],
        "mov" => &["-c:v", "libx264", "-preset", "medium", "-crf", "20", "-pix_fmt", "yuv420p", "-vf", "scale=trunc(iw/2)*2:trunc(ih/2)*2", "-c:a", "aac", "-b:a", "192k"],
        "mkv" => &["-c:v", "libx264", "-preset", "medium", "-crf", "20", "-c:a", "aac", "-b:a", "192k"],
        "webm" => &["-c:v", "libvpx-vp9", "-b:v", "0", "-crf", "32", "-row-mt", "1", "-cpu-used", "4", "-c:a", "libopus", "-b:a", "128k"],
        "avi" => &["-c:v", "mpeg4", "-q:v", "4", "-c:a", "libmp3lame", "-b:a", "192k"],
        "wmv" => &["-c:v", "wmv2", "-q:v", "4", "-c:a", "wmav2", "-b:a", "192k"],
        "gif" => &["-vf", "fps=12,scale='min(iw,640)':-2:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse", "-an"],
        "mp3" => &["-vn", "-c:a", "libmp3lame", "-q:a", "2"],
        "m4a" => &["-vn", "-c:a", "aac", "-b:a", "192k"],
        "wav" => &["-vn", "-c:a", "pcm_s16le"],
        "flac" => &["-vn", "-c:a", "flac"],
        "ogg" => &["-vn", "-c:a", "libvorbis", "-q:a", "5"],
        "opus" => &["-vn", "-c:a", "libopus", "-b:a", "128k"],
        "aiff" => &["-vn", "-c:a", "pcm_s16be"],
        "wma" => &["-vn", "-c:a", "wmav2", "-b:a", "192k"],
        _ => &[],
    };
    s(v)
}

pub fn convert(job: &Job, input: &Path, target: &str) -> Result<PathBuf, String> {
    let out = sibling(input, "", target);
    let total = duration(input);
    atomically(&out, |tmp| ffmpeg(job, &[input], &convert_args(target), tmp, total))
}

/// One input to one output beside it, with extra arguments.
pub fn transform(job: &Job, input: &Path, suffix: &str, ext: Option<&str>, args: Vec<String>) -> Result<PathBuf, String> {
    let e = ext.map(str::to_string).unwrap_or_else(|| crate::catalog::ext_of(input));
    let out = sibling(input, suffix, &e);
    let total = duration(input);
    atomically(&out, |tmp| ffmpeg(job, &[input], &args, tmp, total))
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
pub fn bleep_filter(ranges: &[(f64, f64)]) -> String {
    let cond = |rs: &[(f64, f64)]| rs.iter().map(|(a, b)| format!("between(t,{a:.3},{b:.3})")).collect::<Vec<_>>().join("+");
    let c = cond(ranges);
    format!("[0:a]volume=enable='{c}':volume=0[m];sine=f=1000:r=48000,volume=0.25,volume=enable='not({c})':volume=0[t];[m][t]amix=inputs=2:duration=first:normalize=0[out]")
}

/// Width and height of the first video stream.
pub fn size(p: &Path) -> Option<(u32, u32)> {
    let ff = crate::ffmpeg()?;
    let probe = ff.with_file_name("ffprobe.exe");
    let probe = if probe.exists() { probe } else { PathBuf::from("ffprobe") };
    let a = s(&["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0", &p.display().to_string()]);
    let o = run(None, &probe, &a, None).ok()?;
    let (w, h) = o.trim().split_once(',')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

pub fn has_audio(p: &Path) -> bool {
    let Some(ff) = crate::ffmpeg() else { return false };
    let probe = ff.with_file_name("ffprobe.exe");
    let probe = if probe.exists() { probe } else { PathBuf::from("ffprobe") };
    let a = s(&["-v", "error", "-select_streams", "a", "-show_entries", "stream=index", "-of", "csv=p=0", &p.display().to_string()]);
    run(None, &probe, &a, None).map(|o| !o.trim().is_empty()).unwrap_or(false)
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
    fn every_target_has_arguments() {
        for t in ["mp4", "mov", "mkv", "webm", "avi", "wmv", "gif", "mp3", "m4a", "wav", "flac", "ogg", "opus", "aiff", "wma"] {
            assert!(!convert_args(t).is_empty(), "{t}");
        }
    }
    #[test]
    fn bleep_covers_each_range() { assert!(bleep_filter(&[(1.0, 2.0), (3.0, 4.0)]).contains("between(t,3.000,4.000)")); }
}
