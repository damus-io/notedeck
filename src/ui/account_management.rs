use egui::{Align, Align2, Button, Frame, Layout, Margin, RichText, ScrollArea, Vec2, Window};

use crate::{
    account_manager::{AccountManager, SimpleProfilePreviewController},
    app_style::NotedeckTextStyle,
    ui::{self, Preview, View},
};

pub struct AccountManagementView<'a> {
    account_manager: AccountManager<'a>,
    simple_preview_controller: SimpleProfilePreviewController<'a>,
    edit_mode: &'a mut bool,
}

impl<'a> View for AccountManagementView<'a> {
    fn ui(&mut self, ui: &mut egui::Ui) {
        if ui::is_mobile(ui.ctx()) {
            self.show_mobile(ui);
        } else {
            self.show(ui);
        }
    }
}

impl<'a> AccountManagementView<'a> {
    pub fn new(
        account_manager: AccountManager<'a>,
        simple_preview_controller: SimpleProfilePreviewController<'a>,
        edit_mode: &'a mut bool,
    ) -> Self {
        AccountManagementView {
            account_manager,
            simple_preview_controller,
            edit_mode,
        }
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        ui.add_space(24.0);
        let screen_size = ui.ctx().screen_rect();
        let margin_amt = 128.0;
        let window_size = Vec2::new(
            screen_size.width() - margin_amt,
            screen_size.height() - margin_amt,
        );

        Window::new("Account Management")
            .frame(Frame::window(ui.style()))
            .collapsible(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .resizable(false)
            .title_bar(false)
            .default_size(window_size)
            .show(ui.ctx(), |ui| {
                ui.add(title());
                ui.add(self.buttons_widget());
                ui.add_space(8.0);
                self.show_accounts(ui);
            });
    }

    fn show_accounts(&mut self, ui: &mut egui::Ui) {
        scroll_area().show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let maybe_remove = self.simple_preview_controller.set_profile_previews(
                    &self.account_manager,
                    ui,
                    *self.edit_mode,
                    |ui, preview, edit_mode| {
                        let mut should_remove = false;

                        ui.add_sized(preview.dimensions(), |ui: &mut egui::Ui| {
                            simple_preview_frame(ui)
                                .show(ui, |ui| {
                                    ui.vertical_centered(|ui| {
                                        ui.add(preview);
                                        if edit_mode {
                                            should_remove = ui
                                                .add(delete_button(ui.visuals().dark_mode))
                                                .clicked();
                                        }
                                    });
                                })
                                .response
                        });
                        should_remove
                    },
                );

                self.maybe_remove_accounts(maybe_remove);
            });
        });
    }

    fn show_accounts_mobile(&mut self, ui: &mut egui::Ui) {
        scroll_area().show(ui, |ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(ui.available_size_before_wrap().x, 32.0),
                Layout::top_down(egui::Align::Min),
                |ui| {
                    let maybe_remove = self.simple_preview_controller.set_profile_previews(
                        &self.account_manager,
                        ui,
                        *self.edit_mode,
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
                                                    ui.with_layout(
                                                        Layout::right_to_left(Align::Center),
                                                        |ui| {
                                                            should_remove = ui
                                                                .add(delete_button(
                                                                    ui.visuals().dark_mode,
                                                                ))
                                                                .clicked();
                                                        },
                                                    );
                                                }
                                            });
                                        })
                                        .response
                                },
                            );
                            ui.add_space(16.0);
                            should_remove
                        },
                    );

                    self.maybe_remove_accounts(maybe_remove);
                },
            );
        });
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
                ui.add(title());
                ui.add(self.buttons_widget());
                ui.add_space(8.0);
                self.show_accounts_mobile(ui);
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
                        if *self.edit_mode {
                            if ui.add(done_account_button()).clicked() {
                                *self.edit_mode = false;
                            }
                        } else if ui.add(edit_account_button()).clicked() {
                            *self.edit_mode = true;
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

fn simple_preview_frame(ui: &mut egui::Ui) -> Frame {
    Frame::none()
        .rounding(ui.visuals().window_rounding)
        .fill(ui.visuals().window_fill)
        .stroke(ui.visuals().window_stroke)
        .outer_margin(Margin::same(2.0))
        .inner_margin(12.0)
}

fn title() -> impl egui::Widget {
    |ui: &mut egui::Ui| {
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new("Accounts")
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

// PREVIEWS

mod preview {
    use nostr_sdk::{Keys, PublicKey};
    use nostrdb::{Config, Ndb};

    use super::*;
    use crate::key_storage::KeyStorage;
    use crate::relay_generation::RelayGenerator;
    use crate::{account_manager::UserAccount, imgcache::ImageCache, test_data};
    use std::path::Path;

    pub struct AccountManagementPreview {
        accounts: Vec<UserAccount>,
        ndb: Ndb,
        img_cache: ImageCache,
        edit_mode: bool,
    }

    impl AccountManagementPreview {
        fn new() -> Self {
            let account_hexes = [
                "3efdaebb1d8923ebd99c9e7ace3b4194ab45512e2be79c1b7d68d9243e0d2681",
                "32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245",
                "bd1e19980e2c91e6dc657e92c25762ca882eb9272d2579e221f037f93788de91",
                "5c10ed0678805156d39ef1ef6d46110fe1e7e590ae04986ccf48ba1299cb53e2",
                "4c96d763eb2fe01910f7e7220b7c7ecdbe1a70057f344b9f79c28af080c3ee30",
                "edf16b1dd61eab353a83af470cc13557029bff6827b4cb9b7fc9bdb632a2b8e6",
                "3efdaebb1d8923ebd99c9e7ace3b4194ab45512e2be79c1b7d68d9243e0d2681",
                "32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245",
                "32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245",
                "32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245",
            ];

            let accounts: Vec<UserAccount> = account_hexes
                .iter()
                .map(|account_hex| {
                    let key = Keys::from_public_key(PublicKey::from_hex(account_hex).unwrap());

                    UserAccount {
                        key,
                        relays: test_data::sample_pool(),
                    }
                })
                .collect();

            let mut config = Config::new();
            config.set_ingester_threads(2);

            let db_dir = Path::new(".");
            let path = db_dir.to_str().unwrap();
            let ndb = Ndb::new(path, &config).expect("ndb");
            let imgcache_dir = db_dir.join("cache/img");
            let img_cache = ImageCache::new(imgcache_dir);

            AccountManagementPreview {
                accounts,
                ndb,
                img_cache,
                edit_mode: false,
            }
        }
    }

    impl View for AccountManagementPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            let account_manager = AccountManager::new(
                &mut self.accounts,
                KeyStorage::None,
                RelayGenerator::Constant,
            );

            AccountManagementView::new(
                account_manager,
                SimpleProfilePreviewController::new(&self.ndb, &mut self.img_cache),
                &mut self.edit_mode,
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
}
