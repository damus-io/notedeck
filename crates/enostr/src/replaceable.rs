//! Resolving addressable / replaceable nostr events out of nostrdb.
//!
//! nostrdb keeps *every* revision of a replaceable event (kinds 10000–19999 and
//! the parameterized 30000–39999 range) — it does not collapse them internally.
//! So a reader that wants the *effective* state has to fold the matching notes
//! and keep, per address, the revision with the highest `created_at`.
//!
//! This is that fold, shared so every consumer (calendar sync, Dave sessions,
//! Horizon, …) resolves replaceable events the same way. It will likely move
//! into nostrdb itself eventually; living here keeps it in one place until then.

use nostrdb::{Filter, Ndb, Note, NoteKey, Transaction};
use std::collections::HashMap;

/// Resolve replaceable events matching `filters`, deduplicating by `d` tag.
///
/// Returns one [`NoteKey`] per unique `d`-tag value — the revision with the
/// highest `created_at`. Notes without a `d` tag are ignored. See the module
/// docs for why this is necessary.
pub fn query_replaceable(ndb: &Ndb, txn: &Transaction, filters: &[Filter]) -> Vec<NoteKey> {
    query_replaceable_filtered(ndb, txn, filters, |_| true)
}

/// Like [`query_replaceable`], but `predicate` decides whether the *winning*
/// (latest) revision of each `d` tag is kept.
///
/// The predicate is evaluated only against the latest revision of each group; an
/// older revision passing it does not resurrect the group. This matters when
/// callers want "the current state, but only if it still satisfies X".
pub fn query_replaceable_filtered(
    ndb: &Ndb,
    txn: &Transaction,
    filters: &[Filter],
    predicate: impl Fn(&Note) -> bool,
) -> Vec<NoteKey> {
    // For each d-tag value track the latest created_at seen and, when that latest
    // revision passes the predicate, its key. Notes can arrive in any order, so
    // we always advance on a newer timestamp and only retain a key when valid.
    let best = ndb.fold(
        txn,
        filters,
        HashMap::<String, (u64, Option<NoteKey>)>::new(),
        |mut acc, note| {
            let Some(d_tag) = tag_value(&note, "d") else {
                return acc;
            };
            let created_at = note.created_at();
            if acc.get(d_tag).is_some_and(|(ts, _)| created_at <= *ts) {
                return acc;
            }
            let key = predicate(&note).then(|| note.key().expect("queried note has a key"));
            acc.insert(d_tag.to_string(), (created_at, key));
            acc
        },
    );

    match best {
        Ok(map) => map.into_values().filter_map(|(_, key)| key).collect(),
        Err(_) => Vec::new(),
    }
}

/// First value of the single-letter-or-named tag `name` on `note`, if any.
fn tag_value<'a>(note: &'a Note<'a>, name: &str) -> Option<&'a str> {
    note.tags().iter().find_map(|tag| {
        (tag.count() >= 2 && tag.get_str(0) == Some(name)).then(|| tag.get_str(1))?
    })
}
