//! Reads the current selection from the foreground Explorer window or the desktop, for the keyboard flow.
use std::path::PathBuf;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Variant::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

fn selection_of(disp: &IDispatch) -> Option<(HWND, Vec<PathBuf>, bool)> {
    unsafe {
        let sp: IServiceProvider = disp.cast().ok()?;
        let browser: IShellBrowser = sp.QueryService(&SID_STopLevelBrowser).ok()?;
        let frame = browser.GetWindow().ok()?;
        let view = browser.QueryActiveShellView().ok()?;
        let visible = view.GetWindow().map(|h| IsWindowVisible(h).as_bool()).unwrap_or(true);
        let fv: IFolderView2 = view.cast().ok()?;
        let arr: IShellItemArray = match fv.GetSelection(false) { Ok(a) => a, Err(_) => return Some((frame, vec![], visible)) };
        let mut out = vec![];
        for i in 0..arr.GetCount().ok()? {
            let item = arr.GetItemAt(i).ok()?;
            if let Ok(s) = item.GetDisplayName(SIGDN_FILESYSPATH) {
                if let Ok(p) = s.to_string() { out.push(PathBuf::from(p)); }
                CoTaskMemFree(Some(s.0 as *const _));
            }
        }
        Some((frame, out, visible))
    }
}

pub fn selection() -> Vec<PathBuf> {
    unsafe {
        let fg = GetForegroundWindow();
        let mut class = [0u16; 64];
        let n = GetClassNameW(fg, &mut class) as usize;
        let class = String::from_utf16_lossy(&class[..n]);
        let Ok(shell): Result<IShellWindows> = CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER | CLSCTX_INPROC_SERVER) else { return vec![] };
        if class == "Progman" || class == "WorkerW" {
            let loc = VARIANT::from(0i32); // CSIDL_DESKTOP
            let root = VARIANT::default();
            let mut h = 0i32;
            return shell.FindWindowSW(&loc, &root, SWC_DESKTOP, &mut h, SWFO_NEEDDISPATCH).ok()
                .and_then(|d| selection_of(&d)).map(|s| s.1).unwrap_or_default();
        }
        if class != "CabinetWClass" && class != "ExploreWClass" { return vec![]; }
        let count = shell.Count().unwrap_or(0);
        for i in 0..count {
            let Ok(d) = shell.Item(&VARIANT::from(i)) else { continue };
            // On Windows 11 each tab is its own entry with the same frame; only the visible tab counts.
            if let Some((frame, paths, visible)) = selection_of(&d) {
                if frame == fg && visible { return paths; }
            }
        }
        vec![]
    }
}
