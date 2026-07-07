use enostr::{
    full_history_targets_have_work, FullHistorySubId, FullHistoryTarget, OutboxIdRegistry,
    OutboxSubId, RelayUrlPkgs,
};
use nostrdb::Filter;

use super::author_plan::AuthorOutboxPlanJobRequest;
use super::fact::ScopedSubOutput;

/// Scoped-sub output from one runtime transition.
#[derive(Default)]
pub(crate) struct ScopedSubDelta {
    outbox_ops: ScopedSubOutboxOps,
    output: ScopedSubOutput,
    effects: ScopedSubEffects,
}

impl ScopedSubDelta {
    pub(crate) fn new(output: ScopedSubOutput, outbox_ops: ScopedSubOutboxOps) -> Self {
        Self {
            outbox_ops,
            output,
            effects: ScopedSubEffects::default(),
        }
    }

    pub(crate) fn new_with_effects(
        output: ScopedSubOutput,
        outbox_ops: ScopedSubOutboxOps,
        effects: ScopedSubEffects,
    ) -> Self {
        Self {
            outbox_ops,
            output,
            effects,
        }
    }

    pub(crate) fn extend(&mut self, other: Self) {
        self.outbox_ops.extend(other.outbox_ops);
        self.output.extend(other.output);
        self.effects.extend(other.effects);
    }

    pub(crate) fn into_parts(self) -> (ScopedSubOutput, ScopedSubOutboxOps, ScopedSubEffects) {
        (self.output, self.outbox_ops, self.effects)
    }
}

/// Non-outbox effects returned by scoped-sub runtime transitions.
///
/// These effects are executed by bridge-owned runners. Scoped-sub runtime code
/// may request the work, but must not own the executor, callback, or receiver.
#[derive(Default)]
pub(crate) struct ScopedSubEffects {
    effects: Vec<ScopedSubEffect>,
}

impl ScopedSubEffects {
    pub(super) fn push(&mut self, effect: ScopedSubEffect) {
        self.effects.push(effect);
    }

    pub(crate) fn extend(&mut self, other: Self) {
        self.effects.extend(other.effects);
    }

    pub(crate) fn into_effects(self) -> Vec<ScopedSubEffect> {
        self.effects
    }
}

/// One non-outbox effect requested by scoped-sub runtime code.
pub(crate) enum ScopedSubEffect {
    StartAuthorOutboxPlanJob(AuthorOutboxPlanJobRequest),
}

impl From<AuthorOutboxPlanJobRequest> for ScopedSubEffect {
    fn from(request: AuthorOutboxPlanJobRequest) -> Self {
        Self::StartAuthorOutboxPlanJob(request)
    }
}

/// Concrete outbox operations returned by one scoped-sub runtime transition.
///
/// Bridge-facing scoped-sub APIs return this value. Lower planning helpers may
/// append to a locally owned instance while constructing that return value; the
/// bridge must not provide an op accumulator.
#[derive(Default)]
pub(crate) struct ScopedSubOutboxOps {
    ops: Vec<ScopedSubOutboxOp>,
}

impl ScopedSubOutboxOps {
    pub(crate) fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub(crate) fn extend(&mut self, other: Self) {
        self.ops.extend(other.ops);
    }

    pub(super) fn try_subscribe(
        &mut self,
        ids: &OutboxIdRegistry,
        filters: Vec<Filter>,
        urls: RelayUrlPkgs,
    ) -> Option<OutboxSubId> {
        if filters.iter().all(|filter| filter.num_elements() == 0) {
            return None;
        }

        let id = ids.next_sub_id();
        self.ops.push(ScopedSubOutboxOp::SetLive {
            id,
            filters,
            relay_pkgs: urls,
        });
        Some(id)
    }

    pub(super) fn set_live(
        &mut self,
        id: OutboxSubId,
        filters: Vec<Filter>,
        relay_pkgs: RelayUrlPkgs,
    ) {
        self.ops.push(ScopedSubOutboxOp::SetLive {
            id,
            filters,
            relay_pkgs,
        });
    }

    pub(super) fn start_fetch(
        &mut self,
        id: OutboxSubId,
        filters: Vec<Filter>,
        relay_pkgs: RelayUrlPkgs,
    ) {
        self.ops.push(ScopedSubOutboxOp::StartFetch {
            id,
            filters,
            relay_pkgs,
        });
    }

    pub(super) fn try_subscribe_full_history_targets(
        &mut self,
        ids: &OutboxIdRegistry,
        targets: Vec<FullHistoryTarget>,
    ) -> Option<FullHistorySubId> {
        let id = ids.next_full_history_id();
        if !full_history_targets_are_accepted(&targets) {
            return None;
        }

        self.ops
            .push(ScopedSubOutboxOp::SetFullHistoryTargets { id, targets });
        Some(id)
    }

    pub(super) fn modify_full_history_targets(
        &mut self,
        id: FullHistorySubId,
        targets: Vec<FullHistoryTarget>,
    ) -> bool {
        if !full_history_targets_are_accepted(&targets) {
            self.remove_full_history(id);
            return false;
        }

        self.ops
            .push(ScopedSubOutboxOp::SetFullHistoryTargets { id, targets });
        true
    }

    pub(super) fn unsubscribe(&mut self, id: OutboxSubId) {
        self.ops.push(ScopedSubOutboxOp::UnsubscribeLive { id });
    }

    pub(super) fn clear_fetch(&mut self, id: OutboxSubId) {
        self.ops.push(ScopedSubOutboxOp::ClearFetch { id });
    }

    pub(super) fn remove_full_history(&mut self, id: FullHistorySubId) {
        self.ops.push(ScopedSubOutboxOp::RemoveFullHistory { id });
    }

    pub(crate) fn into_ops(self) -> Vec<ScopedSubOutboxOp> {
        self.ops
    }
}

/// One concrete outbox mutation emitted by scoped-sub runtime code.
pub(crate) enum ScopedSubOutboxOp {
    SetLive {
        id: OutboxSubId,
        filters: Vec<Filter>,
        relay_pkgs: RelayUrlPkgs,
    },
    StartFetch {
        id: OutboxSubId,
        filters: Vec<Filter>,
        relay_pkgs: RelayUrlPkgs,
    },
    UnsubscribeLive {
        id: OutboxSubId,
    },
    ClearFetch {
        id: OutboxSubId,
    },
    SetFullHistoryTargets {
        id: FullHistorySubId,
        targets: Vec<FullHistoryTarget>,
    },
    RemoveFullHistory {
        id: FullHistorySubId,
    },
}

fn full_history_targets_are_accepted(targets: &[FullHistoryTarget]) -> bool {
    full_history_targets_have_work(targets)
}
