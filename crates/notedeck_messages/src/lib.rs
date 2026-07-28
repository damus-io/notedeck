pub mod cache;
pub mod convo_renderable;
pub mod loader;
pub mod nav;
pub mod nip17;
mod relay_ensure;
mod relay_prefetch;
pub mod ui;

use enostr::Pubkey;
use hashbrown::{HashMap, HashSet};
use nav::{process_messages_ui_response, Route};
use nostrdb::{Ndb, Subscription, Transaction};
use notedeck::{
    ui::is_narrow, Accounts, App, AppContext, AppResponse, RemoteApi, Router, SubKey, SubOwnerKey,
    TabNotifications,
};

use crate::{
    cache::{ConversationCache, ConversationListState, ConversationStates},
    loader::{LoaderMsg, MessagesLoader},
    nip17::{conversation_filter, known_participant_dm_relay_list_authors},
    relay_ensure::ensure_selected_account_dm_list,
    ui::{login_nsec_prompt, messages::messages_ui},
};
use std::thread;

/// Max loader messages to process per frame to avoid UI stalls.
const MAX_LOADER_MSGS_PER_FRAME: usize = 8;

/// Messages application state and background loaders.
pub struct MessagesApp {
    messages: ConversationsCtx,
    states: ConversationStatesByAccount,
    router: Router<Route>,
    loader: MessagesLoader,
    inflight_messages: HashSet<ConversationLoadKey>,
    giftwrap_workers: Vec<thread::JoinHandle<()>>,
}

impl MessagesApp {
    pub fn new() -> Self {
        Self {
            messages: ConversationsCtx::default(),
            states: ConversationStatesByAccount::default(),
            router: Router::new(vec![Route::ConvoList]),
            loader: MessagesLoader::new(),
            inflight_messages: HashSet::new(),
            giftwrap_workers: Vec::new(),
        }
    }
}

impl Default for MessagesApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for MessagesApp {
    fn drop(&mut self) {
        join_giftwrap_workers(&mut self.giftwrap_workers);
    }
}

impl App for MessagesApp {
    #[profiling::function]
    fn update(&mut self, ctx: &mut AppContext<'_>, egui_ctx: &egui::Context) {
        let is_narrow = is_narrow(egui_ctx);
        let Some(cache) = self.messages.get_current_mut(ctx.accounts) else {
            return;
        };

        self.loader.start(egui_ctx.clone(), ctx.ndb.clone());

        ensure_selected_account_dm_relay_list(ctx.ndb, &mut ctx.remote, ctx.accounts, cache);

        match cache.state {
            ConversationListState::Initializing => {
                reap_finished_giftwrap_workers(&mut self.giftwrap_workers);
                initialize(
                    ctx,
                    cache,
                    is_narrow,
                    &self.loader,
                    &mut self.giftwrap_workers,
                );
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
            &mut self.messages,
            &self.loader,
            &mut self.inflight_messages,
        );

        if let Some(cache) = self.messages.get_current_mut(ctx.accounts) {
            ensure_selected_startup(
                ctx,
                cache,
                &self.loader,
                &mut self.inflight_messages,
                is_narrow,
            );
        }
    }

    #[profiling::function]
    fn render(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        let Some(cache) = self.messages.get_current_mut(ctx.accounts) else {
            login_nsec_prompt(ui, ctx.i18n);
            return AppResponse::none();
        };

        let selected_pubkey = ctx.accounts.selected_account_pubkey();
        let states = self.states.for_account_mut(selected_pubkey);

        let contacts_state = ctx
            .accounts
            .get_selected_account()
            .data
            .contacts
            .get_state();
        let resp = messages_ui(
            cache,
            states,
            ctx.media_jobs.sender(),
            ctx.ndb,
            selected_pubkey,
            ui,
            ctx.img_cache,
            &self.router,
            ctx.settings.get_settings_mut(),
            contacts_state,
            ctx.i18n,
            ctx.clipboard,
        );
        let processed = process_messages_ui_response(
            resp,
            ctx,
            cache,
            &mut self.router,
            is_narrow(ui.ctx()),
            &self.loader,
            &mut self.inflight_messages,
        );
        if let Some(send) = processed.send_message {
            let result =
                nip17::send_conversation_message(send.conversation_id, send.content, cache, ctx);
            if let nip17::SendMessageResult::NotSent { content } = result {
                restore_unsent_message(states, send.conversation_id, content);
            }
        }

        AppResponse::action(processed.app_action)
    }

    fn tab_notifications(&self, ctx: &AppContext<'_>) -> TabNotifications {
        let Some(cache) = self.messages.get_current(ctx.accounts) else {
            return TabNotifications::default();
        };
        let states = self
            .states
            .for_account(ctx.accounts.selected_account_pubkey());
        let unread = cache
            .iter()
            .filter(|(id, convo)| {
                let last_read = states
                    .and_then(|s| s.cache.get(id))
                    .and_then(|s| s.last_read);
                crate::ui::ConversationSummary::new(convo, last_read).unread
            })
            .count();
        TabNotifications::count(unread as u32)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ConversationLoadKey {
    account_pubkey: Pubkey,
    conversation_id: cache::ConversationId,
}

impl ConversationLoadKey {
    pub(crate) fn new(account_pubkey: Pubkey, conversation_id: cache::ConversationId) -> Self {
        Self {
            account_pubkey,
            conversation_id,
        }
    }
}

#[derive(Default)]
struct ConversationStatesByAccount {
    states: HashMap<Pubkey, ConversationStates>,
}

impl ConversationStatesByAccount {
    fn for_account_mut(&mut self, account: &Pubkey) -> &mut ConversationStates {
        self.states.entry(*account).or_default()
    }

    fn for_account(&self, account: &Pubkey) -> Option<&ConversationStates> {
        self.states.get(account)
    }
}

fn restore_unsent_message(
    states: &mut ConversationStates,
    conversation_id: cache::ConversationId,
    content: String,
) {
    states.get_or_insert(conversation_id).composer = content;
}

/// Start the conversation list loader and subscription for the active account.
#[profiling::function]
fn initialize(
    ctx: &mut AppContext,
    cache: &mut ConversationCache,
    is_narrow: bool,
    loader: &MessagesLoader,
    giftwrap_workers: &mut Vec<thread::JoinHandle<()>>,
) {
    tracing::debug!(
        "initializing Messages conversation list for selected_account={} narrow={is_narrow}",
        ctx.accounts.selected_account_pubkey()
    );
    // Reprocess only wrappers that were ingested before the account nsec was
    // available. Once `Ndb` has the key, new kind 1059 wrappers are unwrapped
    // by ingestion, so do not add a live giftwrap polling path here.
    let giftwrap_ndb = ctx.ndb.clone();
    let r = std::thread::Builder::new()
        .name("process_giftwraps".into())
        .spawn(move || {
            let txn = Transaction::new(&giftwrap_ndb).expect("txn");
            // although the actual giftwrap processing happens on the ingestion
            // pool, this function still looks up giftwraps to process on the main
            // thread, which can cause a freeze.
            //
            // TODO(jb55): move the giftwrap query logic into the internal
            // threadpool so we don't have to spawn a thread here
            tracing::debug!("starting background giftwrap processing during Messages initialize");
            giftwrap_ndb.process_giftwraps(&txn);
            tracing::debug!("finished background giftwrap processing during Messages initialize");
        });

    match r {
        Ok(handle) => giftwrap_workers.push(handle),
        Err(err) => tracing::error!("failed to spawn process_giftwraps thread: {err}"),
    }

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
    let subscription_present = sub.is_some();
    cache.state = ConversationListState::Loading { subscription: sub };
    tracing::debug!(
        "Messages conversation loader started for selected_account={} subscription_present={}",
        ctx.accounts.selected_account_pubkey(),
        subscription_present
    );

    if !is_narrow {
        cache.active = None;
    }
}

/// Joins any completed giftwrap worker threads and removes them from the worker list.
fn reap_finished_giftwrap_workers(workers: &mut Vec<thread::JoinHandle<()>>) {
    let mut idx = 0;
    while idx < workers.len() {
        if workers[idx].is_finished() {
            let worker = workers.swap_remove(idx);
            if let Err(err) = worker.join() {
                tracing::error!("process_giftwraps thread panicked during shutdown: {err:?}");
            }
        } else {
            idx += 1;
        }
    }
}

/// Joins all tracked giftwrap worker threads before Messages teardown completes.
fn join_giftwrap_workers(workers: &mut Vec<thread::JoinHandle<()>>) {
    for worker in workers.drain(..) {
        if let Err(err) = worker.join() {
            tracing::error!("process_giftwraps thread panicked during shutdown: {err:?}");
        }
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
    messages: &mut ConversationsCtx,
    loader: &MessagesLoader,
    inflight_messages: &mut HashSet<ConversationLoadKey>,
) {
    let mut handled = 0;
    while handled < MAX_LOADER_MSGS_PER_FRAME {
        let Some(msg) = loader.try_recv() else {
            break;
        };
        handled += 1;

        match msg {
            LoaderMsg::ConversationBatch {
                account_pubkey,
                keys,
            } => {
                let Some(cache) = messages.get_mut(&account_pubkey) else {
                    continue;
                };
                ingest_note_keys(ctx, cache, &keys);
            }
            LoaderMsg::ConversationFinished { account_pubkey } => {
                let Some(cache) = messages.get_mut(&account_pubkey) else {
                    continue;
                };
                finish_conversation_list_loading(cache);
            }
            LoaderMsg::ConversationMessagesBatch {
                account_pubkey,
                keys,
                ..
            } => {
                let Some(cache) = messages.get_mut(&account_pubkey) else {
                    continue;
                };
                ingest_note_keys(ctx, cache, &keys);
            }
            LoaderMsg::ConversationMessagesFinished {
                account_pubkey,
                conversation_id,
            } => {
                inflight_messages
                    .remove(&ConversationLoadKey::new(account_pubkey, conversation_id));
            }
            LoaderMsg::Failed {
                account_pubkey,
                conversation_id,
                error,
            } => {
                if let Some(conversation_id) = conversation_id {
                    inflight_messages
                        .remove(&ConversationLoadKey::new(account_pubkey, conversation_id));
                }
                tracing::error!("messages loader error for account {account_pubkey}: {error}");
            }
        }
    }
}

fn finish_conversation_list_loading(cache: &mut ConversationCache) {
    let current = std::mem::replace(&mut cache.state, ConversationListState::Initializing);
    cache.state = match current {
        ConversationListState::Loading { subscription } => {
            cache.mark_selected_startup_pending();
            ConversationListState::Initialized(subscription)
        }
        other => other,
    };
}

fn ensure_selected_startup(
    ctx: &mut AppContext<'_>,
    cache: &mut ConversationCache,
    loader: &MessagesLoader,
    inflight_messages: &mut HashSet<ConversationLoadKey>,
    is_narrow: bool,
) {
    if !cache.selected_startup_pending() {
        return;
    }

    if !matches!(&cache.state, ConversationListState::Initialized(_)) {
        return;
    }

    let account_pubkey = *ctx.accounts.selected_account_pubkey();
    let known_participants = startup_prefetch_participants(ctx.ndb, cache, &account_pubkey);
    relay_prefetch::ensure_participant_prefetch(
        &mut ctx.remote,
        ctx.accounts,
        cache,
        &known_participants,
    );

    if cache.active.is_none() && !is_narrow {
        if let Some(first) = cache.first_convo_id() {
            open_conversation_with_prefetch(&mut ctx.remote, ctx.accounts, cache, first);
            request_conversation_messages(cache, &account_pubkey, first, loader, inflight_messages);
        }
    }

    cache.mark_selected_startup_complete();
}

/// Collects known participants for startup relay-list prefetch from cache and local NDB state.
fn startup_prefetch_participants(
    ndb: &Ndb,
    cache: &ConversationCache,
    account_pubkey: &Pubkey,
) -> Vec<Pubkey> {
    let mut participants = HashSet::new();
    participants.extend(cache.known_participants_except(account_pubkey));

    let txn = Transaction::new(ndb).expect("txn");
    participants.extend(known_participant_dm_relay_list_authors(
        ndb,
        &txn,
        account_pubkey,
    ));

    participants.into_iter().collect()
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
    inflight_messages: &mut HashSet<ConversationLoadKey>,
) {
    let load_key = ConversationLoadKey::new(*me, conversation_id);
    if inflight_messages.contains(&load_key) {
        return;
    }

    let Some(conversation) = cache.get(conversation_id) else {
        return;
    };

    inflight_messages.insert(load_key);
    loader.load_conversation_messages(
        conversation_id,
        conversation.metadata.participants.clone(),
        *me,
    );
}

/// Scoped-sub owner namespace for messages DM relay-list lifecycles.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum RelayListOwner {
    Prefetch,
    Ensure,
}

const RELAY_LIST_KEY: &str = "dm_relay_list";

/// Stable owner for DM relay-list prefetch subscriptions per selected account.
fn list_prefetch_owner_key(account_pk: Pubkey) -> SubOwnerKey {
    SubOwnerKey::builder(RelayListOwner::Prefetch)
        .with(account_pk)
        .finish()
}

/// Stable owner for selected-account DM relay-list ensure subscriptions per selected account.
fn list_ensure_owner_key(account_pk: Pubkey) -> SubOwnerKey {
    SubOwnerKey::builder(RelayListOwner::Ensure)
        .with(account_pk)
        .finish()
}

/// Stable account-level key for the participant DM relay-list prefetch stream.
pub(crate) fn list_prefetch_sub_key() -> SubKey {
    SubKey::builder(RELAY_LIST_KEY)
        .with("participant_prefetch")
        .finish()
}

/// Stable key for one participant's DM relay-list remote stream.
pub fn list_fetch_sub_key(participant: &Pubkey) -> SubKey {
    SubKey::builder(RELAY_LIST_KEY)
        .with(*participant.bytes())
        .finish()
}

#[profiling::function]
pub(crate) fn ensure_selected_account_dm_relay_list(
    ndb: &mut Ndb,
    remote: &mut RemoteApi<'_>,
    accounts: &Accounts,
    cache: &mut ConversationCache,
) {
    ensure_selected_account_dm_list(ndb, remote, accounts, cache.dm_relay_list_ensure_mut())
}

/// Marks a conversation active and ensures participant relay-list prefetch.
#[profiling::function]
pub(crate) fn open_conversation_with_prefetch(
    remote: &mut RemoteApi<'_>,
    accounts: &Accounts,
    cache: &mut ConversationCache,
    conversation_id: cache::ConversationId,
) {
    cache.active = Some(conversation_id);
    relay_prefetch::ensure_conversation_prefetch(remote, accounts, cache, conversation_id);
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

    /// Get an existing conversation cache for an account-addressed async result.
    pub fn get_mut(&mut self, account: &Pubkey) -> Option<&mut ConversationCache> {
        self.convos_per_acc.get_mut(account)
    }

    /// Read-only conversation cache for the selected account, if one exists.
    pub fn get_current(&self, accounts: &Accounts) -> Option<&ConversationCache> {
        accounts.get_selected_account().keypair().secret_key?;
        self.convos_per_acc.get(accounts.selected_account_pubkey())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsent_message_restore_is_scoped_by_account() {
        let account_a = Pubkey::new([0xA1; 32]);
        let account_b = Pubkey::new([0xB2; 32]);
        let conversation_id: cache::ConversationId = 0;
        let mut states = ConversationStatesByAccount::default();

        restore_unsent_message(
            states.for_account_mut(&account_a),
            conversation_id,
            "account a draft".to_owned(),
        );

        assert_eq!(
            states
                .for_account_mut(&account_a)
                .get_or_insert(conversation_id)
                .composer,
            "account a draft"
        );
        assert!(
            states
                .for_account_mut(&account_b)
                .get_or_insert(conversation_id)
                .composer
                .is_empty(),
            "same ConversationId in a different account must not inherit a restored draft"
        );
    }

    #[test]
    fn inflight_conversation_load_is_scoped_by_account() {
        let account_a = Pubkey::new([0xA1; 32]);
        let account_b = Pubkey::new([0xB2; 32]);
        let conversation_id: cache::ConversationId = 0;
        let mut inflight = HashSet::new();

        inflight.insert(ConversationLoadKey::new(account_a, conversation_id));

        assert!(inflight.contains(&ConversationLoadKey::new(account_a, conversation_id)));
        assert!(!inflight.contains(&ConversationLoadKey::new(account_b, conversation_id)));
    }

    #[test]
    fn loader_finished_defers_selected_account_startup() {
        let mut cache = ConversationCache::new();
        cache.state = ConversationListState::Loading { subscription: None };

        finish_conversation_list_loading(&mut cache);

        assert!(matches!(
            &cache.state,
            ConversationListState::Initialized(None)
        ));
        assert!(cache.selected_startup_pending());
    }

    #[test]
    fn loader_finished_does_not_requeue_selected_startup_for_stale_finish() {
        let mut cache = ConversationCache::new();
        cache.state = ConversationListState::Initialized(None);
        cache.mark_selected_startup_complete();

        finish_conversation_list_loading(&mut cache);

        assert!(matches!(
            &cache.state,
            ConversationListState::Initialized(None)
        ));
        assert!(!cache.selected_startup_pending());
    }
}
