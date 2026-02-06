use egui_nav::ReturnType;
use notedeck::{AppContext, RelayPool};

use crate::{
    route::cleanup_popped_route,
    timeline::{kind::ListKind, TimelineKind},
    Damus, Route,
};

// TODO(kernelkind): should account for mutes
#[profiling::function]
pub fn unseen_notification(
    columns: &mut Damus,
    accounts: &notedeck::Accounts,
    active_col: usize,
) -> bool {
    let top = columns.columns(accounts).column(active_col).router().top();
    let current_pk = accounts.get_selected_account().keypair().pubkey;

    if let Route::Timeline(TimelineKind::Notifications(notif_pk)) = top {
        if notif_pk == current_pk {
            return false;
        }
    }

    let notif_kind = TimelineKind::Notifications(*current_pk);
    let Some(tl) = columns.timeline_cache.get_mut(&notif_kind) else {
        return false;
    };

    !tl.seen_latest_notes
}

/// When you click the toolbar button, these actions
/// are returned
#[derive(Debug, Eq, PartialEq)]
pub enum ToolbarAction {
    Notifications,
    Search,
    Home,
}

impl ToolbarAction {
    pub fn process(&self, app: &mut Damus, ctx: &mut AppContext) {
        let cur_acc_pk = ctx.accounts.get_selected_account().key.pubkey;
        let route = match &self {
            ToolbarAction::Notifications => {
                Route::timeline(TimelineKind::Notifications(cur_acc_pk))
            }
            ToolbarAction::Search => Route::Search,
            ToolbarAction::Home => {
                Route::timeline(TimelineKind::List(ListKind::Contact(cur_acc_pk)))
            }
        };

        let Some(cols) = app.decks_cache.active_columns_mut(ctx.i18n, ctx.accounts) else {
            return;
        };

        let selection_result = cols.select_by_route(route);

        match selection_result {
            crate::column::SelectionResult::AlreadySelected(col_index) => {
                // We're already on this toolbar view, so pop all routes to go to top
                pop_to_root(app, ctx, col_index);
                app.scroll_to_top();
            }
            crate::column::SelectionResult::NewSelection(_) => {
                // we already selected this, so scroll to top
                app.scroll_to_top();
            }
            crate::column::SelectionResult::Failed => {
                // oh no, something went wrong
                // TODO(jb55): handle tab selection failure
            }
        }
    }
}

/// Pop all routes in the column until we're back at depth 1 (the base route).
/// This is used when clicking a toolbar button for a view we're already on
/// to immediately return to the top level regardless of navigation depth.
fn pop_to_root(app: &mut Damus, ctx: &mut AppContext, col_index: usize) {
    let Some(cols) = app.decks_cache.active_columns_mut(ctx.i18n, ctx.accounts) else {
        return;
    };

    let column = cols.column_mut(col_index);

    // Close any open sheets first
    if column.sheet_router.route().is_some() {
        column.sheet_router.go_back();
    }

    // Pop all routes except the base route
    while column.router().routes().len() > 1 {
        if let Some(popped) = column.router_mut().pop() {
            // Clean up resources for the popped route
            cleanup_popped_route(
                &popped,
                &mut app.timeline_cache,
                &mut app.threads,
                &mut app.view_state,
                ctx.ndb,
                &mut RelayPool::new(&mut ctx.pool, ctx.accounts),
                ReturnType::Click,
                col_index,
            );
        }
    }
}
