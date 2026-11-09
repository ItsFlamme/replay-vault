//! What's new: CHANGELOG.md, built into the app, shown version by version.

use crate::*;

const CHANGELOG: &str = include_str!("../../CHANGELOG.md");

/// One release: its heading ("0.1.0 - 2026-11-20"), its groups (New, Fixed, Changed) with their
/// lines, and any note under it.
pub(crate) struct Release<'a> {
    pub(crate) title: &'a str,
    pub(crate) groups: Vec<(&'a str, Vec<&'a str>)>,
    pub(crate) notes: Vec<&'a str>,
}

/// The released versions in the changelog, newest first. Unreleased is left out.
pub(crate) fn releases(text: &str) -> Vec<Release<'_>> {
    let mut out: Vec<Release> = Vec::new();
    for line in text.lines().map(str::trim_end) {
        if let Some(title) = line.strip_prefix("## ") {
            out.push(Release { title: title.trim(), groups: Vec::new(), notes: Vec::new() });
        } else if let Some(r) = out.last_mut() {
            if let Some(group) = line.strip_prefix("### ") {
                r.groups.push((group.trim(), Vec::new()));
            } else if let Some(item) = line.strip_prefix("- ") {
                match r.groups.last_mut() {
                    Some((_, items)) => items.push(item.trim()),
                    None => r.notes.push(item.trim()),
                }
            } else if !line.trim().is_empty() {
                r.notes.push(line.trim());
            }
        }
    }
    out.retain(|r| !r.title.eq_ignore_ascii_case("unreleased"));
    out
}

impl App {
    /// Each version is a card that opens and closes by its heading; the newest starts open.
    pub(crate) fn whats_new_tab(&mut self, ui: &mut egui::Ui) {
        for (i, r) in releases(CHANGELOG).iter().enumerate() {
            let head = if r.title.split(' ').next() == Some(env!("CARGO_PKG_VERSION")) { format!("{}  (this version)", r.title) } else { r.title.to_string() };
            let id = ui.make_persistent_id(("whats-new", r.title));
            let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, i == 0);
            egui::Frame::new().fill(pal().card).corner_radius(R_CARD).inner_margin(16).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let open = state.is_open();
                let summary = r.groups.iter().map(|(g, items)| format!("{} {}", items.len(), g.to_lowercase())).collect::<Vec<_>>().join(", ");
                // One borderless button for the whole heading: chevron, version, and when closed
                // how much changed.
                let mut job = egui::text::LayoutJob::default();
                let fmt = |font: egui::FontId, color: Color32| egui::TextFormat { font_id: font, valign: egui::Align::Center, color, ..Default::default() };
                if ICONS.load(std::sync::atomic::Ordering::Relaxed) {
                    job.append(&(if open { icon::CHEVRON_DOWN } else { icon::RIGHT }).to_string(), 0.0, fmt(egui::FontId::new(12.0, egui::FontFamily::Name("icons".into())), pal().muted));
                }
                job.append(&head, if job.is_empty() { 0.0 } else { 10.0 }, fmt(egui::FontId::new(15.0, egui::FontFamily::Name("strong".into())), pal().text));
                if !open && !summary.is_empty() {
                    job.append(&summary, 12.0, fmt(egui::FontId::proportional(12.0), pal().muted));
                }
                let header = ui
                    .add(egui::Button::new(job).frame(false))
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(if open { "Hide this version's changes" } else { "Show this version's changes" });
                if header.clicked() {
                    state.toggle(ui);
                }
                state.show_body_unindented(ui, |ui| {
                    ui.add_space(8.0);
                    for (group, items) in &r.groups {
                        ui.label(heading(*group, 14.0).color(pal().text));
                        for item in items {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new("\u{2022}").color(pal().muted));
                                ui.label(*item);
                            });
                        }
                        ui.add_space(6.0);
                    }
                    for note in &r.notes {
                        ui.label(RichText::new(*note).small().color(pal().muted));
                    }
                });
                state.store(ui.ctx());
            });
            ui.add_space(12.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changelog_reads_as_releases() {
        let text = "# Changelog\nintro\n\n## Unreleased\n### New\n- not out yet\n\n## 0.2.0 - 2026-12-01\n### Fixed\n- Save key works again.\n### New\n- A thing.\n\nA note.\n## 0.1.0\n- First.\n";
        let r = releases(text);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].title, "0.2.0 - 2026-12-01");
        assert_eq!(r[0].groups, vec![("Fixed", vec!["Save key works again."]), ("New", vec!["A thing."])]);
        assert_eq!(r[0].notes, vec!["A note."]);
        assert_eq!(r[1].notes, vec!["First."]);
    }

    /// The real changelog has this version in it, so a release can't ship without its notes.
    #[test]
    fn changelog_has_this_version() {
        let r = releases(CHANGELOG);
        assert!(r.iter().any(|r| r.title.split(' ').next() == Some(env!("CARGO_PKG_VERSION"))), "add ## {} to CHANGELOG.md", env!("CARGO_PKG_VERSION"));
        assert!(r.iter().all(|r| !r.groups.is_empty()));
    }
}
