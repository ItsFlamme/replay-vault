//! Clip files through Windows' own Media Foundation: length, a still frame for thumbnails, and
//! trimming. Trimming copies the video and audio as they are (no re-encode), so it's fast and
//! lossless; cuts start on the keyframe at or before the chosen start. trim_mixed instead
//! decodes the audio tracks and mixes them into one, with a volume per track, and export also
//! re-encodes the video, smaller or cropped, for sharing.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::{GUID, HSTRING};

/// Media Foundation times are in 100 ns units.
const SEC: f64 = 10_000_000.0;
const ANY_STREAM: u32 = MF_SOURCE_READER_ANY_STREAM.0 as u32;
const FIRST_VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
const ALL_STREAMS: u32 = MF_SOURCE_READER_ALL_STREAMS.0 as u32;
const END_OF_STREAM: u32 = MF_SOURCE_READERF_ENDOFSTREAM.0 as u32;
/// Mixing happens in 48 kHz stereo float, what OBS records at.
pub(crate) const RATE: u32 = 48_000;
/// Timestamps closer than this (1 ms) to where a track already is are treated as contiguous, so
/// rounding jitter between AAC frames doesn't add or drop single samples.
const SLACK: i64 = 48;

/// A decoded frame, RGBA.
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// COM and Media Foundation for this thread (safe to call again).
fn init() -> anyhow::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
    }
    Ok(())
}

fn reader(path: &Path, decode_video: bool) -> anyhow::Result<IMFSourceReader> {
    init()?;
    unsafe {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.ok_or_else(|| anyhow::anyhow!("no attributes"))?;
        if decode_video {
            // Lets the reader convert decoded frames to RGB for us.
            attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
        }
        Ok(MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), &attrs)?)
    }
}

fn seek(r: &IMFSourceReader, secs: f64) -> anyhow::Result<()> {
    let pos = PROPVARIANT::from((secs.max(0.0) * SEC) as i64);
    unsafe { r.SetCurrentPosition(&GUID::zeroed(), &pos)? };
    Ok(())
}

fn length(r: &IMFSourceReader) -> anyhow::Result<f64> {
    let v = unsafe { r.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)? };
    Ok(u64::try_from(&v)? as f64 / SEC)
}

/// Clip length in seconds.
pub fn duration(path: &Path) -> anyhow::Result<f64> {
    length(&reader(path, false)?)
}

/// The frame at `at` seconds, scaled down to at most `max_w` pixels wide.
pub fn frame(path: &Path, at: f64, max_w: usize) -> anyhow::Result<Frame> {
    decode_frame(path, at, max_w, true)
}

/// The keyframe at or before `at`: up to a couple of seconds early (keyint_sec 2), but several
/// times quicker than `frame`, for rough previews.
pub fn keyframe(path: &Path, at: f64, max_w: usize) -> anyhow::Result<Frame> {
    decode_frame(path, at, max_w, false)
}

/// `frame`, or with `exact` false the first frame decoded after seeking (the keyframe).
fn decode_frame(path: &Path, at: f64, max_w: usize, exact: bool) -> anyhow::Result<Frame> {
    let r = reader(path, true)?;
    unsafe {
        r.SetStreamSelection(ALL_STREAMS, false)?;
        r.SetStreamSelection(FIRST_VIDEO, true)?;
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        r.SetCurrentMediaType(FIRST_VIDEO, None, &t)?;
        let current = r.GetCurrentMediaType(FIRST_VIDEO)?;
        let size = current.GetUINT64(&MF_MT_FRAME_SIZE)?;
        let (w, h) = ((size >> 32) as usize, (size & 0xffff_ffff) as usize);
        let stride = current.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or((w * 4) as i32);
        seek(&r, at)?;
        // Decoding starts at the keyframe before `at`; read on until we reach it.
        let target = (at * SEC) as i64;
        let mut last = None;
        for _ in 0..600 {
            let (mut flags, mut ts, mut sample) = (0u32, 0i64, None);
            r.ReadSample(FIRST_VIDEO, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))?;
            if sample.is_some() {
                last = sample;
                if ts >= target || !exact {
                    break;
                }
            }
            if flags & END_OF_STREAM != 0 {
                break;
            }
        }
        let sample = last.ok_or_else(|| anyhow::anyhow!("no video frame"))?;
        let buf = sample.ConvertToContiguousBuffer()?;
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        buf.Lock(&mut ptr, None, Some(&mut len))?;
        let data = std::slice::from_raw_parts(ptr, len as usize);
        let row = stride.unsigned_abs() as usize;
        let scale = w.div_ceil(max_w.max(1)).max(1);
        let (ow, oh) = (w / scale, h / scale);
        let mut rgba = Vec::with_capacity(ow * oh * 4);
        for y in 0..oh {
            // A negative stride means the image is stored bottom-up.
            let sy = if stride < 0 { h - 1 - y * scale } else { y * scale };
            for x in 0..ow {
                let i = sy * row + x * scale * 4;
                let px = data.get(i..i + 4).unwrap_or(&[0, 0, 0, 0]);
                rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
            }
        }
        let _ = buf.Unlock();
        Ok(Frame { width: ow, height: oh, rgba })
    }
}

/// Copies `start`..`end` seconds of `src` into a new file `dst`, all streams, without re-encoding.
pub fn trim(src: &Path, dst: &Path, start: f64, end: f64) -> anyhow::Result<()> {
    let r = reader(src, false)?;
    let end = end.min(length(&r)?);
    anyhow::ensure!(end > start, "the end has to come after the start");
    unsafe {
        let w = sink_writer(dst, false)?;

        // Every stream (video plus each audio track) goes across as it is.
        let mut types = Vec::new(); // by reader stream
        let mut video = None;
        while let Ok(t) = r.GetNativeMediaType(types.len() as u32, 0) {
            let i = types.len() as u32;
            r.SetStreamSelection(i, true)?;
            r.SetCurrentMediaType(i, None, &t)?;
            if t.GetGUID(&MF_MT_MAJOR_TYPE)? == MFMediaType_Video && video.is_none() {
                video = Some(i as usize);
            }
            types.push(t);
        }
        let video = video.ok_or_else(|| anyhow::anyhow!("the clip has no video"))?;
        // The writer stores tracks in the order they're added: video, then audio in track order.
        let audio = audio_streams(&r);
        let order: Vec<usize> = (0..types.len())
            .filter(|i| !audio.contains(&(*i as u32)))
            .chain(audio.iter().map(|&i| i as usize))
            .collect();
        let mut out = vec![0u32; types.len()]; // writer stream, by reader stream
        for i in order {
            out[i] = w.AddStream(&types[i])?;
            w.SetInputMediaType(out[i], &types[i], None)?;
        }
        w.BeginWriting()?;
        // After a seek, video starts at the keyframe before `start` but audio only at `start`, so
        // the cut would open with up to 2 s of silence. Seeking again just past that keyframe
        // gets sound for the whole cut.
        seek(&r, start)?;
        let keyframe = loop {
            let (mut idx, mut flags, mut ts, mut sample) = (0u32, 0u32, 0i64, None);
            r.ReadSample(ANY_STREAM, 0, Some(&mut idx), Some(&mut flags), Some(&mut ts), Some(&mut sample))?;
            if idx as usize == video {
                if sample.is_some() {
                    break ts;
                }
                anyhow::ensure!(flags & END_OF_STREAM == 0, "nothing in that range");
            }
        };
        seek(&r, (keyframe + 10_000) as f64 / SEC)?;

        let end_t = (end * SEC) as i64;
        let mut done = vec![false; out.len()];
        // The clip starts at the first video frame (a keyframe, `first`); audio waits for it.
        // Times are shifted by that frame's decode time (`base`), see timeline_start.
        let mut first: Option<i64> = None;
        let mut base = 0;
        let mut held: Vec<(usize, i64, IMFSample)> = Vec::new();
        let mut wrote = 0u32;
        while done.iter().any(|d| !d) {
            let (mut idx, mut flags, mut ts, mut sample) = (0u32, 0u32, 0i64, None);
            r.ReadSample(ANY_STREAM, 0, Some(&mut idx), Some(&mut flags), Some(&mut ts), Some(&mut sample))?;
            let s = idx as usize;
            if s >= out.len() {
                continue;
            }
            if let Some(sample) = sample {
                if ts > end_t {
                    done[s] = true;
                    r.SetStreamSelection(idx, false)?;
                } else if s == video && first.is_none() {
                    first = Some(ts);
                    base = timeline_start(&sample, ts);
                    for (hs, hts, hsample) in held.drain(..) {
                        if hts >= ts {
                            rebase(&hsample, hts, base)?;
                            w.WriteSample(out[hs], &hsample)?;
                        }
                    }
                    rebase(&sample, ts, base)?;
                    w.WriteSample(out[s], &sample)?;
                    wrote += 1;
                } else if let Some(f) = first {
                    // Frames from before the keyframe (B-frames of the previous group) can't be shown.
                    if ts >= f {
                        rebase(&sample, ts, base)?;
                        w.WriteSample(out[s], &sample)?;
                        wrote += 1;
                    }
                } else {
                    held.push((s, ts, sample));
                }
            }
            if flags & END_OF_STREAM != 0 && !done[s] {
                done[s] = true;
            }
        }
        anyhow::ensure!(wrote > 0, "nothing in that range");
        w.Finalize()?;
    }
    Ok(())
}

/// Moves a sample's presentation time, and its decode time if it has one, back by `base`.
/// (The MP4 writer uses the decode time for the file's length.)
unsafe fn rebase(s: &IMFSample, ts: i64, base: i64) -> anyhow::Result<()> {
    unsafe {
        s.SetSampleTime(ts - base)?;
        if let Ok(dts) = s.GetUINT64(&MFSampleExtension_DecodeTimestamp) {
            s.SetUINT64(&MFSampleExtension_DecodeTimestamp, (dts as i64 - base) as u64)?;
        }
    }
    Ok(())
}

/// Where a cut's timeline starts, given its first frame (a keyframe at `ts`): that frame's decode
/// time. With B-frames it's a frame or two before the presentation time; shifting by the
/// presentation time instead would give the first frame a negative decode time, and the MP4
/// writer then leaves it out of the keyframe list, so players skip ahead to the next keyframe
/// (2 s in) when playing from the start.
unsafe fn timeline_start(s: &IMFSample, ts: i64) -> i64 {
    unsafe { s.GetUINT64(&MFSampleExtension_DecodeTimestamp) }.map_or(ts, |d| (d as i64).min(ts))
}

/// Frames per second of the clip's video; None if it can't be read.
pub fn frame_rate(path: &Path) -> Option<f64> {
    let r = reader(path, false).ok()?;
    let packed = unsafe { r.GetNativeMediaType(FIRST_VIDEO, 0).and_then(|t| t.GetUINT64(&MF_MT_FRAME_RATE)) }.ok()?;
    // Numerator in the high 32 bits, denominator in the low.
    let (num, den) = ((packed >> 32) as f64, (packed & 0xffff_ffff) as f64);
    (num > 0.0 && den > 0.0).then(|| num / den)
}

/// Number of audio tracks in the clip.
pub fn audio_tracks(path: &Path) -> anyhow::Result<usize> {
    Ok(audio_streams(&reader(path, false)?).len())
}

/// Reader stream numbers of the audio tracks, in track order (Track 1 first). Windows' MP4 reader
/// lists a file's tracks last to first (an OBS clip stored as video, A1, A2, A3 reads back as
/// A3, A2, A1, video), which put the full mix under the Track 3 slider. Its own stream IDs follow
/// that same order, so they can't fix it; the list is just turned around.
fn audio_streams(r: &IMFSourceReader) -> Vec<u32> {
    let mut audio: Vec<u32> = (0..)
        .map_while(|i| unsafe { r.GetNativeMediaType(i, 0) }.ok().map(|t| (i, t)))
        .filter(|(_, t)| unsafe { t.GetGUID(&MF_MT_MAJOR_TYPE) }.is_ok_and(|g| g == MFMediaType_Audio))
        .map(|(i, _)| i)
        .collect();
    audio.reverse();
    audio
}

/// Like trim, but the audio tracks are decoded, each scaled by `gains[i]` (by audio track order),
/// and summed into one stereo AAC track at 192 kbps. Video is still copied without re-encoding.
pub fn trim_mixed(src: &Path, dst: &Path, start: f64, end: f64, gains: &[f32]) -> anyhow::Result<()> {
    remix(src, dst, start, end, gains, None, &|_| {})
}

/// trim_mixed, or with `enc` the video re-encoded to H.264 as it says. `progress` hears how far
/// along it is (0 to 1) as video frames are encoded.
fn remix(src: &Path, dst: &Path, start: f64, end: f64, gains: &[f32], enc: Option<&Encode>, progress: &dyn Fn(f32)) -> anyhow::Result<()> {
    enum Out {
        Copy(u32),
        Mix(usize),
        Encode(u32),
    }
    let r = reader(src, false)?;
    let end = end.min(length(&r)?);
    anyhow::ensure!(end > start, "the end has to come after the start");
    unsafe {
        let w = sink_writer(dst, enc.is_some_and(|e| e.hw))?;

        // Video goes across as it is; each audio track is decoded for the mix.
        let audio = audio_streams(&r); // in track order, which is what gains[] follows
        let mut outs = Vec::new(); // by reader stream
        let mut video = None;
        let tracks = audio.len();
        let mut i = 0;
        while let Ok(t) = r.GetNativeMediaType(i, 0) {
            r.SetStreamSelection(i, true)?;
            let major = t.GetGUID(&MF_MT_MAJOR_TYPE)?;
            if let Some(track) = audio.iter().position(|&a| a == i) {
                decode_to_float(&r, i)?;
                outs.push(Out::Mix(track));
            } else if let Some(e) = enc.filter(|_| major == MFMediaType_Video && video.is_none()) {
                decode_to_nv12(&r, i)?;
                let o = w.AddStream(&video_type(e, true)?)?;
                w.SetInputMediaType(o, &video_type(e, false)?, None)?;
                video = Some(i as usize);
                outs.push(Out::Encode(o));
            } else {
                r.SetCurrentMediaType(i, None, &t)?;
                let o = w.AddStream(&t)?;
                w.SetInputMediaType(o, &t, None)?;
                if major == MFMediaType_Video && video.is_none() {
                    video = Some(i as usize);
                }
                outs.push(Out::Copy(o));
            }
            i += 1;
        }
        let video = video.ok_or_else(|| anyhow::anyhow!("the clip has no video"))?;
        anyhow::ensure!(tracks > 0, "the clip has no sound to mix");
        anyhow::ensure!(gains.len() == tracks, "got {} track volumes for {tracks} audio tracks", gains.len());
        let mixed = w.AddStream(&audio_type(true)?)?;
        w.SetInputMediaType(mixed, &audio_type(false)?, None)?;
        w.BeginWriting()?;
        // After a seek, video starts at the keyframe before `start` but audio only at `start`, so
        // the cut would open with up to 2 s of silence. Finding that keyframe and seeking again
        // just past it gets sound for the whole cut.
        seek(&r, start)?;
        let keyframe = loop {
            let (mut idx, mut flags, mut ts, mut sample) = (0u32, 0u32, 0i64, None);
            r.ReadSample(ANY_STREAM, 0, Some(&mut idx), Some(&mut flags), Some(&mut ts), Some(&mut sample))?;
            if idx as usize == video {
                if sample.is_some() {
                    break ts;
                }
                anyhow::ensure!(flags & END_OF_STREAM == 0, "nothing in that range");
            }
        };
        seek(&r, (keyframe + 10_000) as f64 / SEC)?;

        // Same flow as trim: the clip starts at the first video frame (`first`), earlier samples
        // wait. Frames from before it (B-frames of the previous group) can't be shown. When
        // encoding, frames past the frame rate wanted are dropped (`next` is when the next is due).
        let end_t = (end * SEC) as i64;
        let mut next = 0;
        let mut put = |m: &mut Mixer, first: i64, out: &Out, ts: i64, s: &IMFSample| -> anyhow::Result<bool> {
            match *out {
                Out::Copy(o) if ts >= first => {
                    rebase(s, ts, m.base)?;
                    w.WriteSample(o, s)?;
                    Ok(true)
                }
                Out::Encode(o) if ts >= first => {
                    let Some(e) = enc else { return Ok(false) };
                    let step = (SEC / e.fps) as i64;
                    if ts - m.base + step / 2 < next {
                        return Ok(false);
                    }
                    next = ts - m.base + step;
                    w.WriteSample(o, &reframe(&r, video as u32, s, e, ts - m.base)?)?;
                    progress(((ts - first) as f64 / (end_t - first).max(1) as f64) as f32);
                    Ok(true)
                }
                Out::Copy(_) | Out::Encode(_) => Ok(false),
                Out::Mix(t) => {
                    m.push(t, ts, &floats(s)?);
                    Ok(false)
                }
            }
        };
        let mut done = vec![false; outs.len()];
        let mut ended = vec![false; tracks];
        let mut mixer: Option<Mixer> = None;
        let mut first = 0;
        let mut held: Vec<(usize, i64, IMFSample)> = Vec::new();
        let mut wrote = 0u32;
        while done.iter().any(|d| !d) {
            let (mut idx, mut flags, mut ts, mut sample) = (0u32, 0u32, 0i64, None);
            r.ReadSample(ANY_STREAM, 0, Some(&mut idx), Some(&mut flags), Some(&mut ts), Some(&mut sample))?;
            let s = idx as usize;
            if s >= outs.len() {
                continue;
            }
            let past_end = sample.is_some() && ts > end_t;
            if let Some(sample) = sample.filter(|_| !past_end) {
                if s == video && mixer.is_none() {
                    first = ts;
                    let mut m = Mixer::new(tracks, timeline_start(&sample, ts));
                    for (hs, hts, hsample) in held.drain(..) {
                        put(&mut m, first, &outs[hs], hts, &hsample)?;
                    }
                    put(&mut m, first, &outs[s], ts, &sample)?;
                    wrote += 1;
                    mixer = Some(m);
                } else if let Some(m) = &mut mixer {
                    wrote += put(m, first, &outs[s], ts, &sample)? as u32;
                } else {
                    held.push((s, ts, sample));
                }
            }
            if past_end {
                r.SetStreamSelection(idx, false)?;
            }
            if past_end || flags & END_OF_STREAM != 0 {
                done[s] = true;
                if let Out::Mix(t) = outs[s] {
                    ended[t] = true;
                }
            }
            if let Some(m) = &mut mixer {
                ended.iter().enumerate().filter(|(_, e)| **e).for_each(|(t, _)| m.end(t));
                write_mix(&w, mixed, m, gains)?;
            }
        }
        anyhow::ensure!(wrote > 0, "nothing in that range");
        if let Some(m) = &mut mixer {
            (0..tracks).for_each(|t| m.end(t));
            write_mix(&w, mixed, m, gains)?;
        }
        w.Finalize()?;
    }
    Ok(())
}

/// A copy made for sharing (export).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Share {
    Discord10,
    Discord50,
    P720,
    /// The middle of the frame, 9:16, for TikTok and Shorts.
    Vertical,
}

impl Share {
    /// Goes in the copy's file name: "Clip (Discord 10MB).mp4".
    pub fn label(self) -> &'static str {
        match self {
            Share::Discord10 => "Discord 10MB",
            Share::Discord50 => "Discord 50MB",
            Share::P720 => "720p",
            Share::Vertical => "Vertical",
        }
    }

    /// The size the copy has to fit, in MB.
    fn limit_mb(self) -> Option<u64> {
        match self {
            Share::Discord10 => Some(10),
            Share::Discord50 => Some(50),
            _ => None,
        }
    }
}

/// The mixed track's bitrate.
const AUDIO_KBPS: u32 = 192;
/// Below this the video turns to mush, so a size preset asks for a shorter clip instead.
const MIN_KBPS: f64 = 700.0;

/// What a share copy's video becomes: the part of the frame kept (x, y, width, height), the size
/// that's scaled to, the frame rate (frames past it are dropped) and the bitrate.
#[derive(Clone, Copy, Debug)]
struct Encode {
    crop: [u32; 4],
    width: u32,
    height: u32,
    fps: f64,
    kbps: u32,
    /// On the graphics card's encoder; off to retry in software.
    hw: bool,
}

/// How to encode a `w`x`h` clip at `fps`, `secs` long, for `share`. The size presets spend what
/// fits (keeping 10% back for the file itself and the encoder going over), and go down to 720p or
/// 480p at 30 fps when that's little.
fn plan(share: Share, w: u32, h: u32, fps: f64, secs: f64) -> anyhow::Result<Encode> {
    let even = |v: f64| (v as u32) & !1;
    let (cw, ch) = match share {
        Share::Vertical if w * 16 > h * 9 => (even(h as f64 * 9.0 / 16.0), even(h as f64)),
        Share::Vertical => (even(w as f64), even(w as f64 * 16.0 / 9.0)),
        _ => (even(w as f64), even(h as f64)),
    };
    let mut fps = fps.min(60.0);
    let (height, kbps) = match share.limit_mb() {
        Some(mb) => {
            let budget = mb as f64 * 8000.0 * 0.9; // kilobits
            let kbps = budget / secs.max(1.0) - AUDIO_KBPS as f64;
            let longest = budget / (MIN_KBPS + AUDIO_KBPS as f64);
            let other = if mb == 10 { ", or pick 50 MB" } else { "" };
            anyhow::ensure!(kbps >= MIN_KBPS, "This clip is too long to look OK at {mb} MB. Trim it to under {longest:.0} s first{other}.");
            if kbps < 1500.0 {
                fps = fps.min(30.0);
            }
            (if kbps < 1500.0 { 480 } else if kbps < 3000.0 { 720 } else { 1080 }, kbps)
        }
        None if share == Share::Vertical => (1920, f64::MAX),
        None => (720, f64::MAX),
    };
    let height = height.min(ch);
    // Rounded to an even number, which H.264 needs.
    let width = (height as f64 * cw as f64 / ch as f64 / 2.0).round() as u32 * 2;
    // About 0.12 bits a pixel looks good for game footage; more is wasted.
    let kbps = kbps.min(width as f64 * height as f64 * fps * 0.12 / 1000.0) as u32;
    Ok(Encode { crop: [((w - cw) / 2) & !1, ((h - ch) / 2) & !1, cw, ch], width, height, fps, kbps, hw: true })
}

/// A copy of `src` for sharing at `dst` (see Share), with track 1's sound. On the graphics card
/// when it can, else in software. `progress` hears how far along it is, 0 to 1 (twice, if a size
/// preset came out too big and is made again).
pub fn export(src: &Path, dst: &Path, share: Share, progress: &dyn Fn(f32)) -> anyhow::Result<()> {
    let r = reader(src, false)?;
    let secs = length(&r)?;
    let size = unsafe { r.GetNativeMediaType(FIRST_VIDEO, 0)?.GetUINT64(&MF_MT_FRAME_SIZE)? };
    let gains = track1(audio_streams(&r).len());
    drop(r);
    let mut e = plan(share, (size >> 32) as u32, size as u32, frame_rate(src).unwrap_or(60.0), secs)?;
    for again in [false, true] {
        gpu_else_cpu(dst, e, &|| false, |e| remix(src, dst, 0.0, secs, &gains, Some(e), progress))?;
        let Some(limit) = share.limit_mb().map(|mb| mb * 1_000_000) else { break };
        let got = std::fs::metadata(dst)?.len();
        if got <= limit {
            break;
        }
        anyhow::ensure!(!again, "it came out at {:.1} MB, over {} MB. Trim it a little and try again.", got as f64 / 1e6, limit / 1_000_000);
        // The graphics card's encoder takes the bitrate as a hint and can go over by half on
        // long clips; once more, asking for less by as much as it went over.
        e.kbps = (e.kbps as f64 * limit as f64 * 0.95 / got as f64) as u32;
    }
    Ok(())
}

/// The longest part that can be saved as a GIF, in seconds. GIFs get big fast.
pub const GIF_MAX_SECS: f64 = 15.0;
const GIF_WIDTH: usize = 480;
const GIF_FPS: f64 = 15.0;

/// `start`..`end` seconds of `src` as a looping GIF at `dst`, 480 px wide at 15 fps, no sound.
/// `progress` hears how far along it is, 0 to 1.
pub fn save_gif(src: &Path, dst: &Path, start: f64, end: f64, progress: &dyn Fn(f32)) -> anyhow::Result<()> {
    let r = reader(src, true)?;
    let end = end.min(length(&r)?);
    anyhow::ensure!(end > start, "the end has to come after the start");
    anyhow::ensure!(end - start <= GIF_MAX_SECS + 0.01, "a GIF can be up to {GIF_MAX_SECS:.0} s long");
    unsafe {
        r.SetStreamSelection(ALL_STREAMS, false)?;
        r.SetStreamSelection(FIRST_VIDEO, true)?;
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        r.SetCurrentMediaType(FIRST_VIDEO, None, &t)?;
        let current = r.GetCurrentMediaType(FIRST_VIDEO)?;
        let size = current.GetUINT64(&MF_MT_FRAME_SIZE)?;
        let (w, h) = ((size >> 32) as usize, (size & 0xffff_ffff) as usize);
        let stride = current.GetUINT32(&MF_MT_DEFAULT_STRIDE).map_or((w * 4) as i32, |s| s as i32);
        let row = stride.unsigned_abs() as usize;
        let (ow, oh) = (GIF_WIDTH.min(w), (h * GIF_WIDTH.min(w)).div_ceil(w.max(1)));
        let mut out = gif::Encoder::new(std::io::BufWriter::new(std::fs::File::create(dst)?), ow as u16, oh as u16, &[])?;
        out.set_repeat(gif::Repeat::Infinite)?;
        seek(&r, start)?;
        let (start_t, end_t, step) = ((start * SEC) as i64, (end * SEC) as i64, SEC / GIF_FPS);
        // Frame n is due at start + n steps; decoding begins at the keyframe before the start.
        let mut n = 0u32;
        let mut px = vec![0u8; ow * oh * 4];
        while (n as f64 * step) < (end_t - start_t) as f64 {
            let (mut flags, mut ts, mut sample) = (0u32, 0i64, None);
            r.ReadSample(FIRST_VIDEO, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))?;
            if ts > end_t || flags & END_OF_STREAM != 0 {
                break;
            }
            let Some(sample) = sample.filter(|_| ts as f64 + step / 2.0 >= start_t as f64 + n as f64 * step) else { continue };
            let buf = sample.ConvertToContiguousBuffer()?;
            let (mut ptr, mut len) = (std::ptr::null_mut(), 0u32);
            buf.Lock(&mut ptr, None, Some(&mut len))?;
            let data = std::slice::from_raw_parts(ptr, len as usize);
            let fits = data.len() >= row * h;
            if fits {
                scale(data, row, 4, [0, 0, w, h], &mut px, ow, oh);
            }
            let _ = buf.Unlock();
            anyhow::ensure!(fits, "the video frames are smaller than the clip says");
            // BGRA to RGBA; a negative stride means the rows came bottom-up.
            let mut rgba: Vec<u8> = Vec::with_capacity(px.len());
            for y in 0..oh {
                let sy = if stride < 0 { oh - 1 - y } else { y };
                for [b, g, r, _] in px[sy * ow * 4..(sy + 1) * ow * 4].as_chunks::<4>().0 {
                    rgba.extend_from_slice(&[*r, *g, *b, 255]);
                }
            }
            let mut f = gif::Frame::from_rgba_speed(ow as u16, oh as u16, &mut rgba, 10);
            // Delays are in 1/100 s; rounded so they add up to 15 fps over the whole GIF.
            let cs = |k: u32| (k as f64 * 100.0 / GIF_FPS).round() as u16;
            f.delay = cs(n + 1) - cs(n);
            out.write_frame(&f)?;
            n += 1;
            progress(((ts - start_t) as f64 / (end_t - start_t).max(1) as f64) as f32);
        }
        anyhow::ensure!(n > 0, "nothing in that range");
    }
    Ok(())
}

/// One part of a joined video: `start`..`end` seconds of `path`, its audio tracks mixed at
/// `gains` (one per track, as trim_mixed takes them).
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub path: PathBuf,
    pub start: f64,
    pub end: f64,
    pub gains: Vec<f32>,
}

/// Music under a joined video: a sound file (mp3, m4a, wav) at `volume` (0 to 1), looped or cut
/// to length.
pub struct Music {
    pub path: PathBuf,
    pub volume: f32,
}

/// How long the fade at each joint lasts, and the music's fade out at the very end, in seconds.
const FADE: f64 = 0.4;
const MUSIC_FADE: f64 = 2.0;

/// The parts one after another as one MP4 at `dst`, re-encoded at the first part's size and frame
/// rate (other sizes are stretched to it). With `fades`, each part fades in from black and out to
/// black, its sound too. `music` plays under it all and fades out at the end. On the graphics card
/// when it can, else in software. `progress` hears how far along it is (0 to 1); once `stop`
/// says so it gives up with an error.
pub fn join(parts: &[Part], dst: &Path, fades: bool, music: Option<&Music>, progress: &dyn Fn(f32), stop: &dyn Fn() -> bool) -> anyhow::Result<()> {
    let first = parts.first().ok_or_else(|| anyhow::anyhow!("nothing to join"))?;
    let size = unsafe { reader(&first.path, false)?.GetNativeMediaType(FIRST_VIDEO, 0)?.GetUINT64(&MF_MT_FRAME_SIZE)? };
    let (w, h) = ((size >> 32) as u32 & !1, size as u32 & !1);
    let fps = frame_rate(&first.path).unwrap_or(60.0);
    // About 0.12 bits a pixel, as plan uses.
    let kbps = (w as f64 * h as f64 * fps * 0.12 / 1000.0) as u32;
    let e = Encode { crop: [0, 0, w, h], width: w, height: h, fps, kbps, hw: true };
    gpu_else_cpu(dst, e, stop, |e| join_as(parts, dst, e, fades, music, progress, stop))
}

/// Runs `encode` with `e` on the graphics card, and if that fails (and `stop` doesn't say to
/// give up) once more in software. A file the failed try left at `dst` is removed.
fn gpu_else_cpu(dst: &Path, mut e: Encode, stop: &dyn Fn() -> bool, encode: impl Fn(&Encode) -> anyhow::Result<()>) -> anyhow::Result<()> {
    let Err(err) = encode(&e) else { return Ok(()) };
    let _ = std::fs::remove_file(dst);
    if stop() {
        return Err(err);
    }
    if !cfg!(test) {
        crate::log::write(&format!("Hardware encoding didn't work ({err:#}); trying in software."));
    }
    e.hw = false;
    encode(&e)
}


/// A sink writer for `dst`, with the graphics card's encoders allowed when `hw`.
unsafe fn sink_writer(dst: &Path, hw: bool) -> anyhow::Result<IMFSinkWriter> {
    unsafe {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 2)?;
        let attrs = attrs.ok_or_else(|| anyhow::anyhow!("no attributes"))?;
        attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
        if hw {
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
        }
        Ok(MFCreateSinkWriterFromURL(&HSTRING::from(dst.as_os_str()), None, &attrs)?)
    }
}

/// Has the reader decode video `stream` to NV12, which the H.264 encoder takes.
unsafe fn decode_to_nv12(r: &IMFSourceReader, stream: u32) -> anyhow::Result<()> {
    unsafe {
        let nv12 = MFCreateMediaType()?;
        nv12.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        nv12.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        Ok(r.SetCurrentMediaType(stream, None, &nv12)?)
    }
}

/// Track volumes for `n` audio tracks the way a clip plays: track 1 only.
pub fn track1(n: usize) -> Vec<f32> {
    (0..n).map(|i| if i == 0 { 1.0 } else { 0.0 }).collect()
}

/// join, encoding as `e` says.
fn join_as(parts: &[Part], dst: &Path, e: &Encode, fades: bool, music: Option<&Music>, progress: &dyn Fn(f32), stop: &dyn Fn() -> bool) -> anyhow::Result<()> {
    let step = SEC / e.fps;
    // Each part's length in output frames; its sound is cut to match.
    let mut frames = Vec::new();
    for p in parts {
        let end = p.end.min(duration(&p.path)?);
        anyhow::ensure!(end > p.start, "the end has to come after the start");
        frames.push(((end - p.start) * e.fps).round().max(1.0) as u64);
    }
    let total: u64 = frames.iter().sum();
    // Where output video frame `n` falls in the sound, in 48 kHz frames.
    let sound_at = |n: u64| (n as f64 * RATE as f64 / e.fps).round() as u64;
    // How loud (0 to 1) a part is at `t` seconds of its `len`: rising from black at the start,
    // back down at the end.
    let fade = |t: f64, len: f64| if fades { (t.min(len - t) / FADE.min(len / 3.0)).clamp(0.0, 1.0) as f32 } else { 1.0 };
    let mut music = match music {
        Some(m) => Some((AudioSource::open(&m.path)?, m.volume)),
        None => None,
    };
    let music_end = sound_at(total);
    unsafe {
        let w = sink_writer(dst, e.hw)?;
        let vo = w.AddStream(&video_type(e, true)?)?;
        w.SetInputMediaType(vo, &video_type(e, false)?, None)?;
        let ao = w.AddStream(&audio_type(true)?)?;
        w.SetInputMediaType(ao, &audio_type(false)?, None)?;
        w.BeginWriting()?;
        // Writes a part's sound at frame `at` of the output: faded, with the music (looped, then
        // faded out at the very end) mixed in. `part` is where the part's sound starts and its length.
        let mut sound = |pcm: &mut [f32], at: u64, part: (u64, u64)| -> anyhow::Result<()> {
            let n = pcm.len() / 2;
            let (tune, volume) = match &mut music {
                Some((m, volume)) => {
                    let mut t = m.read(n, &[1.0])?;
                    if t.len() < n * 2 {
                        m.seek(0.0)?;
                        t.extend(m.read(n - t.len() / 2, &[1.0])?);
                    }
                    (t, *volume)
                }
                None => (Vec::new(), 0.0),
            };
            for (i, lr) in pcm.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                let k = at + i as u64;
                let g = fade(((k - part.0) as f64 + 0.5) / RATE as f64, part.1 as f64 / RATE as f64);
                let m = volume * (music_end.saturating_sub(k) as f64 / (MUSIC_FADE * RATE as f64)).min(1.0) as f32;
                for (c, x) in lr.iter_mut().enumerate() {
                    *x = limit(*x * g + tune.get(i * 2 + c).copied().unwrap_or(0.0) * m);
                }
            }
            write_pcm(&w, ao, at, pcm)
        };
        let mut done = 0u64; // output video frames written
        for (p, &n) in parts.iter().zip(&frames) {
            let r = reader(&p.path, false)?;
            let audio = audio_streams(&r);
            anyhow::ensure!(p.gains.len() == audio.len(), "got {} track volumes for {} audio tracks", p.gains.len(), audio.len());
            let video = (0..)
                .map_while(|i| r.GetNativeMediaType(i, 0).ok().map(|t| (i, t)))
                .find(|(_, t)| t.GetGUID(&MF_MT_MAJOR_TYPE).is_ok_and(|g| g == MFMediaType_Video))
                .map(|(i, _)| i)
                .ok_or_else(|| anyhow::anyhow!("the clip has no video"))?;
            let size = r.GetNativeMediaType(video, 0)?.GetUINT64(&MF_MT_FRAME_SIZE)?;
            // This part's whole picture, scaled to the output's size.
            let pe = Encode { crop: [0, 0, (size >> 32) as u32 & !1, size as u32 & !1], ..*e };
            r.SetStreamSelection(ALL_STREAMS, false)?;
            r.SetStreamSelection(video, true)?;
            decode_to_nv12(&r, video)?;
            for &a in &audio {
                r.SetStreamSelection(a, true)?;
                decode_to_float(&r, a)?;
            }
            seek(&r, p.start)?;
            let start_t = (p.start * SEC) as i64;
            let end_t = start_t + (n as f64 * step) as i64;
            let len = n as f64 / e.fps;
            let (from, sounds) = (sound_at(done), sound_at(done + n) - sound_at(done));
            let mut mixer = Mixer::new(audio.len(), start_t);
            // This part's video and sound frames written so far.
            let (mut k, mut heard) = (0u64, 0u64);
            // Writes this part's frames up to `upto` as `last` (the newest decoded frame, and its
            // pixels once worked out).
            let fill = |k: &mut u64, upto: u64, last: &mut (IMFSample, Option<Vec<u8>>)| -> anyhow::Result<()> {
                if *k >= upto {
                    return Ok(());
                }
                let px = match &mut last.1 {
                    Some(px) => px,
                    none => none.insert(frame_pixels(&r, video, &last.0, &pe)?),
                };
                while *k < upto {
                    let f = fade((*k as f64 + 0.5) / e.fps, len);
                    w.WriteSample(vo, &nv12_sample(px, &pe, ((done + *k) as f64 * step) as i64, f)?)?;
                    *k += 1;
                }
                progress((done + *k) as f32 / total as f32);
                Ok(())
            };
            // Mixed sound so far, up to the part's length; with `pad`, silence for what's missing.
            let mut pour = |mixer: &mut Mixer, heard: &mut u64, pad: bool| -> anyhow::Result<()> {
                while *heard < sounds {
                    let want = ((sounds - *heard) as usize).min(RATE as usize / 10);
                    let mut pcm = mixer.pop(want, &p.gains);
                    if pad {
                        pcm.resize(want * 2, 0.0);
                    } else if pcm.is_empty() {
                        break;
                    }
                    sound(&mut pcm, from + *heard, (from, sounds))?;
                    *heard += pcm.len() as u64 / 2;
                }
                Ok(())
            };
            let mut last: Option<(IMFSample, Option<Vec<u8>>)> = None;
            let mut video_on = true;
            let mut ended = vec![false; audio.len()];
            while video_on || ended.iter().any(|e| !e) {
                anyhow::ensure!(!stop(), "stopped");
                let (mut idx, mut flags, mut ts, mut sample) = (0u32, 0u32, 0i64, None);
                r.ReadSample(ANY_STREAM, 0, Some(&mut idx), Some(&mut flags), Some(&mut ts), Some(&mut sample))?;
                let eos = flags & END_OF_STREAM != 0;
                if idx == video && video_on {
                    if let Some(s) = sample {
                        // The frames due before this one is the nearest show the one before it
                        // (or this one, at the very start). Decoding starts at a keyframe before
                        // the part, so the first few go by without any due.
                        let upto = (((ts - start_t) as f64 - step / 2.0) / step).ceil().clamp(0.0, n as f64) as u64;
                        let mut l = last.take().unwrap_or((s.clone(), None));
                        fill(&mut k, upto, &mut l)?;
                        last = Some(if l.0 == s { l } else { (s, None) });
                    }
                    if k == n || eos {
                        video_on = false;
                        r.SetStreamSelection(video, false)?;
                    }
                } else if let Some(t) = audio.iter().position(|&a| a == idx) {
                    match sample {
                        Some(s) if ts <= end_t => mixer.push(t, ts, &floats(&s)?),
                        Some(_) => {
                            ended[t] = true;
                            r.SetStreamSelection(idx, false)?;
                        }
                        None => {}
                    }
                    if ended[t] || eos {
                        ended[t] = true;
                        mixer.end(t);
                    }
                }
                pour(&mut mixer, &mut heard, false)?;
            }
            let mut l = last.ok_or_else(|| anyhow::anyhow!("nothing in that range"))?;
            fill(&mut k, n, &mut l)?;
            (0..audio.len()).for_each(|t| mixer.end(t));
            pour(&mut mixer, &mut heard, true)?;
            done += n;
        }
        w.Finalize()?;
    }
    Ok(())
}

/// The encoder's output (H.264) or input (NV12) for `e`.
unsafe fn video_type(e: &Encode, h264: bool) -> anyhow::Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, if h264 { &MFVideoFormat_H264 } else { &MFVideoFormat_NV12 })?;
        // Pairs packed as high and low 32 bits.
        t.SetUINT64(&MF_MT_FRAME_SIZE, (e.width as u64) << 32 | e.height as u64)?;
        t.SetUINT64(&MF_MT_FRAME_RATE, ((e.fps * 1000.0).round() as u64) << 32 | 1000)?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, 1 << 32 | 1)?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        if h264 {
            t.SetUINT32(&MF_MT_AVG_BITRATE, e.kbps * 1000)?;
            t.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
        }
        Ok(t)
    }
}

/// The decoded NV12 frame in `s` cropped and scaled as `e` says, as a new sample at `time`.
unsafe fn reframe(r: &IMFSourceReader, stream: u32, s: &IMFSample, e: &Encode, time: i64) -> anyhow::Result<IMFSample> {
    unsafe { nv12_sample(&frame_pixels(r, stream, s, e)?, e, time, 1.0) }
}

/// The decoded NV12 frame in `s` cropped and scaled as `e` says.
unsafe fn frame_pixels(r: &IMFSourceReader, stream: u32, s: &IMFSample, e: &Encode) -> anyhow::Result<Vec<u8>> {
    unsafe {
        let t = r.GetCurrentMediaType(stream)?;
        let width = (t.GetUINT64(&MF_MT_FRAME_SIZE)? >> 32) as usize;
        let stride = t.GetUINT32(&MF_MT_DEFAULT_STRIDE).map_or(width, |s| (s as i32).unsigned_abs() as usize);
        let buf = s.ConvertToContiguousBuffer()?;
        let (mut ptr, mut len) = (std::ptr::null_mut(), 0u32);
        buf.Lock(&mut ptr, None, Some(&mut len))?;
        let data = std::slice::from_raw_parts(ptr, len as usize);
        // Rows of brightness; the decoder pads the height (1080 to 1088), colour comes after.
        let rows = len as usize * 2 / 3 / stride.max(1);
        let [cx, cy, cw, ch] = e.crop.map(|v| v as usize);
        let (ow, oh) = (e.width as usize, e.height as usize);
        let fits = cx + cw <= width && cy + ch <= rows;
        let mut o = vec![0u8; ow * oh * 3 / 2];
        if fits {
            let (y, uv) = data.split_at(stride * rows);
            let (oy, ouv) = o.split_at_mut(ow * oh);
            scale(y, stride, 1, [cx, cy, cw, ch], oy, ow, oh);
            scale(uv, stride, 2, [cx / 2, cy / 2, cw / 2, ch / 2], ouv, ow / 2, oh / 2);
        }
        let _ = buf.Unlock();
        anyhow::ensure!(fits, "the video frames are smaller than the clip says");
        Ok(o)
    }
}

/// An NV12 frame (from frame_pixels) as a sample at `time`, faded toward black by `fade`
/// (1 as it is, 0 black).
unsafe fn nv12_sample(px: &[u8], e: &Encode, time: i64, fade: f32) -> anyhow::Result<IMFSample> {
    unsafe {
        let out = MFCreateMemoryBuffer(px.len() as u32)?;
        let mut optr = std::ptr::null_mut();
        out.Lock(&mut optr, None, None)?;
        let o = std::slice::from_raw_parts_mut(optr, px.len());
        if fade >= 1.0 {
            o.copy_from_slice(px);
        } else {
            // Black is brightness 16 and colour 128.
            let luma = (e.width * e.height) as usize;
            for (i, (o, &v)) in o.iter_mut().zip(px).enumerate() {
                let black = if i < luma { 16.0 } else { 128.0 };
                *o = (black + (v as f32 - black) * fade.max(0.0)) as u8;
            }
        }
        out.Unlock()?;
        out.SetCurrentLength(px.len() as u32)?;
        let f = MFCreateSample()?;
        f.AddBuffer(&out)?;
        f.SetSampleTime(time)?;
        f.SetSampleDuration((SEC / e.fps) as i64)?;
        Ok(f)
    }
}

/// Crops a plane (rows `stride` bytes apart, `n` bytes a pixel: 1 for brightness, 2 for NV12's
/// colour pairs) to `crop` (x, y, width, height in pixels) and scales that to `ow`x`oh` into
/// `dst`, blending the nearest four pixels.
fn scale(src: &[u8], stride: usize, n: usize, crop: [usize; 4], dst: &mut [u8], ow: usize, oh: usize) {
    let [cx, cy, cw, ch] = crop;
    if (cw, ch) == (ow, oh) {
        for (y, row) in dst.chunks_exact_mut(ow * n).enumerate() {
            let at = (cy + y) * stride + cx * n;
            row.copy_from_slice(&src[at..at + ow * n]);
        }
        return;
    }
    // Where an output pixel's centre falls in the crop: the pixel before it, how far on to the
    // next (0 to 256), and the next.
    let pos = |i: usize, out: usize, size: usize| {
        let f = ((i as f64 + 0.5) * size as f64 / out as f64 - 0.5).clamp(0.0, (size - 1) as f64);
        (f as usize, (f.fract() * 256.0) as u32, (f as usize + 1).min(size - 1))
    };
    let cols: Vec<_> = (0..ow).map(|x| pos(x, ow, cw)).collect();
    // Bands of rows on several cores: a full-length export scales thousands of frames.
    let band = oh.div_ceil(std::thread::available_parallelism().map_or(1, |c| c.get().min(8)));
    std::thread::scope(|s| {
        for (b, rows) in dst.chunks_mut(band * ow * n).enumerate() {
            let (cols, pos) = (&cols, &pos);
            s.spawn(move || {
                for (i, row) in rows.chunks_exact_mut(ow * n).enumerate() {
                    let (y0, fy, y1) = pos(b * band + i, oh, ch);
                    let (r0, r1) = (&src[(cy + y0) * stride + cx * n..], &src[(cy + y1) * stride + cx * n..]);
                    for (px, &(x0, fx, x1)) in row.chunks_exact_mut(n).zip(cols) {
                        for (c, v) in px.iter_mut().enumerate() {
                            let mix = |r: &[u8]| r[x0 * n + c] as u32 * (256 - fx) + r[x1 * n + c] as u32 * fx;
                            *v = ((mix(r0) * (256 - fy) + mix(r1) * fy + (1 << 15)) >> 16) as u8;
                        }
                    }
                }
            });
        }
    });
}

/// The mixed track's format: 16-bit PCM going into the writer, AAC at 192 kbps coming out, the
/// same as OBS records.
unsafe fn audio_type(aac: bool) -> anyhow::Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        t.SetGUID(&MF_MT_SUBTYPE, if aac { &MFAudioFormat_AAC } else { &MFAudioFormat_PCM })?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, RATE)?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
        t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        if aac {
            t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AUDIO_KBPS * 1000 / 8)?;
        } else {
            t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)?;
            t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, RATE * 4)?;
        }
        Ok(t)
    }
}

/// Writes whatever the mixer has ready, in pieces of at most 100 ms.
unsafe fn write_mix(w: &IMFSinkWriter, stream: u32, m: &mut Mixer, gains: &[f32]) -> anyhow::Result<()> {
    loop {
        let at = m.next;
        let pcm = m.pop(RATE as usize / 10, gains);
        if pcm.is_empty() {
            return Ok(());
        }
        unsafe { write_pcm(w, stream, at, &pcm)? };
    }
}

/// Writes interleaved stereo floats as one 16-bit PCM sample starting at frame `at`.
unsafe fn write_pcm(w: &IMFSinkWriter, stream: u32, at: u64, pcm: &[f32]) -> anyhow::Result<()> {
    // Frames to 100 ns units without float rounding: 10_000_000 / 48_000 = 625 / 3.
    let time = |frames: u64| (frames * 625 / 3) as i64;
    unsafe {
        let bytes = (pcm.len() * 2) as u32;
        let buf = MFCreateMemoryBuffer(bytes)?;
        let mut ptr = std::ptr::null_mut();
        buf.Lock(&mut ptr, None, None)?;
        let out = std::slice::from_raw_parts_mut(ptr as *mut i16, pcm.len());
        for (o, x) in out.iter_mut().zip(pcm) {
            *o = (x * 32767.0) as i16;
        }
        buf.Unlock()?;
        buf.SetCurrentLength(bytes)?;
        let s = MFCreateSample()?;
        s.AddBuffer(&buf)?;
        s.SetSampleTime(time(at))?;
        s.SetSampleDuration(time(at + pcm.len() as u64 / 2) - time(at))?;
        w.WriteSample(stream, &s)?;
    }
    Ok(())
}

/// Asks the reader to decode `stream` to 48 kHz stereo float, the format the mixer works in.
unsafe fn decode_to_float(r: &IMFSourceReader, stream: u32) -> anyhow::Result<()> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float)?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, RATE)?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
        t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 32)?;
        t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 8)?;
        t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, RATE * 8)?;
        r.SetCurrentMediaType(stream, None, &t)
            .map_err(|e| anyhow::anyhow!("couldn't decode an audio track to 48 kHz stereo. {}", e.message()))?;
    }
    Ok(())
}

/// Waveform steps per second.
pub const PEAKS_PER_SEC: usize = 20;

/// How loud the clip's track 1 (the full mix) is, as the loudest sample (0 to 1) in each
/// 1/PEAKS_PER_SEC of a second, for drawing a waveform. Stops early, with what it has, once
/// `wanted` says no. A clip without sound gives an empty list.
pub fn peaks(path: &Path, wanted: impl Fn() -> bool) -> anyhow::Result<Vec<f32>> {
    let r = reader(path, false)?;
    let Some(&stream) = audio_streams(&r).first() else { return Ok(Vec::new()) };
    let step = (RATE as usize / PEAKS_PER_SEC) as i64;
    let mut out: Vec<f32> = Vec::new();
    unsafe {
        r.SetStreamSelection(ALL_STREAMS, false)?;
        r.SetStreamSelection(stream, true)?;
        decode_to_float(&r, stream)?;
        while wanted() {
            let (mut flags, mut ts, mut sample) = (0u32, 0i64, None);
            r.ReadSample(stream, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))?;
            if let Some(s) = sample {
                let first = (ts as f64 * RATE as f64 / SEC).round() as i64;
                for (i, [l, r]) in floats(&s)?.as_chunks::<2>().0.iter().enumerate() {
                    let b = ((first + i as i64) / step).max(0) as usize;
                    if b >= out.len() {
                        out.resize(b + 1, 0.0);
                    }
                    out[b] = out[b].max(l.abs()).max(r.abs()).min(1.0);
                }
            }
            if flags & END_OF_STREAM != 0 {
                break;
            }
        }
    }
    Ok(out)
}

/// The loudest moments in a waveform (from peaks), in seconds, loudest first: steps well above
/// the clip's usual (median) loudness, at most 10. Loud steps less than 1.5 s apart are one
/// moment, marked where it's loudest.
pub fn loud_moments(wave: &[f32]) -> Vec<f64> {
    let mut sorted = wave.to_vec();
    sorted.sort_by(f32::total_cmp);
    let Some(&median) = sorted.get(sorted.len() / 2) else { return Vec::new() };
    let loud = (median * 2.0).max(median + 0.3);
    let apart = PEAKS_PER_SEC * 3 / 2;
    // Each moment as (its loudest step, the last loud step in it).
    let mut moments: Vec<(usize, usize)> = Vec::new();
    for i in (0..wave.len()).filter(|&i| wave[i] >= loud) {
        match moments.last_mut() {
            Some((top, last)) if i - *last < apart => {
                if wave[i] > wave[*top] {
                    *top = i;
                }
                *last = i;
            }
            _ => moments.push((i, i)),
        }
    }
    moments.sort_by(|a, b| wave[b.0].total_cmp(&wave[a.0]));
    moments.iter().take(10).map(|&(i, _)| i as f64 / PEAKS_PER_SEC as f64).collect()
}

/// The PCM in a decoded sample: interleaved stereo floats.
unsafe fn floats(s: &IMFSample) -> anyhow::Result<Vec<f32>> {
    unsafe {
        let buf = s.ConvertToContiguousBuffer()?;
        let (mut ptr, mut len) = (std::ptr::null_mut(), 0u32);
        buf.Lock(&mut ptr, None, Some(&mut len))?;
        let pcm = std::slice::from_raw_parts(ptr as *const f32, len as usize / 4).to_vec();
        let _ = buf.Unlock();
        Ok(pcm)
    }
}

/// Soft limiter for the summed tracks: untouched up to 0.8, then eases toward full scale
/// instead of clipping hard.
fn limit(x: f32) -> f32 {
    let a = x.abs();
    if a <= 0.8 { x } else { x.signum() * (0.8 + 0.2 * ((a - 0.8) / 0.2).tanh()) }
}

/// Lines decoded tracks up by timestamp and sums them with per-track gains. Each track keeps a
/// queue of stereo frames that starts at frame `next`; a mixed frame goes out once every track
/// that hasn't ended has reached it.
struct Mixer {
    /// Time of frame 0, in 100 ns units.
    base: i64,
    /// The next frame pop returns.
    next: u64,
    /// Queued interleaved stereo, and whether the track has ended.
    tracks: Vec<(VecDeque<f32>, bool)>,
}

impl Mixer {
    fn new(tracks: usize, base: i64) -> Mixer {
        Mixer { base, next: 0, tracks: (0..tracks).map(|_| (VecDeque::new(), false)).collect() }
    }

    /// Adds `pcm` from `track`, decoded at `ts`. Gaps are filled with silence and anything
    /// before the base time or already queued is dropped.
    fn push(&mut self, track: usize, ts: i64, pcm: &[f32]) {
        let Some((q, _)) = self.tracks.get_mut(track) else { return };
        let at = ((ts - self.base) as f64 * RATE as f64 / SEC).round() as i64;
        let queued_to = (self.next + q.len() as u64 / 2) as i64;
        let mut pcm = pcm;
        if at > queued_to + SLACK {
            q.extend(std::iter::repeat_n(0.0, (at - queued_to) as usize * 2));
        } else if at < queued_to - SLACK {
            pcm = pcm.get((queued_to - at) as usize * 2..).unwrap_or(&[]);
        }
        q.extend(pcm);
    }

    fn end(&mut self, track: usize) {
        if let Some(t) = self.tracks.get_mut(track) {
            t.1 = true;
        }
    }

    /// Every track has ended and everything queued has been mixed.
    fn done(&self) -> bool {
        self.tracks.iter().all(|(q, ended)| *ended && q.is_empty())
    }

    /// Up to `max` mixed frames: as far as every live track has reached, or everything left
    /// once all have ended.
    fn pop(&mut self, max: usize, gains: &[f32]) -> Vec<f32> {
        let frames = |q: &VecDeque<f32>| q.len() / 2;
        let ready = match self.tracks.iter().filter(|t| !t.1).map(|t| frames(&t.0)).min() {
            Some(n) => n,
            None => self.tracks.iter().map(|t| frames(&t.0)).max().unwrap_or(0),
        };
        let n = ready.min(max);
        let mut out = vec![0.0f32; n * 2];
        for (i, (q, _)) in self.tracks.iter_mut().enumerate() {
            let g = gains.get(i).copied().unwrap_or(0.0);
            let take = (n * 2).min(q.len());
            for (o, x) in out.iter_mut().zip(q.drain(..take)) {
                *o += g * x;
            }
        }
        out.iter_mut().for_each(|x| *x = limit(*x));
        self.next += n as u64;
        out
    }
}

/// Every audio track of a clip, decoded and mixed, for the player's own sound output.
pub(crate) struct AudioSource {
    reader: IMFSourceReader,
    streams: Vec<u32>,
    mixer: Mixer,
}

impl AudioSource {
    pub(crate) fn open(path: &Path) -> anyhow::Result<AudioSource> {
        let reader = reader(path, false)?;
        let streams = audio_streams(&reader);
        anyhow::ensure!(!streams.is_empty(), "the clip has no sound");
        unsafe {
            reader.SetStreamSelection(ALL_STREAMS, false)?;
            for &s in &streams {
                reader.SetStreamSelection(s, true)?;
                decode_to_float(&reader, s)?;
            }
        }
        let mixer = Mixer::new(streams.len(), 0);
        Ok(AudioSource { reader, streams, mixer })
    }

    /// Restarts decoding at `secs`.
    pub(crate) fn seek(&mut self, secs: f64) -> anyhow::Result<()> {
        seek(&self.reader, secs)?;
        self.mixer = Mixer::new(self.streams.len(), (secs.max(0.0) * SEC) as i64);
        Ok(())
    }

    /// Up to `max` mixed stereo frames, decoding as needed. Fewer only at the end of the clip.
    pub(crate) fn read(&mut self, max: usize, gains: &[f32]) -> anyhow::Result<Vec<f32>> {
        let mut out = Vec::with_capacity(max * 2);
        while out.len() < max * 2 {
            let pcm = self.mixer.pop(max - out.len() / 2, gains);
            if !pcm.is_empty() {
                out.extend(pcm);
                continue;
            }
            if self.mixer.done() {
                break;
            }
            let (mut idx, mut flags, mut ts, mut sample) = (0u32, 0u32, 0i64, None);
            unsafe {
                self.reader.ReadSample(ANY_STREAM, 0, Some(&mut idx), Some(&mut flags), Some(&mut ts), Some(&mut sample))?
            };
            let Some(track) = self.streams.iter().position(|&s| s == idx) else { continue };
            if let Some(s) = sample {
                self.mixer.push(track, ts, &unsafe { floats(&s) }?);
            }
            if flags & END_OF_STREAM != 0 {
                self.mixer.end(track);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A made-up clip (no real clip read): `secs` of a colored scene with a moving bar, H.264
    /// 640x360 30 fps and one quiet AAC track.
    pub(crate) fn synth_clip(dst: &Path, secs: u32, rgb: [u8; 3]) {
        init().unwrap();
        let e = Encode { crop: [0, 0, 640, 360], width: 640, height: 360, fps: 30.0, kbps: 1500, hw: false };
        let yuv = |r: f32, g: f32, b: f32| {
            let y = 16.0 + 0.257 * r + 0.504 * g + 0.098 * b;
            let u = 128.0 - 0.148 * r - 0.291 * g + 0.439 * b;
            let v = 128.0 + 0.439 * r - 0.368 * g - 0.071 * b;
            (y as u8, u as u8, v as u8)
        };
        unsafe {
            let w = sink_writer(dst, false).unwrap();
            let v = w.AddStream(&video_type(&e, true).unwrap()).unwrap();
            w.SetInputMediaType(v, &video_type(&e, false).unwrap(), None).unwrap();
            let a = w.AddStream(&audio_type(true).unwrap()).unwrap();
            w.SetInputMediaType(a, &audio_type(false).unwrap(), None).unwrap();
            w.BeginWriting().unwrap();
            let (ww, hh) = (640usize, 360usize);
            for f in 0..secs as usize * 30 {
                let mut px = vec![0u8; ww * hh * 3 / 2];
                let bar = (f * 8) % ww;
                for y in 0..hh {
                    for x in 0..ww {
                        let shade = 0.55 + 0.45 * (y as f32 / hh as f32);
                        let (mut r, mut g, mut b) = (rgb[0] as f32 * shade, rgb[1] as f32 * shade, rgb[2] as f32 * shade);
                        if x >= bar && x < bar + 60 {
                            (r, g, b) = (240.0, 240.0, 230.0);
                        }
                        if y > hh * 3 / 4 && (x / 40) % 2 == 0 {
                            (r, g, b) = (r * 0.5, g * 0.5, b * 0.5);
                        }
                        let (yy, u, vv) = yuv(r, g, b);
                        px[y * ww + x] = yy;
                        if y % 2 == 0 && x % 2 == 0 {
                            let i = ww * hh + (y / 2) * ww + x;
                            px[i] = u;
                            px[i + 1] = vv;
                        }
                    }
                }
                let t = (f as f64 * SEC / 30.0) as i64;
                w.WriteSample(v, &nv12_sample(&px, &e, t, 1.0).unwrap()).unwrap();
            }
            let total = secs as u64 * RATE as u64;
            let mut at = 0u64;
            while at < total {
                let k = (total - at).min(4800);
                let pcm: Vec<f32> = (at..at + k).flat_map(|s| [0.05 * (s as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin(); 2]).collect();
                write_pcm(&w, a, at, &pcm).unwrap();
                at += k;
            }
            w.Finalize().unwrap();
        }
    }

    /// Six made-up clips in `dir` (written once, oldest first so their times sort), for rendering
    /// the app's screens without reading anyone's real clips.
    pub(crate) fn demo_clips(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        let clips = [
            ("VALORANT 2026-11-07 21-07-33.mp4", [200, 60, 70]),
            ("VALORANT 2026-11-07 20-53-10.mp4", [60, 90, 200]),
            ("Elden Ring 2026-11-06 23-41-10.mp4", [170, 130, 50]),
            ("Desktop 2026-11-06 18-10-53.mp4", [50, 150, 120]),
            ("Rocket League 2026-11-05 22-02-41.mp4", [40, 110, 220]),
            ("Counter-Strike 2 2026-11-04 19-30-05.mp4", [150, 150, 90]),
        ];
        for (name, rgb) in clips.into_iter().rev() {
            let p = dir.join(name);
            if !p.exists() {
                synth_clip(&p, 6, rgb);
                std::thread::sleep(std::time::Duration::from_millis(1100));
            }
        }
    }

    /// Writes the demo clips folder to SYNTH_OUT. Run on demand.
    #[test]
    #[ignore]
    fn synth_demo_clips() {
        demo_clips(Path::new(&std::env::var("SYNTH_OUT").unwrap()));
    }

    // The two slowest tests encode video (exports_a_real_clip_for_sharing, joins_real_clips) and
    // are #[ignore]d, so a debug `cargo test` stays quick. Run them with:
    // cargo test --release -- --ignored exports_a_real_clip_for_sharing joins_real_clips

    /// Uses the newest real clip in the clips folder; skipped if there isn't one.
    fn sample_clip() -> Option<std::path::PathBuf> {
        let folder = crate::config::Config::load().folder;
        std::fs::read_dir(folder)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("mp4")) && !p.to_string_lossy().contains("(trim"))
            .max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok())
    }

    #[test]
    fn trims_a_real_clip_without_reencoding() {
        let Some(src) = sample_clip() else { return };
        let len = duration(&src).unwrap();
        assert!(len > 5.0, "clip is {len} s");
        let dst = std::env::temp_dir().join("instant-replay-trim-test.mp4");
        let _ = std::fs::remove_file(&dst);
        let (start, end) = (len - 5.0, len);
        trim(&src, &dst, start, end).unwrap();
        let got = duration(&dst).unwrap();
        // Starts on a keyframe up to 2 s early (keyint_sec 2).
        assert!((4.5..=7.5).contains(&got), "trimmed clip is {got} s");
        let audio = reader(&dst, false).unwrap();
        let streams = (0..).take_while(|&i| unsafe { audio.GetNativeMediaType(i, 0) }.is_ok()).count();
        let src_streams = (0..).take_while(|&i| unsafe { reader(&src, false).unwrap().GetNativeMediaType(i, 0) }.is_ok()).count();
        assert_eq!(streams, src_streams, "every track kept");
        // Decodes at the start and near the end.
        let a = frame(&dst, 0.0, 160).unwrap();
        let b = frame(&dst, got - 0.5, 160).unwrap();
        assert!(a.width == 160 && a.height > 0 && b.rgba.len() == b.width * b.height * 4);
        let _ = std::fs::remove_file(&dst);
    }

    #[test]
    fn keyframe_decodes_a_real_clip() {
        let Some(src) = sample_clip() else { return };
        let f = keyframe(&src, duration(&src).unwrap() / 2.0, 160).unwrap();
        assert!(f.width == 160 && f.height > 0 && f.rgba.len() == f.width * f.height * 4);
    }

    /// Loudness of each track in tone_clip, and their pitches.
    pub(crate) const TONES: [(f32, f32); 3] = [(0.5, 440.0), (0.3, 660.0), (0.2, 880.0)];

    /// A test clip with three audio tracks of known sound, since the real clips may have only
    /// one: the video of the smallest real clip (from a temp copy, so the original is never
    /// touched) plus one steady sine tone per track (TONES). Written to %TEMP%\`name`; None if
    /// there are no clips.
    pub(crate) fn tone_clip(name: &str) -> Option<std::path::PathBuf> {
        let folder = crate::config::Config::load().folder;
        let real = std::fs::read_dir(folder)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("mp4")))
            .min_by_key(|p| p.metadata().map(|m| m.len()).unwrap_or(u64::MAX))?;
        let copy = std::env::temp_dir().join(format!("{name}.src.mp4"));
        std::fs::copy(&real, &copy).unwrap();
        let dst = std::env::temp_dir().join(name);
        let _ = std::fs::remove_file(&dst);
        let r = reader(&copy, false).unwrap();
        unsafe {
            r.SetStreamSelection(ALL_STREAMS, false).unwrap();
            r.SetStreamSelection(FIRST_VIDEO, true).unwrap();
            let t = r.GetNativeMediaType(FIRST_VIDEO, 0).unwrap();
            r.SetCurrentMediaType(FIRST_VIDEO, None, &t).unwrap();
            let w = MFCreateSinkWriterFromURL(&HSTRING::from(dst.as_os_str()), None, None).unwrap();
            let tracks: Vec<u32> = TONES
                .iter()
                .map(|_| {
                    let o = w.AddStream(&audio_type(true).unwrap()).unwrap();
                    w.SetInputMediaType(o, &audio_type(false).unwrap(), None).unwrap();
                    o
                })
                .collect();
            let v = w.AddStream(&t).unwrap();
            w.SetInputMediaType(v, &t, None).unwrap();
            w.BeginWriting().unwrap();
            let (mut base, mut frames) = (None, 0u64);
            loop {
                let (mut flags, mut ts, mut sample) = (0u32, 0i64, None);
                r.ReadSample(FIRST_VIDEO, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample)).unwrap();
                if let Some(s) = sample {
                    let b = *base.get_or_insert_with(|| timeline_start(&s, ts));
                    rebase(&s, ts, b).unwrap();
                    w.WriteSample(v, &s).unwrap();
                    // Tones up to this frame, so the tracks stay interleaved with the video.
                    let upto = (ts - b).max(0) as u64 * RATE as u64 / SEC as u64;
                    while frames < upto {
                        let n = (upto - frames).min(4800);
                        for (&o, &(amp, hz)) in tracks.iter().zip(&TONES) {
                            let pcm: Vec<f32> = (frames..frames + n)
                                .flat_map(|k| [amp * (k as f32 * hz * std::f32::consts::TAU / RATE as f32).sin(); 2])
                                .collect();
                            write_pcm(&w, o, frames, &pcm).unwrap();
                        }
                        frames += n;
                    }
                }
                if flags & END_OF_STREAM != 0 {
                    break;
                }
            }
            w.Finalize().unwrap();
        }
        drop(r);
        std::fs::remove_file(&copy).unwrap();
        Some(dst)
    }

    /// The loudest sample in `path` from `start` on, mixed with `gains`. With `settle`, the first
    /// half second is left out: AAC overshoots for a few ms wherever decoding starts.
    fn peak(path: &Path, start: f64, settle: bool, gains: &[f32]) -> f32 {
        let mut src = AudioSource::open(path).unwrap();
        src.seek(start).unwrap();
        if settle {
            src.read(RATE as usize / 2, gains).unwrap();
        }
        let mut peak = 0.0f32;
        loop {
            let pcm = src.read(4800, gains).unwrap();
            if pcm.is_empty() {
                return peak;
            }
            peak = pcm.iter().fold(peak, |p, x| p.max(x.abs()));
        }
    }

    #[test]
    fn peaks_follow_track_one() {
        let Some(src) = tone_clip("instant-replay-peaks.mp4") else { return };
        let len = duration(&src).unwrap();
        let took = std::time::Instant::now();
        let p = peaks(&src, || true).unwrap();
        eprintln!("waveform of {len:.0} s took {:?}", took.elapsed());
        // One step per 50 ms of the clip, give or take the audio's last frame.
        assert!((p.len() as f64 - len * PEAKS_PER_SEC as f64).abs() <= 2.0, "{} steps for {len} s", p.len());
        // Track 1 is a steady tone at 0.5 (AAC wobbles a little; the first steps settle).
        let mid = &p[10..p.len() - 2];
        assert!(mid.iter().all(|&x| (0.45..=0.56).contains(&x)), "{:?}", &mid[..mid.len().min(20)]);
        // Asked to stop right away, it reads nothing.
        assert!(peaks(&src, || false).unwrap().is_empty());
        std::fs::remove_file(&src).unwrap();
    }

    #[test]
    fn trim_mixed_makes_one_track_at_the_chosen_volumes() {
        let Some(src) = tone_clip("instant-replay-mix-src.mp4") else { return };
        assert_eq!(audio_tracks(&src).unwrap(), 3);
        let len = duration(&src).unwrap();
        assert!(len > 5.0, "clip is {len} s");
        let (start, end) = (len - 5.0, len);
        let dst = std::env::temp_dir().join("instant-replay-mix-test.mp4");
        // The source decodes to the tones it was written with.
        let p = peak(&src, start, true, &[1.0, 0.0, 0.0]);
        assert!((0.45..0.55).contains(&p), "track 1 of the source peaks at {p}");

        // (gains, expected peak range of the mix)
        let cases: [([f32; 3], std::ops::Range<f32>); 4] = [
            ([1.0, 0.0, 0.0], 0.45..0.55),
            ([0.0, 2.0, 0.0], 0.55..0.65),
            ([0.0, 0.0, 0.0], 0.0..0.001),
            // 2x everything sums past full scale; the limiter keeps it under.
            ([2.0, 2.0, 2.0], 0.8..1.05),
        ];
        for (gains, want) in cases {
            let _ = std::fs::remove_file(&dst);
            trim_mixed(&src, &dst, start, end, &gains).unwrap();
            let got = duration(&dst).unwrap();
            // Starts on a keyframe up to 2 s early (keyint_sec 2), like trim.
            assert!((4.5..=7.5).contains(&got), "trimmed clip is {got} s");
            assert_eq!(audio_tracks(&dst).unwrap(), 1);
            // Loudness is measured once the decoder settles; silence over the whole file.
            let p = peak(&dst, 0.0, gains != [0.0; 3], &[1.0]);
            eprintln!("gains {gains:?}: {got:.2} s, peak {p}");
            assert!(want.contains(&p), "gains {gains:?} peak at {p}, expected {want:?}");
        }
        let a = frame(&dst, duration(&dst).unwrap() - 0.5, 160).unwrap();
        assert!(a.width == 160 && a.height > 0);

        assert!(trim_mixed(&src, &dst, start, end, &[1.0; 2]).is_err(), "wrong gain count is refused");
        let _ = std::fs::remove_file(&dst);
        std::fs::remove_file(&src).unwrap();
    }

    #[test]
    fn trim_mixed_on_a_real_clip() {
        // The smallest real clip (any number of tracks), copied so the original is never touched.
        let folder = crate::config::Config::load().folder;
        let Some(real) = std::fs::read_dir(folder)
            .ok()
            .and_then(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("mp4"))).min_by_key(|p| p.metadata().map(|m| m.len()).unwrap_or(u64::MAX)))
        else {
            return;
        };
        let src = std::env::temp_dir().join("instant-replay-one-src.mp4");
        std::fs::copy(&real, &src).unwrap();
        let dst = std::env::temp_dir().join("instant-replay-one-test.mp4");
        let _ = std::fs::remove_file(&dst);
        let len = duration(&src).unwrap();
        let tracks = audio_tracks(&src).unwrap();
        assert!(tracks >= 1);
        trim_mixed(&src, &dst, (len - 5.0).max(0.0), len, &vec![0.5; tracks]).unwrap();
        let got = duration(&dst).unwrap();
        assert!((4.5..=7.5).contains(&got) || len < 5.0, "trimmed clip is {got} s");
        assert_eq!(audio_tracks(&dst).unwrap(), 1);
        let _ = std::fs::remove_file(&dst);
        std::fs::remove_file(&src).unwrap();
    }

    #[test]
    fn trim_keeps_track_order_and_starts_with_sound() {
        let Some(src) = tone_clip("instant-replay-order-src.mp4") else { return };
        let len = duration(&src).unwrap();
        let dst = std::env::temp_dir().join("instant-replay-order-test.mp4");
        let _ = std::fs::remove_file(&dst);
        // Start mid group, so the cut opens on a keyframe before it.
        trim(&src, &dst, len - 5.0, len).unwrap();
        assert_eq!(audio_tracks(&dst).unwrap(), 3);
        // Each track still holds its own tone.
        for (t, &(amp, _)) in TONES.iter().enumerate() {
            let mut g = [0.0; 3];
            g[t] = 1.0;
            let p = peak(&dst, 0.0, true, &g);
            assert!((amp - 0.05..amp + 0.05).contains(&p), "track {} peaks at {p}, expected {amp}", t + 1);
        }
        // Sound from the very start, not after a gap.
        let mut a = AudioSource::open(&dst).unwrap();
        a.seek(0.0).unwrap();
        let opening = a.read(RATE as usize / 4, &[1.0, 0.0, 0.0]).unwrap();
        let p = opening.iter().fold(0.0f32, |p, x| p.max(x.abs()));
        assert!(p > 0.3, "first 0.25 s peaks at {p}");
        // The first frame is a keyframe a player can start on: seeking to 0 lands at 0, not 2 s in.
        let r = reader(&dst, false).unwrap();
        seek(&r, 0.0).unwrap();
        let (mut flags, mut ts, mut sample) = (0u32, 0i64, None);
        unsafe { r.ReadSample(FIRST_VIDEO, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample)) }.unwrap();
        assert!(sample.is_some() && ts < SEC as i64 / 10, "first frame at {} s", ts as f64 / SEC);
        drop((a, r));
        let _ = std::fs::remove_file(&dst);
        std::fs::remove_file(&src).unwrap();
    }

    #[test]
    fn share_plans() {
        // 10 s of 1080p60 fits 10 MB at full size; 5 minutes doesn't, and says how long would.
        let e = plan(Share::Discord10, 1920, 1080, 60.0, 10.0).unwrap();
        assert_eq!((e.width, e.height, e.fps), (1920, 1080, 60.0));
        assert!((6000..7200).contains(&e.kbps), "{} kbps", e.kbps);
        let err = plan(Share::Discord10, 1920, 1080, 60.0, 300.0).unwrap_err().to_string();
        assert!(err.contains("under 81 s") && err.contains("50 MB"), "{err}");
        // 2 minutes at 50 MB is 720p; 4 minutes drops to 480p at 30 fps.
        assert_eq!(plan(Share::Discord50, 1920, 1080, 60.0, 120.0).unwrap().height, 720);
        let low = plan(Share::Discord50, 1920, 1080, 60.0, 240.0).unwrap();
        assert_eq!((low.width, low.height, low.fps), (854, 480, 30.0));
        // 720p caps the frame rate at 60 and never scales up.
        let e = plan(Share::P720, 2560, 1440, 120.0, 300.0).unwrap();
        assert_eq!((e.width, e.height, e.fps, e.crop), (1280, 720, 60.0, [0, 0, 2560, 1440]));
        assert_eq!(plan(Share::P720, 1280, 720, 30.0, 9.0).unwrap().width, 1280);
        // Vertical keeps the middle 9:16, at 1080x1920 only when the clip is that big.
        let v = plan(Share::Vertical, 1920, 1080, 60.0, 300.0).unwrap();
        assert_eq!((v.crop, v.width, v.height), ([656, 0, 606, 1080], 606, 1080));
        let v = plan(Share::Vertical, 3840, 2160, 60.0, 300.0).unwrap();
        assert_eq!((v.width, v.height), (1080, 1920));
    }

    #[test]
    fn scale_crops_and_blends() {
        // A 4x2 plane, rows 5 bytes apart (one byte of padding).
        let src = [0, 100, 200, 250, 9, 50, 150, 250, 250, 9];
        let mut out = [0u8; 2];
        // The middle two columns of the top row, as they are.
        scale(&src, 5, 1, [1, 0, 2, 1], &mut out, 2, 1);
        assert_eq!(out, [100, 200]);
        // The left half halved: each output pixel blends its two columns and rows.
        let mut one = [0u8; 1];
        scale(&src, 5, 1, [0, 0, 2, 2], &mut one, 1, 1);
        assert_eq!(one, [75]);
        // Colour pairs keep their two bytes apart.
        let mut uv = [0u8; 2];
        scale(&src, 5, 2, [0, 0, 2, 1], &mut uv, 1, 1);
        assert_eq!(uv, [100, 175]);
    }

    /// Each share preset on ~10 s of a real clip (a trimmed temp copy).
    #[test]
    #[ignore = "slow: encodes every share preset (about 45 s in a debug build)"]
    fn exports_a_real_clip_for_sharing() {
        let Some(real) = sample_clip() else { return };
        let src = std::env::temp_dir().join("instant-replay-share-src.mp4");
        let len = duration(&real).unwrap();
        trim(&real, &src, (len - 10.0).max(0.0), len).unwrap();
        let (len, src_bytes) = (duration(&src).unwrap(), std::fs::metadata(&src).unwrap().len());
        for share in [Share::Discord10, Share::Discord50, Share::P720, Share::Vertical] {
            let dst = std::env::temp_dir().join(format!("instant-replay-share-{}.mp4", share.label()));
            let _ = std::fs::remove_file(&dst);
            let took = std::time::Instant::now();
            let last = std::sync::Mutex::new(0.0f32);
            export(&src, &dst, share, &|f| *last.lock().unwrap() = f).unwrap();
            let bytes = std::fs::metadata(&dst).unwrap().len();
            let got = duration(&dst).unwrap();
            let f = frame(&dst, got / 2.0, 4000).unwrap();
            eprintln!("{share:?}: {:.1} MB, {}x{}, {got:.2} s of {len:.2}, {:?}", bytes as f64 / 1e6, f.width, f.height, took.elapsed());
            assert!((got - len).abs() < 0.2, "{share:?} is {got} s, the source {len} s");
            assert!(*last.lock().unwrap() > 0.95, "progress reached the end");
            assert_eq!(audio_tracks(&dst).unwrap(), 1);
            // Not a blank frame.
            assert!(f.rgba.chunks(4).any(|p| p[0] > 20 || p[1] > 20 || p[2] > 20));
            match share {
                // Close to the size, unless the clip itself is smaller than that (quiet footage
                // needs fewer bits, and the encoder doesn't pad).
                Share::Discord10 => assert!(bytes < 10_000_000 && (bytes > 6_000_000 || bytes > src_bytes / 2), "{bytes} bytes from {src_bytes}"),
                Share::Discord50 => assert!(bytes < 50_000_000),
                Share::P720 => assert_eq!(f.height, 720),
                Share::Vertical => assert!((f.width as f64 / f.height as f64 - 9.0 / 16.0).abs() < 0.01, "{}x{}", f.width, f.height),
            }
            let _ = std::fs::remove_file(&dst);
        }
        std::fs::remove_file(&src).unwrap();
    }

    /// Hand check, run in release (`cargo test --release real_clip_bench -- --ignored --nocapture`):
    /// on a temp copy of the biggest real clip (a full game clip), times each share preset, finds
    /// the loud moments, and saves a frame at each plus a 4 s GIF at the first into target\bench
    /// to look at. Prints what it finds rather than asserting much.
    #[test]
    #[ignore]
    fn real_clip_bench() {
        let folder = crate::config::Config::load().folder;
        let Some(real) = std::fs::read_dir(folder).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("mp4"))).max_by_key(|p| p.metadata().map(|m| m.len()).unwrap_or(0)) else { return };
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(r"target\bench");
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        let src = std::env::temp_dir().join("instant-replay-bench-src.mp4");
        std::fs::copy(&real, &src).unwrap();
        let len = duration(&src).unwrap();
        eprintln!("{}: {len:.1} s, {:.0} MB", real.display(), std::fs::metadata(&src).unwrap().len() as f64 / 1e6);
        for share in [Share::Discord10, Share::Discord50, Share::P720, Share::Vertical] {
            let dst = std::env::temp_dir().join(format!("instant-replay-bench-{}.mp4", share.label()));
            let took = std::time::Instant::now();
            match export(&src, &dst, share, &|_| {}) {
                Ok(()) => {
                    let f = frame(&dst, 1.0, 4000).unwrap();
                    eprintln!("{share:?}: {:.1} s, {:.1} MB, {}x{}", took.elapsed().as_secs_f64(), std::fs::metadata(&dst).unwrap().len() as f64 / 1e6, f.width, f.height);
                }
                Err(e) => eprintln!("{share:?}: refused in {:.1} s: {e:#}", took.elapsed().as_secs_f64()),
            }
            let _ = std::fs::remove_file(&dst);
        }
        let took = std::time::Instant::now();
        let wave = peaks(&src, || true).unwrap();
        let loud = loud_moments(&wave);
        eprintln!("Loud moments ({:.1} s to read the audio): {loud:.1?}", took.elapsed().as_secs_f64());
        for (i, &t) in loud.iter().take(8).enumerate() {
            let f = frame(&src, t, 960).unwrap();
            image::RgbaImage::from_raw(f.width as u32, f.height as u32, f.rgba).unwrap().save(out.join(format!("loud-{i}-{t:.0}s.png"))).unwrap();
        }
        if let Some(&t) = loud.first() {
            let took = std::time::Instant::now();
            save_gif(&src, &out.join("loud-0.gif"), (t - 2.0).max(0.0), t + 2.0, &|_| {}).unwrap();
            eprintln!("GIF of 4 s: {:.1} s, {:.1} MB", took.elapsed().as_secs_f64(), std::fs::metadata(out.join("loud-0.gif")).unwrap().len() as f64 / 1e6);
        }
        std::fs::remove_file(&src).unwrap();
    }

    /// 3 s of a temp copy of a real clip as a GIF: 480 wide, ~45 frames, looping.
    #[test]
    fn saves_a_real_clip_part_as_gif() {
        let Some(real) = sample_clip() else { return };
        let src = std::env::temp_dir().join("instant-replay-gif-src.mp4");
        std::fs::copy(&real, &src).unwrap();
        let dst = std::env::temp_dir().join("instant-replay-gif-test.gif");
        let _ = std::fs::remove_file(&dst);
        let at = duration(&src).unwrap() / 2.0;
        let took = std::time::Instant::now();
        let last = std::sync::Mutex::new(0.0f32);
        save_gif(&src, &dst, at, at + 3.0, &|f| *last.lock().unwrap() = f).unwrap();
        let bytes = std::fs::read(&dst).unwrap();
        let mut opts = gif::DecodeOptions::new();
        opts.set_color_output(gif::ColorOutput::Indexed);
        let mut d = opts.read_info(&bytes[..]).unwrap();
        let (w, h) = (d.width(), d.height());
        let mut frames = 0;
        let mut delay = 0u32;
        while let Some(f) = d.read_next_frame().unwrap() {
            frames += 1;
            delay += f.delay as u32;
        }
        eprintln!("GIF: {w}x{h}, {frames} frames, {delay} cs, {:.1} MB, {:?}", bytes.len() as f64 / 1e6, took.elapsed());
        assert!(bytes.starts_with(b"GIF89a"));
        assert_eq!(w, 480);
        assert!(h > 100 && h < 480, "{h} high");
        assert!((44..=46).contains(&frames), "{frames} frames");
        assert!((295..=305).contains(&delay), "{delay} cs long");
        assert!(bytes.len() > 50_000 && bytes.len() < 15_000_000, "{} bytes", bytes.len());
        assert!(*last.lock().unwrap() > 0.9, "progress reached the end");
        // Too long is refused.
        assert!(save_gif(&src, &dst, 0.0, GIF_MAX_SECS + 1.0, &|_| {}).is_err());
        let _ = std::fs::remove_file(&dst);
        std::fs::remove_file(&src).unwrap();
    }

    /// The last ~4 s of each of the two newest real clips (or the newest twice), as temp copies.
    fn two_short_clips(tag: &str) -> Option<[std::path::PathBuf; 2]> {
        let folder = crate::config::Config::load().folder;
        let mut real: Vec<_> = std::fs::read_dir(folder)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("mp4")) && !p.to_string_lossy().contains("(trim"))
            .collect();
        real.sort_by_key(|p| std::cmp::Reverse(p.metadata().and_then(|m| m.modified()).ok()));
        let pick = [real.first()?.clone(), real.get(1).or(real.first())?.clone()];
        Some([0, 1].map(|i| {
            let dst = std::env::temp_dir().join(format!("instant-replay-{tag}-{i}.mp4"));
            let len = duration(&pick[i]).unwrap();
            trim(&pick[i], &dst, (len - 4.0).max(0.0), len).unwrap();
            dst
        }))
    }

    /// A part of `path` with track 1 at full volume, the way a clip plays.
    fn part(path: &Path, start: f64, end: f64) -> Part {
        Part { path: path.to_path_buf(), start, end, gains: track1(audio_tracks(path).unwrap()) }
    }

    /// Two clips joined: as long as both, the first one's size and frame rate even when the second
    /// is smaller; with fades, it opens on black. Parts of one clip add up the same way.
    #[test]
    #[ignore = "slow: encodes two joins (about 20 s in a debug build)"]
    fn joins_real_clips() {
        let Some([a, b]) = two_short_clips("join") else { return };
        // The second at 720p, so it has to be scaled up to fit.
        let b720 = std::env::temp_dir().join("instant-replay-join-720.mp4");
        export(&b, &b720, Share::P720, &|_| {}).unwrap();
        // Only 1.5 s of it: scaling up is slow in a debug build.
        let (la, lb) = (duration(&a).unwrap(), 1.5);
        let fa = frame(&a, 1.0, 4000).unwrap();
        let dst = std::env::temp_dir().join("instant-replay-join-test.mp4");
        let took = std::time::Instant::now();
        let last = std::sync::Mutex::new(0.0f32);
        join(&[part(&a, 0.0, la), part(&b720, 0.0, lb)], &dst, false, None, &|f| *last.lock().unwrap() = f, &|| false).unwrap();
        let got = duration(&dst).unwrap();
        let f = frame(&dst, la + lb / 2.0, 4000).unwrap();
        eprintln!("joined {la:.2} + {lb:.2} s into {got:.2} s, {}x{}, {:?}", f.width, f.height, took.elapsed());
        assert!((got - (la + lb)).abs() <= 0.1, "{got} s");
        assert_eq!((f.width, f.height), (fa.width, fa.height));
        assert!((frame_rate(&dst).unwrap() - frame_rate(&a).unwrap()).abs() < 0.1);
        assert_eq!(audio_tracks(&dst).unwrap(), 1);
        assert!(*last.lock().unwrap() > 0.99, "progress reached the end");
        assert!(f.rgba.chunks(4).any(|p| p[0] > 20 || p[1] > 20 || p[2] > 20), "the scaled part isn't blank");

        // With fades the first frame is black.
        join(&[part(&a, 0.0, 1.5), part(&b, 1.0, 2.5)], &dst, true, None, &|_| {}, &|| false).unwrap();
        let f = frame(&dst, 0.0, 160).unwrap();
        let mean = f.rgba.chunks(4).map(|p| p[0] as f64 + p[1] as f64 + p[2] as f64).sum::<f64>() / (f.rgba.len() / 4 * 3) as f64;
        assert!(mean < 6.0, "first frame averages {mean}");
        assert!((duration(&dst).unwrap() - 3.0).abs() <= 0.1);

        // Several parts of one clip, in order.
        join(&[part(&a, 0.5, 1.5), part(&a, 2.0, 3.5)], &dst, false, None, &|_| {}, &|| false).unwrap();
        let got = duration(&dst).unwrap();
        assert!((got - 2.5).abs() <= 0.1, "parts came out {got} s");
        // Stopping gives up and leaves no file.
        assert!(join(&[part(&a, 0.0, la)], &dst, false, None, &|_| {}, &|| true).is_err());
        assert!(!dst.exists());
        for f in [a, b, b720] {
            std::fs::remove_file(f).unwrap();
        }
    }

    /// Music under a clip with its own sound off: heard the whole way (a 1 s tune, looped), the
    /// sound as long as the video, faded out at the end.
    #[test]
    fn joins_with_music() {
        let Some([a, b]) = two_short_clips("music") else { return };
        // 1 s of a 440 Hz tone at half volume: 44.1 kHz mono 16-bit, so it has to be converted.
        let wav = std::env::temp_dir().join("instant-replay-music.wav");
        let pcm: Vec<u8> = (0..44_100).flat_map(|k| ((0.5 * (k as f32 * 440.0 * std::f32::consts::TAU / 44_100.0).sin() * 32767.0) as i16).to_le_bytes()).collect();
        let mut file = Vec::new();
        file.extend(b"RIFF");
        file.extend((36 + pcm.len() as u32).to_le_bytes());
        file.extend(b"WAVEfmt ");
        for v in [16u32, 1 | 1 << 16, 44_100, 88_200, 2 | 16 << 16] {
            file.extend(v.to_le_bytes());
        }
        file.extend(b"data");
        file.extend((pcm.len() as u32).to_le_bytes());
        file.extend(pcm);
        std::fs::write(&wav, file).unwrap();
        let dst = std::env::temp_dir().join("instant-replay-music-test.mp4");
        let mut quiet = part(&a, 0.0, 3.5);
        quiet.gains.fill(0.0);
        join(&[quiet], &dst, false, Some(&Music { path: wav.clone(), volume: 1.0 }), &|_| {}, &|| false).unwrap();
        let got = duration(&dst).unwrap();
        assert!((got - 3.5).abs() <= 0.1, "{got} s");
        let mut src = AudioSource::open(&dst).unwrap();
        src.seek(0.0).unwrap();
        let mut sound = Vec::new();
        loop {
            let pcm = src.read(4800, &[1.0]).unwrap();
            if pcm.is_empty() {
                break;
            }
            sound.extend(pcm);
        }
        let secs = sound.len() as f64 / 2.0 / RATE as f64;
        let peak = |from: f64, to: f64| sound[(from * RATE as f64) as usize * 2..(to * RATE as f64) as usize * 2].iter().fold(0.0f32, |p, x| p.max(x.abs()));
        let (start, looped, end) = (peak(0.2, 0.9), peak(1.1, 1.4), peak(secs - 0.1, secs));
        eprintln!("{secs:.2} s of sound; peaks {start} {looped} {end}");
        assert!((secs - got).abs() <= 0.1, "{secs} s of sound in {got} s");
        assert!(start > 0.4 && looped > 0.35, "the tune plays, and again after 1 s");
        assert!(end < 0.1, "faded out at the end");
        drop(src);
        for f in [a, b, wav, dst] {
            std::fs::remove_file(f).unwrap();
        }
    }

    #[test]
    fn loud_moments_from_made_up_peaks() {
        let steps = |s: f64| (s * PEAKS_PER_SEC as f64) as usize;
        // 60 s of quiet (0.1) with a shot at 10 s (0.9), a shout over 30.0 to 30.5 s peaking at
        // 30.2 s (0.7), a bump at 45 s (0.3) that isn't loud enough, and 3 s of loud steps 1 s
        // apart (50 to 53 s) that are one moment, loudest at 52 s.
        let mut wave = vec![0.1f32; steps(60.0)];
        wave[steps(10.0)] = 0.9;
        wave[steps(30.0)..steps(30.5)].fill(0.6);
        wave[steps(30.2)] = 0.7;
        wave[steps(45.0)] = 0.3;
        for (s, v) in [(50.0, 0.5), (51.0, 0.5), (52.0, 0.55), (53.0, 0.5)] {
            wave[steps(s)] = v;
        }
        assert_eq!(loud_moments(&wave), vec![10.0, 30.2, 52.0]);
        // A loud clip needs more than a little extra to count.
        assert!(loud_moments(&vec![0.6; 100]).is_empty());
        assert!(loud_moments(&[]).is_empty());
        // Many spikes: the 10 loudest, 1.5 s or more apart, loudest first.
        let mut many = vec![0.05f32; steps(60.0)];
        for k in 0..30 {
            many[steps(k as f64 * 2.0)] = 0.5 + k as f32 / 100.0;
        }
        let got = loud_moments(&many);
        assert_eq!(got.len(), 10);
        assert_eq!(got[0], 58.0);
        assert!(got.windows(2).all(|w| w[0] > w[1]));
    }

    #[test]
    fn mixer_lines_tracks_up_by_time() {
        let mut m = Mixer::new(2, 0);
        // Track 0 starts 2 ms in (a 96 frame gap), track 1 starts on time.
        m.push(0, 20_000, &[0.5; 200]);
        m.push(1, 0, &[0.25; 296]);
        let out = m.pop(1000, &[1.0, 2.0]);
        assert_eq!(out.len(), 296);
        assert_eq!((out[0], out[295]), (0.5, limit(1.0)));
        // Audio already queued is dropped, gain 0 silences a track, and a finished track stops
        // holding the other back.
        m.push(0, 30_000, &[0.5; 96]);
        m.end(1);
        let rest = m.pop(1000, &[0.0, 1.0]);
        assert!(rest.len() == 96 && rest.iter().all(|x| *x == 0.0));
        m.end(0);
        assert!(m.done());
    }

    #[test]
    fn thumbnail_from_a_real_clip() {
        let Some(src) = sample_clip() else { return };
        let f = frame(&src, 1.0, 192).unwrap();
        assert!(f.width <= 192 && f.width > 100 && f.height > 50);
        // Not a blank frame.
        assert!(f.rgba.chunks(4).any(|p| p[0] > 20 || p[1] > 20 || p[2] > 20));
    }
}
