use enostr::{FullKeypair, Pubkey};
use nostrdb::{Ndb, Transaction};

use notedeck::{Accounts, Images, SingleUnkIdAction, UnknownIds};

use crate::app::get_active_columns_mut;
use crate::decks::DecksCache;
use crate::{
    login_manager::AcquireKeyState,
    route::Route,
    ui::{
        account_login_view::{AccountLoginResponse, AccountLoginView},
        accounts::{AccountsView, AccountsViewResponse},
    },
};
use egui_winit::clipboard::Clipboard;
use tracing::info;

mod route;

pub use route::{AccountsRoute, AccountsRouteResponse};

impl AddAccountAction {
    // Simple wrapper around processing the unknown action to expose too
    // much internal logic. This allows us to have a must_use on our
    // LoginAction type, otherwise the SingleUnkIdAction's must_use will
    // be lost when returned in the login action
    pub fn process_action(&mut self, ids: &mut UnknownIds, ndb: &Ndb, txn: &Transaction) {
        self.unk_id_action.process_action(ids, ndb, txn);
    }
}

#[derive(Debug, Clone)]
pub struct SwitchAccountAction {
    pub source_column: usize,

    /// The account to switch to
    pub switch_to: Pubkey,
}

impl SwitchAccountAction {
    pub fn new(source_column: usize, switch_to: Pubkey) -> Self {
        SwitchAccountAction {
            source_column,
            switch_to,
        }
    }
}

#[derive(Debug)]
pub enum AccountsAction {
    Switch(SwitchAccountAction),
    Remove(Pubkey),
}

#[must_use = "You must call process_login_action on this to handle unknown ids"]
pub struct AddAccountAction {
    pub accounts_action: Option<AccountsAction>,
    pub unk_id_action: SingleUnkIdAction,
}

/// Render account management views from a route
#[allow(clippy::too_many_arguments)]
pub fn render_accounts_route(
    ui: &mut egui::Ui,
    ndb: &Ndb,
    col: usize,
    img_cache: &mut Images,
    accounts: &mut Accounts,
    decks: &mut DecksCache,
    login_state: &mut AcquireKeyState,
    clipboard: &mut Clipboard,
    route: AccountsRoute,
) -> AddAccountAction {
    let resp = match route {
        AccountsRoute::Accounts => AccountsView::new(ndb, accounts, img_cache)
            .ui(ui)
            .inner
            .map(AccountsRouteResponse::Accounts),

        AccountsRoute::AddAccount => AccountLoginView::new(login_state, clipboard)
            .ui(ui)
            .inner
            .map(AccountsRouteResponse::AddAccount),
    };

    if let Some(resp) = resp {
        match resp {
            AccountsRouteResponse::Accounts(response) => {
                let action = process_accounts_view_response(accounts, decks, col, response);
                AddAccountAction {
                    accounts_action: action,
                    unk_id_action: SingleUnkIdAction::no_action(),
                }
            }
            AccountsRouteResponse::AddAccount(response) => {
                let action = process_login_view_response(accounts, decks, col, ndb, response);
                *login_state = Default::default();
                let router = get_active_columns_mut(accounts, decks)
                    .column_mut(col)
                    .router_mut();
                router.go_back();
                action
            }
        }
    } else {
        AddAccountAction {
            accounts_action: None,
            unk_id_action: SingleUnkIdAction::no_action(),
        }
    }
}

pub fn process_accounts_view_response(
    accounts: &mut Accounts,
    decks: &mut DecksCache,
    col: usize,
    response: AccountsViewResponse,
) -> Option<AccountsAction> {
    let router = get_active_columns_mut(accounts, decks)
        .column_mut(col)
        .router_mut();
    let mut action = None;
    match response {
        AccountsViewResponse::RemoveAccount(pk_to_remove) => {
            let cur_action = AccountsAction::Remove(pk_to_remove);
            info!("account selection: {:?}", action);
            action = Some(cur_action);
        }
        AccountsViewResponse::SelectAccount(new_pk) => {
            let acc_sel = AccountsAction::Switch(SwitchAccountAction::new(col, new_pk));
            info!("account selection: {:?}", acc_sel);
            action = Some(acc_sel);
        }
        AccountsViewResponse::RouteToLogin => {
            router.route_to(Route::add_account());
        }
    }
    accounts.needs_relay_config();
    action
}

pub fn process_login_view_response(
    manager: &mut Accounts,
    decks: &mut DecksCache,
    col: usize,
    ndb: &Ndb,
    response: AccountLoginResponse,
) -> AddAccountAction {
    let (r, pubkey) = match response {
        AccountLoginResponse::CreateNew => {
            let kp = FullKeypair::generate().to_keypair();
            let pubkey = kp.pubkey;
            let txn = Transaction::new(ndb).expect("txn");
            (manager.add_account(ndb, &txn, kp), pubkey)
        }
        AccountLoginResponse::LoginWith(keypair) => {
            let pubkey = keypair.pubkey;
            let txn = Transaction::new(ndb).expect("txn");
            (manager.add_account(ndb, &txn, keypair), pubkey)
        }
    };

    decks.add_deck_default(pubkey);

    if let Some(action) = r {
        AddAccountAction {
            accounts_action: Some(AccountsAction::Switch(SwitchAccountAction {
                source_column: col,
                switch_to: action.switch_to,
            })),
            unk_id_action: action.unk_id_action,
        }
    } else {
        AddAccountAction {
            accounts_action: None,
            unk_id_action: SingleUnkIdAction::NoAction,
        }
    }
}
