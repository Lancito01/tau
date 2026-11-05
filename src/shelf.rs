//! A small always-on-top holding area. Drop files in, drag them out into any app.
use crate::gfx::*;
use std::cell::RefCell;
use std::path::PathBuf;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::Common::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const ROW: i32 = 30;
const HEAD: i32 = 34;
const WIDTH: i32 = 280;

#[derive(Default)]
struct Shelf { hwnd: Option<HWND>, items: Vec<PathBuf>, pinned: bool }
thread_local! { static S: RefCell<Shelf> = RefCell::new(Shelf::default()); }

fn colorref(c: u32) -> COLORREF { COLORREF(((c >> 16) & 255) | (c & 0xFF00) | ((c & 255) << 16)) }

fn data_object(paths: &[PathBuf]) -> Option<IDataObject> {
    unsafe {
        let mut pidls: Vec<*mut ITEMIDLIST> = vec![];
        for p in paths {
            let mut pidl = std::ptr::null_mut();
            let w = HSTRING::from(p.as_os_str());
            if SHParseDisplayName(&w, None, &mut pidl, 0, None).is_ok() { pidls.push(pidl); }
        }
        let ptrs: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| *p as *const _).collect();
        let arr: Option<IShellItemArray> = SHCreateShellItemArrayFromIDLists(&ptrs).ok();
        let obj = arr.and_then(|a| a.BindToHandler(None, &BHID_DataObject).ok());
        for p in pidls { CoTaskMemFree(Some(p as *const _)); }
        obj
    }
}

fn scale(hwnd: HWND) -> f32 { unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) as f32 / 96.0 } }

fn resize(hwnd: HWND) {
    S.with(|s| {
        let s = s.borrow();
        let sc = scale(hwnd);
        let h = ((HEAD + ROW * s.items.len().max(1) as i32 + 8) as f32 * sc) as i32;
        let w = (WIDTH as f32 * sc) as i32;
        unsafe {
            let wa = work_area_at(cursor_pos());
            if s.pinned {
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), wa.right - w, wa.top + (wa.bottom - wa.top - h) / 2, w, h, SWP_NOACTIVATE);
            } else {
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, w, h, SWP_NOMOVE | SWP_NOACTIVATE);
            }
            let _ = InvalidateRect(Some(hwnd), None, true);
        }
    });
}

fn paint(hwnd: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut ps);
        let sc = scale(hwnd);
        let px = |v: i32| (v as f32 * sc) as i32;
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let bg = CreateSolidBrush(colorref(BG));
        FillRect(dc, &rc, bg);
        let _ = DeleteObject(bg.into());
        SetBkMode(dc, TRANSPARENT);
        let font = CreateFontW(-px(13), 0, 0, 0, 500, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
        let icon = CreateFontW(-px(14), 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe MDL2 Assets"));
        let old = SelectObject(dc, font.into());
        S.with(|s| {
            let s = s.borrow();
            SetTextColor(dc, colorref(TEXT));
            let mut title = wide(&format!("Shelf  ({})", s.items.len()));
            let mut r = RECT { left: px(12), top: 0, right: rc.right - px(70), bottom: px(HEAD) };
            DrawTextW(dc, &mut title, &mut r, DT_SINGLELINE | DT_VCENTER | DT_LEFT);
            SelectObject(dc, icon.into());
            SetTextColor(dc, colorref(if s.pinned { ACCENT } else { MUTED }));
            let mut pin = wide("\u{E718}");
            let mut r = RECT { left: rc.right - px(62), top: 0, right: rc.right - px(34), bottom: px(HEAD) };
            DrawTextW(dc, &mut pin, &mut r, DT_SINGLELINE | DT_VCENTER | DT_CENTER);
            SetTextColor(dc, colorref(MUTED));
            let mut x = wide("\u{E711}");
            let mut r = RECT { left: rc.right - px(32), top: 0, right: rc.right - px(4), bottom: px(HEAD) };
            DrawTextW(dc, &mut x, &mut r, DT_SINGLELINE | DT_VCENTER | DT_CENTER);
            SelectObject(dc, font.into());
            if s.items.is_empty() {
                SetTextColor(dc, colorref(MUTED));
                let mut t = wide("Drop files here to keep them handy");
                let mut r = RECT { left: px(12), top: px(HEAD), right: rc.right - px(8), bottom: px(HEAD + ROW) };
                DrawTextW(dc, &mut t, &mut r, DT_SINGLELINE | DT_VCENTER | DT_LEFT);
            }
            for (i, p) in s.items.iter().enumerate() {
                let y = px(HEAD + ROW * i as i32);
                let pill = CreateSolidBrush(colorref(SURFACE));
                let row = RECT { left: px(8), top: y, right: rc.right - px(8), bottom: y + px(ROW - 3) };
                FillRect(dc, &row, pill);
                let _ = DeleteObject(pill.into());
                SetTextColor(dc, colorref(TEXT));
                let mut t = wide(&p.file_name().unwrap_or(p.as_os_str()).to_string_lossy());
                let mut r = RECT { left: px(14), top: y, right: rc.right - px(14), bottom: y + px(ROW - 3) };
                DrawTextW(dc, &mut t, &mut r, DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_END_ELLIPSIS | DT_NOPREFIX);
            }
        });
        SelectObject(dc, old);
        let _ = DeleteObject(font.into());
        let _ = DeleteObject(icon.into());
        let _ = EndPaint(hwnd, &ps);
    }
}

fn row_at(hwnd: HWND, y: i32) -> Option<usize> {
    let sc = scale(hwnd);
    let y = (y as f32 / sc) as i32;
    if y < HEAD { return None; }
    let i = ((y - HEAD) / ROW) as usize;
    S.with(|s| (i < s.borrow().items.len()).then_some(i))
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let (x, y) = ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32);
        match msg {
            WM_PAINT => { paint(hwnd); LRESULT(0) }
            WM_DROPFILES => {
                let h = HDROP(wp.0 as *mut _);
                let n = DragQueryFileW(h, 0xFFFF_FFFF, None);
                let mut added = vec![];
                for i in 0..n {
                    let mut b = vec![0u16; 32768];
                    let l = DragQueryFileW(h, i, Some(&mut b)) as usize;
                    added.push(PathBuf::from(String::from_utf16_lossy(&b[..l])));
                }
                DragFinish(h);
                add(added);
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let sc = scale(hwnd);
                let (dx, dy) = ((x as f32 / sc) as i32, (y as f32 / sc) as i32);
                if dy < HEAD {
                    if dx >= WIDTH - 32 { let _ = DestroyWindow(hwnd); return LRESULT(0); }
                    if dx >= WIDTH - 62 {
                        S.with(|s| { let mut s = s.borrow_mut(); s.pinned = !s.pinned; });
                        resize(hwnd);
                        return LRESULT(0);
                    }
                    // Header drags the window. A drag from the empty header can be moved anywhere.
                    let _ = ReleaseCapture();
                    SendMessageW(hwnd, WM_NCLBUTTONDOWN, Some(WPARAM(HTCAPTION as usize)), Some(LPARAM(0)));
                    return LRESULT(0);
                }
                if let Some(i) = row_at(hwnd, y) {
                    let mut p = POINT { x, y };
                    let _ = ClientToScreen(hwnd, &mut p);
                    if DragDetect(hwnd, p).as_bool() {
                        let path = S.with(|s| s.borrow().items[i].clone());
                        if let Some(obj) = data_object(std::slice::from_ref(&path)) {
                            let _ = SHDoDragDrop(Some(hwnd), &obj, None, DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK);
                        }
                    }
                }
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                if let Some(i) = row_at(hwnd, y) {
                    S.with(|s| { s.borrow_mut().items.remove(i); });
                    resize(hwnd);
                }
                LRESULT(0)
            }
            WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => { let _ = DestroyWindow(hwnd); LRESULT(0) }
            WM_DESTROY => { S.with(|s| { let mut s = s.borrow_mut(); s.hwnd = None; s.items.clear(); }); LRESULT(0) }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn ensure_window() -> Option<HWND> {
    if let Some(h) = S.with(|s| s.borrow().hwnd) { return Some(h); }
    unsafe {
        let inst = GetModuleHandleW(None).ok()?;
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: inst.into(), lpszClassName: w!("OrreryShelf"), hCursor: LoadCursorW(None, IDC_ARROW).ok()?, ..Default::default() };
        RegisterClassW(&wc);
        let p = cursor_pos();
        let hwnd = CreateWindowExW(WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_ACCEPTFILES, w!("OrreryShelf"), w!("Orrery shelf"), WS_POPUP, p.x + 40, p.y - 60, 280, 100, None, None, Some(inst.into()), None).ok()?;
        let pref = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &pref as *const _ as _, 4);
        S.with(|s| s.borrow_mut().hwnd = Some(hwnd));
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        Some(hwnd)
    }
}

pub fn add(paths: Vec<PathBuf>) {
    let Some(h) = ensure_window() else { return };
    S.with(|s| {
        let mut s = s.borrow_mut();
        for p in paths { if !s.items.contains(&p) { s.items.push(p); } }
    });
    resize(h);
}
