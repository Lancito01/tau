//! PDF work. Rendering and OCR use what ships with Windows; structure edits use lopdf.
use crate::image::{self, Bmp};
use crate::job::*;
use lopdf::{dictionary, Document, Object, ObjectId, Stream};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use windows::core::*;
use windows::Data::Pdf::*;
use windows::Graphics::Imaging::*;
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::*;
use windows::Storage::*;

fn es(e: Error) -> String { e.message().to_string() }

fn open(p: &Path) -> std::result::Result<PdfDocument, String> {
    let abs = std::fs::canonicalize(p).map_err(|e| e.to_string())?;
    let s = abs.display().to_string();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s).to_string();
    let f = StorageFile::GetFileFromPathAsync(&HSTRING::from(s)).map_err(es)?.join().map_err(es)?;
    PdfDocument::LoadFromFileAsync(&f).map_err(es)?.join().map_err(|e| format!("Could not open PDF: {}", es(e)))
}

fn read_stream(st: &InMemoryRandomAccessStream) -> std::result::Result<Vec<u8>, String> {
    let size = st.Size().map_err(es)? as u32;
    st.Seek(0).map_err(es)?;
    let r = DataReader::CreateDataReader(&st.GetInputStreamAt(0).map_err(es)?).map_err(es)?;
    r.LoadAsync(size).map_err(es)?.join().map_err(es)?;
    let mut v = vec![0u8; size as usize];
    r.ReadBytes(&mut v).map_err(es)?;
    Ok(v)
}

fn render_page(doc: &PdfDocument, i: u32, dpi: f64, stream: &InMemoryRandomAccessStream, encoder: Option<GUID>) -> std::result::Result<(), String> {
    let page = doc.GetPage(i).map_err(es)?;
    let size = page.Size().map_err(es)?;
    let opts = PdfPageRenderOptions::new().map_err(es)?;
    // Page size is in DIPs at 96 per inch.
    opts.SetDestinationWidth((size.Width as f64 / 96.0 * dpi).round() as u32).map_err(es)?;
    opts.SetDestinationHeight((size.Height as f64 / 96.0 * dpi).round() as u32).map_err(es)?;
    if let Some(g) = encoder { opts.SetBitmapEncoderId(g).map_err(es)?; }
    page.RenderWithOptionsToStreamAsync(stream, &opts).map_err(es)?.join().map_err(es)
}

pub fn page_count(p: &Path) -> std::result::Result<u32, String> { open(p)?.PageCount().map_err(es) }

/// PNG or JPEG at 600 DPI: one file for a single page, a folder for several.
pub fn to_images(job: &Job, input: &Path, ext: &str) -> std::result::Result<PathBuf, String> { images_at(job, input, ext, 600.0) }

/// Slides fill a screen and PowerPoint recompresses what it is given, so 300 DPI already holds all of it.
pub fn to_slide_images(job: &Job, input: &Path) -> std::result::Result<PathBuf, String> { images_at(job, input, "png", 300.0) }

fn images_at(job: &Job, input: &Path, ext: &str, dpi: f64) -> std::result::Result<PathBuf, String> {
    let doc = open(input)?;
    let n = doc.PageCount().map_err(es)?;
    // Always rendered to PNG. The Windows JPEG encoder here has no quality setting, so our own does that step.
    let enc = BitmapEncoder::PngEncoderId().map_err(es)?;
    let dir = if n > 1 { Some(sibling_dir(input, " pages")?) } else { None };
    let mut last = PathBuf::new();
    for i in 0..n {
        if job.cancelled() { return Err("Cancelled".into()); }
        job.set_frac(i as f32 / n as f32);
        let st = InMemoryRandomAccessStream::new().map_err(es)?;
        render_page(&doc, i, dpi, &st, Some(enc))?;
        let bytes = read_stream(&st)?;
        last = match &dir { Some(d) => d.join(format!("page-{:03}.{ext}", i + 1)), None => sibling(input, "", ext) };
        if ext == "png" {
            std::fs::write(&last, bytes).map_err(|e| e.to_string())?;
        } else {
            let t = TempDir::new()?;
            let png = t.file("page.png");
            std::fs::write(&png, bytes).map_err(|e| e.to_string())?;
            image::save(&image::load(&png)?, &last, &image::SaveOpts::default())?;
        }
    }
    Ok(dir.unwrap_or(last))
}

/// Text of one page by OCR, for pages with no text layer.
fn ocr_page(doc: &PdfDocument, i: u32) -> std::result::Result<String, String> {
    let st = InMemoryRandomAccessStream::new().map_err(es)?;
    render_page(doc, i, 200.0, &st, Some(BitmapEncoder::PngEncoderId().map_err(es)?))?;
    st.Seek(0).map_err(es)?;
    let dec = BitmapDecoder::CreateAsync(&st).map_err(es)?.join().map_err(es)?;
    let sb = dec.GetSoftwareBitmapAsync().map_err(es)?.join().map_err(es)?;
    let eng = OcrEngine::TryCreateFromUserProfileLanguages().map_err(|_| "No OCR language installed".to_string())?;
    let r = eng.RecognizeAsync(&sb).map_err(es)?.join().map_err(es)?;
    Ok(r.Text().map_err(es)?.to_string())
}

/// Rough check that extraction gave real words rather than encoding junk.
pub fn looks_like_text(t: &str) -> bool {
    let total = t.chars().filter(|c| !c.is_whitespace()).count();
    if total < 8 { return false; }
    let good = t.chars().filter(|c| c.is_alphanumeric() || c.is_ascii_punctuation()).count();
    good as f32 / total as f32 > 0.7
}

/// Text per page, OCR where the page has no usable text layer.
pub fn extract_pages(job: &Job, input: &Path) -> std::result::Result<Vec<String>, String> {
    let d = Document::load(input).map_err(|e| format!("Could not read PDF: {e}"))?;
    let ids: Vec<u32> = d.get_pages().keys().copied().collect();
    let render = open(input).ok();
    let mut out = vec![];
    for (idx, n) in ids.iter().enumerate() {
        if job.cancelled() { return Err("Cancelled".into()); }
        job.set_frac(idx as f32 / ids.len() as f32);
        let t = d.extract_text(&[*n]).unwrap_or_default();
        if looks_like_text(&t) { out.push(t); continue; }
        let ocr = render.as_ref().and_then(|r| ocr_page(r, idx as u32).ok()).unwrap_or_default();
        out.push(ocr);
    }
    // An empty "converted" file would look like success.
    if out.iter().all(|t| t.trim().is_empty()) { return Err("No readable text found in this PDF".into()); }
    Ok(out)
}

/// Light structure from plain text: short standalone title-like lines become headings, bullets stay lists.
pub fn text_to_markdown(pages: &[String]) -> String {
    let mut md = String::new();
    for page in pages {
        for line in page.lines() {
            let l = line.trim();
            if l.is_empty() { md.push('\n'); continue; }
            let wordy = l.split_whitespace().count();
            if let Some(rest) = l.strip_prefix('•').or_else(|| l.strip_prefix("- ")) {
                md.push_str(&format!("- {}\n", rest.trim()));
            } else if wordy <= 8 && !l.ends_with(['.', ',', ';', ':']) && l.chars().any(|c| c.is_alphabetic()) && l.chars().next().is_some_and(|c| c.is_uppercase()) {
                md.push_str(&format!("\n## {l}\n\n"));
            } else {
                md.push_str(l);
                md.push('\n');
            }
        }
        md.push('\n');
    }
    md
}

// ---- writing ----

/// The bytes of a JPEG that a PDF can hold as they are, with its size and colour space. A rotated one cannot, since
/// the PDF would show it sideways, and neither can CMYK, which needs more than a colour space name.
fn embeddable_jpeg(input: &Path) -> Option<(Vec<u8>, u32, u32, &'static str)> {
    if !matches!(crate::catalog::ext_of(input).as_str(), "jpg" | "jpeg") { return None; }
    let bytes = std::fs::read(input).ok()?;
    let info = crate::quality::jpeg_info(&bytes)?;
    let space = crate::quality::pdf_color_space(&info)?;
    if image::jpeg_orientation(&bytes) != 1 { return None; }
    Some((bytes, info.w, info.h, space))
}

/// One PDF, one page per image. JPEGs go in byte for byte, and everything else goes in without loss.
pub fn images_to_pdf(job: &Job, inputs: &[PathBuf], out: &Path) -> std::result::Result<(), String> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let mut kids = vec![];
    for (i, input) in inputs.iter().enumerate() {
        if job.cancelled() { return Err("Cancelled".into()); }
        job.set_frac(i as f32 / inputs.len() as f32);
        let (img, w, h) = match embeddable_jpeg(input) {
            Some((jpeg, w, h, space)) => (doc.add_object(Stream::new(dictionary! {
                "Type" => "XObject", "Subtype" => "Image", "Width" => w as i64, "Height" => h as i64,
                "ColorSpace" => space, "BitsPerComponent" => 8, "Filter" => "DCTDecode",
            }, jpeg).with_compression(false)), w, h),
            None => {
                let bmp: Bmp = image::load(input)?;
                let mut rgb = Vec::with_capacity(bmp.px.len() / 4 * 3);
                for p in bmp.px.chunks_exact(4) { rgb.extend_from_slice(&[p[2], p[1], p[0]]); }
                let mut d = dictionary! {
                    "Type" => "XObject", "Subtype" => "Image", "Width" => bmp.w as i64, "Height" => bmp.h as i64,
                    "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
                };
                if bmp.has_alpha() {
                    let alpha: Vec<u8> = bmp.px.chunks_exact(4).map(|p| p[3]).collect();
                    let mask = doc.add_object(Stream::new(dictionary! {
                        "Type" => "XObject", "Subtype" => "Image", "Width" => bmp.w as i64, "Height" => bmp.h as i64,
                        "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8,
                    }, alpha));
                    d.set("SMask", mask);
                }
                (doc.add_object(Stream::new(d, rgb)), bmp.w, bmp.h)
            }
        };
        // 96 px per inch: a sensible default page size for photos and screenshots alike.
        let (pw, ph) = (w as f32 * 0.75, h as f32 * 0.75);
        let content = doc.add_object(Stream::new(dictionary! {}, format!("q {pw} 0 0 {ph} 0 0 cm /I0 Do Q").into_bytes()));
        let page = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), Object::Real(pw), Object::Real(ph)],
            "Contents" => content,
            "Resources" => dictionary! { "XObject" => dictionary! { "I0" => img } },
        });
        kids.push(Object::Reference(page));
    }
    // Flate on the pixel streams, which is lossless. The JPEG streams opted out above.
    doc.compress();
    finish_doc(doc, pages_id, kids, out)
}

fn finish_doc(mut doc: Document, pages_id: ObjectId, kids: Vec<Object>, out: &Path) -> std::result::Result<(), String> {
    let count = kids.len() as i64;
    doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count }));
    let cat = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", cat);
    doc.save(out).map(|_| ()).map_err(|e| e.to_string())
}

/// Builds a new file from (document index, 1-based page, extra rotation) triples, in order.
/// Rotation-free pages keep vector content; nothing is rasterized.
pub fn assemble(sources: &[Document], picks: &[(usize, u32, i64)], out: &Path) -> std::result::Result<(), String> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let mut kids = vec![];
    let mut next = 1u32;
    // Every source gets its own id range, added once. A page picked twice gets a fresh page dictionary.
    let mut offsets: BTreeMap<usize, u32> = BTreeMap::new();
    doc.max_id = 1 + pages_id.0;
    next += pages_id.0;
    for &(src, page, rot) in picks {
        let s = sources.get(src).ok_or("Bad source")?;
        let page_id = *s.get_pages().get(&page).ok_or_else(|| format!("No page {page}"))?;
        let off = *offsets.entry(src).or_insert_with(|| {
            let o = next;
            for (id, obj) in s.objects.iter() {
                let mut c = obj.clone();
                remap(&mut c, o);
                doc.objects.insert((id.0 + o, id.1), c);
            }
            next = o + s.max_id + 1;
            o
        });
        let inherited = inherited_attrs(s, page_id);
        let mut d = s.get_object(page_id).and_then(|o| o.as_dict()).map_err(|e| e.to_string())?.clone();
        for (k, v) in inherited {
            if !d.has(k.as_bytes()) { let mut v = v; remap(&mut v, 0); d.set(k, v); }
        }
        let mut obj = Object::Dictionary(d);
        remap_shallow(&mut obj, off);
        let Object::Dictionary(mut d) = obj else { unreachable!() };
        d.set("Parent", pages_id);
        let cur = d.get(b"Rotate").and_then(|o| o.as_i64()).unwrap_or(0);
        if rot != 0 || cur != 0 { d.set("Rotate", (cur + rot).rem_euclid(360)); }
        let new_id = (next, 0);
        next += 1;
        doc.objects.insert(new_id, Object::Dictionary(d));
        kids.push(Object::Reference(new_id));
    }
    doc.max_id = next;
    finish_doc(doc, pages_id, kids, out)?;
    // A second pass drops objects no kept page references.
    let mut d = Document::load(out).map_err(|e| e.to_string())?;
    d.prune_objects();
    d.compress();
    d.save(out).map(|_| ()).map_err(|e| e.to_string())
}

fn inherited_attrs(s: &Document, page: ObjectId) -> Vec<(String, Object)> {
    let mut out = vec![];
    let mut cur = page;
    for _ in 0..32 {
        let Ok(d) = s.get_object(cur).and_then(|o| o.as_dict()) else { break };
        for k in ["Resources", "MediaBox", "CropBox", "Rotate"] {
            if !out.iter().any(|(n, _): &(String, Object)| n == k) {
                if let Ok(v) = d.get(k.as_bytes()) { out.push((k.to_string(), v.clone())); }
            }
        }
        match d.get(b"Parent").and_then(|o| o.as_reference()) { Ok(p) => cur = p, Err(_) => break }
    }
    out
}

fn remap_shallow(o: &mut Object, off: u32) { remap(o, off) }

fn remap(o: &mut Object, off: u32) {
    match o {
        Object::Reference(r) => r.0 += off,
        Object::Array(a) => a.iter_mut().for_each(|x| remap(x, off)),
        Object::Dictionary(d) => d.iter_mut().for_each(|(_, v)| remap(v, off)),
        Object::Stream(s) => s.dict.iter_mut().for_each(|(_, v)| remap(v, off)),
        _ => {}
    }
}

pub fn load_doc(p: &Path) -> std::result::Result<Document, String> {
    Document::load(p).map_err(|e| format!("Could not read {}: {e}", p.file_name().unwrap_or_default().to_string_lossy()))
}

pub fn merge(job: &Job, inputs: &[PathBuf], out: &Path) -> std::result::Result<(), String> {
    let mut docs = vec![];
    let mut picks = vec![];
    for (i, p) in inputs.iter().enumerate() {
        if job.cancelled() { return Err("Cancelled".into()); }
        job.set_frac(i as f32 / inputs.len() as f32 * 0.5);
        let d = load_doc(p)?;
        for n in d.get_pages().keys() { picks.push((i, *n, 0)); }
        docs.push(d);
    }
    assemble(&docs, &picks, out)
}

pub fn split(job: &Job, input: &Path) -> std::result::Result<PathBuf, String> {
    let d = load_doc(input)?;
    let pages: Vec<u32> = d.get_pages().keys().copied().collect();
    let dir = sibling_dir(input, " pages")?;
    let docs = vec![d];
    for (i, n) in pages.iter().enumerate() {
        if job.cancelled() { return Err("Cancelled".into()); }
        job.set_frac(i as f32 / pages.len() as f32);
        assemble(&docs, &[(0, *n, 0)], &dir.join(format!("page-{:03}.pdf", i + 1)))?;
    }
    Ok(dir)
}

/// Parses an order spec such as `3, 1-2, 5r90, 2x2`. `rN` rotates, `xN` duplicates, omitted pages are removed.
pub fn parse_order(spec: &str, max: u32) -> std::result::Result<Vec<(u32, i64)>, String> {
    let mut out = vec![];
    for tok in spec.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        let (tok, dup) = match tok.split_once('x') { Some((a, b)) => (a, b.trim().parse::<u32>().map_err(|_| format!("Bad count in {tok}"))?), None => (tok, 1) };
        let (range, rot) = match tok.split_once('r') { Some((a, b)) => (a, b.trim().parse::<i64>().map_err(|_| format!("Bad rotation in {tok}"))?), None => (tok, 0) };
        if rot % 90 != 0 { return Err(format!("Rotation must be a multiple of 90: {tok}")); }
        let (a, b) = match range.split_once('-') {
            Some((a, b)) => (a.trim().parse::<u32>().map_err(|_| format!("Bad page: {a}"))?, b.trim().parse::<u32>().map_err(|_| format!("Bad page: {b}"))?),
            None => { let v = range.trim().parse::<u32>().map_err(|_| format!("Bad page: {range}"))?; (v, v) }
        };
        if a < 1 || b > max || a > b { return Err(format!("Pages run from 1 to {max}: {range}")); }
        for _ in 0..dup { for n in a..=b { out.push((n, rot)); } }
    }
    if out.is_empty() { return Err("Keep at least one page".into()); }
    Ok(out)
}

/// Recompresses structure, and with a DPI re-renders every page as an image for a much smaller scan-style file.
pub fn compress(job: &Job, input: &Path, dpi: Option<f64>) -> std::result::Result<PathBuf, String> {
    let out = sibling(input, " compressed", "pdf");
    atomically(&out, |tmp| {
        match dpi {
            None => {
                let mut d = load_doc(input)?;
                d.prune_objects();
                d.compress();
                d.save(tmp).map(|_| ()).map_err(|e| e.to_string())
            }
            Some(dpi) => {
                let src = open(input)?;
                let n = src.PageCount().map_err(es)?;
                let enc = BitmapEncoder::JpegEncoderId().map_err(es)?;
                let t = TempDir::new()?;
                let mut files = vec![];
                for i in 0..n {
                    if job.cancelled() { return Err("Cancelled".into()); }
                    job.set_frac(i as f32 / n as f32 * 0.9);
                    let st = InMemoryRandomAccessStream::new().map_err(es)?;
                    render_page(&src, i, dpi, &st, Some(enc))?;
                    let f = t.file(&format!("{i}.jpg"));
                    std::fs::write(&f, read_stream(&st)?).map_err(|e| e.to_string())?;
                    files.push(f);
                }
                images_to_pdf(job, &files, tmp)
            }
        }
    })
}

pub fn set_metadata(input: &Path, fields: &[(&str, String)], strip: bool) -> std::result::Result<PathBuf, String> {
    let out = sibling(input, " edited", "pdf");
    atomically(&out, |tmp| {
        let mut d = load_doc(input)?;
        if strip {
            d.trailer.remove(b"Info");
            let root = d.trailer.get(b"Root").and_then(|o| o.as_reference()).map_err(|e| e.to_string())?;
            if let Ok(cat) = d.get_object_mut(root).and_then(|o| o.as_dict_mut()) { cat.remove(b"Metadata"); }
            d.prune_objects();
        } else {
            let mut info = match d.trailer.get(b"Info").ok().and_then(|o| o.as_reference().ok()) {
                Some(id) => d.get_object(id).and_then(|o| o.as_dict()).cloned().unwrap_or_default(),
                None => lopdf::Dictionary::new(),
            };
            for (k, v) in fields { if !v.is_empty() { info.set(*k, Object::string_literal(v.as_str())); } }
            let id = d.add_object(info);
            d.trailer.set("Info", id);
        }
        d.save(tmp).map(|_| ()).map_err(|e| e.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn order_spec_parses() {
        assert_eq!(parse_order("3, 1-2", 3).unwrap(), vec![(3, 0), (1, 0), (2, 0)]);
        assert_eq!(parse_order("2r90", 3).unwrap(), vec![(2, 90)]);
        assert_eq!(parse_order("1x2", 3).unwrap(), vec![(1, 0), (1, 0)]);
    }
    #[test]
    fn order_spec_rejects_out_of_range() {
        assert!(parse_order("9", 3).is_err());
        assert!(parse_order("", 3).is_err());
        assert!(parse_order("1r45", 3).is_err());
    }
    #[test]
    fn junk_text_is_detected() {
        assert!(looks_like_text("Hello there, this is real text."));
        assert!(!looks_like_text("\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}"));
    }
}
