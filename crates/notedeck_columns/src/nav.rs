use crate::{
    accounts::render_accounts_route,
    actionbar::NoteAction,
    app::{get_active_columns_mut, get_decks_mut},
    column::ColumnsAction,
    deck_state::DeckState,
    decks::{Deck, DecksAction, DecksCache},
    profile::{ProfileAction, SaveProfileChanges},
    profile_state::ProfileState,
    relay_pool_manager::RelayPoolManager,
    route::Route,
    timeline::{route::render_timeline_route, TimelineCache},
    ui::{
        self,
        add_column::render_add_column_routes,
        column::NavTitle,
        configure_deck::ConfigureDeckView,
        edit_deck::{EditDeckResponse, EditDeckView},
        note::{PostAction, PostType},
        profile::EditProfileView,
        support::SupportView,
        RelayView, View,
    },
    Damus,
};

use egui_nav::{Nav, NavAction, NavResponse, NavUiType};
use nostrdb::Transaction;
use notedeck::{AccountsAction, AppContext};
use tracing::error;

#[allow(clippy::enum_variant_names)]
pub enum RenderNavAction {
    Back,
    RemoveColumn,
    PostAction(PostAction),
    NoteAction(NoteAction),
    ProfileAction(ProfileAction),
    SwitchingAction(SwitchingAction),
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
    pub fn process_render_nav_response(&self, app: &mut Damus, ctx: &mut AppContext<'_>) -> bool {
        let mut switching_occured: bool = false;
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
                    app.columns_mut(ctx.accounts)
                        .column_mut(col)
                        .router_mut()
                        .go_back();
                }

                RenderNavAction::RemoveColumn => {
                    let kinds_to_pop = app.columns_mut(ctx.accounts).delete_column(col);

                    for kind in &kinds_to_pop {
                        if let Err(err) = app.timeline_cache.pop(kind, ctx.ndb, ctx.pool) {
                            error!("error popping timeline: {err}");
                        }
                    }

                    switching_occured = true;
                }

                RenderNavAction::PostAction(post_action) => {
                    let txn = Transaction::new(ctx.ndb).expect("txn");
                    let _ = post_action.execute(ctx.ndb, &txn, ctx.pool, &mut app.drafts);
                    get_active_columns_mut(ctx.accounts, &mut app.decks_cache)
                        .column_mut(col)
                        .router_mut()
                        .go_back();
                }

                RenderNavAction::NoteAction(note_action) => {
                    let txn = Transaction::new(ctx.ndb).expect("txn");

                    note_action.execute_and_process_result(
                        ctx.ndb,
                        get_active_columns_mut(ctx.accounts, &mut app.decks_cache),
                        col,
                        &mut app.timeline_cache,
                        ctx.note_cache,
                        ctx.pool,
                        &txn,
                        ctx.unknown_ids,
                    );
                }

                RenderNavAction::SwitchingAction(switching_action) => {
                    switching_occured = switching_action.process(
                        &mut app.timeline_cache,
                        &mut app.decks_cache,
                        ctx,
                    );
                }
                RenderNavAction::ProfileAction(profile_action) => {
                    profile_action.process(
                        &mut app.view_state.pubkey_to_profile_state,
                        ctx.ndb,
                        ctx.pool,
                        get_active_columns_mut(ctx.accounts, &mut app.decks_cache)
                            .column_mut(col)
                            .router_mut(),
                    );
                }
            }
        }

        if let Some(action) = self.response.action {
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

                    switching_occured = true;
                }

                NavAction::Navigated => {
                    let cur_router = app.columns_mut(ctx.accounts).column_mut(col).router_mut();
                    cur_router.navigating = false;
                    if cur_router.is_replacing() {
                        cur_router.remove_previous_routes();
                    }
                    switching_occured = true;
                }

                NavAction::Dragging => {}
                NavAction::Returning => {}
                NavAction::Resetting => {}
                NavAction::Navigating => {}
            }
        }

        switching_occured
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
    match top {
        Route::Timeline(kind) => render_timeline_route(
            ctx.ndb,
            ctx.img_cache,
            ctx.unknown_ids,
            ctx.note_cache,
            &mut app.timeline_cache,
            &mut app.view_state.gifs,
            ctx.accounts,
            kind,
            col,
            app.textmode,
            depth,
            ui,
        ),
        Route::Accounts(amr) => {
            let mut action = render_accounts_route(
                ui,
                ctx.ndb,
                col,
                ctx.img_cache,
                &mut app.view_state.gifs,
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

                let response = egui::ScrollArea::vertical().show(ui, |ui| {
                    ui::PostReplyView::new(
                        ctx.ndb,
                        poster,
                        draft,
                        ctx.note_cache,
                        ctx.img_cache,
                        &mut app.view_state.gifs,
                        &note,
                        inner_rect,
                    )
                    .id_source(id)
                    .show(ui)
                });

                response.inner.action
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

            let response = egui::ScrollArea::vertical().show(ui, |ui| {
                crate::ui::note::QuoteRepostView::new(
                    ctx.ndb,
                    poster,
                    ctx.note_cache,
                    ctx.img_cache,
                    &mut app.view_state.gifs,
                    draft,
                    &note,
                    inner_rect,
                )
                .id_source(id)
                .show(ui)
            });

            response.inner.action.map(Into::into)
        }

        Route::ComposeNote => {
            let kp = ctx.accounts.get_selected_account()?.to_full()?;
            let draft = app.drafts.compose_mut();

            let txn = Transaction::new(ctx.ndb).expect("txn");
            let post_response = ui::PostView::new(
                ctx.ndb,
                draft,
                PostType::New,
                ctx.img_cache,
                ctx.note_cache,
                &mut app.view_state.gifs,
                kp,
                inner_rect,
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
        Route::NewDeck => {
            let id = ui.id().with("new-deck");
            let new_deck_state = app.view_state.id_to_deck_state.entry(id).or_default();
            let mut resp = None;
            if let Some(config_resp) = ConfigureDeckView::new(new_deck_state).ui(ui) {
                if let Some(cur_acc) = ctx.accounts.get_selected_account() {
                    app.decks_cache.add_deck(
                        cur_acc.pubkey,
                        Deck::new(config_resp.icon, config_resp.name),
                    );

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
            let id = ui.id().with((
                "edit-deck",
                ctx.accounts.get_selected_account().map(|k| k.pubkey),
                index,
            ));
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
                if EditProfileView::new(state, ctx.img_cache, &mut app.view_state.gifs).ui(ui) {
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
            &mut app.view_state.gifs,
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

    RenderNavResponse::new(col, nav_response)
}
