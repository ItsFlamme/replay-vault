//! The General page: clip length, folders, cleanup, archive, appearance, and settings export and import.

use crate::*;

/// A limit that's off (0) until ticked, then starts at `start`. `about` says how the rule works,
/// on hover.
pub(crate) fn limit(ui: &mut egui::Ui, value: &mut u32, text: &str, unit: &str, start: u32, max: u32, about: &str) {
    ui.horizontal(|ui| {
        let mut on = *value > 0;
        if ui.checkbox(&mut on, text).on_hover_text(format!("Tick to turn this rule on. {about}")).changed() {
            *value = if on { start } else { 0 };
        }
        if on {
            ui.add(egui::DragValue::new(value).range(1..=max).suffix(format!(" {unit}"))).on_hover_text("Drag or click to type the number");
        }
    });
}

/// The Appearance card (on General): the theme, light or dark, and the accent
/// color. Changes show right away and are kept with Save.
pub(crate) fn appearance(ui: &mut egui::Ui, cfg: &mut Config) {
    card(ui, "Appearance", |ui| {
        row(ui, "Theme", |ui| {
            ui.horizontal_wrapped(|ui| {
                let theme = if cfg.theme == "retro" { "retro" } else { "minimalist" };
                let mut pick = theme;
                chip(ui, &mut pick, "minimalist", "Minimalist").on_hover_text("Clean and flat: buttons look like keyboard keys");
                chip(ui, &mut pick, "retro", "Retro").on_hover_text("The 80s video tape look: tape labels, chunky buttons and a VCR font");
                if pick != theme {
                    cfg.theme = pick.into();
                }
            });
        });
        row(ui, "Mode", |ui| {
            ui.horizontal_wrapped(|ui| {
                chip(ui, &mut cfg.light_mode, false, "Dark").on_hover_text("Light text on dark backgrounds");
                chip(ui, &mut cfg.light_mode, true, "Light").on_hover_text("Dark text on light backgrounds");
            });
        });
        if cfg.theme == "retro" {
            row(ui, "Effects", |ui| {
                ui.checkbox(&mut cfg.reduce_effects, "Reduce effects").on_hover_text("Turns off the scanlines, grain and glows, for a calmer, flat background");
            });
        }
        row(ui, "Accent color", |ui| {
            ui.horizontal_wrapped(|ui| {
                for (i, (name, ..)) in ACCENTS.iter().enumerate() {
                    chip(ui, &mut cfg.accent, i as u8, *name).on_hover_text("Use this color for buttons and highlights");
                }
                chip(ui, &mut cfg.accent, CUSTOM_ACCENT, "Custom").on_hover_text("Pick any color for buttons and highlights");
            });
        });
        if cfg.accent == CUSTOM_ACCENT {
            row(ui, "Custom color", |ui| {
                ui.horizontal(|ui| {
                    let mut color = parse_hex(&cfg.custom_accent).unwrap_or(ACCENTS[0].1);
                    if egui::color_picker::color_edit_button_srgba(ui, &mut color, egui::color_picker::Alpha::Opaque).on_hover_text("Click to pick a color from the color wheel").changed() {
                        cfg.custom_accent = hex(color);
                    }
                    // What's typed stays while the box has focus, even when it isn't a color yet;
                    // otherwise it shows the color in use.
                    let id = ui.scope_id().with("custom accent hex");
                    let mut typed = ui.ctx().data(|d| d.get_temp::<String>(id)).unwrap_or_else(|| cfg.custom_accent.clone());
                    let valid = parse_hex(&typed);
                    // Wide enough for the widest #RRGGBB in this font, its margins and the cursor.
                    let font = egui::TextStyle::Body.resolve(ui.style());
                    let width = ui.painter().layout_no_wrap("#WWWWWW".into(), font, pal().text).size().x + 2.0 * 8.0 + 4.0;
                    let edit = egui::TextEdit::singleline(&mut typed).char_limit(7).font(egui::TextStyle::Body).margin(egui::vec2(8.0, 4.0)).desired_width(width).text_color(if valid.is_some() { pal().text } else { pal().red });
                    let r = ui.add(edit).on_hover_text("Type a color as #RRGGBB, like #8448F0");
                    match parse_hex(&typed) {
                        Some(c) if r.changed() => cfg.custom_accent = hex(c),
                        Some(_) => {}
                        None => _ = ui.label(RichText::new("Not a color: use #RRGGBB").small().color(pal().red)),
                    }
                    if let Some(c) = parse_hex(&cfg.custom_accent)
                        && fit_accent(c, pal()) != c
                    {
                        let how = if luminance(fit_accent(c, pal())) > luminance(c) { "lighter" } else { "darker" };
                        let mode = if pal().light { "light" } else { "dark" };
                        ui.label(RichText::new(format!("Shown a little {how} in {mode} mode so it stays readable")).small().color(pal().muted));
                    }
                    if !r.has_focus() {
                        typed = cfg.custom_accent.clone();
                    }
                    ui.ctx().data_mut(|d| d.insert_temp(id, typed));
                });
            });
        }
    });
}

/// General's tabs, so its settings aren't one long scroll.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum GeneralPart {
    Clips,
    Storage,
    Appearance,
    Startup,
    Sharing,
    Backup,
}

/// The Discord webhook box's hover text: what it's for, where to get one, and that it's private.
pub(crate) const WEBHOOK_ABOUT: &str = "Paste your Discord channel's webhook link here, for Post to Discord in a clip's menu. In Discord: Edit Channel > Integrations > Webhooks > New Webhook > Copy Webhook URL. Keep it private: anyone with it can post there.";

/// Use this folder anyway's hover text: what auto-cleanup and the archive would then do there.
const USE_ANYWAY: &str = "Lets auto-cleanup and the archive work in this folder after all. Every video and picture in it, and in the folders right inside it, then counts as a clip, including your own: auto-cleanup can move any of them to the Recycle Bin, and the archive can move them to the archive folder. Only for this folder; pick another and you're asked again. Takes effect when you Save.";

/// The clips folder risk worked out off the UI thread (see `App::clips_folder_risk`): the
/// (folder, confirmed folder) to work out next, whether a thread is on it, and the last answer
/// with what it was for.
struct RiskJob {
    want: Option<(String, String)>,
    busy: bool,
    done: Option<((String, String), Option<String>)>,
}

impl RiskJob {
    /// Wants `key` worked out next, over any key wanted before. True if no thread is on it, so
    /// one must start.
    fn want(&mut self, key: (String, String)) -> bool {
        self.want = Some(key);
        !std::mem::replace(&mut self.busy, true)
    }

    /// The next key for the thread, or none: then the thread is done (in the same lock, so none
    /// is missed).
    fn next(&mut self) -> Option<(String, String)> {
        let next = self.want.take();
        self.busy = next.is_some();
        next
    }

    /// The last answer, if it's for `key`. An answer for a folder since changed is dropped; the
    /// newer one is on its way.
    fn answer_for(&mut self, key: &(String, String)) -> Option<Option<String>> {
        self.done.take().filter(|(k, _)| k == key).map(|(_, why)| why)
    }
}

static RISK: std::sync::Mutex<RiskJob> = std::sync::Mutex::new(RiskJob { want: None, busy: false, done: None });

/// Has `clips::folder_risk` worked out for `key` on a thread, then repaints. One thread at a
/// time, taking the newest key wanted, so typing a folder on a slow NAS doesn't pile them up.
fn want_risk(key: (String, String), ctx: &egui::Context) {
    if !RISK.lock().unwrap().want(key) {
        return;
    }
    let ctx = ctx.clone();
    // A closure, so the lock is let go before the work: `while let` would hold it through the loop.
    let next = || RISK.lock().unwrap().next();
    std::thread::spawn(move || {
        while let Some((folder, ok)) = next() {
            let why = clips::folder_risk(&folder, &ok);
            RISK.lock().unwrap().done = Some(((folder, ok), why));
            ctx.request_repaint();
        }
    });
}

/// Remove from this PC's hover text.
pub(crate) const REMOVE_ABOUT: &str = "Puts back everything the app changed on this PC, moves its private OBS copy and settings to the Recycle Bin and uninstalls the app. Asks first; your clips stay.";

impl GeneralPart {
    const ALL: [(GeneralPart, &str, &str); 6] = [
        (GeneralPart::Clips, "Clips", "Clip length, the clips folder, what a save does and clip names"),
        (GeneralPart::Storage, "Storage", "Auto-cleanup and the archive drive"),
        (GeneralPart::Appearance, "Appearance", "Theme, light or dark, and the accent color"),
        (GeneralPart::Startup, "Startup", "Starting with Windows, updates and Windows settings"),
        (GeneralPart::Sharing, "Sharing", "Your Discord webhook"),
        (GeneralPart::Backup, "Backup", "Move your settings to a new PC, or remove the app"),
    ];
}

impl App {
    /// The clips in `folder`: the last read of the clips folder when it's that one, else read now.
    fn listed(&self, folder: &str) -> std::borrow::Cow<'_, [clips::Clip]> {
        if folder == self.saved.folder { std::borrow::Cow::Borrowed(&self.clip_list) } else { std::borrow::Cow::Owned(clips::list(folder, usize::MAX)) }
    }

    /// General, in tabs: Clips (length, folder, game folders, what a save does, names and covers),
    /// Storage (cleanup, archive), Appearance (the look), Startup (startup, updates, Windows),
    /// Sharing (Discord) and Backup (moving PCs, Remove).
    pub(crate) fn general_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            for (part, name, about) in GeneralPart::ALL {
                // Backup is hidden during setup, along with its cards.
                if part == GeneralPart::Backup && self.wizard.is_some() {
                    continue;
                }
                chip(ui, &mut self.general_part, part, name).on_hover_text(about);
            }
        });
        ui.add_space(12.0);
        match self.general_part {
            GeneralPart::Clips => {
                self.general_clips(ui);
                self.names_card(ui);
            }
            GeneralPart::Storage => self.storage_cards(ui),
            GeneralPart::Appearance => appearance(ui, &mut self.cfg),
            GeneralPart::Startup => {
                self.startup_cards(ui);
                self.windows_card(ui);
            }
            GeneralPart::Sharing => {
                self.discord_card(ui);
                self.share_links_card(ui);
            }
            GeneralPart::Backup => self.backup_cards(ui),
        }
    }

    /// Why auto-cleanup and the archive won't work in the clips folder being set
    /// (`clips::folder_risk`), looked up again only when the folder typed, or the one confirmed
    /// with Use this folder anyway, changes. That follows links, which can wait on a NAS, so it's
    /// worked out off the UI thread (`want_risk`); until it's back, what the folders as typed say.
    fn clips_folder_risk(&mut self, ctx: &egui::Context) -> Option<String> {
        let (folder, ok) = (&self.cfg.folder, &self.cfg.shared_folder_ok);
        if self.folder_risk.as_ref().is_none_or(|((f, o), _)| f != folder || o != ok) {
            let key = (folder.clone(), ok.clone());
            if cfg!(test) {
                // Tests read the folders in place, so a frame shows the answer.
                self.folder_risk = Some((key, clips::folder_risk(folder, ok)));
            } else {
                self.folder_risk = Some((key.clone(), clips::rules_off(folder, folder, ok, clips::known_folders())));
                want_risk(key, ctx);
            }
        }
        if let Some((key, _)) = &self.folder_risk
            && let Some(why) = RISK.lock().unwrap().answer_for(key)
        {
            self.folder_risk = Some((key.clone(), why));
        }
        self.folder_risk.as_ref().and_then(|(_, why)| why.clone())
    }

    /// Under the clips folder (here and in setup): why auto-cleanup and the archive won't run in it.
    pub(crate) fn clips_folder_note(&mut self, ui: &mut egui::Ui) {
        if let Some(why) = self.clips_folder_risk(ui.ctx()) {
            let text = format!("This is {why}. Auto-cleanup and the archive won't run in it, as they would take your own videos and pictures too. Pick a folder just for clips, like a new one inside it.");
            ui.label(RichText::new(text).small().color(pal().amber));
        }
    }

    /// Under the Auto-cleanup or Archive card's "is off" line: lets both work in the clips folder
    /// anyway. It's a setting like the others, kept with Save.
    fn use_folder_anyway(&mut self, ui: &mut egui::Ui) {
        if button(ui, "Use this folder anyway").on_hover_text(USE_ANYWAY).clicked() {
            self.cfg.shared_folder_ok = self.cfg.folder.trim().to_string();
        }
    }

    fn general_clips(&mut self, ui: &mut egui::Ui) {
        card(ui, "Clips", |ui| {
            row(ui, "Clip length", |ui| self.length_chips(ui));
            row(ui, "Clips folder", |ui| folder_field(ui, &mut self.cfg.folder, ""));
            self.clips_folder_note(ui);
            ui.checkbox(&mut self.cfg.sort_by_game, "Sort clips into a folder per game")
                .on_hover_text("Named after the title of the window in front when you save. Only the title is read; the game itself is never opened or touched.");
            ui.checkbox(&mut self.cfg.pause_without_game, "Pause instant replay when no game is open").on_hover_text(
                "After 2 minutes with no game in front, replay stops to save memory and power, and starts again when a game is in front. A game is something full screen, or a window you've saved clips from before. For a game in a smaller window, play it, then pick Count last window as a game in the tray menu. Leave this off to clip anything on your desktop.",
            );
        });
        card(ui, "When a clip is saved", |ui| {
            ui.checkbox(&mut self.cfg.save_popup, "Show a popup").on_hover_text("A small note in the corner of the screen when a clip is saved");
            ui.checkbox(&mut self.cfg.save_sound, "Play a soft sound (screenshots too)").on_hover_text("A quiet sound, so you know it worked without looking");
        });
    }

    fn startup_cards(&mut self, ui: &mut egui::Ui) {
        card(ui, "Startup", |ui| {
            self.startup_checkbox(ui);
            let updates = crate::instance::current().updates_allowed();
            let why = crate::instance::UPDATES_OFF;
            allowed_checkbox(ui, updates, &mut self.cfg.check_updates, "Tell me when a new version is out", why)
                .on_hover_text("Asks GitHub once a day whether a newer Replay Vault is out. Nothing about you or this PC is sent, and nothing is downloaded until you choose Update now.");
            ui.horizontal(|ui| {
                let check = ui.add_enabled_ui(updates, |ui| button(ui, "Check for updates")).inner;
                if check.on_hover_text("Asks GitHub now whether a newer Replay Vault is out").on_disabled_hover_text(why).clicked() {
                    let c = ui.ctx().clone();
                    crate::app_update::check_now(move || c.request_repaint());
                }
                if let Some(s) = crate::app_update::status() {
                    ui.label(RichText::new(s).color(pal().muted));
                }
            });
        });
    }

    fn names_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "Clip names and covers", |ui| {
            row(ui, "Date style", |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (i, (_, example)) in recorder::NAME_STYLES.iter().enumerate() {
                        chip(ui, &mut self.cfg.name_style, i as u8, *example).on_hover_text("Write the date in clip names like this");
                    }
                });
            });
            ui.label(RichText::new(format!("Clips are named like: {}", recorder::example_name(&self.cfg))).small().color(pal().muted));
            ui.checkbox(&mut self.cfg.steam_covers, "Look up game covers online (Steam)").on_hover_text(
                "Sends each game folder's name (like \"Elden Ring\"), and your IP address as with any web request, to Steam's store search, then downloads the cover picture. Each name is asked once (one with no match again after a week). Names that look like a web page, a file, an email address or a non-game app (browser, Explorer, Discord, editors) are never sent.",
            );
        });
    }

    fn storage_cards(&mut self, ui: &mut egui::Ui) {
        card(ui, "Auto-cleanup", |ui| {
            let rules = "Oldest first, to the Recycle Bin. Starred clips are never removed and don't count toward the size. Clips from the last 10 minutes are left alone.";
            limit(ui, &mut self.cfg.keep_under_gb, "Keep clips under", "GB", 100, 10_000, rules);
            limit(ui, &mut self.cfg.keep_days, "Remove clips older than", "days", 30, 3650, rules);
            let rule = cleanup_rule(&self.cfg);
            let lost = clips::lost_stars(&self.lost, &self.favorites);
            let risk = if rule == clips::Cleanup::default() { None } else { self.clips_folder_risk(ui.ctx()) };
            if let Some(why) = risk {
                let text = format!("Auto-cleanup is off: the clips folder ({}) is {why}, so it would remove your own videos and pictures too. Pick a folder just for clips on the Clips tab.", self.cfg.folder.trim());
                ui.label(RichText::new(text).small().color(pal().amber));
                self.use_folder_anyway(ui);
            } else if rule != clips::Cleanup::default() && self.unfiled {
                let text = "Auto-cleanup is on hold: your stars couldn't be saved for the new clips folder, so it can't tell starred clips there from the rest. Restart the app to try again.";
                ui.label(RichText::new(text).small().color(pal().amber));
            } else if rule != clips::Cleanup::default() && lost > 0 {
                let text = format!(
                    "Auto-cleanup is on hold: {} from your earlier clips folder {} found in this one, and cleanup couldn't tell {} from the rest. Once {} in the clips folder (renamed or in a game folder is fine), cleanup goes on by itself.",
                    count(lost, "starred clip"),
                    if lost == 1 { "wasn't" } else { "weren't" },
                    if lost == 1 { "it" } else { "them" },
                    if lost == 1 { "it's" } else { "they're" },
                );
                ui.label(RichText::new(text).small().color(pal().amber));
                let (label, about) = if lost == 1 {
                    ("Go on without it", "Auto-cleanup runs again. If that clip is in the clips folder, it may be removed, as it no longer counts as starred.")
                } else {
                    ("Go on without them", "Auto-cleanup runs again. If those clips are in the clips folder, they may be removed, as they no longer count as starred.")
                };
                if button(ui, label).on_hover_text(about).clicked() {
                    self.forget_lost();
                }
            } else if rule != clips::Cleanup::default() {
                let stale = self.cleanup_preview.is_none_or(|(r, at, ..)| r != rule || at.elapsed() > std::time::Duration::from_secs(5));
                if stale {
                    let all = self.listed(&self.cfg.folder);
                    let plan = clips::cleanup_plan(&all, &self.favorites, rule, std::time::SystemTime::now());
                    let planned: std::collections::HashSet<&std::path::PathBuf> = plan.iter().collect();
                    let bytes = all.iter().filter(|c| planned.contains(&c.path)).map(|c| c.bytes).sum();
                    self.cleanup_preview = Some((rule, std::time::Instant::now(), plan.len(), bytes));
                }
                ui.ctx().request_repaint_after(std::time::Duration::from_secs(5));
                if let Some((_, _, n, bytes)) = self.cleanup_preview {
                    let text = if n == 0 { "Nothing to remove right now.".to_string() } else { format!("Right now this would move {} ({}) to the Recycle Bin.", count(n, "clip"), clips::size(bytes)) };
                    ui.label(RichText::new(text).small().color(if n == 0 { pal().muted } else { pal().amber }));
                }
            }
        });
        card(ui, "Archive", |ui| {
            let rules = "Oldest first, into the same game folders in the archive folder. Stars, tags, notes and share links go along. Clips from the last 10 minutes are left alone.";
            limit(ui, &mut self.cfg.archive_days, "Move clips older than", "days", 30, 3650, rules);
            if self.cfg.archive_days > 0 {
                row(ui, "Archive folder", |ui| folder_field(ui, &mut self.cfg.archive_folder, "Another drive or a NAS folder"));
                let risk = self.clips_folder_risk(ui.ctx());
                let off = risk.is_some() && archive_rule(&self.cfg).is_some();
                let (text, color) = match archive_rule(&self.cfg) {
                    Some(_) if let Some(why) = risk => (format!("The archive is off: the clips folder ({}) is {why}, so it would move your own videos and pictures too. Pick a folder just for clips on the Clips tab.", self.cfg.folder.trim()), pal().amber),
                    None if self.cfg.archive_folder.trim().is_empty() => ("Pick a folder to move them to.".to_string(), pal().muted),
                    None => ("Pick a folder outside the clips folder.".to_string(), pal().amber),
                    Some(_) if self.cfg == self.saved && clips::ARCHIVE_MISSING.load(std::sync::atomic::Ordering::Relaxed) => {
                        ("That folder wasn't there last time (drive or NAS off?). Clips wait until it's back.".to_string(), pal().amber)
                    }
                    Some(rule) => {
                        let stale = self.archive_preview.as_ref().is_none_or(|(r, at, ..)| *r != rule || at.elapsed() > std::time::Duration::from_secs(5));
                        if stale {
                            let all = self.listed(&self.cfg.folder);
                            let plan = clips::archive_plan(&all, rule.after, std::time::SystemTime::now());
                            let (n, bytes) = (plan.len(), plan.iter().map(|c| c.bytes).sum());
                            self.archive_preview = Some((rule, std::time::Instant::now(), n, bytes));
                        }
                        ui.ctx().request_repaint_after(std::time::Duration::from_secs(5));
                        match self.archive_preview {
                            Some((_, _, n, bytes)) if n > 0 => (format!("Right now this would move {} ({}) there.", count(n, "clip"), clips::size(bytes)), accent_light()),
                            _ => ("Nothing to move right now.".to_string(), pal().muted),
                        }
                    }
                };
                ui.label(RichText::new(text).small().color(color));
                if off {
                    self.use_folder_anyway(ui);
                }
            }
        });
    }

    fn discord_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "Discord", |ui| {
            row(ui, "Webhook", |ui| {
                ui.add(egui::TextEdit::singleline(&mut self.cfg.discord_webhook).password(true).hint_text("https://discord.com/api/webhooks/...").desired_width(f32::INFINITY)).on_hover_text(WEBHOOK_ABOUT);
            });
            match self.cfg.discord_webhook.trim() {
                "" => {}
                w if upload::is_discord_webhook(w) => _ = ui.label(RichText::new("Clips go to that channel with Post to Discord in a clip's menu (up to 10 MB).").small().color(pal().muted)),
                _ => _ = ui.label(RichText::new("That doesn't look like a Discord webhook link (https://discord.com/api/webhooks/...).").small().color(pal().red)),
            }
        });
    }

    /// Share links of clips that are gone (deleted outside the app, or recycled without their link),
    /// with Delete all. Only shown while there are some, or links are being deleted.
    fn share_links_card(&mut self, ui: &mut egui::Ui) {
        let lc = &self.link_cleanup;
        let (n, busy) = (lc.orphans.len(), lc.job.is_some() || !lc.waiting.is_empty());
        if n == 0 && !busy {
            return;
        }
        card(ui, "Share links", |ui| {
            if n > 0 {
                ui.horizontal(|ui| {
                    ui.label(format!("{} of deleted clips", count(n, "share link")));
                    let delete = ui.add_enabled(!busy, egui::Button::new("Delete all"));
                    if delete.on_hover_text("Takes the uploaded copies of those clips off kappa.lol, so their links stop working").clicked() {
                        let paths = self.link_cleanup.orphans.iter().map(std::path::PathBuf::from).collect();
                        self.delete_links(paths);
                    }
                });
            }
            if busy {
                ui.label(RichText::new("Deleting share links...").small().color(accent_light()));
            } else if let Some(msg) = &self.link_cleanup.msg {
                ui.label(RichText::new(msg).small().color(pal().amber));
            }
        });
    }

    fn windows_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "Windows", |ui| {
            let allowed = crate::instance::current().windows_tweaks_allowed();
            allowed_checkbox(ui, allowed, &mut self.cfg.game_bar_off, "Turn off Xbox Game Bar capture", crate::instance::TWEAKS_OFF)
                .on_hover_text("Stops Windows' own Game Bar from also recording in the background, which costs some FPS and competes for the GPU encoder. Unticking puts your own setting back.");
            ui.checkbox(&mut self.cfg.gpu_high, "Run the recorder on the high-performance GPU")
                .on_hover_text("On PCs with two graphics chips (like a laptop, or a CPU with graphics), records with the faster one. Only applies to the app's private OBS copy. Takes effect after Save.");
        });
    }

    fn backup_cards(&mut self, ui: &mut egui::Ui) {
        if self.wizard.is_none() {
            card(ui, "Move to a new PC", |ui| {
                ui.horizontal(|ui| {
                    if button(ui, "Export settings...").on_hover_text("Saves your settings, stars, tags and notes to one file, for a new PC or a reinstall. Passwords and the Discord webhook are left out.").clicked() {
                        self.export_settings();
                    }
                    if button(ui, "Import settings...").on_hover_text("Loads settings from a file you exported before").clicked() {
                        self.pick_import();
                    }
                });
            });
            card(ui, "Remove", |ui| {
                if button(ui, "Remove from this PC...").on_hover_text(REMOVE_ABOUT).clicked() {
                    self.confirm_remove = true;
                }
            });
        }
    }

    /// The clip length as a few chips and Custom, which shows a slider (10 s to 20 minutes, or
    /// less: as long as fits in a quarter of this PC's RAM at the video settings). A length that
    /// isn't one of the chips is Custom.
    pub(crate) fn length_chips(&mut self, ui: &mut egui::Ui) {
        const LENGTHS: [u32; 4] = [60, 120, 180, 300];
        let max = self.max_length();
        // Settings changed elsewhere (higher quality) can leave the length past what fits: it's
        // capped on Save (`cap_length`), not here, so just looking doesn't change a setting.
        let id = egui::Id::NULL.with("custom clip length");
        let mut custom = ui.ctx().data(|d| d.get_temp::<bool>(id)).unwrap_or(false) || !LENGTHS.contains(&self.cfg.buffer_secs);
        let too_long = format!("Longer than fits at your video settings ({}) in a quarter of this PC's {} GB of RAM", mmss(max), (profile::ram_mb() + 512) / 1024);
        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                for secs in LENGTHS {
                    let mut shown = if custom { None } else { Some(self.cfg.buffer_secs) };
                    let fits = secs <= max;
                    let r = ui.add_enabled_ui(fits, |ui| chip(ui, &mut shown, Some(secs), mmss(secs))).inner;
                    if r.on_hover_text("How much a saved clip keeps: the last this long").on_disabled_hover_text(&too_long).clicked() {
                        (self.cfg.buffer_secs, custom) = (secs, false);
                    }
                }
                if chip(ui, &mut custom, true, "Custom").on_hover_text(format!("Any length from 0:10 to {}", mmss(max))).clicked() {
                    custom = true;
                }
            });
            if custom {
                ui.add(egui::Slider::new(&mut self.cfg.buffer_secs, 10..=max).clamping(egui::SliderClamping::Edits).custom_formatter(|v, _| mmss(v as u32))).on_hover_text("How much a saved clip keeps: the last this long");
            }
            if max < 1200 {
                ui.label(RichText::new(format!("Up to {} at your video settings", mmss(max))).small().color(pal().muted))
                    .on_hover_text(format!("The replay is kept in RAM and takes at most a quarter of this PC's {} GB. Lower the quality on the Video page for longer.", (profile::ram_mb() + 512) / 1024));
            }
        });
        ui.ctx().data_mut(|d| d.insert_temp(id, custom));
    }

    /// The longest clip that fits in a quarter of this PC's RAM at the video settings being set.
    fn max_length(&self) -> u32 {
        recorder::pick_monitor(&self.monitors, &self.cfg).map_or(1200, |m| recorder::max_secs(&self.cfg, &m))
    }

    /// Shortens the clip length to what fits (`max_length`), as it's saved.
    pub(crate) fn cap_length(&mut self) {
        self.cfg.buffer_secs = self.cfg.buffer_secs.min(self.max_length());
    }

    /// Writes the saved settings, stars and tags (no secrets) to a file the user picks.
    pub(crate) fn export_settings(&mut self) {
        let Some(path) = overlay::file_dialog().set_title("Export settings").set_file_name("Replay Vault settings.json").add_filter("Settings", &["json"]).save_file() else { return };
        let export = config::Export { notes: self.notes.clone(), ..config::Export::new(&self.saved, &self.favorites, &self.tags) };
        self.status = match export.write(&path) {
            Ok(()) => format!("Settings exported to {}.", path.display()),
            Err(e) => format!("Couldn't export settings: {e}"),
        };
    }

    /// Reads a settings file the user picks, for the import dialog to show what it changes.
    pub(crate) fn pick_import(&mut self) {
        let Some(path) = overlay::file_dialog().set_title("Import settings").add_filter("Settings", &["json"]).pick_file() else { return };
        match config::Export::read(&path) {
            Ok(file) => self.import = Some(file),
            Err(e) => self.status = format!("Couldn't read that settings file: {e}"),
        }
    }

    /// Start with Windows, kept right away (no Save): it's a Windows setting, not one of ours.
    pub(crate) fn startup_checkbox(&mut self, ui: &mut egui::Ui) {
        // With no show/hide key (its key went to another action), just X.
        let keys = if self.cfg.overlay_hotkey.trim().is_empty() { "The X".to_string() } else { format!("{} or the X", self.cfg.overlay_hotkey) };
        let about = format!("Starts the app quietly in the tray when you sign in, so it's always recording. {keys} hides this window; the app keeps running in the tray.");
        let changed = ui
            .horizontal(|ui| {
                let changed = ui.checkbox(&mut self.start_with_windows, "Start with Windows (in the tray)").on_hover_text(about).changed();
                if self.startup_turned_off {
                    ui.label(RichText::new("Turned off in Task Manager").small().color(pal().amber))
                        .on_hover_text("Windows skips it at sign-in: it was turned off in Task Manager's Startup apps. Tick the box to turn it back on.");
                }
                changed
            })
            .inner;
        if changed {
            if let Err(e) = startup::set_enabled(self.start_with_windows) {
                self.status = format!("Couldn't change the startup setting: {e}");
            }
            self.refresh_startup();
        }
    }

    /// Reads Start with Windows again, and whether Task Manager turned it off (two registry reads,
    /// so only at start, after a change, and when the window is shown).
    pub(crate) fn refresh_startup(&mut self) {
        self.start_with_windows = startup::is_enabled();
        self.startup_turned_off = startup::turned_off_in_task_manager();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::{NodeT, Queryable};

    /// Whether `text` is in a line of text on screen (not only in hover text).
    fn on_screen(h: &egui_kittest::Harness<'_, App>, text: &str) -> bool {
        h.query_by(|n| n.role() == egui::accesskit::Role::Label && n.value().is_some_and(|v| v.contains(text))).is_some()
    }

    /// Opening General with a clip length past what fits leaves the settings as saved (no Unsaved
    /// changes from just looking); Save shortens it to what fits.
    #[test]
    fn length_is_capped_on_save_not_on_sight() {
        let setup = |app: &mut App| {
            (app.cfg.quality, app.cfg.buffer_secs, app.general_part) = (0, 1200, GeneralPart::Clips);
            app.saved = app.cfg.clone();
        };
        let mut h = testing::page(setup, |ui, app| app.general_tab(ui));
        h.run();
        assert!(h.state().cfg == h.state().saved, "drawing changed nothing");
        let app = h.state_mut();
        app.cap_length();
        assert_eq!(app.cfg.buffer_secs, app.max_length().min(1200));
    }

    /// What used to be grey text under Discord's webhook box and Remove from this PC is now the
    /// hover text of the box and the button, and nothing on Sharing or Backup is grey text.
    #[test]
    fn explanations_are_hover_text() {
        let mut h = testing::page(|app| app.general_part = GeneralPart::Sharing, |ui, app| app.general_tab(ui));
        h.run();
        let webhook = h.get_by_role(egui::accesskit::Role::PasswordInput);
        assert_eq!(webhook.accesskit_node().description().as_deref(), Some(WEBHOOK_ABOUT));
        assert!(WEBHOOK_ABOUT.contains("Keep it private") && WEBHOOK_ABOUT.contains("New Webhook"));
        assert!(!on_screen(&h, "Edit Channel"));

        h.state_mut().general_part = GeneralPart::Backup;
        h.run();
        let remove = h.get_by_label("Remove from this PC...");
        assert_eq!(remove.accesskit_node().description().as_deref(), Some(REMOVE_ABOUT));
        assert!(on_screen(&h, "Move to a new PC"), "the card's title is");
        assert!(!on_screen(&h, "Puts back everything") && !on_screen(&h, "left out"));
        let export = h.get_by_label("Export settings...").accesskit_node().description().unwrap_or_default();
        assert!(export.contains("Passwords and the Discord webhook are left out"), "{export}");
    }

    /// Starred clips a folder change couldn't carry put auto-cleanup on hold, said on its card,
    /// until the user goes on without them.
    #[test]
    fn cleanup_card_says_its_on_hold() {
        let lost = r"Q:\Old clips\a.mp4".to_string();
        let mut h = testing::page(
            |app| {
                app.general_part = GeneralPart::Storage;
                app.cfg.folder = std::env::temp_dir().join("instant-replay-no-such-folder").display().to_string();
                app.cfg.keep_days = 30;
                app.favorites.insert(lost.clone());
                app.lost.insert(lost.clone());
            },
            |ui, app| app.general_tab(ui),
        );
        h.run();
        assert!(on_screen(&h, "Auto-cleanup is on hold: 1 starred clip from your earlier clips folder wasn't found"));
        assert!(h.query_by_label("Go on without them").is_none(), "one clip is it");
        let go_on = h.get_by_label("Go on without it");
        assert!(go_on.accesskit_node().description().unwrap_or_default().contains("If that clip is in the clips folder, it may be removed"));
        go_on.click();
        h.run();
        assert!(h.state().lost.is_empty());
        assert!(!on_screen(&h, "on hold"));
    }

    /// With the clips folder set to the Videos folder itself, the Clips tab warns under the folder
    /// and the cleanup and archive cards say they're off, naming the folder. Use this folder anyway
    /// lets both work there (once saved), says what it means on hover, and the warnings go; another
    /// folder asks again.
    #[test]
    fn rules_off_in_a_shared_folder() {
        let videos = clips::known_folders().iter().find(|(n, _)| *n == "Videos").map(|(_, p)| p.display().to_string()).unwrap();
        let shown = videos.clone();
        let mut h = testing::page(
            move |app| {
                app.cfg.folder = shown;
                (app.cfg.keep_days, app.cfg.archive_days, app.cfg.archive_folder) = (30, 30, r"Q:\Archive".into());
                // Saved, so the cards' previews use the (empty) clips list rather than reading the folder.
                app.saved = app.cfg.clone();
            },
            |ui, app| app.general_tab(ui),
        );
        h.run();
        assert!(on_screen(&h, "This is your Videos folder. Auto-cleanup and the archive won't run in it"));
        h.state_mut().general_part = GeneralPart::Storage;
        h.run();
        assert!(on_screen(&h, &format!("Auto-cleanup is off: the clips folder ({videos}) is your Videos folder")));
        assert!(on_screen(&h, &format!("The archive is off: the clips folder ({videos}) is your Videos folder")));
        let anyway = h.query_all_by_label("Use this folder anyway").collect::<Vec<_>>();
        assert_eq!(anyway.len(), 2, "on both cards");
        assert!(anyway[0].accesskit_node().description().unwrap_or_default().contains("Every video and picture in it, and in the folders right inside it, then counts as a clip"));
        anyway[0].click();
        h.run();
        assert_eq!(h.state().cfg.shared_folder_ok, videos);
        assert!(h.state().cfg != h.state().saved, "kept with Save, like other settings");
        assert!(!on_screen(&h, "is off") && h.query_by_label("Use this folder anyway").is_none());
        h.state_mut().general_part = GeneralPart::Clips;
        h.run();
        assert!(!on_screen(&h, "won't run in it"));
        // Another shared folder isn't covered by it.
        h.state_mut().cfg.folder = std::path::Path::new(&videos).parent().unwrap().display().to_string();
        h.run();
        assert!(on_screen(&h, "Auto-cleanup and the archive won't run in it"));
    }

    /// The folder risk thread takes the newest folder wanted, one thread at a time, and an answer
    /// for a folder since changed is dropped. A local job, not the shared one, so nothing runs.
    #[test]
    fn folder_risk_newest_wins() {
        let key = |f: &str| (f.to_string(), String::new());
        let (a, b) = (key(r"D:\A"), key(r"D:\B"));
        let mut job = RiskJob { want: None, busy: false, done: None };
        assert!(job.want(a.clone()), "nothing on it: a thread starts");
        assert_eq!(job.next(), Some(a.clone()));
        assert!(!job.want(b.clone()), "busy: no second thread");
        // A's answer lands, but the folder is B by now: dropped.
        job.done = Some((a.clone(), Some("A".into())));
        assert_eq!(job.answer_for(&b), None);
        assert!(job.done.is_none());
        assert_eq!(job.next(), Some(b.clone()));
        // Back to A while B is worked out: B's answer is dropped, A is worked out again and kept.
        assert!(!job.want(a.clone()));
        job.done = Some((b.clone(), None));
        assert_eq!(job.answer_for(&a), None);
        assert_eq!(job.next(), Some(a.clone()));
        job.done = Some((a.clone(), Some("A".into())));
        assert_eq!(job.answer_for(&a), Some(Some("A".into())));
        // Nothing more wanted: the thread is done, and the next folder starts a new one.
        assert_eq!(job.next(), None);
        assert!(!job.busy);
        assert!(job.want(b));
    }

    /// Sharing lists the share links of deleted clips, only when there are some, and Delete all
    /// hands them to be deleted (nothing is sent here: that starts in `finish_uploads`).
    #[test]
    fn share_links_of_deleted_clips() {
        let gone = [r"D:\Clips\a.mp4".to_string(), r"D:\Clips\b.mp4".to_string()];
        let mut h = testing::page(|app| app.general_part = GeneralPart::Sharing, |ui, app| app.general_tab(ui));
        h.run();
        assert!(h.query_by_label("Delete all").is_none());
        h.state_mut().link_cleanup.orphans = gone.to_vec();
        h.run();
        assert!(on_screen(&h, "2 share links of deleted clips"));
        h.get_by_label("Delete all").click();
        h.run();
        let waiting: Vec<String> = h.state().link_cleanup.waiting.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(waiting, gone);
        assert!(on_screen(&h, "Deleting share links..."));
    }

    /// A box that is turned off (`allowed_checkbox`, see `instance`) shows what really happens:
    /// unticked and greyed, with the reason on screen, whatever the setting says, and clicking it
    /// changes nothing. Allowed, it's an ordinary checkbox.
    #[test]
    fn a_box_turned_off_shows_what_it_does() {
        for allowed in [false, true] {
            let mut h = testing::page(
                |app| app.cfg.game_bar_off = true,
                move |ui, app| _ = allowed_checkbox(ui, allowed, &mut app.cfg.game_bar_off, "Turn off Xbox Game Bar capture", "Not available."),
            );
            h.run();
            let node = h.get_by_label("Turn off Xbox Game Bar capture");
            assert_eq!(node.accesskit_node().toggled() == Some(egui::accesskit::Toggled::True), allowed, "allowed {allowed}");
            assert_eq!(node.accesskit_node().is_disabled(), !allowed, "allowed {allowed}");
            assert_eq!(on_screen(&h, "Not available."), !allowed, "allowed {allowed}");
            node.click();
            h.run();
            assert_eq!(h.state().cfg.game_bar_off, !allowed, "allowed {allowed}: a click unticks it, or does nothing");
        }
    }

    /// The Show tips and explanations setting is gone from Appearance.
    #[test]
    fn no_tips_setting() {
        let mut h = testing::page(|app| app.general_part = GeneralPart::Appearance, |ui, app| app.general_tab(ui));
        h.run();
        assert!(h.query_by_label("Theme").is_some());
        assert!(h.query_by_label("Show tips and explanations").is_none());
    }
}
