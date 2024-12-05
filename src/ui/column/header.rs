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

use egui::{RichText, Stroke};
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
        ui.spacing_mut().item_spacing.x = 10.0;

        let back_button_resp = prev(self.routes).map(|r| self.back_button(ui, r));

        let delete_button_resp =
            self.title(ui, self.routes.last().unwrap(), back_button_resp.is_some());

        if delete_button_resp.map_or(false, |r| r.clicked()) {
            Some(RenderNavAction::RemoveColumn)
        } else if back_button_resp.map_or(false, |r| r.clicked()) {
            Some(RenderNavAction::Back)
        } else {
            None
        }
    }

    fn back_button(&self, ui: &mut egui::Ui, prev: &Route) -> egui::Response {
        let prev_spacing = ui.spacing().item_spacing.x;
        ui.spacing_mut().item_spacing.x = 4.0;

        //let color = ui.visuals().hyperlink_color;
        let color = ui.style().visuals.noninteractive().fg_stroke.color;

        let chev_resp = chevron(
            ui,
            2.0,
            egui::Vec2::new(10.0, 15.0),
            Stroke::new(2.0, color),
        );

        let back_label = ui.add(
            egui::Label::new(
                RichText::new(prev.title(self.columns).to_string())
                    .color(color)
                    .text_style(NotedeckTextStyle::Body.text_style()),
            )
            .selectable(false)
            .sense(egui::Sense::click()),
        );

        ui.spacing_mut().item_spacing.x = prev_spacing;

        back_label.union(chev_resp)
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

    fn title_label(&self, ui: &mut egui::Ui, top: &Route) {
        ui.add(
            egui::Label::new(
                RichText::new(top.title(self.columns))
                    .text_style(NotedeckTextStyle::Body.text_style()),
            )
            .selectable(false),
        );
    }

    fn title(
        &mut self,
        ui: &mut egui::Ui,
        top: &Route,
        navigating: bool,
    ) -> Option<egui::Response> {
        self.title_pfp(ui, top);

        if !navigating {
            self.title_label(ui, top);
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if navigating {
                self.title_label(ui, top);
                None
            } else {
                Some(self.delete_column_button(ui, 32.0))
            }
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

fn chevron(
    ui: &mut egui::Ui,
    pad: f32,
    size: egui::Vec2,
    stroke: impl Into<Stroke>,
) -> egui::Response {
    let (r, painter) = ui.allocate_painter(size, egui::Sense::click());

    let min = r.rect.min;
    let max = r.rect.max;

    let apex = egui::Pos2::new(min.x + pad, min.y + size.y / 2.0);
    let top = egui::Pos2::new(max.x - pad, min.y + pad);
    let bottom = egui::Pos2::new(max.x - pad, max.y - pad);

    let stroke = stroke.into();
    painter.line_segment([apex, top], stroke);
    painter.line_segment([apex, bottom], stroke);

    r
}
