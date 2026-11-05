//! Background jobs, process running, and output naming.
use std::io::{BufRead, BufReader, Read};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub const NO_WINDOW: u32 = 0x0800_0000;

#[derive(Default)]
pub struct Job {
    pub title: String,
    pub total: usize,
    pub done: AtomicUsize,
    pub frac: Mutex<f32>,
    pub cancel: AtomicBool,
    pub finished: AtomicBool,
    pub errors: Mutex<Vec<String>>,
    pub last_output: Mutex<Option<PathBuf>>,
    pub finished_at: Mutex<Option<std::time::Instant>>,
    /// Shown when finished, in place of the saved file name.
    pub note: Mutex<Option<String>>,
    /// What the format being written cannot hold, shown on the card while it works.
    pub detail: Mutex<Option<String>>,
}

impl Job {
    pub fn cancelled(&self) -> bool { self.cancel.load(Ordering::Relaxed) }
    pub fn set_frac(&self, f: f32) { *self.frac.lock().unwrap() = f.clamp(0.0, 1.0); }
    pub fn progress(&self) -> f32 {
        if self.total == 0 { return 0.0; }
        (self.done.load(Ordering::Relaxed) as f32 + *self.frac.lock().unwrap()) / self.total as f32
    }
}

/// Runs `work` on each item in a background thread, one at a time, and registers the job with the HUD.
pub fn spawn<T, F>(title: String, items: Vec<T>, work: F) -> Arc<Job>
where T: Send + 'static, F: Fn(&Job, &T) -> Result<PathBuf, String> + Send + 'static {
    let job = Arc::new(Job { title, total: items.len(), ..Default::default() });
    crate::hud::add(job.clone());
    let j = job.clone();
    std::thread::spawn(move || {
        com_init();
        for it in &items {
            if j.cancelled() { break; }
            j.set_frac(0.0);
            match work(&j, it) {
                Ok(p) => { *j.last_output.lock().unwrap() = Some(p); }
                Err(e) => { if !j.cancelled() { j.errors.lock().unwrap().push(e); } }
            }
            j.done.fetch_add(1, Ordering::Relaxed);
        }
        *j.finished_at.lock().unwrap() = Some(std::time::Instant::now());
        j.finished.store(true, Ordering::Relaxed);
    });
    job
}

pub fn com_init() {
    use windows::Win32::System::Com::*;
    unsafe { let _ = CoInitializeEx(None, COINIT_MULTITHREADED); }
}

/// `<dir>/<stem>.<ext>`, or `<stem> (2).<ext>` and so on. Never an existing file.
pub fn unique(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let dot = if ext.is_empty() { String::new() } else { format!(".{ext}") };
    let first = dir.join(format!("{stem}{dot}"));
    if !first.exists() { return first; }
    (2..).map(|n| dir.join(format!("{stem} ({n}){dot}"))).find(|p| !p.exists()).unwrap()
}

pub fn sibling(input: &Path, suffix: &str, ext: &str) -> PathBuf {
    let dir = input.parent().unwrap_or(Path::new("."));
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    unique(dir, &format!("{stem}{suffix}"), ext)
}

/// A new folder beside the input, for outputs that are several files.
pub fn sibling_dir(input: &Path, suffix: &str) -> Result<PathBuf, String> {
    let d = sibling(input, suffix, "");
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    Ok(d)
}

/// A scratch folder removed on drop.
pub struct TempDir(pub PathBuf);
impl TempDir {
    pub fn new() -> Result<TempDir, String> {
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!("tau-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        Ok(TempDir(d))
    }
    pub fn file(&self, name: &str) -> PathBuf { self.0.join(name) }
}
impl Drop for TempDir { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

/// Builds the real output through a temp name so a failed or cancelled run leaves nothing behind.
pub fn atomically(out: &Path, f: impl FnOnce(&Path) -> Result<(), String>) -> Result<PathBuf, String> {
    let ext = out.extension().and_then(|e| e.to_str()).unwrap_or("tmp");
    let tmp = out.with_extension(format!("tau-part.{ext}"));
    match f(&tmp) {
        Ok(()) => { std::fs::rename(&tmp, out).map_err(|e| e.to_string())?; Ok(out.to_path_buf()) }
        Err(e) => { let _ = std::fs::remove_file(&tmp); Err(e) }
    }
}

/// Runs a program with no console window, polling for cancel. Returns stdout.
pub fn run(job: Option<&Job>, exe: &Path, args: &[String], on_line: Option<&(dyn Fn(&str) + Sync)>) -> Result<String, String> {
    let mut child = Command::new(exe).args(args)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .creation_flags(NO_WINDOW).spawn()
        .map_err(|e| format!("{}: {e}", exe.display()))?;
    let so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    std::thread::scope(|sc| {
        let out = sc.spawn(move || {
            let mut all = String::new();
            for line in BufReader::new(so).lines().map_while(|l| l.ok()) {
                if let Some(cb) = on_line { cb(&line); }
                all.push_str(&line); all.push('\n');
            }
            all
        });
        let err = sc.spawn(move || { let mut s = vec![]; let _ = se.read_to_end(&mut s); String::from_utf8_lossy(&s).to_string() });
        let status = loop {
            if let Some(j) = job { if j.cancelled() { let _ = child.kill(); } }
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(40)),
                Err(e) => return Err(e.to_string()),
            }
        };
        let stdout = out.join().unwrap_or_default();
        let stderr = err.join().unwrap_or_default();
        if job.is_some_and(|j| j.cancelled()) { return Err("Cancelled".into()); }
        if status.success() { Ok(stdout) } else {
            let tail = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("failed").to_string();
            Err(tail)
        }
    })
}

/// Windows PowerShell with the script on stdin, values passed through the environment so paths need no quoting.
pub fn powershell(job: &Job, script: &str, env: &[(&str, String)]) -> Result<String, String> {
    use std::io::Write;
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", "-"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).creation_flags(NO_WINDOW);
    for (k, v) in env { cmd.env(k, v); }
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let script = format!("$ErrorActionPreference='Stop'\n{script}\n");
    child.stdin.take().unwrap().write_all(script.as_bytes()).map_err(|e| e.to_string())?;
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let t1 = std::thread::spawn(move || { let mut s = vec![]; let _ = so.read_to_end(&mut s); String::from_utf8_lossy(&s).to_string() });
    let t2 = std::thread::spawn(move || { let mut s = vec![]; let _ = se.read_to_end(&mut s); String::from_utf8_lossy(&s).to_string() });
    let status = loop {
        if job.cancelled() { let _ = child.kill(); }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(60)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let out = t1.join().unwrap_or_default();
    let err = t2.join().unwrap_or_default();
    if job.cancelled() { return Err("Cancelled".into()); }
    if status.success() { Ok(out) } else {
        Err(err.lines().find(|l| !l.trim().is_empty() && !l.starts_with("#<")).unwrap_or("Office automation failed").trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unique_never_overwrites() {
        let t = TempDir::new().unwrap();
        let a = unique(&t.0, "a", "png");
        assert_eq!(a.file_name().unwrap(), "a.png");
        std::fs::write(&a, b"x").unwrap();
        let b = unique(&t.0, "a", "png");
        assert_eq!(b.file_name().unwrap(), "a (2).png");
    }
    #[test]
    fn atomically_removes_partial_on_failure() {
        let t = TempDir::new().unwrap();
        let out = t.file("o.bin");
        let r = atomically(&out, |tmp| { std::fs::write(tmp, b"x").unwrap(); Err("no".into()) });
        assert!(r.is_err());
        assert_eq!(std::fs::read_dir(&t.0).unwrap().count(), 0);
    }
}
