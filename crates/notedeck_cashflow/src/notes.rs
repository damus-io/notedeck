//! The notes left on transactions, kept current for the views: which
//! records have any (the register's 📝), the open detail's thread, and the
//! notes the detail asked to post.
//!
//! [`NoteSource`] wraps core's [`NotesCache`], which subscribes to the
//! account's kind-1111 notes and refolds only when one arrives, and reads
//! the open record's thread ([`notes::notes_of`]) only when that cache moved
//! or another record opened, never on a quiet frame. It is updated from
//! `render`, after the chrome's nav stack has said which detail is open, so
//! a detail's first frame already has its thread.
//!
//! A post doesn't go through the Transactions view's outbox of
//! [`cashflow::triage::Action`]s: those are edits, each planned off-thread
//! against a re-derive before it is made, and a note never feeds derive.
//! So it has its own small outbox, [`NotesCx::outbox`], flushed by
//! [`NoteSource::flush`] beside the edits.

use std::collections::HashSet;

use cashflow::Vault;
use cashflow::notes::{self, Note, NotesCache};
use nostrdb::Transaction;
use nostrdb_net::Pubkey;
use notedeck::AppContext;

/// A note the detail asked to post: `text` on `record`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotePost {
    /// The record it is on.
    pub record: String,
    /// What it says.
    pub text: String,
}

/// One record's thread, as last read.
#[derive(Default)]
pub struct Thread {
    /// The record it is the thread of; empty with no detail open.
    pub record: String,
    /// Bumped each time it is read again, so a view's galleys know.
    pub generation: u64,
    /// Its notes, oldest first.
    pub notes: Vec<Note>,
    /// The account reading it, so a note can say "you".
    pub me: Option<Pubkey>,
}

/// What the views read of the notes, and where they queue a post.
pub struct NotesCx<'a> {
    /// The records with notes; `None` until the first poll.
    pub noted: Option<&'a HashSet<String>>,
    /// Bumps whenever [`Self::noted`] may have changed.
    pub generation: u64,
    /// The open detail's thread.
    pub thread: &'a Thread,
    /// Notes to post once this frame is drawn.
    pub outbox: &'a mut Vec<NotePost>,
    /// Whether a post was made that hasn't come back through the
    /// subscription yet.
    pub saving: bool,
}

/// The account's notes, polled each frame. See the module docs.
#[derive(Default)]
pub struct NoteSource {
    /// The live cache; `None` until the first update.
    cache: Option<NotesCache>,
    /// The open detail's thread.
    thread: Thread,
    /// What the detail asked to post this frame.
    outbox: Vec<NotePost>,
    /// The last post's stamp, which the next one stamps past.
    after: u64,
    /// [`NotesCache::generation`] when the last post was made, while it
    /// hasn't arrived: the detail says "Saving…".
    saving_since: Option<u64>,
}

impl NoteSource {
    /// Take any notes that arrived, and read `open`'s thread again if they
    /// did or `open` changed.
    #[profiling::function]
    pub fn update(&mut self, ctx: &mut AppContext<'_>, open: Option<&str>) {
        let author = *ctx.accounts.selected_account_pubkey();
        if self.cache.as_ref().is_none_or(|c| c.author() != &author) {
            // Before the txn the first poll reads: see `NotesCache::new`.
            self.cache = Some(NotesCache::new(ctx.ndb, author));
            self.thread = Thread::default();
        }
        let Some(cache) = self.cache.as_mut() else {
            return;
        };
        let Ok(txn) = Transaction::new(ctx.ndb) else {
            return;
        };
        let moved = cache.poll(ctx.ndb, &txn);
        if self
            .saving_since
            .is_some_and(|since| cache.generation() > since)
        {
            self.saving_since = None;
        }
        let open = open.unwrap_or("");
        if !moved && self.thread.record == open {
            return;
        }
        if self.thread.record != open {
            self.thread.record = open.to_owned();
        }
        self.thread.notes = if open.is_empty() {
            Vec::new()
        } else {
            notes::notes_of(ctx.ndb, &txn, &author, open)
        };
        self.thread.me = Some(author);
        self.thread.generation += 1;
    }

    /// What the views read this frame.
    pub fn cx(&mut self) -> NotesCx<'_> {
        NotesCx {
            noted: self.cache.as_ref().and_then(NotesCache::noted),
            generation: self.cache.as_ref().map_or(0, NotesCache::generation),
            thread: &self.thread,
            outbox: &mut self.outbox,
            saving: self.saving_since.is_some(),
        }
    }

    /// Whether a post is on its way, so the frame polls again soon.
    pub fn is_saving(&self) -> bool {
        self.saving_since.is_some()
    }

    /// Post what the detail queued, as the selected account. Returns why
    /// the last one that failed did, for the detail to say.
    pub fn flush(&mut self, ctx: &mut AppContext<'_>) -> Option<String> {
        if self.outbox.is_empty() {
            return None;
        }
        let vault = ctx
            .accounts
            .selected_filled()
            .and_then(|full| Vault::new(&full.secret_key.secret_bytes()));
        let mut failed = None;
        for post in self.outbox.drain(..) {
            let Some(vault) = &vault else {
                failed = Some(
                    "this account has no secret key, so it can't write to the vault".to_owned(),
                );
                continue;
            };
            match notes::post(vault, ctx.ndb, &post.record, &post.text, self.after) {
                Ok(written) => {
                    self.after = written.created_at;
                    let generation = self.cache.as_ref().map_or(0, NotesCache::generation);
                    self.saving_since = Some(generation);
                }
                Err(err) => {
                    tracing::warn!("cashflow: posting a note on {}: {err}", post.record);
                    failed = Some(err.to_string());
                }
            }
        }
        failed
    }
}
