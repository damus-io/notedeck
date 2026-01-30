//! # notedeck_git
//!
//! A git collaboration viewer for Notedeck implementing NIP-34.
//!
//! This crate provides a GitHub/GitLab-style UI for viewing git events on nostr:
//! - Repository announcements (kind 30617)
//! - Repository state/branches (kind 30618)
//! - Patches (kind 1617)
//! - Pull requests (kind 1618, 1619)
//! - Issues (kind 1621)
//! - Status events (kinds 1630-1633)

mod events;
mod subscriptions;
mod ui;

pub use events::{
    GitEvent, GitIssue, GitPatch, GitPullRequest, GitRepo, GitStatus, RepoState, StatusKind,
};
pub use subscriptions::GitSubscriptions;
pub use ui::{GitAction, GitApp, GitResponse, GitRoute};
