use crate::{
    app_style::NotedeckTextStyle,
    column::Columns,
    imgcache::ImageCache,
    nav::RenderNavAction,
    route::Route,
    timeline::{TimelineId, TimelineRoute},
    ui::{
        self,
        anim::{AnimationHelper, ICON_EXPANSION_MULTIPLE},
    },
};

use egui::{pos2, RichText, Stroke};
use enostr::Pubkey;
use nostrdb::{Ndb, Transaction};

pub struct NavTitle<'a> {
    ndb: &'a Ndb,
    img_cache: &'a mut ImageCache,
    columns: &'a Columns,
    deck_author: Option<&'a Pubkey>,
    routes: &'a [Route],
}

impl<'a> NavTitle<'a> {
    pub fn new(
        ndb: &'a Ndb,
        img_cache: &'a mut ImageCache,
        columns: &'a Columns,
        deck_author: Option<&'a Pubkey>,
        routes: &'a [Route],
    ) -> Self {
        NavTitle {
            ndb,
            img_cache,
            columns,
            deck_author,
            routes,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<RenderNavAction> {
        ui::padding(8.0, ui, |ui| {
            let mut rect = ui.available_rect_before_wrap();
            rect.set_height(48.0);

            let mut child_ui =
                ui.child_ui(rect, egui::Layout::left_to_right(egui::Align::Center), None);

            let r = self.title_bar(&mut child_ui);

            ui.advance_cursor_after_rect(rect);

            r
        })
        .inner
    }

    fn title_bar(&mut self, ui: &mut egui::Ui) -> Option<RenderNavAction> {
        let icon_width = 32.0;

        let back_button_resp = if prev(self.routes).is_some() {
            let (button_rect, _resp) =
                ui.allocate_exact_size(egui::vec2(icon_width, icon_width), egui::Sense::hover());

            Some(self.back_button(ui, button_rect))
        } else {
            None
        };

        let delete_button_resp = self.title(ui, self.routes.last().unwrap());

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

    fn delete_column_button(&self, ui: &mut egui::Ui, icon_width: f32) -> egui::Response {
        let img_size = 16.0;
        let max_size = icon_width * ICON_EXPANSION_MULTIPLE;

        let img_data = if ui.visuals().dark_mode {
            egui::include_image!("../../../assets/icons/column_delete_icon_4x.png")
        } else {
            egui::include_image!("../../../assets/icons/column_delete_icon_light_4x.png")
        };
        let img = egui::Image::new(img_data).max_width(img_size);

        let helper =
            AnimationHelper::new(ui, "delete-column-button", egui::vec2(max_size, max_size));

        let cur_img_size = helper.scale_1d_pos_min_max(0.0, img_size);

        let animation_rect = helper.get_animation_rect();
        let animation_resp = helper.take_animation_response();

        img.paint_at(ui, animation_rect.shrink((max_size - cur_img_size) / 2.0));

        animation_resp
    }

    fn pubkey_pfp<'txn, 'me>(
        &'me mut self,
        txn: &'txn Transaction,
        pubkey: &[u8; 32],
        pfp_size: f32,
    ) -> Option<ui::ProfilePic<'me, 'txn>> {
        self.ndb
            .get_profile_by_pubkey(txn, pubkey)
            .as_ref()
            .ok()
            .and_then(move |p| {
                Some(ui::ProfilePic::from_profile(self.img_cache, p)?.size(pfp_size))
            })
    }

    fn timeline_pfp(&mut self, ui: &mut egui::Ui, id: TimelineId, pfp_size: f32) {
        let txn = Transaction::new(self.ndb).unwrap();

        if let Some(pfp) = self
            .columns
            .find_timeline(id)
            .and_then(|tl| tl.kind.pubkey_source())
            .and_then(|pksrc| self.deck_author.map(|da| pksrc.to_pubkey(da)))
            .and_then(|pk| self.pubkey_pfp(&txn, pk.bytes(), pfp_size))
        {
            ui.add(pfp);
        } else {
            ui.add(
                ui::ProfilePic::new(self.img_cache, ui::ProfilePic::no_pfp_url()).size(pfp_size),
            );
        }
    }

    fn title_pfp(&mut self, ui: &mut egui::Ui, top: &Route) {
        let pfp_size = 32.0;
        match top {
            Route::Timeline(tlr) => match tlr {
                TimelineRoute::Timeline(tlid) => {
                    self.timeline_pfp(ui, *tlid, pfp_size);
                }

                TimelineRoute::Thread(_note_id) => {}
                TimelineRoute::Reply(_note_id) => {}
                TimelineRoute::Quote(_note_id) => {}

                TimelineRoute::Profile(pubkey) => {
                    let txn = Transaction::new(self.ndb).unwrap();
                    if let Some(pfp) = self.pubkey_pfp(&txn, pubkey.bytes(), pfp_size) {
                        ui.add(pfp);
                    } else {
                        ui.add(
                            ui::ProfilePic::new(self.img_cache, ui::ProfilePic::no_pfp_url())
                                .size(pfp_size),
                        );
                    }
                }
            },

            Route::Accounts(_as) => {}
            Route::ComposeNote => {}
            Route::AddColumn(_add_col_route) => {}
            Route::Support => {}
            Route::Relays => {}
        }
    }

    fn title(&mut self, ui: &mut egui::Ui, top: &Route) -> egui::Response {
        ui.spacing_mut().item_spacing.x = 10.0;

        self.title_pfp(ui, top);

        ui.label(
            RichText::new(top.title(self.columns)).text_style(NotedeckTextStyle::Body.text_style()),
        );

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            self.delete_column_button(ui, 32.0)
        })
        .inner
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
