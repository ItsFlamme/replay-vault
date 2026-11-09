#![windows_subsystem = "windows"]

mod amd;
mod app_update;
mod autopause;
mod clips;
mod config;
mod covers;
mod devices;
mod display;
mod drag_out;
mod feedback;
mod hotkey;
mod log;
mod media;
mod obs;
mod obs_import;
mod obs_install;
mod overlay;
mod perf;
mod profile;
mod recorder;
mod startup;
mod support;
mod player;
mod toast;
mod tweaks;
mod upload;
mod tray;
mod setup;
mod pages {
    pub(crate) mod clips;
    pub(crate) mod player;
    pub(crate) mod help;
    pub(crate) mod audio;
    pub(crate) mod video;
    pub(crate) mod keybinds;
    pub(crate) mod general;
    pub(crate) mod whats_new;
}
mod ui {
    pub(crate) mod theme;
    pub(crate) mod widgets;
}

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui::{self, Color32, RichText};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use config::{Config, recorder_settings};
use obs::{Cmd, Event};
use ui::theme::*;
use ui::widgets::*;
use tray::*;
use pages::clips::*;
use pages::player::*;
use pages::audio::*;
use pages::keybinds::*;
use pages::general::GeneralPart;

/// Built from a work branch rather than master or a release (see build.rs).
pub(crate) const DEV_BUILD: bool = option_env!("RV_DEV_BUILD").is_some();
/// The window's title, which a second launch also uses to find the running copy.
pub(crate) const APP_TITLE: &str = if DEV_BUILD { "Replay Vault (Dev build)" } else { "Replay Vault" };

fn main() -> eframe::Result {
    // DLLs loaded by name from here on come from System32 only, never from the exe's folder
    // (often Downloads) or the current folder. build.rs does the same for the exe's own imports.
    {
        use windows::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories};
        let _ = unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) };
    }
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
    log::write(&format!("App started (version {}{}).", env!("CARGO_PKG_VERSION"), if DEV_BUILD { ", dev build" } else { "" }));
    startup::repair();
    // Draw the UI with DirectX 12, not Vulkan: Vulkan loads every installed overlay layer
    // (OBS, Overwolf, Steam, Epic) into this process. Safe here: no other threads exist yet.
    unsafe { std::env::set_var("WGPU_BACKEND", "dx12") };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 700.0])
            .with_min_inner_size([860.0, 560.0])
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../assets/replay-vault-256.png")).unwrap_or_default())
            .with_visible(false),
        ..Default::default()
    };
    eframe::run_native(APP_TITLE, options, Box::new(move |cc| Ok(Box::new(App::new(cc, minimized)))))
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
    /// A message to us, sent to the feedback bot.
    Feedback,
    /// How the recorder is doing, logs and the support zip (it was "Logs").
    Help,
    /// The changelog.
    WhatsNew,
}

impl Tab {
    /// The settings pages, in the sidebar's order under SETTINGS.
    const SETTINGS: [Tab; 5] = [Tab::General, Tab::Keybinds, Tab::Video, Tab::Audio, Tab::Feedback];
    const ALL: [Tab; 8] = [Tab::Clips, Tab::General, Tab::Keybinds, Tab::Video, Tab::Audio, Tab::Feedback, Tab::Help, Tab::WhatsNew];

    /// Its name (in the sidebar and title, and its key for More options and tips), its sidebar
    /// glyph, the line under its title, and its sidebar link's hover text.
    fn info(self) -> (&'static str, char, &'static str, &'static str) {
        match self {
            Tab::Clips => ("Clips", icon::CLIPS, "", "Your saved clips, to play, trim and share"),
            Tab::General => ("General", icon::SETTINGS, "Clip length, where clips go, and how the app behaves.", "Clip length, where clips go, cleanup and how the app behaves"),
            Tab::Keybinds => ("Keybinds", icon::KEYBOARD, "Keys that work anywhere, even in a game.", "The keys that save clips and more, even in a game"),
            Tab::Video => ("Video", icon::VIDEO, "What gets recorded and how it looks.", "Which screen is recorded and how the video looks"),
            Tab::Audio => ("Audio", icon::AUDIO, "Which sounds go into clips, and on which tracks.", "Which sounds go into clips"),
            Tab::Feedback => ("Feedback", icon::FEEDBACK, "Found a bug or have an idea? Tell us here.", "Send a bug report or an idea to the developer"),
            Tab::Help => ("Help", icon::HELP, "How recording is going, and where to get help.", "How recording is going, logs and a support zip"),
            Tab::WhatsNew => ("What's new", icon::NEW, "New things, fixes and changes in each version.", "What changed in each version"),
        }
    }

    fn name(self) -> &'static str {
        self.info().0
    }

    /// The settings Restore defaults puts back here, if any.
    fn page(self) -> Option<config::Page> {
        match self {
            Tab::General => Some(config::Page::General),
            Tab::Keybinds => Some(config::Page::Keybinds),
            Tab::Video => Some(config::Page::Video),
            Tab::Audio => Some(config::Page::Audio),
            Tab::Clips | Tab::Help | Tab::Feedback | Tab::WhatsNew => None,
        }
    }

    /// Its tape's stripe in Retro.
    fn stripe(self) -> Color32 {
        match self {
            Tab::Clips | Tab::Audio => accent(),
            Tab::General | Tab::Help | Tab::Feedback => MAGENTA,
            Tab::Keybinds | Tab::WhatsNew => TRACKING,
            Tab::Video => PHOSPHOR,
        }
    }
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
    /// When a key capture last ended. Esc cancels a capture on the hotkey thread a moment
    /// before the window sees the same press, which must not then hide the app.
    capture_ended: Option<std::time::Instant>,
    /// Keys another program or Windows also uses, with what that means, found by
    /// `check_conflicts` for the keys in `conflicts_keys`, at `conflicts_checked`.
    conflicts: Vec<(Target, String)>,
    conflicts_keys: Vec<String>,
    conflicts_checked: Option<std::time::Instant>,
    /// Every key as last registered; None until then, and while one is being set.
    registered: Option<Vec<String>>,
    /// Keys another app held when last tried, and when that was: they're tried again every few
    /// seconds, so they work once that app lets go (NVIDIA's overlay closing at start, say).
    hotkey_failed: Vec<Target>,
    hotkeys_tried: std::time::Instant,
    tab: Tab,
    /// The settings as last written to disk; the Save bar shows while `cfg` differs.
    saved: Config,
    started: std::time::Instant,
    minimized: bool,
    /// The tray icon and the menu items that change with the app's state.
    tray: Option<(TrayIcon, TrayMenu)>,
    /// A full recording is running.
    recording: bool,
    /// The tray icon as drawn: its look, and the accent it was drawn in.
    tray_look: Option<(TrayLook, Color32)>,
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
    /// Custom is picked on the Audio page, so it stays open even while its tracks match a choice.
    audio_custom: bool,
    /// Which of General's tabs is showing.
    general_part: GeneralPart,
    /// Whether our firewall rule exists; checked once when the OBS setup step shows.
    firewall_ok: Option<bool>,
    /// Setup's install also adds the firewall rule (ticked by default).
    setup_firewall: bool,
    /// NVIDIA's overlay: (switched on, its program running), looked at when the Keybinds page needs it.
    nvidia: Option<(bool, bool)>,
    /// AMD Software's settings (None inside if it isn't on this PC), looked at when needed.
    amd: Option<Option<amd::Amd>>,
    /// The Logs page's Send feedback card.
    feedback: feedback::Form,
    /// The user has their own OBS setup to import from, and what was last imported.
    obs_settings_found: bool,
    imported: Option<String>,
    /// Clips tab: the folder's clips (re-read every few seconds), their frames and lengths.
    clip_list: Vec<clips::Clip>,
    clips_checked: std::time::Instant,
    /// The archive folder and its clips (None: its drive or NAS isn't connected), looked at in the
    /// background while the Archive view is up (a NAS that's off can take a while to answer).
    archive_list: Option<(std::path::PathBuf, Option<Vec<clips::Clip>>)>,
    archive_scan: Option<std::sync::mpsc::Receiver<(std::path::PathBuf, Option<Vec<clips::Clip>>)>>,
    archive_checked: std::time::Instant,
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
    /// The new-version popup was closed with Later at this many finds (see `app_update::finds`).
    update_later: u32,
    /// The window was opened for the new-version popup at this many finds.
    update_pushed: u32,
    /// Installing a new version: its download, or why it failed.
    update_job: Option<obs_install::Shared>,
    update_error: Option<String>,
    /// A settings file picked with Import settings, waiting for its changes to be confirmed. Its
    /// folders aren't looked at until then (a network one would be contacted).
    import: Option<config::Export>,
    /// A clip waiting for "Post to Discord?" to be answered.
    confirm_discord: Option<std::path::PathBuf>,
    /// Starred clips, by path.
    favorites: std::collections::BTreeSet<String>,
    /// The clip being renamed and the name typed so far.
    renaming: Option<(std::path::PathBuf, String)>,
    /// Tags by clip path, and the clips getting a new tag with the name typed so far.
    tags: clips::Tags,
    tagging: Option<(Vec<std::path::PathBuf>, String)>,
    /// One-line notes by clip path, and the clip whose note is being typed on its card.
    notes: clips::Notes,
    noting: Option<(std::path::PathBuf, String)>,
    /// The clip picked with a click or the arrow keys, for Enter, F2 and Delete.
    selected: Option<std::path::PathBuf>,
    /// Clips picked with Ctrl+click, Shift+click or Ctrl+A, in the order picked; with 2 or more the
    /// selection bar acts on them all.
    picked: Vec<std::path::PathBuf>,
    /// Where a drag out of the app ended: Windows kept the button's release, so egui gets it here.
    drag_out_ended: Option<egui::Pos2>,
    /// When the window's place and page were last looked at (see `overlay::save_placement`).
    placement_checked: std::time::Instant,
    /// Picked clips waiting for "Move to Recycle Bin?" to be answered.
    confirm_recycle: Option<Vec<std::path::PathBuf>>,
    /// Share copies waiting to be made, one at a time after the running ones.
    export_queue: std::collections::VecDeque<(std::path::PathBuf, media::Share)>,
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
}

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

const STEPS: [&str; 3] = ["Your clips", "This PC", "You're ready"];

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
        if new_user
            && let Some(mon) = recorder::chosen_monitor(&cfg) {
            let s = profile::apply(&mut cfg, &hw, &mon);
            log::write(&format!("Detected {}. Recommended settings: {s}", hw.summary()));
            recommended = Some(s);
        }
        // Before anything looks for OBS: the private copy may live on a drive the user picked.
        obs_install::set_install_root(&cfg.obs_dir);
        // Before anything in it runs: only this account may change the private OBS copy.
        if obs_install::install_root().exists() {
            match obs_install::secure_install_root() {
                Ok(Some(fixed)) => log::write(&format!("The private OBS folder {} was open to other accounts ({fixed}); made it private again.", obs_install::install_root().display())),
                Ok(None) => {}
                // Never run OBS from a folder others can change: use the default one in this
                // account's profile instead (OBS is downloaded there if it isn't yet).
                Err(e) => {
                    log::write(&format!("The private OBS folder can't be kept private, so the default folder is used instead: {e:#}"));
                    obs_install::set_install_root("");
                    if obs_install::install_root().exists()
                        && let Err(e) = obs_install::secure_install_root()
                    {
                        log::write(&format!("Warning: the default OBS folder couldn't be checked either: {e:#}"));
                    }
                }
            }
        }
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

        let ctx = cc.egui_ctx.clone();
        app_update::start(move || ctx.request_repaint());

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
                hotkey::Action::Record => Cmd::ToggleRecord,
                hotkey::Action::Screenshot => Cmd::Screenshot,
                hotkey::Action::Replay => Cmd::ToggleReplay,
                hotkey::Action::Mic => Cmd::ToggleMic,
            };
            let _ = hotkey_cmds.lock().unwrap().send(cmd);
        });
        let ctx = cc.egui_ctx.clone();
        let previews = clips::Previews::new(move || ctx.request_repaint());

        autopause::start(cmd_tx.clone(), &REPLAY_ON, || Config::load().folder);
        let (tray, tray_error) = match make_tray(&cc.egui_ctx, cmd_tx.clone()) {
            Ok(t) => (Some(t), None),
            Err(e) => (None, Some(e)),
        };

        let mut app = Self {
            status: "Starting...".into(),
            start_with_windows: startup::is_enabled(),
            // Setup needs the window, even at a Windows-startup launch.
            minimized: minimized && !needs_setup,
            tray,
            wizard: needs_setup.then_some(0),
            obs_exe,
            log_checked: std::time::Instant::now() - std::time::Duration::from_secs(2),
            recommended,
            obs_settings_found: obs_import::found(),
            links: upload::links(),
            favorites: clips::favorites(),
            tags: clips::tags(),
            notes: clips::notes(),
            feedback: feedback::Form::load(),
            ..Self::base(cfg, cmd_tx, ev_rx, hw, previews)
        };
        if needs_setup {
            app.status.clear();
        }
        // The window's last place and page.
        let placement = overlay::load_placement();
        overlay::restore_placement(&placement);
        if let Some(tab) = Tab::ALL.into_iter().find(|t| t.name() == placement.page) {
            app.tab = tab;
        }
        if updated_app(&placement.version, !needs_setup) {
            app.tab = Tab::WhatsNew;
            log::write(&format!("Updated to {}: What's new opens first.", env!("CARGO_PKG_VERSION")));
        }
        if let Some(v) = updated {
            app.status = format!("OBS was updated to {v}.");
            log::write(&app.status);
        }
        match hook {
            Ok(()) => app.sync_hotkeys(),
            Err(e) => app.hotkey_error = Some(format!("Couldn't start listening for keys: {e}")),
        }
        if let Some(e) = tray_error {
            app.status = format!("Couldn't create the tray icon: {e}");
        }
        app.refresh_devices();
        // Persists the generated recorder password (and a new user's recommended settings). If
        // that fails (the status says so), the app still runs with them.
        if !app.write_settings() {
            app.saved = app.cfg.clone();
        }
        // The recorder starts once setup is done (OBS present, settings picked).
        app.put_in_effect(None);
        clips::start_cleanup();
        clips::start_archive();
        if !app.minimized {
            overlay::show();
        }
        app
    }

    /// Every field at its starting value, with nothing running and nothing read from disk but the
    /// monitors; `new` and the tests' `preview` fill in the rest.
    fn base(cfg: Config, cmds: Sender<Cmd>, events: Receiver<Event>, hw: profile::Hardware, previews: clips::Previews) -> Self {
        Self {
            cfg,
            cmds,
            events,
            status: String::new(),
            connected: false,
            buffer: false,
            start_with_windows: false,
            devices: Vec::new(),
            open_apps: Vec::new(),
            monitors: display::list(),
            applied: String::new(),
            hotkey_error: None,
            capturing: None,
            capture_ended: None,
            conflicts: Vec::new(),
            conflicts_keys: Vec::new(),
            conflicts_checked: None,
            registered: None,
            hotkey_failed: Vec::new(),
            hotkeys_tried: std::time::Instant::now(),
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
            hw,
            recommended: None,
            audio_custom: false,
            general_part: GeneralPart::Clips,
            firewall_ok: None,
            setup_firewall: true,
            nvidia: None,
            amd: None,
            feedback: Default::default(),
            obs_settings_found: false,
            imported: None,
            clip_list: Vec::new(),
            clips_checked: std::time::Instant::now().checked_sub(std::time::Duration::from_secs(60)).unwrap_or_else(std::time::Instant::now),
            archive_list: None,
            archive_scan: None,
            archive_checked: std::time::Instant::now(),
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
            links: Default::default(),
            uploads: Default::default(),
            confirm_upload: None,
            confirm_discord: None,
            exports: Default::default(),
            exported: None,
            confirm_remove: false,
            update_later: 0,
            update_pushed: 0,
            update_job: None,
            update_error: None,
            import: None,
            favorites: Default::default(),
            renaming: None,
            tags: Default::default(),
            tagging: None,
            notes: Default::default(),
            noting: None,
            selected: None,
            picked: Vec::new(),
            drag_out_ended: None,
            placement_checked: std::time::Instant::now(),
            confirm_recycle: None,
            export_queue: Default::default(),
            search: String::new(),
            montage: None,
            cleanup_preview: None,
            archive_preview: None,
            perf: None,
            encoder_hint: None,
            slim: Slim::Visible,
        }
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
        let look = (TrayLook::of(self.connected, self.buffer, self.recording), accent());
        if self.tray_look != Some(look) {
            self.tray_look = Some(look);
            let look = look.0;
            if let Some((tray, _)) = &self.tray {
                let _ = tray.set_icon(tray_icon_image(look).ok());
                let _ = tray.set_tooltip(Some(look.tooltip()));
            }
        }
        // The menu's handlers run while the window is hidden, so they read and leave state here.
        REPLAY_ON.store(self.buffer, std::sync::atomic::Ordering::Relaxed);
        if STARTUP_TOGGLED.swap(false, std::sync::atomic::Ordering::Relaxed) {
            self.start_with_windows = startup::is_enabled();
        }
        if let Some((_, menu)) = &mut self.tray {
            let status = match (self.connected, self.buffer) {
                (true, true) => format!("Recording: last {}", mmss(self.saved.buffer_secs)),
                (true, false) => "Paused".to_string(),
                _ => "Recorder not running".to_string(),
            };
            let shown = (status, format!("Save clip ({})", self.saved.hotkey), self.connected, self.buffer, self.start_with_windows);
            if menu.shown.as_ref() != Some(&shown) {
                menu.status.set_text(&shown.0);
                menu.save.set_text(&shown.1);
                menu.save.set_enabled(shown.3);
                menu.pause.set_text(if shown.3 { "Pause replay" } else { "Resume replay" });
                menu.pause.set_enabled(shown.2);
                menu.startup.set_checked(shown.4);
                menu.shown = Some(shown);
            }
        }
    }

    /// (Re)starts the recorder with the saved settings.
    fn apply(&mut self) {
        self.applied = recorder_settings(&self.saved);
        let _ = self.cmds.send(Cmd::Start(Box::new(self.saved.clone())));
    }

    /// Writes the settings to disk as they are now. False (and says why) if that failed.
    fn write_settings(&mut self) -> bool {
        match self.cfg.save() {
            Ok(()) => {
                self.saved = self.cfg.clone();
                true
            }
            Err(e) => {
                self.status = format!("Couldn't save settings: {e}");
                false
            }
        }
    }

    /// The Save button (and setup, as it goes): keeps the changes for next time and puts them
    /// into effect.
    fn keep_settings(&mut self) {
        let before = self.saved.clone();
        if self.write_settings() {
            self.put_in_effect(Some(&before));
        }
    }

    /// Keeps a view choice (More options open, the sort, a tip closed) right away, without Save.
    fn keep_view(&mut self, change: impl Fn(&mut Config)) {
        change(&mut self.cfg);
        change(&mut self.saved);
        if let Err(e) = self.saved.save() {
            self.status = format!("Couldn't save settings: {e}");
        }
    }

    /// Puts the saved settings into effect: the Windows tweaks that changed since `before` (at
    /// start, None: the ones that are on), what the recorder thread and the cleanup and archive
    /// read, and a recorder restart when a setting it runs with changed (not during setup).
    fn put_in_effect(&mut self, before: Option<&Config>) {
        let start = before.is_none();
        let before = before.cloned().unwrap_or_default();
        let s = &self.saved;
        if s.game_bar_off != before.game_bar_off
            && let Err(e) = tweaks::set_game_bar_off(s.game_bar_off) {
            log::write(&format!("Couldn't change Game Bar: {e:#}"));
            if !start {
                self.status = format!("Couldn't change Game Bar: {e:#}");
            }
        }
        // At every start too, in case an NVIDIA update switched it back on.
        if s.nvidia_off != before.nvidia_off {
            match tweaks::set_nvidia_overlay_off(s.nvidia_off) {
                // At start, closing it waits on another thread, so the window isn't held up.
                Ok(()) if s.nvidia_off && start => {
                    if tweaks::nvidia_overlay_running() {
                        std::thread::spawn(|| log::write(if tweaks::close_nvidia_overlay() { "Closed NVIDIA's overlay." } else { "NVIDIA's overlay is still running." }));
                    }
                }
                Ok(()) if s.nvidia_off => {
                    let closed = tweaks::close_nvidia_overlay();
                    log::write(if closed { "Turned NVIDIA's overlay off and closed it." } else { "Turned NVIDIA's overlay off; it's still running." });
                }
                Ok(()) => {
                    tweaks::start_nvidia_overlay();
                    log::write("Put NVIDIA's overlay setting back.");
                }
                Err(e) => {
                    log::write(&format!("Couldn't change NVIDIA's overlay: {e:#}"));
                    if !start {
                        self.status = format!("Couldn't change NVIDIA's overlay: {e:#}");
                    }
                }
            }
            self.nvidia = None;
        }
        // AMD Software's Instant Replay and hotkeys (untested), at every start too. Only switched
        // off on a PC that has AMD Software, so nothing is written for it anywhere else.
        let amd_off = self.saved.amd_off;
        if amd_off != before.amd_off && (!amd_off || amd::detect().is_some()) {
            match tweaks::set_amd_off(amd_off) {
                Ok(()) if start => {}
                Ok(()) if amd_off => log::write("Turned AMD Software's Instant Replay and hotkeys off (untested); it picks this up when it next starts."),
                Ok(()) => log::write("Put AMD Software's Instant Replay and hotkey settings back."),
                Err(e) => {
                    log::write(&format!("Couldn't change AMD Software's settings: {e:#}"));
                    if !start {
                        self.status = format!("Couldn't change AMD Software's settings: {e:#}");
                    }
                }
            }
            self.amd = None;
        }
        let s = &self.saved;
        obs::SAVE_POPUP.store(s.save_popup, std::sync::atomic::Ordering::Relaxed);
        obs::SORT_BY_GAME.store(s.sort_by_game, std::sync::atomic::Ordering::Relaxed);
        obs::SAVE_SOUND.store(s.save_sound, std::sync::atomic::Ordering::Relaxed);
        clips::set_cleanup(&s.folder, cleanup_rule(s));
        clips::set_archive(&s.folder, archive_rule(s));
        if self.wizard.is_none() && recorder_settings(s) != self.applied {
            self.apply();
        }
    }

    /// The Discard button: back to the saved settings.
    fn discard_settings(&mut self) {
        self.cfg = self.saved.clone();
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
    fn restore_defaults(&mut self, page: config::Page) {
        self.cfg = config::restore_page(&self.cfg, &self.defaults(), page);
    }

    /// Left side: the name, the pages and your games, and at the bottom the replay switch and
    /// the live status.
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        // The height kept free at the bottom for the status: a line more when it says why it
        // isn't recording.
        let status_h = if self.connected && self.buffer { 92.0 } else { 108.0 };
        // A hairline on its right edge. The whole panel, margins too.
        let panel = ui.max_rect().expand2(egui::vec2(12.0, 0.0));
        let back = ui.ctx().layer_painter(ui.layer_id()).with_clip_rect(panel);
        back.vline(panel.right() - 0.5, panel.y_range(), egui::Stroke::new(1.0, pal().border));
        ui.add_space(22.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            brand(ui);
        });
        ui.add_space(22.0);
        // Minimalist rows sit close, like a list; Retro's tape labels keep their room.
        let row_gap = if retro() { ui.spacing().item_spacing.y } else { 2.0 };
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = row_gap;
            self.nav_item(ui, Tab::Clips);
            ui.add_space(if retro() { 14.0 } else { 18.0 });
            side_heading(ui, "Settings");
            ui.add_space(2.0);
            for tab in Tab::SETTINGS {
                self.nav_item(ui, tab);
            }
            ui.add_space(16.0);
            self.nav_item(ui, Tab::Help);
            self.nav_item(ui, Tab::WhatsNew);
        });

        // The games, played last first, each opening its clips. Left out of a window too short
        // for their heading and one row, so the status below stays on screen.
        let games = self.games();
        if !games.is_empty() && ui.available_height() - status_h >= 44.0 + 46.0 {
            ui.add_space(20.0);
            side_heading(ui, "Your games");
            ui.add_space(2.0);
            let room = (ui.available_height() - status_h).max(46.0);
            egui::ScrollArea::vertical().id_salt("side games").auto_shrink(false).max_height(room).show(ui, |ui| {
                for g in &games {
                    let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 46.0), egui::Sense::click());
                    let open = self.tab == Tab::Clips && self.view == ClipsView::Game(g.name.clone());
                    if open || r.hovered() {
                        ui.painter().rect_filled(rect, R_SMALL, pal().card);
                    }
                    let pic = egui::Rect::from_min_size(rect.left_top() + egui::vec2(8.0, 6.0), egui::vec2(34.0, 34.0));
                    match self.game_picture(ui.ctx(), g) {
                        Some(tex) => paint_fill(ui, &tex, pic, R_SMALL),
                        None => _ = ui.painter().rect_filled(pic, R_SMALL, pal().raised),
                    }
                    let x = pic.right() + 10.0;
                    let name = g.name.as_deref().unwrap_or("Not sorted");
                    // One line, cut with "…", so a long name never runs into the clip count.
                    let mut job = egui::text::LayoutJob::simple_singleline(name.into(), egui::FontId::new(13.0, egui::FontFamily::Name("strong".into())), pal().text);
                    job.wrap = egui::text::TextWrapping::truncate_at_width(rect.right() - x - 4.0);
                    let galley = ui.painter().layout_job(job);
                    let cut = galley.elided;
                    ui.painter().galley(egui::pos2(x, rect.top() + 6.0), galley, pal().text);
                    let what = count(g.count, "clip");
                    ui.painter().text(egui::pos2(x, rect.bottom() - 7.0), egui::Align2::LEFT_BOTTOM, what, egui::FontId::proportional(12.0), pal().muted);
                    let tip = if cut { format!("{name}: shows this game's clips") } else { "Shows this game's clips".into() };
                    if r.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        self.tab = Tab::Clips;
                        self.player = None;
                        self.view = ClipsView::Game(g.name.clone());
                    }
                }
            });
        }
        ui.add_space((ui.available_height() - status_h).max(0.0));
        self.side_status(ui);
    }

    /// Across the top of every page: the clip search and Save clip.
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // Narrower in a small window, so Save clip still fits beside it.
            let width = (ui.available_width() - 140.0).clamp(140.0, 300.0);
            let search = egui::TextEdit::singleline(&mut self.search).hint_text("Search clips").desired_width(width).min_size(egui::vec2(0.0, H_BIG)).margin(egui::vec2(12.0, 8.0));
            if ui.add(search).on_hover_text("Type part of a clip's name, game, tag or note to find it").changed() && !self.search.trim().is_empty() {
                self.tab = Tab::Clips;
            }
            if primary(ui, self.buffer, strong("Save clip", 14.0), egui::Vec2::ZERO)
                .on_hover_text(format!("Saves the last {}. Same as pressing {}.", mmss(self.saved.buffer_secs), self.saved.hotkey))
                .on_disabled_hover_text(if self.connected { "Instant replay is off: turn it on with the switch at the bottom left" } else { "The recorder isn't running yet" })
                .clicked()
            {
                let _ = self.cmds.send(Cmd::Save);
            }
        });
        ui.add_space(18.0);
    }

    /// "More options" under a page's main cards: a row that opens or closes the rest of the page
    /// (`body`), left open or closed per page (a view choice, kept right away without Save).
    fn more_options(&mut self, ui: &mut egui::Ui, page: Tab, body: impl FnOnce(&mut Self, &mut egui::Ui)) {
        let open = self.cfg.more_open.iter().any(|p| p == page.name());
        let button = icon_button(if open { icon::DOWN } else { icon::RIGHT }, "More options").min_size(egui::vec2(0.0, H));
        let r = cap(ui, true, button).on_hover_text(if open { "Hides the settings most people never change" } else { "Shows every other setting on this page" });
        ui.add_space(12.0);
        if r.clicked() {
            self.set_more_open(page, !open);
        }
        if open {
            body(self, ui);
        }
    }

    /// Opens or closes a page's More options, kept right away.
    fn set_more_open(&mut self, page: Tab, open: bool) {
        self.keep_view(|c| {
            c.more_open.retain(|p| p != page.name());
            if open {
                c.more_open.push(page.name().to_string());
            }
        });
    }

    /// The bottom of the sidebar: the replay switch and a status light (REC, PAUSE or NO SIGNAL)
    /// in the on-screen display font, with one line under it: what a save keeps while recording,
    /// otherwise why it isn't.
    fn side_status(&mut self, ui: &mut egui::Ui) {
        if self.saved.check_updates
            && let Some(n) = app_update::found()
        {
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                if ui.link(RichText::new(format!("Version {} is out", n.version)).small()).on_hover_text("Shows what's new and lets you install it").clicked() {
                    self.update_later = u32::MAX;
                    self.update_error = None;
                }
            });
            ui.add_space(8.0);
        }
        let (dot, color, text) = match (self.connected, self.buffer) {
            (true, true) => (RED, pal().phosphor, "REC"),
            (true, false) => (pal().amber, pal().amber, "PAUSE"),
            _ => (RED, pal().red, "NO SIGNAL"),
        };
        ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), egui::Stroke::new(1.0, pal().border));
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            let mut on = self.buffer;
            if ui.add_enabled(self.connected, toggle(&mut on)).on_hover_text("Turn instant replay on or off").changed() {
                let _ = self.cmds.send(Cmd::SetReplay(on));
            }
            ui.add_space(4.0);
            let mut job = egui::text::LayoutJob::default();
            job.append("\u{25CF} ", 0.0, egui::TextFormat { font_id: egui::FontId::proportional(12.0), color: dot, valign: egui::Align::Center, ..Default::default() });
            let font = if retro() { egui::FontId::new(21.0, egui::FontFamily::Name("osd".into())) } else { egui::FontId::new(15.0, egui::FontFamily::Name("strong".into())) };
            job.append(text, 0.0, egui::TextFormat { font_id: font, color, valign: egui::Align::Center, ..Default::default() });
            ui.label(job).on_hover_text(&self.status);
        });
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            if self.connected && self.buffer {
                let what = format!("Last {} \u{B7} {} saves", mmss(self.saved.buffer_secs), self.saved.hotkey);
                ui.add(egui::Label::new(RichText::new(what).small().color(pal().muted)).truncate())
                    .on_hover_text(format!("Press {} anywhere, even in a game, to save the last {}.", self.saved.hotkey, mmss(self.saved.buffer_secs)));
            } else {
                // Why it isn't recording: on up to two lines (the sidebar keeps room for them),
                // all of it on hover.
                let small = egui::TextStyle::Small.resolve(ui.style());
                let paused = self.connected && autopause::PAUSED.load(std::sync::atomic::Ordering::Relaxed);
                let status = if paused { "Paused: no game open. Starts again when one is.".to_string() } else { self.status.clone() };
                let mut why = egui::text::LayoutJob::simple(status, small, pal().muted, ui.available_width());
                why.wrap.max_rows = 2;
                ui.add(egui::Label::new(why).wrap()).on_hover_text(&self.status);
            }
        });
        // A key another program took since the keys were set (Keybinds says who and what to do),
        // so it isn't first noticed when a save does nothing.
        self.check_conflicts();
        if let Some((target, text)) = self.conflicts.first().cloned() {
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                let more = if self.conflicts.len() > 1 { format!(" (+{} more)", self.conflicts.len() - 1) } else { String::new() };
                let line = egui::Label::new(RichText::new(format!("Key clash: {}{more}", target.name())).small().color(pal().amber)).truncate().sense(egui::Sense::click());
                if ui.add(line).on_hover_text(format!("{}: {text}\nClick to open Keybinds.", target.name())).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    self.tab = Tab::Keybinds;
                }
            });
        }
        ui.add_space(14.0);
    }

    /// One page link in the sidebar. Retro: a tape on the site's shelf, a dark spine with a
    /// handwritten paper label and a colored stripe; the open page's tape is pulled out a little and
    /// outlined in its color. Minimalist: a quiet list row, the open page's marked with an accent bar.
    fn nav_item(&mut self, ui: &mut egui::Ui, tab: Tab) {
        let (name, glyph, _, hint) = tab.info();
        let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), if retro() { 40.0 } else { 36.0 }), egui::Sense::click());
        let selected = self.tab == tab;
        // Painted below, so say what it is for screen readers (and tests that find it by name).
        r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, selected, name));
        let painter = ui.ctx().layer_painter(ui.layer_id());
        if retro() {
            let stripe = tab.stripe();
            let pull = if selected { 10.0 } else if r.hovered() { 4.0 } else { 0.0 };
            let spine = egui::Rect::from_min_max(rect.min, egui::pos2(rect.right() - 10.0 + pull, rect.bottom())).shrink2(egui::vec2(0.0, 2.0));
            // In light mode the spines are a soft lilac a shade under the shelf, not black, so they
            // don't sit like holes in it or ring the labels in heavy outlines.
            let spine_fill = if pal().light { pal().edge } else if selected { Color32::BLACK } else { Color32::from_rgb(8, 6, 11) };
            painter.rect_filled(spine, 4.0, spine_fill);
            if selected {
                painter.rect_stroke(spine, 4.0, egui::Stroke::new(2.0, stripe), egui::StrokeKind::Inside);
            }
            let label = spine.shrink2(egui::vec2(6.0, 5.0));
            painter.rect_filled(label, 3.0, if selected { PAPER } else { PAPER.lerp_to_gamma(pal().sidebar, 0.25) });
            painter.rect_filled(label.with_max_x(label.left() + 6.0), egui::CornerRadius { nw: 3, sw: 3, ne: 0, se: 0 }, stripe);
            let mut x = label.left() + 14.0;
            if paint_icon(&painter, egui::pos2(x, label.center().y), egui::Align2::LEFT_CENTER, glyph, 13.0, INK) {
                x += 22.0;
            }
            let font = egui::FontId::new(15.0, egui::FontFamily::Name("marker".into()));
            painter.text(egui::pos2(x, label.center().y), egui::Align2::LEFT_CENTER, name, font, INK);
        } else {
            // A quiet list row: the open page on a raised surface with an accent bar, the others
            // bare until hovered.
            if selected || r.hovered() {
                painter.rect_filled(rect, R_SMALL, if selected { pal().raised } else { pal().card });
            }
            if selected {
                let bar = egui::Rect::from_min_size(rect.left_center() - egui::vec2(0.0, 8.0), egui::vec2(3.0, 16.0));
                painter.rect_filled(bar, 2.0, accent());
            }
            let mut x = rect.left() + 12.0;
            if paint_icon(&painter, egui::pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, glyph, 14.0, if selected { pal().text } else { pal().muted }) {
                x += 26.0;
            }
            painter.text(egui::pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, name, egui::FontId::proportional(14.0), if selected { pal().text } else { pal().muted.lerp_to_gamma(pal().text, 0.5) });
        }
        if r.on_hover_text(hint).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            self.tab = tab;
        }
    }

    /// Picks up what finished in the background (exports and the next one waiting, uploads, the
    /// montage, a trim or GIF), whatever page is open.
    fn pump_jobs(&mut self, ctx: &egui::Context) {
        self.finish_uploads(ctx);
        self.finish_exports(ctx);
        self.finish_montage();
        self.finish_trim();
    }

    /// A dialog is up: rename, new tag, recycle, upload, Discord, import, Remove or a new version.
    fn dialog_open(&self) -> bool {
        self.asked_dialog_open() || self.update_due()
    }

    /// One of the dialogs the user opened is up (all but the new-version popup).
    fn asked_dialog_open(&self) -> bool {
        self.confirm_upload.is_some() || self.confirm_discord.is_some() || self.confirm_remove || self.renaming.is_some() || self.tagging.is_some() || self.import.is_some() || self.confirm_recycle.is_some()
    }

    /// The new-version popup is up: a newer version was found and not put off with Later, or it's
    /// being installed. It waits while a recording runs or setup is open.
    fn update_due(&self) -> bool {
        app_update::found().is_some()
            && (self.update_job.is_some() || self.update_error.is_some() || (app_update::finds() != self.update_later && !self.recording && self.wizard.is_none()))
    }

    /// The new-version popup: what's new, then Install now and Later (the download page only if
    /// the release has nothing this copy can install). While updating it shows the download, then
    /// quits for the installer, or (portable) swaps the exe and restarts.
    fn update_dialog(&mut self, ctx: &egui::Context, n: &app_update::Newer) {
        let installer = app_update::pick(n);
        let progress = self.update_job.as_ref().map(|j| j.lock().unwrap().clone());
        if let Some(r) = progress.as_ref().and_then(|p| p.result.clone()) {
            self.update_job = None;
            match r.and_then(|()| app_update::apply().map_err(|e| format!("couldn't put the new version in place: {e}"))) {
                Ok(()) => {
                    log::write(&format!("Installing Replay Vault {}; closing for the installer.", n.version));
                    shutdown();
                    std::process::exit(0);
                }
                Err(e) => {
                    log::write(&format!("Couldn't update to Replay Vault {}: {e}", n.version));
                    self.update_error = Some(e);
                }
            }
            return;
        }
        if progress.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        let mut later = false;
        let r = egui::Modal::new(egui::Id::unique("update_popup")).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.label(RichText::new(format!("Replay Vault {} is out", n.version)).strong().size(16.0));
            ui.label(RichText::new(format!("You have {}. Your settings and clips stay.", env!("CARGO_PKG_VERSION"))).color(pal().muted));
            if !n.notes.is_empty() {
                ui.add_space(4.0);
                egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| ui.label(&n.notes));
            }
            ui.add_space(8.0);
            if let Some(p) = &progress {
                ui.label(format!("Downloading: {} of {}", clips::size(p.done), clips::size(p.total)));
                ui.add(egui::ProgressBar::new(if p.total > 0 { p.done as f32 / p.total as f32 } else { 0.0 }));
                return;
            }
            if let Some(e) = &self.update_error {
                ui.label(RichText::new(format!("Couldn't update: {e}")).color(RED));
            }
            let install = installer.clone().filter(|_| self.update_error.is_none());
            ui.horizontal(|ui| {
                let (label, hint) = match install {
                    Some(_) => ("Update now", "Downloads it, checks it and puts it in place of this copy, then opens the new version. Settings and clips stay."),
                    None => ("Open download page", "Opens the release on GitHub, to download it and install it over this copy"),
                };
                if primary(ui, !self.recording, RichText::new(label).strong(), egui::Vec2::ZERO).on_hover_text(hint).on_disabled_hover_text("Stop the recording first").clicked() {
                    match install {
                        Some(i) => {
                            let c = ui.ctx().clone();
                            self.update_job = Some(app_update::download(i, move || c.request_repaint()));
                        }
                        None => {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(&n.url));
                            later = true;
                        }
                    }
                }
                if button(ui, "Update later").on_hover_text("Closes this. The sidebar keeps a link to it, and it asks again at the next check (once a day).").clicked() {
                    later = true;
                }
            });
        });
        if later || (r.should_close() && progress.is_none()) {
            self.update_later = app_update::finds();
            self.update_error = None;
        }
    }

    /// The dialog that's up, if any (see `dialog_open`). Each is taken out of its field while
    /// drawn and put back while it stays open.
    fn dialogs(&mut self, ctx: &egui::Context) {
        const CANCEL: &str = "Closes this without changing anything";
        if let Some((path, mut name)) = self.renaming.take() {
            let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
            let confirm = Confirm { label: "Rename", hint: "Renames the file", danger: false };
            match modal(ctx, "rename_clip", 380.0, "Rename clip", confirm, CANCEL, |ui| {
                let edit = ui.horizontal(|ui| {
                    let edit = ui.add(egui::TextEdit::singleline(&mut name).desired_width(300.0)).on_hover_text("The clip's new name");
                    ui.label(RichText::new(&ext).color(pal().muted));
                    edit
                });
                (!name.trim().is_empty(), focus_enter(ui, &edit.inner))
            }) {
                Some(true) => self.rename_clip(&path, &name),
                Some(false) => {}
                None => self.renaming = Some((path, name)),
            }
        }
        if let Some((paths, mut name)) = self.tagging.take() {
            let hint = if paths.len() > 1 { "Makes the tag and gives it to the clips picked" } else { "Makes the tag and gives it to the clip" };
            let confirm = Confirm { label: "Add", hint, danger: false };
            match modal(ctx, "new_tag", 320.0, "New tag", confirm, CANCEL, |ui| {
                let edit = ui.add(egui::TextEdit::singleline(&mut name).hint_text("Aces, Funny, ...").desired_width(280.0)).on_hover_text("The new tag's name");
                (!name.trim().is_empty(), focus_enter(ui, &edit))
            }) {
                Some(true) => {
                    self.tag(&paths, &name, true);
                }
                Some(false) => {}
                None => self.tagging = Some((paths, name)),
            }
        }
        if let Some(paths) = self.confirm_recycle.take() {
            let confirm = Confirm { label: "Move to Recycle Bin", hint: "Moves every clip picked to the Recycle Bin", danger: true };
            match modal(ctx, "recycle_picked", 360.0, &format!("Move {} to the Recycle Bin?", count(paths.len(), "clip")), confirm, CANCEL, |ui| {
                ui.label("You can get them back from the Recycle Bin.");
                (true, false)
            }) {
                Some(true) => self.recycle_picked(&paths),
                Some(false) => {}
                None => self.confirm_recycle = Some(paths),
            }
        }
        if let Some(path) = self.confirm_upload.take() {
            let confirm = Confirm { label: "Upload", hint: "Uploads the clip and copies its link", danger: false };
            match modal(ctx, "upload_notice", 360.0, "Upload for a share link?", confirm, CANCEL, |ui| {
                ui.label("The clip goes to kappa.lol, a free third-party host. Anyone with the link can watch it. You can delete it from there later with Delete link. Max 100 MB.");
                (true, false)
            }) {
                Some(true) => {
                    let c = ctx.clone();
                    self.uploads.insert(path.clone(), upload::start(&path, move || c.request_repaint()));
                }
                Some(false) => {}
                None => self.confirm_upload = Some(path),
            }
        }
        if let Some(path) = self.confirm_discord.take() {
            let confirm = Confirm { label: "Post", hint: "Sends the clip to the webhook's channel", danger: false };
            match modal(ctx, "discord_confirm", 380.0, "Post to Discord?", confirm, "Closes this without sending anything", |ui| {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                ui.label(RichText::new(format!("{name}  ({})", clips::size(bytes))).strong());
                ui.label("It goes to the channel of the webhook set on the General page, where everyone in that channel can see it. A Discord post can't be taken back from here; delete it in Discord.");
                let fits = bytes <= upload::DISCORD_MAX_BYTES;
                if !fits {
                    ui.label(RichText::new("Discord takes up to 10 MB here. Use Export for sharing > Discord 10 MB, then post the copy.").color(pal().amber));
                }
                (fits, false)
            }) {
                Some(true) => {
                    let c = ctx.clone();
                    self.uploads.insert(path.clone(), upload::post_discord(&path, &self.cfg.discord_webhook, move || c.request_repaint()));
                }
                Some(false) => {}
                None => self.confirm_discord = Some(path),
            }
        }
        if let Some(file) = self.import.take() {
            let new = file.applied_to(&self.cfg);
            let shown = config::shown_changes(&self.cfg, &new);
            let changed = config::changes(&self.cfg, &new);
            let warnings: Vec<String> = [&new.folder, &new.archive_folder]
                .into_iter()
                .filter_map(|f| config::folder_warning(f, config::this_pc_drive))
                .collect();
            let stars = file.favorites.difference(&self.favorites).count();
            let notes = file.notes.keys().filter(|k| !self.notes.contains_key(*k)).count();
            let confirm = Confirm { label: "Import", hint: "Adds the stars and tags and fills in these settings; Save to use them", danger: false };
            match modal(ctx, "import_settings", 460.0, "Import settings?", confirm, CANCEL, |ui| {
                if !shown.is_empty() {
                    egui::Grid::new("import_values").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
                        for (name, from, to) in &shown {
                            ui.label(RichText::new(*name).color(pal().muted));
                            ui.add(egui::Label::new(format!("{from}  ->  {to}")).wrap());
                            ui.end_row();
                        }
                    });
                }
                if !changed.is_empty() {
                    ui.label(format!("Also changes: {}.", changed.join(", ")));
                } else if shown.is_empty() {
                    ui.label("No settings change.");
                }
                if notes > 0 {
                    ui.label(format!("Adds {}, tags on {} and {}.", count(stars, "star"), count(file.tags.len(), "clip"), count(notes, "note")));
                } else if stars > 0 || !file.tags.is_empty() {
                    ui.label(format!("Adds {} and tags on {}.", count(stars, "star"), count(file.tags.len(), "clip")));
                }
                for w in &warnings {
                    ui.label(RichText::new(format!("Check this: {w}")).color(pal().amber));
                }
                note(ui, "Nothing is changed until you click Import; the settings then apply when you Save. Passwords and the Discord webhook stay as they are here.");
                (true, false)
            }) {
                Some(true) => {
                    self.cfg = new;
                    self.favorites.extend(file.favorites);
                    clips::save_favorites(&self.favorites);
                    for (path, tags) in file.tags {
                        self.tags.entry(path).or_default().extend(tags);
                    }
                    clips::save_tags(&self.tags);
                    // A clip that has a note here keeps it.
                    for (path, note) in file.notes {
                        self.notes.entry(path).or_insert(note);
                    }
                    clips::save_notes(&self.notes);
                    // Only now, and only on this PC's own drives, are the folders looked for.
                    let missing = config::missing_folders(&self.cfg);
                    if !missing.is_empty() {
                        self.status = format!("{} isn't on this PC. Pick another folder, or create it, before you Save.", missing.join(", "));
                    }
                }
                Some(false) => {}
                None => self.import = Some(file),
            }
        }
        if self.confirm_remove {
            let confirm = Confirm { label: "Remove", hint: "Puts this PC back and uninstalls the app. Your clips stay.", danger: true };
            match modal(ctx, "remove_confirm", 380.0, "Remove Replay Vault from this PC?", confirm, CANCEL, |ui| {
                ui.label("This stops recording and puts back everything the app changed: Start with Windows, Game Bar and graphics settings, and the firewall rule (Windows asks first). The private OBS copy and the app's settings go to the Recycle Bin, then the app uninstalls itself along with its shortcuts. Your clips and your own OBS stay as they are.");
                (true, false)
            }) {
                Some(true) => self.remove_everything(),
                Some(false) => self.confirm_remove = false,
                None => {}
            }
        }
        if !self.asked_dialog_open()
            && self.update_due()
            && let Some(n) = app_update::found()
        {
            self.update_dialog(ctx, &n);
        }
    }

    /// Undoes everything the app did to this PC, then quits. Clips are kept.
    fn remove_everything(&mut self) {
        self.player = None;
        recorder::stop();
        tweaks::give_back_nvidia_overlay();
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
        problems.extend(obs_install::recycle_install_root().into_iter().map(|p| format!("Private OBS: {p}")));
        if let Err(e) = clips::recycle(&config::dir()) {
            problems.push(format!("Settings at {}: {e:#}", config::dir().display()));
        }
        if let Err(e) = uninstall_app() {
            problems.push(format!("The app itself: {e:#}"));
        }
        if !problems.is_empty() {
            // Left on the Desktop, since the settings folder is gone.
            let note = dirs::desktop_dir().unwrap_or_default().join("Replay Vault - remove notes.txt");
            let _ = std::fs::write(&note, format!("Removed, except:\r\n{}\r\n", problems.join("\r\n")));
        }
        std::process::exit(0);
    }
}

/// Takes the app itself off the PC once it has quit. An installed copy runs its uninstaller,
/// which also takes away the Start menu and desktop shortcuts. A portable copy deletes its exe
/// and notices a moment after it quits. A dev build is left alone.
fn uninstall_app() -> anyhow::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| anyhow::anyhow!("no folder"))?;
    if app_update::installed() {
        std::process::Command::new(dir.join("unins000.exe")).args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"]).spawn()?;
    } else if !DEV_BUILD {
        let files: Vec<String> = [exe.clone(), dir.join("LICENSE"), dir.join("NOTICE"), dir.join("THIRD-PARTY-NOTICES.md"), dir.join("README.md"), dir.join("debug.log")]
            .iter()
            .map(|p| format!("\"{}\"", p.display()))
            .collect();
        // Waits about 2 s for this process to end, so its exe can be deleted. rd only removes
        // the folder if nothing else is in it.
        std::process::Command::new("cmd")
            .raw_arg(format!("/c ping -n 3 127.0.0.1 >nul & del /f /q {} & rd \"{}\"", files.join(" "), dir.display()))
            .current_dir(std::env::temp_dir())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
    }
    Ok(())
}

impl App {
    /// A one-line hint at the top of a page, until its x is clicked. Kept right away, no Save.
    fn tip(&mut self, ui: &mut egui::Ui) {
        let page = self.tab.name();
        if !self.cfg.show_tips || self.cfg.tips_seen.iter().any(|p| p == page) {
            return;
        }
        let text = match self.tab {
            Tab::Clips => format!("Press {} in a game to save the last {}. Click a card to play it.", self.saved.hotkey, mmss(self.saved.buffer_secs)),
            Tab::General => "Changes here wait for the Save button at the bottom.".into(),
            Tab::Keybinds => "Click a key, then press the keys you want. They work even in a game.".into(),
            Tab::Video => "Not sure what to pick? Use recommended settings sets it up for this PC.".into(),
            Tab::Audio => "Keep your mic on its own track to change its volume when you trim.".into(),
            Tab::Help => "If clips stop saving, the status here says why. Copy it or make a support zip to ask for help.".into(),
            Tab::Feedback => "For a bug, say what you did and what happened. Leaving the PC summary ticked helps a lot.".into(),
            Tab::WhatsNew => "Each update lists what's new, what got fixed and what changed.".into(),
        };
        let mut close = false;
        egui::Frame::new().fill(pal().card.lerp_to_gamma(accent(), 0.1)).corner_radius(R_CARD).inner_margin(egui::Margin::symmetric(12, 6)).show(ui, |ui| {
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
            self.keep_view(|c| c.tips_seen.push(page.to_string()));
        }
    }

    /// The bar that appears while settings are changed but not saved.
    fn footer(&mut self, ui: &mut egui::Ui) {
        let restart = recorder_settings(&self.cfg) != self.applied;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(strong("Unsaved changes", 14.0).color(pal().text));
                let note = if restart {
                    "Saving restarts the recorder, so what it kept so far is cleared."
                } else {
                    "Save keeps them for next time. Discard goes back to what was saved."
                };
                if tips_on() {
                    ui.label(RichText::new(note).small().color(pal().muted));
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if primary(ui, true, strong("Save", 14.0), egui::Vec2::ZERO).on_hover_text("Keeps your changes").clicked() {
                    self.keep_settings();
                }
                if cap(ui, true, egui::Button::new("Discard").min_size(egui::vec2(90.0, H_BIG))).on_hover_text("Undoes your changes, back to what was saved").clicked() {
                    self.discard_settings();
                }
            });
        });
    }
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

/// Whether this start is the first one after an update, so What's new opens: the version that last
/// ran differs. Versions before 0.2.0 didn't keep it, so an empty one counts once setup was done
/// (a new user has nothing to compare with and starts in setup).
fn updated_app(last_version: &str, set_up: bool) -> bool {
    if last_version.is_empty() { set_up } else { last_version != env!("CARGO_PKG_VERSION") }
}

/// How often a hotkey another app held is tried again.
const HOTKEY_RETRY: std::time::Duration = std::time::Duration::from_secs(3);

/// Don't leave the hidden recorder running after the app closes, or NVIDIA's overlay off.
fn shutdown() {
    obs::end_session("app closed");
    log::write("App closed.");
    recorder::stop();
    tweaks::give_back_nvidia_overlay();
}

/// "1 clip", "2 clips": `n` and `one`, made plural with an s past 1.
fn count(n: usize, one: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {one}s") }
}

/// Seconds as m:ss.
fn mmss(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

impl eframe::App for App {
    fn on_exit(&mut self) {
        overlay::save_placement(self.tab.name());
        shutdown();
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        if let Some(pos) = self.drag_out_ended.take() {
            raw.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        }
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
        self.pump_jobs(ui.ctx());
        // The window's place and page, for next start (the tray's Quit skips on_exit).
        if self.placement_checked.elapsed() >= std::time::Duration::from_secs(1) {
            self.placement_checked = std::time::Instant::now();
            overlay::save_placement(self.tab.name());
        }
        // A new version comes to the user: from the tray the window opens with its popup, once per
        // find, but never over a game or mid-typing (it waits until none is in front and the PC
        // has had no input for a minute, which also covers a windowed game it can't tell apart).
        if self.update_due() && self.update_job.is_none() && self.update_pushed != app_update::finds() && overlay::is_hidden() {
            if autopause::game_in_front(std::path::Path::new(&self.saved.folder)) || autopause::idle() < std::time::Duration::from_secs(60) {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs(60));
            } else {
                self.update_pushed = app_update::finds();
                overlay::show();
            }
        }
        self.finish_capture();
        self.sync_hotkeys();
        // Keeps trying a key another app held, even from the tray.
        if !self.hotkey_failed.is_empty() {
            ui.ctx().request_repaint_after(HOTKEY_RETRY);
        }
        // Setup has no Save button: what's picked there is kept as it goes.
        if self.wizard.is_some() && self.cfg != self.saved {
            self.keep_settings();
        }
        let moved = clips::take_moved();
        if !moved.is_empty() {
            for (old, new) in &moved {
                self.clip_moved(old, new.as_deref());
            }
            // Saved even if nothing changed here, over anything the threads wrote from a stale read.
            clips::save_favorites(&self.favorites);
            clips::save_tags(&self.tags);
            clips::save_notes(&self.notes);
        }
        NOTES.store(self.cfg.show_tips, std::sync::atomic::Ordering::Relaxed);
        app_update::ENABLED.store(self.saved.check_updates, std::sync::atomic::Ordering::Relaxed);
        autopause::ENABLED.store(self.saved.pause_without_game, std::sync::atomic::Ordering::Relaxed);
        // The look previews as it's picked, before Save. Kept per window (egui context), so a new
        // one gets it on its first frame too.
        let custom = parse_hex(&self.cfg.custom_accent).map_or(0x8448F0, |c| u32::from_be_bytes([0, c.r(), c.g(), c.b()]));
        let look = (self.cfg.accent, custom, self.cfg.theme == "retro", self.cfg.light_mode);
        ACCENT.store(look.0, std::sync::atomic::Ordering::Relaxed);
        CUSTOM.store(look.1, std::sync::atomic::Ordering::Relaxed);
        RETRO.store(look.2, std::sync::atomic::Ordering::Relaxed);
        LIGHT.store(look.3, std::sync::atomic::Ordering::Relaxed);
        EFFECTS.store(look.2 && !self.cfg.reduce_effects, std::sync::atomic::Ordering::Relaxed);
        let id = egui::Id::NULL.with("applied look");
        if ui.ctx().data(|d| d.get_temp(id)) != Some(look) {
            ui.ctx().data_mut(|d| d.insert_temp(id, look));
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
        // The Esc that just cancelled a key capture.
        let esc = esc && !self.capture_ended.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(500));
        let in_setup = self.wizard.is_some();
        if close && !in_setup {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        // On the Clips page, Esc first lets go of the clips picked.
        let unpick = esc && !typing && self.capturing.is_none() && self.tab == Tab::Clips && !self.picked.is_empty() && !self.dialog_open();
        if unpick {
            self.picked.clear();
        }
        if !in_setup && (close || (esc && !unpick && self.capturing.is_none() && !typing)) {
            overlay::close();
        }

        let bar = egui::Frame::new().fill(pal().sidebar).inner_margin(egui::Margin { left: 14, right: 14, top: 12, bottom: 0 });
        if let Some(step) = self.wizard {
            // One centered column, like the settings pages, however wide the window is.
            const W: f32 = 720.0;
            egui::Panel::top("wizard_header").frame(bar.inner_margin(egui::Margin::symmetric(16, 14))).resizable(false).show(ui, |ui| {
                column(ui, W, |ui| self.wizard_header(ui, step));
            });
            egui::Panel::bottom("wizard_footer").frame(egui::Frame::new().fill(pal().sidebar).inner_margin(egui::Margin::symmetric(16, 10))).resizable(false).show(ui, |ui| {
                column(ui, W, |ui| {
                    self.wizard_footer(ui, step);
                    ui.add_space(4.0);
                    ui.add(egui::Label::new(RichText::new(&self.status).small().color(pal().muted)).truncate());
                });
            });
            egui::CentralPanel::default().frame(egui::Frame::new().fill(pal().bg).inner_margin(egui::Margin::symmetric(16, 20))).show(ui, |ui| {
                room(ui, ui.max_rect().expand2(egui::vec2(16.0, 20.0)));
                egui::ScrollArea::vertical().id_salt(("wizard", step)).auto_shrink(false).show(ui, |ui| column(ui, W, |ui| self.wizard_body(ui, step)));
            });
            focus_ring(ui.ctx());
            return;
        }
        // Re-read every few seconds: the Clips page and the sidebar's games both show it.
        if self.clips_checked.elapsed() > std::time::Duration::from_secs(3) {
            self.clips_checked = std::time::Instant::now();
            self.clip_list = clips::list(&self.saved.folder, usize::MAX);
            self.refind_marked();
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(3));
        let nav = egui::Frame::new().fill(pal().sidebar).inner_margin(egui::Margin { left: 12, right: 12, top: 0, bottom: 0 });
        egui::Panel::left("nav").frame(nav).exact_size(220.0).resizable(false).show(ui, |ui| self.sidebar(ui));
        // Only while there are settings to save (or waiting for a recorder restart).
        if self.cfg != self.saved || recorder_settings(&self.cfg) != self.applied {
            let frame = egui::Frame::new().fill(pal().card).stroke(egui::Stroke::new(1.0, pal().border)).inner_margin(egui::Margin::symmetric(24, 12));
            egui::Panel::bottom("apply").frame(frame).resizable(false).show(ui, |ui| self.footer(ui));
        }
        // The player only lives on the Clips tab, and pauses before the window hides.
        if self.tab != Tab::Clips {
            self.player = None;
        }
        if self.tab != Tab::Help {
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
        let page = egui::Frame::new().fill(pal().bg).inner_margin(egui::Margin { left: 28, right: 28, top: 22, bottom: 12 });
        egui::CentralPanel::default().frame(page).show(ui, |ui| {
            room(ui, egui::Rect::from_min_max(ui.max_rect().min - egui::vec2(28.0, 22.0), ui.max_rect().max + egui::vec2(28.0, 12.0)));
            // An open clip takes the whole page: the video is a child window, so it can't scroll.
            if self.tab == Tab::Clips && self.player.is_some() {
                self.player_page(ui);
                return;
            }
            self.top_bar(ui);
            if self.tab == Tab::Clips {
                self.clips_header(ui);
            } else {
                // Same width as the settings column below.
                ui.scope(|ui| {
                    ui.set_max_width(ui.available_width().min(760.0));
                    ui.horizontal(|ui| {
                        retro_title(ui, self.tab.name(), 38.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if let Some(page) = self.tab.page()
                                && button(ui, "Restore defaults").on_hover_text("Puts this page back the way a fresh install sets it up on this PC. Nothing changes until you Save.").clicked()
                            {
                                self.restore_defaults(page);
                            }
                        });
                    });
                    if tips_on() {
                        ui.label(RichText::new(self.tab.info().2).color(pal().muted));
                    }
                    ui.add_space(14.0);
                });
            }
            ui.scope(|ui| {
                if self.tab != Tab::Clips {
                    ui.set_max_width(ui.available_width().min(760.0));
                }
                self.tip(ui);
            });
            egui::ScrollArea::vertical().id_salt(("tab", self.tab as u8)).auto_shrink(false).show(ui, |ui| {
                // Settings read best in a column, not stretched across a wide window.
                if self.tab != Tab::Clips {
                    ui.set_max_width(ui.available_width().min(760.0));
                }
                // Each page: what most people need first, the rest under More options.
                match self.tab {
                    Tab::Clips => self.clips_tab(ui, clip_keys),
                    Tab::General => self.general_tab(ui),
                    Tab::Keybinds => self.keybinds_tab(ui),
                    Tab::Video => self.video_tab(ui),
                    Tab::Audio => self.audio_tab(ui),
                    Tab::Help => self.help_tab(ui),
                    Tab::Feedback => self.feedback_card(ui),
                    Tab::WhatsNew => self.whats_new_tab(ui),
                }
            });
        });
        focus_ring(ui.ctx());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    mod demos;

    impl App {
        /// The app with the saved settings and nothing running (no recorder, tray, hotkeys or
        /// window), for rendering its screens in tests.
        fn preview(cc: &eframe::CreationContext) -> App {
            apply_theme(&cc.egui_ctx);
            let (cmds, _) = mpsc::channel();
            let (_, events) = mpsc::channel();
            let ctx = cc.egui_ctx.clone();
            let mut app = App {
                status: "Instant replay is on.".into(),
                connected: true,
                buffer: true,
                start_with_windows: true,
                ..App::base(Config::load(), cmds, events, profile::detect(), clips::Previews::new(move || ctx.request_repaint()))
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
        // SCREENS_OUT / SCREENS_FOLDER: render elsewhere, from another clips folder (for the website).
        let out = std::env::var("SCREENS_OUT").map(std::path::PathBuf::from).unwrap_or(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(r"target\screens"));
        std::fs::create_dir_all(&out).unwrap();
        let demo = demo_folder();
        let size: Vec<f32> = std::env::var("SCREEN_SIZE").unwrap_or("480x640".into()).split('x').map(|n| n.parse().unwrap()).collect();
        let tabs = [(Tab::Clips, "clips"), (Tab::General, "general"), (Tab::Keybinds, "keybinds"), (Tab::Video, "video"), (Tab::Audio, "audio"), (Tab::Feedback, "feedback"), (Tab::Help, "help"), (Tab::WhatsNew, "whats-new")];
        let pages = tabs
            .map(|(t, n)| (t, None, n.to_string()))
            .into_iter()
            .chain([(Tab::Video, None, "unsaved".to_string())])
            // Each page with More options open.
            .chain(tabs.into_iter().skip(1).map(|(t, n)| (t, None, format!("{n}-more"))))
            .chain([(Tab::General, None, "general-retro-light".to_string())])
            .chain([(Tab::Video, None, "video-untested".to_string()), (Tab::Audio, None, "audio-chat".to_string()), (Tab::Audio, None, "audio-custom".to_string())])
            .chain([(Tab::Clips, None, "clips-starred".to_string()), (Tab::General, None, "general-cleanup".to_string())])
            .chain([(Tab::Clips, None, "clips-search".to_string()), (Tab::Clips, None, "clips-rename".to_string())])
            .chain([(Tab::Clips, None, "clips-games".to_string()), (Tab::Clips, None, "clips-game".to_string())])
            .chain([(Tab::Clips, None, "clips-selected".to_string()), (Tab::Clips, None, "clips-scrub".to_string())])
            .chain([(Tab::Clips, None, "clips-export".to_string()), (Tab::Clips, None, "clips-export-menu".to_string())])
            .chain([(Tab::Clips, None, "clips-tags".to_string()), (Tab::Clips, None, "clips-tag-menu".to_string()), (Tab::Clips, None, "clips-new-tag".to_string())])
            .chain([(Tab::Clips, None, "clips-tag-filter".to_string())])
            .chain([(Tab::Clips, None, "clips-sort-menu".to_string()), (Tab::Clips, None, "clips-sorted".to_string())])
            .chain(["clips-picked", "clips-picked-tag-menu", "clips-picked-recycle"].map(|n| (Tab::Clips, None, n.to_string())))
            .chain(["clips-note", "clips-note-editing", "clips-note-search"].map(|n| (Tab::Clips, None, n.to_string())))
            .chain([(Tab::Clips, None, "clips-archive".to_string()), (Tab::Clips, None, "clips-archive-offline".to_string())])
            .chain([(Tab::Clips, None, "clips-montage".to_string()), (Tab::Clips, None, "clips-montage-making".to_string())])
            .chain([(Tab::General, None, "general-discord".to_string()), (Tab::General, None, "general-discord-bad".to_string()), (Tab::Clips, None, "clips-discord-menu".to_string())])
            .chain([(Tab::General, None, "general-archive".to_string())])
            .chain([(Tab::General, None, "general-move".to_string()), (Tab::General, None, "general-import".to_string()), (Tab::General, None, "general-import-network".to_string())])
            .chain([(Tab::Clips, None, "clips-discord-confirm".to_string())])
            .chain(tabs.map(|(t, n)| (t, None, format!("{n}-tip"))))
            .chain([(Tab::General, None, "general-accent".to_string()), (Tab::General, None, "general-no-tips".to_string()), (Tab::Clips, None, "clips-selected-accent".to_string())])
            .chain([(Tab::Video, None, "video-hint".to_string())])
            .chain(["keybinds-nvidia", "keybinds-nvidia-off", "keybinds-amd", "keybinds-amd-off", "keybinds-both"].map(|n| (Tab::Keybinds, None, n.to_string())))
            .chain(["feedback-draft", "feedback-sent", "feedback-error", "feedback-limit"].map(|n| (Tab::Feedback, None, n.to_string())))
            .chain([(Tab::Audio, None, "audio-apps".to_string()), (Tab::Video, None, "video-follow".to_string())])
            .chain([(Tab::Video, None, "video-hdr".to_string())])
            // Each theme in each mode, and a custom accent.
            .chain(["minimalist-dark", "minimalist-light", "retro-dark", "retro-light"].into_iter().flat_map(|look| [(Tab::General, None, format!("general-{look}")), (Tab::Clips, None, format!("clips-{look}"))]))
            .chain([(Tab::General, None, "general-custom-color".to_string()), (Tab::General, None, "general-custom-color-light".to_string())])
            .chain((0..STEPS.len()).map(|s| (Tab::Clips, Some(s), format!("setup{}", s + 1))));
        // Copies of the newest clips sorted into game folders, for the games pages; the clips
        // folder is only read.
        let games = out.join("games");
        for (i, c) in clips::list(&demo, 2).iter().enumerate() {
            for dir in [["Desktop", "VALORANT"][i], [".", "Elden Ring"][i]] {
                let to = games.join(dir).join(&c.name);
                if !to.exists() {
                    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
                    std::fs::copy(&c.path, &to).unwrap();
                }
            }
        }
        for (tab, wizard, name) in pages {
            let builder = egui_kittest::Harness::builder().with_max_steps(12).with_size([size[0], size[1]]).with_accessibility_check(false);
            let mut h = builder.wgpu().build_eframe(|cc| {
                let mut app = App::preview(cc);
                app.cfg.folder = demo.clone();
                app.saved.folder = demo.clone();
                // Screens for the website and README show the default keys, not this PC's own.
                if std::env::var_os("SCREENS_FOLDER").is_some() {
                    let d = Config::default();
                    for c in [&mut app.cfg, &mut app.saved] {
                        (c.hotkey, c.overlay_hotkey) = (d.hotkey.clone(), d.overlay_hotkey.clone());
                        (c.hotkey_record, c.hotkey_screenshot, c.hotkey_replay, c.hotkey_mic) = (d.hotkey_record.clone(), d.hotkey_screenshot.clone(), d.hotkey_replay.clone(), d.hotkey_mic.clone());
                    }
                    // And a made-up PC, not this one's GPU, monitors, sound devices or user folder.
                    (app.hw.gpu_name, app.hw.vram_mb, app.hw.ram_mb) = ("NVIDIA GeForce RTX 4070".into(), 12288, 32768);
                    app.monitors.iter_mut().for_each(|m| {
                        (m.id, m.name) = (String::new(), "Monitor".into());
                        (m.width, m.height, m.hz) = (2560, 1440, 165);
                    });
                    let (mut mics, mut outs) = (0, 0);
                    for dev in app.devices.iter_mut().filter(|d| !d.key.ends_with(":default")) {
                        let n = if dev.key.starts_with("in:") { mics += 1; mics } else { outs += 1; outs };
                        dev.name = match (dev.key.starts_with("in:"), n) {
                            (true, 1) => "Microphone (USB Audio)".into(),
                            (true, n) => format!("Microphone {n} (USB Audio)"),
                            (false, 1) => "Speakers (Realtek Audio)".into(),
                            (false, n) => format!("Headphones {n} (USB Audio)"),
                        };
                        for c in [&mut app.cfg, &mut app.saved] {
                            if let Some(e) = c.devices.get_mut(&dev.key) {
                                e.name = dev.name.clone();
                            }
                        }
                    }
                    obs_install::set_install_root(r"C:\Users\You\AppData\Local\replay-vault");
                }
                app.applied = recorder_settings(&app.cfg);
                app.tab = tab;
                app.wizard = wizard;
                // Minimalist and dark unless the name asks for Retro or light (in memory only).
                app.cfg.theme = if name.contains("-retro") { "retro" } else { "minimalist" }.into();
                app.cfg.light_mode = name.contains("-light");
                // SCREENS_LOOK=retro-light (or minimalist-dark, ...) renders every page in that look.
                if let Ok(look) = std::env::var("SCREENS_LOOK") {
                    app.cfg.theme = if look.contains("retro") { "retro" } else { "minimalist" }.into();
                    app.cfg.light_mode = look.contains("light");
                }
                if name.starts_with("general-custom-color") {
                    (app.cfg.accent, app.cfg.custom_accent) = (CUSTOM_ACCENT, if name.ends_with("-light") { "#8448F0" } else { "#1FA39A" }.into());
                }
                // More options open where the page's state is (a view choice; nothing is saved).
                let more = ["general-discord", "general-discord-bad", "general-archive", "general-move", "general-cleanup", "video-hint", "video-follow", "video-hdr", "audio-apps"];
                if name.ends_with("-more") || more.contains(&name.as_str()) {
                    app.cfg.more_open = ["General", "Keybinds", "Video", "Audio", "Help"].map(String::from).to_vec();
                    app.saved.more_open = app.cfg.more_open.clone();
                }
                // A graphics card nobody has tried yet, then game, mic and chat apart, then a custom
                // 5-track setup (in memory only).
                if name == "video-untested" {
                    (app.hw.gpu, app.hw.gpu_name, app.hw.vram_mb) = (recorder::Gpu::Amd, "AMD Radeon RX 7800 XT".into(), 16 * 1024);
                }
                if name.starts_with("audio-c") {
                    set_sound_preset(&mut app.cfg, if name == "audio-chat" { 2 } else { 1 });
                }
                if name == "audio-custom" {
                    (app.audio_custom, app.cfg.track_count) = (true, 5);
                    app.cfg.track_names[3] = "Music".into();
                }
                if name.starts_with("audio-c") {
                    app.saved = app.cfg.clone();
                    app.applied = recorder_settings(&app.cfg);
                }
                (app.saved.theme, app.saved.light_mode, app.saved.accent, app.saved.custom_accent) = (app.cfg.theme.clone(), app.cfg.light_mode, app.cfg.accent, app.cfg.custom_accent.clone());
                // A change waiting for Save, to show the bar.
                if name == "unsaved" {
                    app.cfg.fps = if app.cfg.fps == 60 { 120 } else { 60 };
                }
                // The skipped-frames note, as after a health check at Best quality.
                if name.starts_with("video-hint") {
                    (app.cfg.encoder_effort, app.saved.encoder_effort) = (0, 0);
                    app.encoder_hint = Some((412, 0));
                }
                // Green and blue accents (in memory only).
                if name.ends_with("-accent") {
                    app.cfg.accent = if name == "general-accent" { 2 } else { 1 };
                    app.saved.accent = app.cfg.accent;
                }
                if name == "general-no-tips" {
                    app.cfg.show_tips = false;
                    app.saved.show_tips = false;
                }
                // General opens on the tab the render is about.
                app.general_part = match name.as_str() {
                    n if n.starts_with("general-discord") => GeneralPart::Sharing,
                    "general-archive" | "general-cleanup" => GeneralPart::Storage,
                    n if n == "general-move" || n.starts_with("general-import") => GeneralPart::Backup,
                    n if n == "general-accent" || n == "general-no-tips" || n.starts_with("general-custom-color") => GeneralPart::Appearance,
                    _ => GeneralPart::Clips,
                };
                // NVIDIA's overlay on, then switched off but still running (only shown; nothing changed).
                if name == "keybinds-nvidia" {
                    (app.nvidia, app.amd) = (Some((true, true)), Some(None));
                }
                if name == "keybinds-nvidia-off" {
                    (app.cfg.nvidia_off, app.saved.nvidia_off, app.nvidia, app.amd) = (true, true, Some((false, true)), Some(None));
                }
                // Send feedback with a draft, then sent, then refused by the server (only shown;
                // nothing is posted).
                if name.ends_with("feedback-draft") {
                    (app.feedback.kind, app.feedback.contact) = (feedback::Kind::Idea, "flamme#0001".into());
                    app.feedback.message = "Could the clip-saved sound be a bit quieter? It's loud next to game audio.".into();
                }
                if name.ends_with("feedback-sent") {
                    (app.feedback.state, app.feedback.last_send) = (feedback::State::Sent, Some(std::time::Instant::now()));
                }
                // Two sent today (made up; nothing is read or written).
                if name.ends_with("feedback-limit") {
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
                    app.feedback.sends = vec![now - 600, now - 7200];
                    app.feedback.message = "One more idea: a key to save the last 10 seconds.".into();
                }
                if name.ends_with("feedback-error") {
                    app.feedback.message = "The save key stopped working after a Windows update.".into();
                    app.feedback.state = feedback::State::Error(feedback::refusal(429, r#"{"error":"That's a lot of feedback at once. Try again in a minute."}"#));
                }
                // AMD Software with Instant Replay on and the save key on both apps (made up; nothing read).
                if name.ends_with("keybinds-amd") {
                    app.nvidia = Some((false, false));
                    let save = hotkey::Combo::parse(&app.cfg.hotkey).unwrap();
                    app.amd = Some(Some(amd::Amd { replay_on: true, hotkeys_off: false, hotkeys: vec![("SaveInstantReplay".into(), save)] }));
                }
                // The same after ticking the AMD switch-off and saving (only shown; nothing written).
                if name.ends_with("keybinds-amd-off") {
                    app.nvidia = Some((false, false));
                    (app.cfg.amd_off, app.saved.amd_off) = (true, true);
                    app.amd = Some(Some(amd::Amd { replay_on: false, hotkeys_off: true, hotkeys: vec![] }));
                }
                // NVIDIA's overlay and AMD Software both on: one switch for both (made up; nothing read).
                if name == "keybinds-both" {
                    app.nvidia = Some((true, true));
                    app.amd = Some(Some(amd::Amd { replay_on: true, hotkeys_off: false, hotkeys: vec![] }));
                }
                // Setup's welcome with an OBS of their own found (the question only; nothing read).
                if name == "setup1" {
                    app.obs_settings_found = true;
                }
                // First-run tips only on the "-tip" pages.
                if !name.ends_with("-tip") {
                    app.cfg.tips_seen = ["Clips", "General", "Keybinds", "Video", "Audio", "Help"].map(String::from).to_vec();
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
                // A made-up webhook (only shown; nothing is posted), and a wrong link.
                if name == "general-discord" || name.starts_with("clips-discord") {
                    app.cfg.discord_webhook = "https://discord.com/api/webhooks/123/made-up-token".into();
                    app.saved = app.cfg.clone();
                }
                if name == "general-discord-bad" {
                    app.cfg.discord_webhook = "https://discord.com/channels/123".into();
                    app.saved = app.cfg.clone();
                }
                // The Archive view, on the games copies as the archive (one starred, one tagged; in
                // memory only), then with a drive letter that isn't on this PC (nothing is moved).
                if name.starts_with("clips-archive") {
                    let offline = (b'D'..=b'Z').rev().map(|l| format!(r"{}:\Clips Archive", l as char)).find(|d| !std::path::Path::new(&d[..3]).exists()).unwrap();
                    let dir = if name == "clips-archive" { games.clone() } else { offline.into() };
                    (app.cfg.archive_days, app.cfg.archive_folder) = (14, dir.display().to_string());
                    app.saved = app.cfg.clone();
                    app.view = ClipsView::Archive;
                    let mut shown: Vec<std::path::PathBuf> = clips::list(&dir.join("VALORANT").display().to_string(), 1).into_iter().chain(clips::list(&dir.join("Elden Ring").display().to_string(), 1)).map(|c| c.path).collect();
                    if name != "clips-archive" {
                        shown = vec![dir.join(r"VALORANT\VALORANT 2026-09-14 21-07-33.mp4"), dir.join(r"Elden Ring\Elden Ring 2026-09-02 23-41-10.mp4")];
                    }
                    if let [a, b] = &shown[..] {
                        app.favorites.insert(a.display().to_string());
                        clips::set_tag(&mut app.tags, a, "Aces", true);
                        clips::set_tag(&mut app.tags, b, "Funny", true);
                    }
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
                // Longest first in All, its menu open; then biggest first in Basic (in memory only;
                // config.json isn't written).
                if name == "clips-sort-menu" || name == "clips-sorted" {
                    app.view = ClipsView::All;
                    app.cfg.clip_sort = if name == "clips-sort-menu" { 2 } else { 3 };
                    app.saved.clip_sort = app.cfg.clip_sort;
                }
                // Notes on the two newest (one too long for the card), one being typed, and a search
                // that finds a clip by its note (in memory only; notes.json isn't written).
                if name.contains("-note") {
                    app.view = ClipsView::All;
                    let newest: Vec<std::path::PathBuf> = clips::list(&app.cfg.folder, 2).into_iter().map(|c| c.path).collect();
                    for (p, n) in newest.iter().zip(["Clutch 1v4 on B site, last round of the match, then the whole team went quiet for a minute", "Funny bug with the door"]) {
                        clips::set_note(&mut app.notes, p, n);
                    }
                    if name == "clips-note-editing" {
                        app.noting = newest.first().map(|p| (p.clone(), "Clutch 1v4 on B site, send to Sam".into()));
                    }
                    if name == "clips-note-search" {
                        app.search = "clutch".into();
                    }
                }
                // The three newest picked, with the selection bar (one tagged; in memory only), its
                // Tag menu open, then "Move them?" (nothing is recycled).
                if name.contains("-picked") {
                    app.view = ClipsView::All;
                    app.picked = clips::list(&app.cfg.folder, 3).into_iter().map(|c| c.path).collect();
                    app.selected = app.picked.first().cloned();
                    if let Some(p) = app.picked.first() {
                        clips::set_tag(&mut app.tags, p, "Aces", true);
                    }
                    if name == "clips-picked-recycle" {
                        app.confirm_recycle = Some(app.picked.clone());
                    }
                }
                if name == "clips-new-tag" {
                    app.tagging = clips::list(&app.cfg.folder, 1).first().map(|c| (vec![c.path.clone()], "Clutch".into()));
                }
                if name == "clips-rename" {
                    app.renaming = clips::list(&app.cfg.folder, 1).first().map(|c| (c.path.clone(), "Ace on Ascent".into()));
                }
                // "Only these apps" with two made-up picks (only shown; the recorder isn't touched).
                if name == "audio-apps" {
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
                if name == "video-follow" {
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
                    app.saved = app.cfg.clone();
                }
                // A made-up settings file picked for import (only shown; nothing is applied).
                if name.starts_with("general-import") {
                    let mut cfg = app.cfg.clone();
                    (cfg.buffer_secs, cfg.hotkey, cfg.folder) = (cfg.buffer_secs + 30, "Alt+F9".into(), r"Z:\Old PC\Clips".into());
                    let favorites = ["a.mp4".to_string(), "b.mp4".to_string()].into();
                    let tags = [("a.mp4".to_string(), ["Aces".to_string()].into())].into();
                    if name == "general-import-network" {
                        (cfg.archive_days, cfg.archive_folder, cfg.keep_days) = (1, r"\\attacker.example@SSL\DavWWWRoot\x".into(), 7);
                    }
                    app.import = Some(config::Export::new(&cfg, &favorites, &tags));
                }
                // "Post to Discord?" for the first clip (nothing is posted).
                if name == "clips-discord-confirm" {
                    app.confirm_discord = clips::list(&app.cfg.folder, 1).first().map(|c| c.path.clone());
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
                // Scrolled down to the Discord card.
                if (name.starts_with("general-discord") || name == "general-archive" || name == "general-move" || name == "general-accent" || name == "general-no-tips" || name.starts_with("general-custom-color")) && i == 30 {
                    use egui_kittest::kittest::Queryable;
                    let label = match name.as_str() {
                        "general-move" => "Export settings...",
                        "general-accent" | "general-no-tips" => "Accent color",
                        "general-custom-color" | "general-custom-color-light" => "Custom color",
                        "general-archive" => "Move clips older than",
                        _ => "Webhook",
                    };
                    h.query_all_by_label(label).next().unwrap().scroll_to_me();
                }
                if name.contains("keybinds-amd") && i == 30 {
                    use egui_kittest::kittest::Queryable;
                    h.query_all_by_label("Check again").last().unwrap().scroll_to_me();
                }
                if name.starts_with("feedback") && i == 30 {
                    use egui_kittest::kittest::Queryable;
                    h.get_by_label("Report on GitHub").scroll_to_me();
                }
                if name == "video-follow" && i == 30 {
                    use egui_kittest::kittest::Queryable;
                    if let Some(n) = h.query_all_by_label("Follow the mouse").next() {
                        n.scroll_to_me();
                    }
                }
                if name == "audio-apps" && i == 30 {
                    use egui_kittest::kittest::Queryable;
                    h.get_by_label("Refresh").scroll_to_me();
                }
                if name == "clips-sort-menu" && i == 30 {
                    use egui_kittest::kittest::Queryable;
                    h.get_by_label("Sort: Longest").click();
                }
                if name == "clips-picked-tag-menu" && i == 30 {
                    use egui_kittest::kittest::Queryable;
                    h.query_all_by_label_contains("Tag").next().unwrap().click();
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
        let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_size([size[0] - 260.0, 100.0]).wgpu().build_ui(move |ui| {
            apply_theme(ui.ctx());
            let (mut view, mut start, mut end) = ((0.0, 30.0), 6.0, 18.0);
            egui::Frame::new().fill(pal().card).inner_margin(12).show(ui, |ui| {
                range_bar(ui, 30.0, &mut view, &mut start, &mut end, 8.0, false, &wave, &media::loud_moments(&wave), &[]);
            });
        });
        h.step();
        h.render().unwrap().save(out.join("waveform.png")).unwrap();
        // Two parts kept (green) and a third being picked, with their chips and the save button.
        let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_size([size[0] - 260.0, 150.0]).wgpu().build_ui(move |ui| {
            apply_theme(ui.ctx());
            let mut t = TrimEdit { path: "x.mp4".into(), len: 30.0, start: 14.0, end: 18.0, view: (0.0, 30.0), gains: vec![], want_split: false, wave: Arc::default(), job: None, parts: vec![(2.0, 5.5), (20.0, 26.0)], gif: None, gif_msg: None, loud: None };
            egui::Frame::new().fill(pal().card).inner_margin(12).show(ui, |ui| {
                range_bar(ui, 30.0, &mut t.view, &mut t.start, &mut t.end, 15.0, false, &parts_wave, &[], &t.parts);
                parts_row(ui, &mut t, false);
                primary(ui, true, RichText::new(format!("Save {} parts as one clip", t.parts.len())).strong(), egui::Vec2::ZERO);
            });
        });
        h.step();
        h.render().unwrap().save(out.join("trim-parts.png")).unwrap();
        // Save as GIF: a 4 s part (ready), the whole 30 s (too long), and one being made.
        let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_size([520.0, 140.0]).wgpu().build_ui(move |ui| {
            apply_theme(ui.ctx());
            let gif = Arc::new(ExportJob::default());
            gif.pct.store(40, std::sync::atomic::Ordering::Relaxed);
            for (start, end, gif) in [(6.0, 10.0, None), (0.0, 30.0, None), (6.0, 10.0, Some(gif))] {
                let busy = gif.is_some();
                let mut t = TrimEdit { path: "x.mp4".into(), len: 30.0, start, end, view: (0.0, 30.0), gains: vec![], want_split: false, wave: Arc::default(), job: None, parts: vec![], gif, gif_msg: None, loud: None };
                ui.horizontal(|ui| {
                    primary(ui, !busy, RichText::new("Save trimmed copy").strong(), egui::Vec2::ZERO);
                    gif_button(ui, &mut t, busy);
                    ui.label(RichText::new(format!("{} s selected", end - start)).small().color(pal().muted));
                });
            }
        });
        h.step();
        h.render().unwrap().save(out.join("trim-gif.png")).unwrap();
    }

    /// How long one frame takes (the app's own pass: layout, painting and tessellation), headless,
    /// per page and look, and one render on the GPU. Prints a table. Run on demand:
    /// cargo test --release frame_time -- --ignored --nocapture
    #[test]
    #[ignore]
    fn frame_time() {
        unsafe { std::env::set_var("WGPU_BACKEND", "dx12") };
        let demo = demo_folder();
        let tabs = [(Tab::Clips, "clips"), (Tab::General, "general"), (Tab::Keybinds, "keybinds"), (Tab::Video, "video"), (Tab::Audio, "audio"), (Tab::Feedback, "feedback"), (Tab::Help, "help"), (Tab::WhatsNew, "whats-new")];
        let mut total = 0.0;
        for (theme, light) in [("minimalist", false), ("minimalist", true), ("retro", false), ("retro", true)] {
            for (tab, t) in tabs {
                let built = std::time::Instant::now();
                let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_size([1100.0, 700.0]).with_accessibility_check(false).wgpu().build_eframe(|cc| {
                    let mut app = App::preview(cc);
                    (app.cfg.folder, app.cfg.theme, app.cfg.light_mode, app.tab) = (demo.clone(), theme.into(), light, tab);
                    app.cfg.tips_seen = ["Clips", "General", "Keybinds", "Video", "Audio", "Help"].map(String::from).to_vec();
                    app.cfg.more_open = ["General", "Keybinds", "Video", "Audio", "Help"].map(String::from).to_vec();
                    app.saved = app.cfg.clone();
                    app.applied = recorder_settings(&app.cfg);
                    app
                });
                let build = built.elapsed().as_secs_f64() * 1000.0;
                h.step();
                let first = built.elapsed().as_secs_f64() * 1000.0 - build;
                // Thumbnails and the first layout settle first.
                for _ in 0..20 {
                    h.step();
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                let n = 60;
                let at = std::time::Instant::now();
                for _ in 0..n {
                    h.step();
                }
                let ms = at.elapsed().as_secs_f64() * 1000.0 / n as f64;
                let at = std::time::Instant::now();
                h.render().unwrap();
                let render = at.elapsed().as_secs_f64() * 1000.0;
                total += ms;
                println!("{theme:>10} {:<5} {t:<9} build {build:6.1} ms   first frame {first:6.1} ms   frame {ms:6.2} ms   render {render:6.1} ms", if light { "light" } else { "dark" });
            }
        }
        println!("average frame {:.2} ms", total / 24.0);
    }

    /// The clips folder screens are rendered from: SCREENS_FOLDER, or six made-up clips written
    /// once to target\demo-clips (never the real clips folder).
    fn demo_folder() -> String {
        std::env::var("SCREENS_FOLDER").unwrap_or_else(|_| {
            let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(r"target\demo-clips");
            media::tests::demo_clips(&dir);
            dir.display().to_string()
        })
    }

    /// One render of the app at `size` to `out\name.png`, set up by `setup`, poked by `act` each step.
    fn audit_shot(out: &std::path::Path, size: [f32; 2], name: &str, setup: impl FnOnce(&mut App), mut act: impl FnMut(&mut egui_kittest::Harness<'_, App>, usize)) {
        let path = out.join(format!("{name}.png"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let demo = demo_folder();
        let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_size(size).with_accessibility_check(false).wgpu().build_eframe(|cc| {
            let mut app = App::preview(cc);
            app.cfg.folder = demo.clone();
            app.cfg.tips_seen = ["Clips", "General", "Keybinds", "Video", "Audio", "Help"].map(String::from).to_vec();
            setup(&mut app);
            app.saved = app.cfg.clone();
            app.applied = recorder_settings(&app.cfg);
            app
        });
        for i in 0..45 {
            act(&mut h, i);
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(35));
        }
        h.render().unwrap().save(path).unwrap();
    }

    /// Every page in every look (Minimalist and Retro, dark and light), with More options closed
    /// and open, each accent and two custom extremes, small, large and tall windows, and extra
    /// states, into SCREENS_OUT\audit (default target\screens\audit).
    /// AUDIT_ONLY=core,accent,size,state,tall picks groups. Run on demand:
    /// cargo test --release audit_screens -- --ignored
    #[test]
    #[ignore]
    fn audit_screens() {
        unsafe { std::env::set_var("WGPU_BACKEND", "dx12") };
        let out = std::env::var("SCREENS_OUT").map(std::path::PathBuf::from).unwrap_or(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(r"target\screens")).join("audit");
        let only = std::env::var("AUDIT_ONLY").unwrap_or_default();
        let want = |group: &str| only.is_empty() || only.split(',').any(|g| g == group);
        let tabs = [(Tab::Clips, "clips"), (Tab::General, "general"), (Tab::Keybinds, "keybinds"), (Tab::Video, "video"), (Tab::Audio, "audio"), (Tab::Feedback, "feedback"), (Tab::Help, "help"), (Tab::WhatsNew, "whats-new")];
        let looks = [("minimalist", false), ("minimalist", true), ("retro", false), ("retro", true)];
        let look_name = |t: &str, l: bool| format!("{t}-{}", if l { "light" } else { "dark" });
        let base = [1100.0, 700.0];
        let set_look = |app: &mut App, theme: &str, light: bool| {
            app.cfg.theme = theme.into();
            app.cfg.light_mode = light;
        };
        // More options open on every page that has it (set as a view choice; nothing is saved).
        let open_all = |app: &mut App| app.cfg.more_open = ["General", "Keybinds", "Video", "Audio", "Help"].map(String::from).to_vec();
        // Every page in each look, then with More options open.
        if want("core") {
            for (theme, light) in looks {
                for more in [false, true] {
                    for (tab, t) in tabs.into_iter().filter(|(t, _)| !more || *t != Tab::Clips) {
                        let name = format!("core/{}-{t}{}", look_name(theme, light), if more { "-more" } else { "" });
                        audit_shot(&out, base, &name, |a| {
                            set_look(a, theme, light);
                            a.tab = tab;
                            if more {
                                open_all(a);
                            }
                        }, |_, _| {});
                    }
                }
            }
        }
        // The pages with More options open, tall enough to show all of it.
        if want("tall") {
            for (theme, light) in [("minimalist", false), ("retro", true)] {
                for (tab, t) in tabs.into_iter().skip(1) {
                    audit_shot(&out, [1100.0, 2000.0], &format!("tall/{}-{t}-more", look_name(theme, light)), |a| {
                        set_look(a, theme, light);
                        a.tab = tab;
                        open_all(a);
                    }, |_, _| {});
                }
            }
        }
        // Each accent and two custom extremes, on Clips (newest picked) and General (at Appearance).
        if want("accent") {
            let accents: Vec<(u8, &str, String)> = ACCENTS.iter().enumerate().map(|(i, a)| (i as u8, "", a.0.to_lowercase())).chain([(CUSTOM_ACCENT, "#F2E35B", "custom-F2E35B".to_string()), (CUSTOM_ACCENT, "#2A1B4E", "custom-2A1B4E".to_string())]).collect();
            for (theme, light) in looks {
                for (idx, hexv, an) in &accents {
                    for (tab, t) in [(Tab::Clips, "clips"), (Tab::General, "general")] {
                        let name = format!("accent/{}-{an}-{t}", look_name(theme, light));
                        let (idx, hexv) = (*idx, hexv.to_string());
                        audit_shot(&out, base, &name, |a| {
                            set_look(a, theme, light);
                            (a.cfg.accent, a.tab, a.general_part) = (idx, tab, GeneralPart::Appearance);
                            open_all(a);
                            if !hexv.is_empty() {
                                a.cfg.custom_accent = hexv.clone();
                            }
                            if tab == Tab::Clips {
                                a.selected = clips::list(&a.cfg.folder, 1).first().map(|c| c.path.clone());
                            }
                        }, |h, i| {
                            if tab == Tab::General && i == 20 {
                                use egui_kittest::kittest::Queryable;
                                h.query_all_by_label("Accent color").next().unwrap().scroll_to_me();
                            }
                        });
                    }
                }
            }
        }
        // The smallest window the app allows, and a large one.
        if want("size") {
            for (size, sn) in [([860.0, 560.0], "860x560"), ([1600.0, 1000.0], "1600x1000")] {
                for (theme, light) in looks {
                    if light && theme == "minimalist" && sn != "860x560" {
                        continue;
                    }
                    for (tab, t) in tabs {
                        audit_shot(&out, size, &format!("size/{sn}-{}-{t}", look_name(theme, light)), |a| {
                            set_look(a, theme, light);
                            a.tab = tab;
                        }, |_, _| {});
                    }
                }
                for step in 0..STEPS.len() {
                    for (theme, light) in [("minimalist", false), ("retro", true)] {
                        audit_shot(&out, size, &format!("size/{sn}-{}-setup{}", look_name(theme, light), step + 1), |a| {
                            set_look(a, theme, light);
                            a.wizard = Some(step);
                        }, |_, _| {});
                    }
                }
            }
        }
        // Extra states in every look.
        if want("state") {
            let empty = std::env::temp_dir().join("rv-audit-empty");
            std::fs::create_dir_all(&empty).unwrap();
            for (theme, light) in looks {
                let ln = look_name(theme, light);
                let e = empty.display().to_string();
                audit_shot(&out, base, &format!("state/{ln}-clips-empty"), |a| {
                    set_look(a, theme, light);
                    (a.tab, a.cfg.folder) = (Tab::Clips, e.clone());
                }, |_, _| {});
                audit_shot(&out, base, &format!("state/{ln}-nosignal"), |a| {
                    set_look(a, theme, light);
                    (a.tab, a.connected, a.buffer, a.status) = (Tab::Clips, false, false, "Can't reach the recorder. Starting it again...".into());
                }, |_, _| {});
                audit_shot(&out, base, &format!("state/{ln}-paused"), |a| {
                    set_look(a, theme, light);
                    (a.tab, a.buffer, a.status) = (Tab::General, false, "Instant replay is off.".into());
                }, |_, _| {});
                audit_shot(&out, base, &format!("state/{ln}-unsaved"), |a| {
                    set_look(a, theme, light);
                    a.tab = Tab::General;
                }, |h, i| {
                    if i == 1 {
                        h.state_mut().cfg.buffer_secs += 30;
                    }
                });
                audit_shot(&out, base, &format!("state/{ln}-tip-save"), |a| {
                    set_look(a, theme, light);
                    a.tab = Tab::Clips;
                }, |h, i| {
                    if i == 5 {
                        use egui_kittest::kittest::Queryable;
                        h.get_by_label("Save clip").hover();
                    }
                });
                // Keyboard focus moved with Tab: its ring.
                audit_shot(&out, base, &format!("state/{ln}-focus"), |a| {
                    set_look(a, theme, light);
                    a.tab = Tab::Keybinds;
                }, |h, i| {
                    if (5..9).contains(&i) {
                        h.key_press(egui::Key::Tab);
                    }
                });
                // Retro with "Reduce effects", at its switch.
                if theme == "retro" {
                    audit_shot(&out, base, &format!("state/{ln}-reduced"), |a| {
                        set_look(a, theme, light);
                        (a.tab, a.cfg.reduce_effects, a.general_part) = (Tab::General, true, GeneralPart::Appearance);
                        open_all(a);
                    }, |h, i| {
                        if i == 20 {
                            use egui_kittest::kittest::Queryable;
                            h.query_all_by_label("Accent color").next().unwrap().scroll_to_me();
                        }
                    });
                }
                audit_shot(&out, base, &format!("state/{ln}-tip-nav"), |a| {
                    set_look(a, theme, light);
                    a.tab = Tab::Clips;
                }, |h, i| {
                    if i >= 5 {
                        h.hover_at(egui::pos2(110.0, 190.0));
                    }
                });
                // A graphics card nobody has tried yet: the untested note.
                audit_shot(&out, base, &format!("state/{ln}-video-untested"), |a| {
                    set_look(a, theme, light);
                    a.tab = Tab::Video;
                    (a.hw.gpu, a.hw.gpu_name, a.hw.vram_mb) = (recorder::Gpu::Amd, "AMD Radeon RX 7800 XT".into(), 16 * 1024);
                }, |_, _| {});
                // Game, mic and chat apart (in memory only), then a custom 5-track setup.
                audit_shot(&out, base, &format!("state/{ln}-audio-chat"), |a| {
                    set_look(a, theme, light);
                    a.tab = Tab::Audio;
                    set_sound_preset(&mut a.cfg, 2);
                }, |_, _| {});
                audit_shot(&out, [1100.0, 900.0], &format!("state/{ln}-audio-custom"), |a| {
                    set_look(a, theme, light);
                    a.tab = Tab::Audio;
                    set_sound_preset(&mut a.cfg, 1);
                    (a.audio_custom, a.cfg.track_count) = (true, 5);
                    a.cfg.track_names[3] = "Music".into();
                }, |_, _| {});
            }
        }
    }

    /// What's new opens on the first start of a new version, never for a new user.
    #[test]
    fn whats_new_after_an_update() {
        assert!(updated_app("0.1.0", true), "another version ran last");
        assert!(!updated_app(env!("CARGO_PKG_VERSION"), true), "same version");
        assert!(updated_app("", true), "from 0.1.3, which didn't keep it");
        assert!(!updated_app("", false), "new user, in setup");
    }

    /// Custom accent colors: "#RRGGBB" (the # optional, any case) and nothing else.
    #[test]
    fn hex_colors() {
        assert_eq!(parse_hex("#8448F0"), Some(Color32::from_rgb(0x84, 0x48, 0xF0)));
        assert_eq!(parse_hex(" 1fa39a "), Some(Color32::from_rgb(0x1F, 0xA3, 0x9A)));
        for bad in ["", "#", "#12345", "#1234567", "#GG0000", "red", "#+12345", "#12 345"] {
            assert_eq!(parse_hex(bad), None, "{bad:?}");
        }
        assert_eq!(hex(Color32::from_rgb(0x1F, 0xA3, 0x9A)), "#1FA39A");
        assert_eq!(parse_hex(&hex(ACCENTS[3].1)), Some(ACCENTS[3].1));
    }

    /// WCAG contrast: the known end points and a known middle value.
    #[test]
    fn contrast_math() {
        let close = |a: f32, b: f32| (a - b).abs() < 0.01;
        assert!(close(contrast(Color32::WHITE, Color32::BLACK), 21.0));
        assert!(close(contrast(Color32::BLACK, Color32::WHITE), 21.0));
        assert!(close(contrast(INK, INK), 1.0));
        // #767676 is the classic grey that just passes on white.
        assert!(close(contrast(Color32::WHITE, Color32::from_gray(0x76)), 4.54), "{}", contrast(Color32::WHITE, Color32::from_gray(0x76)));
        assert_eq!(ink_on(Color32::from_rgb(0xF2, 0xE3, 0x5B)), INK);
        assert_eq!(ink_on(Color32::from_rgb(0x2A, 0x1B, 0x4E)), Color32::WHITE);
        // Already far enough: unchanged. Never: the end color.
        assert_eq!(reach(Color32::BLACK, Color32::WHITE, &[Color32::WHITE], 4.5), Color32::BLACK);
        assert_eq!(reach(Color32::from_gray(128), Color32::WHITE, &[Color32::WHITE], 4.5), Color32::WHITE);
    }

    /// Every preset and any custom color, extremes too, in both modes: the fill stands out 3:1
    /// from the pages, its text reads 4.5:1 (hovered and pressed too), and accent text reads 4.5:1
    /// on every surface. The presets need no adjusting.
    #[test]
    fn accents_stay_readable() {
        let custom = ["#F2E35B", "#2A1B4E", "#FFFFFF", "#000000", "#1FA39A", "#808080", "#3478F6"].map(|h| parse_hex(h).unwrap());
        for p in [&DARK, &LIGHT_PALETTE, &GRAPHITE, &GRAPHITE_LIGHT] {
            let mode = if p.light { "light" } else { "dark" };
            for &(name, main, ..) in &ACCENTS {
                assert_eq!(fit_accent(main, p), main, "{name} {mode} needed adjusting");
                assert!(contrast(Color32::WHITE, main) >= 4.5, "{name}: white on it is {}", contrast(Color32::WHITE, main));
            }
            let all = ACCENTS.iter().map(|a| (a.0.to_string(), a.1, a.2)).chain(custom.iter().map(|&c| {
                let f = fit_accent(c, p);
                (hex(c), f, f.lerp_to_gamma(Color32::WHITE, 0.35))
            }));
            for (name, main, light) in all {
                for s in [p.sidebar, p.bg, p.card] {
                    assert!(contrast(main, s) >= 3.0, "{name} {mode}: fill {} vs {} is {}", hex(main), hex(s), contrast(main, s));
                }
                for fill in [main, shade(main, 0.12), shade(main, 0.2)] {
                    assert!(contrast(ink_on(main), fill) >= 4.5, "{name} {mode}: text on {} is {}", hex(fill), contrast(ink_on(main), fill));
                }
                let text = accent_text(main, light, p);
                for s in [p.sidebar, p.bg, p.card, p.raised] {
                    assert!(contrast(text, s) >= 4.5, "{name} {mode}: accent text {} on {} is {}", hex(text), hex(s), contrast(text, s));
                }
            }
        }
    }

    /// Status and error text reads 4.5:1 on every surface it sits on, a disabled main button's
    /// grey label 4.5:1 on its slab, and control outlines stand out 3:1 from the cards, in both modes.
    #[test]
    fn palette_contrast() {
        for p in [&DARK, &LIGHT_PALETTE, &GRAPHITE, &GRAPHITE_LIGHT] {
            for (name, c) in [("amber", p.amber), ("phosphor", p.phosphor), ("red", p.red), ("muted", p.muted), ("text", p.text)] {
                for s in [p.sidebar, p.bg, p.card, p.raised] {
                    assert!(contrast(c, s) >= 4.5, "{name} on {} (light {}) is {}", hex(s), p.light, contrast(c, s));
                }
            }
            assert!(contrast(p.outline, p.card) >= 3.0, "outline (light {})", p.light);
            assert!(contrast(p.outline, p.bg) >= 3.0, "outline on page (light {})", p.light);
        }
        assert!(contrast(Color32::WHITE, RED) >= 4.5, "Remove button");
    }

    /// Settings the recorder doesn't use never restart it (a restart clears the replay buffer).
    #[test]
    fn app_only_settings_keep_the_recorder() {
        let base = Config::default();
        let mut c = base.clone();
        (c.show_tips, c.nvidia_off, c.game_bar_off, c.accent) = (false, true, true, 3);
        (c.theme, c.light_mode, c.custom_accent, c.reduce_effects) = ("retro".into(), true, "#123456".into(), true);
        c.overlay_hotkey = "Ctrl+Q".into();
        c.clip_sort = 3;
        assert_eq!(recorder_settings(&c), recorder_settings(&base));
        c.buffer_secs += 30;
        assert_ne!(recorder_settings(&c), recorder_settings(&base), "the buffer length does need one");
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

    /// Each sound choice sets its tracks and reads back as itself; the devices recorded stay
    /// recorded; anything else is custom.
    #[test]
    fn sound_presets_map_to_tracks() {
        // A fresh install is Everything together.
        let mut c = Config::default();
        assert_eq!(sound_preset(&c), Some(0));
        let on = |c: &Config, k: &str| c.devices.get(k).map(|d| d.tracks);
        let t = |bits: [u8; 6]| bits.map(|b| b == 1);

        // Game and mic separate gives the mic a track: the default one, as none was recorded.
        set_sound_preset(&mut c, 1);
        assert_eq!((c.track_count, sound_preset(&c)), (3, Some(1)));
        assert_eq!(on(&c, "out:default"), Some(t([1, 1, 0, 0, 0, 0])));
        assert_eq!(on(&c, "in:default"), Some(t([1, 0, 1, 0, 0, 0])));
        assert_eq!(c.chat_tracks, [false; 6]);
        assert_eq!(&c.track_names[..3], ["Everything", "Game and PC", "Mic"]);

        // Game, mic and chat separate adds voice chat on track 4 (never on track 1).
        set_sound_preset(&mut c, 2);
        assert_eq!((c.track_count, sound_preset(&c)), (4, Some(2)));
        assert_eq!(c.chat_tracks, t([0, 0, 0, 1, 0, 0]));
        assert_eq!(c.track_names[3], "Voice chat");

        // Back to one track: the mic stays recorded, now with everything else.
        set_sound_preset(&mut c, 0);
        assert_eq!((c.track_count, sound_preset(&c)), (1, Some(0)));
        assert_eq!(on(&c, "in:default"), Some(t([1, 0, 0, 0, 0, 0])));
        assert_eq!(c.chat_tracks, [false; 6]);

        // A headset someone picked is kept, and nothing that wasn't recorded is added.
        let mut c = Config::default();
        c.devices.clear();
        c.devices.insert("out:headset".into(), config::DeviceSel { name: "Headset".into(), tracks: t([1, 0, 0, 0, 0, 0]) });
        c.devices.insert("out:speakers".into(), config::DeviceSel { name: "Speakers".into(), tracks: [false; 6] });
        set_sound_preset(&mut c, 1);
        assert_eq!(on(&c, "out:headset"), Some(t([1, 1, 0, 0, 0, 0])));
        assert_eq!(on(&c, "out:speakers"), Some([false; 6]));
        assert!(!c.devices.contains_key("out:default"));

        // Two playback devices on different tracks is custom, and track 1 needn't have both.
        let mut c = Config { track_count: 3, ..Config::default() };
        c.devices.clear();
        c.devices.insert("out:chat".into(), config::DeviceSel { name: String::new(), tracks: six([false, true, false]) });
        c.devices.insert("out:game".into(), config::DeviceSel { name: String::new(), tracks: six([true, false, false]) });
        assert_eq!(sound_preset(&c), None);

        // Picking a mic records only that one, on the tracks given.
        set_mic(&mut c, Some("in:usb"), t([1, 0, 1, 0, 0, 0]));
        set_mic(&mut c, Some("in:headset"), t([1, 0, 1, 0, 0, 0]));
        assert_eq!(used_devices(&c, "in:").map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["in:headset"]);
        set_mic(&mut c, None, [true; 6]);
        assert_eq!(used_devices(&c, "in:").count(), 0);
    }

    /// The sidebar's page links are named tabs in both themes (they're painted), and clicking one
    /// opens its page.
    #[test]
    fn nav_items_are_named() {
        use egui_kittest::kittest::Queryable;
        for theme in ["minimalist", "retro"] {
            let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_size([1100.0, 700.0]).with_accessibility_check(false).build_eframe(|cc| {
                let mut app = App::preview(cc);
                (app.cfg.theme, app.tab) = (theme.into(), Tab::Clips);
                app.saved = app.cfg.clone();
                app
            });
            h.run();
            h.get_by_role_and_label(egui::Role::Tab, "Keybinds").click();
            h.run();
            assert!(h.state().tab == Tab::Keybinds, "{theme}");
        }
    }

    /// What's new: the newest version starts open and its heading closes it.
    #[test]
    fn whats_new_newest_opens_and_closes() {
        use egui_kittest::kittest::Queryable;
        let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_size([1100.0, 700.0]).with_accessibility_check(false).build_eframe(|cc| {
            let mut app = App::preview(cc);
            app.tab = Tab::WhatsNew;
            app
        });
        h.run();
        let item = "The app reopens maximized if you left it maximized.";
        assert!(h.query_by_label(item).is_some());
        h.get_by_label_contains("(this version)").click();
        // Past the closing animation.
        h.run_steps(30);
        assert!(h.query_by_label(item).is_none());
    }

    /// A newer release shows a link in the sidebar, unless the user turned the check off.
    #[test]
    fn new_version_link_in_sidebar() {
        use egui_kittest::kittest::Queryable;
        let _one_at_a_time = FOUND_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        app_update::set_found(Some(nine()));
        let harness = |check: bool| {
            let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_size([1100.0, 700.0]).with_accessibility_check(false).build_eframe(move |cc| {
                let mut app = App::preview(cc);
                app.saved.check_updates = check;
                app
            });
            h.run();
            h.query_by_label_contains("Version 9.0.0 is out").is_some()
        };
        let (on, off) = (harness(true), harness(false));
        app_update::set_found(None);
        assert!(on, "the link shows");
        assert!(!off, "no link with the check turned off");
    }

    /// The found release is global, so the tests that set it take turns.
    static FOUND_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn nine() -> app_update::Newer {
        app_update::Newer {
            version: "9.0.0".into(),
            url: "https://github.com/ItsFlamme/replay-vault/releases/tag/v9.0.0".into(),
            notes: "- Faster saves".into(),
            installer: None,
            portable: Some(app_update::Installer { url: "https://github.com/ItsFlamme/replay-vault/releases/download/v9.0.0/ReplayVault-9.0.0.exe".into(), size: 1, sha256: "0".repeat(64) }),
        }
    }

    /// On a PC without the recorder yet, setup shows the GPU preference (ticked by default) next to
    /// the firewall rule, so it isn't set without the user seeing it. Never clicks: setup writes
    /// settings as it goes. With SCREENS_OUT set it also saves a picture of the step.
    #[test]
    fn setup_shows_gpu_choice() {
        use egui_kittest::kittest::Queryable;
        fn build(cc: &mut eframe::CreationContext<'_>) -> App {
            let mut app = App::preview(cc);
            app.wizard = Some(0);
            app.obs_exe = None;
            app
        }
        let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_size([1100.0, 900.0]).with_accessibility_check(false).build_eframe(build);
        h.run();
        assert!(h.query_by_label("Run the recorder on the high-performance GPU").is_some());
        assert!(h.query_by_label("Block other devices from reaching the recorder").is_some());
        assert!(Config::default().gpu_high, "ticked unless the user unticks it");
        if let Some(out) = std::env::var_os("SCREENS_OUT") {
            let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_size([1100.0, 900.0]).with_accessibility_check(false).wgpu().build_eframe(build);
            h.run();
            h.render().unwrap().save(std::path::Path::new(&out).join("setup-obs.png")).unwrap();
        }
    }

    /// The sidebar link opens the new-version popup with what's new; this test copy isn't
    /// installed, so it updates itself from the release's exe: Install now, not the download
    /// page. Later closes it.
    #[test]
    fn new_version_popup() {
        use egui_kittest::kittest::Queryable;
        let _one_at_a_time = FOUND_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        app_update::set_found(Some(nine()));
        let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_size([1100.0, 700.0]).with_accessibility_check(false).build_eframe(|cc| App::preview(cc));
        h.run();
        let shown_before = h.query_by_label("Replay Vault 9.0.0 is out").is_some();
        h.get_by_label_contains("Version 9.0.0 is out").click();
        h.run();
        let shown = h.query_by_label("Replay Vault 9.0.0 is out").is_some() && h.query_by_label("- Faster saves").is_some();
        let offer = h.query_by_label("Update now").is_some() && h.query_by_label("Open download page").is_none();
        h.get_by_label("Update later").click();
        h.run();
        let gone = h.query_by_label("Replay Vault 9.0.0 is out").is_none();
        app_update::set_found(None);
        assert!(!shown_before, "not counted as found in tests, so it waits for the click");
        assert!(shown, "title and what's new");
        assert!(offer, "portable: Update now from the release's exe, not the download page");
        assert!(gone, "Later closes it");
    }

    /// Changes show the Save bar; Discard and Restore defaults work. Never clicks Save, so the
    /// settings file is left alone.
    #[test]
    fn save_bar_discard_and_defaults() {
        use egui_kittest::kittest::Queryable;
        let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_size([1100.0, 700.0]).with_accessibility_check(false).build_eframe(|cc| {
            let mut app = App::preview(cc);
            app.tab = Tab::Keybinds;
            // A saved key that isn't the default, so this page's Restore defaults changes something
            // on any PC, a fresh one included (a General setting here didn't count on Keybinds).
            app.cfg.hotkey = "Ctrl+Alt+F12".into();
            assert_ne!(app.cfg.hotkey, Config::default().hotkey);
            app.saved = app.cfg.clone();
            app.applied = recorder_settings(&app.saved);
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
            self.drag_at(from_x, to_x, Self::Y);
        }

        /// The same at height `y` on the bar.
        fn drag_at(&mut self, from_x: f32, to_x: f32, y: f32) {
            let button = |x, pressed| egui::Event::PointerButton {
                pos: egui::pos2(x, y),
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            self.frame(vec![egui::Event::PointerMoved(egui::pos2(from_x, y))], None);
            self.frame(vec![button(from_x, true)], None);
            let steps = 20;
            for k in 1..=steps {
                let x = from_x + (to_x - from_x) * k as f32 / steps as f32;
                self.frame(vec![egui::Event::PointerMoved(egui::pos2(x, y))], None);
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
            cfg.devices.insert(key.into(), config::DeviceSel { name: name.into(), tracks: six(tracks) });
        }
        let names: Vec<String> = (0..3).map(|i| track_name(&cfg, i)).collect();
        assert_eq!(names, ["Everything", "Chat + Game", "Chat Mic"]);
        // A name the user typed wins; clearing it goes back to the devices.
        cfg.track_names[1] = "Comms + game".into();
        assert_eq!(track_name(&cfg, 1), "Comms + game");
        cfg.track_names[1] = "  ".into();
        assert_eq!(track_name(&cfg, 1), "Chat + Game");
        // A track nothing is recorded on.
        cfg.devices.get_mut("in:mic").unwrap().tracks = six([true, false, false]);
        assert_eq!(track_name(&cfg, 2), "Track 3");
    }

    /// Apps go by the name in their window title, the exe's when the title has none.
    #[test]
    fn app_names_from_titles() {
        let app = |title: &str, exe: &str| config::AppSel { title: title.into(), class: String::new(), exe: exe.into() };
        assert_eq!(app_label(&app("VALORANT  ", "VALORANT-Win64-Shipping.exe")), "VALORANT");
        assert_eq!(app_label(&app("#general | Friends - Discord", "Discord.exe")), "Discord");
        assert_eq!(app_label(&app("Lofi beats - YouTube - Google Chrome", "chrome.exe")), "Google Chrome");
        assert_eq!(app_label(&app("  ", "Game.EXE")), "Game");
        assert_eq!(app_label(&app(&"x".repeat(60), "longtitle.exe")), "longtitle");
        assert_eq!(app_detail(&app("VALORANT  ", "VALORANT-Win64-Shipping.exe")), "VALORANT-Win64-Shipping.exe (VALORANT)");
    }

    /// Ticks for the first three tracks, the other three off.
    fn six(t: [bool; 3]) -> [bool; 6] {
        [t[0], t[1], t[2], false, false, false]
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

    /// Each Sort order, on made-up clips (nothing is read): ties and unknown lengths keep the
    /// list's order, names sort without minding case.
    #[test]
    fn sort_orders() {
        let at = |s| std::time::UNIX_EPOCH + std::time::Duration::from_secs(s);
        let clip = |name: &str, bytes, secs| clips::Clip { path: name.into(), name: name.into(), bytes, modified: at(secs), group: None, is_picture: false };
        // Newest first, as the folder lists them.
        let all = [clip("b.mp4", 300, 40), clip("C.mp4", 100, 30), clip("a.mp4", 300, 20), clip("d.png", 50, 10)];
        let lengths: std::collections::HashMap<std::path::PathBuf, f64> = [("b.mp4".into(), 12.0), ("C.mp4".into(), 95.5), ("a.mp4".into(), 30.0)].into();
        let order = |sort| {
            let mut list: Vec<&clips::Clip> = all.iter().rev().collect();
            sort_clips(&mut list, sort, &lengths);
            list.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
        };
        assert_eq!(order(0), ["b.mp4", "C.mp4", "a.mp4", "d.png"]);
        assert_eq!(order(1), ["d.png", "a.mp4", "C.mp4", "b.mp4"]);
        assert_eq!(order(2), ["C.mp4", "a.mp4", "b.mp4", "d.png"], "no length known: last");
        assert_eq!(order(3), ["a.mp4", "b.mp4", "C.mp4", "d.png"], "same size: list order kept");
        assert_eq!(order(4), ["a.mp4", "b.mp4", "C.mp4", "d.png"]);
        assert_eq!(order(9), order(0), "an order from a newer version: newest");
        assert!(SORTS[0].1.is_none() && SORTS[1].1.is_none(), "by date: a heading per day");
    }

    /// Ctrl+click toggles a clip (the clip clicked before joins first), Shift+click picks the run
    /// from the anchor in page order, either way round.
    #[test]
    fn ctrl_and_shift_clicks_pick_clips() {
        let p = |n: &str| std::path::PathBuf::from(format!("{n}.mp4"));
        let shown: Vec<std::path::PathBuf> = ["a", "b", "c", "d", "e"].map(p).to_vec();
        let (a, b, c, d) = (p("a"), p("b"), p("c"), p("d"));
        // Ctrl+click after a plain click on a: both.
        let picked = pick_after_click(&[], Some(&a), &c, false, &shown);
        assert_eq!(picked, [a.clone(), c.clone()]);
        // Again on a: off, in the order picked.
        assert_eq!(pick_after_click(&picked, Some(&c), &a, false, &shown), std::slice::from_ref(&c));
        // Ctrl+click on the clip already picked, with nothing else: just it.
        assert_eq!(pick_after_click(&[], Some(&a), &a, false, &shown), std::slice::from_ref(&a));
        // Shift+click from b to d, and from d back to b.
        assert_eq!(pick_after_click(&picked, Some(&b), &d, true, &shown), [b.clone(), c.clone(), d.clone()]);
        assert_eq!(pick_after_click(&[], Some(&d), &b, true, &shown), [b.clone(), c.clone(), d.clone()]);
        // No anchor, or one not on the page: just the clip.
        assert_eq!(pick_after_click(&[], None, &c, true, &shown), std::slice::from_ref(&c));
        assert_eq!(pick_after_click(&[], Some(&p("gone")), &c, true, &shown), std::slice::from_ref(&c));
    }

    /// Hover scrubbing asks for one frame at a time, and drops its frames (not the thumbnails)
    /// once there are too many. Uses a clip that doesn't exist, so nothing is decoded.
    #[test]
    fn scrub_frames_one_at_a_time_and_capped() {
        let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_accessibility_check(false).build_eframe(|cc| App::preview(cc));
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
        let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_size([1100.0, 700.0]).with_accessibility_check(false).build_eframe(|cc| {
            let mut app = App::preview(cc);
            app.tab = Tab::Clips;
            // The keys work on the clip lists (Home's rows don't take them).
            app.view = ClipsView::All;
            app.saved.folder = folder.clone();
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

    /// Ctrl+A picks every clip shown, Delete then asks once for them all (the question is left
    /// unanswered, so nothing is recycled), and Esc lets them go. Pictures in a temp folder.
    #[test]
    fn ctrl_a_picks_all_and_esc_lets_go() {
        let dir = std::env::temp_dir().join("instant-replay-pick-all");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for n in ["a.png", "b.png", "c.png"] {
            std::fs::write(dir.join(n), b"not really a picture").unwrap();
        }
        let folder = dir.display().to_string();
        let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_size([1100.0, 700.0]).with_accessibility_check(false).build_eframe(|cc| {
            let mut app = App::preview(cc);
            (app.tab, app.view, app.cfg.folder) = (Tab::Clips, ClipsView::All, folder);
            app.saved.folder = app.cfg.folder.clone();
            app
        });
        h.run_steps(2);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.run_steps(2);
        assert_eq!(h.state().picked.len(), 3);
        h.key_press(egui::Key::Delete);
        h.run_steps(2);
        assert_eq!(h.state().confirm_recycle.as_ref().map(Vec::len), Some(3));
        h.state_mut().confirm_recycle = None;
        h.run_steps(2);
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        assert!(h.state().picked.is_empty());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 3, "nothing recycled");
        drop(h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A save key another program holds is found without pressing it, and named "another
    /// program" since it isn't NVIDIA's or AMD's. Ctrl+Alt+Shift+F22, held by another thread,
    /// stands in for that program.
    #[test]
    fn finds_a_save_key_another_program_holds() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_CONTROL, MOD_SHIFT, RegisterHotKey, UnregisterHotKey};
        let c = hotkey::Combo { vk: 0x85, ctrl: true, alt: true, shift: true, win: false };
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let other = std::thread::spawn(move || unsafe {
            RegisterHotKey(None, 0xA2, MOD_CONTROL | MOD_ALT | MOD_SHIFT, 0x85).unwrap();
            held_tx.send(()).unwrap();
            let _ = done_rx.recv();
            let _ = UnregisterHotKey(None, 0xA2);
        });
        held_rx.recv().unwrap();
        let mut h = egui_kittest::Harness::builder().with_max_steps(4).with_accessibility_check(false).build_eframe(|cc| App::preview(cc));
        h.state_mut().cfg.hotkey = c.to_string();
        h.state_mut().check_conflicts();
        let found = h.state().conflicts.iter().find(|(t, _)| *t == Target::Save).map(|(_, s)| s.clone());
        done_tx.send(()).unwrap();
        other.join().unwrap();
        let text = found.expect("the save key's clash");
        assert!(text.starts_with(&format!("Another program (or Windows) also uses {c}, so pressing it does both.")), "{text}");
    }

    /// A key another program registered first still works here: it's shared (heard through Raw
    /// Input), and freed keys go back to being the app's own. Ctrl+Alt+Shift+F21, held by
    /// another thread, stands in for NVIDIA's overlay.
    #[test]
    fn a_held_key_is_shared_not_dead() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_CONTROL, MOD_SHIFT, RegisterHotKey, UnregisterHotKey};
        let c = hotkey::Combo { vk: 0x84, ctrl: true, alt: true, shift: true, win: false };
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let other = std::thread::spawn(move || unsafe {
            RegisterHotKey(None, 0xA3, MOD_CONTROL | MOD_ALT | MOD_SHIFT, 0x84).unwrap();
            held_tx.send(()).unwrap();
            let _ = done_rx.recv();
            let _ = UnregisterHotKey(None, 0xA3);
        });
        held_rx.recv().unwrap();
        let _ = hotkey::install(|_| {});
        assert_eq!(hotkey::set_combo(hotkey::Action::Mic, Some(c)), Ok(hotkey::Held::Shared));
        done_tx.send(()).unwrap();
        other.join().unwrap();
        assert_eq!(hotkey::set_combo(hotkey::Action::Mic, Some(c)), Ok(hotkey::Held::Ours), "free now");
        assert_eq!(hotkey::set_combo(hotkey::Action::Mic, None), Ok(hotkey::Held::Ours));
    }

    /// Picking a key another action already has moves it, rather than refusing; the save key's
    /// own key isn't taken from it.
    #[test]
    fn a_picked_key_moves_from_another_action() {
        let mut h = egui_kittest::Harness::builder().with_max_steps(4).with_accessibility_check(false).build_eframe(|cc| App::preview(cc));
        let app = h.state_mut();
        (app.cfg.hotkey, app.cfg.hotkey_screenshot, app.cfg.hotkey_record) = ("Alt+F10".into(), "Alt+F12".into(), String::new());
        app.capturing = Some(Target::Record);
        hotkey::fake_capture(hotkey::Combo::parse("Alt+F12").unwrap());
        app.finish_capture();
        assert_eq!((app.cfg.hotkey_record.as_str(), app.cfg.hotkey_screenshot.as_str()), ("Alt+F12", ""));
        assert!(app.status.contains("moved"), "{}", app.status);
        app.capturing = Some(Target::Record);
        hotkey::fake_capture(hotkey::Combo::parse("Alt+F10").unwrap());
        app.finish_capture();
        assert_eq!((app.cfg.hotkey.as_str(), app.cfg.hotkey_record.as_str()), ("Alt+F10", "Alt+F12"), "the save key keeps its key");
    }

    /// The Esc that cancels a key capture does nothing else (it used to hide the app): here it
    /// leaves the picked clips picked, and an Esc pressed later clears them as usual.
    #[test]
    fn esc_that_cancels_a_key_capture_is_swallowed() {
        let dir = std::env::temp_dir().join("instant-replay-esc-capture");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.png"), b"not really a picture").unwrap();
        let folder = dir.display().to_string();
        let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_size([1100.0, 700.0]).with_accessibility_check(false).build_eframe(|cc| {
            let mut app = App::preview(cc);
            (app.tab, app.view, app.cfg.folder) = (Tab::Clips, ClipsView::All, folder);
            app.saved.folder = app.cfg.folder.clone();
            app.picked = vec![dir.join("a.png")];
            app.capture_ended = Some(std::time::Instant::now());
            app
        });
        h.run_steps(2);
        // Just ended (the first frames can take longer than the 500 ms it allows on a busy PC).
        h.state_mut().capture_ended = Some(std::time::Instant::now());
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        assert_eq!(h.state().picked.len(), 1, "swallowed");
        h.state_mut().capture_ended = Some(std::time::Instant::now() - std::time::Duration::from_secs(2));
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        assert!(h.state().picked.is_empty(), "a later Esc works");
        drop(h);
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("instant-replay-esc-capture"));
    }

    /// A renamed clip keeps its place in the montage, the picks, the selection and the export
    /// queue (they used to keep the old name, so a montage made after a rename failed). In a temp
    /// folder.
    #[test]
    fn a_renamed_clip_is_followed() {
        let dir = std::env::temp_dir().join("instant-replay-rename-follows");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (old, new) = (dir.join("a.png"), dir.join("b.png"));
        std::fs::write(&old, b"not really a picture").unwrap();
        let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_accessibility_check(false).build_eframe(|cc| App::preview(cc));
        let app = h.state_mut();
        app.picked = vec![old.clone()];
        app.selected = Some(old.clone());
        app.montage = Some(Montage { clips: vec![old.clone()], fades: true, music: None, volume: 0.4, job: None });
        app.export_queue.push_back((old.clone(), media::Share::P720));
        app.rename_clip(&old, "b");
        assert!(new.exists() && !old.exists());
        assert_eq!((app.picked.clone(), app.selected.clone()), (vec![new.clone()], Some(new.clone())));
        assert_eq!(app.montage.as_ref().unwrap().clips, vec![new.clone()]);
        assert_eq!(app.export_queue.front().map(|(p, _)| p.clone()), Some(new));
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
        let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_accessibility_check(false).build_eframe(|cc| {
            let mut app = App::preview(cc);
            (app.tab, app.cfg.folder) = (Tab::Clips, folder);
            app.saved.folder = app.cfg.folder.clone();
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
    fn range_bar_moves_along_when_zoomed() {
        let mut b = Bar::new();
        // The whole clip in view: the plain wheel is left for the page.
        b.wheel(b.x_of(5.0), -3.0, Default::default());
        assert_eq!(b.view, (0.0, 10.0));
        // Zoomed in, the wheel turned down moves later.
        b.view = (0.0, 2.0);
        b.wheel(b.x_of(1.0), -3.0, Default::default());
        assert!(b.view.0 > 0.0 && b.view.1 == 2.0, "view after the wheel: {:?}", b.view);
        // Dragging the strip along the bottom (the whole clip across the bar) moves the view
        // with it: 300 of 1000 px is 3 of 10 s. The edges stay put.
        let at = b.view.0;
        let x = b.rect.left() + (at / 10.0) as f32 * b.rect.width() + 5.0;
        b.drag_at(x, x + 300.0, b.rect.top() + 34.0);
        assert!((b.view.0 - (at + 3.0).min(8.0)).abs() < 0.1, "view after dragging the strip from {at}: {:?}", b.view);
        assert_eq!((b.start, b.end), (0.0, 10.0));
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
