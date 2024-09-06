use crate::colors::PINK;
use crate::{
    account_manager::AccountManager,
    app_style::NotedeckTextStyle,
    ui,
    ui::{profile_preview_controller, Preview, PreviewConfig, View},
    Damus,
};
use egui::{Align, Button, Frame, Image, Layout, Response, RichText, ScrollArea, Vec2};

use super::profile::preview::SimpleProfilePreview;
use super::profile::ProfilePreviewOp;

pub struct AccountManagementView {}

impl AccountManagementView {
    pub fn ui(app: &mut Damus, ui: &mut egui::Ui) -> Response {
        Frame::none()
            .outer_margin(12.0)
            .show(ui, |ui| {
                Self::top_section_buttons_widget(ui);

                ui.add_space(8.0);
                scroll_area().show(ui, |ui| Self::show_accounts(app, ui));
            })
            .response
    }

    fn show_accounts(app: &mut Damus, ui: &mut egui::Ui) {
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_size_before_wrap().x, 32.0),
            Layout::top_down(egui::Align::Min),
            |ui| {
                // create all account 'cards' and get the indicies the user requested to remove
                let maybe_remove = profile_preview_controller::set_profile_previews(
                    app,
                    ui,
                    account_card_ui(), // closure for creating an account 'card'
                );

                // remove all account indicies user requested
                if let Some(indicies_to_remove) = maybe_remove {
                    Self::remove_accounts(&mut app.account_manager, indicies_to_remove);
                }
            },
        );
    }

    fn remove_accounts(manager: &mut AccountManager, account_indices: Vec<usize>) {
        account_indices
            .iter()
            .for_each(|index| manager.remove_account(*index));
    }

    fn top_section_buttons_widget(ui: &mut egui::Ui) -> egui::Response {
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(ui.available_size_before_wrap().x, 32.0),
                Layout::left_to_right(egui::Align::Center),
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

fn account_card_ui() -> fn(
    ui: &mut egui::Ui,
    preview: SimpleProfilePreview,
    width: f32,
    is_selected: bool,
) -> Option<ProfilePreviewOp> {
    |ui, preview, width, is_selected| {
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
    use crate::test_data;

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
            AccountManagementView::ui(&mut self.app, ui);
        }
    }

    impl Preview for AccountManagementView {
        type Prev = AccountManagementPreview;

        fn preview(_cfg: PreviewConfig) -> Self::Prev {
            AccountManagementPreview::new()
        }
    }
}
