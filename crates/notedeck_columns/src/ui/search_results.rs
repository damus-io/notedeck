use egui::{vec2, FontId, Pos2, Rect, ScrollArea, Vec2b};
use nostrdb::{Ndb, ProfileRecord, Transaction};
use notedeck::{fonts::get_font_size, ImageCache, NotedeckTextStyle};
use tracing::error;

use crate::{
    profile::get_display_name,
    ui::anim::{AnimationHelper, ICON_EXPANSION_MULTIPLE},
};

use super::{profile::get_profile_url, ProfilePic};

pub struct SearchResultsView<'a> {
    ndb: &'a Ndb,
    txn: &'a Transaction,
    img_cache: &'a mut ImageCache,
    results: &'a Vec<&'a [u8; 32]>,
}

impl<'a> SearchResultsView<'a> {
    pub fn new(
        img_cache: &'a mut ImageCache,
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

    fn show(&mut self, ui: &mut egui::Ui, width: f32) -> Option<usize> {
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

        selection
    }

    pub fn show_in_rect(&mut self, rect: egui::Rect, ui: &mut egui::Ui) -> Option<usize> {
        let widget_id = ui.id().with("search_results");
        let area_resp = egui::Area::new(widget_id)
            .order(egui::Order::Foreground)
            .fixed_pos(rect.left_top())
            .constrain_to(rect)
            .show(ui.ctx(), |ui| {
                egui::Frame::none()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(8.0)
                    .show(ui, |ui| {
                        let width = rect.width();
                        let scroll_resp = ScrollArea::vertical()
                            .max_width(width)
                            .auto_shrink(Vec2b::FALSE)
                            .show(ui, |ui| self.show(ui, width));
                        ui.advance_cursor_after_rect(rect);
                        scroll_resp.inner
                    })
                    .inner
            });

        area_resp.inner
    }
}

fn user_result<'a>(
    profile: &'a ProfileRecord<'_>,
    cache: &'a mut ImageCache,
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
            ProfilePic::new(cache, get_profile_url(Some(profile)))
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
