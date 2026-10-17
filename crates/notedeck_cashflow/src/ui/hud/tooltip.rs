//! The HUD tooltip: a dark glass panel in a gilt hairline, a bold gold
//! title over a rule that fades out, white body text, and right-aligned
//! values in the medium face colored by [`Rarity`].
//!
//! It comes in two halves so it can sit in any hover container:
//! [`tooltip_frame`] is the panel, and [`HudTooltip`] (via [`tooltip_ui`]) is
//! what goes in it. [`show_tooltip`] puts both on a widget's hover, in place
//! of egui's `on_hover_ui`. A `notedeck_ui::charts` time-series chart takes
//! the frame through `TimeSeriesStyle::tooltip_frame` and the contents through
//! its tooltip closure.

use std::fmt::Display;

use egui::{FontId, Frame, Response, RichText, Stroke, TextStyle, Ui, vec2};

use super::Rarity;
use super::paint::{Fade, fade_rule};
use crate::ui::palette::{HudPalette, Palette};

/// The widest a HUD tooltip grows. Its right-aligned values push it out to
/// the width of the container it is in, so outside an auto-sized popup this
/// is the width it takes.
pub const TOOLTIP_MAX_WIDTH: f32 = 280.0;

/// The title's size: a clear step above the 14pt body.
const TITLE_SIZE: f32 = 18.0;

/// The most rules (the title's and separators) one tooltip draws. Rules wait
/// in a fixed array until the tooltip's width is known, so a tooltip with
/// more than this drops the extras rather than allocating.
const MAX_RULES: usize = 4;

/// The tooltip panel: [`HudPalette::panel`] glass inside a
/// [`HudPalette::border`] gilt hairline, with a deep shadow.
pub fn tooltip_frame(hud: &HudPalette) -> Frame {
    Frame::new()
        .fill(hud.panel)
        .stroke(Stroke::new(notedeck::tokens::STROKE_THIN, hud.border))
        .corner_radius(3)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .shadow(egui::Shadow {
            offset: [0, 4],
            blur: 14,
            spread: 0,
            color: hud.shade,
        })
}

/// Shows a HUD tooltip on `response` while it is hovered, in place of
/// `response.on_hover_ui`. Returns where it was drawn, if it was.
pub fn show_tooltip(
    response: &Response,
    palette: &Palette,
    add_contents: impl FnOnce(&mut HudTooltip<'_>),
) -> Option<egui::Rect> {
    let mut tooltip = egui::Tooltip::for_enabled(response);
    tooltip.popup = tooltip.popup.frame(tooltip_frame(&palette.hud));
    tooltip
        .show(|ui| tooltip_ui(ui, palette, add_contents))
        .map(|shown| shown.response.rect)
}

/// Lays out HUD tooltip contents in `ui`, which should already be inside a
/// [`tooltip_frame`]. The lines stack top to bottom whatever `ui`'s own
/// layout is.
pub fn tooltip_ui(ui: &mut Ui, palette: &Palette, add_contents: impl FnOnce(&mut HudTooltip<'_>)) {
    ui.vertical(|ui| {
        ui.set_max_width(TOOLTIP_MAX_WIDTH);
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 3.0);
        let mut tooltip = HudTooltip {
            ui,
            palette,
            rules: [None; MAX_RULES],
        };
        add_contents(&mut tooltip);
        tooltip.paint_rules();
    });
}

/// A rule a tooltip draws once it knows how wide it is.
#[derive(Debug, Clone, Copy)]
struct Rule {
    /// Where it runs.
    y: f32,
    /// Under the title, or between groups of lines.
    kind: RuleKind,
}

/// The two rules a tooltip draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleKind {
    /// Gold, fading out to the right, under the title.
    Title,
    /// Gilt, fading out at both ends, between groups of lines.
    Separator,
}

/// What goes in a HUD tooltip, one line at a time.
///
/// Every line takes its text as `impl Display`, so a `&str` or a
/// `format_args!` goes straight into the one `String` egui lays out.
pub struct HudTooltip<'a> {
    ui: &'a mut Ui,
    palette: &'a Palette,
    /// Rules waiting for the tooltip's final width.
    rules: [Option<Rule>; MAX_RULES],
}

impl HudTooltip<'_> {
    /// The bold gold title line, with a gold rule under it.
    pub fn title(&mut self, text: impl Display) -> &mut Self {
        let font = FontId::new(TITLE_SIZE, self.palette.faces.bold.clone());
        let text = RichText::new(text.to_string())
            .font(font)
            .color(self.palette.hud.title);
        self.ui.label(text);
        self.rule(RuleKind::Title, 5.0);
        self
    }

    /// A line of white body text, wrapped to the tooltip's width.
    pub fn text(&mut self, text: impl Display) -> &mut Self {
        let text = RichText::new(text.to_string()).color(self.palette.hud.text);
        self.ui.label(text);
        self
    }

    /// A muted caption, for where a number came from.
    pub fn note(&mut self, text: impl Display) -> &mut Self {
        let text = RichText::new(text.to_string())
            .small()
            .color(self.palette.hud.text_muted);
        self.ui.label(text);
        self
    }

    /// A whole line in `rarity`'s color.
    pub fn line(&mut self, text: impl Display, rarity: Rarity) -> &mut Self {
        let text = RichText::new(text.to_string()).color(rarity.color(&self.palette.hud));
        self.ui.label(text);
        self
    }

    /// `label` on the left in body text, `value` right-aligned in the medium
    /// face, in `rarity`'s color.
    pub fn value(&mut self, label: impl Display, value: impl Display, rarity: Rarity) -> &mut Self {
        let palette = self.palette;
        let hud = &palette.hud;
        egui::Sides::new().spacing(16.0).show(
            self.ui,
            |ui| ui.label(RichText::new(label.to_string()).color(hud.text)),
            |ui| {
                let size = TextStyle::Body.resolve(ui.style()).size;
                let font = FontId::new(size, palette.faces.medium.clone());
                let value = RichText::new(value.to_string()).font(font);
                ui.label(value.color(rarity.color(hud)))
            },
        );
        self
    }

    /// A gilt rule between groups of lines.
    pub fn separator(&mut self) -> &mut Self {
        self.rule(RuleKind::Separator, 9.0)
    }

    /// Leaves `height` of space and marks a `kind` rule across its middle.
    fn rule(&mut self, kind: RuleKind, height: f32) -> &mut Self {
        let (rect, _) = self
            .ui
            .allocate_exact_size(vec2(0.0, height), egui::Sense::hover());
        let y = rect.center().y.round();
        if let Some(slot) = self.rules.iter_mut().find(|r| r.is_none()) {
            *slot = Some(Rule { y, kind });
        }
        self
    }

    /// Draws the rules across the width the tooltip ended up.
    fn paint_rules(&self) {
        let hud = &self.palette.hud;
        let x = self.ui.min_rect().x_range();
        let painter = self.ui.painter();
        for rule in self.rules.iter().flatten() {
            match rule.kind {
                RuleKind::Title => fade_rule(
                    painter,
                    x,
                    rule.y - 2.0,
                    hud.title.gamma_multiply(0.7),
                    Fade::Out,
                ),
                RuleKind::Separator => fade_rule(painter, x, rule.y, hud.border, Fade::Both),
            }
        }
    }
}
