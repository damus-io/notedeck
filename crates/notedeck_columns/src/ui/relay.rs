use crate::relay_pool_manager::{RelayPoolManager, RelayStatus};
use crate::ui::{Preview, PreviewConfig, View};
use egui::{Align, Button, Frame, Layout, Margin, Rgba, RichText, Rounding, Ui, Vec2};

use enostr::RelayPool;
use notedeck::NotedeckTextStyle;

pub struct RelayView<'a> {
    manager: RelayPoolManager<'a>,
}

impl View for RelayView<'_> {
    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(24.0);

        ui.horizontal(|ui| {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.label(
                    RichText::new("Relays").text_style(NotedeckTextStyle::Heading2.text_style()),
                );
            });

            // TODO: implement manually adding relays
            // ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            //     if ui.add(add_relay_button()).clicked() {
            //         // TODO: navigate to 'add relay view'
            //     };
            // });
        });

        ui.add_space(8.0);

        egui::ScrollArea::vertical()
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                if let Some(indices) = self.show_relays(ui) {
                    self.manager.remove_relays(indices);
                }
            });
    }
}

impl<'a> RelayView<'a> {
    pub fn new(manager: RelayPoolManager<'a>) -> Self {
        RelayView { manager }
    }

    pub fn panel(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui.ctx(), |ui| self.ui(ui));
    }

    /// Show the current relays, and returns the indices of relays the user requested to delete
    fn show_relays(&'a self, ui: &mut Ui) -> Option<Vec<usize>> {
        let mut indices_to_remove: Option<Vec<usize>> = None;
        for (index, relay_info) in self.manager.get_relay_infos().iter().enumerate() {
            ui.add_space(8.0);
            ui.vertical_centered_justified(|ui| {
                relay_frame(ui).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            Frame::none()
                                // This frame is needed to add margin because the label will be added to the outer frame first and centered vertically before the connection status is added so the vertical centering isn't accurate.
                                // TODO: remove this hack and actually center the url & status at the same time
                                .inner_margin(Margin::symmetric(0.0, 4.0))
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
                                indices_to_remove.get_or_insert_with(Vec::new).push(index);
                            };

                            show_connection_status(ui, relay_info.status);
                        });
                    });
                });
            });
        }

        indices_to_remove
    }
}

fn get_right_side_width(status: &RelayStatus) -> f32 {
    match status {
        RelayStatus::Connected => 150.0,
        RelayStatus::Connecting => 160.0,
        RelayStatus::Disconnected => 175.0,
    }
}

#[allow(unused)]
fn add_relay_button() -> egui::Button<'static> {
    Button::new("+ Add relay").min_size(Vec2::new(0.0, 32.0))
}

fn delete_button(_dark_mode: bool) -> egui::Button<'static> {
    /*
    let img_data = if dark_mode {
        egui::include_image!("../../assets/icons/delete_icon_4x.png")
    } else {
        // TODO: use light delete icon
        egui::include_image!("../../assets/icons/delete_icon_4x.png")
    };
    */
    let img_data = egui::include_image!("../../../../assets/icons/delete_icon_4x.png");

    egui::Button::image(egui::Image::new(img_data).max_width(10.0)).frame(false)
}

fn relay_frame(ui: &mut Ui) -> Frame {
    Frame::none()
        .inner_margin(Margin::same(8.0))
        .rounding(ui.style().noninteractive().rounding)
        .stroke(ui.style().visuals.noninteractive().bg_stroke)
}

fn show_connection_status(ui: &mut Ui, status: &RelayStatus) {
    let fg_color = match status {
        RelayStatus::Connected => ui.visuals().selection.bg_fill,
        RelayStatus::Connecting => ui.visuals().warn_fg_color,
        RelayStatus::Disconnected => ui.visuals().error_fg_color,
    };
    let bg_color = egui::lerp(Rgba::from(fg_color)..=Rgba::BLACK, 0.8).into();

    let label_text = match status {
        RelayStatus::Connected => "Connected",
        RelayStatus::Connecting => "Connecting...",
        RelayStatus::Disconnected => "Not Connected",
    };

    let frame = Frame::none()
        .rounding(Rounding::same(100.0))
        .fill(bg_color)
        .inner_margin(Margin::symmetric(12.0, 4.0));

    frame.show(ui, |ui| {
        ui.label(RichText::new(label_text).color(fg_color));
        ui.add(get_connection_icon(status));
    });
}

fn get_connection_icon(status: &RelayStatus) -> egui::Image<'static> {
    let img_data = match status {
        RelayStatus::Connected => {
            egui::include_image!("../../../../assets/icons/connected_icon_4x.png")
        }
        RelayStatus::Connecting => {
            egui::include_image!("../../../../assets/icons/connecting_icon_4x.png")
        }
        RelayStatus::Disconnected => {
            egui::include_image!("../../../../assets/icons/disconnected_icon_4x.png")
        }
    };

    egui::Image::new(img_data)
}

// PREVIEWS

mod preview {
    use super::*;
    use crate::test_data::sample_pool;

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

    impl View for RelayViewPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            self.pool.try_recv();
            RelayView::new(RelayPoolManager::new(&mut self.pool)).ui(ui);
        }
    }

    impl Preview for RelayView<'_> {
        type Prev = RelayViewPreview;

        fn preview(_cfg: PreviewConfig) -> Self::Prev {
            RelayViewPreview::new()
        }
    }
}
