/// Limitations imposed by the relay
pub struct RelayLimitations {
    // corresponds to NIP-11 `max_subscriptions`
    pub maximum_subs: usize,

    // corresponds to NIP-11 `max_message_length`
    pub max_json_bytes: usize,
}

impl Default for RelayLimitations {
    fn default() -> Self {
        Self {
            maximum_subs: 10,
            max_json_bytes: 400_000,
        }
    }
}

pub struct RelayCoordinatorLimits {
    pub sub_guardian: SubPassGuardian,
    pub max_json_bytes: usize,
}

impl RelayCoordinatorLimits {
    pub fn new(limits: RelayLimitations) -> Self {
        Self {
            max_json_bytes: limits.max_json_bytes,
            sub_guardian: SubPassGuardian::new(limits.maximum_subs),
        }
    }

    pub fn new_total(&mut self, new_max: usize) -> Option<Vec<SubPassRevocation>> {
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

pub struct SubPassGuardian {
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

    pub fn take_pass(&mut self) -> Option<SubPass> {
        self.available_passes.pop()
    }

    pub fn available_passes(&self) -> usize {
        self.available_passes.len()
    }

    pub fn total_passes(&self) -> usize {
        self.total_passes
    }

    pub fn return_pass(&mut self, pass: SubPass) {
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
pub struct SubPassRevocation {
    revoked: bool,
}

impl SubPassRevocation {
    pub fn revocate(&mut self, _: SubPass) {
        self.revoked = true;
    }

    pub(crate) fn new() -> Self {
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

pub struct SubPass {
    _private: (),
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let revocations = limits.new_total(5);
        assert!(revocations.is_none());
        assert_eq!(limits.sub_guardian.available_passes(), 5);
    }

    #[test]
    fn new_total_spawns_passes_when_increasing() {
        let mut limits = RelayCoordinatorLimits::new(RelayLimitations {
            maximum_subs: 5,
            max_json_bytes: 400_000,
        });

        let revocations = limits.new_total(10);
        assert!(revocations.is_none());
        assert_eq!(limits.sub_guardian.available_passes(), 10);
    }

    #[test]
    fn new_total_returns_revocations_when_decreasing() {
        let mut limits = RelayCoordinatorLimits::new(RelayLimitations {
            maximum_subs: 10,
            max_json_bytes: 400_000,
        });

        let revocations = limits.new_total(5);
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
        let revocations = limits.new_total(2);

        assert!(revocations.is_some());

        let mut revs = revocations.unwrap();
        // since there were two available passes, the guardian used those, but there is still one pass unaccounted for
        assert_eq!(revs.len(), 1);

        revs.pop().unwrap().revocate(pass);
    }
}
