//! Plays a clip with sound inside the app window. Windows' Media Foundation Media Engine (the
//! engine behind the old Edge <video> tag) decodes on the GPU and draws straight into a child
//! window we place over the egui layout, so frames never pass through egui.
//!
//! The engine plays only one audio track and has no per-track volume, so for the sound it stays
//! muted and a thread of ours decodes every track, mixes them with the per-track gains, and
//! plays the mix through WASAPI, following the engine's position.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BLACK_BRUSH, GetStockObject, HBRUSH};
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CLSCTX_ALL, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BSTR, Interface, PCWSTR, implement, w};

const CLASS: PCWSTR = w!("InstantReplayPlayer");
/// How long open waits for the first frame before calling the file unplayable.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
/// DXGI_FORMAT_B8G8R8A8_UNORM, the swap chain format the engine renders into.
const BGRA: u32 = 87;
/// Our sound is re-synced to the engine when they drift further apart than this, in seconds.
const MAX_DRIFT: f64 = 0.1;
/// How long a fresh start gets to settle before drift counts.
const SETTLE: Duration = Duration::from_millis(500);
/// WASAPI buffer, in 100 ns units (100 ms). Also how quickly a gain change is heard.
const BUFFER: i64 = 1_000_000;

pub struct Player {
    engine: IMFMediaEngineEx,
    hwnd: HWND,
    /// The area set_rect was last given, so unchanged frames cost nothing.
    area: (i32, i32, i32, i32),
    /// Native video size, for letterboxing.
    video: (u32, u32),
    visible: bool,
    tracks: usize,
    /// The clip's frame rate, for stepping a frame at a time.
    fps: f64,
    /// Playback speed; our sound only plays at 1.
    rate: f64,
    /// The frame frame-stepping is on; forgotten when playing or seeking.
    stepped: Option<f64>,
    /// The time a frame step last sent, when, and how many times, until the engine is there.
    pending: Cell<Option<(f64, Instant, u32)>>,
    /// Our own sound output. None for a silent clip, or when it couldn't start (no sound
    /// device); the engine then plays track 1 itself and track gains do nothing.
    audio: Option<Audio>,
}

/// Receives the engine's events on its worker threads and forwards the ones open waits for.
#[implement(IMFMediaEngineNotify)]
struct Notify {
    tx: Sender<u32>,
}

impl IMFMediaEngineNotify_Impl for Notify_Impl {
    fn EventNotify(&self, event: u32, _param1: usize, _param2: u32) -> windows::core::Result<()> {
        let _ = self.tx.send(event);
        Ok(())
    }
}

/// The UI side of our sound thread.
struct Audio {
    shared: Arc<Shared>,
    /// Some(secs): play from there. None: stop. Dropping it ends the thread.
    tx: Option<Sender<Option<f64>>>,
    thread: Option<JoinHandle<()>>,
    /// Whether the thread was last told to play, and when.
    running: Cell<bool>,
    started: Cell<Option<Instant>>,
}

/// What the UI and the sound thread share.
struct Shared {
    gains: Mutex<Vec<f32>>,
    /// Master volume, f32 bits.
    volume: AtomicU32,
    /// The clip time being heard right now, f64 bits; NaN while stopped.
    heard: AtomicU64,
}

impl Audio {
    /// Starts the sound thread for `path`, stopped. Fails if the clip can't be decoded or there's
    /// no sound device.
    fn open(path: &Path, tracks: usize) -> anyhow::Result<Audio> {
        let mut gains = vec![0.0; tracks];
        gains[0] = 1.0;
        let shared = Arc::new(Shared {
            gains: Mutex::new(gains),
            volume: AtomicU32::new(1.0f32.to_bits()),
            heard: AtomicU64::new(f64::NAN.to_bits()),
        });
        let (tx, rx) = channel();
        let (ready_tx, ready) = channel();
        let (path, s) = (path.to_path_buf(), shared.clone());
        let thread = std::thread::spawn(move || sound_thread(path, s, rx, ready_tx));
        let ok = ready.recv().unwrap_or_else(|_| Err(anyhow::anyhow!("the sound thread quit")));
        if let Err(e) = ok {
            let _ = thread.join();
            return Err(e);
        }
        Ok(Audio { shared, tx: Some(tx), thread: Some(thread), running: Cell::new(false), started: Cell::new(None) })
    }

    fn start(&self, secs: f64) {
        self.send(Some(secs));
        self.running.set(true);
        self.started.set(Some(Instant::now()));
    }

    fn stop(&self) {
        self.send(None);
        self.running.set(false);
    }

    fn send(&self, cmd: Option<f64>) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(cmd);
        }
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Decodes and plays the mix. Waits while stopped; while playing, tops up the device buffer
/// every 10 ms with the current gains.
fn sound_thread(path: PathBuf, shared: Arc<Shared>, rx: Receiver<Option<f64>>, ready: Sender<anyhow::Result<()>>) {
    let mut out = match Output::open(&path) {
        Ok(o) => {
            let _ = ready.send(Ok(()));
            o
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let mut playing = false;
    loop {
        let cmd = if playing {
            match rx.try_recv() {
                Ok(c) => Some(c),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => return,
            }
        } else {
            match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            }
        };
        let step = match cmd {
            Some(Some(at)) => {
                playing = true;
                out.restart(at, &shared)
            }
            Some(None) => {
                playing = false;
                out.stop(&shared);
                Ok(())
            }
            None => out.fill(&shared),
        };
        if let Err(e) = step {
            crate::log::write(&format!("player sound stopped: {e:#}"));
            out.stop(&shared);
            // Keep draining commands so the UI side never blocks, but play nothing.
            while rx.recv().is_ok() {}
            return;
        }
        if playing && cmd.is_none() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// The decoder plus a shared-mode WASAPI stream on the default playback device.
struct Output {
    src: crate::media::AudioSource,
    client: IAudioClient,
    render: IAudioRenderClient,
    /// Device buffer size, in frames.
    size: u32,
    /// Clip time the current run started at, and frames written since.
    start: f64,
    written: u64,
}

impl Output {
    fn open(path: &Path) -> anyhow::Result<Output> {
        // Sets up COM (multithreaded) for this thread too.
        let src = crate::media::AudioSource::open(path)?;
        unsafe {
            let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let device = en
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(|_| anyhow::anyhow!("there's no sound output device"))?;
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
            // Our mix format; AUTOCONVERTPCM has Windows convert it to the device's own.
            let format = WAVEFORMATEX {
                wFormatTag: 3, // WAVE_FORMAT_IEEE_FLOAT
                nChannels: 2,
                nSamplesPerSec: crate::media::RATE,
                nAvgBytesPerSec: crate::media::RATE * 8,
                nBlockAlign: 8,
                wBitsPerSample: 32,
                cbSize: 0,
            };
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                BUFFER,
                0,
                &format,
                None,
            )?;
            let size = client.GetBufferSize()?;
            let render: IAudioRenderClient = client.GetService()?;
            Ok(Output { src, client, render, size, start: 0.0, written: 0 })
        }
    }

    /// Drops whatever is buffered and plays from `at` seconds.
    fn restart(&mut self, at: f64, shared: &Shared) -> anyhow::Result<()> {
        self.stop(shared);
        self.src.seek(at)?;
        self.start = at;
        self.written = 0;
        self.fill(shared)?;
        unsafe { self.client.Start()? };
        Ok(())
    }

    fn stop(&mut self, shared: &Shared) {
        unsafe {
            _ = self.client.Stop();
            _ = self.client.Reset();
        }
        shared.heard.store(f64::NAN.to_bits(), Ordering::Relaxed);
    }

    /// Tops the device buffer up with freshly mixed sound. Past the end of the clip it writes
    /// nothing, which the device plays as silence.
    fn fill(&mut self, shared: &Shared) -> anyhow::Result<()> {
        let volume = f32::from_bits(shared.volume.load(Ordering::Relaxed));
        let gains: Vec<f32> = shared.gains.lock().map(|g| g.iter().map(|x| x * volume).collect()).unwrap_or_default();
        unsafe {
            let free = self.size - self.client.GetCurrentPadding()?;
            let pcm = self.src.read(free as usize, &gains)?;
            let frames = (pcm.len() / 2) as u32;
            if frames > 0 {
                let buf = self.render.GetBuffer(frames)?;
                std::ptr::copy_nonoverlapping(pcm.as_ptr(), buf as *mut f32, pcm.len());
                self.render.ReleaseBuffer(frames, 0)?;
                self.written += frames as u64;
            }
            let queued = self.client.GetCurrentPadding()? as u64;
            // Once the sound has run out there's nothing to drift; NaN keeps follow from restarting.
            let heard = if frames == 0 && queued == 0 {
                f64::NAN
            } else {
                self.start + self.written.saturating_sub(queued) as f64 / crate::media::RATE as f64
            };
            shared.heard.store(heard.to_bits(), Ordering::Relaxed);
        }
        Ok(())
    }
}

impl Player {
    /// Loads `path` paused on its first frame. Blocks until that frame is decoded (a few ms for a
    /// local file) so a broken file fails here instead of showing a black box.
    pub fn open(parent_hwnd: isize, path: &Path) -> anyhow::Result<Player> {
        anyhow::ensure!(path.is_file(), "the clip {} doesn't exist", path.display());
        init()?;
        let hwnd = child_window(parent_hwnd)?;
        match load(hwnd, path) {
            Ok((engine, video)) => {
                let tracks = crate::media::audio_tracks(path).unwrap_or(0);
                let audio = (tracks > 0)
                    .then(|| Audio::open(path, tracks))
                    .and_then(|a| a.map_err(|e| crate::log::write(&format!("player sound: {e:#}"))).ok());
                if audio.is_some() {
                    unsafe { _ = engine.SetMuted(true) };
                }
                let fps = crate::media::frame_rate(path).unwrap_or(60.0);
                Ok(Player { engine, hwnd, area: (0, 0, 0, 0), video, visible: true, tracks, fps, rate: 1.0, stepped: None, pending: Cell::new(None), audio })
            }
            Err(e) => {
                unsafe { _ = DestroyWindow(hwnd) };
                Err(e)
            }
        }
    }

    /// Places the video area, in physical pixels relative to the parent's client area. The child
    /// window covers only the letterboxed picture, so the bars show whatever egui drew there.
    pub fn set_rect(&mut self, x: i32, y: i32, w: i32, h: i32) {
        if self.area == (x, y, w, h) {
            return;
        }
        self.area = (x, y, w, h);
        let (x, y, w, h) = fit(self.area, self.video);
        unsafe {
            _ = SetWindowPos(self.hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            let dst = RECT { left: 0, top: 0, right: w, bottom: h };
            _ = self.engine.UpdateVideoStream(None, Some(&dst), None);
            // A paused engine doesn't redraw on resize; seeking in place makes it present again.
            if self.engine.IsPaused().as_bool() {
                _ = self.engine.SetCurrentTime(self.engine.GetCurrentTime());
            }
        }
    }

    /// Hides the video, for when the tab changes or the overlay hides. Doesn't pause.
    pub fn set_visible(&mut self, visible: bool) {
        if self.visible != visible {
            self.visible = visible;
            unsafe { _ = ShowWindow(self.hwnd, if visible { SW_SHOWNA } else { SW_HIDE }) };
        }
    }

    pub fn play(&mut self) {
        self.stepped = None;
        self.pending.set(None);
        unsafe {
            if self.engine.IsEnded().as_bool() {
                _ = self.engine.SetCurrentTime(0.0);
            }
            _ = self.engine.Play();
            if let Some(a) = self.audio.as_ref().filter(|_| self.rate == 1.0) {
                a.start(self.engine.GetCurrentTime());
            }
        }
    }

    /// Playback speed (1 is normal). Sound plays only at normal speed; at any other it's silent.
    pub fn set_rate(&mut self, rate: f64) {
        if self.rate == rate {
            return;
        }
        self.rate = rate;
        // Both: the current rate alone doesn't take while paused (play resets it to the default).
        unsafe {
            _ = self.engine.SetDefaultPlaybackRate(rate);
            _ = self.engine.SetPlaybackRate(rate);
        }
        // follow() starts or stops our sound to match on the next frame.
        if let Some(a) = self.audio.as_ref().filter(|a| a.running.get()) {
            a.stop();
        }
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// Pauses and shows the next frame (`forward`) or the one before. Seeks a quarter of the way
    /// into that frame (clips are recorded at a steady frame rate), counting frames itself so
    /// quick presses add up even before the last seek has landed. (The engine's own FrameStep
    /// drops a step asked for while the last one is still going, and can't step back.)
    /// While playing, the first press only pauses, on the frame showing: the engine runs on a
    /// frame or so after Pause, and a seek sent during that is lost.
    pub fn step(&mut self, forward: bool) {
        if self.engine_playing() {
            self.pause();
            return;
        }
        // Paused from playback, the time can sit late in a frame while the engine already holds
        // the next one (a seek into it is then a no-op), so a fresh step starts from the nearest.
        let here = self.stepped.unwrap_or_else(|| (self.position() * self.fps).round());
        let frame = (here + if forward { 1.0 } else { -1.0 }).max(0.0);
        let at = (frame + 0.25) / self.fps;
        if self.duration().is_none_or(|d| at < d) {
            unsafe { _ = self.engine.SetCurrentTime(at) };
            self.stepped = Some(frame);
            self.pending.set(Some((at, Instant::now(), 0)));
        }
    }

    /// For a moment after a pause the engine ignores seeks, and once it has ignored one it
    /// treats the same time as done. So while a frame step hasn't landed, this sends it again
    /// every 40 ms, nudged to another point in the same frame, for up to half a second (the
    /// engine takes 100 to 150 ms). Runs with position(), every frame while stepping() is true.
    fn settle(&self) {
        let Some((at, sent, tries)) = self.pending.get() else { return };
        // It can report the new time and then snap back, so it has to hold there a while.
        let there = unsafe { (self.engine.GetCurrentTime() - at).abs() < 1e-4 };
        if (there && sent.elapsed() >= Duration::from_millis(150)) || tries >= 12 {
            self.pending.set(None);
        } else if !there && sent.elapsed() >= Duration::from_millis(40) {
            let frame = (at * self.fps).floor();
            let next = (frame + 0.25 + 0.05 * ((tries + 1) % 8) as f64) / self.fps;
            unsafe { _ = self.engine.SetCurrentTime(next) };
            self.pending.set(Some((next, Instant::now(), tries + 1)));
        }
    }

    /// A frame step hasn't landed yet; the page should keep repainting until it has.
    pub fn stepping(&self) -> bool {
        self.pending.get().is_some()
    }

    pub fn pause(&mut self) {
        unsafe { _ = self.engine.Pause() };
        if let Some(a) = &self.audio {
            a.stop();
        }
    }

    pub fn is_playing(&self) -> bool {
        let playing = self.engine_playing();
        self.follow(playing);
        playing
    }

    fn engine_playing(&self) -> bool {
        unsafe { !self.engine.IsPaused().as_bool() && !self.engine.IsEnded().as_bool() }
    }

    /// Jumps to `secs`. Takes effect a moment later; position() reports the target meanwhile.
    pub fn seek(&mut self, secs: f64) {
        self.stepped = None;
        self.pending.set(None);
        let secs = self.duration().map_or(secs, |d| secs.min(d)).max(0.0);
        unsafe { _ = self.engine.SetCurrentTime(secs) };
        if let Some(a) = self.audio.as_ref().filter(|a| a.running.get()) {
            a.start(secs);
        }
    }

    pub fn position(&self) -> f64 {
        self.follow(self.engine_playing());
        self.settle();
        unsafe { self.engine.GetCurrentTime() }
    }

    /// Per-track volumes (0 to 2, by audio track order) for our sound; takes effect within
    /// about 100 ms. Tracks without a gain are silent. The default plays only track 1, the full
    /// mix in OBS's split setup, so it sounds the same as before.
    pub fn set_track_gains(&mut self, gains: &[f32]) {
        if let Some(a) = &self.audio
            && let Ok(mut g) = a.shared.gains.lock()
        {
            *g = gains.iter().map(|x| x.clamp(0.0, 2.0)).collect();
        }
    }

    /// Number of audio tracks in the clip.
    pub fn audio_tracks(&self) -> usize {
        self.tracks
    }

    /// Keeps our sound in step with the engine: stops it when the engine stopped on its own
    /// (the clip ended), and restarts it at the engine's position if they drifted apart. Runs
    /// whenever the UI asks for the position or play state, which it does every frame.
    fn follow(&self, playing: bool) {
        let Some(a) = &self.audio else { return };
        if !playing || self.rate != 1.0 {
            if a.running.get() {
                a.stop();
            }
            return;
        }
        let pos = unsafe { self.engine.GetCurrentTime() };
        let heard = f64::from_bits(a.shared.heard.load(Ordering::Relaxed));
        let settled = a.started.get().is_some_and(|t| t.elapsed() > SETTLE);
        if !a.running.get() || (settled && !heard.is_nan() && (heard - pos).abs() > MAX_DRIFT) {
            a.start(pos);
        }
    }

    /// The clip's own width and height in pixels; (0, 0) if unknown.
    pub fn video_size(&self) -> (u32, u32) {
        self.video
    }

    /// Clip length in seconds; None until the engine knows it.
    pub fn duration(&self) -> Option<f64> {
        let d = unsafe { self.engine.GetDuration() };
        (d.is_finite() && d > 0.0).then_some(d)
    }

    /// Volume from 0 to 1.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_volume(&mut self, v: f32) {
        let v = v.clamp(0.0, 1.0);
        unsafe { _ = self.engine.SetVolume(v as f64) };
        if let Some(a) = &self.audio {
            a.shared.volume.store(v.to_bits(), Ordering::Relaxed);
        }
    }

    pub fn ended(&self) -> bool {
        unsafe { self.engine.IsEnded().as_bool() }
    }
}

impl Drop for Player {
    /// Shutdown stops playback and closes the file, so it can be moved or deleted right after.
    fn drop(&mut self) {
        // Joins our sound thread, which closes its own handle on the file.
        self.audio = None;
        unsafe {
            _ = self.engine.Shutdown();
            _ = DestroyWindow(self.hwnd);
        }
    }
}

/// COM and Media Foundation for this thread. COM may already be set up on the UI thread, in
/// either mode; the engine works in both, so RPC_E_CHANGED_MODE is fine to ignore.
fn init() -> anyhow::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
    }
    Ok(())
}

/// Creates the engine on `hwnd`, starts loading `path`, and waits for the first frame.
fn load(hwnd: HWND, path: &Path) -> anyhow::Result<(IMFMediaEngineEx, (u32, u32))> {
    let (tx, rx) = channel();
    let notify: IMFMediaEngineNotify = Notify { tx }.into();
    unsafe {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 3)?;
        let attrs = attrs.ok_or_else(|| anyhow::anyhow!("no attributes"))?;
        attrs.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify)?;
        attrs.SetUINT64(&MF_MEDIA_ENGINE_PLAYBACK_HWND, hwnd.0 as u64)?;
        attrs.SetUINT32(&MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, BGRA)?;
        let factory: IMFMediaEngineClassFactory = CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)?;
        let engine: IMFMediaEngineEx = factory.CreateInstance(0, &attrs)?.cast()?;
        let ready = engine
            .SetPreload(MF_MEDIA_ENGINE_PRELOAD_AUTOMATIC)
            .and_then(|_| engine.SetSource(&BSTR::from(path.as_os_str().to_string_lossy().as_ref())))
            .map_err(anyhow::Error::from)
            .and_then(|_| wait_first_frame(&engine, &rx));
        if let Err(e) = ready {
            _ = engine.Shutdown();
            return Err(e);
        }
        let (mut w, mut h) = (0u32, 0u32);
        _ = engine.GetNativeVideoSize(Some(&mut w), Some(&mut h));
        Ok((engine, (w, h)))
    }
}

fn wait_first_frame(engine: &IMFMediaEngineEx, rx: &Receiver<u32>) -> anyhow::Result<()> {
    let deadline = Instant::now() + OPEN_TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let event = rx.recv_timeout(left).map_err(|_| anyhow::anyhow!("the clip took too long to open"))?;
        if event == MF_MEDIA_ENGINE_EVENT_LOADEDDATA.0 as u32 {
            return Ok(());
        }
        if event == MF_MEDIA_ENGINE_EVENT_ERROR.0 as u32 {
            let code = unsafe { engine.GetError().and_then(|e| e.GetExtendedErrorCode()) };
            let why = code.err().map(|e| e.message()).unwrap_or_default();
            anyhow::bail!("couldn't play this clip. {why}");
        }
    }
}

/// A black child window of `parent` that the engine draws into.
fn child_window(parent: isize) -> anyhow::Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?.into();
        static REGISTERED: OnceLock<()> = OnceLock::new();
        REGISTERED.get_or_init(|| {
            let class = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassW(&class);
        });
        let hwnd = CreateWindowExW(
            WS_EX_NOPARENTNOTIFY,
            CLASS,
            None,
            WS_CHILD | WS_CLIPSIBLINGS | WS_VISIBLE,
            0,
            0,
            0,
            0,
            Some(HWND(parent as _)),
            None,
            Some(instance),
            None,
        )?;
        Ok(hwnd)
    }
}

/// Lets mouse input fall through to the parent, so egui still gets clicks on the video.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCHITTEST {
        return LRESULT(HTTRANSPARENT as isize);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// The largest rect with the video's aspect ratio, centered in `area`.
fn fit((x, y, w, h): (i32, i32, i32, i32), (vw, vh): (u32, u32)) -> (i32, i32, i32, i32) {
    if vw == 0 || vh == 0 || w <= 0 || h <= 0 {
        return (x, y, w.max(0), h.max(0));
    }
    let scale = (w as f64 / vw as f64).min(h as f64 / vh as f64);
    let (fw, fh) = ((vw as f64 * scale).round() as i32, (vh as f64 * scale).round() as i32);
    (x + (w - fw) / 2, y + (h - fh) / 2, fw, fh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letterboxes() {
        assert_eq!(fit((0, 0, 1000, 1000), (1920, 1080)), (0, 218, 1000, 563));
        assert_eq!(fit((10, 0, 400, 100), (1920, 1080)), (121, 0, 178, 100));
    }

    #[test]
    fn rejects_a_broken_file() {
        let bad = std::env::temp_dir().join("instant-replay-player-bad.mp4");
        std::fs::write(&bad, b"not a video").unwrap();
        let parent = hidden_window();
        assert!(Player::open(parent.0 as isize, &bad).is_err());
        std::fs::remove_file(&bad).unwrap();
        unsafe { _ = DestroyWindow(parent) };
    }

    /// Plays the smallest real clip's video with three tone tracks (from a copy); skipped if
    /// there's no clip.
    #[test]
    fn plays_a_real_clip_and_lets_go_of_it() {
        let Some(copy) = crate::media::tests::tone_clip("instant-replay-player-test.mp4") else { return };
        let expected = crate::media::duration(&copy).unwrap();
        let parent = hidden_window();

        let mut p = Player::open(parent.0 as isize, &copy).unwrap();
        // Silent: our sound plays, but every track at 0 and the master volume at 0.
        assert_eq!(p.audio_tracks(), 3);
        p.set_track_gains(&vec![0.0; p.audio_tracks()]);
        p.set_volume(0.0);
        p.set_rect(0, 0, 640, 480);
        let d = p.duration().unwrap();
        assert!((d - expected).abs() < 0.5, "duration {d}, expected {expected}");
        assert!(!p.is_playing() && p.position() < 0.1);
        p.play();
        std::thread::sleep(Duration::from_millis(1200));
        let pos = p.position();
        assert!(p.is_playing() && (0.5..2.0).contains(&pos), "position {pos} after 1.2 s");
        if let Some(a) = &p.audio {
            let heard = f64::from_bits(a.shared.heard.load(Ordering::Relaxed));
            assert!((heard - pos).abs() < 0.25, "our sound is at {heard}, the video at {pos}");
        } else {
            assert_eq!(p.audio_tracks(), 0, "our sound didn't start (see app.log)");
        }
        p.seek(1.0);
        std::thread::sleep(Duration::from_millis(300));
        let pos = p.position();
        if let Some(a) = &p.audio {
            let heard = f64::from_bits(a.shared.heard.load(Ordering::Relaxed));
            assert!((heard - pos).abs() < 0.25, "after seeking, our sound is at {heard}, the video at {pos}");
        }
        p.pause();
        assert!(!p.is_playing());
        std::thread::sleep(Duration::from_millis(100));
        assert!(p.audio.as_ref().is_none_or(|a| f64::from_bits(a.shared.heard.load(Ordering::Relaxed)).is_nan()));

        // Double speed: the video runs about twice as fast and our sound stays off.
        let from = p.position();
        p.set_rate(2.0);
        p.play();
        std::thread::sleep(Duration::from_millis(1000));
        let ran = p.position() - from;
        assert!((1.5..2.6).contains(&ran), "moved {ran} s in 1 s at 2x");
        assert!(p.audio.as_ref().is_none_or(|a| !a.running.get()), "sound plays at 2x");
        p.set_rate(1.0);
        p.pause();

        // A frame on and back, at the clip's own frame rate.
        let fps = crate::media::frame_rate(&copy).unwrap();
        assert!((10.0..=500.0).contains(&fps), "frame rate {fps}");
        // Positions settle a moment after a pause or seek, as they have between keypresses.
        // Like the page: window messages handled and position() asked every frame (~60 a
        // second) for 200 ms, and on while a frame step is still landing, as the page repaints.
        let settled = |p: &Player| {
            for i in 0..60 {
                pump(16);
                p.position();
                if i >= 12 && !p.stepping() {
                    break;
                }
            }
            p.position()
        };
        let frame_of = |t: f64| (t * fps).floor();
        // While playing, the first press just pauses where it is.
        p.play();
        pump(300);
        p.step(true);
        let paused_at = (settled(&p) * fps).round();
        assert!(!p.is_playing() && p.stepped.is_none(), "the first press from playing only pauses");
        // From a paused time late in a frame, the next step still moves.
        p.step(true);
        let mut expect = frame_of(settled(&p));
        assert_eq!(expect, paused_at + 1.0, "first step after the pause");
        // Each step waited for, then quick pairs (the second before the first has landed).
        for (keys, wait) in [(&[true][..], true), (&[true], true), (&[false], true), (&[true], true), (&[true, true], false), (&[false, true], false), (&[false, false], false), (&[true, false], false)] {
            for &on in keys {
                p.step(on);
                expect += if on { 1.0 } else { -1.0 };
                if wait {
                    settled(&p);
                }
            }
            let at = settled(&p);
            assert!(!p.is_playing() && frame_of(at) == expect, "after {keys:?}: at {at} = frame {}, expected {expect}", frame_of(at));
        }
        drop(p);

        let moved = copy.with_extension("moved.mp4");
        std::fs::rename(&copy, &moved).expect("the clip is still locked after drop");
        std::fs::remove_file(&moved).unwrap();
        unsafe { _ = DestroyWindow(parent) };
    }

    /// Handles this thread's window messages for `ms`, as the app's own window loop does.
    fn pump(ms: u64) {
        let until = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < until {
            let mut msg = MSG::default();
            while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
                unsafe {
                    _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// A top-level window that is never shown.
    fn hidden_window() -> HWND {
        unsafe {
            let instance = GetModuleHandleW(None).unwrap();
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                None,
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                800,
                600,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .unwrap()
        }
    }
}
