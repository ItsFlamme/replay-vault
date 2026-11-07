//! Reads the video recording settings of the user's own OBS (read-only), offered as an option:
//! resolution, frame rate, codec and quality. Their OBS files are never written.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use crate::config::Config;
use crate::display::Monitor;
use crate::{obs_install, recorder, startup};

type Ini = HashMap<(String, String), String>;

fn read_ini(path: PathBuf) -> Option<Ini> {
    let text = fs::read_to_string(path).ok()?;
    let mut out = Ini::new();
    let mut section = String::new();
    for line in text.lines().map(|l| l.trim_start_matches('\u{feff}').trim()) {
        if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = s.to_string();
        } else if let Some((k, v)) = line.split_once('=') {
            out.insert((section.clone(), k.trim().to_string()), v.trim().to_string());
        }
    }
    Some(out)
}

fn get<'a>(ini: &'a Ini, section: &str, key: &str) -> Option<&'a str> {
    ini.get(&(section.to_string(), key.to_string())).map(String::as_str).filter(|v| !v.is_empty())
}

/// The user's main OBS settings folder: next to their install in portable mode, else %APPDATA%.
fn config_root() -> Option<PathBuf> {
    if let Some(root) = startup::obs_exe().and_then(|e| obs_install::portable_install_root(&e)) {
        return Some(root.join(r"config\obs-studio"));
    }
    Some(dirs::config_dir()?.join("obs-studio")).filter(|p| p.exists())
}

/// Their active profile's basic.ini.
fn active_profile() -> Option<PathBuf> {
    let root = config_root()?;
    // OBS 31+ keeps [Basic] in user.ini; older versions in global.ini.
    let user = read_ini(root.join("user.ini"))
        .filter(|i| get(i, "Basic", "ProfileDir").is_some())
        .or_else(|| read_ini(root.join("global.ini")))?;
    let profile = root.join(r"basic\profiles").join(get(&user, "Basic", "ProfileDir")?).join("basic.ini");
    profile.exists().then_some(profile)
}

/// Whether there's an OBS setup to import from.
pub fn found() -> bool {
    active_profile().is_some()
}

/// Copies their video settings into `cfg`. Returns a summary, or `None` if nothing
/// usable was found.
pub fn apply(cfg: &mut Config, mon: &Monitor) -> Option<String> {
    let profile_path = active_profile()?;
    let basic = read_ini(profile_path.clone())?;
    let mut got = Vec::new();
    let adv = get(&basic, "Output", "Mode") == Some("Advanced");

    if let Some(h) = get(&basic, "Video", "OutputCY").and_then(|s| s.parse::<u32>().ok()) {
        cfg.out_height = if h < mon.height { h } else { 0 };
        let fps = match get(&basic, "Video", "FPSType").unwrap_or("0") {
            "1" => get(&basic, "Video", "FPSInt").and_then(|s| s.parse::<f64>().ok()),
            "2" => {
                let n = get(&basic, "Video", "FPSNum").and_then(|s| s.parse::<f64>().ok());
                let d = get(&basic, "Video", "FPSDen").and_then(|s| s.parse::<f64>().ok()).filter(|d| *d > 0.0);
                n.zip(d).map(|(n, d)| n / d)
            }
            _ => get(&basic, "Video", "FPSCommon").and_then(|s| s.parse::<f64>().ok()),
        };
        if let Some(f) = fps {
            cfg.fps = (f.round() as u32).clamp(10, 360);
        }
        got.push("resolution, frame rate");
    }

    // Recording encoder; Advanced mode's "none" means it shares the streaming encoder.
    let own_rec_encoder = adv && get(&basic, "AdvOut", "RecEncoder").is_some_and(|e| e != "none");
    let encoder = if adv {
        if own_rec_encoder { get(&basic, "AdvOut", "RecEncoder") } else { get(&basic, "AdvOut", "Encoder") }
    } else {
        get(&basic, "SimpleOutput", "RecEncoder")
    };
    if let Some(e) = encoder.map(str::to_lowercase) {
        cfg.codec = if e.contains("av1") { "av1" } else if e.contains("hevc") || e.contains("265") { "hevc" } else { "h264" }.into();
        got.push("codec");
    }
    // Quality only from a recording setup; a shared streaming bitrate is too low for clips.
    let quality = if !adv {
        match get(&basic, "SimpleOutput", "RecQuality") {
            Some("Lossless") => Some(0),
            Some("HQ") => Some(1),
            Some("Small") => Some(2),
            _ => None,
        }
    } else if own_rec_encoder {
        let enc = fs::read_to_string(profile_path.with_file_name("recordEncoder.json")).ok();
        let kbps = enc.and_then(|s| serde_json::from_str::<Value>(&s).ok()).and_then(|v| v["bitrate"].as_u64());
        kbps.map(|k| {
            (0..4u8)
                .min_by_key(|&q| {
                    let mut c = cfg.clone();
                    c.quality = q;
                    recorder::plan(&c, mon).kbps.abs_diff(k)
                })
                .unwrap_or(1)
        })
    } else {
        None
    };
    if let Some(q) = quality {
        cfg.quality = q;
        got.push("quality");
    }

    (!got.is_empty()).then(|| {
        let p = recorder::plan(cfg, mon);
        format!(
            "Imported {} from your OBS: {}p at {} fps, {}, {} quality.",
            match got.split_last() {
                Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
                _ => got.join(""),
            },
            p.out_h,
            p.fps,
            cfg.codec.to_uppercase().replace("H264", "H.264"),
            ["Highest", "High", "Medium", "Low"][cfg.quality.min(3) as usize],
        )
    })
}
