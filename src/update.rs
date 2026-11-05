//! Auto-update from GitHub Releases. Uses curl.exe, built into Windows 10 1803 and later, so the app needs no HTTP stack.
//! The repository comes from the CI build. Local builds have none and never update.
use crate::job::*;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

pub const REPO: Option<&str> = option_env!("GITHUB_REPOSITORY");
const DAY: u64 = 24 * 60 * 60;
const FIRST_CHECK_DELAY: u64 = 45;
static RUNNING: AtomicBool = AtomicBool::new(false);

/// True when `latest` is a newer dotted version than `current`. A leading `v` is ignored.
pub fn is_newer(latest: &str, current: &str) -> bool {
    let parts = |s: &str| -> Vec<u64> { s.trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    let (a, b) = (parts(latest), parts(current));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y { return x > y; }
    }
    false
}

/// First value of `"key": "value"` in a JSON document. Enough for the fixed shape of the releases API.
fn json_str(doc: &str, key: &str, from: usize) -> Option<(String, usize)> {
    let pat = format!("\"{key}\"");
    let i = doc[from..].find(&pat)? + from + pat.len();
    let rest = &doc[i..];
    let q = rest.find('"')?;
    if !rest[..q].trim_start().starts_with(':') { return None; }
    let body = &rest[q + 1..];
    let end = body.find('"')?;
    Some((body[..end].to_string(), i + q + 1 + end))
}

#[derive(Debug, PartialEq)]
pub struct Release { pub tag: String, pub msi_url: String, pub sha_url: Option<String> }

pub fn parse_release(doc: &str) -> Option<Release> {
    let (tag, _) = json_str(doc, "tag_name", 0)?;
    let (mut msi, mut sha) = (None, None);
    let mut pos = 0;
    while let Some((url, next)) = json_str(doc, "browser_download_url", pos) {
        let lower = url.to_ascii_lowercase();
        if lower.ends_with(".msi") { msi = Some(url); } else if lower.ends_with(".msi.sha256") { sha = Some(url); }
        pos = next;
    }
    Some(Release { tag, msi_url: msi?, sha_url: sha })
}

fn curl(args: &[&str]) -> Result<String, String> {
    let exe = PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())).join(r"System32\curl.exe");
    let mut a: Vec<String> = ["-fsSL", "--retry", "2", "--max-time", "300", "-A", "orrery-updater"].iter().map(|s| s.to_string()).collect();
    a.extend(args.iter().map(|s| s.to_string()));
    run(None, &exe, &a, None)
}

fn sha256_of(p: &Path) -> Result<String, String> {
    let exe = PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())).join(r"System32\certutil.exe");
    let out = run(None, &exe, &["-hashfile".into(), p.display().to_string(), "SHA256".into()], None)?;
    out.lines().nth(1).map(|l| l.replace(' ', "").to_ascii_lowercase()).ok_or_else(|| "No hash".to_string())
}

fn check() -> Result<Option<Release>, String> {
    let repo = REPO.ok_or("This is a local build with no update channel")?;
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let doc = curl(&["-H", "Accept: application/vnd.github+json", &url])?;
    let r = parse_release(&doc).ok_or("The latest release has no installer")?;
    Ok(is_newer(&r.tag, crate::VERSION).then_some(r))
}

/// Downloads, verifies, then hands over to msiexec and exits so the files are free to replace.
fn install(r: &Release) -> Result<(), String> {
    let dir = std::env::temp_dir().join("orrery-update");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let msi = dir.join("Orrery.msi");
    curl(&["-o", &msi.display().to_string(), &r.msi_url])?;
    // Without a published hash there is nothing to check a corrupted download against, so refuse it.
    let sha_url = r.sha_url.as_deref().ok_or("The release has no checksum")?;
    let want = curl(&[sha_url])?;
    let want = want.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    if want != sha256_of(&msi)? {
        let _ = std::fs::remove_file(&msi);
        return Err("The download did not match its checksum".into());
    }
    // Per-user MSI: quiet, no elevation. The short pause lets this process exit first; then the new version starts.
    let script = format!(
        "ping -n 3 127.0.0.1 >nul & msiexec /i \"{}\" /qn /norestart & start \"\" \"{}\"",
        msi.display(), crate::settings::exe()
    );
    Command::new("cmd.exe").raw_arg(format!("/c \"{script}\"")).creation_flags(NO_WINDOW).spawn().map_err(|e| e.to_string())?;
    crate::request_quit();
    Ok(())
}

fn wait_until_idle() {
    while crate::hud::busy() { std::thread::sleep(std::time::Duration::from_secs(5)); }
}

/// Silent daily check. Any failure just waits for the next one: being offline is normal for this app.
pub fn start_background() {
    if REPO.is_none() { return; }
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(FIRST_CHECK_DELAY));
        loop {
            if crate::settings::Settings::load().auto_update && !RUNNING.swap(true, Ordering::SeqCst) {
                if let Ok(Some(r)) = check() {
                    wait_until_idle();
                    let _ = install(&r);
                }
                RUNNING.store(false, Ordering::SeqCst);
            }
            std::thread::sleep(std::time::Duration::from_secs(DAY));
        }
    });
}

/// From the tray menu: same flow, but with visible feedback.
pub fn check_now() {
    spawn("Checking for updates".to_string(), vec![()], |job, _| {
        job.set_frac(0.3);
        if RUNNING.swap(true, Ordering::SeqCst) { return Err("Already checking".into()); }
        let res = check().and_then(|r| match r {
            Some(rel) => install(&rel).map(|_| format!("Installing {}", rel.tag)),
            None => Ok(format!("Version {} is the latest", crate::VERSION)),
        });
        RUNNING.store(false, Ordering::SeqCst);
        res.map(|m| { *job.note.lock().unwrap() = Some(m); PathBuf::new() })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn version_order() {
        assert!(is_newer("v0.1.10", "0.1.9"));
        assert!(!is_newer("v0.1.9", "0.1.9"));
        assert!(!is_newer("0.1.8", "0.1.9"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(is_newer("0.1.0.1", "0.1.0"));
    }
    #[test]
    fn picks_assets_from_release_json() {
        let d = r#"{"tag_name": "v0.1.5", "assets":[
            {"name":"x","browser_download_url":"https://e/Orrery-0.1.5.msi.sha256"},
            {"name":"y","browser_download_url":"https://e/Orrery-0.1.5.msi"}]}"#;
        let r = parse_release(d).unwrap();
        assert_eq!(r.tag, "v0.1.5");
        assert!(r.msi_url.ends_with(".msi"));
        assert!(r.sha_url.unwrap().ends_with(".sha256"));
    }
    #[test]
    fn release_without_installer_is_ignored() {
        assert_eq!(parse_release(r#"{"tag_name":"v1","assets":[]}"#), None);
    }
}
