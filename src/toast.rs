//! Small "Clip saved" popup in the top-right corner. A separate Win32 window that never takes
//! focus and lets clicks pass through, so a game keeps the keyboard and mouse while it shows.

use std::path::Path;
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

const W: i32 = 320;
const H: i32 = 64;
const SHOW: u32 = WM_APP + 1;
const HIDE_TIMER: usize = 1;
/// A saved clip's thumbnail: 16:9, left of the text.
const THUMB_W: usize = 84;
const THUMB_H: usize = 48;
const THUMB_X: i32 = 14;
/// How long a save's popup waits for its thumbnail before showing without it.
const THUMB_WAIT: Duration = Duration::from_millis(400);

static THREAD: OnceLock<u32> = OnceLock::new();
static TEXT: Mutex<(String, String)> = Mutex::new((String::new(), String::new()));
/// The thumbnail (BGRA, top-down) and length ("0:30") of the clip the popup is for.
static THUMB: Mutex<Option<(Vec<u8>, String)>> = Mutex::new(None);
/// The bar's colour (the app's accent), as [r, g, b].
pub static BAR: Mutex<[u8; 3]> = Mutex::new([132, 72, 240]);

/// Starts the popup's thread. Call once.
pub fn start() {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || unsafe { run(tx) });
    if let Ok(id) = rx.recv() {
        let _ = THREAD.set(id);
    }
}

/// Shows the popup for a few seconds: `title` (e.g. "Clip saved") over `detail` (e.g. the file name).
pub fn show(title: &str, detail: String) {
    post(title, detail, None);
}

fn post(title: &str, detail: String, thumb: Option<(Vec<u8>, String)>) {
    *THUMB.lock().unwrap() = thumb;
    *TEXT.lock().unwrap() = (title.to_string(), detail);
    if let Some(&id) = THREAD.get() {
        unsafe {
            let _ = PostThreadMessageW(id, SHOW, WPARAM(0), LPARAM(0));
        }
    }
}

/// `show` with the clip's thumbnail and length, if they're decoded within THUMB_WAIT; blocks
/// the caller that long at most.
pub fn show_clip(title: &str, detail: String, clip: &Path) {
    let (tx, rx) = mpsc::channel();
    let clip = clip.to_path_buf();
    std::thread::spawn(move || {
        if let Ok(t) = thumbnail(&clip) {
            let _ = tx.send(t);
        }
    });
    post(title, detail, rx.recv_timeout(THUMB_WAIT).ok());
}

/// A clip's thumbnail for the popup and its length ("0:30"). The keyframe near the end: the
/// moment just saved, and much quicker than an exact frame.
fn thumbnail(clip: &Path) -> anyhow::Result<(Vec<u8>, String)> {
    let len = crate::media::duration(clip)?;
    let f = crate::media::keyframe(clip, (len - 1.0).max(0.0), THUMB_W * 2)?;
    Ok((fit(&f, THUMB_W, THUMB_H), crate::mmss(len.round() as u32)))
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
                let flags = SWP_NOACTIVATE | SWP_SHOWWINDOW;
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), r.right - W - 24, r.top + 24, W, H, flags);
                let _ = InvalidateRect(Some(hwnd), None, true);
                SetTimer(Some(hwnd), HIDE_TIMER, 2500, None);
            } else {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
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

/// Draws the popup: background, accent bar, the thumbnail if any, and the text.
unsafe fn paint(hdc: HDC) {
    unsafe {
        fill(hdc, RECT { left: 0, top: 0, right: W, bottom: H }, rgb(24, 24, 28));
        let [r, g, b] = *BAR.lock().unwrap();
        fill(hdc, RECT { left: 0, top: 0, right: 6, bottom: H }, rgb(r, g, b));
        SetBkMode(hdc, TRANSPARENT);
        let (title, detail) = TEXT.lock().unwrap().clone();
        let mut left = 20;
        if let Some((px, len)) = THUMB.lock().unwrap().as_ref() {
            let (tw, th, top) = (THUMB_W as i32, THUMB_H as i32, (H - THUMB_H as i32) / 2);
            let header = BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: tw,
                biHeight: -th,
                biPlanes: 1,
                biBitCount: 32,
                ..Default::default()
            };
            let bmi = BITMAPINFO { bmiHeader: header, ..Default::default() };
            SetDIBitsToDevice(hdc, THUMB_X, top, tw as u32, th as u32, 0, 0, 0, th as u32, px.as_ptr().cast(), &bmi, DIB_RGB_COLORS);
            // The length in a dark tag in the thumbnail's corner, like a video player's.
            let tag = RECT { left: THUMB_X + tw - 34, top: top + th - 17, right: THUMB_X + tw - 2, bottom: top + th - 2 };
            fill(hdc, tag, rgb(16, 16, 20));
            text(hdc, len, 13, FW_SEMIBOLD.0 as i32, rgb(240, 240, 240), RECT { left: tag.left + 3, ..tag });
            left = THUMB_X + tw + 12;
        }
        text(hdc, &title, 20, FW_SEMIBOLD.0 as i32, rgb(240, 240, 240), RECT { left, top: 9, right: W - 12, bottom: 34 });
        text(hdc, &detail, 15, FW_NORMAL.0 as i32, rgb(160, 160, 165), RECT { left, top: 35, right: W - 12, bottom: 56 });
    }
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

unsafe fn text(hdc: HDC, s: &str, height: i32, weight: i32, color: COLORREF, mut rect: RECT) {
    // DrawTextW crashes on an empty string (the window paints once at creation, before any save).
    if s.is_empty() {
        return;
    }
    unsafe {
        let font = CreateFontW(
            -height, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY, 0, w!("Segoe UI"),
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

    /// Paints the popup for a real clip into an off-screen bitmap (no window) and saves it as
    /// target\screens\toast.png to look at. cargo test toast_screen -- --ignored
    #[test]
    #[ignore]
    fn toast_screen() {
        let folder = crate::config::Config::load().folder;
        let clip = crate::clips::list(&folder, 5).into_iter().find(|c| !c.is_picture).expect("a clip");
        *THUMB.lock().unwrap() = Some(thumbnail(&clip.path).unwrap());
        *TEXT.lock().unwrap() = ("Clip saved".into(), clip.name.clone());
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
        let out = std::env::var("SCREENS_OUT").map(std::path::PathBuf::from).unwrap_or(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(r"target\screens")).join("toast.png");
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
        assert_eq!(px.len(), THUMB_W * THUMB_H * 4);
        assert!(px.chunks(4).any(|p| p[..3] != [0, 0, 0]), "thumbnail is all black");
        println!("thumbnail in {took:?}");
    }
}
