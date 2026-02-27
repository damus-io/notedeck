use crate::scoped_sub_owners::ScopedSubOwners;
use crate::scoped_subs::ScopedSubRuntime;
use crate::{Accounts, Outbox, ScopedSubApi};

/// Host-owned scoped subscription state.
///
/// This keeps scoped owner slots and runtime state together so they are
/// managed as one unit by the host.
#[derive(Default)]
pub struct ScopedSubsState {
    runtime: ScopedSubRuntime,
    owners: ScopedSubOwners,
}

impl ScopedSubsState {
    /// Borrow owner/runtime internals for legacy callsites that still expect
    /// both references separately.
    pub(crate) fn split_mut(&mut self) -> (&mut ScopedSubOwners, &mut ScopedSubRuntime) {
        (&mut self.owners, &mut self.runtime)
    }

    /// Build the app-facing scoped subscription API bound to host resources.
    pub fn api<'o, 'a>(
        &'o mut self,
        pool: &'o mut Outbox<'a>,
        accounts: &'o Accounts,
    ) -> ScopedSubApi<'o, 'a> {
        let (owners, runtime) = self.split_mut();
        ScopedSubApi::new(pool, accounts, owners, runtime)
    }

    /// Mutable access to runtime internals for host account-switch integration.
    pub(crate) fn runtime_mut(&mut self) -> &mut ScopedSubRuntime {
        &mut self.runtime
    }
}
