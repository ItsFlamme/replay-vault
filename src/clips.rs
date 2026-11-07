//! The clips folder: listing, opening, recycling, free space, and thumbnails made in the background.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::media;

pub struct Clip {
    pub path: PathBuf,
    pub name: String,
    pub bytes: u64,
    pub modified: SystemTime,
    /// The subfolder it's in (a game, "Recordings"), or None for the clips folder itself.
    pub group: Option<String>,
    pub is_picture: bool,
}

/// Videos, screenshots and GIFs in `folder` and its subfolders (one level), newest first.
pub fn list(folder: &str, max: usize) -> Vec<Clip> {
    let root = Path::new(folder);
    let mut dirs = vec![(root.to_path_buf(), None)];
    for e in std::fs::read_dir(root).into_iter().flatten().flatten() {
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            dirs.push((e.path(), Some(e.file_name().to_string_lossy().into_owned())));
        }
    }
    let mut clips: Vec<Clip> = dirs
        .into_iter()
        .flat_map(|(dir, group)| std::fs::read_dir(dir).into_iter().flatten().flatten().map(move |e| (e, group.clone())))
        .filter_map(|(e, group)| {
            let path = e.path();
            let ext = path.extension()?.to_string_lossy().to_lowercase();
            let is_picture = matches!(ext.as_str(), "png" | "gif");
            // Skips a clip still being cut down by a short save.
            if !(is_picture || matches!(ext.as_str(), "mp4" | "mkv" | "mov")) || path.to_string_lossy().contains(".cutting.") {
                return None;
            }
            let m = e.metadata().ok()?;
            let modified = m.modified().ok()?;
            // A recording OBS is still writing can't be played or moved yet.
            if modified.elapsed().is_ok_and(|age| age.as_secs() < 15) && std::fs::OpenOptions::new().append(true).open(&path).is_err() {
                return None;
            }
            Some(Clip { name: path.file_name()?.to_string_lossy().into_owned(), bytes: m.len(), modified, path, group, is_picture })
        })
        .collect();
    clips.sort_by(|a, b| b.modified.cmp(&a.modified));
    clips.truncate(max);
    clips
}

/// The folder name for whatever is in front (by its window title only; the game's process is
/// never opened, so anti-cheats have nothing to see), or "Desktop".
pub fn game_folder_name() -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowTextW;
    let window = crate::overlay::window_in_front();
    let mut buf = [0u16; 256];
    let n = if window.is_invalid() { 0 } else { unsafe { GetWindowTextW(window, &mut buf) } };
    folder_name_from_title(&String::from_utf16_lossy(&buf[..n.max(0) as usize]))
}

/// "VALORANT  " -> "VALORANT", "Elden Ring - v1.2" -> "Elden Ring"; nothing usable -> "Desktop".
fn folder_name_from_title(title: &str) -> String {
    let title = [" - ", " | ", " \u{2014} ", ": "].iter().fold(title, |t, sep| t.split(sep).next().unwrap_or(t));
    let clean = windows_safe(title);
    let clean = clean.as_str();
    if clean.is_empty() || clean == "Program Manager" {
        return "Desktop".into();
    }
    clean.chars().take(60).collect::<String>().trim().to_string()
}

/// `name` without characters Windows doesn't allow in file names, and without the spaces and
/// dots Windows drops from the end.
fn windows_safe(name: &str) -> String {
    let clean: String = name.chars().filter(|c| !r#"<>:"/\|?*"#.contains(*c) && !c.is_control()).collect();
    clean.trim().trim_end_matches('.').trim().to_string()
}

/// Renames a clip within its folder to `name` (the extension stays; typing it too is fine),
/// never over another file. Returns the new path.
pub fn rename(path: &Path, name: &str) -> anyhow::Result<PathBuf> {
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let mut stem = windows_safe(name);
    let typed_ext = stem.len().checked_sub(ext.len()).filter(|&c| !ext.is_empty() && stem.is_char_boundary(c) && stem[c..].eq_ignore_ascii_case(&ext));
    if let Some(cut) = typed_ext {
        stem.truncate(cut);
        stem = windows_safe(&stem);
    }
    anyhow::ensure!(!stem.is_empty(), "the name can't be empty");
    let dest = path.with_file_name(format!("{stem}{ext}"));
    // Only the case changing is the same file to Windows.
    let same = dest.to_string_lossy().to_lowercase() == path.to_string_lossy().to_lowercase();
    if dest == path {
        return Ok(dest);
    }
    anyhow::ensure!(same || !dest.exists(), "there's already a file called {stem}{ext}");
    std::fs::rename(path, &dest).map_err(|e| anyhow::anyhow!("Windows wouldn't rename it (open somewhere else?): {e}"))?;
    Ok(dest)
}

/// Moves a file into `dir` (made if needed), never over an existing file; returns where it went.
/// Retries for a few seconds in case OBS hasn't let go of it yet. To another drive or a NAS,
/// which Windows can't move to, it's copied, checked and then removed.
pub fn move_into(path: &Path, dir: &Path) -> std::io::Result<PathBuf> {
    if path.parent() == Some(dir) {
        return Ok(path.to_path_buf());
    }
    std::fs::create_dir_all(dir)?;
    let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let dest = (1..)
        .map(|n| dir.join(if n == 1 { format!("{stem}{ext}") } else { format!("{stem} ({n}){ext}") }))
        .find(|p| !p.exists())
        .unwrap();
    let mut tries = 0;
    loop {
        match std::fs::rename(path, &dest) {
            Ok(()) => return Ok(dest),
            Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => return copy_then_remove(path, &dest).map(|()| dest),
            Err(_) if tries < 10 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Copies `path` to `dest` through a ".part" file, so a copy cut short never looks like the clip,
/// and checks the size.
fn copy_checked(path: &Path, dest: &Path) -> std::io::Result<()> {
    let part = PathBuf::from(format!("{}.part", dest.display()));
    let copied = (|| {
        let size = std::fs::metadata(path)?.len();
        std::fs::copy(path, &part)?;
        if std::fs::metadata(&part)?.len() != size {
            return Err(std::io::Error::other("the copy came out a different size"));
        }
        std::fs::rename(&part, dest)
    })();
    if copied.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    copied
}

/// `copy_checked`, then removes `path`. If `path` can't be removed, the copy goes instead.
fn copy_then_remove(path: &Path, dest: &Path) -> std::io::Result<()> {
    copy_checked(path, dest)?;
    if let Err(e) = std::fs::remove_file(path)
        && path.exists()
    {
        let _ = std::fs::remove_file(dest);
        return Err(e);
    }
    Ok(())
}

/// Carries a clip's star, tags and share link to where it went (renamed or archived), or with
/// `new` None (recycled) drops its star and tags; the share link stays, to be deleted. Saves
/// whatever changed.
pub fn moved(favs: &mut std::collections::BTreeSet<String>, tags: &mut Tags, old: &Path, new: Option<&Path>) {
    if favs.remove(&old.display().to_string()) {
        favs.extend(new.map(|n| n.display().to_string()));
        save_favorites(favs);
    }
    if move_tags(tags, old, new) {
        save_tags(tags);
    }
    if let Some(new) = new {
        crate::upload::renamed(old, new);
    }
}

/// Local "2026-11-06 20-58-10", for screenshot names.
pub fn stamp(t: SystemTime) -> String {
    let (y, m, d, h, mi, s) = local_full(t).unwrap_or_default();
    format!("{y}-{m:02}-{d:02} {h:02}-{mi:02}-{s:02}")
}

/// Moves a file to the Recycle Bin, so it can be restored. If it's too big for the Recycle Bin,
/// Windows asks before deleting it for good instead of doing that silently.
pub fn recycle(path: &Path) -> anyhow::Result<()> {
    use windows::Win32::UI::Shell::{FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING, SHFILEOPSTRUCTW, SHFileOperationW};
    // The list of paths ends with an extra NUL.
    let from: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().chain([0, 0]).collect();
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: windows::core::PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO.0 | FOF_NOCONFIRMATION.0 | FOF_SILENT.0 | FOF_NOERRORUI.0 | FOF_WANTNUKEWARNING.0) as u16,
        ..Default::default()
    };
    let code = unsafe { SHFileOperationW(&mut op) };
    anyhow::ensure!(code == 0 && !path.exists(), "Windows couldn't move it to the Recycle Bin (code {code})");
    Ok(())
}

fn favorites_file() -> PathBuf {
    crate::config::dir().join("favorites.json")
}

/// Starred clips, by path.
pub fn favorites() -> std::collections::BTreeSet<String> {
    std::fs::read_to_string(favorites_file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save_favorites(favs: &std::collections::BTreeSet<String>) {
    let _ = std::fs::create_dir_all(crate::config::dir());
    let _ = std::fs::write(favorites_file(), serde_json::to_string_pretty(favs).unwrap_or_default());
}

/// Tags ("Aces", "Funny") by clip path.
pub type Tags = std::collections::BTreeMap<String, std::collections::BTreeSet<String>>;

fn tags_file() -> PathBuf {
    crate::config::dir().join("tags.json")
}

pub fn tags() -> Tags {
    read_tags(&tags_file())
}

pub fn save_tags(tags: &Tags) {
    write_tags(&tags_file(), tags);
}

fn read_tags(file: &Path) -> Tags {
    std::fs::read_to_string(file).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn write_tags(file: &Path, tags: &Tags) {
    let _ = std::fs::create_dir_all(file.parent().unwrap_or(Path::new(".")));
    let _ = std::fs::write(file, serde_json::to_string_pretty(tags).unwrap_or_default());
}

/// Puts `tag` on a clip or takes it off. A new tag that matches one in use but for case takes
/// that one's spelling.
pub fn set_tag(tags: &mut Tags, path: &Path, tag: &str, on: bool) {
    let tag = tag.trim();
    if tag.is_empty() {
        return;
    }
    let tag = tags.values().flatten().find(|t| t.eq_ignore_ascii_case(tag)).cloned().unwrap_or_else(|| tag.chars().take(30).collect());
    let key = path.display().to_string();
    let set = tags.entry(key.clone()).or_default();
    if on {
        set.insert(tag);
    } else {
        set.remove(&tag);
    }
    if set.is_empty() {
        tags.remove(&key);
    }
}

/// The tag that marks a clip for the cloud backup (any case, so a "cloud" tag counts too).
pub const CLOUD: &str = "Cloud";

pub fn is_cloud(tag: &str) -> bool {
    tag.eq_ignore_ascii_case(CLOUD)
}

/// Carries a clip's tags to its new path after a rename, or drops them (`new` None, recycled).
/// True if anything changed.
pub fn move_tags(tags: &mut Tags, old: &Path, new: Option<&Path>) -> bool {
    let Some(set) = tags.remove(&old.display().to_string()) else { return false };
    if let Some(new) = new {
        tags.insert(new.display().to_string(), set);
    }
    true
}

/// Auto-cleanup limits; None is no limit.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Cleanup {
    pub max_bytes: Option<u64>,
    pub max_age: Option<std::time::Duration>,
}

/// Clips this new are never cleaned up, so there's time to star them.
const GRACE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// What auto-cleanup would move to the Recycle Bin, oldest first: clips older than the age limit,
/// then the oldest until the rest fit under the size limit. Favorites are never picked and don't
/// count toward the size.
pub fn cleanup_plan(clips: &[Clip], favs: &std::collections::BTreeSet<String>, rule: Cleanup, now: SystemTime) -> Vec<PathBuf> {
    let age = |c: &Clip| now.duration_since(c.modified).unwrap_or_default();
    let mut others: Vec<&Clip> = clips.iter().filter(|c| !favs.contains(&c.path.display().to_string())).collect();
    let mut total: u64 = others.iter().map(|c| c.bytes).sum();
    others.sort_by_key(|c| c.modified);
    let mut out = Vec::new();
    for c in others.into_iter().filter(|c| age(c) >= GRACE) {
        let too_old = rule.max_age.is_some_and(|max| age(c) > max);
        let too_big = rule.max_bytes.is_some_and(|max| total > max);
        if too_old || too_big {
            total -= c.bytes;
            out.push(c.path.clone());
        }
    }
    out
}

static CLEANUP: Mutex<Option<(String, Cleanup)>> = Mutex::new(None);

/// The folder and limits auto-cleanup works with (from the saved settings).
pub fn set_cleanup(folder: &str, rule: Cleanup) {
    let mut c = CLEANUP.lock().unwrap();
    if c.as_ref().is_none_or(|(f, r)| f != folder || *r != rule) {
        *c = Some((folder.to_string(), rule));
    }
}

/// Once a minute, moves what `cleanup_plan` picks to the Recycle Bin. A clip that can't be moved
/// (open somewhere, or too big for the Recycle Bin and not okayed) is left alone after that.
pub fn start_cleanup() {
    let mut skipped = std::collections::HashSet::new();
    every(60, &CLEANUP, move |(folder, rule)| {
        if rule == Cleanup::default() || folder.is_empty() {
            return;
        }
        for path in cleanup_plan(&list(&folder, usize::MAX), &favorites(), rule, SystemTime::now()) {
            if skipped.contains(&path) {
                continue;
            }
            match recycle(&path) {
                Ok(()) => crate::log::write(&format!("Auto-cleanup moved {} to the Recycle Bin.", path.display())),
                Err(e) => {
                    crate::log::write(&format!("Auto-cleanup left {} alone: {e:#}", path.display()));
                    skipped.insert(path);
                }
            }
        }
    });
}

/// On a thread of its own, every `secs` seconds: `pass` with the rule in `slot`, unless it's None.
fn every<T: Clone + Send + 'static>(secs: u64, slot: &'static Mutex<Option<T>>, mut pass: impl FnMut(T) + Send + 'static) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(secs));
            let rule = slot.lock().unwrap().clone();
            if let Some(rule) = rule {
                pass(rule);
            }
        }
    });
}

/// Archive: the folder old clips move to (another drive or a NAS), and after how long.
#[derive(Clone, PartialEq, Debug)]
pub struct Archive {
    pub dir: PathBuf,
    pub after: std::time::Duration,
}

/// What the archive would move, oldest first: clips older than `after` (never one from the last
/// 10 minutes).
pub fn archive_plan(clips: &[Clip], after: std::time::Duration, now: SystemTime) -> Vec<&Clip> {
    let mut out: Vec<&Clip> = clips.iter().filter(|c| now.duration_since(c.modified).unwrap_or_default() > after.max(GRACE)).collect();
    out.sort_by_key(|c| c.modified);
    out
}

/// Whether `dir` is the clips folder or inside it (archived clips would be listed, and cleaned up, again).
pub fn inside(dir: &str, folder: &str) -> bool {
    let lower = |s: &str| PathBuf::from(s.to_lowercase());
    lower(dir).starts_with(lower(folder))
}

static ARCHIVE: Mutex<Option<(String, Archive)>> = Mutex::new(None);
/// Clips the archive moved (old, new), for the window to carry stars and tags over in its own
/// copy. Reloading the files instead could lose a star or tag set while the archive was writing.
static ARCHIVED: Mutex<Vec<(PathBuf, PathBuf)>> = Mutex::new(Vec::new());

pub fn take_archived() -> Vec<(PathBuf, PathBuf)> {
    std::mem::take(&mut ARCHIVED.lock().unwrap())
}
/// Set while the archive folder wasn't there the last time it was looked for.
pub static ARCHIVE_MISSING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The clips folder and archive rule to work with (from the saved settings); None is off.
pub fn set_archive(folder: &str, rule: Option<Archive>) {
    *ARCHIVE.lock().unwrap() = rule.map(|r| (folder.to_string(), r));
}

/// Every 5 minutes, moves what `archive_plan` picks into the archive's folder of the same name
/// (VALORANT\x.mp4 to <archive>\VALORANT\x.mp4), with its star, tags and share link. If the
/// archive folder isn't there (a NAS that's off), waits for it, saying so in the log once.
pub fn start_archive() {
    use std::sync::atomic::Ordering::Relaxed;
    let mut skipped = std::collections::HashSet::new();
    every(300, &ARCHIVE, move |(folder, rule)| {
        if !rule.dir.is_dir() {
            if !ARCHIVE_MISSING.swap(true, Relaxed) {
                crate::log::write(&format!("Archive folder {} isn't there (drive or NAS off?); clips wait until it's back.", rule.dir.display()));
            }
            return;
        }
        ARCHIVE_MISSING.store(false, Relaxed);
        let clips = list(&folder, usize::MAX);
        for c in archive_plan(&clips, rule.after, SystemTime::now()) {
            if skipped.contains(&c.path) {
                continue;
            }
            let dir = c.group.as_ref().map_or(rule.dir.clone(), |g| rule.dir.join(g));
            match move_into(&c.path, &dir) {
                Ok(new) => {
                    // Written here too, so it holds while the window is asleep in the tray. Read
                    // fresh: a copy to a NAS can take a while.
                    moved(&mut favorites(), &mut tags(), &c.path, Some(&new));
                    ARCHIVED.lock().unwrap().push((c.path.clone(), new.clone()));
                    crate::log::write(&format!("Archived {} to {}.", c.path.display(), new.display()));
                }
                Err(e) => {
                    crate::log::write(&format!("Archive left {} alone: {e}", c.path.display()));
                    skipped.insert(c.path.clone());
                }
            }
        }
    });
}

/// Copies clips tagged Cloud into `backup` (a OneDrive or Google Drive folder; its app uploads
/// them), in their game folder (worked out from whichever of `roots` they're in), unless a file
/// the same size is already there. Nothing in `backup` is ever removed. Returns how many were copied.
pub fn backup_tagged(tags: &Tags, roots: &[PathBuf], backup: &Path) -> usize {
    let mut copied = 0;
    for path in tags.iter().filter(|(_, t)| t.iter().any(|t| is_cloud(t))).map(|(p, _)| Path::new(p)) {
        let Ok(size) = std::fs::metadata(path).map(|m| m.len()) else { continue };
        let rel = roots.iter().find_map(|r| path.strip_prefix(r).ok()).or(path.file_name().map(Path::new)).unwrap_or(path);
        let dest = backup.join(rel);
        if std::fs::metadata(&dest).is_ok_and(|m| m.len() == size) {
            continue;
        }
        let made = dest.parent().map_or(Ok(()), std::fs::create_dir_all);
        match made.and_then(|()| copy_checked(path, &dest)) {
            Ok(()) => copied += 1,
            Err(e) => crate::log::write(&format!("Couldn't back up {}: {e}", path.display())),
        }
    }
    copied
}

/// The folders backed-up clips can be in (clips folder, archive), and the backup folder; None is off.
static BACKUP: Mutex<Option<(Vec<PathBuf>, PathBuf)>> = Mutex::new(None);

pub fn set_backup(rule: Option<(Vec<PathBuf>, PathBuf)>) {
    *BACKUP.lock().unwrap() = rule;
}

/// Every 5 minutes, `backup_tagged`. A backup folder that can't be made is logged once.
pub fn start_backup() {
    let mut failed = false;
    every(300, &BACKUP, move |(roots, dir)| {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            if !std::mem::replace(&mut failed, true) {
                crate::log::write(&format!("Backup folder {} isn't available: {e}", dir.display()));
            }
            return;
        }
        failed = false;
        let n = backup_tagged(&tags(), &roots, &dir);
        if n > 0 {
            crate::log::write(&format!("Backed up {n} clips to {}.", dir.display()));
        }
    });
}

/// Puts the file on the clipboard the way File Explorer's Copy does, so it pastes into Discord,
/// a chat or a folder as the file itself.
pub fn copy_file(path: &Path) -> anyhow::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{GlobalFree, HANDLE};
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GHND, GlobalAlloc, GlobalLock, GlobalUnlock};
    use windows::Win32::System::Ole::CF_HDROP;
    use windows::Win32::UI::Shell::DROPFILES;
    // A DROPFILES header, then the paths as UTF-16, ending with an extra NUL.
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0, 0]).collect();
    let header = std::mem::size_of::<DROPFILES>();
    unsafe {
        let mem = GlobalAlloc(GHND, header + wide.len() * 2)?;
        let p = GlobalLock(mem) as *mut u8;
        if p.is_null() {
            let _ = GlobalFree(Some(mem));
            anyhow::bail!("Windows couldn't make room for it on the clipboard");
        }
        let drop = p as *mut DROPFILES;
        (*drop).pFiles = header as u32;
        (*drop).fWide = true.into();
        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, p.add(header), wide.len() * 2);
        let _ = GlobalUnlock(mem);
        // With no owner window, Windows may refuse the data.
        if let Err(e) = OpenClipboard(crate::overlay::hwnd()) {
            let _ = GlobalFree(Some(mem));
            return Err(e.into());
        }
        // Once set, the clipboard owns the memory.
        let set = EmptyClipboard().and_then(|()| SetClipboardData(CF_HDROP.0 as u32, Some(HANDLE(mem.0))));
        let _ = CloseClipboard();
        if let Err(e) = set {
            let _ = GlobalFree(Some(mem));
            return Err(e.into());
        }
    }
    Ok(())
}

/// Opens a file with its default app, or shows it selected in File Explorer, in front of the
/// always-on-top overlay (which is hidden so it isn't in the way).
pub fn open(path: &Path, reveal: bool) {
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(u32::MAX);
    }
    let mut cmd = std::process::Command::new(crate::recorder::windows_exe("explorer.exe"));
    if reveal {
        // explorer wants /select,"path" as one argument.
        use std::os::windows::process::CommandExt;
        cmd.raw_arg(format!("/select,\"{}\"", path.display()));
    } else {
        cmd.arg(path);
    }
    let _ = cmd.spawn();
    crate::overlay::hide();
}

/// Free bytes on the drive holding `folder` (for this user).
pub fn free_space(folder: &str) -> Option<u64> {
    let mut free = 0u64;
    let dir = windows::core::HSTRING::from(folder);
    unsafe { windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(&dir, Some(&mut free), None, None).ok()? };
    Some(free)
}

/// A clip's still frame and length, once worked out.
pub struct Preview {
    pub frame: Option<media::Frame>,
    pub secs: Option<f64>,
}

/// Makes thumbnails and frame previews on one background thread, so the window never waits on a
/// video decode. Results are keyed by (path, time in tenths of a second).
pub struct Previews {
    want: Sender<(PathBuf, u32, usize, bool)>,
    done: Arc<Mutex<HashMap<(PathBuf, u32), Preview>>>,
    asked: std::collections::HashSet<(PathBuf, u32)>,
}

impl Previews {
    pub fn new(repaint: impl Fn() + Send + 'static) -> Self {
        let (want, rx) = mpsc::channel::<(PathBuf, u32, usize, bool)>();
        let done: Arc<Mutex<HashMap<(PathBuf, u32), Preview>>> = Arc::default();
        let out = done.clone();
        std::thread::spawn(move || {
            while let Ok((path, tenths, width, quick)) = rx.recv() {
                let secs = media::duration(&path).ok();
                // Never past the end (a short clip asked for its 1 s frame).
                let at = (tenths as f64 / 10.0).min(secs.unwrap_or(0.0) - 0.1).max(0.0);
                let frame = if quick { media::keyframe(&path, at, width) } else { media::frame(&path, at, width) }.ok();
                out.lock().unwrap().insert((path, tenths), Preview { frame, secs });
                repaint();
            }
        });
        Self { want, done, asked: Default::default() }
    }

    /// The preview of `path` at `at` seconds if it's ready; asks for it otherwise. `quick` takes
    /// the keyframe before `at` (media::keyframe), for scrubbing.
    pub fn get(&mut self, path: &Path, at: f64, width: usize, quick: bool) -> Option<Preview> {
        let key = (path.to_path_buf(), (at * 10.0).round().max(0.0) as u32);
        if let Some(p) = self.done.lock().unwrap().remove(&key) {
            return Some(p);
        }
        if self.asked.insert(key.clone()) {
            let _ = self.want.send((key.0, key.1, width, quick));
        }
        None
    }

    /// Drops everything made so far; previews are made again when next asked for.
    pub fn forget(&mut self) {
        self.retain(|_| false);
    }

    /// Forgets the previews whose time in tenths `keep` says no to; they're made again when next
    /// asked for.
    pub fn retain(&mut self, keep: impl Fn(u32) -> bool) {
        self.done.lock().unwrap().retain(|k, _| keep(k.1));
        self.asked.retain(|k| keep(k.1));
    }
}

/// A time as local calendar fields (year, month, day, hour, minute, weekday 0 = Sunday).
fn local(t: SystemTime) -> Option<(u16, u16, u16, u16, u16, u16)> {
    local_system_time(t).map(|l| (l.wYear, l.wMonth, l.wDay, l.wHour, l.wMinute, l.wDayOfWeek))
}

/// Local (year, month, day, hour, minute, second).
fn local_full(t: SystemTime) -> Option<(u16, u16, u16, u16, u16, u16)> {
    local_system_time(t).map(|l| (l.wYear, l.wMonth, l.wDay, l.wHour, l.wMinute, l.wSecond))
}

fn local_system_time(t: SystemTime) -> Option<windows::Win32::Foundation::SYSTEMTIME> {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    let unix_100ns = t.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos() / 100;
    let ft = (unix_100ns + 116_444_736_000_000_000) as u64; // 1601 -> 1970
    let ft = FILETIME { dwLowDateTime: ft as u32, dwHighDateTime: (ft >> 32) as u32 };
    let (mut utc, mut loc) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    unsafe {
        FileTimeToSystemTime(&ft, &mut utc).ok()?;
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut loc).ok()?;
    }
    Some(loc)
}

/// The local calendar day of `t`, for grouping.
pub fn day(t: SystemTime) -> (u16, u16, u16) {
    local(t).map_or((0, 0, 0), |(y, m, d, ..)| (y, m, d))
}

/// "Today", "Yesterday", or "Wed, Nov 4" (with the year if it isn't this year).
pub fn day_label(t: SystemTime) -> String {
    let now = SystemTime::now();
    let d = day(t);
    if d == day(now) {
        return "Today".into();
    }
    if d == day(now - std::time::Duration::from_secs(86_400)) {
        return "Yesterday".into();
    }
    let Some((y, m, dd, .., wd)) = local(t) else { return "Earlier".into() };
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let label = format!("{}, {} {dd}", DAYS[wd as usize % 7], MONTHS[(m as usize + 11) % 12]);
    if Some(y) == local(now).map(|l| l.0) { label } else { format!("{label}, {y}") }
}

/// Local "6:42 PM".
pub fn time_label(t: SystemTime) -> String {
    match local(t) {
        Some((.., h, mi, _)) => format!("{}:{mi:02} {}", if h % 12 == 0 { 12 } else { h % 12 }, if h < 12 { "AM" } else { "PM" }),
        None => String::new(),
    }
}

/// "12.3 MB" / "1.2 GB".
pub fn size(bytes: u64) -> String {
    let mb = bytes as f64 / 1_048_576.0;
    if mb >= 1024.0 { format!("{:.1} GB", mb / 1024.0) } else { format!("{mb:.1} MB") }
}

/// A free name next to `src` for a copy marked `tag`: "Clip (trim).mp4", "Clip (trim 2).mp4", ...
pub fn copy_name(src: &Path, tag: &str) -> PathBuf {
    let stem = src.file_stem().unwrap_or_default().to_string_lossy();
    let ext = src.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "mp4".into());
    (1..)
        .map(|n| src.with_file_name(if n == 1 { format!("{stem} ({tag}).{ext}") } else { format!("{stem} ({tag} {n}).{ext}") }))
        .find(|p| !p.exists())
        .unwrap()
}

/// A free name in `folder` for a montage made at `now`: "Montage 2026-11-07 10-53-12.mp4", then
/// "... (2).mp4" and so on if that's taken.
pub fn montage_name(folder: &Path, now: SystemTime) -> PathBuf {
    let stem = format!("Montage {}", stamp(now));
    (1..)
        .map(|n| folder.join(if n == 1 { format!("{stem}.mp4") } else { format!("{stem} ({n}).mp4") }))
        .find(|p| !p.exists())
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn montage_names_never_overwrite() {
        let dir = std::env::temp_dir().join("instant-replay-montage-names");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now();
        let first = montage_name(&dir, now);
        assert_eq!(first.file_name().unwrap().to_string_lossy(), format!("Montage {}.mp4", stamp(now)));
        std::fs::write(&first, b"x").unwrap();
        let second = montage_name(&dir, now);
        assert_eq!(second.file_name().unwrap().to_string_lossy(), format!("Montage {} (2).mp4", stamp(now)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Previews dropped by `retain` are asked for again; the rest are kept. Uses a clip that
    /// doesn't exist, so nothing is decoded.
    #[test]
    fn previews_retain_forgets_the_rest() {
        let mut p = Previews::new(|| {});
        let path = std::env::temp_dir().join("instant-replay-no-such-clip.mp4");
        assert!(p.get(&path, 1.0, 160, false).is_none());
        assert!(p.get(&path, 5.0, 160, true).is_none());
        while p.done.lock().unwrap().len() < 2 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        p.retain(|t| t == 10);
        assert_eq!(p.asked.len(), 1);
        assert!(p.get(&path, 1.0, 160, false).is_some());
        assert!(p.get(&path, 5.0, 160, true).is_none(), "asked for again, not answered from memory");
    }

    #[test]
    fn copy_names_dont_overwrite() {
        let dir = std::env::temp_dir().join("instant-replay-names");
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("Replay 1.mp4");
        assert_eq!(copy_name(&src, "trim").file_name().unwrap(), "Replay 1 (trim).mp4");
        std::fs::write(dir.join("Replay 1 (trim).mp4"), b"x").unwrap();
        assert_eq!(copy_name(&src, "trim").file_name().unwrap(), "Replay 1 (trim 2).mp4");
        assert_eq!(copy_name(&src, "Discord 10MB").file_name().unwrap(), "Replay 1 (Discord 10MB).mp4");
        // A GIF of it gets the .gif extension, and is listed as a picture.
        let gif = copy_name(&src.with_extension("gif"), "GIF");
        assert_eq!(gif.file_name().unwrap(), "Replay 1 (GIF).gif");
        std::fs::write(&gif, b"GIF89a").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        let listed = list(&dir.to_string_lossy(), 10);
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().any(|c| c.path == gif && c.is_picture));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Tags save and load (in a temp file, not the real tags.json), keep one spelling, and follow
    /// a rename or go with a recycle.
    #[test]
    fn tags_store() {
        let file = std::env::temp_dir().join("instant-replay-tags-test").join("tags.json");
        let (a, b) = (Path::new(r"C:\clips\a.mp4"), Path::new(r"C:\clips\b.mp4"));
        let mut t = Tags::new();
        set_tag(&mut t, a, " Aces ", true);
        set_tag(&mut t, a, "Funny", true);
        set_tag(&mut t, b, "aces", true);
        set_tag(&mut t, b, "  ", true);
        assert_eq!(t[&b.display().to_string()].iter().collect::<Vec<_>>(), ["Aces"]);
        write_tags(&file, &t);
        assert_eq!(read_tags(&file), t);
        set_tag(&mut t, b, "Aces", false);
        assert!(!t.contains_key(&b.display().to_string()), "no tags left: no entry");
        let c = Path::new(r"C:\clips\c.mp4");
        assert!(move_tags(&mut t, a, Some(c)));
        assert_eq!(t[&c.display().to_string()].len(), 2);
        assert!(!move_tags(&mut t, a, Some(c)), "nothing left at the old path");
        assert!(move_tags(&mut t, c, None) && t.is_empty());
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    /// Puts a small file in the real Recycle Bin, so it only runs when asked (--ignored).
    #[test]
    #[ignore]
    fn recycles_a_file() {
        let f = std::env::temp_dir().join("instant-replay-recycle-test.txt");
        std::fs::write(&f, b"test").unwrap();
        recycle(&f).unwrap();
        assert!(!f.exists());
    }

    #[test]
    fn renames_safely() {
        let dir = std::env::temp_dir().join("instant-replay-rename");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("Replay 1.mp4");
        std::fs::write(&clip, b"x").unwrap();
        std::fs::write(dir.join("Taken.mp4"), b"y").unwrap();
        // Bad characters go, the extension stays and isn't doubled.
        let a = rename(&clip, "ace: clutch?").unwrap();
        assert_eq!(a.file_name().unwrap(), "ace clutch.mp4");
        let b = rename(&a, "Ace Clutch.MP4").unwrap();
        assert_eq!(b.file_name().unwrap(), "Ace Clutch.mp4");
        // Never over another file, never to nothing.
        assert!(rename(&b, "taken").is_err());
        assert!(rename(&b, " ?. ").is_err());
        // A name ending in a wide character next to where the extension would be.
        let c = rename(&b, "Ślą").unwrap();
        assert_eq!(c.file_name().unwrap(), "Ślą.mp4");
        assert_eq!(std::fs::read(&c).unwrap(), b"x");
        assert_eq!(std::fs::read(dir.join("Taken.mp4")).unwrap(), b"y");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cleanup_picks_oldest_and_spares_favorites() {
        let now = SystemTime::now();
        let hours = |h: u64| now - std::time::Duration::from_secs(h * 3600);
        let clip = |name: &str, gb: u64, modified| Clip { path: PathBuf::from(name), name: name.into(), bytes: gb << 30, modified, group: None, is_picture: false };
        let clips = vec![
            clip("new", 5, now - std::time::Duration::from_secs(60)),
            clip("a", 2, hours(1)),
            clip("fav", 50, hours(500)),
            clip("b", 2, hours(30)),
            clip("c", 2, hours(60)),
        ];
        let favs = ["fav".to_string()].into();
        let names = |rule| cleanup_plan(&clips, &favs, rule, now).iter().map(|p| p.display().to_string()).collect::<Vec<_>>();
        // 11 GB besides the favorite; under 8 GB means the two oldest go. The new clip is never picked.
        assert_eq!(names(Cleanup { max_bytes: Some(8 << 30), max_age: None }), ["c", "b"]);
        assert_eq!(names(Cleanup { max_bytes: Some(1 << 30), max_age: None }), ["c", "b", "a"]);
        assert_eq!(names(Cleanup { max_bytes: None, max_age: Some(std::time::Duration::from_secs(48 * 3600)) }), ["c"]);
        assert!(names(Cleanup { max_bytes: Some(20 << 30), max_age: None }).is_empty());
        assert!(names(Cleanup::default()).is_empty());
    }

    #[test]
    fn archive_picks_old_clips_oldest_first() {
        let now = SystemTime::now();
        let days = |d: u64| now - std::time::Duration::from_secs(d * 86_400);
        let clip = |name: &str, modified| Clip { path: PathBuf::from(name), name: name.into(), bytes: 1, modified, group: None, is_picture: false };
        let clips = vec![clip("new", now), clip("b", days(10)), clip("a", days(40)), clip("c", days(6))];
        let names = |d: u64| archive_plan(&clips, std::time::Duration::from_secs(d * 86_400), now).iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(7), ["a", "b"]);
        assert_eq!(names(0), ["a", "b", "c"], "the newest is never picked");
        assert!(inside(r"D:\Clips\archive", r"d:\clips") && inside(r"D:\Clips", r"D:\Clips"));
        assert!(!inside(r"E:\Archive", r"D:\Clips") && !inside(r"D:\Clips2", r"D:\Clips"));
    }

    /// Moves a clip between two temp folders (on two drives here: temp on C:, target on D:, so
    /// the copy-then-remove path runs) keeping its game folder, never over a clip already there.
    #[test]
    fn archive_moves_into_game_folder() {
        let (from, to) = (std::env::temp_dir().join("instant-replay-archive-from"), Path::new(env!("CARGO_MANIFEST_DIR")).join(r"target\archive-test"));
        for d in [&from, &to] {
            let _ = std::fs::remove_dir_all(d);
        }
        std::fs::create_dir_all(from.join("VALORANT")).unwrap();
        std::fs::create_dir_all(to.join("VALORANT")).unwrap();
        let clip = from.join(r"VALORANT\x.mp4");
        std::fs::write(&clip, b"clip").unwrap();
        std::fs::write(to.join(r"VALORANT\x.mp4"), b"older").unwrap();
        let new = move_into(&clip, &to.join("VALORANT")).unwrap();
        assert_eq!(new, to.join(r"VALORANT\x (2).mp4"));
        assert_eq!(std::fs::read(&new).unwrap(), b"clip");
        assert_eq!(std::fs::read(to.join(r"VALORANT\x.mp4")).unwrap(), b"older");
        assert!(!clip.exists() && !to.join(r"VALORANT\x (2).mp4.part").exists());
        for d in [&from, &to] {
            std::fs::remove_dir_all(d).unwrap();
        }
    }

    /// Clips tagged Cloud (any case) are copied once into their game folder; a changed one is
    /// copied again, others (even with another tag) aren't, and nothing in the backup is removed.
    #[test]
    fn backup_copies_tagged() {
        let (clips, backup) = (std::env::temp_dir().join("instant-replay-backup-clips"), std::env::temp_dir().join("instant-replay-backup-to"));
        for d in [&clips, &backup] {
            let _ = std::fs::remove_dir_all(d);
        }
        std::fs::create_dir_all(clips.join("VALORANT")).unwrap();
        let (a, b, c) = (clips.join(r"VALORANT\a.mp4"), clips.join("b.mp4"), clips.join("c.mp4"));
        for f in [&a, &b, &c] {
            std::fs::write(f, b"clip").unwrap();
        }
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(backup.join("old.mp4"), b"x").unwrap();
        let mut tags = Tags::new();
        set_tag(&mut tags, &a, "cloud", true);
        set_tag(&mut tags, &b, CLOUD, true);
        set_tag(&mut tags, &c, "Aces", true);
        let roots = [clips.clone()];
        assert_eq!(backup_tagged(&tags, &roots, &backup), 2);
        assert_eq!(std::fs::read(backup.join(r"VALORANT\a.mp4")).unwrap(), b"clip");
        assert!(backup.join("b.mp4").exists() && !backup.join("c.mp4").exists());
        assert_eq!(backup_tagged(&tags, &roots, &backup), 0, "already there");
        std::fs::write(&b, b"longer clip").unwrap();
        assert_eq!(backup_tagged(&tags, &roots, &backup), 1);
        assert_eq!(std::fs::read(backup.join("b.mp4")).unwrap(), b"longer clip");
        std::fs::remove_file(&a).unwrap();
        assert_eq!(backup_tagged(&tags, &roots, &backup), 0);
        assert!(backup.join(r"VALORANT\a.mp4").exists() && backup.join("old.mp4").exists());
        for d in [&clips, &backup] {
            std::fs::remove_dir_all(d).unwrap();
        }
    }

    #[test]
    fn local_time_labels() {
        let now = SystemTime::now();
        assert_eq!(day_label(now), "Today");
        assert_eq!(day_label(now - std::time::Duration::from_secs(86_400)), "Yesterday");
        assert!(time_label(now).ends_with('M'));
        // 2026-01-15 12:00 UTC is Thursday Jan 15 in US time zones.
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_768_478_400);
        assert!(day_label(t).starts_with("Thu, Jan 15"), "{}", day_label(t));
    }

    #[test]
    fn game_folder_names() {
        assert_eq!(folder_name_from_title("VALORANT  "), "VALORANT");
        assert_eq!(folder_name_from_title("Elden Ring - v1.2"), "Elden Ring");
        assert_eq!(folder_name_from_title("Counter-Strike 2"), "Counter-Strike 2");
        assert_eq!(folder_name_from_title(""), "Desktop");
        assert_eq!(folder_name_from_title("Program Manager"), "Desktop");
        assert_eq!(folder_name_from_title("a?b*c"), "abc");
    }

    #[test]
    fn free_space_reads() {
        assert!(free_space(&std::env::temp_dir().display().to_string()).unwrap() > 0);
    }
}
