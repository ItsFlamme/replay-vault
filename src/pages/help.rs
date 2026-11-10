//! The Help page (how recording is going, logs, the support zip) and the Feedback page.

use crate::*;

/// Opens a text file in Notepad, in front of everything.
pub(crate) fn open_in_notepad(path: &std::path::Path) {
    // Notepad starts through a broker process, so let it take focus, and get the
    // always-on-top overlay out of its way.
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(u32::MAX);
    }
    let _ = std::process::Command::new(recorder::windows_exe("notepad.exe")).arg(path).spawn();
    overlay::hide();
}

impl App {
    /// Re-reads the app log when it changes, at most once a second.
    pub(crate) fn refresh_log(&mut self) {
        if self.log_checked.elapsed() < std::time::Duration::from_secs(1) {
            return;
        }
        self.log_checked = std::time::Instant::now();
        let stamp = std::fs::metadata(log::path()).map(|m| (m.len(), m.modified().ok())).unwrap_or_default();
        if stamp == self.log_stamp {
            return;
        }
        self.log_stamp = stamp;
        let text = String::from_utf8_lossy(&std::fs::read(log::path()).unwrap_or_default()).into_owned();
        let lines: Vec<&str> = text.lines().collect();
        self.log_view = lines[lines.len().saturating_sub(500)..].iter().map(|l| l.to_string()).collect();
    }

    /// Zips the logs, settings (no secrets) and a PC summary onto the Desktop and shows it there.
    pub(crate) fn support_zip(&mut self) {
        let out = dirs::desktop_dir().unwrap_or_default().join("Replay Vault support.zip");
        let dir = config::dir();
        let mut files = vec![("app.log", log::path()), ("app.old.log", dir.join("app.old.log")), ("tweaks.json", dir.join("tweaks.json"))];
        files.extend(recorder::obs_log().map(|p| ("obs.log", p)));
        match support::write(&out, &files, &self.saved, &support::summary(&self.hw, &self.monitors)) {
            Ok(()) => {
                self.status = "Support zip saved to your Desktop.".into();
                clips::open(&out, true);
            }
            Err(e) => self.status = format!("Couldn't make the support zip: {e}"),
        }
    }

    /// Help: how recording is going, with Copy status, the support zip and a restart, then the
    /// performance, the health report and the log.
    pub(crate) fn help_tab(&mut self, ui: &mut egui::Ui) {
        self.refresh_log();
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
        let healthy = self.health_report().is_none_or(|l| l.ends_with("Health: OK"));
        let (ok, head, color, line) = if !self.connected {
            (false, "The recorder isn't running", pal().red, self.status.clone())
        } else if !self.buffer {
            (false, "Instant replay is off", pal().amber, "Turn it on with the switch at the bottom left.".to_string())
        } else if !healthy {
            (false, "Recording, but the last check found a problem", pal().amber, "Make a support zip and send it to us on Discord or GitHub (links on the Feedback page).".to_string())
        } else {
            (true, "Recording fine", accent_light(), format!("Press {} to save the last {}.", self.saved.hotkey, mmss(self.saved.buffer_secs)))
        };
        card(ui, "Status", |ui| {
            status(ui, ok, RichText::new(head).strong().size(16.0), color, 16.0);
            ui.label(RichText::new(line).color(pal().muted));
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if primary(ui, true, RichText::new("Copy status"), egui::Vec2::ZERO).on_hover_text("Copies how recording is going, the app version and this PC's graphics card, to paste when asking for help").clicked() {
                    ui.ctx().copy_text(self.status_text());
                    self.status = "Status copied.".into();
                }
                if cap(ui, true, egui::Button::new("Make support zip")).on_hover_text("Logs, settings (no passwords or keys) and a PC summary in one zip on your Desktop, to send when asking for help").clicked() {
                    self.support_zip();
                }
                if cap(ui, true, egui::Button::new("Restart recorder").min_size(egui::vec2(0.0, H_BIG))).on_hover_text(format!("Starts the hidden recorder again. What it kept so far (the last {}) is lost.", mmss(self.saved.buffer_secs))).clicked() {
                    self.apply();
                }
            });
        });
        self.help_details(ui);
    }

    /// The newest health report line in the log, if there is one.
    pub(crate) fn health_report(&self) -> Option<&String> {
        self.log_view.iter().rev().find(|l| l.contains("SESSION REPORT") || l.contains("Hourly check"))
    }

    /// What Copy status puts on the clipboard: the app, how recording is going, this PC, and the
    /// last health report. No paths, keys or names beyond what the status line says.
    pub(crate) fn status_text(&self) -> String {
        let replay = match (self.connected, self.buffer) {
            (true, true) => format!("on, keeping the last {} (save key {})", mmss(self.saved.buffer_secs), self.saved.hotkey),
            (true, false) => "off".to_string(),
            _ => "recorder not running".to_string(),
        };
        let report = self.health_report().map_or("none yet", String::as_str);
        format!(
            "Replay Vault {}\nInstant replay: {replay}\nStatus: {}\nEncoder: {}\nPC: {}; {}\nLast health report: {report}",
            env!("CARGO_PKG_VERSION"),
            self.status,
            recorder::gpu().label(),
            support::windows_version(),
            self.hw.summary()
        )
    }

    /// Live performance, the last health report and the log.
    pub(crate) fn help_details(&mut self, ui: &mut egui::Ui) {
        let perf = self.perf.get_or_insert_with(perf::Sampler::new);
        perf.tick(recorder::pid());
        let (rec, app) = (perf.recorder, perf.app);
        card(ui, "Performance", |ui| {
            let cell = |ui: &mut egui::Ui, v: Option<String>| {
                ui.label(RichText::new(v.unwrap_or("-".into())).color(pal().text));
            };
            egui::Grid::new("perf").num_columns(5).spacing([28.0, 8.0]).min_col_width(70.0).show(ui, |ui| {
                for head in ["", "CPU", "GPU", "Video encoder", "Memory"] {
                    ui.label(RichText::new(head).small().color(pal().muted));
                }
                ui.end_row();
                for (name, u) in [("Recorder (OBS)", rec), ("Replay Vault", app)] {
                    ui.label(name);
                    cell(ui, u.map(|u| format!("{:.1}%", u.cpu)));
                    cell(ui, u.map(|u| format!("{:.1}%", u.gpu)));
                    cell(ui, u.map(|u| format!("{:.1}%", u.encoder)));
                    cell(ui, u.map(|u| if u.ram_mb >= 1024.0 { format!("{:.2} GB", u.ram_mb / 1024.0) } else { format!("{:.0} MB", u.ram_mb) }));
                    ui.end_row();
                }
            });
            ui.add_space(2.0);
            let note = if rec.is_none() {
                "The recorder isn't running."
            } else {
                "Live, while this page is open. CPU is a share of all cores. The video encoder is a separate chip on the GPU that games don't use. The recorder's memory is mostly the last few minutes it keeps for the next clip."
            };
            // The explanation is a tip; "isn't running" is a status and always shows.
            if rec.is_none() || tips_on() {
                ui.label(RichText::new(note).small().color(pal().muted));
            }
        });

        let report = self.health_report();
        card(ui, "Last health report", |ui| match report {
            Some(line) => {
                let ok = line.ends_with("Health: OK");
                status(ui, ok, RichText::new(if ok { "Healthy" } else { "Needs a look" }).strong(), if ok { accent_light() } else { pal().amber }, 14.0);
                ui.label(RichText::new(line).small().color(pal().muted));
            }
            None => {
                ui.label(RichText::new("No report yet. One is written when replay turns off, when the app closes, and every hour.").small().color(pal().muted));
            }
        });

        card(ui, "Log", |ui| {
            ui.horizontal(|ui| {
                if button(ui, "Open in Notepad").on_hover_text("Opens the whole log in Notepad").clicked() {
                    open_in_notepad(&log::path());
                }
                if let Some(obs_log) = recorder::obs_log()
                    && button(ui, "Open OBS log").on_hover_text("The hidden recorder's own log for its current run").clicked() {
                    open_in_notepad(&obs_log);
                }
            });
            ui.add_space(6.0);
            egui::ScrollArea::vertical()
                .id_salt("log")
                .auto_shrink(false)
                .max_height(320.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    if self.log_view.is_empty() {
                        ui.label(RichText::new("Nothing logged yet.").small().color(pal().muted));
                    }
                    for line in &self.log_view {
                        let color = if line.contains("Error") || line.contains("CRASH") || line.contains("didn't") || line.contains("Health: CHECK") {
                            pal().red
                        } else if line.contains("Clip saved") || line.contains("Health: OK") {
                            accent_light()
                        } else {
                            pal().muted
                        };
                        ui.add(egui::Label::new(RichText::new(line).monospace().size(11.0).color(color)).wrap());
                    }
                });
        });
    }

    /// A message to the developer, with a PC summary and the end of the log if left ticked.
    pub(crate) fn feedback_card(&mut self, ui: &mut egui::Ui) {
        use feedback::{Kind, State};
        self.feedback.poll();
        let mut send = false;
        card(ui, "Send feedback", |ui| {
            let f = &mut self.feedback;
            ui.horizontal(|ui| {
                chip(ui, &mut f.kind, Kind::Bug, "Bug").on_hover_text("Something doesn't work right");
                chip(ui, &mut f.kind, Kind::Idea, "Idea").on_hover_text("Something you'd like it to do");
                chip(ui, &mut f.kind, Kind::Other, "Other").on_hover_text("Anything else");
            });
            ui.add_space(6.0);
            ui.add(egui::TextEdit::multiline(&mut f.message).hint_text("What happened, or what would you like?").char_limit(feedback::MAX_MESSAGE).desired_rows(4).desired_width(f32::INFINITY))
                .on_hover_text("Your message (needed to send)");
            ui.label(RichText::new(format!("{} / {}", f.message.chars().count(), feedback::MAX_MESSAGE)).small().color(pal().muted));
            ui.add(egui::TextEdit::singleline(&mut f.contact).hint_text("Discord name or email, if you want a reply").char_limit(feedback::MAX_CONTACT).desired_width(f32::INFINITY))
                .on_hover_text("Optional. Only used to answer you.");
            ui.checkbox(&mut f.include, "Include PC summary and recent log lines").on_hover_text(format!(
                "Always sent: your message, Bug / Idea / Other, your contact if you gave one, and the app version.\nTicked, also: the Windows version, GPU name and memory, RAM, and the last {} lines of the app log (at most {} characters) with your user folder, user name, email addresses, link query strings and keys taken out.",
                feedback::LOG_LINES,
                feedback::MAX_LOG
            ));
            ui.add_space(4.0);
            let sending = matches!(f.state, State::Sending(_));
            let wait = f.wait_left();
            let problem = f.problem();
            let later = match wait {
                0 => String::new(),
                s if s < 90 => format!("You can send again in {s} seconds."),
                s if s < 5400 => format!("You can send again in {} minutes.", s.div_ceil(60)),
                s => format!("You can send again in {} hours.", s.div_ceil(3600)),
            };
            ui.horizontal(|ui| {
                let why = if sending { "Sending...".to_string() } else if wait > 0 { later.clone() } else { problem.unwrap_or_default().to_string() };
                send = primary(ui, !sending && wait == 0 && problem.is_none(), RichText::new("Send").strong(), egui::vec2(80.0, 0.0))
                    .on_hover_text("Sends it to the developer of Replay Vault")
                    .on_disabled_hover_text(why)
                    .clicked();
                ui.hyperlink_to("Report on GitHub", feedback::GITHUB_ISSUES).on_hover_text("Opens a new GitHub issue in your browser (needs a GitHub account)");
                ui.hyperlink_to("Ask in Discord", feedback::DISCORD).on_hover_text("Opens the Replay Vault Discord in your browser");
            });
            match &f.state {
                State::Idle => {}
                State::Sending(_) => {
                    ui.label(RichText::new("Sending...").small().color(pal().muted));
                }
                State::Sent => {
                    status(ui, true, RichText::new("Thanks, it was sent").small(), accent_light(), 12.0);
                }
                State::Error(e) => {
                    ui.label(RichText::new(e).small().color(pal().red));
                }
            }
            if wait > 0 {
                if !sending && wait > feedback::WAIT.as_secs() {
                    ui.label(RichText::new(format!("{later} You can send {} a day here. For anything more serious, post in the Discord or on GitHub.", feedback::PER_DAY)).small().color(pal().muted));
                }
                ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
            }
        });
        if !send {
            return;
        }
        // The log as it is now, even if Help (which reads it) wasn't opened first.
        if let Some(t) = std::time::Instant::now().checked_sub(std::time::Duration::from_secs(5)) {
            self.log_checked = t;
        }
        self.refresh_log();
        let f = &self.feedback;
        let (pc, log) = if f.include {
            let home = dirs::home_dir().map(|h| h.display().to_string());
            let user = std::env::var("USERNAME").ok();
            let secrets: Vec<&str> = [&self.cfg.recorder_password, &self.cfg.discord_webhook].into_iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
            (format!("{}; {}", support::windows_version(), self.hw.summary()), feedback::log_tail(&self.log_view, home.as_deref(), user.as_deref(), &secrets))
        } else {
            (String::new(), String::new())
        };
        let body = feedback::payload(f.kind, &f.message, &f.contact, &pc, &log).to_string();
        let ctx = ui.ctx().clone();
        self.feedback.start(body, move || ctx.request_repaint());
    }
}
