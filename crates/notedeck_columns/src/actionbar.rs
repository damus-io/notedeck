use std::collections::HashSet;

use crate::{
    column::Columns,
    nav::{RouterAction, RouterType},
    route::Route,
    timeline::{
        thread::{selected_has_at_least_n_replies, NoteSeenFlags, ThreadNode, Threads},
        InsertionResponse, ThreadSelection, TimelineCache, TimelineKind,
    },
    view_state::ViewState,
};

use egui_nav::Percent;
use enostr::{NoteId, Pubkey, RelayPool};
use nostrdb::{Ndb, NoteKey, Transaction};
use notedeck::{
    get_wallet_for, note::ZapTargetAmount, Accounts, GlobalWallet, Images, NoteAction, NoteCache,
    NoteZapTargetOwned, UnknownIds, ZapAction, ZapTarget, ZappingError, Zaps,
};
use notedeck_ui::media::MediaViewerFlags;
use tracing::error;

pub struct NewNotes {
    pub id: TimelineKind,
    pub notes: Vec<NoteKey>,
}

pub enum NotesOpenResult {
    Timeline(TimelineOpenResult),
    Thread(NewThreadNotes),
}

pub struct TimelineOpenResult {
    new_notes: Option<NewNotes>,
    new_pks: Option<HashSet<Pubkey>>,
}

struct NoteActionResponse {
    timeline_res: Option<NotesOpenResult>,
    router_action: Option<RouterAction>,
}

/// The note action executor for notedeck_columns
#[allow(clippy::too_many_arguments)]
fn execute_note_action(
    action: NoteAction,
    ndb: &mut Ndb,
    timeline_cache: &mut TimelineCache,
    threads: &mut Threads,
    note_cache: &mut NoteCache,
    pool: &mut RelayPool,
    txn: &Transaction,
    accounts: &mut Accounts,
    global_wallet: &mut GlobalWallet,
    zaps: &mut Zaps,
    images: &mut Images,
    view_state: &mut ViewState,
    router_type: RouterType,
    ui: &mut egui::Ui,
    col: usize,
) -> NoteActionResponse {
    let mut timeline_res = None;
    let mut router_action = None;
    let can_post = accounts.get_selected_account().key.secret_key.is_some();

    match action {
        NoteAction::Scroll(ref scroll_info) => {
            tracing::trace!("timeline scroll {scroll_info:?}")
        }

        NoteAction::Reply(note_id) => {
            if can_post {
                router_action = Some(RouterAction::route_to(Route::reply(note_id)));
            } else {
                router_action = Some(RouterAction::route_to(Route::accounts()));
            }
        }
        NoteAction::Profile(pubkey) => {
            let kind = TimelineKind::Profile(pubkey);
            router_action = Some(RouterAction::route_to(Route::Timeline(kind.clone())));
            timeline_res = timeline_cache
                .open(ndb, note_cache, txn, pool, &kind)
                .map(NotesOpenResult::Timeline);
        }
        NoteAction::Note { note_id, preview } => 'ex: {
            let Ok(thread_selection) = ThreadSelection::from_note_id(ndb, note_cache, txn, note_id)
            else {
                tracing::error!("No thread selection for {}?", hex::encode(note_id.bytes()));
                break 'ex;
            };

            timeline_res = threads
                .open(
                    ndb,
                    txn,
                    pool,
                    &thread_selection,
                    Some(note_id),
                    preview,
                    col,
                )
                .map(NotesOpenResult::Thread);

            let route = Route::Thread(thread_selection);

            router_action = Some(RouterAction::Overlay {
                route,
                make_new: preview,
            });
        }
        NoteAction::Hashtag(htag) => {
            let kind = TimelineKind::Hashtag(vec![htag.clone()]);
            router_action = Some(RouterAction::route_to(Route::Timeline(kind.clone())));
            timeline_res = timeline_cache
                .open(ndb, note_cache, txn, pool, &kind)
                .map(NotesOpenResult::Timeline);
        }
        NoteAction::Repost(note_id) => {
            if can_post {
                router_action = Some(RouterAction::route_to_sheet(
                    Route::RepostDecision(note_id),
                    egui_nav::Split::AbsoluteFromBottom(224.0),
                ));
            } else {
                router_action = Some(RouterAction::route_to(Route::accounts()));
            }
        }
        NoteAction::Zap(zap_action) => {
            let cur_acc = accounts.get_selected_account();

            let sender = cur_acc.key.pubkey;

            match &zap_action {
                ZapAction::Send(target) => 'a: {
                    let Some(wallet) = get_wallet_for(accounts, global_wallet, sender.bytes())
                    else {
                        zaps.send_error(
                            sender.bytes(),
                            ZapTarget::Note((&target.target).into()),
                            ZappingError::SenderNoWallet,
                        );
                        break 'a;
                    };

                    if let RouterType::Sheet(_) = router_type {
                        router_action = Some(RouterAction::GoBack);
                    }

                    send_zap(
                        &sender,
                        zaps,
                        pool,
                        target,
                        wallet.default_zap.get_default_zap_msats(),
                    )
                }
                ZapAction::ClearError(target) => clear_zap_error(&sender, zaps, target),
                ZapAction::CustomizeAmount(target) => {
                    let route = Route::CustomizeZapAmount(target.to_owned());
                    router_action = Some(RouterAction::route_to_sheet(
                        route,
                        egui_nav::Split::PercentFromTop(Percent::new(35).expect("35 <= 100")),
                    ));
                }
            }
        }
        NoteAction::Context(context) => match ndb.get_note_by_key(txn, context.note_key) {
            Err(err) => tracing::error!("{err}"),
            Ok(note) => {
                context.action.process(
                    ui,
                    &note,
                    pool,
                    *accounts.selected_account_pubkey().bytes() == *note.pubkey(),
                );
            }
        },
        NoteAction::Media(media_action) => {
            media_action.on_view_media(|medias| {
                view_state.media_viewer.media_info = medias.clone();
                tracing::debug!("on_view_media {:?}", &medias);
                view_state
                    .media_viewer
                    .flags
                    .set(MediaViewerFlags::Open, true);
            });

            media_action.process_default_media_actions(images)
        }
    }

    NoteActionResponse {
        timeline_res,
        router_action,
    }
}

/// Execute a NoteAction and process the result
#[allow(clippy::too_many_arguments)]
pub fn execute_and_process_note_action(
    action: NoteAction,
    ndb: &mut Ndb,
    columns: &mut Columns,
    col: usize,
    timeline_cache: &mut TimelineCache,
    threads: &mut Threads,
    note_cache: &mut NoteCache,
    pool: &mut RelayPool,
    txn: &Transaction,
    unknown_ids: &mut UnknownIds,
    accounts: &mut Accounts,
    global_wallet: &mut GlobalWallet,
    zaps: &mut Zaps,
    images: &mut Images,
    view_state: &mut ViewState,
    ui: &mut egui::Ui,
) -> Option<RouterAction> {
    let router_type = {
        let sheet_router = &mut columns.column_mut(col).sheet_router;

        if sheet_router.route().is_some() {
            RouterType::Sheet(sheet_router.split)
        } else {
            RouterType::Stack
        }
    };

    let resp = execute_note_action(
        action,
        ndb,
        timeline_cache,
        threads,
        note_cache,
        pool,
        txn,
        accounts,
        global_wallet,
        zaps,
        images,
        view_state,
        router_type,
        ui,
        col,
    );

    if let Some(br) = resp.timeline_res {
        match br {
            NotesOpenResult::Timeline(timeline_open_result) => {
                timeline_open_result.process(ndb, note_cache, txn, timeline_cache, unknown_ids);
            }
            NotesOpenResult::Thread(thread_open_result) => {
                thread_open_result.process(threads, ndb, txn, unknown_ids, note_cache);
            }
        }
    }

    resp.router_action
}

fn send_zap(
    sender: &Pubkey,
    zaps: &mut Zaps,
    pool: &RelayPool,
    target_amount: &ZapTargetAmount,
    default_msats: u64,
) {
    let zap_target = ZapTarget::Note((&target_amount.target).into());

    let msats = target_amount.specified_msats.unwrap_or(default_msats);

    let sender_relays: Vec<String> = pool.relays.iter().map(|r| r.url().to_string()).collect();
    zaps.send_zap(sender.bytes(), sender_relays, zap_target, msats);
}

fn clear_zap_error(sender: &Pubkey, zaps: &mut Zaps, target: &NoteZapTargetOwned) {
    zaps.clear_error_for(sender.bytes(), ZapTarget::Note(target.into()));
}

impl TimelineOpenResult {
    pub fn new_notes(notes: Vec<NoteKey>, id: TimelineKind) -> Self {
        Self {
            new_notes: Some(NewNotes { id, notes }),
            new_pks: None,
        }
    }

    pub fn new_pks(pks: HashSet<Pubkey>) -> Self {
        Self {
            new_notes: None,
            new_pks: Some(pks),
        }
    }

    pub fn insert_pks(&mut self, pks: HashSet<Pubkey>) {
        match &mut self.new_pks {
            Some(cur_pks) => cur_pks.extend(pks),
            None => self.new_pks = Some(pks),
        }
    }

    pub fn process(
        &self,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &Transaction,
        storage: &mut TimelineCache,
        unknown_ids: &mut UnknownIds,
    ) {
        // update the thread for next render if we have new notes
        if let Some(new_notes) = &self.new_notes {
            new_notes.process(storage, ndb, txn, unknown_ids, note_cache);
        }

        let Some(pks) = &self.new_pks else {
            return;
        };

        for pk in pks {
            unknown_ids.add_pubkey_if_missing(ndb, txn, pk);
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
        let reversed = false;

        let timeline = if let Some(profile) = timeline_cache.get_mut(&self.id) {
            profile
        } else {
            error!("NewNotes: could not get timeline for key {:?}", self.id);
            return;
        };

        if let Err(err) = timeline.insert(&self.notes, ndb, txn, unknown_ids, note_cache, reversed)
        {
            error!("error inserting notes into profile timeline: {err}")
        }
    }
}

pub struct NewThreadNotes {
    pub selected_note_id: NoteId,
    pub notes: Vec<NoteKey>,
}

impl NewThreadNotes {
    pub fn process(
        &self,
        threads: &mut Threads,
        ndb: &Ndb,
        txn: &Transaction,
        unknown_ids: &mut UnknownIds,
        note_cache: &mut NoteCache,
    ) {
        let Some(node) = threads.threads.get_mut(&self.selected_note_id.bytes()) else {
            tracing::error!("Could not find thread node for {:?}", self.selected_note_id);
            return;
        };

        process_thread_notes(
            &self.notes,
            node,
            &mut threads.seen_flags,
            ndb,
            txn,
            unknown_ids,
            note_cache,
        );
    }
}

pub fn process_thread_notes(
    notes: &Vec<NoteKey>,
    thread: &mut ThreadNode,
    seen_flags: &mut NoteSeenFlags,
    ndb: &Ndb,
    txn: &Transaction,
    unknown_ids: &mut UnknownIds,
    note_cache: &mut NoteCache,
) {
    if notes.is_empty() {
        return;
    }

    let mut has_spliced_resp = false;
    let mut num_new_notes = 0;
    for key in notes {
        let note = if let Ok(note) = ndb.get_note_by_key(txn, *key) {
            note
        } else {
            tracing::error!(
                "hit race condition in poll_notes_into_view: https://github.com/damus-io/nostrdb/issues/35 note {:?} was not added to timeline",
                key
            );
            continue;
        };

        // Ensure that unknown ids are captured when inserting notes
        UnknownIds::update_from_note(txn, ndb, unknown_ids, note_cache, &note);

        let created_at = note.created_at();
        let note_ref = notedeck::NoteRef {
            key: *key,
            created_at,
        };

        if thread.replies.contains_key(&note_ref.key) {
            continue;
        }

        let insertion_resp = thread.replies.insert(note_ref);
        if let InsertionResponse::Merged(crate::timeline::MergeKind::Spliced) = insertion_resp {
            has_spliced_resp = true;
        }

        if matches!(insertion_resp, InsertionResponse::Merged(_)) {
            num_new_notes += 1;
        }

        if !seen_flags.contains(note.id()) {
            let cached_note = note_cache.cached_note_or_insert_mut(*key, &note);

            let note_reply = cached_note.reply.borrow(note.tags());

            let has_reply = if let Some(root) = note_reply.root() {
                selected_has_at_least_n_replies(ndb, txn, Some(note.id()), root.id, 1)
            } else {
                selected_has_at_least_n_replies(ndb, txn, None, note.id(), 1)
            };

            seen_flags.mark_replies(note.id(), has_reply);
        }
    }

    if has_spliced_resp {
        tracing::debug!(
            "spliced when inserting {} new notes, resetting virtual list",
            num_new_notes
        );
        thread.list.reset();
    }
}
