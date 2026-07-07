mod negentropy;
mod outbox_read_model;
mod remote_bridge;

#[cfg(test)]
mod tests;

use self::remote_bridge::{RemoteBridgeEvent, RemoteBridgeHandle};
use crate::jobs::JobSpawner;
use crate::{RemoteApi, ScopedSubsState};
use nostrdb::Ndb;

pub(crate) use outbox_read_model::RemoteOutboxReadModel;
pub(crate) use remote_bridge::{
    BridgeAccountState, RemoteAdvertisedFetchCoverage, RemoteBridgeConfig, RemoteBridgeInput,
    RemoteFetchCommand, RemoteIntent, RemoteIntentBatchBuilder, RemotePublishCommand,
};

/// Owned host backing state for remote transport plus durable remote intent.
///
/// The bridge owns remote policy and drives an enostr-owned outbox service. This
/// main-thread state owns durable scoped-sub intent and committed read-model
/// facts emitted by the bridge.
pub(crate) struct RemoteState {
    bridge: RemoteBridgeHandle,
    outbox_read_model: RemoteOutboxReadModel,
    scoped_sub_state: ScopedSubsState,
}

impl RemoteState {
    /// Build owned remote backing state with explicit bridge configuration.
    pub(crate) fn new_with_config(
        ndb: &Ndb,
        job_spawner: JobSpawner,
        wake_host: impl Fn() + Send + Sync + 'static,
        config: RemoteBridgeConfig,
    ) -> Self {
        let bridge = RemoteBridgeHandle::spawn(ndb.clone(), job_spawner, wake_host, config);
        let scoped_sub_state = ScopedSubsState::default();

        Self {
            bridge,
            outbox_read_model: RemoteOutboxReadModel::default(),
            scoped_sub_state,
        }
    }

    /// Override the maximum number of live websocket connections.
    pub(crate) fn set_max_websocket_connections(&mut self, max_connections: Option<usize>) {
        self.bridge
            .send(RemoteBridgeInput::SetMaxWebsocketConnections(
                max_connections,
            ));
    }

    /// Drain bridge events into the host-side snapshot.
    #[profiling::function]
    pub(crate) fn poll_bridge(&mut self) {
        self.bridge.drain_events(|event| match event {
            RemoteBridgeEvent::Outbox(event) => self.outbox_read_model.apply_event(event),
            RemoteBridgeEvent::ScopedSub(fact) => self.scoped_sub_state.apply_bridge_fact(fact),
        });
    }

    /// Open one handler-backed remote API over the owned remote backing state.
    pub(crate) fn api(&mut self) -> RemoteApi<'_> {
        RemoteApi::new(
            self.bridge.input_sender(),
            &self.outbox_read_model,
            &mut self.scoped_sub_state,
        )
    }
}
