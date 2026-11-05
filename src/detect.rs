//! Finds faces and words in a picture using what ships with Windows. Runs on a worker thread.
use std::path::Path;
use windows::core::*;
use windows::Graphics::Imaging::*;
use windows::Media::FaceAnalysis::FaceDetector;
use windows::Media::Ocr::OcrEngine;
use windows::Storage::*;

/// Regions as (x, y, w, h) fractions of the image.
pub fn regions(p: &Path, w: u32, h: u32) -> Result<Vec<(f64, f64, f64, f64)>> {
    let abs = std::fs::canonicalize(p).map_err(|_| Error::from_hresult(windows::Win32::Foundation::E_FAIL))?;
    let s = abs.display().to_string();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s).to_string();
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(s))?.join()?;
    let stream = file.OpenAsync(FileAccessMode::Read)?.join()?;
    let dec = BitmapDecoder::CreateAsync(&stream)?.join()?;
    let sb = dec.GetSoftwareBitmapAsync()?.join()?;
    let (fw, fh) = (w.max(1) as f64, h.max(1) as f64);
    let mut out = vec![];
    if FaceDetector::IsSupported().unwrap_or(false) {
        let gray = SoftwareBitmap::Convert(&sb, BitmapPixelFormat::Gray8)?;
        let det = FaceDetector::CreateAsync()?.join()?;
        for f in det.DetectFacesAsync(&gray)?.join()? {
            let b = f.FaceBox()?;
            out.push((b.X as f64 / fw, b.Y as f64 / fh, b.Width as f64 / fw, b.Height as f64 / fh));
        }
    }
    if let Ok(eng) = OcrEngine::TryCreateFromUserProfileLanguages() {
        if let Ok(res) = eng.RecognizeAsync(&sb).and_then(|a| a.join()) {
            for line in res.Lines()? {
                for word in line.Words()? {
                    let b = word.BoundingRect()?;
                    out.push((b.X as f64 / fw, b.Y as f64 / fh, b.Width as f64 / fw, b.Height as f64 / fh));
                }
            }
        }
    }
    Ok(out)
}
