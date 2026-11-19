//! A tool's reach to the relays: publish, a one-shot read, and a fetch it
//! can wait on.
//!
//! ```text
//! prepare (frame thread)                     job (worker thread)
//! ─────────────────────                      ───────────────────
//! cx.fetch(filters, bound)
//!   ├─ ndb.subscribe(filters)   ◀── 1st ──┐
//!   └─ oneshot REQ → frame batch           │ RelayFetch::wait(enough)
//!          │ (flushed when the tick ends)  │   └─ notes land in ndb, or the
//!          ▼                               │      bound passes; unsubscribe
//!       relays ── EVENTs ── ndb ingest ────┘
//! ```
//!
//! The local subscription comes first, so a note the relays answer with
//! can never land before anything is watching for it. Only the REQ rides
//! the frame's batch: the bridge still gets one input a frame, and the wait
//! happens later, on the job's thread, never on the frame.
//!
//! v1 waits for *notes* (or the bound), not for the relays' EOSE: the
//! bridge tracks EOSE only for scoped subscriptions, on the frame thread.
//! So a fetch that finds nothing costs its whole bound.

use std::time::Duration;

use nostrdb::{Filter, Ndb, Subscription, SubscriptionStream};

use crate::remote_data::RemoteIntentBatchBuilder;
use crate::{ExplicitPublishApi, OneshotApi};

/// The relay half of a [`ToolContext`](crate::ToolContext): the frame's
/// remote batch, which both its [`publisher`](Self::publisher) and its
/// [`oneshot`](Self::oneshot) reads write into. One field rather than two,
/// since both borrow the same batch.
pub struct ToolRemote<'a> {
    batch: &'a mut RemoteIntentBatchBuilder,
}

impl<'a> ToolRemote<'a> {
    /// A tool's reach over the frame's `batch`.
    pub(crate) fn new(batch: &'a mut RemoteIntentBatchBuilder) -> Self {
        Self { batch }
    }

    /// Publish to explicit relays, for a tool that writes: the path
    /// [`fan_out_event_frame`](crate::fan_out_event_frame) and
    /// [`NoteSender`](crate::NoteSender) send through.
    pub fn publisher(&mut self) -> ExplicitPublishApi<'_> {
        ExplicitPublishApi::new(self.batch)
    }

    /// Ask the selected account's read relays for something, without
    /// waiting for it: what [`ToolContext::fetch`](crate::ToolContext::fetch)
    /// sends, and what a tool that answers "fetching; try again" sends
    /// alone.
    pub fn oneshot(&mut self) -> OneshotApi<'_> {
        OneshotApi::new(self.batch)
    }

    /// The same reach for a shorter borrow, for a helper that takes one by
    /// value.
    pub fn reborrow(&mut self) -> ToolRemote<'_> {
        ToolRemote { batch: self.batch }
    }
}

/// A fetch sent with [`ToolContext::fetch`](crate::ToolContext::fetch):
/// owned and [`Send`], so a [`SlowTool`](crate::SlowTool)'s job carries it
/// to its thread and [`wait`](Self::wait)s there.
///
/// Dropped without a wait, it closes its local subscription.
pub struct RelayFetch {
    ndb: Ndb,
    /// The local subscription to the fetch's filters; `None` once a wait
    /// has taken it.
    sub: Option<Subscription>,
    bound: Duration,
}

/// How a [`RelayFetch`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchEnd {
    /// As many notes as were asked for landed in the db.
    Arrived,
    /// The bound passed first, with `seen` notes landed.
    TimedOut { seen: usize },
    /// The db closed the subscription (it is shutting down).
    Closed,
}

impl RelayFetch {
    /// A fetch watching `filters` on `ndb` for at most `bound`: subscribed
    /// here, before the caller sends the REQ.
    pub(crate) fn subscribe(
        ndb: &Ndb,
        filters: &[Filter],
        bound: Duration,
    ) -> Result<Self, String> {
        let sub = ndb
            .subscribe(filters)
            .map_err(|err| format!("watching the db for the fetch: {err}"))?;
        Ok(Self {
            ndb: ndb.clone(),
            sub: Some(sub),
            bound,
        })
    }

    /// Wait, blocking this thread, until `enough` notes matching the
    /// fetch's filters land in the db or its bound passes, then close the
    /// subscription. Call it only off the render loop: from a job.
    ///
    /// The wait runs on a runtime of its own, on a thread of its own, so it
    /// is safe on a thread already inside a runtime (a key binding running a
    /// job inline, a tokio worker), where blocking on another would panic.
    pub fn wait(mut self, enough: u32) -> FetchEnd {
        let Some(sub) = self.sub.take() else {
            return FetchEnd::Closed;
        };
        let ndb = self.ndb.clone();
        let bound = self.bound;
        let enough = enough.max(1);
        std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    let runtime = match tokio::runtime::Builder::new_current_thread()
                        .enable_time()
                        .build()
                    {
                        Ok(runtime) => runtime,
                        Err(err) => {
                            tracing::warn!("no runtime for a relay fetch's wait: {err}");
                            return FetchEnd::Closed;
                        }
                    };
                    // The stream unsubscribes when it drops, here.
                    let mut stream = SubscriptionStream::new(ndb, sub).notes_per_await(enough);
                    match runtime.block_on(stream.wait_for_notes(enough as usize, bound)) {
                        Ok(_) => FetchEnd::Arrived,
                        Err(nostrdb::Error::WaitTimeout { seen, .. }) => {
                            FetchEnd::TimedOut { seen }
                        }
                        Err(_) => FetchEnd::Closed,
                    }
                })
                .join()
                .unwrap_or(FetchEnd::Closed)
        })
    }
}

impl Drop for RelayFetch {
    /// Close the local subscription of a fetch nobody waited on (a job that
    /// never ran).
    fn drop(&mut self) {
        let Some(sub) = self.sub.take() else {
            return;
        };
        if let Err(err) = self.ndb.unsubscribe(sub) {
            tracing::debug!("closing an unwaited relay fetch: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_data::{
        RemoteBridgeInput, RemoteFetchCommand, RemoteIntent, RemoteOutboxReadModel,
    };
    use crate::test_util::TempNdb;
    use crate::{RemoteApi, ScopedSubsState};
    use nostrdb::{IngestMetadata, NoteBuilder};
    use nostrdb_net::{ClientMessage, FullKeypair};

    /// A signed kind-0 frame by `kp`, ready to ingest.
    fn profile_frame(kp: &FullKeypair) -> String {
        let note = NoteBuilder::new()
            .kind(0)
            .content(r#"{"name":"fetched"}"#)
            .sign(&kp.secret_key.secret_bytes())
            .build()
            .expect("note");
        ClientMessage::event(&note)
            .expect("event")
            .to_json()
            .expect("json")
    }

    /// The fetch's REQ rides the frame's batch as a one-shot read, and a
    /// note matching it that lands after the fetch was sent ends the wait.
    #[test]
    fn a_fetch_batches_its_req_and_a_landed_note_ends_the_wait() {
        let TempNdb { dir: _tmp, ndb } = TempNdb::new();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let read_model = RemoteOutboxReadModel::default();
        let mut scoped = ScopedSubsState::default();
        let kp = FullKeypair::generate();
        let filter = || {
            Filter::new()
                .authors([kp.pubkey.bytes()])
                .kinds([0])
                .build()
        };

        let fetch = {
            let mut remote = RemoteApi::new(sender, &read_model, &mut scoped);
            let mut tool_remote = remote.tool_remote();
            let fetch = RelayFetch::subscribe(&ndb, &[filter()], Duration::from_secs(10))
                .expect("subscribe");
            tool_remote.oneshot().oneshot(vec![filter()]);
            remote.flush();
            fetch
        };

        let RemoteBridgeInput::Ui(batch) = receiver.try_recv().expect("frame batch") else {
            panic!("expected the frame's batch");
        };
        assert!(matches!(
            batch.sections()[0].intents(),
            [RemoteIntent::Fetch(
                RemoteFetchCommand::SelectedAccountRead { .. }
            )]
        ));

        ndb.process_event_with(&profile_frame(&kp), IngestMetadata::new().client(true))
            .expect("ingest");
        assert_eq!(fetch.wait(1), FetchEnd::Arrived);
    }

    /// With nothing landing, the wait ends at its bound, having seen none.
    #[test]
    fn a_fetch_nothing_answers_ends_at_its_bound() {
        let TempNdb { dir: _tmp, ndb } = TempNdb::new();
        let kp = FullKeypair::generate();
        let filter = Filter::new()
            .authors([kp.pubkey.bytes()])
            .kinds([0])
            .build();
        let bound = Duration::from_millis(100);
        let fetch = RelayFetch::subscribe(&ndb, &[filter], bound).expect("subscribe");
        let started = std::time::Instant::now();
        assert_eq!(fetch.wait(1), FetchEnd::TimedOut { seen: 0 });
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// A fetch dropped unwaited closes its local subscription.
    #[test]
    fn an_unwaited_fetch_closes_its_subscription() {
        let TempNdb { dir: _tmp, ndb } = TempNdb::new();
        let before = ndb.subscription_count();
        let filter = Filter::new().kinds([0]).build();
        let fetch =
            RelayFetch::subscribe(&ndb, &[filter], Duration::from_secs(1)).expect("subscribe");
        assert_eq!(ndb.subscription_count(), before + 1);
        drop(fetch);
        assert_eq!(ndb.subscription_count(), before);
    }
}
