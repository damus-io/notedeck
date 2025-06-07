use crate::{
    accounts::render_accounts_route,
    app::{get_active_columns_mut, get_decks_mut},
    column::ColumnsAction,
    deck_state::DeckState,
    decks::{Deck, DecksAction, DecksCache},
    profile::{ProfileAction, SaveProfileChanges},
    profile_state::ProfileState,
    relay_pool_manager::RelayPoolManager,
    route::{Route, Router, SingletonRouter},
    timeline::{route::render_timeline_route, TimelineCache},
    ui::{
        self,
        add_column::render_add_column_routes,
        column::NavTitle,
        configure_deck::ConfigureDeckView,
        edit_deck::{EditDeckResponse, EditDeckView},
        note::{custom_zap::CustomZapView, NewPostAction, PostAction, PostType},
        profile::EditProfileView,
        search::{FocusState, SearchView},
        support::SupportView,
        wallet::{get_default_zap_state, WalletAction, WalletState, WalletView},
        RelayView,
    },
    Damus,
};

use egui_nav::{Nav, NavAction, NavResponse, NavUiType, Percent, PopupResponse, PopupSheet};
use nostrdb::Transaction;
use notedeck::{
    get_current_default_msats, get_current_wallet, AccountsAction, AppContext, NoteAction,
    NoteContext,
};
use notedeck_ui::View;
use tracing::error;

/// The result of processing a nav response
pub enum ProcessNavResult {
    SwitchOccurred,
    PfpClicked,
}

impl ProcessNavResult {
    pub fn switch_occurred(&self) -> bool {
        matches!(self, Self::SwitchOccurred)
    }
}

#[allow(clippy::enum_variant_names)]
pub enum RenderNavAction {
    Back,
    RemoveColumn,
    /// The response when the user interacts with a pfp in the nav header
    PfpClicked,
    PostAction(NewPostAction),
    NoteAction(NoteAction),
    ProfileAction(ProfileAction),
    SwitchingAction(SwitchingAction),
    WalletAction(WalletAction),
}

pub enum SwitchingAction {
    Accounts(AccountsAction),
    Columns(ColumnsAction),
    Decks(crate::decks::DecksAction),
}

impl SwitchingAction {
    /// process the action, and return whether switching occured
    pub fn process(
        &self,
        timeline_cache: &mut TimelineCache,
        decks_cache: &mut DecksCache,
        ctx: &mut AppContext<'_>,
    ) -> bool {
        match &self {
            SwitchingAction::Accounts(account_action) => match account_action {
                AccountsAction::Switch(switch_action) => {
                    ctx.accounts.select_account(switch_action.switch_to);
                    // pop nav after switch
                    if let Some(src) = switch_action.source {
                        get_active_columns_mut(ctx.accounts, decks_cache)
                            .column_mut(src)
                            .router_mut()
                            .go_back();
                    }
                }
                AccountsAction::Remove(index) => ctx.accounts.remove_account(*index),
            },
            SwitchingAction::Columns(columns_action) => match *columns_action {
                ColumnsAction::Remove(index) => {
                    let kinds_to_pop =
                        get_active_columns_mut(ctx.accounts, decks_cache).delete_column(index);
                    for kind in &kinds_to_pop {
                        if let Err(err) = timeline_cache.pop(kind, ctx.ndb, ctx.pool) {
                            error!("error popping timeline: {err}");
                        }
                    }
                }

                ColumnsAction::Switch(from, to) => {
                    get_active_columns_mut(ctx.accounts, decks_cache).move_col(from, to);
                }
            },
            SwitchingAction::Decks(decks_action) => match *decks_action {
                DecksAction::Switch(index) => {
                    get_decks_mut(ctx.accounts, decks_cache).set_active(index)
                }
                DecksAction::Removing(index) => {
                    get_decks_mut(ctx.accounts, decks_cache).remove_deck(index)
                }
            },
        }
        true
    }
}

impl From<PostAction> for RenderNavAction {
    fn from(post_action: PostAction) -> Self {
        match post_action {
            PostAction::QuotedNoteAction(note_action) => Self::NoteAction(note_action),
            PostAction::NewPostAction(new_post) => Self::PostAction(new_post),
        }
    }
}

impl From<NewPostAction> for RenderNavAction {
    fn from(post_action: NewPostAction) -> Self {
        Self::PostAction(post_action)
    }
}

impl From<NoteAction> for RenderNavAction {
    fn from(note_action: NoteAction) -> RenderNavAction {
        Self::NoteAction(note_action)
    }
}

enum NotedeckNavResponse {
    Popup(Box<PopupResponse<Option<RenderNavAction>>>),
    Nav(Box<NavResponse<Option<RenderNavAction>>>),
}

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
    pub fn process_render_nav_response(
        self,
        app: &mut Damus,
        ctx: &mut AppContext<'_>,
        ui: &mut egui::Ui,
    ) -> Option<ProcessNavResult> {
        match self.response {
            NotedeckNavResponse::Popup(nav_action) => {
                process_popup_resp(*nav_action, app, ctx, ui, self.column)
            }
            NotedeckNavResponse::Nav(nav_response) => {
                process_nav_resp(app, ctx, ui, *nav_response, self.column)
            }
        }
    }
}

fn process_popup_resp(
    action: PopupResponse<Option<RenderNavAction>>,
    app: &mut Damus,
    ctx: &mut AppContext<'_>,
    ui: &mut egui::Ui,
    col: usize,
) -> Option<ProcessNavResult> {
    let mut process_result: Option<ProcessNavResult> = None;
    if let Some(nav_action) = action.response {
        process_result = process_render_nav_action(app, ctx, ui, col, nav_action);
    }

    if let Some(NavAction::Returned) = action.action {
        let column = app.columns_mut(ctx.accounts).column_mut(col);
        column.sheet_router.clear();
    } else if let Some(NavAction::Navigating) = action.action {
        let column = app.columns_mut(ctx.accounts).column_mut(col);
        column.sheet_router.navigating = false;
    }

    process_result
}

fn process_nav_resp(
    app: &mut Damus,
    ctx: &mut AppContext<'_>,
    ui: &mut egui::Ui,
    response: NavResponse<Option<RenderNavAction>>,
    col: usize,
) -> Option<ProcessNavResult> {
    let mut process_result: Option<ProcessNavResult> = None;

    if let Some(action) = response.response.or(response.title_response) {
        // start returning when we're finished posting

        process_result = process_render_nav_action(app, ctx, ui, col, action);
    }

    if let Some(action) = response.action {
        match action {
            NavAction::Returned => {
                let r = app
                    .columns_mut(ctx.accounts)
                    .column_mut(col)
                    .router_mut()
                    .pop();

                if let Some(Route::Timeline(kind)) = &r {
                    if let Err(err) = app.timeline_cache.pop(kind, ctx.ndb, ctx.pool) {
                        error!("popping timeline had an error: {err} for {:?}", kind);
                    }
                };

                process_result = Some(ProcessNavResult::SwitchOccurred);
            }

            NavAction::Navigated => {
                let cur_router = app.columns_mut(ctx.accounts).column_mut(col).router_mut();
                cur_router.navigating = false;
                if cur_router.is_replacing() {
                    cur_router.remove_previous_routes();
                }

                process_result = Some(ProcessNavResult::SwitchOccurred);
            }

            NavAction::Dragging => {}
            NavAction::Returning => {}
            NavAction::Resetting => {}
            NavAction::Navigating => {}
        }
    }

    process_result
}

pub enum RouterAction {
    GoBack,
    /// We clicked on a pfp in a route. We currently don't carry any
    /// information about the pfp since we only use it for toggling the
    /// chrome atm
    PfpClicked,
    RouteTo(Route, RouterType),
}

pub enum RouterType {
    Sheet,
    Stack,
}

impl RouterAction {
    pub fn process(
        self,
        stack_router: &mut Router<Route>,
        sheet_router: &mut SingletonRouter<Route>,
    ) -> Option<ProcessNavResult> {
        match self {
            RouterAction::GoBack => {
                if sheet_router.route().is_some() {
                    sheet_router.go_back();
                } else {
                    stack_router.go_back();
                }

                None
            }

            RouterAction::PfpClicked => Some(ProcessNavResult::PfpClicked),

            RouterAction::RouteTo(route, router_type) => match router_type {
                RouterType::Sheet => {
                    sheet_router.route_to(route);
                    None
                }
                RouterType::Stack => {
                    stack_router.route_to(route);
                    None
                }
            },
        }
    }

    pub fn route_to(route: Route) -> Self {
        RouterAction::RouteTo(route, RouterType::Stack)
    }

    pub fn route_to_sheet(route: Route) -> Self {
        RouterAction::RouteTo(route, RouterType::Sheet)
    }
}

fn process_render_nav_action(
    app: &mut Damus,
    ctx: &mut AppContext<'_>,
    ui: &mut egui::Ui,
    col: usize,
    action: RenderNavAction,
) -> Option<ProcessNavResult> {
    let router_action = match action {
        RenderNavAction::Back => Some(RouterAction::GoBack),
        RenderNavAction::PfpClicked => Some(RouterAction::PfpClicked),

        RenderNavAction::RemoveColumn => {
            let kinds_to_pop = app.columns_mut(ctx.accounts).delete_column(col);

            for kind in &kinds_to_pop {
                if let Err(err) = app.timeline_cache.pop(kind, ctx.ndb, ctx.pool) {
                    error!("error popping timeline: {err}");
                }
            }

            return Some(ProcessNavResult::SwitchOccurred);
        }

        RenderNavAction::PostAction(new_post_action) => {
            let txn = Transaction::new(ctx.ndb).expect("txn");
            match new_post_action.execute(ctx.ndb, &txn, ctx.pool, &mut app.drafts) {
                Err(err) => tracing::error!("Error executing post action: {err}"),
                Ok(_) => tracing::debug!("Post action executed"),
            }

            Some(RouterAction::GoBack)
        }

        RenderNavAction::NoteAction(note_action) => {
            let txn = Transaction::new(ctx.ndb).expect("txn");

            crate::actionbar::execute_and_process_note_action(
                note_action,
                ctx.ndb,
                get_active_columns_mut(ctx.accounts, &mut app.decks_cache),
                col,
                &mut app.timeline_cache,
                ctx.note_cache,
                ctx.pool,
                &txn,
                ctx.unknown_ids,
                ctx.accounts,
                ctx.global_wallet,
                ctx.zaps,
                ctx.img_cache,
                ui,
            )
        }

        RenderNavAction::SwitchingAction(switching_action) => {
            if switching_action.process(&mut app.timeline_cache, &mut app.decks_cache, ctx) {
                return Some(ProcessNavResult::SwitchOccurred);
            } else {
                return None;
            }
        }
        RenderNavAction::ProfileAction(profile_action) => profile_action.process(
            &mut app.view_state.pubkey_to_profile_state,
            ctx.ndb,
            ctx.pool,
        ),
        RenderNavAction::WalletAction(wallet_action) => {
            wallet_action.process(ctx.accounts, ctx.global_wallet)
        }
    };

    if let Some(action) = router_action {
        let cols = get_active_columns_mut(ctx.accounts, &mut app.decks_cache).column_mut(col);
        let router = &mut cols.router;
        let sheet_router = &mut cols.sheet_router;

        action.process(router, sheet_router)
    } else {
        None
    }
}

fn render_nav_body(
    ui: &mut egui::Ui,
    app: &mut Damus,
    ctx: &mut AppContext<'_>,
    top: &Route,
    depth: usize,
    col: usize,
    inner_rect: egui::Rect,
) -> Option<RenderNavAction> {
    let mut note_context = NoteContext {
        ndb: ctx.ndb,
        img_cache: ctx.img_cache,
        note_cache: ctx.note_cache,
        zaps: ctx.zaps,
        pool: ctx.pool,
        job_pool: ctx.job_pool,
        missing_events_ids: &mut ctx.missing_events_ids,
        current_account_has_wallet: get_current_wallet(ctx.accounts, ctx.global_wallet).is_some(),
    };
    match top {
        Route::Timeline(kind) => render_timeline_route(
            ctx.unknown_ids,
            &mut app.timeline_cache,
            ctx.accounts,
            kind,
            col,
            app.note_options,
            depth,
            ui,
            &mut note_context,
            &mut app.jobs,
        ),
        Route::Accounts(amr) => {
            let mut action = render_accounts_route(
                ui,
                ctx.ndb,
                col,
                ctx.img_cache,
                ctx.accounts,
                &mut app.decks_cache,
                &mut app.view_state.login,
                *amr,
            );
            let txn = Transaction::new(ctx.ndb).expect("txn");
            action.process_action(ctx.unknown_ids, ctx.ndb, &txn);
            action
                .accounts_action
                .map(|f| RenderNavAction::SwitchingAction(SwitchingAction::Accounts(f)))
        }
        Route::Relays => {
            let manager = RelayPoolManager::new(ctx.pool);
            RelayView::new(ctx.accounts, manager, &mut app.view_state.id_string_map).ui(ui);
            None
        }
        Route::Reply(id) => {
            let txn = if let Ok(txn) = Transaction::new(ctx.ndb) {
                txn
            } else {
                ui.label("Reply to unknown note");
                return None;
            };

            let note = if let Ok(note) = ctx.ndb.get_note_by_id(&txn, id.bytes()) {
                note
            } else {
                ui.label("Reply to unknown note");
                return None;
            };

            let id = egui::Id::new(("post", col, note.key().unwrap()));
            let poster = ctx.accounts.selected_or_first_nsec()?;

            let action = {
                let draft = app.drafts.reply_mut(note.id());

                let response = egui::ScrollArea::vertical()
                    .show(ui, |ui| {
                        ui::PostReplyView::new(
                            &mut note_context,
                            poster,
                            draft,
                            &note,
                            inner_rect,
                            app.note_options,
                            &mut app.jobs,
                        )
                        .id_source(id)
                        .show(ui)
                    })
                    .inner;

                response.action
            };

            action.map(Into::into)
        }
        Route::Quote(id) => {
            let txn = Transaction::new(ctx.ndb).expect("txn");

            let note = if let Ok(note) = ctx.ndb.get_note_by_id(&txn, id.bytes()) {
                note
            } else {
                ui.label("Quote of unknown note");
                return None;
            };

            let id = egui::Id::new(("post", col, note.key().unwrap()));

            let poster = ctx.accounts.selected_or_first_nsec()?;
            let draft = app.drafts.quote_mut(note.id());

            let response = egui::ScrollArea::vertical()
                .show(ui, |ui| {
                    crate::ui::note::QuoteRepostView::new(
                        &mut note_context,
                        poster,
                        draft,
                        &note,
                        inner_rect,
                        app.note_options,
                        &mut app.jobs,
                    )
                    .id_source(id)
                    .show(ui)
                })
                .inner;

            response.action.map(Into::into)
        }
        Route::ComposeNote => {
            let kp = ctx.accounts.get_selected_account()?.key.to_full()?;
            let draft = app.drafts.compose_mut();

            let txn = Transaction::new(ctx.ndb).expect("txn");
            let post_response = ui::PostView::new(
                &mut note_context,
                draft,
                PostType::New,
                kp,
                inner_rect,
                app.note_options,
                &mut app.jobs,
            )
            .ui(&txn, ui);

            post_response.action.map(Into::into)
        }
        Route::AddColumn(route) => {
            render_add_column_routes(ui, app, ctx, col, route);

            None
        }
        Route::Support => {
            SupportView::new(&mut app.support).show(ui);
            None
        }
        Route::Search => {
            let id = ui.id().with(("search", depth, col));
            let navigating = get_active_columns_mut(ctx.accounts, &mut app.decks_cache)
                .column(col)
                .router()
                .navigating;
            let search_buffer = app.view_state.searches.entry(id).or_default();
            let txn = Transaction::new(ctx.ndb).expect("txn");

            if navigating {
                search_buffer.focus_state = FocusState::Navigating
            } else if search_buffer.focus_state == FocusState::Navigating {
                // we're not navigating but our last search buffer state
                // says we were navigating. This means that navigating has
                // stopped. Let's make sure to focus the input field
                search_buffer.focus_state = FocusState::ShouldRequestFocus;
            }

            SearchView::new(
                &txn,
                &ctx.accounts.mutefun(),
                app.note_options,
                search_buffer,
                &mut note_context,
                &ctx.accounts.get_selected_account().map(|a| (&a.key).into()),
                &mut app.jobs,
            )
            .show(ui, ctx.clipboard)
            .map(RenderNavAction::NoteAction)
        }
        Route::NewDeck => {
            let id = ui.id().with("new-deck");
            let new_deck_state = app.view_state.id_to_deck_state.entry(id).or_default();
            let mut resp = None;
            if let Some(config_resp) = ConfigureDeckView::new(new_deck_state).ui(ui) {
                if let Some(cur_acc) = ctx.accounts.selected_account_pubkey() {
                    app.decks_cache
                        .add_deck(*cur_acc, Deck::new(config_resp.icon, config_resp.name));

                    // set new deck as active
                    let cur_index = get_decks_mut(ctx.accounts, &mut app.decks_cache)
                        .decks()
                        .len()
                        - 1;
                    resp = Some(RenderNavAction::SwitchingAction(SwitchingAction::Decks(
                        DecksAction::Switch(cur_index),
                    )));
                }

                new_deck_state.clear();
                get_active_columns_mut(ctx.accounts, &mut app.decks_cache)
                    .get_first_router()
                    .go_back();
            }
            resp
        }
        Route::EditDeck(index) => {
            let mut action = None;
            let cur_deck = get_decks_mut(ctx.accounts, &mut app.decks_cache)
                .decks_mut()
                .get_mut(*index)
                .expect("index wasn't valid");
            let id = ui
                .id()
                .with(("edit-deck", ctx.accounts.selected_account_pubkey(), index));
            let deck_state = app
                .view_state
                .id_to_deck_state
                .entry(id)
                .or_insert_with(|| DeckState::from_deck(cur_deck));
            if let Some(resp) = EditDeckView::new(deck_state).ui(ui) {
                match resp {
                    EditDeckResponse::Edit(configure_deck_response) => {
                        cur_deck.edit(configure_deck_response);
                    }
                    EditDeckResponse::Delete => {
                        action = Some(RenderNavAction::SwitchingAction(SwitchingAction::Decks(
                            DecksAction::Removing(*index),
                        )));
                    }
                }
                get_active_columns_mut(ctx.accounts, &mut app.decks_cache)
                    .get_first_router()
                    .go_back();
            }

            action
        }
        Route::EditProfile(pubkey) => {
            let mut action = None;
            if let Some(kp) = ctx.accounts.get_full(pubkey.bytes()) {
                let state = app
                    .view_state
                    .pubkey_to_profile_state
                    .entry(*kp.pubkey)
                    .or_insert_with(|| {
                        let txn = Transaction::new(ctx.ndb).expect("txn");
                        if let Ok(record) = ctx.ndb.get_profile_by_pubkey(&txn, kp.pubkey.bytes()) {
                            ProfileState::from_profile(&record)
                        } else {
                            ProfileState::default()
                        }
                    });
                if EditProfileView::new(state, ctx.img_cache).ui(ui) {
                    if let Some(taken_state) =
                        app.view_state.pubkey_to_profile_state.remove(kp.pubkey)
                    {
                        action = Some(RenderNavAction::ProfileAction(ProfileAction::SaveChanges(
                            SaveProfileChanges::new(kp.to_full(), taken_state),
                        )))
                    }
                }
            } else {
                error!("Pubkey in EditProfile route did not have an nsec attached in Accounts");
            }
            action
        }
        Route::Wallet(wallet_type) => {
            let state = match wallet_type {
                notedeck::WalletType::Auto => 's: {
                    if let Some(cur_acc) = ctx.accounts.get_selected_account_mut() {
                        if let Some(wallet) = &mut cur_acc.wallet {
                            let default_zap_state = get_default_zap_state(&mut wallet.default_zap);
                            break 's WalletState::Wallet {
                                wallet: &mut wallet.wallet,
                                default_zap_state,
                                can_create_local_wallet: false,
                            };
                        }
                    }

                    let Some(wallet) = &mut ctx.global_wallet.wallet else {
                        break 's WalletState::NoWallet {
                            state: &mut ctx.global_wallet.ui_state,
                            show_local_only: true,
                        };
                    };

                    let default_zap_state = get_default_zap_state(&mut wallet.default_zap);
                    WalletState::Wallet {
                        wallet: &mut wallet.wallet,
                        default_zap_state,
                        can_create_local_wallet: true,
                    }
                }
                notedeck::WalletType::Local => 's: {
                    let Some(cur_acc) = ctx.accounts.get_selected_account_mut() else {
                        break 's WalletState::NoWallet {
                            state: &mut ctx.global_wallet.ui_state,
                            show_local_only: false,
                        };
                    };
                    let Some(wallet) = &mut cur_acc.wallet else {
                        break 's WalletState::NoWallet {
                            state: &mut ctx.global_wallet.ui_state,
                            show_local_only: false,
                        };
                    };

                    let default_zap_state = get_default_zap_state(&mut wallet.default_zap);
                    WalletState::Wallet {
                        wallet: &mut wallet.wallet,
                        default_zap_state,
                        can_create_local_wallet: false,
                    }
                }
            };

            WalletView::new(state)
                .ui(ui)
                .map(RenderNavAction::WalletAction)
        }
        Route::CustomizeZapAmount(target) => {
            let txn = Transaction::new(ctx.ndb).expect("txn");
            let default_msats = get_current_default_msats(ctx.accounts, ctx.global_wallet);
            CustomZapView::new(
                ctx.img_cache,
                ctx.ndb,
                &txn,
                &target.zap_recipient,
                default_msats,
            )
            .ui(ui)
            .map(|msats| {
                get_active_columns_mut(ctx.accounts, &mut app.decks_cache)
                    .column_mut(col)
                    .router_mut()
                    .go_back();
                RenderNavAction::NoteAction(NoteAction::Zap(notedeck::ZapAction::Send(
                    notedeck::note::ZapTargetAmount {
                        target: target.clone(),
                        specified_msats: Some(msats),
                    },
                )))
            })
        }
    }
}

#[must_use = "RenderNavResponse must be handled by calling .process_render_nav_response(..)"]
pub fn render_nav(
    col: usize,
    inner_rect: egui::Rect,
    app: &mut Damus,
    ctx: &mut AppContext<'_>,
    ui: &mut egui::Ui,
) -> RenderNavResponse {
    if let Some(sheet_route) = app
        .columns(ctx.accounts)
        .column(col)
        .sheet_router
        .route()
        .clone()
    {
        let navigating = app
            .columns(ctx.accounts)
            .column(col)
            .sheet_router
            .navigating;
        let returning = app.columns(ctx.accounts).column(col).sheet_router.returning;
        let bg_route = app
            .columns(ctx.accounts)
            .column(col)
            .router()
            .routes()
            .last()
            .cloned();
        if let Some(bg_route) = bg_route {
            let resp = PopupSheet::new(&bg_route, &sheet_route)
                .id_source(egui::Id::new(("nav", col)))
                .navigating(navigating)
                .returning(returning)
                .with_split_percent_from_top(Percent::new(35).expect("35 <= 100"))
                .show_mut(ui, |ui, typ, route| match typ {
                    NavUiType::Title => NavTitle::new(
                        ctx.ndb,
                        ctx.img_cache,
                        get_active_columns_mut(ctx.accounts, &mut app.decks_cache),
                        &[route.clone()],
                        col,
                    )
                    .show(ui),
                    NavUiType::Body => render_nav_body(ui, app, ctx, route, 1, col, inner_rect),
                });

            return RenderNavResponse::new(col, NotedeckNavResponse::Popup(Box::new(resp)));
        }
    };

    let nav_response = Nav::new(
        &app.columns(ctx.accounts)
            .column(col)
            .router()
            .routes()
            .clone(),
    )
    .navigating(
        app.columns_mut(ctx.accounts)
            .column_mut(col)
            .router_mut()
            .navigating,
    )
    .returning(
        app.columns_mut(ctx.accounts)
            .column_mut(col)
            .router_mut()
            .returning,
    )
    .id_source(egui::Id::new(("nav", col)))
    .show_mut(ui, |ui, render_type, nav| match render_type {
        NavUiType::Title => NavTitle::new(
            ctx.ndb,
            ctx.img_cache,
            get_active_columns_mut(ctx.accounts, &mut app.decks_cache),
            nav.routes(),
            col,
        )
        .show(ui),
        NavUiType::Body => {
            if let Some(top) = nav.routes().last() {
                render_nav_body(ui, app, ctx, top, nav.routes().len(), col, inner_rect)
            } else {
                None
            }
        }
    });

    RenderNavResponse::new(col, NotedeckNavResponse::Nav(Box::new(nav_response)))
}
