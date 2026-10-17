//! Test fixtures shared by this crate's unit tests.

use std::time::Duration;

use nostrdb::{Config, Ndb};
use tempfile::TempDir;

/// Backstop for an ingest wait. A local ingest commits in microseconds, so
/// this only trips when a note is never coming.
pub const INGEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A nostrdb in a temp dir, removed when dropped.
pub struct TempNdb {
    pub ndb: Ndb,
    /// Held so the directory outlives the database.
    _dir: TempDir,
}

impl TempNdb {
    /// A fresh, empty nostrdb.
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &test_config()).unwrap();
        Self { ndb, _dir: dir }
    }
}

/// A [`Config`] with a small mapsize on Windows, where LMDB allocates the
/// whole map on disk and nostrdb's default fills a CI runner's.
fn test_config() -> Config {
    if cfg!(target_os = "windows") {
        Config::new().set_mapsize(32 * 1024 * 1024)
    } else {
        Config::new()
    }
}
