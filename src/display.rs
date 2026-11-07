//! Lists monitors with the IDs OBS's Display Capture uses, plus resolution, refresh rate and
//! whether HDR is on.

use windows::Win32::Graphics::Gdi::{
    DEVMODEW, DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, DISPLAY_DEVICE_PRIMARY_DEVICE, DISPLAY_DEVICEW,
    ENUM_CURRENT_SETTINGS, EnumDisplayDevicesW, EnumDisplaySettingsW,
};
use windows::Win32::UI::WindowsAndMessaging::EDD_GET_DEVICE_INTERFACE_NAME;
use windows::core::PCWSTR;

#[derive(Clone)]
pub struct Monitor {
    /// Device interface path, e.g. `\\?\DISPLAY#MSI3CD8#...#{e6f07b5f-...}`; OBS calls it `monitor_id`.
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub hz: u32,
    /// Top-left corner on the desktop, in pixels.
    pub x: i32,
    pub y: i32,
    pub primary: bool,
    /// HDR is on: Windows' brightness for normal (SDR) content on this monitor, in nits.
    pub hdr: Option<u32>,
}

impl Monitor {
    pub fn label(&self) -> String {
        // The model code sits between the first two '#'s, e.g. MSI3CD8.
        let model = self.id.split('#').nth(1).unwrap_or("Display");
        let primary = if self.primary { ", primary" } else { "" };
        format!("{model}: {}x{} @ {} Hz{primary}", self.width, self.height, self.hz)
    }
}

fn wide_to_string(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

pub fn list() -> Vec<Monitor> {
    monitors(true)
}

/// The monitors; with `hdr` false their HDR state isn't looked up (`hdr` is None), which skips
/// the much slower DXGI and display config queries.
fn monitors(hdr: bool) -> Vec<Monitor> {
    let mut out = Vec::new();
    let hdr = if hdr { hdr_monitors() } else { Vec::new() };
    for i in 0.. {
        let mut adapter = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
        if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), i, &mut adapter, 0) }.as_bool() {
            break;
        }
        if (adapter.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP).0 == 0 {
            continue;
        }
        let name = PCWSTR(adapter.DeviceName.as_ptr());
        let mut mode = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
        if !unsafe { EnumDisplaySettingsW(name, ENUM_CURRENT_SETTINGS, &mut mode) }.as_bool() {
            continue;
        }
        let mut mon = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
        if !unsafe { EnumDisplayDevicesW(name, 0, &mut mon, EDD_GET_DEVICE_INTERFACE_NAME) }.as_bool() {
            continue;
        }
        let pos = unsafe { mode.Anonymous1.Anonymous2.dmPosition };
        out.push(Monitor {
            id: wide_to_string(&mon.DeviceID),
            width: mode.dmPelsWidth,
            height: mode.dmPelsHeight,
            hz: mode.dmDisplayFrequency,
            x: pos.x,
            y: pos.y,
            primary: (adapter.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE).0 != 0,
            hdr: hdr.iter().find(|(gdi, _)| *gdi == wide_to_string(&adapter.DeviceName)).map(|&(_, nits)| nits),
        });
    }
    out.sort_by_key(|m| (!m.primary, m.x));
    out
}

/// Monitors with HDR on, by GDI name (`\\.\DISPLAY1`), with Windows' brightness for SDR
/// content on them (300 nits, OBS's default, if Windows doesn't say). HDR is on when DXGI
/// reports the HDR10 (PQ) color space for the output.
fn hdr_monitors() -> Vec<(String, u32)> {
    use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput6};
    use windows::core::Interface;
    let mut out = Vec::new();
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else { return out };
    let mut a = 0;
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(a) } {
        let mut o = 0;
        while let Ok(output) = unsafe { adapter.EnumOutputs(o) } {
            if let Ok(desc) = output.cast::<IDXGIOutput6>().and_then(|o| unsafe { o.GetDesc1() })
                && desc.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020
            {
                let gdi = wide_to_string(&desc.DeviceName);
                let nits = sdr_white(&gdi).unwrap_or(300);
                out.push((gdi, nits));
            }
            o += 1;
        }
        a += 1;
    }
    out
}

/// Windows' "SDR content brightness" for a monitor (by GDI name), in nits.
fn sdr_white(gdi: &str) -> Option<u32> {
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SDR_WHITE_LEVEL, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
    };
    let (mut np, mut nm) = (0, 0);
    unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) }.ok().ok()?;
    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
    unsafe { QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None) }.ok().ok()?;
    let header = |kind, size: usize, adapter, id| DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: kind, size: size as u32, adapterId: adapter, id };
    paths.iter().take(np as usize).find_map(|p| {
        let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: header(DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(), p.sourceInfo.adapterId, p.sourceInfo.id),
            ..Default::default()
        };
        if unsafe { DisplayConfigGetDeviceInfo(&mut src.header) } != 0 || wide_to_string(&src.viewGdiDeviceName) != gdi {
            return None;
        }
        let mut level = DISPLAYCONFIG_SDR_WHITE_LEVEL {
            header: header(DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>(), p.targetInfo.adapterId, p.targetInfo.id),
            ..Default::default()
        };
        // Windows gives it in thousandths of 80 nits.
        (unsafe { DisplayConfigGetDeviceInfo(&mut level.header) } == 0).then(|| level.SDRWhiteLevel * 80 / 1000)
    })
}

/// The monitor holding the desktop point (x, y), if any.
pub fn at(mons: &[Monitor], x: i32, y: i32) -> Option<&Monitor> {
    mons.iter().find(|m| (m.x..m.x + m.width as i32).contains(&x) && (m.y..m.y + m.height as i32).contains(&y))
}

/// The monitor the mouse pointer is on (without its HDR state; it's asked every second). Only
/// reads the pointer position.
pub fn under_pointer() -> Option<Monitor> {
    let mut p = windows::Win32::Foundation::POINT::default();
    unsafe { windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut p) }.ok()?;
    at(&monitors(false), p.x, p.y).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(id: &str, x: i32, y: i32, width: u32, height: u32) -> Monitor {
        Monitor { id: id.into(), width, height, hz: 60, x, y, primary: x == 0 && y == 0, hdr: None }
    }

    /// A 1440p main screen with a 1080p one to its left, set lower, and a gap below that one.
    #[test]
    fn pointer_maps_to_its_monitor() {
        let mons = [mon("main", 0, 0, 2560, 1440), mon("left", -1920, 200, 1920, 1080)];
        let id = |x, y| at(&mons, x, y).map(|m| m.id.as_str());
        assert_eq!(id(0, 0), Some("main"));
        assert_eq!(id(2559, 1439), Some("main"));
        assert_eq!(id(2560, 100), None);
        assert_eq!(id(-1, 200), Some("left"));
        assert_eq!(id(-1920, 1279), Some("left"));
        assert_eq!(id(-5, 100), None);
    }

    /// Windows reports an SDR brightness for a connected monitor, HDR on or not (80 to 480 nits
    /// in its settings).
    #[test]
    fn sdr_white_reads() {
        let nits = sdr_white(r"\\.\DISPLAY1").or_else(|| (2..=8).find_map(|i| sdr_white(&format!(r"\\.\DISPLAY{i}"))));
        assert!(nits.is_some_and(|n| (80..=480).contains(&n)), "{nits:?}");
    }
}
