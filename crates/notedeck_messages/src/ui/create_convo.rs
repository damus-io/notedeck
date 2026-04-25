use egui::{Frame, Label, Margin, RichText};
use enostr::Pubkey;
use nostrdb::{Ndb, Transaction};
use notedeck::{tr, ContactState, Images, Localization, MediaJobSender, NotedeckTextStyle};
use notedeck_ui::{
    profile_row_widget, search_input_box, search_profiles, ContactsListView, ProfileRowOptions,
    ProfileSearchResult,
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
        Frame::new()
            .inner_margin(Margin {
                left: 16,
                right: 16,
                top: 8,
                bottom: 0,
            })
            .show(ui, |ui| self.content_ui(ui))
            .inner
    }

    fn content_ui(&mut self, ui: &mut egui::Ui) -> Option<CreateConvoResponse> {
        let txn = Transaction::new(self.ndb).expect("txn");

        // Search input
        let hint = tr!(
            self.i18n,
            "Search profiles...",
            "Placeholder for profile search input"
        );
        ui.add(search_input_box(&mut self.state.query, &hint));
        ui.add_space(12.0);

        let query = self.state.query.trim();

        if query.is_empty() {
            let contacts_margin = Margin {
                left: 0,
                right: 0,
                top: 4,
                bottom: 0,
            };
            if let ContactState::Received { contacts, .. } = self.contacts {
                Frame::new()
                    .inner_margin(contacts_margin)
                    .show(ui, |ui| {
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
                    })
                    .inner
            } else {
                Frame::new()
                    .inner_margin(contacts_margin)
                    .show(ui, |ui| {
                        ui.label(tr!(
                            self.i18n,
                            "No contacts yet",
                            "Shown when user has no contacts to display"
                        ));
                        None
                    })
                    .inner
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
            let recipient = Pubkey::new(result.pk);
            let label = recipient.npub().unwrap_or_else(|| recipient.hex());
            let response = ui.add(profile_row_widget(
                profile.as_ref(),
                img_cache,
                jobs,
                i18n,
                ProfileRowOptions::new().contact_badge(result.is_contact),
            ));
            response.widget_info(move || {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.clone())
            });
            if response.clicked() {
                action = Some(CreateConvoResponse { recipient });
            }
        }
    });

    action
}
