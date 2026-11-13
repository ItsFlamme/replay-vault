//! The hidden "recorder" OBS: our private copy in portable mode with its own config folder, so
//! the user's own OBS install, profile and scenes are never touched.

use std::collections::BTreeMap;
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;

use crate::config::{self, Config, DeviceSel};
use crate::display::{self, Monitor};
use crate::{devices, hotkey, log, obs_install};

pub const PORT: u16 = 4466;
const NAME: &str = "InstantReplay";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Process id of the recorder this app started; 0 when none.
static PID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The running recorder's process id, if this app started one.
pub fn pid() -> Option<u32> {
    Some(PID.load(std::sync::atomic::Ordering::Relaxed)).filter(|&p| p != 0)
}

/// A Windows program by its full path (`System32\<name>`, or the Windows folder for explorer), so
/// a same-named exe next to the app or in the current folder is never run instead.
pub fn windows_exe(name: &str) -> PathBuf {
    let win = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()));
    match name {
        "explorer.exe" => win.join(name),
        "powershell.exe" => win.join(r"System32\WindowsPowerShell\v1.0").join(name),
        _ => win.join("System32").join(name),
    }
}

fn obs_exe() -> anyhow::Result<PathBuf> {
    obs_install::find().ok_or_else(|| anyhow::anyhow!("OBS isn't installed yet; finish setup first"))
}

/// `--portable` makes OBS keep its settings in `<install>\config` instead of %APPDATA%.
pub fn portable_root(exe: &Path) -> PathBuf {
    exe.ancestors().nth(3).unwrap_or(Path::new(".")).join(r"config\obs-studio")
}

fn pid_file() -> PathBuf {
    config::dir().join("recorder.pid")
}

/// The monitor the user picked, else the primary one.
pub fn chosen_monitor(cfg: &Config) -> Option<Monitor> {
    pick_monitor(&display::list(), cfg)
}

/// `chosen_monitor` out of `all`, a list already made.
pub fn pick_monitor(all: &[Monitor], cfg: &Config) -> Option<Monitor> {
    all.iter().find(|m| m.id == cfg.monitor_id).or_else(|| all.first()).cloned()
}

/// Windows' SDR brightness (nits) when the recorded monitor has HDR on; when following the
/// mouse, that of any HDR monitor, since the capture may land on it.
fn hdr_white(cfg: &Config, mon: &Monitor) -> Option<u32> {
    hdr_white_of(cfg, mon, display::list)
}

/// `hdr_white`, with `all` giving every monitor when it's needed.
fn hdr_white_of(cfg: &Config, mon: &Monitor, all: impl FnOnce() -> Vec<Monitor>) -> Option<u32> {
    if cfg.follows_mouse() { all().iter().find_map(|m| m.hdr) } else { mon.hdr }
}

/// What the recorder's settings take from the monitors, for `cfg`: the recorded monitor, its
/// size, its refresh rate when the clips use it, and the HDR white level. A display change that
/// leaves this as it was needs no recorder restart.
#[derive(Clone, Debug, PartialEq)]
pub struct DisplayKey {
    id: String,
    size: (u32, u32),
    hz: Option<u32>,
    hdr: Option<u32>,
}

/// The `DisplayKey` of `cfg` on monitors `all`; None with no monitor at all.
pub fn display_key(cfg: &Config, all: &[Monitor]) -> Option<DisplayKey> {
    let mon = pick_monitor(all, cfg)?;
    Some(DisplayKey {
        hdr: hdr_white_of(cfg, &mon, || all.to_vec()),
        hz: (cfg.fps == 0).then_some(mon.hz),
        size: (mon.width, mon.height),
        id: mon.id,
    })
}

/// Copies the main OBS's version stamps so the hidden copy never shows "What's new" dialogs. Only
/// used when our copy's version.txt is missing.
fn version_lines(main_ini: &Path) -> String {
    fs::read_to_string(main_ini)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.starts_with("LastVersion=") || l.starts_with("InfoLastVersion="))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// Whose hardware video encoder the recorder uses.
#[derive(Clone, Copy, PartialEq)]
pub enum Gpu {
    Nvidia,
    Amd,
    Intel,
    /// No supported GPU: x264 on the CPU.
    None,
}

impl Gpu {
    pub fn label(self) -> &'static str {
        match self {
            Gpu::Nvidia => "NVIDIA NVENC",
            Gpu::Amd => "AMD AMF",
            Gpu::Intel => "Intel Quick Sync",
            Gpu::None => "CPU (x264)",
        }
    }
}

impl Gpu {
    /// PCI vendor ID of this GPU maker.
    pub fn vendor(self) -> u32 {
        match self {
            Gpu::Nvidia => 0x10DE,
            Gpu::Amd => 0x1002,
            Gpu::Intel => 0x8086,
            Gpu::None => 0,
        }
    }
}

/// A graphics adapter as Windows (DXGI) reports it.
pub struct Adapter {
    pub vendor: u32,
    pub name: String,
    /// Dedicated video memory; integrated GPUs report little or none, except AMD's, which report
    /// the slice of RAM set aside for them (often 1 to 4 GB).
    pub vram_mb: u64,
    /// Its place in Windows' list, for `shares_ram`.
    pub index: u32,
}

/// Whether adapter `index` is built into the processor and shares the PC's RAM (Direct3D 12's
/// "UMA" answer), which graphics memory alone can't tell for AMD's. None if Direct3D 12 can't say.
pub fn shares_ram(index: u32) -> Option<bool> {
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
    use windows::Win32::Graphics::Direct3D12::{D3D12_FEATURE_ARCHITECTURE, D3D12_FEATURE_DATA_ARCHITECTURE, D3D12CreateDevice, ID3D12Device};
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    unsafe {
        let adapter = CreateDXGIFactory1::<IDXGIFactory1>().ok()?.EnumAdapters1(index).ok()?;
        let mut device: Option<ID3D12Device> = None;
        D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device).ok()?;
        let mut arch = D3D12_FEATURE_DATA_ARCHITECTURE::default();
        device?.CheckFeatureSupport(D3D12_FEATURE_ARCHITECTURE, (&mut arch as *mut D3D12_FEATURE_DATA_ARCHITECTURE).cast(), size_of::<D3D12_FEATURE_DATA_ARCHITECTURE>() as u32).ok()?;
        Some(arch.UMA.as_bool())
    }
}

/// Hardware graphics adapters (software renderers left out).
pub fn adapters() -> Vec<Adapter> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1};
    let mut out = Vec::new();
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else { return out };
    let mut i = 0;
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(i) } {
        if let Ok(desc) = unsafe { adapter.GetDesc1() }
            && desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0 {
            let end = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
            out.push(Adapter {
                vendor: desc.VendorId,
                name: String::from_utf16_lossy(&desc.Description[..end]).trim().to_string(),
                vram_mb: desc.DedicatedVideoMemory as u64 / (1024 * 1024),
                index: i,
            });
        }
        i += 1;
    }
    out
}

/// Picks the encoder from the graphics adapters Windows reports: NVIDIA, then AMD, then Intel
/// (so a laptop's dedicated GPU wins over its integrated one), else the CPU.
pub fn gpu() -> Gpu {
    static GPU: std::sync::OnceLock<Gpu> = std::sync::OnceLock::new();
    *GPU.get_or_init(|| {
        // Testing / support override: INSTANT_REPLAY_ENCODER=nvidia|amd|intel|cpu.
        match std::env::var("INSTANT_REPLAY_ENCODER").unwrap_or_default().to_lowercase().as_str() {
            "nvidia" => return Gpu::Nvidia,
            "amd" => return Gpu::Amd,
            "intel" => return Gpu::Intel,
            "cpu" => return Gpu::None,
            _ => {}
        }
        let vendors: Vec<u32> = adapters().iter().map(|a| a.vendor).collect();
        let sys = PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into())).join("System32");
        if vendors.contains(&0x10DE) && sys.join("nvEncodeAPI64.dll").exists() {
            Gpu::Nvidia
        } else if vendors.contains(&0x1002) {
            Gpu::Amd
        } else if vendors.contains(&0x8086) {
            Gpu::Intel
        } else {
            Gpu::None
        }
    })
}

pub struct Plan {
    pub base_w: u32,
    pub base_h: u32,
    pub out_w: u32,
    pub out_h: u32,
    pub fps: u32,
    pub kbps: u64,
    /// Replay buffer size cap in MB; OBS holds the buffer in RAM.
    pub mb: u64,
    /// When the chosen clip length needs more than a quarter of the PC's RAM: the seconds that
    /// fit in `mb` instead (OBS drops the oldest part of the replay past its size cap).
    pub fits_secs: Option<u32>,
}

/// The most of the PC's RAM the replay buffer may take: a quarter, leaving the rest to the game.
pub fn buffer_cap_mb() -> u64 {
    (crate::profile::ram_mb() / 4).max(512)
}

pub fn plan(cfg: &Config, mon: &Monitor) -> Plan {
    // Canvas = the monitor; the clip is only ever scaled down, keeping the aspect ratio.
    let (base_w, base_h) = (mon.width, mon.height);
    // Encoders want even sizes.
    let out_h = (if cfg.out_height == 0 { base_h } else { cfg.out_height.min(base_h) }) & !1;
    let out_w = (base_w * out_h / base_h) & !1;
    let fps = if cfg.fps == 0 { mon.hz.max(1) } else { cfg.fps };
    // Bitrate scales with pixels per second, then by the quality preset; HEVC and AV1 need less.
    let quality = [1.5, 1.0, 0.7, 0.45][cfg.quality.min(3) as usize];
    let codec = match cfg.codec.as_str() {
        "hevc" => 0.75,
        "av1" => 0.65,
        _ => 1.0,
    };
    let base = out_w as f64 * out_h as f64 * fps as f64 / 6_000.0;
    let kbps = ((base * quality * codec) as u64).clamp(8_000, 150_000);
    // Video + each audio track (192 kbps) for the whole clip, with headroom for VBR peaks.
    let per_sec = kbps + 192 * cfg.track_count.clamp(1, 6) as u64;
    let need = per_sec * cfg.buffer_secs as u64 / 8 / 1000 * 5 / 4 + 64;
    let cap = buffer_cap_mb();
    let fits_secs = (need > cap).then(|| ((cap - 64) * 4 / 5 * 1000 * 8 / per_sec) as u32);
    Plan { base_w, base_h, out_w, out_h, fps, kbps, mb: need.min(cap), fits_secs }
}

/// The longest replay (10 s to 20 min) that fits under `buffer_cap_mb` at these settings, rounded
/// down to 10 s.
pub fn max_secs(cfg: &Config, mon: &Monitor) -> u32 {
    let longest = Config { buffer_secs: 1200, ..cfg.clone() };
    plan(&longest, mon).fits_secs.map_or(1200, |s| (s / 10 * 10).clamp(10, 1200))
}

/// The tracks (bit 0 = track 1) holding a mic together with other sound. Centering raises one
/// side of a track, which would move the mic (mono, so already in the middle) off to that side;
/// these tracks get the loud side copied instead (media::center).
pub fn mic_mixed_tracks(cfg: &Config) -> u8 {
    let n = cfg.tracks();
    let ticked = |mic: bool| -> u8 {
        let devices = cfg.devices.iter().filter(|(k, _)| k.starts_with("in:") == mic);
        let mut mask = devices.fold(0u8, |m, (_, d)| (0..n).filter(|&i| d.tracks[i]).fold(m, |m, i| m | 1 << i));
        if !mic {
            mask |= (0..n).filter(|&i| cfg.chat_tracks[i]).fold(0, |m, i| m | 1 << i);
            // Only these apps with no playback track ticked: the apps go on track 1.
            if cfg.only_apps && !cfg.apps.is_empty() && mask == 0 {
                mask = 1;
            }
        }
        mask
    };
    ticked(true) & ticked(false)
}

/// OBS's RecTracks: one bit per recorded audio track, tracks 1 to `count` (1 to 6).
pub fn track_mask(count: u8) -> u32 {
    (1u32 << count.clamp(1, 6)) - 1
}

/// "About 8.5 GB per hour of clips (144.4 MB for a full 1-minute replay)": the video bitrate
/// (always a set VBR bitrate) plus 192 kbps per audio track.
pub fn space_estimate(cfg: &Config, kbps: u64) -> String {
    let per_sec = (kbps + 192 * cfg.track_count as u64) * 1000 / 8;
    let secs = cfg.buffer_secs as u64;
    let len = if secs.is_multiple_of(60) { format!("{}-minute", secs / 60) } else { format!("{secs}-second") };
    format!("About {} per hour of clips ({} for a full {len} replay)", crate::clips::size(per_sec * 3600), crate::clips::size(per_sec * secs))
}

/// Clip name date styles: OBS's format string, and how it looks.
pub const NAME_STYLES: [(&str, &str); 2] = [
    ("%CCYY-%MM-%DD %hh-%mm-%ss", "2026-11-06 18-42-33"),
    ("%MM-%DD-%CCYY %I-%mm-%ss %p", "11-06-2026 06-42-33 PM"),
];

/// What a clip saved in VALORANT now would be called. OBS names it by the date alone; the game
/// goes in front when the clip is moved into place.
pub fn example_name(cfg: &Config) -> String {
    let (_, date) = NAME_STYLES[(cfg.name_style as usize).min(NAME_STYLES.len() - 1)];
    format!("VALORANT {date}.mp4")
}

pub fn write_config(cfg: &Config) -> anyhow::Result<()> {
    let exe = obs_exe()?;
    let mon = chosen_monitor(cfg).ok_or_else(|| anyhow::anyhow!("no monitor found"))?;
    let main_root = dirs::config_dir().unwrap_or_default().join("obs-studio");
    // Version stamps stop "what's new" dialogs: our private copy knows its own version (its
    // version.txt); without it, the user's main OBS settings are only read for theirs.
    let private_stamp = obs_install::private_version().and_then(|v| obs_install::last_version_stamp(&v));
    // Asked here, on the recorder's thread, each time it starts: the default can change any time.
    let defaults = devices::defaults();
    // A mic muted with its key starts muted, so no unmuted moment lands in the replay.
    let mic_muted = crate::obs::MIC_MUTED.load(std::sync::atomic::Ordering::Relaxed);
    log::write(&write_files(cfg, &portable_root(&exe), &mon, &main_root, private_stamp, &defaults, mic_muted)?);
    Ok(())
}

/// The recorder's encoder ID and its settings (recordEncoder.json) for `gpu` at `kbps`.
fn encoder_settings(gpu: Gpu, cfg: &Config, kbps: u64) -> (&'static str, serde_json::Value) {
    // Encoder IDs and setting names are OBS's own (obs-nvenc, obs-ffmpeg's AMF, obs-qsv11).
    let max = kbps * 3 / 2;
    let enc_profile = if cfg.codec == "h264" { "high" } else { "main" };
    // Encoder effort: 0 best quality (the original settings), 1 balanced, 2 max FPS. Lighter
    // levels do less work per frame, so clips look a little worse at the same bitrate.
    let effort = cfg.encoder_effort.min(2) as usize;
    let (nv_preset, nv_multipass) = [("p5", "qres"), ("p4", "disabled"), ("p1", "disabled")][effort];
    let amf_preset = ["quality", "balanced", "speed"][effort];
    let qsv_usage = ["TU4", "TU5", "TU7"][effort];
    let x264_preset = ["veryfast", "superfast", "ultrafast"][effort];
    let (encoder, mut enc_settings) = match (gpu, cfg.codec.as_str()) {
        // obs-nvenc (OBS 30.2+) reads "preset" and "adaptive_quantization"; "preset2" and
        // "psycho_aq" are the old names, which it only carries over for its old encoder IDs.
        // Both are written so either kind of OBS gets the same settings.
        (Gpu::Nvidia, codec) => (
            match codec { "hevc" => "obs_nvenc_hevc_tex", "av1" => "obs_nvenc_av1_tex", _ => "obs_nvenc_h264_tex" },
            json!({
                "rate_control": "VBR", "bitrate": kbps, "max_bitrate": max,
                "preset": nv_preset, "preset2": nv_preset, "tune": "hq", "multipass": nv_multipass,
                "profile": enc_profile, "bf": 2, "keyint_sec": 2,
            }),
        ),
        (Gpu::Amd, codec) => (
            match codec { "hevc" => "h265_texture_amf", "av1" => "av1_texture_amf", _ => "h264_texture_amf" },
            json!({
                "rate_control": "VBR", "bitrate": kbps, "max_bitrate": max,
                "preset": amf_preset, "profile": enc_profile, "keyint_sec": 2,
            }),
        ),
        (Gpu::Intel, codec) => (
            match codec { "hevc" => "obs_qsv11_hevc", "av1" => "obs_qsv11_av1", _ => "obs_qsv11_v2" },
            json!({
                "rate_control": "VBR", "bitrate": kbps, "max_bitrate": max,
                "target_usage": qsv_usage, "profile": enc_profile, "keyint_sec": 2,
            }),
        ),
        // No hardware encoder: H.264 on the CPU, light preset to leave the game its frames.
        (Gpu::None, _) => (
            "obs_x264",
            json!({ "rate_control": "VBR", "bitrate": kbps, "preset": x264_preset, "keyint_sec": 2 }),
        ),
    };
    // Psycho-visual tuning (adaptive quantization) runs on the GPU's CUDA cores, which games use too. Left at OBS's own
    // default for best quality.
    if gpu == Gpu::Nvidia && effort > 0 {
        enc_settings["adaptive_quantization"] = json!(false);
        enc_settings["psycho_aq"] = json!(false);
    }
    (encoder, enc_settings)
}

/// OBS's version stamp (`obs_install::last_version_stamp`) of 30.2, the first with hybrid MP4.
const HYBRID_MP4_FROM: u64 = 30 << 24 | 2 << 16;

/// The container of full recordings (`RecFormat2`) for our private OBS, by its version stamp.
/// Hybrid MP4 stays playable if the recorder, the app or the PC stops mid-recording, and becomes
/// a normal MP4 when the recording ends. OBS before 30.2 doesn't know the name and would take it
/// as the file extension, for replays too, with no muxer for it; an unknown version gets plain MP4
/// for that reason. Fragmented MP4 is no fallback: OBS hands its muxer flags to the replay buffer
/// as well, which would change every clip. With hybrid MP4, replays stay plain MP4 from the replay
/// buffer's own muxer.
fn rec_format(obs_stamp: Option<u64>) -> &'static str {
    if obs_stamp.is_some_and(|s| s >= HYBRID_MP4_FROM) { "hybrid_mp4" } else { "mp4" }
}

/// A scene item that fits its source into the `w` x `h` canvas whatever its size: OBS's "scale to
/// inner bounds" (bounds_type 2) keeps the aspect ratio and never stretches or crops, and
/// bounds_align 0 centres it, so a picture of another shape gets even bars (a 4:3 game on a 16:9
/// canvas at the sides, an ultrawide one above and below). The bounds' top-left corner (align 5)
/// is the canvas's. A monitor whose size changed, the one under the mouse, or a game at its own
/// resolution all fill the clip this way, and the clip's size never changes.
fn fit_to_canvas(name: &str, w: u32, h: u32) -> serde_json::Value {
    json!({
        "name": name, "visible": true, "pos": { "x": 0, "y": 0 }, "scale": { "x": 1, "y": 1 }, "align": 5,
        "bounds_type": 2, "bounds_align": 0, "bounds": { "x": w, "y": h },
    })
}

/// The sound devices to record, with the tracks (OBS mixers, bit 0 = track 1) each goes on: those
/// on any of the first `n` tracks. A device that Windows uses as the default right now (`defaults`)
/// and that is also ticked as the "default" one would be recorded twice, its sound doubled: it's
/// left out and its tracks go to "default" too, which records the same device. "default" is the one
/// kept as it follows the sound to whichever device is the default when the recorder next starts.
fn device_sources<'a>(devices: &'a BTreeMap<String, DeviceSel>, n: usize, defaults: &devices::Defaults) -> Vec<(&'a str, &'a DeviceSel, u32)> {
    let mixers = |d: &DeviceSel| -> u32 { (0..n).filter(|&i| d.tracks[i]).map(|i| 1 << i).sum() };
    let mut out: Vec<(&str, &DeviceSel, u32)> = devices.iter().map(|(k, d)| (k.as_str(), d, mixers(d))).filter(|s| s.2 != 0).collect();
    for (kind, id) in [("out", &defaults.out), ("in", &defaults.mic)] {
        let Some(id) = id else { continue };
        let (default_key, same_key) = (format!("{kind}:default"), format!("{kind}:{id}"));
        let (Some(at), Some(same)) = (out.iter().position(|s| s.0 == default_key), out.iter().position(|s| s.0 == same_key)) else { continue };
        out[at].2 |= out[same].2;
        out.remove(same);
    }
    out
}

/// Writes the recorder's settings, profile and scene collection under `root`. Returns the
/// encoder line for the app log. `private_stamp` is our private OBS's version stamp, if known;
/// `defaults` are Windows' default sound devices now; `mic_muted` starts the mics muted.
fn write_files(cfg: &Config, root: &Path, mon: &Monitor, main_root: &Path, private_stamp: Option<u64>, defaults: &devices::Defaults, mic_muted: bool) -> anyhow::Result<String> {
    let profile = root.join(r"basic\profiles").join(NAME);
    fs::create_dir_all(&profile)?;
    fs::create_dir_all(root.join(r"basic\scenes"))?;
    fs::create_dir_all(root.join(r"plugin_config\obs-websocket"))?;
    for (file, collection_file) in [("global.ini", NAME.to_string()), ("user.ini", format!("{NAME}.json"))] {
        let versions = match private_stamp {
            Some(s) => format!("LastVersion={s}\n"),
            None => version_lines(&main_root.join(file)),
        };
        let ini = format!(
            "[General]\nFirstRun=true\nConfirmOnExit=false\nEnableAutoUpdates=false\n{versions}\n\
             [BasicWindow]\nSysTrayEnabled=true\nSysTrayWhenStarted=true\n\n\
             [Basic]\nProfile={NAME}\nProfileDir={NAME}\nSceneCollection={NAME}\nSceneCollectionFile={collection_file}\n",
        );
        fs::write(root.join(file), ini)?;
    }

    // obs-websocket 5 (OBS 30 and 31) has no setting to listen on 127.0.0.1 only: its config.json
    // holds only these keys, and its server always binds every interface (the one address option
    // is --websocket_ipv4_only, which limits IPv4 vs IPv6, not the interface). So the random
    // password and the inbound firewall rule (obs_install::add_firewall_rule) keep others out.
    let ws = json!({
        "alerts_enabled": false, "auth_required": true, "first_load": false,
        "server_enabled": true, "server_port": PORT, "server_password": cfg.recorder_password,
    });
    fs::write(root.join(r"plugin_config\obs-websocket\config.json"), ws.to_string())?;

    let Plan { base_w, base_h, out_w, out_h, fps, kbps, mb, fits_secs, .. } = plan(cfg, mon);
    // A length past what fits in the memory cap (settings saved before the cap, or another PC's) is
    // kept as long as fits, so the replay time OBS is told matches what it can hold.
    let secs = fits_secs.map_or(cfg.buffer_secs, |s| s.min(cfg.buffer_secs));
    let (encoder, enc_settings) = encoder_settings(gpu(), cfg, kbps);
    let summary = format!("Encoder: {} ({encoder}), {out_w}x{out_h} at {fps} fps, {kbps} kbps.", gpu().label());
    let mask = track_mask(cfg.track_count);
    // OBS won't start the replay buffer if the clip folder is missing ("bad output path").
    fs::create_dir_all(&cfg.folder)?;
    let folder = cfg.folder.replace('\\', "/");
    let (name_format, _) = NAME_STYLES[(cfg.name_style as usize).min(NAME_STYLES.len() - 1)];
    // HDR monitor: clips stay normal (SDR) video in Rec. 709, and OBS's display capture tone-maps
    // the HDR picture down. OBS's SDR white level set to Windows' own SDR brightness keeps the
    // desktop and SDR games as bright as on screen, not washed out or dim.
    let hdr = hdr_white(cfg, mon)
        .map(|nits| format!("ColorFormat=NV12\nColorSpace=709\nColorRange=Partial\nSdrWhiteLevel={}\n", nits.clamp(80, 480)))
        .unwrap_or_default();
    let rec_format = rec_format(private_stamp);
    let basic = format!(
        "[General]\nName={NAME}\n\n[Output]\nMode=Advanced\nFilenameFormatting={name_format}\n\n\
         [AdvOut]\nRecType=Standard\nRecFilePath={folder}\nRecFormat2={rec_format}\nRecEncoder={encoder}\nRecRBPrefix=\n\
         RecTracks={mask}\nRecRB=true\nRecRBTime={}\nRecRBSize={mb}\nRecAudioEncoder=ffmpeg_aac\n\
         Track1Bitrate=192\nTrack2Bitrate=192\nTrack3Bitrate=192\nTrack4Bitrate=192\nTrack5Bitrate=192\nTrack6Bitrate=192\n\n\
         [Video]\nBaseCX={base_w}\nBaseCY={base_h}\nOutputCX={out_w}\nOutputCY={out_h}\nFPSType=1\nFPSInt={fps}\n{hdr}\n\
         [Audio]\nSampleRate=48000\nChannelSetup=Stereo\n",
        secs
    );
    // OBS itself listens for the save hotkey, so this app never watches the keyboard for it.
    let binding = hotkey::Combo::parse(&cfg.hotkey).and_then(|c| c.obs_binding());
    let basic = match binding {
        Some(b) => format!("{basic}\n[Hotkeys]\nReplayBuffer={b}\n"),
        None => basic,
    };
    fs::write(profile.join("basic.ini"), basic)?;
    fs::write(profile.join("recordEncoder.json"), enc_settings.to_string())?;

    // Scene: one full-canvas display capture plus one WASAPI source per selected device.
    let mut sources = vec![json!({
        "name": "Screen", "id": "monitor_capture", "versioned_id": "monitor_capture",
        "settings": { "monitor_id": mon.id, "capture_cursor": true },
    })];
    let mut items = vec![fit_to_canvas("Screen", base_w, base_h)];
    // Game capture mode: OBS hooks into whatever fullscreen game runs and draws it over the
    // screen capture, which still shows whenever no game is hooked. Off unless the user picks it.
    if cfg.capture == "game" {
        sources.push(json!({
            "name": "Game", "id": "game_capture", "versioned_id": "game_capture",
            "settings": { "capture_mode": "any_fullscreen", "capture_cursor": true, "anti_cheat_hook": true },
        }));
        items.push(fit_to_canvas("Game", base_w, base_h));
    }
    let n = cfg.tracks();
    let unique =|items: &[serde_json::Value], mut name: String| {
        while items.iter().any(|i| i["name"] == name.as_str()) {
            name.push('\''); // OBS source names must be unique
        }
        name
    };
    // With "Only these apps", the playback devices only say which tracks the apps go on.
    let mut app_mixers = 0;
    for (key, dev, mixers) in device_sources(&cfg.devices, n, defaults) {
        let (kind, id) = match key.split_once(':') {
            Some(("in", id)) => ("wasapi_input_capture", id),
            Some(_) if cfg.only_apps => {
                app_mixers |= mixers;
                continue;
            }
            Some((_, id)) => ("wasapi_output_capture", id),
            None => continue,
        };
        let name = unique(&items, dev.name.clone());
        // Mics get OBS's "Downmix to mono" (flag 2, OBS_SOURCE_FLAG_FORCE_MONO): a voice is one
        // channel, and audio interfaces often put it on the left only. Playback stays stereo; a
        // clip that comes out louder on one side is evened after saving (obs::center_clip).
        let mono = kind == "wasapi_input_capture";
        let mut source = json!({
            "name": name, "id": kind, "versioned_id": kind, "mixers": mixers,
            "settings": { "device_id": id },
            "flags": if mono { 2 } else { 0 },
        });
        if kind == "wasapi_input_capture" && cfg.noise != "off" {
            source["filters"] = json!([{
                "name": "Noise Suppression", "id": "noise_suppress_filter", "versioned_id": "noise_suppress_filter_v2",
                "enabled": true, "settings": { "method": cfg.noise },
            }]);
        }
        // OBS loads a source muted when its saved state says so (obs::set_mics mutes them all alike).
        if mono && mic_muted {
            source["muted"] = json!(true);
        }
        sources.push(source);
        items.push(json!({ "name": name, "visible": true }));
    }
    // OBS's application audio capture (Windows 10 2004+): the sound of one program, matched by
    // its exe name (priority 2) so a changing window title still matches. Nothing is injected.
    if cfg.only_apps {
        for app in &cfg.apps {
            let name = unique(&items, app.exe.trim_end_matches(".exe").to_string());
            sources.push(json!({
                "name": name, "id": "wasapi_process_output_capture", "versioned_id": "wasapi_process_output_capture",
                "mixers": if app_mixers == 0 { 1 } else { app_mixers },
                "settings": { "window": crate::devices::obs_window(app), "priority": 2 },
            }));
            items.push(json!({ "name": name, "visible": true }));
        }
    }
    // Voice chat on the tracks picked for it: Discord's sound alone, matched by its exe like the
    // apps above.
    let chat: u32 = (0..n).filter(|&i| cfg.chat_tracks[i]).map(|i| 1 << i).sum();
    if chat != 0 {
        let name = unique(&items, "Voice chat".to_string());
        sources.push(json!({
            "name": name, "id": "wasapi_process_output_capture", "versioned_id": "wasapi_process_output_capture",
            "mixers": chat, "settings": { "window": crate::devices::obs_window(&crate::devices::chat_app()), "priority": 2 },
        }));
        items.push(json!({ "name": name, "visible": true }));
    }
    sources.insert(0, json!({
        "name": "Replay", "id": "scene", "versioned_id": "scene", "settings": { "items": items },
    }));
    let collection = json!({
        "name": NAME, "current_scene": "Replay", "current_program_scene": "Replay",
        "scene_order": [{ "name": "Replay" }], "sources": sources,
    });
    fs::write(root.join(r"basic\scenes").join(format!("{NAME}.json")), collection.to_string())?;
    Ok(summary)
}

/// The hidden OBS's newest log file (OBS writes one per run).
pub fn obs_log() -> Option<PathBuf> {
    let dir = portable_root(&obs_exe().ok()?).join("logs");
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "txt"))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

/// The last error-looking line in the hidden OBS's log, without its timestamp.
pub fn last_obs_error() -> Option<String> {
    let text = String::from_utf8_lossy(&fs::read(obs_log()?).ok()?).into_owned();
    text.lines()
        .rev()
        .find(|l| {
            let l = l.to_lowercase();
            l.contains("error") || l.contains("fail") || l.contains("stopped because")
        })
        .map(|l| l.split_once(": ").map_or(l, |(_, rest)| rest).trim().to_string())
}

fn pid_alive(pid: u32) -> bool {
    Command::new(windows_exe("tasklist.exe"))
        .args(["/FI", &format!("PID eq {pid}"), "/FI", "IMAGENAME eq obs64.exe", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("obs64.exe"))
        .unwrap_or(false)
}

fn saved_pid() -> Option<u32> {
    fs::read_to_string(pid_file()).ok()?.trim().parse().ok()
}

pub fn is_running() -> bool {
    saved_pid().is_some_and(pid_alive)
}

/// The private OBS folder (`obs-studio`) of every recorder this process started, so one started
/// before the user moved the private OBS in setup is still stopped.
static LAUNCHED_DIRS: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// The `obs-studio` folder that holds `<obs-studio>\bin\64bit\obs64.exe`, or None if the path is too short.
fn obs_dir_of(exe: &Path) -> Option<PathBuf> {
    exe.ancestors().nth(3).filter(|d| !d.as_os_str().is_empty()).map(Path::to_path_buf)
}

/// This copy's running hidden recorders, including strays left by a crash or an older build of
/// it. Matched by the launch flags only this app uses, so the user's own OBS is never picked up,
/// and by an exe inside this copy's own private OBS folder (`obs-studio`, not the whole install
/// folder, which could hold another copy's), so another copy's recorder never is. A process whose
/// exe path can't be read isn't matched. Empty if the query fails.
fn all_recorders() -> Vec<u32> {
    let script = "[Console]::OutputEncoding = [Text.Encoding]::UTF8; \
                  Get-CimInstance Win32_Process -Filter \"Name='obs64.exe'\" | Where-Object { \
                  $_.CommandLine -match '--portable' -and $_.CommandLine -match '--multi' -and \
                  $_.CommandLine -match '--startreplaybuffer' } | ForEach-Object { \"$($_.ProcessId)|$($_.ExecutablePath)\" }";
    let Ok(out) = Command::new(windows_exe("powershell.exe"))
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return Vec::new();
    };
    let mut roots = vec![obs_install::private_dir()];
    roots.extend(LAUNCHED_DIRS.lock().unwrap().iter().cloned());
    parse_recorders(&String::from_utf8_lossy(&out.stdout))
        .into_iter()
        .filter(|(_, exe)| roots.iter().any(|r| is_ours(exe, r)))
        .map(|(pid, _)| pid)
        .collect()
}

/// The "pid|exe path" lines `all_recorders` asks PowerShell for. Lines without a number before
/// the bar are skipped; the path may be empty (Windows wouldn't say).
fn parse_recorders(out: &str) -> Vec<(u32, String)> {
    out.lines()
        .filter_map(|l| {
            let (pid, exe) = l.trim().split_once('|')?;
            Some((pid.trim().parse().ok()?, exe.trim().to_string()))
        })
        .collect()
}

/// `exe` is inside `root`, by its path as written or, where both exist, by their real paths (so
/// an 8.3 short name or another spelling of the same folder still counts).
fn is_ours(exe: &str, root: &Path) -> bool {
    if exe.is_empty() {
        return false;
    }
    let real = |p: &Path| fs::canonicalize(p).ok().map(|p| p.to_string_lossy().into_owned());
    under_root(exe, &root.to_string_lossy())
        || real(Path::new(exe)).zip(real(root)).is_some_and(|(e, r)| under_root(&e, &r))
}

/// `exe` is a path inside the folder `root`: ignoring case, `/` or `\`, a trailing slash and the
/// `\\?\` prefix, and only on a whole folder name (`C:\obs` doesn't hold `C:\obs2\obs64.exe`).
fn under_root(exe: &str, root: &str) -> bool {
    fn norm(p: &str) -> String {
        let p = p.trim().replace('/', "\\");
        let p = match p.strip_prefix(r"\\?\UNC\") {
            Some(rest) => format!(r"\\{rest}"),
            None => p.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(p),
        };
        p.trim_end_matches('\\').to_lowercase()
    }
    let (exe, root) = (norm(exe), norm(root));
    !root.is_empty() && exe.strip_prefix(&root).is_some_and(|rest| rest.starts_with('\\'))
}

/// The running recorder's WebSocket password, so `stop` can reach it to finish a recording.
static PASSWORD: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// Stops a full recording if one is running and waits (up to 10 s) for OBS to finish writing the
/// file, so the recorder can then be killed without leaving an MP4 that won't play. Returns the
/// recording's path if there was one.
pub async fn finish_recording(c: &obws::Client) -> anyhow::Result<Option<String>> {
    if !c.recording().status().await?.active {
        return Ok(None);
    }
    let path = c.recording().stop().await?;
    recording_stopped(&path);
    // OBS answers before it has closed the file; it's done once the recording shows as stopped.
    for _ in 0..40 {
        if !c.recording().status().await.is_ok_and(|s| s.active) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    Ok(Some(path))
}

/// `finish_recording` for `stop`, on a thread of its own (stop is also called from inside the
/// recorder thread's runtime). Gives up after 15 s, so a recorder that hangs can't hold up quitting.
fn finish_recording_now() {
    use std::time::Duration;
    let password = PASSWORD.lock().unwrap().clone();
    if password.is_empty() {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
        let path = rt.block_on(async {
            let c = tokio::time::timeout(Duration::from_secs(3), obws::Client::connect("127.0.0.1", PORT, Some(password.as_str()))).await.ok()?.ok()?;
            tokio::time::timeout(Duration::from_secs(12), finish_recording(&c)).await.ok()?.ok().flatten()
        });
        let _ = tx.send(path);
    });
    if let Ok(Some(path)) = rx.recv_timeout(Duration::from_secs(15)) {
        log::write(&format!("Stopped the full recording before closing the recorder: {path}"));
    }
}

/// The clips folder the running recorder saves into (`RecFilePath`); replays saved during a full
/// recording land in its Recordings folder.
static OUTPUT: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);
/// When the app last asked the recorder to save a replay that hasn't landed yet.
static SAVE_ASKED: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
/// The full recording the recorder last stopped, and when: OBS can still be writing its end for a
/// moment after it says the recording stopped.
static LAST_RECORDING: std::sync::Mutex<Option<(PathBuf, std::time::Instant)>> = std::sync::Mutex::new(None);

/// A save asked this long ago that never landed has failed (obs.rs says so after 10 s).
const SAVE_PENDING_FOR: std::time::Duration = std::time::Duration::from_secs(30);
/// Videos created or changed this recently may still be being written. A replay save writes for
/// seconds, a long one on a slow disk for a minute or two.
const RECENT: std::time::Duration = std::time::Duration::from_secs(300);
/// How long `stop` waits for a save or a recording's end to be written before it closes the
/// recorder anyway. Long enough for a few minutes of replay (1 to 3 GB) to land on an SSD, short
/// enough that quitting never feels stuck, and well inside what Windows allows at shutdown.
pub const SAVE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// The app asked the recorder to save a replay (the Save button, or a noticed save key).
pub fn save_asked() {
    *SAVE_ASKED.lock().unwrap() = Some(std::time::Instant::now());
}

/// The recorder reported a saved replay, or the save request failed: nothing is on its way.
pub fn save_done() {
    *SAVE_ASKED.lock().unwrap() = None;
}

/// A full recording was just stopped; `wait_for_writes` makes sure its file is closed.
pub fn recording_stopped(path: &str) {
    *LAST_RECORDING.lock().unwrap() = Some((PathBuf::from(path), std::time::Instant::now()));
}

/// Waits (up to `cap`) until nothing the recorder writes is still being written: no replay save the
/// app asked for is still on its way, and every recent video in the clips folder and its Recordings
/// folder, and the full recording last stopped, is closed by its writer and the same size twice in
/// a row. Covers saves from OBS's own save key, which the app may not know about. Only files from
/// the last few minutes are looked at, so a big clips folder costs one folder listing per look.
/// Returns false, and logs it, if `cap` ran out first. Call before closing the recorder.
pub fn wait_for_writes(cap: std::time::Duration) -> bool {
    let started = std::time::Instant::now();
    let dirs: Vec<PathBuf> = OUTPUT.lock().unwrap().iter().flat_map(|f| [f.clone(), f.join("Recordings")]).collect();
    let files: Vec<PathBuf> = LAST_RECORDING.lock().unwrap().iter().filter(|(_, at)| at.elapsed() < RECENT).map(|(p, _)| p.clone()).collect();
    let pending = || SAVE_ASKED.lock().unwrap().is_some_and(|at| at.elapsed() < SAVE_PENDING_FOR);
    let done = wait_until_written(&dirs, &files, pending, cap);
    let waited = started.elapsed().as_secs_f32();
    if !done {
        log::write(&format!("A clip or recording was still being written after {waited:.0} s; closing the recorder anyway."));
    } else if waited >= 0.5 {
        log::write(&format!("Waited {waited:.1} s for a clip or recording to finish writing before closing the recorder."));
    }
    done
}

/// `wait_for_writes` on given folders and files, with `pending` saying whether a save the app asked
/// for is still on its way. Looks every 250 ms.
fn wait_until_written(dirs: &[PathBuf], files: &[PathBuf], pending: impl Fn() -> bool, cap: std::time::Duration) -> bool {
    let started = std::time::Instant::now();
    let mut before: Option<Vec<(PathBuf, u64)>> = None;
    loop {
        let since = std::time::SystemTime::now().checked_sub(RECENT).unwrap_or(std::time::UNIX_EPOCH);
        let mut paths: Vec<PathBuf> = dirs.iter().flat_map(|d| recent_videos(d, since)).collect();
        paths.extend(files.iter().cloned());
        paths.sort();
        paths.dedup();
        // Sizes from the file itself: the folder listing's can lag while it's open.
        let now: Vec<(PathBuf, u64)> = paths.into_iter().filter_map(|p| fs::metadata(&p).ok().map(|m| (p, m.len()))).collect();
        // Nothing recent: no second look needed.
        let settled = now.is_empty() || before.as_ref().is_some_and(|b| !still_writing(b, &now, held_for_writing));
        if settled && !pending() {
            return true;
        }
        if started.elapsed() >= cap {
            return false;
        }
        before = Some(now);
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

/// Videos directly in `dir` created or changed at or after `since`, by the folder listing alone
/// (no file is opened). The listing's times can lag for a file still open, so creation counts too.
fn recent_videos(dir: &Path, since: std::time::SystemTime) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| ["mp4", "mkv", "mov"].iter().any(|v| x.eq_ignore_ascii_case(v))))
        .filter(|e| e.metadata().is_ok_and(|m| m.is_file() && [m.created(), m.modified()].into_iter().flatten().any(|t| t >= since)))
        .map(|e| e.path())
        .collect()
}

/// Whether a file may still be being written, from two looks at the recent videos (`before`, then
/// `now`, each a path and its size): one is new since the last look, changed size, or `held` says
/// a writer still has it open.
fn still_writing(before: &[(PathBuf, u64)], now: &[(PathBuf, u64)], held: impl Fn(&Path) -> bool) -> bool {
    now.iter().any(|(p, len)| !before.iter().any(|(b, l)| b == p && l == len) || held(p))
}

/// Whether another program has `path` open for writing. OBS's writers (ffmpeg's muxer and the
/// hybrid MP4 writer) let others read and even write, so a plain open always works; asking to be
/// the only writer fails while any writer has it open. Readers (Explorer, thumbnails) don't count.
/// The handle is closed at once; a missing file or any other error counts as not held.
pub fn held_for_writing(path: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;
    const FILE_SHARE_DELETE: u32 = 4;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    match fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE).open(path) {
        Ok(_) => false,
        Err(e) => e.raw_os_error() == Some(ERROR_SHARING_VIOLATION),
    }
}

/// Stops this copy's hidden recorder(s), after finishing a full recording if one is running and
/// waiting (up to `SAVE_WAIT`) for a replay save or the recording's end to be written. Safe to
/// kill then: OBS skips its crash check for `--multi` copies.
/// Only processes launched with our flags from this copy's own private OBS folder are killed
/// (`all_recorders`): a saved PID alone could by now belong to the user's own OBS (Windows reuses
/// PIDs), and another copy of the app (a portable one) has its own recorder that isn't ours to stop.
pub fn stop() {
    finish_recording_now();
    wait_for_writes(SAVE_WAIT);
    for pid in all_recorders() {
        let _ = Command::new(windows_exe("taskkill.exe"))
            .args(["/PID", &pid.to_string(), "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
    PID.store(0, std::sync::atomic::Ordering::Relaxed);
    let _ = fs::remove_file(pid_file());
}

/// (Re)writes the recorder config and starts the hidden OBS with the replay buffer running.
pub fn restart(cfg: &Config) -> anyhow::Result<()> {
    stop();
    write_config(cfg)?;
    let exe = obs_exe()?;
    if let Err(e) = crate::tweaks::set_gpu_high(&exe, cfg.gpu_high) {
        log::write(&format!("Couldn't set the recorder's graphics preference: {e:#}"));
    }
    *PASSWORD.lock().unwrap() = cfg.recorder_password.clone();
    *OUTPUT.lock().unwrap() = Some(PathBuf::from(&cfg.folder));
    let front = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
    let child = Command::new(&exe)
        .args(["--portable", "--multi", "--minimize-to-tray", "--startreplaybuffer", "--disable-updater"])
        .current_dir(exe.parent().unwrap())
        // Always as a normal user, even if a compatibility setting asks for admin, so the app can stop it.
        .env("__COMPAT_LAYER", "RunAsInvoker")
        .spawn()?;
    if let Some(dir) = obs_dir_of(&exe) {
        let mut launched = LAUNCHED_DIRS.lock().unwrap();
        if !launched.contains(&dir) {
            launched.push(dir);
        }
    }
    let _ = tie_to_app(&child);
    keep_focus(front, child.id());
    PID.store(child.id(), std::sync::atomic::Ordering::Relaxed);
    fs::create_dir_all(config::dir())?;
    fs::write(pid_file(), child.id().to_string())?;
    Ok(())
}

/// OBS's hidden window takes the foreground as it starts when this app had it (after Save on a
/// settings page), so typing went nowhere until something was clicked. For 10 s, if the recorder
/// holds the foreground, it goes back to `front`. Anything else the user switches to is left alone.
fn keep_focus(front: windows::Win32::Foundation::HWND, pid: u32) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow};
    if front.is_invalid() {
        return;
    }
    // HWND isn't Send; the number is.
    let front = front.0 as isize;
    std::thread::spawn(move || {
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let mut owner = 0;
            unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut owner)) };
            if owner == pid {
                let _ = unsafe { SetForegroundWindow(HWND(front as *mut _)) };
                return;
            }
        }
    });
}

/// Puts the recorder in a job that Windows closes when this app's process ends, however it ends
/// (crash, Task Manager), so a hidden OBS never keeps recording with nothing to stop it.
fn tie_to_app(child: &std::process::Child) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    // One job for the app's lifetime; its handle is never closed, so it closes when we exit.
    static JOB: std::sync::OnceLock<Option<isize>> = std::sync::OnceLock::new();
    let job = JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(None, None).ok()?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .ok()?;
        Some(job.0 as isize)
    });
    let ok = job.is_some_and(|j| unsafe {
        AssignProcessToJobObject(HANDLE(j as *mut _), HANDLE(child.as_raw_handle())).is_ok()
    });
    if !ok {
        log::write("Couldn't tie the recorder to the app; if the app crashes, the next start stops it.");
    }
    ok
}

/// The next lighter Encoder effort (0 best quality, 1 balanced, 2 max FPS), or None at the lightest.
pub fn lighter_effort(effort: u8) -> Option<u8> {
    (effort < 2).then_some(effort + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppSel, DeviceSel};
    use std::time::{Duration, Instant};

    /// A trimmed OBS (`obs_install::prune`) really records: a copy of OBS at `RV_OBS_COPY` (its
    /// obs-studio folder, a throwaway copy) is trimmed, started on port 4477 with this app's settings, saves a clip
    /// from the replay buffer, and its log shows no module that failed to load. Runs beside the
    /// user's own recorder without touching it. By hand:
    /// `$env:RV_OBS_COPY='...'; cargo test --release live_pruned_obs -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_pruned_obs() {
        let copy = PathBuf::from(std::env::var("RV_OBS_COPY").expect("RV_OBS_COPY"));
        obs_install::prune(&copy);
        assert!(!copy.join(r"bin\64bit\obs64.pdb").exists() && copy.join(r"core\obs-browser").exists());
        let clips = std::env::temp_dir().join("replay-vault-pruned-obs-clips");
        let _ = fs::remove_dir_all(&clips);
        let cfg = Config { folder: clips.to_string_lossy().into(), recorder_password: "pruned-test".into(), buffer_secs: 20, ..Config::default() };
        let root = copy.join(r"config\obs-studio");
        let _ = fs::remove_dir_all(root.join("logs"));
        let mon = chosen_monitor(&cfg).expect("a monitor");
        println!("{}", write_files(&cfg, &root, &mon, &copy.join("no-main-obs"), obs_install::last_version_stamp("33.0.1"), &devices::defaults(), false).unwrap());
        let ws = root.join(r"plugin_config\obs-websocket\config.json");
        let mut json: serde_json::Value = serde_json::from_str(&fs::read_to_string(&ws).unwrap()).unwrap();
        json["server_port"] = serde_json::json!(4477);
        fs::write(&ws, json.to_string()).unwrap();
        let bin = copy.join(r"bin\64bit");
        let mut obs = Command::new(bin.join("obs64.exe"))
            .args(["--portable", "--multi", "--minimize-to-tray", "--startreplaybuffer", "--disable-updater"])
            .current_dir(&bin)
            .spawn()
            .unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let saved = rt.block_on(async {
            let mut client = None;
            for _ in 0..40 {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if let Ok(c) = obws::Client::connect("127.0.0.1", 4477, Some("pruned-test")).await {
                    client = Some(c);
                    break;
                }
            }
            // Killed below either way, so a failed run leaves no OBS (or its popups) behind.
            let Some(c) = client else { return Err("the trimmed OBS never answered".to_string()) };
            for _ in 0..20 {
                if c.replay_buffer().status().await.unwrap_or(false) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
            c.replay_buffer().save().await.map_err(|e| e.to_string())
        });
        std::thread::sleep(Duration::from_secs(4));
        let _ = obs.kill();
        let _ = obs.wait();
        saved.expect("replay buffer saved");
        let clip = fs::read_dir(&clips).unwrap().flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "mp4")).expect("a clip");
        let size = fs::metadata(&clip).unwrap().len();
        println!("clip: {} ({} KB)", clip.display(), size / 1024);
        assert!(size > 100_000);
        let log = fs::read_dir(root.join("logs")).unwrap().flatten().map(|e| fs::read_to_string(e.path()).unwrap_or_default()).collect::<String>();
        // A plugin that didn't load (a missing translation or NVIDIA's optional denoiser is fine).
        let bad: Vec<&str> = log.lines().filter(|l| l.contains("Failed to load core module") || l.contains("plugin_not_loaded") || l.contains("Failed to load module")).collect();
        println!("log problems: {bad:#?}");
        assert!(bad.is_empty());
        fs::remove_dir_all(&clips).unwrap();
    }

    fn test_monitor() -> Monitor {
        Monitor { id: r"\\?\DISPLAY#TEST#1".into(), width: 2560, height: 1440, hz: 144, x: 0, y: 0, primary: true, hdr: None, name: String::new() }
    }

    /// Writes the recorder files into a fresh temp folder (never the real OBS config) and
    /// returns basic.ini and the scene collection.
    fn written(tag: &str, cfg: &mut Config, mon: &Monitor) -> (String, serde_json::Value) {
        written_for(tag, cfg, mon, None)
    }

    /// `written` for a private OBS of version `obs` ("33.0.1"), or of an unknown version.
    fn written_for(tag: &str, cfg: &mut Config, mon: &Monitor, obs: Option<&str>) -> (String, serde_json::Value) {
        written_with(tag, cfg, mon, obs, &devices::Defaults::default(), false)
    }

    /// `written_for` with Windows' default sound devices as `defaults`.
    fn written_with(tag: &str, cfg: &mut Config, mon: &Monitor, obs: Option<&str>, defaults: &devices::Defaults, mic_muted: bool) -> (String, serde_json::Value) {
        let root = std::env::temp_dir().join(format!("replay-vault-recorder-test-{tag}"));
        let _ = fs::remove_dir_all(&root);
        cfg.folder = root.join("clips").display().to_string();
        write_files(cfg, &root, mon, &root.join("main"), obs.and_then(obs_install::last_version_stamp), defaults, mic_muted).unwrap();
        let basic = fs::read_to_string(root.join(r"basic\profiles").join(NAME).join("basic.ini")).unwrap();
        let scene = fs::read_to_string(root.join(r"basic\scenes").join(format!("{NAME}.json"))).unwrap();
        let _ = fs::remove_dir_all(&root);
        (basic, serde_json::from_str(&scene).unwrap())
    }

    /// An HDR monitor gets SDR Rec. 709 output with Windows' SDR brightness as OBS's white level;
    /// an SDR one keeps OBS's own color defaults.
    #[test]
    fn hdr_monitor_gets_sdr_colors() {
        let mut cfg = Config::default();
        let (basic, _) = written("sdr", &mut cfg, &test_monitor());
        assert!(!basic.contains("SdrWhiteLevel") && !basic.contains("ColorSpace"));
        let hdr = Monitor { hdr: Some(240), ..test_monitor() };
        let (basic, _) = written("hdr", &mut cfg, &hdr);
        let video = basic.split("[Video]").nth(1).unwrap().split("\n[").next().unwrap();
        for line in ["ColorFormat=NV12", "ColorSpace=709", "ColorRange=Partial", "SdrWhiteLevel=240", "BaseCX=2560"] {
            assert!(video.lines().any(|l| l == line), "{line} missing from [Video]:\n{video}");
        }
        assert!(basic.contains("\n\n[Audio]\n"));
        // Out of OBS's range: clamped.
        let (basic, _) = written("hdr-bright", &mut cfg, &Monitor { hdr: Some(1000), ..test_monitor() });
        assert!(basic.contains("SdrWhiteLevel=480\n"));
    }

    /// The screen, and in game mode the game, are always fitted into the canvas (scale to inner
    /// bounds, centred, from the canvas's corner), following the mouse or not: a monitor or game of
    /// another size or shape gets bars, never a crop, a stretch or an empty corner.
    #[test]
    fn captures_fit_the_canvas() {
        let fitted = |item: &serde_json::Value, name: &str| {
            assert_eq!(item["name"], name);
            assert_eq!((item["bounds_type"].as_i64(), item["bounds_align"].as_i64(), item["align"].as_i64()), (Some(2), Some(0), Some(5)), "{item}");
            assert_eq!((item["bounds"]["x"].as_u64(), item["bounds"]["y"].as_u64()), (Some(2560), Some(1440)));
            assert_eq!((item["pos"]["x"].as_i64(), item["pos"]["y"].as_i64(), item["scale"]["x"].as_f64()), (Some(0), Some(0), Some(1.0)));
        };
        for (tag, follow, capture) in [("fit", false, "desktop"), ("fit-follow", true, "desktop"), ("fit-game", false, "game"), ("fit-follow-game", true, "game")] {
            let mut cfg = Config { follow_mouse: follow, capture: capture.into(), ..Config::default() };
            let (_, scene) = written(tag, &mut cfg, &test_monitor());
            let items = scene["sources"][0]["settings"]["items"].as_array().unwrap().clone();
            fitted(&items[0], "Screen");
            if capture == "game" {
                fitted(&items[1], "Game");
            } else {
                assert!(!items.iter().any(|i| i["name"] == "Game"), "{tag}");
            }
        }
    }

    /// Only what the recorder takes from the monitors counts as a change: another monitor's size
    /// or name doesn't, the recorded one's size, its going away, or its HDR (any monitor's, when
    /// following the mouse) does; its refresh rate only when the clips use it.
    #[test]
    fn display_changes_that_matter() {
        let main = test_monitor();
        let side = Monitor { id: "side".into(), primary: false, x: 2560, width: 1920, height: 1080, ..test_monitor() };
        let cfg = Config::default();
        let key = |cfg: &Config, all: &[Monitor]| display_key(cfg, all);
        let before = key(&cfg, &[main.clone(), side.clone()]);
        assert!(before.is_some());
        assert_eq!(key(&cfg, &[main.clone(), Monitor { width: 1280, height: 720, name: "TV".into(), ..side.clone() }]), before);
        assert_eq!(key(&cfg, std::slice::from_ref(&main)), before, "another monitor unplugged");
        assert_ne!(key(&cfg, &[Monitor { width: 1920, height: 1080, ..main.clone() }, side.clone()]), before);
        assert_ne!(key(&cfg, &[Monitor { hdr: Some(240), ..main.clone() }, side.clone()]), before);
        assert_ne!(key(&cfg, &[Monitor { primary: true, ..side.clone() }]), before, "the recorded monitor unplugged");
        assert_eq!(key(&cfg, &[]), None);
        // Refresh rate: only when the clip frame rate follows it.
        assert_eq!(key(&cfg, &[Monitor { hz: 60, ..main.clone() }]), key(&cfg, std::slice::from_ref(&main)));
        let native = Config { fps: 0, ..Config::default() };
        assert_ne!(key(&native, &[Monitor { hz: 60, ..main.clone() }]), key(&native, std::slice::from_ref(&main)));
        // HDR on another monitor: only when the capture may land on it.
        let hdr_side = [main.clone(), Monitor { hdr: Some(200), ..side.clone() }];
        assert_eq!(key(&cfg, &hdr_side), before);
        let follow = Config { follow_mouse: true, ..Config::default() };
        assert_ne!(key(&follow, &hdr_side), key(&follow, &[main, side]));
    }

    /// Full recordings are hybrid MP4 on OBS 30.2 and later, plain MP4 on older or unknown OBS; the
    /// rest of the recording and replay settings are the same either way.
    #[test]
    fn full_recordings_crash_safe_where_obs_can() {
        let mut cfg = Config::default();
        // [AdvOut] without the clips folder, which each test run has its own of.
        let adv_out = |basic: &str| basic.split("[AdvOut]\n").nth(1).unwrap().split("\n\n").next().unwrap().lines().filter(|l| !l.starts_with("RecFilePath=")).collect::<Vec<_>>().join("\n");
        let format_line = |basic: &str| adv_out(basic).lines().filter(|l| l.starts_with("RecFormat")).map(str::to_string).collect::<Vec<_>>();
        let (hybrid, _) = written_for("hybrid", &mut cfg, &test_monitor(), Some("33.0.1"));
        assert_eq!(format_line(&hybrid), ["RecFormat2=hybrid_mp4"]);
        let (first, _) = written_for("hybrid-30.2", &mut cfg, &test_monitor(), Some("30.2.0"));
        assert_eq!(format_line(&first), ["RecFormat2=hybrid_mp4"]);
        for (tag, obs) in [("old", Some("30.1.2")), ("older", Some("29.1.3")), ("unknown", None), ("junk", Some("junk"))] {
            let (plain, _) = written_for(tag, &mut cfg, &test_monitor(), obs);
            assert_eq!(format_line(&plain), ["RecFormat2=mp4"], "{obs:?}");
            assert_eq!(adv_out(&plain).replace("RecFormat2=mp4", "RecFormat2=hybrid_mp4"), adv_out(&hybrid), "only the format differs");
            // No muxer flags: OBS would give them to the replay buffer too.
            assert!(!plain.contains("RecMuxerCustom") && !plain.contains("movflags"));
        }
        assert!(!hybrid.contains("RecMuxerCustom") && !hybrid.contains("fragmented"));
    }

    #[test]
    fn space_estimates() {
        let cfg = Config { track_count: 1, buffer_secs: 60, ..Config::default() };
        assert_eq!(space_estimate(&cfg, 20_000), "About 8.5 GB per hour of clips (144.4 MB for a full 1-minute replay)");
        let cfg = Config { track_count: 3, buffer_secs: 90, ..Config::default() };
        assert_eq!(space_estimate(&cfg, 40_000), "About 17.0 GB per hour of clips (435.3 MB for a full 90-second replay)");
    }

    fn source<'a>(scene: &'a serde_json::Value, id: &str) -> Vec<&'a serde_json::Value> {
        scene["sources"].as_array().unwrap().iter().filter(|s| s["id"] == id).collect()
    }

    /// Desktop audio by default; with "Only these apps", one app capture per app on the
    /// playback device's tracks, and no desktop audio source.
    #[test]
    fn only_these_apps_replaces_desktop_audio() {
        let mut cfg = Config { track_count: 3, ..Config::default() };
        cfg.devices.insert("out:default".into(), DeviceSel { name: "Default playback device".into(), tracks: [true, true, false, false, false, false] });
        cfg.devices.insert("in:default".into(), DeviceSel { name: "Default microphone".into(), tracks: [true, false, true, false, false, false] });
        let (_, scene) = written("desktop", &mut cfg, &test_monitor());
        assert_eq!(source(&scene, "wasapi_output_capture").len(), 1);
        assert!(source(&scene, "wasapi_process_output_capture").is_empty());

        cfg.only_apps = true;
        cfg.apps = vec![
            AppSel { title: "VALORANT  ".into(), class: "UnrealWindow".into(), exe: "VALORANT-Win64-Shipping.exe".into() },
            AppSel { title: "#general: Discord".into(), class: "Chrome_WidgetWin_1".into(), exe: "Discord.exe".into() },
        ];
        let (_, scene) = written("apps", &mut cfg, &test_monitor());
        assert!(source(&scene, "wasapi_output_capture").is_empty());
        let apps = source(&scene, "wasapi_process_output_capture");
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[1]["name"], "Discord");
        assert_eq!(apps[1]["mixers"], 0b011);
        assert_eq!(apps[1]["settings"]["window"], "#22general#3A Discord:Chrome_WidgetWin_1:Discord.exe");
        assert_eq!(apps[1]["settings"]["priority"], 2);
        // The mic is still recorded, and every source is in the scene.
        assert_eq!(source(&scene, "wasapi_input_capture")[0]["mixers"], 0b101);
        let items = scene["sources"][0]["settings"]["items"].as_array().unwrap();
        assert!(items.iter().any(|i| i["name"] == "VALORANT-Win64-Shipping"));
    }

    /// A new config records the default mic too, in mono; playback stays stereo.
    #[test]
    fn default_mic_in_mono() {
        let mut cfg = Config::default();
        assert!(cfg.devices["in:default"].tracks[0]);
        let (_, scene) = written("mono", &mut cfg, &test_monitor());
        assert_eq!(source(&scene, "wasapi_output_capture")[0]["flags"], 0);
        assert_eq!(source(&scene, "wasapi_input_capture")[0]["flags"], 2);
    }

    /// A headset ticked while it's also Windows' default, with "Default playback device" ticked
    /// too, is recorded once, on both's tracks; the same for a mic. Another device isn't touched,
    /// nor one that isn't the default, nor anything when Windows didn't say.
    #[test]
    fn default_device_recorded_once() {
        let sel = |tracks: [bool; 6]| DeviceSel { name: String::new(), tracks };
        let t = |bits: u8| std::array::from_fn::<bool, 6, _>(|i| bits & 1 << i != 0);
        let mut devices = BTreeMap::new();
        devices.insert("out:default".to_string(), sel(t(0b001)));
        devices.insert("out:{headset}".to_string(), sel(t(0b010)));
        devices.insert("out:{speakers}".to_string(), sel(t(0b001)));
        devices.insert("out:{off}".to_string(), sel(t(0)));
        devices.insert("in:default".to_string(), sel(t(0b001)));
        devices.insert("in:{mic}".to_string(), sel(t(0b100)));
        let keys = |s: &[(&str, &DeviceSel, u32)]| s.iter().map(|(k, _, m)| (k.to_string(), *m)).collect::<Vec<_>>();
        let pairs = |p: &[(&str, u32)]| p.iter().map(|(k, m)| (k.to_string(), *m)).collect::<Vec<_>>();
        let none = devices::Defaults::default();
        assert_eq!(
            keys(&device_sources(&devices, 3, &none)),
            pairs(&[("in:default", 0b001), ("in:{mic}", 0b100), ("out:default", 0b001), ("out:{headset}", 0b010), ("out:{speakers}", 0b001)])
        );
        let both = devices::Defaults { out: Some("{headset}".into()), mic: Some("{mic}".into()) };
        assert_eq!(keys(&device_sources(&devices, 3, &both)), pairs(&[("in:default", 0b101), ("out:default", 0b011), ("out:{speakers}", 0b001)]));
        // Past the track count nothing is recorded, so nothing is folded.
        assert_eq!(keys(&device_sources(&devices, 1, &both)), pairs(&[("in:default", 0b001), ("out:default", 0b001), ("out:{speakers}", 0b001)]));
        // "default" not ticked: the device is recorded as itself.
        devices.get_mut("out:default").unwrap().tracks = t(0);
        assert_eq!(keys(&device_sources(&devices, 3, &both)), pairs(&[("in:default", 0b101), ("out:{headset}", 0b010), ("out:{speakers}", 0b001)]));

        // In the scene: one playback source and one mic, on the tracks of both.
        let mut cfg = Config { track_count: 3, devices: devices.clone(), ..Config::default() };
        cfg.devices.get_mut("out:default").unwrap().tracks = t(0b001);
        let (_, scene) = written_with("dedup", &mut cfg, &test_monitor(), None, &both, false);
        let outs = source(&scene, "wasapi_output_capture");
        assert_eq!(outs.iter().map(|s| (s["settings"]["device_id"].as_str().unwrap(), s["mixers"].as_u64().unwrap())).collect::<Vec<_>>(), [("default", 0b011), ("{speakers}", 0b001)]);
        let mics = source(&scene, "wasapi_input_capture");
        assert_eq!((mics.len(), mics[0]["settings"]["device_id"].as_str(), mics[0]["mixers"].as_u64()), (1, Some("default"), Some(0b101)));
    }

    /// A mic muted with its key is written muted, every mic alike, so a restarted recorder never
    /// records a moment of it; playback isn't touched, and unmuted nothing says muted.
    #[test]
    fn muted_mic_starts_muted() {
        let mut cfg = Config::default();
        cfg.devices.insert("in:{usb}".into(), DeviceSel { name: "USB mic".into(), tracks: [true, false, false, false, false, false] });
        let none = devices::Defaults::default();
        let muted = |scene: &serde_json::Value, id: &str| source(scene, id).iter().map(|s| s.get("muted").and_then(|m| m.as_bool())).collect::<Vec<_>>();
        let (_, scene) = written_with("unmuted", &mut cfg, &test_monitor(), None, &none, false);
        assert_eq!(muted(&scene, "wasapi_input_capture"), [None, None]);
        assert_eq!(muted(&scene, "wasapi_output_capture"), [None]);
        let (_, scene) = written_with("muted", &mut cfg, &test_monitor(), None, &none, true);
        assert_eq!(muted(&scene, "wasapi_input_capture"), [Some(true), Some(true)]);
        assert_eq!(muted(&scene, "wasapi_output_capture"), [None]);
    }

    /// Only tracks with a mic and other sound count as mixed.
    #[test]
    fn mic_mixed_tracks_found() {
        let mut cfg = Config { track_count: 3, ..Config::default() };
        cfg.devices.insert("out:default".into(), DeviceSel { name: "Speakers".into(), tracks: [true, true, false, false, false, false] });
        cfg.devices.insert("in:default".into(), DeviceSel { name: "Mic".into(), tracks: [true, false, true, false, true, false] });
        cfg.chat_tracks = [false; 6];
        assert_eq!(mic_mixed_tracks(&cfg), 0b001, "track 1 mixed, 2 game only, 3 mic only, 5 past the count");
        cfg.chat_tracks = [false, false, true, false, false, false];
        assert_eq!(mic_mixed_tracks(&cfg), 0b101, "voice chat on the mic's track");
        cfg.devices.remove("in:default");
        assert_eq!(mic_mixed_tracks(&cfg), 0);
    }

    /// RecTracks has one bit per track, 1 to 6, and a hand-edited count can't overflow it.
    #[test]
    fn track_bitmask() {
        assert_eq!([1, 2, 3, 4, 5, 6].map(track_mask), [0b1, 0b11, 0b111, 0b1111, 0b1_1111, 0b11_1111]);
        assert_eq!((track_mask(0), track_mask(200)), (0b1, 0b11_1111));
    }

    /// Six tracks: RecTracks covers them all with a bitrate each, each source's mixers are its
    /// ticked tracks, voice chat gets its own capture on its ticked tracks (track 1 too), and
    /// the memory estimate grows by a track's 192 kbps.
    #[test]
    fn six_tracks_and_voice_chat() {
        let mut cfg = Config { track_count: 6, ..Config::default() };
        cfg.devices.insert("out:default".into(), DeviceSel { name: "Default playback device".into(), tracks: [true, true, false, false, true, false] });
        cfg.devices.insert("in:default".into(), DeviceSel { name: "Default microphone".into(), tracks: [true, false, true, false, false, true] });
        cfg.chat_tracks = [true, false, false, true, false, false];
        let (basic, scene) = written("six", &mut cfg, &test_monitor());
        assert!(basic.contains("\nRecTracks=63\n"), "{basic}");
        assert!(basic.contains("\nTrack6Bitrate=192\n"));
        assert_eq!(source(&scene, "wasapi_output_capture")[0]["mixers"], 0b01_0011);
        assert_eq!(source(&scene, "wasapi_input_capture")[0]["mixers"], 0b10_0101);
        let chat = source(&scene, "wasapi_process_output_capture");
        assert_eq!((chat.len(), chat[0]["name"].as_str()), (1, Some("Voice chat")));
        assert_eq!(chat[0]["mixers"], 0b00_1001, "tracks 1 and 4");
        assert!(chat[0]["settings"]["window"].as_str().unwrap().ends_with(":Discord.exe"));
        // Ticks past the track count aren't recorded, and no chat track means no chat capture.
        cfg.track_count = 3;
        let (basic, scene) = written("three", &mut cfg, &test_monitor());
        assert!(basic.contains("\nRecTracks=7\n"));
        assert_eq!(source(&scene, "wasapi_input_capture")[0]["mixers"], 0b101);
        assert_eq!(source(&scene, "wasapi_process_output_capture")[0]["mixers"], 0b001, "track 4's tick is past the count");
        cfg.chat_tracks = [false, false, false, true, false, false];
        let (_, scene) = written("three-nochat", &mut cfg, &test_monitor());
        assert!(source(&scene, "wasapi_process_output_capture").is_empty());
        // A minute stays under the RAM cap even on a 4 GB PC.
        let mb = |t: u8| plan(&Config { track_count: t, buffer_secs: 60, ..Config::default() }, &test_monitor()).mb;
        assert!((1..=3).contains(&(mb(2) - mb(1))), "about 1.8 MB per track for a minute, with headroom: {}", mb(2) - mb(1));
        // 20 minutes at about 130 Mbps needs about 24 GB: capped to a quarter of the RAM, and it
        // says how long fits (OBS keeps the newest part).
        let big = plan(&Config { buffer_secs: 1200, quality: 0, ..Config::default() }, &test_monitor());
        if buffer_cap_mb() < 20_000 {
            let secs = big.fits_secs.expect("capped");
            assert_eq!(big.mb, buffer_cap_mb());
            let per_mb = (big.kbps + 192 * Config::default().track_count as u64) as f64 / 8000.0 * 1.25;
            assert!((secs as f64 * per_mb + 64.0 - big.mb as f64).abs() < 10.0, "{secs} s should fill {} MB", big.mb);
            // The length slider's top: fits whole, and 10 s more wouldn't.
            let top = Config { quality: 0, ..Config::default() };
            let max = max_secs(&top, &test_monitor());
            assert!(max < 1200 && max.is_multiple_of(10));
            assert!(plan(&Config { buffer_secs: max, ..top.clone() }, &test_monitor()).fits_secs.is_none());
            assert!(plan(&Config { buffer_secs: max + 10, ..top.clone() }, &test_monitor()).fits_secs.is_some());
        }
        // Light settings (720p30, the 8 Mbps floor) fit the full 20 minutes on 8 GB or more.
        if buffer_cap_mb() >= 2048 {
            let light = Config { quality: 3, out_height: 720, fps: 30, ..Config::default() };
            assert_eq!(max_secs(&light, &test_monitor()), 1200);
        }
    }

    /// NVENC gets its preset under both the current key and the old one, at every effort.
    #[test]
    fn nvenc_preset_under_both_keys() {
        for (effort, preset, aq) in [(0, "p5", None), (1, "p4", Some(false)), (2, "p1", Some(false))] {
            let cfg = Config { encoder_effort: effort, ..Config::default() };
            let (id, s) = encoder_settings(Gpu::Nvidia, &cfg, 20_000);
            assert_eq!(id, "obs_nvenc_h264_tex");
            assert_eq!((s["preset"].as_str(), s["preset2"].as_str()), (Some(preset), Some(preset)), "effort {effort}");
            assert_eq!((s["adaptive_quantization"].as_bool(), s["psycho_aq"].as_bool()), (aq, aq), "effort {effort}");
        }
        // The others keep their own keys.
        let (_, amd) = encoder_settings(Gpu::Amd, &Config::default(), 20_000);
        assert!(amd.get("preset2").is_none() && amd["preset"].is_string());
    }

    #[test]
    fn lighter_effort_steps_down_to_max_fps() {
        assert_eq!(lighter_effort(0), Some(1));
        assert_eq!(lighter_effort(1), Some(2));
        assert_eq!(lighter_effort(2), None);
        assert_eq!(lighter_effort(7), None);
    }

    const INSTALLED: &str = r"C:\Users\You\AppData\Local\replay-vault\obs-studio";
    const PORTABLE: &str = r"G:\Portable\Replay Vault\replay-vault-data\obs\obs-studio";

    /// Each copy matches only the recorder in its own private OBS folder.
    #[test]
    fn recorder_matched_only_in_its_own_folder() {
        let installed = format!(r"{INSTALLED}\bin\64bit\obs64.exe");
        let portable = format!(r"{PORTABLE}\bin\64bit\obs64.exe");
        assert!(under_root(&installed, INSTALLED) && under_root(&portable, PORTABLE));
        assert!(!under_root(&portable, INSTALLED) && !under_root(&installed, PORTABLE));
        // The user's own OBS.
        assert!(!under_root(r"C:\Program Files\obs-studio\bin\64bit\obs64.exe", INSTALLED));
    }

    /// A copy installed at a drive root (its OBS in G:\obs-studio) and a portable copy further
    /// down the same drive don't match each other's recorder.
    #[test]
    fn recorder_not_matched_in_a_nested_copy() {
        let (installed, installed_exe) = (r"G:\obs-studio", r"G:\obs-studio\bin\64bit\obs64.exe");
        let portable_exe = format!(r"{PORTABLE}\bin\64bit\obs64.exe");
        assert!(!under_root(&portable_exe, installed) && !under_root(installed_exe, PORTABLE));
        assert!(under_root(installed_exe, installed) && under_root(&portable_exe, PORTABLE));
    }

    /// The install folder itself (G:\) holds the portable copy's recorder, so only its obs-studio folder is matched.
    #[test]
    fn install_root_alone_would_match_another_copy() {
        let portable_exe = format!(r"{PORTABLE}\bin\64bit\obs64.exe");
        assert!(under_root(&portable_exe, r"G:\"));
        assert!(!under_root(&portable_exe, &Path::new(r"G:\").join("obs-studio").to_string_lossy()));
    }

    /// `restart` remembers the obs-studio folder of the exe it ran.
    #[test]
    fn obs_dir_found_from_the_exe() {
        assert_eq!(obs_dir_of(Path::new(&format!(r"{PORTABLE}\bin\64bit\obs64.exe"))), Some(PathBuf::from(PORTABLE)));
        assert_eq!(obs_dir_of(Path::new(r"G:\obs-studio\bin\64bit\obs64.exe")), Some(PathBuf::from(r"G:\obs-studio")));
        assert_eq!(obs_dir_of(Path::new(r"bin\64bit\obs64.exe")), None);
        assert_eq!(obs_dir_of(Path::new(r"64bit\obs64.exe")), None);
        assert_eq!(obs_dir_of(Path::new(r"C:\obs64.exe")), None);
        assert_eq!(obs_dir_of(Path::new("")), None);
    }

    #[test]
    fn recorder_path_spellings_match() {
        let exe = r"g:/portable/REPLAY VAULT/replay-vault-data/OBS/OBS-Studio/bin/64bit/obs64.exe";
        assert!(under_root(exe, PORTABLE));
        assert!(under_root(exe, &format!(r"{PORTABLE}\")));
        assert!(under_root(exe, r"G:/Portable/Replay Vault/replay-vault-data/obs/obs-studio//"));
        assert!(under_root(&format!(r"\\?\{PORTABLE}\bin\64bit\obs64.exe"), PORTABLE));
        assert!(under_root(&format!(r"{PORTABLE}\bin\64bit\obs64.exe"), &format!(r"\\?\{PORTABLE}")));
        assert!(under_root(r"\\?\UNC\server\share\obs\obs-studio\bin\64bit\obs64.exe", r"\\server\share\obs\obs-studio"));
        assert!(under_root(r"D:\obs-studio\bin\64bit\obs64.exe", r"D:\obs-studio\"));
    }

    #[test]
    fn recorder_not_matched_in_a_sibling_folder_or_without_a_path() {
        assert!(!under_root(r"C:\obs-studio2\bin\64bit\obs64.exe", r"C:\obs-studio"));
        assert!(!under_root(r"C:\obs-studio.old\bin\64bit\obs64.exe", r"C:\obs-studio"));
        assert!(!under_root(r"C:\obs-studio", r"C:\obs-studio"));
        assert!(!under_root("", r"C:\obs"));
        assert!(!under_root(r"C:\obs\obs64.exe", ""));
        assert!(!under_root(r"C:\obs\obs64.exe", "  "));
        assert!(!is_ours("", Path::new(INSTALLED)));
    }

    /// Real folders compare by their real paths, so another spelling of the same folder matches and
    /// a sibling folder still doesn't.
    #[test]
    fn recorder_matched_by_real_path() {
        let base = std::env::temp_dir().join(format!("replay-vault-roots-{}", std::process::id()));
        let root = base.join(r"obs\obs-studio");
        let exe = root.join(r"bin\64bit\obs64.exe");
        let sibling = base.join(r"obs\obs-studio2\bin\64bit\obs64.exe");
        for f in [&exe, &sibling] {
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, b"").unwrap();
        }
        let canonical = fs::canonicalize(&exe).unwrap();
        assert!(canonical.to_string_lossy().starts_with(r"\\?\"));
        assert!(is_ours(&canonical.to_string_lossy(), &root));
        assert!(is_ours(&exe.to_string_lossy().to_uppercase(), &root));
        // Only the real paths show this root holds the exe.
        assert!(!under_root(&exe.to_string_lossy(), &base.join(r"obs\obs-studio2\..\obs-studio").to_string_lossy()));
        assert!(is_ours(&exe.to_string_lossy(), &base.join(r"obs\obs-studio2\..\obs-studio")));
        assert!(!is_ours(&sibling.to_string_lossy(), &root));
        assert!(!is_ours(&fs::canonicalize(&sibling).unwrap().to_string_lossy(), &root));
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn recorder_query_output_parsed() {
        let out = "123|C:\\obs\\obs64.exe\r\n  456 | D:\\x y\\obs64.exe \r\n789|\r\n\r\nnot a pid|C:\\a\r\nWARNING: something\r\n12a|C:\\b\r\n|C:\\c\r\n";
        assert_eq!(
            parse_recorders(out),
            vec![(123, r"C:\obs\obs64.exe".to_string()), (456, r"D:\x y\obs64.exe".to_string()), (789, String::new())]
        );
        assert!(parse_recorders("").is_empty());
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("replay-vault-writes-{tag}-{}", crate::config::random_hex()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A writer that blocks everyone else, and one that shares like OBS's writers do, both count as
    /// holding the file; a reader doesn't, nor a closed or missing file.
    #[test]
    fn file_held_for_writing() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = temp_dir("held");
        let clip = dir.join("clip.mp4");
        fs::write(&clip, b"x").unwrap();
        assert!(!held_for_writing(&clip));
        let only_me = fs::OpenOptions::new().write(true).share_mode(0).open(&clip).unwrap();
        assert!(held_for_writing(&clip));
        drop(only_me);
        assert!(!held_for_writing(&clip));
        // ffmpeg and OBS's hybrid MP4 writer open with every kind of sharing allowed.
        let shared = fs::OpenOptions::new().write(true).share_mode(1 | 2 | 4).open(&clip).unwrap();
        assert!(held_for_writing(&clip));
        drop(shared);
        let reader = fs::File::open(&clip).unwrap();
        assert!(!held_for_writing(&clip), "a reader like Explorer's thumbnails doesn't count");
        drop(reader);
        assert!(!held_for_writing(&dir.join("gone.mp4")));
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Settled only when every recent file was there last time at the same size and nothing holds it.
    #[test]
    fn still_writing_from_two_looks() {
        let p = |n: &str, len: u64| (PathBuf::from(n), len);
        let free = |_: &Path| false;
        let before = [p("a.mp4", 10), p("b.mp4", 20)];
        assert!(!still_writing(&before, &before, free));
        assert!(still_writing(&before, &[p("a.mp4", 10), p("b.mp4", 25)], free), "b grew");
        assert!(still_writing(&before, &[p("a.mp4", 10), p("b.mp4", 20), p("c.mp4", 0)], free), "c is new");
        assert!(!still_writing(&before, &[p("a.mp4", 10)], free), "b was moved away");
        assert!(still_writing(&before, &before, |f: &Path| f == Path::new("b.mp4")), "b still open");
        assert!(!still_writing(&[], &[], free));
    }

    /// Only videos from the last few minutes are looked at; old ones and other files aren't.
    #[test]
    fn only_recent_videos_looked_at() {
        use std::os::windows::fs::FileTimesExt;
        let dir = temp_dir("recent");
        fs::write(dir.join("new.mp4"), b"x").unwrap();
        fs::write(dir.join("notes.txt"), b"x").unwrap();
        fs::create_dir(dir.join("VALORANT.mp4")).unwrap();
        let old = dir.join("old.MKV");
        let hour_ago = std::time::SystemTime::now() - Duration::from_secs(3600);
        let f = fs::File::create(&old).unwrap();
        f.set_times(fs::FileTimes::new().set_modified(hour_ago).set_created(hour_ago)).unwrap();
        drop(f);
        let since = std::time::SystemTime::now() - RECENT;
        assert_eq!(recent_videos(&dir, since), vec![dir.join("new.mp4")]);
        assert!(recent_videos(&dir.join("missing"), since).is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    /// The wait ends once a save being written is closed, runs to the cap while it isn't or while a
    /// save is still on its way, and costs nothing when nothing recent is there.
    #[test]
    fn waits_for_a_save_to_be_written() {
        use std::io::Write;
        use std::os::windows::fs::OpenOptionsExt;
        let dir = temp_dir("wait");
        let dirs = [dir.clone(), dir.join("Recordings")];
        let t = Instant::now();
        assert!(wait_until_written(&dirs, &[], || false, Duration::from_secs(5)));
        assert!(t.elapsed() < Duration::from_millis(100), "nothing recent: no wait");

        // A save written in pieces for about 0.6 s, shared like OBS's writers.
        let clip = dir.join("2026-11-12 10-00-00.mp4");
        let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).share_mode(1 | 2 | 4).open(&clip).unwrap();
        let writer = std::thread::spawn(move || {
            for _ in 0..6 {
                f.write_all(&[0u8; 4096]).unwrap();
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        let t = Instant::now();
        assert!(wait_until_written(&dirs, &[], || false, Duration::from_secs(5)));
        writer.join().unwrap();
        assert!(t.elapsed() >= Duration::from_millis(400), "waited for the writer: {:?}", t.elapsed());

        // Still held when the cap runs out.
        let held = fs::OpenOptions::new().write(true).share_mode(1 | 2 | 4).open(&clip).unwrap();
        let t = Instant::now();
        assert!(!wait_until_written(&dirs, &[], || false, Duration::from_millis(600)));
        assert!(t.elapsed() >= Duration::from_millis(600) && t.elapsed() < Duration::from_secs(2));
        drop(held);
        // A save asked for that hasn't landed holds the wait too, even with every file closed.
        assert!(!wait_until_written(&dirs, &[], || true, Duration::from_millis(300)));
        // A recording outside the folders, named as just stopped, counts as well.
        let elsewhere = temp_dir("wait-recording");
        let rec = elsewhere.join("old recording.mp4");
        fs::write(&rec, b"x").unwrap();
        let held = fs::OpenOptions::new().write(true).share_mode(1 | 2 | 4).open(&rec).unwrap();
        assert!(!wait_until_written(&[], std::slice::from_ref(&rec), || false, Duration::from_millis(300)));
        drop(held);
        assert!(wait_until_written(&[], &[rec], || false, Duration::from_secs(2)));
        fs::remove_dir_all(&dir).unwrap();
        fs::remove_dir_all(&elsewhere).unwrap();
    }

    /// A harmless hidden background process gets tied to this (test) process's kill-on-close job.
    #[test]
    fn recorder_is_tied_to_the_app() {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::core::BOOL;
        use windows::Win32::System::JobObjects::IsProcessInJob;
        let mut child = Command::new(windows_exe("PING.EXE"))
            .args(["-n", "30", "127.0.0.1"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let tied = tie_to_app(&child);
        let mut in_job = BOOL(0);
        let _ = unsafe { IsProcessInJob(HANDLE(child.as_raw_handle()), None, &mut in_job) };
        let _ = child.kill();
        assert!(tied && in_job.as_bool());
    }
}