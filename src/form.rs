//! The option form every simple tool uses. Callers describe fields; the page lays them out.
use crate::gfx::*;
use crate::ui::{self, r, Align, Page, Style, Ui, FOOT_H, PAD, TITLE_H};
use std::cell::RefCell;
use windows::Win32::UI::Input::KeyboardAndMouse::*;

#[derive(Clone)]
pub enum Field {
    Text(&'static str, String),
    Choice(&'static str, Vec<String>, usize),
    /// A choice that only applies while the toggle at the given field index is on.
    ChoiceIf(usize, &'static str, Vec<String>, usize),
    Check(&'static str, bool),
    Slider(&'static str, i32, i32, i32),
    Color(&'static str, String),
    Note(String),
    Heading(&'static str),
    /// A row of found / not found chips.
    Status(Vec<(String, bool)>),
}

pub fn choice(label: &'static str, opts: &[&str], default: usize) -> Field {
    Field::Choice(label, opts.iter().map(|s| s.to_string()).collect(), default)
}
pub fn text(label: &'static str, v: &str) -> Field { Field::Text(label, v.to_string()) }

thread_local! { static CONTEXT: RefCell<String> = const { RefCell::new(String::new()) }; }
/// What the next windows are working on, shown under their title.
pub fn set_context(s: String) { CONTEXT.with(|c| *c.borrow_mut() = s); }
pub fn context() -> String { CONTEXT.with(|c| c.borrow().clone()) }

const SWATCHES: [&str; 6] = ["#FFFFFF", "#000000", "#F2EFE8", "#1E293B", "#2563EB", "#E11D48"];
const WIDTH: f32 = 480.0;

struct Value { text: String, sel: usize, on: bool, num: f32 }

pub struct Form {
    title: String,
    sub: String,
    fields: Vec<Field>,
    values: Vec<Value>,
    ok: String,
    content: f32,
    max_h: f32,
    started: bool,
    result: Option<bool>,
}

/// Short option lists read best as a segmented control.
fn fits_segmented(opts: &[String]) -> bool {
    opts.len() <= 4 && opts.iter().map(|o| o.chars().count() + 3).sum::<usize>() <= 44
}

fn color_of(s: &str) -> u32 {
    let c = crate::image::parse_color(s);
    ((c[2] as u32) << 16) | ((c[1] as u32) << 8) | c[0] as u32
}

impl Form {
    pub fn new(title: &str, sub: &str, fields: &[Field], ok: &str) -> Form {
        let values = fields.iter().map(|f| {
            let mut v = Value { text: String::new(), sel: 0, on: false, num: 0.0 };
            match f {
                Field::Text(_, s) | Field::Color(_, s) => v.text = s.clone(),
                Field::Choice(_, _, d) | Field::ChoiceIf(_, _, _, d) => v.sel = *d,
                Field::Check(_, b) => v.on = *b,
                Field::Slider(_, _, _, d) => v.num = *d as f32,
                _ => {}
            }
            v
        }).collect();
        let at = cursor_pos();
        let wa = work_area_at(at);
        let max_h = (wa.bottom - wa.top) as f32 / dpi_scale_at(at) - 80.0;
        Form { title: title.into(), sub: sub.into(), fields: fields.to_vec(), values, ok: ok.into(), content: 120.0, max_h, started: false, result: None }
    }
    fn output(&self) -> Vec<String> {
        self.fields.iter().zip(&self.values).map(|(f, v)| match f {
            Field::Text(..) | Field::Color(..) => v.text.clone(),
            Field::Choice(_, o, _) | Field::ChoiceIf(_, _, o, _) => o.get(v.sel).cloned().unwrap_or_default(),
            Field::Check(..) => if v.on { "1".into() } else { "0".into() },
            Field::Slider(..) => format!("{}", v.num as i32),
            _ => String::new(),
        }).collect()
    }
    fn height(&self) -> f32 { (TITLE_H + self.content + FOOT_H).min(self.max_h) }

    /// Lays out and draws the fields from `y`, returning where they end.
    fn body(&mut self, ui: &mut Ui, start: f32) -> f32 {
        let (x, w) = (PAD, WIDTH - PAD * 2.0);
        let mut y = start;
        for i in 0..self.fields.len() {
            let f = self.fields[i].clone();
            let key = format!("f{i}");
            match &f {
                Field::Heading(l) => {
                    if i > 0 { y += 6.0; }
                    ui.text(l, r(x, y, w, 18.0), 12.0, 600, ACCENT, Align::Left);
                    y += 24.0;
                }
                Field::Note(s) => {
                    let h = ui.wrapped_height(s, 12.5, 400, w) + 2.0;
                    ui.wrapped(s, r(x, y, w, h), 12.5, 400, MUTED);
                    y += h + 12.0;
                }
                Field::Status(items) => {
                    let mut cx = x;
                    for (name, ok) in items {
                        let tw = ui.measure(name, 12.0, 500) + 30.0;
                        if cx + tw > x + w { cx = x; y += 34.0; }
                        let q = r(cx, y, tw, 28.0);
                        ui.fill(q, 14.0, if *ok { SUCCESS } else { TEXT }, if *ok { 0.12 } else { 0.05 });
                        ui.dot(q.x + 13.0, q.y + 14.0, 3.0, if *ok { SUCCESS } else { FAINT }, 1.0);
                        ui.text(name, r(q.x + 22.0, q.y, tw - 24.0, 28.0), 12.0, 500, if *ok { TEXT } else { MUTED }, Align::Left);
                        cx += tw + 6.0;
                    }
                    y += 40.0;
                }
                Field::Check(l, _) => {
                    ui.text(l, r(x, y, w - 52.0, 36.0), 13.0, 400, TEXT, Align::Left);
                    ui.toggle(&key, r(x, y, w, 36.0), &mut self.values[i].on);
                    y += 38.0;
                }
                Field::Text(l, _) => {
                    ui.text(l, r(x, y, w, 18.0), 12.5, 500, MUTED, Align::Left);
                    if !self.started { ui.focus = ui::id(&key); self.started = true; }
                    ui.text_input(&key, r(x, y + 22.0, w, 38.0), &mut self.values[i].text, "");
                    y += 72.0;
                }
                Field::Choice(l, o, _) if fits_segmented(o) => {
                    ui.text(l, r(x, y, w, 18.0), 12.5, 500, MUTED, Align::Left);
                    ui.segmented(&key, r(x, y + 22.0, w, 38.0), o, &mut self.values[i].sel);
                    y += 72.0;
                }
                Field::Choice(l, o, _) if o.iter().any(|s| s.chars().count() > 24) => {
                    ui.text(l, r(x, y, w, 18.0), 12.5, 500, MUTED, Align::Left);
                    ui.select(&key, r(x, y + 22.0, w, 38.0), o, &mut self.values[i].sel, true);
                    y += 72.0;
                }
                Field::Choice(l, o, _) => {
                    ui.text(l, r(x, y, w - 220.0, 38.0), 13.0, 400, TEXT, Align::Left);
                    ui.select(&key, r(x + w - 210.0, y, 210.0, 38.0), o, &mut self.values[i].sel, true);
                    y += 44.0;
                }
                Field::ChoiceIf(dep, l, o, _) => {
                    let on = self.values.get(*dep).is_some_and(|v| v.on);
                    ui.text(l, r(x, y, w - 220.0, 38.0), 13.0, 400, if on { TEXT } else { FAINT }, Align::Left);
                    ui.select(&key, r(x + w - 210.0, y, 210.0, 38.0), o, &mut self.values[i].sel, on);
                    y += 44.0;
                }
                Field::Slider(l, min, max, _) => {
                    let n = self.values[i].num as i32;
                    ui.text(l, r(x, y, w - 60.0, 20.0), 13.0, 400, TEXT, Align::Left);
                    ui.text(&format!("{}{n}", if n > 0 && *min < 0 { "+" } else { "" }), r(x + w - 60.0, y, 60.0, 20.0), 12.5, 600, MUTED, Align::Right);
                    let origin = if *min < 0 { 0.0 } else { *min as f32 };
                    ui.slider(&key, r(x - 9.0, y + 20.0, w + 18.0, 28.0), *min as f32, *max as f32, origin, &mut self.values[i].num);
                    y += 54.0;
                }
                Field::Color(l, _) => {
                    ui.text(l, r(x, y, w, 18.0), 12.5, 500, MUTED, Align::Left);
                    let mut sx = x;
                    let picked = color_of(&self.values[i].text);
                    for (si, c) in SWATCHES.iter().enumerate() {
                        let q = r(sx, y + 22.0, 38.0, 38.0);
                        let col = color_of(c);
                        let (hover, _, clicked) = ui.hit(ui::id(&format!("{key}/s{si}")), q);
                        if clicked { self.values[i].text = c.to_string(); }
                        ui.fill(q.inset(4.0), 8.0, col, 1.0);
                        ui.stroke(q.inset(4.0), 8.0, 0xFFFFFF, 0.14, 1.0);
                        if picked == col { ui.stroke(q, 11.0, ACCENT, 1.0, 2.0); } else if hover { ui.stroke(q, 11.0, TEXT, 0.3, 1.5); }
                        sx += 42.0;
                    }
                    ui.text_input(&key, r(sx + 8.0, y + 22.0, x + w - sx - 8.0, 38.0), &mut self.values[i].text, "#RRGGBB");
                    y += 72.0;
                }
            }
        }
        y
    }
}

impl Page for Form {
    fn size(&self) -> (f32, f32) { (WIDTH, self.height()) }
    fn done(&self) -> bool { self.result.is_some() }
    fn on_close(&mut self) { self.result = Some(false); }
    fn frame(&mut self, ui: &mut Ui) {
        if ui.titlebar(&self.title.clone(), &self.sub.clone()) { self.result = Some(false); }
        let h = self.height();
        let view = r(0.0, TITLE_H, WIDTH, h - TITLE_H - FOOT_H);
        let overflow = self.content > view.h + 0.5;
        let real_mouse = ui.mouse;
        let off = if overflow { ui.scroll_area("form.body", view, self.content) } else { 0.0 };
        // Rows scrolled under the title or footer must not catch the pointer.
        if overflow && !view.has(real_mouse) { ui.mouse = (-1000.0, -1000.0); }
        let top = TITLE_H + 6.0 - off;
        let end = self.body(ui, top);
        ui.mouse = real_mouse;
        if overflow { ui.unclip(); }
        self.content = end - top + 8.0;

        let fy = h - FOOT_H;
        ui.divider(fy, 0.0, WIDTH);
        let by = fy + (FOOT_H - 38.0) / 2.0;
        let primary = r(WIDTH - PAD - 120.0, by, 120.0, 38.0);
        let cancel = r(primary.x - 96.0, by, 88.0, 38.0);
        if ui.button("form.cancel", "Cancel", cancel, Style::Ghost, true) { self.result = Some(false); }
        if ui.button("form.ok", &self.ok.clone(), primary, Style::Primary, true) { self.result = Some(true); }
        if ui.key(VK_ESCAPE) { self.result = Some(false); }
        if ui.key(VK_RETURN) { self.result = Some(true); }
    }
}

/// Shows the form and blocks. Returns one string per field, or None on cancel.
/// Choice gives the chosen option text, Check gives "1" or "0", Slider the number, Note and Heading "".
pub fn show(title: &str, fields: &[Field], ok_label: &str) -> Option<Vec<String>> {
    let f = ui::run_modal(Form::new(title, &context(), fields, ok_label), title, None)?;
    (f.result == Some(true)).then(|| f.output())
}

/// The form as a PNG, for design review.
pub fn render(title: &str, sub: &str, fields: &[Field], ok: &str, out: &std::path::Path) -> Result<(), String> {
    ui::render_png(&mut Form::new(title, sub, fields, ok), 1.0, None, out)
}
