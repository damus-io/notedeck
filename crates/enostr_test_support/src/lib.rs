//! Test-only helpers for crates that exercise enostr APIs.

mod enostr_api {
    pub use enostr::{
        EventIngestCapability, EventIngestRequest, FullHistoryCapability,
        FullHistoryLocalPresenceRequest, FullHistoryLocalPresenceResult,
        FullHistoryLocalSetRequest, FullHistoryLocalSetResult,
        FullHistoryPendingIngestionPresenceRequest, FullHistoryPendingIngestionPresenceResult,
        Nip11Capability, Nip11FetchRequest, Nip11LimitationsRaw, NormRelayUrl, OutboxService,
    };
}

pub mod outbox;
pub mod relay;
