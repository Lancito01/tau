//! A small always-on-top holding area. Drop files in, drag them out into any app.
use crate::catalog::{classify, Cat};
use crate::gfx::*;
use crate::ui::{self, glyph, id, r, Align, Page, Ui, PAD, TITLE_H};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Ole::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::Common::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const WIDTH: f32 = 320.0;
const ROW: f32 = 44.0;
const MAX_ROWS: usize = 8;
const EMPTY_H: f32 = 124.0;

#[derive(Default)]
struct State { hwnd: Option<HWND>, items: Vec<PathBuf>, pinned: bool }
thread_local! { static S: RefCell<State> = RefCell::new(State::default()); }

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

fn file_glyph(p: &Path) -> u16 {
    match classify(p) {
        Cat::Image => glyph::PHOTO,
        Cat::Video => glyph::VIDEO,
        Cat::Audio => glyph::MUSIC,
        Cat::Folder => glyph::FOLDER,
        _ => glyph::FILE,
    }
}

struct ShelfPage { pending: Option<usize>, closed: bool, preview: Option<Vec<PathBuf>> }

impl ShelfPage {
    fn items(&self) -> Vec<PathBuf> {
        if let Some(p) = &self.preview { return p.clone(); }
        S.with(|s| s.borrow().items.clone())
    }
    fn pinned(&self) -> bool { S.with(|s| s.borrow().pinned) }
}

impl Page for ShelfPage {
    fn size(&self) -> (f32, f32) {
        let n = self.items().len();
        let body = if n == 0 { EMPTY_H } else { n.min(MAX_ROWS) as f32 * ROW };
        (WIDTH, TITLE_H + body + 12.0)
    }
    fn done(&self) -> bool { self.closed }
    fn on_close(&mut self) { self.closed = true; }
    fn place(&mut self, w: i32, h: i32) -> Option<POINT> {
        if !self.pinned() || self.preview.is_some() { return None; }
        let wa = work_area_at(cursor_pos());
        Some(POINT { x: wa.right - w, y: wa.top + (wa.bottom - wa.top - h) / 2 })
    }
    fn message(&mut self, _hwnd: HWND, msg: u32, wp: WPARAM, _lp: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_DROPFILES => unsafe {
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
                Some(LRESULT(0))
            },
            WM_DESTROY => {
                S.with(|s| { let mut s = s.borrow_mut(); s.hwnd = None; s.items.clear(); });
                None
            }
            _ => None,
        }
    }
    fn frame(&mut self, ui: &mut Ui) {
        let items = self.items();
        let pinned = self.pinned();
        // Before the title bar, or pressing it would also start moving the window.
        if ui.icon_button("shelf.pin", if pinned { glyph::PINNED } else { glyph::PIN }, r(WIDTH - 80.0, 12.0, 32.0, 32.0), 13.0, pinned) {
            S.with(|s| { let mut s = s.borrow_mut(); s.pinned = !s.pinned; });
        }
        let sub = match items.len() { 0 => String::new(), 1 => "1 file".to_string(), n => format!("{n} files") };
        if ui.titlebar("Shelf", &sub) { self.closed = true; }
        if ui.key(VK_ESCAPE) { self.closed = true; }

        if items.is_empty() {
            let q = r(PAD - 6.0, TITLE_H, WIDTH - (PAD - 6.0) * 2.0, EMPTY_H - 2.0);
            ui.fill(q, 12.0, SURFACE, 1.0);
            ui.stroke(q, 12.0, TEXT, 0.10, 1.0);
            ui.icon(glyph::ADD, r(q.x, q.y + 22.0, q.w, 30.0), 18.0, MUTED, 1.0);
            ui.text("Drop files here to keep them handy", r(q.x, q.y + 60.0, q.w, 20.0), 12.5, 400, MUTED, Align::Center);
            return;
        }

        let view = r(0.0, TITLE_H, WIDTH, items.len().min(MAX_ROWS) as f32 * ROW);
        let off = ui.scroll_area("shelf.rows", view, items.len() as f32 * ROW);
        let real = ui.mouse;
        if !view.has(real) { ui.mouse = (-1000.0, -1000.0); }
        let mut remove: Option<usize> = None;
        for (i, p) in items.iter().enumerate() {
            let q = r(8.0, TITLE_H + i as f32 * ROW - off, WIDTH - 16.0, ROW - 4.0);
            let hovered = q.has(ui.mouse);
            let x_q = r(q.right() - 32.0, q.y + 7.0, 26.0, 26.0);
            if hovered && ui.icon_button(&format!("shelf.x{i}"), glyph::CLOSE, x_q, 9.0, false) { remove = Some(i); }
            let (hover, _, _) = ui.hit(id(&format!("shelf.row{i}")), q);
            if hover && ui.pressed && !x_q.has(ui.mouse) { self.pending = Some(i); }
            if hover && ui.rpressed { remove = Some(i); }
            let t = ui.anim(id(&format!("shelf.hv{i}")), if hovered { 1.0 } else { 0.0 }, 22.0);
            ui.fill(q, 10.0, TEXT, 0.03 + 0.06 * t);
            ui.icon(file_glyph(p), r(q.x + 8.0, q.y, 24.0, q.h), 14.0, MUTED, 1.0);
            let name = p.file_name().unwrap_or(p.as_os_str()).to_string_lossy().to_string();
            ui.text(&name, r(q.x + 40.0, q.y, q.w - 76.0, q.h), 13.0, 400, TEXT, Align::Left);
        }
        ui.mouse = real;
        ui.unclip();
        if let Some(i) = remove {
            S.with(|s| { let mut s = s.borrow_mut(); if i < s.items.len() { s.items.remove(i); } });
        }
    }
    fn after(&mut self) -> bool {
        let Some(i) = self.pending.take() else { return false };
        let Some((hwnd, path)) = S.with(|s| { let s = s.borrow(); Some((s.hwnd?, s.items.get(i)?.clone())) }) else { return true };
        unsafe {
            let at = cursor_pos();
            if DragDetect(hwnd, at).as_bool() {
                let _ = ReleaseCapture();
                if let Some(obj) = data_object(std::slice::from_ref(&path)) {
                    let _ = SHDoDragDrop(Some(hwnd), &obj, None, DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK);
                }
            }
        }
        true
    }
}

pub fn add(paths: Vec<PathBuf>) {
    let open_now = S.with(|s| s.borrow().hwnd);
    let h = match open_now {
        Some(h) => h,
        None => {
            let p = cursor_pos();
            let page = ShelfPage { pending: None, closed: false, preview: None };
            let Some(h) = ui::open(page, "Tau shelf", POINT { x: p.x + 40, y: p.y - 60 }, WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_ACCEPTFILES) else { return };
            S.with(|s| s.borrow_mut().hwnd = Some(h));
            h
        }
    };
    S.with(|s| {
        let mut s = s.borrow_mut();
        for p in paths { if !s.items.contains(&p) { s.items.push(p); } }
    });
    ui::refresh(h);
}

/// The shelf as a PNG with sample files, or empty, for design review.
pub fn render(samples: Vec<PathBuf>, out: &Path) -> std::result::Result<(), String> {
    ui::render_png(&mut ShelfPage { pending: None, closed: false, preview: Some(samples) }, 1.0, Some((150.0, 120.0)), out)
}
