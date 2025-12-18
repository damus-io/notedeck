use std::collections::HashSet;

use crate::ProfilePic;
use egui::{RichText, Sense};
use enostr::Pubkey;
use nostrdb::{Ndb, Transaction};
use notedeck::{
    name::get_display_name, profile::get_profile_url, DragResponse, Images, MediaJobSender,
};

pub struct ContactsListView<'a, 'txn> {
    contacts: ContactsCollection<'a>,
    jobs: &'a MediaJobSender,
    ndb: &'a Ndb,
    img_cache: &'a mut Images,
    txn: &'txn Transaction,
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
    ) -> Self {
        ContactsListView {
            contacts,
            ndb,
            img_cache,
            txn,
            jobs,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> DragResponse<ContactsListAction> {
        let mut action = None;

        egui::ScrollArea::vertical().show(ui, |ui| {
            let clip_rect = ui.clip_rect();

            for contact_pubkey in self.contacts.iter() {
                let (rect, resp) =
                    ui.allocate_exact_size(egui::vec2(ui.available_width(), 56.0), Sense::click());

                if !clip_rect.intersects(rect) {
                    continue;
                }

                let profile = self
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

                    ui.add(&mut ProfilePic::new(self.img_cache, self.jobs, profile_url).size(48.0));

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
                    action = Some(ContactsListAction::Select(*contact_pubkey));
                }
            }
        });

        DragResponse::output(action)
    }
}
