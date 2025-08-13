use nostrdb::Transaction;
use notedeck::AppContext;

use crate::{
    timeline::{kind::ListKind, TimelineKind},
    Damus, Route,
};

pub fn unseen_notification(
    columns: &mut Damus,
    ndb: &nostrdb::Ndb,
    current_pk: notedeck::enostr::Pubkey,
) -> bool {
    let Some(tl) = columns
        .timeline_cache
        .get_mut(&TimelineKind::Notifications(current_pk))
    else {
        return false;
    };

    let freshness = &mut tl.current_view_mut().freshness;
    freshness.update(|timestamp_last_viewed| {
        let filter = crate::timeline::kind::notifications_filter(&current_pk)
            .since_mut(timestamp_last_viewed);
        let txn = Transaction::new(ndb).expect("txn");

        let Some(res) = ndb.query(&txn, &[filter], 1).ok() else {
            return false;
        };

        !res.is_empty()
    });

    freshness.has_unseen()
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
