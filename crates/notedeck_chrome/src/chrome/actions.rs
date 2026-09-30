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
///
/// `msg` is an [`OpenUri`](notedeck::OpenUri) message: Dave sends it into the
/// opened session ([`Dave::open_with_message`](notedeck_dave::Dave::open_with_message)).
/// The other apps take no message, so it's logged and dropped there.
#[cfg_attr(
    not(any(feature = "dave", feature = "notebook", feature = "headway")),
    allow(unused_variables)
)]
fn open_note_in_owning_app(
    chrome: &mut Chrome,
    ctx: &mut AppContext,
    note_id: nostrdb_net::NoteId,
    msg: Option<String>,
) -> bool {
    #[cfg(feature = "dave")]
    if is_agentium_note(ctx, note_id) {
        chrome.switch_to_dave();
        if let Some(dave) = chrome.get_dave_app() {
            dave.open_with_message(note_id, msg);
        }
        return true;
    }

    if msg.is_some() {
        tracing::warn!(
            "open: {} isn't an agentium session; dropping its message",
            note_id.hex()
        );
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

/// How long an unresolved [`AppAction::Open`] is retried before it's given up,
/// in egui time. Long enough to cover a reference cache seeding on the frame
/// after the one that subscribed it (the common case, one frame) and a note
/// that's still a moment away from ingesting; short enough that a press that
/// did nothing is reported while the user still remembers pressing it.
const OPEN_RETRY_WINDOW_SECS: f64 = 2.0;

/// Resolves tried before an unresolved open may be given up, however much egui
/// time has passed. Guards the window against one slow frame (a big fold, a
/// debugger pause) eating it whole before the retry that would have hit.
const OPEN_MIN_ATTEMPTS: u32 = 3;

/// The repaint an unresolved open schedules between retries, so it retries
/// without waiting for unrelated input yet doesn't spin the render loop flat
/// out for the whole window.
const OPEN_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// An [`AppAction::Open`] whose reference didn't resolve when it was raised,
/// held on [`Chrome`] and retried by [`retry_pending_open`] each frame.
///
/// A miss on the first try is usually a matter of timing, not a bad reference.
/// Dave's `agentium:` parser resolves through a
/// [`RealtimeCache`](notedeck::RealtimeCache), which subscribes on its first
/// read for an author and only seeds on the next (see its `advance`), so the
/// first open of a session nothing has drawn a chip for yet always misses.
/// Dropping that open would silently lose the press — and its message, which
/// is text the user wrote.
pub(super) struct PendingOpen {
    open: notedeck::OpenUri,
    /// Resolves tried so far, the one at raise time included.
    attempts: u32,
    /// The egui time after which, with [`OPEN_MIN_ATTEMPTS`] tried, it's
    /// given up.
    give_up_at: f64,
}

impl PendingOpen {
    /// Hold `open`, whose first resolve just missed.
    fn new(open: notedeck::OpenUri, egui_ctx: &egui::Context) -> Self {
        Self {
            open,
            attempts: 1,
            give_up_at: egui_ctx.input(|i| i.time) + OPEN_RETRY_WINDOW_SECS,
        }
    }
}

/// Retry the held [`PendingOpen`], if any: route it once its reference
/// resolves, give it up (with a warning) once its window has passed, and
/// otherwise keep holding it and schedule the next retry. Does nothing, and
/// allocates nothing, when no open is held.
#[profiling::function]
pub(super) fn retry_pending_open(chrome: &mut Chrome, ctx: &mut AppContext, ui: &mut egui::Ui) {
    let Some(mut pending) = chrome.pending_open.take() else {
        return;
    };
    pending.attempts += 1;

    if let Some(note_id) = resolve_reference(ctx, &pending.open.reference) {
        route_open(chrome, ctx, note_id, pending.open.msg, ui);
        return;
    }

    let now = ui.input(|i| i.time);
    if pending.attempts >= OPEN_MIN_ATTEMPTS && now >= pending.give_up_at {
        tracing::warn!(
            "open: no registered parser resolves {:?} (gave up after {} tries)",
            pending.open.reference,
            pending.attempts
        );
        return;
    }

    chrome.pending_open = Some(pending);
    ui.ctx().request_repaint_after(OPEN_RETRY_INTERVAL);
}

/// Route an open whose reference resolved to `note_id` as the click on its
/// inline chip would be, so an open by reference and an open by click can
/// never land differently — except that the owning app also gets the message
/// (Dave sends it).
fn route_open(
    chrome: &mut Chrome,
    ctx: &mut AppContext,
    note_id: nostrdb_net::NoteId,
    msg: Option<String>,
    ui: &mut egui::Ui,
) {
    if open_note_in_owning_app(chrome, ctx, note_id, msg) {
        return;
    }
    let click = notedeck::NoteAction::note(note_id);
    chrome_handle_app_action(chrome, ctx, AppAction::Note(click), ui);
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
                // Often just not resolvable *yet* (see `PendingOpen`): hold it
                // and retry on the next frames rather than dropping it.
                if let Some(dropped) = chrome.pending_open.take() {
                    tracing::debug!(
                        "open: {:?} replaces the still-unresolved {:?}",
                        open.reference,
                        dropped.open.reference
                    );
                }
                chrome.pending_open = Some(PendingOpen::new(open, ui.ctx()));
                ui.ctx().request_repaint_after(OPEN_RETRY_INTERVAL);
                return;
            };
            route_open(chrome, ctx, note_id, open.msg, ui);
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
                if open_note_in_owning_app(chrome, ctx, *note_id, None) {
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

// Cross-app routing of an open by reference, through a chrome built the way
// the app builds one (`new_headless`), so the apps that claim a reference are
// the real ones. Needs the apps compiled in, which `cargo test --workspace`
// doesn't do; CI runs it with `--features dave,notebook,headway`.
#[cfg(all(test, feature = "dave", feature = "headway"))]
mod open_tests {
    use super::*;
    use egui_nav::{NavAction, ReturnType};
    use nostrdb::{Filter, IngestMetadata, NoteBuilder};
    use nostrdb_net::FullKeypair;
    use notedeck::{AppId, NavRequest, Notedeck, OpenUri};
    use notedeck_dave::session_events::AI_SESSION_STATE_KIND;

    /// The session the open names: its kind-31988 d-tag.
    const SESSION_ID: &str = "chrome-open-test-session";

    /// What the open asks the session, as Headway's `S` does.
    const MSG: &str = "launch a /code-review for the work done in this session";

    /// Write `SESSION_ID`'s kind-31988 state note, signed by `kp`, and wait
    /// until it's committed so its `agentium:` reference resolves.
    async fn ingest_session(ndb: &nostrdb::Ndb, kp: &FullKeypair) -> nostrdb_net::NoteId {
        let filter = Filter::new().kinds([AI_SESSION_STATE_KIND as u64]).build();
        let sub = ndb.subscribe(&[filter]).expect("subscribe");
        let note = NoteBuilder::new()
            .kind(AI_SESSION_STATE_KIND)
            .content("")
            .created_at(1_000)
            .start_tag()
            .tag_str("d")
            .tag_str(SESSION_ID)
            .start_tag()
            .tag_str("title")
            .tag_str("Open test")
            .start_tag()
            .tag_str("status")
            .tag_str("working")
            .sign(&kp.secret_key.secret_bytes())
            .build()
            .expect("note");
        let frame = nostrdb_net::ClientMessage::event(&note)
            .expect("event")
            .to_json()
            .expect("json");
        ndb.process_event_with(&frame, IngestMetadata::new().client(true))
            .expect("ingest");
        ndb.wait_for_notes(sub, 1).await.expect("committed");
        nostrdb_net::NoteId::new(*note.id())
    }

    /// Run one egui frame at `time` (egui seconds) with a `ui` under a
    /// central panel, as the chrome's app route has one.
    fn frame_at(egui_ctx: &egui::Context, time: f64, mut f: impl FnMut(&mut egui::Ui)) {
        let input = egui::RawInput {
            time: Some(time),
            ..Default::default()
        };
        let _ = egui_ctx.run(input, |c| {
            egui::CentralPanel::default().show(c, |ui| f(ui));
        });
    }

    /// A chrome built the way the app builds one, with a fresh account.
    struct OpenFixture {
        _dir: tempfile::TempDir,
        kp: FullKeypair,
        egui_ctx: egui::Context,
        notedeck: Notedeck,
        chrome: Chrome,
    }

    impl OpenFixture {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().expect("tmp dir");
            let kp = FullKeypair::generate();
            let args: Vec<String> = [
                "notedeck-test",
                "--testrunner",
                "--nsec",
                &kp.secret_key.to_secret_hex(),
            ]
            .iter()
            .map(|s| s.to_string())
            .collect();

            let egui_ctx = egui::Context::default();
            let mut notedeck = Notedeck::init(&egui_ctx, dir.path(), &args);
            let chrome = Chrome::new_headless(&args, &mut notedeck).expect("chrome");
            Self {
                _dir: dir,
                kp,
                egui_ctx,
                notedeck,
                chrome,
            }
        }
    }

    /// An `AppAction::Open` of an `agentium:` reference with a message, raised
    /// while Headway is in front (as its review queue's `S` raises it) for a
    /// session nothing has resolved yet — no chip for it drawn, so Dave's
    /// session cache has only just subscribed and the open's own resolve
    /// misses (see `RealtimeCache::advance`). The chrome holds it rather than
    /// dropping it, and the next frame's retry lands it in Dave as ONE
    /// global-history entry, Dave holds the session and the message for its
    /// next update, and one back returns to Headway. The Headway side (exactly
    /// one open, no history entry of its own) is
    /// `shift_s_in_the_queue_opens_the_session_asking_for_a_review`.
    #[tokio::test]
    async fn a_cold_open_is_held_until_it_resolves_then_lands_in_dave() {
        let OpenFixture {
            _dir,
            kp,
            egui_ctx,
            mut notedeck,
            mut chrome,
        } = OpenFixture::new();
        let mut ctx = notedeck.app_context();
        assert_eq!(*ctx.accounts.selected_account_pubkey(), kp.pubkey);
        let state_note = ingest_session(ctx.ndb, &kp).await;

        let headway = chrome.headway_slot().expect("headway in the roster");
        chrome.set_active(headway as i32);
        let before = chrome.global_nav.as_ref().expect("nav").len();

        let open = OpenUri {
            reference: agentium_core::wordid::session_ref(SESSION_ID),
            msg: Some(MSG.to_string()),
        };
        frame_at(&egui_ctx, 0.0, |ui| {
            chrome_handle_app_action(&mut chrome, &mut ctx, AppAction::Open(open.clone()), ui);
        });

        // The cold resolve missed: held, nothing routed yet.
        assert!(chrome.pending_open.is_some(), "the unresolved open is held");
        assert_eq!(chrome.global_nav.as_ref().expect("nav").len(), before);
        assert_eq!(chrome.active, headway as i32);

        // The next frame's retry finds the seeded session and routes it.
        frame_at(&egui_ctx, 1.0 / 60.0, |ui| {
            retry_pending_open(&mut chrome, &mut ctx, ui);
        });
        assert!(chrome.pending_open.is_none(), "the resolved open is let go");

        let dave = chrome
            .apps
            .iter()
            .position(|app| matches!(app, crate::app::NotedeckApp::Dave(_)))
            .expect("dave in the roster");
        let nav = chrome.global_nav.as_ref().expect("nav");
        assert_eq!(nav.len(), before + 1, "the open is one history entry");
        assert_eq!(nav.top().app, AppId(dave));
        assert_eq!(chrome.active, dave as i32);

        let pending = chrome
            .get_dave_app()
            .and_then(|dave| dave.pending_open())
            .expect("Dave holds the open");
        assert_eq!(pending.note, state_note);
        assert_eq!(pending.msg.as_deref(), Some(MSG));

        // Later retries have nothing to do: still exactly one entry.
        frame_at(&egui_ctx, 2.0 / 60.0, |ui| {
            retry_pending_open(&mut chrome, &mut ctx, ui);
        });
        assert_eq!(chrome.global_nav.as_ref().expect("nav").len(), before + 1);

        // One back returns to Headway. The pop lands once the slide does,
        // which `nav_frame` reconciles; drive the same reconcile here.
        chrome.apply_nav_requests(vec![NavRequest::Back]);
        chrome
            .global_nav
            .as_mut()
            .expect("nav")
            .reconcile(NavAction::Returned(ReturnType::Click));
        chrome.sync_active_from_nav();
        let nav = chrome.global_nav.as_ref().expect("nav");
        assert_eq!(nav.top().app, AppId(headway));
        assert_eq!(chrome.active, headway as i32);
    }

    /// An open whose reference never resolves is retried through its window
    /// and then given up: nothing is pushed, the active app doesn't change.
    /// Both halves of the bound hold — neither the minimum tries alone nor
    /// the elapsed time alone lets it go.
    #[tokio::test]
    async fn an_open_that_never_resolves_is_given_up_and_pushes_nothing() {
        let OpenFixture {
            _dir,
            egui_ctx,
            mut notedeck,
            mut chrome,
            ..
        } = OpenFixture::new();
        let mut ctx = notedeck.app_context();

        let headway = chrome.headway_slot().expect("headway in the roster");
        chrome.set_active(headway as i32);
        let before = chrome.global_nav.as_ref().expect("nav").len();

        let open = OpenUri {
            reference: agentium_core::wordid::session_ref("no-such-session-anywhere"),
            msg: Some(MSG.to_string()),
        };
        let raise_at = |chrome: &mut Chrome, ctx: &mut AppContext, t: f64| {
            frame_at(&egui_ctx, t, |ui| {
                chrome_handle_app_action(chrome, ctx, AppAction::Open(open.clone()), ui);
            });
            assert!(chrome.pending_open.is_some(), "the unresolved open is held");
        };
        let retry_at = |chrome: &mut Chrome, ctx: &mut AppContext, t: f64| {
            frame_at(&egui_ctx, t, |ui| retry_pending_open(chrome, ctx, ui));
        };

        // Enough tries, but inside the window: still held.
        raise_at(&mut chrome, &mut ctx, 0.0);
        for i in 1..OPEN_MIN_ATTEMPTS {
            retry_at(&mut chrome, &mut ctx, f64::from(i) / 60.0);
        }
        assert!(
            chrome.pending_open.is_some(),
            "tries alone don't give it up"
        );
        // Past the window too: given up.
        retry_at(&mut chrome, &mut ctx, OPEN_RETRY_WINDOW_SECS + 0.1);
        assert!(chrome.pending_open.is_none(), "given up");

        // Past the window on the very next frame (one slow frame), but short
        // of the minimum tries: still held, until the tries are spent too.
        let t = 100.0;
        raise_at(&mut chrome, &mut ctx, t);
        let late = t + OPEN_RETRY_WINDOW_SECS + 1.0;
        for _ in 2..OPEN_MIN_ATTEMPTS {
            retry_at(&mut chrome, &mut ctx, late);
            assert!(
                chrome.pending_open.is_some(),
                "one slow frame doesn't give it up"
            );
        }
        retry_at(&mut chrome, &mut ctx, late);
        assert!(chrome.pending_open.is_none(), "given up");
        assert_eq!(chrome.global_nav.as_ref().expect("nav").len(), before);
        assert_eq!(chrome.active, headway as i32);
    }
}
