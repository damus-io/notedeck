use egui::{Button, Layout, SidePanel, Vec2, Widget};

use crate::{ui::profile_preview_controller, Damus};

use super::{ProfilePic, View};

pub struct DesktopSidePanel<'a> {
    app: &'a mut Damus,
}

impl<'a> View for DesktopSidePanel<'a> {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.show(ui);
    }
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

impl<'a> DesktopSidePanel<'a> {
    pub fn new(app: &'a mut Damus) -> Self {
        DesktopSidePanel { app }
    }

    pub fn panel() -> SidePanel {
        egui::SidePanel::left("side_panel")
            .resizable(false)
            .exact_width(40.0)
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> SidePanelResponse {
        let dark_mode = ui.ctx().style().visuals.dark_mode;
        let spacing_amt = 16.0;

        let inner = ui
            .with_layout(Layout::bottom_up(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.y = spacing_amt;
                let pfp_resp = self.pfp_button(ui);
                let settings_resp = ui.add(settings_button(dark_mode));
                let column_resp = ui.add(add_column_button(dark_mode));

                if pfp_resp.clicked() {
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

    fn pfp_button(&mut self, ui: &mut egui::Ui) -> egui::Response {
        if let Some(resp) =
            profile_preview_controller::show_with_selected_pfp(self.app, ui, show_pfp())
        {
            resp
        } else {
            add_button_to_ui(ui, no_account_pfp())
        }
    }

    pub fn perform_action(app: &mut Damus, action: SidePanelAction) {
        match action {
            SidePanelAction::Panel => {} // TODO
            SidePanelAction::Account => app.show_account_switcher = !app.show_account_switcher,
            SidePanelAction::Settings => {} // TODO
            SidePanelAction::Columns => (), // TODO
        }
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

    use egui_extras::{Size, StripBuilder};

    use crate::{
        test_data,
        ui::{AccountSelectionWidget, Preview, PreviewConfig},
    };

    use super::*;

    pub struct DesktopSidePanelPreview {
        app: Damus,
    }

    impl DesktopSidePanelPreview {
        fn new() -> Self {
            let app = test_data::test_app();
            DesktopSidePanelPreview { app }
        }
    }

    impl View for DesktopSidePanelPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            StripBuilder::new(ui)
                .size(Size::exact(40.0))
                .sizes(Size::remainder(), 0)
                .clip(true)
                .horizontal(|mut strip| {
                    strip.cell(|ui| {
                        let mut panel = DesktopSidePanel::new(&mut self.app);
                        let response = panel.show(ui);
                        DesktopSidePanel::perform_action(&mut self.app, response.action);
                    });
                });

            AccountSelectionWidget::ui(&mut self.app, ui);
        }
    }

    impl<'a> Preview for DesktopSidePanel<'a> {
        type Prev = DesktopSidePanelPreview;

        fn preview(_cfg: PreviewConfig) -> Self::Prev {
            DesktopSidePanelPreview::new()
        }
    }
}
