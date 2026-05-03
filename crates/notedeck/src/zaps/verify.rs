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

        // extract recipient pubkey from p-tag
        let Some(recipient) = get_p_tag(note) else {
            tracing::warn!("zap verify: no p-tag on 9735 note");
            return;
        };
        let recipient = Pubkey::new(*recipient);

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

fn get_p_tag<'a>(note: &nostrdb::Note<'a>) -> Option<&'a [u8; 32]> {
    for tag in note.tags() {
        if tag.count() >= 2 {
            if let Some("p") = tag.get_str(0) {
                return tag.get_id(1);
            }
        }
    }
    None
}
