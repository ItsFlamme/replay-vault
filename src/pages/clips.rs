//! The Clips page: the grid and its cards, picking, sharing, exports, montages and the archive.

use crate::*;

/// What the Clips page shows: the filter picked with the chips or the sidebar's game rows.
#[derive(Clone, PartialEq)]
pub(crate) enum ClipsView {
    /// Every clip.
    All,
    /// One folder's clips; None is the clips folder itself, for clips not sorted into a game.
    Game(Option<String>),
    /// Starred clips.
    Starred,
    /// Clips with this tag.
    Tag(String),
    /// Clips the archive moved to its drive or NAS.
    Archive,
}

/// Share links of clips that are gone, and deleting them (with Move to Recycle Bin, or Delete all
/// on General > Sharing).
pub(crate) struct LinkCleanup {
    /// Clips with a share link that are gone from the clips folder, as of the last read of it.
    pub(crate) orphans: Vec<String>,
    /// "Also delete their share links" in the Recycle Bin dialog.
    pub(crate) with_recycle: bool,
    /// The links being deleted, and the clips whose links wait for it to finish.
    pub(crate) job: Option<Arc<upload::Job>>,
    pub(crate) waiting: Vec<std::path::PathBuf>,
    /// Why the last deletion failed, for General > Sharing.
    pub(crate) msg: Option<String>,
}

impl Default for LinkCleanup {
    fn default() -> Self {
        Self { orphans: Vec::new(), with_recycle: true, job: None, waiting: Vec::new(), msg: None }
    }
}

/// One clip as the grid shows it.
pub(crate) struct ClipItem {
    pub(crate) path: std::path::PathBuf,
    pub(crate) name: String,
    pub(crate) bytes: u64,
    pub(crate) modified: std::time::SystemTime,
    pub(crate) is_picture: bool,
    pub(crate) favorite: bool,
    pub(crate) tags: Vec<String>,
    pub(crate) note: Option<String>,
    /// Its game's place in the tape colors (`tape_color`), if it's in the clips folder.
    pub(crate) tape: Option<usize>,
}

/// The Clips page's cards as laid out: the clips the view and search leave, in the order picked,
/// capped (the first 60 and every starred one), under their headings, and their paths in page
/// order. Made by `App::cards`, again only when something they come from changes.
#[derive(Default)]
pub(crate) struct Cards {
    pub(crate) days: Vec<(String, Vec<ClipItem>)>,
    pub(crate) shown: Vec<std::path::PathBuf>,
}

/// `Cards` as last made, and what they were made from, so they're made again only when that
/// changes: the view, search, sort or archive here, or (`stale`) the clips, stars, tags, notes or
/// the archive's clips.
#[derive(Default)]
pub(crate) struct CardsCache {
    cards: std::rc::Rc<Cards>,
    made: Option<(ClipsView, String, u8, bool, usize)>,
    stale: bool,
}

impl CardsCache {
    /// Has the cards made again when next drawn.
    pub(crate) fn invalidate(&mut self) {
        self.stale = true;
    }
}

/// Frame textures by (path, tenths of a second), None when the clip can't be decoded, with when
/// each was last used, so the thumbnails least recently drawn go once there are too many.
#[derive(Default)]
pub(crate) struct Textures {
    map: std::collections::HashMap<(std::path::PathBuf, u32), (Option<egui::TextureHandle>, u64)>,
    clock: u64,
}

/// The most thumbnails (frames at 1 s) kept; past it the least recently drawn go, down to
/// THUMBS_KEPT. About 0.3 MB of graphics memory each.
const THUMBS_MAX: usize = 200;
const THUMBS_KEPT: usize = 150;

impl Textures {
    /// The texture for `key`, marked as just used.
    pub(crate) fn get(&mut self, key: &(std::path::PathBuf, u32)) -> Option<&Option<egui::TextureHandle>> {
        self.clock += 1;
        let now = self.clock;
        self.map.get_mut(key).map(|(t, used)| {
            *used = now;
            &*t
        })
    }

    pub(crate) fn insert(&mut self, key: (std::path::PathBuf, u32), tex: Option<egui::TextureHandle>) {
        self.clock += 1;
        self.map.insert(key, (tex, self.clock));
    }

    pub(crate) fn contains_key(&self, key: &(std::path::PathBuf, u32)) -> bool {
        self.map.contains_key(key)
    }

    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }

    pub(crate) fn clear(&mut self) {
        self.map.clear();
    }

    pub(crate) fn retain(&mut self, keep: impl Fn(&(std::path::PathBuf, u32)) -> bool) {
        self.map.retain(|k, _| keep(k));
    }

    /// Past THUMBS_MAX thumbnails, drops the least recently used down to THUMBS_KEPT. Hover-scrub
    /// frames have their own limit (`App::scrub_frame`).
    pub(crate) fn trim_thumbnails(&mut self) {
        if self.map.len() <= THUMBS_MAX {
            return;
        }
        let mut used: Vec<u64> = self.map.iter().filter(|(k, _)| k.1 == 10).map(|(_, v)| v.1).collect();
        if used.len() <= THUMBS_MAX {
            return;
        }
        used.sort_unstable();
        let cut = used[used.len() - THUMBS_KEPT];
        self.map.retain(|k, v| k.1 != 10 || v.1 >= cut);
    }
}

/// A game folder (None: clips not sorted into one) and its clips.
pub(crate) struct Game {
    pub(crate) name: Option<String>,
    pub(crate) count: usize,
    /// Its newest video, for the picture until there's a cover.
    pub(crate) thumb: Option<std::path::PathBuf>,
    /// Its place in the tape colors (see `tape_slots`).
    pub(crate) tape: usize,
}

/// The game folders in `list` (newest first; None: clips not in any) with their clips counted,
/// played last first, each with its place in the tape colors.
pub(crate) fn games_in(list: &[clips::Clip]) -> Vec<Game> {
    // Each game's first clip is its newest.
    let mut games: Vec<Game> = Vec::new();
    for c in list {
        let i = games.iter().position(|g| g.name == c.group).unwrap_or_else(|| {
            games.push(Game { name: c.group.clone(), count: 0, thumb: None, tape: 0 });
            games.len() - 1
        });
        let g = &mut games[i];
        g.count += 1;
        if g.thumb.is_none() && !c.is_picture {
            g.thumb = Some(c.path.clone());
        }
    }
    tape_slots(&mut games);
    games
}

/// Hands the tape colors out by name, A to Z with Not sorted last, so a game keeps its color
/// whatever is saved, deleted or starred and however the clips are sorted, and the first four
/// games never share one. Only a game folder coming or going moves the ones after it.
pub(crate) fn tape_slots(games: &mut [Game]) {
    // Case doesn't count. Names are compared as they are, without lowercase copies; only the
    // list of places is made, once each time the folder is read.
    let mut order: Vec<usize> = (0..games.len()).collect();
    order.sort_by(|&a, &b| match (games[a].name.as_deref(), games[b].name.as_deref()) {
        (Some(x), Some(y)) => x.chars().flat_map(char::to_lowercase).cmp(y.chars().flat_map(char::to_lowercase)),
        (x, y) => x.is_none().cmp(&y.is_none()),
    });
    for (slot, i) in order.into_iter().enumerate() {
        games[i].tape = slot;
    }
}

/// What was clicked in the grid this frame, acted on after drawing it.
#[derive(Default)]
pub(crate) struct ClipActions {
    pub(crate) play: Option<std::path::PathBuf>,
    pub(crate) trim: Option<std::path::PathBuf>,
    pub(crate) recycle: Option<std::path::PathBuf>,
    /// With Delete in the grid: the card to highlight once `recycle` is gone.
    pub(crate) recycle_next: Option<std::path::PathBuf>,
    pub(crate) star: Option<std::path::PathBuf>,
    pub(crate) export: Option<(std::path::PathBuf, media::Share)>,
    /// A tag put on (true) or taken off a clip.
    pub(crate) tag: Option<(std::path::PathBuf, String, bool)>,
    /// A clip added to or taken out of the montage.
    pub(crate) pick: Option<std::path::PathBuf>,
    /// A card clicked with Ctrl (false) or Shift (true) held, for the selection.
    pub(crate) select: Option<(std::path::PathBuf, bool)>,
    /// A card dragged out of the app (with the other picked clips, if it's one of them), and
    /// where the drag began.
    pub(crate) drag_out: Option<(std::path::PathBuf, egui::Pos2)>,
}

/// A new file (trim, share copy, GIF, montage) being made in the background: how far along
/// (percent), then the new file.
#[derive(Default)]
pub(crate) struct ExportJob {
    pub(crate) pct: std::sync::atomic::AtomicU32,
    pub(crate) done: std::sync::Mutex<Option<Result<std::path::PathBuf, String>>>,
    /// Asks it to stop (montages only).
    pub(crate) stop: std::sync::atomic::AtomicBool,
}

impl ExportJob {
    /// Makes `out` with `make` on a background thread. A failed try's file is removed; a made one
    /// goes in the log as "`what` <out>". Repaints when done.
    pub(crate) fn spawn(out: std::path::PathBuf, what: String, ctx: &egui::Context, make: impl FnOnce(&ExportJob, &std::path::Path) -> anyhow::Result<()> + Send + 'static) -> Arc<Self> {
        let job = Arc::new(ExportJob::default());
        let (slot, ctx) = (job.clone(), ctx.clone());
        std::thread::spawn(move || {
            let r = make(&slot, &out).map(|()| out.clone()).map_err(|e| {
                let _ = std::fs::remove_file(&out);
                format!("{e:#}")
            });
            if r.is_ok() {
                log::write(&format!("{what} {}", out.display()));
            }
            *slot.done.lock().unwrap() = Some(r);
            ctx.request_repaint();
        });
        job
    }

    /// Stores how far along it is, 0 to 1.
    pub(crate) fn progress(&self, f: f32) {
        self.pct.store((f * 100.0) as u32, std::sync::atomic::Ordering::Relaxed);
    }
}

/// "Saved Clip (GIF).gif (2.1 MB).", for a file just made.
pub(crate) fn saved_note(out: &std::path::Path) -> String {
    let size = std::fs::metadata(out).map(|m| clips::size(m.len())).unwrap_or_default();
    format!("Saved {} ({size}).", out.file_name().unwrap_or_default().to_string_lossy())
}

/// Montage mode on the Clips page: the clips picked, in the order they play, and how they're put
/// together.
pub(crate) struct Montage {
    pub(crate) clips: Vec<std::path::PathBuf>,
    pub(crate) fades: bool,
    pub(crate) music: Option<std::path::PathBuf>,
    /// The music's volume, 0 to 1.
    pub(crate) volume: f32,
    pub(crate) job: Option<Arc<ExportJob>>,
}

impl Montage {
    /// Empty, with fades on and the music at 40%.
    pub(crate) fn new() -> Self {
        Montage { clips: Vec::new(), fades: true, music: None, volume: 0.4, job: None }
    }
}

/// The share copies to pick from, as a menu's buttons; the one clicked.
pub(crate) fn share_menu(ui: &mut egui::Ui) -> Option<media::Share> {
    let mut pick = None;
    for (share, text, hint) in SHARES {
        if button(ui, text).on_hover_text(hint).clicked() {
            pick = Some(share);
        }
    }
    pick
}

/// The share presets in the clip menu, with a hint for each.
pub(crate) const SHARES: [(media::Share, &str, &str); 4] = [
    (media::Share::Discord10, "Fit for Discord (up to 10 MB)", "Small enough for Discord without Nitro. Best for clips under a minute."),
    (media::Share::Discord50, "Fit for Discord (up to 50 MB)", "For Discord servers with a 50 MB limit."),
    (media::Share::P720, "720p", "A smaller copy at 1280x720 that still looks sharp."),
    (media::Share::Vertical, "Vertical for TikTok / Shorts", "The middle of the picture, cut to 9:16."),
];

/// The Clips page's orders (Config::clip_sort): the name, the heading over the clips (None: one
/// per day), and a hint.
pub(crate) const SORTS: [(&str, Option<&str>, &str); 5] = [
    ("Newest", None, "Newest first, under a heading per day"),
    ("Oldest", None, "Oldest first, under a heading per day"),
    ("Longest", Some("Longest first"), "Longest first. Clips whose length isn't known yet go last."),
    ("Biggest", Some("Biggest first"), "Biggest files first, to see what takes the most space"),
    ("Name", Some("By name"), "A to Z by file name"),
];

/// Puts clips in the order `sort` (an index into SORTS) picks. Ties, and clips whose length isn't
/// known yet (Longest, at the end), keep their order in `list`.
pub(crate) fn sort_clips(list: &mut [&clips::Clip], sort: u8, lengths: &std::collections::HashMap<std::path::PathBuf, f64>) {
    match sort {
        1 => list.sort_by_key(|c| c.modified),
        2 => list.sort_by(|a, b| {
            let len = |c: &clips::Clip| lengths.get(&c.path).copied().unwrap_or(-1.0);
            len(b).total_cmp(&len(a))
        }),
        3 => list.sort_by_key(|c| std::cmp::Reverse(c.bytes)),
        4 => list.sort_by_key(|c| c.name.to_lowercase()),
        _ => list.sort_by_key(|c| std::cmp::Reverse(c.modified)),
    }
}

/// Where an arrow key moves from card `at` (row, column) in a grid with rows `rows` long:
/// left and right go card by card, on into the next or last row; up and down keep the column,
/// or take the row's last card when it's shorter. Stays put at the edges.
pub(crate) fn grid_step(rows: &[usize], (r, c): (usize, usize), key: egui::Key) -> (usize, usize) {
    use egui::Key::*;
    match key {
        ArrowLeft if c > 0 => (r, c - 1),
        ArrowLeft if r > 0 => (r - 1, rows[r - 1] - 1),
        ArrowRight if c + 1 < rows[r] => (r, c + 1),
        ArrowRight if r + 1 < rows.len() => (r + 1, 0),
        ArrowUp if r > 0 => (r - 1, c.min(rows[r - 1] - 1)),
        ArrowDown if r + 1 < rows.len() => (r + 1, c.min(rows[r + 1] - 1)),
        _ => (r, c),
    }
}

/// The clips picked after a click with Ctrl (`range` false) or Shift on `clicked`, from those
/// picked before, the clip picked last (`anchor`) and the clips in the order the page shows them.
/// Ctrl toggles the clip; with nothing picked yet the anchor joins first, so a Ctrl+click after a
/// plain click picks both. Shift picks the clips from the anchor to it in page order instead (just
/// the clip when the anchor isn't on the page).
pub(crate) fn pick_after_click(picked: &[std::path::PathBuf], anchor: Option<&std::path::Path>, clicked: &std::path::Path, range: bool, shown: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    let at = |p: &std::path::Path| shown.iter().position(|s| s == p);
    if range {
        return match (anchor.and_then(at), at(clicked)) {
            (Some(a), Some(b)) => shown[a.min(b)..=a.max(b)].to_vec(),
            _ => vec![clicked.to_path_buf()],
        };
    }
    let mut out: Vec<std::path::PathBuf> = if picked.is_empty() { anchor.filter(|a| *a != clicked).map(std::path::Path::to_path_buf).into_iter().collect() } else { picked.to_vec() };
    match out.iter().position(|p| p == clicked) {
        Some(i) => _ = out.remove(i),
        None => out.push(clicked.to_path_buf()),
    }
    out
}

/// A read of the clips folder, made on another thread (`App::scan_clips`).
pub(crate) struct ClipsScan {
    folder: String,
    list: Vec<clips::Clip>,
    /// The folders read in full (`clips::list_read`); empty when the clips folder couldn't be read.
    read: Vec<std::path::PathBuf>,
    /// Free space on its drive.
    free: Option<u64>,
    /// Starred, tagged and noted clips found again after a rename or move outside the app (old, new).
    found: Vec<(std::path::PathBuf, std::path::PathBuf)>,
    /// App::clip_moves when it began.
    moves: u64,
}

/// The starred, tagged and noted clips (`marked`) for a scan to look for, the lost ones, and the
/// archive folder with its clips as last looked at.
pub(crate) struct Refind {
    marked: std::collections::BTreeSet<String>,
    lost: std::collections::BTreeSet<String>,
    archive: Option<(std::path::PathBuf, Vec<clips::Clip>)>,
}

impl ClipsScan {
    /// Lists `folder`, and looks for the marked clips in it and the archive (`clips::refind_saved`:
    /// existence checks and fingerprint reads, which a NAS or a sleeping disk can make slow).
    /// `moves` is App::clip_moves now.
    pub(crate) fn read(folder: String, job: Refind, moves: u64) -> ClipsScan {
        let ((list, read), free) = (clips::list_read(&folder, usize::MAX), clips::free_space(&folder));
        let found = if job.marked.is_empty() {
            Vec::new()
        } else {
            let scanned: Vec<&clips::Clip> = list.iter().chain(job.archive.iter().flat_map(|(_, l)| l)).collect();
            let roots: Vec<&std::path::Path> = std::iter::once(std::path::Path::new(&folder)).chain(job.archive.as_ref().map(|(d, _)| d.as_path())).collect();
            clips::refind_saved(&job.marked, &scanned, &roots, &job.lost)
        };
        ClipsScan { folder, list, read, free, found, moves }
    }
}

/// Whether the clips folder is due another read: right away after `rescan_clips` (`now`), else
/// once 3 s have gone by since the last one.
pub(crate) fn scan_due(now: bool, since: std::time::Duration) -> bool {
    now || since >= std::time::Duration::from_secs(3)
}

impl App {
    /// A frame of `path` at `at` seconds as a texture: Some(None) if it can't be decoded, None while
    /// it's still being made. Also learns the clip's length. `quick` takes the keyframe before `at`.
    pub(crate) fn frame_texture(&mut self, ctx: &egui::Context, path: &std::path::Path, at: f64, width: usize, quick: bool) -> Option<Option<egui::TextureHandle>> {
        let key = (path.to_path_buf(), (at * 10.0).round().max(0.0) as u32);
        if let Some(t) = self.textures.get(&key) {
            return Some(t.clone());
        }
        let p = self.previews.get(path, at, width, quick, ctx.cumulative_frame_nr())?;
        if let Some(secs) = p.secs {
            self.lengths.insert(path.to_path_buf(), secs);
        }
        let tex = p.frame.map(|f| {
            let img = egui::ColorImage::from_rgba_unmultiplied([f.width, f.height], &f.rgba);
            ctx.load_texture(format!("{}@{}", path.display(), key.1), img, egui::TextureOptions::LINEAR)
        });
        self.textures.insert(key, tex.clone());
        self.textures.trim_thumbnails();
        Some(tex)
    }

    /// The hover-scrub frame of `path` near `at` seconds, or None until it's decoded. A frame takes
    /// a few tenths of a second, so only one is asked for at a time: sweeping across a card doesn't
    /// queue up frames that are stale by the time they're made.
    pub(crate) fn scrub_frame(&mut self, ctx: &egui::Context, path: &std::path::Path, at: f64, width: usize) -> Option<egui::TextureHandle> {
        if let Some((p, t)) = self.scrub_wait.clone()
            && self.frame_texture(ctx, &p, t, width, true).is_some()
        {
            self.scrub_wait = None;
        }
        if !self.textures.contains_key(&(path.to_path_buf(), (at * 10.0).round() as u32)) {
            if self.scrub_wait.is_some() {
                return None;
            }
            // Scrub frames add up (about 0.25 MB each); past a few hundred, drop them all but keep
            // the thumbnails (the frames at 1 s).
            if self.textures.len() > 300 {
                self.textures.retain(|k| k.1 == 10);
                self.previews.retain(|t| t == 10);
            }
            self.scrub_wait = Some((path.to_path_buf(), at));
        }
        self.frame_texture(ctx, path, at, width, true).flatten()
    }

    /// The Clips page's header: the title with how many clips there are and the free space, the
    /// search box, Montage and a menu with Open folder; then the filter chips with the sort on the
    /// right, the selection bar and the last message (upload done, recycled, ...).
    pub(crate) fn clips_header(&mut self, ui: &mut egui::Ui) {
        // A filter with nothing left to show (the archive turned off, a game's clips or a tag all
        // recycled or renamed away) goes back to all games.
        let archive = archive_rule(&self.saved).is_some();
        let tags = self.tags_in_use();
        let gone = match &self.view {
            ClipsView::Archive => !archive,
            ClipsView::Game(g) => !self.clip_list.iter().any(|c| c.group == *g),
            ClipsView::Tag(t) => !tags.contains(t),
            ClipsView::All | ClipsView::Starred => false,
        };
        if gone {
            self.view = ClipsView::All;
        }
        let info = match (&self.view, &self.archive_list) {
            (ClipsView::Archive, Some((dir, Some(list)))) => format!("{} in {}", count(list.len(), "clip"), dir.display()),
            (ClipsView::Archive, Some((dir, None))) => dir.display().to_string(),
            (ClipsView::Archive, None) => "Looking at the archive...".to_string(),
            _ => {
                let mut info = count(self.clip_list.len(), "clip");
                if let Some(free) = self.clips_free {
                    info.push_str(&format!("  ·  {} free", clips::size(free)));
                }
                info
            }
        };
        ui.horizontal(|ui| {
            retro_title(ui, "Clips", 38.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // In a game, its own folder; in the archive, the archive's (once it's connected).
                let (folder, there) = match &self.view {
                    ClipsView::Game(Some(g)) => (std::path::Path::new(&self.saved.folder).join(g), true),
                    ClipsView::Archive => match &self.archive_list {
                        Some((dir, list)) => (dir.clone(), list.is_some()),
                        None => (std::path::PathBuf::new(), false),
                    },
                    _ => (std::path::PathBuf::from(&self.saved.folder), true),
                };
                let (more, _) = cap_menu(ui, egui::Button::new(icon(icon::MORE, 14.0)).min_size(egui::vec2(H, H)), |ui| {
                    if cap(ui, there, icon_button(icon::FOLDER, "Open folder")).on_hover_text("Opens this folder in File Explorer").clicked() {
                        clips::open(&folder, false);
                    }
                });
                more.on_hover_text("More: open the folder");
                let making = self.montage.as_ref().is_some_and(|m| m.job.is_some());
                let text = if self.montage.is_some() { "Close montage" } else { "Montage" };
                let enabled = !making && (!self.clip_list.is_empty() || self.montage.is_some());
                let montage = cap(ui, enabled, egui::Button::new(key_label(text)).min_size(egui::vec2(0.0, H)));
                key_named(&montage, enabled, text);
                let montage = montage.on_hover_text("Pick clips, put them in order and join them into one video, with fades and music if you like");
                if montage.clicked() {
                    self.montage = match self.montage {
                        Some(_) => None,
                        None => Some(Montage::new()),
                    };
                }
                let mut search = egui::TextEdit::singleline(&mut self.search).hint_text(key_label("Search clips")).desired_width((ui.available_width() - 160.0).clamp(140.0, 240.0)).margin(egui::vec2(10.0, 6.0));
                if let Some(k) = look().key_text {
                    search = search.font(k.face.font(k.size));
                }
                ui.add(search).on_hover_text("Type part of a clip's name, game, tag or note to find it");
                // Next to the title, cut short when the window is narrow.
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add_space(4.0);
                    ui.add(egui::Label::new(RichText::new(info).color(pal().muted)).truncate())
                        .on_hover_text("Click a clip to pick it, or use the arrow keys. Enter plays it, F2 renames it, Delete asks, then moves it to the Recycle Bin. Ctrl+click, Shift+click or Ctrl+A picks several; Esc lets them go.");
                });
            });
        });
        if !self.clip_list.is_empty() || archive {
            ui.add_space(10.0);
            let games = std::rc::Rc::clone(&self.clip_games);
            self.filter_chips(ui, archive, tags, &games);
        }
        if self.picked.len() >= 2 {
            self.selection_bar(ui);
        }
        if let Some(msg) = self.clips_msg.clone() {
            ui.add_space(4.0);
            let file = self.exported.as_ref().filter(|(m, _)| *m == msg).map(|(_, f)| f.clone());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(msg).color(accent_light()));
                if let Some(f) = file
                    && cap(ui, true, icon_button(icon::COPY, "Copy").small()).on_hover_text("Copy the file, then paste it into Discord or a chat").clicked()
                {
                    self.clips_msg = Some(match clips::copy_file(&f) {
                        Ok(()) => "Copied. Paste it into Discord, a chat or a folder.".into(),
                        Err(e) => format!("Couldn't copy it: {e:#}"),
                    });
                }
            });
        }
        ui.add_space(14.0);
    }

    /// The filters: All games, a chip per game with its picture and clip count (the same filter
    /// as the game rows in the sidebar), Starred, Archive when it's on, and the tags in use; the
    /// sort on the right.
    pub(crate) fn filter_chips(&mut self, ui: &mut egui::Ui, archive: bool, tags: Vec<String>, games: &[Game]) {
        // Not sorted last, and only beside real games (on its own it's the same as All games).
        let sorted = games.iter().any(|g| g.name.is_some());
        let order = games.iter().filter(|g| g.name.is_some()).chain(games.iter().filter(|g| g.name.is_none() && sorted));
        // Room kept on the right of the first line for the sort.
        let now = (self.cfg.clip_sort as usize).min(SORTS.len() - 1);
        let sort_w = egui::WidgetText::from(key_label(&format!("Sort: {}", SORTS[now].0))).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button).size().x + 2.0 * ui.spacing().button_padding.x;
        ui.horizontal_top(|ui| {
            let w = (ui.available_width() - sort_w - 16.0).max(0.0);
            ui.allocate_ui(egui::vec2(w, 0.0), |ui| {
                ui.horizontal_wrapped(|ui| {
                    chip(ui, &mut self.view, ClipsView::All, "All games").on_hover_text("Every clip, in the order picked with Sort");
                    for g in order {
                        self.game_chip(ui, g);
                    }
                    chip(ui, &mut self.view, ClipsView::Starred, "Starred").on_hover_text("Only your starred clips");
                    if archive {
                        chip(ui, &mut self.view, ClipsView::Archive, "Archive").on_hover_text("Clips moved to the archive drive or NAS");
                    }
                    if !tags.is_empty() {
                        ui.separator();
                    }
                    for t in tags {
                        chip(ui, &mut self.view, ClipsView::Tag(t.clone()), format!("#{t}")).on_hover_text("Only clips with this tag");
                    }
                });
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| self.sort_menu(ui));
        });
    }

    /// A game's chip: its picture in a small circle (a square with its tape color's edge when the
    /// look hands them out), its name (cut when long, all of it on hover) and how many clips it
    /// has, styled like the other chips.
    fn game_chip(&mut self, ui: &mut egui::Ui, g: &Game) {
        let view = ClipsView::Game(g.name.clone());
        let on = self.view == view;
        let tex = self.game_picture(ui.ctx(), g);
        // The cover fits the plain chips' height, so the row stays level.
        let d = (H - 2.0 * ui.spacing().button_padding.y).clamp(12.0, 20.0);
        let tape = tape_color(g.tape);
        let round = if tape.is_some() { 2.0 } else { d / 2.0 };
        let pic = egui::Atom::paint(egui::vec2(d, d), move |ui, a| {
            match &tex {
                Some(tex) => paint_fill(ui, tex, a.rect, round),
                None => _ = ui.painter().rect_filled(a.rect, round, pal().card),
            }
            if let Some(c) = tape {
                ui.painter().rect_stroke(a.rect, round, egui::Stroke::new(2.0, c), egui::StrokeKind::Inside);
            }
        });
        let name = g.name.as_deref().unwrap_or("Not sorted");
        const MAX: usize = 24;
        let cut = name.chars().count() > MAX;
        let shown = if cut { format!("{}...", name.chars().take(MAX - 3).collect::<String>().trim_end()) } else { name.to_string() };
        let label = format!("{shown} {}", g.count);
        let r = chip_with(ui, on, &label, |ink| (pic, key_label(&shown).color(ink), key_label(&g.count.to_string()).color(if on { ink } else { pal().muted })));
        let r = r.on_hover_text(if cut { format!("{name}: only this game's clips") } else { "Only this game's clips".to_string() });
        if r.clicked() {
            self.view = view;
        }
    }

    /// With 2 or more clips picked, a slim bar to star, tag, export, put in a montage or recycle
    /// them all at once, with how many are picked and Clear.
    pub(crate) fn selection_bar(&mut self, ui: &mut egui::Ui) {
        let paths = self.picked.clone();
        let videos: Vec<std::path::PathBuf> = paths.iter().filter(|p| !clips::is_picture(p)).cloned().collect();
        ui.add_space(8.0);
        let frame = egui::Frame::new().fill(pal().card).stroke(egui::Stroke::new(1.0, accent())).corner_radius(R_CARD).inner_margin(egui::Margin::symmetric(12, 6));
        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(strong(format!("{} selected", paths.len()), 14.0).color(pal().text));
                ui.separator();
                let starred = paths.iter().all(|p| self.favorites.contains(&p.display().to_string()));
                let (glyph, text, hint) = if starred {
                    (icon::STAR_FILLED, "Unstar", "Takes the star off every clip picked")
                } else {
                    (icon::STAR, "Star", "Stars every clip picked: kept at the top and safe from auto-cleanup")
                };
                if cap(ui, true, icon_button(glyph, text).small()).on_hover_text(hint).clicked() {
                    self.star(&paths, !starred);
                }
                let (r, tag) = cap_menu(ui, icon_button(icon::TAG, "Tag").small(), |ui| self.tags_menu(ui, &paths));
                r.on_hover_text("Gives every clip picked a tag, or takes it off");
                if let Some((t, on)) = tag.flatten() {
                    self.tag(&paths, &t, on);
                }
                if !videos.is_empty() {
                    let (r, share) = cap_menu(ui, icon_button(icon::SHARE, "Export for sharing").small(), share_menu);
                    r.on_hover_text("Makes a copy of each video picked that fits where you want to share it, one at a time");
                    if let Some(share) = share.flatten() {
                        let waiting: Vec<std::path::PathBuf> = videos.iter().filter(|v| !self.exports.contains_key(*v) && !self.export_queue.iter().any(|(p, _)| p == *v)).cloned().collect();
                        self.clips_msg = Some(format!("Exporting {} for sharing, one at a time.", count(waiting.len(), "clip")));
                        self.export_queue.extend(waiting.into_iter().map(|v| (v, share)));
                    }
                    let making = self.montage.as_ref().is_some_and(|m| m.job.is_some());
                    if cap(ui, !making, icon_button(icon::CLIPS, "Montage").small()).on_hover_text("Puts the videos picked in a montage, in the order you picked them").clicked() {
                        let m = self.montage.get_or_insert_with(Montage::new);
                        for v in &videos {
                            if !m.clips.contains(v) {
                                m.clips.push(v.clone());
                            }
                        }
                        self.picked.clear();
                    }
                }
                if self.can_recycle() && cap(ui, true, icon_button(icon::DELETE, "Move to Recycle Bin").small()).on_hover_text("Deletes the clips picked, after asking; you can get them back from the Recycle Bin").clicked() {
                    self.confirm_recycle = Some(paths.clone());
                }
                ui.separator();
                if cap(ui, true, icon_button(icon::CANCEL, "Clear").small()).on_hover_text("Unpicks them all (Esc)").clicked() {
                    self.picked.clear();
                }
            });
        });
    }

    /// "Sort: Newest" next to the filters, a chip that opens the orders to pick from. The pick is
    /// kept right away (no Save), like More options.
    pub(crate) fn sort_menu(&mut self, ui: &mut egui::Ui) {
        let now = (self.cfg.clip_sort as usize).min(SORTS.len() - 1);
        let under = ui.painter().add(egui::Shape::Noop);
        let words = format!("Sort: {}", SORTS[now].0);
        // A keycap's edge under it, like the chips beside it.
        let (fill, radius, lip, edge) = match look().chip {
            Chip::Ring => (pal().raised, R_SMALL, 2.0, pal().edge),
            Chip::Key { lip, radius } => (pal().key, radius, lip, pal().lip),
        };
        let b = egui::Button::new(key_label(&words).color(pal().text)).fill(fill).corner_radius(radius).stroke(egui::Stroke::NONE).min_size(egui::vec2(0.0, H));
        let mut pick = None;
        let (r, _) = egui::containers::menu::MenuButton::from_button(b).ui(ui, |ui| {
            for (i, (name, _, hint)) in SORTS.iter().enumerate() {
                if ui.add(egui::Button::selectable(i == now, *name)).on_hover_text(*hint).clicked() {
                    pick = Some(i as u8);
                }
            }
        });
        if !r.is_pointer_button_down_on() {
            ui.painter().set(under, egui::Shape::rect_filled(r.rect.translate(egui::vec2(0.0, lip)), radius.min(5), edge));
        }
        key_named(&r, true, &words);
        r.on_hover_text("How the clips are ordered: newest, oldest, longest, biggest or by name");
        if let Some(s) = pick {
            self.keep_view(|c| c.clip_sort = s);
        }
    }

    /// The clips as a grid of cards in the order picked (Sort), under a heading per day when that's
    /// by date. With `keys`, the arrow keys pick a clip and Enter, F2 and Delete act on it.
    pub(crate) fn clips_tab(&mut self, ui: &mut egui::Ui, keys: bool) {
        // A clip clicked to play opens once its file has answered.
        self.finish_opening(ui.ctx());
        let archive = archive_rule(&self.saved).filter(|_| self.view == ClipsView::Archive);
        if self.clip_list.is_empty() && archive.is_none() {
            card(ui, "No clips yet", |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Press").size(16.0).color(pal().text));
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(0.0, 30.0), egui::Sense::hover());
                    let w = keycaps(ui.painter(), egui::pos2(rect.left() + 4.0, rect.center().y), &self.saved.hotkey);
                    ui.add_space(w + 8.0);
                    ui.label(RichText::new("in a game to save your first clip.").size(16.0).color(pal().text));
                });
                ui.label(RichText::new(format!("Each clip keeps the last {}. They show up here.", mmss(self.saved.buffer_secs))).color(pal().muted));
            });
            return;
        }
        if let Some(rule) = &archive {
            self.scan_archive(ui.ctx(), &rule.dir);
            match &self.archive_list {
                None => {
                    ui.label(RichText::new("Looking at the archive...").color(pal().muted));
                    return;
                }
                Some((_, None)) => {
                    self.archive_offline(ui, &rule.dir);
                    return;
                }
                Some((_, Some(list))) if list.is_empty() => {
                    card(ui, "Nothing archived yet", |ui| {
                        ui.label(RichText::new(format!("Clips older than {} days move to {}.", self.saved.archive_days, rule.dir.display())).color(pal().muted));
                    });
                    return;
                }
                _ => {}
            }
        }
        self.montage_panel(ui);
        let cards = self.cards(archive.is_some());
        if cards.shown.is_empty() {
            let text = if self.view == ClipsView::Starred && self.search.trim().is_empty() { "No starred clips yet. Click the star on a clip to keep it here." } else { "No clips match." };
            ui.label(RichText::new(text).color(pal().muted));
            return;
        }
        const GAP: f32 = 16.0;
        let avail = ui.available_width();
        let (cols, w) = grid_cols(avail, 240.0, GAP);
        // Less the card border, drawn outside this width.
        let w = w - 2.0;
        let mut act = ClipActions::default();
        // The cards as they're laid out, row by row, for the arrow keys.
        let rows: Vec<&[ClipItem]> = cards.days.iter().flat_map(|(_, items)| items.chunks(cols)).collect();
        let lens: Vec<usize> = rows.iter().map(|r| r.len()).collect();
        let at = self.selected.as_ref().and_then(|s| rows.iter().enumerate().find_map(|(r, row)| row.iter().position(|i| i.path == *s).map(|c| (r, c))));
        // The clips on the page in order, for Shift+click and Ctrl+A. Picks no longer shown (another
        // view, a search) are let go.
        let shown = &cards.shown;
        self.picked.retain(|p| shown.contains(p));
        let mut reveal = false;
        if keys {
            let pressed = |key| ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key));
            for key in [egui::Key::ArrowLeft, egui::Key::ArrowRight, egui::Key::ArrowUp, egui::Key::ArrowDown] {
                if pressed(key) {
                    let (r, c) = at.map_or((0, 0), |at| grid_step(&lens, at, key));
                    self.selected = Some(rows[r][c].path.clone());
                    self.picked.clear();
                    reveal = true;
                }
            }
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::A)) {
                self.picked = shown.clone();
            }
            // Delete with several picked asks once for them all (not in the archive, as below).
            if self.picked.len() >= 2 && archive.is_none() && pressed(egui::Key::Delete) {
                self.confirm_recycle = Some(self.picked.clone());
            }
            if let Some((r, c)) = at {
                let item = &rows[r][c];
                if pressed(egui::Key::Enter) {
                    if item.is_picture { clips::open(&item.path, false) } else { act.play = Some(item.path.clone()) }
                }
                if pressed(egui::Key::F2) {
                    self.start_rename(&item.path);
                }
                if self.montage.is_some() && !item.is_picture && pressed(egui::Key::Space) {
                    act.pick = Some(item.path.clone());
                }
                // Not in the archive: on a NAS there's no Recycle Bin to get it back from.
                if archive.is_none() && pressed(egui::Key::Delete) {
                    act.recycle = Some(item.path.clone());
                    // The next card along (or the one before the last), so Delete can be pressed
                    // again. Highlighted once the clip is gone, not if the question is cancelled.
                    let next = [egui::Key::ArrowRight, egui::Key::ArrowLeft].map(|k| grid_step(&lens, (r, c), k)).into_iter().find(|&n| n != (r, c));
                    act.recycle_next = next.map(|(r, c)| rows[r][c].path.clone());
                }
            }
        }
        // Small and quiet, like the sidebar's headings, so the clips stay what stands out.
        let h = look().side_heading;
        // Only rows in view, and one either side so their thumbnails are ready, are drawn (and ask
        // for thumbnails); the rest only take their room, at the height they had when last drawn.
        let view = ui.clip_rect();
        let mut guess = w * 9.0 / 16.0 + 120.0;
        for (label, items) in &cards.days {
            let label = if h.caps { label.to_uppercase() } else { label.clone() };
            ui.label(RichText::new(label).family(h.face.family()).size(h.size).color(pal().muted));
            ui.add_space(8.0);
            for row in items.chunks(cols) {
                let id = ui.scope_id().with(("clip row", &row[0].path, cols));
                let known = ui.ctx().data(|d| d.get_temp::<f32>(id));
                let height = known.unwrap_or(guess);
                let top = ui.cursor().top();
                let near = top < view.bottom() + height && top + 2.0 * height > view.top();
                // The card the arrow keys just picked, so it can be scrolled to.
                let picked = reveal && row.iter().any(|i| self.selected.as_ref() == Some(&i.path));
                if near || picked {
                    let drawn = ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = GAP;
                        for item in row {
                            let card = self.clip_card(ui, item, w, &mut act);
                            if reveal && self.selected.as_ref() == Some(&item.path) {
                                card.scroll_to_me(None);
                            }
                        }
                    });
                    let drawn = drawn.response.rect.height();
                    if known != Some(drawn) {
                        ui.ctx().data_mut(|d| d.insert_temp(id, drawn));
                    }
                    guess = drawn;
                } else {
                    ui.allocate_space(egui::vec2(ui.available_width(), height));
                }
                ui.add_space(GAP);
            }
            ui.add_space(4.0);
        }
        if let Some((path, range)) = act.select.take() {
            self.select_click(&path, range, &cards.shown);
        }
        self.clip_actions(act);
    }

    /// Acts on what was clicked on the clip cards this frame.
    pub(crate) fn clip_actions(&mut self, act: ClipActions) {
        if let Some(path) = act.play {
            self.play(&path);
        }
        if let Some((path, at)) = act.drag_out {
            let paths = if self.picked.contains(&path) { self.picked.clone() } else { vec![path] };
            if let Err(e) = crate::drag_out::start(&paths) {
                self.clips_msg = Some(format!("Couldn't drag it out: {e:#}"));
            }
            // Windows took the button's release; egui still thinks it's down until it's told.
            self.drag_out_ended = Some(at);
        }
        if let (Some(path), Some(m)) = (act.pick, &mut self.montage) {
            match m.clips.iter().position(|c| *c == path) {
                Some(i) => _ = m.clips.remove(i),
                None => m.clips.push(path),
            }
        }
        // Asks first, like several clips do.
        if let Some(path) = act.recycle {
            self.confirm_recycle = Some(vec![path]);
            self.recycle_next = act.recycle_next;
        }
        if let Some(path) = act.trim {
            self.begin_trim(&path);
        }
        // One export at a time: it waits its turn behind the others.
        if let Some(export) = act.export {
            self.export_queue.push_back(export);
        }
        if let Some(path) = act.star {
            let on = !self.favorites.contains(&path.display().to_string());
            self.star(&[path], on);
        }
        if let Some((path, tag, on)) = act.tag {
            self.tag(&[path], &tag, on);
        }
    }

    /// Stars `paths` (or takes the star off).
    pub(crate) fn star(&mut self, paths: &[std::path::PathBuf], on: bool) {
        for p in paths {
            let key = p.display().to_string();
            if on {
                self.favorites.insert(key);
            } else {
                self.favorites.remove(&key);
            }
        }
        clips::save_favorites(&self.favorites);
        self.clip_cards.invalidate();
    }

    /// Puts `tag` on `paths` (or takes it off).
    pub(crate) fn tag(&mut self, paths: &[std::path::PathBuf], tag: &str, on: bool) {
        for p in paths {
            clips::set_tag(&mut self.tags, p, tag, on);
        }
        clips::save_tags(&self.tags);
        self.clip_cards.invalidate();
    }

    /// Not in the archive: on a NAS there's no Recycle Bin to get clips back from.
    pub(crate) fn can_recycle(&self) -> bool {
        self.view != ClipsView::Archive
    }

    /// Moves a clip to the Recycle Bin, closing it first if it's playing (the player holds the file
    /// open), and takes it out of the trim, the montage and the selection.
    pub(crate) fn recycle(&mut self, path: &std::path::Path) -> anyhow::Result<()> {
        if self.player.as_ref().is_some_and(|(p, _)| p == path) {
            self.player = None;
        }
        let r = clips::recycle(path);
        if r.is_ok() {
            self.clip_moved(path, None);
        } else {
            // Still there (open in another app): it stays in the montage, picks and queue.
            if !path.exists() {
                self.follow_clip(path, None);
            }
            self.rescan_clips();
        }
        r
    }

    /// A clip moved to `new` or went away (None): its star, tags, note, fingerprint and share link
    /// go with it, and the trim, the montage, the selection and the export queue follow it or let
    /// go. Close the player first if it's playing: it holds the file open.
    pub(crate) fn clip_moved(&mut self, old: &std::path::Path, new: Option<&std::path::Path>) {
        self.carry_clip(old, new, false);
    }

    /// `clip_moved`; `renamed` when the user just renamed it to `new`, a name no file had, so what's
    /// filed under that name is a deleted clip's and goes (see `clips::moved`).
    fn carry_clip(&mut self, old: &std::path::Path, new: Option<&std::path::Path>, renamed: bool) {
        clips::moved(&mut self.favorites, &mut self.tags, &mut self.notes, old, new, renamed);
        // Recycled too: auto-cleanup may have deleted its link.
        self.links = upload::links();
        self.follow_clip(old, new);
        // Frames and lengths of either path are another file's now (a name used again).
        for path in std::iter::once(old).chain(new) {
            self.forget_frames(path);
        }
        // A read of the folder begun before this would bring the clip back where it was.
        self.clip_moves += 1;
        self.clip_cards.invalidate();
        self.rescan_clips();
    }

    /// Drops the frames, length and previews made of `path`, so they're made again from the file
    /// there now.
    fn forget_frames(&mut self, path: &std::path::Path) {
        self.textures.retain(|k| k.0 != path);
        self.lengths.remove(path);
        self.previews.forget_path(path);
        if self.scrub_wait.as_ref().is_some_and(|(p, _)| p == path) {
            self.scrub_wait = None;
        }
    }

    /// The trim, the montage, the selection and the export queue, with `old` changed to `new` or
    /// taken out (None).
    fn follow_clip(&mut self, old: &std::path::Path, new: Option<&std::path::Path>) {
        if self.trim.as_ref().is_some_and(|t| t.path == old) {
            self.trim = None;
        }
        let follow = |paths: &mut Vec<std::path::PathBuf>| match new {
            Some(new) => paths.iter_mut().filter(|p| *p == old).for_each(|p| *p = new.to_path_buf()),
            None => paths.retain(|p| p != old),
        };
        if let Some(m) = &mut self.montage {
            follow(&mut m.clips);
        }
        follow(&mut self.picked);
        match new {
            Some(new) => self.export_queue.iter_mut().filter(|(p, _)| p == old).for_each(|(p, _)| *p = new.to_path_buf()),
            None => self.export_queue.retain(|(p, _)| p != old),
        }
        if let Some(new) = new
            && self.selected.as_deref() == Some(old)
        {
            self.selected = Some(new.to_path_buf());
        }
        // An upload running for it files its link where the clip is now, once done (`finish_uploads`).
        if let Some(new) = new
            && !self.uploads.contains_key(new)
            && let Some(job) = self.uploads.remove(old)
        {
            self.uploads.insert(new.to_path_buf(), job);
        }
    }

    /// Reads the clips folder again on the next frame.
    pub(crate) fn rescan_clips(&mut self) {
        self.rescan_now = true;
    }

    /// Moves the clips picked to the Recycle Bin (once "Move them?" is answered), and says how it went.
    pub(crate) fn recycle_picked(&mut self, paths: &[std::path::PathBuf]) {
        let linked = self.linked(paths);
        let failed: Vec<String> = paths.iter().filter_map(|p| self.recycle(p).err().map(|e| format!("{}: {e:#}", p.file_name().unwrap_or_default().to_string_lossy()))).collect();
        let moved = paths.len() - failed.len();
        let mut msg = format!("Moved {} to the Recycle Bin.", count(moved, "clip"));
        if !failed.is_empty() {
            msg.push_str(&format!(" Couldn't recycle {}", failed.join("; ")));
        }
        self.clips_msg = Some(msg);
        // Only the clips that went: `delete_gone` skips any still there.
        if self.link_cleanup.with_recycle && !linked.is_empty() {
            self.delete_links(linked);
        }
        self.picked.clear();
        // Delete in the grid on one clip goes on to the next card.
        let next = self.recycle_next.take();
        if self.selected.as_ref().is_some_and(|s| paths.contains(s) && failed.len() < paths.len()) {
            self.selected = next;
        }
    }

    /// The clips of `paths` with a share link.
    fn linked(&self, paths: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
        paths.iter().filter(|p| clips::stored_key(self.links.keys(), p).is_some()).cloned().collect()
    }

    /// In the Recycle Bin dialog, when any of `paths` has a share link: "Also delete their share
    /// links" (on unless unticked, then off for the rest of the session). Compared without
    /// allocating, as the dialog draws every frame.
    pub(crate) fn recycle_links_box(&mut self, ui: &mut egui::Ui, paths: &[std::path::PathBuf]) {
        if self.links.keys().any(|k| paths.iter().any(|p| p.as_os_str() == k.as_str())) {
            let text = if paths.len() == 1 { "Also delete its share link" } else { "Also delete their share links" };
            ui.checkbox(&mut self.link_cleanup.with_recycle, text).on_hover_text("Takes the uploaded copies off kappa.lol, so the links stop working. Left on, a link keeps working after the clip is gone.");
        }
    }

    /// Deletes the share links of `paths` whose clips are gone, in the background (see
    /// `upload::delete_gone`), from the next frame on, after any being deleted now.
    pub(crate) fn delete_links(&mut self, paths: Vec<std::path::PathBuf>) {
        self.link_cleanup.msg = None;
        self.link_cleanup.waiting.extend(paths);
    }

    /// The share links of clips gone from the clips folder, from its last read (`upload::orphans`),
    /// for General > Sharing. Worked out when a read comes in, not every frame. None while the
    /// clips folder can't be read.
    fn note_orphans(&mut self) {
        self.link_cleanup.orphans = upload::orphans(self.links.keys(), &self.saved.folder, &self.clip_list, &self.clips_read);
    }

    /// The selection after a Ctrl+click on a card (toggles it) or a Shift+click (`range`) on it,
    /// done in `pick_after_click`, with the clip picked last as the anchor.
    pub(crate) fn select_click(&mut self, path: &std::path::Path, range: bool, shown: &[std::path::PathBuf]) {
        self.picked = pick_after_click(&self.picked, self.selected.as_deref(), path, range, shown);
        // One left picked is the one Enter, F2 and Delete act on, as its highlight shows.
        if let [one] = self.picked.as_slice() {
            self.selected = Some(one.clone());
        }
        // Shift+clicks keep the anchor, so the range can be changed from the same clip.
        if (!range || self.selected.is_none()) && self.picked.iter().any(|p| p == path) {
            self.selected = Some(path.to_path_buf());
        }
    }

    /// The cards to show (`Cards`), made again only when the view, search, sort or `archive` (the
    /// Archive view is showing the archive's clips) changed, or the cache was invalidated. Sorted
    /// by length, also when more lengths are known.
    fn cards(&mut self, archive: bool) -> std::rc::Rc<Cards> {
        let lengths = if self.cfg.clip_sort == 2 { self.lengths.len() } else { 0 };
        let made = &self.clip_cards.made;
        let same = made.as_ref().is_some_and(|(v, s, sort, a, l)| *v == self.view && *s == self.search && *sort == self.cfg.clip_sort && *a == archive && *l == lengths);
        if !same || self.clip_cards.stale {
            self.clip_cards.cards = std::rc::Rc::new(self.make_cards(archive));
            self.clip_cards.made = Some((self.view.clone(), self.search.clone(), self.cfg.clip_sort, archive, lengths));
            self.clip_cards.stale = false;
        }
        std::rc::Rc::clone(&self.clip_cards.cards)
    }

    /// The clips of the view (the archive's with `archive`) with every word searched for, in the
    /// order picked: the first 60 and every starred one however far down. Starred ones first under
    /// their own heading (unless only starred ones are shown), then a heading per day, or one for
    /// the order picked.
    fn make_cards(&self, archive: bool) -> Cards {
        let words: Vec<String> = self.search.to_lowercase().split_whitespace().map(String::from).collect();
        let no_tags = std::collections::BTreeSet::new();
        let tags_of = |c: &clips::Clip| self.tags.get(&c.path.display().to_string()).unwrap_or(&no_tags);
        let source = match (archive, &self.archive_list) {
            (true, Some((_, Some(list)))) => list,
            _ => &self.clip_list,
        };
        let mut found: Vec<&clips::Clip> = source
            .iter()
            .filter(|c| match &self.view {
                ClipsView::Game(g) => c.group == *g,
                ClipsView::Tag(t) => tags_of(c).contains(t),
                ClipsView::Starred => self.favorites.contains(&c.path.display().to_string()),
                ClipsView::All | ClipsView::Archive => true,
            })
            // Every word typed appears in the name, the game folder, a tag or the note.
            .filter(|c| {
                words.is_empty() || {
                    let tags: Vec<&str> = tags_of(c).iter().map(String::as_str).collect();
                    let note = self.notes.get(&c.path.display().to_string()).map_or("", String::as_str);
                    let hay = format!("{} {} {} {note}", c.name, c.group.as_deref().unwrap_or_default(), tags.join(" ")).to_lowercase();
                    words.iter().all(|w| hay.contains(w.as_str()))
                }
            })
            .collect();
        sort_clips(&mut found, self.cfg.clip_sort, &self.lengths);
        // Each game's place in the tape colors, among the clips folder's games.
        let tape_of = |group: &Option<String>| self.clip_games.iter().find(|g| g.name == *group).map(|g| g.tape);
        let mut n = 0;
        let list: Vec<ClipItem> = found
            .into_iter()
            .filter(|c| {
                n += 1;
                n <= 60 || self.favorites.contains(&c.path.display().to_string())
            })
            .map(|c| ClipItem { tape: tape_of(&c.group), ..self.clip_item(c) })
            .collect();
        let (starred, list): (Vec<ClipItem>, Vec<ClipItem>) = list.into_iter().partition(|i| i.favorite && self.view != ClipsView::Starred);
        let mut days: Vec<(String, Vec<ClipItem>)> = Vec::new();
        if !starred.is_empty() {
            days.push(("Starred".into(), starred));
        }
        let group_by = SORTS.get(self.cfg.clip_sort as usize).and_then(|s| s.1);
        for item in list {
            let label = group_by.map_or_else(|| clips::day_label(item.modified), String::from);
            match days.last_mut() {
                Some((l, items)) if *l == label => items.push(item),
                _ => days.push((label, vec![item])),
            }
        }
        let shown = days.iter().flat_map(|(_, items)| items.iter().map(|i| i.path.clone())).collect();
        Cards { days, shown }
    }

    pub(crate) fn clip_item(&self, c: &clips::Clip) -> ClipItem {
        let key = c.path.display().to_string();
        ClipItem {
            path: c.path.clone(),
            name: c.name.clone(),
            bytes: c.bytes,
            modified: c.modified,
            is_picture: c.is_picture,
            favorite: self.favorites.contains(&key),
            tags: self.tags.get(&key).map(|t| t.iter().cloned().collect()).unwrap_or_default(),
            note: self.notes.get(&key).cloned(),
            tape: None,
        }
    }

    /// Picks up the last read of the clips folder, and every 3 s (or right away after
    /// rescan_clips) reads it again on another thread, so a big library or a NAS never holds up
    /// the window. Marked clips are looked for again on that thread too (`ClipsScan::read`). Tests
    /// read it in place.
    pub(crate) fn scan_clips(&mut self, ctx: &egui::Context) {
        if let Some(scan) = self.clips_scan.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.clips_scan = None;
            self.take_scan(scan);
        }
        if self.clips_scan.is_some() || !scan_due(self.rescan_now, self.clips_checked.elapsed()) {
            return;
        }
        self.clips_checked = std::time::Instant::now();
        self.rescan_now = false;
        let (folder, refind, moves) = (self.saved.folder.clone(), self.refind_job(), self.clip_moves);
        if cfg!(test) {
            self.take_scan(ClipsScan::read(folder, refind, moves));
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(ClipsScan::read(folder, refind, moves));
            ctx.request_repaint();
        });
        self.clips_scan = Some(rx);
    }

    /// What a scan needs to look for the starred, tagged and noted clips: those clips, the lost
    /// ones, and the archive as last looked at (so its clips are looked in too).
    fn refind_job(&self) -> Refind {
        let marked: std::collections::BTreeSet<String> = self.favorites.iter().chain(self.tags.keys()).chain(self.notes.keys()).cloned().collect();
        let archive = self.archive_list.as_ref().filter(|_| !marked.is_empty()).and_then(|(dir, list)| Some((dir.clone(), list.clone()?)));
        Refind { marked, lost: self.lost.clone(), archive }
    }

    /// Takes a scan's read of the clips folder, and carries the star, tags, note and share link of
    /// the marked clips it found again. Not if the folder setting changed since it began, or a clip
    /// moved (`clip_moved`, which asks for another read): it would show the clip where it was.
    fn take_scan(&mut self, scan: ClipsScan) {
        if scan.folder != self.saved.folder || scan.moves != self.clip_moves {
            return;
        }
        self.clips_free = scan.free;
        self.clips_read = scan.read;
        self.set_clip_list(scan.list);
        for (old, new) in &scan.found {
            self.clip_moved(old, Some(new));
            log::write(&format!("Found {} again at {}; its star, tags and note went with it.", old.display(), new.display()));
        }
        if !(self.favorites.is_empty() && self.tags.is_empty() && self.notes.is_empty()) {
            let lost = self.lost.len();
            // Found again (carried away from the old key just now), or no longer starred, tagged or noted.
            self.lost.retain(|k| self.favorites.contains(k) || self.tags.contains_key(k) || self.notes.contains_key(k));
            if self.lost.len() != lost {
                clips::save_lost(&self.lost);
            }
        }
        self.note_orphans();
    }

    /// The clips folder setting changed from `old`: the stars, tags, notes, fingerprints and share
    /// links filed under it follow to the same place in the new folder (see `clips::refolder_plan`).
    /// Marked clips found in neither are kept as lost: they're looked for by fingerprint in every
    /// scan, and while any is starred auto-cleanup holds off (General > Storage says so). Records
    /// the new folder as the one they're filed against (`clips::filed`). Looks at the disk for each
    /// marked clip, on the UI thread: only on Save of a new folder, or once at start.
    pub(crate) fn refolder(&mut self, old: &str) {
        let new = self.saved.folder.clone();
        let mut keys: std::collections::BTreeSet<String> = self.favorites.iter().chain(self.tags.keys()).chain(self.notes.keys()).cloned().collect();
        keys.extend(self.links.keys().cloned());
        let (moves, lost) = clips::refolder_plan(&keys, old, &new, std::path::Path::exists);
        let mut saved = clips::rekey(&mut self.favorites, &mut self.tags, &mut self.notes, &moves);
        self.clip_cards.invalidate();
        self.links = upload::links();
        // Share links alone aren't worth holding cleanup for, nor looking for.
        let lost: Vec<String> = lost.into_iter().filter(|k| self.favorites.contains(k) || self.tags.contains_key(k) || self.notes.contains_key(k)).collect();
        log::write(&format!("Clips folder changed from {old} to {new}: {} marked clips carried over, {} not found.", moves.len(), lost.len()));
        if !lost.is_empty() {
            self.lost.extend(lost);
            saved &= clips::save_lost(&self.lost);
        }
        // Only now that they're saved under it: a crash or a failed save before this re-keys again
        // next start. Until then auto-cleanup holds off in the new folder (`clips::unfiled`), as the
        // stars it reads are still under the old keys.
        self.unfiled = !clips::save_filed_if(&new, saved);
        self.rescan_clips();
    }

    /// Lets go of the lost clips (see `refolder`): their stars, tags and notes stay filed under the
    /// old place, and auto-cleanup goes on.
    pub(crate) fn forget_lost(&mut self) {
        log::write(&format!("Auto-cleanup goes on; {} clips from an earlier clips folder are no longer looked for.", self.lost.len()));
        self.lost.clear();
        clips::save_lost(&self.lost);
    }

    /// Picks up the last look at the archive folder `dir`, and every few seconds looks again on
    /// another thread, so a NAS that's slow to answer never holds up the window.
    pub(crate) fn scan_archive(&mut self, ctx: &egui::Context, dir: &std::path::Path) {
        if let Some(found) = self.archive_scan.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.archive_list = Some(found);
            self.archive_scan = None;
            self.clip_cards.invalidate();
        }
        if self.archive_list.as_ref().is_some_and(|(d, _)| d != dir) {
            self.archive_list = None;
            self.clip_cards.invalidate();
        }
        let stale = self.archive_list.is_none() || self.archive_checked.elapsed() > std::time::Duration::from_secs(3);
        if self.archive_scan.is_none() && stale {
            self.archive_checked = std::time::Instant::now();
            let (tx, rx) = std::sync::mpsc::channel();
            let (dir, ctx) = (dir.to_path_buf(), ctx.clone());
            std::thread::spawn(move || {
                let found = clips::archived(&dir);
                let _ = tx.send((dir, found));
                ctx.request_repaint();
            });
            self.archive_scan = Some(rx);
        }
    }

    /// The Archive view while its drive or NAS isn't connected: a note saying so, and the starred,
    /// tagged and noted clips known to be in it (read-only; they play once it's back).
    pub(crate) fn archive_offline(&self, ui: &mut egui::Ui, dir: &std::path::Path) {
        card(ui, "Archive drive not connected", |ui| {
            ui.label(RichText::new("Connect the drive or turn on the NAS to play archived clips. Clips due to move wait until then.").color(pal().muted));
        });
        let folder = dir.display().to_string();
        let mut known: Vec<&String> = self.favorites.iter().chain(self.tags.keys()).chain(self.notes.keys()).filter(|p| clips::inside(p, &folder)).collect();
        known.sort_by_key(|p| p.to_lowercase());
        known.dedup();
        if known.is_empty() {
            return;
        }
        ui.label(heading("Starred, tagged and noted in the archive", 15.0).color(pal().text));
        ui.add_space(8.0);
        for key in known {
            let path = std::path::Path::new(key);
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let game = path.parent().filter(|p| p != &dir).and_then(|p| p.file_name()).map(|g| g.to_string_lossy());
            let tags: Vec<String> = self.tags.get(key).map(|t| t.iter().cloned().collect()).unwrap_or_default();
            let row = egui::Frame::new().fill(pal().card).corner_radius(R_CARD).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    let (glyph, color) = if self.favorites.contains(key) { (icon::STAR_FILLED, pal().amber) } else { (icon::STAR, pal().muted) };
                    ui.label(icon(glyph, 14.0).color(color));
                    ui.label(heading(&name, 14.0).color(pal().text));
                    if let Some(g) = game {
                        ui.label(RichText::new(g).small().color(pal().muted));
                    }
                    tag_pills(ui, &tags);
                });
                if let Some(note) = self.notes.get(key) {
                    ui.add(egui::Label::new(RichText::new(note).small().color(pal().text)).truncate());
                }
            });
            row.response.on_hover_text(format!("{key}\nPlays once the archive drive is connected"));
            ui.add_space(6.0);
        }
    }

    /// Takes a new read of the clips folder, and its games with it (the only way clip_list changes).
    /// The cards are made again from it, so day headings and stars, tags or notes changed from
    /// elsewhere also show within a read.
    pub(crate) fn set_clip_list(&mut self, list: Vec<clips::Clip>) {
        self.clip_games = games_in(&list).into();
        self.clip_list = list;
        self.clip_cards.invalidate();
    }

    /// A game's picture: its Steam cover once downloaded, else a frame of its newest clip (the
    /// same frame its card shows: frames are cached by time, not size, so it's asked for at 640 px
    /// wide, sharp enough for either).
    pub(crate) fn game_picture(&mut self, ctx: &egui::Context, g: &Game) -> Option<egui::TextureHandle> {
        if let Some(name) = &g.name {
            if !self.covers.contains_key(name)
                && let Some(path) = covers::get(name, self.cfg.steam_covers && self.saved.steam_covers)
            {
                let tex = covers::load(&path).map(|img| ctx.load_texture(format!("cover {name}"), img, egui::TextureOptions::LINEAR));
                self.covers.insert(name.clone(), tex);
            }
            if let Some(Some(tex)) = self.covers.get(name) {
                return Some(tex.clone());
            }
        }
        g.thumb.as_ref().and_then(|p| self.frame_texture(ctx, p, 1.0, 640, false)).flatten()
    }

    /// One clip in the grid: its thumbnail (click to play; the length in the corner), name, time
    /// and size, then Trim, sharing and a menu with the rest.
    pub(crate) fn clip_card(&mut self, ui: &mut egui::Ui, item: &ClipItem, w: f32, act: &mut ClipActions) -> egui::Response {
        let path = &item.path;
        // Hovered last frame: the card lifts, with a purple edge and glow. Selected: a brighter edge.
        let hover_id = ui.scope_id().with(("card hover", path));
        let lifted = ui.ctx().data(|d| d.get_temp::<bool>(hover_id)).unwrap_or(false);
        // In montage mode, its place in the montage (None while it's not in it).
        let picked = self.montage.as_ref().filter(|_| !item.is_picture).map(|m| m.clips.iter().position(|c| c == path));
        // Picked with Ctrl or Shift (with 2 or more, a tick on it too), or else the one clicked.
        let ticked = self.picked.len() >= 2 && self.picked.contains(path);
        let chosen = if self.picked.is_empty() { self.selected.as_ref() == Some(path) } else { self.picked.contains(path) };
        let selected = chosen || matches!(picked, Some(Some(_)));
        // A glow in Retro only; Minimalist stays flat.
        let glow = if effects() { egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: alpha(accent(), 60) } } else { egui::Shadow::NONE };
        let (stroke, shadow) = match (selected, lifted) {
            // Same width as the others, so the grid doesn't shift.
            (true, _) => (egui::Stroke::new(1.0, accent()), glow),
            (false, true) => (egui::Stroke::new(1.0, accent().lerp_to_gamma(pal().border, 0.35)), glow),
            _ => (egui::Stroke::new(1.0, pal().card), egui::Shadow::NONE),
        };
        let (style, radius) = (look().clip, look().card_radius);
        let frame = egui::Frame::new().fill(pal().card).stroke(stroke).corner_radius(radius).shadow(shadow);
        // A dark lip under the card, painted once its size is known.
        let under = ui.painter().add(egui::Shape::Noop);
        // Clicks on the card itself (not its buttons) select it.
        let body = egui::UiBuilder::new().id_salt(("card", path)).sense(egui::Sense::click());
        let shown = ui.scope_builder(body, |ui| frame.show(ui, |ui| ui.vertical(|ui| {
            ui.set_width(w);
            ui.spacing_mut().item_spacing.y = 4.0;
            let (rect, thumb) = ui.allocate_exact_size(egui::vec2(w, w * 9.0 / 16.0), egui::Sense::click_and_drag());
            let rounding = egui::CornerRadius { nw: radius, ne: radius, sw: 0, se: 0 };
            let width = (w * 1.5) as usize;
            // Hover scrubbing: across a video, the pointer's place picks the frame, in 16 steps
            // (the middle of each, as a fraction of the clip, and in seconds).
            let scrub = match (thumb.hover_pos(), self.lengths.get(path)) {
                (Some(p), Some(&len)) if !item.is_picture => {
                    let frac = (((p.x - rect.left()) / rect.width() * 16.0).floor().clamp(0.0, 15.0) as f64 + 0.5) / 16.0;
                    Some((frac, (frac * len * 10.0).round() / 10.0))
                }
                _ => None,
            };
            let shown_id = hover_id.with("scrubbed");
            let tex = match scrub {
                _ if item.is_picture => None,
                Some((_, at)) => match self.scrub_frame(ui.ctx(), path, at, width) {
                    Some(t) => {
                        ui.ctx().data_mut(|d| d.insert_temp(shown_id, t.clone()));
                        Some(Some(t))
                    }
                    // Until it's decoded, the frame shown last (or the thumbnail) stays up.
                    None => ui.ctx().data(|d| d.get_temp(shown_id)).map(Some).or_else(|| self.frame_texture(ui.ctx(), path, 1.0, width, false)),
                },
                None => {
                    ui.ctx().data_mut(|d| d.remove::<egui::TextureHandle>(shown_id));
                    self.frame_texture(ui.ctx(), path, 1.0, width, false)
                }
            };
            match tex {
                Some(Some(tex)) => {
                    // A clip that isn't 16:9 (ultrawide, 4:3, tall) keeps its shape, with black bars.
                    let size = tex.size_vec2();
                    let fit = size.x / size.y / (rect.width() / rect.height());
                    if size.min_elem() > 0.0 && (fit - 1.0).abs() > 0.02 {
                        ui.painter().rect_filled(rect, rounding, Color32::BLACK);
                        let picture = egui::Rect::from_center_size(rect.center(), size * (rect.width() / size.x).min(rect.height() / size.y));
                        egui::Image::new(&tex).paint_at(ui, picture);
                    } else {
                        egui::Image::new(&tex).corner_radius(rounding).paint_at(ui, rect);
                    }
                }
                other => {
                    ui.painter().rect_filled(rect, rounding, pal().raised);
                    let gif = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("gif"));
                    let (glyph, text) = if item.is_picture { (icon::PICTURE, if gif { "GIF" } else { "Screenshot" }) } else if other.is_some() { (icon::CLIPS, "No preview") } else { (icon::CLIPS, "") };
                    paint_icon(ui.painter(), rect.center() - egui::vec2(0.0, 8.0), egui::Align2::CENTER_CENTER, glyph, 26.0, pal().muted);
                    ui.painter().text(rect.center() + egui::vec2(0.0, 20.0), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(12.0), pal().muted);
                }
            }
            if thumb.hovered() {
                // Darken (only a little while scrubbing, to see the frame), with a play button in the middle.
                ui.painter().rect_filled(rect, rounding, Color32::from_black_alpha(if scrub.is_some() { 40 } else { 90 }));
                if !item.is_picture && picked.is_none() {
                    ui.painter().circle_filled(rect.center(), 24.0, Color32::from_white_alpha(230));
                    paint_icon(ui.painter(), rect.center() + egui::vec2(2.0, 0.0), egui::Align2::CENTER_CENTER, icon::PLAY, 18.0, Color32::BLACK);
                }
            }
            // How far along the clip the scrub is, along the bottom edge.
            if let Some((frac, _)) = scrub {
                let bar = egui::Rect::from_min_size(rect.left_bottom() - egui::vec2(0.0, 3.0), egui::vec2(rect.width() * frac as f32, 3.0));
                ui.painter().rect_filled(bar, 0.0, accent());
            }
            // The VCR's on-screen display: PLAY in the corner and the length, white on a black shadow.
            let osd = |at: egui::Pos2, align: egui::Align2, text: &str| {
                let font = Face::Osd.font(20.0);
                ui.painter().text(at + egui::vec2(1.0, 1.0), align, text, font.clone(), Color32::BLACK);
                ui.painter().text(at, align, text, font, Color32::WHITE);
            };
            if style.osd && !item.is_picture && picked.is_none() {
                osd(rect.left_top() + egui::vec2(8.0, 4.0), egui::Align2::LEFT_TOP, "PLAY \u{25B6}");
            }
            if let Some(len) = self.lengths.get(path) {
                let text = mmss(len.round() as u32);
                if style.osd {
                    osd(rect.right_bottom() - egui::vec2(8.0, 2.0), egui::Align2::RIGHT_BOTTOM, &text);
                } else {
                    let galley = ui.painter().layout_no_wrap(text, egui::FontId::new(12.0, egui::FontFamily::Name("strong".into())), Color32::WHITE);
                    let badge = egui::Rect::from_min_size(rect.right_bottom() - galley.size() - egui::vec2(18.0, 14.0), galley.size() + egui::vec2(10.0, 4.0));
                    ui.painter().rect_filled(badge, 4.0, Color32::from_black_alpha(190));
                    ui.painter().galley(badge.min + egui::vec2(5.0, 2.0), galley, Color32::WHITE);
                }
            }
            // The game's color in a thin stripe under the picture.
            if style.stripe > 0.0 {
                let stripe = egui::Rect::from_min_size(rect.left_bottom(), egui::vec2(rect.width(), style.stripe));
                ui.painter().rect_filled(stripe, 0.0, item.tape.and_then(tape_color).unwrap_or(pal().raised));
            }
            // In montage mode: a ring to click, or its number once it's in.
            if let Some(at) = picked {
                let c = rect.left_top() + egui::vec2(20.0, 20.0);
                match at {
                    Some(i) => {
                        ui.painter().circle_filled(c, 12.0, accent());
                        ui.painter().text(c, egui::Align2::CENTER_CENTER, (i + 1).to_string(), egui::FontId::new(13.0, egui::FontFamily::Name("strong".into())), on_accent());
                    }
                    None => {
                        ui.painter().circle(c, 11.0, Color32::from_black_alpha(120), egui::Stroke::new(2.0, Color32::WHITE));
                    }
                }
            }
            if ticked {
                let c = rect.right_top() + egui::vec2(-20.0, 20.0);
                ui.painter().circle(c, 12.0, accent(), egui::Stroke::new(2.0, Color32::WHITE));
                paint_icon(ui.painter(), c, egui::Align2::CENTER_CENTER, icon::CHECK, 12.0, on_accent());
            }
            let hint = if picked.is_some() { "Adds it to the montage" } else if item.is_picture { "Opens the picture" } else { "Plays the clip. Move across it to preview." };
            let thumb = thumb.on_hover_text(format!("{hint}\nDrag it into Discord, a browser or a folder to share it.\nCtrl+click or Shift+click picks several clips.")).on_hover_cursor(egui::CursorIcon::PointingHand);
            let mods = ui.input(|i| i.modifiers);
            if thumb.drag_started() && picked.is_none() {
                act.drag_out = Some((path.clone(), thumb.interact_pointer_pos().unwrap_or(rect.center())));
            } else if thumb.clicked() && (mods.command || mods.shift) {
                act.select = Some((path.clone(), mods.shift));
            } else if thumb.clicked() {
                self.picked.clear();
                self.selected = Some(path.clone());
                if picked.is_some() {
                    act.pick = Some(path.clone());
                } else if item.is_picture {
                    clips::open(path, false)
                } else {
                    act.play = Some(path.clone())
                }
            }
            egui::Frame::new().inner_margin(egui::Margin { left: 12, right: 12, top: 8, bottom: 10 }).show(ui, |ui| {
                ui.set_width(w - 24.0);
                // Text that can be selected takes the click, so a click on the name or the line
                // under it wouldn't pick the card (and Delete then had nothing to act on).
                ui.style_mut().interaction.selectable_labels = false;
                // Without ".mp4": a cut name ends in "...", not "....".
                let title = std::path::Path::new(&item.name).file_stem().map_or(item.name.as_str().into(), |s| s.to_string_lossy());
                let (face, size, caps) = style.title;
                let title = if caps { title.to_uppercase() } else { title.into_owned() };
                ui.add(egui::Label::new(RichText::new(title).family(face.family()).size(size).color(pal().text)).truncate()).on_hover_text(&item.name);
                if self.noting.as_ref().is_some_and(|(p, _)| p == path) {
                    self.note_field(ui);
                } else if let Some(note) = &item.note {
                    ui.add(egui::Label::new(RichText::new(note).small().color(pal().text)).truncate()).on_hover_text(note);
                }
                ui.horizontal(|ui| {
                    let (face, size) = style.details;
                    ui.label(RichText::new(format!("{}  ·  {}", clips::time_label(item.modified), clips::size(item.bytes))).family(face.family()).size(size).color(pal().muted));
                    if let Some(job) = self.exports.get(path) {
                        ui.label(RichText::new(format!("Exporting {}%", job.pct.load(std::sync::atomic::Ordering::Relaxed))).small().color(accent_light()));
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
                    } else if self.export_queue.iter().any(|(p, _)| p == path) {
                        ui.label(RichText::new("Waiting to export").small().color(pal().muted));
                    }
                });
                if !item.tags.is_empty() {
                    ui.horizontal_wrapped(|ui| tag_pills(ui, &item.tags));
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if item.is_picture {
                        if ui.small_button("Open").on_hover_text("Opens the picture").clicked() {
                            clips::open(path, false);
                        }
                    } else if cap(ui, true, icon_button(icon::CUT, "Trim").small()).on_hover_text("Cut the clip shorter, or keep a few parts").clicked() {
                        act.trim = Some(path.clone());
                    }
                    self.share_button(ui, path, item.bytes);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Framed like Trim and Share, so the row reads as one set of buttons.
                        cap_menu(ui, egui::Button::new(icon(icon::MORE, 14.0)).small(), |ui| {
                            if button(ui, "Show in folder").on_hover_text("Opens its folder with the file picked").clicked() {
                                clips::open(path, true);
                            }
                            // In here, not on the card: the card's row has no room for it beside Share.
                            if button(ui, "Copy file").on_hover_text("Copy the file, then paste it into Discord, a chat or a folder").clicked() {
                                let name = path.file_name().unwrap_or_default().to_string_lossy();
                                self.clips_msg = Some(match clips::copy_file(path) {
                                    // Discord's free upload limit.
                                    Ok(()) if item.bytes > 10 << 20 => format!("Copied {name}. Paste it anywhere; Discord takes files over 10 MB only with Nitro."),
                                    Ok(()) => format!("Copied {name}. Paste it into Discord, a chat or a folder."),
                                    Err(e) => format!("Couldn't copy it: {e:#}"),
                                });
                            }
                            if ui.add(egui::Button::new("Rename...").shortcut_text("F2")).on_hover_text("Gives the clip a new name").clicked() {
                                self.start_rename(path);
                            }
                            let text = if item.note.is_some() { "Edit note" } else { "Add note" };
                            if button(ui, text).on_hover_text("A one-line note under the clip's name, found by search too").clicked() {
                                self.noting = Some((path.clone(), item.note.clone().unwrap_or_default()));
                            }
                            if !item.is_picture && !self.exports.contains_key(path) && !self.export_queue.iter().any(|(p, _)| p == path) {
                                ui.menu_button("Export for sharing", |ui| {
                                    if let Some(share) = share_menu(ui) {
                                        act.export = Some((path.clone(), share));
                                    }
                                })
                                .response
                                .on_hover_text("Makes a copy that fits where you want to share it");
                            }
                            ui.menu_button("Tags", |ui| {
                                if let Some((t, on)) = self.tags_menu(ui, std::slice::from_ref(path)) {
                                    act.tag = Some((path.clone(), t, on));
                                }
                            })
                            .response
                            .on_hover_text("Labels like Aces or Funny, to find clips later");
                            if upload::is_discord_webhook(&self.saved.discord_webhook)
                                && !self.uploads.contains_key(path)
                                && button(ui, "Post to Discord").on_hover_text("Sends the file to your webhook's channel (up to 10 MB)").clicked()
                            {
                                self.confirm_discord = Some(path.clone());
                            }
                            if self.links.contains_key(&path.display().to_string())
                                && button(ui, "Delete share link").on_hover_text("Takes the uploaded copy off kappa.lol").clicked()
                            {
                                let c = ui.ctx().clone();
                                self.uploads.insert(path.clone(), upload::delete(path, move || c.request_repaint()));
                            }
                            if self.can_recycle()
                                && ui.add(egui::Button::new("Move to Recycle Bin").shortcut_text("Del")).on_hover_text("Deletes the clip, after asking; you can get it back from the Recycle Bin").clicked()
                            {
                                act.recycle = Some(path.clone());
                            }
                        })
                        .0
                        .on_hover_text("More: rename, tags, export, delete");
                        let (glyph, color, hint) = if item.favorite {
                            (icon::STAR_FILLED, pal().amber, "Starred. Click to unstar.")
                        } else {
                            (icon::STAR, pal().muted, "Star it: keeps it at the top and safe from auto-cleanup")
                        };
                        if ui.add(egui::Button::new(icon(glyph, 14.0).color(color)).frame(false)).on_hover_text(hint).clicked() {
                            act.star = Some(path.clone());
                        }
                    });
                });
            });
        })));
        if style.lip > 0.0 {
            ui.painter().set(under, egui::Shape::rect_filled(shown.response.rect.translate(egui::vec2(0.0, style.lip)), radius, pal().lip));
        }
        let mods = ui.input(|i| i.modifiers);
        if shown.response.clicked() && (mods.command || mods.shift) {
            act.select = Some((path.clone(), mods.shift));
        } else if shown.response.clicked() {
            self.selected = Some(path.clone());
            self.picked.clear();
        }
        let hovered = ui.rect_contains_pointer(shown.response.rect);
        if hovered != lifted {
            ui.ctx().data_mut(|d| d.insert_temp(hover_id, hovered));
            ui.ctx().request_repaint();
        }
        shown.response
    }

    /// The note being typed on a card, in place of the note line. Enter or clicking away keeps it
    /// (empty takes it off), Esc leaves it as it was.
    pub(crate) fn note_field(&mut self, ui: &mut egui::Ui) {
        let Some((path, text)) = &mut self.noting else { return };
        let edit = egui::TextEdit::singleline(text).char_limit(clips::NOTE_MAX).hint_text("Clutch 1v4 on B site").font(egui::TextStyle::Small).desired_width(ui.available_width());
        let edit = ui.add(edit).on_hover_text("A one-line note on the clip. Enter keeps it, Esc cancels.");
        if !edit.has_focus() && !edit.lost_focus() {
            edit.request_focus();
        }
        if edit.lost_focus() {
            if !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                clips::set_note(&mut self.notes, path, text);
                clips::save_notes(&self.notes);
                self.clip_cards.invalidate();
            }
            self.noting = None;
        }
    }

    /// Upload, its progress, or Copy link once uploaded.
    pub(crate) fn share_button(&mut self, ui: &mut egui::Ui, path: &std::path::Path, bytes: u64) {
        if let Some(job) = self.uploads.get(path) {
            use std::sync::atomic::Ordering::Relaxed;
            let (sent, total) = (job.sent.load(Relaxed), job.total.load(Relaxed).max(1));
            ui.label(RichText::new(if total > 1 { format!("Uploading {}%", sent * 100 / total) } else { "Working...".into() }).small().color(accent_light()));
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
            return;
        }
        if let Some(l) = self.links.get(&path.display().to_string()).cloned() {
            if cap(ui, true, icon_button(icon::SHARE, "Copy link").small()).on_hover_text(&l.link).clicked() {
                ui.ctx().copy_text(l.link.clone());
                self.clips_msg = Some(format!("Copied {}", l.link));
            }
        } else if bytes <= upload::MAX_BYTES
            && cap(ui, true, icon_button(icon::SHARE, "Share").small()).on_hover_text("Upload it and get a link (kappa.lol, up to 100 MB)").clicked() {
            self.confirm_upload = Some(path.to_path_buf());
        }
    }

    /// Makes a copy of `path` for sharing in the background, next to it.
    pub(crate) fn start_export(&mut self, path: &std::path::Path, share: media::Share, ctx: &egui::Context) {
        let src = path.to_path_buf();
        let what = format!("Exported {} to", src.display());
        let job = ExportJob::spawn(clips::copy_name(path, share.label()), what, ctx, move |job, out| media::export(&src, out, share, &|f| job.progress(f)));
        self.exports.insert(path.to_path_buf(), job);
    }

    /// Picks up finished exports: says where the copy went and offers to copy it. Then starts the
    /// next one waiting, once none is running.
    pub(crate) fn finish_exports(&mut self, ctx: &egui::Context) {
        let done: Vec<_> = self.exports.iter().filter_map(|(p, j)| j.done.lock().unwrap().take().map(|r| (p.clone(), r))).collect();
        for (path, result) in done {
            self.exports.remove(&path);
            match result {
                Ok(out) => self.show_saved(out),
                Err(e) => self.clips_msg = Some(format!("Couldn't export it: {e}")),
            }
        }
        if self.exports.is_empty()
            && let Some((path, share)) = self.export_queue.pop_front()
        {
            self.start_export(&path, share, ctx);
        }
    }

    /// Says a new file was saved, with a Copy button for it, and lists it.
    pub(crate) fn show_saved(&mut self, out: std::path::PathBuf) {
        let msg = saved_note(&out);
        self.clips_msg = Some(msg.clone());
        self.exported = Some((msg, out));
        self.rescan_clips();
    }

    /// Picks up a finished montage: says where it went (and leaves montage mode), or why it wasn't made.
    pub(crate) fn finish_montage(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        let finished = self.montage.as_ref().and_then(|m| m.job.as_ref()).and_then(|j| Some((j.done.lock().unwrap().take()?, j.stop.load(Relaxed))));
        let Some(m) = &mut self.montage else { return };
        match finished {
            Some((Ok(out), _)) => {
                self.show_saved(out);
                self.montage = None;
            }
            Some((Err(e), stopped)) => {
                self.clips_msg = Some(if stopped { "Montage stopped.".into() } else { format!("Couldn't make the montage: {e}") });
                m.job = None;
            }
            None => {}
        }
    }

    /// Montage mode: the clips picked, in order (each can move up, down or off), fades, music, and
    /// Make montage, which joins them into a new clip in the clips folder in the background.
    pub(crate) fn montage_panel(&mut self, ui: &mut egui::Ui) {
        use std::sync::atomic::Ordering::Relaxed;
        let Some(m) = &mut self.montage else { return };
        let busy = m.job.is_some();
        card(ui, "Montage", |ui| {
            if m.clips.is_empty() {
                ui.label(RichText::new("Click clips to add them in the order they should play, or pick one with the arrow keys and press Space. Two clips without fades are simply joined into one.").color(pal().muted));
            }
            // A clip to move up (-1), down (1) or take out (0).
            let mut change = None;
            let n = m.clips.len();
            for (i, path) in m.clips.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{}.", i + 1)).color(pal().muted));
                    ui.label(path.file_stem().unwrap_or_default().to_string_lossy());
                    if let Some(len) = self.lengths.get(path) {
                        ui.label(RichText::new(mmss(len.round() as u32)).small().color(pal().muted));
                    }
                    if bare_icon(ui, i > 0 && !busy, icon::UP, "Earlier") {
                        change = Some((i, -1));
                    }
                    if bare_icon(ui, i + 1 < n && !busy, icon::DOWN, "Later") {
                        change = Some((i, 1));
                    }
                    if bare_icon(ui, !busy, icon::CANCEL, "Take it out") {
                        change = Some((i, 0));
                    }
                });
            }
            match change {
                Some((i, 0)) => _ = m.clips.remove(i),
                Some((i, d)) => m.clips.swap(i, i.saturating_add_signed(d)),
                None => {}
            }
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled(!busy, egui::Checkbox::new(&mut m.fades, "Fade between clips")).on_hover_text("Blends each clip into the next instead of a hard cut");
                ui.separator();
                let name = m.music.as_ref().map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned());
                let pick = cap(ui, !busy, icon_button(icon::MUSIC, name.as_deref().unwrap_or("Add music"))).on_hover_text("An MP3, M4A or WAV under the clips' sound, looped or cut to fit, faded out at the end");
                if pick.clicked()
                    && let Some(f) = overlay::file_dialog().set_title("Music for the montage").add_filter("Music", &["mp3", "m4a", "wav"]).pick_file()
                {
                    m.music = Some(f);
                }
                if m.music.is_some() {
                    if bare_icon(ui, !busy, icon::CANCEL, "No music") {
                        m.music = None;
                    }
                    let mut pct = m.volume * 100.0;
                    if ui.add_enabled(!busy, egui::Slider::new(&mut pct, 0.0..=100.0).suffix("% music").integer()).on_hover_text("How loud the music is under the clips' sound").changed() {
                        m.volume = pct / 100.0;
                    }
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let text = match &m.job {
                    Some(j) => format!("Making montage {}%", j.pct.load(Relaxed)),
                    None => "Make montage".into(),
                };
                if primary(ui, !busy && !m.clips.is_empty(), RichText::new(text).strong(), egui::Vec2::ZERO).on_hover_text("Joins the clips into one new video in your clips folder").clicked() {
                    let out = clips::montage_name(std::path::Path::new(&self.saved.folder), std::time::SystemTime::now());
                    let music = m.music.clone().map(|path| media::Music { path, volume: m.volume });
                    let (list, fades) = (m.clips.clone(), m.fades);
                    let what = format!("Made a montage of {} clips at", list.len());
                    m.job = Some(ExportJob::spawn(out, what, ui.ctx(), move |job, out| {
                        // Each whole clip with track 1's sound, the way it plays.
                        let parts = list.iter().map(|p| Ok(media::Part { path: p.clone(), start: 0.0, end: f64::INFINITY, gains: media::track1(media::audio_tracks(p)?) }));
                        let parts = parts.collect::<anyhow::Result<Vec<_>>>()?;
                        media::join(&parts, out, fades, music.as_ref(), &|f| job.progress(f), &|| job.stop.load(Relaxed))
                    }));
                }
                if let Some(j) = &m.job {
                    if button(ui, "Stop").on_hover_text("Stops making the montage").clicked() {
                        j.stop.store(true, Relaxed);
                    }
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
                }
            });
        });
    }

    /// Picks up finished uploads and deletes.
    pub(crate) fn finish_uploads(&mut self, ctx: &egui::Context) {
        let done: Vec<(std::path::PathBuf, Result<String, String>)> =
            self.uploads.iter().filter_map(|(p, j)| j.done.lock().unwrap().take().map(|r| (p.clone(), r))).collect();
        for (path, result) in done {
            self.uploads.remove(&path);
            if let Ok(link) = &result
                && link.starts_with("https://")
            {
                // `path` is where the clip is now (`follow_clip`).
                upload::keep_at(link, &path);
            }
            self.links = upload::links();
            self.clips_msg = Some(match result {
                Ok(s) if s == "deleted" => "Link deleted. The clip is no longer on kappa.lol.".into(),
                Ok(s) if s == "posted" => format!("Posted {} to Discord.", path.file_name().unwrap_or_default().to_string_lossy()),
                Ok(link) => {
                    ctx.copy_text(link.clone());
                    format!("Uploaded. Link copied: {link}")
                }
                Err(e) => e,
            });
        }
        let lc = &mut self.link_cleanup;
        if let Some(result) = lc.job.as_ref().and_then(|j| j.done.lock().unwrap().take()) {
            lc.job = None;
            lc.msg = result.as_ref().err().cloned();
            self.clips_msg = Some(match result {
                Ok(n) if n == "0" => "No share links needed deleting.".into(),
                Ok(n) if n == "1" => "Deleted 1 share link. That clip is no longer on kappa.lol.".into(),
                Ok(n) => format!("Deleted {n} share links. Those clips are no longer on kappa.lol."),
                Err(e) => e,
            });
            self.links = upload::links();
            self.note_orphans();
        }
        let lc = &mut self.link_cleanup;
        if lc.job.is_none() && !lc.waiting.is_empty() {
            let c = ctx.clone();
            lc.job = Some(upload::delete_gone(std::mem::take(&mut lc.waiting), move || c.request_repaint()));
        }
    }

    /// Renames a clip, keeping its star, tags and share link, and its place in the montage, the
    /// selection and the export queue.
    pub(crate) fn rename_clip(&mut self, path: &std::path::Path, name: &str) {
        // The player holds the file open.
        if self.player.as_ref().is_some_and(|(p, _)| p == path) {
            self.player = None;
            self.trim = None;
        }
        self.clips_msg = Some(match clips::rename(path, name) {
            Ok(new) => {
                self.carry_clip(path, Some(&new), true);
                log::write(&format!("Renamed {} to {}", path.display(), new.display()));
                format!("Renamed to {}.", new.file_name().unwrap_or_default().to_string_lossy())
            }
            Err(e) => format!("Couldn't rename it: {e:#}"),
        });
        self.rescan_clips();
    }

    /// The tags on clips in the clips folder, sorted. Tags of clips that are gone (auto-cleanup,
    /// archive) stay in tags.json but aren't offered.
    pub(crate) fn tags_in_use(&self) -> Vec<String> {
        let paths: std::collections::HashSet<&std::ffi::OsStr> = self.clip_list.iter().map(|c| c.path.as_os_str()).collect();
        let listed = |p: &String| paths.contains(std::ffi::OsStr::new(p.as_str()));
        let mut tags: Vec<String> = self.tags.iter().filter(|(p, _)| listed(p)).flat_map(|(_, t)| t).cloned().collect();
        tags.sort_by_key(|t| t.to_lowercase());
        tags.dedup();
        tags
    }

    /// What's in a Tags menu, on a clip's menu or the selection bar: a tick per tag in use (ticked
    /// when every clip in `paths` has it) and New tag. Returns a tag ticked on (true) or off.
    pub(crate) fn tags_menu(&mut self, ui: &mut egui::Ui, paths: &[std::path::PathBuf]) -> Option<(String, bool)> {
        let many = paths.len() > 1;
        let mut ticked = None;
        for t in self.tags_in_use() {
            let mut on = paths.iter().all(|p| self.tags.get(&p.display().to_string()).is_some_and(|s| s.contains(&t)));
            if ui.checkbox(&mut on, t.as_str()).on_hover_text(if many { "Tick to give the clips picked this tag" } else { "Tick to give the clip this tag" }).clicked() {
                ticked = Some((t, on));
            }
        }
        if button(ui, "New tag...").on_hover_text(if many { "Makes a new tag and gives it to the clips picked" } else { "Makes a new tag and gives it to this clip" }).clicked() {
            self.tagging = Some((paths.to_vec(), String::new()));
        }
        ticked
    }

    /// Opens the rename box for `path`, with its current name.
    pub(crate) fn start_rename(&mut self, path: &std::path::Path) {
        let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        self.renaming = Some((path.to_path_buf(), stem));
    }
}

#[cfg(test)]
mod tests {
    use super::scan_due;
    use std::time::Duration;

    #[test]
    fn scan_due_after_rescan_or_three_seconds() {
        assert!(!scan_due(false, Duration::ZERO));
        assert!(!scan_due(false, Duration::from_millis(2999)));
        assert!(scan_due(false, Duration::from_secs(3)));
        // Asked for right away, however soon after the last read (no clock stepped back).
        assert!(scan_due(true, Duration::ZERO));
    }

    /// A running upload follows its clip when it's renamed or archived, so its link is filed
    /// where the clip is once done. Recycled, it stays put, so the link shows as an orphan.
    #[test]
    fn a_running_upload_follows_its_clip() {
        use crate::*;
        let (old, new) = (std::path::PathBuf::from(r"D:\Clips\a.mp4"), std::path::PathBuf::from(r"D:\Clips\b.mp4"));
        let job = std::sync::Arc::new(upload::Job::default());
        let mut h = testing::page(|app| _ = app.uploads.insert(old.clone(), job), |_, _| {});
        h.state_mut().follow_clip(&old, Some(&new));
        assert!(h.state().uploads.contains_key(&new) && !h.state().uploads.contains_key(&old));
        h.state_mut().follow_clip(&new, None);
        assert!(h.state().uploads.contains_key(&new));
    }

    /// A clip moved in the window drops the frames and lengths of its old and new paths (another
    /// file can have either name), and a read of the folder begun before the move is dropped
    /// instead of showing the clip where it was. In a temp folder.
    #[test]
    fn a_move_drops_old_frames_and_reads() {
        use crate::*;
        let dir = std::env::temp_dir().join("instant-replay-move-drops");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        std::fs::write(&a, b"not really a picture").unwrap();
        let folder = dir.display().to_string();
        let mut h = testing::page(|app| app.saved.folder = folder, |_, _| {});
        let ctx = h.ctx.clone();
        let app = h.state_mut();
        app.scan_clips(&ctx);
        assert_eq!(app.clip_list.len(), 1);
        let stale = ClipsScan::read(app.saved.folder.clone(), app.refind_job(), app.clip_moves);
        for p in [&a, &b] {
            app.textures.insert((p.clone(), 10), None);
            app.lengths.insert(p.clone(), 3.0);
        }
        std::fs::rename(&a, &b).unwrap();
        app.clip_moved(&a, Some(&b));
        assert!(app.textures.len() == 0 && app.lengths.is_empty(), "frames of both names dropped");
        app.set_clip_list(Vec::new());
        app.take_scan(stale);
        assert!(app.clip_list.is_empty(), "the read from before the move is dropped");
        app.scan_clips(&ctx);
        assert_eq!(app.clip_list.iter().map(|c| c.path.as_path()).collect::<Vec<_>>(), [b.as_path()], "a new read is taken");
        drop(h);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A scan offers the share links of clips missing from a clips folder it read, and none while
    /// the folder can't be read (a NAS asleep, a drive unplugged): then General > Sharing hides
    /// them. In a temp folder.
    #[test]
    fn orphans_only_from_a_folder_that_reads() {
        use crate::*;
        let dir = std::env::temp_dir().join(format!("instant-replay-orphans-{}", config::random_hex()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("here.mp4"), b"x").unwrap();
        let link = |n: &str| (dir.join(n).display().to_string(), upload::Link { link: format!("https://kappa.lol/{n}"), key: n.into() });
        let (folder, links) = (dir.display().to_string(), [link("here.mp4"), link("gone.mp4")]);
        let mut h = testing::page(
            |app| {
                app.saved.folder = folder;
                app.links = links.into();
            },
            |_, _| {},
        );
        let ctx = h.ctx.clone();
        let app = h.state_mut();
        app.scan_clips(&ctx);
        assert_eq!(app.link_cleanup.orphans, [dir.join("gone.mp4").display().to_string()]);
        std::fs::remove_dir_all(&dir).unwrap();
        app.rescan_clips();
        app.scan_clips(&ctx);
        assert!(app.clip_list.is_empty(), "nothing to show, as before");
        assert!(app.link_cleanup.orphans.is_empty(), "the clips folder can't be read, so no link is offered");
    }

    /// The cards keep the 60-clip cap with every starred clip on top, and are made again only when
    /// the view changes or they're invalidated, not every frame. Made-up clips; nothing is read.
    #[test]
    fn cards_are_cached_until_something_changes() {
        use crate::*;
        let at = |s| std::time::UNIX_EPOCH + std::time::Duration::from_secs(s);
        let list: Vec<clips::Clip> = (0..70u64).rev().map(|i| clips::Clip { path: format!(r"D:\Clips\{i}.mp4").into(), name: format!("{i}.mp4"), bytes: 1, modified: at(1_000_000 + i), group: None, is_picture: false }).collect();
        let mut h = testing::page(|app| app.set_clip_list(list), |_, _| {});
        let app = h.state_mut();
        app.favorites.insert(r"D:\Clips\3.mp4".into());
        let cards = app.cards(false);
        assert_eq!(cards.shown.len(), 61, "the first 60 and the starred one further down");
        assert_eq!(cards.days[0].0, "Starred");
        assert_eq!(cards.shown[0], std::path::PathBuf::from(r"D:\Clips\3.mp4"));
        // Starred again without saying so: the same cards.
        app.favorites.insert(r"D:\Clips\2.mp4".into());
        assert!(std::rc::Rc::ptr_eq(&cards, &app.cards(false)), "made once");
        app.clip_cards.invalidate();
        assert_eq!(app.cards(false).shown.len(), 62);
        app.view = ClipsView::Starred;
        assert_eq!(app.cards(false).shown.len(), 2, "a new view: made again");
        app.search = "2.mp4".into();
        assert_eq!(app.cards(false).shown.len(), 1);
    }

    /// Past the limit, the thumbnails drawn least recently go; scrub frames are left to their own
    /// limit. No real textures.
    #[test]
    fn least_used_thumbnails_go_first() {
        let key = |i: usize, t: u32| (std::path::PathBuf::from(format!("{i}.mp4")), t);
        let mut t = super::Textures::default();
        t.insert(key(0, 55), None);
        for i in 0..=super::THUMBS_MAX {
            t.insert(key(i, 10), None);
        }
        // The first ones drawn again just now.
        for i in 0..10 {
            assert!(t.get(&key(i, 10)).is_some());
        }
        t.trim_thumbnails();
        assert_eq!(t.len(), super::THUMBS_KEPT + 1);
        assert!((0..10).all(|i| t.contains_key(&key(i, 10))), "recently drawn: kept");
        assert!(!t.contains_key(&key(10, 10)), "least recently drawn: gone");
        assert!(t.contains_key(&key(0, 55)), "a scrub frame isn't a thumbnail");
    }

    /// The Recycle Bin dialog offers to delete share links (on by default) only when a picked clip
    /// has one.
    #[test]
    fn recycle_dialog_offers_to_delete_links() {
        use crate::*;
        use egui_kittest::kittest::Queryable;
        let (linked, plain) = (std::path::PathBuf::from(r"D:\Clips\a.mp4"), std::path::PathBuf::from(r"D:\Clips\b.mp4"));
        let link = upload::Link { link: "https://kappa.lol/a".into(), key: "k".into() };
        let key = linked.display().to_string();
        let paths = vec![plain.clone()];
        let mut h = testing::page(|app| _ = app.links.insert(key.clone(), link.clone()), move |ui, app| app.recycle_links_box(ui, &paths));
        h.run();
        assert!(h.query_by_label_contains("share link").is_none());
        let paths = vec![plain, linked];
        let mut h = testing::page(|app| _ = app.links.insert(key, link), move |ui, app| app.recycle_links_box(ui, &paths));
        h.run();
        assert!(h.state().link_cleanup.with_recycle, "on by default");
        h.get_by_label("Also delete their share links").click();
        h.run();
        assert!(!h.state().link_cleanup.with_recycle, "unticked");
    }
}
