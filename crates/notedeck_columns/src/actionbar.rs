use crate::{
    column::Columns,
    route::{Route, Router},
    timeline::{TimelineCache, TimelineKind},
};

use enostr::{NoteId, Pubkey, RelayPool};
use nostrdb::{Ndb, NoteKey, Transaction};
use notedeck::{
    get_wallet_for_mut, Accounts, GlobalWallet, NoteCache, NoteZapTargetOwned, UnknownIds,
    ZapTarget, ZappingError, Zaps,
};
use tracing::error;

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum NoteAction {
    Reply(NoteId),
    Quote(NoteId),
    OpenTimeline(TimelineKind),
    Zap(ZapAction),
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum ZapAction {
    Send(NoteZapTargetOwned),
    ClearError(NoteZapTargetOwned),
}

pub struct NewNotes {
    pub id: TimelineKind,
    pub notes: Vec<NoteKey>,
}

pub enum TimelineOpenResult {
    NewNotes(NewNotes),
}

impl NoteAction {
    #[allow(clippy::too_many_arguments)]
    pub fn execute(
        &self,
        ndb: &Ndb,
        router: &mut Router<Route>,
        timeline_cache: &mut TimelineCache,
        note_cache: &mut NoteCache,
        pool: &mut RelayPool,
        txn: &Transaction,
        accounts: &mut Accounts,
        global_wallet: &mut GlobalWallet,
        zaps: &mut Zaps,
    ) -> Option<TimelineOpenResult> {
        match self {
            NoteAction::Reply(note_id) => {
                router.route_to(Route::reply(*note_id));
                None
            }

            NoteAction::OpenTimeline(kind) => {
                router.route_to(Route::Timeline(kind.to_owned()));
                timeline_cache.open(ndb, note_cache, txn, pool, kind)
            }

            NoteAction::Quote(note_id) => {
                router.route_to(Route::quote(*note_id));
                None
            }

            NoteAction::Zap(zap_action) => 's: {
                let Some(cur_acc) = accounts.get_selected_account_mut() else {
                    break 's None;
                };

                let sender = cur_acc.key.pubkey;

                match zap_action {
                    ZapAction::Send(target) => {
                        if get_wallet_for_mut(accounts, global_wallet, sender.bytes()).is_some() {
                            send_zap(&sender, zaps, pool, target)
                        } else {
                            zaps.send_error(
                                sender.bytes(),
                                ZapTarget::Note(target.into()),
                                ZappingError::SenderNoWallet,
                            );
                        }
                    }
                    ZapAction::ClearError(target) => clear_zap_error(&sender, zaps, target),
                }

                None
            }
        }
    }

    /// Execute the NoteAction and process the TimelineOpenResult
    #[allow(clippy::too_many_arguments)]
    pub fn execute_and_process_result(
        &self,
        ndb: &Ndb,
        columns: &mut Columns,
        col: usize,
        timeline_cache: &mut TimelineCache,
        note_cache: &mut NoteCache,
        pool: &mut RelayPool,
        txn: &Transaction,
        unknown_ids: &mut UnknownIds,
        accounts: &mut Accounts,
        global_wallet: &mut GlobalWallet,
        zaps: &mut Zaps,
    ) {
        let router = columns.column_mut(col).router_mut();
        if let Some(br) = self.execute(
            ndb,
            router,
            timeline_cache,
            note_cache,
            pool,
            txn,
            accounts,
            global_wallet,
            zaps,
        ) {
            br.process(ndb, note_cache, txn, timeline_cache, unknown_ids);
        }
    }
}

fn send_zap(sender: &Pubkey, zaps: &mut Zaps, pool: &RelayPool, target: &NoteZapTargetOwned) {
    let default_zap_msats = 10_000; // TODO(kernelkind): allow the user to set this default
    let zap_target = ZapTarget::Note(target.into());

    let sender_relays: Vec<String> = pool.relays.iter().map(|r| r.url().to_string()).collect();
    zaps.send_zap(sender.bytes(), sender_relays, zap_target, default_zap_msats);
}

fn clear_zap_error(sender: &Pubkey, zaps: &mut Zaps, target: &NoteZapTargetOwned) {
    zaps.clear_error_for(sender.bytes(), ZapTarget::Note(target.into()));
}

impl TimelineOpenResult {
    pub fn new_notes(notes: Vec<NoteKey>, id: TimelineKind) -> Self {
        Self::NewNotes(NewNotes::new(notes, id))
    }

    pub fn process(
        &self,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &Transaction,
        storage: &mut TimelineCache,
        unknown_ids: &mut UnknownIds,
    ) {
        match self {
            // update the thread for next render if we have new notes
            TimelineOpenResult::NewNotes(new_notes) => {
                new_notes.process(storage, ndb, txn, unknown_ids, note_cache);
            }
        }
    }
}

impl NewNotes {
    pub fn new(notes: Vec<NoteKey>, id: TimelineKind) -> Self {
        NewNotes { notes, id }
    }

    /// Simple helper for processing a NewThreadNotes result. It simply
    /// inserts/merges the notes into the corresponding timeline cache
    pub fn process(
        &self,
        timeline_cache: &mut TimelineCache,
        ndb: &Ndb,
        txn: &Transaction,
        unknown_ids: &mut UnknownIds,
        note_cache: &mut NoteCache,
    ) {
        let reversed = matches!(&self.id, TimelineKind::Thread(_));

        let timeline = if let Some(profile) = timeline_cache.timelines.get_mut(&self.id) {
            profile
        } else {
            error!("NewNotes: could not get timeline for key {}", self.id);
            return;
        };

        if let Err(err) = timeline.insert(&self.notes, ndb, txn, unknown_ids, note_cache, reversed)
        {
            error!("error inserting notes into profile timeline: {err}")
        }
    }
}
