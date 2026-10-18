//! A nostrdb in a throwaway directory, for tests.
//!
//! This crate depends on nothing but nostrdb and tempfile, so headway,
//! agentium-core and cashflow, which sit below notedeck and
//! notedeck_testing, reach the same fixture those two re-export. Before it,
//! each of them carried its own copy of [`TempNdb`] and [`test_config`],
//! and a copy that dropped the Windows mapsize filled CI's disk.

use nostrdb::{Config, Ndb};
use tempfile::TempDir;

/// Returns a [`Config`] with a small mapsize suitable for tests.
///
/// On Windows LMDB actually allocates the full mapsize on disk, so tests
/// that use the default (very large) mapsize can exhaust disk space or hit
/// CI resource limits. On other platforms the map is only virtually mapped,
/// so the default is left alone.
pub fn test_config() -> Config {
    if cfg!(target_os = "windows") {
        Config::new().set_mapsize(32 * 1024 * 1024) // 32 MiB
    } else {
        Config::new()
    }
}

/// A nostrdb opened in a fresh temporary directory, for tests.
///
/// `ndb` is declared first so it closes before `dir` deletes its files.
pub struct TempNdb {
    pub ndb: Ndb,
    pub dir: TempDir,
}

impl TempNdb {
    /// Open a nostrdb with [`test_config`] in a new temporary directory.
    pub fn new() -> Self {
        Self::with_config(&test_config())
    }

    /// Open a nostrdb with `config` in a new temporary directory.
    pub fn with_config(config: &Config) -> Self {
        let dir = TempDir::new().expect("tmp dir");
        let ndb = Ndb::new(dir.path().to_str().expect("path"), config).expect("ndb");
        Self { ndb, dir }
    }
}

impl Default for TempNdb {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostrdb::Transaction;

    /// The db opens in its own directory and outlives nothing it needs.
    #[test]
    fn opens_an_empty_db_in_its_own_dir() {
        let db = TempNdb::new();
        assert!(db.dir.path().exists());
        let txn = Transaction::new(&db.ndb).expect("txn");
        assert!(db.ndb.get_note_by_id(&txn, &[0u8; 32]).is_err());
    }

    /// Two fixtures never share a directory.
    #[test]
    fn each_fixture_gets_a_fresh_dir() {
        let a = TempNdb::new();
        let b = TempNdb::new();
        assert_ne!(a.dir.path(), b.dir.path());
    }
}
