//! Reads the recording settings of the user's own OBS, read-only: resolution, frame rate, codec
//! and quality, and the sound devices they record with their tracks and mic noise suppression.
//! Their OBS files are only ever read, never written, moved or deleted.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use crate::config::{Config, DeviceSel};
use crate::devices::Device;
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

/// One of OBS's [Basic] settings: OBS 31+ keeps them in user.ini, older versions in global.ini.
fn basic_setting(root: &std::path::Path, key: &str) -> Option<String> {
    ["user.ini", "global.ini"].iter().find_map(|f| read_ini(root.join(f)).and_then(|i| get(&i, "Basic", key).map(String::from)))
}

/// Their active profile's basic.ini.
fn active_profile(root: &std::path::Path) -> Option<PathBuf> {
    let profile = root.join(r"basic\profiles").join(basic_setting(root, "ProfileDir")?).join("basic.ini");
    profile.exists().then_some(profile)
}

/// Their active scene collection, where OBS keeps the sound sources.
fn active_scenes(root: &std::path::Path) -> Option<Value> {
    let file = basic_setting(root, "SceneCollectionFile")?;
    let file = if file.ends_with(".json") { file } else { format!("{file}.json") };
    serde_json::from_str(&fs::read_to_string(root.join(r"basic\scenes").join(file)).ok()?).ok()
}

/// Whether there's an OBS setup to import from.
pub fn found() -> bool {
    config_root().is_some_and(|r| active_profile(&r).is_some())
}

/// The mic noise suppression of an OBS source, in our terms ("rnnoise" or "speex").
fn noise_of(source: &Value) -> Option<String> {
    source["filters"].as_array()?.iter().filter(|f| f["enabled"].as_bool() != Some(false)).find_map(|f| {
        let id = f["id"].as_str()?;
        if !id.starts_with("noise_suppress_filter") {
            return None;
        }
        // The old filter was Speex only; the newer one defaults to RNNoise. NVIDIA's has no
        // equivalent here, so it becomes RNNoise.
        let method = f["settings"]["method"].as_str().unwrap_or(if id == "noise_suppress_filter" { "speex" } else { "rnnoise" });
        Some(if method == "speex" { "speex" } else { "rnnoise" }.to_string())
    })
}

/// Copies the sound devices of their OBS into `cfg`: the playback devices and mics they record
/// (not muted, present on this PC), on the same tracks (all six OBS has). Returns their names, or
/// `None` (and `cfg` unchanged) if nothing usable was found.
fn import_audio(cfg: &mut Config, scenes: &Value, basic: &Ini, present: &[Device]) -> Option<Vec<String>> {
    let adv = get(basic, "Output", "Mode") == Some("Advanced");
    let rec_tracks = get(basic, if adv { "AdvOut" } else { "SimpleOutput" }, "RecTracks")
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|m| *m != 0)
        .unwrap_or(1);
    // The OBS tracks they record become ours, in order.
    let obs_tracks: Vec<u32> = (0..6).filter(|b| rec_tracks & (1 << b) != 0).collect();
    let globals = ["DesktopAudioDevice1", "DesktopAudioDevice2", "AuxAudioDevice1", "AuxAudioDevice2", "AuxAudioDevice3", "AuxAudioDevice4"]
        .iter()
        .filter_map(|k| scenes.get(*k));
    let mut found: BTreeMap<String, DeviceSel> = BTreeMap::new();
    let mut noise = None;
    for s in globals.chain(scenes["sources"].as_array().into_iter().flatten()) {
        let prefix = match s["id"].as_str() {
            Some("wasapi_output_capture") => "out",
            Some("wasapi_input_capture") => "in",
            _ => continue,
        };
        if s["muted"].as_bool() == Some(true) || s["enabled"].as_bool() == Some(false) {
            continue;
        }
        let key = format!("{prefix}:{}", s["settings"]["device_id"].as_str().filter(|d| !d.is_empty()).unwrap_or("default"));
        let Some(dev) = present.iter().find(|d| d.key == key) else { continue };
        // OBS puts a new source on all six tracks unless changed.
        let mixers = s["mixers"].as_u64().unwrap_or(0x3f);
        let sel = found.entry(key).or_insert_with(|| DeviceSel { name: dev.name.clone(), tracks: [false; 6] });
        for (i, b) in obs_tracks.iter().enumerate() {
            sel.tracks[i] |= mixers & (1 << b) != 0;
        }
        if prefix == "in" && noise.is_none() {
            noise = noise_of(s);
        }
    }
    found.retain(|_, d| d.tracks.iter().any(|&t| t));
    if found.is_empty() {
        return None;
    }
    for d in cfg.devices.values_mut() {
        d.tracks = [false; 6];
    }
    if found.keys().any(|k| k.starts_with("in:")) {
        cfg.noise = noise.unwrap_or_else(|| "off".into());
    }
    let names = found.values().map(|d| d.name.clone()).collect();
    cfg.devices.extend(found);
    cfg.track_count = obs_tracks.len() as u8;
    cfg.only_apps = false;
    Some(names)
}

/// Copies their video and sound settings into `cfg`. Returns a summary, or `None` if nothing
/// usable was found. Their OBS folder is only read, never written (see `their_obs_is_left_as_it_was`).
pub fn apply(cfg: &mut Config, mon: &Monitor, present: &[Device]) -> Option<String> {
    apply_from(&config_root()?, cfg, mon, present)
}

fn apply_from(root: &std::path::Path, cfg: &mut Config, mon: &Monitor, present: &[Device]) -> Option<String> {
    let profile_path = active_profile(root)?;
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

    let sound = active_scenes(root).and_then(|s| import_audio(cfg, &s, &basic, present));
    if sound.is_some() {
        got.push("sound");
    }

    (!got.is_empty()).then(|| {
        let p = recorder::plan(cfg, mon);
        let mut s = format!(
            "Imported {} from your OBS: {}p at {} fps, {}, {} quality.",
            match got.split_last() {
                Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
                _ => got.join(""),
            },
            p.out_h,
            p.fps,
            cfg.codec.to_uppercase().replace("H264", "H.264"),
            ["Highest", "High", "Medium", "Low"][cfg.quality.min(3) as usize],
        );
        if let Some(names) = sound {
            s.push_str(&format!(" Sound: {}.", names.join(", ")));
        }
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dev(key: &str, name: &str) -> Device {
        Device { key: key.into(), name: name.into() }
    }

    /// Like a real OBS setup: a mic and a playback device on tracks 1 and 2 (recording tracks 1
    /// and 2), a muted app source and a device that isn't plugged in, which are left out.
    #[test]
    fn imports_sound_devices_and_tracks() {
        let scenes = json!({ "sources": [
            { "id": "wasapi_input_capture", "settings": { "device_id": "{mic}" }, "mixers": 3, "muted": false,
              "filters": [{ "id": "noise_suppress_filter_v2", "settings": { "method": "speex" } }] },
            { "id": "wasapi_output_capture", "settings": { "device_id": "{chat}" }, "mixers": 2 },
            { "id": "wasapi_output_capture", "settings": { "device_id": "{gone}" }, "mixers": 1 },
            { "id": "wasapi_process_output_capture", "settings": {}, "mixers": 1, "muted": true },
            { "id": "wasapi_input_capture", "settings": { "device_id": "{muted}" }, "muted": true },
        ]});
        let mut basic = Ini::new();
        basic.insert(("Output".into(), "Mode".into()), "Advanced".into());
        basic.insert(("AdvOut".into(), "RecTracks".into()), "3".into());
        let present = [dev("in:{mic}", "Mic"), dev("out:{chat}", "Chat"), dev("in:{muted}", "Muted")];
        let mut cfg = Config::default();
        let names = import_audio(&mut cfg, &scenes, &basic, &present).unwrap();
        assert_eq!(names, ["Mic", "Chat"]);
        assert_eq!(cfg.track_count, 2);
        assert_eq!(cfg.devices["in:{mic}"].tracks, [true, true, false, false, false, false]);
        assert_eq!(cfg.devices["out:{chat}"].tracks, [false, true, false, false, false, false]);
        assert_eq!(cfg.devices["out:default"].tracks, [false; 6], "the old default is no longer recorded");
        assert!(!cfg.devices.contains_key("in:{muted}"));
        assert_eq!(cfg.noise, "speex");
        // All six OBS tracks recorded: all six come across, in order.
        basic.insert(("AdvOut".into(), "RecTracks".into()), "63".into());
        let mut cfg = Config::default();
        import_audio(&mut cfg, &scenes, &basic, &present).unwrap();
        assert_eq!(cfg.track_count, 6);
        assert_eq!(cfg.devices["out:{chat}"].tracks, [false, true, false, false, false, false]);
    }

    /// Every file in `dir` (path, bytes, last-modified time), to compare before and after.
    fn snapshot(dir: &std::path::Path) -> BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)> {
        let mut out = BTreeMap::new();
        let mut todo = vec![dir.to_path_buf()];
        while let Some(d) = todo.pop() {
            for e in fs::read_dir(&d).unwrap().flatten() {
                let (path, meta) = (e.path(), e.metadata().unwrap());
                if meta.is_dir() {
                    todo.push(path);
                } else {
                    out.insert(path.clone(), (fs::read(&path).unwrap(), meta.modified().unwrap()));
                }
            }
        }
        out
    }

    /// The import reads a whole OBS setup (OBS 31 layout: user.ini, a profile with its own
    /// recording encoder, a scene collection with a mic) and leaves every file and folder of it
    /// exactly as it was: nothing written, added, renamed or removed.
    #[test]
    fn their_obs_is_left_as_it_was() {
        let root = std::env::temp_dir().join(format!("replay-vault-obs-import-{}", crate::config::random_hex()));
        let profile = root.join(r"basic\profiles\Gaming");
        fs::create_dir_all(&profile).unwrap();
        fs::create_dir_all(root.join(r"basic\scenes")).unwrap();
        fs::write(root.join("user.ini"), "[Basic]\nProfileDir=Gaming\nSceneCollectionFile=Main\n").unwrap();
        fs::write(root.join("global.ini"), "[General]\nLastVersion=520093696\n").unwrap();
        fs::write(
            profile.join("basic.ini"),
            "[Video]\nOutputCY=1080\nFPSType=1\nFPSInt=60\n[Output]\nMode=Advanced\n[AdvOut]\nRecEncoder=jim_hevc_nvenc\nRecTracks=1\n",
        )
        .unwrap();
        fs::write(profile.join("recordEncoder.json"), r#"{"bitrate": 40000}"#).unwrap();
        let scenes = json!({ "sources": [{ "id": "wasapi_input_capture", "settings": { "device_id": "{mic}" }, "mixers": 1 }] });
        fs::write(root.join(r"basic\scenes\Main.json"), scenes.to_string()).unwrap();
        let before = snapshot(&root);

        let mon = Monitor { id: "m".into(), width: 2560, height: 1440, hz: 144, x: 0, y: 0, primary: true, hdr: None, name: String::new() };
        let mut cfg = Config::default();
        let summary = apply_from(&root, &mut cfg, &mon, &[dev("in:{mic}", "Mic")]);

        let after = snapshot(&root);
        let _ = fs::remove_dir_all(&root);
        let summary = summary.expect("the sample setup should import");
        assert!(summary.contains("1080p at 60 fps") && summary.contains("Mic"), "{summary}");
        assert_eq!(cfg.codec, "hevc");
        assert!(before == after, "the import changed their OBS folder");
    }

    /// Nothing usable leaves the sound setup as it was.
    #[test]
    fn no_usable_sound_changes_nothing() {
        let scenes = json!({ "sources": [{ "id": "wasapi_output_capture", "settings": { "device_id": "{gone}" } }] });
        let mut cfg = Config::default();
        let before = cfg.clone();
        assert!(import_audio(&mut cfg, &scenes, &Ini::new(), &[dev("out:default", "Default")]).is_none());
        assert!(cfg == before);
    }
}
