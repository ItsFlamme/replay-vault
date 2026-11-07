//! Start-with-Windows registration, and finding the OBS install.

use std::path::PathBuf;

use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW,
    RegSetKeyValueW,
};
use windows::core::{PCWSTR, w};

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const RUN_VALUE: PCWSTR = w!("Replay Vault");
/// The entry's name before the rename, which pointed at obs-instant-replay.exe.
const OLD_RUN_VALUE: PCWSTR = w!("OBS Instant Replay");

pub fn read_string(root: HKEY, key: PCWSTR, value: PCWSTR) -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let err = unsafe {
        RegGetValueW(root, key, value, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut size))
    };
    if err.is_err() {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1); // size includes the terminating NUL
    Some(String::from_utf16_lossy(&buf[..len]))
}

pub fn is_enabled() -> bool {
    read_string(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE).is_some()
}

/// Adds or removes this exe from the current user's Windows startup list.
pub fn set_enabled(on: bool) -> anyhow::Result<()> {
    if !on {
        let _ = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE) };
        return Ok(());
    }
    let cmd = format!("\"{}\" --minimized\0", std::env::current_exe()?.display());
    let wide: Vec<u16> = cmd.encode_utf16().collect();
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            RUN_VALUE,
            REG_SZ.0,
            Some(wide.as_ptr().cast()),
            (wide.len() * 2) as u32,
        )
    }
    .ok()?;
    Ok(())
}

/// If the startup entry points at an exe that no longer exists (the app was moved), point it here.
pub fn repair() {
    // From before the rename: swap the old entry for one pointing at this exe.
    if read_string(HKEY_CURRENT_USER, RUN_KEY, OLD_RUN_VALUE).is_some() {
        let _ = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, OLD_RUN_VALUE) };
        match set_enabled(true) {
            Ok(()) => crate::log::write("Moved the Start with Windows entry over to Replay Vault."),
            Err(e) => crate::log::write(&format!("Couldn't move the Start with Windows entry: {e:#}")),
        }
    }
    let Some(cmd) = read_string(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE) else { return };
    let exe = cmd.trim_start_matches('"').split('"').next().unwrap_or_default();
    if !std::path::Path::new(exe).exists() {
        match set_enabled(true) {
            Ok(()) => crate::log::write(&format!("Start with Windows pointed at a missing file ({exe}); fixed it.")),
            Err(e) => crate::log::write(&format!("Couldn't fix the Start with Windows entry: {e:#}")),
        }
    }
}

pub fn obs_exe() -> Option<PathBuf> {
    let dir = read_string(HKEY_LOCAL_MACHINE, w!(r"SOFTWARE\OBS Studio"), PCWSTR::null())?;
    let exe = PathBuf::from(dir).join(r"bin\64bit\obs64.exe");
    exe.exists().then_some(exe)
}
