//! Shows and hides the app window on top of the game. Done with plain Win32 calls: eframe's
//! own focus command goes through winit, which fakes an Alt keypress to steal focus.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetForegroundWindow, GetWindowRect, HWND_TOPMOST, IsIconic, IsWindow, IsWindowVisible, IsZoomed, SW_HIDE, SW_RESTORE,
    SW_SHOWMAXIMIZED, SWP_NOSIZE, SWP_SHOWWINDOW, SetForegroundWindow, SetWindowPos, ShowWindow,
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

/// The app window's handle, for the clip player.
pub fn raw() -> Option<isize> {
    WINDOW.get().copied()
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

/// Brings up the window of the copy that's already running (used by a second launch).
pub fn show_existing() {
    let title: Vec<u16> = crate::APP_TITLE.encode_utf16().chain([0]).collect();
    if let Ok(window) = unsafe { FindWindowW(None, PCWSTR(title.as_ptr())) } {
        bring_up(window, false);
    }
}

/// Restores the window on top of everything; `center` puts it in the middle of the monitor
/// the focused window (usually the game) is on.
fn bring_up(window: HWND, center: bool) {
    unsafe {
        if IsIconic(window).as_bool() {
            let _ = ShowWindow(window, SW_RESTORE);
        }
        let (mut x, mut y) = (0, 0);
        let mut flags = SWP_SHOWWINDOW | SWP_NOSIZE;
        let mut rect = RECT::default();
        let monitor = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if center && GetWindowRect(window, &mut rect).is_ok() && GetMonitorInfoW(monitor, &mut info).as_bool() {
            let m = info.rcWork;
            x = m.left + (m.right - m.left - (rect.right - rect.left)) / 2;
            y = m.top + (m.bottom - m.top - (rect.bottom - rect.top)) / 2;
        } else {
            flags |= windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE;
        }
        let _ = SetWindowPos(window, Some(HWND_TOPMOST), x, y, 0, 0, flags);
        if MAXIMIZE.swap(false, Ordering::Relaxed) {
            let _ = ShowWindow(window, SW_SHOWMAXIMIZED);
        }
        let _ = SetForegroundWindow(window);
    }
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

/// Where the window was (left, top, right, bottom, in screen pixels), whether it was maximized, the
/// page that was open and the version that last ran (to open What's new after an update), kept
/// across starts in window.json. Not a setting: it's never part of Save / Discard.
#[derive(serde::Serialize, serde::Deserialize, Default, PartialEq, Debug)]
#[serde(default)]
pub struct Placement {
    pub rect: Option<[i32; 4]>,
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
    let mut r = RECT::default();
    // Minimized, in the tray or maximized: keep the size and place it had before.
    let rect = unsafe { (shown && !maximized && GetWindowRect(window, &mut r).is_ok()).then_some([r.left, r.top, r.right, r.bottom]) };
    let now = Placement { rect: rect.or(before.rect), maximized, page: page.into(), version: env!("CARGO_PKG_VERSION").into() };
    if before != now {
        let _ = std::fs::create_dir_all(crate::config::dir());
        let _ = std::fs::write(placement_file(), serde_json::to_string(&now).unwrap_or_default());
    }
    *last = Some(now);
}

/// Puts the window back where `p` says, if that place is still on a screen (a monitor may have
/// been unplugged since); otherwise it's centered on first show as before.
pub fn restore_placement(p: &Placement) {
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromRect};
    use windows::Win32::UI::WindowsAndMessaging::{SWP_NOACTIVATE, SWP_NOZORDER};
    // Maximized when it's first shown (it may start in the tray).
    MAXIMIZE.store(p.maximized, Ordering::Relaxed);
    let (Some(window), Some([left, top, right, bottom])) = (hwnd(), p.rect) else { return };
    // The title bar's middle has to be on a screen, so the window can be grabbed.
    let grab = RECT { left: (left + right) / 2 - 20, top: top + 4, right: (left + right) / 2 + 20, bottom: top + 24 };
    if right - left < 200 || bottom - top < 200 || unsafe { MonitorFromRect(&grab, MONITOR_DEFAULTTONULL) }.is_invalid() {
        return;
    }
    unsafe {
        if SetWindowPos(window, None, left, top, right - left, bottom - top, SWP_NOZORDER | SWP_NOACTIVATE).is_ok() {
            PLACED.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A damaged window.json is ignored; a good one comes back as written.
    #[test]
    fn placement_reads_back() {
        std::fs::write(placement_file(), "not json").unwrap();
        assert_eq!(load_placement(), Placement::default());
        let p = Placement { rect: Some([100, 80, 1200, 780]), maximized: true, page: "Clips".into(), version: "0.1.3".into() };
        std::fs::write(placement_file(), serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(load_placement(), p);
        // A file from 0.1.3 (no maximized, no version) still reads.
        std::fs::write(placement_file(), r#"{"rect":[1,2,300,400],"page":"Video"}"#).unwrap();
        assert_eq!(load_placement(), Placement { rect: Some([1, 2, 300, 400]), page: "Video".into(), ..Default::default() });
        let _ = std::fs::remove_file(placement_file());
    }
}