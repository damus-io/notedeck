/// Namecoin NIP-05 resolution via ElectrumX.
///
/// Provides censorship-resistant NIP-05 identity verification using the
/// Namecoin blockchain. Users can set their `nip05` field to a `.bit` domain
/// (e.g. `alice@example.bit`) or direct Namecoin name (`d/example`, `id/alice`)
/// and notedeck will resolve the pubkey mapping via ElectrumX instead of HTTP.
///
/// Ported from Amethyst's Namecoin NIP-05 feature:
/// - https://github.com/vitorpamplona/amethyst/pull/1734
/// - https://github.com/vitorpamplona/amethyst/pull/1771
pub mod cache;
pub mod electrumx;
pub mod identifier;

use std::sync::mpsc::{self, Receiver, Sender};

use enostr::Pubkey;

use self::cache::NamecoinLookupCache;
use self::electrumx::{default_servers, ElectrumxServer};
use self::identifier::NamecoinIdentifier;

/// The result of resolving a Namecoin identifier to a Nostr pubkey.
#[derive(Debug, Clone)]
pub struct NamecoinResolveResult {
    pub pubkey: Pubkey,
    pub identifier: NamecoinIdentifier,
}

/// Why a Namecoin resolution returned no pubkey.
#[derive(Debug, Clone)]
pub enum NamecoinResolveError {
    /// The name doesn't exist on the blockchain.
    NameNotFound,
    /// All ElectrumX servers were unreachable or returned errors.
    ServersUnreachable(String),
}

/// Status of a Namecoin resolution request.
#[derive(Debug, Clone)]
pub enum NamecoinResolveStatus {
    Resolving,
    Resolved(Result<NamecoinResolveResult, NamecoinResolveError>),
}

struct ResolveCompletion {
    /// The cache key (original input string).
    cache_key: String,
    /// The resolved pubkey, or an error describing what went wrong.
    result: Result<Pubkey, NamecoinResolveError>,
    /// The identifier that was resolved.
    identifier: NamecoinIdentifier,
}

/// Service for resolving Namecoin identifiers to Nostr pubkeys.
///
/// Uses an async background task (via `tokio::spawn`) to perform ElectrumX
/// lookups without blocking the UI thread. Results are polled each frame.
pub struct NamecoinResolver {
    cache: NamecoinLookupCache,
    tx: Sender<ResolveCompletion>,
    rx: Receiver<ResolveCompletion>,
    servers: Vec<ElectrumxServer>,
    /// Currently in-flight resolution requests (by cache key).
    pending: std::collections::HashSet<String>,
}

impl Default for NamecoinResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl NamecoinResolver {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            cache: NamecoinLookupCache::new(),
            tx,
            rx,
            servers: default_servers(),
            pending: std::collections::HashSet::new(),
        }
    }

    /// Set custom ElectrumX servers.
    pub fn set_servers(&mut self, servers: Vec<ElectrumxServer>) {
        self.servers = servers;
    }

    /// Try to resolve a Namecoin identifier. Returns immediately with cached
    /// result or `Resolving` status if a background lookup is in flight.
    ///
    /// Call `poll()` each frame to process completed lookups.
    pub fn resolve(&mut self, input: &str) -> NamecoinResolveStatus {
        let Some(identifier) = NamecoinIdentifier::parse(input) else {
            return NamecoinResolveStatus::Resolved(Err(NamecoinResolveError::NameNotFound));
        };

        let cache_key = input.to_string();

        // Check cache first
        if let Some(entry) = self.cache.get(&cache_key) {
            return match (&entry.pubkey, &entry.error) {
                (Some(pk), _) => NamecoinResolveStatus::Resolved(Ok(NamecoinResolveResult {
                    pubkey: *pk,
                    identifier,
                })),
                (_, Some(err)) => NamecoinResolveStatus::Resolved(Err(err.clone())),
                _ => NamecoinResolveStatus::Resolved(Err(NamecoinResolveError::NameNotFound)),
            };
        }

        // Already resolving?
        if self.pending.contains(&cache_key) {
            return NamecoinResolveStatus::Resolving;
        }

        // Start background resolution
        self.pending.insert(cache_key.clone());
        let tx = self.tx.clone();
        let servers = self.servers.clone();
        let id = identifier.clone();
        let id_for_completion = identifier;

        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                resolve_identifier(&servers, &id)
            })
            .await
            .unwrap_or(Err(NamecoinResolveError::ServersUnreachable("task failed".to_string())));

            let _ = tx.send(ResolveCompletion {
                cache_key,
                result,
                identifier: id_for_completion,
            });
        });

        NamecoinResolveStatus::Resolving
    }

    /// Poll for completed background resolutions. Call this each frame.
    pub fn poll(&mut self) {
        while let Ok(completion) = self.rx.try_recv() {
            self.pending.remove(&completion.cache_key);
            self.cache
                .insert(completion.cache_key, completion.result);
        }
    }

    /// Check if a string looks like a Namecoin identifier.
    pub fn is_namecoin_identifier(input: &str) -> bool {
        NamecoinIdentifier::is_namecoin_identifier(input)
    }
}

/// Resolve a Namecoin identifier to a Nostr pubkey using ElectrumX.
fn resolve_identifier(servers: &[ElectrumxServer], id: &NamecoinIdentifier) -> Result<Pubkey, NamecoinResolveError> {
    tracing::info!("Namecoin: resolving '{}' (local_part: '{}')", id.name, id.local_part);
    let result = electrumx::name_show(servers, &id.name);

    match result {
        Ok(name_result) => {
            tracing::info!(
                "Namecoin: got value for '{}' at height {}: {}",
                id.name,
                name_result.height,
                &name_result.value
            );
            let pubkey = extract_pubkey_from_value(&name_result.value, &id.local_part);
            if let Some(pk) = pubkey {
                tracing::info!("Namecoin: resolved '{}' → {}", id.name, pk.hex());
                Ok(pk)
            } else {
                tracing::warn!(
                    "Namecoin: no pubkey found for local_part '{}' in value: {}",
                    id.local_part,
                    &name_result.value
                );
                Err(NamecoinResolveError::NameNotFound)
            }
        }
        Err(e) => {
            tracing::warn!("Namecoin: resolution failed for '{}': {}", id.name, e);
            match e {
                electrumx::ElectrumxError::NameNotFound => Err(NamecoinResolveError::NameNotFound),
                _ => Err(NamecoinResolveError::ServersUnreachable(e.to_string())),
            }
        }
    }
}

/// Extract a Nostr pubkey from a Namecoin name's on-chain JSON value.
///
/// Supports two value formats:
///
/// **Simple format** (pubkey directly in `nostr` field):
/// ```json
/// {"nostr": "hexencodedpubkey"}
/// ```
///
/// **Extended NIP-05-like format** (with names mapping):
/// ```json
/// {"nostr": {"names": {"alice": "hexencodedpubkey", "_": "hexdefault"}}}
/// ```
///
/// For root lookups (local_part == "_"), falls back to the first available
/// entry if `_` is not present (fix from Amethyst PR #1771).
fn extract_pubkey_from_value(value: &str, local_part: &str) -> Option<Pubkey> {
    let json: serde_json::Value = serde_json::from_str(value).ok()?;

    let nostr_field = json.get("nostr")?;

    // Simple format: {"nostr": "hexkey"}
    if let Some(hex_str) = nostr_field.as_str() {
        return pubkey_from_hex(hex_str);
    }

    // Extended format: {"nostr": {"names": {"user": "hexkey"}}}
    if let Some(nostr_obj) = nostr_field.as_object() {
        if let Some(names) = nostr_obj.get("names").and_then(|n| n.as_object()) {
            // Try the requested local part first
            if let Some(pk_val) = names.get(local_part) {
                return pubkey_from_json_value(pk_val);
            }

            // Try "_" fallback for non-root lookups
            if local_part != "_" {
                if let Some(pk_val) = names.get("_") {
                    return pubkey_from_json_value(pk_val);
                }
            } else {
                // Root lookup: fall back to first available entry when "_" is absent.
                // This is the fix from Amethyst PR #1771 (bug #3).
                if let Some((_, first_val)) = names.iter().next() {
                    return pubkey_from_json_value(first_val);
                }
            }
        }
    }

    None
}

fn pubkey_from_hex(hex_str: &str) -> Option<Pubkey> {
    let bytes = hex::decode(hex_str).ok()?;
    if bytes.len() == 32 {
        let arr: [u8; 32] = bytes.try_into().ok()?;
        Some(Pubkey::new(arr))
    } else {
        None
    }
}

fn pubkey_from_json_value(val: &serde_json::Value) -> Option<Pubkey> {
    val.as_str().and_then(pubkey_from_hex)
}

/// Validate a NIP-05 `.bit` domain against the Namecoin blockchain.
///
/// Returns `true` if the pubkey matches the on-chain record.
pub fn validate_nip05_bit(pubkey: &Pubkey, nip05: &str) -> bool {
    let Some(identifier) = NamecoinIdentifier::parse(nip05) else {
        return false;
    };

    let servers = default_servers();
    let result = resolve_identifier(&servers, &identifier);

    match result {
        Ok(resolved_pk) => resolved_pk == *pubkey,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_simple_format() {
        let value = r#"{"nostr": "6cdebcca09b3a31f72db57e7038c9f07de579defe9a44a4e44e2a18667d"}"#;
        // This hex is 29 bytes, not 32, so it won't parse as a valid pubkey
        assert!(extract_pubkey_from_value(value, "_").is_none());

        // Valid 32-byte hex
        let hex32 = "a".repeat(64);
        let value = format!(r#"{{"nostr": "{}"}}"#, hex32);
        let pk = extract_pubkey_from_value(&value, "_");
        assert!(pk.is_some());
    }

    #[test]
    fn test_extract_extended_format() {
        let hex32 = "b".repeat(64);
        let value = format!(
            r#"{{"nostr": {{"names": {{"m": "{}"}}}}}}"#,
            hex32
        );

        // Lookup by name
        let pk = extract_pubkey_from_value(&value, "m");
        assert!(pk.is_some());

        // Root lookup should fall back to first entry (PR #1771 fix)
        let pk_root = extract_pubkey_from_value(&value, "_");
        assert!(pk_root.is_some());
    }

    #[test]
    fn test_extract_with_underscore_key() {
        let hex_a = "a".repeat(64);
        let hex_b = "b".repeat(64);
        let value = format!(
            r#"{{"nostr": {{"names": {{"_": "{}", "alice": "{}"}}}}}}"#,
            hex_a, hex_b
        );

        let pk_root = extract_pubkey_from_value(&value, "_");
        assert!(pk_root.is_some());
        assert_eq!(pk_root.unwrap().hex(), hex_a);

        let pk_alice = extract_pubkey_from_value(&value, "alice");
        assert!(pk_alice.is_some());
        assert_eq!(pk_alice.unwrap().hex(), hex_b);
    }

    #[test]
    fn test_non_root_no_fallback() {
        // Non-root lookups should NOT fall back to first entry (PR #1771 fix)
        let hex = "c".repeat(64);
        let value = format!(
            r#"{{"nostr": {{"names": {{"m": "{}"}}}}}}"#,
            hex
        );

        let pk = extract_pubkey_from_value(&value, "nonexistent");
        assert!(pk.is_none());
    }
}
