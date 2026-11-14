//! `headway diff`: find the commit a card's review record names on this host —
//! fetching it from the host that recorded it when it isn't here — and print
//! it `git show`-style. The no-UI exercise of [`headway::git::resolve`].
//!
//! The resolution line (where the commit was found) goes to stderr and the
//! commit to stdout, so `headway diff <card> | git apply` works. `--note`
//! prints one of the commit's explain notes instead (see [`git::fetch_notes`]).

use std::path::{Path, PathBuf};
use std::time::Duration;

use headway::edit::pick_record;
use headway::event::{self, BoardView, resolve_card};
use headway::git::{self, NoteKind, ResolveCtx};
use headway::wordid;

use nostrdb_net::relay::sync::Result;

/// The patch size `diff` prints before cutting it off.
const MAX_PATCH_BYTES: usize = 4 << 20;

/// How long one `git fetch` from another host may take.
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// Resolve card `sel`'s newest review record (or the one whose commit starts
/// with `record`) to a commit and print it, or with `note` that explain note of
/// it. A card with no records falls back to the `Headway:` trailer search in
/// this checkout. `cache_root` is where the headway-owned bare cache repos
/// live.
pub(crate) fn print_diff(
    view: &BoardView,
    sel: &str,
    record: Option<&str>,
    note: Option<NoteKind>,
    cache_root: &Path,
) -> Result<()> {
    let id = resolve_card(view, sel)?;
    let card = event::all_cards(view)
        .find(|c| c.id == id)
        .ok_or_else(|| format!("no card matching '{sel}'"))?;
    let card_ref = wordid::card_ref(&view.id, id.bytes());
    let local_host = git::host_name().unwrap_or_default();
    let cwd = git::toplevel(Path::new(".")).ok().map(PathBuf::from);
    let checkouts = git::known_checkouts(view, &local_host, cwd);

    // Where the commit's notes are fetched from when they aren't with it: the
    // place the commit itself is fetched from. A trailer-only find has none.
    let mut notes_src = None;
    let resolved = match pick_record(&card.reviews, record)? {
        Some(review) => {
            let ctx = ResolveCtx {
                local_host: &local_host,
                cache_root,
                known_checkouts: &checkouts,
                timeout: FETCH_TIMEOUT,
            };
            let resolved = git::resolve(&review.fields, &card_ref, &ctx)?;
            notes_src = git::fetch_source(&review.fields, &resolved.repo_dir, &ctx);
            resolved
        }
        None => git::resolve_by_trailer(&card_ref, &checkouts).ok_or_else(|| {
            format!(
                "{card_ref} has no review record, and no commit in this checkout \
                 carries a 'Headway: {card_ref}' trailer"
            )
        })?,
    };
    eprintln!("{}", nostrdb_net::relay::sync::dim(&resolved.to_string()));

    if let Some(kind) = note {
        let notes = git::fetch_notes(&resolved, notes_src.as_deref(), FETCH_TIMEOUT)?;
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
        println!("{text}");
        return Ok(());
    }

    let patch = git::commit_patch(&resolved.repo_dir, &resolved.sha, MAX_PATCH_BYTES)?;
    println!("commit {}", patch.sha);
    println!("Author: {}", patch.author);
    println!("Date:   {}", patch.date);
    println!();
    for line in patch.message.lines() {
        println!("    {line}");
    }
    println!();
    print!("{}", patch.patch);
    if patch.truncated {
        eprintln!("(patch truncated at {MAX_PATCH_BYTES} bytes)");
    }
    Ok(())
}
