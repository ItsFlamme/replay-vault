//! Optional Windows settings: Game Bar capture off, and the high-performance GPU for the private
//! OBS copy. Each value's original is saved the first time it's changed (tweaks.json in the app's
//! settings folder), so turning the option off, or Remove, puts it back exactly. All per-user
//! (HKCU), no admin needed.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_DWORD, REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW,
    RegSetKeyValueW,
};
use windows::core::HSTRING;

const GAME_BAR: [(&str, &str); 2] = [
    (r"System\GameConfigStore", "GameDVR_Enabled"),
    (r"Software\Microsoft\Windows\CurrentVersion\GameDVR", "AppCaptureEnabled"),
];
const GPU_KEY: &str = r"Software\Microsoft\DirectX\UserGpuPreferences";

fn file() -> std::path::PathBuf {
    crate::config::dir().join("tweaks.json")
}

/// "key|value" -> the original data ({"dword": n} / {"string": s}), or null if it wasn't set.
fn saved() -> BTreeMap<String, Value> {
    std::fs::read_to_string(file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn store(map: &BTreeMap<String, Value>) {
    let _ = std::fs::create_dir_all(crate::config::dir());
    let _ = std::fs::write(file(), serde_json::to_string_pretty(map).unwrap_or_default());
}

fn read(key: &str, value: &str) -> Value {
    let (k, v) = (HSTRING::from(key), HSTRING::from(value));
    let mut dword = 0u32;
    let mut size = 4u32;
    if unsafe { RegGetValueW(HKEY_CURRENT_USER, &k, &v, RRF_RT_REG_DWORD, None, Some((&mut dword as *mut u32).cast()), Some(&mut size)) }.is_ok() {
        return json!({ "dword": dword });
    }
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    if unsafe { RegGetValueW(HKEY_CURRENT_USER, &k, &v, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut size)) }.is_ok() {
        let len = (size as usize / 2).saturating_sub(1);
        return json!({ "string": String::from_utf16_lossy(&buf[..len]) });
    }
    Value::Null
}

fn write(key: &str, value: &str, data: &Value) -> anyhow::Result<()> {
    let (k, v) = (HSTRING::from(key), HSTRING::from(value));
    let result = if let Some(n) = data["dword"].as_u64() {
        let n = n as u32;
        unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, &k, &v, REG_DWORD.0, Some((&n as *const u32).cast()), 4) }
    } else if let Some(s) = data["string"].as_str() {
        let wide: Vec<u16> = s.encode_utf16().chain([0]).collect();
        unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, &k, &v, REG_SZ.0, Some(wide.as_ptr().cast()), (wide.len() * 2) as u32) }
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
    if !map.contains_key(&id) {
        map.insert(id, read(key, value));
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
        let _ = write(key, "Thing", &Value::Null);
        let _ = unsafe { windows::Win32::System::Registry::RegDeleteKeyW(HKEY_CURRENT_USER, &HSTRING::from(key)) };
    }
}
