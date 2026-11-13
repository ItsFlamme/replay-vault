//! The Video page: this PC, quality, the screen and the encoder.

use crate::*;

/// The monitor to record. Following the mouse (under More options) shows here too, and picking a
/// monitor turns it off.
/// `name` only tells this list apart from the other monitor list in the UI (egui's widget id);
/// it isn't cryptographic.
pub(crate) fn monitor_combo(ui: &mut egui::Ui, cfg: &mut Config, monitors: &[display::Monitor], name: &str, mon: &display::Monitor) {
    let follow = cfg.follows_mouse();
    let text = if follow { "Follow the mouse".to_string() } else { mon.label() };
    egui::ComboBox::from_id_salt(egui::Id::NULL.with(name)).selected_text(text).width(ui.available_width().min(420.0)).show_ui(ui, |ui| {
        for m in monitors {
            if ui.selectable_label(!follow && m.id == mon.id, m.label()).clicked() {
                (cfg.monitor_id, cfg.follow_mouse) = (m.id.clone(), false);
            }
        }
    })
    .response
    .on_hover_text("Which screen is recorded");
}

/// What a video plan makes, in one line: "1440p at 60 fps, about 40 Mbps, up to 1.5 GB of memory".
pub(crate) fn plan_text(p: &recorder::Plan) -> String {
    format!("{}p at {} fps, about {} Mbps, up to {:.1} GB of memory", p.out_h, p.fps, p.kbps / 1000, p.mb as f64 / 1024.0)
}

/// The encoder effort choices, with what each one does.
const EFFORTS: [(&str, &str); 3] = [
    ("Best quality", "The encoder takes its time on every frame for the cleanest clips."),
    ("Balanced", "Less work per frame and no extra GPU passes. A little more FPS when your GPU is maxed out; clips look slightly softer in fast motion."),
    ("Max FPS", "The lightest encoding. The most FPS headroom; fast motion looks blockier at the same bitrate (raise Quality to make up for it)."),
];

impl App {
    /// The Video page's top card, and setup's second step: this PC, a quality choice scaled to
    /// it with what each one makes, Use recommended settings, and the screen when there's a choice.
    pub(crate) fn this_pc_card(&mut self, ui: &mut egui::Ui, mon: &display::Monitor) {
        let current = profile::preset_of(&self.cfg, &self.hw, mon);
        let mut pick = current;
        let mut custom = false;
        let mut recommend = false;
        card(ui, "This PC", |ui| {
            ui.label(RichText::new(self.hw.summary()).strong());
            // Only NVIDIA has been tried so far: say so, and never claim the others work.
            if self.hw.gpu != recorder::Gpu::Nvidia {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Untested on this graphics card. It should work, but nobody has checked yet. Tell us how it goes:").color(pal().amber));
                    if ui.link("Send feedback").on_hover_text("Opens the Feedback page").clicked() {
                        self.tab = Tab::Feedback;
                    }
                });
            }
            ui.add_space(8.0);
            ui.label(RichText::new("Quality").strong());
            ui.add_space(2.0);
            egui::Grid::new("quality").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                for (i, name) in profile::PRESETS.into_iter().enumerate() {
                    let mut c = self.cfg.clone();
                    profile::set_preset(&mut c, &self.hw, mon, i);
                    let text = if i + 1 == profile::PRESETS.len() { format!("{name} (recommended)") } else { name.to_string() };
                    chip(ui, &mut pick, Some(i), text).on_hover_text("Each choice is scaled to what this PC handles well. Higher looks better but makes bigger files and uses more of the PC.");
                    ui.label(RichText::new(plan_text(&recorder::plan(&c, mon))).color(pal().muted));
                    ui.end_row();
                }
                // Setup has no More options: Custom shows there only for settings already set by hand.
                if self.wizard.is_none() || current.is_none() {
                    custom = chip(ui, &mut pick, None, "Custom").on_hover_text("Your own resolution, frame rate and bitrate, under More options").clicked();
                    let text = if current.is_none() { plan_text(&recorder::plan(&self.cfg, mon)) } else { "Set your own under More options".into() };
                    ui.label(RichText::new(text).color(pal().muted));
                    ui.end_row();
                }
            });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                recommend = button(ui, "Use recommended settings").on_hover_text("Sets resolution, frame rate, quality and clip length to suit this PC").clicked();
                if let Some(s) = &self.recommended {
                    status(ui, true, RichText::new(s).small(), accent_light(), 12.0);
                }
            });
            if self.monitors.len() > 1 {
                ui.add_space(6.0);
                row(ui, "Screen", |ui| monitor_combo(ui, &mut self.cfg, &self.monitors, "monitor", mon));
            }
        });
        if let Some(level) = pick.filter(|_| pick != current) {
            profile::set_preset(&mut self.cfg, &self.hw, mon, level);
        }
        if custom {
            self.set_more_open(Tab::Video, true);
        }
        if recommend {
            let s = profile::apply(&mut self.cfg, &self.hw, mon);
            log::write(&format!("Detected {}. Recommended settings: {s}", self.hw.summary()));
            self.recommended = Some(s);
        }
    }

    /// The note after OBS skipped frames while encoding: suggests the next lighter effort. The
    /// button only changes the form; Save still puts it into effect.
    pub(crate) fn encoder_hint(&mut self, ui: &mut egui::Ui) {
        let names = ["Best quality", "Balanced", "Max FPS"];
        let Some((n, effort)) = self.encoder_hint else { return };
        let Some(lighter) = recorder::lighter_effort(effort).filter(|_| self.cfg.encoder_effort == effort) else { return };
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("OBS skipped {n} frames while encoding. Try Encoder effort: {}.", names[lighter as usize])).color(pal().amber));
            if button(ui, format!("Use {}", names[lighter as usize])).on_hover_text("Switches to the lighter setting. Takes effect after Save.").clicked() {
                self.cfg.encoder_effort = lighter;
            }
            if bare_icon(ui, true, icon::CANCEL, "Dismiss") {
                self.encoder_hint = None;
            }
        });
    }

    /// A line when the recorded monitor (following the mouse: any monitor) has HDR on.
    pub(crate) fn hdr_note(&self, ui: &mut egui::Ui, mon: &display::Monitor) {
        if self.monitors.iter().any(|m| m.hdr.is_some() && (self.cfg.follows_mouse() || m.id == mon.id)) {
            ui.label(RichText::new("HDR monitor: clips are converted to normal colors.").small().color(pal().muted));
        }
    }

    pub(crate) fn video_tab(&mut self, ui: &mut egui::Ui) {
        // From the list made at start (with HDR), not looked up again every frame.
        let Some(mon) = recorder::pick_monitor(&self.monitors, &self.cfg) else {
            ui.label(RichText::new("No monitor found.").color(pal().muted));
            return;
        };
        self.this_pc_card(ui, &mon);
        self.more_options(ui, Tab::Video, |this, ui| this.video_more(ui, &mon));
    }

    /// The Video page's More options: capture, encoding, size and frame rate, what that makes,
    /// and importing from your own OBS.
    pub(crate) fn video_more(&mut self, ui: &mut egui::Ui, mon: &display::Monitor) {
        card(ui, "Capture", |ui| {
            row(ui, "Mode", |ui| {
                ui.horizontal_wrapped(|ui| {
                    chip(ui, &mut self.cfg.capture, "desktop".to_string(), "Desktop").on_hover_text("Records the whole monitor. Never touches the game, so it's safe with any anti-cheat. Popups and notifications on that monitor show up in clips.");
                    chip(ui, &mut self.cfg.capture, "game".to_string(), "Game capture").on_hover_text("Records just the game window. Some anti-cheats block it.");
                });
            });
            row(ui, "Resolution", |ui| {
                ui.horizontal_wrapped(|ui| {
                    chip(ui, &mut self.cfg.out_height, 0, format!("Native {}p", mon.height)).on_hover_text("Same size as your screen");
                    for h in [1440, 1080, 900, 720].into_iter().filter(|&h| h < mon.height) {
                        chip(ui, &mut self.cfg.out_height, h, format!("{h}p")).on_hover_text("Smaller video: smaller files, a little less sharp");
                    }
                });
            });
            row(ui, "Frame rate", |ui| {
                ui.horizontal_wrapped(|ui| {
                    for f in [30, 60, 120, 144, 240] {
                        chip(ui, &mut self.cfg.fps, f, f.to_string()).on_hover_text("Frames per second: higher is smoother, files get bigger");
                    }
                    chip(ui, &mut self.cfg.fps, 0, format!("Native {}", mon.hz)).on_hover_text("As smooth as your screen refreshes");
                });
            });
            if self.cfg.capture == "game" {
                ui.colored_label(pal().amber, "Game capture hooks into the game. Anti-cheats like Vanguard (Valorant), FACEIT or ESEA may block it or flag it. Use Desktop for those games.");
            } else {
                ui.label(RichText::new("Popups and notifications on that monitor show up in clips.").small().color(pal().muted));
                if self.monitors.len() > 1 {
                    ui.checkbox(&mut self.cfg.follow_mouse, "Follow the mouse")
                        .on_hover_text("Records whichever monitor your mouse is on, switching about 2 seconds after it moves over. Clips keep one size; a monitor with another resolution is scaled to fit.");
                }
            }
            self.hdr_note(ui, mon);
        });
        card(ui, "Encoding", |ui| {
            row(ui, "Bitrate", |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (i, q) in ["Highest", "High", "Medium", "Low"].into_iter().enumerate() {
                        chip(ui, &mut self.cfg.quality, i as u8, q).on_hover_text("Higher looks better and makes bigger files");
                    }
                });
            });
            row(ui, "Encoder", |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (v, name) in [("h264", "H.264"), ("hevc", "HEVC"), ("av1", "AV1")] {
                        chip(ui, &mut self.cfg.codec, v.to_string(), name).on_hover_text("The video format. H.264 plays everywhere; HEVC and AV1 make smaller files. AV1 needs a recent GPU (RTX 40, RX 7000, Intel Arc or newer).");
                    }
                });
            });
            row(ui, "Encoder effort", |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (i, (name, about)) in EFFORTS.into_iter().enumerate() {
                        chip(ui, &mut self.cfg.encoder_effort, i as u8, name).on_hover_text(about);
                    }
                });
            });
            self.encoder_hint(ui);
            if recorder::gpu() == recorder::Gpu::None {
                ui.label(RichText::new("No NVIDIA, AMD or Intel video encoder found, so clips are H.264 encoded on the CPU. That can cost some FPS in games.").small().color(pal().amber));
            }
        });
        let p = recorder::plan(&self.cfg, mon);
        card(ui, "Result", |ui| {
            for (k, v) in [
                ("Clip", format!("{}x{} at {} fps", p.out_w, p.out_h, p.fps)),
                ("Encoder", recorder::gpu().label().to_string()),
                ("Bitrate", format!("about {} Mbps", p.kbps / 1000)),
                ("Memory", format!("up to {:.1} GB of RAM", p.mb as f64 / 1024.0)),
                ("Disk", recorder::space_estimate(&self.cfg, p.kbps)),
            ] {
                row(ui, RichText::new(k).color(pal().muted), |ui| ui.label(RichText::new(v).strong()));
            }
            if let Some(secs) = p.fits_secs {
                let want = self.cfg.buffer_secs;
                ui.label(RichText::new(format!(
                    "A {}:{:02} replay at this quality needs more than a quarter of this PC's {} GB of RAM, so the replay keeps the last {}:{:02}. Lower the quality or the replay length to keep all of it.",
                    want / 60, want % 60, (profile::ram_mb() + 512) / 1024, secs / 60, secs % 60,
                )).color(pal().amber));
            }
        });
        self.obs_import_card(ui, mon, false);
    }
}
