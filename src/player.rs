//! Plays a video in a viewport. One ffmpeg decodes from a time, scaled down and at a steady rate, and its frames are
//! handed over as they fall due, so playing costs one process rather than one per frame. Picture only: the sound is
//! not played.
use crate::image::Bmp;
use std::collections::VecDeque;
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Longest side of a played frame. Larger would only move more bytes through the pipe for a picture shown smaller.
const MAX_SIDE: u32 = 1280;
/// Played no faster than this. The viewport redraws at 60 a second at best, and a picture is enough to judge by.
const MAX_FPS: f64 = 30.0;
/// Frames decoded ahead of the one on screen.
const AHEAD: usize = 6;

/// What a viewport needs to play its video.
#[derive(Clone)]
pub struct Source {
    pub path: PathBuf,
    /// A filter that makes a frame look right as a picture, such as deinterlacing.
    pub filter: Option<String>,
    pub fps: f64,
}

struct Shared {
    frames: Mutex<VecDeque<Bmp>>,
    ended: AtomicBool,
    stop: AtomicBool,
}

pub struct Player {
    shared: Arc<Shared>,
    child: Arc<Mutex<Option<Child>>>,
    from: f64,
    end: f64,
    /// Frames per second of the video's own time that are decoded.
    rate: f64,
    /// How much faster than real time the video's own time goes by.
    speed: f64,
    /// Starts with the first frame, so the time ffmpeg takes to start does not count as playing.
    clock: Option<Instant>,
    /// Frames taken off the queue.
    taken: u64,
}

/// The rate frames are played at.
fn play_rate(fps: f64) -> f64 { if fps > 0.0 && fps <= 240.0 { fps.min(MAX_FPS) } else { 25.0 } }

/// Frames per second of the video's own time to decode at a speed. Fast, only every few are needed to show
/// `MAX_FPS` a second; slow, every frame there is, each shown for longer.
fn sample_rate(fps: f64, speed: f64) -> f64 {
    let own = if fps > 0.0 && fps <= 240.0 { fps } else { 25.0 };
    own.min(MAX_FPS / speed.max(0.01))
}

/// A size with the same shape as `(w, h)`, no longer than `MAX_SIDE`.
fn fit(w: u32, h: u32) -> (u32, u32) {
    let (w, h) = (w.max(1), h.max(1));
    let long = w.max(h);
    if long <= MAX_SIDE { return (w, h); }
    let f = MAX_SIDE as f64 / long as f64;
    (((w as f64 * f).round() as u32).max(1), ((h as f64 * f).round() as u32).max(1))
}

fn args(src: &Source, from: f64, end: f64, rate: f64, (w, h): (u32, u32)) -> Vec<String> {
    let mut vf: Vec<String> = src.filter.iter().cloned().collect();
    vf.push(format!("fps={rate}"));
    vf.push(format!("scale={w}:{h}:flags=bilinear"));
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin"].iter().map(|s| s.to_string()).collect();
    if from > 0.0 { a.extend(["-ss".to_string(), format!("{from:.3}")]); }
    a.extend(["-i".to_string(), src.path.display().to_string()]);
    if end > from { a.extend(["-t".to_string(), format!("{:.3}", end - from)]); }
    a.extend(["-map", "0:v:0", "-an", "-sn", "-dn", "-vf"].iter().map(|s| s.to_string()));
    a.push(vf.join(","));
    a.extend(["-pix_fmt", "bgra", "-f", "rawvideo", "pipe:1"].iter().map(|s| s.to_string()));
    a
}

impl Player {
    /// Plays from `from` until `end`, at about the size of `size`.
    pub fn start(src: &Source, from: f64, end: f64, size: (u32, u32)) -> Player { Player::start_at_speed(src, from, end, size, 1.0) }

    /// The same, sped up or slowed down by `speed`.
    pub fn start_at_speed(src: &Source, from: f64, end: f64, size: (u32, u32), speed: f64) -> Player {
        let speed = if speed > 0.0 { speed } else { 1.0 };
        let rate = sample_rate(src.fps, speed);
        let shared = Arc::new(Shared { frames: Mutex::new(VecDeque::new()), ended: AtomicBool::new(false), stop: AtomicBool::new(false) });
        let child = Arc::new(Mutex::new(None));
        let (w, h) = fit(size.0, size.1);
        let spawned = crate::ffmpeg().ok_or(()).and_then(|ff| {
            Command::new(ff).args(args(src, from, end, rate, (w, h)))
                .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null())
                .creation_flags(crate::job::NO_WINDOW).spawn().map_err(|_| ())
        });
        match spawned {
            Ok(mut c) => {
                let out = c.stdout.take();
                *child.lock().unwrap() = Some(c);
                let s = shared.clone();
                std::thread::spawn(move || {
                    if let Some(mut out) = out {
                        let len = (w * h * 4) as usize;
                        loop {
                            let mut px = vec![0u8; len];
                            if out.read_exact(&mut px).is_err() { break; }
                            // Decoding waits while enough is in hand; the pipe then holds ffmpeg back too.
                            while s.frames.lock().unwrap().len() >= AHEAD && !s.stop.load(Ordering::Relaxed) {
                                std::thread::sleep(std::time::Duration::from_millis(4));
                            }
                            if s.stop.load(Ordering::Relaxed) { break; }
                            s.frames.lock().unwrap().push_back(Bmp { w, h, px });
                        }
                    }
                    s.ended.store(true, Ordering::SeqCst);
                });
            }
            Err(_) => shared.ended.store(true, Ordering::SeqCst),
        }
        Player { shared, child, from, end, rate, speed, clock: None, taken: 0 }
    }

    /// The newest frame that is due, if one is. Frames the window was too busy to show are skipped.
    pub fn tick(&mut self) -> Option<Bmp> {
        let mut q = self.shared.frames.lock().unwrap();
        let clock = match self.clock {
            Some(c) => c,
            None => {
                let first = q.pop_front()?;
                self.clock = Some(Instant::now());
                self.taken = 1;
                return Some(first);
            }
        };
        let pace = self.rate * self.speed;
        let due = (clock.elapsed().as_secs_f64() * pace) as u64 + 1;
        let mut last = None;
        while self.taken < due {
            let Some(f) = q.pop_front() else { break };
            self.taken += 1;
            last = Some(f);
        }
        // Decoding fell behind: the clock waits for it, so playing slows down rather than jumping ahead.
        if self.taken < due && q.is_empty() && !self.shared.ended.load(Ordering::Relaxed) {
            self.clock = Some(Instant::now() - std::time::Duration::from_secs_f64(self.taken.saturating_sub(1) as f64 / pace));
        }
        last
    }

    /// The time of the frame on screen.
    pub fn time(&self) -> f64 {
        let t = self.from + self.taken.saturating_sub(1) as f64 / self.rate;
        if self.end > self.from { t.min(self.end) } else { t }
    }

    /// True once every frame has been shown.
    pub fn ended(&self) -> bool { self.shared.ended.load(Ordering::SeqCst) && self.shared.frames.lock().unwrap().is_empty() }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            std::thread::spawn(move || { let _ = c.wait(); });
        }
    }
}

/// Where playing starts: from `t`, unless that is at the end, when it goes back to `start`.
pub fn start_from(t: f64, start: f64, end: f64, fps: f64) -> f64 {
    let frame = 1.0 / play_rate(fps);
    if t < start || t >= end - frame * 2.0 { start } else { t }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn played_frames_keep_their_shape_and_stay_small() {
        assert_eq!(fit(3840, 2160), (1280, 720));
        assert_eq!(fit(1080, 1920), (720, 1280));
        assert_eq!(fit(640, 360), (640, 360));
    }
    #[test]
    fn rate_is_capped_and_never_zero() {
        assert_eq!(play_rate(60.0), 30.0);
        assert_eq!(play_rate(24.0), 24.0);
        assert_eq!(play_rate(0.0), 25.0);
    }
    #[test]
    fn fast_playing_decodes_fewer_frames() {
        assert_eq!(sample_rate(30.0, 1.0), 30.0);
        assert_eq!(sample_rate(30.0, 4.0), 7.5);
        assert_eq!(sample_rate(60.0, 0.25), 60.0);
    }
    #[test]
    fn playing_at_the_end_starts_over() {
        assert_eq!(start_from(9.99, 0.0, 10.0, 30.0), 0.0);
        assert_eq!(start_from(4.0, 0.0, 10.0, 30.0), 4.0);
        assert_eq!(start_from(1.0, 2.0, 10.0, 30.0), 2.0);
    }
    #[test]
    fn decodes_from_the_time_asked() {
        let s = Source { path: "a.mp4".into(), filter: Some("bwdif".into()), fps: 25.0 };
        let a = args(&s, 2.5, 4.0, 25.0, (640, 360)).join(" ");
        assert!(a.contains("-ss 2.500 -i a.mp4 -t 1.500"));
        assert!(a.contains("bwdif,fps=25,scale=640:360"));
        assert!(a.ends_with("-pix_fmt bgra -f rawvideo pipe:1"));
    }
}
