//! Keycap widget: a small framed box showing a keyboard shortcut.

use egui::{Pos2, Rect, Response, Sense, Ui, Vec2};

/// A visual keybinding hint - a small framed box with a letter or number inside.
/// Used to indicate keyboard shortcuts in the UI.
pub struct KeybindHint<'a> {
    text: &'a str,
    size: f32,
    width: Option<f32>,
}

impl<'a> KeybindHint<'a> {
    /// Create a new keybinding hint with the given text
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            size: 18.0,
            width: None,
        }
    }

    /// Set the size (height) of the hint box (default: 18.0)
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    /// Override the width (default: same as size, i.e. square)
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// Show the keybinding hint and return the response
    pub fn show(self, ui: &mut Ui) -> Response {
        let w = self.width.unwrap_or(self.size);
        let (rect, response) = ui.allocate_exact_size(Vec2::new(w, self.size), Sense::hover());
        self.paint(ui, rect);
        response
    }

    /// Paint the keybinding hint at a specific position (for use with painters)
    pub fn paint_at(self, ui: &Ui, center: Pos2) {
        let w = self.width.unwrap_or(self.size);
        let rect = Rect::from_center_size(center, Vec2::new(w, self.size));
        self.paint(ui, rect);
    }

    fn paint(self, ui: &Ui, rect: Rect) {
        let painter = ui.painter();
        let visuals = ui.visuals();

        // Frame/border
        let stroke_color = visuals.widgets.noninteractive.fg_stroke.color;
        let bg_color = visuals.widgets.noninteractive.bg_fill;
        let corner_radius = 3.0;

        // Background fill
        painter.rect_filled(rect, corner_radius, bg_color);

        // Border stroke
        painter.rect_stroke(
            rect,
            corner_radius,
            egui::Stroke::new(
                notedeck::tokens::STROKE_THIN,
                stroke_color.gamma_multiply(0.6),
            ),
            egui::StrokeKind::Inside,
        );

        // Text in center (slight vertical nudge for better optical centering)
        let font_size = self.size * 0.65;
        let text_pos = rect.center() + Vec2::new(0.0, 2.0);
        painter.text(
            text_pos,
            egui::Align2::CENTER_CENTER,
            self.text,
            egui::FontId::monospace(font_size),
            visuals.text_color(),
        );
    }
}

/// Draw a keybinding hint inline (for use in horizontal layouts)
pub fn keybind_hint(ui: &mut Ui, text: &str) -> Response {
    KeybindHint::new(text).show(ui)
}

/// Draw a keybinding hint at a specific position using the painter
pub fn paint_keybind_hint(ui: &Ui, center: Pos2, text: &str, size: f32) {
    KeybindHint::new(text).size(size).paint_at(ui, center);
}

/// Height of a keycap in a which-key strip ([`key_hints_ui`]).
pub const KEYCAP: f32 = 18.0;

/// Extra keycap width per character past the first, so `gg` and `esc` fit.
pub const KEYCAP_PER_CHAR: f32 = 8.0;

/// Width of the keycap for `key`: [`KEYCAP`] square, widened by
/// [`KEYCAP_PER_CHAR`] for each character past the first.
pub fn keycap_width(key: &str) -> f32 {
    let extra_chars = key.chars().count().saturating_sub(1) as f32;
    KEYCAP + extra_chars * KEYCAP_PER_CHAR
}

/// Width a hint takes in a row: its `keys`' keycaps and a label `label_width`
/// wide, with an `item_gap` after each keycap. The measure [`key_hints_ui`]
/// wraps on; it lays the hint out from [`keycap_width`] too, so the two agree.
fn hint_width<'a>(keys: impl IntoIterator<Item = &'a str>, label_width: f32, item_gap: f32) -> f32 {
    keys.into_iter()
        .map(|key| keycap_width(key) + item_gap)
        .sum::<f32>()
        + label_width
}

/// Draw a which-key strip: rows of hints, each a run of keycaps followed by its
/// label in `label_color`, [`SPACING_MD`](notedeck::tokens::SPACING_MD) apart.
/// A hint is a `(keycaps, label)` pair; keys that share a job share a hint
/// (`j k  up/down`).
///
/// A hint is measured before it is placed and starts a new row if it won't fit
/// what's left of this one, so a narrow pane wraps whole hints and never splits
/// a keycap from its label. (egui can't do this by itself: it places a nested
/// `horizontal` before knowing its width, so the hint would run off the right
/// edge instead.) Walks `hints` once, lays each label out once, and collects
/// nothing, so the caller can hand it a lazy iterator over its own tables.
pub fn key_hints_ui<'a, K>(
    ui: &mut Ui,
    label_color: egui::Color32,
    hints: impl IntoIterator<Item = (K, &'a str)>,
) where
    K: IntoIterator<Item = &'a str> + Clone,
{
    let gap = notedeck::tokens::SPACING_MD;
    ui.horizontal_wrapped(|ui| {
        let item_gap = ui.spacing().item_spacing.x;
        for (i, (keys, label)) in hints.into_iter().enumerate() {
            let label = egui::WidgetText::from(egui::RichText::new(label).color(label_color))
                .into_galley(
                    ui,
                    Some(egui::TextWrapMode::Extend),
                    f32::INFINITY,
                    egui::TextStyle::Body,
                );
            let width = hint_width(keys.clone(), label.size().x, item_gap);
            if i > 0 {
                if gap + item_gap + width > ui.available_size_before_wrap().x {
                    ui.end_row();
                } else {
                    ui.add_space(gap);
                }
            }
            ui.horizontal(|ui| {
                for key in keys {
                    KeybindHint::new(key)
                        .size(KEYCAP)
                        .width(keycap_width(key))
                        .show(ui);
                }
                ui.label(label);
            });
        }
    });
}

/// Draw a which-key strip ([`key_hints_ui`]) floating along the bottom of
/// `rect`, over whatever is drawn there, so that showing or hiding it never
/// moves what's beneath. `hints` is `None` while it's hidden.
///
/// It sits on its own foreground [`egui::Area`] the size of `rect`, which is
/// click-through (`interactable(false)`): the pointer reaches what's beneath
/// as if the strip weren't there. Its fill is the panel's, so keycaps stay
/// readable over a busy view, with a separator along its top edge so it reads
/// as floating. The strip is laid out once out of sight to learn its height,
/// then again at the bottom, so it is where it belongs on the frame it first
/// shows, rather than a frame later as a self-sizing area would be.
///
/// egui spends an area's first-ever show measuring it invisibly. While the
/// strip is hidden this shows the empty area that once, so the strip's first
/// appearance isn't that invisible pass; after that, hidden, it does nothing
/// and allocates nothing.
pub fn key_hints_overlay<'a, K, I>(
    ctx: &egui::Context,
    id: egui::Id,
    rect: Rect,
    label_color: egui::Color32,
    hints: Option<I>,
) where
    K: IntoIterator<Item = &'a str> + Clone,
    I: Iterator<Item = (K, &'a str)> + Clone,
{
    if hints.is_none() && egui::AreaState::load(ctx, id).is_some() {
        return;
    }
    egui::Area::new(id)
        .order(egui::Order::Foreground)
        .fixed_pos(rect.min)
        .constrain_to(rect)
        .interactable(false)
        .fade_in(false)
        .show(ctx, |ui| {
            ui.set_min_size(rect.size());
            let Some(hints) = hints else {
                return;
            };
            let margin = notedeck::tokens::SPACING_MD as i8;
            let frame = egui::Frame::new()
                .fill(ui.visuals().panel_fill)
                .inner_margin(egui::Margin::same(margin));
            let strip = |ui: &mut Ui, hints: I| {
                frame
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        key_hints_ui(ui, label_color, hints);
                    })
                    .response
                    .rect
            };

            let mut measure = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt("measure")
                    .max_rect(rect)
                    .invisible(),
            );
            let height = strip(&mut measure, hints.clone()).height();

            let bottom =
                Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - height), rect.max);
            let mut shown = ui.new_child(egui::UiBuilder::new().id_salt("strip").max_rect(bottom));
            let drawn = strip(&mut shown, hints);
            let edge = ui.visuals().widgets.noninteractive.bg_stroke;
            ui.painter().hline(drawn.x_range(), drawn.top(), edge);
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::{NodeT, Queryable};
    use egui_kittest::Harness;

    /// Hints wide enough together that a narrow strip can't hold them on one row.
    const HINTS: &[(&[&str], &str)] = &[
        (&["j", "k"], "up/down"),
        (&["h", "l"], "columns"),
        (&["gg", "G"], "first/last"),
        (&["esc"], "clear"),
    ];

    /// A hint label's box: its left, top and right edges.
    #[derive(Debug)]
    struct LabelBox {
        left: f64,
        top: f64,
        right: f64,
    }

    /// Draw [`HINTS`] in a strip `width` wide and return, per label, its box's
    /// left, top and right edges.
    fn label_boxes(width: f32) -> Vec<LabelBox> {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(width, 200.0))
            .build_ui(|ui| {
                let hints = HINTS
                    .iter()
                    .map(|(keys, label)| (keys.iter().copied(), *label));
                key_hints_ui(ui, egui::Color32::GRAY, hints);
            });
        harness.run();
        HINTS
            .iter()
            .map(|(_, label)| {
                let bb = harness
                    .get_by_label(label)
                    .accesskit_node()
                    .bounding_box()
                    .expect("a hint label has a box");
                LabelBox {
                    left: bb.x0,
                    top: bb.y0,
                    right: bb.x1,
                }
            })
            .collect()
    }

    #[test]
    fn a_wide_strip_is_one_row() {
        let boxes = label_boxes(1000.0);
        assert!(boxes.windows(2).all(|w| w[0].top == w[1].top), "{boxes:?}");
    }

    /// Ported from headway's `review_queue_key_hints_wrap_on_a_narrow_pane`:
    /// on a narrow strip whole hints wrap onto new rows, and every label still
    /// ends inside the strip.
    #[test]
    fn a_narrow_strip_wraps_whole_hints() {
        let width = 160.0;
        let boxes = label_boxes(width);
        assert!(
            boxes.windows(2).any(|w| w[1].top > w[0].top),
            "wrapped: {boxes:?}"
        );
        for b in &boxes {
            assert!(b.right <= width as f64, "{boxes:?} runs past {width}");
            assert!(b.left >= 0.0);
        }
    }
}
