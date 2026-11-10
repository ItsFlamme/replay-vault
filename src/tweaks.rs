//! Optional Windows settings: Game Bar capture off, NVIDIA's overlay off, AMD Software's Instant
//! Replay and hotkeys off (untested), and the
//! high-performance GPU for the private OBS copy. Each value's original is saved the first time it's changed (tweaks.json in the app's
//! settings folder), so turning the option off, or Remove, puts it back exactly. All per-user
//! (HKCU), no admin needed.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};
use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_BINARY, REG_DWORD, REG_SZ, REG_VALUE_TYPE, RRF_NOEXPAND, RRF_RT_ANY, RegDeleteKeyValueW, RegGetValueW,
    RegSetKeyValueW,
};
use windows::core::HSTRING;

const GAME_BAR: [(&str, &str); 2] = [
    (r"System\GameConfigStore", "GameDVR_Enabled"),
    (r"Software\Microsoft\Windows\CurrentVersion\GameDVR", "AppCaptureEnabled"),
];
const GPU_KEY: &str = r"Software\Microsoft\DirectX\UserGpuPreferences";
/// What the NVIDIA app's Settings > Features > NVIDIA Overlay switch changes (found by flipping it
/// and comparing the registry): both values go from 1 to 0, as 4-byte binary.
const NVIDIA_KEY: &str = r"Software\NVIDIA Corporation\Global\ShadowPlay\NVSPCAPS";
const NVIDIA_OVERLAY: [&str; 2] = ["IsShadowPlayEnabled", "IsShadowPlayEnabledUser"];

/// Tests use a file of their own, never the real record of what to give back.
fn file() -> std::path::PathBuf {
    if cfg!(test) { std::env::temp_dir().join("instant-replay-tweaks-test.json") } else { crate::config::dir().join("tweaks.json") }
}

/// "key|value" -> the original data (see `read`), or null if it wasn't set. A damaged file is
/// kept as tweaks.bad.json before anything overwrites it, so the originals in it aren't lost.
fn saved() -> BTreeMap<String, Value> {
    let Ok(raw) = std::fs::read_to_string(file()) else { return BTreeMap::new() };
    serde_json::from_str(&raw).unwrap_or_else(|_| {
        let _ = std::fs::copy(file(), crate::config::dir().join("tweaks.bad.json"));
        BTreeMap::new()
    })
}

fn store(map: &BTreeMap<String, Value>) {
    let _ = std::fs::create_dir_all(crate::config::dir());
    let _ = std::fs::write(file(), serde_json::to_string_pretty(map).unwrap_or_default());
}

/// The value as it is: {"dword"}, {"string"} or {"binary"}, any other type as its raw
/// {"type", "bytes"}, null only when it isn't there, and {"unreadable": true} when Windows wouldn't
/// say (never mistaken for "not there", so restoring can't delete it).
fn read(key: &str, value: &str) -> Value {
    let (k, v) = (HSTRING::from(key), HSTRING::from(value));
    let flags = RRF_RT_ANY | RRF_NOEXPAND;
    let mut kind = REG_VALUE_TYPE(0);
    let mut size = 0u32;
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, &k, &v, flags, Some(&mut kind), None, Some(&mut size)) };
    if r == ERROR_FILE_NOT_FOUND {
        return Value::Null;
    }
    // A little spare room: the value can grow between the two calls.
    let mut buf = vec![0u8; size as usize + 64];
    let mut size = buf.len() as u32;
    let r2 = unsafe { RegGetValueW(HKEY_CURRENT_USER, &k, &v, flags, Some(&mut kind), Some(buf.as_mut_ptr().cast()), Some(&mut size)) };
    if r.is_err() || r2.is_err() {
        return json!({ "unreadable": true });
    }
    buf.truncate(size as usize);
    match kind {
        REG_DWORD if buf.len() == 4 => json!({ "dword": u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) }),
        REG_SZ => {
            let wide: Vec<u16> = buf.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
            json!({ "string": String::from_utf16_lossy(&wide).trim_end_matches('\0') })
        }
        REG_BINARY => json!({ "binary": buf }),
        other => json!({ "type": other.0, "bytes": buf }),
    }
}

fn write(key: &str, value: &str, data: &Value) -> anyhow::Result<()> {
    let (k, v) = (HSTRING::from(key), HSTRING::from(value));
    let result = if let Some(n) = data["dword"].as_u64() {
        let n = n as u32;
        unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, &k, &v, REG_DWORD.0, Some((&n as *const u32).cast()), 4) }
    } else if let Some(s) = data["string"].as_str() {
        let wide: Vec<u16> = s.encode_utf16().chain([0]).collect();
        unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, &k, &v, REG_SZ.0, Some(wide.as_ptr().cast()), (wide.len() * 2) as u32) }
    } else if let Some(b) = data["binary"].as_array() {
        let bytes: Vec<u8> = b.iter().map(|x| x.as_u64().unwrap_or(0) as u8).collect();
        unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, &k, &v, REG_BINARY.0, Some(bytes.as_ptr().cast()), bytes.len() as u32) }
    } else if let (Some(kind), Some(b)) = (data["type"].as_u64(), data["bytes"].as_array()) {
        let bytes: Vec<u8> = b.iter().map(|x| x.as_u64().unwrap_or(0) as u8).collect();
        unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, &k, &v, kind as u32, Some(bytes.as_ptr().cast()), bytes.len() as u32) }
    } else if !data.is_null() {
        // The original couldn't be read, or the saved copy is something unknown: leave the value
        // as it is rather than delete what may be the user's own setting.
        crate::log::write(&format!("Left {key}\\{value} as it is: its original wasn't readable, so it isn't put back."));
        return Ok(());
    } else {
        // Wasn't there before: take it away again.
        let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &k, &v) };
        if r.0 == 2 { windows::Win32::Foundation::WIN32_ERROR(0) } else { r } // 2 = already gone
    };
    result.ok().map_err(|e| anyhow::anyhow!("couldn't change {key}\\{value}: {}", e.message()))
}

/// Sets a value, keeping its original the first time.
fn change(key: &str, value: &str, data: Value) -> anyhow::Result<()> {
    let mut map = saved();
    let id = format!("{key}|{value}");
    if let std::collections::btree_map::Entry::Vacant(e) = map.entry(id) {
        let original = read(key, value);
        // Without its original it couldn't be put back, so it isn't changed.
        anyhow::ensure!(original.get("unreadable").is_none(), "couldn't read {key}\\{value}, so it was left as it is");
        e.insert(original);
        store(&map);
    }
    write(key, value, &data)
}

/// Puts one value back to its original, if this app ever changed it.
fn restore(key: &str, value: &str) -> anyhow::Result<()> {
    let mut map = saved();
    let Some(original) = map.remove(&format!("{key}|{value}")) else { return Ok(()) };
    write(key, value, &original)?;
    store(&map);
    Ok(())
}

/// Game Bar's background capture fights the recorder for the GPU encoder; some players also get
/// more FPS with it off. `off = false` puts the user's own setting back.
pub fn set_game_bar_off(off: bool) -> anyhow::Result<()> {
    for (key, value) in GAME_BAR {
        if off { change(key, value, json!({ "dword": 0 }))? } else { restore(key, value)? }
    }
    Ok(())
}

/// Whether NVIDIA's overlay (with its Instant Replay and Alt+Z, Alt+F10... keys) is switched on.
/// False on PCs without the NVIDIA app.
pub fn nvidia_overlay_on() -> bool {
    NVIDIA_OVERLAY.iter().any(|v| read(NVIDIA_KEY, v)["binary"].get(0).and_then(Value::as_u64).is_some_and(|b| b != 0))
}

/// Switches NVIDIA's overlay off the way its own settings switch does, so its hotkeys stop
/// firing alongside ours. NVIDIA reads it when its overlay starts, so a running one is closed with
/// `close_nvidia_overlay`. `off = false` puts the user's own setting back (it shows again once
/// switched on in the NVIDIA app, or after a restart).
pub fn set_nvidia_overlay_off(off: bool) -> anyhow::Result<()> {
    for value in NVIDIA_OVERLAY {
        if !off {
            restore(NVIDIA_KEY, value)?;
        } else if !read(NVIDIA_KEY, value).is_null() {
            // Only ever a value NVIDIA wrote itself, never one it doesn't have.
            change(NVIDIA_KEY, value, json!({ "binary": [0, 0, 0, 0] }))?;
        }
    }
    Ok(())
}

/// Windows' list of running programs: (id, parent id, exe name). A snapshot; nothing is opened.
pub fn programs() -> Vec<(u32, u32, String)> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            out.push((e.th32ProcessID, e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..len])));
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

const NVIDIA_OVERLAY_EXE: &str = "NVIDIA Overlay.exe";

/// Whether NVIDIA's overlay program is running.
pub fn nvidia_overlay_running() -> bool {
    programs().iter().any(|(_, _, name)| name.eq_ignore_ascii_case(NVIDIA_OVERLAY_EXE))
}

/// NVIDIA's shortcuts as its overlay saves them in NVIDIA_KEY: `{name}Count` keys, held in
/// `{name}0`, `{name}1`..., each a virtual-key code as 4-byte binary. With the names NVIDIA shows.
const NVIDIA_SHORTCUTS: [(&str, &str); 10] = [
    ("GFEOverlayHKeyV2", "Open NVIDIA overlay"),
    ("ScreenshotHKey", "Screenshot"),
    ("ManualHKey", "Start/stop recording"),
    ("DVRHKey", "Save Instant Replay"),
    ("IRToggleHKey", "Instant Replay on/off"),
    ("AnselUIHKey", "Photo mode"),
    ("FSUIHKeyV1", "Game filters"),
    ("PMOCOverlayHKey", "Performance overlay"),
    ("MicToggleHKey", "Microphone on/off"),
    ("ChatUIToggleHKey", "Chat"),
];

/// NVIDIA's helper that holds its shortcuts. It keeps them even with the overlay closed or
/// switched off, and NVIDIA restarts it if it's closed.
const NVIDIA_HELPER_EXE: &str = "nvsphelper64.exe";

/// The NVIDIA shortcut that uses `c`, while NVIDIA's helper is running and holding them.
pub fn nvidia_shortcut_for(c: crate::hotkey::Combo) -> Option<&'static str> {
    if !programs().iter().any(|(_, _, name)| name.eq_ignore_ascii_case(NVIDIA_HELPER_EXE)) {
        return None;
    }
    let byte = |value: String| read(NVIDIA_KEY, &value)["binary"].get(0).and_then(Value::as_u64);
    NVIDIA_SHORTCUTS.iter().find_map(|(name, label)| {
        let count = byte(format!("{name}Count")).unwrap_or(0);
        let vks: Vec<u64> = (0..count).filter_map(|i| byte(format!("{name}{i}"))).collect();
        (nvidia_combo(&vks) == Some(c)).then_some(*label)
    })
}

/// NVIDIA's key list (modifiers first, e.g. [18, 112] for Alt+F1) as keys, or None if it's empty.
fn nvidia_combo(vks: &[u64]) -> Option<crate::hotkey::Combo> {
    let mut c = crate::hotkey::Combo { vk: 0, ctrl: false, alt: false, shift: false, win: false };
    for &vk in vks {
        match vk {
            16 => c.shift = true,
            17 => c.ctrl = true,
            18 => c.alt = true,
            91 | 92 => c.win = true,
            _ => c.vk = u16::try_from(vk).ok()?,
        }
    }
    (c.vk != 0).then_some(c)
}

/// NVIDIA's running overlay programs and the NVIDIA host (an nvcontainer.exe) that started them,
/// host first.
fn overlay_and_host(list: &[(u32, u32, String)]) -> Vec<u32> {
    let overlays: Vec<&(u32, u32, String)> = list.iter().filter(|(_, _, n)| n.eq_ignore_ascii_case(NVIDIA_OVERLAY_EXE)).collect();
    let mut ids: Vec<u32> = Vec::new();
    for (_, parent, _) in &overlays {
        if list.iter().any(|(id, _, n)| id == parent && n.eq_ignore_ascii_case("nvcontainer.exe")) && !ids.contains(parent) {
            ids.push(*parent);
        }
    }
    ids.extend(overlays.iter().map(|(id, ..)| *id));
    ids
}

/// Closes NVIDIA's running overlay and the host that started it, so the switch takes effect
/// without a restart: NVIDIA starts the host again by itself, and with the switch off the host no
/// longer starts the overlay (tried on 2026-11-07, NVIDIA app 11.0). Both run as the user, so no
/// admin is needed. NVIDIA's capture helper (nvsphelper64) runs under a system service and is left
/// alone. True if nothing of the overlay is left running.
pub fn close_nvidia_overlay() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    for id in overlay_and_host(&programs()) {
        unsafe {
            if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, id) {
                let _ = TerminateProcess(h, 0);
                let _ = CloseHandle(h);
            }
        }
    }
    // The overlay's helper processes go with it; give them a moment.
    for _ in 0..20 {
        if !nvidia_overlay_running() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

/// Starts NVIDIA's overlay again if its setting is on and it isn't running. Starting its program
/// hands over to the NVIDIA host, which then runs the overlay as usual, same as the NVIDIA app's
/// switch does (tried on 2026-11-07, NVIDIA app 11.0).
pub fn start_nvidia_overlay() {
    if !nvidia_overlay_on() || nvidia_overlay_running() {
        return;
    }
    let exe = Path::new(&std::env::var("ProgramFiles").unwrap_or(r"C:\Program Files".into())).join(r"NVIDIA Corporation\NVIDIA App\CEF").join(NVIDIA_OVERLAY_EXE);
    if exe.exists() {
        let _ = std::process::Command::new(exe).spawn();
    }
}

/// When the app closes: if it switched NVIDIA's overlay off, puts the user's setting back and
/// starts the overlay again, so NVIDIA's keys work while the app isn't running. The next start
/// switches it off again.
pub fn give_back_nvidia_overlay() {
    if saved().keys().any(|k| k.starts_with(&format!("{NVIDIA_KEY}|"))) && set_nvidia_overlay_off(false).is_ok() {
        start_nvidia_overlay();
    }
}

/// AMD Software (Adrenalin) keeps its Record & Stream settings here (amd.rs reads them). The user's
/// own values win over the machine-wide ones, so these are set per user.
const AMD_KEY: &str = r"Software\AMD\DVR";
/// Instant Replay off, and Settings > Hotkeys > Use Hotkeys off (which stops Alt+Z, Alt+R and the
/// rest, the in-game overlay's keys included).
const AMD_OFF: [(&str, u32); 2] = [("InstantReplayEnabled", 0), ("HotkeysDisabled", 1)];

/// Switches AMD Software's Instant Replay and hotkeys off. UNTESTED: written from the values amd.rs
/// reads, never tried on a PC with AMD Software. Nothing is closed, so AMD Software picks it up
/// when it next starts (a PC restart). Unlike NVIDIA's it stays off while the app is closed: put
/// back only when unticked or on Remove. `off = false` puts the user's own values back.
pub fn set_amd_off(off: bool) -> anyhow::Result<()> {
    for (value, data) in AMD_OFF {
        if off { change(AMD_KEY, value, json!({ "dword": data }))? } else { restore(AMD_KEY, value)? }
    }
    Ok(())
}

/// Windows' per-app graphics preference (Settings > Display > Graphics) for `exe`: "High
/// performance" on a PC with two GPUs, so the recorder encodes on the one the game runs on.
pub fn set_gpu_high(exe: &Path, on: bool) -> anyhow::Result<()> {
    let value = exe.display().to_string();
    if on { change(GPU_KEY, &value, json!({ "string": "GpuPreference=2;" })) } else { restore(GPU_KEY, &value) }
}

/// Puts back every value this app ever changed (for Remove).
pub fn restore_all() -> anyhow::Result<()> {
    for id in saved().keys() {
        if let Some((key, value)) = id.split_once('|') {
            restore(key, value)?;
        }
    }
    let _ = std::fs::remove_file(file());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvidia_key_lists() {
        use crate::hotkey::Combo;
        assert_eq!(nvidia_combo(&[18, 112]), Combo::parse("Alt+F1"));
        assert_eq!(nvidia_combo(&[17, 18, 77]), Combo::parse("Ctrl+Alt+M"));
        assert_eq!(nvidia_combo(&[18, 16, 90]), Combo::parse("Alt+Shift+Z"));
        assert_eq!(nvidia_combo(&[192]), Some(Combo { vk: 192, ctrl: false, alt: false, shift: false, win: false }));
        assert_eq!(nvidia_combo(&[]), None);
        assert_eq!(nvidia_combo(&[18]), None);
    }

    /// Round trip on a value of our own under HKCU, never a real Windows setting.
    #[test]
    fn change_then_restore_puts_it_back() {
        let key = r"Software\obs-instant-replay-test";
        let _ = write(key, "Thing", &json!({ "dword": 7 }));
        change(key, "Thing", json!({ "dword": 0 })).unwrap();
        assert_eq!(read(key, "Thing"), json!({ "dword": 0 }));
        restore(key, "Thing").unwrap();
        assert_eq!(read(key, "Thing"), json!({ "dword": 7 }));
        // A value that didn't exist is removed again.
        change(key, "New", json!({ "string": "x" })).unwrap();
        restore(key, "New").unwrap();
        assert_eq!(read(key, "New"), Value::Null);
        // Binary values, the kind NVIDIA uses.
        let _ = write(key, "Bin", &json!({ "binary": [1, 0, 0, 0] }));
        change(key, "Bin", json!({ "binary": [0, 0, 0, 0] })).unwrap();
        assert_eq!(read(key, "Bin"), json!({ "binary": [0, 0, 0, 0] }));
        restore(key, "Bin").unwrap();
        assert_eq!(read(key, "Bin"), json!({ "binary": [1, 0, 0, 0] }));
        // Other types and long values come back exactly, never deleted.
        let qword = json!({ "type": 11, "bytes": [1, 2, 3, 4, 5, 6, 7, 8] });
        let _ = write(key, "Q", &qword);
        assert_eq!(read(key, "Q"), qword);
        change(key, "Q", json!({ "dword": 0 })).unwrap();
        restore(key, "Q").unwrap();
        assert_eq!(read(key, "Q"), qword);
        let long = json!({ "string": "x".repeat(3000) });
        let _ = write(key, "Long", &long);
        change(key, "Long", json!({ "string": "y" })).unwrap();
        restore(key, "Long").unwrap();
        assert_eq!(read(key, "Long"), long);
        // A saved original that couldn't be read leaves the value alone.
        let _ = write(key, "Kept", &json!({ "dword": 5 }));
        write(key, "Kept", &json!({ "unreadable": true })).unwrap();
        assert_eq!(read(key, "Kept"), json!({ "dword": 5 }));
        for v in ["Bin", "Thing", "Q", "Long", "Kept"] {
            let _ = write(key, v, &Value::Null);
        }
        let _ = unsafe { windows::Win32::System::Registry::RegDeleteKeyW(HKEY_CURRENT_USER, &HSTRING::from(key)) };
    }

    /// Only the overlay and the nvcontainer that started it are picked, never NVIDIA's other
    /// containers or helpers (made-up process list).
    #[test]
    fn picks_only_the_overlay_and_its_host() {
        let list = [
            (1, 0, "services.exe"),
            (10, 1, "nvcontainer.exe"),
            (20, 10, "nvcontainer.exe"),
            (21, 10, "nvcontainer.exe"),
            (30, 20, "NVIDIA Overlay.exe"),
            (31, 30, "NVIDIA Overlay.exe"),
            (40, 10, "nvsphelper64.exe"),
            (50, 99, "nvidia overlay.exe"),
        ]
        .map(|(a, b, n)| (a, b, n.to_string()));
        assert_eq!(overlay_and_host(&list), [20, 30, 31, 50]);
        assert!(overlay_and_host(&list[..4]).is_empty());
    }
}
