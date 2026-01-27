use egui::{Color32, Response, Ui, Vec2};

/// Badge variants that determine the color scheme
#[derive(Clone, Copy, Default)]
#[allow(dead_code)]
pub enum BadgeVariant {
    /// Default muted style
    #[default]
    Default,
    /// Informational blue
    Info,
    /// Success green
    Success,
    /// Warning amber/yellow
    Warning,
    /// Error/danger red
    Destructive,
}

impl BadgeVariant {
    /// Get background and text colors for this variant
    fn colors(&self, ui: &Ui) -> (Color32, Color32) {
        let is_dark = ui.visuals().dark_mode;

        match self {
            BadgeVariant::Default => {
                let bg = if is_dark {
                    Color32::from_rgba_unmultiplied(255, 255, 255, 20)
                } else {
                    Color32::from_rgba_unmultiplied(0, 0, 0, 15)
                };
                let fg = ui.visuals().text_color();
                (bg, fg)
            }
            BadgeVariant::Info => {
                // Blue tones
                let bg = if is_dark {
                    Color32::from_rgba_unmultiplied(59, 130, 246, 30)
                } else {
                    Color32::from_rgba_unmultiplied(59, 130, 246, 25)
                };
                let fg = if is_dark {
                    Color32::from_rgb(147, 197, 253) // blue-300
                } else {
                    Color32::from_rgb(29, 78, 216) // blue-700
                };
                (bg, fg)
            }
            BadgeVariant::Success => {
                // Green tones
                let bg = if is_dark {
                    Color32::from_rgba_unmultiplied(34, 197, 94, 30)
                } else {
                    Color32::from_rgba_unmultiplied(34, 197, 94, 25)
                };
                let fg = if is_dark {
                    Color32::from_rgb(134, 239, 172) // green-300
                } else {
                    Color32::from_rgb(21, 128, 61) // green-700
                };
                (bg, fg)
            }
            BadgeVariant::Warning => {
                // Amber/yellow tones
                let bg = if is_dark {
                    Color32::from_rgba_unmultiplied(245, 158, 11, 30)
                } else {
                    Color32::from_rgba_unmultiplied(245, 158, 11, 25)
                };
                let fg = if is_dark {
                    Color32::from_rgb(252, 211, 77) // amber-300
                } else {
                    Color32::from_rgb(180, 83, 9) // amber-700
                };
                (bg, fg)
            }
            BadgeVariant::Destructive => {
                // Red tones
                let bg = if is_dark {
                    Color32::from_rgba_unmultiplied(239, 68, 68, 30)
                } else {
                    Color32::from_rgba_unmultiplied(239, 68, 68, 25)
                };
                let fg = if is_dark {
                    Color32::from_rgb(252, 165, 165) // red-300
                } else {
                    Color32::from_rgb(185, 28, 28) // red-700
                };
                (bg, fg)
            }
        }
    }
}

/// A pill-shaped status badge widget (shadcn-style)
pub struct StatusBadge<'a> {
    text: &'a str,
    variant: BadgeVariant,
}

impl<'a> StatusBadge<'a> {
    /// Create a new status badge with the given text
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            variant: BadgeVariant::Default,
        }
    }

    /// Set the badge variant
    pub fn variant(mut self, variant: BadgeVariant) -> Self {
        self.variant = variant;
        self
    }

    /// Show the badge and return the response
    pub fn show(self, ui: &mut Ui) -> Response {
        let (bg_color, text_color) = self.variant.colors(ui);

        // Calculate text size for proper allocation
        let font_id = egui::FontId::proportional(11.0);
        let galley = ui.painter().layout_no_wrap(
            self.text.to_string(),
            font_id.clone(),
            text_color,
        );

        // Padding: horizontal 8px, vertical 2px
        let padding = Vec2::new(8.0, 3.0);
        let desired_size = galley.size() + padding * 2.0;

        let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::hover());

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();

            // Full pill rounding (half of height)
            let rounding = rect.height() / 2.0;

            // Background
            painter.rect_filled(rect, rounding, bg_color);

            // Text centered
            let text_pos = rect.center() - galley.size() / 2.0;
            painter.galley(text_pos, galley, text_color);
        }

        response
    }
}

