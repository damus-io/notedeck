use enostr::Pubkey;
use nostrdb::{Ndb, Transaction};

use crate::{account_manager::AccountManager, imgcache::ImageCache, DisplayName};

use super::preview::{get_display_name, SimpleProfilePreview};

pub struct SimpleProfilePreviewController<'a> {
    ndb: &'a Ndb,
    img_cache: &'a mut ImageCache,
}

#[derive(Debug)]
pub enum ProfilePreviewOp {
    RemoveAccount,
    SwitchTo,
}

impl<'a> SimpleProfilePreviewController<'a> {
    pub fn new(ndb: &'a Ndb, img_cache: &'a mut ImageCache) -> Self {
        SimpleProfilePreviewController { ndb, img_cache }
    }

    pub fn set_profile_previews(
        &mut self,
        account_manager: &mut AccountManager,
        ui: &mut egui::Ui,
        add_preview_ui: fn(
            ui: &mut egui::Ui,
            preview: SimpleProfilePreview,
            width: f32,
            is_selected: bool,
        ) -> Option<ProfilePreviewOp>,
    ) -> Option<Vec<usize>> {
        let mut to_remove: Option<Vec<usize>> = None;

        let width = ui.available_width();

        for i in 0..account_manager.num_accounts() {
            if let Some(account) = account_manager.get_account(i) {
                if let Ok(txn) = Transaction::new(self.ndb) {
                    let profile = self
                        .ndb
                        .get_profile_by_pubkey(&txn, account.key.pubkey.bytes());

                    if let Ok(profile) = profile {
                        let preview = SimpleProfilePreview::new(&profile, self.img_cache);

                        let is_selected =
                            if let Some(selected) = account_manager.get_selected_account_index() {
                                i == selected
                            } else {
                                false
                            };

                        if let Some(op) = add_preview_ui(ui, preview, width, is_selected) {
                            match op {
                                ProfilePreviewOp::RemoveAccount => {
                                    if to_remove.is_none() {
                                        to_remove = Some(Vec::new());
                                    }
                                    to_remove.as_mut().unwrap().push(i);
                                }
                                ProfilePreviewOp::SwitchTo => account_manager.select_account(i),
                            }
                        }
                    };
                }
            }
        }

        to_remove
    }

    pub fn view_profile_previews(
        &mut self,
        account_manager: &mut AccountManager,
        ui: &mut egui::Ui,
        add_preview_ui: fn(
            ui: &mut egui::Ui,
            preview: SimpleProfilePreview,
            width: f32,
            is_selected: bool,
            index: usize,
        ) -> bool,
    ) {
        let width = ui.available_width();

        for i in 0..account_manager.num_accounts() {
            if let Some(account) = account_manager.get_account(i) {
                if let Ok(txn) = Transaction::new(self.ndb) {
                    let profile = self
                        .ndb
                        .get_profile_by_pubkey(&txn, account.key.pubkey.bytes());

                    if let Ok(profile) = profile {
                        let preview = SimpleProfilePreview::new(&profile, self.img_cache);

                        let is_selected =
                            if let Some(selected) = account_manager.get_selected_account_index() {
                                i == selected
                            } else {
                                false
                            };

                        if add_preview_ui(ui, preview, width, is_selected, i) {
                            account_manager.select_account(i);
                        }
                    }
                }
            }
        }
    }

    pub fn show_with_nickname(
        &'a self,
        ui: &mut egui::Ui,
        key: &Pubkey,
        ui_element: fn(ui: &mut egui::Ui, username: &DisplayName) -> egui::Response,
    ) -> Option<egui::Response> {
        if let Ok(txn) = Transaction::new(self.ndb) {
            let profile = self.ndb.get_profile_by_pubkey(&txn, key.bytes());

            if let Ok(profile) = profile {
                return Some(ui_element(ui, &get_display_name(&profile)));
            }
        }
        None
    }
}
