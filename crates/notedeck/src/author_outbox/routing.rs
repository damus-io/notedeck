use super::{RelayDirectoryRead, RelayDirectoryState};
use enostr::{NormRelayUrl, Pubkey};
use hashbrown::HashMap;
use nostrdb::{Filter, FilterElement, FilterField};

/// One relay-specific `Filter` derived from an original author `Filter`.
#[derive(Clone, Debug)]
pub(crate) struct RoutedFilter {
    pub(crate) relay: NormRelayUrl,
    pub(crate) filter_index: usize,
    pub(crate) filter: Filter,
    pub(crate) relay_priority: RoutedRelayPriority,
}

/// Planner-provided relay ordering for additive author-outbox coverage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RoutedRelayPriority {
    pub(crate) connection_weight: u32,
    pub(crate) order: usize,
}

impl Default for RoutedRelayPriority {
    fn default() -> Self {
        Self {
            connection_weight: 0,
            order: usize::MAX,
        }
    }
}

/// Ordered source-filter shape that makes `RoutedFilter::filter_index` valid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RoutedFilterShape {
    filters: Vec<RoutedFilterInputShape>,
}

impl RoutedFilterShape {
    pub(crate) fn from_filters(filters: &[Filter]) -> Option<Self> {
        filters
            .iter()
            .map(routed_filter_input_shape)
            .collect::<Option<Vec<_>>>()
            .map(|filters| Self { filters })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RoutedFilterInputShape {
    non_author_json: String,
    authors: Vec<Pubkey>,
}

impl RoutedFilter {
    pub(crate) fn is_empty(&self) -> bool {
        self.filter.num_elements() == 0
    }
}

/// Route one author filter into relay-specific filters using local author relay state.
///
/// Each author with known write relays is assigned to those relays. The output
/// contains one filter per relay, with that relay's routed authors kept together.
/// Authors without resolved write relays produce no filter here; the caller's
/// baseline subscription remains responsible for unresolved coverage.
/// Filters with local custom predicates produce no routed filters because those
/// predicates cannot be expressed in a remote relay subscription.
/// Route one indexed author filter into relay-specific filters.
pub(crate) fn route_filter_at_index<D>(
    filter_index: usize,
    filter: &Filter,
    directory: &D,
) -> Vec<RoutedFilter>
where
    D: RelayDirectoryRead + ?Sized,
{
    if contains_custom_filter(filter) {
        return Vec::new();
    }

    let authors = filter_author_pubkeys(filter);
    route_filter_authors(filter_index, filter, directory, authors)
}

fn route_filter_authors<D>(
    filter_index: usize,
    filter: &Filter,
    directory: &D,
    mut authors: Vec<Pubkey>,
) -> Vec<RoutedFilter>
where
    D: RelayDirectoryRead + ?Sized,
{
    if authors.is_empty() {
        return Vec::new();
    }
    authors.sort_unstable();
    authors.dedup();

    let mut authors_by_relay: HashMap<NormRelayUrl, Vec<Pubkey>> = HashMap::new();

    for author in authors {
        let RelayDirectoryState::Known(relay_set) = directory.author_state(&author) else {
            continue;
        };

        for relay in relay_set {
            authors_by_relay
                .entry(relay.clone())
                .or_default()
                .push(author);
        }
    }

    let mut routed = Vec::new();
    for (relay, mut authors) in authors_by_relay {
        authors.sort_unstable();
        authors.dedup();
        let Some(filter) = clone_filter_with_authors(filter, authors) else {
            continue;
        };
        routed.push(RoutedFilter {
            relay,
            filter_index,
            filter,
            relay_priority: RoutedRelayPriority::default(),
        });
    }

    routed
}

fn routed_filter_input_shape(filter: &Filter) -> Option<RoutedFilterInputShape> {
    let mut authors = filter_author_pubkeys(filter);
    authors.sort_unstable();
    authors.dedup();

    let non_author_filter = clone_filter_with_authors(filter, Vec::new())?;
    let non_author_json = non_author_filter.json().ok()?;

    Some(RoutedFilterInputShape {
        non_author_json,
        authors,
    })
}

/// Return whether one filter carries a local-only custom predicate.
fn contains_custom_filter(filter: &Filter) -> bool {
    filter
        .into_iter()
        .any(|field| matches!(field, FilterField::Custom(_)))
}

/// Extract author pubkeys from one filter's `authors` field.
pub(crate) fn filter_author_pubkeys(filter: &Filter) -> Vec<Pubkey> {
    let mut authors = Vec::new();
    for field in filter {
        if let FilterField::Authors(filter_authors) = field {
            authors.extend(
                filter_authors
                    .into_iter()
                    .map(|author| Pubkey::new(*author)),
            );
        }
    }
    authors
}

/// Rebuild one relay-specific filter from an original multi-author filter.
fn clone_filter_with_authors(filter: &Filter, mut authors: Vec<Pubkey>) -> Option<Filter> {
    authors.sort_unstable();
    authors.dedup();

    let mut builder = Filter::new();

    for index in 0..filter.data.num_elements {
        let Some(field) = filter.data.field(index) else {
            continue;
        };

        builder = match field {
            FilterField::Ids(ids) => builder.ids(ids),
            FilterField::Authors(_) => builder,
            FilterField::Kinds(kinds) => builder.kinds(kinds),
            FilterField::Tags(chr, tags) => copy_tag_field(builder, chr, tags),
            FilterField::Since(n) => builder.since(n),
            FilterField::Until(n) => builder.until(n),
            FilterField::Limit(n) => builder.limit(n),
            FilterField::Search(search) => builder.search(search),
            FilterField::Relays(_) => {
                let elements = filter
                    .data
                    .elements(index)
                    .expect("relay field should still exist");
                builder.relays(str_elements(elements))
            }
            FilterField::Custom(_) => return None,
        };
    }

    if !authors.is_empty() {
        builder = builder.authors(authors.iter().map(Pubkey::bytes));
    }

    Some(builder.build())
}

fn str_elements<'a>(
    elements: impl IntoIterator<Item = FilterElement<'a>>,
) -> impl Iterator<Item = &'a str> {
    elements.into_iter().filter_map(|element| match element {
        FilterElement::Str(value) => Some(value),
        _ => None,
    })
}

fn copy_tag_field<'a>(
    mut builder: nostrdb::FilterBuilder,
    tag: char,
    elements: impl IntoIterator<Item = FilterElement<'a>>,
) -> nostrdb::FilterBuilder {
    builder.start_tags_field(tag).expect("start tag field");
    for element in elements {
        match element {
            FilterElement::Id(id) => builder.add_id_element(id).expect("copy tag id"),
            FilterElement::Str(value) => builder.add_str_element(value).expect("copy tag str"),
            FilterElement::Int(value) => builder.add_int_element(value).expect("copy tag int"),
            FilterElement::Custom => {}
        }
    }
    builder.end_field();
    builder
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::author_outbox::RelayDirectorySnapshot;
    use crate::test_utils::{nip65_note_for_test, wait_for_nip65_for_test};
    use nostrdb::Config;
    use serde_json::Value;
    use tempfile::TempDir;

    fn route_filter(filter: &Filter, directory: &impl RelayDirectoryRead) -> Vec<RoutedFilter> {
        route_filter_at_index(0, filter, directory)
    }

    fn relay(url: &str) -> NormRelayUrl {
        NormRelayUrl::new(url).expect("valid relay")
    }

    fn authors_json(filter: &Filter) -> Vec<String> {
        let json = filter.json().expect("filter json");
        let value: Value = serde_json::from_str(&json).expect("filter json value");
        let mut authors = value["authors"]
            .as_array()
            .expect("authors array")
            .iter()
            .map(|entry| entry.as_str().expect("author hex").to_owned())
            .collect::<Vec<_>>();
        authors.sort();
        authors
    }

    fn new_ndb() -> (TempDir, nostrdb::Ndb) {
        let tmp = TempDir::new().expect("tmp dir");
        let ndb =
            nostrdb::Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
        (tmp, ndb)
    }

    fn local_relay_list_fixture(
        ndb: &mut nostrdb::Ndb,
        authors: impl IntoIterator<Item = Pubkey>,
    ) -> RelayDirectorySnapshot {
        let authors = authors.into_iter().collect::<hashbrown::HashSet<_>>();
        RelayDirectorySnapshot::from_ndb_authors(ndb, &authors)
    }

    #[test]
    fn route_filter_emits_relay_sized_filters_and_preserves_other_fields() {
        let (_tmp, mut ndb) = new_ndb();
        let relay_a = relay("wss://relay-a.example.com");
        let relay_b = relay("wss://relay-b.example.com");
        let author_a = enostr::FullKeypair::generate();
        let author_b = enostr::FullKeypair::generate();
        let author_c = enostr::FullKeypair::generate();
        let pk_a = author_a.pubkey;
        let pk_b = author_b.pubkey;
        let pk_c = author_c.pubkey;

        let filter = Filter::new()
            .authors([pk_a.bytes(), pk_b.bytes(), pk_c.bytes()])
            .kinds([1])
            .limit(25)
            .build();

        ndb.process_client_event(
            &nip65_note_for_test(&author_a, &[("wss://relay-a.example.com", Some("write"))])
                .json()
                .expect("json"),
        )
        .expect("ingest");
        ndb.process_client_event(
            &nip65_note_for_test(&author_b, &[("wss://relay-a.example.com", Some("write"))])
                .json()
                .expect("json"),
        )
        .expect("ingest");
        ndb.process_client_event(
            &nip65_note_for_test(&author_c, &[("wss://relay-b.example.com", Some("write"))])
                .json()
                .expect("json"),
        )
        .expect("ingest");
        wait_for_nip65_for_test(&ndb, &pk_a);
        wait_for_nip65_for_test(&ndb, &pk_b);
        wait_for_nip65_for_test(&ndb, &pk_c);
        let directory = local_relay_list_fixture(&mut ndb, [pk_a, pk_b, pk_c]);

        let routed = route_filter(&filter, &directory);

        assert_eq!(routed.len(), 2);

        let relay_a_filter = routed
            .iter()
            .find(|r| r.relay == relay_a)
            .expect("relay-a filter");
        let relay_a_filter = relay_a_filter.filter.clone();
        let mut relay_a_expected = vec![pk_a.hex(), pk_b.hex()];
        relay_a_expected.sort();
        assert_eq!(authors_json(&relay_a_filter), relay_a_expected);
        assert_eq!(relay_a_filter.limit(), Some(25));

        let relay_b_filter = routed
            .iter()
            .find(|r| r.relay == relay_b)
            .expect("relay-b filter");
        let relay_b_filter = relay_b_filter.filter.clone();
        assert_eq!(authors_json(&relay_b_filter), vec![pk_c.hex()]);
        assert_eq!(relay_b_filter.limit(), Some(25));
    }

    #[test]
    fn route_filter_keeps_large_author_sets_together_per_relay() {
        let (_tmp, mut ndb) = new_ndb();
        let relay_a = relay("wss://relay-a.example.com");
        let authors = (0..65)
            .map(|_| enostr::FullKeypair::generate())
            .collect::<Vec<_>>();
        let pubkeys = authors
            .iter()
            .map(|author| author.pubkey)
            .collect::<Vec<_>>();
        let filter = Filter::new()
            .authors(pubkeys.iter().map(Pubkey::bytes))
            .kinds([1])
            .limit(25)
            .build();

        for author in &authors {
            ndb.process_client_event(
                &nip65_note_for_test(author, &[("wss://relay-a.example.com", Some("write"))])
                    .json()
                    .expect("json"),
            )
            .expect("ingest");
        }
        for pubkey in &pubkeys {
            wait_for_nip65_for_test(&ndb, pubkey);
        }
        let directory = local_relay_list_fixture(&mut ndb, pubkeys.iter().copied());

        let routed = route_filter(&filter, &directory);

        assert_eq!(routed.len(), 1);
        assert!(routed.iter().all(|routed| routed.relay == relay_a));
        let mut expected_authors = pubkeys.iter().map(Pubkey::hex).collect::<Vec<_>>();
        expected_authors.sort();
        assert_eq!(authors_json(&routed[0].filter), expected_authors);
    }

    #[test]
    fn route_filter_omits_explicit_none_and_missing_authors() {
        let (_tmp, mut ndb) = new_ndb();
        let author_known = enostr::FullKeypair::generate();
        let author_none = enostr::FullKeypair::generate();
        let author_missing = enostr::FullKeypair::generate();
        let pk_known = author_known.pubkey;
        let pk_none = author_none.pubkey;
        let pk_missing = author_missing.pubkey;
        let filter = Filter::new()
            .authors([pk_known.bytes(), pk_none.bytes(), pk_missing.bytes()])
            .kinds([1])
            .limit(25)
            .build();

        ndb.process_client_event(
            &nip65_note_for_test(
                &author_known,
                &[("wss://relay-known.example.com", Some("write"))],
            )
            .json()
            .expect("json"),
        )
        .expect("ingest");
        ndb.process_client_event(
            &nip65_note_for_test(
                &author_none,
                &[("wss://relay-readonly.example.com", Some("read"))],
            )
            .json()
            .expect("json"),
        )
        .expect("ingest");
        wait_for_nip65_for_test(&ndb, &pk_known);
        wait_for_nip65_for_test(&ndb, &pk_none);

        let directory = local_relay_list_fixture(&mut ndb, [pk_known, pk_none]);

        let routed = route_filter(&filter, &directory);

        let known_filter = routed
            .iter()
            .find(|r| r.relay == relay("wss://relay-known.example.com"))
            .expect("known filter");
        let known_filter = known_filter.filter.clone();
        assert_eq!(authors_json(&known_filter), vec![pk_known.hex()]);
        assert_eq!(routed.len(), 1);
    }

    #[test]
    fn route_filter_refuses_custom_filters() {
        let (_tmp, mut ndb) = new_ndb();
        let author = enostr::FullKeypair::generate();
        let pubkey = author.pubkey;
        let filter = Filter::new()
            .authors([pubkey.bytes()])
            .kinds([1])
            .custom(|_| true)
            .build();

        ndb.process_client_event(
            &nip65_note_for_test(&author, &[("wss://relay-known.example.com", Some("write"))])
                .json()
                .expect("json"),
        )
        .expect("ingest");
        wait_for_nip65_for_test(&ndb, &pubkey);

        let directory = local_relay_list_fixture(&mut ndb, [pubkey]);

        let routed = route_filter(&filter, &directory);

        assert!(routed.is_empty());
    }

    #[test]
    fn route_filter_preserves_non_author_fields_when_rewriting_authors() {
        let (_tmp, mut ndb) = new_ndb();
        let relay_a = relay("wss://relay-a.example.com");
        let author_a = enostr::FullKeypair::generate();
        let author_b = enostr::FullKeypair::generate();
        let pk_a = author_a.pubkey;
        let pk_b = author_b.pubkey;

        let filter = Filter::new()
            .authors([pk_a.bytes(), pk_b.bytes()])
            .kinds([1, 42])
            .since(11)
            .until(22)
            .limit(33)
            .search("hello world")
            .tags(["reply-root", "reply-branch"], 'e')
            .relays(["wss://relay-hint.example.com"])
            .build();

        ndb.process_client_event(
            &nip65_note_for_test(&author_a, &[("wss://relay-a.example.com", Some("write"))])
                .json()
                .expect("json"),
        )
        .expect("ingest");
        ndb.process_client_event(
            &nip65_note_for_test(&author_b, &[("wss://relay-a.example.com", Some("write"))])
                .json()
                .expect("json"),
        )
        .expect("ingest");
        wait_for_nip65_for_test(&ndb, &pk_a);
        wait_for_nip65_for_test(&ndb, &pk_b);

        let directory = local_relay_list_fixture(&mut ndb, [pk_a, pk_b]);

        let routed = route_filter(&filter, &directory);
        assert_eq!(routed.len(), 1);
        let routed_filter = routed
            .iter()
            .find(|r| r.relay == relay_a)
            .expect("relay-a filter")
            .filter
            .clone();
        let routed_json = routed_filter.json().expect("filter json");
        let routed_value: Value = serde_json::from_str(&routed_json).expect("filter json value");

        assert_eq!(routed_filter.limit(), Some(33));
        assert_eq!(routed_filter.since(), Some(11));
        assert_eq!(routed_filter.until(), Some(22));
        let mut expected_authors = vec![pk_a.hex(), pk_b.hex()];
        expected_authors.sort();
        assert_eq!(authors_json(&routed_filter), expected_authors);
        assert_eq!(routed_value["search"].as_str(), Some("hello world"));

        let mut tag_values = routed_value["#e"]
            .as_array()
            .expect("e tag array")
            .iter()
            .map(|value| value.as_str().expect("tag string").to_owned())
            .collect::<Vec<_>>();
        tag_values.sort();
        assert_eq!(tag_values, vec!["reply-branch", "reply-root"]);

        let relays = routed_value["relays"]
            .as_array()
            .expect("relay hints")
            .iter()
            .map(|value| value.as_str().expect("relay hint"))
            .collect::<Vec<_>>();
        assert_eq!(relays, vec!["wss://relay-hint.example.com"]);
    }
}
