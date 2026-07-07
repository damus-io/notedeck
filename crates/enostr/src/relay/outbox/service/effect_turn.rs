use std::collections::VecDeque;

use hashbrown::HashMap;

use crate::relay::outbox::RelayDemandChanged;
use crate::relay::{NormRelayUrl, OutboxSubId};

use super::{OutboxPoolFact, OutboxPoolOutput, OutboxServiceOutput};

/// Owns service output staging for ready events and active effect turns.
#[derive(Default)]
pub(super) struct OutboxServiceOutputs {
    active_effect_turn: Option<OutboxEffectAccumulator>,
    pub(super) ready_outputs: VecDeque<OutboxServiceOutput>,
}

impl OutboxServiceOutputs {
    pub(super) fn begin_effect_turn(&mut self) {
        debug_assert!(
            self.active_effect_turn.is_none(),
            "outbox effect turns must not be nested"
        );
        self.active_effect_turn = Some(OutboxEffectAccumulator::default());
    }

    pub(super) fn end_effect_turn(&mut self) -> Option<OutboxPoolOutput> {
        self.active_effect_turn
            .take()
            .map(OutboxEffectAccumulator::finish)
    }

    pub(super) fn handle_pool_output(
        &mut self,
        output: OutboxPoolOutput,
    ) -> Option<OutboxPoolOutput> {
        if let Some(turn) = self.active_effect_turn.as_mut() {
            turn.record(output);
            return None;
        }
        Some(output)
    }

    pub(super) fn pop_ready(&mut self) -> Option<OutboxServiceOutput> {
        self.ready_outputs.pop_front()
    }
}

/// Accumulates unactivated pool output for one service effect turn.
#[derive(Default)]
pub(super) struct OutboxEffectAccumulator {
    output: OutboxPoolOutput,
}

impl OutboxEffectAccumulator {
    pub(super) fn record(&mut self, output: OutboxPoolOutput) {
        self.output.facts.extend(output.facts);
        self.output
            .relay_demand_changes
            .extend(output.relay_demand_changes);
        self.output
            .transport_effects
            .extend(output.transport_effects);
        self.output
            .full_history_effects
            .extend(output.full_history_effects);
    }

    pub(super) fn finish(mut self) -> OutboxPoolOutput {
        self.output.facts = reduce_facts(self.output.facts);
        self.output.relay_demand_changes =
            reduce_relay_demand_changes(self.output.relay_demand_changes);
        self.output
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum OutboxPoolFactKey {
    RelayReqStatus(OutboxSubId, NormRelayUrl),
    OutboxSubRelayEose(OutboxSubId),
}

impl From<&OutboxPoolFact> for OutboxPoolFactKey {
    fn from(fact: &OutboxPoolFact) -> Self {
        match fact {
            OutboxPoolFact::RelayReqStatus { id, relay, .. } => {
                Self::RelayReqStatus(*id, relay.clone())
            }
            OutboxPoolFact::OutboxSubRelayEose { id, .. } => Self::OutboxSubRelayEose(*id),
        }
    }
}

fn reduce_facts(facts: Vec<OutboxPoolFact>) -> Vec<OutboxPoolFact> {
    let mut reduced = Vec::new();
    let mut indexes = HashMap::<OutboxPoolFactKey, usize>::new();

    for fact in facts {
        let key = OutboxPoolFactKey::from(&fact);
        if let Some(index) = indexes.get(&key).copied() {
            reduced[index] = fact;
            continue;
        }

        indexes.insert(key, reduced.len());
        reduced.push(fact);
    }

    reduced
}

fn reduce_relay_demand_changes(changes: Vec<RelayDemandChanged>) -> Vec<RelayDemandChanged> {
    let mut reduced = Vec::new();
    let mut indexes = HashMap::<NormRelayUrl, usize>::new();

    for change in changes {
        if let Some(index) = indexes.get(&change.relay).copied() {
            reduced[index] = change;
            continue;
        }

        indexes.insert(change.relay.clone(), reduced.len());
        reduced.push(change);
    }

    reduced
}
