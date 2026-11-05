//! Convert wheel: routes one target format to the engine for each input's category.
use crate::catalog::{classify, dominant, Cat};
use crate::job::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub fn one(job: &Job, input: &Path, target: &str) -> Result<PathBuf, String> {
    match classify(input) {
        Cat::Image if target == "pdf" => {
            let out = sibling(input, "", "pdf");
            atomically(&out, |tmp| crate::pdf::images_to_pdf(job, &[input.to_path_buf()], tmp))
        }
        Cat::Image => crate::image::convert(job, input, target),
        Cat::Video | Cat::Audio => crate::media::convert(job, input, target),
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

pub fn run(target: &str, paths: Vec<PathBuf>) -> Arc<Job> {
    let paths = matching(paths);
    let t = target.to_string();
    let title = format!("Converting to {}", target.to_ascii_uppercase());
    // Several pictures to PDF is one document, not a pile of one-page files.
    if target == "pdf" && paths.len() > 1 && dominant(&paths) == Cat::Image {
        return spawn(title, vec![paths], |j, ps: &Vec<PathBuf>| {
            let out = sibling(&ps[0], " combined", "pdf");
            atomically(&out, |tmp| crate::pdf::images_to_pdf(j, ps, tmp))
        });
    }
    spawn(title, paths, move |j, p: &PathBuf| one(j, p, &t))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mixed_selection_keeps_the_dominant_type() {
        let v = vec![PathBuf::from("a.jpg"), PathBuf::from("b.png"), PathBuf::from("c.pdf")];
        assert_eq!(matching(v), vec![PathBuf::from("a.jpg"), PathBuf::from("b.png")]);
    }
}
