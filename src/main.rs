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
mod instance;
mod log;
mod media;
mod obs;
mod obs_import;
mod obs_install;
mod overlay;
mod perf;
mod profile;
mod recorder;
mod session;
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
    // The uninstaller: this copy, if it's running, quits the normal way first (see `session`).
    if std::env::args().any(|a| a == "--quit") {
        session::ask_to_quit(std::time::Duration::from_secs(30));
        return Ok(());
    }
    // Started by Windows at login: stay in the tray.
    let minimized = std::env::args().any(|a| a == "--minimized");
    // Only one copy at a time: a second launch brings up the running one and quits, or waits for
    // one that's closing. If it's another copy of the app (one in another folder), it says so
    // instead.
    if let Err(lock) = take_lock()
        && !wait_for_turn(&lock, minimized)
    {
        return Ok(());
    }
    log::catch_panics();
    log::write(&format!("App started (version {}{}).", env!("CARGO_PKG_VERSION"), if DEV_BUILD { ", dev build" } else { "" }));
    startup::repair();
    // Shutting down or signing out never closes the window, so on_exit wouldn't run.
    if session::watch(session_notice).is_none() {
        log::write("Couldn't listen for Windows shutting down; a recording running then may be cut off. Display changes won't be noticed either.");
    }
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

/// The one-copy-at-a-time lock (a named mutex) another copy holds. The handle is never closed:
/// Windows releases the mutex when this process exits.
struct Lock(windows::Win32::Foundation::HANDLE);

impl Lock {
    /// Waits up to `ms` for the copy holding it to let go; true once this one holds it.
    fn wait(&self, ms: u32) -> bool {
        use windows::Win32::Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0};
        let got = unsafe { windows::Win32::System::Threading::WaitForSingleObject(self.0, ms) };
        got == WAIT_OBJECT_0 || got == WAIT_ABANDONED
    }
}

/// Takes the lock for the life of the process, or gives it back to wait on if another copy holds
/// it. A dev build has its own, so it and a release copy can run side by side.
fn take_lock() -> Result<(), Lock> {
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows::Win32::System::Threading::CreateMutexW;
    let name = if DEV_BUILD { windows::core::w!("Local\\ReplayVaultDev") } else { windows::core::w!("Local\\ReplayVault") };
    match unsafe { CreateMutexW(None, true, name) } {
        Ok(h) if (unsafe { GetLastError() }) == ERROR_ALREADY_EXISTS => Err(Lock(h)),
        _ => Ok(()),
    }
}

/// Exists while a copy is closing (stopping its recorder can take a while), so a launch meanwhile
/// waits for it rather than bringing up its window or saying another copy is running.
fn closing_name() -> windows::core::PCWSTR {
    if DEV_BUILD { windows::core::w!("Local\\ReplayVaultDev.closing") } else { windows::core::w!("Local\\ReplayVault.closing") }
}

/// Marks this copy as closing until it exits (the handle is never closed).
fn mark_closing() {
    let _ = unsafe { windows::Win32::System::Threading::CreateEventW(None, true, false, closing_name()) };
}

/// Whether a copy has said it's closing.
fn other_closing() -> bool {
    use windows::Win32::System::Threading::{OpenEventW, SYNCHRONIZATION_SYNCHRONIZE};
    match unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, false, closing_name()) } {
        Ok(h) => {
            let _ = unsafe { windows::Win32::Foundation::CloseHandle(h) };
            true
        }
        Err(_) => false,
    }
}

/// What a launch waiting for the lock does after one look around.
#[derive(Debug, PartialEq)]
enum Turn {
    /// This same copy is running and not closing: bring up its window and quit.
    BringUp,
    /// Wait a little more for the lock.
    Wait,
    /// Waited long enough: say why and quit.
    GiveUp(Busy),
}

/// Why a launch gave up.
#[derive(Debug, PartialEq)]
enum Busy {
    OtherCopy,
    StillClosing,
}

/// One pass of `wait_for_turn`: `closing` is whether a copy is closing right now, `window` whether
/// this same copy's window is there, `seen_closing` whether any pass saw one closing, and
/// `waited` how long this launch has waited. A copy that's closing gets up to 30 s (finishing a
/// recording alone can take 15 s); otherwise about 2 s is enough for a copy started a moment ago
/// to show its window.
fn next_turn(closing: bool, window: bool, seen_closing: bool, waited: std::time::Duration) -> Turn {
    if window && !closing {
        return Turn::BringUp;
    }
    if waited < std::time::Duration::from_secs(if seen_closing { 30 } else { 2 }) {
        Turn::Wait
    } else if closing {
        Turn::GiveUp(Busy::StillClosing)
    } else {
        Turn::GiveUp(Busy::OtherCopy)
    }
}

/// A launch that found `lock` taken. Brings up this same copy's window if it's running (not when
/// started at sign-in), or waits for a copy that's closing (quitting, or restarting into a
/// portable update) and then starts; see `next_turn`. True when this launch got the lock and
/// should start.
fn wait_for_turn(lock: &Lock, minimized: bool) -> bool {
    let start = std::time::Instant::now();
    let mut seen_closing = false;
    loop {
        let running = overlay::running_copy();
        let closing = other_closing() || matches!(running, Some(overlay::Running::Closing));
        seen_closing |= closing;
        match (next_turn(closing, matches!(running, Some(overlay::Running::Window(_))), seen_closing, start.elapsed()), running) {
            (Turn::BringUp, Some(overlay::Running::Window(window))) => {
                if !minimized {
                    overlay::show_existing(window);
                }
                return false;
            }
            (Turn::GiveUp(why), _) => {
                busy(why);
                return false;
            }
            _ => {}
        }
        if lock.wait(250) {
            return true;
        }
    }
}

/// Tells a launch why it isn't starting. Also for a launch at sign-in (`--minimized`), or two
/// copies both set to start with Windows would fail silently.
fn busy(why: Busy) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MessageBoxW};
    let text = match why {
        Busy::OtherCopy => windows::core::w!("Another copy of Replay Vault is already running. Quit it from its tray icon (right-click, Quit), then open this one again."),
        Busy::StillClosing => windows::core::w!("Replay Vault is still closing. Open it again in a moment."),
    };
    unsafe { MessageBoxW(None, text, &windows::core::HSTRING::from(APP_TITLE), MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND) };
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
    const SETTINGS: [Tab; 4] = [Tab::General, Tab::Keybinds, Tab::Video, Tab::Audio];
    /// The small links at the bottom of the sidebar.
    const LINKS: [Tab; 3] = [Tab::Help, Tab::WhatsNew, Tab::Feedback];
    const ALL: [Tab; 8] = [Tab::Clips, Tab::General, Tab::Keybinds, Tab::Video, Tab::Audio, Tab::Feedback, Tab::Help, Tab::WhatsNew];

    /// Its name (in the sidebar and title, and its key for More options), its sidebar glyph, the
    /// line under its title, and its sidebar link's hover text.
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
    /// The startup entry is there but turned off in Task Manager (see `refresh_startup`).
    startup_turned_off: bool,
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
    /// Keys another app held when last tried, when that was, and how long until the next try:
    /// they're tried again so they work once that app lets go (NVIDIA's overlay closing at start,
    /// say), less often the longer it holds them (see `next_hotkey_retry`).
    hotkey_failed: Vec<Target>,
    hotkeys_tried: std::time::Instant,
    hotkey_retry: std::time::Duration,
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
    /// Last lines of the app log for the Help page, and the file size/time they were read at.
    log_view: Vec<String>,
    log_stamp: (u64, Option<std::time::SystemTime>),
    log_checked: std::time::Instant,
    /// Setup's last settings that couldn't be saved, and when: tried again every 2 seconds, not
    /// every frame.
    setup_unsaved: Option<(Config, std::time::Instant)>,
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
    /// The Feedback page's form.
    feedback: feedback::Form,
    /// The user has their own OBS setup to import from, and what was last imported.
    obs_settings_found: bool,
    imported: Option<String>,
    /// Clips tab: the folder's clips (re-read every few seconds), their frames and lengths.
    clip_list: Vec<clips::Clip>,
    /// The games in clip_list (see `games_in`), worked out once per read of the folder.
    clip_games: std::rc::Rc<[Game]>,
    clips_checked: std::time::Instant,
    /// Read the clips folder again on the next frame, without waiting out the 3 s (see `rescan_clips`).
    rescan_now: bool,
    /// The clips folder being read on another thread.
    clips_scan: Option<std::sync::mpsc::Receiver<ClipsScan>>,
    /// Clips moved in the window so far, so a read begun before the last one is dropped.
    clip_moves: u64,
    /// Free space on the clips drive, from the last scan.
    clips_free: Option<u64>,
    /// The folders the last scan read in full (`clips::list_read`): share links are judged
    /// orphaned only in those.
    clips_read: Vec<std::path::PathBuf>,
    /// The archive folder and its clips (None: its drive or NAS isn't connected), looked at in the
    /// background while the Archive view is up (a NAS that's off can take a while to answer).
    archive_list: Option<(std::path::PathBuf, Option<Vec<clips::Clip>>)>,
    archive_scan: Option<std::sync::mpsc::Receiver<(std::path::PathBuf, Option<Vec<clips::Clip>>)>>,
    archive_checked: std::time::Instant,
    previews: clips::Previews,
    /// Frame textures by (path, tenths of a second); None when the clip can't be decoded.
    textures: Textures,
    /// The Clips page's cards, made again only when what they show changes.
    clip_cards: CardsCache,
    /// Game covers by game; None when the file can't be read.
    covers: std::collections::HashMap<String, Option<egui::TextureHandle>>,
    lengths: std::collections::HashMap<std::path::PathBuf, f64>,
    /// The hover-scrub frame being decoded (path, seconds); one at a time.
    scrub_wait: Option<(std::path::PathBuf, f64)>,
    trim: Option<TrimEdit>,
    clips_msg: Option<String>,
    /// The clip playing in the window, if any.
    player: Option<(std::path::PathBuf, player::Player)>,
    /// A clip being opened in the player (see `Opening`).
    opening: Option<Opening>,
    /// What the Clips page shows: every clip, one game's, the starred ones, a tag's or the archive.
    view: ClipsView,
    /// Share links, running uploads/deletes, and a clip waiting for the upload notice to be accepted.
    links: std::collections::BTreeMap<String, upload::Link>,
    uploads: std::collections::HashMap<std::path::PathBuf, Arc<upload::Job>>,
    /// Share links of clips that are gone, and deleting them.
    link_cleanup: LinkCleanup,
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
    /// Marked clips a change of the clips folder couldn't carry over (see `refolder`).
    lost: std::collections::BTreeSet<String>,
    /// Set when a change of the clips folder couldn't save every store under it: auto-cleanup holds
    /// off until a start re-keys them (see `refolder`).
    unfiled: bool,
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
    /// The card to highlight after Delete in the grid, once the clip is gone.
    recycle_next: Option<std::path::PathBuf>,
    /// Share copies waiting to be made, one at a time after the running ones.
    export_queue: std::collections::VecDeque<(std::path::PathBuf, media::Share)>,
    /// Clips page search text.
    search: String,
    /// Montage mode, while it's on.
    montage: Option<Montage>,
    /// Why auto-cleanup and the archive won't work in the clips folder being set, for the folder
    /// and the folder confirmed anyway it was worked out for (see `clips_folder_risk`).
    folder_risk: Option<((String, String), Option<String>)>,
    /// What auto-cleanup would remove right now (clips, bytes), for the rule it was worked out for.
    cleanup_preview: Option<(clips::Cleanup, std::time::Instant, usize, u64)>,
    /// What the archive would move right now (clips, bytes), for the rule it was worked out for.
    archive_preview: Option<(clips::Archive, std::time::Instant, usize, u64)>,
    /// CPU, GPU and memory readings; only while the Help page is open.
    perf: Option<perf::Sampler>,
    /// OBS skipped frames while encoding: how many, at which Encoder effort. Until dismissed.
    encoder_hint: Option<(u64, u8)>,
    /// How far the app has slimmed down since the window was hidden.
    slim: Slim,
    /// The mic is muted with its key (until unmuted or the app closes).
    mic_muted: bool,
    /// A display change the recorder hasn't been restarted for yet (see `follow_displays`).
    display_wait: Option<DisplayWait>,
    /// When a display change last restarted the recorder.
    display_restarted: Option<std::time::Instant>,
}

/// A display change waiting to restart the recorder: what the recorder took from the monitors
/// when it was started, when to look again, and the last hold that was logged.
struct DisplayWait {
    running: Option<recorder::DisplayKey>,
    next: std::time::Instant,
    logged: Option<DisplayHold>,
}

/// What to do about a display change (`display_step`).
#[derive(Clone, Copy, Debug, PartialEq)]
enum DisplayStep {
    Restart,
    /// What the recorder takes from the monitors is as it was: nothing to do.
    Unchanged,
    Hold(DisplayHold),
}

/// Why a display change waits.
#[derive(Clone, Copy, Debug, PartialEq)]
enum DisplayHold {
    /// A full recording runs: a restart would end it.
    Recording,
    /// A display change restarted the recorder less than `DISPLAY_RESTART_GAP` ago; this long is left.
    TooSoon(std::time::Duration),
    /// A game is in front: a restart would lose the replay so far, and a game in exclusive full
    /// screen changes the resolution only while it runs (the capture is fitted to the canvas meanwhile).
    Game,
}

/// Display changes restart the recorder at most this often.
const DISPLAY_RESTART_GAP: std::time::Duration = std::time::Duration::from_secs(30);
/// How often a held display change is looked at again.
const DISPLAY_RECHECK: std::time::Duration = std::time::Duration::from_secs(10);

/// Whether a display change restarts the recorder now: `running` and `now` are what the recorder
/// took from the monitors when started and what it would take now, `since_restart` how long ago a
/// display change last restarted it, `game` whether a game is in front (asked last, as it costs most).
fn display_step(running: &Option<recorder::DisplayKey>, now: &Option<recorder::DisplayKey>, recording: bool, since_restart: Option<std::time::Duration>, game: impl FnOnce() -> bool) -> DisplayStep {
    if recording {
        return DisplayStep::Hold(DisplayHold::Recording);
    }
    if running == now {
        return DisplayStep::Unchanged;
    }
    if let Some(since) = since_restart.filter(|s| *s < DISPLAY_RESTART_GAP) {
        return DisplayStep::Hold(DisplayHold::TooSoon(DISPLAY_RESTART_GAP - since));
    }
    if game() { DisplayStep::Hold(DisplayHold::Game) } else { DisplayStep::Restart }
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

/// Show / hide key presses counted on the hotkey thread, for the UI thread to act on. Showing or
/// hiding the window from another thread waits on the UI thread, which may itself be waiting on
/// the hotkey thread (`hotkey::set_combo`), so the hotkey thread never does. A minimized or hidden
/// window still runs `App::logic` when asked for a repaint, and a playing clip is paused in `ui`
/// once the window is hidden.
static TOGGLE_PRESSES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Whether the presses since the last pass leave the window toggled: each press toggles, so quick
/// show then hide ends where it started.
fn toggles(presses: u32) -> bool {
    presses % 2 == 1
}

const STEPS: [&str; 3] = ["Your clips", "This PC", "You're ready"];

/// Stores the look in `cfg` for the theme to draw with, and returns it (to see when it changes).
fn store_look(cfg: &Config) -> (u8, u32, bool, bool) {
    let custom = parse_hex(&cfg.custom_accent).map_or(0x8448F0, |c| u32::from_be_bytes([0, c.r(), c.g(), c.b()]));
    let look = (cfg.accent, custom, cfg.theme == "retro", cfg.light_mode);
    ACCENT.store(look.0, std::sync::atomic::Ordering::Relaxed);
    CUSTOM.store(look.1, std::sync::atomic::Ordering::Relaxed);
    RETRO.store(look.2, std::sync::atomic::Ordering::Relaxed);
    LIGHT.store(look.3, std::sync::atomic::Ordering::Relaxed);
    EFFECTS.store(!cfg.reduce_effects, std::sync::atomic::Ordering::Relaxed);
    look
}

/// The save popup's accent bar and VCR face, from the look stored last.
fn toast_look() {
    *toast::BAR.lock().unwrap() = [accent().r(), accent().g(), accent().b()];
    toast::VCR.store(ui::theme::look().vcr_popup, std::sync::atomic::Ordering::Relaxed);
}

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

        // A start in the tray saves clips before the window ever draws, so the popups need the look now.
        store_look(&cfg);
        toast_look();
        toast::start();
        let clips_root = std::path::PathBuf::from(&cfg.folder);
        let archive = (cfg.archive_days > 0 && !cfg.archive_folder.trim().is_empty()).then(|| std::path::PathBuf::from(cfg.archive_folder.trim()));
        std::thread::spawn(move || covers::tidy(&clips_root, archive.as_deref()));
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
        let hotkey_cmds = cmd_tx.clone();
        let hook = hotkey::install(move |action| {
            let cmd = match action {
                hotkey::Action::Overlay => {
                    TOGGLE_PRESSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    ctx.request_repaint();
                    return;
                }
                hotkey::Action::Record => Cmd::ToggleRecord,
                hotkey::Action::Screenshot => Cmd::Screenshot,
                hotkey::Action::Replay => Cmd::ToggleReplay,
                hotkey::Action::Mic => Cmd::ToggleMic,
                hotkey::Action::SaveKey => Cmd::save_key_pressed(),
            };
            let _ = hotkey_cmds.send(cmd);
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
            startup_turned_off: startup::turned_off_in_task_manager(),
            // Setup needs the window, even at a Windows-startup launch.
            minimized: minimized && !needs_setup,
            tray,
            wizard: needs_setup.then_some(0),
            obs_exe,
            log_checked: std::time::Instant::now() - std::time::Duration::from_secs(2),
            setup_unsaved: None,
            recommended,
            obs_settings_found: obs_import::found(),
            links: upload::links(),
            favorites: clips::favorites(),
            tags: clips::tags(),
            notes: clips::notes(),
            lost: clips::lost(),
            feedback: feedback::Form::load(),
            ..Self::base(cfg, cmd_tx, ev_rx, hw, previews)
        };
        if needs_setup {
            app.status.clear();
        }
        // Once: stars an earlier clips folder kept from before they followed a change of folder.
        clips::adopt_stray_stars(&mut app.lost, &app.favorites, &app.cfg.folder, &app.cfg.archive_folder);
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
        // Logged already, with where the damaged file's copy went. The popup too, as the status
        // line soon shows the recorder's and a start at sign-in stays in the tray.
        if let Some(notice) = config::take_notice() {
            app.status = notice;
            toast::show(toast::Kind::Warning, "Settings file was damaged", "Open Replay Vault to check your settings".into());
        }
        if let Some(e) = tray_error {
            app.status = format!("Couldn't create the tray icon: {e}");
        }
        app.refresh_devices();
        // Display changes from the monitors listed above on (`follow_displays`).
        let ctx = cc.egui_ctx.clone();
        display::watch(&app.monitors, move || ctx.request_repaint());
        // Persists the generated recorder password (and a new user's recommended settings). If
        // that fails (the status says so), the app still runs with them.
        if !app.write_settings() {
            app.saved = app.cfg.clone();
        }
        // The keys as saved, now that they are.
        match hook {
            Ok(()) => app.sync_hotkeys(),
            Err(e) => app.hotkey_error = Some(format!("Couldn't start listening for keys: {e}")),
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
            startup_turned_off: false,
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
            hotkey_retry: HOTKEY_RETRY,
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
            setup_unsaved: None,
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
            clip_games: Default::default(),
            clips_checked: std::time::Instant::now(),
            rescan_now: true,
            archive_list: None,
            archive_scan: None,
            clips_scan: None,
            clip_moves: 0,
            clips_free: None,
            clips_read: Vec::new(),
            archive_checked: std::time::Instant::now(),
            previews,
            textures: Default::default(),
            clip_cards: Default::default(),
            covers: Default::default(),
            lengths: Default::default(),
            scrub_wait: None,
            trim: None,
            clips_msg: None,
            player: None,
            opening: None,
            view: ClipsView::All,
            links: Default::default(),
            uploads: Default::default(),
            link_cleanup: Default::default(),
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
            lost: Default::default(),
            unfiled: false,
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
            recycle_next: None,
            export_queue: Default::default(),
            search: String::new(),
            montage: None,
            folder_risk: None,
            cleanup_preview: None,
            archive_preview: None,
            perf: None,
            encoder_hint: None,
            slim: Slim::Visible,
            mic_muted: false,
            display_wait: None,
            display_restarted: None,
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
                Event::MicMuted(on) => self.mic_muted = on,
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
            self.refresh_startup();
        }
        if let Some((_, menu)) = &mut self.tray {
            let status = match (self.connected, self.buffer) {
                (true, true) => format!("Instant replay: last {}", mmss(self.saved.buffer_secs)),
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

    /// Monitors changed (`display::changed`): the app shows them as they are now, and the recorder
    /// is restarted, the way Save does, when what it takes from them changed (`display_step`):
    /// never during a full recording or while a game is in front, and at most every 30 s.
    fn follow_displays(&mut self, ctx: &egui::Context) {
        if let Some(now) = display::changed() {
            let running = recorder::display_key(&self.saved, &self.monitors);
            self.monitors = now;
            // During setup the recorder isn't running; it starts with the monitors as they are.
            // A change the recorder doesn't care about (another monitor) needs nothing more.
            if self.wizard.is_none() && self.display_wait.is_none() && recorder::display_key(&self.saved, &self.monitors) != running {
                self.display_wait = Some(DisplayWait { running, next: std::time::Instant::now(), logged: None });
            }
        }
        let Some(wait) = &self.display_wait else { return };
        // A recording's end comes as an event, which repaints; other holds are looked at again later.
        if !self.recording && std::time::Instant::now() < wait.next {
            return;
        }
        let now = if self.recording { None } else { recorder::display_key(&self.saved, &self.monitors) };
        let folder = std::path::Path::new(&self.saved.folder);
        let step = display_step(&wait.running, &now, self.recording, self.display_restarted.map(|t| t.elapsed()), || autopause::game_in_front(folder));
        match step {
            DisplayStep::Restart => {
                log::write("The display changed (a monitor, its resolution or HDR); restarting the recorder to match.");
                self.display_restarted = Some(std::time::Instant::now());
                self.apply();
            }
            DisplayStep::Unchanged => {
                if wait.logged.is_some() {
                    log::write("The display is back as the recorder has it; no restart needed.");
                }
                self.display_wait = None;
            }
            DisplayStep::Hold(hold) => {
                let again = match hold {
                    DisplayHold::TooSoon(left) => left,
                    _ => DISPLAY_RECHECK,
                };
                if wait.logged != Some(hold) && !matches!(hold, DisplayHold::TooSoon(_)) {
                    log::write(match hold {
                        DisplayHold::Recording => "The display changed; the recorder restarts to match once the full recording is stopped.",
                        _ => "The display changed; the recorder restarts to match once no game is in front.",
                    });
                }
                if let Some(wait) = &mut self.display_wait {
                    if !matches!(hold, DisplayHold::TooSoon(_)) {
                        wait.logged = Some(hold);
                    }
                    if hold != DisplayHold::Recording {
                        wait.next = std::time::Instant::now() + again;
                        ctx.request_repaint_after(again);
                    }
                }
            }
        }
    }

    /// (Re)starts the recorder with the saved settings.
    fn apply(&mut self) {
        // It starts with the monitors as they are now.
        self.display_wait = None;
        self.applied = recorder_settings(&self.saved);
        let _ = self.cmds.send(Cmd::Start(Box::new(self.saved.clone())));
        // OBS saves on its own key; the app notices the key too, to check the clip lands.
        if let Err(e) = hotkey::watch_save(hotkey::Combo::parse(&self.saved.hotkey).filter(|c| c.obs_binding().is_some())) {
            log::write(&format!("Can't notice the save key: {e}"));
        }
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
        // Higher video settings can leave the clip length past what fits in memory.
        self.cap_length();
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
    /// read, and a recorder restart when a setting it runs with changed (not during setup). A copy
    /// that may not change Windows settings skips Game Bar, NVIDIA and AMD, so nothing is logged
    /// as done that wasn't.
    fn put_in_effect(&mut self, before: Option<&Config>) {
        let start = before.is_none();
        let before = before.cloned().unwrap_or_default();
        let s = &self.saved;
        if s.game_bar_off != before.game_bar_off
            && tweaks::shared_allowed()
            && let Err(e) = tweaks::set_game_bar_off(s.game_bar_off) {
            log::write(&format!("Couldn't change Game Bar: {e:#}"));
            if !start {
                self.status = format!("Couldn't change Game Bar: {e:#}");
            }
        }
        // At every start too, in case an NVIDIA update switched it back on.
        if s.nvidia_off != before.nvidia_off && tweaks::shared_allowed() {
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
        if amd_off != before.amd_off && tweaks::shared_allowed() && (!amd_off || amd::detect().is_some()) {
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
        // Before cleanup gets the new folder: stars there must be filed under it first. At start,
        // also when it changed outside Save (settings edited or restored, or a crash before they
        // followed): from the folder they were last filed against.
        if start {
            match clips::refile_at_start(clips::filed().as_deref(), &self.saved.folder) {
                clips::Refile::Nothing => {}
                clips::Refile::Record => clips::save_filed(&self.saved.folder),
                clips::Refile::From(old) => self.refolder(&old),
            }
        } else if before.folder != self.saved.folder {
            self.refolder(&before.folder);
        }
        let s = &self.saved;
        obs::SAVE_POPUP.store(s.save_popup, std::sync::atomic::Ordering::Relaxed);
        obs::SORT_BY_GAME.store(s.sort_by_game, std::sync::atomic::Ordering::Relaxed);
        obs::SAVE_SOUND.store(s.save_sound, std::sync::atomic::Ordering::Relaxed);
        obs::CENTER_SOUND.store(s.center_sound, std::sync::atomic::Ordering::Relaxed);
        clips::set_cleanup(&s.folder, &s.shared_folder_ok, cleanup_rule(s));
        clips::set_archive(&s.folder, &s.shared_folder_ok, archive_rule(s));
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

    /// Left side: the name, Clips with your games under it, the settings pages, and at the bottom
    /// the small links and the status box.
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        // A hairline on its right edge. The whole panel, margins too.
        let panel = ui.max_rect().expand2(egui::vec2(12.0, 0.0));
        let back = ui.ctx().layer_painter(ui.layer_id()).with_clip_rect(panel);
        back.vline(panel.right() - 0.5, panel.y_range(), egui::Stroke::new(1.0, pal().border));
        // The heights as laid out before, per theme (it sets them): the games between Clips and
        // Settings get the room that's left, so the status box always stays on screen. A window
        // too short for the usual spacing draws compact; that's decided from the usual spacing's
        // heights and the bottom's (the same either way), never from the compact one's, so it
        // can't flip between frames.
        let heights_id = ui.scope_id().with(("side heights", self.cfg.theme.as_str()));
        let seen = ui.data(|d| d.get_temp::<SideHeights>(heights_id)).unwrap_or(SideHeights { fixed: [None; 2], bottom: 190.0 });
        let compact = seen.fixed[0].is_some_and(|(head, settings)| head + settings + seen.bottom > ui.available_height());
        let nav = if compact { look().nav.compact } else { look().nav.normal };
        let top = ui.cursor().top();
        ui.add_space(nav.brand);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            brand(ui);
        });
        ui.add_space(nav.brand);
        // Rows sit close, like a list; tapes keep their room.
        let row_gap = nav.gap.unwrap_or(ui.spacing().item_spacing.y);
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = row_gap;
            self.nav_item(ui, Tab::Clips, nav.height);
        });
        let head = ui.cursor().top() - top;
        let settings_h = seen.fixed[compact as usize].or(seen.fixed[0]).map_or(200.0, |(_, settings)| settings);
        // Less the gap under the list.
        let room = ui.available_height() - settings_h - seen.bottom - ui.spacing().item_spacing.y;
        let games = std::rc::Rc::clone(&self.clip_games);
        self.side_games(ui, room, &games);
        let top = ui.cursor().top();
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = row_gap;
            ui.add_space(nav.after_clips);
            side_heading(ui, "Settings");
            ui.add_space(2.0);
            for tab in Tab::SETTINGS {
                self.nav_item(ui, tab, nav.height);
            }
        });
        let settings = ui.cursor().top() - top;
        ui.add_space((ui.available_height() - seen.bottom).max(0.0));
        let top = ui.cursor().top();
        self.side_links(ui);
        self.side_status(ui);
        let mut heights = SideHeights { fixed: seen.fixed, bottom: ui.cursor().top() - top };
        heights.fixed[compact as usize] = Some((head, settings));
        if heights != seen {
            ui.data_mut(|d| d.insert_temp(heights_id, heights));
            ui.ctx().request_repaint();
        }
    }

    /// Under Clips: All games, each game (played last first) and Not sorted, with how many clips
    /// each has; a click shows just those on the Clips page. Scrolls inside `room` when there are
    /// many, and isn't shown when not even one row fits.
    fn side_games(&mut self, ui: &mut egui::Ui, room: f32, games: &[Game]) {
        if games.is_empty() || room < SIDE_ROW {
            return;
        }
        // Not sorted only beside real games: on its own it's the same as All games.
        let sorted = games.iter().any(|g| g.name.is_some());
        let rows = std::iter::once((ClipsView::All, "All games", self.clip_list.len(), None))
            .chain(games.iter().filter(|g| g.name.is_some()).map(|g| (ClipsView::Game(g.name.clone()), g.name.as_deref().unwrap_or_default(), g.count, tape_color(g.tape))))
            .chain(games.iter().filter(|g| g.name.is_none() && sorted).map(|g| (ClipsView::Game(None), "Not sorted", g.count, tape_color(g.tape))));
        egui::ScrollArea::vertical().id_salt("side games").auto_shrink([false, true]).max_height(room).min_scrolled_height(SIDE_ROW).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (view, name, n, tape) in rows {
                self.side_game(ui, view, name, n, tape);
            }
        });
    }

    /// One row under Clips: the name (cut with "...", all of it on hover) and its clip count, on a
    /// soft surface while it's the filter shown; a small mark in its tape color, if it has one.
    fn side_game(&mut self, ui: &mut egui::Ui, view: ClipsView, name: &str, n: usize, tape: Option<Color32>) {
        let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), SIDE_ROW), egui::Sense::click());
        let open = self.tab == Tab::Clips && self.view == view;
        // Painted below, so say what it is for screen readers (and tests that find it by name).
        r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, open, name));
        if open || r.hovered() {
            ui.painter().rect_filled(rect, R_SMALL, pal().card);
        }
        let (face, size) = look().side_row;
        let count = ui.painter().layout_no_wrap(n.to_string(), face.font(size), pal().muted);
        // Lined up with the page names above it.
        let (x, right) = (rect.left() + 38.0, rect.right() - 10.0);
        if let Some(c) = tape {
            ui.painter().rect_filled(egui::Rect::from_center_size(egui::pos2(x - 10.0, rect.center().y), egui::vec2(4.0, 12.0)), 1.0, c);
        }
        let color = if open { pal().text } else { pal().muted.lerp_to_gamma(pal().text, 0.5) };
        let mut job = egui::text::LayoutJob::simple_singleline(name.into(), face.font(size), color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(right - count.size().x - 8.0 - x);
        let galley = ui.painter().layout_job(job);
        let cut = galley.elided;
        ui.painter().galley(egui::pos2(x, rect.center().y - galley.size().y / 2.0), galley, color);
        ui.painter().galley(egui::pos2(right - count.size().x, rect.center().y - count.size().y / 2.0), count, pal().muted);
        let tip = match &view {
            ClipsView::All => "Shows every clip".to_string(),
            ClipsView::Game(None) => "Shows the clips that aren't in a game's folder".to_string(),
            _ if cut => format!("{name}: shows this game's clips"),
            _ => "Shows this game's clips".to_string(),
        };
        if r.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            self.tab = Tab::Clips;
            self.player = None;
            self.view = view;
        }
    }

    /// Help, What's new and Feedback as a row of small links, the open one marked.
    fn side_links(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            ui.spacing_mut().item_spacing.x = 14.0;
            let (face, size, caps) = look().side_links;
            for tab in Tab::LINKS {
                let (name, _, _, hint) = tab.info();
                let open = self.tab == tab;
                let text = RichText::new(if caps { name.to_uppercase() } else { name.to_string() }).family(face.family()).size(size).color(if open { pal().text } else { pal().muted });
                let r = ui.add(egui::Label::new(if open { text.underline() } else { text }).selectable(false).sense(egui::Sense::click()));
                // A tab to screen readers (and tests that find it by name), like the pages above.
                r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, open, name));
                if r.on_hover_text(hint).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    self.tab = tab;
                }
            }
        });
        ui.add_space(8.0);
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

    /// The bottom of the sidebar: "Version X is out" when there is one, then the status box: the
    /// Instant replay switch, a line with a light (red while recording, amber while paused) saying
    /// what a save keeps or else why it isn't recording, a key clash if there is one, and Save clip.
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
        let sbox = look().status_box;
        let fill = if sbox.display { pal().display } else { pal().card };
        let frame = egui::Frame::new().fill(fill).stroke(egui::Stroke::new(1.0, pal().border)).corner_radius(sbox.radius).inner_margin(12);
        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.horizontal(|ui| {
                // The VCR's display says whether it's on, in its green.
                if sbox.display {
                    let (face, size) = look().status;
                    let (text, color) = if self.buffer { ("REPLAY ON", pal().phosphor) } else { ("REPLAY OFF", pal().muted) };
                    ui.label(RichText::new(text).family(face.family()).size(size).color(color)).on_hover_text("Instant replay");
                } else {
                    ui.label(RichText::new("Instant replay").color(pal().text));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut on = self.buffer;
                    if ui.add_enabled(self.connected, toggle(&mut on)).on_hover_text("Turn instant replay on or off").changed() {
                        let _ = self.cmds.send(Cmd::SetReplay(on));
                    }
                });
            });
            self.status_line(ui);
            if self.mic_muted {
                let line = egui::Label::new(RichText::new("Mic muted").small().color(pal().amber)).sense(egui::Sense::click());
                let key = &self.saved.hotkey_mic;
                let hover = |ui: &mut egui::Ui| {
                    ui.label(if key.is_empty() { "Your mic isn't in clips or recordings.\nClick to unmute it.".to_string() } else { format!("Your mic isn't in clips or recordings.\nClick to unmute it, or press {key}.") });
                };
                if ui.add(line).on_hover_ui(hover).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    let _ = self.cmds.send(Cmd::ToggleMic);
                }
            }
            // A key another program took since the keys were set (Keybinds says who and what to
            // do), so it isn't first noticed when a save does nothing.
            self.check_conflicts();
            if let Some((target, text)) = self.conflicts.first().cloned() {
                let more = if self.conflicts.len() > 1 { format!(" (+{} more)", self.conflicts.len() - 1) } else { String::new() };
                let line = egui::Label::new(RichText::new(format!("Key clash: {}{more}", target.name())).small().color(pal().amber)).truncate().sense(egui::Sense::click());
                if ui.add(line).on_hover_text(format!("{}: {text}\nClick to open Keybinds.", target.name())).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    self.tab = Tab::Keybinds;
                }
            }
            self.save_button(ui);
        });
        ui.add_space(14.0);
    }

    /// The status box's line: "Keeping the last 5:00" while recording, otherwise why it isn't
    /// (paused with no game open, replay off, the recorder not running) on up to two lines, all of
    /// it on hover. The light in front keeps REC (red), PAUSE (amber) and NO SIGNAL (red) apart.
    fn status_line(&mut self, ui: &mut egui::Ui) {
        let paused = self.connected && autopause::PAUSED.load(std::sync::atomic::Ordering::Relaxed);
        let keeping = self.connected && self.buffer;
        let (dot, text, color) = match (self.connected, self.buffer) {
            (true, true) if look().status_box.display => (RED, format!("REC  ·  LAST {}", mmss(self.saved.buffer_secs)), pal().text),
            (true, true) => (RED, format!("Keeping the last {}", mmss(self.saved.buffer_secs)), pal().text),
            (true, false) if paused => (pal().amber, "Paused: no game open. Starts again when one is.".to_string(), pal().muted),
            (true, false) => (pal().amber, self.status.clone(), pal().muted),
            _ => (RED, self.status.clone(), pal().muted),
        };
        let (face, size) = look().status;
        let hover = if keeping { format!("Press {} anywhere, even in a game, to save the last {}.", self.saved.hotkey, mmss(self.saved.buffer_secs)) } else { self.status.clone() };
        ui.horizontal_top(|ui| {
            // The light, on the middle of the first line; a second line starts under the text.
            let first = ui.painter().layout_no_wrap("A".into(), face.font(size), color).size().y;
            let (light, _) = ui.allocate_exact_size(egui::vec2(8.0, first), egui::Sense::hover());
            ui.painter().circle_filled(light.center(), 3.5, dot);
            ui.spacing_mut().item_spacing.x = 6.0;
            let mut job = egui::text::LayoutJob::simple(text, face.font(size), color, ui.available_width());
            job.wrap.max_rows = 2;
            ui.add(egui::Label::new(job).wrap()).on_hover_text(hover);
        });
    }

    /// Save clip across the status box, with the save key on it in a small chip (left out when it
    /// doesn't fit); the same as pressing the key.
    fn save_button(&mut self, ui: &mut egui::Ui) {
        // The words and the key are painted over a main button without a label of its own.
        let r = primary(ui, self.buffer, RichText::new(""), egui::vec2(ui.available_width(), 0.0));
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, self.buffer, "Save clip"));
        let ink = if self.buffer { on_accent() } else { pal().muted };
        let p = ui.painter();
        // In the look's key text when it has one (SAVE CLIP and ALT+F10 on a VCR's keys).
        let (label, key) = match look().key_text {
            Some(k) => (p.layout_no_wrap("SAVE CLIP".into(), k.face.font(k.size), ink), p.layout_no_wrap(self.saved.hotkey.to_uppercase(), k.face.font(k.size - 2.0), ink)),
            None => (p.layout_no_wrap("Save clip".into(), egui::FontId::new(14.0, egui::FontFamily::Name("strong".into())), ink), p.layout_no_wrap(self.saved.hotkey.clone(), egui::FontId::proportional(11.0), ink)),
        };
        let (chip, gap) = (key.size() + egui::vec2(10.0, 4.0), 8.0);
        let fits = !self.saved.hotkey.is_empty() && label.size().x + gap + chip.x <= r.rect.width() - 16.0;
        let w = label.size().x + if fits { gap + chip.x } else { 0.0 };
        let (mut x, y) = (r.rect.center().x - w / 2.0, r.rect.center().y);
        p.galley(egui::pos2(x, y - label.size().y / 2.0), label.clone(), ink);
        if fits {
            x += label.size().x + gap;
            let chip = egui::Rect::from_min_size(egui::pos2(x, y - chip.y / 2.0), chip);
            p.rect_filled(chip, 4.0, if self.buffer { Color32::from_black_alpha(60) } else { pal().card });
            p.galley(chip.min + egui::vec2(5.0, 2.0), key, ink);
        }
        if r.on_hover_text(format!("Saves the last {}. Same as pressing {}.", mmss(self.saved.buffer_secs), self.saved.hotkey))
            .on_disabled_hover_text(if self.connected { "Instant replay is off: turn it on with the switch above" } else { "The recorder isn't running yet" })
            .clicked()
        {
            let _ = self.cmds.send(Cmd::Save);
        }
    }

    /// One page link in the sidebar. Retro: a tape on the site's shelf, a dark spine with a
    /// handwritten paper label and a colored stripe; the open page's tape is pulled out a little and
    /// outlined in its color. Minimalist: a quiet list row, the open page's marked with an accent bar.
    fn nav_item(&mut self, ui: &mut egui::Ui, tab: Tab, height: f32) {
        let (name, glyph, _, hint) = tab.info();
        let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click());
        let selected = self.tab == tab;
        // Painted below, so say what it is for screen readers (and tests that find it by name).
        r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, selected, name));
        let painter = ui.ctx().layer_painter(ui.layer_id());
        if look().nav.style == NavStyle::Tape {
            let stripe = tab.stripe();
            let pull = if selected { 10.0 } else if r.hovered() { 4.0 } else { 0.0 };
            let spine = egui::Rect::from_min_max(rect.min, egui::pos2(rect.right() - 10.0 + pull, rect.bottom())).shrink2(egui::vec2(0.0, 2.0));
            let label = spine.shrink2(egui::vec2(6.0, 5.0));
            if pal().light {
                // No dark spine in light mode: the cream label and its stripe carry it, on a soft
                // lip, and the open page's is outlined in its color.
                painter.rect_filled(label.translate(egui::vec2(0.0, 2.0)), 3.0, pal().lip);
                if selected {
                    painter.rect_stroke(label.expand(3.0), 5.0, egui::Stroke::new(2.0, stripe), egui::StrokeKind::Inside);
                }
            } else {
                painter.rect_filled(spine, 4.0, if selected { Color32::BLACK } else { Color32::from_rgb(8, 6, 11) });
                if selected {
                    painter.rect_stroke(spine, 4.0, egui::Stroke::new(2.0, stripe), egui::StrokeKind::Inside);
                }
            }
            painter.rect_filled(label, 3.0, if selected { PAPER } else { PAPER.lerp_to_gamma(pal().sidebar, 0.25) });
            painter.rect_filled(label.with_max_x(label.left() + 6.0), egui::CornerRadius { nw: 3, sw: 3, ne: 0, se: 0 }, stripe);
            let mut x = label.left() + 14.0;
            if paint_icon(&painter, egui::pos2(x, label.center().y), egui::Align2::LEFT_CENTER, glyph, 13.0, INK) {
                x += 22.0;
            }
            painter.text(egui::pos2(x, label.center().y), egui::Align2::LEFT_CENTER, name, Face::Marker.font(15.0), INK);
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
        // With checks off, only after "Check for updates" was clicked.
        app_update::found().is_some()
            && (self.saved.check_updates || app_update::status().is_some())
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
            // Cancelled: back to the popup as it was, the part downloaded thrown away.
            if r.as_ref().is_err_and(|e| e == obs_install::CANCELLED) {
                log::write(&format!("Cancelled the download of Replay Vault {}.", n.version));
                return;
            }
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
        let r = dialog(ctx, "update_popup").show(ctx, |ui| {
            ui.set_max_width(420.0);
            dialog_title(ui, &format!("Replay Vault {} is out", n.version));
            ui.label(RichText::new(format!("You have {}. Your settings and clips stay.", env!("CARGO_PKG_VERSION"))).color(pal().muted));
            if !n.notes.is_empty() {
                ui.add_space(4.0);
                egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| ui.label(&n.notes));
            }
            ui.add_space(8.0);
            if let Some(p) = &progress {
                ui.label(format!("Downloading: {} of {}", clips::size(p.done), clips::size(p.total)));
                ui.add(egui::ProgressBar::new(if p.total > 0 { p.done as f32 / p.total as f32 } else { 0.0 }));
                ui.add_space(4.0);
                let label = if p.cancel { "Cancelling..." } else { "Cancel" };
                if cap(ui, !p.cancel, egui::Button::new(label)).on_hover_text("Stops the download and throws away the part that came. This copy stays as it is.").clicked()
                    && let Some(job) = &self.update_job
                {
                    obs_install::cancel(job);
                }
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
            let set: std::collections::HashSet<&std::path::PathBuf> = paths.iter().collect();
            let bytes: u64 = self.clip_list.iter().filter(|c| set.contains(&c.path)).map(|c| c.bytes).sum();
            let title = format!("Move {} ({}) to the Recycle Bin?", count(paths.len(), "clip"), clips::size(bytes));
            match modal(ctx, "recycle_picked", 360.0, &title, confirm, CANCEL, |ui| {
                match paths.as_slice() {
                    [one] => ui.label(format!("{}\n\nYou can get it back from the Recycle Bin.", one.file_name().unwrap_or_default().to_string_lossy())),
                    _ => ui.label("You can get them back from the Recycle Bin."),
                };
                self.recycle_links_box(ui, &paths);
                (true, false)
            }) {
                Some(true) => self.recycle_picked(&paths),
                Some(false) => self.recycle_next = None,
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
                    self.uploads.insert(path.clone(), upload::post_discord(&path, &self.saved.discord_webhook, move || c.request_repaint()));
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
            let confirm = Confirm { label: "Import", hint: "Adds the stars and tags and fills in these settings; Save to use them. Passwords and the Discord webhook stay as they are here.", danger: false };
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
        obs::QUITTING.store(true, std::sync::atomic::Ordering::Relaxed);
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
    /// The bar that appears while settings are changed but not saved.
    fn footer(&mut self, ui: &mut egui::Ui) {
        let restart = recorder_settings(&self.cfg) != self.applied;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(strong("Unsaved changes", 14.0).color(pal().text));
                // A warning, so it always shows.
                if restart {
                    ui.label(RichText::new("Saving restarts the recorder, so what it kept so far is cleared.").small().color(pal().muted));
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

/// How soon a hotkey another app held is first tried again, and the longest wait between tries.
const HOTKEY_RETRY: std::time::Duration = std::time::Duration::from_secs(3);
const HOTKEY_RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(60);

/// The wait before the next try of a key still held after waiting `last`: twice as long, up to a
/// minute, so a key another program keeps doesn't wake the app every few seconds for good.
fn next_hotkey_retry(last: std::time::Duration) -> std::time::Duration {
    (last * 2).min(HOTKEY_RETRY_MAX)
}

/// Don't leave the hidden recorder running after the app closes, or NVIDIA's overlay off. The
/// window closing, the tray's Quit and Windows ending the session (`end_session`) can each get
/// here, on different threads: it's done once, and a second caller waits for that.
fn shutdown() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        mark_closing();
        obs::end_session("app closed");
        log::write("App closed.");
        obs::QUITTING.store(true, std::sync::atomic::Ordering::Relaxed);
        recorder::stop();
        tweaks::give_back_nvidia_overlay();
    });
}

/// Windows is ending the session, an installer is closing the app, or the uninstaller asked it to
/// quit (`session::watch`): finishes a recording and a clip being saved, as Quit does, then ends.
fn end_session(why: &str) {
    log::write(&format!("Closing: {why}."));
    shutdown();
    std::process::exit(0);
}

/// What the session window passes on, on its thread: the end of the session, or a look at the
/// monitors (a change there reaches the UI thread through `display::changed`).
fn session_notice(notice: session::Notice) {
    match notice {
        session::Notice::Quit(why) => end_session(why),
        session::Notice::Displays => display::look_again(),
    }
}

/// "1 clip", "2 clips": `n` and `one`, made plural with an s past 1.
fn count(n: usize, one: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {one}s") }
}

/// The height of a game's row under Clips in the sidebar.
const SIDE_ROW: f32 = 28.0;

/// The sidebar's heights as laid out (see `App::sidebar`): above the games (the name and Clips)
/// and the Settings links, with the usual spacing and compact, and the bottom (the small links
/// and the status box), which is the same either way.
#[derive(Clone, Copy, PartialEq)]
struct SideHeights {
    fixed: [Option<(f32, f32)>; 2],
    bottom: f32,
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

    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // The show / hide key, pressed on the hotkey thread. Here and not in `ui`: eframe runs
        // only `logic` for a minimized window, so the key would be dead there and the next
        // restore would hide the window. A visible or hidden window runs this just before `ui`.
        if toggles(TOGGLE_PRESSES.swap(0, std::sync::atomic::Ordering::Relaxed)) {
            overlay::toggle();
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
        // From the tray too: the recorder follows a display change while the window is hidden.
        self.follow_displays(ui.ctx());
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
            ui.ctx().request_repaint_after(self.hotkey_retry.saturating_sub(self.hotkeys_tried.elapsed()));
        }
        // Setup has no Save button: what's picked there is kept as it goes.
        if self.wizard.is_some() && self.cfg != self.saved {
            let wait = std::time::Duration::from_secs(2);
            let failed_now = self.setup_unsaved.as_ref().is_some_and(|(c, at)| *c == self.cfg && at.elapsed() < wait);
            if failed_now {
                ui.ctx().request_repaint_after(wait);
            } else {
                self.keep_settings();
                self.setup_unsaved = (self.cfg != self.saved).then(|| (self.cfg.clone(), std::time::Instant::now()));
            }
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
        app_update::ENABLED.store(self.saved.check_updates, std::sync::atomic::Ordering::Relaxed);
        autopause::ENABLED.store(self.saved.pause_without_game, std::sync::atomic::Ordering::Relaxed);
        // The look previews as it's picked, before Save. Kept per window (egui context), so a new
        // one gets it on its first frame too.
        let look = store_look(&self.cfg);
        let id = egui::Id::NULL.with("applied look");
        if ui.ctx().data(|d| d.get_temp(id)) != Some(look) {
            ui.ctx().data_mut(|d| d.insert_temp(id, look));
            set_visuals(ui.ctx());
            toast_look();
        }

        // In the tray, stay out of the game's way: eframe still runs this when something asks
        // for a repaint, so build nothing (nothing then asks for more), let go of the clip
        // thumbnails' GPU memory, and hand the app's idle memory back to Windows.
        // Windows' own answer: egui's `visible()` only knows minimized and covered, not hidden.
        let hidden = overlay::is_hidden();
        if hidden && self.capturing.is_none() {
            match self.slim {
                Slim::Visible => {
                    // However it was hidden (X, the tray, the key), a clip stops playing.
                    if let Some((_, p)) = &mut self.player {
                        p.pause();
                    }
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
        if self.slim != Slim::Visible {
            // Shown again: a key another app held is tried again soon, as the user may have just
            // freed it, and Start with Windows may have been turned off in Task Manager meanwhile.
            self.hotkey_retry = HOTKEY_RETRY;
            self.refresh_startup();
        }
        self.slim = Slim::Visible;

        // Closing (Alt+F4, X) only hides the overlay; Quit lives in the tray menu. During setup
        // nothing is recording yet, so closing quits instead of vanishing into the tray. Esc never
        // hides it; it only cancels or lets go of things.
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
        if !in_setup && close {
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
        self.scan_clips(ui.ctx());
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
        // Read before this frame's dialogs and text fields, so the Enter that closes the rename box
        // or leaves the search box doesn't also play a clip.
        let clip_keys = !self.dialog_open() && !ui.ctx().egui_wants_keyboard_input();
        self.dialogs(ui.ctx());
        let dialog_open = self.dialog_open();
        if let Some((_, p)) = &mut self.player {
            // A child window draws over everything, dialogs included.
            p.set_visible(!dialog_open);
        }
        let page = egui::Frame::new().fill(pal().bg).inner_margin(egui::Margin { left: 28, right: 28, top: 22, bottom: 12 });
        egui::CentralPanel::default().frame(page).show(ui, |ui| {
            room(ui, egui::Rect::from_min_max(ui.max_rect().min - egui::vec2(28.0, 22.0), ui.max_rect().max + egui::vec2(28.0, 12.0)));
            // An open clip takes the whole page: the video is a child window, so it can't scroll.
            if self.tab == Tab::Clips && self.player.is_some() {
                self.player_page(ui);
                return;
            }
            if self.tab == Tab::Clips {
                self.clips_header(ui);
            } else {
                // Same width as the settings column below.
                ui.scope(|ui| {
                    ui.set_max_width(ui.available_width().min(760.0));
                    retro_title(ui, self.tab.name(), 38.0);
                    ui.label(RichText::new(self.tab.info().2).color(pal().muted));
                    ui.add_space(14.0);
                });
            }
            // Cards on a page are striped in its tape's color.
            set_card_tape(ui.ctx(), self.tab.stripe());
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
                // At the bottom, out of the way of the first click.
                if let Some(page) = self.tab.page() {
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let restore = cap(ui, true, egui::Button::new(key_label("Restore defaults")));
                            key_named(&restore, true, "Restore defaults");
                            if restore.on_hover_text("Puts this page back the way a fresh install sets it up on this PC. Nothing changes until you Save.").clicked() {
                                self.restore_defaults(page);
                            }
                        });
                    });
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

    /// Every show / hide key press counts, even several before one pass: show then hide quickly
    /// ends where it started, a third press toggles again.
    #[test]
    fn show_hide_presses_toggle_on_an_odd_count() {
        assert!(!toggles(0));
        assert!(toggles(1));
        assert!(!toggles(2));
        assert!(toggles(3));
    }

    /// Copy A is closing; launches B and C both wait. Once A is gone and B has opened, C brings up
    /// B's window instead of waiting out A's deadline.
    #[test]
    fn launch_waits_for_a_closing_copy_then_brings_up_the_new_one() {
        let s = std::time::Duration::from_secs;
        // A's window is still there, but A is closing: wait.
        assert_eq!(next_turn(true, true, true, s(0)), Turn::Wait);
        assert_eq!(next_turn(true, false, true, s(10)), Turn::Wait);
        // A has gone and B is up: C brings it up, although it saw A closing.
        assert_eq!(next_turn(false, true, true, s(12)), Turn::BringUp);
        // The running copy, not closing, comes up on the very first pass.
        assert_eq!(next_turn(false, true, false, s(0)), Turn::BringUp);
        // B took the lock but has no window yet: keep waiting past 2 s, since a copy was closing.
        assert_eq!(next_turn(false, false, true, s(5)), Turn::Wait);
    }

    /// Why a launch gives up: a copy still closing after 30 s, or another copy after about 2 s.
    #[test]
    fn launch_gives_up_with_the_right_reason() {
        let s = std::time::Duration::from_secs;
        assert_eq!(next_turn(false, false, false, s(1)), Turn::Wait);
        assert_eq!(next_turn(false, false, false, s(2)), Turn::GiveUp(Busy::OtherCopy));
        assert_eq!(next_turn(true, false, true, s(29)), Turn::Wait);
        assert_eq!(next_turn(true, true, true, s(30)), Turn::GiveUp(Busy::StillClosing));
        assert_eq!(next_turn(false, false, true, s(30)), Turn::GiveUp(Busy::OtherCopy));
    }

    /// A key another program keeps is tried after 3 s, then twice as long each time, never more
    /// than a minute apart.
    #[test]
    fn held_keys_are_tried_less_often() {
        let s = std::time::Duration::from_secs;
        let waits: Vec<_> = std::iter::successors(Some(HOTKEY_RETRY), |w| Some(next_hotkey_retry(*w))).take(7).collect();
        assert_eq!(waits, [s(3), s(6), s(12), s(24), s(48), s(60), s(60)]);
    }

    impl App {
        /// The app with default settings and nothing running (no recorder, tray, hotkeys or
        /// window), for rendering its screens in tests. Never this PC's own settings, GPU or clips:
        /// the clips folder is an empty one of the tests' own, and tests that need clips set theirs.
        fn preview(cc: &eframe::CreationContext) -> App {
            apply_theme(&cc.egui_ctx);
            let (cmds, _) = mpsc::channel();
            let (_, events) = mpsc::channel();
            let ctx = cc.egui_ctx.clone();
            let folder = std::env::temp_dir().join("replay-vault-preview-clips");
            let _ = std::fs::create_dir_all(&folder);
            let cfg = Config { folder: folder.display().to_string(), ..Config::default() };
            let hw = profile::Hardware { gpu: recorder::Gpu::Nvidia, gpu_name: "NVIDIA GeForce RTX 4070".into(), vram_mb: 12288, ram_mb: 32768, integrated: false };
            let mut app = App {
                status: "Instant replay is on.".into(),
                connected: true,
                buffer: true,
                start_with_windows: true,
                ..App::base(cfg, cmds, events, hw, clips::Previews::new(move || ctx.request_repaint()))
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
    /// SCREENS_ONLY=general-cleanup,setup2 renders only those pages (names as in `pages`, or
    /// waveform, trim-parts, trim-gif).
    #[test]
    #[ignore]
    fn screens() {
        unsafe { std::env::set_var("WGPU_BACKEND", "dx12") };
        // SCREENS_OUT / SCREENS_FOLDER: render elsewhere, from another clips folder (for the website).
        let out = std::env::var("SCREENS_OUT").map(std::path::PathBuf::from).unwrap_or(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(r"target\screens"));
        let only: Option<Vec<String>> = std::env::var("SCREENS_ONLY").ok().map(|s| s.split(',').map(|n| n.trim().to_string()).collect());
        let wanted = |name: &str| only.as_ref().is_none_or(|o| o.iter().any(|n| n == name));
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
            .chain([(Tab::Clips, None, "clips-games".to_string()), (Tab::Clips, None, "clips-game".to_string()), (Tab::Clips, None, "clips-game-side".to_string())])
            .chain([(Tab::Clips, None, "clips-starred-only".to_string())])
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
            .chain([(Tab::General, None, "general-accent".to_string()), (Tab::Clips, None, "clips-selected-accent".to_string())])
            .chain([(Tab::Video, None, "video-hint".to_string())])
            .chain(["keybinds-nvidia", "keybinds-nvidia-off", "keybinds-amd", "keybinds-amd-off", "keybinds-both"].map(|n| (Tab::Keybinds, None, n.to_string())))
            .chain(["feedback-draft", "feedback-sent", "feedback-error", "feedback-limit"].map(|n| (Tab::Feedback, None, n.to_string())))
            .chain([(Tab::Audio, None, "audio-apps".to_string()), (Tab::Video, None, "video-follow".to_string())])
            .chain([(Tab::Video, None, "video-hdr".to_string())])
            // Each theme in each mode, and a custom accent.
            .chain(["minimalist-dark", "minimalist-light", "retro-dark", "retro-light"].into_iter().flat_map(|look| [(Tab::General, None, format!("general-{look}")), (Tab::Clips, None, format!("clips-{look}"))]))
            .chain([(Tab::General, None, "general-custom-color".to_string()), (Tab::General, None, "general-custom-color-light".to_string())])
            .chain((0..STEPS.len()).map(|s| (Tab::Clips, Some(s), format!("setup{}", s + 1))))
            .chain(["general-cleanup-shared", "general-cleanup-hold", "general-clips-shared"].map(|n| (Tab::General, None, n.to_string())))
            .chain([(Tab::Clips, Some(0), "setup-clips-shared".to_string())])
            .chain(["clips-muted", "clips-picked-recycle-links"].map(|n| (Tab::Clips, None, n.to_string())))
            .chain([(Tab::General, None, "general-sharing-orphans".to_string())])
            .chain([(Tab::General, None, "general-startup-taskmanager".to_string()), (Tab::Clips, None, "clips-update-cancel".to_string()), (Tab::Clips, None, "clips-opening".to_string())])
            .filter(|(_, _, n)| wanted(n));
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
            // A made-up newer release for the update popup, cleared before the next page.
            let _found = (name == "clips-update-cancel").then(|| {
                app_update::set_found(Some(nine()));
                ForgetFound
            });
            // Keeps a clip opening (see clips-opening) until the page is rendered.
            let mut _opening = None;
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
                let more = ["general-discord", "general-discord-bad", "general-archive", "general-move", "general-cleanup", "general-cleanup-shared", "general-cleanup-hold", "video-hint", "video-follow", "video-hdr", "audio-apps"];
                if name.ends_with("-more") || more.contains(&name.as_str()) {
                    app.cfg.more_open = ["General", "Keybinds", "Video", "Audio", "Feedback", "Help", "What's new"].map(String::from).to_vec();
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
                // General opens on the tab the render is about.
                app.general_part = match name.as_str() {
                    n if n.starts_with("general-discord") || n == "general-sharing-orphans" => GeneralPart::Sharing,
                    n if n == "general-archive" || n.starts_with("general-cleanup") => GeneralPart::Storage,
                    n if n == "general-move" || n.starts_with("general-import") => GeneralPart::Backup,
                    n if n == "general-accent" || n.starts_with("general-custom-color") => GeneralPart::Appearance,
                    "general-startup-taskmanager" => GeneralPart::Startup,
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
                // Stars the second-newest clip (in memory only; favorites.json isn't written), then
                // shows only the starred ones.
                if name.starts_with("clips-starred") {
                    app.favorites = clips::list(&app.cfg.folder, 2).iter().skip(1).map(|c| c.path.display().to_string()).collect();
                }
                if name == "clips-starred-only" {
                    app.view = ClipsView::Starred;
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
                    if name.starts_with("clips-picked-recycle") {
                        app.confirm_recycle = Some(app.picked.clone());
                    }
                    // A made-up share link on the first one (nothing is uploaded or deleted).
                    if name == "clips-picked-recycle-links"
                        && let Some(p) = app.picked.first()
                    {
                        app.links.insert(p.display().to_string(), upload::Link { link: "https://kappa.lol/made-up".into(), key: "made-up".into() });
                    }
                }
                // The mic muted, as after the mic key (only shown; the recorder isn't touched).
                if name == "clips-muted" {
                    app.mic_muted = true;
                }
                // Made-up share links of two clips gone from the clips folder (nothing is deleted).
                if name == "general-sharing-orphans" {
                    for n in ["VALORANT 2026-09-14 21-07-33.mp4", "Elden Ring 2026-09-02 23-41-10.mp4"] {
                        let gone = std::path::Path::new(&app.cfg.folder).join(n).display().to_string();
                        app.links.insert(gone.clone(), upload::Link { link: "https://kappa.lol/made-up".into(), key: "made-up".into() });
                        app.link_cleanup.orphans.push(gone);
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
                // Cleanup and the archive on in a clips folder judged shared (made up: the risk is
                // put in its cache, so the demo folder is still the one listed).
                if name == "general-cleanup-shared" {
                    (app.cfg.keep_days, app.cfg.archive_days, app.cfg.archive_folder) = (30, 30, r"Q:\Archive".into());
                    app.saved = app.cfg.clone();
                }
                if name.ends_with("-shared") {
                    app.folder_risk = Some(((app.cfg.folder.clone(), app.cfg.shared_folder_ok.clone()), Some("your Videos folder".into())));
                }
                // Cleanup on hold for a starred clip from an earlier clips folder (in memory only).
                if name == "general-cleanup-hold" {
                    app.cfg.keep_days = 30;
                    app.saved = app.cfg.clone();
                    let lost = r"Q:\Old clips\a.mp4".to_string();
                    app.favorites.insert(lost.clone());
                    app.lost.insert(lost);
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
                // Start with Windows turned off in Task Manager (made up; the registry isn't read,
                // as it's only read again when the window is shown or the box is ticked).
                if name == "general-startup-taskmanager" {
                    app.startup_turned_off = true;
                }
                // The new version downloading, with Cancel (made up; nothing is downloaded).
                if name == "clips-update-cancel" {
                    let progress = obs_install::Progress { stage: "Downloading".into(), done: 38 << 20, total: 92 << 20, ..Default::default() };
                    app.update_job = Some(Arc::new(std::sync::Mutex::new(progress)));
                }
                // The newest clip opening (nothing is read: no answer comes while the sender is kept).
                if name == "clips-opening"
                    && let Some(c) = clips::list(&app.cfg.folder, 1).first()
                {
                    _opening = Some(app.opening_unanswered(&c.path));
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
                // The first card's menu open on Export for sharing (the header's menu comes first).
                if name == "clips-export-menu" {
                    use egui_kittest::kittest::Queryable;
                    if i == 30 {
                        h.query_all_by_label(&icon::MORE.to_string()).nth(1).unwrap().click();
                    }
                    if i == 35 {
                        h.get_by_label("Export for sharing").hover();
                    }
                }
                // Scrolled down to the Discord card.
                if (name.starts_with("general-discord") || name == "general-archive" || name == "general-move" || name == "general-accent" || name.starts_with("general-custom-color")) && i == 30 {
                    use egui_kittest::kittest::Queryable;
                    let label = match name.as_str() {
                        "general-move" => "Export settings...",
                        "general-accent" => "Accent color",
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
                // Setup scrolled down to the note under its clips folder.
                if name == "setup-clips-shared" && i == 30 {
                    use egui_kittest::kittest::Queryable;
                    h.query_all_by_label_contains("This is your Videos folder").next().unwrap().scroll_to_me();
                }
                // A game picked from its row in the sidebar.
                if name == "clips-game-side" && i == 30 {
                    use egui_kittest::kittest::Queryable;
                    h.get_by_role_and_label(egui::Role::Tab, "VALORANT").click();
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
                        h.query_all_by_label(&icon::MORE.to_string()).nth(1).unwrap().click();
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
        if wanted("waveform") {
            let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_size([size[0] - 260.0, 100.0]).wgpu().build_ui(move |ui| {
                apply_theme(ui.ctx());
                let (mut view, mut start, mut end) = ((0.0, 30.0), 6.0, 18.0);
                egui::Frame::new().fill(pal().card).inner_margin(12).show(ui, |ui| {
                    range_bar(ui, 30.0, &mut view, &mut start, &mut end, 8.0, false, &wave, &media::loud_moments(&wave), &[]);
                });
            });
            h.step();
            h.render().unwrap().save(out.join("waveform.png")).unwrap();
        }
        // Two parts kept (green) and a third being picked, with their chips and the save button.
        if wanted("trim-parts") {
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
        }
        // Save as GIF: a 4 s part (ready), the whole 30 s (too long), and one being made.
        if wanted("trim-gif") {
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
                    app.cfg.more_open = ["General", "Keybinds", "Video", "Audio", "Feedback", "Help", "What's new"].map(String::from).to_vec();
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
        let open_all = |app: &mut App| app.cfg.more_open = ["General", "Keybinds", "Video", "Audio", "Feedback", "Help", "What's new"].map(String::from).to_vec();
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
            // On a VCR remote's key and on the status box as the VCR's display.
            for (name, c) in [("text", p.text), ("muted", p.muted), ("phosphor", p.phosphor), ("amber", p.amber)] {
                for (on, s) in [("key", p.key), ("display", p.display)] {
                    assert!(contrast(c, s) >= 4.5, "{name} on {on} (light {}) is {}", p.light, contrast(c, s));
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
        (c.nvidia_off, c.game_bar_off, c.accent) = (true, true, 3);
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
        let item = "The sidebar, the Clips page and the settings pages have a cleaner layout, and the Retro theme now reaches every screen.";
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

    /// Clears the found release when dropped, so a failing test doesn't leave it set.
    struct ForgetFound;

    impl Drop for ForgetFound {
        fn drop(&mut self) {
            app_update::set_found(None);
        }
    }

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

    /// The sidebar link opens the new-version popup with what's new; the exe tests run from isn't
    /// installed, so it updates itself from the release's exe: Install now, not the download
    /// page. Later closes it.
    #[test]
    fn new_version_popup() {
        use egui_kittest::kittest::Queryable;
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

        // At the bottom of the page now: scrolled to first, as a person would.
        h.get_by_label("Restore defaults").scroll_to_me();
        h.run();
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
        // Delete on one clip asks too, and the highlight stays on it until it's gone.
        h.key_press(egui::Key::ArrowRight);
        h.run_steps(2);
        let one = h.state().selected.clone();
        h.key_press(egui::Key::Delete);
        h.run_steps(2);
        assert_eq!(h.state().confirm_recycle.as_ref().map(Vec::len), Some(1));
        assert_eq!(h.state().selected, one, "still on the clip asked about");
        assert!(h.state().recycle_next.is_some() && h.state().recycle_next != one, "the next card waits");
        h.state_mut().confirm_recycle = None;
        h.run_steps(2);
        // And on one clicked (below its picture, which would open it): the click focuses the card.
        h.state_mut().selected = None;
        egui_kittest::kittest::Queryable::get_by_label(&h, "b").click();
        h.run_steps(2);
        assert_eq!(h.state().selected.as_ref(), Some(&dir.join("b.png")), "the click picks it");
        h.key_press(egui::Key::Delete);
        h.run_steps(2);
        assert_eq!(h.state().confirm_recycle.as_ref().map(Vec::len), Some(1), "Delete after a click");
        h.state_mut().confirm_recycle = None;
        h.run_steps(2);
        // Ctrl+click on two pictures (just above their names), then Delete asks for both. From no
        // selection: Delete moved it to the next card, which a Ctrl+click would pick too.
        h.state_mut().selected = None;
        for name in ["a", "c"] {
            let at = egui_kittest::kittest::Queryable::get_by_label(&h, name).rect().center_top() - egui::vec2(0.0, 30.0);
            h.event(egui::Event::ModifiersChanged(egui::Modifiers::COMMAND));
            h.event(egui::Event::PointerMoved(at));
            for pressed in [true, false] {
                h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::COMMAND });
            }
            h.run_steps(2);
        }
        h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        h.run_steps(1);
        assert_eq!(h.state().picked.len(), 2, "Ctrl+click picks");
        h.key_press(egui::Key::Delete);
        h.run_steps(2);
        assert_eq!(h.state().confirm_recycle.as_ref().map(Vec::len), Some(2), "Delete after Ctrl+clicks");
        h.state_mut().confirm_recycle = None;
        h.run_steps(2);
        // With a game picked too (here Not sorted): a click on a clip there, then Delete.
        (h.state_mut().view, h.state_mut().selected) = (ClipsView::Game(None), None);
        h.state_mut().picked.clear();
        h.run_steps(2);
        egui_kittest::kittest::Queryable::get_all_by_label(&h, "b").next().unwrap().click();
        h.run_steps(2);
        assert_eq!(h.state().selected.as_ref(), Some(&dir.join("b.png")), "picked with a game picked");
        h.key_press(egui::Key::Delete);
        h.run_steps(2);
        assert_eq!(h.state().confirm_recycle.as_ref().map(Vec::len), Some(1), "Delete with a game picked");
        h.state_mut().confirm_recycle = None;
        h.run_steps(2);
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

    /// The games under Clips follow the folder: a renamed clip stays counted, a new game's folder
    /// shows up, and a game whose last clip is gone leaves the list. In a temp folder.
    #[test]
    fn games_follow_the_clips_folder() {
        let dir = game_folder("instant-replay-games-follow");
        let folder = dir.display().to_string();
        let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_accessibility_check(false).build_eframe(|cc| {
            let mut app = App::preview(cc);
            (app.tab, app.cfg.folder) = (Tab::Clips, folder);
            app.saved.folder = app.cfg.folder.clone();
            app
        });
        h.run_steps(2);
        // In name order: the files are about as new as each other.
        let names = |app: &App| app.clip_games.iter().map(|g| (g.name.clone(), g.count)).collect::<std::collections::BTreeSet<_>>();
        assert_eq!(names(h.state()), [(None, 1), (Some("VALORANT".into()), 1)].into());
        h.state_mut().rename_clip(&dir.join(r"VALORANT\a.png"), "c");
        std::fs::create_dir_all(dir.join("Elden Ring")).unwrap();
        std::fs::write(dir.join(r"Elden Ring\d.png"), b"not really a picture").unwrap();
        h.run_steps(2);
        assert_eq!(names(h.state()), [(None, 1), (Some("Elden Ring".into()), 1), (Some("VALORANT".into()), 1)].into());
        std::fs::remove_file(dir.join(r"VALORANT\c.png")).unwrap();
        h.state_mut().rescan_clips();
        h.run_steps(2);
        assert_eq!(names(h.state()), [(None, 1), (Some("Elden Ring".into()), 1)].into());
        drop(h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The AMD switch-off, once saved, says to restart the PC and check AMD Software (it's
    /// untested), and Desktop capture says popups end up in clips; neither only on hover.
    #[test]
    fn amd_off_and_desktop_say_so() {
        use egui_kittest::kittest::Queryable;
        let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_size([1100.0, 2400.0]).with_accessibility_check(false).build_eframe(|cc| {
            let mut app = App::preview(cc);
            app.tab = Tab::Keybinds;
            app.nvidia = Some((false, false));
            (app.cfg.amd_off, app.saved.amd_off) = (true, true);
            app.amd = Some(Some(amd::Amd { replay_on: false, hotkeys_off: true, hotkeys: vec![] }));
            app
        });
        h.run_steps(2);
        assert!(h.query_by_label_contains("AMD Software reads this when it starts: restart the PC").is_some());
        let app = h.state_mut();
        (app.tab, app.cfg.capture) = (Tab::Video, "desktop".into());
        app.cfg.more_open = vec!["Video".into()];
        h.run_steps(2);
        assert!(h.query_by_label_contains("Popups and notifications on that monitor show up in clips.").is_some());
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

    /// The status box's Save clip sends a save, the same as the save key (there's no recorder
    /// here: the command only goes into a channel).
    #[test]
    fn status_box_save_clip_saves() {
        use egui_kittest::kittest::Queryable;
        let (tx, rx) = mpsc::channel();
        let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_size([1100.0, 700.0]).with_accessibility_check(false).build_eframe(move |cc| {
            let mut app = App::preview(cc);
            (app.cmds, app.tab) = (tx, Tab::Keybinds);
            app
        });
        h.run_steps(2);
        h.get_by_role_and_label(egui::Role::Button, "Save clip").click();
        h.run_steps(2);
        assert!(rx.try_iter().any(|c| matches!(c, Cmd::Save)));
    }

    /// A muted mic shows in the status box until it's unmuted; clicking it unmutes.
    #[test]
    fn status_box_shows_a_muted_mic() {
        use egui_kittest::kittest::Queryable;
        let (tx, rx) = mpsc::channel();
        let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_size([1100.0, 700.0]).with_accessibility_check(false).build_eframe(move |cc| {
            let mut app = App::preview(cc);
            (app.cmds, app.tab) = (tx, Tab::Keybinds);
            app
        });
        h.run_steps(2);
        assert!(h.query_by_label("Mic muted").is_none());
        h.state_mut().mic_muted = true;
        h.run_steps(2);
        h.get_by_label("Mic muted").click();
        h.run_steps(2);
        assert!(rx.try_iter().any(|c| matches!(c, Cmd::ToggleMic)));
    }

    /// Pictures in a temp folder `name`: a.png in VALORANT's folder, b.png not sorted.
    fn game_folder(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("VALORANT")).unwrap();
        std::fs::write(dir.join(r"VALORANT\a.png"), b"not really a picture").unwrap();
        std::fs::write(dir.join("b.png"), b"not really a picture").unwrap();
        dir
    }

    /// Each game keeps its tape color: the same clips give the same colors, and neither a newer
    /// clip of another game (which moves that game to the front) nor another order changes any.
    /// The first four games get four different colors in Retro; Minimalist hands out none.
    #[test]
    fn tape_colors_stay_with_their_game() {
        let clip = |game: Option<&str>, secs: u64| clips::Clip {
            path: format!("{secs}.mp4").into(),
            name: format!("{secs}.mp4"),
            bytes: 1,
            modified: std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs),
            group: game.map(String::from),
            is_picture: false,
        };
        let slots = |list: &[clips::Clip]| games_in(list).into_iter().map(|g| (g.name, g.tape)).collect::<std::collections::BTreeMap<_, _>>();
        let list = vec![clip(Some("VALORANT"), 30), clip(None, 20), clip(Some("Elden Ring"), 10), clip(Some("apex"), 5)];
        let before = slots(&list);
        assert_eq!(slots(&list), before);
        let newer: Vec<clips::Clip> = std::iter::once(clip(Some("Elden Ring"), 40)).chain(list.iter().cloned()).collect();
        assert_eq!(games_in(&newer)[0].name.as_deref(), Some("Elden Ring"), "played last, so first");
        assert_eq!(slots(&newer), before);
        let oldest_first: Vec<clips::Clip> = list.iter().rev().cloned().collect();
        assert_eq!(slots(&oldest_first), before);
        assert_eq!(before[&None], 3, "Not sorted last");
        let colors: std::collections::BTreeSet<[u8; 4]> = before.values().map(|&s| RETRO_LOOK.tape_colors[s % RETRO_LOOK.tape_colors.len()].to_array()).collect();
        assert_eq!(colors.len(), 4);
        assert!(MINIMALIST.tape_colors.is_empty());
    }

    /// A game's row under Clips in the sidebar opens the Clips page showing just that game, and
    /// All games shows every clip again.
    #[test]
    fn sidebar_game_rows_filter_clips() {
        use egui_kittest::kittest::Queryable;
        let dir = game_folder("instant-replay-side-games");
        let folder = dir.display().to_string();
        let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_size([1100.0, 700.0]).with_accessibility_check(false).build_eframe(|cc| {
            let mut app = App::preview(cc);
            (app.tab, app.cfg.folder) = (Tab::General, folder);
            app.saved.folder = app.cfg.folder.clone();
            app
        });
        h.run_steps(2);
        h.get_by_role_and_label(egui::Role::Tab, "VALORANT").click();
        h.run_steps(2);
        assert!(h.state().tab == Tab::Clips && h.state().view == ClipsView::Game(Some("VALORANT".into())));
        assert!(h.query_by_label("a").is_some() && h.query_by_label("b").is_none(), "only VALORANT's clip");
        h.get_by_role_and_label(egui::Role::Tab, "All games").click();
        h.run_steps(2);
        assert!(h.state().view == ClipsView::All);
        assert!(h.query_by_label("a").is_some() && h.query_by_label("b").is_some(), "every clip");
        drop(h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// In the smallest window the app allows, in either look and with the most the sidebar's
    /// bottom can hold (the new-version link and a key clash), the sidebar draws compact and the
    /// status box and its Save clip stay on screen, with no room for even one game row, so the
    /// games aren't shown; a normal height shows them.
    #[test]
    fn sidebar_skips_games_when_no_row_fits() {
        use egui_kittest::kittest::Queryable;
        app_update::set_found(Some(nine()));
        let forget = ForgetFound;
        let dir = game_folder("instant-replay-side-short");
        let folder = dir.display().to_string();
        let sidebar = |theme: &str, height: f32, most: bool| {
            let (folder, theme) = (folder.clone(), theme.to_string());
            let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_size([860.0, height]).with_accessibility_check(false).build_eframe(move |cc| {
                let mut app = App::preview(cc);
                (app.tab, app.cfg.folder, app.cfg.theme) = (Tab::General, folder, theme);
                (app.saved.folder, app.saved.check_updates) = (app.cfg.folder.clone(), most);
                // A clash on the save key, as if just checked.
                if most {
                    app.conflicts = vec![(Target::Save, "Also used by another program.".into())];
                    app.conflicts_keys = Target::ALL.into_iter().map(|t| app.hotkey_of(t).clone()).collect();
                    app.conflicts_checked = Some(std::time::Instant::now());
                }
                app
            });
            h.run_steps(4);
            let games = h.query_by_role_and_label(egui::Role::Tab, "VALORANT").is_some();
            let shown = h.query_by_label_contains("Version 9.0.0 is out").is_some() && h.query_by_label_contains("Key clash").is_some();
            let status = h.get_by_role_and_label(egui::Role::Button, "Save clip").rect().bottom();
            (games, shown, status)
        };
        let short = ["minimalist", "retro"].map(|theme| (theme, sidebar(theme, 560.0, true), sidebar(theme, 560.0, false).2));
        let normal = ["minimalist", "retro"].map(|theme| (theme, sidebar(theme, 700.0, false).0));
        drop(forget);
        let _ = std::fs::remove_dir_all(&dir);
        for (theme, (games, shown, status), plain) in short {
            assert!(shown, "{theme}: the link and the clash show");
            assert!(!games, "{theme}: no games when no row fits");
            assert!(status <= 560.0 && plain <= 560.0, "{theme}: status box on screen at 560, its button ends at {status} ({plain} without the link and clash)");
        }
        for (theme, games) in normal {
            assert!(games, "{theme}: games at a normal height");
        }
    }

    /// A game's chip on the Clips page is the same filter as its row in the sidebar (both show it
    /// picked), the All games chip shows every clip again, and Starred only the starred ones.
    #[test]
    fn game_chips_and_starred_filter_clips() {
        use egui_kittest::kittest::{NodeT, Queryable};
        let dir = game_folder("instant-replay-game-chips");
        let folder = dir.display().to_string();
        let mut h = egui_kittest::Harness::builder().with_max_steps(12).with_size([1100.0, 700.0]).with_accessibility_check(false).build_eframe(|cc| {
            let mut app = App::preview(cc);
            (app.tab, app.cfg.folder) = (Tab::Clips, folder);
            app.saved.folder = app.cfg.folder.clone();
            app
        });
        h.run_steps(2);
        h.get_by_label("VALORANT 1").click();
        h.run_steps(2);
        assert!(h.state().view == ClipsView::Game(Some("VALORANT".into())));
        assert_eq!(h.get_by_role_and_label(egui::Role::Tab, "VALORANT").accesskit_node().toggled(), Some(egui::accesskit::Toggled::True), "picked in the sidebar too");
        assert!(h.query_by_label("a").is_some() && h.query_by_label("b").is_none(), "only VALORANT's clip");
        h.get_by_role_and_label(egui::Role::Button, "All games").click();
        h.run_steps(2);
        assert!(h.state().view == ClipsView::All);
        assert!(h.query_by_label("a").is_some() && h.query_by_label("b").is_some(), "every clip");
        // Starred in memory only (favorites.json isn't written).
        h.state_mut().favorites.insert(dir.join("b.png").display().to_string());
        h.get_by_label("Starred").click();
        h.run_steps(2);
        assert!(h.state().view == ClipsView::Starred);
        assert!(h.query_by_label("a").is_none() && h.query_by_label("b").is_some(), "only the starred clip");
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

    /// A display change restarts the recorder only when what it takes from the monitors changed,
    /// never during a full recording or while a game is in front, and at most every 30 s.
    #[test]
    fn display_changes_restart_the_recorder_when_they_matter() {
        use std::time::Duration;
        let mon = |w: u32, h: u32| display::Monitor { id: "main".into(), width: w, height: h, hz: 144, x: 0, y: 0, primary: true, hdr: None, name: String::new() };
        let cfg = Config::default();
        let (old, new) = (recorder::display_key(&cfg, &[mon(2560, 1440)]), recorder::display_key(&cfg, &[mon(1920, 1080)]));
        let no_game = || false;
        let step = |recording, since: Option<u64>, game: bool| display_step(&old, &new, recording, since.map(Duration::from_secs), || game);
        assert_eq!(step(false, None, false), DisplayStep::Restart);
        assert_eq!(step(false, Some(31), false), DisplayStep::Restart);
        assert_eq!(step(true, None, false), DisplayStep::Hold(DisplayHold::Recording));
        assert_eq!(step(false, None, true), DisplayStep::Hold(DisplayHold::Game));
        assert_eq!(step(false, Some(10), false), DisplayStep::Hold(DisplayHold::TooSoon(Duration::from_secs(20))));
        // Back as it was (a game's own resolution, gone with the game): nothing to do, and the game
        // isn't even asked about.
        assert_eq!(display_step(&old, &old, false, None, || panic!("asked about a game")), DisplayStep::Unchanged);
        assert_eq!(display_step(&old, &old, true, None, no_game), DisplayStep::Hold(DisplayHold::Recording), "known once the recording stops");
        assert_eq!(display_step(&old, &None, false, None, no_game), DisplayStep::Restart, "no monitor at all");
    }
}
