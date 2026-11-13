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

/// Glyphs in Segoe MDL2 Assets.
pub(crate) mod icon {
    pub const CLIPS: char = '\u{E8B2}'; // MovieClapper
    pub const SETTINGS: char = '\u{E713}';
    pub const KEYBOARD: char = '\u{E765}';
    pub const VIDEO: char = '\u{E714}';
    pub const AUDIO: char = '\u{E767}';
    pub const MIC: char = '\u{E720}'; // Microphone
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

/// A heading or a name, in the look's heading face (Minimalist keeps bold for page titles and lets
/// size and color carry the hierarchy).
pub(crate) fn heading(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).family(look().heading.family()).size(size)
}

/// A readout, like the setup step counter: the VCR on-screen display font in Retro, small bold
/// text in Minimalist.
pub(crate) fn osd(text: impl Into<String>, size: f32) -> RichText {
    look().readout.text(text, size)
}

/// A sidebar section heading, as the look draws it.
pub(crate) fn side_heading(ui: &mut egui::Ui, text: &str) {
    let h = look().side_heading;
    ui.horizontal(|ui| {
        ui.add_space(h.indent);
        let text = if h.caps { text.to_uppercase() } else { text.to_string() };
        ui.label(RichText::new(text).family(h.face.family()).size(h.size).color(pal().muted));
    });
}

/// A big title in the look's title type. With fringes (and effects on), the site's faint orange
/// and teal fringes sit behind it, like a TV picture.
pub(crate) fn retro_title(ui: &mut egui::Ui, text: &str, size: f32) {
    let title = look().title;
    if !look().title_fringe {
        ui.label(title.text(text, size).color(pal().text));
        return;
    }
    let galley = ui.painter().layout_no_wrap(text.to_string(), title.face.font(title.size(size)), pal().text);
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
/// Minimalist: flat caps like its buttons, with a thin darker edge under them. Retro: a VCR
/// remote's dark buttons on a lip, in the VCR font. Returns the width taken.
pub(crate) fn keycaps(painter: &egui::Painter, at: egui::Pos2, keys: &str) -> f32 {
    let k = look().keycap;
    let font = k.face.font(k.size);
    let (fill, edge, ink) = match k.fill {
        KeyFill::Raised => (pal().raised, pal().edge, pal().text),
        KeyFill::Remote => (pal().key, pal().lip, pal().text),
    };
    let (radius, depth) = (k.radius, k.depth);
    let mut x = at.x;
    for (i, key) in keys.split('+').enumerate() {
        if i > 0 {
            let plus = painter.layout_no_wrap("+".into(), font.clone(), pal().muted);
            painter.galley(egui::pos2(x + 5.0, at.y - plus.size().y / 2.0), plus.clone(), pal().muted);
            x += plus.size().x + 10.0;
        }
        let key = if k.caps { key.trim().to_uppercase() } else { key.trim().to_string() };
        let g = painter.layout_no_wrap(key, font.clone(), ink);
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
    let words = text.text().to_string();
    let text = if look().key_text.is_some() { key_label(&words) } else { text.family(egui::FontFamily::Name("strong".into())) };
    let text = text.color(if enabled { on_accent() } else { pal().muted });
    let button = egui::Button::new(text).fill(Color32::TRANSPARENT).stroke(egui::Stroke::NONE).min_size(min.max(egui::vec2(110.0, H_BIG)));
    // Not faded further, as egui does to disabled widgets.
    let r = ui
        .scope(|ui| {
            ui.visuals_mut().disabled_alpha = 1.0;
            ui.add_enabled(enabled, button)
        })
        .inner;
    key_named(&r, enabled, &words);
    let pressed = r.is_pointer_button_down_on();
    // Flat: the fill alone says it's the main action.
    let (lip, edge, radius) = match look().button {
        Button::Chunky { lip, radius } => (if pressed { 1.0 } else { lip }, accent().lerp_to_gamma(Color32::BLACK, 0.45), radius),
        Button::Flat => (0.0, Color32::TRANSPARENT, R_SMALL),
    };
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
/// stronger control outline (that one is for text boxes and checkboxes). Chunky: a remote-control
/// key on a dark lip. Frameless buttons (in menus) stay flat.
pub(crate) fn cap(ui: &mut egui::Ui, enabled: bool, button: egui::Button) -> egui::Response {
    match look().button {
        Button::Flat if ui.visuals().button_frame => ui.add_enabled(enabled, button.stroke(egui::Stroke::new(1.0, pal().border))),
        Button::Chunky { lip, radius } if ui.visuals().button_frame => on_lip(ui, lip, radius, pal().lip, |ui| ui.add_enabled(enabled, button.corner_radius(radius))),
        _ => ui.add_enabled(enabled, button),
    }
}

/// A button that opens a menu of `content`, flat with a hairline in Minimalist, a key on a lip
/// when chunky (see `cap`). Returns what `content` gave, while the menu is open.
pub(crate) fn cap_menu<R>(ui: &mut egui::Ui, button: egui::Button, content: impl FnOnce(&mut egui::Ui) -> R) -> (egui::Response, Option<R>) {
    let framed = ui.visuals().button_frame;
    let (r, inner) = match look().button {
        Button::Flat if framed => egui::containers::menu::MenuButton::from_button(button.stroke(egui::Stroke::new(1.0, pal().border))).ui(ui, content),
        Button::Chunky { lip, radius } if framed => {
            let mut out = None;
            let r = on_lip(ui, lip, radius, pal().lip, |ui| {
                let (r, inner) = egui::containers::menu::MenuButton::from_button(button.corner_radius(radius).frame_when_inactive(true)).ui(ui, content);
                out = inner;
                r
            });
            (r, out)
        }
        _ => egui::containers::menu::MenuButton::from_button(button.frame_when_inactive(framed)).ui(ui, content),
    };
    (r, inner.map(|i| i.inner))
}

/// Adds a widget with a key's lip under it: `lip` tall in `color`, sunk to 1 while it's pressed.
pub(crate) fn on_lip(ui: &mut egui::Ui, lip: f32, radius: u8, color: Color32, add: impl FnOnce(&mut egui::Ui) -> egui::Response) -> egui::Response {
    let under = ui.painter().add(egui::Shape::Noop);
    let r = add(ui);
    let lip = if r.is_pointer_button_down_on() { 1.0 } else { lip };
    ui.painter().set(under, egui::Shape::rect_filled(r.rect.translate(egui::vec2(0.0, lip)), radius, color));
    r
}

/// `text` as the look writes it on keys (VCR capitals in Retro), or plain.
pub(crate) fn key_label(text: &str) -> RichText {
    match look().key_text {
        Some(k) => RichText::new(if k.caps { text.to_uppercase() } else { text.to_string() }).family(k.face.family()).size(k.size).extra_letter_spacing(k.spacing),
        None => RichText::new(text),
    }
}

/// A key whose words `key_label` may have put in capitals keeps its plain name, for screen
/// readers (and tests that find it by name).
pub(crate) fn key_named(r: &egui::Response, enabled: bool, text: &str) {
    if look().key_text.is_some() && !text.is_empty() {
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, text));
    }
}

/// A tape label across `rect`: the cream paper, a color `stripe` down its left and `text` in the
/// marker font (wrapped when it's too long). The text is a real label, so it's read out like any
/// heading.
pub(crate) fn tape_label(ui: &mut egui::Ui, rect: egui::Rect, text: &str, stripe: Color32, radius: egui::CornerRadius) {
    let p = ui.painter();
    p.rect_filled(rect, radius, PAPER);
    p.rect_filled(rect.with_max_x(rect.left() + 6.0), egui::CornerRadius { nw: radius.nw, sw: radius.sw, ne: 0, se: 0 }, stripe);
    let words = egui::Rect::from_min_max(egui::pos2(rect.left() + 16.0, rect.top()), egui::pos2(rect.right() - 8.0, rect.bottom()));
    let label = egui::Label::new(RichText::new(text).family(Face::Marker.family()).size(17.0).color(INK)).wrap().selectable(false);
    // In a child of its own, so it takes no room in `ui` (the room is the caller's to give).
    ui.new_child(egui::UiBuilder::new().max_rect(words).layout(egui::Layout::left_to_right(egui::Align::Center))).add(label);
}

/// The stripe color of the cards on the page being drawn (the page's own tape), set before it's
/// drawn; the accent until then.
pub(crate) fn set_card_tape(ctx: &egui::Context, color: Color32) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::NULL.with("card tape"), color));
}

fn card_tape(ctx: &egui::Context) -> Color32 {
    ctx.data(|d| d.get_temp(egui::Id::NULL.with("card tape"))).unwrap_or_else(accent)
}

/// A dialog's title: bold text, or a tape label across the top of the dialog.
pub(crate) fn dialog_title(ui: &mut egui::Ui, title: &str) {
    if look().labels == Labels::Plain {
        ui.label(RichText::new(title).strong().size(16.0));
        return;
    }
    // Out to the dialog's edges, over its margin.
    let m = ui.style().spacing.menu_margin;
    // Tall enough for a title that takes two lines (see `tape_label` for the 24 px of padding).
    let wrap = ui.available_width() + (m.left + m.right) as f32 - 24.0;
    let height = ui.painter().layout(title.to_string(), Face::Marker.font(17.0), INK, wrap).size().y + 12.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height.max(30.0)), egui::Sense::hover());
    let strip = egui::Rect::from_min_max(rect.min - egui::vec2(m.left as f32, m.top as f32), egui::pos2(rect.right() + m.right as f32, rect.bottom()));
    let r = look().card_radius;
    tape_label(ui, strip, title, accent(), egui::CornerRadius { nw: r, ne: r, sw: 0, se: 0 });
    ui.add_space(4.0);
}

/// A dialog in the look's frame (Minimalist keeps egui's own).
pub(crate) fn dialog(ctx: &egui::Context, id: &str) -> egui::Modal {
    let m = egui::Modal::new(egui::Id::unique(id));
    if look().labels == Labels::Plain { m } else { m.frame(egui::Frame::popup(&ctx.global_style()).corner_radius(look().card_radius)) }
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
    let r = dialog(ctx, id).show(ctx, |ui| {
        ui.set_max_width(width);
        dialog_title(ui, title);
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

/// A titled card on the page. With tape labels, the title is a tape label across its top, striped
/// in the page's color (see `set_card_tape`).
pub(crate) fn card(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    let (r, tape) = (look().card_radius, look().labels == Labels::Tape);
    let margin = if tape { egui::Margin { left: 16, right: 16, top: 0, bottom: 16 } } else { egui::Margin::same(16) };
    // The label's room, filled in once the card's width is known.
    let mut label = egui::Rect::NOTHING;
    let shown = egui::Frame::new().fill(pal().card).corner_radius(r).inner_margin(margin).show(ui, |ui| {
        ui.set_width(ui.available_width());
        if tape {
            label = ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::hover()).0;
            ui.add_space(6.0);
        } else {
            ui.label(heading(title, look().card_title).color(pal().text));
            ui.add_space(4.0);
        }
        body(ui);
    });
    if tape {
        let card = shown.response.rect;
        let strip = egui::Rect::from_x_y_ranges(card.x_range(), card.top()..=label.bottom());
        tape_label(ui, strip, title, card_tape(ui.ctx()), egui::CornerRadius { nw: r, ne: r, sw: 0, se: 0 });
    }
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

/// Settings rows share one grid, so they line up within a card and across a page's cards: a label
/// column this wide, the control, then a slot this wide at the right edge (for Clear / Set or a
/// small status), with this gap between them.
const ROW_LABEL: f32 = 170.0;
const ROW_SLOT: f32 = 72.0;
const ROW_GAP: f32 = 12.0;

/// A settings row with its slot left empty: `label`, then `control`.
pub(crate) fn row<R>(ui: &mut egui::Ui, label: impl Into<egui::WidgetText>, control: impl FnOnce(&mut egui::Ui) -> R) -> R {
    row_slot(ui, label, control, |_| {})
}

/// A settings row: `label`, then `control`, then `slot` at the right edge. A row is at least H
/// tall, with the label and the slot centered beside the control however many lines it takes.
pub(crate) fn row_slot<R>(ui: &mut egui::Ui, label: impl Into<egui::WidgetText>, control: impl FnOnce(&mut egui::Ui) -> R, slot: impl FnOnce(&mut egui::Ui)) -> R {
    let gap = ui.spacing().item_spacing.x;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = ROW_GAP;
        let top = ui.cursor().min.y;
        // The label is placed once the row's height is known.
        let left = ui.allocate_space(egui::vec2(ROW_LABEL, H)).1.left();
        let width = (ui.available_width() - ROW_SLOT - ROW_GAP).max(0.0);
        let control = ui.allocate_ui_with_layout(egui::vec2(width, H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_min_size(egui::vec2(width, H));
            ui.spacing_mut().item_spacing.x = gap;
            control(ui)
        });
        let height = control.response.rect.height();
        ui.allocate_ui_with_layout(egui::vec2(ROW_SLOT, height), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.set_min_size(egui::vec2(ROW_SLOT, height));
            ui.spacing_mut().item_spacing.x = gap;
            slot(ui);
        });
        let rect = egui::Rect::from_x_y_ranges(left..=left + ROW_LABEL, top..=ui.min_rect().bottom());
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
            ui.add(egui::Label::new(label).truncate());
        });
        control.inner
    })
    .inner
}

/// The quiet button in a settings row's slot (Clear, Set): muted text, framed on hover.
pub(crate) fn slot_button(ui: &mut egui::Ui, enabled: bool, text: &str) -> egui::Response {
    let label = if look().key_text.is_some() { key_label(text) } else { RichText::new(text).size(13.0) };
    let r = ui.add_enabled(enabled, egui::Button::new(label.color(pal().muted)).frame_when_inactive(false).min_size(egui::vec2(0.0, H)));
    key_named(&r, enabled, text);
    r
}

/// A button that selects `value`, marked when selected (see `chip_with`).
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

/// A checkbox for something this copy may not be allowed to do (see `instance`). Not allowed, it's
/// greyed and unticked whatever `on` says, as this copy doesn't do it, with `why` under it. `on`
/// only changes on a click, so just looking at the page never changes a setting.
pub(crate) fn allowed_checkbox(ui: &mut egui::Ui, allowed: bool, on: &mut bool, text: &str, why: &str) -> egui::Response {
    let mut shown = allowed && *on;
    let r = ui.add_enabled(allowed, egui::Checkbox::new(&mut shown, text));
    if r.changed() {
        *on = shown;
    }
    if !allowed {
        ui.label(RichText::new(why).small().color(pal().muted));
    }
    r
}

fn chip_look(ui: &mut egui::Ui, selected: bool, text: String) -> egui::Response {
    chip_with(ui, selected, &text, |ink| key_label(&text).color(ink))
}

/// A chip of `atoms`, made in the chip's text color by `atoms(ink)`; `name` is what it's called
/// to screen readers. Remote-control keys: on a dark lip, the picked one filled with the accent on
/// a darker accent lip. Minimalist: every chip a quiet raised surface, the picked one ringed in the
/// accent with its text in the accent's text color, so a page full of choices doesn't turn into a
/// wall of accent.
pub(crate) fn chip_with<'a, A: egui::IntoAtoms<'a>>(ui: &mut egui::Ui, selected: bool, name: &str, atoms: impl FnOnce(Color32) -> A) -> egui::Response {
    let r = match look().chip {
        Chip::Key { lip, radius } => {
            let (fill, ink, edge) = if selected { (accent(), on_accent(), accent().lerp_to_gamma(Color32::BLACK, 0.45)) } else { (pal().key, pal().text, pal().lip) };
            // The lip is its edge: no outline (the theme's hairline or hover one).
            let b = egui::Button::new(atoms(ink)).fill(fill).stroke(egui::Stroke::NONE).corner_radius(radius).min_size(egui::vec2(0.0, H));
            on_lip(ui, lip, radius, edge, |ui| ui.add(b))
        }
        Chip::Ring => {
            let (ink, stroke) = if selected { (accent_light(), egui::Stroke::new(1.5, accent())) } else { (pal().text, egui::Stroke::new(1.0, pal().border)) };
            ui.add(egui::Button::new(atoms(ink)).fill(pal().raised).stroke(stroke).corner_radius(R_SMALL).min_size(egui::vec2(0.0, H)))
        }
    };
    key_named(&r, true, name);
    r
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

/// For the pages' own tests: one page of a fresh app, drawn in a harness.
#[cfg(test)]
pub(crate) mod testing {
    use crate::*;

    /// A harness drawing `show` for an app with default settings, nothing running and nothing
    /// looked up (`setup` can change it first). The keys are never checked against other programs
    /// (that tries them for a moment), and no graphics card overlay is found.
    pub(crate) fn page(setup: impl FnOnce(&mut App), mut show: impl FnMut(&mut egui::Ui, &mut App) + 'static) -> egui_kittest::Harness<'static, App> {
        let (cmds, _) = std::sync::mpsc::channel();
        let (_, events) = std::sync::mpsc::channel();
        let hw = profile::Hardware { gpu: recorder::Gpu::Nvidia, gpu_name: "Test GPU".into(), vram_mb: 8192, ram_mb: 16384, integrated: false };
        let mut app = App::base(Config::default(), cmds, events, hw, clips::Previews::new(|| {}));
        app.saved = app.cfg.clone();
        (app.nvidia, app.amd) = (Some((false, false)), Some(None));
        setup(&mut app);
        // The look follows this test's settings, not whatever the thread drew last.
        crate::store_look(&app.cfg);
        let frame = move |ui: &mut egui::Ui, app: &mut App| {
            app.conflicts_keys = Target::ALL.into_iter().map(|t| app.hotkey_of(t).clone()).collect();
            app.conflicts_checked = Some(std::time::Instant::now());
            show(ui, app);
        };
        egui_kittest::Harness::builder().with_size([1000.0, 900.0]).with_accessibility_check(false).with_context_setup(apply_theme).build_ui_state(frame, app)
    }
}
