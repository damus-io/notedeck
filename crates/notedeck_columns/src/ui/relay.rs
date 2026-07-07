use std::collections::HashMap;

use egui::{Align, Button, CornerRadius, Frame, Id, Layout, Margin, Rgba, RichText, Ui, Vec2};
use egui_virtual_list::VirtualList;
use enostr::{NormRelayUrl, RelayStatus};
use notedeck::{
    tr, DragResponse, Localization, NotedeckTextStyle, RelayAction, RelayInspectApi,
    RelayInspectEntry, RelaySpec,
};
use notedeck_ui::app_images;
use notedeck_ui::{colors::PINK, padding};
use tracing::debug;

use super::widgets::styled_button;

pub struct RelayView<'a> {
    relay_inspect: RelayInspectApi<'a>,
    advertised_relays: &'a std::collections::BTreeSet<RelaySpec>,
    private_relays: &'a std::collections::BTreeSet<NormRelayUrl>,
    relay_state: &'a mut RelayViewState,
    id_string_map: &'a mut HashMap<Id, String>,
    i18n: &'a mut Localization,
}

/// UI state for the relay inventory list.
#[derive(Default)]
pub struct RelayViewState {
    list: VirtualList,
    item_count: usize,
}

impl RelayViewState {
    fn list_for_item_count(&mut self, item_count: usize) -> &mut VirtualList {
        if self.item_count != item_count {
            self.list.reset();
            self.item_count = item_count;
        }
        &mut self.list
    }
}

#[derive(Debug, Eq, PartialEq)]
struct RelayRow {
    relay_url: String,
    status: RelayStatus,
}

/// Which relay list a row belongs to, controlling whether/how it can be removed.
#[derive(Clone, Copy, PartialEq)]
enum RelaySection {
    /// Advertised NIP-65 relays (kind 10002); deletable via [`RelayAction::Remove`].
    Advertised,
    /// Connected-but-not-advertised relays; not editable.
    Other,
    /// kind-10013 NIP-37 private-sync relays; deletable via [`RelayAction::RemovePrivate`].
    Private,
}

enum RelayListItem<'a> {
    SectionHeader(&'a str),
    EmptySection,
    Row {
        row: &'a RelayRow,
        section: RelaySection,
    },
    AddPrivateRelay(&'a str),
}

impl RelaySection {
    /// The remove action for a row in this section, if it can be removed.
    fn remove_action(self, url: String) -> Option<RelayAction> {
        match self {
            RelaySection::Advertised => Some(RelayAction::Remove(url)),
            RelaySection::Private => Some(RelayAction::RemovePrivate(url)),
            RelaySection::Other => None,
        }
    }
}

impl RelayView<'_> {
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
                        let mut action = self.show_relays(ui);
                        ui.add_space(8.0);
                        if let Some(relay_to_add) = self.show_add_relay_ui(ui) {
                            action = action.or(Some(RelayAction::Add(relay_to_add)));
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

impl<'a> RelayView<'a> {
    pub fn new(
        relay_inspect: RelayInspectApi<'a>,
        advertised_relays: &'a std::collections::BTreeSet<RelaySpec>,
        private_relays: &'a std::collections::BTreeSet<NormRelayUrl>,
        relay_state: &'a mut RelayViewState,
        id_string_map: &'a mut HashMap<Id, String>,
        i18n: &'a mut Localization,
    ) -> Self {
        RelayView {
            relay_inspect,
            advertised_relays,
            private_relays,
            relay_state,
            id_string_map,
            i18n,
        }
    }

    pub fn panel(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui.ctx(), |ui| self.ui(ui));
    }

    /// Show known relay websockets, grouped by whether the relay is advertised by the selected account.
    fn show_relays(&mut self, ui: &mut Ui) -> Option<RelayAction> {
        let relay_infos = self.relay_inspect.known_relay_infos();
        let (advertised, private, outbox_other) =
            relay_rows(relay_infos, self.advertised_relays, self.private_relays);

        let mut action = None;
        let advertised_label = tr!(
            self.i18n,
            "Advertised",
            "Section header for advertised relays"
        );
        let private_label = tr!(
            self.i18n,
            "Private sync",
            "Section header for private sync relays"
        );
        let outbox_other_label = tr!(
            self.i18n,
            "Other",
            "Section header for non-advertised connected relays"
        );
        let add_private_label = tr!(
            self.i18n,
            "Add private relay",
            "Button label to add a private sync relay"
        );

        let mut items = Vec::with_capacity(
            advertised.len()
                + private.len()
                + outbox_other.len()
                + RELAY_SECTION_ITEM_COUNT * 3
                + 1,
        );
        push_relay_section_items(
            &mut items,
            &advertised_label,
            &advertised,
            RelaySection::Advertised,
        );
        push_relay_section_items(&mut items, &private_label, &private, RelaySection::Private);
        items.push(RelayListItem::AddPrivateRelay(&add_private_label));
        push_relay_section_items(
            &mut items,
            &outbox_other_label,
            &outbox_other,
            RelaySection::Other,
        );

        let i18n = &mut *self.i18n;
        let id_string_map = &mut *self.id_string_map;
        let item_count = items.len();
        self.relay_state
            .list_for_item_count(item_count)
            .ui_custom_layout(ui, item_count, |ui, index| {
                match &items[index] {
                    RelayListItem::SectionHeader(title) => show_relay_section_header(ui, title),
                    RelayListItem::EmptySection => show_empty_relay_section(ui, i18n),
                    RelayListItem::Row { row, section } => {
                        let row_action = show_relay_row(ui, row, *section, i18n);
                        if action.is_none() {
                            action = row_action;
                        }
                    }
                    RelayListItem::AddPrivateRelay(label) => {
                        let add_action = show_add_relay_entry_ui(
                            ui,
                            id_string_map,
                            i18n,
                            "add-private-relay)",
                            (*label).to_owned(),
                        )
                        .map(RelayAction::AddPrivate);
                        if action.is_none() {
                            action = add_action;
                        }
                    }
                }
                1
            });

        action
    }

    fn show_add_relay_ui(&mut self, ui: &mut Ui) -> Option<String> {
        let label = tr!(self.i18n, "Add relay", "Button label to add a relay");
        show_add_relay_entry_ui(ui, self.id_string_map, self.i18n, "add-relay)", label)
    }
}

const RELAY_PREFILL: &str = "wss://";
const RELAY_SECTION_ITEM_COUNT: usize = 2;

fn push_relay_section_items<'a>(
    items: &mut Vec<RelayListItem<'a>>,
    title: &'a str,
    rows: &'a [RelayRow],
    section: RelaySection,
) {
    items.push(RelayListItem::SectionHeader(title));
    if rows.is_empty() {
        items.push(RelayListItem::EmptySection);
        return;
    }

    items.extend(rows.iter().map(|row| RelayListItem::Row { row, section }));
}

fn show_relay_section_header(ui: &mut Ui, title: &str) {
    ui.add_space(8.0);
    ui.label(
        RichText::new(title)
            .text_style(NotedeckTextStyle::Body.text_style())
            .strong(),
    );
    ui.add_space(4.0);
}

fn show_empty_relay_section(ui: &mut Ui, i18n: &mut Localization) {
    ui.label(
        RichText::new(tr!(i18n, "None", "Empty relay section placeholder"))
            .text_style(NotedeckTextStyle::Body.text_style())
            .weak(),
    );
}

fn show_relay_row(
    ui: &mut Ui,
    relay_row: &RelayRow,
    section: RelaySection,
    i18n: &mut Localization,
) -> Option<RelayAction> {
    let mut action = None;

    ui.add_space(8.0);
    ui.scope(|ui| {
        ui.set_min_width(ui.available_width());
        relay_frame(ui).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.vertical(|ui| {
                let text_height = ui.text_style_height(&NotedeckTextStyle::Monospace.text_style());
                let response = ui.add_sized(
                    [ui.available_width(), text_height],
                    egui::Label::new(
                        RichText::new(&relay_row.relay_url)
                            .text_style(NotedeckTextStyle::Monospace.text_style())
                            .color(ui.style().visuals.noninteractive().fg_stroke.color),
                    )
                    .selectable(false)
                    .truncate(),
                );
                response.on_hover_text(&relay_row.relay_url);

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    show_connection_status(ui, i18n, relay_row.status);

                    if section != RelaySection::Other {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.add(delete_button(ui.visuals().dark_mode)).clicked() {
                                action = section.remove_action(relay_row.relay_url.clone());
                            }
                        });
                    }
                });
            });
        });
    });

    action
}

fn show_add_relay_entry_ui(
    ui: &mut Ui,
    id_string_map: &mut HashMap<Id, String>,
    i18n: &mut Localization,
    id_key: &str,
    button_label: String,
) -> Option<String> {
    // Collapsed "add relay" button that expands into a relay-url entry. `id_key`
    // namespaces the entry's transient text buffer so multiple add fields do not
    // share state.
    let id = ui.id().with(id_key);
    match id_string_map.get(&id) {
        None => {
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                let relay_button = add_relay_button(button_label);
                if ui.add(relay_button).clicked() {
                    debug!("add relay clicked");
                    id_string_map.insert(id, RELAY_PREFILL.to_string());
                };
            });
            None
        }
        Some(_) => {
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                add_relay_entry(ui, id_string_map, i18n, id)
            })
            .inner
        }
    }
}

fn add_relay_entry(
    ui: &mut Ui,
    id_string_map: &mut HashMap<Id, String>,
    i18n: &mut Localization,
    id: Id,
) -> Option<String> {
    padding(16.0, ui, |ui| {
        let text_buffer = id_string_map
            .entry(id)
            .or_insert_with(|| RELAY_PREFILL.to_string());
        let is_enabled = NormRelayUrl::new(text_buffer).is_ok();
        let text_edit = egui::TextEdit::singleline(text_buffer)
            .hint_text(
                RichText::new(tr!(
                    i18n,
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
            .add_sized(egui::vec2(50.0, 40.0), add_relay_button2(i18n, is_enabled))
            .clicked()
        {
            id_string_map.remove(&id)
        } else {
            None
        }
    })
    .inner
}

fn relay_rows(
    relay_infos: Vec<RelayInspectEntry<'_>>,
    advertised_relays: &std::collections::BTreeSet<RelaySpec>,
    private_relays: &std::collections::BTreeSet<NormRelayUrl>,
) -> (Vec<RelayRow>, Vec<RelayRow>, Vec<RelayRow>) {
    let mut advertised = Vec::with_capacity(advertised_relays.len());
    let mut advertised_index = HashMap::with_capacity(advertised_relays.len());

    for (index, relay) in advertised_relays.iter().enumerate() {
        advertised_index.insert(relay.url.clone(), index);
        advertised.push(RelayRow {
            relay_url: relay.url.to_string(),
            status: RelayStatus::Disconnected,
        });
    }

    let mut private = Vec::with_capacity(private_relays.len());
    let mut private_index = HashMap::with_capacity(private_relays.len());

    for (index, relay_url) in private_relays.iter().enumerate() {
        private_index.insert(relay_url.clone(), index);
        private.push(RelayRow {
            relay_url: relay_url.to_string(),
            status: RelayStatus::Disconnected,
        });
    }

    let mut outbox_other = Vec::new();

    for relay_info in relay_infos {
        let mut matched = false;
        if let Some(index) = advertised_index.get(relay_info.relay_url) {
            advertised[*index].status = relay_info.status;
            matched = true;
        }
        if let Some(index) = private_index.get(relay_info.relay_url) {
            private[*index].status = relay_info.status;
            matched = true;
        }
        if !matched {
            outbox_other.push(RelayRow {
                relay_url: relay_info.relay_url.to_string(),
                status: relay_info.status,
            });
        }
    }

    outbox_other.sort_by(|left, right| left.relay_url.cmp(&right.relay_url));

    (advertised, private, outbox_other)
}

fn add_relay_button(label: String) -> Button<'static> {
    Button::image_and_text(
        app_images::add_relay_image().fit_to_exact_size(Vec2::new(48.0, 48.0)),
        RichText::new(label)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn relay_url(url: &str) -> NormRelayUrl {
        NormRelayUrl::new(url).expect("relay url")
    }

    fn relay_spec(url: &NormRelayUrl) -> RelaySpec {
        RelaySpec::new(url.clone(), false, false)
    }

    #[test]
    fn relay_rows_show_disconnected_advertised_and_private_relays() {
        let advertised_active = relay_url("wss://relay-advertised-active.example.com");
        let advertised_inactive = relay_url("wss://relay-advertised-inactive.example.com");
        let private_active = relay_url("wss://relay-private-active.example.com");
        let private_inactive = relay_url("wss://relay-private-inactive.example.com");
        let other_active = relay_url("wss://relay-other-active.example.com");
        let other_inactive = relay_url("wss://relay-other-inactive.example.com");
        let advertised_relays = BTreeSet::from([
            relay_spec(&advertised_active),
            relay_spec(&advertised_inactive),
        ]);
        let private_relays = BTreeSet::from([private_active.clone(), private_inactive.clone()]);
        let relay_infos = vec![
            RelayInspectEntry {
                relay_url: &advertised_active,
                status: RelayStatus::Connected,
            },
            RelayInspectEntry {
                relay_url: &private_active,
                status: RelayStatus::Connected,
            },
            RelayInspectEntry {
                relay_url: &other_active,
                status: RelayStatus::Connecting,
            },
            RelayInspectEntry {
                relay_url: &other_inactive,
                status: RelayStatus::Disconnected,
            },
        ];

        let (advertised, private, other) =
            relay_rows(relay_infos, &advertised_relays, &private_relays);

        assert_eq!(
            advertised,
            vec![
                RelayRow {
                    relay_url: advertised_active.to_string(),
                    status: RelayStatus::Connected,
                },
                RelayRow {
                    relay_url: advertised_inactive.to_string(),
                    status: RelayStatus::Disconnected,
                }
            ]
        );
        assert_eq!(
            private,
            vec![
                RelayRow {
                    relay_url: private_active.to_string(),
                    status: RelayStatus::Connected,
                },
                RelayRow {
                    relay_url: private_inactive.to_string(),
                    status: RelayStatus::Disconnected,
                }
            ]
        );
        assert_eq!(
            other,
            vec![
                RelayRow {
                    relay_url: other_active.to_string(),
                    status: RelayStatus::Connecting,
                },
                RelayRow {
                    relay_url: other_inactive.to_string(),
                    status: RelayStatus::Disconnected,
                }
            ]
        );
    }

    #[test]
    fn relay_rows_sort_other_relays_by_url() {
        let other_b = relay_url("wss://relay-b.example.com");
        let other_a = relay_url("wss://relay-a.example.com");
        let relay_infos = vec![
            RelayInspectEntry {
                relay_url: &other_b,
                status: RelayStatus::Connected,
            },
            RelayInspectEntry {
                relay_url: &other_a,
                status: RelayStatus::Connecting,
            },
        ];

        let (_, _, other) = relay_rows(relay_infos, &BTreeSet::new(), &BTreeSet::new());

        assert_eq!(
            other,
            vec![
                RelayRow {
                    relay_url: other_a.to_string(),
                    status: RelayStatus::Connecting,
                },
                RelayRow {
                    relay_url: other_b.to_string(),
                    status: RelayStatus::Connected,
                }
            ]
        );
    }

    #[test]
    fn relay_rows_updates_advertised_and_private_overlap() {
        let relay = relay_url("wss://relay-overlap.example.com");
        let advertised_relays = BTreeSet::from([relay_spec(&relay)]);
        let private_relays = BTreeSet::from([relay.clone()]);
        let relay_infos = vec![RelayInspectEntry {
            relay_url: &relay,
            status: RelayStatus::Connected,
        }];

        let (advertised, private, other) =
            relay_rows(relay_infos, &advertised_relays, &private_relays);

        assert_eq!(
            advertised,
            vec![RelayRow {
                relay_url: relay.to_string(),
                status: RelayStatus::Connected,
            }]
        );
        assert_eq!(
            private,
            vec![RelayRow {
                relay_url: relay.to_string(),
                status: RelayStatus::Connected,
            }]
        );
        assert!(other.is_empty());
    }
}
