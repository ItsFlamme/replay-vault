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
    /// The name Windows shows in its display settings, from the monitor itself (EDID), like
    /// "MAG274QRF"; empty if Windows doesn't have one.
    pub name: String,
}

impl Monitor {
    /// "Main monitor: MAG274QRF (2560x1440, 144 Hz)". Without a name, the model code from the
    /// device path stands in (between its first two '#'s, e.g. MSI3CD8).
    pub fn label(&self) -> String {
        let name = if self.name.trim().is_empty() { self.id.split('#').nth(1).unwrap_or("Monitor") } else { self.name.trim() };
        let main = if self.primary { "Main monitor: " } else { "" };
        format!("{main}{name} ({}x{}, {} Hz)", self.width, self.height, self.hz)
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
    let (hdr, names) = if hdr { (hdr_monitors(), friendly_names()) } else { (Vec::new(), Vec::new()) };
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
        let id = wide_to_string(&mon.DeviceID);
        out.push(Monitor {
            name: names.iter().find(|(path, _)| path.eq_ignore_ascii_case(&id)).map(|(_, n)| n.clone()).unwrap_or_default(),
            id,
            width: mode.dmPelsWidth,
            height: mode.dmPelsHeight,
            hz: nominal_hz(mode.dmDisplayFrequency),
            x: pos.x,
            y: pos.y,
            primary: (adapter.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE).0 != 0,
            hdr: hdr.iter().find(|(gdi, _)| *gdi == wide_to_string(&adapter.DeviceName)).map(|&(_, nits)| nits),
        });
    }
    out.sort_by_key(|m| (!m.primary, m.x));
    out
}

/// Windows rounds fractional rates down (59.94 Hz reads as 59); give the rate people know.
fn nominal_hz(hz: u32) -> u32 {
    match hz {
        23 | 29 | 47 | 59 | 71 | 74 | 89 | 99 | 119 | 143 | 164 | 179 | 199 | 239 | 359 => hz + 1,
        _ => hz,
    }
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

/// Each connected monitor's device path (as in `Monitor::id`) and the name Windows shows for it,
/// read from the monitor (EDID) by Windows' display config.
fn friendly_names() -> Vec<(String, String)> {
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
        DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
    };
    let (mut np, mut nm) = (0, 0);
    if unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) }.is_err() {
        return Vec::new();
    }
    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
    if unsafe { QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None) }.is_err() {
        return Vec::new();
    }
    paths
        .iter()
        .take(np as usize)
        .filter_map(|p| {
            let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                    size: size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
                    adapterId: p.targetInfo.adapterId,
                    id: p.targetInfo.id,
                },
                ..Default::default()
            };
            (unsafe { DisplayConfigGetDeviceInfo(&mut target.header) } == 0)
                .then(|| (wide_to_string(&target.monitorDevicePath), wide_to_string(&target.monitorFriendlyDeviceName)))
                .filter(|(_, name)| !name.trim().is_empty())
        })
        .collect()
}

/// What `look_again` compares: each monitor's id, size, refresh rate and HDR white level.
type Shape = (String, u32, u32, u32, Option<u32>);

fn shapes(mons: &[Monitor]) -> Vec<Shape> {
    mons.iter().map(|m| (m.id.clone(), m.width, m.height, m.hz, m.hdr)).collect()
}

/// The monitors last seen by `look_again`, a change waiting for `changed`, and who to tell.
struct Watch {
    seen: Vec<Shape>,
    changed: Option<Vec<Monitor>>,
    tell: std::sync::Arc<dyn Fn() + Send + Sync>,
}

type Slot = std::sync::Mutex<Option<Watch>>;

static WATCH: Slot = std::sync::Mutex::new(None);

/// From now on, `look_again` keeps a change to the monitors since `now` for `changed` and calls
/// `tell` (on its own thread).
pub fn watch(now: &[Monitor], tell: impl Fn() + Send + Sync + 'static) {
    watch_in(&WATCH, now, tell);
}

/// Lists the monitors again (a few ms, so off the UI thread) and, if `watch` was called and they
/// changed since last seen, keeps them for `changed` and says so.
pub fn look_again() {
    look_again_in(&WATCH, list);
}

/// The monitors as they are now, if `look_again` found them changed since the last call.
pub fn changed() -> Option<Vec<Monitor>> {
    changed_in(&WATCH)
}

fn changed_in(slot: &Slot) -> Option<Vec<Monitor>> {
    slot.lock().unwrap().as_mut().and_then(|w| w.changed.take())
}

fn watch_in(slot: &Slot, now: &[Monitor], tell: impl Fn() + Send + Sync + 'static) {
    *slot.lock().unwrap() = Some(Watch { seen: shapes(now), changed: None, tell: std::sync::Arc::new(tell) });
}

/// `look_again` on `slot`, with `list` listing the monitors.
fn look_again_in(slot: &Slot, list: impl FnOnce() -> Vec<Monitor>) {
    if slot.lock().unwrap().is_none() {
        return;
    }
    let now = list();
    let tell = {
        let mut watch = slot.lock().unwrap();
        let Some(w) = watch.as_mut() else { return };
        let shape = shapes(&now);
        if shape == w.seen {
            return;
        }
        w.seen = shape;
        w.changed = Some(now);
        w.tell.clone()
    };
    tell();
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
        Monitor { id: id.into(), width, height, hz: 60, x, y, primary: x == 0 && y == 0, hdr: None, name: String::new() }
    }

    /// The main monitor says so first, a name from the monitor wins over the device path's model
    /// code, and each connected monitor this PC lists has a name or a code.
    #[test]
    fn monitor_labels() {
        let mut m = mon(r"\\?\DISPLAY#MSI3CA8#5&1&UID4353#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}", 0, 0, 2560, 1440);
        assert_eq!(m.label(), "Main monitor: MSI3CA8 (2560x1440, 60 Hz)");
        (m.name, m.primary) = ("MAG274QRF".into(), false);
        assert_eq!(m.label(), "MAG274QRF (2560x1440, 60 Hz)");
        for m in list() {
            assert!(!m.label().starts_with(" ("), "{}", m.label());
        }
    }

    #[test]
    fn fractional_rates_read_as_nominal() {
        assert_eq!([59, 60, 143, 144, 164, 239, 24].map(nominal_hz), [60, 60, 144, 144, 165, 240, 24]);
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

    /// Nothing is listed before `watch`; then a change from what it was given is kept once and
    /// told, the same monitors again are no change, and a moved or renamed one isn't either.
    #[test]
    fn display_changes_kept_once() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SLOT: Slot = std::sync::Mutex::new(None);
        let told = std::sync::Arc::new(AtomicU32::new(0));
        let main = mon("main", 0, 0, 2560, 1440);
        look_again_in(&SLOT, || panic!("listed before watch"));
        assert!(changed_in(&SLOT).is_none());
        let t = told.clone();
        watch_in(&SLOT, std::slice::from_ref(&main), move || {
            t.fetch_add(1, Ordering::Relaxed);
        });
        let told_now = || told.load(Ordering::Relaxed);
        look_again_in(&SLOT, || vec![Monitor { name: "MAG274QRF".into(), x: 10, ..main.clone() }]);
        assert!(changed_in(&SLOT).is_none() && told_now() == 0, "moved or named: no change");
        let hdr = Monitor { hdr: Some(240), ..main.clone() };
        look_again_in(&SLOT, || vec![hdr.clone()]);
        assert_eq!(changed_in(&SLOT).map(|m| shapes(&m)), Some(shapes(std::slice::from_ref(&hdr))));
        assert_eq!(told_now(), 1);
        assert!(changed_in(&SLOT).is_none(), "taken");
        look_again_in(&SLOT, || vec![hdr.clone()]);
        assert!(changed_in(&SLOT).is_none() && told_now() == 1, "the same monitors again");
        look_again_in(&SLOT, || vec![hdr.clone(), mon("side", 2560, 0, 1920, 1080)]);
        assert_eq!(changed_in(&SLOT).map(|m| m.len()), Some(2), "one plugged in");
    }

    /// Windows reports an SDR brightness for a connected monitor, HDR on or not (80 to 480 nits
    /// in its settings).
    #[test]
    fn sdr_white_reads() {
        let nits = sdr_white(r"\\.\DISPLAY1").or_else(|| (2..=8).find_map(|i| sdr_white(&format!(r"\\.\DISPLAY{i}"))));
        assert!(nits.is_some_and(|n| (80..=480).contains(&n)), "{nits:?}");
    }
}
