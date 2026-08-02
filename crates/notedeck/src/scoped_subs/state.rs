use super::{
    config::ScopedSubKey, declarations::ScopedSubDeclarations, ScopedSubApi, ScopedSubFact,
    ScopedSubReadiness,
};
use crate::{remote_data::RemoteIntentBatchBuilder, Accounts};
use hashbrown::HashMap;

/// Host-owned scoped subscription declarations and committed bridge facts.
///
/// The UI thread owns only synchronous declaration/cache state. Remote
/// realization lives in the bridge-owned scoped-sub runtime.
#[derive(Default)]
pub struct ScopedSubsState {
    declarations: ScopedSubDeclarations,
    read_model: ScopedSubReadModel,
}

impl ScopedSubsState {
    /// Build the app-facing scoped subscription API.
    pub(crate) fn api<'o>(
        &'o mut self,
        accounts: &'o Accounts,
        batch: &'o mut RemoteIntentBatchBuilder,
    ) -> ScopedSubApi<'o> {
        ScopedSubApi::new(accounts, &mut self.declarations, &self.read_model, batch)
    }

    pub(crate) fn apply_bridge_fact(&mut self, fact: ScopedSubFact) {
        self.read_model.apply_fact(fact);
    }
}

#[derive(Default)]
pub(super) struct ScopedSubReadModel {
    readiness: HashMap<ScopedSubKey, ScopedSubReadiness>,
}

impl ScopedSubReadModel {
    fn apply_fact(&mut self, fact: ScopedSubFact) {
        match fact {
            ScopedSubFact::ReadinessChanged { scoped, readiness } => {
                if readiness == ScopedSubReadiness::Missing {
                    self.readiness.remove(&scoped);
                    return;
                }

                self.readiness.insert(scoped, readiness);
            }
        }
    }

    pub(super) fn readiness(&self, scoped: &ScopedSubKey) -> Option<ScopedSubReadiness> {
        self.readiness.get(scoped).copied()
    }
}
