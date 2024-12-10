use crate::{
    accounts::Accounts,
    column::Columns,
    draft::Drafts,
    imgcache::ImageCache,
    muted::MuteFun,
    nav::RenderNavAction,
    notecache::NoteCache,
    notes_holder::NotesHolderStorage,
    profile::Profile,
    thread::Thread,
    timeline::{TimelineId, TimelineKind},
    ui::{
        self,
        note::{NoteOptions, QuoteRepostView},
        profile::ProfileView,
    },
    unknowns::UnknownIds,
};

use enostr::{NoteId, Pubkey};
use nostrdb::{Ndb, Transaction};

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum TimelineRoute {
    Timeline(TimelineId),
    Thread(NoteId),
    Profile(Pubkey),
    Reply(NoteId),
    Quote(NoteId),
}

#[allow(clippy::too_many_arguments)]
pub fn render_timeline_route(
    ndb: &Ndb,
    columns: &mut Columns,
    drafts: &mut Drafts,
    img_cache: &mut ImageCache,
    unknown_ids: &mut UnknownIds,
    note_cache: &mut NoteCache,
    threads: &mut NotesHolderStorage<Thread>,
    profiles: &mut NotesHolderStorage<Profile>,
    accounts: &mut Accounts,
    route: TimelineRoute,
    col: usize,
    textmode: bool,
    ui: &mut egui::Ui,
) -> Option<RenderNavAction> {
    match route {
        TimelineRoute::Timeline(timeline_id) => {
            let note_options = {
                let is_universe = if let Some(timeline) = columns.find_timeline(timeline_id) {
                    timeline.kind == TimelineKind::Universe
                } else {
                    false
                };

                let mut options = NoteOptions::new(is_universe);
                options.set_textmode(textmode);
                options
            };

            let note_action = ui::TimelineView::new(
                timeline_id,
                columns,
                ndb,
                note_cache,
                img_cache,
                note_options,
            )
            .ui(ui);

            note_action.map(RenderNavAction::NoteAction)
        }

        TimelineRoute::Thread(id) => ui::ThreadView::new(
            threads,
            ndb,
            note_cache,
            unknown_ids,
            img_cache,
            id.bytes(),
            textmode,
        )
        .id_source(egui::Id::new(("threadscroll", col)))
        .ui(ui, &accounts.mutefun())
        .map(Into::into),

        TimelineRoute::Reply(id) => {
            let txn = if let Ok(txn) = Transaction::new(ndb) {
                txn
            } else {
                ui.label("Reply to unknown note");
                return None;
            };

            let note = if let Ok(note) = ndb.get_note_by_id(&txn, id.bytes()) {
                note
            } else {
                ui.label("Reply to unknown note");
                return None;
            };

            let id = egui::Id::new(("post", col, note.key().unwrap()));
            let poster = accounts.selected_or_first_nsec()?;

            let action = {
                let draft = drafts.reply_mut(note.id());

                let response = egui::ScrollArea::vertical().show(ui, |ui| {
                    ui::PostReplyView::new(ndb, poster, draft, note_cache, img_cache, &note)
                        .id_source(id)
                        .show(ui)
                });

                response.inner.action
            };

            action.map(Into::into)
        }

        TimelineRoute::Profile(pubkey) => render_profile_route(
            &pubkey,
            ndb,
            profiles,
            img_cache,
            note_cache,
            col,
            ui,
            &accounts.mutefun(),
        ),

        TimelineRoute::Quote(id) => {
            let txn = Transaction::new(ndb).expect("txn");

            let note = if let Ok(note) = ndb.get_note_by_id(&txn, id.bytes()) {
                note
            } else {
                ui.label("Quote of unknown note");
                return None;
            };

            let id = egui::Id::new(("post", col, note.key().unwrap()));

            let poster = accounts.selected_or_first_nsec()?;
            let draft = drafts.quote_mut(note.id());

            let response = egui::ScrollArea::vertical().show(ui, |ui| {
                QuoteRepostView::new(ndb, poster, note_cache, img_cache, draft, &note)
                    .id_source(id)
                    .show(ui)
            });

            response.inner.action.map(Into::into)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn render_profile_route(
    pubkey: &Pubkey,
    ndb: &Ndb,
    profiles: &mut NotesHolderStorage<Profile>,
    img_cache: &mut ImageCache,
    note_cache: &mut NoteCache,
    col: usize,
    ui: &mut egui::Ui,
    is_muted: &MuteFun,
) -> Option<RenderNavAction> {
    let note_action = ProfileView::new(
        pubkey,
        col,
        profiles,
        ndb,
        note_cache,
        img_cache,
        NoteOptions::default(),
    )
    .ui(ui, is_muted);

    note_action.map(RenderNavAction::NoteAction)
}
