use nostrdb::{Ndb, Transaction};

use crate::{account_manager::AccountManager, imgcache::ImageCache, DisplayName, Result};

use super::{
    preview::{get_display_name, get_profile_url, SimpleProfilePreview},
    ProfilePic,
};

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

        let txn = if let Ok(txn) = Transaction::new(self.ndb) {
            txn
        } else {
            return None;
        };

        for i in 0..account_manager.num_accounts() {
            let account = if let Some(account) = account_manager.get_account(i) {
                account
            } else {
                continue;
            };

            let profile =
                if let Ok(profile) = self.ndb.get_profile_by_pubkey(&txn, account.pubkey.bytes()) {
                    profile
                } else {
                    continue;
                };

            let preview = SimpleProfilePreview::new(&profile, self.img_cache);

            let is_selected = if let Some(selected) = account_manager.get_selected_account_index() {
                i == selected
            } else {
                false
            };

            let op = if let Some(op) = add_preview_ui(ui, preview, width, is_selected) {
                op
            } else {
                continue;
            };

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

        to_remove
    }

    pub fn view_profile_previews(
        &mut self,
        account_manager: &AccountManager,
        ui: &mut egui::Ui,
        add_preview_ui: fn(
            ui: &mut egui::Ui,
            preview: SimpleProfilePreview,
            width: f32,
            is_selected: bool,
            index: usize,
        ) -> bool,
    ) -> Option<usize> {
        let width = ui.available_width();

        let txn = if let Ok(txn) = Transaction::new(self.ndb) {
            txn
        } else {
            return None;
        };

        for i in 0..account_manager.num_accounts() {
            let account = if let Some(account) = account_manager.get_account(i) {
                account
            } else {
                continue;
            };

            let profile =
                if let Ok(profile) = self.ndb.get_profile_by_pubkey(&txn, account.pubkey.bytes()) {
                    profile
                } else {
                    continue;
                };

            let preview = SimpleProfilePreview::new(&profile, self.img_cache);

            let is_selected = if let Some(selected) = account_manager.get_selected_account_index() {
                i == selected
            } else {
                false
            };

            if add_preview_ui(ui, preview, width, is_selected, i) {
                return Some(i);
            }
        }

        None
    }

    pub fn show_with_nickname(
        &self,
        ui: &mut egui::Ui,
        key: &[u8; 32],
        ui_element: fn(ui: &mut egui::Ui, username: &DisplayName) -> egui::Response,
    ) -> Result<egui::Response> {
        let txn = Transaction::new(self.ndb)?;
        let profile = self.ndb.get_profile_by_pubkey(&txn, key)?;
        Ok(ui_element(ui, &get_display_name(&profile)))
    }

    pub fn show_with_pfp(
        self,
        ui: &mut egui::Ui,
        key: &[u8; 32],
        ui_element: fn(ui: &mut egui::Ui, pfp: ProfilePic) -> egui::Response,
    ) -> Option<egui::Response> {
        if let Ok(txn) = Transaction::new(self.ndb) {
            let profile = self.ndb.get_profile_by_pubkey(&txn, key);

            if let Ok(profile) = profile {
                return Some(ui_element(
                    ui,
                    ProfilePic::new(self.img_cache, get_profile_url(&profile)),
                ));
            }
        }
        None
    }
}
