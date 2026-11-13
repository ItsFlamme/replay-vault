//! New versions of Replay Vault: once a day (or when asked), GitHub's latest published release is
//! compared with this version. Nothing about the user or the PC is sent (only the request itself,
//! like any web page). Drafts and pre-releases don't count. Installing one downloads the release's
//! installer (or, for a portable copy, its exe), checks it against the SHA-256 GitHub published
//! for it, and runs it (or swaps it in and restarts). Only the app's own files are replaced, and
//! the old ones are deleted for good, never left in the Recycle Bin. Settings, notes, tags and
//! clips live elsewhere (`config::dir`, the clips folder) and are never touched. A copy that may not
//! update (see `instance`) never checks, downloads or installs.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::instance::UPDATE_CHECK_OFF;
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
/// The SHA-256 the last download matched, so a portable swap can check the file again.
static CHECKED: Mutex<Option<String>> = Mutex::new(None);

/// Whether this copy updates at all.
fn allowed() -> bool {
    crate::instance::current().updates_allowed()
}

/// The newer release the last check found, if any.
#[cfg(not(test))]
pub fn found() -> Option<Newer> {
    FOUND.lock().unwrap().clone()
}

/// For tests: what `set_found` set on this thread. Tests run side by side, each on its own thread,
/// and one showing the sidebar's "Version X is out" mustn't move another's status box between the
/// frame a click is aimed in and the frame it lands in. Tests never check GitHub.
#[cfg(test)]
pub fn found() -> Option<Newer> {
    FOUND_HERE.with(|f| f.borrow().clone())
}

#[cfg(test)]
thread_local! {
    static FOUND_HERE: std::cell::RefCell<Option<Newer>> = const { std::cell::RefCell::new(None) };
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
    /// The release's exe on its own, for portable copies.
    pub portable: Option<Installer>,
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
    let asset = |name: String| {
        json["assets"].as_array().into_iter().flatten().find(|a| a["name"].as_str() == Some(&name)).and_then(|a| {
            let url = a["browser_download_url"].as_str().filter(|u| u.starts_with(DOWNLOADS))?;
            let sha256 = a["digest"].as_str()?.strip_prefix("sha256:")?.to_lowercase();
            (sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit())).then(|| Installer { url: url.into(), size: a["size"].as_u64().unwrap_or(0), sha256 })
        })
    };
    let (installer, portable) = (asset(format!("ReplayVault-{version}-setup.exe")), asset(format!("ReplayVault-{version}.exe")));
    let notes = json["body"].as_str().unwrap_or_default().lines().take_while(|l| !l.starts_with("Check a download")).collect::<Vec<_>>().join("\n");
    Some(Newer { version, url: url.into(), notes: notes.trim().into(), installer, portable })
}

/// For tests: what a check found, for this thread only (see `found`). `finds` isn't counted, so the
/// popup only shows in a test that asks for it.
#[cfg(test)]
pub fn set_found(n: Option<Newer>) {
    FOUND_HERE.with(|f| *f.borrow_mut() = n);
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
/// Also deletes what an update left behind: its installer, or a portable copy's old exe. A copy
/// that may not update does neither: nothing is left behind.
pub fn start(repaint: impl Fn() + Send + 'static) {
    if !allowed() {
        return;
    }
    std::thread::spawn(move || {
        let _ = fs::remove_file(installer_path());
        if let Ok(exe) = std::env::current_exe() {
            // The old exe can take a moment to be let go of after the restart.
            for _ in 0..10 {
                let left = [beside(&exe, "old"), beside(&exe, "new")].into_iter().filter(|p| fs::remove_file(p).is_err() && p.exists()).count();
                if left == 0 {
                    break;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
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
    if !allowed() {
        *STATUS.lock().unwrap() = Some(UPDATE_CHECK_OFF.into());
        repaint();
        return;
    }
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

/// What to download for this copy: the installer for an installed one, the exe for a portable one.
pub fn pick(n: &Newer) -> Option<Installer> {
    if installed() { n.installer.clone() } else { n.portable.clone() }
}

/// `exe` with ".old" or ".new" after it: where a portable update parks the old exe and stages
/// the new one, beside it (so the swap is a rename on the same drive).
fn beside(exe: &Path, what: &str) -> PathBuf {
    let mut name = exe.as_os_str().to_owned();
    name.push(format!(".{what}"));
    PathBuf::from(name)
}

/// Downloads `inst` (from `pick`) on a thread, checking it against its SHA-256 as it comes: to
/// `installer_path`, or beside a portable copy's exe.
pub fn download(inst: Installer, repaint: impl Fn() + Send + 'static) -> Shared {
    let dest = match std::env::current_exe() {
        Ok(exe) if !installed() => beside(&exe, "new"),
        _ => installer_path(),
    };
    obs_install::spawn(
        move |p| {
            anyhow::ensure!(allowed(), "{UPDATE_CHECK_OFF}");
            fetch(&inst, &dest, p)
        },
        repaint,
    )
}

/// Puts the downloaded update in place; the app must quit right after. Installed: the installer
/// replaces the app's files. Portable: the new exe takes the old one's name (the old one, still
/// running, is parked beside it and deleted for good at the next start) and is started once this
/// copy has quit.
pub fn apply() -> std::io::Result<()> {
    if !allowed() {
        return Err(std::io::Error::other(UPDATE_CHECK_OFF));
    }
    if installed() {
        return run_installer();
    }
    let exe = std::env::current_exe()?;
    swap(&exe)?;
    restart(&exe)
}

/// Starts `exe` after a moment, once this copy has let go of the one-copy-at-a-time lock.
fn restart(exe: &Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let line = format!("ping -n 3 127.0.0.1 >nul & start \"\" \"{}\"", exe.display());
    std::process::Command::new("cmd").arg("/c").raw_arg(format!("\"{line}\"")).current_dir(std::env::temp_dir()).creation_flags(CREATE_NO_WINDOW).spawn().map(|_| ())
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
    // Cancelled, stalled or cut off: the part that came is thrown away.
    let downloaded = obs_install::get(&inst.url, None).map_err(anyhow::Error::from).and_then(|mut resp| obs_install::download_to(&mut resp.body_mut().as_reader(), &mut file, p, "Downloading", inst.size));
    drop(file);
    let hash = downloaded.inspect_err(|_| {
        let _ = fs::remove_file(dest);
    })?;
    let got = hex(hash);
    if got != inst.sha256 {
        let _ = fs::remove_file(dest);
        anyhow::bail!("the download didn't match its published checksum, so it was thrown away");
    }
    *CHECKED.lock().unwrap() = Some(got);
    Ok(())
}

fn hex(hash: Sha256) -> String {
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Puts `exe`.new in `exe`'s place, parking `exe` as `exe`.old; on failure `exe` is put back.
/// The new exe is hashed again through a handle no one else can write to, rename or delete, and
/// moved through that same handle, so what starts is what matched the download's checksum.
fn swap(exe: &Path) -> std::io::Result<()> {
    let (new, old) = (beside(exe, "new"), beside(exe, "old"));
    let mut file = {
        use std::os::windows::fs::OpenOptionsExt;
        const GENERIC_READ: u32 = 0x8000_0000;
        const DELETE: u32 = 0x0001_0000;
        const FILE_SHARE_READ: u32 = 1;
        File::options().access_mode(GENERIC_READ | DELETE).share_mode(FILE_SHARE_READ).open(&new)?
    };
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    let want = CHECKED.lock().unwrap().clone();
    if want.is_none() || want != Some(hex(hash)) {
        return Err(std::io::Error::other("the new version changed after it was checked, so it wasn't used"));
    }
    let _ = fs::remove_file(&old);
    fs::rename(exe, &old)?;
    if let Err(e) = rename_open(&file, exe) {
        let _ = fs::rename(&old, exe);
        return Err(e);
    }
    Ok(())
}

/// Renames the open `file` to `to` (a full path), through its handle.
fn rename_open(file: &File, to: &Path) -> std::io::Result<()> {
    use std::os::windows::{ffi::OsStrExt, io::AsRawHandle};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{FILE_RENAME_INFO, FileRenameInfo, SetFileInformationByHandle};
    let name: Vec<u16> = to.as_os_str().encode_wide().collect();
    // The struct ends with the first letter of the name; the rest follows it.
    let size = std::mem::size_of::<FILE_RENAME_INFO>() + name.len() * 2;
    let mut buf = vec![0u64; size.div_ceil(8)];
    let info = buf.as_mut_ptr() as *mut FILE_RENAME_INFO;
    unsafe {
        (*info).FileNameLength = (name.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(name.as_ptr(), std::ptr::addr_of_mut!((*info).FileName) as *mut u16, name.len());
        SetFileInformationByHandle(HANDLE(file.as_raw_handle()), FileRenameInfo, info as *const _, size as u32)?;
    }
    Ok(())
}

/// Starts the downloaded installer: it installs over this copy, keeping settings and clips, and
/// opens the new version when it's done. The app must quit right after.
fn run_installer() -> std::io::Result<()> {
    std::process::Command::new(installer_path()).args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART"]).spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn release(tag: &str) -> serde_json::Value {
        json!({ "tag_name": tag, "html_url": format!("https://github.com/ItsFlamme/replay-vault/releases/tag/{tag}"), "draft": false, "prerelease": false })
    }

    /// A release one test thread sets isn't seen by another, so one test's "Version X is out"
    /// can't move another's sidebar mid-click.
    #[test]
    fn found_is_per_test_thread() {
        let other = std::thread::spawn(|| {
            set_found(newer_in(&release("v9.0.0"), "0.1.0"));
            found().map(|n| n.version)
        });
        assert_eq!(other.join().unwrap(), Some("9.0.0".into()));
        assert!(found().is_none());
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
        // A portable copy's update: the release's exe, checked the same way.
        let exe = "https://github.com/ItsFlamme/replay-vault/releases/download/v0.2.0/ReplayVault-0.2.0.exe";
        let n = with(json!([asset("ReplayVault-0.2.0.exe", exe, &format!("sha256:{sha}")), asset("ReplayVault-0.2.0-setup.exe", ours, &format!("sha256:{sha}"))]));
        assert_eq!(n.portable, Some(Installer { url: exe.into(), size: 7, sha256: sha.clone() }));
        assert!(with(json!([asset("ReplayVault-0.2.0.exe", exe, "")])).portable.is_none(), "no checksum");
    }

    /// A portable update puts the new exe in the old one's place and parks the old one; if the
    /// new one can't be moved in, the old one goes back. The new one is used only if it still
    /// matches what the download was checked against.
    #[test]
    fn portable_swap() {
        let dir = std::env::temp_dir().join("replay-vault-swap-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("replay-vault.exe");
        fs::write(&exe, b"old").unwrap();
        let mut h = Sha256::new();
        h.update(b"new");
        *CHECKED.lock().unwrap() = Some(hex(h));
        assert!(swap(&exe).is_err(), "nothing downloaded");
        assert_eq!(fs::read(&exe).unwrap(), b"old", "put back");
        fs::write(beside(&exe, "new"), b"tampered").unwrap();
        assert!(swap(&exe).is_err(), "changed after the check");
        assert_eq!(fs::read(&exe).unwrap(), b"old");
        assert!(beside(&exe, "new").exists() && !beside(&exe, "old").exists(), "both left alone");
        fs::write(beside(&exe, "new"), b"new").unwrap();
        swap(&exe).unwrap();
        assert_eq!(fs::read(&exe).unwrap(), b"new");
        assert_eq!(fs::read(beside(&exe, "old")).unwrap(), b"old", "parked, deleted at the next start");
        assert!(!beside(&exe, "new").exists());
        *CHECKED.lock().unwrap() = None;
        fs::rename(beside(&exe, "old"), beside(&exe, "new")).unwrap();
        assert!(swap(&exe).is_err(), "nothing checked this run: not used");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// The real thing against GitHub, as a portable 0.1.1 would do it: finds the latest release,
    /// downloads its exe beside a stand-in exe, checks it, swaps it in, and the restart line starts
    /// a program in a folder with a space. A tampered checksum is refused and the file thrown away.
    /// Run by hand: `cargo test --release live_portable_update -- --ignored`.
    #[test]
    #[ignore]
    fn live_portable_update() {
        let dir = std::env::temp_dir().join("replay-vault live update");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("root")).unwrap();
        obs_install::set_install_root(dir.join("root").to_str().unwrap());
        let resp = ureq::get(LATEST).header("User-Agent", "replay-vault-test").header("Accept", "application/vnd.github+json").call().unwrap();
        let json: serde_json::Value = serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
        let n = newer_in(&json, "0.1.1").expect("a newer release than 0.1.1");
        assert!(n.installer.is_some(), "installer asset");
        let inst = n.portable.clone().expect("portable exe asset");
        let exe = dir.join("ReplayVault.exe");
        fs::write(&exe, b"old").unwrap();
        // Tampered: refused, nothing left behind.
        let bad = Installer { sha256: "0".repeat(64), ..inst.clone() };
        let p: Shared = Default::default();
        assert!(fetch(&bad, &beside(&exe, "new"), &p).is_err());
        assert!(!beside(&exe, "new").exists(), "a bad download is deleted");
        // The real one.
        fetch(&inst, &beside(&exe, "new"), &p).unwrap();
        assert_eq!(fs::metadata(beside(&exe, "new")).unwrap().len(), inst.size);
        swap(&exe).unwrap();
        assert!(fs::metadata(&exe).unwrap().len() > 1_000_000, "the new exe is in place");
        assert_eq!(fs::read(beside(&exe, "old")).unwrap(), b"old");
        // The restart line, on a script that leaves a mark (no window).
        let mark = dir.join("ran.txt");
        let vbs = dir.join("restart test.vbs");
        fs::write(&vbs, format!("CreateObject(\"Scripting.FileSystemObject\").CreateTextFile(\"{}\").Close", mark.display())).unwrap();
        restart(&vbs).unwrap();
        let started = (0..30).any(|_| {
            std::thread::sleep(Duration::from_millis(500));
            mark.exists()
        });
        assert!(started, "the restart line started the program");
        obs_install::set_install_root("");
        fs::remove_dir_all(&dir).unwrap();
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
