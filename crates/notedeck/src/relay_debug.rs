use egui::ScrollArea;
use enostr::{RelayLogEvent, SubsDebug};

pub struct RelayDebugView<'a> {
    debug: &'a mut SubsDebug,
}

impl<'a> RelayDebugView<'a> {
    pub fn new(debug: &'a mut SubsDebug) -> Self {
        Self { debug }
    }
}

impl RelayDebugView<'_> {
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ScrollArea::vertical()
            .id_salt(ui.id().with("relays_debug"))
            .max_height(ui.max_rect().height() / 2.0)
            .show(ui, |ui| {
                ui.label("Active Relays:");
                for (relay_str, data) in self.debug.get_data() {
                    egui::CollapsingHeader::new(format!(
                        "{} {} {}",
                        relay_str,
                        format_total(&data.count),
                        format_sec(&data.count)
                    ))
                    .default_open(true)
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            for (i, sub_data) in data.sub_data.values().enumerate() {
                                ui.label(format!(
                                    "Filter {} ({})",
                                    i + 1,
                                    format_sec(&sub_data.count)
                                ))
                                .on_hover_cursor(egui::CursorIcon::Help)
                                .on_hover_text(sub_data.filter.to_string());
                            }
                        })
                    });
                }
            });

        ui.separator();
        egui::ComboBox::from_label("Show events from relay")
            .selected_text(
                self.debug
                    .relay_events_selection
                    .as_ref()
                    .map_or(String::new(), |s| s.clone()),
            )
            .show_ui(ui, |ui| {
                let mut make_selection = None;
                for relay in self.debug.get_data().keys() {
                    if ui
                        .selectable_label(
                            if let Some(s) = &self.debug.relay_events_selection {
                                *s == *relay
                            } else {
                                false
                            },
                            relay,
                        )
                        .clicked()
                    {
                        make_selection = Some(relay.clone());
                    }
                }
                if make_selection.is_some() {
                    self.debug.relay_events_selection = make_selection
                }
            });
        let show_relay_evs =
            |ui: &mut egui::Ui, relay: Option<String>, events: Vec<RelayLogEvent>| {
                for ev in events {
                    ui.horizontal_wrapped(|ui| {
                        if let Some(r) = &relay {
                            ui.label("relay").on_hover_text(r.clone());
                        }
                        match ev {
                            RelayLogEvent::Send(client_message) => {
                                ui.label("SEND: ");
                                let msg = &match client_message {
                                    enostr::ClientMessage::Event { .. } => "Event",
                                    enostr::ClientMessage::Req { .. } => "Req",
                                    enostr::ClientMessage::Close { .. } => "Close",
                                    enostr::ClientMessage::Raw(_) => "Raw",
                                };

                                if let Ok(json) = client_message.to_json() {
                                    ui.label(*msg).on_hover_text(json)
                                } else {
                                    ui.label(*msg)
                                }
                            }
                            RelayLogEvent::Recieve(e) => {
                                ui.label("RECIEVE: ");
                                match e {
                                    enostr::OwnedRelayEvent::Opened => ui.label("Opened"),
                                    enostr::OwnedRelayEvent::Closed => ui.label("Closed"),
                                    enostr::OwnedRelayEvent::Other(s) => {
                                        ui.label("Other").on_hover_text(s)
                                    }
                                    enostr::OwnedRelayEvent::Error(s) => {
                                        ui.label("Error").on_hover_text(s)
                                    }
                                    enostr::OwnedRelayEvent::Message(s) => {
                                        ui.label("Message").on_hover_text(s)
                                    }
                                }
                            }
                        }
                    });
                }
            };

        ScrollArea::vertical()
            .id_salt(ui.id().with("events"))
            .show(ui, |ui| {
                if let Some(relay) = &self.debug.relay_events_selection {
                    if let Some(data) = self.debug.get_data().get(relay) {
                        show_relay_evs(ui, None, data.events.clone());
                    }
                } else {
                    for (relay, data) in self.debug.get_data() {
                        show_relay_evs(ui, Some(relay.clone()), data.events.clone());
                    }
                }
            });

        self.debug.try_increment_stats();
    }

    pub fn window(ctx: &egui::Context, debug: &mut SubsDebug) {
        let mut open = true;
        egui::Window::new("Relay Debugger")
            .open(&mut open)
            .show(ctx, |ui| {
                RelayDebugView::new(debug).ui(ui);
            });
    }
}

fn format_sec(c: &enostr::TransferStats) -> String {
    format!(
        "⬇{} ⬆️{}",
        byte_to_string(c.down_sec_prior),
        byte_to_string(c.up_sec_prior)
    )
}

fn format_total(c: &enostr::TransferStats) -> String {
    format!(
        "total: ⬇{} ⬆️{}",
        byte_to_string(c.down_total),
        byte_to_string(c.up_total)
    )
}

const MB: usize = 1_048_576;
const KB: usize = 1024;
fn byte_to_string(b: usize) -> String {
    if b >= MB {
        let mbs = b as f32 / MB as f32;
        format!("{mbs:.2} MB")
    } else if b >= KB {
        let kbs = b as f32 / KB as f32;
        format!("{kbs:.2} KB")
    } else {
        format!("{b} B")
    }
}
