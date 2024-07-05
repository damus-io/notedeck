use crate::{
    account_manager::UserAccount, colors::PINK, profile::DisplayName, route::Route,
    ui::profile_preview_controller, Damus, Result,
};

use nostrdb::Ndb;

use egui::{
    Align, Button, Color32, Frame, Id, Image, Layout, Margin, RichText, Rounding, ScrollArea,
    Sense, Vec2,
};

use super::profile::preview::SimpleProfilePreview;

pub struct AccountSelectionWidget {}

enum AccountSelectAction {
    RemoveAccount { _index: usize },
    SelectAccount { _index: usize },
    OpenAccountManagement,
}

#[derive(Default)]
struct AccountSelectResponse {
    action: Option<AccountSelectAction>,
}

impl AccountSelectionWidget {
    pub fn ui(app: &mut Damus, ui: &mut egui::Ui) {
        if !app.show_account_switcher {
            return;
        }

        if app.is_mobile() {
            Self::show_mobile(ui);
        } else {
            account_switcher_window(&mut app.show_account_switcher.clone()).show(
                ui.ctx(),
                |ui: &mut egui::Ui| {
                    let (account_selection_response, response) = Self::show(app, ui);
                    if let Some(action) = account_selection_response.action {
                        Self::perform_action(app, action);
                    }
                    response
                },
            );
        }
    }

    fn perform_action(app: &mut Damus, action: AccountSelectAction) {
        match action {
            AccountSelectAction::RemoveAccount { _index } => {
                app.account_manager.remove_account(_index)
            }
            AccountSelectAction::SelectAccount { _index } => {
                app.show_account_switcher = false;
                app.account_manager.select_account(_index);
            }
            AccountSelectAction::OpenAccountManagement => {
                app.show_account_switcher = false;
                app.global_nav.push(Route::ManageAccount);
                app.show_global_popup = true;
            }
        }
    }

    fn show(app: &mut Damus, ui: &mut egui::Ui) -> (AccountSelectResponse, egui::Response) {
        let mut res = AccountSelectResponse::default();
        let mut selected_index = app.account_manager.get_selected_account_index();

        let response = Frame::none()
            .outer_margin(8.0)
            .show(ui, |ui| {
                res = top_section_widget(ui);

                scroll_area().show(ui, |ui| {
                    if let Some(_index) = Self::show_accounts(app, ui) {
                        selected_index = Some(_index);
                        res.action = Some(AccountSelectAction::SelectAccount { _index });
                    }
                });
                ui.add_space(8.0);
                if ui.add(add_account_button()).clicked() {
                    app.global_nav.push(Route::AddAccount);
                    app.show_account_switcher = false;
                    app.show_global_popup = true;
                }

                if let Some(_index) = selected_index {
                    if let Some(account) = app.account_manager.get_account(_index) {
                        ui.add_space(8.0);
                        if Self::handle_sign_out(&app.ndb, ui, account) {
                            res.action = Some(AccountSelectAction::RemoveAccount { _index })
                        }
                    }
                }

                ui.add_space(8.0);
            })
            .response;

        (res, response)
    }

    fn handle_sign_out(ndb: &Ndb, ui: &mut egui::Ui, account: &UserAccount) -> bool {
        if let Ok(response) = Self::sign_out_button(ndb, ui, account) {
            response.clicked()
        } else {
            false
        }
    }

    fn show_mobile(ui: &mut egui::Ui) -> egui::Response {
        let _ = ui;
        todo!()
    }

    fn show_accounts(app: &mut Damus, ui: &mut egui::Ui) -> Option<usize> {
        profile_preview_controller::view_profile_previews(app, ui, account_switcher_card_ui)
    }

    fn sign_out_button(
        ndb: &Ndb,
        ui: &mut egui::Ui,
        account: &UserAccount,
    ) -> Result<egui::Response> {
        profile_preview_controller::show_with_nickname(
            ndb,
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

fn account_switcher_card_ui(
    ui: &mut egui::Ui,
    preview: SimpleProfilePreview,
    width: f32,
    is_selected: bool,
    index: usize,
) -> bool {
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

fn account_switcher_window(open: &'_ mut bool) -> egui::Window<'_> {
    egui::Window::new("account switcher")
        .title_bar(false)
        .collapsible(false)
        .anchor(egui::Align2::LEFT_BOTTOM, Vec2::new(4.0, -44.0))
        .fixed_size(Vec2::new(360.0, 406.0))
        .open(open)
        .movable(false)
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

    use crate::{
        test_data,
        ui::{Preview, PreviewConfig, View},
        Damus,
    };

    use super::AccountSelectionWidget;

    pub struct AccountSelectionPreview {
        app: Damus,
    }

    impl AccountSelectionPreview {
        fn new(is_mobile: bool) -> Self {
            let app = test_data::test_app(is_mobile);
            AccountSelectionPreview { app }
        }
    }

    impl View for AccountSelectionPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            AccountSelectionWidget::ui(&mut self.app, ui);
        }
    }

    impl Preview for AccountSelectionWidget {
        type Prev = AccountSelectionPreview;

        fn preview(cfg: PreviewConfig) -> Self::Prev {
            AccountSelectionPreview::new(cfg.is_mobile)
        }
    }
}
