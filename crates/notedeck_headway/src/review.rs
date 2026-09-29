//! Loading a card's review commit for the review pane, off the UI thread.
//!
//! [`headway::git::resolve`] and [`headway::git::commit_patch`] shell out to
//! `git`, and resolving a commit recorded on another host means a `git fetch`
//! over ssh that can take seconds. None of that may run in the render loop, so
//! each load gets its own thread (a fetch would starve the 2-thread
//! [`JobPool`](notedeck::JobPool) of the blurhash work it exists for), reports
//! back over a channel the pane drains each frame, and wakes the host so the
//! result draws without waiting for input — the same shape as Dave's
//! `git_status`.
//!
//! Results are cached per [`ReviewSource`] for the life of the app, parsed into a
//! [`GitPatch`] once on the worker, so stepping between a card's records (or
//! backing out and in again) never re-runs git. A failed load stays cached too,
//! until the pane asks to [`retry`](ReviewLoader::retry) it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use headway::event::ReviewFields;
use headway::git::{self, CommitPatch, Found, GitError, ResolveCtx, Resolved};
use nostrdb_net::NoteId;
use notedeck::{Localization, Waker};
use notedeck_ui::diff::{GitPatch, GitPatchState};

/// The patch size the pane loads before cutting it off (as `headway diff`).
const MAX_PATCH_BYTES: usize = 4 << 20;

/// How long one `git fetch` from another host may take (as `headway diff`).
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// What a load resolves: one review record, or — for a card with none — the
/// card's `Headway:` trailer. The cache key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ReviewSource {
    /// The review record with this note id.
    Record(NoteId),
    /// The newest commit carrying this card's `Headway:` trailer.
    Trailer(NoteId),
}

/// Everything a load needs, owned so it can move onto the worker thread.
pub(crate) struct ReviewJob {
    /// The record to resolve; `None` searches by trailer alone.
    pub record: Option<ReviewFields>,
    /// The card's `headway:<board>/<word-id>`, for the trailer search.
    pub card_ref: String,
    /// Checkouts on this host to try before the bare cache (see
    /// [`git::known_checkouts`]).
    pub checkouts: Vec<PathBuf>,
    /// Where the headway-owned bare cache repos live.
    pub cache_root: PathBuf,
}

/// One load's state, as the pane draws it.
pub(crate) enum ReviewLoad {
    /// The worker is still running. `note` says what it's doing, formatted once
    /// when the load started.
    Pending { note: String },
    /// The commit was found and read back.
    Ready(Box<LoadedReview>),
    /// git failed; the pane shows the command and git's stderr verbatim.
    Failed(GitError),
}

/// A commit the worker found and read, with the view state its diff scrolls in.
pub(crate) struct LoadedReview {
    /// Where the commit was found, e.g. `fetched from jex0:repos/notedeck into …`.
    pub found: String,
    /// It was found by its `Headway:` trailer, so its hash may not be the one a
    /// record names (a rebase).
    pub by_trailer: bool,
    /// `author · date`, formatted once.
    pub byline: String,
    pub commit: CommitPatch,
    pub patch: GitPatch,
    pub patch_state: GitPatchState,
}

/// What the worker sends back: the resolution and the parsed patch, before the
/// UI thread attaches the localized [`GitPatchState`].
struct Fetched {
    resolved: Resolved,
    commit: CommitPatch,
    patch: GitPatch,
}

/// The review pane's loads: started on demand, cached per [`ReviewSource`], and
/// drained from the worker threads by [`poll`](Self::poll).
pub(crate) struct ReviewLoader {
    /// This host's name, looked up on first use.
    local_host: Option<String>,
    loads: HashMap<ReviewSource, ReviewLoad>,
    tx: Sender<(ReviewSource, Result<Fetched, GitError>)>,
    rx: Receiver<(ReviewSource, Result<Fetched, GitError>)>,
}

impl Default for ReviewLoader {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            local_host: None,
            loads: HashMap::new(),
            tx,
            rx,
        }
    }
}

impl ReviewLoader {
    /// This host's name, as review records' `host` is compared against. Looked
    /// up once; empty when the OS gives back nothing usable.
    pub(crate) fn local_host(&mut self) -> &str {
        self.local_host
            .get_or_insert_with(|| git::host_name().unwrap_or_default())
    }

    /// Whether `source` has been started (pending, loaded or failed).
    pub(crate) fn contains(&self, source: ReviewSource) -> bool {
        self.loads.contains_key(&source)
    }

    /// Start loading `source` on its own thread, unless it already has been.
    /// `waker` is woken when the result is ready to draw.
    pub(crate) fn start(&mut self, source: ReviewSource, job: ReviewJob, waker: Waker) {
        if self.contains(source) {
            return;
        }
        let local_host = self.local_host().to_string();
        let note = pending_note(job.record.as_ref(), &local_host);
        self.loads.insert(source, ReviewLoad::Pending { note });

        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = load(&job, &local_host);
            // The loader outlives every worker it spawned unless the app is
            // shutting down, when nobody is left to tell.
            let _ = tx.send((source, result));
            waker.wake();
        });
    }

    /// Drop `source`'s result so the next frame starts it again. A no-op while
    /// it's still pending, so a double-click can't run two fetches at once.
    pub(crate) fn retry(&mut self, source: ReviewSource) {
        if !matches!(self.loads.get(&source), Some(ReviewLoad::Pending { .. })) {
            self.loads.remove(&source);
        }
    }

    /// Take every result the workers have sent since the last frame. Each
    /// patch's view state is built here, on the UI thread, where the
    /// localization it formats its labels with lives.
    pub(crate) fn poll(&mut self, i18n: &mut Localization) {
        while let Ok((source, result)) = self.rx.try_recv() {
            let load = match result {
                Ok(fetched) => ReviewLoad::Ready(Box::new(loaded(fetched, i18n))),
                Err(e) => ReviewLoad::Failed(e),
            };
            self.loads.insert(source, load);
        }
    }

    /// `source`'s load, if it has been started.
    pub(crate) fn get_mut(&mut self, source: ReviewSource) -> Option<&mut ReviewLoad> {
        self.loads.get_mut(&source)
    }
}

/// The worker: resolve the commit (fetching it if needed), then read it back.
fn load(job: &ReviewJob, local_host: &str) -> Result<Fetched, GitError> {
    let resolved = match &job.record {
        Some(record) => {
            let ctx = ResolveCtx {
                local_host,
                cache_root: &job.cache_root,
                known_checkouts: &job.checkouts,
                timeout: FETCH_TIMEOUT,
            };
            git::resolve(record, &job.card_ref, &ctx)?
        }
        None => git::resolve_by_trailer(&job.card_ref, &job.checkouts).ok_or_else(|| GitError {
            command: format!("log --all --grep='Headway: {}'", job.card_ref),
            stderr: format!(
                "no review record, and no commit in {} known checkout(s) on this \
                 host carries a 'Headway: {}' trailer",
                job.checkouts.len(),
                job.card_ref
            ),
        })?,
    };
    let commit = git::commit_patch(&resolved.repo_dir, &resolved.sha, MAX_PATCH_BYTES)?;
    let patch = GitPatch::parse(commit.patch.clone());
    Ok(Fetched {
        resolved,
        commit,
        patch,
    })
}

/// A worker's result, with the view state for its diff attached.
fn loaded(fetched: Fetched, i18n: &mut Localization) -> LoadedReview {
    let Fetched {
        resolved,
        commit,
        patch,
    } = fetched;
    LoadedReview {
        found: resolved.to_string(),
        by_trailer: resolved.how == Found::ByTrailer,
        byline: format!("{} · {}", commit.author, commit.date),
        patch_state: GitPatchState::new(&patch, i18n),
        commit,
        patch,
    }
}

/// What a pending load says it's doing. A record made on another host will
/// usually need a fetch from it, which is the slow part worth naming; the
/// resolver decides for itself, so this is a hint, not a promise.
fn pending_note(record: Option<&ReviewFields>, local_host: &str) -> String {
    let Some(record) = record else {
        return "searching local checkouts for the card's Headway trailer…".to_string();
    };
    let sha = record.commit.as_deref().map_or("the commit", short_sha);
    match record.host.as_deref() {
        Some(host) if host != local_host => {
            let path = record.path.as_deref().unwrap_or("");
            format!("resolving {sha}… (recorded on {host}:{path}, may fetch from there)")
        }
        _ => format!("resolving {sha}…"),
    }
}

/// The first 12 characters of a sha (or all of a shorter one), as the pane and
/// the CLI abbreviate it.
pub(crate) fn short_sha(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record from another host names the host it may fetch from; one made
    /// here, or with no host, just says it's resolving; no record at all is the
    /// trailer search.
    #[test]
    fn pending_note_names_the_remote_host() {
        let record = ReviewFields {
            commit: Some("136ceb9d3bfa0123456789".to_string()),
            host: Some("jex0".to_string()),
            path: Some("/home/jb55/dev/notedeck".to_string()),
            ..Default::default()
        };
        assert_eq!(
            pending_note(Some(&record), "quiver"),
            "resolving 136ceb9d3bfa… (recorded on jex0:/home/jb55/dev/notedeck, may fetch from there)"
        );
        assert_eq!(
            pending_note(Some(&record), "jex0"),
            "resolving 136ceb9d3bfa…"
        );
        assert!(pending_note(None, "jex0").contains("Headway trailer"));
    }

    /// Short shas are cut to 12, shorter strings pass through.
    #[test]
    fn short_sha_cuts_to_twelve() {
        assert_eq!(short_sha("136ceb9d3bfa0123"), "136ceb9d3bfa");
        assert_eq!(short_sha("abc"), "abc");
    }
}
