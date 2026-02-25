use std::collections::{HashMap, HashSet};

use egui::{Align, Button, CornerRadius, Frame, Id, Layout, Margin, Rgba, RichText, Ui, Vec2};
use enostr::{NormRelayUrl, RelayStatus};
use notedeck::{
    tr, DragResponse, Localization, NotedeckTextStyle, RelayAction, RelayInspectApi, RelaySpec,
};
use notedeck_ui::app_images;
use notedeck_ui::{colors::PINK, padding};
use tracing::debug;

use super::widgets::styled_button;

pub struct RelayView<'r, 'a> {
    relay_inspect: RelayInspectApi<'r, 'a>,
    advertised_relays: &'a std::collections::BTreeSet<RelaySpec>,
    id_string_map: &'a mut HashMap<Id, String>,
    i18n: &'a mut Localization,
}

struct RelayRow {
    relay_url: String,
    status: RelayStatus,
}

impl RelayView<'_, '_> {
    pub fn ui(&mut self, ui: &mut egui::Ui) -> DragResponse<RelayAction> {
        let scroll_out = Frame::new()
            .inner_margin(Margin::symmetric(10, 0))
            .show(ui, |ui| {
                ui.add_space(24.0);

                ui.horizontal(|ui| {
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.label(
                            RichText::new(tr!(self.i18n, "Relays", "Label for relay list section"))
                                .text_style(NotedeckTextStyle::Heading2.text_style()),
                        );
                    });
                });

                ui.add_space(8.0);

                egui::ScrollArea::vertical()
                    .id_salt(RelayView::scroll_id())
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        let mut action = None;
                        if let Some(relay_to_remove) = self.show_relays(ui) {
                            action = Some(RelayAction::Remove(relay_to_remove));
                        }
                        ui.add_space(8.0);
                        if let Some(relay_to_add) = self.show_add_relay_ui(ui) {
                            action = Some(RelayAction::Add(relay_to_add));
                        }
                        action
                    })
            })
            .inner;

        DragResponse::scroll(scroll_out)
    }

    pub fn scroll_id() -> egui::Id {
        egui::Id::new("relay_scroll")
    }
}

impl<'r, 'a> RelayView<'r, 'a> {
    pub fn new(
        relay_inspect: RelayInspectApi<'r, 'a>,
        advertised_relays: &'a std::collections::BTreeSet<RelaySpec>,
        id_string_map: &'a mut HashMap<Id, String>,
        i18n: &'a mut Localization,
    ) -> Self {
        RelayView {
            relay_inspect,
            advertised_relays,
            id_string_map,
            i18n,
        }
    }

    pub fn panel(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui.ctx(), |ui| self.ui(ui));
    }

    /// Show the selected account's advertised relays and
    /// any other currently-connected outbox relays.
    fn show_relays(&mut self, ui: &mut Ui) -> Option<String> {
        let relay_infos = self.relay_inspect.relay_infos();
        let status_by_url: HashMap<String, RelayStatus> = relay_infos
            .iter()
            .map(|relay_info| (relay_info.relay_url.to_string(), relay_info.status))
            .collect();

        let advertised_urls: HashSet<String> = self
            .advertised_relays
            .iter()
            .map(|relay| relay.url.to_string())
            .collect();

        let mut advertised = Vec::new();

        for relay in self.advertised_relays {
            let url = relay.url.to_string();
            let status = status_by_url
                .get(&url)
                .copied()
                .unwrap_or(RelayStatus::Disconnected);

            advertised.push(RelayRow {
                relay_url: url,
                status,
            });
        }

        let mut outbox_other = Vec::new();
        for relay_info in relay_infos {
            let url = relay_info.relay_url.to_string();
            if advertised_urls.contains(&url) {
                continue;
            }
            outbox_other.push(RelayRow {
                relay_url: url,
                status: relay_info.status,
            });
        }

        let mut relay_to_remove = None;
        let advertised_label = tr!(
            self.i18n,
            "Advertised",
            "Section header for advertised relays"
        );
        let outbox_other_label = tr!(
            self.i18n,
            "Other",
            "Section header for non-advertised connected relays"
        );

        relay_to_remove = relay_to_remove.or_else(|| {
            self.show_relay_section(ui, &advertised_label, &advertised, true, "relay-advertised")
        });
        relay_to_remove = relay_to_remove.or_else(|| {
            self.show_relay_section(
                ui,
                &outbox_other_label,
                &outbox_other,
                false,
                "relay-outbox-other",
            )
        });

        relay_to_remove
    }

    fn show_relay_section(
        &mut self,
        ui: &mut Ui,
        title: &str,
        rows: &[RelayRow],
        allow_delete: bool,
        id_prefix: &'static str,
    ) -> Option<String> {
        let mut relay_to_remove = None;

        ui.add_space(8.0);
        ui.label(
            RichText::new(title)
                .text_style(NotedeckTextStyle::Body.text_style())
                .strong(),
        );
        ui.add_space(4.0);

        if rows.is_empty() {
            ui.label(
                RichText::new(tr!(self.i18n, "None", "Empty relay section placeholder"))
                    .text_style(NotedeckTextStyle::Body.text_style())
                    .weak(),
            );
            return None;
        }

        for (index, relay_row) in rows.iter().enumerate() {
            relay_to_remove = relay_to_remove
                .or_else(|| self.show_relay_row(ui, relay_row, allow_delete, (id_prefix, index)));
        }

        relay_to_remove
    }

    fn show_relay_row(
        &mut self,
        ui: &mut Ui,
        relay_row: &RelayRow,
        allow_delete: bool,
        id_salt: impl std::hash::Hash,
    ) -> Option<String> {
        let mut relay_to_remove = None;

        ui.add_space(8.0);
        ui.vertical_centered_justified(|ui| {
            relay_frame(ui).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        Frame::new()
                            // This frame is needed to add margin because the label will be added to the outer frame first and centered vertically before the connection status is added so the vertical centering isn't accurate.
                            // TODO: remove this hack and actually center the url & status at the same time
                            .inner_margin(Margin::symmetric(0, 4))
                            .show(ui, |ui| {
                                egui::ScrollArea::horizontal()
                                    .id_salt(id_salt)
                                    .max_width(
                                        ui.max_rect().width()
                                            - get_right_side_width(relay_row.status),
                                    ) // TODO: refactor to dynamically check the size of the 'right to left' portion and set the max width to be the screen width minus padding minus 'right to left' width
                                    .show(ui, |ui| {
                                        ui.label(
                                            RichText::new(&relay_row.relay_url)
                                                .text_style(
                                                    NotedeckTextStyle::Monospace.text_style(),
                                                )
                                                .color(
                                                    ui.style()
                                                        .visuals
                                                        .noninteractive()
                                                        .fg_stroke
                                                        .color,
                                                ),
                                        );
                                    });
                            });
                    });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if allow_delete && ui.add(delete_button(ui.visuals().dark_mode)).clicked() {
                            relay_to_remove = Some(relay_row.relay_url.clone());
                        }

                        show_connection_status(ui, self.i18n, relay_row.status);
                    });
                });
            });
        });

        relay_to_remove
    }

    const RELAY_PREFILL: &'static str = "wss://";

    fn show_add_relay_ui(&mut self, ui: &mut Ui) -> Option<String> {
        let id = ui.id().with("add-relay)");
        match self.id_string_map.get(&id) {
            None => {
                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                    let relay_button = add_relay_button(self.i18n);
                    if ui.add(relay_button).clicked() {
                        debug!("add relay clicked");
                        self.id_string_map
                            .insert(id, Self::RELAY_PREFILL.to_string());
                    };
                });
                None
            }
            Some(_) => {
                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                    self.add_relay_entry(ui, id)
                })
                .inner
            }
        }
    }

    pub fn add_relay_entry(&mut self, ui: &mut Ui, id: Id) -> Option<String> {
        padding(16.0, ui, |ui| {
            let text_buffer = self
                .id_string_map
                .entry(id)
                .or_insert_with(|| Self::RELAY_PREFILL.to_string());
            let is_enabled = NormRelayUrl::new(text_buffer).is_ok();
            let text_edit = egui::TextEdit::singleline(text_buffer)
                .hint_text(
                    RichText::new(tr!(
                        self.i18n,
                        "Enter the relay here",
                        "Placeholder for relay input field"
                    ))
                    .text_style(NotedeckTextStyle::Body.text_style()),
                )
                .vertical_align(Align::Center)
                .desired_width(f32::INFINITY)
                .min_size(Vec2::new(0.0, 40.0))
                .margin(Margin::same(12));
            ui.add(text_edit);
            ui.add_space(8.0);
            if ui
                .add_sized(
                    egui::vec2(50.0, 40.0),
                    add_relay_button2(self.i18n, is_enabled),
                )
                .clicked()
            {
                self.id_string_map.remove(&id) // remove and return the value
            } else {
                None
            }
        })
        .inner
    }
}

fn add_relay_button(i18n: &mut Localization) -> Button<'static> {
    Button::image_and_text(
        app_images::add_relay_image().fit_to_exact_size(Vec2::new(48.0, 48.0)),
        RichText::new(tr!(i18n, "Add relay", "Button label to add a relay"))
            .size(16.0)
            // TODO: this color should not be hard coded. Find some way to add it to the visuals
            .color(PINK),
    )
    .frame(false)
}

fn add_relay_button2<'a>(i18n: &'a mut Localization, is_enabled: bool) -> impl egui::Widget + 'a {
    move |ui: &mut egui::Ui| -> egui::Response {
        let add_text = tr!(i18n, "Add", "Button label to add a relay");
        let button_widget = styled_button(add_text.as_str(), notedeck_ui::colors::PINK);
        ui.add_enabled(is_enabled, button_widget)
    }
}

fn get_right_side_width(status: RelayStatus) -> f32 {
    match status {
        RelayStatus::Connected => 150.0,
        RelayStatus::Connecting => 160.0,
        RelayStatus::Disconnected => 175.0,
    }
}

fn delete_button(dark_mode: bool) -> egui::Button<'static> {
    let img = if dark_mode {
        app_images::delete_dark_image()
    } else {
        app_images::delete_light_image()
    };

    egui::Button::image(img.max_width(10.0)).frame(false)
}

fn relay_frame(ui: &mut Ui) -> Frame {
    Frame::new()
        .inner_margin(Margin::same(8))
        .corner_radius(ui.style().noninteractive().corner_radius)
        .stroke(ui.style().visuals.noninteractive().bg_stroke)
}

fn show_connection_status(ui: &mut Ui, i18n: &mut Localization, status: RelayStatus) {
    let fg_color = match status {
        RelayStatus::Connected => ui.visuals().selection.bg_fill,
        RelayStatus::Connecting => ui.visuals().warn_fg_color,
        RelayStatus::Disconnected => ui.visuals().error_fg_color,
    };
    let bg_color = egui::lerp(Rgba::from(fg_color)..=Rgba::BLACK, 0.8).into();

    let label_text = match status {
        RelayStatus::Connected => tr!(i18n, "Connected", "Status label for connected relay"),
        RelayStatus::Connecting => tr!(i18n, "Connecting...", "Status label for connecting relay"),
        RelayStatus::Disconnected => {
            tr!(i18n, "Not Connected", "Status label for disconnected relay")
        }
    };

    let frame = Frame::new()
        .corner_radius(CornerRadius::same(100))
        .fill(bg_color)
        .inner_margin(Margin::symmetric(12, 4));

    frame.show(ui, |ui| {
        ui.label(RichText::new(label_text).color(fg_color));
        ui.add(get_connection_icon(status));
    });
}

fn get_connection_icon(status: RelayStatus) -> egui::Image<'static> {
    match status {
        RelayStatus::Connected => app_images::connected_image(),
        RelayStatus::Connecting => app_images::connecting_image(),
        RelayStatus::Disconnected => app_images::disconnected_image(),
    }
}
