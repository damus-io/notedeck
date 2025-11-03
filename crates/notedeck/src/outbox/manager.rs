use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use enostr::{PubkeyRef, RelayPool};
use nostrdb::{Ndb, Transaction};

use super::{OutboxRelayIndex, RelayHint};

#[derive(Debug, Default)]
pub struct RelaySelection {
    pub relays: Vec<String>,
    pub per_author: HashMap<[u8; 32], Vec<String>>,
}

impl RelaySelection {
    pub fn is_empty(&self) -> bool {
        self.relays.is_empty()
    }

    pub fn len(&self) -> usize {
        self.relays.len()
    }
}

#[derive(Default)]
struct RelayAggregate {
    score: u32,
    authors: usize,
}

impl RelayAggregate {
    fn add_hint(&mut self, hint: &RelayHint) {
        self.score += hint.score as u32;
        self.authors = self.authors.saturating_add(1);
    }
}

pub struct OutboxManager {
    enabled: bool,
    connection_budget: usize,
    index: OutboxRelayIndex,
    ephemeral_relays: HashMap<String, usize>,
    active_requests: HashMap<String, Vec<String>>,
}

impl OutboxManager {
    pub fn new(
        ttl: Duration,
        max_relays_per_author: usize,
        query_limit: usize,
        connection_budget: usize,
    ) -> Self {
        Self {
            enabled: true,
            connection_budget,
            index: OutboxRelayIndex::new(ttl, max_relays_per_author, query_limit),
            ephemeral_relays: HashMap::new(),
            active_requests: HashMap::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, value: bool) {
        self.enabled = value;
    }

    pub fn toggle(&mut self) {
        self.enabled = !self.enabled;
    }

    pub fn connection_budget(&self) -> usize {
        self.connection_budget
    }

    pub fn set_connection_budget(&mut self, budget: usize) {
        self.connection_budget = budget.max(1);
    }

    pub fn index_mut(&mut self) -> &mut OutboxRelayIndex {
        &mut self.index
    }

    pub fn record_event_delivery(&mut self, author: PubkeyRef<'_>, relay: &str) {
        self.index.record_observed(author, relay);
    }

    pub fn clear_cache(&mut self) {
        self.index.clear();
    }

    pub fn resolve_relays_for_authors<'a, I>(
        &mut self,
        txn: &Transaction,
        ndb: &Ndb,
        authors: I,
    ) -> RelaySelection
    where
        I: IntoIterator<Item = PubkeyRef<'a>>,
    {
        if !self.enabled {
            return RelaySelection::default();
        }

        let mut aggregates: HashMap<String, RelayAggregate> = HashMap::new();
        let mut per_author: HashMap<[u8; 32], Vec<String>> = HashMap::new();

        for author in authors {
            let hints = self.index.hints_for_author(txn, ndb, author);
            if hints.is_empty() {
                continue;
            }

            let author_key = *author.bytes();
            let author_entry = per_author.entry(author_key).or_default();

            for hint in hints {
                author_entry.push(hint.url.clone());
                aggregates
                    .entry(hint.url.clone())
                    .or_default()
                    .add_hint(&hint);
            }
        }

        if aggregates.is_empty() {
            return RelaySelection::default();
        }

        let mut scored: Vec<(String, RelayAggregate)> = aggregates.into_iter().collect();
        scored.sort_by(|a, b| {
            b.1.score
                .cmp(&a.1.score)
                .then_with(|| b.1.authors.cmp(&a.1.authors))
                .then_with(|| a.0.cmp(&b.0))
        });

        if scored.len() > self.connection_budget {
            scored.truncate(self.connection_budget);
        }

        let relays = scored.into_iter().map(|(url, _)| url).collect();

        RelaySelection { relays, per_author }
    }

    pub fn ensure_connections(
        &mut self,
        pool: &mut RelayPool,
        relays: &[String],
        wakeup: impl Fn() + Send + Sync + Clone + 'static,
    ) -> Vec<String> {
        let mut touched = Vec::new();

        for relay in relays {
            if self.ephemeral_relays.contains_key(relay) {
                if let Some(count) = self.ephemeral_relays.get_mut(relay) {
                    *count = count.saturating_add(1);
                }
                touched.push(relay.clone());
                continue;
            }

            if pool.has(relay) {
                continue;
            }

            if let Err(err) = pool.add_url(relay.clone(), wakeup.clone()) {
                tracing::warn!("outbox: failed to add relay {relay}: {err}");
                continue;
            }

            self.ephemeral_relays.insert(relay.clone(), 1);
            touched.push(relay.clone());
        }

        touched
    }

    pub fn begin_request(&mut self, sub_id: &str, relays: Vec<String>) {
        if relays.is_empty() {
            return;
        }
        self.active_requests
            .entry(sub_id.to_string())
            .or_default()
            .extend(relays);
    }

    pub fn finish_request(&mut self, pool: &mut RelayPool, sub_id: &str) {
        let Some(relays) = self.active_requests.remove(sub_id) else {
            return;
        };
        self.release_connections(pool, &relays);
    }

    fn release_connections(&mut self, pool: &mut RelayPool, relays: &[String]) {
        let mut to_remove: BTreeSet<String> = BTreeSet::new();

        for relay in relays {
            if let Some(entry) = self.ephemeral_relays.get_mut(relay) {
                if entry.saturating_sub(1) == 0 {
                    self.ephemeral_relays.remove(relay);
                    to_remove.insert(relay.clone());
                } else {
                    *entry -= 1;
                }
            }
        }

        if !to_remove.is_empty() {
            pool.remove_urls(&to_remove);
        }
    }
}

impl Default for OutboxManager {
    fn default() -> Self {
        const DEFAULT_TTL_SECS: u64 = 300;
        const DEFAULT_MAX_PER_AUTHOR: usize = 6;
        const DEFAULT_QUERY_LIMIT: usize = 16;
        const DEFAULT_CONNECTION_BUDGET: usize = 12;

        Self::new(
            Duration::from_secs(DEFAULT_TTL_SECS),
            DEFAULT_MAX_PER_AUTHOR,
            DEFAULT_QUERY_LIMIT,
            DEFAULT_CONNECTION_BUDGET,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use enostr::Pubkey;
    use nostrdb::{Config, Ndb};
    use tempfile::tempdir;

    #[test]
    fn observed_relays_surface_in_selection() {
        let tmp = tempdir().expect("tempdir");
        let db_path = tmp.path().join("outbox_ndb");
        let mut ndb = Ndb::new(db_path.to_str().unwrap(), &Config::new()).expect("ndb");
        let txn = Transaction::new(&ndb).expect("txn");

        let mut manager = OutboxManager::default();
        let pk = Pubkey::new([7u8; 32]);
        manager.record_event_delivery(pk.as_ref(), "wss://example.com");

        let selection =
            manager.resolve_relays_for_authors(&txn, &ndb, std::iter::once(pk.as_ref()));

        assert_eq!(selection.relays.len(), 1);
        assert_eq!(selection.relays[0], "wss://example.com");
    }
}
