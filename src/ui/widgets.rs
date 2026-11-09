//! The small pieces every page is built from: cards, buttons, chips, icons and labels.

use crate::*;

/// A folder box with a Browse button.
pub(crate) fn folder_field(ui: &mut egui::Ui, value: &mut String, hint: &str) {
    ui.horizontal(|ui| {
        if button(ui, "Browse").on_hover_text("Pick a folder").clicked()
            && let Some(dir) = overlay::file_dialog().set_directory(&*value).pick_folder() {
            *value = dir.display().to_string();
        }
        // A long path is cut at the box's edge; the whole of it shows on hover.
        let about = if value.is_empty() { "The folder; type or paste a path, or use Browse".to_string() } else { format!("{value}\nType or paste a path, or use Browse") };
        ui.add(egui::TextEdit::singleline(value).hint_text(hint).desired_width(f32::INFINITY)).on_hover_text(about);
    });
}

/// "Show tips and explanations" (General), for `note`.
pub(crate) static NOTES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// The small grey explanation under a setting; left out when tips and explanations are off.
pub(crate) fn note(ui: &mut egui::Ui, text: impl Into<String>) {
    if NOTES.load(std::sync::atomic::Ordering::Relaxed) {
        ui.label(RichText::new(text.into()).small().color(pal().muted));
    }
}

/// Glyphs in Segoe MDL2 Assets.
pub(crate) mod icon {
    pub const CLIPS: char = '\u{E8B2}'; // MovieClapper
    pub const SETTINGS: char = '\u{E713}';
    pub const KEYBOARD: char = '\u{E765}';
    pub const VIDEO: char = '\u{E714}';
    pub const AUDIO: char = '\u{E767}';
    pub const MIC: char = '\u{E720}'; // Microphone
    pub const CHAT: char = '\u{E8F2}'; // ChatBubbles
    pub const HELP: char = '\u{E897}'; // Help
    pub const FEEDBACK: char = '\u{E8BD}'; // Message
    pub const NEW: char = '\u{E81C}'; // History
    pub const PLAY: char = '\u{E768}';
    pub const PAUSE: char = '\u{E769}';
    pub const BACK: char = '\u{E72B}';
    pub const FOLDER: char = '\u{E838}';
    pub const CUT: char = '\u{E8C6}';
    pub const SHARE: char = '\u{E72D}';
    pub const MORE: char = '\u{E712}';
    pub const PICTURE: char = '\u{E91B}';
    pub const COPY: char = '\u{E8C8}';
    pub const STAR: char = '\u{E734}'; // FavoriteStar
    pub const STAR_FILLED: char = '\u{E735}'; // FavoriteStarFill
    pub const ADD: char = '\u{E710}';
    pub const CANCEL: char = '\u{E711}';
    pub const UP: char = '\u{E74A}';
    pub const DOWN: char = '\u{E74B}';
    pub const MUSIC: char = '\u{E8D6}'; // MusicNote
    pub const TIP: char = '\u{E82F}'; // Lightbulb
    pub const LEFT: char = '\u{E76B}'; // ChevronLeft
    pub const RIGHT: char = '\u{E76C}'; // ChevronRight
    pub const CHEVRON_DOWN: char = '\u{E70D}'; // ChevronDown
    pub const CHECK: char = '\u{E73E}'; // CheckMark
    pub const WARNING: char = '\u{E7BA}';
    pub const DELETE: char = '\u{E74D}';
    pub const TAG: char = '\u{E8EC}';
}

/// An icon glyph at `size`, or nothing if the icon font isn't there.
pub(crate) fn icon(glyph: char, size: f32) -> RichText {
    if ICONS.load(std::sync::atomic::Ordering::Relaxed) {
        RichText::new(glyph.to_string()).family(egui::FontFamily::Name("icons".into())).size(size)
    } else {
        RichText::new("")
    }
}

/// Paints an icon glyph at `pos`; false (and nothing painted) if the icon font isn't there.
pub(crate) fn paint_icon(painter: &egui::Painter, pos: egui::Pos2, align: egui::Align2, glyph: char, size: f32, color: Color32) -> bool {
    let there = ICONS.load(std::sync::atomic::Ordering::Relaxed);
    if there {
        painter.text(pos, align, glyph, egui::FontId::new(size, egui::FontFamily::Name("icons".into())), color);
    }
    there
}

/// As many columns at least `min` wide as fit in `avail` with `gap` between them, and their width.
pub(crate) fn grid_cols(avail: f32, min: f32, gap: f32) -> (usize, f32) {
    let cols = ((avail + gap) / (min + gap)).floor().max(1.0) as usize;
    (cols, (avail - gap * (cols - 1) as f32) / cols as f32)
}

/// A small icon with no button frame, like the x on a tip; true when clicked.
pub(crate) fn bare_icon(ui: &mut egui::Ui, enabled: bool, glyph: char, hint: &str) -> bool {
    ui.add_enabled(enabled, egui::Button::new(icon(glyph, 12.0)).frame(false)).on_hover_text(hint).clicked()
}

/// A button with an icon before its text.
pub(crate) fn icon_button(glyph: char, text: &str) -> egui::Button<'static> {
    let mut job = egui::text::LayoutJob::default();
    if ICONS.load(std::sync::atomic::Ordering::Relaxed) {
        let fmt = egui::TextFormat { font_id: egui::FontId::new(13.0, egui::FontFamily::Name("icons".into())), valign: egui::Align::Center, color: pal().text, ..Default::default() };
        job.append(&glyph.to_string(), 0.0, fmt);
    }
    let fmt = egui::TextFormat { font_id: egui::FontId::proportional(14.0), valign: egui::Align::Center, color: pal().text, ..Default::default() };
    job.append(text, if job.is_empty() { 0.0 } else { 8.0 }, fmt);
    egui::Button::new(job)
}

/// A status line: a check or warning icon, then the text, both in `color`.
pub(crate) fn status(ui: &mut egui::Ui, ok: bool, text: RichText, color: Color32, size: f32) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(icon(if ok { icon::CHECK } else { icon::WARNING }, size).color(color));
        ui.label(text.color(color));
    });
}

/// Text in the semibold face, for titles and names.
pub(crate) fn strong(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).family(egui::FontFamily::Name("strong".into())).size(size)
}

/// A heading or a name: bold in Retro; regular in Minimalist, where size and color carry the
/// hierarchy and bold is kept for page titles.
pub(crate) fn heading(text: impl Into<String>, size: f32) -> RichText {
    if retro() { strong(text, size) } else { RichText::new(text).size(size) }
}

/// Text in the VCR on-screen display font in Retro; small bold text in Minimalist.
pub(crate) fn osd(text: impl Into<String>, size: f32) -> RichText {
    if retro() { RichText::new(text).family(egui::FontFamily::Name("osd".into())).size(size) } else { strong(text, (size * 0.72).round()) }
}

/// A sidebar section heading: the VCR font in capitals in Retro, quiet small text in Minimalist.
pub(crate) fn side_heading(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.add_space(if retro() { 4.0 } else { 12.0 });
        let label = if retro() { osd(text.to_uppercase(), 17.0) } else { RichText::new(text).size(12.0) };
        ui.label(label.color(pal().muted));
    });
}

/// A big title. Retro: the VCR font with the site's faint orange and teal fringes, like a TV
/// picture. Minimalist: plain bold text.
pub(crate) fn retro_title(ui: &mut egui::Ui, text: &str, size: f32) {
    if !retro() {
        ui.label(strong(text, (size * 0.72).round()).color(pal().text));
        return;
    }
    let galley = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::new(size, egui::FontFamily::Name("osd".into())), pal().text);
    let (rect, _) = ui.allocate_exact_size(galley.size(), egui::Sense::hover());
    let p = ui.painter();
    if effects() {
        p.galley_with_override_text_color(rect.min + egui::vec2(2.0, 0.0), galley.clone(), alpha(TRACKING, 110));
        p.galley_with_override_text_color(rect.min - egui::vec2(2.0, 0.0), galley.clone(), alpha(PHOSPHOR, 90));
    }
    p.galley(rect.min, galley, pal().text);
}

/// The site's logo: a purple rounded square with a play triangle, then the name.
pub(crate) fn brand(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::hover());
        if effects() {
            glow(ui.painter(), rect.center(), 26.0, alpha(accent(), 50));
        }
        ui.painter().rect_filled(rect, 7.0, accent());
        let c = rect.center() + egui::vec2(1.5, 0.0);
        let tri = vec![c + egui::vec2(-4.0, -6.0), c + egui::vec2(6.0, 0.0), c + egui::vec2(-4.0, 6.0)];
        ui.painter().add(egui::Shape::convex_polygon(tri, on_accent(), egui::Stroke::NONE));
        ui.add_space(2.0);
        ui.vertical(|ui| {
            ui.label(strong("Replay Vault", 17.0).color(pal().text));
            if crate::DEV_BUILD {
                ui.label(RichText::new("DEV BUILD").small().strong().color(pal().amber)).on_hover_text(format!("Built from the {} branch", option_env!("RV_DEV_BUILD").unwrap_or("")));
            }
        });
    });
}

/// Keys drawn as keycaps ("Alt+F10" is [Alt] + [F10]), starting at `at`'s x and centered on its y.
/// Minimalist: flat caps like its buttons, with a thin darker edge under them. Retro: chunky accent
/// caps on a deep lip, like its main buttons. Returns the width taken.
pub(crate) fn keycaps(painter: &egui::Painter, at: egui::Pos2, keys: &str) -> f32 {
    let font = egui::FontId::new(13.0, egui::FontFamily::Name("strong".into()));
    let (fill, edge, ink, radius, depth) = if retro() {
        (accent(), accent().lerp_to_gamma(Color32::BLACK, 0.45), on_accent(), R_SMALL, 4.0)
    } else {
        (pal().raised, pal().edge, pal().text, R_KEY, 2.0)
    };
    let mut x = at.x;
    for (i, key) in keys.split('+').enumerate() {
        if i > 0 {
            let plus = painter.layout_no_wrap("+".into(), font.clone(), pal().muted);
            painter.galley(egui::pos2(x + 5.0, at.y - plus.size().y / 2.0), plus.clone(), pal().muted);
            x += plus.size().x + 10.0;
        }
        let g = painter.layout_no_wrap(key.trim().to_string(), font.clone(), ink);
        let cap = egui::Rect::from_min_size(egui::pos2(x, at.y - (22.0 + depth) / 2.0), egui::vec2(g.size().x.max(10.0) + 14.0, 22.0));
        painter.rect_filled(cap.translate(egui::vec2(0.0, depth)), radius, edge);
        painter.rect_filled(cap, radius, fill);
        painter.galley(cap.center() - g.size() / 2.0, g, ink);
        x = cap.right();
    }
    x - at.x
}

/// Paints `tex` filling `rect`, cut to its shape from the middle rather than stretched.
pub(crate) fn paint_fill(ui: &egui::Ui, tex: &egui::TextureHandle, rect: egui::Rect, radius: impl Into<egui::CornerRadius>) {
    let [w, h] = tex.size().map(|v| v as f32);
    let k = rect.aspect_ratio() / (w / h);
    let uv = if k < 1.0 {
        egui::Rect::from_x_y_ranges((1.0 - k) / 2.0..=(1.0 + k) / 2.0, 0.0..=1.0)
    } else {
        egui::Rect::from_x_y_ranges(0.0..=1.0, (1.0 - 1.0 / k) / 2.0..=(1.0 + 1.0 / k) / 2.0)
    };
    egui::Image::new(tex).uv(uv).corner_radius(radius).paint_at(ui, rect);
}

/// The main action on a page, in the accent. Retro: a chunky button like the site's, on a deep
/// darker lip it sinks onto while pressed. Minimalist: a keycap with a thin darker edge. Every one
/// is the same size and type as Save clip: bold, H_BIG tall and at least 110 wide (`min` can ask
/// for more).
pub(crate) fn primary(ui: &mut egui::Ui, enabled: bool, text: RichText, min: egui::Vec2) -> egui::Response {
    // Painted under the button once its size is known.
    let under = ui.painter().add(egui::Shape::Noop);
    // No stroke: the theme's control outline and hover outline would ring the painted shape.
    // Disabled: a plain raised slab with grey text, still readable.
    let text = text.family(egui::FontFamily::Name("strong".into())).color(if enabled { on_accent() } else { pal().muted });
    let button = egui::Button::new(text).fill(Color32::TRANSPARENT).stroke(egui::Stroke::NONE).min_size(min.max(egui::vec2(110.0, H_BIG)));
    // Not faded further, as egui does to disabled widgets.
    let r = ui
        .scope(|ui| {
            ui.visuals_mut().disabled_alpha = 1.0;
            ui.add_enabled(enabled, button)
        })
        .inner;
    let pressed = r.is_pointer_button_down_on();
    let radius = R_SMALL;
    // Minimalist: flat, the fill alone says it's the main action.
    let (lip, edge) = if retro() { (if pressed { 1.0 } else { 4.0 }, accent().lerp_to_gamma(Color32::BLACK, 0.45)) } else { (0.0, Color32::TRANSPARENT) };
    let mut shapes = Vec::new();
    if enabled && lip > 0.0 {
        shapes.push(egui::Shape::rect_filled(r.rect.translate(egui::vec2(0.0, lip)), radius, edge));
    }
    let fill = match (enabled, r.hovered()) {
        (false, _) => pal().raised,
        (true, _) if pressed => shade(accent(), 0.2),
        (true, true) => shade(accent(), 0.12),
        (true, false) => accent(),
    };
    // Disabled, a soft edge keeps the slab from vanishing into a light page.
    let edge = if enabled { egui::Stroke::NONE } else { egui::Stroke::new(1.0, pal().border) };
    shapes.push(egui::epaint::RectShape::new(r.rect, radius, fill, edge, egui::StrokeKind::Inside).into());
    ui.painter().set(under, egui::Shape::Vec(shapes));
    r
}

/// Adds `button`. In Minimalist it's a flat raised surface with a hairline, not the theme's
/// stronger control outline (that one is for text boxes and checkboxes). Frameless buttons (in
/// menus) stay flat.
pub(crate) fn cap(ui: &mut egui::Ui, enabled: bool, button: egui::Button) -> egui::Response {
    let flat = !retro() && ui.visuals().button_frame;
    ui.add_enabled(enabled, if flat { button.stroke(egui::Stroke::new(1.0, pal().border)) } else { button })
}

/// A button that opens a menu of `content`, flat with a hairline in Minimalist (see `cap`).
/// Returns what `content` gave, while the menu is open.
pub(crate) fn cap_menu<R>(ui: &mut egui::Ui, button: egui::Button, content: impl FnOnce(&mut egui::Ui) -> R) -> (egui::Response, Option<R>) {
    let flat = !retro() && ui.visuals().button_frame;
    let (r, inner) = egui::containers::menu::MenuButton::from_button(if flat { button.stroke(egui::Stroke::new(1.0, pal().border)) } else { button.frame_when_inactive(ui.visuals().button_frame) }).ui(ui, content);
    (r, inner.map(|i| i.inner))
}

/// A plain button, flat with a hairline in Minimalist (see `cap`).
pub(crate) fn button<'a>(ui: &mut egui::Ui, text: impl egui::IntoAtoms<'a>) -> egui::Response {
    cap(ui, true, egui::Button::new(text))
}

/// A dialog's confirm button: its text and hover text, in red when it removes things.
pub(crate) struct Confirm<'a> {
    pub(crate) label: &'a str,
    pub(crate) hint: &'a str,
    pub(crate) danger: bool,
}

/// A dialog: its title, `body`, then `confirm` and Cancel (with `cancel` as its hover text).
/// `body` says whether confirm can be clicked and whether Enter was pressed to confirm. Some(true)
/// once confirmed, Some(false) once cancelled or clicked away, None while it stays up.
pub(crate) fn modal(ctx: &egui::Context, id: &str, width: f32, title: &str, confirm: Confirm, cancel: &str, body: impl FnOnce(&mut egui::Ui) -> (bool, bool)) -> Option<bool> {
    let mut done = None;
    let r = egui::Modal::new(egui::Id::unique(id)).show(ctx, |ui| {
        ui.set_max_width(width);
        ui.label(RichText::new(title).strong().size(16.0));
        let (ok, enter) = body(ui);
        ui.horizontal(|ui| {
            let text = RichText::new(confirm.label).strong();
            let r = if confirm.danger { cap(ui, ok, egui::Button::new(text.color(Color32::WHITE)).fill(RED)) } else { primary(ui, ok, text, egui::Vec2::ZERO) };
            if r.on_hover_text(confirm.hint).clicked() || (enter && ok) {
                done = Some(true);
            }
            if button(ui, "Cancel").on_hover_text(cancel).clicked() {
                done = Some(false);
            }
        });
    });
    done.or_else(|| r.should_close().then_some(false))
}

/// Keeps the keyboard in a dialog's text box and says whether Enter was pressed in it.
pub(crate) fn focus_enter(ui: &egui::Ui, edit: &egui::Response) -> bool {
    if !edit.has_focus() && !edit.lost_focus() {
        edit.request_focus();
    }
    edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
}

/// A titled card on the page.
pub(crate) fn card(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new().fill(pal().card).corner_radius(R_CARD).inner_margin(16).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(heading(title, if retro() { 15.0 } else { 16.0 }).color(pal().text));
        ui.add_space(4.0);
        body(ui);
    });
    ui.add_space(12.0);
}

/// A centered column at most `max` wide.
pub(crate) fn column<R>(ui: &mut egui::Ui, max: f32, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let w = ui.available_width().min(max);
    ui.horizontal(|ui| {
        ui.add_space((ui.available_width() - w) / 2.0);
        ui.vertical(|ui| {
            ui.set_width(w);
            body(ui)
        })
        .inner
    })
    .inner
}

/// Two-column label / control grid.
pub(crate) fn form(ui: &mut egui::Ui, id: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Grid::new(id).num_columns(2).spacing([14.0, 10.0]).min_col_width(110.0).show(ui, body);
}

/// A button that selects `value`, filled with the accent when selected: a pill in Retro, a keycap
/// in Minimalist.
pub(crate) fn chip<T: PartialEq>(ui: &mut egui::Ui, current: &mut T, value: T, text: impl Into<String>) -> egui::Response {
    let r = chip_look(ui, *current == value, text.into());
    if r.clicked() {
        *current = value;
    }
    r
}

/// A chip that turns `on` on and off, with a check mark while on so it doesn't rely on color.
pub(crate) fn toggle_chip(ui: &mut egui::Ui, on: &mut bool, text: &str) -> egui::Response {
    let label = if *on { format!("\u{2713} {text}") } else { text.to_string() };
    let r = chip_look(ui, *on, label);
    if r.clicked() {
        *on = !*on;
    }
    r
}

/// Retro: the picked chip filled with the accent. Minimalist: every chip a quiet raised surface,
/// the picked one ringed in the accent with its text in the accent's text color, so a page full
/// of choices doesn't turn into a wall of accent.
fn chip_look(ui: &mut egui::Ui, selected: bool, text: String) -> egui::Response {
    let button = if retro() {
        let (fill, color) = if selected { (accent(), on_accent()) } else { (pal().raised, pal().text) };
        let b = egui::Button::new(RichText::new(text).color(color)).fill(fill).corner_radius(R_PILL);
        // Filled with the accent, it needs no outline (the theme's hairline or hover one).
        if selected { b.stroke(egui::Stroke::NONE) } else { b }
    } else {
        let (color, stroke) = if selected { (accent_light(), egui::Stroke::new(1.5, accent())) } else { (pal().text, egui::Stroke::new(1.0, pal().border)) };
        egui::Button::new(RichText::new(text).color(color)).fill(pal().raised).stroke(stroke).corner_radius(R_SMALL)
    };
    ui.add(button.min_size(egui::vec2(0.0, H)))
}

/// A clip's tags as small pills, in a row.
pub(crate) fn tag_pills(ui: &mut egui::Ui, tags: &[String]) {
    ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
    for t in tags {
        egui::Frame::new().fill(pal().raised).corner_radius(R_KEY).inner_margin(egui::Margin::symmetric(6, 1)).show(ui, |ui| {
            ui.label(RichText::new(t).small().color(accent_light()));
        });
    }
}

/// An on/off switch.
pub(crate) fn toggle(on: &mut bool) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| {
        let size = egui::vec2(40.0, 22.0);
        let (rect, mut response) = ui.allocate_exact_size(size, egui::Sense::click());
        if response.clicked() {
            *on = !*on;
            response.mark_changed();
        }
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let fill = if ui.is_enabled() { pal().raised.lerp_to_gamma(accent(), t) } else { pal().card };
        // Off, an outline shows the track and the knob is grey, so it reads on a light card too.
        let edge = alpha(pal().outline, (255.0 * (1.0 - t)) as u8);
        ui.painter().rect(rect, 11.0, fill, egui::Stroke::new(1.0, edge), egui::StrokeKind::Inside);
        let x = egui::lerp((rect.left() + 11.0)..=(rect.right() - 11.0), t);
        let off = if pal().light { pal().muted } else { Color32::WHITE };
        ui.painter().circle_filled(egui::pos2(x, rect.center().y), 8.0, off.lerp_to_gamma(on_accent(), t));
        response
    }
}
