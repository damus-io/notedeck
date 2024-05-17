use egui::{Align2, CentralPanel, RichText, Vec2, Window};

use crate::Damus;

use super::{
    persist_state::{PERSISTED_GLOBAL_POPUP, PERSISTED_SIDE_PANEL},
    AccountManagementView, View,
};

#[derive(Clone, Copy, Debug)]
pub enum GlobalPopupType {
    AccountManagement,
}

static ACCOUNT_MANAGEMENT_TITLE: &str = "Account Management";

impl GlobalPopupType {
    pub fn title(&self) -> &'static str {
        match self {
            Self::AccountManagement => ACCOUNT_MANAGEMENT_TITLE,
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
            let available_size = ui.available_size();
            let window_size = available_size - MARGIN;

            if let Some(popup) = PERSISTED_SIDE_PANEL.get_state(ctx) {
                let mut show_global_popup = PERSISTED_GLOBAL_POPUP.get_state(ctx);
                if show_global_popup {
                    overlay_window(&mut show_global_popup, window_size, popup.title()).show(
                        ctx,
                        |ui| {
                            match popup {
                                GlobalPopupType::AccountManagement => {
                                    AccountManagementView::from_app(app).ui(ui)
                                }
                            };
                        },
                    );

                    // user could have closed the window, set the new state in egui memory
                    PERSISTED_GLOBAL_POPUP.set_state(ctx, show_global_popup);
                }
            }
        });
    }
}

mod preview {
    use crate::{
        test_data::get_test_accounts,
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
            app.accounts = get_test_accounts();
            GlobalPopupPreview { app }
        }
    }

    impl View for GlobalPopupPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            let mut panel = DesktopSidePanel::new(ui.ctx());
            DesktopSidePanel::panel().show(ui.ctx(), |ui| panel.ui(ui));
            DesktopGlobalPopup::new(&mut self.app).ui(ui);
        }
    }
}
