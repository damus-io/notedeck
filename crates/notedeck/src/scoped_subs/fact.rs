use super::{config::ScopedSubKey, ScopedSubReadiness};

/// Bridge-to-UI scoped-sub read-model fact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ScopedSubFact {
    ReadinessChanged {
        scoped: ScopedSubKey,
        readiness: ScopedSubReadiness,
    },
}

/// Scoped-sub facts produced by one concrete runtime transition.
#[derive(Default)]
pub(crate) struct ScopedSubOutput {
    facts: Vec<ScopedSubFact>,
}

impl ScopedSubOutput {
    pub(crate) fn push(&mut self, fact: ScopedSubFact) {
        self.facts.push(fact);
    }

    pub(crate) fn extend(&mut self, other: Self) {
        self.facts.extend(other.facts);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.facts.is_empty()
    }

    pub(crate) fn into_facts(self) -> Vec<ScopedSubFact> {
        self.facts
    }
}
