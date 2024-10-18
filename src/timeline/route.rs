use crate::{
    account_manager::AccountManager,
    column::Columns,
    draft::Drafts,
    imgcache::ImageCache,
    note::RootNoteId,
    notecache::NoteCache,
    subscriptions::SubRefs,
    timeline::{TimelineCache, TimelineCacheKey, TimelineId},
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
    Thread(RootNoteId),
    Reply(NoteId),
    Quote(NoteId),
    Profile(Pubkey),
}

impl TimelineRoute {
    // TODO(jb55): remove this and centralize subscriptions
    pub fn subscriptions<'a>(
        &self,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &Transaction,
        columns: &'a Columns,
        timeline_cache: &'a TimelineCache,
    ) -> Option<SubRefs<'a>> {
        match self {
            TimelineRoute::Reply(_) => None,
            TimelineRoute::Quote(_) => None,

            TimelineRoute::Profile(pubkey) => timeline_cache
                .notes(
                    ndb,
                    note_cache,
                    txn,
                    &TimelineCacheKey::pubkey(pubkey),
                )
                .get_ptr()
                .subscriptions(),

            TimelineRoute::Timeline(tlid) => Some(columns.find_timeline(*tlid)?.subscriptions()),

            TimelineRoute::Thread(root_note_id) => timeline_cache
                .notes(
                    ndb,
                    note_cache,
                    txn,
                    &TimelineCacheKey::thread(*root_note_id),
                )
                .get_ptr()
                .subscriptions(),
        }
    }
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
    timeline_cache: &mut TimelineCache,
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

                bar_action.execute_and_process_result(
                    ndb,
                    router,
                    timeline_cache,
                    note_cache,
                    pool,
                    &txn,
                );
            }

            timeline_response
                .open_profile
                .map(AfterRouteExecution::OpenProfile)
        }

        TimelineRoute::Profile(pubkey) => render_profile_route(
            &pubkey,
            ndb,
            columns,
            pool,
            img_cache,
            note_cache,
            timeline_cache,
            col,
            ui,
        ),

        TimelineRoute::Thread(id) => {
            let timeline_response = ui::ThreadView::new(
                timeline_cache,
                ndb,
                note_cache,
                img_cache,
                id.bytes(),
                textmode,
            )
            .id_source(egui::Id::new(("threadscroll", col)))
            .ui(ui);
            if let Some(bar_action) = timeline_response.bar_action {
                let txn = Transaction::new(ndb).expect("txn");
                let mut cur_column = columns.columns_mut();
                let router = cur_column[col].router_mut();
                bar_action.execute_and_process_result(
                    ndb,
                    router,
                    timeline_cache,
                    note_cache,
                    pool,
                    &txn,
                );
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
    pool: &mut RelayPool,
    img_cache: &mut ImageCache,
    note_cache: &mut NoteCache,
    timeline_cache: &mut TimelineCache,
    col: usize,
    ui: &mut egui::Ui,
) -> Option<AfterRouteExecution> {
    let timeline_response =
        ProfileView::new(pubkey, col, timeline_cache, ndb, note_cache, img_cache).ui(ui);
    if let Some(bar_action) = timeline_response.bar_action {
        let txn = nostrdb::Transaction::new(ndb).expect("txn");
        let mut cur_column = columns.columns_mut();
        let router = cur_column[col].router_mut();

        bar_action.execute_and_process_result(ndb, router, timeline_cache, note_cache, pool, &txn);
    }

    timeline_response
        .open_profile
        .map(AfterRouteExecution::OpenProfile)
}
