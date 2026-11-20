//! Plays sound for the editor's preview. One ffmpeg mixes the edit from a time to 16-bit stereo PCM on its output, and
//! the samples go to the default device through waveOut, which ships with every Windows. The samples played so far are
//! the clock the picture keeps time with.
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::{MMTIME, TIME_SAMPLES};

pub const RATE: u32 = 48000;
const CHANNELS: u16 = 2;
const FRAME_BYTES: usize = 4;
/// A tenth of a second a buffer, four buffers queued: enough to ride out a busy moment without lagging the picture.
const CHUNK: usize = RATE as usize / 10 * FRAME_BYTES;
const BUFFERS: usize = 4;

struct Shared {
    stop: AtomicBool,
    ended: AtomicBool,
    /// Sample frames the device has played, once it has started.
    played: AtomicU64,
    started: AtomicBool,
}

pub struct Out {
    shared: Arc<Shared>,
    child: Arc<Mutex<Option<Child>>>,
}

/// The arguments that make ffmpeg write what this module plays, after the inputs and filters.
pub fn pcm_args() -> Vec<String> {
    ["-f", "s16le", "-acodec", "pcm_s16le", "-ar", "48000", "-ac", "2", "pipe:1"].iter().map(|s| s.to_string()).collect()
}

impl Out {
    /// Starts ffmpeg with `args`, which must end in `pcm_args`, and plays what it writes.
    pub fn start(args: Vec<String>) -> Option<Out> {
        let ff = crate::ffmpeg()?;
        let mut c = Command::new(ff).args(["-hide_banner", "-loglevel", "error", "-nostdin"]).args(&args)
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null())
            .creation_flags(crate::job::NO_WINDOW).spawn().ok()?;
        let out = c.stdout.take()?;
        let shared = Arc::new(Shared { stop: AtomicBool::new(false), ended: AtomicBool::new(false), played: AtomicU64::new(0), started: AtomicBool::new(false) });
        let s = shared.clone();
        std::thread::spawn(move || {
            play(out, &s);
            s.ended.store(true, Ordering::SeqCst);
        });
        Some(Out { shared, child: Arc::new(Mutex::new(Some(c))) })
    }

    /// Seconds of sound played so far, or None until the first has been heard.
    pub fn position(&self) -> Option<f64> {
        self.shared.started.load(Ordering::SeqCst).then(|| self.shared.played.load(Ordering::SeqCst) as f64 / RATE as f64)
    }

    pub fn ended(&self) -> bool { self.shared.ended.load(Ordering::SeqCst) }
}

impl Drop for Out {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            std::thread::spawn(move || { let _ = c.wait(); });
        }
    }
}

/// Feeds the device until the pipe runs dry or playing is stopped.
fn play(mut pipe: impl Read, s: &Shared) {
    unsafe {
        let fmt = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16, nChannels: CHANNELS, nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * FRAME_BYTES as u32, nBlockAlign: FRAME_BYTES as u16, wBitsPerSample: 16, cbSize: 0,
        };
        let mut h = HWAVEOUT::default();
        if waveOutOpen(Some(&mut h), WAVE_MAPPER, &fmt, None, None, CALLBACK_NULL) != 0 { return; }
        let mut bufs: Vec<Vec<u8>> = (0..BUFFERS).map(|_| vec![0u8; CHUNK]).collect();
        let mut hdrs: Vec<WAVEHDR> = (0..BUFFERS).map(|_| std::mem::zeroed()).collect();
        let size = std::mem::size_of::<WAVEHDR>() as u32;
        let free = |hd: &WAVEHDR| hd.dwFlags & WHDR_PREPARED == 0 || hd.dwFlags & WHDR_DONE != 0;
        let mut next = 0;
        let mut eof = false;
        while !s.stop.load(Ordering::Relaxed) {
            // The position runs on while the device plays; reading it here keeps the clock fresh between chunks too.
            let mut t = MMTIME { wType: TIME_SAMPLES, ..Default::default() };
            if waveOutGetPosition(h, &mut t, std::mem::size_of::<MMTIME>() as u32) == 0 && t.wType == TIME_SAMPLES {
                s.played.store(t.u.sample as u64, Ordering::SeqCst);
            }
            if eof {
                if hdrs.iter().all(free) { break; }
                std::thread::sleep(std::time::Duration::from_millis(5));
                continue;
            }
            if !free(&hdrs[next]) { std::thread::sleep(std::time::Duration::from_millis(3)); continue; }
            if hdrs[next].dwFlags & WHDR_PREPARED != 0 { waveOutUnprepareHeader(h, &mut hdrs[next], size); }
            let mut got = 0;
            while got < CHUNK {
                match pipe.read(&mut bufs[next][got..]) { Ok(0) | Err(_) => break, Ok(n) => got += n }
            }
            got -= got % FRAME_BYTES;
            if got == 0 { eof = true; continue; }
            hdrs[next] = std::mem::zeroed();
            hdrs[next].lpData = windows::core::PSTR(bufs[next].as_mut_ptr());
            hdrs[next].dwBufferLength = got as u32;
            waveOutPrepareHeader(h, &mut hdrs[next], size);
            waveOutWrite(h, &mut hdrs[next], size);
            s.started.store(true, Ordering::SeqCst);
            if got < CHUNK { eof = true; }
            next = (next + 1) % BUFFERS;
        }
        waveOutReset(h);
        for hd in &mut hdrs { if hd.dwFlags & WHDR_PREPARED != 0 { waveOutUnprepareHeader(h, hd, size); } }
        waveOutClose(h);
    }
}
