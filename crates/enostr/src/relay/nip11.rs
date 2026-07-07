/// Raw `limitation` object from a relay NIP-11 document.
///
/// Outbox code decides which fields matter for runtime behavior.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Nip11LimitationsRaw {
    pub max_message_length: Option<i64>,
    pub max_subscriptions: Option<i64>,
    // Intentionally omit `max_filters`: it is not part of the canonical nostr
    // repository's NIP-11 limitation schema.
    pub max_limit: Option<i64>,
    pub max_subid_length: Option<i64>,
    pub max_event_tags: Option<i64>,
    pub max_content_length: Option<i64>,
    pub min_pow_difficulty: Option<i64>,
    pub auth_required: Option<bool>,
    pub payment_required: Option<bool>,
    pub created_at_lower_limit: Option<i64>,
    pub created_at_upper_limit: Option<i64>,
}

/// Result of applying a raw NIP-11 response to a relay coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nip11ApplyOutcome {
    Applied,
    Unchanged,
    UnsupportedSubIdLength { max_subid_length: usize },
    RelayUnknown,
}
