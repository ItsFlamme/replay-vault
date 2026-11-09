//! Shows and hides the app window on top of the game. Done with plain Win32 calls: eframe's
//! own focus command goes through winit, which fakes an Alt keypress to steal focus.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetForegroundWindow, GetWindowRect, HWND_TOPMOST, IsIconic, IsWindow, IsWindowVisible, SW_HIDE, SW_RESTORE,
    SWP_NOSIZE, SWP_SHOWWINDOW, SetForegroundWindow, SetWindowPos, ShowWindow,
};
use windows::core::PCWSTR;

static WINDOW: OnceLock<isize> = OnceLock::new();
/// Centered once on first show; after that it stays wherever the user moved it.
static PLACED: AtomicBool = AtomicBool::new(false);
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
        let _ = SetForegroundWindow(window);
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
