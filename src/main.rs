#![windows_subsystem = "windows"]

mod clips;
mod config;
mod covers;
mod devices;
mod display;
mod hotkey;
mod log;
mod media;
mod obs;
mod obs_import;
mod obs_install;
mod overlay;
mod perf;
mod phone;
mod profile;
mod recorder;
mod startup;
mod support;
mod player;
mod toast;
mod tweaks;
mod upload;

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui::{self, Color32, RichText};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use config::Config;
use obs::{Cmd, Event};

/// Accent colors: name, main, light (text and marks), dim (pressed and disabled fills).
const ACCENTS: [(&str, Color32, Color32, Color32); 6] = [
    ("Purple", Color32::from_rgb(132, 72, 240), Color32::from_rgb(178, 136, 255), Color32::from_rgb(64, 38, 112)),
    ("Blue", Color32::from_rgb(52, 120, 246), Color32::from_rgb(124, 172, 255), Color32::from_rgb(28, 58, 112)),
    ("Green", Color32::from_rgb(34, 160, 92), Color32::from_rgb(110, 216, 150), Color32::from_rgb(20, 78, 48)),
    ("Orange", Color32::from_rgb(226, 112, 28), Color32::from_rgb(255, 170, 100), Color32::from_rgb(108, 54, 18)),
    ("Pink", Color32::from_rgb(222, 62, 148), Color32::from_rgb(255, 132, 196), Color32::from_rgb(104, 30, 72)),
    ("Red", Color32::from_rgb(214, 54, 54), Color32::from_rgb(255, 122, 122), Color32::from_rgb(102, 28, 28)),
];
/// The accent in use (an index into ACCENTS), set from the settings every frame.
static ACCENT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

fn accent_of() -> &'static (&'static str, Color32, Color32, Color32) {
    &ACCENTS[ACCENT.load(std::sync::atomic::Ordering::Relaxed) as usize % ACCENTS.len()]
}
fn accent() -> Color32 {
    accent_of().1
}
fn accent_light() -> Color32 {
    accent_of().2
}
fn accent_dim() -> Color32 {
    accent_of().3
}
/// `c` see-through, for glows.
fn alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}
/// Neutral charcoal greys, darkest (sidebar) to lightest (raised controls).
const SIDEBAR: Color32 = Color32::from_rgb(14, 14, 17);
const BG: Color32 = Color32::from_rgb(20, 20, 24);
const BAR: Color32 = SIDEBAR;
const CARD: Color32 = Color32::from_rgb(28, 28, 34);
const RAISED: Color32 = Color32::from_rgb(38, 38, 46);
const BORDER: Color32 = Color32::from_rgb(42, 42, 50);
const MUTED: Color32 = Color32::from_rgb(142, 142, 154);
const AMBER: Color32 = Color32::from_rgb(230, 170, 40);
const RED: Color32 = Color32::from_rgb(220, 70, 60);

fn main() -> eframe::Result {
    // Started by Windows at login: stay in the tray.
    let minimized = std::env::args().any(|a| a == "--minimized");
    // Only one copy at a time: a second launch brings up the running one and quits.
    if !first_instance() {
        if !minimized {
            overlay::show_existing();
        }
        return Ok(());
    }
    log::catch_panics();
    log::write(&format!("App started (version {}).", env!("CARGO_PKG_VERSION")));
    startup::repair();
    // Draw the UI with DirectX 12, not Vulkan: Vulkan loads every installed overlay layer
    // (OBS, Overwolf, Steam, Epic) into this process. Safe here: no other threads exist yet.
    unsafe { std::env::set_var("WGPU_BACKEND", "dx12") };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 700.0])
            .with_min_inner_size([860.0, 560.0])
            .with_visible(false),
        ..Default::default()
    };
    eframe::run_native("Replay Vault", options, Box::new(move |cc| Ok(Box::new(App::new(cc, minimized)))))
}

/// Holds a named mutex for the life of the process; false if another copy already holds it.
fn first_instance() -> bool {
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows::Win32::System::Threading::CreateMutexW;
    // The handle is never closed: Windows releases the mutex when this process exits.
    match unsafe { CreateMutexW(None, true, windows::core::w!("Local\\ReplayVault")) } {
        Ok(_) => (unsafe { GetLastError() }) != ERROR_ALREADY_EXISTS,
        Err(_) => true,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    General,
    Clips,
    Keybinds,
    Video,
    Audio,
    Logs,
}

/// Which hotkey is being set.
#[derive(Clone, Copy, PartialEq)]
enum Target {
    Save,
    Save30,
    Save60,
    Overlay,
    Record,
    Screenshot,
    Replay,
    Mic,
}

impl Target {
    const ALL: [Target; 8] =
        [Target::Save, Target::Save30, Target::Save60, Target::Overlay, Target::Record, Target::Screenshot, Target::Replay, Target::Mic];

    /// The app's own RegisterHotKey id; None for the save key, which belongs to OBS.
    fn action(self) -> Option<hotkey::Action> {
        Some(match self {
            Target::Save => return None,
            Target::Save30 => hotkey::Action::Save30,
            Target::Save60 => hotkey::Action::Save60,
            Target::Overlay => hotkey::Action::Overlay,
            Target::Record => hotkey::Action::Record,
            Target::Screenshot => hotkey::Action::Screenshot,
            Target::Replay => hotkey::Action::Replay,
            Target::Mic => hotkey::Action::Mic,
        })
    }
}

/// What the Clips page shows.
#[derive(Clone, PartialEq)]
enum ClipsView {
    /// Home: the newest clips in a row, then a card per game folder (and one for clips not in any).
    Games,
    All,
    /// One folder's clips; None is the clips folder itself, for clips not sorted into a game.
    Game(Option<String>),
    /// Clips with this tag.
    Tag(String),
}

/// One clip as the grid shows it.
struct ClipItem {
    path: std::path::PathBuf,
    name: String,
    bytes: u64,
    modified: std::time::SystemTime,
    is_picture: bool,
    favorite: bool,
    tags: Vec<String>,
}

/// A game folder (None: clips not sorted into one) and its clips.
struct Game {
    name: Option<String>,
    count: usize,
    bytes: u64,
    /// Its newest video, for the picture until there's a cover.
    thumb: Option<std::path::PathBuf>,
}

/// What was clicked in the grid this frame, acted on after drawing it.
#[derive(Default)]
struct ClipActions {
    play: Option<std::path::PathBuf>,
    trim: Option<std::path::PathBuf>,
    recycle: Option<std::path::PathBuf>,
    star: Option<std::path::PathBuf>,
    export: Option<(std::path::PathBuf, media::Share)>,
    /// A tag put on (true) or taken off a clip.
    tag: Option<(std::path::PathBuf, String, bool)>,
    /// A clip added to or taken out of the montage.
    pick: Option<std::path::PathBuf>,
}

/// A new file (trim, share copy, GIF, montage) being made in the background: how far along
/// (percent), then the new file.
#[derive(Default)]
struct ExportJob {
    pct: std::sync::atomic::AtomicU32,
    done: std::sync::Mutex<Option<Result<std::path::PathBuf, String>>>,
    /// Asks it to stop (montages only).
    stop: std::sync::atomic::AtomicBool,
}

impl ExportJob {
    /// Makes `out` with `make` on a background thread. A failed try's file is removed; a made one
    /// goes in the log as "`what` <out>". Repaints when done.
    fn spawn(out: std::path::PathBuf, what: String, ctx: &egui::Context, make: impl FnOnce(&ExportJob, &std::path::Path) -> anyhow::Result<()> + Send + 'static) -> Arc<Self> {
        let job = Arc::new(ExportJob::default());
        let (slot, ctx) = (job.clone(), ctx.clone());
        std::thread::spawn(move || {
            let r = make(&slot, &out).map(|()| out.clone()).map_err(|e| {
                let _ = std::fs::remove_file(&out);
                format!("{e:#}")
            });
            if r.is_ok() {
                log::write(&format!("{what} {}", out.display()));
            }
            *slot.done.lock().unwrap() = Some(r);
            ctx.request_repaint();
        });
        job
    }

    /// Stores how far along it is, 0 to 1.
    fn progress(&self, f: f32) {
        self.pct.store((f * 100.0) as u32, std::sync::atomic::Ordering::Relaxed);
    }
}

/// "Saved Clip (GIF).gif (2.1 MB).", for a file just made.
fn saved_note(out: &std::path::Path) -> String {
    let size = std::fs::metadata(out).map(|m| clips::size(m.len())).unwrap_or_default();
    format!("Saved {} ({size}).", out.file_name().unwrap_or_default().to_string_lossy())
}

/// Montage mode on the Clips page: the clips picked, in the order they play, and how they're put
/// together.
struct Montage {
    clips: Vec<std::path::PathBuf>,
    fades: bool,
    music: Option<std::path::PathBuf>,
    /// The music's volume, 0 to 1.
    volume: f32,
    job: Option<Arc<ExportJob>>,
}

/// The share presets in the clip menu, with a hint for each.
const SHARES: [(media::Share, &str, &str); 4] = [
    (media::Share::Discord10, "Fit for Discord (up to 10 MB)", "Small enough for Discord without Nitro. Best for clips under a minute."),
    (media::Share::Discord50, "Fit for Discord (up to 50 MB)", "For Discord servers with a 50 MB limit."),
    (media::Share::P720, "720p", "A smaller copy at 1280x720 that still looks sharp."),
    (media::Share::Vertical, "Vertical for TikTok / Shorts", "The middle of the picture, cut to 9:16."),
];

/// A clip being trimmed: the range picked, and the running cut once started.
struct TrimEdit {
    path: std::path::PathBuf,
    len: f64,
    start: f64,
    end: f64,
    /// The part of the clip the timeline shows: (from, length) in seconds. Ctrl+scroll zooms.
    view: (f64, f64),
    /// Volume per audio track (0 to 2). Starts as track 1 only, which is what the clip plays as.
    gains: Vec<f32>,
    /// "Record game and mic apart" was pressed.
    want_split: bool,
    /// The clip's loudness for the waveform (media::peaks), once worked out in the background.
    wave: Arc<std::sync::Mutex<Option<Vec<f32>>>>,
    job: Option<Arc<ExportJob>>,
    /// Parts kept with Add part (start, end), in time order, shaded on the timeline. With any,
    /// Save joins them into one clip.
    parts: Vec<(f64, f64)>,
    /// A GIF of the selection being made, then how that went.
    gif: Option<Arc<ExportJob>>,
    gif_msg: Option<String>,
    /// Loud moments in the wave (media::loud_moments), worked out once it's there.
    loud: Option<Vec<f64>>,
}

struct App {
    cfg: Config,
    cmds: Sender<Cmd>,
    events: Receiver<Event>,
    status: String,
    connected: bool,
    buffer: bool,
    start_with_windows: bool,
    devices: Vec<devices::Device>,
    /// Open apps to pick for "Only these apps", looked up with the devices.
    open_apps: Vec<config::AppSel>,
    monitors: Vec<display::Monitor>,
    /// Settings the recorder is currently running with, to flag unapplied changes.
    applied: String,
    hotkey_error: Option<String>,
    capturing: Option<Target>,
    tab: Tab,
    /// The settings as last written to disk; the Save bar shows while `cfg` differs.
    saved: Config,
    started: std::time::Instant,
    minimized: bool,
    tray: Option<TrayIcon>,
    /// A full recording is running, and the look the tray icon was last given.
    recording: bool,
    /// The tray icon as drawn: its look, and the accent it was drawn in.
    tray_look: Option<(TrayLook, u8)>,
    /// Current first-run wizard step; `None` once setup is finished.
    wizard: Option<usize>,
    /// The OBS the recorder will use, re-checked after each install job.
    obs_exe: Option<std::path::PathBuf>,
    /// Running OBS or VC++ runtime download.
    job: Option<obs_install::Shared>,
    job_error: Option<String>,
    /// Last lines of the app log for the Logs tab, and the file size/time they were read at.
    log_view: Vec<String>,
    log_stamp: (u64, Option<std::time::SystemTime>),
    log_checked: std::time::Instant,
    /// This PC's GPU and RAM, for the recommended settings.
    hw: profile::Hardware,
    /// Summary of the recommended settings last applied.
    recommended: Option<String>,
    /// Whether our firewall rule exists; checked once when the OBS setup step shows.
    firewall_ok: Option<bool>,
    /// The user has their own OBS setup to import from, and what was last imported.
    obs_settings_found: bool,
    imported: Option<String>,
    /// Clips tab: the folder's clips (re-read every few seconds), their frames and lengths.
    clip_list: Vec<clips::Clip>,
    clips_checked: std::time::Instant,
    previews: clips::Previews,
    /// Frame textures by (path, tenths of a second); None when the clip can't be decoded.
    textures: std::collections::HashMap<(std::path::PathBuf, u32), Option<egui::TextureHandle>>,
    /// Game covers by game; None when the file can't be read.
    covers: std::collections::HashMap<String, Option<egui::TextureHandle>>,
    /// The first of the Recent clips shown (the row's arrows move it).
    recent_at: usize,
    lengths: std::collections::HashMap<std::path::PathBuf, f64>,
    /// The hover-scrub frame being decoded (path, seconds); one at a time.
    scrub_wait: Option<(std::path::PathBuf, f64)>,
    trim: Option<TrimEdit>,
    clips_msg: Option<String>,
    /// The clip playing in the window, if any.
    player: Option<(std::path::PathBuf, player::Player)>,
    /// What the Clips page shows: the games, everything, or one folder.
    view: ClipsView,
    /// Share links, running uploads/deletes, and a clip waiting for the upload notice to be accepted.
    links: std::collections::BTreeMap<String, upload::Link>,
    uploads: std::collections::HashMap<std::path::PathBuf, Arc<upload::Job>>,
    confirm_upload: Option<std::path::PathBuf>,
    /// Running share exports, and the last one done (its message and file, for the Copy button).
    exports: std::collections::HashMap<std::path::PathBuf, Arc<ExportJob>>,
    exported: Option<(String, std::path::PathBuf)>,
    confirm_remove: bool,
    /// A settings file picked with Import settings, waiting for its changes to be confirmed, and
    /// its folders that aren't on this PC (looked for once: a NAS that's off can take seconds).
    import: Option<(config::Export, Vec<String>)>,
    /// Starred clips, by path.
    favorites: std::collections::BTreeSet<String>,
    /// The clip being renamed and the name typed so far.
    renaming: Option<(std::path::PathBuf, String)>,
    /// Tags by clip path, and the clip getting a new tag with the name typed so far.
    tags: clips::Tags,
    tagging: Option<(std::path::PathBuf, String)>,
    /// The clip picked with a click or the arrow keys, for Enter, F2 and Delete.
    selected: Option<std::path::PathBuf>,
    /// Clips page search text.
    search: String,
    /// Montage mode, while it's on.
    montage: Option<Montage>,
    /// What auto-cleanup would remove right now (clips, bytes), for the rule it was worked out for.
    cleanup_preview: Option<(clips::Cleanup, std::time::Instant, usize, u64)>,
    /// What the archive would move right now (clips, bytes), for the rule it was worked out for.
    archive_preview: Option<(clips::Archive, std::time::Instant, usize, u64)>,
    /// CPU, GPU and memory readings; only while the Logs page is open.
    perf: Option<perf::Sampler>,
    /// OBS skipped frames while encoding: how many, at which Encoder effort. Until dismissed.
    encoder_hint: Option<(u64, u8)>,
    /// How far the app has slimmed down since the window was hidden.
    slim: Slim,
    /// The phone page and the (folder, key, addresses) it was started for, or why it couldn't start.
    phone: Option<(PhoneFor, Result<phone::Server, String>)>,
    /// This PC's addresses for the phone page, and when they were last looked at.
    phone_ips: (std::time::Instant, Vec<std::net::IpAddr>),
}

/// What the phone page was started for: folder, key, addresses.
type PhoneFor = (String, String, Vec<std::net::IpAddr>);

#[derive(Clone, Copy, PartialEq)]
enum Slim {
    Visible,
    /// Thumbnails dropped; their GPU memory goes with the next paint.
    Freed,
    /// Idle memory handed back to Windows.
    Trimmed,
}

/// Hands the app's idle memory pages back to Windows, so a game can use the RAM. They come back
/// on their own when the window opens again.
fn trim_memory() {
    use windows::Win32::System::ProcessStatus::K32EmptyWorkingSet;
    use windows::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        let _ = K32EmptyWorkingSet(GetCurrentProcess());
    }
}

/// Set from the hotkey thread to hide the window on the UI thread, so a playing clip is paused
/// first (the player can only be touched from the UI thread).
static HIDE_REQUEST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static PLAYING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

const STEPS: [&str; 6] = ["Welcome", "OBS", "Clips & hotkeys", "Video", "Audio", "Done"];

impl App {
    fn new(cc: &eframe::CreationContext, minimized: bool) -> Self {
        apply_theme(&cc.egui_ctx);
        if let Ok(RawWindowHandle::Win32(h)) = cc.window_handle().map(|h| h.as_raw()) {
            overlay::set_window(h.hwnd.get());
        }
        let new_user = !config::exists();
        let mut cfg = Config::load();
        // A brand-new user starts from settings that suit their PC.
        let hw = profile::detect();
        let mut recommended = None;
        if new_user {
            if let Some(mon) = recorder::chosen_monitor(&cfg) {
                let s = profile::apply(&mut cfg, &hw, &mon);
                log::write(&format!("Detected {}. Recommended settings: {s}", hw.summary()));
                recommended = Some(s);
            }
        }
        // Before anything looks for OBS: the private copy may live on a drive the user picked.
        obs_install::set_install_root(&cfg.obs_dir);
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ev_tx, ev_rx) = mpsc::channel();

        // A newer OBS downloaded last session goes in now, before the recorder starts.
        let updated = obs_install::apply_staged_update();
        let obs_exe = obs_install::find();
        let needs_setup = !cfg.setup_done || obs_exe.is_none();

        toast::start();
        let ctx = cc.egui_ctx.clone();
        let repaint: obs::Repaint = Arc::new(move || ctx.request_repaint());
        let (update_tx, update_repaint) = (ev_tx.clone(), repaint.clone());
        std::thread::spawn(move || obs::run(cmd_rx, ev_tx, repaint));
        // Keeps our private OBS copy current; it only ever downloads, the swap waits for a restart.
        std::thread::spawn(move || loop {
            let msg = match obs_install::check_for_update() {
                Ok(Some(v)) => Some(format!("OBS {v} downloaded. It installs the next time the app starts.")),
                Ok(None) => None,
                Err(e) => Some(format!("Couldn't check for OBS updates: {e:#}")),
            };
            if let Some(msg) = msg {
                log::write(&msg);
                let _ = update_tx.send(Event::Status(msg));
                update_repaint();
            }
            std::thread::sleep(std::time::Duration::from_secs(6 * 3600));
        });

        // The app's hotkeys work even while a game has focus.
        let ctx = cc.egui_ctx.clone();
        let hotkey_cmds = std::sync::Mutex::new(cmd_tx.clone());
        let hook = hotkey::install(move |action| {
            let cmd = match action {
                hotkey::Action::Overlay => {
                    if overlay::is_visible() && PLAYING.load(std::sync::atomic::Ordering::Relaxed) {
                        HIDE_REQUEST.store(true, std::sync::atomic::Ordering::Relaxed);
                    } else {
                        overlay::toggle();
                    }
                    ctx.request_repaint();
                    return;
                }
                hotkey::Action::Save30 => Cmd::SaveLast(30),
                hotkey::Action::Save60 => Cmd::SaveLast(60),
                hotkey::Action::Record => Cmd::ToggleRecord,
                hotkey::Action::Screenshot => Cmd::Screenshot,
                hotkey::Action::Replay => Cmd::ToggleReplay,
                hotkey::Action::Mic => Cmd::ToggleMic,
            };
            let _ = hotkey_cmds.lock().unwrap().send(cmd);
        });
        let ctx = cc.egui_ctx.clone();
        let previews = clips::Previews::new(move || ctx.request_repaint());

        let (tray, tray_error) = match make_tray(&cc.egui_ctx, cmd_tx.clone()) {
            Ok(t) => (Some(t), None),
            Err(e) => (None, Some(e)),
        };

        let mut app = Self {
            cfg,
            cmds: cmd_tx,
            events: ev_rx,
            status: "Starting...".into(),
            connected: false,
            buffer: false,
            start_with_windows: startup::is_enabled(),
            devices: Vec::new(),
            open_apps: Vec::new(),
            monitors: display::list(),
            applied: String::new(),
            hotkey_error: None,
            capturing: None,
            tab: Tab::General,
            saved: Config::default(),
            started: std::time::Instant::now(),
            // Setup needs the window, even at a Windows-startup launch.
            minimized: minimized && !needs_setup,
            tray,
            recording: false,
            tray_look: None,
            wizard: needs_setup.then_some(0),
            obs_exe,
            job: None,
            job_error: None,
            log_view: Vec::new(),
            log_stamp: (0, None),
            log_checked: std::time::Instant::now() - std::time::Duration::from_secs(2),
            hw,
            recommended,
            firewall_ok: None,
            obs_settings_found: obs_import::found(),
            imported: None,
            clip_list: Vec::new(),
            clips_checked: std::time::Instant::now() - std::time::Duration::from_secs(60),
            previews,
            textures: Default::default(),
            covers: Default::default(),
            recent_at: 0,
            lengths: Default::default(),
            scrub_wait: None,
            trim: None,
            clips_msg: None,
            player: None,
            view: ClipsView::Games,
            links: upload::links(),
            uploads: Default::default(),
            confirm_upload: None,
            exports: Default::default(),
            exported: None,
            confirm_remove: false,
            import: None,
            favorites: clips::favorites(),
            renaming: None,
            tags: clips::tags(),
            tagging: None,
            selected: None,
            search: String::new(),
            montage: None,
            cleanup_preview: None,
            archive_preview: None,
            perf: None,
            encoder_hint: None,
            slim: Slim::Visible,
            phone: None,
            phone_ips: (std::time::Instant::now() - std::time::Duration::from_secs(60), Vec::new()),
        };
        clips::set_cleanup(&app.cfg.folder, cleanup_rule(&app.cfg));
        clips::start_cleanup();
        clips::set_archive(&app.cfg.folder, archive_rule(&app.cfg));
        clips::start_archive();
        clips::set_backup(backup_rule(&app.cfg));
        clips::start_backup();
        if app.cfg.game_bar_off {
            if let Err(e) = tweaks::set_game_bar_off(true) {
                log::write(&format!("Couldn't turn Game Bar capture off: {e:#}"));
            }
        }
        if needs_setup {
            app.status.clear();
        }
        if let Some(v) = updated {
            app.status = format!("OBS was updated to {v}.");
            log::write(&app.status);
        }
        match hook {
            Ok(()) => app.register_overlay_hotkey(),
            Err(e) => app.hotkey_error = Some(format!("Could not start the hotkey listener: {e}")),
        }
        if let Some(e) = tray_error {
            app.status = format!("Could not create the tray icon: {e}");
        }
        app.refresh_devices();
        // Persists the generated recorder password (and a new user's recommended settings).
        app.write_settings();
        // The recorder starts once setup is done (OBS present, settings picked).
        if !needs_setup {
            app.apply();
        }
        if !app.minimized {
            overlay::show();
        }
        app
    }

    fn hotkey_of(&mut self, target: Target) -> &mut String {
        match target {
            Target::Save => &mut self.cfg.hotkey,
            Target::Save30 => &mut self.cfg.hotkey_30,
            Target::Save60 => &mut self.cfg.hotkey_60,
            Target::Overlay => &mut self.cfg.overlay_hotkey,
            Target::Record => &mut self.cfg.hotkey_record,
            Target::Screenshot => &mut self.cfg.hotkey_screenshot,
            Target::Replay => &mut self.cfg.hotkey_replay,
            Target::Mic => &mut self.cfg.hotkey_mic,
        }
    }

    /// Registers the app's own hotkeys (all but the save key); an empty one is off.
    fn register_overlay_hotkey(&mut self) {
        let mut errors = Vec::new();
        for target in Target::ALL {
            let Some(action) = target.action() else { continue };
            let text = self.hotkey_of(target).clone();
            let combo = hotkey::Combo::parse(&text);
            if combo.is_none() && !text.is_empty() {
                errors.push(format!("'{text}' isn't a valid hotkey. Click it to set a new one."));
            }
            // Another app (NVIDIA, Discord, ...) may hold the combo already.
            if let Err(e) = hotkey::set_combo(action, combo) {
                errors.push(format!("{}: {e}", target_name(target)));
            }
        }
        self.hotkey_error = (!errors.is_empty()).then(|| errors.join(" "));
    }

    fn start_capture(&mut self, target: Target, ctx: &egui::Context) {
        // Free the app's keys so they can be picked again.
        for a in hotkey::Action::ALL {
            let _ = hotkey::set_combo(a, None);
        }
        self.capturing = Some(target);
        let ctx = ctx.clone();
        hotkey::start_capture(move || ctx.request_repaint());
    }

    fn finish_capture(&mut self) {
        let Some(target) = self.capturing else { return };
        if hotkey::is_capturing() {
            return;
        }
        self.capturing = None;
        let captured = hotkey::take_captured();
        let Some(c) = captured else {
            self.register_overlay_hotkey();
            return;
        };
        let name = c.to_string();
        // One key, one job.
        let taken = Target::ALL
            .into_iter()
            .find(|&t| t != target && hotkey::Combo::parse(self.hotkey_of(t)) == Some(c));
        if let Some(other) = taken {
            self.register_overlay_hotkey();
            self.hotkey_error = Some(format!("{name} is already set for \"{}\". Pick another.", target_name(other)));
            return;
        }
        if target == Target::Save && c.obs_binding().is_none() {
            self.register_overlay_hotkey();
            self.hotkey_error = Some(format!("OBS can't use {c} as a hotkey. Pick another."));
            return;
        }
        *self.hotkey_of(target) = name;
        self.register_overlay_hotkey();
    }

    fn handle_events(&mut self) {
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                Event::Status(s) => self.status = s,
                Event::State { connected, buffer } => {
                    self.connected = connected;
                    self.buffer = buffer;
                }
                Event::EncoderSkipped { frames, effort } => self.encoder_hint = Some((frames, effort)),
                Event::Recording(on) => self.recording = on,
            }
        }
        // A recorder that went away took its recording with it.
        self.recording &= self.connected;
        let look = (TrayLook::of(self.connected, self.buffer, self.recording), ACCENT.load(std::sync::atomic::Ordering::Relaxed));
        if self.tray_look != Some(look) {
            self.tray_look = Some(look);
            let look = look.0;
            if let Some(tray) = &self.tray {
                let _ = tray.set_icon(tray_icon_image(look).ok());
                let _ = tray.set_tooltip(Some(look.tooltip()));
            }
        }
    }

    fn apply(&mut self) {
        self.applied = recorder_settings(&self.cfg);
        let _ = self.cmds.send(Cmd::Start(self.cfg.clone()));
    }

    /// Writes the settings to disk as they are now.
    fn write_settings(&mut self) {
        match self.cfg.save() {
            Ok(()) => self.saved = self.cfg.clone(),
            Err(e) => self.status = format!("Could not save settings: {e}"),
        }
    }

    /// The Save button: keeps the changes for next time and puts them into effect, restarting
    /// the recorder if they need it.
    fn save_settings(&mut self) {
        self.keep_settings();
        if self.cfg == self.saved && recorder_settings(&self.cfg) != self.applied {
            self.apply();
        }
    }

    /// Saves the settings and puts the Windows tweaks in them into effect.
    fn keep_settings(&mut self) {
        if self.cfg.game_bar_off != self.saved.game_bar_off {
            if let Err(e) = tweaks::set_game_bar_off(self.cfg.game_bar_off) {
                self.status = format!("Couldn't change Game Bar: {e:#}");
            }
        }
        self.write_settings();
    }

    /// Starts, restarts or stops the phone page to match the saved settings.
    /// Also restarts it when this PC's addresses change (another Wi-Fi, Tailscale coming up after
    /// the app), looked at once a minute.
    fn sync_phone(&mut self) {
        if self.saved.phone_page && self.phone_ips.0.elapsed() >= std::time::Duration::from_secs(60) {
            self.phone_ips = (std::time::Instant::now(), phone::local_ips());
        }
        let want = self.saved.phone_page.then_some((self.saved.folder.as_str(), self.saved.phone_key.as_str(), self.phone_ips.1.as_slice()));
        if want == self.phone.as_ref().map(|((f, k, ips), _)| (f.as_str(), k.as_str(), ips.as_slice())) {
            return;
        }
        // The old listener closes before the new one opens on the same port.
        self.phone = None;
        self.phone = want.map(|(folder, key, ips)| {
            let addrs: Vec<_> = ips.iter().map(|ip| std::net::SocketAddr::new(*ip, phone::PORT)).collect();
            let server = phone::start(&addrs, key, folder).map_err(|e| format!("Couldn't start the phone page: {e}."));
            if let Err(e) = &server {
                log::write(e);
            }
            ((folder.to_string(), key.to_string(), ips.to_vec()), server)
        });
    }

    /// The Discard button: back to the saved settings.
    fn discard_settings(&mut self) {
        self.cfg = self.saved.clone();
        self.register_overlay_hotkey();
    }

    /// What a fresh install picks on this PC: the defaults, tuned to its GPU, RAM and monitor.
    fn defaults(&self) -> Config {
        let mut d = Config::default();
        if let Some(mon) = recorder::chosen_monitor(&d) {
            profile::apply(&mut d, &self.hw, &mon);
        }
        d
    }

    /// Puts one settings page back to its defaults. Not saved until Save. The clip folder stays,
    /// since that's where the existing clips are.
    fn restore_defaults(&mut self, tab: Tab) {
        let d = self.defaults();
        let c = &mut self.cfg;
        match tab {
            Tab::General => {
                c.buffer_secs = d.buffer_secs;
                c.clip_prefix = d.clip_prefix;
                c.name_style = d.name_style;
                c.sort_by_game = d.sort_by_game;
                c.keep_under_gb = d.keep_under_gb;
                c.keep_days = d.keep_days;
                c.archive_days = d.archive_days;
                c.archive_folder = d.archive_folder;
                c.backup = d.backup;
                c.backup_folder = d.backup_folder;
                c.save_popup = d.save_popup;
                c.save_sound = d.save_sound;
                c.game_bar_off = d.game_bar_off;
                c.gpu_high = d.gpu_high;
                // The Discord webhook is kept: it's a link you pasted, not a setting with a default.
                c.phone_page = d.phone_page;
                c.accent = d.accent;
                c.show_tips = d.show_tips;
            }
            Tab::Keybinds => {
                c.hotkey = d.hotkey;
                c.overlay_hotkey = d.overlay_hotkey;
                c.hotkey_30 = d.hotkey_30;
                c.hotkey_60 = d.hotkey_60;
                c.hotkey_record = d.hotkey_record;
                c.hotkey_screenshot = d.hotkey_screenshot;
                c.hotkey_replay = d.hotkey_replay;
                c.hotkey_mic = d.hotkey_mic;
            }
            Tab::Video => {
                c.capture = d.capture;
                c.monitor_id = d.monitor_id;
                c.follow_mouse = d.follow_mouse;
                c.out_height = d.out_height;
                c.fps = d.fps;
                c.quality = d.quality;
                c.codec = d.codec;
                c.encoder_effort = d.encoder_effort;
            }
            Tab::Audio => {
                c.track_count = d.track_count;
                c.track_names = d.track_names;
                c.devices = d.devices;
                c.only_apps = d.only_apps;
                c.apps = d.apps;
                c.noise = d.noise;
            }
            Tab::Clips | Tab::Logs => return,
        }
        self.register_overlay_hotkey();
    }

    /// Top bar: title, Save clip, live status and the replay switch.
    /// Left side: the name and the pages, and at the bottom the live status, the replay switch
    /// and Save clip.
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        // A hairline on its right edge. The whole panel, margins too.
        let panel = ui.max_rect().expand2(egui::vec2(12.0, 0.0));
        let back = ui.ctx().layer_painter(ui.layer_id()).with_clip_rect(panel);
        back.vline(panel.right() - 0.5, panel.y_range(), egui::Stroke::new(1.0, Color32::from_rgb(30, 30, 38)));
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            // The tray icon's record dot.
            let (rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
            glow(ui.painter(), rect.center(), 22.0, alpha(accent_light(), 60));
            ui.painter().circle_filled(rect.center(), 10.0, RAISED);
            ui.painter().circle_filled(rect.center(), 6.0, accent_light());
            ui.label(strong("Replay Vault", 17.0).color(Color32::WHITE));
        });
        ui.add_space(24.0);
        self.nav_item(ui, Tab::Clips, icon::CLIPS, "Clips");
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(RichText::new("SETTINGS").size(11.0).color(MUTED));
        });
        ui.add_space(2.0);
        for (tab, glyph, name) in [
            (Tab::General, icon::SETTINGS, "General"),
            (Tab::Keybinds, icon::KEYBOARD, "Keybinds"),
            (Tab::Video, icon::VIDEO, "Video"),
            (Tab::Audio, icon::AUDIO, "Audio"),
        ] {
            self.nav_item(ui, tab, glyph, name);
        }
        ui.add_space(16.0);
        self.nav_item(ui, Tab::Logs, icon::LOG, "Logs");

        // The games, played last first, each opening its clips.
        let games = self.games();
        if !games.is_empty() {
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(RichText::new("YOUR GAMES").size(11.0).color(MUTED));
            });
            ui.add_space(2.0);
            egui::ScrollArea::vertical().id_salt("side games").auto_shrink(false).show(ui, |ui| {
                for g in &games {
                    let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 46.0), egui::Sense::click());
                    let open = self.tab == Tab::Clips && self.view == ClipsView::Game(g.name.clone());
                    if open || r.hovered() {
                        ui.painter().rect_filled(rect, 8.0, CARD);
                    }
                    let pic = egui::Rect::from_min_size(rect.left_top() + egui::vec2(8.0, 6.0), egui::vec2(34.0, 34.0));
                    match self.game_picture(ui.ctx(), g) {
                        Some(tex) => paint_fill(ui, &tex, pic, 6),
                        None => _ = ui.painter().rect_filled(pic, 6, RAISED),
                    }
                    let x = pic.right() + 10.0;
                    let name = g.name.as_deref().unwrap_or("Not sorted");
                    let name = ui.painter().layout(name.into(), egui::FontId::new(13.0, egui::FontFamily::Name("strong".into())), Color32::WHITE, rect.right() - x - 4.0);
                    ui.painter().galley(egui::pos2(x, rect.top() + 6.0), name, Color32::WHITE);
                    let what = if g.count == 1 { "1 clip".to_string() } else { format!("{} clips", g.count) };
                    ui.painter().text(egui::pos2(x, rect.bottom() - 7.0), egui::Align2::LEFT_BOTTOM, what, egui::FontId::proportional(12.0), MUTED);
                    if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        self.tab = Tab::Clips;
                        self.player = None;
                        self.view = ClipsView::Game(g.name.clone());
                    }
                }
            });
        }
    }

    /// Across the top of every page: the clip search, Save clip, and whether instant replay is on.
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let search = egui::TextEdit::singleline(&mut self.search).hint_text("Search clips").desired_width(300.0).min_size(egui::vec2(0.0, 34.0)).margin(egui::vec2(12.0, 8.0));
            if ui.add(search).changed() && !self.search.trim().is_empty() {
                self.tab = Tab::Clips;
            }
            if primary(ui, self.buffer, strong("Save clip", 14.0), egui::vec2(110.0, 34.0)).on_hover_text(format!("Saves the last {}. Same as pressing {}.", mmss(self.cfg.buffer_secs), self.cfg.hotkey)).clicked() {
                let _ = self.cmds.send(Cmd::Save);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut on = self.buffer;
                if ui.add_enabled(self.connected, toggle(&mut on)).on_hover_text("Turn instant replay on or off").changed() {
                    let _ = self.cmds.send(Cmd::SetReplay(on));
                }
                let (color, text) = match (self.connected, self.buffer) {
                    (true, true) => (accent_light(), "Instant replay on"),
                    (true, false) => (AMBER, "Instant replay off"),
                    _ => (RED, "Recorder starting..."),
                };
                // Under it the clip length and key, or what's wrong while it isn't on.
                let note = if self.connected && self.buffer { format!("Last {}  ·  {} saves", mmss(self.cfg.buffer_secs), self.cfg.hotkey) } else { self.status.clone() };
                let w = (ui.available_width() - 8.0).clamp(0.0, 260.0);
                ui.allocate_ui_with_layout(egui::vec2(w, 34.0), egui::Layout::top_down(egui::Align::Max), |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(strong(text, 13.0).color(color)).on_hover_text(&self.status);
                    ui.add(egui::Label::new(RichText::new(note).small().color(MUTED)).truncate()).on_hover_text(&self.status);
                });
            });
        });
        ui.add_space(18.0);
    }

    /// One page link in the sidebar: the open page is tinted, with a purple mark on the sidebar's
    /// right edge.
    fn nav_item(&mut self, ui: &mut egui::Ui, tab: Tab, glyph: char, name: &str) {
        let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 36.0), egui::Sense::click());
        let selected = self.tab == tab;
        let painter = ui.ctx().layer_painter(ui.layer_id());
        if selected || r.hovered() {
            painter.rect_filled(rect, 8.0, if selected { CARD.lerp_to_gamma(accent(), 0.1) } else { CARD });
        }
        if selected {
            // Past the panel's 12 px margin, on its edge.
            let edge = egui::Rect::from_min_size(egui::pos2(rect.right() + 9.0, rect.top() + 6.0), egui::vec2(3.0, rect.height() - 12.0));
            painter.rect_filled(edge, 2.0, accent_light());
        }
        let color = if selected { Color32::WHITE } else { Color32::from_rgb(186, 186, 198) };
        let mut x = rect.left() + 14.0;
        if ICONS.load(std::sync::atomic::Ordering::Relaxed) {
            let font = egui::FontId::new(15.0, egui::FontFamily::Name("icons".into()));
            painter.text(egui::pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, glyph, font, if selected { accent_light() } else { MUTED });
            x += 28.0;
        }
        let font = egui::FontId::new(14.0, if selected { egui::FontFamily::Name("strong".into()) } else { egui::FontFamily::Proportional });
        painter.text(egui::pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, name, font, color);
        if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            self.tab = tab;
        }
    }

    fn hotkey_button(&mut self, ui: &mut egui::Ui, target: Target, hint: &str) {
        let current = self.hotkey_of(target).clone();
        let capturing = self.capturing == Some(target);
        let text = if capturing {
            "Press keys... (Esc cancels)".into()
        } else if current.is_empty() {
            "Not set".into()
        } else {
            current
        };
        let mut button = egui::Button::new(RichText::new(text).strong()).min_size(egui::vec2(150.0, 28.0));
        if capturing {
            button = button.fill(accent_dim());
        }
        if ui.add_enabled(self.capturing.is_none() || capturing, button).on_hover_text(hint).clicked() && !capturing {
            self.start_capture(target, ui.ctx());
        }
    }

    fn general_tab(&mut self, ui: &mut egui::Ui) {
        card(ui, "Clips", |ui| {
            form(ui, "clips", |ui| {
                ui.label("Length");
                ui.add(egui::Slider::new(&mut self.cfg.buffer_secs, 10..=1200).custom_formatter(|v, _| mmss(v as u32)));
                ui.end_row();

                ui.label("Folder");
                folder_field(ui, &mut self.cfg.folder, "");
                ui.end_row();

                ui.label("File name");
                ui.add(egui::TextEdit::singleline(&mut self.cfg.clip_prefix).hint_text("(none)").desired_width(f32::INFINITY))
                    .on_hover_text("Comes before the date and time. Characters Windows doesn't allow in names are left out.");
                ui.end_row();

                ui.label("Date style");
                ui.horizontal_wrapped(|ui| {
                    for (i, (_, example)) in recorder::NAME_STYLES.iter().enumerate() {
                        chip(ui, &mut self.cfg.name_style, i as u8, *example);
                    }
                });
                ui.end_row();
            });
            note(ui, format!("Clips are named like: {}", recorder::example_name(&self.cfg)));
            ui.checkbox(&mut self.cfg.sort_by_game, "Sort clips into a folder per game")
                .on_hover_text("Named after the title of the window in front when you save. Only the title is read; the game itself is never opened or touched.");
        });
        card(ui, "Auto-cleanup", |ui| {
            limit(ui, &mut self.cfg.keep_under_gb, "Keep clips under", "GB", 100, 10_000);
            limit(ui, &mut self.cfg.keep_days, "Remove clips older than", "days", 30, 3650);
            note(ui, "Oldest first, to the Recycle Bin. Starred clips are never removed and don't count toward the size. Clips from the last 10 minutes are left alone.");
            let rule = cleanup_rule(&self.cfg);
            if rule != clips::Cleanup::default() {
                let stale = self.cleanup_preview.is_none_or(|(r, at, ..)| r != rule || at.elapsed() > std::time::Duration::from_secs(5));
                if stale {
                    let plan = clips::cleanup_plan(&clips::list(&self.cfg.folder, usize::MAX), &self.favorites, rule, std::time::SystemTime::now());
                    let bytes = plan.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
                    self.cleanup_preview = Some((rule, std::time::Instant::now(), plan.len(), bytes));
                }
                ui.ctx().request_repaint_after(std::time::Duration::from_secs(5));
                if let Some((_, _, n, bytes)) = self.cleanup_preview {
                    let text = if n == 0 { "Nothing to remove right now.".to_string() } else { format!("Right now this would move {n} clips ({}) to the Recycle Bin.", clips::size(bytes)) };
                    ui.label(RichText::new(text).small().color(if n == 0 { MUTED } else { AMBER }));
                }
            }
        });
        card(ui, "Archive", |ui| {
            limit(ui, &mut self.cfg.archive_days, "Move clips older than", "days", 30, 3650);
            if self.cfg.archive_days > 0 {
                folder_field(ui, &mut self.cfg.archive_folder, "Another drive or a NAS folder");
                note(ui, "Oldest first, into the same game folders there. Stars, tags and share links go along. Clips from the last 10 minutes are left alone.");
                let (text, color) = match archive_rule(&self.cfg) {
                    None if self.cfg.archive_folder.trim().is_empty() => ("Pick a folder to move them to.".to_string(), MUTED),
                    None => ("Pick a folder outside the clips folder.".to_string(), AMBER),
                    Some(_) if self.cfg == self.saved && clips::ARCHIVE_MISSING.load(std::sync::atomic::Ordering::Relaxed) => {
                        ("That folder wasn't there last time (drive or NAS off?). Clips wait until it's back.".to_string(), AMBER)
                    }
                    Some(rule) => {
                        let stale = self.archive_preview.as_ref().is_none_or(|(r, at, ..)| *r != rule || at.elapsed() > std::time::Duration::from_secs(5));
                        if stale {
                            let all = clips::list(&self.cfg.folder, usize::MAX);
                            let plan = clips::archive_plan(&all, rule.after, std::time::SystemTime::now());
                            let (n, bytes) = (plan.len(), plan.iter().map(|c| c.bytes).sum());
                            self.archive_preview = Some((rule, std::time::Instant::now(), n, bytes));
                        }
                        ui.ctx().request_repaint_after(std::time::Duration::from_secs(5));
                        match self.archive_preview {
                            Some((_, _, n, bytes)) if n > 0 => (format!("Right now this would move {n} clips ({}) there.", clips::size(bytes)), accent_light()),
                            _ => ("Nothing to move right now.".to_string(), MUTED),
                        }
                    }
                };
                ui.label(RichText::new(text).small().color(color));
            }
        });
        card(ui, "Cloud backup", |ui| {
            if ui.checkbox(&mut self.cfg.backup, "Copy clips you pick to a cloud folder").changed() && self.cfg.backup && self.cfg.backup_folder.trim().is_empty() {
                self.cfg.backup_folder = onedrive_folder().unwrap_or_default();
            }
            if self.cfg.backup {
                folder_field(ui, &mut self.cfg.backup_folder, "A OneDrive or Google Drive folder");
                note(ui, "Pick a clip with Back up to cloud in its ... menu; it gets the Cloud tag. Its app uploads them. Each one is copied once, into its game folder. Nothing there is ever removed, even if you untag or delete the clip here.");
                if backup_rule(&self.cfg).is_none() && !self.cfg.backup_folder.trim().is_empty() {
                    ui.label(RichText::new("Pick a folder outside the clips folder.").small().color(AMBER));
                }
            }
        });
        card(ui, "Behavior", |ui| {
            ui.checkbox(&mut self.cfg.save_popup, "Show a popup when a clip is saved");
            ui.checkbox(&mut self.cfg.save_sound, "Play a soft sound when a clip or screenshot is saved");
            ui.checkbox(&mut self.cfg.show_tips, "Show tips and explanations").on_hover_text("The tip boxes at the top of pages and the small grey text under settings");
            ui.horizontal_wrapped(|ui| {
                ui.label("Accent color");
                for (i, (name, ..)) in ACCENTS.iter().enumerate() {
                    chip(ui, &mut self.cfg.accent, i as u8, *name);
                }
            });
            if ui.checkbox(&mut self.start_with_windows, "Start with Windows (in the tray)").changed() {
                if let Err(e) = startup::set_enabled(self.start_with_windows) {
                    self.status = format!("Could not change startup setting: {e}");
                    self.start_with_windows = startup::is_enabled();
                }
            }
            note(ui, format!("{}, Esc or X hides this window. The app keeps running in the tray.", self.cfg.overlay_hotkey));
        });
        card(ui, "Discord", |ui| {
            form(ui, "discord", |ui| {
                ui.label("Webhook");
                ui.add(egui::TextEdit::singleline(&mut self.cfg.discord_webhook).password(true).hint_text("https://discord.com/api/webhooks/...").desired_width(f32::INFINITY));
                ui.end_row();
            });
            match self.cfg.discord_webhook.trim() {
                "" => note(ui, "For Post to Discord in a clip's menu. In Discord: Edit Channel > Integrations > Webhooks > New Webhook > Copy Webhook URL, then paste it here. Keep it private: anyone with it can post there."),
                w if upload::is_discord_webhook(w) => note(ui, "Clips go to that channel with Post to Discord in a clip's menu (up to 10 MB)."),
                _ => _ = ui.label(RichText::new("That doesn't look like a Discord webhook link (https://discord.com/api/webhooks/...).").small().color(RED)),
            }
        });
        card(ui, "Phone", |ui| {
            ui.checkbox(&mut self.cfg.phone_page, "Watch clips from your phone");
            note(ui, "A private page with your clips, for phones on your home network or Tailscale. Anyone with the link can watch, so keep it to yourself. Windows may ask to let the app through its firewall: allow it for private networks.");
            if self.cfg.phone_page {
                // Not a tip: shown even with tips and explanations off.
                ui.label(RichText::new("The page isn't encrypted (plain http). Use it at home or over Tailscale, not on public Wi-Fi, where others on the network could see the link.").small().color(AMBER));
            }
            match &self.phone {
                Some((_, Ok(server))) => form(ui, "phone", |ui| {
                    for (ip, url) in server.urls() {
                        ui.label(if phone::is_tailscale(ip) { "Tailscale" } else if ip.is_loopback() { "This PC" } else { "Home network" });
                        ui.horizontal(|ui| {
                            if ui.button("Copy").clicked() {
                                ui.ctx().copy_text(url.clone());
                            }
                            ui.label(RichText::new(url).monospace());
                        });
                        ui.end_row();
                    }
                }),
                Some((_, Err(e))) => {
                    ui.label(RichText::new(e).small().color(RED));
                }
                None if self.cfg.phone_page => {
                    ui.label(RichText::new("Starts after Save.").small().color(MUTED));
                }
                None => {}
            }
            if self.cfg.phone_page && ui.button("New key").on_hover_text("Makes new links; the old ones stop working after Save.").clicked() {
                self.cfg.phone_key = config::random_hex();
            }
        });
        card(ui, "Windows", |ui| {
            ui.checkbox(&mut self.cfg.game_bar_off, "Turn off Xbox Game Bar capture");
            note(ui, "Game Bar records in the background too, which costs some FPS and competes for the GPU encoder. Unticking puts your own setting back.");
            ui.checkbox(&mut self.cfg.gpu_high, "Run the recorder on the high-performance GPU");
            note(ui, "For PCs with two GPUs (like a laptop or a CPU with graphics). Only applies to the app's private OBS copy. Takes effect after Save.");
        });
        if self.wizard.is_none() {
            card(ui, "Move to a new PC", |ui| {
                note(ui, "One file with your saved settings, stars and tags, for a new PC or a reinstall. Passwords, the phone key and the Discord webhook are left out.");
                ui.horizontal(|ui| {
                    if ui.button("Export settings...").clicked() {
                        self.export_settings();
                    }
                    if ui.button("Import settings...").clicked() {
                        self.pick_import();
                    }
                });
            });
            card(ui, "Remove", |ui| {
                note(ui, "Puts back everything the app changed on this PC and moves its private OBS copy to the Recycle Bin. Your clips stay.");
                if ui.button("Remove from this PC...").clicked() {
                    self.confirm_remove = true;
                }
            });
        }
    }

    /// Writes the saved settings, stars and tags (no secrets) to a file the user picks.
    fn export_settings(&mut self) {
        let Some(path) = rfd::FileDialog::new().set_title("Export settings").set_file_name("Replay Vault settings.json").add_filter("Settings", &["json"]).save_file() else { return };
        self.status = match config::Export::new(&self.saved, &self.favorites, &self.tags).write(&path) {
            Ok(()) => format!("Settings exported to {}.", path.display()),
            Err(e) => format!("Couldn't export settings: {e}"),
        };
    }

    /// Reads a settings file the user picks, for the import dialog to show what it changes.
    fn pick_import(&mut self) {
        let Some(path) = rfd::FileDialog::new().set_title("Import settings").add_filter("Settings", &["json"]).pick_file() else { return };
        match config::Export::read(&path) {
            Ok(file) => {
                let missing = config::missing_folders(&file.config);
                self.import = Some((file, missing));
            }
            Err(e) => self.status = format!("Couldn't read that settings file: {e}"),
        }
    }

    fn keybinds_tab(&mut self, ui: &mut egui::Ui) {
        card(ui, "Keybinds", |ui| {
            form(ui, "keybinds", |ui| {
                ui.label(format!("Save clip ({})", mmss(self.cfg.buffer_secs)));
                self.hotkey_button(ui, Target::Save, "Click, then press the keys. OBS listens for it; takes effect after Save.");
                ui.end_row();

                let optional = [
                    (Target::Save30, "Saves the replay, then keeps only its end."),
                    (Target::Save60, "Saves the replay, then keeps only its end."),
                    (Target::Record, "Starts a full recording; press again to stop. Recordings go in the Recordings folder."),
                    (Target::Screenshot, "Saves a picture of the screen next to your clips."),
                    (Target::Replay, "Turns instant replay on or off."),
                    (Target::Mic, "Mutes or unmutes your mic in clips and recordings."),
                ];
                for (target, what) in optional {
                    ui.label(target_name(target));
                    ui.horizontal(|ui| {
                        self.hotkey_button(ui, target, &format!("Click, then press the keys. Works right away. {what}"));
                        if !self.hotkey_of(target).is_empty() && self.capturing.is_none() && ui.button("Clear").clicked() {
                            self.hotkey_of(target).clear();
                            self.register_overlay_hotkey();
                        }
                    });
                    ui.end_row();
                }

                ui.label("Show / hide this window");
                self.hotkey_button(ui, Target::Overlay, "Click, then press the keys. Works right away.");
                ui.end_row();
            });
            if let Some(err) = &self.hotkey_error {
                ui.colored_label(RED, err);
            }
            ui.add_space(4.0);
            note(ui, "Click a key, then press the combination you want. Use Ctrl, Alt, Shift or Win with a letter or number; F-keys, Pause, Insert, Home, End, Page Up/Down, Print Screen and Scroll Lock also work on their own. Esc cancels.");
        });
    }

    /// A frame of `path` at `at` seconds as a texture: Some(None) if it can't be decoded, None while
    /// it's still being made. Also learns the clip's length. `quick` takes the keyframe before `at`.
    fn frame_texture(&mut self, ctx: &egui::Context, path: &std::path::Path, at: f64, width: usize, quick: bool) -> Option<Option<egui::TextureHandle>> {
        let key = (path.to_path_buf(), (at * 10.0).round().max(0.0) as u32);
        if let Some(t) = self.textures.get(&key) {
            return Some(t.clone());
        }
        let p = self.previews.get(path, at, width, quick)?;
        if let Some(secs) = p.secs {
            self.lengths.insert(path.to_path_buf(), secs);
        }
        let tex = p.frame.map(|f| {
            let img = egui::ColorImage::from_rgba_unmultiplied([f.width, f.height], &f.rgba);
            ctx.load_texture(format!("{}@{}", path.display(), key.1), img, egui::TextureOptions::LINEAR)
        });
        self.textures.insert(key, tex.clone());
        Some(tex)
    }

    /// The hover-scrub frame of `path` near `at` seconds, or None until it's decoded. A frame takes
    /// a few tenths of a second, so only one is asked for at a time: sweeping across a card doesn't
    /// queue up frames that are stale by the time they're made.
    fn scrub_frame(&mut self, ctx: &egui::Context, path: &std::path::Path, at: f64, width: usize) -> Option<egui::TextureHandle> {
        if let Some((p, t)) = self.scrub_wait.clone()
            && self.frame_texture(ctx, &p, t, width, true).is_some()
        {
            self.scrub_wait = None;
        }
        if !self.textures.contains_key(&(path.to_path_buf(), (at * 10.0).round() as u32)) {
            if self.scrub_wait.is_some() {
                return None;
            }
            // Scrub frames add up (about 0.25 MB each); past a few hundred, drop them all but keep
            // the thumbnails (the frames at 1 s).
            if self.textures.len() > 300 {
                self.textures.retain(|k, _| k.1 == 10);
                self.previews.retain(|t| t == 10);
            }
            self.scrub_wait = Some((path.to_path_buf(), at));
        }
        self.frame_texture(ctx, path, at, width, true).flatten()
    }

    /// The Clips page's title row: counts, free space, Open folder, the game filter and the last
    /// message (upload done, recycled, ...).
    fn clips_header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(strong("Clips", 24.0).color(Color32::WHITE));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // In a game, its own folder.
                let folder = match &self.view {
                    ClipsView::Game(Some(g)) => std::path::Path::new(&self.cfg.folder).join(g),
                    _ => std::path::PathBuf::from(&self.cfg.folder),
                };
                if ui.add(icon_button(icon::FOLDER, "Open folder")).clicked() {
                    clips::open(&folder, false);
                }
                let making = self.montage.as_ref().is_some_and(|m| m.job.is_some());
                let text = if self.montage.is_some() { "Close montage" } else { "Montage" };
                let montage = ui.add_enabled(!making, icon_button(icon::CLIPS, text)).on_hover_text("Pick clips, put them in order and join them into one video, with fades and music if you like");
                if montage.clicked() {
                    self.montage = match self.montage {
                        Some(_) => None,
                        None => Some(Montage { clips: Vec::new(), fades: true, music: None, volume: 0.4, job: None }),
                    };
                }
            });
        });
        let mut info = format!("{} clips", self.clip_list.len());
        if let Some(free) = clips::free_space(&self.cfg.folder) {
            info.push_str(&format!("  ·  {} free", clips::size(free)));
        }
        ui.label(RichText::new(info).color(MUTED))
            .on_hover_text("Click a clip to pick it, or use the arrow keys. Enter plays it, F2 renames it, Delete moves it to the Recycle Bin.");
        // One chip per game folder (and Recordings), when there are any.
        let mut groups: Vec<String> = self.clip_list.iter().filter_map(|c| c.group.clone()).collect();
        groups.sort_by_key(|g| g.to_lowercase());
        groups.dedup();
        // The game shown has no clips left (all recycled or renamed away): back to the games.
        if let ClipsView::Game(g) = &self.view
            && !self.clip_list.iter().any(|c| c.group == *g)
        {
            self.view = ClipsView::Games;
        }
        // Then one per tag in use.
        let tags = self.tags_in_use();
        if matches!(&self.view, ClipsView::Tag(t) if !tags.contains(t)) {
            self.view = ClipsView::Games;
        }
        if !self.clip_list.is_empty() {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                chip(ui, &mut self.view, ClipsView::Games, "Home");
                chip(ui, &mut self.view, ClipsView::All, "All");
                for g in &groups {
                    chip(ui, &mut self.view, ClipsView::Game(Some(g.clone())), g);
                }
                if !groups.is_empty() && self.clip_list.iter().any(|c| c.group.is_none()) {
                    chip(ui, &mut self.view, ClipsView::Game(None), "Not sorted");
                }
                if !groups.is_empty() && !tags.is_empty() {
                    ui.separator();
                }
                for t in tags {
                    chip(ui, &mut self.view, ClipsView::Tag(t.clone()), format!("#{t}"));
                }
            });
        }
        if let Some(msg) = self.clips_msg.clone() {
            ui.add_space(4.0);
            let file = self.exported.as_ref().filter(|(m, _)| *m == msg).map(|(_, f)| f.clone());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(msg).color(accent_light()));
                if let Some(f) = file
                    && ui.add(icon_button(icon::COPY, "Copy").small()).on_hover_text("Copy the file, then paste it into Discord or a chat").clicked()
                {
                    self.clips_msg = Some(match clips::copy_file(&f) {
                        Ok(()) => "Copied. Paste it into Discord, a chat or a folder.".into(),
                        Err(e) => format!("Couldn't copy it: {e:#}"),
                    });
                }
            });
        }
        ui.add_space(14.0);
    }

    /// The clips as a grid of cards, under a heading per day, newest first. With `keys`, the arrow
    /// keys pick a clip and Enter, F2 and Delete act on it.
    fn clips_tab(&mut self, ui: &mut egui::Ui, keys: bool) {
        self.finish_uploads(ui.ctx());
        self.finish_exports();
        if self.clip_list.is_empty() {
            card(ui, "No clips yet", |ui| {
                ui.label(RichText::new(format!("Press {} in a game to save the last {}.", self.cfg.hotkey, mmss(self.cfg.buffer_secs))).color(MUTED));
            });
            return;
        }
        self.montage_panel(ui);
        let words: Vec<String> = self.search.to_lowercase().split_whitespace().map(String::from).collect();
        // A search looks through everything, from the games too.
        if self.view == ClipsView::Games && words.is_empty() {
            let mut act = ClipActions::default();
            self.recent_row(ui, &mut act);
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.label(strong("Your games", 18.0).color(Color32::WHITE));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("All clips").clicked() {
                        self.view = ClipsView::All;
                    }
                });
            });
            ui.add_space(10.0);
            self.games_grid(ui);
            self.clip_actions(act, ui.ctx());
            return;
        }
        // The newest 60, and every starred clip however old.
        let mut n = 0;
        let no_tags = std::collections::BTreeSet::new();
        let tags_of = |c: &clips::Clip| self.tags.get(&c.path.display().to_string()).unwrap_or(&no_tags);
        let list: Vec<ClipItem> = self
            .clip_list
            .iter()
            .filter(|c| match &self.view {
                ClipsView::Game(g) => c.group == *g,
                ClipsView::Tag(t) => tags_of(c).contains(t),
                _ => true,
            })
            // Every word typed appears in the name, the game folder or a tag.
            .filter(|c| {
                words.is_empty() || {
                    let tags: Vec<&str> = tags_of(c).iter().map(String::as_str).collect();
                    let hay = format!("{} {} {}", c.name, c.group.as_deref().unwrap_or_default(), tags.join(" ")).to_lowercase();
                    words.iter().all(|w| hay.contains(w.as_str()))
                }
            })
            .filter(|c| {
                n += 1;
                n <= 60 || self.favorites.contains(&c.path.display().to_string())
            })
            .map(|c| self.clip_item(c))
            .collect();
        if list.is_empty() {
            ui.label(RichText::new("No clips match.").color(MUTED));
            return;
        }
        // As many columns of at least 240 px as fit.
        const GAP: f32 = 16.0;
        let avail = ui.available_width();
        let cols = ((avail + GAP) / (240.0 + GAP)).floor().max(1.0) as usize;
        // Less the card border, drawn outside this width.
        let w = (avail - GAP * (cols - 1) as f32) / cols as f32 - 2.0;
        let mut act = ClipActions::default();
        // Starred clips first, under their own heading.
        let (starred, list): (Vec<ClipItem>, Vec<ClipItem>) = list.into_iter().partition(|i| i.favorite);
        let mut days: Vec<(String, Vec<ClipItem>)> = Vec::new();
        if !starred.is_empty() {
            days.push(("Favorites".into(), starred));
        }
        for item in list {
            let label = clips::day_label(item.modified);
            match days.last_mut() {
                Some((l, items)) if *l == label => items.push(item),
                _ => days.push((label, vec![item])),
            }
        }
        // The cards as they're laid out, row by row, for the arrow keys.
        let rows: Vec<&[ClipItem]> = days.iter().flat_map(|(_, items)| items.chunks(cols)).collect();
        let lens: Vec<usize> = rows.iter().map(|r| r.len()).collect();
        let at = self.selected.as_ref().and_then(|s| rows.iter().enumerate().find_map(|(r, row)| row.iter().position(|i| i.path == *s).map(|c| (r, c))));
        let mut reveal = false;
        if keys {
            let pressed = |key| ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key));
            for key in [egui::Key::ArrowLeft, egui::Key::ArrowRight, egui::Key::ArrowUp, egui::Key::ArrowDown] {
                if pressed(key) {
                    let (r, c) = at.map_or((0, 0), |at| grid_step(&lens, at, key));
                    self.selected = Some(rows[r][c].path.clone());
                    reveal = true;
                }
            }
            if let Some((r, c)) = at {
                let item = &rows[r][c];
                if pressed(egui::Key::Enter) {
                    if item.is_picture { clips::open(&item.path, false) } else { act.play = Some(item.path.clone()) }
                }
                if pressed(egui::Key::F2) {
                    self.start_rename(&item.path);
                }
                if self.montage.is_some() && !item.is_picture && pressed(egui::Key::Space) {
                    act.pick = Some(item.path.clone());
                }
                if pressed(egui::Key::Delete) {
                    act.recycle = Some(item.path.clone());
                    // The next card along (or the one before the last), so Delete can be pressed again.
                    let next = [egui::Key::ArrowRight, egui::Key::ArrowLeft].map(|k| grid_step(&lens, (r, c), k)).into_iter().find(|&n| n != (r, c));
                    self.selected = next.map(|(r, c)| rows[r][c].path.clone());
                }
            }
        }
        for (label, items) in &days {
            ui.label(strong(label, 15.0).color(Color32::WHITE));
            ui.add_space(8.0);
            for row in items.chunks(cols) {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = GAP;
                    for item in row {
                        let card = self.clip_card(ui, item, w, &mut act);
                        if reveal && self.selected.as_ref() == Some(&item.path) {
                            card.scroll_to_me(None);
                        }
                    }
                });
                ui.add_space(GAP);
            }
            ui.add_space(4.0);
        }
        self.clip_actions(act, ui.ctx());
    }

    /// Acts on what was clicked on the clip cards this frame.
    fn clip_actions(&mut self, act: ClipActions, ctx: &egui::Context) {
        if let Some(path) = act.play {
            self.play(&path);
        }
        if let (Some(path), Some(m)) = (act.pick, &mut self.montage) {
            match m.clips.iter().position(|c| *c == path) {
                Some(i) => _ = m.clips.remove(i),
                None => m.clips.push(path),
            }
        }
        if let Some(path) = act.recycle {
            // The player holds the file open.
            if self.player.as_ref().is_some_and(|(p, _)| *p == path) {
                self.player = None;
            }
            self.clips_msg = Some(match clips::recycle(&path) {
                Ok(()) => {
                    clips::moved(&mut self.favorites, &mut self.tags, &path, None);
                    format!("Moved {} to the Recycle Bin.", path.file_name().unwrap_or_default().to_string_lossy())
                }
                Err(e) => format!("Couldn't recycle it: {e:#}"),
            });
            if self.trim.as_ref().is_some_and(|t| t.path == path) {
                self.trim = None;
            }
            if let Some(m) = &mut self.montage {
                m.clips.retain(|c| *c != path);
            }
            self.clips_checked -= std::time::Duration::from_secs(60);
        }
        if let Some(path) = act.trim {
            self.begin_trim(&path);
        }
        if let Some((path, share)) = act.export {
            self.start_export(&path, share, ctx);
        }
        if let Some(path) = act.star {
            let key = path.display().to_string();
            if !self.favorites.remove(&key) {
                self.favorites.insert(key);
            }
            clips::save_favorites(&self.favorites);
        }
        if let Some((path, tag, on)) = act.tag {
            clips::set_tag(&mut self.tags, &path, &tag, on);
            clips::save_tags(&self.tags);
        }
    }

    fn clip_item(&self, c: &clips::Clip) -> ClipItem {
        let key = c.path.display().to_string();
        ClipItem {
            path: c.path.clone(),
            name: c.name.clone(),
            bytes: c.bytes,
            modified: c.modified,
            is_picture: c.is_picture,
            favorite: self.favorites.contains(&key),
            tags: self.tags.get(&key).map(|t| t.iter().cloned().collect()).unwrap_or_default(),
        }
    }

    /// "Recent clips": the 12 newest in one row of wide cards, moved along with the arrows.
    fn recent_row(&mut self, ui: &mut egui::Ui, act: &mut ClipActions) {
        const GAP: f32 = 16.0;
        let avail = ui.available_width();
        let cols = ((avail + GAP) / (260.0 + GAP)).floor().max(1.0) as usize;
        let w = (avail - GAP * (cols - 1) as f32) / cols as f32 - 2.0;
        let total = self.clip_list.len().min(12);
        self.recent_at = self.recent_at.min(total.saturating_sub(cols));
        ui.horizontal(|ui| {
            ui.label(strong("Recent clips", 18.0).color(Color32::WHITE));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(self.recent_at + cols < total, egui::Button::new(icon(icon::RIGHT, 12.0))).on_hover_text("Older").clicked() {
                    self.recent_at += 1;
                }
                if ui.add_enabled(self.recent_at > 0, egui::Button::new(icon(icon::LEFT, 12.0))).on_hover_text("Newer").clicked() {
                    self.recent_at -= 1;
                }
            });
        });
        ui.add_space(10.0);
        let items: Vec<ClipItem> = self.clip_list[self.recent_at..total].iter().take(cols).map(|c| self.clip_item(c)).collect();
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            for item in &items {
                self.clip_card(ui, item, w, act);
            }
        });
    }

    /// A game folder (None: clips not in any) with its clips counted, played last first.
    fn games(&self) -> Vec<Game> {
        // The list is newest first, so each game's first clip is its newest.
        let mut games: Vec<Game> = Vec::new();
        for c in &self.clip_list {
            let i = games.iter().position(|g| g.name == c.group).unwrap_or_else(|| {
                games.push(Game { name: c.group.clone(), count: 0, bytes: 0, thumb: None });
                games.len() - 1
            });
            let g = &mut games[i];
            g.count += 1;
            g.bytes += c.bytes;
            if g.thumb.is_none() && !c.is_picture {
                g.thumb = Some(c.path.clone());
            }
        }
        games
    }

    /// A game's picture: its Steam cover once downloaded, else a frame of its newest clip (640 px
    /// wide, enough to cut a tall card from; frames are cached by time, not size).
    fn game_picture(&mut self, ctx: &egui::Context, g: &Game) -> Option<egui::TextureHandle> {
        if let Some(name) = &g.name {
            if !self.covers.contains_key(name)
                && let Some(path) = covers::get(name)
            {
                let tex = covers::load(&path).map(|img| ctx.load_texture(format!("cover {name}"), img, egui::TextureOptions::LINEAR));
                self.covers.insert(name.clone(), tex);
            }
            if let Some(Some(tex)) = self.covers.get(name) {
                return Some(tex.clone());
            }
        }
        g.thumb.as_ref().and_then(|p| self.frame_texture(ctx, p, 1.0, 640, false)).flatten()
    }

    /// "Your games": a tall card per game folder (and one for clips not in any), the game played
    /// last first, with its cover, how many clips and their size. Click to see its clips.
    fn games_grid(&mut self, ui: &mut egui::Ui) {
        let games = self.games();
        const GAP: f32 = 18.0;
        let avail = ui.available_width();
        let cols = ((avail + GAP) / (150.0 + GAP)).floor().max(1.0) as usize;
        let w = (avail - GAP * (cols - 1) as f32) / cols as f32;
        let mut open = None;
        for row in games.chunks(cols) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for g in row {
                    let r = ui.vertical(|ui| {
                        ui.set_width(w);
                        ui.spacing_mut().item_spacing.y = 2.0;
                        let (rect, r) = ui.allocate_exact_size(egui::vec2(w, w * 1.5), egui::Sense::click());
                        match self.game_picture(ui.ctx(), g) {
                            Some(tex) => paint_fill(ui, &tex, rect, 8),
                            None => _ = ui.painter().rect_filled(rect, 8, RAISED),
                        }
                        if r.hovered() {
                            ui.painter().rect_stroke(rect, 8, egui::Stroke::new(2.0, accent_light()), egui::StrokeKind::Inside);
                        }
                        ui.add_space(6.0);
                        let title = g.name.as_deref().unwrap_or("Not sorted");
                        ui.add(egui::Label::new(strong(title, 14.0).color(Color32::WHITE)).truncate()).on_hover_text(title);
                        let what = if g.count == 1 { "1 clip".to_string() } else { format!("{} clips", g.count) };
                        ui.label(RichText::new(format!("{what}  ·  {}", clips::size(g.bytes))).small().color(MUTED));
                        r
                    });
                    if r.inner.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        open = Some(g.name.clone());
                    }
                }
            });
            ui.add_space(GAP);
        }
        if let Some(g) = open {
            self.view = ClipsView::Game(g);
        }
    }

    /// One clip in the grid: its thumbnail (click to play; the length in the corner), name, time
    /// and size, then Trim, sharing and a menu with the rest.
    fn clip_card(&mut self, ui: &mut egui::Ui, item: &ClipItem, w: f32, act: &mut ClipActions) -> egui::Response {
        let path = &item.path;
        // Hovered last frame: the card lifts, with a purple edge and glow. Selected: a brighter edge.
        let hover_id = ui.scope_id().with(("card hover", path));
        let lifted = ui.ctx().data(|d| d.get_temp::<bool>(hover_id)).unwrap_or(false);
        // In montage mode, its place in the montage (None while it's not in it).
        let picked = self.montage.as_ref().filter(|_| !item.is_picture).map(|m| m.clips.iter().position(|c| c == path));
        let selected = self.selected.as_ref() == Some(path) || matches!(picked, Some(Some(_)));
        let glow = egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: alpha(accent(), 60) };
        let (stroke, shadow) = match (selected, lifted) {
            // Same width as the others, so the grid doesn't shift.
            (true, _) => (egui::Stroke::new(1.0, accent()), glow),
            (false, true) => (egui::Stroke::new(1.0, accent().lerp_to_gamma(BORDER, 0.35)), glow),
            _ => (egui::Stroke::new(1.0, CARD), egui::Shadow::NONE),
        };
        let frame = egui::Frame::new().fill(CARD).stroke(stroke).corner_radius(10).shadow(shadow);
        // Clicks on the card itself (not its buttons) select it.
        let body = egui::UiBuilder::new().id_salt(("card", path)).sense(egui::Sense::click());
        let shown = ui.scope_builder(body, |ui| frame.show(ui, |ui| ui.vertical(|ui| {
            ui.set_width(w);
            ui.spacing_mut().item_spacing.y = 4.0;
            let (rect, thumb) = ui.allocate_exact_size(egui::vec2(w, w * 9.0 / 16.0), egui::Sense::click());
            let rounding = egui::CornerRadius { nw: 10, ne: 10, sw: 0, se: 0 };
            let width = (w * 1.5) as usize;
            // Hover scrubbing: across a video, the pointer's place picks the frame, in 16 steps
            // (the middle of each, as a fraction of the clip, and in seconds).
            let scrub = match (thumb.hover_pos(), self.lengths.get(path)) {
                (Some(p), Some(&len)) if !item.is_picture => {
                    let frac = (((p.x - rect.left()) / rect.width() * 16.0).floor().clamp(0.0, 15.0) as f64 + 0.5) / 16.0;
                    Some((frac, (frac * len * 10.0).round() / 10.0))
                }
                _ => None,
            };
            let shown_id = hover_id.with("scrubbed");
            let tex = match scrub {
                _ if item.is_picture => None,
                Some((_, at)) => match self.scrub_frame(ui.ctx(), path, at, width) {
                    Some(t) => {
                        ui.ctx().data_mut(|d| d.insert_temp(shown_id, t.clone()));
                        Some(Some(t))
                    }
                    // Until it's decoded, the frame shown last (or the thumbnail) stays up.
                    None => ui.ctx().data(|d| d.get_temp(shown_id)).map(Some).or_else(|| self.frame_texture(ui.ctx(), path, 1.0, width, false)),
                },
                None => {
                    ui.ctx().data_mut(|d| d.remove::<egui::TextureHandle>(shown_id));
                    self.frame_texture(ui.ctx(), path, 1.0, width, false)
                }
            };
            match tex {
                Some(Some(tex)) => {
                    egui::Image::new(&tex).corner_radius(rounding).paint_at(ui, rect);
                }
                other => {
                    ui.painter().rect_filled(rect, rounding, RAISED);
                    let gif = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("gif"));
                    let (glyph, text) = if item.is_picture { (icon::PICTURE, if gif { "GIF" } else { "Screenshot" }) } else if other.is_some() { (icon::CLIPS, "No preview") } else { (icon::CLIPS, "") };
                    if ICONS.load(std::sync::atomic::Ordering::Relaxed) {
                        let font = egui::FontId::new(26.0, egui::FontFamily::Name("icons".into()));
                        ui.painter().text(rect.center() - egui::vec2(0.0, 8.0), egui::Align2::CENTER_CENTER, glyph, font, MUTED);
                    }
                    ui.painter().text(rect.center() + egui::vec2(0.0, 20.0), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(12.0), MUTED);
                }
            }
            if thumb.hovered() {
                // Darken (only a little while scrubbing, to see the frame), with a play button in the middle.
                ui.painter().rect_filled(rect, rounding, Color32::from_black_alpha(if scrub.is_some() { 40 } else { 90 }));
                if !item.is_picture && picked.is_none() {
                    ui.painter().circle_filled(rect.center(), 24.0, Color32::from_white_alpha(230));
                    if ICONS.load(std::sync::atomic::Ordering::Relaxed) {
                        let font = egui::FontId::new(18.0, egui::FontFamily::Name("icons".into()));
                        ui.painter().text(rect.center() + egui::vec2(2.0, 0.0), egui::Align2::CENTER_CENTER, icon::PLAY, font, Color32::BLACK);
                    }
                }
            }
            // How far along the clip the scrub is, along the bottom edge.
            if let Some((frac, _)) = scrub {
                let bar = egui::Rect::from_min_size(rect.left_bottom() - egui::vec2(0.0, 3.0), egui::vec2(rect.width() * frac as f32, 3.0));
                ui.painter().rect_filled(bar, 0.0, accent());
            }
            // The length, in the bottom right corner.
            if let Some(len) = self.lengths.get(path) {
                let text = mmss(len.round() as u32);
                let galley = ui.painter().layout_no_wrap(text, egui::FontId::new(12.0, egui::FontFamily::Name("strong".into())), Color32::WHITE);
                let badge = egui::Rect::from_min_size(rect.right_bottom() - galley.size() - egui::vec2(18.0, 14.0), galley.size() + egui::vec2(10.0, 4.0));
                ui.painter().rect_filled(badge, 4.0, Color32::from_black_alpha(190));
                ui.painter().galley(badge.min + egui::vec2(5.0, 2.0), galley, Color32::WHITE);
            }
            // In montage mode: a ring to click, or its number once it's in.
            if let Some(at) = picked {
                let c = rect.left_top() + egui::vec2(20.0, 20.0);
                match at {
                    Some(i) => {
                        ui.painter().circle_filled(c, 12.0, accent());
                        ui.painter().text(c, egui::Align2::CENTER_CENTER, (i + 1).to_string(), egui::FontId::new(13.0, egui::FontFamily::Name("strong".into())), Color32::WHITE);
                    }
                    None => {
                        ui.painter().circle(c, 11.0, Color32::from_black_alpha(120), egui::Stroke::new(2.0, Color32::WHITE));
                    }
                }
            }
            let thumb = thumb.on_hover_cursor(egui::CursorIcon::PointingHand);
            if thumb.clicked() {
                self.selected = Some(path.clone());
                if picked.is_some() {
                    act.pick = Some(path.clone());
                } else if item.is_picture {
                    clips::open(path, false)
                } else {
                    act.play = Some(path.clone())
                }
            }
            egui::Frame::new().inner_margin(egui::Margin { left: 12, right: 12, top: 8, bottom: 10 }).show(ui, |ui| {
                ui.set_width(w - 24.0);
                ui.add(egui::Label::new(strong(&item.name, 14.0).color(Color32::WHITE)).truncate()).on_hover_text(&item.name);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{}  ·  {}", clips::time_label(item.modified), clips::size(item.bytes))).small().color(MUTED));
                    if let Some(job) = self.exports.get(path) {
                        ui.label(RichText::new(format!("Exporting {}%", job.pct.load(std::sync::atomic::Ordering::Relaxed))).small().color(accent_light()));
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
                    }
                });
                if !item.tags.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                        for t in &item.tags {
                            egui::Frame::new().fill(RAISED).corner_radius(8).inner_margin(egui::Margin::symmetric(6, 1)).show(ui, |ui| {
                                ui.label(RichText::new(t).small().color(accent_light()));
                            });
                        }
                    });
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if item.is_picture {
                        if ui.small_button("Open").clicked() {
                            clips::open(path, false);
                        }
                    } else if ui.add(icon_button(icon::CUT, "Trim").small()).clicked() {
                        act.trim = Some(path.clone());
                    }
                    self.share_button(ui, path, item.bytes);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.menu_button(icon(icon::MORE, 14.0), |ui| {
                            if ui.button("Show in folder").clicked() {
                                clips::open(path, true);
                            }
                            if ui.add(egui::Button::new("Rename...").shortcut_text("F2")).clicked() {
                                self.start_rename(path);
                            }
                            if !item.is_picture && !self.exports.contains_key(path) {
                                ui.menu_button("Export for sharing", |ui| {
                                    for (share, text, hint) in SHARES {
                                        if ui.button(text).on_hover_text(hint).clicked() {
                                            act.export = Some((path.clone(), share));
                                        }
                                    }
                                });
                            }
                            if self.cfg.backup {
                                let cloud = item.tags.iter().find(|t| clips::is_cloud(t)).cloned();
                                let mut on = cloud.is_some();
                                if ui.checkbox(&mut on, "Back up to cloud").on_hover_text("Copies it to your cloud folder and tags it Cloud").clicked() {
                                    act.tag = Some((path.clone(), cloud.unwrap_or_else(|| clips::CLOUD.into()), on));
                                }
                            }
                            ui.menu_button("Tags", |ui| {
                                for t in self.tags_in_use().into_iter().filter(|t| !(self.cfg.backup && clips::is_cloud(t))) {
                                    let mut on = item.tags.contains(&t);
                                    if ui.checkbox(&mut on, t.as_str()).clicked() {
                                        act.tag = Some((path.clone(), t, on));
                                    }
                                }
                                if ui.button("New tag...").clicked() {
                                    self.tagging = Some((path.clone(), String::new()));
                                }
                            });
                            if upload::is_discord_webhook(&self.cfg.discord_webhook)
                                && !self.uploads.contains_key(path)
                                && ui.button("Post to Discord").on_hover_text("Sends the file to your webhook's channel (up to 10 MB)").clicked()
                            {
                                let c = ui.ctx().clone();
                                self.uploads.insert(path.clone(), upload::post_discord(path, &self.cfg.discord_webhook, move || c.request_repaint()));
                            }
                            if self.links.contains_key(&path.display().to_string())
                                && ui.button("Delete share link").on_hover_text("Takes the uploaded copy off kappa.lol").clicked()
                            {
                                let c = ui.ctx().clone();
                                self.uploads.insert(path.clone(), upload::delete(path, move || c.request_repaint()));
                            }
                            if ui.add(egui::Button::new("Move to Recycle Bin").shortcut_text("Del")).clicked() {
                                act.recycle = Some(path.clone());
                            }
                        })
                        .response
                        .on_hover_text("More");
                        let (glyph, color, hint) = if item.favorite {
                            (icon::STAR_FILLED, AMBER, "Starred. Click to unstar.")
                        } else {
                            (icon::STAR, MUTED, "Star it: keeps it at the top and safe from auto-cleanup")
                        };
                        if ui.add(egui::Button::new(icon(glyph, 14.0).color(color)).frame(false)).on_hover_text(hint).clicked() {
                            act.star = Some(path.clone());
                        }
                        if ui.add(egui::Button::new(icon(icon::COPY, 14.0).color(MUTED)).frame(false)).on_hover_text("Copy the file, then paste it into Discord, a chat or a folder").clicked() {
                            let name = path.file_name().unwrap_or_default().to_string_lossy();
                            self.clips_msg = Some(match clips::copy_file(path) {
                                // Discord's free upload limit.
                                Ok(()) if item.bytes > 10 << 20 => format!("Copied {name}. Paste it anywhere; Discord takes files over 10 MB only with Nitro."),
                                Ok(()) => format!("Copied {name}. Paste it into Discord, a chat or a folder."),
                                Err(e) => format!("Couldn't copy it: {e:#}"),
                            });
                        }
                    });
                });
            });
        })));
        if shown.response.clicked() {
            self.selected = Some(path.clone());
        }
        let hovered = ui.rect_contains_pointer(shown.response.rect);
        if hovered != lifted {
            ui.ctx().data_mut(|d| d.insert_temp(hover_id, hovered));
            ui.ctx().request_repaint();
        }
        shown.response
    }

    /// Upload, its progress, or Copy link once uploaded.
    fn share_button(&mut self, ui: &mut egui::Ui, path: &std::path::Path, bytes: u64) {
        if let Some(job) = self.uploads.get(path) {
            use std::sync::atomic::Ordering::Relaxed;
            let (sent, total) = (job.sent.load(Relaxed), job.total.load(Relaxed).max(1));
            ui.label(RichText::new(if total > 1 { format!("Uploading {}%", sent * 100 / total) } else { "Working...".into() }).small().color(accent_light()));
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
            return;
        }
        if let Some(l) = self.links.get(&path.display().to_string()).cloned() {
            if ui.add(icon_button(icon::SHARE, "Copy link").small()).on_hover_text(&l.link).clicked() {
                ui.ctx().copy_text(l.link.clone());
                self.clips_msg = Some(format!("Copied {}", l.link));
            }
        } else if bytes <= upload::MAX_BYTES {
            if ui.add(icon_button(icon::SHARE, "Share").small()).on_hover_text("Upload it and get a link (kappa.lol, up to 100 MB)").clicked() {
                self.confirm_upload = Some(path.to_path_buf());
            }
        }
    }
    /// Makes a copy of `path` for sharing in the background, next to it.
    fn start_export(&mut self, path: &std::path::Path, share: media::Share, ctx: &egui::Context) {
        let src = path.to_path_buf();
        let what = format!("Exported {} to", src.display());
        let job = ExportJob::spawn(clips::copy_name(path, share.label()), what, ctx, move |job, out| media::export(&src, out, share, &|f| job.progress(f)));
        self.exports.insert(path.to_path_buf(), job);
    }

    /// Picks up finished exports: says where the copy went and offers to copy it.
    fn finish_exports(&mut self) {
        let done: Vec<_> = self.exports.iter().filter_map(|(p, j)| j.done.lock().unwrap().take().map(|r| (p.clone(), r))).collect();
        for (path, result) in done {
            self.exports.remove(&path);
            match result {
                Ok(out) => self.show_saved(out),
                Err(e) => self.clips_msg = Some(format!("Couldn't export it: {e}")),
            }
        }
    }

    /// Says a new file was saved, with a Copy button for it, and lists it.
    fn show_saved(&mut self, out: std::path::PathBuf) {
        let msg = saved_note(&out);
        self.clips_msg = Some(msg.clone());
        self.exported = Some((msg, out));
        self.clips_checked -= std::time::Duration::from_secs(60);
    }

    /// Montage mode: the clips picked, in order (each can move up, down or off), fades, music, and
    /// Make montage, which joins them into a new clip in the clips folder in the background.
    fn montage_panel(&mut self, ui: &mut egui::Ui) {
        use std::sync::atomic::Ordering::Relaxed;
        let finished = self.montage.as_ref().and_then(|m| m.job.as_ref()).and_then(|j| Some((j.done.lock().unwrap().take()?, j.stop.load(Relaxed))));
        let Some(m) = &mut self.montage else { return };
        match finished {
            Some((Ok(out), _)) => {
                self.show_saved(out);
                self.montage = None;
                return;
            }
            Some((Err(e), stopped)) => {
                self.clips_msg = Some(if stopped { "Montage stopped.".into() } else { format!("Couldn't make the montage: {e}") });
                m.job = None;
            }
            None => {}
        }
        let busy = m.job.is_some();
        card(ui, "Montage", |ui| {
            if m.clips.is_empty() {
                ui.label(RichText::new("Click clips to add them in the order they should play, or pick one with the arrow keys and press Space. Two clips without fades are simply joined into one.").color(MUTED));
            }
            // A clip to move up (-1), down (1) or take out (0).
            let mut change = None;
            let n = m.clips.len();
            for (i, path) in m.clips.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{}.", i + 1)).color(MUTED));
                    ui.label(path.file_stem().unwrap_or_default().to_string_lossy());
                    if let Some(len) = self.lengths.get(path) {
                        ui.label(RichText::new(mmss(len.round() as u32)).small().color(MUTED));
                    }
                    if bare_icon(ui, i > 0 && !busy, icon::UP, "Earlier") {
                        change = Some((i, -1));
                    }
                    if bare_icon(ui, i + 1 < n && !busy, icon::DOWN, "Later") {
                        change = Some((i, 1));
                    }
                    if bare_icon(ui, !busy, icon::CANCEL, "Take it out") {
                        change = Some((i, 0));
                    }
                });
            }
            match change {
                Some((i, 0)) => _ = m.clips.remove(i),
                Some((i, d)) => m.clips.swap(i, i.saturating_add_signed(d)),
                None => {}
            }
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled(!busy, egui::Checkbox::new(&mut m.fades, "Fade between clips"));
                ui.separator();
                let name = m.music.as_ref().map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned());
                let pick = ui.add_enabled(!busy, icon_button(icon::MUSIC, name.as_deref().unwrap_or("Add music"))).on_hover_text("An MP3, M4A or WAV under the clips' sound, looped or cut to fit, faded out at the end");
                if pick.clicked()
                    && let Some(f) = rfd::FileDialog::new().set_title("Music for the montage").add_filter("Music", &["mp3", "m4a", "wav"]).pick_file()
                {
                    m.music = Some(f);
                }
                if m.music.is_some() {
                    if bare_icon(ui, !busy, icon::CANCEL, "No music") {
                        m.music = None;
                    }
                    let mut pct = m.volume * 100.0;
                    if ui.add_enabled(!busy, egui::Slider::new(&mut pct, 0.0..=100.0).suffix("% music").integer()).changed() {
                        m.volume = pct / 100.0;
                    }
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let text = match &m.job {
                    Some(j) => format!("Making montage {}%", j.pct.load(Relaxed)),
                    None => "Make montage".into(),
                };
                if primary(ui, !busy && !m.clips.is_empty(), RichText::new(text).strong(), egui::Vec2::ZERO).clicked() {
                    let out = clips::montage_name(std::path::Path::new(&self.cfg.folder), std::time::SystemTime::now());
                    let music = m.music.clone().map(|path| media::Music { path, volume: m.volume });
                    let (list, fades) = (m.clips.clone(), m.fades);
                    let what = format!("Made a montage of {} clips at", list.len());
                    m.job = Some(ExportJob::spawn(out, what, ui.ctx(), move |job, out| {
                        // Each whole clip with track 1's sound, the way it plays.
                        let parts = list.iter().map(|p| Ok(media::Part { path: p.clone(), start: 0.0, end: f64::INFINITY, gains: media::track1(media::audio_tracks(p)?) }));
                        let parts = parts.collect::<anyhow::Result<Vec<_>>>()?;
                        media::join(&parts, out, fades, music.as_ref(), &|f| job.progress(f), &|| job.stop.load(Relaxed))
                    }));
                }
                if let Some(j) = &m.job {
                    if ui.button("Stop").clicked() {
                        j.stop.store(true, Relaxed);
                    }
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
                }
            });
        });
    }

    /// Picks up finished uploads and deletes.
    fn finish_uploads(&mut self, ctx: &egui::Context) {
        let done: Vec<(std::path::PathBuf, Result<String, String>)> =
            self.uploads.iter().filter_map(|(p, j)| j.done.lock().unwrap().take().map(|r| (p.clone(), r))).collect();
        for (path, result) in done {
            self.uploads.remove(&path);
            self.links = upload::links();
            self.clips_msg = Some(match result {
                Ok(s) if s == "deleted" => "Link deleted. The clip is no longer on kappa.lol.".into(),
                Ok(s) if s == "posted" => format!("Posted {} to Discord.", path.file_name().unwrap_or_default().to_string_lossy()),
                Ok(link) => {
                    ctx.copy_text(link.clone());
                    format!("Uploaded. Link copied: {link}")
                }
                Err(e) => e,
            });
        }
    }

    /// An open clip, filling the Clips page: back to the grid, the video as big as fits with
    /// room for the controls, play/pause and time, then the seek bar or the trim tools.
    fn player_page(&mut self, ui: &mut egui::Ui) {
        // A trim only belongs to the clip it was started on.
        if self.trim.as_ref().is_some_and(|t| self.player.as_ref().is_none_or(|(p, _)| *p != t.path)) {
            self.trim = None;
        }
        self.finish_trim();
        let Some((path, p)) = &mut self.player else { return };
        let path = path.clone();
        // Space plays and pauses, unless a text box has the keyboard. Taken before any button is
        // drawn, so a focused button doesn't also get pressed by it.
        let range = self.trim.as_ref().map(|t| (t.start, t.end));
        if !ui.ctx().egui_wants_keyboard_input() {
            use egui::Key;
            let [space, j, k, l, back, on] = ui.input_mut(|i| [Key::Space, Key::J, Key::K, Key::L, Key::Comma, Key::Period].map(|key| i.consume_key(egui::Modifiers::NONE, key)));
            let playing = p.is_playing() && !p.ended();
            if space {
                if playing { p.pause() } else { start_playing(p, range) }
            }
            // K stops (back to normal speed). L plays, then 2x, 4x; J plays at half speed, then
            // slower, or steps L's speed back down. Windows' player can't play these clips backwards.
            if k {
                p.pause();
                p.set_rate(1.0);
            }
            if l || j {
                let rate = match (playing, l) {
                    (false, true) => 1.0,
                    (false, false) => 0.5,
                    (true, true) => (p.rate() * 2.0).min(4.0),
                    (true, false) => (p.rate() / 2.0).max(0.25),
                };
                p.set_rate(rate);
                if !playing {
                    start_playing(p, range);
                }
            }
            // , and . step a frame back or on.
            if back || on {
                p.step(on);
            }
        }
        let (mut close, mut begin_trim) = (false, false);
        let trimming = self.trim.is_some();
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        ui.horizontal(|ui| {
            if ui.add(icon_button(icon::BACK, "Clips")).clicked() {
                close = true;
            }
            ui.add_space(6.0);
            ui.add(egui::Label::new(strong(&name, 17.0).color(Color32::WHITE)).truncate()).on_hover_text(&name);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !trimming && ui.add(icon_button(icon::CUT, "Trim")).clicked() {
                    begin_trim = true;
                }
                if ui.add(icon_button(icon::FOLDER, "Show in folder")).clicked() {
                    clips::open(&path, true);
                }
            });
        });
        ui.add_space(12.0);
        // The video gets what the controls under it leave.
        let reserve = if trimming { 330.0 } else { 90.0 };
        let w = ui.available_width();
        let h = (w * 9.0 / 16.0).min(ui.available_height() - reserve).max(160.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
        ui.painter().rect_filled(rect, 10.0, Color32::BLACK);
        let ppp = ui.ctx().pixels_per_point();
        p.set_rect((rect.left() * ppp) as i32, (rect.top() * ppp) as i32, (rect.width() * ppp) as i32, (rect.height() * ppp) as i32);
        let len = p.duration().unwrap_or(0.0);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let playing = p.is_playing() && !p.ended();
            let (glyph, text) = if playing { (icon::PAUSE, "Pause") } else { (icon::PLAY, "Play") };
            let keys = "Space plays and pauses. J slower, K stop, L faster. , and . step one frame.";
            if ui.add(icon_button(glyph, text).min_size(egui::vec2(96.0, 32.0))).on_hover_text(keys).clicked() {
                if playing { p.pause() } else { start_playing(p, range) }
            }
            ui.label(RichText::new(format!("{} / {}", secs_text(p.position()), secs_text(len))).color(MUTED));
            if p.rate() != 1.0 {
                ui.label(RichText::new(format!("{}x  ·  no sound", p.rate())).color(AMBER)).on_hover_text("Sound plays only at normal speed. K goes back to it.");
            }
        });
        if !trimming {
            let mut at = p.position();
            ui.spacing_mut().slider_width = ui.available_width() - 8.0;
            if ui.add(egui::Slider::new(&mut at, 0.0..=len.max(0.1)).show_value(false)).changed() {
                p.seek(at);
            }
        }
        if p.is_playing() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
        } else if p.stepping() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
        }
        if trimming {
            ui.add_space(8.0);
            egui::ScrollArea::vertical().id_salt("trim").auto_shrink(false).show(ui, |ui| self.trim_panel(ui));
        }
        if begin_trim {
            self.begin_trim(&path);
        }
        if close {
            self.player = None;
            self.trim = None;
        }
    }

    /// Opens `path` paused in the player with the whole clip selected for trimming.
    fn begin_trim(&mut self, path: &std::path::Path) {
        if self.player.as_ref().is_none_or(|(p, _)| p != path) {
            self.play(path);
        }
        match &mut self.player {
            Some((p, player)) if p == path => {
                player.pause();
                let len = player.duration().or_else(|| media::duration(path).ok()).unwrap_or(0.0);
                self.clips_msg = None;
                let gains = media::track1(player.audio_tracks());
                player.set_track_gains(&gains);
                let wave: Arc<std::sync::Mutex<Option<Vec<f32>>>> = Arc::default();
                // The thread only holds a weak link, so closing the trim stops it.
                let (slot, src) = (Arc::downgrade(&wave), path.to_path_buf());
                std::thread::spawn(move || {
                    let peaks = media::peaks(&src, || slot.strong_count() > 0).unwrap_or_else(|e| {
                        log::write(&format!("No waveform for {}: {e:#}", src.display()));
                        Vec::new()
                    });
                    if let Some(s) = slot.upgrade() {
                        *s.lock().unwrap() = Some(peaks);
                    }
                });
                self.trim = Some(TrimEdit { path: path.to_path_buf(), len, start: 0.0, end: len, view: (0.0, len), gains, want_split: false, wave, job: None, parts: Vec::new(), gif: None, gif_msg: None, loud: None });
            }
            _ => self.clips_msg = Some("Can't open that clip here for trimming.".into()),
        }
    }

    /// The trim tools for the clip in the player, under the video.
    fn trim_panel(&mut self, ui: &mut egui::Ui) {
        let split_on = self.cfg.track_count > 1;
        let (Some(t), Some((_, p))) = (&mut self.trim, &mut self.player) else { return };
        let mut keep = true;
        let cfg = &self.cfg;
        card(ui, "Trim", |ui| keep = trim_controls(ui, p, t, split_on, cfg));
        if t.want_split {
            // Set it up and show it in the Audio tab, where Save puts it into effect.
            self.split_tracks();
            self.tab = Tab::Audio;
            keep = false;
        }
        if !keep {
            self.trim = None;
        }
    }

    /// Picks up a finished cut (closes the trim) or GIF, and lists the new file.
    fn finish_trim(&mut self) {
        let Some(t) = &mut self.trim else { return };
        // A GIF leaves the trim open, to make another or save the cut too.
        if let Some(done) = t.gif.as_ref().and_then(|g| g.done.lock().unwrap().take()) {
            t.gif = None;
            t.gif_msg = Some(match done {
                Ok(out) => {
                    self.clips_checked -= std::time::Duration::from_secs(60);
                    saved_note(&out)
                }
                Err(e) => format!("Couldn't make the GIF: {e}"),
            });
        }
        let Some(job) = &t.job else { return };
        let done = job.done.lock().unwrap().take();
        match done {
            Some(Ok(out)) => {
                self.clips_msg = Some(format!("Saved {}. The original is kept.", out.file_name().unwrap_or_default().to_string_lossy()));
                self.clips_checked -= std::time::Duration::from_secs(60);
                self.trim = None;
            }
            Some(Err(e)) => {
                self.clips_msg = Some(format!("Trim didn't work: {e}"));
                t.job = None;
            }
            None => {}
        }
    }

    /// Opens `path` in the window's player, or in the default app if it can't be played here.
    fn play(&mut self, path: &std::path::Path) {
        self.player = None; // lets go of the old file first
        let opened = overlay::raw().ok_or_else(|| anyhow::anyhow!("no window")).and_then(|h| player::Player::open(h, path));
        match opened {
            Ok(mut p) => {
                p.play();
                self.player = Some((path.to_path_buf(), p));
            }
            Err(e) => {
                log::write(&format!("Couldn't play {} here ({e:#}); opened it in the default app.", path.display()));
                clips::open(path, false);
            }
        }
    }

    /// Renames a clip, keeping its star, tags and share link.
    fn rename_clip(&mut self, path: &std::path::Path, name: &str) {
        // The player holds the file open.
        if self.player.as_ref().is_some_and(|(p, _)| p == path) {
            self.player = None;
            self.trim = None;
        }
        self.clips_msg = Some(match clips::rename(path, name) {
            Ok(new) => {
                clips::moved(&mut self.favorites, &mut self.tags, path, Some(&new));
                self.links = upload::links();
                log::write(&format!("Renamed {} to {}", path.display(), new.display()));
                format!("Renamed to {}.", new.file_name().unwrap_or_default().to_string_lossy())
            }
            Err(e) => format!("Couldn't rename it: {e:#}"),
        });
        self.clips_checked -= std::time::Duration::from_secs(60);
    }

    /// The rename box, the upload notice or the Remove confirmation is up.
    fn dialog_open(&self) -> bool {
        self.confirm_upload.is_some() || self.confirm_remove || self.renaming.is_some() || self.tagging.is_some() || self.import.is_some()
    }

    /// The tags on clips in the clips folder, sorted. Tags of clips that are gone (auto-cleanup,
    /// archive) stay in tags.json but aren't offered.
    fn tags_in_use(&self) -> Vec<String> {
        let listed = |p: &String| self.clip_list.iter().any(|c| c.path.as_os_str() == p.as_str());
        let mut tags: Vec<String> = self.tags.iter().filter(|(p, _)| listed(p)).flat_map(|(_, t)| t).cloned().collect();
        tags.sort_by_key(|t| t.to_lowercase());
        tags.dedup();
        tags
    }

    /// Opens the rename box for `path`, with its current name.
    fn start_rename(&mut self, path: &std::path::Path) {
        let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        self.renaming = Some((path.to_path_buf(), stem));
    }

    /// The rename box, the upload notice and the Remove confirmation.
    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some((path, mut name)) = self.renaming.take() {
            let mut done = None;
            let r = egui::Modal::new(egui::Id::unique("rename_clip")).show(ctx, |ui| {
                ui.set_max_width(380.0);
                ui.label(RichText::new("Rename clip").strong().size(16.0));
                let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
                let edit = ui.horizontal(|ui| {
                    let edit = ui.add(egui::TextEdit::singleline(&mut name).desired_width(300.0));
                    ui.label(RichText::new(&ext).color(MUTED));
                    edit
                });
                let edit = edit.inner;
                if !edit.has_focus() && !edit.lost_focus() {
                    edit.request_focus();
                }
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    if primary(ui, !name.trim().is_empty(), RichText::new("Rename").strong(), egui::Vec2::ZERO).clicked() || (enter && !name.trim().is_empty()) {
                        done = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        done = Some(false);
                    }
                });
            });
            match done {
                Some(true) => self.rename_clip(&path, &name),
                Some(false) => {}
                None if r.should_close() => {}
                None => self.renaming = Some((path, name)),
            }
        }
        if let Some((path, mut name)) = self.tagging.take() {
            let mut done = None;
            let r = egui::Modal::new(egui::Id::unique("new_tag")).show(ctx, |ui| {
                ui.set_max_width(320.0);
                ui.label(RichText::new("New tag").strong().size(16.0));
                let edit = ui.add(egui::TextEdit::singleline(&mut name).hint_text("Aces, Funny, ...").desired_width(280.0));
                if !edit.has_focus() && !edit.lost_focus() {
                    edit.request_focus();
                }
                let ok = !name.trim().is_empty();
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    if primary(ui, ok, RichText::new("Add").strong(), egui::Vec2::ZERO).clicked() || (enter && ok) {
                        done = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        done = Some(false);
                    }
                });
            });
            match done {
                Some(true) => {
                    clips::set_tag(&mut self.tags, &path, &name, true);
                    clips::save_tags(&self.tags);
                }
                Some(false) => {}
                None if r.should_close() => {}
                None => self.tagging = Some((path, name)),
            }
        }
        if let Some(path) = self.confirm_upload.clone() {
            let r = egui::Modal::new(egui::Id::unique("upload_notice")).show(ctx, |ui| {
                ui.set_max_width(360.0);
                ui.label(RichText::new("Upload for a share link?").strong().size(16.0));
                ui.label("The clip goes to kappa.lol, a free third-party host. Anyone with the link can watch it. You can delete it from there later with Delete link. Max 100 MB.");
                ui.horizontal(|ui| {
                    if primary(ui, true, RichText::new("Upload").strong(), egui::Vec2::ZERO).clicked() {
                        let c = ctx.clone();
                        self.uploads.insert(path.clone(), upload::start(&path, move || c.request_repaint()));
                        self.confirm_upload = None;
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_upload = None;
                    }
                });
            });
            if r.should_close() {
                self.confirm_upload = None;
            }
        }
        if let Some((file, missing)) = self.import.take() {
            let new = file.applied_to(&self.cfg);
            let changed = config::changes(&self.cfg, &new);
            let stars = file.favorites.difference(&self.favorites).count();
            let mut done = None;
            let r = egui::Modal::new(egui::Id::unique("import_settings")).show(ctx, |ui| {
                ui.set_max_width(420.0);
                ui.label(RichText::new("Import settings?").strong().size(16.0));
                ui.label(if changed.is_empty() { "No settings change.".to_string() } else { format!("Changes: {}.", changed.join(", ")) });
                if stars > 0 || !file.tags.is_empty() {
                    ui.label(format!("Adds {stars} stars and tags on {} clips.", file.tags.len()));
                }
                for f in &missing {
                    ui.label(RichText::new(format!("{f} isn't on this PC. Pick another folder, or create it, before you Save.")).small().color(AMBER));
                }
                ui.label(RichText::new("Nothing changes until you Save. Passwords, the phone key and the Discord webhook stay as they are here.").small().color(MUTED));
                ui.horizontal(|ui| {
                    if primary(ui, true, RichText::new("Import").strong(), egui::Vec2::ZERO).clicked() {
                        done = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        done = Some(false);
                    }
                });
            });
            match done {
                Some(true) => {
                    self.cfg = new;
                    self.favorites.extend(file.favorites);
                    clips::save_favorites(&self.favorites);
                    for (path, tags) in file.tags {
                        self.tags.entry(path).or_default().extend(tags);
                    }
                    clips::save_tags(&self.tags);
                    self.register_overlay_hotkey();
                }
                Some(false) => {}
                None if r.should_close() => {}
                None => self.import = Some((file, missing)),
            }
        }
        if self.confirm_remove {
            let r = egui::Modal::new(egui::Id::unique("remove_confirm")).show(ctx, |ui| {
                ui.set_max_width(380.0);
                ui.label(RichText::new("Remove Replay Vault from this PC?").strong().size(16.0));
                ui.label("This stops recording and puts back everything the app changed: Start with Windows, Game Bar and graphics settings, and the firewall rule (Windows asks first). The private OBS copy and the app's settings go to the Recycle Bin, then the app closes. Your clips and your own OBS stay as they are.");
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new(RichText::new("Remove").strong().color(Color32::WHITE)).fill(RED)).clicked() {
                        self.remove_everything();
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_remove = false;
                    }
                });
            });
            if r.should_close() {
                self.confirm_remove = false;
            }
        }
    }

    /// Undoes everything the app did to this PC, then quits. Clips are kept.
    fn remove_everything(&mut self) {
        self.player = None;
        recorder::stop();
        let mut problems = Vec::new();
        if let Err(e) = tweaks::restore_all() {
            problems.push(format!("Windows settings: {e:#}"));
        }
        if let Err(e) = startup::set_enabled(false) {
            problems.push(format!("Start with Windows: {e:#}"));
        }
        if let Err(e) = obs_install::remove_firewall_rule() {
            problems.push(format!("Firewall rule: {e:#}"));
        }
        let private = obs_install::private_dir();
        if private.exists() {
            if let Err(e) = clips::recycle(&private) {
                problems.push(format!("Private OBS copy at {}: {e:#}", private.display()));
            }
        }
        if !problems.is_empty() {
            // Left on the Desktop, since the settings folder is about to go.
            let note = dirs::desktop_dir().unwrap_or_default().join("Replay Vault - remove notes.txt");
            let _ = std::fs::write(&note, format!("Removed, except:\r\n{}\r\n", problems.join("\r\n")));
        }
        let _ = clips::recycle(&config::dir());
        std::process::exit(0);
    }

    /// Re-reads the app log when it changes, at most once a second.
    fn refresh_log(&mut self) {
        if self.log_checked.elapsed() < std::time::Duration::from_secs(1) {
            return;
        }
        self.log_checked = std::time::Instant::now();
        let stamp = std::fs::metadata(log::path()).map(|m| (m.len(), m.modified().ok())).unwrap_or_default();
        if stamp == self.log_stamp {
            return;
        }
        self.log_stamp = stamp;
        let text = String::from_utf8_lossy(&std::fs::read(log::path()).unwrap_or_default()).into_owned();
        let lines: Vec<&str> = text.lines().collect();
        self.log_view = lines[lines.len().saturating_sub(500)..].iter().map(|l| l.to_string()).collect();
    }

    /// Zips the logs, settings (no secrets) and a PC summary onto the Desktop and shows it there.
    fn support_zip(&mut self) {
        let out = dirs::desktop_dir().unwrap_or_default().join("Replay Vault support.zip");
        let dir = config::dir();
        let mut files = vec![("app.log", log::path()), ("app.old.log", dir.join("app.old.log")), ("tweaks.json", dir.join("tweaks.json"))];
        files.extend(recorder::obs_log().map(|p| ("obs.log", p)));
        match support::write(&out, &files, &self.saved, &support::summary(&self.hw, &self.monitors)) {
            Ok(()) => {
                self.status = "Support zip saved to your Desktop.".into();
                clips::open(&out, true);
            }
            Err(e) => self.status = format!("Couldn't make the support zip: {e}"),
        }
    }

    fn logs_tab(&mut self, ui: &mut egui::Ui) {
        self.refresh_log();
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));

        let perf = self.perf.get_or_insert_with(perf::Sampler::new);
        perf.tick(recorder::pid());
        let (rec, app) = (perf.recorder, perf.app);
        card(ui, "Performance", |ui| {
            let cell = |ui: &mut egui::Ui, v: Option<String>| {
                ui.label(RichText::new(v.unwrap_or("-".into())).color(Color32::WHITE));
            };
            egui::Grid::new("perf").num_columns(5).spacing([28.0, 8.0]).min_col_width(70.0).show(ui, |ui| {
                for head in ["", "CPU", "GPU", "Video encoder", "Memory"] {
                    ui.label(RichText::new(head).small().color(MUTED));
                }
                ui.end_row();
                for (name, u) in [("Recorder (OBS)", rec), ("Replay Vault", app)] {
                    ui.label(name);
                    cell(ui, u.map(|u| format!("{:.1}%", u.cpu)));
                    cell(ui, u.map(|u| format!("{:.1}%", u.gpu)));
                    cell(ui, u.map(|u| format!("{:.1}%", u.encoder)));
                    cell(ui, u.map(|u| if u.ram_mb >= 1024.0 { format!("{:.2} GB", u.ram_mb / 1024.0) } else { format!("{:.0} MB", u.ram_mb) }));
                    ui.end_row();
                }
            });
            ui.add_space(2.0);
            let note = if rec.is_none() {
                "The recorder isn't running."
            } else {
                "Live, while this page is open. CPU is a share of all cores. The video encoder is a separate chip on the GPU that games don't use. Memory is mostly the replay buffer."
            };
            ui.label(RichText::new(note).small().color(MUTED));
        });

        let report = self.log_view.iter().rev().find(|l| l.contains("SESSION REPORT") || l.contains("Hourly check"));
        card(ui, "Last health report", |ui| match report {
            Some(line) => {
                let ok = line.ends_with("Health: OK");
                ui.label(RichText::new(if ok { "✔ Healthy" } else { "⚠ Needs a look" }).strong().color(if ok { accent_light() } else { AMBER }));
                ui.label(RichText::new(line).small().color(MUTED));
            }
            None => {
                ui.label(RichText::new("No report yet. One is written when replay turns off, when the app closes, and every hour.").small().color(MUTED));
            }
        });

        card(ui, "Log", |ui| {
            ui.horizontal(|ui| {
                if ui.button("Restart recorder").on_hover_text("Starts the hidden recorder again. The current replay buffer is lost.").clicked() {
                    self.apply();
                }
                if ui.button("Open in Notepad").clicked() {
                    open_in_notepad(&log::path());
                }
                if let Some(obs_log) = recorder::obs_log() {
                    if ui.button("Open OBS log").on_hover_text("The hidden recorder's own log for its current run").clicked() {
                        open_in_notepad(&obs_log);
                    }
                }
                if ui.button("Make support zip").on_hover_text("Logs, settings (no passwords or keys) and a PC summary in one zip on your Desktop, to send when asking for help").clicked() {
                    self.support_zip();
                }
            });
            ui.add_space(6.0);
            egui::ScrollArea::vertical()
                .id_salt("log")
                .auto_shrink(false)
                .max_height((ui.available_height() - 12.0).max(120.0))
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    if self.log_view.is_empty() {
                        ui.label(RichText::new("Nothing logged yet.").small().color(MUTED));
                    }
                    for line in &self.log_view {
                        let color = if line.contains("Error") || line.contains("CRASH") || line.contains("didn't") || line.contains("Health: CHECK") {
                            RED
                        } else if line.contains("Clip saved") || line.contains("Health: OK") {
                            accent_light()
                        } else {
                            MUTED
                        };
                        ui.add(egui::Label::new(RichText::new(line).monospace().size(11.0).color(color)).wrap());
                    }
                });
        });
    }

    fn refresh_devices(&mut self) {
        self.devices = devices::list();
        self.open_apps = devices::open_apps();
        for d in &self.devices {
            self.cfg.devices.entry(d.key.clone()).or_default().name = d.name.clone();
        }
    }

    /// Three tracks: everything, then game and PC sound, then the mic. Takes effect on Save.
    fn split_tracks(&mut self) {
        self.cfg.track_count = 3;
        for (key, d) in self.cfg.devices.iter_mut() {
            if d.tracks.iter().any(|&t| t) {
                d.tracks = if key.starts_with("in:") { [true, false, true] } else { [true, true, false] };
            }
        }
    }

    /// A one-line hint at the top of a page, until its x is clicked. Kept right away, no Save.
    fn tip(&mut self, ui: &mut egui::Ui, page: &str) {
        if !self.cfg.show_tips || self.cfg.tips_seen.iter().any(|p| p == page) {
            return;
        }
        let text = match self.tab {
            Tab::Clips => format!("Press {} in a game to save the last {}. Click a card to play it.", self.cfg.hotkey, mmss(self.cfg.buffer_secs)),
            Tab::General => "Changes here wait for the Save button at the bottom.".into(),
            Tab::Keybinds => "Click a key, then press the keys you want. They work even in a game.".into(),
            Tab::Video => "Not sure what to pick? Basic sets this PC up in one click.".into(),
            Tab::Audio => "Put your mic on its own track to change its volume when you trim.".into(),
            Tab::Logs => "If clips stop saving, the health report and log here say why.".into(),
        };
        let mut close = false;
        egui::Frame::new().fill(CARD.lerp_to_gamma(accent(), 0.1)).corner_radius(8).inner_margin(egui::Margin::symmetric(12, 6)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(icon(icon::TIP, 14.0).color(accent_light()));
                ui.label(text);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = bare_icon(ui, true, icon::CANCEL, "Got it");
                });
            });
        });
        ui.add_space(12.0);
        if close {
            self.cfg.tips_seen.push(page.to_string());
            self.saved.tips_seen = self.cfg.tips_seen.clone();
            if let Err(e) = self.saved.save() {
                self.status = format!("Could not save settings: {e}");
            }
        }
    }

    /// Basic | Advanced, at the top of Video and Audio. A view choice: kept right away, no Save.
    fn view_switch(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let mut advanced = self.cfg.advanced;
            chip(ui, &mut advanced, false, "Basic");
            chip(ui, &mut advanced, true, "Advanced");
            note(ui, if advanced { "Every setting." } else { "Just the essentials; the rest is picked for you." });
            if advanced != self.cfg.advanced {
                self.cfg.advanced = advanced;
                self.saved.advanced = advanced;
                if let Err(e) = self.saved.save() {
                    self.status = format!("Could not save settings: {e}");
                }
            }
        });
        ui.add_space(10.0);
    }

    /// Video for everyone: a one-click setup for this PC, then a few plain choices.
    fn basic_video(&mut self, ui: &mut egui::Ui, mon: &display::Monitor) {
        card(ui, "Set up for this PC", |ui| {
            ui.label(RichText::new(self.hw.summary()).strong());
            note(ui, "Picks the size, smoothness, quality and clip length this PC handles well.");
            ui.add_space(4.0);
            if ui.button("Use recommended settings").clicked() {
                let s = profile::apply(&mut self.cfg, &self.hw, mon);
                log::write(&format!("Detected {}. Recommended settings: {s}", self.hw.summary()));
                self.recommended = Some(s);
            }
            if let Some(s) = &self.recommended {
                ui.label(RichText::new(format!("✔ {s}")).small().color(accent_light()));
            }
        });
        card(ui, "Your clips", |ui| {
            form(ui, "basic_video", |ui| {
                if self.monitors.len() > 1 {
                    ui.label("Screen");
                    monitor_combo(ui, &mut self.cfg, &self.monitors, "basic_monitor", mon);
                    ui.end_row();
                }

                ui.label("Size");
                ui.horizontal_wrapped(|ui| {
                    chip(ui, &mut self.cfg.out_height, 0, format!("Full screen ({}p)", mon.height));
                    if mon.height > 1080 {
                        chip(ui, &mut self.cfg.out_height, 1080, "1080p (smaller files)");
                    }
                    let h = self.cfg.out_height;
                    if h != 0 && !(h == 1080 && mon.height > 1080) {
                        chip(ui, &mut self.cfg.out_height, h, format!("{h}p"));
                    }
                });
                ui.end_row();

                ui.label("Smoothness");
                ui.horizontal_wrapped(|ui| {
                    chip(ui, &mut self.cfg.fps, 30, "30 fps (smaller files)");
                    chip(ui, &mut self.cfg.fps, 60, "60 fps (smooth)");
                    let f = self.cfg.fps;
                    if f != 30 && f != 60 {
                        let text = if f == 0 { format!("{} fps", mon.hz) } else { format!("{f} fps") };
                        chip(ui, &mut self.cfg.fps, f, text);
                    }
                });
                ui.end_row();

                ui.label("Quality");
                ui.horizontal_wrapped(|ui| {
                    for (i, q) in ["Highest", "High", "Medium", "Low"].into_iter().enumerate() {
                        chip(ui, &mut self.cfg.quality, i as u8, q);
                    }
                });
                ui.end_row();

                ui.label("Game performance");
                ui.horizontal_wrapped(|ui| {
                    for (i, name) in ["Best looking clips", "Balanced", "Most FPS"].into_iter().enumerate() {
                        chip(ui, &mut self.cfg.encoder_effort, i as u8, name);
                    }
                });
                ui.end_row();
            });
            note(ui, "Higher quality makes bigger files. \"Most FPS\" gives games a little more room when your graphics card is maxed out, for slightly blockier clips.");
            self.encoder_hint(ui, "Game performance", ["Best looking clips", "Balanced", "Most FPS"]);
            let p = recorder::plan(&self.cfg, mon);
            ui.add_space(4.0);
            ui.label(RichText::new(format!(
                "Clips: {}x{} at {} fps, about {} Mbps, up to {:.1} GB of memory while recording.",
                p.out_w, p.out_h, p.fps, p.kbps / 1000, p.mb as f64 / 1024.0
            )).small().color(accent_light()));
            ui.label(RichText::new(format!("{}.", recorder::space_estimate(&self.cfg, p.kbps))).small().color(accent_light()));
            self.hdr_note(ui, mon);
            if self.cfg.capture == "game" {
                ui.label(RichText::new("Game capture is on (changed in Advanced).").small().color(AMBER));
            }
        });
    }

    /// Audio for everyone: one sound source, one mic, and two plain switches.
    fn basic_audio(&mut self, ui: &mut egui::Ui) {
        let current = basic_audio(&self.cfg);
        // A setup Basic can't show (several devices, own track layout) is left as it is until
        // something is picked here.
        let mut pick = current.clone().unwrap_or_else(|| basic_audio_guess(&self.cfg));
        let custom = current.is_none();
        let name_of = |key: &Option<String>, none: &str| match key {
            Some(k) => self.devices.iter().find(|d| &d.key == k).map_or(k.clone(), |d| d.name.clone()),
            None => none.to_string(),
        };
        let (out_text, mic_text) = if custom {
            ("Custom (see Advanced)".to_string(), "Custom (see Advanced)".to_string())
        } else {
            (name_of(&pick.out, "Nothing"), name_of(&pick.mic, "No mic"))
        };
        let mut changed = false;
        card(ui, "Sound in your clips", |ui| {
            form(ui, "basic_audio", |ui| {
                ui.label("Game and PC sound");
                egui::ComboBox::from_id_salt("basic_out").selected_text(out_text).width(ui.available_width()).show_ui(ui, |ui| {
                    for d in self.devices.iter().filter(|d| d.key.starts_with("out:")) {
                        changed |= ui.selectable_value(&mut pick.out, Some(d.key.clone()), &d.name).clicked();
                    }
                    changed |= ui.selectable_value(&mut pick.out, None, "Nothing").clicked();
                });
                ui.end_row();

                ui.label("Microphone");
                egui::ComboBox::from_id_salt("basic_mic").selected_text(mic_text).width(ui.available_width()).show_ui(ui, |ui| {
                    for d in self.devices.iter().filter(|d| d.key.starts_with("in:")) {
                        changed |= ui.selectable_value(&mut pick.mic, Some(d.key.clone()), &d.name).clicked();
                    }
                    changed |= ui.selectable_value(&mut pick.mic, None, "No mic").clicked();
                });
                ui.end_row();
            });
            ui.add_space(2.0);
            let mut split = pick.split && pick.mic.is_some();
            if ui.add_enabled(pick.mic.is_some(), egui::Checkbox::new(&mut split, "Put my mic on its own track")).changed() {
                pick.split = split;
                changed = true;
            }
            note(ui, "Lets you turn your voice down or off when trimming. Clips still play with everything mixed.");
            let mut denoise = self.cfg.noise != "off";
            if ui.add_enabled(pick.mic.is_some(), egui::Checkbox::new(&mut denoise, "Reduce background noise on my mic")).changed() {
                self.cfg.noise = if denoise { "rnnoise" } else { "off" }.into();
            }
            if self.cfg.only_apps {
                ui.label(RichText::new("Only the apps you picked are recorded (changed in Advanced).").small().color(AMBER));
            }
            if custom {
                ui.add_space(4.0);
                ui.label(RichText::new("Your audio has a custom setup (more than one device, or your own tracks). Picking something here replaces it; Advanced shows all of it.").small().color(AMBER));
            }
            ui.add_space(4.0);
            if ui.button("Refresh devices").on_hover_text("Plugged in a headset or mic? Look for it again.").clicked() {
                self.refresh_devices();
            }
        });
        if changed {
            set_basic_audio(&mut self.cfg, &pick);
        }
    }

    fn audio_tab(&mut self, ui: &mut egui::Ui) {
        self.view_switch(ui);
        if !self.cfg.advanced {
            self.basic_audio(ui);
            return;
        }
        card(ui, "Tracks", |ui| {
            ui.horizontal(|ui| {
                ui.label("Tracks per clip");
                for n in 1..=3u8 {
                    chip(ui, &mut self.cfg.track_count, n, n.to_string());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Refresh devices").clicked() {
                        self.refresh_devices();
                    }
                });
            });
            // Editor-friendly layout in one click: a full mix that plays anywhere, plus game and mic apart.
            let has_mic = self.cfg.devices.iter().any(|(k, d)| k.starts_with("in:") && d.tracks.iter().any(|&t| t));
            if has_mic {
                ui.add_space(2.0);
                if ui.button("Split mic and game").clicked() {
                    self.split_tracks();
                }
                note(ui, "Track 1: everything (what players play). Track 2: game and PC sound. Track 3: your mic, so you can mute or lower it when editing.");
            }
            // Names for the tracks, shown on the device columns below and the trim sliders. Left
            // empty, a track is named after the devices ticked for it below.
            if self.cfg.track_count > 1 {
                ui.add_space(4.0);
                for i in 0..self.cfg.track_count as usize {
                    let auto = device_track_name(&self.cfg, i);
                    ui.horizontal(|ui| {
                        ui.label(format!("Track {} name", i + 1));
                        ui.add(egui::TextEdit::singleline(&mut self.cfg.track_names[i]).desired_width(160.0).char_limit(24).hint_text(auto));
                    });
                }
                note(ui, "Left empty, a track is named after the devices on it.");
            }
        });
        card(ui, "Devices", |ui| {
            let n = self.cfg.track_count as usize;
            // Names get whatever the track columns leave; long ones are cut with "..." (full name on hover).
            let heads: Vec<RichText> = (0..n).map(|i| RichText::new(track_name(&self.cfg, i)).small().color(MUTED)).collect();
            let cols: f32 = heads
                .iter()
                .map(|h| egui::WidgetText::from(h.clone()).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Small).size().x.max(20.0) + 14.0)
                .sum();
            let name_w = (ui.available_width() - cols - 2.0).max(120.0);
            egui::Grid::new("devices").striped(true).spacing([14.0, 6.0]).show(ui, |ui| {
                ui.label(RichText::new("Windows device").small().color(MUTED));
                for h in heads {
                    ui.label(h);
                }
                ui.end_row();
                for (heading, prefix) in [("Playback", "out:"), ("Recording", "in:")] {
                    ui.label(RichText::new(heading).strong().color(accent_light()));
                    ui.end_row();
                    for d in self.devices.iter().filter(|d| d.key.starts_with(prefix)) {
                        let sel = self.cfg.devices.entry(d.key.clone()).or_default();
                        ui.allocate_ui_with_layout(egui::vec2(name_w, 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.set_width(name_w);
                            ui.add(egui::Label::new(&d.name).truncate()).on_hover_text(&d.name);
                        });
                        for t in sel.tracks.iter_mut().take(n) {
                            ui.checkbox(t, "");
                        }
                        ui.end_row();
                    }
                }
            });
        });
        card(ui, "App sound", |ui| {
            ui.horizontal_wrapped(|ui| {
                chip(ui, &mut self.cfg.only_apps, false, "Everything (desktop audio)");
                chip(ui, &mut self.cfg.only_apps, true, "Only these apps");
            });
            if !self.cfg.only_apps {
                note(ui, "Records everything your playback devices play.");
                return;
            }
            let mut remove = None;
            for (i, a) in self.cfg.apps.iter().enumerate() {
                ui.horizontal(|ui| {
                    if bare_icon(ui, true, icon::CANCEL, "Remove") {
                        remove = Some(i);
                    }
                    ui.add(egui::Label::new(app_label(a)).truncate()).on_hover_text(&a.title);
                });
            }
            if let Some(i) = remove {
                self.cfg.apps.remove(i);
            }
            ui.horizontal(|ui| {
                let mut add = None;
                egui::ComboBox::from_id_salt("add_app").selected_text("Add an open app").width(260.0).show_ui(ui, |ui| {
                    for a in self.open_apps.iter().filter(|a| !self.cfg.apps.iter().any(|p| p.exe.eq_ignore_ascii_case(&a.exe))) {
                        if ui.add(egui::Button::selectable(false, app_label(a)).truncate()).on_hover_text(&a.title).clicked() {
                            add = Some(a.clone());
                        }
                    }
                });
                self.cfg.apps.extend(add);
                if ui.button("Refresh").on_hover_text("Just opened a game or app? Look for it again.").clicked() {
                    self.open_apps = devices::open_apps();
                }
            });
            if self.cfg.apps.is_empty() {
                ui.label(RichText::new("Pick at least one app, or clips have no game or PC sound.").small().color(AMBER));
            }
            note(ui, "Only these apps' sound is recorded, on the tracks ticked for your playback devices. Apps are matched by program, so a changing window title is fine. Your mic is set above.");
        });
        card(ui, "Microphone", |ui| {
            form(ui, "mic", |ui| {
                ui.label("Noise suppression");
                ui.horizontal_wrapped(|ui| {
                    for (v, name) in [("off", "Off"), ("rnnoise", "RNNoise"), ("speex", "Speex")] {
                        chip(ui, &mut self.cfg.noise, v.to_string(), name);
                    }
                });
                ui.end_row();
            });
            note(ui, "RNNoise cleans up better but uses more CPU; Speex is lighter.");
        });
    }

    /// The note after OBS skipped frames while encoding: suggests the next lighter effort. The
    /// button only changes the form; Save still puts it into effect.
    fn encoder_hint(&mut self, ui: &mut egui::Ui, setting: &str, names: [&str; 3]) {
        let Some((n, effort)) = self.encoder_hint else { return };
        let Some(lighter) = recorder::lighter_effort(effort).filter(|_| self.cfg.encoder_effort == effort) else { return };
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("OBS skipped {n} frames while encoding. Try {setting}: {}.", names[lighter as usize])).color(AMBER));
            if ui.button(format!("Use {}", names[lighter as usize])).clicked() {
                self.cfg.encoder_effort = lighter;
            }
            if bare_icon(ui, true, icon::CANCEL, "Dismiss") {
                self.encoder_hint = None;
            }
        });
    }

    /// A line when the recorded monitor (following the mouse: any monitor) has HDR on.
    fn hdr_note(&self, ui: &mut egui::Ui, mon: &display::Monitor) {
        if self.monitors.iter().any(|m| m.hdr.is_some() && (self.cfg.follows_mouse() || m.id == mon.id)) {
            note(ui, "HDR monitor: clips are converted to normal colours.");
        }
    }

    fn video_tab(&mut self, ui: &mut egui::Ui) {
        self.view_switch(ui);
        // From the list made at start (with HDR), not looked up again every frame.
        let Some(mon) = recorder::pick_monitor(&self.monitors, &self.cfg) else {
            ui.label(RichText::new("No monitor found.").color(MUTED));
            return;
        };
        if !self.cfg.advanced {
            self.basic_video(ui, &mon);
            return;
        }
        card(ui, "This PC", |ui| {
            ui.label(RichText::new(self.hw.summary()).strong());
            ui.add_space(4.0);
            match &self.recommended {
                Some(s) => {
                    ui.label(RichText::new(format!("✔ Recommended settings applied. {s}")).small().color(accent_light()));
                }
                None => {
                    note(ui, "Sets resolution, frame rate, quality and clip length to suit this GPU and RAM.");
                }
            }
            ui.add_space(4.0);
            if ui.button("Use recommended settings").clicked() {
                let s = profile::apply(&mut self.cfg, &self.hw, &mon);
                log::write(&format!("Detected {}. Recommended settings: {s}", self.hw.summary()));
                self.recommended = Some(s);
            }
        });
        // Optional: start from the video settings of the user's own OBS.
        if self.obs_settings_found {
            card(ui, "Your OBS", |ui| {
                match &self.imported {
                    Some(s) => _ = ui.label(RichText::new(format!("✔ {s}")).small().color(accent_light())),
                    None => note(ui, "Use the resolution, frame rate, encoder and quality from your own OBS. Your OBS itself isn't changed."),
                };
                ui.add_space(4.0);
                if ui.button("Import my OBS video settings").clicked() {
                    let s = obs_import::apply(&mut self.cfg, &mon).unwrap_or_else(|| "Nothing usable was found in your OBS video settings.".into());
                    log::write(&s);
                    self.imported = Some(s);
                }
            });
        }
        card(ui, "Capture", |ui| {
            form(ui, "capture", |ui| {
                ui.label("Mode");
                ui.horizontal_wrapped(|ui| {
                    chip(ui, &mut self.cfg.capture, "desktop".to_string(), "Desktop");
                    chip(ui, &mut self.cfg.capture, "game".to_string(), "Game capture");
                });
                ui.end_row();

                ui.label("Monitor");
                monitor_combo(ui, &mut self.cfg, &self.monitors, "monitor", &mon);
                ui.end_row();

                ui.label("Resolution");
                ui.horizontal_wrapped(|ui| {
                    chip(ui, &mut self.cfg.out_height, 0, format!("Native {}p", mon.height));
                    for h in [1440, 1080, 900, 720].into_iter().filter(|&h| h < mon.height) {
                        chip(ui, &mut self.cfg.out_height, h, format!("{h}p"));
                    }
                });
                ui.end_row();

                ui.label("Frame rate");
                ui.horizontal_wrapped(|ui| {
                    for f in [30, 60, 120, 144, 240] {
                        chip(ui, &mut self.cfg.fps, f, f.to_string());
                    }
                    chip(ui, &mut self.cfg.fps, 0, format!("Native {}", mon.hz));
                });
                ui.end_row();
            });
            if self.cfg.capture == "game" {
                ui.colored_label(AMBER, "Game capture hooks into the game. Anti-cheats like Vanguard (Valorant), FACEIT or ESEA may block it or flag it. Use Desktop for those games.");
            } else {
                note(ui, "Records the whole monitor. Never touches the game, so it's safe with any anti-cheat. Popups and notifications on that monitor show up in clips.");
            }
            self.hdr_note(ui, &mon);
            if self.cfg.follows_mouse() {
                note(ui, "Follow the mouse: records the monitor your mouse is on, switching about 2 seconds after it moves over. Clips keep one size; a monitor with another resolution is scaled to fit.");
            }
        });
        card(ui, "Encoding", |ui| {
            form(ui, "encoding", |ui| {
                ui.label("Quality");
                ui.horizontal_wrapped(|ui| {
                    for (i, q) in ["Highest", "High", "Medium", "Low"].into_iter().enumerate() {
                        chip(ui, &mut self.cfg.quality, i as u8, q);
                    }
                });
                ui.end_row();

                ui.label("Encoder");
                ui.horizontal_wrapped(|ui| {
                    for (v, name) in [("h264", "H.264"), ("hevc", "HEVC"), ("av1", "AV1")] {
                        chip(ui, &mut self.cfg.codec, v.to_string(), name);
                    }
                });
                ui.end_row();

                ui.label("Encoder effort");
                ui.horizontal_wrapped(|ui| {
                    for (i, name) in ["Best quality", "Balanced", "Max FPS"].into_iter().enumerate() {
                        chip(ui, &mut self.cfg.encoder_effort, i as u8, name);
                    }
                });
                ui.end_row();
            });
            note(ui, "HEVC and AV1 make smaller files; H.264 plays everywhere. AV1 needs a recent GPU (RTX 40, RX 7000, Intel Arc or newer).");
            let effort = [
                "Best quality: the encoder takes its time on every frame for the cleanest clips.",
                "Balanced: less work per frame and no extra GPU passes. A little more FPS when your GPU is maxed out; clips look slightly softer in fast motion.",
                "Max FPS: the lightest encoding. The most FPS headroom; fast motion looks blockier at the same bitrate (raise Quality to make up for it).",
            ][self.cfg.encoder_effort.min(2) as usize];
            note(ui, effort);
            self.encoder_hint(ui, "Encoder effort", ["Best quality", "Balanced", "Max FPS"]);
            if recorder::gpu() == recorder::Gpu::None {
                ui.label(RichText::new("No NVIDIA, AMD or Intel video encoder found, so clips are H.264 encoded on the CPU. That can cost some FPS in games.").small().color(AMBER));
            }
        });
        let p = recorder::plan(&self.cfg, &mon);
        card(ui, "Result", |ui| {
            form(ui, "result", |ui| {
                for (k, v) in [
                    ("Clip", format!("{}x{} at {} fps", p.out_w, p.out_h, p.fps)),
                    ("Encoder", recorder::gpu().label().to_string()),
                    ("Bitrate", format!("about {} Mbps", p.kbps / 1000)),
                    ("Memory", format!("up to {:.1} GB of RAM", p.mb as f64 / 1024.0)),
                    ("Disk", recorder::space_estimate(&self.cfg, p.kbps)),
                ] {
                    ui.label(RichText::new(k).color(MUTED));
                    ui.label(RichText::new(v).strong());
                    ui.end_row();
                }
            });
        });
    }

    fn wizard_header(&mut self, ui: &mut egui::Ui, step: usize) {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 10.0, RAISED);
            ui.painter().circle_filled(rect.center(), 6.0, accent_light());
            ui.label(strong("Replay Vault", 17.0).color(Color32::WHITE));
        });
        ui.add_space(18.0);
        ui.label(strong(STEPS[step], 24.0).color(Color32::WHITE));
        ui.label(RichText::new(format!("Setup  ·  step {} of {}", step + 1, STEPS.len())).color(MUTED));
        ui.add_space(12.0);
        // Progress dots.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for i in 0..STEPS.len() {
                let w = (ui.available_width() - 6.0 * (STEPS.len() - 1 - i) as f32) / (STEPS.len() - i) as f32;
                let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 4.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 2.0, if i <= step { accent() } else { Color32::from_rgb(40, 36, 52) });
            }
        });
    }

    fn wizard_body(&mut self, ui: &mut egui::Ui, step: usize) {
        match step {
            0 => card(ui, "Welcome", |ui| {
                ui.label(RichText::new("Save the last few minutes of your game with one key.").size(16.0).strong());
                ui.add_space(4.0);
                ui.label("Replay Vault keeps a rolling recording in memory, like ShadowPlay. Nothing is written to disk until you press the save hotkey.");
                ui.add_space(4.0);
                ui.label(RichText::new("It records with OBS Studio, running hidden in the background. The next steps get OBS ready and pick where clips go, your hotkeys, quality and audio.").color(MUTED));
            }),
            1 => self.obs_step(ui),
            2 => {
                self.general_tab(ui);
                self.keybinds_tab(ui);
            }
            3 => self.video_tab(ui),
            4 => self.audio_tab(ui),
            _ => card(ui, "All set", |ui| {
                ui.label(RichText::new("You're ready to go.").size(16.0).strong());
                ui.add_space(4.0);
                for line in [
                    format!("Press {} to save the last {}.", self.cfg.hotkey, mmss(self.cfg.buffer_secs)),
                    format!("Press {} to open or hide this window.", self.cfg.overlay_hotkey),
                    format!("Clips go to {}.", self.cfg.folder),
                    "Everything here can be changed later from this window.".to_string(),
                ] {
                    ui.label(format!("•  {line}"));
                }
            }),
        }
    }

    /// Checks for a usable OBS and offers to download a private copy (and the VC++ runtime).
    fn obs_step(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if let Some(job) = &self.job {
            let p = job.lock().unwrap().clone();
            match p.result {
                Some(r) => {
                    self.job = None;
                    self.job_error = r.err();
                    self.obs_exe = obs_install::find();
                    self.firewall_ok = None; // re-check after any job
                }
                None => {
                    card(ui, "Working", |ui| {
                        let frac = if p.total > 0 { p.done as f32 / p.total as f32 } else { 0.0 };
                        let detail = if p.stage.starts_with("Downloading OBS") {
                            format!("{} / {} MB", p.done / 1_048_576, p.total / 1_048_576)
                        } else if p.total > 0 {
                            format!("{} / {}", p.done, p.total)
                        } else {
                            String::new()
                        };
                        ui.label(&p.stage);
                        ui.add(egui::ProgressBar::new(frac).text(detail).animate(p.total == 0));
                    });
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    return;
                }
            }
        }

        let exe = self.obs_exe.clone();
        let (mut get_obs, mut get_vc) = (false, false);
        // Some(new folder) when the user picks where the private copy goes; Some("") = default.
        let mut new_dir: Option<String> = None;
        let custom_dir = !self.cfg.obs_dir.is_empty();
        card(ui, "OBS Studio", |ui| match &exe {
            Some(exe) => {
                ui.label(RichText::new("✔  OBS is ready").strong().color(accent_light()));
                let dir = exe.ancestors().nth(3).unwrap_or(exe);
                ui.add(egui::Label::new(RichText::new(dir.display().to_string()).small().color(MUTED)).truncate());
                if obs_install::is_private(exe) {
                    let v = obs_install::private_version().unwrap_or_default();
                    ui.label(format!("A private copy (OBS {v}) used only by this app. It's kept up to date automatically."));
                } else {
                    ui.label("Using your OBS install. The recorder runs its own hidden copy with separate settings, so your OBS scenes and settings are never changed.");
                }
            }
            None => {
                ui.label("OBS Studio (free and open source) does the recording. This downloads the official release from GitHub into a private folder used only by this app, checks it against its published checksum, and keeps it up to date. Any OBS you already have isn't touched.");
                ui.add_space(6.0);
                // Where it goes: any drive, so it needn't take space on the system drive.
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Install to").color(MUTED));
                    if ui.button("Browse").clicked() {
                        if let Some(dir) = rfd::FileDialog::new().set_title("Where should OBS go?").pick_folder() {
                            new_dir = Some(dir.join("Replay Vault OBS").display().to_string());
                        }
                    }
                    if custom_dir && ui.button("Default").on_hover_text("Back to the app's own folder on the system drive").clicked() {
                        new_dir = Some(String::new());
                    }
                });
                ui.add(egui::Label::new(RichText::new(obs_install::private_dir().display().to_string()).small()).truncate());
                ui.label(RichText::new("Needs about 800 MB there: the 225 MB download plus about 550 MB unpacked.").small().color(MUTED));
                ui.add_space(6.0);
                get_obs = primary(ui, true, RichText::new("Download OBS (about 225 MB)").strong(), egui::vec2(0.0, 32.0)).clicked();
            }
        });

        if !obs_install::has_vc_runtime() {
            card(ui, "Visual C++ runtime", |ui| {
                ui.label("OBS also needs Microsoft's Visual C++ runtime, which this PC doesn't have yet. Windows will ask for permission to install it.");
                ui.add_space(4.0);
                get_vc = ui.add(egui::Button::new(RichText::new("Install the runtime").strong()).min_size(egui::vec2(0.0, 30.0))).clicked();
            });
        }
        if let Some(dir) = new_dir {
            self.cfg.obs_dir = dir;
            obs_install::set_install_root(&self.cfg.obs_dir);
            log::write(&format!("Private OBS folder set to {}", obs_install::private_dir().display()));
        }
        // Only for our private copy: the user's own OBS is the same program as their main OBS,
        // and blocking it could break their remotes or plugins.
        let mut get_fw = false;
        if let Some(exe) = exe.as_ref().filter(|e| obs_install::is_private(e)) {
            let fw_ok = *self.firewall_ok.get_or_insert_with(obs_install::firewall_rule_exists);
            if !fw_ok {
                card(ui, "Firewall (optional)", |ui| {
                    ui.label("The hidden recorder only talks to this app, on this PC. Without a firewall rule, Windows may pop up asking whether OBS can use your network. This adds a rule that keeps it off the network, so that question never comes up. Windows will ask for permission once.");
                    ui.add_space(4.0);
                    get_fw = ui.add(egui::Button::new(RichText::new("Add firewall rule").strong()).min_size(egui::vec2(0.0, 30.0))).clicked();
                });
            } else {
                card(ui, "Firewall", |ui| {
                    ui.label(RichText::new("✔  The recorder is kept off your network").color(accent_light()));
                });
            }
            if get_fw {
                self.job_error = None;
                let c = ctx.clone();
                self.job = Some(obs_install::add_firewall_rule(exe.clone(), move || c.request_repaint()));
            }
        }
        if get_obs || get_vc {
            self.job_error = None;
            let c = ctx.clone();
            let repaint = move || c.request_repaint();
            self.job = Some(if get_obs { obs_install::install(repaint) } else { obs_install::install_vc_runtime(repaint) });
        }
        if let Some(e) = &self.job_error {
            ui.colored_label(RED, format!("That didn't work: {e}. You can try again."));
        }
    }

    /// Whether the wizard can move past `step`.
    fn step_ready(&self, step: usize) -> bool {
        match step {
            1 => self.job.is_none() && self.obs_exe.is_some() && obs_install::has_vc_runtime(),
            2 => self.capturing.is_none(),
            _ => true,
        }
    }

    fn wizard_footer(&mut self, ui: &mut egui::Ui, step: usize) {
        ui.horizontal(|ui| {
            if step > 0 && ui.add(egui::Button::new("Back").min_size(egui::vec2(90.0, 34.0))).clicked() {
                self.wizard = Some(step - 1);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let last = step + 1 == STEPS.len();
                let label = if last { "Start recording" } else { "Next" };
                let ready = self.step_ready(step);
                if primary(ui, ready, RichText::new(label).strong(), egui::vec2(130.0, 34.0)).clicked() {
                    if last {
                        self.cfg.setup_done = true;
                        self.wizard = None;
                        self.keep_settings();
                        self.apply();
                    } else {
                        self.wizard = Some(step + 1);
                    }
                }
            });
        });
    }

    /// The bar that appears while settings are changed but not saved.
    fn footer(&mut self, ui: &mut egui::Ui) {
        let restart = recorder_settings(&self.cfg) != self.applied;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(strong("Unsaved changes", 14.0).color(Color32::WHITE));
                let note = if restart {
                    "Saving restarts the recorder, so the current replay buffer is cleared."
                } else {
                    "Save keeps them for next time. Discard goes back to what was saved."
                };
                ui.label(RichText::new(note).small().color(MUTED));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if primary(ui, true, strong("Save", 14.0), egui::vec2(110.0, 34.0)).clicked() {
                    self.save_settings();
                }
                if ui.add(egui::Button::new("Discard").min_size(egui::vec2(90.0, 34.0))).clicked() {
                    self.discard_settings();
                }
            });
        });
    }
}

/// Tray icon: left click toggles the overlay, right click opens the menu. The handlers run
/// even while the window is hidden, so they act directly instead of waiting for a frame.
fn make_tray(ctx: &egui::Context, cmds: Sender<Cmd>) -> anyhow::Result<TrayIcon> {
    let open = MenuItem::new("Open overlay", true, None);
    let save = MenuItem::new("Save clip", true, None);
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    menu.append(&open)?;
    menu.append(&save)?;
    menu.append(&quit)?;
    let (open_id, save_id, quit_id) = (open.id().clone(), save.id().clone(), quit.id().clone());

    let c = ctx.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        if e.id == open_id {
            overlay::show();
            c.request_repaint();
        } else if e.id == save_id {
            let _ = cmds.send(Cmd::Save);
        } else if e.id == quit_id {
            obs::end_session("app closed");
            log::write("App closed.");
            recorder::stop();
            std::process::exit(0);
        }
    }));
    let c = ctx.clone();
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
            overlay::toggle();
            c.request_repaint();
        }
    }));

    Ok(TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("Replay Vault")
        .with_icon(tray_icon_image(TrayLook::Off)?)
        .build()?)
}

/// What the tray icon shows: replay off (or no recorder), replay on, or a full recording running.
#[derive(Clone, Copy, PartialEq, Debug)]
enum TrayLook {
    Off,
    Buffer,
    Recording,
}

impl TrayLook {
    fn of(connected: bool, buffer: bool, recording: bool) -> TrayLook {
        match (connected, buffer, recording) {
            (true, _, true) => TrayLook::Recording,
            (true, true, false) => TrayLook::Buffer,
            _ => TrayLook::Off,
        }
    }

    fn tooltip(self) -> &'static str {
        match self {
            TrayLook::Off => "Replay Vault: instant replay off",
            TrayLook::Buffer => "Replay Vault: instant replay on",
            TrayLook::Recording => "Replay Vault: recording",
        }
    }
}

const TRAY_N: u32 = 32;

fn tray_icon_image(look: TrayLook) -> anyhow::Result<tray_icon::Icon> {
    Ok(tray_icon::Icon::from_rgba(tray_rgba(look), TRAY_N, TRAY_N)?)
}

/// A record dot with a dark ring, drawn in code: the accent colour while replay is on, gray when
/// off, red while recording.
fn tray_rgba(look: TrayLook) -> Vec<u8> {
    const N: u32 = TRAY_N;
    let on = accent_light();
    let dot = match look {
        TrayLook::Off => [120, 120, 128, 255],
        TrayLook::Buffer => [on.r(), on.g(), on.b(), 255],
        TrayLook::Recording => [235, 60, 60, 255],
    };
    let mut rgba = Vec::with_capacity((N * N * 4) as usize);
    for y in 0..N {
        for x in 0..N {
            let d = ((x as f32 - 15.5).powi(2) + (y as f32 - 15.5).powi(2)).sqrt();
            let px = if d < 9.5 {
                dot
            } else if d < 14.5 {
                [30, 30, 34, 255]
            } else if d < 15.5 {
                [30, 30, 34, ((15.5 - d) * 255.0) as u8]
            } else {
                [0, 0, 0, 0]
            };
            rgba.extend_from_slice(&px);
        }
    }
    rgba
}

/// The settings that need a recorder restart (the overlay hotkey and popup apply instantly).
/// A limit that's off (0) until ticked, then starts at `start`.
fn limit(ui: &mut egui::Ui, value: &mut u32, text: &str, unit: &str, start: u32, max: u32) {
    ui.horizontal(|ui| {
        let mut on = *value > 0;
        if ui.checkbox(&mut on, text).changed() {
            *value = if on { start } else { 0 };
        }
        if on {
            ui.add(egui::DragValue::new(value).range(1..=max).suffix(format!(" {unit}")));
        }
    });
}

fn cleanup_rule(cfg: &Config) -> clips::Cleanup {
    clips::Cleanup {
        max_bytes: (cfg.keep_under_gb > 0).then_some((cfg.keep_under_gb as u64) << 30),
        max_age: (cfg.keep_days > 0).then(|| std::time::Duration::from_secs(cfg.keep_days as u64 * 86_400)),
    }
}

/// The archive rule, if it's on and its folder is set and outside the clips folder.
fn archive_rule(cfg: &Config) -> Option<clips::Archive> {
    let dir = cfg.archive_folder.trim();
    let ok = cfg.archive_days > 0 && !dir.is_empty() && !clips::inside(dir, &cfg.folder);
    ok.then(|| clips::Archive { dir: dir.into(), after: std::time::Duration::from_secs(cfg.archive_days as u64 * 86_400) })
}

/// The folders backed-up clips can be in (the clips folder, the archive) and the backup folder, if
/// the backup is on and its folder is set and outside the clips folder.
fn backup_rule(cfg: &Config) -> Option<(Vec<std::path::PathBuf>, std::path::PathBuf)> {
    let dir = cfg.backup_folder.trim();
    let ok = cfg.backup && !dir.is_empty() && !clips::inside(dir, &cfg.folder);
    let roots = std::iter::once(cfg.folder.clone().into()).chain(archive_rule(cfg).map(|a| a.dir)).collect();
    ok.then(|| (roots, dir.into()))
}

/// %OneDrive%\Replay Vault, if OneDrive is set up.
fn onedrive_folder() -> Option<String> {
    std::env::var("OneDrive").ok().filter(|d| !d.is_empty()).map(|d| std::path::Path::new(&d).join("Replay Vault").display().to_string())
}

/// A folder box with a Browse button.
fn folder_field(ui: &mut egui::Ui, value: &mut String, hint: &str) {
    ui.horizontal(|ui| {
        if ui.button("Browse").clicked() {
            if let Some(dir) = rfd::FileDialog::new().set_directory(&*value).pick_folder() {
                *value = dir.display().to_string();
            }
        }
        ui.add(egui::TextEdit::singleline(value).hint_text(hint).desired_width(f32::INFINITY));
    });
}

/// The monitor to record, or "Follow the mouse" (Desktop mode, more than one monitor).
fn monitor_combo(ui: &mut egui::Ui, cfg: &mut Config, monitors: &[display::Monitor], salt: &str, mon: &display::Monitor) {
    let follow = cfg.follows_mouse();
    let text = if follow { "Follow the mouse".to_string() } else { mon.label() };
    egui::ComboBox::from_id_salt(salt).selected_text(text).width(ui.available_width()).show_ui(ui, |ui| {
        for m in monitors {
            if ui.selectable_label(!follow && m.id == mon.id, m.label()).clicked() {
                (cfg.monitor_id, cfg.follow_mouse) = (m.id.clone(), false);
            }
        }
        if monitors.len() > 1 && cfg.capture != "game" && ui.selectable_label(follow, "Follow the mouse").clicked() {
            cfg.follow_mouse = true;
        }
    });
}

/// "Discord (#general - Discord)": the program, then the window title it had when picked.
fn app_label(a: &config::AppSel) -> String {
    let name = a.exe.strip_suffix(".exe").or(a.exe.strip_suffix(".EXE")).unwrap_or(&a.exe);
    format!("{name}  ({})", a.title.trim())
}

fn recorder_settings(cfg: &Config) -> String {
    let mut c = cfg.clone();
    c.overlay_hotkey.clear();
    c.hotkey_30.clear();
    c.hotkey_60.clear();
    c.hotkey_record.clear();
    c.hotkey_screenshot.clear();
    c.hotkey_replay.clear();
    c.hotkey_mic.clear();
    c.save_popup = false;
    c.sort_by_game = false;
    c.save_sound = false;
    c.game_bar_off = false;
    c.track_names = Default::default();
    c.advanced = false;
    c.keep_under_gb = 0;
    c.keep_days = 0;
    c.archive_days = 0;
    c.archive_folder.clear();
    c.backup = false;
    c.backup_folder.clear();
    c.discord_webhook.clear();
    c.phone_page = false;
    c.phone_key.clear();
    c.tips_seen.clear();
    c.accent = 0;
    serde_json::to_string(&c).unwrap_or_default()
}

/// Charcoal and the accent color, flat, rounded, in Windows' own Segoe UI.
fn apply_theme(ctx: &egui::Context) {
    use_windows_fonts(ctx);
    set_visuals(ctx);
}

/// The colours and spacing, in the current accent.
fn set_visuals(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.window_stroke = egui::Stroke::new(1.0, BORDER);
    v.extreme_bg_color = SIDEBAR;
    v.faint_bg_color = CARD;
    v.selection.bg_fill = accent();
    v.selection.stroke = egui::Stroke::new(1.0, Color32::WHITE);
    v.hyperlink_color = accent_light();
    v.slider_trailing_fill = true;
    let radius = egui::CornerRadius::same(6);
    let w = &mut v.widgets;
    w.noninteractive.bg_stroke = egui::Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(218, 218, 226));
    for (state, fill) in [
        (&mut w.inactive, RAISED),
        (&mut w.hovered, Color32::from_rgb(50, 48, 62)),
        (&mut w.active, accent_dim()),
        (&mut w.open, Color32::from_rgb(46, 44, 56)),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.corner_radius = radius;
        state.bg_stroke = egui::Stroke::NONE;
    }
    w.hovered.bg_stroke = egui::Stroke::new(1.0, accent());
    w.inactive.fg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(230, 230, 238));
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.style_mut_of(egui::Theme::Dark, |s| {
        s.spacing.item_spacing = egui::vec2(8.0, 8.0);
        s.spacing.button_padding = egui::vec2(12.0, 6.0);
        s.spacing.interact_size.y = 28.0;
        s.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
        s.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
        s.text_styles.insert(egui::TextStyle::Small, egui::FontId::proportional(12.0));
        s.text_styles.insert(egui::TextStyle::Heading, egui::FontId::new(24.0, egui::FontFamily::Name("strong".into())));
    });
}

/// Segoe UI (and its semibold, as the "strong" family) from the Windows fonts folder, with
/// Segoe UI Symbol behind both for marks like ✔ and ⚠. Read at startup, never bundled; if a
/// file is missing egui's built-in font stays.
fn use_windows_fonts(ctx: &egui::Context) {
    let dir = std::path::Path::new(&std::env::var("WINDIR").unwrap_or(r"C:\Windows".into())).join("Fonts");
    let load = |name: &str| std::fs::read(dir.join(name)).ok();
    let (Some(regular), Some(semibold)) = (load("segoeui.ttf"), load("seguisb.ttf")) else { return };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("segoe".into(), Arc::new(egui::FontData::from_owned(regular)));
    fonts.font_data.insert("segoe-semibold".into(), Arc::new(egui::FontData::from_owned(semibold)));
    let mut fallback = vec![];
    if let Some(symbols) = load("seguisym.ttf") {
        fonts.font_data.insert("segoe-symbol".into(), Arc::new(egui::FontData::from_owned(symbols)));
        fallback.push("segoe-symbol".to_string());
    }
    let proportional = fonts.families.entry(egui::FontFamily::Proportional).or_default();
    for (i, f) in ["segoe".to_string()].into_iter().chain(fallback.clone()).enumerate() {
        proportional.insert(i, f);
    }
    let strong = ["segoe-semibold".to_string()].into_iter().chain(fallback).chain(proportional.clone()).collect();
    fonts.families.insert(egui::FontFamily::Name("strong".into()), strong);
    // Windows' icon font, for the sidebar and buttons.
    if let Some(icons) = load("segmdl2.ttf") {
        fonts.font_data.insert("icons".into(), Arc::new(egui::FontData::from_owned(icons)));
        fonts.families.insert(egui::FontFamily::Name("icons".into()), vec!["icons".into()]);
        ICONS.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    ctx.set_fonts(fonts);
}

/// Segoe MDL2 Assets was found and loaded.
static ICONS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// "Show tips and explanations" (General), for `note`.
static NOTES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// The small grey explanation under a setting; left out when tips and explanations are off.
fn note(ui: &mut egui::Ui, text: impl Into<String>) {
    if NOTES.load(std::sync::atomic::Ordering::Relaxed) {
        ui.label(RichText::new(text.into()).small().color(MUTED));
    }
}

/// Glyphs in Segoe MDL2 Assets.
mod icon {
    pub const CLIPS: char = '\u{E8B2}'; // MovieClapper
    pub const SETTINGS: char = '\u{E713}';
    pub const KEYBOARD: char = '\u{E765}';
    pub const VIDEO: char = '\u{E714}';
    pub const AUDIO: char = '\u{E767}';
    pub const LOG: char = '\u{E9F9}'; // ReportDocument
    pub const PLAY: char = '\u{E768}';
    pub const PAUSE: char = '\u{E769}';
    pub const BACK: char = '\u{E72B}';
    pub const FOLDER: char = '\u{E838}';
    pub const CUT: char = '\u{E8C6}';
    pub const SHARE: char = '\u{E72D}';
    pub const MORE: char = '\u{E712}';
    pub const PICTURE: char = '\u{E91B}';
    pub const COPY: char = '\u{E8C8}';
    pub const STAR: char = '\u{E734}'; // FavoriteStar
    pub const STAR_FILLED: char = '\u{E735}'; // FavoriteStarFill
    pub const ADD: char = '\u{E710}';
    pub const CANCEL: char = '\u{E711}';
    pub const UP: char = '\u{E74A}';
    pub const DOWN: char = '\u{E74B}';
    pub const MUSIC: char = '\u{E8D6}'; // MusicNote
    pub const TIP: char = '\u{E82F}'; // Lightbulb
    pub const LEFT: char = '\u{E76B}'; // ChevronLeft
    pub const RIGHT: char = '\u{E76C}'; // ChevronRight
}

/// An icon glyph at `size`, or nothing if the icon font isn't there.
fn icon(glyph: char, size: f32) -> RichText {
    if ICONS.load(std::sync::atomic::Ordering::Relaxed) {
        RichText::new(glyph.to_string()).family(egui::FontFamily::Name("icons".into())).size(size)
    } else {
        RichText::new("")
    }
}

/// A small icon with no button frame, like the x on a tip; true when clicked.
fn bare_icon(ui: &mut egui::Ui, enabled: bool, glyph: char, hint: &str) -> bool {
    ui.add_enabled(enabled, egui::Button::new(icon(glyph, 12.0)).frame(false)).on_hover_text(hint).clicked()
}

/// A button with an icon before its text.
fn icon_button(glyph: char, text: &str) -> egui::Button<'static> {
    let mut job = egui::text::LayoutJob::default();
    if ICONS.load(std::sync::atomic::Ordering::Relaxed) {
        let fmt = egui::TextFormat { font_id: egui::FontId::new(13.0, egui::FontFamily::Name("icons".into())), valign: egui::Align::Center, color: Color32::from_rgb(230, 230, 238), ..Default::default() };
        job.append(&glyph.to_string(), 0.0, fmt);
    }
    let fmt = egui::TextFormat { font_id: egui::FontId::proportional(14.0), valign: egui::Align::Center, color: Color32::from_rgb(230, 230, 238), ..Default::default() };
    job.append(text, if job.is_empty() { 0.0 } else { 8.0 }, fmt);
    egui::Button::new(job)
}

/// Text in the semibold face, for titles and names.
fn strong(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).family(egui::FontFamily::Name("strong".into())).size(size)
}

/// Opens a text file in Notepad, in front of everything.
fn open_in_notepad(path: &std::path::Path) {
    // Notepad starts through a broker process, so let it take focus, and get the
    // always-on-top overlay out of its way.
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(u32::MAX);
    }
    let _ = std::process::Command::new(recorder::windows_exe("notepad.exe")).arg(path).spawn();
    overlay::hide();
}

/// A page's big title and a line about it.
fn page_title(ui: &mut egui::Ui, title: &str, about: &str) {
    ui.label(strong(title, 24.0).color(Color32::WHITE));
    if !about.is_empty() {
        ui.label(RichText::new(about).color(MUTED));
    }
    ui.add_space(14.0);
}

/// Paints `tex` filling `rect`, cut to its shape from the middle rather than stretched.
fn paint_fill(ui: &egui::Ui, tex: &egui::TextureHandle, rect: egui::Rect, radius: impl Into<egui::CornerRadius>) {
    let [w, h] = tex.size().map(|v| v as f32);
    let k = rect.aspect_ratio() / (w / h);
    let uv = if k < 1.0 {
        egui::Rect::from_x_y_ranges((1.0 - k) / 2.0..=(1.0 + k) / 2.0, 0.0..=1.0)
    } else {
        egui::Rect::from_x_y_ranges(0.0..=1.0, (1.0 - 1.0 / k) / 2.0..=(1.0 + 1.0 / k) / 2.0)
    };
    egui::Image::new(tex).uv(uv).corner_radius(radius).paint_at(ui, rect);
}

/// A soft round light: `color` in the middle, fading to nothing at `radius`.
fn glow(painter: &egui::Painter, center: egui::Pos2, radius: f32, color: Color32) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(center, color);
    const N: u32 = 64;
    for i in 0..N {
        let a = i as f32 / N as f32 * std::f32::consts::TAU;
        mesh.colored_vertex(center + radius * egui::vec2(a.cos(), a.sin()), Color32::TRANSPARENT);
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % N);
    }
    painter.add(mesh);
}

/// The main action on a page: purple with a faint sheen on top and a glow under it.
fn primary(ui: &mut egui::Ui, enabled: bool, text: RichText, min: egui::Vec2) -> egui::Response {
    // Painted under the button once its size is known.
    let under = ui.painter().add(egui::Shape::Noop);
    let button = egui::Button::new(text.color(Color32::WHITE)).fill(Color32::TRANSPARENT).min_size(min);
    let r = ui.add_enabled(enabled, button);
    let radius = egui::CornerRadius::same(6);
    let mut shapes = Vec::new();
    if enabled {
        let glow = egui::Shadow { offset: [0, 4], blur: if r.hovered() { 22 } else { 14 }, spread: 0, color: alpha(accent(), 70) };
        shapes.push(glow.as_shape(r.rect, radius).into());
    }
    let fill = match (enabled, r.hovered()) {
        (false, _) => accent_dim(),
        (true, true) => accent().lerp_to_gamma(Color32::WHITE, 0.1),
        (true, false) => accent(),
    };
    shapes.push(egui::Shape::rect_filled(r.rect, radius, fill));
    let top = egui::Rect::from_min_max(r.rect.min, egui::pos2(r.rect.max.x, r.rect.center().y));
    shapes.push(egui::Shape::rect_filled(top, egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, Color32::from_white_alpha(14)));
    ui.painter().set(under, egui::Shape::Vec(shapes));
    r
}

/// A titled card on the page.
fn card(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new().fill(CARD).corner_radius(10).inner_margin(16).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(strong(title, 15.0).color(Color32::WHITE));
        ui.add_space(4.0);
        body(ui);
    });
    ui.add_space(12.0);
}

/// A centered column at most `max` wide.
fn column<R>(ui: &mut egui::Ui, max: f32, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let w = ui.available_width().min(max);
    ui.horizontal(|ui| {
        ui.add_space((ui.available_width() - w) / 2.0);
        ui.vertical(|ui| {
            ui.set_width(w);
            body(ui)
        })
        .inner
    })
    .inner
}

/// Two-column label / control grid.
fn form(ui: &mut egui::Ui, id: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Grid::new(id).num_columns(2).spacing([14.0, 10.0]).min_col_width(110.0).show(ui, body);
}

/// A pill button that selects `value`; filled purple when selected.
fn chip<T: PartialEq>(ui: &mut egui::Ui, current: &mut T, value: T, text: impl Into<String>) {
    let selected = *current == value;
    let (fill, color) = if selected { (accent(), Color32::WHITE) } else { (RAISED, Color32::from_rgb(206, 206, 216)) };
    let button = egui::Button::new(RichText::new(text.into()).color(color)).fill(fill).corner_radius(14).min_size(egui::vec2(0.0, 28.0));
    if ui.add(button).clicked() {
        *current = value;
    }
}

/// Trim controls under the video: the timeline (drag the start or end edge you're over, or drag
/// anywhere else to scrub), a preview of just the selection, and the cut. Returns false when cancelled.
fn trim_controls(ui: &mut egui::Ui, p: &mut player::Player, t: &mut TrimEdit, split_on: bool, cfg: &Config) -> bool {
    let len = t.len.max(0.1);
    // Playback stays inside the selection: it stops on the end edge (start_playing begins it on
    // the start edge). Checked every frame while playing, so it overshoots by a frame at most.
    let playing = p.is_playing() && !p.ended();
    if playing {
        if p.position() >= t.end {
            p.pause();
            p.seek(t.end);
        } else {
            ui.ctx().request_repaint();
        }
    }
    // Borrowed, not copied: the worker writes it once and is done.
    let wave = t.wave.lock().unwrap();
    if wave.is_none() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    }
    if let (None, Some(w)) = (&t.loud, wave.as_deref()) {
        t.loud = Some(media::loud_moments(w));
    }
    let loud = t.loud.as_deref().unwrap_or(&[]);
    let mut scrub = range_bar(ui, len, &mut t.view, &mut t.start, &mut t.end, p.position(), playing, wave.as_deref().unwrap_or(&[]), loud, &t.parts);
    drop(wave);
    // M jumps to the next loud moment, around to the first after the last.
    if !ui.ctx().egui_wants_keyboard_input() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::M)) {
        let at = p.position() + 0.1;
        let later = loud.iter().copied().filter(|&m| m > at).min_by(f64::total_cmp);
        let jump = later.or_else(|| loud.iter().copied().min_by(f64::total_cmp));
        if let Some(m) = jump.filter(|m| !(t.view.0..t.view.0 + t.view.1).contains(m)) {
            t.view.0 = (m - t.view.1 / 2.0).clamp(0.0, len - t.view.1);
        }
        scrub = jump.or(scrub);
    }
    if let Some(at) = scrub {
        p.pause();
        p.seek(at);
    }
    let busy = t.job.is_some() || t.gif.is_some();
    let mut keep = true;
    ui.horizontal_wrapped(|ui| {
        if ui.button("Play selection").clicked() {
            p.seek(t.start);
            p.play();
        }
        note(ui, "Drag the purple edges to set the start and end. Ctrl+scroll on the timeline zooms; Shift+scroll moves along.");
        if t.loud.as_ref().is_some_and(|l| !l.is_empty()) {
            note(ui, "Yellow marks are loud moments: click one, or press M for the next.");
        }
    });
    parts_row(ui, t, busy);
    // A volume per audio track, heard live. Only clips recorded with separate tracks have more than one.
    let untouched = t.gains == media::track1(t.gains.len());
    if !t.gains.is_empty() {
        let one = t.gains.len() == 1;
        let mut changed = false;
        for (i, g) in t.gains.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                let name = if one {
                    "Volume".to_string()
                } else {
                    track_name(cfg, i)
                };
                ui.add_sized([130.0, 18.0], egui::Label::new(name));
                let mut pct = *g * 100.0;
                if ui.add(egui::Slider::new(&mut pct, 0.0..=200.0).suffix("%").integer()).changed() {
                    *g = pct / 100.0;
                    changed = true;
                }
            });
        }
        if changed {
            p.set_track_gains(&t.gains);
        }
        if !one {
            let first = track_name(cfg, 0);
            note(ui, format!("{first} (track 1) is the full mix, so to change one sound, turn {first} down and use the others. Rename tracks in the Audio tab."));
        } else if split_on {
            note(ui, "This clip was recorded with the game and your mic on one track, so they go up and down together. Clips saved from now on have them apart, with a slider each.");
        } else {
            note(ui, "This clip has the game and your mic on one track, so they go up and down together. A clip can't be split afterwards, but new ones can be recorded with them apart.");
            if ui.button("Record game and mic apart").clicked() {
                t.want_split = true;
            }
        }
    }
    let how = if !t.parts.is_empty() {
        "Save joins the parts in order into one clip, each starting right where you set it (it's re-encoded)."
    } else if untouched {
        "Copied without re-encoding, so it may start up to 2 s early (at a keyframe)."
    } else {
        "The video is copied as is and may start up to 2 s early (at a keyframe); the sound is remixed at these levels into one track."
    };
    ui.label(
        RichText::new(format!(
            "{} to {}  ·  {} long. {how} The original is kept.",
            secs_text(t.start),
            secs_text(t.end),
            secs_text(t.end - t.start)
        ))
        .small()
        .color(MUTED),
    );
    ui.horizontal(|ui| {
        let text = match &t.job {
            Some(j) if !t.parts.is_empty() => format!("Saving {}%", j.pct.load(std::sync::atomic::Ordering::Relaxed)),
            Some(_) => "Saving...".into(),
            None if t.parts.is_empty() => "Save trimmed copy".into(),
            None => format!("Save {} parts as one clip", t.parts.len()),
        };
        if primary(ui, !busy, RichText::new(text).strong(), egui::Vec2::ZERO).clicked() {
            let (src, start, end) = (t.path.clone(), t.start, t.end);
            let gains = (!untouched).then(|| t.gains.clone());
            let parts: Vec<media::Part> = t.parts.iter().map(|&(start, end)| media::Part { path: t.path.clone(), start, end, gains: t.gains.clone() }).collect();
            let what = format!("Trimmed {} to", src.display());
            t.job = Some(ExportJob::spawn(clips::copy_name(&t.path, "trim"), what, ui.ctx(), move |job, out| match &gains {
                _ if !parts.is_empty() => media::join(&parts, out, false, None, &|f| job.progress(f), &|| false),
                Some(g) => media::trim_mixed(&src, out, start, end, g),
                None => media::trim(&src, out, start, end),
            }));
        }
        gif_button(ui, t, busy);
        if ui.add_enabled(!busy, egui::Button::new("Cancel trim")).clicked() {
            keep = false;
            // Back to how the clip normally plays.
            p.set_track_gains(&media::track1(t.gains.len()));
        }
    });
    if let Some(msg) = &t.gif_msg {
        ui.label(RichText::new(msg).color(accent_light()));
    }
    if busy {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    }
    keep
}

/// "Save as GIF" for the selection, only when it's short (media::GIF_MAX_SECS); made in the
/// background into t.gif.
fn gif_button(ui: &mut egui::Ui, t: &mut TrimEdit, busy: bool) {
    let short = t.end - t.start <= media::GIF_MAX_SECS;
    let gif_text = match &t.gif {
        Some(g) => format!("Making GIF {}%", g.pct.load(std::sync::atomic::Ordering::Relaxed)),
        None => "Save as GIF".into(),
    };
    let gif = ui
        .add_enabled(!busy && short, egui::Button::new(gif_text))
        .on_hover_text("A looping GIF of the selection, 480 px wide, no sound")
        .on_disabled_hover_text(if short { "Wait for the save to finish." } else { "GIFs are up to 15 s. Make the selection shorter." });
    if !short {
        ui.label(RichText::new("GIF: 15 s max").small().color(MUTED));
    }
    if gif.clicked() {
        let (src, start, end) = (t.path.clone(), t.start, t.end);
        let what = format!("Made a GIF of {} at", src.display());
        t.gif = Some(ExportJob::spawn(clips::copy_name(&t.path.with_extension("gif"), "GIF"), what, ui.ctx(), move |job, out| media::save_gif(&src, out, start, end, &|f| job.progress(f))));
        t.gif_msg = None;
    }
}

/// Parts shaded on the timeline (range_bar).
const PART_FILL: Color32 = Color32::from_rgba_premultiplied(40, 80, 72, 110);
const PART_EDGE: Color32 = Color32::from_rgb(110, 200, 178);

/// "Add part" keeps the selection as a part, to save several bits of the clip as one; then a
/// chip per part with its times and a button to take it off.
fn parts_row(ui: &mut egui::Ui, t: &mut TrimEdit, busy: bool) {
    ui.horizontal_wrapped(|ui| {
        let add = ui.add_enabled(!busy, icon_button(icon::ADD, "Add part")).on_hover_text("Keep the selection as a part, then pick another. The parts are saved in order as one clip.");
        if add.clicked() && !t.parts.contains(&(t.start, t.end)) {
            t.parts.push((t.start, t.end));
            t.parts.sort_by(|a, b| a.0.total_cmp(&b.0));
        }
        let mut remove = None;
        for (i, &(s, e)) in t.parts.iter().enumerate() {
            egui::Frame::new().fill(RAISED).corner_radius(8).inner_margin(egui::Margin::symmetric(8, 2)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Part {}: {} to {}", i + 1, secs_text(s), secs_text(e))).small().color(PART_EDGE));
                    if ui.add_enabled(!busy, egui::Button::new(icon(icon::CANCEL, 10.0).color(MUTED)).frame(false)).on_hover_text("Take this part off").clicked() {
                        remove = Some(i);
                    }
                });
            });
        }
        if let Some(i) = remove {
            t.parts.remove(i);
        }
    });
}

/// A timeline with the kept part shaded between two edges, plus the playhead. Point at the start
/// or end edge and drag to move it (it lights up while you're on it); drag anywhere else to scrub.
/// Returns the time to show: the edge being moved, or where you're scrubbing.
///
/// `view` is the part of the clip shown, (from, length) in seconds: Ctrl+scroll over the bar zooms
/// in and out around the pointer, Shift+scroll moves along, and dragging past either end of the
/// bar moves the view with you. With `follow` (playing), the view keeps the playhead in sight.
/// `wave` (media::peaks, may be empty) is drawn as the clip's sound along the bar, and `loud`
/// (seconds) as yellow marks along the top; scrubbing onto one lands exactly on it. `parts`
/// (start, end) are shaded green.
#[allow(clippy::too_many_arguments)]
fn range_bar(ui: &mut egui::Ui, len: f64, view: &mut (f64, f64), start: &mut f64, end: &mut f64, playhead: f64, follow: bool, wave: &[f32], loud: &[f64], parts: &[(f64, f64)]) -> Option<f64> {
    #[derive(Clone, Copy, PartialEq)]
    enum Grab {
        Scrub,
        Start,
        End,
    }
    const REACH: f32 = 10.0; // how close to an edge counts as on it
    // Taller with a waveform to show.
    let height = if wave.is_empty() { 36.0 } else { 64.0 };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click_and_drag());
    // Zoomed in down to one second across the bar.
    let fit = |v: &mut (f64, f64)| {
        v.1 = v.1.clamp(len.min(1.0), len);
        v.0 = v.0.clamp(0.0, len - v.1);
    };
    if let Some(hover) = resp.hover_pos() {
        let (zoom, pan) = ui.input(|i| (i.zoom_delta() as f64, i.smooth_scroll_delta.x as f64));
        if zoom != 1.0 {
            // Keep the moment under the pointer where it is.
            let at = view.0 + ((hover.x - rect.left()) / rect.width()) as f64 * view.1;
            let span = (view.1 / zoom).clamp(len.min(1.0), len);
            view.0 = at - (at - view.0) * span / view.1;
            view.1 = span;
        }
        if pan != 0.0 {
            view.0 -= pan / rect.width() as f64 * view.1;
        }
    }
    fit(view);
    let v = *view;
    let x_of = |t: f64| rect.left() + ((t - v.0) / v.1) as f32 * rect.width();
    let t_of = |x: f32| (v.0 + ((x - rect.left()) / rect.width()) as f64 * v.1).clamp(0.0, len);
    // The edge under the pointer, the nearer one if both are close.
    let under = |x: f32, s: f64, e: f64| {
        let (ds, de) = ((x - x_of(s)).abs(), (x - x_of(e)).abs());
        if ds.min(de) > REACH { Grab::Scrub } else if de < ds { Grab::End } else { Grab::Start }
    };
    // Which edge is held is picked on the press, where it started, before anything moves; and
    // forgotten on release. (Picking it at drag start used the last drag's edge for the first
    // few pixels, which pulled the start edge over to the end.)
    let id = resp.id.with("grab");
    let mut grab = ui.data(|d| d.get_temp::<Grab>(id));
    if !resp.is_pointer_button_down_on() {
        grab = None;
        ui.data_mut(|d| d.remove::<Grab>(id));
    } else if grab.is_none() {
        grab = ui.input(|i| i.pointer.press_origin()).map(|p| under(p.x, *start, *end));
        ui.data_mut(|d| d.insert_temp(id, grab.unwrap_or(Grab::Scrub)));
    }
    let mut show = None;
    if let (Some(pos), Some(g)) = (resp.interact_pointer_pos(), grab) {
        let at = t_of(pos.x);
        show = Some(match g {
            Grab::Start => {
                *start = at.min(*end - 0.5).max(0.0);
                *start
            }
            Grab::End => {
                *end = at.max(*start + 0.5).min(len);
                *end
            }
            Grab::Scrub => loud.iter().copied().find(|&m| (x_of(m) - pos.x).abs() <= 6.0).unwrap_or(at),
        });
    }
    // Keep what's being dragged, or the playhead while playing, in view.
    if let Some(t) = show.or(follow.then_some(playhead)) {
        if t < view.0 {
            view.0 = t;
        } else if t > view.0 + view.1 {
            view.0 = if show.is_some() { t - view.1 } else { t };
        }
        fit(view);
    }
    let v = *view;
    let x_of = |t: f64| rect.left() + ((t - v.0) / v.1) as f32 * rect.width();
    // What to light up: the edge being dragged, else the one being pointed at.
    let active = if resp.is_pointer_button_down_on() { grab } else { resp.hover_pos().map(|p| under(p.x, *start, *end)) };
    // Clipped to the bar: zoomed in, the selection and edges can be partly or wholly off it.
    let painter = ui.painter_at(rect);
    let track_h = if wave.is_empty() { 8.0 } else { height / 2.0 - 4.0 };
    let track = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.center().y - track_h), egui::pos2(rect.right(), rect.center().y + track_h));
    painter.rect_filled(track, 4.0, Color32::from_rgb(36, 33, 46));
    for &(s, e) in parts {
        let r = egui::Rect::from_min_max(egui::pos2(x_of(s), track.top()), egui::pos2(x_of(e), track.bottom()));
        painter.rect_filled(r, 2.0, PART_FILL);
        painter.rect_stroke(r, 2.0, egui::Stroke::new(1.0, PART_EDGE), egui::StrokeKind::Inside);
    }
    let sel =egui::Rect::from_min_max(egui::pos2(x_of(*start), track.top()), egui::pos2(x_of(*end), track.bottom()));
    painter.rect_filled(sel, 2.0, accent_dim());
    // The sound, a bar every 2 px mirrored around the middle, brighter inside the kept part.
    // Square root so quiet parts still show.
    let t_at = |x: f32| v.0 + ((x - rect.left()) / rect.width()) as f64 * v.1;
    let half = track_h - 2.0;
    let mut x = rect.left();
    while x < rect.right() && !wave.is_empty() {
        let (a, b) = (t_at(x), t_at(x + 2.0));
        let i0 = (a * media::PEAKS_PER_SEC as f64) as usize;
        let i1 = ((b * media::PEAKS_PER_SEC as f64).ceil() as usize).max(i0 + 1).min(wave.len());
        if let Some(steps) = wave.get(i0..i1) {
            let h = steps.iter().fold(0.0f32, |m, &s| m.max(s)).sqrt() * half;
            if h >= 0.5 {
                let kept = a >= *start && a <= *end;
                let color = if kept { Color32::from_rgba_unmultiplied(200, 170, 255, 210) } else { Color32::from_rgba_unmultiplied(150, 150, 165, 110) };
                painter.line_segment([egui::pos2(x + 1.0, rect.center().y - h), egui::pos2(x + 1.0, rect.center().y + h)], egui::Stroke::new(1.5, color));
            }
        }
        x += 2.0;
    }
    painter.rect_stroke(sel, 2.0, egui::Stroke::new(1.5, accent()), egui::StrokeKind::Inside);
    for x in loud.iter().map(|&m| x_of(m)) {
        let top = rect.top();
        painter.line_segment([egui::pos2(x, top + 6.0), egui::pos2(x, track.bottom())], egui::Stroke::new(1.0, AMBER.gamma_multiply(0.5)));
        painter.add(egui::Shape::convex_polygon(vec![egui::pos2(x - 5.0, top), egui::pos2(x + 5.0, top), egui::pos2(x, top + 7.0)], AMBER, egui::Stroke::NONE));
    }
    for (edge, x) in [(Grab::Start, x_of(*start)), (Grab::End, x_of(*end))] {
        let lit = active == Some(edge);
        let size = if lit { egui::vec2(9.0, height - 2.0) } else { egui::vec2(6.0, height - 6.0) };
        painter.rect_filled(egui::Rect::from_center_size(egui::pos2(x, rect.center().y), size), 3.0, if lit { Color32::WHITE } else { accent() });
    }
    let px = x_of(show.unwrap_or(playhead));
    painter.line_segment([egui::pos2(px, rect.top()), egui::pos2(px, rect.bottom())], egui::Stroke::new(2.0, Color32::WHITE));
    // Zoomed in: a thin strip along the bottom shows which part of the clip is in view.
    if v.1 < len {
        let y = rect.bottom() - 1.5;
        let at = |t: f64| rect.left() + (t / len) as f32 * rect.width();
        painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], egui::Stroke::new(3.0, Color32::from_rgb(36, 33, 46)));
        painter.line_segment([egui::pos2(at(v.0), y), egui::pos2(at(v.0 + v.1), y)], egui::Stroke::new(3.0, accent_light()));
    }
    let on_edge = matches!(active, Some(Grab::Start | Grab::End));
    resp.on_hover_cursor(if on_edge { egui::CursorIcon::ResizeHorizontal } else { egui::CursorIcon::PointingHand });
    show
}

/// Starts playback. While trimming (`range` = the selection), a playhead outside the selection
/// (or sitting on its end) goes back to the start edge first, so play never runs past the cut.
fn start_playing(p: &mut player::Player, range: Option<(f64, f64)>) {
    if let Some((start, end)) = range {
        let at = p.position();
        if at < start || at >= end - 0.05 {
            p.seek(start);
        }
    }
    p.play();
}

/// The audio setup as the Basic view shows it: one playback device, one mic, and whether the mic
/// also gets a track of its own.
#[derive(Clone, Debug, PartialEq)]
struct BasicAudio {
    out: Option<String>,
    mic: Option<String>,
    split: bool,
}

/// Track layouts Basic uses: everything on track 1; split adds game (2) and mic (3).
const BASIC_OUT: [[bool; 3]; 2] = [[true, false, false], [true, true, false]];
const BASIC_MIC: [[bool; 3]; 2] = [[true, false, false], [true, false, true]];

/// Devices recorded on any of the clip's tracks.
fn used_devices<'a>(cfg: &'a Config, prefix: &'a str) -> impl Iterator<Item = (&'a String, [bool; 3])> + 'a {
    let n = cfg.track_count.clamp(1, 3) as usize;
    cfg.devices.iter().filter(move |(k, _)| k.starts_with(prefix)).filter_map(move |(k, d)| {
        let mut t = [false; 3];
        t[..n].copy_from_slice(&d.tracks[..n]);
        t.iter().any(|&x| x).then_some((k, t))
    })
}

/// The setup in Basic terms, or None when Basic can't show it (custom).
fn basic_audio(cfg: &Config) -> Option<BasicAudio> {
    let outs: Vec<_> = used_devices(cfg, "out:").collect();
    let mics: Vec<_> = used_devices(cfg, "in:").collect();
    if outs.len() > 1 || mics.len() > 1 {
        return None;
    }
    let split = match cfg.track_count {
        1 => false,
        3 if !mics.is_empty() => true,
        _ => return None,
    };
    let s = split as usize;
    if outs.iter().any(|(_, t)| *t != BASIC_OUT[s]) || mics.iter().any(|(_, t)| *t != BASIC_MIC[s]) {
        return None;
    }
    Some(BasicAudio { out: outs.first().map(|(k, _)| (*k).clone()), mic: mics.first().map(|(k, _)| (*k).clone()), split })
}

/// A starting point for a custom setup: its first sound device and mic.
fn basic_audio_guess(cfg: &Config) -> BasicAudio {
    BasicAudio {
        out: used_devices(cfg, "out:").next().map(|(k, _)| k.clone()),
        mic: used_devices(cfg, "in:").next().map(|(k, _)| k.clone()),
        split: cfg.track_count == 3,
    }
}

/// Replaces the device and track setup with the Basic one.
fn set_basic_audio(cfg: &mut Config, b: &BasicAudio) {
    let split = b.split && b.mic.is_some();
    for d in cfg.devices.values_mut() {
        d.tracks = [false; 3];
    }
    if let Some(k) = &b.out {
        cfg.devices.entry(k.clone()).or_default().tracks = BASIC_OUT[split as usize];
    }
    if let Some(k) = &b.mic {
        cfg.devices.entry(k.clone()).or_default().tracks = BASIC_MIC[split as usize];
    }
    cfg.track_count = if split { 3 } else { 1 };
}

/// What a track is called: the user's name for it, else after the devices recorded on it.
fn track_name(cfg: &Config, i: usize) -> String {
    match cfg.track_names.get(i).map(|n| n.trim()) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => device_track_name(cfg, i),
    }
}

/// A track named after the Audio tab's devices on it ("Chat + Game"), "Everything" for a track
/// with every recorded device on it (when there's more than one), or "Track N" if it has none.
fn device_track_name(cfg: &Config, i: usize) -> String {
    let used: Vec<&config::DeviceSel> = cfg.devices.values().filter(|d| d.tracks.iter().take(cfg.track_count as usize).any(|&t| t)).collect();
    let on: Vec<&config::DeviceSel> = used.iter().copied().filter(|d| d.tracks.get(i) == Some(&true)).collect();
    if on.is_empty() {
        return format!("Track {}", i + 1);
    }
    if on.len() > 1 && on.len() == used.len() {
        return "Everything".into();
    }
    // "Chat (TC-Helicon GoXLR Mini)" reads as "Chat".
    let short = |d: &&config::DeviceSel| d.name.split(" (").next().unwrap_or(&d.name).trim().to_string();
    on.iter().map(short).collect::<Vec<_>>().join(" + ")
}

/// An on/off switch.
fn toggle(on: &mut bool) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| {
        let size = egui::vec2(40.0, 22.0);
        let (rect, mut response) = ui.allocate_exact_size(size, egui::Sense::click());
        if response.clicked() {
            *on = !*on;
            response.mark_changed();
        }
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let fill = if ui.is_enabled() { Color32::from_rgb(44, 40, 56).lerp_to_gamma(accent(), t) } else { Color32::from_rgb(36, 34, 42) };
        ui.painter().rect_filled(rect, 11.0, fill);
        let x = egui::lerp((rect.left() + 11.0)..=(rect.right() - 11.0), t);
        ui.painter().circle_filled(egui::pos2(x, rect.center().y), 8.0, Color32::WHITE);
        response
    }
}

fn target_name(t: Target) -> &'static str {
    match t {
        Target::Save => "Save clip",
        Target::Save30 => "Save last 0:30",
        Target::Save60 => "Save last 1:00",
        Target::Overlay => "Show / hide this window",
        Target::Record => "Start / stop recording",
        Target::Screenshot => "Screenshot",
        Target::Replay => "Instant replay on / off",
        Target::Mic => "Mute / unmute mic",
    }
}

/// Seconds as m:ss.s, for trim points.
fn secs_text(secs: f64) -> String {
    let s = secs.max(0.0);
    format!("{}:{:04.1}", (s / 60.0) as u32, s % 60.0)
}


/// Seconds as m:ss.
fn mmss(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Where an arrow key moves from card `at` (row, column) in a grid with rows `rows` long:
/// left and right go card by card, on into the next or last row; up and down keep the column,
/// or take the row's last card when it's shorter. Stays put at the edges.
fn grid_step(rows: &[usize], (r, c): (usize, usize), key: egui::Key) -> (usize, usize) {
    use egui::Key::*;
    match key {
        ArrowLeft if c > 0 => (r, c - 1),
        ArrowLeft if r > 0 => (r - 1, rows[r - 1] - 1),
        ArrowRight if c + 1 < rows[r] => (r, c + 1),
        ArrowRight if r + 1 < rows.len() => (r + 1, 0),
        ArrowUp if r > 0 => (r - 1, c.min(rows[r - 1] - 1)),
        ArrowDown if r + 1 < rows.len() => (r + 1, c.min(rows[r + 1] - 1)),
        _ => (r, c),
    }
}

impl eframe::App for App {
    /// Don't leave the hidden recorder running after the app closes.
    fn on_exit(&mut self) {
        obs::end_session("app closed");
        log::write("App closed.");
        recorder::stop();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // eframe shows the window itself once its first frame is painted; undo that when
        // started minimized. Hidden passes only run when something asks for one, so this stops
        // on its own.
        if self.minimized && self.started.elapsed() < std::time::Duration::from_secs(2) {
            overlay::hide();
            ui.ctx().request_repaint();
        }
        self.handle_events();
        self.finish_capture();
        // Setup has no Save button: what's picked there is kept as it goes.
        if self.wizard.is_some() && self.cfg != self.saved {
            self.keep_settings();
        }
        obs::SAVE_POPUP.store(self.saved.save_popup, std::sync::atomic::Ordering::Relaxed);
        obs::SORT_BY_GAME.store(self.saved.sort_by_game, std::sync::atomic::Ordering::Relaxed);
        obs::SAVE_SOUND.store(self.saved.save_sound, std::sync::atomic::Ordering::Relaxed);
        clips::set_cleanup(&self.saved.folder, cleanup_rule(&self.saved));
        clips::set_archive(&self.saved.folder, archive_rule(&self.saved));
        clips::set_backup(backup_rule(&self.saved));
        let archived = clips::take_archived();
        if !archived.is_empty() {
            for (old, new) in &archived {
                clips::moved(&mut self.favorites, &mut self.tags, old, Some(new));
            }
            // Saved even if nothing changed here, over anything the archive wrote from a stale read.
            clips::save_favorites(&self.favorites);
            clips::save_tags(&self.tags);
            self.links = upload::links();
            self.clips_checked -= std::time::Duration::from_secs(60);
        }
        self.sync_phone();
        NOTES.store(self.cfg.show_tips, std::sync::atomic::Ordering::Relaxed);
        if ACCENT.swap(self.cfg.accent, std::sync::atomic::Ordering::Relaxed) != self.cfg.accent {
            set_visuals(ui.ctx());
            *toast::BAR.lock().unwrap() = [accent().r(), accent().g(), accent().b()];
        }

        // In the tray, stay out of the game's way: eframe still runs this when something asks
        // for a repaint, so build nothing (nothing then asks for more), let go of the clip
        // thumbnails' GPU memory, and hand the app's idle memory back to Windows.
        // Windows' own answer: egui's `visible()` only knows minimized and covered, not hidden.
        let hidden = overlay::is_hidden();
        if hidden && self.capturing.is_none() {
            match self.slim {
                Slim::Visible => {
                    self.textures.clear();
                    self.covers.clear();
                    self.previews.forget();
                    // One more pass paints, which is when the freed textures actually go.
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
                    ui.ctx().request_repaint();
                    self.slim = Slim::Freed;
                }
                Slim::Freed => {
                    // Not while the app is still starting up: that work would just fill it again.
                    let settle = std::time::Duration::from_secs(15);
                    if self.started.elapsed() < settle {
                        ui.ctx().request_repaint_after(settle - self.started.elapsed());
                    } else {
                        trim_memory();
                        log::write("Window hidden: freed thumbnails and trimmed idle memory.");
                        self.slim = Slim::Trimmed;
                    }
                }
                Slim::Trimmed => {}
            }
            return;
        }
        self.slim = Slim::Visible;

        // Closing (Alt+F4) or Esc only hides the overlay; Quit lives in the tray menu. During
        // setup nothing is recording yet, so closing quits instead of vanishing into the tray.
        // Esc pressed in a text box just leaves the box (focus is still last frame's here).
        let typing = ui.ctx().text_edit_focused();
        let (close, esc) = ui.input(|i| (i.viewport().close_requested(), i.key_pressed(egui::Key::Escape)));
        let in_setup = self.wizard.is_some();
        if close && !in_setup {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if !in_setup && (close || (esc && self.capturing.is_none() && !typing)) {
            overlay::close();
        }

        let bar = egui::Frame::new().fill(BAR).inner_margin(egui::Margin { left: 14, right: 14, top: 12, bottom: 0 });
        if let Some(step) = self.wizard {
            // One centered column, like the settings pages, however wide the window is.
            const W: f32 = 720.0;
            egui::Panel::top("wizard_header").frame(bar.inner_margin(egui::Margin::symmetric(16, 22))).resizable(false).show(ui, |ui| {
                column(ui, W, |ui| self.wizard_header(ui, step));
            });
            egui::Panel::bottom("wizard_footer").frame(egui::Frame::new().fill(BAR).inner_margin(egui::Margin::symmetric(16, 14))).resizable(false).show(ui, |ui| {
                column(ui, W, |ui| {
                    self.wizard_footer(ui, step);
                    ui.add_space(4.0);
                    ui.add(egui::Label::new(RichText::new(&self.status).small().color(MUTED)).truncate());
                });
            });
            egui::CentralPanel::default().frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::symmetric(16, 20))).show(ui, |ui| {
                let area = ui.max_rect().expand2(egui::vec2(16.0, 20.0));
                let back = ui.ctx().layer_painter(ui.layer_id()).with_clip_rect(area);
                glow(&back, area.center_top() + egui::vec2(0.0, -80.0), 620.0, alpha(accent(), 30));
                egui::ScrollArea::vertical().id_salt(("wizard", step)).auto_shrink(false).show(ui, |ui| column(ui, W, |ui| self.wizard_body(ui, step)));
            });
            return;
        }
        // Re-read every few seconds: the Clips page and the sidebar's games both show it.
        if self.clips_checked.elapsed() > std::time::Duration::from_secs(3) {
            self.clips_checked = std::time::Instant::now();
            self.clip_list = clips::list(&self.cfg.folder, usize::MAX);
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(3));
        let nav = egui::Frame::new().fill(SIDEBAR).inner_margin(egui::Margin { left: 12, right: 12, top: 0, bottom: 0 });
        egui::Panel::left("nav").frame(nav).exact_size(220.0).resizable(false).show(ui, |ui| self.sidebar(ui));
        // Only while there are settings to save (or waiting for a recorder restart).
        if self.cfg != self.saved || recorder_settings(&self.cfg) != self.applied {
            let frame = egui::Frame::new().fill(CARD).stroke(egui::Stroke::new(1.0, BORDER)).inner_margin(egui::Margin::symmetric(24, 12));
            egui::Panel::bottom("apply").frame(frame).resizable(false).show(ui, |ui| self.footer(ui));
        }
        // The player only lives on the Clips tab, and pauses before the window hides.
        if self.tab != Tab::Clips {
            self.player = None;
        }
        if self.tab != Tab::Logs {
            self.perf = None;
        }
        if HIDE_REQUEST.swap(false, std::sync::atomic::Ordering::Relaxed) {
            if let Some((_, p)) = &mut self.player {
                p.pause();
            }
            overlay::close();
        }
        // Read before this frame's dialogs and text fields, so the Enter that closes the rename box
        // or leaves the search box doesn't also play a clip.
        let clip_keys = !self.dialog_open() && !ui.ctx().egui_wants_keyboard_input();
        self.dialogs(ui.ctx());
        let dialog_open = self.dialog_open();
        if let Some((_, p)) = &mut self.player {
            // A child window draws over everything, dialogs included.
            p.set_visible(!dialog_open);
        }
        PLAYING.store(self.player.as_ref().is_some_and(|(_, p)| p.is_playing()), std::sync::atomic::Ordering::Relaxed);
        let page = egui::Frame::new().fill(BG).inner_margin(egui::Margin { left: 28, right: 28, top: 22, bottom: 12 });
        egui::CentralPanel::default().frame(page).show(ui, |ui| {
            // An open clip takes the whole page: the video is a child window, so it can't scroll.
            if self.tab == Tab::Clips && self.player.is_some() {
                self.player_page(ui);
                return;
            }
            self.top_bar(ui);
            let (title, about) = match self.tab {
                Tab::Clips => ("Clips", ""),
                Tab::General => ("General", "Clip length, where clips go, and how the app behaves."),
                Tab::Keybinds => ("Keybinds", "Keys that work anywhere, even in a game."),
                Tab::Video => ("Video", "What gets recorded and how it looks."),
                Tab::Audio => ("Audio", "Which sounds go into clips, and on which tracks."),
                Tab::Logs => ("Logs", "How the recorder is doing."),
            };
            if self.tab == Tab::Clips {
                self.clips_header(ui);
            } else if self.tab == Tab::Logs {
                page_title(ui, title, about);
            } else {
                // Same width as the settings column below.
                ui.scope(|ui| {
                    ui.set_max_width(ui.available_width().min(760.0));
                    ui.horizontal(|ui| {
                        ui.label(strong(title, 24.0).color(Color32::WHITE));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Restore defaults").on_hover_text("Puts this page back the way a fresh install sets it up on this PC. Nothing changes until you Save.").clicked() {
                                self.restore_defaults(self.tab);
                            }
                        });
                    });
                    ui.label(RichText::new(about).color(MUTED));
                    ui.add_space(14.0);
                });
            }
            ui.scope(|ui| {
                if !matches!(self.tab, Tab::Clips | Tab::Logs) {
                    ui.set_max_width(ui.available_width().min(760.0));
                }
                self.tip(ui, title);
            });
            // The log has its own scroll area that follows new lines.
            if self.tab == Tab::Logs {
                self.logs_tab(ui);
                return;
            }
            egui::ScrollArea::vertical().id_salt(("tab", self.tab as u8)).auto_shrink(false).show(ui, |ui| {
                // Settings read best in a column, not stretched across a wide window.
                if self.tab != Tab::Clips {
                    ui.set_max_width(ui.available_width().min(760.0));
                }
                match self.tab {
                    Tab::General => self.general_tab(ui),
                    Tab::Clips => self.clips_tab(ui, clip_keys),
                    Tab::Keybinds => self.keybinds_tab(ui),
                    Tab::Video => self.video_tab(ui),
                    Tab::Audio => self.audio_tab(ui),
                    Tab::Logs => {}
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl App {
        /// The app with the saved settings and nothing running (no recorder, tray, hotkeys or
        /// window), for rendering its screens in tests.
        fn preview(cc: &eframe::CreationContext) -> App {
            apply_theme(&cc.egui_ctx);
            let (cmds, _) = mpsc::channel();
            let (_, events) = mpsc::channel();
            let ctx = cc.egui_ctx.clone();
            let mut app = App {
                cfg: Config::load(),
                cmds,
                events,
                status: "Instant replay is on.".into(),
                connected: true,
                buffer: true,
                start_with_windows: true,
                devices: Vec::new(),
                open_apps: Vec::new(),
                monitors: display::list(),
                applied: String::new(),
                hotkey_error: None,
                capturing: None,
                tab: Tab::General,
                saved: Config::default(),
                started: std::time::Instant::now(),
                minimized: false,
                tray: None,
                recording: false,
                tray_look: None,
                wizard: None,
                obs_exe: None,
                job: None,
                job_error: None,
                log_view: Vec::new(),
                log_stamp: (0, None),
                log_checked: std::time::Instant::now(),
                hw: profile::detect(),
                recommended: None,
                firewall_ok: None,
                obs_settings_found: false,
                imported: None,
                clip_list: Vec::new(),
                clips_checked: std::time::Instant::now() - std::time::Duration::from_secs(60),
                previews: clips::Previews::new(move || ctx.request_repaint()),
                textures: Default::default(),
                covers: Default::default(),
                recent_at: 0,
                lengths: Default::default(),
                scrub_wait: None,
                trim: None,
                clips_msg: None,
                player: None,
                view: ClipsView::Games,
                links: Default::default(),
                uploads: Default::default(),
                confirm_upload: None,
                exports: Default::default(),
                exported: None,
                confirm_remove: false,
                import: None,
                favorites: Default::default(),
                renaming: None,
                tags: Default::default(),
                tagging: None,
                selected: None,
                search: String::new(),
                montage: None,
                cleanup_preview: None,
                archive_preview: None,
                perf: None,
                encoder_hint: None,
                slim: Slim::Visible,
                phone: None,
                phone_ips: (std::time::Instant::now() - std::time::Duration::from_secs(60), Vec::new()),
            };
            app.refresh_devices();
            app.applied = recorder_settings(&app.cfg);
            // Nothing to save: the screens are only looked at.
            app.saved = app.cfg.clone();
            app
        }
    }

    /// Renders each tab to target\screens\<tab>.png, to look at the design without opening the
    /// app. Reads the clip folder (for thumbnails) and changes nothing. Run on demand:
    /// cargo test screens -- --ignored
    #[test]
    #[ignore]
    fn screens() {
        unsafe { std::env::set_var("WGPU_BACKEND", "dx12") };
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(r"target\screens");
        std::fs::create_dir_all(&out).unwrap();
        let size: Vec<f32> = std::env::var("SCREEN_SIZE").unwrap_or("480x640".into()).split('x').map(|n| n.parse().unwrap()).collect();
        let tabs = [(Tab::Clips, "clips"), (Tab::General, "general"), (Tab::Keybinds, "keybinds"), (Tab::Video, "video"), (Tab::Audio, "audio"), (Tab::Logs, "logs")];
        let pages = tabs
            .map(|(t, n)| (t, None, n.to_string()))
            .into_iter()
            .chain([(Tab::Video, None, "unsaved".to_string())])
            .chain([(Tab::Video, None, "video-advanced".to_string()), (Tab::Audio, None, "audio-advanced".to_string())])
            .chain([(Tab::Clips, None, "clips-starred".to_string()), (Tab::General, None, "general-cleanup".to_string())])
            .chain([(Tab::Clips, None, "clips-search".to_string()), (Tab::Clips, None, "clips-rename".to_string())])
            .chain([(Tab::Clips, None, "clips-games".to_string()), (Tab::Clips, None, "clips-game".to_string())])
            .chain([(Tab::Clips, None, "clips-selected".to_string()), (Tab::Clips, None, "clips-scrub".to_string())])
            .chain([(Tab::Clips, None, "clips-export".to_string()), (Tab::Clips, None, "clips-export-menu".to_string())])
            .chain([(Tab::Clips, None, "clips-tags".to_string()), (Tab::Clips, None, "clips-tag-menu".to_string()), (Tab::Clips, None, "clips-new-tag".to_string())])
            .chain([(Tab::Clips, None, "clips-tag-filter".to_string())])
            .chain([(Tab::Clips, None, "clips-montage".to_string()), (Tab::Clips, None, "clips-montage-making".to_string())])
            .chain([(Tab::General, None, "general-discord".to_string()), (Tab::General, None, "general-discord-bad".to_string()), (Tab::Clips, None, "clips-discord-menu".to_string())])
            .chain([(Tab::General, None, "general-phone".to_string()), (Tab::General, None, "general-archive".to_string())])
            .chain([(Tab::General, None, "general-move".to_string()), (Tab::General, None, "general-import".to_string())])
            .chain(tabs.map(|(t, n)| (t, None, format!("{n}-tip"))))
            .chain([(Tab::General, None, "general-accent".to_string()), (Tab::General, None, "general-no-tips".to_string()), (Tab::Clips, None, "clips-selected-accent".to_string())])
            .chain([(Tab::Video, None, "video-hint".to_string()), (Tab::Video, None, "video-hint-advanced".to_string())])
            .chain([(Tab::Audio, None, "audio-apps-advanced".to_string()), (Tab::Video, None, "video-follow-advanced".to_string())])
            .chain([(Tab::Video, None, "video-hdr".to_string()), (Tab::Video, None, "video-hdr-advanced".to_string())])
            .chain((0..STEPS.len()).map(|s| (Tab::Clips, Some(s), format!("setup{}", s + 1))));
        // Copies of the newest clips sorted into game folders, for the games pages; the real
        // clips folder is only read.
        let games = out.join("games");
        for (i, c) in clips::list(&Config::load().folder, 2).iter().enumerate() {
            for dir in [["Desktop", "VALORANT"][i], [".", "Elden Ring"][i]] {
                let to = games.join(dir).join(&c.name);
                if !to.exists() {
                    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
                    std::fs::copy(&c.path, &to).unwrap();
                }
            }
        }
        for (tab, wizard, name) in pages {
            let builder = egui_kittest::Harness::builder().with_size([size[0], size[1]]).with_accessibility_check(false);
            let mut h = builder.wgpu().build_eframe(|cc| {
                let mut app = App::preview(cc);
                app.tab = tab;
                app.wizard = wizard;
                // A change waiting for Save, to show the bar.
                if name == "unsaved" {
                    app.cfg.fps = if app.cfg.fps == 60 { 120 } else { 60 };
                }
                // The skipped-frames note, as after a health check at Best quality.
                if name.starts_with("video-hint") {
                    (app.cfg.encoder_effort, app.saved.encoder_effort) = (0, 0);
                    app.encoder_hint = Some((412, 0));
                }
                // Basic is the default view; these show the full pages.
                app.cfg.advanced = name.ends_with("-advanced");
                app.saved.advanced = app.cfg.advanced;
                // Green and blue accents (in memory only).
                if name.ends_with("-accent") {
                    app.cfg.accent = if name == "general-accent" { 2 } else { 1 };
                    app.saved.accent = app.cfg.accent;
                }
                if name == "general-no-tips" {
                    app.cfg.show_tips = false;
                    app.saved.show_tips = false;
                }
                // First-run tips only on the "-tip" pages.
                if !name.ends_with("-tip") {
                    app.cfg.tips_seen = ["Clips", "General", "Keybinds", "Video", "Audio", "Logs"].map(String::from).to_vec();
                    app.saved.tips_seen = app.cfg.tips_seen.clone();
                }
                // Stars the second-newest clip (in memory only; favorites.json isn't written).
                if name == "clips-starred" {
                    app.favorites = clips::list(&app.cfg.folder, 2).iter().skip(1).map(|c| c.path.display().to_string()).collect();
                }
                // A search that matches one clip, and the rename box (only shown; nothing is renamed).
                if name == "clips-search" {
                    app.search = "10-53".into();
                }
                if name.starts_with("clips-game") {
                    app.cfg.folder = games.display().to_string();
                    app.saved.folder = app.cfg.folder.clone();
                    app.applied = recorder_settings(&app.cfg);
                }
                if name == "clips-game" {
                    app.view = ClipsView::Game(Some("VALORANT".into()));
                }
                // The newest clip picked, as after a click or an arrow key.
                if name.starts_with("clips-selected") {
                    app.selected = clips::list(&app.cfg.folder, 1).first().map(|c| c.path.clone());
                }
                // An export running on the newest clip and one finished (only shown; nothing is encoded).
                if name == "clips-export"
                    && let [a, b] = &clips::list(&app.cfg.folder, 2)[..]
                {
                    let job = ExportJob::default();
                    job.pct.store(40, std::sync::atomic::Ordering::Relaxed);
                    app.exports.insert(a.path.clone(), Arc::new(job));
                    let out = clips::copy_name(&b.path, "Discord 10MB");
                    let msg = format!("Saved {} (8.6 MB).", out.file_name().unwrap().to_string_lossy());
                    app.clips_msg = Some(msg.clone());
                    app.exported = Some((msg, out));
                }
                // Tags on the two newest clips (in memory only; tags.json isn't written).
                if name.starts_with("clips-tag") || name == "clips-new-tag" || name == "clips-games" {
                    for (i, c) in clips::list(&app.cfg.folder, 2).iter().enumerate() {
                        for t in [&["Aces", "Funny"][..], &["Aces"]][i] {
                            clips::set_tag(&mut app.tags, &c.path, t, true);
                        }
                    }
                }
                // Cloud backup on (in memory only; no folder, so nothing is copied), the newest clip picked.
                if name == "clips-tag-menu" {
                    app.cfg.backup = true;
                    app.saved = app.cfg.clone();
                    if let Some(c) = clips::list(&app.cfg.folder, 1).first() {
                        clips::set_tag(&mut app.tags, &c.path, clips::CLOUD, true);
                    }
                }
                // A made-up webhook (only shown; nothing is posted), and a wrong link.
                if name == "general-discord" || name == "clips-discord-menu" {
                    app.cfg.discord_webhook = "https://discord.com/api/webhooks/123/made-up-token".into();
                    app.saved = app.cfg.clone();
                }
                if name == "general-discord-bad" {
                    app.cfg.discord_webhook = "https://discord.com/channels/123".into();
                    app.saved = app.cfg.clone();
                }
                // The phone page on, with a made-up key; tests only open it on 127.0.0.1.
                if name == "general-phone" {
                    app.cfg.phone_page = true;
                    app.cfg.phone_key = "0123456789abcdef0123456789abcdef".into();
                    app.saved = app.cfg.clone();
                }
                // Montage mode with the two newest clips picked (second newest first) and made-up
                // music; then one of them being made (only shown; nothing is encoded).
                if name.starts_with("clips-montage") {
                    let n = if name == "clips-montage" { 2 } else { 1 };
                    let clips = clips::list(&app.cfg.folder, n).iter().rev().map(|c| c.path.clone()).collect();
                    let job = (name == "clips-montage-making").then(|| {
                        let job = ExportJob::default();
                        job.pct.store(40, std::sync::atomic::Ordering::Relaxed);
                        Arc::new(job)
                    });
                    app.montage = Some(Montage { clips, fades: true, music: Some("Night Drive.mp3".into()), volume: 0.4, job });
                    app.view = ClipsView::All;
                }
                if name == "clips-tag-filter" {
                    app.view = ClipsView::Tag("Funny".into());
                }
                if name == "clips-new-tag" {
                    app.tagging = clips::list(&app.cfg.folder, 1).first().map(|c| (c.path.clone(), "Clutch".into()));
                }
                if name == "clips-rename" {
                    app.renaming = clips::list(&app.cfg.folder, 1).first().map(|c| (c.path.clone(), "Ace on Ascent".into()));
                }
                // "Only these apps" with two made-up picks (only shown; the recorder isn't touched).
                if name == "audio-apps-advanced" {
                    app.cfg.only_apps = true;
                    app.cfg.apps = vec![
                        config::AppSel { title: "VALORANT  ".into(), class: "UnrealWindow".into(), exe: "VALORANT-Win64-Shipping.exe".into() },
                        config::AppSel { title: "#general | Friends - Discord".into(), class: "Chrome_WidgetWin_1".into(), exe: "Discord.exe".into() },
                    ];
                    app.saved = app.cfg.clone();
                    app.applied = recorder_settings(&app.cfg);
                }
                // The monitors shown as HDR (this PC's may not be).
                if name.starts_with("video-hdr") {
                    app.monitors.iter_mut().for_each(|m| m.hdr = Some(240));
                }
                if name == "video-follow-advanced" {
                    (app.cfg.capture, app.cfg.follow_mouse) = ("desktop".into(), true);
                    app.saved = app.cfg.clone();
                    app.applied = recorder_settings(&app.cfg);
                }
                // Limits ticked, to show the card and what it would remove (nothing is removed).
                if name == "general-cleanup" {
                    (app.cfg.keep_under_gb, app.cfg.keep_days) = (100, 30);
                    app.saved = app.cfg.clone();
                }
                // Archive on, to a made-up NAS folder (nothing is moved).
                if name == "general-archive" {
                    (app.cfg.archive_days, app.cfg.archive_folder) = (14, r"\\nas\clips".into());
                    (app.cfg.backup, app.cfg.backup_folder) = (true, r"C:\Users\you\OneDrive\Replay Vault".into());
                    app.saved = app.cfg.clone();
                }
                // A made-up settings file picked for import (only shown; nothing is applied).
                if name == "general-import" {
                    let mut cfg = app.cfg.clone();
                    (cfg.buffer_secs, cfg.hotkey, cfg.folder) = (cfg.buffer_secs + 30, "Alt+F9".into(), r"Z:\Old PC\Clips".into());
                    let favorites = ["a.mp4".to_string(), "b.mp4".to_string()].into();
                    let tags = [("a.mp4".to_string(), ["Aces".to_string()].into())].into();
                    app.import = Some((config::Export::new(&cfg, &favorites, &tags), config::missing_folders(&cfg)));
                }
                app
            });
            // Thumbnails decode on another thread; give them time to arrive.
            for i in 0..60 {
                // The pointer two thirds across the first card's picture (at 1100x700), once its
                // length is known, to show hover scrubbing.
                if name == "clips-scrub" && i == 30 {
                    h.hover_at(egui::pos2(420.0, 215.0));
                }
                // The first card's menu open on Export for sharing.
                if name == "clips-export-menu" {
                    use egui_kittest::kittest::Queryable;
                    if i == 30 {
                        h.query_all_by_label(&icon::MORE.to_string()).next().unwrap().click();
                    }
                    if i == 35 {
                        h.get_by_label("Export for sharing").hover();
                    }
                }
                // Scrolled down to the Discord or Phone card.
                if (name.starts_with("general-discord") || name == "general-phone" || name == "general-archive" || name == "general-move" || name == "general-accent" || name == "general-no-tips") && i == 30 {
                    use egui_kittest::kittest::Queryable;
                    let label = match name.as_str() {
                        "general-phone" => "New key",
                        "general-move" => "Export settings...",
                        "general-accent" | "general-no-tips" => "Accent color",
                        "general-archive" => "Move clips older than",
                        _ => "Webhook",
                    };
                    h.query_all_by_label(label).next().unwrap().scroll_to_me();
                }
                // The monitor list open.
                if name == "video-follow-advanced" && i == 30 {
                    use egui_kittest::kittest::Queryable;
                    h.get_by_value("Follow the mouse").click();
                }
                if name == "audio-apps-advanced" && i == 30 {
                    use egui_kittest::kittest::Queryable;
                    h.get_by_label("Refresh").scroll_to_me();
                }
                if name == "clips-tag-menu" || name == "clips-discord-menu" {
                    use egui_kittest::kittest::Queryable;
                    if i == 30 {
                        h.query_all_by_label(&icon::MORE.to_string()).next().unwrap().click();
                    }
                    if i == 35 && name == "clips-tag-menu" {
                        h.get_by_label("Tags").hover();
                    }
                }
                h.step();
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            h.render().unwrap().save(out.join(format!("{name}.png"))).unwrap();
        }
        // The trim timeline with a made-up 30 s waveform (quiet, a loud burst, two shots, quiet),
        // part kept, with its loud moments marked.
        let wave: Vec<f32> = (0..30 * media::PEAKS_PER_SEC).map(|i| if (200..260).contains(&i) { 0.9 } else if i == 80 || i == 440 { 0.7 } else { 0.05 + 0.1 * ((i as f32) * 0.7).sin().abs() }).collect();
        let parts_wave = wave.clone();
        let mut h = egui_kittest::Harness::builder().with_size([size[0] - 260.0, 100.0]).wgpu().build_ui(move |ui| {
            apply_theme(ui.ctx());
            let (mut view, mut start, mut end) = ((0.0, 30.0), 6.0, 18.0);
            egui::Frame::new().fill(CARD).inner_margin(12).show(ui, |ui| {
                range_bar(ui, 30.0, &mut view, &mut start, &mut end, 8.0, false, &wave, &media::loud_moments(&wave), &[]);
            });
        });
        h.step();
        h.render().unwrap().save(out.join("waveform.png")).unwrap();
        // Two parts kept (green) and a third being picked, with their chips and the save button.
        let mut h = egui_kittest::Harness::builder().with_size([size[0] - 260.0, 150.0]).wgpu().build_ui(move |ui| {
            apply_theme(ui.ctx());
            let mut t = TrimEdit { path: "x.mp4".into(), len: 30.0, start: 14.0, end: 18.0, view: (0.0, 30.0), gains: vec![], want_split: false, wave: Arc::default(), job: None, parts: vec![(2.0, 5.5), (20.0, 26.0)], gif: None, gif_msg: None, loud: None };
            egui::Frame::new().fill(CARD).inner_margin(12).show(ui, |ui| {
                range_bar(ui, 30.0, &mut t.view, &mut t.start, &mut t.end, 15.0, false, &parts_wave, &[], &t.parts);
                parts_row(ui, &mut t, false);
                primary(ui, true, RichText::new(format!("Save {} parts as one clip", t.parts.len())).strong(), egui::Vec2::ZERO);
            });
        });
        h.step();
        h.render().unwrap().save(out.join("trim-parts.png")).unwrap();
        // Save as GIF: a 4 s part (ready), the whole 30 s (too long), and one being made.
        let mut h = egui_kittest::Harness::builder().with_size([520.0, 140.0]).wgpu().build_ui(move |ui| {
            apply_theme(ui.ctx());
            let gif = Arc::new(ExportJob::default());
            gif.pct.store(40, std::sync::atomic::Ordering::Relaxed);
            for (start, end, gif) in [(6.0, 10.0, None), (0.0, 30.0, None), (6.0, 10.0, Some(gif))] {
                let busy = gif.is_some();
                let mut t = TrimEdit { path: "x.mp4".into(), len: 30.0, start, end, view: (0.0, 30.0), gains: vec![], want_split: false, wave: Arc::default(), job: None, parts: vec![], gif, gif_msg: None, loud: None };
                ui.horizontal(|ui| {
                    primary(ui, !busy, RichText::new("Save trimmed copy").strong(), egui::Vec2::ZERO);
                    gif_button(ui, &mut t, busy);
                    ui.label(RichText::new(format!("{} s selected", end - start)).small().color(MUTED));
                });
            }
        });
        h.step();
        h.render().unwrap().save(out.join("trim-gif.png")).unwrap();
    }

    /// The tray icon's three looks: 32x32, a coloured dot in the middle, clear corners.
    #[test]
    fn tray_icon_looks() {
        assert_eq!(TrayLook::of(true, true, false), TrayLook::Buffer);
        assert_eq!(TrayLook::of(true, false, true), TrayLook::Recording);
        assert_eq!(TrayLook::of(true, false, false), TrayLook::Off);
        assert_eq!(TrayLook::of(false, true, true), TrayLook::Off);
        let centre = ((16 * TRAY_N + 16) * 4) as usize;
        for (look, rgb) in [(TrayLook::Off, [120, 120, 128]), (TrayLook::Buffer, [178, 136, 255]), (TrayLook::Recording, [235, 60, 60])] {
            let px = tray_rgba(look);
            assert_eq!(px.len(), (TRAY_N * TRAY_N * 4) as usize);
            assert_eq!(px[centre..centre + 4], [rgb[0], rgb[1], rgb[2], 255], "{look:?}");
            assert_eq!(px[3], 0, "corner is see-through");
            assert!(tray_icon_image(look).is_ok());
        }
    }

    /// Basic audio reads simple setups back as they were set, and leaves custom ones alone.
    #[test]
    fn basic_audio_round_trip() {
        let mut c = Config::default();
        c.devices.clear();
        for split in [false, true] {
            let b = BasicAudio { out: Some("out:game".into()), mic: Some("in:mic".into()), split };
            set_basic_audio(&mut c, &b);
            assert_eq!(basic_audio(&c), Some(b));
        }
        // No mic: the split goes away, everything is one track.
        set_basic_audio(&mut c, &BasicAudio { out: Some("out:game".into()), mic: None, split: true });
        assert_eq!(c.track_count, 1);
        assert_eq!(basic_audio(&c), Some(BasicAudio { out: Some("out:game".into()), mic: None, split: false }));

        // Two playback devices on custom tracks (full mix / game + chat / mic) is custom.
        let mut c = Config { track_count: 3, ..Config::default() };
        c.devices.clear();
        let sel = |t: [bool; 3]| config::DeviceSel { name: String::new(), tracks: t };
        c.devices.insert("out:chat".into(), sel([true, true, false]));
        c.devices.insert("out:game".into(), sel([true, true, false]));
        c.devices.insert("in:mic".into(), sel([true, false, true]));
        assert_eq!(basic_audio(&c), None);
        let guess = basic_audio_guess(&c);
        assert_eq!((guess.out.as_deref(), guess.mic.as_deref(), guess.split), (Some("out:chat"), Some("in:mic"), true));
    }

    /// Changes show the Save bar; Discard and Restore defaults work. Never clicks Save, so the
    /// settings file is left alone.
    #[test]
    fn save_bar_discard_and_defaults() {
        use egui_kittest::kittest::Queryable;
        let mut h = egui_kittest::Harness::builder().with_size([1100.0, 700.0]).with_accessibility_check(false).build_eframe(|cc| {
            let mut app = App::preview(cc);
            app.tab = Tab::Keybinds;
            app
        });
        h.run();
        assert!(h.query_by_label("Unsaved changes").is_none());
        let saved = h.state().cfg.clone();

        h.state_mut().cfg.save_popup = !saved.save_popup;
        h.run();
        assert!(h.query_by_label("Unsaved changes").is_some());
        h.get_by_label("Discard").click();
        h.run();
        assert!(h.state().cfg == saved);
        assert!(h.query_by_label("Unsaved changes").is_none());

        h.get_by_label("Restore defaults").click();
        h.run();
        assert_eq!(h.state().cfg.hotkey, Config::default().hotkey);
        assert_eq!(h.state().cfg.folder, saved.folder);
        h.get_by_label("Discard").click();
        h.run();
        assert!(h.state().cfg == saved);
    }

    /// A range_bar for a 10 s clip on a 1000 px panel, driven with the input events a real mouse
    /// sends, a frame at a time.
    struct Bar {
        ctx: egui::Context,
        start: f64,
        end: f64,
        view: (f64, f64),
        rect: egui::Rect,
    }

    impl Bar {
        const Y: f32 = 18.0;

        fn new() -> Bar {
            let mut b = Bar { ctx: egui::Context::default(), start: 0.0, end: 10.0, view: (0.0, 10.0), rect: egui::Rect::NOTHING };
            b.frame(vec![], None);
            b
        }

        /// One frame; `playing` is the playhead while the clip plays.
        fn frame(&mut self, events: Vec<egui::Event>, playing: Option<f64>) {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 200.0))),
                events,
                ..Default::default()
            };
            let (view, start, end, rect) = (&mut self.view, &mut self.start, &mut self.end, &mut self.rect);
            self.ctx
                .run_ui(input, |ui| {
                    ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                    *rect = ui.available_rect_before_wrap();
                    let _ = range_bar(ui, 10.0, view, start, end, playing.unwrap_or(0.0), playing.is_some(), &[], &[], &[]);
                })
                .textures_delta
                .clear();
        }

        /// Where time `t` is on the bar right now.
        fn x_of(&self, t: f64) -> f32 {
            self.rect.left() + ((t - self.view.0) / self.view.1) as f32 * self.rect.width()
        }

        /// A press at `from_x`, a drag to `to_x` a few pixels a frame, and a release.
        fn drag(&mut self, from_x: f32, to_x: f32) {
            let button = |x, pressed| egui::Event::PointerButton {
                pos: egui::pos2(x, Self::Y),
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            self.frame(vec![egui::Event::PointerMoved(egui::pos2(from_x, Self::Y))], None);
            self.frame(vec![button(from_x, true)], None);
            let steps = 20;
            for k in 1..=steps {
                let x = from_x + (to_x - from_x) * k as f32 / steps as f32;
                self.frame(vec![egui::Event::PointerMoved(egui::pos2(x, Self::Y))], None);
            }
            self.frame(vec![button(to_x, false)], None);
            self.frame(vec![], None);
        }

        /// The wheel turned `lines` notches (up is positive) over `x`, with keys held, then a few
        /// frames for egui's smooth scrolling to play out.
        fn wheel(&mut self, x: f32, lines: f32, modifiers: egui::Modifiers) {
            self.frame(vec![egui::Event::PointerMoved(egui::pos2(x, Self::Y))], None);
            self.frame(vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(0.0, lines),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers,
            }], None);
            for _ in 0..30 {
                self.frame(vec![], None);
            }
        }
    }
    #[test]
    fn tracks_are_named_after_their_devices() {
        let mut cfg = Config { track_count: 3, ..Default::default() };
        cfg.devices.clear();
        for (key, name, tracks) in [
            ("out:chat", "Chat (TC-Helicon GoXLR Mini)", [true, true, false]),
            ("out:game", "Game (TC-Helicon GoXLR Mini)", [true, true, false]),
            ("out:system", "System (TC-Helicon GoXLR Mini)", [true, false, false]),
            ("out:music", "Music (TC-Helicon GoXLR Mini)", [false, false, false]),
            ("in:mic", "Chat Mic (TC-Helicon GoXLR Mini)", [true, false, true]),
        ] {
            cfg.devices.insert(key.into(), config::DeviceSel { name: name.into(), tracks });
        }
        let names: Vec<String> = (0..3).map(|i| track_name(&cfg, i)).collect();
        assert_eq!(names, ["Everything", "Chat + Game", "Chat Mic"]);
        // A name the user typed wins; clearing it goes back to the devices.
        cfg.track_names[1] = "Comms + game".into();
        assert_eq!(track_name(&cfg, 1), "Comms + game");
        cfg.track_names[1] = "  ".into();
        assert_eq!(track_name(&cfg, 1), "Chat + Game");
        // A track nothing is recorded on.
        cfg.devices.get_mut("in:mic").unwrap().tracks = [true, false, false];
        assert_eq!(track_name(&cfg, 2), "Track 3");
    }

    #[test]
    fn arrow_keys_walk_the_clip_grid() {
        use egui::Key::*;
        // A day with 3 + 1 cards, then a day with 2.
        let rows = [3, 1, 2];
        assert_eq!(grid_step(&rows, (0, 2), ArrowRight), (1, 0));
        assert_eq!(grid_step(&rows, (1, 0), ArrowLeft), (0, 2));
        assert_eq!(grid_step(&rows, (0, 2), ArrowDown), (1, 0));
        assert_eq!(grid_step(&rows, (1, 0), ArrowDown), (2, 0));
        assert_eq!(grid_step(&rows, (2, 1), ArrowUp), (1, 0));
        // The edges.
        assert_eq!(grid_step(&rows, (0, 0), ArrowLeft), (0, 0));
        assert_eq!(grid_step(&rows, (0, 1), ArrowUp), (0, 1));
        assert_eq!(grid_step(&rows, (2, 1), ArrowRight), (2, 1));
        assert_eq!(grid_step(&rows, (2, 1), ArrowDown), (2, 1));
    }

    /// Hover scrubbing asks for one frame at a time, and drops its frames (not the thumbnails)
    /// once there are too many. Uses a clip that doesn't exist, so nothing is decoded.
    #[test]
    fn scrub_frames_one_at_a_time_and_capped() {
        let mut h = egui_kittest::Harness::builder().with_accessibility_check(false).build_eframe(|cc| App::preview(cc));
        let ctx = h.ctx.clone();
        let app = h.state_mut();
        let path = std::env::temp_dir().join("instant-replay-no-such-clip.mp4");
        assert!(app.scrub_frame(&ctx, &path, 5.0, 160).is_none());
        assert!(app.scrub_frame(&ctx, &path, 7.0, 160).is_none());
        // 7 s waits until 5 s is done, then is asked for.
        assert_eq!(app.scrub_wait.as_ref().map(|w| w.1), Some(5.0));
        // Up to 10 s: the decoder may first be busy with the real clips' pictures (sidebar games).
        for _ in 0..1000 {
            if app.scrub_wait.is_none() {
                break;
            }
            app.scrub_frame(&ctx, &path, 7.0, 160);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(app.textures.contains_key(&(path.clone(), 50)) && app.textures.contains_key(&(path.clone(), 70)));
        // Past the cap, the scrub frames go and the thumbnail (1 s) stays.
        for t in 100..400 {
            app.textures.insert((path.clone(), t), None);
        }
        app.textures.insert((path.clone(), 10), None);
        app.scrub_frame(&ctx, &path, 9.0, 160);
        assert_eq!(app.textures.len(), 1);
        assert!(app.textures.contains_key(&(path.clone(), 10)));
    }

    /// The arrow keys pick clips on the Clips page and F2 opens the rename box for the one picked.
    /// Uses two pictures in a temp folder (Enter and Delete would open or recycle a file, so they
    /// aren't pressed here).
    #[test]
    fn clip_keys_pick_and_rename() {
        let dir = std::env::temp_dir().join("instant-replay-keys");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for n in ["a.png", "b.png"] {
            std::fs::write(dir.join(n), b"not really a picture").unwrap();
        }
        let folder = dir.display().to_string();
        let mut h = egui_kittest::Harness::builder().with_size([1100.0, 700.0]).with_accessibility_check(false).build_eframe(|cc| {
            let mut app = App::preview(cc);
            app.tab = Tab::Clips;
            // The keys work on the clip lists (Home's rows don't take them).
            app.view = ClipsView::All;
            app.cfg.folder = folder;
            app
        });
        h.run_steps(2);
        h.key_press(egui::Key::ArrowRight);
        h.run_steps(2);
        let first = h.state().selected.clone().expect("an arrow key picks the first clip");
        h.key_press(egui::Key::ArrowRight);
        h.run_steps(2);
        let second = h.state().selected.clone().unwrap();
        assert_ne!(first, second);
        h.key_press(egui::Key::F2);
        h.run_steps(2);
        assert_eq!(h.state().renaming.as_ref().map(|r| &r.0), Some(&second));
        // With the rename box up, the arrows leave the pick alone.
        h.key_press(egui::Key::ArrowLeft);
        h.run_steps(2);
        assert_eq!(h.state().selected.as_ref(), Some(&second));
        drop(h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Only tags of clips still in the folder are offered (in memory; tags.json isn't written).
    #[test]
    fn tags_of_gone_clips_are_not_offered() {
        let dir = std::env::temp_dir().join("instant-replay-tags-in-use");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.png"), b"x").unwrap();
        let folder = dir.display().to_string();
        let mut h = egui_kittest::Harness::builder().with_accessibility_check(false).build_eframe(|cc| {
            let mut app = App::preview(cc);
            (app.tab, app.cfg.folder) = (Tab::Clips, folder);
            app
        });
        h.run_steps(2);
        let app = h.state_mut();
        clips::set_tag(&mut app.tags, &dir.join("a.png"), "Aces", true);
        clips::set_tag(&mut app.tags, &dir.join("gone.png"), "Old", true);
        assert_eq!(app.tags_in_use(), ["Aces"]);
        drop(h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn range_bar_grabs_the_edge_you_press_on() {
        let mut b = Bar::new();
        // Start edge first, then the end edge: the second drag must move the end, not the start.
        b.drag(b.x_of(0.0) + 2.0, b.x_of(2.0));
        assert!((b.start - 2.0).abs() < 0.1 && b.end == 10.0, "after dragging start: {}..{}", b.start, b.end);
        b.drag(b.x_of(10.0) - 2.0, b.x_of(7.0));
        assert!((b.start - 2.0).abs() < 0.1 && (b.end - 7.0).abs() < 0.1, "after dragging end: {}..{}", b.start, b.end);
        // And back to the start edge.
        b.drag(b.x_of(2.0), b.x_of(1.0));
        assert!((b.start - 1.0).abs() < 0.1 && (b.end - 7.0).abs() < 0.1, "after dragging start again: {}..{}", b.start, b.end);
        // Dragging in the middle only scrubs.
        b.drag(b.x_of(4.0), b.x_of(5.0));
        assert!((b.start - 1.0).abs() < 0.1 && (b.end - 7.0).abs() < 0.1, "after scrubbing: {}..{}", b.start, b.end);
    }

    #[test]
    fn range_bar_zooms_with_ctrl_scroll() {
        let mut b = Bar::new();
        let ctrl = egui::Modifiers { ctrl: true, command: true, ..Default::default() };
        // Ctrl+wheel up over 5 s zooms in around it; the app itself doesn't zoom.
        b.wheel(b.x_of(5.0), 3.0, ctrl);
        assert!(b.view.1 < 9.0, "view after zooming in: {:?}", b.view);
        assert!((b.x_of(5.0) - (b.rect.left() + 0.5 * b.rect.width())).abs() < 2.0, "5 s stayed under the pointer: {:?}", b.view);
        assert_eq!(b.ctx.zoom_factor(), 1.0);
        // Dragging an edge zoomed in is finer: the same pixels move it less.
        b.start = 4.0;
        let before = b.view;
        b.drag(b.x_of(4.0), b.x_of(4.0) + 50.0);
        let moved = b.start - 4.0;
        assert!(moved > 0.0 && moved < 50.0 / b.rect.width() as f64 * 10.0, "start moved {moved} s at {before:?}");
        // Shift+wheel moves along.
        let at = b.view.0;
        b.wheel(b.x_of(5.0), -3.0, egui::Modifiers { shift: true, ..Default::default() });
        assert!(b.view.0 != at, "view didn't move: {:?}", b.view);
        // Playing keeps the playhead in view.
        b.frame(vec![], Some(9.5));
        assert!(b.view.0 <= 9.5 && 9.5 <= b.view.0 + b.view.1, "playhead out of view: {:?}", b.view);
        // Ctrl+wheel down zooms back out, no further than the whole clip.
        for _ in 0..10 {
            b.wheel(b.x_of(5.0), -3.0, ctrl);
        }
        assert_eq!(b.view, (0.0, 10.0));
    }
}
