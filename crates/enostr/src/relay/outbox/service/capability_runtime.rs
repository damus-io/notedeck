use std::future::Future;

use tokio::sync::mpsc;

use crate::relay::{
    FullHistoryLocalPresenceRequest, FullHistoryLocalPresenceResult, FullHistoryLocalSetRequest,
    FullHistoryPendingIngestionPresenceRequest, FullHistoryPendingIngestionPresenceResult,
};

use super::{
    CapabilityResult, EventIngestCapability, EventIngestRequest, FullHistoryCapability,
    FullHistoryLocalSetResult,
};

/// Executes host-provided async capabilities and routes completed work back to the service loop.
pub(super) struct CapabilityRuntime<F, E> {
    full_history: F,
    event_ingest: E,
    results_tx: mpsc::UnboundedSender<CapabilityResult>,
    results_rx: mpsc::UnboundedReceiver<CapabilityResult>,
}

impl<F, E> CapabilityRuntime<F, E>
where
    F: FullHistoryCapability<
        LocalSetOutput = FullHistoryLocalSetResult,
        LocalPresenceOutput = FullHistoryLocalPresenceResult,
        PendingIngestionPresenceOutput = FullHistoryPendingIngestionPresenceResult,
    >,
    E: EventIngestCapability,
{
    pub(super) fn new(full_history: F, event_ingest: E) -> Self {
        let (results_tx, results_rx) = mpsc::unbounded_channel();
        Self {
            full_history,
            event_ingest,
            results_tx,
            results_rx,
        }
    }

    pub(super) async fn recv(&mut self) -> Option<CapabilityResult> {
        self.results_rx.recv().await
    }

    pub(super) fn start_full_history_local_set(&mut self, request: FullHistoryLocalSetRequest) {
        let future = self.full_history.build_local_set(request);
        self.start_future(future, CapabilityResult::FullHistoryLocalSet);
    }

    pub(super) fn start_full_history_local_presence(
        &mut self,
        request: FullHistoryLocalPresenceRequest,
    ) {
        let future = self.full_history.check_local_presence(request);
        self.start_future(future, CapabilityResult::FullHistoryLocalPresence);
    }

    pub(super) fn start_full_history_pending_ingestion_presence(
        &mut self,
        request: FullHistoryPendingIngestionPresenceRequest,
    ) {
        let future = self.full_history.check_pending_ingestion_presence(request);
        self.start_future(
            future,
            CapabilityResult::FullHistoryPendingIngestionPresence,
        );
    }

    pub(super) fn start_event_ingest(&mut self, request: EventIngestRequest) {
        let future = self.event_ingest.ingest_event(request);
        self.start_future(future, |()| CapabilityResult::EventIngest);
    }

    fn start_future<Fut, Map>(&mut self, future: Fut, map: Map)
    where
        Fut: Future + Send + 'static,
        Fut::Output: Send + 'static,
        Map: FnOnce(Fut::Output) -> CapabilityResult + Send + 'static,
    {
        let tx = self.results_tx.clone();
        tokio::spawn(async move {
            let result = map(future.await);
            let _ = tx.send(result);
        });
    }
}
