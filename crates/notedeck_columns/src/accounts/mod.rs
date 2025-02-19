use enostr::FullKeypair;
use nostrdb::Ndb;

use notedeck::{
    Accounts, AccountsAction, AddAccountAction, Images, SingleUnkIdAction, SwitchAccountAction,
    UrlMimes,
};

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
use tracing::info;

mod route;

pub use route::{AccountsRoute, AccountsRouteResponse};

/// Render account management views from a route
#[allow(clippy::too_many_arguments)]
pub fn render_accounts_route(
    ui: &mut egui::Ui,
    ndb: &Ndb,
    col: usize,
    img_cache: &mut Images,
    urls: &mut UrlMimes,
    accounts: &mut Accounts,
    decks: &mut DecksCache,
    login_state: &mut AcquireKeyState,
    route: AccountsRoute,
) -> AddAccountAction {
    let resp = match route {
        AccountsRoute::Accounts => AccountsView::new(ndb, accounts, img_cache, urls)
            .ui(ui)
            .inner
            .map(AccountsRouteResponse::Accounts),

        AccountsRoute::AddAccount => AccountLoginView::new(login_state)
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
                let action = process_login_view_response(accounts, decks, response);
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
    let mut selection = None;
    match response {
        AccountsViewResponse::RemoveAccount(index) => {
            let acc_sel = AccountsAction::Remove(index);
            info!("account selection: {:?}", acc_sel);
            selection = Some(acc_sel);
        }
        AccountsViewResponse::SelectAccount(index) => {
            let acc_sel = AccountsAction::Switch(SwitchAccountAction::new(Some(col), index));
            info!("account selection: {:?}", acc_sel);
            selection = Some(acc_sel);
        }
        AccountsViewResponse::RouteToLogin => {
            router.route_to(Route::add_account());
        }
    }
    accounts.needs_relay_config();
    selection
}

pub fn process_login_view_response(
    manager: &mut Accounts,
    decks: &mut DecksCache,
    response: AccountLoginResponse,
) -> AddAccountAction {
    let (r, pubkey) = match response {
        AccountLoginResponse::CreateNew => {
            let kp = FullKeypair::generate().to_keypair();
            let pubkey = kp.pubkey;
            (manager.add_account(kp), pubkey)
        }
        AccountLoginResponse::LoginWith(keypair) => {
            let pubkey = keypair.pubkey;
            (manager.add_account(keypair), pubkey)
        }
    };

    decks.add_deck_default(pubkey);

    r
}
