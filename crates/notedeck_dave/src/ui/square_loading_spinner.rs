use std::time::Duration;

use egui::{vec2, Color32, Response, Sense, Ui, Widget};

/// A loading spinner made of a row of squares with a brightness wave moving
/// across them, à la the Claude Code spinner.
///
/// Unlike [`egui::Spinner`], which calls `request_repaint()` every frame and so
/// forces the whole app to re-render continuously, this spinner advances at a
/// fixed interval (500ms by default) using `request_repaint_after`. That keeps
/// an otherwise idle egui app from spinning the CPU just to animate it.
#[must_use = "You should put this widget in an ui with `ui.add(widget);`"]
pub struct SquareLoadingSpinner {
    /// Height/width of each square, in points.
    square_size: f32,
    /// Gap between squares, in points.
    gap: f32,
    /// Number of squares in the row.
    count: usize,
    /// How long each animation frame lasts.
    interval: Duration,
    color: Option<Color32>,
}

impl Default for SquareLoadingSpinner {
    fn default() -> Self {
        Self {
            square_size: 4.0,
            gap: 2.0,
            count: 4,
            interval: Duration::from_millis(500),
            color: None,
        }
    }
}

impl SquareLoadingSpinner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Scale the spinner to roughly fit the given height, the way
    /// [`egui::Spinner::size`] does. Keeps the square/gap proportions.
    pub fn size(mut self, size: f32) -> Self {
        self.square_size = (size * 0.35).max(2.0);
        self.gap = (size * 0.18).max(1.0);
        self
    }

    pub fn color(mut self, color: impl Into<Color32>) -> Self {
        self.color = Some(color.into());
        self
    }

    /// How long each animation frame lasts (default 500ms).
    pub fn interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }
}

impl Widget for SquareLoadingSpinner {
    fn ui(self, ui: &mut Ui) -> Response {
        let width = self.count as f32 * self.square_size + (self.count - 1) as f32 * self.gap;
        let height = ui.style().spacing.interact_size.y.max(self.square_size);
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());

        if ui.is_rect_visible(rect) {
            // Advance one frame per `interval` rather than every render, and ask
            // egui to wake us only when the next frame is due.
            let interval_secs = self.interval.as_secs_f64().max(0.001);
            let time = ui.input(|i| i.time);
            let frame = (time / interval_secs) as usize;

            // Schedule the next repaint at the frame boundary so we don't
            // continuously re-render while idle.
            let elapsed_in_frame = time.rem_euclid(interval_secs);
            let until_next = (interval_secs - elapsed_in_frame).max(0.0);
            ui.ctx()
                .request_repaint_after(Duration::from_secs_f64(until_next));

            let color = self
                .color
                .unwrap_or_else(|| ui.visuals().strong_text_color());
            // The wave position cycles through the squares with a short pause.
            let cycle = self.count + 1;
            let active = frame % cycle;

            let painter = ui.painter();
            let top = rect.center().y - self.square_size / 2.0;
            for i in 0..self.count {
                let x = rect.left() + i as f32 * (self.square_size + self.gap);
                let square = egui::Rect::from_min_size(
                    egui::pos2(x, top),
                    egui::vec2(self.square_size, self.square_size),
                );
                // Light up the active square plus a dimmer trailing neighbour.
                let alpha = if i == active {
                    1.0
                } else if active > 0 && i + 1 == active {
                    0.45
                } else {
                    0.18
                };
                painter.rect_filled(square, 1.0, color.gamma_multiply(alpha));
            }
        }

        response
    }
}
