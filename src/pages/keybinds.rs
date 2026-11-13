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

    /// This key in `cfg`.
    pub(crate) fn key(self, cfg: &Config) -> &str {
        match self {
            Target::Save => &cfg.hotkey,
            Target::Overlay => &cfg.overlay_hotkey,
            Target::Record => &cfg.hotkey_record,
            Target::Screenshot => &cfg.hotkey_screenshot,
            Target::Replay => &cfg.hotkey_replay,
            Target::Mic => &cfg.hotkey_mic,
        }
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
#[derive(Debug, PartialEq)]
pub(crate) enum Holder {
    /// One of NVIDIA's overlay shortcuts, by name.
    Nvidia(&'static str),
    Amd,
    /// Something holds it, but not one of the apps whose keys can be read.
    Unknown,
}

/// Who else reacts to a key: NVIDIA when it's one of NVIDIA's shortcuts (`nvidia`, its name) and
/// something holds it, or, if this app registered the key first, while NVIDIA's overlay is on
/// (`nvidia_on`), as NVIDIA's helper listens for its keys its own way (it keeps running and its
/// values stay with the overlay off); then AMD Software when it's one of its hotkeys; then anything
/// else that holds it (`held`).
pub(crate) fn holder(held: bool, nvidia: Option<&'static str>, nvidia_on: bool, on_amd: bool) -> Option<Holder> {
    match nvidia {
        Some(s) if held || nvidia_on => Some(Holder::Nvidia(s)),
        _ if on_amd => Some(Holder::Amd),
        _ => held.then_some(Holder::Unknown),
    }
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

/// How a key is set, on every key's hover text.
const PRESS: &str = "Click, then press the keys you want; Esc cancels. Use Ctrl, Alt, Shift or Win with a letter or number; F-keys, Pause, Insert, Home, End, Page Up/Down, Print Screen and Scroll Lock also work on their own.";

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

    /// Every key as saved, which is what's registered: a key changed on the page works after Save.
    pub(crate) fn saved_keys(&self) -> Vec<String> {
        Target::ALL.into_iter().map(|t| t.key(&self.saved).to_string()).collect()
    }

    /// Registers the app's own hotkeys (all but the save key, which OBS has) as saved, when any
    /// changed since the last time (after Save), but not while one is being set. An empty one is
    /// off. Keys another app held are tried again after a few seconds, then less and less often
    /// (only those, so working keys aren't touched), and work once it lets go: NVIDIA's overlay is
    /// closed on another thread at start, and ticking its box closes it a moment after.
    pub(crate) fn sync_hotkeys(&mut self) {
        let keys = self.saved_keys();
        let changed = self.registered.as_ref() != Some(&keys);
        let retry = !self.hotkey_failed.is_empty() && self.hotkeys_tried.elapsed() >= self.hotkey_retry;
        if self.capturing.is_some() || !hotkey::is_running() || !(changed || retry) {
            return;
        }
        self.registered = Some(keys);
        self.hotkeys_tried = std::time::Instant::now();
        // A key still held after a retry is tried less often; a change starts over.
        self.hotkey_retry = if changed { crate::HOTKEY_RETRY } else { crate::next_hotkey_retry(self.hotkey_retry) };
        let mut errors = Vec::new();
        let mut failed = Vec::new();
        for target in Target::ALL {
            let Some(action) = target.action() else { continue };
            let text = target.key(&self.saved).to_string();
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
    /// registered (as saved) are fine as far as Windows goes; every other key (the save key, which
    /// OBS listens for, any that failed, and any changed but not saved yet) is tried for a moment to
    /// see if something holds it.
    /// NVIDIA's and AMD's shortcuts are named; anything else is "another program".
    pub(crate) fn check_conflicts(&mut self) {
        let keys: Vec<String> = Target::ALL.into_iter().map(|t| self.hotkey_of(t).clone()).collect();
        let fresh = self.conflicts_checked.is_some_and(|t| t.elapsed() < CONFLICT_RECHECK) && self.conflicts_keys == keys;
        if self.capturing.is_some() || fresh {
            return;
        }
        let amd = self.amd.get_or_insert_with(amd::detect).clone();
        let nvidia = tweaks::nvidia_shortcuts();
        let nvidia_on = self.nvidia.get_or_insert_with(|| (tweaks::nvidia_overlay_on(), tweaks::nvidia_overlay_running())).0;
        let mut found = Vec::new();
        for (target, key) in Target::ALL.into_iter().zip(&keys) {
            let Some(c) = hotkey::Combo::parse(key) else { continue };
            let ours = target.action().is_some() && self.registered.is_some() && !self.hotkey_failed.contains(&target) && *key == target.key(&self.saved);
            let held = !ours && hotkey::held_elsewhere(c);
            let on_amd = amd.as_ref().is_some_and(|a| !a.clashes(&[("", c)]).is_empty());
            let on_nvidia = nvidia.iter().find(|(n, _)| *n == c).map(|(_, s)| *s);
            let Some(holder) = holder(held, on_nvidia, nvidia_on, on_amd) else { continue };
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
            // Unframed, the keycaps are the button; its frame only shows on hover.
            let button = egui::Button::new("").min_size(egui::vec2((w + 24.0).max(width), H_BIG)).frame_when_inactive(look().keycap.framed);
            let r = ui.add_enabled(self.capturing.is_none(), button).on_hover_text(hint);
            keycaps(ui.painter(), egui::pos2(r.rect.left() + 12.0, r.rect.center().y), &current);
            if r.clicked() {
                self.start_capture(target, ui.ctx());
            }
            return;
        }
        // Unframed, an empty key is quiet text where the keycaps would start, framed on hover.
        if !capturing && !look().keycap.framed {
            let button = egui::Button::new("").min_size(egui::vec2(width, H_BIG)).frame_when_inactive(false);
            let r = ui.add_enabled(self.capturing.is_none(), button).on_hover_text(hint);
            let (text, font) = match look().key_text {
                Some(k) => ("NOT SET", k.face.font(k.size)),
                None => ("Not set", egui::FontId::proportional(14.0)),
            };
            ui.painter().text(egui::pos2(r.rect.left() + 12.0, r.rect.center().y), egui::Align2::LEFT_CENTER, text, font, pal().muted);
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
        // One width for every key button (the widest set key's), so they look like one set.
        let measure = ui.painter().with_clip_rect(egui::Rect::NOTHING);
        let width = Target::ALL.into_iter().filter_map(|t| Some(keycaps(&measure, egui::Pos2::ZERO, Some(self.hotkey_of(t).as_str()).filter(|k| !k.is_empty())?) + 24.0)).fold(150.0, f32::max);
        card(ui, "Keys", |ui| {
            let keeps = mmss(self.cfg.buffer_secs);
            let keys = [
                (Target::Save, format!("Saves the last {keeps}, even in a game. OBS listens for it; takes effect after Save. {PRESS}")),
                (Target::Overlay, format!("Takes effect after Save. {PRESS}")),
                (Target::Record, format!("Starts a full recording; press again to stop. Recordings go in the Recordings folder. Takes effect after Save. {PRESS}")),
                (Target::Screenshot, format!("Saves a picture of the screen next to your clips. Takes effect after Save. {PRESS}")),
                (Target::Replay, format!("Turns instant replay on or off. Takes effect after Save. {PRESS}")),
                (Target::Mic, format!("Mutes or unmutes your mic in clips and recordings. Takes effect after Save. {PRESS}")),
            ];
            let idle = self.capturing.is_none();
            for (target, hint) in keys {
                let set = !self.hotkey_of(target).is_empty();
                let mut slot = false;
                // Save clip always has a key (clips need one), so its slot stays empty. The others
                // can be cleared, or set when they have none, like clicking the key.
                row_slot(ui, target.name(), |ui| self.hotkey_button(ui, target, &hint, width), |ui| {
                    if target != Target::Save {
                        let (text, about) = if set { ("Clear", "Removes this key") } else { ("Set", PRESS) };
                        slot = slot_button(ui, idle, text).on_hover_text(about).clicked();
                    }
                });
                if slot && set {
                    self.hotkey_of(target).clear();
                } else if slot {
                    self.start_capture(target, ui.ctx());
                }
            }
            if let Some(err) = &self.hotkey_error {
                ui.colored_label(pal().red, err);
            }
            self.check_conflicts();
            self.conflict_notes(ui, None);
        });
        self.overlay_card(ui);
    }

    /// One switch for the graphics card's own recorder: NVIDIA's overlay (its Instant Replay and
    /// keys like Alt+F10, Alt+R) and AMD Software's Instant Replay and hotkeys (untested), for
    /// whichever of them this PC has. Shown when either is found, or while this app has one off.
    pub(crate) fn overlay_card(&mut self, ui: &mut egui::Ui) {
        let (on, running) = *self.nvidia.get_or_insert_with(|| (tweaks::nvidia_overlay_on(), tweaks::nvidia_overlay_running()));
        let amd = self.amd.get_or_insert_with(amd::detect).clone();
        // A copy that may not change them never switches either off, whatever its settings say.
        let allowed = crate::instance::current().windows_tweaks_allowed();
        let has_nvidia = on || running || (allowed && (self.cfg.nvidia_off || self.saved.nvidia_off));
        let has_amd = amd.is_some() || (allowed && (self.cfg.amd_off || self.saved.amd_off));
        if !has_nvidia && !has_amd {
            return;
        }
        let ours: Vec<(&str, hotkey::Combo)> = Target::ALL.into_iter().filter_map(|t| Some((t.name(), hotkey::Combo::parse(self.hotkey_of(t))?))).collect();
        let clashes = amd.as_ref().map_or_else(Vec::new, |a| a.clashes(&ours));
        let amd_replay = amd.as_ref().is_some_and(|a| a.replay_on);
        let mut off = allowed && ((has_nvidia && self.cfg.nvidia_off) || (has_amd && self.cfg.amd_off));
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
                hover.push("AMD: sets the values AMD Software keeps for its Instant Replay and Use Hotkeys switches. Not tried on a PC with AMD Software yet. AMD Software reads them when it starts: restart the PC, then check that Record & Stream > Instant Replay and Hotkeys > Use Hotkeys are off. If they aren't, switch them off there and tell us.");
            }
            hover.push("Unticking it, or Remove, puts your own settings back.");
            if allowed_checkbox(ui, allowed, &mut off, label, crate::instance::TWEAKS_OFF).on_hover_text(hover.join("\n")).changed() {
                if has_nvidia {
                    self.cfg.nvidia_off = off;
                }
                if has_amd {
                    self.cfg.amd_off = off;
                }
            }
            if has_nvidia {
                if allowed && self.saved.nvidia_off && !on && running {
                    ui.label(RichText::new("Switched off, but NVIDIA's overlay is still running. Restart the PC to finish.").small().color(pal().muted));
                } else if !on && !running {
                    status(ui, true, RichText::new("NVIDIA's overlay is off").small(), accent_light(), 12.0);
                }
            }
            if has_amd && allowed && self.saved.amd_off {
                ui.label(RichText::new("AMD Software reads this when it starts: restart the PC, then check that Record & Stream > Instant Replay and Hotkeys > Use Hotkeys are off. If they aren't, switch them off there and tell us.").small().color(pal().muted));
            }
            if !off {
                // Only above the steps below, when there are any.
                if has_nvidia || amd_replay || !clashes.is_empty() {
                    ui.label(RichText::new("Or switch it off yourself:").small().color(pal().muted));
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    /// Every key row but Save clip's has its Clear (key set) or Set (no key) in the same slot at
    /// the right, so they line up; Save clip always keeps a key, so its slot stays empty. Clear
    /// empties the key, and the row then offers Set.
    #[test]
    fn key_rows_have_clear_or_set_in_one_slot() {
        let mut h = crate::ui::widgets::testing::page(|app| app.cfg.hotkey_mic.clear(), |ui, app| app.keybinds_tab(ui));
        h.run();
        let slot = |h: &egui_kittest::Harness<'_, App>, target: Target| {
            let y = h.get_by_label(target.name()).rect().center().y;
            let near = |text| h.query_all_by_label(text).find(|b| (b.rect().center().y - y).abs() < 4.0).map(|b| (text, b.rect().right()));
            near("Clear").or_else(|| near("Set"))
        };
        assert!(slot(&h, Target::Save).is_none(), "Save clip keeps its key");
        let slots: Vec<_> = [Target::Overlay, Target::Record, Target::Screenshot, Target::Replay, Target::Mic].into_iter().map(|t| slot(&h, t).unwrap_or_else(|| panic!("{} has no Clear or Set", t.name()))).collect();
        assert!(slots.iter().all(|(_, right)| (right - slots[0].1).abs() < 0.5), "one slot: {slots:?}");
        assert_eq!(slots[4].0, "Set", "the mic has no key");
        assert!(slots[..4].iter().all(|(text, _)| *text == "Clear"), "{slots:?}");

        let y = h.get_by_label(Target::Record.name()).rect().center().y;
        h.query_all_by_label("Clear").find(|b| (b.rect().center().y - y).abs() < 4.0).unwrap().click();
        h.run();
        assert!(h.state().cfg.hotkey_record.is_empty());
        assert_eq!(slot(&h, Target::Record).map(|s| s.0), Some("Set"));
    }

    /// One of NVIDIA's shortcuts is named NVIDIA's even when this app registered the key first
    /// (NVIDIA's helper still reacts to it) while NVIDIA's overlay is on, then AMD's, then whoever
    /// else holds it. With the overlay off, an unheld NVIDIA key isn't NVIDIA's. A key nobody else
    /// uses has no clash.
    #[test]
    fn who_else_reacts_to_a_key() {
        for held in [false, true] {
            assert_eq!(holder(held, Some("Screenshot"), true, false), Some(Holder::Nvidia("Screenshot")), "held {held}");
            assert_eq!(holder(held, Some("Screenshot"), true, true), Some(Holder::Nvidia("Screenshot")), "held {held}");
            for nvidia_on in [false, true] {
                assert_eq!(holder(held, None, nvidia_on, true), Some(Holder::Amd), "held {held} on {nvidia_on}");
            }
        }
        // NVIDIA's overlay switched off (by this app or in the NVIDIA app): its key, Alt+F10 by
        // default, only counts when something actually holds it.
        assert_eq!(holder(true, Some("Save Instant Replay"), false, false), Some(Holder::Nvidia("Save Instant Replay")));
        assert_eq!(holder(false, Some("Save Instant Replay"), false, false), None);
        assert_eq!(holder(false, Some("Save Instant Replay"), false, true), Some(Holder::Amd));
        assert_eq!(holder(true, None, false, false), Some(Holder::Unknown));
        assert_eq!(holder(false, None, false, false), None);
        assert!(clash_text("Alt+F1", &Holder::Nvidia("Screenshot")).starts_with("NVIDIA's \"Screenshot\" shortcut also uses Alt+F1, so pressing it does both."));
    }

    /// The keys registered are the saved ones: a key changed on the page isn't, until Save, and
    /// Discard leaves them as they were.
    #[test]
    fn keys_register_as_saved() {
        let mut h = crate::ui::widgets::testing::page(|_| {}, |ui, app| app.keybinds_tab(ui));
        h.run();
        let saved = h.state().saved_keys();
        let record = Target::ALL.iter().position(|t| *t == Target::Record).unwrap();
        assert!(!saved[record].is_empty());
        let y = h.get_by_label(Target::Record.name()).rect().center().y;
        h.query_all_by_label("Clear").find(|b| (b.rect().center().y - y).abs() < 4.0).unwrap().click();
        h.run();
        assert!(h.state().cfg.hotkey_record.is_empty());
        assert_eq!(h.state().saved_keys(), saved, "not saved yet");
        h.state_mut().discard_settings();
        assert_eq!(h.state().saved_keys(), saved);
        let app = h.state_mut();
        app.cfg.hotkey_record.clear();
        app.saved = app.cfg.clone();
        assert!(app.saved_keys()[record].is_empty(), "after Save");
    }
}
