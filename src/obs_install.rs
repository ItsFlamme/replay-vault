//! Finding an OBS the recorder can use, or downloading a private portable copy of the official
//! release (github.com/obsproject/obs-studio) and keeping that copy up to date. The recorder only
//! ever runs that private copy; the user's own OBS install is never run, updated or changed.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use crate::recorder;

const RELEASES: &str = "https://api.github.com/repos/obsproject/obs-studio/releases/latest";
const VC_REDIST: &str = "https://aka.ms/vs/17/release/vc_redist.x64.exe";
const AGENT: &str = "replay-vault";

/// %LOCALAPPDATA%\replay-vault. An OBS copy installed before the rename stays where it is
/// (obs-instant-replay): moving it would break the firewall rule and graphics setting that name its exe.
fn base() -> PathBuf {
    if let Some(d) = crate::config::local_data() {
        return d.join("obs");
    }
    let root = dirs::data_local_dir().unwrap_or_default();
    let old = root.join("obs-instant-replay");
    if old.exists() { old } else { root.join("replay-vault") }
}

/// A folder the user picked for the private OBS (so it needn't sit on the system drive).
static INSTALL_ROOT: std::sync::RwLock<Option<PathBuf>> = std::sync::RwLock::new(None);

/// Sets where the private OBS lives; empty means the default under %LOCALAPPDATA%. Ignored by a
/// copy that may not pick it (see `instance`).
pub fn set_install_root(dir: &str) {
    *INSTALL_ROOT.write().unwrap() = chosen_root(dir, crate::instance::current().obs_dir_allowed());
}

/// The folder picked for the private OBS, if one was and this copy may use it.
fn chosen_root(dir: &str, allowed: bool) -> Option<PathBuf> {
    (allowed && !dir.trim().is_empty()).then(|| PathBuf::from(dir.trim()))
}

/// The folder holding the private OBS, its pending update and the download.
pub fn install_root() -> PathBuf {
    INSTALL_ROOT.read().unwrap().clone().unwrap_or_else(base)
}

/// Makes sure only this Windows account, SYSTEM and Administrators can change the install folder
/// (and everything in it), since this app runs the programs in it. A folder made on a second
/// drive takes that drive's permissions, which on Windows let every signed-in account change it;
/// those are replaced, here and in every file under it. Returns what was wrong if anything was
/// fixed; an error if it can't be made private (another account owns it, or the drive has no
/// permissions at all, like FAT32).
pub fn secure_install_root() -> anyhow::Result<Option<String>> {
    secure_dir(&install_root())
}

fn secure_dir(dir: &Path) -> anyhow::Result<Option<String>> {
    let trusted = trusted_sids()?;
    let Some(problem) = acl_problem(dir, &trusted)? else { return Ok(None) };
    // Only inheriting from a private folder above (like a new folder in the user's profile) is
    // harmless. If another account could get in, it could have planted programs, and a file it
    // owns stays changeable by it whatever the permissions say.
    // Every file and folder counts: one with its own open permissions inside a private folder
    // would let another account in just the same.
    let exposed = tree_exposed(dir, &trusted);
    set_dacl(dir, &format!("D:P(A;OICI;FA;;;{})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)", trusted[0]), true)?;
    if let Some(still) = acl_problem(dir, &trusted)? {
        anyhow::bail!("{} can't be kept private: {still}", dir.display());
    }
    if !exposed {
        return Ok(Some(problem));
    }
    // So nothing in it is run again: the OBS copy is downloaded and checked afresh.
    for name in ["obs-studio", "obs-studio.update", "obs-studio.old", "obs-studio.partial", "obs-download.zip"] {
        let p = dir.join(name);
        let r = if p.is_dir() { fs::remove_dir_all(&p) } else { fs::remove_file(&p) };
        if let Err(e) = r
            && e.kind() != io::ErrorKind::NotFound
        {
            anyhow::bail!("couldn't remove {} after other accounts could change it: {e}", p.display());
        }
    }
    Ok(Some(format!("{problem}; removed the OBS copy in it, so it's downloaded again")))
}

/// Whether another account owns, or may get into, `path` or anything under it. Anything that can't
/// be read counts as exposed.
fn tree_exposed(path: &Path, trusted: &[String]) -> bool {
    let open = read_acl(path).map_or(true, |acl| {
        acl.is_none_or(|(owner, _, aces)| !trusted.contains(&owner) || aces.iter().any(|(allowed, sid)| *allowed != Some(false) && !trusted.contains(sid)))
    });
    open || (path.is_dir() && fs::read_dir(path).map_or(true, |items| items.flatten().any(|e| tree_exposed(&e.path(), trusted))))
}

/// This user's SID, then SYSTEM and Administrators.
fn trusted_sids() -> anyhow::Result<[String; 3]> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    // u64s, so the TOKEN_USER in it is aligned.
    let mut buf = [0u64; 64];
    let mut len = 0u32;
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
        let r = GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), size_of_val(&buf) as u32, &mut len);
        let _ = CloseHandle(token);
        r?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        Ok([sid_string(user.User.Sid)?, "S-1-5-18".into(), "S-1-5-32-544".into()])
    }
}

/// "S-1-5-21-...".
unsafe fn sid_string(sid: windows::Win32::Security::PSID) -> anyhow::Result<String> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    let mut s = windows::core::PWSTR::null();
    unsafe {
        windows::Win32::Security::Authorization::ConvertSidToStringSidW(sid, &mut s)?;
        let out = s.to_string();
        LocalFree(Some(HLOCAL(s.0.cast())));
        Ok(out?)
    }
}

/// The folder's owner, whether it ignores the permissions of the folder above, and its permission
/// entries as (allowed, SID); allowed is false for a deny entry and None for any other kind.
/// None when the drive keeps no permissions.
type Acl = (String, bool, Vec<(Option<bool>, String)>);

fn read_acl(path: &Path) -> anyhow::Result<Option<Acl>> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce, GetSecurityDescriptorControl, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
        SE_DACL_PROTECTED,
    };
    let (mut owner, mut dacl, mut sd) = (PSID::default(), std::ptr::null_mut::<ACL>(), PSECURITY_DESCRIPTOR::default());
    let name = windows::core::HSTRING::from(path.as_os_str());
    unsafe {
        GetNamedSecurityInfoW(&name, SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION, Some(&mut owner), None, Some(&mut dacl), None, &mut sd)
            .ok()
            .map_err(|e| anyhow::anyhow!("couldn't read the permissions of {}: {}", path.display(), e.message()))?;
        let read = || -> anyhow::Result<Option<Acl>> {
            if dacl.is_null() || owner.is_invalid() {
                return Ok(None);
            }
            let (mut control, mut rev) = (0u16, 0u32);
            GetSecurityDescriptorControl(sd, &mut control, &mut rev)?;
            let mut aces = Vec::new();
            for i in 0..(*dacl).AceCount as u32 {
                let mut ace = std::ptr::null_mut();
                GetAce(dacl, i, &mut ace)?;
                // GetAce fills it on success; never read through an empty one regardless.
                anyhow::ensure!(!ace.is_null(), "permission entry {i} of {} is missing", path.display());
                let kind = (*(ace as *const ACE_HEADER)).AceType;
                // Allowed (0) and denied (1) entries have the same layout, the SID at SidStart.
                let sid = PSID((&raw const (*(ace as *const ACCESS_ALLOWED_ACE)).SidStart).cast_mut().cast());
                aces.push(match kind {
                    0 | 1 => (Some(kind == 0), sid_string(sid)?),
                    _ => (None, String::new()),
                });
            }
            Ok(Some((sid_string(owner)?, control & SE_DACL_PROTECTED.0 != 0, aces)))
        };
        let r = read();
        LocalFree(Some(HLOCAL(sd.0)));
        r
    }
}

/// What's wrong with `dir` for holding programs this app runs, or None when it's private.
fn acl_problem(dir: &Path, trusted: &[String]) -> anyhow::Result<Option<String>> {
    let Some((owner, protected, aces)) = read_acl(dir)? else { return Ok(Some("the drive keeps no permissions (not NTFS?)".into())) };
    if !trusted.contains(&owner) {
        return Ok(Some(format!("it belongs to another account ({owner})")));
    }
    if !protected {
        return Ok(Some("it takes its permissions from the folder above".into()));
    }
    Ok(aces.iter().find_map(|(allowed, sid)| match allowed {
        Some(false) => None,
        Some(true) if trusted.contains(sid) => None,
        Some(true) => Some(format!("{sid} can get in")),
        None => Some("it has an unusual permission entry".into()),
    }))
}

/// Sets `dir`'s permissions from SDDL; `tree` also resets everything under it to inherit them.
fn set_dacl(dir: &Path, sddl: &str, tree: bool) -> anyhow::Result<()> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, ProgressInvokeNever, SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW, TREE_SEC_INFO_RESET,
        TreeSetNamedSecurityInfoW,
    };
    use windows::Win32::Security::{ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};
    let mut sd = PSECURITY_DESCRIPTOR::default();
    let name = windows::core::HSTRING::from(dir.as_os_str());
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(&windows::core::HSTRING::from(sddl), SDDL_REVISION_1, &mut sd, None)?;
        let (mut present, mut defaulted, mut dacl) = (Default::default(), Default::default(), std::ptr::null_mut::<ACL>());
        let r = GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted).map_err(anyhow::Error::from).and_then(|()| {
            let info = if sddl.starts_with("D:P") { DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION } else { DACL_SECURITY_INFORMATION };
            let err = if tree {
                TreeSetNamedSecurityInfoW(&name, SE_FILE_OBJECT, info, None, None, Some(dacl), None, TREE_SEC_INFO_RESET, None, ProgressInvokeNever, None)
            } else {
                SetNamedSecurityInfoW(&name, SE_FILE_OBJECT, info, None, None, Some(dacl), None)
            };
            err.ok().map_err(|e| anyhow::anyhow!("couldn't set the permissions of {}: {}", dir.display(), e.message()))
        });
        LocalFree(Some(HLOCAL(sd.0)));
        r
    }
}

/// Our private copy, and where a downloaded update waits until the next start.
pub fn private_dir() -> PathBuf {
    install_root().join("obs-studio")
}

/// Moves what the app keeps in the install folder to the Recycle Bin (the private OBS, a waiting
/// update or download, the update stamp), then removes the folder if that left it empty. Only
/// these by name, so anything else in a folder the user picked stays. Returns what couldn't go.
pub fn recycle_install_root() -> Vec<String> {
    let root = install_root();
    if let Some(why) = crate::instance::current().install_root_kept(&root) {
        return vec![why];
    }
    let ours = [private_dir(), staged_dir(), root.join("obs-studio.old"), root.join("obs-studio.partial"), root.join("obs-download.zip"), update_stamp(), crate::app_update::installer_path()];
    let problems = ours
        .iter()
        .filter(|p| p.exists())
        .filter_map(|p| crate::clips::recycle(p).err().map(|e| format!("{}: {e:#}", p.display())))
        .collect();
    let _ = fs::remove_dir(&root);
    problems
}

fn staged_dir() -> PathBuf {
    install_root().join("obs-studio.update")
}

/// When the update check last ran; next to the private copy, so its folder is known to exist.
fn update_stamp() -> PathBuf {
    install_root().join("last-update-check")
}

fn exe_in(dir: &Path) -> PathBuf {
    dir.join(r"bin\64bit\obs64.exe")
}

/// The install folder of an OBS that runs in portable mode (OBS checks for these marker files
/// there), whose own settings then live in `<install>\config`, where the recorder writes.
pub fn portable_install_root(exe: &Path) -> Option<PathBuf> {
    let root = exe.ancestors().nth(3)?;
    ["portable_mode", "obs_portable_mode", "portable_mode.txt", "obs_portable_mode.txt"]
        .iter()
        .any(|m| root.join(m).exists())
        .then(|| root.to_path_buf())
}

/// The OBS the recorder runs: always our private copy. The user's own OBS install is never run
/// and nothing is ever written into its folder or its settings, so it can't be harmed.
pub fn find() -> Option<PathBuf> {
    let exe = exe_in(&private_dir());
    exe.exists().then_some(exe)
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
    /// Set by `cancel`: a download (`download_to`) stops at its next read and ends with `CANCELLED`.
    pub cancel: bool,
}

pub type Shared = Arc<Mutex<Progress>>;

/// The error a cancelled download ends with.
pub(crate) const CANCELLED: &str = "cancelled";

/// A download that gets nothing for this long stops, however long the whole thing takes on a slow link.
const STALL: Duration = Duration::from_secs(60);

/// The progress, even if a job panicked while holding it.
fn lock(p: &Shared) -> std::sync::MutexGuard<'_, Progress> {
    p.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn set(p: &Shared, stage: &str, done: u64, total: u64) {
    let mut g = lock(p);
    g.stage = stage.into();
    g.done = done;
    g.total = total;
}

/// Asks the job's download to stop; its `result` comes once it has (`CANCELLED`, partial file removed).
pub(crate) fn cancel(p: &Shared) {
    lock(p).cancel = true;
}

/// Runs `job` on a thread, reporting into a fresh progress handle. A job that panics ends with an
/// error too, so whatever waits on it isn't left waiting.
pub(crate) fn spawn(job: impl FnOnce(&Shared) -> anyhow::Result<()> + Send + 'static, repaint: impl Fn() + Send + 'static) -> Shared {
    let p: Shared = Arc::default();
    let p2 = p.clone();
    std::thread::spawn(move || {
        let r = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&p2))) {
            Ok(r) => r.map_err(|e| format!("{e:#}")),
            Err(_) => Err("it stopped unexpectedly (the log has details)".into()),
        };
        lock(&p2).result = Some(r);
        repaint();
    });
    p
}

/// Copies a download from `reader` into `file` as it comes, reporting `stage` and how far it got
/// out of `total` into `p`, and returns its SHA-256. Ends with `CANCELLED` once `cancel` was asked
/// for, and says so when the connection stalled (`get` gives up after `STALL` without a byte).
pub(crate) fn download_to(reader: &mut impl Read, file: &mut impl Write, p: &Shared, stage: &str, total: u64) -> anyhow::Result<Sha256> {
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    loop {
        if lock(p).cancel {
            anyhow::bail!(CANCELLED);
        }
        let n = match reader.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if stalled(&e) => anyhow::bail!("nothing came in for a minute, so the download stopped. Check your internet connection and try again"),
            Err(e) => return Err(e.into()),
        };
        if n == 0 {
            return Ok(hash);
        }
        file.write_all(&buf[..n])?;
        hash.update(&buf[..n]);
        done += n as u64;
        set(p, stage, done, total);
    }
}

/// Whether a read failed because nothing came in time: the socket's own timeout, or ureq's per-read one.
fn stalled(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::TimedOut || e.get_ref().and_then(|i| i.downcast_ref::<ureq::Error>()).is_some_and(|u| matches!(u, ureq::Error::Timeout(_)))
}

struct Release {
    version: String,
    url: String,
    size: u64,
    sha256: String,
}

/// GET with timeouts, so a dead connection fails with a message instead of hanging setup forever:
/// one that sends nothing for `STALL` stops. `body` caps the whole answer; None for a download,
/// which may take long on a slow link while it keeps coming.
pub(crate) fn get(url: &str, body: Option<Duration>) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    ureq::get(url)
        .header("User-Agent", AGENT)
        .config()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_per_read(Some(STALL))
        .timeout_recv_body(body)
        .build()
        .call()
}

fn latest_release() -> anyhow::Result<Release> {
    let mut resp = get(RELEASES, Some(Duration::from_secs(60)))?;
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
    if let Some(fixed) = secure_install_root()? {
        crate::log::write(&format!("Made the private OBS folder {} private to this account ({fixed}).", install_root().display()));
    }
    let zip_path = install_root().join("obs-download.zip");
    // Kept open from download to unpacking, and no one else may write to it meanwhile, so what's
    // unpacked is exactly the bytes whose checksum was checked.
    let mut file = {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        File::options().read(true).write(true).create(true).truncate(true).share_mode(FILE_SHARE_READ).open(&zip_path)?
    };
    {
        let downloaded = get(&rel.url, None).map_err(anyhow::Error::from).and_then(|mut resp| download_to(&mut resp.body_mut().as_reader(), &mut file, p, &format!("Downloading OBS {}", rel.version), rel.size));
        let hash = match downloaded {
            Ok(h) => h,
            Err(e) => {
                drop(file);
                let _ = fs::remove_file(&zip_path);
                return Err(e);
            }
        };
        let got: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if got != rel.sha256 {
            drop(file);
            let _ = fs::remove_file(&zip_path);
            anyhow::bail!("the OBS download didn't match its published checksum, so it was thrown away");
        }
    }

    let tmp = dest.with_extension("partial");
    let _ = fs::remove_dir_all(&tmp);
    io::Seek::rewind(&mut file)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let total = zip.len() as u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        // enclosed_name rejects paths that would escape the folder.
        let Some(rel_path) = entry.enclosed_name().filter(|r| !unused(r)) else { continue };
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
    drop(zip);
    let _ = fs::remove_file(&zip_path);
    Ok(())
}

/// Everything setup needs in one go: the VC++ runtime if missing, the private OBS copy if there's
/// no OBS yet, and, if `firewall`, the rule that blocks incoming connections to our copy.
pub fn install(firewall: bool, repaint: impl Fn() + Send + 'static) -> Shared {
    spawn(
        move |p| {
            if !has_vc_runtime() {
                vc_runtime(p)?;
            }
            if find().is_none() {
                set(p, "Finding the latest OBS", 0, 0);
                let rel = latest_release().map_err(|e| anyhow::anyhow!("{e:#}. Check your internet connection"))?;
                fetch(&rel, &private_dir(), p)?;
            }
            if firewall && let Some(exe) = find() {
                firewall_rule(exe, p)?;
            }
            Ok(())
        },
        repaint,
    )
}

/// Downloads and runs Microsoft's VC++ runtime installer; Windows asks the user for permission.
fn vc_runtime(p: &Shared) -> anyhow::Result<()> {
    set(p, "Downloading the Visual C++ runtime", 0, 0);
    // A new folder each time, so nothing can be swapped in at a known path before it runs.
    let dir = base().join(format!("vc-{}", crate::config::random_hex()));
    fs::create_dir_all(&dir)?;
    let path = dir.join("vc_redist.x64.exe");
    let mut resp = get(VC_REDIST, None)?;
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
        // Started from PowerShell 7, its module path would be inherited and Windows PowerShell
        // would find 7's Security module first, which it can't load.
        .env_remove("PSModulePath")
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .is_ok_and(|s| s.success())
}

/// What the rule was called before the rename; still recognized and removed.
const OLD_FIREWALL_RULE: &str = "Instant Replay recorder";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Whether our firewall rule is in place for the private copy where it is now (readable without
/// admin). A rule left for another folder (OBS moved to another drive) doesn't count.
pub fn firewall_rule_exists() -> bool {
    rule_state(&exe_in(&private_dir())) == Rule::Ok
}

/// How our firewall rule stands for one OBS exe.
#[derive(Debug, PartialEq)]
enum Rule {
    Missing,
    Ok,
    /// There, under this name, but for another program path.
    Elsewhere(&'static str),
}

/// The rule names `inst` looks at and owns: its own first, then (if it may) the old one.
fn rule_names(inst: &crate::instance::Instance) -> Vec<&str> {
    names_of_rules(inst.firewall_rule(), inst.legacy_names_allowed())
}

/// `own`, then the old name if `legacy` (`rule_names`).
fn names_of_rules(own: &str, legacy: bool) -> Vec<&str> {
    let mut names = vec![own];
    if legacy {
        names.push(OLD_FIREWALL_RULE);
    }
    names
}

fn rule_state(exe: &Path) -> Rule {
    let mut other = None;
    for name in rule_names(crate::instance::current()) {
        let Some(listing) = show_rule(name) else { continue };
        if lists_program(&listing, exe) {
            return Rule::Ok;
        }
        other.get_or_insert(name);
    }
    other.map_or(Rule::Missing, Rule::Elsewhere)
}

fn rule_exists(name: &str) -> bool {
    show_rule(name).is_some()
}

/// netsh's full listing of the rules called `name`, or None when there are none.
fn show_rule(name: &str) -> Option<String> {
    use std::os::windows::process::CommandExt;
    let out = Command::new(crate::recorder::windows_exe("netsh.exe"))
        .args(["advfirewall", "firewall", "show", "rule", &format!("name={name}"), "verbose"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    out.status.success().then(|| oem_text(&out.stdout))
}

/// Whether a netsh rule listing has `exe` as a rule's program: a line whose value (after the first
/// colon, whatever the field is called in this Windows language) is its path, in any case.
fn lists_program(listing: &str, exe: &Path) -> bool {
    let exe = exe.to_string_lossy().to_lowercase();
    listing.lines().filter_map(|l| l.split_once(':')).any(|(_, v)| v.trim().to_lowercase() == exe)
}

/// netsh's output, which comes in the console's OEM code page, not UTF-8: a path with letters
/// outside ASCII (an accented user name) only compares right decoded.
fn oem_text(bytes: &[u8]) -> String {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MultiByteToWideChar(code_page: u32, flags: u32, s: *const u8, len: i32, out: *mut u16, out_len: i32) -> i32;
    }
    const CP_OEMCP: u32 = 1;
    let Ok(len) = i32::try_from(bytes.len()) else { return String::from_utf8_lossy(bytes).into_owned() };
    if bytes.is_ascii() {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    unsafe {
        let n = MultiByteToWideChar(CP_OEMCP, 0, bytes.as_ptr(), len, std::ptr::null_mut(), 0).max(0);
        let mut wide = vec![0u16; n as usize];
        let n = MultiByteToWideChar(CP_OEMCP, 0, bytes.as_ptr(), len, wide.as_mut_ptr(), n).max(0);
        String::from_utf16_lossy(&wide[..n as usize])
    }
}

/// OBS's WebSocket listens on every network interface and can't be limited to this PC, so a
/// fresh copy makes Windows Firewall ask about it. Blocking inbound network traffic for our
/// private copy stops that prompt and keeps other devices from connecting to it (outgoing
/// traffic isn't blocked); the app's own connection stays on this PC, which the firewall
/// doesn't filter. Windows asks for permission (UAC).
pub fn add_firewall_rule(exe: PathBuf, repaint: impl Fn() + Send + 'static) -> Shared {
    spawn(move |p| firewall_rule(exe, p), repaint)
}

/// Adds our rule for `exe`, or points it at `exe` if it's there for another path. Nothing to do,
/// and no prompt, when it's already right.
fn firewall_rule(exe: PathBuf, p: &Shared) -> anyhow::Result<()> {
    let (args, done) = match rule_state(&exe) {
        Rule::Ok => return Ok(()),
        Rule::Elsewhere(name) => (
            format!("advfirewall firewall set rule name=\"{name}\" new program=\"{}\"", exe.display()),
            "Pointed the firewall rule at the private OBS copy's new folder.",
        ),
        Rule::Missing => (
            format!("advfirewall firewall add rule name=\"{}\" dir=in action=block enable=yes profile=any program=\"{}\"", crate::instance::current().firewall_rule(), exe.display()),
            "Added a firewall rule blocking incoming connections to the private OBS copy.",
        ),
    };
    set(p, "Waiting for Windows to allow the firewall rule", 0, 0);
    let code = run_elevated(&crate::recorder::windows_exe("netsh.exe"), &args)?;
    if code != 0 || rule_state(&exe) != Rule::Ok {
        anyhow::bail!("the firewall rule wasn't added (netsh code {code})");
    }
    crate::log::write(done);
    Ok(())
}

/// Takes our firewall rule away again (for Remove); Windows asks for permission (UAC).
pub fn remove_firewall_rule() -> anyhow::Result<()> {
    for name in rule_names(crate::instance::current()).into_iter().filter(|n| rule_exists(n)) {
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

/// Whether `rel`, a path inside the OBS zip or our copy, is debug symbols (.pdb, about 80 MB in
/// OBS 33), which OBS never loads, so it's left out. OBS's own plugins all stay, even the ones the
/// recorder never uses (capture cards, the browser): OBS 32+ treats each `core\<plugin>` as
/// required and loads no plugin at all, encoders included, if one is missing (tested 2026-11-09).
fn unused(rel: &Path) -> bool {
    rel.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdb"))
}

/// Takes the debug symbols out of a copy unpacked before they were left out.
pub(crate) fn prune(dir: &Path) {
    fn walk(dir: &Path, freed: &mut u64) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let path = e.path();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                walk(&path, freed);
            } else if unused(&path) {
                let size = e.metadata().map_or(0, |m| m.len());
                if fs::remove_file(&path).is_ok() {
                    *freed += size;
                }
            }
        }
    }
    let mut freed = 0;
    // Only where the .pdb files live, so a start doesn't walk OBS's thousands of data files.
    for sub in ["core", "bin", "obs-plugins"] {
        walk(&dir.join(sub), &mut freed);
    }
    if freed > 0 {
        crate::log::write(&format!("Removed OBS's debug files, which it never loads ({} MB).", freed >> 20));
    }
}

/// Swaps in a downloaded update, and takes OBS's debug files out of our copy (`prune`). Call
/// before the recorder starts.
pub fn apply_staged_update() -> Option<String> {
    prune(&private_dir());
    // The last swap's old copy, if a file in it was still in use then: deleted for good, not
    // recycled.
    let _ = fs::remove_dir_all(install_root().join("obs-studio.old"));
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
    let stamp = update_stamp();
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

    /// Only debug symbols are taken out of an OBS copy; every plugin stays (OBS needs them all).
    #[test]
    fn prune_takes_only_debug_files() {
        let dir = std::env::temp_dir().join("replay-vault-prune-test");
        let _ = fs::remove_dir_all(&dir);
        let kept = [r"bin\64bit\obs64.exe", r"core\obs-browser\libcef.dll", r"core\aja\aja.dll", r"core\win-capture\win-capture.dll", r"data\obs-studio\x.pdb"];
        let gone = [r"bin\64bit\obs64.pdb", r"core\win-capture\win-capture.PDB"];
        for f in kept.iter().chain(&gone) {
            fs::create_dir_all(dir.join(f).parent().unwrap()).unwrap();
            fs::write(dir.join(f), b"x").unwrap();
        }
        prune(&dir);
        assert!(kept.iter().all(|f| dir.join(f).exists()), "plugins and data stay");
        assert!(gone.iter().all(|f| !dir.join(f).exists()), "debug files go");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A download is copied and hashed as it comes; Cancel stops it at the next read; a connection
    /// that stalls says so; any other read error is passed on as it is.
    #[test]
    fn download_to_cancels_and_reports_a_stall() {
        let p: Shared = Arc::default();
        let mut out = Vec::new();
        let hash = download_to(&mut &b"hello"[..], &mut out, &p, "Downloading", 5).unwrap();
        assert_eq!(out, b"hello");
        assert_eq!(hash.finalize()[..], Sha256::digest(b"hello")[..]);
        let got = lock(&p).clone();
        assert_eq!((got.done, got.total), (5, 5));

        // Cancel pressed while the first chunk came in: no more is read.
        struct Then<'a>(&'a Shared, u32);
        impl Read for Then<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.1 += 1;
                cancel(self.0);
                buf[0] = 1;
                Ok(1)
            }
        }
        let p: Shared = Arc::default();
        let mut reader = Then(&p, 0);
        let e = download_to(&mut reader, &mut Vec::new(), &p, "Downloading", 10).unwrap_err();
        assert_eq!(e.to_string(), CANCELLED);
        assert_eq!(reader.1, 1, "stopped at the next read");

        // Stalled: the socket's timeout, or ureq's per-read one wrapped in an io error.
        struct Fails(fn() -> io::Error);
        impl Read for Fails {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err((self.0)())
            }
        }
        for e in [|| io::Error::from(io::ErrorKind::TimedOut), || ureq::Error::Timeout(ureq::Timeout::PerRead).into_io()] {
            let msg = download_to(&mut Fails(e), &mut Vec::new(), &Arc::default(), "Downloading", 10).unwrap_err().to_string();
            assert!(msg.starts_with("nothing came in for a minute"), "{msg}");
        }
        let msg = download_to(&mut Fails(|| io::Error::from(io::ErrorKind::ConnectionReset)), &mut Vec::new(), &Arc::default(), "Downloading", 10).unwrap_err().to_string();
        assert!(!msg.starts_with("nothing came in"), "{msg}");
    }

    /// A job that panics still ends, with an error, so the dialog waiting on it closes.
    #[test]
    fn a_panicking_job_still_ends() {
        let p = spawn(|_| panic!("test panic"), || {});
        let result = (0..100).find_map(|_| {
            std::thread::sleep(Duration::from_millis(20));
            lock(&p).result.clone()
        });
        assert!(result.is_some_and(|r| r.is_err()));
    }

    /// Real download into the private folder: `cargo test -- --ignored download`.
    #[test]
    #[ignore]
    fn download_private_copy() {
        let p = install(false, || {});
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
        let _ = fs::remove_file(update_stamp());
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

    /// The rule counts only for the program it names; netsh's field names change with the Windows
    /// language and its path's case can differ.
    #[test]
    fn firewall_rule_program_path() {
        let exe = Path::new(r"D:\Videos\Replay Vault OBS\obs-studio\bin\64bit\obs64.exe");
        let listing = "\r\nRule Name:                            Replay Vault recorder\r\n----------\r\nEnabled:                              Yes\r\n\
                       Direction:                            In\r\nProgram:                              D:\\Videos\\Replay Vault OBS\\obs-studio\\bin\\64bit\\obs64.exe\r\n\
                       Action:                               Block\r\nOk.\r\n";
        assert!(lists_program(listing, exe));
        assert!(lists_program(&listing.to_uppercase().replace("PROGRAM", "Programm"), exe));
        assert!(!lists_program(listing, Path::new(r"C:\Users\x\AppData\Local\replay-vault\obs-studio\bin\64bit\obs64.exe")));
        assert!(!lists_program("No rules match the specified criteria.\r\n", exe));
        assert_eq!(oem_text(br"Program: C:\a.exe"), r"Program: C:\a.exe");
        // Not ASCII: decoded as text, not dropped.
        assert_eq!(oem_text(&[b'a', 0x82]).chars().count(), 2);
    }

    /// This copy keeps its rule's name and still finds the old one; without old names it has only
    /// its own. netsh gets the name as one argument (show) or in quotes
    /// (add, set, delete), so spaces and brackets in a name stay part of it.
    #[test]
    fn firewall_rule_names() {
        assert_eq!(rule_names(crate::instance::current()), ["Replay Vault recorder", "Instant Replay recorder"]);
        assert_eq!(names_of_rules("Replay Vault recorder (2)", false), ["Replay Vault recorder (2)"]);
    }

    /// Tests that change the install folder take turns, since it's one global.
    static ROOT_LOCK: Mutex<()> = Mutex::new(());

    /// With a chosen folder the default one under %LOCALAPPDATA% may not exist, so the update
    /// check's stamp has to go next to the private copy, or writing it fails every start.
    #[test]
    fn update_stamp_in_chosen_folder() {
        let _g = ROOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("replay-vault-root-{}", crate::config::random_hex()));
        fs::create_dir_all(&root).unwrap();
        set_install_root(root.to_str().unwrap());
        let stamp = update_stamp();
        let wrote = fs::write(&stamp, b"");
        set_install_root("");
        let _ = fs::remove_dir_all(&root);
        assert_eq!(stamp, root.join("last-update-check"));
        assert!(wrote.is_ok(), "{wrote:?}");
    }

    #[test]
    fn chosen_install_folder() {
        let _g = ROOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_install_root(r"E:\Games\Instant Replay OBS");
        assert_eq!(private_dir(), PathBuf::from(r"E:\Games\Instant Replay OBS\obs-studio"));
        assert_eq!(exe_in(&private_dir()), PathBuf::from(r"E:\Games\Instant Replay OBS\obs-studio\bin\64bit\obs64.exe"));
        set_install_root("  ");
        assert_eq!(install_root(), base());
    }

    /// A copy that may not pick its OBS folder never gets one, whatever is saved.
    #[test]
    fn obs_dir_ignored_when_not_allowed() {
        assert_eq!(chosen_root(r"E:\Games\Replay Vault OBS", true), Some(PathBuf::from(r"E:\Games\Replay Vault OBS")));
        assert_eq!(chosen_root(" ", true), None);
        assert_eq!(chosen_root(r"E:\Games\Replay Vault OBS", false), None);
    }

    /// A folder open to every signed-in account (like one on a second drive), with a file in it,
    /// is made private: no Authenticated Users, Users or Everyone entry left on it or the file,
    /// and nothing taken from the folder above. In a temp folder; skipped where there are no
    /// permissions (not NTFS).
    #[test]
    fn install_folder_made_private() {
        let root = std::env::temp_dir().join(format!("replay-vault-acl-{}", crate::config::random_hex()));
        fs::create_dir_all(root.join("obs-studio")).unwrap();
        fs::write(root.join(r"obs-studio\obs64.exe"), b"x").unwrap();
        let trusted = trusted_sids().unwrap();
        if read_acl(&root).unwrap().is_none() {
            eprintln!("no permissions on this drive, skipped");
            let _ = fs::remove_dir_all(&root);
            return;
        }
        // What a second drive gives a new folder: Authenticated Users may modify, inherited.
        set_dacl(&root, &format!("D:(A;OICI;0x1301bf;;;AU)(A;OICI;0x1301bf;;;BU)(A;OICI;FA;;;{})", trusted[0]), true).unwrap();
        let open = |p: &Path| read_acl(p).unwrap().unwrap().2.iter().any(|(a, s)| *a == Some(true) && ["S-1-5-11", "S-1-5-32-545", "S-1-1-0"].contains(&s.as_str()));
        assert!(open(&root) && open(&root.join(r"obs-studio\obs64.exe")), "the test setup didn't take");
        assert!(acl_problem(&root, &trusted).unwrap().is_some());

        fs::write(root.join("keep.txt"), b"x").unwrap();
        let fixed = secure_dir(&root).unwrap();
        assert!(fixed.is_some_and(|f| f.contains("downloaded again")));
        assert_eq!(acl_problem(&root, &trusted).unwrap(), None);
        let (_, protected, _) = read_acl(&root).unwrap().unwrap();
        assert!(protected, "nothing comes from the folder above");
        assert!(!root.join("obs-studio").exists(), "an OBS copy others could change is never run again");
        for p in [root.clone(), root.join("keep.txt")] {
            assert!(!open(&p), "{} is still open to other accounts", p.display());
        }
        assert_eq!(secure_dir(&root).unwrap(), None, "already private: nothing to do");
        let _ = fs::remove_dir_all(&root);
    }

    /// A folder that only inherits private permissions (a new folder in the user's profile) is
    /// made private on its own, and its OBS copy is kept.
    #[test]
    fn private_inherited_folder_keeps_obs() {
        let root = std::env::temp_dir().join(format!("replay-vault-acl-{}", crate::config::random_hex()));
        fs::create_dir_all(root.join("obs-studio")).unwrap();
        fs::write(root.join(r"obs-studio\obs64.exe"), b"x").unwrap();
        let trusted = trusted_sids().unwrap();
        if read_acl(&root).unwrap().is_none() {
            let _ = fs::remove_dir_all(&root);
            return;
        }
        // Inherited, and only this account, SYSTEM and Administrators.
        let private = format!("D:(A;OICI;FA;;;{})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)", trusted[0]);
        set_dacl(&root, &private, true).unwrap();
        let fixed = secure_dir(&root).unwrap();
        let kept = root.join(r"obs-studio\obs64.exe").exists();
        // The same, but one folder inside has its own entry letting every signed-in account in.
        set_dacl(&root, &private, true).unwrap();
        set_dacl(&root.join("obs-studio"), &format!("D:P(A;OICI;0x1301bf;;;AU)(A;OICI;FA;;;{})", trusted[0]), true).unwrap();
        let inner = secure_dir(&root).unwrap();
        let removed = !root.join("obs-studio").exists();
        let _ = fs::remove_dir_all(&root);
        assert!(fixed.is_some_and(|f| !f.contains("downloaded again")));
        assert!(kept, "a copy nobody else could change is kept");
        assert!(inner.is_some_and(|f| f.contains("downloaded again")) && removed, "an open folder inside counts too");
    }

    #[test]
    fn version_stamps() {
        assert_eq!(last_version_stamp("33.0.1"), Some(553648129));
        assert!(last_version_stamp("33.1.0") > last_version_stamp("33.0.9"));
        assert_eq!(last_version_stamp("junk"), None);
    }
}
