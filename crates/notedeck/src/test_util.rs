use nostrdb::Config;

/// Returns a [`Config`] with a small mapsize suitable for tests.
///
/// On Windows LMDB actually allocates the full mapsize on disk, so tests
/// that use the default (very large) mapsize can exhaust disk space or hit
/// CI resource limits.
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
    pub ndb: nostrdb::Ndb,
    pub dir: tempfile::TempDir,
}

impl TempNdb {
    /// Open a nostrdb with [`test_config`] in a new temporary directory.
    pub fn new() -> Self {
        Self::with_config(&test_config())
    }

    /// Open a nostrdb with `config` in a new temporary directory.
    pub fn with_config(config: &Config) -> Self {
        let dir = tempfile::TempDir::new().expect("tmp dir");
        let ndb = nostrdb::Ndb::new(dir.path().to_str().expect("path"), config).expect("ndb");
        Self { ndb, dir }
    }
}
