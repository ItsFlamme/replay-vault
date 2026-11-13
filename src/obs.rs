//! Background worker that runs the hidden recorder OBS and talks to it over its WebSocket.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use obws::Client;

use crate::config::Config;
use crate::{autopause, clips, display, log, media, perf, recorder, toast};

/// Whether to show the "Clip saved" popup; the UI keeps it in sync with the setting.
pub static SAVE_POPUP: AtomicBool = AtomicBool::new(true);
/// Sort saved clips and screenshots into a folder per game; play a soft sound on save.
pub static SORT_BY_GAME: AtomicBool = AtomicBool::new(false);
pub static SAVE_SOUND: AtomicBool = AtomicBool::new(false);
/// Even out a saved clip that's louder on one side (experimental, off unless turned on).
pub static CENTER_SOUND: AtomicBool = AtomicBool::new(false);

/// Clips OBS has reported saved, to spot a save request that never lands.
static SAVED: AtomicU32 = AtomicU32::new(0);

/// Audio tracks the recorder writes into each clip (1 to 6), to check saved clips against.
static TRACKS: AtomicU8 = AtomicU8::new(1);
/// The app is quitting or removing itself: a recorder that's gone stays gone.
pub static QUITTING: AtomicBool = AtomicBool::new(false);
/// recorder::mic_mixed_tracks of the settings the recorder runs with.
static MIC_MIXED: AtomicU8 = AtomicU8::new(0);
/// The mic is muted with its key: kept through recorder restarts, not past this run of the app.
/// Changed only by the recorder thread (`run`); `recorder::write_config` reads it, so a restarted
/// recorder starts with the mic muted.
pub static MIC_MUTED: AtomicBool = AtomicBool::new(false);

/// The clips folder of the running config; saved-clip paths from the recorder must be inside it.
static FOLDER: Mutex<String> = Mutex::new(String::new());

fn clips_folder() -> std::path::PathBuf {
    std::path::PathBuf::from(FOLDER.lock().unwrap().clone())
}

/// Where a new clip or screenshot from `game` belongs: its folder when sorting, else the clips folder.
fn destination(game: &str) -> std::path::PathBuf {
    let root = clips_folder();
    if SORT_BY_GAME.load(Ordering::Relaxed) { root.join(game) } else { root }
}

/// Moves a just-saved clip to where it belongs (the game's folder when sorting, else the clips
/// folder; a replay saved during a full recording lands in the Recordings folder at first) and
/// puts the game in front of its name. Keeps it where it is if the move fails.
fn place(path: &std::path::Path) -> std::path::PathBuf {
    let game = clips::saving_folder_name();
    autopause::saved_from(&game);
    match clips::move_into(path, &destination(&game), &game) {
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
            None => toast::show(toast::Kind::Saved, title, detail),
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

/// How long one request to the recorder may take. obws has no timeout of its own and every command
/// waits its turn on one thread, so a recorder that's running but stuck would hold every key and
/// button forever.
const ANSWER_WITHIN: Duration = Duration::from_secs(10);
/// How long a command (a few requests) may take before the recorder counts as gone.
const COMMAND_WITHIN: Duration = Duration::from_secs(15);
/// How long connecting may take; a recorder that's up answers in well under a second.
const CONNECT_WITHIN: Duration = Duration::from_secs(5);
/// A recorder that's running but hasn't answered for this long is stuck: it's closed and started again.
const HUNG_FOR: Duration = Duration::from_secs(60);
/// Looks at whether the recorder's process is there (tasklist) while it can't be reached: this
/// often at first, twice as long each time after, up to `LOOK_MAX`.
const LOOK_FIRST: Duration = Duration::from_secs(2);
const LOOK_MAX: Duration = Duration::from_secs(30);
/// Restarts of a recorder that runs but doesn't work: at most this often, further apart after
/// each one that didn't help, up to `RESCUE_GAP_MAX`.
const RESCUE_GAP: Duration = Duration::from_secs(180);
const RESCUE_GAP_MAX: Duration = Duration::from_secs(1800);

/// The recorder took too long to answer; handled like a recorder that went away.
#[derive(Debug)]
struct NoAnswer;

impl std::fmt::Display for NoAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("The recorder didn't answer")
    }
}

impl std::error::Error for NoAnswer {}

/// One request to the recorder, given up on after `ANSWER_WITHIN`.
async fn ask<T, E: std::error::Error + Send + Sync + 'static>(request: impl Future<Output = Result<T, E>>) -> anyhow::Result<T> {
    Ok(tokio::time::timeout(ANSWER_WITHIN, request).await.map_err(|_| NoAnswer)??)
}

/// `work`, given up on after `limit` (None: no limit) as if the recorder had stopped answering.
async fn within<T>(limit: Option<Duration>, work: impl Future<Output = anyhow::Result<T>>) -> anyhow::Result<T> {
    match limit {
        Some(limit) => tokio::time::timeout(limit, work).await.unwrap_or_else(|_| Err(NoAnswer.into())),
        None => work.await,
    }
}

/// When to look next whether the recorder's process is there, while it can't be reached.
struct Backoff {
    next: Option<Instant>,
    step: Duration,
}

impl Backoff {
    fn new() -> Self {
        Backoff { next: None, step: LOOK_FIRST }
    }

    fn due(&self, now: Instant) -> bool {
        self.next.is_none_or(|n| now >= n)
    }

    fn looked(&mut self, now: Instant) {
        self.next = Some(now + self.step);
        self.step = (self.step * 2).min(LOOK_MAX);
    }
}

/// Restarts of a recorder that's running but not working (stuck, or instant replay won't start
/// again): the last one, and how many since it last worked.
#[derive(Default)]
struct Rescue {
    last: Option<Instant>,
    tries: u32,
}

impl Rescue {
    /// 3 minutes after the first, then twice as long after each one, up to 30 minutes.
    fn gap(&self) -> Duration {
        (RESCUE_GAP * (1 << self.tries.saturating_sub(1).min(4))).min(RESCUE_GAP_MAX)
    }

    fn due(&self, now: Instant) -> bool {
        self.last.is_none_or(|l| now.saturating_duration_since(l) >= self.gap())
    }

    /// Whether to close a recorder that's running but hasn't answered since `since`, and start it again.
    fn hung(&self, since: Instant, now: Instant) -> bool {
        now.saturating_duration_since(since) >= HUNG_FOR && self.due(now)
    }

    fn restarted(&mut self, now: Instant) {
        self.last = Some(now);
        self.tries += 1;
    }

    /// The recorder works again: the next restart, if one is ever needed, waits the shortest gap.
    fn worked(&mut self) {
        self.tries = 0;
    }
}

/// What the watchdog does about instant replay that should be on but is off.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Revive {
    Wait,
    /// Ask the recorder to turn it on again.
    TurnOn,
    /// Restart the recorder.
    Restart,
}

/// The watchdog's next step after `tries` tries at turning instant replay back on, the last one
/// `since_try` ago: up to 3 tries 20 s apart, then a recorder restart when `restart_due` (which
/// starts the tries over once it's back).
fn revive_step(tries: u32, since_try: Duration, restart_due: bool) -> Revive {
    match tries {
        0..3 if since_try > Duration::from_secs(20) => Revive::TurnOn,
        0..3 => Revive::Wait,
        _ if restart_due => Revive::Restart,
        _ => Revive::Wait,
    }
}

/// Whether OBS refused to turn instant replay `on` (or off) only because it already is.
fn already(on: bool, e: &obws::error::Error) -> bool {
    use obws::responses::StatusCode;
    matches!(e, obws::error::Error::Api { code, .. } if *code == if on { StatusCode::OutputRunning } else { StatusCode::OutputNotRunning })
}

pub type Repaint = Arc<dyn Fn() + Send + Sync>;

pub enum Cmd {
    /// Write the recorder config and (re)start the hidden OBS.
    Start(Box<Config>),
    /// Save the replay into the clips folder.
    Save,
    /// Turn instant replay on or off (by the user: the switch, the tray menu).
    SetReplay(bool),
    /// Auto-pause turns instant replay on or off (a game opened, or none for a while).
    AutoReplay(bool),
    /// Flip instant replay, start/stop a full recording, mute/unmute the mic (hotkeys).
    ToggleReplay,
    ToggleRecord,
    ToggleMic,
    /// Save a PNG of the screen next to the clips.
    Screenshot,
    /// OBS's save key was pressed `at`, with `saved_before` clips saved until then. OBS saves on
    /// it by itself; this only checks the clip lands, the way Save clip does. Made by `save_key_pressed`.
    SaveKeyPressed { saved_before: u32, at: Instant },
}

impl Cmd {
    /// `SaveKeyPressed` for a press now.
    pub fn save_key_pressed() -> Cmd {
        Cmd::SaveKeyPressed { saved_before: SAVED.load(Ordering::Relaxed), at: Instant::now() }
    }
}

pub enum Event {
    Status(String),
    State { connected: bool, buffer: bool },
    /// The encoder skipped `frames` this recorder run, at Encoder effort `effort` (not the lightest).
    EncoderSkipped { frames: u64, effort: u8 },
    /// A full recording started or stopped.
    Recording(bool),
    /// The mic was muted or unmuted (for this session, through recorder restarts).
    MicMuted(bool),
}

/// Mutes or unmutes every mic the recorder records, all alike so none is left half muted. Errs if
/// no mic is recorded.
async fn set_mics(c: &Client, mute: bool) -> anyhow::Result<()> {
    let mics = ask(c.inputs().list(Some("wasapi_input_capture"))).await?;
    anyhow::ensure!(!mics.is_empty(), "No mic is being recorded (pick one on the Audio page)");
    for m in &mics {
        ask(c.inputs().set_muted((&m.id).into(), mute)).await?;
    }
    Ok(())
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
    // Watchdog: tries at turning a buffer back on that went off by itself, when the last one was,
    // and whether the user was told it stopped (so they hear when it's back).
    // `seen_on`: the buffer ran since replay was last turned on, so it going off is news (a first
    // start that's just slow isn't).
    let (mut revive_tries, mut revived_at, mut told_stopped, mut seen_on) = (0u32, ago(60), false, false);
    let mut last_checkin = Instant::now();
    // A save we asked for: when, and how many clips had landed before it.
    let mut save_pending: Option<(Instant, u32)> = None;
    let mut reported_no_connect = false;
    let (mut disk_checked, mut disk_low) = (ago(60), false);
    let (mut sampler, mut sampled) = (perf::Sampler::recorder_only(), Instant::now());
    let last_state = Cell::new((false, false));
    // Follow the mouse: checked every second, while the health check stays at every 2 s.
    let mut follow: Option<Follow> = None;
    let mut checked = Instant::now();
    // Since when the recorder hasn't answered (None while it does or was just started), when to
    // look next whether its process is there, and restarts of one that runs but doesn't work.
    let (mut unreachable_since, mut look, mut rescue): (Option<Instant>, _, Rescue) = (None, Backoff::new(), Rescue::default());

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
            Event::Recording(_) | Event::MicMuted(_) => {}
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
        let cmd = match cmd {
            Some(Cmd::ToggleReplay) => Some(Cmd::SetReplay(!last_state.get().1)),
            c => c,
        };
        // Turned on or off by the user (the switch, the tray, the key), not by auto-pause.
        let by_hand = matches!(cmd, Some(Cmd::SetReplay(_)));
        // A command that hangs counts as a recorder that's gone. A start has its own limits, and
        // the health check gives up on each request by itself, so it can go on to reconnect.
        let limit = (!matches!(cmd, None | Some(Cmd::Start(_)))).then_some(COMMAND_WITHIN);
        let result: anyhow::Result<Option<String>> = rt.block_on(within(limit, async {
            match cmd {
                Some(Cmd::Start(new_cfg)) => {
                    // New settings restart OBS: a full recording is finished and kept first.
                    if let Some(c) = client.take() {
                        let finished = tokio::time::timeout(Duration::from_secs(15), recorder::finish_recording(&c)).await;
                        match finished.unwrap_or_else(|_| Err(anyhow::anyhow!("the recorder didn't answer"))) {
                            Ok(Some(path)) => {
                                send(Event::Recording(false));
                                feedback("Recording saved", std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), None);
                                send(Event::Status(format!("Recording saved before the recorder restarted: {path}")));
                            }
                            Ok(None) => {}
                            Err(e) => log::write(&format!("Couldn't stop the full recording before restarting the recorder: {e:#}")),
                        }
                    }
                    follow = None;
                    send(Event::State { connected: false, buffer: false });
                    begin_session();
                    with_session(recorder_restarted);
                    *FOLDER.lock().unwrap() = new_cfg.folder.clone();
                    TRACKS.store(new_cfg.track_count.clamp(1, 6), Ordering::Relaxed);
                    MIC_MIXED.store(recorder::mic_mixed_tracks(&new_cfg), Ordering::Relaxed);
                    // Instant replay starts on at first; after that new settings keep it as it
                    // was (turned off, or paused with no game open).
                    want_buffer = want_buffer || cfg.is_none();
                    // Set first, so a start that fails is tried again by the health check, every 20 s.
                    (last_launch, unreachable_since) = (Instant::now(), None);
                    let new_cfg = cfg.insert(*new_cfg);
                    recorder::restart(new_cfg)?;
                    (reported_stuck, reported_no_connect, revive_tries, told_stopped, seen_on) = (false, false, 0, false, false);
                    Ok(Some("Starting the recorder...".into()))
                }
                Some(Cmd::Save) => {
                    let c = client.as_ref().ok_or_else(|| anyhow::anyhow!("The recorder is still starting, try again in a few seconds"))?;
                    // OBS drops the save without a word if the clip folder is gone (e.g. a removed drive).
                    if let Some(cfg) = &cfg {
                        let _ = std::fs::create_dir_all(&cfg.folder);
                    }
                    // Marked first, so a recorder closed right after the click waits for the clip.
                    recorder::save_asked();
                    ask(c.replay_buffer().save()).await.inspect_err(|_| recorder::save_done())?;
                    save_pending = Some((Instant::now(), SAVED.load(Ordering::Relaxed)));
                    Ok(Some("Saving clip...".into()))
                }
                Some(Cmd::SaveKeyPressed { saved_before, at }) => {
                    let (connected, buffer) = last_state.get();
                    if !connected {
                        log::write("The save key was pressed while the recorder isn't connected.");
                        return Ok(None);
                    }
                    if !buffer {
                        let why = if autopause::PAUSED.load(Ordering::Relaxed) { "Nothing was saved: no game is open" } else { "Nothing was saved. Turn it on in Replay Vault" };
                        toast::show(toast::Kind::Warning, "Instant replay is paused", why.into());
                        return Ok(Some("The save key was pressed while instant replay is off; nothing was saved.".into()));
                    }
                    // OBS drops the save without a word if the clip folder is gone; the next press works.
                    if let Some(cfg) = &cfg {
                        let _ = std::fs::create_dir_all(&cfg.folder);
                    }
                    // Unless the clip already landed, it's on its way: checked like a click on Save clip.
                    if SAVED.load(Ordering::Relaxed) == saved_before {
                        recorder::save_asked();
                        save_pending = Some((at, saved_before));
                    }
                    Ok(None)
                }
                Some(Cmd::SetReplay(on) | Cmd::AutoReplay(on)) => {
                    let c = client.as_ref().ok_or_else(|| anyhow::anyhow!("The recorder is still starting"))?;
                    let done = if on { c.replay_buffer().start().await } else { c.replay_buffer().stop().await };
                    // Already on (or off) is what was asked for.
                    match done {
                        Err(e) if !already(on, &e) => return Err(e.into()),
                        _ => {}
                    }
                    autopause::replay_set(on, by_hand);
                    (want_buffer, reported_stuck, connected_at, revive_tries, told_stopped, seen_on) = (on, false, Instant::now(), 0, false, false);
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
                        recorder::recording_stopped(&path);
                        // Stopped and saved, whatever happens to the folder change below.
                        send(Event::Recording(false));
                        feedback("Recording saved", std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), None);
                        // Replays go back to the clips folder. OBS refuses the change ("OutputRunning")
                        // until it has finished closing the file, which takes a moment.
                        let dir = root.display().to_string();
                        let mut tries = 0;
                        while let Err(e) = c.config().set_record_directory(&dir).await {
                            tries += 1;
                            if tries > 20 {
                                return Err(anyhow::Error::from(e).context(format!("Recording saved: {path}, but replays couldn't be pointed back at the clips folder")));
                            }
                            tokio::time::sleep(Duration::from_millis(250)).await;
                        }
                        Ok(Some(format!("Recording saved: {path}")))
                    } else {
                        // OBS has one output folder for recordings and replays alike; replays saved
                        // meanwhile are moved back out when they land.
                        let dir = root.join("Recordings");
                        std::fs::create_dir_all(&dir)?;
                        c.config().set_record_directory(&dir.display().to_string()).await?;
                        if let Err(e) = c.recording().start().await {
                            // Not recording after all: replays go on landing in the clips folder.
                            if let Err(back) = c.config().set_record_directory(&root.display().to_string()).await {
                                log::write(&format!("Couldn't point replays back at the clips folder: {back}"));
                            }
                            return Err(e.into());
                        }
                        send(Event::Recording(true));
                        feedback("Recording", "Press the key again to stop".into(), None);
                        Ok(Some("Recording started.".into()))
                    }
                }
                Some(Cmd::Screenshot) => {
                    let c = client.as_ref().ok_or_else(|| anyhow::anyhow!("The recorder is still starting"))?;
                    let game = clips::saving_folder_name();
                    autopause::saved_from(&game);
                    let dir = destination(&game);
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
                    let mute = !MIC_MUTED.load(Ordering::Relaxed);
                    set_mics(c, mute).await?;
                    MIC_MUTED.store(mute, Ordering::Relaxed);
                    send(Event::MicMuted(mute));
                    toast::show(toast::Kind::Info, if mute { "Mic muted" } else { "Mic on" }, "In clips and recordings".into());
                    Ok(Some(if mute { "Mic muted." } else { "Mic unmuted." }.into()))
                }
                // Turned into SetReplay above.
                Some(Cmd::ToggleReplay) => Ok(None),
                // Health check: keep the connection alive and the recorder running.
                None => {
                    let Some(cfg) = &cfg else { return Ok(None) };
                    if let Some(c) = &client {
                        if let Some(f) = follow.as_mut()
                            && let Some(id) = f.step(display::under_pointer().map(|m| m.id).as_deref(), Instant::now())
                        {
                            let moved = ask(c.inputs().set_settings(obws::requests::inputs::SetSettings {
                                input: obws::requests::inputs::InputId::Name("Screen"),
                                settings: &serde_json::json!({ "monitor_id": id }),
                                overlay: Some(true),
                            }))
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
                        if let Ok(st) = ask(c.general().stats()).await
                            && let Ok(buffer) = ask(c.replay_buffer().status()).await.or_else(|e| if e.is::<NoAnswer>() { Err(e) } else { Ok(false) })
                        {
                            send(Event::State { connected: true, buffer });
                            if buffer || !want_buffer {
                                rescue.worked();
                            }
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
                            // Watchdog: replay should be on but isn't (it never started, or stopped
                            // by itself). Turn it back on, up to 3 times 20 s apart, and say so; then
                            // restart the recorder (spaced out by `rescue`) and start over.
                            if want_buffer && buffer {
                                if told_stopped {
                                    toast::show(toast::Kind::Info, "Instant replay is back on", "Clips are being kept again".into());
                                }
                                (revive_tries, told_stopped, reported_stuck, seen_on) = (0, false, false, true);
                            } else if want_buffer && connected_at.elapsed() > Duration::from_secs(10) {
                                let step = revive_step(revive_tries, revived_at.elapsed(), rescue.due(Instant::now()) && !QUITTING.load(Ordering::Relaxed));
                                // A restart would end a full recording: it waits until that's stopped.
                                let step = match step {
                                    Revive::Restart if ask(c.recording().status()).await.is_ok_and(|s| s.active) => Revive::Wait,
                                    s => s,
                                };
                                let obs_says = || match recorder::last_obs_error() {
                                    Some(line) => format!("Instant replay is off and didn't start again. OBS says: {line}"),
                                    None => "Instant replay is off and didn't start again. Check the OBS log.".into(),
                                };
                                if step == Revive::Restart {
                                    let first = !reported_stuck;
                                    reported_stuck = true;
                                    if first {
                                        toast::show(toast::Kind::Warning, "Instant replay is off", "It didn't start again. Restarting the recorder".into());
                                    }
                                    let msg = format!("{} Restarting the recorder...", obs_says());
                                    let now = Instant::now();
                                    rescue.restarted(now);
                                    with_session(|s| {
                                        s.restarts += 1;
                                        recorder_restarted(s);
                                    });
                                    last_launch = now;
                                    recorder::restart(cfg)?;
                                    (last_launch, unreachable_since) = (Instant::now(), None);
                                    return Ok(Some(msg));
                                }
                                if step == Revive::TurnOn {
                                    if seen_on && !told_stopped {
                                        told_stopped = true;
                                        toast::show(toast::Kind::Warning, "Instant replay stopped", "Turning it back on".into());
                                    }
                                    revive_tries += 1;
                                    revived_at = Instant::now();
                                    log::write(&format!("Instant replay is off but should be on; turning it back on (try {revive_tries} of 3)."));
                                    if let Err(e) = ask(c.replay_buffer().start()).await {
                                        log::write(&format!("Couldn't turn instant replay back on: {e}"));
                                    }
                                } else if revive_tries >= 3 && !reported_stuck {
                                    // OBS refuses (bad folder, encoder error) and only says why in its
                                    // own log. Said once per stretch; a restart comes when it's due.
                                    reported_stuck = true;
                                    toast::show(toast::Kind::Warning, "Instant replay is off", "It didn't start again. Open Replay Vault to see why.".into());
                                    anyhow::bail!(obs_says());
                                }
                            }
                            if let Some((asked, before)) = save_pending {
                                if SAVED.load(Ordering::Relaxed) > before {
                                    save_pending = None;
                                } else if asked.elapsed() > Duration::from_secs(10) {
                                    save_pending = None;
                                    // Its report may have been missed (the listener reconnecting): a clip
                                    // that landed since counts, and one still being written gets more time.
                                    let now = std::time::SystemTime::now();
                                    let landed = unplaced_clips(&clips_folder(), now.checked_sub(asked.elapsed()).unwrap_or(now));
                                    let found = !landed.is_empty();
                                    if !handle_found(landed, &events, &repaint).is_empty() {
                                        save_pending = Some((Instant::now(), before));
                                    }
                                    if found {
                                        return Ok(None);
                                    }
                                    toast::show(toast::Kind::Warning, "Clip not saved", "Open Replay Vault to see why".into());
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
                                        toast::show(toast::Kind::Warning, "Low disk space", format!("{} left for clips", clips::size(free)));
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
                    // Connected counts once a request is answered: a stuck recorder may still let us connect.
                    let reached = async {
                        let c = tokio::time::timeout(CONNECT_WITHIN, Client::connect("127.0.0.1", recorder::PORT, Some(cfg.recorder_password.as_str()))).await.map_err(|_| NoAnswer)??;
                        let buffer = ask(c.replay_buffer().status()).await.or_else(|e| if e.is::<NoAnswer>() { Err(e) } else { Ok(false) })?;
                        anyhow::Ok((c, buffer))
                    };
                    let now = Instant::now();
                    match reached.await {
                        Ok((c, mut buffer)) => {
                            // The recorder always starts with instant replay on; it stays off if it was.
                            if buffer && !want_buffer && ask(c.replay_buffer().stop()).await.is_ok() {
                                buffer = false;
                            }
                            // A restarted recorder starts with the mic muted already (its scene says
                            // so); muted again here in case that didn't take.
                            if MIC_MUTED.load(Ordering::Relaxed)
                                && let Err(e) = set_mics(&c, true).await
                            {
                                log::write(&format!("Couldn't mute the mic again on the restarted recorder: {e:#}"));
                            }
                            send(Event::State { connected: true, buffer });
                            client = Some(c);
                            // A new recorder run: instant replay failing to start again is news again.
                            (connected_at, revive_tries, reported_stuck, unreachable_since, look) = (Instant::now(), 0, false, None, Backoff::new());
                            // A (re)started recorder shows the chosen monitor again.
                            follow = cfg.follows_mouse().then(|| recorder::chosen_monitor(cfg)).flatten().map(|m| Follow::new(m.id));
                            reported_no_connect = false;
                            // The buffer often starts a moment after OBS connects; the header shows its live state.
                            Ok(Some("Connected to the recorder.".into()))
                        }
                        Err(_) => {
                            let since = *unreachable_since.get_or_insert(now);
                            // Whether the recorder's process is gone, looked at less often the longer
                            // it can't be reached (each look starts tasklist).
                            let gone = (!QUITTING.load(Ordering::Relaxed) && last_launch.elapsed() > Duration::from_secs(20) && look.due(now)).then(|| {
                                look.looked(now);
                                !recorder::is_running()
                            });
                            match gone {
                                // Recorder died (or was closed from its tray icon): bring it back, at most every 20 s.
                                Some(true) => {
                                    with_session(|s| {
                                        s.restarts += 1;
                                        recorder_restarted(s);
                                    });
                                    if want_buffer && !told_stopped {
                                        told_stopped = true;
                                        toast::show(toast::Kind::Warning, "Instant replay stopped", "The recorder closed. Restarting it".into());
                                    }
                                    // Set first: a restart that fails waits 20 s before the next try.
                                    (last_launch, unreachable_since) = (Instant::now(), None);
                                    recorder::restart(cfg)?;
                                    Ok(Some("Recorder stopped, restarting it...".into()))
                                }
                                // Running but stuck: closed (after a save or recording it was writing
                                // is finished, up to recorder::SAVE_WAIT) and started again.
                                Some(false) if rescue.hung(since, now) => {
                                    rescue.restarted(now);
                                    with_session(|s| {
                                        s.restarts += 1;
                                        recorder_restarted(s);
                                    });
                                    if want_buffer && !told_stopped {
                                        told_stopped = true;
                                        toast::show(toast::Kind::Warning, "Instant replay stopped", "The recorder stopped answering. Restarting it".into());
                                    }
                                    log::write(&format!("The recorder is running but hasn't answered for {} s; closing it and starting it again.", now.saturating_duration_since(since).as_secs()));
                                    last_launch = Instant::now();
                                    recorder::restart(cfg)?;
                                    // Closing a stuck recorder can take a while; its replacement gets its own start time.
                                    (last_launch, unreachable_since) = (Instant::now(), None);
                                    Ok(Some("The recorder stopped answering, restarting it...".into()))
                                }
                                // Running but never answering: usually another app holds the port.
                                _ if !reported_no_connect && last_launch.elapsed() > Duration::from_secs(30) => {
                                    reported_no_connect = true;
                                    let why = recorder::last_obs_error().map(|l| format!(" OBS says: {l}")).unwrap_or_default();
                                    anyhow::bail!("Can't reach the recorder on port {}. Another app may be using that port, or the recorder didn't start.{why}", recorder::PORT)
                                }
                                _ => Ok(None),
                            }
                        }
                    }
                }
            }
        }));
        if let Err(e) = &result
            && e.is::<NoAnswer>()
        {
            client = None;
            unreachable_since.get_or_insert_with(Instant::now);
            send(Event::State { connected: false, buffer: false });
        }
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

/// Brings a just-saved clip's sound back to the middle if a track is louder on one side all the
/// way through (a balance setting in Windows or another app, a one-sided mic), so it plays even
/// for whoever it's shared with. Stays stereo. On any failure the clip is kept as it was.
/// `energy` is the clip's channel_energy.
fn center_clip(path: &std::path::Path, energy: &[(f64, f64)]) {
    // The fixed copy is an MP4; never put one under another kind's name (our recorder writes MP4).
    if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("mp4")) {
        return;
    }
    let tmp = path.with_extension("centering.mp4");
    // rename replaces the original in one step on Windows, so there's never a moment with neither.
    // Something can hold the new clip for a moment (antivirus, Explorer's thumbnails), so retry.
    let swap = || {
        let mut tries = 0;
        loop {
            match std::fs::rename(&tmp, path) {
                Err(e) if tries < 10 && e.kind() == std::io::ErrorKind::PermissionDenied => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(500));
                }
                r => return r,
            }
        }
    };
    match media::center(path, &tmp, energy, MIC_MIXED.load(Ordering::Relaxed)).and_then(|fixed| if fixed { Ok(swap().map(|()| true)?) } else { Ok(false) }) {
        Ok(true) => log::write(&format!("Centered a clip's sound (one side was louder): {}", path.display())),
        Ok(false) => {}
        Err(e) => {
            if path.exists() {
                let _ = std::fs::remove_file(&tmp);
            }
            log::write(&format!("Couldn't center a clip's sound, kept it as saved: {e:#} ({})", path.display()));
        }
    }
}

/// `secs` before now, or now if the PC hasn't been on that long (Instant can't go before boot).
fn ago(secs: u64) -> Instant {
    Instant::now().checked_sub(Duration::from_secs(secs)).unwrap_or_else(Instant::now)
}

fn clip_saved(path: &std::path::Path, events: &Sender<Event>, repaint: &Repaint) {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    feedback("Clip saved", name, Some(path));
    let msg = format!("Clip saved: {}", path.display());
    log::write(&msg);
    let _ = events.send(Event::Status(msg));
    repaint();
}

/// What's wrong with a just-saved clip, if anything: missing or empty, unreadable, under a second,
/// fewer sound tracks than the recorder writes, or track 1 (the full mix) silent from start to
/// end (no sound at all reached the recorder; quiet sound still counts). `energy` is the clip's
/// channel_energy, when it could be read.
fn check_clip(path: &std::path::Path, tracks: usize, energy: Option<&[(f64, f64)]>) -> Option<String> {
    if std::fs::metadata(path).map_or(0, |m| m.len()) == 0 {
        return Some("the file is missing or empty.".into());
    }
    let Ok(len) = media::duration(path) else { return Some("it can't be opened.".into()) };
    if len < 1.0 {
        return Some("it's under a second long.".into());
    }
    let got = media::audio_tracks(path).unwrap_or(0);
    if got < tracks {
        return Some(format!("it has {got} of {tracks} sound tracks."));
    }
    if energy.is_some_and(|e| e.first().is_none_or(|&(l, r)| l + r == 0.0)) {
        return Some("it has no sound at all. If something was playing, check the Audio page.".into());
    }
    None
}

/// File names (lowercase) of the last saved clips handled, as the recorder named them.
static HANDLED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Marks the clip the recorder saved at `path` as handled; false if it already was (reported, and
/// found in the clips folder too).
fn claim(path: &std::path::Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let mut handled = HANDLED.lock().unwrap();
    if handled.contains(&name) {
        return false;
    }
    if handled.len() >= 64 {
        handled.remove(0);
    }
    handled.push(name);
    true
}

/// `s` with every digit as 9 and every letter as a.
fn shape(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_digit() { '9' } else if c.is_alphabetic() { 'a' } else { c }).collect()
}

/// Whether `path` is a video named the way the recorder names clips, by the date alone in one of
/// `recorder::NAME_STYLES` ("2026-11-06 18-42-33.mp4"): one `place` hasn't put a game in front of.
fn named_by_recorder(path: &std::path::Path) -> bool {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let stem = shape(&path.file_stem().unwrap_or_default().to_string_lossy());
    matches!(ext.as_str(), "mp4" | "mkv" | "mov") && recorder::NAME_STYLES.iter().any(|(_, example)| stem == shape(example))
}

/// Clips the recorder wrote into the clips folder after `since` that are still as it named them:
/// what a missed saved-clip report leaves. Only the folder itself is looked at, where the recorder
/// writes; game folders only get clips that were handled already.
fn unplaced_clips(folder: &std::path::Path, since: std::time::SystemTime) -> Vec<std::path::PathBuf> {
    let mut found: Vec<_> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| named_by_recorder(&e.path()) && e.metadata().is_ok_and(|m| m.is_file() && m.modified().is_ok_and(|t| t > since)))
        .map(|e| e.path())
        .collect();
    found.sort();
    found
}

/// A clip the recorder saved, reported or found in the clips folder: counted, moved into place
/// and checked, with the popup. Handled once however often it turns up.
fn handle_saved(path: std::path::PathBuf, events: &Sender<Event>, repaint: &Repaint) {
    if !claim(&path) {
        return;
    }
    SAVED.fetch_add(1, Ordering::Relaxed);
    recorder::save_done();
    if !is_our_clip(&path) {
        log::write(&format!("Ignored a saved-clip report outside the clips folder: {}", path.display()));
        return;
    }
    with_session(|s| s.clips += 1);
    let (events, repaint) = (events.clone(), repaint.clone());
    // Moving and checking take a moment; don't hold up the event stream.
    std::thread::spawn(move || {
        let path = place(&path);
        clip_saved(&path, &events, &repaint);
        // After the popup, so a long clip doesn't hold it up. One pass over
        // the sound serves both centering and the check, at background
        // priority (CPU and disk) so a game in front keeps its frames.
        unsafe {
            use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN};
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN);
        }
        let energy = media::channel_energy(&path).inspect_err(|e| log::write(&format!("Couldn't read a clip's sound: {e:#} ({})", path.display()))).ok();
        if let Some(e) = &energy
            && CENTER_SOUND.load(Ordering::Relaxed)
        {
            center_clip(&path, e);
        }
        if let Some(problem) = check_clip(&path, TRACKS.load(Ordering::Relaxed) as usize, energy.as_deref()) {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            toast::show(toast::Kind::Warning, "Check this clip", format!("{name}: {problem}"));
            let _ = events.send(Event::Status(format!("Check this clip: {problem} ({})", path.display())));
            log::write(&format!("Saved clip problem: {problem} ({})", path.display()));
            repaint();
        }
    });
}

/// Handles the clips in `found` the recorder has finished writing, as saved clips whose report was
/// missed; returns those it's still writing.
fn handle_found(found: Vec<std::path::PathBuf>, events: &Sender<Event>, repaint: &Repaint) -> Vec<std::path::PathBuf> {
    let (writing, done): (Vec<_>, Vec<_>) = found.into_iter().partition(|p| recorder::held_for_writing(p));
    for path in done {
        log::write(&format!("Found a saved clip whose report was missed: {}", path.display()));
        handle_saved(path, events, repaint);
    }
    writing
}

/// Listens for saved clips, whether from OBS's own hotkey or the Save button, and shows the popup.
fn watch_saves(password: String, events: Sender<Event>, repaint: Repaint) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async move {
        // Until when every saved clip was surely reported to us.
        let mut listened = std::time::SystemTime::now();
        loop {
            if let Ok(Ok(client)) = tokio::time::timeout(CONNECT_WITHIN, Client::connect("127.0.0.1", recorder::PORT, Some(password.as_str()))).await
                && let Ok(mut stream) = client.events().map(Box::pin) {
                // Clips saved while nobody listened (the recorder restarting, say). A few seconds
                // back, for file times; ones still being written are reported when done.
                let since = listened.checked_sub(Duration::from_secs(5)).unwrap_or(listened);
                handle_found(unplaced_clips(&clips_folder(), since), &events, &repaint);
                let mut health = tokio::time::interval(Duration::from_secs(5));
                loop {
                    tokio::select! {
                        ev = stream.next() => match ev {
                            Some(obws::events::Event::ReplayBufferSaved { path }) => handle_saved(path, &events, &repaint),
                            Some(_) => {}
                            None => break,
                        },
                        // The stream stays open after OBS goes away (e.g. a restart); notice and reconnect.
                        _ = health.tick() => {
                            let checked = std::time::SystemTime::now();
                            if ask(client.general().version()).await.is_err() {
                                break;
                            }
                            listened = checked;
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{Backoff, FOLLOW_DELAY, Follow, Frames, Rescue, Revive, revive_step};
    use std::time::{Duration, Instant};

    /// Instant replay that won't come back on gets 3 tries 20 s apart, then a recorder restart
    /// whenever one is due (never giving up), and the tries start over after it.
    #[test]
    fn watchdog_restarts_after_three_tries() {
        let s = Duration::from_secs;
        assert_eq!(revive_step(0, s(60), false), Revive::TurnOn);
        assert_eq!(revive_step(1, s(5), true), Revive::Wait, "20 s between tries");
        assert_eq!(revive_step(2, s(21), true), Revive::TurnOn);
        assert_eq!(revive_step(3, s(21), true), Revive::Restart);
        assert_eq!(revive_step(3, s(21), false), Revive::Wait, "a restart only when one is due");
        assert_eq!(revive_step(7, s(3600), true), Revive::Restart);
        // With the spacing of restarts: the first at once, the next 3 minutes later.
        let t0 = Instant::now();
        let mut r = Rescue::default();
        assert_eq!(revive_step(3, s(30), r.due(t0)), Revive::Restart);
        r.restarted(t0);
        assert_eq!(revive_step(3, s(30), r.due(t0 + s(60))), Revive::Wait);
        assert_eq!(revive_step(3, s(30), r.due(t0 + s(180))), Revive::Restart);
    }

    /// While the recorder can't be reached, its process is looked at after 2, 4, 8, 16 s, then
    /// every 30 s; not again before each look is due.
    #[test]
    fn process_looks_back_off() {
        let t0 = Instant::now();
        let mut b = Backoff::new();
        assert!(b.due(t0), "the first look is right away");
        let mut at = t0;
        let mut gaps = Vec::new();
        for _ in 0..7 {
            b.looked(at);
            let next = b.next.unwrap();
            assert!(!b.due(next - Duration::from_millis(1)));
            assert!(b.due(next));
            gaps.push(next.duration_since(at).as_secs());
            at = next;
        }
        assert_eq!(gaps, [2, 4, 8, 16, 30, 30, 30]);
    }

    /// A recorder that's running but silent is restarted once it's been silent for a minute, then
    /// no sooner than 3, 6, 12, 24, 30, 30 minutes after each restart that didn't help; once it
    /// works, the next wait is the shortest again.
    #[test]
    fn stuck_recorder_restarts_spaced_out() {
        let t0 = Instant::now();
        let s = |secs: u64| t0 + Duration::from_secs(secs);
        let mut r = Rescue::default();
        assert!(!r.hung(t0, s(59)), "under a minute silent");
        assert!(r.hung(t0, s(60)));
        // A silence that started after "now" (clock order) never underflows.
        assert!(!r.hung(s(100), s(60)));
        let mut at = s(60);
        let mut gaps = Vec::new();
        for _ in 0..6 {
            r.restarted(at);
            let gap = r.gap();
            assert!(!r.due(at + gap - Duration::from_secs(1)) && r.due(at + gap));
            // Silent again for long enough, but the gap rules.
            assert!(!r.hung(t0, at + gap - Duration::from_secs(1)));
            gaps.push(gap.as_secs() / 60);
            at += gap;
        }
        assert_eq!(gaps, [3, 6, 12, 24, 30, 30]);
        r.restarted(at);
        r.worked();
        assert_eq!(r.gap().as_secs(), 180);
        assert!(!r.due(at + Duration::from_secs(179)), "never more than once in 3 minutes");
        assert!(r.due(at + Duration::from_secs(180)));
    }

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

    /// Turning instant replay on when it's on (or off when it's off) counts as done; any other
    /// refusal doesn't.
    #[test]
    fn replay_already_in_that_state() {
        use obws::error::Error;
        use obws::responses::StatusCode;
        let api = |code| Error::Api { code, message: None };
        assert!(super::already(true, &api(StatusCode::OutputRunning)));
        assert!(super::already(false, &api(StatusCode::OutputNotRunning)));
        assert!(!super::already(true, &api(StatusCode::OutputNotRunning)));
        assert!(!super::already(false, &api(StatusCode::OutputRunning)));
        assert!(!super::already(true, &api(StatusCode::OutputDisabled)));
        assert!(!super::already(true, &Error::Disconnected));
    }

    /// Only videos named by the date alone, in either name style, are the recorder's own unplaced
    /// clips; a game in front of the name, a copy or a screenshot isn't.
    #[test]
    fn recorder_names() {
        use std::path::Path;
        for yes in ["2026-11-06 18-42-33.mp4", "11-06-2026 06-42-33 PM.mp4", "11-06-2026 06-42-33 AM.MKV", "2026-01-02 03-04-05.mov"] {
            assert!(super::named_by_recorder(Path::new(yes)), "{yes}");
        }
        for no in ["VALORANT 2026-11-06 18-42-33.mp4", "2026-11-06 18-42-33 (trim).mp4", "2026-11-06 18-42-33.png", "2026-11-06 18-42-33.centering.mp4", "Montage 2026-11-06 18-42-33.mp4", "2026-11-06.mp4", ""] {
            assert!(!super::named_by_recorder(Path::new(no)), "{no}");
        }
    }

    /// The clips folder is looked at for the recorder's own clips newer than a time: not older ones,
    /// placed ones, other files, or game folders.
    #[test]
    fn finds_unplaced_clips() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let dir = std::env::temp_dir().join(format!("rv-unplaced-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("VALORANT")).unwrap();
        let new = dir.join("2026-11-06 18-42-33.mp4");
        let old = dir.join("2026-11-06 17-00-00.mp4");
        for f in [&new, &old, &dir.join("VALORANT 2026-11-06 18-42-34.mp4"), &dir.join("2026-11-06 18-42-35.png"), &dir.join("VALORANT").join("2026-11-06 18-42-36.mp4")] {
            std::fs::write(f, b"x").unwrap();
        }
        let hour_ago = SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::options().write(true).open(&old).unwrap().set_modified(hour_ago - Duration::from_secs(60)).unwrap();
        assert_eq!(super::unplaced_clips(&dir, hour_ago), std::slice::from_ref(&new));
        assert_eq!(super::unplaced_clips(&dir, UNIX_EPOCH), [old, new]);
        assert!(super::unplaced_clips(&dir, SystemTime::now() + Duration::from_secs(60)).is_empty());
        assert!(super::unplaced_clips(&dir.join("gone"), UNIX_EPOCH).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A clip both reported and found in the folder is handled once.
    #[test]
    fn saved_clip_handled_once() {
        let p = std::path::Path::new(r"C:\clips\2001-01-01 00-00-01.mp4");
        assert!(super::claim(p));
        assert!(!super::claim(std::path::Path::new(r"C:/clips/2001-01-01 00-00-01.MP4")));
        assert!(super::claim(std::path::Path::new(r"C:\clips\2001-01-01 00-00-02.mp4")));
    }

    #[test]
    fn encoder_struggles_at_one_percent_skipped() {
        let f = |encoded, encode_skipped| Frames { encoded, encode_skipped, ..Default::default() };
        assert!(f(3000, 30).encoder_struggling());
        assert!(!f(3001 * 100, 3000).encoder_struggling());
        // A short hiccup at the start isn't worth a note.
        assert!(!f(100, 29).encoder_struggling());
    }

    /// Real clips pass the saved-clip check (read only); an empty file and a missing one don't.
    #[test]
    fn saved_clip_check() {
        let dir = std::env::temp_dir().join("rv-check-clip");
        let _ = std::fs::create_dir_all(&dir);
        let empty = dir.join("empty.mp4");
        std::fs::write(&empty, b"").unwrap();
        assert!(super::check_clip(&empty, 1, None).is_some());
        assert!(super::check_clip(&dir.join("gone.mp4"), 1, None).is_some());
        let junk = dir.join("junk.mp4");
        std::fs::write(&junk, b"not a video").unwrap();
        assert!(super::check_clip(&junk, 1, None).is_some());
        let _ = std::fs::remove_dir_all(&dir);
        let folder = crate::config::Config::load().folder;
        for c in crate::clips::list(&folder, 5) {
            let energy = crate::media::channel_energy(&c.path).ok();
            let got = super::check_clip(&c.path, 1, energy.as_deref());
            assert!(got.is_none() || got.as_deref().is_some_and(|p| p.contains("no sound")), "{}: {got:?}", c.path.display());
            println!("{}: {got:?}", c.path.display());
        }
    }
}
