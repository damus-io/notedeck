//! Saved list views: an account's [`ViewSpec`]s, kept in nostrdb as kind-30626
//! notes ([`event::KIND_VIEW`]), one per view slug, latest-wins.
//!
//! Personal, like the board preference ([`crate::store::save_board_pref`]):
//! each view is a private latest-wins note ([`private_note`]), signed by the
//! account key and PNS-wrapped, so only that account can read it. A deleted
//! view is a tombstone — the slug republished with a bare `["deleted"]` tag —
//! so a delete supersedes like any edit. The spec is stored as tags
//! ([`event::build_view`]), like every other headway kind. See
//! `headway:headway/brave-project-parade`.

use nostrdb::{Ndb, Transaction};
use nostrdb_net::Pubkey;

use crate::event::{self, KIND_VIEW, ViewRevision};
use crate::query::ViewSpec;
use crate::store::board_slug;

/// A view an account has saved: the slug its notes are keyed by, and its spec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedView {
    /// The view's `d` tag, made from its name when first saved. Stable across
    /// renames, so a rename supersedes the same note.
    pub slug: String,
    /// What the view shows.
    pub spec: ViewSpec,
}

/// Every view `author` has saved and not deleted, in slug order: the newest
/// revision of each slug ([`private_note::newest_per_d`]), its tags read back
/// by [`event::parse_view`].
pub fn load_views(ndb: &Ndb, author: &Pubkey) -> Vec<SavedView> {
    let Ok(txn) = Transaction::new(ndb) else {
        return Vec::new();
    };
    private_note::newest_per_d(ndb, &txn, author, KIND_VIEW)
        .into_iter()
        .filter_map(|slot| match event::parse_view(&slot.note) {
            ViewRevision::Live(spec) => Some(SavedView {
                slug: slot.d.to_owned(),
                spec,
            }),
            ViewRevision::Deleted => None,
        })
        .collect()
}

/// The view among `views` that `name` names: by its name, ignoring case, or
/// by its slug.
pub fn find_view<'a>(views: &'a [SavedView], name: &str) -> Option<&'a SavedView> {
    let name = name.trim();
    views
        .iter()
        .find(|v| v.spec.name.eq_ignore_ascii_case(name) || v.slug == name)
}

/// A slug for a new view named `name`, unused by any of `views`. A deleted
/// view's slug may be reused: saving over its tombstone revives the slot.
pub fn new_slug(views: &[SavedView], name: &str) -> String {
    board_slug(name, |s| views.iter().any(|v| v.slug == s))
}

/// Save `view` for `author`: its spec as tags under its slug
/// ([`event::build_view`]), signed with `secret`, PNS-wrapped and ingested,
/// superseding any earlier revision of that slug (including a tombstone).
/// Returns the stamp to pass as the next write's `after` (see [`write`]).
pub fn save_view(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    view: &SavedView,
    after: u64,
) -> u64 {
    let note = event::build_view(&view.slug, &view.spec);
    write(ndb, author, secret, &view.slug, note, after)
}

/// Delete `author`'s view `slug`: a tombstone ([`event::build_view_tombstone`])
/// that supersedes its last revision. Returns the stamp to pass as the next
/// write's `after`.
pub fn delete_view(ndb: &Ndb, author: &Pubkey, secret: &[u8; 32], slug: &str, after: u64) -> u64 {
    write(
        ndb,
        author,
        secret,
        slug,
        event::build_view_tombstone(slug),
        after,
    )
}

/// Write `note`, one revision of view `slug`, with
/// [`private_note::write_note`], stamped strictly past both its stored
/// revision and `after`, the stamp of the caller's last view write: two quick
/// edits made before the first is ingested would otherwise tie in the same
/// second. Returns the new stamp, or `after` when nothing was stored.
fn write<'a>(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &'a [u8; 32],
    slug: &str,
    note: nostrdb::NoteBuilder<'a>,
    after: u64,
) -> u64 {
    let slot = private_note::Slot {
        author,
        kind: KIND_VIEW,
        d: slug,
    };
    private_note::write_note(ndb, secret, slot, note, after).unwrap_or(after)
}

#[cfg(test)]
mod tests {
    use super::*;

    use nostrdb::SubscriptionStream;
    use nostrdb_net::FullKeypair;

    use crate::query::{
        DisplayProp, DisplayProps, GroupBy, Grouping, SortBy, Sorting, StateFilter,
    };
    use crate::test_config;

    /// A view with every field off its default.
    fn hot() -> ViewSpec {
        ViewSpec {
            name: "Hot".to_string(),
            query: "p:high label:ui".to_string(),
            grouping: Grouping {
                by: GroupBy::Label,
                sub: GroupBy::Priority,
                show_empty: true,
            },
            sorting: Sorting {
                completed_by_recency: true,
                ..Sorting::by(SortBy::WorkOrder)
            },
            display: DisplayProps::of(&[DisplayProp::Id, DisplayProp::Board]),
            hide_subissues: true,
            board: Some("headway".to_string()),
        }
    }

    /// The spec survives serde — what `headway list --views --json` prints —
    /// its layout spelled by name, and a spec missing fields reads with their
    /// defaults. The note stores tags instead (see `event::parse`'s tests).
    #[test]
    fn view_spec_serde_round_trip() {
        let json = serde_json::to_string(&hot()).unwrap();
        assert!(json.contains(r#""query":"p:high label:ui""#), "{json}");
        assert!(json.contains(r#""display":["id","board"]"#), "{json}");
        assert!(json.contains(r#""by":"work-order""#), "{json}");
        assert_eq!(serde_json::from_str::<ViewSpec>(&json).unwrap(), hot());

        let sparse: ViewSpec = serde_json::from_str(r#"{"name":"Old","query":"p:low"}"#).unwrap();
        assert_eq!(sparse.name, "Old");
        assert_eq!(sparse.grouping, Grouping::default());
        assert_eq!(sparse.display, DisplayProps::default());
        assert_eq!(sparse.board, None);
    }

    /// A view's filter defaults to open cards, keeps the query's own state,
    /// and carries the subissue toggle.
    #[test]
    fn view_spec_filter() {
        let filter = hot().filter();
        assert_eq!(filter.state, Some(StateFilter::Open));
        assert!(filter.hide_subissues);
        let all = ViewSpec {
            query: "is:any".to_string(),
            ..ViewSpec::default()
        };
        assert_eq!(all.filter().state, Some(StateFilter::Any));
    }

    /// Await the writer's ingest until `author`'s views read back as `want`.
    async fn wait_views(
        ndb: &Ndb,
        stream: &mut SubscriptionStream,
        author: &Pubkey,
        want: &[SavedView],
    ) {
        while load_views(ndb, author) != want {
            stream
                .wait_for_notes(1, crate::INGEST_TIMEOUT)
                .await
                .expect("a note ingested before the deadline");
        }
    }

    /// Saving builds a kind-30626 note that parses back to the spec; a re-save
    /// supersedes it (a rename keeps the slug); a delete tombstones it, and a
    /// later save under the same slug revives it.
    #[tokio::test]
    async fn views_save_rename_delete_round_trip() {
        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &test_config()).unwrap();
        let kp = FullKeypair::generate();
        let secret = kp.secret_key.secret_bytes();
        assert!(ndb.add_key(&secret));
        let filter = nostrdb::Filter::new()
            .authors([kp.pubkey.bytes()])
            .kinds([KIND_VIEW as u64])
            .build();
        let sub = ndb.subscribe(&[filter]).unwrap();
        let mut stream = SubscriptionStream::new(ndb.clone(), sub).notes_per_await(64);
        let me = &kp.pubkey;
        let mut stamp = 0;

        assert!(load_views(&ndb, me).is_empty());
        let slug = new_slug(&[], "Hot issues!");
        assert_eq!(slug, "hot-issues");
        let mut view = SavedView { slug, spec: hot() };
        stamp = save_view(&ndb, me, &secret, &view, stamp);
        wait_views(&ndb, &mut stream, me, std::slice::from_ref(&view)).await;
        let views = load_views(&ndb, me);
        assert_eq!(find_view(&views, "hot").map(|v| &v.spec), Some(&hot()));
        assert_eq!(
            find_view(&views, "hot-issues").map(|v| &v.slug),
            Some(&view.slug)
        );

        view.spec.name = "Urgent".to_string();
        stamp = save_view(&ndb, me, &secret, &view, stamp);
        wait_views(&ndb, &mut stream, me, std::slice::from_ref(&view)).await;

        stamp = delete_view(&ndb, me, &secret, &view.slug, stamp);
        wait_views(&ndb, &mut stream, me, &[]).await;

        save_view(&ndb, me, &secret, &view, stamp);
        wait_views(&ndb, &mut stream, me, std::slice::from_ref(&view)).await;
    }
}
