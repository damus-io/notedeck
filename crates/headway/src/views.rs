//! Saved list views: an account's [`ViewSpec`]s, kept in nostrdb as kind-30626
//! notes ([`event::KIND_VIEW`]), one per view slug, latest-wins.
//!
//! Personal, like the board preference ([`crate::store::save_board_pref`]): each
//! note is signed by the account key and PNS-wrapped, so only that account can
//! read it, and front ends pass [`NoPublish`](crate::store::NoPublish) so it
//! stays in the local cache. A deleted view is a tombstone — the slug
//! republished with empty content — so a delete supersedes like any edit. See
//! `headway:headway/brave-project-parade`.

use nostrdb::Ndb;
use nostrdb_net::Pubkey;

use crate::event::{self, ViewNote};
use crate::query::ViewSpec;
use crate::store::{Publisher, board_slug, ingest_pns, next_after};

/// A view an account has saved: the slug its notes are keyed by, and its spec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedView {
    /// The view's `d` tag, made from its name when first saved. Stable across
    /// renames, so a rename supersedes the same note.
    pub slug: String,
    /// What the view shows.
    pub spec: ViewSpec,
}

/// Every view `author` has saved and not deleted, in slug order. A note whose
/// content isn't a [`ViewSpec`] is skipped rather than failing the rest.
pub fn load_views(ndb: &Ndb, author: &Pubkey) -> Vec<SavedView> {
    event::view_notes(ndb, author)
        .into_iter()
        .filter_map(decode)
        .collect()
}

/// The live view a stored note holds, or `None` for a tombstone or content
/// that doesn't parse.
fn decode(note: ViewNote) -> Option<SavedView> {
    if note.content.is_empty() {
        return None;
    }
    let spec = serde_json::from_str(&note.content).ok()?;
    Some(SavedView {
        slug: note.slug,
        spec,
    })
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

/// Save `view` for `author`: its spec as JSON under its slug, signed with
/// `secret`, PNS-wrapped and ingested, superseding any earlier revision of
/// that slug (including a tombstone).
pub fn save_view(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    view: &SavedView,
    publisher: &mut dyn Publisher,
) {
    let Ok(content) = serde_json::to_string(&view.spec) else {
        return;
    };
    write(ndb, author, secret, &view.slug, &content, publisher);
}

/// Delete `author`'s view `slug`: a tombstone (empty content) that supersedes
/// its last revision.
pub fn delete_view(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    slug: &str,
    publisher: &mut dyn Publisher,
) {
    write(ndb, author, secret, slug, "", publisher);
}

/// Sign and ingest one revision of view `slug`, stamped strictly past its
/// current one so a same-second re-save still wins latest-wins.
fn write(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    slug: &str,
    content: &str,
    publisher: &mut dyn Publisher,
) {
    let current = event::view_notes(ndb, author)
        .into_iter()
        .find(|n| n.slug == slug)
        .map_or(0, |n| n.created_at);
    let Some(inner) = event::build_view(slug, content)
        .created_at(next_after(current))
        .sign(secret)
        .build()
    else {
        return;
    };
    ingest_pns(ndb, &inner, secret, publisher);
}

#[cfg(test)]
mod tests {
    use super::*;

    use nostrdb::SubscriptionStream;
    use nostrdb_net::FullKeypair;

    use crate::query::{
        DisplayProp, DisplayProps, GroupBy, Grouping, SortBy, Sorting, StateFilter,
    };
    use crate::store::NoPublish;
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

    /// The spec survives JSON, its layout spelled by name, and a stored spec
    /// missing fields loads with their defaults.
    #[test]
    fn view_spec_json_round_trip() {
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
            .kinds([event::KIND_VIEW as u64])
            .build();
        let sub = ndb.subscribe(&[filter]).unwrap();
        let mut stream = SubscriptionStream::new(ndb.clone(), sub).notes_per_await(64);
        let me = &kp.pubkey;

        assert!(load_views(&ndb, me).is_empty());
        let slug = new_slug(&[], "Hot issues!");
        assert_eq!(slug, "hot-issues");
        let mut view = SavedView { slug, spec: hot() };
        save_view(&ndb, me, &secret, &view, &mut NoPublish);
        wait_views(&ndb, &mut stream, me, std::slice::from_ref(&view)).await;
        let views = load_views(&ndb, me);
        assert_eq!(find_view(&views, "hot").map(|v| &v.spec), Some(&hot()));
        assert_eq!(
            find_view(&views, "hot-issues").map(|v| &v.slug),
            Some(&view.slug)
        );

        view.spec.name = "Urgent".to_string();
        save_view(&ndb, me, &secret, &view, &mut NoPublish);
        wait_views(&ndb, &mut stream, me, std::slice::from_ref(&view)).await;

        delete_view(&ndb, me, &secret, &view.slug, &mut NoPublish);
        wait_views(&ndb, &mut stream, me, &[]).await;

        save_view(&ndb, me, &secret, &view, &mut NoPublish);
        wait_views(&ndb, &mut stream, me, std::slice::from_ref(&view)).await;
    }
}
