//! Background worker that runs the hidden recorder OBS and talks to it over its WebSocket.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use obws::Client;

use crate::config::Config;
use crate::{clips, display, log, media, perf, recorder, toast};

/// Whether to show the "Clip saved" popup; the UI keeps it in sync with the setting.
pub static SAVE_POPUP: AtomicBool = AtomicBool::new(true);
/// Sort saved clips and screenshots into a folder per game; play a soft sound on save.
pub static SORT_BY_GAME: AtomicBool = AtomicBool::new(false);
pub static SAVE_SOUND: AtomicBool = AtomicBool::new(false);

/// Clips OBS has reported saved, to spot a save request that never lands.
static SAVED: AtomicU32 = AtomicU32::new(0);

/// A "save only the last N seconds" request, cut down once OBS reports the clip saved.
static SHORTEN: Mutex<Option<(u32, Instant)>> = Mutex::new(None);

/// The clips folder of the running config; saved-clip paths from the recorder must be inside it.
static FOLDER: Mutex<String> = Mutex::new(String::new());

fn clips_folder() -> std::path::PathBuf {
    std::path::PathBuf::from(FOLDER.lock().unwrap().clone())
}

/// Where a new clip or screenshot belongs: the game's folder when sorting, else the clips folder.
fn destination() -> std::path::PathBuf {
    let root = clips_folder();
    if SORT_BY_GAME.load(Ordering::Relaxed) { root.join(clips::game_folder_name()) } else { root }
}

/// Moves a just-saved file to where it belongs (a replay saved during a full recording lands in
/// the Recordings folder at first). Keeps it where it is if the move fails.
fn place(path: &std::path::Path) -> std::path::PathBuf {
    match clips::move_into(path, &destination()) {
        Ok(p) => p,
        Err(e) => {
            log::write(&format!("Couldn't move {} into its folder: {e}", path.display()));
            path.to_path_buf()
        }
    }
}

fn feedback(title: &str, detail: String, clip: Option<&std::path::Path>) {
    if SAVE_SOUND.load(Ordering::Relaxed) {
        toast::chime();
    }
    if SAVE_POPUP.load(Ordering::Relaxed) {
        match clip {
            Some(clip) => toast::show_clip(title, detail, clip),
            None => toast::show(title, detail),
        }
    }
}

/// Whether `path` is a video file inside the clips folder. The path comes over the WebSocket, so
/// anything else (another program on the port, a confused recorder) is never cut or moved.
fn is_our_clip(path: &std::path::Path) -> bool {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let folder = FOLDER.lock().unwrap().clone();
    match (std::fs::canonicalize(path), std::fs::canonicalize(&folder)) {
        (Ok(p), Ok(f)) => matches!(ext.as_str(), "mp4" | "mkv" | "mov") && p.starts_with(f),
        _ => false,
    }
}

/// Below this much free space on the clips drive, warn (OBS fails quietly on a full disk).
const LOW_DISK: u64 = 5 * 1024 * 1024 * 1024;

/// Frame counters from OBS's stats; they restart from zero whenever the recorder restarts.
#[derive(Default, Clone, Copy)]
struct Frames {
    rendered: u64,
    render_lagged: u64,
    encoded: u64,
    encode_skipped: u64,
}

impl Frames {
    fn add(self, o: Frames) -> Frames {
        Frames {
            rendered: self.rendered + o.rendered,
            render_lagged: self.render_lagged + o.render_lagged,
            encoded: self.encoded + o.encoded,
            encode_skipped: self.encode_skipped + o.encode_skipped,
        }
    }

    /// The encoder can't keep up: at least half a second of frames, and 1% or more, skipped.
    fn encoder_struggling(self) -> bool {
        self.encode_skipped >= 30 && self.encode_skipped * 100 >= self.encoded
    }
}

/// One stretch of recording, from the replay buffer going on to it going off or the app closing.
struct Session {
    started: Instant,
    clips: u32,
    restarts: u32,
    errors: u32,
    /// Frames from recorder runs that have ended, plus the live run.
    done: Frames,
    live: Frames,
    /// Encoder efforts (bit per level) already warned about for skipped frames.
    skip_warned: u8,
    /// The recorder's CPU, GPU and video encoder use, sampled every few seconds.
    load: perf::Load,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);

fn begin_session() {
    let mut s = SESSION.lock().unwrap();
    if s.is_none() {
        *s = Some(Session { started: Instant::now(), clips: 0, restarts: 0, errors: 0, done: Frames::default(), live: Frames::default(), skip_warned: 0, load: perf::Load::default() });
        drop(s);
        log::write("Session started.");
    }
}

fn with_session(f: impl FnOnce(&mut Session)) {
    if let Some(s) = SESSION.lock().unwrap().as_mut() {
        f(s);
    }
}

/// The recorder restarted: its frame counters start over.
fn recorder_restarted(s: &mut Session) {
    s.done = s.done.add(s.live);
    s.live = Frames::default();
}

fn summary(s: &Session) -> String {
    let f = s.done.add(s.live);
    let pct = |part: u64, all: u64| if all == 0 { 0.0 } else { part as f64 * 100.0 / all as f64 };
    let (lag, skip) = (pct(f.render_lagged, f.rendered), pct(f.encode_skipped, f.encoded));
    let mins = s.started.elapsed().as_secs() / 60;
    let ok = s.restarts == 0 && s.errors == 0 && lag < 1.0 && skip < 1.0;
    format!(
        "{}h {:02}m, clips saved: {}, recorder restarts: {}, errors: {}, render lag {lag:.2}% ({} of {} frames), \
         encoder skipped {skip:.2}% ({} of {} frames), recorder load {}. Health: {}",
        mins / 60,
        mins % 60,
        s.clips,
        s.restarts,
        s.errors,
        f.render_lagged,
        f.rendered,
        f.encode_skipped,
        f.encoded,
        s.load.summary(),
        if ok { "OK" } else { "CHECK (see the lines above and the OBS log)" },
    )
}

/// Writes the end-of-session health report to the app log.
pub fn end_session(reason: &str) {
    let Some(s) = SESSION.lock().unwrap().take() else { return };
    log::write(&format!("SESSION REPORT ({reason}): {}", summary(&s)));
    if let Some(p) = recorder::obs_log() {
        log::write(&format!("OBS log for this session: {}", p.display()));
    }
}

/// How long the mouse stays on another monitor before the capture moves there.
const FOLLOW_DELAY: Duration = Duration::from_secs(2);

/// Follow the mouse: the monitor the capture shows, and another one the pointer has been on
/// since when.
struct Follow {
    showing: String,
    since: Option<(String, Instant)>,
}

impl Follow {
    fn new(showing: String) -> Self {
        Follow { showing, since: None }
    }

    /// Given the monitor under the pointer now, the monitor to move the capture to, once the
    /// pointer has stayed on it for FOLLOW_DELAY. Off every monitor (a gap) changes nothing.
    fn step(&mut self, under: Option<&str>, now: Instant) -> Option<String> {
        let under = under?;
        if under == self.showing {
            self.since = None;
            return None;
        }
        match &self.since {
            Some((id, at)) if id == under => {
                if now.duration_since(*at) < FOLLOW_DELAY {
                    return None;
                }
                self.showing = under.to_string();
                self.since = None;
                Some(self.showing.clone())
            }
            _ => {
                self.since = Some((under.to_string(), now));
                None
            }
        }
    }
}

pub type Repaint = Arc<dyn Fn() + Send + Sync>;

pub enum Cmd {
    /// Write the recorder config and (re)start the hidden OBS.
    Start(Config),
    /// Save the replay into the clips folder.
    Save,
    /// Save, then keep only the last this many seconds.
    SaveLast(u32),
    /// Turn instant replay on or off.
    SetReplay(bool),
    /// Flip instant replay, start/stop a full recording, mute/unmute the mic (hotkeys).
    ToggleReplay,
    ToggleRecord,
    ToggleMic,
    /// Save a PNG of the screen next to the clips.
    Screenshot,
}

pub enum Event {
    Status(String),
    State { connected: bool, buffer: bool },
    /// The encoder skipped `frames` this recorder run, at Encoder effort `effort` (not the lightest).
    EncoderSkipped { frames: u64, effort: u8 },
    /// A full recording started or stopped.
    Recording(bool),
}

pub fn run(cmds: Receiver<Cmd>, events: Sender<Event>, repaint: Repaint) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let mut client: Option<Client> = None;
    let mut cfg: Option<Config> = None;
    let mut last_launch = Instant::now();
    let mut watching = false;
    // Whether the replay buffer should be on, and when we connected (to spot a buffer that never starts).
    let mut want_buffer = false;
    let mut connected_at = Instant::now();
    let mut reported_stuck = false;
    let mut last_checkin = Instant::now();
    // A save we asked for: when, and how many clips had landed before it.
    let mut save_pending: Option<(Instant, u32)> = None;
    let mut reported_no_connect = false;
    let (mut disk_checked, mut disk_low) = (Instant::now() - Duration::from_secs(60), false);
    let (mut sampler, mut sampled) = (perf::Sampler::new(), Instant::now());
    let last_state = Cell::new((false, false));
    // Follow the mouse: checked every second, while the health check stays at every 2 s.
    let mut follow: Option<Follow> = None;
    let mut checked = Instant::now();

    // Everything shown in the footer also goes to the app log, along with on/off changes.
    // State is only sent when it changes: the window drains events only while it's open, so
    // a repeat every 2 s would pile up while the app sits in the tray.
    let send = |e: Event| {
        match &e {
            Event::Status(s) => log::write(s),
            &Event::State { connected, buffer } => {
                let (was_connected, was_buffer) = last_state.replace((connected, buffer));
                if (connected, buffer) == (was_connected, was_buffer) {
                    return;
                }
                if connected != was_connected {
                    log::write(if connected { "Recorder connected." } else { "Recorder disconnected." });
                }
                if connected && buffer != was_buffer {
                    log::write(if buffer { "Replay buffer on." } else { "Replay buffer off." });
                }
            }
            Event::EncoderSkipped { frames, .. } => log::write(&format!("The encoder skipped {frames} frames; suggesting a lighter Encoder effort.")),
            Event::Recording(_) => {}
        }
        let _ = events.send(e);
        repaint();
    };

    loop {
        let tick = if follow.is_some() { 1 } else { 2 };
        let cmd = match cmds.recv_timeout(Duration::from_secs(tick)) {
            Ok(cmd) => Some(cmd),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if let (Some(Cmd::Start(c)), false) = (&cmd, watching) {
            let (password, events, repaint) = (c.recorder_password.clone(), events.clone(), repaint.clone());
            std::thread::spawn(move || watch_saves(password, events, repaint));
            watching = true;
        }
        // A short save is a normal save that gets cut down once it lands.
        let cmd = match cmd {
            Some(Cmd::SaveLast(secs)) => {
                *SHORTEN.lock().unwrap() = Some((secs, Instant::now()));
                Some(Cmd::Save)
            }
            Some(Cmd::ToggleReplay) => Some(Cmd::SetReplay(!last_state.get().1)),
            c => c,
        };
        let result: anyhow::Result<Option<String>> = rt.block_on(async {
            match cmd {
                Some(Cmd::Start(new_cfg)) => {
                    (client, follow) = (None, None);
                    send(Event::State { connected: false, buffer: false });
                    begin_session();
                    with_session(recorder_restarted);
                    *FOLDER.lock().unwrap() = new_cfg.folder.clone();
                    recorder::restart(&new_cfg)?;
                    last_launch = Instant::now();
                    cfg = Some(new_cfg);
                    (want_buffer, reported_stuck, reported_no_connect) = (true, false, false);
                    Ok(Some("Starting the recorder...".into()))
                }
                Some(Cmd::Save) => {
                    let c = client.as_ref().ok_or_else(|| anyhow::anyhow!("The recorder is still starting, try again in a few seconds"))?;
                    // OBS drops the save without a word if the clip folder is gone (e.g. a removed drive).
                    if let Some(cfg) = &cfg {
                        let _ = std::fs::create_dir_all(&cfg.folder);
                    }
                    c.replay_buffer().save().await?;
                    save_pending = Some((Instant::now(), SAVED.load(Ordering::Relaxed)));
                    Ok(Some("Saving clip...".into()))
                }
                Some(Cmd::SetReplay(on)) => {
                    let c = client.as_ref().ok_or_else(|| anyhow::anyhow!("The recorder is still starting"))?;
                    if on { c.replay_buffer().start().await? } else { c.replay_buffer().stop().await? }
                    (want_buffer, reported_stuck, connected_at) = (on, false, Instant::now());
                    if on {
                        begin_session();
                    } else {
                        end_session("replay turned off");
                    }
                    send(Event::State { connected: true, buffer: on });
                    Ok(Some(if on { "Instant replay is on." } else { "Instant replay is off." }.into()))
                }
                Some(Cmd::ToggleRecord) => {
                    let c = client.as_ref().ok_or_else(|| anyhow::anyhow!("The recorder is still starting"))?;
                    let root = clips_folder();
                    if c.recording().status().await?.active {
                        let path = c.recording().stop().await?;
                        // Replays go back to the clips folder. OBS refuses the change ("OutputRunning")
                        // until it has finished closing the file, which takes a moment.
                        let dir = root.display().to_string();
                        let mut tries = 0;
                        while let Err(e) = c.config().set_record_directory(&dir).await {
                            tries += 1;
                            if tries > 20 {
                                return Err(e.into());
                            }
                            tokio::time::sleep(Duration::from_millis(250)).await;
                        }
                        send(Event::Recording(false));
                        feedback("Recording saved", std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), None);
                        Ok(Some(format!("Recording saved: {path}")))
                    } else {
                        // OBS has one output folder for recordings and replays alike; replays saved
                        // meanwhile are moved back out when they land.
                        let dir = root.join("Recordings");
                        std::fs::create_dir_all(&dir)?;
                        c.config().set_record_directory(&dir.display().to_string()).await?;
                        c.recording().start().await?;
                        send(Event::Recording(true));
                        feedback("Recording", "Press the key again to stop".into(), None);
                        Ok(Some("Recording started.".into()))
                    }
                }
                Some(Cmd::Screenshot) => {
                    let c = client.as_ref().ok_or_else(|| anyhow::anyhow!("The recorder is still starting"))?;
                    let dir = destination();
                    std::fs::create_dir_all(&dir)?;
                    let path = dir.join(format!("Screenshot {}.png", clips::stamp(std::time::SystemTime::now())));
                    c.sources()
                        .save_screenshot(obws::requests::sources::SaveScreenshot {
                            source: obws::requests::sources::SourceId::Name("Replay"),
                            format: "png",
                            width: None,
                            height: None,
                            compression_quality: None,
                            file_path: &path,
                        })
                        .await?;
                    feedback("Screenshot saved", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), None);
                    Ok(Some(format!("Screenshot saved: {}", path.display())))
                }
                Some(Cmd::ToggleMic) => {
                    let c = client.as_ref().ok_or_else(|| anyhow::anyhow!("The recorder is still starting"))?;
                    let mics = c.inputs().list(Some("wasapi_input_capture")).await?;
                    anyhow::ensure!(!mics.is_empty(), "No mic is being recorded (pick one in the Audio tab)");
                    // All mics follow the first, so they never end up half muted.
                    let mute = !c.inputs().muted((&mics[0].id).into()).await?;
                    for m in &mics {
                        c.inputs().set_muted((&m.id).into(), mute).await?;
                    }
                    toast::show(if mute { "Mic muted" } else { "Mic on" }, "In clips and recordings".into());
                    Ok(Some(if mute { "Mic muted." } else { "Mic unmuted." }.into()))
                }
                // Turned into Save / SetReplay above.
                Some(Cmd::SaveLast(_) | Cmd::ToggleReplay) => Ok(None),
                // Health check: keep the connection alive and the recorder running.
                None => {
                    let Some(cfg) = &cfg else { return Ok(None) };
                    if let Some(c) = &client {
                        if let Some(f) = follow.as_mut()
                            && let Some(id) = f.step(display::under_pointer().map(|m| m.id).as_deref(), Instant::now())
                        {
                            let moved = c
                                .inputs()
                                .set_settings(obws::requests::inputs::SetSettings {
                                    input: obws::requests::inputs::InputId::Name("Screen"),
                                    settings: &serde_json::json!({ "monitor_id": id }),
                                    overlay: Some(true),
                                })
                                .await;
                            // Not fatal: the health check below still runs and notices a gone recorder.
                            if let Err(e) = moved {
                                log::write(&format!("Couldn't move the capture to the monitor under the mouse: {e}"));
                            }
                        }
                        if checked.elapsed() < Duration::from_millis(1900) {
                            return Ok(None);
                        }
                        checked = Instant::now();
                        if let Ok(st) = c.general().stats().await {
                            let buffer = c.replay_buffer().status().await.unwrap_or(false);
                            send(Event::State { connected: true, buffer });
                            // Skipped frames in this run (these settings): suggest a lighter Encoder
                            // effort, once per effort per session.
                            let effort = cfg.encoder_effort.min(2);
                            let mut skipped = None;
                            with_session(|s| {
                                s.live = Frames {
                                    rendered: st.render_total_frames.into(),
                                    render_lagged: st.render_skipped_frames.into(),
                                    encoded: st.output_total_frames.into(),
                                    encode_skipped: st.output_skipped_frames.into(),
                                };
                                if s.live.encoder_struggling() && s.skip_warned & 1 << effort == 0 {
                                    s.skip_warned |= 1 << effort;
                                    skipped = Some(s.live.encode_skipped);
                                }
                            });
                            if let Some(frames) = skipped
                                && recorder::lighter_effort(effort).is_some()
                            {
                                send(Event::EncoderSkipped { frames, effort });
                            }
                            if sampled.elapsed() > Duration::from_secs(5) {
                                sampled = Instant::now();
                                sampler.tick(recorder::pid());
                                if let Some(u) = sampler.recorder {
                                    with_session(|s| s.load.add(u));
                                }
                            }
                            if last_checkin.elapsed() > Duration::from_secs(3600) {
                                last_checkin = Instant::now();
                                with_session(|s| log::write(&format!("Hourly check: {}", summary(s))));
                            }
                            // OBS sometimes refuses to start the buffer (bad folder, encoder error) and
                            // only says why in its own log; bring that up once.
                            if want_buffer && !buffer && !reported_stuck && connected_at.elapsed() > Duration::from_secs(10) {
                                reported_stuck = true;
                                anyhow::bail!(match recorder::last_obs_error() {
                                    Some(line) => format!("The replay buffer didn't start. OBS says: {line}"),
                                    None => "The replay buffer didn't start. Check the OBS log.".into(),
                                });
                            }
                            if let Some((asked, before)) = save_pending {
                                if SAVED.load(Ordering::Relaxed) > before {
                                    save_pending = None;
                                } else if asked.elapsed() > Duration::from_secs(10) {
                                    save_pending = None;
                                    anyhow::bail!(match recorder::last_obs_error() {
                                        Some(line) => format!("The clip wasn't saved. OBS says: {line}"),
                                        None => "The clip wasn't saved. Check the clips folder and the OBS log.".into(),
                                    });
                                }
                            }
                            // A full disk makes OBS fail quietly; warn once while it stays low.
                            if disk_checked.elapsed() > Duration::from_secs(60) {
                                disk_checked = Instant::now();
                                if let Some(free) = clips::free_space(&cfg.folder) {
                                    if free < LOW_DISK && !disk_low {
                                        disk_low = true;
                                        let msg = format!("Low disk space: {} left for clips in {}. Clips may fail to save.", clips::size(free), cfg.folder);
                                        toast::show("Low disk space", format!("{} left for clips", clips::size(free)));
                                        return Ok(Some(msg));
                                    }
                                    // A little above the line, so it doesn't flap.
                                    disk_low = disk_low && free < LOW_DISK + LOW_DISK / 5;
                                }
                            }
                            return Ok(None);
                        }
                        client = None;
                        send(Event::State { connected: false, buffer: false });
                    }
                    match Client::connect("127.0.0.1", recorder::PORT, Some(cfg.recorder_password.as_str())).await {
                        Ok(c) => {
                            let buffer = c.replay_buffer().status().await.unwrap_or(false);
                            send(Event::State { connected: true, buffer });
                            client = Some(c);
                            connected_at = Instant::now();
                            // A (re)started recorder shows the chosen monitor again.
                            follow = cfg.follows_mouse().then(|| recorder::chosen_monitor(cfg)).flatten().map(|m| Follow::new(m.id));
                            reported_no_connect = false;
                            // The buffer often starts a moment after OBS connects; the header shows its live state.
                            Ok(Some("Connected to the recorder.".into()))
                        }
                        // Recorder died (or was closed from its tray icon): bring it back, at most every 20 s.
                        Err(_) if !recorder::is_running() && last_launch.elapsed() > Duration::from_secs(20) => {
                            with_session(|s| {
                                s.restarts += 1;
                                recorder_restarted(s);
                            });
                            recorder::restart(cfg)?;
                            last_launch = Instant::now();
                            Ok(Some("Recorder stopped, restarting it...".into()))
                        }
                        // Running but never answering: usually another app holds the port.
                        Err(_) if !reported_no_connect && last_launch.elapsed() > Duration::from_secs(30) => {
                            reported_no_connect = true;
                            let why = recorder::last_obs_error().map(|l| format!(" OBS says: {l}")).unwrap_or_default();
                            anyhow::bail!("Can't reach the recorder on port {}. Another app may be using that port, or the recorder didn't start.{why}", recorder::PORT)
                        }
                        Err(_) => Ok(None),
                    }
                }
            }
        });
        match result {
            Ok(Some(msg)) => send(Event::Status(msg)),
            Ok(None) => {}
            Err(e) => {
                with_session(|s| s.errors += 1);
                send(Event::Status(format!("Error: {e:#}")))
            }
        }
    }
}

/// Cuts a just-saved clip down to its last `secs` seconds, in place. Returns a note for the popup,
/// or None when the clip was short enough already. On failure the full clip is kept.
fn shorten(path: &std::path::Path, secs: u32) -> Option<String> {
    let len = media::duration(path).ok()?;
    if len <= secs as f64 + 1.0 {
        return None;
    }
    let tmp = path.with_extension("cutting.mp4");
    // rename replaces the original in one step on Windows, so there's never a moment with neither.
    let result = media::trim(path, &tmp, len - secs as f64, len).and_then(|()| Ok(std::fs::rename(&tmp, path)?));
    match result {
        Ok(()) => Some(format!("last {}:{:02}", secs / 60, secs % 60)),
        Err(e) => {
            // The cut copy is the only one left if the original somehow went missing.
            if path.exists() {
                let _ = std::fs::remove_file(&tmp);
            }
            log::write(&format!("Couldn't cut the clip to the last {secs} s, kept the full clip: {e:#}"));
            Some("full length, couldn't cut it".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FOLLOW_DELAY, Follow, Frames};
    use std::time::{Duration, Instant};

    #[test]
    fn follow_switches_after_the_mouse_stays() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut f = Follow::new("main".into());
        assert_eq!(f.step(Some("main"), at(0)), None);
        // A quick pass over the other monitor and back: no switch.
        assert_eq!(f.step(Some("left"), at(1)), None);
        assert_eq!(f.step(Some("main"), at(2)), None);
        assert_eq!(f.step(Some("left"), at(4)), None);
        // In a gap between monitors: nothing changes, the wait goes on.
        assert_eq!(f.step(None, at(5)), None);
        assert_eq!(f.step(Some("left"), at(4) + FOLLOW_DELAY - Duration::from_millis(1)), None);
        assert_eq!(f.step(Some("left"), at(4) + FOLLOW_DELAY).as_deref(), Some("left"));
        // Already there: no repeat. Moving to a third monitor starts a new wait.
        assert_eq!(f.step(Some("left"), at(10)), None);
        assert_eq!(f.step(Some("right"), at(11)), None);
        assert_eq!(f.step(Some("main"), at(12)), None);
        assert_eq!(f.step(Some("main"), at(14)).as_deref(), Some("main"));
    }

    #[test]
    fn encoder_struggles_at_one_percent_skipped() {
        let f = |encoded, encode_skipped| Frames { encoded, encode_skipped, ..Default::default() };
        assert!(f(3000, 30).encoder_struggling());
        assert!(!f(3001 * 100, 3000).encoder_struggling());
        // A short hiccup at the start isn't worth a note.
        assert!(!f(100, 29).encoder_struggling());
    }

    /// A short save on a copy of a real clip: cut in place to about its last 30 s.
    #[test]
    fn short_save_cuts_in_place() {
        let folder = crate::config::Config::load().folder;
        let Some(src) = crate::clips::list(&folder, 20).into_iter().map(|c| c.path).find(|p| !p.to_string_lossy().contains("(trim")) else { return };
        let len = crate::media::duration(&src).unwrap();
        if len < 45.0 {
            return;
        }
        // A 40 s copy to work on, so the real clip is never touched.
        let copy = std::env::temp_dir().join("instant-replay-short-test.mp4");
        let _ = std::fs::remove_file(&copy);
        crate::media::trim(&src, &copy, len - 40.0, len).unwrap();
        assert_eq!(super::shorten(&copy, 30).as_deref(), Some("last 0:30"));
        let got = crate::media::duration(&copy).unwrap();
        assert!((29.5..=32.5).contains(&got), "cut clip is {got} s");
        assert!(!copy.with_extension("cutting.mp4").exists());
        // Already short enough: left alone.
        assert_eq!(super::shorten(&copy, 60), None);
        let _ = std::fs::remove_file(&copy);
    }
}

fn clip_saved(path: &std::path::Path, note: Option<String>, events: &Sender<Event>, repaint: &Repaint) {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    feedback("Clip saved", match &note { Some(n) => format!("{name} ({n})"), None => name }, Some(path));
    let msg = match note {
        Some(n) => format!("Clip saved ({n}): {}", path.display()),
        None => format!("Clip saved: {}", path.display()),
    };
    log::write(&msg);
    let _ = events.send(Event::Status(msg));
    repaint();
}

/// Listens for saved clips, whether from OBS's own hotkey or the Save button, and shows the popup.
fn watch_saves(password: String, events: Sender<Event>, repaint: Repaint) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async move {
        loop {
            if let Ok(client) = Client::connect("127.0.0.1", recorder::PORT, Some(password.as_str())).await {
                if let Ok(mut stream) = client.events().map(Box::pin) {
                    let mut health = tokio::time::interval(Duration::from_secs(5));
                    loop {
                        tokio::select! {
                            ev = stream.next() => match ev {
                                Some(obws::events::Event::ReplayBufferSaved { path }) => {
                                    SAVED.fetch_add(1, Ordering::Relaxed);
                                    if !is_our_clip(&path) {
                                        log::write(&format!("Ignored a saved-clip report outside the clips folder: {}", path.display()));
                                        continue;
                                    }
                                    with_session(|s| s.clips += 1);
                                    let short = SHORTEN.lock().unwrap().take().filter(|(_, at)| at.elapsed() < Duration::from_secs(20));
                                    let (events, repaint) = (events.clone(), repaint.clone());
                                    // Cutting takes a moment; don't hold up the event stream.
                                    std::thread::spawn(move || {
                                        let note = short.map(|(secs, _)| shorten(&path, secs));
                                        let path = place(&path);
                                        clip_saved(&path, note.flatten(), &events, &repaint);
                                    });
                                }
                                Some(_) => {}
                                None => break,
                            },
                            // The stream stays open after OBS goes away (e.g. a restart); notice and reconnect.
                            _ = health.tick() => if client.general().version().await.is_err() { break },
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}
