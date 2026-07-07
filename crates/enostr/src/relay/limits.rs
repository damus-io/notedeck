use nostrdb::Filter;

use crate::relay::RelayReqId;

/// Local outbound REQ filter-count compatibility guard.
pub(crate) const MAX_FILTERS_PER_REQ: usize = 200;

#[derive(Clone)]
pub(crate) struct IndexedFilter {
    pub(crate) source_index: usize,
    pub(crate) filter: Filter,
    pub(crate) json_size: usize,
}

/// Limitations imposed by the relay
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayLimitations {
    /// Corresponds to NIP-11 `max_subscriptions`.
    pub maximum_subs: usize,

    /// Corresponds to NIP-11 `max_message_length`.
    pub max_json_bytes: usize,
}

/// Local hard caps for untrusted NIP-11 relay limitations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RelayLimitCaps {
    /// Highest NIP-11 `max_subscriptions` value accepted as local capacity.
    pub(super) maximum_subs: usize,

    /// Highest NIP-11 `max_message_length` accepted for outbound sizing.
    pub(super) max_json_bytes: usize,
}

impl RelayLimitCaps {
    /// Default cap for NIP-11 `max_subscriptions`.
    pub(super) const DEFAULT_MAXIMUM_SUBS: usize = 1024;

    /// Default cap for NIP-11 `max_message_length`.
    pub(super) const DEFAULT_MAX_JSON_BYTES: usize = 8 * 1024 * 1024;

    /// Applies these caps to effective relay limitations.
    pub(super) fn clamp(self, limitations: RelayLimitations) -> RelayLimitations {
        RelayLimitations {
            maximum_subs: limitations.maximum_subs.min(self.maximum_subs),
            max_json_bytes: limitations.max_json_bytes.min(self.max_json_bytes),
        }
    }
}

impl Default for RelayLimitCaps {
    fn default() -> Self {
        Self {
            maximum_subs: Self::DEFAULT_MAXIMUM_SUBS,
            max_json_bytes: Self::DEFAULT_MAX_JSON_BYTES,
        }
    }
}

impl Default for RelayLimitations {
    fn default() -> Self {
        Self {
            maximum_subs: 200,
            max_json_bytes: 131_072,
        }
    }
}

pub struct RelayCoordinatorLimits {
    maximum_subs: usize,
    pub(in crate::relay) sub_guardian: SubPassGuardian,
    pub max_json_bytes: usize,
}

/// Per-REQ relay limits used when placing and constructing outbound REQs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReqFilterLimits {
    pub max_filters_per_req: usize,
    pub max_json_bytes: usize,
}

impl ReqFilterLimits {
    pub fn new(max_filters_per_req: usize, max_json_bytes: usize) -> Self {
        Self {
            max_filters_per_req: max_filters_per_req.max(1),
            max_json_bytes,
        }
    }

    pub fn from_relay_limits(limits: &RelayCoordinatorLimits) -> Self {
        Self::new(MAX_FILTERS_PER_REQ, limits.max_json_bytes)
    }

    /// Returns whether `filters` can be represented by one relay REQ.
    pub fn filters_fit_single_req(&self, filters: &[Filter]) -> Option<bool> {
        let filter_json_size = filters.iter().try_fold(0usize, |sum, filter| {
            Some(sum.saturating_add(Self::filter_json_size(filter)?))
        })?;

        Some(self.can_fit(0, filters.len(), 0, filter_json_size))
    }

    /// Returns cloned filters only when they fit into one relay REQ.
    pub fn filters_for_single_req(&self, filters: &[Filter]) -> Option<Vec<Filter>> {
        self.filters_fit_single_req(filters)?
            .then(|| filters.to_vec())
    }

    pub(crate) fn indexed_filters_for_single_req(
        &self,
        filters: impl IntoIterator<Item = (usize, Filter)>,
    ) -> Option<Vec<IndexedFilter>> {
        let mut filter_json_size = 0usize;
        let mut indexed_filters = Vec::new();
        for (source_index, filter) in filters {
            let json_size = Self::filter_json_size(&filter)?;
            filter_json_size = filter_json_size.saturating_add(json_size);
            indexed_filters.push(IndexedFilter {
                source_index,
                filter,
                json_size,
            });
        }

        self.can_fit(0, indexed_filters.len(), 0, filter_json_size)
            .then_some(indexed_filters)
    }

    pub fn can_fit(
        &self,
        current_filter_count: usize,
        new_filter_count: usize,
        current_json_size: usize,
        new_json_size: usize,
    ) -> bool {
        let filter_count = current_filter_count.saturating_add(new_filter_count);
        let filter_json_size = current_json_size.saturating_add(new_json_size);
        filter_count <= self.max_filters_per_req
            && Self::req_json_size(filter_count, filter_json_size) <= self.max_json_bytes
    }

    /// Returns fixed serialized bytes in a REQ frame excluding filter JSON and
    /// commas between filters.
    pub fn req_overhead() -> usize {
        // `["REQ","abc...123",...]`
        11 + RelayReqId::byte_len()
    }

    /// Returns the serialized JSON byte count for a REQ with the given number
    /// of filters and total already-serialized filter JSON bytes.
    pub fn req_json_size(filter_count: usize, filter_json_size: usize) -> usize {
        if filter_count == 0 {
            return Self::req_overhead().saturating_add(3);
        }

        Self::req_overhead()
            .saturating_add(filter_json_size)
            .saturating_add(filter_count.saturating_sub(1))
    }

    /// Returns the serialized JSON byte count for one filter.
    pub fn filter_json_size(filter: &Filter) -> Option<usize> {
        filter.json().ok().map(|json| json.len())
    }
}

impl RelayCoordinatorLimits {
    pub fn new(limits: RelayLimitations) -> Self {
        Self {
            maximum_subs: limits.maximum_subs,
            max_json_bytes: limits.max_json_bytes,
            sub_guardian: SubPassGuardian::new(limits.maximum_subs),
        }
    }

    pub fn maximum_subs(&self) -> usize {
        self.maximum_subs
    }

    pub(in crate::relay) fn set_maximum_subs(
        &mut self,
        maximum_subs: usize,
    ) -> Option<Vec<SubPassRevocation>> {
        self.maximum_subs = maximum_subs;
        self.set_effective_total(maximum_subs)
    }

    fn set_effective_total(&mut self, new_max: usize) -> Option<Vec<SubPassRevocation>> {
        let old = self.sub_guardian.total_passes;

        if new_max == old {
            return None;
        }

        if new_max > old {
            let add = new_max - old;
            self.sub_guardian.spawn_passes(add);
            self.sub_guardian.total_passes = new_max;
            return None;
        }

        // new_max < old
        let remove = old - new_max;
        self.sub_guardian.total_passes = new_max;

        let mut pending = Vec::new();

        for _ in 0..remove {
            let mut revocation = SubPassRevocation::new();
            if let Some(pass) = self.sub_guardian.available_passes.pop() {
                // can revoke immediately -> do NOT return a revocation object for it
                revocation.revocate(pass);
            } else {
                // can't revoke now -> return a revocation object to be fulfilled later
                pending.push(revocation);
            }
        }

        if pending.is_empty() {
            None
        } else {
            Some(pending)
        }
    }
}

pub(in crate::relay) struct SubPassGuardian {
    total_passes: usize,
    available_passes: Vec<SubPass>,
}

impl SubPassGuardian {
    pub(crate) fn new(max_subs: usize) -> Self {
        Self {
            available_passes: (0..max_subs)
                .map(|_| SubPass { _private: () })
                .collect::<Vec<_>>(),
            total_passes: max_subs,
        }
    }

    pub(in crate::relay) fn take_pass(&mut self) -> Option<SubPass> {
        self.available_passes.pop()
    }

    pub(in crate::relay) fn available_passes(&self) -> usize {
        self.available_passes.len()
    }

    pub(in crate::relay) fn total_passes(&self) -> usize {
        self.total_passes
    }

    pub(in crate::relay) fn return_pass(&mut self, pass: SubPass) {
        self.available_passes.push(pass);
        tracing::debug!(
            "Returned pass. Using {} of {} passes",
            self.total_passes - self.available_passes(),
            self.total_passes
        );
    }

    pub(crate) fn spawn_passes(&mut self, new_passes: usize) {
        for _ in 0..new_passes {
            self.available_passes.push(SubPass { _private: () });
        }
    }
}

/// Annihilates an existing `SubPass`. These should only be generated from the `RelayCoordinatorLimits`
/// when there is a new total subs which is less than the existing amount
#[derive(Debug)]
pub(in crate::relay) struct SubPassRevocation {
    revoked: bool,
}

impl SubPassRevocation {
    pub(in crate::relay) fn revocate(&mut self, _: SubPass) {
        self.revoked = true;
    }

    pub(in crate::relay) fn new() -> Self {
        Self { revoked: false }
    }
}

/// It completely breaks subscription management if we don't have strict accounting, so we crash if we fail to revocate
impl Drop for SubPassRevocation {
    fn drop(&mut self) {
        if !self.revoked {
            panic!("The subscription pass revocator did not revoke the SubPass");
        }
    }
}

#[derive(Debug)]
pub(in crate::relay) struct SubPass {
    _private: (),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientMessage, Pubkey};

    fn actual_req_json_len(filters: Vec<Filter>) -> usize {
        let sid = RelayReqId::from("123e4567-e89b-12d3-a456-426614174000");
        ClientMessage::req(sid.to_string(), filters)
            .to_json()
            .expect("serialize req")
            .len()
    }

    fn filter_json_size_sum(filters: &[Filter]) -> usize {
        filters
            .iter()
            .map(|filter| ReqFilterLimits::filter_json_size(filter).expect("filter json"))
            .sum()
    }

    #[test]
    fn req_filter_limits_rejects_one_filter_at_actual_serialized_boundary() {
        let filters = vec![Filter::new().kinds(vec![1]).build()];
        let actual_len = actual_req_json_len(filters.clone());
        let filter_json_size = filter_json_size_sum(&filters);

        assert_eq!(
            actual_len,
            ReqFilterLimits::req_json_size(filters.len(), filter_json_size)
        );
        assert!(ReqFilterLimits::new(1, actual_len).can_fit(0, 1, 0, filter_json_size));
        assert!(!ReqFilterLimits::new(1, actual_len - 1).can_fit(0, 1, 0, filter_json_size));
    }

    #[test]
    fn req_filter_limits_rejects_multi_filter_at_actual_serialized_boundary() {
        let filters = vec![
            Filter::new().kinds(vec![1]).build(),
            Filter::new().kinds(vec![2]).build(),
        ];
        let actual_len = actual_req_json_len(filters.clone());
        let filter_json_size = filter_json_size_sum(&filters);

        assert_eq!(
            actual_len,
            ReqFilterLimits::req_json_size(filters.len(), filter_json_size)
        );
        assert!(ReqFilterLimits::new(2, actual_len).can_fit(0, 2, 0, filter_json_size));
        assert!(!ReqFilterLimits::new(2, actual_len - 1).can_fit(0, 2, 0, filter_json_size));

        assert_eq!(
            ReqFilterLimits::new(2, actual_len - 1).filters_fit_single_req(&filters),
            Some(false)
        );
    }

    #[test]
    fn oversized_single_filter_is_rejected_without_rewriting_authors() {
        let pubkeys = (0..6)
            .map(|index| {
                let mut bytes = [0u8; 32];
                bytes[31] = index;
                crate::Pubkey::new(bytes)
            })
            .collect::<Vec<_>>();
        let filter = Filter::new()
            .authors(pubkeys.iter().map(Pubkey::bytes))
            .kinds([1])
            .limit(20)
            .build();
        let two_author_filter = Filter::new()
            .authors(pubkeys[0..2].iter().map(Pubkey::bytes))
            .kinds([1])
            .limit(20)
            .build();
        let three_author_filter = Filter::new()
            .authors(pubkeys[0..3].iter().map(Pubkey::bytes))
            .kinds([1])
            .limit(20)
            .build();
        let two_author_size =
            ReqFilterLimits::filter_json_size(&two_author_filter).expect("two author size");
        let three_author_size =
            ReqFilterLimits::filter_json_size(&three_author_filter).expect("three author size");

        let limits = ReqFilterLimits::new(200, ReqFilterLimits::req_json_size(1, two_author_size));

        assert!(three_author_size > two_author_size);
        assert_eq!(
            limits.filters_fit_single_req(std::slice::from_ref(&filter)),
            Some(false)
        );
        assert!(limits.filters_for_single_req(&[filter]).is_none());
    }

    // ==================== SubPassGuardian tests ====================

    #[test]
    fn guardian_starts_with_correct_passes() {
        let guardian = SubPassGuardian::new(10);
        assert_eq!(guardian.available_passes(), 10);
    }

    #[test]
    fn guardian_take_pass_decrements() {
        let mut guardian = SubPassGuardian::new(5);
        let pass = guardian.take_pass();
        assert!(pass.is_some());
        assert_eq!(guardian.available_passes(), 4);
    }

    #[test]
    fn guardian_take_pass_returns_none_when_empty() {
        let mut guardian = SubPassGuardian::new(1);
        let _pass = guardian.take_pass();
        assert!(guardian.take_pass().is_none());
        assert_eq!(guardian.available_passes(), 0);
    }

    #[test]
    fn guardian_return_pass_increments() {
        let mut guardian = SubPassGuardian::new(1);
        let pass = guardian.take_pass().unwrap();
        assert_eq!(guardian.available_passes(), 0);
        guardian.return_pass(pass);
        assert_eq!(guardian.available_passes(), 1);
    }

    #[test]
    fn guardian_spawn_passes_adds_new_passes() {
        let mut guardian = SubPassGuardian::new(2);
        assert_eq!(guardian.available_passes(), 2);
        guardian.spawn_passes(3);
        assert_eq!(guardian.available_passes(), 5);
    }

    #[test]
    fn guardian_multiple_take_and_return() {
        let mut guardian = SubPassGuardian::new(3);

        let pass1 = guardian.take_pass().unwrap();
        let pass2 = guardian.take_pass().unwrap();
        assert_eq!(guardian.available_passes(), 1);

        guardian.return_pass(pass1);
        assert_eq!(guardian.available_passes(), 2);

        let _pass3 = guardian.take_pass().unwrap();
        assert_eq!(guardian.available_passes(), 1);

        guardian.return_pass(pass2);
        assert_eq!(guardian.available_passes(), 2);
    }

    // ==================== SubPassRevocation tests ====================

    #[test]
    #[should_panic(expected = "did not revoke")]
    fn revocation_panics_if_not_revoked() {
        let _revocation = SubPassRevocation::new();
        // drop triggers panic
    }

    #[test]
    fn revocation_does_not_panic_when_revoked() {
        let mut guardian = SubPassGuardian::new(1);
        let pass = guardian.take_pass().unwrap();
        let mut revocation = SubPassRevocation::new();
        revocation.revocate(pass);
        // drop should not panic since revoked is true
    }

    #[test]
    fn revocation_marks_as_revoked_after_revocate() {
        let mut guardian = SubPassGuardian::new(1);
        let pass = guardian.take_pass().unwrap();
        let mut revocation = SubPassRevocation::new();

        assert!(!revocation.revoked);
        revocation.revocate(pass);
        assert!(revocation.revoked);
    }

    // ==================== RelayCoordinatorLimits tests ====================

    #[test]
    fn new_total_returns_none_when_same() {
        let mut limits = RelayCoordinatorLimits::new(RelayLimitations {
            maximum_subs: 5,
            max_json_bytes: 400_000,
        });

        let revocations = limits.set_maximum_subs(5);
        assert!(revocations.is_none());
        assert_eq!(limits.sub_guardian.available_passes(), 5);
    }

    #[test]
    fn new_total_spawns_passes_when_increasing() {
        let mut limits = RelayCoordinatorLimits::new(RelayLimitations {
            maximum_subs: 5,
            max_json_bytes: 400_000,
        });

        let revocations = limits.set_maximum_subs(10);
        assert!(revocations.is_none());
        assert_eq!(limits.sub_guardian.available_passes(), 10);
    }

    #[test]
    fn new_total_returns_revocations_when_decreasing() {
        let mut limits = RelayCoordinatorLimits::new(RelayLimitations {
            maximum_subs: 10,
            max_json_bytes: 400_000,
        });

        let revocations = limits.set_maximum_subs(5);
        assert!(revocations.is_none());
    }

    #[test]
    fn new_total_partial_revocations_when_passes_in_use() {
        let mut limits = RelayCoordinatorLimits::new(RelayLimitations {
            maximum_subs: 5,
            max_json_bytes: 400_000,
        });

        // Take 3 passes (simulate them being in use)
        let pass = limits.sub_guardian.take_pass().unwrap();
        limits.sub_guardian.take_pass();
        limits.sub_guardian.take_pass();
        assert_eq!(limits.sub_guardian.available_passes(), 2);

        // Now reduce to 2 total (need to remove 3)
        let revocations = limits.set_maximum_subs(2);

        assert!(revocations.is_some());

        let mut revs = revocations.unwrap();
        // since there were two available passes, the guardian used those, but there is still one pass unaccounted for
        assert_eq!(revs.len(), 1);

        revs.pop().unwrap().revocate(pass);
    }
}
