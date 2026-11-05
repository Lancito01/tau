//! Every decision about how much quality survives an operation. Pure logic with no Win32, so
//! `tests/fidelity.rs` can put these exact arguments through a real ffmpeg on any OS.
//!
//! The order of preference, everywhere:
//!   1. Copy the original bits (stream copy, remux, embedding a JPEG as is).
//!   2. Lossless encode, when the operation keeps the file's format: redact, crop, trim, split, speed, join.
//!   3. The highest setting of the target codec, when converting to a format that is lossy by nature.
//! Compress is the only tool that gives quality away, because that is what the user asked it to do.
//!
//! Never trust a filter or an encoder to keep the pixel format by itself. ffmpeg converts silently when
//! one does not support it (drawbox turns 10-bit into 8-bit without a word), so formats are set explicitly.
use std::collections::BTreeMap;

/// x264 at this CRF stays in the High profile that every player decodes, unlike true lossless.
pub const CONVERT_X264_CRF: &str = "10";
const CONVERT_VP9_CRF: &str = "4";
/// GIF timing has centisecond steps, and browsers slow down anything faster than 2 of them.
const GIF_MAX_FPS: f64 = 50.0;

fn sv(v: &[&str]) -> Vec<String> { v.iter().map(|x| x.to_string()).collect() }

// ---- what is in the file ----

#[derive(Clone, Default, Debug, PartialEq)]
pub struct VideoInfo {
    pub codec: String,
    pub pix_fmt: String,
    pub width: u32,
    pub height: u32,
    pub rotation: i32,
    pub fps: f64,
    pub full_range: bool,
    pub extradata: String,
}

impl VideoInfo {
    /// The size filters see and the user sees, which differs from the stored size on phone video.
    pub fn display_size(&self) -> (u32, u32) {
        if self.rotation.rem_euclid(180) == 90 { (self.height, self.width) } else { (self.width, self.height) }
    }
}

#[derive(Clone, Default, Debug, PartialEq)]
pub struct AudioInfo {
    pub codec: String,
    pub sample_fmt: String,
    pub bits: u32,
    pub rate: u32,
    pub channels: u32,
}

impl AudioInfo {
    pub fn is_lossless(&self) -> bool {
        self.codec.starts_with("pcm_") || matches!(self.codec.as_str(), "flac" | "alac" | "wavpack" | "ape" | "tta" | "truehd" | "mlp")
    }
}

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Probe {
    pub video: Option<VideoInfo>,
    /// Cover art, which ffmpeg lists as a video stream.
    pub cover: bool,
    pub audio: Vec<AudioInfo>,
    pub subs: Vec<String>,
}

/// ffprobe arguments whose output `parse_probe` reads.
pub fn probe_args(path: &str) -> Vec<String> {
    sv(&["-v", "error", "-show_data_hash", "md5", "-show_entries",
        "stream=codec_type,codec_name,pix_fmt,width,height,avg_frame_rate,r_frame_rate,color_range,sample_fmt,bits_per_sample,bits_per_raw_sample,sample_rate,channels,extradata_hash:stream_disposition=attached_pic:stream_side_data=rotation",
        "-of", "flat", path])
}

fn parse_rate(s: &str) -> f64 {
    match s.split_once('/') {
        Some((a, b)) => { let (a, b): (f64, f64) = (a.parse().unwrap_or(0.0), b.parse().unwrap_or(0.0)); if b > 0.0 { a / b } else { 0.0 } }
        None => s.parse().unwrap_or(0.0),
    }
}

pub fn parse_probe(flat: &str) -> Probe {
    let mut streams: BTreeMap<u32, BTreeMap<String, String>> = BTreeMap::new();
    for line in flat.lines() {
        let Some(rest) = line.strip_prefix("streams.stream.") else { continue };
        let Some((n, kv)) = rest.split_once('.') else { continue };
        let Some((k, v)) = kv.split_once('=') else { continue };
        let Ok(n) = n.parse() else { continue };
        streams.entry(n).or_default().insert(k.to_string(), v.trim().trim_matches('"').to_string());
    }
    let mut p = Probe::default();
    for s in streams.values() {
        let get = |k: &str| s.get(k).map(String::as_str).unwrap_or("");
        let num = |k: &str| get(k).parse::<u32>().unwrap_or(0);
        match get("codec_type") {
            "video" if get("disposition.attached_pic") == "1" => p.cover = true,
            "video" if p.video.is_none() => {
                let fps = parse_rate(get("avg_frame_rate"));
                p.video = Some(VideoInfo {
                    codec: get("codec_name").into(),
                    pix_fmt: get("pix_fmt").into(),
                    width: num("width"),
                    height: num("height"),
                    rotation: get("side_data_list.side_data.0.rotation").parse().unwrap_or(0),
                    fps: if fps > 0.0 { fps } else { parse_rate(get("r_frame_rate")) },
                    full_range: get("color_range") == "pc" || get("pix_fmt").starts_with("yuvj"),
                    extradata: get("extradata_hash").into(),
                });
            }
            "audio" => p.audio.push(AudioInfo {
                codec: get("codec_name").into(),
                sample_fmt: get("sample_fmt").into(),
                bits: num("bits_per_raw_sample").max(num("bits_per_sample")),
                rate: num("sample_rate"),
                channels: num("channels"),
            }),
            "subtitle" => p.subs.push(get("codec_name").into()),
            _ => {}
        }
    }
    p
}

// ---- pixel formats ----

/// The planar format holding exactly the same samples. Packed and semi-planar layouts just rearrange them.
pub fn planar_of(pix: &str) -> Option<String> {
    let f = match pix {
        "nv12" | "nv21" => "yuv420p",
        "nv16" | "yuyv422" | "uyvy422" => "yuv422p",
        "nv24" | "nv42" => "yuv444p",
        "p010le" | "p010be" => "yuv420p10le",
        "p210le" | "p210be" | "y210le" | "y210be" => "yuv422p10le",
        "p410le" | "p410be" => "yuv444p10le",
        "p012le" | "p012be" => "yuv420p12le",
        "rgb24" | "bgr24" | "rgb0" | "bgr0" | "0rgb" | "0bgr" => "gbrp",
        "rgba" | "bgra" | "argb" | "abgr" | "pal8" => "gbrap",
        "rgb48le" | "rgb48be" | "bgr48le" | "bgr48be" => "gbrp16le",
        "rgba64le" | "rgba64be" | "bgra64le" | "bgra64be" => "gbrap16le",
        p if p.starts_with("yuv") || p.starts_with("gbr") || p.starts_with("gray") => p,
        _ => return None,
    };
    Some(f.to_string())
}

/// Bits per sample of a planar format name: yuv420p10le is 10, yuv420p is 8.
pub fn depth_of(planar: &str) -> u32 {
    let t = planar.trim_end_matches("le").trim_end_matches("be");
    let digits: String = t.chars().rev().take_while(char::is_ascii_digit).collect::<Vec<_>>().into_iter().rev().collect();
    // The 8 in gray8, and the digits of yuv420 before its p, are not what trailing digits after a p mean.
    if t.starts_with("gray") { return digits.parse().unwrap_or(8); }
    if t.ends_with(|c: char| !c.is_ascii_digit()) { return 8; }
    digits.parse().unwrap_or(8)
}

fn is_rgb(planar: &str) -> bool { planar.starts_with("gbr") }
fn has_alpha(planar: &str) -> bool { planar.starts_with("yuva") || planar.starts_with("gbra") || planar.starts_with("graya") }

/// True when a dimension is odd in a direction the chroma is subsampled, which H.264 cannot store.
fn odd_for_chroma(planar: &str, w: u32, h: u32) -> bool {
    let sub_w = ["420", "422", "411", "410"].iter().any(|c| planar.contains(c));
    let sub_h = ["420", "440", "410"].iter().any(|c| planar.contains(c));
    (sub_w && w % 2 == 1) || (sub_h && h % 2 == 1)
}

fn x264_holds(planar: &str) -> bool {
    matches!(planar, "yuv420p" | "yuvj420p" | "yuv422p" | "yuvj422p" | "yuv444p" | "yuvj444p" | "yuv420p10le" | "yuv422p10le" | "yuv444p10le")
}

fn vp9_holds(planar: &str) -> bool {
    matches!(planar, "yuv420p" | "yuva420p" | "yuv422p" | "yuva422p" | "yuv440p" | "yuv444p" | "yuva444p"
        | "yuv420p10le" | "yuva420p10le" | "yuv422p10le" | "yuva422p10le" | "yuv440p10le" | "yuv444p10le" | "yuva444p10le"
        | "yuv420p12le" | "yuv422p12le" | "yuv440p12le" | "yuv444p12le" | "yuva444p12le"
        | "gbrp" | "gbrap" | "gbrp10le" | "gbrap10le" | "gbrp12le" | "gbrap12le")
}

/// Closest format H.264 can hold, never dropping chroma or depth it has room for.
fn x264_nearest(pix: &str) -> String {
    let p = planar_of(pix).unwrap_or_else(|| "yuv444p".into());
    if x264_holds(&p) { return p; }
    let deep = depth_of(&p) > 8;
    // Grey comes out of H.264 as colour with neutral chroma, so the lightest chroma loses nothing.
    if p.starts_with("gray") { return if deep { "yuv420p10le".into() } else { "yuv420p".into() }; }
    let chroma = if p.contains("420") { "420" } else if p.contains("422") { "422" } else { "444" };
    format!("yuv{chroma}p{}", if deep { "10le" } else { "" })
}

/// Same idea for VP9, which has no greyscale and no separate full-range formats.
fn vp9_nearest(pix: &str) -> String {
    let p = planar_of(pix).unwrap_or_else(|| "yuv444p".into()).replace("yuvj", "yuv");
    if vp9_holds(&p) { return p; }
    if p.starts_with("gray") { return if depth_of(&p) > 8 { "yuv420p10le".into() } else { "yuv420p".into() }; }
    format!("yuv444p{}", if depth_of(&p) > 8 { "12le" } else { "" })
}

// ---- audio ----

/// Whether a codec can go into a container without touching it.
fn audio_fits(container: &str, codec: &str) -> bool {
    match container {
        "mp4" | "m4v" => matches!(codec, "aac" | "mp3" | "ac3" | "eac3" | "alac" | "opus"),
        "mov" => matches!(codec, "aac" | "mp3" | "ac3" | "eac3" | "alac") || codec.starts_with("pcm_"),
        "mkv" => !codec.is_empty(),
        "webm" => matches!(codec, "opus" | "vorbis"),
        "avi" => matches!(codec, "mp3" | "ac3" | "pcm_s16le"),
        "wmv" | "wma" => matches!(codec, "wmav1" | "wmav2" | "wmapro"),
        "mp3" => codec == "mp3",
        "m4a" => matches!(codec, "aac" | "alac"),
        "aac" => codec == "aac",
        "wav" => matches!(codec, "pcm_s16le" | "pcm_s24le" | "pcm_s32le" | "pcm_f32le" | "pcm_f64le" | "pcm_u8"),
        "flac" => codec == "flac",
        "ogg" => matches!(codec, "vorbis" | "opus" | "flac"),
        "opus" => codec == "opus",
        "aiff" | "aif" => matches!(codec, "pcm_s16be" | "pcm_s24be" | "pcm_s32be" | "pcm_f32be" | "pcm_f64be" | "pcm_s8"),
        _ => false,
    }
}

/// The PCM that holds every bit of the source. Lossy sources decode to float, which also keeps peaks above full scale.
fn pcm_codec(a: &AudioInfo, big_endian: bool) -> String {
    let e = if big_endian { "be" } else { "le" };
    match a.sample_fmt.trim_end_matches('p') {
        "s16" => format!("pcm_s16{e}"),
        "s32" | "s64" => if a.bits > 0 && a.bits <= 24 { format!("pcm_s24{e}") } else { format!("pcm_s32{e}") },
        "flt" => format!("pcm_f32{e}"),
        "dbl" => format!("pcm_f64{e}"),
        "u8" => if big_endian { "pcm_s8".into() } else { "pcm_u8".into() },
        _ => format!("pcm_s24{e}"),
    }
}

/// Per-stream options for encoding one audio stream at the best the target allows. `extra` is a filter to run first.
fn encode_audio(target: &str, a: &AudioInfo, extra: Option<&str>) -> Vec<(&'static str, String)> {
    let ch = a.channels.max(2);
    let mut o: Vec<(&'static str, String)> = vec![];
    let mut cap = 0u32;
    match target {
        "mp3" | "avi" => {
            o.extend([("c", "libmp3lame".to_string()), ("b", "320k".to_string()), ("compression_level", "0".to_string())]);
            cap = 48000;
            if a.channels > 2 { o.push(("ac", "2".into())); }
        }
        "wma" | "wmv" => {
            o.extend([("c", "wmav2".to_string()), ("b", "320k".to_string())]);
            cap = 48000;
            if a.channels > 2 { o.push(("ac", "2".into())); }
        }
        "m4a" | "mp4" | "m4v" | "mov" if a.is_lossless() => o.push(("c", "alac".into())),
        "m4a" | "mp4" | "m4v" | "mov" | "aac" => o.extend([("c", "aac".to_string()), ("b", format!("{}k", 160 * ch))]),
        "mkv" | "flac" => o.extend([("c", "flac".to_string()), ("compression_level", "8".to_string())]),
        "ogg" => o.extend([("c", "libvorbis".to_string()), ("q", "10".to_string())]),
        "opus" | "webm" => {
            // libopus refuses more than 256 kbps per channel.
            o.extend([("c", "libopus".to_string()), ("b", format!("{}k", 256 * a.channels.max(1)))]);
            cap = 48000;
            if a.channels > 2 { o.push(("mapping_family", "1".into())); }
        }
        "wav" => o.push(("c", pcm_codec(a, false))),
        "aiff" | "aif" => o.push(("c", pcm_codec(a, true))),
        _ => o.extend([("c", "aac".to_string()), ("b", format!("{}k", 160 * ch))]),
    }
    let rate = if target == "opus" || target == "webm" { 48000 } else if cap > 0 && a.rate > cap { if a.rate % 44100 == 0 { 44100 } else { cap } } else { a.rate };
    let mut filters: Vec<String> = extra.map(str::to_string).into_iter().collect();
    // ffmpeg's automatic resampling is the default-quality one. loudnorm also leaves the audio at 192 kHz.
    if rate > 0 && (rate != a.rate || extra.is_some()) { filters.push(format!("aresample={rate}:filter_size=128:phase_shift=14")); }
    if !filters.is_empty() { o.push(("filter", filters.join(","))); }
    if rate > 0 && (rate != a.rate || extra.is_some()) { o.push(("ar", rate.to_string())); }
    o
}

fn push_audio(args: &mut Vec<String>, j: usize, opts: &[(&str, String)]) {
    for (k, v) in opts { args.push(format!("-{k}:a:{j}")); args.push(v.clone()); }
}

/// Maps every audio track. Tracks that fit are copied, unless a filter means they must be re-encoded.
fn audio_streams(p: &Probe, container: &str, extra: Option<&str>) -> Vec<String> {
    let mut a = vec![];
    for (i, info) in p.audio.iter().enumerate() {
        a.extend(["-map".to_string(), format!("0:a:{i}")]);
        if extra.is_none() && audio_fits(container, &info.codec) {
            a.extend([format!("-c:a:{i}"), "copy".to_string()]);
        } else {
            push_audio(&mut a, i, &encode_audio(container, info, extra));
        }
    }
    a
}

// ---- subtitles ----

fn sub_is_text(codec: &str) -> bool { matches!(codec, "subrip" | "srt" | "ass" | "ssa" | "webvtt" | "mov_text" | "text") }

/// Maps every subtitle track the container can hold, converting between text formats where it must.
/// Picture subtitles have no form an MP4 or WebM can carry, so only MKV keeps them.
fn sub_streams(p: &Probe, container: &str) -> Vec<String> {
    let mut a = vec![];
    let mut j = 0;
    for (i, c) in p.subs.iter().enumerate() {
        let to = match container {
            "mp4" | "m4v" | "mov" => if sub_is_text(c) { Some(if c == "mov_text" { "copy" } else { "mov_text" }) } else { None },
            "mkv" => Some(if c == "mov_text" { "srt" } else { "copy" }),
            "webm" => if sub_is_text(c) { Some(if c == "webvtt" { "copy" } else { "webvtt" }) } else { None },
            _ => None,
        };
        if let Some(t) = to {
            a.extend(["-map".to_string(), format!("0:s:{i}"), format!("-c:s:{j}"), t.to_string()]);
            j += 1;
        }
    }
    a
}

fn container_flags(container: &str) -> Vec<String> {
    match container {
        "mp4" | "m4v" | "mov" => sv(&["-movflags", "+faststart+use_metadata_tags"]),
        _ => vec![],
    }
}

fn keep_frames_as_they_are(container: &str) -> Vec<String> {
    // Without this ffmpeg may drop or repeat frames to fit a rate. AVI and ASF have no choice but a fixed one.
    if matches!(container, "avi" | "wmv") { vec![] } else { sv(&["-fps_mode", "passthrough"]) }
}

// ---- lossless, for operations that keep the file's format ----

pub struct Lossless {
    /// Differs from the source extension only when that container cannot hold lossless video.
    pub ext: String,
    pub args: Vec<String>,
}

/// Exact encoder for these pixels, or which container to fall back to. Returns (ext, encoder args).
fn lossless_video(planar: &str, w: u32, h: u32, src_ext: &str) -> (String, Vec<String>) {
    let odd = odd_for_chroma(planar, w, h);
    match src_ext {
        "mp4" | "m4v" | "mov" | "mkv" if x264_holds(planar) && !odd => (src_ext.to_string(), sv(&["-c:v", "libx264", "-qp", "0", "-preset", "medium"])),
        "webm" if vp9_holds(planar) && !odd => ("webm".to_string(), sv(&["-c:v", "libvpx-vp9", "-lossless", "1", "-row-mt", "1", "-deadline", "good", "-cpu-used", "2"])),
        _ => {
            let mut a = sv(&["-c:v", "ffv1", "-level", "3", "-g", "1", "-slices", "16", "-slicecrc", "1"]);
            // FFV1 stores 8-bit RGB as packed, which holds the same samples.
            if planar == "gbrp" { a.extend(sv(&["-pix_fmt", "bgr0"])); }
            if planar == "gbrap" { a.extend(sv(&["-pix_fmt", "bgra"])); }
            ("mkv".to_string(), a)
        }
    }
}

/// Re-encodes video so every frame decodes to exactly what `filter` produced. `audio_filter` forces audio through
/// an encoder; timing filters pass `drop_timed` too, since subtitles and chapters would no longer line up.
pub fn lossless_args(p: &Probe, src_ext: &str, filter: Option<&str>, audio_filter: Option<&str>, drop_timed: bool) -> Option<Lossless> {
    let v = p.video.as_ref()?;
    let planar = planar_of(&v.pix_fmt)?;
    let (ow, oh) = v.display_size();
    let (ext, enc) = lossless_video(&planar, ow, oh, src_ext);
    let mut a = sv(&["-map", "0:V:0"]);
    let chain = match filter { Some(f) => format!("format={planar},{f}"), None => format!("format={planar}") };
    a.extend(["-vf".to_string(), chain]);
    a.extend(enc);
    a.extend(audio_streams(p, &ext, audio_filter));
    if drop_timed { a.extend(sv(&["-map_chapters", "-1"])); } else {
        a.extend(sub_streams(p, &ext));
        a.extend(sv(&["-map_chapters", "0"]));
        if ext == "mkv" { a.extend(sv(&["-map", "0:t?", "-c:t", "copy"])); }
    }
    a.extend(sv(&["-map_metadata", "0"]));
    a.extend(keep_frames_as_they_are(&ext));
    a.extend(container_flags(&ext));
    Some(Lossless { ext, args: a })
}

/// Everything as it is, for edits that do not touch the pictures. `-map 0` fails on odd data tracks in
/// some containers, so the second attempt leaves those out.
pub fn copy_attempts(leading: &[String], tail: &[String]) -> Vec<Vec<String>> {
    let build = |maps: &[&str]| {
        let mut a = leading.to_vec();
        a.extend(sv(maps));
        a.extend_from_slice(tail);
        a
    };
    vec![build(&["-map", "0", "-c", "copy"]), build(&["-map", "0:v?", "-map", "0:a?", "-map", "0:s?", "-c", "copy"])]
}

/// A GIF edited by a filter goes back through a fresh palette for every frame, undithered so that frames
/// with few colours come back unchanged.
pub fn gif_edit_args(filter: Option<&str>) -> Vec<String> {
    let lead = filter.map(|f| format!("{f},")).unwrap_or_default();
    vec!["-vf".into(), format!("{lead}split[a][b];[a]palettegen=stats_mode=single[p];[b][p]paletteuse=new=1:dither=none:diff_mode=none"), "-gifflags".into(), "-transdiff".into()]
}

// ---- redaction ----

/// Grows a box to even edges, so it covers whole chroma samples and no colour leaks out at its border.
pub fn redact_box(x: u32, y: u32, w: u32, h: u32, fw: u32, fh: u32) -> (u32, u32, u32, u32) {
    let (x0, y0) = (x & !1, y & !1);
    let (x1, y1) = ((x + w + 1) & !1, (y + h + 1) & !1);
    (x0, y0, x1.min(fw).saturating_sub(x0).max(1), y1.min(fh).saturating_sub(y0).max(1))
}

/// Lossless, minus anything that can hold a picture of what was just covered. Cover art never gets this far, since only
/// the main video stream is mapped, but an MKV can carry a thumbnail as an attachment. Fonts go with it, which only
/// changes how styled subtitles look.
pub fn redact_args(p: &Probe, src_ext: &str, filter: &str) -> Option<Lossless> {
    let mut l = lossless_args(p, src_ext, Some(filter), None, false)?;
    let attachments = sv(&["-map", "0:t?", "-c:t", "copy"]);
    if let Some(at) = l.args.windows(4).position(|w| w == attachments.as_slice()) { l.args.drain(at..at + 4); }
    Some(l)
}

/// Solid black over the boxes, in the source's own format. geq rather than drawbox because drawbox only
/// handles 8-bit: on anything deeper ffmpeg would silently convert the whole video first.
pub fn redact_filter(pix: &str, full_range: bool, boxes: &[(u32, u32, u32, u32)]) -> Option<String> {
    let planar = planar_of(pix)?;
    let c = boxes.iter().map(|(x, y, w, h)| format!("between(X/SW,{x},{})*between(Y/SH,{y},{})", x + w - 1, y + h - 1)).collect::<Vec<_>>().join("+");
    let d = depth_of(&planar);
    let alpha = if has_alpha(&planar) { ":a='alpha(X,Y)'" } else { "" };
    Some(if is_rgb(&planar) {
        format!("geq=r='if({c},0,r(X,Y))':g='if({c},0,g(X,Y))':b='if({c},0,b(X,Y))'{alpha}")
    } else if planar.starts_with("gray") {
        format!("geq=lum='if({c},{},p(X,Y))'", if full_range { 0 } else { 16u32 << (d - 8) })
    } else {
        let (y, n) = (if full_range { 0 } else { 16u32 << (d - 8) }, 128u32 << (d - 8));
        format!("geq=lum='if({c},{y},p(X,Y))':cb='if({c},{n},p(X,Y))':cr='if({c},{n},p(X,Y))'{alpha}")
    })
}

// ---- conversion, where the target format decides what is possible ----

fn video_fits(container: &str, codec: &str) -> bool {
    match container {
        "mp4" | "m4v" => matches!(codec, "h264" | "hevc" | "av1" | "mpeg4" | "vp9" | "mpeg2video" | "mjpeg"),
        "mov" => matches!(codec, "h264" | "hevc" | "prores" | "mjpeg" | "mpeg4" | "dvvideo" | "qtrle" | "png" | "mpeg2video"),
        "mkv" => !matches!(codec, "" | "gif"),
        "webm" => matches!(codec, "vp8" | "vp9" | "av1"),
        "avi" => matches!(codec, "mpeg4" | "msmpeg4v3" | "mjpeg" | "ffv1" | "huffyuv"),
        "wmv" => matches!(codec, "wmv1" | "wmv2" | "wmv3" | "vc1"),
        _ => false,
    }
}

/// Encoder arguments for a video target at its top setting. Keeps depth and chroma, and pads rather than
/// resamples a frame whose odd size a codec cannot store: a scale filter would soften every pixel to add one row.
fn encode_video(target: &str, v: Option<&VideoInfo>) -> Vec<String> {
    let pix = v.map(|i| i.pix_fmt.as_str()).unwrap_or("");
    let mut a: Vec<String> = vec![];
    let fmt = match target {
        "mp4" | "m4v" | "mov" | "mkv" => {
            a.extend(sv(&["-c:v", "libx264", "-preset", "slow", "-crf", CONVERT_X264_CRF]));
            (!pix.is_empty()).then(|| x264_nearest(pix))
        }
        "webm" => {
            a.extend(sv(&["-c:v", "libvpx-vp9", "-b:v", "0", "-crf", CONVERT_VP9_CRF, "-qmin", "0", "-qmax", "20", "-deadline", "good", "-cpu-used", "2", "-row-mt", "1"]));
            (!pix.is_empty()).then(|| vp9_nearest(pix))
        }
        // These codecs stop at 8-bit 4:2:0. Scale 1 is their finest.
        "avi" => { a.extend(sv(&["-c:v", "mpeg4", "-q:v", "1", "-qmin", "1", "-mbd", "rd"])); Some("yuv420p".to_string()) }
        "wmv" => { a.extend(sv(&["-c:v", "wmv2", "-q:v", "1", "-qmin", "1", "-mbd", "rd"])); Some("yuv420p".to_string()) }
        _ => None,
    };
    let needs_pad = match (&fmt, v) {
        (Some(f), Some(i)) => { let (w, h) = i.display_size(); odd_for_chroma(f, w, h) }
        (Some(f), None) => f == "yuv420p",
        // Nothing known about the source, so be safe. Padding an even frame changes nothing.
        (None, _) => true,
    };
    let mut chain = vec![];
    if needs_pad { chain.push("pad=ceil(iw/2)*2:ceil(ih/2)*2".to_string()); }
    if let Some(f) = &fmt {
        chain.push(format!("format={f}"));
        a.extend(["-pix_fmt".to_string(), f.clone()]);
    }
    if !chain.is_empty() { a.extend(["-vf".to_string(), chain.join(",")]); }
    a.extend(sv(&["-sws_flags", "lanczos+accurate_rnd+full_chroma_int"]));
    a
}

fn gif_args(v: Option<&VideoInfo>) -> Vec<String> {
    let cap = v.is_some_and(|i| i.fps > GIF_MAX_FPS);
    let lead = if cap { format!("fps={GIF_MAX_FPS},") } else { String::new() };
    vec!["-map".into(), "0:V:0".into(),
        "-vf".into(), format!("{lead}split[a][b];[a]palettegen=stats_mode=single[p];[b][p]paletteuse=new=1:dither=floyd_steinberg"),
        "-sws_flags".into(), "lanczos+accurate_rnd+full_chroma_int".into(), "-an".into()]
}

pub fn audio_target_args(p: &Probe, target: &str, with_cover: bool) -> Vec<String> {
    let mut a = sv(&["-map", "0:a:0"]);
    let info = p.audio.first().cloned().unwrap_or_default();
    if !p.audio.is_empty() && audio_fits(target, &info.codec) {
        a.extend(sv(&["-c:a:0", "copy"]));
    } else {
        push_audio(&mut a, 0, &encode_audio(target, &info, None));
    }
    if with_cover { a.extend(sv(&["-map", "0:v:0", "-c:v", "copy", "-disposition:v", "attached_pic"])); }
    a.extend(sv(&["-map_metadata", "0"]));
    a
}

/// Attempts in order of how much of the original they keep. A later one runs only if the one before failed,
/// which is how the unusual combinations a table cannot predict still end up with a file.
pub fn convert_attempts(p: &Probe, target: &str) -> Vec<Vec<String>> {
    match target {
        "gif" => vec![gif_args(p.video.as_ref())],
        "mp3" | "m4a" | "wav" | "flac" | "ogg" | "opus" | "aiff" | "wma" => {
            let mut v = vec![];
            if p.cover && p.video.is_none() && matches!(target, "mp3" | "m4a" | "flac") { v.push(audio_target_args(p, target, true)); }
            v.push(audio_target_args(p, target, false));
            v
        }
        _ => {
            let Some(info) = p.video.as_ref() else {
                // ffprobe is missing or could not read the file: still the best encoder settings, for what ffmpeg picks.
                let mut a = encode_video(target, None);
                push_audio(&mut a, 0, &encode_audio(target, &AudioInfo::default(), None));
                a.extend(keep_frames_as_they_are(target));
                a.extend(container_flags(target));
                return vec![a];
            };
            let build = |copy: bool, subs: bool| {
                let mut a = sv(&["-map", "0:V:0"]);
                if copy {
                    a.extend(sv(&["-c:v", "copy"]));
                    // Windows and Apple players want this tag on HEVC in these containers.
                    if info.codec == "hevc" && matches!(target, "mp4" | "m4v" | "mov") { a.extend(sv(&["-tag:v", "hvc1"])); }
                } else {
                    a.extend(encode_video(target, Some(info)));
                    a.extend(keep_frames_as_they_are(target));
                }
                a.extend(audio_streams(p, target, None));
                if subs {
                    a.extend(sub_streams(p, target));
                    if target == "mkv" { a.extend(sv(&["-map", "0:t?", "-c:t", "copy"])); }
                }
                a.extend(sv(&["-map_metadata", "0", "-map_chapters", "0"]));
                a.extend(container_flags(target));
                a
            };
            let mut v = vec![];
            if video_fits(target, &info.codec) { v.push(build(true, true)); }
            v.push(build(false, true));
            if !p.subs.is_empty() { v.push(build(false, false)); }
            v
        }
    }
}

/// Audio filtered in place (normalize, channels, bleep): the same format at its best settings.
pub fn audio_edit_args(p: &Probe, ext: &str, filter: &str) -> Vec<String> {
    let info = p.audio.first().cloned().unwrap_or_default();
    let mut a = sv(&["-map", "0:a:0"]);
    push_audio(&mut a, 0, &encode_audio(ext, &info, Some(filter)));
    a.extend(sv(&["-map_metadata", "0"]));
    a
}

/// For a filter graph, which sets its own output rate: only what follows the graph.
pub fn audio_graph_args(p: &Probe, ext: &str) -> Vec<String> {
    let info = p.audio.first().cloned().unwrap_or_default();
    let opts: Vec<_> = encode_audio(ext, &info, None).into_iter().filter(|(k, _)| *k != "filter").collect();
    let mut a = vec![];
    push_audio(&mut a, 0, &opts);
    a.extend(sv(&["-map_metadata", "0"]));
    a
}

/// Lossy audio is cut by copying whole frames, at no cost. Lossless audio re-encodes to itself, which costs nothing
/// either and lands on the exact sample.
pub fn audio_trim_args(p: &Probe, ext: &str) -> Vec<String> {
    let info = p.audio.first().cloned().unwrap_or_default();
    let mut a = sv(&["-map", "0:a:0"]);
    if info.codec.is_empty() || (!info.is_lossless() && audio_fits(ext, &info.codec)) {
        a.extend(sv(&["-c:a:0", "copy"]));
    } else {
        push_audio(&mut a, 0, &encode_audio(ext, &info, None));
    }
    a.extend(sv(&["-map_metadata", "0"]));
    a
}

/// Compress is the one tool that spends quality on purpose, and only as much as was chosen.
/// Depth, chroma and every track stay; audio is copied when it fits, since the saving is in the video.
pub fn compress_video_args(p: &Probe, crf: &str, height: Option<u32>) -> Vec<String> {
    let mut a = sv(&["-map", "0:V:0", "-c:v", "libx264", "-preset", "slow", "-crf", crf]);
    let mut chain = vec![];
    if let Some(h) = height { chain.push(format!("scale=-2:'min({h},ih)':flags=lanczos+accurate_rnd")); }
    if let Some(v) = &p.video {
        let f = x264_nearest(&v.pix_fmt);
        let (w, h) = v.display_size();
        if height.is_none() && odd_for_chroma(&f, w, h) { chain.push("pad=ceil(iw/2)*2:ceil(ih/2)*2".into()); }
        chain.push(format!("format={f}"));
    }
    if !chain.is_empty() { a.extend(["-vf".to_string(), chain.join(",")]); }
    for (i, info) in p.audio.iter().enumerate() {
        a.extend(["-map".to_string(), format!("0:a:{i}")]);
        if audio_fits("mp4", &info.codec) { a.extend([format!("-c:a:{i}"), "copy".to_string()]); }
        else { a.extend([format!("-c:a:{i}"), "aac".to_string(), format!("-b:a:{i}"), "256k".to_string()]); }
    }
    a.extend(sub_streams(p, "mp4"));
    a.extend(sv(&["-map_metadata", "0", "-map_chapters", "0"]));
    a.extend(keep_frames_as_they_are("mp4"));
    a.extend(container_flags("mp4"));
    a
}

// ---- trimming ----

/// ffprobe arguments listing the packets just after a time, for `is_keyframe_at`.
pub fn keyframe_probe_args(path: &str, t: f64) -> Vec<String> {
    // The read starts at the keyframe before `from`, however long ago that was, and runs to an absolute end.
    let (from, to) = ((t - 0.5).max(0.0), t + 0.5);
    sv(&["-v", "error", "-select_streams", "v:0", "-read_intervals", &format!("{from:.3}%{to:.3}"), "-show_entries", "packet=pts_time,flags", "-of", "csv=p=0", path])
}

/// True when the frame nearest `t` starts a keyframe, so a cut there can copy the original bits.
pub fn is_keyframe_at(csv: &str, t: f64, fps: f64) -> bool {
    let half_frame = if fps > 0.0 { 0.5 / fps } else { 0.02 };
    csv.lines().filter_map(|l| {
        let (time, flags) = l.split_once(',')?;
        Some((time.parse::<f64>().ok()?, flags.to_string()))
    }).any(|(time, flags)| flags.starts_with('K') && (time - t).abs() < half_frame)
}

/// Goes before -i. After it, a stream copy drops everything up to the next keyframe, because B-frames give the
/// first packet a negative timestamp: a whole group of pictures vanishes. On the input it is also far faster
/// on long files, and still exact when re-encoding.
pub fn seek_args(a: f64, b: f64) -> Vec<String> {
    sv(&["-ss", &format!("{a:.6}"), "-to", &format!("{b:.6}")])
}

/// A cut can copy the original bits only from a keyframe to the end of the file. Ending anywhere else overshoots
/// by a packet or two, and the last frame decodes wrong because a frame it depends on was left behind.
pub fn trim_can_copy(starts_on_keyframe: bool, b: f64, duration: f64, fps: f64) -> bool {
    let half_frame = if fps > 0.0 { 0.5 / fps } else { 0.02 };
    starts_on_keyframe && duration > 0.0 && b >= duration - half_frame
}

/// The tail of a cut from a keyframe, with the original video and audio bits untouched.
pub fn trim_copy_tail() -> Vec<String> { sv(&["-avoid_negative_ts", "make_zero"]) }

// ---- joining ----

/// True when the clips could have come out of one encode, so joining them needs no re-encoding at all.
pub fn can_concat_copy(probes: &[Probe], exts: &[String]) -> bool {
    let Some(first) = probes.first() else { return false };
    let Some(v) = &first.video else { return false };
    if v.extradata.is_empty() && matches!(v.codec.as_str(), "h264" | "hevc") { return false; }
    exts.iter().all(|e| *e == exts[0]) && probes.iter().all(|p| {
        p.video.as_ref().is_some_and(|o| o == v) && p.audio == first.audio && p.subs.is_empty()
    })
}

/// Text of a concat demuxer list, with quotes in paths escaped.
pub fn concat_list(paths: &[String]) -> String {
    paths.iter().map(|p| format!("file '{}'\n", p.replace('\\', "/").replace('\'', "'\\''"))).collect()
}

pub struct JoinSpec { pub size: (u32, u32), pub fps: f64, pub pix: String, pub rate: u32, pub layout: &'static str }

/// The output format that loses nothing from any clip: largest frame, fastest rate, deepest pixels.
pub fn join_spec(probes: &[Probe]) -> JoinSpec {
    let (mut w, mut h, mut fps) = (2u32, 2u32, 0f64);
    let (mut deep, mut c444, mut c422) = (false, false, false);
    let (mut rate, mut ch) = (0u32, 0u32);
    for p in probes {
        if let Some(v) = &p.video {
            let (vw, vh) = v.display_size();
            w = w.max(vw);
            h = h.max(vh);
            fps = fps.max(v.fps);
            let f = x264_nearest(&v.pix_fmt);
            deep |= depth_of(&f) > 8;
            c444 |= f.contains("444");
            c422 |= f.contains("422");
        }
        for a in &p.audio { rate = rate.max(a.rate); ch = ch.max(a.channels); }
    }
    let chroma = if c444 { "444" } else if c422 { "422" } else { "420" };
    JoinSpec {
        size: (w & !1, h & !1),
        fps: if fps > 0.0 { fps } else { 30.0 },
        pix: format!("yuv{chroma}p{}", if deep { "10le" } else { "" }),
        rate: if rate > 0 { rate } else { 48000 },
        layout: if ch > 2 { "5.1" } else { "stereo" },
    }
}

/// `silent` holds, per clip, the length of the silence it needs when it has no sound of its own.
pub fn join_filter(s: &JoinSpec, silent: &[Option<f64>]) -> String {
    let (w, h) = s.size;
    let (fps, pix, rate, layout) = (s.fps, &s.pix, s.rate, s.layout);
    let mut f = String::new();
    let mut tail = String::new();
    for (i, a) in silent.iter().enumerate() {
        // Upscale only what is smaller than the largest clip; a clip already at this size passes through untouched.
        f.push_str(&format!("[{i}:v]scale={w}:{h}:force_original_aspect_ratio=decrease:flags=lanczos+accurate_rnd,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps={fps:.6},format={pix}[v{i}];"));
        match a {
            // A clip with no sound still needs a silent track of its own length, or concat fails.
            Some(d) => f.push_str(&format!("anullsrc=r={rate}:cl={layout},atrim=duration={d:.3}[a{i}];")),
            None => f.push_str(&format!("[{i}:a]aresample={rate}:filter_size=128:phase_shift=14,aformat=sample_rates={rate}:channel_layouts={layout}[a{i}];")),
        }
        tail.push_str(&format!("[v{i}][a{i}]"));
    }
    format!("{f}{tail}concat=n={}:v=1:a=1[v][a]", silent.len())
}

/// Lossless video and the best audio for a join's output. Returns (ext, args after the filter graph).
pub fn join_output_args(s: &JoinSpec, probes: &[Probe]) -> (String, Vec<String>) {
    let (ext, enc) = lossless_video(&s.pix, s.size.0, s.size.1, "mp4");
    let lossless_audio = probes.iter().flat_map(|p| &p.audio).all(AudioInfo::is_lossless);
    let info = AudioInfo { codec: if lossless_audio { "flac".into() } else { "aac".into() }, channels: if s.layout == "stereo" { 2 } else { 6 }, rate: s.rate, ..Default::default() };
    let mut a = sv(&["-map", "[v]", "-map", "[a]"]);
    a.extend(enc);
    push_audio(&mut a, 0, &encode_audio(&ext, &info, None));
    a.extend(keep_frames_as_they_are(&ext));
    a.extend(container_flags(&ext));
    (ext, a)
}

// ---- images ----

/// Where a redacted picture goes. Lossy formats cannot be re-saved without changing every pixel, so they become PNG.
pub fn lossless_image_ext(ext: &str) -> &'static str {
    match ext { "tif" => "tif", "tiff" => "tiff", "bmp" => "bmp", _ => "png" }
}

/// ffmpeg arguments for saving an edited picture at the best its format allows.
pub fn image_encode_args(ext: &str) -> Vec<String> {
    let mut a = match ext {
        "jpg" | "jpeg" => sv(&["-c:v", "mjpeg", "-q:v", "1", "-qmin", "1", "-huffman", "optimal", "-pix_fmt", "yuvj444p"]),
        "webp" => sv(&["-c:v", "libwebp", "-lossless", "1", "-compression_level", "6"]),
        "avif" => avif_lossless_args(),
        _ => vec![],
    };
    a.extend(sv(&["-sws_flags", "lanczos+accurate_rnd+full_chroma_int"]));
    a
}

/// AVIF only stays lossless with RGB samples stored directly, since converting to YUV rounds.
pub fn avif_lossless_args() -> Vec<String> {
    sv(&["-c:v", "libaom-av1", "-still-picture", "1", "-crf", "0", "-lossless", "1", "-pix_fmt", "gbrp", "-cpu-used", "4"])
}

#[derive(Debug, PartialEq)]
pub struct JpegInfo { pub w: u32, pub h: u32, pub comps: u8, pub bits: u8 }

/// Reads the frame header, without decoding anything.
pub fn jpeg_info(b: &[u8]) -> Option<JpegInfo> {
    if b.get(..2)? != [0xFF, 0xD8] { return None; }
    let mut i = 2;
    loop {
        if *b.get(i)? != 0xFF { return None; }
        let m = *b.get(i + 1)?;
        if m == 0xFF { i += 1; continue; }
        if matches!(m, 0xD8 | 0x01 | 0xD0..=0xD7) { i += 2; continue; }
        if matches!(m, 0xD9 | 0xDA) { return None; }
        let len = u16::from_be_bytes([*b.get(i + 2)?, *b.get(i + 3)?]) as usize;
        if matches!(m, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
            let s = b.get(i + 4..i + 2 + len)?;
            return Some(JpegInfo { bits: *s.first()?, h: u16::from_be_bytes([*s.get(1)?, *s.get(2)?]) as u32, w: u16::from_be_bytes([*s.get(3)?, *s.get(4)?]) as u32, comps: *s.get(5)? });
        }
        i += 2 + len;
    }
}

/// The PDF colour space a JPEG can be embedded under byte for byte. CMYK needs more than a name, so it is left out.
pub fn pdf_color_space(i: &JpegInfo) -> Option<&'static str> {
    if i.bits != 8 { return None; }
    match i.comps { 1 => Some("DeviceGray"), 3 => Some("DeviceRGB"), _ => None }
}

/// Collage tile: big enough that nothing is shrunk, unless the sheet would pass what memory can sensibly hold.
pub fn collage_tile(sizes: &[(u32, u32)], cols: u32, rows: u32, gap: u32) -> (u32, u32) {
    const MAX_PIXELS: f64 = 500_000_000.0;
    let (tw, th) = sizes.iter().fold((1, 1), |(a, b), (w, h)| (a.max(*w), b.max(*h)));
    let total = (cols * tw + (cols + 1) * gap) as f64 * (rows * th + (rows + 1) * gap) as f64;
    if total <= MAX_PIXELS { return (tw, th); }
    let f = (MAX_PIXELS / total).sqrt();
    (((tw as f64 * f) as u32).max(1), ((th as f64 * f) as u32).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLAT: &str = r#"streams.stream.0.codec_name="hevc"
streams.stream.0.codec_type="video"
streams.stream.0.width=3840
streams.stream.0.height=2160
streams.stream.0.pix_fmt="yuv420p10le"
streams.stream.0.color_range="tv"
streams.stream.0.avg_frame_rate="30000/1001"
streams.stream.0.extradata_hash="md5:abc"
streams.stream.0.disposition.attached_pic=0
streams.stream.0.side_data_list.side_data.0.rotation=-90
streams.stream.1.codec_name="aac"
streams.stream.1.codec_type="audio"
streams.stream.1.sample_fmt="fltp"
streams.stream.1.sample_rate="48000"
streams.stream.1.channels=2
streams.stream.2.codec_name="mjpeg"
streams.stream.2.codec_type="video"
streams.stream.2.disposition.attached_pic=1
streams.stream.3.codec_name="subrip"
streams.stream.3.codec_type="subtitle"
"#;

    #[test]
    fn probe_reads_every_kind_of_stream() {
        let p = parse_probe(FLAT);
        let v = p.video.unwrap();
        assert_eq!((v.codec.as_str(), v.pix_fmt.as_str(), v.rotation), ("hevc", "yuv420p10le", -90));
        assert_eq!(v.display_size(), (2160, 3840));
        assert!((v.fps - 29.97).abs() < 0.01);
        assert!(p.cover);
        assert_eq!(p.audio[0].rate, 48000);
        assert_eq!(p.subs, vec!["subrip"]);
    }
    #[test]
    fn depth_is_read_from_the_format_name() {
        for (f, d) in [("yuv420p", 8), ("yuv420p10le", 10), ("yuv444p12le", 12), ("gbrp", 8), ("gbrap16le", 16), ("gray", 8), ("gray10le", 10), ("yuvj420p", 8), ("yuv422p", 8)] {
            assert_eq!(depth_of(f), d, "{f}");
        }
    }
    #[test]
    fn deeper_sources_never_lose_depth_in_x264() {
        assert_eq!(x264_nearest("yuv420p10le"), "yuv420p10le");
        assert_eq!(x264_nearest("yuv420p12le"), "yuv420p10le");
        assert_eq!(x264_nearest("yuv444p"), "yuv444p");
        assert_eq!(x264_nearest("bgra"), "yuv444p");
        assert_eq!(x264_nearest("pal8"), "yuv444p");
        assert_eq!(x264_nearest("p010le"), "yuv420p10le");
    }
    #[test]
    fn every_target_has_arguments() {
        let p = parse_probe(FLAT);
        for t in ["mp4", "mov", "mkv", "webm", "avi", "wmv", "gif", "mp3", "m4a", "wav", "flac", "ogg", "opus", "aiff", "wma"] {
            let a = convert_attempts(&p, t);
            assert!(!a.is_empty() && a.iter().all(|x| !x.is_empty()), "{t}");
        }
    }
    #[test]
    fn matching_codecs_are_copied_first() {
        let p = parse_probe(FLAT);
        let first = convert_attempts(&p, "mkv").remove(0).join(" ");
        assert!(first.contains("-c:v copy") && first.contains("-c:a:0 copy"), "{first}");
        let m4a = convert_attempts(&p, "m4a").remove(0).join(" ");
        assert!(m4a.contains("-c:a:0 copy"), "{m4a}");
    }
    #[test]
    fn gif_keeps_size_and_frame_rate_up_to_the_format_limit() {
        let mut p = parse_probe(FLAT);
        let a = convert_attempts(&p, "gif").remove(0).join(" ");
        assert!(!a.contains("fps=") && !a.contains("scale"), "{a}");
        p.video.as_mut().unwrap().fps = 60.0;
        assert!(convert_attempts(&p, "gif").remove(0).join(" ").contains("fps=50"));
    }
    #[test]
    fn opus_bitrate_respects_the_per_channel_limit() {
        let a = AudioInfo { codec: "flac".into(), sample_fmt: "s32".into(), bits: 24, rate: 96000, channels: 1 };
        let o = encode_audio("opus", &a, None);
        assert!(o.contains(&("b", "256k".to_string())) && o.contains(&("ar", "48000".to_string())));
    }
    #[test]
    fn pcm_keeps_the_source_depth() {
        let a = |f: &str, b| AudioInfo { sample_fmt: f.into(), bits: b, ..Default::default() };
        assert_eq!(pcm_codec(&a("s16", 16), false), "pcm_s16le");
        assert_eq!(pcm_codec(&a("s32", 24), false), "pcm_s24le");
        assert_eq!(pcm_codec(&a("s32", 32), true), "pcm_s32be");
        assert_eq!(pcm_codec(&a("fltp", 0), false), "pcm_f32le");
    }
    #[test]
    fn lossless_needs_ffv1_when_h264_cannot_hold_the_pixels() {
        assert_eq!(lossless_video("yuv420p12le", 640, 360, "mp4").0, "mkv");
        assert_eq!(lossless_video("yuv420p", 641, 360, "mp4").0, "mkv");
        assert_eq!(lossless_video("yuv444p", 641, 361, "mp4").0, "mp4");
        assert_eq!(lossless_video("yuv420p", 640, 360, "avi").0, "mkv");
        assert_eq!(lossless_video("yuv420p", 640, 360, "wmv").0, "mkv");
        assert_eq!(lossless_video("yuv420p", 640, 360, "webm").0, "webm");
    }
    #[test]
    fn redaction_does_not_carry_cover_art_over() {
        let l = lossless_args(&parse_probe(FLAT), "mp4", Some("null"), None, false).unwrap();
        let a = l.args.join(" ");
        assert!(a.contains("-map 0:V:0") && !a.contains("-map 0 "), "{a}");
    }
    #[test]
    fn redaction_does_not_carry_attachments_over() {
        let p = parse_probe(FLAT);
        let a = redact_args(&p, "mkv", "null").unwrap().args.join(" ");
        assert!(!a.contains("0:t"), "{a}");
        assert!(lossless_args(&p, "mkv", Some("null"), None, false).unwrap().args.join(" ").contains("0:t"));
    }
    #[test]
    fn redact_box_only_grows_and_stays_inside() {
        assert_eq!(redact_box(101, 61, 10, 10, 640, 360), (100, 60, 12, 12));
        assert_eq!(redact_box(100, 60, 10, 10, 640, 360), (100, 60, 10, 10));
        assert_eq!(redact_box(639, 359, 1, 1, 640, 360), (638, 358, 2, 2));
        let (x, _, w, _) = redact_box(637, 0, 3, 3, 639, 360);
        assert_eq!(x + w, 639);
    }
    #[test]
    fn redact_black_matches_depth_and_range() {
        let f = redact_filter("yuv420p10le", false, &[(0, 0, 2, 2)]).unwrap();
        assert!(f.contains(",64,") && f.contains(",512,"), "{f}");
        let f = redact_filter("yuvj420p", true, &[(0, 0, 2, 2)]).unwrap();
        assert!(f.contains(",0,p(") && f.contains(",128,"), "{f}");
    }
    #[test]
    fn keyframe_check_uses_half_a_frame() {
        let csv = "0.000000,K__\n0.033367,___\n2.002000,K__\n";
        assert!(is_keyframe_at(csv, 0.0, 29.97));
        assert!(is_keyframe_at(csv, 2.0, 29.97));
        assert!(!is_keyframe_at(csv, 1.0, 29.97));
        assert!(!is_keyframe_at(csv, 0.033, 29.97));
    }
    #[test]
    fn copying_a_cut_needs_a_keyframe_and_the_end_of_the_file() {
        assert!(trim_can_copy(true, 10.0, 10.0, 30.0));
        assert!(trim_can_copy(true, 9.99, 10.0, 30.0));
        assert!(!trim_can_copy(true, 8.0, 10.0, 30.0));
        assert!(!trim_can_copy(false, 10.0, 10.0, 30.0));
        assert!(!trim_can_copy(true, 10.0, 0.0, 30.0));
    }
    #[test]
    fn concat_copy_needs_identical_streams() {
        let p = parse_probe(FLAT);
        let e = |s: &str| vec![s.to_string(), s.to_string()];
        let mut plain = p.clone();
        plain.subs.clear();
        assert!(can_concat_copy(&[plain.clone(), plain.clone()], &e("mp4")));
        let mut other = plain.clone();
        other.video.as_mut().unwrap().extradata = "md5:zzz".into();
        assert!(!can_concat_copy(&[plain.clone(), other], &e("mp4")));
        assert!(!can_concat_copy(&[plain.clone(), plain], &["mp4".to_string(), "mov".to_string()]));
    }
    #[test]
    fn join_keeps_the_richest_of_each_property() {
        let mut a = parse_probe(FLAT);
        a.video.as_mut().unwrap().rotation = 0;
        let mut b = a.clone();
        b.video.as_mut().unwrap().pix_fmt = "yuv444p".into();
        b.video.as_mut().unwrap().fps = 60.0;
        let s = join_spec(&[a, b]);
        assert_eq!((s.size, s.pix.as_str(), s.fps), ((3840, 2160), "yuv444p10le", 60.0));
    }
    #[test]
    fn join_fills_silence_for_clips_without_sound() {
        let s = JoinSpec { size: (1280, 720), fps: 30.0, pix: "yuv420p".into(), rate: 48000, layout: "stereo" };
        let f = join_filter(&s, &[None, Some(2.5)]);
        assert!(f.contains("[0:a]aresample") && f.contains("anullsrc") && f.contains("duration=2.500") && f.ends_with("concat=n=2:v=1:a=1[v][a]"));
    }
    #[test]
    fn jpeg_header_is_read_without_decoding() {
        let mut j = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 1, 2, 0xFF, 0xC2, 0, 11, 8];
        j.extend_from_slice(&[0x01, 0x02, 0x03, 0x04, 3, 0, 0, 0]);
        let i = jpeg_info(&j).unwrap();
        assert_eq!(i, JpegInfo { w: 0x0304, h: 0x0102, comps: 3, bits: 8 });
        assert_eq!(pdf_color_space(&i), Some("DeviceRGB"));
        assert!(jpeg_info(b"not a jpeg").is_none());
        assert_eq!(pdf_color_space(&JpegInfo { w: 1, h: 1, comps: 4, bits: 8 }), None);
    }
    #[test]
    fn lossy_images_redact_to_png() {
        for e in ["jpg", "jpeg", "heic", "webp", "avif", "png"] { assert_eq!(lossless_image_ext(e), "png"); }
        assert_eq!(lossless_image_ext("tiff"), "tiff");
        assert_eq!(lossless_image_ext("bmp"), "bmp");
    }
    #[test]
    fn collage_keeps_full_size_until_the_sheet_is_enormous() {
        assert_eq!(collage_tile(&[(100, 50), (50, 100)], 2, 1, 10), (100, 100));
        let (w, h) = collage_tile(&[(12000, 9000)], 5, 5, 0);
        assert!((w * 5) as f64 * (h * 5) as f64 <= 500_000_000.0 && w < 12000);
    }
    #[test]
    fn concat_list_escapes_quotes() {
        assert_eq!(concat_list(&[r"C:\a'b.mp4".to_string()]), "file 'C:/a'\\''b.mp4'\n");
    }
}
