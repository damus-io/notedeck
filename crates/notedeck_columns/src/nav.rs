use crate::{
    accounts::{render_accounts_route, AccountsAction, AccountsResponse},
    app::{get_active_columns_mut, get_decks_mut},
    column::ColumnsAction,
    deck_state::DeckState,
    decks::{Deck, DecksAction, DecksCache},
    options::AppOptions,
    profile::{ProfileAction, SaveProfileChanges},
    repost::RepostAction,
    route::{Route, Router, SingletonRouter},
    subscriptions::Subscriptions,
    timeline::{
        kind::ListKind,
        route::{render_thread_route, render_timeline_route},
        TimelineCache, TimelineKind,
    },
    ui::{
        self,
        add_column::render_add_column_routes,
        column::NavTitle,
        configure_deck::ConfigureDeckView,
        edit_deck::{EditDeckResponse, EditDeckView},
        note::{custom_zap::CustomZapView, NewPostAction, PostAction, PostType},
        profile::EditProfileView,
        repost::RepostDecisionView,
        search::{FocusState, SearchView},
        settings::SettingsAction,
        support::SupportView,
        wallet::{get_default_zap_state, WalletAction, WalletState, WalletView},
        RelayView, SettingsView,
    },
    Damus,
};

use egui::scroll_area::ScrollAreaOutput;
use egui_nav::{
    Nav, NavAction, NavResponse, NavUiType, PopupResponse, PopupSheet, RouteResponse, Split,
};
use enostr::ProfileState;
use nostrdb::{Filter, Ndb, Transaction};
use notedeck::{
    get_current_default_msats, tr, ui::is_narrow, Accounts, AppContext, NoteAction, NoteContext,
    RelayAction,
};
use notedeck_ui::NoteOptions;
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
    RelayAction(RelayAction),
    SettingsAction(SettingsAction),
    RepostAction(RepostAction),
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
        subs: &mut Subscriptions,
        ui_ctx: &egui::Context,
    ) -> bool {
        match &self {
            SwitchingAction::Accounts(account_action) => match account_action {
                AccountsAction::Switch(switch_action) => {
                    {
                        let txn = Transaction::new(ctx.ndb).expect("txn");
                        ctx.accounts.select_account(
                            &switch_action.switch_to,
                            ctx.ndb,
                            &txn,
                            ctx.pool,
                            ui_ctx,
                        );

                        let contacts_sub = ctx.accounts.get_subs().contacts.remote.clone();
                        // this is cringe but we're gonna get a new sub manager soon...
                        subs.subs.insert(
                            contacts_sub,
                            crate::subscriptions::SubKind::FetchingContactList(TimelineKind::List(
                                ListKind::Contact(*ctx.accounts.selected_account_pubkey()),
                            )),
                        );
                    }

                    if switch_action.switching_to_new {
                        decks_cache.add_deck_default(ctx, timeline_cache, switch_action.switch_to);
                    }

                    // pop nav after switch
                    get_active_columns_mut(ctx.i18n, ctx.accounts, decks_cache)
                        .column_mut(switch_action.source_column)
                        .router_mut()
                        .go_back();
                }
                AccountsAction::Remove(to_remove) => 's: {
                    if !ctx
                        .accounts
                        .remove_account(to_remove, ctx.ndb, ctx.pool, ui_ctx)
                    {
                        break 's;
                    }

                    decks_cache.remove(ctx.i18n, to_remove, timeline_cache, ctx.ndb, ctx.pool);
                }
            },
            SwitchingAction::Columns(columns_action) => match *columns_action {
                ColumnsAction::Remove(index) => {
                    let kinds_to_pop = get_active_columns_mut(ctx.i18n, ctx.accounts, decks_cache)
                        .delete_column(index);
                    for kind in &kinds_to_pop {
                        if let Err(err) = timeline_cache.pop(kind, ctx.ndb, ctx.pool) {
                            error!("error popping timeline: {err}");
                        }
                    }
                }

                ColumnsAction::Switch(from, to) => {
                    get_active_columns_mut(ctx.i18n, ctx.accounts, decks_cache).move_col(from, to);
                }
            },
            SwitchingAction::Decks(decks_action) => match *decks_action {
                DecksAction::Switch(index) => {
                    get_decks_mut(ctx.i18n, ctx.accounts, decks_cache).set_active(index)
                }
                DecksAction::Removing(index) => {
                    get_decks_mut(ctx.i18n, ctx.accounts, decks_cache).remove_deck(
                        index,
                        timeline_cache,
                        ctx.ndb,
                        ctx.pool,
                    );
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

    pub fn can_take_drag_from(&self) -> Vec<egui::Id> {
        match &self.response {
            NotedeckNavResponse::Popup(_) => Vec::new(), // TODO(kernelkind): upgrade once popup supports drag ids
            NotedeckNavResponse::Nav(nav_response) => nav_response.can_take_drag_from.clone(),
        }
    }

    #[must_use = "Make sure to save columns if result is true"]
    #[profiling::function]
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

    if let Some(NavAction::Returned(_)) = action.action {
        let column = app.columns_mut(ctx.i18n, ctx.accounts).column_mut(col);
        if let Some(after_action) = column.sheet_router.after_action.clone() {
            column.router_mut().route_to(after_action);
        }
        column.sheet_router.clear();
    } else if let Some(NavAction::Navigating) = action.action {
        let column = app.columns_mut(ctx.i18n, ctx.accounts).column_mut(col);
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
            NavAction::Returned(return_type) => {
                let r = app
                    .columns_mut(ctx.i18n, ctx.accounts)
                    .column_mut(col)
                    .router_mut()
                    .pop();

                if let Some(Route::Timeline(kind)) = &r {
                    if let Err(err) = app.timeline_cache.pop(kind, ctx.ndb, ctx.pool) {
                        error!("popping timeline had an error: {err} for {:?}", kind);
                    }
                };

                if let Some(Route::Thread(selection)) = &r {
                    app.threads
                        .close(ctx.ndb, ctx.pool, selection, return_type, col);
                }

                // we should remove profile state once we've returned
                if let Some(Route::EditProfile(pk)) = &r {
                    app.view_state.pubkey_to_profile_state.remove(pk);
                }

                process_result = Some(ProcessNavResult::SwitchOccurred);
            }

            NavAction::Navigated => {
                let cur_router = app
                    .columns_mut(ctx.i18n, ctx.accounts)
                    .column_mut(col)
                    .router_mut();
                cur_router.navigating = false;
                if cur_router.is_replacing() {
                    cur_router.remove_previous_routes();
                }

                process_result = Some(ProcessNavResult::SwitchOccurred);
            }

            NavAction::Dragging => {}
            NavAction::Returning(_) => {}
            NavAction::Resetting => {}
            NavAction::Navigating => {
                // explicitly update the edit profile state when navigating
                handle_navigating_edit_profile(ctx.ndb, ctx.accounts, app, col);
            }
        }
    }

    process_result
}

/// We are navigating to edit profile, prepare the profile state
/// if we don't have it
fn handle_navigating_edit_profile(ndb: &Ndb, accounts: &Accounts, app: &mut Damus, col: usize) {
    let pk = {
        let Route::EditProfile(pk) = app.columns(accounts).column(col).router().top() else {
            return;
        };

        if app.view_state.pubkey_to_profile_state.contains_key(pk) {
            return;
        }

        pk.to_owned()
    };

    let txn = Transaction::new(ndb).expect("txn");
    app.view_state.pubkey_to_profile_state.insert(pk, {
        let filter = Filter::new_with_capacity(1)
            .kinds([0])
            .authors([pk.bytes()])
            .build();

        if let Ok(results) = ndb.query(&txn, &[filter], 1) {
            if let Some(result) = results.first() {
                tracing::debug!(
                    "refreshing profile state for edit view: {}",
                    result.note.content()
                );
                ProfileState::from_note_contents(result.note.content())
            } else {
                ProfileState::default()
            }
        } else {
            ProfileState::default()
        }
    });
}

pub enum RouterAction {
    GoBack,
    /// We clicked on a pfp in a route. We currently don't carry any
    /// information about the pfp since we only use it for toggling the
    /// chrome atm
    PfpClicked,
    RouteTo(Route, RouterType),
    CloseSheetThenRoute(Route),
    Overlay {
        route: Route,
        make_new: bool,
    },
}

pub enum RouterType {
    Sheet(Split),
    Stack,
}

fn go_back(stack: &mut Router<Route>, sheet: &mut SingletonRouter<Route>) {
    if sheet.route().is_some() {
        sheet.go_back();
    } else {
        stack.go_back();
    }
}

impl RouterAction {
    pub fn process(
        self,
        stack_router: &mut Router<Route>,
        sheet_router: &mut SingletonRouter<Route>,
    ) -> Option<ProcessNavResult> {
        match self {
            RouterAction::GoBack => {
                go_back(stack_router, sheet_router);

                None
            }

            RouterAction::PfpClicked => {
                if stack_router.routes().len() == 1 {
                    // if we're at the top level and we click a profile pic,
                    // bubble it up so that it can be handled by the chrome
                    // to open the sidebar
                    Some(ProcessNavResult::PfpClicked)
                } else {
                    // Otherwise just execute a back action
                    go_back(stack_router, sheet_router);

                    None
                }
            }

            RouterAction::RouteTo(route, router_type) => match router_type {
                RouterType::Sheet(percent) => {
                    sheet_router.route_to(route, percent);
                    None
                }
                RouterType::Stack => {
                    stack_router.route_to(route);
                    None
                }
            },
            RouterAction::Overlay { route, make_new } => {
                if make_new {
                    stack_router.route_to_overlaid_new(route);
                } else {
                    stack_router.route_to_overlaid(route);
                }
                None
            }
            RouterAction::CloseSheetThenRoute(route) => {
                sheet_router.go_back();
                sheet_router.after_action = Some(route);
                None
            }
        }
    }

    pub fn route_to(route: Route) -> Self {
        RouterAction::RouteTo(route, RouterType::Stack)
    }

    pub fn route_to_sheet(route: Route, split: Split) -> Self {
        RouterAction::RouteTo(route, RouterType::Sheet(split))
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
            let kinds_to_pop = app.columns_mut(ctx.i18n, ctx.accounts).delete_column(col);

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
                get_active_columns_mut(ctx.i18n, ctx.accounts, &mut app.decks_cache),
                col,
                &mut app.timeline_cache,
                &mut app.threads,
                ctx.note_cache,
                ctx.pool,
                &txn,
                ctx.unknown_ids,
                ctx.accounts,
                ctx.global_wallet,
                ctx.zaps,
                ctx.img_cache,
                &mut app.view_state,
                ui,
            )
        }
        RenderNavAction::SwitchingAction(switching_action) => {
            if switching_action.process(
                &mut app.timeline_cache,
                &mut app.decks_cache,
                ctx,
                &mut app.subscriptions,
                ui.ctx(),
            ) {
                return Some(ProcessNavResult::SwitchOccurred);
            } else {
                return None;
            }
        }
        RenderNavAction::ProfileAction(profile_action) => {
            profile_action.process_profile_action(ui.ctx(), ctx.ndb, ctx.pool, ctx.accounts)
        }
        RenderNavAction::WalletAction(wallet_action) => {
            wallet_action.process(ctx.accounts, ctx.global_wallet)
        }
        RenderNavAction::RelayAction(action) => {
            ctx.accounts
                .process_relay_action(ui.ctx(), ctx.pool, action);
            None
        }
        RenderNavAction::SettingsAction(action) => {
            action.process_settings_action(app, ctx.settings, ctx.i18n, ctx.img_cache, ui.ctx())
        }
        RenderNavAction::RepostAction(action) => {
            action.process(ctx.ndb, &ctx.accounts.get_selected_account().key, ctx.pool)
        }
    };

    if let Some(action) = router_action {
        let cols =
            get_active_columns_mut(ctx.i18n, ctx.accounts, &mut app.decks_cache).column_mut(col);
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
    ctx: &mut AppContext,
    top: &Route,
    depth: usize,
    col: usize,
    inner_rect: egui::Rect,
) -> BodyResponse<RenderNavAction> {
    let mut note_context = NoteContext {
        ndb: ctx.ndb,
        accounts: ctx.accounts,
        img_cache: ctx.img_cache,
        note_cache: ctx.note_cache,
        zaps: ctx.zaps,
        pool: ctx.pool,
        job_pool: ctx.job_pool,
        unknown_ids: ctx.unknown_ids,
        clipboard: ctx.clipboard,
        i18n: ctx.i18n,
        global_wallet: ctx.global_wallet,
    };
    match top {
        Route::Timeline(kind) => {
            // did something request scroll to top for the selection column?
            let scroll_to_top = app
                .decks_cache
                .selected_column_index(ctx.accounts)
                .is_some_and(|ind| ind == col)
                && app.options.contains(AppOptions::ScrollToTop);

            let resp = render_timeline_route(
                &mut app.timeline_cache,
                kind,
                col,
                app.note_options,
                depth,
                ui,
                &mut note_context,
                &mut app.jobs,
                scroll_to_top,
            );

            app.timeline_cache.set_fresh(kind);

            // always clear the scroll_to_top request
            if scroll_to_top {
                app.options.remove(AppOptions::ScrollToTop);
            }

            resp
        }
        Route::Thread(selection) => render_thread_route(
            &mut app.threads,
            selection,
            col,
            app.note_options,
            ui,
            &mut note_context,
            &mut app.jobs,
        ),
        Route::Accounts(amr) => {
            let resp = render_accounts_route(
                ui,
                ctx,
                &mut app.jobs,
                &mut app.view_state.login,
                &mut app.onboarding,
                &mut app.view_state.follow_packs,
                *amr,
            );

            resp.map_output_maybe(|action| match action {
                AccountsResponse::ViewProfile(pubkey) => {
                    Some(RenderNavAction::NoteAction(NoteAction::Profile(pubkey)))
                }
                AccountsResponse::Account(accounts_route_response) => {
                    let mut action = accounts_route_response.process(ctx, app, col);

                    let txn = Transaction::new(ctx.ndb).expect("txn");
                    action.process_action(ctx.unknown_ids, ctx.ndb, &txn);
                    action
                        .accounts_action
                        .map(|f| RenderNavAction::SwitchingAction(SwitchingAction::Accounts(f)))
                }
            })
        }
        Route::Relays => RelayView::new(ctx.pool, &mut app.view_state.id_string_map, ctx.i18n)
            .ui(ui)
            .map_output(RenderNavAction::RelayAction),

        Route::Settings => SettingsView::new(
            ctx.settings.get_settings_mut(),
            &mut note_context,
            &mut app.note_options,
            &mut app.jobs,
        )
        .ui(ui)
        .map_output(RenderNavAction::SettingsAction),

        Route::Reply(id) => {
            let txn = if let Ok(txn) = Transaction::new(ctx.ndb) {
                txn
            } else {
                ui.label(tr!(
                    note_context.i18n,
                    "Reply to unknown note",
                    "Error message when reply note cannot be found"
                ));
                return BodyResponse::none();
            };

            let note = if let Ok(note) = ctx.ndb.get_note_by_id(&txn, id.bytes()) {
                note
            } else {
                ui.label(tr!(
                    note_context.i18n,
                    "Reply to unknown note",
                    "Error message when reply note cannot be found"
                ));
                return BodyResponse::none();
            };

            let Some(poster) = ctx.accounts.selected_filled() else {
                return BodyResponse::none();
            };

            let resp = {
                let draft = app.drafts.reply_mut(note.id());

                let mut options = app.note_options;
                options.set(NoteOptions::Wide, false);

                ui::PostReplyView::new(
                    &mut note_context,
                    poster,
                    draft,
                    &note,
                    inner_rect,
                    options,
                    &mut app.jobs,
                    col,
                )
                .show(ui)
            };

            resp.map_output_maybe(|o| Some(o.action?.into()))
        }
        Route::Quote(id) => {
            let txn = Transaction::new(ctx.ndb).expect("txn");

            let note = if let Ok(note) = ctx.ndb.get_note_by_id(&txn, id.bytes()) {
                note
            } else {
                ui.label(tr!(
                    note_context.i18n,
                    "Quote of unknown note",
                    "Error message when quote note cannot be found"
                ));
                return BodyResponse::none();
            };

            let Some(poster) = ctx.accounts.selected_filled() else {
                return BodyResponse::none();
            };

            let draft = app.drafts.quote_mut(note.id());

            let response = crate::ui::note::QuoteRepostView::new(
                &mut note_context,
                poster,
                draft,
                &note,
                inner_rect,
                app.note_options,
                &mut app.jobs,
                col,
            )
            .show(ui);

            response.map_output_maybe(|o| Some(o.action?.into()))
        }
        Route::ComposeNote => {
            let Some(kp) = ctx.accounts.get_selected_account().key.to_full() else {
                return BodyResponse::none();
            };
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

            post_response.map_output_maybe(|o| Some(o.action?.into()))
        }
        Route::AddColumn(route) => {
            render_add_column_routes(ui, app, ctx, col, route);

            BodyResponse::none()
        }
        Route::Support => {
            SupportView::new(&mut app.support, ctx.i18n).show(ui);
            BodyResponse::none()
        }
        Route::Search => {
            let id = ui.id().with(("search", depth, col));
            let navigating =
                get_active_columns_mut(note_context.i18n, ctx.accounts, &mut app.decks_cache)
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
                app.note_options,
                search_buffer,
                &mut note_context,
                &mut app.jobs,
            )
            .show(ui)
            .map_output(RenderNavAction::NoteAction)
        }
        Route::NewDeck => {
            let id = ui.id().with("new-deck");
            let new_deck_state = app.view_state.id_to_deck_state.entry(id).or_default();
            let mut resp = None;
            if let Some(config_resp) = ConfigureDeckView::new(new_deck_state, ctx.i18n).ui(ui) {
                let cur_acc = ctx.accounts.selected_account_pubkey();
                app.decks_cache
                    .add_deck(*cur_acc, Deck::new(config_resp.icon, config_resp.name));

                // set new deck as active
                let cur_index = get_decks_mut(ctx.i18n, ctx.accounts, &mut app.decks_cache)
                    .decks()
                    .len()
                    - 1;
                resp = Some(RenderNavAction::SwitchingAction(SwitchingAction::Decks(
                    DecksAction::Switch(cur_index),
                )));

                new_deck_state.clear();
                get_active_columns_mut(ctx.i18n, ctx.accounts, &mut app.decks_cache)
                    .get_selected_router()
                    .go_back();
            }

            BodyResponse::output(resp)
        }
        Route::EditDeck(index) => {
            let mut action = None;
            let cur_deck = get_decks_mut(ctx.i18n, ctx.accounts, &mut app.decks_cache)
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
            if let Some(resp) = EditDeckView::new(deck_state, ctx.i18n).ui(ui) {
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
                get_active_columns_mut(ctx.i18n, ctx.accounts, &mut app.decks_cache)
                    .get_selected_router()
                    .go_back();
            }

            BodyResponse::output(action)
        }
        Route::EditProfile(pubkey) => {
            let Some(kp) = ctx.accounts.get_full(pubkey) else {
                error!("Pubkey in EditProfile route did not have an nsec attached in Accounts");
                return BodyResponse::none();
            };

            let Some(state) = app.view_state.pubkey_to_profile_state.get_mut(kp.pubkey) else {
                tracing::error!(
                    "No profile state when navigating to EditProfile... was handle_navigating_edit_profile not called?"
                );
                return BodyResponse::none();
            };

            EditProfileView::new(ctx.i18n, state, ctx.img_cache, ctx.clipboard)
                .ui(ui)
                .map_output_maybe(|save| {
                    if save {
                        app.view_state
                            .pubkey_to_profile_state
                            .get(kp.pubkey)
                            .map(|state| {
                                RenderNavAction::ProfileAction(ProfileAction::SaveChanges(
                                    SaveProfileChanges::new(kp.to_full(), state.clone()),
                                ))
                            })
                    } else {
                        None
                    }
                })
        }
        Route::Wallet(wallet_type) => {
            let state = match wallet_type {
                notedeck::WalletType::Auto => 's: {
                    if let Some(cur_acc_wallet) = ctx.accounts.get_selected_wallet_mut() {
                        let default_zap_state =
                            get_default_zap_state(&mut cur_acc_wallet.default_zap);
                        break 's WalletState::Wallet {
                            wallet: &mut cur_acc_wallet.wallet,
                            default_zap_state,
                            can_create_local_wallet: false,
                        };
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
                    let cur_acc = ctx.accounts.get_selected_wallet_mut();
                    let Some(wallet) = cur_acc else {
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

            BodyResponse::output(WalletView::new(state, ctx.i18n, ctx.clipboard).ui(ui))
                .map_output(RenderNavAction::WalletAction)
        }
        Route::CustomizeZapAmount(target) => {
            let txn = Transaction::new(ctx.ndb).expect("txn");
            let default_msats = get_current_default_msats(ctx.accounts, ctx.global_wallet);
            BodyResponse::output(
                CustomZapView::new(
                    ctx.i18n,
                    ctx.img_cache,
                    ctx.ndb,
                    &txn,
                    &target.zap_recipient,
                    default_msats,
                )
                .ui(ui),
            )
            .map_output(|msats| {
                get_active_columns_mut(ctx.i18n, ctx.accounts, &mut app.decks_cache)
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
        Route::RepostDecision(note_id) => {
            BodyResponse::output(RepostDecisionView::new(note_id).show(ui))
                .map_output(RenderNavAction::RepostAction)
        }
    }
}

pub struct BodyResponse<R> {
    pub drag_id: Option<egui::Id>, // the id which was used for dragging.
    pub output: Option<R>,
}

impl<R> BodyResponse<R> {
    pub fn none() -> Self {
        Self {
            drag_id: None,
            output: None,
        }
    }

    pub fn scroll(output: ScrollAreaOutput<Option<R>>) -> Self {
        Self {
            drag_id: Some(Self::scroll_output_to_drag_id(output.id)),
            output: output.inner,
        }
    }

    pub fn set_scroll_id(&mut self, output: &ScrollAreaOutput<Option<R>>) {
        self.drag_id = Some(Self::scroll_output_to_drag_id(output.id));
    }

    pub fn output(output: Option<R>) -> Self {
        Self {
            drag_id: None,
            output,
        }
    }

    pub fn set_output(&mut self, output: R) {
        self.output = Some(output);
    }

    /// The id of an `egui::ScrollAreaOutput`
    /// Should use `Self::scroll` when possible
    pub fn scroll_raw(mut self, id: egui::Id) -> Self {
        self.drag_id = Some(Self::scroll_output_to_drag_id(id));
        self
    }

    /// The id which is directly used for dragging
    pub fn set_drag_id_raw(&mut self, id: egui::Id) {
        self.drag_id = Some(id);
    }

    fn scroll_output_to_drag_id(id: egui::Id) -> egui::Id {
        id.with("area")
    }

    pub fn map_output<S>(self, f: impl FnOnce(R) -> S) -> BodyResponse<S> {
        BodyResponse {
            drag_id: self.drag_id,
            output: self.output.map(f),
        }
    }

    pub fn map_output_maybe<S>(self, f: impl FnOnce(R) -> Option<S>) -> BodyResponse<S> {
        BodyResponse {
            drag_id: self.drag_id,
            output: self.output.and_then(f),
        }
    }

    pub fn maybe_map_output<S>(self, f: impl FnOnce(Option<R>) -> S) -> BodyResponse<S> {
        BodyResponse {
            drag_id: self.drag_id,
            output: Some(f(self.output)),
        }
    }

    /// insert the contents of the new BodyResponse if they are empty in Self
    pub fn insert(&mut self, body: BodyResponse<R>) {
        self.drag_id = self.drag_id.or(body.drag_id);
        if self.output.is_none() {
            self.output = body.output;
        }
    }
}

#[must_use = "RenderNavResponse must be handled by calling .process_render_nav_response(..)"]
#[profiling::function]
pub fn render_nav(
    col: usize,
    inner_rect: egui::Rect,
    app: &mut Damus,
    ctx: &mut AppContext<'_>,
    ui: &mut egui::Ui,
) -> RenderNavResponse {
    let narrow = is_narrow(ui.ctx());

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
        let split = app.columns(ctx.accounts).column(col).sheet_router.split;
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
                .with_split(split)
                .show_mut(ui, |ui, typ, route| match typ {
                    NavUiType::Title => NavTitle::new(
                        ctx.ndb,
                        ctx.img_cache,
                        get_active_columns_mut(ctx.i18n, ctx.accounts, &mut app.decks_cache),
                        std::slice::from_ref(route),
                        col,
                        ctx.i18n,
                    )
                    .show_move_button(!narrow)
                    .show_delete_button(!narrow)
                    .show(ui),
                    NavUiType::Body => {
                        render_nav_body(ui, app, ctx, route, 1, col, inner_rect).output
                    }
                });

            return RenderNavResponse::new(col, NotedeckNavResponse::Popup(Box::new(resp)));
        }
    };

    let routes = app
        .columns(ctx.accounts)
        .column(col)
        .router()
        .routes()
        .clone();
    let nav = Nav::new(&routes).id_source(egui::Id::new(("nav", col)));

    let nav_response = nav
        .navigating(
            app.columns_mut(ctx.i18n, ctx.accounts)
                .column_mut(col)
                .router_mut()
                .navigating,
        )
        .returning(
            app.columns_mut(ctx.i18n, ctx.accounts)
                .column_mut(col)
                .router_mut()
                .returning,
        )
        .show_mut(ui, |ui, render_type, nav| match render_type {
            NavUiType::Title => {
                let show_menu_hint = nav.routes().len() == 1
                    && nav
                        .routes()
                        .last()
                        .is_some_and(|route| matches!(route, Route::Timeline(_)));
                let action = NavTitle::new(
                    ctx.ndb,
                    ctx.img_cache,
                    get_active_columns_mut(ctx.i18n, ctx.accounts, &mut app.decks_cache),
                    nav.routes(),
                    col,
                    ctx.i18n,
                )
                .show_move_button(!narrow)
                .show_delete_button(!narrow)
                .show_menu_hint(show_menu_hint)
                .show(ui);
                RouteResponse {
                    response: action,
                    can_take_drag_from: Vec::new(),
                }
            }

            NavUiType::Body => {
                let resp = if let Some(top) = nav.routes().last() {
                    render_nav_body(ui, app, ctx, top, nav.routes().len(), col, inner_rect)
                } else {
                    BodyResponse::none()
                };

                RouteResponse {
                    response: resp.output,
                    can_take_drag_from: resp.drag_id.map(|d| vec![d]).unwrap_or(Vec::new()),
                }
            }
        });

    RenderNavResponse::new(col, NotedeckNavResponse::Nav(Box::new(nav_response)))
}
