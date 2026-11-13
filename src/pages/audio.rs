//! The Audio page: devices, apps, tracks and the sound presets.

use crate::*;

/// An app's name as people know it, from the window title it had when picked: "VALORANT", or
/// the part after the last " - " ("#general | Friends - Discord" is "Discord"). The exe's name
/// when the title doesn't give one. Only the title is read: the app itself is never opened.
pub(crate) fn app_label(a: &config::AppSel) -> String {
    let title = a.title.trim();
    let name = title.rsplit(" - ").next().unwrap_or(title).trim();
    if !name.is_empty() && name.chars().count() <= 40 {
        return name.to_string();
    }
    a.exe.strip_suffix(".exe").or(a.exe.strip_suffix(".EXE")).unwrap_or(&a.exe).to_string()
}

/// The program and the window title behind an app's name, for its hover text.
pub(crate) fn app_detail(a: &config::AppSel) -> String {
    format!("{} ({})", a.exe, a.title.trim())
}

/// The Audio page's answers to "How should sound be saved?", with a line on each. Custom comes
/// after them.
pub(crate) const SOUND_PRESETS: [(&str, &str); 3] = [
    ("Everything together", "One track. Plays the same everywhere."),
    ("Game and mic separate", "3 tracks: everything, game and PC, your mic."),
    ("Game, mic and chat separate", "4 tracks: adds Discord voice chat on its own."),
];

/// A sound choice's tracks: how many, then which ones playback devices, mics and voice chat
/// go on. Track 1 always has everything (voice chat is already in the playback sound there).
pub(crate) fn preset_layout(p: usize) -> (u8, [bool; 6], [bool; 6], [bool; 6]) {
    const T: bool = true;
    const F: bool = false;
    match p {
        0 => (1, [T, F, F, F, F, F], [T, F, F, F, F, F], [F; 6]),
        1 => (3, [T, T, F, F, F, F], [T, F, T, F, F, F], [F; 6]),
        _ => (4, [T, T, F, F, F, F], [T, F, T, F, F, F], [F, F, F, T, F, F]),
    }
}

/// Which sound choice the settings match, or None for a custom setup.
pub(crate) fn sound_preset(cfg: &Config) -> Option<usize> {
    let n = cfg.tracks();
    let mut chat = [false; 6];
    chat[..n].copy_from_slice(&cfg.chat_tracks[..n]);
    (0..SOUND_PRESETS.len()).find(|&p| {
        let (count, out, mic, want_chat) = preset_layout(p);
        count as usize == n && chat == want_chat && used_devices(cfg, "out:").all(|(_, t)| t == out) && used_devices(cfg, "in:").all(|(_, t)| t == mic)
    })
}

/// Sets the tracks for a sound choice. The devices already recorded stay recorded; with none, the
/// default playback device is, and the default mic too for the choices that give it a track.
pub(crate) fn set_sound_preset(cfg: &mut Config, p: usize) {
    let (count, out, mic, chat) = preset_layout(p);
    let n = cfg.tracks();
    let used = move |d: &config::DeviceSel| d.tracks[..n].iter().any(|&t| t);
    for (prefix, key, name) in [("out:", "out:default", "Default playback device"), ("in:", "in:default", "Default microphone")] {
        if (prefix == "out:" || p > 0) && !cfg.devices.iter().any(|(k, d)| k.starts_with(prefix) && used(d)) {
            cfg.devices.entry(key.into()).or_insert_with(|| config::DeviceSel { name: name.into(), ..Default::default() }).tracks[0] = true;
        }
    }
    for (k, d) in cfg.devices.iter_mut() {
        d.tracks = if !used(d) {
            [false; 6]
        } else if k.starts_with("in:") {
            mic
        } else {
            out
        };
    }
    cfg.chat_tracks = chat;
    cfg.track_count = count;
    cfg.track_names = Default::default();
    for (i, name) in ["Everything", "Game and PC", "Mic", "Voice chat"].into_iter().take(count as usize).enumerate() {
        cfg.track_names[i] = name.into();
    }
}

/// A Windows sound device's name and its hardware: "Chat (TC-Helicon GoXLR Mini)" is
/// ("Chat", "TC-Helicon GoXLR Mini"); a name without hardware gives "".
pub(crate) fn device_parts(name: &str) -> (&str, &str) {
    match name.split_once(" (") {
        Some((short, rest)) => (short.trim(), rest.strip_suffix(')').unwrap_or(rest).trim()),
        None => (name.trim(), ""),
    }
}

/// Records only this mic (or none), on `tracks`.
pub(crate) fn set_mic(cfg: &mut Config, key: Option<&str>, tracks: [bool; 6]) {
    for (_, d) in cfg.devices.iter_mut().filter(|(k, _)| k.starts_with("in:")) {
        d.tracks = [false; 6];
    }
    if let Some(k) = key {
        cfg.devices.entry(k.into()).or_default().tracks = tracks;
    }
}

/// Devices recorded on any of the clip's tracks.
pub(crate) fn used_devices<'a>(cfg: &'a Config, prefix: &'a str) -> impl Iterator<Item = (&'a String, [bool; 6])> + 'a {
    let n = cfg.tracks();
    cfg.devices.iter().filter(move |(k, _)| k.starts_with(prefix)).filter_map(move |(k, d)| {
        let mut t = [false; 6];
        t[..n].copy_from_slice(&d.tracks[..n]);
        t.iter().any(|&x| x).then_some((k, t))
    })
}

/// What a track is called: the user's name for it, else after the devices recorded on it.
pub(crate) fn track_name(cfg: &Config, i: usize) -> String {
    match cfg.track_names.get(i).map(|n| n.trim()) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => device_track_name(cfg, i),
    }
}

/// A track named after the Audio tab's devices on it ("Chat + Game"), "Everything" for a track
/// with every recorded device on it (when there's more than one), or "Track N" if it has none.
pub(crate) fn device_track_name(cfg: &Config, i: usize) -> String {
    let used: Vec<&config::DeviceSel> = cfg.devices.values().filter(|d| d.tracks.iter().take(cfg.track_count as usize).any(|&t| t)).collect();
    let on: Vec<&config::DeviceSel> = used.iter().copied().filter(|d| d.tracks.get(i) == Some(&true)).collect();
    if on.is_empty() {
        return format!("Track {}", i + 1);
    }
    if on.len() > 1 && on.len() == used.len() {
        return "Everything".into();
    }
    // "Chat (TC-Helicon GoXLR Mini)" reads as "Chat".
    let short = |d: &&config::DeviceSel| d.name.split(" (").next().unwrap_or(&d.name).trim().to_string();
    on.iter().map(short).collect::<Vec<_>>().join(" + ")
}

/// The Custom track count's hover text: what track 1 is for, and what each track costs.
const TRACKS_ABOUT: &str = "How many separate sound tracks each clip has. Track 1 is what most players and Discord play, so put everything you want heard on it. If one device already carries another's sound (like a mixer's stream mix), leave the other off that track. Each extra track adds about 7 MB of memory per 5 minutes.";

impl App {
    pub(crate) fn refresh_devices(&mut self) {
        self.devices = devices::list();
        self.open_apps = devices::open_apps();
        for d in &self.devices {
            self.cfg.devices.entry(d.key.clone()).or_default().name = d.name.clone();
        }
    }

    /// Game and mic separate: everything, then game and PC sound, then the mic. Takes effect on Save.
    pub(crate) fn split_tracks(&mut self) {
        self.audio_custom = false;
        set_sound_preset(&mut self.cfg, 1);
    }

    /// The Audio page: how sound is saved (one track, or game, mic and chat apart) and the mic,
    /// then under More options the playback devices, per-app sound and the noise filter.
    pub(crate) fn audio_tab(&mut self, ui: &mut egui::Ui) {
        let custom_at = SOUND_PRESETS.len();
        let current = sound_preset(&self.cfg).filter(|_| !self.audio_custom);
        let mut pick = current.unwrap_or(custom_at);
        card(ui, "How should sound be saved?", |ui| {
            egui::Grid::new("sound_presets").num_columns(1).spacing([12.0, 6.0]).show(ui, |ui| {
                for (i, (name, hint)) in SOUND_PRESETS.into_iter().enumerate() {
                    chip(ui, &mut pick, i, name).on_hover_text(hint);
                    ui.end_row();
                }
                chip(ui, &mut pick, custom_at, "Custom").on_hover_text("Pick 1 to 6 tracks, and what goes on each");
                ui.end_row();
            });
            if pick != current.unwrap_or(custom_at) {
                self.audio_custom = pick == custom_at;
                if !self.audio_custom {
                    set_sound_preset(&mut self.cfg, pick);
                }
            }
            if pick == custom_at {
                ui.add_space(6.0);
                self.custom_tracks(ui);
            }
            ui.add_space(8.0);
            let mics: Vec<String> = used_devices(&self.cfg, "in:").map(|(k, _)| k.clone()).collect();
            let name_of = |k: &String| self.devices.iter().find(|d| &d.key == k).map_or_else(|| self.cfg.devices[k].name.clone(), |d| d.name.clone());
            let text = match mics.len() {
                0 => "No mic".to_string(),
                1 => name_of(&mics[0]),
                n => format!("{n} mics"),
            };
            let mut chosen: Option<Option<String>> = None;
            row(ui, "Microphone", |ui| {
                egui::ComboBox::from_id_salt("mic").selected_text(text).width(ui.available_width().min(360.0)).show_ui(ui, |ui| {
                    for d in self.devices.iter().filter(|d| d.key.starts_with("in:")) {
                        if ui.selectable_label(mics.len() == 1 && mics[0] == d.key, &d.name).clicked() {
                            chosen = Some(Some(d.key.clone()));
                        }
                    }
                    if ui.selectable_label(mics.is_empty(), "No mic").clicked() {
                        chosen = Some(None);
                    }
                })
                .response
                .on_hover_text("The mic whose voice goes into clips");
            });
            if let Some(key) = chosen {
                let tracks = current.map_or([true, false, false, false, false, false], |p| preset_layout(p).2);
                set_mic(&mut self.cfg, key.as_deref(), tracks);
            }
            let mut denoise = self.cfg.noise != "off";
            if ui.add_enabled(!mics.is_empty(), egui::Checkbox::new(&mut denoise, "Reduce background noise on my mic")).on_hover_text("Filters out fans, keyboards and hum from your mic").changed() {
                self.cfg.noise = if denoise { "rnnoise" } else { "off" }.into();
            }
            ui.checkbox(&mut self.cfg.center_sound, "Even out sound that's louder on one side (experimental)")
                .on_hover_text("After a clip is saved, a track that's louder on one side all the way through is raised on the quiet side, so it plays centered for whoever you share it with. Your mic stays in the middle. Off: clips are kept exactly as recorded.");
            let n = self.cfg.tracks();
            if self.cfg.chat_tracks[..n].iter().any(|&t| t) && !self.open_apps.iter().any(|a| a.exe.eq_ignore_ascii_case(&devices::chat_app().exe)) {
                ui.label(RichText::new("Discord isn't open right now. Its voice chat is saved whenever it is.").small().color(pal().amber));
            }
            if self.cfg.only_apps {
                ui.label(RichText::new("Only the apps picked under More options are recorded.").small().color(pal().amber));
            }
        });
        self.more_options(ui, Tab::Audio, Self::audio_more);
    }

    /// Custom sound: how many tracks, a name for each, and what goes on it, track 1 included, so
    /// a device whose sound is already in another (like a mixer's stream mix) can stay off it.
    pub(crate) fn custom_tracks(&mut self, ui: &mut egui::Ui) {
        row(ui, "Tracks", |ui| {
            ui.horizontal_wrapped(|ui| {
                for n in 1..=6u8 {
                    chip(ui, &mut self.cfg.track_count, n, n.to_string()).on_hover_text(TRACKS_ABOUT);
                }
            });
        });
        let n = self.cfg.tracks();
        // Every device recorded on any track; one taken off all of them is turned on again
        // under More options.
        let recorded: Vec<(String, String)> = ["out:", "in:"]
            .into_iter()
            .flat_map(|p| used_devices(&self.cfg, p).map(|(k, _)| k.clone()).collect::<Vec<_>>())
            .map(|k| {
                let name = &self.cfg.devices[&k].name;
                let short = name.split(" (").next().unwrap_or(name).trim().to_string();
                (k, short)
            })
            .collect();
        // Like OBS's Advanced Audio Properties: a row per sound, a column per track.
        egui::Grid::new("custom_tracks").num_columns(n + 1).spacing([10.0, 6.0]).min_col_width(28.0).show(ui, |ui| {
            ui.label(RichText::new("Sound").strong());
            for i in 0..n {
                ui.label(RichText::new((i + 1).to_string()).strong());
            }
            ui.end_row();

            for (k, name) in &recorded {
                let glyph = if k.starts_with("in:") { icon::MIC } else { icon::AUDIO };
                ui.horizontal(|ui| {
                    ui.label(icon(glyph, 14.0).color(pal().muted));
                    ui.label(name.as_str());
                });
                for i in 0..n {
                    if let Some(d) = self.cfg.devices.get_mut(k) {
                        ui.add(egui::Checkbox::without_text(&mut d.tracks[i])).on_hover_text(format!("Put {name} on track {}", i + 1));
                    }
                }
                ui.end_row();
            }
        });
        // Discord's own capture is only for "Game, mic and chat separate". Custom has no row for
        // it, and Discord is already in the playback sound, so it's never recorded here.
        self.cfg.chat_tracks = [false; 6];
        if n > 1 {
            ui.add_space(4.0);
            row(ui, "Track names", |ui| {
                ui.horizontal_wrapped(|ui| {
                    for i in 0..n {
                        let auto = device_track_name(&self.cfg, i);
                        ui.label((i + 1).to_string());
                        ui.add(egui::TextEdit::singleline(&mut self.cfg.track_names[i]).desired_width(100.0).char_limit(24).hint_text(auto)).on_hover_text("A name for this track, shown when trimming");
                    }
                });
            });
        }
    }

    /// A chip for each device whose key starts with `prefix`, in a row per piece of hardware
    /// ("Chat (TC-Helicon GoXLR Mini)" is Chat on the GoXLR Mini). Turning one on puts it on
    /// `on_tracks`; off takes it off every track.
    fn device_chips(&mut self, ui: &mut egui::Ui, prefix: &str, hover: &str, on_tracks: [bool; 6]) {
        let mut groups: Vec<(&str, Vec<&devices::Device>)> = Vec::new();
        for d in self.devices.iter().filter(|d| d.key.starts_with(prefix)) {
            let hw = device_parts(&d.name).1;
            match groups.iter_mut().find(|(h, _)| *h == hw) {
                Some((_, list)) => list.push(d),
                None => groups.push((hw, vec![d])),
            }
        }
        let tracks = self.cfg.tracks();
        egui::Grid::new(("device_chips", prefix)).num_columns(2).spacing([14.0, 10.0]).min_col_width(210.0).show(ui, |ui| {
            for (hw, list) in &groups {
                ui.add(egui::Label::new(RichText::new(if hw.is_empty() { "Windows" } else { hw }).color(pal().muted)).truncate()).on_hover_text(*hw);
                ui.horizontal_wrapped(|ui| {
                    for d in list {
                        let sel = self.cfg.devices.entry(d.key.clone()).or_default();
                        let mut on = sel.tracks.iter().take(tracks).any(|&t| t);
                        if toggle_chip(ui, &mut on, device_parts(&d.name).0).on_hover_text(format!("{}: {hover}", d.name)).clicked() {
                            sel.tracks = if on { on_tracks } else { [false; 6] };
                        }
                    }
                });
                ui.end_row();
            }
        });
    }

    /// The Audio page's More options: playback devices, mics, per-app sound, and the noise filter.
    pub(crate) fn audio_more(&mut self, ui: &mut egui::Ui) {
        let current = sound_preset(&self.cfg).filter(|_| !self.audio_custom);
        const TRACK_1: [bool; 6] = [true, false, false, false, false, false];
        card(ui, "Game and PC sound", |ui| {
            self.device_chips(ui, "out:", "record what this device plays", current.map_or(TRACK_1, |p| preset_layout(p).1));
        });
        card(ui, "Microphones", |ui| {
            self.device_chips(ui, "in:", "record this mic. With Custom sound, give each mic its own track.", current.map_or(TRACK_1, |p| preset_layout(p).2));
            ui.add_space(4.0);
            if button(ui, "Refresh devices").on_hover_text("Plugged in a headset or mic? Look for it again.").clicked() {
                self.refresh_devices();
            }
        });
        card(ui, "App sound", |ui| {
            ui.horizontal_wrapped(|ui| {
                chip(ui, &mut self.cfg.only_apps, false, "Everything (desktop audio)").on_hover_text("Records all the sound your PC plays");
                chip(ui, &mut self.cfg.only_apps, true, "Only these apps")
                    .on_hover_text("Records only the apps you pick, so music or calls stay out. Their sound goes on the same tracks as game and PC sound. Apps are matched by program, so a changing window title is fine. Your mic is set above.");
            });
            if !self.cfg.only_apps {
                return;
            }
            let mut remove = None;
            for (i, a) in self.cfg.apps.iter().enumerate() {
                ui.horizontal(|ui| {
                    if bare_icon(ui, true, icon::CANCEL, "Stop recording this app's sound") {
                        remove = Some(i);
                    }
                    ui.add(egui::Label::new(app_label(a)).truncate()).on_hover_text(app_detail(a));
                });
            }
            if let Some(i) = remove {
                self.cfg.apps.remove(i);
            }
            ui.horizontal(|ui| {
                let mut add = None;
                egui::ComboBox::from_id_salt("add_app").selected_text("Add an open app").width(260.0).show_ui(ui, |ui| {
                    for a in self.open_apps.iter().filter(|a| !self.cfg.apps.iter().any(|p| p.exe.eq_ignore_ascii_case(&a.exe))) {
                        if ui.add(egui::Button::selectable(false, app_label(a)).truncate()).on_hover_text(app_detail(a)).clicked() {
                            add = Some(a.clone());
                        }
                    }
                })
                .response
                .on_hover_text("Pick an open game or app to record its sound");
                self.cfg.apps.extend(add);
                if button(ui, "Refresh").on_hover_text("Just opened a game or app? Look for it again.").clicked() {
                    self.open_apps = devices::open_apps();
                }
            });
            if self.cfg.apps.is_empty() {
                ui.label(RichText::new("Pick at least one app, or clips have no game or PC sound.").small().color(pal().amber));
            }
        });
        if self.cfg.noise != "off" {
            card(ui, "Mic noise filter", |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (v, name, about) in [("rnnoise", "RNNoise", "Cleans up your mic's background noise better, but uses more CPU"), ("speex", "Speex", "Cleans up your mic's background noise with less CPU than RNNoise")] {
                        chip(ui, &mut self.cfg.noise, v.to_string(), name).on_hover_text(about);
                    }
                });
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A second mic turned on the way the Microphones card does it keeps the sound choice.
    #[test]
    fn two_mics_keep_the_sound_choice() {
        let mut cfg = Config::default();
        set_sound_preset(&mut cfg, 1);
        for key in ["in:headset", "in:desk"] {
            cfg.devices.insert(key.into(), config::DeviceSel { name: key.into(), tracks: preset_layout(1).2 });
        }
        assert_eq!(sound_preset(&cfg), Some(1));
        // The choice already turned on the default mic; both new ones are recorded with it.
        let mics: Vec<&str> = used_devices(&cfg, "in:").map(|(k, _)| k.as_str()).collect();
        assert!(mics.contains(&"in:headset") && mics.contains(&"in:desk"), "{mics:?}");
    }

    #[test]
    fn device_names_split_into_name_and_hardware() {
        assert_eq!(device_parts("Chat (TC-Helicon GoXLR Mini)"), ("Chat", "TC-Helicon GoXLR Mini"));
        assert_eq!(device_parts("Speakers (Steam Streaming Speakers)"), ("Speakers", "Steam Streaming Speakers"));
        assert_eq!(device_parts("Default playback device"), ("Default playback device", ""));
    }
}
