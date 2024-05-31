use egui::{Button, Layout, SidePanel, Vec2, Widget};

use crate::account_manager::AccountManager;

use super::{profile::SimpleProfilePreviewController, ProfilePic, View};

pub struct DesktopSidePanel<'a> {
    selected_account: Option<&'a [u8; 32]>,
    simple_preview_controller: SimpleProfilePreviewController<'a>,
}

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum SidePanelAction {
    Panel,
    Account,
    Settings,
    Columns,
}

pub struct SidePanelResponse {
    pub response: egui::Response,
    pub action: SidePanelAction,
}

impl SidePanelResponse {
    fn new(action: SidePanelAction, response: egui::Response) -> Self {
        SidePanelResponse { action, response }
    }
}

impl<'a> Widget for DesktopSidePanel<'a> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        self.show(ui).response
    }
}

impl<'a> DesktopSidePanel<'a> {
    pub fn new(
        selected_account: Option<&'a [u8; 32]>,
        simple_preview_controller: SimpleProfilePreviewController<'a>,
    ) -> Self {
        DesktopSidePanel {
            selected_account,
            simple_preview_controller,
        }
    }

    pub fn panel() -> SidePanel {
        egui::SidePanel::left("side_panel")
            .resizable(false)
            .exact_width(40.0)
    }

    pub fn show(self, ui: &mut egui::Ui) -> SidePanelResponse {
        let dark_mode = ui.ctx().style().visuals.dark_mode;
        let spacing_amt = 16.0;

        let inner = ui
            .with_layout(Layout::bottom_up(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.y = spacing_amt;
                let pfp_resp = self.pfp_button(ui);
                let settings_resp = ui.add(settings_button(dark_mode));
                let column_resp = ui.add(add_column_button(dark_mode));

                if pfp_resp.clicked() || pfp_resp.hovered() {
                    egui::InnerResponse::new(SidePanelAction::Account, pfp_resp)
                } else if settings_resp.clicked() || settings_resp.hovered() {
                    egui::InnerResponse::new(SidePanelAction::Settings, settings_resp)
                } else if column_resp.clicked() || column_resp.hovered() {
                    egui::InnerResponse::new(SidePanelAction::Columns, column_resp)
                } else {
                    egui::InnerResponse::new(SidePanelAction::Panel, pfp_resp)
                }
            })
            .inner;

        SidePanelResponse::new(inner.inner, inner.response)
    }

    fn pfp_button(self, ui: &mut egui::Ui) -> egui::Response {
        if let Some(selected_account) = self.selected_account {
            if let Some(response) =
                self.simple_preview_controller
                    .show_with_pfp(ui, selected_account, show_pfp())
            {
                return response;
            }
        }

        add_button_to_ui(ui, no_account_pfp())
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
            let selected_account = self
                .account_manager
                .get_selected_account()
                .map(|x| x.pubkey.bytes());

            let panel = DesktopSidePanel::new(
                selected_account,
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
