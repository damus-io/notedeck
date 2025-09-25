use egui::{vec2, FontId, Layout, Pos2, Rect, ScrollArea, UiBuilder, Vec2b};
use nostrdb::{Ndb, ProfileRecord, Transaction};
use notedeck::{
    fonts::get_font_size, name::get_display_name, profile::get_profile_url, Images,
    NotedeckTextStyle,
};
use notedeck_ui::{
    anim::{AnimationHelper, ICON_EXPANSION_MULTIPLE},
    widgets::x_button,
    ProfilePic,
};
use tracing::error;

use crate::nav::BodyResponse;

/// Displays user profiles for the user to pick from.
/// Useful for manually typing a username and selecting the profile desired
pub struct MentionPickerView<'a> {
    ndb: &'a Ndb,
    txn: &'a Transaction,
    img_cache: &'a mut Images,
    results: &'a Vec<&'a [u8; 32]>,
}

pub enum MentionPickerResponse {
    SelectResult(Option<usize>),
    DeleteMention,
}

impl<'a> MentionPickerView<'a> {
    pub fn new(
        img_cache: &'a mut Images,
        ndb: &'a Ndb,
        txn: &'a Transaction,
        results: &'a Vec<&'a [u8; 32]>,
    ) -> Self {
        Self {
            ndb,
            txn,
            img_cache,
            results,
        }
    }

    fn show(&mut self, ui: &mut egui::Ui, width: f32) -> MentionPickerResponse {
        let mut selection = None;
        ui.vertical(|ui| {
            for (i, res) in self.results.iter().enumerate() {
                let profile = match self.ndb.get_profile_by_pubkey(self.txn, res) {
                    Ok(rec) => rec,
                    Err(e) => {
                        error!("Error fetching profile for pubkey {:?}: {e}", res);
                        return;
                    }
                };

                if ui
                    .add(user_result(&profile, self.img_cache, i, width))
                    .clicked()
                {
                    selection = Some(i)
                }
            }
        });

        MentionPickerResponse::SelectResult(selection)
    }

    pub fn show_in_rect(
        &mut self,
        rect: egui::Rect,
        ui: &mut egui::Ui,
    ) -> BodyResponse<MentionPickerResponse> {
        let widget_id = ui.id().with("mention_results");
        let area_resp = egui::Area::new(widget_id)
            .order(egui::Order::Foreground)
            .fixed_pos(rect.left_top())
            .constrain_to(rect)
            .show(ui.ctx(), |ui| {
                let inner_margin_size = 8.0;
                egui::Frame::NONE
                    .fill(ui.visuals().panel_fill)
                    .show(ui, |ui| {
                        let width = rect.width() - (2.0 * inner_margin_size);

                        ui.allocate_space(vec2(ui.available_width(), inner_margin_size));
                        let close_button_resp = {
                            let close_button_size = 16.0;
                            let (close_section_rect, _) = ui.allocate_exact_size(
                                vec2(width, close_button_size),
                                egui::Sense::hover(),
                            );
                            let (_, button_rect) = close_section_rect.split_left_right_at_x(
                                close_section_rect.right() - close_button_size,
                            );
                            let button_resp = ui.allocate_rect(button_rect, egui::Sense::click());
                            ui.allocate_new_ui(
                                UiBuilder::new()
                                    .max_rect(close_section_rect)
                                    .layout(Layout::right_to_left(egui::Align::Center)),
                                |ui| ui.add(x_button(button_resp.rect)).clicked(),
                            )
                            .inner
                        };

                        ui.allocate_space(vec2(ui.available_width(), inner_margin_size));

                        let scroll_resp = ScrollArea::vertical()
                            .max_width(rect.width())
                            .auto_shrink(Vec2b::FALSE)
                            .show(ui, |ui| Some(self.show(ui, width)));
                        ui.advance_cursor_after_rect(rect);

                        BodyResponse::scroll(scroll_resp).map_output(|o| {
                            if close_button_resp {
                                MentionPickerResponse::DeleteMention
                            } else {
                                o
                            }
                        })
                    })
                    .inner
            });

        area_resp.inner
    }
}

fn user_result<'a>(
    profile: &'a ProfileRecord<'_>,
    cache: &'a mut Images,
    index: usize,
    width: f32,
) -> impl egui::Widget + 'a {
    move |ui: &mut egui::Ui| -> egui::Response {
        let min_img_size = 48.0;
        let max_image = min_img_size * ICON_EXPANSION_MULTIPLE;
        let spacing = 8.0;
        let body_font_size = get_font_size(ui.ctx(), &NotedeckTextStyle::Body);

        let animation_rect = {
            let max_width = ui.available_width();
            let extra_width = (max_width - width) / 2.0;
            let left = ui.cursor().left();
            let (rect, _) =
                ui.allocate_exact_size(vec2(width + extra_width, max_image), egui::Sense::click());

            let (_, right) = rect.split_left_right_at_x(left + extra_width);
            right
        };

        let helper = AnimationHelper::new_from_rect(ui, ("user_result", index), animation_rect);

        let icon_rect = {
            let r = helper.get_animation_rect();
            let mut center = r.center();
            center.x = r.left() + (max_image / 2.0);
            let size = helper.scale_1d_pos(min_img_size);
            Rect::from_center_size(center, vec2(size, size))
        };

        let pfp_resp = ui.put(
            icon_rect,
            &mut ProfilePic::new(cache, get_profile_url(Some(profile)))
                .size(helper.scale_1d_pos(min_img_size)),
        );

        let name_font = FontId::new(
            helper.scale_1d_pos(body_font_size),
            NotedeckTextStyle::Body.font_family(),
        );
        let painter = ui.painter_at(helper.get_animation_rect());
        let name_galley = painter.layout(
            get_display_name(Some(profile)).name().to_owned(),
            name_font,
            ui.visuals().text_color(),
            width,
        );

        let galley_pos = {
            let right_top = pfp_resp.rect.right_top();
            let galley_pos_y = pfp_resp.rect.center().y - (name_galley.rect.height() / 2.0);
            Pos2::new(right_top.x + spacing, galley_pos_y)
        };

        painter.galley(galley_pos, name_galley, ui.visuals().text_color());
        ui.advance_cursor_after_rect(helper.get_animation_rect());

        pfp_resp.union(helper.take_animation_response())
    }
}
