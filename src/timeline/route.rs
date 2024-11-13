use crate::{
    account_manager::AccountManager,
    column::Columns,
    draft::Drafts,
    imgcache::ImageCache,
    notecache::NoteCache,
    notes_holder::NotesHolderStorage,
    profile::Profile,
    thread::Thread,
    timeline::TimelineId,
    ui::{
        self,
        note::{
            post::{PostAction, PostResponse},
            QuoteRepostView,
        },
        profile::ProfileView,
    },
};

use enostr::{NoteId, Pubkey, RelayPool};
use nostrdb::{Ndb, Transaction};

#[derive(Debug, Eq, PartialEq, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub enum TimelineRoute {
    Timeline(TimelineId),
    Thread(NoteId),
    Reply(NoteId),
    Quote(NoteId),
}

pub enum AfterRouteExecution {
    Post(PostResponse),
    OpenProfile(Pubkey),
}

impl AfterRouteExecution {
    pub fn post(post: PostResponse) -> Self {
        AfterRouteExecution::Post(post)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn render_timeline_route(
    ndb: &Ndb,
    columns: &mut Columns,
    pool: &mut RelayPool,
    drafts: &mut Drafts,
    img_cache: &mut ImageCache,
    note_cache: &mut NoteCache,
    threads: &mut NotesHolderStorage<Thread>,
    accounts: &mut AccountManager,
    route: TimelineRoute,
    col: usize,
    textmode: bool,
    ui: &mut egui::Ui,
) -> Option<AfterRouteExecution> {
    match route {
        TimelineRoute::Timeline(timeline_id) => {
            let timeline_response =
                ui::TimelineView::new(timeline_id, columns, ndb, note_cache, img_cache, textmode)
                    .ui(ui);
            if let Some(bar_action) = timeline_response.bar_action {
                let txn = Transaction::new(ndb).expect("txn");
                let mut cur_column = columns.columns_mut();
                let router = cur_column[col].router_mut();

                bar_action.execute_and_process_result(ndb, router, threads, note_cache, pool, &txn);
            }

            timeline_response
                .open_profile
                .map(AfterRouteExecution::OpenProfile)
        }

        TimelineRoute::Thread(id) => {
            let timeline_response =
                ui::ThreadView::new(threads, ndb, note_cache, img_cache, id.bytes(), textmode)
                    .id_source(egui::Id::new(("threadscroll", col)))
                    .ui(ui);
            if let Some(bar_action) = timeline_response.bar_action {
                let txn = Transaction::new(ndb).expect("txn");
                let mut cur_column = columns.columns_mut();
                let router = cur_column[col].router_mut();
                bar_action.execute_and_process_result(ndb, router, threads, note_cache, pool, &txn);
            }

            timeline_response
                .open_profile
                .map(AfterRouteExecution::OpenProfile)
        }

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
            let draft = drafts.reply_mut(note.id());

            let response = egui::ScrollArea::vertical().show(ui, |ui| {
                ui::PostReplyView::new(ndb, poster, draft, note_cache, img_cache, &note)
                    .id_source(id)
                    .show(ui)
            });

            if let Some(action) = &response.inner.action {
                PostAction::execute(poster, action, pool, draft, |np, seckey| {
                    np.to_reply(seckey, &note)
                });
            }

            Some(AfterRouteExecution::post(response.inner))
        }

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

            if let Some(action) = &response.inner.action {
                PostAction::execute(poster, action, pool, draft, |np, seckey| {
                    np.to_quote(seckey, &note)
                });
            }
            Some(AfterRouteExecution::post(response.inner))
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn render_profile_route(
    pubkey: &Pubkey,
    ndb: &Ndb,
    columns: &mut Columns,
    profiles: &mut NotesHolderStorage<Profile>,
    pool: &mut RelayPool,
    img_cache: &mut ImageCache,
    note_cache: &mut NoteCache,
    threads: &mut NotesHolderStorage<Thread>,
    col: usize,
    ui: &mut egui::Ui,
) -> Option<AfterRouteExecution> {
    let timeline_response =
        ProfileView::new(pubkey, col, profiles, ndb, note_cache, img_cache).ui(ui);
    if let Some(bar_action) = timeline_response.bar_action {
        let txn = nostrdb::Transaction::new(ndb).expect("txn");
        let mut cur_column = columns.columns_mut();
        let router = cur_column[col].router_mut();

        bar_action.execute_and_process_result(ndb, router, threads, note_cache, pool, &txn);
    }

    timeline_response
        .open_profile
        .map(AfterRouteExecution::OpenProfile)
}
