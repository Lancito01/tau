//! Picks points or ranges in time over a filmstrip or waveform. Shared by trim, split, snapshot and bleep.
use crate::gfx::*;
use crate::image::Bmp;
use std::cell::RefCell;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    /// Exactly two marks: what to keep.
    Range,
    /// Any number of marks: cut points or snapshots.
    Points,
    /// Marks read as pairs: the ranges to act on.
    Ranges,
}

const ID_OK: usize = 201;
const ID_CANCEL: usize = 202;
const STRIP_H: i32 = 110;
const GRAB_PX: i32 = 8;

struct St { mode: Mode, strip: Option<Bmp>, dur: f64, marks: Vec<f64>, drag: Option<usize>, hint: String, done: Option<bool> }
thread_local! { static T: RefCell<Option<St>> = const { RefCell::new(None) }; }

fn colorref(c: u32) -> COLORREF { COLORREF(((c >> 16) & 255) | (c & 0xFF00) | ((c & 255) << 16)) }

pub fn fmt_time(t: f64) -> String {
    let t = t.max(0.0);
    let (h, m, s) = ((t / 3600.0) as u32, ((t % 3600.0) / 60.0) as u32, t % 60.0);
    if h > 0 { format!("{h}:{m:02}:{s:05.2}") } else { format!("{m}:{s:05.2}") }
}

/// The mark nearest to x if close enough to grab.
pub fn nearest(marks: &[f64], t: f64, tolerance: f64) -> Option<usize> {
    marks.iter().enumerate()
        .map(|(i, m)| (i, (m - t).abs()))
        .filter(|(_, d)| *d <= tolerance)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// Spans of the strip that are shaded as selected, as start and end times.
pub fn spans(mode: Mode, marks: &[f64]) -> Vec<(f64, f64)> {
    let mut m = marks.to_vec();
    m.sort_by(f64::total_cmp);
    match mode {
        Mode::Points => vec![],
        Mode::Range | Mode::Ranges => m.chunks_exact(2).map(|c| (c[0], c[1])).collect(),
    }
}

fn scale(hwnd: HWND) -> i32 { unsafe { GetDpiForWindow(hwnd) as i32 } }
fn strip_rect(hwnd: HWND) -> RECT {
    unsafe {
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let d = scale(hwnd);
        RECT { left: 16 * d / 96, top: 44 * d / 96, right: rc.right - 16 * d / 96, bottom: (44 + STRIP_H) * d / 96 }
    }
}

fn paint(hwnd: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let mem = CreateCompatibleDC(Some(dc));
        let bm = CreateCompatibleBitmap(dc, rc.right.max(1), rc.bottom.max(1));
        let old = SelectObject(mem, bm.into());
        let bg = CreateSolidBrush(colorref(BG));
        FillRect(mem, &rc, bg);
        let _ = DeleteObject(bg.into());
        let r = strip_rect(hwnd);
        let d = scale(hwnd);
        let font = CreateFontW(-(13 * d / 96), 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
        let of = SelectObject(mem, font.into());
        SetBkMode(mem, TRANSPARENT);
        T.with(|t| {
            let t = t.borrow();
            let Some(s) = t.as_ref() else { return };
            SetTextColor(mem, colorref(MUTED));
            let mut hint = wide(&s.hint);
            let mut hr = RECT { left: r.left, top: 8 * d / 96, right: r.right, bottom: r.top };
            DrawTextW(mem, &mut hint, &mut hr, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
            let surf = CreateSolidBrush(colorref(SURFACE));
            FillRect(mem, &r, surf);
            let _ = DeleteObject(surf.into());
            if let Some(b) = &s.strip {
                let info = BITMAPINFO { bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: b.w as i32, biHeight: -(b.h as i32), biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() }, ..Default::default() };
                SetStretchBltMode(mem, HALFTONE);
                StretchDIBits(mem, r.left, r.top, r.right - r.left, r.bottom - r.top, 0, 0, b.w as i32, b.h as i32, Some(b.px.as_ptr() as *const _), &info, DIB_RGB_COLORS, SRCCOPY);
            }
            let w = (r.right - r.left) as f64;
            let xof = |t: f64| r.left + (t / s.dur.max(0.001) * w) as i32;
            // Band above and below the strip marks the selection; the picture stays visible.
            let accent = CreateSolidBrush(colorref(ACCENT));
            for (a, b) in spans(s.mode, &s.marks) {
                let band = 6 * d / 96;
                FillRect(mem, &RECT { left: xof(a), top: r.top - band, right: xof(b), bottom: r.top }, accent);
                FillRect(mem, &RECT { left: xof(a), top: r.bottom, right: xof(b), bottom: r.bottom + band }, accent);
            }
            for m in &s.marks {
                let x = xof(*m);
                FillRect(mem, &RECT { left: x - 1, top: r.top - 8 * d / 96, right: x + 2, bottom: r.bottom + 8 * d / 96 }, accent);
                SetTextColor(mem, colorref(TEXT));
                let mut lab = wide(&fmt_time(*m));
                let mut lr = RECT { left: x - 60 * d / 96, top: r.bottom + 10 * d / 96, right: x + 60 * d / 96, bottom: r.bottom + 30 * d / 96 };
                DrawTextW(mem, &mut lab, &mut lr, DT_CENTER | DT_SINGLELINE | DT_VCENTER);
            }
            let _ = DeleteObject(accent.into());
        });
        SelectObject(mem, of);
        let _ = DeleteObject(font.into());
        let _ = BitBlt(dc, 0, 0, rc.right, rc.bottom, Some(mem), 0, 0, SRCCOPY);
        SelectObject(mem, old);
        let _ = DeleteObject(bm.into());
        let _ = DeleteDC(mem);
        let _ = EndPaint(hwnd, &ps);
    }
}

fn time_at(hwnd: HWND, x: i32) -> f64 {
    let r = strip_rect(hwnd);
    let f = ((x - r.left) as f64 / (r.right - r.left).max(1) as f64).clamp(0.0, 1.0);
    T.with(|t| t.borrow().as_ref().map(|s| s.dur * f).unwrap_or(0.0))
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let x = (lp.0 & 0xFFFF) as i16 as i32;
        match msg {
            WM_PAINT => { paint(hwnd); LRESULT(0) }
            WM_ERASEBKGND => LRESULT(1),
            WM_COMMAND => {
                let v = match wp.0 & 0xFFFF { ID_OK => Some(true), ID_CANCEL => Some(false), _ => None };
                T.with(|t| if let Some(s) = t.borrow_mut().as_mut() { if v.is_some() { s.done = v; } });
                LRESULT(0)
            }
            WM_CLOSE => { T.with(|t| if let Some(s) = t.borrow_mut().as_mut() { s.done = Some(false); }); LRESULT(0) }
            WM_LBUTTONDOWN => {
                let at = time_at(hwnd, x);
                let r = strip_rect(hwnd);
                let per_px = T.with(|t| t.borrow().as_ref().map(|s| s.dur)).unwrap_or(0.0) / (r.right - r.left).max(1) as f64;
                T.with(|t| {
                    let mut b = t.borrow_mut();
                    let s = b.as_mut().unwrap();
                    let hit = nearest(&s.marks, at, per_px * GRAB_PX as f64);
                    s.drag = match (hit, s.mode) {
                        (Some(i), _) => Some(i),
                        (None, Mode::Range) => nearest(&s.marks, at, f64::MAX),
                        (None, _) => { s.marks.push(at); Some(s.marks.len() - 1) }
                    };
                    if let Some(i) = s.drag { s.marks[i] = at; }
                });
                let _ = SetCapture(hwnd);
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let at = time_at(hwnd, x);
                let moved = T.with(|t| { let mut b = t.borrow_mut(); let s = b.as_mut().unwrap(); if let Some(i) = s.drag { s.marks[i] = at; true } else { false } });
                if moved { let _ = InvalidateRect(Some(hwnd), None, false); }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let _ = ReleaseCapture();
                T.with(|t| t.borrow_mut().as_mut().unwrap().drag = None);
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                let at = time_at(hwnd, x);
                let r = strip_rect(hwnd);
                T.with(|t| {
                    let mut b = t.borrow_mut();
                    let s = b.as_mut().unwrap();
                    let per_px = s.dur / (r.right - r.left).max(1) as f64;
                    if s.mode != Mode::Range {
                        if let Some(i) = nearest(&s.marks, at, per_px * GRAB_PX as f64 * 2.0) { s.marks.remove(i); }
                    }
                });
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }
            WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => { T.with(|t| t.borrow_mut().as_mut().unwrap().done = Some(false)); LRESULT(0) }
            WM_KEYDOWN if wp.0 == VK_RETURN.0 as usize => { T.with(|t| t.borrow_mut().as_mut().unwrap().done = Some(true)); LRESULT(0) }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// Opens the picker and blocks while pumping messages. Returns the sorted marks.
pub fn pick(title: &str, strip: Option<Bmp>, dur: f64, mode: Mode, ok: &str) -> Option<Vec<f64>> {
    let hint = match mode {
        Mode::Range => "Drag the two markers around the part to keep.",
        Mode::Points => "Click to add a marker. Drag to move. Right-click to remove.",
        Mode::Ranges => "Click twice to mark a range. Drag to move. Right-click to remove.",
    };
    let marks = if mode == Mode::Range { vec![0.0, dur] } else { vec![] };
    unsafe {
        let inst = GetModuleHandleW(None).ok()?;
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: inst.into(), lpszClassName: w!("OrreryTimeline"), hCursor: LoadCursorW(None, IDC_ARROW).ok()?, ..Default::default() };
        RegisterClassW(&wc);
        let t = wz(title);
        T.with(|c| *c.borrow_mut() = Some(St { mode, strip, dur, marks, drag: None, hint: hint.to_string(), done: None }));
        let wa = work_area_at(cursor_pos());
        let hwnd = CreateWindowExW(WS_EX_TOPMOST, w!("OrreryTimeline"), PCWSTR(t.as_ptr()), WS_CAPTION | WS_SYSMENU, 0, 0, 100, 100, None, None, Some(inst.into()), None).ok()?;
        let d = GetDpiForWindow(hwnd) as i32;
        let px = |v: i32| v * d / 96;
        let (ww, wh) = (px(860), px(280));
        let _ = SetWindowPos(hwnd, None, wa.left + (wa.right - wa.left - ww) / 2, wa.top + (wa.bottom - wa.top - wh) / 2, ww, wh, SWP_NOZORDER);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let font = CreateFontW(-px(14), 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
        let btn = |label: &str, id: usize, x: i32| {
            let l = wz(label);
            if let Ok(b) = CreateWindowExW(WINDOW_EX_STYLE(0), w!("BUTTON"), PCWSTR(l.as_ptr()), WS_CHILD | WS_VISIBLE, x, rc.bottom - px(42), px(96), px(30), Some(hwnd), Some(HMENU(id as *mut _)), Some(inst.into()), None) {
                SendMessageW(b, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            }
        };
        btn(ok, ID_OK, rc.right - px(112));
        btn("Cancel", ID_CANCEL, rc.right - px(216));
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        let mut msg = MSG::default();
        let mut res = None;
        while res.is_none() && GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            res = T.with(|t| t.borrow().as_ref().and_then(|s| s.done));
        }
        let _ = DestroyWindow(hwnd);
        let _ = DeleteObject(font.into());
        let st = T.with(|t| t.borrow_mut().take())?;
        let mut m = st.marks;
        m.sort_by(f64::total_cmp);
        (res == Some(true)).then_some(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn time_format() {
        assert_eq!(fmt_time(5.5), "0:05.50");
        assert_eq!(fmt_time(65.0), "1:05.00");
        assert_eq!(fmt_time(3725.0), "1:02:05.00");
    }
    #[test]
    fn grabs_only_within_tolerance() {
        assert_eq!(nearest(&[1.0, 5.0], 1.1, 0.5), Some(0));
        assert_eq!(nearest(&[1.0, 5.0], 3.0, 0.5), None);
    }
    #[test]
    fn range_pairs_sort_and_ignore_a_lone_mark() {
        assert_eq!(spans(Mode::Ranges, &[5.0, 1.0, 9.0]), vec![(1.0, 5.0)]);
        assert!(spans(Mode::Points, &[1.0, 2.0]).is_empty());
    }
}
