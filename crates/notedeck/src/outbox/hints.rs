use std::collections::HashMap;
use std::time::{Duration, Instant};

use enostr::PubkeyRef;
use nostrdb::{Filter, Ndb, NoteKey, Transaction};

use crate::account::relay::AccountRelayData;

/// Different ways we learn about a relay for an author. Higher quality
/// inputs get higher base scores so they win ties when we rank relays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintSource {
    Nip65Read,
    Nip65Bidirectional,
    Observed,
}

/// Lightweight record describing one relay candidate for a pubkey. We keep
/// this struct intentionally small since the index refreshes every few
/// frames inside egui.
#[derive(Debug, Clone)]
pub struct RelayHint {
    pub url: String,
    pub score: u8,
    pub source: HintSource,
    pub updated_at: Instant,
}

impl RelayHint {
    fn observed(url: impl Into<String>) -> Self {
        RelayHint {
            url: url.into(),
            score: OBSERVED_SCORE,
            source: HintSource::Observed,
            updated_at: Instant::now(),
        }
    }
}

/// Cached lookup result for a single author. The TTL keeps us from hammering
/// nostrdb every frame while still letting the in-memory hints stay fresh.
#[derive(Debug)]
struct CacheEntry {
    hints: Vec<RelayHint>,
    fetched_at: Instant,
}

impl CacheEntry {
    fn new(hints: Vec<RelayHint>) -> Self {
        CacheEntry {
            hints,
            fetched_at: Instant::now(),
        }
    }

    fn is_fresh(&self, ttl: Duration) -> bool {
        self.fetched_at.elapsed() < ttl
    }

    fn update_timestamp(&mut self) {
        self.fetched_at = Instant::now();
    }

    fn upsert_observed(&mut self, relay: &str) {
        let mut replaced = false;
        for hint in &mut self.hints {
            if hint.url == relay {
                if hint.score < OBSERVED_SCORE {
                    hint.score = OBSERVED_SCORE;
                    hint.source = HintSource::Observed;
                }
                hint.updated_at = Instant::now();
                replaced = true;
                break;
            }
        }

        if !replaced {
            self.hints.push(RelayHint::observed(relay));
        }

        self.hints
            .sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.url.cmp(&b.url)));
        self.update_timestamp();
    }
}

const OBSERVED_SCORE: u8 = 4;

pub struct OutboxRelayIndex {
    ttl: Duration,
    max_per_author: usize,
    query_limit: usize,
    cache: HashMap<[u8; 32], CacheEntry>,
}

impl OutboxRelayIndex {
    pub fn new(ttl: Duration, max_per_author: usize, query_limit: usize) -> Self {
        Self {
            ttl,
            max_per_author,
            query_limit,
            cache: HashMap::new(),
        }
    }

    pub fn set_ttl(&mut self, ttl: Duration) {
        self.ttl = ttl;
    }

    pub fn hints_for_author(
        &mut self,
        txn: &Transaction,
        ndb: &Ndb,
        author: PubkeyRef<'_>,
    ) -> Vec<RelayHint> {
        let key = *author.bytes();

        if let Some(entry) = self.cache.get(&key) {
            if entry.is_fresh(self.ttl) {
                return entry.hints.clone();
            }
        }

        let hints = self.fetch_nip65_hints(txn, ndb, author);
        self.cache.insert(key, CacheEntry::new(hints.clone()));
        hints
    }

    pub fn record_observed(&mut self, author: PubkeyRef<'_>, relay: &str) {
        let key = *author.bytes();
        let entry = self
            .cache
            .entry(key)
            .or_insert_with(|| CacheEntry::new(Vec::new()));
        entry.upsert_observed(relay);
    }

    pub fn clear(&mut self) {
        self.cache.clear();
    }

    fn fetch_nip65_hints(
        &self,
        txn: &Transaction,
        ndb: &Ndb,
        author: PubkeyRef<'_>,
    ) -> Vec<RelayHint> {
        let filter = Filter::new()
            .authors([author.bytes()])
            .kinds([10002])
            .limit(self.query_limit as u64)
            .build();

        let limit = filter.limit().unwrap_or(self.query_limit as u64) as i32;

        let Ok(results) = ndb.query(txn, std::slice::from_ref(&filter), limit) else {
            return Vec::new();
        };

        let note_keys: Vec<NoteKey> = results.iter().map(|qr| qr.note_key).collect();

        let mut dedup = HashMap::<String, RelayHint>::new();
        let now = Instant::now();

        for spec in AccountRelayData::harvest_nip65_relays(ndb, txn, &note_keys) {
            if !spec.is_readable() {
                continue;
            }

            let (score, source) = if spec.has_read_marker {
                (3, HintSource::Nip65Read)
            } else {
                (2, HintSource::Nip65Bidirectional)
            };

            let hint = RelayHint {
                url: spec.url.clone(),
                score,
                source,
                updated_at: now,
            };

            dedup
                .entry(spec.url)
                .and_modify(|existing| {
                    if existing.score < hint.score {
                        *existing = hint.clone();
                    } else {
                        existing.updated_at = now;
                    }
                })
                .or_insert(hint);
        }

        let mut hints: Vec<RelayHint> = dedup.into_values().collect();
        hints.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.url.cmp(&b.url)));

        if hints.len() > self.max_per_author {
            hints.truncate(self.max_per_author);
        }

        hints
    }
}
