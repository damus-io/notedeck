use std::collections::HashSet;

use egui::{Align, Color32, CornerRadius, Label, RichText, Stroke, TextEdit};
use enostr::Pubkey;
use nostrdb::{Ndb, Transaction};
use notedeck::{
    name::get_display_name, tr, ContactState, Images, Localization, MediaJobSender,
    NotedeckTextStyle,
};
use notedeck_ui::{
    contacts_list::ContactsCollection, icons::search_icon, profile_row, ContactsListView,
};

use crate::cache::CreateConvoState;

pub struct CreateConvoUi<'a> {
    ndb: &'a Ndb,
    jobs: &'a MediaJobSender,
    img_cache: &'a mut Images,
    contacts: &'a ContactState,
    i18n: &'a mut Localization,
    state: &'a mut CreateConvoState,
}

pub struct CreateConvoResponse {
    pub recipient: Pubkey,
}

impl<'a> CreateConvoUi<'a> {
    pub fn new(
        ndb: &'a Ndb,
        jobs: &'a MediaJobSender,
        img_cache: &'a mut Images,
        contacts: &'a ContactState,
        i18n: &'a mut Localization,
        state: &'a mut CreateConvoState,
    ) -> Self {
        Self {
            ndb,
            jobs,
            img_cache,
            contacts,
            i18n,
            state,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<CreateConvoResponse> {
        let contacts_set = match self.contacts {
            ContactState::Received { contacts, .. } => Some(contacts),
            _ => None,
        };

        let txn = Transaction::new(self.ndb).expect("txn");

        // Search input
        ui.add_space(8.0);
        search_input(&mut self.state.query, self.i18n, ui);
        ui.add_space(12.0);

        let query = self.state.query.trim();

        if query.is_empty() {
            // Show contacts list when not searching
            ui.add(Label::new(
                RichText::new(tr!(
                    self.i18n,
                    "Contacts",
                    "Heading shown when choosing a contact to start a new chat"
                ))
                .text_style(NotedeckTextStyle::Heading.text_style()),
            ));

            if let Some(contacts) = contacts_set {
                let resp = ContactsListView::new(
                    ContactsCollection::Set(contacts),
                    self.jobs,
                    self.ndb,
                    self.img_cache,
                    &txn,
                    self.i18n,
                )
                .ui(ui);

                resp.output.map(|a| match a {
                    notedeck_ui::ContactsListAction::Select(pubkey) => {
                        CreateConvoResponse { recipient: pubkey }
                    }
                })
            } else {
                // No contacts yet
                ui.label(tr!(
                    self.i18n,
                    "No contacts yet",
                    "Shown when user has no contacts to display"
                ));
                None
            }
        } else {
            // Show search results
            ui.add(Label::new(
                RichText::new(tr!(
                    self.i18n,
                    "Results",
                    "Heading shown above search results"
                ))
                .text_style(NotedeckTextStyle::Heading.text_style()),
            ));

            let results = search_profiles(self.ndb, &txn, query, contacts_set);

            if results.is_empty() {
                ui.add_space(20.0);
                ui.label(
                    RichText::new(tr!(
                        self.i18n,
                        "No profiles found",
                        "Shown when profile search returns no results"
                    ))
                    .weak(),
                );
                None
            } else {
                search_results_list(
                    ui,
                    &results,
                    self.ndb,
                    &txn,
                    self.img_cache,
                    self.jobs,
                    self.i18n,
                )
            }
        }
    }
}

/// Renders the search input field for profile search.
fn search_input(query: &mut String, i18n: &mut Localization, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        let search_container = egui::Frame {
            inner_margin: egui::Margin::symmetric(8, 0),
            outer_margin: egui::Margin::ZERO,
            corner_radius: CornerRadius::same(18),
            shadow: Default::default(),
            fill: if ui.visuals().dark_mode {
                Color32::from_rgb(30, 30, 30)
            } else {
                Color32::from_rgb(240, 240, 240)
            },
            stroke: if ui.visuals().dark_mode {
                Stroke::new(1.0, Color32::from_rgb(60, 60, 60))
            } else {
                Stroke::new(1.0, Color32::from_rgb(200, 200, 200))
            },
        };

        search_container.show(ui, |ui| {
            ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 0.0);

                let search_height = 34.0;
                ui.add(search_icon(16.0, search_height));

                ui.add_sized(
                    [ui.available_width(), search_height],
                    TextEdit::singleline(query)
                        .hint_text(
                            RichText::new(tr!(
                                i18n,
                                "Search profiles...",
                                "Placeholder for profile search input"
                            ))
                            .weak(),
                        )
                        .margin(egui::vec2(0.0, 8.0))
                        .frame(false),
                );
            });
        });
    });
}

/// A profile search result.
struct SearchResult<'a> {
    /// The public key bytes of the matched profile.
    pk: &'a [u8; 32],
    /// Whether this profile is in the user's contacts.
    is_contact: bool,
}

/// Searches for profiles matching `query` in nostrdb and the user's contacts.
/// Contacts are prioritized and appear first in results. Returns up to 20 matches.
fn search_profiles<'a>(
    ndb: &Ndb,
    txn: &'a Transaction,
    query: &str,
    contacts: Option<&'a HashSet<Pubkey>>,
) -> Vec<SearchResult<'a>> {
    let mut results: Vec<SearchResult<'a>> = Vec::new();
    let mut seen: HashSet<&[u8; 32]> = HashSet::new();
    let query_lower = query.to_lowercase();

    // First, add matching contacts (prioritized)
    if let Some(contacts) = contacts {
        for pk in contacts {
            if let Ok(profile) = ndb.get_profile_by_pubkey(txn, pk.bytes()) {
                let name = get_display_name(Some(&profile)).name();
                if name.to_lowercase().contains(&query_lower) {
                    results.push(SearchResult {
                        pk: pk.bytes(),
                        is_contact: true,
                    });
                    seen.insert(pk.bytes());
                }
            }
        }
    }

    // Then add nostrdb search results
    if let Ok(pks) = ndb.search_profile(txn, query, 20) {
        for pk_bytes in pks {
            if !seen.contains(pk_bytes) {
                let is_contact = contacts.is_some_and(|c| c.contains(pk_bytes));
                results.push(SearchResult {
                    pk: pk_bytes,
                    is_contact,
                });
                seen.insert(pk_bytes);
            }
        }
    }

    results.truncate(20);
    results
}

/// Renders a scrollable list of search results. Returns `Some(CreateConvoResponse)`
/// if the user selects a profile.
fn search_results_list(
    ui: &mut egui::Ui,
    results: &[SearchResult<'_>],
    ndb: &Ndb,
    txn: &Transaction,
    img_cache: &mut Images,
    jobs: &MediaJobSender,
    i18n: &mut Localization,
) -> Option<CreateConvoResponse> {
    let mut action = None;

    egui::ScrollArea::vertical().show(ui, |ui| {
        for result in results {
            let profile = ndb.get_profile_by_pubkey(txn, result.pk).ok();

            if profile_row(
                ui,
                profile.as_ref(),
                result.is_contact,
                img_cache,
                jobs,
                i18n,
            ) {
                action = Some(CreateConvoResponse {
                    recipient: Pubkey::new(*result.pk),
                });
            }
        }
    });

    action
}
