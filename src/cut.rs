//! The editor's timeline: clips on one picture track and any number of sound tracks, the edits made to them, and the
//! one ffmpeg graph that renders any stretch of the result. Export, playback and the still under the playhead all come
//! from that graph, so what is seen while editing is what gets saved. No Win32 here, so all of it can be tested.
use crate::canvas::{self, Shape};
use crate::quality::VideoInfo;
use std::path::PathBuf;

/// Shortest a clip can be trimmed to.
pub const MIN_LEN: f64 = 0.1;
/// How long a picture lasts when it is added.
pub const STILL_LEN: f64 = 5.0;
/// Times this close count as the same, so float rounding never opens a hairline gap.
const EPS: f64 = 1e-6;

#[derive(Clone, Debug, Default)]
pub struct Media {
    pub path: PathBuf,
    /// Length in seconds. A picture has none of its own and can be stretched to any length.
    pub dur: f64,
    pub video: Option<VideoInfo>,
    pub has_audio: bool,
    pub still: bool,
}

impl Media {
    /// The size the picture is shown at, or none for sound.
    pub fn size(&self) -> Option<(u32, u32)> { self.video.as_ref().map(|v| v.display_size()).filter(|s| s.0 > 0 && s.1 > 0) }
    /// How far a clip of it can reach into the source.
    pub fn limit(&self) -> f64 { if self.still { f64::MAX } else { self.dur } }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub id: u64,
    pub media: usize,
    /// Where it starts on the timeline.
    pub start: f64,
    /// The stretch of the source it plays.
    pub src_in: f64,
    pub src_out: f64,
    pub speed: f64,
    /// 1 leaves the sound as it is.
    pub volume: f32,
    pub muted: bool,
    /// Seconds of timeline the picture and sound take to come up from black and silence, and to go back down.
    pub fade_in: f64,
    pub fade_out: f64,
    /// Redactions and annotations, for the whole clip.
    pub shapes: Vec<Shape>,
}

impl Clip {
    pub fn new(id: u64, media: usize, start: f64, len: f64) -> Clip {
        Clip { id, media, start, src_in: 0.0, src_out: len, speed: 1.0, volume: 1.0, muted: false, fade_in: 0.0, fade_out: 0.0, shapes: vec![] }
    }
    /// Its length on the timeline.
    pub fn len(&self) -> f64 { (self.src_out - self.src_in) / self.speed }
    pub fn end(&self) -> f64 { self.start + self.len() }
    fn mid(&self) -> f64 { self.start + self.len() / 2.0 }
    /// The source time playing at timeline time `t`.
    pub fn src_at(&self, t: f64) -> f64 { self.src_in + (t - self.start) * self.speed }
    pub fn covers(&self, t: f64) -> bool { t >= self.start - EPS && t < self.end() - EPS }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track { pub clips: Vec<Clip>, pub muted: bool }

impl Track {
    pub fn end(&self) -> f64 { self.clips.iter().map(Clip::end).fold(0.0, f64::max) }
    fn sort(&mut self) { self.clips.sort_by(|a, b| a.start.total_cmp(&b.start)); }
    pub fn find(&self, id: u64) -> Option<usize> { self.clips.iter().position(|c| c.id == id) }
}

/// Which track: the picture, or a sound track counted from 0. The picture track's clips carry their own sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lane { Video, Audio(usize) }

#[derive(Clone, Debug, Default)]
pub struct Project {
    pub media: Vec<Media>,
    pub video: Track,
    pub audio: Vec<Track>,
    next_id: u64,
}

impl Project {
    pub fn track(&self, l: Lane) -> Option<&Track> { match l { Lane::Video => Some(&self.video), Lane::Audio(i) => self.audio.get(i) } }
    pub fn track_mut(&mut self, l: Lane) -> Option<&mut Track> { match l { Lane::Video => Some(&mut self.video), Lane::Audio(i) => self.audio.get_mut(i) } }
    pub fn lanes(&self) -> Vec<Lane> { std::iter::once(Lane::Video).chain((0..self.audio.len()).map(Lane::Audio)).collect() }
    pub fn duration(&self) -> f64 { self.lanes().iter().filter_map(|l| self.track(*l)).map(Track::end).fold(0.0, f64::max) }
    pub fn clip_count(&self) -> usize { self.lanes().iter().filter_map(|l| self.track(*l)).map(|t| t.clips.len()).sum() }
    pub fn has_picture(&self) -> bool { !self.video.clips.is_empty() }

    /// Where a clip is, by id.
    pub fn locate(&self, id: u64) -> Option<(Lane, usize)> {
        self.lanes().into_iter().find_map(|l| self.track(l)?.find(id).map(|i| (l, i)))
    }
    pub fn clip(&self, id: u64) -> Option<&Clip> { let (l, i) = self.locate(id)?; self.track(l)?.clips.get(i) }
    pub fn clip_mut(&mut self, id: u64) -> Option<&mut Clip> { let (l, i) = self.locate(id)?; self.track_mut(l)?.clips.get_mut(i) }

    fn fresh_id(&mut self) -> u64 { self.next_id += 1; self.next_id }

    /// Adds a source and returns its index. The same file twice is one source.
    pub fn add_media(&mut self, m: Media) -> usize {
        if let Some(i) = self.media.iter().position(|x| x.path == m.path) { return i; }
        self.media.push(m);
        self.media.len() - 1
    }

    /// The lane a source's clips belong on: pictures and video on the picture track, sound on a sound track.
    pub fn lane_for(&self, media: usize, wanted: Option<usize>) -> Lane {
        let m = &self.media[media];
        if m.video.is_some() || m.still { Lane::Video } else { Lane::Audio(wanted.unwrap_or(0)) }
    }

    /// A whole source as a new clip at `at`, or at the end of its track. Returns the clip's id.
    pub fn insert(&mut self, media: usize, lane: Lane, at: Option<f64>) -> u64 {
        if let Lane::Audio(i) = lane { while self.audio.len() <= i { self.audio.push(Track::default()); } }
        let m = &self.media[media];
        let len = if m.still { STILL_LEN } else { m.dur.max(MIN_LEN) };
        let id = self.fresh_id();
        let track = self.track_mut(lane).unwrap();
        let at = at.unwrap_or_else(|| track.end());
        place(track, Clip::new(id, media, 0.0, len), at);
        id
    }

    /// Cuts every clip on the given lanes that runs across `t` in two. Returns the ids of the new right-hand parts.
    pub fn split(&mut self, lanes: &[Lane], t: f64) -> Vec<u64> {
        let mut made = vec![];
        for l in lanes {
            let ids: Vec<u64> = self.track(*l).map(|tr| tr.clips.iter().filter(|c| t > c.start + MIN_LEN / 2.0 && t < c.end() - MIN_LEN / 2.0).map(|c| c.id).collect()).unwrap_or_default();
            for id in ids {
                let nid = self.fresh_id();
                let tr = self.track_mut(*l).unwrap();
                let i = tr.find(id).unwrap();
                let mut right = tr.clips[i].clone();
                let at = tr.clips[i].src_at(t);
                tr.clips[i].src_out = at;
                tr.clips[i].fade_out = 0.0;
                right.id = nid;
                right.src_in = at;
                right.start = t;
                right.fade_in = 0.0;
                tr.clips.insert(i + 1, right);
                made.push(nid);
            }
        }
        made
    }

    /// Takes a clip out. A ripple delete closes the space it leaves.
    pub fn remove(&mut self, id: u64, ripple: bool) {
        let Some((l, i)) = self.locate(id) else { return };
        let tr = self.track_mut(l).unwrap();
        let c = tr.clips.remove(i);
        if ripple { for o in tr.clips.iter_mut().filter(|o| o.start >= c.start - EPS) { o.start -= c.len(); } }
    }

    /// Moves a clip to `start` on `lane`. Dropped over other clips, it goes in between them and pushes the later ones
    /// along; dropped in a gap that fits, it stays exactly there.
    pub fn move_clip(&mut self, id: u64, lane: Lane, start: f64) {
        let Some((from, i)) = self.locate(id) else { return };
        // The picture track only takes pictures, and sound tracks only sound.
        let lane = if (from == Lane::Video) != (lane == Lane::Video) { from } else { lane };
        let c = self.track_mut(from).unwrap().clips.remove(i);
        if let Lane::Audio(k) = lane { while self.audio.len() <= k { self.audio.push(Track::default()); } }
        place(self.track_mut(lane).unwrap(), c, start);
    }

    /// Moves a clip's start edge to `t`, keeping its end where it is.
    pub fn trim_start(&mut self, id: u64, t: f64) {
        let Some((l, i)) = self.locate(id) else { return };
        let tr = self.track_mut(l).unwrap();
        let prev_end = if i > 0 { tr.clips[i - 1].end() } else { 0.0 };
        let c = &mut tr.clips[i];
        let end = c.end();
        let earliest = (c.start - c.src_in / c.speed).max(prev_end);
        let t = t.clamp(earliest, end - MIN_LEN);
        c.src_in = (c.src_in + (t - c.start) * c.speed).max(0.0);
        c.start = t;
        c.fade_in = c.fade_in.min(c.len());
    }

    /// Moves a clip's end edge to `t`, keeping its start where it is.
    pub fn trim_end(&mut self, id: u64, t: f64, limit: f64) {
        let Some((l, i)) = self.locate(id) else { return };
        let tr = self.track_mut(l).unwrap();
        let next_start = tr.clips.get(i + 1).map(|n| n.start).unwrap_or(f64::MAX);
        let c = &mut tr.clips[i];
        let latest = (c.start + (limit - c.src_in) / c.speed).min(next_start);
        let t = t.clamp(c.start + MIN_LEN, latest.max(c.start + MIN_LEN));
        c.src_out = c.src_in + (t - c.start) * c.speed;
        c.fade_out = c.fade_out.min(c.len());
    }

    /// A new speed for a clip. It keeps the same stretch of the source, so it grows or shrinks, and anything after it
    /// that it would now run into moves along.
    pub fn set_speed(&mut self, id: u64, speed: f64) {
        let Some((l, i)) = self.locate(id) else { return };
        let tr = self.track_mut(l).unwrap();
        tr.clips[i].speed = speed.clamp(0.1, 16.0);
        let c = tr.clips[i].clone();
        tr.clips[i].fade_in = c.fade_in.min(c.len());
        tr.clips[i].fade_out = c.fade_out.min(c.len());
        push_from(tr, i + 1, c.end());
    }

    /// Slides every clip on a lane back so none has space before it.
    pub fn close_gaps(&mut self, lane: Lane) {
        let Some(tr) = self.track_mut(lane) else { return };
        let mut t = 0.0;
        for c in &mut tr.clips { c.start = t; t = c.end(); }
    }

    /// Times worth snapping to: the start, the end of everything, and every clip's edges, except the clip being dragged.
    pub fn snap_points(&self, except: Option<u64>) -> Vec<f64> {
        let mut v = vec![0.0];
        for l in self.lanes() {
            for c in self.track(l).map(|t| t.clips.as_slice()).unwrap_or(&[]) {
                if Some(c.id) != except { v.push(c.start); v.push(c.end()); }
            }
        }
        v
    }
}

/// Puts a clip in a track at `start`. In a gap that fits, it goes exactly there. Over other clips, it goes after the
/// last one whose middle it has passed, and everything after it moves along just enough to make room.
fn place(tr: &mut Track, mut c: Clip, start: f64) {
    c.start = start.max(0.0);
    let overlaps = tr.clips.iter().any(|o| o.start < c.end() - EPS && c.start < o.end() - EPS);
    if overlaps {
        let before = tr.clips.iter().filter(|o| o.mid() <= c.start).map(Clip::end).fold(0.0, f64::max);
        c.start = c.start.max(before);
        // The first clip that stays after it, and how far everything from there must move.
        let at = tr.clips.iter().filter(|o| o.mid() > c.start).map(|o| o.start).fold(f64::MAX, f64::min);
        if at < c.end() {
            let shift = c.end() - at;
            for o in tr.clips.iter_mut().filter(|o| o.start >= at - EPS) { o.start += shift; }
        }
    }
    tr.clips.push(c);
    tr.sort();
}

/// Moves the clips from index `i` on, if the first of them starts before `t`, so that it starts there.
fn push_from(tr: &mut Track, i: usize, t: f64) {
    let Some(first) = tr.clips.get(i) else { return };
    let shift = t - first.start;
    if shift <= EPS { return; }
    for o in &mut tr.clips[i..] { o.start += shift; }
}

/// The nearest point within `tol` of `t`, or `t` itself.
pub fn snap_to(t: f64, points: &[f64], tol: f64) -> f64 {
    points.iter().copied().filter(|p| (p - t).abs() <= tol).min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs())).unwrap_or(t)
}

// ---- rendering ----

/// What a render makes: picture, sound or both, and at what size and rate.
#[derive(Clone, Debug, PartialEq)]
pub struct Out { pub w: u32, pub h: u32, pub fps: f64, pub picture: bool, pub sound: bool }

/// The frame of the result: the size of the first picture, or 1280x720 when there is none, and its rate, in whole even
/// pixels since every common codec needs them.
pub fn natural_frame(p: &Project) -> (u32, u32, f64) {
    let first = p.video.clips.iter().filter_map(|c| p.media.get(c.media)).find(|m| !m.still && m.size().is_some())
        .or_else(|| p.video.clips.iter().filter_map(|c| p.media.get(c.media)).find(|m| m.size().is_some()));
    let (w, h) = first.and_then(Media::size).unwrap_or((1280, 720));
    let fps = first.and_then(|m| m.video.as_ref()).map(|v| v.fps).filter(|f| *f > 0.0 && *f <= 120.0).unwrap_or(30.0);
    (even(w), even(h), fps)
}

/// The same shape no taller than `height`, in even pixels. None keeps the size.
pub fn frame_for_height(w: u32, h: u32, height: Option<u32>) -> (u32, u32) {
    match height.filter(|t| *t < h) {
        Some(t) => (even((w as f64 * t as f64 / h as f64).round() as u32), even(t)),
        None => (even(w), even(h)),
    }
}

/// The largest size with the same shape as `w`x`h` that fits inside `bw`x`bh`, in even pixels.
pub fn fit_even(w: u32, h: u32, bw: u32, bh: u32) -> (u32, u32) {
    let f = (bw as f64 / w.max(1) as f64).min(bh as f64 / h.max(1) as f64);
    (even((w as f64 * f).round() as u32), even((h as f64 * f).round() as u32))
}

fn even(v: u32) -> u32 { (v & !1).max(2) }

/// A graph for one render, with the inputs it reads.
pub struct Graph {
    /// Everything that goes before the filter: each input with its own seek.
    pub inputs: Vec<String>,
    pub filter: String,
    /// The picture's and the sound's outputs, as labels to map.
    pub picture: Option<String>,
    pub sound: Option<String>,
}

/// Annotation pictures, by clip id and size, already saved as PNGs by the caller.
pub type Layers<'a> = &'a dyn Fn(u64, u32, u32) -> Option<PathBuf>;

/// The part of a clip inside `[from, to)` on the timeline, as (timeline start, timeline end, source start).
fn part(c: &Clip, from: f64, to: f64) -> Option<(f64, f64, f64)> {
    let (a, b) = (c.start.max(from), c.end().min(to));
    (b - a > 1e-3).then(|| (a, b, c.src_at(a)))
}

/// The inputs of a render, in order. A video clip's picture and sound share one.
#[derive(Default)]
struct Inputs { args: Vec<String>, n: usize, opened: Vec<(u64, usize)> }

impl Inputs {
    fn add(&mut self, a: Vec<String>) -> usize { self.args.extend(a); self.n += 1; self.n - 1 }
    /// The input for the part of a clip from `a` to `b` on the timeline, starting at `src` in its source.
    fn clip(&mut self, p: &Project, c: &Clip, a: f64, b: f64, src: f64, fps: f64) -> usize {
        if let Some(k) = self.opened.iter().find(|o| o.0 == c.id).map(|o| o.1) { return k; }
        let m = &p.media[c.media];
        let path = m.path.display().to_string();
        let a = if m.still {
            vec!["-loop".into(), "1".into(), "-framerate".into(), num(fps), "-t".into(), num(b - a), "-i".into(), path]
        } else {
            vec!["-ss".into(), num(src), "-t".into(), num((b - a) * c.speed), "-i".into(), path]
        };
        let k = self.add(a);
        self.opened.push((c.id, k));
        k
    }
}

fn num(v: f64) -> String { format!("{v:.6}") }

/// Renders `[from, to)` of the edit. Each clip is read from its own seek, so a long source costs only the part used.
pub fn graph(p: &Project, from: f64, to: f64, out: &Out, layers: Layers) -> Graph {
    let mut ins = Inputs::default();
    let mut g: Vec<String> = vec![];
    let span = (to - from).max(1e-3);
    let (w, h, fps) = (out.w, out.h, out.fps);

    let picture = out.picture.then(|| {
        let mut segs: Vec<String> = vec![];
        let mut t = from;
        let gap = |d: f64, i: usize, g: &mut Vec<String>| -> String {
            g.push(format!("color=c=black:s={w}x{h}:r={}:d={},format=yuv420p,setsar=1[vg{i}]", num(fps), num(d)));
            format!("[vg{i}]")
        };
        for c in &p.video.clips {
            let Some((a, b, src)) = part(c, from, to) else { continue };
            if a - t > 1e-3 { segs.push(gap(a - t, segs.len(), &mut g)); }
            let k = ins.clip(p, c, a, b, src, fps);
            let m = &p.media[c.media];
            let (sw, sh) = m.size().map(|(mw, mh)| fit_even(mw, mh, w, h)).unwrap_or((w, h));
            let i = segs.len();
            let mut chain = vec![format!("[{k}:v:0]setpts=(PTS-STARTPTS)/{}", num(if m.still { 1.0 } else { c.speed }))];
            if let Some(f) = m.video.as_ref().and_then(crate::quality::still_filter) { chain.push(f); }
            chain.push(format!("scale={sw}:{sh}:flags=bicubic,setsar=1,fps={},format=yuv420p", num(fps)));
            let (covers, notes) = canvas::split(&c.shapes);
            let mut label = format!("vc{i}");
            if !covers.is_empty() {
                if let Some(r) = crate::quality::redact_graph("yuv420p", "bt709", false, sw, sh, &canvas::marks_for(&covers, sw, sh, true)) {
                    chain.push(r);
                }
            }
            g.push(format!("{}[{label}]", chain.join(",")));
            if !notes.is_empty() {
                if let Some(png) = layers(c.id, sw, sh) {
                    let li = ins.add(vec!["-i".into(), png.display().to_string()]);
                    g.push(format!("[{label}][{li}:v]overlay=format=auto:eof_action=repeat,format=yuv420p[vn{i}]"));
                    label = format!("vn{i}");
                }
            }
            // The clip is cut to exactly its length, padded to the frame, and faded where it fades.
            let mut tail = vec![format!("trim=duration={}", num(b - a)), format!("pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:black"), "setsar=1".to_string()];
            let into = a - c.start;
            if c.fade_in > 0.0 && into < c.fade_in { tail.push(format!("fade=t=in:st=0:d={}", num(c.fade_in - into))); }
            if c.fade_out > 0.0 && c.end() - c.fade_out < b {
                let st = (c.end() - c.fade_out - a).max(0.0);
                tail.push(format!("fade=t=out:st={}:d={}", num(st), num((b - a - st).max(1e-3))));
            }
            g.push(format!("[{label}]{}[vs{i}]", tail.join(",")));
            segs.push(format!("[vs{i}]"));
            t = b;
        }
        if to - t > 1e-3 { segs.push(gap(to - t, segs.len(), &mut g)); }
        if segs.len() == 1 { g.push(format!("{}null[vout]", segs[0])); }
        else { g.push(format!("{}concat=n={}:v=1:a=0[vout]", segs.concat(), segs.len())); }
        "[vout]".to_string()
    });

    let sound = out.sound.then(|| {
        let silence = |d: f64, tag: &str, g: &mut Vec<String>| -> String {
            g.push(format!("anullsrc=r=48000:cl=stereo,atrim=duration={}[{tag}]", num(d)));
            format!("[{tag}]")
        };
        let mut lanes: Vec<String> = vec![];
        for (li, l) in p.lanes().into_iter().enumerate() {
            let tr = p.track(l).unwrap();
            let mut segs: Vec<String> = vec![];
            let mut t = from;
            for c in &tr.clips {
                let Some((a, b, src)) = part(c, from, to) else { continue };
                if a - t > 1e-3 { segs.push(silence(a - t, &format!("ag{li}_{}", segs.len()), &mut g)); }
                let m = &p.media[c.media];
                let tag = format!("as{li}_{}", segs.len());
                if tr.muted || c.muted || !m.has_audio || c.volume <= 0.0 {
                    segs.push(silence(b - a, &tag, &mut g));
                } else {
                    let k = ins.clip(p, c, a, b, src, fps);
                    let mut chain = vec![format!("[{k}:a:0]asetpts=PTS-STARTPTS")];
                    if (c.speed - 1.0).abs() > 1e-6 { chain.push(crate::media::atempo_chain(c.speed)); }
                    chain.push("aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo".into());
                    if (c.volume - 1.0).abs() > 1e-3 { chain.push(format!("volume={:.3}", c.volume)); }
                    let into = a - c.start;
                    if c.fade_in > 0.0 && into < c.fade_in { chain.push(format!("afade=t=in:st=0:d={}", num(c.fade_in - into))); }
                    if c.fade_out > 0.0 && c.end() - c.fade_out < b {
                        let st = (c.end() - c.fade_out - a).max(0.0);
                        chain.push(format!("afade=t=out:st={}:d={}", num(st), num((b - a - st).max(1e-3))));
                    }
                    // Sound can stop short of the picture, so it is padded to the clip's exact length.
                    chain.push(format!("apad,atrim=duration={}", num(b - a)));
                    g.push(format!("{}[{tag}]", chain.join(",")));
                    segs.push(format!("[{tag}]"));
                }
                t = b;
            }
            if segs.is_empty() { continue; }
            if to - t > 1e-3 { segs.push(silence(to - t, &format!("ae{li}"), &mut g)); }
            let tag = format!("al{li}");
            if segs.len() == 1 { g.push(format!("{}anull[{tag}]", segs[0])); }
            else { g.push(format!("{}concat=n={}:v=0:a=1[{tag}]", segs.concat(), segs.len())); }
            lanes.push(format!("[{tag}]"));
        }
        match lanes.len() {
            0 => { g.push(format!("anullsrc=r=48000:cl=stereo,atrim=duration={}[aout]", num(span))); }
            1 => g.push(format!("{}anull[aout]", lanes[0])),
            k => g.push(format!("{}amix=inputs={k}:duration=longest:normalize=0[aout]", lanes.concat())),
        }
        "[aout]".to_string()
    });

    Graph { inputs: ins.args, filter: g.join(";"), picture, sound }
}

// ---- export settings ----

/// What to write.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Format { Mp4, Gif, Audio }

impl Format {
    pub const ALL: [Format; 3] = [Format::Mp4, Format::Gif, Format::Audio];
    pub fn label(self) -> &'static str { match self { Format::Mp4 => "MP4 video", Format::Gif => "GIF", Format::Audio => "Sound only" } }
    pub fn ext(self) -> &'static str { match self { Format::Mp4 => "mp4", Format::Gif => "gif", Format::Audio => "m4a" } }
}

/// One export's settings. Picture quality is the same 0 to 100 scale Compress uses.
#[derive(Clone, Debug, PartialEq)]
pub struct Export {
    pub format: Format,
    pub quality: i32,
    /// Tallest picture, or none to keep the frame's own.
    pub height: Option<u32>,
    pub fps: Option<u32>,
    pub audio_kbps: u32,
    pub speed: crate::quality::Speed,
}

/// The presets, best first: name, picture quality, tallest picture, audio kbps.
pub const PRESETS: [(&str, i32, u32, u32); 5] = [
    ("Best quality", 94, 0, 320),
    ("High quality", 80, 0, 192),
    ("Balanced, 1080p", 68, 1080, 160),
    ("Small file, 720p", 56, 720, 128),
    ("Very small, 480p", 44, 480, 64),
];
pub const QUALITIES: [(&str, i32); 6] = [("Visually lossless", 94), ("Very high", 86), ("High", 80), ("Medium", 68), ("Low", 56), ("Very low", 44)];
pub const HEIGHTS: [u32; 7] = [0, 2160, 1440, 1080, 720, 480, 360];
pub const RATES: [u32; 5] = [0, 60, 30, 24, 15];
pub const AUDIO_KBPS: [u32; 7] = [320, 256, 192, 160, 128, 96, 64];

impl Export {
    pub fn preset(i: usize, format: Format) -> Export {
        let (_, q, h, a) = PRESETS[i.min(PRESETS.len() - 1)];
        Export { format, quality: q, height: (h > 0).then_some(h), fps: None, audio_kbps: a, speed: crate::quality::Speed::Quality }
    }
    /// The preset these settings match, if any.
    pub fn matching_preset(&self) -> Option<usize> {
        PRESETS.iter().position(|(_, q, h, a)| *q == self.quality && self.height == (*h > 0).then_some(*h) && *a == self.audio_kbps && self.fps.is_none())
    }
}

/// The encoder half of an export's arguments, after the graph.
pub fn encode_args(e: &Export, g: &Graph, fps: f64) -> Vec<String> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<String>>();
    let mut a = vec![];
    let audio = |a: &mut Vec<String>| {
        if let Some(l) = &g.sound { a.extend(["-map".to_string(), l.clone()]); a.extend(s(&["-c:a", "aac", "-b:a"])); a.push(format!("{}k", e.audio_kbps)); }
    };
    match e.format {
        Format::Mp4 => {
            if let Some(l) = &g.picture {
                let preset = match e.speed { crate::quality::Speed::Best => "slow", crate::quality::Speed::Quality => "medium", crate::quality::Speed::Balanced => "veryfast", crate::quality::Speed::Fast => "ultrafast" };
                a.extend(["-map".to_string(), l.clone()]);
                a.extend(s(&["-c:v", "libx264", "-preset", preset, "-crf"]));
                a.push(crate::quality::crf_for(e.quality).to_string());
                a.extend(s(&["-pix_fmt", "yuv420p"]));
                if fps > 0.0 { a.extend(["-r".to_string(), num(fps)]); }
            }
            audio(&mut a);
            a.extend(s(&["-movflags", "+faststart"]));
        }
        Format::Audio => audio(&mut a),
        Format::Gif => {
            if let Some(l) = &g.picture {
                a.extend(["-map".to_string(), l.clone()]);
                a.extend(s(&["-an", "-gifflags", "-transdiff"]));
            }
        }
    }
    a
}

/// The filter tail that turns the picture into a GIF, and the label it ends on.
pub fn gif_tail(picture: &str) -> (String, String) {
    (format!("{picture}split[gs0][gs1];[gs0]palettegen=stats_mode=diff[gpal];[gs1][gpal]paletteuse=dither=bayer:bayer_scale=4[gout]"), "[gout]".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn media(dur: f64, video: bool) -> Media {
        Media {
            path: PathBuf::from(format!("m{dur}.mp4")), dur, has_audio: true, still: false,
            video: video.then(|| VideoInfo { width: 1920, height: 1080, fps: 30.0, ..Default::default() }),
        }
    }
    fn two() -> (Project, u64, u64) {
        let mut p = Project::default();
        let a = p.add_media(media(10.0, true));
        let b = p.add_media(media(4.0, true));
        let ia = p.insert(a, Lane::Video, None);
        let ib = p.insert(b, Lane::Video, None);
        (p, ia, ib)
    }

    #[test]
    fn clips_append_end_to_end() {
        let (p, a, b) = two();
        assert_eq!(p.clip(a).unwrap().start, 0.0);
        assert_eq!(p.clip(b).unwrap().start, 10.0);
        assert_eq!(p.duration(), 14.0);
    }
    #[test]
    fn splitting_keeps_every_frame_once() {
        let (mut p, a, _) = two();
        let made = p.split(&[Lane::Video], 4.0);
        assert_eq!(made.len(), 1);
        let (l, r) = (p.clip(a).unwrap().clone(), p.clip(made[0]).unwrap().clone());
        assert_eq!((l.src_in, l.src_out, r.src_in, r.src_out), (0.0, 4.0, 4.0, 10.0));
        assert_eq!(r.start, 4.0);
        assert_eq!(p.duration(), 14.0);
        assert!(p.split(&[Lane::Video], 10.0).is_empty(), "an edge is not a cut");
    }
    #[test]
    fn deleting_leaves_a_gap_unless_it_ripples() {
        let (mut p, a, b) = two();
        p.remove(a, false);
        assert_eq!(p.clip(b).unwrap().start, 10.0);
        let (mut p, a, b) = two();
        p.remove(a, true);
        assert_eq!(p.clip(b).unwrap().start, 0.0);
    }
    #[test]
    fn moving_into_a_gap_leaves_it_there_and_over_a_clip_makes_room() {
        let (mut p, a, b) = two();
        p.move_clip(b, Lane::Video, 20.0);
        assert_eq!(p.clip(b).unwrap().start, 20.0, "dragged apart");
        p.move_clip(b, Lane::Video, 1.0);
        // Its start is before a's middle, so it goes first and pushes a along.
        assert_eq!(p.clip(b).unwrap().start, 1.0);
        assert_eq!(p.clip(a).unwrap().start, 5.0);
        p.close_gaps(Lane::Video);
        assert_eq!((p.clip(b).unwrap().start, p.clip(a).unwrap().start), (0.0, 4.0), "dragged together");
    }
    #[test]
    fn pictures_stay_on_the_picture_track() {
        let (mut p, a, _) = two();
        p.move_clip(a, Lane::Audio(0), 0.0);
        assert_eq!(p.locate(a).unwrap().0, Lane::Video);
        let s = p.add_media(media(3.0, false));
        let id = p.insert(s, Lane::Audio(1), Some(2.0));
        assert_eq!(p.audio.len(), 2, "tracks are made as needed");
        p.move_clip(id, Lane::Audio(0), 0.0);
        assert_eq!(p.locate(id).unwrap(), (Lane::Audio(0), 0));
    }
    #[test]
    fn trims_stop_at_the_source_and_the_neighbours() {
        let (mut p, a, b) = two();
        p.trim_end(a, 50.0, 10.0);
        assert_eq!(p.clip(a).unwrap().end(), 10.0, "no further than the source");
        p.trim_end(a, 6.0, 10.0);
        p.trim_start(b, 2.0);
        assert_eq!(p.clip(b).unwrap().start, 10.0, "b starts at its source's start already");
        p.trim_start(b, 11.0);
        let c = p.clip(b).unwrap();
        assert_eq!((c.start, c.src_in), (11.0, 1.0));
        p.trim_start(b, 3.0);
        assert_eq!(p.clip(b).unwrap().start, 10.0, "back as far as the source allows");
    }
    #[test]
    fn speed_changes_length_and_pushes_what_follows() {
        let (mut p, a, b) = two();
        p.set_speed(a, 0.5);
        assert_eq!(p.clip(a).unwrap().len(), 20.0);
        assert_eq!(p.clip(b).unwrap().start, 20.0);
        p.set_speed(a, 2.0);
        assert_eq!(p.clip(b).unwrap().start, 20.0, "a gap opens rather than anything moving back");
    }
    #[test]
    fn snapping_finds_the_nearest_edge() {
        assert_eq!(snap_to(9.9, &[0.0, 10.0, 14.0], 0.2), 10.0);
        assert_eq!(snap_to(9.0, &[0.0, 10.0], 0.2), 9.0);
    }
    #[test]
    fn graph_reads_only_what_is_used_and_fills_gaps() {
        let (mut p, a, b) = two();
        p.move_clip(b, Lane::Video, 12.0);
        p.set_speed(a, 2.0);
        let out = Out { w: 1280, h: 720, fps: 30.0, picture: true, sound: true };
        let g = graph(&p, 0.0, p.duration(), &out, &|_, _, _| None);
        let ins = g.inputs.join(" ");
        assert!(ins.contains("-ss 0.000000 -t 10.000000 -i m10.mp4"), "{ins}");
        assert_eq!(g.inputs.iter().filter(|x| *x == "-i").count(), 2, "sound shares the picture's input");
        assert!(g.filter.contains("setpts=(PTS-STARTPTS)/2.000000"));
        assert!(g.filter.contains("atempo=2.0000"));
        assert!(g.filter.contains("color=c=black:s=1280x720"), "the gap is black");
        assert!(g.filter.contains("concat=n=3:v=1:a=0[vout]"));
        assert_eq!((g.picture.as_deref(), g.sound.as_deref()), (Some("[vout]"), Some("[aout]")));
        // A preview from the middle of the second clip opens only that one, from the right point.
        let g = graph(&p, 13.0, p.duration(), &out, &|_, _, _| None);
        assert!(g.inputs.join(" ").contains("-ss 1.000000 -t 3.000000 -i m4.mp4"), "{:?}", g.inputs);
    }
    #[test]
    fn muted_sound_is_silence_and_tracks_mix() {
        let (mut p, a, _) = two();
        p.clip_mut(a).unwrap().muted = true;
        let s = p.add_media(media(3.0, false));
        p.insert(s, Lane::Audio(0), Some(1.0));
        let g = graph(&p, 0.0, p.duration(), &Out { w: 640, h: 360, fps: 30.0, picture: false, sound: true }, &|_, _, _| None);
        assert!(g.picture.is_none());
        assert!(g.filter.contains("amix=inputs=2"));
        assert!(!g.inputs.contains(&"m10.mp4".to_string()), "the muted clip is not read for sound: {:?}", g.inputs);
        assert!(g.filter.starts_with("anullsrc=r=48000:cl=stereo,atrim=duration=10.000000"), "it is silence instead");
    }
    /// The graph through a real ffmpeg, when there is one: every kind of edit at once has to come out as one file of
    /// the right length with picture and sound.
    #[test]
    fn renders_through_ffmpeg() {
        use std::process::Command;
        if Command::new("ffmpeg").arg("-version").output().is_err() { eprintln!("no ffmpeg, skipped"); return; }
        let dir = std::env::temp_dir().join(format!("tau-cut-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ff = |a: &[&str]| assert!(Command::new("ffmpeg").args(["-y", "-v", "error"]).args(a).status().unwrap().success(), "{a:?}");
        let v = dir.join("v.mp4");
        ff(&["-f", "lavfi", "-i", "testsrc2=s=640x360:r=25:d=4", "-f", "lavfi", "-i", "sine=f=440:d=4", "-c:v", "libx264", "-c:a", "aac", "-shortest", v.to_str().unwrap()]);
        let tall = dir.join("tall.mp4");
        ff(&["-f", "lavfi", "-i", "testsrc2=s=360x640:r=30:d=2", "-c:v", "libx264", tall.to_str().unwrap()]);
        let music = dir.join("m.wav");
        ff(&["-f", "lavfi", "-i", "sine=f=220:d=3", music.to_str().unwrap()]);
        let png = dir.join("layer.png");
        ff(&["-f", "lavfi", "-i", "color=c=red@0.5:s=640x360,format=rgba", "-frames:v", "1", png.to_str().unwrap()]);
        let vi = |w, h, fps| Some(VideoInfo { width: w, height: h, fps, pix_fmt: "yuv420p".into(), ..Default::default() });
        let mut p = Project::default();
        let a = p.add_media(Media { path: v.clone(), dur: 4.0, video: vi(640, 360, 25.0), has_audio: true, still: false });
        let b = p.add_media(Media { path: tall.clone(), dur: 2.0, video: vi(360, 640, 30.0), has_audio: false, still: false });
        let m = p.add_media(Media { path: music.clone(), dur: 3.0, video: None, has_audio: true, still: false });
        let ia = p.insert(a, Lane::Video, None);
        let ib = p.insert(b, Lane::Video, Some(5.0));
        p.insert(m, Lane::Audio(0), Some(0.5));
        p.set_speed(ib, 2.0);
        let split = p.split(&[Lane::Video], 2.0)[0];
        {
            let c = p.clip_mut(ia).unwrap();
            c.fade_in = 0.5;
            c.shapes = vec![Shape::cover_rect(0.1, 0.1, 0.4, 0.4), Shape::new(crate::canvas::Tool::Arrow, (0.5, 0.5), (0.9, 0.9), 0xFF0000)];
        }
        p.clip_mut(split).unwrap().volume = 0.5;
        let total = p.duration();
        assert_eq!(total, 6.0);
        let (w, h, fps) = natural_frame(&p);
        let e = Export::preset(3, Format::Mp4);
        let (w, h) = frame_for_height(w, h, e.height);
        let g = graph(&p, 0.0, total, &Out { w, h, fps, picture: true, sound: true }, &|_, _, _| Some(png.clone()));
        let out = dir.join("out.mp4");
        let mut args: Vec<String> = vec!["-y".into(), "-v".into(), "error".into()];
        args.extend(g.inputs.clone());
        args.extend(["-filter_complex".to_string(), g.filter.clone()]);
        args.extend(encode_args(&e, &g, fps));
        args.push(out.display().to_string());
        let r = Command::new("ffmpeg").args(&args).output().unwrap();
        assert!(r.status.success(), "{}
{}", String::from_utf8_lossy(&r.stderr), g.filter);
        let probe = Command::new("ffprobe").args(["-v", "error", "-show_entries", "format=duration:stream=codec_type,width,height", "-of", "flat", out.to_str().unwrap()]).output().unwrap();
        let info = String::from_utf8_lossy(&probe.stdout).to_string();
        assert!(info.contains("width=640") && info.contains("height=360"), "{info}");
        assert!(info.contains("codec_type=\"audio\""), "{info}");
        let dur: f64 = info.lines().find_map(|l| l.strip_prefix("format.duration=")).unwrap().trim_matches('"').parse().unwrap();
        assert!((dur - total).abs() < 0.1, "{dur} vs {total}");
        // Rendering one frame from the middle of a gap and from the middle of a clip both work.
        for t in [4.5, 5.5] {
            let g = graph(&p, t, t + 0.04, &Out { w: 320, h: 180, fps, picture: true, sound: false }, &|_, _, _| None);
            let mut a: Vec<String> = vec!["-y".into(), "-v".into(), "error".into()];
            a.extend(g.inputs.clone());
            a.extend(["-filter_complex".to_string(), g.filter.clone(), "-map".into(), "[vout]".into(), "-frames:v".into(), "1".into(), "-f".into(), "rawvideo".into(), "-pix_fmt".into(), "bgra".into(), "pipe:1".into()]);
            let r = Command::new("ffmpeg").args(&a).output().unwrap();
            assert_eq!(r.stdout.len(), 320 * 180 * 4, "{t}: {}", String::from_utf8_lossy(&r.stderr));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn presets_round_trip_and_sizes_stay_even() {
        let e = Export::preset(2, Format::Mp4);
        assert_eq!(e.matching_preset(), Some(2));
        assert_eq!(frame_for_height(1920, 1080, Some(720)), (1280, 720));
        assert_eq!(frame_for_height(1081, 1921, None), (1080, 1920));
        assert_eq!(fit_even(1080, 1920, 1280, 720), (404, 720));
    }
}
