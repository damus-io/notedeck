use egui::{Align2, CentralPanel, RichText, Vec2, Window};

use crate::Damus;

use super::{
    profile::SimpleProfilePreviewController,
    state_in_memory::{STATE_ACCOUNT_MANAGEMENT, STATE_ACCOUNT_SWITCHER, STATE_SIDE_PANEL},
    AccountManagementView, AccountSelectionWidget, View,
};

#[derive(Clone, Copy, Debug)]
pub enum GlobalPopupType {
    AccountManagement,
    AccountSwitcher,
}

static ACCOUNT_MANAGEMENT_TITLE: &str = "Manage accounts";
static ACCOUNT_SWITCHER_TITLE: &str = "Account switcher";

impl GlobalPopupType {
    pub fn title(&self) -> &'static str {
        match self {
            Self::AccountManagement => ACCOUNT_MANAGEMENT_TITLE,
            Self::AccountSwitcher => ACCOUNT_SWITCHER_TITLE,
        }
    }
}

pub trait FromApp<'a> {
    fn from_app(app: &'a mut crate::Damus) -> Self
    where
        Self: Sized;
}

fn title(title_str: &'static str) -> RichText {
    RichText::new(title_str).size(24.0)
}

fn overlay_window<'a>(
    open: &'a mut bool,
    window_size: Vec2,
    title_str: &'static str,
) -> Window<'a> {
    egui::Window::new(title(title_str))
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .collapsible(false)
        .auto_sized()
        .movable(false)
        .open(open)
        .default_size(window_size)
}

fn account_switcher_window(open: &'_ mut bool) -> Window<'_> {
    egui::Window::new("account switcher")
        .title_bar(false)
        .collapsible(false)
        .anchor(Align2::LEFT_BOTTOM, Vec2::new(0.0, -52.0))
        .fixed_size(Vec2::new(360.0, 406.0))
        .open(open)
        .movable(false)
}

static MARGIN: Vec2 = Vec2 { x: 100.0, y: 100.0 };

pub struct DesktopGlobalPopup<'a> {
    app: &'a mut Damus,
}

impl<'a> View for DesktopGlobalPopup<'a> {
    fn ui(&mut self, ui: &mut egui::Ui) {
        DesktopGlobalPopup::global_popup(self.app, ui.ctx())
    }
}

impl<'a> DesktopGlobalPopup<'a> {
    pub fn new(app: &'a mut Damus) -> Self {
        DesktopGlobalPopup { app }
    }
    pub fn global_popup(app: &mut Damus, ctx: &egui::Context) {
        CentralPanel::default().show(ctx, |ui| {
            if let Some(popup) = STATE_SIDE_PANEL.get_state(ctx) {
                match popup {
                    GlobalPopupType::AccountManagement => {
                        Self::account_management(app, ctx, ui, popup.title());
                    }
                    GlobalPopupType::AccountSwitcher => {
                        let mut show_account_switcher = STATE_ACCOUNT_SWITCHER.get_state(ctx);
                        if show_account_switcher {
                            STATE_ACCOUNT_MANAGEMENT.set_state(ctx, false);
                            account_switcher_window(&mut show_account_switcher).show(ctx, |ui| {
                                AccountSelectionWidget::new(
                                    &mut app.account_manager,
                                    SimpleProfilePreviewController::new(
                                        &app.ndb,
                                        &mut app.img_cache,
                                    ),
                                )
                                .ui(ui);
                            });
                        }
                    }
                }
            }
        });
    }

    fn account_management(
        app: &mut Damus,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        title: &'static str,
    ) {
        let available_size = ui.available_size();
        let window_size = available_size - MARGIN;
        let mut show_account_management = STATE_ACCOUNT_MANAGEMENT.get_state(ctx);
        if show_account_management {
            overlay_window(&mut show_account_management, window_size, title).show(ctx, |ui| {
                AccountManagementView::from_app(app).ui(ui);
            });
            // user could have closed the window, set the new state in egui memory
            STATE_ACCOUNT_MANAGEMENT.set_state(ctx, show_account_management);
        }
    }
}

mod preview {
    use crate::{
        test_data,
        ui::{DesktopSidePanel, Preview, View},
        Damus,
    };

    use super::DesktopGlobalPopup;

    pub struct GlobalPopupPreview {
        app: Damus,
    }

    impl<'a> Preview for DesktopGlobalPopup<'a> {
        type Prev = GlobalPopupPreview;

        fn preview() -> Self::Prev {
            GlobalPopupPreview::new()
        }
    }

    impl GlobalPopupPreview {
        fn new() -> Self {
            let mut app = Damus::mock(".");
            let accounts = test_data::get_test_accounts();
            accounts
                .into_iter()
                .for_each(|acc| app.account_manager.add_account(acc.key, || {}));
            GlobalPopupPreview { app }
        }
    }

    impl View for GlobalPopupPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            let mut panel = DesktopSidePanel::new();
            DesktopSidePanel::panel().show(ui.ctx(), |ui| panel.ui(ui));
            DesktopGlobalPopup::new(&mut self.app).ui(ui);
        }
    }
}
