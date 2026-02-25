pub mod cache;
pub mod convo_renderable;
pub mod loader;
pub mod nav;
pub mod nip17;
pub mod ui;

use enostr::Pubkey;
use hashbrown::{HashMap, HashSet};
use nav::{process_messages_ui_response, Route};
use nostrdb::{Subscription, Transaction};
use notedeck::{
    try_process_events_core, ui::is_narrow, Accounts, App, AppContext, AppResponse, Router,
};

use crate::{
    cache::{ConversationCache, ConversationListState, ConversationStates},
    loader::{LoaderMsg, MessagesLoader},
    nip17::conversation_filter,
    ui::{login_nsec_prompt, messages::messages_ui},
};

/// Max loader messages to process per frame to avoid UI stalls.
const MAX_LOADER_MSGS_PER_FRAME: usize = 8;

/// Messages application state and background loaders.
pub struct MessagesApp {
    messages: ConversationsCtx,
    states: ConversationStates,
    router: Router<Route>,
    loader: MessagesLoader,
    inflight_messages: HashSet<cache::ConversationId>,
}

impl MessagesApp {
    pub fn new() -> Self {
        Self {
            messages: ConversationsCtx::default(),
            states: ConversationStates::default(),
            router: Router::new(vec![Route::ConvoList]),
            loader: MessagesLoader::new(),
            inflight_messages: HashSet::new(),
        }
    }
}

impl Default for MessagesApp {
    fn default() -> Self {
        Self::new()
    }
}

impl App for MessagesApp {
    #[profiling::function]
    fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        try_process_events_core(ctx, ui.ctx(), |_, _| {});

        let Some(cache) = self.messages.get_current_mut(ctx.accounts) else {
            login_nsec_prompt(ui, ctx.i18n);
            return AppResponse::none();
        };

        self.loader.start(ui.ctx().clone(), ctx.ndb.clone());

        's: {
            let Some(secret) = &ctx.accounts.get_selected_account().key.secret_key else {
                break 's;
            };

            ctx.ndb.add_key(&secret.secret_bytes());

            let giftwrap_ndb = ctx.ndb.clone();
            let r = std::thread::Builder::new()
                .name("process_giftwraps".into())
                .spawn(move || {
                    let txn = Transaction::new(&giftwrap_ndb).expect("txn");
                    // although the actual giftwrap processing happens on the ingestion pool, this
                    // function still looks up giftwraps to process on the main thread, which can
                    // cause a freeze.
                    //
                    // TODO(jb55): move the giftwrap query logic into the internal threadpool so we
                    // don't have to spawn a thread here
                    giftwrap_ndb.process_giftwraps(&txn);
                });

            if let Err(err) = r {
                tracing::error!("failed to spawn process_giftwraps thread: {err}");
            }
        }

        match cache.state {
            ConversationListState::Initializing => {
                initialize(ctx, cache, is_narrow(ui.ctx()), &self.loader);
            }
            ConversationListState::Loading { subscription } => {
                if let Some(sub) = subscription {
                    update_initialized(ctx, cache, sub);
                }
            }
            ConversationListState::Initialized(subscription) => 's: {
                let Some(sub) = subscription else {
                    break 's;
                };
                update_initialized(ctx, cache, sub);
            }
        }

        handle_loader_messages(
            ctx,
            cache,
            &self.loader,
            &mut self.inflight_messages,
            is_narrow(ui.ctx()),
        );

        let selected_pubkey = ctx.accounts.selected_account_pubkey();

        let contacts_state = ctx
            .accounts
            .get_selected_account()
            .data
            .contacts
            .get_state();
        let resp = messages_ui(
            cache,
            &mut self.states,
            ctx.media_jobs.sender(),
            ctx.ndb,
            selected_pubkey,
            ui,
            ctx.img_cache,
            &self.router,
            ctx.settings.get_settings_mut(),
            contacts_state,
            ctx.i18n,
        );
        let action = process_messages_ui_response(
            resp,
            ctx,
            cache,
            &mut self.router,
            is_narrow(ui.ctx()),
            &self.loader,
            &mut self.inflight_messages,
        );

        AppResponse::action(action)
    }
}

/// Start the conversation list loader and subscription for the active account.
#[profiling::function]
fn initialize(
    ctx: &mut AppContext,
    cache: &mut ConversationCache,
    is_narrow: bool,
    loader: &MessagesLoader,
) {
    let sub = match ctx
        .ndb
        .subscribe(&conversation_filter(ctx.accounts.selected_account_pubkey()))
    {
        Ok(sub) => Some(sub),
        Err(e) => {
            tracing::error!("couldn't sub ndb: {e}");
            None
        }
    };

    loader.load_conversation_list(*ctx.accounts.selected_account_pubkey());
    cache.state = ConversationListState::Loading { subscription: sub };

    if !is_narrow {
        cache.active = None;
    }
}

/// Poll the live subscription for new conversation notes.
#[profiling::function]
fn update_initialized(ctx: &mut AppContext, cache: &mut ConversationCache, sub: Subscription) {
    let notes = ctx.ndb.poll_for_notes(sub, 10);
    let txn = Transaction::new(ctx.ndb).expect("txn");
    for key in notes {
        let note = match ctx.ndb.get_note_by_key(&txn, key) {
            Ok(n) => n,
            Err(e) => {
                tracing::error!("could not find note key: {e}");
                continue;
            }
        };
        cache.ingest_chatroom_msg(note, key, ctx.ndb, &txn, ctx.note_cache, ctx.unknown_ids);
    }
}

/// Drain loader messages and apply updates to the conversation cache.
#[profiling::function]
fn handle_loader_messages(
    ctx: &mut AppContext<'_>,
    cache: &mut ConversationCache,
    loader: &MessagesLoader,
    inflight_messages: &mut HashSet<cache::ConversationId>,
    is_narrow: bool,
) {
    let mut handled = 0;
    while handled < MAX_LOADER_MSGS_PER_FRAME {
        let Some(msg) = loader.try_recv() else {
            break;
        };
        handled += 1;

        match msg {
            LoaderMsg::ConversationBatch(keys) => {
                ingest_note_keys(ctx, cache, &keys);
            }
            LoaderMsg::ConversationFinished => {
                let current =
                    std::mem::replace(&mut cache.state, ConversationListState::Initializing);
                cache.state = match current {
                    ConversationListState::Loading { subscription } => {
                        ConversationListState::Initialized(subscription)
                    }
                    other => other,
                };

                if cache.active.is_none() && !is_narrow {
                    if let Some(first) = cache.first_convo_id() {
                        cache.active = Some(first);
                        request_conversation_messages(
                            cache,
                            ctx.accounts.selected_account_pubkey(),
                            first,
                            loader,
                            inflight_messages,
                        );
                    }
                }
            }
            LoaderMsg::ConversationMessagesBatch { keys, .. } => {
                ingest_note_keys(ctx, cache, &keys);
            }
            LoaderMsg::ConversationMessagesFinished { conversation_id } => {
                inflight_messages.remove(&conversation_id);
            }
            LoaderMsg::Failed(err) => {
                tracing::error!("messages loader error: {err}");
            }
        }
    }
}

/// Lookup note keys in NostrDB and ingest them into the conversation cache.
fn ingest_note_keys(
    ctx: &mut AppContext<'_>,
    cache: &mut ConversationCache,
    keys: &[nostrdb::NoteKey],
) {
    let txn = Transaction::new(ctx.ndb).expect("txn");
    for key in keys {
        let note = match ctx.ndb.get_note_by_key(&txn, *key) {
            Ok(n) => n,
            Err(e) => {
                tracing::error!("could not find note key: {e}");
                continue;
            }
        };
        cache.ingest_chatroom_msg(note, *key, ctx.ndb, &txn, ctx.note_cache, ctx.unknown_ids);
    }
}

/// Schedule a background load for a conversation's message history.
fn request_conversation_messages(
    cache: &ConversationCache,
    me: &Pubkey,
    conversation_id: cache::ConversationId,
    loader: &MessagesLoader,
    inflight_messages: &mut HashSet<cache::ConversationId>,
) {
    if inflight_messages.contains(&conversation_id) {
        return;
    }

    let Some(conversation) = cache.get(conversation_id) else {
        return;
    };

    inflight_messages.insert(conversation_id);
    loader.load_conversation_messages(
        conversation_id,
        conversation.metadata.participants.clone(),
        *me,
    );
}

/// Storage for conversations per account. Account management is performed by `Accounts`
#[derive(Default)]
struct ConversationsCtx {
    convos_per_acc: HashMap<Pubkey, ConversationCache>,
}

impl ConversationsCtx {
    /// Get the conversation cache for the selected account. Return None if we don't have a full kp
    pub fn get_current_mut(&mut self, accounts: &Accounts) -> Option<&mut ConversationCache> {
        accounts.get_selected_account().keypair().secret_key?;

        let current = accounts.selected_account_pubkey();
        Some(
            self.convos_per_acc
                .raw_entry_mut()
                .from_key(current)
                .or_insert_with(|| (*current, ConversationCache::new()))
                .1,
        )
    }
}
