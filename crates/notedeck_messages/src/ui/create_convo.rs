use egui::{Label, RichText};
use enostr::Pubkey;
use nostrdb::{Ndb, Transaction};
use notedeck::{tr, ContactState, Images, Localization, MediaJobSender, NotedeckTextStyle};
use notedeck_ui::{contacts_list::ContactsCollection, ContactsListView};

pub struct CreateConvoUi<'a> {
    ndb: &'a Ndb,
    jobs: &'a MediaJobSender,
    img_cache: &'a mut Images,
    contacts: &'a ContactState,
    i18n: &'a mut Localization,
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
    ) -> Self {
        Self {
            ndb,
            jobs,
            img_cache,
            contacts,
            i18n,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<CreateConvoResponse> {
        let ContactState::Received { contacts, .. } = self.contacts else {
            // TODO render something about not having contacts
            return None;
        };

        let txn = Transaction::new(self.ndb).expect("txn");

        ui.add(Label::new(
            RichText::new(tr!(
                self.i18n,
                "Contacts",
                "Heading shown when choosing a contact to start a new chat"
            ))
            .text_style(NotedeckTextStyle::Heading.text_style()),
        ));
        let resp = ContactsListView::new(
            ContactsCollection::Set(contacts),
            self.jobs,
            self.ndb,
            self.img_cache,
            &txn,
        )
        .ui(ui);

        resp.output.map(|a| match a {
            notedeck_ui::ContactsListAction::Select(pubkey) => {
                CreateConvoResponse { recipient: pubkey }
            }
        })
    }
}
