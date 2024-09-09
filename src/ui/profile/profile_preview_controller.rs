use egui::Ui;
use nostrdb::{Ndb, ProfileRecord, Transaction};

use crate::{
    imgcache::ImageCache, ui::account_management::show_profile_card, Damus, DisplayName, Result,
};

use super::{
    preview::{get_display_name, get_profile_url, SimpleProfilePreview},
    ProfilePic,
};

#[derive(Debug)]
pub enum ProfilePreviewOp {
    RemoveAccount,
    SwitchTo,
}

pub fn profile_preview_view(
    ui: &mut Ui,
    profile: Option<&'_ ProfileRecord<'_>>,
    img_cache: &mut ImageCache,
    is_selected: bool,
) -> Option<ProfilePreviewOp> {
    let width = ui.available_width();

    let preview = SimpleProfilePreview::new(profile, img_cache);
    show_profile_card(ui, preview, width, is_selected)
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

    for i in 0..app.accounts.num_accounts() {
        let account = if let Some(account) = app.accounts.get_account(i) {
            account
        } else {
            continue;
        };

        let profile = app
            .ndb
            .get_profile_by_pubkey(&txn, account.pubkey.bytes())
            .ok();

        let preview = SimpleProfilePreview::new(profile.as_ref(), &mut app.img_cache);

        let is_selected = if let Some(selected) = app.accounts.get_selected_account_index() {
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
    Ok(ui_element(ui, &get_display_name(Some(&profile))))
}

pub fn show_with_selected_pfp(
    app: &mut Damus,
    ui: &mut egui::Ui,
    ui_element: fn(ui: &mut egui::Ui, pfp: ProfilePic) -> egui::Response,
) -> Option<egui::Response> {
    let selected_account = app.accounts.get_selected_account();
    if let Some(selected_account) = selected_account {
        if let Ok(txn) = Transaction::new(&app.ndb) {
            let profile = app
                .ndb
                .get_profile_by_pubkey(&txn, selected_account.pubkey.bytes());

            return Some(ui_element(
                ui,
                ProfilePic::new(&mut app.img_cache, get_profile_url(profile.ok().as_ref())),
            ));
        }
    }

    None
}

pub fn show_with_pfp(
    app: &mut Damus,
    ui: &mut egui::Ui,
    key: &[u8; 32],
    ui_element: fn(ui: &mut egui::Ui, pfp: ProfilePic) -> egui::Response,
) -> Option<egui::Response> {
    if let Ok(txn) = Transaction::new(&app.ndb) {
        let profile = app.ndb.get_profile_by_pubkey(&txn, key);

        return Some(ui_element(
            ui,
            ProfilePic::new(&mut app.img_cache, get_profile_url(profile.ok().as_ref())),
        ));
    }
    None
}
