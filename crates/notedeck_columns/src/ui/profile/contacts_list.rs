use egui::{RichText, Sense};
use enostr::Pubkey;
use nostrdb::Transaction;
use notedeck::{name::get_display_name, profile::get_profile_url, BodyResponse, NoteContext};
use notedeck_ui::ProfilePic;

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
            let clip_rect = ui.clip_rect();

            for contact_pubkey in &self.contacts {
                let (rect, resp) =
                    ui.allocate_exact_size(egui::vec2(ui.available_width(), 56.0), Sense::click());

                if !clip_rect.intersects(rect) {
                    continue;
                }

                let profile = self
                    .note_context
                    .ndb
                    .get_profile_by_pubkey(self.txn, contact_pubkey.bytes())
                    .ok();

                let display_name = get_display_name(profile.as_ref());
                let name_str = display_name.display_name.unwrap_or("Anonymous");
                let profile_url = get_profile_url(profile.as_ref());

                let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);

                if resp.hovered() {
                    ui.painter()
                        .rect_filled(rect, 0.0, ui.visuals().widgets.hovered.weak_bg_fill);
                }

                let mut child_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                child_ui.horizontal(|ui| {
                    ui.add_space(16.0);

                    ui.add(
                        &mut ProfilePic::new(
                            self.note_context.img_cache,
                            self.note_context.jobs,
                            profile_url,
                        )
                        .size(48.0),
                    );

                    ui.add_space(12.0);

                    ui.add(
                        egui::Label::new(
                            RichText::new(name_str)
                                .size(16.0)
                                .color(ui.visuals().text_color()),
                        )
                        .selectable(false),
                    );
                });

                if resp.clicked() {
                    action = Some(ContactsListAction::OpenProfile(*contact_pubkey));
                }
            }
        });

        BodyResponse::output(action)
    }
}
