use egui::{Button, Layout, SidePanel, Vec2, Widget};

use crate::{account_manager::AccountManager, ui::global_popup::GlobalPopupType};

use super::{
    profile::SimpleProfilePreviewController,
    state_in_memory::{STATE_ACCOUNT_SWITCHER, STATE_SIDE_PANEL},
    ProfilePic, View,
};

pub struct DesktopSidePanel<'a> {
    account_manager: &'a mut AccountManager,
    simple_preview_controller: SimpleProfilePreviewController<'a>,
}

static ID: &str = "left panel";

impl<'a> View for DesktopSidePanel<'a> {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.inner(ui);
    }
}

impl<'a> DesktopSidePanel<'a> {
    pub fn new(
        account_manager: &'a mut AccountManager,
        simple_preview_controller: SimpleProfilePreviewController<'a>,
    ) -> Self {
        DesktopSidePanel {
            account_manager,
            simple_preview_controller,
        }
    }

    pub fn inner(&mut self, ui: &mut egui::Ui) {
        let dark_mode = ui.ctx().style().visuals.dark_mode;
        let spacing_amt = 16.0;
        ui.with_layout(Layout::bottom_up(egui::Align::Center), |ui| {
            ui.add_space(spacing_amt);
            if self.pfp_button(ui).clicked() {
                STATE_SIDE_PANEL.set_state(ui.ctx(), Some(GlobalPopupType::AccountSwitcher));
                let previous_val = STATE_ACCOUNT_SWITCHER.get_state(ui.ctx());
                STATE_ACCOUNT_SWITCHER.set_state(ui.ctx(), !previous_val);
            }
            ui.add_space(spacing_amt);
            ui.add(settings_button(dark_mode));
            ui.add_space(spacing_amt);
            ui.add(add_column_button(dark_mode));
            ui.add_space(spacing_amt);
        });
    }

    fn pfp_button(&mut self, ui: &mut egui::Ui) -> egui::Response {
        if let Some(selected_account) = self.account_manager.get_selected_account() {
            if let Some(response) = self.simple_preview_controller.show_with_pfp(
                ui,
                &selected_account.key.pubkey,
                show_pfp(),
            ) {
                return response;
            }
        }

        add_button_to_ui(ui, no_account_pfp())
    }

    pub fn panel() -> SidePanel {
        egui::SidePanel::left(ID).resizable(false).exact_width(40.0)
    }
}

fn show_pfp() -> fn(ui: &mut egui::Ui, pfp: ProfilePic) -> egui::Response {
    |ui, pfp| {
        let response = pfp.ui(ui);
        ui.allocate_rect(response.rect, egui::Sense::click())
    }
}

fn settings_button(dark_mode: bool) -> egui::Button<'static> {
    let _ = dark_mode;
    let img_data = egui::include_image!("../../assets/icons/settings_dark_4x.png");

    egui::Button::image(egui::Image::new(img_data).max_width(32.0)).frame(false)
}

fn add_button_to_ui(ui: &mut egui::Ui, button: Button) -> egui::Response {
    ui.add_sized(Vec2::new(32.0, 32.0), button)
}

fn no_account_pfp() -> Button<'static> {
    Button::new("A")
        .rounding(20.0)
        .min_size(Vec2::new(38.0, 38.0))
}

fn add_column_button(dark_mode: bool) -> egui::Button<'static> {
    let _ = dark_mode;
    let img_data = egui::include_image!("../../assets/icons/add_column_dark_4x.png");

    egui::Button::image(egui::Image::new(img_data).max_width(32.0)).frame(false)
}

mod preview {
    use nostrdb::Ndb;

    use crate::{imgcache::ImageCache, test_data, ui::Preview};

    use super::*;

    pub struct DesktopSidePanelPreview {
        account_manager: AccountManager,
        ndb: Ndb,
        img_cache: ImageCache,
    }

    impl DesktopSidePanelPreview {
        fn new() -> Self {
            let (account_manager, ndb, img_cache) = test_data::get_accmgr_and_ndb_and_imgcache();
            DesktopSidePanelPreview {
                account_manager,
                ndb,
                img_cache,
            }
        }
    }

    impl View for DesktopSidePanelPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            let mut panel = DesktopSidePanel::new(
                &mut self.account_manager,
                SimpleProfilePreviewController::new(&self.ndb, &mut self.img_cache),
            );
            DesktopSidePanel::panel().show(ui.ctx(), |ui| panel.ui(ui));
        }
    }

    impl<'a> Preview for DesktopSidePanel<'a> {
        type Prev = DesktopSidePanelPreview;

        fn preview() -> Self::Prev {
            DesktopSidePanelPreview::new()
        }
    }
}
