//! An open clip: the player, trimming, GIFs and the parts of a clip.

use crate::*;

/// A clip being trimmed: the range picked, and the running cut once started.
pub(crate) struct TrimEdit {
    pub(crate) path: std::path::PathBuf,
    pub(crate) len: f64,
    pub(crate) start: f64,
    pub(crate) end: f64,
    /// The part of the clip the timeline shows: (from, length) in seconds. Ctrl+scroll zooms.
    pub(crate) view: (f64, f64),
    /// Volume per audio track (0 to 2). Starts as track 1 only, which is what the clip plays as.
    pub(crate) gains: Vec<f32>,
    /// "Record game and mic apart" was pressed.
    pub(crate) want_split: bool,
    /// The clip's loudness for the waveform (media::peaks), once worked out in the background.
    pub(crate) wave: Arc<std::sync::Mutex<Option<Vec<f32>>>>,
    pub(crate) job: Option<Arc<ExportJob>>,
    /// Parts kept with Add part (start, end), in time order, shaded on the timeline. With any,
    /// Save joins them into one clip.
    pub(crate) parts: Vec<(f64, f64)>,
    /// A GIF of the selection being made, then how that went.
    pub(crate) gif: Option<Arc<ExportJob>>,
    pub(crate) gif_msg: Option<String>,
    /// Loud moments in the wave (media::loud_moments), worked out once it's there.
    pub(crate) loud: Option<Vec<f64>>,
}

/// Trim controls under the video: the timeline (drag the start or end edge you're over, or drag
/// anywhere else to scrub), a preview of just the selection, and the cut. Returns false when cancelled.
pub(crate) fn trim_controls(ui: &mut egui::Ui, p: &mut player::Player, t: &mut TrimEdit, split_on: bool, cfg: &Config) -> bool {
    let len = t.len.max(0.1);
    // Playback stays inside the selection: it stops on the end edge (start_playing begins it on
    // the start edge). Checked every frame while playing, so it overshoots by a frame at most.
    let playing = p.is_playing() && !p.ended();
    if playing {
        if p.position() >= t.end {
            p.pause();
            p.seek(t.end);
        } else {
            ui.ctx().request_repaint();
        }
    }
    // Borrowed, not copied: the worker writes it once and is done.
    let wave = t.wave.lock().unwrap();
    if wave.is_none() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    }
    if let (None, Some(w)) = (&t.loud, wave.as_deref()) {
        t.loud = Some(media::loud_moments(w));
    }
    let loud = t.loud.as_deref().unwrap_or(&[]);
    let mut scrub = range_bar(ui, len, &mut t.view, &mut t.start, &mut t.end, p.position(), playing, wave.as_deref().unwrap_or(&[]), loud, &t.parts);
    drop(wave);
    // M jumps to the next loud moment, around to the first after the last.
    if !ui.ctx().egui_wants_keyboard_input() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::M)) {
        let at = p.position() + 0.1;
        let later = loud.iter().copied().filter(|&m| m > at).min_by(f64::total_cmp);
        let jump = later.or_else(|| loud.iter().copied().min_by(f64::total_cmp));
        if let Some(m) = jump.filter(|m| !(t.view.0..t.view.0 + t.view.1).contains(m)) {
            t.view.0 = (m - t.view.1 / 2.0).clamp(0.0, len - t.view.1);
        }
        scrub = jump.or(scrub);
    }
    if let Some(at) = scrub {
        p.pause();
        p.seek(at);
    }
    let busy = t.job.is_some() || t.gif.is_some();
    let mut keep = true;
    ui.horizontal_wrapped(|ui| {
        if button(ui, "Play selection").on_hover_text("Plays just the part you'll keep").clicked() {
            p.seek(t.start);
            p.play();
        }
        note(ui, "Drag the purple edges to set the start and end. Ctrl+scroll on the timeline zooms; then scroll, or drag the strip under it, to move along.");
        if t.loud.as_ref().is_some_and(|l| !l.is_empty()) {
            note(ui, "Yellow marks are loud moments: click one, or press M for the next.");
        }
    });
    parts_row(ui, t, busy);
    // A volume per audio track, heard live. Only clips recorded with separate tracks have more than one.
    let untouched = t.gains == media::track1(t.gains.len());
    if !t.gains.is_empty() {
        let one = t.gains.len() == 1;
        let mut changed = false;
        for (i, g) in t.gains.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                let name = if one {
                    "Volume".to_string()
                } else {
                    track_name(cfg, i)
                };
                ui.add_sized([130.0, 18.0], egui::Label::new(name));
                let mut pct = *g * 100.0;
                if ui.add(egui::Slider::new(&mut pct, 0.0..=200.0).suffix("%").integer()).on_hover_text("How loud this sound is in the trimmed copy").changed() {
                    *g = pct / 100.0;
                    changed = true;
                }
            });
        }
        if changed {
            p.set_track_gains(&t.gains);
        }
        if !one {
            let first = track_name(cfg, 0);
            note(ui, format!("{first} (track 1) is the full mix, so to change one sound, turn {first} down and use the others. Rename tracks on the Audio page."));
        } else if split_on {
            note(ui, "This clip was recorded with the game and your mic on one track, so they go up and down together. Clips saved from now on have them apart, with a slider each.");
        } else {
            note(ui, "This clip has the game and your mic on one track, so they go up and down together. A clip can't be split afterwards, but new ones can be recorded with them apart.");
            if button(ui, "Record game and mic apart").on_hover_text("New clips get your mic on its own track. Takes effect after Save.").clicked() {
                t.want_split = true;
            }
        }
    }
    let how = if !t.parts.is_empty() {
        "Save joins the parts in order into one clip, each starting right where you set it (it's re-encoded)."
    } else if untouched {
        "Copied without re-encoding, so it may start up to 2 s early (at a keyframe)."
    } else {
        "The video is copied as is and may start up to 2 s early (at a keyframe); the sound is remixed at these levels into one track."
    };
    ui.label(
        RichText::new(format!(
            "{} to {}  ·  {} long. {how} The original is kept.",
            secs_text(t.start),
            secs_text(t.end),
            secs_text(t.end - t.start)
        ))
        .small()
        .color(pal().muted),
    );
    ui.horizontal(|ui| {
        let text = match &t.job {
            Some(j) if !t.parts.is_empty() => format!("Saving {}%", j.pct.load(std::sync::atomic::Ordering::Relaxed)),
            Some(_) => "Saving...".into(),
            None if t.parts.is_empty() => "Save trimmed copy".into(),
            None => format!("Save {} parts as one clip", t.parts.len()),
        };
        if primary(ui, !busy, RichText::new(text).strong(), egui::Vec2::ZERO).on_hover_text("Saves the kept part as a new clip. The original stays.").clicked() {
            let (src, start, end) = (t.path.clone(), t.start, t.end);
            let gains = (!untouched).then(|| t.gains.clone());
            let parts: Vec<media::Part> = t.parts.iter().map(|&(start, end)| media::Part { path: t.path.clone(), start, end, gains: t.gains.clone() }).collect();
            let what = format!("Trimmed {} to", src.display());
            t.job = Some(ExportJob::spawn(clips::copy_name(&t.path, "trim"), what, ui.ctx(), move |job, out| match &gains {
                _ if !parts.is_empty() => media::join(&parts, out, false, None, &|f| job.progress(f), &|| false),
                Some(g) => media::trim_mixed(&src, out, start, end, g),
                None => media::trim(&src, out, start, end),
            }));
        }
        gif_button(ui, t, busy);
        if cap(ui, !busy, egui::Button::new("Cancel trim")).on_hover_text("Stops trimming without saving anything").clicked() {
            keep = false;
            // Back to how the clip normally plays.
            p.set_track_gains(&media::track1(t.gains.len()));
        }
    });
    if let Some(msg) = &t.gif_msg {
        ui.label(RichText::new(msg).color(accent_light()));
    }
    if busy {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    }
    keep
}

/// "Save as GIF" for the selection, only when it's short (media::GIF_MAX_SECS); made in the
/// background into t.gif.
pub(crate) fn gif_button(ui: &mut egui::Ui, t: &mut TrimEdit, busy: bool) {
    let short = t.end - t.start <= media::GIF_MAX_SECS;
    let gif_text = match &t.gif {
        Some(g) => format!("Making GIF {}%", g.pct.load(std::sync::atomic::Ordering::Relaxed)),
        None => "Save as GIF".into(),
    };
    let gif = cap(ui, !busy && short, egui::Button::new(gif_text))
        .on_hover_text("A looping GIF of the selection, 480 px wide, no sound")
        .on_disabled_hover_text(if short { "Wait for the save to finish." } else { "GIFs are up to 15 s. Make the selection shorter." });
    if !short {
        ui.label(RichText::new("GIF: 15 s max").small().color(pal().muted));
    }
    if gif.clicked() {
        let (src, start, end) = (t.path.clone(), t.start, t.end);
        let what = format!("Made a GIF of {} at", src.display());
        t.gif = Some(ExportJob::spawn(clips::copy_name(&t.path.with_extension("gif"), "GIF"), what, ui.ctx(), move |job, out| media::save_gif(&src, out, start, end, &|f| job.progress(f))));
        t.gif_msg = None;
    }
}

/// Parts shaded on the timeline (range_bar).
pub(crate) const PART_FILL: Color32 = Color32::from_rgba_premultiplied(40, 80, 72, 110);

pub(crate) const PART_EDGE: Color32 = Color32::from_rgb(110, 200, 178);

/// "Add part" keeps the selection as a part, to save several bits of the clip as one; then a
/// chip per part with its times and a button to take it off.
pub(crate) fn parts_row(ui: &mut egui::Ui, t: &mut TrimEdit, busy: bool) {
    ui.horizontal_wrapped(|ui| {
        let add = cap(ui, !busy, icon_button(icon::ADD, "Add part")).on_hover_text("Keep the selection as a part, then pick another. The parts are saved in order as one clip.");
        if add.clicked() && !t.parts.contains(&(t.start, t.end)) {
            t.parts.push((t.start, t.end));
            t.parts.sort_by(|a, b| a.0.total_cmp(&b.0));
        }
        let mut remove = None;
        for (i, &(s, e)) in t.parts.iter().enumerate() {
            egui::Frame::new().fill(pal().raised).corner_radius(R_SMALL).inner_margin(egui::Margin::symmetric(8, 2)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Part {}: {} to {}", i + 1, secs_text(s), secs_text(e))).small().color(PART_EDGE));
                    if ui.add_enabled(!busy, egui::Button::new(icon(icon::CANCEL, 10.0).color(pal().muted)).frame(false)).on_hover_text("Take this part off").clicked() {
                        remove = Some(i);
                    }
                });
            });
        }
        if let Some(i) = remove {
            t.parts.remove(i);
        }
    });
}

/// A timeline with the kept part shaded between two edges, plus the playhead. Point at the start
/// or end edge and drag to move it (it lights up while you're on it); drag anywhere else to scrub.
/// Returns the time to show: the edge being moved, or where you're scrubbing.
///
/// `view` is the part of the clip shown, (from, length) in seconds: Ctrl+scroll over the bar zooms
/// in and out around the pointer; zoomed in, scrolling (or Shift+scroll) or dragging the in-view
/// strip along the bottom moves along, and dragging past either end of the bar moves the view
/// with you. With `follow` (playing), the view keeps the playhead in sight.
/// `wave` (media::peaks, may be empty) is drawn as the clip's sound along the bar, and `loud`
/// (seconds) as yellow marks along the top; scrubbing onto one lands exactly on it. `parts`
/// (start, end) are shaded green.
#[allow(clippy::too_many_arguments)]
pub(crate) fn range_bar(ui: &mut egui::Ui, len: f64, view: &mut (f64, f64), start: &mut f64, end: &mut f64, playhead: f64, follow: bool, wave: &[f32], loud: &[f64], parts: &[(f64, f64)]) -> Option<f64> {
    #[derive(Clone, Copy, PartialEq)]
    enum Grab {
        Scrub,
        Start,
        End,
        View,
    }
    const REACH: f32 = 10.0; // how close to an edge counts as on it
    const STRIP: f32 = 10.0; // the bottom of the bar that grabs the in-view strip
    let height = if wave.is_empty() { 36.0 } else { 64.0 };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click_and_drag());
    // Zoomed in down to one second across the bar.
    let fit = |v: &mut (f64, f64)| {
        v.1 = v.1.clamp(len.min(1.0), len);
        v.0 = v.0.clamp(0.0, len - v.1);
    };
    if let Some(hover) = resp.hover_pos() {
        // Zoomed in, the plain wheel moves along too, and the trim page doesn't scroll with it.
        let wheel = if view.1 < len { ui.input_mut(|i| std::mem::take(&mut i.smooth_scroll_delta.y)) } else { 0.0 };
        let (zoom, pan) = ui.input(|i| (i.zoom_delta() as f64, (i.smooth_scroll_delta.x + wheel) as f64));
        if zoom != 1.0 {
            // Keep the moment under the pointer where it is.
            let at = view.0 + ((hover.x - rect.left()) / rect.width()) as f64 * view.1;
            let span = (view.1 / zoom).clamp(len.min(1.0), len);
            view.0 = at - (at - view.0) * span / view.1;
            view.1 = span;
        }
        if pan != 0.0 {
            view.0 -= pan / rect.width() as f64 * view.1;
        }
    }
    fit(view);
    let v = *view;
    // Zoomed in, the strip along the bottom (it shows which part is in view) can be dragged.
    let on_strip = |p: egui::Pos2| v.1 < len && p.y >= rect.bottom() - STRIP;
    let x_of = |t: f64| rect.left() + ((t - v.0) / v.1) as f32 * rect.width();
    let t_of = |x: f32| (v.0 + ((x - rect.left()) / rect.width()) as f64 * v.1).clamp(0.0, len);
    // The edge under the pointer, the nearer one if both are close.
    let under = |x: f32, s: f64, e: f64| {
        let (ds, de) = ((x - x_of(s)).abs(), (x - x_of(e)).abs());
        if ds.min(de) > REACH { Grab::Scrub } else if de < ds { Grab::End } else { Grab::Start }
    };
    // Which edge is held is picked on the press, where it started, before anything moves; and
    // forgotten on release. (Picking it at drag start used the last drag's edge for the first
    // few pixels, which pulled the start edge over to the end.)
    let id = resp.id.with("grab");
    let mut grab = ui.data(|d| d.get_temp::<Grab>(id));
    if !resp.is_pointer_button_down_on() {
        grab = None;
        ui.data_mut(|d| d.remove::<Grab>(id));
    } else if grab.is_none() {
        grab = ui.input(|i| i.pointer.press_origin()).map(|p| if on_strip(p) { Grab::View } else { under(p.x, *start, *end) });
        ui.data_mut(|d| d.insert_temp(id, grab.unwrap_or(Grab::Scrub)));
    }
    let mut show = None;
    if let (Some(pos), Some(g)) = (resp.interact_pointer_pos(), grab) {
        let at = t_of(pos.x);
        show = match g {
            // The strip is the whole clip across the bar.
            Grab::View => {
                view.0 += (resp.drag_delta().x / rect.width()) as f64 * len;
                fit(view);
                None
            }
            Grab::Start => {
                *start = at.min(*end - 0.5).max(0.0);
                Some(*start)
            }
            Grab::End => {
                *end = at.max(*start + 0.5).min(len);
                Some(*end)
            }
            Grab::Scrub => Some(loud.iter().copied().find(|&m| (x_of(m) - pos.x).abs() <= 6.0).unwrap_or(at)),
        };
    }
    // Keep what's being dragged, or the playhead while playing, in view.
    if let Some(t) = show.or((follow && grab != Some(Grab::View)).then_some(playhead)) {
        if t < view.0 {
            view.0 = t;
        } else if t > view.0 + view.1 {
            view.0 = if show.is_some() { t - view.1 } else { t };
        }
        fit(view);
    }
    let v = *view;
    let x_of = |t: f64| rect.left() + ((t - v.0) / v.1) as f32 * rect.width();
    // What to light up: the edge being dragged, else the one being pointed at.
    let active = if resp.is_pointer_button_down_on() { grab } else { resp.hover_pos().map(|p| under(p.x, *start, *end)) };
    // Clipped to the bar: zoomed in, the selection and edges can be partly or wholly off it.
    let painter = ui.painter_at(rect);
    let track_h = if wave.is_empty() { 8.0 } else { height / 2.0 - 4.0 };
    let track = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.center().y - track_h), egui::pos2(rect.right(), rect.center().y + track_h));
    painter.rect_filled(track, 4.0, pal().raised);
    for &(s, e) in parts {
        let r = egui::Rect::from_min_max(egui::pos2(x_of(s), track.top()), egui::pos2(x_of(e), track.bottom()));
        painter.rect_filled(r, 2.0, PART_FILL);
        painter.rect_stroke(r, 2.0, egui::Stroke::new(1.0, PART_EDGE), egui::StrokeKind::Inside);
    }
    let sel =egui::Rect::from_min_max(egui::pos2(x_of(*start), track.top()), egui::pos2(x_of(*end), track.bottom()));
    painter.rect_filled(sel, 2.0, accent_dim());
    // The sound, a bar every 2 px mirrored around the middle, brighter inside the kept part.
    // Square root so quiet parts still show.
    let t_at = |x: f32| v.0 + ((x - rect.left()) / rect.width()) as f64 * v.1;
    let half = track_h - 2.0;
    let mut x = rect.left();
    while x < rect.right() && !wave.is_empty() {
        let (a, b) = (t_at(x), t_at(x + 2.0));
        let i0 = (a * media::PEAKS_PER_SEC as f64) as usize;
        let i1 = ((b * media::PEAKS_PER_SEC as f64).ceil() as usize).max(i0 + 1).min(wave.len());
        if let Some(steps) = wave.get(i0..i1) {
            let h = steps.iter().fold(0.0f32, |m, &s| m.max(s)).sqrt() * half;
            if h >= 0.5 {
                let kept = a >= *start && a <= *end;
                let color = if kept { Color32::from_rgba_unmultiplied(200, 170, 255, 210) } else { Color32::from_rgba_unmultiplied(150, 150, 165, 110) };
                painter.line_segment([egui::pos2(x + 1.0, rect.center().y - h), egui::pos2(x + 1.0, rect.center().y + h)], egui::Stroke::new(1.5, color));
            }
        }
        x += 2.0;
    }
    painter.rect_stroke(sel, 2.0, egui::Stroke::new(1.5, accent()), egui::StrokeKind::Inside);
    for x in loud.iter().map(|&m| x_of(m)) {
        let top = rect.top();
        painter.line_segment([egui::pos2(x, top + 6.0), egui::pos2(x, track.bottom())], egui::Stroke::new(1.0, pal().amber.gamma_multiply(0.5)));
        painter.add(egui::Shape::convex_polygon(vec![egui::pos2(x - 5.0, top), egui::pos2(x + 5.0, top), egui::pos2(x, top + 7.0)], pal().amber, egui::Stroke::NONE));
    }
    for (edge, x) in [(Grab::Start, x_of(*start)), (Grab::End, x_of(*end))] {
        let lit = active == Some(edge);
        let size = if lit { egui::vec2(9.0, height - 2.0) } else { egui::vec2(6.0, height - 6.0) };
        painter.rect_filled(egui::Rect::from_center_size(egui::pos2(x, rect.center().y), size), 3.0, if lit { pal().text } else { accent() });
    }
    let px = x_of(show.unwrap_or(playhead));
    painter.line_segment([egui::pos2(px, rect.top()), egui::pos2(px, rect.bottom())], egui::Stroke::new(2.0, pal().text));
    // Zoomed in: a thin strip along the bottom shows which part of the clip is in view, thicker
    // while pointed at or dragged.
    let strip_lit = active == Some(Grab::View) || (!resp.is_pointer_button_down_on() && resp.hover_pos().is_some_and(on_strip));
    if v.1 < len {
        let w = if strip_lit { 6.0 } else { 3.0 };
        let y = rect.bottom() - w / 2.0;
        let at = |t: f64| rect.left() + (t / len) as f32 * rect.width();
        painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], egui::Stroke::new(w, pal().raised));
        painter.line_segment([egui::pos2(at(v.0), y), egui::pos2(at(v.0 + v.1), y)], egui::Stroke::new(w, accent_light()));
    }
    let on_edge = matches!(active, Some(Grab::Start | Grab::End));
    let cursor = if strip_lit {
        egui::CursorIcon::Grab
    } else if on_edge {
        egui::CursorIcon::ResizeHorizontal
    } else {
        egui::CursorIcon::PointingHand
    };
    resp.on_hover_text("Drag an edge to set the start or end, or drag anywhere else to look through the clip. Zoomed in, scroll or drag the strip along the bottom to move along.").on_hover_cursor(cursor);
    show
}

/// Starts playback. While trimming (`range` = the selection), a playhead outside the selection
/// (or sitting on its end) goes back to the start edge first, so play never runs past the cut.
pub(crate) fn start_playing(p: &mut player::Player, range: Option<(f64, f64)>) {
    if let Some((start, end)) = range {
        let at = p.position();
        if at < start || at >= end - 0.05 {
            p.seek(start);
        }
    }
    p.play();
}

/// Seconds as m:ss.s, for trim points.
pub(crate) fn secs_text(secs: f64) -> String {
    let s = secs.max(0.0);
    format!("{}:{:04.1}", (s / 60.0) as u32, s % 60.0)
}

impl App {
    /// An open clip, filling the Clips page: back to the grid, the video as big as fits with
    /// room for the controls, play/pause and time, then the seek bar or the trim tools.
    pub(crate) fn player_page(&mut self, ui: &mut egui::Ui) {
        // A trim only belongs to the clip it was started on.
        if self.trim.as_ref().is_some_and(|t| self.player.as_ref().is_none_or(|(p, _)| *p != t.path)) {
            self.trim = None;
        }
        let Some((path, p)) = &mut self.player else { return };
        let path = path.clone();
        // Space plays and pauses, unless a text box has the keyboard. Taken before any button is
        // drawn, so a focused button doesn't also get pressed by it.
        let range = self.trim.as_ref().map(|t| (t.start, t.end));
        if !ui.ctx().egui_wants_keyboard_input() {
            use egui::Key;
            let [space, j, k, l, back, on] = ui.input_mut(|i| [Key::Space, Key::J, Key::K, Key::L, Key::Comma, Key::Period].map(|key| i.consume_key(egui::Modifiers::NONE, key)));
            let playing = p.is_playing() && !p.ended();
            if space {
                if playing { p.pause() } else { start_playing(p, range) }
            }
            // K stops (back to normal speed). L plays, then 2x, 4x; J plays at half speed, then
            // slower, or steps L's speed back down. Windows' player can't play these clips backwards.
            if k {
                p.pause();
                p.set_rate(1.0);
            }
            if l || j {
                let rate = match (playing, l) {
                    (false, true) => 1.0,
                    (false, false) => 0.5,
                    (true, true) => (p.rate() * 2.0).min(4.0),
                    (true, false) => (p.rate() / 2.0).max(0.25),
                };
                p.set_rate(rate);
                if !playing {
                    start_playing(p, range);
                }
            }
            // , and . step a frame back or on.
            if back || on {
                p.step(on);
            }
        }
        let (mut close, mut begin_trim) = (false, false);
        let trimming = self.trim.is_some();
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        ui.horizontal(|ui| {
            if cap(ui, true, icon_button(icon::BACK, "Clips")).on_hover_text("Back to your clips").clicked() {
                close = true;
            }
            ui.add_space(6.0);
            ui.add(egui::Label::new(strong(&name, 17.0).color(pal().text)).truncate()).on_hover_text(&name);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !trimming && cap(ui, true, icon_button(icon::CUT, "Trim")).on_hover_text("Cut the clip shorter, or keep a few parts").clicked() {
                    begin_trim = true;
                }
                if cap(ui, true, icon_button(icon::FOLDER, "Show in folder")).on_hover_text("Opens its folder with the file picked").clicked() {
                    clips::open(&path, true);
                }
            });
        });
        ui.add_space(12.0);
        // The video gets what the controls under it leave.
        let reserve = if trimming { 330.0 } else { 90.0 };
        let w = ui.available_width();
        let h = (w * 9.0 / 16.0).min(ui.available_height() - reserve).max(160.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
        ui.painter().rect_filled(rect, R_CARD, Color32::BLACK);
        let ppp = ui.ctx().pixels_per_point();
        p.set_rect((rect.left() * ppp) as i32, (rect.top() * ppp) as i32, (rect.width() * ppp) as i32, (rect.height() * ppp) as i32);
        let len = p.duration().unwrap_or(0.0);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let playing = p.is_playing() && !p.ended();
            let (glyph, text) = if playing { (icon::PAUSE, "Pause") } else { (icon::PLAY, "Play") };
            let keys = "Space plays and pauses. J slower, K stop, L faster. , and . step one frame.";
            if cap(ui, true, icon_button(glyph, text).min_size(egui::vec2(96.0, 32.0))).on_hover_text(keys).clicked() {
                if playing { p.pause() } else { start_playing(p, range) }
            }
            ui.label(RichText::new(format!("{} / {}", secs_text(p.position()), secs_text(len))).color(pal().muted));
            if p.rate() != 1.0 {
                ui.label(RichText::new(format!("{}x  ·  no sound", p.rate())).color(pal().amber)).on_hover_text("Sound plays only at normal speed. K goes back to it.");
            }
        });
        if !trimming {
            let mut at = p.position();
            ui.spacing_mut().slider_width = ui.available_width() - 8.0;
            if ui.add(egui::Slider::new(&mut at, 0.0..=len.max(0.1)).show_value(false)).on_hover_text("Drag to jump to any moment").changed() {
                p.seek(at);
            }
        }
        if p.is_playing() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
        } else if p.stepping() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
        }
        if trimming {
            ui.add_space(8.0);
            egui::ScrollArea::vertical().id_salt("trim").auto_shrink(false).show(ui, |ui| self.trim_panel(ui));
        }
        if begin_trim {
            self.begin_trim(&path);
        }
        if close {
            self.player = None;
            self.trim = None;
        }
    }

    /// Opens `path` paused in the player with the whole clip selected for trimming.
    pub(crate) fn begin_trim(&mut self, path: &std::path::Path) {
        if self.player.as_ref().is_none_or(|(p, _)| p != path) {
            self.play(path);
        }
        match &mut self.player {
            Some((p, player)) if p == path => {
                player.pause();
                let len = player.duration().or_else(|| media::duration(path).ok()).unwrap_or(0.0);
                self.clips_msg = None;
                let gains = media::track1(player.audio_tracks());
                player.set_track_gains(&gains);
                let wave: Arc<std::sync::Mutex<Option<Vec<f32>>>> = Arc::default();
                // The thread only holds a weak link, so closing the trim stops it.
                let (slot, src) = (Arc::downgrade(&wave), path.to_path_buf());
                std::thread::spawn(move || {
                    let peaks = media::peaks(&src, || slot.strong_count() > 0).unwrap_or_else(|e| {
                        log::write(&format!("No waveform for {}: {e:#}", src.display()));
                        Vec::new()
                    });
                    if let Some(s) = slot.upgrade() {
                        *s.lock().unwrap() = Some(peaks);
                    }
                });
                self.trim = Some(TrimEdit { path: path.to_path_buf(), len, start: 0.0, end: len, view: (0.0, len), gains, want_split: false, wave, job: None, parts: Vec::new(), gif: None, gif_msg: None, loud: None });
            }
            _ => self.clips_msg = Some("Can't open that clip here for trimming.".into()),
        }
    }

    /// The trim tools for the clip in the player, under the video.
    pub(crate) fn trim_panel(&mut self, ui: &mut egui::Ui) {
        let split_on = self.cfg.track_count > 1;
        let (Some(t), Some((_, p))) = (&mut self.trim, &mut self.player) else { return };
        let mut keep = true;
        let cfg = &self.cfg;
        card(ui, "Trim", |ui| keep = trim_controls(ui, p, t, split_on, cfg));
        if t.want_split {
            // Set it up and show it in the Audio tab, where Save puts it into effect.
            self.split_tracks();
            self.tab = Tab::Audio;
            keep = false;
        }
        if !keep {
            self.trim = None;
        }
    }

    /// Picks up a finished cut (closes the trim) or GIF, and lists the new file.
    pub(crate) fn finish_trim(&mut self) {
        let Some(t) = &mut self.trim else { return };
        // A GIF leaves the trim open, to make another or save the cut too.
        if let Some(done) = t.gif.as_ref().and_then(|g| g.done.lock().unwrap().take()) {
            t.gif = None;
            t.gif_msg = Some(match done {
                Ok(out) => {
                    self.clips_checked -= std::time::Duration::from_secs(60);
                    saved_note(&out)
                }
                Err(e) => format!("Couldn't make the GIF: {e}"),
            });
        }
        let Some(job) = &t.job else { return };
        let done = job.done.lock().unwrap().take();
        match done {
            Some(Ok(out)) => {
                self.clips_msg = Some(format!("Saved {}. The original is kept.", out.file_name().unwrap_or_default().to_string_lossy()));
                self.rescan_clips();
                self.trim = None;
            }
            Some(Err(e)) => {
                self.clips_msg = Some(format!("Trim didn't work: {e}"));
                t.job = None;
            }
            None => {}
        }
    }

    /// Opens `path` in the window's player, or in the default app if it can't be played here.
    pub(crate) fn play(&mut self, path: &std::path::Path) {
        self.player = None; // lets go of the old file first
        let opened = overlay::raw().ok_or_else(|| anyhow::anyhow!("no window")).and_then(|h| player::Player::open(h, path));
        match opened {
            Ok(mut p) => {
                p.play();
                self.player = Some((path.to_path_buf(), p));
            }
            Err(e) => {
                log::write(&format!("Couldn't play {} here ({e:#}); opened it in the default app.", path.display()));
                clips::open(path, false);
            }
        }
    }
}
