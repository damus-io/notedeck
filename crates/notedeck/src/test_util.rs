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
