//! Lists the Windows audio devices (playback and recording) via Core Audio, and the open apps
//! whose sound OBS can record on its own.

use crate::config::AppSel;

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{DEVICE_STATE_ACTIVE, IMMDeviceEnumerator, MMDeviceEnumerator, eCapture, eRender};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, STGM_READ};

pub struct Device {
    /// "out:<id>" or "in:<id>"; the id is what OBS's WASAPI sources call `device_id`.
    pub key: String,
    pub name: String,
}

pub fn list() -> Vec<Device> {
    let mut out = vec![
        Device { key: "out:default".into(), name: "Default playback device".into() },
        Device { key: "in:default".into(), name: "Default microphone".into() },
    ];
    unsafe {
        // Fails harmlessly if COM is already set up on this thread in another mode.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let Ok(en) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else {
            return out;
        };
        for (flow, prefix) in [(eRender, "out"), (eCapture, "in")] {
            let Ok(devices) = en.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) else { continue };
            for i in 0..devices.GetCount().unwrap_or(0) {
                let Ok(dev) = devices.Item(i) else { continue };
                let Ok(raw) = dev.GetId() else { continue };
                let id = raw.to_string().unwrap_or_default();
                CoTaskMemFree(Some(raw.0 as *const _));
                let name = dev
                    .OpenPropertyStore(STGM_READ)
                    .and_then(|store| store.GetValue(&PKEY_Device_FriendlyName))
                    .map(|v| v.to_string())
                    .unwrap_or_else(|_| id.clone());
                out.push(Device { key: format!("{prefix}:{id}"), name });
            }
        }
    }
    out
}

/// The specific devices Windows uses as the default ones right now (their ids, as in `Device::key`
/// after the "out:" or "in:"), which "out:default" and "in:default" record.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Defaults {
    pub out: Option<String>,
    /// Only when the same mic is Windows' default for both normal use and calls, so whichever of
    /// the two OBS follows, it's this one.
    pub mic: Option<String>,
}

/// Asks Windows for its default devices. Nothing is opened or changed.
pub fn defaults() -> Defaults {
    use windows::Win32::Media::Audio::{EDataFlow, ERole, eCommunications, eConsole};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let Ok(en) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else {
            return Defaults::default();
        };
        let id = |flow: EDataFlow, role: ERole| -> Option<String> {
            let raw = en.GetDefaultAudioEndpoint(flow, role).ok()?.GetId().ok()?;
            let id = raw.to_string().ok();
            CoTaskMemFree(Some(raw.0 as *const _));
            id
        };
        let mic = id(eCapture, eConsole).filter(|m| id(eCapture, eCommunications).as_ref() == Some(m));
        Defaults { out: id(eRender, eConsole), mic }
    }
}

/// Open apps whose sound can be recorded on its own: visible top-level windows with a title,
/// one per program, sorted by exe name. The exe name comes from Windows' list of running
/// programs (a snapshot), so no app or game process is ever opened.
pub fn open_apps() -> Vec<AppSel> {
    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GWL_EXSTYLE, GetClassNameW, GetWindow, GetWindowLongW, GetWindowTextW,
        GetWindowThreadProcessId, IsWindowVisible, WS_EX_TOOLWINDOW,
    };
    use windows::core::BOOL;

    unsafe extern "system" fn add(hwnd: HWND, list: LPARAM) -> BOOL {
        unsafe { (*(list.0 as *mut Vec<HWND>)).push(hwnd) };
        BOOL(1)
    }
    let mut windows: Vec<HWND> = Vec::new();
    let mut exes = std::collections::HashMap::new();
    unsafe {
        let _ = EnumWindows(Some(add), LPARAM(&mut windows as *mut _ as isize));
        if let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            let mut ok = Process32FirstW(snap, &mut e).is_ok();
            while ok {
                exes.insert(e.th32ProcessID, text(&e.szExeFile));
                ok = Process32NextW(snap, &mut e).is_ok();
            }
            let _ = CloseHandle(snap);
        }
    }
    let mut out: Vec<AppSel> = Vec::new();
    for hwnd in windows {
        let mut pid = 0;
        let (mut title, mut class) = ([0u16; 256], [0u16; 256]);
        let shown = unsafe {
            IsWindowVisible(hwnd).as_bool()
                && GetWindow(hwnd, GW_OWNER).is_err()
                && GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 == 0
                // Hidden by Windows itself (background Store apps, other virtual desktops).
                && !cloaked(hwnd)
        };
        if !shown {
            continue;
        }
        let (n, c) = unsafe { (GetWindowTextW(hwnd, &mut title), GetClassNameW(hwnd, &mut class)) };
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        let title = String::from_utf16_lossy(&title[..n.max(0) as usize]);
        let class = String::from_utf16_lossy(&class[..c.max(0) as usize]);
        let Some(exe) = exes.get(&pid).cloned() else { continue };
        if title.trim().is_empty() || pid == std::process::id() || class == "Progman" {
            continue;
        }
        if !out.iter().any(|a| a.exe.eq_ignore_ascii_case(&exe)) {
            out.push(AppSel { title, class, exe });
        }
    }
    out.sort_by_key(|a| a.exe.to_lowercase());
    out
}

fn cloaked(hwnd: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
    let mut v = 0u32;
    unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, (&mut v as *mut u32).cast(), 4) }.is_ok() && v != 0
}

fn text(w: &[u16]) -> String {
    String::from_utf16_lossy(&w[..w.iter().position(|&c| c == 0).unwrap_or(w.len())])
}

/// Voice chat for its own track: Discord, matched by its exe (window title and class are only
/// what OBS falls back on).
pub fn chat_app() -> AppSel {
    AppSel { title: "Discord".into(), class: "Chrome_WidgetWin_1".into(), exe: "Discord.exe".into() }
}

/// The "window" setting of OBS's application audio capture: "title:class:exe", with '#' and ':'
/// escaped the way OBS does.
pub fn obs_window(app: &AppSel) -> String {
    let enc = |s: &str| s.replace('#', "#22").replace(':', "#3A");
    format!("{}:{}:{}", enc(&app.title), enc(&app.class), enc(&app.exe))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obs_window_escapes_like_obs() {
        let a = AppSel { title: "#general | Server: Discord".into(), class: "Chrome_WidgetWin_1".into(), exe: "Discord.exe".into() };
        assert_eq!(obs_window(&a), "#22general | Server#3A Discord:Chrome_WidgetWin_1:Discord.exe");
    }

    /// Windows' default devices, when it has them, are among the devices listed. Read only.
    #[test]
    fn defaults_are_listed_devices() {
        let (d, all) = (defaults(), list());
        for (kind, id) in [("out", &d.out), ("in", &d.mic)] {
            if let Some(id) = id {
                assert!(all.iter().any(|dev| dev.key == format!("{kind}:{id}")), "{kind}:{id}");
            }
        }
    }

    /// Lists without error, one entry per program, never this test process.
    #[test]
    fn open_apps_lists_programs_once() {
        let apps = open_apps();
        let mut exes: Vec<String> = apps.iter().map(|a| a.exe.to_lowercase()).collect();
        exes.dedup();
        assert_eq!(exes.len(), apps.len());
        assert!(apps.iter().all(|a| !a.title.trim().is_empty() && !a.exe.is_empty()));
    }
}
