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
//! Results are cached per [`ReviewSource`], parsed into a [`GitPatch`] once on
//! the worker, so stepping between a card's records (or backing out and in
//! again) doesn't re-run git. A cached load goes stale by [`stale_in`]: a failed
//! one after [`FAILED_BACKOFF`] (a refused ssh is often back a moment later), a
//! trailer search whenever the pane is re-opened (a rebase moves the commit it
//! finds, and nothing on the card changes to say so), and every settled load of
//! a card once that card's review records change. A record's own load is
//! otherwise kept: the record pins its sha. The pane can still ask to
//! [`retry`](ReviewLoader::retry) a load by hand.
//!
//! A record missing an explainer source or a code walk has the commit's explain
//! git notes read in the same load ([`git::fetch_notes`]), which can fetch
//! from the host the commit came from; the pane draws a note wherever the
//! record has no source of its own ([`review_walk::source_text`]).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use headway::event::{ReviewFields, ReviewSourceRole, ReviewView};
use headway::git::{self, CommitNotes, CommitPatch, Found, GitError, ResolveCtx, Resolved};
use nostrdb_net::NoteId;
use notedeck::{Localization, Renderer3d, Waker};
use notedeck_ui::diff::{GitPatch, GitPatchState};

use crate::review_body::CommitBody;
use crate::review_images::{self, FetchedImages};
use crate::review_models::{self, FetchedModels, ReviewModels};
use crate::review_walk::{self, CodeWalk, ReviewNotes, SourceKey};

/// The patch size the pane loads before cutting it off (as `headway diff`).
const MAX_PATCH_BYTES: usize = 4 << 20;

/// How long one `git fetch` from another host may take (as `headway diff`).
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// How long reading the commit's explain notes from another host may take.
/// Shorter than [`FETCH_TIMEOUT`]: the diff waits on it, and the notes only
/// stand in for sources the record hasn't got.
const NOTES_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a failed load is shown before the pane starts it again on its own.
const FAILED_BACKOFF: Duration = Duration::from_secs(30);

/// What a load resolves: one review record, or — for a card with none — the
/// card's `Headway:` trailer. The cache key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ReviewSource {
    /// The review record `record`, on card `card`.
    Record { card: NoteId, record: NoteId },
    /// The newest commit carrying this card's `Headway:` trailer.
    Trailer(NoteId),
}

impl ReviewSource {
    /// The card this load belongs to.
    fn card(self) -> NoteId {
        match self {
            Self::Record { card, .. } | Self::Trailer(card) => card,
        }
    }
}

/// Everything a load needs, owned so it can move onto the worker thread.
pub(crate) struct ReviewJob {
    /// The record to resolve; `None` searches by trailer alone.
    pub record: Option<ReviewFields>,
    /// Read the commit's explain notes too: the record lacks a source they
    /// could stand in for (see [`wants_notes`]).
    pub notes: bool,
    /// The card's `headway:<board>/<word-id>`, for the trailer search.
    pub card_ref: String,
    /// Checkouts on this host to try before the bare cache (see
    /// [`git::known_checkouts`]).
    pub checkouts: Vec<PathBuf>,
    /// Where the headway-owned bare cache repos live.
    pub cache_root: PathBuf,
    /// The host's 3D renderer, to load the commit's changed models with;
    /// `None` leaves them "Binary file not shown".
    pub renderer: Option<Renderer3d>,
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

impl ReviewLoad {
    /// Which of the three states this is, for [`stale_in`].
    fn kind(&self) -> LoadKind {
        match self {
            Self::Pending { .. } => LoadKind::Pending,
            Self::Ready(_) => LoadKind::Ready,
            Self::Failed(_) => LoadKind::Failed,
        }
    }
}

/// A [`ReviewLoad`]'s state without its payload, so the staleness decision is
/// a pure function tests can drive without a worker or a patch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoadKind {
    Pending,
    Ready,
    Failed,
}

/// A cached load and when it settled (when it started, while pending).
struct CachedLoad {
    load: ReviewLoad,
    at: Instant,
}

/// Enough of a card's review records to notice they changed, without keeping
/// them: records are append-only and sorted newest first, so a new one changes
/// the count and the head, and a collapse of duplicates changes the count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RecordSet {
    len: usize,
    newest: Option<NoteId>,
}

impl RecordSet {
    /// The fingerprint of `reviews` (a card's records, newest first).
    pub(crate) fn of(reviews: &[ReviewView]) -> Self {
        Self {
            len: reviews.len(),
            newest: reviews.first().map(|r| r.id),
        }
    }
}

/// How long until `source`'s cached load, of `kind` and settled at `at`, goes
/// stale: `Some(ZERO)` if it already is, `Some(d)` if it will be in `d` (so the
/// pane can schedule the frame that restarts it), `None` if time alone never
/// makes it stale. `reopened` is whether the pane was just opened onto it.
///
/// - Pending never is — restarting it would run a second fetch alongside the
///   first.
/// - Failed is after [`FAILED_BACKOFF`], so a flaky ssh recovers on its own.
/// - Ready from a trailer search is on re-open: nothing on the card changes
///   when the commit it found is rebased, so the only cue is the reviewer
///   coming back. Not on a timer, which would yank a diff away mid-read.
/// - Ready from a record never is: the record names its sha, and a new
///   record is caught by [`ReviewLoader::note_records`] instead.
fn stale_in(
    kind: LoadKind,
    source: ReviewSource,
    at: Instant,
    now: Instant,
    reopened: bool,
) -> Option<Duration> {
    match (kind, source) {
        (LoadKind::Pending, _) => None,
        (LoadKind::Failed, _) => {
            Some(FAILED_BACKOFF.saturating_sub(now.saturating_duration_since(at)))
        }
        (LoadKind::Ready, ReviewSource::Trailer(_)) => reopened.then_some(Duration::ZERO),
        (LoadKind::Ready, ReviewSource::Record { .. }) => None,
    }
}

/// A commit the worker found and read, with the view state its diff scrolls in.
pub(crate) struct LoadedReview {
    /// Where the commit came from in a few words, e.g. `local checkout` (see
    /// [`Resolved::source_label`]).
    pub source: String,
    /// The full sentence behind [`source`](Self::source), with the repo's
    /// path, e.g. `fetched from jex0:repos/notedeck into …`; shown on hover.
    pub source_hover: String,
    /// It was found by its `Headway:` trailer, so its hash may not be the one a
    /// record names (a rebase).
    pub by_trailer: bool,
    /// The commit's author and when, e.g. `William Casarin · 1d ago`.
    pub byline: Byline,
    /// The commit message under its subject, with its trailers split out;
    /// parsed from [`CommitPatch::message`] once, when the load lands.
    pub body: CommitBody,
    pub commit: CommitPatch,
    pub patch: GitPatch,
    pub patch_state: GitPatchState,
    /// The views of the commit's changed 3D models, turned by drags the
    /// patch view reports; `None` when it changes none (or there's no
    /// renderer).
    pub models: Option<ReviewModels>,
    /// The record's code walk, threaded into this diff; resolved when the
    /// pane first draws the record's comments, and again when its source
    /// changes (see [`sync_walk`](Self::sync_walk)). `None` when neither the
    /// record nor the commit's note has a walk.
    pub walk: Option<CodeWalk>,
    /// The commit's explain notes, for the sources the record hasn't got;
    /// empty when the record has both, or the commit has none.
    pub notes: ReviewNotes,
}

impl LoadedReview {
    /// Resolve the walk against this diff when its source changed: `record`'s
    /// code walk, else the commit's note. Returns the walk's key, `None` when
    /// there is none. A key compare on every other frame.
    pub(crate) fn sync_walk(&mut self, record: Option<&ReviewView>) -> Option<SourceKey> {
        let code = review_walk::source_text(record, Some(&self.notes), ReviewSourceRole::Code);
        let key = code.map(|c| c.key);
        if self.walk.as_ref().map(|w| w.source) != key {
            self.walk = code.map(|c| CodeWalk::resolve(c.text, c.key, &self.patch));
        }
        key
    }
}

/// Whether a load of `record` should read the commit's explain notes: it
/// lacks a source a note could stand in for. A card found by its trailer has
/// no record to draw them on, so it doesn't.
pub(crate) fn wants_notes(record: Option<&ReviewView>) -> bool {
    record.is_some_and(|r| {
        [ReviewSourceRole::Explainer, ReviewSourceRole::Code]
            .into_iter()
            .any(|role| r.source(role).is_none())
    })
}

/// A commit's author line, formatted once when its load lands rather than
/// every frame.
pub(crate) struct Byline {
    /// The author's name and a relative date: `William Casarin · 1d ago`.
    pub short: String,
    /// The full name, email and ISO date, for hover:
    /// `William Casarin <jb55@jb55.com> · 2026-09-29T02:14:33-07:00`.
    pub full: String,
}

impl Byline {
    /// `commit`'s byline, its relative date measured against
    /// [`headway::fmt::rel_time`]'s clock (frozen in tests).
    fn of(commit: &CommitPatch) -> Self {
        Self {
            short: format!(
                "{} · {}",
                commit.author_name(),
                headway::fmt::rel_time(commit.time)
            ),
            full: format!("{} · {}", commit.author, commit.date),
        }
    }
}

/// What the worker sends back: the resolution, the parsed patch, its changed
/// images decoded and its changed models uploaded, before the UI thread
/// attaches the localized [`GitPatchState`], uploads the images and makes
/// the models' views.
struct Fetched {
    resolved: Resolved,
    notes: CommitNotes,
    commit: CommitPatch,
    patch: GitPatch,
    images: Vec<FetchedImages>,
    models: Vec<FetchedModels>,
}

/// The review pane's loads: started on demand, cached per [`ReviewSource`], and
/// drained from the worker threads by [`poll`](Self::poll).
pub(crate) struct ReviewLoader {
    /// This host's name, looked up on first use.
    local_host: Option<String>,
    loads: HashMap<ReviewSource, CachedLoad>,
    /// Each card's records as last seen, to drop its loads when they change.
    records: HashMap<NoteId, RecordSet>,
    tx: Sender<(ReviewSource, Result<Fetched, GitError>)>,
    rx: Receiver<(ReviewSource, Result<Fetched, GitError>)>,
}

impl Default for ReviewLoader {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            local_host: None,
            loads: HashMap::new(),
            records: HashMap::new(),
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
        let pending = CachedLoad {
            load: ReviewLoad::Pending { note },
            at: Instant::now(),
        };
        self.loads.insert(source, pending);

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
        if !matches!(self.loads.get(&source), Some(c) if c.load.kind() == LoadKind::Pending) {
            self.loads.remove(&source);
        }
    }

    /// Drop `source`'s load if it has gone stale by [`stale_in`], so the frame
    /// that follows starts it again. Returns how long until it will be stale,
    /// for the pane to schedule a repaint at, or `None` when only an event
    /// (a re-open, a new record) could make it so. A map lookup; allocates
    /// nothing.
    pub(crate) fn expire(
        &mut self,
        source: ReviewSource,
        now: Instant,
        reopened: bool,
    ) -> Option<Duration> {
        let cached = self.loads.get(&source)?;
        let left = stale_in(cached.load.kind(), source, cached.at, now, reopened)?;
        if left.is_zero() {
            self.loads.remove(&source);
            return None;
        }
        Some(left)
    }

    /// Record `card`'s review records as `seen` this frame. When they differ
    /// from the last time the card was seen, every settled load of the card is
    /// dropped: the picker's indices now point at other records, and a card
    /// that gained its first record has no use for its trailer search. Pending
    /// loads are kept (no double fetch); their results land as usual. The first
    /// sight of a card only remembers it.
    pub(crate) fn note_records(&mut self, card: NoteId, seen: RecordSet) {
        let Some(last) = self.records.insert(card, seen) else {
            return;
        };
        if last == seen {
            return;
        }
        self.loads.retain(|source, cached| {
            source.card() != card || cached.load.kind() == LoadKind::Pending
        });
    }

    /// Take every result the workers have sent since the last frame. Each
    /// patch's view state is built here, on the UI thread, where the
    /// localization it formats its labels with lives, its images are
    /// uploaded to `ctx` and its models' views made on `renderer`, once.
    pub(crate) fn poll(
        &mut self,
        ctx: &egui::Context,
        renderer: Option<&Renderer3d>,
        i18n: &mut Localization,
    ) {
        while let Ok((source, result)) = self.rx.try_recv() {
            let load = match result {
                Ok(fetched) => {
                    ReviewLoad::Ready(Box::new(loaded(fetched, source, ctx, renderer, i18n)))
                }
                Err(e) => ReviewLoad::Failed(e),
            };
            self.loads.insert(
                source,
                CachedLoad {
                    load,
                    at: Instant::now(),
                },
            );
        }
    }

    /// `source`'s load, if it has been started.
    pub(crate) fn get(&self, source: ReviewSource) -> Option<&ReviewLoad> {
        self.loads.get(&source).map(|c| &c.load)
    }

    /// `source`'s load, if it has been started, to change.
    pub(crate) fn get_mut(&mut self, source: ReviewSource) -> Option<&mut ReviewLoad> {
        self.loads.get_mut(&source).map(|c| &mut c.load)
    }
}

/// The worker: resolve the commit (fetching it if needed), then read it back.
fn load(job: &ReviewJob, local_host: &str) -> Result<Fetched, GitError> {
    let mut notes = CommitNotes::default();
    let resolved = match &job.record {
        Some(record) => {
            let ctx = ResolveCtx {
                local_host,
                cache_root: &job.cache_root,
                known_checkouts: &job.checkouts,
                timeout: FETCH_TIMEOUT,
            };
            let resolved = git::resolve(record, &job.card_ref, &ctx)?;
            if job.notes {
                let src = git::fetch_source(record, &resolved.repo_dir, &ctx);
                // Notes are extra: a failed read leaves the record's sources
                // as they are rather than failing the diff.
                notes = git::fetch_notes(&resolved, src.as_deref(), NOTES_TIMEOUT).unwrap_or_else(
                    |e| {
                        tracing::debug!("explain notes of {}: {e}", resolved.sha);
                        CommitNotes::default()
                    },
                );
            }
            resolved
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
    let images = review_images::fetch(&resolved.repo_dir, &resolved.sha, &patch);
    let models = review_models::fetch(
        &resolved.repo_dir,
        &resolved.sha,
        &patch,
        job.renderer.as_ref(),
    );
    Ok(Fetched {
        resolved,
        notes,
        commit,
        patch,
        images,
        models,
    })
}

/// A worker's result, with the view state for its diff attached. Runs on the
/// UI thread, so the byline's relative date reads the thread's frozen clock
/// in tests.
///
/// The diff's scroll is salted with `source` and the commit's sha: the pane
/// draws every card's diff at the same place, so with one shared salt the next
/// card in the queue would open at the last one's offset. Per diff, a new card
/// opens at the top and stepping back to one returns to where it was left. The
/// sha is in the salt because a trailer search can land on a different commit
/// under the same source (a rebase, then a re-open), and that is a new diff.
fn loaded(
    fetched: Fetched,
    source: ReviewSource,
    ctx: &egui::Context,
    renderer: Option<&Renderer3d>,
    i18n: &mut Localization,
) -> LoadedReview {
    let Fetched {
        resolved,
        notes,
        commit,
        patch,
        images,
        models,
    } = fetched;
    // The header carries the byline, the body and the comments on the commit,
    // so they scroll away with the diff.
    let mut patch_state = GitPatchState::new(&patch, i18n)
        .with_id_salt((source, &commit.sha))
        .with_header();
    review_images::upload(images, &commit.sha, &mut patch_state, ctx, i18n);
    let models = ReviewModels::upload(
        models,
        &mut patch_state,
        renderer,
        ctx.pixels_per_point(),
        i18n,
    );
    LoadedReview {
        source: resolved.source_label().into_owned(),
        source_hover: resolved.to_string(),
        by_trailer: resolved.how == Found::ByTrailer,
        byline: Byline::of(&commit),
        body: CommitBody::parse(&commit.message),
        patch_state,
        models,
        commit,
        patch,
        walk: None,
        notes: ReviewNotes::new(notes),
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

    /// The byline keeps only the author's name and measures the date against
    /// the frozen clock; the hover keeps the email and the exact date.
    #[test]
    fn byline_is_name_and_relative_date() {
        let commit = CommitPatch {
            sha: "a".repeat(40),
            author: "William Casarin <jb55@jb55.com>".to_string(),
            date: "2026-09-29T02:14:33-07:00".to_string(),
            time: 1_000_000,
            message: String::new(),
            patch: String::new(),
            truncated: false,
        };
        headway::fmt::freeze_now(1_000_000 + 86_400 + 5);
        let byline = Byline::of(&commit);
        assert_eq!(byline.short, "William Casarin · 1d ago");
        assert_eq!(
            byline.full,
            "William Casarin <jb55@jb55.com> · 2026-09-29T02:14:33-07:00"
        );

        headway::fmt::freeze_now(1_000_000 + 30);
        assert_eq!(Byline::of(&commit).short, "William Casarin · just now");
    }

    fn id(b: u8) -> NoteId {
        NoteId::new([b; 32])
    }

    /// A failed load waits out its backoff (and says how long is left so the
    /// pane can wake for it), then goes stale; a pending one never does, even
    /// long past the backoff or on re-open, so no second fetch can start.
    #[test]
    fn failed_load_goes_stale_after_backoff_pending_never() {
        let at = Instant::now();
        let src = ReviewSource::Trailer(id(1));
        let failed = |after| stale_in(LoadKind::Failed, src, at, at + after, false);
        assert_eq!(failed(Duration::ZERO), Some(FAILED_BACKOFF));
        assert_eq!(
            failed(Duration::from_secs(10)),
            Some(FAILED_BACKOFF - Duration::from_secs(10))
        );
        assert_eq!(failed(FAILED_BACKOFF), Some(Duration::ZERO));
        assert_eq!(failed(FAILED_BACKOFF * 5), Some(Duration::ZERO));

        let later = at + FAILED_BACKOFF * 5;
        assert_eq!(stale_in(LoadKind::Pending, src, at, later, true), None);
    }

    /// A trailer search is re-run when the pane re-opens and kept otherwise,
    /// however old; a record's load is kept either way (its sha is pinned).
    #[test]
    fn trailer_load_goes_stale_on_reopen_record_load_never() {
        let at = Instant::now();
        let later = at + Duration::from_secs(3600);
        let trailer = ReviewSource::Trailer(id(1));
        let record = ReviewSource::Record {
            card: id(1),
            record: id(2),
        };
        assert_eq!(stale_in(LoadKind::Ready, trailer, at, later, false), None);
        assert_eq!(
            stale_in(LoadKind::Ready, trailer, at, at, true),
            Some(Duration::ZERO)
        );
        assert_eq!(stale_in(LoadKind::Ready, record, at, later, false), None);
        assert_eq!(stale_in(LoadKind::Ready, record, at, later, true), None);
    }

    /// A failed load, backdated past its backoff, is dropped by `expire`; one
    /// still inside it stays and reports the time left.
    #[test]
    fn expire_drops_only_stale_loads() {
        let mut loader = ReviewLoader::default();
        let src = ReviewSource::Trailer(id(1));
        let now = Instant::now();
        loader.loads.insert(src, failed_at(now));
        assert_eq!(
            loader.expire(src, now + Duration::from_secs(1), false),
            Some(FAILED_BACKOFF - Duration::from_secs(1))
        );
        assert!(loader.contains(src));
        assert_eq!(loader.expire(src, now + FAILED_BACKOFF, false), None);
        assert!(!loader.contains(src));
    }

    /// When a card's records change, its settled loads go and its pending one
    /// stays; another card's loads are untouched, and the first sight of a
    /// card (or an unchanged one) drops nothing.
    #[test]
    fn note_records_drops_the_cards_settled_loads_on_change() {
        let mut loader = ReviewLoader::default();
        let now = Instant::now();
        let (card, other) = (id(1), id(9));
        let old = ReviewSource::Record {
            card,
            record: id(2),
        };
        let trailer = ReviewSource::Trailer(card);
        let pending = ReviewSource::Record {
            card,
            record: id(3),
        };
        let theirs = ReviewSource::Trailer(other);
        for src in [old, trailer, theirs] {
            loader.loads.insert(src, failed_at(now));
        }
        let note = "resolving…".to_string();
        let load = ReviewLoad::Pending { note };
        loader.loads.insert(pending, CachedLoad { load, at: now });

        let one = RecordSet {
            len: 1,
            newest: Some(id(2)),
        };
        loader.note_records(card, one);
        loader.note_records(card, one);
        assert_eq!(loader.loads.len(), 4, "first sight and no change keep all");

        loader.note_records(
            card,
            RecordSet {
                len: 2,
                newest: Some(id(3)),
            },
        );
        assert!(!loader.contains(old));
        assert!(!loader.contains(trailer));
        assert!(loader.contains(pending));
        assert!(loader.contains(theirs));
    }

    fn failed_at(at: Instant) -> CachedLoad {
        let err = GitError {
            command: "fetch".to_string(),
            stderr: "ssh: connect to host jex0: Connection refused".to_string(),
        };
        CachedLoad {
            load: ReviewLoad::Failed(err),
            at,
        }
    }

    /// A walk citing the one hunk of an empty file's creation.
    const WALK: &str = "Opening.\n\n```hunk new.txt\n```\n\nThe file.\n";

    /// A one-file patch, its commit and the notes it was loaded with, landed
    /// on the UI side as the pane would.
    fn loaded_with(notes: CommitNotes) -> LoadedReview {
        let patch =
            "diff --git a/new.txt b/new.txt\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hi\n";
        let commit = CommitPatch {
            sha: "a".repeat(40),
            author: "t <t@t>".to_string(),
            date: String::new(),
            time: 0,
            message: "subject\n".to_string(),
            patch: patch.to_string(),
            truncated: false,
        };
        let fetched = Fetched {
            resolved: Resolved {
                repo_dir: PathBuf::from("/nonexistent"),
                sha: commit.sha.clone(),
                target: git::Target::Checkout,
                how: Found::Local,
            },
            notes,
            patch: GitPatch::parse(commit.patch.clone()),
            commit,
            images: Vec::new(),
            models: Vec::new(),
        };
        let ctx = egui::Context::default();
        let source = ReviewSource::Trailer(id(1));
        loaded(fetched, source, &ctx, None, &mut Localization::default())
    }

    fn record(sources: Vec<headway::event::ReviewSourceView>) -> ReviewView {
        ReviewView {
            id: id(2),
            author: [0; 32],
            created_at: 0,
            fields: ReviewFields::default(),
            comments: Vec::new(),
            sources,
        }
    }

    /// A record with no sources gets its walk from the commit's note,
    /// threaded onto the diff's hunk; a record with a walk of its own keeps
    /// it over the note.
    #[test]
    fn a_record_without_sources_walks_the_note() {
        let notes = CommitNotes {
            explain: None,
            code: Some(WALK.to_string()),
        };
        let mut loaded = loaded_with(notes);
        let bare = record(Vec::new());
        let key = loaded.sync_walk(Some(&bare));
        assert!(matches!(key, Some(SourceKey::Note(_))));
        let walk = loaded.walk.as_ref().expect("walk from the note");
        let shown: Vec<_> = walk
            .notes
            .iter()
            .map(|n| (n.at.is_some(), n.prose.as_str()))
            .collect();
        assert_eq!(shown, [(false, "Opening."), (true, "The file.")]);
        assert_eq!(
            loaded.sync_walk(Some(&bare)),
            key,
            "unchanged on the next frame"
        );

        let own = record(vec![headway::event::ReviewSourceView {
            id: id(3),
            author: [0; 32],
            role: ReviewSourceRole::Code,
            name: "x.code.md".to_string(),
            text: "Own walk.\n".to_string(),
            created_at: 0,
        }]);
        assert_eq!(loaded.sync_walk(Some(&own)), Some(SourceKey::Event(id(3))));
        let walk = loaded.walk.as_ref().expect("the record's walk");
        assert_eq!(walk.notes[0].prose, "Own walk.");
    }

    /// Notes are read for a record missing either source, not for one with
    /// both, nor for a trailer search with no record to draw them on.
    #[test]
    fn notes_are_read_only_for_a_record_missing_a_source() {
        let view = |role| headway::event::ReviewSourceView {
            id: id(4),
            author: [0; 32],
            role,
            name: String::new(),
            text: String::new(),
            created_at: 0,
        };
        assert!(!wants_notes(None));
        assert!(wants_notes(Some(&record(Vec::new()))));
        assert!(wants_notes(Some(&record(vec![view(
            ReviewSourceRole::Code
        )]))));
        let both = record(vec![
            view(ReviewSourceRole::Explainer),
            view(ReviewSourceRole::Code),
        ]);
        assert!(!wants_notes(Some(&both)));
    }

    /// The worker reads a local commit's explain note beside its patch when
    /// asked to, and not otherwise.
    #[test]
    fn the_worker_reads_the_commits_notes() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        run(&["init", "-q"]);
        run(&["commit", "-q", "--allow-empty", "-m", "work"]);
        let sha = run(&["rev-parse", "HEAD"]);
        run(&[
            "notes",
            "--ref",
            git::EXPLAIN_REF,
            "add",
            "-m",
            "# page",
            &sha,
        ]);

        let mut job = ReviewJob {
            record: Some(ReviewFields {
                commit: Some(sha.clone()),
                host: Some("here".to_string()),
                path: Some(dir.to_str().unwrap().to_string()),
                ..Default::default()
            }),
            notes: true,
            card_ref: "headway:b/a-b-c".to_string(),
            checkouts: Vec::new(),
            cache_root: dir.join("cache"),
            renderer: None,
        };
        let fetched = load(&job, "here").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(fetched.notes.explain.as_deref(), Some("# page"));
        assert_eq!(fetched.notes.code, None);

        job.notes = false;
        assert!(load(&job, "here").unwrap().notes.is_empty());
    }

    /// Short shas are cut to 12, shorter strings pass through.
    #[test]
    fn short_sha_cuts_to_twelve() {
        assert_eq!(short_sha("136ceb9d3bfa0123"), "136ceb9d3bfa");
        assert_eq!(short_sha("abc"), "abc");
    }
}
