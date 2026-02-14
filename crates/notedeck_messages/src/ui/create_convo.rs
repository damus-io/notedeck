use egui::{Label, RichText};
use enostr::Pubkey;
use nostrdb::{Ndb, Transaction};
use notedeck::{tr, ContactState, Images, Localization, MediaJobSender, NotedeckTextStyle};
use notedeck_ui::{
    profile_row, search_input_box, search_profiles, ContactsListView, ProfileSearchResult,
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
        let txn = Transaction::new(self.ndb).expect("txn");

        // Search input
        ui.add_space(8.0);
        let hint = tr!(
            self.i18n,
            "Search profiles...",
            "Placeholder for profile search input"
        );
        ui.add(search_input_box(&mut self.state.query, &hint));
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

            if let ContactState::Received { contacts, .. } = self.contacts {
                let resp = ContactsListView::new(
                    contacts,
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

            let results = search_profiles(self.ndb, &txn, query, self.contacts, 128);

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

/// Renders a scrollable list of search results. Returns `Some(CreateConvoResponse)`
/// if the user selects a profile.
fn search_results_list(
    ui: &mut egui::Ui,
    results: &[ProfileSearchResult],
    ndb: &Ndb,
    txn: &Transaction,
    img_cache: &mut Images,
    jobs: &MediaJobSender,
    i18n: &mut Localization,
) -> Option<CreateConvoResponse> {
    let mut action = None;

    egui::ScrollArea::vertical().show(ui, |ui| {
        for result in results {
            let profile = ndb.get_profile_by_pubkey(txn, &result.pk).ok();

            if profile_row(
                ui,
                profile.as_ref(),
                result.is_contact,
                img_cache,
                jobs,
                i18n,
            ) {
                action = Some(CreateConvoResponse {
                    recipient: Pubkey::new(result.pk),
                });
            }
        }
    });

    action
}
