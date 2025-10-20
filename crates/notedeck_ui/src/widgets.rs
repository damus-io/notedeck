use crate::anim::{AnimationHelper, ICON_EXPANSION_MULTIPLE};
use egui::{
    self, emath::GuiRounding, pos2, vec2, Color32, CornerRadius, CursorIcon, Key, Pos2, Response,
    Sense, Stroke, StrokeKind, Ui, Vec2, Widget,
};
use notedeck::NotedeckTextStyle;

pub fn x_button(rect: egui::Rect) -> impl egui::Widget {
    move |ui: &mut egui::Ui| -> egui::Response {
        let max_width = rect.width();
        let helper = AnimationHelper::new_from_rect(ui, "user_search_close", rect);

        let fill_color = ui.visuals().text_color();

        let radius = max_width / (2.0 * ICON_EXPANSION_MULTIPLE);

        let painter = ui.painter();
        let ppp = ui.ctx().pixels_per_point();
        let nw_edge = helper
            .scale_pos_from_center(Pos2::new(-radius, radius))
            .round_to_pixel_center(ppp);
        let se_edge = helper
            .scale_pos_from_center(Pos2::new(radius, -radius))
            .round_to_pixel_center(ppp);
        let sw_edge = helper
            .scale_pos_from_center(Pos2::new(-radius, -radius))
            .round_to_pixel_center(ppp);
        let ne_edge = helper
            .scale_pos_from_center(Pos2::new(radius, radius))
            .round_to_pixel_center(ppp);

        let line_width = helper.scale_1d_pos(2.0);

        painter.line_segment([nw_edge, se_edge], Stroke::new(line_width, fill_color));
        painter.line_segment([ne_edge, sw_edge], Stroke::new(line_width, fill_color));

        helper.take_animation_response()
    }
}

/// Button styled in the Notedeck theme
pub fn styled_button_toggleable(
    text: &str,
    fill_color: egui::Color32,
    enabled: bool,
) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| -> egui::Response {
        let painter = ui.painter();
        let text_color = if ui.visuals().dark_mode {
            egui::Color32::WHITE
        } else {
            egui::Color32::BLACK
        };

        let galley = painter.layout(
            text.to_owned(),
            NotedeckTextStyle::Button.get_font_id(ui.ctx()),
            text_color,
            ui.available_width(),
        );

        let size = galley.rect.expand2(egui::vec2(16.0, 8.0)).size();
        let mut button = egui::Button::new(galley).corner_radius(8.0);

        if !enabled {
            button = button
                .sense(egui::Sense::focusable_noninteractive())
                .fill(ui.visuals().noninteractive().bg_fill)
                .stroke(ui.visuals().noninteractive().bg_stroke);
        } else {
            button = button.fill(fill_color);
        }

        let mut resp = ui.add_sized(size, button);

        if !enabled {
            resp = resp.on_hover_cursor(egui::CursorIcon::NotAllowed);
        }

        resp
    }
}

pub struct IosSwitch<'a> {
    value: &'a mut bool,
    size: Vec2,
}

impl<'a> IosSwitch<'a> {
    pub fn new(value: &'a mut bool) -> Self {
        Self {
            value,
            size: vec2(52.0, 32.0),
        }
    }

    pub fn size(mut self, width: f32, height: f32) -> Self {
        self.size = vec2(width, height);
        self
    }
}

impl<'a> Widget for IosSwitch<'a> {
    fn ui(self, ui: &mut Ui) -> Response {
        let (rect, mut response) = ui.allocate_exact_size(self.size, Sense::click());
        response = response.on_hover_cursor(CursorIcon::PointingHand);

        if response.clicked()
            || (response.has_focus()
                && ui.input(|i| i.key_pressed(Key::Space) || i.key_pressed(Key::Enter)))
        {
            *self.value = !*self.value;
            response.mark_changed();
        }

        let t = ui.ctx().animate_bool(response.id, *self.value);
        let visuals = &ui.style().visuals;
        let painter = ui.painter();
        let h = rect.height();
        let rounding: CornerRadius = (h * 0.5).into();

        let off_col = visuals.widgets.inactive.bg_fill;
        let on_col = visuals.selection.bg_fill;
        let track_col = egui::ecolor::tint_color_towards(off_col, on_col);
        painter.rect(rect, rounding, track_col, Stroke::NONE, StrokeKind::Inside);

        let knob_margin = 2.0;
        let knob_d = h - knob_margin * 2.0;
        let knob_r = knob_d * 0.5;
        let left_x = rect.left() + knob_margin + knob_r;
        let right_x = rect.right() - knob_margin - knob_r;
        let knob_x = egui::lerp(left_x..=right_x, t);
        let knob_center = pos2(knob_x, rect.center().y);

        painter.circle_filled(
            knob_center + vec2(0.0, 1.0),
            knob_r + 1.0,
            Color32::from_black_alpha(30),
        );
        painter.circle_filled(knob_center, knob_r, visuals.extreme_bg_color);
        painter.circle_stroke(
            knob_center,
            knob_r,
            Stroke::new(1.0, Color32::from_black_alpha(40)),
        );

        if response.has_focus() {
            painter.rect_stroke(
                rect.expand(2.0),
                rounding,
                Stroke::new(1.0, visuals.selection.stroke.color),
                StrokeKind::Inside,
            );
        }

        response
    }
}

pub fn info_icon(ui: &mut Ui, tooltip: &str) -> Response {
    let size = vec2(18.0, 18.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    let visuals = ui.style().visuals.clone();

    let radius = size.x * 0.5;
    painter.circle_filled(rect.center(), radius, visuals.selection.bg_fill);
    painter.circle_stroke(rect.center(), radius, visuals.selection.stroke);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "i",
        egui::FontId::proportional(12.0),
        visuals.strong_text_color(),
    );

    response.on_hover_text(tooltip)
}
