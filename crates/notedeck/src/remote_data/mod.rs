mod negentropy;

#[cfg(test)]
mod tests;

use self::negentropy::{FullHistoryNegSetProvider, NdbEventChecker};
use crate::jobs::{JobCache, JobPool, JobSpawner};
use crate::relay_limits::{enqueue_nip11_fetch, RelayLimitJobs};
use crate::{EguiWakeup, RemoteApi, ScopedSubsState};
use egui::Context;
use enostr::{OutboxRecvBudget, RelayImplType};
use nostrdb::Ndb;
use std::time::{Duration, Instant, SystemTime};

const REMOTE_EVENT_TIME_BUDGET: Duration = Duration::from_millis(8);

/// Owned host backing state for remote transport plus durable remote intent.
///
/// This is the sole owner of the relay pool and all long-lived remote-side
/// host state. Callers only get short-lived scoped wrappers from it.
pub(crate) struct RemoteState {
    pool: enostr::OutboxPool,
    relay_limit_jobs: RelayLimitJobs,
    scoped_sub_state: ScopedSubsState,
}

impl RemoteState {
    /// Build owned remote backing state with full-history services configured.
    pub(crate) fn new(ndb: &Ndb, job_spawner: JobSpawner) -> Self {
        let mut pool = enostr::OutboxPool::default();
        pool.set_neg_set_provider(Box::new(FullHistoryNegSetProvider::new(
            ndb.clone(),
            job_spawner,
        )));
        pool.set_event_checker(Box::new(NdbEventChecker { ndb: ndb.clone() }));
        let (send_new_relay_jobs, receive_new_relay_jobs) = std::sync::mpsc::channel();

        Self {
            pool,
            relay_limit_jobs: JobCache::new(receive_new_relay_jobs, send_new_relay_jobs),
            scoped_sub_state: ScopedSubsState::default(),
        }
    }

    /// Override websocket pong timeout handling for the owned outbox pool.
    pub(crate) fn set_pong_timeout(&mut self, timeout: Duration) {
        self.pool.set_pong_timeout(timeout);
    }

    /// Run host-owned relay-limit maintenance for the owned outbox pool.
    #[profiling::function]
    pub(crate) fn service_relays(&mut self, job_pool: &mut JobPool) {
        let now = SystemTime::now();
        for request in self.pool.take_nip11_fetch_requests(16, now) {
            enqueue_nip11_fetch(self.relay_limit_jobs.sender(), request);
        }

        self.relay_limit_jobs.run_received(job_pool, |_| {});
        self.relay_limit_jobs.deliver_all_completed(|completed| {
            let response = completed.response;
            let now = SystemTime::now();
            match response.result {
                Ok(raw) => {
                    let _ = self.pool.apply_nip11_limits(&response.relay, raw, now);
                }
                Err(error) => {
                    self.pool
                        .record_nip11_failure(&response.relay, error.to_string(), now)
                }
            }
        });
    }

    /// Run host-owned websocket keepalive and relay event ingestion.
    #[profiling::function]
    pub(crate) fn process_events(&mut self, ui_ctx: &Context, ndb: &Ndb) {
        self.process_events_with_budget(
            ui_ctx,
            ndb,
            OutboxRecvBudget::until(Instant::now() + REMOTE_EVENT_TIME_BUDGET),
        );
    }

    #[profiling::function]
    fn process_events_with_budget(
        &mut self,
        ui_ctx: &Context,
        ndb: &Ndb,
        budget: OutboxRecvBudget,
    ) {
        let repaint_ctx = ui_ctx.clone();
        let wakeup = move || repaint_ctx.request_repaint();

        self.pool.keepalive_ping(wakeup);
        let recv = self.pool.try_recv_with_budget(budget, |ev| {
            let from_client = match ev.relay_type {
                RelayImplType::Websocket => false,
                RelayImplType::Multicast => true,
            };

            profiling::scope!("ndb process event");
            if let Err(err) = ndb.process_event_with(
                ev.event_json,
                nostrdb::IngestMetadata::new()
                    .client(from_client)
                    .relay(ev.url),
            ) {
                tracing::error!("error processing event {}: {err}", ev.event_json);
            }
        });
        if recv.time_budget_exhausted {
            ui_ctx.request_repaint();
        }
    }

    #[cfg(test)]
    pub(super) fn process_events_for_test(
        &mut self,
        ui_ctx: &Context,
        ndb: &Ndb,
        budget: OutboxRecvBudget,
    ) {
        self.process_events_with_budget(ui_ctx, ndb, budget);
    }

    /// Open one handler-backed remote API over the owned remote backing state.
    pub(crate) fn api(&mut self, ui_ctx: &Context) -> RemoteApi<'_> {
        let wakeup = EguiWakeup::new(ui_ctx.clone());
        let outbox = enostr::OutboxSessionHandler::new(&mut self.pool, wakeup);
        RemoteApi::new(outbox, &mut self.scoped_sub_state)
    }

    /// Request an egui repaint for the next full-history maintenance deadline.
    pub(crate) fn request_repaint_for_next_full_history_deadline(&self, ctx: &Context) {
        let Some(deadline) = self.pool.next_full_history_deadline() else {
            return;
        };

        ctx.request_repaint_after(deadline.saturating_duration_since(Instant::now()));
    }
}
