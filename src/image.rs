//! Image decode and encode through WIC, which ships with Windows. ffmpeg covers what WIC cannot.
use crate::job::*;
use std::path::{Path, PathBuf};
use windows::core::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::Foundation::GENERIC_ACCESS_RIGHTS;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::*;
use windows::Win32::System::Variant::VARIANT;

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
            run(None, &ff, &["-y".into(), "-hide_banner".into(), "-i".into(), p.display().to_string(), "-frames:v".into(), "1".into(), "-sws_flags".into(), "lanczos+accurate_rnd+full_chroma_int".into(), "-compression_level".into(), "0".into(), tmp.display().to_string()], None)
                .map_err(|e| format!("Could not read {}: {e}", p.file_name().unwrap_or_default().to_string_lossy()))?;
            wic_load(&tmp).map_err(es)
        }
    }
}

/// Quality 1.0 is the top of the scale, and it means truly lossless for formats that can be.
pub struct SaveOpts {
    pub quality: f32,
    /// A lossy source's size in bytes. WebP and AVIF then keep the best setting that does not outgrow it, in place of a
    /// lossless copy that would be several times the size and show nothing more.
    pub fit: Option<u64>,
    /// A quick speed swaps AVIF's slow lossless encode for a fast lossy one, and ignores `quality` and `fit`.
    pub speed: crate::quality::Speed,
}
impl Default for SaveOpts { fn default() -> Self { SaveOpts { quality: 1.0, fit: None, speed: Default::default() } } }

fn property(bag: &IPropertyBag2Wrap, name: PCWSTR, v: f32) { bag.write(name, VARIANT::from(v)); }

struct IPropertyBag2Wrap(windows::Win32::System::Com::StructuredStorage::IPropertyBag2);
impl IPropertyBag2Wrap {
    fn write(&self, name: PCWSTR, val: VARIANT) {
        use windows::Win32::System::Com::StructuredStorage::*;
        unsafe {
            let opt = PROPBAG2 { pstrName: PWSTR(name.0 as *mut u16), ..Default::default() };
            let _ = self.0.Write(1, &opt, &val);
        }
    }
}

/// A colour profile and print resolution to carry across, when there are any.
#[derive(Clone, Default)]
struct Carry { color: Option<IWICColorContext>, dpi: (f64, f64) }

/// Encodes any WIC source via WIC, at the source's own depth when the format has room for it.
/// The container comes from the extension.
fn wic_encode(f: &IWICImagingFactory, src: &IWICBitmapSource, out: &Path, ext: &str, o: &SaveOpts, carry: &Carry) -> Result<()> {
    unsafe {
        let guid = match ext {
            "png" => GUID_ContainerFormatPng,
            "jpg" | "jpeg" => GUID_ContainerFormatJpeg,
            "tif" | "tiff" => GUID_ContainerFormatTiff,
            "bmp" => GUID_ContainerFormatBmp,
            "gif" => GUID_ContainerFormatGif,
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
        if let Some(b) = bag.clone() {
            let b = IPropertyBag2Wrap(b);
            if matches!(ext, "jpg" | "jpeg") { property(&b, w!("ImageQuality"), o.quality); }
            // WIC halves JPEG colour resolution unless told otherwise. Only the smallest compress setting wants that.
            if matches!(ext, "jpg" | "jpeg") && o.quality >= 0.7 { b.write(w!("JpegYCrCbSubsampling"), VARIANT::from(WICJpegYCrCbSubsampling444.0 as u8)); }
        }
        frame.Initialize(bag.as_ref())?;
        let (mut w, mut h) = (0, 0);
        src.GetSize(&mut w, &mut h)?;
        frame.SetSize(w, h)?;
        if carry.dpi.0 > 0.0 && carry.dpi.1 > 0.0 { let _ = frame.SetResolution(carry.dpi.0, carry.dpi.1); }
        let src_fmt = src.GetPixelFormat()?;
        // JPEG has no alpha; BMP readers handle 24-bit best. Grey stays grey. Everything else offers its own
        // format, and the encoder answers with the closest it can write.
        let grey = src_fmt == GUID_WICPixelFormat8bppGray || src_fmt == GUID_WICPixelFormat16bppGray;
        let mut fmt = if matches!(ext, "jpg" | "jpeg" | "bmp") && !grey { GUID_WICPixelFormat24bppBGR } else { src_fmt };
        frame.SetPixelFormat(&mut fmt)?;
        // Not every format can hold a profile, and a picture is still worth saving without.
        if let Some(c) = &carry.color { let _ = frame.SetColorContexts(&[Some(c.clone())]); }
        if fmt == src_fmt {
            frame.WriteSource(src, std::ptr::null())?;
        } else {
            let conv: IWICBitmapSource = WICConvertBitmapSource(&fmt, src)?;
            frame.WriteSource(&conv, std::ptr::null())?;
        }
        frame.Commit()?;
        enc.Commit()?;
        Ok(())
    }
}

fn wic_save(b: &Bmp, out: &Path, ext: &str, o: &SaveOpts, carry: &Carry) -> Result<()> {
    unsafe {
        let f = factory()?;
        let source = f.CreateBitmapFromMemory(b.w, b.h, &GUID_WICPixelFormat32bppBGRA, b.w * 4, &b.px)?;
        wic_encode(&f, &source.cast()?, out, ext, o, carry)
    }
}

/// WebP and AVIF through ffmpeg, from a PNG that holds everything the picture has.
fn ffmpeg_still(png: &Path, out: &Path, ext: &str, o: &SaveOpts) -> std::result::Result<(), String> {
    let ff = crate::ffmpeg().ok_or("Needs ffmpeg")?;
    let lossless = o.quality >= 1.0;
    let aom = |crf: i32| -> Vec<String> { vec!["-c:v".into(), "libaom-av1".into(), "-still-picture".into(), "1".into(), "-crf".into(), crf.to_string(), "-pix_fmt".into(), "yuv444p".into(), "-cpu-used".into(), "4".into()] };
    let quick = if ext == "avif" { crate::quality::avif_quick_args(o.speed) } else { None };
    let encoders: Vec<Vec<String>> = if let Some(q) = quick { vec![q] } else { match (ext, lossless) {
        (_, true) if o.fit.is_some() => crate::quality::still_ladder(ext),
        ("webp", true) => vec![crate::quality::image_encode_args("webp")],
        ("webp", false) => vec![vec!["-c:v".into(), "libwebp".into(), "-quality".into(), ((o.quality * 100.0) as i32).to_string()]],
        // A build that refuses RGB AVIF still gets the closest thing to lossless it can do.
        (_, true) => vec![crate::quality::image_encode_args("avif"), aom(0)],
        (_, false) => vec![aom(((1.0 - o.quality) * 63.0) as i32 + 8)],
    } };
    let mut last = Err("Nothing to do".to_string());
    for enc in encoders {
        let mut a: Vec<String> = vec!["-y".into(), "-hide_banner".into(), "-i".into(), png.display().to_string()];
        a.extend(enc);
        a.extend(["-frames:v".into(), "1".into(), out.display().to_string()]);
        last = run(None, &ff, &a, None).map(|_| ());
        if last.is_err() { continue; }
        // Without a size to fit, the first that works is the one. With one, the first that fits; the last tried otherwise.
        let Some(limit) = o.fit.filter(|_| o.speed == crate::quality::Speed::Best) else { break };
        if std::fs::metadata(out).is_ok_and(|m| m.len() <= limit) { break; }
    }
    last
}

/// HEIC through ffmpeg's x265, so that it needs nothing from the Store. A lossy source takes the best setting that does
/// not outgrow it, as with WebP and AVIF.
fn save_heic(b: &Bmp, out: &Path, o: &SaveOpts, carry: &Carry) -> std::result::Result<(), String> {
    let ff = crate::ffmpeg().ok_or("Needs ffmpeg")?;
    let t = TempDir::new()?;
    let png = t.file("in.png");
    wic_save(b, &png, "png", &SaveOpts::default(), &Carry::default()).map_err(es)?;
    let size = crate::heif::size_for(b.w, b.h);
    let icc = carry.color.as_ref().and_then(profile_bytes);
    let encode = |crf: u32, alpha: bool| -> std::result::Result<crate::heif::Stream, String> {
        let f = t.file(if alpha { "a.hevc" } else { "p.hevc" });
        let path = f.display().to_string();
        let args = if alpha { crate::heif::alpha_args(size, crf, &path) } else { crate::heif::picture_args(size, crf, &path) };
        let mut a: Vec<String> = vec!["-y".into(), "-hide_banner".into(), "-i".into(), png.display().to_string()];
        a.extend(args);
        run(None, &ff, &a, None).map_err(|e| if e.contains("libx265") { "This ffmpeg has no HEVC encoder (libx265), which HEIC needs".to_string() } else { e })?;
        crate::heif::parse_stream(&std::fs::read(&f).map_err(es_io)?)
    };
    // Transparency has only flat edges to store, and softening them shows, so it stays near the top of the scale.
    let alpha = if b.has_alpha() { Some(encode(10, true)?) } else { None };
    let top = 12 + ((1.0 - o.quality.clamp(0.0, 1.0)) * 30.0) as u32;
    let ladder: Vec<u32> = if o.fit.is_some() { vec![14, 18, 22, 26, 30] } else { vec![top] };
    let mut file = vec![];
    for crf in ladder {
        file = crate::heif::mux(&encode(crf, false)?, alpha.as_ref(), size, icc.as_deref())?;
        if o.fit.is_none_or(|limit| file.len() as u64 <= limit) { break; }
    }
    std::fs::write(out, file).map_err(es_io)
}

/// The bytes of a colour profile that came from a file, rather than one Windows knows by name.
fn profile_bytes(c: &IWICColorContext) -> Option<Vec<u8>> {
    unsafe {
        if c.GetType().ok()? != WICColorContextProfile { return None; }
        let mut n = 0;
        c.GetProfileBytes(&mut [], &mut n).ok()?;
        let mut b = vec![0u8; n as usize];
        c.GetProfileBytes(&mut b, &mut n).ok()?;
        (!b.is_empty()).then_some(b)
    }
}

fn es_io(e: std::io::Error) -> String { e.to_string() }

fn save_carrying(b: &Bmp, out: &Path, o: &SaveOpts, carry: &Carry) -> std::result::Result<(), String> {
    let ext = crate::catalog::ext_of(out);
    let ext = ext.split('.').next_back().unwrap_or("").to_string();
    let mut tmp;
    let b = if matches!(ext.as_str(), "jpg" | "jpeg" | "bmp") && b.has_alpha() { tmp = b.clone(); tmp.flatten_white(); &tmp } else { b };
    match ext.as_str() {
        "webp" | "avif" => {
            let t = TempDir::new()?;
            let png = t.file("in.png");
            wic_save(b, &png, "png", &SaveOpts::default(), carry).map_err(es)?;
            ffmpeg_still(&png, out, &ext, o)
        }
        "heic" | "heif" => save_heic(b, out, o, carry),
        _ => wic_save(b, out, &ext, o, carry).map_err(es),
    }
}

/// Saves a bitmap, flattening alpha where the format has none. Output must not exist yet.
pub fn save(b: &Bmp, out: &Path, o: &SaveOpts) -> std::result::Result<(), String> {
    save_carrying(b, out, o, &Carry::default())
}

// ---- pictures at their stored depth ----

/// Layout of a pixel format that can be edited in place: bytes per pixel, and the offset of alpha if there is any.
fn layout(fmt: &GUID) -> Option<(usize, Option<usize>)> {
    Some(match *fmt {
        f if f == GUID_WICPixelFormat8bppGray => (1, None),
        f if f == GUID_WICPixelFormat16bppGray => (2, None),
        f if f == GUID_WICPixelFormat24bppBGR || f == GUID_WICPixelFormat24bppRGB => (3, None),
        f if f == GUID_WICPixelFormat32bppBGR => (4, None),
        f if f == GUID_WICPixelFormat32bppBGRA || f == GUID_WICPixelFormat32bppRGBA => (4, Some(3)),
        f if f == GUID_WICPixelFormat48bppRGB || f == GUID_WICPixelFormat48bppBGR => (6, None),
        f if f == GUID_WICPixelFormat64bppRGBA || f == GUID_WICPixelFormat64bppBGRA => (8, Some(6)),
        _ => return None,
    })
}

/// The same formats, described sample by sample.
fn samples_of(fmt: &GUID, w: u32, h: u32, stride: usize) -> Option<crate::redact::Layout> {
    let (sample_bytes, channels, rgb, alpha) = match *fmt {
        f if f == GUID_WICPixelFormat8bppGray => (1, 1, [0, 0, 0], None),
        f if f == GUID_WICPixelFormat16bppGray => (2, 1, [0, 0, 0], None),
        f if f == GUID_WICPixelFormat24bppBGR => (1, 3, [2, 1, 0], None),
        f if f == GUID_WICPixelFormat24bppRGB => (1, 3, [0, 1, 2], None),
        f if f == GUID_WICPixelFormat32bppBGR => (1, 4, [2, 1, 0], None),
        f if f == GUID_WICPixelFormat32bppBGRA => (1, 4, [2, 1, 0], Some(3)),
        f if f == GUID_WICPixelFormat32bppRGBA => (1, 4, [0, 1, 2], Some(3)),
        f if f == GUID_WICPixelFormat48bppRGB => (2, 3, [0, 1, 2], None),
        f if f == GUID_WICPixelFormat48bppBGR => (2, 3, [2, 1, 0], None),
        f if f == GUID_WICPixelFormat64bppRGBA => (2, 4, [0, 1, 2], Some(3)),
        f if f == GUID_WICPixelFormat64bppBGRA => (2, 4, [2, 1, 0], Some(3)),
        _ => return None,
    };
    Some(crate::redact::Layout { w, h, stride, sample_bytes, channels, rgb, alpha })
}

/// A picture as decoded, upright, without the trip through 8-bit BGRA: 16-bit stays 16-bit, grey stays grey.
pub struct Native { bmp: IWICBitmap, carry: Carry }

/// The embedded ICC profile. Without it, wide-gamut pictures shift colour when saved.
fn profile_of(f: &IWICImagingFactory, frame: &IWICBitmapFrameDecode) -> Option<IWICColorContext> {
    unsafe {
        let mut n = 0u32;
        frame.GetColorContexts(&mut [], &mut n).ok()?;
        if n == 0 { return None; }
        let mut all: Vec<Option<IWICColorContext>> = (0..n).map(|_| f.CreateColorContext().ok()).collect();
        frame.GetColorContexts(&mut all, &mut n).ok()?;
        all.into_iter().flatten().find(|c| c.GetType().is_ok_and(|t| t == WICColorContextProfile))
    }
}

/// Indexed, CMYK and more exotic formats convert to a form with enough depth to hold them.
fn editable(f: &IWICImagingFactory, src: IWICBitmapSource) -> Result<IWICBitmapSource> {
    unsafe {
        let fmt = src.GetPixelFormat()?;
        if layout(&fmt).is_some() { return Ok(src); }
        let info: IWICPixelFormatInfo = f.CreateComponentInfo(&fmt)?.cast()?;
        let per_channel = info.GetBitsPerPixel()? / info.GetChannelCount()?.max(1);
        let to = if per_channel > 8 { GUID_WICPixelFormat64bppRGBA } else { GUID_WICPixelFormat32bppBGRA };
        WICConvertBitmapSource(&to, &src)
    }
}

fn orientation_transform(orient: u32) -> WICBitmapTransformOptions {
    match orient {
        2 => WICBitmapTransformFlipHorizontal,
        3 => WICBitmapTransformRotate180,
        4 => WICBitmapTransformFlipVertical,
        5 => WICBitmapTransformOptions(WICBitmapTransformRotate90.0 | WICBitmapTransformFlipHorizontal.0),
        6 => WICBitmapTransformRotate90,
        7 => WICBitmapTransformOptions(WICBitmapTransformRotate270.0 | WICBitmapTransformFlipHorizontal.0),
        8 => WICBitmapTransformRotate270,
        _ => WICBitmapTransformRotate0,
    }
}

fn wic_native(p: &Path) -> Result<Native> {
    unsafe {
        let f = factory()?;
        let dec = f.CreateDecoderFromFilename(&HSTRING::from(p.as_os_str()), None, GENERIC_ACCESS_RIGHTS(GENERIC_READ), WICDecodeMetadataCacheOnDemand)?;
        let frame = dec.GetFrame(0)?;
        let orient = exif_orientation(&frame);
        let color = profile_of(&f, &frame);
        let (mut dx, mut dy) = (0.0, 0.0);
        let _ = frame.GetResolution(&mut dx, &mut dy);
        if matches!(orient, 5..=8) { std::mem::swap(&mut dx, &mut dy); }
        let src = editable(&f, frame.cast()?)?;
        let rot = f.CreateBitmapFlipRotator()?;
        rot.Initialize(&src, orientation_transform(orient))?;
        // In memory, because the source file may be a temporary and the pixels are about to be edited.
        let bmp = f.CreateBitmapFromSource(&rot, WICBitmapCacheOnLoad)?;
        Ok(Native { bmp, carry: Carry { color, dpi: (dx, dy) } })
    }
}

/// Decodes at the stored depth through WIC, then ffmpeg for what has no codec installed.
pub fn load_native(p: &Path) -> std::result::Result<Native, String> {
    match wic_native(p) {
        Ok(n) => Ok(n),
        Err(wic_err) => {
            let Some(ff) = crate::ffmpeg() else { return Err(format!("{}: no decoder ({})", p.display(), es(wic_err))); };
            let t = TempDir::new()?;
            let tmp = t.file("in.png");
            // ffmpeg writes a deep source as 16-bit PNG by itself.
            run(None, &ff, &["-y".into(), "-hide_banner".into(), "-i".into(), p.display().to_string(), "-frames:v".into(), "1".into(), "-sws_flags".into(), "lanczos+accurate_rnd+full_chroma_int".into(), "-compression_level".into(), "0".into(), tmp.display().to_string()], None)
                .map_err(|e| format!("Could not read {}: {e}", p.file_name().unwrap_or_default().to_string_lossy()))?;
            wic_native(&tmp).map_err(es)
        }
    }
}

impl Native {
    pub fn size(&self) -> (u32, u32) {
        let (mut w, mut h) = (0, 0);
        unsafe { let _ = self.bmp.GetSize(&mut w, &mut h); }
        (w, h)
    }
    /// Paints redaction marks in the picture's own format. Nothing they do not cover is touched.
    pub fn apply(&self, marks: &[crate::redact::Mark]) -> std::result::Result<(), String> {
        unsafe {
            let fmt = self.bmp.GetPixelFormat().map_err(es)?;
            let (w, h) = self.size();
            let rect = WICRect { X: 0, Y: 0, Width: w as i32, Height: h as i32 };
            let lock = self.bmp.Lock(&rect, WICBitmapLockWrite.0 as u32).map_err(es)?;
            let stride = lock.GetStride().map_err(es)? as usize;
            let (mut size, mut data) = (0u32, std::ptr::null_mut());
            lock.GetDataPointer(&mut size, &mut data).map_err(es)?;
            let buf = std::slice::from_raw_parts_mut(data, size as usize);
            let l = samples_of(&fmt, w, h, stride).ok_or("Unsupported pixel format")?;
            crate::redact::apply(buf, &l, marks);
            Ok(())
        }
    }
    pub fn cropped(&self, x: u32, y: u32, w: u32, h: u32) -> std::result::Result<Native, String> {
        unsafe {
            let f = factory().map_err(es)?;
            let clip = f.CreateBitmapClipper().map_err(es)?;
            let rect = WICRect { X: x as i32, Y: y as i32, Width: w as i32, Height: h as i32 };
            clip.Initialize(&self.bmp, &rect).map_err(es)?;
            let bmp = f.CreateBitmapFromSource(&clip, WICBitmapCacheOnLoad).map_err(es)?;
            Ok(Native { bmp, carry: self.carry.clone() })
        }
    }
    fn to_bmp(&self) -> std::result::Result<Bmp, String> {
        unsafe {
            let (w, h) = self.size();
            let conv = WICConvertBitmapSource(&GUID_WICPixelFormat32bppBGRA, &self.bmp).map_err(es)?;
            let mut px = vec![0u8; (w * h * 4) as usize];
            conv.CopyPixels(std::ptr::null(), w * 4, &mut px).map_err(es)?;
            Ok(Bmp { w, h, px })
        }
    }
}

/// Saves at the picture's own depth, as far as the format allows. Output must not exist yet.
pub fn save_native(n: &Native, out: &Path, o: &SaveOpts) -> std::result::Result<(), String> {
    let ext = crate::catalog::ext_of(out);
    let ext = ext.split('.').next_back().unwrap_or("").to_string();
    let fmt = unsafe { n.bmp.GetPixelFormat() }.map_err(es)?;
    let has_alpha_channel = layout(&fmt).is_some_and(|(_, a)| a.is_some());
    // Transparency has to be flattened by hand, or converting away the channel reveals what was under it.
    if matches!(ext.as_str(), "jpg" | "jpeg" | "bmp") && has_alpha_channel {
        return save_carrying(&n.to_bmp()?, out, o, &n.carry);
    }
    let f = factory().map_err(es)?;
    let src: IWICBitmapSource = n.bmp.cast().map_err(es)?;
    match ext.as_str() {
        "webp" | "avif" => {
            let t = TempDir::new()?;
            let png = t.file("in.png");
            wic_encode(&f, &src, &png, "png", &SaveOpts::default(), &n.carry).map_err(es)?;
            ffmpeg_still(&png, out, &ext, o)
        }
        "heic" | "heif" => save_heic(&n.to_bmp()?, out, o, &n.carry),
        _ => wic_encode(&f, &src, out, &ext, o, &n.carry).map_err(es),
    }
}

/// EXIF orientation of a JPEG, 1 when it has none. Reads headers only.
pub fn jpeg_orientation(b: &[u8]) -> u16 {
    let mut i = 2;
    while let (Some(0xFF), Some(&m)) = (b.get(i), b.get(i + 1)) {
        if m == 0xDA || m == 0xD9 { break; }
        let Some(len) = b.get(i + 2..i + 4).map(|l| u16::from_be_bytes([l[0], l[1]]) as usize) else { break };
        if m == 0xE1 {
            if let Some(o) = b.get(i + 4..i + 2 + len).and_then(exif_orientation_in) { return o; }
        }
        i += 2 + len;
    }
    1
}

pub fn write_rgba_png(rgba: &[u8], w: u32, h: u32, out: &Path) -> std::result::Result<(), String> {
    let mut px = rgba.to_vec();
    for p in px.chunks_exact_mut(4) { p.swap(0, 2); }
    let _ = std::fs::remove_file(out);
    wic_save(&Bmp { w, h, px }, out, "png", &SaveOpts::default(), &Carry::default()).map_err(es)
}

pub fn convert(job: &Job, input: &Path, target: &str, speed: crate::quality::Speed) -> std::result::Result<PathBuf, String> {
    let n = load_native(input)?;
    if job.cancelled() { return Err("Cancelled".into()); }
    job.set_frac(0.5);
    let out = sibling(input, "", target);
    let bytes = std::fs::read(input).ok();
    let e = crate::catalog::ext_of(input);
    // A WebP says which kind it is in its first chunk.
    let lossy = matches!(e.as_str(), "jpg" | "jpeg" | "heic" | "heif" | "avif") || (e == "webp" && bytes.as_ref().is_some_and(|b| b.get(12..16) == Some(b"VP8 ")));
    let fit = if lossy { bytes.map(|b| b.len() as u64) } else { None };
    atomically(&out, |tmp| save_native(&n, tmp, &SaveOpts { fit, speed, ..Default::default() }))
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
