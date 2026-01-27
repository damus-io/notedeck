use egui::{Pos2, Rect, Response, Sense, Ui, Vec2};

/// A visual keybinding hint - a small framed box with a letter or number inside.
/// Used to indicate keyboard shortcuts in the UI.
pub struct KeybindHint<'a> {
    text: &'a str,
    size: f32,
}

impl<'a> KeybindHint<'a> {
    /// Create a new keybinding hint with the given text
    pub fn new(text: &'a str) -> Self {
        Self { text, size: 18.0 }
    }

    /// Set the size of the hint box (default: 18.0)
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    /// Show the keybinding hint and return the response
    pub fn show(self, ui: &mut Ui) -> Response {
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(self.size), Sense::hover());
        self.paint(ui, rect);
        response
    }

    /// Paint the keybinding hint at a specific position (for use with painters)
    pub fn paint_at(self, ui: &Ui, center: Pos2) {
        let rect = Rect::from_center_size(center, Vec2::splat(self.size));
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
            egui::Stroke::new(1.0, stroke_color.gamma_multiply(0.6)),
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
