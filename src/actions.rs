//! The five file actions. They work on anything, including folders and unknown types.
use crate::gfx::wz;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::*;
use windows::Win32::UI::Shell::*;

unsafe fn global_from(bytes: &[u8]) -> Option<HGLOBAL> {
    let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).ok()?;
    let p = GlobalLock(h);
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len());
    let _ = GlobalUnlock(h);
    Some(h)
}

fn set_clipboard(fmt: u32, bytes: &[u8]) -> bool {
    unsafe {
        if OpenClipboard(None).is_err() { return false; }
        let _ = EmptyClipboard();
        let ok = global_from(bytes).is_some_and(|h| SetClipboardData(fmt, Some(HANDLE(h.0))).is_ok());
        let _ = CloseClipboard();
        ok
    }
}

/// DROPFILES header plus a double-null path list, as the shell pastes it.
pub fn dropfiles_bytes(paths: &[PathBuf]) -> Vec<u8> {
    let mut v = vec![0u8; 20];
    v[0..4].copy_from_slice(&20u32.to_le_bytes());
    v[16..20].copy_from_slice(&1u32.to_le_bytes()); // fWide
    for p in paths { for c in wz(&p.display().to_string()) { v.extend_from_slice(&c.to_le_bytes()); } }
    v.extend_from_slice(&[0, 0]);
    v
}

pub fn copy_files(paths: &[PathBuf]) -> bool { set_clipboard(CF_HDROP.0 as u32, &dropfiles_bytes(paths)) }

pub fn paths_text(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\r\n")
}

pub fn copy_paths(paths: &[PathBuf]) -> bool {
    let mut b = vec![];
    for c in wz(&paths_text(paths)) { b.extend_from_slice(&c.to_le_bytes()); }
    set_clipboard(CF_UNICODETEXT.0 as u32, &b)
}

fn pick_folder() -> Option<PathBuf> {
    unsafe {
        let d: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let o = d.GetOptions().ok()?;
        d.SetOptions(o | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        d.SetTitle(w!("Move to")).ok()?;
        d.Show(None).ok()?;
        let s = d.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = s.to_string().ok()?;
        CoTaskMemFree(Some(s.0 as *const _));
        Some(PathBuf::from(path))
    }
}

/// Moves through the shell so conflicts, cross-drive moves and undo behave as they do in Explorer.
fn move_files(paths: &[PathBuf], dest: &Path) {
    let mut from: Vec<u16> = vec![];
    for p in paths { from.extend(wz(&p.display().to_string())); }
    from.push(0);
    let mut to = wz(&dest.display().to_string());
    to.push(0);
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_MOVE, pFrom: PCWSTR(from.as_ptr()), pTo: PCWSTR(to.as_ptr()),
        fFlags: (FOF_ALLOWUNDO.0) as u16, ..Default::default()
    };
    unsafe { let _ = SHFileOperationW(&mut op); }
}

pub fn terminal_dir(p: &Path) -> PathBuf {
    if p.is_dir() { p.to_path_buf() } else { p.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(".")) }
}

fn open_terminal(dir: &Path) {
    const NEW_CONSOLE: u32 = 0x10;
    // Windows Terminal when present; the classic console otherwise.
    let wt = Command::new("wt.exe").arg("-d").arg(dir).spawn();
    if wt.is_err() {
        let _ = Command::new("cmd.exe").current_dir(dir).creation_flags(NEW_CONSOLE).spawn();
    }
}

pub fn reveal(p: &Path) {
    let _ = Command::new("explorer.exe").raw_arg(format!("/select,\"{}\"", p.display())).spawn();
}

pub fn run(id: &str, paths: Vec<PathBuf>) {
    match id {
        "act.copy" => { copy_files(&paths); }
        "act.path" => { copy_paths(&paths); }
        "act.move" => { if let Some(d) = pick_folder() { move_files(&paths, &d); } }
        "act.terminal" => { if let Some(p) = paths.first() { open_terminal(&terminal_dir(p)); } }
        "act.shelf" => crate::shelf::add(paths),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dropfiles_layout() {
        let v = dropfiles_bytes(&[PathBuf::from("a"), PathBuf::from("b")]);
        assert_eq!(&v[0..4], &20u32.to_le_bytes());
        assert_eq!(v[16], 1);
        // a NUL b NUL NUL, as UTF-16.
        assert_eq!(&v[20..], &[b'a', 0, 0, 0, b'b', 0, 0, 0, 0, 0]);
    }
    #[test]
    fn path_list_is_one_per_line() {
        assert_eq!(paths_text(&[PathBuf::from("a"), PathBuf::from("b")]), "a\r\nb");
    }
    #[test]
    fn terminal_uses_parent_of_a_file() {
        assert_eq!(terminal_dir(Path::new(r"C:\no\such\file.txt")), PathBuf::from(r"C:\no\such"));
    }
}
