//! Documents, slides, sheets, EPUB, subtitles and archives.
//! Office work is driven through Windows PowerShell COM scripts: the app stays small and Office is the slow part anyway.
use crate::catalog::ext_of;
use crate::job::*;
use std::path::{Path, PathBuf};

fn rerr(e: std::io::Error) -> String { e.to_string() }
fn stem(p: &Path) -> String { p.file_stem().and_then(|s| s.to_str()).unwrap_or("Document").to_string() }

pub fn read_text(p: &Path) -> Result<String, String> {
    let b = std::fs::read(p).map_err(rerr)?;
    if b.starts_with(&[0xFF, 0xFE]) {
        let u: Vec<u16> = b[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return Ok(String::from_utf16_lossy(&u));
    }
    let b = b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&b);
    Ok(String::from_utf8_lossy(b).to_string())
}

// ---- Office ----

const WORD_CONVERT: &str = r#"
$w = New-Object -ComObject Word.Application
$w.Visible = $false; $w.DisplayAlerts = 0
try {
  # Word answers "Command failed" while it is still busy with the last document, so give it a few tries.
  for ($i = 1; ; $i++) {
    try {
      $d = $w.Documents.Open($env:OR_IN, $false, $true, $false)
      $d.SaveAs2($env:OR_OUT, [int]$env:OR_FMT)
      $d.Close(0)
      break
    } catch {
      if ($i -ge 3) { throw }
      Start-Sleep -Seconds 2
    }
  }
} finally { $w.Quit() }
"#;

/// Word file-format numbers: 12 docx, 17 pdf, 2 text, 10 filtered html, 6 rtf.
fn word(job: &Job, input: &Path, out: &Path, fmt: u32) -> Result<(), String> {
    powershell(job, WORD_CONVERT, &[("OR_IN", input.display().to_string()), ("OR_OUT", out.display().to_string()), ("OR_FMT", fmt.to_string())]).map(|_| ())
}

const EXCEL_CONVERT: &str = r#"
$x = New-Object -ComObject Excel.Application
$x.Visible = $false; $x.DisplayAlerts = $false
try {
  $b = $x.Workbooks.Open($env:OR_IN, 0, $true)
  if ($env:OR_FMT -eq 'pdf') { $b.ExportAsFixedFormat(0, $env:OR_OUT) } else { $b.SaveAs($env:OR_OUT, [int]$env:OR_FMT) }
  $b.Close($false)
} finally { $x.Quit() }
"#;

const SLIDES_CONVERT: &str = r#"
$p = New-Object -ComObject PowerPoint.Application
try {
  $d = $p.Presentations.Open($env:OR_IN, -1, 0, 0)
  $d.SaveAs($env:OR_OUT, [int]$env:OR_FMT)
  $d.Close()
} finally { $p.Quit() }
"#;

const PDF_TO_SLIDES: &str = r#"
$p = New-Object -ComObject PowerPoint.Application
try {
  $d = $p.Presentations.Add(0)
  $files = Get-ChildItem -LiteralPath $env:OR_IN | Sort-Object Name
  foreach ($f in $files) {
    $i = $d.Slides.Count + 1
    $s = $d.Slides.Add($i, 12)
    $s.Shapes.AddPicture($f.FullName, 0, -1, 0, 0, $d.PageSetup.SlideWidth, $d.PageSetup.SlideHeight) | Out-Null
  }
  $d.SaveAs($env:OR_OUT, 24)
  $d.Close()
} finally { $p.Quit() }
"#;

// ---- markup ----

pub fn md_to_html(md: &str) -> String {
    let parser = pulldown_cmark::Parser::new_ext(md, pulldown_cmark::Options::ENABLE_TABLES | pulldown_cmark::Options::ENABLE_STRIKETHROUGH);
    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, parser);
    out
}

pub fn escape(s: &str) -> String { s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;") }

pub fn text_to_html(t: &str) -> String {
    t.split("\n\n").map(|p| format!("<p>{}</p>\n", escape(p.trim()).replace('\n', "<br/>"))).collect()
}

pub fn page(title: &str, body: &str) -> String {
    format!("<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>{}</title><style>body{{font:16px/1.55 'Segoe UI',sans-serif;max-width:46em;margin:2em auto;padding:0 1em}}img{{max-width:100%}}table{{border-collapse:collapse}}td,th{{border:1px solid #bbb;padding:.3em .6em}}</style></head><body>\n{}\n</body></html>\n", escape(title), body)
}

fn decode_entities(s: &str) -> String {
    let mut out = s.replace("&nbsp;", " ").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&apos;", "'");
    while let Some(i) = out.find("&#") {
        let Some(j) = out[i..].find(';') else { break };
        let code = &out[i + 2..i + j];
        let v = if let Some(h) = code.strip_prefix(['x', 'X']) { u32::from_str_radix(h, 16).ok() } else { code.parse().ok() };
        let c = v.and_then(char::from_u32).unwrap_or(' ');
        out.replace_range(i..i + j + 1, &c.to_string());
    }
    out.replace("&amp;", "&")
}

/// HTML to Markdown (or plain text when `md` is false). Handles headings, lists, emphasis, links and paragraphs.
pub fn html_to_text(html: &str, md: bool) -> String {
    let mut out = String::new();
    let mut rest = html;
    let mut skip = 0;
    let mut href: Vec<String> = vec![];
    while let Some(i) = rest.find('<') {
        if skip == 0 {
            let t = rest[..i].split_whitespace().collect::<Vec<_>>().join(" ");
            if !t.is_empty() {
                if rest[..i].starts_with(char::is_whitespace) && !out.ends_with(['\n', ' ']) { out.push(' '); }
                out.push_str(&decode_entities(&t));
                if rest[..i].ends_with(char::is_whitespace) { out.push(' '); }
            }
        }
        let Some(j) = rest[i..].find('>') else { rest = ""; break };
        let raw = &rest[i + 1..i + j];
        rest = &rest[i + j + 1..];
        let closing = raw.starts_with('/');
        let name = raw.trim_start_matches('/').split([' ', '/', '\n', '\t']).next().unwrap_or("").to_ascii_lowercase();
        match name.as_str() {
            "script" | "style" | "head" | "title" => { if closing { skip = (skip as i32 - 1).max(0) as usize; } else if !raw.ends_with('/') { skip += 1; } }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" if skip == 0 => {
                if closing { out.push_str("\n\n"); } else {
                    out.push_str("\n\n");
                    if md { out.push_str(&"#".repeat(name[1..].parse().unwrap_or(1))); out.push(' '); }
                }
            }
            "p" | "div" | "tr" | "section" | "article" | "blockquote" | "ul" | "ol" | "table" if skip == 0 => {
                if !out.ends_with("\n\n") { out.push_str(if out.ends_with('\n') { "\n" } else { "\n\n" }); }
            }
            "br" | "hr" if skip == 0 => out.push('\n'),
            "li" if skip == 0 && !closing => { if !out.ends_with('\n') { out.push('\n'); } out.push_str("- "); }
            "td" | "th" if skip == 0 && closing => out.push_str(if md { " | " } else { "\t" }),
            "b" | "strong" if skip == 0 && md => out.push_str("**"),
            "i" | "em" if skip == 0 && md => out.push('*'),
            "a" if skip == 0 && md => {
                if closing {
                    if let Some(h) = href.pop() { out.push_str(&format!("]({h})")); }
                } else if let Some(p) = raw.find("href=\"") {
                    let v = &raw[p + 6..];
                    href.push(v[..v.find('"').unwrap_or(v.len())].to_string());
                    out.push('[');
                }
            }
            _ => {}
        }
    }
    if skip == 0 { out.push_str(&decode_entities(rest)); }
    let mut cleaned = String::new();
    let mut blanks = 0;
    for l in out.lines() {
        let l = l.trim_end();
        if l.is_empty() { blanks += 1; if blanks == 1 && !cleaned.is_empty() { cleaned.push('\n'); } } else { blanks = 0; cleaned.push_str(l); cleaned.push('\n'); }
    }
    cleaned
}

// ---- edge headless, for HTML to PDF with no Office ----

fn edge_exe() -> Option<PathBuf> {
    for base in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Ok(b) = std::env::var(base) {
            let p = PathBuf::from(b).join(r"Microsoft\Edge\Application\msedge.exe");
            if p.exists() { return Some(p); }
        }
    }
    None
}
pub fn has_edge() -> bool { edge_exe().is_some() }

/// A file URL with the characters that would end or corrupt the path percent-encoded.
pub fn file_url(p: &Path) -> String {
    let s = p.display().to_string().replace('\\', "/").replace('%', "%25").replace(' ', "%20").replace('#', "%23").replace('?', "%3F");
    format!("file:///{s}")
}

fn html_to_pdf(job: &Job, html: &Path, out: &Path) -> Result<(), String> {
    let edge = edge_exe().ok_or("Needs Edge")?;
    let t = TempDir::new()?;
    let url = file_url(html);
    let a = vec!["--headless".into(), "--disable-gpu".into(), "--no-pdf-header-footer".into(),
        format!("--user-data-dir={}", t.0.display()), format!("--print-to-pdf={}", out.display()), url];
    run(Some(job), &edge, &a, None)?;
    for _ in 0..50 { if out.exists() { return Ok(()); } std::thread::sleep(std::time::Duration::from_millis(100)); }
    Err("Edge produced no PDF".into())
}

// ---- store-only zip, for EPUB ----

fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 { c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 }; }
    }
    !c
}

/// A valid zip with every entry stored. EPUB requires `mimetype` first and uncompressed.
pub fn zip_store(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let (mut out, mut central) = (vec![], vec![]);
    let le16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_le_bytes());
    let le32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    for (name, data) in entries {
        let off = out.len() as u32;
        let crc = crc32(data);
        le32(&mut out, 0x0403_4B50);
        for x in [20u16, 0x0800, 0, 0, 0x21] { le16(&mut out, x); }
        le32(&mut out, crc); le32(&mut out, data.len() as u32); le32(&mut out, data.len() as u32);
        le16(&mut out, name.len() as u16); le16(&mut out, 0);
        out.extend_from_slice(name.as_bytes()); out.extend_from_slice(data);
        le32(&mut central, 0x0201_4B50);
        for x in [20u16, 20, 0x0800, 0, 0, 0x21] { le16(&mut central, x); }
        le32(&mut central, crc); le32(&mut central, data.len() as u32); le32(&mut central, data.len() as u32);
        le16(&mut central, name.len() as u16);
        for x in [0u16, 0, 0, 0] { le16(&mut central, x); }
        le32(&mut central, 0); le32(&mut central, off);
        central.extend_from_slice(name.as_bytes());
    }
    let cd_off = out.len() as u32;
    out.extend_from_slice(&central);
    le32(&mut out, 0x0605_4B50);
    for x in [0u16, 0, entries.len() as u16, entries.len() as u16] { le16(&mut out, x); }
    le32(&mut out, central.len() as u32); le32(&mut out, cd_off); le16(&mut out, 0);
    out
}

pub fn write_epub(title: &str, html_body: &str, out: &Path) -> Result<(), String> {
    let xhtml = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title>{0}</title></head><body>{1}</body></html>", escape(title), to_xhtml(html_body));
    let opf = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"id\"><metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:identifier id=\"id\">orrery-{1}</dc:identifier><dc:title>{0}</dc:title><dc:language>en</dc:language><meta property=\"dcterms:modified\">2000-01-01T00:00:00Z</meta></metadata><manifest><item id=\"c\" href=\"c.xhtml\" media-type=\"application/xhtml+xml\"/><item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/></manifest><spine><itemref idref=\"c\"/></spine></package>", escape(title), crc32(title.as_bytes()));
    let nav = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\"><head><title>Contents</title></head><body><nav epub:type=\"toc\"><ol><li><a href=\"c.xhtml\">{}</a></li></ol></nav></body></html>", escape(title));
    let container = "<?xml version=\"1.0\"?>\n<container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\"><rootfiles><rootfile full-path=\"OEBPS/content.opf\" media-type=\"application/oebps-package+xml\"/></rootfiles></container>";
    let z = zip_store(&[
        ("mimetype", b"application/epub+zip".to_vec()),
        ("META-INF/container.xml", container.as_bytes().to_vec()),
        ("OEBPS/content.opf", opf.into_bytes()),
        ("OEBPS/nav.xhtml", nav.into_bytes()),
        ("OEBPS/c.xhtml", xhtml.into_bytes()),
    ]);
    std::fs::write(out, z).map_err(rerr)
}

fn to_xhtml(h: &str) -> String { h.replace("<br>", "<br/>").replace("<hr>", "<hr/>").replace("&nbsp;", "&#160;") }

/// Extracts an EPUB and joins its spine into one HTML body. Returns the temp folder so images stay resolvable.
fn epub_to_html(input: &Path) -> Result<(TempDir, PathBuf, String), String> {
    let t = TempDir::new()?;
    extract_to(None, input, &t.0)?;
    let container = read_text(&t.0.join("META-INF/container.xml"))?;
    let opf_rel = attr_after(&container, "full-path=\"").ok_or("Not a valid EPUB")?;
    let opf_path = t.0.join(&opf_rel);
    let opf = read_text(&opf_path)?;
    let base = opf_path.parent().unwrap_or(&t.0).to_path_buf();
    let mut manifest = std::collections::HashMap::new();
    for item in opf.split("<item ").skip(1) {
        let head = &item[..item.find('>').unwrap_or(item.len())];
        if let (Some(id), Some(href)) = (attr_after(head, "id=\""), attr_after(head, "href=\"")) { manifest.insert(id, href); }
    }
    let mut body = String::new();
    for r in opf.split("<itemref ").skip(1) {
        let head = &r[..r.find('>').unwrap_or(r.len())];
        let Some(id) = attr_after(head, "idref=\"") else { continue };
        let Some(href) = manifest.get(&id) else { continue };
        let Ok(html) = read_text(&base.join(href.replace("%20", " "))) else { continue };
        let lower = html.to_ascii_lowercase();
        let start = lower.find("<body").and_then(|i| lower[i..].find('>').map(|j| i + j + 1)).unwrap_or(0);
        let end = lower.rfind("</body>").unwrap_or(html.len());
        body.push_str(&html[start..end.max(start)]);
        body.push('\n');
    }
    if body.trim().is_empty() { return Err("This EPUB has no readable chapters".into()); }
    Ok((t, base, body))
}

fn attr_after(s: &str, key: &str) -> Option<String> {
    let i = s.find(key)? + key.len();
    let v = &s[i..];
    Some(v[..v.find('"')?].to_string())
}

// ---- subtitles ----

pub fn srt_to_vtt(s: &str) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for line in s.lines() {
        if line.contains("-->") { out.push_str(&line.replace(',', ".")); } else { out.push_str(line); }
        out.push('\n');
    }
    out
}

pub fn vtt_to_srt(s: &str) -> String {
    let mut n = 0;
    let mut out = String::new();
    let mut in_cue = false;
    for line in s.lines() {
        if line.contains("-->") {
            n += 1;
            // Cue settings after the end time have no SRT form.
            let parts: Vec<&str> = line.split_whitespace().collect();
            let fix = |t: &str| { let t = t.replace('.', ","); if t.matches(':').count() == 1 { format!("00:{t}") } else { t } };
            if parts.len() >= 3 { out.push_str(&format!("{n}\n{} --> {}\n", fix(parts[0]), fix(parts[2]))); }
            in_cue = true;
        } else if in_cue {
            out.push_str(line);
            out.push('\n');
            if line.trim().is_empty() { in_cue = false; }
        }
    }
    out
}

pub fn subs_to_text(s: &str) -> String {
    let mut out = vec![];
    for line in s.lines() {
        let l = line.trim();
        if l.is_empty() || l.contains("-->") || l.starts_with("WEBVTT") || l.chars().all(|c| c.is_ascii_digit()) { continue; }
        let stripped = html_to_text(l, false);
        let stripped = stripped.trim();
        if out.last().map(String::as_str) != Some(stripped) { out.push(stripped.to_string()); }
    }
    out.join("\n") + "\n"
}

// ---- archives ----

/// Always the system bsdtar: Git's tar on PATH does not read zip or rar.
pub fn tar_exe() -> PathBuf {
    PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())).join(r"System32\tar.exe")
}

pub fn extract_to(job: Option<&Job>, input: &Path, dir: &Path) -> Result<(), String> {
    let a = vec!["-xf".into(), input.display().to_string(), "-C".into(), dir.display().to_string()];
    run(job, &tar_exe(), &a, None).map(|_| ())
}

fn archive_stem(p: &Path) -> String {
    let n = p.file_name().and_then(|s| s.to_str()).unwrap_or("archive");
    for suf in [".tar.gz", ".tgz", ".tar", ".zip", ".rar", ".7z", ".gz"] {
        if n.to_ascii_lowercase().ends_with(suf) { return n[..n.len() - suf.len()].to_string(); }
    }
    n.to_string()
}

pub fn extract(job: &Job, input: &Path) -> Result<PathBuf, String> {
    let dir = unique(input.parent().unwrap_or(Path::new(".")), &archive_stem(input), "");
    std::fs::create_dir_all(&dir).map_err(rerr)?;
    match extract_to(Some(job), input, &dir) {
        Ok(()) => Ok(dir),
        Err(e) => { let _ = std::fs::remove_dir_all(&dir); Err(e) }
    }
}

fn pack(job: &Job, src_dir: &Path, entries: &[String], out: &Path, target: &str) -> Result<(), String> {
    let mut a: Vec<String> = match target {
        "zip" => vec!["--format".into(), "zip".into(), "-cf".into()],
        "tar" => vec!["-cf".into()],
        _ => vec!["-czf".into()],
    };
    a.push(out.display().to_string());
    a.push("-C".into());
    a.push(src_dir.display().to_string());
    a.extend_from_slice(entries);
    run(Some(job), &tar_exe(), &a, None).map(|_| ())
}

fn archive_convert(job: &Job, input: &Path, target: &str) -> Result<PathBuf, String> {
    let ext = if target == "gz" { "tar.gz" } else { target };
    let name = archive_stem(input);
    let out = unique(input.parent().unwrap_or(Path::new(".")), &name, ext);
    atomically(&out, |tmp| {
        if input.is_dir() {
            let parent = input.parent().ok_or("No parent folder")?;
            let leaf = input.file_name().ok_or("No folder name")?.to_string_lossy().to_string();
            pack(job, parent, &[leaf], tmp, target)
        } else {
            let t = TempDir::new()?;
            extract_to(Some(job), input, &t.0)?;
            job.set_frac(0.5);
            pack(job, &t.0, &[".".to_string()], tmp, target)
        }
    })
}

// ---- csv ----

/// Parses delimited text honouring quoted fields.
pub fn parse_delimited(s: &str, d: char) -> Vec<Vec<String>> {
    let (mut rows, mut row, mut f, mut q) = (vec![], vec![], String::new(), false);
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '"' if q && it.peek() == Some(&'"') => { f.push('"'); it.next(); }
            '"' => q = !q,
            c if c == d && !q => row.push(std::mem::take(&mut f)),
            '\r' if !q => {}
            '\n' if !q => { row.push(std::mem::take(&mut f)); rows.push(std::mem::take(&mut row)); }
            c => f.push(c),
        }
    }
    if !f.is_empty() || !row.is_empty() { row.push(f); rows.push(row); }
    rows
}

pub fn write_delimited(rows: &[Vec<String>], d: char) -> String {
    let mut out = String::new();
    for r in rows {
        let cells: Vec<String> = r.iter().map(|c| if c.contains([d, '"', '\n']) { format!("\"{}\"", c.replace('"', "\"\"")) } else { c.clone() }).collect();
        out.push_str(&cells.join(&d.to_string()));
        out.push('\n');
    }
    out
}

// ---- dispatch ----

fn pdf_then_images(job: &Job, input: &Path, ext: &str, make: impl FnOnce(&Path) -> Result<(), String>) -> Result<PathBuf, String> {
    let t = TempDir::new()?;
    let name = input.file_name().unwrap_or_default().to_string_lossy().to_string();
    let pdf = t.file(&format!("{name}.pdf"));
    make(&pdf)?;
    let imgs = crate::pdf::to_images(job, &pdf, ext)?;
    // Images landed in the temp folder; move them beside the input.
    let dest = if imgs.is_dir() { sibling(input, " pages", "") } else { sibling(input, "", ext) };
    if imgs.is_dir() {
        std::fs::create_dir_all(&dest).map_err(rerr)?;
        for f in std::fs::read_dir(&imgs).map_err(rerr)?.flatten() {
            std::fs::copy(f.path(), dest.join(f.file_name())).map_err(rerr)?;
        }
    } else {
        std::fs::copy(&imgs, &dest).map_err(rerr)?;
    }
    Ok(dest)
}

/// Text or Markdown input turned into an HTML file in `t`.
fn to_temp_html(input: &Path, t: &TempDir) -> Result<PathBuf, String> {
    let src = read_text(input)?;
    let body = match ext_of(input).as_str() {
        "md" | "markdown" => md_to_html(&src),
        "html" | "htm" => return Ok(input.to_path_buf()),
        _ => text_to_html(&src),
    };
    let f = t.file("in.html");
    std::fs::write(&f, page(&stem(input), &body)).map_err(rerr)?;
    Ok(f)
}

pub fn convert_doc(job: &Job, input: &Path, target: &str) -> Result<PathBuf, String> {
    let ext = ext_of(input);
    let is_word = matches!(ext.as_str(), "docx" | "doc" | "rtf" | "odt");
    let out = sibling(input, "", target);
    if is_word {
        return match target {
            "png" | "jpg" => pdf_then_images(job, input, target, |pdf| word(job, input, pdf, 17)),
            "docx" => atomically(&out, |tmp| word(job, input, tmp, 12)),
            "pdf" => atomically(&out, |tmp| word(job, input, tmp, 17)),
            _ => {
                let t = TempDir::new()?;
                let html = t.file("in.html");
                word(job, input, &html, 10)?;
                let body = read_text(&html)?;
                job.set_frac(0.8);
                match target {
                    "html" => { std::fs::write(&out, body).map_err(rerr)?; Ok(out) }
                    "txt" => { std::fs::write(&out, html_to_text(&body, false)).map_err(rerr)?; Ok(out) }
                    "md" => { std::fs::write(&out, html_to_text(&body, true)).map_err(rerr)?; Ok(out) }
                    "epub" => {
                        // Word's filtered HTML is not XML: rebuild it through Markdown so the EPUB validates.
                        let clean = md_to_html(&html_to_text(&body, true));
                        write_epub(&stem(input), &clean, &out)?;
                        Ok(out)
                    }
                    _ => Err(format!("Cannot make {target} from {ext}")),
                }
            }
        };
    }
    // Plain text family.
    let t = TempDir::new()?;
    match target {
        "html" => {
            let f = to_temp_html(input, &t)?;
            std::fs::copy(f, &out).map_err(rerr)?;
            Ok(out)
        }
        "txt" | "md" => {
            let src = read_text(input)?;
            let body = match (ext.as_str(), target) {
                ("html" | "htm", "md") => html_to_text(&src, true),
                ("html" | "htm", _) => html_to_text(&src, false),
                ("md" | "markdown", "txt") => html_to_text(&md_to_html(&src), false),
                _ => src,
            };
            std::fs::write(&out, body).map_err(rerr)?;
            Ok(out)
        }
        "pdf" => { let f = to_temp_html(input, &t)?; atomically(&out, |tmp| html_to_pdf(job, &f, tmp)) }
        "png" | "jpg" => { let f = to_temp_html(input, &t)?; pdf_then_images(job, input, target, |pdf| html_to_pdf(job, &f, pdf)) }
        "docx" => { let f = to_temp_html(input, &t)?; atomically(&out, |tmp| word(job, &f, tmp, 12)) }
        "epub" => {
            let src = read_text(input)?;
            let body = match ext.as_str() {
                "md" | "markdown" => md_to_html(&src),
                "html" | "htm" => md_to_html(&html_to_text(&src, true)),
                _ => text_to_html(&src),
            };
            write_epub(&stem(input), &body, &out)?;
            Ok(out)
        }
        _ => Err(format!("Cannot make {target} from {ext}")),
    }
}

// Office refuses unknown double extensions, so it gets a name ending in the real one.

pub fn convert_slides(job: &Job, input: &Path, target: &str) -> Result<PathBuf, String> {
    let run = |out: &Path, fmt: u32| powershell(job, SLIDES_CONVERT, &[("OR_IN", input.display().to_string()), ("OR_OUT", out.display().to_string()), ("OR_FMT", fmt.to_string())]).map(|_| ());
    match target {
        "pdf" => { let o = sibling(input, "", "pdf"); run(&o, 32)?; Ok(o) }
        "pptx" => { let o = sibling(input, "", "pptx"); run(&o, 24)?; Ok(o) }
        // PowerPoint writes a folder of slides when saving as an image type.
        "jpg" | "png" => { let o = sibling(input, " slides", ""); run(&o, if target == "jpg" { 17 } else { 18 })?; Ok(o) }
        _ => Err(format!("Cannot make {target} from slides")),
    }
}

pub fn convert_sheet(job: &Job, input: &Path, target: &str) -> Result<PathBuf, String> {
    let ext = ext_of(input);
    let out = sibling(input, "", target);
    if matches!(ext.as_str(), "csv" | "tsv") && matches!(target, "csv" | "tsv") {
        let d = if ext == "csv" { ',' } else { '\t' };
        let to = if target == "csv" { ',' } else { '\t' };
        std::fs::write(&out, write_delimited(&parse_delimited(&read_text(input)?, d), to)).map_err(rerr)?;
        return Ok(out);
    }
    let fmt = match target { "xlsx" => "51", "ods" => "60", "csv" => "62", "tsv" => "42", "pdf" => "pdf", _ => return Err(format!("Cannot make {target}")) };
    // Excel reads a CSV without a BOM in the system code page, so text files reach it as marked UTF-8.
    let t = TempDir::new()?;
    let source = if matches!(ext.as_str(), "csv" | "tsv") {
        let d = if ext == "csv" { ',' } else { '\t' };
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(write_delimited(&parse_delimited(&read_text(input)?, d), ',').as_bytes());
        let f = t.file("in.csv");
        std::fs::write(&f, bytes).map_err(rerr)?;
        f
    } else { input.to_path_buf() };
    atomically(&out, |tmp| powershell(job, EXCEL_CONVERT, &[("OR_IN", source.display().to_string()), ("OR_OUT", tmp.display().to_string()), ("OR_FMT", fmt.into())]).map(|_| ()))
}

pub fn convert_epub(job: &Job, input: &Path, target: &str) -> Result<PathBuf, String> {
    let (t, base, body) = epub_to_html(input)?;
    let out = sibling(input, "", target);
    match target {
        "txt" => std::fs::write(&out, html_to_text(&body, false)).map_err(rerr)?,
        "md" => std::fs::write(&out, html_to_text(&body, true)).map_err(rerr)?,
        "html" => std::fs::write(&out, page(&stem(input), &body)).map_err(rerr)?,
        "pdf" | "docx" => {
            // Beside the chapters so relative image links still resolve.
            let f = base.join("__orrery.html");
            std::fs::write(&f, page(&stem(input), &body)).map_err(rerr)?;
            if target == "pdf" { html_to_pdf(job, &f, &out)?; } else { word(job, &f, &out, 12)?; }
        }
        _ => return Err(format!("Cannot make {target} from EPUB")),
    }
    drop(t);
    Ok(out)
}

pub fn convert_subs(input: &Path, target: &str) -> Result<PathBuf, String> {
    let s = read_text(input)?;
    let out = sibling(input, "", target);
    let body = match target { "vtt" => srt_to_vtt(&s), "srt" => vtt_to_srt(&s), _ => subs_to_text(&s) };
    std::fs::write(&out, body).map_err(rerr)?;
    Ok(out)
}

pub fn convert_pdf(job: &Job, input: &Path, target: &str) -> Result<PathBuf, String> {
    let out = sibling(input, "", target);
    match target {
        "jpg" | "png" => crate::pdf::to_images(job, input, target),
        "txt" => { let p = crate::pdf::extract_pages(job, input)?; std::fs::write(&out, p.join("\n\n")).map_err(rerr)?; Ok(out) }
        "md" => { let p = crate::pdf::extract_pages(job, input)?; std::fs::write(&out, crate::pdf::text_to_markdown(&p)).map_err(rerr)?; Ok(out) }
        "docx" => atomically(&out, |tmp| word(job, input, tmp, 12)),
        "epub" => {
            let p = crate::pdf::extract_pages(job, input)?;
            write_epub(&stem(input), &md_to_html(&crate::pdf::text_to_markdown(&p)), &out)?;
            Ok(out)
        }
        "pptx" => {
            let imgs = crate::pdf::to_images(job, input, "png")?;
            // The script reads a folder of pages, so a lone page moves into a scratch folder first.
            let scratch = TempDir::new()?;
            let dir = if imgs.is_dir() { imgs.clone() } else {
                std::fs::rename(&imgs, scratch.file("page-001.png")).map_err(rerr)?;
                scratch.0.clone()
            };
            let r = powershell(job, PDF_TO_SLIDES, &[("OR_IN", dir.display().to_string()), ("OR_OUT", out.display().to_string())]);
            if imgs.is_dir() { let _ = std::fs::remove_dir_all(&imgs); }
            r?; Ok(out)
        }
        "xlsx" => {
            // Columns inferred from runs of spaces; a best effort for tabular PDFs, not a layout engine.
            let p = crate::pdf::extract_pages(job, input)?;
            let t = TempDir::new()?;
            let csv = t.file("in.csv");
            let rows: Vec<Vec<String>> = p.iter().flat_map(|pg| pg.lines()).filter(|l| !l.trim().is_empty())
                .map(|l| l.split("  ").map(str::trim).filter(|c| !c.is_empty()).map(str::to_string).collect()).collect();
            let mut bytes = vec![0xEF, 0xBB, 0xBF];
            bytes.extend_from_slice(write_delimited(&rows, ',').as_bytes());
            std::fs::write(&csv, bytes).map_err(rerr)?;
            powershell(job, EXCEL_CONVERT, &[("OR_IN", csv.display().to_string()), ("OR_OUT", out.display().to_string()), ("OR_FMT", "51".into())])?;
            Ok(out)
        }
        _ => Err(format!("Cannot make {target} from PDF")),
    }
}

pub fn convert_archive(job: &Job, input: &Path, target: &str) -> Result<PathBuf, String> { archive_convert(job, input, target) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_urls_escape_path_breakers() {
        assert_eq!(file_url(Path::new(r"C:\a b\c#1 100%.html")), "file:///C:/a%20b/c%231%20100%25.html");
    }
    #[test]
    fn srt_vtt_round_trip_keeps_timing() {
        let srt = "1\n00:00:01,000 --> 00:00:02,500\nHello\n\n2\n00:00:03,000 --> 00:00:04,000\nThere\n";
        let vtt = srt_to_vtt(srt);
        assert!(vtt.starts_with("WEBVTT"));
        assert!(vtt.contains("00:00:01.000 --> 00:00:02.500"));
        let back = vtt_to_srt(&vtt);
        assert!(back.contains("00:00:01,000 --> 00:00:02,500"));
        assert!(back.contains("2\n00:00:03,000"));
    }
    #[test]
    fn vtt_short_times_and_settings() {
        let s = vtt_to_srt("WEBVTT\n\n01:02.000 --> 01:03.000 align:start\nHi\n");
        assert!(s.contains("00:01:02,000 --> 00:01:03,000\n"));
    }
    #[test]
    fn subs_text_drops_cues_and_tags() {
        assert_eq!(subs_to_text("1\n00:00:01,000 --> 00:00:02,000\n<i>Hello</i>\n"), "Hello\n");
    }
    #[test]
    fn csv_quotes_round_trip() {
        let rows = parse_delimited("a,\"b,c\",\"d\"\"e\"\n1,2,3\n", ',');
        assert_eq!(rows[0], vec!["a", "b,c", "d\"e"]);
        assert_eq!(parse_delimited(&write_delimited(&rows, '\t'), '\t'), rows);
    }
    #[test]
    fn html_to_markdown_basics() {
        let m = html_to_text("<h2>Title</h2><p>Some <b>bold</b> &amp; <a href=\"u\">link</a></p><ul><li>one</li></ul><script>x</script>", true);
        assert!(m.contains("## Title"));
        assert!(m.contains("**bold** & [link](u)"));
        assert!(m.contains("- one"));
        assert!(!m.contains('x'));
    }
    #[test]
    fn numeric_entities() { assert_eq!(html_to_text("<p>&#65;&#x42;</p>", false).trim(), "AB"); }
    #[test]
    fn crc_matches_known_value() { assert_eq!(crc32(b"123456789"), 0xCBF43926); }
    #[test]
    fn archive_stem_handles_double_extension() { assert_eq!(archive_stem(Path::new("a.b.tar.gz")), "a.b"); }
}
