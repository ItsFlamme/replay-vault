//! First-run setup: its steps, the OBS install and import, and the clips folder.

use crate::*;

/// The clips folder that goes with the private OBS in `obs_dir` ("D:\Games\Replay Vault OBS"):
/// "Replay Vault Clips" beside it, or the usual Videos folder when OBS is in its default place ("").
fn clips_next_to(obs_dir: &str) -> String {
    match std::path::Path::new(obs_dir.trim()).parent() {
        Some(parent) if !obs_dir.trim().is_empty() => parent.join("Replay Vault Clips").display().to_string(),
        _ => Config::default().folder,
    }
}

impl App {
    /// Setup's first card: the save key and where clips go.
    pub(crate) fn setup_clips_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "Your clips", |ui| {
            form(ui, "setup_clips", |ui| {
                ui.label("Save key");
                self.hotkey_button(ui, Target::Save, "Click, then press the keys you want. Works even in a game.", 0.0);
                ui.end_row();

                ui.label("Clips folder");
                if self.obs_exe.is_none() {
                    // Picked together with OBS's folder above, so setup has one Browse.
                    ui.add(egui::Label::new(RichText::new(&self.cfg.folder).color(pal().muted)).truncate())
                        .on_hover_text("Set with OBS's folder above. You can move it later under General.");
                } else {
                    folder_field(ui, &mut self.cfg.folder, "");
                }
                ui.end_row();
            });
            self.check_conflicts();
            self.conflict_notes(ui, Some(Target::Save));
        });
    }

    /// The logo with the step count beside it, the step's title and the progress bar: kept short,
    /// so a small window still has room for the step itself.
    pub(crate) fn wizard_header(&mut self, ui: &mut egui::Ui, step: usize) {
        ui.horizontal(|ui| {
            brand(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(osd(format!("SETUP  {}/{}", step + 1, STEPS.len()), 20.0).color(pal().phosphor));
            });
        });
        ui.add_space(6.0);
        retro_title(ui, STEPS[step], 40.0);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for i in 0..STEPS.len() {
                let w = (ui.available_width() - 6.0 * (STEPS.len() - 1 - i) as f32) / (STEPS.len() - i) as f32;
                let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 4.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 2.0, if i <= step { accent() } else { pal().raised });
            }
        });
    }

    pub(crate) fn wizard_body(&mut self, ui: &mut egui::Ui, step: usize) {
        match step {
            0 => {
                card(ui, "Welcome", |ui| {
                    ui.label(RichText::new("Save the last few minutes of your game with one key.").size(16.0).strong());
                    ui.add_space(4.0);
                    ui.label(RichText::new("Replay Vault keeps your last minutes in memory, like ShadowPlay, and writes a clip only when you press the save key. It records with OBS Studio, hidden in the background.").color(pal().muted));
                });
                self.obs_step(ui);
                self.setup_clips_card(ui);
            }
            1 => match recorder::pick_monitor(&self.monitors, &self.cfg) {
                Some(mon) => {
                    self.this_pc_card(ui, &mon);
                    self.obs_import_card(ui, &mon, true);
                }
                None => _ = ui.label(RichText::new("No monitor found.").color(pal().muted)),
            },
            _ => card(ui, "You're ready", |ui| {
                ui.label(RichText::new(format!("Press {} in a game to save the last {}.", self.cfg.hotkey, mmss(self.cfg.buffer_secs))).size(16.0).strong());
                ui.add_space(4.0);
                ui.label(RichText::new(format!("{} opens or hides this window. Clips go to {}. You can change all of it later.", self.cfg.overlay_hotkey, self.cfg.folder)).color(pal().muted));
                ui.add_space(10.0);
                self.startup_checkbox(ui);
            }),
        }
    }

    /// When the user has their own OBS set up, offers to start from its video and sound settings.
    /// In setup (`ask`) the question is always shown; on the Video page it's a tip.
    pub(crate) fn obs_import_card(&mut self, ui: &mut egui::Ui, mon: &display::Monitor, ask: bool) {
        if !self.obs_settings_found {
            return;
        }
        card(ui, "Your OBS", |ui| {
            match &self.imported {
                Some(s) => status(ui, true, RichText::new(s).small(), accent_light(), 12.0),
                None if ask => _ = ui.label("We found your own OBS settings. Use its video and sound setup here (resolution, frame rate, encoder, quality, sound devices and tracks)? Otherwise the recommended settings for this PC are used. Your OBS is only read, never changed."),
                None => note(ui, "Use the resolution, frame rate, encoder and quality from your own OBS, and its sound devices and tracks. Your OBS is only read, never changed."),
            };
            ui.add_space(4.0);
            let label = if ask { "Use my OBS settings" } else { "Import my OBS video and sound settings" };
            if button(ui, label).on_hover_text("Copies your own OBS's video and sound setup. Your OBS isn't changed.").clicked() {
                let s = obs_import::apply(&mut self.cfg, mon, &self.devices).unwrap_or_else(|| "Nothing usable was found in your OBS settings.".into());
                log::write(&s);
                self.imported = Some(s);
            }
        });
    }

    /// Checks for a usable OBS and offers to download a private copy (and the VC++ runtime).
    pub(crate) fn obs_step(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if let Some(job) = &self.job {
            let p = job.lock().unwrap().clone();
            match p.result {
                Some(r) => {
                    self.job = None;
                    self.job_error = r.err();
                    self.obs_exe = obs_install::find();
                    self.firewall_ok = None; // re-check after any job
                }
                None => {
                    card(ui, "Working", |ui| {
                        let frac = if p.total > 0 { p.done as f32 / p.total as f32 } else { 0.0 };
                        let detail = if p.stage.starts_with("Downloading OBS") {
                            format!("{} / {} MB", p.done / 1_048_576, p.total / 1_048_576)
                        } else if p.total > 0 {
                            format!("{} / {}", p.done, p.total)
                        } else {
                            String::new()
                        };
                        ui.label(&p.stage);
                        ui.add(egui::ProgressBar::new(frac).text(detail).animate(p.total == 0));
                    });
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    return;
                }
            }
        }

        let exe = self.obs_exe.clone();
        // Some(new folder) when the user picks where the private copy goes; Some("") = default.
        let mut new_dir: Option<String> = None;
        let custom_dir = !self.cfg.obs_dir.is_empty();
        card(ui, "OBS Studio", |ui| match &exe {
            Some(exe) => {
                status(ui, true, RichText::new("OBS is ready").strong(), accent_light(), 14.0);
                let dir = exe.ancestors().nth(3).unwrap_or(exe);
                ui.add(egui::Label::new(RichText::new(dir.display().to_string()).small().color(pal().muted)).truncate());
                let v = obs_install::private_version().unwrap_or_default();
                ui.label(format!("A private copy (OBS {v}) used only by this app. It's kept up to date automatically. Any OBS you already have isn't touched."));
            }
            None => {
                ui.label("OBS Studio (free and open source) does the recording. Install downloads the official release (about 225 MB) from GitHub into a private folder used only by this app, checks it against its published checksum, and keeps it up to date. Any OBS you already have isn't touched.");
                ui.add_space(6.0);
                // Where it goes: any drive, so it needn't take space on the system drive.
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Install to").color(pal().muted));
                    if button(ui, "Browse").on_hover_text("Pick a drive or folder: the recorder goes in Replay Vault OBS and your clips in Replay Vault Clips, side by side").clicked()
                        && let Some(dir) = overlay::file_dialog().set_title("Where should OBS and your clips go?").pick_folder() {
                        new_dir = Some(dir.join("Replay Vault OBS").display().to_string());
                    }
                    if custom_dir && button(ui, "Default").on_hover_text("Back to the app's own folder on the system drive, and clips in Videos").clicked() {
                        new_dir = Some(String::new());
                    }
                });
                ui.add(egui::Label::new(RichText::new(obs_install::private_dir().display().to_string()).small()).truncate());
                ui.label(RichText::new("Needs about 800 MB there: the 225 MB download plus about 550 MB unpacked.").small().color(pal().muted));
                if !obs_install::has_vc_runtime() {
                    ui.add_space(4.0);
                    ui.label("It also installs Microsoft's Visual C++ runtime, which OBS needs and this PC doesn't have yet. Windows asks for permission.");
                }
                ui.add_space(4.0);
                ui.checkbox(&mut self.setup_firewall, "Block other devices from reaching the recorder")
                    .on_hover_text("Adds a firewall rule that blocks incoming connections to the private OBS from other devices, so Windows never asks whether it can use your network. Windows asks for permission once.");
                ui.checkbox(&mut self.cfg.gpu_high, "Run the recorder on the high-performance GPU")
                    .on_hover_text("Sets Windows' graphics preference (Settings > Display > Graphics) for the private OBS to High performance, so on a PC with two graphics chips it records on the one your games use. Only for that copy; you can change it later under General, and Remove from this PC puts it back.");
            }
        });

        if exe.is_some() && !obs_install::has_vc_runtime() {
            card(ui, "Visual C++ runtime", |ui| {
                ui.label("OBS also needs Microsoft's Visual C++ runtime, which this PC doesn't have yet. Install adds it; Windows asks for permission.");
            });
        }
        if let Some(dir) = new_dir {
            // Clips go where the recorder went, unless the clips folder was set by hand.
            if self.cfg.folder == clips_next_to(&self.cfg.obs_dir) {
                self.cfg.folder = clips_next_to(&dir);
            }
            self.cfg.obs_dir = dir;
            obs_install::set_install_root(&self.cfg.obs_dir);
            log::write(&format!("Private OBS folder set to {}", obs_install::private_dir().display()));
        }
        // Only for our private copy: the user's own OBS is the same program as their main OBS,
        // and blocking it could break their remotes or plugins.
        let mut get_fw = false;
        if let Some(exe) = exe.as_ref().filter(|e| obs_install::is_private(e)) {
            let fw_ok = *self.firewall_ok.get_or_insert_with(obs_install::firewall_rule_exists);
            if !fw_ok {
                card(ui, "Firewall (optional)", |ui| {
                    ui.label("The hidden recorder only talks to this app, on this PC. Without a firewall rule, Windows may pop up asking whether OBS can use your network. This adds a rule that blocks other devices from connecting to it, so that question never comes up. Windows will ask for permission once.");
                    ui.add_space(4.0);
                    get_fw = cap(ui, true, egui::Button::new(RichText::new("Add firewall rule").strong()).min_size(egui::vec2(0.0, 30.0))).on_hover_text("Blocks other devices from reaching the recorder. Windows asks first.").clicked();
                });
            } else {
                card(ui, "Firewall", |ui| {
                    status(ui, true, RichText::new("Other devices can't reach the recorder"), accent_light(), 14.0);
                });
            }
            if get_fw {
                self.job_error = None;
                let c = ctx.clone();
                self.job = Some(obs_install::add_firewall_rule(exe.clone(), move || c.request_repaint()));
            }
        }
        if let Some(e) = &self.job_error {
            ui.colored_label(pal().red, format!("That didn't work: {e}. You can try again."));
        }
    }

    /// Whether the wizard can move past `step`.
    pub(crate) fn step_ready(&self, step: usize) -> bool {
        match step {
            0 => self.obs_ready() && self.capturing.is_none(),
            _ => true,
        }
    }

    /// OBS and its runtime are installed, and nothing is being installed.
    pub(crate) fn obs_ready(&self) -> bool {
        self.job.is_none() && self.obs_exe.is_some() && obs_install::has_vc_runtime()
    }

    pub(crate) fn wizard_footer(&mut self, ui: &mut egui::Ui, step: usize) {
        ui.horizontal(|ui| {
            if step > 0 && cap(ui, true, egui::Button::new("Back").min_size(egui::vec2(90.0, H_BIG))).on_hover_text("Back to the last step").clicked() {
                self.wizard = Some(step - 1);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let last = step + 1 == STEPS.len();
                // Without OBS (or its runtime): one click installs it, then Next shows.
                let install = step == 0 && self.job.is_none() && !self.obs_ready();
                let label = if install { "Install OBS" } else if last { "Start recording" } else { "Next" };
                let ready = install || self.step_ready(step);
                let hint = if install { "Downloads and sets up the recorder" } else if last { "Finishes setup and starts recording in the background" } else { "On to the next step" };
                if primary(ui, ready, RichText::new(label).strong(), egui::vec2(if install { 150.0 } else { 130.0 }, H_BIG)).on_hover_text(hint).clicked() {
                    if install {
                        self.job_error = None;
                        let c = ui.ctx().clone();
                        self.job = Some(obs_install::install(self.setup_firewall, move || c.request_repaint()));
                    } else if last {
                        self.cfg.setup_done = true;
                        self.wizard = None;
                        self.keep_settings();
                        // They couldn't be written (the status says why): record anyway.
                        if self.applied.is_empty() {
                            self.apply();
                        }
                    } else {
                        self.wizard = Some(step + 1);
                    }
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clips_go_beside_the_recorder() {
        assert_eq!(clips_next_to(r"E:\Replay Vault OBS"), r"E:\Replay Vault Clips");
        assert_eq!(clips_next_to(r"D:\Games\Replay Vault OBS"), r"D:\Games\Replay Vault Clips");
        assert_eq!(clips_next_to(""), Config::default().folder);
    }
}
