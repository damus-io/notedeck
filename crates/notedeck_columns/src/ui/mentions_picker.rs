use egui::{vec2, Layout, ScrollArea, UiBuilder, Vec2b};
use nostrdb::{Ndb, Transaction};
use notedeck::{DragResponse, Images, Localization, MediaJobSender};
use notedeck_ui::{profile_row, widgets::x_button, ProfileSearchResult};
use tracing::error;

/// Displays user profiles for the user to pick from.
/// Useful for manually typing a username and selecting the profile desired
pub struct MentionPickerView<'a> {
    ndb: &'a Ndb,
    txn: &'a Transaction,
    img_cache: &'a mut Images,
    results: &'a [ProfileSearchResult],
    jobs: &'a MediaJobSender,
    i18n: &'a mut Localization,
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
        results: &'a [ProfileSearchResult],
        jobs: &'a MediaJobSender,
        i18n: &'a mut Localization,
    ) -> Self {
        Self {
            ndb,
            txn,
            img_cache,
            results,
            jobs,
            i18n,
        }
    }

    fn show(&mut self, ui: &mut egui::Ui) -> MentionPickerResponse {
        let mut selection = None;
        ui.vertical(|ui| {
            for (i, res) in self.results.iter().enumerate() {
                let profile = match self.ndb.get_profile_by_pubkey(self.txn, &res.pk) {
                    Ok(rec) => rec,
                    Err(e) => {
                        error!("Error fetching profile for pubkey {:?}: {e}", res.pk);
                        return;
                    }
                };

                if profile_row(
                    ui,
                    Some(&profile),
                    res.is_contact,
                    self.img_cache,
                    self.jobs,
                    self.i18n,
                ) {
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
    ) -> DragResponse<MentionPickerResponse> {
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
                        ui.allocate_space(vec2(ui.available_width(), inner_margin_size));
                        let close_button_resp = {
                            let close_button_size = 16.0;
                            let width = rect.width() - (2.0 * inner_margin_size);
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
                            .show(ui, |ui| Some(self.show(ui)));
                        ui.advance_cursor_after_rect(rect);

                        DragResponse::scroll(scroll_resp).map_output(|o| {
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
