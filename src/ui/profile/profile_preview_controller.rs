use nostrdb::{Ndb, Transaction};

use crate::{account_manager::AccountManager, imgcache::ImageCache};

use super::preview::SimpleProfilePreview;

pub struct SimpleProfilePreviewController<'a> {
    ndb: &'a Ndb,
    img_cache: &'a mut ImageCache,
}

impl<'a> SimpleProfilePreviewController<'a> {
    pub fn new(ndb: &'a Ndb, img_cache: &'a mut ImageCache) -> Self {
        SimpleProfilePreviewController { ndb, img_cache }
    }

    pub fn set_profile_previews(
        &mut self,
        account_manager: &AccountManager,
        ui: &mut egui::Ui,
        edit_mode: bool,
        add_preview_ui: fn(
            ui: &mut egui::Ui,
            preview: SimpleProfilePreview,
            edit_mode: bool,
        ) -> bool,
    ) -> Option<Vec<usize>> {
        let mut to_remove: Option<Vec<usize>> = None;

        for i in 0..account_manager.num_accounts() {
            if let Some(account) = account_manager.get_account(i) {
                if let Ok(txn) = Transaction::new(self.ndb) {
                    let profile = self
                        .ndb
                        .get_profile_by_pubkey(&txn, account.key.pubkey.bytes());

                    if let Ok(profile) = profile {
                        let preview = SimpleProfilePreview::new(&profile, self.img_cache);

                        if add_preview_ui(ui, preview, edit_mode) {
                            if to_remove.is_none() {
                                to_remove = Some(Vec::new());
                            }
                            to_remove.as_mut().unwrap().push(i);
                        }
                    };
                }
            }
        }

        to_remove
    }

    pub fn view_profile_previews(
        &mut self,
        account_manager: &'a AccountManager,
        ui: &mut egui::Ui,
        add_preview_ui: fn(ui: &mut egui::Ui, preview: SimpleProfilePreview, index: usize) -> bool,
    ) -> Option<usize> {
        let mut clicked_at: Option<usize> = None;

        for i in 0..account_manager.num_accounts() {
            if let Some(account) = account_manager.get_account(i) {
                if let Ok(txn) = Transaction::new(self.ndb) {
                    let profile = self
                        .ndb
                        .get_profile_by_pubkey(&txn, account.key.pubkey.bytes());

                    if let Ok(profile) = profile {
                        let preview = SimpleProfilePreview::new(&profile, self.img_cache);

                        if add_preview_ui(ui, preview, i) {
                            clicked_at = Some(i)
                        }
                    }
                }
            }
        }

        clicked_at
    }
}
