use std::collections::HashSet;

use crate::ProfilePic;
use egui::{RichText, Sense, Stroke};
use enostr::Pubkey;
use nostrdb::{Ndb, ProfileRecord, Transaction};
use notedeck::{
    name::get_display_name, profile::get_profile_url, tr, ContactState, DragResponse, Images,
    Localization, MediaJobSender,
};

/// Configuration options for profile row rendering.
#[derive(Default)]
pub struct ProfileRowOptions {
    /// Show "Contact" badge next to the name
    pub show_contact_badge: bool,
    /// Show X button on the right (visual only - deletion handled by caller)
    pub show_x_button: bool,
    /// Highlight as selected (keyboard navigation)
    pub is_selected: bool,
}

impl ProfileRowOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn contact_badge(mut self, show: bool) -> Self {
        self.show_contact_badge = show;
        self
    }

    pub fn x_button(mut self, show: bool) -> Self {
        self.show_x_button = show;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.is_selected = selected;
        self
    }
}

/// Render a profile row with picture and name, with configurable options.
/// Returns the response for handling clicks.
pub fn profile_row_widget<'a>(
    profile: Option<&'a ProfileRecord<'a>>,
    img_cache: &'a mut Images,
    jobs: &'a MediaJobSender,
    i18n: &'a mut Localization,
    options: ProfileRowOptions,
) -> impl egui::Widget + 'a {
    move |ui: &mut egui::Ui| -> egui::Response {
        let (rect, resp) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 56.0), Sense::click());

        if !ui.clip_rect().intersects(rect) {
            return resp;
        }

        let name_str = get_display_name(profile).name();
        let profile_url = get_profile_url(profile);

        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);

        // Selection highlighting
        if options.is_selected {
            ui.painter()
                .rect_filled(rect, 4.0, ui.visuals().selection.bg_fill);
        }

        // Hover highlighting
        if resp.hovered() {
            ui.painter()
                .rect_filled(rect, 4.0, ui.visuals().widgets.hovered.bg_fill);
        }

        let mut child_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        child_ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.add(&mut ProfilePic::new(img_cache, jobs, profile_url).size(48.0));
            ui.add_space(8.0);
            ui.add(
                egui::Label::new(
                    RichText::new(name_str)
                        .size(16.0)
                        .color(ui.visuals().text_color()),
                )
                .selectable(false),
            );
            if options.show_contact_badge {
                ui.add_space(8.0);
                let badge_text = tr!(
                    i18n,
                    "Contact",
                    "Badge indicating this profile is in contacts"
                );
                ui.add(
                    egui::Label::new(
                        RichText::new(badge_text)
                            .size(12.0)
                            .color(ui.visuals().weak_text_color()),
                    )
                    .selectable(false),
                );
            }
        });

        // Draw X button on the right
        if options.show_x_button {
            let x_button_size = 32.0;
            let x_size = 12.0;
            let x_rect = egui::Rect::from_min_size(
                egui::Pos2::new(rect.right() - x_button_size, rect.top()),
                egui::vec2(x_button_size, rect.height()),
            );
            let x_center = x_rect.center();
            let painter = ui.painter();
            painter.line_segment(
                [
                    egui::Pos2::new(x_center.x - x_size / 2.0, x_center.y - x_size / 2.0),
                    egui::Pos2::new(x_center.x + x_size / 2.0, x_center.y + x_size / 2.0),
                ],
                Stroke::new(1.5, ui.visuals().text_color()),
            );
            painter.line_segment(
                [
                    egui::Pos2::new(x_center.x + x_size / 2.0, x_center.y - x_size / 2.0),
                    egui::Pos2::new(x_center.x - x_size / 2.0, x_center.y + x_size / 2.0),
                ],
                Stroke::new(1.5, ui.visuals().text_color()),
            );
        }

        resp
    }
}

/// Render a profile row with picture and name, optionally showing a contact badge. Returns true if clicked.
pub fn profile_row(
    ui: &mut egui::Ui,
    profile: Option<&ProfileRecord<'_>>,
    is_contact: bool,
    img_cache: &mut Images,
    jobs: &MediaJobSender,
    i18n: &mut Localization,
) -> bool {
    let options = ProfileRowOptions::new().contact_badge(is_contact);
    ui.add(profile_row_widget(profile, img_cache, jobs, i18n, options))
        .clicked()
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

/// A profile search result.
pub struct ProfileSearchResult {
    /// The public key bytes of the matched profile.
    pub pk: [u8; 32],
    /// Whether this profile is in the user's contacts.
    pub is_contact: bool,
}

/// Searches for profiles matching `query`, prioritizing contacts first and deduplicating.
/// Contacts that match appear first, followed by non-contact results.
/// Returns up to `max_results` matches.
pub fn search_profiles(
    ndb: &Ndb,
    txn: &Transaction,
    query: &str,
    contacts_state: &ContactState,
    max_results: usize,
) -> Vec<ProfileSearchResult> {
    let contacts_set = match contacts_state {
        ContactState::Received { contacts, .. } => Some(contacts),
        _ => None,
    };

    // Get ndb search results and partition into contacts and non-contacts
    let mut contact_results: Vec<ProfileSearchResult> = Vec::new();
    let mut other_results: Vec<ProfileSearchResult> = Vec::new();
    let mut seen: HashSet<&[u8; 32]> = HashSet::new();

    if let Ok(pks) = ndb.search_profile(txn, query, max_results as u32) {
        for pk_bytes in pks {
            // Skip duplicates
            if seen.contains(pk_bytes) {
                continue;
            }
            seen.insert(pk_bytes);

            let is_contact = contacts_set.is_some_and(|c| c.contains(pk_bytes));
            let result = ProfileSearchResult {
                pk: *pk_bytes,
                is_contact,
            };
            if is_contact {
                contact_results.push(result);
            } else {
                other_results.push(result);
            }
        }
    }

    // Combine: contacts first, then others
    contact_results.extend(other_results);
    contact_results.truncate(max_results);
    contact_results
}
