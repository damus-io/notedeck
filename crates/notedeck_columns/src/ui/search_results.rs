use egui::{FontId, Layout, Pos2, Rect, ScrollArea, UiBuilder, Vec2b, vec2};
use nostrdb::{Ndb, ProfileRecord, Transaction};
use notedeck::{
    Images, NotedeckTextStyle, fonts::get_font_size, name::get_display_name,
    profile::get_profile_url,
};
use notedeck_ui::{
    ProfilePic,
    anim::{AnimationHelper, ICON_EXPANSION_MULTIPLE},
    widgets::x_button,
};
use tracing::error;

pub struct SearchResultsView<'a> {
    ndb: &'a Ndb,
    txn: &'a Transaction,
    img_cache: &'a mut Images,
    results: &'a Vec<&'a [u8; 32]>,
}

pub enum SearchResultsResponse {
    SelectResult(Option<usize>),
    DeleteMention,
}

impl<'a> SearchResultsView<'a> {
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

    fn show(&mut self, ui: &mut egui::Ui, width: f32) -> SearchResultsResponse {
        let mut search_results_selection = None;
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
                    search_results_selection = Some(i)
                }
            }
        });

        SearchResultsResponse::SelectResult(search_results_selection)
    }

    pub fn show_in_rect(&mut self, rect: egui::Rect, ui: &mut egui::Ui) -> SearchResultsResponse {
        let widget_id = ui.id().with("search_results");
        let area_resp = egui::Area::new(widget_id)
            .order(egui::Order::Foreground)
            .fixed_pos(rect.left_top())
            .constrain_to(rect)
            .show(ui.ctx(), |ui| {
                let inner_margin_size = 8.0;
                egui::Frame::NONE
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(inner_margin_size)
                    .show(ui, |ui| {
                        let width = rect.width() - (2.0 * inner_margin_size);

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

                        ui.add_space(8.0);

                        let scroll_resp = ScrollArea::vertical()
                            .max_width(width)
                            .auto_shrink(Vec2b::FALSE)
                            .show(ui, |ui| self.show(ui, width));
                        ui.advance_cursor_after_rect(rect);

                        if close_button_resp {
                            SearchResultsResponse::DeleteMention
                        } else {
                            scroll_resp.inner
                        }
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

        let helper = AnimationHelper::new(ui, ("user_result", index), vec2(width, max_image));

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
