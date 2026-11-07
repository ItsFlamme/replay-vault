//! Game cover art (Steam's tall 600x900 library picture) for the game cards, found by the game
//! folder's name. Only the name is sent, and only when it isn't cached yet. Kept in
//! `covers\` in the app's folder: `<game>.jpg`, or `<game>.none` when Steam has no game by exactly
//! that name (asked again after a week).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// Games being looked up right now.
static ASKING: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Tests use their own folder, filled only by `steam_lookup_live`.
fn dir() -> PathBuf {
    if cfg!(test) { std::env::temp_dir().join("instant-replay-covers-test") } else { crate::config::dir().join("covers") }
}

/// The game's cover once it's downloaded. Until then None, and it's looked up in the background
/// (never in tests).
pub fn get(game: &str) -> Option<PathBuf> {
    let dir = dir();
    match state(&dir, game, SystemTime::now()) {
        State::Have(path) => Some(path),
        State::Missing => None,
        State::Ask if cfg!(test) => None,
        State::Ask => {
            let fresh = ASKING.lock().unwrap().get_or_insert_default().insert(game.to_string());
            if fresh {
                let game = game.to_string();
                std::thread::spawn(move || {
                    if let Err(e) = fetch(&dir, &game) {
                        crate::log::write(&format!("Couldn't get the cover for {game}: {e:#}"));
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
    if recent || game.eq_ignore_ascii_case("desktop") { State::Missing } else { State::Ask }
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
