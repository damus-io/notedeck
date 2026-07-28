use std::collections::HashSet;
use std::sync::mpsc;

use enostr::Pubkey;
use nostrdb::{Ndb, Transaction};
use url::Url;

use crate::zaps::networking::{
    convert_lnurl_to_endpoint_url, fetch_pay_req_async, generate_endpoint_url, LNUrlPayResponse,
};
use crate::zaps::{get_users_zap_address, ZapAddress};
use crate::ZapError;

use super::cache::PayCache;

struct ZapVerifyResult {
    zap_note_id: [u8; 32],
    zap_note_pubkey: [u8; 32],
    endpoint_url: Url,
    result: Result<LNUrlPayResponse, ZapError>,
}

pub struct ZapVerifier {
    in_flight: HashSet<[u8; 32]>,
    tx: mpsc::Sender<ZapVerifyResult>,
    rx: mpsc::Receiver<ZapVerifyResult>,
}

impl Default for ZapVerifier {
    fn default() -> Self {
        Self::new()
    }
}

impl ZapVerifier {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            in_flight: HashSet::new(),
            tx,
            rx,
        }
    }

    pub fn queue_verification(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        note: &nostrdb::Note<'_>,
        pay_cache: &mut PayCache,
    ) {
        let note_id = note.id();

        // skip if already in flight
        if self.in_flight.contains(note_id) {
            return;
        }

        // check if already verified via metadata flags
        if let Ok(mut meta) = ndb.get_note_metadata(txn, note_id) {
            let flags = *meta.flags();
            // NDB_NOTE_META_FLAG_ZAP_VERIFIED = 4
            if flags & 4 != 0 {
                return;
            }
        }

        // securely get the zap target's pubkey. The p-tag can be faked
        // on note zaps, so we look up the referenced note's actual author.
        let Some(recipient_bytes) = get_zap_target_pubkey(note, ndb, txn) else {
            tracing::warn!(
                "zap verify: can't determine recipient for 9735 note {}",
                hex::encode(note_id)
            );
            return;
        };
        let recipient = Pubkey::new(recipient_bytes);

        // look up their LNURL endpoint
        let address = match get_users_zap_address(txn, ndb, &recipient) {
            Ok(addr) => addr,
            Err(e) => {
                tracing::warn!("zap verify: can't get zap address for {recipient}: {e}");
                return;
            }
        };

        let endpoint_url = match address_to_endpoint_url(&address) {
            Ok(url) => url,
            Err(e) => {
                tracing::warn!("zap verify: can't generate endpoint URL: {e}");
                return;
            }
        };

        let zap_note_id = *note_id;
        let zap_note_pubkey = *note.pubkey();

        // check if we already have a cached response for this endpoint
        if let Some(response) = pay_cache.get_response(&endpoint_url) {
            // verify immediately
            if verify_zap_pubkey(response, &zap_note_pubkey) {
                ndb.verify_zap(txn, &zap_note_id);
                tracing::info!("zap verified (cached): {}", hex::encode(zap_note_id));
            } else {
                tracing::warn!(
                    "zap verify failed (cached): pubkey mismatch for {}",
                    hex::encode(zap_note_id)
                );
            }
            return;
        }

        // spawn async fetch
        self.in_flight.insert(zap_note_id);
        let tx = self.tx.clone();
        let url = endpoint_url.clone();

        tokio::spawn(async move {
            let result = fetch_pay_req_async(&url).await.map(|raw| raw.into());
            let _ = tx.send(ZapVerifyResult {
                zap_note_id,
                zap_note_pubkey,
                endpoint_url: url,
                result,
            });
        });
    }

    pub fn poll(&mut self, ndb: &Ndb, pay_cache: &mut PayCache) {
        while let Ok(result) = self.rx.try_recv() {
            self.in_flight.remove(&result.zap_note_id);

            match result.result {
                Ok(response) => {
                    let verified = verify_zap_pubkey(&response, &result.zap_note_pubkey);

                    // cache the response regardless of verification outcome
                    pay_cache.insert(crate::zaps::networking::PayEntry {
                        url: result.endpoint_url,
                        response,
                    });

                    if verified {
                        let txn = match Transaction::new(ndb) {
                            Ok(txn) => txn,
                            Err(e) => {
                                tracing::error!("zap verify: can't create txn: {e}");
                                continue;
                            }
                        };
                        ndb.verify_zap(&txn, &result.zap_note_id);
                        tracing::info!("zap verified: {}", hex::encode(result.zap_note_id));
                    } else {
                        tracing::warn!(
                            "zap verify failed: pubkey mismatch for {}",
                            hex::encode(result.zap_note_id)
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "zap verify: endpoint fetch failed for {}: {e}",
                        hex::encode(result.zap_note_id)
                    );
                }
            }
        }
    }
}

fn verify_zap_pubkey(response: &LNUrlPayResponse, zap_note_pubkey: &[u8; 32]) -> bool {
    if !response.allow_nostr {
        return false;
    }

    match &response.nostr_pubkey {
        Ok(pk) => pk.bytes() == zap_note_pubkey,
        Err(_) => false,
    }
}

fn address_to_endpoint_url(address: &ZapAddress) -> Result<Url, ZapError> {
    match address {
        ZapAddress::Lud16(lud16) => generate_endpoint_url(lud16),
        ZapAddress::Lud06(lnurl) => convert_lnurl_to_endpoint_url(lnurl),
    }
}

/// Securely get the zap target's pubkey. The p-tag on note zaps can be
/// faked, so we need to look up the actual note author from the database.
fn get_zap_target_pubkey(
    note: &nostrdb::Note<'_>,
    ndb: &Ndb,
    txn: &Transaction,
) -> Option<[u8; 32]> {
    let e_tag = get_single_tag_id(note, "e");

    match e_tag {
        TagMatch::None => {
            // No e-tags: this is a profile zap (p-tag only).
            // The p-tag is the only source, require exactly one.
            match get_single_tag_id(note, "p") {
                TagMatch::One(id) => Some(*id),
                _ => None,
            }
        }

        TagMatch::Many => {
            // Multiple e-tags: reject to prevent fake note zap attacks
            tracing::warn!("zap verify: rejecting zap with multiple e-tags");
            None
        }

        TagMatch::One(referenced_note_id) => {
            // We can't trust the p-tag on note zaps because it can be
            // faked. Look up the actual note to get the real author.
            match ndb.get_note_by_id(txn, referenced_note_id) {
                Ok(referenced_note) => Some(*referenced_note.pubkey()),
                Err(_) => {
                    // We don't have the referenced note in the db so
                    // we can't verify the author. Fall back to the
                    // p-tag. This leaks a bit of correctness but
                    // avoids rejecting valid zaps for notes we simply
                    // haven't seen yet.
                    tracing::debug!(
                        "zap verify: note {} not in db, falling back to p-tag",
                        hex::encode(referenced_note_id)
                    );
                    match get_single_tag_id(note, "p") {
                        TagMatch::One(id) => Some(*id),
                        _ => None,
                    }
                }
            }
        }
    }
}

enum TagMatch<T> {
    None,
    One(T),
    Many,
}

fn get_single_tag_id<'a>(note: &nostrdb::Note<'a>, tag_name: &str) -> TagMatch<&'a [u8; 32]> {
    let mut found: Option<&[u8; 32]> = None;
    for tag in note.tags() {
        if tag.count() >= 2 {
            if let Some(name) = tag.get_str(0) {
                if name == tag_name {
                    if let Some(id) = tag.get_id(1) {
                        if found.is_some() {
                            return TagMatch::Many;
                        }
                        found = Some(id);
                    }
                }
            }
        }
    }
    match found {
        Some(id) => TagMatch::One(id),
        None => TagMatch::None,
    }
}
