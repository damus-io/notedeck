use egui::{RichText, ScrollArea, Sense};
use enostr::Pubkey;
use nostrdb::Transaction;
use notedeck::{name::get_display_name, profile::get_profile_url, NoteContext};
use notedeck_ui::ProfilePic;

use crate::nav::BodyResponse;

pub struct ContactsListView<'a, 'd> {
    pubkey: &'a Pubkey,
    contacts: Vec<Pubkey>,
    note_context: &'a mut NoteContext<'d>,
}

pub enum ContactsListAction {
    OpenProfile(Pubkey),
}

impl<'a, 'd> ContactsListView<'a, 'd> {
    pub fn new(
        pubkey: &'a Pubkey,
        contacts: Vec<Pubkey>,
        note_context: &'a mut NoteContext<'d>,
    ) -> Self {
        ContactsListView {
            pubkey,
            contacts,
            note_context,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> BodyResponse<ContactsListAction> {
        let mut action = None;
        let scroll_id = egui::Id::new(("contacts_list", self.pubkey));

        ScrollArea::vertical()
            .id_salt(scroll_id)
            .animated(false)
            .show(ui, |ui| {
                ui.add_space(12.0);

                for contact_pubkey in &self.contacts {
                    let txn = Transaction::new(self.note_context.ndb).expect("txn");
                    let profile = self
                        .note_context
                        .ndb
                        .get_profile_by_pubkey(&txn, contact_pubkey.bytes())
                        .ok();

                    let (rect, mut resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 48.0 + 8.0),
                        Sense::click(),
                    );

                    let mut child_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                    child_ui.horizontal(|ui| {
                            ui.add_space(12.0);

                            ui.add(
                                &mut ProfilePic::new(
                                    self.note_context.img_cache,
                                    get_profile_url(profile.as_ref()),
                                )
                                .size(48.0),
                            );

                            ui.add_space(12.0);

                            let display_name = get_display_name(profile.as_ref());
                            let name_str = display_name.display_name.unwrap_or("Anonymous");
                            ui.label(
                                RichText::new(name_str)
                                    .size(16.0)
                                    .color(ui.visuals().text_color()),
                            );
                        });

                    resp = resp
                        .interact(Sense::click())
                        .on_hover_cursor(egui::CursorIcon::PointingHand);

                    if resp.clicked() {
                        action = Some(ContactsListAction::OpenProfile(*contact_pubkey));
                    }
                }
            });

        BodyResponse::output(action)
    }
}
