//! Image decode and encode through WIC, which ships with Windows. ffmpeg covers what WIC cannot.
use crate::job::*;
use std::path::{Path, PathBuf};
use windows::core::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::Foundation::GENERIC_ACCESS_RIGHTS;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::*;

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;

/// Straight-alpha BGRA, top-down.
#[derive(Clone)]
pub struct Bmp { pub w: u32, pub h: u32, pub px: Vec<u8> }

impl Bmp {
    pub fn new(w: u32, h: u32, bgra: [u8; 4]) -> Bmp {
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h { px.extend_from_slice(&bgra); }
        Bmp { w, h, px }
    }
    pub fn has_alpha(&self) -> bool { self.px.chunks_exact(4).any(|p| p[3] != 255) }
    pub fn flatten_white(&mut self) {
        for p in self.px.chunks_exact_mut(4) {
            let a = p[3] as u32;
            for c in &mut p[..3] { *c = ((*c as u32 * a + 255 * (255 - a)) / 255) as u8; }
            p[3] = 255;
        }
    }
    pub fn crop(&self, x: u32, y: u32, w: u32, h: u32) -> Bmp {
        let (x, y) = (x.min(self.w - 1), y.min(self.h - 1));
        let (w, h) = (w.min(self.w - x).max(1), h.min(self.h - y).max(1));
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for row in y..y + h {
            let s = ((row * self.w + x) * 4) as usize;
            px.extend_from_slice(&self.px[s..s + (w * 4) as usize]);
        }
        Bmp { w, h, px }
    }
    pub fn scaled(&self, w: u32, h: u32) -> std::result::Result<Bmp, String> {
        unsafe {
            let f = factory().map_err(es)?;
            let src = f.CreateBitmapFromMemory(self.w, self.h, &GUID_WICPixelFormat32bppBGRA, self.w * 4, &self.px).map_err(es)?;
            let sc = f.CreateBitmapScaler().map_err(es)?;
            sc.Initialize(&src, w.max(1), h.max(1), WICBitmapInterpolationModeHighQualityCubic).map_err(es)?;
            let mut px = vec![0u8; (w.max(1) * h.max(1) * 4) as usize];
            sc.CopyPixels(std::ptr::null(), w.max(1) * 4, &mut px).map_err(es)?;
            Ok(Bmp { w: w.max(1), h: h.max(1), px })
        }
    }
    /// Alpha-composite `src` at (x, y).
    pub fn blit(&mut self, src: &Bmp, x: i32, y: i32) {
        for sy in 0..src.h as i32 {
            let dy = y + sy;
            if dy < 0 || dy >= self.h as i32 { continue; }
            for sx in 0..src.w as i32 {
                let dx = x + sx;
                if dx < 0 || dx >= self.w as i32 { continue; }
                let s = ((sy * src.w as i32 + sx) * 4) as usize;
                let d = ((dy * self.w as i32 + dx) * 4) as usize;
                let a = src.px[s + 3] as u32;
                for c in 0..3 { self.px[d + c] = ((src.px[s + c] as u32 * a + self.px[d + c] as u32 * (255 - a)) / 255) as u8; }
                self.px[d + 3] = (a + self.px[d + 3] as u32 * (255 - a) / 255).min(255) as u8;
            }
        }
    }
    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, bgra: [u8; 4]) {
        for row in y.max(0)..(y + h).min(self.h as i32) {
            for col in x.max(0)..(x + w).min(self.w as i32) {
                let d = ((row * self.w as i32 + col) * 4) as usize;
                self.px[d..d + 4].copy_from_slice(&bgra);
            }
        }
    }
}

fn es(e: Error) -> String { e.message().to_string() }

fn factory() -> Result<IWICImagingFactory> {
    unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
}

fn exif_orientation(frame: &IWICBitmapFrameDecode) -> u32 {
    unsafe {
        let Ok(q) = frame.GetMetadataQueryReader() else { return 1 };
        for name in [w!("/app1/ifd/{ushort=274}"), w!("/ifd/{ushort=274}")] {
            let mut v = PROPVARIANT::default();
            if q.GetMetadataByName(name, &mut v).is_ok() {
                if let Ok(n) = u16::try_from(&v) { return n as u32; }
            }
        }
        1
    }
}

fn wic_load(p: &Path) -> Result<Bmp> {
    unsafe {
        let f = factory()?;
        let dec = f.CreateDecoderFromFilename(&HSTRING::from(p.as_os_str()), None, GENERIC_ACCESS_RIGHTS(GENERIC_READ), WICDecodeMetadataCacheOnDemand)?;
        let frame = dec.GetFrame(0)?;
        let orient = exif_orientation(&frame);
        let conv = f.CreateFormatConverter()?;
        conv.Initialize(&frame, &GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
        let opts = match orient {
            2 => WICBitmapTransformFlipHorizontal,
            3 => WICBitmapTransformRotate180,
            4 => WICBitmapTransformFlipVertical,
            5 => WICBitmapTransformOptions(WICBitmapTransformRotate90.0 | WICBitmapTransformFlipHorizontal.0),
            6 => WICBitmapTransformRotate90,
            7 => WICBitmapTransformOptions(WICBitmapTransformRotate270.0 | WICBitmapTransformFlipHorizontal.0),
            8 => WICBitmapTransformRotate270,
            _ => WICBitmapTransformRotate0,
        };
        let rot = f.CreateBitmapFlipRotator()?;
        rot.Initialize(&conv, opts)?;
        let (mut w, mut h) = (0, 0);
        rot.GetSize(&mut w, &mut h)?;
        let mut px = vec![0u8; (w * h * 4) as usize];
        rot.CopyPixels(std::ptr::null(), w * 4, &mut px)?;
        Ok(Bmp { w, h, px })
    }
}

/// Decodes through WIC, then falls back to ffmpeg for what has no codec installed (HEIC, AVIF, SVG).
pub fn load(p: &Path) -> std::result::Result<Bmp, String> {
    match wic_load(p) {
        Ok(b) => Ok(b),
        Err(wic_err) => {
            let Some(ff) = crate::ffmpeg() else {
                return Err(format!("{}: no decoder ({})", p.display(), es(wic_err)));
            };
            let t = TempDir::new()?;
            let tmp = t.file("in.png");
            run(None, &ff, &["-y".into(), "-hide_banner".into(), "-i".into(), p.display().to_string(), "-frames:v".into(), "1".into(), tmp.display().to_string()], None)
                .map_err(|e| format!("Could not read {}: {e}", p.file_name().unwrap_or_default().to_string_lossy()))?;
            wic_load(&tmp).map_err(es)
        }
    }
}

pub struct SaveOpts { pub quality: f32 }
impl Default for SaveOpts { fn default() -> Self { SaveOpts { quality: 0.92 } } }

fn property(bag: &IPropertyBag2Wrap, name: PCWSTR, v: f32) { bag.write(name, v); }

struct IPropertyBag2Wrap(windows::Win32::System::Com::StructuredStorage::IPropertyBag2);
impl IPropertyBag2Wrap {
    fn write(&self, name: PCWSTR, v: f32) {
        use windows::Win32::System::Com::StructuredStorage::*;
        use windows::Win32::System::Variant::*;
        unsafe {
            let opt = PROPBAG2 { pstrName: PWSTR(name.0 as *mut u16), ..Default::default() };
            let val = VARIANT::from(v);
            let _ = val.vt() == VT_R4;
            let _ = self.0.Write(1, &opt, &val);
        }
    }
}

/// Encodes to a stream target via WIC. The container comes from the extension.
fn wic_save(b: &Bmp, out: &Path, ext: &str, o: &SaveOpts) -> Result<()> {
    unsafe {
        let f = factory()?;
        let guid = match ext {
            "png" => GUID_ContainerFormatPng,
            "jpg" | "jpeg" => GUID_ContainerFormatJpeg,
            "tif" | "tiff" => GUID_ContainerFormatTiff,
            "bmp" => GUID_ContainerFormatBmp,
            "gif" => GUID_ContainerFormatGif,
            "heic" | "heif" => GUID_ContainerFormatHeif,
            _ => return Err(Error::new(windows::Win32::Foundation::E_INVALIDARG, "format")),
        };
        let stream = f.CreateStream()?;
        stream.InitializeFromFilename(&HSTRING::from(out.as_os_str()), GENERIC_WRITE)?;
        let enc = f.CreateEncoder(&guid, std::ptr::null())?;
        enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        let mut bag = None;
        enc.CreateNewFrame(&mut frame, &mut bag)?;
        let frame = frame.unwrap();
        if matches!(ext, "jpg" | "jpeg" | "heic" | "heif") {
            if let Some(b) = bag.clone() { property(&IPropertyBag2Wrap(b), w!("ImageQuality"), o.quality); }
        }
        frame.Initialize(bag.as_ref())?;
        frame.SetSize(b.w, b.h)?;
        // JPEG has no alpha; BMP and GIF readers handle 24-bit best.
        let source = f.CreateBitmapFromMemory(b.w, b.h, &GUID_WICPixelFormat32bppBGRA, b.w * 4, &b.px)?;
        let mut fmt = if matches!(ext, "jpg" | "jpeg" | "bmp") { GUID_WICPixelFormat24bppBGR } else { GUID_WICPixelFormat32bppBGRA };
        frame.SetPixelFormat(&mut fmt)?;
        let conv: IWICBitmapSource = WICConvertBitmapSource(&fmt, &source)?;
        frame.WriteSource(&conv, std::ptr::null())?;
        frame.Commit()?;
        enc.Commit()?;
        Ok(())
    }
}

/// Saves a bitmap, flattening alpha where the format has none. Output must not exist yet.
pub fn save(b: &Bmp, out: &Path, o: &SaveOpts) -> std::result::Result<(), String> {
    let ext = crate::catalog::ext_of(out);
    let ext = ext.split('.').next_back().unwrap_or("").to_string();
    let mut tmp;
    let b = if matches!(ext.as_str(), "jpg" | "jpeg" | "bmp") && b.has_alpha() { tmp = b.clone(); tmp.flatten_white(); &tmp } else { b };
    match ext.as_str() {
        "webp" | "avif" => {
            let ff = crate::ffmpeg().ok_or("Needs ffmpeg")?;
            let t = TempDir::new()?;
            let png = t.file("in.png");
            wic_save(b, &png, "png", &SaveOpts::default()).map_err(es)?;
            let q = (o.quality * 100.0) as i32;
            let mut a: Vec<String> = vec!["-y".into(), "-hide_banner".into(), "-i".into(), png.display().to_string()];
            if ext == "webp" {
                a.extend(["-c:v".into(), "libwebp".into(), "-quality".into(), q.to_string()]);
            } else {
                let crf = ((1.0 - o.quality) * 63.0) as i32 + 8;
                a.extend(["-c:v".into(), "libaom-av1".into(), "-still-picture".into(), "1".into(), "-crf".into(), crf.to_string(), "-cpu-used".into(), "6".into()]);
            }
            a.extend(["-frames:v".into(), "1".into(), out.display().to_string()]);
            run(None, &ff, &a, None).map(|_| ())
        }
        "heic" | "heif" => wic_save(b, out, &ext, o).map_err(|_| {
            let _ = std::fs::remove_file(out);
            "HEIC output needs the HEIF Image and HEVC Video Extensions from the Microsoft Store".to_string()
        }),
        _ => wic_save(b, out, &ext, o).map_err(es),
    }
}

pub fn write_rgba_png(rgba: &[u8], w: u32, h: u32, out: &Path) -> std::result::Result<(), String> {
    let mut px = rgba.to_vec();
    for p in px.chunks_exact_mut(4) { p.swap(0, 2); }
    let _ = std::fs::remove_file(out);
    wic_save(&Bmp { w, h, px }, out, "png", &SaveOpts::default()).map_err(es)
}

pub fn convert(job: &Job, input: &Path, target: &str) -> std::result::Result<PathBuf, String> {
    let bmp = load(input)?;
    if job.cancelled() { return Err("Cancelled".into()); }
    job.set_frac(0.5);
    let out = sibling(input, "", target);
    atomically(&out, |tmp| save(&bmp, tmp, &SaveOpts::default()))
}

/// Removes metadata without touching the pixels. Returns None for anything it cannot walk safely.
pub fn strip_metadata(ext: &str, b: &[u8]) -> Option<Vec<u8>> {
    match ext { "jpg" | "jpeg" => strip_jpeg(b), "png" => strip_png(b), _ => None }
}

fn exif_orientation_in(app1: &[u8]) -> Option<u16> {
    let t = app1.strip_prefix(b"Exif\0\0")?;
    let le = match t.get(..2)? { b"II" => true, b"MM" => false, _ => return None };
    let u16_at = |o: usize| -> Option<u16> { let s = t.get(o..o + 2)?; Some(if le { u16::from_le_bytes([s[0], s[1]]) } else { u16::from_be_bytes([s[0], s[1]]) }) };
    let s = t.get(4..8)?;
    let ifd = if le { u32::from_le_bytes([s[0], s[1], s[2], s[3]]) } else { u32::from_be_bytes([s[0], s[1], s[2], s[3]]) } as usize;
    for i in 0..u16_at(ifd)? as usize {
        let e = ifd + 2 + i * 12;
        if u16_at(e)? == 0x0112 { return u16_at(e + 8); }
    }
    None
}

/// Keeps JFIF, the colour profile and Adobe's colour marker, which change how pixels decode. The rest goes.
/// Orientation survives as the only EXIF tag, or a stripped phone photo would display sideways.
fn strip_jpeg(b: &[u8]) -> Option<Vec<u8>> {
    if b.get(..2)? != [0xFF, 0xD8] { return None; }
    let mut out = vec![0xFF, 0xD8];
    let mut kept = vec![];
    let mut orient = None;
    let mut i = 2;
    loop {
        if *b.get(i)? != 0xFF { return None; }
        let m = *b.get(i + 1)?;
        if m == 0xFF { i += 1; continue; }
        if m == 0xDA { break; }
        let len = u16::from_be_bytes([*b.get(i + 2)?, *b.get(i + 3)?]) as usize;
        if len < 2 { return None; }
        let seg = b.get(i..i + 2 + len)?;
        let body = &seg[4..];
        if m == 0xE1 && orient.is_none() { orient = exif_orientation_in(body); }
        let keep = match m {
            0xE0 => body.starts_with(b"JFIF"),
            0xE2 => body.starts_with(b"ICC_PROFILE"),
            0xEE => body.starts_with(b"Adobe"),
            0xE1 | 0xE3..=0xED | 0xEF | 0xFE => false,
            _ => true,
        };
        if keep { kept.extend_from_slice(seg); }
        i += 2 + len;
    }
    // JFIF must stay first, so the rebuilt EXIF goes after it.
    let jfif = if kept.get(1) == Some(&0xE0) { 2 + u16::from_be_bytes([kept[2], kept[3]]) as usize } else { 0 };
    out.extend_from_slice(&kept[..jfif]);
    if let Some(o @ 2..=8) = orient {
        out.extend_from_slice(&[0xFF, 0xE1, 0, 34]);
        out.extend_from_slice(b"Exif\0\0II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0");
        out.extend_from_slice(&o.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    }
    out.extend_from_slice(&kept[jfif..]);
    out.extend_from_slice(&b[i..]);
    Some(out)
}

fn strip_png(b: &[u8]) -> Option<Vec<u8>> {
    const SIG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if b.get(..8)? != SIG { return None; }
    let mut out = SIG.to_vec();
    let mut i = 8;
    while i < b.len() {
        let len = u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?) as usize;
        let end = i.checked_add(12)?.checked_add(len)?;
        let chunk = b.get(i..end)?;
        if !matches!(&chunk[4..8], b"tEXt" | b"iTXt" | b"zTXt" | b"eXIf" | b"tIME") { out.extend_from_slice(chunk); }
        i = end;
    }
    Some(out)
}

/// Parses "#RRGGBB" to BGRA.
pub fn parse_color(s: &str) -> [u8; 4] {
    let h = s.trim().trim_start_matches('#');
    let v = u32::from_str_radix(h, 16).unwrap_or(0xFFFFFF);
    [(v & 255) as u8, ((v >> 8) & 255) as u8, ((v >> 16) & 255) as u8, 255]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn color_parses() { assert_eq!(parse_color("#102030"), [0x30, 0x20, 0x10, 255]); }
    #[test]
    fn crop_clamps_to_bounds() { let b = Bmp::new(10, 10, [0, 0, 0, 255]); let c = b.crop(8, 8, 50, 50); assert_eq!((c.w, c.h), (2, 2)); }
}

#[cfg(test)]
mod strip_tests {
    use super::*;
    fn seg(m: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![0xFF, m];
        v.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
        v.extend_from_slice(body);
        v
    }
    fn exif(orient: u16) -> Vec<u8> {
        let mut e = b"Exif\0\0II*\0\x08\0\0\0\x02\0\x0F\x01\x02\0\x05\0\0\0\x26\0\0\0\x12\x01\x03\0\x01\0\0\0".to_vec();
        e.extend_from_slice(&orient.to_le_bytes());
        e.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        e.extend_from_slice(b"SECRET\0");
        e
    }
    fn jpeg(extra: &[Vec<u8>]) -> Vec<u8> {
        let mut j = vec![0xFF, 0xD8];
        j.extend(seg(0xE0, b"JFIF\0data"));
        for e in extra { j.extend_from_slice(e); }
        j.extend(seg(0xDB, b"quant"));
        j.extend_from_slice(&[0xFF, 0xDA, 0, 2, 1, 2, 3, 0xFF, 0xD9]);
        j
    }
    fn has(h: &[u8], n: &[u8]) -> bool { h.windows(n.len()).any(|w| w == n) }
    #[test]
    fn jpeg_loses_exif_xmp_and_comments_but_keeps_scan() {
        let j = jpeg(&[seg(0xE1, &exif(1)), seg(0xE1, b"http://ns.adobe.com/xap/SECRET"), seg(0xED, b"Photoshop SECRET"), seg(0xFE, b"SECRET")]);
        let o = strip_jpeg(&j).unwrap();
        assert!(!has(&o, b"SECRET") && !has(&o, b"Exif"));
        assert!(has(&o, b"JFIF") && has(&o, b"quant") && o.ends_with(&[0xFF, 0xDA, 0, 2, 1, 2, 3, 0xFF, 0xD9]));
    }
    #[test]
    fn jpeg_keeps_only_orientation_and_it_still_reads() {
        let o = strip_jpeg(&jpeg(&[seg(0xE1, &exif(6))])).unwrap();
        assert!(!has(&o, b"SECRET"));
        let at = o.windows(6).position(|w| w == b"Exif\0\0").unwrap();
        assert_eq!(exif_orientation_in(&o[at..]), Some(6));
        assert!(has(&o[..24], b"JFIF"), "JFIF stays first");
    }
    #[test]
    fn jpeg_keeps_colour_profile() {
        assert!(has(&strip_jpeg(&jpeg(&[seg(0xE2, b"ICC_PROFILE\0x")])).unwrap(), b"ICC_PROFILE"));
    }
    #[test]
    fn truncated_files_are_refused_not_mangled() {
        assert!(strip_jpeg(&[0xFF, 0xD8, 0xFF, 0xE1, 0xFF]).is_none());
        assert!(strip_png(b"\x89PNG\r\n\x1a\n\0\0\0\xFFtEXt").is_none());
        assert!(strip_metadata("jpg", b"not a jpeg").is_none());
    }
    #[test]
    fn png_loses_text_chunks_only() {
        let ch = |t: &[u8], d: &[u8]| { let mut v = (d.len() as u32).to_be_bytes().to_vec(); v.extend_from_slice(t); v.extend_from_slice(d); v.extend_from_slice(&[0; 4]); v };
        let mut p = b"\x89PNG\r\n\x1a\n".to_vec();
        for c in [ch(b"IHDR", b"hdr"), ch(b"tEXt", b"SECRET"), ch(b"eXIf", b"SECRET"), ch(b"IDAT", b"pix"), ch(b"IEND", b"")] { p.extend(c); }
        let o = strip_png(&p).unwrap();
        assert!(!has(&o, b"SECRET") && has(&o, b"IDAT") && has(&o, b"IEND"));
    }
}
