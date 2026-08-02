#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RouteWorkResult {
    Complete,
    /// Author-outbox planning is waiting for a concrete plan-ready transition.
    PlanPending,
    FullRefreshRequired,
    RebuildRequired,
}
