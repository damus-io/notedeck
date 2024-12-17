use crate::{
    column::Columns,
    nav::RenderNavAction,
    route::Route,
    timeline::{ColumnTitle, TimelineId, TimelineRoute},
    ui::{
        self,
        anim::{AnimationHelper, ICON_EXPANSION_MULTIPLE},
    },
};

use egui::{RichText, Stroke, UiBuilder};
use enostr::Pubkey;
use nostrdb::{Ndb, Transaction};
use notedeck::{ImageCache, NotedeckTextStyle};

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

            let mut child_ui = ui.new_child(
                UiBuilder::new()
                    .max_rect(rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );

            let r = self.title_bar(&mut child_ui);

            ui.advance_cursor_after_rect(rect);

            r
        })
        .inner
    }

    fn title_bar(&mut self, ui: &mut egui::Ui) -> Option<RenderNavAction> {
        let item_spacing = 8.0;
        ui.spacing_mut().item_spacing.x = item_spacing;

        let chev_x = 8.0;
        let back_button_resp =
            prev(self.routes).map(|r| self.back_button(ui, r, egui::Vec2::new(chev_x, 15.0)));

        // add some space where chevron would have been. this makes the ui
        // less bumpy when navigating
        if back_button_resp.is_none() {
            ui.add_space(chev_x + item_spacing);
        }

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

    fn back_button(
        &mut self,
        ui: &mut egui::Ui,
        prev: &Route,
        chev_size: egui::Vec2,
    ) -> egui::Response {
        //let color = ui.visuals().hyperlink_color;
        let color = ui.style().visuals.noninteractive().fg_stroke.color;

        //let spacing_prev = ui.spacing().item_spacing.x;
        //ui.spacing_mut().item_spacing.x = 0.0;

        let chev_resp = chevron(ui, 2.0, chev_size, Stroke::new(2.0, color));

        //ui.spacing_mut().item_spacing.x = spacing_prev;

        // NOTE(jb55): include graphic in back label as well because why
        // not it looks cool
        self.title_pfp(ui, prev, 32.0);

        let column_title = prev.title(self.columns);

        let back_resp = match &column_title {
            ColumnTitle::Simple(title) => ui.add(Self::back_label(title, color)),

            ColumnTitle::NeedsDb(need_db) => {
                let txn = Transaction::new(self.ndb).unwrap();
                let title = need_db.title(&txn, self.ndb, self.deck_author);
                ui.add(Self::back_label(title, color))
            }
        };

        back_resp.union(chev_resp)
    }

    fn back_label(title: &str, color: egui::Color32) -> egui::Label {
        egui::Label::new(
            RichText::new(title.to_string())
                .color(color)
                .text_style(NotedeckTextStyle::Body.text_style()),
        )
        .selectable(false)
        .sense(egui::Sense::click())
    }

    fn delete_column_button(&self, ui: &mut egui::Ui, icon_width: f32) -> egui::Response {
        let img_size = 16.0;
        let max_size = icon_width * ICON_EXPANSION_MULTIPLE;

        let img_data = if ui.visuals().dark_mode {
            egui::include_image!("../../../../../assets/icons/column_delete_icon_4x.png")
        } else {
            egui::include_image!("../../../../../assets/icons/column_delete_icon_light_4x.png")
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

    fn title_pfp(&mut self, ui: &mut egui::Ui, top: &Route, pfp_size: f32) {
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
            Route::NewDeck => {}
            Route::EditDeck(_) => {}
        }
    }

    fn title_label_value(title: &str) -> egui::Label {
        egui::Label::new(RichText::new(title).text_style(NotedeckTextStyle::Body.text_style()))
            .selectable(false)
    }

    fn title_label(&self, ui: &mut egui::Ui, top: &Route) {
        let column_title = top.title(self.columns);

        match &column_title {
            ColumnTitle::Simple(title) => {
                ui.add(Self::title_label_value(title));
            }

            ColumnTitle::NeedsDb(need_db) => {
                let txn = Transaction::new(self.ndb).unwrap();
                let title = need_db.title(&txn, self.ndb, self.deck_author);
                ui.add(Self::title_label_value(title));
            }
        };
    }

    fn title(
        &mut self,
        ui: &mut egui::Ui,
        top: &Route,
        navigating: bool,
    ) -> Option<egui::Response> {
        if !navigating {
            self.title_pfp(ui, top, 32.0);
            self.title_label(ui, top);
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if navigating {
                self.title_label(ui, top);
                self.title_pfp(ui, top, 32.0);
                None
            } else {
                Some(self.delete_column_button(ui, 32.0))
            }
        })
        .inner
    }
}

fn prev<R>(xs: &[R]) -> Option<&R> {
    xs.get(xs.len().checked_sub(2)?)
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
