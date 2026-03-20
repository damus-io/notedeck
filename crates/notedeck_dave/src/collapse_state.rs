use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Tracks which host and cwd groups are collapsed in the session list.
///
/// Used by navigation (Ctrl+Tab, Ctrl+1-9, Ctrl+N/P) to skip sessions
/// hidden inside collapsed folders.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct CollapseState {
    hosts: HashSet<String>,
    /// Stored as "hostname\0display_cwd" to avoid allocating a tuple on every lookup.
    cwds: HashSet<String>,
}

impl Default for CollapseState {
    fn default() -> Self {
        Self::new()
    }
}

fn cwd_key(hostname: &str, display_cwd: &str) -> String {
    format!("{}\0{}", hostname, display_cwd)
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

    pub fn toggle_cwd(&mut self, hostname: &str, display_cwd: &str) {
        let key = cwd_key(hostname, display_cwd);
        if !self.cwds.remove(&key) {
            self.cwds.insert(key);
        }
    }

    pub fn is_host_collapsed(&self, hostname: &str) -> bool {
        self.hosts.contains(hostname)
    }

    pub fn is_cwd_collapsed(&self, hostname: &str, display_cwd: &str) -> bool {
        self.cwds.contains(&cwd_key(hostname, display_cwd))
    }

    /// Returns true if a session in this host/cwd is visible (not collapsed).
    pub fn is_visible(&self, hostname: &str, display_cwd: &str) -> bool {
        !self.is_host_collapsed(hostname) && !self.is_cwd_collapsed(hostname, display_cwd)
    }
}

#[cfg(test)]
mod tests {
    use super::CollapseState;

    #[test]
    fn toggle_cwd_only_hides_that_cwd() {
        let mut collapse = CollapseState::new();

        assert!(collapse.is_visible("remote-a", "/srv/app"));
        assert!(collapse.is_visible("remote-a", "/srv/other"));

        collapse.toggle_cwd("remote-a", "/srv/app");

        assert!(collapse.is_cwd_collapsed("remote-a", "/srv/app"));
        assert!(!collapse.is_visible("remote-a", "/srv/app"));
        assert!(collapse.is_visible("remote-a", "/srv/other"));

        collapse.toggle_cwd("remote-a", "/srv/app");

        assert!(!collapse.is_cwd_collapsed("remote-a", "/srv/app"));
        assert!(collapse.is_visible("remote-a", "/srv/app"));
    }

    #[test]
    fn host_collapse_hides_all_cwds_without_clearing_cwd_state() {
        let mut collapse = CollapseState::new();
        collapse.toggle_cwd("remote-a", "/srv/app");

        collapse.toggle_host("remote-a");

        assert!(collapse.is_host_collapsed("remote-a"));
        assert!(!collapse.is_visible("remote-a", "/srv/app"));
        assert!(!collapse.is_visible("remote-a", "/srv/other"));

        collapse.toggle_host("remote-a");

        assert!(!collapse.is_host_collapsed("remote-a"));
        assert!(collapse.is_cwd_collapsed("remote-a", "/srv/app"));
        assert!(!collapse.is_visible("remote-a", "/srv/app"));
        assert!(collapse.is_visible("remote-a", "/srv/other"));
    }
}
