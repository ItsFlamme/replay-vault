//! The General page: clip length, folders, cleanup, archive, appearance, and settings export and import.

use crate::*;

/// A limit that's off (0) until ticked, then starts at `start`.
pub(crate) fn limit(ui: &mut egui::Ui, value: &mut u32, text: &str, unit: &str, start: u32, max: u32) {
    ui.horizontal(|ui| {
        let mut on = *value > 0;
        if ui.checkbox(&mut on, text).on_hover_text("Tick to turn this rule on").changed() {
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
        form(ui, "appearance", |ui| {
            ui.label("Theme");
            ui.horizontal_wrapped(|ui| {
                let theme = if cfg.theme == "retro" { "retro" } else { "minimalist" };
                let mut pick = theme;
                chip(ui, &mut pick, "minimalist", "Minimalist").on_hover_text("Clean and flat: buttons look like keyboard keys");
                chip(ui, &mut pick, "retro", "Retro").on_hover_text("The 80s video tape look: tape labels, chunky buttons and a VCR font");
                if pick != theme {
                    cfg.theme = pick.into();
                }
            });
            ui.end_row();

            ui.label("Mode");
            ui.horizontal_wrapped(|ui| {
                chip(ui, &mut cfg.light_mode, false, "Dark").on_hover_text("Light text on dark backgrounds");
                chip(ui, &mut cfg.light_mode, true, "Light").on_hover_text("Dark text on light backgrounds");
            });
            ui.end_row();

            if cfg.theme == "retro" {
                ui.label("Effects");
                ui.checkbox(&mut cfg.reduce_effects, "Reduce effects").on_hover_text("Turns off the scanlines, grain and glows, for a calmer, flat background");
                ui.end_row();
            }

            ui.label("Accent color");
            ui.horizontal_wrapped(|ui| {
                for (i, (name, ..)) in ACCENTS.iter().enumerate() {
                    chip(ui, &mut cfg.accent, i as u8, *name).on_hover_text("Use this color for buttons and highlights");
                }
                chip(ui, &mut cfg.accent, CUSTOM_ACCENT, "Custom").on_hover_text("Pick any color for buttons and highlights");
            });
            ui.end_row();

            if cfg.accent == CUSTOM_ACCENT {
                ui.label("Custom color");
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
                ui.end_row();
            }
        });
    });
}

impl App {
    /// General: clip length, the folder and its game folders, what a save does, startup and the
    /// look; the rest (names, cleanup, archive, Discord, Windows tweaks, moving PCs) under More options.
    pub(crate) fn general_tab(&mut self, ui: &mut egui::Ui) {
        card(ui, "Clips", |ui| {
            form(ui, "clips", |ui| {
                ui.label("Clip length");
                self.length_chips(ui);
                ui.end_row();

                ui.label("Clips folder");
                folder_field(ui, &mut self.cfg.folder, "");
                ui.end_row();
            });
            ui.checkbox(&mut self.cfg.sort_by_game, "Sort clips into a folder per game")
                .on_hover_text("Named after the title of the window in front when you save. Only the title is read; the game itself is never opened or touched.");
        });
        appearance(ui, &mut self.cfg);
        card(ui, "When a clip is saved", |ui| {
            ui.checkbox(&mut self.cfg.save_popup, "Show a popup").on_hover_text("A small note in the corner of the screen when a clip is saved");
            ui.checkbox(&mut self.cfg.save_sound, "Play a soft sound (screenshots too)").on_hover_text("A quiet sound, so you know it worked without looking");
        });
        card(ui, "Startup", |ui| {
            self.startup_checkbox(ui);
            ui.checkbox(&mut self.cfg.check_updates, "Tell me when a new version is out")
                .on_hover_text("Asks GitHub once a day whether a newer Replay Vault is out. Nothing about you or this PC is sent, and nothing is downloaded until you choose Install now.");
            ui.horizontal(|ui| {
                if button(ui, "Check for updates").on_hover_text("Asks GitHub now whether a newer Replay Vault is out").clicked() {
                    let c = ui.ctx().clone();
                    crate::app_update::check_now(move || c.request_repaint());
                }
                if let Some(s) = crate::app_update::status() {
                    ui.label(RichText::new(s).color(pal().muted));
                }
            });
            note(ui, format!("{}, Esc or X hides this window. The app keeps running in the tray.", self.cfg.overlay_hotkey));
        });
        card(ui, "Tips", |ui| {
            ui.checkbox(&mut self.cfg.show_tips, "Show tips and explanations").on_hover_text("Untick to turn off the tip boxes at the top of every page, for good, and the small grey text under settings");
        });
        self.more_options(ui, Tab::General, Self::general_more);
    }

    /// General's More options.
    pub(crate) fn general_more(&mut self, ui: &mut egui::Ui) {
        card(ui, "Clip names and covers", |ui| {
            form(ui, "names", |ui| {
                ui.label("Date style");
                ui.horizontal_wrapped(|ui| {
                    for (i, (_, example)) in recorder::NAME_STYLES.iter().enumerate() {
                        chip(ui, &mut self.cfg.name_style, i as u8, *example).on_hover_text("Write the date in clip names like this");
                    }
                });
                ui.end_row();
            });
            note(ui, format!("Clips are named like: {}", recorder::example_name(&self.cfg)));
            ui.checkbox(&mut self.cfg.steam_covers, "Look up game covers online (Steam)").on_hover_text(
                "Sends each game folder's name (like \"Elden Ring\"), and your IP address as with any web request, to Steam's store search, then downloads the cover picture. Each name is asked once (one with no match again after a week). Names that look like a web page, a file, an email address or a non-game app (browser, Explorer, Discord, editors) are never sent.",
            );
        });
        card(ui, "Auto-cleanup", |ui| {
            limit(ui, &mut self.cfg.keep_under_gb, "Keep clips under", "GB", 100, 10_000);
            limit(ui, &mut self.cfg.keep_days, "Remove clips older than", "days", 30, 3650);
            note(ui, "Oldest first, to the Recycle Bin. Starred clips are never removed and don't count toward the size. Clips from the last 10 minutes are left alone.");
            let rule = cleanup_rule(&self.cfg);
            if rule != clips::Cleanup::default() {
                let stale = self.cleanup_preview.is_none_or(|(r, at, ..)| r != rule || at.elapsed() > std::time::Duration::from_secs(5));
                if stale {
                    let plan = clips::cleanup_plan(&clips::list(&self.cfg.folder, usize::MAX), &self.favorites, rule, std::time::SystemTime::now());
                    let bytes = plan.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
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
            limit(ui, &mut self.cfg.archive_days, "Move clips older than", "days", 30, 3650);
            if self.cfg.archive_days > 0 {
                folder_field(ui, &mut self.cfg.archive_folder, "Another drive or a NAS folder");
                note(ui, "Oldest first, into the same game folders there. Stars, tags and share links go along. Clips from the last 10 minutes are left alone.");
                let (text, color) = match archive_rule(&self.cfg) {
                    None if self.cfg.archive_folder.trim().is_empty() => ("Pick a folder to move them to.".to_string(), pal().muted),
                    None => ("Pick a folder outside the clips folder.".to_string(), pal().amber),
                    Some(_) if self.cfg == self.saved && clips::ARCHIVE_MISSING.load(std::sync::atomic::Ordering::Relaxed) => {
                        ("That folder wasn't there last time (drive or NAS off?). Clips wait until it's back.".to_string(), pal().amber)
                    }
                    Some(rule) => {
                        let stale = self.archive_preview.as_ref().is_none_or(|(r, at, ..)| *r != rule || at.elapsed() > std::time::Duration::from_secs(5));
                        if stale {
                            let all = clips::list(&self.cfg.folder, usize::MAX);
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
            }
        });
        card(ui, "Discord", |ui| {
            form(ui, "discord", |ui| {
                ui.label("Webhook");
                ui.add(egui::TextEdit::singleline(&mut self.cfg.discord_webhook).password(true).hint_text("https://discord.com/api/webhooks/...").desired_width(f32::INFINITY)).on_hover_text("Paste your Discord channel's webhook link here");
                ui.end_row();
            });
            match self.cfg.discord_webhook.trim() {
                "" => note(ui, "For Post to Discord in a clip's menu. In Discord: Edit Channel > Integrations > Webhooks > New Webhook > Copy Webhook URL, then paste it here. Keep it private: anyone with it can post there."),
                w if upload::is_discord_webhook(w) => note(ui, "Clips go to that channel with Post to Discord in a clip's menu (up to 10 MB)."),
                _ => _ = ui.label(RichText::new("That doesn't look like a Discord webhook link (https://discord.com/api/webhooks/...).").small().color(pal().red)),
            }
        });
        card(ui, "Windows", |ui| {
            ui.checkbox(&mut self.cfg.game_bar_off, "Turn off Xbox Game Bar capture").on_hover_text("Stops Windows' own Game Bar from also recording in the background");
            note(ui, "Game Bar records in the background too, which costs some FPS and competes for the GPU encoder. Unticking puts your own setting back.");
            ui.checkbox(&mut self.cfg.gpu_high, "Run the recorder on the high-performance GPU").on_hover_text("On PCs with two graphics chips, records with the faster one");
            note(ui, "For PCs with two GPUs (like a laptop or a CPU with graphics). Only applies to the app's private OBS copy. Takes effect after Save.");
        });
        if self.wizard.is_none() {
            card(ui, "Move to a new PC", |ui| {
                note(ui, "One file with your saved settings, stars and tags, for a new PC or a reinstall. Passwords and the Discord webhook are left out.");
                ui.horizontal(|ui| {
                    if button(ui, "Export settings...").on_hover_text("Saves your settings, stars and tags to one file").clicked() {
                        self.export_settings();
                    }
                    if button(ui, "Import settings...").on_hover_text("Loads settings from a file you exported before").clicked() {
                        self.pick_import();
                    }
                });
            });
            card(ui, "Remove", |ui| {
                note(ui, "Puts back everything the app changed on this PC and moves its private OBS copy to the Recycle Bin. Your clips stay.");
                if button(ui, "Remove from this PC...").on_hover_text("Undoes what the app changed on this PC. Asks first; your clips stay.").clicked() {
                    self.confirm_remove = true;
                }
            });
        }
    }

    /// The clip length as a few chips and Custom, which shows a slider (10 s to 20 minutes). A
    /// length that isn't one of the chips is Custom.
    pub(crate) fn length_chips(&mut self, ui: &mut egui::Ui) {
        const LENGTHS: [u32; 4] = [30, 60, 120, 300];
        let id = egui::Id::NULL.with("custom clip length");
        let mut custom = ui.ctx().data(|d| d.get_temp::<bool>(id)).unwrap_or(false) || !LENGTHS.contains(&self.cfg.buffer_secs);
        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                for secs in LENGTHS {
                    let mut shown = if custom { None } else { Some(self.cfg.buffer_secs) };
                    if chip(ui, &mut shown, Some(secs), mmss(secs)).on_hover_text("How much a saved clip keeps: the last this long").clicked() {
                        (self.cfg.buffer_secs, custom) = (secs, false);
                    }
                }
                if chip(ui, &mut custom, true, "Custom").on_hover_text("Any length from 0:10 to 20:00").clicked() {
                    custom = true;
                }
            });
            if custom {
                ui.add(egui::Slider::new(&mut self.cfg.buffer_secs, 10..=1200).custom_formatter(|v, _| mmss(v as u32))).on_hover_text("How much a saved clip keeps: the last this long");
            }
        });
        ui.ctx().data_mut(|d| d.insert_temp(id, custom));
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
        if ui.checkbox(&mut self.start_with_windows, "Start with Windows (in the tray)").on_hover_text("Starts the app quietly in the tray when you sign in, so it's always recording").changed()
            && let Err(e) = startup::set_enabled(self.start_with_windows) {
            self.status = format!("Couldn't change the startup setting: {e}");
            self.start_with_windows = startup::is_enabled();
        }
    }
}
