//! What each wheel offers for a given selection. Pure data, no Win32.
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Cat { Image, Video, Audio, Pdf, Doc, Slides, Sheet, Epub, Sub, Archive, Folder, Other }

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind { Convert, Tools, Actions }

impl Kind {
    pub fn title(self) -> &'static str {
        match self { Kind::Convert => "Convert", Kind::Tools => "Tools", Kind::Actions => "Actions" }
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: &'static str,
    pub label: &'static str,
    pub icon: u16,
    /// Why this entry cannot run right now; drawn dimmed when set.
    pub blocked: Option<&'static str>,
}

/// Which optional engines exist on this machine.
#[derive(Clone, Copy, Default, Debug)]
pub struct Engines { pub ffmpeg: bool, pub word: bool, pub excel: bool, pub powerpoint: bool, pub edge: bool, pub tar: bool }

pub fn ext_of(p: &Path) -> String {
    p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

pub fn classify(p: &Path) -> Cat {
    if p.is_dir() { return Cat::Folder; }
    match ext_of(p).as_str() {
        "jpg" | "jpeg" | "png" | "webp" | "heic" | "heif" | "tif" | "tiff" | "avif" | "bmp" | "svg" => Cat::Image,
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "wmv" | "gif" | "m4v"
        // Camcorder and disc formats: AVCHD and Blu-ray transport streams, then MPEG-2 program streams.
        | "mts" | "m2ts" | "m2t" | "mod" | "tod" | "vob" | "mpg" | "mpeg" => Cat::Video,
        "mp3" | "m4a" | "wav" | "flac" | "ogg" | "opus" | "aiff" | "aif" | "wma" | "aac" => Cat::Audio,
        "pdf" => Cat::Pdf,
        "docx" | "doc" | "md" | "markdown" | "txt" | "rtf" | "html" | "htm" | "odt" => Cat::Doc,
        "ppt" | "pptx" => Cat::Slides,
        "xlsx" | "xls" | "ods" | "csv" | "tsv" => Cat::Sheet,
        "epub" => Cat::Epub,
        "srt" | "vtt" => Cat::Sub,
        "zip" | "tar" | "gz" | "tgz" | "rar" | "7z" => Cat::Archive,
        _ => Cat::Other,
    }
}

/// The category that decides the wheel: the most common one in the selection.
pub fn dominant(paths: &[PathBuf]) -> Cat {
    let mut best = (Cat::Other, 0usize);
    for p in paths {
        let c = classify(p);
        let n = paths.iter().filter(|q| classify(q) == c).count();
        if n > best.1 && c != Cat::Other { best = (c, n); }
    }
    best.0
}

fn fmt(id: &'static str, label: &'static str) -> Entry { Entry { id, label, icon: 0, blocked: None } }
fn tool(id: &'static str, label: &'static str, icon: u16) -> Entry { Entry { id, label, icon, blocked: None } }

const NEED_FFMPEG: &str = "Needs ffmpeg";
const NEED_WORD: &str = "Needs Word";
const NEED_EXCEL: &str = "Needs Excel";
const NEED_PPT: &str = "Needs PowerPoint";
const NEED_EDGE: &str = "Needs Edge";

/// Target formats and which engine each one depends on, for one source extension.
fn convert_targets(cat: Cat, src_ext: &str, e: &Engines) -> Vec<Entry> {
    let needs = |mut en: Entry, ok: bool, why: &'static str| { if !ok { en.blocked = Some(why); } en };
    let list: Vec<Entry> = match cat {
        Cat::Image => vec![
            fmt("jpg", "JPG"), fmt("png", "PNG"),
            needs(fmt("webp", "WEBP"), e.ffmpeg, NEED_FFMPEG),
            needs(fmt("heic", "HEIC"), e.ffmpeg, NEED_FFMPEG), fmt("tiff", "TIFF"),
            needs(fmt("avif", "AVIF"), e.ffmpeg, NEED_FFMPEG),
            fmt("bmp", "BMP"), fmt("pdf", "PDF"),
        ],
        Cat::Video => ["mp4", "mov", "mkv", "webm", "avi", "wmv", "gif", "mp3"].iter().zip(
            ["MP4", "MOV", "MKV", "WEBM", "AVI", "WMV", "GIF", "MP3"].iter())
            .map(|(i, l)| needs(fmt(i, l), e.ffmpeg, NEED_FFMPEG)).collect(),
        Cat::Audio => ["mp3", "m4a", "wav", "flac", "ogg", "opus", "aiff", "wma"].iter().zip(
            ["MP3", "M4A", "WAV", "FLAC", "OGG", "OPUS", "AIFF", "WMA"].iter())
            .map(|(i, l)| needs(fmt(i, l), e.ffmpeg, NEED_FFMPEG)).collect(),
        Cat::Pdf => vec![
            needs(fmt("docx", "DOCX"), e.word, NEED_WORD),
            needs(fmt("pptx", "PPTX"), e.powerpoint, NEED_PPT),
            needs(fmt("xlsx", "XLSX"), e.excel, NEED_EXCEL),
            fmt("jpg", "JPG"), fmt("png", "PNG"), fmt("epub", "EPUB"), fmt("txt", "TXT"), fmt("md", "MD"),
        ],
        Cat::Doc => {
            let is_word = matches!(src_ext, "docx" | "doc" | "rtf" | "odt");
            let w = |en: Entry| if is_word { needs(en, e.word, NEED_WORD) } else { en };
            let pdf_ok = if is_word { e.word } else { e.edge };
            let pdf_why = if is_word { NEED_WORD } else { NEED_EDGE };
            vec![
                needs(fmt("docx", "DOCX"), e.word, NEED_WORD),
                needs(fmt("pdf", "PDF"), pdf_ok, pdf_why),
                w(fmt("txt", "TXT")), w(fmt("md", "MD")), w(fmt("html", "HTML")), w(fmt("epub", "EPUB")),
                needs(fmt("png", "PNG"), pdf_ok, pdf_why),
                needs(fmt("jpg", "JPG"), pdf_ok, pdf_why),
            ]
        }
        Cat::Slides => vec![
            needs(fmt("pdf", "PDF"), e.powerpoint, NEED_PPT),
            needs(fmt("jpg", "JPG"), e.powerpoint, NEED_PPT),
            needs(fmt("png", "PNG"), e.powerpoint, NEED_PPT),
            needs(fmt("pptx", "PPTX"), e.powerpoint, NEED_PPT),
        ],
        Cat::Sheet => {
            let text = matches!(src_ext, "csv" | "tsv");
            vec![
                needs(fmt("xlsx", "XLSX"), e.excel, NEED_EXCEL),
                needs(fmt("ods", "ODS"), e.excel, NEED_EXCEL),
                if text { fmt("csv", "CSV") } else { needs(fmt("csv", "CSV"), e.excel, NEED_EXCEL) },
                if text { fmt("tsv", "TSV") } else { needs(fmt("tsv", "TSV"), e.excel, NEED_EXCEL) },
                needs(fmt("pdf", "PDF"), e.excel, NEED_EXCEL),
            ]
        }
        Cat::Epub => vec![
            needs(fmt("pdf", "PDF"), e.edge, NEED_EDGE),
            needs(fmt("docx", "DOCX"), e.word, NEED_WORD),
            fmt("txt", "TXT"), fmt("md", "MD"), fmt("html", "HTML"),
        ],
        Cat::Sub => vec![fmt("srt", "SRT"), fmt("vtt", "VTT"), fmt("txt", "TXT")],
        Cat::Archive | Cat::Folder => vec![fmt("zip", "ZIP"), fmt("tar", "TAR"), fmt("gz", "GZIP")],
        Cat::Other => vec![],
    };
    let same = |id: &str| id == src_ext || (id == "jpg" && src_ext == "jpeg") || (id == "tiff" && src_ext == "tif")
        || (id == "md" && src_ext == "markdown") || (id == "html" && src_ext == "htm") || (id == "aiff" && src_ext == "aif")
        || (id == "heic" && src_ext == "heif");
    list.into_iter().filter(|en| !same(en.id)).collect()
}

fn tool_entries(cat: Cat, count: usize, e: &Engines) -> Vec<Entry> {
    let ff = |mut en: Entry| { if !e.ffmpeg { en.blocked = Some(NEED_FFMPEG); } en };
    match cat {
        Cat::Image => {
            let mut v = vec![
                tool("img.compress", "COMPRESS", 0xE73F), tool("img.metadata", "METADATA", 0xE8EC),
                ff(tool("img.edit", "EDIT", 0xE790)), tool("img.annotate", "ANNOTATE", 0xE70F),
                tool("img.background", "ADD BG", 0xE8A9), tool("img.crop", "CROP", 0xE7A8),
                tool("img.redact", "REDACT", 0xED1A),
            ];
            if count > 1 { v.push(tool("img.collage", "COLLAGE", 0xECA5)); }
            v
        }
        Cat::Video => {
            let mut v = vec![
                ff(tool("vid.compress", "COMPRESS", 0xE73F)), ff(tool("vid.metadata", "METADATA", 0xE8EC)),
                ff(tool("vid.mute", "MUTE", 0xE74F)), ff(tool("vid.trim", "TRIM", 0xE8C6)),
                ff(tool("vid.crop", "CROP", 0xE7A8)), ff(tool("vid.speed", "SPEED", 0xEC4A)),
                ff(tool("vid.snapshot", "SNAPSHOT", 0xE722)), ff(tool("vid.split", "SPLIT", 0xE8A4)),
                ff(tool("vid.redact", "REDACT", 0xED1A)),
            ];
            // Nine nodes is the limit that stays readable, so JOIN replaces SPLIT for a selection.
            if count > 1 { v[7] = ff(tool("vid.join", "JOIN", 0xE71B)); }
            v
        }
        Cat::Audio => vec![
            ff(tool("aud.compress", "COMPRESS", 0xE73F)), ff(tool("aud.metadata", "METADATA", 0xE8EC)),
            ff(tool("aud.normalize", "NORMALIZE", 0xE9E9)), ff(tool("aud.visualize", "VISUALIZE", 0xE714)),
            ff(tool("aud.trim", "TRIM", 0xE8C6)), ff(tool("aud.channels", "CHANNELS", 0xE767)),
            ff(tool("aud.bleep", "BLEEP", 0xE720)),
        ],
        Cat::Pdf => {
            let mut v = vec![
                tool("pdf.compress", "COMPRESS", 0xE73F), tool("pdf.metadata", "METADATA", 0xE8EC),
                tool("pdf.split", "SPLIT", 0xE8A4), tool("pdf.organize", "ORGANIZE", 0xE8CB),
                tool("pdf.redact", "REDACT", 0xED1A),
            ];
            if count > 1 { v.push(tool("pdf.merge", "MERGE", 0xE71B)); }
            v
        }
        Cat::Archive => vec![tool("arc.extract", "EXTRACT", 0xE8B7)],
        _ => vec![],
    }
}

pub fn action_entries() -> Vec<Entry> {
    vec![
        tool("act.shelf", "SHELF", 0xE718),
        tool("act.copy", "COPY", 0xE8C8), tool("act.move", "MOVE", 0xE8DE),
        tool("act.path", "COPY PATH", 0xE71B), tool("act.terminal", "TERMINAL", 0xE756),
    ]
}

pub fn entries(kind: Kind, paths: &[PathBuf], e: &Engines) -> Vec<Entry> {
    let cat = dominant(paths);
    match kind {
        Kind::Actions => action_entries(),
        Kind::Tools => tool_entries(cat, paths.len(), e),
        Kind::Convert => {
            let src = paths.iter().find(|p| classify(p) == cat).map(|p| ext_of(p)).unwrap_or_default();
            let mut t = convert_targets(cat, &src, e);
            t.truncate(9);
            t
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn all() -> Engines { Engines { ffmpeg: true, word: true, excel: true, powerpoint: true, edge: true, tar: true } }
    fn p(s: &str) -> Vec<PathBuf> { vec![PathBuf::from(s)] }

    #[test]
    fn hides_the_source_format() {
        let ids: Vec<_> = entries(Kind::Convert, &p("a.JPEG"), &all()).iter().map(|e| e.id).collect();
        assert!(!ids.contains(&"jpg"));
        assert!(ids.contains(&"png"));
    }
    #[test]
    fn missing_engine_blocks_instead_of_hiding() {
        let en = entries(Kind::Convert, &p("a.mp4"), &Engines::default());
        assert!(!en.is_empty());
        assert!(en.iter().all(|e| e.blocked.is_some()));
    }
    #[test]
    fn unknown_type_only_has_actions() {
        assert!(entries(Kind::Convert, &p("a.xyz"), &all()).is_empty());
        assert!(entries(Kind::Tools, &p("a.xyz"), &all()).is_empty());
        assert_eq!(entries(Kind::Actions, &p("a.xyz"), &all()).len(), 5);
    }
    #[test]
    fn multi_select_adds_batch_tools() {
        let two = vec![PathBuf::from("a.pdf"), PathBuf::from("b.pdf")];
        assert!(entries(Kind::Tools, &two, &all()).iter().any(|e| e.id == "pdf.merge"));
        assert!(!entries(Kind::Tools, &p("a.pdf"), &all()).iter().any(|e| e.id == "pdf.merge"));
    }
    #[test]
    fn no_wheel_exceeds_nine_nodes() {
        for f in ["a.jpg", "a.mp4", "a.mp3", "a.pdf", "a.docx", "a.pptx", "a.xlsx", "a.epub", "a.srt", "a.zip"] {
            let two = vec![PathBuf::from(f), PathBuf::from(f)];
            for k in [Kind::Convert, Kind::Tools, Kind::Actions] {
                assert!(entries(k, &two, &all()).len() <= 9, "{f} {k:?}");
            }
        }
    }
}
