//! The HUD tooltip: a dark translucent panel with a thin bright border, a
//! gold title, white body text, and right-aligned values colored by
//! [`Rarity`].
//!
//! It comes in two halves so it can sit in any hover container:
//! [`tooltip_frame`] is the panel, and [`HudTooltip`] (via [`tooltip_ui`]) is
//! what goes in it. [`show_tooltip`] puts both on a widget's hover, in place
//! of egui's `on_hover_ui`. A `notedeck_ui::charts` time-series chart takes
//! the frame through `TimeSeriesStyle::tooltip_frame` and the contents through
//! its tooltip closure.

use std::fmt::Display;

use egui::{Frame, Response, RichText, Stroke, Ui};

use super::Rarity;
use crate::ui::palette::{HudPalette, Palette};

/// The widest a HUD tooltip grows. Its right-aligned values push it out to
/// the width of the container it is in, so outside an auto-sized popup this
/// is the width it takes.
pub const TOOLTIP_MAX_WIDTH: f32 = 280.0;

/// The tooltip panel: [`HudPalette::panel`] glass inside a
/// [`HudPalette::border`] hairline, with rounded corners and a soft shadow.
pub fn tooltip_frame(hud: &HudPalette) -> Frame {
    Frame::new()
        .fill(hud.panel)
        .stroke(Stroke::new(notedeck::tokens::STROKE_THIN, hud.border))
        .corner_radius(notedeck::tokens::RADIUS_SM)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .shadow(egui::Shadow {
            offset: [0, 2],
            blur: 8,
            spread: 0,
            color: egui::Color32::from_black_alpha(96),
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
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 2.0);
        let mut tooltip = HudTooltip {
            ui,
            hud: &palette.hud,
        };
        add_contents(&mut tooltip);
    });
}

/// What goes in a HUD tooltip, one line at a time.
///
/// Every line takes its text as `impl Display`, so a `&str` or a
/// `format_args!` goes straight into the one `String` egui lays out.
pub struct HudTooltip<'a> {
    ui: &'a mut Ui,
    hud: &'a HudPalette,
}

impl HudTooltip<'_> {
    /// The gold title line.
    pub fn title(&mut self, text: impl Display) -> &mut Self {
        let text = RichText::new(text.to_string())
            .strong()
            .size(15.0)
            .color(self.hud.title);
        self.ui.label(text);
        self
    }

    /// A line of white body text, wrapped to the tooltip's width.
    pub fn text(&mut self, text: impl Display) -> &mut Self {
        self.ui
            .label(RichText::new(text.to_string()).color(self.hud.text));
        self
    }

    /// A muted caption, for where a number came from.
    pub fn note(&mut self, text: impl Display) -> &mut Self {
        let text = RichText::new(text.to_string())
            .small()
            .color(self.hud.text_muted);
        self.ui.label(text);
        self
    }

    /// A whole line in `rarity`'s color.
    pub fn line(&mut self, text: impl Display, rarity: Rarity) -> &mut Self {
        let text = RichText::new(text.to_string()).color(rarity.color(self.hud));
        self.ui.label(text);
        self
    }

    /// `label` on the left in body text, `value` right-aligned in
    /// `rarity`'s color.
    pub fn value(&mut self, label: impl Display, value: impl Display, rarity: Rarity) -> &mut Self {
        let hud = self.hud;
        egui::Sides::new().spacing(16.0).show(
            self.ui,
            |ui| ui.label(RichText::new(label.to_string()).color(hud.text)),
            |ui| ui.label(RichText::new(value.to_string()).color(rarity.color(hud))),
        );
        self
    }

    /// A hairline between groups of lines.
    pub fn separator(&mut self) -> &mut Self {
        let stroke = Stroke::new(1.0, self.hud.border.gamma_multiply(0.5));
        self.ui.visuals_mut().widgets.noninteractive.bg_stroke = stroke;
        self.ui.add(egui::Separator::default().spacing(8.0));
        self
    }
}
