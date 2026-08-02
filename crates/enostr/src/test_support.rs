mod enostr_api {
    pub use crate::{
        EventIngestCapability, EventIngestRequest, FullHistoryCapability,
        FullHistoryLocalPresenceRequest, FullHistoryLocalPresenceResult,
        FullHistoryLocalSetRequest, FullHistoryLocalSetResult,
        FullHistoryPendingIngestionPresenceRequest, FullHistoryPendingIngestionPresenceResult,
        Nip11Capability, Nip11FetchRequest, Nip11LimitationsRaw, OutboxService,
    };
}

#[path = "../../enostr_test_support/src/outbox.rs"]
pub mod outbox;
