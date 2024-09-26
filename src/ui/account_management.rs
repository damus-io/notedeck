use crate::colors::PINK;
use crate::imgcache::ImageCache;
use crate::{
    account_manager::AccountManager,
    route::{Route, Router},
    ui::{Preview, PreviewConfig, View},
    Damus,
};
use egui::{Align, Button, Frame, Image, InnerResponse, Layout, RichText, ScrollArea, Ui, Vec2};
use nostrdb::{Ndb, Transaction};

use super::profile::preview::SimpleProfilePreview;

pub struct AccountsView<'a> {
    ndb: &'a Ndb,
    accounts: &'a AccountManager,
    img_cache: &'a mut ImageCache,
}

#[derive(Clone, Debug)]
pub enum AccountsViewResponse {
    SelectAccount(usize),
    RemoveAccount(usize),
    RouteToLogin,
}

#[derive(Debug)]
enum ProfilePreviewOp {
    RemoveAccount,
    SwitchTo,
}

impl<'a> AccountsView<'a> {
    pub fn new(ndb: &'a Ndb, accounts: &'a AccountManager, img_cache: &'a mut ImageCache) -> Self {
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
        account_manager: &AccountManager,
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

                for i in 0..account_manager.num_accounts() {
                    let account_pubkey = account_manager
                        .get_account(i)
                        .map(|account| account.pubkey.bytes());

                    let account_pubkey = if let Some(pubkey) = account_pubkey {
                        pubkey
                    } else {
                        continue;
                    };

                    let profile = ndb.get_profile_by_pubkey(&txn, account_pubkey).ok();
                    let is_selected =
                        if let Some(selected) = account_manager.get_selected_account_index() {
                            i == selected
                        } else {
                            false
                        };

                    let profile_peview_view = {
                        let width = ui.available_width();
                        let preview = SimpleProfilePreview::new(profile.as_ref(), img_cache);
                        show_profile_card(ui, preview, width, is_selected)
                    };

                    if let Some(op) = profile_peview_view {
                        return_op = Some(match op {
                            ProfilePreviewOp::SwitchTo => AccountsViewResponse::SelectAccount(i),
                            ProfilePreviewOp::RemoveAccount => {
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
    width: f32,
    is_selected: bool,
) -> Option<ProfilePreviewOp> {
    let mut op: Option<ProfilePreviewOp> = None;

    ui.add_sized(Vec2::new(width, 50.0), |ui: &mut egui::Ui| {
        Frame::none()
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(preview);

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if is_selected {
                            ui.add(selected_widget());
                        } else {
                            if ui
                                .add(switch_button(ui.style().visuals.dark_mode))
                                .clicked()
                            {
                                op = Some(ProfilePreviewOp::SwitchTo);
                            }
                            if ui.add(sign_out_button(ui)).clicked() {
                                op = Some(ProfilePreviewOp::RemoveAccount)
                            }
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

fn sign_out_button(ui: &egui::Ui) -> egui::Button<'static> {
    let img_data = egui::include_image!("../../assets/icons/signout_icon_4x.png");
    let img = Image::new(img_data).fit_to_exact_size(Vec2::new(16.0, 16.0));

    egui::Button::image_and_text(
        img,
        RichText::new("Sign out").color(ui.visuals().noninteractive().fg_stroke.color),
    )
    .frame(false)
}

fn switch_button(dark_mode: bool) -> egui::Button<'static> {
    let _ = dark_mode;

    egui::Button::new("Switch").min_size(Vec2::new(76.0, 32.0))
}

fn selected_widget() -> impl egui::Widget {
    |ui: &mut egui::Ui| {
        Frame::none()
            .show(ui, |ui| {
                ui.label(RichText::new("Selected").size(13.0).color(PINK));
                let img_data = egui::include_image!("../../assets/icons/select_icon_3x.png");
                let img = Image::new(img_data).max_size(Vec2::new(16.0, 16.0));
                ui.add(img);
            })
            .response
    }
}

// PREVIEWS
mod preview {

    use super::*;
    use crate::{account_manager::process_accounts_view_response, test_data};

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
