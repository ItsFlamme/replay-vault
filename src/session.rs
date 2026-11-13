//! Windows ending the session (shut down, restart, sign out, or an installer or update closing the
//! app) and the uninstaller asking the app to quit. The main window never hears of either (winit
//! doesn't pass them on), so a hidden window of its own, on its own thread, listens for them. It's
//! an ordinary top-level window that's never shown: message-only windows don't get the end of
//! the session, nor display changes, which it passes on too.

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Shutdown::{ShutdownBlockReasonCreate, ShutdownBlockReasonDestroy};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

/// The window's class, which `ask_to_quit` looks for.
const CLASS: PCWSTR = w!("ReplayVaultSession");

/// What the window tells `quit`, for the log.
pub const ENDING: &str = "Windows is shutting down, restarting or signing out";
pub const CLOSING: &str = "an installer or update is closing the app";
pub const ASKED: &str = "the uninstaller asked it to quit";

/// What the window passes on.
#[derive(Debug, PartialEq)]
pub enum Notice {
    /// The session ends or the app is asked to quit, and why (one of the reasons above).
    Quit(&'static str),
    /// The monitors may have changed: a display change (a resolution, a monitor plugged in or
    /// out) settled, or it's time for the periodic look that catches HDR being turned on or off,
    /// which doesn't always send one.
    Displays,
}

/// A burst of display changes (Windows sends several while it rearranges the monitors) is passed
/// on once, this long after the last one.
const DISPLAYS_SETTLE_MS: u32 = 2000;
/// How often `Notice::Displays` comes without a display change.
const DISPLAYS_LOOK_MS: u32 = 60_000;
const SETTLE_TIMER: usize = 1;
const LOOK_TIMER: usize = 2;

/// Starts the window. `on` runs on its thread with each notice. For `Notice::Quit` at the end of a
/// session Windows waits for it, showing why while it takes a while, and ends the app once it
/// returns. Returns the window, or None if Windows wouldn't make it.
pub fn watch(on: fn(Notice)) -> Option<HWND> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || unsafe { run(on, tx) });
    rx.recv().ok().flatten().map(|h| HWND(h as *mut _))
}

unsafe fn run(on: fn(Notice), tx: std::sync::mpsc::Sender<Option<isize>>) {
    unsafe {
        let class = WNDCLASSW { lpfnWndProc: Some(wndproc), lpszClassName: CLASS, ..Default::default() };
        RegisterClassW(&class);
        let Ok(hwnd) = CreateWindowExW(WS_EX_TOOLWINDOW, CLASS, w!("Replay Vault session"), WS_POPUP, 0, 0, 0, 0, None, None, None, None) else {
            let _ = tx.send(None);
            return;
        };
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, on as usize as isize);
        SetTimer(Some(hwnd), LOOK_TIMER, DISPLAYS_LOOK_MS, None);
        let _ = tx.send(Some(hwnd.0 as isize));
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        let on = || {
            let f = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            (f != 0).then(|| std::mem::transmute::<usize, fn(Notice)>(f as usize))
        };
        match msg {
            // The session may end: what's running is finished at WM_ENDSESSION, once it's sure.
            WM_QUERYENDSESSION => LRESULT(1),
            WM_ENDSESSION => {
                if wparam.0 != 0
                    && let Some(on) = on()
                {
                    let why = if lparam.0 as u32 & ENDSESSION_CLOSEAPP != 0 { CLOSING } else { ENDING };
                    // Shown by Windows if this takes a while (a recording being finished).
                    let _ = ShutdownBlockReasonCreate(hwnd, w!("Saving your recording and clips"));
                    on(Notice::Quit(why));
                    let _ = ShutdownBlockReasonDestroy(hwnd);
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                if let Some(on) = on() {
                    on(Notice::Quit(ASKED));
                }
                LRESULT(0)
            }
            // Sent to every top-level window. Setting the timer again restarts its wait.
            WM_DISPLAYCHANGE => {
                SetTimer(Some(hwnd), SETTLE_TIMER, DISPLAYS_SETTLE_MS, None);
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == SETTLE_TIMER || wparam.0 == LOOK_TIMER => {
                if wparam.0 == SETTLE_TIMER {
                    let _ = KillTimer(Some(hwnd), SETTLE_TIMER);
                }
                if let Some(on) = on() {
                    on(Notice::Displays);
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// `--quit`, from the uninstaller: asks this same copy (run from this exe), if it's running, to
/// quit the way the tray's Quit does, so a full recording is finished and saved, and waits up to
/// `wait` for it to be gone. A copy from before this window has nothing to ask; whatever is left
/// the uninstaller then stops by force.
pub fn ask_to_quit(wait: std::time::Duration) {
    let Some(crate::overlay::Running::Window(main)) = crate::overlay::running_copy() else { return };
    let pid = process_of(main);
    let Some(window) = window_of(pid) else { return };
    let Ok(process) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }) else { return };
    unsafe {
        if PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0)).is_ok() {
            WaitForSingleObject(process, wait.as_millis().min(u32::MAX as u128) as u32);
        }
        let _ = CloseHandle(process);
    }
}

fn process_of(window: HWND) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    pid
}

/// The session window of process `pid`, if it has one.
fn window_of(pid: u32) -> Option<HWND> {
    let mut after = None;
    while let Ok(window) = unsafe { FindWindowExW(None, after, CLASS, None) } {
        if process_of(window) == pid {
            return Some(window);
        }
        after = Some(window);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    static CALLS: std::sync::Mutex<Vec<Notice>> = std::sync::Mutex::new(Vec::new());

    fn record(n: Notice) {
        CALLS.lock().unwrap().push(n);
    }

    static DISPLAY_CALLS: std::sync::Mutex<Vec<Notice>> = std::sync::Mutex::new(Vec::new());

    fn record_displays(n: Notice) {
        DISPLAY_CALLS.lock().unwrap().push(n);
    }

    /// A burst of display changes is passed on once, about 2 s after the last one. Sent to this
    /// test's own window only.
    #[test]
    fn display_changes_settle_first() {
        use std::time::{Duration, Instant};
        let hwnd = watch(record_displays).expect("the window");
        let start = Instant::now();
        for _ in 0..3 {
            unsafe { SendMessageW(hwnd, WM_DISPLAYCHANGE, Some(WPARAM(32)), Some(LPARAM(0))) };
            std::thread::sleep(Duration::from_millis(300));
        }
        while DISPLAY_CALLS.lock().unwrap().is_empty() && start.elapsed() < Duration::from_secs(6) {
            std::thread::sleep(Duration::from_millis(50));
        }
        let waited = start.elapsed();
        assert!(waited >= Duration::from_millis(2500), "2 s after the last of them: {waited:?}");
        std::thread::sleep(Duration::from_millis(2500));
        assert_eq!(*DISPLAY_CALLS.lock().unwrap(), [Notice::Displays], "once for the burst");
    }

    /// The window is a top-level one (so Windows sends it the end of the session; a message-only
    /// one would never hear of it), says yes to the session ending, and runs `quit` only once the
    /// session really ends, or when asked to quit. Messages are sent to this test's own window
    /// only: nothing is broadcast and the PC isn't shut down.
    #[test]
    fn hears_the_session_end() {
        let hwnd = watch(record).expect("the window");
        unsafe {
            assert_eq!(GetAncestor(hwnd, GA_PARENT), GetDesktopWindow(), "a top-level window, not message-only");
            assert!(!IsWindowVisible(hwnd).as_bool());
            assert_eq!(process_of(hwnd), std::process::id());
            // Other tests here make session windows of this process too (and they stay), so
            // window_of may find one of theirs: it must find one of ours, and the class must find
            // this one among them.
            let found = window_of(std::process::id()).expect("a session window of this process");
            assert_eq!(process_of(found), std::process::id());
            let mut all = Vec::new();
            let mut after = None;
            while let Ok(window) = FindWindowExW(None, after, CLASS, None) {
                all.push(window);
                after = Some(window);
            }
            assert!(all.contains(&hwnd), "the class lookup finds this window");
            let send = |msg, w: usize, l: isize| SendMessageW(hwnd, msg, Some(WPARAM(w)), Some(LPARAM(l))).0;
            assert_eq!(send(WM_QUERYENDSESSION, 0, 0), 1);
            // Shut down cancelled by another app: nothing happens.
            send(WM_ENDSESSION, 0, 0);
            assert!(CALLS.lock().unwrap().is_empty());
            send(WM_ENDSESSION, 1, 0x8000_0000u32 as isize);
            send(WM_ENDSESSION, 1, ENDSESSION_CLOSEAPP as isize);
            send(WM_CLOSE, 0, 0);
            // (A real display change meanwhile would add a Displays notice; only quits count here.)
            let quits: Vec<_> = CALLS.lock().unwrap().drain(..).filter(|n| n != &Notice::Displays).collect();
            assert_eq!(quits, [Notice::Quit(ENDING), Notice::Quit(CLOSING), Notice::Quit(ASKED)]);
            assert!(IsWindow(Some(hwnd)).as_bool(), "WM_CLOSE doesn't destroy it");
        }
    }
}
