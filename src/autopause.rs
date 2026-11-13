//! "Pause instant replay when no game is open" (General, off by default): with no game in front
//! for a couple of minutes, the replay buffer stops, and it starts again as soon as one is.
//! A game is told apart only from what Windows says about the window in front (full screen, as
//! big as its monitor but the taskbar, a title the clips folder already has a game folder for, or
//! one a clip was saved from lately); the game's process is never opened.
//! Replay the user turned off themselves is never turned back on.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::RECT;

use crate::obs::Cmd;

/// The setting, kept here for the thread.
pub static ENABLED: AtomicBool = AtomicBool::new(false);
/// Replay is off because no game was open (not because the user turned it off). Set here when
/// pausing or resuming, and by the recorder thread once a change is made (`replay_set`), so a
/// change by hand clears it.
pub static PAUSED: AtomicBool = AtomicBool::new(false);

/// How long with no game before replay pauses: alt-tabbing out or a loading screen in a window
/// shouldn't cost the buffer.
const GRACE: Duration = Duration::from_secs(120);
const TICK: Duration = Duration::from_secs(5);

/// Looks every few seconds; `replay_on` says whether the buffer is running now.
pub fn start(cmds: Sender<Cmd>, replay_on: &'static AtomicBool, folder: impl Fn() -> String + Send + 'static) {
    std::thread::spawn(move || {
        let mut no_game_since: Option<Instant> = None;
        loop {
            std::thread::sleep(TICK);
            remember_front();
            let enabled = ENABLED.load(Ordering::Relaxed);
            let game = enabled && in_front(Path::new(&folder()), true);
            let running = replay_on.load(Ordering::Relaxed);
            if let Some(on) = decide(enabled, game, running, PAUSED.load(Ordering::Relaxed), &mut no_game_since, Instant::now()) {
                PAUSED.store(!on, Ordering::Relaxed);
                // The user may have turned it back on themselves meanwhile.
                if on == running {
                    continue;
                }
                crate::log::write(if on { "A game is open: instant replay is back on." } else { "No game open for 2 minutes: instant replay paused." });
                if cmds.send(Cmd::AutoReplay(on)).is_err() {
                    return;
                }
            }
        }
    });
}

/// The title of the last window in front that wasn't Windows' own (taskbar, tray, desktop, Start)
/// or this app's, for the tray's "Count last window as a game": opening the tray puts the
/// taskbar in front, so the game is the one before it.
static LAST_WINDOW: Mutex<String> = Mutex::new(String::new());

fn remember_front() {
    use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetWindowTextW, GetWindowThreadProcessId};
    let window = crate::overlay::window_in_front();
    if window.is_invalid() {
        return;
    }
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    if pid == std::process::id() {
        return;
    }
    let mut class = [0u16; 64];
    let n = unsafe { GetClassNameW(window, &mut class) };
    let class = String::from_utf16_lossy(&class[..n.max(0) as usize]);
    if SHELL.contains(&class.as_str()) {
        return;
    }
    let mut title = [0u16; 256];
    let n = unsafe { GetWindowTextW(window, &mut title) };
    let title = String::from_utf16_lossy(&title[..n.max(0) as usize]);
    if !title.trim().is_empty() {
        *LAST_WINDOW.lock().unwrap_or_else(|e| e.into_inner()) = title;
    }
}

/// Windows' own windows that come to the front on the way to the tray menu.
const SHELL: [&str; 7] =
    ["Shell_TrayWnd", "Shell_SecondaryTrayWnd", "NotifyIconOverflowWindow", "TopLevelWindowForOverflowXamlIsland", "Progman", "WorkerW", "Windows.UI.Core.CoreWindow"];

/// Makes the last window in front count as a game from now on: its game folder is made in the
/// clips folder (where its clips go anyway), which `game_in_front` already counts. Returns its name.
pub fn count_last_window(folder: &Path) -> anyhow::Result<String> {
    let title = LAST_WINDOW.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let name = crate::clips::folder_name_from_title(&title);
    anyhow::ensure!(name != "Desktop", "no game window has been in front yet");
    std::fs::create_dir_all(folder.join(&name))?;
    Ok(name)
}

/// Instant replay was just turned `on` or off, `by_hand` (the switch, the tray, the key) or by
/// auto-pause. A change by hand is the user's: replay they turned off stays off when a game opens.
pub fn replay_set(on: bool, by_hand: bool) {
    PAUSED.store(paused_after(on, by_hand), Ordering::Relaxed);
}

/// Whether replay counts as paused by auto-pause after it was turned `on` or off.
fn paused_after(on: bool, by_hand: bool) -> bool {
    !on && !by_hand
}

/// How long a window a clip was saved from keeps counting as a game after it was last in front.
const SAVED_FROM_FOR: Duration = Duration::from_secs(3600);

/// Game folder names of windows a clip or screenshot was saved from, and when each last counted.
static SAVED_FROM: Mutex<Vec<(String, Instant)>> = Mutex::new(Vec::new());

/// A clip or screenshot was saved from the window named `name` (its game folder name): it counts
/// as a game while it's in front, until it's been away for an hour.
pub fn saved_from(name: &str) {
    if name != "Desktop" {
        note(&mut SAVED_FROM.lock().unwrap_or_else(|e| e.into_inner()), name, Instant::now());
    }
}

fn note(list: &mut Vec<(String, Instant)>, name: &str, now: Instant) {
    list.retain(|(n, _)| n != name);
    list.push((name.to_string(), now));
}

/// Whether `name` is in `list` and was last counted within SAVED_FROM_FOR; counting it starts its
/// hour over. Ones that ran out are forgotten.
fn saved_lately(list: &mut Vec<(String, Instant)>, name: &str, now: Instant) -> bool {
    list.retain(|(_, at)| now.saturating_duration_since(*at) < SAVED_FROM_FOR);
    match list.iter_mut().find(|(n, _)| n == name) {
        Some((_, at)) => {
            *at = now;
            true
        }
        None => false,
    }
}

/// Whether a window at `window` covers all of `area` (a monitor, or the part the taskbar leaves).
fn covers(window: &RECT, area: &RECT) -> bool {
    window.left <= area.left && window.top <= area.top && window.right >= area.right && window.bottom >= area.bottom
}

/// What to do now: Some(true) turn replay back on, Some(false) pause it, None leave it.
fn decide(enabled: bool, game: bool, replay_on: bool, paused: bool, no_game_since: &mut Option<Instant>, now: Instant) -> Option<bool> {
    if !enabled || game {
        *no_game_since = None;
        // Turning the setting off, or a game opening, undoes a pause of ours (and only ours).
        return paused.then_some(true);
    }
    if paused || !replay_on {
        // Already paused, or the user has replay off: nothing to do.
        return None;
    }
    let since = *no_game_since.get_or_insert(now);
    (now.duration_since(since) >= GRACE).then_some(false)
}

/// How long since the last key press or mouse move anywhere on the PC.
pub fn idle() -> Duration {
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        return Duration::ZERO;
    }
    Duration::from_millis(unsafe { GetTickCount() }.wrapping_sub(info.dwTime) as u64)
}

/// Whether the window in front looks like a game: Windows calls it full screen (a game, or
/// anything else full screen, which is fine to keep recording), it covers its whole monitor
/// (borderless), its title has a game folder in the clips folder already, or a clip was saved
/// from it lately.
pub fn game_in_front(folder: &Path) -> bool {
    in_front(folder, false)
}

/// `game_in_front`, and with `work_area` a window covering all of its monitor but the taskbar
/// counts too: a game in a window that size, or anything maximized. Auto-pause counts those (it
/// only ever keeps recording on them); holding back a recorder restart or a popup for every
/// maximized window would be too much.
fn in_front(folder: &Path, work_area: bool) -> bool {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
    use windows::Win32::UI::Shell::{QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState};
    use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetWindowRect};
    if let Ok(state) = unsafe { SHQueryUserNotificationState() }
        && [QUNS_BUSY, QUNS_RUNNING_D3D_FULL_SCREEN, QUNS_PRESENTATION_MODE].contains(&state)
    {
        return true;
    }
    let window = crate::overlay::window_in_front();
    if window.is_invalid() {
        return false;
    }
    let mut class = [0u16; 64];
    let n = unsafe { GetClassNameW(window, &mut class) };
    let class = String::from_utf16_lossy(&class[..n.max(0) as usize]);
    // The desktop itself covers the monitor too.
    if class == "Progman" || class == "WorkerW" {
        return false;
    }
    let mut rect = Default::default();
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    // The work area is inside the monitor, so a window covering the monitor covers it too.
    let big = unsafe {
        GetWindowRect(window, &mut rect).is_ok()
            && GetMonitorInfoW(MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST), &mut info).as_bool()
            && covers(&rect, if work_area { &info.rcWork } else { &info.rcMonitor })
    };
    if big {
        return true;
    }
    let name = crate::clips::game_folder_name();
    name != "Desktop" && (folder.join(&name).is_dir() || saved_lately(&mut SAVED_FROM.lock().unwrap_or_else(|e| e.into_inner()), &name, Instant::now()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pauses_after_the_grace_and_comes_back_with_a_game() {
        let t0 = Instant::now();
        let mut since = None;
        // No game, replay on: nothing until the grace is up, then pause.
        assert_eq!(decide(true, false, true, false, &mut since, t0), None);
        assert_eq!(decide(true, false, true, false, &mut since, t0 + GRACE - Duration::from_secs(1)), None);
        assert_eq!(decide(true, false, true, false, &mut since, t0 + GRACE), Some(false));
        // Paused by us: a game brings it back.
        assert_eq!(decide(true, false, false, true, &mut since, t0 + GRACE * 2), None);
        assert_eq!(decide(true, true, false, true, &mut since, t0 + GRACE * 2), Some(true));
        // A game in front for a moment starts the wait over.
        let mut since = None;
        decide(true, false, true, false, &mut since, t0);
        decide(true, true, true, false, &mut since, t0 + GRACE / 2);
        assert_eq!(decide(true, false, true, false, &mut since, t0 + GRACE), None);
    }

    /// A window counts as a game when it covers its monitor, or all of it but the taskbar (a
    /// maximized window's edges stick out a little); a smaller window doesn't.
    #[test]
    fn big_windows_count() {
        let r = |left, top, right, bottom| RECT { left, top, right, bottom };
        let (monitor, work) = (r(0, 0, 1920, 1080), r(0, 0, 1920, 1040));
        assert!(covers(&r(0, 0, 1920, 1080), &work), "borderless full screen");
        assert!(covers(&r(-8, -8, 1928, 1048), &work), "maximized");
        assert!(!covers(&r(-8, -8, 1928, 1048), &monitor));
        assert!(!covers(&r(160, 90, 1760, 990), &work), "a 1600x900 window");
        assert!(!covers(&r(0, 0, 1920, 1000), &work), "short of the taskbar");
        // On a second monitor to the left, with the taskbar on its left side.
        let work = r(-1920 + 48, 0, 0, 1080);
        assert!(covers(&r(-1920, 0, 0, 1080), &work));
        assert!(!covers(&r(0, 0, 1920, 1080), &work), "on the other monitor");
    }

    /// A window saved from counts as a game while it keeps coming to the front, and stops an hour
    /// after it was last in front.
    #[test]
    fn saved_from_counts_for_an_hour_in_front() {
        let t0 = Instant::now();
        let min = |m: u64| t0 + Duration::from_secs(m * 60);
        let mut list = Vec::new();
        assert!(!saved_lately(&mut list, "Hollow Knight", t0));
        note(&mut list, "Hollow Knight", t0);
        assert!(saved_lately(&mut list, "Hollow Knight", min(50)));
        // In front 50 minutes in: its hour starts over from then.
        assert!(saved_lately(&mut list, "Hollow Knight", min(100)));
        assert!(!saved_lately(&mut list, "Notepad", min(100)));
        assert!(!saved_lately(&mut list, "Hollow Knight", min(160)));
        assert!(list.is_empty(), "ran out and forgotten");
        note(&mut list, "A", t0);
        note(&mut list, "A", t0);
        assert_eq!(list.len(), 1);
    }

    /// Replay turned on or off by hand is never left marked paused, so a game opening later
    /// doesn't turn back on what the user turned off; only auto-pause's own pause marks it.
    #[test]
    fn a_change_by_hand_clears_the_pause() {
        let t0 = Instant::now();
        let mut since = None;
        // Paused by us after the grace.
        decide(true, false, true, false, &mut since, t0);
        assert_eq!(decide(true, false, true, false, &mut since, t0 + GRACE), Some(false));
        assert!(paused_after(false, false));
        // The user turns it on by hand, then off by hand: no longer ours.
        assert!(!paused_after(true, true));
        let paused = paused_after(false, true);
        assert!(!paused);
        // A game opens: replay stays off.
        assert_eq!(decide(true, true, false, paused, &mut since, t0 + GRACE * 2), None);
        // Auto-pause turning it back on clears it too.
        assert!(!paused_after(true, false));
    }

    #[test]
    fn never_turns_on_what_the_user_turned_off() {
        let t0 = Instant::now();
        let mut since = None;
        // Off by the user, a game opens: stays off.
        assert_eq!(decide(true, true, false, false, &mut since, t0), None);
        assert_eq!(decide(true, false, false, false, &mut since, t0 + GRACE * 2), None);
        // The setting turned off while paused by us: back on.
        assert_eq!(decide(false, false, false, true, &mut since, t0), Some(true));
        // The setting off: never pauses.
        assert_eq!(decide(false, false, true, false, &mut since, t0 + GRACE * 3), None);
    }
}
