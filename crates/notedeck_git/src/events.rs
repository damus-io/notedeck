//! NIP-34 event type definitions.
//!
//! This module defines Rust types for all git-related nostr events as specified in NIP-34.

use nostrdb::{Note, NoteKey};

/// Event kind constants for NIP-34 git events.
#[allow(dead_code)]
pub mod kinds {
    /// Repository announcement event kind.
    pub const REPO_ANNOUNCEMENT: u64 = 30617;
    /// Repository state event kind.
    pub const REPO_STATE: u64 = 30618;
    /// Patch event kind.
    pub const PATCH: u64 = 1617;
    /// Pull request event kind.
    pub const PULL_REQUEST: u64 = 1618;
    /// Pull request update event kind.
    pub const PULL_REQUEST_UPDATE: u64 = 1619;
    /// Issue event kind.
    pub const ISSUE: u64 = 1621;
    /// Status: Open.
    pub const STATUS_OPEN: u64 = 1630;
    /// Status: Applied/Merged/Resolved.
    pub const STATUS_APPLIED: u64 = 1631;
    /// Status: Closed.
    pub const STATUS_CLOSED: u64 = 1632;
    /// Status: Draft.
    pub const STATUS_DRAFT: u64 = 1633;
}

/// A git repository announced on nostr (kind 30617).
#[derive(Debug, Clone)]
pub struct GitRepo {
    /// The note key in nostrdb.
    pub key: NoteKey,
    /// Repository identifier (d tag), usually kebab-case.
    pub id: String,
    /// Human-readable project name.
    pub name: Option<String>,
    /// Brief project description.
    pub description: Option<String>,
    /// URLs for web browsing.
    pub web_urls: Vec<String>,
    /// URLs for git cloning.
    pub clone_urls: Vec<String>,
    /// Relays that monitor this repository.
    pub relays: Vec<String>,
    /// Earliest unique commit ID for identifying the repo across forks.
    pub earliest_unique_commit: Option<String>,
    /// Other recognized maintainers (pubkeys).
    pub maintainers: Vec<String>,
    /// Repository owner pubkey.
    pub owner: [u8; 32],
    /// Whether this is a personal fork.
    pub is_personal_fork: bool,
    /// Hashtag labels.
    pub labels: Vec<String>,
    /// Created timestamp.
    pub created_at: u64,
}

impl GitRepo {
    /// Parse a GitRepo from a nostrdb Note.
    ///
    /// Returns None if the note is not a valid repository announcement.
    #[profiling::function]
    pub fn from_note(note: &Note) -> Option<Self> {
        if note.kind() as u64 != kinds::REPO_ANNOUNCEMENT {
            return None;
        }

        let mut id = None;
        let mut name = None;
        let mut description = None;
        let mut web_urls = Vec::new();
        let mut clone_urls = Vec::new();
        let mut relays = Vec::new();
        let mut earliest_unique_commit = None;
        let mut maintainers = Vec::new();
        let mut is_personal_fork = false;
        let mut labels = Vec::new();

        for tag in note.tags() {
            let tag_name = tag.get(0).and_then(|t| t.variant().str());
            match tag_name {
                Some("d") => id = tag.get(1).and_then(|t| t.variant().str()).map(String::from),
                Some("name") => name = tag.get(1).and_then(|t| t.variant().str()).map(String::from),
                Some("description") => {
                    description = tag.get(1).and_then(|t| t.variant().str()).map(String::from)
                }
                Some("web") => {
                    if let Some(url) = tag.get(1).and_then(|t| t.variant().str()) {
                        web_urls.push(url.to_string());
                    }
                }
                Some("clone") => {
                    if let Some(url) = tag.get(1).and_then(|t| t.variant().str()) {
                        clone_urls.push(url.to_string());
                    }
                }
                Some("relays") => {
                    for i in 1..tag.count() {
                        if let Some(relay) = tag.get(i).and_then(|t| t.variant().str()) {
                            relays.push(relay.to_string());
                        }
                    }
                }
                Some("r") => {
                    // Check for "euc" marker indicating earliest unique commit
                    if tag.get(2).and_then(|t| t.variant().str()) == Some("euc") {
                        earliest_unique_commit =
                            tag.get(1).and_then(|t| t.variant().str()).map(String::from);
                    }
                }
                Some("maintainers") => {
                    for i in 1..tag.count() {
                        if let Some(pk) = tag.get(i).and_then(|t| t.variant().str()) {
                            maintainers.push(pk.to_string());
                        }
                    }
                }
                Some("t") => {
                    if let Some(label) = tag.get(1).and_then(|t| t.variant().str()) {
                        if label == "personal-fork" {
                            is_personal_fork = true;
                        } else {
                            labels.push(label.to_string());
                        }
                    }
                }
                _ => {}
            }
        }

        Some(GitRepo {
            key: note.key().expect("note should have key"),
            id: id?,
            name,
            description,
            web_urls,
            clone_urls,
            relays,
            earliest_unique_commit,
            maintainers,
            owner: *note.pubkey(),
            is_personal_fork,
            labels,
            created_at: note.created_at(),
        })
    }

    /// Get a display name for the repository.
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}

/// Repository state tracking branches and tags (kind 30618).
#[derive(Debug, Clone)]
pub struct RepoState {
    /// The note key in nostrdb.
    pub key: NoteKey,
    /// Repository identifier (d tag).
    pub repo_id: String,
    /// Branch references (name -> commit id).
    pub branches: Vec<(String, String)>,
    /// Tag references (name -> commit id).
    pub tags: Vec<(String, String)>,
    /// Current HEAD reference.
    pub head: Option<String>,
    /// Created timestamp.
    pub created_at: u64,
}

impl RepoState {
    /// Parse a RepoState from a nostrdb Note.
    #[profiling::function]
    pub fn from_note(note: &Note) -> Option<Self> {
        if note.kind() as u64 != kinds::REPO_STATE {
            return None;
        }

        let mut repo_id = None;
        let mut branches = Vec::new();
        let mut tags = Vec::new();
        let mut head = None;

        for tag in note.tags() {
            let tag_name = tag.get(0).and_then(|t| t.variant().str());
            match tag_name {
                Some("d") => repo_id = tag.get(1).and_then(|t| t.variant().str()).map(String::from),
                Some("HEAD") => head = tag.get(1).and_then(|t| t.variant().str()).map(String::from),
                Some(name) if name.starts_with("refs/heads/") => {
                    let branch_name = name.strip_prefix("refs/heads/").unwrap();
                    if let Some(commit) = tag.get(1).and_then(|t| t.variant().str()) {
                        branches.push((branch_name.to_string(), commit.to_string()));
                    }
                }
                Some(name) if name.starts_with("refs/tags/") => {
                    let tag_name = name.strip_prefix("refs/tags/").unwrap();
                    if let Some(commit) = tag.get(1).and_then(|t| t.variant().str()) {
                        tags.push((tag_name.to_string(), commit.to_string()));
                    }
                }
                _ => {}
            }
        }

        Some(RepoState {
            key: note.key().expect("note should have key"),
            repo_id: repo_id?,
            branches,
            tags,
            head,
            created_at: note.created_at(),
        })
    }
}

/// A git patch submitted to a repository (kind 1617).
#[derive(Debug, Clone)]
pub struct GitPatch {
    /// The note key in nostrdb.
    pub key: NoteKey,
    /// The patch content (git format-patch output).
    pub content: String,
    /// Repository address tag (30617:pubkey:repo-id).
    pub repo_address: Option<String>,
    /// Earliest unique commit of the target repo.
    pub repo_commit: Option<String>,
    /// Whether this is the root patch in a series.
    pub is_root: bool,
    /// Whether this is a root revision.
    pub is_root_revision: bool,
    /// Current commit ID if specified.
    pub commit_id: Option<String>,
    /// Parent commit ID if specified.
    pub parent_commit: Option<String>,
    /// Author pubkey.
    pub author: [u8; 32],
    /// Created timestamp.
    pub created_at: u64,
}

impl GitPatch {
    /// Parse a GitPatch from a nostrdb Note.
    #[profiling::function]
    pub fn from_note(note: &Note) -> Option<Self> {
        if note.kind() as u64 != kinds::PATCH {
            return None;
        }

        let mut repo_address = None;
        let mut repo_commit = None;
        let mut is_root = false;
        let mut is_root_revision = false;
        let mut commit_id = None;
        let mut parent_commit = None;

        for tag in note.tags() {
            let tag_name = tag.get(0).and_then(|t| t.variant().str());
            match tag_name {
                Some("a") => {
                    repo_address = tag.get(1).and_then(|t| t.variant().str()).map(String::from)
                }
                Some("r") => {
                    // Could be repo commit or commit ID reference
                    if repo_commit.is_none() {
                        repo_commit = tag.get(1).and_then(|t| t.variant().str()).map(String::from);
                    }
                }
                Some("t") => {
                    if let Some(label) = tag.get(1).and_then(|t| t.variant().str()) {
                        match label {
                            "root" => is_root = true,
                            "root-revision" => is_root_revision = true,
                            _ => {}
                        }
                    }
                }
                Some("commit") => {
                    commit_id = tag.get(1).and_then(|t| t.variant().str()).map(String::from)
                }
                Some("parent-commit") => {
                    parent_commit = tag.get(1).and_then(|t| t.variant().str()).map(String::from)
                }
                _ => {}
            }
        }

        Some(GitPatch {
            key: note.key().expect("note should have key"),
            content: note.content().to_string(),
            repo_address,
            repo_commit,
            is_root,
            is_root_revision,
            commit_id,
            parent_commit,
            author: *note.pubkey(),
            created_at: note.created_at(),
        })
    }

    /// Extract the subject line from the patch content.
    pub fn subject(&self) -> Option<&str> {
        // Look for Subject: line in git format-patch output
        for line in self.content.lines() {
            if let Some(subject) = line.strip_prefix("Subject: ") {
                // Remove [PATCH x/y] prefix if present
                let subject = subject.trim();
                if let Some(idx) = subject.find(']') {
                    return Some(subject[idx + 1..].trim());
                }
                return Some(subject);
            }
        }
        None
    }
}

/// A pull request (kind 1618).
#[derive(Debug, Clone)]
pub struct GitPullRequest {
    /// The note key in nostrdb.
    pub key: NoteKey,
    /// PR description (markdown).
    pub content: String,
    /// Repository address tag.
    pub repo_address: Option<String>,
    /// PR subject/title.
    pub subject: Option<String>,
    /// Labels.
    pub labels: Vec<String>,
    /// Tip commit ID.
    pub commit_id: Option<String>,
    /// Clone URLs to fetch the commit.
    pub clone_urls: Vec<String>,
    /// Recommended branch name.
    pub branch_name: Option<String>,
    /// Author pubkey.
    pub author: [u8; 32],
    /// Created timestamp.
    pub created_at: u64,
}

impl GitPullRequest {
    /// Parse a GitPullRequest from a nostrdb Note.
    #[profiling::function]
    pub fn from_note(note: &Note) -> Option<Self> {
        if note.kind() as u64 != kinds::PULL_REQUEST {
            return None;
        }

        let mut repo_address = None;
        let mut subject = None;
        let mut labels = Vec::new();
        let mut commit_id = None;
        let mut clone_urls = Vec::new();
        let mut branch_name = None;

        for tag in note.tags() {
            let tag_name = tag.get(0).and_then(|t| t.variant().str());
            match tag_name {
                Some("a") => {
                    repo_address = tag.get(1).and_then(|t| t.variant().str()).map(String::from)
                }
                Some("subject") => {
                    subject = tag.get(1).and_then(|t| t.variant().str()).map(String::from)
                }
                Some("t") => {
                    if let Some(label) = tag.get(1).and_then(|t| t.variant().str()) {
                        labels.push(label.to_string());
                    }
                }
                Some("c") => {
                    commit_id = tag.get(1).and_then(|t| t.variant().str()).map(String::from)
                }
                Some("clone") => {
                    if let Some(url) = tag.get(1).and_then(|t| t.variant().str()) {
                        clone_urls.push(url.to_string());
                    }
                }
                Some("branch-name") => {
                    branch_name = tag.get(1).and_then(|t| t.variant().str()).map(String::from)
                }
                _ => {}
            }
        }

        Some(GitPullRequest {
            key: note.key().expect("note should have key"),
            content: note.content().to_string(),
            repo_address,
            subject,
            labels,
            commit_id,
            clone_urls,
            branch_name,
            author: *note.pubkey(),
            created_at: note.created_at(),
        })
    }

    /// Get a display title for the PR.
    pub fn display_title(&self) -> &str {
        self.subject.as_deref().unwrap_or("Untitled PR")
    }
}

/// A git issue (kind 1621).
#[derive(Debug, Clone)]
pub struct GitIssue {
    /// The note key in nostrdb.
    pub key: NoteKey,
    /// Issue body (markdown).
    pub content: String,
    /// Repository address tag.
    pub repo_address: Option<String>,
    /// Issue subject/title.
    pub subject: Option<String>,
    /// Labels.
    pub labels: Vec<String>,
    /// Author pubkey.
    pub author: [u8; 32],
    /// Created timestamp.
    pub created_at: u64,
}

impl GitIssue {
    /// Parse a GitIssue from a nostrdb Note.
    #[profiling::function]
    pub fn from_note(note: &Note) -> Option<Self> {
        if note.kind() as u64 != kinds::ISSUE {
            return None;
        }

        let mut repo_address = None;
        let mut subject = None;
        let mut labels = Vec::new();

        for tag in note.tags() {
            let tag_name = tag.get(0).and_then(|t| t.variant().str());
            match tag_name {
                Some("a") => {
                    repo_address = tag.get(1).and_then(|t| t.variant().str()).map(String::from)
                }
                Some("subject") => {
                    subject = tag.get(1).and_then(|t| t.variant().str()).map(String::from)
                }
                Some("t") => {
                    if let Some(label) = tag.get(1).and_then(|t| t.variant().str()) {
                        labels.push(label.to_string());
                    }
                }
                _ => {}
            }
        }

        Some(GitIssue {
            key: note.key().expect("note should have key"),
            content: note.content().to_string(),
            repo_address,
            subject,
            labels,
            author: *note.pubkey(),
            created_at: note.created_at(),
        })
    }

    /// Get a display title for the issue.
    pub fn display_title(&self) -> &str {
        self.subject.as_deref().unwrap_or("Untitled Issue")
    }
}

/// Status event kind variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    /// Open (kind 1630).
    Open,
    /// Applied/Merged/Resolved (kind 1631).
    Applied,
    /// Closed (kind 1632).
    Closed,
    /// Draft (kind 1633).
    Draft,
}

impl StatusKind {
    /// Get the event kind number.
    pub fn kind_number(&self) -> u64 {
        match self {
            StatusKind::Open => kinds::STATUS_OPEN,
            StatusKind::Applied => kinds::STATUS_APPLIED,
            StatusKind::Closed => kinds::STATUS_CLOSED,
            StatusKind::Draft => kinds::STATUS_DRAFT,
        }
    }

    /// Parse from event kind number.
    pub fn from_kind(kind: u64) -> Option<Self> {
        match kind {
            kinds::STATUS_OPEN => Some(StatusKind::Open),
            kinds::STATUS_APPLIED => Some(StatusKind::Applied),
            kinds::STATUS_CLOSED => Some(StatusKind::Closed),
            kinds::STATUS_DRAFT => Some(StatusKind::Draft),
            _ => None,
        }
    }

    /// Get a display label.
    pub fn label(&self) -> &'static str {
        match self {
            StatusKind::Open => "Open",
            StatusKind::Applied => "Merged",
            StatusKind::Closed => "Closed",
            StatusKind::Draft => "Draft",
        }
    }
}

/// A status event for patches/PRs/issues (kinds 1630-1633).
#[derive(Debug, Clone)]
pub struct GitStatus {
    /// The note key in nostrdb.
    pub key: NoteKey,
    /// Status kind.
    pub status: StatusKind,
    /// Optional markdown content.
    pub content: String,
    /// The root event ID being status'd.
    pub target_event: Option<String>,
    /// Author pubkey.
    pub author: [u8; 32],
    /// Created timestamp.
    pub created_at: u64,
}

impl GitStatus {
    /// Parse a GitStatus from a nostrdb Note.
    #[profiling::function]
    pub fn from_note(note: &Note) -> Option<Self> {
        let status = StatusKind::from_kind(note.kind() as u64)?;

        let mut target_event = None;

        for tag in note.tags() {
            let tag_name = tag.get(0).and_then(|t| t.variant().str());
            if tag_name == Some("e") {
                // Look for root marker
                if tag.get(3).and_then(|t| t.variant().str()) == Some("root") {
                    target_event = tag.get(1).and_then(|t| t.variant().str()).map(String::from);
                }
            }
        }

        Some(GitStatus {
            key: note.key().expect("note should have key"),
            status,
            content: note.content().to_string(),
            target_event,
            author: *note.pubkey(),
            created_at: note.created_at(),
        })
    }
}

/// Enum representing any NIP-34 git event.
#[derive(Debug, Clone)]
pub enum GitEvent {
    /// Repository announcement.
    Repo(GitRepo),
    /// Repository state.
    State(RepoState),
    /// Patch.
    Patch(GitPatch),
    /// Pull request.
    PullRequest(GitPullRequest),
    /// Issue.
    Issue(GitIssue),
    /// Status event.
    Status(GitStatus),
}

impl GitEvent {
    /// Try to parse any git event from a nostrdb Note.
    #[profiling::function]
    pub fn from_note(note: &Note) -> Option<Self> {
        let kind = note.kind() as u64;

        match kind {
            kinds::REPO_ANNOUNCEMENT => GitRepo::from_note(note).map(GitEvent::Repo),
            kinds::REPO_STATE => RepoState::from_note(note).map(GitEvent::State),
            kinds::PATCH => GitPatch::from_note(note).map(GitEvent::Patch),
            kinds::PULL_REQUEST => GitPullRequest::from_note(note).map(GitEvent::PullRequest),
            kinds::ISSUE => GitIssue::from_note(note).map(GitEvent::Issue),
            kinds::STATUS_OPEN
            | kinds::STATUS_APPLIED
            | kinds::STATUS_CLOSED
            | kinds::STATUS_DRAFT => GitStatus::from_note(note).map(GitEvent::Status),
            _ => None,
        }
    }
}
