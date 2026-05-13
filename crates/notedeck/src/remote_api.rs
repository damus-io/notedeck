use crate::{
    Accounts, ExplicitPublishApi, OneshotApi, Outbox, PublishApi, ScopedSubApi, ScopedSubsState,
};
use enostr::{NormRelayUrl, Pubkey, RelayStatus};

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
            .websocket_statuses()
            .into_iter()
            .map(|(url, status)| RelayInspectEntry {
                relay_url: url,
                status,
            })
            .collect()
    }
}

/// Unowned remote API over exactly one staged outbox session.
///
/// This is the only mutating relay/outbox facade exposed to app code. It owns
/// one staged [`crate::Outbox`] handler and borrows the scoped subscription
/// state needed to interpret durable logical subscriptions.
pub struct RemoteApi<'a> {
    outbox: Outbox<'a>,
    scoped_sub_state: &'a mut ScopedSubsState,
}

impl<'a> RemoteApi<'a> {
    /// Construct the host-facing remote facade over one outbox session and
    /// scoped-subscription state bundle.
    pub(crate) fn new(outbox: Outbox<'a>, scoped_sub_state: &'a mut ScopedSubsState) -> Self {
        Self {
            outbox,
            scoped_sub_state,
        }
    }

    /// Access scoped subscription APIs bound to the selected account.
    pub fn scoped_subs<'o>(&'o mut self, accounts: &'o Accounts) -> ScopedSubApi<'o, 'a> {
        let (outbox, scoped_sub_state) = self.split_mut();
        scoped_sub_state.api(outbox, accounts)
    }

    /// Access one-shot read APIs bound to the selected account.
    pub fn oneshot<'o>(&'o mut self, accounts: &'o Accounts) -> OneshotApi<'o, 'a> {
        OneshotApi::new(self.outbox_mut(), accounts)
    }

    /// Access publishing APIs bound to the selected account.
    pub fn publisher<'o>(&'o mut self, accounts: &'o Accounts) -> PublishApi<'o, 'a> {
        PublishApi::new(self.outbox_mut(), accounts)
    }

    /// Access explicit-relay publishing APIs with no account dependency.
    pub fn publisher_explicit<'o>(&'o mut self) -> ExplicitPublishApi<'o, 'a> {
        ExplicitPublishApi::new(self.outbox_mut())
    }

    /// Access read-only relay inspection data for UI rendering.
    pub fn relay_inspect(&self) -> RelayInspectApi<'_, 'a> {
        RelayInspectApi::new(self.outbox_ref())
    }

    /// Host account-switch transition hook for scoped subscription teardown and restore.
    pub(crate) fn on_account_switched(
        &mut self,
        old_account: Pubkey,
        new_account: Pubkey,
        accounts: &Accounts,
    ) {
        let (outbox, scoped_sub_state) = self.split_mut();
        scoped_sub_state.runtime_mut().on_account_switched(
            outbox,
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
        let (outbox, scoped_sub_state) = self.split_mut();
        scoped_sub_state
            .runtime_mut()
            .retarget_selected_account_read_relays(outbox, accounts);
    }

    fn outbox_mut(&mut self) -> &mut Outbox<'a> {
        &mut self.outbox
    }

    fn outbox_ref(&self) -> &Outbox<'a> {
        &self.outbox
    }

    fn split_mut(&mut self) -> (&mut Outbox<'a>, &mut ScopedSubsState) {
        let outbox = &mut self.outbox;
        let scoped_sub_state = &mut *self.scoped_sub_state;
        (outbox, scoped_sub_state)
    }
}
