use std::collections::{HashMap, HashSet, VecDeque};

use enostr::Pubkey;
use nostrdb::{Filter, Ndb, Note, Transaction};

/// Configuration for computing a web-of-trust graph.
/// `max_depth` counts hops away from the root (0 = root, 1 = direct follows).
#[derive(Clone, Copy, Debug)]
pub struct WebOfTrustConfig {
    pub max_depth: u8,
    pub include_self: bool,
}

impl Default for WebOfTrustConfig {
    fn default() -> Self {
        Self {
            max_depth: 2,
            include_self: true,
        }
    }
}

/// Resulting web-of-trust graph.
pub struct WebOfTrust {
    root: Pubkey,
    config: WebOfTrustConfig,
    trusted: HashSet<Pubkey>,
}

impl WebOfTrust {
    fn new(root: Pubkey, config: WebOfTrustConfig, trusted: HashSet<Pubkey>) -> Self {
        Self {
            root,
            config,
            trusted,
        }
    }

    pub fn root(&self) -> &Pubkey {
        &self.root
    }

    pub fn config(&self) -> WebOfTrustConfig {
        self.config
    }

    pub fn contains(&self, candidate: &Pubkey) -> bool {
        self.trusted.contains(candidate)
    }

    pub fn contains_bytes(&self, candidate: &[u8; 32]) -> bool {
        self.trusted.contains(&Pubkey::new(*candidate))
    }

    pub fn contains_hex(&self, candidate: &str) -> bool {
        Pubkey::from_hex(candidate)
            .map(|pk| self.trusted.contains(&pk))
            .unwrap_or(false)
    }

    pub fn len(&self) -> usize {
        self.trusted.len()
    }

    pub fn is_empty(&self) -> bool {
        self.trusted.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Pubkey> {
        self.trusted.iter()
    }

    pub fn to_hex_set(&self) -> HashSet<String> {
        self.trusted
            .iter()
            .map(|pk| hex::encode(pk.bytes()))
            .collect()
    }
}

/// Builder used to construct a reusable web-of-trust graph.
pub struct WebOfTrustBuilder<'a> {
    ndb: &'a Ndb,
    txn: &'a Transaction,
    root: Pubkey,
    config: WebOfTrustConfig,
    seed_contacts: Option<HashSet<Pubkey>>,
}

impl<'a> WebOfTrustBuilder<'a> {
    pub fn new(ndb: &'a Ndb, txn: &'a Transaction, root: Pubkey) -> Self {
        Self {
            ndb,
            txn,
            root,
            config: WebOfTrustConfig::default(),
            seed_contacts: None,
        }
    }

    pub fn max_depth(mut self, depth: u8) -> Self {
        self.config.max_depth = depth;
        self
    }

    pub fn include_self(mut self, include: bool) -> Self {
        self.config.include_self = include;
        self
    }

    pub fn with_seed_contacts(mut self, contacts: HashSet<Pubkey>) -> Self {
        self.seed_contacts = Some(contacts);
        self
    }

    pub fn build(self) -> WebOfTrust {
        let mut visited: HashSet<Pubkey> = HashSet::new();
        let mut queue: VecDeque<(Pubkey, u8)> = VecDeque::new();
        let mut cache: HashMap<Pubkey, HashSet<Pubkey>> = HashMap::new();

        if let Some(seed) = &self.seed_contacts {
            cache.insert(self.root, seed.clone());
        }

        if self.config.include_self {
            visited.insert(self.root);
        }

        queue.push_back((self.root, 0));

        while let Some((current, depth)) = queue.pop_front() {
            if depth >= self.config.max_depth {
                continue;
            }

            let contacts = fetch_contacts_for(&self, &mut cache, current);
            for contact in contacts {
                if visited.insert(contact) {
                    queue.push_back((contact, depth + 1));
                }
            }
        }

        WebOfTrust::new(self.root, self.config, visited)
    }
}

fn fetch_contacts_for(
    builder: &WebOfTrustBuilder<'_>,
    cache: &mut HashMap<Pubkey, HashSet<Pubkey>>,
    target: Pubkey,
) -> HashSet<Pubkey> {
    if let Some(existing) = cache.get(&target) {
        return existing.clone();
    }

    let filter = Filter::new()
        .authors([target.bytes()])
        .kinds([3])
        .limit(1)
        .build();

    let filters = vec![filter];
    let contacts = builder
        .ndb
        .query(builder.txn, &filters, 1)
        .ok()
        .and_then(|results| results.into_iter().next())
        .map(|result| contact_pubkeys(&result.note))
        .unwrap_or_default();

    cache.insert(target, contacts.clone());
    contacts
}

fn contact_pubkeys(note: &Note<'_>) -> HashSet<Pubkey> {
    let mut contacts = HashSet::new();
    for tag in note.tags() {
        if tag.count() < 2 {
            continue;
        }

        match tag.get_str(0) {
            Some("p") => {
                if let Some(pk) = tag.get_id(1) {
                    contacts.insert(Pubkey::new(*pk));
                }
            }
            _ => {}
        }
    }
    contacts
}
