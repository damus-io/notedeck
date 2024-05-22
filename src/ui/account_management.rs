use crate::ui::global_popup::FromApp;
use crate::{
    account_manager::{AccountManager, UserAccount},
    app_style::NotedeckTextStyle,
    ui::{self, Preview, View},
};
use egui::{Align, Button, Frame, Id, Layout, Margin, RichText, ScrollArea, Sense, Vec2};

use super::global_popup::GlobalPopupType;
use super::profile::preview::SimpleProfilePreview;
use super::profile::SimpleProfilePreviewController;
use super::state_in_memory::STATE_ACCOUNT_MANAGEMENT;

pub struct AccountManagementView<'a> {
    account_manager: &'a mut AccountManager,
    simple_preview_controller: SimpleProfilePreviewController<'a>,
}

impl<'a> View for AccountManagementView<'a> {
    fn ui(&mut self, ui: &mut egui::Ui) {
        if ui::is_mobile() {
            self.show_mobile(ui);
        } else {
            self.show(ui);
        }
    }
}

impl<'a> AccountManagementView<'a> {
    pub fn new(
        account_manager: &'a mut AccountManager,
        simple_preview_controller: SimpleProfilePreviewController<'a>,
    ) -> Self {
        AccountManagementView {
            account_manager,
            simple_preview_controller,
        }
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        ui.add(self.buttons_widget());
        ui.add_space(8.0);
        scroll_area().show(ui, |ui| {
            self.show_accounts(ui);
        });
    }

    fn show_accounts(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let maybe_remove = self.simple_preview_controller.set_profile_previews(
                self.account_manager,
                ui,
                STATE_ACCOUNT_MANAGEMENT.get_state(ui.ctx()),
                desktop_account_card_ui(),
            );

            self.maybe_remove_accounts(maybe_remove);
        });
    }

    fn show_accounts_mobile(&mut self, ui: &mut egui::Ui) {
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_size_before_wrap().x, 32.0),
            Layout::top_down(egui::Align::Min),
            |ui| {
                // create all account 'cards' and get the indicies the user requested to remove
                let maybe_remove = self.simple_preview_controller.set_profile_previews(
                    self.account_manager,
                    ui,
                    STATE_ACCOUNT_MANAGEMENT.get_state(ui.ctx()),
                    mobile_account_card_ui(), // closure for creating an account 'card'
                );

                // remove all account indicies user requested
                self.maybe_remove_accounts(maybe_remove);
            },
        );
    }

    fn maybe_remove_accounts(&mut self, account_indices: Option<Vec<usize>>) {
        if let Some(to_remove) = account_indices {
            to_remove
                .iter()
                .for_each(|index| self.account_manager.remove_account(*index));
        }
    }

    fn show_mobile(&mut self, ui: &mut egui::Ui) -> egui::Response {
        egui::CentralPanel::default()
            .show(ui.ctx(), |ui| {
                ui.add(mobile_title());
                ui.add(self.buttons_widget());
                ui.add_space(8.0);
                scroll_area().show(ui, |ui| {
                    self.show_accounts_mobile(ui);
                });
            })
            .response
    }

    fn buttons_widget(&mut self) -> impl egui::Widget + '_ {
        |ui: &mut egui::Ui| {
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_size_before_wrap().x, 32.0),
                    Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        if STATE_ACCOUNT_MANAGEMENT.get_state(ui.ctx()) {
                            if ui.add(done_account_button()).clicked() {
                                STATE_ACCOUNT_MANAGEMENT.set_state(ui.ctx(), false);
                            }
                        } else if ui.add(edit_account_button()).clicked() {
                            STATE_ACCOUNT_MANAGEMENT.set_state(ui.ctx(), true);
                        }
                    },
                );

                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_size_before_wrap().x, 32.0),
                    Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if ui.add(add_account_button()).clicked() {
                            // TODO: route to AccountLoginView
                        }
                    },
                );
            })
            .response
        }
    }
}

fn mobile_account_card_ui(
) -> fn(ui: &mut egui::Ui, preview: SimpleProfilePreview, edit_mode: bool) -> bool {
    |ui, preview, edit_mode| {
        let mut should_remove = false;

        ui.add_sized(
            Vec2::new(ui.available_width(), 50.0),
            |ui: &mut egui::Ui| {
                Frame::none()
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.add(preview);
                            if edit_mode {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    should_remove =
                                        ui.add(delete_button(ui.visuals().dark_mode)).clicked();
                                });
                            }
                        });
                    })
                    .response
            },
        );
        ui.add_space(16.0);
        should_remove
    }
}

fn desktop_account_card_ui(
) -> fn(ui: &mut egui::Ui, preview: SimpleProfilePreview, edit_mode: bool) -> bool {
    |ui: &mut egui::Ui, preview, edit_mode| {
        let mut should_remove = false;

        ui.add_sized(preview.dimensions(), |ui: &mut egui::Ui| {
            simple_preview_frame(ui)
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add(preview);
                        if edit_mode {
                            should_remove = ui.add(delete_button(ui.visuals().dark_mode)).clicked();
                        }
                    });
                })
                .response
        });
        should_remove
    }
}

impl<'a> FromApp<'a> for AccountManagementView<'a> {
    fn from_app(app: &'a mut crate::Damus) -> Self {
        AccountManagementView::new(
            &mut app.account_manager,
            SimpleProfilePreviewController::new(&app.ndb, &mut app.img_cache),
        )
    }
}

fn simple_preview_frame(ui: &mut egui::Ui) -> Frame {
    Frame::none()
        .rounding(ui.visuals().window_rounding)
        .fill(ui.visuals().window_fill)
        .stroke(ui.visuals().window_stroke)
        .outer_margin(Margin::same(2.0))
        .inner_margin(12.0)
}

fn mobile_title() -> impl egui::Widget {
    |ui: &mut egui::Ui| {
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new(GlobalPopupType::AccountManagement.title())
                    .text_style(NotedeckTextStyle::Heading2.text_style())
                    .strong(),
            );
        })
        .response
    }
}

fn scroll_area() -> ScrollArea {
    egui::ScrollArea::vertical()
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .auto_shrink([false; 2])
}

fn add_account_button() -> Button<'static> {
    Button::new("Add Account").min_size(Vec2::new(0.0, 32.0))
}

fn edit_account_button() -> Button<'static> {
    Button::new("Edit").min_size(Vec2::new(0.0, 32.0))
}

fn done_account_button() -> Button<'static> {
    Button::new("Done").min_size(Vec2::new(0.0, 32.0))
}

fn delete_button(_dark_mode: bool) -> egui::Button<'static> {
    let img_data = egui::include_image!("../../assets/icons/delete_icon_4x.png");

    egui::Button::image(egui::Image::new(img_data).max_width(30.0)).frame(true)
}

pub struct AccountSelectionWidget<'a> {
    account_manager: &'a mut AccountManager,
    simple_preview_controller: SimpleProfilePreviewController<'a>,
}

impl<'a> AccountSelectionWidget<'a> {
    fn ui(&'a mut self, ui: &mut egui::Ui) -> Option<&'a UserAccount> {
        let mut result: Option<&'a UserAccount> = None;
        scroll_area().show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let clicked_at = self.simple_preview_controller.view_profile_previews(
                    self.account_manager,
                    ui,
                    |ui, preview, index| {
                        let resp = ui.add_sized(preview.dimensions(), |ui: &mut egui::Ui| {
                            simple_preview_frame(ui)
                                .show(ui, |ui| {
                                    ui.vertical_centered(|ui| {
                                        ui.add(preview);
                                    });
                                })
                                .response
                        });

                        ui.interact(resp.rect, Id::new(index), Sense::click())
                            .clicked()
                    },
                );

                if let Some(index) = clicked_at {
                    result = self.account_manager.get_account(index);
                };
            });
        });
        result
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

// PREVIEWS

mod preview {
    use nostrdb::{Config, Ndb};

    use super::*;
    use crate::imgcache::ImageCache;
    use crate::key_storage::KeyStorage;
    use crate::relay_generation::RelayGenerator;
    use crate::test_data;
    use std::path::Path;

    pub struct AccountManagementPreview {
        account_manager: AccountManager,
        ndb: Ndb,
        img_cache: ImageCache,
    }

    fn get_ndb_and_img_cache() -> (Ndb, ImageCache) {
        let mut config = Config::new();
        config.set_ingester_threads(2);

        let db_dir = Path::new(".");
        let path = db_dir.to_str().unwrap();
        let ndb = Ndb::new(path, &config).expect("ndb");
        let imgcache_dir = db_dir.join("cache/img");
        let img_cache = ImageCache::new(imgcache_dir);
        (ndb, img_cache)
    }

    impl AccountManagementPreview {
        fn new() -> Self {
            let mut account_manager =
                AccountManager::new(None, KeyStorage::None, RelayGenerator::Constant, || {});
            let accounts = test_data::get_test_accounts();
            accounts
                .into_iter()
                .for_each(|acc| account_manager.add_account(acc.key, || {}));
            let (ndb, img_cache) = get_ndb_and_img_cache();

            AccountManagementPreview {
                account_manager,
                ndb,
                img_cache,
            }
        }
    }

    impl View for AccountManagementPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            ui.add_space(24.0);
            AccountManagementView::new(
                &mut self.account_manager,
                SimpleProfilePreviewController::new(&self.ndb, &mut self.img_cache),
            )
            .ui(ui);
        }
    }

    impl<'a> Preview for AccountManagementView<'a> {
        type Prev = AccountManagementPreview;

        fn preview() -> Self::Prev {
            AccountManagementPreview::new()
        }
    }

    pub struct AccountSelectionPreview {
        account_manager: AccountManager,
        ndb: Ndb,
        img_cache: ImageCache,
    }

    impl AccountSelectionPreview {
        fn new() -> Self {
            let mut account_manager =
                AccountManager::new(None, KeyStorage::None, RelayGenerator::Constant, || {});
            let accounts = test_data::get_test_accounts();
            accounts
                .into_iter()
                .for_each(|acc| account_manager.add_account(acc.key, || {}));
            let (ndb, img_cache) = get_ndb_and_img_cache();
            AccountSelectionPreview {
                account_manager,
                ndb,
                img_cache,
            }
        }
    }

    impl View for AccountSelectionPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            let mut widget = AccountSelectionWidget::new(
                &mut self.account_manager,
                SimpleProfilePreviewController::new(&self.ndb, &mut self.img_cache),
            );

            if let Some(account) = widget.ui(ui) {
                println!("User made selection: {:?}", account.key);
            }
        }
    }

    impl<'a> Preview for AccountSelectionWidget<'a> {
        type Prev = AccountSelectionPreview;

        fn preview() -> Self::Prev {
            AccountSelectionPreview::new()
        }
    }
}
