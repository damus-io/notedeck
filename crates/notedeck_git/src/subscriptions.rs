//! Git event subscription management.
//!
//! This module handles subscribing to relays for NIP-34 git events
//! using notedeck's existing subscription patterns.

use crate::events::{
    kinds, GitEvent, GitIssue, GitPatch, GitPullRequest, GitRepo, GitStatus, StatusKind,
};
use enostr::RelayPool;
use nostrdb::{Filter, Ndb, Note, Transaction};
use notedeck::UnifiedSubscription;
use std::collections::HashMap;
use uuid::Uuid;

/// Git-specific relays known to host NIP-34 events.
/// These are added to the relay pool when git subscriptions are initialized.
const GIT_RELAYS: &[&str] = &["wss://relay.ngit.dev", "wss://gitnostr.com"];

/// Manages subscriptions and caching for NIP-34 git events.
pub struct GitSubscriptions {
    /// Unified subscription for repository announcements.
    pub repo_sub: Option<UnifiedSubscription>,
    /// Unified subscription for patches, PRs, issues, and status events.
    pub events_sub: Option<UnifiedSubscription>,
    /// Cached repositories.
    pub repos: Vec<GitRepo>,
    /// Cached issues by repo address.
    pub issues: HashMap<String, Vec<GitIssue>>,
    /// Cached patches by repo address.
    pub patches: HashMap<String, Vec<GitPatch>>,
    /// Cached pull requests by repo address.
    pub pull_requests: HashMap<String, Vec<GitPullRequest>>,
    /// Status events by target event ID (hex encoded note key).
    pub statuses: HashMap<String, GitStatus>,
}

impl Default for GitSubscriptions {
    fn default() -> Self {
        Self::new()
    }
}

impl GitSubscriptions {
    /// Create a new GitSubscriptions instance.
    pub fn new() -> Self {
        Self {
            repo_sub: None,
            events_sub: None,
            repos: Vec::new(),
            issues: HashMap::new(),
            patches: HashMap::new(),
            pull_requests: HashMap::new(),
            statuses: HashMap::new(),
        }
    }

    /// Initialize subscriptions for git events.
    ///
    /// This subscribes to both local nostrdb and remote relays.
    /// Also adds git-specific relays if not already present.
    #[profiling::function]
    pub fn subscribe(
        &mut self,
        ndb: &Ndb,
        pool: &mut RelayPool,
        wakeup: impl Fn() + Send + Sync + Clone + 'static,
    ) {
        if self.repo_sub.is_some() {
            return; // Already subscribed
        }

        // Add git-specific relays if not already in pool
        for relay_url in GIT_RELAYS {
            if !pool.has(relay_url) {
                if let Err(e) = pool.add_url((*relay_url).to_string(), wakeup.clone()) {
                    tracing::warn!("Failed to add git relay {}: {}", relay_url, e);
                } else {
                    tracing::info!("Added git relay: {}", relay_url);
                }
            }
        }

        tracing::info!(
            "Initializing git subscriptions, pool has {} relays: {:?}",
            pool.relays.len(),
            pool.urls()
        );

        // Filter for repository announcements
        let repo_filter = vec![Filter::new()
            .kinds([kinds::REPO_ANNOUNCEMENT])
            .limit(500)
            .build()];

        // Filter for all other git events
        let events_filter = vec![
            Filter::new().kinds([kinds::PATCH]).limit(500).build(),
            Filter::new()
                .kinds([kinds::PULL_REQUEST])
                .limit(500)
                .build(),
            Filter::new().kinds([kinds::ISSUE]).limit(500).build(),
            Filter::new()
                .kinds([
                    kinds::STATUS_OPEN,
                    kinds::STATUS_APPLIED,
                    kinds::STATUS_CLOSED,
                    kinds::STATUS_DRAFT,
                ])
                .limit(1000)
                .build(),
        ];

        // Subscribe locally to nostrdb
        if let Ok(repo_local_sub) = ndb.subscribe(&repo_filter) {
            let repo_remote_id = Uuid::new_v4().to_string();
            pool.subscribe(repo_remote_id.clone(), repo_filter);

            self.repo_sub = Some(UnifiedSubscription {
                local: repo_local_sub,
                remote: repo_remote_id,
            });

            tracing::info!("Subscribed to NIP-34 repository announcements");
        }

        if let Ok(events_local_sub) = ndb.subscribe(&events_filter) {
            let events_remote_id = Uuid::new_v4().to_string();
            pool.subscribe(events_remote_id.clone(), events_filter);

            self.events_sub = Some(UnifiedSubscription {
                local: events_local_sub,
                remote: events_remote_id,
            });

            tracing::info!("Subscribed to NIP-34 patches/PRs/issues/status events");
        }

        // Do initial query from existing data
        self.refresh_from_db(ndb);
    }

    /// Poll for new notes from subscriptions.
    #[profiling::function]
    pub fn poll(&mut self, ndb: &Ndb) {
        let mut new_note_keys = Vec::new();

        // Poll repo subscription
        if let Some(sub) = &self.repo_sub {
            let repo_notes = ndb.poll_for_notes(sub.local, 100);
            if !repo_notes.is_empty() {
                tracing::debug!("Polled {} repo announcement notes", repo_notes.len());
            }
            new_note_keys.extend(repo_notes);
        }

        // Poll events subscription
        if let Some(sub) = &self.events_sub {
            let event_notes = ndb.poll_for_notes(sub.local, 500);
            if !event_notes.is_empty() {
                tracing::debug!("Polled {} git event notes", event_notes.len());
            }
            new_note_keys.extend(event_notes);
        }

        if new_note_keys.is_empty() {
            return;
        }

        tracing::info!("Processing {} new git notes", new_note_keys.len());

        let Ok(txn) = Transaction::new(ndb) else {
            return;
        };

        for note_key in new_note_keys {
            if let Ok(note) = ndb.get_note_by_key(&txn, note_key) {
                self.process_note(&note);
            }
        }

        // Sort after processing new notes
        self.sort_caches();
    }

    /// Refresh all caches from the database.
    #[profiling::function]
    pub fn refresh_from_db(&mut self, ndb: &Ndb) {
        let Ok(txn) = Transaction::new(ndb) else {
            return;
        };

        // Clear existing caches
        self.repos.clear();
        self.issues.clear();
        self.patches.clear();
        self.pull_requests.clear();
        self.statuses.clear();

        // Query repositories
        let repo_filter = Filter::new()
            .kinds([kinds::REPO_ANNOUNCEMENT])
            .limit(500)
            .build();

        match ndb.query(&txn, &[repo_filter], 500) {
            Ok(results) => {
                tracing::info!(
                    "refresh_from_db: queried {} repo announcements from ndb",
                    results.len()
                );
                for result in results {
                    if let Ok(note) = ndb.get_note_by_key(&txn, result.note_key) {
                        self.process_note(&note);
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Failed to query repos from ndb: {:?}", e);
            }
        }

        // Query patches
        let patch_filter = Filter::new().kinds([kinds::PATCH]).limit(500).build();
        if let Ok(results) = ndb.query(&txn, &[patch_filter], 500) {
            for result in results {
                if let Ok(note) = ndb.get_note_by_key(&txn, result.note_key) {
                    self.process_note(&note);
                }
            }
        }

        // Query PRs
        let pr_filter = Filter::new()
            .kinds([kinds::PULL_REQUEST])
            .limit(500)
            .build();
        if let Ok(results) = ndb.query(&txn, &[pr_filter], 500) {
            for result in results {
                if let Ok(note) = ndb.get_note_by_key(&txn, result.note_key) {
                    self.process_note(&note);
                }
            }
        }

        // Query issues
        let issue_filter = Filter::new().kinds([kinds::ISSUE]).limit(500).build();
        if let Ok(results) = ndb.query(&txn, &[issue_filter], 500) {
            for result in results {
                if let Ok(note) = ndb.get_note_by_key(&txn, result.note_key) {
                    self.process_note(&note);
                }
            }
        }

        // Query status events
        let status_filter = Filter::new()
            .kinds([
                kinds::STATUS_OPEN,
                kinds::STATUS_APPLIED,
                kinds::STATUS_CLOSED,
                kinds::STATUS_DRAFT,
            ])
            .limit(1000)
            .build();
        if let Ok(results) = ndb.query(&txn, &[status_filter], 1000) {
            for result in results {
                if let Ok(note) = ndb.get_note_by_key(&txn, result.note_key) {
                    self.process_note(&note);
                }
            }
        }

        self.sort_caches();
    }

    /// Process a single note and add to appropriate cache.
    fn process_note(&mut self, note: &Note) {
        tracing::debug!(
            "Processing note kind={} id={}",
            note.kind(),
            hex::encode(note.id())
        );

        let Some(event) = GitEvent::from_note(note) else {
            tracing::debug!("Note kind {} not a valid git event", note.kind());
            return;
        };

        match event {
            GitEvent::Repo(repo) => {
                tracing::info!("Found git repo: {}", repo.display_name());
                // Avoid duplicates
                if !self.repos.iter().any(|r| r.key == repo.key) {
                    self.repos.push(repo);
                }
            }
            GitEvent::Patch(patch) => {
                if let Some(addr) = &patch.repo_address {
                    let patches = self.patches.entry(addr.clone()).or_default();
                    if !patches.iter().any(|p| p.key == patch.key) {
                        patches.push(patch);
                    }
                }
            }
            GitEvent::PullRequest(pr) => {
                if let Some(addr) = &pr.repo_address {
                    let prs = self.pull_requests.entry(addr.clone()).or_default();
                    if !prs.iter().any(|p| p.key == pr.key) {
                        prs.push(pr);
                    }
                }
            }
            GitEvent::Issue(issue) => {
                if let Some(addr) = &issue.repo_address {
                    let issues = self.issues.entry(addr.clone()).or_default();
                    if !issues.iter().any(|i| i.key == issue.key) {
                        issues.push(issue);
                    }
                }
            }
            GitEvent::Status(status) => {
                if let Some(target) = &status.target_event {
                    // Keep most recent status per target
                    let entry = self
                        .statuses
                        .entry(target.clone())
                        .or_insert(status.clone());
                    if status.created_at > entry.created_at {
                        *entry = status;
                    }
                }
            }
            GitEvent::State(_) => {
                // TODO: Handle repo state events
            }
        }
    }

    /// Sort all caches by created_at descending.
    fn sort_caches(&mut self) {
        self.repos.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        for patches in self.patches.values_mut() {
            patches.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        }

        for prs in self.pull_requests.values_mut() {
            prs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        }

        for issues in self.issues.values_mut() {
            issues.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        }
    }

    /// Get the status for a note (by hex-encoded note ID).
    pub fn get_status(&self, note_id: &str) -> Option<StatusKind> {
        self.statuses.get(note_id).map(|s| s.status)
    }

    /// Get issues for a repository address.
    pub fn get_issues(&self, repo_address: &str) -> Option<&Vec<GitIssue>> {
        self.issues.get(repo_address)
    }

    /// Get patches for a repository address.
    pub fn get_patches(&self, repo_address: &str) -> Option<&Vec<GitPatch>> {
        self.patches.get(repo_address)
    }

    /// Get pull requests for a repository address.
    pub fn get_pull_requests(&self, repo_address: &str) -> Option<&Vec<GitPullRequest>> {
        self.pull_requests.get(repo_address)
    }
}
