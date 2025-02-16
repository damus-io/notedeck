use crate::colors;
use crate::column::ColumnsAction;
use crate::nav::RenderNavAction;
use crate::nav::SwitchingAction;
use crate::{
    column::Columns,
    gif::GifStateMap,
    route::Route,
    timeline::{ColumnTitle, TimelineKind},
    ui::{
        self,
        anim::{AnimationHelper, ICON_EXPANSION_MULTIPLE},
    },
};

use egui::Margin;
use egui::{RichText, Stroke, UiBuilder};
use enostr::Pubkey;
use nostrdb::{Ndb, Transaction};
use notedeck::UrlMimes;
use notedeck::{Images, NotedeckTextStyle};

pub struct NavTitle<'a> {
    ndb: &'a Ndb,
    img_cache: &'a mut Images,
    urls: &'a mut UrlMimes,
    gifs: &'a mut GifStateMap,
    columns: &'a Columns,
    routes: &'a [Route],
    col_id: usize,
}

impl<'a> NavTitle<'a> {
    pub fn new(
        ndb: &'a Ndb,
        img_cache: &'a mut Images,
        urls: &'a mut UrlMimes,
        gifs: &'a mut GifStateMap,
        columns: &'a Columns,
        routes: &'a [Route],
        col_id: usize,
    ) -> Self {
        NavTitle {
            ndb,
            img_cache,
            urls,
            gifs,
            columns,
            routes,
            col_id,
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

        if let Some(back_resp) = &back_button_resp {
            if back_resp.hovered() || back_resp.clicked() {
                ui::show_pointer(ui);
            }
        } else {
            // add some space where chevron would have been. this makes the ui
            // less bumpy when navigating
            ui.add_space(chev_x + item_spacing);
        }

        let title_resp = self.title(ui, self.routes.last().unwrap(), back_button_resp.is_some());

        if let Some(resp) = title_resp {
            match resp {
                TitleResponse::RemoveColumn => Some(RenderNavAction::RemoveColumn),
                TitleResponse::MoveColumn(to_index) => {
                    let from = self.col_id;
                    Some(RenderNavAction::SwitchingAction(SwitchingAction::Columns(
                        ColumnsAction::Switch(from, to_index),
                    )))
                }
            }
        } else if back_button_resp.is_some_and(|r| r.clicked()) {
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

        let column_title = prev.title();

        let back_resp = match &column_title {
            ColumnTitle::Simple(title) => ui.add(Self::back_label(title, color)),

            ColumnTitle::NeedsDb(need_db) => {
                let txn = Transaction::new(self.ndb).unwrap();
                let title = need_db.title(&txn, self.ndb);
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

    fn delete_button_section(&self, ui: &mut egui::Ui) -> bool {
        let id = ui.id().with("title");

        let delete_button_resp = self.delete_column_button(ui, 32.0);
        if delete_button_resp.clicked() {
            ui.data_mut(|d| d.insert_temp(id, true));
        }

        if ui.data_mut(|d| *d.get_temp_mut_or_default(id)) {
            let mut confirm_pressed = false;
            delete_button_resp.show_tooltip_ui(|ui| {
                let confirm_resp = ui.button("Confirm");
                if confirm_resp.clicked() {
                    confirm_pressed = true;
                }

                if confirm_resp.clicked() || ui.button("Cancel").clicked() {
                    ui.data_mut(|d| d.insert_temp(id, false));
                }
            });
            if !confirm_pressed && delete_button_resp.clicked_elsewhere() {
                ui.data_mut(|d| d.insert_temp(id, false));
            }
            confirm_pressed
        } else {
            false
        }
    }

    // returns the column index to switch to, if any
    fn move_button_section(&mut self, ui: &mut egui::Ui) -> Option<usize> {
        let cur_id = ui.id().with("move");
        let mut move_resp = ui.add(grab_button());

        // showing the hover text while showing the move tooltip causes some weird visuals
        if ui.data(|d| d.get_temp::<bool>(cur_id).is_none()) {
            move_resp = move_resp.on_hover_text("Moves this column to another positon");
        }

        if move_resp.clicked() {
            ui.data_mut(|d| {
                if let Some(val) = d.get_temp::<bool>(cur_id) {
                    if val {
                        d.remove_temp::<bool>(cur_id);
                    } else {
                        d.insert_temp(cur_id, true);
                    }
                } else {
                    d.insert_temp(cur_id, true);
                }
            });
        } else if move_resp.hovered() {
            ui::show_pointer(ui);
        }

        ui.data(|d| d.get_temp(cur_id)).and_then(|val| {
            if val {
                let resp = self.add_move_tooltip(cur_id, &move_resp);
                if move_resp.clicked_elsewhere() || resp.is_some() {
                    ui.data_mut(|d| d.remove_temp::<bool>(cur_id));
                }
                resp
            } else {
                None
            }
        })
    }

    fn move_tooltip_col_presentation(&mut self, ui: &mut egui::Ui, col: usize) -> egui::Response {
        ui.horizontal(|ui| {
            self.title_presentation(ui, self.columns.column(col).router().top(), 32.0);
        })
        .response
    }

    fn add_move_tooltip(&mut self, id: egui::Id, move_resp: &egui::Response) -> Option<usize> {
        let mut inner_resp = None;
        move_resp.show_tooltip_ui(|ui| {
            // dnd frame color workaround
            ui.visuals_mut().widgets.inactive.bg_stroke = Stroke::default();
            let x_range = ui.available_rect_before_wrap().x_range();
            let is_dragging = egui::DragAndDrop::payload::<usize>(ui.ctx()).is_some(); // must be outside ui.dnd_drop_zone to capture properly
            let (_, _) = ui.dnd_drop_zone::<usize, ()>(
                egui::Frame::none().inner_margin(Margin::same(8.0)),
                |ui| {
                    let distances: Vec<(egui::Response, f32)> =
                        self.collect_column_distances(ui, id);

                    if let Some((closest_index, closest_resp, distance)) =
                        self.find_closest_column(&distances)
                    {
                        if is_dragging && closest_index != self.col_id {
                            if self.should_draw_hint(closest_index, distance) {
                                ui.painter().hline(
                                    x_range,
                                    self.calculate_hint_y(
                                        &distances,
                                        closest_resp,
                                        closest_index,
                                        distance,
                                    ),
                                    egui::Stroke::new(1.0, ui.visuals().text_color()),
                                );
                            }

                            if ui.input(|i| i.pointer.any_released()) {
                                inner_resp =
                                    Some(self.calculate_new_index(closest_index, distance));
                            }
                        }
                    }
                },
            );
        });
        inner_resp
    }

    fn collect_column_distances(
        &mut self,
        ui: &mut egui::Ui,
        id: egui::Id,
    ) -> Vec<(egui::Response, f32)> {
        let y_margin = 4.0;
        let item_frame = egui::Frame::none()
            .rounding(egui::Rounding::same(8.0))
            .inner_margin(Margin::symmetric(8.0, y_margin));

        (0..self.columns.num_columns())
            .filter_map(|col| {
                let item_id = id.with(col);
                let col_resp = if col == self.col_id {
                    ui.dnd_drag_source(item_id, col, |ui| {
                        item_frame
                            .stroke(egui::Stroke::new(2.0, colors::PINK))
                            .fill(ui.visuals().widgets.noninteractive.bg_stroke.color)
                            .show(ui, |ui| self.move_tooltip_col_presentation(ui, col));
                    })
                    .response
                } else {
                    item_frame
                        .show(ui, |ui| {
                            self.move_tooltip_col_presentation(ui, col)
                                .on_hover_cursor(egui::CursorIcon::NotAllowed)
                        })
                        .response
                };

                ui.input(|i| i.pointer.interact_pos()).map(|pointer| {
                    let distance = pointer.y - col_resp.rect.center().y;
                    (col_resp, distance)
                })
            })
            .collect()
    }

    fn find_closest_column(
        &'a self,
        distances: &'a [(egui::Response, f32)],
    ) -> Option<(usize, &'a egui::Response, f32)> {
        distances
            .iter()
            .enumerate()
            .min_by(|(_, (_, dist1)), (_, (_, dist2))| {
                dist1.abs().partial_cmp(&dist2.abs()).unwrap()
            })
            .filter(|(index, (_, distance))| {
                (index + 1 != self.col_id && *distance > 0.0)
                    || (index.saturating_sub(1) != self.col_id && *distance < 0.0)
            })
            .map(|(index, (resp, dist))| (index, resp, *dist))
    }

    fn should_draw_hint(&self, closest_index: usize, distance: f32) -> bool {
        let is_above = distance < 0.0;
        (is_above && closest_index.saturating_sub(1) != self.col_id)
            || (!is_above && closest_index + 1 != self.col_id)
    }

    fn calculate_new_index(&self, closest_index: usize, distance: f32) -> usize {
        let moving_up = self.col_id > closest_index;
        match (distance < 0.0, moving_up) {
            (true, true) | (false, false) => closest_index,
            (true, false) => closest_index.saturating_sub(1),
            (false, true) => closest_index + 1,
        }
    }

    fn calculate_hint_y(
        &self,
        distances: &[(egui::Response, f32)],
        closest_resp: &egui::Response,
        closest_index: usize,
        distance: f32,
    ) -> f32 {
        let y_margin = 4.0;

        let offset = if distance < 0.0 {
            distances
                .get(closest_index.wrapping_sub(1))
                .map(|(above_resp, _)| (closest_resp.rect.top() - above_resp.rect.bottom()) / 2.0)
                .unwrap_or(y_margin)
        } else {
            distances
                .get(closest_index + 1)
                .map(|(below_resp, _)| (below_resp.rect.top() - closest_resp.rect.bottom()) / 2.0)
                .unwrap_or(y_margin)
        };

        if distance < 0.0 {
            closest_resp.rect.top() - offset
        } else {
            closest_resp.rect.bottom() + offset
        }
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
                Some(
                    ui::ProfilePic::from_profile(self.img_cache, self.urls, self.gifs, p)?
                        .size(pfp_size),
                )
            })
    }

    fn timeline_pfp(&mut self, ui: &mut egui::Ui, id: &TimelineKind, pfp_size: f32) {
        let txn = Transaction::new(self.ndb).unwrap();

        if let Some(pfp) = id
            .pubkey()
            .and_then(|pk| self.pubkey_pfp(&txn, pk.bytes(), pfp_size))
        {
            ui.add(pfp);
        } else {
            ui.add(
                ui::ProfilePic::new(
                    self.img_cache,
                    self.urls,
                    self.gifs,
                    ui::ProfilePic::no_pfp_url(),
                )
                .size(pfp_size),
            );
        }
    }

    fn title_pfp(&mut self, ui: &mut egui::Ui, top: &Route, pfp_size: f32) {
        match top {
            Route::Timeline(kind) => match kind {
                TimelineKind::Hashtag(_ht) => {
                    ui.add(
                        egui::Image::new(egui::include_image!(
                            "../../../../../assets/icons/hashtag_icon_4x.png"
                        ))
                        .fit_to_exact_size(egui::vec2(pfp_size, pfp_size)),
                    );
                }

                TimelineKind::Profile(pubkey) => {
                    self.show_profile(ui, pubkey, pfp_size);
                }

                TimelineKind::Thread(_) => {
                    // no pfp for threads
                }

                TimelineKind::Universe
                | TimelineKind::Algo(_)
                | TimelineKind::Notifications(_)
                | TimelineKind::Generic(_)
                | TimelineKind::List(_) => {
                    self.timeline_pfp(ui, kind, pfp_size);
                }
            },

            Route::Reply(_) => {}
            Route::Quote(_) => {}
            Route::Accounts(_as) => {}
            Route::ComposeNote => {}
            Route::AddColumn(_add_col_route) => {}
            Route::Support => {}
            Route::Relays => {}
            Route::NewDeck => {}
            Route::EditDeck(_) => {}
            Route::EditProfile(pubkey) => {
                self.show_profile(ui, pubkey, pfp_size);
            }
        }
    }

    fn show_profile(&mut self, ui: &mut egui::Ui, pubkey: &Pubkey, pfp_size: f32) {
        let txn = Transaction::new(self.ndb).unwrap();
        if let Some(pfp) = self.pubkey_pfp(&txn, pubkey.bytes(), pfp_size) {
            ui.add(pfp);
        } else {
            ui.add(
                ui::ProfilePic::new(
                    self.img_cache,
                    self.urls,
                    self.gifs,
                    ui::ProfilePic::no_pfp_url(),
                )
                .size(pfp_size),
            );
        };
    }

    fn title_label_value(title: &str) -> egui::Label {
        egui::Label::new(RichText::new(title).text_style(NotedeckTextStyle::Body.text_style()))
            .selectable(false)
    }

    fn title_label(&self, ui: &mut egui::Ui, top: &Route) {
        let column_title = top.title();

        match &column_title {
            ColumnTitle::Simple(title) => {
                ui.add(Self::title_label_value(title));
            }

            ColumnTitle::NeedsDb(need_db) => {
                let txn = Transaction::new(self.ndb).unwrap();
                let title = need_db.title(&txn, self.ndb);
                ui.add(Self::title_label_value(title));
            }
        };
    }

    fn title(&mut self, ui: &mut egui::Ui, top: &Route, navigating: bool) -> Option<TitleResponse> {
        if !navigating {
            self.title_presentation(ui, top, 32.0);
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if navigating {
                self.title_presentation(ui, top, 32.0);
                None
            } else {
                let move_col = self.move_button_section(ui);
                let remove_col = self.delete_button_section(ui);
                if let Some(col) = move_col {
                    Some(TitleResponse::MoveColumn(col))
                } else if remove_col {
                    Some(TitleResponse::RemoveColumn)
                } else {
                    None
                }
            }
        })
        .inner
    }

    fn title_presentation(&mut self, ui: &mut egui::Ui, top: &Route, pfp_size: f32) {
        self.title_pfp(ui, top, pfp_size);
        self.title_label(ui, top);
    }
}

enum TitleResponse {
    RemoveColumn,
    MoveColumn(usize),
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

fn grab_button() -> impl egui::Widget {
    |ui: &mut egui::Ui| -> egui::Response {
        let max_size = egui::vec2(20.0, 20.0);
        let helper = AnimationHelper::new(ui, "grab", max_size);
        let painter = ui.painter_at(helper.get_animation_rect());
        let min_circle_radius = 1.0;
        let cur_circle_radius = helper.scale_1d_pos(min_circle_radius);
        let horiz_spacing = 4.0;
        let vert_spacing = 10.0;
        let horiz_from_center = (horiz_spacing + min_circle_radius) / 2.0;
        let vert_from_center = (vert_spacing + min_circle_radius) / 2.0;

        let color = ui.style().visuals.noninteractive().fg_stroke.color;

        let middle_left = helper.scale_from_center(-horiz_from_center, 0.0);
        let middle_right = helper.scale_from_center(horiz_from_center, 0.0);
        let top_left = helper.scale_from_center(-horiz_from_center, -vert_from_center);
        let top_right = helper.scale_from_center(horiz_from_center, -vert_from_center);
        let bottom_left = helper.scale_from_center(-horiz_from_center, vert_from_center);
        let bottom_right = helper.scale_from_center(horiz_from_center, vert_from_center);

        painter.circle_filled(middle_left, cur_circle_radius, color);
        painter.circle_filled(middle_right, cur_circle_radius, color);
        painter.circle_filled(top_left, cur_circle_radius, color);
        painter.circle_filled(top_right, cur_circle_radius, color);
        painter.circle_filled(bottom_left, cur_circle_radius, color);
        painter.circle_filled(bottom_right, cur_circle_radius, color);

        helper.take_animation_response()
    }
}
