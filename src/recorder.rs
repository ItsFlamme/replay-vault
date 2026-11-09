//! The hidden "recorder" OBS: our private copy in portable mode with its own config folder, so
//! the user's own OBS install, profile and scenes are never touched.

use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;

use crate::config::{self, Config};
use crate::display::{self, Monitor};
use crate::{hotkey, log, obs_install};

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
    if cfg.follows_mouse() { display::list().iter().find_map(|m| m.hdr) } else { mon.hdr }
}

/// Copies the main OBS's version stamps so the hidden copy never shows "What's new" dialogs.
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
    /// Dedicated video memory; integrated GPUs report little or none.
    pub vram_mb: u64,
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
}

pub fn plan(cfg: &Config, mon: &Monitor) -> Plan {
    // Canvas = the monitor; the clip is only ever scaled down, keeping the aspect ratio.
    let (base_w, base_h) = (mon.width, mon.height);
    let out_h = if cfg.out_height == 0 { base_h } else { cfg.out_height.min(base_h) };
    let out_w = (base_w * out_h / base_h) & !1; // encoders want an even width
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
    let mb = (kbps + 192 * cfg.track_count.clamp(1, 6) as u64) * cfg.buffer_secs as u64 / 8 / 1000 * 5 / 4 + 64;
    Plan { base_w, base_h, out_w, out_h, fps, kbps, mb }
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
    // Version stamps stop "what's new" dialogs: our private copy knows its own version, a
    // shared install matches the user's main OBS settings.
    let private_stamp = obs_install::is_private(&exe)
        .then(obs_install::private_version)
        .flatten()
        .and_then(|v| obs_install::last_version_stamp(&v));
    log::write(&write_files(cfg, &portable_root(&exe), &mon, &main_root, private_stamp)?);
    Ok(())
}

/// Writes the recorder's settings, profile and scene collection under `root`. Returns the
/// encoder line for the app log.
fn write_files(cfg: &Config, root: &Path, mon: &Monitor, main_root: &Path, private_stamp: Option<u64>) -> anyhow::Result<String> {
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

    let Plan { base_w, base_h, out_w, out_h, fps, kbps, mb } = plan(cfg, mon);
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
    let (encoder, mut enc_settings) = match (gpu(), cfg.codec.as_str()) {
        (Gpu::Nvidia, codec) => (
            match codec { "hevc" => "obs_nvenc_hevc_tex", "av1" => "obs_nvenc_av1_tex", _ => "obs_nvenc_h264_tex" },
            json!({
                "rate_control": "VBR", "bitrate": kbps, "max_bitrate": max,
                "preset2": nv_preset, "tune": "hq", "multipass": nv_multipass,
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
    // Psycho-visual tuning runs on the GPU's CUDA cores, which games use too. Left at OBS's own
    // default for best quality.
    if gpu() == Gpu::Nvidia && effort > 0 {
        enc_settings["psycho_aq"] = json!(false);
    }
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
    let basic = format!(
        "[General]\nName={NAME}\n\n[Output]\nMode=Advanced\nFilenameFormatting={name_format}\n\n\
         [AdvOut]\nRecType=Standard\nRecFilePath={folder}\nRecFormat2=mp4\nRecEncoder={encoder}\nRecRBPrefix=\n\
         RecTracks={mask}\nRecRB=true\nRecRBTime={}\nRecRBSize={mb}\nRecAudioEncoder=ffmpeg_aac\n\
         Track1Bitrate=192\nTrack2Bitrate=192\nTrack3Bitrate=192\nTrack4Bitrate=192\nTrack5Bitrate=192\nTrack6Bitrate=192\n\n\
         [Video]\nBaseCX={base_w}\nBaseCY={base_h}\nOutputCX={out_w}\nOutputCY={out_h}\nFPSType=1\nFPSInt={fps}\n{hdr}\n\
         [Audio]\nSampleRate=48000\nChannelSetup=Stereo\n",
        cfg.buffer_secs
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
    let mut items = vec![json!({ "name": "Screen", "visible": true })];
    // Following the mouse switches the capture to monitors of other sizes; scale each to fit
    // the canvas (OBS "scale to inner bounds", centred), so the clip size never changes.
    if cfg.follows_mouse() {
        items[0] = json!({
            "name": "Screen", "visible": true, "pos": { "x": 0, "y": 0 }, "scale": { "x": 1, "y": 1 }, "align": 5,
            "bounds_type": 2, "bounds_align": 0, "bounds": { "x": base_w, "y": base_h },
        });
    }
    // Game capture mode: OBS hooks into whatever fullscreen game runs and draws it over the
    // screen capture, which still shows whenever no game is hooked. Off unless the user picks it.
    if cfg.capture == "game" {
        sources.push(json!({
            "name": "Game", "id": "game_capture", "versioned_id": "game_capture",
            "settings": { "capture_mode": "any_fullscreen", "capture_cursor": true, "anti_cheat_hook": true },
        }));
        items.push(json!({ "name": "Game", "visible": true }));
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
    for (key, dev) in &cfg.devices {
        let mixers: u32 = (0..n).filter(|&i| dev.tracks[i]).map(|i| 1 << i).sum();
        if mixers == 0 {
            continue;
        }
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
        let mut source = json!({
            "name": name, "id": kind, "versioned_id": kind, "mixers": mixers,
            "settings": { "device_id": id },
        });
        if kind == "wasapi_input_capture" && cfg.noise != "off" {
            source["filters"] = json!([{
                "name": "Noise Suppression", "id": "noise_suppress_filter", "versioned_id": "noise_suppress_filter_v2",
                "enabled": true, "settings": { "method": cfg.noise },
            }]);
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

/// Every running hidden recorder, including strays left by a crash or an older build. Matched
/// by the launch flags only this app uses, so the user's own OBS is never picked up.
fn all_recorders() -> Vec<u32> {
    let script = "Get-CimInstance Win32_Process -Filter \"Name='obs64.exe'\" | Where-Object { \
                  $_.CommandLine -match '--portable' -and $_.CommandLine -match '--multi' -and \
                  $_.CommandLine -match '--startreplaybuffer' } | ForEach-Object { $_.ProcessId }";
    Command::new(windows_exe("powershell.exe"))
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| l.trim().parse().ok()).collect())
        .unwrap_or_default()
}

/// Stops the hidden recorder(s). Safe to kill: OBS skips its crash check for `--multi` copies.
/// Only processes launched with our flags are killed: a saved PID alone could by now belong to the
/// user's own OBS (Windows reuses PIDs).
pub fn stop() {
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
    // Only for our private copy: the user's own OBS exe keeps whatever they chose for it.
    if obs_install::is_private(&exe)
        && let Err(e) = crate::tweaks::set_gpu_high(&exe, cfg.gpu_high) {
        log::write(&format!("Couldn't set the recorder's graphics preference: {e:#}"));
    }
    let child = Command::new(&exe)
        .args(["--portable", "--multi", "--minimize-to-tray", "--startreplaybuffer", "--disable-updater"])
        .current_dir(exe.parent().unwrap())
        // The user's OBS is set to run as admin; run this copy as a normal user so the app can stop it.
        .env("__COMPAT_LAYER", "RunAsInvoker")
        .spawn()?;
    let _ = tie_to_app(&child);
    PID.store(child.id(), std::sync::atomic::Ordering::Relaxed);
    fs::create_dir_all(config::dir())?;
    fs::write(pid_file(), child.id().to_string())?;
    Ok(())
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

    fn test_monitor() -> Monitor {
        Monitor { id: r"\\?\DISPLAY#TEST#1".into(), width: 2560, height: 1440, hz: 144, x: 0, y: 0, primary: true, hdr: None, name: String::new() }
    }

    /// Writes the recorder files into a fresh temp folder (never the real OBS config) and
    /// returns basic.ini and the scene collection.
    fn written(tag: &str, cfg: &mut Config, mon: &Monitor) -> (String, serde_json::Value) {
        let root = std::env::temp_dir().join(format!("replay-vault-recorder-test-{tag}"));
        let _ = fs::remove_dir_all(&root);
        cfg.folder = root.join("clips").display().to_string();
        write_files(cfg, &root, mon, &root.join("main"), None).unwrap();
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

    /// Following the mouse fits whichever monitor is captured into the canvas; not in game mode.
    #[test]
    fn follow_the_mouse_fits_monitors_to_the_canvas() {
        let mut cfg = Config { follow_mouse: true, ..Config::default() };
        let (_, scene) = written("follow", &mut cfg, &test_monitor());
        let screen = &scene["sources"][0]["settings"]["items"][0];
        assert_eq!(screen["name"], "Screen");
        assert_eq!((screen["bounds_type"].as_i64(), screen["bounds_align"].as_i64()), (Some(2), Some(0)));
        assert_eq!((screen["bounds"]["x"].as_u64(), screen["bounds"]["y"].as_u64()), (Some(2560), Some(1440)));
        cfg.capture = "game".into();
        let (_, scene) = written("follow-game", &mut cfg, &test_monitor());
        assert!(scene["sources"][0]["settings"]["items"][0].get("bounds_type").is_none());
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
        let mb = |t: u8| plan(&Config { track_count: t, buffer_secs: 300, ..Config::default() }, &test_monitor()).mb;
        assert!((7..=10).contains(&(mb(2) - mb(1))), "about 7 MB per track for 5 minutes, with headroom: {}", mb(2) - mb(1));
    }

    #[test]
    fn lighter_effort_steps_down_to_max_fps() {
        assert_eq!(lighter_effort(0), Some(1));
        assert_eq!(lighter_effort(1), Some(2));
        assert_eq!(lighter_effort(2), None);
        assert_eq!(lighter_effort(7), None);
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