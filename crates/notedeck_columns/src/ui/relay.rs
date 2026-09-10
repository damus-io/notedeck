use std::collections::HashMap;

use egui::{
    Align, Button, Color32, Frame, Id, Layout, Margin, Rect, RichText, Sense, Ui, UiBuilder, Vec2,
};
use egui_virtual_list::VirtualList;
use enostr::{NormRelayUrl, RelayStatus};
use notedeck::{
    tr, DragResponse, Localization, NotedeckTextStyle, RelayAction, RelayInspectApi,
    RelayInspectEntry, RelaySpec,
};
use notedeck_ui::app_images;
use notedeck_ui::{
    colors::{GREEN, PINK},
    padding,
};
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
struct RelayRow<'a> {
    relay_url: &'a NormRelayUrl,
    status: RelayStatus,
}

/// Which relay list a row belongs to, controlling whether/how it can be removed.
#[derive(Clone, Copy, PartialEq)]
enum RelaySection {
    /// Advertised NIP-65 relays (kind 10002); deletable via [`RelayAction::Remove`].
    Advertised,
    /// Active-but-not-advertised relays; not editable.
    Other,
    /// kind-10013 NIP-37 private-sync relays; deletable via [`RelayAction::RemovePrivate`].
    Private,
}

enum RelayListItem<'a> {
    SectionHeader(&'a str),
    EmptySection,
    Row {
        row: &'a RelayRow<'a>,
        section: RelaySection,
        /// Last row of its section: suppresses the separator so groups stay distinct.
        last: bool,
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

    /// Show active relay websockets, grouped by whether the relay is advertised by the selected account.
    fn show_relays(&mut self, ui: &mut Ui) -> Option<RelayAction> {
        let relay_infos = self.relay_inspect.relay_infos();
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
                    RelayListItem::Row { row, section, last } => {
                        let row_action = show_relay_row(ui, row, *section, *last, i18n);
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

/// Height of one relay list row.
const ROW_HEIGHT: f32 = 38.0;
/// Horizontal breathing room between a row's edge and its first/last element.
const ROW_PADDING: f32 = 8.0;
/// Left edge of a row's text, past the status dot.
const ROW_TEXT_INSET: f32 = ROW_PADDING + STATUS_DOT_RADIUS * 2.0 + 10.0;
const STATUS_DOT_RADIUS: f32 = 4.0;
/// Square hit area reserved on the right of every row for the remove button.
const ICON_SIZE: f32 = 24.0;

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

    let last_index = rows.len() - 1;
    items.extend(
        rows.iter()
            .enumerate()
            .map(|(index, row)| RelayListItem::Row {
                row,
                section,
                last: index == last_index,
            }),
    );
}

fn show_relay_section_header(ui: &mut Ui, title: &str) {
    ui.add_space(20.0);
    ui.horizontal(|ui| {
        ui.add_space(ROW_PADDING);
        ui.label(
            RichText::new(title)
                .text_style(NotedeckTextStyle::Small.text_style())
                .color(ui.visuals().weak_text_color())
                .strong(),
        );
    });
    ui.add_space(6.0);
}

fn show_empty_relay_section(ui: &mut Ui, i18n: &mut Localization) {
    ui.horizontal(|ui| {
        ui.add_space(ROW_TEXT_INSET);
        ui.label(
            RichText::new(tr!(i18n, "None", "Empty relay section placeholder"))
                .text_style(NotedeckTextStyle::Small.text_style())
                .color(ui.visuals().weak_text_color()),
        );
    });
}

/// Render one relay as a single-line list row: status dot, url, remove button.
///
/// The row is laid out from an explicitly allocated rect rather than nested
/// layouts so the url always truncates against a fixed right gutter, whether or
/// not the section has a remove button.
fn show_relay_row(
    ui: &mut Ui,
    relay_row: &RelayRow,
    section: RelaySection,
    last: bool,
    i18n: &mut Localization,
) -> Option<RelayAction> {
    let mut action = None;
    let relay_url = relay_row.relay_url.as_str();
    let removable = section != RelaySection::Other;

    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_HEIGHT), Sense::hover());

    if response.hovered() {
        ui.painter().rect_filled(
            rect,
            ui.visuals().widgets.active.corner_radius,
            ui.visuals().widgets.hovered.weak_bg_fill,
        );
    } else if !last {
        let separator_y = rect.bottom() - 0.5;
        ui.painter().hline(
            (rect.left() + ROW_TEXT_INSET)..=(rect.right() - ROW_PADDING),
            separator_y,
            ui.visuals().widgets.noninteractive.bg_stroke,
        );
    }

    ui.painter().circle_filled(
        egui::pos2(
            rect.left() + ROW_PADDING + STATUS_DOT_RADIUS,
            rect.center().y,
        ),
        STATUS_DOT_RADIUS,
        status_color(ui, relay_row.status),
    );

    let gutter = Rect::from_center_size(
        egui::pos2(
            rect.right() - ROW_PADDING - ICON_SIZE / 2.0,
            rect.center().y,
        ),
        egui::Vec2::splat(ICON_SIZE),
    );

    if removable {
        let mut gutter_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(gutter)
                .layout(Layout::centered_and_justified(egui::Direction::TopDown)),
        );
        if gutter_ui
            .add(delete_button(gutter_ui.visuals().dark_mode))
            .clicked()
        {
            action = section.remove_action(relay_url.to_owned());
        }
    }

    let text_rect = Rect::from_min_max(
        egui::pos2(rect.left() + ROW_TEXT_INSET, rect.top()),
        egui::pos2(gutter.left() - 8.0, rect.bottom()),
    );
    let mut text_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(text_rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    text_ui.add(
        egui::Label::new(
            RichText::new(relay_display_name(relay_url))
                .text_style(NotedeckTextStyle::Body.text_style()),
        )
        .selectable(false)
        .truncate(),
    );

    response.on_hover_ui(|ui| {
        ui.label(relay_url);
        ui.label(
            RichText::new(status_label(i18n, relay_row.status))
                .color(status_color(ui, relay_row.status)),
        );
    });

    action
}

/// Drop the `wss://` scheme and trailing slash so the host reads as the row's title.
///
/// `ws://` is left intact: an unencrypted relay is worth showing.
fn relay_display_name(url: &str) -> &str {
    let host = url.strip_prefix("wss://").unwrap_or(url);
    host.strip_suffix('/').unwrap_or(host)
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
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(ROW_PADDING);
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

fn relay_rows<'a>(
    relay_infos: impl IntoIterator<Item = RelayInspectEntry<'a>>,
    advertised_relays: &'a std::collections::BTreeSet<RelaySpec>,
    private_relays: &'a std::collections::BTreeSet<NormRelayUrl>,
) -> (Vec<RelayRow<'a>>, Vec<RelayRow<'a>>, Vec<RelayRow<'a>>) {
    let advertised_urls = advertised_relays
        .iter()
        .map(|relay| &relay.url)
        .collect::<std::collections::BTreeSet<_>>();
    let mut advertised = Vec::new();
    let mut private = Vec::new();
    let mut outbox_other = Vec::new();

    for relay_info in relay_infos {
        if relay_info.status == RelayStatus::Disconnected {
            continue;
        }

        let mut matched = false;
        if advertised_urls.contains(&relay_info.relay_url) {
            advertised.push(RelayRow {
                relay_url: relay_info.relay_url,
                status: relay_info.status,
            });
            matched = true;
        }
        if private_relays.contains(relay_info.relay_url) {
            private.push(RelayRow {
                relay_url: relay_info.relay_url,
                status: relay_info.status,
            });
            matched = true;
        }
        if !matched {
            outbox_other.push(RelayRow {
                relay_url: relay_info.relay_url,
                status: relay_info.status,
            });
        }
    }

    advertised.sort_by(|left, right| left.relay_url.cmp(right.relay_url));
    private.sort_by(|left, right| left.relay_url.cmp(right.relay_url));
    outbox_other.sort_by(|left, right| left.relay_url.cmp(right.relay_url));

    (advertised, private, outbox_other)
}

fn add_relay_button(label: String) -> Button<'static> {
    Button::image_and_text(
        app_images::add_relay_image().fit_to_exact_size(Vec2::splat(ICON_SIZE)),
        RichText::new(label)
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

    egui::Button::image(img.max_width(14.0).tint(Color32::from_white_alpha(150))).frame(false)
}

/// The dot color standing in for a relay's connection state.
fn status_color(ui: &Ui, status: RelayStatus) -> Color32 {
    match status {
        RelayStatus::Connected => GREEN,
        RelayStatus::Connecting => ui.visuals().warn_fg_color,
        RelayStatus::Disconnected => ui.visuals().error_fg_color,
    }
}

/// The localized name of a relay's connection state, shown on hover.
fn status_label(i18n: &mut Localization, status: RelayStatus) -> String {
    match status {
        RelayStatus::Connected => tr!(i18n, "Connected", "Status label for connected relay"),
        RelayStatus::Connecting => tr!(i18n, "Connecting...", "Status label for connecting relay"),
        RelayStatus::Disconnected => {
            tr!(i18n, "Not Connected", "Status label for disconnected relay")
        }
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
    fn relay_rows_only_shows_active_relay_infos() {
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
            vec![RelayRow {
                relay_url: &advertised_active,
                status: RelayStatus::Connected,
            }]
        );
        assert_eq!(
            private,
            vec![RelayRow {
                relay_url: &private_active,
                status: RelayStatus::Connected,
            }]
        );
        assert_eq!(
            other,
            vec![RelayRow {
                relay_url: &other_active,
                status: RelayStatus::Connecting,
            }]
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

        let advertised_relays = BTreeSet::new();
        let private_relays = BTreeSet::new();
        let (_, _, other) = relay_rows(relay_infos, &advertised_relays, &private_relays);

        assert_eq!(
            other,
            vec![
                RelayRow {
                    relay_url: &other_a,
                    status: RelayStatus::Connecting,
                },
                RelayRow {
                    relay_url: &other_b,
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
                relay_url: &relay,
                status: RelayStatus::Connected,
            }]
        );
        assert_eq!(
            private,
            vec![RelayRow {
                relay_url: &relay,
                status: RelayStatus::Connected,
            }]
        );
        assert!(other.is_empty());
    }
}
