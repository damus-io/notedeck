//! `diff`: find the commit a card's review record names on this host —
//! fetching it from the host that recorded it when it isn't here — and read it
//! back `git show`-style, or one of its explain notes. Shared by the `headway`
//! CLI and the app's `headway_diff` tool.
//!
//! It runs in two halves so a caller with a render loop can keep it free: a
//! [`DiffPlan`] reads what it needs from the board, quickly, and owns it; then
//! [`DiffPlan::run`] does the git work, which can fetch for up to
//! [`FETCH_TIMEOUT`], anywhere.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::edit::pick_record;
use crate::event::{self, BoardView, ReviewFields, resolve_card};
use crate::git::{self, CommitPatch, NoteKind, ResolveCtx, Resolved};
use crate::wordid;

/// The patch size `diff` reads before cutting it off.
pub const MAX_PATCH_BYTES: usize = 4 << 20;

/// How long one `git fetch` from another host may take.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// What a `diff` needs from the board, owned, so the git work can run on
/// another thread from the one that read the board.
#[derive(Clone, Debug)]
pub struct DiffPlan {
    /// The card's `headway:<board>/<word-id>`, for the trailer search.
    card_ref: String,
    /// The review record to resolve; `None` searches by trailer alone.
    record: Option<ReviewFields>,
    /// This host's checkouts to try, the caller's first.
    checkouts: Vec<PathBuf>,
    /// This host's name, to tell a recorded path here from one elsewhere.
    local_host: String,
    /// Read this explain note of the commit instead of its patch.
    note: Option<NoteKind>,
}

/// What a `diff` read back: where the commit was found, and what of it.
#[derive(Clone, Debug)]
pub struct DiffOut {
    pub resolved: Resolved,
    pub body: DiffBody,
}

/// The part of the commit a `diff` read.
#[derive(Clone, Debug)]
pub enum DiffBody {
    /// The commit, as `git show` prints it (capped at [`MAX_PATCH_BYTES`]).
    Patch(CommitPatch),
    /// One of its explain notes.
    Note { kind: NoteKind, text: String },
}

impl DiffPlan {
    /// Plan a `diff` of card `sel`'s newest review record on `view` (or the
    /// one whose commit starts with `record`), or with `note` that explain
    /// note of it. `cwd` is the caller's checkout, tried first and searched
    /// by trailer when the card has no record.
    ///
    /// Errors when `sel` names no card or `record` no record.
    pub fn new(
        view: &BoardView,
        sel: &str,
        record: Option<&str>,
        note: Option<NoteKind>,
        cwd: Option<&Path>,
    ) -> Result<Self, String> {
        let id = resolve_card(view, sel)?;
        let card = event::all_cards(view)
            .find(|c| c.id == id)
            .ok_or_else(|| format!("no card matching '{sel}'"))?;
        let local_host = git::host_name().unwrap_or_default();
        let cwd = cwd
            .and_then(|dir| git::toplevel(dir).ok())
            .map(PathBuf::from);
        Ok(Self {
            card_ref: wordid::card_ref(&view.id, id.bytes()),
            record: pick_record(&card.reviews, record)?.map(|r| r.fields.clone()),
            checkouts: git::known_checkouts(view, &local_host, cwd),
            local_host,
            note,
        })
    }

    /// Resolve the commit, fetching it into `cache_root`'s bare repos when
    /// this host lacks it, and read it back. A card with no record falls
    /// back to the `Headway:` trailer search in the known checkouts.
    ///
    /// Errors with git's own message when the fetch fails, or when the
    /// commit has no such note.
    pub fn run(&self, cache_root: &Path) -> Result<DiffOut, String> {
        // Where the commit's notes are fetched from when they aren't with it:
        // the place the commit itself is fetched from. A trailer-only find has
        // none.
        let mut notes_src = None;
        let resolved = match &self.record {
            Some(record) => {
                let ctx = ResolveCtx {
                    local_host: &self.local_host,
                    cache_root,
                    known_checkouts: &self.checkouts,
                    timeout: FETCH_TIMEOUT,
                };
                let resolved =
                    git::resolve(record, &self.card_ref, &ctx).map_err(|e| e.to_string())?;
                notes_src = git::fetch_source(record, &resolved.repo_dir, &ctx);
                resolved
            }
            None => git::resolve_by_trailer(&self.card_ref, &self.checkouts).ok_or_else(|| {
                format!(
                    "{} has no review record, and no commit in this checkout \
                     carries a 'Headway: {}' trailer",
                    self.card_ref, self.card_ref
                )
            })?,
        };

        let body = match self.note {
            Some(kind) => {
                let notes = git::fetch_notes(&resolved, notes_src.as_deref(), FETCH_TIMEOUT)
                    .map_err(|e| e.to_string())?;
                let text = notes.get(kind).ok_or_else(|| {
                    let mut msg = format!(
                        "commit {} has no {} note ({}) in {}",
                        &resolved.sha[..12.min(resolved.sha.len())],
                        kind.name(),
                        kind.notes_ref(),
                        resolved.repo_dir.display()
                    );
                    if let Some(src) = notes_src.as_deref() {
                        msg.push_str(&format!(" or {src}"));
                    }
                    msg
                })?;
                DiffBody::Note {
                    kind,
                    text: text.to_owned(),
                }
            }
            None => DiffBody::Patch(
                git::commit_patch(&resolved.repo_dir, &resolved.sha, MAX_PATCH_BYTES)
                    .map_err(|e| e.to_string())?,
            ),
        };
        Ok(DiffOut { resolved, body })
    }
}

impl DiffOut {
    /// What goes to stdout: the note's text, or the commit `git show`-style,
    /// so `diff <card> | git apply` works.
    pub fn text(&self) -> String {
        let patch = match &self.body {
            DiffBody::Note { text, .. } => return format!("{text}\n"),
            DiffBody::Patch(patch) => patch,
        };
        let mut out = format!(
            "commit {}\nAuthor: {}\nDate:   {}\n\n",
            patch.sha, patch.author, patch.date
        );
        for line in patch.message.lines() {
            out.push_str(&format!("    {line}\n"));
        }
        out.push('\n');
        out.push_str(&patch.patch);
        out
    }

    /// What goes to stderr, beside the text: where the commit was found,
    /// and that the patch was cut short when it was.
    pub fn notice(&self) -> String {
        let mut out = self.resolved.to_string();
        if let DiffBody::Patch(patch) = &self.body
            && patch.truncated
        {
            out.push_str(&format!("\n(patch truncated at {MAX_PATCH_BYTES} bytes)"));
        }
        out
    }
}
