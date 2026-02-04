use std::collections::HashSet;

use crate::ProfilePic;
use egui::{RichText, Sense};
use enostr::Pubkey;
use nostrdb::{Ndb, ProfileRecord, Transaction};
use notedeck::{
    name::get_display_name, profile::get_profile_url, tr, DragResponse, Images, Localization,
    MediaJobSender,
};

/// Render a profile row with picture and name, optionally showing a contact badge. Returns true if clicked.
pub fn profile_row(
    ui: &mut egui::Ui,
    profile: Option<&ProfileRecord<'_>>,
    is_contact: bool,
    img_cache: &mut Images,
    jobs: &MediaJobSender,
    i18n: &mut Localization,
) -> bool {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 56.0), Sense::click());

    if !ui.clip_rect().intersects(rect) {
        return false;
    }

    let name_str = get_display_name(profile).name();
    let profile_url = get_profile_url(profile);

    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);

    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 0.0, ui.visuals().widgets.hovered.weak_bg_fill);
    }

    let mut child_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    child_ui.horizontal(|ui| {
        ui.add_space(16.0);
        ui.add(&mut ProfilePic::new(img_cache, jobs, profile_url).size(48.0));
        ui.add_space(12.0);
        ui.add(
            egui::Label::new(
                RichText::new(name_str)
                    .size(16.0)
                    .color(ui.visuals().text_color()),
            )
            .selectable(false),
        );
        if is_contact {
            ui.add_space(8.0);
            ui.add(
                egui::Label::new(
                    RichText::new(tr!(
                        i18n,
                        "Contact",
                        "Badge indicating this profile is in contacts"
                    ))
                    .size(12.0)
                    .color(ui.visuals().weak_text_color()),
                )
                .selectable(false),
            );
        }
    });

    resp.clicked()
}

pub struct ContactsListView<'a, 'txn> {
    contacts: ContactsCollection<'a>,
    jobs: &'a MediaJobSender,
    ndb: &'a Ndb,
    img_cache: &'a mut Images,
    txn: &'txn Transaction,
    i18n: &'a mut Localization,
}

#[derive(Clone)]
pub enum ContactsListAction {
    Select(Pubkey),
}

pub enum ContactsCollection<'a> {
    Vec(&'a Vec<Pubkey>),
    Set(&'a HashSet<Pubkey>),
}

pub enum ContactsIter<'a> {
    Vec(std::slice::Iter<'a, Pubkey>),
    Set(std::collections::hash_set::Iter<'a, Pubkey>),
}

impl<'a> ContactsCollection<'a> {
    pub fn iter(&'a self) -> ContactsIter<'a> {
        match self {
            ContactsCollection::Vec(v) => ContactsIter::Vec(v.iter()),
            ContactsCollection::Set(s) => ContactsIter::Set(s.iter()),
        }
    }
}

impl<'a> Iterator for ContactsIter<'a> {
    type Item = &'a Pubkey;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            ContactsIter::Vec(iter) => iter.next().as_ref().copied(),
            ContactsIter::Set(iter) => iter.next().as_ref().copied(),
        }
    }
}

impl<'a, 'txn> ContactsListView<'a, 'txn> {
    pub fn new(
        contacts: ContactsCollection<'a>,
        jobs: &'a MediaJobSender,
        ndb: &'a Ndb,
        img_cache: &'a mut Images,
        txn: &'txn Transaction,
        i18n: &'a mut Localization,
    ) -> Self {
        ContactsListView {
            contacts,
            ndb,
            img_cache,
            txn,
            jobs,
            i18n,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> DragResponse<ContactsListAction> {
        let mut action = None;

        egui::ScrollArea::vertical().show(ui, |ui| {
            for contact_pubkey in self.contacts.iter() {
                let profile = self
                    .ndb
                    .get_profile_by_pubkey(self.txn, contact_pubkey.bytes())
                    .ok();

                if profile_row(
                    ui,
                    profile.as_ref(),
                    false,
                    self.img_cache,
                    self.jobs,
                    self.i18n,
                ) {
                    action = Some(ContactsListAction::Select(*contact_pubkey));
                }
            }
        });

        DragResponse::output(action)
    }
}
