//! Convert wheel: routes one target format to the engine for each input's category.
use crate::catalog::{classify, dominant, ext_of, Cat};
use crate::job::*;
use crate::quality::Speed;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What this conversion gives up, for the progress card.
fn set_detail(job: &Job, input: &Path, target: &str, speed: Speed) {
    use crate::quality::{document_notice, image_notice, media_notice};
    if speed != Speed::Best {
        *job.detail.lock().unwrap() = Some(format!("{} speed: quicker and smaller, at some cost in detail.", speed_label(speed)));
        return;
    }
    let n = match classify(input) {
        Cat::Image if target == "pdf" => None,
        Cat::Image => image_notice(&ext_of(input), target),
        Cat::Video | Cat::Audio => media_notice(&crate::media::probe(input), target),
        Cat::Pdf => document_notice("pdf", target),
        Cat::Doc => document_notice("doc", target),
        Cat::Slides => document_notice("slides", target),
        Cat::Sheet => document_notice("sheet", target),
        Cat::Epub => document_notice("epub", target),
        Cat::Sub => document_notice("sub", target),
        _ => None,
    };
    *job.detail.lock().unwrap() = n;
}

fn speed_label(s: Speed) -> &'static str {
    match s { Speed::Best => "Best", Speed::Quality => "Quality", Speed::Balanced => "Balanced", Speed::Fast => "Fast" }
}

/// True when converting this selection to `target` is slow enough that a quicker, lossier speed is worth offering.
/// Video is probed, since a stream that only needs copying is instant at any speed. Only a handful of files are looked
/// at, as this runs while the wheel waits.
pub fn offers_speeds(paths: &[PathBuf], target: &str) -> bool {
    let cat = dominant(paths);
    match cat {
        Cat::Image => target == "avif",
        Cat::Video => target == "gif" || paths.iter().filter(|p| classify(p) == cat).take(4)
            .any(|p| crate::quality::speed_matters(&crate::media::probe(p), target)),
        _ => false,
    }
}

/// The preset list, slowest first. Long enough labels that the form draws a dropdown, not a row of buttons.
const PRESETS: [(Speed, &str); 4] = [
    (Speed::Best, "Best: keeps everything, slowest"),
    (Speed::Quality, "Quality: near identical, quicker"),
    (Speed::Balanced, "Balanced: good, much quicker"),
    (Speed::Fast, "Fast: rough, quickest"),
];

pub fn preset_fields(target: &str) -> Vec<crate::form::Field> {
    let note = match target {
        "gif" => "Quicker presets lower the frame rate and size: Quality 20 fps at 800 px, Balanced 15 fps at 640 px, Fast 12 fps at 480 px.",
        "avif" => "Best is lossless. The others are lossy and far smaller.",
        _ => "Quicker presets use faster encoder settings. Size and frame rate stay the same.",
    };
    let labels: Vec<&str> = PRESETS.iter().map(|p| p.1).collect();
    vec![crate::form::choice("Preset", &labels, 2), crate::form::Field::Note(note.into())]
}

/// For a slow conversion, asks which preset to use; anything else is Best without asking. None when cancelled.
pub fn ask_speed(target: &str, paths: &[PathBuf]) -> Option<Speed> {
    if !offers_speeds(paths, target) { return Some(Speed::Best); }
    let m = matching(paths.to_vec());
    crate::form::set_context(match m.as_slice() {
        [one] => one.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        many => format!("{} files", many.len()),
    });
    let v = crate::form::show(&format!("Convert to {}", target.to_ascii_uppercase()), &preset_fields(target), "Convert")?;
    PRESETS.iter().find(|p| p.1 == v[0]).map(|p| p.0)
}

pub fn one(job: &Job, input: &Path, target: &str, speed: Speed) -> Result<PathBuf, String> {
    set_detail(job, input, target, speed);
    match classify(input) {
        Cat::Image if target == "pdf" => {
            let out = sibling(input, "", "pdf");
            atomically(&out, |tmp| crate::pdf::images_to_pdf(job, &[input.to_path_buf()], tmp))
        }
        Cat::Image => crate::image::convert(job, input, target, speed),
        Cat::Video | Cat::Audio => crate::media::convert(job, input, target, speed),
        Cat::Pdf => crate::docs::convert_pdf(job, input, target),
        Cat::Doc => crate::docs::convert_doc(job, input, target),
        Cat::Slides => crate::docs::convert_slides(job, input, target),
        Cat::Sheet => crate::docs::convert_sheet(job, input, target),
        Cat::Epub => crate::docs::convert_epub(job, input, target),
        Cat::Sub => crate::docs::convert_subs(input, target),
        Cat::Archive | Cat::Folder => crate::docs::convert_archive(job, input, target),
        Cat::Other => Err(format!("{} has no conversions", input.file_name().unwrap_or_default().to_string_lossy())),
    }
}

/// The wheel showed targets for the dominant type, so files of other types in the selection sit this one out.
pub fn matching(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let cat = dominant(&paths);
    paths.into_iter().filter(|p| classify(p) == cat).collect()
}

pub fn run(target: &str, speed: Speed, paths: Vec<PathBuf>) -> Arc<Job> {
    let paths = matching(paths);
    let t = target.to_string();
    let title = match speed {
        Speed::Best => format!("Converting to {}", target.to_ascii_uppercase()),
        s => format!("Converting to {} ({})", target.to_ascii_uppercase(), speed_label(s).to_ascii_lowercase()),
    };
    // Several pictures to PDF is one document, not a pile of one-page files.
    if target == "pdf" && paths.len() > 1 && dominant(&paths) == Cat::Image {
        return spawn(title, vec![paths], |j, ps: &Vec<PathBuf>| {
            let out = sibling(&ps[0], " combined", "pdf");
            atomically(&out, |tmp| crate::pdf::images_to_pdf(j, ps, tmp))
        });
    }
    // Files whose outputs could compete for a name go one at a time, so each still gets the name it always would.
    let workers = if names_may_clash(&paths) { 1 } else { workers_for(dominant(&paths), &t) };
    spawn_with(title, paths, workers, move |j, p: &PathBuf| one(j, p, &t, speed))
}

/// True when two files in one folder share a name, or one name is another's with " (n)" added, as "photo.jpg" and
/// "photo.png" or "photo (2).jpg" are. Their outputs are numbered in the order they are made.
fn names_may_clash(paths: &[PathBuf]) -> bool {
    let key = |p: &PathBuf| (p.parent().map(Path::to_path_buf), p.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default());
    let keys: Vec<_> = paths.iter().map(key).collect();
    keys.iter().enumerate().any(|(i, (d, a))| keys.iter().skip(i + 1).any(|(e, b)| d == e && (a == b || a.starts_with(&format!("{b} (")) || b.starts_with(&format!("{a} (")))))
}

/// How many files of a batch convert at once. Pictures, sound and text each keep about one core busy, so several run
/// side by side. Video already uses every core. Office, Edge and PDF rendering stay one at a time, as they are driven
/// through one application.
fn workers_for(cat: Cat, target: &str) -> usize {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    match cat {
        Cat::Video if target == "gif" => 1,
        Cat::Image | Cat::Audio | Cat::Sub => cores.min(6),
        Cat::Video | Cat::Archive => 2.min(cores),
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mixed_selection_keeps_the_dominant_type() {
        let v = vec![PathBuf::from("a.jpg"), PathBuf::from("b.png"), PathBuf::from("c.pdf")];
        assert_eq!(matching(v), vec![PathBuf::from("a.jpg"), PathBuf::from("b.png")]);
    }
    #[test]
    fn files_that_could_share_an_output_name_are_noticed() {
        let p = |v: &[&str]| v.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert!(names_may_clash(&p(&["d/photo.jpg", "d/Photo.png"])));
        assert!(names_may_clash(&p(&["d/photo.jpg", "d/photo (2).png"])));
        assert!(!names_may_clash(&p(&["d/photo.jpg", "e/photo.png"])));
        assert!(!names_may_clash(&p(&["d/a.jpg", "d/b.jpg", "d/ab.jpg"])));
    }
    #[test]
    fn speeds_are_offered_for_slow_targets_only() {
        let p = |s: &str| vec![PathBuf::from(s)];
        assert!(offers_speeds(&p("a.mp4"), "gif"));
        assert!(offers_speeds(&p("a.png"), "avif"));
        assert!(!offers_speeds(&p("a.png"), "jpg"));
        assert!(!offers_speeds(&p("a.mp4"), "mp3"));
        assert!(!offers_speeds(&p("a.docx"), "pdf"));
    }
    #[test]
    fn presets_show_as_a_dropdown() {
        assert!(PRESETS.iter().any(|p| p.1.chars().count() > 24), "the form draws short lists as buttons");
    }
    /// Draws the preset window to TAU_RENDER for design review.
    #[test]
    #[ignore]
    fn render_preset_window() {
        crate::job::com_init();
        let out = std::env::var("TAU_RENDER").unwrap();
        crate::form::render("Convert to GIF", "clip.mp4", &preset_fields("gif"), "Convert", Path::new(&out)).unwrap();
    }
}
