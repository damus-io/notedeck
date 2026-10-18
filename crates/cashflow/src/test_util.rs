//! Test fixtures shared by this crate's unit tests.

use std::time::Duration;

pub use ndb_testing::TempNdb;

/// Backstop for an ingest wait. A local ingest commits in microseconds, so
/// this only trips when a note is never coming.
pub const INGEST_TIMEOUT: Duration = Duration::from_secs(30);
