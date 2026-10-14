use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// Tracks which host, project, and cwd groups are collapsed in the session list.
///
/// Used by navigation (Ctrl+J/K, Ctrl+1-9, Ctrl+N/P) to skip sessions
/// hidden inside collapsed folders.
#[derive(Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CollapseState {
    hosts: HashSet<String>,
    /// Collapsed projects, stored as "hostname\0project_root" (see [`scoped_key`]).
    #[serde(default)]
    projects: HashSet<String>,
    /// Collapsed workspaces, stored as "hostname\0raw_cwd" (see [`scoped_key`]).
    cwds: HashSet<String>,
}

/// A host-scoped path key ("hostname\0path"), used for both project roots and
/// cwds so a path collapsed on one host doesn't collapse the same path on
/// another. A plain `String` avoids allocating a tuple on every lookup.
fn scoped_key(hostname: &str, path: &Path) -> String {
    format!("{}\0{}", hostname, path.display())
}

impl CollapseState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn toggle_host(&mut self, hostname: &str) {
        if !self.hosts.remove(hostname) {
            self.hosts.insert(hostname.to_string());
        }
    }

    pub fn toggle_project(&mut self, hostname: &str, root: &Path) {
        let key = scoped_key(hostname, root);
        if !self.projects.remove(&key) {
            self.projects.insert(key);
        }
    }

    pub fn toggle_cwd(&mut self, hostname: &str, cwd: &Path) {
        let key = scoped_key(hostname, cwd);
        if !self.cwds.remove(&key) {
            self.cwds.insert(key);
        }
    }

    pub fn is_host_collapsed(&self, hostname: &str) -> bool {
        self.hosts.contains(hostname)
    }

    pub fn is_project_collapsed(&self, hostname: &str, root: &Path) -> bool {
        self.projects.contains(&scoped_key(hostname, root))
    }

    pub fn is_cwd_collapsed(&self, hostname: &str, cwd: &Path) -> bool {
        self.cwds.contains(&scoped_key(hostname, cwd))
    }
}

#[cfg(test)]
mod tests {
    use super::CollapseState;
    use std::path::Path;

    #[test]
    fn toggle_cwd_only_hides_that_cwd() {
        let mut collapse = CollapseState::new();

        assert!(!collapse.is_cwd_collapsed("remote-a", Path::new("/srv/app")));

        collapse.toggle_cwd("remote-a", Path::new("/srv/app"));

        assert!(collapse.is_cwd_collapsed("remote-a", Path::new("/srv/app")));
        assert!(!collapse.is_cwd_collapsed("remote-a", Path::new("/srv/other")));

        collapse.toggle_cwd("remote-a", Path::new("/srv/app"));

        assert!(!collapse.is_cwd_collapsed("remote-a", Path::new("/srv/app")));
    }

    #[test]
    fn project_and_cwd_collapse_are_independent() {
        let mut collapse = CollapseState::new();
        collapse.toggle_project("remote-a", Path::new("/srv/repo"));

        assert!(collapse.is_project_collapsed("remote-a", Path::new("/srv/repo")));
        // Same path on another host stays open (host-scoped keys).
        assert!(!collapse.is_project_collapsed("remote-b", Path::new("/srv/repo")));
        // A cwd equal to the project root is a separate axis from the project.
        assert!(!collapse.is_cwd_collapsed("remote-a", Path::new("/srv/repo")));
    }

    #[test]
    fn host_collapse_does_not_clear_project_or_cwd_state() {
        let mut collapse = CollapseState::new();
        collapse.toggle_cwd("remote-a", Path::new("/srv/app"));
        collapse.toggle_project("remote-a", Path::new("/srv/repo"));

        collapse.toggle_host("remote-a");
        assert!(collapse.is_host_collapsed("remote-a"));

        collapse.toggle_host("remote-a");
        assert!(!collapse.is_host_collapsed("remote-a"));
        // Inner collapse state survives a host collapse/expand round-trip.
        assert!(collapse.is_cwd_collapsed("remote-a", Path::new("/srv/app")));
        assert!(collapse.is_project_collapsed("remote-a", Path::new("/srv/repo")));
    }

    #[test]
    fn serde_roundtrip_preserves_hosts_projects_and_cwds() {
        let mut collapse = CollapseState::new();
        collapse.toggle_host("remote-a");
        collapse.toggle_project("remote-b", Path::new("/srv/repo"));
        collapse.toggle_cwd("remote-b", Path::new("/srv/api"));

        let json = serde_json::to_string(&collapse).expect("collapse state should serialize");
        let restored: CollapseState =
            serde_json::from_str(&json).expect("collapse state should deserialize");

        assert!(restored.is_host_collapsed("remote-a"));
        assert!(restored.is_project_collapsed("remote-b", Path::new("/srv/repo")));
        assert!(restored.is_cwd_collapsed("remote-b", Path::new("/srv/api")));
        assert!(!restored.is_cwd_collapsed("remote-b", Path::new("/srv/other")));
    }

    #[test]
    fn deserializes_legacy_state_without_projects_field() {
        // Pre-project persisted state has no `projects` key. Emulate it by
        // serializing current state and dropping that field, then reload.
        let mut collapse = CollapseState::new();
        collapse.toggle_host("remote-a");
        collapse.toggle_cwd("remote-a", Path::new("/srv/app"));
        let mut value: serde_json::Value =
            serde_json::to_value(&collapse).expect("serialize to value");
        value
            .as_object_mut()
            .expect("state is an object")
            .remove("projects");

        let restored: CollapseState =
            serde_json::from_value(value).expect("legacy collapse state should deserialize");
        assert!(restored.is_host_collapsed("remote-a"));
        assert!(restored.is_cwd_collapsed("remote-a", Path::new("/srv/app")));
        assert!(!restored.is_project_collapsed("remote-a", Path::new("/srv/app")));
    }
}
