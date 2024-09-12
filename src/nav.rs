use crate::{
    account_manager::render_accounts_route,
    relay_pool_manager::RelayPoolManager,
    route::Route,
    thread::thread_unsubscribe,
    timeline::route::{render_timeline_route, TimelineRoute, TimelineRouteResponse},
    ui::{note::PostAction, RelayView, View},
    Damus,
};

use egui_nav::{Nav, NavAction};

pub fn render_nav(show_postbox: bool, col: usize, app: &mut Damus, ui: &mut egui::Ui) {
    let nav_response = Nav::new(app.columns().column(col).router().routes().clone())
        .navigating(app.columns_mut().column_mut(col).router_mut().navigating)
        .returning(app.columns_mut().column_mut(col).router_mut().returning)
        .title(false)
        .show_mut(ui, |ui, nav| match nav.top() {
            Route::Timeline(tlr) => render_timeline_route(
                &app.ndb,
                &mut app.columns,
                &mut app.pool,
                &mut app.drafts,
                &mut app.img_cache,
                &mut app.note_cache,
                &mut app.threads,
                &mut app.accounts,
                *tlr,
                col,
                show_postbox,
                app.textmode,
                ui,
            ),
            Route::Accounts(amr) => {
                render_accounts_route(
                    ui,
                    &app.ndb,
                    col,
                    &mut app.columns,
                    &mut app.img_cache,
                    &mut app.accounts,
                    &mut app.view_state.login,
                    *amr,
                );
                None
            }
            Route::Relays => {
                let manager = RelayPoolManager::new(app.pool_mut());
                RelayView::new(manager).ui(ui);
                None
            }
        });

    if let Some(reply_response) = nav_response.inner {
        // start returning when we're finished posting
        match reply_response {
            TimelineRouteResponse::Post(resp) => {
                if let Some(action) = resp.action {
                    match action {
                        PostAction::Post(_) => {
                            app.columns_mut().column_mut(col).router_mut().returning = true;
                        }
                    }
                }
            }
        }
    }

    if let Some(NavAction::Returned) = nav_response.action {
        let r = app.columns_mut().column_mut(col).router_mut().go_back();
        if let Some(Route::Timeline(TimelineRoute::Thread(id))) = r {
            thread_unsubscribe(
                &app.ndb,
                &mut app.threads,
                &mut app.pool,
                &mut app.note_cache,
                id.bytes(),
            );
        }
        app.columns_mut().column_mut(col).router_mut().returning = false;
    } else if let Some(NavAction::Navigated) = nav_response.action {
        app.columns_mut().column_mut(col).router_mut().navigating = false;
    }
}
