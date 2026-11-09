//! Telling the user a newer Replay Vault is out: once a day, GitHub's latest published release is
//! compared with this version. Nothing is downloaded or installed, and nothing about the user or
//! the PC is sent (only the request itself, like any web page). Drafts and pre-releases don't count.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const LATEST: &str = "https://api.github.com/repos/ItsFlamme/replay-vault/releases/latest";

/// Whether to check, from the settings (General > Startup); read by the checking thread.
pub static ENABLED: AtomicBool = AtomicBool::new(true);

static FOUND: Mutex<Option<Newer>> = Mutex::new(None);

/// The newer release the last check found, if any.
pub fn found() -> Option<Newer> {
    FOUND.lock().unwrap().clone()
}

/// A newer release: its version ("0.2.0") and its page on GitHub.
#[derive(Clone, Debug, PartialEq)]
pub struct Newer {
    pub version: String,
    pub url: String,
}

/// "v0.2.0" or "0.2.0" as numbers; None for anything else.
fn parse(v: &str) -> Option<(u32, u32, u32)> {
    let mut parts = v.trim().trim_start_matches('v').splitn(3, '.').map(|p| p.parse::<u32>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// The release in GitHub's answer if it's newer than `current`.
fn newer_in(json: &serde_json::Value, current: &str) -> Option<Newer> {
    if json["draft"].as_bool() == Some(true) || json["prerelease"].as_bool() == Some(true) {
        return None;
    }
    let tag = json["tag_name"].as_str()?;
    let url = json["html_url"].as_str().filter(|u| u.starts_with("https://github.com/ItsFlamme/replay-vault/"))?;
    (parse(tag)? > parse(current)?).then(|| Newer { version: tag.trim_start_matches('v').to_string(), url: url.to_string() })
}

#[cfg(test)]
pub fn set_found(n: Option<Newer>) {
    *FOUND.lock().unwrap() = n;
}

/// Asks GitHub once. No release yet (or the repository isn't public) is Ok(None).
pub fn check() -> anyhow::Result<Option<Newer>> {
    let resp = ureq::get(LATEST)
        .header("User-Agent", concat!("replay-vault/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .config()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .call()?;
    match resp.status().as_u16() {
        200 => {}
        404 => return Ok(None),
        s => anyhow::bail!("GitHub answered {s}"),
    }
    let json: serde_json::Value = serde_json::from_str(&resp.into_body().read_to_string()?)?;
    Ok(newer_in(&json, env!("CARGO_PKG_VERSION")))
}

/// Checks a minute after start, then once a day while `ENABLED`; `repaint` runs when a newer
/// release turns up, so the window shows it.
pub fn start(repaint: impl Fn() + Send + 'static) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(60));
        loop {
            if ENABLED.load(Ordering::Relaxed) {
                match check() {
                    Ok(Some(n)) => {
                        crate::log::write(&format!("Replay Vault {} is out: {}", n.version, n.url));
                        *FOUND.lock().unwrap() = Some(n);
                        repaint();
                    }
                    Ok(None) => {}
                    Err(e) => crate::log::write(&format!("Couldn't check for a new Replay Vault version: {e:#}")),
                }
            }
            std::thread::sleep(Duration::from_secs(24 * 3600));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn release(tag: &str) -> serde_json::Value {
        json!({ "tag_name": tag, "html_url": format!("https://github.com/ItsFlamme/replay-vault/releases/tag/{tag}"), "draft": false, "prerelease": false })
    }

    #[test]
    fn only_newer_releases_count() {
        assert_eq!(newer_in(&release("v0.2.0"), "0.1.0").map(|n| n.version), Some("0.2.0".into()));
        assert_eq!(newer_in(&release("v0.10.0"), "0.9.3").map(|n| n.version), Some("0.10.0".into()), "numbers, not text");
        assert!(newer_in(&release("v0.1.0"), "0.1.0").is_none(), "same version");
        assert!(newer_in(&release("v0.0.9"), "0.1.0").is_none(), "older");
        assert!(newer_in(&release("nightly"), "0.1.0").is_none(), "not a version");
        let mut pre = release("v0.2.0");
        pre["prerelease"] = json!(true);
        assert!(newer_in(&pre, "0.1.0").is_none(), "pre-releases don't count");
        let mut elsewhere = release("v0.2.0");
        elsewhere["html_url"] = json!("https://example.com/download");
        assert!(newer_in(&elsewhere, "0.1.0").is_none(), "only ever link to this repository");
    }
}
