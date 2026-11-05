//! A small modal option form built from plain Win32 controls.
use crate::gfx::wz;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone)]
pub enum Field {
    Text(&'static str, String),
    Choice(&'static str, Vec<String>, usize),
    Check(&'static str, bool),
    Note(String),
}

pub fn choice(label: &'static str, opts: &[&str], default: usize) -> Field {
    Field::Choice(label, opts.iter().map(|s| s.to_string()).collect(), default)
}
pub fn text(label: &'static str, v: &str) -> Field { Field::Text(label, v.to_string()) }

const IDOK_: i32 = 1;
const IDCANCEL_: i32 = 2;
const CBS_DROPDOWNLIST_: u32 = 3;
const BS_AUTOCHECKBOX_: u32 = 3;
const BS_DEFPUSHBUTTON_: u32 = 1;
const CB_ADDSTRING_: u32 = 0x0143;
const CB_SETCURSEL_: u32 = 0x014E;
const CB_GETCURSEL_: u32 = 0x0147;
const BM_GETCHECK_: u32 = 0x00F0;
const BM_SETCHECK_: u32 = 0x00F1;

static mut RESULT: i32 = 0;

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_COMMAND => {
                let id = (wp.0 & 0xFFFF) as i32;
                if id == IDOK_ || id == IDCANCEL_ { RESULT = id; }
                LRESULT(0)
            }
            WM_CLOSE => { RESULT = IDCANCEL_; LRESULT(0) }
            WM_CTLCOLORSTATIC => {
                // Keep labels on the window's own background.
                SetBkMode(HDC(wp.0 as *mut _), TRANSPARENT);
                LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// Shows the form and blocks. Returns one string per field, or None on cancel.
/// Choice gives the chosen option text, Check gives "1" or "0", Note gives "".
pub fn show(title: &str, fields: &[Field], ok_label: &str) -> Option<Vec<String>> {
    unsafe {
        let inst = GetModuleHandleW(None).ok()?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc), hInstance: inst.into(), lpszClassName: w!("OrreryForm"),
            hCursor: LoadCursorW(None, IDC_ARROW).ok()?, hbrBackground: GetSysColorBrush(COLOR_WINDOW), ..Default::default()
        };
        RegisterClassW(&wc);
        let t = wz(title);
        let hwnd = CreateWindowExW(WS_EX_TOPMOST | WS_EX_DLGMODALFRAME, w!("OrreryForm"), PCWSTR(t.as_ptr()),
            WS_CAPTION | WS_SYSMENU, CW_USEDEFAULT, CW_USEDEFAULT, 100, 100, None, None, Some(inst.into()), None).ok()?;
        let dpi = GetDpiForWindow(hwnd) as f32 / 96.0;
        let sc = |v: i32| (v as f32 * dpi) as i32;
        let font = CreateFontW(-sc(14), 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
        let setf = |h: HWND| { SendMessageW(h, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1))); };
        let (lx, cx, cw, rh) = (sc(16), sc(200), sc(250), sc(32));
        let mut y = sc(14);
        let mut controls: Vec<(HWND, &Field)> = vec![];
        let tabstop = WINDOW_STYLE(WS_TABSTOP.0 | WS_CHILD.0 | WS_VISIBLE.0);
        for f in fields {
            let label = |s: &str, y: i32, w: i32, h: i32| {
                let l = wz(s);
                if let Ok(c) = CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), PCWSTR(l.as_ptr()), WS_CHILD | WS_VISIBLE, lx, y + sc(4), w, h, Some(hwnd), None, Some(inst.into()), None) { setf(c); }
            };
            match f {
                Field::Text(l, v) => {
                    label(l, y, cx - lx - sc(6), sc(22));
                    let vs = wz(v);
                    let c = CreateWindowExW(WS_EX_CLIENTEDGE, w!("EDIT"), PCWSTR(vs.as_ptr()), WINDOW_STYLE(tabstop.0 | 0x80), cx, y, cw, sc(26), Some(hwnd), None, Some(inst.into()), None).ok()?;
                    setf(c);
                    controls.push((c, f));
                    y += rh;
                }
                Field::Choice(l, opts, d) => {
                    label(l, y, cx - lx - sc(6), sc(22));
                    let c = CreateWindowExW(WINDOW_EX_STYLE(0), w!("COMBOBOX"), None, WINDOW_STYLE(tabstop.0 | CBS_DROPDOWNLIST_ | WS_VSCROLL.0), cx, y, cw, sc(240), Some(hwnd), None, Some(inst.into()), None).ok()?;
                    setf(c);
                    for o in opts { let w = wz(o); SendMessageW(c, CB_ADDSTRING_, None, Some(LPARAM(w.as_ptr() as isize))); }
                    SendMessageW(c, CB_SETCURSEL_, Some(WPARAM(*d)), None);
                    controls.push((c, f));
                    y += rh;
                }
                Field::Check(l, v) => {
                    let w = wz(l);
                    let c = CreateWindowExW(WINDOW_EX_STYLE(0), w!("BUTTON"), PCWSTR(w.as_ptr()), WINDOW_STYLE(tabstop.0 | BS_AUTOCHECKBOX_), lx, y, cx + cw - lx, sc(26), Some(hwnd), None, Some(inst.into()), None).ok()?;
                    setf(c);
                    SendMessageW(c, BM_SETCHECK_, Some(WPARAM(*v as usize)), None);
                    controls.push((c, f));
                    y += rh;
                }
                Field::Note(s) => {
                    let lines = s.lines().map(|l| 1 + l.len() as i32 / 62).sum::<i32>().max(1);
                    let h = sc(19) * lines + sc(4);
                    let w = wz(s);
                    if let Ok(c) = CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), PCWSTR(w.as_ptr()), WS_CHILD | WS_VISIBLE, lx, y, cx + cw - lx, h, Some(hwnd), None, Some(inst.into()), None) { setf(c); }
                    y += h + sc(6);
                }
            }
        }
        y += sc(6);
        let w = wz(ok_label);
        let bw = sc(96);
        let right = cx + cw;
        let ok = CreateWindowExW(WINDOW_EX_STYLE(0), w!("BUTTON"), PCWSTR(w.as_ptr()), WINDOW_STYLE(tabstop.0 | BS_DEFPUSHBUTTON_), right - bw, y, bw, sc(30), Some(hwnd), Some(HMENU(IDOK_ as *mut _)), Some(inst.into()), None).ok()?;
        setf(ok);
        let cancel = CreateWindowExW(WINDOW_EX_STYLE(0), w!("BUTTON"), w!("Cancel"), tabstop, right - bw * 2 - sc(8), y, bw, sc(30), Some(hwnd), Some(HMENU(IDCANCEL_ as *mut _)), Some(inst.into()), None).ok()?;
        setf(cancel);
        y += sc(30) + sc(14);

        let mut r = RECT { left: 0, top: 0, right: right + lx, bottom: y };
        let _ = AdjustWindowRectEx(&mut r, WS_CAPTION | WS_SYSMENU, false, WS_EX_DLGMODALFRAME);
        let (ww, wh) = (r.right - r.left, r.bottom - r.top);
        let wa = crate::gfx::work_area_at(crate::gfx::cursor_pos());
        let _ = SetWindowPos(hwnd, None, wa.left + (wa.right - wa.left - ww) / 2, wa.top + (wa.bottom - wa.top - wh) / 2, ww, wh, SWP_NOZORDER);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        if let Some((c, _)) = controls.first() { let _ = SetFocus(Some(*c)); }

        RESULT = 0;
        let mut msg = MSG::default();
        while RESULT == 0 && GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            if msg.message == WM_KEYDOWN && msg.wParam.0 == VK_ESCAPE.0 as usize { RESULT = IDCANCEL_; break; }
            if !IsDialogMessageW(hwnd, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let accepted = RESULT == IDOK_;
        let mut out = vec![];
        let mut ci = controls.iter();
        for f in fields {
            match f {
                Field::Note(_) => out.push(String::new()),
                _ => {
                    let (c, _) = ci.next().unwrap();
                    out.push(match f {
                        Field::Text(..) => {
                            let n = GetWindowTextLengthW(*c) as usize;
                            let mut b = vec![0u16; n + 1];
                            let got = GetWindowTextW(*c, &mut b) as usize;
                            String::from_utf16_lossy(&b[..got])
                        }
                        Field::Choice(_, opts, _) => {
                            let i = SendMessageW(*c, CB_GETCURSEL_, None, None).0 as usize;
                            opts.get(i).cloned().unwrap_or_default()
                        }
                        Field::Check(..) => if SendMessageW(*c, BM_GETCHECK_, None, None).0 == 1 { "1".into() } else { "0".into() },
                        Field::Note(_) => unreachable!(),
                    });
                }
            }
        }
        let _ = DestroyWindow(hwnd);
        let _ = DeleteObject(font.into());
        accepted.then_some(out)
    }
}
