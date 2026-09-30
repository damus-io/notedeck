//! Routing what the chrome and its apps ask for: the sidebar's
//! [`ChromePanelAction`]s, and the [`AppAction`]s apps bubble up — including
//! sending a click on another app's inline widget to that app (Dave, Notebook,
//! Headway) instead of the Columns timeline.

use super::Chrome;
use egui::ThemePreference;
use nostrdb::Transaction;
use notedeck::{AppAction, AppContext, WalletType};
use notedeck_columns::timeline::TimelineKind;

pub enum ChromePanelAction {
    Support,
    Settings,
    Account,
    Wallet,
    SaveTheme(ThemePreference),
    Profile(nostrdb_net::Pubkey),
    #[cfg(feature = "auto-update")]
    ApplyUpdate,
    #[cfg(feature = "auto-update")]
    DismissUpdate,
}

impl ChromePanelAction {
    fn columns_navigate(ctx: &mut AppContext, chrome: &mut Chrome, route: notedeck_columns::Route) {
        chrome.switch_to_columns();

        if let Some(c) = chrome.get_columns_app().and_then(|columns| {
            columns
                .decks_cache
                .selected_column_mut(ctx.i18n, ctx.accounts)
        }) {
            if c.router().routes().iter().any(|r| r == &route) {
                // return if we are already routing to accounts
                c.router_mut().go_back();
            } else {
                c.router_mut().route_to(route);
                //c..route_to(Route::relays());
            }
        };
    }

    #[profiling::function]
    pub(super) fn process(&self, ctx: &mut AppContext, chrome: &mut Chrome, ui: &mut egui::Ui) {
        match self {
            Self::SaveTheme(theme) => {
                ui.ctx().set_theme(*theme);
                ctx.settings.set_theme(*theme);
            }

            Self::Support => {
                Self::columns_navigate(ctx, chrome, notedeck_columns::Route::Support);
            }

            Self::Account => {
                Self::columns_navigate(ctx, chrome, notedeck_columns::Route::accounts());
            }

            Self::Settings => {
                Self::columns_navigate(ctx, chrome, notedeck_columns::Route::Settings);
            }

            Self::Wallet => {
                Self::columns_navigate(
                    ctx,
                    chrome,
                    notedeck_columns::Route::Wallet(WalletType::Auto),
                );
            }
            Self::Profile(pk) => {
                columns_route_to_profile(pk, chrome, ctx, ui);
            }

            #[cfg(feature = "auto-update")]
            Self::ApplyUpdate => {
                if let Err(e) = chrome.updater.apply_and_restart() {
                    tracing::error!("failed to apply update: {e}");
                }
            }

            #[cfg(feature = "auto-update")]
            Self::DismissUpdate => {
                chrome.updater.dismiss();
            }
        }
    }
}

/// Whether `note_id` refers to a headway board/issue event, so a click on its
/// inline widget routes to the Headway app instead of the timeline.
#[cfg(feature = "headway")]
fn is_headway_note(ctx: &mut AppContext, note_id: nostrdb_net::NoteId) -> bool {
    let Ok(txn) = Transaction::new(ctx.ndb) else {
        return false;
    };
    ctx.ndb
        .get_note_by_id(&txn, note_id.bytes())
        .map(|note| notedeck_headway::is_headway_kind(note.kind()))
        .unwrap_or(false)
}

/// Open an inline headway board/issue click in the Headway app as ONE global
/// history entry, so a single back press returns to the source app.
///
/// Headway mints the route itself (switching its active board eagerly); if it
/// can't — e.g. the note hasn't resolved to a board yet — fall back to a plain
/// app switch so the user still lands in Headway, on its board root.
#[cfg(feature = "headway")]
fn open_headway_note(chrome: &mut Chrome, ctx: &mut AppContext, note_id: nostrdb_net::NoteId) {
    let Some(slot) = chrome.headway_slot() else {
        return;
    };
    if chrome.open_note_in_app(ctx, slot, note_id) {
        return;
    }
    chrome.set_active(slot as i32);
}

/// Whether `note_id` refers to an agentium session-state event, so a click on its
/// inline widget routes to the Dave app instead of the timeline.
#[cfg(feature = "dave")]
fn is_agentium_note(ctx: &mut AppContext, note_id: nostrdb_net::NoteId) -> bool {
    let Ok(txn) = Transaction::new(ctx.ndb) else {
        return false;
    };
    ctx.ndb
        .get_note_by_id(&txn, note_id.bytes())
        .map(|note| notedeck_dave::is_agentium_kind(note.kind()))
        .unwrap_or(false)
}

/// Whether `note_id` refers to a notebook node event, so a click on its inline
/// widget routes to the Notebook app instead of the timeline.
#[cfg(feature = "notebook")]
fn is_notebook_note(ctx: &mut AppContext, note_id: nostrdb_net::NoteId) -> bool {
    let Ok(txn) = Transaction::new(ctx.ndb) else {
        return false;
    };
    ctx.ndb
        .get_note_by_id(&txn, note_id.bytes())
        .map(|note| notedeck_notebook::is_notebook_kind(note.kind()))
        .unwrap_or(false)
}

/// Resolve `reference` — exactly one reference, e.g. `agentium:some-word-id` —
/// to the note it names, through the same registered
/// [`ReferenceParser`](notedeck::ReferenceParser)s the inline-chip renderer
/// uses, relative to the selected account. `None` when no parser recognizes it
/// or it doesn't resolve locally yet.
fn resolve_reference(ctx: &mut AppContext, reference: &str) -> Option<nostrdb_net::NoteId> {
    let txn = Transaction::new(ctx.ndb).ok()?;
    let resolve_ctx = notedeck::ReferenceResolveCtx {
        ndb: ctx.ndb,
        txn: &txn,
        selected_account: Some(*ctx.accounts.selected_account_pubkey()),
    };
    let resolved = ctx
        .registries
        .reference_parsers
        .resolve_exact(reference, &resolve_ctx)?;
    Some(resolved.note_id)
}

/// Open `note_id` in the app that owns its kind — an agentium session in Dave,
/// a notebook node in Notebook, a headway board/issue in Headway — the way a
/// click on its inline widget does. Returns `false`, doing nothing, when no
/// such app claims it, so the caller hands it to the Columns timeline.
#[cfg_attr(
    not(any(feature = "dave", feature = "notebook", feature = "headway")),
    allow(unused_variables)
)]
fn open_note_in_owning_app(
    chrome: &mut Chrome,
    ctx: &mut AppContext,
    note_id: nostrdb_net::NoteId,
) -> bool {
    #[cfg(feature = "dave")]
    if is_agentium_note(ctx, note_id) {
        chrome.switch_to_dave();
        if let Some(dave) = chrome.get_dave_app() {
            dave.open(note_id);
        }
        return true;
    }

    #[cfg(feature = "notebook")]
    if is_notebook_note(ctx, note_id) {
        chrome.switch_to_notebook();
        if let Some(notebook) = chrome.get_notebook_app() {
            notebook.open(note_id);
        }
        return true;
    }

    #[cfg(feature = "headway")]
    if is_headway_note(ctx, note_id) {
        open_headway_note(chrome, ctx, note_id);
        return true;
    }

    false
}

pub(super) fn chrome_handle_app_action(
    chrome: &mut Chrome,
    ctx: &mut AppContext,
    action: AppAction,
    ui: &mut egui::Ui,
) {
    match action {
        AppAction::ToggleChrome => {
            chrome.toggle();
        }

        AppAction::Open(open) => {
            let Some(note_id) = resolve_reference(ctx, &open.reference) else {
                tracing::warn!("open: no registered parser resolves {:?}", open.reference);
                return;
            };
            // Route it as the click on its inline chip would be, so an open by
            // reference and an open by click can never land differently.
            // `open.msg` is dropped here: no app consumes it yet.
            let click = notedeck::NoteAction::note(note_id);
            chrome_handle_app_action(chrome, ctx, AppAction::Note(click), ui);
        }

        AppAction::Note(note_action) => {
            // Intercept SummarizeThread — route to Dave instead of Columns
            #[cfg(feature = "dave")]
            if let notedeck::NoteAction::Context(ref context) = note_action {
                if let notedeck::NoteContextSelection::SummarizeThread(note_id) = context.action {
                    chrome.switch_to_dave();
                    if let Some(dave) = chrome.get_dave_app() {
                        dave.summarize_thread(note_id);
                    }
                    return;
                }
            }

            // A click on another app's inline widget opens in that app rather than
            // the timeline (see `open_note_in_owning_app`).
            if let notedeck::NoteAction::Note { note_id, .. } = &note_action {
                if open_note_in_owning_app(chrome, ctx, *note_id) {
                    return;
                }
            }

            chrome.switch_to_columns();
            let Some(columns) = chrome.get_columns_app() else {
                return;
            };

            let txn = Transaction::new(ctx.ndb).unwrap();

            let cols = columns
                .decks_cache
                .active_columns_mut(ctx.i18n, ctx.accounts)
                .unwrap();
            let m_action = notedeck_columns::actionbar::execute_and_process_note_action(
                note_action,
                ctx.ndb,
                cols,
                0,
                &mut columns.timeline_cache,
                &mut columns.threads,
                ctx.note_cache,
                &mut ctx.remote,
                &txn,
                ctx.unknown_ids,
                ctx.accounts,
                ctx.global_wallet,
                ctx.zaps,
                ctx.img_cache,
                &mut columns.view_state,
                ctx.media_jobs.sender(),
                ui,
                ctx.settings.columns_use_outbox_relays(),
            );

            if let Some(action) = m_action {
                let col = cols.selected_mut();

                action.process_router_action(&mut col.router, &mut col.sheet_router, ctx.sound);
            }
        }
    }
}

fn columns_route_to_profile(
    pk: &nostrdb_net::Pubkey,
    chrome: &mut Chrome,
    ctx: &mut AppContext,
    ui: &mut egui::Ui,
) {
    chrome.switch_to_columns();
    let Some(columns) = chrome.get_columns_app() else {
        return;
    };

    let cols = columns
        .decks_cache
        .active_columns_mut(ctx.i18n, ctx.accounts)
        .unwrap();

    let router = cols.get_selected_router();
    if router.routes().iter().any(|r| {
        matches!(
            r,
            notedeck_columns::Route::Timeline(TimelineKind::Profile(_))
        )
    }) {
        router.go_back();
        return;
    }

    let txn = Transaction::new(ctx.ndb).unwrap();
    let m_action = notedeck_columns::actionbar::execute_and_process_note_action(
        notedeck::NoteAction::Profile(*pk),
        ctx.ndb,
        cols,
        0,
        &mut columns.timeline_cache,
        &mut columns.threads,
        ctx.note_cache,
        &mut ctx.remote,
        &txn,
        ctx.unknown_ids,
        ctx.accounts,
        ctx.global_wallet,
        ctx.zaps,
        ctx.img_cache,
        &mut columns.view_state,
        ctx.media_jobs.sender(),
        ui,
        ctx.settings.columns_use_outbox_relays(),
    );

    if let Some(action) = m_action {
        let col = cols.selected_mut();

        action.process_router_action(&mut col.router, &mut col.sheet_router, ctx.sound);
    }
}
