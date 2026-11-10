//! Puts the arguments the app builds through a real ffmpeg and measures what comes out. The two modules it
//! covers have no Win32 in them, so this runs on any OS, without building the app:
//!
//!     rustc --edition 2021 --test tests/fidelity.rs -o target/fidelity && target/fidelity --test-threads=4
//!
//! Exactness is checked against crops of the untouched source in its native pixel format, never against the
//! source run through the same filter. A filter that silently converts formats would fool that comparison.
//! Encoders missing from the local ffmpeg build are reported and skipped.
#[path = "../src/fade.rs"]
#[allow(dead_code)]
mod fade;
#[path = "../src/gif.rs"]
#[allow(dead_code)]
mod gif;
#[path = "../src/quality.rs"]
#[allow(dead_code)]
mod quality;
#[path = "../src/redact.rs"]
#[allow(dead_code)]
mod redact;

use quality::*;
use redact::{Fill, Mark};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const BOX: (u32, u32, u32, u32) = (100, 60, 120, 80);
const BLACK: Fill = Fill::Solid([0, 0, 0]);
fn mark_at(b: (u32, u32, u32, u32), ellipse: bool, fill: Fill, feather: u32) -> Mark { Mark { x: b.0, y: b.1, w: b.2, h: b.3, ellipse, fill, feather } }
fn graph(v: &VideoInfo, w: u32, h: u32, marks: &[Mark]) -> String { redact_graph(&v.pix_fmt, &v.color_space, v.full_range, w, h, marks).unwrap() }

struct Dir(PathBuf);
impl Dir {
    fn new() -> Dir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!("tau-fidelity-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }
    fn p(&self, n: &str) -> PathBuf { self.0.join(n) }
}
impl Drop for Dir { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

fn ff(args: &[String]) -> Result<Vec<u8>, String> {
    let o = Command::new("ffmpeg").args(["-y", "-hide_banner", "-nostdin", "-loglevel", "error"]).args(args).output().map_err(|e| e.to_string())?;
    if o.status.success() { Ok(o.stdout) } else { Err(String::from_utf8_lossy(&o.stderr).to_string()) }
}
fn ffs(args: &[&str]) -> Vec<u8> {
    let a: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    ff(&a).unwrap_or_else(|e| panic!("ffmpeg {args:?}: {e}"))
}
#[path = "../src/heif.rs"]
mod heif;

/// Encodes a PNG as a HEIC the way the app does.
fn heic_from(d: &Dir, png: &Path, w: u32, h: u32, alpha: bool, name: &str) -> PathBuf {
    let size = heif::size_for(w, h);
    let stream = |args: Vec<String>, f: &str| {
        let mut a = vec!["-i".to_string(), png.display().to_string()];
        a.extend(args);
        ff(&a).unwrap();
        heif::parse_stream(&std::fs::read(f).unwrap()).unwrap()
    };
    let (pf, af) = (d.p(&format!("{name}.p.hevc")).display().to_string(), d.p(&format!("{name}.a.hevc")).display().to_string());
    let picture = stream(heif::picture_args(size, 12, &pf), &pf);
    let a = alpha.then(|| stream(heif::alpha_args(size, 10, &af), &af));
    let out = d.p(&format!("{name}.heic"));
    std::fs::write(&out, heif::mux(&picture, a.as_ref(), size, Some(b"not really a profile")).unwrap()).unwrap();
    out
}

#[test]
fn heic_decodes_to_the_same_picture_at_its_own_size() {
    if !have_ffmpeg() || !has_encoder("libx265") { return; }
    let d = Dir::new();
    for (w, h) in [(640, 360), (641, 361), (5, 3)] {
        let png = d.p(&format!("s{w}.png"));
        ffs(&["-f", "lavfi", "-i", &format!("testsrc2=s={w}x{h}"), "-frames:v", "1", png.to_str().unwrap()]);
        let heic = heic_from(&d, &png, w, h, false, &format!("s{w}"));
        assert_eq!(field(&heic, "v:0", "stream=codec_name"), "hevc");
        let back = d.p(&format!("b{w}.png"));
        ffs(&["-i", heic.to_str().unwrap(), "-frames:v", "1", back.to_str().unwrap()]);
        // ffmpeg's own crop stops at whole chroma samples, so an odd side reads one short here. Viewers crop exactly.
        assert_eq!(field(&back, "v:0", "stream=width").parse::<u32>().unwrap(), w & !1, "{w}x{h}");
        assert_eq!(field(&back, "v:0", "stream=height").parse::<u32>().unwrap(), h & !1, "{w}x{h}");
        if w > 100 {
            let q = psnr(&png, &back);
            assert!(q > 34.0, "{w}x{h} came back at {q} dB");
        }
    }
}

#[test]
fn heic_keeps_transparency_as_a_second_picture() {
    if !have_ffmpeg() || !has_encoder("libx265") { return; }
    let d = Dir::new();
    let png = d.p("a.png");
    ffs(&["-f", "lavfi", "-i", "color=red:s=64x64,format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='if(lt(X,32),255,0)'", "-frames:v", "1", png.to_str().unwrap()]);
    let heic = heic_from(&d, &png, 64, 64, true, "a");
    let bytes = std::fs::read(&heic).unwrap();
    for kind in [&b"auxC"[..], b"auxl", b"urn:mpeg:hevc:2015:auxid:1"] { assert!(bytes.windows(kind.len()).any(|w| w == kind)); }
    // The transparency item is its own stream, and reads back half full.
    let back = d.p("alpha.png");
    ffs(&["-i", heic.to_str().unwrap(), "-map", "0:v:1", "-frames:v", "1", back.to_str().unwrap()]);
    let raw = ffs(&["-i", back.to_str().unwrap(), "-vf", "format=gray", "-f", "rawvideo", "pipe:1"]);
    assert_eq!(raw.len(), 64 * 64);
    assert!(raw[10 * 64 + 8] > 245 && raw[10 * 64 + 56] < 10, "{} {}", raw[10 * 64 + 8], raw[10 * 64 + 56]);
}

#[test]
fn camcorder_footage_comes_out_whole_frames_at_the_right_shape() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    // An interlaced MPEG-2 file with non-square pixels and AC-3, as a tape camcorder writes one.
    let src = d.p("MOV001.vob");
    ffs(&["-f", "lavfi", "-i", "testsrc2=s=720x480:r=60000/1001:d=1", "-f", "lavfi", "-i", "sine=d=1", "-vf", "setsar=8/9,tinterlace=interleave_top", "-c:v", "mpeg2video", "-flags", "+ildct+ilme", "-top", "1", "-b:v", "6M", "-c:a", "ac3", "-f", "vob", src.to_str().unwrap()]);
    let info = probe(&src);
    let v = info.video.clone().unwrap();
    assert!(v.interlaced, "{v:?}");
    assert!((v.pixel_aspect - 8.0 / 9.0).abs() < 0.001, "{v:?}");
    assert_eq!(muxer_for("MOD"), Some("vob"));
    assert_eq!(muxer_for("mts"), None, "ffmpeg knows this one");

    let out = d.p("c.mp4");
    run_attempts(&src, &convert_attempts(&info, "mp4", Speed::Best), &out);
    assert_eq!(field(&out, "v:0", "stream=codec_name"), "h264", "MPEG-2 in MP4 would not play");
    assert_eq!(field(&out, "v:0", "stream=field_order"), "progressive");
    assert_eq!(field(&out, "a:0", "stream=codec_name"), "ac3");
    assert_eq!(field(&out, "v:0", "stream=sample_aspect_ratio"), "8:9", "still shows at 4:3");
    let rate: f64 = { let r = field(&out, "v:0", "stream=avg_frame_rate"); let (a, b) = r.split_once('/').unwrap(); a.parse::<f64>().unwrap() / b.parse::<f64>().unwrap() };
    assert!((rate - 59.94).abs() < 1.0, "one picture per field: {rate}");

    // A still is what a player would show: square pixels, not 720 wide.
    let png = d.p("still.png");
    ffs(&["-i", src.to_str().unwrap(), "-vf", &still_filter(&v).unwrap(), "-frames:v", "1", png.to_str().unwrap()]);
    assert_eq!(field(&png, "v:0", "stream=width"), "640");
    assert!(still_filter(&VideoInfo { pixel_aspect: 1.0, ..Default::default() }).is_none(), "ordinary video goes untouched");
}

/// The picture of each frame, in order, to find out which frame a cut or a still really begins on.
fn frame_hashes(before: &[String], src: &Path, after: &[String]) -> Vec<String> {
    let mut a: Vec<String> = before.to_vec();
    a.extend(["-i".to_string(), src.display().to_string()]);
    a.extend_from_slice(after);
    a.extend(["-map", "0:v:0", "-f", "framemd5", "-"].map(String::from));
    let refs: Vec<&str> = a.iter().map(String::as_str).collect();
    let out = ffs(&refs);
    String::from_utf8_lossy(&out).lines().filter(|l| !l.starts_with('#')).filter_map(|l| l.rsplit(',').next()).map(|h| h.trim().to_string()).collect()
}

#[test]
fn camcorder_containers_seek_to_the_exact_frame() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let fps = 30000.0 / 1001.0;
    for (name, codec, format) in [("long.mts", "libx264", "mpegts"), ("long.mod", "mpeg2video", "vob")] {
        let src = d.p(name);
        ffs(&["-f", "lavfi", "-i", "testsrc2=s=160x90:r=30000/1001:d=20", "-c:v", codec, "-g", "15", "-bf", "2", "-f", format, src.to_str().unwrap()]);
        let all = frame_hashes(&[], &src, &[]);
        let ext = name.rsplit('.').next().unwrap();
        assert!(seeks_loosely(ext));
        for k in [0usize, 7, 40, 157, 302, 455, 590] {
            let (before, mut after) = still_seek(ext, k as f64 / fps);
            after.extend(["-frames:v", "1"].map(String::from));
            assert_eq!(frame_hashes(&before, &src, &after), [all[k].clone()], "{name} frame {k}");
        }
        // A stretch from the middle holds exactly those frames.
        let (a, b) = (157, 302);
        let (before, after) = cut_seek(ext, a as f64 / fps, b as f64 / fps);
        assert_eq!(frame_hashes(&before, &src, &after), all[a..b].to_vec(), "{name} cut");
    }
    // A clip with a single keyframe, the way a chopped AVCHD file can be, still has its later frames.
    let src = d.p("short.mts");
    ffs(&["-f", "lavfi", "-i", "testsrc2=s=160x90:r=60000/1001:d=0.5", "-c:v", "libx264", "-g", "1000", "-bf", "0", "-f", "mpegts", src.to_str().unwrap()]);
    let all = frame_hashes(&[], &src, &[]);
    let (before, mut after) = still_seek("mts", 25.0 * 1001.0 / 60000.0);
    after.extend(["-frames:v", "1"].map(String::from));
    assert_eq!(frame_hashes(&before, &src, &after), [all[25].clone()]);
    // Everything else keeps the fast seek, which is exact there.
    assert_eq!(still_seek("mp4", 5.0).1.len(), 0);
    assert!(!seeks_loosely("mkv"));
}

fn have_ffmpeg() -> bool { Command::new("ffmpeg").arg("-version").output().is_ok() }
fn has_encoder(name: &str) -> bool {
    let o = Command::new("ffmpeg").args(["-hide_banner", "-encoders"]).output().unwrap();
    String::from_utf8_lossy(&o.stdout).lines().any(|l| l.split_whitespace().nth(1) == Some(name))
}
fn probe(p: &Path) -> Probe {
    let o = Command::new("ffprobe").args(probe_args(&p.display().to_string())).output().unwrap();
    parse_probe(&String::from_utf8_lossy(&o.stdout))
}
fn field(p: &Path, stream: &str, entry: &str) -> String {
    let o = Command::new("ffprobe").args(["-v", "error", "-select_streams", stream, "-show_entries", entry, "-of", "default=nw=1:nk=1", &p.display().to_string()]).output().unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}
/// What media.rs does: inputs, the app's arguments, then the output.
fn run_app_args(input: &Path, args: &[String], out: &Path) -> Result<(), String> {
    let mut a = vec!["-i".to_string(), input.display().to_string()];
    a.extend_from_slice(args);
    a.push(out.display().to_string());
    ff(&a).map(|_| ())
}
/// Same, with options before the input.
fn run_seek(seek: &[String], input: &Path, args: &[String], out: &Path) -> Result<(), String> {
    let mut a = seek.to_vec();
    a.extend(["-i".to_string(), input.display().to_string()]);
    a.extend_from_slice(args);
    a.push(out.display().to_string());
    ff(&a).map(|_| ())
}
/// One decoded frame's luma range per frame of a region.
fn luma_ranges(p: &Path, c: (u32, u32, u32, u32)) -> Vec<(u32, u32)> {
    let o = ff(&["-i".to_string(), p.display().to_string(), "-vf".to_string(), format!("crop={}:{}:{}:{},format=gray,signalstats,metadata=print:file=-", c.2, c.3, c.0, c.1), "-f".to_string(), "null".to_string(), "-".to_string()]).unwrap();
    let t = String::from_utf8_lossy(&o).to_string();
    let vals = |k: &str| -> Vec<u32> { t.lines().filter_map(|l| l.split_once(k).map(|(_, v)| v.trim().parse().unwrap())).collect() };
    vals("signalstats.YMIN=").into_iter().zip(vals("signalstats.YMAX=")).collect()
}
/// First attempt that succeeds, like media::attempts.
fn run_attempts(input: &Path, attempts: &[Vec<String>], out: &Path) -> usize {
    let mut last = String::new();
    for (i, a) in attempts.iter().enumerate() {
        match run_app_args(input, a, out) { Ok(()) => return i, Err(e) => last = e }
    }
    panic!("every attempt failed for {}: {last}", out.display());
}
/// One hash per frame of a region, decoded in the file's own pixel format with no frames dropped or repeated.
fn region(p: &Path, crop: Option<(u32, u32, u32, u32)>, pix: Option<&str>) -> Vec<String> {
    let mut a = vec!["-i".to_string(), p.display().to_string(), "-map".into(), "0:V:0".into(), "-fps_mode".into(), "passthrough".into()];
    let mut chain = vec![];
    if let Some(f) = pix { chain.push(format!("format={f}")); }
    if let Some((x, y, w, h)) = crop { chain.push(format!("crop={w}:{h}:{x}:{y}")); }
    if !chain.is_empty() { a.extend(["-vf".to_string(), chain.join(",")]); }
    a.extend(["-f".to_string(), "framemd5".to_string(), "-".to_string()]);
    let o = ff(&a).unwrap_or_else(|e| panic!("hash {}: {e}", p.display()));
    String::from_utf8_lossy(&o).lines().filter(|l| !l.starts_with('#')).map(|l| l.rsplit(',').next().unwrap().trim().to_string()).collect()
}
/// The four strips around BOX, each a hash per frame.
fn outside_box(p: &Path, w: u32, h: u32, pix: Option<&str>) -> Vec<Vec<String>> {
    let (bx, by, bw, bh) = BOX;
    vec![
        region(p, Some((0, 0, w, by)), pix),
        region(p, Some((0, by, bx, bh)), pix),
        region(p, Some((bx + bw, by, w - bx - bw, bh)), pix),
        region(p, Some((0, by + bh, w, h - by - bh)), pix),
    ]
}
fn psnr(a: &Path, b: &Path) -> f64 {
    let o = Command::new("ffmpeg").args(["-hide_banner", "-nostdin", "-i"]).arg(a).arg("-i").arg(b)
        .args(["-lavfi", "[0:v]settb=1/1000,setpts=N[a];[1:v]settb=1/1000,setpts=N[b];[a][b]psnr", "-f", "null", "-"]).output().unwrap();
    let s = String::from_utf8_lossy(&o.stderr);
    let v = s.rsplit("average:").next().unwrap().split_whitespace().next().unwrap();
    if v == "inf" { f64::INFINITY } else { v.parse().unwrap_or_else(|_| panic!("psnr: {s}")) }
}

/// Encodes a test clip: moving colour detail, in the given format.
fn clip(d: &Dir, name: &str, pix: &str, codec: &[&str], extra: &str) -> PathBuf {
    let out = d.p(name);
    let vf = format!("format={pix}{extra}");
    let mut a = vec!["-f", "lavfi", "-i", "testsrc2=s=640x360:r=30:d=1", "-vf", &vf];
    a.extend_from_slice(codec);
    let s = out.display().to_string();
    a.push(&s);
    ffs(&a);
    out
}

fn assert_redacted_exactly(src: &Path, ext: &str, label: &str) {
    let d = Dir::new();
    let p = probe(src);
    let v = p.video.clone().unwrap();
    let (w, h) = v.display_size();
    let (x, y, bw, bh) = redact_box(BOX.0, BOX.1, BOX.2, BOX.3, w, h);
    let f = graph(&v, w, h, &[mark_at((x, y, bw, bh), false, BLACK, 0)]);
    let l = redact_args(&p, ext, &f).unwrap();
    let out = d.p(&format!("out.{}", l.ext));
    run_app_args(src, &l.args, &out).unwrap_or_else(|e| panic!("{label}: {e}"));
    // Compare in the source's planar form, which only rearranges samples.
    let planar = planar_of(&v.pix_fmt).unwrap();
    let want = outside_box(src, w, h, Some(&planar));
    let got = outside_box(&out, w, h, Some(&planar));
    assert_eq!(want[0].len(), got[0].len(), "{label}: frame count");
    assert!(!want[0].is_empty(), "{label}: no frames");
    assert_eq!(want, got, "{label}: pixels outside the box changed");
    // Format is kept unless the encoder stores the same samples another way.
    let out_fmt = field(&out, "v:0", "stream=pix_fmt");
    assert_eq!(planar_of(&out_fmt).unwrap(), planar, "{label}: pixel format");
    // And the box really is solid: one value per frame across the region.
    let boxed = region(&out, Some((x, y, bw, bh)), None);
    assert_eq!(boxed.iter().collect::<std::collections::HashSet<_>>().len(), 1, "{label}: box is not solid");
    let sb = region(src, Some((x, y, bw, bh)), None);
    assert_ne!(boxed[0], sb[0], "{label}: box region was not painted");
    println!("{label}: {} frames exact outside the box, format {out_fmt}, container {}", want[0].len(), l.ext);
}

#[test]
fn redact_is_exact_in_every_pixel_format() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let x264 = ["-c:v", "libx264", "-crf", "23"];
    let ffv1 = ["-c:v", "ffv1"];
    let cases: Vec<(&str, &str, &[&str], &str)> = vec![
        ("yuv420p.mp4", "yuv420p", &x264, "mp4"),
        ("yuv422p.mov", "yuv422p", &x264, "mov"),
        ("yuv444p.mkv", "yuv444p", &x264, "mkv"),
        ("yuv420p10.mp4", "yuv420p10le", &x264, "mp4"),
        ("yuv422p10.mov", "yuv422p10le", &x264, "mov"),
        ("yuv444p10.mkv", "yuv444p10le", &x264, "mkv"),
        ("yuv420p12.mkv", "yuv420p12le", &ffv1, "mkv"),
        ("yuva444p10.mkv", "yuva444p10le", &ffv1, "mkv"),
        ("rgb.mkv", "bgr0", &ffv1, "mkv"),
        ("gray.mkv", "gray", &ffv1, "mkv"),
    ];
    for (name, pix, codec, ext) in cases {
        let s = clip(&d, name, pix, codec, "");
        assert_redacted_exactly(&s, ext, &format!("{pix} in {ext}"));
    }
}

#[test]
fn redact_webm_is_exact_and_stays_webm() {
    if !have_ffmpeg() || !has_encoder("libvpx-vp9") { println!("skipped: no libvpx-vp9"); return; }
    let d = Dir::new();
    let s = clip(&d, "a.webm", "yuv420p", &["-c:v", "libvpx-vp9", "-crf", "30", "-b:v", "0", "-cpu-used", "8"], "");
    assert_redacted_exactly(&s, "webm", "vp9 webm");
    let t = clip(&d, "b.webm", "yuv420p10le", &["-c:v", "libvpx-vp9", "-crf", "30", "-b:v", "0", "-cpu-used", "8"], "");
    assert_redacted_exactly(&t, "webm", "10-bit vp9 webm");
}

#[test]
fn redact_keeps_hdr_tags_full_range_and_every_track() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let tags = ",setparams=color_trc=smpte2084:color_primaries=bt2020:colorspace=bt2020nc:range=tv";
    let v = clip(&d, "hdr.mp4", "yuv420p10le", &["-c:v", "libx264", "-crf", "23"], tags);
    let s = d.p("full.mkv");
    let meta = d.p("ch.txt");
    std::fs::write(&meta, ";FFMETADATA1\ntitle=Holiday\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=500\ntitle=Start\n").unwrap();
    let srt = d.p("s.srt");
    std::fs::write(&srt, "1\n00:00:00,000 --> 00:00:00,500\nHello\n").unwrap();
    ffs(&["-i", &v.display().to_string(), "-f", "lavfi", "-i", "sine=f=440:r=48000:d=1", "-f", "lavfi", "-i", "sine=f=880:r=44100:d=1", "-i", &srt.display().to_string(), "-i", &meta.display().to_string(),
        "-map", "0:v", "-map", "1:a", "-map", "2:a", "-map", "3:s", "-map_metadata", "4", "-map_chapters", "4", "-c:v", "copy", "-c:a", "flac", "-c:s", "srt", &s.display().to_string()]);
    let p = probe(&s);
    let info = p.video.clone().unwrap();
    let f = graph(&info, 640, 360, &[mark_at(BOX, false, BLACK, 0)]);
    let l = redact_args(&p, "mkv", &f).unwrap();
    let out = d.p("out.mkv");
    run_app_args(&s, &l.args, &out).unwrap();
    assert_eq!(field(&out, "v:0", "stream=color_transfer"), "smpte2084");
    assert_eq!(field(&out, "v:0", "stream=color_primaries"), "bt2020");
    assert_eq!(field(&out, "v:0", "stream=pix_fmt"), "yuv420p10le");
    assert_eq!(field(&out, "a", "stream=codec_name").lines().count(), 2, "both audio tracks");
    assert_eq!(field(&out, "a", "stream=codec_name").lines().next(), Some("flac"), "audio copied, not re-encoded");
    assert_eq!(field(&out, "s", "stream=codec_name"), "subrip");
    let chapters = Command::new("ffprobe").args(["-v", "error", "-show_chapters", "-of", "csv=p=0"]).arg(&out).output().unwrap();
    assert!(String::from_utf8_lossy(&chapters.stdout).contains("Start"), "chapters kept");
    assert!(field(&out, "", "format_tags=title").contains("Holiday"), "tags kept");

    // A full-range source blacks out at 0, not 16, and stays tagged full range.
    let fr = clip(&d, "pc.mp4", "yuv420p", &["-c:v", "libx264", "-crf", "23"], ",setparams=range=pc");
    let p = probe(&fr);
    assert!(p.video.as_ref().unwrap().full_range);
    assert_redacted_exactly(&fr, "mp4", "full range");
    let i = p.video.unwrap();
    let f = graph(&i, 640, 360, &[mark_at(BOX, false, BLACK, 0)]);
    let l = redact_args(&probe(&fr), "mp4", &f).unwrap();
    let o2 = d.p("pc-out.mp4");
    run_app_args(&fr, &l.args, &o2).unwrap();
    assert_eq!(field(&o2, "v:0", "stream=color_range"), "pc");
}

#[test]
fn redact_lands_where_the_user_marked_it_on_rotated_video() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let s = clip(&d, "a.mp4", "yuv420p", &["-c:v", "libx264", "-crf", "23"], "");
    let rot = d.p("rot.mp4");
    ffs(&["-display_rotation", "90", "-i", &s.display().to_string(), "-c", "copy", &rot.display().to_string()]);
    let p = probe(&rot);
    let v = p.video.clone().unwrap();
    // A box near the bottom edge of the upright picture, which is outside the frame if stored size is used.
    assert_eq!(v.display_size(), (360, 640));
    let target = (40, 500, 120, 80);
    let f = graph(&v, 360, 640, &[mark_at(target, false, BLACK, 0)]);
    let l = redact_args(&p, "mp4", &f).unwrap();
    let out = d.p("out.mp4");
    run_app_args(&rot, &l.args, &out).unwrap();
    let boxed = region(&out, Some(target), None);
    assert_eq!(boxed.iter().collect::<std::collections::HashSet<_>>().len(), 1, "box is on the marked spot");
    // Elsewhere the upright output matches the upright source exactly.
    assert_eq!(region(&rot, Some((0, 0, 360, 400)), None), region(&out, Some((0, 0, 360, 400)), None));
}

#[test]
fn redact_gif_changes_only_the_box() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let rgba = "bgra";
    for (name, filter, frames) in [
        ("global.gif", "split[a][b];[a]palettegen=stats_mode=full[p];[b][p]paletteuse=dither=none", "global palette"),
        ("local.gif", "split[a][b];[a]palettegen=stats_mode=single[p];[b][p]paletteuse=new=1:dither=none", "local palettes"),
        ("delta.gif", "split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=diff_mode=rectangle:dither=bayer", "partial frames"),
    ] {
        let s = d.p(name);
        ffs(&["-f", "lavfi", "-i", "testsrc2=s=320x180:r=20:d=2", "-vf", filter, &s.display().to_string()]);
        let bytes = std::fs::read(&s).unwrap();
        assert_eq!(gif::screen_size(&bytes), Some((320, 180)));
        let out = d.p(&format!("red-{name}"));
        std::fs::write(&out, gif::redact(&bytes, &[mark_at(BOX, false, BLACK, 0)]).unwrap()).unwrap();
        assert_eq!(outside_box(&s, 320, 180, Some(rgba)), outside_box(&out, 320, 180, Some(rgba)), "{frames}: pixels outside the box changed");
        let want_frames = region(&s, None, Some(rgba)).len();
        assert_eq!(region(&out, None, Some(rgba)).len(), want_frames, "{frames}: frame count");
        assert_eq!(field(&s, "v:0", "stream=r_frame_rate"), field(&out, "v:0", "stream=r_frame_rate"), "{frames}: timing");
        assert_eq!(field(&s, "", "format=duration"), field(&out, "", "format=duration"), "{frames}: duration");
        let ranges = luma_ranges(&out, BOX);
        assert_eq!(ranges.len(), want_frames, "{frames}: every frame measured");
        for (lo, hi) in ranges { assert!(lo == hi && hi < 48, "{frames}: box must be one dark colour, was {lo}..{hi}"); }
        println!("{frames}: {want_frames} frames exact outside the box");
    }
    // A frame missed by the box is copied byte for byte: redacting nothing changes nothing.
    let s = d.p("global.gif");
    let b = std::fs::read(&s).unwrap();
    assert_eq!(gif::redact(&b, &[]).unwrap(), b);
}

#[test]
fn matching_codecs_copy_with_no_change_at_all() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let s = d.p("s.mp4");
    ffs(&["-f", "lavfi", "-i", "testsrc2=s=641x361:r=30000/1001:d=2", "-f", "lavfi", "-i", "sine=r=48000:d=2", "-f", "lavfi", "-i", "sine=f=880:r=44100:d=2",
        "-vf", "format=yuv444p10le", "-c:v", "libx264", "-crf", "20", "-c:a", "aac", "-map", "0:v", "-map", "1:a", "-map", "2:a", &s.display().to_string()]);
    let p = probe(&s);
    let want = region(&s, None, None);
    for t in ["mkv", "mov"] {
        let out = d.p(&format!("o.{t}"));
        assert_eq!(run_attempts(&s, &convert_attempts(&p, t, Speed::Best), &out), 0, "{t}: stream copy worked first time");
        assert_eq!(region(&out, None, None), want, "{t}: frames");
        assert_eq!(field(&out, "a", "stream=codec_name").lines().count(), 2, "{t}: tracks");
        assert_eq!(field(&out, "v:0", "stream=pix_fmt"), "yuv444p10le", "{t}: format");
    }
    let m4a = d.p("o.m4a");
    run_attempts(&s, &convert_attempts(&p, "m4a", Speed::Best), &m4a);
    let aud = |f: &Path| { let o = ff(&["-i".into(), f.display().to_string(), "-map".into(), "0:a:0".into(), "-c".into(), "copy".into(), "-f".into(), "md5".into(), "-".into()]).unwrap(); String::from_utf8_lossy(&o).to_string() };
    assert_eq!(aud(&s), aud(&m4a), "aac in mp4 moves into m4a bit for bit");
}

#[test]
fn reencoding_conversions_keep_format_size_and_tracks() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let s = d.p("s.mkv");
    let tags = "format=yuv422p10le,setparams=color_trc=smpte2084:color_primaries=bt2020:colorspace=bt2020nc:range=tv";
    ffs(&["-f", "lavfi", "-i", "testsrc2=s=640x360:r=30:d=1", "-f", "lavfi", "-i", "sine=r=48000:d=1", "-vf", tags, "-c:v", "ffv1", "-c:a", "flac", &s.display().to_string()]);
    let p = probe(&s);
    // A lossless source stays lossless wherever the target can hold it.
    let want = region(&s, None, None);
    let m = d.p("o.mp4");
    run_attempts(&s, &convert_attempts(&p, "mp4", Speed::Best), &m);
    assert_eq!(field(&m, "v:0", "stream=pix_fmt"), "yuv422p10le", "10-bit 4:2:2 kept");
    assert_eq!(field(&m, "v:0", "stream=color_transfer"), "smpte2084", "HDR tag kept");
    assert_eq!(field(&m, "a:0", "stream=codec_name"), "alac", "lossless audio stays lossless");
    assert_eq!(region(&m, None, None), want, "every frame identical");
    if has_encoder("libvpx-vp9") {
        let w = d.p("o.webm");
        let plain = clip(&d, "plain8.mkv", "yuv420p", &["-c:v", "ffv1"], "");
        run_attempts(&plain, &convert_attempts(&probe(&plain), "webm", Speed::Best), &w);
        assert_eq!(region(&w, None, None), region(&plain, None, None), "webm lossless");
    }
    let plain = clip(&d, "plain.mkv", "yuv420p", &["-c:v", "ffv1"], "");
    let wv = d.p("q.wmv");
    run_attempts(&plain, &convert_attempts(&probe(&plain), "wmv", Speed::Best), &wv);
    assert!(psnr(&wv, &plain) > 40.0, "wmv: {}", psnr(&wv, &plain));
}

/// Detailed moving video, so that a bitrate cap has real work to do.
fn lossy_source(d: &Dir, name: &str, codec: &[&str]) -> (PathBuf, PathBuf) {
    let reference = d.p(&format!("ref-{name}.mkv"));
    if !reference.exists() {
        ffs(&["-f", "lavfi", "-i", "testsrc2=s=1280x720:r=30:d=2,noise=alls=3:allf=t,gblur=sigma=0.5", "-c:v", "ffv1", "-pix_fmt", "yuv420p", &reference.display().to_string()]);
    }
    let s = d.p(name);
    let mut a = vec!["-i".to_string(), reference.display().to_string()];
    a.extend(codec.iter().map(|c| c.to_string()));
    a.push(s.display().to_string());
    ff(&a).unwrap();
    (s, reference)
}
fn kbps(p: &Path) -> f64 { field(p, "", "format=bit_rate").parse::<f64>().unwrap() / 1000.0 }

#[test]
fn lossy_sources_are_not_blown_up_and_still_hold_their_quality() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    // WMV into MOV and H.264 into WebM cannot be copied, so both have to re-encode.
    let (src, reference) = lossy_source(&d, "s.mp4", &["-c:v", "libx264", "-b:v", "5000k", "-maxrate", "5000k", "-bufsize", "10000k"]);
    let p = probe(&src);
    let source_rate = p.video.as_ref().unwrap().bitrate as f64 / 1000.0;
    assert!(source_rate > 3000.0, "{source_rate}");
    let base = psnr(&src, &reference);
    let check = |ext: &str, limit: f64, floor: f64| {
        let o = d.p(&format!("c.{ext}"));
        run_attempts(&src, &convert_attempts(&p, ext, Speed::Best), &o);
        let k = kbps(&o);
        let (to_ref, to_src) = (psnr(&o, &reference), psnr(&o, &src));
        println!("{ext}: {source_rate:.0} -> {k:.0} kbps, {base:.1} -> {to_ref:.1} dB against the true original, {to_src:.1} dB against the source");
        assert!(k <= source_rate * limit, "{ext}: {k} kbps from a {source_rate} kbps source");
        assert!(to_ref >= base - floor, "{ext}: {to_ref} dB, source was {base}");
    };
    if has_encoder("libvpx-vp9") { check("webm", 1.1, 1.5); }
    check("wmv", 1.75, 3.0);

    // A VP9 source into MP4 is given more bits, since H.264 needs them for the same picture.
    if has_encoder("libvpx-vp9") {
        let (v, r2) = lossy_source(&d, "v.webm", &["-c:v", "libvpx-vp9", "-b:v", "3000k", "-cpu-used", "5", "-row-mt", "1"]);
        let pv = probe(&v);
        let rate = pv.video.as_ref().unwrap().bitrate as f64 / 1000.0;
        assert!(rate > 500.0, "bitrate from the file total when the stream gives none: {rate}");
        let o = d.p("from-vp9.mp4");
        run_attempts(&v, &convert_attempts(&pv, "mp4", Speed::Best), &o);
        let k = kbps(&o);
        println!("vp9 -> mp4: {rate:.0} -> {k:.0} kbps, {:.1} -> {:.1} dB", psnr(&v, &r2), psnr(&o, &r2));
        assert!(k <= rate / 0.65 * 1.1, "{k} from {rate}");
        assert!(psnr(&o, &r2) >= psnr(&v, &r2) - 1.5);
    }

    // Lossy audio follows too.
    let m = d.p("a128.mp3");
    ffs(&["-f", "lavfi", "-i", "sine=f=440:r=44100:d=4", "-ac", "2", "-c:a", "libmp3lame", "-b:a", "128k", &m.display().to_string()]);
    let pm = probe(&m);
    assert_eq!(pm.audio[0].bitrate, 128000);
    let a = d.p("a.m4a");
    run_attempts(&m, &convert_attempts(&pm, "m4a", Speed::Best), &a);
    assert_eq!(field(&a, "a:0", "stream=codec_name"), "aac", "a lossy source does not become a bigger ALAC");
    assert!(field(&a, "a:0", "stream=bit_rate").parse::<u32>().unwrap() < 130_000);
}

#[test]
fn every_redaction_style_leaves_the_rest_exact() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let tags = ",setparams=color_trc=smpte2084:color_primaries=bt2020:colorspace=bt2020nc:range=tv";
    let x264 = ["-c:v", "libx264", "-crf", "20"];
    let sources = [
        (clip(&d, "a8.mp4", "yuv420p", &x264, ""), "mp4", "yuv420p"),
        (clip(&d, "a10.mp4", "yuv420p10le", &x264, tags), "mp4", "yuv420p10le"),
        (clip(&d, "a444.mkv", "yuv444p12le", &["-c:v", "ffv1"], ""), "mkv", "yuv444p12le"),
        (clip(&d, "argb.mkv", "bgr0", &["-c:v", "ffv1"], ""), "mkv", "gbrp"),
    ];
    let bounds = (100, 60, 120, 80);
    let marks = [
        ("ellipse", vec![mark_at(bounds, true, Fill::Solid([200, 30, 30]), 0)]),
        ("blur", vec![mark_at(bounds, false, Fill::Blur(10.0), 0)]),
        ("feathered blur", vec![mark_at(bounds, true, Fill::Blur(10.0), 16)]),
        ("feathered colour", vec![mark_at(bounds, false, Fill::Solid([30, 60, 200]), 16)]),
        ("several", vec![mark_at((100, 60, 40, 40), false, BLACK, 0), mark_at((160, 90, 60, 50), true, Fill::Blur(6.0), 8)]),
    ];
    for (src, ext, planar) in &sources {
        let p = probe(src);
        let v = p.video.clone().unwrap();
        for (name, m) in &marks {
            let l = redact_args(&p, ext, &graph(&v, 640, 360, m)).unwrap();
            let out = d.p(&format!("o-{planar}-{}.{}", name.replace(' ', "_"), l.ext));
            run_app_args(src, &l.args, &out).unwrap_or_else(|e| panic!("{planar} {name}: {e}"));
            // Everything past the mark and its feather is identical in the source's own format.
            let (ox, oy, ow, oh) = (100 - 16, 60 - 16, 120 + 32, 80 + 32);
            let around = |f: &Path| vec![
                region(f, Some((0, 0, 640, oy)), Some(planar)),
                region(f, Some((0, oy, ox, oh)), Some(planar)),
                region(f, Some((ox + ow, oy, 640 - ox - ow, oh)), Some(planar)),
                region(f, Some((0, oy + oh, 640, 360 - oy - oh)), Some(planar)),
            ];
            assert_eq!(around(src), around(&out), "{planar} {name}");
            assert_eq!(planar_of(&field(&out, "v:0", "stream=pix_fmt")).unwrap(), planar_of(&v.pix_fmt).unwrap(), "{planar} {name}: format");
            // The middle of the mark changed.
            assert_ne!(region(src, Some((110, 70, 100, 60)), Some(planar)), region(&out, Some((110, 70, 100, 60)), Some(planar)), "{planar} {name}: nothing painted");
        }
        println!("{planar}: all redaction styles exact outside the marks");
    }
}

#[test]
fn colours_come_out_as_chosen() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let src = clip(&d, "a.mp4", "yuv420p", &["-c:v", "libx264", "-crf", "20"], ",setparams=colorspace=bt709:range=tv");
    let p = probe(&src);
    let v = p.video.clone().unwrap();
    let l = redact_args(&p, "mp4", &graph(&v, 640, 360, &[mark_at(BOX, false, Fill::Solid([220, 40, 40]), 0)])).unwrap();
    let out = d.p("o.mp4");
    run_app_args(&src, &l.args, &out).unwrap();
    let png = d.p("box.png");
    ffs(&["-i", &out.display().to_string(), "-vf", "crop=20:20:150:90,scale=1:1:flags=area,format=rgb24", "-frames:v", "1", "-f", "rawvideo", &png.display().to_string()]);
    let rgb = std::fs::read(&png).unwrap();
    println!("asked for 220,40,40, got {rgb:?}");
    assert!((rgb[0] as i32 - 220).abs() <= 4 && (rgb[1] as i32 - 40).abs() <= 4 && (rgb[2] as i32 - 40).abs() <= 4, "{rgb:?}");
}

#[test]
fn gif_redaction_styles() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let s = d.p("s.gif");
    ffs(&["-f", "lavfi", "-i", "testsrc2=s=320x180:r=20:d=1", "-vf", "split[a][b];[a]palettegen=stats_mode=single[p];[b][p]paletteuse=new=1:dither=none", &s.display().to_string()]);
    let bytes = std::fs::read(&s).unwrap();
    // A hard ellipse stays in place, exact outside its box, with the corners of the box untouched too.
    let m = mark_at(BOX, true, BLACK, 0);
    let out = d.p("e.gif");
    std::fs::write(&out, gif::redact(&bytes, &[m]).unwrap()).unwrap();
    assert_eq!(outside_box(&s, 320, 180, Some("bgra")), outside_box(&out, 320, 180, Some("bgra")));
    assert_eq!(region(&s, Some((100, 60, 8, 8)), Some("bgra")), region(&out, Some((100, 60, 8, 8)), Some("bgra")), "corner of the box");
    assert!(gif::redact(&bytes, &[mark_at(BOX, false, Fill::Blur(4.0), 0)]).is_err(), "a blur has to take the filter path");
    // A blur goes through the filter path and stays a GIF of the same length.
    let g = redact_graph("bgra", "", true, 320, 180, &[mark_at(BOX, true, Fill::Blur(6.0), 6)]).unwrap();
    let o2 = d.p("b.gif");
    run_app_args(&s, &gif_edit_args(Some(&format!("format=gbrap,{g}"))), &o2).unwrap();
    assert_eq!(region(&o2, None, Some("bgra")).len(), region(&s, None, Some("bgra")).len());
}

#[test]
fn odd_sizes_are_padded_not_resampled() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let s = d.p("odd.mkv");
    ffs(&["-f", "lavfi", "-i", "testsrc2=s=800x600:r=30:d=1", "-vf", "format=yuv444p,crop=641:361:0:0", "-c:v", "ffv1", &s.display().to_string()]);
    assert_eq!(field(&s, "v:0", "stream=width"), "641", "fixture");
    let p = probe(&s);
    // 4:4:4 stores odd sizes as they are.
    let m = d.p("o.mp4");
    run_attempts(&s, &convert_attempts(&p, "mp4", Speed::Best), &m);
    assert_eq!(field(&m, "v:0", "stream=width"), "641");
    // 4:2:0 cannot, so a row and a column of padding go on, and the picture itself is not resampled.
    let a = d.p("o.wmv");
    run_attempts(&s, &convert_attempts(&p, "wmv", Speed::Best), &a);
    assert_eq!((field(&a, "v:0", "stream=width"), field(&a, "v:0", "stream=height")), ("642".to_string(), "362".to_string()));
    let o = Command::new("ffmpeg").args(["-hide_banner", "-nostdin", "-i"]).arg(&a).arg("-i").arg(&s)
        .args(["-lavfi", "[0:v]crop=640:360:0:0,format=yuv420p,settb=1/1000,setpts=N[a];[1:v]format=yuv444p,crop=640:360:0:0,format=yuv420p,settb=1/1000,setpts=N[b];[a][b]psnr", "-f", "null", "-"]).output().unwrap();
    let t = String::from_utf8_lossy(&o.stderr);
    let v: f64 = t.rsplit("average:").next().unwrap().split_whitespace().next().unwrap().parse().unwrap();
    assert!(v > 38.0, "psnr {v}");
}

#[test]
fn gif_best_keeps_full_size_and_frame_rate() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let s = d.p("s.mkv");
    ffs(&["-f", "lavfi", "-i", "testsrc2=s=1280x720:r=30:d=1", "-vf", "format=yuv420p", "-c:v", "ffv1", &s.display().to_string()]);
    let out = d.p("o.gif");
    run_attempts(&s, &convert_attempts(&probe(&s), "gif", Speed::Best), &out);
    assert_eq!(field(&out, "v:0", "stream=width"), "1280");
    assert_eq!(region(&out, None, None).len(), region(&s, None, None).len(), "every frame kept");
    let v = d.p("fast.mkv");
    ffs(&["-f", "lavfi", "-i", "testsrc2=s=320x180:r=60:d=1", "-c:v", "ffv1", &v.display().to_string()]);
    let g = d.p("fast.gif");
    run_attempts(&v, &convert_attempts(&probe(&v), "gif", Speed::Best), &g);
    assert_eq!(region(&g, None, None).len(), 50, "60 fps caps at the 50 GIF can show");
}

#[test]
fn quick_video_speeds_encode_and_play() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let s = d.p("s.mkv");
    ffs(&["-f", "lavfi", "-i", "testsrc2=s=640x360:r=30:d=1", "-vf", "format=yuv420p", "-c:v", "ffv1", &s.display().to_string()]);
    for sp in [Speed::Quality, Speed::Balanced, Speed::Fast] {
        for t in ["mp4", "webm"] {
            let o = d.p(&format!("{}.{t}", sp.id()));
            run_attempts(&s, &convert_attempts(&probe(&s), t, sp), &o);
            assert_eq!(region(&o, None, None).len(), 30, "{t} {sp:?} keeps every frame");
        }
    }
    let g = d.p("fast.gif");
    run_attempts(&s, &convert_attempts(&probe(&s), "gif", Speed::Fast), &g);
    assert_eq!((field(&g, "v:0", "stream=width"), region(&g, None, None).len()), ("480".into(), 12));
}

#[test]
fn gif_is_shrunk_but_never_enlarged() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let s = d.p("s.mkv");
    ffs(&["-f", "lavfi", "-i", "testsrc2=s=1280x720:r=30:d=1", "-vf", "format=yuv420p", "-c:v", "ffv1", &s.display().to_string()]);
    let out = d.p("o.gif");
    run_attempts(&s, &convert_attempts(&probe(&s), "gif", Speed::Balanced), &out);
    assert_eq!((field(&out, "v:0", "stream=width"), field(&out, "v:0", "stream=height")), ("640".into(), "360".into()));
    assert_eq!(region(&out, None, None).len(), 15, "30 fps comes down to 15");
    let tall = d.p("tall.mkv");
    ffs(&["-f", "lavfi", "-i", "testsrc2=s=720x1280:r=30:d=1", "-c:v", "ffv1", &tall.display().to_string()]);
    let t = d.p("tall.gif");
    run_attempts(&tall, &convert_attempts(&probe(&tall), "gif", Speed::Balanced), &t);
    assert_eq!(field(&t, "v:0", "stream=height"), "640", "the long side is the one limited");
    let small = d.p("small.mkv");
    ffs(&["-f", "lavfi", "-i", "testsrc2=s=320x180:r=10:d=1", "-c:v", "ffv1", &small.display().to_string()]);
    let g = d.p("small.gif");
    run_attempts(&small, &convert_attempts(&probe(&small), "gif", Speed::Balanced), &g);
    assert_eq!(field(&g, "v:0", "stream=width"), "320");
    assert_eq!(region(&g, None, None).len(), 10, "a slow source keeps every frame");
}

#[test]
fn audio_keeps_depth_rate_and_cover_art() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let w = d.p("s.wav");
    ffs(&["-f", "lavfi", "-i", "sine=f=997:r=96000:d=1", "-c:a", "pcm_s24le", &w.display().to_string()]);
    let f = d.p("s.flac");
    let p = probe(&w);
    run_attempts(&w, &convert_attempts(&p, "flac", Speed::Best), &f);
    assert_eq!(field(&f, "a:0", "stream=sample_rate"), "96000");
    assert_eq!(field(&f, "a:0", "stream=bits_per_raw_sample"), "24");
    let pcm = |a: &Path| { let o = ff(&["-i".into(), a.display().to_string(), "-map".into(), "0:a:0".into(), "-f".into(), "s32le".into(), "-c:a".into(), "pcm_s32le".into(), "-".into()]).unwrap(); o };
    assert_eq!(pcm(&w), pcm(&f), "wav to flac is bit exact");
    let back = d.p("b.wav");
    run_attempts(&f, &convert_attempts(&probe(&f), "wav", Speed::Best), &back);
    assert_eq!(field(&back, "a:0", "stream=codec_name"), "pcm_s24le", "24-bit stays 24-bit, the old code cut it to 16");
    assert_eq!(pcm(&w), pcm(&back));
    let a = d.p("s.aiff");
    run_attempts(&w, &convert_attempts(&p, "aiff", Speed::Best), &a);
    assert_eq!(field(&a, "a:0", "stream=codec_name"), "pcm_s24be");
    assert_eq!(pcm(&w), pcm(&a));
    let m = d.p("s.m4a");
    run_attempts(&w, &convert_attempts(&p, "m4a", Speed::Best), &m);
    assert_eq!(field(&m, "a:0", "stream=codec_name"), "alac");
    assert_eq!(pcm(&w), pcm(&m), "m4a from a lossless source is lossless");

    // Lossy targets at their top settings, with resampling where the format needs it.
    let mp3 = d.p("s.mp3");
    run_attempts(&w, &convert_attempts(&p, "mp3", Speed::Best), &mp3);
    assert_eq!(field(&mp3, "a:0", "stream=bit_rate"), "320000");
    assert_eq!(field(&mp3, "a:0", "stream=sample_rate"), "48000");
    let wma = d.p("s.wma");
    run_attempts(&w, &convert_attempts(&p, "wma", Speed::Best), &wma);
    assert_eq!(field(&wma, "a:0", "stream=sample_rate"), "48000");
    if has_encoder("libopus") {
        let o = d.p("s.opus");
        run_attempts(&w, &convert_attempts(&p, "opus", Speed::Best), &o);
        assert_eq!(field(&o, "a:0", "stream=sample_rate"), "48000");
        // Stereo and surround have higher ceilings than mono.
        let st = d.p("st.flac");
        ffs(&["-f", "lavfi", "-i", "sine=r=48000:d=1", "-af", "pan=5.1|c0=c0|c1=c0|c2=c0|c3=c0|c4=c0|c5=c0", &st.display().to_string()]);
        let so = d.p("st.opus");
        run_attempts(&st, &convert_attempts(&probe(&st), "opus", Speed::Best), &so);
        assert_eq!(field(&so, "a:0", "stream=channels"), "6");
    }

    // Cover art survives.
    let jpg = d.p("c.jpg");
    ffs(&["-f", "lavfi", "-i", "testsrc2=s=64x64:d=1", "-frames:v", "1", &jpg.display().to_string()]);
    let tagged = d.p("t.flac");
    ffs(&["-i", &f.display().to_string(), "-i", &jpg.display().to_string(), "-map", "0:a", "-map", "1:v", "-c", "copy", "-disposition:v", "attached_pic", "-metadata", "title=Song", &tagged.display().to_string()]);
    let tp = probe(&tagged);
    assert!(tp.cover);
    for t in ["mp3", "m4a"] {
        let o = d.p(&format!("cover.{t}"));
        assert_eq!(run_attempts(&tagged, &convert_attempts(&tp, t, Speed::Best), &o), 0, "{t}: cover attempt worked");
        assert!(probe(&o).cover, "{t}: cover kept");
        assert!(field(&o, "", "format_tags=title").contains("Song"), "{t}: tags kept");
    }
}

#[test]
fn audio_edits_keep_the_format_at_its_best() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let s = d.p("s.mp3");
    ffs(&["-f", "lavfi", "-i", "sine=f=440:r=44100:d=2", "-ac", "2", "-c:a", "libmp3lame", "-b:a", "320k", &s.display().to_string()]);
    let p = probe(&s);
    let o = d.p("o.mp3");
    run_app_args(&s, &audio_edit_args(&p, "mp3", "loudnorm=I=-16:TP=-1.5:LRA=11"), &o).unwrap();
    assert_eq!(field(&o, "a:0", "stream=bit_rate"), "320000", "not the default 128k");
    assert_eq!(field(&o, "a:0", "stream=sample_rate"), "44100", "not the 192 kHz loudnorm leaves behind");
    let w = d.p("s.wav");
    ffs(&["-f", "lavfi", "-i", "sine=f=440:r=48000:d=2", "-ac", "2", "-c:a", "pcm_s24le", &w.display().to_string()]);
    let o2 = d.p("o.wav");
    run_app_args(&w, &audio_edit_args(&probe(&w), "wav", "pan=stereo|c0=c1|c1=c0"), &o2).unwrap();
    assert_eq!(field(&o2, "a:0", "stream=codec_name"), "pcm_s24le");
    assert_eq!(field(&o2, "a:0", "stream=sample_rate"), "48000");
}

#[test]
fn speed_and_crop_stay_lossless() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let s = d.p("s.mp4");
    ffs(&["-f", "lavfi", "-i", "testsrc2=s=640x360:r=30:d=2", "-f", "lavfi", "-i", "sine=r=48000:d=2", "-vf", "format=yuv420p10le", "-c:v", "libx264", "-crf", "20", "-c:a", "aac", &s.display().to_string()]);
    let p = probe(&s);
    let c = lossless_args(&p, "mp4", Some("crop=320:180:100:60"), None, false).unwrap();
    let o = d.p("c.mp4");
    run_app_args(&s, &c.args, &o).unwrap();
    assert_eq!(region(&s, Some((100, 60, 320, 180)), None), region(&o, None, None), "cropped pixels are the original pixels");
    assert_eq!(field(&o, "v:0", "stream=pix_fmt"), "yuv420p10le");

    let sp = lossless_args(&p, "mp4", Some("setpts=PTS/2"), Some("atempo=2.0000"), true).unwrap();
    let o = d.p("sp.mp4");
    run_app_args(&s, &sp.args, &o).unwrap();
    assert_eq!(region(&s, None, None), region(&o, None, None), "every frame kept, only faster");
    let dur: f64 = field(&o, "v:0", "stream=duration").parse().unwrap();
    assert!((dur - 1.0).abs() < 0.05, "duration {dur}");
    let br: u32 = field(&o, "a:0", "stream=bit_rate").parse().unwrap();
    assert!(br > 100_000, "audio bitrate {br}");
}

#[test]
fn trim_from_a_keyframe_copies_and_elsewhere_stays_lossless() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let s = d.p("s.mp4");
    ffs(&["-f", "lavfi", "-i", "testsrc2=s=640x360:r=30:d=4", "-f", "lavfi", "-i", "sine=r=48000:d=4", "-vf", "format=yuv420p", "-c:v", "libx264", "-crf", "20", "-g", "30", "-keyint_min", "30", "-sc_threshold", "0", "-c:a", "aac", &s.display().to_string()]);
    let all = region(&s, None, None);
    let p = probe(&s);
    let fps = p.video.as_ref().unwrap().fps;
    let packets = |t: f64| { let o = Command::new("ffprobe").args(keyframe_probe_args(&s.display().to_string(), t)).output().unwrap(); String::from_utf8_lossy(&o.stdout).to_string() };
    assert!(is_keyframe_at(&packets(1.0), 1.0, fps), "{}", packets(1.0));
    assert!(!is_keyframe_at(&packets(1.5), 1.5, fps));
    // Copy from a keyframe: the frames are the original frames, at no cost at all.
    let o = d.p("copy.mp4");
    // Only a cut running to the end of the file can copy: see trim_can_copy, and the frames counted here.
    let cp = |a: f64, b: f64, o: &Path| run_seek(&seek_args(a, b), &s, &copy_attempts(&[], &trim_copy_tail()).remove(0), o).unwrap();
    cp(1.0, 4.0, &o);
    assert_eq!(&all[30..], &region(&o, None, None)[..], "copied frames are exactly the originals");
    // From the very start too, where seeking after the input loses the first group of pictures.
    let o0 = d.p("copy0.mp4");
    cp(0.0, 4.0, &o0);
    assert_eq!(all, region(&o0, None, None), "the first frame is the first frame");
    // Audio cuts: lossy copies whole frames, lossless lands on the sample.
    let w = d.p("a.wav");
    ffs(&["-f", "lavfi", "-i", "sine=r=48000:d=4", "-c:a", "pcm_s24le", &w.display().to_string()]);
    let wo = d.p("cut.wav");
    run_seek(&seek_args(1.0, 2.0), &w, &audio_trim_args(&probe(&w), "wav"), &wo).unwrap();
    assert_eq!(field(&wo, "a:0", "stream=codec_name"), "pcm_s24le");
    assert_eq!(field(&wo, "a:0", "stream=duration_ts"), "48000", "exact samples");
    let m = d.p("a.mp3");
    ffs(&["-f", "lavfi", "-i", "sine=r=44100:d=4", "-c:a", "libmp3lame", "-b:a", "320k", &m.display().to_string()]);
    let mo = d.p("cut.mp3");
    run_seek(&seek_args(1.0, 2.0), &m, &audio_trim_args(&probe(&m), "mp3"), &mo).unwrap();
    assert_eq!(field(&mo, "a:0", "stream=bit_rate"), "320000", "copied, not re-encoded");
    let cut: f64 = field(&mo, "a:0", "stream=duration").parse().unwrap();
    assert!((cut - 1.0).abs() < 0.06, "{cut}");
    // From anywhere else: exact frames, through a lossless encode.
    let l = lossless_args(&p, "mp4", None, None, false).unwrap();
    let o = d.p("exact.mp4");
    run_seek(&seek_args(1.5, 2.5), &s, &l.args, &o).unwrap();
    let got = region(&o, None, None);
    assert_eq!(&all[45..75], &got[..], "the exact range, pixel for pixel");
}

#[test]
fn join_copies_when_it_can_and_loses_nothing_when_it_cannot() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let s = d.p("s.mp4");
    ffs(&["-f", "lavfi", "-i", "testsrc2=s=640x360:r=30:d=4", "-f", "lavfi", "-i", "sine=r=48000:d=4", "-vf", "format=yuv420p", "-c:v", "libx264", "-crf", "20", "-g", "30", "-c:a", "aac", &s.display().to_string()]);
    let probes = [probe(&s), probe(&s)];
    let exts = ["mp4".to_string(), "mp4".to_string()];
    assert!(can_concat_copy(&probes, &exts), "{:?}", probes);
    let list = d.p("list.txt");
    std::fs::write(&list, concat_list(&[s.display().to_string(), s.display().to_string()])).unwrap();
    let o = d.p("joined.mp4");
    ffs(&["-f", "concat", "-safe", "0", "-i", &list.display().to_string(), "-map", "0", "-c", "copy", "-map_metadata", "0", &o.display().to_string()]);
    let one = region(&s, None, None);
    let twice: Vec<String> = one.iter().chain(&one).cloned().collect();
    assert_eq!(region(&o, None, None), twice, "joined without a single changed frame");

    // Different clips: nothing is shrunk, slowed or flattened to match the first.
    let small = clip(&d, "small.mp4", "yuv420p", &["-c:v", "libx264", "-crf", "20"], "");
    let big = d.p("big.mp4");
    ffs(&["-f", "lavfi", "-i", "testsrc2=s=1280x720:r=60:d=1", "-vf", "format=yuv420p10le", "-c:v", "libx264", "-crf", "20", &big.display().to_string()]);
    let probes = [probe(&small), probe(&big)];
    let spec = join_spec(&probes);
    assert_eq!((spec.size, spec.pix.as_str()), ((1280, 720), "yuv420p10le"));
    let silent = [Some(1.0), Some(1.0)];
    let (ext, tail) = join_output_args(&spec, &probes);
    let o = d.p(&format!("mixed.{ext}"));
    let mut args = vec!["-i".to_string(), small.display().to_string(), "-i".to_string(), big.display().to_string(), "-filter_complex".to_string(), join_filter(&spec, &silent)];
    args.extend(tail);
    args.push(o.display().to_string());
    ff(&args).unwrap();
    assert_eq!(field(&o, "v:0", "stream=pix_fmt"), "yuv420p10le");
    assert_eq!(field(&o, "v:0", "stream=width"), "1280");
    assert_eq!(field(&o, "v:0", "stream=r_frame_rate"), "60/1");
    // The 720p clip passes through without being resampled: the second half is pixel for pixel the original.
    let got = region(&o, None, Some("yuv420p10le"));
    let want = region(&big, None, Some("yuv420p10le"));
    assert_eq!(&got[got.len() - want.len()..], &want[..], "matching clip is untouched");
}

#[test]
fn compress_spends_only_what_was_chosen() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let s = d.p("s.mkv");
    ffs(&["-f", "lavfi", "-i", "testsrc2=s=800x600:r=30:d=1", "-f", "lavfi", "-i", "sine=r=48000:d=1", "-vf", "format=yuv444p10le,crop=641:360:0:0,format=yuv420p10le", "-c:v", "ffv1", "-c:a", "aac", &s.display().to_string()]);
    assert_eq!(field(&s, "v:0", "stream=width"), "641", "fixture");
    let o = d.p("o.mp4");
    run_app_args(&s, &compress_video_args(&probe(&s), &Squeeze { crf: 23, height: None, audio_kbps: None, fps: None }), &o).unwrap();
    assert_eq!(field(&o, "v:0", "stream=pix_fmt"), "yuv420p10le", "HDR-capable depth kept");
    assert_eq!(field(&o, "v:0", "stream=width"), "642", "padded, not resampled");
    let o = d.p("h.mp4");
    run_app_args(&s, &compress_video_args(&probe(&s), &Squeeze { crf: 23, height: Some(180), audio_kbps: None, fps: None }), &o).unwrap();
    assert_eq!(field(&o, "v:0", "stream=height"), "180");
    assert_eq!(field(&o, "a:0", "stream=codec_name"), "aac", "no allowance, so the audio is only copied");
}

#[test]
fn custom_compress_squeezes_picture_and_audio_as_far_as_asked() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let s = d.p("s.mkv");
    ffs(&["-f", "lavfi", "-i", "testsrc2=s=800x600:r=30:d=2", "-f", "lavfi", "-i", "sine=r=48000:d=2", "-c:v", "libx264", "-crf", "12", "-c:a", "ac3", "-ac", "2", "-b:a", "192k", &s.display().to_string()]);
    let o = d.p("o.mp4");
    run_app_args(&s, &compress_video_args(&probe(&s), &Squeeze { crf: 51, height: Some(144), audio_kbps: Some(8), fps: Some(5) }), &o).unwrap();
    assert_eq!(field(&o, "v:0", "stream=height"), "144");
    assert_eq!(field(&o, "v:0", "stream=avg_frame_rate"), "5/1");
    assert_eq!(field(&o, "a:0", "stream=codec_name"), "aac");
    assert_eq!(field(&o, "a:0", "stream=channels"), "1");
    assert_eq!(field(&o, "a:0", "stream=sample_rate"), "11025");
    assert!(std::fs::metadata(&o).unwrap().len() < std::fs::metadata(&s).unwrap().len() / 20, "far smaller");
}

#[test]
fn edited_jpegs_save_at_top_quality_with_full_colour() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let s = d.p("s.png");
    ffs(&["-f", "lavfi", "-i", "testsrc2=s=640x360:d=1", "-frames:v", "1", &s.display().to_string()]);
    let mut a = vec!["-vf".to_string(), "hflip,hflip".to_string(), "-frames:v".to_string(), "1".to_string(), "-update".to_string(), "1".to_string()];
    a.extend(image_encode_args("jpg"));
    let o = d.p("o.jpg");
    run_app_args(&s, &a, &o).unwrap();
    let f = field(&o, "v:0", "stream=pix_fmt");
    assert!(f.contains("444"), "chroma not subsampled: {f}");
    let v = psnr(&o, &s);
    assert!(v > 40.0, "psnr {v}");
    println!("jpeg edit: {v:.1} dB, {f}");
    let info = jpeg_info(&std::fs::read(&o).unwrap()).unwrap();
    assert_eq!((info.w, info.h, info.comps), (640, 360, 3));
}

#[test]
fn snapshot_of_deep_video_stays_deep() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let s = clip(&d, "s.mp4", "yuv420p10le", &["-c:v", "libx264", "-crf", "20"], "");
    let o = d.p("f.png");
    ffs(&["-i", &s.display().to_string(), "-frames:v", "1", "-sws_flags", "lanczos+accurate_rnd+full_chroma_int", &o.display().to_string()]);
    assert_eq!(field(&o, "v:0", "stream=pix_fmt"), "rgb48be");
}

/// These need encoders that not every ffmpeg build has, so each runs only where its encoder exists and says when it cannot.
#[test]
fn encoders_some_builds_lack() {
    if !have_ffmpeg() { return; }
    let d = Dir::new();
    let png = d.p("s.png");
    ffs(&["-f", "lavfi", "-i", "testsrc2=s=640x360:d=1", "-frames:v", "1", "-pix_fmt", "rgb24", &png.display().to_string()]);
    let rgb = |f: &Path| region(f, None, Some("rgb24"));
    let jpg = d.p("s.jpg");
    ffs(&["-i", &png.display().to_string(), "-q:v", "3", &jpg.display().to_string()]);
    let limit = std::fs::metadata(&jpg).unwrap().len();
    let fits = |ext: &str| {
        let mut size = 0;
        for enc in still_ladder(ext) {
            let o = d.p(&format!("ladder.{ext}"));
            run_app_args(&jpg, &enc, &o).unwrap_or_else(|e| panic!("{ext} ladder: {e}"));
            size = std::fs::metadata(&o).unwrap().len();
            if size <= limit { break; }
        }
        println!("{ext} from a {limit} byte JPEG: {size} bytes");
    };
    if has_encoder("libwebp") {
        let o = d.p("s.webp");
        run_app_args(&png, &image_encode_args("webp"), &o).unwrap();
        assert_eq!(rgb(&png), rgb(&o), "lossless WebP");
        fits("webp");
    } else { println!("not checked: WebP, this ffmpeg has no libwebp"); }
    if has_encoder("libaom-av1") {
        let o = d.p("s.avif");
        run_app_args(&png, &avif_lossless_args(), &o).unwrap();
        assert_eq!(rgb(&png), rgb(&o), "lossless AVIF");
        fits("avif");
    } else { println!("not checked: AVIF, this ffmpeg has no libaom"); }
    if has_encoder("libvorbis") {
        let f = d.p("s.flac");
        ffs(&["-f", "lavfi", "-i", "sine=r=44100:d=2", "-ac", "2", &f.display().to_string()]);
        let o = d.p("s.ogg");
        run_attempts(&f, &convert_attempts(&probe(&f), "ogg", Speed::Best), &o);
        assert_eq!(field(&o, "a:0", "stream=codec_name"), "vorbis");
        let m = d.p("s.mp3");
        ffs(&["-i", &f.display().to_string(), "-c:a", "libmp3lame", "-b:a", "128k", &m.display().to_string()]);
        let o2 = d.p("m.ogg");
        run_attempts(&m, &convert_attempts(&probe(&m), "ogg", Speed::Best), &o2);
        assert_eq!(field(&o2, "a:0", "stream=codec_name"), "vorbis");
    } else { println!("not checked: Ogg Vorbis, this ffmpeg has no libvorbis"); }
}
