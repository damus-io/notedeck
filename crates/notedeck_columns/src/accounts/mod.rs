use enostr::{FullKeypair, Pubkey};
use nostrdb::{Ndb, Transaction};

use notedeck::{Accounts, AppContext, JobsCache, Localization, SingleUnkIdAction, UnknownIds};
use notedeck_ui::nip51_set::Nip51SetUiCache;

pub use crate::accounts::route::AccountsResponse;
use crate::app::get_active_columns_mut;
use crate::decks::DecksCache;
use crate::onboarding::Onboarding;
use crate::profile::send_new_contact_list;
use crate::subscriptions::Subscriptions;
use crate::ui::onboarding::{FollowPackOnboardingView, FollowPacksResponse, OnboardingResponse};
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
    pub switching_to_new: bool,
}

impl SwitchAccountAction {
    pub fn new(source_column: usize, switch_to: Pubkey) -> Self {
        SwitchAccountAction {
            source_column,
            switch_to,
            switching_to_new: false,
        }
    }

    pub fn switching_to_new(mut self) -> Self {
        self.switching_to_new = true;
        self
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
    app_ctx: &mut AppContext,
    jobs: &mut JobsCache,
    login_state: &mut AcquireKeyState,
    onboarding: &mut Onboarding,
    follow_packs_ui: &mut Nip51SetUiCache,
    route: AccountsRoute,
) -> Option<AccountsResponse> {
    match route {
        AccountsRoute::Accounts => AccountsView::new(
            app_ctx.ndb,
            app_ctx.accounts,
            app_ctx.img_cache,
            app_ctx.i18n,
        )
        .ui(ui)
        .inner
        .map(AccountsRouteResponse::Accounts)
        .map(AccountsResponse::Account),
        AccountsRoute::AddAccount => {
            AccountLoginView::new(login_state, app_ctx.clipboard, app_ctx.i18n)
                .ui(ui)
                .inner
                .map(AccountsRouteResponse::AddAccount)
                .map(AccountsResponse::Account)
        }
        AccountsRoute::Onboarding => FollowPackOnboardingView::new(
            onboarding,
            follow_packs_ui,
            app_ctx.ndb,
            app_ctx.img_cache,
            app_ctx.i18n,
            app_ctx.job_pool,
            jobs,
        )
        .ui(ui)
        .map(|r| match r {
            OnboardingResponse::FollowPacks(follow_packs_response) => {
                AccountsResponse::Account(AccountsRouteResponse::AddAccount(
                    AccountLoginResponse::Onboarding(follow_packs_response),
                ))
            }
            OnboardingResponse::ViewProfile(pubkey) => AccountsResponse::ViewProfile(pubkey),
        }),
    }
}

pub fn process_accounts_view_response(
    i18n: &mut Localization,
    accounts: &mut Accounts,
    decks: &mut DecksCache,
    col: usize,
    response: AccountsViewResponse,
) -> Option<AccountsAction> {
    let router = get_active_columns_mut(i18n, accounts, decks)
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
    action
}

pub fn process_login_view_response(
    app_ctx: &mut AppContext,
    decks: &mut DecksCache,
    subs: &mut Subscriptions,
    onboarding: &mut Onboarding,
    col: usize,
    response: AccountLoginResponse,
) -> AddAccountAction {
    let cur_router = get_active_columns_mut(app_ctx.i18n, app_ctx.accounts, decks)
        .column_mut(col)
        .router_mut();

    let r = match response {
        AccountLoginResponse::LoginWith(keypair) => {
            cur_router.go_back();
            app_ctx.accounts.add_account(keypair)
        }
        AccountLoginResponse::CreatingNew => {
            cur_router.route_to(Route::Accounts(AccountsRoute::Onboarding));

            onboarding.process(app_ctx.pool, app_ctx.ndb, subs, app_ctx.unknown_ids);

            None
        }
        AccountLoginResponse::Onboarding(onboarding_response) => match onboarding_response {
            FollowPacksResponse::NoFollowPacks => {
                onboarding.process(app_ctx.pool, app_ctx.ndb, subs, app_ctx.unknown_ids);
                None
            }
            FollowPacksResponse::UserSelectedPacks(nip51_sets_ui_state) => {
                let pks_to_follow = nip51_sets_ui_state.get_all_selected();

                let kp = FullKeypair::generate();

                send_new_contact_list(kp.to_filled(), app_ctx.ndb, app_ctx.pool, pks_to_follow);
                cur_router.go_back();
                onboarding.end_onboarding(app_ctx.pool, app_ctx.ndb);

                app_ctx.accounts.add_account(kp.to_keypair())
            }
        },
    };

    if let Some(action) = r {
        AddAccountAction {
            accounts_action: Some(AccountsAction::Switch(SwitchAccountAction {
                source_column: col,
                switch_to: action.switch_to,
                switching_to_new: true,
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

impl AccountsRouteResponse {
    pub fn process(
        self,
        app_ctx: &mut AppContext,
        app: &mut crate::Damus,
        col: usize,
    ) -> AddAccountAction {
        match self {
            AccountsRouteResponse::Accounts(response) => {
                let action = process_accounts_view_response(
                    app_ctx.i18n,
                    app_ctx.accounts,
                    &mut app.decks_cache,
                    col,
                    response,
                );
                AddAccountAction {
                    accounts_action: action,
                    unk_id_action: notedeck::SingleUnkIdAction::no_action(),
                }
            }
            AccountsRouteResponse::AddAccount(response) => {
                let action = process_login_view_response(
                    app_ctx,
                    &mut app.decks_cache,
                    &mut app.subscriptions,
                    &mut app.onboarding,
                    col,
                    response,
                );
                app.view_state.login = Default::default();

                action
            }
        }
    }
}
