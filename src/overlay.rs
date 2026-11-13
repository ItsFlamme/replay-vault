//! Shows and hides the app window on top of the game. Done with plain Win32 calls: eframe's
//! own focus command goes through winit, which fakes an Alt keypress to steal focus.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromRect, MonitorFromWindow,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetForegroundWindow, GetWindowRect, HWND_TOPMOST, IsIconic, IsWindow, IsWindowVisible, IsZoomed, SW_HIDE, SW_RESTORE,
    SW_SHOWMAXIMIZED, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SetForegroundWindow, SetWindowPos, ShowWindow,
};
use windows::core::PCWSTR;

static WINDOW: OnceLock<isize> = OnceLock::new();
/// Centered once on first show; after that it stays wherever the user moved it.
static PLACED: AtomicBool = AtomicBool::new(false);
/// It was maximized when the app last ran: maximize it again the first time it's shown.
static MAXIMIZE: AtomicBool = AtomicBool::new(false);
/// The window that had focus when the overlay came up (usually the game), to hand focus back to.
static PREVIOUS: AtomicIsize = AtomicIsize::new(0);

pub fn set_window(hwnd: isize) {
    let _ = WINDOW.set(hwnd);
    // The clip player is a child window; keep the app's own drawing out of its area.
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{GWL_STYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_CLIPCHILDREN};
        let h = HWND(hwnd as *mut _);
        SetWindowLongPtrW(h, GWL_STYLE, GetWindowLongPtrW(h, GWL_STYLE) | WS_CLIPCHILDREN.0 as isize);
    }
}

pub fn hwnd() -> Option<HWND> {
    WINDOW.get().map(|&h| HWND(h as *mut _))
}

/// A file or folder picker owned by the app's window, so it opens in front of it even while the
/// window is kept on top.
pub fn file_dialog() -> rfd::FileDialog {
    struct Owner(std::num::NonZeroIsize);
    impl raw_window_handle::HasWindowHandle for Owner {
        fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
            let raw = raw_window_handle::RawWindowHandle::Win32(raw_window_handle::Win32WindowHandle::new(self.0));
            Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(raw) })
        }
    }
    impl raw_window_handle::HasDisplayHandle for Owner {
        fn display_handle(&self) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
            Ok(raw_window_handle::DisplayHandle::windows())
        }
    }
    let dialog = rfd::FileDialog::new();
    match WINDOW.get().and_then(|&h| std::num::NonZeroIsize::new(h)) {
        Some(h) => dialog.set_parent(&Owner(h)),
        None => dialog,
    }
}

/// The window in front, as far as clips are concerned: while this app is in front, the one that
/// was in front before it opened (usually the game). Null if none.
pub fn window_in_front() -> HWND {
    let front = unsafe { GetForegroundWindow() };
    if hwnd() == Some(front) { HWND(PREVIOUS.load(Ordering::Relaxed) as *mut _) } else { front }
}

/// Shown and not minimized.
pub fn is_visible() -> bool {
    hwnd().is_some_and(|h| unsafe { IsWindowVisible(h).as_bool() && !IsIconic(h).as_bool() })
}

/// The window exists and is hidden (in the tray) or minimized.
pub fn is_hidden() -> bool {
    hwnd().is_some() && !is_visible()
}

pub fn show() {
    if let Some(window) = hwnd() {
        let front = unsafe { GetForegroundWindow() };
        if front != window {
            PREVIOUS.store(front.0 as isize, Ordering::Relaxed);
        }
        let center = !PLACED.swap(true, Ordering::Relaxed);
        bring_up(window, center);
    }
}

/// What a second launch finds of this same copy already running.
pub enum Running {
    /// Its window: bring it up.
    Window(HWND),
    /// It's on its way out: run from `<exe>.old`, which a portable update parks it as before
    /// restarting.
    Closing,
}

/// This same copy's window, for a second launch. A File Explorer window on the install folder has
/// the same title, and so does another copy of the app, so the window also has to belong to a
/// process run from this very exe (or, while a portable update restarts, from `<exe>.old`).
pub fn running_copy() -> Option<Running> {
    let ours = std::env::current_exe().ok()?;
    let title: Vec<u16> = crate::APP_TITLE.encode_utf16().chain([0]).collect();
    let mut after = None;
    while let Ok(window) = unsafe { FindWindowExW(None, after, None, PCWSTR(title.as_ptr())) } {
        match exe_path(window).and_then(|p| copy_of(&p, &ours)) {
            Some(Match::Same) => return Some(Running::Window(window)),
            Some(Match::Old) => return Some(Running::Closing),
            None => {}
        }
        after = Some(window);
    }
    None
}

/// Brings up the running copy's window that `running_copy` found.
pub fn show_existing(window: HWND) {
    bring_up(window, false);
}

#[derive(Debug, PartialEq)]
enum Match {
    Same,
    /// This exe's previous self, parked by a portable update (app_update's swap).
    Old,
}

/// Whether the process run from `exe` is this copy (run from `ours`).
fn copy_of(exe: &std::path::Path, ours: &std::path::Path) -> Option<Match> {
    let mut old = ours.as_os_str().to_owned();
    old.push(".old");
    if same_exe(exe, ours) {
        Some(Match::Same)
    } else if same_exe(exe, std::path::Path::new(&old)) {
        Some(Match::Old)
    } else {
        None
    }
}

/// The same exe file: the same real path when both can be resolved (so a junction, a subst or
/// mapped drive, or a short 8.3 name still match), else the same text ignoring case (Windows
/// paths are).
fn same_exe(a: &std::path::Path, b: &std::path::Path) -> bool {
    let text = |a: &std::path::Path, b: &std::path::Path| a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase();
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => text(&a, &b),
        _ => text(a, b),
    }
}

/// The full path of the exe that made `window`, if it can be read.
fn exe_path(window: HWND) -> Option<std::path::PathBuf> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let read = unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len) };
    let _ = unsafe { CloseHandle(process) };
    read.ok()?;
    Some(std::path::PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
}

/// Restores the window on top of everything; `center` puts it in the middle of the monitor
/// the focused window (usually the game) is on. A window left off every screen (its monitor was
/// unplugged while it was in the tray) is centered there too, and one bigger than its monitor's
/// work area is made to fit.
fn bring_up(window: HWND, center: bool) {
    unsafe {
        if IsIconic(window).as_bool() {
            let _ = ShowWindow(window, SW_RESTORE);
        }
        let mut flags = SWP_SHOWWINDOW | SWP_NOSIZE | SWP_NOMOVE;
        let mut target = [0; 4];
        let lost = window_rect(window).is_some_and(|r| !on_screen(r));
        // Maximized on a monitor that's gone: back to its normal size first, maximized again below.
        let maximize = lost && IsZoomed(window).as_bool();
        if maximize {
            let _ = ShowWindow(window, SW_RESTORE);
        }
        let monitor = if center || lost { MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY) } else { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
        // A maximized window's rect is a little bigger than the work area; it's left as it is.
        if let (Some(rect), Some(work), false) = (window_rect(window), work_area(monitor), IsZoomed(window).as_bool()) {
            target = fit_window(rect, work, center || lost);
            if target != rect {
                flags = SWP_SHOWWINDOW;
            }
        }
        let [left, top, right, bottom] = target;
        let _ = SetWindowPos(window, Some(HWND_TOPMOST), left, top, right - left, bottom - top, flags);
        if MAXIMIZE.swap(false, Ordering::Relaxed) || maximize {
            let _ = ShowWindow(window, SW_SHOWMAXIMIZED);
        }
        let _ = SetForegroundWindow(window);
    }
}

/// The window's rect (left, top, right, bottom) in screen pixels.
fn window_rect(window: HWND) -> Option<[i32; 4]> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(window, &mut r) }.ok().map(|()| [r.left, r.top, r.right, r.bottom])
}

/// A monitor's work area (without the taskbar) in screen pixels.
fn work_area(monitor: HMONITOR) -> Option<[i32; 4]> {
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool().then_some([info.rcWork.left, info.rcWork.top, info.rcWork.right, info.rcWork.bottom])
}

/// Whether a window at `r` can be grabbed: the middle of its title bar is on a screen.
fn on_screen(r: [i32; 4]) -> bool {
    let [left, top, right, _] = r;
    let grab = RECT { left: (left + right) / 2 - 20, top: top + 4, right: (left + right) / 2 + 20, bottom: top + 24 };
    !unsafe { MonitorFromRect(&grab, MONITOR_DEFAULTTONULL) }.is_invalid()
}

/// Where a window at `win` goes on a monitor whose work area is `work`: no bigger than the work
/// area, and in its middle with `center`. Otherwise it stays put, only moved back inside the
/// work area when it had to shrink.
fn fit_window(win: [i32; 4], work: [i32; 4], center: bool) -> [i32; 4] {
    let ([l, t, r, b], [wl, wt, wr, wb]) = (win, work);
    let (w, h) = ((r - l).min(wr - wl).max(1), (b - t).min(wb - wt).max(1));
    let (x, y) = if center {
        (wl + (wr - wl - w) / 2, wt + (wb - wt - h) / 2)
    } else if (w, h) != (r - l, b - t) {
        (l.clamp(wl, (wr - w).max(wl)), t.clamp(wt, (wb - h).max(wt)))
    } else {
        (l, t)
    };
    [x, y, x + w, y + h]
}

/// Keeps the window open but no longer above everything, so a window opened from it (File
/// Explorer) comes up in front. The next time it's brought up it's on top again.
pub fn drop_to_normal() {
    if let Some(window) = hwnd() {
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{HWND_NOTOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE};
            let _ = SetWindowPos(window, Some(HWND_NOTOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }
}

pub fn hide() {
    if let Some(window) = hwnd() {
        unsafe {
            let _ = ShowWindow(window, SW_HIDE);
        }
    }
}

/// Hides the overlay and puts focus back on the window that had it before (usually the game),
/// so there's no need to click back in. Focus moves first: once hidden we may not be allowed to.
pub fn close() {
    let prev = HWND(PREVIOUS.swap(0, Ordering::Relaxed) as *mut _);
    unsafe {
        if !prev.is_invalid() && IsWindow(Some(prev)).as_bool() && IsWindowVisible(prev).as_bool() {
            let _ = SetForegroundWindow(prev);
        }
    }
    hide();
}

pub fn toggle() {
    if is_visible() { close() } else { show() }
}

/// Where the window was (left, top, right, bottom, in screen pixels) and its monitor's scaling
/// then, whether it was maximized, the page that was open and the version that last ran (to open
/// What's new after an update), kept across starts in window.json. Not a setting: it's never part
/// of Save / Discard.
#[derive(serde::Serialize, serde::Deserialize, Default, PartialEq, Debug)]
#[serde(default)]
pub struct Placement {
    pub rect: Option<[i32; 4]>,
    /// The scaling `rect` was measured at, in DPI (96 is 100%); 0 in files from before it was kept.
    pub dpi: u32,
    pub maximized: bool,
    pub page: String,
    pub version: String,
}

fn placement_file() -> std::path::PathBuf {
    if cfg!(test) { std::env::temp_dir().join("replay-vault-window-test.json") } else { crate::config::dir().join("window.json") }
}

pub fn load_placement() -> Placement {
    std::fs::read_to_string(placement_file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// Remembers where the window is now (unless it's minimized or hidden), whether it's maximized,
/// `page` and this version, writing only when any of them changed.
pub fn save_placement(page: &str) {
    static LAST: std::sync::Mutex<Option<Placement>> = std::sync::Mutex::new(None);
    let Some(window) = hwnd() else { return };
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let before = last.take().unwrap_or_else(load_placement);
    let shown = is_visible();
    let maximized = if shown { unsafe { IsZoomed(window).as_bool() } } else { before.maximized };
    // Minimized, in the tray or maximized: keep the size and place it had before.
    let (rect, dpi) = match window_rect(window).filter(|_| shown && !maximized) {
        Some(r) => (Some(r), crate::toast::monitor_dpi(unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) })),
        None => (before.rect, before.dpi),
    };
    let now = Placement { rect, dpi, maximized, page: page.into(), version: env!("CARGO_PKG_VERSION").into() };
    if before != now {
        let _ = std::fs::create_dir_all(crate::config::dir());
        let _ = std::fs::write(placement_file(), serde_json::to_string(&now).unwrap_or_default());
    }
    *last = Some(now);
}

/// Puts the window back where `p` says, if that place is still on a screen (a monitor may have
/// been unplugged since); otherwise it's centered on first show as before. Its size follows a
/// change of scaling since and is kept within the monitor's work area.
pub fn restore_placement(p: &Placement) {
    use windows::Win32::UI::WindowsAndMessaging::{SWP_NOACTIVATE, SWP_NOZORDER};
    // Maximized when it's first shown (it may start in the tray).
    MAXIMIZE.store(p.maximized, Ordering::Relaxed);
    let (Some(window), Some(rect @ [left, top, right, bottom])) = (hwnd(), p.rect) else { return };
    // The title bar's middle has to be on a screen, so the window can be grabbed.
    if right - left < 200 || bottom - top < 200 || !on_screen(rect) {
        return;
    }
    unsafe {
        // Moved there first, so it takes that monitor's scaling (the window resizes itself for
        // it), then sized.
        let flags = SWP_NOZORDER | SWP_NOACTIVATE;
        if SetWindowPos(window, None, left, top, 0, 0, flags | SWP_NOSIZE).is_err() {
            return;
        }
        let monitor = MonitorFromRect(&RECT { left, top, right, bottom }, MONITOR_DEFAULTTONEAREST);
        let dpi = crate::toast::monitor_dpi(monitor);
        let sized = [left, top, left + rescale(right - left, p.dpi, dpi), top + rescale(bottom - top, p.dpi, dpi)];
        let [l, t, r, b] = work_area(monitor).map_or(sized, |work| fit_window(sized, work, false));
        if SetWindowPos(window, None, l, t, r - l, b - t, flags).is_ok() {
            PLACED.store(true, Ordering::Relaxed);
        }
    }
}

/// `len` pixels measured at `was` DPI, at `now` DPI. Unchanged when `was` isn't known (0).
fn rescale(len: i32, was: u32, now: u32) -> i32 {
    if was == 0 || now == 0 { len } else { (len as i64 * now as i64 / was as i64) as i32 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A second launch brings up only the copy run from the very same exe.
    #[test]
    fn same_exe_by_full_path() {
        let ours = Path::new(r"G:\Portable\Replay Vault\main\replay-vault.exe");
        assert!(same_exe(ours, ours));
        assert!(same_exe(ours, Path::new(r"g:\portable\replay vault\MAIN\REPLAY-VAULT.EXE")));
        assert!(!same_exe(ours, Path::new(r"G:\Portable\Replay Vault\other\replay-vault.exe")));
        assert!(!same_exe(ours, Path::new(r"C:\Program Files\Replay Vault\replay-vault.exe")));
    }

    /// Paths that look different but resolve to the same file are the same exe.
    #[test]
    fn same_exe_by_real_path() {
        let dir = std::env::temp_dir().join(format!("replay-vault-same-{}", crate::config::random_hex()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let exe = dir.join("app.exe");
        std::fs::write(&exe, b"x").unwrap();
        std::fs::write(dir.join("other.exe"), b"x").unwrap();
        assert!(same_exe(&exe, &dir.join(r"sub\..\.\app.exe")));
        assert!(!same_exe(&exe, &dir.join(r"sub\..\other.exe")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A portable update parks the running exe as `<exe>.old`: still this copy, closing.
    #[test]
    fn parked_exe_is_this_copy_closing() {
        let ours = Path::new(r"D:\Apps\Replay Vault\replay-vault.exe");
        assert_eq!(copy_of(ours, ours), Some(Match::Same));
        assert_eq!(copy_of(Path::new(r"d:\apps\replay vault\REPLAY-VAULT.exe.old"), ours), Some(Match::Old));
        assert_eq!(copy_of(Path::new(r"D:\Apps\Replay Vault\replay-vault.exe.new"), ours), None);
        assert_eq!(copy_of(Path::new(r"D:\Other\replay-vault.exe.old"), ours), None);
    }

    /// A damaged window.json is ignored; a good one comes back as written.
    #[test]
    fn placement_reads_back() {
        std::fs::write(placement_file(), "not json").unwrap();
        assert_eq!(load_placement(), Placement::default());
        let p = Placement { rect: Some([100, 80, 1200, 780]), dpi: 144, maximized: true, page: "Clips".into(), version: "0.1.3".into() };
        std::fs::write(placement_file(), serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(load_placement(), p);
        // A file from 0.1.3 (no maximized, no version) still reads.
        std::fs::write(placement_file(), r#"{"rect":[1,2,300,400],"page":"Video"}"#).unwrap();
        assert_eq!(load_placement(), Placement { rect: Some([1, 2, 300, 400]), page: "Video".into(), ..Default::default() });
        let _ = std::fs::remove_file(placement_file());
    }

    /// A window is fitted into the work area: centered when asked, otherwise left where it is
    /// unless it had to shrink, then moved back inside.
    #[test]
    fn fit_window_into_the_work_area() {
        let work = [0, 0, 1920, 1040];
        // Fits and on screen: unchanged, even partly past the edge.
        assert_eq!(fit_window([100, 50, 1300, 850], work, false), [100, 50, 1300, 850]);
        assert_eq!(fit_window([1500, 50, 2700, 850], work, false), [1500, 50, 2700, 850]);
        // Centered at its size.
        assert_eq!(fit_window([5000, 300, 6200, 1100], work, true), [360, 120, 1560, 920]);
        // Too big (from a bigger or higher scaled monitor): shrunk to the work area.
        assert_eq!(fit_window([-200, -100, 3640, 2060], work, false), work);
        assert_eq!(fit_window([5000, 300, 8840, 2460], work, true), work);
        assert_eq!(fit_window([1000, 200, 3000, 1000], work, false), [0, 200, 1920, 1000]);
        // A second monitor left of the primary, taskbar at its top.
        let left = [-1280, 40, 0, 1024];
        assert_eq!(fit_window([-1400, 0, 200, 1200], left, false), left);
        assert_eq!(fit_window([300, 300, 1100, 900], left, true), [-1040, 232, -240, 832]);
    }

    /// A size saved at one scaling comes back the same size on screen at another.
    #[test]
    fn size_follows_scaling() {
        assert_eq!(rescale(1500, 144, 96), 1000);
        assert_eq!(rescale(1000, 96, 144), 1500);
        assert_eq!(rescale(1000, 96, 96), 1000);
        // Saved before the scaling was kept: as it was.
        assert_eq!(rescale(1000, 0, 144), 1000);
    }
}