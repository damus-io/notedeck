//! Small shared board widgets: progress pills, label chips, count badges,
//! section labels and headings, the Linear-style status and priority icons and
//! their colors, the card frame, and nostr URIs for copy-link.

use nostrdb_net::NoteId;
use notedeck::ColorTheme;
use notedeck::tokens::{
    BUTTON_SM, PALETTE, RADIUS_MD, RADIUS_PILL, SPACING_MD, SPACING_SM, SPACING_XS, STROKE_THIN,
};

use crate::event::{self, Priority};

/// Linear's status-circle yellow, painted for columns in the first half of the
/// board's middle (e.g. In Progress). Deliberately theme-independent, like the
/// status colors it mirrors.
const STATUS_STARTED_EARLY: egui::Color32 = egui::Color32::from_rgb(0xF2, 0xC9, 0x4C);

/// Linear's status-circle green, for columns in the second half of the middle
/// (e.g. In Review).
const STATUS_STARTED_LATE: egui::Color32 = egui::Color32::from_rgb(0x4C, 0xB7, 0x82);

/// Linear's status-circle indigo, for the board's final (done) column.
pub(super) const STATUS_DONE: egui::Color32 = egui::Color32::from_rgb(0x5E, 0x6A, 0xD2);

/// Linear's urgent-priority amber, painted for the [`Priority::Urgent`] box.
/// Theme-independent, like the status colors it sits beside.
const PRIORITY_URGENT: egui::Color32 = egui::Color32::from_rgb(0xF2, 0x99, 0x4A);

/// A small rounded `done/total` pill — the shared visual behind the card
/// footer's `grid::subissue_progress_pill` and a collapsed graph node's subtree
/// progress. Returns the frame response so the caller can attach its own hover
/// text (the two read "subissues" vs "sub-issues in this branch").
pub(super) fn progress_pill(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    done: usize,
    total: usize,
) -> egui::Response {
    egui::Frame::new()
        .fill(theme.surface_secondary)
        .corner_radius(egui::CornerRadius::same(RADIUS_PILL as u8))
        .inner_margin(egui::Margin::symmetric(SPACING_SM as i8, 1))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(format!("{done}/{total}"))
                    .small()
                    .color(theme.text_muted),
            );
        })
        .response
}

/// A deterministic color for a label, derived from its text.
pub(super) fn label_color(label: &str) -> egui::Color32 {
    let mut h: usize = 0;
    for b in label.bytes() {
        h = h.wrapping_mul(31).wrapping_add(b as usize);
    }
    PALETTE[h % PALETTE.len()]
}

/// A small colored pill showing a label's text.
pub(super) fn label_chip(ui: &mut egui::Ui, theme: &ColorTheme, label: &str) {
    let color = label_color(label);
    egui::Frame::new()
        .fill(color.gamma_multiply(0.30))
        .corner_radius(egui::CornerRadius::same(RADIUS_PILL as u8))
        .inner_margin(egui::Margin::symmetric(SPACING_SM as i8, 1))
        .show(ui, |ui| {
            // Extend (don't wrap) so the chip reports its full natural width.
            // Otherwise, when the wrapping row runs out of horizontal space, the
            // text inside the last chip wraps character-by-character (vertical
            // `p/e/r/f`) instead of the whole chip moving to the next row.
            ui.add(
                egui::Label::new(egui::RichText::new(label).small().color(theme.text_primary))
                    .extend(),
            );
        });
}

/// A small rounded pill showing a count (e.g. cards in a column).
pub(super) fn count_badge(ui: &mut egui::Ui, theme: &ColorTheme, n: usize) {
    text_pill(ui, theme, &n.to_string());
}

/// A small rounded pill of muted text, e.g. the review queue's `3 / 12`.
pub(super) fn text_pill(ui: &mut egui::Ui, theme: &ColorTheme, text: &str) -> egui::Response {
    tinted_pill(ui, text, theme.text_muted)
}

/// Width a [`text_pill`] of `text` draws at, measured without drawing it: for a
/// row that has to know its fixed parts before it lays out the rest.
pub(super) fn pill_width(ui: &egui::Ui, text: &str) -> f32 {
    notedeck_ui::text_width(ui, text, &egui::TextStyle::Small) + 2.0 * SPACING_SM
}

/// A small rounded pill of `color` text, e.g. the review pane's warning-coloured
/// `patch truncated`: a [`tinted_control`] that only senses hover.
pub(super) fn tinted_pill(ui: &mut egui::Ui, text: &str, color: egui::Color32) -> egui::Response {
    tinted_control(
        ui,
        egui::RichText::new(text).small(),
        color,
        ControlSize::Pill,
        egui::Sense::hover(),
    )
}

/// How big a [`tinted_control`] draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ControlSize {
    /// Inline with text: [`PILL_PAD_Y`] above and below the text row,
    /// [`SPACING_SM`] either side, fully rounded.
    Pill,
    /// A pane's action: [`BUTTON_SM`] tall, [`SPACING_MD`] either side,
    /// [`RADIUS_MD`] corners.
    Button,
}

/// Fill strength of a [`tinted_control`], as a fraction of its text colour.
const TINT_IDLE: f32 = 0.18;
/// ...while the pointer is over a clickable one.
const TINT_HOVERED: f32 = 0.28;
/// ...while it's held down.
const TINT_PRESSED: f32 = 0.36;

/// Room above and below the text row in a [`ControlSize::Pill`]. The row
/// already carries the font's own ascent and descent, so a little is enough.
const PILL_PAD_Y: f32 = 3.0;

/// The one way headway draws a filled chip or button around a line of text —
/// the sha pill, the queue's `1 / 2`, count badges, "patch truncated", "±
/// Review diff". Two rules make them read as one family:
///
/// - **The fill is a tint of the text's own `color`**, never a neutral grey
///   under coloured text (which muddied it), so a control is one hue; a
///   clickable one deepens the tint on hover and press.
/// - **One geometry**: a pill or a button size, the text's row centred in it
///   and its advance (not its ink) setting the width, so two shas of the same
///   length make the same width of pill and the text after them lines up.
pub(super) fn tinted_control(
    ui: &mut egui::Ui,
    text: egui::RichText,
    color: egui::Color32,
    size: ControlSize,
    sense: egui::Sense,
) -> egui::Response {
    let galley = egui::WidgetText::from(text.color(color)).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Button,
    );
    let (pad_x, height, radius) = match size {
        ControlSize::Pill => (SPACING_SM, galley.size().y + 2.0 * PILL_PAD_Y, RADIUS_PILL),
        ControlSize::Button => (SPACING_MD, BUTTON_SM.max(galley.size().y), RADIUS_MD),
    };
    let desired = egui::vec2(galley.size().x + 2.0 * pad_x, height);
    let (rect, response) = ui.allocate_exact_size(desired, sense);
    let kind = if sense.senses_click() {
        egui::WidgetType::Button
    } else {
        egui::WidgetType::Label
    };
    response.widget_info(|| egui::WidgetInfo::labeled(kind, ui.is_enabled(), galley.text()));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let tint = if !sense.senses_click() {
        TINT_IDLE
    } else if response.is_pointer_button_down_on() {
        TINT_PRESSED
    } else if response.hovered() {
        TINT_HOVERED
    } else {
        TINT_IDLE
    };
    ui.painter().rect_filled(
        rect,
        egui::CornerRadius::same(radius as u8),
        color.gamma_multiply(tint),
    );
    let origin = egui::pos2(rect.min.x + pad_x, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(origin, galley, color);
    response
}

/// A one-line label of small text cut in its middle to fit a width —
/// `monad:/home/jb…/notedeck-headway` — with the full text on hover. It
/// re-elides only when the width it's given changes, so a steady frame formats
/// nothing.
#[derive(Default)]
pub(super) struct MiddleElided {
    full: String,
    /// The whole-pixel width [`shown`](Self::shown) was cut for.
    width: Option<f32>,
    shown: String,
}

impl MiddleElided {
    /// `full`, not yet elided: the first [`ui`](Self::ui) cuts it.
    pub(super) fn new(full: String) -> Self {
        Self {
            full,
            ..Default::default()
        }
    }

    /// Whether there's no text to show.
    pub(super) fn is_empty(&self) -> bool {
        self.full.is_empty()
    }

    /// Draw the text in small `color`, no wider than `max_width`.
    pub(super) fn ui(&mut self, ui: &mut egui::Ui, color: egui::Color32, max_width: f32) {
        let width = max_width.floor();
        if self.width != Some(width) {
            let font = egui::TextStyle::Small.resolve(ui.style());
            self.shown = ui.fonts(|fonts| {
                elide_middle(&self.full, width, |s| {
                    fonts
                        .layout_no_wrap(s.to_owned(), font.clone(), color)
                        .size()
                        .x
                })
            });
            self.width = Some(width);
        }
        let label = ui.label(
            egui::RichText::new(self.shown.as_str())
                .small()
                .color(color),
        );
        if self.shown != self.full {
            label.on_hover_text(self.full.as_str());
        }
    }
}

/// `text` as it is if `measure` says it fits in `max_width`, else cut in the
/// middle to the most characters that do fit around a `…`. The tail keeps
/// twice the head's share, since the end of a path names the repo.
pub(super) fn elide_middle(text: &str, max_width: f32, measure: impl Fn(&str) -> f32) -> String {
    if text.is_empty() || measure(text) <= max_width {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let cut = |keep: usize| -> String {
        let tail = keep - keep / 3;
        let head = keep - tail;
        chars[..head]
            .iter()
            .chain(std::iter::once(&'…'))
            .chain(&chars[chars.len() - tail..])
            .collect()
    };
    // The largest `keep` that fits; zero (a lone `…`) when nothing does.
    let (mut lo, mut hi) = (0, chars.len() - 1);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if measure(&cut(mid)) <= max_width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    cut(lo)
}

/// A small muted, sentence-case group heading (Linear-style) used for the
/// detail sheet's sidebar panels.
pub(super) fn section_label(ui: &mut egui::Ui, theme: &ColorTheme, text: &str) {
    ui.label(egui::RichText::new(text).small().color(theme.text_muted));
}

/// A semibold sentence-case heading for the detail pane's main-column sections
/// ("Sub-issues", "Activity"), matching Linear's typography rather than the
/// muted all-caps sidebar labels.
pub(super) fn detail_heading(ui: &mut egui::Ui, theme: &ColorTheme, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(14.0)
            .strong()
            .color(theme.text_primary),
    );
}

/// A secondary button for a pane's action — "± Review diff" in the detail's
/// Review section, "☍ View dependency graph" under an epic's sub-issues — so
/// both read as the same kind of button: an accent [`tinted_control`] (one
/// leading glyph, one space, the label), [`BUTTON_SM`] tall.
///
/// The leading glyph must be one the loaded fonts carry (`tests/glyphs.rs`
/// checks every non-ASCII character in headway's string literals); "⧉", the
/// original icon, is in none of them and rendered as a box.
pub(super) fn secondary_action_button(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    text: &str,
) -> egui::Response {
    tinted_control(
        ui,
        egui::RichText::new(text),
        theme.accent,
        ControlSize::Button,
        egui::Sense::click(),
    )
}

/// Which Linear-style status circle to paint for a column or subissue.
#[derive(Clone, Copy)]
pub(super) enum StatusIcon {
    /// The first column: a dashed muted ring.
    Backlog,
    /// The second column: a plain muted ring.
    Todo,
    /// A middle column: a ring with a pie-slice fill of this fraction —
    /// yellow through the first half of the middle columns, green after.
    Started(f32),
    /// The final column (or an archived child): a filled indigo disc with a
    /// check.
    Done,
}

impl StatusIcon {
    /// The icon for the column at `idx` on a board of `n` columns, mapped
    /// positionally the way Linear maps status types: first = backlog,
    /// second = todo, last = done, and anything between = started, filled in
    /// proportion to how far along the middle it sits.
    pub(super) fn for_column(idx: usize, n: usize) -> Self {
        if idx + 1 >= n {
            Self::Done
        } else if idx == 0 {
            Self::Backlog
        } else if idx == 1 {
            Self::Todo
        } else {
            Self::Started((idx - 1) as f32 / (n - 2) as f32)
        }
    }

    /// The circle's stroke/fill color: muted for unstarted, Linear's yellow →
    /// green through the middle, indigo for done.
    fn color(self, theme: &ColorTheme) -> egui::Color32 {
        match self {
            Self::Backlog | Self::Todo => theme.text_muted,
            Self::Started(f) if f <= 0.5 => STATUS_STARTED_EARLY,
            Self::Started(_) => STATUS_STARTED_LATE,
            Self::Done => STATUS_DONE,
        }
    }
}

/// Paint one Linear-style status circle, `size` px square, and return its
/// response (hover only; callers wrap it when a click target is needed).
pub(super) fn status_icon_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    icon: StatusIcon,
    size: f32,
) -> egui::Response {
    use std::f32::consts::TAU;

    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let painter = ui.painter();
    let center = rect.center();
    let r = size * 0.5 - 1.0;
    let stroke = egui::Stroke::new(1.5_f32, icon.color(theme));

    // Sample an arc of the ring into line points. Twelve o'clock start,
    // clockwise, matching Linear's pies.
    let arc = |from: f32, to: f32| -> Vec<egui::Pos2> {
        let steps = 16;
        (0..=steps)
            .map(|k| {
                let a = -TAU / 4.0 + from + (to - from) * k as f32 / steps as f32;
                center + r * egui::vec2(a.cos(), a.sin())
            })
            .collect()
    };

    match icon {
        StatusIcon::Backlog => {
            // A dashed ring: six dashes with even gaps.
            for k in 0..6 {
                let a0 = k as f32 / 6.0 * TAU;
                painter.add(egui::Shape::line(arc(a0, a0 + TAU / 10.0), stroke));
            }
        }
        StatusIcon::Todo => {
            painter.circle_stroke(center, r, stroke);
        }
        StatusIcon::Started(f) => {
            painter.circle_stroke(center, r, stroke);
            // The pie wedge: centre plus an inset arc of the fill fraction.
            let inset = r - 2.5;
            let mut points = vec![center];
            points.extend(
                arc(0.0, TAU * f.clamp(0.0, 1.0))
                    .into_iter()
                    .map(|p| center + (p - center) * (inset / r)),
            );
            painter.add(egui::Shape::convex_polygon(
                points,
                stroke.color,
                egui::Stroke::NONE,
            ));
        }
        StatusIcon::Done => {
            painter.circle_filled(center, r + 0.5, stroke.color);
            // The check, drawn as two strokes on the disc.
            let p = |dx: f32, dy: f32| center + r * egui::vec2(dx, dy);
            let check = egui::Stroke::new(1.5_f32, egui::Color32::WHITE);
            painter.line_segment([p(-0.45, 0.05), p(-0.1, 0.4)], check);
            painter.line_segment([p(-0.1, 0.4), p(0.5, -0.3)], check);
        }
    }
    resp
}

/// The human label for a priority, matching Linear's menu wording
/// ("No priority" for the unset default).
pub(super) fn priority_label(priority: Priority) -> &'static str {
    match priority {
        Priority::None => "No priority",
        Priority::Low => "Low",
        Priority::Medium => "Medium",
        Priority::High => "High",
        Priority::Urgent => "Urgent",
    }
}

/// Paint a Linear-style priority icon, `size` px square: three ascending signal
/// bars filled up to the level (one for Low … three for High), or an amber box
/// with a white "!" for Urgent. [`Priority::None`] paints three idle bars. The
/// board row omits the icon entirely for `None` (see `grid::card_ui`); the detail
/// menu shows it so "No priority" has a glyph. Returns its (hover) response.
pub(super) fn priority_icon_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    priority: Priority,
    size: f32,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let painter = ui.painter();

    if priority == Priority::Urgent {
        // A filled amber square with a white exclamation, like Linear's urgent.
        let box_rect = rect.shrink(size * 0.08);
        painter.rect_filled(
            box_rect,
            egui::CornerRadius::same((size * 0.2) as u8),
            PRIORITY_URGENT,
        );
        let cx = box_rect.center().x;
        let stem = egui::Stroke::new(size * 0.11, egui::Color32::WHITE);
        painter.line_segment(
            [
                egui::pos2(cx, box_rect.top() + size * 0.22),
                egui::pos2(cx, box_rect.bottom() - size * 0.32),
            ],
            stem,
        );
        painter.circle_filled(
            egui::pos2(cx, box_rect.bottom() - size * 0.18),
            size * 0.06,
            egui::Color32::WHITE,
        );
        return resp;
    }

    // Three bottom-aligned bars of ascending height; the first `filled` are lit,
    // the rest sit idle so the icon reads the same shape at every level.
    let filled = match priority {
        Priority::Low => 1,
        Priority::Medium => 2,
        Priority::High => 3,
        _ => 0,
    };
    let active = theme.text_muted;
    let idle = theme.text_muted.gamma_multiply(0.3);
    let bar_w = size * 0.2;
    let gap = (size - 3.0 * bar_w) / 2.0;
    for i in 0..3 {
        let h = size * (0.4 + 0.3 * i as f32);
        let x = rect.left() + i as f32 * (bar_w + gap);
        let bar = egui::Rect::from_min_max(
            egui::pos2(x, rect.bottom() - h),
            egui::pos2(x + bar_w, rect.bottom()),
        );
        let color = if i < filled { active } else { idle };
        painter.rect_filled(bar, egui::CornerRadius::same(1), color);
    }
    resp
}

/// A `nostr:nevent…` URI for an issue card, ready to paste into a notebook note
/// (or anywhere else that resolves nostr refs). Carries the issue kind as a hint.
pub(super) fn issue_nostr_uri(card_id: &NoteId) -> Option<String> {
    use nostr::nips::nip19::ToBech32;
    let event_id = nostr::EventId::from_slice(card_id.bytes()).ok()?;
    let nevent = nostr::nips::nip19::Nip19Event::new(event_id, Vec::<String>::new())
        .kind(nostr::Kind::from(event::KIND_ISSUE as u16));
    Some(format!("nostr:{}", nevent.to_bech32().ok()?))
}

/// A `nostr:naddr…` URI for a board, addressing the replaceable board event by
/// its `(kind, author, identifier)` coordinate.
pub(super) fn board_nostr_uri(author: &[u8; 32], board_id: &str) -> Option<String> {
    use nostr::nips::nip19::ToBech32;
    let pubkey = nostr::PublicKey::from_slice(author).ok()?;
    let mut coord =
        nostr::nips::nip01::Coordinate::new(nostr::Kind::from(event::KIND_BOARD as u16), pubkey);
    coord.identifier = board_id.to_string();
    Some(format!("nostr:{}", coord.to_bech32().ok()?))
}

/// Render a compact, read-only headway card frame: an optional label row, a
/// title, and a one-line body preview. Shared by [`issue_inline_ui`](super::issue_inline_ui) (the
/// creation-time snapshot) and [`card_inline_ui`](super::card_inline_ui) (the folded current state).
pub(super) fn card_frame_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    labels: &[String],
    title: &str,
    body: &str,
) -> egui::Response {
    egui::Frame::new()
        .fill(theme.surface_elevated)
        .corner_radius(egui::CornerRadius::same(RADIUS_MD as u8))
        .stroke(egui::Stroke::new(STROKE_THIN, theme.border_default))
        .inner_margin(egui::Margin::same(SPACING_SM as i8))
        .show(ui, |ui| {
            // The notebook lays node content out centered (egui's `Ui::put`);
            // force left alignment so the card reads like a card, not centered.
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(ui.available_width());
                if !labels.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        for label in labels {
                            label_chip(ui, theme, label);
                        }
                    });
                    ui.add_space(SPACING_XS);
                }
                ui.label(egui::RichText::new(title).color(theme.text_primary));
                if !body.is_empty() {
                    ui.add_space(2.0);
                    ui.add(
                        egui::Label::new(egui::RichText::new(body).small().color(theme.text_muted))
                            .truncate(),
                    );
                }
            });
        })
        .response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One unit of width per character.
    fn chars(s: &str) -> f32 {
        s.chars().count() as f32
    }

    /// Text that fits comes back whole; longer text keeps as many characters
    /// as fit around the `…`, two thirds of them from the end; a width too
    /// small for anything leaves only the `…`.
    #[test]
    fn elide_middle_cuts_to_fit_keeping_the_tail() {
        let path = "monad:/home/jb55/dev/notedeck-headway";
        assert_eq!(elide_middle(path, 100.0, chars), path);
        assert_eq!(elide_middle(path, chars(path), chars), path);

        let cut = elide_middle(path, 16.0, chars);
        assert_eq!(cut, "monad…ck-headway");
        assert_eq!(chars(&cut), 16.0);

        assert_eq!(elide_middle(path, 0.0, chars), "…");
        assert_eq!(elide_middle("", -1.0, chars), "");
    }
}
