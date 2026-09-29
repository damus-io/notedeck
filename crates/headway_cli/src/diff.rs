//! `headway diff`: find the commit a card's review record names on this host —
//! fetching it from the host that recorded it when it isn't here — and print
//! it `git show`-style. The no-UI exercise of [`headway::git::resolve`].
//!
//! The resolution line (where the commit was found) goes to stderr and the
//! commit to stdout, so `headway diff <card> | git apply` works.

use std::path::{Path, PathBuf};
use std::time::Duration;

use headway::event::{self, BoardView, ReviewView, resolve_card};
use headway::git::{self, ResolveCtx};
use headway::wordid;

use nostrdb_net::relay::sync::Result;

use crate::review::host_name;

/// The patch size `diff` prints before cutting it off.
const MAX_PATCH_BYTES: usize = 4 << 20;

/// How long one `git fetch` from another host may take.
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// Resolve card `sel`'s newest review record (or the one whose commit starts
/// with `record`) to a commit and print it. A card with no records falls back to
/// the `Headway:` trailer search in this checkout. `cache_root` is where the
/// headway-owned bare cache repos live.
pub(crate) fn print_diff(
    view: &BoardView,
    sel: &str,
    record: Option<&str>,
    cache_root: &Path,
) -> Result<()> {
    let id = resolve_card(view, sel)?;
    let card = event::all_cards(view)
        .find(|c| c.id == id)
        .ok_or_else(|| format!("no card matching '{sel}'"))?;
    let card_ref = wordid::card_ref(&view.id, id.bytes());
    let local_host = host_name().unwrap_or_default();
    let checkouts = known_checkouts(view, &local_host);

    let resolved = match pick_record(&card.reviews, record)? {
        Some(review) => {
            let ctx = ResolveCtx {
                local_host: &local_host,
                cache_root,
                known_checkouts: &checkouts,
                timeout: FETCH_TIMEOUT,
            };
            git::resolve(&review.fields, &card_ref, &ctx)?
        }
        None => git::resolve_by_trailer(&card_ref, &checkouts).ok_or_else(|| {
            format!(
                "{card_ref} has no review record, and no commit in this checkout \
                 carries a 'Headway: {card_ref}' trailer"
            )
        })?,
    };
    eprintln!("{}", nostrdb_net::relay::sync::dim(&resolved.to_string()));

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

/// The review record `diff` shows: the one whose commit starts with `prefix`
/// when given (newest first, as [`event::CardView::reviews`] is ordered), else
/// the newest. `None` when the card has no records and no prefix was asked for.
fn pick_record<'a>(
    reviews: &'a [ReviewView],
    prefix: Option<&str>,
) -> Result<Option<&'a ReviewView>> {
    let Some(prefix) = prefix else {
        return Ok(reviews.first());
    };
    reviews
        .iter()
        .find(|r| {
            r.fields
                .commit
                .as_deref()
                .is_some_and(|c| c.starts_with(prefix))
        })
        .map(Some)
        .ok_or_else(|| format!("no review record with a commit starting '{prefix}'").into())
}

/// The checkouts on this host worth trying before the cache: the one the
/// command runs in, then every path a review record on this board says was
/// recorded here. Deduplicated, in that order.
fn known_checkouts(view: &BoardView, local_host: &str) -> Vec<PathBuf> {
    let cwd = git::toplevel(Path::new(".")).ok();
    let recorded = event::all_cards(view)
        .flat_map(|c| c.reviews.iter())
        .filter(|r| r.fields.host.as_deref() == Some(local_host))
        .filter_map(|r| r.fields.path.clone());
    let mut out: Vec<PathBuf> = Vec::new();
    for path in cwd.into_iter().chain(recorded).map(PathBuf::from) {
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use headway::event::ReviewFields;
    use nostrdb_net::NoteId;

    /// A review record carrying only `commit`.
    fn review(commit: &str) -> ReviewView {
        ReviewView {
            id: NoteId::new([0; 32]),
            author: [0; 32],
            created_at: 0,
            fields: ReviewFields {
                commit: Some(commit.to_string()),
                ..Default::default()
            },
        }
    }

    /// No prefix takes the newest; a prefix picks the first match; a prefix
    /// matching nothing is an error rather than a silent fallback.
    #[test]
    fn pick_record_by_prefix() {
        let reviews = [review("bbb222"), review("aaa111")];
        assert_eq!(pick_record(&reviews, None).unwrap(), Some(&reviews[0]));
        assert_eq!(
            pick_record(&reviews, Some("aaa")).unwrap(),
            Some(&reviews[1])
        );
        assert!(pick_record(&reviews, Some("ccc")).is_err());
        assert_eq!(pick_record(&[], None).unwrap(), None);
    }
}
