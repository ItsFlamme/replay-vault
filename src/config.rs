use std::{collections::BTreeMap, fs, path::PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub folder: String,
    pub buffer_secs: u32,
    /// Number of audio tracks in each clip: 1 to 3.
    pub track_count: u8,
    /// What each audio track is called in the Audio tab and on the trim sliders. Only a label;
    /// what gets recorded on each track is set per device. Empty means named after its devices.
    pub track_names: [String; 3],
    /// Save-clip hotkey; handled by the hidden OBS itself.
    pub hotkey: String,
    /// Shows or hides the overlay.
    pub overlay_hotkey: String,
    /// Save only the last 30 seconds / 1 minute; empty means not set.
    pub hotkey_30: String,
    pub hotkey_60: String,
    /// Start/stop a full recording, take a screenshot, turn instant replay on/off, mute the mic.
    pub hotkey_record: String,
    pub hotkey_screenshot: String,
    pub hotkey_replay: String,
    pub hotkey_mic: String,
    /// Moves each clip into a folder named after the window in front when it was saved.
    pub sort_by_game: bool,
    /// Auto-cleanup: keep clips (not counting favorites) under this many GB, and remove clips
    /// older than this many days, to the Recycle Bin. 0 means no limit.
    pub keep_under_gb: u32,
    pub keep_days: u32,
    /// Archive: moves clips older than this many days (0 is off) into this folder, on another
    /// drive or a NAS.
    pub archive_days: u32,
    pub archive_folder: String,
    /// Copies clips tagged Cloud into this folder (a OneDrive or Google Drive folder its app uploads).
    pub backup: bool,
    pub backup_folder: String,
    /// "desktop" (whole monitor, never touches the game) or "game" (OBS game capture, which
    /// hooks into the game; some anti-cheats object).
    pub capture: String,
    /// A short, quiet sound when a clip or screenshot is saved.
    pub save_sound: bool,
    /// Turns Windows Game Bar capture off while set (put back when unset or on Remove).
    pub game_bar_off: bool,
    /// Asks Windows to run the private OBS copy on the high-performance GPU.
    pub gpu_high: bool,
    /// Clip file names: this, then the date and time.
    pub clip_prefix: String,
    /// 0: "2026-11-06 18-42-33", 1: "11-06-2026 6-42-33 PM".
    pub name_style: u8,
    /// Shows a small "Clip saved" popup in the corner.
    pub save_popup: bool,
    /// 0 Highest, 1 High, 2 Medium, 3 Low.
    pub quality: u8,
    /// "h264", "hevc" or "av1".
    pub codec: String,
    /// How hard the encoder works: 0 best quality, 1 balanced, 2 max FPS (lighter, lower quality).
    pub encoder_effort: u8,
    /// Mic noise suppression: "off", "rnnoise" or "speex".
    pub noise: String,
    /// Monitor to record (OBS `monitor_id`); empty means the primary monitor.
    pub monitor_id: String,
    /// Records the monitor the mouse is on (Desktop mode only); clips keep `monitor_id`'s size.
    pub follow_mouse: bool,
    /// Clip height in pixels; 0 means the monitor's native resolution.
    pub out_height: u32,
    /// Clip frame rate; 0 means the monitor's refresh rate.
    pub fps: u32,
    /// Windows audio device key ("out:<id>" / "in:<id>") -> its name and tracks.
    pub devices: BTreeMap<String, DeviceSel>,
    /// Game and PC sound: everything the playback devices play (false), or only these apps
    /// (OBS application audio capture), on the tracks the playback devices use.
    pub only_apps: bool,
    pub apps: Vec<AppSel>,
    /// WebSocket password of the hidden recorder OBS (generated, never shown).
    pub recorder_password: String,
    /// Video and Audio show every setting (true) or the simple Basic view (false). A view choice
    /// only: kept without Save and never restarts the recorder.
    pub advanced: bool,
    /// The first-run setup wizard has been finished.
    pub setup_done: bool,
    /// Discord webhook link that "Post to Discord" sends clips to. A secret: never logged or shown.
    pub discord_webhook: String,
    /// "Watch clips from your phone": the clips page on the home network and Tailscale, and the
    /// random key every link to it carries (a secret, like the webhook).
    pub phone_page: bool,
    pub phone_key: String,
    /// Where the private OBS copy is installed; empty means %LOCALAPPDATA%\replay-vault (obs-instant-replay on older installs).
    pub obs_dir: String,
    /// Pages whose first-run tip was closed. A view choice: kept without Save.
    pub tips_seen: Vec<String>,
    /// Tip boxes and the grey explanations under settings (General).
    pub show_tips: bool,
    /// The app's accent color, an index into the list on the General page (0 is purple).
    pub accent: u8,
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceSel {
    pub name: String,
    pub tracks: [bool; 3],
}

/// An app picked for "Only these apps": its window title and class when picked, and its exe
/// name, which is what OBS matches it by.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AppSel {
    pub title: String,
    pub class: String,
    pub exe: String,
}

impl Default for Config {
    fn default() -> Self {
        // Clips need sound out of the box: default playback device on track 1.
        let mut devices = BTreeMap::new();
        devices.insert(
            "out:default".to_string(),
            DeviceSel { name: "Default playback device".into(), tracks: [true, false, false] },
        );
        Self {
            folder: dirs::video_dir().unwrap_or_default().join("Replay Vault").display().to_string(),
            buffer_secs: 60,
            track_count: 1,
            track_names: Default::default(),
            // Not Alt+Z (NVIDIA overlay) or Alt+F10 (NVIDIA's own replay save).
            hotkey: "Alt+F8".into(),
            overlay_hotkey: "Alt+~".into(),
            hotkey_30: String::new(),
            hotkey_60: String::new(),
            hotkey_record: String::new(),
            hotkey_screenshot: String::new(),
            hotkey_replay: String::new(),
            hotkey_mic: String::new(),
            sort_by_game: true,
            keep_under_gb: 0,
            keep_days: 0,
            archive_days: 0,
            archive_folder: String::new(),
            backup: false,
            backup_folder: String::new(),
            capture: "desktop".into(),
            save_sound: false,
            game_bar_off: false,
            gpu_high: true,
            clip_prefix: "Replay".into(),
            name_style: 0,
            save_popup: true,
            quality: 1,
            codec: "h264".into(),
            encoder_effort: 0,
            noise: "off".into(),
            monitor_id: String::new(),
            follow_mouse: false,
            out_height: 0,
            fps: 60,
            devices,
            only_apps: false,
            apps: Vec::new(),
            recorder_password: String::new(),
            advanced: false,
            setup_done: false,
            discord_webhook: String::new(),
            phone_page: false,
            phone_key: String::new(),
            obs_dir: String::new(),
            tips_seen: Vec::new(),
            show_tips: true,
            accent: 0,
        }
    }
}

/// %APPDATA%\replay-vault. Settings from before the rename (obs-instant-replay) move here the first
/// time; if they can't be moved (an old copy still running), the old folder keeps being used.
pub fn dir() -> PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let root = dirs::config_dir().unwrap_or_default();
        let (new, old) = (root.join("replay-vault"), root.join("obs-instant-replay"));
        if !new.exists() && old.exists() && fs::rename(&old, &new).is_err() {
            return old;
        }
        new
    })
    .clone()
}

fn path() -> PathBuf {
    dir().join("config.json")
}

/// Whether settings were ever saved (false for a brand-new user).
pub fn exists() -> bool {
    path().exists()
}

impl Config {
    pub fn load() -> Self {
        let raw = fs::read_to_string(path()).ok();
        // Editors may add a UTF-8 byte-order mark when the file is hand-edited.
        let parsed = raw.as_deref().map(|s| serde_json::from_str::<Self>(s.trim_start_matches('\u{feff}')));
        // A damaged file would be overwritten with defaults; keep it for recovery first.
        if let Some(Err(e)) = &parsed {
            let _ = fs::copy(path(), dir().join("config.bad.json"));
            crate::log::write(&format!("Settings file was damaged ({e}); kept a copy as config.bad.json and started from defaults."));
        }
        let ok = matches!(parsed, Some(Ok(_)));
        let mut cfg: Self = parsed.and_then(Result::ok).unwrap_or_default();
        // Settings saved before the wizard existed were set up by hand already.
        if ok && raw.is_some_and(|s| !s.contains("setup_done")) {
            cfg.setup_done = true;
        }
        if cfg.recorder_password.is_empty() {
            cfg.recorder_password = random_hex();
        }
        if cfg.phone_key.is_empty() {
            cfg.phone_key = random_hex();
        }
        cfg
    }

    /// Whether the capture follows the mouse: never with game capture.
    pub fn follows_mouse(&self) -> bool {
        self.follow_mouse && self.capture != "game"
    }

    pub fn save(&self) -> anyhow::Result<()> {
        fs::create_dir_all(dir())?;
        fs::write(path(), serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

/// 128 random bits from Windows' system RNG, as 32 hex digits.
pub fn random_hex() -> String {
    use windows::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
    let mut bytes = [0u8; 16];
    let status = unsafe { BCryptGenRandom(None, &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    assert!(status.is_ok(), "Windows' random number generator failed");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The settings with the secrets taken out: the recorder password, the phone key and the Discord
/// webhook.
pub fn without_secrets(cfg: &Config) -> Config {
    let mut c = cfg.clone();
    c.recorder_password.clear();
    c.phone_key.clear();
    c.discord_webhook.clear();
    c
}

/// A settings file for a new PC or a reinstall: the settings (no secrets), stars and tags.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Export {
    pub config: Config,
    pub favorites: std::collections::BTreeSet<String>,
    pub tags: crate::clips::Tags,
}

impl Export {
    pub fn new(cfg: &Config, favorites: &std::collections::BTreeSet<String>, tags: &crate::clips::Tags) -> Self {
        Self { config: without_secrets(cfg), favorites: favorites.clone(), tags: tags.clone() }
    }

    pub fn write(&self, path: &std::path::Path) -> anyhow::Result<()> {
        fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn read(path: &std::path::Path) -> anyhow::Result<Self> {
        let raw = fs::read_to_string(path)?;
        Ok(serde_json::from_str(raw.trim_start_matches('\u{feff}'))?)
    }

    /// The imported settings on top of `current`, which keeps its secrets, where its private OBS
    /// copy is, and whether setup was done.
    pub fn applied_to(&self, current: &Config) -> Config {
        let mut c = self.config.clone();
        c.recorder_password = current.recorder_password.clone();
        c.phone_key = current.phone_key.clone();
        c.discord_webhook = current.discord_webhook.clone();
        c.obs_dir = current.obs_dir.clone();
        c.setup_done = current.setup_done;
        c.tips_seen = current.tips_seen.clone();
        c
    }
}

/// The names of the settings that differ between `a` and `b`, in plain words.
pub fn changes(a: &Config, b: &Config) -> Vec<String> {
    let (serde_json::Value::Object(a), serde_json::Value::Object(b)) = (serde_json::json!(a), serde_json::json!(b)) else { return Vec::new() };
    a.iter()
        .filter(|(k, v)| b.get(*k) != Some(v))
        .map(|(k, _)| match k.as_str() {
            "folder" => "clips folder".into(),
            "buffer_secs" => "clip length".into(),
            "hotkey" => "save clip key".into(),
            "out_height" => "resolution".into(),
            "monitor_id" => "monitor".into(),
            "devices" => "audio devices".into(),
            "apps" => "apps to record".into(),
            "noise" => "mic noise filter".into(),
            k => k.strip_prefix("hotkey_").map(|h| format!("{h} key")).unwrap_or_else(|| k.replace('_', " ")),
        })
        .collect()
}

/// The folders in `cfg` (clips, archive, cloud backup) that aren't on this PC.
pub fn missing_folders(cfg: &Config) -> Vec<String> {
    [&cfg.folder, &cfg.archive_folder, &cfg.backup_folder]
        .into_iter()
        .filter(|f| !f.trim().is_empty() && !std::path::Path::new(f.trim()).exists())
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A settings file round-trips (in a temp folder), carries no secrets, and importing it keeps
    /// this PC's secrets and flags folders that aren't here.
    #[test]
    fn export_import_round_trip() {
        let dir = std::env::temp_dir().join("replay-vault-export-test");
        let _ = fs::create_dir_all(&dir);
        let file = dir.join("settings.json");
        let mut cfg = Config { buffer_secs: 90, hotkey: "Alt+F9".into(), ..Config::default() };
        cfg.folder = dir.display().to_string();
        cfg.archive_folder = r"Z:\no\such\folder".into();
        (cfg.recorder_password, cfg.phone_key, cfg.discord_webhook) = ("pw-secret-1".into(), "key-secret-2".into(), "https://discord.com/api/webhooks/1/hook-secret-3".into());
        let favs = ["a.mp4".to_string()].into();
        let tags = [("a.mp4".to_string(), ["Aces".to_string()].into())].into();
        Export::new(&cfg, &favs, &tags).write(&file).unwrap();

        let raw = fs::read_to_string(&file).unwrap();
        for secret in ["pw-secret-1", "key-secret-2", "hook-secret-3"] {
            assert!(!raw.contains(secret), "{secret} was exported");
        }
        let back = Export::read(&file).unwrap();
        assert!(back.favorites == favs && back.tags == tags);

        let here = Config { recorder_password: "here-pw".into(), phone_key: "here-key".into(), discord_webhook: "here-hook".into(), ..Config::default() };
        let merged = back.applied_to(&here);
        assert_eq!((merged.buffer_secs, merged.hotkey.as_str()), (90, "Alt+F9"));
        assert_eq!((merged.recorder_password.as_str(), merged.phone_key.as_str(), merged.discord_webhook.as_str()), ("here-pw", "here-key", "here-hook"));
        let changed = changes(&here, &merged);
        assert!(changed.contains(&"clip length".to_string()) && changed.contains(&"save clip key".to_string()), "{changed:?}");
        assert!(!changed.iter().any(|c| c.contains("password") || c.contains("webhook")), "{changed:?}");
        assert_eq!(missing_folders(&merged), vec![r"Z:\no\such\folder".to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }
}
