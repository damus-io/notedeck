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
    pub fn colors(&self, ui: &Ui) -> (Color32, Color32) {
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

/// An icon a badge can paint in place of its text glyph. Drawn with vector
/// strokes so it stays crisp at any DPI (unlike a unicode glyph).
#[derive(Clone, Copy)]
pub enum BadgeIcon {
    /// A right-pointing chevron (›), e.g. "next" navigation.
    ChevronRight,
}

/// A pill-shaped status badge widget (shadcn-style)
pub struct StatusBadge<'a> {
    text: &'a str,
    variant: BadgeVariant,
    keybind: Option<&'a str>,
    min_size: Option<Vec2>,
    icon: Option<BadgeIcon>,
}

impl<'a> StatusBadge<'a> {
    /// Create a new status badge with the given text
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            variant: BadgeVariant::Default,
            keybind: None,
            min_size: None,
            icon: None,
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

    /// Enforce a minimum size for the badge's interactive rect. Useful for
    /// icon-only badges (e.g. a single glyph) whose natural size is too small
    /// to comfortably tap on touch/narrow layouts.
    pub fn min_size(mut self, min_size: Vec2) -> Self {
        self.min_size = Some(min_size);
        self
    }

    /// Paint a vector icon in place of the text glyph. The `text` still serves
    /// as the accessibility label (so the badge stays queryable in tests).
    pub fn icon(mut self, icon: BadgeIcon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Show the badge and return the response
    pub fn show(self, ui: &mut Ui) -> Response {
        let (bg_color, text_color) = self.variant.colors(ui);

        // Calculate text size for proper allocation
        let font_id = egui::FontId::proportional(11.0);
        let galley =
            ui.painter()
                .layout_no_wrap(self.text.to_string(), font_id.clone(), text_color);

        // Calculate keybind box size if present
        let keybind_box_size = 14.0;
        let keybind_spacing = 5.0;
        let keybind_extra = if self.keybind.is_some() {
            keybind_box_size + keybind_spacing
        } else {
            0.0
        };

        // An icon replaces the text glyph; size the content to a fixed icon
        // footprint instead of the (label-only) galley.
        let icon_size = Vec2::new(9.0, 11.0);
        let content_size = if self.icon.is_some() {
            icon_size
        } else {
            galley.size()
        };

        // Padding: horizontal 8px, vertical 2px
        let padding = Vec2::new(8.0, 3.0);
        let mut desired_size =
            Vec2::new(content_size.x + keybind_extra, content_size.y) + padding * 2.0;

        // Grow the tap target to the requested minimum (icon-only badges).
        if let Some(min) = self.min_size {
            desired_size = desired_size.max(min);
        }

        let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::click());
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, self.text));

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();

            // Full pill rounding (half of height)
            let rounding = rect.height() / 2.0;

            // Adjust background color based on hover/click state
            let bg_color = if response.is_pointer_button_down_on() {
                bg_color.gamma_multiply(1.8)
            } else if response.hovered() {
                bg_color.gamma_multiply(1.4)
            } else {
                bg_color
            };

            // Background
            painter.rect_filled(rect, rounding, bg_color);

            // Content (offset left if keybind present)
            let content_offset_x = if self.keybind.is_some() {
                -keybind_extra / 2.0
            } else {
                0.0
            };
            let content_center = rect.center() + Vec2::new(content_offset_x, 0.0);

            match self.icon {
                Some(BadgeIcon::ChevronRight) => {
                    // Reuse the shared chevron painter so the glyph matches the
                    // rest of the app and stays crisp at any DPI.
                    let icon_rect = egui::Rect::from_center_size(content_center, icon_size);
                    notedeck_ui::header::paint_chevron(
                        painter,
                        icon_rect,
                        1.5,
                        notedeck_ui::header::ChevronDir::Right,
                        egui::Stroke::new(1.5_f32, text_color),
                    );
                }
                None => {
                    let text_pos = content_center - galley.size() / 2.0;
                    painter.galley(text_pos, galley, text_color);
                }
            }

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
                    egui::Stroke::new(notedeck::tokens::STROKE_THIN, box_stroke),
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
        let galley =
            ui.painter()
                .layout_no_wrap(self.text.to_string(), font_id.clone(), self.text_color);

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
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, self.text));

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

            // Text (offset right if keybind present, since keybind goes on left)
            let text_offset_x = if self.keybind.is_some() {
                keybind_extra / 2.0
            } else {
                0.0
            };
            let text_pos = rect.center() + Vec2::new(text_offset_x, 0.0) - galley.size() / 2.0;
            painter.galley(text_pos, galley, self.text_color);

            // Draw keybind hint on left side (white border, no fill)
            if let Some(key) = self.keybind {
                let box_center = egui::pos2(
                    rect.left() + padding.x + keybind_box_size / 2.0,
                    rect.center().y,
                );
                let box_rect =
                    egui::Rect::from_center_size(box_center, Vec2::splat(keybind_box_size));

                // White border only
                painter.rect_stroke(
                    box_rect,
                    3.0,
                    egui::Stroke::new(notedeck::tokens::STROKE_THIN, Color32::WHITE),
                    egui::StrokeKind::Inside,
                );

                // Keybind text with vertical nudge for optical centering
                painter.text(
                    box_center + Vec2::new(0.0, 1.0),
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

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;

    /// A tiny icon-only badge (a single glyph) has a tap target that's too small
    /// to hit comfortably; `min_size` must grow the interactive rect to at least
    /// the requested size while leaving the natural size for wider badges.
    #[test]
    fn min_size_grows_tiny_badge_tap_target() {
        let want = Vec2::new(40.0, 28.0);

        let mut harness = Harness::new_ui_state(
            |ui, state: &mut (Vec2, Vec2)| {
                let chevron = || StatusBadge::new("Next").icon(BadgeIcon::ChevronRight);
                state.0 = chevron().show(ui).rect.size();
                state.1 = chevron().min_size(want).show(ui).rect.size();
            },
            (Vec2::ZERO, Vec2::ZERO),
        );
        harness.run();

        let (natural, enlarged) = *harness.state();

        // The bare chevron badge is genuinely small — that's the problem we fix.
        assert!(
            natural.x < want.x,
            "expected the chevron badge to be narrower than the tap target, got {natural:?}"
        );
        // min_size lifts it to (at least) the requested tap target.
        assert!(
            enlarged.x >= want.x && enlarged.y >= want.y,
            "min_size should grow the badge to at least {want:?}, got {enlarged:?}"
        );
    }

    /// Visualize the chevron icon badge across variants. Ignored by default;
    /// render with `scripts/snapshot-test snapshot_chevron_badge`.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_chevron_badge() {
        let mut harness = Harness::builder()
            .with_size(Vec2::new(220.0, 40.0))
            .renderer(notedeck::software_renderer())
            .build_ui(|ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    for variant in [
                        BadgeVariant::Warning,
                        BadgeVariant::Destructive,
                        BadgeVariant::Info,
                    ] {
                        StatusBadge::new("Next")
                            .icon(BadgeIcon::ChevronRight)
                            .variant(variant)
                            .min_size(Vec2::new(40.0, 28.0))
                            .show(ui);
                    }
                });
            });

        harness.run();
        harness.snapshot("chevron_badge");
    }
}
