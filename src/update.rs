//! Auto-update from GitHub Releases. Uses curl.exe, built into Windows 10 1803 and later, so the app needs no HTTP stack.
//! The repository comes from the CI build. Local builds have none and never update.
//!
//! An update runs in two processes:
//! 1. The app checks, downloads and verifies the MSI, copies its own exe to the temp folder as a helper, starts the
//!    helper with `--apply-update`, and quits.
//! 2. The helper waits until the app's process has really exited, so no file is locked and the single-instance mutex is
//!    free. It then runs msiexec, waits for the result, and starts the installed exe. If the install failed, it still
//!    starts whatever exe is there, so Tau always comes back.
//! The helper runs from the temp folder, so it never locks the files the installer replaces.
use crate::job::*;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const REPO: Option<&str> = option_env!("GITHUB_REPOSITORY");
/// Passed to the relaunched app so it can say it was updated.
pub const UPDATED_FLAG: &str = "--updated";
const APPLY_FLAG: &str = "--apply-update";
const DAY: u64 = 24 * 60 * 60;
const FIRST_CHECK_DELAY: u64 = 45;
/// How long the helper waits for the app to quit on its own before ending it.
const EXIT_WAIT_MS: u32 = 20_000;
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

fn system32(exe: &str) -> PathBuf {
    PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())).join("System32").join(exe)
}

fn curl(args: &[&str]) -> Result<String, String> {
    let mut a: Vec<String> = ["-fsSL", "--retry", "2", "--max-time", "300", "-A", "tau-updater"].iter().map(|s| s.to_string()).collect();
    a.extend(args.iter().map(|s| s.to_string()));
    run(None, &system32("curl.exe"), &a, None)
}

fn sha256_of(p: &Path) -> Result<String, String> {
    let out = run(None, &system32("certutil.exe"), &["-hashfile".into(), p.display().to_string(), "SHA256".into()], None)?;
    out.lines().nth(1).map(|l| l.replace(' ', "").to_ascii_lowercase()).ok_or_else(|| "No hash".to_string())
}

/// Everything the updater writes lives here, and is cleared on the next start.
fn work_dir() -> PathBuf { std::env::temp_dir().join("tau-update") }

fn log(line: &str) {
    use std::io::Write;
    let _ = std::fs::create_dir_all(work_dir());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(work_dir().join("update.log")) {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(f, "[{t}] {line}");
    }
}

fn check() -> Result<Option<Release>, String> {
    let repo = REPO.ok_or("This is a local build with no update channel")?;
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let doc = curl(&["-H", "Accept: application/vnd.github+json", &url])?;
    let r = parse_release(&doc).ok_or("The latest release has no installer")?;
    Ok(is_newer(&r.tag, crate::VERSION).then_some(r))
}

/// Downloads the MSI into a folder of its own for this version and checks it against the published hash.
fn download(r: &Release) -> Result<PathBuf, String> {
    // Without a published hash there is nothing to check a corrupted download against, so refuse it.
    let sha_url = r.sha_url.as_deref().ok_or("The release has no checksum")?;
    let want = curl(&[sha_url])?;
    let want = want.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    if want.len() != 64 { return Err("The release checksum is malformed".into()); }
    let dir = work_dir().join(r.tag.trim_start_matches('v'));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let msi = dir.join("Tau.msi");
    // A copy left by an earlier attempt is reused when it still matches.
    if msi.exists() && sha256_of(&msi).ok().as_deref() == Some(want.as_str()) { return Ok(msi); }
    let part = dir.join("Tau.msi.part");
    let _ = std::fs::remove_file(&part);
    curl(&["-o", &part.display().to_string(), &r.msi_url])?;
    if sha256_of(&part)? != want {
        let _ = std::fs::remove_file(&part);
        return Err("The download did not match its checksum".into());
    }
    let _ = std::fs::remove_file(&msi);
    std::fs::rename(&part, &msi).map_err(|e| e.to_string())?;
    Ok(msi)
}

/// Starts the helper and quits. Nothing is torn down unless the helper actually started.
fn hand_off(msi: &Path) -> Result<(), String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let helper = msi.with_file_name("tau-update.exe");
    std::fs::copy(&me, &helper).map_err(|e| format!("Could not stage the updater: {e}"))?;
    log(&format!("handing off to {} for {}", helper.display(), msi.display()));
    Command::new(&helper)
        .arg(APPLY_FLAG).arg(msi).arg(std::process::id().to_string()).arg(&me)
        .current_dir(helper.parent().unwrap_or(Path::new(".")))
        .creation_flags(NO_WINDOW)
        .spawn()
        .map_err(|e| format!("Could not start the updater: {e}"))?;
    crate::request_quit();
    Ok(())
}

fn install(r: &Release) -> Result<(), String> {
    let msi = download(r)?;
    hand_off(&msi)
}

/// Waits until no job is running and no window (an editor, Settings, a form) is open, so quitting loses nothing.
fn wait_until_idle() {
    while crate::hud::busy() || crate::ui::modal_open() { std::thread::sleep(Duration::from_secs(5)); }
}

/// Clears what an earlier update left behind. Logs are kept so a failed update can still be looked into.
fn clean_up() {
    let Ok(entries) = std::fs::read_dir(work_dir()) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "log") { continue; }
        let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
    }
}

/// Silent daily check. Any failure just waits for the next one: being offline is normal for this app.
pub fn start_background() {
    if REPO.is_none() { return; }
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(FIRST_CHECK_DELAY));
        clean_up();
        loop {
            if crate::settings::Settings::load().auto_update && !RUNNING.swap(true, Ordering::SeqCst) {
                match check() {
                    Ok(Some(r)) => {
                        // Download first, then wait for idle: the quiet part happens while the user works.
                        let res = download(&r).and_then(|msi| { wait_until_idle(); hand_off(&msi) });
                        if let Err(e) = res { log(&format!("update to {} failed: {e}", r.tag)); }
                    }
                    Ok(None) => {}
                    Err(e) => log(&format!("check failed: {e}")),
                }
                RUNNING.store(false, Ordering::SeqCst);
            }
            std::thread::sleep(Duration::from_secs(DAY));
        }
    });
}

/// From the tray menu: same flow, but with visible feedback.
pub fn check_now() {
    spawn("Checking for updates".to_string(), vec![()], |job, _| {
        if RUNNING.swap(true, Ordering::SeqCst) { return Err("Already checking".into()); }
        job.set_frac(0.2);
        let res = check().and_then(|r| match r {
            Some(rel) => {
                job.set_frac(0.5);
                install(&rel).map(|_| format!("Restarting into {}", rel.tag))
            }
            None => Ok(format!("Version {} is the latest", crate::VERSION)),
        });
        RUNNING.store(false, Ordering::SeqCst);
        res.map(|m| { *job.note.lock().unwrap() = Some(m); PathBuf::new() })
    });
}

/// Shows a card after a relaunch by the updater.
pub fn announce() {
    let job = std::sync::Arc::new(Job { title: "Tau updated".into(), total: 1, ..Default::default() });
    job.done.store(1, Ordering::Relaxed);
    *job.note.lock().unwrap() = Some(format!("Now on version {}", crate::VERSION));
    *job.finished_at.lock().unwrap() = Some(std::time::Instant::now());
    job.finished.store(true, Ordering::Relaxed);
    crate::hud::add(job);
}

// ---- helper process ----

/// Handles `--apply-update <msi> <pid> <exe>`. Returns None for any other command line.
pub fn apply_from_cli(args: &[String]) -> Option<i32> {
    if args.first().map(String::as_str) != Some(APPLY_FLAG) || args.len() < 4 { return None; }
    let (msi, pid, fallback) = (PathBuf::from(&args[1]), args[2].parse().unwrap_or(0), PathBuf::from(&args[3]));
    Some(apply(&msi, pid, &fallback))
}

/// Waits for the old process to end, ending it if it hangs on the way out.
fn wait_for_exit(pid: u32) {
    if pid == 0 { return; }
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) else { return }; // Already gone.
        if WaitForSingleObject(h, EXIT_WAIT_MS) != WAIT_OBJECT_0 {
            log("the app did not quit in time; ending it");
            let _ = TerminateProcess(h, 1);
            let _ = WaitForSingleObject(h, 5_000);
        }
        let _ = CloseHandle(h);
    }
}

/// Runs msiexec quietly and returns its exit code.
fn msiexec(msi: &Path) -> i32 {
    let log_path = work_dir().join("msiexec.log");
    // 1618: another install is running. It usually ends soon, so wait and try again.
    for attempt in 0..6 {
        let status = Command::new(system32("msiexec.exe"))
            .arg("/i").arg(msi).args(["/qn", "/norestart"]).arg("/l*v").arg(&log_path)
            .creation_flags(NO_WINDOW)
            .status();
        let code = match status { Ok(s) => s.code().unwrap_or(-1), Err(e) => { log(&format!("msiexec did not start: {e}")); -1 } };
        log(&format!("msiexec attempt {} exited with {code}", attempt + 1));
        if code != 1618 { return code; }
        std::thread::sleep(Duration::from_secs(10));
    }
    1618
}

/// Where the MSI put tau.exe, from the value it writes. Falls back to where the old copy ran from.
fn installed_exe(fallback: &Path) -> PathBuf {
    unsafe {
        let mut buf = [0u16; 1024];
        let mut size = (buf.len() * 2) as u32;
        let ok = RegGetValueW(HKEY_CURRENT_USER, w!(r"Software\Tau\Install"), w!("Path"), RRF_RT_REG_SZ, None,
            Some(buf.as_mut_ptr() as *mut _), Some(&mut size)).is_ok();
        if ok {
            let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
            let p = PathBuf::from(String::from_utf16_lossy(&buf[..len])).join("tau.exe");
            if p.exists() { return p; }
        }
    }
    fallback.to_path_buf()
}

fn launch(exe: &Path, updated: bool) -> bool {
    let mut c = Command::new(exe);
    if updated { c.arg(UPDATED_FLAG); }
    if let Some(d) = exe.parent() { c.current_dir(d); }
    match c.spawn() {
        Ok(_) => { log(&format!("started {}", exe.display())); true }
        Err(e) => { log(&format!("could not start {}: {e}", exe.display())); false }
    }
}

fn apply(msi: &Path, pid: u32, fallback: &Path) -> i32 {
    log(&format!("helper running for pid {pid}"));
    wait_for_exit(pid);
    let mut code = msiexec(msi);
    // 0 is success; 3010 and 1641 are success with a restart pending.
    let mut ok = matches!(code, 0 | 3010 | 1641);
    let mut exe = installed_exe(fallback);
    if !exe.exists() {
        // A failed install can leave the old product removed and the new one missing. One more go usually fixes it.
        log("no exe after install; trying again");
        code = msiexec(msi);
        ok = matches!(code, 0 | 3010 | 1641);
        exe = installed_exe(fallback);
    }
    if exe.exists() && launch(&exe, ok) { return if ok { 0 } else { code }; }
    let text = crate::gfx::wz(&format!(
        "Tau could not finish updating (installer code {code}).\n\nRun the installer again from the Releases page, \
         or start Tau from the Start menu.\n\nDetails are in {}",
        work_dir().display()
    ));
    unsafe { MessageBoxW(None, PCWSTR(text.as_ptr()), w!("Tau"), MB_ICONERROR | MB_SETFOREGROUND); }
    1
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
            {"name":"x","browser_download_url":"https://e/Tau-0.1.5.msi.sha256"},
            {"name":"y","browser_download_url":"https://e/Tau-0.1.5.msi"}]}"#;
        let r = parse_release(d).unwrap();
        assert_eq!(r.tag, "v0.1.5");
        assert!(r.msi_url.ends_with(".msi"));
        assert!(r.sha_url.unwrap().ends_with(".sha256"));
    }
    #[test]
    fn release_without_installer_is_ignored() {
        assert_eq!(parse_release(r#"{"tag_name":"v1","assets":[]}"#), None);
    }
    #[test]
    fn apply_flag_needs_all_arguments() {
        assert_eq!(apply_from_cli(&["--apply-update".into(), "x.msi".into()]), None);
        assert_eq!(apply_from_cli(&["--version".into()]), None);
    }
}
