//! Plain-text app log (`%APPDATA%\replay-vault\app.log`): status changes, errors, saved
//! clips, recorder restarts and the end-of-session health report. Kept under 1 MB; the previous
//! file is kept as app.old.log.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::config;

const MAX_BYTES: u64 = 1024 * 1024;
static LOCK: Mutex<()> = Mutex::new(());

/// Tests log to a folder of their own, never into the real app.log.
fn dir() -> PathBuf {
    if cfg!(test) { std::env::temp_dir().join("instant-replay-log-test") } else { config::dir() }
}

pub fn path() -> PathBuf {
    dir().join("app.log")
}

pub fn write(msg: &str) {
    let _guard = LOCK.lock();
    let path = path();
    let _ = fs::create_dir_all(dir());
    if fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = fs::rename(&path, dir().join("app.old.log"));
    }
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let line = format!(
        "{}-{:02}-{:02} {:02}:{:02}:{:02}  {msg}\r\n",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    );
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Records panics, so a crash leaves a reason behind.
pub fn catch_panics() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        write(&format!("CRASH: {info}"));
        default(info);
    }));
}
