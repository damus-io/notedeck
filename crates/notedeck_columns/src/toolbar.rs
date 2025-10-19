use notedeck::AppContext;

use crate::{
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

        match cols.select_by_route(route) {
            crate::column::SelectionResult::AlreadySelected(_) => {} // great! no need to go to top yet
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
