//! The Keybinds page: the keys, setting one by pressing it, and the overlays that fight over them.

use crate::*;

/// Which hotkey is being set.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Target {
    Save,
    Overlay,
    Record,
    Screenshot,
    Replay,
    Mic,
}

impl Target {
    pub(crate) const ALL: [Target; 6] = [Target::Save, Target::Overlay, Target::Record, Target::Screenshot, Target::Replay, Target::Mic];

    /// The app's own RegisterHotKey id; None for the save key, which belongs to OBS.
    pub(crate) fn action(self) -> Option<hotkey::Action> {
        Some(match self {
            Target::Save => return None,
            Target::Overlay => hotkey::Action::Overlay,
            Target::Record => hotkey::Action::Record,
            Target::Screenshot => hotkey::Action::Screenshot,
            Target::Replay => hotkey::Action::Replay,
            Target::Mic => hotkey::Action::Mic,
        })
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Target::Save => "Save clip",
            Target::Overlay => "Show / hide this window",
            Target::Record => "Start / stop recording",
            Target::Screenshot => "Screenshot",
            Target::Replay => "Instant replay on / off",
            Target::Mic => "Mute / unmute mic",
        }
    }
}

/// Who else uses a key, as far as can be told.
pub(crate) enum Holder {
    /// One of NVIDIA's overlay shortcuts, by name.
    Nvidia(&'static str),
    Amd,
    /// Something holds it, but not one of the apps whose keys can be read.
    Unknown,
}

/// What a clash means, in a sentence or two. The key still reaches this app (the save key
/// through OBS, which listens for it itself, the others read from the key state), so both react.
pub(crate) fn clash_text(key: &str, holder: &Holder) -> String {
    let (who, fix) = match holder {
        Holder::Nvidia(s) => (
            format!("NVIDIA's \"{s}\" shortcut"),
            "If you don't use it there, that's fine. Otherwise pick another key, or change it in NVIDIA's overlay (Settings > Keyboard shortcuts).",
        ),
        Holder::Amd => ("AMD Software".to_string(), "If you don't use it there, that's fine. Otherwise pick another key, or change it in AMD Software > Settings > Hotkeys."),
        Holder::Unknown => ("Another program (or Windows)".to_string(), "Pick another key, or change it in the program that uses it."),
    };
    format!("{who} also uses {key}, so pressing it does both. {fix}")
}

/// How often the Keybinds page looks again for keys other programs took.
const CONFLICT_RECHECK: std::time::Duration = std::time::Duration::from_secs(5);

/// The NVIDIA app's program, if it's installed where NVIDIA puts it.
pub(crate) fn nvidia_app() -> Option<std::path::PathBuf> {
    let exe = std::path::PathBuf::from(std::env::var_os("ProgramFiles")?).join(r"NVIDIA Corporation\NVIDIA app\CEF\NVIDIA App.exe");
    exe.is_file().then_some(exe)
}

impl App {
    pub(crate) fn hotkey_of(&mut self, target: Target) -> &mut String {
        match target {
            Target::Save => &mut self.cfg.hotkey,
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
            // Another app (NVIDIA, Discord, ...) may hold the combo already. Then the key is shared
            // (it still works here, and there too), `check_conflicts` says who has it, and it's
            // tried again until it's free to be this app's alone.
            match hotkey::set_combo(action, combo) {
                Ok(hotkey::Held::Ours) => {
                    if combo.is_some() && (changed || self.hotkey_failed.contains(&target)) {
                        crate::log::write(&format!("Key {text} for {}: registered, this app's own.", target.name()));
                    }
                }
                Ok(hotkey::Held::Shared) => {
                    if changed {
                        crate::log::write(&format!("Key {text} for {}: another program holds it, shared (read from the key state).", target.name()));
                    }
                    failed.push(target)
                }
                Err(e) => {
                    crate::log::write(&format!("Key {text} for {}: {e}", target.name()));
                    errors.push(e);
                    failed.push(target);
                }
            }
        }
        self.hotkey_failed = failed;
        self.hotkey_error = (!errors.is_empty()).then(|| errors.join(" "));
    }

    /// Looks for keys another program or Windows also uses, every few seconds and whenever the
    /// keys change, so a clash shows before the key is ever pressed. The app's own keys that
    /// registered are fine as far as Windows goes; every other key (the save key, which OBS
    /// listens for, and any that failed) is tried for a moment to see if something holds it.
    /// NVIDIA's and AMD's shortcuts are named; anything else is "another program".
    pub(crate) fn check_conflicts(&mut self) {
        let keys: Vec<String> = Target::ALL.into_iter().map(|t| self.hotkey_of(t).clone()).collect();
        let fresh = self.conflicts_checked.is_some_and(|t| t.elapsed() < CONFLICT_RECHECK) && self.conflicts_keys == keys;
        if self.capturing.is_some() || fresh {
            return;
        }
        let amd = self.amd.get_or_insert_with(amd::detect).clone();
        let mut found = Vec::new();
        for (target, key) in Target::ALL.into_iter().zip(&keys) {
            let Some(c) = hotkey::Combo::parse(key) else { continue };
            let ours = target.action().is_some() && self.registered.is_some() && !self.hotkey_failed.contains(&target);
            let held = !ours && hotkey::held_elsewhere(c);
            let on_amd = amd.as_ref().is_some_and(|a| !a.clashes(&[("", c)]).is_empty());
            let holder = match (held, tweaks::nvidia_shortcut_for(c)) {
                (true, Some(s)) => Holder::Nvidia(s),
                _ if on_amd => Holder::Amd,
                (true, None) => Holder::Unknown,
                (false, _) => continue,
            };
            found.push((target, clash_text(key, &holder)));
        }
        self.conflicts = found;
        self.conflicts_keys = keys;
        self.conflicts_checked = Some(std::time::Instant::now());
    }

    /// The clashes `check_conflicts` found, for `only` one key or all of them.
    pub(crate) fn conflict_notes(&self, ui: &mut egui::Ui, only: Option<Target>) {
        for (target, text) in self.conflicts.iter().filter(|(t, _)| only.is_none_or(|o| o == *t)) {
            ui.label(RichText::new(format!("{}: {text}", target.name())).small().color(pal().amber));
        }
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
        self.capture_ended = Some(std::time::Instant::now());
        let Some(c) = hotkey::take_captured() else {
            self.sync_hotkeys();
            return;
        };
        let name = c.to_string();
        // One key, one job: a key set for another action moves here, except the save key's,
        // which would leave clips with no way to be saved. Keys other programs use are kept
        // (`check_conflicts` says so).
        let taken = Target::ALL
            .into_iter()
            .find(|&t| t != target && hotkey::Combo::parse(self.hotkey_of(t)) == Some(c));
        let error = if target == Target::Save && c.obs_binding().is_none() {
            Some(format!("OBS can't use {c} as a key. Pick another."))
        } else if taken == Some(Target::Save) {
            Some(format!("{name} is the Save clip key. Give Save clip another key first."))
        } else {
            if let Some(other) = taken {
                self.hotkey_of(other).clear();
                self.status = format!("{name} moved from \"{}\" to \"{}\".", other.name(), target.name());
            }
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
                ui.label(Target::Save.name());
                self.hotkey_button(ui, Target::Save, &format!("Saves the last {}. Click, then press the keys. OBS listens for it; takes effect after Save.", mmss(self.cfg.buffer_secs)), width);
                ui.end_row();

                ui.label("Show / hide this window");
                self.hotkey_button(ui, Target::Overlay, "Click, then press the keys. Works right away.", width);
                ui.end_row();

                let optional = [
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
            self.check_conflicts();
            self.conflict_notes(ui, None);
            ui.add_space(4.0);
            note(ui, format!("Save clip keeps the last {}, even in a game. Click a key, then press the combination you want; Esc cancels.", mmss(self.cfg.buffer_secs)));
            note(ui, "Use Ctrl, Alt, Shift or Win with a letter or number; F-keys, Pause, Insert, Home, End, Page Up/Down, Print Screen and Scroll Lock also work on their own.");
        });
        self.overlay_card(ui);
    }

    /// One switch for the graphics card's own recorder: NVIDIA's overlay (its Instant Replay and
    /// keys like Alt+F10, Alt+R) and AMD Software's Instant Replay and hotkeys (untested), for
    /// whichever of them this PC has. Shown when either is found, or while this app has one off.
    pub(crate) fn overlay_card(&mut self, ui: &mut egui::Ui) {
        let (on, running) = *self.nvidia.get_or_insert_with(|| (tweaks::nvidia_overlay_on(), tweaks::nvidia_overlay_running()));
        let amd = self.amd.get_or_insert_with(amd::detect).clone();
        let has_nvidia = on || running || self.cfg.nvidia_off || self.saved.nvidia_off;
        let has_amd = amd.is_some() || self.cfg.amd_off || self.saved.amd_off;
        if !has_nvidia && !has_amd {
            return;
        }
        let ours: Vec<(&str, hotkey::Combo)> = Target::ALL.into_iter().filter_map(|t| Some((t.name(), hotkey::Combo::parse(self.hotkey_of(t))?))).collect();
        let clashes = amd.as_ref().map_or_else(Vec::new, |a| a.clashes(&ours));
        let amd_replay = amd.as_ref().is_some_and(|a| a.replay_on);
        let mut off = (has_nvidia && self.cfg.nvidia_off) || (has_amd && self.cfg.amd_off);
        let label = match (has_nvidia, has_amd) {
            (true, true) => "Turn off NVIDIA's and AMD's overlays (AMD untested)",
            (true, false) => "Turn off NVIDIA's overlay",
            _ => "Turn off AMD's overlay (untested)",
        };
        card(ui, "Graphics card overlay", |ui| {
            let step = |ui: &mut egui::Ui, text: String| {
                ui.label(RichText::new(format!("•  {text}")).color(pal().text)).on_hover_text("Change it there yourself, or tick the box above.");
            };
            if !off {
                if on {
                    ui.label(RichText::new("NVIDIA's overlay is on. Its Instant Replay records alongside ours, and its keys (Alt+F10, Alt+R and more) work in both apps.").small().color(pal().amber));
                }
                if amd_replay {
                    ui.label(RichText::new("AMD Software's Instant Replay is on. It records in the background too, using the same video encoder.").small().color(pal().amber))
                        .on_hover_text("Two recorders on one GPU can drop frames in clips and cost FPS in games.");
                }
                for (combo, name) in &clashes {
                    ui.label(RichText::new(format!("AMD Software also uses {combo}, set here for \"{name}\", so both apps would react to it.")).small().color(pal().amber))
                        .on_hover_text("Pick another key here, or change AMD's.");
                }
            }
            let mut hover = Vec::new();
            if has_nvidia {
                hover.push("NVIDIA: the same switch as in the NVIDIA app. Its Instant Replay stops recording alongside ours.");
            }
            if has_amd {
                hover.push("AMD: sets the values AMD Software keeps for its Instant Replay and Use Hotkeys switches. Not tried on a PC with AMD Software yet, so check AMD Software afterwards.");
            }
            hover.push("Unticking it, or Remove, puts your own settings back.");
            if ui.checkbox(&mut off, label).on_hover_text(hover.join("\n")).changed() {
                if has_nvidia {
                    self.cfg.nvidia_off = off;
                }
                if has_amd {
                    self.cfg.amd_off = off;
                }
            }
            if has_nvidia {
                if self.saved.nvidia_off && !on && running {
                    ui.label(RichText::new("Switched off, but NVIDIA's overlay is still running. Restart the PC to finish.").small().color(pal().muted));
                } else if !on && !running {
                    status(ui, true, RichText::new("NVIDIA's overlay is off").small(), accent_light(), 12.0);
                }
            }
            if has_amd && self.saved.amd_off {
                note(ui, "AMD Software reads this when it starts: restart the PC, then check that Record & Stream > Instant Replay and Hotkeys > Use Hotkeys are off. If they aren't, switch them off there and tell us.");
            }
            if !off {
                note(ui, "Or switch it off yourself:");
                if has_nvidia {
                    step(ui, "NVIDIA app > Settings > Features > NVIDIA Overlay > Off".into());
                }
                if amd_replay {
                    step(ui, "AMD Software > Settings > Record & Stream: turn off Instant Replay".into());
                }
                for (combo, _) in &clashes {
                    step(ui, format!("AMD Software > Settings > Hotkeys: change or turn off {combo}"));
                }
            }
            ui.horizontal(|ui| {
                if has_nvidia
                    && !off
                    && let Some(exe) = nvidia_app()
                    && button(ui, "Open NVIDIA app").on_hover_text("Opens the NVIDIA app, to change its overlay yourself").clicked()
                    && let Err(e) = std::process::Command::new(&exe).spawn()
                {
                    self.status = format!("Couldn't open the NVIDIA app: {e}");
                }
                if button(ui, "Check again").on_hover_text("Looks again for NVIDIA's overlay and AMD Software").clicked() {
                    (self.nvidia, self.amd) = (None, None);
                }
            });
        });
    }
}
