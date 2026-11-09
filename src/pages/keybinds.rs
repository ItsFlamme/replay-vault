//! The Keybinds page: the keys, setting one by pressing it, and the overlays that fight over them.

use crate::*;

/// Which hotkey is being set.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Target {
    Save,
    Save30,
    Save60,
    Overlay,
    Record,
    Screenshot,
    Replay,
    Mic,
}

impl Target {
    pub(crate) const ALL: [Target; 8] =
        [Target::Save, Target::Save30, Target::Save60, Target::Overlay, Target::Record, Target::Screenshot, Target::Replay, Target::Mic];

    /// The app's own RegisterHotKey id; None for the save key, which belongs to OBS.
    pub(crate) fn action(self) -> Option<hotkey::Action> {
        Some(match self {
            Target::Save => return None,
            Target::Save30 => hotkey::Action::Save30,
            Target::Save60 => hotkey::Action::Save60,
            Target::Overlay => hotkey::Action::Overlay,
            Target::Record => hotkey::Action::Record,
            Target::Screenshot => hotkey::Action::Screenshot,
            Target::Replay => hotkey::Action::Replay,
            Target::Mic => hotkey::Action::Mic,
        })
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Target::Save => "Save key",
            Target::Save30 => "Save the last 0:30",
            Target::Save60 => "Save the last 1:00",
            Target::Overlay => "Show / hide this window",
            Target::Record => "Start / stop recording",
            Target::Screenshot => "Screenshot",
            Target::Replay => "Instant replay on / off",
            Target::Mic => "Mute / unmute mic",
        }
    }
}

/// The NVIDIA app's program, if it's installed where NVIDIA puts it.
pub(crate) fn nvidia_app() -> Option<std::path::PathBuf> {
    let exe = std::path::PathBuf::from(std::env::var_os("ProgramFiles")?).join(r"NVIDIA Corporation\NVIDIA app\CEF\NVIDIA App.exe");
    exe.is_file().then_some(exe)
}

impl App {
    pub(crate) fn hotkey_of(&mut self, target: Target) -> &mut String {
        match target {
            Target::Save => &mut self.cfg.hotkey,
            Target::Save30 => &mut self.cfg.hotkey_30,
            Target::Save60 => &mut self.cfg.hotkey_60,
            Target::Overlay => &mut self.cfg.overlay_hotkey,
            Target::Record => &mut self.cfg.hotkey_record,
            Target::Screenshot => &mut self.cfg.hotkey_screenshot,
            Target::Replay => &mut self.cfg.hotkey_replay,
            Target::Mic => &mut self.cfg.hotkey_mic,
        }
    }

    /// Registers the app's own hotkeys (all but the save key, which OBS has) when any key changed
    /// since the last time, but not while one is being set. An empty one is off. Keys another app
    /// held are tried again every few seconds (only those, so working keys aren't touched), and
    /// work as soon as it lets go: NVIDIA's overlay is closed on another thread at start, and
    /// ticking its box closes it a moment after.
    pub(crate) fn sync_hotkeys(&mut self) {
        let keys: Vec<String> = Target::ALL.into_iter().map(|t| self.hotkey_of(t).clone()).collect();
        let changed = self.registered.as_ref() != Some(&keys);
        let retry = !self.hotkey_failed.is_empty() && self.hotkeys_tried.elapsed() >= crate::HOTKEY_RETRY;
        if self.capturing.is_some() || !hotkey::is_running() || !(changed || retry) {
            return;
        }
        self.registered = Some(keys);
        self.hotkeys_tried = std::time::Instant::now();
        let mut errors = Vec::new();
        let mut failed = Vec::new();
        for target in Target::ALL {
            let Some(action) = target.action() else { continue };
            let text = self.hotkey_of(target).clone();
            let combo = hotkey::Combo::parse(&text);
            if combo.is_none() && !text.is_empty() {
                errors.push(format!("'{text}' isn't a key that works. Click it to set a new one."));
            }
            if !changed && !self.hotkey_failed.contains(&target) {
                continue;
            }
            // Another app (NVIDIA, Discord, ...) may hold the combo already.
            if let Err(e) = hotkey::set_combo(action, combo) {
                failed.push(target);
                let who = combo.map(|c| self.who_holds(c)).unwrap_or_default();
                errors.push(format!("{}: {e}{who}", target.name()));
            }
        }
        self.hotkey_failed = failed;
        self.hotkey_error = (!errors.is_empty()).then(|| errors.join(" "));
    }

    /// Which app likely holds `c`, said only when that app is there and uses it: NVIDIA (one of its
    /// shortcuts, which it holds even with its overlay off) or AMD Software (its keys, while on).
    fn who_holds(&mut self, c: hotkey::Combo) -> String {
        if let Some(shortcut) = tweaks::nvidia_shortcut_for(c) {
            return format!(" NVIDIA uses it for its \"{shortcut}\" shortcut: pick another key here, or change that one in NVIDIA's overlay (Settings > Keyboard shortcuts).");
        }
        if self.amd.get_or_insert_with(amd::detect).as_ref().is_some_and(|a| !a.clashes(&[("", c)]).is_empty()) {
            return " AMD Software uses it (AMD Software > Settings > Hotkeys).".into();
        }
        String::new()
    }

    pub(crate) fn start_capture(&mut self, target: Target, ctx: &egui::Context) {
        // Free the app's keys so they can be picked again.
        for a in hotkey::Action::ALL {
            let _ = hotkey::set_combo(a, None);
        }
        self.registered = None;
        self.capturing = Some(target);
        let ctx = ctx.clone();
        hotkey::start_capture(move || ctx.request_repaint());
    }

    pub(crate) fn finish_capture(&mut self) {
        let Some(target) = self.capturing else { return };
        if hotkey::is_capturing() {
            return;
        }
        self.capturing = None;
        let Some(c) = hotkey::take_captured() else {
            self.sync_hotkeys();
            return;
        };
        let name = c.to_string();
        // One key, one job.
        let taken = Target::ALL
            .into_iter()
            .find(|&t| t != target && hotkey::Combo::parse(self.hotkey_of(t)) == Some(c));
        let error = if let Some(other) = taken {
            Some(format!("{name} is already set for \"{}\". Pick another.", other.name()))
        } else if target == Target::Save && c.obs_binding().is_none() {
            Some(format!("OBS can't use {c} as a key. Pick another."))
        } else {
            *self.hotkey_of(target) = name;
            None
        };
        self.sync_hotkeys();
        if error.is_some() {
            self.hotkey_error = error;
        }
    }

    /// The button that shows and sets `target`'s key, at least `width` wide (one width for a
    /// list of them lines up what comes after).
    pub(crate) fn hotkey_button(&mut self, ui: &mut egui::Ui, target: Target, hint: &str, width: f32) {
        let current = self.hotkey_of(target).clone();
        let capturing = self.capturing == Some(target);
        let width = width.max(150.0);
        // A set key shows as keycaps, measured first so the button fits them.
        if !capturing && !current.is_empty() {
            let w = keycaps(&ui.painter().with_clip_rect(egui::Rect::NOTHING), egui::Pos2::ZERO, &current);
            // Minimalist: the keycaps are the button; its frame only shows on hover.
            let button = egui::Button::new("").min_size(egui::vec2((w + 24.0).max(width), H_BIG)).frame_when_inactive(retro());
            let r = ui.add_enabled(self.capturing.is_none(), button).on_hover_text(hint);
            keycaps(ui.painter(), egui::pos2(r.rect.left() + 12.0, r.rect.center().y), &current);
            if r.clicked() {
                self.start_capture(target, ui.ctx());
            }
            return;
        }
        // Minimalist: an empty key is quiet text where the keycaps would start, framed on hover.
        if !capturing && !retro() {
            let button = egui::Button::new("").min_size(egui::vec2(width, H_BIG)).frame_when_inactive(false);
            let r = ui.add_enabled(self.capturing.is_none(), button).on_hover_text(hint);
            ui.painter().text(egui::pos2(r.rect.left() + 12.0, r.rect.center().y), egui::Align2::LEFT_CENTER, "Not set", egui::FontId::proportional(14.0), pal().muted);
            if r.clicked() {
                self.start_capture(target, ui.ctx());
            }
            return;
        }
        let text = if capturing {
            "Press keys... (Esc cancels)".into()
        } else if current.is_empty() {
            "Not set".into()
        } else {
            current
        };
        let mut button = egui::Button::new(RichText::new(text).strong()).min_size(egui::vec2(width, H_BIG));
        if capturing {
            button = button.fill(accent_dim());
        }
        if cap(ui, self.capturing.is_none() || capturing, button).on_hover_text(hint).clicked() && !capturing {
            self.start_capture(target, ui.ctx());
        }
    }

    pub(crate) fn keybinds_tab(&mut self, ui: &mut egui::Ui) {
        // One width for every key button (the widest set key's), so the Clear buttons line up.
        let measure = ui.painter().with_clip_rect(egui::Rect::NOTHING);
        let width = Target::ALL.into_iter().filter_map(|t| Some(keycaps(&measure, egui::Pos2::ZERO, Some(self.hotkey_of(t).as_str()).filter(|k| !k.is_empty())?) + 24.0)).fold(150.0, f32::max);
        card(ui, "Keys", |ui| {
            form(ui, "keybinds", |ui| {
                ui.label("Save key");
                self.hotkey_button(ui, Target::Save, &format!("Saves the last {}. Click, then press the keys. OBS listens for it; takes effect after Save.", mmss(self.cfg.buffer_secs)), width);
                ui.end_row();

                ui.label("Show / hide this window");
                self.hotkey_button(ui, Target::Overlay, "Click, then press the keys. Works right away.", width);
                ui.end_row();

                let optional = [
                    (Target::Save30, "Saves a clip of only the last 0:30."),
                    (Target::Save60, "Saves a clip of only the last 1:00."),
                    (Target::Record, "Starts a full recording; press again to stop. Recordings go in the Recordings folder."),
                    (Target::Screenshot, "Saves a picture of the screen next to your clips."),
                    (Target::Replay, "Turns instant replay on or off."),
                    (Target::Mic, "Mutes or unmutes your mic in clips and recordings."),
                ];
                for (target, what) in optional {
                    ui.label(target.name());
                    ui.horizontal(|ui| {
                        self.hotkey_button(ui, target, &format!("Click, then press the keys. Works right away. {what}"), width);
                        if !self.hotkey_of(target).is_empty() && self.capturing.is_none() && button(ui, "Clear").on_hover_text("Removes this key").clicked() {
                            self.hotkey_of(target).clear();
                        }
                    });
                    ui.end_row();
                }
            });
            if let Some(err) = &self.hotkey_error {
                ui.colored_label(pal().red, err);
            }
            ui.add_space(4.0);
            note(ui, format!("The save key keeps the last {}, even in a game. Click a key, then press the combination you want; Esc cancels.", mmss(self.cfg.buffer_secs)));
            note(ui, "Use Ctrl, Alt, Shift or Win with a letter or number; F-keys, Pause, Insert, Home, End, Page Up/Down, Print Screen and Scroll Lock also work on their own.");
        });
        if crate::OVERLAY_SWITCHES {
            self.nvidia_card(ui);
            self.amd_card(ui);
        }
    }

    /// AMD Software's Instant Replay records alongside this app, and its hotkeys may be ours too.
    /// Shown when one of those clashes, or while this app has them switched off (untested).
    pub(crate) fn amd_card(&mut self, ui: &mut egui::Ui) {
        let Some(amd) = self.amd.get_or_insert_with(amd::detect).clone() else { return };
        let ours: Vec<(&str, hotkey::Combo)> = Target::ALL.into_iter().filter_map(|t| Some((t.name(), hotkey::Combo::parse(self.hotkey_of(t))?))).collect();
        let clashes = amd.clashes(&ours);
        let ours_off = self.cfg.amd_off || self.saved.amd_off;
        if !amd.replay_on && clashes.is_empty() && !ours_off {
            return;
        }
        card(ui, "AMD Software", |ui| {
            let step = |ui: &mut egui::Ui, text: String| {
                ui.label(RichText::new(format!("•  {text}")).color(pal().text)).on_hover_text("Change it in AMD Software yourself, or tick the box above.");
            };
            if amd.replay_on && !self.cfg.amd_off {
                ui.label(RichText::new("AMD Software's Instant Replay is on. It records in the background too, using the same video encoder.").small().color(pal().amber))
                    .on_hover_text("Two recorders on one GPU can drop frames in clips and cost FPS in games.");
            }
            if !self.cfg.amd_off {
                for (combo, name) in &clashes {
                    ui.label(RichText::new(format!("AMD Software also uses {combo}, set here for \"{name}\", so both apps would react to it.")).small().color(pal().amber))
                        .on_hover_text("Pick another key here, or change AMD's.");
                }
            }
            ui.checkbox(&mut self.cfg.amd_off, "Turn off AMD's Instant Replay and hotkeys (untested)").on_hover_text(
                "Sets the values AMD Software keeps for its Instant Replay and Use Hotkeys switches. Not tried on a PC with AMD Software yet, so check AMD Software afterwards. Unticking it, or Remove, puts your own settings back.",
            );
            if self.saved.amd_off {
                note(ui, "AMD Software reads this when it starts: restart the PC, then check that Record & Stream > Instant Replay and Hotkeys > Use Hotkeys are off. If they aren't, switch them off there and tell us.");
            }
            if !self.cfg.amd_off {
                note(ui, "Or switch them off yourself:");
                if amd.replay_on {
                    step(ui, "AMD Software > Settings > Record & Stream: turn off Instant Replay".into());
                }
                for (combo, _) in &clashes {
                    step(ui, format!("AMD Software > Settings > Hotkeys: change or turn off {combo}"));
                }
            }
            ui.add_space(2.0);
            if button(ui, "Check again").on_hover_text("Looks again at AMD Software's settings").clicked() {
                self.amd = None;
            }
        });
    }

    /// NVIDIA's overlay has its own Instant Replay and Save key (Alt+F10), so both apps would react.
    /// Shown when it's on, or when this app turned it off.
    pub(crate) fn nvidia_card(&mut self, ui: &mut egui::Ui) {
        let (on, running) = *self.nvidia.get_or_insert_with(|| (tweaks::nvidia_overlay_on(), tweaks::nvidia_overlay_running()));
        if !on && !running && !self.cfg.nvidia_off && !self.saved.nvidia_off {
            return;
        }
        card(ui, "NVIDIA overlay", |ui| {
            if on && !self.cfg.nvidia_off {
                ui.label(RichText::new("NVIDIA's overlay is on. Its Instant Replay records alongside ours, and its Save key (Alt+F10 unless you changed it) would save in both apps.").small().color(pal().amber));
            }
            ui.checkbox(&mut self.cfg.nvidia_off, "Turn off NVIDIA's overlay and Instant Replay")
                .on_hover_text("The same switch as in the NVIDIA app. Unticking it, or Remove, puts your own setting back.");
            if self.saved.nvidia_off && !on && running {
                ui.label(RichText::new("Switched off, but NVIDIA's overlay is still running. Restart the PC to finish.").small().color(pal().muted));
            } else if !on && !running {
                status(ui, true, RichText::new("NVIDIA's overlay is off").small(), accent_light(), 12.0);
            }
            if !self.cfg.nvidia_off {
                note(ui, "Or switch it off yourself: NVIDIA app > Settings > Features > NVIDIA Overlay > Off.");
            }
            ui.horizontal(|ui| {
                if !self.cfg.nvidia_off
                    && let Some(exe) = nvidia_app()
                    && button(ui, "Open NVIDIA app").on_hover_text("Opens the NVIDIA app, to change its overlay yourself").clicked()
                    && let Err(e) = std::process::Command::new(&exe).spawn()
                {
                    self.status = format!("Couldn't open the NVIDIA app: {e}");
                }
                if button(ui, "Check again").on_hover_text("Looks again whether NVIDIA's overlay is on").clicked() {
                    self.nvidia = None;
                }
            });
        });
    }
}
