use crate::{
    app_style::{get_font_size, NotedeckTextStyle},
    column::Columns,
    fonts::NamedFontFamily,
    nav::RenderNavAction,
    route::Route,
    ui::anim::{AnimationHelper, ICON_EXPANSION_MULTIPLE},
};

use egui::{pos2, Color32, Stroke};

pub struct NavTitle<'a> {
    columns: &'a Columns,
    routes: &'a [Route],
}

impl<'a> NavTitle<'a> {
    pub fn new(columns: &'a Columns, routes: &'a [Route]) -> Self {
        NavTitle { columns, routes }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<RenderNavAction> {
        let mut rect = ui.available_rect_before_wrap();
        rect.set_height(48.0);
        let bar = ui.allocate_rect(rect, egui::Sense::hover());

        self.title_bar(ui, bar)
    }

    fn title_bar(
        &mut self,
        ui: &mut egui::Ui,
        allocated_response: egui::Response,
    ) -> Option<RenderNavAction> {
        let icon_width = 32.0;
        let padding_external = 16.0;
        let padding_internal = 8.0;
        let has_back = prev(self.routes).is_some();

        let (spacing_rect, titlebar_rect) = allocated_response
            .rect
            .split_left_right_at_x(allocated_response.rect.left() + padding_external);

        ui.advance_cursor_after_rect(spacing_rect);

        let (titlebar_resp, back_button_resp) = if has_back {
            let (button_rect, titlebar_rect) = titlebar_rect.split_left_right_at_x(
                allocated_response.rect.left() + icon_width + padding_external,
            );
            (
                allocated_response.with_new_rect(titlebar_rect),
                Some(self.back_button(ui, button_rect)),
            )
        } else {
            (allocated_response, None)
        };

        self.title(
            ui,
            self.routes.last().unwrap(),
            titlebar_resp.rect,
            icon_width,
            if has_back {
                padding_internal
            } else {
                padding_external
            },
        );

        let delete_button_resp =
            self.delete_column_button(ui, titlebar_resp, icon_width, padding_external);

        if delete_button_resp.clicked() {
            Some(RenderNavAction::RemoveColumn)
        } else if back_button_resp.map_or(false, |r| r.clicked()) {
            Some(RenderNavAction::Back)
        } else {
            None
        }
    }

    fn back_button(&self, ui: &mut egui::Ui, button_rect: egui::Rect) -> egui::Response {
        let horizontal_length = 10.0;
        let arrow_length = 5.0;

        let helper = AnimationHelper::new_from_rect(ui, "note-compose-button", button_rect);
        let painter = ui.painter_at(helper.get_animation_rect());
        let stroke = Stroke::new(1.5, ui.visuals().text_color());

        // Horizontal segment
        let left_horizontal_point = pos2(-horizontal_length / 2., 0.);
        let right_horizontal_point = pos2(horizontal_length / 2., 0.);
        let scaled_left_horizontal_point = helper.scale_pos_from_center(left_horizontal_point);
        let scaled_right_horizontal_point = helper.scale_pos_from_center(right_horizontal_point);

        painter.line_segment(
            [scaled_left_horizontal_point, scaled_right_horizontal_point],
            stroke,
        );

        // Top Arrow
        let sqrt_2_over_2 = std::f32::consts::SQRT_2 / 2.;
        let right_top_arrow_point = helper.scale_pos_from_center(pos2(
            left_horizontal_point.x + (sqrt_2_over_2 * arrow_length),
            right_horizontal_point.y + sqrt_2_over_2 * arrow_length,
        ));

        let scaled_left_arrow_point = scaled_left_horizontal_point;
        painter.line_segment([scaled_left_arrow_point, right_top_arrow_point], stroke);

        let right_bottom_arrow_point = helper.scale_pos_from_center(pos2(
            left_horizontal_point.x + (sqrt_2_over_2 * arrow_length),
            right_horizontal_point.y - sqrt_2_over_2 * arrow_length,
        ));

        painter.line_segment([scaled_left_arrow_point, right_bottom_arrow_point], stroke);

        helper.take_animation_response()
    }

    fn delete_column_button(
        &self,
        ui: &mut egui::Ui,
        allocation_response: egui::Response,
        icon_width: f32,
        padding: f32,
    ) -> egui::Response {
        let img_size = 16.0;
        let max_size = icon_width * ICON_EXPANSION_MULTIPLE;

        let img_data = if ui.visuals().dark_mode {
            egui::include_image!("../../../assets/icons/column_delete_icon_4x.png")
        } else {
            egui::include_image!("../../../assets/icons/column_delete_icon_light_4x.png")
        };
        let img = egui::Image::new(img_data).max_width(img_size);

        let button_rect = {
            let titlebar_rect = allocation_response.rect;
            let titlebar_width = titlebar_rect.width();
            let titlebar_center = titlebar_rect.center();
            let button_center_y = titlebar_center.y;
            let button_center_x =
                titlebar_center.x + (titlebar_width / 2.0) - (max_size / 2.0) - padding;
            egui::Rect::from_center_size(
                pos2(button_center_x, button_center_y),
                egui::vec2(max_size, max_size),
            )
        };

        let helper = AnimationHelper::new_from_rect(ui, "delete-column-button", button_rect);

        let cur_img_size = helper.scale_1d_pos_min_max(0.0, img_size);

        let animation_rect = helper.get_animation_rect();
        let animation_resp = helper.take_animation_response();

        img.paint_at(ui, animation_rect.shrink((max_size - cur_img_size) / 2.0));

        animation_resp
    }

    fn title(
        &mut self,
        ui: &mut egui::Ui,
        top: &Route,
        titlebar_rect: egui::Rect,
        icon_width: f32,
        padding: f32,
    ) {
        let painter = ui.painter_at(titlebar_rect);

        let font = egui::FontId::new(
            get_font_size(ui.ctx(), &NotedeckTextStyle::Body),
            egui::FontFamily::Name(NamedFontFamily::Bold.as_str().into()),
        );

        let max_title_width = titlebar_rect.width() - icon_width - padding * 2.;

        let title_galley = ui.fonts(|f| {
            f.layout(
                top.title(self.columns).to_string(),
                font,
                ui.visuals().text_color(),
                max_title_width,
            )
        });

        let pos = {
            let titlebar_center = titlebar_rect.center();
            let text_height = title_galley.rect.height();

            let galley_pos_x = titlebar_rect.left() + padding;
            let galley_pos_y = titlebar_center.y - (text_height / 2.);
            pos2(galley_pos_x, galley_pos_y)
        };

        painter.galley(pos, title_galley, Color32::WHITE);
    }
}

fn prev<R>(xs: &[R]) -> Option<&R> {
    let len = xs.len() as i32;
    let ind = len - 2;
    if ind < 0 {
        None
    } else {
        Some(&xs[ind as usize])
    }
}
