//! Hotkeys without a keyboard hook. The save-clip hotkey lives inside the hidden OBS (see
//! `Combo::obs_binding`), so this app never watches the keyboard for it. The overlay hotkey uses
//! Windows' standard RegisterHotKey. Setting a combo by pressing it reads the key state only while
//! the user is doing that in our own window. The save-the-last-30-s / 1-min keys use RegisterHotKey too.

use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey,
    UnregisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_APP, WM_HOTKEY,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Combo {
    pub vk: u16,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

type Callback = Box<dyn Fn() + Send>;
type Reply = Sender<Result<(), String>>;

/// The app's own hotkeys (the full save key belongs to OBS).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Overlay = 1,
    Save30 = 2,
    Save60 = 3,
    Record = 4,
    Screenshot = 5,
    Replay = 6,
    Mic = 7,
}

impl Action {
    pub const ALL: [Action; 7] =
        [Action::Overlay, Action::Save30, Action::Save60, Action::Record, Action::Screenshot, Action::Replay, Action::Mic];

    fn from_id(id: usize) -> Option<Action> {
        Action::ALL.into_iter().find(|a| *a as usize == id)
    }
}

const VK_ESCAPE: u16 = 0x1B;

static THREAD_ID: OnceLock<u32> = OnceLock::new();
static REQUEST: Mutex<Option<(Action, Option<Combo>, Reply)>> = Mutex::new(None);

struct Capture {
    active: bool,
    result: Option<Combo>,
    on_done: Option<Callback>,
}

static CAPTURE: Mutex<Capture> = Mutex::new(Capture { active: false, result: None, on_done: None });

/// Starts the thread that owns the app's hotkeys and calls `on_press` when one fires.
pub fn install(on_press: impl Fn(Action) + Send + 'static) -> anyhow::Result<()> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut msg = MSG::default();
        unsafe {
            // Creates this thread's message queue before anyone posts to it.
            let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
            let _ = tx.send(GetCurrentThreadId());
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                if msg.message == WM_HOTKEY {
                    if let Some(a) = Action::from_id(msg.wParam.0) {
                        on_press(a);
                    }
                } else if msg.message == WM_APP
                    && let Some((action, combo, reply)) = REQUEST.lock().unwrap().take() {
                    let id = action as i32;
                    let _ = UnregisterHotKey(None, id);
                    let result = match combo {
                        None => Ok(()),
                        Some(c) => RegisterHotKey(None, id, c.modifiers() | MOD_NOREPEAT, c.vk as u32)
                            .map_err(|_| format!("{c} is already used by another app. Pick another.")),
                    };
                    let _ = reply.send(result);
                }
            }
        }
    });
    let id = rx.recv()?;
    let _ = THREAD_ID.set(id);
    Ok(())
}

/// Registers one of the app's hotkeys (or none). Fails if another app already owns the combo.
pub fn set_combo(action: Action, combo: Option<Combo>) -> Result<(), String> {
    let id = *THREAD_ID.get().ok_or("The hotkey listener isn't running.")?;
    let (tx, rx) = mpsc::channel();
    *REQUEST.lock().unwrap() = Some((action, combo, tx));
    unsafe { PostThreadMessageW(id, WM_APP, WPARAM(0), LPARAM(0)) }.map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(2)).map_err(|_| "The hotkey listener didn't answer.".to_string())?
}

/// Waits for the next combo the user presses and calls `on_done`. Escape cancels.
pub fn start_capture(on_done: impl Fn() + Send + 'static) {
    {
        let mut c = CAPTURE.lock().unwrap();
        c.active = true;
        c.result = None;
        c.on_done = Some(Box::new(on_done));
    }
    std::thread::spawn(|| {
        // Keys held when capture starts (like the one that clicked the button) don't count.
        let held: Vec<u16> = (0x08u16..=0xFE).filter(|&vk| down(vk)).collect();
        while is_capturing() {
            poll_capture(&held);
            std::thread::sleep(Duration::from_millis(10));
        }
    });
}

/// Whether the thread that owns the app's hotkeys started.
pub fn is_running() -> bool {
    THREAD_ID.get().is_some()
}

pub fn is_capturing() -> bool {
    CAPTURE.lock().unwrap().active
}

/// The combo pressed during capture, or `None` (still waiting, or cancelled with Escape).
pub fn take_captured() -> Option<Combo> {
    CAPTURE.lock().unwrap().result.take()
}

fn down(vk: u16) -> bool {
    (unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000) != 0
}

fn poll_capture(held: &[u16]) {
    let mut c = CAPTURE.lock().unwrap();
    if !c.active {
        return;
    }
    for vk in 0x08u16..=0xFE {
        let modifier = matches!(vk, 0x10..=0x12 | 0x5B | 0x5C | 0xA0..=0xA5);
        if modifier || held.contains(&vk) || !down(vk) {
            continue;
        }
        let combo = Combo { vk, ctrl: down(0x11), alt: down(0x12), shift: down(0x10), win: down(0x5B) || down(0x5C) };
        if vk == VK_ESCAPE || combo.has_modifier() || allowed_bare(vk) {
            c.active = false;
            c.result = (vk != VK_ESCAPE).then_some(combo);
            if let Some(f) = &c.on_done {
                f();
            }
        }
        return;
    }
}

/// Keys that may be a hotkey on their own; anything else needs Ctrl/Alt/Shift/Win, or it would
/// fire while typing.
fn allowed_bare(vk: u16) -> bool {
    matches!(vk, 0x70..=0x87 | 0x13 | 0x91 | 0x2C | 0x2D | 0x21..=0x24)
}

/// Name shown in the app, and OBS's name for the same key.
const NAMED: &[(u16, &str, &str)] = &[
    (0x08, "Backspace", "BACKSPACE"), (0x09, "Tab", "TAB"), (0x0D, "Enter", "RETURN"), (0x13, "Pause", "PAUSE"),
    (0x14, "CapsLock", "CAPSLOCK"), (0x1B, "Esc", "ESCAPE"), (0x20, "Space", "SPACE"), (0x21, "PageUp", "PAGEUP"),
    (0x22, "PageDown", "PAGEDOWN"), (0x23, "End", "END"), (0x24, "Home", "HOME"), (0x25, "Left", "LEFT"),
    (0x26, "Up", "UP"), (0x27, "Right", "RIGHT"), (0x28, "Down", "DOWN"), (0x2C, "PrintScreen", "PRINT"),
    (0x2D, "Insert", "INSERT"), (0x2E, "Delete", "DELETE"), (0x91, "ScrollLock", "SCROLLLOCK"),
    (0xC0, "~", "ASCIITILDE"), (0xBD, "-", "MINUS"), (0xBB, "=", "EQUAL"), (0xDB, "[", "BRACKETLEFT"),
    (0xDD, "]", "BRACKETRIGHT"), (0xDC, "\\", "BACKSLASH"), (0xBA, ";", "SEMICOLON"), (0xDE, "'", "APOSTROPHE"),
    (0xBC, ",", "COMMA"), (0xBE, ".", "PERIOD"), (0xBF, "/", "SLASH"),
];

fn key_name(vk: u16) -> String {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => char::from(vk as u8).to_string(),
        0x60..=0x69 => format!("Num{}", vk - 0x60),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        _ => NAMED.iter().find(|(v, ..)| *v == vk).map_or(format!("Key{vk}"), |(_, n, _)| n.to_string()),
    }
}

fn key_code(name: &str) -> Option<u16> {
    let up = name.to_ascii_uppercase();
    if up.len() == 1 && up.as_bytes()[0].is_ascii_alphanumeric() {
        return Some(up.as_bytes()[0] as u16);
    }
    if name == "`" {
        return Some(0xC0);
    }
    if let Some(n) = up.strip_prefix("NUM").and_then(|d| d.parse::<u16>().ok()).filter(|n| *n <= 9) {
        return Some(0x60 + n);
    }
    if let Some(n) = up.strip_prefix('F').and_then(|d| d.parse::<u16>().ok()).filter(|n| (1..=24).contains(n)) {
        return Some(0x6F + n);
    }
    if let Some(n) = up.strip_prefix("KEY").and_then(|d| d.parse().ok()) {
        return Some(n);
    }
    NAMED.iter().find(|(_, n, _)| n.eq_ignore_ascii_case(name)).map(|(v, ..)| *v)
}

impl Combo {
    pub fn parse(s: &str) -> Option<Self> {
        let mut c = Combo { vk: 0, ctrl: false, alt: false, shift: false, win: false };
        for part in s.split('+').map(str::trim) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => c.ctrl = true,
                "alt" => c.alt = true,
                "shift" => c.shift = true,
                "win" | "super" => c.win = true,
                _ => c.vk = key_code(part)?,
            }
        }
        (c.vk != 0 && (c.has_modifier() || allowed_bare(c.vk))).then_some(c)
    }

    fn has_modifier(&self) -> bool {
        self.ctrl || self.alt || self.shift || self.win
    }

    fn modifiers(&self) -> HOT_KEY_MODIFIERS {
        let mut m = HOT_KEY_MODIFIERS(0);
        for (on, flag) in [(self.ctrl, MOD_CONTROL), (self.alt, MOD_ALT), (self.shift, MOD_SHIFT), (self.win, MOD_WIN)] {
            if on {
                m |= flag;
            }
        }
        m
    }

    /// The combo as an OBS hotkey binding for the replay-buffer save action, or `None` if OBS
    /// has no name for the key.
    pub fn obs_binding(&self) -> Option<String> {
        let key = match self.vk {
            0x30..=0x39 | 0x41..=0x5A => char::from(self.vk as u8).to_string(),
            0x60..=0x69 => format!("NUM{}", self.vk - 0x60),
            0x70..=0x87 => format!("F{}", self.vk - 0x6F),
            vk => NAMED.iter().find(|(v, ..)| *v == vk)?.2.to_string(),
        };
        let mut binding = String::new();
        for (on, name) in [(self.alt, "alt"), (self.ctrl, "control"), (self.shift, "shift"), (self.win, "command")] {
            if on {
                binding.push_str(&format!("\"{name}\":true,"));
            }
        }
        Some(format!("{{\"ReplayBuffer.Save\":[{{{binding}\"key\":\"OBS_KEY_{key}\"}}]}}"))
    }
}

impl std::fmt::Display for Combo {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        for (on, name) in [(self.ctrl, "Ctrl+"), (self.alt, "Alt+"), (self.shift, "Shift+"), (self.win, "Win+")] {
            if on {
                f.write_str(name)?;
            }
        }
        f.write_str(&key_name(self.vk))
    }
}

#[cfg(test)]
mod tests {
    use super::Combo;

    #[test]
    fn round_trips() {
        for s in ["Alt+Z", "Alt+F10", "Ctrl+Shift+S", "F9", "Win+PrintScreen", "Ctrl+Num5", "Alt+~"] {
            assert_eq!(Combo::parse(s).unwrap().to_string(), s);
        }
        assert_eq!(Combo::parse("alt+z").unwrap().to_string(), "Alt+Z");
        assert_eq!(Combo::parse("Alt+`").unwrap().to_string(), "Alt+~");
        assert!(Combo::parse("Alt+").is_none());
        assert!(Combo::parse("C").is_none(), "a bare letter would fire while typing");
    }

    #[test]
    fn obs_bindings() {
        assert_eq!(
            Combo::parse("Alt+Z").unwrap().obs_binding().unwrap(),
            r#"{"ReplayBuffer.Save":[{"alt":true,"key":"OBS_KEY_Z"}]}"#
        );
        assert_eq!(
            Combo::parse("Ctrl+Shift+F9").unwrap().obs_binding().unwrap(),
            r#"{"ReplayBuffer.Save":[{"control":true,"shift":true,"key":"OBS_KEY_F9"}]}"#
        );
    }
}
