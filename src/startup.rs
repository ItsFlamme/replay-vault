//! Start-with-Windows registration, and finding the OBS install.

use std::path::PathBuf;

use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, REG_BINARY, REG_SZ, RRF_RT_REG_BINARY, RRF_RT_REG_SZ, RegDeleteKeyValueW,
    RegGetValueW, RegSetKeyValueW,
};
use windows::core::{HSTRING, PCWSTR, w};

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
/// Where Task Manager's Startup apps keeps which entries are turned off.
const APPROVED_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run");
/// The entry's name before the rename, which pointed at obs-instant-replay.exe. This copy moves
/// it (see `instance`).
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

/// This copy's entry name (see `instance`).
fn run_value() -> HSTRING {
    HSTRING::from(crate::instance::current().run_value())
}

/// Whether the app starts with Windows: its entry is there and not turned off in Task Manager.
pub fn is_enabled() -> bool {
    has_entry() && !turned_off_in_task_manager()
}

fn has_entry() -> bool {
    read_string(HKEY_CURRENT_USER, RUN_KEY, PCWSTR(run_value().as_ptr())).is_some()
}

/// Whether the entry is there but turned off in Task Manager's Startup apps (or Settings > Apps >
/// Startup), so Windows skips it.
pub fn turned_off_in_task_manager() -> bool {
    has_entry() && read_binary(HKEY_CURRENT_USER, APPROVED_KEY, &run_value()).is_some_and(|b| approved(&b) == Some(false))
}

/// What Task Manager keeps for an entry: on or off by the first byte (2 or 6 on, 3 or 7 off: the
/// low bit means off), then when it was turned off. None if there's nothing to read.
fn approved(bytes: &[u8]) -> Option<bool> {
    bytes.first().map(|b| b & 1 == 0)
}

/// Task Manager's "on" for an entry: what it writes itself when one is turned back on.
const APPROVED_ON: [u8; 12] = [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

fn read_binary(root: HKEY, key: PCWSTR, value: &HSTRING) -> Option<Vec<u8>> {
    let mut buf = [0u8; 64];
    let mut size = buf.len() as u32;
    let err = unsafe { RegGetValueW(root, key, value, RRF_RT_REG_BINARY, None, Some(buf.as_mut_ptr().cast()), Some(&mut size)) };
    err.is_ok().then(|| buf[..(size as usize).min(buf.len())].to_vec())
}

/// Adds or removes this exe from the current user's Windows startup list. Adding it also turns it
/// back on if it was turned off in Task Manager, so it really starts.
pub fn set_enabled(on: bool) -> anyhow::Result<()> {
    if !on {
        let _ = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, &run_value()) };
        return Ok(());
    }
    write_entry()?;
    if turned_off_in_task_manager() {
        unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, APPROVED_KEY, &run_value(), REG_BINARY.0, Some(APPROVED_ON.as_ptr().cast()), APPROVED_ON.len() as u32) }.ok()?;
    }
    Ok(())
}

/// Points the startup entry at this exe, leaving Task Manager's on or off as it is.
fn write_entry() -> anyhow::Result<()> {
    let cmd = format!("\"{}\" --minimized\0", std::env::current_exe()?.display());
    let wide: Vec<u16> = cmd.encode_utf16().collect();
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            &run_value(),
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
    if crate::instance::current().legacy_names_allowed() && read_string(HKEY_CURRENT_USER, RUN_KEY, OLD_RUN_VALUE).is_some() {
        let _ = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, OLD_RUN_VALUE) };
        match write_entry() {
            Ok(()) => crate::log::write("Moved the Start with Windows entry over to Replay Vault."),
            Err(e) => crate::log::write(&format!("Couldn't move the Start with Windows entry: {e:#}")),
        }
    }
    let Some(cmd) = read_string(HKEY_CURRENT_USER, RUN_KEY, PCWSTR(run_value().as_ptr())) else { return };
    let exe = cmd.trim_start_matches('"').split('"').next().unwrap_or_default();
    if !std::path::Path::new(exe).exists() {
        match write_entry() {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Task Manager's bytes: 2 or 6 first is on, 3 or 7 is off (with when, after it), nothing is unknown.
    #[test]
    fn task_manager_on_or_off() {
        let off = [3, 0, 0, 0, 0x10, 0x2f, 0x6a, 0x1c, 0x4e, 0x5a, 0xdb, 0x01];
        assert_eq!(approved(&off), Some(false));
        assert_eq!(approved(&[7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]), Some(false));
        assert_eq!(approved(&APPROVED_ON), Some(true), "what's written to turn it back on");
        assert_eq!(approved(&[6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]), Some(true));
        assert_eq!(approved(&[]), None);
    }
}
