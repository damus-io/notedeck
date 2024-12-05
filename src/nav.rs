use crate::{
    accounts::render_accounts_route,
    actionbar::NoteAction,
    notes_holder::NotesHolder,
    profile::Profile,
    relay_pool_manager::RelayPoolManager,
    route::Route,
    thread::Thread,
    timeline::{
        route::{render_timeline_route, TimelineRoute},
        Timeline,
    },
    ui::{
        self,
        add_column::render_add_column_routes,
        column::NavTitle,
        note::{PostAction, PostType},
        support::SupportView,
        RelayView, View,
    },
    Damus,
};

use egui_nav::{Nav, NavAction, NavResponse, NavUiType};
use nostrdb::{Ndb, Transaction};
use tracing::{error, info};

pub enum RenderNavAction {
    Back,
    RemoveColumn,
    PostAction(PostAction),
    NoteAction(NoteAction),
}

impl From<PostAction> for RenderNavAction {
    fn from(post_action: PostAction) -> Self {
        Self::PostAction(post_action)
    }
}

impl From<NoteAction> for RenderNavAction {
    fn from(note_action: NoteAction) -> RenderNavAction {
        Self::NoteAction(note_action)
    }
}

pub type NotedeckNavResponse = NavResponse<Option<RenderNavAction>>;

pub struct RenderNavResponse {
    column: usize,
    response: NotedeckNavResponse,
}

impl RenderNavResponse {
    #[allow(private_interfaces)]
    pub fn new(column: usize, response: NotedeckNavResponse) -> Self {
        RenderNavResponse { column, response }
    }

    #[must_use = "Make sure to save columns if result is true"]
    pub fn process_render_nav_response(&self, app: &mut Damus) -> bool {
        let mut col_changed: bool = false;
        let col = self.column;

        if let Some(action) = self
            .response
            .response
            .as_ref()
            .or(self.response.title_response.as_ref())
        {
            // start returning when we're finished posting
            match action {
                RenderNavAction::Back => {
                    app.columns_mut().column_mut(col).router_mut().go_back();
                }

                RenderNavAction::RemoveColumn => {
                    let tl = app.columns().find_timeline_for_column_index(col);
                    if let Some(timeline) = tl {
                        unsubscribe_timeline(app.ndb(), timeline);
                    }

                    app.columns_mut().delete_column(col);
                    col_changed = true;
                }

                RenderNavAction::PostAction(post_action) => {
                    let txn = Transaction::new(&app.ndb).expect("txn");
                    let _ = post_action.execute(&app.ndb, &txn, &mut app.pool, &mut app.drafts);
                    app.columns_mut().column_mut(col).router_mut().go_back();
                }

                RenderNavAction::NoteAction(note_action) => {
                    let txn = Transaction::new(&app.ndb).expect("txn");

                    note_action.execute_and_process_result(
                        &app.ndb,
                        &mut app.columns,
                        col,
                        &mut app.threads,
                        &mut app.profiles,
                        &mut app.note_cache,
                        &mut app.pool,
                        &txn,
                        &app.accounts.mutefun(),
                    );
                }
            }
        }

        if let Some(action) = self.response.action {
            match action {
                NavAction::Returned => {
                    let r = app.columns_mut().column_mut(col).router_mut().pop();
                    let txn = Transaction::new(&app.ndb).expect("txn");
                    if let Some(Route::Timeline(TimelineRoute::Thread(id))) = r {
                        let root_id = {
                            crate::note::root_note_id_from_selected_id(
                                &app.ndb,
                                &mut app.note_cache,
                                &txn,
                                id.bytes(),
                            )
                        };
                        Thread::unsubscribe_locally(
                            &txn,
                            &app.ndb,
                            &mut app.note_cache,
                            &mut app.threads,
                            &mut app.pool,
                            root_id,
                            &app.accounts.mutefun(),
                        );
                    }

                    if let Some(Route::Timeline(TimelineRoute::Profile(pubkey))) = r {
                        Profile::unsubscribe_locally(
                            &txn,
                            &app.ndb,
                            &mut app.note_cache,
                            &mut app.profiles,
                            &mut app.pool,
                            pubkey.bytes(),
                            &app.accounts.mutefun(),
                        );
                    }
                    col_changed = true;
                }

                NavAction::Navigated => {
                    let cur_router = app.columns_mut().column_mut(col).router_mut();
                    cur_router.navigating = false;
                    if cur_router.is_replacing() {
                        cur_router.remove_previous_routes();
                    }
                    col_changed = true;
                }

                NavAction::Dragging => {}
                NavAction::Returning => {}
                NavAction::Resetting => {}
                NavAction::Navigating => {}
            }
        }

        col_changed
    }
}

fn render_nav_body(
    ui: &mut egui::Ui,
    app: &mut Damus,
    top: &Route,
    col: usize,
) -> Option<RenderNavAction> {
    match top {
        Route::Timeline(tlr) => render_timeline_route(
            &app.ndb,
            &mut app.columns,
            &mut app.drafts,
            &mut app.img_cache,
            &mut app.unknown_ids,
            &mut app.note_cache,
            &mut app.threads,
            &mut app.profiles,
            &mut app.accounts,
            *tlr,
            col,
            app.textmode,
            ui,
        ),
        Route::Accounts(amr) => {
            let action = render_accounts_route(
                ui,
                &app.ndb,
                col,
                &mut app.columns,
                &mut app.img_cache,
                &mut app.accounts,
                &mut app.view_state.login,
                *amr,
            );
            let txn = Transaction::new(&app.ndb).expect("txn");
            action.process_action(&mut app.unknown_ids, &app.ndb, &txn);
            None
        }
        Route::Relays => {
            let manager = RelayPoolManager::new(app.pool_mut());
            RelayView::new(manager).ui(ui);
            None
        }
        Route::ComposeNote => {
            let kp = app.accounts.get_selected_account()?.to_full()?;
            let draft = app.drafts.compose_mut();

            let txn = Transaction::new(&app.ndb).expect("txn");
            let post_response = ui::PostView::new(
                &app.ndb,
                draft,
                PostType::New,
                &mut app.img_cache,
                &mut app.note_cache,
                kp,
            )
            .ui(&txn, ui);

            post_response.action.map(Into::into)
        }
        Route::AddColumn(route) => {
            render_add_column_routes(ui, app, col, route);

            None
        }

        Route::Support => {
            SupportView::new(&mut app.support).show(ui);
            None
        }
    }
}

#[must_use = "RenderNavResponse must be handled by calling .process_render_nav_response(..)"]
pub fn render_nav(col: usize, app: &mut Damus, ui: &mut egui::Ui) -> RenderNavResponse {
    let col_id = app.columns.get_column_id_at_index(col);
    // TODO(jb55): clean up this router_mut mess by using Router<R> in egui-nav directly

    let nav_response = Nav::new(app.columns().column(col).router().routes().clone())
        .navigating(app.columns_mut().column_mut(col).router_mut().navigating)
        .returning(app.columns_mut().column_mut(col).router_mut().returning)
        .id_source(egui::Id::new(col_id))
        .show_mut(ui, |ui, render_type, nav| match render_type {
            NavUiType::Title => NavTitle::new(
                &app.ndb,
                &mut app.img_cache,
                &app.columns,
                app.accounts.get_selected_account().map(|a| &a.pubkey),
                nav.routes_arr(),
            )
            .show(ui),
            NavUiType::Body => render_nav_body(ui, app, nav.routes().last().expect("top"), col),
        });

    RenderNavResponse::new(col, nav_response)
}

fn unsubscribe_timeline(ndb: &Ndb, timeline: &Timeline) {
    if let Some(sub_id) = timeline.subscription {
        if let Err(e) = ndb.unsubscribe(sub_id) {
            error!("unsubscribe error: {}", e);
        } else {
            info!(
                "successfully unsubscribed from timeline {} with sub id {}",
                timeline.id,
                sub_id.id()
            );
        }
    }
}
