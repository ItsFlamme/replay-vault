//! The start-up tidy-up: what the app's own failure paths can leave behind (killed, crashed, the
//! power cut or the disk full at the wrong moment) goes at the next start: a half-done OBS download
//! (up to about 775 MB), a save's, a copy's or an export's half-written file, and two kinds of
//! registry leftover.
//! Only a closed list of exact names in the app's own places (`rule`), each only with proof that the
//! app made it; anything else, or anything that can't be told (a drive that isn't there), stays.
//! Every rule is rooted in this copy's own folders and asks `instance`, so what another copy of the
//! app keeps is never in reach. Off in dev builds, which can share the settings folder.
//!
//! Three phases, on threads that already run at start: the install folder's (`install_root_phase`,
//! on the OBS update thread before its first check), the app update's (`app_update_phase`, only
//! where updates are allowed) and the settings and clips folders' (`settings_phase`). Each lists
//! what it finds (stats of exact names, and `read_dir` of the named folders only), makes a pure
//! `plan` of it and carries that out, skipping anything that fails; one log line per phase if
//! anything went. Runs at every start: the rules cost a few stats and do nothing a second time.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::{app_update, clips, covers, obs_install};

const HOUR: Duration = Duration::from_secs(3600);
const DAY: Duration = Duration::from_secs(86_400);
/// A save's `.json.tmp` is written and renamed within a second; one this old was cut short.
const TMP_AGE: Duration = Duration::from_secs(10 * 60);
/// Freed in one phase, the status line says so too.
const WORTH_SAYING: u64 = 100 << 20;

/// Where something was found.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
enum Place {
    /// The private OBS's install folder (`obs_install::install_root`), maybe one the user picked.
    #[default]
    InstallRoot,
    /// Where the VC++ installers' `vc-` folders go (`obs_install::base`).
    Base,
    /// Beside the exe, where a portable update parks the old one.
    ExeDir,
    /// The settings folder (`config::dir`).
    Settings,
    /// The covers folder (`covers::dir`).
    Covers,
    /// The clips folder or one of its folders (the two levels `clips::list` reads).
    Clips,
    /// The archive folder or one of its folders.
    Archive,
}

/// What a thing found is. A link, junction or anything else is never a rule's.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
enum Kind {
    #[default]
    Other,
    File,
    Dir,
}

/// One thing found where a rule looks, with what its rule needs to know. Each fact is false unless
/// the listing proved it, and false always keeps it.
#[derive(Clone, Debug, Default)]
struct Seen {
    path: PathBuf,
    place: Place,
    kind: Kind,
    bytes: u64,
    /// None when it couldn't be read: kept, as how old it is can't be told.
    modified: Option<SystemTime>,
    /// A centering file's clip is beside it, or an archive copy's source is in the clips folder.
    sibling: bool,
    /// Nothing has it open for writing.
    free: bool,
    /// A `vc-` folder holds nothing but the VC++ installer (or nothing).
    only_redist: bool,
    /// `obs-studio.update` surely has no version.txt, so it can never be put in.
    no_version: bool,
    /// A cover marker's game surely has no folder in the clips folder or the archive.
    orphan: bool,
}

/// How a planned removal is done.
#[derive(Clone, Copy, Debug, PartialEq)]
enum How {
    /// A file, deleted for good.
    Delete,
    /// One of the app's own exactly named folders in the install folder, with all in it.
    DeleteTree,
    /// A `vc-` folder: its installer, then the folder only if that left it empty.
    VcFolder,
    /// A folder, only if it's empty.
    IfEmpty,
    /// To the Recycle Bin, only where it surely has one that takes it (`clips::recycle_unasked`).
    Recycle,
}

#[derive(Debug, PartialEq)]
struct Removal {
    path: PathBuf,
    how: How,
    bytes: u64,
}

/// What the rules compare against besides names.
#[derive(Default)]
struct Ctx<'a> {
    /// This run's exe, whose `.old` and `.new` a portable update leaves.
    exe: Option<&'a Path>,
    /// `obs_install::base` when it isn't the install folder: removed once its last `vc-` folder goes.
    base: Option<&'a Path>,
}

/// The rule `s` falls under, if any: the whole list. Never anything else, and never the private
/// copy, a clip, a recording, a screenshot, a montage, an export (but one cut short, still under its
/// hidden name), a game folder, a `.bak`, tweaks.json or a log.
fn rule(s: &Seen, now: SystemTime, ctx: &Ctx) -> Option<How> {
    use How::*;
    use Kind::{Dir, File};
    let name = s.path.file_name()?.to_str()?;
    let older = |age: Duration| s.modified.is_some_and(|m| now.duration_since(m).is_ok_and(|a| a > age));
    match (s.place, s.kind) {
        // The installer an update ran at the last start.
        (Place::InstallRoot, File) if name == app_update::INSTALLER => Some(Delete),
        // A download or unpacking cut short. One under way is younger (and its zip is held open).
        (Place::InstallRoot, File) if name == obs_install::DOWNLOAD && older(HOUR) => Some(Delete),
        (Place::InstallRoot, Dir) if name == obs_install::PARTIAL && older(HOUR) => Some(DeleteTree),
        // The last swap's old copy, kept then by a file still in use.
        (Place::InstallRoot, Dir) if name == obs_install::OLD => Some(DeleteTree),
        // An update without its version.txt, which `apply_staged_update` can never put in.
        (Place::InstallRoot, Dir) if name == obs_install::STAGED && s.no_version => Some(DeleteTree),
        // A VC++ installer's folder a setup cut short left behind.
        (Place::Base, Dir) if obs_install::is_vc_folder(name) && s.only_redist && older(HOUR) => Some(VcFolder),
        // A portable update's parked old exe, or a new one never swapped in.
        (Place::ExeDir, File) if ctx.exe.is_some_and(|e| s.path == app_update::beside(e, "old") || s.path == app_update::beside(e, "new")) => Some(Delete),
        // A save cut short (`clips::write_json`), of a store it really writes.
        (Place::Settings, File) if clips::STORE_FILES.iter().any(|f| clips::tmp_of(Path::new(f)) == Path::new(name)) && older(TMP_AGE) => Some(Delete),
        // A cover download cut short, or the markers of a game with no folder any more.
        (Place::Covers, File) if is_marker(name, "part") && older(DAY) => Some(Delete),
        (Place::Covers, File) if covers::MARKERS.iter().any(|m| is_marker(name, m)) && s.orphan => Some(Delete),
        // A centered copy that never took its clip's place, under its hidden name. The name 0.2.2
        // and earlier gave it ("x.centering.mp4") was listed as a clip, so it may have been
        // starred: never.
        (Place::Clips, File) if centering_clip(name).is_some() && s.sibling && s.free && older(HOUR) => Some(Recycle),
        // An archive copy cut short, while the clip it copies is still in the clips folder, and
        // nothing is writing it.
        (Place::Archive, File) if part_of(name).is_some() && s.sibling && s.free && older(DAY) => Some(Recycle),
        // An export cut short, still under its hidden name (`clips::make_export`), which `clips::list`
        // never shows; one of an archived clip is made beside it. One under way is younger.
        (Place::Clips | Place::Archive, File) if cut_short_export(name) && s.free && older(DAY) => Some(Recycle),
        _ => None,
    }
}

/// What goes of `seen`, in order. The `vc-` folders' parent goes last, only if every thing in it
/// goes (and `remove_dir` takes only an empty folder anyway).
fn plan(seen: &[Seen], now: SystemTime, ctx: &Ctx) -> Vec<Removal> {
    let mut out: Vec<Removal> = seen.iter().filter_map(|s| rule(s, now, ctx).map(|how| Removal { path: s.path.clone(), how, bytes: s.bytes })).collect();
    if let Some(base) = ctx.base
        && out.iter().any(|r| r.how == How::VcFolder)
        && seen.iter().filter(|s| s.place == Place::Base).all(|s| out.iter().any(|r| r.path == s.path))
    {
        out.push(Removal { path: base.to_path_buf(), how: How::IfEmpty, bytes: 0 });
    }
    out
}

/// Whether `name` is a covers-folder file of a game with extension `ext` (any case).
fn is_marker(name: &str, ext: &str) -> bool {
    let p = Path::new(name);
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)) && p.file_stem().is_some_and(|s| !s.is_empty())
}

/// The clip a centering file (`clips::centering_temp`) is the centered copy of:
/// "x.rv-centering.mp4" is x.mp4's.
fn centering_clip(name: &str) -> Option<String> {
    let stem = name.strip_suffix(".mp4")?.strip_suffix(clips::CENTERING_TEMP)?.strip_suffix('.')?;
    (!stem.is_empty()).then(|| format!("{stem}.mp4"))
}

/// What an archive copy cut short is a copy of: "x.mp4.part" is x.mp4's, for the kinds of file
/// `clips::list` shows only (so never of an app temp, which the archive never copies).
fn part_of(name: &str) -> Option<&str> {
    let target = name.strip_suffix(clips::COPY_PART)?;
    let p = Path::new(target);
    (clips::is_clip(p) && !clips::is_app_temp(target) && p.file_stem().is_some_and(|s| !s.is_empty())).then_some(target)
}

/// Whether `name` is an export's hidden name (`clips::export_temp`) of a kind exports are made as:
/// "x.rv-export.mp4" or "x.rv-export.gif" (`clips::copy_name`, `clips::montage_name`).
fn cut_short_export(name: &str) -> bool {
    clips::app_temp_of(name) == Some(clips::EXPORT_TEMP) && Path::new(name).extension().is_some_and(|e| e == "mp4" || e == "gif")
}

/// What's at `path` (a link isn't followed), as found in `place`; None if it can't be read.
fn look(path: PathBuf, place: Place) -> Option<Seen> {
    let m = fs::symlink_metadata(&path).ok()?;
    let kind = if m.is_file() { Kind::File } else if m.is_dir() { Kind::Dir } else { Kind::Other };
    Some(Seen { bytes: if kind == Kind::File { m.len() } else { 0 }, modified: m.modified().ok(), kind, path, place, ..Default::default() })
}

/// Whether Windows says `path` isn't there (not merely that it couldn't look). Note a drive that
/// isn't there says so too, so a rule that cares checks the drive first.
fn gone(path: &Path) -> bool {
    fs::symlink_metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

/// The files in `root` and in its folders one level down (what `clips::list` reads), each with
/// the name of the folder it's in (None for `root`). Links aren't followed.
fn two_levels(root: &Path) -> Vec<(PathBuf, Option<std::ffi::OsString>)> {
    let Ok(top) = fs::read_dir(root) else { return Vec::new() };
    let mut out = Vec::new();
    for e in top.flatten() {
        let Ok(t) = e.file_type() else { continue };
        if t.is_file() {
            out.push((e.path(), None));
        } else if t.is_dir()
            && let Ok(inner) = fs::read_dir(e.path())
        {
            out.extend(inner.flatten().filter(|i| i.file_type().is_ok_and(|t| t.is_file())).map(|i| (i.path(), Some(e.file_name()))));
        }
    }
    out
}

/// What's in `dir` and below, links not followed. Only for the exactly named folders `rule` lets
/// go whole, just before they go.
fn tree_size(dir: &Path) -> u64 {
    fs::read_dir(dir).map_or(0, |items| {
        items
            .flatten()
            .map(|e| match e.file_type() {
                Ok(t) if t.is_dir() => tree_size(&e.path()),
                Ok(t) if t.is_file() => e.metadata().map_or(0, |m| m.len()),
                _ => 0,
            })
            .sum()
    })
}

/// Carries out `todo`, skipping anything that fails, until `stop` says to; the names of what went
/// and its size. `recycle` moves a file to the Recycle Bin.
fn apply(todo: Vec<Removal>, stop: &dyn Fn() -> bool, recycle: &dyn Fn(&Path) -> anyhow::Result<()>) -> (Vec<String>, u64) {
    let (mut names, mut bytes) = (Vec::new(), 0);
    for r in todo {
        if stop() {
            break;
        }
        let size = match r.how {
            How::DeleteTree => tree_size(&r.path),
            How::VcFolder => fs::metadata(r.path.join(obs_install::VC_REDIST_FILE)).map_or(0, |m| m.len()),
            _ => r.bytes,
        };
        let done = match r.how {
            How::Delete => fs::remove_file(&r.path).is_ok(),
            How::DeleteTree => fs::remove_dir_all(&r.path).is_ok(),
            How::VcFolder => {
                let _ = fs::remove_file(r.path.join(obs_install::VC_REDIST_FILE));
                fs::remove_dir(&r.path).is_ok()
            }
            How::IfEmpty => fs::remove_dir(&r.path).is_ok(),
            How::Recycle => recycle(&r.path).is_ok(),
        };
        if done {
            let name = r.path.file_name().map_or_else(|| r.path.display().to_string(), |n| n.to_string_lossy().into_owned());
            names.push(if r.how == How::Recycle { format!("{name} (to the Recycle Bin)") } else { name });
            bytes += size;
        }
    }
    (names, bytes)
}

/// For phases with nothing to recycle: never does.
fn no_recycle(_: &Path) -> anyhow::Result<()> {
    anyhow::bail!("nothing here goes to the Recycle Bin")
}

/// Whether the app is closing (or being removed): a phase stops.
fn quitting() -> bool {
    crate::obs::QUITTING.load(Ordering::Relaxed)
}

/// The log line for what a phase took away.
fn tidied(names: &[String], bytes: u64) -> String {
    let size = match bytes {
        0 => String::new(),
        b if b >= 1 << 20 => format!(" ({} MB)", b >> 20),
        b => format!(" ({} KB)", b.div_ceil(1024)),
    };
    format!("Tidied up: {}{size}.", names.join(", "))
}

fn say(names: &[String], bytes: u64) {
    if !names.is_empty() {
        crate::log::write(&tidied(names, bytes));
    }
}

/// The install folder's phase, once per start, first thing on the OBS update thread so it never
/// meets a download there. Returns the status line's note when it freed a lot.
pub(crate) fn install_root_phase() -> Option<String> {
    if crate::DEV_BUILD {
        return None;
    }
    let (names, bytes) = install_root_in(&obs_install::install_root(), &obs_install::base(), SystemTime::now(), &quitting);
    say(&names, bytes);
    (bytes >= WORTH_SAYING).then(|| format!("Tidied up {} MB of leftover files.", bytes >> 20))
}

/// The install folder `root`'s rules, the `vc-` folders' in `base`, then OBS's debug files in the
/// private copy (`obs_install::prune`, which logs on its own).
fn install_root_in(root: &Path, base: &Path, now: SystemTime, stop: &dyn Fn() -> bool) -> (Vec<String>, u64) {
    let mut seen: Vec<Seen> = [obs_install::DOWNLOAD, obs_install::PARTIAL, obs_install::OLD, obs_install::STAGED].into_iter().filter_map(|n| look(root.join(n), Place::InstallRoot)).collect();
    for s in &mut seen {
        s.no_version = s.path.file_name().is_some_and(|n| n == obs_install::STAGED) && gone(&s.path.join("version.txt"));
    }
    if let Ok(items) = fs::read_dir(base) {
        for e in items.flatten() {
            let Some(mut s) = look(e.path(), Place::Base) else { continue };
            s.only_redist = s.kind == Kind::Dir && e.file_name().to_str().is_some_and(obs_install::is_vc_folder) && obs_install::holds_only_redist(&s.path);
            seen.push(s);
        }
    }
    let ctx = Ctx { base: (!obs_install::same_dir(base, root)).then_some(base), ..Default::default() };
    let gone = apply(plan(&seen, now, &ctx), stop, &no_recycle);
    if !stop() {
        obs_install::prune(&root.join(obs_install::PRIVATE));
    }
    gone
}

/// The app update's phase, at the top of `app_update::start`'s thread (so only where updates are
/// allowed): its installer, and a portable copy's old exe, which can take a moment to be let go of
/// after the restart (tried for up to 10 s).
pub(crate) fn app_update_phase() {
    if crate::DEV_BUILD {
        return;
    }
    let exe = std::env::current_exe().ok();
    let (names, bytes) = app_update_in(&obs_install::install_root(), exe.as_deref(), SystemTime::now(), &quitting, &|| std::thread::sleep(Duration::from_secs(1)));
    say(&names, bytes);
}

fn app_update_in(root: &Path, exe: Option<&Path>, now: SystemTime, stop: &dyn Fn() -> bool, wait: &dyn Fn()) -> (Vec<String>, u64) {
    let ctx = Ctx { exe, ..Default::default() };
    let (mut names, mut bytes) = (Vec::new(), 0);
    for _ in 0..10 {
        let mut paths = vec![(root.join(app_update::INSTALLER), Place::InstallRoot)];
        paths.extend(exe.into_iter().flat_map(|e| [app_update::beside(e, "old"), app_update::beside(e, "new")]).map(|p| (p, Place::ExeDir)));
        let seen: Vec<Seen> = paths.into_iter().filter_map(|(p, place)| look(p, place)).collect();
        let todo = plan(&seen, now, &ctx);
        let planned = todo.len();
        if planned == 0 {
            break;
        }
        let (gone, b) = apply(todo, stop, &no_recycle);
        let left = planned - gone.len();
        names.extend(gone);
        bytes += b;
        if left == 0 || stop() {
            break;
        }
        wait();
    }
    (names, bytes)
}

/// The places the settings phase looks in, all this copy's own but the clips and archive folders.
struct Folders<'a> {
    settings: &'a Path,
    covers: &'a Path,
    clips: &'a Path,
    archive: Option<&'a Path>,
}

/// The settings and clips folders' phase, on its own thread at start: `clips_folder` and `archive`
/// (None when the archive is off) as saved, and `shared_ok` the folder auto-cleanup may work in
/// anyway (`clips::folder_risk`). The clips and archive rules wait for the next start while an
/// auto-cleanup or archive pass runs, and are off for a folder that's also the user's own (a
/// library or a drive's top), as auto-cleanup is (`rules_on`). Then the registry leftovers.
pub(crate) fn settings_phase(clips_folder: &str, archive: Option<&Path>, shared_ok: &str) {
    if crate::DEV_BUILD {
        return;
    }
    // Held until the clips rules are done, so no pass starts meanwhile.
    let idle = clips::passes_idle();
    let rules = rules_on(idle.is_some(), clips_folder, archive, &|f| clips::folder_risk(f, shared_ok));
    let (settings, covers) = (crate::config::dir(), covers::dir());
    let folders = Folders { settings: &settings, covers: &covers, clips: Path::new(clips_folder), archive };
    let (mut names, bytes) = settings_in(&folders, rules, SystemTime::now(), &quitting, &clips::recycle_unasked);
    drop(idle);
    if !quitting() {
        names.extend(undo_registry());
    }
    say(&names, bytes);
}

/// Whether the clips folder's rules and the archive folder's may run: only while no pass runs
/// (`idle`), and each only for a folder `risk` (`clips::folder_risk`, with the folder the user
/// confirmed) has nothing against. The archive's copies are of the clips folder's clips, so its
/// rules also need the clips folder's.
fn rules_on(idle: bool, clips: &str, archive: Option<&Path>, risk: &dyn Fn(&str) -> Option<String>) -> (bool, bool) {
    let clips_ok = idle && risk(clips).is_none();
    (clips_ok, clips_ok && archive.is_some_and(|a| risk(&a.display().to_string()).is_none()))
}

/// `rules` says whether the clips folder's and the archive folder's rules run (`rules_on`).
fn settings_in(f: &Folders, (clips_rules, archive_rules): (bool, bool), now: SystemTime, stop: &dyn Fn() -> bool, recycle: &dyn Fn(&Path) -> anyhow::Result<()>) -> (Vec<String>, u64) {
    let mut seen: Vec<Seen> = clips::STORE_FILES.iter().filter_map(|s| look(f.settings.join(clips::tmp_of(Path::new(s))), Place::Settings)).collect();
    // A game's markers count as orphans only while the clips folder and the archive are both
    // there: with a drive or NAS off, every game would look gone.
    let roots: Vec<&Path> = std::iter::once(f.clips).chain(f.archive).collect();
    let roots_there = roots.iter().all(|r| r.is_dir());
    if let Ok(items) = fs::read_dir(f.covers) {
        for e in items.flatten() {
            if !e.file_name().to_str().is_some_and(|n| covers::MARKERS.iter().any(|m| is_marker(n, m))) {
                continue;
            }
            let Some(mut s) = look(e.path(), Place::Covers) else { continue };
            s.orphan = roots_there && s.path.file_stem().is_some_and(|game| !roots.iter().any(|r| r.join(game).is_dir()));
            seen.push(s);
        }
    }
    if clips_rules && !stop() {
        for (path, _) in two_levels(f.clips) {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
            let clip = centering_clip(name);
            if clip.is_none() && !cut_short_export(name) {
                continue;
            }
            let Some(mut s) = look(path.clone(), Place::Clips) else { continue };
            s.sibling = clip.is_some_and(|c| path.with_file_name(c).is_file());
            s.free = !crate::recorder::held_for_writing(&path);
            seen.push(s);
        }
    }
    if archive_rules && !stop() {
        for (path, group) in f.archive.map(two_levels).unwrap_or_default() {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
            let target = part_of(name);
            if target.is_none() && !cut_short_export(name) {
                continue;
            }
            let source = target.map(|t| group.map_or_else(|| f.clips.to_path_buf(), |g| f.clips.join(g)).join(t));
            let Some(mut s) = look(path.clone(), Place::Archive) else { continue };
            s.sibling = source.is_some_and(|p| p.is_file());
            s.free = !crate::recorder::held_for_writing(&path);
            seen.push(s);
        }
    }
    apply(plan(&seen, now, &Ctx::default()), stop, recycle)
}

/// A GPU preference recorded in this copy's own tweaks.json (`tweaks::gpu_entries`), with what
/// its rule needs.
pub(crate) struct GpuSeen {
    pub exe: String,
    /// What it was before this copy set it; null when there was none.
    pub original: Value,
    /// Its data in the registry now.
    pub now: Value,
    /// The exe's drive (`X:\`) is there.
    pub drive_there: bool,
    /// Its `<root>\obs-studio` folder surely isn't.
    pub folder_gone: bool,
}

/// The pre-rename startup entry ("OBS Instant Replay", `startup::old_entry`): whether it's in
/// Run, and whether Task Manager still keeps its on or off.
#[derive(Clone, Copy)]
pub(crate) struct OldStartup {
    pub run: bool,
    pub approved: bool,
}

/// A registry leftover to undo.
#[derive(Debug, PartialEq)]
pub(crate) enum Undo {
    /// The graphics setting this copy set for an OBS copy that's gone: deleted, as there was none.
    Gpu(String),
    /// Task Manager's on or off for the pre-rename entry, which is gone.
    OldApproved,
}

impl Undo {
    fn text(&self) -> String {
        match self {
            Undo::Gpu(exe) => format!("the graphics setting for an OBS copy that's gone ({exe})"),
            Undo::OldApproved => "Task Manager's setting for the old OBS Instant Replay startup entry".into(),
        }
    }
}

/// What to undo in the registry, from what's recorded and found: only GPU preferences this copy
/// set where there was none, still exactly as it set them, for a private OBS that's surely gone
/// (not `current_obs`; the drive there, the folder not), and Task Manager's leftover for the old
/// entry only where `inst` may touch old names. Game Bar, NVIDIA, AMD and the Run entry are never
/// here.
pub(crate) fn plan_registry(gpu: &[GpuSeen], old: OldStartup, inst: &crate::instance::Instance, current_obs: &Path) -> Vec<Undo> {
    let current = current_obs.display().to_string().to_lowercase();
    let mut out: Vec<Undo> = gpu
        .iter()
        .filter(|g| g.original.is_null() && g.now == crate::tweaks::gpu_high() && g.exe.to_lowercase() != current && obs_folder_of(&g.exe).is_some() && g.drive_there && g.folder_gone)
        .map(|g| Undo::Gpu(g.exe.clone()))
        .collect();
    if inst.legacy_names_allowed() && old.approved && !old.run {
        out.push(Undo::OldApproved);
    }
    out
}

/// For an exe path of exactly the shape `X:\<root>\obs-studio\bin\64bit\obs64.exe` (`exe_in` of a
/// private copy), its drive's top folder and its `<root>\obs-studio`. None for any other form (a
/// network path, `.` or `..` parts, other names or case), which is never judged.
fn obs_folder_of(exe: &str) -> Option<(PathBuf, PathBuf)> {
    let rest = match exe.as_bytes() {
        [d, b':', b'\\', ..] if d.is_ascii_alphabetic() => &exe[3..],
        _ => return None,
    };
    let parts: Vec<&str> = rest.split('\\').collect();
    let shape = obs_install::exe_in(Path::new(obs_install::PRIVATE));
    let tail: Vec<&str> = shape.to_str()?.split('\\').collect();
    if parts.len() < tail.len() || parts.iter().any(|p| p.is_empty() || *p == "." || *p == ".." || p.contains(['/', ':'])) || parts[parts.len() - tail.len()..] != tail[..] {
        return None;
    }
    Some((PathBuf::from(&exe[..3]), Path::new(exe).ancestors().nth(tail.len() - 1)?.to_path_buf()))
}

/// Undoes what `plan_registry` picks; what it undid, in words.
fn undo_registry() -> Vec<String> {
    let gpu: Vec<GpuSeen> = crate::tweaks::gpu_entries()
        .into_iter()
        .map(|(exe, original, now)| {
            let (drive_there, folder_gone) = obs_folder_of(&exe).map_or((false, false), |(drive, folder)| (drive.is_dir(), gone(&folder)));
            GpuSeen { exe, original, now, drive_there, folder_gone }
        })
        .collect();
    let (run, approved) = crate::startup::old_entry();
    let current = obs_install::exe_in(&obs_install::private_dir());
    plan_registry(&gpu, OldStartup { run, approved }, crate::instance::current(), &current)
        .into_iter()
        .filter(|u| match u {
            Undo::Gpu(exe) => crate::tweaks::restore_gpu(exe).is_ok(),
            Undo::OldApproved => crate::startup::drop_old_approved().is_ok(),
        })
        .map(|u| u.text())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp(what: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("replay-vault-tidy-{what}-{}", crate::config::random_hex()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Makes `path` (a file or a folder) look `by` old.
    fn age(path: &Path, by: Duration) {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        let f = fs::OpenOptions::new().access_mode(FILE_WRITE_ATTRIBUTES).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(path).unwrap();
        f.set_modified(SystemTime::now() - by).unwrap();
    }

    fn file(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"x").unwrap();
    }

    fn seen(path: &str, place: Place, kind: Kind, old: Duration) -> Seen {
        Seen { path: PathBuf::from(path), place, kind, modified: Some(SystemTime::now() - old), ..Default::default() }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        out.sort();
        out
    }

    fn never() -> bool {
        false
    }
    const NEVER: &dyn Fn() -> bool = &never;

    /// Only the listed names count, and only when old enough and proven; anything else in the
    /// install folder (the private copy, the user's own files, a link by a listed name) stays.
    #[test]
    fn install_folder_rules() {
        let now = SystemTime::now();
        let ctx = Ctx::default();
        let r = |s: Seen| rule(&s, now, &ctx);
        let (root, two_h, young) = (Place::InstallRoot, 2 * HOUR, Duration::from_secs(60));
        assert_eq!(r(seen(r"E:\OBS\obs-download.zip", root, Kind::File, two_h)), Some(How::Delete));
        assert_eq!(r(seen(r"E:\OBS\obs-download.zip", root, Kind::File, young)), None, "a download may be under way");
        assert_eq!(r(seen(r"E:\OBS\obs-studio.partial", root, Kind::Dir, two_h)), Some(How::DeleteTree));
        assert_eq!(r(seen(r"E:\OBS\obs-studio.partial", root, Kind::Dir, young)), None);
        assert_eq!(r(seen(r"E:\OBS\obs-studio.old", root, Kind::Dir, young)), Some(How::DeleteTree), "no age rule");
        assert_eq!(r(seen(r"E:\OBS\obs-studio.update", root, Kind::Dir, two_h)), None, "version.txt not proven gone");
        assert_eq!(r(Seen { no_version: true, ..seen(r"E:\OBS\obs-studio.update", root, Kind::Dir, young) }), Some(How::DeleteTree));
        assert_eq!(r(seen(r"E:\OBS\replay-vault-setup.exe", root, Kind::File, young)), Some(How::Delete));
        // Never: the private copy, other names, the wrong kind, a link, no time.
        for (path, kind) in [
            (r"E:\OBS\obs-studio", Kind::Dir),
            (r"E:\OBS\obs-download.zip", Kind::Dir),
            (r"E:\OBS\obs-studio.old", Kind::File),
            (r"E:\OBS\obs-studio.old", Kind::Other),
            (r"E:\OBS\obs-studio.partial", Kind::Other),
            (r"E:\OBS\My video.mp4", Kind::File),
            (r"E:\OBS\obs-download.zip.txt", Kind::File),
            (r"E:\OBS\last-update-check", Kind::File),
        ] {
            assert_eq!(r(seen(path, root, kind, 2 * DAY)), None, "{path} {kind:?}");
        }
        assert_eq!(r(Seen { modified: None, ..seen(r"E:\OBS\obs-download.zip", root, Kind::File, two_h) }), None);
        // The same names elsewhere are not this rule's.
        assert_eq!(r(seen(r"D:\Clips\obs-download.zip", Place::Clips, Kind::File, two_h)), None);
        assert_eq!(r(seen(r"D:\Clips\obs-studio.old", Place::Archive, Kind::Dir, two_h)), None);
    }

    /// A vc- folder needs its exact name, nothing but the installer in it, and an hour; its parent
    /// goes only when it isn't the install folder and everything in it goes.
    #[test]
    fn vc_folder_rules() {
        let now = SystemTime::now();
        let hex = crate::config::random_hex();
        let vc = |name: &str, only: bool, old: Duration| Seen { only_redist: only, ..seen(&format!(r"C:\Local\replay-vault\{name}"), Place::Base, Kind::Dir, old) };
        let good = format!("vc-{hex}");
        let ctx = Ctx::default();
        assert_eq!(rule(&vc(&good, true, 2 * HOUR), now, &ctx), Some(How::VcFolder));
        assert_eq!(rule(&vc(&good, true, Duration::from_secs(60)), now, &ctx), None, "a setup may be under way");
        assert_eq!(rule(&vc(&good, false, 2 * HOUR), now, &ctx), None, "something else in it");
        for name in [format!("vc-{}", &hex[1..]), format!("vc-{}", "0123456789ABCDEF".repeat(2)), format!("vc-{hex}x"), "vc-folder".into()] {
            assert_eq!(rule(&vc(&name, true, 2 * HOUR), now, &ctx), None, "{name}");
        }
        let base = Path::new(r"C:\Local\replay-vault");
        let may_go = Ctx { base: Some(base), ..Default::default() };
        let only_vc = [vc(&good, true, 2 * HOUR)];
        assert_eq!(plan(&only_vc, now, &may_go).last().map(|r| (&r.how, r.path.as_path())), Some((&How::IfEmpty, base)));
        assert!(plan(&only_vc, now, &ctx).iter().all(|r| r.how != How::IfEmpty), "the install folder itself: never");
        let with_other = [vc(&good, true, 2 * HOUR), seen(r"C:\Local\replay-vault\notes.txt", Place::Base, Kind::File, 2 * DAY)];
        assert!(plan(&with_other, now, &may_go).iter().all(|r| r.how != How::IfEmpty), "not empty");
        assert!(plan(&[seen(r"C:\Local\replay-vault\x", Place::Base, Kind::Dir, 2 * DAY)], now, &may_go).is_empty(), "no vc- folder went");
    }

    /// The settings folder: only a known store's .json.tmp, ten minutes on; covers' half downloads
    /// after a day, and markers of games with no folder; never a backup, tweaks.json or a log.
    #[test]
    fn settings_rules() {
        let now = SystemTime::now();
        let ctx = Ctx::default();
        let r = |s: Seen| rule(&s, now, &ctx);
        let s = Place::Settings;
        assert_eq!(r(seen(r"C:\S\tags.json.tmp", s, Kind::File, HOUR)), Some(How::Delete));
        assert_eq!(r(seen(r"C:\S\uploads.json.tmp", s, Kind::File, HOUR)), Some(How::Delete));
        assert_eq!(r(seen(r"C:\S\config.json.tmp", s, Kind::File, Duration::from_secs(60))), None, "a save may be under way");
        for name in ["tweaks.json.tmp", "other.json.tmp", "tags.json.bak", "tags.json", "tweaks.json", "tweaks.bad.json", "config.bad.json", "app.log", "window.json", "recorder.pid"] {
            assert_eq!(r(seen(&format!(r"C:\S\{name}"), s, Kind::File, 30 * DAY)), None, "{name}");
        }
        let c = Place::Covers;
        assert_eq!(r(seen(r"C:\S\covers\VALORANT.part", c, Kind::File, 2 * DAY)), Some(How::Delete));
        assert_eq!(r(seen(r"C:\S\covers\VALORANT.part", c, Kind::File, HOUR)), None);
        assert_eq!(r(seen(r"C:\S\covers\VALORANT.skip", c, Kind::File, 30 * DAY)), None, "its game has a folder");
        assert_eq!(r(Seen { orphan: true, ..seen(r"C:\S\covers\Gone.none", c, Kind::File, HOUR) }), Some(How::Delete));
        assert_eq!(r(Seen { orphan: true, ..seen(r"C:\S\covers\Gone.jpg", c, Kind::File, HOUR) }), None, "covers stay");
    }

    /// Clips and archive: only a centering copy under its hidden name beside its clip, free and an
    /// hour old (never one under the old name, which the clip list showed), and an archive copy of
    /// a clip kind whose source is still there, a day old. Both to the Recycle Bin.
    #[test]
    fn clips_rules() {
        let now = SystemTime::now();
        let ctx = Ctx::default();
        let r = |s: Seen| rule(&s, now, &ctx);
        let proven = |path: &str, place: Place, old: Duration| Seen { sibling: true, free: true, ..seen(path, place, Kind::File, old) };
        // The old name, beside its clip, free and old: never (it was listed, so it may be starred).
        assert_eq!(r(proven(r"D:\Clips\VALORANT\Replay 1.centering.mp4", Place::Clips, 30 * DAY)), None);
        let new = r"D:\Clips\VALORANT\Replay 1.rv-centering.mp4";
        assert_eq!(r(proven(new, Place::Clips, 2 * HOUR)), Some(How::Recycle));
        assert_eq!(r(proven(new, Place::Clips, HOUR / 2)), None, "young");
        assert_eq!(r(Seen { free: false, ..proven(new, Place::Clips, 2 * DAY) }), None, "being written");
        assert_eq!(r(Seen { sibling: false, ..proven(new, Place::Clips, 2 * DAY) }), None, "no clip beside it");
        assert_eq!(r(proven(new, Place::Archive, 2 * DAY)), None, "not in the archive");
        assert_eq!(r(proven(r"E:\Old\VALORANT\x.mp4.part", Place::Archive, 2 * DAY)), Some(How::Recycle));
        assert_eq!(r(proven(r"E:\Old\shot.PNG.part", Place::Archive, 2 * DAY)), Some(How::Recycle));
        assert_eq!(r(proven(r"E:\Old\VALORANT\x.mp4.part", Place::Archive, HOUR)), None, "young");
        assert_eq!(r(Seen { sibling: false, ..proven(r"E:\Old\VALORANT\x.mp4.part", Place::Archive, 2 * DAY) }), None, "its clip is gone");
        assert_eq!(r(Seen { free: false, ..proven(r"E:\Old\VALORANT\x.mp4.part", Place::Archive, 2 * DAY) }), None, "being written");
        assert_eq!(r(proven(r"D:\Clips\x.mp4.part", Place::Clips, 2 * DAY)), None, "never a .part in the clips folder");
        // Clips, recordings, montages, exports, other kinds: never, however old or proven.
        for name in ["x.mp4", "Montage 1.mp4", "x (trim).mp4", "x.png", "x.mkv", "x.txt.part", ".mp4.part", "x.part", "centering.mp4", ".centering.mp4", "x.centering.mkv", "x.mp4.tmp", "x.centering.mp4", "rv-centering.mp4", ".rv-centering.mp4", "x.rv-centering.mkv", "x.rv-centering", "x rv-centering.mp4", "x.rv-centering.mp4.part"] {
            for place in [Place::Clips, Place::Archive] {
                assert_eq!(r(proven(&format!(r"D:\Clips\{name}"), place, 30 * DAY)), None, "{name} {place:?}");
            }
        }
        assert_eq!(centering_clip("Replay 1.centering.mp4"), None);
        assert_eq!(centering_clip("Replay 1.rv-centering.mp4").as_deref(), Some("Replay 1.mp4"));
        assert_eq!(centering_clip(&clips::centering_temp(Path::new("Replay 1.MP4")).display().to_string()).as_deref(), Some("Replay 1.mp4"));
        assert_eq!(part_of("x.MOV.part"), Some("x.MOV"));
    }

    /// An export cut short: only its exact hidden name, as an .mp4 or .gif, in the clips folder or
    /// the archive, free and a day old. To the Recycle Bin.
    #[test]
    fn export_rules() {
        let now = SystemTime::now();
        let ctx = Ctx::default();
        let r = |s: Seen| rule(&s, now, &ctx);
        let free = |path: &str, place: Place, old: Duration| Seen { free: true, ..seen(path, place, Kind::File, old) };
        let trim = r"D:\Clips\VALORANT\Replay 1 (trim).rv-export.mp4";
        assert_eq!(r(free(trim, Place::Clips, 2 * DAY)), Some(How::Recycle));
        assert_eq!(r(free(r"D:\Clips\Montage 2026-11-07 10-53-12.rv-export.mp4", Place::Clips, 2 * DAY)), Some(How::Recycle));
        assert_eq!(r(free(r"E:\Old\VALORANT\x (GIF).rv-export.gif", Place::Archive, 2 * DAY)), Some(How::Recycle));
        assert_eq!(r(free(trim, Place::Clips, 20 * HOUR)), None, "may be under way");
        assert_eq!(r(Seen { free: false, ..free(trim, Place::Clips, 2 * DAY) }), None, "being written");
        assert_eq!(r(Seen { modified: None, ..free(trim, Place::Clips, 2 * DAY) }), None, "age unknown");
        assert_eq!(r(Seen { kind: Kind::Dir, ..free(trim, Place::Clips, 2 * DAY) }), None, "a folder");
        // Never made as these (an .mkv, another case), or not that exact shape.
        for name in ["x.rv-export.mkv", "x.rv-export.mov", "x.rv-export.png", "x.rv-export.MP4", ".rv-export.mp4", "rv-export.mp4", "x.rv-export", "x rv-export.mp4", "x.rv-exports.mp4", "rv-export x.mp4", "x.rv-export.mp4.part", "x.rv-export.mp4.txt", "x.mp4"] {
            for place in [Place::Clips, Place::Archive] {
                assert_eq!(r(free(&format!(r"D:\Clips\{name}"), place, 30 * DAY)), None, "{name} {place:?}");
            }
        }
        for place in [Place::Settings, Place::Covers, Place::InstallRoot, Place::Base, Place::ExeDir] {
            assert_eq!(r(free(trim, place, 30 * DAY)), None, "{place:?}");
        }
    }

    /// GPU preferences: only one this copy set over nothing, still as it set it, for a private
    /// copy's exe whose drive is there and whose folder is gone, and not the OBS in use now.
    #[test]
    fn gpu_rules() {
        let exe = r"E:\Old\obs-studio\bin\64bit\obs64.exe";
        let current = Path::new(r"C:\Users\X\AppData\Local\replay-vault\obs-studio\bin\64bit\obs64.exe");
        let ok = || GpuSeen { exe: exe.into(), original: Value::Null, now: crate::tweaks::gpu_high(), drive_there: true, folder_gone: true };
        let none = OldStartup { run: false, approved: false };
        let inst = crate::instance::current();
        assert_eq!(plan_registry(&[ok()], none, inst, current), [Undo::Gpu(exe.into())]);
        for (g, why) in [
            (GpuSeen { drive_there: false, ..ok() }, "the drive isn't there"),
            (GpuSeen { folder_gone: false, ..ok() }, "the folder is there"),
            (GpuSeen { now: json!({ "string": "GpuPreference=1;" }), ..ok() }, "changed since"),
            (GpuSeen { now: Value::Null, ..ok() }, "already gone"),
            (GpuSeen { original: json!({ "string": "GpuPreference=0;" }), ..ok() }, "there was one before"),
            (GpuSeen { exe: current.display().to_string(), ..ok() }, "the OBS in use"),
            (GpuSeen { exe: r"E:\Old\obs-studio\bin\64bit\other.exe".into(), ..ok() }, "another shape"),
            (GpuSeen { exe: r"\\nas\obs\obs-studio\bin\64bit\obs64.exe".into(), ..ok() }, "a network path"),
        ] {
            assert!(plan_registry(&[g], none, inst, current).is_empty(), "{why}");
        }
        let lower = PathBuf::from(exe.to_lowercase());
        assert!(plan_registry(&[ok()], none, inst, &lower).is_empty(), "the OBS in use, written in another case");
        assert_eq!(obs_folder_of(exe), Some((PathBuf::from(r"E:\"), PathBuf::from(r"E:\Old\obs-studio"))));
        assert_eq!(obs_folder_of(r"E:\obs-studio\bin\64bit\obs64.exe"), Some((PathBuf::from(r"E:\"), PathBuf::from(r"E:\obs-studio"))));
        for not in [
            r"\\?\E:\Old\obs-studio\bin\64bit\obs64.exe",
            r"E:Old\obs-studio\bin\64bit\obs64.exe",
            r"E:/Old/obs-studio/bin/64bit/obs64.exe",
            r"E:\Old\..\obs-studio\bin\64bit\obs64.exe",
            r"E:\Old\\obs-studio\bin\64bit\obs64.exe",
            r"E:\Old\OBS-Studio\bin\64bit\obs64.exe",
            r"E:\Old\obs-studio\bin\64bit\obs64.exe\",
            r"E:\Old\obs-studio\bin\32bit\obs32.exe",
            r"obs-studio\bin\64bit\obs64.exe",
            "",
        ] {
            assert_eq!(obs_folder_of(not), None, "{not}");
        }
    }

    /// Task Manager's leftover for the old entry goes only once the entry itself is gone.
    #[test]
    fn old_startup_rule() {
        let current = Path::new(r"C:\x\obs-studio\bin\64bit\obs64.exe");
        let inst = crate::instance::current();
        assert_eq!(plan_registry(&[], OldStartup { run: false, approved: true }, inst, current), [Undo::OldApproved]);
        assert!(plan_registry(&[], OldStartup { run: true, approved: true }, inst, current).is_empty(), "startup::repair moves it");
        assert!(plan_registry(&[], OldStartup { run: false, approved: false }, inst, current).is_empty());
    }

    #[test]
    fn log_text() {
        assert_eq!(tidied(&["obs-download.zip".into(), "obs-studio.partial".into()], 774 << 20), "Tidied up: obs-download.zip, obs-studio.partial (774 MB).");
        assert_eq!(tidied(&["tags.json.tmp".into()], 1500), "Tidied up: tags.json.tmp (2 KB).");
        assert_eq!(tidied(&[Undo::OldApproved.text()], 0), "Tidied up: Task Manager's setting for the old OBS Instant Replay startup entry.");
    }

    /// On disk: a user-picked install folder keeps the user's files, young leftovers and a waiting
    /// update; old leftovers and vc- folders go, and the vc- folders' parent only once empty and
    /// never when it's the install folder.
    #[test]
    fn install_folder_on_disk() {
        let (root, base) = (temp("root"), temp("base"));
        let hex = || crate::config::random_hex();
        file(&root.join("My video.mp4"));
        file(&root.join(r"obs-studio\bin\64bit\obs64.exe"));
        file(&root.join(r"obs-studio.update\version.txt"));
        file(&root.join(r"obs-studio.old\bin\x.dll"));
        file(&root.join(r"obs-studio.partial\bin\y.dll"));
        file(&root.join("obs-download.zip"));
        let (vc_old, vc_young, vc_full) = (base.join(format!("vc-{}", hex())), base.join(format!("vc-{}", hex())), base.join(format!("vc-{}", hex())));
        for d in [&vc_old, &vc_young, &vc_full] {
            file(&d.join(obs_install::VC_REDIST_FILE));
        }
        file(&vc_full.join("other.txt"));
        for p in [&vc_old, &vc_full] {
            age(p, 2 * HOUR);
        }
        // Young: only the old copy goes.
        let (gone, _) = install_root_in(&root, &base, SystemTime::now(), NEVER);
        assert_eq!(gone, ["obs-studio.old", &*vc_old.file_name().unwrap().to_string_lossy()]);
        assert_eq!(names(&root), ["My video.mp4", "obs-download.zip", "obs-studio", "obs-studio.partial", "obs-studio.update"]);
        assert!(vc_young.exists() && vc_full.exists() && base.exists(), "young, or something else in it");
        // An hour on: the download and unpacking cut short go; the update with its version stays.
        for p in [root.join("obs-download.zip"), root.join("obs-studio.partial")] {
            age(&p, 2 * HOUR);
        }
        let (gone, bytes) = install_root_in(&root, &base, SystemTime::now(), NEVER);
        assert_eq!(gone, ["obs-download.zip", "obs-studio.partial"]);
        assert_eq!(bytes, 2);
        assert!(root.join(r"obs-studio.update\version.txt").exists() && root.join(r"obs-studio\bin\64bit\obs64.exe").exists() && root.join("My video.mp4").exists());
        // Without its version.txt the update can never go in, so it goes.
        fs::remove_file(root.join(r"obs-studio.update\version.txt")).unwrap();
        assert_eq!(install_root_in(&root, &base, SystemTime::now(), NEVER).0, ["obs-studio.update"]);
        // The last vc- folder going takes its parent too, once nothing else is in it.
        fs::remove_dir_all(&vc_full).unwrap();
        age(&vc_young, 2 * HOUR);
        let (gone, _) = install_root_in(&root, &base, SystemTime::now(), NEVER);
        assert_eq!(gone.len(), 2, "{gone:?}");
        assert!(!base.exists());
        // vc- folders in the install folder itself: they go, the folder never does.
        let vc = root.join(format!("vc-{}", hex()));
        file(&vc.join(obs_install::VC_REDIST_FILE));
        age(&vc, 2 * HOUR);
        fs::remove_file(root.join("My video.mp4")).unwrap();
        fs::remove_dir_all(root.join("obs-studio")).unwrap();
        install_root_in(&root, &root, SystemTime::now(), NEVER);
        assert!(!vc.exists() && root.exists());
        // Told to stop: nothing goes.
        file(&root.join(r"obs-studio.old\x.dll"));
        assert!(install_root_in(&root, &root, SystemTime::now(), &|| true).0.is_empty());
        assert!(root.join("obs-studio.old").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    /// On disk: an update's installer and a portable copy's parked exe go; the exe and anything
    /// named like them don't. Nothing left: no waiting.
    #[test]
    fn app_update_on_disk() {
        let (root, dir) = (temp("upd-root"), temp("upd-exe"));
        let exe = dir.join("ReplayVault.exe");
        for p in [root.join(app_update::INSTALLER), root.join("replay-vault-setup.exe.txt"), exe.clone(), app_update::beside(&exe, "old"), app_update::beside(&exe, "new"), app_update::beside(&exe, "bak"), dir.join("Other.exe.old")] {
            file(&p);
        }
        let waits = std::cell::Cell::new(0);
        let (gone, bytes) = app_update_in(&root, Some(&exe), SystemTime::now(), NEVER, &|| waits.set(waits.get() + 1));
        assert_eq!(gone, ["replay-vault-setup.exe", "ReplayVault.exe.old", "ReplayVault.exe.new"]);
        assert_eq!((bytes, waits.get()), (3, 0));
        assert_eq!(names(&root), ["replay-vault-setup.exe.txt"]);
        assert_eq!(names(&dir), ["Other.exe.old", "ReplayVault.exe", "ReplayVault.exe.bak"]);
        assert!(app_update_in(&root, Some(&exe), SystemTime::now(), NEVER, &|| panic!("nothing to wait for")).0.is_empty());
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    /// On disk, with a stand-in Recycle Bin: the settings folder's cut-short saves and orphan cover
    /// markers, centering copies beside their clips and archive copies of clips still there;
    /// everything else stays, and the clips rules only when allowed.
    #[test]
    fn settings_and_clips_on_disk() {
        let root = temp("settings");
        let (settings, covers, clips_dir, archive) = (root.join("settings"), root.join(r"settings\covers"), root.join("clips"), root.join("archive"));
        for f in ["tags.json.tmp", "config.json.tmp", "other.json.tmp", "tags.json.bak", "tweaks.json"] {
            file(&settings.join(f));
        }
        for f in ["tags.json.tmp", "other.json.tmp", "tags.json.bak", "tweaks.json"] {
            age(&settings.join(f), HOUR);
        }
        for f in ["Inbox (3).skip", "Gone.none", "Gone.part", "Gone.jpg", "Kept.skip", "Kept.none", "Kept.part", "Young.part"] {
            file(&covers.join(f));
        }
        age(&covers.join("Kept.part"), 2 * DAY);
        // The .centering.mp4 files have the old name, which stays; n.rv-centering.mp4 is the new one.
        let old = [r"a.centering.mp4", r"a.mp4", r"b.centering.mp4", r"VALORANT\c.centering.mp4", r"VALORANT\c.mp4", r"VALORANT\deep\d.centering.mp4", r"VALORANT\deep\d.mp4", r"x.mp4.part", r"x.mp4", "s.centering.mp4", "s.mp4", "n.rv-centering.mp4", "n.mp4"];
        let exports = [r"a (trim).rv-export.mp4", r"VALORANT\c (GIF).rv-export.gif", r"VALORANT\c.rv-export.mkv", r"VALORANT\deep\d (trim).rv-export.mp4"];
        for f in exports {
            file(&clips_dir.join(f));
            age(&clips_dir.join(f), 2 * DAY);
        }
        file(&clips_dir.join(r"VALORANT\young (trim).rv-export.mp4"));
        for f in old.iter().chain(&["young.centering.mp4", "young.mp4"]) {
            file(&clips_dir.join(f));
        }
        for f in old {
            age(&clips_dir.join(f), 2 * HOUR);
        }
        fs::create_dir_all(clips_dir.join("Empty game")).unwrap();
        fs::create_dir_all(clips_dir.join("Kept")).unwrap();
        let parts = [r"VALORANT\c.mp4.part", r"VALORANT\gone.mp4.part", r"x.mp4.part", r"VALORANT\c.txt.part", r"VALORANT\deep\d.mp4.part"];
        for f in parts {
            file(&archive.join(f));
            age(&archive.join(f), 2 * DAY);
        }
        file(&archive.join(r"VALORANT\young.mp4.part"));
        file(&clips_dir.join(r"VALORANT\young.mp4"));
        for f in [r"VALORANT\x (720p).rv-export.mp4", r"held.rv-export.mp4"] {
            file(&archive.join(f));
            age(&archive.join(f), 2 * DAY);
        }
        let writing = fs::OpenOptions::new().write(true).open(archive.join("held.rv-export.mp4")).unwrap();
        // An archive copy still being written, its source there.
        file(&archive.join(r"VALORANT\h.mp4.part"));
        age(&archive.join(r"VALORANT\h.mp4.part"), 2 * DAY);
        file(&clips_dir.join(r"VALORANT\h.mp4"));
        let copying = fs::OpenOptions::new().write(true).open(archive.join(r"VALORANT\h.mp4.part")).unwrap();
        // Stars, tags and the like aren't these files', so a stand-in that only moves the file will do.
        let recycled = std::cell::RefCell::new(Vec::new());
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let recycle = |p: &Path| -> anyhow::Result<()> {
            recycled.borrow_mut().push(p.strip_prefix(&root).unwrap().display().to_string());
            fs::rename(p, bin.join(recycled.borrow().len().to_string()))?;
            Ok(())
        };
        // With the archive folder not there, no marker is an orphan; clips rules off.
        let missing = root.join("not there");
        let f = Folders { settings: &settings, covers: &covers, clips: &clips_dir, archive: Some(&missing) };
        let (gone, _) = settings_in(&f, (false, false), SystemTime::now(), NEVER, &recycle);
        assert_eq!(gone, ["tags.json.tmp", "Kept.part"]);
        assert!(recycled.borrow().is_empty());
        assert_eq!(names(&settings), ["config.json.tmp", "covers", "other.json.tmp", "tags.json.bak", "tweaks.json"]);
        // Both folders there, clips rules on.
        let f = Folders { archive: Some(&archive), ..f };
        // The archive folder is the user's own (`rules_on`): the clips folder's rules only.
        let (gone, _) = settings_in(&f, (true, false), SystemTime::now(), NEVER, &recycle);
        assert_eq!(gone.len(), 7, "{gone:?}");
        assert!(recycled.borrow().iter().all(|p| p.starts_with("clips")), "{:?}", recycled.borrow());
        let (gone, _) = settings_in(&f, (true, true), SystemTime::now(), NEVER, &recycle);
        assert_eq!(gone.len(), 3, "{gone:?}");
        assert_eq!(names(&covers), ["Gone.jpg", "Kept.none", "Kept.skip"]);
        let mut got = recycled.borrow().clone();
        got.sort();
        assert_eq!(
            got,
            [
                r"archive\VALORANT\c.mp4.part",
                r"archive\VALORANT\x (720p).rv-export.mp4",
                r"archive\x.mp4.part",
                r"clips\VALORANT\c (GIF).rv-export.gif",
                r"clips\a (trim).rv-export.mp4",
                r"clips\n.rv-centering.mp4"
            ]
        );
        for kept in [r"a.centering.mp4", r"VALORANT\c.centering.mp4", r"b.centering.mp4", r"young.centering.mp4", r"x.mp4.part", r"VALORANT\deep\d.centering.mp4", r"a.mp4", r"VALORANT\c.mp4", "x.mp4", "s.centering.mp4", "s.mp4", "n.mp4", "Empty game", r"VALORANT\c.rv-export.mkv", r"VALORANT\deep\d (trim).rv-export.mp4", r"VALORANT\young (trim).rv-export.mp4"] {
            assert!(clips_dir.join(kept).exists(), "{kept}");
        }
        for kept in [r"VALORANT\gone.mp4.part", r"VALORANT\c.txt.part", r"VALORANT\deep\d.mp4.part", r"VALORANT\young.mp4.part", "held.rv-export.mp4", r"VALORANT\h.mp4.part"] {
            assert!(archive.join(kept).exists(), "{kept}");
        }
        // Told to stop: nothing more.
        file(&settings.join("notes.json.tmp"));
        age(&settings.join("notes.json.tmp"), HOUR);
        assert!(settings_in(&f, (true, true), SystemTime::now(), &|| true, &recycle).0.is_empty());
        drop(writing);
        drop(copying);
        fs::remove_dir_all(&root).unwrap();
    }

    /// The clips folder's rules run only while no pass runs and the folder isn't the user's own;
    /// the archive's also only when its own folder isn't, and never without the clips folder's.
    #[test]
    fn folder_gates() {
        let risk = |f: &str| f.ends_with("Videos").then(|| "your Videos folder".to_string());
        let (clips, videos) = (r"D:\Clips", r"C:\Users\X\Videos");
        let (archive, archive_videos) = (Path::new(r"E:\Old"), Path::new(videos));
        assert_eq!(rules_on(true, clips, Some(archive), &risk), (true, true));
        assert_eq!(rules_on(true, clips, None, &risk), (true, false), "no archive");
        assert_eq!(rules_on(true, clips, Some(archive_videos), &risk), (true, false), "the archive is the user's own");
        assert_eq!(rules_on(true, videos, Some(archive), &risk), (false, false), "the clips folder is the user's own");
        assert_eq!(rules_on(false, clips, Some(archive), &risk), (false, false), "a pass is running");
        // With the folder confirmed, `folder_risk` says nothing against it, so its rules run.
        let confirmed = |f: &str| if f == videos { None } else { risk(f) };
        assert_eq!(rules_on(true, clips, Some(archive_videos), &confirmed), (true, true));
    }

    /// Every file in the settings folder saved through `clips::write_json` is in STORE_FILES, and
    /// nothing else is: read from the source, so a new store can't be missed.
    #[test]
    fn every_store_is_listed() {
        fn sources(dir: &Path, out: &mut Vec<String>) {
            for e in fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    sources(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(fs::read_to_string(&p).unwrap());
                }
            }
        }
        let mut texts = Vec::new();
        sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut texts);
        let mut found = std::collections::BTreeSet::new();
        for text in &texts {
            let code = &text[..text.find("mod tests {").unwrap_or(text.len())];
            if !["write_json(", "save_json(", "edit_store("].iter().any(|c| code.contains(c)) {
                continue;
            }
            // The settings folder's, not a test's file in the temp folder.
            let at = "dir().join(\"";
            found.extend(code.match_indices(at).filter(|(i, _)| !code[..*i].ends_with("temp_")).filter_map(|(i, _)| code[i + at.len()..].split('"').next()).filter(|n| n.ends_with(".json")).map(String::from));
        }
        assert_eq!(found, clips::STORE_FILES.iter().map(|s| s.to_string()).collect::<std::collections::BTreeSet<_>>());
    }
}
