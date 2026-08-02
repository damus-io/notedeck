use std::future::{ready, Ready};

use super::enostr_api::{
    EventIngestCapability, EventIngestRequest, FullHistoryCapability,
    FullHistoryLocalPresenceRequest, FullHistoryLocalPresenceResult, FullHistoryLocalSetRequest,
    FullHistoryLocalSetResult, FullHistoryPendingIngestionPresenceRequest,
    FullHistoryPendingIngestionPresenceResult, Nip11Capability, Nip11FetchRequest,
    Nip11LimitationsRaw, OutboxService,
};

/// NIP-11 capability for tests that need an outbox service but do not
/// exercise relay metadata fetching.
#[derive(Clone, Copy, Default)]
pub struct TestNip11Capability;

impl Nip11Capability for TestNip11Capability {
    type Output = Result<Nip11LimitationsRaw, String>;
    type Future = Ready<Self::Output>;

    fn fetch_nip11(&self, _request: Nip11FetchRequest) -> Self::Future {
        ready(Err("test NIP-11 capability unavailable".to_owned()))
    }
}

/// Full-history capability for tests that need an outbox service but do
/// not exercise local history set construction.
#[derive(Clone, Copy, Default)]
pub struct TestFullHistoryCapability;

impl FullHistoryCapability for TestFullHistoryCapability {
    type LocalSetOutput = FullHistoryLocalSetResult;
    type LocalSetFuture = Ready<Self::LocalSetOutput>;
    type LocalPresenceOutput = FullHistoryLocalPresenceResult;
    type LocalPresenceFuture = Ready<Self::LocalPresenceOutput>;
    type PendingIngestionPresenceOutput = FullHistoryPendingIngestionPresenceResult;
    type PendingIngestionPresenceFuture = Ready<Self::PendingIngestionPresenceOutput>;

    fn build_local_set(&self, request: FullHistoryLocalSetRequest) -> Self::LocalSetFuture {
        ready(FullHistoryLocalSetResult {
            history_id: request.history_id,
            request_id: request.request_id,
            result: None,
        })
    }

    fn check_local_presence(
        &self,
        request: FullHistoryLocalPresenceRequest,
    ) -> Self::LocalPresenceFuture {
        ready(FullHistoryLocalPresenceResult {
            request_id: request.request_id,
            missing_ids: request.candidate_ids,
            already_local_ids: Default::default(),
        })
    }

    fn check_pending_ingestion_presence(
        &self,
        _request: FullHistoryPendingIngestionPresenceRequest,
    ) -> Self::PendingIngestionPresenceFuture {
        ready(FullHistoryPendingIngestionPresenceResult {
            stored_ids: Default::default(),
        })
    }
}

/// Event ingest capability for tests that need service-owned websocket
/// behavior but do not assert on storage ingestion side effects.
#[derive(Clone, Copy, Default)]
pub struct TestEventIngestCapability;

impl EventIngestCapability for TestEventIngestCapability {
    type Future = Ready<()>;

    fn ingest_event(&self, _request: EventIngestRequest) -> Self::Future {
        ready(())
    }
}

/// Outbox service type with inert host capabilities for tests.
pub type TestOutboxService =
    OutboxService<TestNip11Capability, TestFullHistoryCapability, TestEventIngestCapability>;

/// Construct an outbox service through the same capability boundary used by
/// production hosts.
pub fn test_outbox_service() -> TestOutboxService {
    OutboxService::with_capabilities(
        TestNip11Capability,
        TestFullHistoryCapability,
        TestEventIngestCapability,
    )
}
