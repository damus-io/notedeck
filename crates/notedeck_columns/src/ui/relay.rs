use std::collections::HashMap;

use egui::{
    Align, Button, Color32, Frame, Id, Layout, Margin, Rect, RichText, Sense, Ui, UiBuilder, Vec2,
};
use egui_virtual_list::VirtualList;
use enostr::{NormRelayUrl, RelayStatus};
use notedeck::{
    tr, DragResponse, Localization, NotedeckTextStyle, PrivateRelayRole, PrivateRelaySet,
    RelayAction, RelayInspectApi, RelayInspectEntry, RelaySpec,
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
    private_relays: &'a PrivateRelaySet,
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

/// Active relays split by the list each belongs to, each sorted by URL.
struct RelaySections<'a> {
    /// In the selected account's advertised (NIP-65) relay list.
    advertised: Vec<RelayRow<'a>>,
    /// In the private-sync relay list: every one, connected or not, since an
    /// encrypted-only relay syncs through the host's own session rather than
    /// the relay pool, and must stay reachable to toggle or remove.
    private: Vec<RelayRow<'a>>,
    /// In neither list, e.g. outbox relays.
    other: Vec<RelayRow<'a>>,
}

#[derive(Debug, Eq, PartialEq)]
struct RelayRow<'a> {
    relay_url: &'a NormRelayUrl,
    status: RelayStatus,
    /// The relay's private-sync role, on rows of the private section only.
    private_role: Option<PrivateRelayRole>,
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
        egui::Id::unique("relay_scroll")
    }
}

impl<'a> RelayView<'a> {
    pub fn new(
        relay_inspect: RelayInspectApi<'a>,
        advertised_relays: &'a std::collections::BTreeSet<RelaySpec>,
        private_relays: &'a PrivateRelaySet,
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
        egui::CentralPanel::default().show(ui, |ui| self.ui(ui));
    }

    /// Show active relay websockets, grouped by whether the relay is advertised by the selected account.
    fn show_relays(&mut self, ui: &mut Ui) -> Option<RelayAction> {
        let relay_infos = self.relay_inspect.relay_infos();
        let RelaySections {
            advertised,
            private,
            other: outbox_other,
        } = relay_rows(relay_infos, self.advertised_relays, self.private_relays);

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

/// Render one relay as a single-line list row: status dot, url, remove button,
/// and for a private relay an "encrypted only" toggle beside it.
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

    let mut text_right = gutter.left() - 8.0;
    if let Some(role) = relay_row.private_role {
        let toggle_rect = Rect::from_min_max(
            egui::pos2(rect.left() + ROW_TEXT_INSET, rect.top()),
            egui::pos2(text_right, rect.bottom()),
        );
        let mut toggle_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(toggle_rect)
                .layout(Layout::right_to_left(Align::Center)),
        );
        let toggle = encrypted_only_toggle(&mut toggle_ui, role, i18n);
        text_right = toggle.rect.left() - 8.0;
        if toggle.clicked() {
            action = Some(RelayAction::SetPrivateRole {
                url: relay_url.to_owned(),
                role: toggled_role(role),
            });
        }
    }

    let text_rect = Rect::from_min_max(
        egui::pos2(rect.left() + ROW_TEXT_INSET, rect.top()),
        egui::pos2(text_right, rect.bottom()),
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

/// The small "encrypted only" chip on a private relay row: selected when the
/// relay may only be sent encrypted envelopes. Its hover says what a click does.
fn encrypted_only_toggle(
    ui: &mut Ui,
    role: PrivateRelayRole,
    i18n: &mut Localization,
) -> egui::Response {
    let encrypted_only = role == PrivateRelayRole::EncryptedOnly;
    let label = tr!(
        i18n,
        "Encrypted only",
        "Toggle on a private relay row: send this relay only encrypted envelopes"
    );
    let hover = if encrypted_only {
        tr!(
            i18n,
            "Only encrypted envelopes go to this relay. Click to also sync plaintext private data here.",
            "Hover text on an encrypted-only private relay's toggle"
        )
    } else {
        tr!(
            i18n,
            "Plaintext private data syncs here too. Click to send this relay only encrypted envelopes, e.g. a relay shared with a team.",
            "Hover text on a private relay's encrypted-only toggle when it is off"
        )
    };
    ui.add(Button::selectable(
        encrypted_only,
        RichText::new(label).text_style(NotedeckTextStyle::Small.text_style()),
    ))
    .on_hover_text(hover)
}

/// The role a click on the encrypted-only toggle switches to.
fn toggled_role(role: PrivateRelayRole) -> PrivateRelayRole {
    match role {
        PrivateRelayRole::Full => PrivateRelayRole::EncryptedOnly,
        PrivateRelayRole::EncryptedOnly => PrivateRelayRole::Full,
    }
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
    let id = ui.scope_id().with(id_key);
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
            .align(egui::Align2::LEFT_CENTER)
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
    private_relays: &'a PrivateRelaySet,
) -> RelaySections<'a> {
    let advertised_urls = advertised_relays
        .iter()
        .map(|relay| &relay.url)
        .collect::<std::collections::BTreeSet<_>>();
    let mut advertised = Vec::new();
    let mut outbox_other = Vec::new();
    // Every private relay gets a row, connected or not: an encrypted-only one
    // is synced by the host's own session and may never appear in the pool.
    let mut pool_status = std::collections::BTreeMap::new();

    for relay_info in relay_infos {
        if relay_info.status == RelayStatus::Disconnected {
            continue;
        }

        let mut matched = false;
        if advertised_urls.contains(&relay_info.relay_url) {
            advertised.push(RelayRow {
                relay_url: relay_info.relay_url,
                status: relay_info.status,
                private_role: None,
            });
            matched = true;
        }
        if private_relays.contains(relay_info.relay_url) {
            pool_status.insert(relay_info.relay_url, relay_info.status);
            matched = true;
        }
        if !matched {
            outbox_other.push(RelayRow {
                relay_url: relay_info.relay_url,
                status: relay_info.status,
                private_role: None,
            });
        }
    }

    // Already sorted by URL: the set iterates in order.
    let private = private_relays
        .urls()
        .map(|relay_url| RelayRow {
            relay_url,
            status: pool_status
                .get(relay_url)
                .copied()
                .unwrap_or(RelayStatus::Disconnected),
            private_role: private_relays.role(relay_url),
        })
        .collect();

    advertised.sort_by(|left, right| left.relay_url.cmp(right.relay_url));
    outbox_other.sort_by(|left, right| left.relay_url.cmp(right.relay_url));

    RelaySections {
        advertised,
        private,
        other: outbox_other,
    }
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

    egui::Button::new(img.max_width(14.0).tint(Color32::from_white_alpha(150))).frame(false)
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

    /// A private relay set of full (plaintext-carrying) relays.
    fn private_set(urls: &[&NormRelayUrl]) -> PrivateRelaySet {
        urls.iter()
            .map(|url| notedeck::PrivateRelay {
                url: (*url).clone(),
                role: PrivateRelayRole::Full,
            })
            .collect()
    }

    /// Advertised and other rows come from the pool's active relays only; the
    /// private section lists every private relay, a disconnected one included.
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
        let private_relays = private_set(&[&private_active, &private_inactive]);
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

        let RelaySections {
            advertised,
            private,
            other,
        } = relay_rows(relay_infos, &advertised_relays, &private_relays);

        assert_eq!(
            advertised,
            vec![RelayRow {
                relay_url: &advertised_active,
                status: RelayStatus::Connected,
                private_role: None,
            }]
        );
        assert_eq!(
            private,
            vec![
                RelayRow {
                    relay_url: &private_active,
                    status: RelayStatus::Connected,
                    private_role: Some(PrivateRelayRole::Full),
                },
                RelayRow {
                    relay_url: &private_inactive,
                    status: RelayStatus::Disconnected,
                    private_role: Some(PrivateRelayRole::Full),
                }
            ]
        );
        assert_eq!(
            other,
            vec![RelayRow {
                relay_url: &other_active,
                status: RelayStatus::Connecting,
                private_role: None,
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
        let private_relays = PrivateRelaySet::default();
        let RelaySections { other, .. } =
            relay_rows(relay_infos, &advertised_relays, &private_relays);

        assert_eq!(
            other,
            vec![
                RelayRow {
                    relay_url: &other_a,
                    status: RelayStatus::Connecting,
                    private_role: None,
                },
                RelayRow {
                    relay_url: &other_b,
                    status: RelayStatus::Connected,
                    private_role: None,
                }
            ]
        );
    }

    #[test]
    fn relay_rows_updates_advertised_and_private_overlap() {
        let relay = relay_url("wss://relay-overlap.example.com");
        let advertised_relays = BTreeSet::from([relay_spec(&relay)]);
        let private_relays = private_set(&[&relay]);
        let relay_infos = vec![RelayInspectEntry {
            relay_url: &relay,
            status: RelayStatus::Connected,
        }];

        let RelaySections {
            advertised,
            private,
            other,
        } = relay_rows(relay_infos, &advertised_relays, &private_relays);

        assert_eq!(
            advertised,
            vec![RelayRow {
                relay_url: &relay,
                status: RelayStatus::Connected,
                private_role: None,
            }]
        );
        assert_eq!(
            private,
            vec![RelayRow {
                relay_url: &relay,
                status: RelayStatus::Connected,
                private_role: Some(PrivateRelayRole::Full),
            }]
        );
        assert!(other.is_empty());
    }

    /// An encrypted-only private relay keeps its role on its row, and the
    /// toggle flips between the two roles.
    #[test]
    fn relay_rows_carry_private_role() {
        let sns = relay_url("wss://sns.example.com");
        let private_relays: PrivateRelaySet = [notedeck::PrivateRelay {
            url: sns.clone(),
            role: PrivateRelayRole::EncryptedOnly,
        }]
        .into_iter()
        .collect();
        let advertised_relays = BTreeSet::new();

        let RelaySections { private, other, .. } =
            relay_rows(Vec::new(), &advertised_relays, &private_relays);

        assert_eq!(
            private,
            vec![RelayRow {
                relay_url: &sns,
                status: RelayStatus::Disconnected,
                private_role: Some(PrivateRelayRole::EncryptedOnly),
            }]
        );
        assert!(other.is_empty());
        assert_eq!(
            toggled_role(PrivateRelayRole::EncryptedOnly),
            PrivateRelayRole::Full
        );
        assert_eq!(
            toggled_role(PrivateRelayRole::Full),
            PrivateRelayRole::EncryptedOnly
        );
    }
}
