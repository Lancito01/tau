//! Every decision about how much quality survives an operation. Pure logic with no Win32, so
//! `tests/fidelity.rs` can put these exact arguments through a real ffmpeg on any OS.
//!
//! The order of preference, everywhere:
//!   1. Copy the original bits (stream copy, remux, embedding a JPEG as is).
//!   2. Lossless encode, when the operation keeps the file's format: redact, crop, trim, split, speed, join.
//!      A conversion gets this too when the source itself is lossless and the target can hold it.
//!   3. A lossy source going to a new codec: very high quality, capped at a bitrate matched to the source. A 5 Mbps MP4
//!      has 5 Mbps of detail, so a lossless copy of it would be many times the size and show nothing more.
//!   4. The highest setting of the target codec, when the format is lossy by nature.
//! Compress is the only tool that gives quality away, because that is what the user asked it to do.
//!
//! Never trust a filter or an encoder to keep the pixel format by itself. ffmpeg converts silently when
//! one does not support it (drawbox turns 10-bit into 8-bit without a word), so formats are set explicitly.
use crate::redact::{Fill, Mark};
use std::collections::BTreeMap;

/// x264 at this CRF stays in the High profile that every player decodes, unlike true lossless.
/// With a lossy source the bitrate cap is what decides, and this only stops it spending bits it does not need.
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
    pub color_space: String,
    /// Bits per second of this stream alone, 0 when the file does not say.
    pub bitrate: u64,
    /// Camcorder footage often stores two half-pictures per frame.
    pub interlaced: bool,
    /// Width of a pixel over its height, 0 when the file does not say. DV and DVD pixels are not square.
    pub pixel_aspect: f64,
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
    /// Bits per second, 0 when the file does not say.
    pub bitrate: u64,
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
    /// Seconds, 0 when the file does not say.
    pub duration: f64,
}

/// ffprobe arguments whose output `parse_probe` reads.
pub fn probe_args(path: &str) -> Vec<String> {
    sv(&["-v", "error", "-show_data_hash", "md5", "-show_entries",
        "stream=codec_type,codec_name,pix_fmt,width,height,avg_frame_rate,r_frame_rate,color_range,sample_fmt,bits_per_sample,bits_per_raw_sample,sample_rate,channels,extradata_hash,bit_rate,color_space,field_order,sample_aspect_ratio:stream_disposition=attached_pic:stream_side_data=rotation:format=bit_rate,duration",
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
    let mut file_bitrate = 0u64;
    let mut duration = 0.0;
    for line in flat.lines() {
        if let Some(v) = line.strip_prefix("format.bit_rate=") { file_bitrate = v.trim_matches('"').parse().unwrap_or(0); }
        if let Some(v) = line.strip_prefix("format.duration=") { duration = v.trim_matches('"').parse().unwrap_or(0.0); }
        let Some(rest) = line.strip_prefix("streams.stream.") else { continue };
        let Some((n, kv)) = rest.split_once('.') else { continue };
        let Some((k, v)) = kv.split_once('=') else { continue };
        let Ok(n) = n.parse() else { continue };
        streams.entry(n).or_default().insert(k.to_string(), v.trim().trim_matches('"').to_string());
    }
    let mut p = Probe { duration, ..Default::default() };
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
                    color_space: get("color_space").into(),
                    bitrate: get("bit_rate").parse().unwrap_or(0),
                    interlaced: matches!(get("field_order"), "tt" | "bb" | "tb" | "bt"),
                    pixel_aspect: parse_rate(&get("sample_aspect_ratio").replace(':', "/")),
                });
            }
            "audio" => p.audio.push(AudioInfo {
                codec: get("codec_name").into(),
                sample_fmt: get("sample_fmt").into(),
                bits: num("bits_per_raw_sample").max(num("bits_per_sample")),
                rate: num("sample_rate"),
                channels: num("channels"),
                bitrate: get("bit_rate").parse().unwrap_or(0),
            }),
            "subtitle" => p.subs.push(get("codec_name").into()),
            _ => {}
        }
    }
    // MKV and WebM usually only give a total, so the video gets what the known audio leaves.
    if let Some(v) = p.video.as_mut() {
        if v.bitrate == 0 { v.bitrate = file_bitrate.saturating_sub(p.audio.iter().map(|a| a.bitrate).sum()); }
    }
    if p.video.is_none() && p.audio.len() == 1 && p.audio[0].bitrate == 0 { p.audio[0].bitrate = file_bitrate; }
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

// ---- what the source is worth ----

/// True for codecs that store every pixel exactly, so a lossless target keeps something real.
pub fn video_is_lossless(codec: &str) -> bool {
    matches!(codec, "ffv1" | "huffyuv" | "ffvhuff" | "utvideo" | "rawvideo" | "png" | "qtrle" | "lagarith" | "magicyuv" | "v210" | "v410")
}

/// Rough bits each codec needs for the same picture, relative to H.264.
fn video_cost(codec: &str) -> f64 {
    match codec {
        "av1" => 0.5,
        "hevc" => 0.6,
        "vp9" => 0.65,
        "h264" => 1.0,
        "vp8" | "vc1" | "wmv3" => 1.1,
        "mpeg4" | "msmpeg4v3" | "msmpeg4v2" | "wmv1" | "wmv2" => 1.5,
        "mpeg2video" | "mpeg1video" => 2.0,
        // Intra-only, so most of their bitrate pays for what the others get from neighbouring frames.
        "mjpeg" | "prores" | "dvvideo" | "dnxhd" | "gif" => 3.0,
        _ => 1.0,
    }
}

/// The kbps that holds what the source holds, in this encoder. None when the file gives no bitrate to go on.
fn video_budget(v: &VideoInfo, encoder_cost: f64) -> Option<u32> {
    if v.bitrate == 0 { return None; }
    Some(((v.bitrate as f64 / 1000.0 * encoder_cost / video_cost(&v.codec)) as u32).max(100))
}

/// The same idea for lossy audio.
fn audio_cost(codec: &str) -> f64 {
    match codec {
        "opus" => 0.65,
        "aac" | "vorbis" | "wmapro" => 0.8,
        "mp3" | "wmav1" | "wmav2" => 1.0,
        "ac3" | "eac3" => 1.2,
        "dts" | "mp2" => 1.5,
        _ => 1.0,
    }
}

/// The lower of the format's top bitrate and what holds everything in the source. Lossless sources get the top.
fn audio_kbps(a: &AudioInfo, codec: &str, top: u32, match_source: bool) -> u32 {
    if !match_source || a.is_lossless() || a.bitrate == 0 { return top; }
    ((a.bitrate as f64 / 1000.0 * audio_cost(codec) / audio_cost(&a.codec)) as u32).clamp(32, top)
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
        "ogg" => matches!(codec, "vorbis" | "opus"),
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
    // An edit keeps the format and has already lost a generation, so it stays at the top. A conversion matches the source.
    let m = extra.is_none();
    let mut o: Vec<(&'static str, String)> = vec![];
    let mut cap = 0u32;
    match target {
        "mp3" | "avi" => {
            o.extend([("c", "libmp3lame".to_string()), ("b", format!("{}k", audio_kbps(a, "mp3", 320, m))), ("compression_level", "0".to_string())]);
            cap = 48000;
            if a.channels > 2 { o.push(("ac", "2".into())); }
        }
        "wma" | "wmv" => {
            o.extend([("c", "wmav2".to_string()), ("b", format!("{}k", audio_kbps(a, "wmav2", 320, m)))]);
            cap = 48000;
            if a.channels > 2 { o.push(("ac", "2".into())); }
        }
        "m4a" | "mp4" | "m4v" | "mov" if a.is_lossless() => o.push(("c", "alac".into())),
        "m4a" | "mp4" | "m4v" | "mov" | "aac" => o.extend([("c", "aac".to_string()), ("b", format!("{}k", audio_kbps(a, "aac", 160 * ch, m)))]),
        "mkv" | "flac" => o.extend([("c", "flac".to_string()), ("compression_level", "8".to_string())]),
        // Quality 10 is its top, but a set bitrate is what can follow the source.
        "ogg" if m && !a.is_lossless() && a.bitrate > 0 => o.extend([("c", "libvorbis".to_string()), ("b", format!("{}k", audio_kbps(a, "vorbis", 500, m)))]),
        "ogg" => o.extend([("c", "libvorbis".to_string()), ("q", "10".to_string())]),
        "opus" | "webm" => {
            // libopus refuses more than 256 kbps per channel.
            o.extend([("c", "libopus".to_string()), ("b", format!("{}k", audio_kbps(a, "opus", 256 * a.channels.max(1), m)))]);
            cap = 48000;
            if a.channels > 2 { o.push(("mapping_family", "1".into())); }
        }
        "wav" => o.push(("c", pcm_codec(a, false))),
        "aiff" | "aif" => o.push(("c", pcm_codec(a, true))),
        _ => o.extend([("c", "aac".to_string()), ("b", format!("{}k", audio_kbps(a, "aac", 160 * ch, m)))]),
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

/// The palette stage of every GIF this app writes. Each frame's palette comes from that frame alone and is used on it
/// alone, so a frame comes out the same whichever frames are encoded with it.
const GIF_PALETTE: &str = "split[a][b];[a]palettegen=stats_mode=single[p];[b][p]paletteuse=new=1";

/// The arguments for one part of a GIF encoded in parts, or None when they do not make each frame on its own.
/// The part holds the frames from `from` up to `to` seconds, plus the first frame after `to`, which is only there so the
/// delay of the frame before it is measured to the true next frame, as in one pass; `gif::splice` drops it.
/// The selection sits just before the palette stage, so every filter ahead of it still sees every frame: a
/// deinterlacer or a frame rate cap works across the cut exactly as in one pass.
pub fn gif_part_args(args: &[String], from: Option<f64>, to: Option<f64>) -> Option<Vec<String>> {
    let i = args.iter().position(|a| a == "-vf")? + 1;
    let vf = args.get(i)?;
    // A rectangle diff makes a frame depend on the one before it.
    if vf.matches(GIF_PALETTE).count() != 1 || vf.contains("diff_mode=rectangle") { return None; }
    let after = |b: f64| format!("gte(t,{b:.6})*lt(prev_t,{b:.6})");
    let expr = match (from, to) {
        (None, Some(b)) => format!("lt(t,{b:.6})+{}", after(b)),
        (Some(a), Some(b)) => format!("gte(t,{a:.6})*lt(t,{b:.6})+{}", after(b)),
        (Some(a), None) => format!("gte(t,{a:.6})"),
        (None, None) => return Some(args.to_vec()),
    };
    let mut out = args.to_vec();
    out[i] = vf.replacen(GIF_PALETTE, &format!("select='{expr}',{GIF_PALETTE}"), 1);
    Some(out)
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

/// RGB to this format's YUV samples, using the matrix the video is tagged with.
fn yuv_of(rgb: [u8; 3], color_space: &str, h: u32, full_range: bool, depth: u32) -> (u32, u32, u32) {
    let (kr, kb) = match color_space {
        s if s.starts_with("bt2020") => (0.2627, 0.0593),
        "bt709" => (0.2126, 0.0722),
        "bt470bg" | "smpte170m" => (0.299, 0.114),
        _ if h >= 720 => (0.2126, 0.0722),
        _ => (0.299, 0.114),
    };
    let (r, g, b) = (rgb[0] as f64 / 255.0, rgb[1] as f64 / 255.0, rgb[2] as f64 / 255.0);
    let y = kr * r + (1.0 - kr - kb) * g + kb * b;
    let (u, v) = ((b - y) / (2.0 * (1.0 - kb)), (r - y) / (2.0 * (1.0 - kr)));
    let top = ((1u32 << depth) - 1) as f64;
    let step = (1u32 << (depth - 8)) as f64;
    if full_range {
        ((y * top).round() as u32, ((u + 0.5) * top).round() as u32, ((v + 0.5) * top).round() as u32)
    } else {
        (((16.0 + 219.0 * y) * step).round() as u32, ((128.0 + 224.0 * u) * step).round() as u32, ((128.0 + 224.0 * v) * step).round() as u32)
    }
}

/// How much of a mark covers a sample, from 0 to `top`. The same shape as `Mark::coverage`, written for geq.
fn coverage_expr(m: &Mark, top: u32) -> String {
    let (xx, yy) = ("(X/SW+0.5)", "(Y/SH+0.5)");
    let (x0, y0, x1, y1) = (m.x as f64, m.y as f64, (m.x + m.w) as f64, (m.y + m.h) as f64);
    let boxed = format!("hypot(max(max({x0}-{xx},{xx}-{x1}),0),max(max({y0}-{yy},{yy}-{y1}),0))");
    let d = if m.ellipse {
        let (rx, ry) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
        let (dx, dy) = (format!("({xx}-{})", x0 + rx), format!("({yy}-{})", y0 + ry));
        let k = format!("(pow({dx}/{rx},2)+pow({dy}/{ry},2))");
        // Inside when k <= 1. Otherwise first-order distance to the curve, never less than to the bounding box.
        format!("if(lte({k},1),0,max(({k}-1)/(2*hypot({dx}/{},{dy}/{})),{boxed}))", rx * rx, ry * ry)
    } else {
        boxed
    };
    if m.feather == 0 { return format!("if(lte({d},0),{top},0)"); }
    let f = m.feather;
    format!("if(lte({d},0),{top},if(gte({d},{f}),0,{top}*pow(1-({d})/{f},2)*(3-2*(1-({d})/{f}))))")
}

/// geq arguments giving each plane of this format a value.
fn planes(planar: &str, luma_or_all: &str, cb: &str, cr: &str) -> String {
    let alpha = if has_alpha(planar) { ":a='0'" } else { "" };
    if is_rgb(planar) { format!("geq=r='{luma_or_all}':g='{cb}':b='{cr}'{alpha}") }
    else if planar.starts_with("gray") { format!("geq=lum='{luma_or_all}'") }
    else { format!("geq=lum='{luma_or_all}':cb='{cb}':cr='{cr}'{alpha}") }
}

/// Applies the marks in the source's own format, for `format=` to precede. Each mark merges a layer through a mask
/// holding its coverage. A zero in the mask leaves a sample exactly as it was, at any depth, and the mask and solid
/// layers are one frame each, repeated, so a long video does not pay to compute them again.
pub fn redact_graph(pix: &str, color_space: &str, full_range: bool, w: u32, h: u32, marks: &[Mark]) -> Option<String> {
    let planar = planar_of(pix)?;
    let d = depth_of(&planar);
    let top = (1u32 << d) - 1;
    let frame = format!("color=black:s={w}x{h}:r=1:d=1,format={planar},");
    let mut g = String::from("null[c0];");
    for (i, m) in marks.iter().enumerate() {
        let n = i + 1;
        let layer = match m.fill {
            Fill::Blur(sigma) => { g.push_str(&format!("[c{i}]split[s{i}][t{i}];[t{i}]gblur=sigma={:.3}:steps=3[l{i}];", sigma.max(0.5))); format!("[s{i}]") }
            Fill::Solid(rgb) => {
                let vals = if is_rgb(&planar) {
                    // gbr planes in geq's r, g and b order, scaled to depth.
                    let s = |c: u8| ((c as f64 / 255.0) * top as f64).round() as u32;
                    (s(rgb[0]), s(rgb[1]), s(rgb[2]))
                } else if planar.starts_with("gray") {
                    let y = 0.299 * rgb[0] as f64 + 0.587 * rgb[1] as f64 + 0.114 * rgb[2] as f64;
                    (if full_range { (y / 255.0 * top as f64).round() as u32 } else { ((16.0 + y / 255.0 * 219.0) * (1u32 << (d - 8)) as f64).round() as u32 }, 0, 0)
                } else {
                    yuv_of(rgb, color_space, h, full_range, d)
                };
                g.push_str(&format!("{frame}{}[l{i}];", planes(&planar, &vals.0.to_string(), &vals.1.to_string(), &vals.2.to_string())));
                format!("[c{i}]")
            }
        };
        let c = coverage_expr(m, top);
        g.push_str(&format!("{frame}{}[m{i}];", planes(&planar, &c, &c, &c)));
        g.push_str(&format!("{layer}[l{i}][m{i}]maskedmerge"));
        if n < marks.len() { g.push_str(&format!("[c{n}];")); }
    }
    Some(g)
}

// ---- conversion, where the target format decides what is possible ----

fn video_fits(container: &str, codec: &str) -> bool {
    match container {
        // MPEG-2 fits both on paper, but next to nothing plays it from there.
        "mp4" | "m4v" => matches!(codec, "h264" | "hevc" | "av1" | "mpeg4" | "vp9" | "mjpeg"),
        "mov" => matches!(codec, "h264" | "hevc" | "prores" | "mjpeg" | "mpeg4" | "dvvideo" | "qtrle" | "png"),
        "mkv" => !matches!(codec, "" | "gif"),
        "webm" => matches!(codec, "vp8" | "vp9" | "av1"),
        "avi" => matches!(codec, "mpeg4" | "msmpeg4v3" | "mjpeg" | "ffv1" | "huffyuv"),
        "wmv" => matches!(codec, "wmv1" | "wmv2" | "wmv3" | "vc1"),
        _ => false,
    }
}

/// x264's cap overshoots by about a quarter on detailed video, so it is set under the budget it must hold.
fn x264_cap(kbps: u32) -> Vec<String> {
    let cap = kbps * 4 / 5;
    vec!["-maxrate".into(), format!("{cap}k"), "-bufsize".into(), format!("{}k", cap * 2)]
}

/// Encoder arguments for a video target, chosen by what the source is worth: lossless when it is lossless and the format
/// can hold that, otherwise capped to match it. Keeps depth and chroma, and pads rather than resamples a frame whose odd
/// size a codec cannot store: a scale filter would soften every pixel to add one row.
fn encode_video(target: &str, v: Option<&VideoInfo>) -> Vec<String> {
    let pix = v.map(|i| i.pix_fmt.as_str()).unwrap_or("");
    let lossless_source = v.is_some_and(|i| video_is_lossless(&i.codec));
    let mut a: Vec<String> = vec![];
    let fmt = match target {
        "mp4" | "m4v" | "mov" | "mkv" => {
            if lossless_source {
                a.extend(sv(&["-c:v", "libx264", "-qp", "0", "-preset", "medium"]));
            } else {
                a.extend(sv(&["-c:v", "libx264", "-preset", "slow", "-crf", CONVERT_X264_CRF]));
                if let Some(k) = v.and_then(|i| video_budget(i, 1.0)) { a.extend(x264_cap(k)); }
            }
            (!pix.is_empty()).then(|| x264_nearest(pix))
        }
        "webm" => {
            if lossless_source {
                a.extend(sv(&["-c:v", "libvpx-vp9", "-lossless", "1", "-row-mt", "1", "-deadline", "good", "-cpu-used", "2"]));
            } else {
                // A VP9 target bitrate lands well under what it is given, so this counts as H.264's budget, not its own.
                let k = v.and_then(|i| video_budget(i, 1.0)).map(|k| format!("{k}k")).unwrap_or_else(|| "0".to_string());
                a.extend(sv(&["-c:v", "libvpx-vp9", "-b:v", &k, "-crf", CONVERT_VP9_CRF, "-qmin", "0", "-qmax", "20", "-deadline", "good", "-cpu-used", "2", "-row-mt", "1"]));
            }
            (!pix.is_empty()).then(|| vp9_nearest(pix))
        }
        // AVI can carry FFV1, so a lossless source stays lossless. These older codecs otherwise stop at 8-bit 4:2:0.
        "avi" if lossless_source => {
            a.extend(sv(&["-c:v", "ffv1", "-level", "3", "-g", "1", "-slices", "16", "-slicecrc", "1"]));
            return a;
        }
        "avi" | "wmv" => {
            let (codec, cost) = if target == "avi" { ("mpeg4", 1.5) } else { ("wmv2", 1.6) };
            a.extend(sv(&["-c:v", codec, "-mbd", "rd", "-qmin", "1"]));
            match v.filter(|i| !lossless_source && i.bitrate > 0).and_then(|i| video_budget(i, cost)) {
                Some(k) => a.extend(["-b:v".to_string(), format!("{k}k"), "-maxrate".to_string(), format!("{k}k"), "-bufsize".to_string(), format!("{}k", k * 2)]),
                None => a.extend(sv(&["-q:v", "1"])),
            }
            Some("yuv420p".to_string())
        }
        _ => None,
    };
    let needs_pad = match (&fmt, v) {
        (Some(f), Some(i)) => { let (w, h) = i.display_size(); odd_for_chroma(f, w, h) }
        (Some(f), None) => f == "yuv420p",
        // Nothing known about the source, so be safe. Padding an even frame changes nothing.
        (None, _) => true,
    };
    let mut chain = vec![];
    // One picture per field, which keeps the motion that interlacing was there to carry. The new formats have no use
    // for fields, and would otherwise show them as combing.
    if v.is_some_and(|i| i.interlaced) { chain.push("bwdif=mode=send_field:deint=interlaced".to_string()); }
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
    let still = v.and_then(still_filter).map(|f| format!("{f},")).unwrap_or_default();
    let lead = if cap { format!("{still}fps={GIF_MAX_FPS},") } else { still };
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

// ---- telling the user what a format cannot do ----

fn format_name(t: &str) -> String { match t { "jpg" | "jpeg" => "JPEG".into(), o => o.to_ascii_uppercase() } }

/// One short line about what this conversion must give up, when it must give up anything.
pub fn media_notice(p: &Probe, target: &str) -> Option<String> {
    let name = format_name(target);
    match target {
        "gif" => return Some("GIF holds 256 colours per frame and has no sound, so colours are approximated.".into()),
        "mp3" | "m4a" | "wav" | "flac" | "ogg" | "opus" | "aiff" | "wma" => {
            let a = p.audio.first()?;
            if audio_fits(target, &a.codec) { return None; }
            if a.channels > 2 && matches!(target, "mp3" | "wma") { return Some(format!("{name} holds two channels, so surround sound is mixed down.")); }
            let target_lossless = matches!(target, "wav" | "flac" | "aiff");
            return match (target_lossless, a.is_lossless()) {
                (true, true) => None,
                (true, false) => Some(format!("The source is lossy. {name} keeps all of it, but cannot bring back what it lost.")),
                (false, true) if target == "m4a" => None,
                (false, true) => Some(format!("{name} is a lossy format, so some detail is lost. Using its top quality.")),
                (false, false) => Some(format!("Both formats are lossy, so a little is lost. Quality is matched to the source.")),
            };
        }
        _ => {}
    }
    let v = p.video.as_ref()?;
    if video_fits(target, &v.codec) { return None; }
    let planar = planar_of(&v.pix_fmt).unwrap_or_default();
    if target == "wmv" { return Some("WMV is lossy and 8-bit, so some detail is lost. Using its top quality.".into()); }
    if target == "avi" && !video_is_lossless(&v.codec) && (depth_of(&planar) > 8 || !planar.contains("420")) {
        return Some("This AVI codec stores 8-bit 4:2:0, so some colour detail is lost.".into());
    }
    if has_alpha(&planar) && matches!(target, "mp4" | "m4v" | "mov") { return Some(format!("{name} video here has no transparency, so it is flattened.")); }
    if video_is_lossless(&v.codec) { return None; }
    Some("The source is lossy, so re-encoding loses a little. Bitrate is matched to the source.".into())
}

pub fn image_notice(src_ext: &str, target: &str) -> Option<String> {
    let lossy_source = matches!(src_ext, "jpg" | "jpeg" | "heic" | "heif" | "avif");
    match target {
        "jpg" | "jpeg" => Some("JPEG is lossy and has no transparency. Saving at top quality with full colour.".into()),
        "heic" | "heif" => Some("HEIC is lossy here. Saving at top quality.".into()),
        "bmp" => Some("BMP keeps every pixel but flattens transparency.".into()),
        "webp" | "avif" if lossy_source => Some(format!("Both formats are lossy, so a little is lost. Size is matched to the source.")),
        _ => None,
    }
}

/// `cat` names the source: pdf, doc, slides, sheet, epub or sub.
pub fn document_notice(cat: &str, target: &str) -> Option<String> {
    Some(match (cat, target) {
        ("pdf", "jpg" | "png") | ("slides", "jpg" | "png") | ("doc", "jpg" | "png") => "Pages become pictures, so text stops being selectable.".into(),
        ("pdf", "txt" | "md" | "epub") => "Only the text carries over. Layout and images are left behind.".into(),
        ("pdf", "docx" | "xlsx") => "PDF does not record structure, so the layout is rebuilt as closely as possible.".into(),
        ("pdf", "pptx") => "Each page becomes a picture on a slide, so text stops being editable.".into(),
        ("doc" | "epub", "txt") => "Plain text has no formatting or images, so only the words carry over.".into(),
        ("doc" | "epub", "md") => "Markdown holds basic formatting only. The rest is simplified.".into(),
        ("sheet", "csv" | "tsv") => format!("{} holds one sheet of plain values. Formatting and formulas are lost.", format_name(target)),
        ("sub", "txt") => "Plain text has no timing, so only the words carry over.".into(),
        _ => return None,
    })
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

/// What Compress may give away. Everything left at `None` stays as it is.
#[derive(Clone, Debug, PartialEq)]
pub struct Squeeze {
    pub crf: u32,
    /// Tallest picture to keep.
    pub height: Option<u32>,
    /// Bits to allow each audio track, in kbps for a stereo pair. A track already within that is copied untouched.
    pub audio_kbps: Option<u32>,
    /// Highest frame rate to keep.
    pub fps: Option<u32>,
}

/// A picture quality from 0 to 100 as an x264 CRF: 100 is visually lossless, 0 is the worst x264 can do.
pub fn crf_for(percent: i32) -> u32 { (51.0 - percent.clamp(0, 100) as f64 * 0.35).round() as u32 }

/// Below these bitrates a full-band stereo signal only turns into noise, so the track gives up channels and bandwidth.
/// `spec` is the stream the options apply to, such as ":a:0", and `target` the extension being written.
pub fn audio_squeeze_args(spec: &str, target: &str, a: &AudioInfo, kbps: u32) -> Vec<String> {
    let mut o = vec![];
    if kbps < 48 && a.channels != 1 { o.extend([format!("-ac{spec}"), "1".to_string()]); }
    let rate = match (target, kbps) {
        // Opus picks its own bandwidth and takes only a few rates.
        ("opus", _) => 0,
        // Vorbis has no setup for so few bits at any higher rate.
        ("ogg", 0..=11) => 8000,
        (_, 0..=15) => 11025,
        (_, 16..=31) => 22050,
        _ => 0,
    };
    if rate > 0 && (a.rate == 0 || a.rate > rate) { o.extend([format!("-ar{spec}"), rate.to_string()]); }
    o
}

/// WMA refuses to go under this many kbps, however little else it is asked to carry.
pub const WMA_FLOOR_KBPS: u32 = 24;

/// One audio track of a compressed video. An allowance of `None`, or a track already within it, is copied when the
/// container can hold it, since re-encoding audio that small costs quality and saves nothing.
fn compress_audio_args(i: usize, a: &AudioInfo, allowance: Option<u32>) -> Vec<String> {
    let fits = audio_fits("mp4", &a.codec);
    let total = allowance.map(|k| k * a.channels.max(2) / 2);
    let within = total.is_some_and(|t| a.bitrate > 0 && a.bitrate <= t as u64 * 1000);
    if fits && (total.is_none() || within) { return vec![format!("-c:a:{i}"), "copy".to_string()]; }
    let mut o = vec![format!("-c:a:{i}"), "aac".to_string(), format!("-b:a:{i}"), format!("{}k", total.unwrap_or(256))];
    if let Some(k) = allowance { o.extend(audio_squeeze_args(&format!(":a:{i}"), "mp4", a, k)); }
    o
}

/// Compress is the one tool that spends quality on purpose, and only as much as was chosen.
/// Depth, chroma and every track stay.
pub fn compress_video_args(p: &Probe, s: &Squeeze) -> Vec<String> {
    let mut a = sv(&["-map", "0:V:0", "-c:v", "libx264", "-preset", "slow", "-crf", &s.crf.to_string()]);
    let mut chain = vec![];
    if let Some(v) = &p.video {
        let f = x264_nearest(&v.pix_fmt);
        let (w, h) = v.display_size();
        // Combing costs a lot of bits, and scaling would smear the two half-pictures together.
        if v.interlaced { chain.push("bwdif=mode=send_frame:deint=interlaced".to_string()); }
        if let Some(fps) = s.fps.filter(|n| v.fps <= 0.0 || v.fps > *n as f64) { chain.push(format!("fps={fps}")); }
        match s.height.filter(|t| h > *t) {
            Some(t) => chain.push(format!("scale=-2:{t}:flags=lanczos+accurate_rnd")),
            None => if odd_for_chroma(&f, w, h) { chain.push("pad=ceil(iw/2)*2:ceil(ih/2)*2".into()); },
        }
        chain.push(format!("format={f}"));
    } else if let Some(t) = s.height {
        chain.push(format!("scale=-2:'min({t},ih)':flags=lanczos+accurate_rnd"));
    }
    if !chain.is_empty() { a.extend(["-vf".to_string(), chain.join(",")]); }
    for (i, info) in p.audio.iter().enumerate() {
        a.extend(["-map".to_string(), format!("0:a:{i}")]);
        a.extend(compress_audio_args(i, info, s.audio_kbps));
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

/// Containers with no index to seek by. ffmpeg guesses a position from timestamps and can come up to a group of pictures
/// late, or find nothing at all near the end. Camcorders and DVDs write these.
pub fn seeks_loosely(ext: &str) -> bool {
    matches!(ext.to_ascii_lowercase().as_str(), "mts" | "m2ts" | "m2t" | "ts" | "tod" | "mod" | "vob" | "mpg" | "mpeg")
}

/// Far more than a group of pictures in any of those, and little enough to decode quickly.
const PRE_ROLL: f64 = 3.0;

/// Options before and after -i that begin exactly at `t`: a jump to somewhere safely earlier, then decoding up to it.
fn exact_seek(ext: &str, t: f64) -> (Vec<String>, Vec<String>) {
    let t = frame_seek(t);
    if !seeks_loosely(ext) { return (sv(&["-ss", &format!("{t:.6}")]), vec![]); }
    let early = t - PRE_ROLL;
    if early > 0.0 { return (sv(&["-ss", &format!("{early:.6}")]), sv(&["-ss", &format!("{PRE_ROLL:.6}")])); }
    (vec![], if t > 0.0 { sv(&["-ss", &format!("{t:.6}")]) } else { vec![] })
}

/// Options before and after -i for a single picture at `t`.
pub fn still_seek(ext: &str, t: f64) -> (Vec<String>, Vec<String>) { exact_seek(ext, t) }

/// Options before and after -i for the stretch from `a` to `b`.
pub fn cut_seek(ext: &str, a: f64, b: f64) -> (Vec<String>, Vec<String>) {
    if !seeks_loosely(ext) { return (seek_args(frame_seek(a), frame_seek(b)), vec![]); }
    let (before, mut after) = exact_seek(ext, a);
    after.extend(sv(&["-t", &format!("{:.6}", frame_seek(b) - frame_seek(a))]));
    (before, after)
}

/// A seek that lands on the frame starting at `t`. A frame's start is rarely a round number, so once written out it can
/// fall just past the frame, and ffmpeg would then begin on the next one. A millisecond before is safe at any real rate.
pub fn frame_seek(t: f64) -> f64 { (t - 0.001).max(0.0) }

/// What a single picture of this video needs in order to look as it does when played: whole frames instead of two
/// combed half-pictures, and square pixels, as an image has no other kind.
pub fn still_filter(v: &VideoInfo) -> Option<String> {
    let mut chain = vec![];
    if v.interlaced { chain.push("bwdif=mode=send_frame:deint=interlaced"); }
    if v.pixel_aspect > 0.0 && (v.pixel_aspect - 1.0).abs() > 0.001 { chain.push("scale=trunc(iw*sar/2)*2:ih:flags=lanczos+accurate_rnd+full_chroma_int,setsar=1"); }
    (!chain.is_empty()).then(|| chain.join(","))
}

/// The muxer to name for an extension ffmpeg cannot place by itself: camcorders gave these their own.
pub fn muxer_for(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "mod" => Some("vob"),
        "tod" => Some("mpegts"),
        _ => None,
    }
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
    // Two takes from one camera differ in bitrate and nothing else.
    let same_audio = |a: &[AudioInfo], b: &[AudioInfo]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| AudioInfo { bitrate: 0, ..x.clone() } == AudioInfo { bitrate: 0, ..y.clone() });
    let plain = |o: &VideoInfo| VideoInfo { bitrate: 0, ..o.clone() };
    exts.iter().all(|e| *e == exts[0]) && probes.iter().all(|p| {
        p.video.as_ref().is_some_and(|o| plain(o) == plain(v)) && same_audio(&p.audio, &first.audio) && p.subs.is_empty()
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

/// Lossy settings to try from a lossy source, best first: the first that does not outgrow the source is kept, the same
/// rule as video bitrate. A lossless copy of a JPEG would be several times its size and show nothing more.
pub fn still_ladder(ext: &str) -> Vec<Vec<String>> {
    if ext == "webp" {
        [100, 98, 95, 90].iter().map(|q: &u32| sv(&["-c:v", "libwebp", "-quality", &q.to_string()])).collect()
    } else {
        [8, 14, 20, 26].iter().map(|c: &u32| sv(&["-c:v", "libaom-av1", "-still-picture", "1", "-crf", &c.to_string(), "-pix_fmt", "yuv444p", "-cpu-used", "4"])).collect()
    }
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
    #[test]
    fn compress_leaves_audio_alone_until_it_is_over_the_allowance() {
        let ac3 = AudioInfo { codec: "ac3".into(), channels: 2, rate: 48000, bitrate: 192_000, ..Default::default() };
        let copied = vec!["-c:a:0".to_string(), "copy".to_string()];
        assert_eq!(compress_audio_args(0, &ac3, None), copied, "no allowance");
        assert_eq!(compress_audio_args(0, &ac3, Some(192)), copied, "already within it");
        let squeezed = compress_audio_args(0, &ac3, Some(64));
        assert_eq!(&squeezed[..4], &sv(&["-c:a:0", "aac", "-b:a:0", "64k"])[..]);
        assert_eq!(squeezed.len(), 4, "64 kbps is still full-band stereo");
    }
    #[test]
    fn compress_gives_surround_its_share_and_starves_tracks_only_when_asked() {
        let surround = AudioInfo { codec: "ac3".into(), channels: 6, rate: 48000, bitrate: 448_000, ..Default::default() };
        assert_eq!(compress_audio_args(1, &surround, Some(192)), vec!["-c:a:1".to_string(), "copy".to_string()], "576 kbps allowed for six channels");
        assert!(compress_audio_args(1, &surround, Some(96)).contains(&"288k".to_string()));
        let tiny = compress_audio_args(0, &AudioInfo { codec: "aac".into(), channels: 2, rate: 48000, bitrate: 128_000, ..Default::default() }, Some(12));
        assert!(tiny.windows(2).any(|w| w == ["-ac:a:0", "1"]), "{tiny:?}");
        assert!(tiny.windows(2).any(|w| w == ["-ar:a:0", "11025"]), "{tiny:?}");
        let mono = AudioInfo { channels: 1, rate: 48000, ..Default::default() };
        assert!(audio_squeeze_args(":a", "mp3", &AudioInfo { rate: 8000, ..mono.clone() }, 8).is_empty(), "nothing left to take");
        assert!(audio_squeeze_args(":a", "opus", &mono, 8).is_empty(), "opus keeps its own rate");
        assert_eq!(audio_squeeze_args(":a", "ogg", &mono, 8), sv(&["-ar:a", "8000"]));
    }
    #[test]
    fn compress_only_shrinks_and_never_stretches_a_small_picture() {
        let p = Probe { video: Some(VideoInfo { pix_fmt: "yuv420p".into(), width: 640, height: 360, fps: 25.0, ..Default::default() }), ..Default::default() };
        let vf = |s: Squeeze| { let a = compress_video_args(&p, &s); a[a.iter().position(|x| x == "-vf").unwrap() + 1].clone() };
        let base = Squeeze { crf: 23, height: None, audio_kbps: None, fps: None };
        assert!(!vf(Squeeze { height: Some(720), ..base.clone() }).contains("scale"));
        assert!(vf(Squeeze { height: Some(240), ..base.clone() }).contains("scale=-2:240"));
        assert!(!vf(Squeeze { fps: Some(30), ..base.clone() }).contains("fps="));
        assert!(vf(Squeeze { fps: Some(10), ..base }).contains("fps=10"));
    }
    #[test]
    fn picture_quality_covers_the_whole_crf_range() {
        assert_eq!((crf_for(0), crf_for(100)), (51, 16));
        assert_eq!(crf_for(80), 23);
    }

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
format.bit_rate="5000000"
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
        let a = AudioInfo { codec: "flac".into(), sample_fmt: "s32".into(), bits: 24, rate: 96000, channels: 1, ..Default::default() };
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
    fn black_lands_on_the_right_value_for_depth_and_range() {
        assert_eq!(yuv_of([0, 0, 0], "bt709", 1080, false, 10), (64, 512, 512));
        assert_eq!(yuv_of([0, 0, 0], "bt709", 1080, false, 8), (16, 128, 128));
        assert_eq!(yuv_of([0, 0, 0], "bt709", 1080, true, 8).0, 0);
        assert_eq!(yuv_of([255, 255, 255], "bt709", 1080, false, 8), (235, 128, 128));
        // Red differs by matrix, so the tag has to be honoured.
        assert_ne!(yuv_of([255, 0, 0], "bt709", 1080, false, 8), yuv_of([255, 0, 0], "bt470bg", 480, false, 8));
    }
    #[test]
    fn lossy_sources_are_capped_and_lossless_ones_are_not() {
        let v = VideoInfo { codec: "h264".into(), pix_fmt: "yuv420p".into(), width: 640, height: 360, bitrate: 5_000_000, ..Default::default() };
        let a = encode_video("mov", Some(&v)).join(" ");
        assert!(a.contains("-maxrate 4000k") && a.contains("-crf"), "{a}");
        let hevc = VideoInfo { codec: "hevc".into(), ..v.clone() };
        assert!(encode_video("mp4", Some(&hevc)).join(" ").contains("-maxrate 6666k"), "HEVC needs more bits as H.264");
        let raw = VideoInfo { codec: "ffv1".into(), ..v.clone() };
        let l = encode_video("mp4", Some(&raw)).join(" ");
        assert!(l.contains("-qp 0") && !l.contains("-maxrate"), "{l}");
        assert!(encode_video("avi", Some(&raw)).join(" ").contains("ffv1"));
        assert!(encode_video("webm", Some(&raw)).join(" ").contains("-lossless 1"));
    }
    #[test]
    fn audio_bitrate_follows_a_lossy_source() {
        let mp3 = AudioInfo { codec: "mp3".into(), sample_fmt: "fltp".into(), rate: 44100, channels: 2, bitrate: 128_000, ..Default::default() };
        assert!(encode_audio("m4a", &mp3, None).contains(&("b", "102k".to_string())));
        assert!(encode_audio("opus", &mp3, None).contains(&("b", "83k".to_string())));
        assert!(encode_audio("wma", &mp3, None).contains(&("b", "128k".to_string())));
        let flac = AudioInfo { codec: "flac".into(), sample_fmt: "s32".into(), rate: 44100, channels: 2, ..Default::default() };
        assert!(encode_audio("mp3", &flac, None).contains(&("b", "320k".to_string())));
        assert_eq!(encode_audio("m4a", &flac, None)[0].1, "alac");
        assert!(encode_audio("mp3", &mp3, Some("volume=1")).contains(&("b", "320k".to_string())), "edits stay at the top");
        let huge = AudioInfo { bitrate: 900_000, ..mp3 };
        assert!(encode_audio("mp3", &huge, None).contains(&("b", "320k".to_string())));
    }
    #[test]
    fn notices_appear_only_when_something_is_given_up() {
        let mut p = parse_probe(FLAT);
        assert!(media_notice(&p, "mkv").is_none(), "stream copy gives up nothing");
        assert!(media_notice(&p, "m4a").is_none(), "AAC moves as it is");
        assert!(media_notice(&p, "gif").unwrap().contains("256"));
        assert!(media_notice(&p, "webm").unwrap().contains("matched"));
        assert!(media_notice(&p, "mp3").unwrap().contains("lossy"));
        assert!(media_notice(&p, "wav").unwrap().contains("cannot bring back"));
        p.audio[0].codec = "flac".into();
        assert!(media_notice(&p, "flac").is_none());
        assert!(media_notice(&p, "m4a").is_none());
        assert!(media_notice(&p, "mp3").unwrap().contains("top quality"));
        assert!(image_notice("png", "jpg").is_some());
        assert!(image_notice("jpg", "png").is_none());
        assert!(document_notice("pdf", "txt").is_some());
        assert!(document_notice("sheet", "xlsx").is_none());
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
        let mut other_rate = plain.clone();
        other_rate.video.as_mut().unwrap().bitrate = 1;
        assert!(can_concat_copy(&[plain.clone(), other_rate], &e("mp4")), "bitrate alone does not stop a join");
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
