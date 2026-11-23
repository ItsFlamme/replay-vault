//! One save, from the press to a saved clip or a failure, as a small state model with no I/O.
//! The recorder's worker (`obs::run`) feeds it what it saw (the time, whether this save's clip
//! landed, the replay buffer's run, the recorder's files on disk) and acts on the step it returns
//! (adopt a clip whose report was missed, tell the user it failed). Popups and the sound only
//! follow its steps. The app never asks OBS to save a moment again by itself: only the user does,
//! with Save again or the key.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// No file from the save this long after the press: the save failed (a clip that lands later is
/// still saved, and says so).
pub const NO_FILE_FOR: Duration = Duration::from_secs(10);
/// A save still being written this long after the press is no longer followed (it's logged).
pub const WRITING_CAP: Duration = Duration::from_secs(120);
/// The writer let go of the file this long ago and OBS hasn't reported it saved: the report was
/// missed, or the write failed. Long enough for the report, which comes as the writer finishes.
pub const ADOPT_AFTER: Duration = Duration::from_secs(3);
/// A write slower than this (press or file seen to the saved report) gets the slow-drive hint.
pub const SLOW_WRITE: Duration = Duration::from_secs(6);

/// Where a save is.
#[derive(Clone, Debug, PartialEq)]
pub enum Stage {
    /// Asked for; no file yet.
    Pressed,
    /// A file from this save is being written (held by its writer, or still growing).
    Writing,
    /// OBS reported the clip saved (or a found one played).
    Saved,
    /// Not saved. The moment stays in OBS's replay until `saveable_until`.
    Failed { reason: Failure, saveable_until: Instant },
}

/// Why a save failed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Failure {
    /// No file showed up.
    NoFile,
    /// The recorder wrote a file that doesn't play.
    Unfinished,
    /// The file the recorder was writing went away, with no report.
    Gone,
    /// The file was still being written at `WRITING_CAP`.
    NeverFinished,
}

/// Which run of OBS's replay buffer this is. A run ends when what the buffer holds is (or may have
/// been) thrown away: it stopped, the recorder went away or restarted, or the app started it while
/// it was thought to be on. A moment pressed in one run is not in a later one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Buffer {
    pub run: u32,
    on: bool,
    /// The last run ended with the recorder out of reach (gone, restarting, not answering), so
    /// it isn't known for sure that the buffer was emptied.
    pub lost: bool,
}

impl Buffer {
    pub const fn new() -> Self {
        Buffer { run: 0, on: false, lost: false }
    }

    /// The buffer was seen on or off (the health check, OBS's own events), with the recorder
    /// `reached` or not. Going off ends the run; coming on doesn't (nothing was thrown away then,
    /// and a stop before it was seen already).
    pub fn seen(&mut self, on: bool, reached: bool) {
        if self.on && !on {
            self.end(!reached);
        }
        self.on = on;
    }

    /// The app turned the buffer on or off and OBS did it: a stop ends the run, and so does a start
    /// while it was thought to be on (it had stopped unseen).
    pub fn set(&mut self, on: bool) {
        if !on || self.on {
            self.end(false);
        }
        self.on = on;
    }

    fn end(&mut self, lost: bool) {
        (self.run, self.lost) = (self.run.wrapping_add(1), lost);
    }
}

/// The recorder's files when a save was asked for, by file name (OBS reports a clip's path in its
/// own spelling). `never`: none of them is that save's (named by the date alone, as a full
/// recording or another writer would, or a file the save before saw as its own). `finished`:
/// replay files finished then, with their size. One of those can be this save's clip (OBS saves
/// on its key before the app sees the press, and the app may look seconds later), but only OBS
/// reporting it says so: the save flow never takes one for its own unless it grows (it was being
/// written), as it may as well be an old file left in the folder.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Before {
    never: Vec<String>,
    finished: Vec<(String, u64)>,
}

impl Before {
    /// What's there now, from the recorder's `files` now: a replay file still being written is
    /// not there at all (it can be this save's own); and every file `earlier` (the save before,
    /// if the app still has it) saw as its own is never this one's.
    pub fn at_press(files: &[Seen], earlier: Option<&Save>) -> Self {
        let mut before = Before::default();
        for f in files {
            match (f.replay, f.held) {
                (false, _) => before.put(file_name(&f.path)),
                (true, false) => before.finished.push((file_name(&f.path), f.len)),
                (true, true) => {}
            }
        }
        if let Some(earlier) = earlier {
            before.merge(&earlier.own);
        }
        before
    }

    /// Whether `file` was there: never this save's, or finished then and the same size now.
    pub fn has(&self, file: &Seen) -> bool {
        let name = file_name(&file.path);
        self.never.contains(&name) || self.finished.iter().any(|(n, l)| *n == name && *l == file.len)
    }

    /// Whether the file at `path` is never this save's.
    pub fn never_has(&self, path: &Path) -> bool {
        self.never.contains(&file_name(path))
    }

    /// Adds `files` as never this save's (whatever their size later).
    fn add(&mut self, files: &[Seen]) {
        for f in files {
            self.put(file_name(&f.path));
        }
    }

    fn merge(&mut self, other: &Before) {
        for name in other.never.iter().chain(other.finished.iter().map(|(n, _)| n)) {
            self.put(name.clone());
        }
    }

    fn put(&mut self, name: String) {
        if !self.never.contains(&name) {
            self.never.push(name);
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// How many recent landings `Landings` keeps.
const LANDINGS_KEPT: usize = 16;

/// The clips that landed lately, for a save the worker begins following only after its clip
/// landed (it saw the save key's press late): when, which file, whether it was surely another
/// save's or given to one, and whether it plays.
#[derive(Debug, Default)]
pub struct Landings(Vec<Landing>);

#[derive(Debug)]
struct Landing {
    at: Instant,
    path: PathBuf,
    /// Its file was there before the save waiting then was asked for.
    older: bool,
    /// Given to a save: the one waiting when it landed, or one that began after.
    given: bool,
    /// Reported saved, or found and checked to play (`checked`).
    plays: bool,
}

impl Landings {
    pub const fn new() -> Self {
        Landings(Vec::new())
    }

    /// A clip landed `at`, `who`'s (`landed`; the waiting save's is given to it): reported, or
    /// `found` (it plays once `checked` says so).
    pub fn push(&mut self, at: Instant, path: &Path, who: Landed, found: bool) {
        if self.0.len() >= LANDINGS_KEPT {
            self.0.remove(0);
        }
        self.0.push(Landing { at, path: path.to_path_buf(), older: who == Landed::Older, given: who == Landed::Ours, plays: !found });
    }

    /// The found clip at `path` was checked: it plays or not.
    pub fn checked(&mut self, path: &Path, plays: bool) {
        for l in self.0.iter_mut().filter(|l| l.path == path) {
            l.plays = plays;
        }
    }

    /// The clip of a save pressed `press` that landed before the save was followed: the first
    /// that landed at or after the press, plays, isn't surely an earlier save's (older, or one of
    /// the files `known` to be the save before's own) and wasn't given to a save yet. Given to it now.
    pub fn take_since(&mut self, press: Instant, known: &Before) -> Option<PathBuf> {
        let l = self.0.iter_mut().find(|l| l.at >= press && l.plays && !l.older && !l.given && !known.never_has(&l.path))?;
        l.given = true;
        Some(l.path.clone())
    }
}

/// Whose a clip that just landed at `path` is, given the files there when the save still waiting
/// for its clip was asked for (`open`, None if no save is waiting).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Landed {
    /// The waiting save's.
    Ours,
    /// Its file is never the waiting save's: an older save's, landing late.
    Older,
    /// No save is waiting (OBS saved without the app seeing a press, or before it did).
    Unknown,
}

/// A report that names a file finished at the press is the waiting save's: OBS reports each save
/// once, as it finishes, and an old file isn't reported again.
pub fn landed(open: Option<&Before>, path: &Path) -> Landed {
    match open {
        Some(before) if before.never_has(path) => Landed::Older,
        Some(_) => Landed::Ours,
        None => Landed::Unknown,
    }
}

/// A file the recorder wrote, as the worker saw it: its size, whether a writer still has it open,
/// and whether it's named the way the recorder names a replay ("Replay <date>"), not by the date
/// alone.
#[derive(Clone, Debug, PartialEq)]
pub struct Seen {
    pub path: PathBuf,
    pub len: u64,
    pub held: bool,
    pub replay: bool,
}

/// What the worker saw at one look.
pub struct Look<'a> {
    pub now: Instant,
    /// This save's clip landed (OBS reported it, or a found one played). Read after the files, so
    /// a clip moved into place in between has landed already.
    pub landed: bool,
    /// This save's found files that turned out not to play, so far.
    pub unfinished: u32,
    /// The replay buffer now (its run, and how the last one ended).
    pub buffer: Buffer,
    /// The recorder's replay files that `Save::belongs` (none looked for once it's over).
    pub files: &'a [Seen],
}

/// What the worker does after a look.
#[derive(Debug, PartialEq)]
pub enum Step {
    Nothing,
    /// A file from this save showed up for the first time.
    FileSeen,
    /// The writer let go of these and OBS didn't report them: check that they play, then they're
    /// saved. The worker says what came of it with `adopted`.
    Adopt(Vec<PathBuf>),
    /// The clip was saved.
    Saved,
    /// The save failed (`Stage::Failed` says why and until when it can be saved again).
    Failed,
    /// Still being written at the cap: no longer followed, and failed (`Failure::NeverFinished`).
    GaveUp,
    /// The replay buffer started over after this failed save: the moment is gone, so the offer to
    /// save it again goes. `unsure`: the run ended with the recorder out of reach, so the moment
    /// may be gone rather than surely.
    Withdrawn { unsure: bool },
}

/// One save on its way.
pub struct Save {
    /// When it was asked for (the key noticed, the button, the tray).
    pub at: Instant,
    pub stage: Stage,
    /// When a file from this save first showed up.
    pub file_seen: Option<Instant>,
    unfinished_before: u32,
    /// How much OBS keeps (the clip length): the moment stays that long after the press.
    buffer: Duration,
    /// The replay buffer's run at the press, when a look first saw a later one, and whether that
    /// run ended with the recorder out of reach.
    run: u32,
    gone_at: Option<Instant>,
    gone_unsure: bool,
    /// The recorder's files there at the press, and the files it saw as its own.
    before: Before,
    own: Before,
    adopted: bool,
    /// The files at the last look, and when one was last new, growing or held.
    last: Vec<Seen>,
    busy_at: Instant,
}

impl Save {
    pub fn new(at: Instant, run: u32, before: Before, buffer: Duration) -> Self {
        Save { at, stage: Stage::Pressed, file_seen: None, unfinished_before: 0, buffer, run, gone_at: None, gone_unsure: false, before, own: Before::default(), adopted: false, last: Vec::new(), busy_at: at }
    }

    /// Still on its way: files are looked for.
    pub fn is_active(&self) -> bool {
        matches!(self.stage, Stage::Pressed | Stage::Writing)
    }

    /// Whether the recorder's replay file `file` can be this save's: it wasn't there when the save
    /// was asked for (`Before::has`). By what was there, not by file times, which a drive with
    /// another clock (a NAS) gets wrong.
    pub fn belongs(&self, file: &Seen) -> bool {
        !self.before.has(file)
    }

    /// The files this save saw as its own.
    pub fn own(&self) -> &Before {
        &self.own
    }

    /// Until when the pressed moment is still in OBS's replay: the clip length after the press,
    /// unless the buffer started over before that.
    pub fn saveable_until(&self) -> Instant {
        let end = self.at + self.buffer;
        self.gone_at.map_or(end, |gone| gone.min(end))
    }

    /// The buffer's run ended since the press with the recorder out of reach: the moment may no
    /// longer be in the replay, not surely.
    pub fn gone_unsure(&self) -> bool {
        self.gone_unsure
    }

    /// The next step, from one look.
    pub fn step(&mut self, look: &Look) -> Step {
        let now = look.now;
        // A clip that landed is a real file, whatever happened to the buffer since.
        if look.landed && self.stage != Stage::Saved {
            self.stage = Stage::Saved;
            return Step::Saved;
        }
        if look.buffer.run != self.run && self.gone_at.is_none() {
            (self.gone_at, self.gone_unsure) = (Some(now), look.buffer.lost);
            if let Stage::Failed { reason, saveable_until } = self.stage
                && now < saveable_until
            {
                self.stage = Stage::Failed { reason, saveable_until: now };
                return Step::Withdrawn { unsure: look.buffer.lost };
            }
        }
        if !self.is_active() {
            return Step::Nothing;
        }
        if now.saturating_duration_since(self.at) >= WRITING_CAP {
            self.fail(Failure::NeverFinished);
            return Step::GaveUp;
        }
        if self.adopted && look.unfinished > self.unfinished_before {
            self.unfinished_before = look.unfinished;
            return self.fail(Failure::Unfinished);
        }
        let busy = look.files.iter().any(|f| f.held || !self.last.iter().any(|l| l.path == f.path && l.len == f.len));
        let first = self.file_seen.is_none() && !look.files.is_empty();
        if first {
            self.file_seen = Some(now);
        }
        self.last = look.files.to_vec();
        self.own.add(look.files);
        if busy {
            self.busy_at = now;
            self.stage = Stage::Writing;
            return if first { Step::FileSeen } else { Step::Nothing };
        }
        if self.file_seen.is_none() {
            return if now.saturating_duration_since(self.at) >= NO_FILE_FOR { self.fail(Failure::NoFile) } else { Step::Nothing };
        }
        // The writer let go (or the file went away) and OBS hasn't reported it.
        if now.saturating_duration_since(self.busy_at) < ADOPT_AFTER {
            return Step::Nothing;
        }
        if look.files.is_empty() {
            return self.fail(Failure::Gone);
        }
        if self.adopted {
            return Step::Nothing;
        }
        Step::Adopt(look.files.iter().map(|f| f.path.clone()).collect())
    }

    /// What came of an `Adopt`: whether a file was handed on to be checked. One that wasn't (its
    /// writer had it open again) is followed as being written, and adopted when it's let go.
    pub fn adopted(&mut self, taken: bool, now: Instant) {
        if taken {
            self.adopted = true;
        } else {
            (self.stage, self.busy_at) = (Stage::Writing, now);
        }
    }

    /// The save failed.
    pub fn fail(&mut self, why: Failure) -> Step {
        self.stage = Stage::Failed { reason: why, saveable_until: self.saveable_until() };
        Step::Failed
    }
}

/// Whether a new press starts a new save: not while one is on its way (that one is followed, and
/// OBS saves the moment once).
pub fn starts_new(current: Option<&Save>) -> bool {
    !current.is_some_and(Save::is_active)
}

/// How long the moment stays saveable, in words: "about 2:40" (rounded down to 10 s), "under a
/// minute", or None once it's gone.
pub fn left_words(left: Duration) -> Option<String> {
    let secs = left.as_secs();
    match secs {
        _ if left.is_zero() => None,
        0..60 => Some("under a minute".into()),
        _ => {
            let s = secs / 10 * 10;
            Some(format!("about {}:{:02}", s / 60, s % 60))
        }
    }
}

/// The "Clip not saved" popup's detail: press the key again and how long is left, or that it's
/// gone from the replay (`unsure`: may be, the recorder was out of reach); after a file that was
/// never finished, that first.
pub fn not_saved_detail(key: &str, left: Duration, why: Failure, unsure: bool) -> String {
    let key = if key.is_empty() { "the save key" } else { key };
    let what = match left_words(left) {
        Some(w) => format!("Press {key} again, {w} left"),
        None if unsure => "It may no longer be in the replay".into(),
        None => "It's no longer in the replay".into(),
    };
    match why {
        Failure::NeverFinished => format!("The recorder never finished writing it. {what}"),
        Failure::NoFile | Failure::Unfinished | Failure::Gone => what,
    }
}

/// The status line's start for a failed save.
pub fn reason_text(why: Failure) -> &'static str {
    match why {
        Failure::NoFile => "The clip wasn't saved.",
        Failure::Unfinished => "The clip wasn't saved: the file the recorder wrote doesn't play (it's kept as it is).",
        Failure::Gone => "The clip wasn't saved: the file the recorder was writing went away.",
        Failure::NeverFinished => "The clip wasn't saved: the recorder was still writing it 2 minutes after the press and never finished.",
    }
}

/// How long a save's write took, when that's slow enough for the hint: from its file showing up
/// (else the press) to the saved report.
pub fn slow_write(at: Instant, file_seen: Option<Instant>, saved: Instant) -> Option<Duration> {
    Some(saved.saturating_duration_since(file_seen.unwrap_or(at))).filter(|d| *d > SLOW_WRITE)
}

/// The timing log line for a saved clip: from the press when the app knows of one (with when its
/// file showed up), else from the saved report.
pub fn timing_line(press: Option<(Instant, Option<Instant>)>, event: Instant, moved: Instant, popup: Instant) -> String {
    let s = |from: Instant, to: Instant| format!("{:.1} s", to.saturating_duration_since(from).as_secs_f32());
    match press {
        Some((at, seen)) => {
            let seen = seen.map(|t| format!("file seen {}, ", s(at, t))).unwrap_or_default();
            format!("Save timing: {seen}saved event {}, moved {}, popup {} after the press", s(at, event), s(at, moved), s(at, popup))
        }
        None => format!("Save timing: moved {}, popup {} after the saved event (no press known)", s(event, moved), s(event, popup)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUFFER: Duration = Duration::from_secs(180);

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn file(len: u64, held: bool) -> Seen {
        Seen { path: PathBuf::from(r"C:\clips\Replay 11-15-2026 12-05-20 AM.mp4"), len, held, replay: true }
    }

    fn named(path: &str, len: u64, held: bool) -> Seen {
        Seen { path: PathBuf::from(path), len, held, replay: path.contains("Replay ") }
    }

    /// One look `t` seconds after the press, in the buffer run it was pressed in.
    fn look(save: &mut Save, t: u64, saved: u32, unfinished: u32, files: &[Seen]) -> Step {
        look_in(save, 0, t, saved > 5, unfinished, files)
    }

    /// One look `t` seconds after the press in buffer run `run`, with this save's clip `landed` or not.
    fn look_in(save: &mut Save, run: u32, t: u64, landed: bool, unfinished: u32, files: &[Seen]) -> Step {
        save.step(&Look { now: save.at + secs(t), landed, unfinished, buffer: Buffer { run, on: true, lost: false }, files })
    }

    fn save() -> Save {
        Save::new(Instant::now(), 0, Before::default(), BUFFER)
    }

    /// The save flow's view: the replay files that can be `s`'s.
    fn its(s: &Save, all: &[Seen]) -> Vec<Seen> {
        all.iter().filter(|f| f.replay && s.belongs(f)).cloned().collect()
    }

    /// A slow disk: the file shows up and is written for 12 s, with no deadline, then OBS reports it.
    #[test]
    fn pressed_writing_saved() {
        let mut s = save();
        assert_eq!(look(&mut s, 1, 5, 0, &[]), Step::Nothing);
        assert_eq!(s.stage, Stage::Pressed);
        assert_eq!(look(&mut s, 2, 5, 0, &[file(10, true)]), Step::FileSeen);
        assert_eq!((s.stage.clone(), s.file_seen), (Stage::Writing, Some(s.at + secs(2))));
        // Past 10 s while it's written: no failure.
        for t in [4, 8, 10, 12] {
            assert_eq!(look(&mut s, t, 5, 0, &[file(t * 100, true)]), Step::Nothing, "{t}");
        }
        assert_eq!(look(&mut s, 13, 6, 0, &[]), Step::Saved);
        assert_eq!(s.stage, Stage::Saved);
        assert_eq!(look(&mut s, 14, 6, 0, &[]), Step::Nothing);
    }

    /// No file for 10 s: failed (never asked again by the app), saveable for the clip length after
    /// the press. A clip that lands late is still saved.
    #[test]
    fn no_file_fails() {
        let mut s = save();
        assert_eq!(look(&mut s, 9, 5, 0, &[]), Step::Nothing);
        assert_eq!(look(&mut s, 10, 5, 0, &[]), Step::Failed);
        assert_eq!(s.stage, Stage::Failed { reason: Failure::NoFile, saveable_until: s.at + BUFFER });
        assert!(!s.is_active());
        assert_eq!(look(&mut s, 12, 5, 0, &[]), Step::Nothing);
        assert_eq!(look(&mut s, 30, 6, 0, &[]), Step::Saved);
        assert_eq!(s.stage, Stage::Saved);
    }

    /// The writer stops and OBS says nothing: after a moment the file is adopted (checked); one
    /// that doesn't play is a failure. One that went away is a failure that says so.
    #[test]
    fn writing_stops_without_a_playable_file() {
        let mut s = save();
        assert_eq!(look(&mut s, 2, 5, 0, &[file(10, true)]), Step::FileSeen);
        assert_eq!(look(&mut s, 4, 5, 0, &[file(20, false)]), Step::Nothing, "grew: still writing");
        assert_eq!(look(&mut s, 6, 5, 0, &[file(20, false)]), Step::Nothing, "the report may still come");
        assert_eq!(look(&mut s, 7, 5, 0, &[file(20, false)]), Step::Adopt(vec![file(0, false).path]));
        s.adopted(true, s.at + secs(7));
        assert_eq!(look(&mut s, 9, 5, 0, &[file(20, false)]), Step::Nothing, "adopted once");
        assert_eq!(look(&mut s, 10, 5, 1, &[file(20, false)]), Step::Failed);
        assert_eq!(s.stage, Stage::Failed { reason: Failure::Unfinished, saveable_until: s.at + BUFFER });

        // The file went away with no report.
        let mut s = save();
        assert_eq!(look(&mut s, 2, 5, 0, &[file(10, true)]), Step::FileSeen);
        assert_eq!(look(&mut s, 4, 5, 0, &[]), Step::Nothing);
        assert_eq!(look(&mut s, 5, 5, 0, &[]), Step::Failed);
        assert!(matches!(s.stage, Stage::Failed { reason: Failure::Gone, .. }));
        assert_eq!(reason_text(Failure::Gone), "The clip wasn't saved: the file the recorder was writing went away.");
    }

    /// A file still written 2 minutes after the press is no longer followed: the save failed, and
    /// says so (with the time left, here a minute). A clip that lands after all still counts.
    #[test]
    fn writing_past_the_cap_gives_up() {
        let mut s = save();
        assert_eq!(look(&mut s, 2, 5, 0, &[file(1, true)]), Step::FileSeen);
        assert_eq!(look(&mut s, 119, 5, 0, &[file(2, true)]), Step::Nothing);
        assert_eq!(look(&mut s, 120, 5, 0, &[file(3, true)]), Step::GaveUp);
        assert!(!s.is_active());
        assert_eq!(s.stage, Stage::Failed { reason: Failure::NeverFinished, saveable_until: s.at + BUFFER });
        assert_eq!(not_saved_detail("Alt+F10", secs(60), Failure::NeverFinished, false), "The recorder never finished writing it. Press Alt+F10 again, about 1:00 left");
        assert_eq!(look(&mut s, 125, 6, 0, &[]), Step::Saved);
    }

    /// A file found let go that couldn't be handed on (its writer opened it again) isn't taken as
    /// adopted: it's followed as being written and adopted when it's let go, so the save still
    /// ends in Saved or a failure the user sees.
    #[test]
    fn adoption_that_did_not_happen() {
        let mut s = save();
        assert_eq!(look(&mut s, 2, 5, 0, &[file(10, false)]), Step::FileSeen);
        assert_eq!(look(&mut s, 5, 5, 0, &[file(10, false)]), Step::Adopt(vec![file(0, false).path]));
        s.adopted(false, s.at + secs(5));
        assert_eq!(s.stage, Stage::Writing);
        assert_eq!(look(&mut s, 6, 5, 0, &[file(10, true)]), Step::Nothing, "held again");
        assert_eq!(look(&mut s, 8, 5, 0, &[file(10, false)]), Step::Nothing);
        assert_eq!(look(&mut s, 9, 5, 0, &[file(10, false)]), Step::Adopt(vec![file(0, false).path]), "tried again");
        s.adopted(true, s.at + secs(9));
        assert_eq!(look(&mut s, 10, 5, 0, &[file(10, false)]), Step::Nothing);
        assert_eq!(look(&mut s, 11, 6, 0, &[]), Step::Saved);
    }

    /// The buffer starting over after the press (stopped, the recorder restarted or gone): a
    /// failure says the moment is gone (or may be, with the recorder out of reach) with nothing
    /// offered, at every stage. A clip that lands anyway is a real file and saved.
    #[test]
    fn buffer_started_over() {
        // No file, buffer gone: failed at 10 s, nothing left to offer.
        let mut s = save();
        assert_eq!(look_in(&mut s, 1, 3, false, 0, &[]), Step::Nothing);
        assert_eq!(look_in(&mut s, 1, 10, false, 0, &[]), Step::Failed);
        assert_eq!(s.stage, Stage::Failed { reason: Failure::NoFile, saveable_until: s.at + secs(3) });
        assert!(!s.gone_unsure());
        assert_eq!(not_saved_detail("Alt+F10", Duration::ZERO, Failure::NoFile, false), "It's no longer in the replay");
        // The run ended with the recorder out of reach: not said as a certainty.
        let mut s = save();
        assert_eq!(s.step(&Look { now: s.at + secs(3), landed: false, unfinished: 0, buffer: Buffer { run: 1, on: false, lost: true }, files: &[] }), Step::Nothing);
        assert_eq!(look_in(&mut s, 1, 10, false, 0, &[]), Step::Failed);
        assert!(s.gone_unsure());
        assert_eq!(not_saved_detail("Alt+F10", Duration::ZERO, Failure::NoFile, true), "It may no longer be in the replay");

        // While its file is written: still followed (the save was asked for in time), and saved.
        let mut s = save();
        assert_eq!(look_in(&mut s, 0, 2, false, 0, &[file(10, true)]), Step::FileSeen);
        assert_eq!(look_in(&mut s, 7, 4, false, 0, &[file(20, true)]), Step::Nothing);
        assert_eq!(look_in(&mut s, 7, 6, true, 0, &[]), Step::Saved);
        // ... or, if it doesn't play, failed.
        let mut s = save();
        assert_eq!(look_in(&mut s, 0, 2, false, 0, &[file(10, true)]), Step::FileSeen);
        assert_eq!(look_in(&mut s, 1, 6, false, 0, &[file(10, false)]), Step::Adopt(vec![file(0, false).path]));
        s.adopted(true, s.at + secs(6));
        assert_eq!(look_in(&mut s, 1, 7, false, 1, &[file(10, false)]), Step::Failed);
        assert_eq!(s.stage, Stage::Failed { reason: Failure::Unfinished, saveable_until: s.at + secs(6) });

        // Failed with time left, then the buffer starts over: the offer is withdrawn, once.
        let mut s = save();
        assert_eq!(look_in(&mut s, 0, 10, false, 0, &[]), Step::Failed);
        assert_eq!(look_in(&mut s, 0, 30, false, 0, &[]), Step::Nothing);
        assert_eq!(look_in(&mut s, 2, 40, false, 0, &[]), Step::Withdrawn { unsure: false });
        assert_eq!(s.stage, Stage::Failed { reason: Failure::NoFile, saveable_until: s.at + secs(40) });
        assert_eq!(look_in(&mut s, 3, 41, false, 0, &[]), Step::Nothing);
        // A clip that lands after all is still saved.
        assert_eq!(look_in(&mut s, 3, 50, true, 0, &[]), Step::Saved);
        // Gone from the replay by time already: nothing to withdraw.
        let mut s = Save::new(Instant::now(), 0, Before::default(), secs(15));
        assert_eq!(look_in(&mut s, 0, 10, false, 0, &[]), Step::Failed);
        assert_eq!(look_in(&mut s, 1, 30, false, 0, &[]), Step::Nothing);
    }

    /// A run ends when the buffer goes off (seen or done by the app) and when the app starts one
    /// it thought was on; it coming on as expected, or being seen as it was, doesn't end it.
    #[test]
    fn buffer_runs() {
        let mut b = Buffer::new();
        b.seen(false, true);
        assert_eq!(b.run, 0, "off at first");
        b.seen(true, true);
        b.seen(true, true);
        assert_eq!(b.run, 0, "came on: nothing thrown away");
        b.seen(false, true);
        assert_eq!(b.run, 1, "stopped, or the recorder went away");
        b.seen(false, true);
        b.set(true);
        assert_eq!(b.run, 1, "turned on again after the stop was seen");
        b.set(true);
        assert_eq!(b.run, 2, "OBS started it while it was thought on: it had stopped unseen");
        b.set(false);
        assert_eq!(b.run, 3);
        b.set(false);
        assert_eq!(b.run, 4, "a stop always ends the run");
        assert!(!b.lost);
        b.seen(true, true);
        b.seen(false, false);
        assert_eq!((b.run, b.lost), (5, true), "the recorder out of reach: not known for sure");
        b.set(true);
        b.set(false);
        assert!(!b.lost);
    }

    /// A save's files are the recorder's replay files that weren't there, finished, at the press
    /// (whatever their times say). A clip OBS reports is the waiting save's unless its file is
    /// never that save's.
    #[test]
    fn files_and_clips_by_what_was_there() {
        let old = PathBuf::from(r"D:\Clips\Replay 11-15-2026 12-05-20 AM.mp4");
        let new = PathBuf::from(r"D:\Clips\Replay 11-15-2026 12-06-00 AM.mp4");
        let seen = |path: &PathBuf, len, held| Seen { path: path.clone(), len, held, replay: true };
        let before = Before::at_press(&[seen(&old, 50, false)], None);
        let s = Save::new(Instant::now(), 0, before.clone(), BUFFER);
        assert!(!s.belongs(&seen(&old, 50, false)) && s.belongs(&seen(&new, 1, true)));
        assert!(s.belongs(&seen(&old, 60, false)), "grew since: being written");
        // OBS reports paths in its own spelling.
        assert!(!s.belongs(&seen(&PathBuf::from("D:/Clips/replay 11-15-2026 12-05-20 am.MP4"), 50, false)));
        assert_eq!(landed(Some(&before), &new), Landed::Ours);
        assert_eq!(landed(None, &new), Landed::Unknown);
    }

    /// Ordering C: the press's file is finished when the save is begun but OBS's report of it
    /// isn't handled yet. The flow doesn't take it by itself, the report is this save's: one clip,
    /// one Clip saved.
    #[test]
    fn finished_but_unreported_at_the_press() {
        let f = file(400, false);
        let before = Before::at_press(std::slice::from_ref(&f), None);
        assert_eq!(landed(Some(&before), &f.path), Landed::Ours);
        let mut s = Save::new(Instant::now(), 0, before, BUFFER);
        assert!(its(&s, std::slice::from_ref(&f)).is_empty());
        assert_eq!(look(&mut s, 1, 5, 0, &[]), Step::Nothing);
        assert_eq!(look(&mut s, 2, 6, 0, &[]), Step::Saved, "the report lands");
    }

    /// An old replay file left in the folder (from before the app started, or handled long ago and
    /// never moved) is never announced as a press's clip by the save flow, on a slow drive or not:
    /// the save fails at 10 s unless OBS reports a clip. If it grows, it's being written for the
    /// save after all.
    #[test]
    fn old_stray_file_is_never_this_press() {
        let stray = named(r"C:\clips\Replay 11-01-2026 09-00-00 PM.mp4", 5_000, false);
        // (a) Pressed before the listener connected, (b) long since handled: the same snapshot.
        for _case in ["before the listener connected", "aged out of the handled list"] {
            let mut s = Save::new(Instant::now(), 0, Before::at_press(std::slice::from_ref(&stray), None), BUFFER);
            for t in 1..10 {
                let files = its(&s, std::slice::from_ref(&stray));
                assert_eq!(look(&mut s, t, 5, 0, &files), Step::Nothing, "{t}");
            }
            let files = its(&s, std::slice::from_ref(&stray));
            assert_eq!(look(&mut s, 10, 5, 0, &files), Step::Failed);
            assert!(matches!(s.stage, Stage::Failed { reason: Failure::NoFile, .. }));
        }
        let s = Save::new(Instant::now(), 0, Before::at_press(std::slice::from_ref(&stray), None), BUFFER);
        assert_eq!(its(&s, &[named(r"C:\clips\Replay 11-01-2026 09-00-00 PM.mp4", 6_000, true)]).len(), 1, "grew: written for this save");
    }

    /// OBS acts on its save key before the app sees the press, and the worker may take the
    /// snapshot seconds later: a replay file still being written then is not there at all, so it's
    /// this save's.
    #[test]
    fn own_file_written_before_the_snapshot() {
        let f = file(0, false).path;
        let before = Before::at_press(&[file(400, true)], None);
        let mut s = Save::new(Instant::now(), 0, before.clone(), BUFFER);
        assert_eq!(landed(Some(&before), &f), Landed::Ours);
        assert_eq!(look(&mut s, 1, 5, 0, &[file(600, true)]), Step::FileSeen);
        for t in [3, 6, 10, 12] {
            assert_eq!(look(&mut s, t, 5, 0, &[file(600 + t, true)]), Step::Nothing, "{t}");
        }
        assert_eq!(look(&mut s, 13, 6, 0, &[]), Step::Saved);
    }

    /// Only a file named as a replay can be this save's. One the recorder names by the date alone
    /// (a full recording, another writer) is never this save's, held or not, whatever its size;
    /// the listener never waits on one either.
    #[test]
    fn only_replay_names_can_be_written_for_the_save() {
        let other = named(r"C:\clips\2026-11-06 18-42-33.mp4", 100, true);
        let replay = file(100, true);
        let before = Before::at_press(&[other.clone(), replay.clone()], None);
        assert_eq!(landed(Some(&before), &other.path), Landed::Older);
        assert_eq!(landed(Some(&before), &replay.path), Landed::Ours);
        assert!(before.has(&Seen { len: 900, held: false, ..other.clone() }), "grown, still known");
        assert!(!before.has(&replay), "a replay being written is waited on");
    }

    /// A save still writing its file at a new press (it failed at the cap, or before) keeps it:
    /// its own files go into the new save's snapshot, held or not, so its late clip never ends the
    /// new save (OBS writes the new one after it).
    #[test]
    fn earlier_saves_file_stays_its_own() {
        let mut first = save();
        assert_eq!(look(&mut first, 2, 5, 0, &[file(10, true)]), Step::FileSeen);
        assert_eq!(look(&mut first, 120, 5, 0, &[file(90, true)]), Step::GaveUp);
        let next = PathBuf::from(r"C:\clips\Replay 11-15-2026 12-07-20 AM.mp4");
        let before = Before::at_press(&[file(95, true)], Some(&first));
        assert_eq!(landed(Some(&before), &file(0, false).path), Landed::Older);
        assert_eq!(landed(Some(&before), &next), Landed::Ours);
        assert!(first.own().never_has(&file(0, false).path));
    }

    /// A clip that landed between the key press and the worker beginning to follow it is that
    /// press's (once), unless it landed before the press, was surely an older save's, was given
    /// to the save waiting when it landed, was a file the save before saw as its own, or is a
    /// found one not checked to play.
    #[test]
    fn clip_landed_before_the_save_was_followed() {
        let press = Instant::now();
        let at = |ms: u64| press + Duration::from_millis(ms);
        let (a, b, c, d) = (PathBuf::from("a.mp4"), PathBuf::from("b.mp4"), PathBuf::from("c.mp4"), PathBuf::from("d.mp4"));
        let mut l = Landings::new();
        l.push(press.checked_sub(Duration::from_millis(5)).unwrap_or(press), &a, Landed::Unknown, false);
        l.push(at(300), &b, Landed::Older, false);
        l.push(at(400), &c, Landed::Unknown, false);
        l.push(at(500), &d, Landed::Unknown, false);
        let mut earlier_own = Before::default();
        earlier_own.put("c.mp4".into());
        assert_eq!(l.take_since(at(0), &earlier_own), Some(d.clone()));
        assert_eq!(l.take_since(at(0), &earlier_own), None, "given once");
        assert_eq!(l.take_since(at(0), &Before::default()), Some(c), "not the earlier save's after all");
        assert_eq!(l.take_since(at(600), &Before::default()), None, "landed before that press");
        // Given to the save waiting when it landed (press A's late clip after press B): never B's.
        let e = PathBuf::from("e.mp4");
        l.push(at(700), &e, Landed::Ours, false);
        assert_eq!(l.take_since(at(600), &Before::default()), None);
        // Found: B's only once it's checked to play.
        let f = PathBuf::from("f.mp4");
        l.push(at(800), &f, Landed::Unknown, true);
        assert_eq!(l.take_since(at(600), &Before::default()), None, "not checked yet");
        l.checked(&f, false);
        assert_eq!(l.take_since(at(600), &Before::default()), None, "doesn't play");
        l.checked(&f, true);
        assert_eq!(l.take_since(at(600), &Before::default()), Some(f));
        // Bounded: the oldest go.
        for n in 0..40 {
            l.push(at(1000 + n), &a, Landed::Unknown, false);
        }
        assert_eq!(l.0.len(), LANDINGS_KEPT);
    }

    /// Never a failure while a file from the save is being written, and none at 10 s once a file
    /// was seen; a moment that left the replay by time offers nothing.
    #[test]
    fn no_failure_while_written() {
        let mut s = save();
        assert_eq!(look(&mut s, 1, 5, 0, &[file(1, true)]), Step::FileSeen);
        for t in 10..40 {
            assert_eq!(look(&mut s, t, 5, 0, &[file(1, true)]), Step::Nothing, "{t}");
        }
        let mut s = Save::new(Instant::now(), 0, Before::default(), secs(8));
        assert_eq!(look(&mut s, 10, 0, 0, &[]), Step::Failed);
        assert_eq!(s.stage, Stage::Failed { reason: Failure::NoFile, saveable_until: s.at + secs(8) });
    }

    /// A second press while a save is on its way follows the first; after it's over, a new one starts.
    #[test]
    fn second_press_during_a_save() {
        let mut s = save();
        assert!(starts_new(None));
        assert!(!starts_new(Some(&s)));
        assert_eq!(look(&mut s, 2, 5, 0, &[file(1, true)]), Step::FileSeen);
        assert!(!starts_new(Some(&s)));
        assert_eq!(look(&mut s, 4, 6, 0, &[]), Step::Saved);
        assert!(starts_new(Some(&s)));
        let mut f = save();
        f.fail(Failure::NoFile);
        assert!(starts_new(Some(&f)), "Save again after a failure");
    }

    #[test]
    fn time_left_text() {
        assert_eq!(left_words(secs(160)).as_deref(), Some("about 2:40"));
        assert_eq!(left_words(secs(169)).as_deref(), Some("about 2:40"), "rounded down");
        assert_eq!(left_words(secs(60)).as_deref(), Some("about 1:00"));
        assert_eq!(left_words(secs(59)).as_deref(), Some("under a minute"));
        assert_eq!(left_words(Duration::from_millis(300)).as_deref(), Some("under a minute"));
        assert_eq!(left_words(Duration::ZERO), None);
        let no_file = Failure::NoFile;
        assert_eq!(not_saved_detail("Alt+F10", secs(165), no_file, false), "Press Alt+F10 again, about 2:40 left");
        assert_eq!(not_saved_detail("Alt+F10", secs(20), Failure::Unfinished, false), "Press Alt+F10 again, under a minute left");
        assert_eq!(not_saved_detail("Alt+F10", Duration::ZERO, no_file, false), "It's no longer in the replay");
        assert_eq!(not_saved_detail("", secs(90), no_file, false), "Press the save key again, about 1:30 left");
        assert_eq!(not_saved_detail("Alt+F10", Duration::ZERO, Failure::NeverFinished, false), "The recorder never finished writing it. It's no longer in the replay");
        assert_eq!(not_saved_detail("Alt+F10", Duration::ZERO, Failure::Gone, true), "It may no longer be in the replay");
    }

    #[test]
    fn slow_writes_and_timing() {
        let at = Instant::now();
        let t = |ms: u64| at + Duration::from_millis(ms);
        assert_eq!(slow_write(at, Some(t(400)), t(12_400)), Some(secs(12)));
        assert_eq!(slow_write(at, None, t(6_000)), None, "6 s is fine");
        assert_eq!(slow_write(at, None, t(7_000)), Some(secs(7)));
        assert_eq!(timing_line(Some((at, Some(t(400)))), t(11_200), t(11_300), t(11_700)), "Save timing: file seen 0.4 s, saved event 11.2 s, moved 11.3 s, popup 11.7 s after the press");
        assert_eq!(timing_line(Some((at, None)), t(1_000), t(1_100), t(1_500)), "Save timing: saved event 1.0 s, moved 1.1 s, popup 1.5 s after the press");
        assert_eq!(timing_line(None, at, t(100), t(500)), "Save timing: moved 0.1 s, popup 0.5 s after the saved event (no press known)");
    }
}
