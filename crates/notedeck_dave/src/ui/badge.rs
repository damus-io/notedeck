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
    keybind: Option<&'a str>,
}

impl<'a> StatusBadge<'a> {
    /// Create a new status badge with the given text
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            variant: BadgeVariant::Default,
            keybind: None,
        }
    }

    /// Set the badge variant
    pub fn variant(mut self, variant: BadgeVariant) -> Self {
        self.variant = variant;
        self
    }

    /// Add a keybind hint inside the badge (e.g., "P" for Ctrl+P)
    pub fn keybind(mut self, key: &'a str) -> Self {
        self.keybind = Some(key);
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

        // Calculate keybind box size if present
        let keybind_box_size = 14.0;
        let keybind_spacing = 5.0;
        let keybind_extra = if self.keybind.is_some() {
            keybind_box_size + keybind_spacing
        } else {
            0.0
        };

        // Padding: horizontal 8px, vertical 2px
        let padding = Vec2::new(8.0, 3.0);
        let desired_size =
            Vec2::new(galley.size().x + keybind_extra, galley.size().y) + padding * 2.0;

        let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::hover());

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();

            // Full pill rounding (half of height)
            let rounding = rect.height() / 2.0;

            // Background
            painter.rect_filled(rect, rounding, bg_color);

            // Text (offset left if keybind present)
            let text_offset_x = if self.keybind.is_some() {
                -keybind_extra / 2.0
            } else {
                0.0
            };
            let text_pos =
                rect.center() + Vec2::new(text_offset_x, 0.0) - galley.size() / 2.0;
            painter.galley(text_pos, galley, text_color);

            // Draw keybind box if present
            if let Some(key) = self.keybind {
                let box_center = egui::pos2(
                    rect.right() - padding.x - keybind_box_size / 2.0,
                    rect.center().y,
                );
                let box_rect =
                    egui::Rect::from_center_size(box_center, Vec2::splat(keybind_box_size));

                // Keybind box background (slightly darker/lighter than badge bg)
                let visuals = ui.visuals();
                let box_bg = visuals.widgets.noninteractive.bg_fill;
                let box_stroke = text_color.gamma_multiply(0.5);

                painter.rect_filled(box_rect, 3.0, box_bg);
                painter.rect_stroke(
                    box_rect,
                    3.0,
                    egui::Stroke::new(1.0, box_stroke),
                    egui::StrokeKind::Inside,
                );

                // Keybind text
                painter.text(
                    box_center + Vec2::new(0.0, 1.0),
                    egui::Align2::CENTER_CENTER,
                    key,
                    egui::FontId::monospace(keybind_box_size * 0.65),
                    visuals.text_color(),
                );
            }
        }

        response
    }
}

/// A pill-shaped action button with integrated keybind hint
pub struct ActionButton<'a> {
    text: &'a str,
    bg_color: Color32,
    text_color: Color32,
    keybind: Option<&'a str>,
}

impl<'a> ActionButton<'a> {
    /// Create a new action button with the given text and colors
    pub fn new(text: &'a str, bg_color: Color32, text_color: Color32) -> Self {
        Self {
            text,
            bg_color,
            text_color,
            keybind: None,
        }
    }

    /// Add a keybind hint inside the button (e.g., "1" for key 1)
    pub fn keybind(mut self, key: &'a str) -> Self {
        self.keybind = Some(key);
        self
    }

    /// Show the button and return the response
    pub fn show(self, ui: &mut Ui) -> Response {
        // Calculate text size for proper allocation
        let font_id = egui::FontId::proportional(13.0);
        let galley = ui.painter().layout_no_wrap(
            self.text.to_string(),
            font_id.clone(),
            self.text_color,
        );

        // Calculate keybind box size if present
        let keybind_box_size = 16.0;
        let keybind_spacing = 6.0;
        let keybind_extra = if self.keybind.is_some() {
            keybind_box_size + keybind_spacing
        } else {
            0.0
        };

        // Padding: horizontal 10px, vertical 4px
        let padding = Vec2::new(10.0, 4.0);
        let desired_size =
            Vec2::new(galley.size().x + keybind_extra, galley.size().y) + padding * 2.0;

        let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::click());

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();

            // Adjust color based on hover/click state
            let bg_color = if response.is_pointer_button_down_on() {
                self.bg_color.gamma_multiply(0.8)
            } else if response.hovered() {
                self.bg_color.gamma_multiply(1.15)
            } else {
                self.bg_color
            };

            // Full pill rounding (half of height)
            let rounding = rect.height() / 2.0;

            // Background
            painter.rect_filled(rect, rounding, bg_color);

            // Text (offset left if keybind present)
            let text_offset_x = if self.keybind.is_some() {
                -keybind_extra / 2.0
            } else {
                0.0
            };
            let text_pos =
                rect.center() + Vec2::new(text_offset_x, 0.0) - galley.size() / 2.0;
            painter.galley(text_pos, galley, self.text_color);

            // Draw keybind hint if present (no background, just text)
            if let Some(key) = self.keybind {
                let key_center = egui::pos2(
                    rect.right() - padding.x - keybind_box_size / 2.0,
                    rect.center().y,
                );

                // Keybind text using the button's text color
                painter.text(
                    key_center,
                    egui::Align2::CENTER_CENTER,
                    key,
                    egui::FontId::monospace(keybind_box_size * 0.7),
                    self.text_color,
                );
            }
        }

        response
    }
}

