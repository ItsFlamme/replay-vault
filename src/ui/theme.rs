//! Colors, the palette, fonts and the page backdrop: what changes for look reasons only.

use crate::*;

/// Accent colors: name, main, light (text and marks), dim (pressed fills). Each main takes white
/// text at 4.5:1 or better and stands out 3:1 from the pages in both modes.
pub(crate) const ACCENTS: [(&str, Color32, Color32, Color32); 6] = [
    ("Purple", Color32::from_rgb(132, 72, 240), Color32::from_rgb(178, 136, 255), Color32::from_rgb(64, 38, 112)),
    ("Blue", Color32::from_rgb(48, 110, 226), Color32::from_rgb(124, 172, 255), Color32::from_rgb(28, 58, 112)),
    ("Green", Color32::from_rgb(28, 133, 76), Color32::from_rgb(110, 216, 150), Color32::from_rgb(20, 78, 48)),
    ("Orange", Color32::from_rgb(183, 91, 23), Color32::from_rgb(255, 170, 100), Color32::from_rgb(108, 54, 18)),
    ("Pink", Color32::from_rgb(204, 57, 136), Color32::from_rgb(255, 132, 196), Color32::from_rgb(104, 30, 72)),
    ("Red", Color32::from_rgb(214, 54, 54), Color32::from_rgb(255, 122, 122), Color32::from_rgb(102, 28, 28)),
];

/// The `accent` setting for "Custom": the color in `custom_accent`.
pub(crate) const CUSTOM_ACCENT: u8 = ACCENTS.len() as u8;

/// The accent in use (an index into ACCENTS, or CUSTOM_ACCENT), set from the settings every frame.
pub(crate) static ACCENT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// The custom accent as 0xRRGGBB, set from the settings every frame.
pub(crate) static CUSTOM: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x8448F0);

/// Light mode and the Retro theme (Dark and Minimalist when off), set from the settings every frame.
pub(crate) static LIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) static RETRO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Retro's scanlines, grain and glows are on (Retro, and "Reduce effects" off), set every frame.
pub(crate) static EFFECTS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn retro() -> bool {
    RETRO.load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn effects() -> bool {
    EFFECTS.load(std::sync::atomic::Ordering::Relaxed)
}

/// The accent's main, light and dim colors. A custom color is first made readable in the current
/// mode (see `fit_accent`), and its light and dim are mixed from that.
pub(crate) fn accent_of() -> (Color32, Color32, Color32) {
    match ACCENTS.get(ACCENT.load(std::sync::atomic::Ordering::Relaxed) as usize) {
        Some(&(_, main, light, dim)) => (main, light, dim),
        None => {
            let [_, r, g, b] = CUSTOM.load(std::sync::atomic::Ordering::Relaxed).to_be_bytes();
            let main = fit_accent(Color32::from_rgb(r, g, b), pal());
            (main, main.lerp_to_gamma(Color32::WHITE, 0.35), main.lerp_to_gamma(Color32::BLACK, 0.52))
        }
    }
}

pub(crate) fn accent() -> Color32 {
    accent_of().0
}

/// The accent for text and marks: lighter on dark pages, darker on light ones, 4.5:1 or better
/// on every page surface.
pub(crate) fn accent_light() -> Color32 {
    let (main, light, _) = accent_of();
    accent_text(main, light, pal())
}

/// `accent_light` in palette `p`, from the accent's `main` and `light`.
pub(crate) fn accent_text(main: Color32, light: Color32, p: &Palette) -> Color32 {
    let (from, to) = if p.light { (main.lerp_to_gamma(Color32::BLACK, 0.3), Color32::BLACK) } else { (light, Color32::WHITE) };
    reach(from, to, &[p.sidebar, p.bg, p.card, p.raised], 4.5)
}

/// WCAG relative luminance, 0 (black) to 1 (white).
pub(crate) fn luminance(c: Color32) -> f32 {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// WCAG contrast ratio between two colors, 1 to 21. Text needs 4.5, control edges 3.
pub(crate) fn contrast(a: Color32, b: Color32) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// The text color on a `fill`: white or the dark ink, whichever stands out more.
pub(crate) fn ink_on(fill: Color32) -> Color32 {
    if contrast(Color32::WHITE, fill) >= contrast(INK, fill) { Color32::WHITE } else { INK }
}

/// Text and marks on the accent.
pub(crate) fn on_accent() -> Color32 {
    ink_on(accent())
}

/// `fill` hovered (`t` about 0.12) or pressed (0.2): darker, or lighter when darker would make its
/// dark ink hard to read.
pub(crate) fn shade(fill: Color32, t: f32) -> Color32 {
    let darker = fill.lerp_to_gamma(Color32::BLACK, t);
    if contrast(ink_on(fill), darker) >= 4.5 { darker } else { fill.lerp_to_gamma(Color32::WHITE, t) }
}

/// `c` mixed toward `to` in 5% steps until it has `need` contrast with every color in `with`
/// (`to` itself if nothing short of it does).
pub(crate) fn reach(c: Color32, to: Color32, with: &[Color32], need: f32) -> Color32 {
    (0..=20).map(|i| c.lerp_to_gamma(to, i as f32 / 20.0)).find(|&x| with.iter().all(|&s| contrast(x, s) >= need)).unwrap_or(to)
}

/// An accent made readable in palette `p`: the nearest color to `c` (mixed toward black or white
/// in 2.5% steps) that stands out 3:1 from the sidebar, page and cards and takes white or the dark
/// ink at 4.5:1. Too light a color comes out darker in light mode, too dark one lighter in dark
/// mode. The presets come back unchanged.
pub(crate) fn fit_accent(c: Color32, p: &Palette) -> Color32 {
    let ok = |x: Color32| [p.sidebar, p.bg, p.card].iter().all(|&s| contrast(x, s) >= 3.0) && contrast(ink_on(x), x) >= 4.5;
    // Black and white end it, and one of them always passes: white on dark pages, black on light.
    (0..=40).flat_map(|i| [Color32::BLACK, Color32::WHITE].map(|to| c.lerp_to_gamma(to, i as f32 / 40.0))).find(|&x| ok(x)).unwrap_or(c)
}

/// The accent for pressed and disabled fills: darker on dark pages, paler on light ones.
pub(crate) fn accent_dim() -> Color32 {
    let dim = if pal().light { accent().lerp_to_gamma(Color32::WHITE, 0.45) } else { accent_of().2 };
    // Body text goes on it (a pressed button, a key being set): 4.5:1 even for a near-white or
    // near-black custom color.
    reach(dim, if pal().light { Color32::WHITE } else { Color32::BLACK }, &[pal().text], 4.5)
}

/// `c` see-through, for glows.
pub(crate) fn alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

/// "#RRGGBB" (the # is optional) as a color, or None if it isn't one.
pub(crate) fn parse_hex(s: &str) -> Option<Color32> {
    let h = s.trim();
    let h = h.strip_prefix('#').unwrap_or(h);
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let [_, r, g, b] = u32::from_str_radix(h, 16).ok()?.to_be_bytes();
    Some(Color32::from_rgb(r, g, b))
}

/// A color as "#RRGGBB".
pub(crate) fn hex(c: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
}

/// The colors that change between dark and light mode.
pub(crate) struct Palette {
    pub(crate) light: bool,
    /// Darkest (sidebar) to lightest (raised controls) in dark mode; the other way round in light.
    pub(crate) sidebar: Color32,
    pub(crate) bg: Color32,
    pub(crate) card: Color32,
    pub(crate) raised: Color32,
    pub(crate) border: Color32,
    /// Grey text, and body text.
    pub(crate) muted: Color32,
    pub(crate) text: Color32,
    /// Buttons hovered, and open combo boxes.
    pub(crate) hovered: Color32,
    pub(crate) open: Color32,
    /// Text boxes.
    pub(crate) field: Color32,
    /// The thin bottom edge of a keycap or a Minimalist button.
    pub(crate) edge: Color32,
    /// Warnings, the on-screen display's green and errors, as text (4.5:1 on every surface).
    pub(crate) amber: Color32,
    pub(crate) phosphor: Color32,
    pub(crate) red: Color32,
    /// The edge of a control that's only a box when empty or off (checkboxes, text boxes, combo
    /// boxes, the off switch): 3:1 against the cards.
    pub(crate) outline: Color32,
}

/// Retro's dark palette, the "tape shelf" of replayvault.app: dark plums and its lilac grey text.
pub(crate) const DARK: Palette = Palette {
    light: false,
    sidebar: Color32::from_rgb(14, 11, 19),
    bg: Color32::from_rgb(23, 18, 31),
    card: Color32::from_rgb(33, 26, 45),
    raised: Color32::from_rgb(46, 36, 60),
    border: Color32::from_rgb(53, 42, 71),
    muted: Color32::from_rgb(179, 170, 196),
    text: Color32::from_rgb(241, 237, 247),
    hovered: Color32::from_rgb(60, 47, 78),
    open: Color32::from_rgb(54, 43, 70),
    field: Color32::from_rgb(14, 11, 19),
    edge: Color32::from_rgb(18, 14, 25),
    amber: Color32::from_rgb(230, 170, 40),
    phosphor: Color32::from_rgb(95, 227, 208),
    red: Color32::from_rgb(255, 110, 100),
    outline: Color32::from_rgb(122, 108, 146),
};

/// The same shelf in daylight: lilac whites, plum text.
pub(crate) const LIGHT_PALETTE: Palette = Palette {
    light: true,
    sidebar: Color32::from_rgb(236, 232, 243),
    bg: Color32::from_rgb(246, 243, 250),
    card: Color32::from_rgb(255, 255, 255),
    raised: Color32::from_rgb(241, 237, 247),
    border: Color32::from_rgb(214, 206, 226),
    muted: Color32::from_rgb(94, 84, 112),
    text: Color32::from_rgb(30, 23, 41),
    hovered: Color32::from_rgb(230, 224, 240),
    open: Color32::from_rgb(234, 229, 242),
    field: Color32::from_rgb(255, 255, 255),
    edge: Color32::from_rgb(196, 187, 212),
    amber: Color32::from_rgb(140, 85, 0),
    phosphor: Color32::from_rgb(0, 110, 96),
    red: Color32::from_rgb(185, 35, 35),
    outline: Color32::from_rgb(142, 130, 162),
};

/// Minimalist's dark palette: plain graphite, so the clips carry the color and the accent marks
/// only what matters.
pub(crate) const GRAPHITE: Palette = Palette {
    light: false,
    sidebar: Color32::from_rgb(15, 15, 16),
    bg: Color32::from_rgb(20, 20, 21),
    card: Color32::from_rgb(28, 28, 30),
    raised: Color32::from_rgb(38, 38, 41),
    border: Color32::from_rgb(44, 44, 47),
    muted: Color32::from_rgb(161, 161, 166),
    text: Color32::from_rgb(237, 237, 237),
    hovered: Color32::from_rgb(48, 48, 52),
    open: Color32::from_rgb(44, 44, 47),
    field: Color32::from_rgb(15, 15, 16),
    edge: Color32::from_rgb(12, 12, 13),
    amber: Color32::from_rgb(230, 170, 40),
    phosphor: Color32::from_rgb(95, 227, 208),
    red: Color32::from_rgb(255, 110, 100),
    outline: Color32::from_rgb(112, 112, 118),
};

/// Minimalist's light palette: the same graphite in daylight.
pub(crate) const GRAPHITE_LIGHT: Palette = Palette {
    light: true,
    sidebar: Color32::from_rgb(244, 244, 245),
    bg: Color32::from_rgb(250, 250, 250),
    card: Color32::from_rgb(255, 255, 255),
    raised: Color32::from_rgb(242, 242, 243),
    border: Color32::from_rgb(229, 229, 231),
    muted: Color32::from_rgb(82, 82, 82),
    text: Color32::from_rgb(23, 23, 23),
    hovered: Color32::from_rgb(232, 232, 234),
    open: Color32::from_rgb(236, 236, 238),
    field: Color32::from_rgb(255, 255, 255),
    edge: Color32::from_rgb(212, 212, 216),
    amber: Color32::from_rgb(140, 85, 0),
    phosphor: Color32::from_rgb(0, 110, 96),
    red: Color32::from_rgb(185, 35, 35),
    outline: Color32::from_rgb(138, 138, 143),
};

/// The component scale: two corner radii (controls: buttons, keys, chips, fields, pills; and
/// cards: cards, tiles, tips, the player), Retro's round chips, and two button heights (everyday
/// buttons and chips; main buttons, the search box and the key buttons).
pub(crate) const R_SMALL: u8 = 8;

pub(crate) const R_CARD: u8 = 12;

/// Keycaps and tags: small enough that their corners stay tighter than the buttons'.
pub(crate) const R_KEY: u8 = 5;

pub(crate) const R_PILL: u8 = 14;

pub(crate) const H: f32 = 28.0;

pub(crate) const H_BIG: f32 = 34.0;

pub(crate) fn pal() -> &'static Palette {
    match (retro(), LIGHT.load(std::sync::atomic::Ordering::Relaxed)) {
        (true, true) => &LIGHT_PALETTE,
        (true, false) => &DARK,
        (false, true) => &GRAPHITE_LIGHT,
        (false, false) => &GRAPHITE,
    }
}

/// The site's tape-label paper and its ink.
pub(crate) const PAPER: Color32 = Color32::from_rgb(239, 230, 210);

pub(crate) const INK: Color32 = Color32::from_rgb(36, 27, 48);

/// The site's 80s orange and magenta, and the CRT on-screen display's phosphor green.
pub(crate) const TRACKING: Color32 = Color32::from_rgb(255, 122, 47);

pub(crate) const MAGENTA: Color32 = Color32::from_rgb(192, 80, 200);

pub(crate) const PHOSPHOR: Color32 = Color32::from_rgb(95, 227, 208);

/// The REC dot and the fill of buttons that remove things (white text on it reads 5.3:1). Red
/// text uses `pal().red`.
pub(crate) const RED: Color32 = Color32::from_rgb(200, 50, 45);

/// The app's fonts, then its colors and spacing.
pub(crate) fn apply_theme(ctx: &egui::Context) {
    use_windows_fonts(ctx);
    set_visuals(ctx);
}

/// The colours and spacing, in the current accent, mode and theme.
pub(crate) fn set_visuals(ctx: &egui::Context) {
    let p = pal();
    let mut v = if p.light { egui::Visuals::light() } else { egui::Visuals::dark() };
    v.panel_fill = p.bg;
    v.window_fill = p.card;
    v.window_stroke = egui::Stroke::new(1.0, p.border);
    v.extreme_bg_color = p.field;
    v.faint_bg_color = p.card;
    v.selection.bg_fill = accent();
    v.selection.stroke = egui::Stroke::new(1.0, on_accent());
    v.hyperlink_color = accent_light();
    v.slider_trailing_fill = true;
    // Buttons share the hotkey keycaps' corners (Minimalist buttons are keycaps).
    let radius = egui::CornerRadius::same(R_SMALL);
    let w = &mut v.widgets;
    w.noninteractive.bg_stroke = egui::Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = egui::Stroke::new(1.0, p.text);
    for (state, fill) in [(&mut w.inactive, p.raised), (&mut w.hovered, p.hovered), (&mut w.active, accent_dim()), (&mut w.open, p.open)] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.corner_radius = radius;
        // An edge that stands out from the card, so empty text boxes and unticked checkboxes show.
        // Minimalist keycaps drop it (see `cap`).
        state.bg_stroke = egui::Stroke::new(1.0, p.outline);
        state.fg_stroke = egui::Stroke::new(1.0, p.text);
    }
    w.hovered.bg_stroke = egui::Stroke::new(1.0, accent());
    let theme = if p.light { egui::Theme::Light } else { egui::Theme::Dark };
    ctx.set_theme(theme);
    ctx.set_visuals_of(theme, v);
    ctx.style_mut_of(theme, |s| {
        s.spacing.item_spacing = egui::vec2(8.0, 8.0);
        s.spacing.button_padding = egui::vec2(12.0, 6.0);
        s.spacing.interact_size.y = H;
        s.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
        s.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
        s.text_styles.insert(egui::TextStyle::Small, egui::FontId::proportional(12.0));
        s.text_styles.insert(egui::TextStyle::Heading, egui::FontId::new(24.0, egui::FontFamily::Name("strong".into())));
    });
}

/// The fonts of replayvault.app, bundled (see assets\fonts for their licenses): Atkinson
/// Hyperlegible for text (its bold as the "strong" family), VT323 (a VCR on-screen display font)
/// as "osd" for page titles and the recording readout, and Permanent Marker as "marker" for the
/// tape labels. Segoe UI and Segoe UI Symbol from Windows sit behind them for characters they
/// lack, like ✔ and ⚠.
pub(crate) fn use_windows_fonts(ctx: &egui::Context) {
    let dir = std::path::Path::new(&std::env::var("WINDIR").unwrap_or(r"C:\Windows".into())).join("Fonts");
    let load = |name: &str| std::fs::read(dir.join(name)).ok();
    let mut fonts = egui::FontDefinitions::default();
    for (name, bytes) in [
        ("atkinson", &include_bytes!("../../assets/fonts/AtkinsonHyperlegible-Regular.ttf")[..]),
        ("atkinson-bold", &include_bytes!("../../assets/fonts/AtkinsonHyperlegible-Bold.ttf")[..]),
        ("vt323", &include_bytes!("../../assets/fonts/VT323-Regular.ttf")[..]),
        ("marker", &include_bytes!("../../assets/fonts/PermanentMarker-Regular.ttf")[..]),
    ] {
        fonts.font_data.insert(name.into(), Arc::new(egui::FontData::from_static(bytes)));
    }
    let mut fallback = vec![];
    for (name, file) in [("segoe", "segoeui.ttf"), ("segoe-symbol", "seguisym.ttf")] {
        if let Some(data) = load(file) {
            fonts.font_data.insert(name.into(), Arc::new(egui::FontData::from_owned(data)));
            fallback.push(name.to_string());
        }
    }
    let proportional = fonts.families.entry(egui::FontFamily::Proportional).or_default();
    for (i, f) in ["atkinson".to_string()].into_iter().chain(fallback).enumerate() {
        proportional.insert(i, f);
    }
    let rest = proportional.clone();
    for (family, first) in [("strong", "atkinson-bold"), ("osd", "vt323"), ("marker", "marker")] {
        let list = [first.to_string()].into_iter().chain(rest.iter().filter(|f| *f != "atkinson").cloned()).collect();
        fonts.families.insert(egui::FontFamily::Name(family.into()), list);
    }
    // Windows' icon font, for the sidebar and buttons.
    if let Some(icons) = load("segmdl2.ttf") {
        fonts.font_data.insert("icons".into(), Arc::new(egui::FontData::from_owned(icons)));
        fonts.families.insert(egui::FontFamily::Name("icons".into()), vec!["icons".into()]);
        ICONS.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    ctx.set_fonts(fonts);
}

/// Segoe MDL2 Assets was found and loaded.
pub(crate) static ICONS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A soft round light: `color` in the middle, fading to nothing at `radius`.
pub(crate) fn glow(painter: &egui::Painter, center: egui::Pos2, radius: f32, color: Color32) {
    oval_glow(painter, center, egui::vec2(radius, radius), color);
}

/// `glow`, stretched to an oval `radii` wide and tall.
pub(crate) fn oval_glow(painter: &egui::Painter, center: egui::Pos2, radii: egui::Vec2, color: Color32) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(center, color);
    const N: u32 = 64;
    for i in 0..N {
        let a = i as f32 / N as f32 * std::f32::consts::TAU;
        mesh.colored_vertex(center + radii * egui::vec2(a.cos(), a.sin()), Color32::TRANSPARENT);
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % N);
    }
    painter.add(mesh);
}

/// The site's room behind a page, painted over `rect`'s plain fill: soft pools of purple, teal,
/// orange and magenta light, faint scanlines and grain over them, and darker corners. All still,
/// and only a few shapes. In light mode: pastel pools, much fainter lines and grain, no dark corners.
/// Retro only: Minimalist, and Retro with "Reduce effects", keep the plain fill.
pub(crate) fn room(ui: &egui::Ui, rect: egui::Rect) {
    if !effects() {
        return;
    }
    let light = pal().light;
    let painter = ui.ctx().layer_painter(ui.layer_id()).with_clip_rect(rect);
    // Where the site puts its light pools, as parts of the page, and how strong each is.
    let at = |x: f32, y: f32| rect.min + rect.size() * egui::vec2(x, y);
    for (x, y, rx, ry, color, a) in [
        (0.11, 0.03, 0.35, 0.29, accent(), 70),
        (0.97, 0.34, 0.29, 0.25, PHOSPHOR, 32),
        (0.21, 0.92, 0.33, 0.27, TRACKING, 26),
        (0.92, 1.02, 0.27, 0.24, MAGENTA, 38),
    ] {
        let a = if light { a * 2 / 3 } else { a };
        oval_glow(&painter, at(x, y), rect.size() * egui::vec2(rx, ry), alpha(color, a));
    }
    // Scanlines and grain, one small tile repeated, made once for each mode.
    let id = ui.scope_id().with(("room grain", light));
    let tex = ui.ctx().data_mut(|d| d.get_temp::<egui::TextureHandle>(id)).unwrap_or_else(|| {
        const S: usize = 128;
        let mut seed = 0x9E37_79B9u32;
        let pixels = (0..S * S)
            .map(|i| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                // Light grain, up to about 9% white, under a 16% dark line every fourth row. In light
                // mode, dark grain up to 3% and 4% lines.
                let (most, line_a) = if light { (0.03, 0.04) } else { (0.09, 0.16) };
                let grain = (seed >> 24) as f32 / 255.0 * most;
                let line = if (i / S).is_multiple_of(4) { line_a } else { 0.0 };
                let a = grain + line - grain * line;
                let white = if light { 0 } else { (grain * (1.0 - line) * 255.0) as u8 };
                Color32::from_rgba_premultiplied(white, white, white, (a * 255.0) as u8)
            })
            .collect();
        let image = egui::ColorImage::new([S, S], pixels);
        let tex = ui.ctx().load_texture("room grain", image, egui::TextureOptions::LINEAR_REPEAT);
        ui.ctx().data_mut(|d| d.insert_temp(id, tex.clone()));
        tex
    });
    let mut mesh = egui::Mesh::with_texture(tex.id());
    let tile = tex.size_vec2();
    mesh.add_rect_with_uv(rect, egui::Rect::from_min_size(egui::Pos2::ZERO, rect.size() / tile), Color32::WHITE);
    painter.add(mesh);
    if light {
        return;
    }
    // Corners darken from just past the middle, like an old TV picture.
    let mut mesh = egui::Mesh::default();
    let dark = Color32::from_rgba_unmultiplied(5, 3, 8, 110);
    let half = rect.size() / 2.0 * std::f32::consts::SQRT_2;
    const N: u32 = 64;
    for i in 0..N {
        let a = i as f32 / N as f32 * std::f32::consts::TAU;
        let dir = egui::vec2(a.cos(), a.sin()) * half;
        mesh.colored_vertex(rect.center() + dir * 0.55, Color32::TRANSPARENT);
        mesh.colored_vertex(rect.center() + dir, dark);
        let (j, k) = (2 * i, 2 * ((i + 1) % N));
        mesh.add_triangle(j, j + 1, k);
        mesh.add_triangle(k, j + 1, k + 1);
    }
    painter.add(mesh);
}

/// A 2 px accent ring around the widget with keyboard focus, once Tab or an arrow key moved it
/// there (a click hides it again). Called last in the frame, so it's drawn over the widget.
pub(crate) fn focus_ring(ctx: &egui::Context) {
    use egui::Key::{ArrowDown, ArrowLeft, ArrowRight, ArrowUp, Tab};
    let id = egui::Id::NULL.with("keyboard focus");
    let (keys, click) = ctx.input(|i| (i.events.iter().any(|e| matches!(e, egui::Event::Key { key: Tab | ArrowUp | ArrowDown | ArrowLeft | ArrowRight, pressed: true, .. })), i.pointer.any_pressed()));
    let on = ctx.data_mut(|d| {
        let on = d.get_temp_mut_or(id, false);
        *on = (*on || keys) && !click;
        *on
    });
    if on
        && let Some(r) = ctx.memory(|m| m.focused()).and_then(|f| ctx.read_response(f))
    {
        let painter = ctx.layer_painter(r.layer_id).with_clip_rect(r.interact_rect.expand(3.0));
        painter.rect_stroke(r.rect.expand(1.0), R_SMALL, egui::Stroke::new(2.0, accent()), egui::StrokeKind::Outside);
    }
}
