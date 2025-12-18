pub mod cache;
pub mod convo_renderable;
pub mod nav;
pub mod nip17;
pub mod ui;

use enostr::Pubkey;
use hashbrown::HashMap;
use nav::{process_messages_ui_response, Route};
use nostrdb::{Subscription, Transaction};
use notedeck::{
    try_process_events_core, ui::is_narrow, Accounts, App, AppContext, AppResponse, Router,
};

use crate::{
    cache::{ConversationCache, ConversationListState, ConversationStates},
    nip17::conversation_filter,
    ui::{login_nsec_prompt, messages::messages_ui},
};

pub struct MessagesApp {
    messages: ConversationsCtx,
    states: ConversationStates,
    router: Router<Route>,
}

impl MessagesApp {
    pub fn new() -> Self {
        Self {
            messages: ConversationsCtx::default(),
            states: ConversationStates::default(),
            router: Router::new(vec![Route::ConvoList]),
        }
    }
}

impl Default for MessagesApp {
    fn default() -> Self {
        Self::new()
    }
}

impl App for MessagesApp {
    fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        try_process_events_core(ctx, ui.ctx(), |_, _| {});

        let Some(cache) = self.messages.get_current_mut(ctx.accounts) else {
            login_nsec_prompt(ui, ctx.i18n);
            return AppResponse::none();
        };

        's: {
            let Some(secret) = &ctx.accounts.get_selected_account().key.secret_key else {
                break 's;
            };

            ctx.ndb.add_key(&secret.secret_bytes());
            let txn = Transaction::new(ctx.ndb).expect("txn");
            ctx.ndb.process_giftwraps(&txn);
        }

        match cache.state {
            ConversationListState::Initializing => initialize(ctx, cache, is_narrow(ui.ctx())),
            ConversationListState::Initialized(subscription) => 's: {
                let Some(sub) = subscription else {
                    break 's;
                };
                update_initialized(ctx, cache, sub);
            }
        }

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
        let action =
            process_messages_ui_response(resp, ctx, cache, &mut self.router, is_narrow(ui.ctx()));

        AppResponse::action(action)
    }
}

fn initialize(ctx: &mut AppContext, cache: &mut ConversationCache, is_narrow: bool) {
    let txn = Transaction::new(ctx.ndb).expect("txn");
    cache.init_conversations(
        ctx.ndb,
        &txn,
        ctx.accounts.selected_account_pubkey(),
        &mut *ctx.note_cache,
        &mut *ctx.unknown_ids,
    );
    if !is_narrow {
        if let Some(first) = cache.first_convo_id() {
            cache.open_conversation(
                ctx.ndb,
                &txn,
                first,
                ctx.note_cache,
                ctx.unknown_ids,
                ctx.accounts.selected_account_pubkey(),
            );
        }
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

    cache.state = ConversationListState::Initialized(sub);
}

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
