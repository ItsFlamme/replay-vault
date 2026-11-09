//! New versions of Replay Vault: once a day (or when asked), GitHub's latest published release is
//! compared with this version. Nothing about the user or the PC is sent (only the request itself,
//! like any web page). Drafts and pre-releases don't count. Installing one downloads the release's
//! installer, checks it against the SHA-256 GitHub published for it, and runs it.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::obs_install::{self, Shared};

const LATEST: &str = "https://api.github.com/repos/ItsFlamme/replay-vault/releases/latest";
const PAGES: &str = "https://github.com/ItsFlamme/replay-vault/";
const DOWNLOADS: &str = "https://github.com/ItsFlamme/replay-vault/releases/download/";

/// Whether to check, from the settings (General > Startup); read by the checking thread.
pub static ENABLED: AtomicBool = AtomicBool::new(true);

static FOUND: Mutex<Option<Newer>> = Mutex::new(None);
/// Goes up each time a check finds a newer release, so the popup can come back after "Later".
static FINDS: AtomicU32 = AtomicU32::new(0);
/// What the last "Check for updates" click found, for the General page.
static STATUS: Mutex<Option<String>> = Mutex::new(None);
static CHECKING: AtomicBool = AtomicBool::new(false);

/// The newer release the last check found, if any.
pub fn found() -> Option<Newer> {
    FOUND.lock().unwrap().clone()
}

/// How many times a check has found a newer release.
pub fn finds() -> u32 {
    FINDS.load(Ordering::Relaxed)
}

/// The answer to the last "Check for updates" click, or "Checking" while it runs.
pub fn status() -> Option<String> {
    STATUS.lock().unwrap().clone()
}

/// A newer release: its version ("0.2.0"), its page on GitHub, what's new in it, and its installer.
#[derive(Clone, Debug, PartialEq)]
pub struct Newer {
    pub version: String,
    pub url: String,
    pub notes: String,
    pub installer: Option<Installer>,
}

/// A release's installer: where to download it, its size and GitHub's SHA-256 of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Installer {
    pub url: String,
    pub size: u64,
    pub sha256: String,
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
    let url = json["html_url"].as_str().filter(|u| u.starts_with(PAGES))?;
    if parse(tag)? <= parse(current)? {
        return None;
    }
    let version = tag.trim_start_matches('v').to_string();
    // Only this release's own installer, from this repository, with a checksum to check it by.
    let name = format!("ReplayVault-{version}-setup.exe");
    let installer = json["assets"].as_array().into_iter().flatten().find(|a| a["name"].as_str() == Some(&name)).and_then(|a| {
        let url = a["browser_download_url"].as_str().filter(|u| u.starts_with(DOWNLOADS))?;
        let sha256 = a["digest"].as_str()?.strip_prefix("sha256:")?.to_lowercase();
        (sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit())).then(|| Installer { url: url.into(), size: a["size"].as_u64().unwrap_or(0), sha256 })
    });
    let notes = json["body"].as_str().unwrap_or_default().lines().take_while(|l| !l.starts_with("Check a download")).collect::<Vec<_>>().join("\n");
    Some(Newer { version, url: url.into(), notes: notes.trim().into(), installer })
}

/// For tests: what a check found. `finds` isn't counted, so the popup only shows in a test that
/// asks for it (tests run side by side and share this).
#[cfg(test)]
pub fn set_found(n: Option<Newer>) {
    *FOUND.lock().unwrap() = n;
}

/// Asks GitHub once. No release yet (or the repository isn't public) is Ok(None).
fn check() -> anyhow::Result<Option<Newer>> {
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

/// Checks once and keeps what it found; the answer in words.
fn check_and_keep() -> String {
    match check() {
        Ok(Some(n)) => {
            crate::log::write(&format!("Replay Vault {} is out: {}", n.version, n.url));
            let words = format!("Version {} is out", n.version);
            *FOUND.lock().unwrap() = Some(n);
            FINDS.fetch_add(1, Ordering::Relaxed);
            words
        }
        Ok(None) => {
            *FOUND.lock().unwrap() = None;
            format!("You have the latest version ({})", env!("CARGO_PKG_VERSION"))
        }
        Err(e) => {
            crate::log::write(&format!("Couldn't check for a new Replay Vault version: {e:#}"));
            "Couldn't reach GitHub. Try again later.".into()
        }
    }
}

/// Checks a minute after start, then once a day while `ENABLED`; `repaint` runs after each check.
/// Also clears away the installer of an update that has since been installed.
pub fn start(repaint: impl Fn() + Send + 'static) {
    std::thread::spawn(move || {
        let _ = fs::remove_file(installer_path());
        std::thread::sleep(Duration::from_secs(60));
        loop {
            if ENABLED.load(Ordering::Relaxed) {
                check_and_keep();
                repaint();
            }
            std::thread::sleep(Duration::from_secs(24 * 3600));
        }
    });
}

/// "Check for updates": checks now, whatever the setting, and says what it found in `status`.
pub fn check_now(repaint: impl Fn() + Send + 'static) {
    if CHECKING.swap(true, Ordering::Relaxed) {
        return;
    }
    *STATUS.lock().unwrap() = Some("Checking...".into());
    std::thread::spawn(move || {
        let words = check_and_keep();
        *STATUS.lock().unwrap() = Some(words);
        CHECKING.store(false, Ordering::Relaxed);
        repaint();
    });
}

/// Whether this copy was put in place by the installer (its uninstaller sits next to it), so an
/// update can be installed over it. The portable zip has none.
pub fn installed() -> bool {
    std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)).is_some_and(|d| installed_in(&d))
}

fn installed_in(dir: &Path) -> bool {
    dir.join("unins000.exe").exists()
}

/// Where a downloaded installer waits to be run: in the app's private folder, which no other
/// account can write to (see `obs_install::secure_install_root`).
pub fn installer_path() -> PathBuf {
    obs_install::install_root().join("replay-vault-setup.exe")
}

/// Downloads `inst` to `installer_path` on a thread, checking it against its SHA-256 as it comes.
pub fn download(inst: Installer, repaint: impl Fn() + Send + 'static) -> Shared {
    obs_install::spawn(move |p| fetch(&inst, &installer_path(), p), repaint)
}

fn fetch(inst: &Installer, dest: &Path, p: &Shared) -> anyhow::Result<()> {
    fs::create_dir_all(dest.parent().unwrap_or(dest))?;
    obs_install::secure_install_root()?;
    // No one else may write to it while it downloads, so what's checked is what gets run.
    let mut file = {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        File::options().write(true).create(true).truncate(true).share_mode(FILE_SHARE_READ).open(dest)?
    };
    let mut resp = obs_install::get(&inst.url, Duration::from_secs(3600))?;
    let mut reader = resp.body_mut().as_reader();
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        hash.update(&buf[..n]);
        done += n as u64;
        obs_install::set(p, "Downloading", done, inst.size);
    }
    drop(file);
    let got: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if got != inst.sha256 {
        let _ = fs::remove_file(dest);
        anyhow::bail!("the download didn't match its published checksum, so it was thrown away");
    }
    Ok(())
}

/// Starts the downloaded installer: it installs over this copy, keeping settings and clips, and
/// opens the new version when it's done. The app must quit right after.
pub fn run_installer() -> std::io::Result<()> {
    std::process::Command::new(installer_path()).args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART"]).spawn().map(|_| ())
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

    /// Only the release's own installer, from this repository and with a SHA-256, is downloaded.
    #[test]
    fn installer_must_be_ours_and_checked() {
        let sha = "ab".repeat(32);
        let asset = |name: &str, url: &str, digest: &str| json!({ "name": name, "browser_download_url": url, "size": 7, "digest": digest });
        let ours = "https://github.com/ItsFlamme/replay-vault/releases/download/v0.2.0/ReplayVault-0.2.0-setup.exe";
        let with = |assets: serde_json::Value| {
            let mut r = release("v0.2.0");
            r["assets"] = assets;
            r["body"] = json!("- Faster saves\n\nCheck a download with Get-FileHash");
            newer_in(&r, "0.1.0").unwrap()
        };
        let n = with(json!([asset("ReplayVault-0.2.0-portable.zip", ours, ""), asset("ReplayVault-0.2.0-setup.exe", ours, &format!("sha256:{sha}"))]));
        assert_eq!(n.installer, Some(Installer { url: ours.into(), size: 7, sha256: sha.clone() }));
        assert_eq!(n.notes, "- Faster saves", "the checksum line isn't shown as news");
        assert!(with(json!([asset("ReplayVault-0.2.0-setup.exe", "https://example.com/setup.exe", &format!("sha256:{sha}"))])).installer.is_none(), "another site");
        assert!(with(json!([asset("ReplayVault-0.2.0-setup.exe", ours, "")])).installer.is_none(), "no checksum");
        assert!(with(json!([asset("ReplayVault-0.1.9-setup.exe", ours, &format!("sha256:{sha}"))])).installer.is_none(), "another version's");
        assert!(with(json!([])).installer.is_none());
    }

    #[test]
    fn only_installed_copies_update_themselves() {
        let dir = std::env::temp_dir().join("replay-vault-installed-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert!(!installed_in(&dir), "portable");
        fs::write(dir.join("unins000.exe"), b"x").unwrap();
        assert!(installed_in(&dir));
        fs::remove_dir_all(&dir).unwrap();
    }
}
