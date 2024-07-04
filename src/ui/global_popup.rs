use std::{cell::RefCell, rc::Rc};

use egui_nav::{Nav, NavAction};

use crate::{route::Route, ui, Damus};

pub struct DesktopGlobalPopup {}

impl DesktopGlobalPopup {
    pub fn show(routes: Vec<Route>, app: &mut Damus, ui: &mut egui::Ui) {
        if routes.is_empty() || !app.show_global_popup {
            return;
        }

        let rect = routes
            .last()
            .map(|r| r.preferred_rect(ui))
            .unwrap_or_else(|| ui.ctx().screen_rect());
        let title = routes.last().map(|r| r.title());

        let app_ctx = Rc::new(RefCell::new(app));

        let resp = ui::FixedWindow::maybe_with_title(title).show(ui, rect, |ui| {
            let nav_response =
                Nav::new(routes)
                    .title(false)
                    .navigating(false)
                    .show(ui, |ui, nav| {
                        nav.top()
                            .show_global_popup(&mut app_ctx.borrow_mut(), ui)
                            .unwrap_or_else(|| ui.label(""))
                    });

            if let Some(NavAction::Returned) = nav_response.action {
                app_ctx.borrow_mut().global_nav.pop();
            }

            nav_response.inner
        });

        let mut app = app_ctx.borrow_mut();

        if resp == ui::FixedWindowResponse::Closed {
            app.global_nav.pop();
            app.show_global_popup = false;
        }
    }
}
