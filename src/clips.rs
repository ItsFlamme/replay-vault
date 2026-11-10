//! The clips folder: listing, opening, recycling, free space, and thumbnails made in the background.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
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
            let is_picture = is_picture(&path);
            if !(is_picture || matches!(ext.as_str(), "mp4" | "mkv" | "mov")) {
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
    clips.sort_by_key(|c| std::cmp::Reverse(c.modified));
    clips.truncate(max);
    clips
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
/// link stays, to be deleted. Saves whatever changed.
/// Anything already filed under the new path is dropped first: it was a clip deleted outside the
/// app, and the clip that has that name now isn't the same one.
pub fn moved(favs: &mut std::collections::BTreeSet<String>, tags: &mut Tags, notes: &mut Notes, old: &Path, new: Option<&Path>) {
    if new == Some(old) {
        return;
    }
    let to = new.map(|n| n.display().to_string());
    let stale = to.as_ref().is_some_and(|k| favs.remove(k));
    if favs.remove(&old.display().to_string()) {
        favs.extend(to.clone());
        save_favorites(favs);
    } else if stale {
        save_favorites(favs);
    }
    let stale = to.as_ref().is_some_and(|k| tags.remove(k).is_some());
    if move_tags(tags, old, new) || stale {
        save_tags(tags);
    }
    let stale = to.as_ref().is_some_and(|k| notes.remove(k).is_some());
    if move_note(notes, old, new) || stale {
        save_notes(notes);
    }
    // A print already at the new path is only replaced, never just dropped: the archive thread
    // and the window both report the same move, and the second must keep what the first carried.
    let mut p = prints();
    if let Some(print) = p.remove(&old.display().to_string()) {
        p.extend(to.map(|n| (n, print)));
        save_prints(&p);
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

/// Stars, tags, notes and fingerprints: read from the file, or from its backup when the file is
/// missing or can't be read.
pub(crate) fn load_json<T: serde::de::DeserializeOwned + Default>(file: &Path) -> T {
    let read = |f: &Path| std::fs::read_to_string(f).ok().and_then(|s| serde_json::from_str(&s).ok());
    read(file).or_else(|| read(&file.with_extension("json.bak"))).unwrap_or_default()
}

/// Written to a new file that then replaces the old one, so a crash or full disk mid-write never
/// leaves half a file. The old one is kept as `.json.bak` once a day, so a bad day can be undone.
pub(crate) fn save_json<T: serde::Serialize>(file: &Path, value: &T) {
    // The window, the archive and the cleanup threads can save at once: one at a time.
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock();
    let _ = std::fs::create_dir_all(file.parent().unwrap_or(Path::new(".")));
    let (tmp, bak) = (file.with_extension("json.tmp"), file.with_extension("json.bak"));
    let Ok(text) = serde_json::to_string_pretty(value) else { return };
    if std::fs::write(&tmp, text).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    let bak_age = std::fs::metadata(&bak).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
    // Only a file that reads back is worth keeping as the backup.
    let readable = std::fs::read_to_string(file).ok().is_some_and(|s| serde_json::from_str::<serde_json::Value>(&s).is_ok());
    if readable && bak_age.is_none_or(|a| a > std::time::Duration::from_secs(86_400)) && std::fs::copy(file, &bak).is_ok() {
        // copy keeps the old file's time; the backup's age is from now.
        let _ = std::fs::File::options().write(true).open(&bak).and_then(|f| f.set_modified(std::time::SystemTime::now()));
    }
    // Antivirus or a reader can hold the file for a moment.
    for tries in 0.. {
        match std::fs::rename(&tmp, file) {
            Ok(()) => break,
            Err(_) if tries < 5 => std::thread::sleep(std::time::Duration::from_millis(200)),
            Err(e) => {
                crate::log::write(&format!("Couldn't save {}: {e}", file.display()));
                let _ = std::fs::remove_file(&tmp);
                break;
            }
        }
    }
}

fn favorites_file() -> PathBuf {
    if cfg!(test) { std::env::temp_dir().join("instant-replay-favorites-test.json") } else { crate::config::dir().join("favorites.json") }
}

/// Starred clips, by path.
pub fn favorites() -> std::collections::BTreeSet<String> {
    load_json(&favorites_file())
}

pub fn save_favorites(favs: &std::collections::BTreeSet<String>) {
    save_json(&favorites_file(), favs);
}

/// Tags ("Aces", "Funny") by clip path.
pub type Tags = std::collections::BTreeMap<String, std::collections::BTreeSet<String>>;

fn tags_file() -> PathBuf {
    if cfg!(test) { std::env::temp_dir().join("instant-replay-tags-test.json") } else { crate::config::dir().join("tags.json") }
}

pub fn tags() -> Tags {
    read_tags(&tags_file())
}

pub fn save_tags(tags: &Tags) {
    write_tags(&tags_file(), tags);
}

fn read_tags(file: &Path) -> Tags {
    load_json(file)
}

fn write_tags(file: &Path, tags: &Tags) {
    save_json(file, tags);
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
    let Some(set) = tags.remove(&old.display().to_string()) else { return false };
    if let Some(new) = new {
        tags.insert(new.display().to_string(), set);
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

pub fn save_notes(notes: &Notes) {
    write_notes(&notes_file(), notes);
}

fn read_notes(file: &Path) -> Notes {
    load_json(file)
}

fn write_notes(file: &Path, notes: &Notes) {
    save_json(file, notes);
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
    let Some(note) = notes.remove(&old.display().to_string()) else { return false };
    if let Some(new) = new {
        notes.insert(new.display().to_string(), note);
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
    load_json(&prints_file())
}

pub fn save_prints(prints: &Prints) {
    save_json(&prints_file(), prints);
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
/// First fingerprints the marked clips in `scanned` that have none yet. Then each marked path with a
/// fingerprint that's gone from under `roots` is looked for in `scanned` (only clips of the same
/// size are read). Returns the moves (old, new), for `moved` to carry the star, tags, note,
/// fingerprint and share link, and whether `prints` changed (it gets the new fingerprints; the
/// found ones stay under their old path until `moved` carries them).
pub fn refind(marked: &std::collections::BTreeSet<String>, prints: &mut Prints, scanned: &[&Clip], roots: &[&Path]) -> (Vec<(PathBuf, PathBuf)>, bool) {
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
    let gone: Vec<(String, String)> = prints
        .iter()
        .filter(|(k, _)| marked.contains(*k) && !listed.contains(Path::new(k)) && roots.iter().any(|r| inside(k, &r.display().to_string())) && !Path::new(k).exists())
        .map(|(k, p)| (k.clone(), p.clone()))
        .collect();
    let mut moves: Vec<(PathBuf, PathBuf)> = Vec::new();
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
    *CLEANUP.lock().unwrap() = Some((folder.to_string(), rule));
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
                Ok(()) => {
                    // Its tags and note go too, as when it's recycled in the window.
                    moved(&mut favorites(), &mut tags(), &mut notes(), &path, None);
                    MOVED.lock().unwrap().push((path.clone(), None));
                    crate::log::write(&format!("Auto-cleanup moved {} to the Recycle Bin.", path.display()));
                }
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

/// The clips in the archive folder and its game folders, newest first; None if it isn't there
/// (the drive or NAS is off).
pub fn archived(dir: &Path) -> Option<Vec<Clip>> {
    dir.is_dir().then(|| list(&dir.to_string_lossy(), usize::MAX))
}

static ARCHIVE: Mutex<Option<(String, Archive)>> = Mutex::new(None);
/// Clips the archive moved (old, new) or auto-cleanup recycled (old, None), for the window to
/// carry or drop stars, tags and notes in its own copy. Reloading the files instead could lose a
/// star or tag set while the thread was writing.
static MOVED: Mutex<Vec<(PathBuf, Option<PathBuf>)>> = Mutex::new(Vec::new());

pub fn take_moved() -> Vec<(PathBuf, Option<PathBuf>)> {
    std::mem::take(&mut MOVED.lock().unwrap())
}
/// Set while the archive folder wasn't there the last time it was looked for.
pub static ARCHIVE_MISSING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The clips folder and archive rule to work with (from the saved settings); None is off.
pub fn set_archive(folder: &str, rule: Option<Archive>) {
    *ARCHIVE.lock().unwrap() = rule.map(|r| (folder.to_string(), r));
}

/// Every 5 minutes, moves what `archive_plan` picks into the archive's folder of the same name
/// (VALORANT\x.mp4 to <archive>\VALORANT\x.mp4), with its star, tags, note and share link. If the
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
            match move_into(&c.path, &dir, "") {
                Ok(new) => {
                    // Written here too, so it holds while the window is asleep in the tray. Read
                    // fresh: a copy to a NAS can take a while.
                    moved(&mut favorites(), &mut tags(), &mut notes(), &c.path, Some(&new));
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
            refind(&marked, prints, &clips.iter().collect::<Vec<_>>(), &[dir.as_path()])
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
    /// fingerprint, not the gone clip's leftovers. Uses the test copies of the files.
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
        let mut p = prints();
        p.insert(key(&old), "1:aa".into());
        p.insert(key(&new), "2:bb".into());
        save_prints(&p);
        moved(&mut favs, &mut t, &mut n, &old, Some(&new));
        assert!(favs.is_empty(), "the old clip had no star; the leftover one goes");
        assert_eq!(t.get(&key(&new)).map(|s| s.iter().cloned().collect::<Vec<_>>()), Some(vec!["Aces".to_string()]));
        assert!(!t.contains_key(&key(&old)));
        assert_eq!(n.keys().collect::<Vec<_>>(), [&key(&gone)], "the leftover note goes, others stay");
        let p = prints();
        assert_eq!(p.get(&key(&new)).map(String::as_str), Some("1:aa"));
        assert!(!p.contains_key(&key(&old)));
        // Renamed to its own name: nothing is dropped.
        moved(&mut favs, &mut t, &mut n, &new, Some(&new));
        assert!(t.contains_key(&key(&new)) && prints().contains_key(&key(&new)));
        let mut p = prints();
        p.retain(|k, _| !k.starts_with(&key(&dir)));
        save_prints(&p);
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
            refind(&marked, prints, &clips.iter().collect::<Vec<_>>(), &[dir.as_path()])
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
}
