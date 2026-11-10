//! Tau: drag a file, tap a key, drop it on the dial.
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod actions;
mod canvas;
mod catalog;
mod convert;
mod detect;
mod docs;
mod explorer;
mod fade;
mod form;
mod gfx;
mod gif;
mod heif;
mod hud;
mod image;
mod job;
mod media;
mod metadata;
mod pdf;
mod player;
mod photo;
mod preview;
mod quality;
mod redact;
mod settings;
mod shelf;
mod speed;
mod timeline;
mod tools;
mod ui;
mod ui_samples;
mod update;
mod wheel;

use catalog::{Engines, Kind};
use settings::Settings;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::OnceLock;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::Console::*;
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::OleInitialize;
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const VERSION: &str = match option_env!("TAU_VERSION") { Some(v) => v, None => env!("CARGO_PKG_VERSION") };

const WM_APP_TRAY: u32 = WM_APP + 30;
const WM_APP_QUIT: u32 = WM_APP + 31;
const HOTKEY_ID: i32 = 1;
const COPYDATA_PATHS: usize = 0x4F52;

static MAIN: AtomicIsize = AtomicIsize::new(0);
static WHEEL: AtomicIsize = AtomicIsize::new(0);
/// Which key group opens each wheel, indexed by `Kind` order.
static KEYS: [AtomicU32; 3] = [AtomicU32::new(1), AtomicU32::new(2), AtomicU32::new(3)];
static HELD: AtomicU32 = AtomicU32::new(0);
/// Key groups whose press Windows saw, so their release must reach it too or the key sticks.
static LEAKED: AtomicU32 = AtomicU32::new(0);
/// Key groups whose press was hidden from Windows, so their release must be hidden too.
static EATEN: AtomicU32 = AtomicU32::new(0);
/// Each wheel has its own key. Otherwise one key opens the wheel and scrolling switches.
static SEPARATE_KEYS: AtomicBool = AtomicBool::new(false);

// ---- engines ----

fn on_path(name: &str) -> Option<PathBuf> {
    let beside = std::env::current_exe().ok()?.with_file_name(name);
    if beside.exists() { return Some(beside); }
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file())
}

pub fn ffmpeg() -> Option<PathBuf> {
    static F: OnceLock<Option<PathBuf>> = OnceLock::new();
    F.get_or_init(|| on_path("ffmpeg.exe")).clone()
}

fn has_progid(id: PCWSTR) -> bool {
    unsafe {
        let mut k = HKEY::default();
        let ok = RegOpenKeyExW(HKEY_CLASSES_ROOT, id, None, KEY_READ, &mut k).is_ok();
        if ok { let _ = RegCloseKey(k); }
        ok
    }
}

pub fn engines() -> Engines {
    static E: OnceLock<Engines> = OnceLock::new();
    *E.get_or_init(|| Engines {
        ffmpeg: ffmpeg().is_some(),
        word: has_progid(w!("Word.Application")),
        excel: has_progid(w!("Excel.Application")),
        powerpoint: has_progid(w!("PowerPoint.Application")),
        edge: docs::has_edge(),
        tar: docs::tar_exe().exists(),
    })
}

pub fn dispatch(kind: Kind, id: &str, paths: Vec<PathBuf>) {
    match kind {
        // A slow conversion first asks how much to give up for time, in a small window of its own, so it gets a thread
        // like a tool does. Checking whether it is slow can probe the files, which also belongs off the dial's thread.
        Kind::Convert => {
            let id = id.to_string();
            std::thread::spawn(move || {
                job::com_init();
                let Some(speed) = convert::ask_speed(&id, &paths) else { return };
                convert::run(&id, speed, paths);
            });
        }
        // Each tool gets a thread and message loop of its own. Sharing the dial's would stack their loops, so an older
        // window could not close, whatever was clicked, until every newer one had.
        Kind::Tools => {
            let id = id.to_string();
            std::thread::spawn(move || { job::com_init(); tools::run_ui(&id, paths); });
        }
        Kind::Actions => actions::run(id, paths),
    }
}

pub fn request_quit() {
    let h = MAIN.load(Ordering::Relaxed);
    if h != 0 { unsafe { let _ = PostMessageW(Some(HWND(h as *mut _)), WM_APP_QUIT, WPARAM(0), LPARAM(0)); } }
}

// ---- keyboard ----

/// Groups left and right variants of a key. 0 is unbound.
pub fn key_group(name: &str) -> u32 {
    match name { "Shift" => 1, "Ctrl" => 2, "Alt" => 3, "Win" => 4, "Caps Lock" => 5, "Tab" => 6, _ => 0 }
}

const WIN_GROUP: u32 = 4;

/// The key group that opens each wheel, in `Kind` order.
fn wheel_keys(s: &Settings) -> [u32; 3] {
    if s.separate_keys {
        [key_group(&s.convert_key), key_group(&s.tools_key), key_group(&s.actions_key)]
    } else {
        [WIN_GROUP, 0, 0]
    }
}

fn group_of(vk: u32) -> u32 {
    let k = VIRTUAL_KEY(vk as u16);
    match k {
        VK_LSHIFT | VK_RSHIFT | VK_SHIFT => 1,
        VK_LCONTROL | VK_RCONTROL | VK_CONTROL => 2,
        VK_LMENU | VK_RMENU | VK_MENU => 3,
        VK_LWIN | VK_RWIN => WIN_GROUP,
        VK_CAPITAL => 5,
        VK_TAB => 6,
        _ => 0,
    }
}

fn apply_settings(s: &Settings) {
    for (slot, group) in KEYS.iter().zip(wheel_keys(s)) { slot.store(group, Ordering::Relaxed); }
    SEPARATE_KEYS.store(s.separate_keys, Ordering::Relaxed);
    let h = HWND(MAIN.load(Ordering::Relaxed) as *mut _);
    unsafe {
        let _ = UnregisterHotKey(Some(h), HOTKEY_ID);
        let m = match s.hotkey.as_str() {
            "Ctrl+Alt+Space" => Some(MOD_CONTROL | MOD_ALT),
            "Ctrl+Shift+Space" => Some(MOD_CONTROL | MOD_SHIFT),
            "Win+Alt+Space" => Some(MOD_WIN | MOD_ALT),
            _ => None,
        };
        if let Some(m) = m { let _ = RegisterHotKey(Some(h), HOTKEY_ID, m | MOD_NOREPEAT, VK_SPACE.0 as u32); }
    }
}

fn primary_down() -> bool {
    unsafe {
        let swapped = GetSystemMetrics(SM_SWAPBUTTON) != 0;
        GetAsyncKeyState(if swapped { VK_RBUTTON.0 } else { VK_LBUTTON.0 } as i32) < 0
    }
}

/// Runs on the UI thread with a hard time budget, so it only reads state and posts messages.
unsafe extern "system" fn key_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let kb = *(lp.0 as *const KBDLLHOOKSTRUCT);
        let msg = wp.0 as u32;
        let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
        let wheel_hwnd = HWND(WHEEL.load(Ordering::Relaxed) as *mut _);
        let mode = wheel::mode();
        let vk = VIRTUAL_KEY(kb.vkCode as u16);
        if mode == wheel::Mode::Modal {
            let nav = match vk {
                VK_LEFT | VK_UP => Some(0),
                VK_RIGHT | VK_DOWN => Some(1),
                VK_RETURN => Some(2),
                VK_ESCAPE => Some(3),
                VK_TAB => Some(4),
                _ => None,
            };
            if let Some(n) = nav {
                if down { let _ = PostMessageW(Some(wheel_hwnd), wheel::WM_APP_NAV, WPARAM(n), LPARAM(0)); }
                return LRESULT(1);
            }
        }
        let group = group_of(kb.vkCode);
        if group != 0 {
            let bit = 1u32 << group;
            if up {
                HELD.fetch_and(!bit, Ordering::Relaxed);
                let leaked = LEAKED.fetch_and(!bit, Ordering::Relaxed) & bit != 0;
                let eaten = EATEN.fetch_and(!bit, Ordering::Relaxed) & bit != 0;
                // A release on its own could still open the Start menu.
                if eaten && !leaked { return LRESULT(1); }
            }
            if let Some(i) = KEYS.iter().position(|k| k.load(Ordering::Relaxed) == group) {
                let dragging = primary_down();
                if down && (dragging || mode == wheel::Mode::Modal) {
                    // Auto-repeat must not re-open or flicker the wheel.
                    if HELD.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
                        // A single key only opens the wheel: pressed again, it must not undo the scrolling.
                        if SEPARATE_KEYS.load(Ordering::Relaxed) || mode == wheel::Mode::Hidden {
                            let _ = PostMessageW(Some(wheel_hwnd), wheel::WM_APP_KEY, WPARAM(i), LPARAM(0));
                        }
                    }
                    // Keys with a job of their own must not also fire it mid-drag. A key held since
                    // before the drag is already down as far as Windows knows, so it stays visible.
                    if group >= WIN_GROUP && LEAKED.load(Ordering::Relaxed) & bit == 0 {
                        EATEN.fetch_or(bit, Ordering::Relaxed);
                        return LRESULT(1);
                    }
                }
            }
            if down { LEAKED.fetch_or(bit, Ordering::Relaxed); }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn win_opens_the_wheel_unless_keys_are_separate() {
        let s = Settings::default();
        assert_eq!(wheel_keys(&s), [WIN_GROUP, 0, 0]);
        assert_eq!(wheel_keys(&Settings { separate_keys: true, ..s }), [1, 2, 3]);
    }
}

// ---- tray and main window ----

fn tray(hwnd: HWND, add: bool) {
    unsafe {
        let mut n = NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: 1, ..Default::default() };
        if add {
            n.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
            n.uCallbackMessage = WM_APP_TRAY;
            let inst = GetModuleHandleW(None).unwrap_or_default();
            n.hIcon = LoadIconW(Some(inst.into()), PCWSTR(1 as *const u16)).or_else(|_| LoadIconW(None, IDI_APPLICATION)).unwrap_or_default();
            for (d, s) in n.szTip.iter_mut().zip(format!("Tau {VERSION}").encode_utf16()) { *d = s; }
            let _ = Shell_NotifyIconW(NIM_ADD, &n);
        } else {
            let _ = Shell_NotifyIconW(NIM_DELETE, &n);
        }
    }
}

fn tray_menu(hwnd: HWND) {
    use ui::{glyph, Menu, MenuItem};
    let at = gfx::cursor_pos();
    let mut items = vec![MenuItem::new("Settings", glyph::SETTINGS)];
    let updates = update::REPO.is_some();
    if updates { items.push(MenuItem::new("Check for updates", glyph::SYNC)); }
    items.push(MenuItem::separator());
    items.push(MenuItem::new("Quit Tau", glyph::POWER));
    let quit = items.len() - 1;
    // From the hidden-icons flyout, open below it, or above it if there is no room, never over
    // it: the flyout can stay on top of the menu (seen with GlazeWM running).
    let (at, flip_y) = match overflow_rect(at) {
        Some(f) => (POINT { x: at.x, y: f.bottom }, f.top),
        None => (at, at.y),
    };
    let chosen = ui::show_menu(Menu::new(230.0, items, None, Some(format!("Tau {VERSION}"))), at, flip_y);
    match chosen {
        Some(i) if i == quit => unsafe { let _ = DestroyWindow(hwnd); },
        Some(0) => { if let Some(s) = settings::show(&Settings::load()) { apply_settings(&s); } }
        Some(1) if updates => update::check_now(),
        _ => {}
    }
}

/// The hidden-icons flyout's bounds, if it is open under the cursor.
fn overflow_rect(at: POINT) -> Option<RECT> {
    // Windows 11, then Windows 10.
    for class in [w!("TopLevelWindowForOverflowXamlIsland"), w!("NotifyIconOverflowWindow")] {
        unsafe {
            let Ok(h) = FindWindowW(class, None) else { continue };
            let mut r = RECT::default();
            if IsWindowVisible(h).as_bool() && GetWindowRect(h, &mut r).is_ok()
                && (r.left..r.right).contains(&at.x) && (r.top..r.bottom).contains(&at.y) {
                return Some(r);
            }
        }
    }
    None
}

fn open_wheel_for(paths: Vec<PathBuf>) {
    if !paths.is_empty() { wheel::open_modal(paths, Kind::Convert); }
}

extern "system" fn main_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_APP_TRAY => {
                let ev = (lp.0 & 0xFFFF) as u32;
                if ev == WM_RBUTTONUP || ev == WM_CONTEXTMENU || ev == WM_LBUTTONUP { tray_menu(hwnd); }
                LRESULT(0)
            }
            WM_HOTKEY => { open_wheel_for(explorer::selection()); LRESULT(0) }
            WM_COPYDATA => {
                let cd = &*(lp.0 as *const COPYDATASTRUCT);
                if cd.dwData == COPYDATA_PATHS && !cd.lpData.is_null() {
                    let units = std::slice::from_raw_parts(cd.lpData as *const u16, cd.cbData as usize / 2);
                    let text = String::from_utf16_lossy(units);
                    open_wheel_for(text.split('\n').filter(|s| !s.is_empty()).map(PathBuf::from).collect());
                }
                LRESULT(1)
            }
            WM_APP_QUIT => { let _ = DestroyWindow(hwnd); LRESULT(0) }
            WM_DESTROY => { tray(hwnd, false); PostQuitMessage(0); LRESULT(0) }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn run_app(paths: Vec<PathBuf>) -> Result<()> {
    unsafe {
        OleInitialize(None)?;
        settings::migrate();
        let inst = GetModuleHandleW(None)?;
        let wc = WNDCLASSW { lpfnWndProc: Some(main_proc), hInstance: inst.into(), lpszClassName: w!("TauMain"), ..Default::default() };
        RegisterClassW(&wc);
        // A hidden top-level window, not message-only: other instances find it with FindWindow.
        let hwnd = CreateWindowExW(WS_EX_TOOLWINDOW, w!("TauMain"), w!("Tau"), WS_POPUP, 0, 0, 0, 0, None, None, Some(inst.into()), None)?;
        MAIN.store(hwnd.0 as isize, Ordering::Relaxed);
        hud::init()?;
        let wheel_hwnd = wheel::init()?;
        WHEEL.store(wheel_hwnd.0 as isize, Ordering::Relaxed);
        apply_settings(&Settings::load());
        tray(hwnd, true);
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_hook), Some(inst.into()), 0)?;
        update::start_background();
        open_wheel_for(paths);
        let mut m = MSG::default();
        while GetMessageW(&mut m, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&m);
            DispatchMessageW(&m);
        }
        let _ = UnhookWindowsHookEx(hook);
        Ok(())
    }
}

/// Hands paths to a copy already running. Returns false if this is the first instance.
fn forward_to_running(paths: &[PathBuf]) -> bool {
    unsafe {
        let m = CreateMutexW(None, false, w!("Local\\TauRunning"));
        if GetLastError() != ERROR_ALREADY_EXISTS { std::mem::forget(m); return false; }
        if let Ok(h) = FindWindowW(w!("TauMain"), None) {
            let text: Vec<u16> = paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n").encode_utf16().collect();
            let cd = COPYDATASTRUCT { dwData: COPYDATA_PATHS, cbData: (text.len() * 2) as u32, lpData: text.as_ptr() as *mut _ };
            SendMessageW(h, WM_COPYDATA, None, Some(LPARAM(&cd as *const _ as isize)));
        }
        true
    }
}

// ---- command line ----

fn wait(job: std::sync::Arc<job::Job>) -> i32 {
    while !job.finished.load(Ordering::Relaxed) { std::thread::sleep(std::time::Duration::from_millis(30)); }
    if let Some(p) = job.last_output.lock().unwrap().as_ref() { println!("{}", p.display()); }
    let errs = job.errors.lock().unwrap();
    for e in errs.iter() { eprintln!("error: {e}"); }
    if errs.is_empty() { 0 } else { 1 }
}

/// Office and Edge resolve relative paths against their own folders, so every path handed on must be absolute.
fn absolute(p: &str) -> PathBuf { std::path::absolute(p).unwrap_or_else(|_| PathBuf::from(p)) }

fn kind_of(s: &str) -> Kind { match s { "tools" => Kind::Tools, "actions" => Kind::Actions, _ => Kind::Convert } }

fn cli(a: &[String]) -> Option<i32> {
    let cmd = a.first()?.as_str();
    let rest = &a[1..];
    match cmd {
        "--version" => { println!("{VERSION}"); Some(0) }
        // Run by the installer on a real uninstall, not on an upgrade.
        "--uninstall" => {
            unsafe {
                if let Ok(h) = FindWindowW(w!("TauMain"), None) {
                    let _ = PostMessageW(Some(h), WM_APP_QUIT, WPARAM(0), LPARAM(0));
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                }
            }
            settings::set_startup(false);
            settings::set_sendto(false);
            Some(0)
        }
        // --convert <format>[:best|quality|balanced|fast] <files...>
        "--convert" if rest.len() >= 2 => {
            job::com_init();
            let (target, speed) = match rest[0].split_once(':') {
                Some((t, s)) => (t, quality::Speed::from_id(s).unwrap_or_default()),
                None => (rest[0].as_str(), quality::Speed::Best),
            };
            Some(wait(convert::run(target, speed, rest[1..].iter().map(|f| absolute(f)).collect())))
        }
        // --tool <id> [key=value ...] <files...>   Values may use \n and \t for shape lists.
        "--tool" if rest.len() >= 2 => {
            job::com_init();
            let (mut o, mut files) = (tools::Opts::new(), vec![]);
            for x in &rest[1..] {
                match x.split_once('=') {
                    Some((k, v)) if !PathBuf::from(x).exists() => { o.insert(k.to_string(), v.replace("\\n", "\n").replace("\\t", "\t")); }
                    _ => files.push(absolute(x)),
                }
            }
            Some(wait(tools::exec(&rest[0], files, o)))
        }
        // --open <id> <files...>: opens a tool's window directly, as if chosen from the dial.
        "--open" if rest.len() >= 2 => {
            job::com_init();
            let files: Vec<PathBuf> = rest[1..].iter().map(|f| absolute(f)).collect();
            if hud::init().is_err() { return Some(1); }
            tools::run_ui(&rest[0], files);
            unsafe {
                let mut m = MSG::default();
                while hud::busy() {
                    while PeekMessageW(&mut m, None, 0, 0, PM_REMOVE).as_bool() { let _ = TranslateMessage(&m); DispatchMessageW(&m); }
                    std::thread::sleep(std::time::Duration::from_millis(15));
                }
            }
            Some(0)
        }
        // --render-wheel <convert|tools|actions> <sample file> <hover|-> <scale> <out.png>
        "--render-wheel" if rest.len() >= 5 => {
            job::com_init();
            let hover = rest[2].parse().ok();
            let scale = rest[3].parse().unwrap_or(1.0);
            match wheel::render_png(kind_of(&rest[0]), &[PathBuf::from(&rest[1])], hover, scale, &PathBuf::from(&rest[4])) {
                Ok(()) => Some(0),
                Err(e) => { eprintln!("error: {e}"); Some(1) }
            }
        }
        // --render-ui <name> <out.png>: draws a window with sample data, for design review.
        "--render-ui" if rest.len() >= 2 => {
            job::com_init();
            match ui_samples::render(&rest[0], &PathBuf::from(&rest[1])) {
                Ok(()) => Some(0),
                Err(e) => { eprintln!("error: {e}"); Some(1) }
            }
        }
        "--engines" => { println!("{:?}", engines()); Some(0) }
        _ => None,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // A windows-subsystem exe has no console of its own; borrow the caller's so CLI output is visible.
    unsafe { let _ = AttachConsole(ATTACH_PARENT_PROCESS); }
    if let Some(code) = cli(&args) { std::process::exit(code); }
    let paths: Vec<PathBuf> = args.iter().filter(|a| !a.starts_with("--")).map(|a| absolute(a)).collect();
    if forward_to_running(&paths) { return; }
    if let Err(e) = run_app(paths) {
        let m = gfx::wz(&format!("Tau could not start: {e}"));
        unsafe { MessageBoxW(None, PCWSTR(m.as_ptr()), w!("Tau"), MB_ICONERROR); }
    }
}
