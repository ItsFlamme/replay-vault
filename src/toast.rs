//! Small "Clip saved" popup in the top-right corner. A separate Win32 window that never takes
//! focus and lets clicks pass through, so a game keeps the keyboard and mouse while it shows.

use std::path::Path;
use std::sync::mpsc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

/// The popup's size at 100% scaling; everything in it grows with the monitor's scaling.
const W: i32 = 320;
const H: i32 = 64;
const SHOW: u32 = WM_APP + 1;
const HIDE_TIMER: usize = 1;
/// A saved clip's thumbnail: 16:9, left of the text, at 100% scaling. Its picture is made at
/// twice that, so it stays sharp up to 200%.
const THUMB_W: usize = 84;
const THUMB_H: usize = 48;
const THUMB_X: i32 = 14;
const THUMB_PX: (usize, usize) = (THUMB_W * 2, THUMB_H * 2);
/// How long a save's popup waits for its thumbnail before showing without it.
const THUMB_WAIT: Duration = Duration::from_millis(400);

static THREAD: OnceLock<u32> = OnceLock::new();
static TEXT: Mutex<(Kind, String, String)> = Mutex::new((Kind::Saved, String::new(), String::new()));
/// The thumbnail (BGRA, top-down) and length ("0:30") of the clip the popup is for.
static THUMB: Mutex<Option<(Vec<u8>, String)>> = Mutex::new(None);
/// The bar's colour (the app's accent), as [r, g, b].
pub static BAR: Mutex<[u8; 3]> = Mutex::new([132, 72, 240]);
/// Drawn like a VCR's display (the look's `vcr_popup`): the VCR font, a red light for a save, the
/// title and length in capitals in the display's green (amber for a warning).
pub static VCR: AtomicBool = AtomicBool::new(false);
/// The scaling of the monitor the popup shows on, in DPI (96 is 100%).
static DPI: AtomicU32 = AtomicU32::new(96);

/// What a popup is about, so a problem doesn't look like a saved clip on the VCR's display.
#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    /// A clip, screenshot or recording saved (or a recording started): the red light.
    Saved,
    /// Something changed and nothing is wrong: no light.
    Info,
    /// Something failed or stopped: no light, the title in amber.
    Warning,
}

/// Starts the popup's thread. Call once.
pub fn start() {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || unsafe { run(tx) });
    if let Ok(id) = rx.recv() {
        let _ = THREAD.set(id);
    }
}

/// Shows the popup for a few seconds: `title` (e.g. "Clip saved") over `detail` (e.g. the file name).
pub fn show(kind: Kind, title: &str, detail: String) {
    post(kind, title, detail, None);
}

fn post(kind: Kind, title: &str, detail: String, thumb: Option<(Vec<u8>, String)>) {
    *THUMB.lock().unwrap() = thumb;
    *TEXT.lock().unwrap() = (kind, title.to_string(), detail);
    if let Some(&id) = THREAD.get() {
        unsafe {
            let _ = PostThreadMessageW(id, SHOW, WPARAM(0), LPARAM(0));
        }
    }
}

/// `show` for a saved clip, with its thumbnail and length if they're decoded within THUMB_WAIT;
/// blocks the caller that long at most.
pub fn show_clip(title: &str, detail: String, clip: &Path) {
    let (tx, rx) = mpsc::channel();
    let clip = clip.to_path_buf();
    std::thread::spawn(move || {
        if let Ok(t) = thumbnail(&clip) {
            let _ = tx.send(t);
        }
    });
    post(Kind::Saved, title, detail, rx.recv_timeout(THUMB_WAIT).ok());
}

/// A clip's thumbnail for the popup and its length ("0:30"). The keyframe near the end: the
/// moment just saved, and much quicker than an exact frame.
fn thumbnail(clip: &Path) -> anyhow::Result<(Vec<u8>, String)> {
    let len = crate::media::duration(clip)?;
    let f = crate::media::keyframe(clip, (len - 1.0).max(0.0), THUMB_PX.0)?;
    Ok((fit(&f, THUMB_PX.0, THUMB_PX.1), crate::mmss(len.round() as u32)))
}

/// `f` scaled to fill `w` x `h` (cropping the overflow evenly), as BGRA for GDI.
fn fit(f: &crate::media::Frame, w: usize, h: usize) -> Vec<u8> {
    let scale = (w as f64 / f.width.max(1) as f64).max(h as f64 / f.height.max(1) as f64);
    let (ox, oy) = ((f.width as f64 * scale - w as f64) / 2.0, (f.height as f64 * scale - h as f64) / 2.0);
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let sy = (((y as f64 + 0.5 + oy) / scale) as usize).min(f.height.saturating_sub(1));
        for x in 0..w {
            let sx = (((x as f64 + 0.5 + ox) / scale) as usize).min(f.width.saturating_sub(1));
            let px = f.rgba.get((sy * f.width + sx) * 4..).unwrap_or(&[0, 0, 0, 0]);
            out.extend_from_slice(&[px[2], px[1], px[0], 255]);
        }
    }
    out
}

/// A short, soft two-note blip (made here, about a fifth of full volume), so there's feedback in a
/// game where the popup can't be seen. Plays on Windows' sound thread and never blocks.
pub fn chime() {
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
    static WAV: OnceLock<Vec<u8>> = OnceLock::new();
    let wav = WAV.get_or_init(|| {
        const RATE: u32 = 44_100;
        let mut samples = Vec::new();
        for (freq, ms) in [(880.0f32, 70u32), (1320.0, 90)] {
            let n = RATE * ms / 1000;
            for i in 0..n {
                let t = i as f32 / RATE as f32;
                // Fade in and out so there's no click.
                let env = (i.min(n - i) as f32 / (RATE as f32 * 0.01)).min(1.0);
                samples.push((f32::sin(t * freq * std::f32::consts::TAU) * env * 0.2 * i16::MAX as f32) as i16);
            }
        }
        let data_len = samples.len() as u32 * 2;
        let mut w = Vec::with_capacity(44 + data_len as usize);
        w.extend(b"RIFF");
        w.extend((36 + data_len).to_le_bytes());
        w.extend(b"WAVEfmt ");
        w.extend(16u32.to_le_bytes());
        w.extend(1u16.to_le_bytes()); // PCM
        w.extend(1u16.to_le_bytes()); // mono
        w.extend(RATE.to_le_bytes());
        w.extend((RATE * 2).to_le_bytes());
        w.extend(2u16.to_le_bytes());
        w.extend(16u16.to_le_bytes());
        w.extend(b"data");
        w.extend(data_len.to_le_bytes());
        samples.iter().for_each(|s| w.extend(s.to_le_bytes()));
        w
    });
    unsafe {
        let _ = PlaySoundW(windows::core::PCWSTR(wav.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}

unsafe fn run(tx: mpsc::Sender<u32>) {
    unsafe {
        let class = WNDCLASSW { lpfnWndProc: Some(wndproc), lpszClassName: w!("InstantReplayToast"), ..Default::default() };
        RegisterClassW(&class);
        let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT;
        let hwnd = CreateWindowExW(ex, w!("InstantReplayToast"), w!(""), WS_POPUP, 0, 0, W, H, None, None, None, None);
        let Ok(hwnd) = hwnd else {
            let _ = tx.send(GetCurrentThreadId());
            return;
        };
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 235, LWA_ALPHA);
        let _ = SetWindowRgn(hwnd, Some(CreateRoundRectRgn(0, 0, W + 1, H + 1, 14, 14)), false);
        let _ = tx.send(GetCurrentThreadId());

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            if msg.hwnd.is_invalid() && msg.message == SHOW {
                // Corner of the monitor the focused window (usually the game) is on.
                let monitor = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY);
                let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
                let _ = GetMonitorInfoW(monitor, &mut info);
                let r = info.rcWork;
                // The app is per-monitor DPI aware, so these are real pixels: scaled to that monitor.
                DPI.store(monitor_dpi(monitor), Ordering::Relaxed);
                let (w, h, gap) = (px(W), px(H), px(24));
                let _ = SetWindowRgn(hwnd, Some(CreateRoundRectRgn(0, 0, w + 1, h + 1, px(14), px(14))), false);
                let flags = SWP_NOACTIVATE | SWP_SHOWWINDOW;
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), r.right - w - gap, r.top + gap, w, h, flags);
                let _ = InvalidateRect(Some(hwnd), None, true);
                SetTimer(Some(hwnd), HIDE_TIMER, 2500, None);
            } else {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

/// `v` (in pixels at 100% scaling) at the popup's monitor's scaling.
fn px(v: i32) -> i32 {
    (v as i64 * DPI.load(Ordering::Relaxed) as i64 / 96) as i32
}

/// A monitor's scaling in DPI (96 is 100%). GetDpiForMonitor is looked up in shcore.dll (Windows
/// 8.1 and later) when first needed; 96 if it can't be.
pub(crate) fn monitor_dpi(monitor: HMONITOR) -> u32 {
    type GetDpiForMonitor = unsafe extern "system" fn(HMONITOR, i32, *mut u32, *mut u32) -> windows::core::HRESULT;
    static FUNC: OnceLock<Option<GetDpiForMonitor>> = OnceLock::new();
    let func = FUNC.get_or_init(|| unsafe {
        use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
        let shcore = LoadLibraryW(w!("shcore.dll")).ok()?;
        let f = GetProcAddress(shcore, windows::core::s!("GetDpiForMonitor"))?;
        Some(std::mem::transmute::<unsafe extern "system" fn() -> isize, GetDpiForMonitor>(f))
    });
    let (mut x, mut y) = (96, 96);
    // 0: MDT_EFFECTIVE_DPI, the scaling set in Windows' display settings.
    match func {
        Some(f) if unsafe { f(monitor, 0, &mut x, &mut y) }.is_ok() && x > 0 => x,
        _ => 96,
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                paint(hdc);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_TIMER => {
                let _ = KillTimer(Some(hwnd), HIDE_TIMER);
                let _ = ShowWindow(hwnd, SW_HIDE);
                LRESULT(0)
            }
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// The VCR font (bundled, as the app uses), added for this process only the first time it's needed.
fn vcr_font() {
    static ADDED: OnceLock<()> = OnceLock::new();
    ADDED.get_or_init(|| unsafe {
        let font = crate::ui::theme::VT323;
        // Windows writes how many fonts it added here.
        let mut count = 0u32;
        AddFontMemResourceEx(font.as_ptr().cast(), font.len() as u32, None, std::ptr::addr_of_mut!(count));
    });
}

/// Draws the popup: background, accent bar, the thumbnail if any, and the text.
unsafe fn paint(hdc: HDC) {
    unsafe {
        let (w, h) = (px(W), px(H));
        let vcr = VCR.load(Ordering::Relaxed);
        fill(hdc, RECT { left: 0, top: 0, right: w, bottom: h }, if vcr { rgb(10, 10, 13) } else { rgb(24, 24, 28) });
        let [r, g, b] = *BAR.lock().unwrap();
        fill(hdc, RECT { left: 0, top: 0, right: px(6), bottom: h }, rgb(r, g, b));
        SetBkMode(hdc, TRANSPARENT);
        let (kind, title, detail) = TEXT.lock().unwrap().clone();
        let mut left = px(20);
        if let Some((pic, len)) = THUMB.lock().unwrap().as_ref() {
            let (x, tw, th) = (px(THUMB_X), px(THUMB_W as i32), px(THUMB_H as i32));
            let top = (h - th) / 2;
            let (pw, ph) = (THUMB_PX.0 as i32, THUMB_PX.1 as i32);
            let header = BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: pw,
                biHeight: -ph,
                biPlanes: 1,
                biBitCount: 32,
                ..Default::default()
            };
            let bmi = BITMAPINFO { bmiHeader: header, ..Default::default() };
            SetStretchBltMode(hdc, HALFTONE);
            StretchDIBits(hdc, x, top, tw, th, 0, 0, pw, ph, Some(pic.as_ptr().cast()), &bmi, DIB_RGB_COLORS, SRCCOPY);
            // The length in a dark tag in the thumbnail's corner, like a video player's (on the
            // VCR's display it's in the title line instead).
            if !vcr {
                let tag = RECT { left: x + tw - px(34), top: top + th - px(17), right: x + tw - px(2), bottom: top + th - px(2) };
                fill(hdc, tag, rgb(16, 16, 20));
                text(hdc, len, px(13), FW_SEMIBOLD.0 as i32, rgb(240, 240, 240), RECT { left: tag.left + px(3), ..tag }, w!("Segoe UI"));
            }
            left = x + tw + px(12);
        }
        if vcr {
            // "● CLIP SAVED  0:15" over "VALORANT 2026-11-09 ...", like the VCR's display. Only a
            // save gets the red light; a warning's title is amber.
            vcr_font();
            let len = THUMB.lock().unwrap().as_ref().map(|(_, len)| format!("  {len}")).unwrap_or_default();
            let (light, color) = vcr_title(kind);
            let mut title_left = left;
            if light {
                let dot = RECT { left, top: px(15), right: left + px(9), bottom: px(24) };
                let brush = CreateSolidBrush(rgb(239, 68, 68));
                let old = SelectObject(hdc, brush.into());
                let pen = SelectObject(hdc, GetStockObject(NULL_PEN));
                let _ = Ellipse(hdc, dot.left, dot.top, dot.right, dot.bottom);
                SelectObject(hdc, pen);
                SelectObject(hdc, old);
                let _ = DeleteObject(brush.into());
                title_left += px(15);
            }
            let title = format!("{}{len}", title.to_uppercase());
            text(hdc, &title, px(26), FW_NORMAL.0 as i32, color, RECT { left: title_left, top: px(6), right: w - px(12), bottom: px(34) }, w!("VT323"));
            text(hdc, &detail.to_uppercase(), px(20), FW_NORMAL.0 as i32, rgb(200, 196, 212), RECT { left, top: px(34), right: w - px(12), bottom: px(58) }, w!("VT323"));
            return;
        }
        text(hdc, &title, px(20), FW_SEMIBOLD.0 as i32, if kind == Kind::Warning { amber() } else { rgb(240, 240, 240) }, RECT { left, top: px(9), right: w - px(12), bottom: px(34) }, w!("Segoe UI"));
        text(hdc, &detail, px(15), FW_NORMAL.0 as i32, rgb(160, 160, 165), RECT { left, top: px(35), right: w - px(12), bottom: px(56) }, w!("Segoe UI"));
    }
}

/// Whether the VCR's display shows the red light for a popup, and its title's color: the light
/// only for a save, the title in amber for a warning and in the display's green otherwise.
fn vcr_title(kind: Kind) -> (bool, COLORREF) {
    (kind == Kind::Saved, if kind == Kind::Warning { amber() } else { rgb(95, 227, 208) })
}

/// Retro's amber (the dark palette's), for a warning's title.
fn amber() -> COLORREF {
    rgb(230, 170, 40)
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

unsafe fn fill(hdc: HDC, rect: RECT, color: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(color);
        FillRect(hdc, &rect, brush);
        let _ = DeleteObject(brush.into());
    }
}

unsafe fn text(hdc: HDC, s: &str, height: i32, weight: i32, color: COLORREF, mut rect: RECT, face: windows::core::PCWSTR) {
    // DrawTextW crashes on an empty string (the window paints once at creation, before any save).
    if s.is_empty() {
        return;
    }
    unsafe {
        let font = CreateFontW(
            -height, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY, 0, face,
        );
        let old = SelectObject(hdc, font.into());
        SetTextColor(hdc, color);
        let mut wide: Vec<u16> = s.encode_utf16().collect();
        DrawTextW(hdc, &mut wide, &mut rect, DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX);
        SelectObject(hdc, old);
        let _ = DeleteObject(font.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4x2 frame, left half red and right half blue, filling a 2x2 thumbnail: the sides are
    /// cropped evenly, so one red and one blue column, in BGRA.
    #[test]
    fn fit_crops_to_fill_and_swaps_to_bgra() {
        let (red, blue) = ([255, 0, 0, 255], [0, 0, 255, 255]);
        let row = [red, red, blue, blue].concat();
        let f = crate::media::Frame { width: 4, height: 2, rgba: [row.clone(), row].concat() };
        let out = fit(&f, 2, 2);
        assert_eq!(out.len(), 2 * 2 * 4);
        assert_eq!(&out[0..4], &[0, 0, 255, 255]);
        assert_eq!(&out[4..8], &[255, 0, 0, 255]);
        assert_eq!(&out[8..16], &out[0..8]);
    }

    /// On the VCR's display only a save gets the red light, and a failure isn't in the save's green.
    #[test]
    fn vcr_title_marks_only_a_save_and_warns_in_amber() {
        assert_eq!(vcr_title(Kind::Saved), (true, rgb(95, 227, 208)));
        assert_eq!(vcr_title(Kind::Info), (false, rgb(95, 227, 208)));
        assert_eq!(vcr_title(Kind::Warning), (false, amber()));
    }

    /// Paints the popup for a real clip (from SCREENS_FOLDER, else the clips folder) into an
    /// off-screen bitmap (no window) and saves it as target\screens\toast.png to look at, and as
    /// the VCR's display as toast-vcr.png; then a warning on the VCR's display as
    /// toast-vcr-warning.png. cargo test toast_screen -- --ignored
    #[test]
    #[ignore]
    fn toast_screen() {
        let folder = std::env::var("SCREENS_FOLDER").unwrap_or_else(|_| crate::config::Config::load().folder);
        let clip = crate::clips::list(&folder, 5).into_iter().find(|c| !c.is_picture).expect("a clip");
        *THUMB.lock().unwrap() = Some(thumbnail(&clip.path).unwrap());
        *TEXT.lock().unwrap() = (Kind::Saved, "Clip saved".into(), clip.name.clone());
        for (vcr, name) in [(false, "toast.png"), (true, "toast-vcr.png")] {
            VCR.store(vcr, Ordering::Relaxed);
            save_toast(name);
        }
        *THUMB.lock().unwrap() = None;
        *TEXT.lock().unwrap() = (Kind::Warning, "Clip not saved".into(), "Open Replay Vault to see why".into());
        save_toast("toast-vcr-warning.png");
    }

    fn save_toast(name: &str) {
        let mut rgba = vec![0u8; (W * H * 4) as usize];
        unsafe {
            let dc = CreateCompatibleDC(None);
            let header = BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: W, biHeight: -H, biPlanes: 1, biBitCount: 32, ..Default::default() };
            let bmi = BITMAPINFO { bmiHeader: header, ..Default::default() };
            let mut bits = std::ptr::null_mut();
            let bmp = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
            let old = SelectObject(dc, bmp.into());
            paint(dc);
            let _ = GdiFlush();
            std::ptr::copy_nonoverlapping(bits as *const u8, rgba.as_mut_ptr(), rgba.len());
            SelectObject(dc, old);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(dc);
        }
        rgba.chunks_mut(4).for_each(|p| {
            p.swap(0, 2);
            p[3] = 255;
        });
        let out = std::env::var("SCREENS_OUT").map(std::path::PathBuf::from).unwrap_or(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(r"target\screens")).join(name);
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        image::save_buffer(&out, &rgba, W as u32, H as u32, image::ColorType::Rgba8).unwrap();
    }

    /// The real thing on a recent clip (only read): the right size, and not blank.
    #[test]
    fn thumbnail_of_a_real_clip() {
        let folder = crate::config::Config::load().folder;
        let Some(clip) = crate::clips::list(&folder, 5).into_iter().find(|c| !c.is_picture) else { return };
        let start = std::time::Instant::now();
        let (px, _) = thumbnail(&clip.path).unwrap();
        let took = start.elapsed();
        assert_eq!(px.len(), THUMB_PX.0 * THUMB_PX.1 * 4);
        assert!(px.chunks(4).any(|p| p[..3] != [0, 0, 0]), "thumbnail is all black");
        println!("thumbnail in {took:?}");
    }
}
