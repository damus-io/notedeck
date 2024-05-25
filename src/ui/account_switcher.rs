use crate::{account_manager::UserAccount, colors::PINK, ui};
use egui::{
    Align, Button, Color32, Frame, Id, Image, Layout, Margin, RichText, Rounding, ScrollArea,
    Sense, Vec2,
};

use crate::account_manager::AccountManager;

use super::{
    profile::{preview::SimpleProfilePreview, SimpleProfilePreviewController},
    state_in_memory::{STATE_ACCOUNT_MANAGEMENT, STATE_ACCOUNT_SWITCHER, STATE_SIDE_PANEL},
};

pub struct AccountSelectionWidget<'a> {
    account_manager: &'a mut AccountManager,
    simple_preview_controller: SimpleProfilePreviewController<'a>,
}

impl<'a> AccountSelectionWidget<'a> {
    pub fn ui(&'a mut self, ui: &mut egui::Ui) {
        if ui::is_mobile() {
            self.show_mobile(ui);
        } else {
            self.show(ui);
        }
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        Frame::none().outer_margin(8.0).show(ui, |ui| {
            ui.add(top_section_widget());
            scroll_area().show(ui, |ui| {
                self.show_accounts(ui);
            });
            ui.add_space(8.0);
            ui.add(add_account_button());

            if let Some(account_index) = self.account_manager.get_selected_account_index() {
                ui.add_space(8.0);
                if self.handle_sign_out(ui, account_index) {
                    self.account_manager.remove_account(account_index);
                }
            }

            ui.add_space(8.0);
        });
    }

    fn handle_sign_out(&mut self, ui: &mut egui::Ui, account_index: usize) -> bool {
        if let Some(account) = self.account_manager.get_account(account_index) {
            if let Some(response) = self.sign_out_button(ui, account) {
                return response.clicked();
            }
        }
        false
    }

    fn show_mobile(&mut self, ui: &mut egui::Ui) -> egui::Response {
        let _ = ui;
        todo!()
    }

    fn show_accounts(&mut self, ui: &mut egui::Ui) {
        self.simple_preview_controller.view_profile_previews(
            self.account_manager,
            ui,
            account_switcher_card_ui(),
        );
    }

    fn sign_out_button(&self, ui: &mut egui::Ui, account: &UserAccount) -> Option<egui::Response> {
        self.simple_preview_controller.show_with_nickname(
            ui,
            &account.key.pubkey,
            |ui, username| {
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

impl<'a> AccountSelectionWidget<'a> {
    pub fn new(
        account_manager: &'a mut AccountManager,
        simple_preview_controller: SimpleProfilePreviewController<'a>,
    ) -> Self {
        AccountSelectionWidget {
            account_manager,
            simple_preview_controller,
        }
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

fn top_section_widget() -> impl egui::Widget {
    |ui: &mut egui::Ui| {
        ui.horizontal(|ui| {
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
                        STATE_ACCOUNT_SWITCHER.set_state(ui.ctx(), false);
                        STATE_SIDE_PANEL.set_state(
                            ui.ctx(),
                            Some(ui::global_popup::GlobalPopupType::AccountManagement),
                        );
                        STATE_ACCOUNT_MANAGEMENT.set_state(ui.ctx(), true);
                    }
                },
            );
        })
        .response
    }
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
        ui::{profile::SimpleProfilePreviewController, Preview, View},
    };

    use super::AccountSelectionWidget;

    pub struct AccountSelectionPreview {
        account_manager: AccountManager,
        ndb: Ndb,
        img_cache: ImageCache,
    }

    impl AccountSelectionPreview {
        fn new() -> Self {
            let (account_manager, ndb, img_cache) = test_data::get_accmgr_and_ndb_and_imgcache();
            AccountSelectionPreview {
                account_manager,
                ndb,
                img_cache,
            }
        }
    }

    impl View for AccountSelectionPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            AccountSelectionWidget::new(
                &mut self.account_manager,
                SimpleProfilePreviewController::new(&self.ndb, &mut self.img_cache),
            )
            .ui(ui);
        }
    }

    impl<'a> Preview for AccountSelectionWidget<'a> {
        type Prev = AccountSelectionPreview;

        fn preview() -> Self::Prev {
            AccountSelectionPreview::new()
        }
    }
}
