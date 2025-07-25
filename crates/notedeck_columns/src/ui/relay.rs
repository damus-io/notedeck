use std::collections::HashMap;

use crate::ui::{Preview, PreviewConfig};
use egui::{Align, Button, CornerRadius, Frame, Id, Layout, Margin, Rgba, RichText, Ui, Vec2};
use enostr::{RelayPool, RelayStatus};
use notedeck::{tr, Localization, NotedeckTextStyle, RelayAction};
use notedeck_ui::app_images;
use notedeck_ui::{colors::PINK, padding};
use tracing::debug;

use super::widgets::styled_button;

pub struct RelayView<'a> {
    pool: &'a RelayPool,
    id_string_map: &'a mut HashMap<Id, String>,
    i18n: &'a mut Localization,
}

impl RelayView<'_> {
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<RelayAction> {
        let mut action = None;
        Frame::new()
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
                        if let Some(relay_to_remove) = self.show_relays(ui) {
                            action = Some(RelayAction::Remove(relay_to_remove));
                        }
                        ui.add_space(8.0);
                        if let Some(relay_to_add) = self.show_add_relay_ui(ui) {
                            action = Some(RelayAction::Add(relay_to_add));
                        }
                    });
            });

        action
    }

    pub fn scroll_id() -> egui::Id {
        egui::Id::new("relay_scroll")
    }
}

impl<'a> RelayView<'a> {
    pub fn new(
        pool: &'a RelayPool,
        id_string_map: &'a mut HashMap<Id, String>,
        i18n: &'a mut Localization,
    ) -> Self {
        RelayView {
            pool,
            id_string_map,
            i18n,
        }
    }

    pub fn panel(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui.ctx(), |ui| self.ui(ui));
    }

    /// Show the current relays and return a relay the user selected to delete
    fn show_relays(&mut self, ui: &mut Ui) -> Option<String> {
        let mut relay_to_remove = None;
        for (index, relay_info) in get_relay_infos(self.pool).iter().enumerate() {
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
                                        .id_salt(index)
                                        .max_width(
                                            ui.max_rect().width()
                                                - get_right_side_width(relay_info.status),
                                        ) // TODO: refactor to dynamically check the size of the 'right to left' portion and set the max width to be the screen width minus padding minus 'right to left' width
                                        .show(ui, |ui| {
                                            ui.label(
                                                RichText::new(relay_info.relay_url)
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
                            if ui.add(delete_button(ui.visuals().dark_mode)).clicked() {
                                relay_to_remove = Some(relay_info.relay_url.to_string());
                            };

                            show_connection_status(ui, self.i18n, relay_info.status);
                        });
                    });
                });
            });
        }
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
            let is_enabled = self.pool.is_valid_url(text_buffer);
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

struct RelayInfo<'a> {
    pub relay_url: &'a str,
    pub status: RelayStatus,
}

fn get_relay_infos(pool: &RelayPool) -> Vec<RelayInfo> {
    pool.relays
        .iter()
        .map(|relay| RelayInfo {
            relay_url: relay.url(),
            status: relay.status(),
        })
        .collect()
}

// PREVIEWS

mod preview {
    use super::*;
    use crate::test_data::sample_pool;
    use notedeck::{App, AppAction, AppContext};

    pub struct RelayViewPreview {
        pool: RelayPool,
    }

    impl RelayViewPreview {
        fn new() -> Self {
            RelayViewPreview {
                pool: sample_pool(),
            }
        }
    }

    impl App for RelayViewPreview {
        fn update(&mut self, app: &mut AppContext<'_>, ui: &mut egui::Ui) -> Option<AppAction> {
            self.pool.try_recv();
            let mut id_string_map = HashMap::new();
            RelayView::new(app.pool, &mut id_string_map, app.i18n).ui(ui);
            None
        }
    }

    impl Preview for RelayView<'_> {
        type Prev = RelayViewPreview;

        fn preview(_cfg: PreviewConfig) -> Self::Prev {
            RelayViewPreview::new()
        }
    }
}
