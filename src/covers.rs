//! Game cover art (Steam's tall 600x900 library picture) for the game cards, found by the game
//! folder's name. Only the name is sent, and only when it isn't cached yet. Kept in
//! `covers\` in the app's folder: `<game>.jpg`, or `<game>.none` when Steam has no game by exactly
//! that name (asked again after a week).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

/// Games being looked up right now.
static ASKING: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Games found with no cover, when and whether online, so the cards don't look in the covers
/// folder every frame. Looked at again after RECHECK, or as soon as a download ends.
static MISSED: Mutex<Option<HashMap<String, (Instant, bool)>>> = Mutex::new(None);

const RECHECK: Duration = Duration::from_secs(45);

/// Whether `game` was found with no cover within RECHECK of `now`, asked the same way.
fn missed_lately(missed: &HashMap<String, (Instant, bool)>, game: &str, online: bool, now: Instant) -> bool {
    missed.get(game).is_some_and(|&(at, was)| was == online && now.saturating_duration_since(at) < RECHECK)
}

/// Tests use their own folder, filled only by `steam_lookup_live`.
fn dir() -> PathBuf {
    if cfg!(test) { std::env::temp_dir().join("instant-replay-covers-test") } else { crate::config::dir().join("covers") }
}

/// The game's cover once it's downloaded. Until then None, and, if `online`, it's looked up in
/// the background (never in tests).
pub fn get(game: &str, online: bool) -> Option<PathBuf> {
    if missed_lately(MISSED.lock().unwrap().get_or_insert_default(), game, online, Instant::now()) {
        return None;
    }
    let dir = dir();
    let state = state(&dir, game, SystemTime::now());
    if !matches!(state, State::Have(_)) {
        MISSED.lock().unwrap().get_or_insert_default().insert(game.to_string(), (Instant::now(), online));
    }
    match state {
        State::Have(path) => Some(path),
        State::Missing => None,
        State::Ask if cfg!(test) || !online => None,
        State::Ask => {
            let fresh = ASKING.lock().unwrap().get_or_insert_default().insert(game.to_string());
            if fresh {
                let game = game.to_string();
                std::thread::spawn(move || {
                    let got = fetch(&dir, &game);
                    // Shown on the next look, not after RECHECK.
                    MISSED.lock().unwrap().get_or_insert_default().remove(&game);
                    if let Err(e) = got {
                        crate::log::write(&format!("Couldn't get the cover for {game}: {e:#}"));
                        // Offline or Steam busy: not asked again for 10 minutes.
                        std::thread::sleep(Duration::from_secs(600));
                    }
                    ASKING.lock().unwrap().get_or_insert_default().remove(&game);
                });
            }
            None
        }
    }
}

#[derive(Debug, PartialEq)]
enum State {
    Have(PathBuf),
    /// No match on Steam, asked within the week (or not a game, like Desktop).
    Missing,
    Ask,
}

fn state(dir: &Path, game: &str, now: SystemTime) -> State {
    let jpg = dir.join(format!("{game}.jpg"));
    if jpg.exists() {
        return State::Have(jpg);
    }
    let asked = std::fs::metadata(dir.join(format!("{game}.none"))).and_then(|m| m.modified()).ok();
    let recent = asked.is_some_and(|t| now.duration_since(t).unwrap_or_default() < Duration::from_secs(7 * 86_400));
    let not_game = !looks_like_game(game) || dir.join(format!("{game}.skip")).exists();
    if recent || not_game || game.eq_ignore_ascii_case("desktop") { State::Missing } else { State::Ask }
}

/// Programs whose windows are in front often but are never games. Lower case.
const NOT_GAMES: &[&str] = &[
    "google chrome", "chrome", "mozilla firefox", "firefox", "microsoft edge", "edge", "opera", "opera gx", "brave", "vivaldi",
    "file explorer", "explorer", "windows explorer", "program manager", "discord", "slack", "microsoft teams", "teams", "zoom",
    "spotify", "steam", "notepad", "notepad++", "visual studio code", "visual studio", "cursor", "sublime text", "obsidian",
    "word", "microsoft word", "excel", "microsoft excel", "powerpoint", "outlook", "settings", "task manager", "windows powershell",
    "command prompt", "terminal", "windows terminal",
];

/// Whether a window title (or a game folder named after one) could be a game: not an email
/// address, a link, a file name, a Discord channel or a known non-game program. Only names that
/// pass are ever sent to Steam.
pub fn looks_like_game(title: &str) -> bool {
    let t = title.trim().to_lowercase();
    if t.is_empty() || t.contains('@') || t.starts_with('#') || t.contains("://") || t.contains("www.") || (t.starts_with("http") && t.contains('.')) {
        return false;
    }
    // "Notes.docx", "gmail.com": a dot then 2 to 5 letters or digits (one a letter) at the end of
    // any part of the title.
    let parts: Vec<&str> = [" - ", " | ", " \u{2014} ", ": "].iter().fold(vec![t.as_str()], |acc, sep| acc.iter().flat_map(|p| p.split(sep)).collect());
    let file_like = |p: &str| {
        p.trim().rsplit_once('.').is_some_and(|(stem, ext)| {
            !stem.is_empty() && (2..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) && ext.chars().any(|c| c.is_ascii_alphabetic())
        })
    };
    !parts.iter().any(|p| file_like(p) || NOT_GAMES.contains(&p.trim()))
}

/// Remembers that the game folder `game` was named after a window that isn't a game (see
/// `looks_like_game`), so it's never looked up; a later save from a game window by that name
/// takes it back.
pub fn note_title(game: &str, title: &str) {
    let skip = dir().join(format!("{game}.skip"));
    if looks_like_game(title) {
        let _ = std::fs::remove_file(skip);
    } else if !skip.exists() && std::fs::create_dir_all(dir()).is_ok() {
        let _ = std::fs::write(skip, b"");
    }
}

/// Removes the markers (`.skip`, `.none`, a half-downloaded `.part`) of games with no folder in
/// the clips folder or the archive any more, and those older versions wrote for every window
/// seen. Covers stay. Does nothing if either folder isn't there (a drive or NAS that's off).
pub fn tidy(clips: &Path, archive: Option<&Path>) {
    tidy_in(&dir(), clips, archive);
}

fn tidy_in(dir: &Path, clips: &Path, archive: Option<&Path>) {
    let roots: Vec<&Path> = std::iter::once(clips).chain(archive).collect();
    if !roots.iter().all(|r| r.is_dir()) {
        return;
    }
    let Ok(files) = std::fs::read_dir(dir) else { return };
    let mut removed = 0;
    for path in files.flatten().map(|f| f.path()) {
        let marker = path.extension().is_some_and(|e| ["skip", "none", "part"].iter().any(|m| e.eq_ignore_ascii_case(m)));
        if marker && path.file_stem().is_some_and(|game| !roots.iter().any(|r| r.join(game).is_dir())) && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        crate::log::write(&format!("Removed {removed} cover notes for games with no clips."));
    }
}

/// Looks the game up on Steam and saves its cover, or a `.none` marker when there's no exact match.
fn fetch(dir: &Path, game: &str) -> anyhow::Result<()> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(20))).build().into();
    let search: serde_json::Value = serde_json::from_str(&agent.get("https://store.steampowered.com/api/storesearch/").query("term", game).query("cc", "us").query("l", "en").call()?.body_mut().read_to_string()?)?;
    std::fs::create_dir_all(dir)?;
    let url = match matching_id(&search, game) {
        Some(id) => {
            let input = format!(r#"{{"ids":[{{"appid":{id}}}],"context":{{"language":"english","country_code":"US"}},"data_request":{{"include_assets":true}}}}"#);
            let items: serde_json::Value = serde_json::from_str(&agent.get("https://api.steampowered.com/IStoreBrowseService/GetItems/v1/").query("input_json", &input).call()?.body_mut().read_to_string()?)?;
            cover_url(&items)
        }
        None => None,
    };
    let Some(url) = url else {
        std::fs::write(dir.join(format!("{game}.none")), b"")?;
        return Ok(());
    };
    let bytes = agent.get(&url).call()?.body_mut().with_config().limit(8 << 20).read_to_vec()?;
    // Written whole, then renamed, so a half-written file is never shown.
    let part = dir.join(format!("{game}.part"));
    std::fs::write(&part, bytes)?;
    std::fs::rename(&part, dir.join(format!("{game}.jpg")))?;
    let _ = std::fs::remove_file(dir.join(format!("{game}.none")));
    Ok(())
}

/// Letters and digits only, lower case: "ELDEN RING" and "Elden Ring" match, "Tom Clancy's" and
/// "Tom Clancys" too.
fn plain(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// The Steam app id of the search result named exactly like the game.
fn matching_id(search: &serde_json::Value, game: &str) -> Option<u64> {
    let want = plain(game);
    search["items"].as_array()?.iter().find(|i| plain(i["name"].as_str().unwrap_or_default()) == want)?["id"].as_u64()
}

/// The tall library picture's address (600x900 when there is one, else 300x450), from Steam's
/// item details.
fn cover_url(items: &serde_json::Value) -> Option<String> {
    let assets = &items["response"]["store_items"][0]["assets"];
    let file = assets["library_capsule_2x"].as_str().or(assets["library_capsule"].as_str())?;
    let format = assets["asset_url_format"].as_str()?;
    Some(format!("https://shared.akamai.steamstatic.com/store_item_assets/{}", format.replace("${FILENAME}", file)))
}

/// The picture, decoded for a texture.
pub fn load(path: &Path) -> Option<eframe::egui::ColorImage> {
    let img = image::load_from_memory(&std::fs::read(path).ok()?).ok()?.to_rgba8();
    Some(eframe::egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], &img))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only an exact name match counts (ignoring case and punctuation), not the first result.
    #[test]
    fn matches_exact_names_only() {
        let search = serde_json::json!({"items": [
            {"name": "ELDEN RING NIGHTREIGN", "id": 2622380},
            {"name": "ELDEN RING", "id": 1245620},
            {"name": "Desktop Dungeons", "id": 226620},
        ]});
        assert_eq!(matching_id(&search, "Elden Ring"), Some(1245620));
        assert_eq!(matching_id(&search, "VALORANT"), None);
        assert_eq!(matching_id(&serde_json::json!({}), "Elden Ring"), None);
        assert_eq!(plain("Tom Clancy's Rainbow Six® Siege"), "tomclancysrainbowsixsiege");
    }

    #[test]
    fn cover_address_from_assets() {
        let items = serde_json::json!({"response": {"store_items": [{"assets": {
            "asset_url_format": "steam/apps/1245620/${FILENAME}?t=1790290043",
            "library_capsule": "library_600x900.jpg"
        }}]}});
        assert_eq!(cover_url(&items).as_deref(), Some("https://shared.akamai.steamstatic.com/store_item_assets/steam/apps/1245620/library_600x900.jpg?t=1790290043"));
        let mut both = items.clone();
        both["response"]["store_items"][0]["assets"]["library_capsule_2x"] = "library_600x900_2x.jpg".into();
        assert!(cover_url(&both).unwrap().contains("/library_600x900_2x.jpg?"));
        assert_eq!(cover_url(&serde_json::json!({"response": {"store_items": [{}]}})), None);
    }

    /// Asks Steam for real (so not in the normal run): Elden Ring gets a picture, VALORANT (not
    /// on Steam) a "no match". Into the tests' covers folder, left there for the screen renders.
    #[test]
    #[ignore]
    fn steam_lookup_live() {
        let dir = dir();
        let _ = std::fs::remove_dir_all(&dir);
        fetch(&dir, "Elden Ring").unwrap();
        fetch(&dir, "VALORANT").unwrap();
        let img = load(&dir.join("Elden Ring.jpg")).unwrap();
        assert_eq!(img.size, [600, 900]);
        assert!(dir.join("VALORANT.none").exists() && !dir.join("VALORANT.jpg").exists());
    }

    /// Only names that could be a game go to Steam.
    #[test]
    fn non_games_are_never_sent() {
        for game in ["VALORANT", "Elden Ring", "Counter-Strike 2", "S.T.A.L.K.E.R. 2", "Mr. Shifty", "Tom Clancy's Rainbow Six® Siege", "Mount & Blade II", "v1.2"] {
            assert!(looks_like_game(game), "{game}");
        }
        for not in [
            "jose@gmail.com",
            "Inbox (3) - jose@gmail.com - Gmail",
            "https://example.com/page",
            "httpsexample.compage",
            "www.reddit.com",
            "Divorce papers.docx",
            "Divorce papers.docx - Word",
            "notes.txt - Notepad",
            "#private-channel",
            "#private-channel | Server - Discord",
            "Elden Ring wiki - Google Chrome",
            "Downloads - File Explorer",
            "main.rs - replay-vault - Visual Studio Code",
            "Discord",
            "Mozilla Firefox",
            "",
        ] {
            assert!(!looks_like_game(not), "{not}");
        }
        let dir = std::env::temp_dir().join("instant-replay-covers-skip");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now();
        assert_eq!(state(&dir, "jose@gmail.com", now), State::Missing);
        assert_eq!(state(&dir, "Inbox (3)", now), State::Ask);
        std::fs::write(dir.join("Inbox (3).skip"), b"").unwrap();
        assert_eq!(state(&dir, "Inbox (3)", now), State::Missing, "named after a browser tab: never asked");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Markers of games with no clips folder go, everything else stays; nothing goes if the
    /// clips folder is missing.
    #[test]
    fn tidy_removes_orphan_markers() {
        let root = std::env::temp_dir().join("instant-replay-covers-tidy");
        let _ = std::fs::remove_dir_all(&root);
        let (dir, clips) = (root.join("covers"), root.join("clips"));
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["Inbox (3).skip", "Gone.none", "Gone.part", "Gone.jpg", "Kept.skip", "Kept.none"] {
            std::fs::write(dir.join(f), b"").unwrap();
        }
        tidy_in(&dir, &clips, None);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 6, "no clips folder: nothing removed");
        std::fs::create_dir_all(clips.join("Kept")).unwrap();
        tidy_in(&dir, &clips, None);
        let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|f| f.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["Gone.jpg", "Kept.none", "Kept.skip"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A game with no cover isn't looked for again until RECHECK has gone by, unless it's now
    /// asked the other way (covers just turned on or off).
    #[test]
    fn misses_are_remembered_for_a_while() {
        let now = Instant::now();
        let missed: HashMap<String, (Instant, bool)> = [("VALORANT".to_string(), (now, true))].into();
        assert!(missed_lately(&missed, "VALORANT", true, now + Duration::from_secs(10)));
        assert!(!missed_lately(&missed, "VALORANT", true, now + RECHECK));
        assert!(!missed_lately(&missed, "VALORANT", false, now), "asked the other way");
        assert!(!missed_lately(&missed, "Elden Ring", true, now));
        // Through get: a cover saved after a miss shows once the miss is forgotten.
        let (game, dir) = ("Replay Vault miss test", dir());
        std::fs::create_dir_all(&dir).unwrap();
        let jpg = dir.join(format!("{game}.jpg"));
        let _ = std::fs::remove_file(&jpg);
        assert_eq!(get(game, false), None);
        std::fs::write(&jpg, b"x").unwrap();
        assert_eq!(get(game, false), None, "remembered as missing");
        MISSED.lock().unwrap().get_or_insert_default().remove(game);
        assert_eq!(get(game, false), Some(jpg.clone()));
        std::fs::remove_file(&jpg).unwrap();
    }

    /// A saved cover is used, a "no match" is trusted for a week, Desktop is never asked about.
    #[test]
    fn cache_states() {
        let dir = std::env::temp_dir().join("instant-replay-covers");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now();
        assert_eq!(state(&dir, "Elden Ring", now), State::Ask);
        assert_eq!(state(&dir, "Desktop", now), State::Missing);
        std::fs::write(dir.join("VALORANT.none"), b"").unwrap();
        assert_eq!(state(&dir, "VALORANT", now), State::Missing);
        assert_eq!(state(&dir, "VALORANT", now + Duration::from_secs(8 * 86_400)), State::Ask);
        std::fs::write(dir.join("Elden Ring.jpg"), b"x").unwrap();
        assert_eq!(state(&dir, "Elden Ring", now), State::Have(dir.join("Elden Ring.jpg")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
