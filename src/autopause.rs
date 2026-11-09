//! "Pause instant replay when no game is open" (General, off by default): with no game in front
//! for a couple of minutes, the replay buffer stops, and it starts again as soon as one is.
//! A game is told apart only from what Windows says about the window in front (full screen, or a
//! title the clips folder already has a game folder for); the game's process is never opened.
//! Replay the user turned off themselves is never turned back on.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crate::obs::Cmd;

/// The setting, kept here for the thread.
pub static ENABLED: AtomicBool = AtomicBool::new(false);
/// Replay is off because no game was open (not because the user turned it off).
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
            let enabled = ENABLED.load(Ordering::Relaxed);
            let game = enabled && game_in_front(Path::new(&folder()));
            let running = replay_on.load(Ordering::Relaxed);
            if let Some(on) = decide(enabled, game, running, PAUSED.load(Ordering::Relaxed), &mut no_game_since, Instant::now()) {
                PAUSED.store(!on, Ordering::Relaxed);
                // The user may have turned it back on themselves meanwhile.
                if on == running {
                    continue;
                }
                crate::log::write(if on { "A game is open: instant replay is back on." } else { "No game open for 2 minutes: instant replay paused." });
                if cmds.send(Cmd::SetReplay(on)).is_err() {
                    return;
                }
            }
        }
    });
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

/// Whether the window in front looks like a game: Windows calls it full screen (a game, or
/// anything else full screen, which is fine to keep recording), it covers its whole monitor
/// (borderless), or its title has a game folder in the clips folder already.
pub fn game_in_front(folder: &Path) -> bool {
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
    let covers = unsafe {
        GetWindowRect(window, &mut rect).is_ok() && GetMonitorInfoW(MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() && {
            let m = info.rcMonitor;
            rect.left <= m.left && rect.top <= m.top && rect.right >= m.right && rect.bottom >= m.bottom
        }
    };
    if covers {
        return true;
    }
    let name = crate::clips::game_folder_name();
    name != "Desktop" && folder.join(&name).is_dir()
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
