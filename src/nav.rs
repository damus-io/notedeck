use crate::{
    account_manager::render_accounts_route,
    relay_pool_manager::RelayPoolManager,
    route::Route,
    thread::thread_unsubscribe,
    timeline::route::{render_timeline_route, AfterRouteExecution, TimelineRoute},
    ui::{
        self,
        add_column::{AddColumnResponse, AddColumnView},
        note::PostAction,
        RelayView, View,
    },
    Damus,
};

use egui_nav::{Nav, NavAction};

pub fn render_nav(col: usize, app: &mut Damus, ui: &mut egui::Ui) {
    // TODO(jb55): clean up this router_mut mess by using Router<R> in egui-nav directly
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
            Route::ComposeNote => {
                let kp = app.accounts.selected_or_first_nsec()?;
                let draft = app.drafts.compose_mut();

                let txn = nostrdb::Transaction::new(&app.ndb).expect("txn");
                let post_response = ui::PostView::new(
                    &app.ndb,
                    draft,
                    crate::draft::DraftSource::Compose,
                    &mut app.img_cache,
                    &mut app.note_cache,
                    kp,
                )
                .ui(&txn, ui);

                if let Some(action) = post_response.action {
                    PostAction::execute(kp, &action, &mut app.pool, draft, |np, seckey| {
                        np.to_note(seckey)
                    });
                    app.columns_mut().column_mut(col).router_mut().go_back();
                }

                None
            }
            Route::AddColumn => {
                let resp = AddColumnView::new(&app.ndb, app.accounts.get_selected_account()).ui(ui);

                if let Some(resp) = resp {
                    match resp {
                        AddColumnResponse::Timeline(timeline) => {
                            let id = timeline.id;
                            if app.columns_mut().add_timeline_to_column(col, timeline) {
                                app.subscribe_new_timeline(id);
                            }
                        }
                    };
                }
                None
            }
        });

    if let Some(after_route_execution) = nav_response.inner {
        // start returning when we're finished posting
        match after_route_execution {
            AfterRouteExecution::Post(resp) => {
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
        let r = app.columns_mut().column_mut(col).router_mut().pop();
        if let Some(Route::Timeline(TimelineRoute::Thread(id))) = r {
            thread_unsubscribe(
                &app.ndb,
                &mut app.threads,
                &mut app.pool,
                &mut app.note_cache,
                id.bytes(),
            );
        }
    } else if let Some(NavAction::Navigated) = nav_response.action {
        let cur_router = app.columns_mut().column_mut(col).router_mut();
        cur_router.navigating = false;
        if cur_router.is_replacing() {
            cur_router.remove_previous_route();
        }
    }
}
