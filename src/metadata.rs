//! Shows everything a file says about itself, folded into sections, and writes the few details people change on images.
use crate::gfx::*;
use crate::ui::{self, glyph, id, r, Align, Page, Style, Ui, FOOT_H, PAD, TITLE_H};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use windows::Win32::UI::Input::KeyboardAndMouse::*;

pub struct Section { pub title: String, pub rows: Vec<(String, String)> }

/// A detail that can be changed. `key` is what the tool is told, `value` what the file holds now.
pub struct Editable { pub key: &'static str, pub label: &'static str, pub value: String }

#[derive(Default)]
pub struct Info {
    pub sections: Vec<Section>,
    pub editable: Vec<Editable>,
}

fn section(title: &str, rows: Vec<(String, String)>) -> Section { Section { title: title.into(), rows } }
fn row(k: &str, v: impl Into<String>) -> (String, String) { (k.to_string(), v.into()) }

pub fn human_size(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.2} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.0} KB", n as f64 / (1u64 << 10) as f64),
        n => format!("{n} bytes"),
    }
}

/// A calendar date and time from seconds since 1970.
fn date_time(secs: u64) -> String {
    let (days, rem) = ((secs / 86400) as i64, secs % 86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y}-{m:02}-{d:02} {:02}:{:02} UTC", rem / 3600, rem % 3600 / 60)
}

fn file_rows(p: &Path) -> Vec<(String, String)> {
    let mut v = vec![row("Name", p.file_name().unwrap_or_default().to_string_lossy()), row("Folder", p.parent().map(|d| d.display().to_string()).unwrap_or_default())];
    if let Ok(m) = std::fs::metadata(p) {
        v.push(row("Size", if m.len() < 1024 { human_size(m.len()) } else { format!("{} ({} bytes)", human_size(m.len()), m.len()) }));
        let when = |t: std::io::Result<std::time::SystemTime>| t.ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| date_time(d.as_secs()));
        if let Some(t) = when(m.modified()) { v.push(row("Modified", t)); }
        if let Some(t) = when(m.created()) { v.push(row("Created", t)); }
    }
    v
}

/// "codec_long_name" as "Codec long name".
fn readable(key: &str) -> String {
    let s = key.replace(['_', '.'], " ");
    let mut c = s.chars();
    match c.next() { Some(f) => f.to_uppercase().collect::<String>() + c.as_str(), None => s }
}

// ---- audio and video ----

/// One value of ffprobe's flat output, back to what it was before escaping.
fn unquote(v: &str) -> String {
    let v = v.trim().trim_matches('"');
    let mut out = String::with_capacity(v.len());
    let mut c = v.chars();
    while let Some(x) = c.next() {
        if x != '\\' { out.push(x); continue; }
        match c.next() { Some('n') => out.push('\n'), Some('t') => out.push('\t'), Some('r') => {} Some(o) => out.push(o), None => {} }
    }
    out
}

fn seconds_label(t: f64) -> String {
    let t = t.max(0.0);
    let (h, m, s) = ((t / 3600.0) as u32, ((t % 3600.0) / 60.0) as u32, t % 60.0);
    if h > 0 { format!("{h}:{m:02}:{s:05.2}") } else { format!("{m}:{s:05.2}") }
}

/// Sections from ffprobe's flat output. Nothing is left out except values that say nothing.
pub fn from_probe(flat: &str) -> (Vec<Section>, BTreeMap<String, String>) {
    let mut format = vec![];
    let mut tags = BTreeMap::new();
    let mut streams: BTreeMap<u32, Vec<(String, String)>> = BTreeMap::new();
    let mut chapters: BTreeMap<u32, Vec<(String, String)>> = BTreeMap::new();
    for line in flat.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = unquote(v);
        if v.is_empty() || v == "N/A" { continue; }
        if let Some(t) = k.strip_prefix("format.tags.") {
            tags.insert(t.to_string(), v);
        } else if let Some(f) = k.strip_prefix("format.") {
            format.push((f.to_string(), v));
        } else if let Some(rest) = k.strip_prefix("streams.stream.") {
            let Some((n, f)) = rest.split_once('.') else { continue };
            // A flag that is off says nothing.
            if f.starts_with("disposition.") && v == "0" { continue; }
            if let Ok(n) = n.parse() { streams.entry(n).or_default().push((f.to_string(), v)); }
        } else if let Some(rest) = k.strip_prefix("chapters.chapter.") {
            let Some((n, f)) = rest.split_once('.') else { continue };
            if let Ok(n) = n.parse() { chapters.entry(n).or_default().push((f.to_string(), v)); }
        }
    }
    let get = |rows: &[(String, String)], k: &str| rows.iter().find(|(a, _)| a == k).map(|(_, b)| b.clone());
    let mut summary = vec![];
    if let Some(v) = get(&format, "format_long_name") { summary.push(row("Format", v)); }
    if let Some(v) = get(&format, "duration").and_then(|d| d.parse().ok()) { summary.push(row("Length", seconds_label(v))); }
    if let Some(v) = get(&format, "bit_rate").and_then(|d| d.parse::<u64>().ok()) { summary.push(row("Bitrate", format!("{} kb/s", v / 1000))); }
    for key in ["title", "artist", "album", "date", "comment"] {
        if let Some((k, v)) = tags.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)) { summary.push((readable(&k.to_ascii_lowercase()), v.clone())); }
    }
    let mut counts: HashMap<String, u32> = HashMap::new();
    let mut stream_sections = vec![];
    for rows in streams.values() {
        let kind = get(rows, "codec_type").unwrap_or_else(|| "data".into());
        let n = { let c = counts.entry(kind.clone()).or_default(); *c += 1; *c };
        let codec = get(rows, "codec_name").unwrap_or_default();
        let line = match kind.as_str() {
            "video" => format!("{codec}, {}x{}{}", get(rows, "width").unwrap_or_default(), get(rows, "height").unwrap_or_default(), rate_of(rows).map(|f| format!(", {f} fps")).unwrap_or_default()),
            "audio" => format!("{codec}, {}, {} Hz", get(rows, "channel_layout").unwrap_or_else(|| format!("{} channels", get(rows, "channels").unwrap_or_default())), get(rows, "sample_rate").unwrap_or_default()),
            _ => codec,
        };
        summary.push((if n > 1 { format!("{} {n}", readable(&kind)) } else { readable(&kind) }, line));
        stream_sections.push(section(&format!("{} {n}", readable(&kind)), rows.iter().filter(|(k, _)| k != "codec_type").map(|(k, v)| (readable(k.strip_prefix("tags.").unwrap_or(k)), v.clone())).collect()));
    }
    let mut out = vec![section("Summary", summary)];
    if !tags.is_empty() { out.push(section("Tags", tags.iter().map(|(k, v)| (readable(k), v.clone())).collect())); }
    out.extend(stream_sections);
    if !chapters.is_empty() {
        let rows = chapters.iter().map(|(n, rows)| {
            let name = get(rows, "tags.title").unwrap_or_default();
            let at = get(rows, "start_time").and_then(|s| s.parse().ok()).map(seconds_label).unwrap_or_default();
            (format!("{}", n + 1), if name.is_empty() { at } else { format!("{at}  {name}") })
        }).collect();
        out.push(section("Chapters", rows));
    }
    out.push(section("Container", format.iter().map(|(k, v)| (readable(k), v.clone())).collect()));
    (out, tags)
}

fn rate_of(rows: &[(String, String)]) -> Option<String> {
    let v = rows.iter().find(|(k, _)| k == "avg_frame_rate").map(|(_, v)| v.as_str())?;
    let (a, b) = v.split_once('/')?;
    let (a, b): (f64, f64) = (a.parse().ok()?, b.parse().ok()?);
    if a <= 0.0 || b <= 0.0 { return None; }
    let s = format!("{:.2}", a / b);
    Some(s.trim_end_matches('0').trim_end_matches('.').to_string())
}

fn probe_flat(p: &Path) -> Option<String> {
    let exe = crate::media::ffprobe()?;
    crate::job::run(None, &exe, &["-v".into(), "error".into(), "-show_format".into(), "-show_streams".into(), "-show_chapters".into(), "-of".into(), "flat".into(), p.display().to_string()], None).ok()
}

pub fn read_media(p: &Path) -> Result<Info, String> {
    let flat = probe_flat(p).ok_or("Could not read this file's details. Put ffprobe.exe next to ffmpeg.exe")?;
    let (mut sections, tags) = from_probe(&flat);
    sections.insert(1, section("File", file_rows(p)));
    let tag = |k: &str| tags.iter().find(|(t, _)| t.eq_ignore_ascii_case(k)).map(|(_, v)| v.clone()).unwrap_or_default();
    Ok(Info { sections, editable: vec![
        Editable { key: "title", label: "Title", value: tag("title") },
        Editable { key: "artist", label: "Artist or author", value: tag("artist") },
        Editable { key: "comment", label: "Comment", value: tag("comment") },
    ] })
}

// ---- PDF ----

/// PDF text is UTF-16 when it says so, and otherwise close enough to Latin-1.
fn pdf_text(b: &[u8]) -> String {
    if b.starts_with(&[0xFE, 0xFF]) {
        let u: Vec<u16> = b[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&u);
    }
    b.iter().map(|&c| c as char).collect()
}

/// D:20240131120000+01'00' as 2024-01-31 12:00:00.
fn pdf_date(s: &str) -> String {
    let d = s.strip_prefix("D:").unwrap_or(s);
    let digits: String = d.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() < 8 { return s.to_string(); }
    let part = |a: usize, b: usize| digits.get(a..b).unwrap_or("00");
    format!("{}-{}-{} {}:{}:{}", part(0, 4), part(4, 6), part(6, 8), part(8, 10), part(10, 12), part(12, 14))
}

pub fn read_pdf(p: &Path) -> Result<Info, String> {
    let d = crate::pdf::load_doc(p)?;
    let mut info = vec![];
    let dict = d.trailer.get(b"Info").ok().and_then(|o| o.as_reference().ok()).and_then(|id| d.get_object(id).ok()).and_then(|o| o.as_dict().ok());
    if let Some(dict) = dict {
        for (k, v) in dict.iter() {
            let name = String::from_utf8_lossy(k).to_string();
            let value = match v.as_str() { Ok(b) => pdf_text(b), Err(_) => format!("{v:?}") };
            if value.is_empty() { continue; }
            let value = if name.ends_with("Date") { pdf_date(&value) } else { value };
            info.push((name, value));
        }
    }
    let value = |k: &str| info.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default();
    let mut summary = vec![row("Pages", d.get_pages().len().to_string()), row("PDF version", d.version.clone())];
    for k in ["Title", "Author", "Subject", "Creator", "Producer"] { if !value(k).is_empty() { summary.insert(summary.len().min(3), row(k, value(k))); } }
    let editable = vec![
        Editable { key: "title", label: "Title", value: value("Title") },
        Editable { key: "author", label: "Author", value: value("Author") },
        Editable { key: "subject", label: "Subject", value: value("Subject") },
    ];
    let mut sections = vec![section("Summary", summary), section("File", file_rows(p))];
    if !info.is_empty() { sections.push(section("Document", info.iter().map(|(k, v)| (readable(k), v.clone())).collect())); }
    // The newer place for the same details, and sometimes more.
    let root = d.trailer.get(b"Root").ok().and_then(|o| o.as_reference().ok()).and_then(|id| d.get_object(id).ok()).and_then(|o| o.as_dict().ok());
    let xmp = root.and_then(|c| c.get(b"Metadata").ok()).and_then(|o| o.as_reference().ok()).and_then(|id| d.get_object(id).ok()).and_then(|o| o.as_stream().ok()).map(|s| from_xmp(&String::from_utf8_lossy(&s.content)));
    if let Some(rows) = xmp { if !rows.is_empty() { sections.push(section("XMP", rows)); } }
    Ok(Info { sections, editable })
}

// ---- images: EXIF ----

fn tag_name(group: &str, tag: u16) -> Option<&'static str> {
    if group == "GPS" {
        return Some(match tag {
            0 => "Version", 1 => "Latitude reference", 2 => "Latitude", 3 => "Longitude reference", 4 => "Longitude",
            5 => "Altitude reference", 6 => "Altitude", 7 => "Time (UTC)", 8 => "Satellites", 9 => "Status", 10 => "Measure mode",
            11 => "Precision", 12 => "Speed unit", 13 => "Speed", 14 => "Track reference", 15 => "Track",
            16 => "Direction reference", 17 => "Direction", 18 => "Map datum", 27 => "Processing method", 29 => "Date", 30 => "Differential",
            _ => return None,
        });
    }
    Some(match tag {
        0x00FE => "Subfile type", 0x0100 => "Width", 0x0101 => "Height", 0x0102 => "Bits per sample", 0x0103 => "Compression",
        0x0106 => "Photometric interpretation", 0x010E => "Description", 0x010F => "Camera make", 0x0110 => "Camera model",
        0x0111 => "Strip offsets", 0x0112 => "Orientation", 0x0115 => "Samples per pixel", 0x0116 => "Rows per strip", 0x0117 => "Strip byte counts",
        0x011A => "Horizontal resolution", 0x011B => "Vertical resolution", 0x011C => "Planar configuration", 0x0128 => "Resolution unit",
        0x0131 => "Software", 0x0132 => "Date modified", 0x013B => "Artist", 0x013C => "Host computer", 0x013D => "Predictor",
        0x013E => "White point", 0x013F => "Primary chromaticities", 0x0201 => "Thumbnail offset", 0x0202 => "Thumbnail length",
        0x0211 => "YCbCr coefficients", 0x0212 => "YCbCr subsampling", 0x0213 => "YCbCr positioning", 0x0214 => "Reference black and white",
        0x4746 => "Rating", 0x8298 => "Copyright", 0x829A => "Exposure time", 0x829D => "F-number", 0x8822 => "Exposure program",
        0x8827 => "ISO", 0x8830 => "Sensitivity type", 0x9000 => "EXIF version", 0x9003 => "Date taken", 0x9004 => "Date digitized",
        0x9010 => "Time zone", 0x9011 => "Time zone (taken)", 0x9012 => "Time zone (digitized)", 0x9101 => "Components",
        0x9102 => "Compressed bits per pixel", 0x9201 => "Shutter speed", 0x9202 => "Aperture", 0x9203 => "Brightness",
        0x9204 => "Exposure compensation", 0x9205 => "Maximum aperture", 0x9206 => "Subject distance", 0x9207 => "Metering mode",
        0x9208 => "Light source", 0x9209 => "Flash", 0x920A => "Focal length", 0x9214 => "Subject area", 0x927C => "Maker note",
        0x9286 => "User comment", 0x9290 => "Subsecond time", 0x9291 => "Subsecond time (taken)", 0x9292 => "Subsecond time (digitized)",
        0x9C9B => "Windows title", 0x9C9C => "Windows comment", 0x9C9D => "Windows author", 0x9C9E => "Windows keywords", 0x9C9F => "Windows subject",
        0xA000 => "FlashPix version", 0xA001 => "Colour space", 0xA002 => "Pixel width", 0xA003 => "Pixel height", 0xA004 => "Related sound file",
        0xA20E => "Focal plane X resolution", 0xA20F => "Focal plane Y resolution", 0xA210 => "Focal plane resolution unit",
        0xA215 => "Exposure index", 0xA217 => "Sensing method", 0xA300 => "File source", 0xA301 => "Scene type", 0xA302 => "CFA pattern",
        0xA401 => "Custom rendered", 0xA402 => "Exposure mode", 0xA403 => "White balance", 0xA404 => "Digital zoom", 0xA405 => "Focal length (35 mm)",
        0xA406 => "Scene capture type", 0xA407 => "Gain control", 0xA408 => "Contrast", 0xA409 => "Saturation", 0xA40A => "Sharpness",
        0xA40C => "Subject distance range", 0xA420 => "Unique image ID", 0xA430 => "Owner", 0xA431 => "Body serial number",
        0xA432 => "Lens specification", 0xA433 => "Lens make", 0xA434 => "Lens model", 0xA435 => "Lens serial number", 0xA460 => "Composite image",
        _ => return None,
    })
}

/// What a code means, for the few that people read.
fn tag_meaning(group: &str, tag: u16, v: u64) -> Option<&'static str> {
    if group == "GPS" { return None; }
    Some(match (tag, v) {
        (0x0112, 1) => "Upright", (0x0112, 2) => "Mirrored", (0x0112, 3) => "Upside down", (0x0112, 4) => "Upside down, mirrored",
        (0x0112, 5) => "On its side, mirrored", (0x0112, 6) => "Turned 90° right", (0x0112, 7) => "On its side, mirrored", (0x0112, 8) => "Turned 90° left",
        (0x0128 | 0xA210, 2) => "Inches", (0x0128 | 0xA210, 3) => "Centimetres",
        (0xA001, 1) => "sRGB", (0xA001, 0xFFFF) => "Uncalibrated",
        (0x8822, 1) => "Manual", (0x8822, 2) => "Program", (0x8822, 3) => "Aperture priority", (0x8822, 4) => "Shutter priority",
        (0x9207, 2) => "Centre weighted", (0x9207, 3) => "Spot", (0x9207, 5) => "Pattern",
        (0xA402, 0) => "Auto", (0xA402, 1) => "Manual", (0xA402, 2) => "Bracket", (0xA403, 0) => "Auto", (0xA403, 1) => "Manual",
        _ => return None,
    })
}

struct Tiff<'a> { b: &'a [u8], le: bool }

impl<'a> Tiff<'a> {
    fn new(b: &'a [u8]) -> Option<Tiff<'a>> {
        let le = match b.get(..4)? { [b'I', b'I', 42, 0] => true, [b'M', b'M', 0, 42] => false, _ => return None };
        Some(Tiff { b, le })
    }
    fn u16(&self, o: usize) -> Option<u16> { let s = self.b.get(o..o.checked_add(2)?)?; Some(if self.le { u16::from_le_bytes([s[0], s[1]]) } else { u16::from_be_bytes([s[0], s[1]]) }) }
    fn u32(&self, o: usize) -> Option<u32> { let s = self.b.get(o..o.checked_add(4)?)?; Some(if self.le { u32::from_le_bytes([s[0], s[1], s[2], s[3]]) } else { u32::from_be_bytes([s[0], s[1], s[2], s[3]]) }) }

    /// The bytes a tag's value lives in, inline or elsewhere.
    fn value_bytes(&self, entry: usize) -> Option<(u16, usize, &'a [u8])> {
        let (ty, n) = (self.u16(entry + 2)?, self.u32(entry + 4)? as usize);
        let size = match ty { 1 | 2 | 6 | 7 => 1, 3 | 8 => 2, 4 | 9 | 11 => 4, 5 | 10 | 12 => 8, _ => return None };
        let len = size * n;
        let at = if len <= 4 { entry + 8 } else { self.u32(entry + 8)? as usize };
        Some((ty, n, self.b.get(at..at.checked_add(len)?)?))
    }

    fn format(&self, group: &str, tag: u16, entry: usize) -> Option<String> {
        let (ty, n, d) = self.value_bytes(entry)?;
        let r16 = |c: &[u8]| if self.le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) };
        let r32 = |c: &[u8]| if self.le { u32::from_le_bytes([c[0], c[1], c[2], c[3]]) } else { u32::from_be_bytes([c[0], c[1], c[2], c[3]]) };
        // Windows keeps its own tags as UTF-16 in plain bytes.
        if (0x9C9B..=0x9C9F).contains(&tag) && group != "GPS" {
            let u: Vec<u16> = d.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            return Some(String::from_utf16_lossy(&u).trim_end_matches('\0').to_string());
        }
        if tag == 0x9286 && group != "GPS" {
            let body = d.get(8..).unwrap_or(&[]);
            return Some(String::from_utf8_lossy(body).trim_end_matches('\0').trim().to_string());
        }
        if matches!(tag, 0x9000 | 0xA000) && group != "GPS" { return Some(String::from_utf8_lossy(d).to_string()); }
        let joined = |v: Vec<String>| {
            if v.len() > 16 { format!("{} values: {} ...", v.len(), v[..8].join(", ")) } else { v.join(", ") }
        };
        Some(match ty {
            2 => String::from_utf8_lossy(d).trim_end_matches('\0').to_string(),
            1 | 6 | 7 => {
                if n > 16 { return Some(human_size(n as u64)); }
                d.iter().map(|x| format!("{x}")).collect::<Vec<_>>().join(", ")
            }
            3 | 8 => {
                if n == 1 { let v = r16(d) as u64; if let Some(m) = tag_meaning(group, tag, v) { return Some(m.to_string()); } }
                joined(d.chunks_exact(2).map(|c| if ty == 8 { (r16(c) as i16).to_string() } else { r16(c).to_string() }).collect())
            }
            4 | 9 => joined(d.chunks_exact(4).map(|c| if ty == 9 { (r32(c) as i32).to_string() } else { r32(c).to_string() }).collect()),
            5 | 10 => joined(d.chunks_exact(8).map(|c| {
                let (a, b) = (r32(&c[..4]), r32(&c[4..]));
                if ty == 10 { fraction(a as i32 as f64, b as i32 as f64) } else { fraction(a as f64, b as f64) }
            }).collect()),
            11 => joined(d.chunks_exact(4).map(|c| f32::from_bits(r32(c)).to_string()).collect()),
            12 => joined(d.chunks_exact(8).map(|c| {
                let bits = if self.le { u64::from_le_bytes(c.try_into().unwrap()) } else { u64::from_be_bytes(c.try_into().unwrap()) };
                f64::from_bits(bits).to_string()
            }).collect()),
            _ => return None,
        })
    }

    fn ifd(&self, group: &str, at: usize, out: &mut Vec<Section>, depth: u32) -> Option<u32> {
        let n = self.u16(at)? as usize;
        let mut rows = vec![];
        let mut subs = vec![];
        for i in 0..n.min(2048) {
            let e = at + 2 + i * 12;
            let tag = self.u16(e)?;
            match (group, tag) {
                ("GPS", _) => {}
                (_, 0x8769) => { subs.push(("Exposure", self.u32(e + 8)? as usize)); continue; }
                (_, 0x8825) => { subs.push(("GPS", self.u32(e + 8)? as usize)); continue; }
                (_, 0xA005) => continue, // an index of how JPEG was written, nothing to read
                _ => {}
            }
            let name = tag_name(group, tag).map(str::to_string).unwrap_or_else(|| format!("Tag 0x{tag:04X}"));
            let (_, count, _) = self.value_bytes(e).unwrap_or((0, 0, &[]));
            let value = if tag == 0x927C && group != "GPS" { human_size(count as u64) } else { self.format(group, tag, e).unwrap_or_default() };
            if !value.is_empty() { rows.push((name, value)); }
        }
        if !rows.is_empty() { out.push(section(group, rows)); }
        if depth < 2 { for (g, o) in subs { self.ifd(g, o, out, depth + 1); } }
        self.u32(at + 2 + n * 12)
    }
}

/// An exposure reads as 1/250, an aperture as 2.8, a whole number as itself.
fn fraction(a: f64, b: f64) -> String {
    if b == 0.0 { return "unknown".into(); }
    if a == 0.0 { return "0".into(); }
    if a.fract() == 0.0 && b.fract() == 0.0 && (a / b).fract() == 0.0 { return format!("{}", a / b); }
    if a == 1.0 { return format!("1/{b}"); }
    let s = format!("{:.3}", a / b);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// All of a TIFF structure, which is what EXIF is too.
pub fn from_tiff(b: &[u8]) -> Vec<Section> {
    let Some(t) = Tiff::new(b) else { return vec![] };
    let mut out = vec![];
    let Some(first) = t.u32(4) else { return out };
    let next = t.ifd("Image", first as usize, &mut out, 0);
    if let Some(n) = next.filter(|n| *n != 0) { t.ifd("Thumbnail", n as usize, &mut out, 1); }
    out
}

/// Plain name and value pairs from an XMP packet, in document order. It is XML, but only its leaves matter here.
pub fn from_xmp(x: &str) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = vec![];
    let mut open: Vec<String> = vec![];
    let mut i = 0;
    while let Some(a) = x[i..].find('<') {
        let start = i + a;
        let Some(z) = x[start..].find('>') else { break };
        let tag = &x[start + 1..start + z];
        i = start + z + 1;
        if tag.starts_with('?') || tag.starts_with('!') { continue; }
        if let Some(_close) = tag.strip_prefix('/') { open.pop(); continue; }
        let self_closing = tag.ends_with('/');
        let body = tag.trim_end_matches('/');
        let name = body.split_whitespace().next().unwrap_or("");
        // Attributes hold values too.
        let mut rest = &body[name.len()..];
        while let Some(eq) = rest.find("=\"") {
            let key = rest[..eq].trim();
            let after = &rest[eq + 2..];
            let Some(end) = after.find('"') else { break };
            let value = &after[..end];
            if !key.starts_with("xmlns") && !key.starts_with("rdf:") && !key.starts_with("x:") && !value.is_empty() { rows.push((xmp_name(key), xml_text(value))); }
            rest = &after[end + 1..];
        }
        if self_closing { continue; }
        open.push(name.to_string());
        let end = x[i..].find('<').map(|n| i + n).unwrap_or(x.len());
        let text = x[i..end].trim();
        if !text.is_empty() {
            let owner = open.iter().rev().find(|n| !n.starts_with("rdf:")).cloned().unwrap_or_default();
            if !owner.is_empty() { rows.push((xmp_name(&owner), xml_text(text))); }
        }
    }
    rows
}

/// "xmp:CreatorTool" as "Creator tool".
fn xmp_name(n: &str) -> String {
    let n = n.rsplit(':').next().unwrap_or(n);
    let mut s = String::new();
    for (i, c) in n.chars().enumerate() {
        if c.is_uppercase() && i > 0 { s.push(' '); s.extend(c.to_lowercase()); } else if i == 0 { s.extend(c.to_uppercase()); } else { s.push(c); }
    }
    s
}

fn xml_text(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

fn add_exif(sections: &mut Vec<Section>, tiff: &[u8]) { sections.extend(from_tiff(tiff)); }
fn add_xmp(sections: &mut Vec<Section>, text: &str) {
    let rows = from_xmp(text);
    if !rows.is_empty() { sections.push(section("XMP", rows)); }
}

fn read_jpeg(b: &[u8], sections: &mut Vec<Section>, summary: &mut Vec<(String, String)>) {
    let mut i = 2;
    let mut other = vec![];
    while let (Some(0xFF), Some(&m)) = (b.get(i), b.get(i + 1)) {
        if m == 0xDA || m == 0xD9 { break; }
        if m == 0xFF { i += 1; continue; }
        let Some(len) = b.get(i + 2..i + 4).map(|l| u16::from_be_bytes([l[0], l[1]]) as usize) else { break };
        let Some(d) = b.get(i + 4..i + 2 + len) else { break };
        match m {
            0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF if d.len() >= 6 => {
                summary.push(row("Dimensions", format!("{} x {}", u16::from_be_bytes([d[3], d[4]]), u16::from_be_bytes([d[1], d[2]]))));
                other.push(row("Bits per sample", d[0].to_string()));
                other.push(row("Colour channels", d[5].to_string()));
                other.push(row("Encoding", if m == 0xC2 { "Progressive" } else { "Baseline" }));
            }
            0xE0 if d.starts_with(b"JFIF\0") && d.len() >= 12 => {
                other.push(row("JFIF version", format!("{}.{:02}", d[5], d[6])));
                other.push(row("Pixel density", format!("{} x {} {}", u16::from_be_bytes([d[8], d[9]]), u16::from_be_bytes([d[10], d[11]]), match d[7] { 1 => "per inch", 2 => "per cm", _ => "(aspect only)" })));
            }
            0xE1 if d.starts_with(b"Exif\0\0") => add_exif(sections, &d[6..]),
            0xE1 if d.starts_with(b"http://ns.adobe.com/xap/1.0/\0") => add_xmp(sections, &String::from_utf8_lossy(&d[29..])),
            0xE2 if d.starts_with(b"ICC_PROFILE\0") => other.push(row("Colour profile", human_size(d.len() as u64))),
            0xED => other.push(row("Photoshop data", human_size(d.len() as u64))),
            0xEE if d.starts_with(b"Adobe") => other.push(row("Adobe colour marker", "Yes")),
            0xFE => other.push(row("Comment", String::from_utf8_lossy(d).trim_end_matches('\0'))),
            _ => {}
        }
        i += 2 + len;
    }
    if !other.is_empty() { sections.insert(0, section("JPEG", other)); }
}

fn read_png(b: &[u8], sections: &mut Vec<Section>, summary: &mut Vec<(String, String)>) {
    let mut i = 8;
    let mut info = vec![];
    let mut text = vec![];
    while let Some(head) = b.get(i..i + 8) {
        let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let kind = &head[4..8];
        let Some(d) = b.get(i + 8..(i + 8).saturating_add(len)) else { break };
        match kind {
            b"IHDR" if d.len() >= 13 => {
                summary.push(row("Dimensions", format!("{} x {}", u32::from_be_bytes([d[0], d[1], d[2], d[3]]), u32::from_be_bytes([d[4], d[5], d[6], d[7]]))));
                info.push(row("Bits per sample", d[8].to_string()));
                info.push(row("Colour type", match d[9] { 0 => "Grey", 2 => "RGB", 3 => "Palette", 4 => "Grey with transparency", 6 => "RGB with transparency", _ => "Unknown" }));
                info.push(row("Interlaced", if d[12] == 1 { "Yes" } else { "No" }));
            }
            b"pHYs" if d.len() >= 9 => info.push(row("Pixel density", format!("{} x {} {}", u32::from_be_bytes([d[0], d[1], d[2], d[3]]), u32::from_be_bytes([d[4], d[5], d[6], d[7]]), if d[8] == 1 { "per metre" } else { "(aspect only)" }))),
            b"gAMA" if d.len() >= 4 => info.push(row("Gamma", fraction(u32::from_be_bytes([d[0], d[1], d[2], d[3]]) as f64, 100000.0))),
            b"sRGB" => info.push(row("Colour space", "sRGB")),
            b"iCCP" => info.push(row("Colour profile", String::from_utf8_lossy(d.split(|c| *c == 0).next().unwrap_or(&[])))),
            b"tIME" if d.len() >= 7 => info.push(row("Date modified", format!("{}-{:02}-{:02} {:02}:{:02}:{:02}", u16::from_be_bytes([d[0], d[1]]), d[2], d[3], d[4], d[5], d[6]))),
            b"eXIf" => add_exif(sections, d),
            b"tEXt" => if let Some(z) = d.iter().position(|c| *c == 0) {
                text.push((String::from_utf8_lossy(&d[..z]).to_string(), d[z + 1..].iter().map(|c| *c as char).collect::<String>()));
            },
            b"iTXt" => if let Some(z) = d.iter().position(|c| *c == 0) {
                let key = String::from_utf8_lossy(&d[..z]).to_string();
                // Compression flag and method, then a language and a translated keyword.
                let compressed = d.get(z + 1).copied() == Some(1);
                let mut at = z + 3;
                for _ in 0..2 { at += d.get(at..).and_then(|s| s.iter().position(|c| *c == 0)).unwrap_or(0) + 1; }
                let value = if compressed { "(compressed text)".to_string() } else { String::from_utf8_lossy(d.get(at..).unwrap_or(&[])).to_string() };
                if key == "XML:com.adobe.xmp" { add_xmp(sections, &value); } else { text.push((key, value)); }
            },
            b"zTXt" => if let Some(z) = d.iter().position(|c| *c == 0) { text.push((String::from_utf8_lossy(&d[..z]).to_string(), "(compressed text)".to_string())); },
            b"IEND" => break,
            _ => {}
        }
        i += 12 + len;
    }
    if !text.is_empty() { sections.insert(0, section("Text", text)); }
    if !info.is_empty() { sections.insert(0, section("PNG", info)); }
}

/// Formats with no walker of their own often still hold standard EXIF and XMP blocks, which mark themselves clearly.
fn scan_blocks(b: &[u8], sections: &mut Vec<Section>) {
    let find = |needle: &[u8], from: usize| b.get(from..).and_then(|s| s.windows(needle.len()).position(|w| w == needle)).map(|n| n + from);
    if let Some(a) = find(b"Exif\0\0", 0) { add_exif(sections, &b[a + 6..]); }
    else if let Some(a) = find(b"EXIF", 0) { if b.get(a + 8..).is_some_and(|t| Tiff::new(t).is_some()) { add_exif(sections, &b[a + 8..]); } }
    if let (Some(a), Some(z)) = (find(b"<x:xmpmeta", 0), find(b"</x:xmpmeta>", 0)) { if z > a { add_xmp(sections, &String::from_utf8_lossy(&b[a..z + 12])); } }
}

fn find_row<'a>(sections: &'a [Section], group: &str, name: &str) -> Option<&'a str> {
    sections.iter().filter(|s| s.title == group).flat_map(|s| s.rows.iter()).find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// Degrees from the three numbers GPS stores, with its letter deciding the sign.
fn gps_position(sections: &[Section]) -> Option<String> {
    let deg = |v: &str, side: &str| -> Option<f64> {
        let n: Vec<f64> = v.split(", ").filter_map(|x| x.parse().ok()).collect();
        let d = n.first()? + n.get(1).unwrap_or(&0.0) / 60.0 + n.get(2).unwrap_or(&0.0) / 3600.0;
        Some(if side == "S" || side == "W" { -d } else { d })
    };
    let lat = deg(find_row(sections, "GPS", "Latitude")?, find_row(sections, "GPS", "Latitude reference").unwrap_or("N"))?;
    let lon = deg(find_row(sections, "GPS", "Longitude")?, find_row(sections, "GPS", "Longitude reference").unwrap_or("E"))?;
    Some(format!("{lat:.5}, {lon:.5}"))
}

pub fn read_image(p: &Path) -> Result<Info, String> {
    let b = std::fs::read(p).map_err(|e| e.to_string())?;
    let e = crate::catalog::ext_of(p);
    let mut sections = vec![];
    let mut summary = vec![row("Format", e.to_ascii_uppercase())];
    match e.as_str() {
        "jpg" | "jpeg" => read_jpeg(&b, &mut sections, &mut summary),
        "png" => read_png(&b, &mut sections, &mut summary),
        "tif" | "tiff" => sections.extend(from_tiff(&b)),
        _ => scan_blocks(&b, &mut sections),
    }
    if !summary.iter().any(|(k, _)| k == "Dimensions") {
        if let (Some(w), Some(h)) = (find_row(&sections, "Image", "Width"), find_row(&sections, "Image", "Height")) { summary.push(row("Dimensions", format!("{w} x {h}"))); }
    }
    // The picture itself, as far as ffmpeg can tell, for what the walkers above do not cover.
    if !matches!(e.as_str(), "jpg" | "jpeg" | "png" | "tif" | "tiff") {
        if let Some(flat) = probe_flat(p) {
            let (probed, _) = from_probe(&flat);
            for mut s in probed {
                if s.title == "Summary" { continue; }
                // To ffmpeg a picture is a very short video.
                if s.title == "Video 1" {
                    s.title = "Picture".into();
                    let cell = |k: &str| s.rows.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
                    if let (Some(w), Some(h)) = (cell("Width"), cell("Height")) { if !summary.iter().any(|(a, _)| a == "Dimensions") { summary.push(row("Dimensions", format!("{w} x {h}"))); } }
                    if let Some(c) = cell("Codec long name") { summary.push(row("Encoding", c)); }
                }
                sections.push(s);
            }
        }
    }
    for (group, name, label) in [("Image", "Camera make", "Camera make"), ("Image", "Camera model", "Camera model"), ("Exposure", "Date taken", "Date taken"), ("Image", "Description", "Description"), ("Image", "Artist", "Artist")] {
        if let Some(v) = find_row(&sections, group, name) { summary.push(row(label, v)); }
    }
    if let Some(at) = gps_position(&sections) { summary.push(row("Location", at)); }
    let mut all = vec![section("Summary", summary), section("File", file_rows(p))];
    let text = |k: &str| find_row(&sections, "Text", k).unwrap_or("").to_string();
    let exif = |k: &str| find_row(&sections, "Image", k).unwrap_or("").to_string();
    let editable = match e.as_str() {
        "jpg" | "jpeg" => vec![
            Editable { key: "title", label: "Description", value: exif("Description") },
            Editable { key: "artist", label: "Artist", value: exif("Artist") },
            Editable { key: "copyright", label: "Copyright", value: exif("Copyright") },
        ],
        "png" => vec![
            Editable { key: "title", label: "Title", value: text("Title") },
            Editable { key: "artist", label: "Author", value: text("Author") },
            Editable { key: "copyright", label: "Copyright", value: text("Copyright") },
        ],
        _ => vec![],
    };
    all.extend(sections);
    Ok(Info { sections: all, editable })
}

// ---- writing image details ----

fn exif_tag_of(key: &str) -> Option<u16> { match key { "title" => Some(0x010E), "artist" => Some(0x013B), "copyright" => Some(0x8298), _ => None } }
fn png_key_of(key: &str) -> Option<&'static str> { match key { "title" => Some("Title"), "artist" => Some("Author"), "copyright" => Some("Copyright"), _ => None } }

/// A new TIFF block with these tags changed, an empty value removing one. The old directory stays in place, unused:
/// every other value, and the sub-directories, stay at the offsets they point from.
fn tiff_with(old: Option<&[u8]>, tags: &[(u16, &str)]) -> Option<Vec<u8>> {
    let mut out = old.map(<[u8]>::to_vec).unwrap_or_else(|| vec![b'I', b'I', 42, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let t = Tiff::new(&out)?;
    let le = t.le;
    let first = t.u32(4)? as usize;
    let n = t.u16(first)? as usize;
    let mut entries: Vec<[u8; 12]> = vec![];
    for i in 0..n {
        let e = first + 2 + i * 12;
        let raw: [u8; 12] = out.get(e..e + 12)?.try_into().ok()?;
        entries.push(raw);
    }
    let next = t.u32(first + 2 + n * 12)?;
    let tag_of = |raw: &[u8; 12]| if le { u16::from_le_bytes([raw[0], raw[1]]) } else { u16::from_be_bytes([raw[0], raw[1]]) };
    let w16 = |v: u16| if le { v.to_le_bytes() } else { v.to_be_bytes() };
    let w32 = |v: u32| if le { v.to_le_bytes() } else { v.to_be_bytes() };
    for (tag, value) in tags {
        entries.retain(|raw| tag_of(raw) != *tag);
        if value.is_empty() { continue; }
        let mut text = value.as_bytes().to_vec();
        text.push(0);
        let mut raw = [0u8; 12];
        raw[..2].copy_from_slice(&w16(*tag));
        raw[2..4].copy_from_slice(&w16(2));
        raw[4..8].copy_from_slice(&w32(text.len() as u32));
        if text.len() <= 4 {
            raw[8..8 + text.len()].copy_from_slice(&text);
        } else {
            if out.len() % 2 == 1 { out.push(0); }
            raw[8..12].copy_from_slice(&w32(out.len() as u32));
            out.extend_from_slice(&text);
        }
        entries.push(raw);
    }
    entries.sort_by_key(|raw| tag_of(raw));
    if out.len() % 2 == 1 { out.push(0); }
    let at = out.len() as u32;
    out.extend_from_slice(&w16(entries.len() as u16));
    for raw in &entries { out.extend_from_slice(raw); }
    out.extend_from_slice(&w32(next));
    out[4..8].copy_from_slice(&w32(at));
    Some(out)
}

fn jpeg_with(b: &[u8], tags: &[(&str, &str)]) -> Result<Vec<u8>, String> {
    if b.get(..2) != Some(&[0xFF, 0xD8]) { return Err("This JPEG could not be read".into()); }
    let mut exif_tags: Vec<(u16, &str)> = tags.iter().filter_map(|(k, v)| Some((exif_tag_of(k)?, *v))).collect();
    // Windows keeps its own copies of these and prefers them, which would go on showing the old value.
    for (k, _) in tags { match *k { "title" => exif_tags.push((0x9C9B, "")), "artist" => exif_tags.push((0x9C9D, "")), _ => {} } }
    let (mut i, mut insert_at, mut found) = (2, 2, None);
    while let (Some(0xFF), Some(&m)) = (b.get(i), b.get(i + 1)) {
        if m == 0xDA || m == 0xD9 { break; }
        let Some(len) = b.get(i + 2..i + 4).map(|l| u16::from_be_bytes([l[0], l[1]]) as usize) else { break };
        let Some(d) = b.get(i + 4..i + 2 + len) else { break };
        if m == 0xE0 { insert_at = i + 2 + len; }
        if m == 0xE1 && d.starts_with(b"Exif\0\0") && Tiff::new(&d[6..]).is_some() { found = Some((i, len)); break; }
        i += 2 + len;
    }
    let old = found.map(|(at, len)| &b[at + 10..at + 2 + len]);
    let tiff = tiff_with(old, &exif_tags).ok_or("This JPEG's details could not be read")?;
    let len = tiff.len() + 8;
    if len > 0xFFFF { return Err("These details are too long to fit in a JPEG".into()); }
    let mut seg = vec![0xFF, 0xE1];
    seg.extend((len as u16).to_be_bytes());
    seg.extend_from_slice(b"Exif\0\0");
    seg.extend(tiff);
    let (start, end) = match found { Some((at, l)) => (at, at + 2 + l), None => (insert_at, insert_at) };
    Ok([&b[..start], &seg[..], &b[end..]].concat())
}

pub fn crc32(parts: &[&[u8]]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for part in parts {
        for &x in *part {
            c ^= x as u32;
            for _ in 0..8 { c = if c & 1 == 1 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 }; }
        }
    }
    !c
}

fn png_with(b: &[u8], tags: &[(&str, &str)]) -> Result<Vec<u8>, String> {
    if b.get(..8) != Some(&[0x89, b'P', b'N', b'G', 13, 10, 26, 10]) { return Err("This PNG could not be read".into()); }
    let keys: Vec<(&str, &str)> = tags.iter().filter_map(|(k, v)| Some((png_key_of(k)?, *v))).collect();
    let mut out = b[..8].to_vec();
    let mut new_chunks = vec![];
    for (k, v) in &keys {
        if v.is_empty() { continue; }
        // Latin-1 text fits the plain chunk. Anything else takes the Unicode one.
        let (kind, mut body): (&[u8; 4], Vec<u8>) = if v.is_ascii() { (b"tEXt", k.as_bytes().to_vec()) } else { (b"iTXt", k.as_bytes().to_vec()) };
        body.push(0);
        if kind == b"iTXt" { body.extend([0, 0, 0, 0]); }
        body.extend_from_slice(v.as_bytes());
        let mut c = (body.len() as u32).to_be_bytes().to_vec();
        c.extend_from_slice(kind);
        c.extend_from_slice(&body);
        c.extend(crc32(&[kind, &body]).to_be_bytes());
        new_chunks.push(c);
    }
    let mut placed = false;
    let mut i = 8;
    while let Some(head) = b.get(i..i + 8) {
        let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let end = i + 12 + len;
        let chunk = b.get(i..end).ok_or("This PNG is cut short")?;
        let kind = &head[4..8];
        let replaced = matches!(kind, b"tEXt" | b"iTXt" | b"zTXt") && {
            let key = chunk[8..chunk.len() - 4].split(|c| *c == 0).next().unwrap_or(&[]);
            keys.iter().any(|(k, _)| k.as_bytes() == key)
        };
        if !placed && (kind == b"IDAT" || kind == b"IEND") {
            for c in &new_chunks { out.extend_from_slice(c); }
            placed = true;
        }
        if !replaced { out.extend_from_slice(chunk); }
        i = end;
    }
    Ok(out)
}

/// The file with these details changed, and nothing else touched. Only formats that offer `editable` get here.
pub fn write_image_tags(ext: &str, b: &[u8], tags: &[(&str, &str)]) -> Result<Vec<u8>, String> {
    match ext {
        "jpg" | "jpeg" => jpeg_with(b, tags),
        "png" => png_with(b, tags),
        _ => Err("Details in this format can only be removed".into()),
    }
}

// ---- the window ----

const WIDTH: f32 = 560.0;
const HEIGHT: f32 = 460.0;
const HEAD: f32 = 38.0;
const LABEL_W: f32 = 168.0;
const TEXT_SIZE: f32 = 12.5;

struct View {
    title: String,
    sub: String,
    info: Info,
    open: Vec<bool>,
    heights: HashMap<(usize, usize), f32>,
    content: f32,
    result: Option<bool>,
}

impl View {
    fn new(title: &str, sub: &str, info: Info) -> View {
        // Only the short version shows at first. The rest is a click away.
        let open = (0..info.sections.len()).map(|i| i == 0).collect();
        View { title: title.into(), sub: sub.into(), info, open, heights: HashMap::new(), content: 0.0, result: None }
    }
    fn rows(&mut self, ui: &mut Ui, s: usize, mut y: f32, view_bottom: f32, view_top: f32) -> f32 {
        let value_w = WIDTH - PAD * 2.0 - LABEL_W - 24.0;
        for i in 0..self.info.sections[s].rows.len() {
            let h = match self.heights.get(&(s, i)) {
                Some(h) => *h,
                None => {
                    let v = self.info.sections[s].rows[i].1.clone();
                    let h = ui.wrapped_height(&v, TEXT_SIZE, 400, value_w).max(17.0) + 7.0;
                    self.heights.insert((s, i), h);
                    h
                }
            };
            if y + h >= view_top && y <= view_bottom {
                let (k, v) = &self.info.sections[s].rows[i];
                let (k, v) = (k.clone(), v.clone());
                ui.text(&k, r(PAD + 12.0, y, LABEL_W - 8.0, 20.0), TEXT_SIZE, 400, MUTED, Align::Left);
                ui.wrapped(&v, r(PAD + 12.0 + LABEL_W, y + 2.0, value_w, h), TEXT_SIZE, 400, TEXT);
            }
            y += h;
        }
        y
    }
}

impl Page for View {
    fn size(&self) -> (f32, f32) { (WIDTH, HEIGHT) }
    fn done(&self) -> bool { self.result.is_some() }
    fn on_close(&mut self) { self.result = Some(false); }
    fn frame(&mut self, ui: &mut Ui) {
        if ui.titlebar(&self.title.clone(), &self.sub.clone()) { self.result = Some(false); }
        let view = r(0.0, TITLE_H, WIDTH, HEIGHT - TITLE_H - FOOT_H);
        let real_mouse = ui.mouse;
        let off = ui.scroll_area("meta.body", view, self.content);
        if !view.has(real_mouse) { ui.mouse = (-1000.0, -1000.0); }
        let top = TITLE_H + 6.0 - off;
        let mut y = top;
        for s in 0..self.info.sections.len() {
            let q = r(PAD, y, WIDTH - PAD * 2.0, HEAD - 4.0);
            let (hover, _, clicked) = ui.hit(id(&format!("meta.sec{s}")), q);
            if clicked { self.open[s] = !self.open[s]; }
            ui.fill(q, 8.0, if hover { TEXT } else { RAISED }, if hover { 0.08 } else { 0.7 });
            ui.icon(if self.open[s] { glyph::CHEVRON } else { 0xE76C }, r(q.x + 8.0, q.y, 20.0, q.h), 9.0, MUTED, 1.0);
            let (title, count) = (self.info.sections[s].title.clone(), self.info.sections[s].rows.len());
            ui.text(&title, r(q.x + 34.0, q.y, q.w - 100.0, q.h), 13.0, 600, TEXT, Align::Left);
            ui.text(&count.to_string(), r(q.right() - 62.0, q.y, 50.0, q.h), 12.0, 400, MUTED, Align::Right);
            y += HEAD;
            if self.open[s] { y = self.rows(ui, s, y + 2.0, view.bottom(), view.y) + 8.0; }
        }
        self.content = y - top + 8.0;
        ui.mouse = real_mouse;
        ui.unclip();
        // A thin bar, so that more below is plain to see.
        if self.content > view.h {
            let bar = view.h * view.h / self.content;
            let at = view.y + (view.h - bar) * off / (self.content - view.h).max(1.0);
            ui.fill(r(WIDTH - 7.0, at, 3.0, bar), 1.5, TEXT, 0.18);
        }
        let fy = HEIGHT - FOOT_H;
        ui.divider(fy, 0.0, WIDTH);
        let by = fy + (FOOT_H - 38.0) / 2.0;
        let primary = r(WIDTH - PAD - 120.0, by, 120.0, 38.0);
        if ui.button("meta.close", "Close", r(primary.x - 96.0, by, 88.0, 38.0), Style::Ghost, true) { self.result = Some(false); }
        if ui.button("meta.edit", "Edit", primary, Style::Primary, true) { self.result = Some(true); }
        if ui.key(VK_ESCAPE) { self.result = Some(false); }
    }
}

/// Shows the details and blocks. Returns the details again if the user asked to edit them.
pub fn show(title: &str, sub: &str, info: Info) -> (Info, bool) {
    match ui::run_modal(View::new(title, sub, info), title, None) {
        Some(v) => { let edit = v.result == Some(true); (v.info, edit) }
        None => (Info::default(), false),
    }
}

/// The window as a PNG with some sections open, for design review.
pub fn render(sub: &str, info: Info, open: &[usize], out: &Path) -> Result<(), String> {
    let mut v = View::new("Metadata", sub, info);
    for i in open { if let Some(o) = v.open.get_mut(*i) { *o = true; } }
    ui::render_png(&mut v, 1.0, None, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exif_jpeg() -> Vec<u8> {
        // An EXIF block with a make, an exposure in a sub-directory and a location, then an empty scan.
        let mut t = vec![b'I', b'I', 42, 0, 8, 0, 0, 0];
        t.extend([3, 0]);
        t.extend([0x0F, 0x01, 2, 0, 6, 0, 0, 0, 50, 0, 0, 0]); // Make at 50
        t.extend([0x69, 0x87, 4, 0, 1, 0, 0, 0, 56, 0, 0, 0]); // Exif directory at 56
        t.extend([0x25, 0x88, 4, 0, 1, 0, 0, 0, 82, 0, 0, 0]); // GPS directory at 82
        t.extend([0, 0, 0, 0]);
        assert_eq!(t.len(), 50);
        t.extend_from_slice(b"Canon\0");
        t.extend([1, 0]);
        t.extend([0x9A, 0x82, 5, 0, 1, 0, 0, 0, 74, 0, 0, 0]); // exposure at 74
        t.extend([0, 0, 0, 0]);
        t.extend([0, 0]);
        t.resize(74, 0);
        t.extend([1, 0, 0, 0, 250, 0, 0, 0]);
        t.extend([2, 0]);
        t.extend([1, 0, 2, 0, 2, 0, 0, 0, b'S', 0, 0, 0]);
        t.extend([2, 0, 5, 0, 3, 0, 0, 0, 112, 0, 0, 0]);
        t.extend([0, 0, 0, 0]);
        t.resize(112, 0);
        for (a, b) in [(34u32, 1u32), (30, 1), (0, 1)] { t.extend(a.to_le_bytes()); t.extend(b.to_le_bytes()); }
        let mut j = vec![0xFF, 0xD8, 0xFF, 0xE1];
        j.extend(((t.len() + 8) as u16).to_be_bytes());
        j.extend_from_slice(b"Exif\0\0");
        j.extend(t);
        j.extend([0xFF, 0xC0, 0, 11, 8, 0, 20, 0, 40, 3, 1, 0x22, 0]);
        j.extend([0xFF, 0xDA, 0, 2, 0xAB, 0xCD, 0xFF, 0xD9]);
        j
    }
    fn parsed(j: &[u8]) -> (Vec<Section>, Vec<(String, String)>) {
        let (mut s, mut sm) = (vec![], vec![]);
        read_jpeg(j, &mut s, &mut sm);
        (s, sm)
    }

    #[test]
    fn reads_every_exif_directory() {
        let (s, sm) = parsed(&exif_jpeg());
        assert_eq!(sm, vec![row("Dimensions", "40 x 20")]);
        assert_eq!(find_row(&s, "Image", "Camera make"), Some("Canon"));
        assert_eq!(find_row(&s, "Exposure", "Exposure time"), Some("1/250"));
        assert_eq!(find_row(&s, "GPS", "Latitude reference"), Some("S"));
        assert_eq!(gps_position(&s).is_none(), true, "no longitude in this one");
        assert_eq!(find_row(&s, "GPS", "Latitude"), Some("34, 30, 0"));
    }
    #[test]
    fn location_reads_as_signed_degrees() {
        let s = vec![section("GPS", vec![row("Latitude", "34, 30, 0"), row("Latitude reference", "S"), row("Longitude", "58, 15, 0"), row("Longitude reference", "W")])];
        assert_eq!(gps_position(&s).unwrap(), "-34.50000, -58.25000");
    }
    #[test]
    fn malformed_exif_never_panics() {
        let good = exif_jpeg();
        for cut in 0..good.len() { let _ = parsed(&good[..cut]); }
        for at in 0..good.len() { let mut bad = good.clone(); bad[at] = 0xFF; let _ = parsed(&bad); }
    }
    #[test]
    fn editing_a_jpeg_keeps_everything_else() {
        let j = exif_jpeg();
        let out = write_image_tags("jpg", &j, &[("title", "A long enough title"), ("artist", "Me"), ("copyright", "")]).unwrap();
        let (s, _) = parsed(&out);
        assert_eq!(find_row(&s, "Image", "Description"), Some("A long enough title"));
        assert_eq!(find_row(&s, "Image", "Artist"), Some("Me"), "short enough to sit in the entry");
        assert_eq!(find_row(&s, "Image", "Camera make"), Some("Canon"));
        assert_eq!(find_row(&s, "Exposure", "Exposure time"), Some("1/250"));
        assert!(out.ends_with(&[0xFF, 0xC0, 0, 11, 8, 0, 20, 0, 40, 3, 1, 0x22, 0, 0xFF, 0xDA, 0, 2, 0xAB, 0xCD, 0xFF, 0xD9]), "the picture is untouched");
        // Clearing one takes it back out.
        let out = write_image_tags("jpg", &out, &[("title", "")]).unwrap();
        let (s, _) = parsed(&out);
        assert_eq!(find_row(&s, "Image", "Description"), None);
        assert_eq!(find_row(&s, "Image", "Artist"), Some("Me"));
    }
    #[test]
    fn a_jpeg_with_no_exif_gets_some_after_jfif() {
        let mut j = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 16];
        j.extend_from_slice(b"JFIF\0\x01\x01\x01\0\x48\0\x48\0\0");
        let pic = [0xFF, 0xDA, 0, 2, 0xFF, 0xD9];
        j.extend(pic);
        let out = write_image_tags("jpg", &j, &[("artist", "Someone else")]).unwrap();
        assert_eq!(&out[..j.len() - pic.len()], &j[..j.len() - pic.len()]);
        assert_eq!(out[j.len() - pic.len() + 1], 0xE1);
        assert_eq!(find_row(&parsed(&out).0, "Image", "Artist"), Some("Someone else"));
    }
    #[test]
    fn crc_matches_the_png_standard() {
        assert_eq!(crc32(&[b"IEND"]), 0xAE42_6082);
    }
    fn tiny_png() -> Vec<u8> {
        let mut p = vec![0x89, b'P', b'N', b'G', 13, 10, 26, 10];
        let chunk = |kind: &[u8; 4], d: &[u8]| {
            let mut c = (d.len() as u32).to_be_bytes().to_vec();
            c.extend_from_slice(kind);
            c.extend_from_slice(d);
            c.extend(crc32(&[kind, d]).to_be_bytes());
            c
        };
        p.extend(chunk(b"IHDR", &[0, 0, 0, 2, 0, 0, 0, 3, 8, 6, 0, 0, 0]));
        p.extend(chunk(b"tEXt", b"Title\0old"));
        p.extend(chunk(b"tEXt", b"Software\0kept"));
        p.extend(chunk(b"IDAT", &[1, 2, 3]));
        p.extend(chunk(b"IEND", &[]));
        p
    }
    #[test]
    fn editing_a_png_replaces_only_what_changed() {
        let out = write_image_tags("png", &tiny_png(), &[("title", "new"), ("artist", "José")]).unwrap();
        let (mut s, mut sm) = (vec![], vec![]);
        read_png(&out, &mut s, &mut sm);
        assert_eq!(find_row(&s, "Text", "Title"), Some("new"));
        assert_eq!(find_row(&s, "Text", "Author"), Some("José"));
        assert_eq!(find_row(&s, "Text", "Software"), Some("kept"));
        assert_eq!(sm, vec![row("Dimensions", "2 x 3")]);
        assert!(out.windows(3).any(|w| w == [1, 2, 3]));
        assert!(out.windows(4).position(|w| w == b"iTXt") < out.windows(4).position(|w| w == b"IDAT"), "text comes before the picture");
    }
    #[test]
    fn xmp_gives_its_leaves() {
        let rows = from_xmp(r#"<?xpacket?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF><rdf:Description rdf:about="" xmlns:dc="d" xmp:CreatorTool="Lightroom &amp; co"><dc:title><rdf:Alt><rdf:li xml:lang="x-default">Beach</rdf:li></rdf:Alt></dc:title><dc:subject><rdf:Bag><rdf:li>sea</rdf:li><rdf:li>sun</rdf:li></rdf:Bag></dc:subject></rdf:Description></rdf:RDF></x:xmpmeta>"#);
        assert!(rows.contains(&row("Creator tool", "Lightroom & co")), "{rows:?}");
        assert!(rows.contains(&row("Title", "Beach")), "{rows:?}");
        assert!(rows.contains(&row("Subject", "sea")) && rows.contains(&row("Subject", "sun")), "{rows:?}");
    }
    #[test]
    fn probe_output_folds_into_sections() {
        let flat = r#"streams.stream.0.codec_name="h264"
streams.stream.0.codec_type="video"
streams.stream.0.width=1920
streams.stream.0.height=1080
streams.stream.0.avg_frame_rate="30000/1001"
streams.stream.0.disposition.default=1
streams.stream.0.disposition.dub=0
streams.stream.0.tags.language="eng"
streams.stream.0.start_pts="N/A"
streams.stream.1.codec_name="aac"
streams.stream.1.codec_type="audio"
streams.stream.1.channels=2
streams.stream.1.sample_rate="48000"
chapters.chapter.0.start_time="0.000000"
chapters.chapter.0.tags.title="Intro"
format.format_long_name="QuickTime / MOV"
format.duration="65.500000"
format.bit_rate="2000000"
format.tags.title="A \"quoted\" name""#;
        let (s, tags) = from_probe(flat);
        let names: Vec<&str> = s.iter().map(|x| x.title.as_str()).collect();
        assert_eq!(names, ["Summary", "Tags", "Video 1", "Audio 1", "Chapters", "Container"]);
        assert_eq!(tags["title"], "A \"quoted\" name");
        assert_eq!(find_row(&s, "Summary", "Length"), Some("1:05.50"));
        assert_eq!(find_row(&s, "Summary", "Video"), Some("h264, 1920x1080, 29.97 fps"));
        assert_eq!(find_row(&s, "Summary", "Audio"), Some("aac, 2 channels, 48000 Hz"));
        assert_eq!(find_row(&s, "Video 1", "Language"), Some("eng"));
        assert_eq!(find_row(&s, "Video 1", "Disposition default"), Some("1"));
        assert_eq!(find_row(&s, "Video 1", "Disposition dub"), None, "a flag that is off is noise");
        assert_eq!(find_row(&s, "Video 1", "Start pts"), None);
        assert_eq!(find_row(&s, "Chapters", "1"), Some("0:00.00  Intro"));
    }
    #[test]
    fn numbers_read_as_people_write_them() {
        assert_eq!(fraction(1.0, 250.0), "1/250");
        assert_eq!(fraction(28.0, 10.0), "2.8");
        assert_eq!(fraction(72.0, 1.0), "72");
        assert_eq!(pdf_date("D:20240131120500+01'00'"), "2024-01-31 12:05:00");
        assert_eq!(date_time(0), "1970-01-01 00:00 UTC");
        assert_eq!(date_time(1_700_000_000), "2023-11-14 22:13 UTC");
    }
}
