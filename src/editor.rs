//! A small video editor: one picture track whose clips carry their own sound, any number of sound tracks, cuts, moves,
//! trims, speed, volume, fades, muting, and redaction and annotation per clip. Everything shown comes from the same
//! ffmpeg graph the export uses (`cut::graph`), so the preview is the result. Sound plays through `sound`.
use crate::canvas;
use crate::catalog::{classify, Cat};
use crate::cut::{self, Clip, Export, Format, Lane, Media, Project};
use crate::form;
use crate::gfx::*;
use crate::image::Bmp;
use crate::job::*;
use crate::timeline::timecode;
use crate::ui::{self, glyph, id, r, Align, Page, Style, Ui, FOOT_H, PAD, R, TITLE_H};
use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const SIDE: f32 = 300.0;
const HEAD_W: f32 = 136.0;
const RULER_H: f32 = 26.0;
const PIC_H: f32 = 64.0;
const VSND_H: f32 = 36.0;
const SND_H: f32 = 48.0;
const ADD_H: f32 = 34.0;
const BAR_H: f32 = 50.0;
/// How close, in pixels, an edge has to be to grab it or snap to it.
const EDGE_PX: f32 = 7.0;
const SNAP_PX: f64 = 9.0;
/// The preview is rendered no larger than this, which is plenty for a viewport and quick to make.
const VIEW_MAX: (u32, u32) = (960, 540);
const PLAY_FPS_MAX: f64 = 30.0;
const THUMBS: usize = 16;
const WAVE_W: u32 = 3000;
const STILL_CACHE: usize = 40;
const SPEEDS: [f64; 9] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0];

const PIC_CLIP: u32 = 0x2B3340;
const SND_CLIP: u32 = 0x23382C;
const WAVE_INK: &str = "0xB7E4C7";

// ---- sources ----

/// What the editor needs to know about a file, read with ffprobe.
fn read_media(p: &Path) -> Result<Media, String> {
    let pr = crate::media::probe(p);
    match classify(p) {
        Cat::Image => {
            let video = pr.video.clone().or_else(|| {
                let b = crate::image::load(p).ok()?;
                Some(crate::quality::VideoInfo { width: b.w, height: b.h, fps: 30.0, pix_fmt: "rgb24".into(), ..Default::default() })
            });
            Ok(Media { path: p.to_path_buf(), dur: 0.0, video, has_audio: false, still: true })
        }
        Cat::Video | Cat::Audio => {
            if pr.duration <= 0.0 { return Err(format!("Could not read the length of {}", name_of(p))); }
            let video = if classify(p) == Cat::Video { pr.video.clone() } else { None };
            Ok(Media { path: p.to_path_buf(), dur: pr.duration, video, has_audio: !pr.audio.is_empty(), still: false })
        }
        _ => Err(format!("{} is not a picture, video or sound file", name_of(p))),
    }
}

/// Several files read side by side, since each is its own ffprobe. In the order given.
fn read_all(paths: &[PathBuf]) -> Vec<Result<Media, String>> {
    std::thread::scope(|sc| {
        let hs: Vec<_> = paths.iter().map(|p| sc.spawn(move || { com_init(); read_media(p) })).collect();
        hs.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("Could not read a file".into()))).collect()
    })
}

fn name_of(p: &Path) -> String { p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default() }
fn stem_of(p: &Path) -> String { p.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default() }

/// Thumbnails across a source and its waveform, fetched off the window's thread.
#[derive(Default)]
struct Assets { thumbs: Vec<Bmp>, wave: Option<Bmp>, pending: bool }

enum Asset { Thumbs(Vec<Bmp>), Wave(Option<Bmp>) }

fn fetch_assets(m: Media, index: usize, slot: Arc<Mutex<Vec<(usize, Asset)>>>) {
    if m.video.is_some() {
        let (m2, slot2) = (m.clone(), slot.clone());
        std::thread::spawn(move || {
            com_init();
            let thumbs: Vec<Bmp> = if m2.still {
                crate::image::load(&m2.path).ok().and_then(|b| small(&b)).into_iter().collect()
            } else {
                std::thread::scope(|sc| {
                    let hs: Vec<_> = (0..THUMBS).map(|i| {
                        let p = &m2.path;
                        let at = m2.dur * (i as f64 + 0.5) / THUMBS as f64;
                        sc.spawn(move || { com_init(); crate::tools::frame_at(p, at).ok().and_then(|b| small(&b)) })
                    }).collect();
                    hs.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
                })
            };
            slot2.lock().unwrap().push((index, Asset::Thumbs(thumbs)));
        });
    }
    if m.has_audio {
        std::thread::spawn(move || {
            com_init();
            slot.lock().unwrap().push((index, Asset::Wave(waveform(&m.path).ok())));
        });
    }
}

/// A thumbnail 90 pixels tall.
fn small(b: &Bmp) -> Option<Bmp> {
    let h = 90u32;
    let w = ((b.w as f64 / b.h.max(1) as f64) * h as f64).round().max(8.0) as u32;
    b.scaled(w, h).ok()
}

fn waveform(p: &Path) -> Result<Bmp, String> {
    let ff = crate::ffmpeg().ok_or("Needs ffmpeg")?;
    let tmp = TempDir::new()?;
    let png = tmp.file("w.png");
    let f = format!("[0:a:0]aformat=channel_layouts=mono,showwavespic=s={WAVE_W}x80:colors={WAVE_INK}:scale=sqrt");
    run(None, &ff, &crate::media::s(&["-y", "-hide_banner", "-v", "error", "-i", &p.display().to_string(), "-filter_complex", &f, "-frames:v", "1", &png.display().to_string()]), None)?;
    crate::image::load(&png)
}

// ---- rendering the edit ----

/// Annotation pictures for the clips, made when a render first asks for them and kept for the next one.
struct LayerDir { dir: TempDir }

impl LayerDir {
    fn get(&self, p: &Project, id: u64, w: u32, h: u32) -> Option<PathBuf> {
        use std::hash::{Hash, Hasher};
        let c = p.clip(id)?;
        let (_, notes) = canvas::split(&c.shapes);
        if notes.is_empty() { return None; }
        let mut hs = std::collections::hash_map::DefaultHasher::new();
        canvas::encode_all(&notes).hash(&mut hs);
        let path = self.dir.file(&format!("{:016x}_{w}x{h}.png", hs.finish()));
        if path.exists() { return Some(path); }
        com_init();
        let px = canvas::annotation_layer(w, h, &notes).ok()?;
        // Written aside and moved into place, so two renders never read a half-written picture.
        let part = self.dir.file(&format!("{:016x}_{w}x{h}.{}.png", hs.finish(), std::process::id() as u64 ^ id ^ (Instant::now().elapsed().as_nanos() as u64)));
        crate::image::save(&Bmp { w, h, px }, &part, &Default::default()).ok()?;
        if std::fs::rename(&part, &path).is_err() { let _ = std::fs::remove_file(&part); }
        path.exists().then_some(path)
    }
}

fn ffmpeg_args(g: &cut::Graph, tail: &[&str]) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin"].iter().map(|s| s.to_string()).collect();
    a.extend(g.inputs.iter().cloned());
    a.extend(["-filter_complex".to_string(), g.filter.clone()]);
    a.extend(tail.iter().map(|s| s.to_string()));
    a
}

fn spawn_ff(args: &[String]) -> Result<Child, String> {
    let ff = crate::ffmpeg().ok_or("Needs ffmpeg")?;
    Command::new(ff).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .creation_flags(NO_WINDOW).spawn().map_err(|e| e.to_string())
}

/// The picture at `t`, exactly as it will be saved.
fn render_still(p: &Project, t: f64, (w, h): (u32, u32), fps: f64, layers: &LayerDir) -> Result<Bmp, String> {
    let out = cut::Out { w, h, fps, picture: true, sound: false };
    let g = cut::graph(p, t, t + 1.5 / fps, &out, &|id, lw, lh| layers.get(p, id, lw, lh));
    let mut c = spawn_ff(&ffmpeg_args(&g, &["-map", "[vout]", "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "bgra", "pipe:1"]))?;
    let mut px = vec![];
    c.stdout.take().ok_or("No output")?.read_to_end(&mut px).map_err(|e| e.to_string())?;
    let mut err = String::new();
    if let Some(mut e) = c.stderr.take() { let _ = e.read_to_string(&mut err); }
    let _ = c.wait();
    if px.len() != (w * h * 4) as usize { return Err(if err.trim().is_empty() { "Could not render this moment".into() } else { err.trim().to_string() }); }
    Ok(Bmp { w, h, px })
}

/// The edit playing from a time: one ffmpeg for the picture, one for the sound, and the sound's clock for both.
struct Playing {
    frames: Arc<Mutex<VecDeque<Bmp>>>,
    ended: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    child: Option<Child>,
    sound: Option<crate::sound::Out>,
    from: f64,
    end: f64,
    rate: f64,
    taken: u64,
    wall: Option<Instant>,
}

impl Playing {
    fn start(p: &Project, from: f64, size: (u32, u32), rate: f64, layers: &LayerDir) -> Playing {
        let end = p.duration();
        let frames = Arc::new(Mutex::new(VecDeque::new()));
        let (ended, stop) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let mut child = None;
        if p.has_picture() {
            let g = cut::graph(p, from, end, &cut::Out { w: size.0, h: size.1, fps: rate, picture: true, sound: false }, &|id, w, h| layers.get(p, id, w, h));
            if let Ok(mut c) = spawn_ff(&ffmpeg_args(&g, &["-map", "[vout]", "-f", "rawvideo", "-pix_fmt", "bgra", "pipe:1"])) {
                let out = c.stdout.take();
                drop(c.stderr.take());
                let (f, e, s) = (frames.clone(), ended.clone(), stop.clone());
                let len = (size.0 * size.1 * 4) as usize;
                std::thread::spawn(move || {
                    if let Some(mut out) = out {
                        loop {
                            let mut px = vec![0u8; len];
                            if out.read_exact(&mut px).is_err() { break; }
                            while f.lock().unwrap().len() >= 8 && !s.load(Ordering::Relaxed) { std::thread::sleep(std::time::Duration::from_millis(4)); }
                            if s.load(Ordering::Relaxed) { break; }
                            f.lock().unwrap().push_back(Bmp { w: size.0, h: size.1, px });
                        }
                    }
                    e.store(true, Ordering::SeqCst);
                });
                child = Some(c);
            } else { ended.store(true, Ordering::SeqCst); }
        } else { ended.store(true, Ordering::SeqCst); }
        let audible = p.lanes().iter().filter_map(|l| p.track(*l)).any(|t| !t.muted && t.clips.iter().any(|c| !c.muted && c.volume > 0.0 && p.media[c.media].has_audio));
        let sound = audible.then(|| {
            let g = cut::graph(p, from, end, &cut::Out { w: 16, h: 16, fps: rate, picture: false, sound: true }, &|_, _, _| None);
            let mut a: Vec<String> = g.inputs.clone();
            a.extend(["-filter_complex".to_string(), g.filter.clone(), "-map".to_string(), "[aout]".to_string()]);
            a.extend(crate::sound::pcm_args());
            crate::sound::Out::start(a)
        }).flatten();
        Playing { frames, ended, stop, child, sound, from, end, rate, taken: 0, wall: None }
    }

    /// Where playing has got to. The sound sets the time; without sound, the clock starts with the first picture.
    fn now(&self) -> f64 {
        if let Some(s) = &self.sound {
            if let Some(pos) = s.position() { return (self.from + pos).min(self.end); }
            if !s.ended() { return self.from; }
        }
        self.wall.map(|w| (self.from + w.elapsed().as_secs_f64()).min(self.end)).unwrap_or(self.from)
    }

    /// The newest picture that is due, if there is a new one.
    fn tick(&mut self) -> Option<Bmp> {
        let mut q = self.frames.lock().unwrap();
        if self.wall.is_none() && !q.is_empty() { self.wall = Some(Instant::now()); }
        let due = ((self.now() - self.from) * self.rate).floor() as u64;
        let mut last = None;
        while self.taken <= due {
            let Some(f) = q.pop_front() else { break };
            self.taken += 1;
            last = Some(f);
        }
        last
    }

    fn done(&self) -> bool {
        let pictures_done = self.ended.load(Ordering::SeqCst) && self.frames.lock().unwrap().is_empty();
        let sound_done = self.sound.as_ref().is_none_or(|s| s.ended());
        (pictures_done && sound_done) || self.now() >= self.end - 1e-3
    }
}

impl Drop for Playing {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(mut c) = self.child.take() { let _ = c.kill(); std::thread::spawn(move || { let _ = c.wait(); }); }
    }
}

// ---- the window ----

#[derive(Clone, Copy, PartialEq, Debug)]
enum Row { Picture, VideoSound, Sound(usize), Add }

enum Drag {
    Scrub,
    /// A press on a clip. It becomes a move once the pointer has gone a few pixels.
    Move { id: u64, grab: f64, press: (f32, f32), moving: bool, at: f64, lane: Lane },
    TrimStart { id: u64, base: Project },
    TrimEnd { id: u64, base: Project },
}

enum Want {
    Export,
    AddFiles,
    Markup(u64),
    Close,
    /// Files dropped at a point in the window, in its own units.
    Drop(Vec<PathBuf>, (f32, f32)),
    /// Dropped files once the point is known as a sound track, if it was one, and a time.
    Place(Vec<PathBuf>, Option<Lane>, Option<f64>),
}

struct Editor {
    p: Project,
    /// Counts every change, so a picture rendered before one is never mistaken for the current state.
    rev: u64,
    undo: Vec<Project>,
    redo: Vec<Project>,
    edited: bool,
    sel: Option<u64>,
    lane: Lane,
    playhead: f64,
    pps: f64,
    scroll_t: f64,
    vscroll: f32,
    fitted: bool,
    drag: Option<Drag>,
    slider_open: bool,
    /// How tall the inspector's content was last time, for its scrolling.
    insp_h: f32,
    assets: HashMap<usize, Assets>,
    asset_slot: Arc<Mutex<Vec<(usize, Asset)>>>,
    frame: Option<Bmp>,
    frame_gen: u64,
    shown: Option<(u64, i64)>,
    still_busy: bool,
    still_slot: Arc<Mutex<Option<((u64, i64), Result<Bmp, String>)>>>,
    still_cache: VecDeque<((u64, i64), Bmp)>,
    error: Option<String>,
    playing: Option<Playing>,
    layers: Arc<LayerDir>,
    want: Option<Want>,
    toast: Option<(String, Instant)>,
    hwnd: Option<HWND>,
    maximized: bool,
    done: bool,
    start_size: (f32, f32),
    export: Option<Export>,
    /// Drawn for design review: nothing is fetched or played.
    offline: bool,
}

impl Editor {
    fn new(p: Project) -> Editor {
        let at = cursor_pos();
        let (wa, s) = (work_area_at(at), dpi_scale_at(at));
        let start_size = (((wa.right - wa.left) as f32 / s * 0.85).max(1060.0), ((wa.bottom - wa.top) as f32 / s * 0.85).max(680.0));
        let mut e = Editor {
            p, rev: 1, undo: vec![], redo: vec![], edited: false, sel: None, lane: Lane::Video, playhead: 0.0, pps: 40.0, scroll_t: 0.0, vscroll: 0.0,
            fitted: false, drag: None, slider_open: false, insp_h: 0.0, assets: HashMap::new(), asset_slot: Arc::new(Mutex::new(vec![])), frame: None, frame_gen: 0,
            shown: None, still_busy: false, still_slot: Arc::new(Mutex::new(None)), still_cache: VecDeque::new(), error: None, playing: None,
            layers: Arc::new(LayerDir { dir: TempDir::new().unwrap_or(TempDir(std::env::temp_dir())) }), want: None, toast: None, hwnd: None,
            maximized: false, done: false, start_size, export: None, offline: false,
        };
        e.sel = e.p.video.clips.first().or_else(|| e.p.audio.first().and_then(|t| t.clips.first())).map(|c| c.id);
        e
    }

    fn fps(&self) -> f64 { cut::natural_frame(&self.p).2 }
    fn view_size(&self) -> (u32, u32) { let (w, h, _) = cut::natural_frame(&self.p); cut::fit_even(w, h, VIEW_MAX.0, VIEW_MAX.1) }
    fn duration(&self) -> f64 { self.p.duration() }

    /// Before every change: one undo step, and anything playing stops, since it is playing the old edit.
    fn checkpoint(&mut self) {
        self.undo.push(self.p.clone());
        if self.undo.len() > 200 { self.undo.remove(0); }
        self.redo.clear();
        self.changed();
    }
    fn changed(&mut self) { self.rev += 1; self.edited = true; self.playing = None; }
    fn step(&mut self, back: bool) {
        let (from, to) = if back { (&mut self.undo, &mut self.redo) } else { (&mut self.redo, &mut self.undo) };
        if let Some(p) = from.pop() { to.push(std::mem::replace(&mut self.p, p)); self.changed(); }
        if self.sel.is_some_and(|s| self.p.clip(s).is_none()) { self.sel = None; }
    }

    fn toast(&mut self, s: impl Into<String>) { self.toast = Some((s.into(), Instant::now())); }

    // ---- sources ----

    /// Adds files: pictures and video on the picture track, sound on `lane` when it is a sound track, otherwise the
    /// first one. At `at` when dropped there, otherwise at the end of each track.
    fn add_files(&mut self, paths: Vec<PathBuf>, lane: Option<Lane>, at: Option<f64>) {
        let found = crate::tools::pump_job("Reading files", move |_| Ok(read_all(&paths))).unwrap_or_default();
        let mut problems = vec![];
        let mut added = false;
        let mut at = at;
        for m in found {
            let m = match m { Ok(m) => m, Err(e) => { problems.push(e); continue } };
            if !added { self.checkpoint(); added = true; }
            let fresh = !self.p.media.iter().any(|x| x.path == m.path);
            let i = self.p.add_media(m);
            if fresh { self.load_assets(i); }
            let want = match lane { Some(Lane::Audio(k)) => Some(k), _ => None };
            let l = self.p.lane_for(i, want);
            let id = self.p.insert(i, l, at);
            // Several dropped together follow one another.
            if let Some(c) = self.p.clip(id) { if at.is_some() { at = Some(c.end()); } }
            self.sel = Some(id);
        }
        if !problems.is_empty() { self.toast(problems.join(". ")); }
        if added { self.fitted = false; }
    }

    fn load_assets(&mut self, i: usize) {
        if self.offline { return; }
        self.assets.insert(i, Assets { pending: true, ..Default::default() });
        fetch_assets(self.p.media[i].clone(), i, self.asset_slot.clone());
    }

    fn take_assets(&mut self, ui: &mut Ui) {
        let got: Vec<(usize, Asset)> = std::mem::take(&mut *self.asset_slot.lock().unwrap());
        for (i, a) in got {
            let e = self.assets.entry(i).or_default();
            match a {
                Asset::Thumbs(t) => { e.thumbs = t; for k in 0..THUMBS { ui.forget_image(id(&format!("th{i}_{k}"))); } }
                Asset::Wave(w) => { e.wave = w; ui.forget_image(id(&format!("wv{i}"))); }
            }
            e.pending = false;
        }
        if self.assets.values().any(|a| a.pending && (a.thumbs.is_empty() || a.wave.is_none())) && !self.offline {
            // Waiting on something: look again shortly.
            if !self.asset_slot.lock().unwrap().is_empty() { ui.want_anim = true; }
        }
    }

    // ---- preview ----

    fn show(&mut self, ui: &mut Ui, b: Bmp) {
        ui.forget_image(id(&format!("ed.view{}", self.frame_gen)));
        self.frame_gen += 1;
        self.frame = Some(b);
    }

    /// Keeps the viewport on the picture at the playhead: from playing, from what is in hand, or rendered anew.
    fn update_view(&mut self, ui: &mut Ui) {
        if self.offline { return; }
        if let Some(pl) = self.playing.as_mut() {
            let fresh = pl.tick();
            self.playhead = pl.now();
            let done = pl.done();
            if let Some(b) = fresh { self.show(ui, b); }
            ui.want_anim = true;
            if done { self.playing = None; self.shown = None; }
            self.follow_playhead(ui.w);
            return;
        }
        let arrived = self.still_slot.lock().unwrap().take();
        if let Some((key, res)) = arrived {
            self.still_busy = false;
            match res {
                Ok(b) => {
                    if self.still_cache.len() >= STILL_CACHE { self.still_cache.pop_front(); }
                    self.still_cache.push_back((key, b.clone()));
                    if self.shown != Some(key) { self.shown = Some(key); self.show(ui, b); }
                    self.error = None;
                }
                Err(e) => { self.shown = Some(key); self.error = Some(e); }
            }
        }
        if !self.p.has_picture() { return; }
        let fps = self.fps().min(PLAY_FPS_MAX * 2.0);
        let last = ((self.duration() * fps).ceil() as i64 - 1).max(0);
        let key = (self.rev, ((self.playhead * fps).round() as i64).min(last));
        if self.shown == Some(key) { return; }
        if let Some(b) = self.still_cache.iter().find(|(k, _)| *k == key).map(|(_, b)| b.clone()) {
            self.shown = Some(key);
            self.show(ui, b);
            return;
        }
        ui.want_anim = true;
        if self.still_busy { return; }
        self.still_busy = true;
        let (p, size, layers, slot) = (self.p.clone(), self.view_size(), self.layers.clone(), self.still_slot.clone());
        std::thread::spawn(move || {
            com_init();
            let res = render_still(&p, key.1 as f64 / fps, size, fps, &layers);
            *slot.lock().unwrap() = Some((key, res));
        });
    }

    fn toggle_play(&mut self) {
        if self.playing.take().is_some() { self.shown = None; return; }
        if self.offline || self.p.clip_count() == 0 { return; }
        if self.playhead >= self.duration() - 0.05 { self.playhead = 0.0; }
        let rate = self.fps().min(PLAY_FPS_MAX);
        self.playing = Some(Playing::start(&self.p, self.playhead, self.view_size(), rate, &self.layers));
    }

    /// While playing, the timeline turns its page when the playhead reaches the edge.
    fn follow_playhead(&mut self, w: f32) {
        let span = ((w - HEAD_W - 20.0) as f64 / self.pps).max(1.0);
        if self.playhead > self.scroll_t + span * 0.95 || self.playhead < self.scroll_t { self.scroll_t = (self.playhead - span * 0.1).max(0.0); }
    }

    fn seek(&mut self, t: f64) {
        self.playhead = t.clamp(0.0, self.duration().max(0.0));
        if self.playing.is_some() { self.playing = None; self.toggle_play(); }
    }

    // ---- edits ----

    fn split_here(&mut self) {
        let lanes: Vec<Lane> = match self.sel.and_then(|s| self.p.locate(s)) {
            Some((l, _)) => vec![l],
            None => self.p.lanes(),
        };
        let t = self.playhead;
        let hits = lanes.iter().filter_map(|l| self.p.track(*l)).any(|tr| tr.clips.iter().any(|c| t > c.start + cut::MIN_LEN / 2.0 && t < c.end() - cut::MIN_LEN / 2.0));
        if !hits { self.toast("Nothing to cut at the playhead"); return; }
        self.checkpoint();
        let made = self.p.split(&lanes, t);
        if let Some(n) = made.first() { self.sel = Some(*n); }
    }

    fn delete_selected(&mut self, ripple: bool) {
        let Some(s) = self.sel else { return };
        self.checkpoint();
        self.p.remove(s, ripple);
        self.sel = None;
    }

    fn set_speed(&mut self, id: u64, f: f64) {
        self.checkpoint();
        self.p.set_speed(id, f);
    }

    // ---- layout ----

    fn rows(&self) -> Vec<(Row, f32)> {
        let mut v = vec![(Row::Picture, PIC_H), (Row::VideoSound, VSND_H)];
        v.extend((0..self.p.audio.len()).map(|i| (Row::Sound(i), SND_H)));
        v.push((Row::Add, ADD_H));
        v
    }
    fn rows_h(&self) -> f32 { self.rows().iter().map(|r| r.1 + 2.0).sum() }

    fn x_of(&self, t: f64, lanes: R) -> f32 { lanes.x + ((t - self.scroll_t) * self.pps) as f32 }
    fn t_of(&self, x: f32, lanes: R) -> f64 { self.scroll_t + (x - lanes.x) as f64 / self.pps }

    fn row_lane(row: Row) -> Option<Lane> {
        match row { Row::Picture | Row::VideoSound => Some(Lane::Video), Row::Sound(i) => Some(Lane::Audio(i)), Row::Add => None }
    }

    // ---- drawing ----

    fn viewer(&mut self, ui: &mut Ui, q: R) {
        ui.fill(q, 0.0, 0x0A0B0C, 1.0);
        let (fw, fh) = match &self.frame { Some(b) => (b.w as f32, b.h as f32), None => { let (w, h) = self.view_size(); (w as f32, h as f32) } };
        let inner = q.inset(14.0);
        let s = (inner.w / fw).min(inner.h / fh).max(0.01);
        let v = r(inner.x + (inner.w - fw * s) / 2.0, inner.y + (inner.h - fh * s) / 2.0, fw * s, fh * s);
        if let Some(b) = &self.frame {
            let b = b.clone();
            ui.image(id(&format!("ed.view{}", self.frame_gen)), &b, v);
        } else {
            ui.fill(v, 0.0, 0, 1.0);
        }
        let msg = if self.p.clip_count() == 0 { Some("Drop videos, pictures or sound here, or use Add clips.".to_string()) }
            else if !self.p.has_picture() { Some("Sound only. Add a video or picture to see something here.".to_string()) }
            else if self.frame.is_none() { Some("Loading the picture…".to_string()) } else { None };
        if let Some(m) = msg { ui.text(&m, v, 13.0, 500, MUTED, Align::Center); }
        if let Some(e) = &self.error {
            let e = e.lines().last().unwrap_or("").to_string();
            ui.fill(r(q.x, q.bottom() - 30.0, q.w, 30.0), 0.0, 0, 0.6);
            ui.text(&format!("Could not show this moment: {e}"), r(q.x + 12.0, q.bottom() - 30.0, q.w - 24.0, 30.0), 11.5, 400, DANGER, Align::Left);
        }
    }

    fn transport(&mut self, ui: &mut Ui, q: R) {
        ui.fill(q, 0.0, SURFACE, 0.6);
        ui.divider(q.y, q.x, q.right());
        let cy = q.y + (q.h - 34.0) / 2.0;
        let mut x = q.x + PAD - 6.0;
        let playing = self.playing.is_some();
        if ui.icon_button("ed.play", if playing { glyph::PAUSE } else { glyph::PLAY }, r(x, cy, 34.0, 34.0), 13.0, false) { self.toggle_play(); }
        x += 42.0;
        let fps = self.fps();
        let tc = format!("{}  /  {}", timecode(self.playhead, fps), timecode(self.duration(), fps));
        ui.text(&tc, r(x, q.y, 190.0, q.h), 12.5, 600, TEXT, Align::Left);
        x += 196.0;
        let has = self.sel.is_some();
        let buttons: [(&str, u16, &str, bool); 4] = [
            ("ed.split", 0xE8C6, "Split", true),
            ("ed.del", 0xE74D, "Delete", has),
            ("ed.gaps", 0xE8E4, "Close gaps", true),
            ("ed.add", glyph::ADD, "Add clips", true),
        ];
        for (k, g, label, on) in buttons {
            let w = ui.measure(label, 13.0, 500) + 44.0;
            if q.right() - x < w + 160.0 { break; }
            if ui.icon_label_button(k, g, label, r(x, cy, w, 34.0), Style::Ghost, on) {
                match k {
                    "ed.split" => self.split_here(),
                    "ed.del" => self.delete_selected(false),
                    "ed.gaps" => {
                        self.checkpoint();
                        let l = self.sel.and_then(|s| self.p.locate(s)).map(|x| x.0);
                        match l { Some(l) => self.p.close_gaps(l), None => for l in self.p.lanes() { self.p.close_gaps(l) } }
                    }
                    _ => self.want = Some(Want::AddFiles),
                }
            }
            x += w + 4.0;
        }
        let rx = q.right() - PAD;
        if ui.icon_button_if("ed.redo", glyph::REDO, r(rx - 30.0, cy + 2.0, 30.0, 30.0), 13.0, !self.redo.is_empty()) { self.step(false); }
        if ui.icon_button_if("ed.undo", glyph::UNDO, r(rx - 62.0, cy + 2.0, 30.0, 30.0), 13.0, !self.undo.is_empty()) { self.step(true); }
        if ui.icon_button("ed.zin", 0xE8A3, r(rx - 104.0, cy + 2.0, 30.0, 30.0), 12.0, false) { self.zoom(1.5, None); }
        if ui.icon_button("ed.zout", 0xE71F, r(rx - 136.0, cy + 2.0, 30.0, 30.0), 12.0, false) { self.zoom(1.0 / 1.5, None); }
    }

    /// Zooms around a time, the playhead by default, keeping it where it is on screen.
    fn zoom(&mut self, by: f64, around: Option<f64>) {
        self.fitted = true;
        let t = around.unwrap_or(self.playhead);
        let x = (t - self.scroll_t) * self.pps;
        self.pps = (self.pps * by).clamp(2.0, 600.0);
        self.scroll_t = (t - x / self.pps).max(0.0);
    }

    fn timeline(&mut self, ui: &mut Ui, q: R) {
        ui.fill(q, 0.0, BG, 1.0);
        let lanes = r(q.x + HEAD_W, q.y + RULER_H, q.w - HEAD_W, q.h - RULER_H);
        // The whole edit fits until the view is zoomed or scrolled by hand.
        if !self.fitted && lanes.w > 50.0 {
            self.pps = ((lanes.w - 40.0) as f64 / self.duration().max(8.0)).clamp(2.0, 600.0);
            self.scroll_t = 0.0;
        }
        // Wheel: across the timeline over the clips, up and down over the track names, zoom with Ctrl.
        if q.has(ui.mouse) && ui.scroll != 0.0 {
            let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) < 0 };
            if ctrl { let t = self.t_of(ui.mouse.0, lanes); self.fitted = true; self.zoom(1.25f64.powf(ui.scroll as f64), Some(t)); }
            else if ui.mouse.0 < lanes.x { self.vscroll = (self.vscroll - ui.scroll * 40.0).clamp(0.0, (self.rows_h() - lanes.h).max(0.0)); }
            else { self.fitted = true; self.scroll_t = (self.scroll_t - ui.scroll as f64 * 90.0 / self.pps).max(0.0); }
            ui.scroll = 0.0;
        }
        self.vscroll = self.vscroll.clamp(0.0, (self.rows_h() - lanes.h).max(0.0));
        self.ruler(ui, r(lanes.x, q.y, lanes.w, RULER_H));
        ui.fill(r(q.x, q.y, HEAD_W, RULER_H), 0.0, SURFACE, 0.6);

        // Rows, clipped below the ruler.
        let mut y = lanes.y - self.vscroll;
        let mut placed: Vec<(Row, R)> = vec![];
        for (row, h) in self.rows() {
            placed.push((row, r(q.x, y, q.w, h)));
            y += h + 2.0;
        }
        let pressed_here = ui.pressed && ui.mouse.0 >= lanes.x && ui.mouse.1 >= q.y && q.has(ui.mouse);
        let (hover_tl, _, _) = ui.hit(id("ed.timeline"), lanes);
        ui.clip(r(q.x, lanes.y, q.w, lanes.h));
        for (row, rq) in placed.clone() {
            if rq.bottom() < lanes.y || rq.y > lanes.bottom() { continue; }
            self.row_head(ui, row, r(q.x, rq.y, HEAD_W, rq.h));
            if row != Row::Add { ui.fill(r(lanes.x, rq.y, lanes.w, rq.h), 0.0, SURFACE, 0.35); }
        }
        ui.clip(lanes);
        for (row, rq) in placed.clone() {
            if row == Row::Add || rq.bottom() < lanes.y || rq.y > lanes.bottom() { continue; }
            self.row_clips(ui, row, r(lanes.x, rq.y, lanes.w, rq.h), lanes);
        }
        if let Some(Drag::Move { id: mid, moving: true, at, lane, .. }) = &self.drag {
            let (mid, at, lane) = (*mid, *at, *lane);
            if let Some(c) = self.p.clip(mid).cloned() {
                for (row, rq) in &placed {
                    if Self::row_lane(*row) == Some(lane) {
                        let x = self.x_of(at, lanes);
                        let g = r(x, rq.y + 2.0, (c.len() * self.pps) as f32, rq.h - 4.0);
                        ui.fill(g, 6.0, ACCENT, 0.18);
                        ui.stroke(g, 6.0, ACCENT, 0.9, 1.5);
                    }
                }
            }
        }
        ui.unclip();
        ui.unclip();
        // Playhead over everything.
        let px = self.x_of(self.playhead, lanes);
        if px >= lanes.x - 1.0 && px <= lanes.right() {
            ui.fill(r(px - 0.75, q.y + 4.0, 1.5, q.h - 4.0), 0.0, ACCENT, 1.0);
            ui.fill(r(px - 5.0, q.y + 4.0, 10.0, 10.0), 2.0, ACCENT, 1.0);
        }
        self.timeline_input(ui, q, lanes, &placed, pressed_here, hover_tl);
    }

    fn ruler(&mut self, ui: &mut Ui, q: R) {
        ui.fill(q, 0.0, SURFACE, 0.6);
        let steps = [0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0];
        let step = steps.iter().copied().find(|s| s * self.pps >= 72.0).unwrap_or(3600.0);
        let mut t = (self.scroll_t / step).floor() * step;
        let end = self.t_of(q.right(), r(q.x, 0.0, 0.0, 0.0));
        while t <= end {
            let x = self.x_of(t, r(q.x, 0.0, 0.0, 0.0));
            if x >= q.x - 1.0 {
                ui.fill(r(x, q.bottom() - 7.0, 1.0, 7.0), 0.0, MUTED, 0.6);
                let label = if step < 1.0 { format!("{}:{:05.2}", (t / 60.0) as u32, t % 60.0) } else { format!("{}:{:02}", (t / 60.0) as u32, (t % 60.0).round() as u32) };
                ui.text(&label, r(x + 4.0, q.y, 80.0, q.h - 4.0), 10.5, 500, MUTED, Align::Left);
            }
            for k in 1..4 {
                let xs = self.x_of(t + step * k as f64 / 4.0, r(q.x, 0.0, 0.0, 0.0));
                if xs >= q.x { ui.fill(r(xs, q.bottom() - 3.0, 1.0, 3.0), 0.0, MUTED, 0.35); }
            }
            t += step;
        }
        ui.divider(q.bottom() - 1.0, q.x, q.right());
    }

    fn row_head(&mut self, ui: &mut Ui, row: Row, q: R) {
        ui.fill(q, 0.0, SURFACE, 0.8);
        ui.fill(r(q.right() - 1.0, q.y, 1.0, q.h), 0.0, BORDER, 0.7);
        let (name, muted) = match row {
            Row::Picture => ("Video".to_string(), None),
            Row::VideoSound => ("Video sound".to_string(), Some(self.p.video.muted)),
            Row::Sound(i) => (format!("Sound {}", i + 1), Some(self.p.audio[i].muted)),
            Row::Add => {
                if ui.icon_label_button("ed.addtrack", glyph::ADD, "Sound track", r(q.x + 8.0, q.y + 3.0, q.w - 14.0, q.h - 6.0), Style::Ghost, true) {
                    self.checkpoint();
                    self.p.audio.push(Default::default());
                }
                return;
            }
        };
        let lit = Self::row_lane(row) == Some(self.lane) && row != Row::VideoSound;
        ui.text(&name, r(q.x + 12.0, q.y, q.w - 50.0, q.h), 12.0, if lit { 600 } else { 500 }, if lit { TEXT } else { MUTED }, Align::Left);
        if let Some(m) = muted {
            let k = format!("ed.mute{row:?}");
            let b = r(q.right() - 38.0, q.y + (q.h - 28.0) / 2.0, 28.0, 28.0);
            if ui.icon_button(&k, if m { 0xE74F } else { 0xE767 }, b, 12.0, m) {
                self.checkpoint();
                match row { Row::VideoSound => self.p.video.muted = !m, Row::Sound(i) => self.p.audio[i].muted = !m, _ => {} }
            }
            if b.has(ui.mouse) { ui.tooltip = Some((if m { "Unmute track".into() } else { "Mute track".into() }, (b.x + 14.0, b.y))); }
        }
        // An empty sound track can go.
        if let Row::Sound(i) = row {
            if self.p.audio[i].clips.is_empty() && q.has(ui.mouse) {
                if ui.icon_button(&format!("ed.rmtrack{i}"), glyph::CLOSE, r(q.right() - 68.0, q.y + (q.h - 26.0) / 2.0, 26.0, 26.0), 9.0, false) {
                    self.checkpoint();
                    self.p.audio.remove(i);
                    if self.lane == Lane::Audio(i) { self.lane = Lane::Video; }
                }
            }
        }
    }

    fn row_clips(&mut self, ui: &mut Ui, row: Row, q: R, lanes: R) {
        let Some(lane) = Self::row_lane(row) else { return };
        let Some(tr) = self.p.track(lane) else { return };
        let track_muted = tr.muted;
        let clips = tr.clips.clone();
        let (vis_a, vis_b) = (self.t_of(lanes.x, lanes), self.t_of(lanes.right(), lanes));
        for c in &clips {
            if c.end() < vis_a || c.start > vis_b { continue; }
            let x0 = self.x_of(c.start, lanes);
            let cq = r(x0, q.y + 2.0, (c.len() * self.pps) as f32, q.h - 4.0);
            let selected = self.sel == Some(c.id);
            let dragging = matches!(&self.drag, Some(Drag::Move { id, moving: true, .. }) if *id == c.id);
            self.draw_clip(ui, row, c, cq, lanes, selected, dragging, track_muted);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_clip(&mut self, ui: &mut Ui, row: Row, c: &Clip, cq: R, lanes: R, selected: bool, dragging: bool, track_muted: bool) {
        let m = self.p.media[c.media].clone();
        let sound_row = row != Row::Picture;
        let base = if sound_row { SND_CLIP } else { PIC_CLIP };
        let alpha = if dragging { 0.45 } else { 1.0 };
        ui.fill(cq, 6.0, base, alpha);
        let vis = r(cq.x.max(lanes.x), cq.y, (cq.right().min(lanes.right()) - cq.x.max(lanes.x)).max(0.0), cq.h);
        ui.clip(r(vis.x, vis.y, vis.w, vis.h));
        let a = self.assets.get(&c.media);
        if !sound_row {
            if let Some(first) = a.and_then(|a| a.thumbs.first()) {
                let th = cq.h - 18.0;
                let tw = (first.w as f32 / first.h.max(1) as f32 * th).max(8.0);
                let n = a.map(|a| a.thumbs.len()).unwrap_or(0).max(1);
                let mut x = cq.x + ((vis.x - cq.x) / tw).floor() * tw;
                while x < vis.right() {
                    let t = self.t_of(x + tw / 2.0, lanes);
                    let k = if m.still { 0 } else { ((c.src_at(t) / m.dur.max(0.001)) * n as f64).floor().clamp(0.0, (n - 1) as f64) as usize };
                    if let Some(b) = a.and_then(|a| a.thumbs.get(k)) {
                        let b = b.clone();
                        ui.image_part(id(&format!("th{}_{k}", c.media)), &b, None, r(x, cq.y + 16.0, tw, th), alpha * 0.92);
                    }
                    x += tw;
                }
            }
        } else if m.has_audio {
            if let Some(w) = a.and_then(|a| a.wave.clone()) {
                let (ta, tb) = (c.start.max(self.t_of(vis.x, lanes)), c.end().min(self.t_of(vis.right(), lanes)));
                if tb > ta {
                    let (sa, sb) = (c.src_at(ta) / m.dur.max(0.001), c.src_at(tb) / m.dur.max(0.001));
                    let src = r((sa * w.w as f64) as f32, 0.0, (((sb - sa) * w.w as f64) as f32).max(1.0), w.h as f32);
                    let top = cq.y + if row == Row::VideoSound { 2.0 } else { 14.0 };
                    let dest = r(self.x_of(ta, lanes), top, ((tb - ta) * self.pps) as f32, cq.bottom() - top - 2.0);
                    let quiet = c.muted || track_muted || c.volume <= 0.0;
                    ui.image_part(id(&format!("wv{}", c.media)), &w, Some(src), dest, alpha * if quiet { 0.25 } else { (0.45 + 0.4 * c.volume.min(1.5)).min(1.0) });
                }
            }
        } else {
            ui.text("No sound", r(vis.x + 8.0, cq.y, vis.w - 10.0, cq.h), 10.5, 400, FAINT, Align::Left);
        }
        // Fades as ramps at the ends.
        let fade = |ui: &mut Ui, x0: f32, x1: f32, up: bool| {
            let (yb, yt) = (cq.bottom() - 2.0, cq.y + 2.0);
            if up { ui.line((x0, yb), (x1, yt), TEXT, 0.7, 1.5); } else { ui.line((x0, yt), (x1, yb), TEXT, 0.7, 1.5); }
        };
        if c.fade_in > 0.0 { fade(ui, cq.x, cq.x + (c.fade_in * self.pps) as f32, true); }
        if c.fade_out > 0.0 { fade(ui, cq.right() - (c.fade_out * self.pps) as f32, cq.right(), false); }
        // Name and badges in a band along the top.
        if row != Row::VideoSound {
            ui.fill(r(cq.x, cq.y, cq.w, 15.0), 0.0, 0, 0.35 * alpha);
            let mut tags = vec![];
            if (c.speed - 1.0).abs() > 1e-6 { tags.push(crate::speed::label(c.speed)); }
            match c.shapes.len() { 0 => {} 1 => tags.push("1 mark".into()), n => tags.push(format!("{n} marks")) }
            if c.muted { tags.push("muted".into()); }
            let label = if tags.is_empty() { stem_of(&m.path) } else { format!("{}  ·  {}", stem_of(&m.path), tags.join("  ·  ")) };
            ui.text(&label, r(vis.x + 6.0, cq.y, (vis.w - 10.0).max(0.0), 15.0), 10.5, 600, TEXT, Align::Left);
        } else if c.muted {
            ui.icon(0xE74F, r(vis.x + 4.0, cq.y, 18.0, cq.h), 10.0, MUTED, 1.0);
        }
        ui.unclip();
        if selected { ui.stroke(cq, 6.0, ACCENT, 1.0, 2.0); } else { ui.stroke(cq, 6.0, TEXT, 0.10, 1.0); }
        let _ = row;
    }

    /// Presses and drags on the timeline: scrub the playhead, select, move, and trim.
    fn timeline_input(&mut self, ui: &mut Ui, q: R, lanes: R, placed: &[(Row, R)], pressed: bool, hover: bool) {
        let ruler = r(lanes.x, q.y, lanes.w, RULER_H);
        let row_at = |y: f32| placed.iter().find(|(row, rq)| *row != Row::Add && y >= rq.y && y < rq.bottom() + 2.0).map(|p| p.0);
        let t_mouse = self.t_of(ui.mouse.0, lanes).max(0.0);
        // What is under the pointer: a clip, and whether one of its edges.
        let under = |me: &Editor| -> Option<(u64, i8)> {
            let row = row_at(ui.mouse.1)?;
            let lane = Self::row_lane(row)?;
            let tr = me.p.track(lane)?;
            tr.clips.iter().rev().find_map(|c| {
                let (x0, x1) = (me.x_of(c.start, lanes), me.x_of(c.end(), lanes));
                if ui.mouse.0 < x0 - 2.0 || ui.mouse.0 > x1 + 2.0 { return None; }
                let edge = if (ui.mouse.0 - x0).abs() <= EDGE_PX && x1 - x0 > EDGE_PX * 3.0 { -1 } else if (ui.mouse.0 - x1).abs() <= EDGE_PX { 1 } else { 0 };
                Some((c.id, edge))
            })
        };
        let hit = if lanes.has(ui.mouse) && !ruler.has(ui.mouse) { under(self) } else { None };
        if hover {
            ui.cursor = match (&self.drag, hit) {
                (Some(Drag::TrimStart { .. } | Drag::TrimEnd { .. }), _) | (None, Some((_, -1 | 1))) => IDC_SIZEWE,
                (Some(Drag::Move { moving: true, .. }), _) => IDC_SIZEALL,
                (None, Some(_)) => IDC_HAND,
                _ => IDC_ARROW,
            };
        }
        if pressed && ui.mouse.1 >= lanes.y {
            if let Some(row) = row_at(ui.mouse.1) { if let Some(l) = Self::row_lane(row) { self.lane = l; } }
        }
        if pressed {
            if ruler.has(ui.mouse) || hit.is_none() {
                if ui.mouse.1 >= q.y { self.drag = Some(Drag::Scrub); if hit.is_none() && !ruler.has(ui.mouse) { self.sel = None; } }
            } else if let Some((cid, edge)) = hit {
                self.sel = Some(cid);
                let c = self.p.clip(cid).cloned().unwrap();
                if ui.double && edge == 0 && self.p.locate(cid).map(|x| x.0) == Some(Lane::Video) { self.want = Some(Want::Markup(cid)); }
                self.drag = Some(match edge {
                    -1 => Drag::TrimStart { id: cid, base: self.p.clone() },
                    1 => Drag::TrimEnd { id: cid, base: self.p.clone() },
                    _ => Drag::Move { id: cid, grab: t_mouse - c.start, press: ui.mouse, moving: false, at: c.start, lane: self.p.locate(cid).unwrap().0 },
                });
            }
        }
        if ui.down {
            let tol = SNAP_PX / self.pps;
            let points = |me: &Editor, except: u64| { let mut v = me.p.snap_points(Some(except)); v.push(me.playhead); v };
            match self.drag.take() {
                Some(Drag::Scrub) => { self.playing = None; self.playhead = t_mouse.min(self.duration()); self.drag = Some(Drag::Scrub); }
                Some(Drag::Move { id: cid, grab, press, mut moving, mut at, mut lane }) => {
                    if !moving && ((ui.mouse.0 - press.0).abs() > 4.0 || (ui.mouse.1 - press.1).abs() > 4.0) { moving = true; }
                    if moving {
                        let len = self.p.clip(cid).map(|c| c.len()).unwrap_or(0.0);
                        let pts = points(self, cid);
                        let raw = (t_mouse - grab).max(0.0);
                        let by_start = cut::snap_to(raw, &pts, tol);
                        let by_end = cut::snap_to(raw + len, &pts, tol) - len;
                        at = if (by_start - raw).abs() > 1e-9 { by_start } else if (by_end - raw).abs() > 1e-9 { by_end.max(0.0) } else { raw };
                        let from = self.p.locate(cid).map(|x| x.0).unwrap_or(Lane::Video);
                        if let Some(l) = row_at(ui.mouse.1).and_then(Self::row_lane) {
                            if (l == Lane::Video) == (from == Lane::Video) { lane = l; }
                        }
                    }
                    self.drag = Some(Drag::Move { id: cid, grab, press, moving, at, lane });
                }
                Some(Drag::TrimStart { id: cid, base }) => {
                    self.p = base.clone();
                    let pts = points(self, cid);
                    self.p.trim_start(cid, cut::snap_to(t_mouse, &pts, tol));
                    self.rev += 1;
                    self.drag = Some(Drag::TrimStart { id: cid, base });
                }
                Some(Drag::TrimEnd { id: cid, base }) => {
                    self.p = base.clone();
                    let limit = self.p.clip(cid).map(|c| self.p.media[c.media].limit()).unwrap_or(f64::MAX);
                    let pts = points(self, cid);
                    self.p.trim_end(cid, cut::snap_to(t_mouse, &pts, tol), limit);
                    self.rev += 1;
                    self.drag = Some(Drag::TrimEnd { id: cid, base });
                }
                None => {}
            }
        }
        if ui.released {
            match self.drag.take() {
                Some(Drag::Move { id: cid, moving: true, at, lane, .. }) => {
                    let same = self.p.clip(cid).is_some_and(|c| (c.start - at).abs() < 1e-9) && self.p.locate(cid).map(|x| x.0) == Some(lane);
                    if !same { self.checkpoint(); self.p.move_clip(cid, lane, at); }
                }
                Some(Drag::TrimStart { base, .. } | Drag::TrimEnd { base, .. }) => {
                    let now = std::mem::replace(&mut self.p, base);
                    let differs = self.p.lanes().iter().any(|l| self.p.track(*l) != now.track(*l));
                    if differs { self.checkpoint(); }
                    self.p = now;
                }
                _ => {}
            }
        }
        // Edges show where they can be pulled.
        if self.drag.is_none() {
            if let Some((cid, edge)) = hit.filter(|h| h.1 != 0) {
                if let Some(c) = self.p.clip(cid) {
                    let t = if edge < 0 { c.start } else { c.end() };
                    let x = self.x_of(t, lanes);
                    if let Some((_, rq)) = placed.iter().find(|(row, rq)| *row != Row::Add && ui.mouse.1 >= rq.y && ui.mouse.1 < rq.bottom()) {
                        ui.fill(r(x - 1.5, rq.y + 2.0, 3.0, rq.h - 4.0), 1.5, ACCENT, 1.0);
                    }
                }
            }
        }
    }

    // ---- inspector ----

    /// The selected clip's settings, scrolling when the window is short.
    fn inspector(&mut self, ui: &mut Ui, q: R) {
        ui.fill(q, 0.0, SURFACE, 0.6);
        ui.fill(r(q.x, q.y, 1.0, q.h), 0.0, BORDER, 0.7);
        if !ui.down { self.slider_open = false; }
        let off = ui.scroll_area("ed.insp", q, self.insp_h);
        let real = ui.mouse;
        if !q.has(real) { ui.mouse = (-1000.0, -1000.0); }
        let end = self.inspector_body(ui, q.x + PAD, q.w - PAD * 2.0, q.y + 16.0 - off);
        self.insp_h = end - (q.y - off) + 16.0;
        ui.mouse = real;
        ui.unclip();
    }

    fn inspector_body(&mut self, ui: &mut Ui, x: f32, w: f32, mut y: f32) -> f32 {
        let Some(c) = self.sel.and_then(|s| self.p.clip(s)).cloned() else {
            ui.text("Edit", r(x, y, w, 24.0), 14.0, 600, TEXT, Align::Left);
            y += 30.0;
            let (fw, fh, fps) = cut::natural_frame(&self.p);
            let info = format!("{} clips  ·  {}\n{}", self.p.clip_count(), crate::timeline::fmt_time(self.duration()),
                if self.p.has_picture() { format!("{fw} × {fh}, {} fps", (fps * 100.0).round() / 100.0) } else { "Sound only".into() });
            ui.wrapped(&info, r(x, y, w, 40.0), 12.0, 400, MUTED);
            y += 52.0;
            if ui.icon_label_button("ed.add2", glyph::ADD, "Add clips", r(x, y, w, 36.0), Style::Secondary, true) { self.want = Some(Want::AddFiles); }
            y += 52.0;
            let tips = "Click a clip to change it. Drag it to move, drag its edges to trim.\n\nSpace  play or pause\nS  split at the playhead\nDelete  remove, Shift+Delete closes the gap\nM  mute the clip\nLeft, Right  one frame, Shift for a second\nCtrl+wheel  zoom\nDouble-click a video  redact and annotate\n\nDrop files on a track to add them there.";
            let th = ui.wrapped_height(tips, 12.0, 400, w);
            ui.wrapped(tips, r(x, y, w, th + 4.0), 12.0, 400, MUTED);
            return y + th;
        };
        let m = self.p.media[c.media].clone();
        let lane = self.p.locate(c.id).map(|l| l.0).unwrap_or(Lane::Video);
        ui.text(&name_of(&m.path), r(x, y, w - 30.0, 22.0), 13.5, 600, TEXT, Align::Left);
        if ui.icon_button("ed.desel", glyph::CLOSE, r(x + w - 26.0, y - 2.0, 26.0, 26.0), 9.0, false) { self.sel = None; return y; }
        y += 22.0;
        let kind = if m.still { "Picture" } else if m.video.is_some() { "Video" } else { "Sound" };
        let src = if m.still { String::new() } else { format!(", from {} to {}", crate::timeline::fmt_time(c.src_in), crate::timeline::fmt_time(c.src_out)) };
        ui.text(&format!("{kind}  ·  {}{src}", crate::timeline::fmt_time(c.len())), r(x, y, w, 18.0), 11.5, 400, MUTED, Align::Left);
        y += 30.0;

        // Speed.
        if !m.still {
            ui.text("Speed", r(x, y, w, 18.0), 12.0, 500, MUTED, Align::Left);
            let names: Vec<String> = SPEEDS.iter().map(|s| crate::speed::label(*s)).collect();
            let cur = SPEEDS.iter().position(|s| (s - c.speed).abs() < 1e-6);
            let mut opts = names.clone();
            if cur.is_none() { opts.push(crate::speed::label(c.speed)); }
            let was = cur.unwrap_or(opts.len() - 1);
            let mut sel = was;
            ui.select("ed.speed", r(x, y + 20.0, w, 34.0), &opts, &mut sel, true);
            if sel != was && sel < SPEEDS.len() { self.set_speed(c.id, SPEEDS[sel]); }
            y += 66.0;
        }

        // Sound.
        if m.has_audio {
            let mut on = !c.muted;
            ui.text("Sound", r(x, y, w - 50.0, 24.0), 12.0, 500, MUTED, Align::Left);
            ui.toggle("ed.clipsound", r(x, y, w, 24.0), &mut on);
            if on == c.muted { self.checkpoint(); if let Some(k) = self.p.clip_mut(c.id) { k.muted = !on; } }
            y += 32.0;
            let was = (c.volume * 100.0).round();
            let v = self.slider(ui, "ed.vol", "Volume", x, w, y, (0.0, 200.0, 100.0), was, |v| format!("{}%", v as i32));
            if v != was { if let Some(k) = self.p.clip_mut(c.id) { k.volume = v / 100.0; } self.rev += 1; }
            y += 50.0;
        }

        // Fades.
        let top = ((c.len() / 2.0).min(5.0) * 10.0).floor() as f32;
        if top >= 1.0 {
            let fmt = |v: f32| if v <= 0.0 { "Off".to_string() } else { format!("{:.1} s", v / 10.0) };
            let was = (c.fade_in * 10.0).round() as f32;
            let v = self.slider(ui, "ed.fin", "Fade in", x, w, y, (0.0, top, 0.0), was.min(top), fmt);
            if v != was.min(top) { if let Some(k) = self.p.clip_mut(c.id) { k.fade_in = v as f64 / 10.0; } self.rev += 1; }
            y += 50.0;
            let was = (c.fade_out * 10.0).round() as f32;
            let v = self.slider(ui, "ed.fout", "Fade out", x, w, y, (0.0, top, 0.0), was.min(top), fmt);
            if v != was.min(top) { if let Some(k) = self.p.clip_mut(c.id) { k.fade_out = v as f64 / 10.0; } self.rev += 1; }
            y += 56.0;
        }

        // Redact and annotate the picture.
        if lane == Lane::Video {
            let (covers, notes) = canvas::split(&c.shapes);
            let label = if c.shapes.is_empty() { "Redact and annotate".to_string() } else { format!("Marks: {} redacted, {} drawn", covers.len(), notes.len()) };
            if ui.icon_label_button("ed.markup", 0xE70F, &label, r(x, y, w, 36.0), Style::Secondary, true) { self.want = Some(Want::Markup(c.id)); }
            y += 46.0;
            if !c.shapes.is_empty() {
                if ui.button("ed.clearmarks", "Remove marks", r(x, y, w, 30.0), Style::Ghost, true) {
                    self.checkpoint();
                    if let Some(k) = self.p.clip_mut(c.id) { k.shapes.clear(); }
                }
                y += 40.0;
            }
        }

        let bw = (w - 8.0) / 2.0;
        if ui.icon_label_button("ed.split2", 0xE8C6, "Split", r(x, y, bw, 34.0), Style::Secondary, c.covers(self.playhead) && self.playhead > c.start + cut::MIN_LEN / 2.0) { self.split_here(); }
        if ui.icon_label_button("ed.del2", 0xE74D, "Delete", r(x + bw + 8.0, y, bw, 34.0), Style::Secondary, true) { self.delete_selected(false); }
        y += 42.0;
        if ui.button("ed.ripple", "Delete and close the gap", r(x, y, w, 30.0), Style::Ghost, true) { self.delete_selected(true); }
        y + 30.0
    }

    /// A slider whose drag is one undo step.
    #[allow(clippy::too_many_arguments)]
    fn slider(&mut self, ui: &mut Ui, key: &str, label: &str, x: f32, w: f32, y: f32, range: (f32, f32, f32), was: f32, show: impl Fn(f32) -> String) -> f32 {
        let mut v = was;
        ui.text(label, r(x, y, w - 60.0, 18.0), 12.0, 500, MUTED, Align::Left);
        ui.text(&show(v), r(x + w - 60.0, y, 60.0, 18.0), 12.0, 600, MUTED, Align::Right);
        ui.slider(key, r(x - 9.0, y + 16.0, w + 18.0, 28.0), range.0, range.1, range.2, &mut v);
        if v != was && !self.slider_open { self.undo.push(self.p.clone()); self.redo.clear(); self.edited = true; self.playing = None; self.slider_open = true; }
        v
    }

    // ---- footer and keys ----

    fn footer(&mut self, ui: &mut Ui, w: f32, h: f32) {
        let fy = h - FOOT_H;
        ui.divider(fy, 0.0, w);
        let toast = self.toast.as_ref().filter(|(_, at)| at.elapsed().as_secs_f32() < 6.0).map(|(s, _)| s.clone());
        if toast.is_some() { ui.want_anim = true; }
        let hint = toast.clone().unwrap_or_else(|| match &self.drag {
            Some(Drag::Move { moving: true, .. }) => "Drop in a gap to leave it there, or over a clip to go in before or after it.".into(),
            Some(Drag::TrimStart { .. } | Drag::TrimEnd { .. }) => "Edges stop at the start and end of the source, and at the next clip.".into(),
            _ => "Space plays. S splits at the playhead. Drag clips to move them, drag their edges to trim.".into(),
        });
        ui.text(&hint, r(PAD, fy, w - 330.0, FOOT_H), 12.5, 400, if toast.is_some() { TEXT } else { MUTED }, Align::Left);
        let by = fy + (FOOT_H - 38.0) / 2.0;
        let primary = r(w - PAD - 130.0, by, 130.0, 38.0);
        if ui.button("ed.close", "Close", r(primary.x - 96.0, by, 88.0, 38.0), Style::Ghost, true) { self.want = Some(Want::Close); }
        if ui.button("ed.export", "Export…", primary, Style::Primary, self.p.clip_count() > 0) { self.want = Some(Want::Export); }
    }

    fn keys(&mut self, ui: &mut Ui) {
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) < 0 };
        if ui.key(VK_SPACE) { self.toggle_play(); }
        if ui.key(VK_S) { self.split_here(); }
        if ui.key(VK_DELETE) || ui.key(VK_BACK) { self.delete_selected(shift); }
        if ui.key(VK_M) {
            if let Some(s) = self.sel {
                self.checkpoint();
                if let Some(c) = self.p.clip_mut(s) { c.muted = !c.muted; }
            }
        }
        let step = if shift { 1.0 } else { 1.0 / self.fps() };
        if ui.key(VK_LEFT) { self.seek(self.playhead - step); }
        if ui.key(VK_RIGHT) { self.seek(self.playhead + step); }
        if ui.key(VK_HOME) { self.seek(0.0); self.scroll_t = 0.0; }
        if ui.key(VK_END) { let d = self.duration(); self.seek(d); }
        if ui.key(VK_OEM_PLUS) || ui.key(VK_ADD) { self.zoom(1.5, None); }
        if ui.key(VK_OEM_MINUS) || ui.key(VK_SUBTRACT) { self.zoom(1.0 / 1.5, None); }
        if ui.key(VK_ESCAPE) { if self.playing.is_some() { self.toggle_play(); } else { self.sel = None; } }
        match ui.ctrl_key(VK_Z) { Some(true) => self.step(false), Some(false) => self.step(true), None => {} }
        if ui.ctrl_key(VK_Y).is_some() { self.step(false); }
        if ui.ctrl_key(VK_E).is_some() && self.p.clip_count() > 0 { self.want = Some(Want::Export); }
    }

    // ---- what opens other windows ----

    fn open_markup(&mut self, cid: u64) {
        let Some(c) = self.p.clip(cid).cloned() else { return };
        let m = self.p.media[c.media].clone();
        if m.video.is_none() { return; }
        self.playing = None;
        let (path, at, still) = (m.path.clone(), (c.src_in + c.src_out) / 2.0, m.still);
        let first = crate::tools::pump_job("Reading file", move |_| if still { crate::image::load(&path) } else { crate::tools::frame_at(&path, at) });
        let first = match first { Ok(b) => b, Err(e) => { self.toast(e); return; } };
        form::set_context(name_of(&m.path));
        let source = (!m.still).then(|| crate::tools::frame_source(&m.path, m.dur, m.video.as_ref()));
        let title = "Redact and annotate clip";
        if let Some(shapes) = canvas::markup(title, first, c.shapes.clone(), vec![], "Done", source) {
            if shapes != c.shapes {
                self.checkpoint();
                if let Some(k) = self.p.clip_mut(cid) { k.shapes = shapes; }
            }
        }
    }

    fn run_export(&mut self) {
        let has_picture = self.p.has_picture();
        let has_sound = self.p.lanes().iter().filter_map(|l| self.p.track(*l)).any(|t| t.clips.iter().any(|c| self.p.media[c.media].has_audio));
        let (w, h, fps) = cut::natural_frame(&self.p);
        let start = self.export.clone().unwrap_or_else(|| Export::preset(1, if has_picture { Format::Mp4 } else { Format::Audio }));
        let page = ExportPage::new(start, has_picture, has_sound, (w, h, fps), self.duration());
        let Some(done) = ui::run_modal(page, "Export", None) else { return };
        let Some(e) = done.result else { return };
        self.export = Some(e.clone());
        let first = self.p.video.clips.first().or_else(|| self.p.audio.iter().flat_map(|t| t.clips.first()).next()).map(|c| self.p.media[c.media].path.clone());
        let Some(first) = first else { return };
        let out = sibling(&first, " edit", e.format.ext());
        let name = name_of(&out);
        let (p, layers) = (self.p.clone(), self.layers.clone());
        spawn("Exporting edit".into(), vec![out], move |j, out: &PathBuf| export(j, &p, &e, &layers, out));
        self.toast(format!("Exporting to {name}. The card in the corner shows how far it has got. You can keep editing."));
    }
}

/// Writes the edit to `out`.
fn export(j: &Job, p: &Project, e: &Export, layers: &LayerDir, out: &Path) -> Result<PathBuf, String> {
    let total = p.duration();
    if total <= 0.0 { return Err("Nothing to export".into()); }
    let (w, h, fps0) = cut::natural_frame(p);
    let (w, h) = cut::frame_for_height(w, h, e.height);
    let mut fps = e.fps.map(|f| (f as f64).min(fps0)).unwrap_or(fps0);
    if e.format == Format::Gif { fps = fps.min(25.0); }
    let picture = e.format != Format::Audio && p.has_picture();
    let sound = e.format != Format::Gif;
    let mut g = cut::graph(p, 0.0, total, &cut::Out { w, h, fps, picture, sound }, &|id, lw, lh| layers.get(p, id, lw, lh));
    if e.format == Format::Gif {
        if let Some(pic) = g.picture.clone() {
            let (tail, label) = cut::gif_tail(&pic);
            g.filter = format!("{};{tail}", g.filter);
            g.picture = Some(label);
        }
    }
    let mut args = vec!["-filter_complex".to_string(), g.filter.clone()];
    args.extend(cut::encode_args(e, &g, fps));
    atomically(out, |tmp| crate::media::ffmpeg_from(j, &g.inputs, &[], &args, tmp, total))
}

impl Page for Editor {
    fn size(&self) -> (f32, f32) { self.start_size }
    fn resizable(&self) -> bool { true }
    fn done(&self) -> bool { self.done }
    fn on_close(&mut self) { self.want = Some(Want::Close); }
    fn message(&mut self, hwnd: HWND, msg: u32, wp: WPARAM, _lp: LPARAM) -> Option<LRESULT> {
        if self.hwnd.is_none() {
            self.hwnd = Some(hwnd);
            unsafe { DragAcceptFiles(hwnd, true); }
        }
        if msg != WM_DROPFILES { return None; }
        unsafe {
            let h = HDROP(wp.0 as *mut _);
            let n = DragQueryFileW(h, 0xFFFF_FFFF, None);
            let mut paths = vec![];
            for i in 0..n {
                let mut b = vec![0u16; 32768];
                let l = DragQueryFileW(h, i, Some(&mut b)) as usize;
                paths.push(PathBuf::from(String::from_utf16_lossy(&b[..l])));
            }
            let mut pt = POINT::default();
            let _ = DragQueryPoint(h, &mut pt);
            DragFinish(h);
            let scale = dpi_scale_at(cursor_pos());
            let m = if self.maximized { 0.0 } else { ui::SHADOW };
            self.want = Some(Want::Drop(paths, (pt.x as f32 / scale - m, pt.y as f32 / scale - m)));
            ui::refresh(hwnd);
        }
        Some(LRESULT(0))
    }
    fn frame(&mut self, ui: &mut Ui) {
        let (w, h) = (ui.w, ui.h);
        self.maximized = ui.maximized;
        ui.maximize_button(w - 80.0);
        let sub = format!("{} clips  ·  {}", self.p.clip_count(), crate::timeline::fmt_time(self.duration()));
        if ui.titlebar("Editor", &sub) { self.want = Some(Want::Close); }
        ui.divider(TITLE_H, 0.0, w);
        self.take_assets(ui);
        self.update_view(ui);
        let body = r(0.0, TITLE_H + 1.0, w, h - TITLE_H - FOOT_H - 1.0);
        let tl_h = (RULER_H + self.rows_h() + 6.0).min(body.h * 0.5).max(180.0);
        let top_h = body.h - tl_h - BAR_H;
        self.inspector(ui, r(w - SIDE, body.y, SIDE, top_h));
        self.viewer(ui, r(0.0, body.y, w - SIDE, top_h));
        self.transport(ui, r(0.0, body.y + top_h, w, BAR_H));
        self.timeline(ui, r(0.0, body.y + top_h + BAR_H, w, tl_h));
        self.footer(ui, w, h);
        self.keys(ui);
        // The layout of a drop is only known here, so its lane and time are worked out now.
        if let Some(Want::Drop(paths, at)) = self.want.take_if(|w| matches!(w, Want::Drop(..))) {
            let lanes = r(HEAD_W, body.y + top_h + BAR_H + RULER_H, w - HEAD_W, tl_h - RULER_H);
            let mut y = lanes.y - self.vscroll;
            let mut lane = None;
            for (row, rh) in self.rows() {
                if at.1 >= y && at.1 < y + rh + 2.0 { lane = Self::row_lane(row).or(Some(Lane::Audio(self.p.audio.len()))); }
                y += rh + 2.0;
            }
            let t = (lane.is_some() && at.0 >= lanes.x).then(|| self.t_of(at.0, lanes).max(0.0));
            let sound = match lane { Some(Lane::Audio(i)) => Some(Lane::Audio(i)), _ => None };
            self.want = Some(Want::Place(paths, sound, t));
        }
    }
    fn after(&mut self) -> bool {
        // Another window is about to take over, so nothing goes on playing behind it.
        if matches!(self.want, Some(Want::Export | Want::AddFiles | Want::Markup(_) | Want::Close)) { self.playing = None; self.shown = None; }
        match self.want.take() {
            None => false,
            Some(Want::Export) => { self.run_export(); true }
            Some(Want::AddFiles) => {
                let lane = Some(self.lane);
                let files = pick_files(self.hwnd);
                if !files.is_empty() { self.add_files(files, lane, None); }
                true
            }
            Some(Want::Markup(cid)) => { self.open_markup(cid); true }
            Some(Want::Place(paths, lane, t)) => { self.add_files(paths, lane, t); true }
            // Placed by the next frame, which knows the layout.
            Some(w @ Want::Drop(..)) => { self.want = Some(w); false }
            Some(Want::Close) => {
                if !self.edited || form::show("Close the editor?", &[form::Field::Note("This edit is not saved anywhere. Export it first to keep it.".into())], "Close").is_some() {
                    self.playing = None;
                    self.done = true;
                }
                true
            }
        }
    }
}

/// Asks for files to add. The dialog belongs to the editor, so it opens in front of it.
fn pick_files(owner: Option<HWND>) -> Vec<PathBuf> {
    use windows::core::*;
    use windows::Win32::System::Com::*;
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    unsafe {
        let Ok(d) = CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) else { return vec![] };
        let Ok(o) = d.GetOptions() else { return vec![] };
        let _ = d.SetOptions(o | FOS_ALLOWMULTISELECT | FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST);
        let _ = d.SetTitle(w!("Add clips"));
        let specs = [
            COMDLG_FILTERSPEC { pszName: w!("Video, pictures and sound"), pszSpec: w!("*.mp4;*.mov;*.mkv;*.webm;*.avi;*.wmv;*.m4v;*.gif;*.mts;*.m2ts;*.mpg;*.mpeg;*.jpg;*.jpeg;*.png;*.webp;*.bmp;*.tif;*.tiff;*.heic;*.mp3;*.m4a;*.wav;*.flac;*.ogg;*.opus;*.aac;*.wma;*.aiff") },
            COMDLG_FILTERSPEC { pszName: w!("All files"), pszSpec: w!("*.*") },
        ];
        let _ = d.SetFileTypes(&specs);
        if d.Show(owner).is_err() { return vec![]; }
        let Ok(items) = d.GetResults() else { return vec![] };
        let n = items.GetCount().unwrap_or(0);
        (0..n).filter_map(|i| {
            let s = items.GetItemAt(i).ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
            let p = s.to_string().ok();
            CoTaskMemFree(Some(s.0 as *const _));
            p.map(PathBuf::from)
        }).collect()
    }
}

/// Opens the editor with these files, in order, and stays until it is closed. Exports run while it is open.
pub fn open(paths: Vec<PathBuf>) {
    form::set_context(String::new());
    let found = crate::tools::pump_job("Reading files", move |_| Ok(read_all(&paths))).unwrap_or_default();
    let mut p = Project::default();
    let mut problems = vec![];
    for m in found {
        match m {
            Ok(m) => { let i = p.add_media(m); let l = p.lane_for(i, None); p.insert(i, l, None); }
            Err(e) => problems.push(e),
        }
    }
    let mut e = Editor::new(p);
    for i in 0..e.p.media.len() { e.load_assets(i); }
    if !problems.is_empty() { e.toast(problems.join(". ")); }
    ui::run_modal(e, "Editor", None);
}

// ---- export window ----

struct ExportPage {
    e: Export,
    picture: bool,
    sound: bool,
    frame: (u32, u32, f64),
    dur: f64,
    result: Option<Export>,
    closed: bool,
}

const ENCODINGS: [(&str, crate::quality::Speed); 4] = [
    ("Best: smallest for the quality, slowest", crate::quality::Speed::Best),
    ("Quality: near the best, quicker", crate::quality::Speed::Quality),
    ("Balanced: a little larger, much quicker", crate::quality::Speed::Balanced),
    ("Fast: largest, quickest", crate::quality::Speed::Fast),
];

impl ExportPage {
    fn new(mut e: Export, picture: bool, sound: bool, frame: (u32, u32, f64), dur: f64) -> ExportPage {
        if !picture { e.format = Format::Audio; }
        if !sound && e.format == Format::Audio { e.format = Format::Mp4; }
        ExportPage { e, picture, sound, frame, dur, result: None, closed: false }
    }
    fn formats(&self) -> Vec<Format> {
        Format::ALL.into_iter().filter(|f| match f { Format::Mp4 | Format::Gif => self.picture, Format::Audio => self.sound }).collect()
    }
    /// Rough size from the bitrates the settings tend to give. Only a guide: busy pictures take more.
    fn estimate(&self) -> String {
        let (w, h) = cut::frame_for_height(self.frame.0, self.frame.1, self.e.height);
        let fps = self.e.fps.map(|f| (f as f64).min(self.frame.2)).unwrap_or(self.frame.2);
        let audio_bits = if self.sound && self.e.format != Format::Gif { self.e.audio_kbps as f64 * 1000.0 } else { 0.0 };
        let video_bits = match self.e.format {
            Format::Audio => 0.0,
            // Bits per pixel per frame for x264 at a CRF, a rule of thumb for ordinary footage.
            Format::Mp4 => w as f64 * h as f64 * fps.min(60.0) * 0.1 * 2f64.powf(-(crate::quality::crf_for(self.e.quality) as f64 - 18.0) / 6.0),
            Format::Gif => w as f64 * h as f64 * fps.min(25.0) * 2.0,
        };
        format!("About {}", crate::preview::format_bytes(((video_bits + audio_bits) / 8.0 * self.dur) as u64))
    }
}

impl Page for ExportPage {
    fn size(&self) -> (f32, f32) { (520.0, match self.e.format { Format::Audio => 400.0, Format::Gif => 470.0, Format::Mp4 => 540.0 }) }
    fn done(&self) -> bool { self.closed }
    fn on_close(&mut self) { self.closed = true; }
    fn frame(&mut self, ui: &mut Ui) {
        let (w, h) = (ui.w, ui.h);
        if ui.titlebar("Export", &form::context()) { self.closed = true; }
        ui.divider(TITLE_H, 0.0, w);
        let (x, cw) = (PAD + 4.0, w - PAD * 2.0 - 8.0);
        let mut y = TITLE_H + 18.0;
        let label = |ui: &mut Ui, s: &str, y: f32| ui.text(s, r(x, y, cw, 18.0), 12.0, 500, MUTED, Align::Left);

        let formats = self.formats();
        let names: Vec<String> = formats.iter().map(|f| f.label().to_string()).collect();
        let mut fi = formats.iter().position(|f| *f == self.e.format).unwrap_or(0);
        label(ui, "Format", y);
        ui.segmented("ex.format", r(x, y + 22.0, cw, 34.0), &names, &mut fi);
        self.e.format = formats[fi.min(formats.len() - 1)];
        y += 70.0;

        // Presets set everything below; changing anything below makes it Custom.
        let mut presets: Vec<String> = cut::PRESETS.iter().map(|p| p.0.to_string()).collect();
        presets.push("Custom".into());
        let was = self.e.matching_preset().unwrap_or(cut::PRESETS.len());
        let mut sel = was;
        label(ui, "Quality preset", y);
        ui.select("ex.preset", r(x, y + 22.0, cw, 36.0), &presets, &mut sel, true);
        if sel != was && sel < cut::PRESETS.len() { let f = self.e.format; let s = self.e.speed; self.e = Export::preset(sel, f); self.e.speed = s; }
        y += 72.0;

        let half = (cw - 12.0) / 2.0;
        if self.e.format != Format::Audio {
            let qn: Vec<String> = cut::QUALITIES.iter().map(|q| q.0.to_string()).collect();
            let qi = cut::QUALITIES.iter().position(|q| q.1 == self.e.quality).unwrap_or(2);
            let mut s = qi;
            label(ui, "Picture quality", y);
            let gif = self.e.format == Format::Gif;
            ui.select("ex.q", r(x, y + 22.0, half, 34.0), &qn, &mut s, !gif);
            if s != qi { self.e.quality = cut::QUALITIES[s].1; }
            let heights: Vec<u32> = cut::HEIGHTS.iter().copied().filter(|h| *h == 0 || *h < self.frame.1).collect();
            let hn: Vec<String> = heights.iter().map(|h| if *h == 0 { format!("Same, {}p", self.frame.1) } else { format!("{h}p") }).collect();
            let hi = heights.iter().position(|h| Some(*h) == self.e.height || (*h == 0 && self.e.height.is_none_or(|t| t >= self.frame.1))).unwrap_or(0);
            let mut s = hi;
            ui.text("Resolution", r(x + half + 12.0, y, half, 18.0), 12.0, 500, MUTED, Align::Left);
            ui.select("ex.h", r(x + half + 12.0, y + 22.0, half, 34.0), &hn, &mut s, true);
            if s != hi { self.e.height = (heights[s] > 0).then_some(heights[s]); }
            y += 70.0;
            let rates: Vec<u32> = cut::RATES.iter().copied().filter(|r| *r == 0 || (*r as f64) < self.frame.2 - 0.5).collect();
            let rn: Vec<String> = rates.iter().map(|r| if *r == 0 { format!("Same, {} fps", (self.frame.2 * 100.0).round() / 100.0) } else { format!("{r} fps") }).collect();
            let ri = rates.iter().position(|r| Some(*r) == self.e.fps || (*r == 0 && self.e.fps.is_none())).unwrap_or(0);
            let mut s = ri;
            label(ui, "Frame rate", y);
            ui.select("ex.fps", r(x, y + 22.0, half, 34.0), &rn, &mut s, true);
            if s != ri { self.e.fps = (rates[s] > 0).then_some(rates[s]); }
            let en: Vec<String> = ENCODINGS.iter().map(|e| e.0.split(':').next().unwrap_or("").to_string()).collect();
            let ei = ENCODINGS.iter().position(|e| e.1 == self.e.speed).unwrap_or(1);
            let mut s = ei;
            ui.text("Encoding", r(x + half + 12.0, y, half, 18.0), 12.0, 500, MUTED, Align::Left);
            ui.select("ex.enc", r(x + half + 12.0, y + 22.0, half, 34.0), &en, &mut s, !gif);
            if s != ei { self.e.speed = ENCODINGS[s].1; }
            y += 70.0;
        }
        if self.e.format != Format::Gif {
            let an: Vec<String> = cut::AUDIO_KBPS.iter().map(|k| format!("{k} kbps")).collect();
            let ai = cut::AUDIO_KBPS.iter().position(|k| *k == self.e.audio_kbps).unwrap_or(2);
            let mut s = ai;
            label(ui, "Sound quality", y);
            ui.select("ex.a", r(x, y + 22.0, half, 34.0), &an, &mut s, self.sound);
            if s != ai { self.e.audio_kbps = cut::AUDIO_KBPS[s]; }
            y += 70.0;
        }
        let note = match self.e.format {
            Format::Mp4 => format!("{}. H.264 and AAC, which plays everywhere.", self.estimate()),
            Format::Gif => format!("{}. No sound, at most 25 frames a second and 256 colours a frame.", self.estimate()),
            Format::Audio => format!("{}. Every sound track mixed into one.", self.estimate()),
        };
        ui.wrapped(&note, r(x, y, cw, 40.0), 12.0, 400, MUTED);

        let fy = h - FOOT_H;
        ui.divider(fy, 0.0, w);
        let by = fy + (FOOT_H - 38.0) / 2.0;
        let primary = r(w - PAD - 120.0, by, 120.0, 38.0);
        if ui.button("ex.cancel", "Cancel", r(primary.x - 96.0, by, 88.0, 38.0), Style::Ghost, true) { self.closed = true; }
        if ui.button("ex.ok", "Export", primary, Style::Primary, true) || ui.key(VK_RETURN) { self.result = Some(self.e.clone()); self.closed = true; }
        if ui.key(VK_ESCAPE) { self.closed = true; }
    }
}

// ---- design review ----

/// The editor with a sample edit, as a PNG.
pub fn render(photo: Bmp, out: &Path) -> Result<(), String> {
    let vi = |w, h| Some(crate::quality::VideoInfo { width: w, height: h, fps: 29.97, ..Default::default() });
    let mut p = Project::default();
    let a = p.add_media(Media { path: "C:\\v\\interview take 3.mp4".into(), dur: 42.0, video: vi(1920, 1080), has_audio: true, still: false });
    let b = p.add_media(Media { path: "C:\\v\\b-roll street.mov".into(), dur: 18.0, video: vi(1920, 1080), has_audio: true, still: false });
    let m = p.add_media(Media { path: "C:\\v\\theme.mp3".into(), dur: 70.0, video: None, has_audio: true, still: false });
    let ia = p.insert(a, Lane::Video, None);
    let ib = p.insert(b, Lane::Video, Some(46.0));
    p.insert(m, Lane::Audio(0), Some(3.0));
    let cut_at = p.split(&[Lane::Video], 20.0)[0];
    p.set_speed(ib, 2.0);
    if let Some(c) = p.clip_mut(ia) { c.fade_in = 1.0; c.shapes = vec![canvas::Shape::cover_rect(0.6, 0.2, 0.8, 0.4)]; }
    if let Some(c) = p.clip_mut(cut_at) { c.volume = 0.6; }
    p.video.muted = false;
    let mut e = Editor::new(p);
    e.offline = true;
    e.start_size = (1280.0, 800.0);
    e.playhead = 24.5;
    e.sel = Some(cut_at);
    let thumbs: Vec<Bmp> = (0..THUMBS).map(|i| { let mut t = small(&photo).unwrap(); for px in t.px.chunks_exact_mut(4) { px[0] = px[0].saturating_add((i * 6) as u8); } t }).collect();
    let mut wave = Bmp::new(WAVE_W, 80, [0, 0, 0, 0]);
    for x in 0..WAVE_W {
        let a = ((x as f32 * 0.05).sin().abs() * 0.6 + (x as f32 * 0.31).sin().abs() * 0.4) * 36.0;
        for y in (40.0 - a) as u32..(40.0 + a) as u32 { let i = ((y * WAVE_W + x) * 4) as usize; wave.px[i..i + 4].copy_from_slice(&[0xC7, 0xE4, 0xB7, 255]); }
    }
    for i in 0..3 { e.assets.insert(i, Assets { thumbs: if i < 2 { thumbs.clone() } else { vec![] }, wave: Some(wave.clone()), pending: false }); }
    e.frame = photo.scaled(960, 540).ok();
    ui::render_png(&mut e, 1.0, None, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn export_preset_survives_a_format_change() {
        let mut pg = ExportPage::new(Export::preset(3, Format::Mp4), true, true, (1920, 1080, 30.0), 10.0);
        assert_eq!(pg.e.matching_preset(), Some(3));
        pg.e.format = Format::Gif;
        assert_eq!(pg.e.matching_preset(), Some(3));
        assert!(pg.estimate().starts_with("About"));
        let silent = ExportPage::new(Export::preset(1, Format::Audio), true, false, (1920, 1080, 30.0), 10.0);
        assert_eq!(silent.e.format, Format::Mp4, "nothing to hear, so not sound only");
        assert!(!silent.formats().contains(&Format::Audio));
    }
}
