use enostr::{NormRelayUrl, RelayUrlSource};
use hashbrown::{HashMap, HashSet};
use nostrdb::Filter;
use std::{cmp::Ordering, collections::BinaryHeap};

use super::routing::{filter_author_pubkeys, route_filter_at_index};
use super::{RelayDirectoryRead, RoutedFilter, RoutedRelayPriority};

/// Plans additive routed coverage for pre-indexed author filters.
pub(crate) fn plan_author_outbox_augmentation_for_indexed_filters<'a, D, I>(
    filters: I,
    directory: &D,
    baseline_relays: &HashSet<NormRelayUrl>,
) -> Vec<RoutedFilter>
where
    D: RelayDirectoryRead + ?Sized,
    I: IntoIterator<Item = (usize, &'a Filter)>,
{
    let mut filters_by_relay: HashMap<NormRelayUrl, Vec<RoutedFilter>> = HashMap::new();

    for (filter_index, filter) in filters {
        // Augmentation planning only routes authors whose write relays are
        // already known locally. Blocked RemoteAdvertised relays are removed
        // before routed relay output so they cannot consume routed relay work.
        for routed in route_filter_at_index(filter_index, filter, directory) {
            if baseline_relays.contains(&routed.relay)
                || !routed
                    .relay
                    .allowed_for_source(RelayUrlSource::RemoteAdvertised)
            {
                continue;
            }
            filters_by_relay
                .entry(routed.relay.clone())
                .or_default()
                .push(routed);
        }
    }

    flatten_routed_filter_buckets(filters_by_relay)
}

/// Assign final relay ranking across one or more route sets from the same frozen plan.
pub(crate) fn rank_author_outbox_routes(route_sets: &mut [&mut Vec<RoutedFilter>]) {
    let priorities = relay_priorities(route_sets);
    for routes in route_sets.iter_mut() {
        for routed in routes.iter_mut() {
            routed.relay_priority = priorities.get(&routed.relay).copied().unwrap_or_default();
        }
        routes.sort_by(|left, right| {
            left.relay_priority
                .order
                .cmp(&right.relay_priority.order)
                .then_with(|| left.relay.cmp(&right.relay))
                .then_with(|| left.filter_index.cmp(&right.filter_index))
        });
    }
}

/// Flatten relay buckets back into deterministic planner output.
fn flatten_routed_filter_buckets(
    filters_by_relay: HashMap<NormRelayUrl, Vec<RoutedFilter>>,
) -> Vec<RoutedFilter> {
    let mut filters_by_relay = filters_by_relay.into_iter().collect::<Vec<_>>();
    filters_by_relay.sort_unstable_by(|left, right| left.0.cmp(&right.0));

    let mut routed = Vec::new();
    for (_relay, filters) in filters_by_relay {
        routed.extend(filters);
    }
    routed
}

#[derive(Clone)]
struct RelayCoverage {
    relay: NormRelayUrl,
    authors: HashSet<enostr::Pubkey>,
    uncovered_count: usize,
    selected: bool,
}

#[derive(Clone, Eq, PartialEq)]
struct RelayCoverageCandidate {
    uncovered_count: usize,
    total_author_count: usize,
    relay_order: usize,
    index: usize,
}

impl Ord for RelayCoverageCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.uncovered_count
            .cmp(&other.uncovered_count)
            .then_with(|| self.total_author_count.cmp(&other.total_author_count))
            .then_with(|| other.relay_order.cmp(&self.relay_order))
    }
}

impl PartialOrd for RelayCoverageCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn relay_priorities(
    route_sets: &[&mut Vec<RoutedFilter>],
) -> HashMap<NormRelayUrl, RoutedRelayPriority> {
    let mut coverage_by_relay: HashMap<NormRelayUrl, HashSet<enostr::Pubkey>> = HashMap::new();
    for routes in route_sets {
        for routed in routes.iter() {
            coverage_by_relay
                .entry(routed.relay.clone())
                .or_default()
                .extend(filter_author_pubkeys(&routed.filter));
        }
    }

    let mut remaining = coverage_by_relay
        .into_iter()
        .map(|(relay, authors)| RelayCoverage {
            relay,
            uncovered_count: authors.len(),
            authors,
            selected: false,
        })
        .collect::<Vec<_>>();
    remaining.sort_unstable_by(|left, right| left.relay.cmp(&right.relay));

    let mut relays_by_author = HashMap::<enostr::Pubkey, Vec<usize>>::new();
    let mut candidates = BinaryHeap::new();
    for (index, coverage) in remaining.iter().enumerate() {
        for author in &coverage.authors {
            relays_by_author.entry(*author).or_default().push(index);
        }
        candidates.push(relay_coverage_candidate(index, coverage));
    }

    let mut covered_authors = HashSet::new();
    let mut priorities = HashMap::with_capacity(remaining.len());
    let mut order = 0usize;
    while let Some(candidate) = candidates.pop() {
        if !relay_coverage_candidate_is_current(&candidate, &remaining[candidate.index]) {
            continue;
        }

        let selected_index = candidate.index;
        let connection_weight = remaining[selected_index].uncovered_count;
        let newly_covered = remaining[selected_index]
            .authors
            .iter()
            .filter(|author| covered_authors.insert(**author))
            .copied()
            .collect::<Vec<_>>();
        remaining[selected_index].selected = true;

        priorities.insert(
            remaining[selected_index].relay.clone(),
            RoutedRelayPriority {
                connection_weight: connection_weight.try_into().unwrap_or(u32::MAX),
                order,
            },
        );
        order = order.saturating_add(1);

        for author in newly_covered {
            let Some(relay_indexes) = relays_by_author.get(&author) else {
                continue;
            };

            for relay_index in relay_indexes {
                let coverage = &mut remaining[*relay_index];
                if coverage.selected || coverage.uncovered_count == 0 {
                    continue;
                }
                coverage.uncovered_count -= 1;
                candidates.push(relay_coverage_candidate(*relay_index, coverage));
            }
        }
    }

    priorities
}

fn relay_coverage_candidate(index: usize, coverage: &RelayCoverage) -> RelayCoverageCandidate {
    RelayCoverageCandidate {
        uncovered_count: coverage.uncovered_count,
        total_author_count: coverage.authors.len(),
        relay_order: index,
        index,
    }
}

fn relay_coverage_candidate_is_current(
    candidate: &RelayCoverageCandidate,
    coverage: &RelayCoverage,
) -> bool {
    !coverage.selected
        && coverage.uncovered_count == candidate.uncovered_count
        && coverage.authors.len() == candidate.total_author_count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::author_outbox::RelayDirectorySnapshot;
    use crate::test_utils::{nip65_note_for_test, wait_for_nip65_for_test};
    use nostrdb::Config;
    use serde_json::Value;
    use tempfile::TempDir;

    fn relay(url: &str) -> NormRelayUrl {
        NormRelayUrl::new(url).expect("valid relay")
    }

    fn pubkey(byte: u8) -> enostr::Pubkey {
        enostr::Pubkey::new([byte; 32])
    }

    fn author_filter(authors: &[enostr::Pubkey]) -> Filter {
        Filter::new()
            .authors(authors.iter().map(enostr::Pubkey::bytes))
            .kinds([1])
            .limit(20)
            .build()
    }

    fn routed_filter(url: &str, authors: &[enostr::Pubkey]) -> RoutedFilter {
        RoutedFilter {
            relay: relay(url),
            filter_index: 0,
            filter: author_filter(authors),
            relay_priority: RoutedRelayPriority::default(),
        }
    }

    fn plan_author_outbox_augmentation<D>(
        filters: &[Filter],
        directory: &D,
        baseline_relays: &HashSet<NormRelayUrl>,
    ) -> Vec<RoutedFilter>
    where
        D: RelayDirectoryRead + ?Sized,
    {
        plan_author_outbox_augmentation_for_indexed_filters(
            filters.iter().enumerate(),
            directory,
            baseline_relays,
        )
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

    fn local_relay_list_fixture(
        ndb: &mut nostrdb::Ndb,
        authors: impl IntoIterator<Item = enostr::Pubkey>,
    ) -> RelayDirectorySnapshot {
        let authors = authors.into_iter().collect::<HashSet<_>>();
        RelayDirectorySnapshot::from_ndb_authors(ndb, &authors)
    }

    #[test]
    fn prioritize_routes_orders_by_marginal_author_coverage() {
        let alice = pubkey(0xA1);
        let bob = pubkey(0xB2);
        let cass = pubkey(0xC3);
        let dana = pubkey(0xD4);
        let mut routes = vec![
            routed_filter("wss://relay-c.example.com", &[dana]),
            routed_filter("wss://relay-b.example.com", &[bob, cass]),
            routed_filter("wss://relay-a.example.com", &[alice, bob]),
        ];

        let mut route_sets = [&mut routes];
        rank_author_outbox_routes(&mut route_sets);

        let ranked = routes
            .iter()
            .map(|routed| {
                (
                    routed.relay.to_string(),
                    routed.relay_priority.connection_weight,
                    routed.relay_priority.order,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            ranked,
            vec![
                ("wss://relay-a.example.com/".to_owned(), 2, 0),
                ("wss://relay-b.example.com/".to_owned(), 1, 1),
                ("wss://relay-c.example.com/".to_owned(), 1, 2),
            ]
        );
    }

    #[test]
    fn plan_omits_augmentation_filters_already_covered_by_baseline_relays() {
        let tmp = TempDir::new().expect("tmp dir");
        let mut ndb =
            nostrdb::Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
        let erin = enostr::FullKeypair::generate();
        let alice = enostr::FullKeypair::generate();
        let bob = enostr::FullKeypair::generate();
        let cass = enostr::FullKeypair::generate();

        for note in [
            nip65_note_for_test(
                &erin,
                &[
                    ("wss://relay-b.example.com", Some("write")),
                    ("wss://relay-c.example.com", Some("write")),
                ],
            ),
            nip65_note_for_test(&alice, &[("wss://relay-c.example.com", Some("write"))]),
            nip65_note_for_test(&bob, &[("wss://relay-a.example.com", Some("write"))]),
            nip65_note_for_test(&cass, &[("wss://relay-b.example.com", Some("write"))]),
        ] {
            ndb.process_client_event(&note.json().expect("json"))
                .expect("ingest");
        }
        for pubkey in [erin.pubkey, alice.pubkey, bob.pubkey, cass.pubkey] {
            wait_for_nip65_for_test(&ndb, &pubkey);
        }

        let directory = local_relay_list_fixture(
            &mut ndb,
            [erin.pubkey, alice.pubkey, bob.pubkey, cass.pubkey],
        );
        let filter = Filter::new()
            .authors([
                erin.pubkey.bytes(),
                alice.pubkey.bytes(),
                bob.pubkey.bytes(),
                cass.pubkey.bytes(),
            ])
            .kinds([1])
            .limit(20)
            .build();
        let routed = plan_author_outbox_augmentation(
            &[filter],
            &directory,
            &HashSet::from_iter([relay("wss://relay-a.example.com")]),
        );

        let routed_relays = routed
            .iter()
            .map(|routed| routed.relay.clone())
            .collect::<HashSet<_>>();
        assert_eq!(
            routed_relays,
            HashSet::from_iter([
                relay("wss://relay-b.example.com"),
                relay("wss://relay-c.example.com")
            ])
        );
        assert_eq!(routed.len(), 2);
        let relay_b_authors = routed
            .iter()
            .find(|routed| routed.relay == relay("wss://relay-b.example.com"))
            .map(|routed| authors_json(&routed.filter))
            .expect("relay-b routed filter");
        assert_eq!(
            relay_b_authors.into_iter().collect::<HashSet<_>>(),
            HashSet::from_iter([cass.pubkey.hex(), erin.pubkey.hex()])
        );
        let relay_c_authors = routed
            .iter()
            .find(|routed| routed.relay == relay("wss://relay-c.example.com"))
            .map(|routed| authors_json(&routed.filter))
            .expect("relay-c routed filter");
        assert_eq!(
            relay_c_authors.into_iter().collect::<HashSet<_>>(),
            HashSet::from_iter([alice.pubkey.hex(), erin.pubkey.hex()])
        );
    }

    #[test]
    fn plan_excludes_baseline_and_keeps_all_allowed_routed_relays() {
        let tmp = TempDir::new().expect("tmp dir");
        let mut ndb =
            nostrdb::Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
        let shared_relay = relay("wss://relay-shared.example.com");
        let baseline_relay = relay("wss://relay-000.example.com");
        let shared_authors = (0..65)
            .map(|_| enostr::FullKeypair::generate())
            .collect::<Vec<_>>();
        let unique_authors = (0..20)
            .map(|_| enostr::FullKeypair::generate())
            .collect::<Vec<_>>();

        for author in &shared_authors {
            ndb.process_client_event(
                &nip65_note_for_test(author, &[("wss://relay-shared.example.com", Some("write"))])
                    .json()
                    .expect("json"),
            )
            .expect("ingest shared");
        }
        for (index, author) in unique_authors.iter().enumerate() {
            let relay_url = format!("wss://relay-{index:03}.example.com");
            ndb.process_client_event(
                &nip65_note_for_test(author, &[(&relay_url, Some("write"))])
                    .json()
                    .expect("json"),
            )
            .expect("ingest unique");
        }

        let pubkeys = shared_authors
            .iter()
            .chain(unique_authors.iter())
            .map(|author| author.pubkey)
            .collect::<Vec<_>>();
        for pubkey in &pubkeys {
            wait_for_nip65_for_test(&ndb, pubkey);
        }

        let directory = local_relay_list_fixture(&mut ndb, pubkeys.iter().copied());
        let filter = Filter::new()
            .authors(pubkeys.iter().map(|pubkey| pubkey.bytes()))
            .kinds([1])
            .limit(20)
            .build();

        let routed = plan_author_outbox_augmentation(
            &[filter],
            &directory,
            &HashSet::from_iter([baseline_relay.clone()]),
        );

        let routed_relays = routed
            .iter()
            .map(|routed| routed.relay.clone())
            .collect::<HashSet<_>>();
        assert_eq!(routed_relays.len(), 20);
        assert!(!routed_relays.contains(&baseline_relay));
        assert!(routed_relays.contains(&shared_relay));

        let shared_filters = routed
            .iter()
            .filter(|routed| routed.relay == shared_relay)
            .collect::<Vec<_>>();
        assert_eq!(shared_filters.len(), 1);
        assert_eq!(authors_json(&shared_filters[0].filter).len(), 65);
    }

    #[test]
    fn plan_keeps_all_routed_relays_without_baseline_coverage() {
        let tmp = TempDir::new().expect("tmp dir");
        let mut ndb =
            nostrdb::Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
        let authors = (0..20)
            .map(|_| enostr::FullKeypair::generate())
            .collect::<Vec<_>>();

        for (index, author) in authors.iter().enumerate() {
            let relay_url = format!("wss://relay-{index:03}.example.com");
            ndb.process_client_event(
                &nip65_note_for_test(author, &[(&relay_url, Some("write"))])
                    .json()
                    .expect("json"),
            )
            .expect("ingest unique");
        }

        let pubkeys = authors
            .iter()
            .map(|author| author.pubkey)
            .collect::<Vec<_>>();
        for pubkey in &pubkeys {
            wait_for_nip65_for_test(&ndb, pubkey);
        }

        let directory = local_relay_list_fixture(&mut ndb, pubkeys.iter().copied());
        let filter = Filter::new()
            .authors(pubkeys.iter().map(|pubkey| pubkey.bytes()))
            .kinds([1])
            .limit(20)
            .build();

        let routed = plan_author_outbox_augmentation(&[filter], &directory, &HashSet::new());

        let routed_relays = routed
            .iter()
            .map(|routed| routed.relay.clone())
            .collect::<HashSet<_>>();
        assert_eq!(routed_relays.len(), 20);
    }

    #[test]
    fn plan_filters_blocked_remote_advertised_relays_before_routed_output() {
        let tmp = TempDir::new().expect("tmp dir");
        let mut ndb =
            nostrdb::Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
        let blocked_authors = (0..20)
            .map(|_| enostr::FullKeypair::generate())
            .collect::<Vec<_>>();
        let allowed_author = enostr::FullKeypair::generate();

        for (index, author) in blocked_authors.iter().enumerate() {
            let relay_url = format!("ws://127.0.0.1:{}", 7000 + index);
            ndb.process_client_event(
                &nip65_note_for_test(author, &[(&relay_url, Some("write"))])
                    .json()
                    .expect("json"),
            )
            .expect("ingest blocked");
        }
        ndb.process_client_event(
            &nip65_note_for_test(
                &allowed_author,
                &[("wss://relay-allowed.example.com", Some("write"))],
            )
            .json()
            .expect("json"),
        )
        .expect("ingest allowed");

        let pubkeys = blocked_authors
            .iter()
            .map(|author| author.pubkey)
            .chain([allowed_author.pubkey])
            .collect::<Vec<_>>();
        for pubkey in &pubkeys {
            wait_for_nip65_for_test(&ndb, pubkey);
        }

        let directory = local_relay_list_fixture(&mut ndb, pubkeys.iter().copied());
        let filter = Filter::new()
            .authors(pubkeys.iter().map(|pubkey| pubkey.bytes()))
            .kinds([1])
            .limit(20)
            .build();

        let routed = plan_author_outbox_augmentation(
            &[filter],
            &directory,
            &HashSet::from_iter([relay("wss://relay-baseline.example.com")]),
        );

        assert_eq!(routed.len(), 1);
        assert_eq!(routed[0].relay, relay("wss://relay-allowed.example.com"));
        assert_eq!(
            authors_json(&routed[0].filter),
            vec![allowed_author.pubkey.hex()]
        );
    }
}
