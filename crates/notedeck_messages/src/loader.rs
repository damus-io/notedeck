//! Background loader for Messages NostrDB queries.

use crossbeam_channel as chan;
use enostr::Pubkey;
use nostrdb::{Filter, Ndb, NoteKey, Transaction};

use notedeck::AsyncLoader;

use crate::{
    cache::ConversationId,
    nip17::{chatroom_filter, conversation_filter},
};

const FOLD_BATCH_SIZE: usize = 100;

/// Commands sent to the messages loader worker thread.
pub enum LoaderCmd {
    /// Load conversation list note keys for the current account.
    LoadConversationList { account_pubkey: Pubkey },
    /// Load note keys for a specific conversation.
    LoadConversationMessages {
        conversation_id: ConversationId,
        participants: Vec<Pubkey>,
        me: Pubkey,
    },
}

/// Messages emitted by the loader worker thread.
pub enum LoaderMsg {
    /// Batch of conversation list note keys.
    ConversationBatch(Vec<NoteKey>),
    /// Conversation list load finished.
    ConversationFinished,
    /// Batch of note keys for a conversation.
    ConversationMessagesBatch {
        conversation_id: ConversationId,
        keys: Vec<NoteKey>,
    },
    /// Conversation messages load finished.
    ConversationMessagesFinished { conversation_id: ConversationId },
    /// Loader error.
    Failed(String),
}

/// Handle for driving the messages loader worker thread.
pub struct MessagesLoader {
    loader: AsyncLoader<LoaderCmd, LoaderMsg>,
}

impl MessagesLoader {
    /// Create an uninitialized loader handle.
    pub fn new() -> Self {
        Self {
            loader: AsyncLoader::new(),
        }
    }

    /// Start the loader workers if they have not been started yet.
    pub fn start(&mut self, egui_ctx: egui::Context, ndb: Ndb) {
        let _ = self
            .loader
            .start(egui_ctx, ndb, 1, "messages-loader", handle_cmd);
    }

    /// Request a conversation list load for the given account.
    pub fn load_conversation_list(&self, account_pubkey: Pubkey) {
        self.loader
            .send(LoaderCmd::LoadConversationList { account_pubkey });
    }

    /// Request a conversation message load for the given conversation.
    pub fn load_conversation_messages(
        &self,
        conversation_id: ConversationId,
        participants: Vec<Pubkey>,
        me: Pubkey,
    ) {
        self.loader.send(LoaderCmd::LoadConversationMessages {
            conversation_id,
            participants,
            me,
        });
    }

    /// Try to receive the next loader message without blocking.
    pub fn try_recv(&self) -> Option<LoaderMsg> {
        self.loader.try_recv()
    }
}

impl Default for MessagesLoader {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy)]
/// Internal fold target kind for note key batches.
enum FoldKind {
    ConversationList,
    ConversationMessages { conversation_id: ConversationId },
}

/// Fold accumulator for batching note keys.
struct FoldAcc {
    batch: Vec<NoteKey>,
    msg_tx: chan::Sender<LoaderMsg>,
    egui_ctx: egui::Context,
    kind: FoldKind,
}

impl FoldAcc {
    fn push_key(&mut self, key: NoteKey) -> Result<(), String> {
        self.batch.push(key);
        if self.batch.len() >= FOLD_BATCH_SIZE {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), String> {
        if self.batch.is_empty() {
            return Ok(());
        }

        let keys = std::mem::take(&mut self.batch);
        let msg = match self.kind {
            FoldKind::ConversationList => LoaderMsg::ConversationBatch(keys),
            FoldKind::ConversationMessages { conversation_id } => {
                LoaderMsg::ConversationMessagesBatch {
                    conversation_id,
                    keys,
                }
            }
        };

        self.msg_tx
            .send(msg)
            .map_err(|_| "messages loader channel closed".to_string())?;
        self.egui_ctx.request_repaint();
        Ok(())
    }
}

/// Run a conversation list load and stream note keys.
fn load_conversation_list(
    egui_ctx: &egui::Context,
    ndb: &Ndb,
    msg_tx: &chan::Sender<LoaderMsg>,
    account_pubkey: Pubkey,
) -> Result<(), String> {
    let filters = conversation_filter(&account_pubkey);
    fold_note_keys(egui_ctx, ndb, msg_tx, &filters, FoldKind::ConversationList)?;
    let _ = msg_tx.send(LoaderMsg::ConversationFinished);
    egui_ctx.request_repaint();
    Ok(())
}

/// Handle loader commands on a worker thread.
fn handle_cmd(
    cmd: LoaderCmd,
    egui_ctx: &egui::Context,
    ndb: &Ndb,
    msg_tx: &chan::Sender<LoaderMsg>,
) {
    let result = match cmd {
        LoaderCmd::LoadConversationList { account_pubkey } => {
            load_conversation_list(egui_ctx, ndb, msg_tx, account_pubkey)
        }
        LoaderCmd::LoadConversationMessages {
            conversation_id,
            participants,
            me,
        } => load_conversation_messages(egui_ctx, ndb, msg_tx, conversation_id, participants, me),
    };

    if let Err(err) = result {
        let _ = msg_tx.send(LoaderMsg::Failed(err));
        egui_ctx.request_repaint();
    }
}

/// Run a conversation messages load and stream note keys.
fn load_conversation_messages(
    egui_ctx: &egui::Context,
    ndb: &Ndb,
    msg_tx: &chan::Sender<LoaderMsg>,
    conversation_id: ConversationId,
    participants: Vec<Pubkey>,
    me: Pubkey,
) -> Result<(), String> {
    let participant_bytes: Vec<[u8; 32]> = participants.iter().map(|p| *p.bytes()).collect();
    let participant_refs: Vec<&[u8; 32]> = participant_bytes.iter().collect();
    let filters = chatroom_filter(participant_refs, me.bytes());

    fold_note_keys(
        egui_ctx,
        ndb,
        msg_tx,
        &filters,
        FoldKind::ConversationMessages { conversation_id },
    )?;

    let _ = msg_tx.send(LoaderMsg::ConversationMessagesFinished { conversation_id });
    egui_ctx.request_repaint();
    Ok(())
}

/// Fold over NostrDB results and emit note key batches.
fn fold_note_keys(
    egui_ctx: &egui::Context,
    ndb: &Ndb,
    msg_tx: &chan::Sender<LoaderMsg>,
    filters: &[Filter],
    kind: FoldKind,
) -> Result<(), String> {
    let txn = Transaction::new(ndb).map_err(|e| e.to_string())?;

    let acc = FoldAcc {
        batch: Vec::with_capacity(FOLD_BATCH_SIZE),
        msg_tx: msg_tx.clone(),
        egui_ctx: egui_ctx.clone(),
        kind,
    };

    let acc = ndb
        .fold(&txn, filters, acc, |mut acc, note| {
            if let Some(key) = note.key() {
                if let Err(err) = acc.push_key(key) {
                    tracing::error!("messages loader flush error: {err}");
                }
            }
            acc
        })
        .map_err(|e| e.to_string())?;

    let mut acc = acc;
    acc.flush()?;
    Ok(())
}
