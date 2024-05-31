use crate::{
    account_manager::{AccountManager, UserAccount},
    colors::PINK,
    profile::DisplayName,
    Result,
};
use egui::{
    Align, Button, Color32, Frame, Id, Image, Layout, Margin, RichText, Rounding, ScrollArea,
    Sense, Vec2,
};

use super::profile::{preview::SimpleProfilePreview, SimpleProfilePreviewController};

pub struct AccountSelectionWidget<'a> {
    is_mobile: bool,
    account_manager: &'a AccountManager,
    simple_preview_controller: SimpleProfilePreviewController<'a>,
}

enum AccountSelectAction {
    RemoveAccount { _index: usize },
    SelectAccount { _index: usize },
    OpenAccountManagement,
}

#[derive(Default)]
struct AccountSelectResponse {
    action: Option<AccountSelectAction>,
}

impl<'a> AccountSelectionWidget<'a> {
    pub fn new(
        is_mobile: bool,
        account_manager: &'a AccountManager,
        simple_preview_controller: SimpleProfilePreviewController<'a>,
    ) -> Self {
        AccountSelectionWidget {
            is_mobile,
            account_manager,
            simple_preview_controller,
        }
    }

    pub fn ui(&'a mut self, ui: &mut egui::Ui) {
        if self.is_mobile {
            self.show_mobile(ui);
        } else {
            self.show(ui);
        }
    }

    fn show(&mut self, ui: &mut egui::Ui) -> AccountSelectResponse {
        let mut res = AccountSelectResponse::default();
        let mut selected_index = self.account_manager.get_selected_account_index();

        Frame::none().outer_margin(8.0).show(ui, |ui| {
            res = top_section_widget(ui);

            scroll_area().show(ui, |ui| {
                if let Some(_index) = self.show_accounts(ui) {
                    selected_index = Some(_index);
                    res.action = Some(AccountSelectAction::SelectAccount { _index });
                }
            });
            ui.add_space(8.0);
            ui.add(add_account_button());

            if let Some(_index) = selected_index {
                if let Some(account) = self.account_manager.get_account(_index) {
                    ui.add_space(8.0);
                    if self.handle_sign_out(ui, account) {
                        res.action = Some(AccountSelectAction::RemoveAccount { _index })
                    }
                }
            }

            ui.add_space(8.0);
        });

        res
    }

    fn handle_sign_out(&mut self, ui: &mut egui::Ui, account: &UserAccount) -> bool {
        if let Ok(response) = self.sign_out_button(ui, account) {
            response.clicked()
        } else {
            false
        }
    }

    fn show_mobile(&mut self, ui: &mut egui::Ui) -> egui::Response {
        let _ = ui;
        todo!()
    }

    fn show_accounts(&mut self, ui: &mut egui::Ui) -> Option<usize> {
        self.simple_preview_controller.view_profile_previews(
            self.account_manager,
            ui,
            account_switcher_card_ui(),
        )
    }

    fn sign_out_button(&self, ui: &mut egui::Ui, account: &UserAccount) -> Result<egui::Response> {
        self.simple_preview_controller.show_with_nickname(
            ui,
            account.pubkey.bytes(),
            |ui: &mut egui::Ui, username: &DisplayName| {
                let img_data = egui::include_image!("../../assets/icons/signout_icon_4x.png");
                let img = Image::new(img_data).fit_to_exact_size(Vec2::new(16.0, 16.0));
                let button = egui::Button::image_and_text(
                    img,
                    RichText::new(format!(" Sign out @{}", username.username()))
                        .color(PINK)
                        .size(16.0),
                )
                .frame(false);

                ui.add(button)
            },
        )
    }
}

fn account_switcher_card_ui() -> fn(
    ui: &mut egui::Ui,
    preview: SimpleProfilePreview,
    width: f32,
    is_selected: bool,
    index: usize,
) -> bool {
    |ui, preview, width, is_selected, index| {
        let resp = ui.add_sized(Vec2::new(width, 50.0), |ui: &mut egui::Ui| {
            Frame::none()
                .show(ui, |ui| {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if is_selected {
                            Frame::none()
                                .rounding(Rounding::same(8.0))
                                .inner_margin(Margin::same(8.0))
                                .fill(Color32::from_rgb(0x45, 0x1B, 0x59))
                                .show(ui, |ui| {
                                    ui.add(preview);
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        ui.add(selection_widget());
                                    });
                                });
                        } else {
                            ui.add_space(8.0);
                            ui.add(preview);
                        }
                    });
                })
                .response
        });

        ui.interact(resp.rect, Id::new(index), Sense::click())
            .clicked()
    }
}

fn selection_widget() -> impl egui::Widget {
    |ui: &mut egui::Ui| {
        let img_data: egui::ImageSource =
            egui::include_image!("../../assets/icons/select_icon_3x.png");
        let img = Image::new(img_data).max_size(Vec2::new(16.0, 16.0));
        ui.add(img)
    }
}

fn top_section_widget(ui: &mut egui::Ui) -> AccountSelectResponse {
    ui.horizontal(|ui| {
        let mut resp = AccountSelectResponse::default();

        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_size_before_wrap().x, 32.0),
            Layout::left_to_right(egui::Align::Center),
            |ui| ui.add(account_switcher_title()),
        );

        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_size_before_wrap().x, 32.0),
            Layout::right_to_left(egui::Align::Center),
            |ui| {
                if ui.add(manage_accounts_button()).clicked() {
                    resp.action = Some(AccountSelectAction::OpenAccountManagement);
                }
            },
        );

        resp
    })
    .inner
}

fn manage_accounts_button() -> egui::Button<'static> {
    Button::new(RichText::new("Manage").color(PINK).size(16.0)).frame(false)
}

fn account_switcher_title() -> impl egui::Widget {
    |ui: &mut egui::Ui| ui.label(RichText::new("Account switcher").size(20.0).strong())
}

fn scroll_area() -> ScrollArea {
    egui::ScrollArea::vertical()
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .auto_shrink([false; 2])
}

fn add_account_button() -> egui::Button<'static> {
    let img_data = egui::include_image!("../../assets/icons/plus_icon_4x.png");
    let img = Image::new(img_data).fit_to_exact_size(Vec2::new(16.0, 16.0));
    Button::image_and_text(img, RichText::new(" Add account").size(16.0).color(PINK)).frame(false)
}

mod previews {
    use nostrdb::Ndb;

    use crate::{
        account_manager::AccountManager,
        imgcache::ImageCache,
        test_data,
        ui::{profile::SimpleProfilePreviewController, Preview, PreviewConfig, View},
    };

    use super::AccountSelectionWidget;

    pub struct AccountSelectionPreview {
        is_mobile: bool,
        account_manager: AccountManager,
        ndb: Ndb,
        img_cache: ImageCache,
    }

    impl AccountSelectionPreview {
        fn new(is_mobile: bool) -> Self {
            let (account_manager, ndb, img_cache) = test_data::get_accmgr_and_ndb_and_imgcache();
            AccountSelectionPreview {
                is_mobile,
                account_manager,
                ndb,
                img_cache,
            }
        }
    }

    impl View for AccountSelectionPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            AccountSelectionWidget::new(
                self.is_mobile,
                &self.account_manager,
                SimpleProfilePreviewController::new(&self.ndb, &mut self.img_cache),
            )
            .ui(ui);
        }
    }

    impl<'a> Preview for AccountSelectionWidget<'a> {
        type Prev = AccountSelectionPreview;

        fn preview(cfg: PreviewConfig) -> Self::Prev {
            AccountSelectionPreview::new(cfg.is_mobile)
        }
    }
}
