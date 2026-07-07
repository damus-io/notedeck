use crate::{Accounts, ExplicitPublishApi, OneshotApi, PublishApi, ScopedSubApi, ScopedSubsState};
use enostr::{NormRelayUrl, RelayStatus};
use tokio::sync::mpsc;

use crate::remote_data::{
    BridgeAccountState, RemoteBridgeInput, RemoteIntentBatchBuilder, RemoteOutboxReadModel,
};

/// Read-only relay inspection row for relay UI surfaces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelayInspectEntry<'a> {
    pub relay_url: &'a NormRelayUrl,
    pub status: RelayStatus,
}

/// Read-only relay inspection facade.
///
/// This exposes only relay status inspection needed by UI code and intentionally
/// does not provide subscription, publish, or one-shot methods.
pub struct RelayInspectApi<'a> {
    read_model: &'a RemoteOutboxReadModel,
}

impl<'a> RelayInspectApi<'a> {
    pub(crate) fn new(read_model: &'a RemoteOutboxReadModel) -> Self {
        Self { read_model }
    }

    /// Iterate active websocket relay statuses for display UI.
    pub fn relay_infos(&self) -> impl Iterator<Item = RelayInspectEntry<'_>> + '_ {
        self.read_model
            .websocket_statuses()
            .filter(|(_, status)| {
                matches!(status, RelayStatus::Connected | RelayStatus::Connecting)
            })
            .map(|(relay_url, status)| RelayInspectEntry { relay_url, status })
    }

    /// Iterate all known websocket relay statuses for stable inventory UI.
    pub fn known_relay_infos(&self) -> impl Iterator<Item = RelayInspectEntry<'_>> + '_ {
        self.read_model
            .websocket_statuses()
            .map(|(relay_url, status)| RelayInspectEntry { relay_url, status })
    }
}

/// Unowned remote API over bridge commands and committed read-model state.
///
/// This is the only mutating relay facade exposed to app code. Mutations cross
/// the bridge as typed commands; local methods only read committed facts.
pub struct RemoteApi<'a> {
    inputs: mpsc::UnboundedSender<RemoteBridgeInput>,
    read_model: &'a RemoteOutboxReadModel,
    scoped_sub_state: &'a mut ScopedSubsState,
    batch: RemoteIntentBatchBuilder,
}

impl<'a> RemoteApi<'a> {
    /// Construct the host-facing remote facade over bridge command/read state.
    pub(crate) fn new(
        inputs: mpsc::UnboundedSender<RemoteBridgeInput>,
        read_model: &'a RemoteOutboxReadModel,
        scoped_sub_state: &'a mut ScopedSubsState,
    ) -> Self {
        Self {
            inputs,
            read_model,
            scoped_sub_state,
            batch: RemoteIntentBatchBuilder::new(),
        }
    }

    /// Access scoped subscription APIs bound to the selected account.
    pub fn scoped_subs<'o>(&'o mut self, accounts: &'o Accounts) -> ScopedSubApi<'o> {
        self.scoped_sub_state.api(accounts, &mut self.batch)
    }

    /// Access one-shot read APIs bound to the selected account.
    pub fn oneshot<'o>(&'o mut self) -> OneshotApi<'o> {
        OneshotApi::new(&mut self.batch)
    }

    /// Access publishing APIs bound to the selected account.
    pub fn publisher<'o>(&'o mut self) -> PublishApi<'o> {
        PublishApi::new(&mut self.batch)
    }

    /// Access explicit-relay publishing APIs with no account dependency.
    pub fn publisher_explicit<'o>(&'o mut self) -> ExplicitPublishApi<'o> {
        ExplicitPublishApi::new(&mut self.batch)
    }

    /// Send the accumulated frame batch to the bridge.
    pub fn flush(&mut self) {
        let Some(batch) = self.batch.take() else {
            return;
        };
        if let Err(err) = self.inputs.send(RemoteBridgeInput::Ui(batch)) {
            tracing::warn!("failed to send remote intent batch to bridge: {err}");
        }
    }

    /// Access read-only relay inspection data for UI rendering.
    pub fn relay_inspect(&self) -> RelayInspectApi<'_> {
        RelayInspectApi::new(self.read_model)
    }

    /// Override the maximum number of live websocket connections.
    pub fn set_max_websocket_connections(&mut self, max_connections: Option<usize>) {
        if let Err(err) = self
            .inputs
            .send(RemoteBridgeInput::SetMaxWebsocketConnections(
                max_connections,
            ))
        {
            tracing::warn!("failed to send websocket connection limit to bridge: {err}");
        }
    }

    /// Host account-switch transition hook for scoped subscription teardown and restore.
    pub(crate) fn on_account_switched(&mut self, accounts: &Accounts) {
        self.on_selected_account_changed(accounts);
    }

    /// Host/account hook for selected-account remote state changes.
    pub(crate) fn on_selected_account_changed(&mut self, accounts: &Accounts) {
        self.batch
            .set_account_changed(Self::bridge_account_state(accounts));
    }

    fn bridge_account_state(accounts: &Accounts) -> BridgeAccountState {
        BridgeAccountState::new(
            *accounts.selected_account_pubkey(),
            accounts.selected_account_read_relays(),
            accounts.selected_account_write_relays(),
        )
    }
}

impl Drop for RemoteApi<'_> {
    fn drop(&mut self) {
        if self.batch.is_empty() || std::thread::panicking() {
            return;
        }

        panic!("RemoteApi dropped with unflushed remote intents");
    }
}
