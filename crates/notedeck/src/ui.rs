use egui::{Color32, Stroke};

use crate::NotedeckTextStyle;

pub const NARROW_SCREEN_WIDTH: f32 = 550.0;

pub fn toggle_ui(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let desired_size = ui.spacing().interact_size.y * egui::vec2(2.0, 1.0);
    let (rect, mut response) = ui.allocate_exact_size(desired_size, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, "")
    });

    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
        let visuals = ui.style().interact_selectable(&response, *on);
        let rect = rect.expand(visuals.expansion);
        let radius = 0.5 * rect.height();
        let stroke_color = if ui.visuals().dark_mode {
            Color32::WHITE
        } else {
            Color32::BLACK
        };
        let stroke = Stroke::new(visuals.fg_stroke.width, stroke_color);

        ui.painter().rect(
            rect,
            radius,
            visuals.bg_fill,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );
        let circle_x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
        let center = egui::pos2(circle_x, rect.center().y);
        ui.painter()
            .circle(center, 0.75 * radius, visuals.bg_fill, stroke);
    }

    response
}

pub fn richtext_small<S>(text: S) -> egui::RichText
where
    S: Into<String>,
{
    egui::RichText::new(text).text_style(NotedeckTextStyle::Small.text_style())
}

/// Determine if the screen is narrow. This is useful for detecting mobile
/// contexts, but with the nuance that we may also have a wide android tablet.
pub fn is_narrow(ctx: &egui::Context) -> bool {
    let screen_size = ctx.input(|c| c.screen_rect().size());
    screen_size.x < NARROW_SCREEN_WIDTH
}

pub fn is_oled(is_mobile_override: bool) -> bool {
    is_mobile_override || is_compiled_as_mobile()
}

#[inline]
#[allow(unreachable_code)]
pub fn is_compiled_as_mobile() -> bool {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        true
    }

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        false
    }
}
