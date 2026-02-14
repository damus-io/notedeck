use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use enostr::Pubkey;

const NIP05_TTL: Duration = Duration::from_secs(8 * 3600); // 8 hours

#[derive(Debug, Clone, PartialEq)]
pub enum Nip05Status {
    Pending,
    Valid,
    Invalid,
}

struct CacheEntry {
    status: Nip05Status,
    checked_at: Instant,
}

struct Completion {
    pubkey: Pubkey,
    status: Nip05Status,
}

pub struct Nip05Cache {
    cache: HashMap<Pubkey, CacheEntry>,
    tx: Sender<Completion>,
    rx: Receiver<Completion>,
}

impl Default for Nip05Cache {
    fn default() -> Self {
        Self::new()
    }
}

impl Nip05Cache {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            cache: HashMap::new(),
            tx,
            rx,
        }
    }

    pub fn status(&self, pubkey: &Pubkey) -> Option<&Nip05Status> {
        self.cache.get(pubkey).map(|entry| &entry.status)
    }

    pub fn request_validation(&mut self, pubkey: Pubkey, nip05: &str) {
        if let Some(entry) = self.cache.get(&pubkey) {
            if entry.checked_at.elapsed() < NIP05_TTL {
                return;
            }
        }

        self.cache.insert(
            pubkey,
            CacheEntry {
                status: Nip05Status::Pending,
                checked_at: Instant::now(),
            },
        );

        let tx = self.tx.clone();
        let nip05 = nip05.to_string();

        tokio::spawn(async move {
            let status = validate_nip05(&pubkey, &nip05).await;
            let _ = tx.send(Completion { pubkey, status });
        });
    }

    pub fn poll(&mut self) {
        while let Ok(completion) = self.rx.try_recv() {
            self.cache.insert(
                completion.pubkey,
                CacheEntry {
                    status: completion.status,
                    checked_at: Instant::now(),
                },
            );
        }
    }
}

async fn validate_nip05(pubkey: &Pubkey, nip05: &str) -> Nip05Status {
    let Some((user, domain)) = parse_nip05(nip05) else {
        return Nip05Status::Invalid;
    };

    let url = format!("https://{}/.well-known/nostr.json?name={}", domain, user);

    let resp = match crate::media::network::http_req(&url).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!("NIP-05 validation failed for {}: {}", nip05, e);
            return Nip05Status::Invalid;
        }
    };

    let json: serde_json::Value = match serde_json::from_slice(&resp.bytes) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("NIP-05 JSON parse failed for {}: {}", nip05, e);
            return Nip05Status::Invalid;
        }
    };

    let expected_hex = pubkey.hex();

    let valid = json
        .get("names")
        .and_then(|names| names.get(user))
        .and_then(|v| v.as_str())
        .map(|hex| hex.eq_ignore_ascii_case(&expected_hex))
        .unwrap_or(false);

    if valid {
        Nip05Status::Valid
    } else {
        Nip05Status::Invalid
    }
}

fn parse_nip05(nip05: &str) -> Option<(&str, &str)> {
    let at_pos = nip05.find('@')?;
    let user = &nip05[..at_pos];
    let domain = &nip05[at_pos + 1..];

    if user.is_empty() || domain.is_empty() {
        return None;
    }

    Some((user, domain))
}
