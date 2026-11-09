use std::{collections::BTreeMap, fs, path::PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub folder: String,
    pub buffer_secs: u32,
    /// Number of audio tracks in each clip: 1 to 6 (OBS records up to 6).
    pub track_count: u8,
    /// What each audio track is called in the Audio tab and on the trim sliders. Only a label;
    /// what gets recorded on each track is set per device. Empty means named after its devices.
    /// Files from before 6 tracks have 3 here; the rest load empty.
    #[serde(deserialize_with = "padded")]
    pub track_names: [String; 6],
    /// The tracks voice chat (Discord's own sound, OBS application audio capture) goes on, apart
    /// from the playback devices that also play it. Never track 1, which has it already.
    #[serde(deserialize_with = "padded")]
    pub chat_tracks: [bool; 6],
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
    /// Looks up game covers on Steam by game folder name (covers.rs).
    pub steam_covers: bool,
    /// Auto-cleanup: keep clips (not counting favorites) under this many GB, and remove clips
    /// older than this many days, to the Recycle Bin. 0 means no limit.
    pub keep_under_gb: u32,
    pub keep_days: u32,
    /// Archive: moves clips older than this many days (0 is off) into this folder, on another
    /// drive or a NAS.
    pub archive_days: u32,
    pub archive_folder: String,
    /// "desktop" (whole monitor, never touches the game) or "game" (OBS game capture, which
    /// hooks into the game; some anti-cheats object).
    pub capture: String,
    /// A short, quiet sound when a clip or screenshot is saved.
    pub save_sound: bool,
    /// Turns Windows Game Bar capture off while set (put back when unset or on Remove).
    pub game_bar_off: bool,
    /// Turns NVIDIA's overlay and Instant Replay off while set, so its hotkeys don't fire with ours
    /// (put back when unset or on Remove).
    pub nvidia_off: bool,
    /// Turns AMD Software's Instant Replay and hotkeys off while set (untested on an AMD PC; put
    /// back when unset or on Remove).
    pub amd_off: bool,
    /// Asks Windows to run the private OBS copy on the high-performance GPU.
    pub gpu_high: bool,
    /// Clip file names are the game, then the date and time in this style.
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
    /// Pages whose "More options" are open. A view choice: kept without Save.
    pub more_open: Vec<String>,
    /// The first-run setup wizard has been finished.
    pub setup_done: bool,
    /// Discord webhook link that "Post to Discord" sends clips to. A secret: never logged or shown.
    pub discord_webhook: String,
    /// Where the private OBS copy is installed; empty means %LOCALAPPDATA%\replay-vault (obs-instant-replay on older installs).
    pub obs_dir: String,
    /// Pages whose first-run tip was closed. A view choice: kept without Save.
    pub tips_seen: Vec<String>,
    /// Tip boxes and the grey explanations under settings (General).
    pub show_tips: bool,
    /// Look for a newer Replay Vault on GitHub once a day (General).
    pub check_updates: bool,
    /// The app's accent color, an index into the list on the General page (0 is purple), or one
    /// past its end for `custom_accent`.
    pub accent: u8,
    /// The accent picked with "Custom", as "#RRGGBB".
    pub custom_accent: String,
    /// "minimalist" (flat, keycap-like buttons) or "retro" (the site's 80s tape look).
    pub theme: String,
    /// Light mode; dark when off.
    pub light_mode: bool,
    /// Retro without its scanlines, grain and glows.
    pub reduce_effects: bool,
    /// How the Clips page orders clips, an index into the sort list there (0 is newest first). A
    /// view choice: kept without Save.
    pub clip_sort: u8,
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceSel {
    pub name: String,
    /// Files from before 6 tracks have 3 here; the rest load off.
    #[serde(deserialize_with = "padded")]
    pub tracks: [bool; 6],
}

/// A list from the settings file as an array of N: shorter lists (older files) are filled up with
/// the default, longer ones cut.
fn padded<'de, D, T, const N: usize>(d: D) -> Result<[T; N], D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    let mut out: [T; N] = std::array::from_fn(|_| T::default());
    for (o, v) in out.iter_mut().zip(Vec::<T>::deserialize(d)?) {
        *o = v;
    }
    Ok(out)
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
            DeviceSel { name: "Default playback device".into(), tracks: [true, false, false, false, false, false] },
        );
        Self {
            folder: dirs::video_dir().unwrap_or_default().join("Replay Vault Clips").display().to_string(),
            buffer_secs: 60,
            track_count: 1,
            track_names: Default::default(),
            chat_tracks: [false; 6],
            // Save is NVIDIA's Alt+F10, so ShadowPlay players keep the habit (the recorder reads it
            // itself, so NVIDIA can't block it). The rest stay off NVIDIA's defaults (Alt+Z, Alt+F9,
            // Alt+F1, Alt+Shift+F10) and AMD's (Alt+Z, Alt+R): NVIDIA holds its keys even with its
            // overlay off, so ours couldn't be registered.
            hotkey: "Alt+F10".into(),
            overlay_hotkey: "Alt+X".into(),
            hotkey_30: String::new(),
            hotkey_60: String::new(),
            hotkey_record: "Alt+F11".into(),
            hotkey_screenshot: "Alt+F12".into(),
            hotkey_replay: "Alt+Shift+F11".into(),
            hotkey_mic: String::new(),
            sort_by_game: true,
            steam_covers: true,
            keep_under_gb: 0,
            keep_days: 0,
            archive_days: 0,
            archive_folder: String::new(),
            capture: "desktop".into(),
            save_sound: false,
            game_bar_off: false,
            nvidia_off: false,
            amd_off: false,
            gpu_high: true,
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
            more_open: Vec::new(),
            setup_done: false,
            discord_webhook: String::new(),
            obs_dir: String::new(),
            tips_seen: Vec::new(),
            show_tips: true,
            check_updates: true,
            accent: 0,
            custom_accent: "#8448F0".into(),
            theme: "minimalist".into(),
            light_mode: false,
            reduce_effects: false,
            clip_sort: 0,
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
        cfg.sanitize();
        cfg
    }

    /// Puts every value back into the range the app can use, so a hand-edited or imported file
    /// can't crash the recorder or a page: unknown choices go to their default, numbers are
    /// clamped, keys that don't parse are cleared (the save key goes back to its default).
    pub fn sanitize(&mut self) {
        let d = Self::default();
        self.track_count = self.track_count.clamp(1, 6);
        // Track 1 already has voice chat, from the playback devices.
        self.chat_tracks[0] = false;
        self.buffer_secs = self.buffer_secs.clamp(10, 1200);
        self.quality = self.quality.min(3);
        self.name_style = self.name_style.min(crate::recorder::NAME_STYLES.len() as u8 - 1);
        self.encoder_effort = self.encoder_effort.min(2);
        self.fps = self.fps.min(240);
        if self.out_height != 0 && !(144..=4320).contains(&self.out_height) {
            self.out_height = 0;
        }
        let pick = |v: &mut String, known: &[&str], default: &str| {
            if !known.contains(&v.as_str()) {
                *v = default.into();
            }
        };
        pick(&mut self.codec, &["h264", "hevc", "av1"], &d.codec);
        pick(&mut self.capture, &["desktop", "game"], &d.capture);
        pick(&mut self.noise, &["off", "rnnoise", "speex"], &d.noise);
        pick(&mut self.theme, &["minimalist", "retro"], &d.theme);
        if crate::hotkey::Combo::parse(&self.hotkey).is_none() {
            self.hotkey = d.hotkey;
        }
        for k in [
            &mut self.overlay_hotkey,
            &mut self.hotkey_30,
            &mut self.hotkey_60,
            &mut self.hotkey_record,
            &mut self.hotkey_screenshot,
            &mut self.hotkey_replay,
            &mut self.hotkey_mic,
        ] {
            if !k.is_empty() && crate::hotkey::Combo::parse(k).is_none() {
                k.clear();
            }
        }
        // The clips folder is written into OBS's basic.ini, so a line break in it would add lines there.
        for v in [&mut self.folder, &mut self.archive_folder] {
            v.retain(|c| !c.is_control());
        }
        if self.folder.trim().is_empty() {
            self.folder = d.folder;
        }
        for n in &mut self.track_names {
            *n = n.chars().filter(|c| !c.is_control()).take(24).collect();
        }
        self.keep_under_gb = self.keep_under_gb.min(10_000);
        self.keep_days = self.keep_days.min(3650);
        self.archive_days = self.archive_days.min(3650);
    }

    /// Whether the capture follows the mouse: never with game capture.
    /// How many audio tracks clips have, 1 to 6, whatever the file says.
    pub fn tracks(&self) -> usize {
        self.track_count.clamp(1, 6) as usize
    }

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

/// The settings pages, for Restore defaults.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Page {
    General,
    Keybinds,
    Video,
    Audio,
}

/// What kind of value a setting is, for Save, export and import.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    /// An ordinary setting: waits for Save, moves with export and import.
    Setting,
    /// A view choice: kept right away without Save, and this PC keeps its own on import.
    View,
    /// About this PC only: this PC keeps its own on import.
    Local,
    /// Never exported, never shown or logged; this PC keeps its own on import.
    Secret,
}

/// Every setting by its name in config.json: the page whose Restore defaults resets it (None: no
/// page, or kept on purpose, like the clips folder where the clips are), whether the recorder runs
/// with it (a change restarts it, which clears the replay), and what kind it is.
pub const FIELDS: &[(&str, Option<Page>, bool, Kind)] = {
    use Kind::*;
    use Page::*;
    &[
        ("folder", None, true, Setting),
        ("buffer_secs", Some(General), true, Setting),
        ("track_count", Some(Audio), true, Setting),
        ("track_names", Some(Audio), false, Setting),
        ("chat_tracks", Some(Audio), true, Setting),
        ("hotkey", Some(Keybinds), true, Setting),
        ("overlay_hotkey", Some(Keybinds), false, Setting),
        ("hotkey_30", Some(Keybinds), false, Setting),
        ("hotkey_60", Some(Keybinds), false, Setting),
        ("hotkey_record", Some(Keybinds), false, Setting),
        ("hotkey_screenshot", Some(Keybinds), false, Setting),
        ("hotkey_replay", Some(Keybinds), false, Setting),
        ("hotkey_mic", Some(Keybinds), false, Setting),
        ("sort_by_game", Some(General), false, Setting),
        ("steam_covers", Some(General), false, Setting),
        ("keep_under_gb", Some(General), false, Setting),
        ("keep_days", Some(General), false, Setting),
        ("archive_days", Some(General), false, Setting),
        ("archive_folder", Some(General), false, Setting),
        ("capture", Some(Video), true, Setting),
        ("save_sound", Some(General), false, Setting),
        ("game_bar_off", Some(General), false, Setting),
        ("nvidia_off", Some(Keybinds), false, Setting),
        ("amd_off", Some(Keybinds), false, Setting),
        ("gpu_high", Some(General), true, Setting),
        ("name_style", Some(General), true, Setting),
        ("save_popup", Some(General), false, Setting),
        ("quality", Some(Video), true, Setting),
        ("codec", Some(Video), true, Setting),
        ("encoder_effort", Some(Video), true, Setting),
        ("noise", Some(Audio), true, Setting),
        ("monitor_id", Some(Video), true, Setting),
        ("follow_mouse", Some(Video), true, Setting),
        ("out_height", Some(Video), true, Setting),
        ("fps", Some(Video), true, Setting),
        ("devices", Some(Audio), true, Setting),
        ("only_apps", Some(Audio), true, Setting),
        ("apps", Some(Audio), true, Setting),
        ("recorder_password", None, true, Secret),
        ("more_open", None, false, View),
        ("setup_done", None, true, Local),
        ("discord_webhook", None, false, Secret),
        ("obs_dir", None, true, Local),
        ("tips_seen", None, false, View),
        ("show_tips", Some(General), false, Setting),
        ("check_updates", Some(General), false, Setting),
        ("accent", Some(General), false, Setting),
        ("custom_accent", Some(General), false, Setting),
        ("theme", Some(General), false, Setting),
        ("light_mode", Some(General), false, Setting),
        ("reduce_effects", Some(General), false, Setting),
        ("clip_sort", None, false, View),
    ]
};

/// The settings as named values, for copying them by `FIELDS`.
fn values(cfg: &Config) -> serde_json::Map<String, serde_json::Value> {
    match serde_json::to_value(cfg) {
        Ok(serde_json::Value::Object(m)) => m,
        _ => Default::default(),
    }
}

/// `to` with the settings `pick` chooses taken from `from`.
fn copy_fields(to: &Config, from: &Config, pick: impl Fn(Option<Page>, bool, Kind) -> bool) -> Config {
    let (mut out, from) = (values(to), values(from));
    for &(name, page, restart, kind) in FIELDS {
        if pick(page, restart, kind)
            && let Some(v) = from.get(name)
        {
            out.insert(name.into(), v.clone());
        }
    }
    serde_json::from_value(serde_json::Value::Object(out)).unwrap_or_else(|_| to.clone())
}

/// The settings the recorder runs with, as one string: when it changes, the recorder needs a
/// restart.
pub fn recorder_settings(cfg: &Config) -> String {
    let all = values(cfg);
    let part: serde_json::Map<_, _> = FIELDS.iter().filter(|f| f.2).filter_map(|f| Some((f.0.to_string(), all.get(f.0)?.clone()))).collect();
    serde_json::Value::Object(part).to_string()
}

/// `cfg` with `page`'s settings put back to `defaults`.
pub fn restore_page(cfg: &Config, defaults: &Config, page: Page) -> Config {
    copy_fields(cfg, defaults, |p, _, _| p == Some(page))
}

/// The settings with the secrets taken out: the recorder password and the Discord webhook.
pub fn without_secrets(cfg: &Config) -> Config {
    copy_fields(cfg, &Config::default(), |_, _, kind| kind == Kind::Secret)
}

/// A settings file for a new PC or a reinstall: the settings (no secrets), stars, tags and notes.
/// Files from before notes load without them.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Export {
    pub config: Config,
    pub favorites: std::collections::BTreeSet<String>,
    pub tags: crate::clips::Tags,
    pub notes: crate::clips::Notes,
}

impl Export {
    pub fn new(cfg: &Config, favorites: &std::collections::BTreeSet<String>, tags: &crate::clips::Tags) -> Self {
        Self { config: without_secrets(cfg), favorites: favorites.clone(), tags: tags.clone(), notes: Default::default() }
    }

    pub fn write(&self, path: &std::path::Path) -> anyhow::Result<()> {
        fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn read(path: &std::path::Path) -> anyhow::Result<Self> {
        let raw = fs::read_to_string(path)?;
        Ok(serde_json::from_str(raw.trim_start_matches('\u{feff}'))?)
    }

    /// The imported settings on top of `current`, which keeps all but the ordinary settings: its
    /// secrets, its view choices, where its private OBS copy is, and whether setup was done.
    pub fn applied_to(&self, current: &Config) -> Config {
        let mut c = copy_fields(&self.config, current, |_, _, kind| kind != Kind::Setting);
        c.sanitize();
        c
    }
}

/// Settings an import shows with their values (where clips go, what removes them, what's on the
/// network), not only by name.
const SHOWN: [&str; 5] = ["folder", "archive_days", "archive_folder", "keep_under_gb", "keep_days"];

/// The settings in `SHOWN` that differ between `a` and `b`: (name, from, to).
pub fn shown_changes(a: &Config, b: &Config) -> Vec<(&'static str, String, String)> {
    let folder = |f: &str| if f.trim().is_empty() { "not set".to_string() } else { f.trim().to_string() };
    let limit = |n: u32, unit: &str| match n {
        0 => "off".to_string(),
        1 => format!("1 {}", unit.trim_end_matches('s')),
        n => format!("{n} {unit}"),
    };
    [
        ("Clips folder", folder(&a.folder), folder(&b.folder)),
        ("Archive clips older than", limit(a.archive_days, "days"), limit(b.archive_days, "days")),
        ("Archive folder", folder(&a.archive_folder), folder(&b.archive_folder)),
        ("Auto-cleanup: keep clips under", limit(a.keep_under_gb, "GB"), limit(b.keep_under_gb, "GB")),
        ("Auto-cleanup: remove clips older than", limit(a.keep_days, "days"), limit(b.keep_days, "days")),
    ]
    .into_iter()
    .filter(|(_, from, to)| from != to)
    .collect()
}

/// What kind of drive a letter is on this PC.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Drive {
    Local,
    Network,
    Missing,
}

/// This PC's drive `letter`: from Windows' list of drives and the drive's type only, so no
/// folder on it is opened.
pub fn this_pc_drive(letter: char) -> Drive {
    use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    let i = (letter.to_ascii_uppercase() as u32).wrapping_sub('A' as u32);
    if i >= 26 || unsafe { GetLogicalDrives() } & (1 << i) == 0 {
        return Drive::Missing;
    }
    const DRIVE_REMOTE: u32 = 4;
    let root = windows::core::HSTRING::from(format!("{}:\\", letter.to_ascii_uppercase()));
    if unsafe { GetDriveTypeW(&root) } == DRIVE_REMOTE { Drive::Network } else { Drive::Local }
}

/// Why a folder from a settings file needs a second look, judged from its text and `drive` alone
/// (never opened, so a network path isn't contacted before the import is confirmed), or None for
/// a folder on one of this PC's own drives.
pub fn folder_warning(path: &str, drive: impl Fn(char) -> Drive) -> Option<String> {
    let p = path.trim();
    if p.is_empty() {
        return None;
    }
    if p.starts_with(r"\\") || p.starts_with("//") {
        return Some(format!("{p} is a network path: clips would go to another computer."));
    }
    let mut chars = p.chars();
    match (chars.next(), chars.next(), chars.next()) {
        (Some(l), Some(':'), None | Some('\\' | '/')) if l.is_ascii_alphabetic() => match drive(l) {
            Drive::Local => None,
            Drive::Network => Some(format!("{p} is on a network drive ({}:): clips would go to another computer.", l.to_ascii_uppercase())),
            Drive::Missing => Some(format!("{p} is on a drive this PC doesn't have ({}:).", l.to_ascii_uppercase())),
        },
        _ => Some(format!("{p} isn't a full folder path on this PC.")),
    }
}

/// The names of the settings that differ between `a` and `b`, in plain words, leaving out the
/// ones `shown_changes` lists with their values.
pub fn changes(a: &Config, b: &Config) -> Vec<String> {
    let (serde_json::Value::Object(a), serde_json::Value::Object(b)) = (serde_json::json!(a), serde_json::json!(b)) else { return Vec::new() };
    a.iter()
        .filter(|(k, v)| b.get(*k) != Some(v) && !SHOWN.contains(&k.as_str()))
        .map(|(k, _)| match k.as_str() {
            "folder" => "clips folder".into(),
            "buffer_secs" => "clip length".into(),
            "hotkey" => "save clip key".into(),
            "out_height" => "resolution".into(),
            "monitor_id" => "monitor".into(),
            "devices" => "audio devices".into(),
            "apps" => "apps to record".into(),
            "noise" => "mic noise filter".into(),
            "chat_tracks" => "voice chat tracks".into(),
            k => k.strip_prefix("hotkey_").map(|h| format!("{h} key")).unwrap_or_else(|| k.replace('_', " ")),
        })
        .collect()
}

/// The folders in `cfg` (clips, archive) on this PC's own drives that aren't there.
/// Network folders are never looked at.
pub fn missing_folders(cfg: &Config) -> Vec<String> {
    [&cfg.folder, &cfg.archive_folder]
        .into_iter()
        .filter(|f| !f.trim().is_empty() && folder_warning(f, this_pc_drive).is_none() && !std::path::Path::new(f.trim()).exists())
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settings saved before the appearance options load with their defaults: Minimalist, dark,
    /// the accent they had.
    #[test]
    fn old_settings_get_appearance_defaults() {
        let c: Config = serde_json::from_str(r#"{"accent": 2, "buffer_secs": 90}"#).unwrap();
        assert_eq!((c.accent, c.buffer_secs), (2, 90));
        assert_eq!((c.theme.as_str(), c.light_mode, c.custom_accent.as_str()), ("minimalist", false, "#8448F0"));
        assert_eq!(c.clip_sort, 0, "and newest first on the Clips page");
        assert!(!c.reduce_effects, "and Retro's effects on");
    }

    /// Settings from before 6 tracks (3 track names, 3 ticks per device) load into 6, the new ones
    /// empty and off, and write back as 6; a list too long is cut.
    #[test]
    fn three_tracks_load_into_six() {
        let raw = r#"{"track_count": 3, "track_names": ["All", "", "Mic"], "advanced": true,
            "devices": {"in:mic": {"name": "Mic", "tracks": [true, false, true]}}}"#;
        let c: Config = serde_json::from_str(raw).unwrap();
        assert_eq!(c.track_count, 3);
        assert_eq!(c.track_names, ["All", "", "Mic", "", "", ""].map(String::from));
        assert_eq!(c.devices["in:mic"].tracks, [true, false, true, false, false, false]);
        assert_eq!((c.chat_tracks, c.more_open.len()), ([false; 6], 0));
        let back: serde_json::Value = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back["devices"]["in:mic"]["tracks"].as_array().unwrap().len(), 6);
        let long: Config = serde_json::from_str(r#"{"chat_tracks": [true, true, false, false, false, false, true, true]}"#).unwrap();
        assert_eq!(long.chat_tracks, [true, true, false, false, false, false]);
        let mut long = long;
        long.sanitize();
        assert!(!long.chat_tracks[0], "track 1 already has voice chat");
    }

    /// A file with every value out of range (hand-edited, or a hostile settings file) loads and
    /// imports as one the app can use: nothing that indexes by these can panic.
    #[test]
    fn hostile_values_are_clamped() {
        let raw = r#"{"track_count": 200, "buffer_secs": 99999999, "quality": 250, "name_style": 9, "encoder_effort": 77,
            "fps": 100000, "out_height": 99999, "codec": "x; rm", "capture": "kernel", "noise": "loud", "theme": "",
            "hotkey": "Banana+Q", "hotkey_30": "nope", "overlay_hotkey": "Alt+Z", "keep_days": 4000000000,
            "track_names": ["a very long name that goes on and on and on", "Mic\n", ""],
            "folder": "D:\\Clips\n[Hotkeys]\nReplayBuffer=x", "archive_folder": "E:\\Old\r\n"}"#;
        let mut c: Config = serde_json::from_str(raw).unwrap();
        c.sanitize();
        assert_eq!((c.track_count, c.buffer_secs, c.quality, c.name_style, c.encoder_effort), (6, 1200, 3, 1, 2));
        assert_eq!((c.fps, c.out_height, c.keep_days), (240, 0, 3650));
        assert_eq!((c.codec.as_str(), c.capture.as_str(), c.noise.as_str(), c.theme.as_str()), ("h264", "desktop", "off", "minimalist"));
        assert_eq!((c.hotkey.as_str(), c.hotkey_30.as_str(), c.overlay_hotkey.as_str()), ("Alt+F10", "", "Alt+Z"));
        assert_eq!(c.track_names[0].chars().count(), 24);
        assert_eq!((c.folder.as_str(), c.archive_folder.as_str(), c.track_names[1].as_str()), (r"D:\Clips[Hotkeys]ReplayBuffer=x", r"E:\Old", "Mic"));
        let zero: Config = serde_json::from_str(r#"{"track_count": 0, "buffer_secs": 0, "out_height": 2}"#).unwrap();
        let file = Export { config: zero, ..Export::default() };
        let c = file.applied_to(&Config::default());
        assert_eq!((c.track_count, c.buffer_secs, c.out_height), (1, 10, 0), "import clamps too");
        // What used to panic: the recorder's per-track loop and its track mask.
        let mon = crate::display::Monitor { id: String::new(), width: 1920, height: 1080, hz: 60, x: 0, y: 0, primary: true, hdr: None, name: String::new() };
        let plan = crate::recorder::plan(&c, &mon);
        assert!(plan.out_h > 0 && plan.fps > 0);
    }

    /// A settings file round-trips (in a temp folder), carries no secrets, and importing it keeps
    /// this PC's secrets and flags folders that aren't here.
    #[test]
    fn export_import_round_trip() {
        let dir = std::env::temp_dir().join("replay-vault-export-test");
        let _ = fs::create_dir_all(&dir);
        let file = dir.join("settings.json");
        let mut cfg = Config { buffer_secs: 90, hotkey: "Alt+F9".into(), ..Config::default() };
        cfg.folder = dir.display().to_string();
        let missing = dir.join("no-such-folder").display().to_string();
        cfg.archive_folder = missing.clone();
        (cfg.recorder_password, cfg.discord_webhook) = ("pw-secret-1".into(), "https://discord.com/api/webhooks/1/hook-secret-3".into());
        let favs = ["a.mp4".to_string()].into();
        let tags = [("a.mp4".to_string(), ["Aces".to_string()].into())].into();
        Export::new(&cfg, &favs, &tags).write(&file).unwrap();

        let raw = fs::read_to_string(&file).unwrap();
        for secret in ["pw-secret-1", "hook-secret-3"] {
            assert!(!raw.contains(secret), "{secret} was exported");
        }
        let back = Export::read(&file).unwrap();
        assert!(back.favorites == favs && back.tags == tags);

        let here = Config { recorder_password: "here-pw".into(), discord_webhook: "here-hook".into(), ..Config::default() };
        let merged = back.applied_to(&here);
        assert_eq!((merged.buffer_secs, merged.hotkey.as_str()), (90, "Alt+F9"));
        assert_eq!((merged.recorder_password.as_str(), merged.discord_webhook.as_str()), ("here-pw", "here-hook"));
        let changed = changes(&here, &merged);
        assert!(changed.contains(&"clip length".to_string()) && changed.contains(&"save clip key".to_string()), "{changed:?}");
        assert!(!changed.iter().any(|c| c.contains("password") || c.contains("webhook")), "{changed:?}");
        assert_eq!(missing_folders(&merged), vec![missing]);
        let _ = fs::remove_dir_all(&dir);
    }

    /// An imported file that sends clips to a network share shows where, warns, and never looks
    /// the share up; a local folder needs no warning.
    #[test]
    fn import_shows_where_clips_go() {
        let drives = |l: char| match l.to_ascii_uppercase() {
            'C' | 'D' => Drive::Local,
            'N' => Drive::Network,
            _ => Drive::Missing,
        };
        assert_eq!(folder_warning(r"D:\Clips", drives), None);
        assert_eq!(folder_warning("", drives), None);
        assert_eq!(folder_warning("c:", drives), None);
        for net in [r"\\attacker.example@SSL\DavWWWRoot\x", "//nas/clips", r"\\?\UNC\nas\clips"] {
            assert!(folder_warning(net, drives).unwrap().contains("network path"), "{net}");
        }
        assert!(folder_warning(r"N:\Clips", drives).unwrap().contains("network drive (N:)"));
        assert!(folder_warning(r"Q:\Clips", drives).unwrap().contains("doesn't have (Q:)"));
        assert!(folder_warning(r"clips\here", drives).unwrap().contains("isn't a full folder path"));
        // Nothing here is looked up: a share that doesn't exist is no slower than a local path.
        let folder = std::env::temp_dir().display().to_string();
        let cfg = Config { folder: folder.clone(), archive_days: 1, archive_folder: r"\\no-such-host.invalid\x".into(), keep_days: 2, ..Config::default() };
        assert!(missing_folders(&cfg).is_empty(), "network folders aren't checked");
        let here = Config { folder, ..Config::default() };
        let shown = shown_changes(&here, &cfg);
        assert_eq!(
            shown,
            vec![
                ("Archive clips older than", "off".to_string(), "1 day".to_string()),
                ("Archive folder", "not set".to_string(), r"\\no-such-host.invalid\x".to_string()),
                ("Auto-cleanup: remove clips older than", "off".to_string(), "2 days".to_string()),
            ]
        );
        assert!(changes(&here, &cfg).is_empty(), "shown with values, not again by name");
    }

    /// Every setting in config.json is in `FIELDS` once, and copying by it loses nothing.
    #[test]
    fn every_setting_is_in_the_table() {
        let keys: Vec<String> = values(&Config::default()).keys().cloned().collect();
        let mut names: Vec<String> = FIELDS.iter().map(|f| f.0.to_string()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), FIELDS.len(), "a setting is listed twice");
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        let mut cfg = Config { buffer_secs: 45, hotkey: "Ctrl+F2".into(), track_names: ["a", "b", "", "", "", "f"].map(String::from), clip_sort: 2, ..Config::default() };
        cfg.devices.insert("in:mic".into(), DeviceSel { name: "Mic".into(), tracks: [true, false, true, false, false, true] });
        cfg.apps.push(AppSel { title: "Game".into(), class: "G".into(), exe: "game.exe".into() });
        (cfg.recorder_password, cfg.more_open) = ("pw".into(), vec!["Video".into()]);
        assert!(copy_fields(&Config::default(), &cfg, |_, _, _| true) == cfg);
    }

    /// A settings file as this version writes it imports with this PC's own view choices (the
    /// clips sort too), secrets and OBS copy, and Restore defaults keeps the clips folder.
    #[test]
    fn todays_settings_file_imports() {
        let raw = r#"{"config": {"folder": "D:\\Clips", "buffer_secs": 120, "hotkey": "Alt+F10", "recorder_password": "",
            "more_open": ["General"], "setup_done": true, "discord_webhook": "", "obs_dir": "E:\\obs", "tips_seen": ["Clips"],
            "clip_sort": 3, "theme": "retro", "devices": {"out:spk": {"name": "Speakers", "tracks": [true, true, false, false, false, false]}}},
            "favorites": ["D:\\Clips\\a.mp4"], "tags": {"D:\\Clips\\a.mp4": ["Aces"]}}"#;
        let file: Export = serde_json::from_str(raw).unwrap();
        let here = Config { recorder_password: "pw".into(), clip_sort: 1, obs_dir: String::new(), tips_seen: vec![], ..Config::default() };
        let c = file.applied_to(&here);
        assert_eq!((c.folder.as_str(), c.buffer_secs, c.theme.as_str()), (r"D:\Clips", 120, "retro"));
        assert_eq!(c.devices["out:spk"].tracks, [true, true, false, false, false, false]);
        assert_eq!((c.clip_sort, c.recorder_password.as_str(), c.obs_dir.as_str(), c.setup_done), (1, "pw", "", false));
        assert!(c.more_open.is_empty() && c.tips_seen.is_empty());
        let restored = restore_page(&c, &Config::default(), Page::General);
        assert_eq!((restored.folder.as_str(), restored.buffer_secs, restored.theme.as_str()), (r"D:\Clips", Config::default().buffer_secs, "minimalist"));
        assert_eq!(restored.devices.len(), 1, "other pages stay");
        assert!(file.notes.is_empty(), "files from before notes load without them");
    }

    /// Notes go into a settings file and come back out of it.
    #[test]
    fn export_carries_notes() {
        let notes: crate::clips::Notes = [("D:\\Clips\\a.mp4".to_string(), "Clutch 1v4".to_string())].into();
        let file = Export { notes: notes.clone(), ..Export::new(&Config::default(), &Default::default(), &Default::default()) };
        let back: Export = serde_json::from_str(&serde_json::to_string(&file).unwrap()).unwrap();
        assert_eq!(back.notes, notes);
    }
}
