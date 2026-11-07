//! Finding an OBS the recorder can use, or downloading a private portable copy of the official
//! release (github.com/obsproject/obs-studio) and keeping that copy up to date. The user's own
//! OBS install is used when its folder is writable, and is never updated or changed by this app.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use crate::{recorder, startup};

const RELEASES: &str = "https://api.github.com/repos/obsproject/obs-studio/releases/latest";
const VC_REDIST: &str = "https://aka.ms/vs/17/release/vc_redist.x64.exe";
const AGENT: &str = "replay-vault";

/// %LOCALAPPDATA%\replay-vault. An OBS copy installed before the rename stays where it is
/// (obs-instant-replay): moving it would break the firewall rule and graphics setting that name its exe.
fn base() -> PathBuf {
    let root = dirs::data_local_dir().unwrap_or_default();
    let old = root.join("obs-instant-replay");
    if old.exists() { old } else { root.join("replay-vault") }
}

/// A folder the user picked for the private OBS (so it needn't sit on the system drive).
static INSTALL_ROOT: std::sync::RwLock<Option<PathBuf>> = std::sync::RwLock::new(None);

/// Sets where the private OBS lives; empty means the default under %LOCALAPPDATA%.
pub fn set_install_root(dir: &str) {
    *INSTALL_ROOT.write().unwrap() = (!dir.trim().is_empty()).then(|| PathBuf::from(dir.trim()));
}

/// The folder holding the private OBS, its pending update and the download.
pub fn install_root() -> PathBuf {
    INSTALL_ROOT.read().unwrap().clone().unwrap_or_else(base)
}

/// Our private copy, and where a downloaded update waits until the next start.
pub fn private_dir() -> PathBuf {
    install_root().join("obs-studio")
}

fn staged_dir() -> PathBuf {
    install_root().join("obs-studio.update")
}

fn exe_in(dir: &Path) -> PathBuf {
    dir.join(r"bin\64bit\obs64.exe")
}

/// The install folder of an OBS that runs in portable mode (OBS checks for these marker files
/// there), whose own settings then live in `<install>\config` — where the recorder writes.
pub fn portable_install_root(exe: &Path) -> Option<PathBuf> {
    let root = exe.ancestors().nth(3)?;
    ["portable_mode", "obs_portable_mode", "portable_mode.txt", "obs_portable_mode.txt"]
        .iter()
        .any(|m| root.join(m).exists())
        .then(|| root.to_path_buf())
}

/// The OBS the recorder runs: the user's install if the recorder can keep its config next to it,
/// else our private copy. A portable-mode install is never used: its own settings live in the
/// same folder the recorder rewrites on every start.
pub fn find() -> Option<PathBuf> {
    if let Some(exe) = startup::obs_exe().filter(|e| writable(e) && portable_install_root(e).is_none()) {
        return Some(exe);
    }
    let exe = exe_in(&private_dir());
    exe.exists().then_some(exe)
}

pub fn is_private(exe: &Path) -> bool {
    exe.starts_with(private_dir())
}

/// Program Files installs aren't writable without admin, so the recorder can't use them.
fn writable(exe: &Path) -> bool {
    let root = recorder::portable_root(exe);
    let probe = root.join(".write-test");
    let ok = fs::create_dir_all(&root).is_ok() && fs::write(&probe, b"").is_ok();
    let _ = fs::remove_file(probe);
    ok
}

/// OBS needs Microsoft's Visual C++ runtime, which the portable zip doesn't include.
pub fn has_vc_runtime() -> bool {
    let sys = PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into())).join("System32");
    sys.join("msvcp140.dll").exists() && sys.join("vcruntime140_1.dll").exists()
}

/// Version of our private copy ("33.0.1"), written when it was unpacked.
pub fn private_version() -> Option<String> {
    fs::read_to_string(private_dir().join("version.txt")).ok().map(|s| s.trim().to_string())
}

/// OBS's own version stamp for global.ini (major << 24 | minor << 16 | patch), so a fresh copy
/// doesn't think it was just updated.
pub fn last_version_stamp(version: &str) -> Option<u64> {
    let mut parts = version.split('.').map(|p| p.parse::<u64>().ok());
    let (major, minor, patch) = (parts.next()??, parts.next().flatten().unwrap_or(0), parts.next().flatten().unwrap_or(0));
    Some(major << 24 | minor << 16 | patch)
}

/// Progress of a background job, polled by the UI.
#[derive(Default, Clone)]
pub struct Progress {
    pub stage: String,
    pub done: u64,
    pub total: u64,
    /// `Some` once finished: `Ok` or the error to show.
    pub result: Option<Result<(), String>>,
}

pub type Shared = Arc<Mutex<Progress>>;

fn set(p: &Shared, stage: &str, done: u64, total: u64) {
    let mut g = p.lock().unwrap();
    g.stage = stage.into();
    g.done = done;
    g.total = total;
}

/// Runs `job` on a thread, reporting into a fresh progress handle.
fn spawn(job: impl FnOnce(&Shared) -> anyhow::Result<()> + Send + 'static, repaint: impl Fn() + Send + 'static) -> Shared {
    let p: Shared = Arc::default();
    let p2 = p.clone();
    std::thread::spawn(move || {
        let r = job(&p2).map_err(|e| format!("{e:#}"));
        p2.lock().unwrap().result = Some(r);
        repaint();
    });
    p
}

struct Release {
    version: String,
    url: String,
    size: u64,
    sha256: String,
}

/// GET with timeouts, so a dead connection fails with a message instead of hanging setup forever.
/// ody caps the whole download (the OBS zip gets long enough for slow links).
fn get(url: &str, body: Duration) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    ureq::get(url)
        .header("User-Agent", AGENT)
        .config()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(body))
        .build()
        .call()
}

fn latest_release() -> anyhow::Result<Release> {
    let mut resp = get(RELEASES, Duration::from_secs(60))?;
    let json: serde_json::Value = serde_json::from_str(&resp.body_mut().read_to_string()?)?;
    let version = json["tag_name"].as_str().unwrap_or_default().to_string();
    let asset = json["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"].as_str().is_some_and(|n| n.ends_with("-Windows-x64.zip")))
        .ok_or_else(|| anyhow::anyhow!("the latest OBS release has no Windows x64 download"))?;
    let sha256 = asset["digest"]
        .as_str()
        .and_then(|d| d.strip_prefix("sha256:"))
        .ok_or_else(|| anyhow::anyhow!("the OBS download has no published checksum, refusing to use it"))?
        .to_lowercase();
    Ok(Release {
        version,
        url: asset["browser_download_url"].as_str().unwrap_or_default().to_string(),
        size: asset["size"].as_u64().unwrap_or(0),
        sha256,
    })
}

/// Downloads the release zip, checks its SHA-256 against GitHub's, and unpacks it into `dest`.
fn fetch(rel: &Release, dest: &Path, p: &Shared) -> anyhow::Result<()> {
    // Downloaded next to the install, so a chosen drive also takes the 225 MB zip.
    fs::create_dir_all(install_root())?;
    let zip_path = install_root().join("obs-download.zip");
    {
        let mut resp = get(&rel.url, Duration::from_secs(2 * 3600))?;
        let mut reader = resp.body_mut().as_reader();
        let mut file = File::create(&zip_path)?;
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
            set(p, &format!("Downloading OBS {}", rel.version), done, rel.size);
        }
        let got: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if got != rel.sha256 {
            let _ = fs::remove_file(&zip_path);
            anyhow::bail!("the OBS download didn't match its published checksum, so it was thrown away");
        }
    }

    let tmp = dest.with_extension("partial");
    let _ = fs::remove_dir_all(&tmp);
    let mut zip = zip::ZipArchive::new(File::open(&zip_path)?)?;
    let total = zip.len() as u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        // enclosed_name rejects paths that would escape the folder.
        let Some(rel_path) = entry.enclosed_name() else { continue };
        let out = tmp.join(rel_path);
        if entry.is_dir() {
            fs::create_dir_all(&out)?;
        } else {
            fs::create_dir_all(out.parent().unwrap_or(&tmp))?;
            io::copy(&mut entry, &mut File::create(&out)?)?;
        }
        set(p, "Unpacking OBS", i as u64 + 1, total);
    }
    fs::write(tmp.join("version.txt"), &rel.version)?;
    let _ = fs::remove_dir_all(dest);
    fs::rename(&tmp, dest)?;
    let _ = fs::remove_file(&zip_path);
    Ok(())
}

/// First install of the private copy.
pub fn install(repaint: impl Fn() + Send + 'static) -> Shared {
    spawn(
        |p| {
            set(p, "Finding the latest OBS", 0, 0);
            let rel = latest_release().map_err(|e| anyhow::anyhow!("{e:#}. Check your internet connection"))?;
            fetch(&rel, &private_dir(), p)
        },
        repaint,
    )
}

/// Downloads and runs Microsoft's VC++ runtime installer; Windows asks the user for permission.
pub fn install_vc_runtime(repaint: impl Fn() + Send + 'static) -> Shared {
    spawn(
        |p| {
            set(p, "Downloading the Visual C++ runtime", 0, 0);
            // A new folder each time, so nothing can be swapped in at a known path before it runs.
            let dir = base().join(format!("vc-{}", crate::config::random_hex()));
            fs::create_dir_all(&dir)?;
            let path = dir.join("vc_redist.x64.exe");
            let mut resp = get(VC_REDIST, Duration::from_secs(30 * 60))?;
            io::copy(&mut resp.body_mut().as_reader(), &mut File::create(&path)?)?;
            if !signed_by_microsoft(&path) {
                let _ = fs::remove_dir_all(&dir);
                anyhow::bail!("the downloaded Visual C++ installer isn't signed by Microsoft, so it wasn't run");
            }
            set(p, "Installing the Visual C++ runtime", 0, 0);
            let status = Command::new(&path).args(["/install", "/passive", "/norestart"]).status()?;
            let _ = fs::remove_dir_all(&dir);
            // 3010 = installed, restart recommended; the DLLs are usable right away.
            if !status.success() && status.code() != Some(3010) && !has_vc_runtime() {
                anyhow::bail!("the Visual C++ runtime installer didn't finish (code {:?})", status.code());
            }
            Ok(())
        },
        repaint,
    )
}

/// Whether Windows says `path` has a valid signature from Microsoft Corporation, issued by
/// Microsoft: the organisation fields must be exactly that, not merely contain it.
fn signed_by_microsoft(path: &Path) -> bool {
    use std::os::windows::process::CommandExt;
    let script = "$s = Get-AuthenticodeSignature -LiteralPath $env:IR_CHECK; $o = '(^|, )O=Microsoft Corporation(,|$)'; \
                  if ($s.Status -eq 'Valid' -and $s.SignerCertificate.Subject -cmatch $o -and $s.SignerCertificate.Issuer -cmatch $o) { exit 0 } else { exit 1 }";
    Command::new(recorder::windows_exe("powershell.exe"))
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("IR_CHECK", path)
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .is_ok_and(|s| s.success())
}

const FIREWALL_RULE: &str = "Replay Vault recorder";
/// What the rule was called before the rename; still recognized and removed.
const OLD_FIREWALL_RULE: &str = "Instant Replay recorder";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Whether our firewall rule for the private copy is in place (readable without admin).
pub fn firewall_rule_exists() -> bool {
    rule_exists(FIREWALL_RULE) || rule_exists(OLD_FIREWALL_RULE)
}

fn rule_exists(name: &str) -> bool {
    use std::os::windows::process::CommandExt;
    Command::new(crate::recorder::windows_exe("netsh.exe"))
        .args(["advfirewall", "firewall", "show", "rule", &format!("name={name}")])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// OBS's WebSocket listens on every network interface and can't be limited to this PC, so a
/// fresh copy makes Windows Firewall ask about it. Blocking inbound network traffic for our
/// private copy stops that prompt and keeps it off the network; the app's own connection stays
/// on this PC, which the firewall doesn't filter. Windows asks for permission (UAC).
pub fn add_firewall_rule(exe: PathBuf, repaint: impl Fn() + Send + 'static) -> Shared {
    spawn(
        move |p| {
            set(p, "Waiting for Windows to allow the firewall rule", 0, 0);
            let args = format!(
                "advfirewall firewall add rule name=\"{FIREWALL_RULE}\" dir=in action=block enable=yes profile=any program=\"{}\"",
                exe.display()
            );
            let code = run_elevated(&crate::recorder::windows_exe("netsh.exe"), &args)?;
            if code != 0 || !firewall_rule_exists() {
                anyhow::bail!("the firewall rule wasn't added (netsh code {code})");
            }
            crate::log::write("Added a firewall rule blocking network access to the private OBS copy.");
            Ok(())
        },
        repaint,
    )
}

/// Takes our firewall rule away again (for Remove); Windows asks for permission (UAC).
pub fn remove_firewall_rule() -> anyhow::Result<()> {
    for name in [FIREWALL_RULE, OLD_FIREWALL_RULE].into_iter().filter(|n| rule_exists(n)) {
        let code = run_elevated(&crate::recorder::windows_exe("netsh.exe"), &format!("advfirewall firewall delete rule name=\"{name}\""))?;
        anyhow::ensure!(code == 0 && !rule_exists(name), "the firewall rule wasn't removed (netsh code {code})");
    }
    Ok(())
}

/// Runs a program with admin rights after the UAC prompt and returns its exit code.
/// `file` is a full path, and it runs from its own folder, so nothing planted in the current
/// folder can be picked up with admin rights.
fn run_elevated(file: &Path, args: &str) -> anyhow::Result<u32> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
    use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    use windows::core::{HSTRING, PCWSTR};

    let dir = HSTRING::from(file.parent().unwrap_or(file).as_os_str());
    let (verb, file, args) = (HSTRING::from("runas"), HSTRING::from(file.as_os_str()), HSTRING::from(args));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(args.as_ptr()),
        lpDirectory: PCWSTR(dir.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    // Fails with "canceled" if the user says no at the UAC prompt.
    unsafe { ShellExecuteExW(&mut info) }.map_err(|e| anyhow::anyhow!("Windows didn't allow it ({})", e.message()))?;
    let mut code = 0u32;
    unsafe {
        WaitForSingleObject(info.hProcess, INFINITE);
        let _ = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
    }
    Ok(code)
}

/// Swaps in a downloaded update. Call before the recorder starts.
pub fn apply_staged_update() -> Option<String> {
    let staged = staged_dir();
    let version = fs::read_to_string(staged.join("version.txt")).ok()?.trim().to_string();
    recorder::stop();
    // Same folder as the copy, so the renames never cross drives.
    let old = install_root().join("obs-studio.old");
    let _ = fs::remove_dir_all(&old);
    if fs::rename(private_dir(), &old).is_err() || fs::rename(&staged, private_dir()).is_err() {
        // Put things back the way they were if the swap failed half way.
        if !private_dir().exists() {
            let _ = fs::rename(&old, private_dir());
        }
        return None;
    }
    let _ = fs::remove_dir_all(&old);
    Some(version)
}

/// Background check, at most once a day, for a newer OBS than our private copy. A newer one
/// is downloaded and unpacked next to it and swapped in at the next app start, so a running
/// replay buffer is never cut off. Returns the version that was staged, if any.
pub fn check_for_update() -> anyhow::Result<Option<String>> {
    let Some(current) = private_version() else { return Ok(None) };
    let stamp = base().join("last-update-check");
    let recent = fs::metadata(&stamp)
        .and_then(|m| m.modified())
        .is_ok_and(|t| SystemTime::now().duration_since(t).unwrap_or_default() < Duration::from_secs(24 * 3600));
    if recent || staged_dir().join("version.txt").exists() {
        return Ok(None);
    }
    let rel = latest_release()?;
    fs::write(&stamp, b"")?;
    let newer = match (last_version_stamp(&rel.version), last_version_stamp(&current)) {
        (Some(new), Some(cur)) => new > cur,
        _ => false,
    };
    if !newer {
        return Ok(None);
    }
    fetch(&rel, &staged_dir(), &Arc::default())?;
    Ok(Some(rel.version))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real download into the private folder: `cargo test -- --ignored download`.
    #[test]
    #[ignore]
    fn download_private_copy() {
        let p = install(|| {});
        let result = loop {
            if let Some(r) = p.lock().unwrap().result.clone() {
                break r;
            }
            std::thread::sleep(Duration::from_millis(200));
        };
        assert_eq!(result, Ok(()));
        assert!(exe_in(&private_dir()).exists());
        assert!(private_version().is_some());
        // Same version as the latest release, so nothing gets staged.
        let _ = fs::remove_file(base().join("last-update-check"));
        assert_eq!(check_for_update().unwrap(), None);
    }

    /// A Windows program passes the installer's signature check; a file we just wrote doesn't.
    #[test]
    fn signature_check() {
        assert!(signed_by_microsoft(&recorder::windows_exe("netsh.exe")));
        let fake = std::env::temp_dir().join("instant-replay-unsigned.exe");
        fs::write(&fake, b"MZ not really").unwrap();
        assert!(!signed_by_microsoft(&fake));
        let _ = fs::remove_file(&fake);
    }

    /// netsh answers "no rule" without admin rights (our rule isn't on a dev PC until added).
    #[test]
    fn firewall_check_runs_without_admin() {
        let started = std::time::Instant::now();
        let _ = firewall_rule_exists();
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn chosen_install_folder() {
        set_install_root(r"E:\Games\Instant Replay OBS");
        assert_eq!(private_dir(), PathBuf::from(r"E:\Games\Instant Replay OBS\obs-studio"));
        assert_eq!(exe_in(&private_dir()), PathBuf::from(r"E:\Games\Instant Replay OBS\obs-studio\bin\64bit\obs64.exe"));
        assert!(is_private(&exe_in(&private_dir())));
        set_install_root("  ");
        assert_eq!(install_root(), base());
    }

    #[test]
    fn version_stamps() {
        assert_eq!(last_version_stamp("33.0.1"), Some(553648129));
        assert!(last_version_stamp("33.1.0") > last_version_stamp("33.0.9"));
        assert_eq!(last_version_stamp("junk"), None);
    }
}
