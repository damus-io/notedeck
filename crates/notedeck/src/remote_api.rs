use egui::Context;
use enostr::{NormRelayUrl, OutboxSession, Pubkey, RelayImplType, RelayStatus};
use nostrdb::Ndb;

use crate::{
    Accounts, ExplicitPublishApi, OneshotApi, Outbox, PublishApi, ScopedSubApi, ScopedSubsState,
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
/// does not provide subscription/publish/oneshot methods.
pub struct RelayInspectApi<'r, 'a> {
    pool: &'r Outbox<'a>,
}

impl<'r, 'a> RelayInspectApi<'r, 'a> {
    pub(crate) fn new(pool: &'r Outbox<'a>) -> Self {
        Self { pool }
    }

    /// Snapshot websocket relay statuses for display/debug UI.
    pub fn relay_infos(&self) -> Vec<RelayInspectEntry<'_>> {
        self.pool
            .outbox
            .websocket_statuses()
            .into_iter()
            .map(|(url, status)| RelayInspectEntry {
                relay_url: url,
                status,
            })
            .collect()
    }
}

/// App-facing facade for relay/outbox transport operations.
///
/// This is the only app-visible entrypoint for scoped subscriptions, one-shot
/// requests, publishing, relay event ingestion, and relay status inspection.
/// Apps should not access raw `Outbox` directly.
pub struct RemoteApi<'a> {
    pool: Outbox<'a>,
    scoped_sub_state: &'a mut ScopedSubsState,
}

impl<'a> RemoteApi<'a> {
    pub(crate) fn new(pool: Outbox<'a>, scoped_sub_state: &'a mut ScopedSubsState) -> Self {
        Self {
            pool,
            scoped_sub_state,
        }
    }

    /// Export the staged outbox session without exposing the raw handler.
    ///
    /// This is only needed during host initialization before the first frame.
    pub(crate) fn export_session(self) -> OutboxSession {
        self.pool.export()
    }

    /// Access scoped subscription APIs bound to the selected account.
    pub fn scoped_subs<'o>(&'o mut self, accounts: &'o Accounts) -> ScopedSubApi<'o, 'a> {
        self.scoped_sub_state.api(&mut self.pool, accounts)
    }

    /// Access one-shot read APIs bound to the selected account.
    pub fn oneshot<'o>(&'o mut self, accounts: &'o Accounts) -> OneshotApi<'o, 'a> {
        OneshotApi::new(&mut self.pool, accounts)
    }

    /// Access publishing APIs bound to the selected account.
    pub fn publisher<'o>(&'o mut self, accounts: &'o Accounts) -> PublishApi<'o, 'a> {
        PublishApi::new(&mut self.pool, accounts)
    }

    /// Access explicit-relay publishing APIs (no account dependency).
    pub fn publisher_explicit<'o>(&'o mut self) -> ExplicitPublishApi<'o, 'a> {
        ExplicitPublishApi::new(&mut self.pool)
    }

    /// Host-only relay ingestion + keepalive maintenance.
    pub(crate) fn process_events(&mut self, ctx: &Context, ndb: &Ndb) {
        try_process_events(ctx, &mut self.pool, ndb);
    }

    /// Access read-only relay inspection data for UI rendering.
    pub fn relay_inspect(&self) -> RelayInspectApi<'_, 'a> {
        RelayInspectApi::new(&self.pool)
    }

    /// Host account-switch transition hook for scoped subscription teardown/restore.
    pub(crate) fn on_account_switched(
        &mut self,
        old_account: Pubkey,
        new_account: Pubkey,
        accounts: &Accounts,
    ) {
        self.scoped_sub_state.runtime_mut().on_account_switched(
            &mut self.pool,
            old_account,
            new_account,
            accounts,
        );
    }

    /// Host/account hook to retarget selected-account-read scoped subscriptions.
    ///
    /// This retargets all live scoped subscriptions that resolve relays from
    /// [`crate::RelaySelection::AccountsRead`] without requiring callers to
    /// individually `set_sub(...)` every declaration.
    pub(crate) fn retarget_selected_account_read_relays(&mut self, accounts: &Accounts) {
        self.scoped_sub_state
            .runtime_mut()
            .retarget_selected_account_read_relays(&mut self.pool, accounts);
    }
}

#[profiling::function]
pub fn try_process_events(ctx: &Context, pool: &mut Outbox, ndb: &Ndb) {
    let ctx2 = ctx.clone();
    let wakeup = move || {
        ctx2.request_repaint();
    };

    pool.outbox.keepalive_ping(wakeup);

    pool.outbox.try_recv(10, |ev| {
        let from_client = match ev.relay_type {
            RelayImplType::Websocket => false,
            enostr::RelayImplType::Multicast => true,
        };

        {
            profiling::scope!("ndb process event");
            if let Err(err) = ndb.process_event_with(
                ev.event_json,
                nostrdb::IngestMetadata::new()
                    .client(from_client)
                    .relay(ev.url),
            ) {
                tracing::error!("error processing event {}: {err}", ev.event_json);
            }
        }
    });
}
