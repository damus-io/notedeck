use enostr::Pubkey;
use nostrdb::Transaction;
use notedeck::NoteContext;

use crate::{nav::BodyResponse, ui::widgets::UserRow};

pub struct ContactsListView<'a, 'd, 'txn> {
    contacts: Vec<Pubkey>,
    note_context: &'a mut NoteContext<'d>,
    txn: &'txn Transaction,
}

#[derive(Clone)]
pub enum ContactsListAction {
    OpenProfile(Pubkey),
}

impl<'a, 'd, 'txn> ContactsListView<'a, 'd, 'txn> {
    pub fn new(
        _pubkey: &'a Pubkey,
        contacts: Vec<Pubkey>,
        note_context: &'a mut NoteContext<'d>,
        txn: &'txn Transaction,
    ) -> Self {
        ContactsListView {
            contacts,
            note_context,
            txn,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> BodyResponse<ContactsListAction> {
        let mut action = None;

        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(8.0);

            for contact_pubkey in &self.contacts {
                let profile = self
                    .note_context
                    .ndb
                    .get_profile_by_pubkey(self.txn, contact_pubkey.bytes())
                    .ok();

                let available_width = ui.available_width() - 16.0;
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    if ui.add(UserRow::new(profile.as_ref(), contact_pubkey, self.note_context.img_cache, available_width)
                        .with_accounts(self.note_context.accounts)).clicked() {
                        action = Some(ContactsListAction::OpenProfile(*contact_pubkey));
                    }
                    ui.add_space(8.0);
                });
            }
        });

        BodyResponse::output(action)
    }
}
