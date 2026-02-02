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
