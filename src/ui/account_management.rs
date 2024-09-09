use crate::colors::PINK;
use crate::imgcache::ImageCache;
use crate::{
    account_manager::AccountManager,
    ui::{Preview, PreviewConfig, View},
    Damus,
};
use egui::{Align, Button, Frame, Image, InnerResponse, Layout, RichText, ScrollArea, Ui, Vec2};
use nostrdb::{Ndb, Transaction};

use super::profile::preview::SimpleProfilePreview;
use super::profile::ProfilePreviewOp;
use super::profile_preview_controller::profile_preview_view;

pub struct AccountManagementView {}

#[derive(Clone, Debug)]
pub enum AccountManagementViewResponse {
    SelectAccount(usize),
    RemoveAccount(usize),
    RouteToLogin,
}

impl AccountManagementView {
    pub fn ui(
        ui: &mut Ui,
        account_manager: &AccountManager,
        ndb: &Ndb,
        img_cache: &mut ImageCache,
    ) -> InnerResponse<Option<AccountManagementViewResponse>> {
        Frame::none().outer_margin(12.0).show(ui, |ui| {
            if let Some(resp) = Self::top_section_buttons_widget(ui).inner {
                return Some(resp);
            }

            ui.add_space(8.0);
            scroll_area()
                .show(ui, |ui| {
                    Self::show_accounts(ui, account_manager, ndb, img_cache)
                })
                .inner
        })
    }

    fn show_accounts(
        ui: &mut Ui,
        account_manager: &AccountManager,
        ndb: &Ndb,
        img_cache: &mut ImageCache,
    ) -> Option<AccountManagementViewResponse> {
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_size_before_wrap().x, 32.0),
            Layout::top_down(egui::Align::Min),
            |ui| {
                let txn = Transaction::new(ndb).ok()?;

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

                    if let Some(op) =
                        profile_preview_view(ui, profile.as_ref(), img_cache, is_selected)
                    {
                        return Some(match op {
                            ProfilePreviewOp::SwitchTo => {
                                AccountManagementViewResponse::SelectAccount(i)
                            }
                            ProfilePreviewOp::RemoveAccount => {
                                AccountManagementViewResponse::RemoveAccount(i)
                            }
                        });
                    }
                }
                None
            },
        )
        .inner
    }

    fn top_section_buttons_widget(
        ui: &mut egui::Ui,
    ) -> InnerResponse<Option<AccountManagementViewResponse>> {
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(ui.available_size_before_wrap().x, 32.0),
                Layout::left_to_right(egui::Align::Center),
                |ui| {
                    if ui.add(add_account_button()).clicked() {
                        Some(AccountManagementViewResponse::RouteToLogin)
                    } else {
                        None
                    }
                },
            )
            .inner
        })
    }
}

pub fn show_profile_card(
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
    use crate::{account_manager::process_management_view_response_stateless, test_data};

    pub struct AccountManagementPreview {
        app: Damus,
    }

    impl AccountManagementPreview {
        fn new() -> Self {
            let app = test_data::test_app();

            AccountManagementPreview { app }
        }
    }

    impl View for AccountManagementPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            ui.add_space(24.0);
            if let Some(response) = AccountManagementView::ui(
                ui,
                &self.app.account_manager,
                &self.app.ndb,
                &mut self.app.img_cache,
            )
            .inner
            {
                process_management_view_response_stateless(&mut self.app.account_manager, response)
            }
        }
    }

    impl Preview for AccountManagementView {
        type Prev = AccountManagementPreview;

        fn preview(_cfg: PreviewConfig) -> Self::Prev {
            AccountManagementPreview::new()
        }
    }
}
