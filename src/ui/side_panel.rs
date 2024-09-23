use egui::{Button, InnerResponse, Layout, RichText, SidePanel, Vec2, Widget};

use crate::{
    account_manager::AccountsRoute,
    column::Column,
    route::{Route, Router},
    ui::profile_preview_controller,
    Damus,
};

use super::{ProfilePic, View};

pub static SIDE_PANEL_WIDTH: f32 = 64.0;

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
    ComposeNote,
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
            .vertical(|ui| {
                let top_resp = ui
                    .with_layout(Layout::top_down(egui::Align::Center), |ui| {
                        let compose_resp = ui.add(compose_note_button());

                        if compose_resp.clicked() {
                            Some(InnerResponse::new(
                                SidePanelAction::ComposeNote,
                                compose_resp,
                            ))
                        } else {
                            None
                        }
                    })
                    .inner;

                let (pfp_resp, bottom_resp) = ui
                    .with_layout(Layout::bottom_up(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.y = spacing_amt;
                        let pfp_resp = self.pfp_button(ui);
                        let settings_resp = ui.add(settings_button(dark_mode));
                        let column_resp = ui.add(add_column_button(dark_mode));

                        let optional_inner = if pfp_resp.clicked() {
                            Some(egui::InnerResponse::new(
                                SidePanelAction::Account,
                                pfp_resp.clone(),
                            ))
                        } else if settings_resp.clicked() || settings_resp.hovered() {
                            Some(egui::InnerResponse::new(
                                SidePanelAction::Settings,
                                settings_resp,
                            ))
                        } else if column_resp.clicked() || column_resp.hovered() {
                            Some(egui::InnerResponse::new(
                                SidePanelAction::Columns,
                                column_resp,
                            ))
                        } else {
                            None
                        };

                        (pfp_resp, optional_inner)
                    })
                    .inner;

                if let Some(bottom_inner) = bottom_resp {
                    bottom_inner
                } else if let Some(top_inner) = top_resp {
                    top_inner
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

    pub fn perform_action(router: &mut Router<Route>, action: SidePanelAction) {
        match action {
            SidePanelAction::Panel => {} // TODO
            SidePanelAction::Account => {
                if router
                    .routes()
                    .iter()
                    .any(|&r| r == Route::Accounts(AccountsRoute::Accounts))
                {
                    // return if we are already routing to accounts
                    router.go_back();
                } else {
                    router.route_to(Route::accounts());
                }
            }
            SidePanelAction::Settings => {
                if router.routes().iter().any(|&r| r == Route::Relays) {
                    // return if we are already routing to accounts
                    router.go_back();
                } else {
                    router.route_to(Route::relays());
                }
            }
            SidePanelAction::Columns => (), // TODO
            SidePanelAction::ComposeNote => {
                if router.routes().iter().any(|&r| r == Route::ComposeNote) {
                    router.go_back();
                } else {
                    router.route_to(Route::ComposeNote);
                }
            }
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

fn compose_note_button() -> Button<'static> {
    Button::new(RichText::new("+").size(32.0)).frame(false)
}

mod preview {

    use egui_extras::{Size, StripBuilder};

    use crate::{
        test_data,
        ui::{Preview, PreviewConfig},
    };

    use super::*;

    pub struct DesktopSidePanelPreview {
        app: Damus,
    }

    impl DesktopSidePanelPreview {
        fn new() -> Self {
            let mut app = test_data::test_app();
            app.columns
                .columns_mut()
                .push(Column::new(vec![Route::accounts()]));
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

                        DesktopSidePanel::perform_action(
                            self.app.columns.columns_mut()[0].router_mut(),
                            response.action,
                        );
                    });
                });
        }
    }

    impl<'a> Preview for DesktopSidePanel<'a> {
        type Prev = DesktopSidePanelPreview;

        fn preview(_cfg: PreviewConfig) -> Self::Prev {
            DesktopSidePanelPreview::new()
        }
    }
}
