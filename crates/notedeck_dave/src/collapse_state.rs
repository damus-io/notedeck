use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// Tracks which host and cwd groups are collapsed in the session list.
///
/// Used by navigation (Ctrl+Tab, Ctrl+1-9, Ctrl+N/P) to skip sessions
/// hidden inside collapsed folders.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct CollapseState {
    hosts: HashSet<String>,
    /// Stored as "hostname\0raw_cwd" to avoid allocating a tuple on every lookup.
    cwds: HashSet<String>,
}

impl Default for CollapseState {
    fn default() -> Self {
        Self::new()
    }
}

fn cwd_key(hostname: &str, cwd: &Path) -> String {
    format!("{}\0{}", hostname, cwd.display())
}

impl CollapseState {
    pub fn new() -> Self {
        Self {
            hosts: HashSet::new(),
            cwds: HashSet::new(),
        }
    }

    pub fn toggle_host(&mut self, hostname: &str) {
        if !self.hosts.remove(hostname) {
            self.hosts.insert(hostname.to_string());
        }
    }

    pub fn toggle_cwd(&mut self, hostname: &str, cwd: &Path) {
        let key = cwd_key(hostname, cwd);
        if !self.cwds.remove(&key) {
            self.cwds.insert(key);
        }
    }

    pub fn is_host_collapsed(&self, hostname: &str) -> bool {
        self.hosts.contains(hostname)
    }

    pub fn is_cwd_collapsed(&self, hostname: &str, cwd: &Path) -> bool {
        self.cwds.contains(&cwd_key(hostname, cwd))
    }

    /// Returns true if a session in this host/cwd is visible (not collapsed).
    pub fn is_visible(&self, hostname: &str, cwd: &Path) -> bool {
        !self.is_host_collapsed(hostname) && !self.is_cwd_collapsed(hostname, cwd)
    }
}

#[cfg(test)]
mod tests {
    use super::CollapseState;
    use std::path::Path;

    #[test]
    fn toggle_cwd_only_hides_that_cwd() {
        let mut collapse = CollapseState::new();

        assert!(collapse.is_visible("remote-a", Path::new("/srv/app")));
        assert!(collapse.is_visible("remote-a", Path::new("/srv/other")));

        collapse.toggle_cwd("remote-a", Path::new("/srv/app"));

        assert!(collapse.is_cwd_collapsed("remote-a", Path::new("/srv/app")));
        assert!(!collapse.is_visible("remote-a", Path::new("/srv/app")));
        assert!(collapse.is_visible("remote-a", Path::new("/srv/other")));

        collapse.toggle_cwd("remote-a", Path::new("/srv/app"));

        assert!(!collapse.is_cwd_collapsed("remote-a", Path::new("/srv/app")));
        assert!(collapse.is_visible("remote-a", Path::new("/srv/app")));
    }

    #[test]
    fn host_collapse_hides_all_cwds_without_clearing_cwd_state() {
        let mut collapse = CollapseState::new();
        collapse.toggle_cwd("remote-a", Path::new("/srv/app"));

        collapse.toggle_host("remote-a");

        assert!(collapse.is_host_collapsed("remote-a"));
        assert!(!collapse.is_visible("remote-a", Path::new("/srv/app")));
        assert!(!collapse.is_visible("remote-a", Path::new("/srv/other")));

        collapse.toggle_host("remote-a");

        assert!(!collapse.is_host_collapsed("remote-a"));
        assert!(collapse.is_cwd_collapsed("remote-a", Path::new("/srv/app")));
        assert!(!collapse.is_visible("remote-a", Path::new("/srv/app")));
        assert!(collapse.is_visible("remote-a", Path::new("/srv/other")));
    }

    #[test]
    fn serde_roundtrip_preserves_hosts_and_cwds() {
        let mut collapse = CollapseState::new();
        collapse.toggle_host("remote-a");
        collapse.toggle_cwd("remote-b", Path::new("/srv/api"));

        let json = serde_json::to_string(&collapse).expect("collapse state should serialize");
        let restored: CollapseState =
            serde_json::from_str(&json).expect("collapse state should deserialize");

        assert!(restored.is_host_collapsed("remote-a"));
        assert!(restored.is_cwd_collapsed("remote-b", Path::new("/srv/api")));
        assert!(!restored.is_visible("remote-a", Path::new("/srv/other")));
        assert!(!restored.is_visible("remote-b", Path::new("/srv/api")));
        assert!(restored.is_visible("remote-b", Path::new("/srv/other")));
    }
}
