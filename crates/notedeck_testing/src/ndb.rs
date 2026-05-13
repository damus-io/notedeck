use std::{
    thread,
    time::{Duration, Instant},
};

use nostrdb::{Filter, Transaction};

use crate::{
    stepping::{assert_device_condition_stable, wait_for_device_condition},
    DeviceHarness,
};

/// Owned local NostrDB query used by app e2e assertions.
pub struct LocalQuery {
    filters: Vec<Filter>,
    max_results: i32,
    query_context: String,
}

impl LocalQuery {
    pub fn new(filters: Vec<Filter>, max_results: i32, query_context: impl Into<String>) -> Self {
        Self {
            filters,
            max_results,
            query_context: query_context.into(),
        }
    }

    pub fn wait_for_count(
        &self,
        device: &mut DeviceHarness,
        expected_count: usize,
        timeout: Duration,
        context: &str,
    ) -> usize {
        wait_for_device_condition(device, timeout, context, |device| {
            let imported = self.count(device);
            if imported >= expected_count {
                Ok(imported)
            } else {
                Err(format!(
                    "expected at least {expected_count}, imported {imported}"
                ))
            }
        })
    }

    pub fn assert_count_stable(
        &self,
        device: &mut DeviceHarness,
        expected_count: usize,
        frames: usize,
        context: &str,
    ) {
        assert_device_condition_stable(device, frames, context, |device| {
            let imported = self.count(device);
            if imported == expected_count {
                Ok(())
            } else {
                Err(format!(
                    "expected imported count to stay at {expected_count}, found {imported}"
                ))
            }
        });
    }

    pub fn wait_for_count_plateau(
        &self,
        device: &mut DeviceHarness,
        minimum_count: usize,
        stable_frames: usize,
        timeout: Duration,
        context: &str,
    ) -> usize {
        let mut last_imported = 0usize;
        let mut unchanged_frames = 0usize;

        wait_for_device_condition(device, timeout, context, |device| {
            let imported = self.count(device);
            if imported >= minimum_count {
                if imported == last_imported {
                    unchanged_frames += 1;
                    if unchanged_frames >= stable_frames {
                        return Ok(imported);
                    }
                } else {
                    last_imported = imported;
                    unchanged_frames = 0;
                }
            }

            Err(format!(
                "expected a stable plateau at or above {minimum_count}, imported {imported}"
            ))
        })
    }

    pub fn count(&self, device: &mut DeviceHarness) -> usize {
        let egui_ctx = device.ctx.clone();
        let app_ctx = &mut device.state_mut().notedeck.app_context(&egui_ctx);
        let txn = Transaction::new(app_ctx.ndb).expect("txn");
        let count = app_ctx
            .ndb
            .query(&txn, &self.filters, self.max_results)
            .unwrap_or_else(|err| panic!("{}: {err:?}", self.query_context))
            .len();
        count
    }
}

/// Steps two devices until both local NostrDB query counts reach expectations.
pub fn wait_for_two_local_query_counts(
    first: &mut DeviceHarness,
    first_expectation: (&LocalQuery, usize),
    second: &mut DeviceHarness,
    second_expectation: (&LocalQuery, usize),
    timeout: Duration,
    context: &str,
) {
    let deadline = Instant::now() + timeout;

    loop {
        first.step();
        second.step();

        let (first_query, first_expected_count) = first_expectation;
        let (second_query, second_expected_count) = second_expectation;
        let first_imported = first_query.count(first);
        let second_imported = second_query.count(second);
        if first_imported >= first_expected_count && second_imported >= second_expected_count {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for {context}; expected ({}, {}), imported ({first_imported}, {second_imported})",
            first_expected_count,
            second_expected_count,
        );

        thread::sleep(Duration::from_millis(20));
    }
}
