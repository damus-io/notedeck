//! Background loader for initial timeline NostrDB queries.

use crossbeam_channel as chan;
use nostrdb::{Ndb, Transaction};
use notedeck::{worker_count, AsyncLoader, FilterState, NoteRef};

use crate::timeline::kind::AlgoTimeline;
use crate::timeline::TimelineKind;

use tracing::{info, warn};

const FOLD_BATCH_SIZE: usize = 200;
const MAX_TIMELINE_LOADER_WORKERS: usize = 4;

/// Commands sent to the timeline loader worker thread.
pub enum TimelineLoaderCmd {
    /// Load initial note refs for a timeline.
    LoadTimeline {
        /// Timeline identifier to apply batches to.
        kind: TimelineKind,
    },
}

/// Messages emitted by the timeline loader worker thread.
pub enum TimelineLoaderMsg {
    /// Batch of note refs for a timeline.
    TimelineBatch {
        /// Timeline identifier to apply batches to.
        kind: TimelineKind,
        /// Note refs discovered by NostrDB fold.
        notes: Vec<NoteRef>,
    },
    /// Timeline initial load finished.
    TimelineFinished { kind: TimelineKind },
    /// Loader error for a timeline.
    Failed { kind: TimelineKind, error: String },
}

/// Handle for driving the timeline loader worker thread.
pub struct TimelineLoader {
    loader: AsyncLoader<TimelineLoaderCmd, TimelineLoaderMsg>,
}

impl TimelineLoader {
    /// Create an uninitialized loader handle.
    pub fn new() -> Self {
        Self {
            loader: AsyncLoader::new(),
        }
    }

    /// Start the loader workers if they have not been started yet.
    pub fn start(&mut self, egui_ctx: egui::Context, ndb: Ndb) {
        let workers = worker_count(MAX_TIMELINE_LOADER_WORKERS);
        let started = self.loader.start(
            egui_ctx,
            ndb,
            workers,
            "columns-timeline-loader",
            handle_cmd,
        );
        if started {
            info!(workers, "starting timeline loader workers");
        }
    }

    /// Request an initial load for a timeline.
    pub fn load_timeline(&self, kind: TimelineKind) {
        self.loader.send(TimelineLoaderCmd::LoadTimeline { kind });
    }

    /// Try to receive the next loader message without blocking.
    pub fn try_recv(&self) -> Option<TimelineLoaderMsg> {
        self.loader.try_recv()
    }
}

impl Default for TimelineLoader {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle loader commands on a worker thread.
fn handle_cmd(
    cmd: TimelineLoaderCmd,
    egui_ctx: &egui::Context,
    ndb: &Ndb,
    msg_tx: &chan::Sender<TimelineLoaderMsg>,
) {
    let result = match cmd {
        TimelineLoaderCmd::LoadTimeline { kind } => load_timeline(egui_ctx, ndb, msg_tx, kind),
    };

    if let Err((kind, err)) = result {
        let _ = msg_tx.send(TimelineLoaderMsg::Failed { kind, error: err });
        egui_ctx.request_repaint();
    }
}

/// Fold accumulator for batching note refs.
struct FoldAcc {
    batch: Vec<NoteRef>,
    msg_tx: chan::Sender<TimelineLoaderMsg>,
    egui_ctx: egui::Context,
    kind: TimelineKind,
}

impl FoldAcc {
    fn push_note(&mut self, note: NoteRef) -> Result<(), String> {
        self.batch.push(note);
        if self.batch.len() >= FOLD_BATCH_SIZE {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), String> {
        if self.batch.is_empty() {
            return Ok(());
        }

        let notes = std::mem::take(&mut self.batch);
        self.msg_tx
            .send(TimelineLoaderMsg::TimelineBatch {
                kind: self.kind.clone(),
                notes,
            })
            .map_err(|_| "timeline loader channel closed".to_string())?;
        self.egui_ctx.request_repaint();
        Ok(())
    }
}

/// Run an initial timeline load and stream note ref batches.
fn load_timeline(
    egui_ctx: &egui::Context,
    ndb: &Ndb,
    msg_tx: &chan::Sender<TimelineLoaderMsg>,
    kind: TimelineKind,
) -> Result<(), (TimelineKind, String)> {
    let txn = Transaction::new(ndb).map_err(|e| (kind.clone(), e.to_string()))?;
    let filter_state = kind.filters(&txn, ndb);
    let FilterState::Ready(filters) = filter_state else {
        warn!(?kind, "timeline loader filter not ready");
        return Err((kind, "timeline filter not ready".to_string()));
    };

    let mut acc = FoldAcc {
        batch: Vec::with_capacity(FOLD_BATCH_SIZE),
        msg_tx: msg_tx.clone(),
        egui_ctx: egui_ctx.clone(),
        kind: kind.clone(),
    };

    let use_query = matches!(kind, TimelineKind::Algo(AlgoTimeline::LastPerPubkey(_)));

    for package in filters.local().packages {
        if package.filters.is_empty() {
            warn!(?kind, "timeline loader received empty filter package");
        }

        if use_query {
            let mut lim = 0i32;
            for filter in package.filters {
                lim += filter.limit().unwrap_or(1) as i32;
            }

            let cur_notes: Vec<NoteRef> = ndb
                .query(&txn, package.filters, lim)
                .map_err(|e| (kind.clone(), e.to_string()))?
                .into_iter()
                .map(NoteRef::from_query_result)
                .collect();
            for note_ref in cur_notes {
                if let Err(err) = acc.push_note(note_ref) {
                    tracing::error!("timeline loader push error: {err}");
                }
            }
            continue;
        }

        let fold_result = ndb.fold(&txn, package.filters, acc, |mut acc, note| {
            if let Some(key) = note.key() {
                let note_ref = NoteRef {
                    key,
                    created_at: note.created_at(),
                };
                if let Err(err) = acc.push_note(note_ref) {
                    tracing::error!("timeline loader flush error: {err}");
                }
            }
            acc
        });

        acc = fold_result.map_err(|e| (kind.clone(), e.to_string()))?;
    }

    acc.flush().map_err(|e| (kind.clone(), e))?;
    let _ = msg_tx.send(TimelineLoaderMsg::TimelineFinished { kind });
    egui_ctx.request_repaint();
    Ok(())
}
