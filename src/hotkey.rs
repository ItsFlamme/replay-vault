//! Hotkeys without a keyboard hook. The save-clip hotkey lives inside the hidden OBS (see
//! `Combo::obs_binding`), so this app never watches the keyboard for it. The app's other keys use
//! Windows' standard RegisterHotKey. A key another program already registered (NVIDIA's overlay
//! keeps Alt+F1, Alt+F9, Alt+R and more even when their features aren't used) is read from the
//! key state instead, every 15 ms, the way OBS reads its own keys; both programs react to it. Not
//! a hook, and nothing touches the game. (Raw Input was tried first: on Vlad's PC Windows sent it
//! nothing for NVIDIA's keys.) The polling only runs while such a shared key exists.
//! Setting a combo by pressing it reads the key state only while the user is doing that in our
//! own window.

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
    GetMessageW, KillTimer, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetTimer, WM_APP, WM_HOTKEY, WM_TIMER,
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
type Reply = Sender<Result<Held, String>>;

/// The app's own hotkeys (the full save key belongs to OBS).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Overlay = 1,
    Record = 4,
    Screenshot = 5,
    Replay = 6,
    Mic = 7,
}

impl Action {
    pub const ALL: [Action; 5] = [Action::Overlay, Action::Record, Action::Screenshot, Action::Replay, Action::Mic];

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
    /// When the last capture ended: the key that ended it is likely still down.
    ended: Option<std::time::Instant>,
}

static CAPTURE: Mutex<Capture> = Mutex::new(Capture { active: false, result: None, on_done: None, ended: None });

/// While a key is being set, and just after: a press then is the new key, not a command.
fn setting_key() -> bool {
    let c = CAPTURE.lock().unwrap();
    c.active || c.ended.is_some_and(|t| t.elapsed() < Duration::from_millis(500))
}

/// How a key was set: registered by this app alone, or shared with the program that holds it
/// (read from the key state; that program reacts to it too).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Held {
    Ours,
    Shared,
}

/// Starts the thread that owns the app's hotkeys and calls `on_press` when one fires.
pub fn install(on_press: impl Fn(Action) + Send + 'static) -> anyhow::Result<()> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut msg = MSG::default();
        let mut shared = Shared::default();
        unsafe {
            // Creates this thread's message queue before anyone posts to it.
            let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
            let _ = tx.send(GetCurrentThreadId());
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                if msg.message == WM_HOTKEY {
                    if let Some(a) = Action::from_id(msg.wParam.0)
                        && !setting_key() {
                        crate::log::write(&format!("Key pressed: {a:?}."));
                        on_press(a);
                    }
                } else if msg.message == WM_TIMER && msg.hwnd.is_invalid() && msg.wParam.0 == shared.timer {
                    // Polled even while a key is being set, so a key held then doesn't count later.
                    let pressed = shared.poll();
                    for a in pressed.into_iter().filter(|_| !setting_key()) {
                        crate::log::write(&format!("Key pressed: {a:?} (shared key)."));
                        on_press(a);
                    }
                } else if msg.message == WM_APP
                    && let Some((action, combo, reply)) = REQUEST.lock().unwrap().take() {
                    let id = action as i32;
                    let _ = UnregisterHotKey(None, id);
                    shared.keys.retain(|(a, _)| *a != action);
                    let result = match combo {
                        None => Ok(Held::Ours),
                        Some(c) if RegisterHotKey(None, id, c.modifiers() | MOD_NOREPEAT, c.vk as u32).is_ok() => Ok(Held::Ours),
                        Some(c) => {
                            shared.keys.push((action, c));
                            // Still held from setting it (or anything else): that press doesn't count.
                            if down(c.vk) && !shared.down.contains(&c.vk) {
                                shared.down.push(c.vk);
                            }
                            Ok(Held::Shared)
                        }
                    };
                    let result = result.and_then(|h| shared.listen().map(|()| h));
                    let _ = reply.send(result);
                }
            }
        }
    });
    let id = rx.recv()?;
    let _ = THREAD_ID.set(id);
    Ok(())
}

/// The keys another program registered first, read from the key state on a timer of the hotkey
/// thread, and which of their keys are down right now (to fire once per press, not on repeat).
#[derive(Default)]
struct Shared {
    keys: Vec<(Action, Combo)>,
    /// The thread timer's id, 0 while there's no shared key.
    timer: usize,
    down: Vec<u16>,
}

/// How often shared keys are read. A real key press lasts 50 ms or more.
const POLL_MS: u32 = 15;

impl Shared {
    /// Runs the timer while there's a shared key, and stops it when there's none.
    fn listen(&mut self) -> Result<(), String> {
        let want = !self.keys.is_empty();
        if want && self.timer == 0 {
            self.timer = unsafe { SetTimer(None, 0, POLL_MS, None) };
            if self.timer == 0 {
                return Err("Couldn't start reading keys other programs also use.".into());
            }
        } else if !want && self.timer != 0 {
            let _ = unsafe { KillTimer(None, self.timer) };
            self.timer = 0;
            self.down.clear();
        }
        Ok(())
    }

    /// The shared keys pressed since the last look (fresh presses only).
    fn poll(&mut self) -> Vec<Action> {
        let mods = (down(0x11), down(0x12), down(0x10), down(0x5B) || down(0x5C));
        let mut vks: Vec<u16> = self.keys.iter().map(|(_, c)| c.vk).collect();
        vks.sort_unstable();
        vks.dedup();
        vks.into_iter()
            .filter_map(|vk| {
                let now = down(vk);
                (now != self.down.contains(&vk)).then(|| on_key(&mut self.down, &self.keys, vk, !now, mods)).flatten()
            })
            .collect()
    }
}

/// One key going down or up: the action whose combo it completes on a fresh press. `mods` is
/// Ctrl, Alt, Shift, Win as they are now.
fn on_key(down: &mut Vec<u16>, keys: &[(Action, Combo)], vk: u16, up: bool, mods: (bool, bool, bool, bool)) -> Option<Action> {
    if up {
        down.retain(|&k| k != vk);
        return None;
    }
    if down.contains(&vk) {
        return None;
    }
    down.push(vk);
    let (ctrl, alt, shift, win) = mods;
    keys.iter().find(|(_, c)| c.vk == vk && c.ctrl == ctrl && c.alt == alt && c.shift == shift && c.win == win).map(|(a, _)| *a)
}

/// Sets one of the app's hotkeys (or none): registered if it's free, otherwise shared with the
/// program that has it. Fails only if the listener isn't working.
pub fn set_combo(action: Action, combo: Option<Combo>) -> Result<Held, String> {
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

/// Tests: as if the user had just pressed `c` while a key was being set.
#[cfg(test)]
pub fn fake_capture(c: Combo) {
    let mut cap = CAPTURE.lock().unwrap();
    (cap.active, cap.result) = (false, Some(c));
}

/// Whether another program (or Windows) already holds `c`: it's registered for a moment on the
/// calling thread and let go at once. A key this app holds itself reads as held too, so ask
/// only about keys the app hasn't registered.
pub fn held_elsewhere(c: Combo) -> bool {
    const PROBE: i32 = 0xB000;
    unsafe {
        if RegisterHotKey(None, PROBE, c.modifiers() | MOD_NOREPEAT, c.vk as u32).is_ok() {
            let _ = UnregisterHotKey(None, PROBE);
            false
        } else {
            true
        }
    }
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
    // Only keys pressed in our own window count: switching away (Alt+Tab to the game) cancels.
    let ours = crate::overlay::hwnd().is_none_or(|h| unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() } == h);
    let alt_tab = down(0x12) && down(0x09);
    if !ours || alt_tab {
        c.active = false;
        c.ended = Some(std::time::Instant::now());
        c.result = None;
        if let Some(f) = &c.on_done {
            f();
        }
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
            c.ended = Some(std::time::Instant::now());
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

    /// A key someone holds reads as held, and free once they let go. Ctrl+Alt+Shift+F23, which
    /// nothing uses, stands in for the other program.
    #[test]
    fn finds_a_held_key() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
        let c = Combo { vk: 0x86, ctrl: true, alt: true, shift: true, win: false };
        assert!(!super::held_elsewhere(c), "free to begin with");
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let other = std::thread::spawn(move || unsafe {
            RegisterHotKey(None, 0xA1, c.modifiers(), c.vk as u32).unwrap();
            held_tx.send(()).unwrap();
            let _ = done_rx.recv();
            let _ = UnregisterHotKey(None, 0xA1);
        });
        held_rx.recv().unwrap();
        assert!(super::held_elsewhere(c), "held by the other thread");
        done_tx.send(()).unwrap();
        other.join().unwrap();
        assert!(!super::held_elsewhere(c), "free again");
    }

    /// By hand on a PC with NVIDIA's overlay: shares Alt+R (NVIDIA's performance overlay) and
    /// Alt+F1 (its screenshot) and prints each press heard here for 30 s. Run with
    /// `cargo test --release live_shared_keys -- --ignored --nocapture` and press them.
    #[test]
    #[ignore]
    fn live_shared_keys() {
        use super::{Action, Held, install, set_combo};
        install(|a| println!("heard {a:?}")).unwrap();
        for (a, k) in [(Action::Record, "Alt+R"), (Action::Screenshot, "Alt+F1")] {
            let held = set_combo(a, Combo::parse(k)).unwrap();
            println!("{k}: {}", if held == Held::Shared { "shared with the program that holds it" } else { "free, this app's alone" });
        }
        println!("Press Alt+R and Alt+F1 now (30 s)...");
        std::thread::sleep(std::time::Duration::from_secs(30));
    }

    /// A shared key fires once when it's pressed with exactly its modifiers, not while it repeats,
    /// and again after it's let go.
    #[test]
    fn shared_key_fires_once_per_press() {
        use super::{Action, on_key};
        let alt_f9 = Combo { vk: 0x78, ctrl: false, alt: true, shift: false, win: false };
        let keys = [(Action::Record, alt_f9)];
        let alt = (false, true, false, false);
        let mut down = Vec::new();
        assert_eq!(on_key(&mut down, &keys, 0x78, false, (false, false, false, false)), None, "F9 alone");
        assert_eq!(on_key(&mut down, &keys, 0x78, true, alt), None);
        assert_eq!(on_key(&mut down, &keys, 0x78, false, alt), Some(Action::Record));
        assert_eq!(on_key(&mut down, &keys, 0x78, false, alt), None, "held down, repeating");
        assert_eq!(on_key(&mut down, &keys, 0x78, true, alt), None);
        assert_eq!(on_key(&mut down, &keys, 0x78, false, (true, true, false, false)), None, "Ctrl+Alt+F9 is another key");
        on_key(&mut down, &keys, 0x78, true, alt);
        assert_eq!(on_key(&mut down, &keys, 0x78, false, alt), Some(Action::Record), "pressed again");
    }

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
