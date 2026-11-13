//! The clips folder: listing, opening, recycling, free space, and thumbnails made in the background.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::media;

#[derive(Clone)]
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
    list_read(folder, max).0
}

/// `list`, and the folders it read in full (the clips folder and its game folders, `tidy`). One
/// that couldn't be read, or only partly (a NAS asleep, a drive unplugged, no access), isn't among
/// them: a clip of it missing from the list may still be there.
pub fn list_read(folder: &str, max: usize) -> (Vec<Clip>, Vec<PathBuf>) {
    // In one form (see `tidy`), so the paths match the keys stars, tags and notes are filed under.
    let folder = tidy(folder);
    let root = Path::new(&folder);
    // A folder's entries, and whether every one of them could be read.
    let entries = |dir: &Path| match std::fs::read_dir(dir) {
        Ok(read) => {
            let all: Vec<std::io::Result<std::fs::DirEntry>> = read.collect();
            let whole = all.iter().all(Result::is_ok);
            (all.into_iter().flatten().collect::<Vec<_>>(), whole)
        }
        Err(_) => (Vec::new(), false),
    };
    let mut read = Vec::new();
    let (top, whole) = entries(root);
    if whole {
        read.push(root.to_path_buf());
    }
    let mut found: Vec<(std::fs::DirEntry, Option<String>)> = Vec::new();
    for e in &top {
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            let dir = e.path();
            let (inner, whole) = entries(&dir);
            let group = Some(e.file_name().to_string_lossy().into_owned());
            found.extend(inner.into_iter().map(|i| (i, group.clone())));
            if whole {
                read.push(dir);
            }
        }
    }
    let mut clips: Vec<Clip> = top
        .into_iter()
        .map(|e| (e, None))
        .chain(found)
        .filter_map(|(e, group)| {
            let path = e.path();
            let ext = path.extension()?.to_string_lossy().to_lowercase();
            let is_picture = is_picture(&path);
            if !(is_picture || matches!(ext.as_str(), "mp4" | "mkv" | "mov")) {
                return None;
            }
            let m = e.metadata().ok()?;
            let modified = m.modified().ok()?;
            // A recording OBS is still writing can't be played or moved yet. OBS's writers let
            // others write too, so only asking to be the one writer tells (only recent files are asked).
            if modified.elapsed().is_ok_and(|age| age.as_secs() < 15) && crate::recorder::held_for_writing(&path) {
                return None;
            }
            Some(Clip { name: path.file_name()?.to_string_lossy().into_owned(), bytes: m.len(), modified, path, group, is_picture })
        })
        .collect();
    clips.sort_by_key(|c| std::cmp::Reverse(c.modified));
    clips.truncate(max);
    (clips, read)
}

/// A screenshot or GIF, not a video.
pub fn is_picture(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png") || e.eq_ignore_ascii_case("gif"))
}

/// The folder name for whatever is in front (by its window title only; the game's process is
/// never opened, so anti-cheats have nothing to see), or "Desktop".
pub fn game_folder_name() -> String {
    folder_name_from_title(&title_in_front())
}

/// The folder name a clip or screenshot being saved goes into. Also remembers when it was named
/// after a browser tab or a document, so that folder is never looked up on Steam.
pub fn saving_folder_name() -> String {
    let title = title_in_front();
    let name = folder_name_from_title(&title);
    crate::covers::note_title(&name, &title);
    name
}

fn title_in_front() -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowTextW;
    let window = crate::overlay::window_in_front();
    let mut buf = [0u16; 256];
    let n = if window.is_invalid() { 0 } else { unsafe { GetWindowTextW(window, &mut buf) } };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// "VALORANT  " -> "VALORANT", "Elden Ring - v1.2" -> "Elden Ring"; nothing usable -> "Desktop".
pub(crate) fn folder_name_from_title(title: &str) -> String {
    let title = [" - ", " | ", " \u{2014} ", ": "].iter().fold(title, |t, sep| t.split(sep).next().unwrap_or(t));
    let clean = windows_safe(title);
    let clean = clean.as_str();
    // Only dots ("..") would name a folder outside the clips folder.
    if clean.chars().all(|c| c == '.' || c.is_whitespace()) || clean == "Program Manager" {
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

/// Moves a file into `dir` (made if needed) with `prefix` and a space put in front of its name
/// (none if empty), never over an existing file; returns where it went.
/// Retries for a few seconds in case OBS hasn't let go of it yet. To another drive or a NAS,
/// which Windows can't move to, it's copied, checked and then removed.
pub fn move_into(path: &Path, dir: &Path, prefix: &str) -> std::io::Result<PathBuf> {
    if path.parent() == Some(dir) && prefix.is_empty() {
        return Ok(path.to_path_buf());
    }
    std::fs::create_dir_all(dir)?;
    let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let stem = if prefix.is_empty() { stem } else { format!("{prefix} {stem}") };
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

/// Carries a clip's star, tags, note, fingerprint and share link to where it went (renamed or
/// archived), or with `new` None (recycled) drops its star, tags, note and fingerprint; the share
/// link stays, to be deleted. Saves whatever changed (see `carry`). The fingerprints are read and
/// saved under the stores' lock, so a thread editing them at the same time loses nothing.
/// `renamed`: the user just renamed it to `new`, a name no file had (see `carry`).
pub fn moved(favs: &mut std::collections::BTreeSet<String>, tags: &mut Tags, notes: &mut Notes, old: &Path, new: Option<&Path>, renamed: bool) {
    {
        let _guard = STORES.lock();
        let mut prints = read_prints(&prints_file());
        let changed = carry(favs, tags, notes, &mut prints, old, new, renamed);
        save_carried(favs, tags, notes, &prints, changed);
    }
    if let Some(new) = new {
        crate::upload::renamed(old, new);
    }
}

/// `moved` for the archive and auto-cleanup threads: on the stores as they are on disk, read,
/// changed and saved under one lock, so a star or tag the window saves meanwhile isn't lost.
pub fn moved_on_disk(old: &Path, new: Option<&Path>) {
    {
        let _guard = STORES.lock();
        let (mut favs, mut tags, mut notes, mut prints) = (favorites(), self::tags(), self::notes(), read_prints(&prints_file()));
        let changed = carry(&mut favs, &mut tags, &mut notes, &mut prints, old, new, false);
        save_carried(&favs, &tags, &notes, &prints, changed);
    }
    if let Some(new) = new {
        crate::upload::renamed(old, new);
    }
}

/// What `moved` does to the stars, tags, notes and fingerprints, in memory: what's filed under
/// `old` goes under `new`, or is dropped (`new` None). Anything already filed under the new path
/// is dropped first: it was a clip deleted outside the app, and the clip that has that name now
/// isn't the same one. With nothing filed under `old` nothing changes, so a move reported twice
/// (the archive thread and the window both report it) keeps what the first report carried. A
/// rename by the user (`renamed`) is reported once, to a name no file had, so what's under the new
/// path goes even then. Says which of the four changed.
fn carry(favs: &mut std::collections::BTreeSet<String>, tags: &mut Tags, notes: &mut Notes, prints: &mut Prints, old: &Path, new: Option<&Path>, renamed: bool) -> [bool; 4] {
    let filed = stored_key(favs.iter(), old).is_some() || stored_key(tags.keys(), old).is_some() || stored_key(notes.keys(), old).is_some() || stored_key(prints.keys(), old).is_some();
    if new == Some(old) || !(filed || renamed) {
        return [false; 4];
    }
    let to = new.map(|n| tidy(&n.display().to_string()));
    // Only renamed in case or slashes: what's under the new path is this clip's own.
    let leftover = to.as_ref().filter(|k| fold(k) != fold(&old.display().to_string()));
    let mut f = leftover.is_some_and(|k| favs.remove(k));
    if let Some(k) = stored_key(favs.iter(), old).cloned() {
        favs.remove(&k);
        favs.extend(to.clone());
        f = true;
    }
    let t = leftover.is_some_and(|k| tags.remove(k).is_some()) | move_tags(tags, old, new);
    let n = leftover.is_some_and(|k| notes.remove(k).is_some()) | move_note(notes, old, new);
    let mut p = leftover.is_some_and(|k| prints.remove(k).is_some());
    if let Some(print) = stored_key(prints.keys(), old).cloned().and_then(|k| prints.remove(&k)) {
        prints.extend(to.map(|k| (k, print)));
        p = true;
    }
    [f, t, n, p]
}

/// Saves the stores `carry` changed. The caller holds the stores' lock.
fn save_carried(favs: &std::collections::BTreeSet<String>, tags: &Tags, notes: &Notes, prints: &Prints, [f, t, n, p]: [bool; 4]) {
    if f {
        save_unlocked(&favorites_file(), favs);
    }
    if t {
        save_unlocked(&tags_file(), tags);
    }
    if n {
        save_unlocked(&notes_file(), notes);
    }
    if p {
        save_unlocked(&prints_file(), prints);
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
    shell_recycle(path, true)
}

/// `recycle` for auto-cleanup, which runs unseen (maybe mid-game), so nothing is ever asked: a file
/// is only moved when its drive surely has a Recycle Bin that takes it. Anything else (no bin, too
/// small, or it couldn't be told) is left alone, saying why, as Windows might delete it for good.
/// The bin is judged where the file really is (`final_path`), and only when that's on the drive
/// its path shows (see `follow_refuses`).
pub fn recycle_unasked(path: &Path) -> anyhow::Result<()> {
    let size = std::fs::metadata(path)?.len();
    let typed = path.display().to_string();
    let real = final_path(&typed);
    let roots = (volume_root(path), real.as_deref().and_then(|r| volume_root(Path::new(r))));
    let why = follow_refuses(&typed, real.as_deref(), roots.0.as_deref(), roots.1.as_deref()).or_else(|| match real.as_deref() {
        Some(real) => bin_refuses(bin_on(Path::new(real)), size),
        None => Some(UNRESOLVED.into()),
    });
    if let Some(why) = why {
        anyhow::bail!("{why}, so Windows might delete it for good");
    }
    shell_recycle(path, false)
}

/// Why auto-cleanup can't trust the Recycle Bin of the drive `typed` shows, given where the file
/// really is (`real`, from `final_path`) and the top folders of both volumes (`volume_root`), or
/// None if it's on that drive. Through a link, junction or SUBST drive the bin that counts is the
/// real one's, and Windows may delete for good when the two differ; anything not known counts as no.
pub(crate) fn follow_refuses(typed: &str, real: Option<&str>, typed_root: Option<&str>, real_root: Option<&str>) -> Option<String> {
    let Some(real) = real else { return Some(UNRESOLVED.into()) };
    let real = tidy(real);
    if real.starts_with(r"\\") {
        return Some("it's really in a network folder (through a link), which has no Recycle Bin".into());
    }
    // Only a full path on a drive letter ("C:\...") is judged; a volume path or anything odd isn't.
    if !matches!(real.as_bytes(), [d, b':', b'\\', ..] if d.is_ascii_alphabetic()) {
        return Some("Windows didn't give a full drive path for where it really is".into());
    }
    let (Some(typed_root), Some(real_root)) = (typed_root, real_root) else { return Some("Windows couldn't tell which drive it's on".into()) };
    // A SUBST drive's path can name the drive it stands for: then the root isn't the path's own.
    if fold(typed_root) != fold(real_root) || relative(typed, typed_root).is_none() {
        return Some("it's really on another drive than its path shows (through a link, junction or SUBST drive)".into());
    }
    None
}

/// Why a clip whose real place (`final_path`) couldn't be told is left alone.
const UNRESOLVED: &str = "Windows couldn't tell where it really is (a link to a drive that's off?)";

/// The top folder of the volume `path` is on ("C:\", or a folder a drive is mounted in), as
/// Windows has it (GetVolumePathNameW). None if Windows can't tell.
fn volume_root(path: &Path) -> Option<String> {
    use windows::Win32::Storage::FileSystem::GetVolumePathNameW;
    let mut root = [0u16; 300];
    unsafe { GetVolumePathNameW(&windows::core::HSTRING::from(path.as_os_str()), &mut root) }.ok()?;
    // The buffer is NUL-ended.
    let len = root.iter().position(|&c| c == 0).unwrap_or(root.len());
    Some(String::from_utf16_lossy(&root[..len]))
}

/// Moves `path` to the Recycle Bin without asking; with `nuke_warning`, Windows asks before
/// deleting it for good when the bin can't take it (without, it does that silently).
fn shell_recycle(path: &Path, nuke_warning: bool) -> anyhow::Result<()> {
    use windows::Win32::UI::Shell::{FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING, SHFILEOPSTRUCTW, SHFileOperationW};
    // The list of paths ends with an extra NUL.
    let from: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().chain([0, 0]).collect();
    let warn = if nuke_warning { FOF_WANTNUKEWARNING.0 } else { 0 };
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: windows::core::PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO.0 | FOF_NOCONFIRMATION.0 | FOF_SILENT.0 | FOF_NOERRORUI.0 | warn) as u16,
        ..Default::default()
    };
    let code = unsafe { SHFileOperationW(&mut op) };
    anyhow::ensure!(code == 0 && !path.exists(), "Windows couldn't move it to the Recycle Bin (code {code})");
    Ok(())
}

/// The kind of drive a file is on, as far as the Recycle Bin goes: Windows only has one on fixed
/// drives.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Drive {
    Fixed,
    Network,
    Removable,
    Other,
}

/// What's known of the Recycle Bin on a file's drive (see `bin_on`), for `bin_refuses`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Bin {
    pub drive: Drive,
    /// Windows named the drive's volume ({guid}). A SUBST or other virtual drive has none, and
    /// Windows may delete for good there.
    pub volume: bool,
    /// Windows answered when asked about the bin on the drive (SHQueryRecycleBinW).
    pub answers: bool,
    /// The Recycle Bin is turned off on this PC by policy (NoRecycleFiles).
    pub policy_off: bool,
    /// The bin on the drive is set to delete files right away (NukeOnDelete).
    pub nuke: bool,
    /// The size set in the bin's properties, in bytes.
    pub max: Option<u64>,
    /// 5% of the drive, for when no size was ever set (Windows' own default is at least that).
    pub share: Option<u64>,
}

/// Why auto-cleanup can't be sure a file of `size` bytes would go to the Recycle Bin, or None if
/// it surely would. Anything not known counts as no: then Windows might delete it for good.
pub(crate) fn bin_refuses(bin: Bin, size: u64) -> Option<String> {
    match bin.drive {
        Drive::Fixed => {}
        Drive::Network => return Some("it's on a network drive, which has no Recycle Bin".into()),
        Drive::Removable => return Some("it's on a removable drive, which has no Recycle Bin".into()),
        Drive::Other => return Some("its drive has no Recycle Bin".into()),
    }
    if bin.policy_off {
        return Some("the Recycle Bin is turned off on this PC".into());
    }
    if !bin.volume {
        return Some("Windows didn't name its drive's volume (a SUBST or other virtual drive?), so it can't be told if there's a Recycle Bin".into());
    }
    if !bin.answers {
        return Some("Windows didn't answer when asked about the Recycle Bin on its drive".into());
    }
    if bin.nuke {
        return Some("the Recycle Bin on its drive is set to delete files right away".into());
    }
    match bin.max.or(bin.share).filter(|&m| m > 0) {
        None => Some("the size of the Recycle Bin on its drive couldn't be read".into()),
        Some(max) if size > max => Some(format!("it's bigger ({}) than the Recycle Bin on its drive holds ({})", self::size(size), self::size(max))),
        Some(_) => None,
    }
}

/// What's known of the Recycle Bin on `path`'s drive (see `Bin`).
fn bin_on(path: &Path) -> Bin {
    use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetVolumeNameForVolumeMountPointW};
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use windows::Win32::UI::Shell::{SHQUERYRBINFO, SHQueryRecycleBinW};
    use windows::core::HSTRING;
    let mut bin = Bin { drive: Drive::Other, volume: false, answers: false, policy_off: false, nuke: false, max: None, share: None };
    let Some(root) = volume_root(path) else { return bin };
    let root = &HSTRING::from(root);
    bin.drive = match unsafe { GetDriveTypeW(root) } {
        3 => Drive::Fixed,
        4 => Drive::Network,
        2 => Drive::Removable,
        _ => Drive::Other,
    };
    if bin.drive != Drive::Fixed {
        return bin;
    }
    let policy = r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer";
    bin.policy_off = [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE].into_iter().any(|r| reg_dword(r, policy, "NoRecycleFiles") == Some(1));
    let mut volume = [0u16; 64];
    let name = unsafe { GetVolumeNameForVolumeMountPointW(root, &mut volume) }.ok().map(|()| String::from_utf16_lossy(&volume));
    // "\\?\Volume{guid}\": the bin's settings are filed under the {guid}.
    let guid = name.and_then(|v| v.get(v.find('{')?..=v.find('}')?).map(str::to_string));
    bin.volume = guid.is_some();
    let mut info = SHQUERYRBINFO { cbSize: size_of::<SHQUERYRBINFO>() as u32, ..Default::default() };
    bin.answers = unsafe { SHQueryRecycleBinW(root, &mut info) }.is_ok();
    let key = guid.map(|g| format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\{g}"));
    let setting = |value: &str| key.as_deref().and_then(|k| reg_dword(HKEY_CURRENT_USER, k, value));
    bin.nuke = setting("NukeOnDelete") == Some(1);
    bin.max = setting("MaxCapacity").map(|mb| u64::from(mb) << 20);
    let mut total = 0u64;
    bin.share = unsafe { GetDiskFreeSpaceExW(root, None, Some(&mut total), None) }.ok().map(|()| total / 20);
    bin
}

/// A DWORD from the registry, if it's there.
fn reg_dword(root: windows::Win32::System::Registry::HKEY, key: &str, value: &str) -> Option<u32> {
    use windows::Win32::System::Registry::{RRF_RT_REG_DWORD, RegGetValueW};
    use windows::core::HSTRING;
    let (mut data, mut len) = (0u32, 4u32);
    let r = unsafe { RegGetValueW(root, &HSTRING::from(key), &HSTRING::from(value), RRF_RT_REG_DWORD, None, Some((&mut data as *mut u32).cast()), Some(&mut len)) };
    r.is_ok().then_some(data)
}

/// Stars, tags, notes and fingerprints: read from the file, or from its backup when the file is
/// missing or can't be read.
pub(crate) fn load_json<T: serde::de::DeserializeOwned + Default>(file: &Path) -> T {
    let read = |f: &Path| std::fs::read_to_string(f).ok().and_then(|s| serde_json::from_str(&s).ok());
    read(file).or_else(|| read(&file.with_extension("json.bak"))).unwrap_or_default()
}

/// Held across a read, change and save of the stars, tags, notes, fingerprints and lost clips
/// (`edit_store`, `moved`), and by every save of them, so one never lands in the middle of another's
/// edit. write_json's own lock only covers one write.
static STORES: Mutex<()> = Mutex::new(());

/// `write_json`, logging a failure, under the stores' lock. False if it failed.
pub(crate) fn save_json<T: serde::Serialize + serde::de::DeserializeOwned>(file: &Path, value: &T) -> bool {
    let _guard = STORES.lock();
    save_unlocked(file, value)
}

/// Reads the store in `file` with `read`, lets `change` edit it and saves it if `change` says it
/// changed, all under the stores' lock: the window and the archive, cleanup and scan threads can
/// edit one at once without losing each other's changes. `change` must not save a store itself.
/// False if the save failed.
pub(crate) fn edit_store<T: serde::Serialize + serde::de::DeserializeOwned>(file: &Path, read: impl FnOnce(&Path) -> T, change: impl FnOnce(&mut T) -> bool) -> bool {
    let _guard = STORES.lock();
    let mut value = read(file);
    !change(&mut value) || save_unlocked(file, &value)
}

/// `save_json` for a caller already holding the stores' lock.
fn save_unlocked<T: serde::Serialize + serde::de::DeserializeOwned>(file: &Path, value: &T) -> bool {
    let saved = write_json(file, value);
    if let Err(e) = &saved {
        crate::log::write(&format!("Couldn't save {}: {e}", file.display()));
    }
    saved.is_ok()
}

/// Written to a new file, flushed to the disk, that then replaces the old one, so a crash, power
/// cut or full disk mid-write never leaves half a file. The old one is kept as `.json.bak` once a
/// day, so a bad day can be undone (and a damaged file has something to come back from).
/// Only an old file that reads back as a `T` is kept: one that doesn't (`[]` for the settings, say)
/// is what the backup is there to come back from, so it never takes its place.
pub(crate) fn write_json<T: serde::Serialize + serde::de::DeserializeOwned>(file: &Path, value: &T) -> std::io::Result<()> {
    use std::io::Write;
    // The window, the archive and the cleanup threads can save at once: one at a time.
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock();
    let _ = std::fs::create_dir_all(file.parent().unwrap_or(Path::new(".")));
    let (tmp, bak) = (file.with_extension("json.tmp"), file.with_extension("json.bak"));
    let text = serde_json::to_string_pretty(value)?;
    // Without the flush, a power cut soon after can leave the renamed file empty.
    let written = std::fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(text.as_bytes())?;
        f.sync_all()
    });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let bak_age = std::fs::metadata(&bak).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
    // Only a file that reads back as what's saved in it is worth keeping as the backup.
    let readable = || std::fs::read_to_string(file).ok().is_some_and(|s| serde_json::from_str::<T>(&s).is_ok());
    if bak_age.is_none_or(|a| a > std::time::Duration::from_secs(86_400)) && readable() && std::fs::copy(file, &bak).is_ok() {
        // copy keeps the old file's time; the backup's age is from now.
        let _ = std::fs::File::options().write(true).open(&bak).and_then(|f| f.set_modified(std::time::SystemTime::now()));
    }
    // Antivirus or a reader can hold the file for a moment.
    for tries in 0.. {
        match std::fs::rename(&tmp, file) {
            Ok(()) => break,
            Err(_) if tries < 5 => std::thread::sleep(std::time::Duration::from_millis(200)),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        }
    }
    Ok(())
}

fn favorites_file() -> PathBuf {
    if cfg!(test) { std::env::temp_dir().join("instant-replay-favorites-test.json") } else { crate::config::dir().join("favorites.json") }
}

/// Starred clips, by path.
pub fn favorites() -> std::collections::BTreeSet<String> {
    read_favorites(&favorites_file())
}

/// Read with the paths in one form (see `tidy_keys`); the next save writes them that way.
fn read_favorites(file: &Path) -> std::collections::BTreeSet<String> {
    let favs: std::collections::BTreeSet<String> = load_json(file);
    tidy_keys(favs.into_iter().map(|k| (k, ())).collect(), |_, ()| {}).into_keys().collect()
}

pub fn save_favorites(favs: &std::collections::BTreeSet<String>) -> bool {
    save_json(&favorites_file(), favs)
}

/// Tags ("Aces", "Funny") by clip path.
pub type Tags = std::collections::BTreeMap<String, std::collections::BTreeSet<String>>;

fn tags_file() -> PathBuf {
    if cfg!(test) { std::env::temp_dir().join("instant-replay-tags-test.json") } else { crate::config::dir().join("tags.json") }
}

pub fn tags() -> Tags {
    read_tags(&tags_file())
}

pub fn save_tags(tags: &Tags) -> bool {
    write_tags(&tags_file(), tags)
}

fn read_tags(file: &Path) -> Tags {
    tidy_keys(load_json(file), |a, b| a.extend(b))
}

fn write_tags(file: &Path, tags: &Tags) -> bool {
    save_json(file, tags)
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

/// Carries a clip's tags to its new path after a rename, or drops them (`new` None, recycled).
/// True if anything changed.
pub fn move_tags(tags: &mut Tags, old: &Path, new: Option<&Path>) -> bool {
    let Some(set) = stored_key(tags.keys(), old).cloned().and_then(|k| tags.remove(&k)) else { return false };
    if let Some(new) = new {
        tags.insert(tidy(&new.display().to_string()), set);
    }
    true
}

/// A one-line note per clip ("Clutch 1v4 on B site"), by clip path, kept in notes.json like the tags.
pub type Notes = std::collections::BTreeMap<String, String>;

/// The longest a note can be, in characters.
pub const NOTE_MAX: usize = 120;

fn notes_file() -> PathBuf {
    if cfg!(test) { std::env::temp_dir().join("instant-replay-notes-test.json") } else { crate::config::dir().join("notes.json") }
}

pub fn notes() -> Notes {
    read_notes(&notes_file())
}

pub fn save_notes(notes: &Notes) -> bool {
    write_notes(&notes_file(), notes)
}

fn read_notes(file: &Path) -> Notes {
    // The same clip written two ways keeps the first note; both are rarely set.
    tidy_keys(load_json(file), |_, _| {})
}

fn write_notes(file: &Path, notes: &Notes) -> bool {
    save_json(file, notes)
}

/// Sets a clip's note: one line (line breaks become spaces), trimmed, at most NOTE_MAX characters.
/// An empty note takes it off.
pub fn set_note(notes: &mut Notes, path: &Path, text: &str) {
    let line: String = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(NOTE_MAX).collect();
    let key = path.display().to_string();
    if line.trim().is_empty() {
        notes.remove(&key);
    } else {
        notes.insert(key, line.trim_end().to_string());
    }
}

/// Carries a clip's note to its new path, or drops it (`new` None, recycled). True if it had one.
pub fn move_note(notes: &mut Notes, old: &Path, new: Option<&Path>) -> bool {
    let Some(note) = stored_key(notes.keys(), old).cloned().and_then(|k| notes.remove(&k)) else { return false };
    if let Some(new) = new {
        notes.insert(tidy(&new.display().to_string()), note);
    }
    true
}

/// Fingerprints (`fingerprint`) of starred, tagged and noted clips by path, so they can be found
/// again after a rename or move outside the app. Kept in a file of their own: tags.json and
/// favorites.json stay as they were, and entries from before fingerprints get one once their clip
/// is next listed.
pub type Prints = std::collections::BTreeMap<String, String>;

fn prints_file() -> PathBuf {
    if cfg!(test) { std::env::temp_dir().join("instant-replay-prints-test.json") } else { crate::config::dir().join("fingerprints.json") }
}

pub fn prints() -> Prints {
    read_prints(&prints_file())
}

fn read_prints(file: &Path) -> Prints {
    tidy_keys(load_json(file), |_, _| {})
}

/// A file's size and a hash of its first and last 64 KB, as "size:hash": quick to work out even
/// for a GB clip on a NAS, and enough to tell clips apart.
pub fn fingerprint(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let size = f.metadata().ok()?.len();
    let mut part = vec![0; size.min(64 << 10) as usize];
    let mut hash = Sha256::new();
    f.read_exact(&mut part).ok()?;
    hash.update(&part);
    f.seek(SeekFrom::Start(size - part.len() as u64)).ok()?;
    f.read_exact(&mut part).ok()?;
    hash.update(&part);
    Some(format!("{size}:{}", hash.finalize().iter().take(8).map(|b| format!("{b:02x}")).collect::<String>()))
}

/// Finds starred, tagged and noted clips (`marked`) again after a rename or move outside the app.
/// First fingerprints the marked clips in `scanned` that have none yet. A marked path that's listed
/// written another way (case, slashes) is that clip. Then each marked path with a fingerprint that's
/// gone from under `roots`, or is one of the `lost` ones (from an old clips folder), is looked for in
/// `scanned` (only clips of the same size are read). Returns the moves (old, new), for `moved` to
/// carry the star, tags, note, fingerprint and share link, and whether `prints` changed (it gets the
/// new fingerprints; the found ones stay under their old path until `moved` carries them).
pub fn refind(marked: &std::collections::BTreeSet<String>, prints: &mut Prints, scanned: &[&Clip], roots: &[&Path], lost: &std::collections::BTreeSet<String>) -> (Vec<(PathBuf, PathBuf)>, bool) {
    let mut changed = false;
    for c in scanned {
        let key = c.path.display().to_string();
        if marked.contains(&key)
            && !prints.contains_key(&key)
            && let Some(print) = fingerprint(&c.path)
        {
            prints.insert(key, print);
            changed = true;
        }
    }
    let listed: std::collections::HashSet<&Path> = scanned.iter().map(|c| c.path.as_path()).collect();
    let mut moves: Vec<(PathBuf, PathBuf)> = Vec::new();
    // Windows finds these at their old key too, so they'd never look gone.
    let unlisted: Vec<&String> = marked.iter().filter(|k| !listed.contains(Path::new(k))).collect();
    if !unlisted.is_empty() {
        let by_fold: HashMap<String, &Clip> = scanned.iter().map(|c| (fold(&c.path.display().to_string()), *c)).collect();
        for k in &unlisted {
            if let Some(c) = by_fold.get(&fold(k))
                && !marked.contains(&c.path.display().to_string())
            {
                moves.push((PathBuf::from(k.as_str()), c.path.clone()));
            }
        }
    }
    let gone: Vec<(String, String)> = prints
        .iter()
        .filter(|(k, _)| marked.contains(*k) && !listed.contains(Path::new(k)) && !moves.iter().any(|(o, _)| o == Path::new(k)))
        .filter(|(k, _)| lost.contains(*k) || (roots.iter().any(|r| inside(k, &r.display().to_string())) && !Path::new(k).exists()))
        .map(|(k, p)| (k.clone(), p.clone()))
        .collect();
    for (old, print) in gone {
        let size = print.split(':').next().and_then(|s| s.parse::<u64>().ok());
        let found = scanned.iter().find(|c| {
            Some(c.bytes) == size && !marked.contains(&c.path.display().to_string()) && !moves.iter().any(|(_, n)| *n == c.path) && fingerprint(&c.path).as_ref() == Some(&print)
        });
        if let Some(c) = found {
            moves.push((PathBuf::from(old), c.path.clone()));
        }
    }
    (moves, changed)
}

/// `refind` with the fingerprints file, for the clips folder's scan thread. The new fingerprints
/// are worked out without holding the stores' lock (reading clips on a NAS can take a while), then
/// added under it, each only where there's none yet. Returns the moves.
pub fn refind_saved(marked: &std::collections::BTreeSet<String>, scanned: &[&Clip], roots: &[&Path], lost: &std::collections::BTreeSet<String>) -> Vec<(PathBuf, PathBuf)> {
    let mut prints = prints();
    let had: std::collections::HashSet<String> = prints.keys().cloned().collect();
    let (moves, changed) = refind(marked, &mut prints, scanned, roots, lost);
    if changed {
        edit_store(&prints_file(), read_prints, |p| {
            let before = p.len();
            for (k, print) in prints.into_iter().filter(|(k, _)| !had.contains(k)) {
                p.entry(k).or_insert(print);
            }
            p.len() != before
        });
    }
    moves
}

/// A clip path (or folder) the way stars, tags, notes, fingerprints and share links are filed
/// under it: backslashes, no `\\?\` in front, no doubled or trailing separators. The case is kept,
/// for showing; `fold` is the form to compare.
pub fn tidy(path: &str) -> String {
    let path = path.replace('/', "\\");
    let path = if let Some(rest) = path.strip_prefix(r"\\?\UNC\") { format!(r"\\{rest}") } else { path.strip_prefix(r"\\?\").map_or(path.clone(), str::to_string) };
    // A network path keeps its two backslashes in front.
    let unc = path.starts_with(r"\\");
    let mut out = String::with_capacity(path.len());
    if unc {
        out.push_str(r"\\");
    } else if path.starts_with('\\') {
        out.push('\\');
    }
    for part in path.split('\\').filter(|p| !p.is_empty()) {
        if !out.is_empty() && !out.ends_with('\\') {
            out.push('\\');
        }
        out.push_str(part);
    }
    // A drive's top folder keeps its backslash ("D:\"), as "D:" alone means something else.
    if out.len() == 2 && out.ends_with(':') && path.len() > 2 {
        out.push('\\');
    }
    out
}

/// `tidy` without case: two paths Windows takes for the same file fold to the same text.
pub fn fold(path: &str) -> String {
    tidy(path).to_lowercase()
}

/// The key `path` is filed under among `keys`: written exactly like it, or else the same but for
/// case or slashes (a clips folder setting retyped, or keys from before they were tidied).
pub fn stored_key<'a>(keys: impl Iterator<Item = &'a String> + Clone, path: &Path) -> Option<&'a String> {
    let key = path.display().to_string();
    keys.clone().find(|k| **k == key).or_else(|| {
        let key = fold(&key);
        keys.into_iter().find(|k| fold(k) == key)
    })
}

/// `map` with its keys tidied (`tidy`). Entries for one clip written two ways (case, slashes, a
/// `\\?\` in front) become one, the later put into the first with `merge`.
pub(crate) fn tidy_keys<V>(map: std::collections::BTreeMap<String, V>, merge: impl Fn(&mut V, V)) -> std::collections::BTreeMap<String, V> {
    let mut out: std::collections::BTreeMap<String, V> = std::collections::BTreeMap::new();
    let mut by_fold: HashMap<String, String> = HashMap::new();
    for (key, value) in map {
        let key = tidy(&key);
        match by_fold.get(&key.to_lowercase()).and_then(|k| out.get_mut(k)) {
            Some(first) => merge(first, value),
            None => {
                by_fold.insert(key.to_lowercase(), key.clone());
                out.insert(key, value);
            }
        }
    }
    out
}

/// `key` relative to the folder `root` (compared without case), if it's inside it.
pub(crate) fn relative(key: &str, root: &str) -> Option<PathBuf> {
    let (key, root) = (tidy(key), tidy(root));
    let mut parts = Path::new(&key).components();
    for r in Path::new(&root).components() {
        let k = parts.next()?;
        if k.as_os_str().to_string_lossy().to_lowercase() != r.as_os_str().to_string_lossy().to_lowercase() {
            return None;
        }
    }
    let rest = parts.as_path();
    (!rest.as_os_str().is_empty()).then(|| rest.to_path_buf())
}

/// What a change of the clips folder from `old` to `new` does to the clips filed under `keys`:
/// each key inside `old` moves to the same place inside `new` when the file is there (or always,
/// when only the folder's case or slashes changed). Ones whose file is still at the old place stay.
/// The rest are lost: neither here nor there (moved by hand, renamed, on a drive that's gone).
/// Returns (moves as (old key, new key), lost keys). `exists` looks at the disk.
pub fn refolder_plan<'a>(keys: impl IntoIterator<Item = &'a String>, old: &str, new: &str, exists: impl Fn(&Path) -> bool) -> (Vec<(String, String)>, Vec<String>) {
    let same = fold(old) == fold(new);
    let (mut moves, mut lost) = (Vec::new(), Vec::new());
    for key in keys {
        let Some(rel) = relative(key, old) else { continue };
        let to = tidy(&Path::new(&tidy(new)).join(&rel).display().to_string());
        if same || exists(Path::new(&to)) {
            if to != *key {
                moves.push((key.clone(), to));
            }
        } else if !exists(Path::new(key)) {
            lost.push(key.clone());
        }
    }
    (moves, lost)
}

/// Files the entries under each old key of `moves` under its new key instead. What's already
/// under a new key stays (it's that file's); the carried entry only fills in. True if anything changed.
fn rekey_map<V>(map: &mut std::collections::BTreeMap<String, V>, moves: &[(String, String)], merge: impl Fn(&mut V, V)) -> bool {
    let mut changed = false;
    for (old, new) in moves {
        if let Some(v) = map.remove(old) {
            match map.get_mut(new) {
                Some(have) => merge(have, v),
                None => _ = map.insert(new.clone(), v),
            }
            changed = true;
        }
    }
    changed
}

/// `refolder_plan`'s moves for the stars, tags, notes, fingerprints and share links, each saved
/// once if it changed. False if any of them couldn't be saved.
pub fn rekey(favs: &mut std::collections::BTreeSet<String>, tags: &mut Tags, notes: &mut Notes, moves: &[(String, String)]) -> bool {
    if moves.is_empty() {
        return true;
    }
    let saved = rekey_in([&favorites_file(), &tags_file(), &notes_file(), &prints_file()], favs, tags, notes, moves);
    crate::upload::rekeyed(|links| rekey_map(links, moves, |_, _| {})) && saved
}

/// `rekey` for the stars, tags, notes and fingerprints saved in `files`, in that order (not the
/// share links).
fn rekey_in(files: [&Path; 4], favs: &mut std::collections::BTreeSet<String>, tags: &mut Tags, notes: &mut Notes, moves: &[(String, String)]) -> bool {
    let mut saved = true;
    let mut f: std::collections::BTreeMap<String, ()> = favs.iter().map(|k| (k.clone(), ())).collect();
    if rekey_map(&mut f, moves, |_, ()| {}) {
        *favs = f.into_keys().collect();
        saved &= save_json(files[0], favs);
    }
    if rekey_map(tags, moves, |a, b| a.extend(b)) {
        saved &= write_tags(files[1], tags);
    }
    if rekey_map(notes, moves, |_, _| {}) {
        saved &= write_notes(files[2], notes);
    }
    edit_store(files[3], read_prints, |p| rekey_map(p, moves, |_, _| {})) && saved
}

/// Starred, tagged and noted clips that a change of the clips folder couldn't carry over (see
/// `refolder_plan`), by their old key, until they're found again or the user lets them go. While
/// any of them is starred, auto-cleanup holds off: it can't tell those clips from the rest.
fn lost_file() -> PathBuf {
    if cfg!(test) { std::env::temp_dir().join("instant-replay-lost-test.json") } else { crate::config::dir().join("lost.json") }
}

pub fn lost() -> std::collections::BTreeSet<String> {
    read_favorites(&lost_file())
}

pub fn save_lost(lost: &std::collections::BTreeSet<String>) -> bool {
    save_json(&lost_file(), lost)
}

/// The clips folder the stars, tags, notes, fingerprints and share links were last filed against,
/// recorded by `refolder` once they're saved under it. A folder changed outside Save (settings
/// edited by hand or restored from the backup, or a crash before they followed) shows at the next
/// start as a different one (`refile_at_start`).
fn filed_file() -> PathBuf {
    if cfg!(test) { std::env::temp_dir().join("instant-replay-filed-test.json") } else { crate::config::dir().join("filed.json") }
}

pub fn filed() -> Option<String> {
    read_filed(&filed_file())
}

fn read_filed(file: &Path) -> Option<String> {
    load_json::<Option<String>>(file).filter(|f| !f.trim().is_empty())
}

pub fn save_filed(folder: &str) {
    save_filed_in(&filed_file(), folder, true);
}

/// `save_filed` after a change of folder, only when `saved`: every store saved under `folder`.
/// Otherwise the old record stays, so the next start re-keys from there again (`refile_at_start`),
/// and auto-cleanup holds off meanwhile (`unfiled`). True if `folder` was recorded.
pub fn save_filed_if(folder: &str, saved: bool) -> bool {
    save_filed_in(&filed_file(), folder, saved)
}

fn save_filed_in(file: &Path, folder: &str, saved: bool) -> bool {
    if saved {
        save_json(file, &folder.to_string())
    } else {
        crate::log::write(&format!("Some stars, tags, notes or share links couldn't be saved under the clips folder {folder}; that's tried again at the next start."));
        false
    }
}

/// Whether the stores aren't filed against the clips folder `folder` (`filed`, compared as the keys
/// are): a change of folder they couldn't all be saved under yet. Auto-cleanup holds off meanwhile,
/// as the stars it reads from disk are still under the old folder's keys. None recorded isn't.
pub fn unfiled(filed: Option<&str>, folder: &str) -> bool {
    filed.is_some_and(|f| fold(f) != fold(folder))
}

/// What to do at start about where the stores were filed (`filed`).
#[derive(PartialEq, Debug)]
pub enum Refile {
    Nothing,
    /// None recorded yet (the first start of a version that records it): record the clips folder
    /// as it is, nothing is re-keyed.
    Record,
    /// The clips folder changed from this one outside Save: `refolder` from it.
    From(String),
}

/// `Refile` for the folder `recorded` (`filed`) and the clips folder setting `current`, compared
/// without case or slashes, as the keys are.
pub fn refile_at_start(recorded: Option<&str>, current: &str) -> Refile {
    match recorded {
        _ if current.trim().is_empty() => Refile::Nothing,
        None => Refile::Record,
        Some(r) if fold(r) == fold(current) => Refile::Nothing,
        Some(r) => Refile::From(r.to_string()),
    }
}

/// Stars an earlier clips folder left behind, from versions before stars followed a change of
/// folder (`refolder`): starred keys outside `folder` and the archive folder `archive` (empty:
/// none), judged from the text alone, so a drive or NAS that's off is never contacted. Returns
/// those with a fingerprint in `prints`, for `lost` (`refind` looks for them, cleanup holds
/// meanwhile), and those without, which can't be found again: holding cleanup for them would be
/// for good, so they're only logged.
pub fn stray_stars(favs: &std::collections::BTreeSet<String>, prints: &Prints, folder: &str, archive: &str) -> (std::collections::BTreeSet<String>, Vec<String>) {
    let printed: std::collections::HashSet<String> = prints.keys().map(|k| fold(k)).collect();
    let outside = |k: &&String| [folder, archive].iter().all(|r| r.trim().is_empty() || relative(k, r).is_none());
    let (lost, unknown): (Vec<String>, Vec<String>) = favs.iter().filter(outside).cloned().partition(|k| printed.contains(&fold(k)));
    (lost.into_iter().collect(), unknown)
}

/// Once, the first start of a version with lost.json (it's written even when empty, so it marks
/// the pass done; it also outlives a settings reset, unlike a setting): adds `stray_stars` to
/// `lost` and saves it, logging the ones that can't be found again.
pub fn adopt_stray_stars(lost: &mut std::collections::BTreeSet<String>, favs: &std::collections::BTreeSet<String>, folder: &str, archive: &str) {
    if lost_file().exists() {
        return;
    }
    let (strays, unknown) = stray_stars(favs, &prints(), folder, archive);
    if !strays.is_empty() {
        crate::log::write(&format!("{} starred clips from an earlier clips folder are looked for; auto-cleanup waits until they're found or let go (General > Storage).", strays.len()));
    }
    if !unknown.is_empty() {
        let some: Vec<&str> = unknown.iter().take(10).map(String::as_str).collect();
        crate::log::write(&format!("{} starred clips from an earlier clips folder can't be looked for (never fingerprinted), so they stay as they are: {}{}", unknown.len(), some.join(", "), if unknown.len() > some.len() { ", ..." } else { "" }));
    }
    lost.extend(strays);
    save_lost(lost);
}

/// How many of the `lost` clips are starred: auto-cleanup holds off while there are any.
pub fn lost_stars(lost: &std::collections::BTreeSet<String>, favs: &std::collections::BTreeSet<String>) -> usize {
    lost.iter().filter(|k| favs.contains(*k)).count()
}

/// The folders of the user's own files (Videos, Pictures, Desktop, Documents, Downloads and the
/// user folder itself), where Windows has them on this PC (moved or redirected ones too). Looked up
/// once.
pub fn known_folders() -> &'static [(&'static str, PathBuf)] {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Pictures, FOLDERID_Profile, FOLDERID_Videos, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    static KNOWN: std::sync::OnceLock<Vec<(&'static str, PathBuf)>> = std::sync::OnceLock::new();
    KNOWN.get_or_init(|| {
        let ids = [("Videos", FOLDERID_Videos), ("Pictures", FOLDERID_Pictures), ("Desktop", FOLDERID_Desktop), ("Documents", FOLDERID_Documents), ("Downloads", FOLDERID_Downloads), ("user", FOLDERID_Profile)];
        ids.into_iter()
            .filter_map(|(name, id)| unsafe {
                let p = SHGetKnownFolderPath(&id, KF_FLAG_DEFAULT, None).ok()?;
                let path = p.to_string().ok();
                CoTaskMemFree(Some(p.0 as *const _));
                Some((name, PathBuf::from(path?)))
            })
            .collect()
    })
}

/// Why auto-cleanup and the archive must leave `folder` alone, or None if they can work in it.
/// They act on every video and picture in it and in the folders right inside it, so a drive's top
/// folder, one of the `known` folders (`known_folders`) or a folder holding one would take the
/// user's own files too. A folder of its own inside one (Videos\Replay Vault Clips) is fine.
pub fn shared_folder(folder: &str, known: &[(&str, PathBuf)]) -> Option<String> {
    let tidy = tidy(folder.trim());
    if tidy.is_empty() {
        return None;
    }
    let parts = Path::new(&tidy).components().count();
    // "D:\" is a prefix and a root; "\\nas\share" is one prefix, with its root.
    if parts <= 2 && Path::new(&tidy).has_root() {
        let top = tidy.trim_end_matches('\\');
        return Some(if tidy.starts_with(r"\\") { format!("the top of {top}") } else { format!("the top of drive {top}") });
    }
    let f = tidy.to_lowercase();
    if let Some((name, _)) = known.iter().find(|(_, k)| fold(&k.display().to_string()) == f) {
        return Some(format!("your {name} folder"));
    }
    let holds = known.iter().find(|(_, k)| k.parent().is_some_and(|p| fold(&p.display().to_string()) == f));
    holds.map(|(name, _)| format!("the folder that holds your {name} folder"))
}

/// `shared_folder`, unless the user said to use `folder` anyway: `ok` is the folder they
/// confirmed (Config::shared_folder_ok), which counts only while it's this same folder (compared
/// folded), so changing the clips folder asks again. `real` is `folder` and `ok` is the confirmed
/// one in their final forms (`final_path`), or as typed when those couldn't be worked out: so a
/// link to a shared folder is caught, and a confirm holds for the same folder written another way
/// but for no other. `folder` is judged both as typed and as `real`.
pub fn rules_off(folder: &str, real: &str, ok: &str, known: &[(&str, PathBuf)]) -> Option<String> {
    let confirmed = !ok.trim().is_empty() && fold(real.trim()) == fold(ok.trim());
    if confirmed { None } else { shared_folder(real, known).or_else(|| shared_folder(folder, known)) }
}

/// `path` the way Windows finally has it, `tidy`: links and junctions, SUBST and mapped drives,
/// short names and `.` or `..` followed. None when it can't be (not there, or the NAS is off).
/// File I/O that can wait on a NAS, so never on the UI thread.
pub fn final_path(path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    std::fs::canonicalize(path).ok().map(|p| tidy(&p.display().to_string()))
}

/// `known`, with each folder also in its final form (`final_path`) when that's written another
/// way, so the clips folder's final form is compared with theirs.
fn with_final(known: &[(&'static str, PathBuf)]) -> Vec<(&'static str, PathBuf)> {
    let mut out = known.to_vec();
    for (name, path) in known {
        let text = path.display().to_string();
        if let Some(real) = final_path(&text).filter(|r| fold(r) != fold(&text)) {
            out.push((name, PathBuf::from(real)));
        }
    }
    out
}

/// `rules_off` with `folder` and `ok` followed to their final forms (`final_path`) where they can
/// be, among `known` (already `with_final`).
fn folder_risk_in(folder: &str, ok: &str, known: &[(&str, PathBuf)]) -> Option<String> {
    let real = |p: &str| final_path(p).unwrap_or_else(|| p.trim().to_string());
    rules_off(folder, &real(folder), &real(ok), known)
}

/// `rules_off` for this PC, with the folders followed to where they really are. File I/O, so for
/// the cleanup and archive threads, or off the UI thread.
pub fn folder_risk(folder: &str, ok: &str) -> Option<String> {
    static KNOWN: std::sync::OnceLock<Vec<(&'static str, PathBuf)>> = std::sync::OnceLock::new();
    folder_risk_in(folder, ok, KNOWN.get_or_init(|| with_final(known_folders())))
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
    // Compared without case or slashes: a star filed under the folder typed another way still counts.
    let favs: std::collections::HashSet<String> = favs.iter().map(|k| fold(k)).collect();
    let mut others: Vec<&Clip> = clips.iter().filter(|c| !favs.contains(&fold(&c.path.display().to_string()))).collect();
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

static CLEANUP: Mutex<Option<(String, Cleanup, String)>> = Mutex::new(None);

/// The folder and limits auto-cleanup works with, and the folder it may work in anyway (`ok`, see
/// `rules_off`), from the saved settings.
pub fn set_cleanup(folder: &str, ok: &str, rule: Cleanup) {
    *CLEANUP.lock().unwrap() = Some((folder.to_string(), rule, ok.to_string()));
}

/// Once a minute, moves what `cleanup_plan` picks to the Recycle Bin. A clip that can't be moved
/// (open somewhere, or one the Recycle Bin wouldn't surely take: see `recycle_unasked`) is left
/// alone after that.
pub fn start_cleanup() {
    let mut skipped = std::collections::HashSet::new();
    let (mut held, mut risky) = (false, false);
    every(60, &CLEANUP, move |(folder, rule, ok)| {
        if rule == Cleanup::default() || folder.is_empty() {
            return;
        }
        // It would take the user's own videos and pictures too.
        if let Some(why) = folder_risk(&folder, &ok) {
            if !risky {
                crate::log::write(&format!("Auto-cleanup is off: the clips folder {folder} is {why}, where it would also remove your own videos and pictures."));
            }
            risky = true;
            return;
        }
        risky = false;
        // The stars here couldn't all be saved under this folder: some would look unstarred.
        if unfiled(filed().as_deref(), &folder) {
            if !held {
                crate::log::write(&format!("Auto-cleanup is holding off: the stars couldn't be saved for the clips folder {folder}; that's tried again at the next start (see General > Storage)."));
            }
            held = true;
            return;
        }
        let favs = favorites();
        // Starred clips from an old clips folder that weren't found: they'd look like any other clip.
        let lost = lost_stars(&lost(), &favs);
        if lost > 0 {
            if !held {
                crate::log::write(&format!("Auto-cleanup is holding off: {lost} starred clips from an earlier clips folder weren't found (see General > Storage)."));
            }
            held = true;
            return;
        }
        held = false;
        let plan = cleanup_plan(&list(&folder, usize::MAX), &favs, rule, SystemTime::now());
        cleanup_pass(plan, &mut skipped, favorites, recycle_unasked, |path| {
            // Its tags and note go too, as when it's recycled in the window.
            moved_on_disk(path, None);
            // Its share link too: nothing would show it any more. Before the window hears of the
            // clip, so it reads the links without this one.
            if let Err(e) = crate::upload::delete_link(path) {
                crate::log::write(&format!("Auto-cleanup couldn't delete the share link of {}: {e:#}", path.display()));
            }
            MOVED.lock().unwrap().push((path.to_path_buf(), None));
            crate::log::write(&format!("Auto-cleanup moved {} to the Recycle Bin (its drive was checked to have one that takes it).", path.display()));
        });
    });
}

/// One auto-cleanup pass: moves each clip in `plan` to the Recycle Bin with `recycle`, then calls
/// `done` for it. One starred since the plan was made is spared: `favorites` is read again before
/// each (the window saves a star right away). One that can't be moved is logged and goes in
/// `skipped`, which is left alone from then on.
fn cleanup_pass(plan: Vec<PathBuf>, skipped: &mut std::collections::HashSet<PathBuf>, favorites: impl Fn() -> std::collections::BTreeSet<String>, recycle: impl Fn(&Path) -> anyhow::Result<()>, mut done: impl FnMut(&Path)) {
    for path in plan {
        if skipped.contains(&path) || stored_key(favorites().iter(), &path).is_some() {
            continue;
        }
        match recycle(&path) {
            Ok(()) => done(&path),
            Err(e) => {
                crate::log::write(&format!("Auto-cleanup left {} alone: {e:#}", path.display()));
                skipped.insert(path);
            }
        }
    }
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

/// The clips in the archive folder and its game folders, newest first; None if it isn't there
/// (the drive or NAS is off).
pub fn archived(dir: &Path) -> Option<Vec<Clip>> {
    dir.is_dir().then(|| list(&dir.to_string_lossy(), usize::MAX))
}

static ARCHIVE: Mutex<Option<(String, Archive, String)>> = Mutex::new(None);
/// Clips the archive moved (old, new) or auto-cleanup recycled (old, None), for the window to
/// carry or drop stars, tags and notes in its own copy. Reloading the files instead could lose a
/// star or tag set while the thread was writing.
static MOVED: Mutex<Vec<(PathBuf, Option<PathBuf>)>> = Mutex::new(Vec::new());

pub fn take_moved() -> Vec<(PathBuf, Option<PathBuf>)> {
    std::mem::take(&mut MOVED.lock().unwrap())
}
/// Set while the archive folder wasn't there the last time it was looked for.
pub static ARCHIVE_MISSING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The clips folder and archive rule to work with, and the folder it may work in anyway (`ok`, see
/// `rules_off`), from the saved settings; None is off.
pub fn set_archive(folder: &str, ok: &str, rule: Option<Archive>) {
    *ARCHIVE.lock().unwrap() = rule.map(|r| (folder.to_string(), r, ok.to_string()));
}

/// Every 5 minutes, moves what `archive_plan` picks into the archive's folder of the same name
/// (VALORANT\x.mp4 to <archive>\VALORANT\x.mp4), with its star, tags, note and share link. If the
/// archive folder isn't there (a NAS that's off), waits for it, saying so in the log once.
pub fn start_archive() {
    use std::sync::atomic::Ordering::Relaxed;
    let mut skipped = std::collections::HashSet::new();
    let mut held = false;
    every(300, &ARCHIVE, move |(folder, rule, ok)| {
        if let Some(why) = folder_risk(&folder, &ok) {
            if !held {
                crate::log::write(&format!("The archive is off: the clips folder {folder} is {why}, where it would also move your own videos and pictures."));
            }
            held = true;
            return;
        }
        held = false;
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
            match move_into(&c.path, &dir, "") {
                Ok(new) => {
                    // Written here too, so it holds while the window is asleep in the tray. Read
                    // fresh: a copy to a NAS can take a while.
                    moved_on_disk(&c.path, Some(&new));
                    MOVED.lock().unwrap().push((c.path.clone(), Some(new.clone())));
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

/// Opens a file with its default app (the always-on-top window hides so it isn't in the way), or
/// shows it selected in File Explorer (the window stays open behind Explorer).
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
    if reveal {
        crate::overlay::drop_to_normal();
    } else {
        crate::overlay::hide();
    }
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
/// video decode. Results are keyed by (path, time in tenths of a second). The newest request goes
/// first, and one not asked for again in the last frame or the one before (its card scrolled out
/// of view, or the page changed) is dropped instead of decoded.
pub struct Previews {
    queue: Arc<(Mutex<Queue>, std::sync::Condvar)>,
}

type PreviewKey = (PathBuf, u32);

/// The previews asked for, being made and made, shared with the thread making them.
#[derive(Default)]
struct Queue {
    want: HashMap<PreviewKey, Want>,
    /// The one being made, and whether it was forgotten meanwhile (it's then let go when done).
    busy: Option<(PreviewKey, bool)>,
    done: HashMap<PreviewKey, Preview>,
    /// The newest frame of the window that asked for any; how many requests have been made.
    frame: u64,
    asked: u64,
    /// Previews was dropped: the thread ends.
    closed: bool,
}

/// A preview asked for: its width, whether a keyframe will do, the window frame it was last asked
/// in and its place among the requests.
struct Want {
    width: usize,
    quick: bool,
    frame: u64,
    order: u64,
}

impl Queue {
    /// The next preview to make: the newest one asked for, after dropping those not asked for in
    /// the last frame or the one before. None when there's none.
    fn next(&mut self) -> Option<(PreviewKey, Want)> {
        let latest = self.frame;
        self.want.retain(|_, w| w.frame + 1 >= latest);
        let key = self.want.iter().max_by_key(|(_, w)| (w.frame, w.order)).map(|(k, _)| k.clone())?;
        let want = self.want.remove(&key)?;
        self.busy = Some((key.clone(), false));
        Some((key, want))
    }

    /// Keeps a preview just made, unless it was forgotten while it was being made.
    fn finish(&mut self, key: PreviewKey, preview: Preview) {
        if let Some((_, false)) = self.busy.take() {
            self.done.insert(key, preview);
        }
    }

    /// Forgets the previews `keep` says no to: made, asked for, or being made.
    fn forget(&mut self, keep: impl Fn(&PreviewKey) -> bool) {
        self.done.retain(|k, _| keep(k));
        self.want.retain(|k, _| keep(k));
        if let Some((k, forgotten)) = &mut self.busy
            && !keep(k)
        {
            *forgotten = true;
        }
    }
}

impl Previews {
    pub fn new(repaint: impl Fn() + Send + 'static) -> Self {
        let queue: Arc<(Mutex<Queue>, std::sync::Condvar)> = Arc::default();
        let shared = queue.clone();
        std::thread::spawn(move || {
            let (lock, wake) = &*shared;
            loop {
                let (key, want) = {
                    let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
                    loop {
                        if q.closed {
                            return;
                        }
                        if let Some(next) = q.next() {
                            break next;
                        }
                        q = wake.wait(q).unwrap_or_else(|e| e.into_inner());
                    }
                };
                let secs = media::duration(&key.0).ok();
                // Never past the end (a short clip asked for its 1 s frame).
                let at = (key.1 as f64 / 10.0).min(secs.unwrap_or(0.0) - 0.1).max(0.0);
                let frame = if want.quick { media::keyframe(&key.0, at, want.width) } else { media::frame(&key.0, at, want.width) }.ok();
                lock.lock().unwrap_or_else(|e| e.into_inner()).finish(key, Preview { frame, secs });
                repaint();
            }
        });
        Self { queue }
    }

    fn queue(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The preview of `path` at `at` seconds if it's ready; asks for it otherwise, as of the
    /// window's frame `frame` (egui's cumulative_frame_nr). `quick` takes the keyframe before `at`
    /// (media::keyframe), for scrubbing.
    pub fn get(&mut self, path: &Path, at: f64, width: usize, quick: bool, frame: u64) -> Option<Preview> {
        let key = (path.to_path_buf(), (at * 10.0).round().max(0.0) as u32);
        let mut q = self.queue();
        if let Some(p) = q.done.remove(&key) {
            return Some(p);
        }
        q.frame = q.frame.max(frame);
        if q.busy.as_ref().is_some_and(|(k, _)| *k == key) {
            return None;
        }
        q.asked += 1;
        let order = q.asked;
        match q.want.entry(key) {
            std::collections::hash_map::Entry::Occupied(mut w) => w.get_mut().frame = frame,
            std::collections::hash_map::Entry::Vacant(w) => {
                w.insert(Want { width, quick, frame, order });
                self.queue.1.notify_one();
            }
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
        self.queue().forget(|k| keep(k.1));
    }

    /// Forgets the previews of `path` (another file has its name now), one being made too; they're
    /// made again from that file when next asked for.
    pub fn forget_path(&mut self, path: &Path) {
        self.queue().forget(|k| k.0 != path);
    }
}

impl Drop for Previews {
    fn drop(&mut self) {
        self.queue().closed = true;
        self.queue.1.notify_all();
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

/// "Today", "Yesterday", or "Wed, Nov 4" (with the year if it isn't this year), in local time.
pub fn day_label(t: SystemTime) -> String {
    day_heading(local(t).map(|(y, m, d, .., wd)| (y, m, d, wd)), day(SystemTime::now()))
}

/// The heading over clips from local day `d` (year, month, day, weekday with 0 = Sunday) seen on
/// local day `today`: "Today", "Yesterday", or "Wed, Nov 4" (with the year if it isn't this
/// year); "Earlier" when the time couldn't be read. Counted in calendar days, so a clip from just
/// before midnight is Yesterday right after it, even on a day the clocks change.
pub fn day_heading(d: Option<(u16, u16, u16, u16)>, today: (u16, u16, u16)) -> String {
    let Some((y, m, dd, wd)) = d else { return "Earlier".into() };
    match day_number(today) - day_number((y, m, dd)) {
        0 => return "Today".into(),
        1 => return "Yesterday".into(),
        _ => {}
    }
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let label = format!("{}, {} {dd}", DAYS[wd as usize % 7], MONTHS[(m as usize + 11) % 12]);
    if y == today.0 { label } else { format!("{label}, {y}") }
}

/// Days since 1970-01-01 of a calendar date (Howard Hinnant's days_from_civil).
fn day_number((y, m, d): (u16, u16, u16)) -> i64 {
    let (y, m, d) = (y as i64 - (m <= 2) as i64, m as i64, d as i64);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
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
/// Always .mp4 except for a GIF: Windows writes the file in the container its extension names,
/// and it has no writer for .mkv or .mov, so a copy of one of those is made as an .mp4.
pub fn copy_name(src: &Path, tag: &str) -> PathBuf {
    let stem = src.file_stem().unwrap_or_default().to_string_lossy();
    let gif = src.extension().is_some_and(|e| e.eq_ignore_ascii_case("gif"));
    let ext = if gif { "gif" } else { "mp4" };
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
        assert!(p.get(&path, 1.0, 160, false, 1).is_none());
        assert!(p.get(&path, 5.0, 160, true, 1).is_none());
        while p.queue().done.len() < 2 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        p.retain(|t| t == 10);
        assert_eq!(p.queue().done.len(), 1);
        assert!(p.get(&path, 1.0, 160, false, 1).is_some());
        assert!(p.get(&path, 5.0, 160, true, 1).is_none(), "asked for again, not answered from memory");
    }

    /// The newest request is made first; one not asked for again in the last two frames is
    /// dropped; one forgotten while it's being made is let go when done.
    #[test]
    fn previews_go_newest_first_and_drop_stale_ones() {
        let key = |n: &str| (PathBuf::from(n), 10);
        let want = |frame, order| Want { width: 160, quick: false, frame, order };
        let mut q = Queue::default();
        q.want.insert(key("old.mp4"), want(1, 1));
        q.want.insert(key("a.mp4"), want(3, 2));
        q.want.insert(key("b.mp4"), want(3, 3));
        q.want.insert(key("c.mp4"), want(2, 4));
        q.frame = 3;
        let first = q.next().map(|(k, _)| k);
        assert_eq!(first, Some(key("b.mp4")), "asked last, in the newest frame");
        assert!(!q.want.contains_key(&key("old.mp4")), "not asked for in the last two frames: dropped");
        q.finish(key("b.mp4"), Preview { frame: None, secs: Some(1.0) });
        assert!(q.done.contains_key(&key("b.mp4")));
        assert_eq!(q.next().map(|(k, _)| k), Some(key("a.mp4")));
        q.forget(|k| k.0 != Path::new("a.mp4"));
        q.finish(key("a.mp4"), Preview { frame: None, secs: None });
        assert!(!q.done.contains_key(&key("a.mp4")), "forgotten while being made");
        assert_eq!(q.next().map(|(k, _)| k), Some(key("c.mp4")));
        assert!(q.next().is_none());
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
        // Windows can't write .mkv or .mov, so their copies are .mp4.
        assert_eq!(copy_name(&dir.join("Replay 1.mkv"), "trim").file_name().unwrap(), "Replay 1 (trim 2).mp4");
        assert_eq!(copy_name(&dir.join("Old.MOV"), "720p").file_name().unwrap(), "Old (720p).mp4");
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

    /// The daily backup is taken only from an old file that reads back as what's being saved: valid
    /// JSON of another shape (`[]` for tags, a setting of the wrong type) leaves the backup alone,
    /// however old it is. Temp files only.
    #[test]
    fn backup_only_from_a_file_of_the_same_kind() {
        let dir = std::env::temp_dir().join(format!("instant-replay-bak-kind-{}", crate::config::random_hex()));
        std::fs::create_dir_all(&dir).unwrap();
        let two_days_ago = SystemTime::now() - std::time::Duration::from_secs(2 * 86_400);
        let old_bak = |file: &Path, text: &str| {
            let bak = file.with_extension("json.bak");
            std::fs::write(&bak, text).unwrap();
            std::fs::File::options().write(true).open(&bak).unwrap().set_modified(two_days_ago).unwrap();
            bak
        };
        let tags_file = dir.join("tags.json");
        let tags = Tags::from([(r"D:\Clips\a.mp4".to_string(), ["Aces".to_string()].into())]);
        std::fs::write(&tags_file, "[]").unwrap();
        let bak = old_bak(&tags_file, r#"{"D:\\Clips\\b.mp4": ["Funny"]}"#);
        write_json(&tags_file, &tags).unwrap();
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), r#"{"D:\\Clips\\b.mp4": ["Funny"]}"#);
        let config_file = dir.join("config.json");
        std::fs::write(&config_file, r#"{"buffer_secs": "x"}"#).unwrap();
        let bak = old_bak(&config_file, "{}");
        write_json(&config_file, &crate::config::Config::default()).unwrap();
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "{}");
        // A good old file still becomes the backup.
        let before = std::fs::read_to_string(&tags_file).unwrap();
        old_bak(&tags_file, "{}");
        write_json(&tags_file, &Tags::new()).unwrap();
        assert_eq!(std::fs::read_to_string(tags_file.with_extension("json.bak")).unwrap(), before);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// At start: nothing recorded yet records the folder; the same folder (any case or slashes)
    /// does nothing; another one re-keys from it; no clips folder set does nothing.
    #[test]
    fn refile_at_start_decides() {
        assert_eq!(refile_at_start(None, r"D:\Clips"), Refile::Record);
        assert_eq!(refile_at_start(Some("d:/clips/"), r"D:\Clips"), Refile::Nothing);
        assert_eq!(refile_at_start(Some(r"E:\Old"), r"D:\Clips"), Refile::From(r"E:\Old".into()));
        assert_eq!(refile_at_start(Some(r"E:\Old"), " "), Refile::Nothing);
        assert_eq!(refile_at_start(None, ""), Refile::Nothing);
    }

    /// The recorded folder saves and reads back (in a temp file); none, or an empty one, is None.
    #[test]
    fn filed_folder_round_trips() {
        let dir = std::env::temp_dir().join(format!("instant-replay-filed-{}", crate::config::random_hex()));
        let file = dir.join("filed.json");
        assert_eq!(read_filed(&file), None);
        write_json(&file, &r"D:\Clips".to_string()).unwrap();
        assert_eq!(read_filed(&file).as_deref(), Some(r"D:\Clips"));
        write_json(&file, &String::new()).unwrap();
        assert_eq!(read_filed(&file), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A re-key cut short (a crash before the new folder was recorded) is done again at the next
    /// start from the same old folder: keys already moved stay put, the rest move, the lost stay lost.
    #[test]
    fn refolder_plan_again_after_a_crash() {
        let keys: Vec<String> = [r"E:\Old\a.mp4", r"E:\Old\VALORANT\b.mp4", r"E:\Old\lost.mp4"].map(String::from).into();
        let there = |p: &Path| [r"D:\Clips\a.mp4", r"D:\Clips\VALORANT\b.mp4"].iter().any(|t| Path::new(t) == p);
        let (moves, lost) = refolder_plan(&keys, r"E:\Old", r"D:\Clips", there);
        assert_eq!(moves.len(), 2);
        assert_eq!(lost, [r"E:\Old\lost.mp4"]);
        // Only the first move was saved before the crash.
        let half: Vec<String> = keys.iter().map(|k| if *k == moves[0].0 { moves[0].1.clone() } else { k.clone() }).collect();
        let (again, lost_again) = refolder_plan(&half, r"E:\Old", r"D:\Clips", there);
        assert_eq!(again, moves[1..]);
        assert_eq!(lost_again, lost);
        // All saved: nothing more to do.
        let done: Vec<String> = keys.iter().map(|k| moves.iter().find(|(o, _)| o == k).map_or(k.clone(), |(_, n)| n.clone())).collect();
        let (none, lost_still) = refolder_plan(&done, r"E:\Old", r"D:\Clips", there);
        assert!(none.is_empty());
        assert_eq!(lost_still, lost);
    }

    /// `list_read` names the clips folder and its game folders as read; a folder that isn't there
    /// reads as nothing, with no folder read.
    #[test]
    fn list_read_says_which_folders_read() {
        let dir = std::env::temp_dir().join(format!("instant-replay-list-read-{}", crate::config::random_hex()));
        std::fs::create_dir_all(dir.join("VALORANT")).unwrap();
        std::fs::write(dir.join("VALORANT").join("Ace.mp4"), b"x").unwrap();
        let (clips, read) = list_read(&dir.display().to_string(), 10);
        assert_eq!(clips.iter().map(|c| c.group.as_deref()).collect::<Vec<_>>(), [Some("VALORANT")]);
        assert_eq!(read, [dir.clone(), dir.join("VALORANT")]);
        std::fs::remove_dir_all(&dir).unwrap();
        let (clips, read) = list_read(&dir.display().to_string(), 10);
        assert!(clips.is_empty() && read.is_empty());
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

    /// A tagged clip renamed and moved into a game folder outside the app is found again by its
    /// fingerprint, not by a clip the same size that only starts the same; an entry from before
    /// fingerprints is left alone, and nothing outside the clips folder is looked for.
    #[test]
    fn refinds_moved_clips_by_fingerprint() {
        let dir = std::env::temp_dir().join("instant-replay-refind");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("VALORANT")).unwrap();
        let body: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let mut other = body.clone();
        *other.last_mut().unwrap() ^= 1;
        let (a, old, elsewhere) = (dir.join("a.mp4"), dir.join("old.mp4"), Path::new(r"Q:\gone\x.mp4"));
        std::fs::write(&a, &body).unwrap();
        let key = |p: &Path| p.display().to_string();
        let marked: std::collections::BTreeSet<String> = [key(&a), key(&old), key(elsewhere)].into();
        let mut prints = Prints::new();
        let scan = |prints: &mut Prints| {
            let clips = list(&dir.to_string_lossy(), usize::MAX);
            refind(&marked, prints, &clips.iter().collect::<Vec<_>>(), &[dir.as_path()], &Default::default())
        };
        // Listed and marked: gets a fingerprint. The old entry has none, so it stays as it is.
        assert_eq!(scan(&mut prints), (vec![], true));
        assert_eq!(prints.keys().collect::<Vec<_>>(), [&key(&a)]);
        assert_eq!(scan(&mut prints), (vec![], false), "nothing new");
        // Moved and renamed outside the app, next to a clip the same size that differs at the end.
        let moved_to = dir.join(r"VALORANT\Ace on Ascent.mp4");
        std::fs::rename(&a, &moved_to).unwrap();
        std::fs::write(dir.join("decoy.mp4"), &other).unwrap();
        prints.insert(key(elsewhere), prints[&key(&a)].clone());
        assert_eq!(scan(&mut prints), (vec![(a.clone(), moved_to.clone())], false));
        assert!(prints.contains_key(&key(&a)) && !prints.contains_key(&key(&moved_to)), "left for moved to carry");
        assert!(prints.contains_key(&key(elsewhere)), "outside the clips folder: not looked for");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A clip renamed to the name of one deleted outside the app keeps its own star, tags, note and
    /// fingerprint, not the gone clip's leftovers. Uses the tests' own copies of the files.
    #[test]
    fn moved_drops_leftovers_at_the_new_name() {
        let dir = std::env::temp_dir().join("instant-replay-moved-leftovers");
        let (old, new, gone) = (dir.join("a.mp4"), dir.join("b.mp4"), dir.join("c.mp4"));
        let key = |p: &Path| p.display().to_string();
        let mut favs: std::collections::BTreeSet<String> = [key(&new)].into();
        let mut t = Tags::new();
        set_tag(&mut t, &old, "Aces", true);
        set_tag(&mut t, &new, "Funny", true);
        let mut n = Notes::new();
        set_note(&mut n, &new, "someone else's clip");
        set_note(&mut n, &gone, "stays");
        edit_store(&prints_file(), read_prints, |p| {
            p.insert(key(&old), "1:aa".into());
            p.insert(key(&new), "2:bb".into());
            true
        });
        moved(&mut favs, &mut t, &mut n, &old, Some(&new), false);
        assert!(favs.is_empty(), "the old clip had no star; the leftover one goes");
        assert_eq!(t.get(&key(&new)).map(|s| s.iter().cloned().collect::<Vec<_>>()), Some(vec!["Aces".to_string()]));
        assert!(!t.contains_key(&key(&old)));
        assert_eq!(n.keys().collect::<Vec<_>>(), [&key(&gone)], "the leftover note goes, others stay");
        let p = prints();
        assert_eq!(p.get(&key(&new)).map(String::as_str), Some("1:aa"));
        assert!(!p.contains_key(&key(&old)));
        // Renamed to its own name: nothing is dropped.
        moved(&mut favs, &mut t, &mut n, &new, Some(&new), false);
        assert!(t.contains_key(&key(&new)) && prints().contains_key(&key(&new)));
        edit_store(&prints_file(), read_prints, |p| {
            p.retain(|k, _| !k.starts_with(&key(&dir)));
            true
        });
    }

    /// The scan thread's look for marked clips: fingerprints the marked clip into the fingerprints
    /// file (the tests' own), keeps a fingerprint saved there meanwhile, and finds the clip again
    /// once it's renamed. In a temp folder.
    #[test]
    fn refind_saved_keeps_its_prints_and_finds_the_clip() {
        let dir = std::env::temp_dir().join("instant-replay-refind-saved");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (a, to) = (dir.join("a.mp4"), dir.join("Ace.mp4"));
        std::fs::write(&a, (0..90_000u32).map(|i| (i % 249) as u8).collect::<Vec<u8>>()).unwrap();
        let key = |p: &Path| p.display().to_string();
        let marked: std::collections::BTreeSet<String> = [key(&a)].into();
        let scan = || {
            let clips = list(&dir.to_string_lossy(), usize::MAX);
            refind_saved(&marked, &clips.iter().collect::<Vec<_>>(), &[dir.as_path()], &Default::default())
        };
        assert!(scan().is_empty());
        assert!(prints().contains_key(&key(&a)), "saved");
        edit_store(&prints_file(), read_prints, |p| p.insert(key(&dir.join("other.mp4")), "1:aa".into()).is_none());
        std::fs::rename(&a, &to).unwrap();
        assert_eq!(scan(), vec![(a.clone(), to.clone())]);
        assert!(prints().contains_key(&key(&dir.join("other.mp4"))), "an edit made meanwhile stays");
        edit_store(&prints_file(), read_prints, |p| {
            p.retain(|k, _| !k.starts_with(&key(&dir)));
            true
        });
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The same move reported twice (the archive thread, then the window) keeps what the first
    /// carried; a rename in case only keeps the clip's own entries. In memory only.
    #[test]
    fn a_move_reported_twice_keeps_what_it_carried() {
        let (a, b) = (Path::new(r"C:\clips\a.mp4"), Path::new(r"C:\clips\b.mp4"));
        let key = |p: &Path| p.display().to_string();
        let mut favs: std::collections::BTreeSet<String> = [key(a)].into();
        let (mut t, mut n, mut p) = (Tags::new(), Notes::new(), Prints::new());
        set_tag(&mut t, a, "Aces", true);
        set_note(&mut n, a, "clutch");
        p.insert(key(a), "1:aa".into());
        assert_eq!(carry(&mut favs, &mut t, &mut n, &mut p, a, Some(b), false), [true; 4]);
        assert_eq!(carry(&mut favs, &mut t, &mut n, &mut p, a, Some(b), false), [false; 4], "nothing left under a: nothing to do");
        assert!(favs.contains(&key(b)) && t.contains_key(&key(b)) && n.contains_key(&key(b)) && p.contains_key(&key(b)));
        // Only the case changes: nothing under the new spelling is taken for a leftover.
        let upper = Path::new(r"C:\clips\B.mp4");
        assert_eq!(carry(&mut favs, &mut t, &mut n, &mut p, b, Some(upper), false), [true; 4]);
        assert_eq!(favs.iter().collect::<Vec<_>>(), [&key(upper)]);
        assert_eq!((t.len(), n.len(), p.len()), (1, 1, 1));
        // Recycled: everything goes; reported again, still nothing to do.
        assert_eq!(carry(&mut favs, &mut t, &mut n, &mut p, upper, None, false), [true; 4]);
        assert!(favs.is_empty() && t.is_empty() && n.is_empty() && p.is_empty());
        assert_eq!(carry(&mut favs, &mut t, &mut n, &mut p, upper, None, false), [false; 4]);
    }

    /// A clip with nothing filed under it, renamed by the user to the name of a marked clip deleted
    /// outside the app, doesn't take over that clip's star, tags, note or fingerprint. Reported by
    /// a background move instead, the leftovers stay (it may be a second report). In memory only.
    #[test]
    fn a_rename_drops_a_deleted_clips_marks() {
        let (a, b) = (Path::new(r"C:\clips\a.mp4"), Path::new(r"C:\clips\b.mp4"));
        let key = |p: &Path| p.display().to_string();
        let mut favs: std::collections::BTreeSet<String> = [key(b)].into();
        let (mut t, mut n, mut p) = (Tags::new(), Notes::new(), Prints::new());
        set_tag(&mut t, b, "Aces", true);
        set_note(&mut n, b, "someone else's clip");
        p.insert(key(b), "2:bb".into());
        assert_eq!(carry(&mut favs, &mut t, &mut n, &mut p, a, Some(b), false), [false; 4]);
        assert_eq!((favs.len(), t.len(), n.len(), p.len()), (1, 1, 1, 1));
        assert_eq!(carry(&mut favs, &mut t, &mut n, &mut p, a, Some(b), true), [true; 4]);
        assert!(favs.is_empty() && t.is_empty() && n.is_empty() && p.is_empty());
        // Renamed in case only: its own entries stay.
        let mut favs: std::collections::BTreeSet<String> = [key(b)].into();
        let upper = Path::new(r"C:\clips\B.mp4");
        assert_eq!(carry(&mut favs, &mut t, &mut n, &mut p, b, Some(upper), true), [true, false, false, false]);
        assert_eq!(favs.iter().collect::<Vec<_>>(), [&key(upper)]);
    }

    /// Edits from several threads at once all land: each reads, changes and saves under one lock.
    /// In a temp file.
    #[test]
    fn store_edits_from_threads_all_land() {
        let dir = std::env::temp_dir().join("instant-replay-edit-store");
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("store.json");
        let read = |f: &Path| load_json::<std::collections::BTreeMap<String, u32>>(f);
        let threads: Vec<_> = (0..8u32)
            .map(|i| {
                let file = file.clone();
                std::thread::spawn(move || {
                    for j in 0..5u32 {
                        edit_store(&file, read, |m| m.insert(format!("{i}-{j}"), i * j).is_none());
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(read(&file).len(), 40);
        // Nothing changed: not saved again.
        let before = std::fs::metadata(&file).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        edit_store(&file, read, |_| false);
        assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), before);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Notes save and load (in a temp file, not the real notes.json), stay one line and short, and
    /// a noted clip renamed outside the app is found again by its fingerprint and keeps its note.
    #[test]
    fn notes_store_and_follow_a_refind() {
        let dir = std::env::temp_dir().join("instant-replay-notes");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.json");
        let (a, b) = (dir.join("a.mp4"), dir.join("b.mp4"));
        let mut n = Notes::new();
        set_note(&mut n, &a, "  Clutch 1v4\non B site  ");
        set_note(&mut n, &b, &"x".repeat(300));
        assert_eq!(n[&a.display().to_string()], "Clutch 1v4 on B site");
        assert_eq!(n[&b.display().to_string()].chars().count(), NOTE_MAX);
        set_note(&mut n, &b, "   ");
        assert!(!n.contains_key(&b.display().to_string()), "an empty note takes it off");
        assert_eq!(read_notes(&file), Notes::new(), "no file yet: no notes (as on an older install)");
        write_notes(&file, &n);
        assert_eq!(read_notes(&file), n);
        // Renamed outside the app: found by its fingerprint, and the note goes with it.
        std::fs::write(&a, (0..100_000u32).map(|i| (i % 251) as u8).collect::<Vec<u8>>()).unwrap();
        let marked: std::collections::BTreeSet<String> = n.keys().cloned().collect();
        let mut prints = Prints::new();
        let scan = |prints: &mut Prints| {
            let clips = list(&dir.to_string_lossy(), usize::MAX);
            refind(&marked, prints, &clips.iter().collect::<Vec<_>>(), &[dir.as_path()], &Default::default())
        };
        scan(&mut prints);
        let to = dir.join("Ace on Ascent.mp4");
        std::fs::rename(&a, &to).unwrap();
        let (moves, _) = scan(&mut prints);
        assert_eq!(moves, vec![(a.clone(), to.clone())]);
        for (old, new) in &moves {
            assert!(move_note(&mut n, old, Some(new)));
        }
        assert_eq!(n[&to.display().to_string()], "Clutch 1v4 on B site");
        assert!(move_note(&mut n, &to, None) && n.is_empty(), "recycled: the note goes");
        std::fs::remove_dir_all(&dir).unwrap();
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

    /// Auto-cleanup only recycles what the Recycle Bin surely takes: a fixed drive Windows names
    /// the volume of and answers about, whose bin is on and big enough. Anything else, known or
    /// not, Windows might delete for good, so it's left alone.
    #[test]
    fn cleanup_only_recycles_what_the_bin_takes() {
        let gb = 1u64 << 30;
        let ok = Bin { drive: Drive::Fixed, volume: true, answers: true, policy_off: false, nuke: false, max: Some(10 * gb), share: Some(50 * gb) };
        assert_eq!(bin_refuses(ok, 2 * gb), None);
        assert_eq!(bin_refuses(Bin { max: Some(2 * gb), ..ok }, 2 * gb), None, "just fits");
        let too_big = bin_refuses(Bin { max: Some(gb), ..ok }, 2 * gb).unwrap();
        assert!(too_big.contains("bigger (2.0 GB)") && too_big.contains("holds (1.0 GB)"), "{too_big}");
        // No size set: 5% of the drive, only on a volume Windows named.
        assert_eq!(bin_refuses(Bin { max: None, ..ok }, 2 * gb), None);
        assert!(bin_refuses(Bin { max: None, share: Some(gb), ..ok }, 2 * gb).unwrap().contains("holds (1.0 GB)"));
        assert!(bin_refuses(Bin { max: None, share: None, ..ok }, 1).unwrap().contains("couldn't be read"));
        assert!(bin_refuses(Bin { max: Some(0), share: None, ..ok }, 1).is_some());
        assert!(bin_refuses(Bin { nuke: true, ..ok }, 1).unwrap().contains("set to delete files right away"));
        assert!(bin_refuses(Bin { policy_off: true, ..ok }, 1).unwrap().contains("turned off"));
        // A SUBST drive: Windows says it's fixed but names no volume, and the bin's size falls
        // back to 5% of the drive. Windows may delete for good there, so it's refused.
        let subst = Bin { volume: false, max: None, ..ok };
        assert!(bin_refuses(subst, 1).unwrap().contains("SUBST"));
        assert!(bin_refuses(Bin { answers: false, ..subst }, 1).is_some());
        assert!(bin_refuses(Bin { answers: false, ..ok }, 1).unwrap().contains("didn't answer"));
        assert!(bin_refuses(Bin { drive: Drive::Network, ..ok }, 1).unwrap().contains("network drive"));
        assert!(bin_refuses(Bin { drive: Drive::Removable, ..ok }, 1).unwrap().contains("removable drive"));
        assert!(bin_refuses(Bin { drive: Drive::Other, ..ok }, 1).is_some());
        // The temp folder is on a fixed drive whose volume Windows names, with a bin of some size.
        let bin = bin_on(&std::env::temp_dir());
        assert_eq!(bin.drive, Drive::Fixed);
        assert!(bin.volume && bin.answers, "{bin:?}");
        assert!(bin.max.or(bin.share).is_some_and(|b| b > 0));
    }

    /// Auto-cleanup judges the bin where a clip really is, and only when that's on the drive its
    /// path shows: a link to a drive that's off, a link to a NAS, a junction to another drive or a
    /// SUBST drive (whatever Windows calls its root) is refused. Nothing is looked at on disk.
    #[test]
    fn cleanup_judges_the_bin_where_the_clip_really_is() {
        let clip = r"C:\Clips\a.mp4";
        assert_eq!(follow_refuses(clip, Some(clip), Some(r"C:\"), Some(r"C:\")), None);
        assert_eq!(follow_refuses(clip, Some(r"C:\Elsewhere\a.mp4"), Some(r"c:\"), Some(r"C:\")), None, "a link on the same drive");
        assert!(follow_refuses(clip, None, Some(r"C:\"), None).unwrap().contains("couldn't tell where"));
        assert!(follow_refuses(clip, Some(r"\\nas\share\a.mp4"), Some(r"C:\"), None).unwrap().contains("network folder"));
        assert!(follow_refuses(clip, Some(r"\\?\UNC\nas\share\a.mp4"), Some(r"C:\"), Some(r"\\nas\share\")).unwrap().contains("network folder"));
        assert!(follow_refuses(clip, Some(r"D:\Clips\a.mp4"), Some(r"C:\"), Some(r"D:\")).unwrap().contains("another drive"), "a junction to another drive");
        assert!(follow_refuses(clip, Some(clip), None, Some(r"C:\")).unwrap().contains("which drive"));
        // Not a full path on a drive letter: a volume path, a relative one, one without its drive.
        for odd in [r"\\?\Volume{12345678-0000-0000-0000-000000000000}\Clips\a.mp4", r"Clips\a.mp4", r"\Clips\a.mp4", r"C:Clips\a.mp4", r"1:\a.mp4"] {
            assert!(follow_refuses(clip, Some(odd), Some(r"C:\"), Some(r"C:\")).unwrap().contains("full drive path"), "{odd}");
        }
        assert_eq!(follow_refuses(clip, Some(r"\\?\C:\Clips\a.mp4"), Some(r"C:\"), Some(r"C:\")), None, "the long form of a drive path");
        // A SUBST drive S: for C:\Sub: Windows gives the root as S:\ (another drive) or as C:\ (not
        // the root of S:\a.mp4); refused either way.
        assert!(follow_refuses(r"S:\a.mp4", Some(r"C:\Sub\a.mp4"), Some(r"S:\"), Some(r"C:\")).is_some());
        assert!(follow_refuses(r"S:\a.mp4", Some(r"C:\Sub\a.mp4"), Some(r"C:\"), Some(r"C:\")).is_some());
        // A drive mounted in a folder: its root is that folder, on both sides.
        assert_eq!(follow_refuses(r"C:\Mnt\Clips\a.mp4", Some(r"C:\Mnt\Clips\a.mp4"), Some(r"C:\Mnt\"), Some(r"C:\Mnt\")), None);
        // The temp folder's root is its drive's top folder.
        let temp = std::env::temp_dir();
        assert!(volume_root(&temp).is_some_and(|r| relative(&temp.display().to_string(), &r).is_some()));
    }

    /// Needs a SUBST drive set up by hand (`subst S: C:\SomeFolder`), with RV_SUBST_DIR set to a
    /// folder on it (`S:\`): auto-cleanup leaves a file there alone. Skips when it isn't set.
    #[test]
    #[ignore]
    fn cleanup_leaves_a_file_on_a_subst_drive() {
        let Ok(dir) = std::env::var("RV_SUBST_DIR") else { return };
        let f = Path::new(&dir).join("instant-replay-subst-test.txt");
        std::fs::write(&f, b"test").unwrap();
        let refused = recycle_unasked(&f);
        let there = f.exists();
        let _ = std::fs::remove_file(&f);
        assert!(refused.is_err() && there, "{refused:?}");
    }

    /// Needs a junction set up by hand to a folder on another drive (`mklink /J C:\Clips D:\Clips`),
    /// with RV_JUNCTION_DIR set to it (`C:\Clips`): auto-cleanup leaves a file in it alone. Skips
    /// when it isn't set.
    #[test]
    #[ignore]
    fn cleanup_leaves_a_file_through_a_junction_to_another_drive() {
        let Ok(dir) = std::env::var("RV_JUNCTION_DIR") else { return };
        let f = Path::new(&dir).join("instant-replay-junction-test.txt");
        std::fs::write(&f, b"test").unwrap();
        let refused = recycle_unasked(&f);
        let there = f.exists();
        let _ = std::fs::remove_file(&f);
        assert!(refused.is_err() && there, "{refused:?}");
    }

    /// A clip starred while a cleanup pass runs is spared; one that can't be moved is left alone
    /// from then on.
    #[test]
    fn cleanup_spares_a_clip_starred_during_the_pass() {
        let (a, b, c) = (PathBuf::from(r"D:\Clips\a.mp4"), PathBuf::from(r"D:\Clips\b.mp4"), PathBuf::from(r"D:\Clips\c.mp4"));
        let favs = std::cell::RefCell::new(std::collections::BTreeSet::new());
        let recycled = std::cell::RefCell::new(Vec::new());
        let mut skipped = std::collections::HashSet::new();
        let recycle = |p: &Path| {
            anyhow::ensure!(p != c, "open somewhere");
            recycled.borrow_mut().push(p.to_path_buf());
            // b is starred (written another way) while a is being recycled.
            favs.borrow_mut().insert(r"d:/clips/B.mp4".to_string());
            Ok(())
        };
        let mut done = Vec::new();
        cleanup_pass(vec![a.clone(), b.clone(), c.clone()], &mut skipped, || favs.borrow().clone(), recycle, |p| done.push(p.to_path_buf()));
        assert_eq!(*recycled.borrow(), vec![a.clone()]);
        assert_eq!(done, vec![a.clone()]);
        assert!(skipped.contains(&c) && !skipped.contains(&b));
        // The next pass doesn't try c again.
        cleanup_pass(vec![c.clone()], &mut skipped, || favs.borrow().clone(), |_| unreachable!(), |_| {});
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
        let new = move_into(&clip, &to.join("VALORANT"), "").unwrap();
        assert_eq!(new, to.join(r"VALORANT\x (2).mp4"));
        assert_eq!(std::fs::read(&new).unwrap(), b"clip");
        assert_eq!(std::fs::read(to.join(r"VALORANT\x.mp4")).unwrap(), b"older");
        assert!(!clip.exists() && !to.join(r"VALORANT\x (2).mp4.part").exists());
        // The Archive view lists both, in their game folder; a folder that isn't there is offline.
        let shown = archived(&to).unwrap();
        assert!(shown.len() == 2 && shown.iter().all(|c| c.group.as_deref() == Some("VALORANT")));
        assert!(archived(&to.join("not plugged in")).is_none());
        for d in [&from, &to] {
            std::fs::remove_dir_all(d).unwrap();
        }
    }

    /// A new clip gets the game in front of its name, also when it stays in the same folder.
    #[test]
    fn saved_clip_gets_game_name() {
        let dir = std::env::temp_dir().join("instant-replay-game-name");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("2026-11-07 21-40-00.mp4");
        std::fs::write(&clip, b"clip").unwrap();
        let new = move_into(&clip, &dir, "VALORANT").unwrap();
        assert_eq!(new, dir.join("VALORANT 2026-11-07 21-40-00.mp4"));
        assert!(!clip.exists());
        let sorted = dir.join("2026-11-07 21-41-00.mp4");
        std::fs::write(&sorted, b"clip").unwrap();
        assert_eq!(move_into(&sorted, &dir.join("VALORANT"), "VALORANT").unwrap(), dir.join(r"VALORANT\VALORANT 2026-11-07 21-41-00.mp4"));
        std::fs::remove_dir_all(&dir).unwrap();
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

    /// Day headings count calendar days: right after midnight a clip from just before it is
    /// Yesterday, across a month, a year and a leap day too; older days get their date (and the
    /// year when it isn't this one), and a time that couldn't be read is Earlier.
    #[test]
    fn day_headings() {
        let today = (2026, 11, 9);
        assert_eq!(day_heading(Some((2026, 11, 9, 1)), today), "Today");
        assert_eq!(day_heading(Some((2026, 11, 8, 0)), today), "Yesterday");
        assert_eq!(day_heading(Some((2026, 11, 7, 6)), today), "Sat, Nov 7");
        assert_eq!(day_heading(Some((2026, 10, 31, 6)), (2026, 11, 1)), "Yesterday");
        assert_eq!(day_heading(Some((2025, 12, 31, 3)), (2026, 1, 1)), "Yesterday");
        assert_eq!(day_heading(Some((2025, 12, 30, 2)), (2026, 1, 1)), "Tue, Dec 30, 2025");
        assert_eq!(day_heading(Some((2024, 2, 29, 4)), (2024, 3, 1)), "Yesterday");
        assert_eq!(day_heading(None, today), "Earlier");
        assert_eq!(day_number((1970, 1, 1)), 0);
        assert_eq!(day_number((2026, 11, 9)) - day_number((2025, 11, 9)), 365);
    }

    /// A save replaces the file whole and keeps the one before as a backup, which is read when
    /// the file is broken.
    #[test]
    fn tags_file_survives_a_bad_write() {
        let dir = std::env::temp_dir().join("instant-replay-tags-safe");
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("tags.json");
        let mut tags = Tags::new();
        set_tag(&mut tags, Path::new("a.mp4"), "Aces", true);
        write_tags(&file, &tags);
        set_tag(&mut tags, Path::new("b.mp4"), "Funny", true);
        write_tags(&file, &tags);
        assert_eq!(read_tags(&file), tags);
        assert!(!dir.join("tags.json.tmp").exists());
        std::fs::write(&file, b"{\"a.mp4\": [\"Ac").unwrap();
        assert_eq!(read_tags(&file).len(), 1, "the backup, from before the second save");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn game_folder_names() {
        assert_eq!(folder_name_from_title("VALORANT  "), "VALORANT");
        assert_eq!(folder_name_from_title("Elden Ring - v1.2"), "Elden Ring");
        assert_eq!(folder_name_from_title("Counter-Strike 2"), "Counter-Strike 2");
        assert_eq!(folder_name_from_title(""), "Desktop");
        assert_eq!(folder_name_from_title("Program Manager"), "Desktop");
        assert_eq!(folder_name_from_title("a?b*c"), "abc");
        // Never a folder outside the clips folder.
        assert_eq!(folder_name_from_title(".. . - Google Chrome"), "Desktop");
        assert_eq!(folder_name_from_title("."), "Desktop");
    }

    #[test]
    fn free_space_reads() {
        assert!(free_space(&std::env::temp_dir().display().to_string()).unwrap() > 0);
    }

    /// One form for every way a path can be typed; `fold` drops the case too.
    #[test]
    fn paths_tidy_and_fold() {
        assert_eq!(tidy("D:/Clips//VALORANT/"), r"D:\Clips\VALORANT");
        assert_eq!(tidy(r"\\?\D:\Clips\a.mp4"), r"D:\Clips\a.mp4");
        assert_eq!(tidy(r"\\?\UNC\nas\share\a.mp4"), r"\\nas\share\a.mp4");
        assert_eq!(tidy("//nas/share/"), r"\\nas\share");
        assert_eq!(tidy("D:/"), r"D:\");
        assert_eq!(tidy(r"D:\Clips\a.mp4"), r"D:\Clips\a.mp4");
        assert_eq!(fold(r"d:/CLIPS/a.MP4"), fold(r"D:\Clips\a.mp4"));
        assert_ne!(fold(r"D:\Clips\a.mp4"), fold(r"D:\Clips2\a.mp4"));
        let keys = [r"D:\Clips\b.mp4".to_string(), r"D:\Clips\A.mp4".to_string()];
        assert_eq!(stored_key(keys.iter(), Path::new("d:/clips/a.mp4")), Some(&keys[1]));
        assert_eq!(stored_key(keys.iter(), Path::new(r"D:\Clips\c.mp4")), None);
        assert_eq!(relative(r"d:\CLIPS\VALORANT\a.mp4", "D:/Clips/"), Some(PathBuf::from(r"VALORANT\a.mp4")));
        assert_eq!(relative(r"D:\Clips2\a.mp4", r"D:\Clips"), None);
    }

    /// Stores written by older versions, with the same clip under keys that differ in case or
    /// slashes, load as one entry per clip (tags put together); so does yesterday's copy when the
    /// file itself is broken. Temp files, not the real ones.
    #[test]
    fn stores_load_with_one_key_per_clip() {
        let dir = std::env::temp_dir().join("instant-replay-tidy-load");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (favs, tags, notes) = (dir.join("favorites.json"), dir.join("tags.json"), dir.join("notes.json"));
        std::fs::write(&favs, r#"["D:/Clips/a.mp4", "d:\\clips\\A.mp4", "\\\\?\\D:\\Clips\\b.mp4"]"#).unwrap();
        assert_eq!(read_favorites(&favs), [r"D:\Clips\a.mp4".to_string(), r"D:\Clips\b.mp4".to_string()].into());
        std::fs::write(&tags, r#"{"D:/Clips/a.mp4": ["Aces"], "d:\\clips\\A.mp4": ["Funny"]}"#).unwrap();
        let t = read_tags(&tags);
        assert_eq!(t.len(), 1);
        assert_eq!(t[r"D:\Clips\a.mp4"].iter().collect::<Vec<_>>(), ["Aces", "Funny"]);
        std::fs::write(&notes, r#"{"D:/Clips/a.mp4": "first", "d:\\clips\\A.mp4": "second"}"#).unwrap();
        assert_eq!(read_notes(&notes).into_iter().collect::<Vec<_>>(), [(r"D:\Clips\a.mp4".to_string(), "first".to_string())]);
        // Saved again, the tidy keys are what's written.
        write_tags(&tags, &t);
        assert!(std::fs::read_to_string(&tags).unwrap().contains(r#""D:\\Clips\\a.mp4""#));
        // The file broken: yesterday's copy (.json.bak) loads the same way.
        std::fs::write(dir.join("favorites.json.bak"), r#"["D:/Clips/c.mp4"]"#).unwrap();
        std::fs::write(&favs, b"[\"D:").unwrap();
        assert_eq!(read_favorites(&favs), [r"D:\Clips\c.mp4".to_string()].into());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A clips folder change carries keys to the same place in the new folder when the file is
    /// there, always when only the case or slashes changed, leaves ones still at the old place, and
    /// calls the rest lost. Keys outside the old folder are left alone.
    #[test]
    fn folder_change_carries_keys() {
        let keys: Vec<String> = [r"D:\Clips\a.mp4", r"D:\Clips\VALORANT\b.mp4", r"D:\Clips\c.mp4", r"F:\Other\x.mp4"].map(String::from).into();
        let on_disk: std::collections::HashSet<PathBuf> = [r"E:\New\a.mp4", r"D:\Clips\VALORANT\b.mp4", r"F:\Other\x.mp4"].map(PathBuf::from).into();
        let exists = |p: &Path| on_disk.contains(p);
        let (moves, lost) = refolder_plan(&keys, r"D:\Clips", "E:/New/", exists);
        assert_eq!(moves, [(keys[0].clone(), r"E:\New\a.mp4".to_string())]);
        assert_eq!(lost, [keys[2].clone()]);
        // Retyped with other case and slashes: every key follows, nothing is looked up.
        let (moves, lost) = refolder_plan(&keys, r"D:\Clips", "d:/clips/", |_| false);
        assert_eq!(moves.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>(), [r"d:\clips\a.mp4", r"d:\clips\VALORANT\b.mp4", r"d:\clips\c.mp4"]);
        assert!(lost.is_empty());
        assert_eq!(refolder_plan(&keys, r"D:\Clips", r"D:\Clips", |_| false), (vec![], vec![]), "same folder: nothing to do");
        // Carried into a map: what's already at the new key stays, the carried entry fills in.
        let mut tags: Tags = [(keys[0].clone(), ["Aces".to_string()].into()), (r"E:\New\a.mp4".to_string(), ["Funny".to_string()].into())].into();
        assert!(rekey_map(&mut tags, &[(keys[0].clone(), r"E:\New\a.mp4".to_string())], |a, b| a.extend(b)));
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[r"E:\New\a.mp4"].len(), 2);
        assert!(!rekey_map(&mut tags, &[(keys[2].clone(), "x".into())], |a, b| a.extend(b)), "nothing at that key");
        let lost: std::collections::BTreeSet<String> = [keys[2].clone(), keys[3].clone()].into();
        assert_eq!(lost_stars(&lost, &[keys[2].clone()].into()), 1);
        assert_eq!(lost_stars(&lost, &[keys[0].clone()].into()), 0);
    }

    /// A store that can't be saved under the new clips folder leaves the folder they're filed
    /// against as it was, so the next start re-keys again, and auto-cleanup holds off in the new
    /// folder until then; once every store saves, it's recorded and cleanup goes on.
    #[test]
    fn failed_store_save_keeps_the_old_filed_folder() {
        let dir = std::env::temp_dir().join(format!("instant-replay-refile-{}", crate::config::random_hex()));
        std::fs::create_dir_all(&dir).unwrap();
        let names = ["favorites.json", "tags.json", "notes.json", "fingerprints.json"].map(|n| dir.join(n));
        let files = names.each_ref().map(PathBuf::as_path);
        let filed = dir.join("filed.json");
        std::fs::write(files[0], r#"["D:\\Old\\a.mp4"]"#).unwrap();
        assert!(save_json(&filed, &r"D:\Old".to_string()));
        let moves = [(r"D:\Old\a.mp4".to_string(), r"E:\New\a.mp4".to_string())];
        // The stars can't be saved: a folder stands where their new file is written first.
        let block = files[0].with_extension("json.tmp");
        std::fs::create_dir_all(&block).unwrap();
        let mut favs = read_favorites(files[0]);
        let saved = rekey_in(files, &mut favs, &mut Tags::new(), &mut Notes::new(), &moves);
        assert!(!saved);
        assert!(!save_filed_in(&filed, r"E:\New", saved));
        assert_eq!(read_filed(&filed).as_deref(), Some(r"D:\Old"));
        assert!(unfiled(read_filed(&filed).as_deref(), r"E:\New"), "auto-cleanup holds off in the new folder");
        assert_eq!(read_favorites(files[0]), [r"D:\Old\a.mp4".to_string()].into(), "the stars on disk are as they were");
        // Saved the next time: the new folder is recorded.
        std::fs::remove_dir(&block).unwrap();
        let mut favs = read_favorites(files[0]);
        let saved = rekey_in(files, &mut favs, &mut Tags::new(), &mut Notes::new(), &moves);
        assert!(saved);
        assert!(save_filed_in(&filed, r"E:\New", saved));
        assert_eq!(read_filed(&filed).as_deref(), Some(r"E:\New"));
        assert!(!unfiled(read_filed(&filed).as_deref(), r"e:/new/"), "auto-cleanup goes on (case and slashes aside)");
        assert!(!unfiled(None, r"E:\New"), "none recorded yet");
        assert_eq!(read_favorites(files[0]), [r"E:\New\a.mp4".to_string()].into());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A clip OBS is still writing (its writers share every kind of access, so a plain open for
    /// writing works) isn't listed until it's closed; one only being read is.
    #[test]
    fn list_skips_a_clip_still_being_written() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = std::env::temp_dir().join(format!("instant-replay-list-writing-{}", crate::config::random_hex()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("a.mp4");
        let writer = std::fs::OpenOptions::new().write(true).create(true).truncate(true).share_mode(1 | 2 | 4).open(&clip).unwrap();
        let listed = || list(&dir.to_string_lossy(), usize::MAX).len();
        assert_eq!(listed(), 0);
        drop(writer);
        let reader = std::fs::File::open(&clip).unwrap();
        assert_eq!(listed(), 1);
        drop(reader);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Stars left under an earlier clips folder (by a version before they followed a change) are
    /// lost when they can be found by fingerprint, and only listed when they can't. Stars in the
    /// clips folder or the archive folder, however typed, aren't strays. Nothing is looked at on disk.
    #[test]
    fn stray_stars_from_an_earlier_folder() {
        let favs: std::collections::BTreeSet<String> = [r"D:\Clips\VALORANT\a.mp4", r"N:\Archive\b.mp4", r"Q:\Old clips\c.mp4", r"Q:\Old clips\d.mp4", r"\\nas\old\e.mp4"].map(String::from).into();
        let prints: Prints = [(r"q:\old clips\C.mp4".to_string(), "1:x".to_string()), (r"\\nas\old\e.mp4".to_string(), "2:y".to_string()), (r"D:\Clips\VALORANT\a.mp4".to_string(), "3:z".to_string())].into();
        let (lost, unknown) = stray_stars(&favs, &prints, "d:/clips/", r"N:\Archive");
        assert_eq!(lost, [r"Q:\Old clips\c.mp4", r"\\nas\old\e.mp4"].map(String::from).into());
        assert_eq!(unknown, [r"Q:\Old clips\d.mp4"]);
        // No archive folder: an archived star is a stray too.
        assert!(stray_stars(&favs, &prints, r"D:\Clips", "").1.contains(&r"N:\Archive\b.mp4".to_string()));
        assert_eq!(stray_stars(&favs, &prints, r"D:\Clips\VALO", r"N:\Archive").0.len(), 3, "a folder whose name starts the same isn't it");
    }

    /// A star filed under the clips folder typed another way still keeps its clip out of cleanup.
    #[test]
    fn cleanup_spares_a_star_written_another_way() {
        let now = SystemTime::now();
        let clip = |name: &str| Clip { path: PathBuf::from(name), name: name.into(), bytes: 1 << 30, modified: now - std::time::Duration::from_secs(86_400), group: None, is_picture: false };
        let clips = vec![clip(r"D:\Clips\fav.mp4"), clip(r"D:\Clips\other.mp4")];
        let favs = ["d:/CLIPS/FAV.mp4".to_string()].into();
        let plan = cleanup_plan(&clips, &favs, Cleanup { max_bytes: Some(1), max_age: None }, now);
        assert_eq!(plan, [PathBuf::from(r"D:\Clips\other.mp4")]);
    }

    /// Auto-cleanup and the archive stay out of a drive's top folder, the user's own folders and a
    /// folder holding one, however typed; a folder of its own inside one is fine.
    #[test]
    fn rules_stay_out_of_shared_folders() {
        let known: Vec<(&str, PathBuf)> = [("Videos", r"C:\Users\v\Videos"), ("Pictures", r"D:\Pics\Pictures"), ("user", r"C:\Users\v")].map(|(n, p)| (n, PathBuf::from(p))).into();
        let why = |f: &str| shared_folder(f, &known);
        assert_eq!(why(r"C:\Users\v\Videos").as_deref(), Some("your Videos folder"));
        assert_eq!(why("c:/users/V/videos/").as_deref(), Some("your Videos folder"));
        assert_eq!(why(r"\\?\C:\Users\v\Videos").as_deref(), Some("your Videos folder"));
        assert_eq!(why(r"C:\Users\v").as_deref(), Some("your user folder"));
        assert_eq!(why(r"D:\Pics").as_deref(), Some("the folder that holds your Pictures folder"));
        assert_eq!(why("D:/").as_deref(), Some("the top of drive D:"));
        assert_eq!(why(r"\\nas\share\").as_deref(), Some(r"the top of \\nas\share"));
        // Clips are listed one folder down, so files right in the user folder would be too.
        assert_eq!(why(r"C:\Users").as_deref(), Some("the folder that holds your user folder"));
        for fine in [r"C:\Users\v\Videos\Replay Vault Clips", r"E:\Clips", r"\\nas\share\clips", "", "  "] {
            assert_eq!(why(fine), None, "{fine}");
        }
        // Used anyway once confirmed for that folder (however typed); another folder isn't.
        let off = |f: &str, ok: &str| rules_off(f, f, ok, &known);
        assert_eq!(off("E:/", r"E:\"), None);
        assert_eq!(off(r"c:\users\V\videos\", r"C:\Users\v\Videos"), None);
        assert_eq!(off("F:/", r"E:\").as_deref(), Some("the top of drive F:"));
        assert_eq!(off(r"C:\Users\v\Videos", r"C:\Users\v").as_deref(), Some("your Videos folder"));
        assert_eq!(off(r"C:\Users\v\Videos", " ").as_deref(), Some("your Videos folder"));
        assert_eq!(off(r"E:\Clips", r"C:\Users\v\Videos"), None, "a fine folder stays fine");
        // This PC's own folders are found, and the default clips folder is fine.
        assert!(known_folders().iter().any(|(n, p)| *n == "Videos" && !p.as_os_str().is_empty()));
        assert_eq!(folder_risk(&crate::config::Config::default().folder, ""), None);
    }

    /// A clips folder that's really a shared one (a link, SUBST or mapped drive, short name or
    /// `..`) is judged by where it really is; a confirm holds for that same folder only.
    #[test]
    fn rules_follow_the_folder_to_where_it_is() {
        let known: Vec<(&str, PathBuf)> = [("Videos", r"C:\Users\v\Videos")].map(|(n, p)| (n, PathBuf::from(p))).into();
        let videos = r"C:\Users\v\Videos";
        // A junction typed as E:\Link whose final form is Videos.
        assert_eq!(rules_off(r"E:\Link", videos, "", &known).as_deref(), Some("your Videos folder"));
        assert_eq!(rules_off(r"E:\Link", r"\\?\C:\USERS\V\VIDEOS", "", &known).as_deref(), Some("your Videos folder"));
        // Confirmed as Videos written another way: holds. Confirmed for another folder: doesn't.
        assert_eq!(rules_off(r"E:\Link", videos, videos, &known), None);
        assert_eq!(rules_off(r"E:\Link", videos, r"E:\Other", &known).as_deref(), Some("your Videos folder"));
        // A typed folder that's shared stays refused, whatever it resolved to.
        assert_eq!(rules_off(videos, r"E:\Clips", "", &known).as_deref(), Some("your Videos folder"));
        assert_eq!(rules_off(r"E:\Clips", r"E:\Clips", "", &known), None);

        // Real folders: a temp folder stands in for Videos, reached with `..`.
        let base = std::env::temp_dir().join(format!("rv-final-{}", std::process::id()));
        let vids = base.join("Vids");
        std::fs::create_dir_all(vids.join("sub")).unwrap();
        let dotted = |p: &Path| format!(r"{}\sub\..", p.display());
        let vids_text = vids.display().to_string();
        assert_eq!(final_path(&dotted(&vids)).map(|p| fold(&p)), final_path(&vids_text).map(|p| fold(&p)));
        // A known folder written another way: `with_final` adds where it really is.
        let known = with_final(&[("Videos", PathBuf::from(dotted(&vids)))]);
        assert_eq!(known.len(), 2);
        assert_eq!(folder_risk_in(&vids_text, "", &known).as_deref(), Some("your Videos folder"));
        let known = with_final(&[("Videos", vids.clone())]);
        assert_eq!(folder_risk_in(&vids_text, "", &known).as_deref(), Some("your Videos folder"));
        assert_eq!(folder_risk_in(&dotted(&vids), "", &known).as_deref(), Some("your Videos folder"));
        assert_eq!(folder_risk_in(&format!(r"{}\.", vids.display()), "", &known).as_deref(), Some("your Videos folder"));
        assert_eq!(folder_risk_in(&base.display().to_string(), "", &known).as_deref(), Some("the folder that holds your Videos folder"));
        assert_eq!(folder_risk_in(&vids.join("sub").display().to_string(), "", &known), None, "a folder of its own inside it is fine");
        // Confirmed as Vids: holds when typed with `..`; a confirm for sub doesn't carry over.
        assert_eq!(folder_risk_in(&dotted(&vids), &vids_text, &known), None);
        assert!(folder_risk_in(&vids_text, &vids.join("sub").display().to_string(), &known).is_some());
        // A folder that isn't there is judged as typed.
        assert_eq!(final_path(&base.join("gone").display().to_string()), None);
        assert_eq!(folder_risk_in(&base.join("gone").display().to_string(), "", &known), None);
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// A marked clip listed with other case is found right away, without a fingerprint; a lost one
    /// from an old folder (outside the roots, never looked at on disk) is found by its fingerprint.
    #[test]
    fn refinds_case_changes_and_lost_clips() {
        let dir = std::env::temp_dir().join("instant-replay-refind-case");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.mp4"), dir.join("b.mp4"));
        std::fs::write(&a, b"aaaa").unwrap();
        std::fs::write(&b, (0..100_000u32).map(|i| (i % 251) as u8).collect::<Vec<u8>>()).unwrap();
        let upper = a.display().to_string().to_uppercase();
        let old = r"Q:\Old clips\renamed.mp4".to_string();
        let marked: std::collections::BTreeSet<String> = [upper.clone(), old.clone()].into();
        let mut prints: Prints = [(old.clone(), fingerprint(&b).unwrap())].into();
        let clips = list(&dir.to_string_lossy(), usize::MAX);
        let scanned: Vec<&Clip> = clips.iter().collect();
        let (moves, _) = refind(&marked, &mut prints, &scanned, &[dir.as_path()], &Default::default());
        assert_eq!(moves, [(PathBuf::from(&upper), a.clone())], "not lost: the old folder isn't looked in");
        let (moves, _) = refind(&marked, &mut prints, &scanned, &[dir.as_path()], &[old.clone()].into());
        assert_eq!(moves, [(PathBuf::from(&upper), a.clone()), (PathBuf::from(&old), b.clone())]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
