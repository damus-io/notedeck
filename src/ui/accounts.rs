use crate::colors::{self, PINK};
use crate::imgcache::ImageCache;
use crate::{
    accounts::Accounts,
    route::{Route, Router},
    ui::{Preview, PreviewConfig, View},
    Damus,
};
use egui::{
    Align, Button, Frame, Image, InnerResponse, Layout, RichText, ScrollArea, Stroke, Ui, Vec2,
};
use nostrdb::{Ndb, Transaction};

use super::profile::preview::SimpleProfilePreview;

pub struct AccountsView<'a> {
    ndb: &'a Ndb,
    accounts: &'a Accounts,
    img_cache: &'a mut ImageCache,
}

#[derive(Clone, Debug)]
pub enum AccountsViewResponse {
    SelectAccount(usize),
    RemoveAccount(usize),
    RouteToLogin,
}

#[derive(Debug)]
enum ProfilePreviewAction {
    RemoveAccount,
    SwitchTo,
}

impl<'a> AccountsView<'a> {
    pub fn new(ndb: &'a Ndb, accounts: &'a Accounts, img_cache: &'a mut ImageCache) -> Self {
        AccountsView {
            ndb,
            accounts,
            img_cache,
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) -> InnerResponse<Option<AccountsViewResponse>> {
        Frame::none().outer_margin(12.0).show(ui, |ui| {
            if let Some(resp) = Self::top_section_buttons_widget(ui).inner {
                return Some(resp);
            }

            ui.add_space(8.0);
            scroll_area()
                .show(ui, |ui| {
                    Self::show_accounts(ui, self.accounts, self.ndb, self.img_cache)
                })
                .inner
        })
    }

    fn show_accounts(
        ui: &mut Ui,
        accounts: &Accounts,
        ndb: &Ndb,
        img_cache: &mut ImageCache,
    ) -> Option<AccountsViewResponse> {
        let mut return_op: Option<AccountsViewResponse> = None;
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_size_before_wrap().x, 32.0),
            Layout::top_down(egui::Align::Min),
            |ui| {
                let txn = if let Ok(txn) = Transaction::new(ndb) {
                    txn
                } else {
                    return;
                };

                for i in 0..accounts.num_accounts() {
                    let (account_pubkey, has_nsec) = match accounts.get_account(i) {
                        Some(acc) => (acc.pubkey.bytes(), acc.secret_key.is_some()),
                        None => continue,
                    };

                    let profile = ndb.get_profile_by_pubkey(&txn, account_pubkey).ok();
                    let is_selected = if let Some(selected) = accounts.get_selected_account_index()
                    {
                        i == selected
                    } else {
                        false
                    };

                    let profile_peview_view = {
                        let max_size = egui::vec2(ui.available_width(), 77.0);
                        let resp = ui.allocate_response(max_size, egui::Sense::click());
                        ui.allocate_ui_at_rect(resp.rect, |ui| {
                            let preview =
                                SimpleProfilePreview::new(profile.as_ref(), img_cache, has_nsec);
                            show_profile_card(ui, preview, max_size, is_selected, resp)
                        })
                        .inner
                    };

                    if let Some(op) = profile_peview_view {
                        return_op = Some(match op {
                            ProfilePreviewAction::SwitchTo => {
                                AccountsViewResponse::SelectAccount(i)
                            }
                            ProfilePreviewAction::RemoveAccount => {
                                AccountsViewResponse::RemoveAccount(i)
                            }
                        });
                    }
                }
            },
        );
        return_op
    }

    fn top_section_buttons_widget(
        ui: &mut egui::Ui,
    ) -> InnerResponse<Option<AccountsViewResponse>> {
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_size_before_wrap().x, 32.0),
            Layout::left_to_right(egui::Align::Center),
            |ui| {
                if ui.add(add_account_button()).clicked() {
                    Some(AccountsViewResponse::RouteToLogin)
                } else {
                    None
                }
            },
        )
    }
}

fn show_profile_card(
    ui: &mut egui::Ui,
    preview: SimpleProfilePreview,
    max_size: egui::Vec2,
    is_selected: bool,
    card_resp: egui::Response,
) -> Option<ProfilePreviewAction> {
    let mut op: Option<ProfilePreviewAction> = None;

    ui.add_sized(max_size, |ui: &mut egui::Ui| {
        let mut frame = Frame::none();
        if is_selected || card_resp.hovered() {
            frame = frame.fill(ui.visuals().noninteractive().weak_bg_fill)
        }
        if is_selected {
            frame = frame.stroke(Stroke::new(2.0, colors::PINK))
        }
        frame
            .rounding(8.0)
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(preview);

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if card_resp.clicked() {
                            op = Some(ProfilePreviewAction::SwitchTo);
                        }
                        if ui
                            .add_sized(egui::Vec2::new(84.0, 32.0), sign_out_button())
                            .clicked()
                        {
                            op = Some(ProfilePreviewAction::RemoveAccount)
                        }
                    });
                });
            })
            .response
    });
    ui.add_space(16.0);
    op
}

fn scroll_area() -> ScrollArea {
    egui::ScrollArea::vertical()
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .auto_shrink([false; 2])
}

fn add_account_button() -> Button<'static> {
    let img_data = egui::include_image!("../../assets/icons/add_account_icon_4x.png");
    let img = Image::new(img_data).fit_to_exact_size(Vec2::new(48.0, 48.0));
    Button::image_and_text(
        img,
        RichText::new(" Add account")
            .size(16.0)
            // TODO: this color should not be hard coded. Find some way to add it to the visuals
            .color(PINK),
    )
    .frame(false)
}

fn sign_out_button() -> egui::Button<'static> {
    egui::Button::new(RichText::new("Sign out"))
}

// PREVIEWS
mod preview {

    use super::*;
    use crate::{accounts::process_accounts_view_response, test_data};

    pub struct AccountsPreview {
        app: Damus,
        router: Router<Route>,
    }

    impl AccountsPreview {
        fn new() -> Self {
            let app = test_data::test_app();
            let router = Router::new(vec![Route::accounts()]);

            AccountsPreview { app, router }
        }
    }

    impl View for AccountsPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            ui.add_space(24.0);
            // TODO(jb55): maybe just use render_nav here so we can step through routes
            if let Some(response) =
                AccountsView::new(&self.app.ndb, &self.app.accounts, &mut self.app.img_cache)
                    .ui(ui)
                    .inner
            {
                process_accounts_view_response(self.app.accounts_mut(), response, &mut self.router);
            }
        }
    }

    impl<'a> Preview for AccountsView<'a> {
        type Prev = AccountsPreview;

        fn preview(_cfg: PreviewConfig) -> Self::Prev {
            AccountsPreview::new()
        }
    }
}
