use crate::{
    account_manager::AccountManager,
    column::Columns,
    draft::Drafts,
    imgcache::ImageCache,
    notecache::NoteCache,
    thread::Threads,
    timeline::TimelineId,
    ui::{
        self,
        note::{post::PostResponse, QuoteRepostView},
    },
};

use enostr::{NoteId, RelayPool};
use nostrdb::{Ndb, Transaction};

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum TimelineRoute {
    Timeline(TimelineId),
    Thread(NoteId),
    Reply(NoteId),
    Quote(NoteId),
}

pub enum TimelineRouteResponse {
    Post(PostResponse),
}

impl TimelineRouteResponse {
    pub fn post(post: PostResponse) -> Self {
        TimelineRouteResponse::Post(post)
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
    threads: &mut Threads,
    accounts: &mut AccountManager,
    route: TimelineRoute,
    col: usize,
    show_postbox: bool,
    textmode: bool,
    ui: &mut egui::Ui,
) -> Option<TimelineRouteResponse> {
    match route {
        TimelineRoute::Timeline(timeline_id) => {
            if show_postbox {
                if let Some(kp) = accounts.selected_or_first_nsec() {
                    ui::timeline::postbox_view(ndb, kp, pool, drafts, img_cache, note_cache, ui);
                }
            }

            if let Some(bar_action) =
                ui::TimelineView::new(timeline_id, columns, ndb, note_cache, img_cache, textmode)
                    .ui(ui)
            {
                let txn = Transaction::new(ndb).expect("txn");
                let router = columns.columns_mut()[col].router_mut();

                bar_action.execute_and_process_result(ndb, router, threads, note_cache, pool, &txn);
            }

            None
        }

        TimelineRoute::Thread(id) => {
            if let Some(bar_action) =
                ui::ThreadView::new(threads, ndb, note_cache, img_cache, id.bytes(), textmode)
                    .id_source(egui::Id::new(("threadscroll", col)))
                    .ui(ui)
            {
                let txn = Transaction::new(ndb).expect("txn");
                let router = columns.columns_mut()[col].router_mut();
                bar_action.execute_and_process_result(ndb, router, threads, note_cache, pool, &txn);
            }

            None
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

            if let Some(poster) = accounts.selected_or_first_nsec() {
                let response = egui::ScrollArea::vertical().show(ui, |ui| {
                    ui::PostReplyView::new(ndb, poster, pool, drafts, note_cache, img_cache, &note)
                        .id_source(id)
                        .show(ui)
                });

                Some(TimelineRouteResponse::post(response.inner))
            } else {
                None
            }
        }

        TimelineRoute::Quote(id) => {
            let txn = if let Ok(txn) = Transaction::new(ndb) {
                txn
            } else {
                ui.label("Quote of unknown note");
                return None;
            };

            let note = if let Ok(note) = ndb.get_note_by_id(&txn, id.bytes()) {
                note
            } else {
                ui.label("Quote of unknown note");
                return None;
            };

            let id = egui::Id::new(("post", col, note.key().unwrap()));
            if let Some(poster) = accounts.selected_or_first_nsec() {
                let response = egui::ScrollArea::vertical().show(ui, |ui| {
                    QuoteRepostView::new(ndb, poster, pool, note_cache, img_cache, drafts, &note)
                        .id_source(id)
                        .show(ui)
                });

                Some(TimelineRouteResponse::post(response.inner))
            } else {
                None
            }
        }
    }
}
