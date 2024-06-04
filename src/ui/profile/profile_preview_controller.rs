use nostrdb::{Ndb, Transaction};

use crate::{Damus, DisplayName, Result};

use super::{
    preview::{get_display_name, get_profile_url, SimpleProfilePreview},
    ProfilePic,
};

#[derive(Debug)]
pub enum ProfilePreviewOp {
    RemoveAccount,
    SwitchTo,
}

pub fn set_profile_previews(
    app: &mut Damus,
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

    let txn = if let Ok(txn) = Transaction::new(&app.ndb) {
        txn
    } else {
        return None;
    };

    for i in 0..app.account_manager.num_accounts() {
        let account = if let Some(account) = app.account_manager.get_account(i) {
            account
        } else {
            continue;
        };

        let profile =
            if let Ok(profile) = app.ndb.get_profile_by_pubkey(&txn, account.pubkey.bytes()) {
                profile
            } else {
                continue;
            };

        let preview = SimpleProfilePreview::new(&profile, &mut app.img_cache);

        let is_selected = if let Some(selected) = app.account_manager.get_selected_account_index() {
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
            ProfilePreviewOp::SwitchTo => app.account_manager.select_account(i),
        }
    }

    to_remove
}

pub fn view_profile_previews(
    app: &mut Damus,
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

    let txn = if let Ok(txn) = Transaction::new(&app.ndb) {
        txn
    } else {
        return None;
    };

    for i in 0..app.account_manager.num_accounts() {
        let account = if let Some(account) = app.account_manager.get_account(i) {
            account
        } else {
            continue;
        };

        let profile =
            if let Ok(profile) = app.ndb.get_profile_by_pubkey(&txn, account.pubkey.bytes()) {
                profile
            } else {
                continue;
            };

        let preview = SimpleProfilePreview::new(&profile, &mut app.img_cache);

        let is_selected = if let Some(selected) = app.account_manager.get_selected_account_index() {
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
    ndb: &Ndb,
    ui: &mut egui::Ui,
    key: &[u8; 32],
    ui_element: fn(ui: &mut egui::Ui, username: &DisplayName) -> egui::Response,
) -> Result<egui::Response> {
    let txn = Transaction::new(ndb)?;
    let profile = ndb.get_profile_by_pubkey(&txn, key)?;
    Ok(ui_element(ui, &get_display_name(&profile)))
}

pub fn show_with_pfp(
    app: &mut Damus,
    ui: &mut egui::Ui,
    key: &[u8; 32],
    ui_element: fn(ui: &mut egui::Ui, pfp: ProfilePic) -> egui::Response,
) -> Option<egui::Response> {
    if let Ok(txn) = Transaction::new(&app.ndb) {
        let profile = app.ndb.get_profile_by_pubkey(&txn, key);

        if let Ok(profile) = profile {
            return Some(ui_element(
                ui,
                ProfilePic::new(&mut app.img_cache, get_profile_url(&profile)),
            ));
        }
    }
    None
}
