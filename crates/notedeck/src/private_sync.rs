//! Cross-device "private relay" sync for GUI apps (headway, notebook).
//!
//! These apps back their document in the local nostrdb and surface it through a
//! local nostrdb subscription. To sync that document across the user's own
//! devices we need two directions over the account's private-sync relays — the
//! kind-10013 NIP-37 "Relay List for Private Content" (an encrypted relay list,
//! see [`crate::construct_private_relay_list_note`]):
//!
//! - **outbound** — fan each locally-ingested event out to the private relays so
//!   an edit on this device reaches the others ([`fan_out_event_frame`]);
//! - **inbound** — a scoped subscription that pulls the app's events back, both
//!   a NIP-77 full-history catch-up *and* a live REQ for realtime, so edits made
//!   on another device land in nostrdb and the local subscription surfaces them
//!   ([`PrivateRelaySync`]).
//!
//! dave already does the equivalent for its PNS session state; this is the
//! shared, domain-agnostic version for the plaintext-event apps. With no private
//! relay marked the relay set is empty and both directions are no-ops, so the
//! app stays purely local.

use enostr::{NormRelayUrl, Pubkey, RelayId};
use hashbrown::HashSet;
use nostrdb::{Filter, Ndb, NoteKey, Transaction};

use crate::{
    AppContext, ExplicitPublishApi, FullHistoryConfig, ScopedSubIdentity, SubConfig, SubKey,
    SubOwnerKey,
};

/// Fan a single locally-ingested `["EVENT", {…}]` frame out to `relays` as a
/// bare-event publish. The outbox re-frames the bare event per relay.
///
/// Shared by the headway/notebook `store::Publisher` adapters: their `ingest`
/// path hands us the framed event, we forward the inner object. An empty relay
/// set or a malformed frame is a no-op — the local ingest already happened.
pub fn fan_out_event_frame(api: &mut ExplicitPublishApi, event_frame: &str, relays: &[RelayId]) {
    if relays.is_empty() {
        return;
    }
    if let Some(event) = serde_json::from_str::<serde_json::Value>(event_frame)
        .ok()
        .and_then(|frame| frame.get(1).cloned())
    {
        api.publish_event_json(event.to_string(), relays.to_vec());
    }
}

/// Fan freshly-ingested local notes out to any private relays they have not yet
/// been seen on.
///
/// [`fan_out_event_frame`] only covers events the app *itself* authors through
/// its `store::Publisher` seam. Events written into the local nostrdb by any
/// *other* path never reach that seam — most importantly the `headway`/`notebook`
/// CLIs, which publish into notedeck's embedded relay, landing the event in the
/// board's nostrdb but never propagating it to the user's private-sync relays.
/// This is the catch-all for those: it runs off the app's ndb subscription poll,
/// so it forwards a locally-ingested note regardless of how it arrived.
///
/// `keys` are the note keys a subscription poll just reported (see
/// [`PrivateRelaySync`]). Each note is published only to the private relays it
/// has **not** already been seen on (per nostrdb's `note.relays()`), so a note
/// pulled *in* by the inbound sync is not echoed straight back out. Even if the
/// seen-on check misses (e.g. a relay-url normalization mismatch), nostrdb never
/// re-reports an event id it already holds, so a redundant publish can't spiral
/// into a loop.
pub fn fan_out_unseen_notes(
    api: &mut ExplicitPublishApi,
    ndb: &Ndb,
    txn: &Transaction,
    keys: &[NoteKey],
    relays: &[RelayId],
) {
    if relays.is_empty() || keys.is_empty() {
        return;
    }
    for &key in keys {
        let Ok(note) = ndb.get_note_by_key(txn, key) else {
            continue;
        };
        // Never fan out an unwrapped rumor in the clear. A rumor reaches nostrdb
        // sealed inside a PNS/SNS/giftwrap envelope; the *envelope* is the sync
        // unit, and nostrdb attributes the envelope's relay to the inner rumor, so
        // without this guard a sealed shared-board edit would be rebroadcast in
        // plaintext to every *other* private relay it hasn't been seen on. The
        // envelope itself is published by the app that authored it (see the SNS
        // publish path); this fan-out only carries plaintext app events.
        if note.is_rumor() {
            continue;
        }
        // Target each private relay the note hasn't been seen on yet. Both the
        // private set and a note's seen-on set are tiny (1-2 relays each), so a
        // nested linear scan beats allocating a lookup set per note. The seen-on
        // url is canonicalized before comparison so a trailing-slash difference
        // doesn't defeat the check. `targets` is the one small allocation the
        // publish API forces (`broadcast_event` takes an owned `Vec`), bounded by
        // the private relay count and only paid when a note actually needs sending.
        let targets: Vec<RelayId> = relays
            .iter()
            .filter(|relay| {
                // The private-sync set only ever holds websocket relays (it's
                // built from the kind-10013 url list), so this arm is just match
                // exhaustiveness.
                let RelayId::Websocket(url) = relay else {
                    return false;
                };
                !note
                    .relays(txn)
                    .any(|seen| NormRelayUrl::new(seen).is_ok_and(|seen| &seen == url))
            })
            .cloned()
            .collect();
        if targets.is_empty() {
            continue;
        }
        let Ok(json) = note.json() else {
            continue;
        };
        api.publish_event_json(json, targets);
    }
}

/// Declares (and tears down) the inbound private-sync subscription for one GUI
/// app, deduping the work so it only touches the outbox when the resolved
/// private relay set actually changes.
///
/// Hold one per app and call [`update`](Self::update) each frame with the app's
/// event filter; it returns the resolved private relays so the caller can reuse
/// them as outbound publish targets (see [`fan_out_event_frame`]).
pub struct PrivateRelaySync {
    /// Human-readable app name, for log lines.
    app: &'static str,
    /// Scoped-sub owner lifecycle, namespaced per app so two apps' private subs
    /// never collide on the shared outbox.
    owner: SubOwnerKey,
    /// Logical sub key under that owner.
    key: SubKey,
    /// Last resolved (selected account, private relay set, filter fingerprint),
    /// so we only re-declare (and log) on a change rather than every frame. The
    /// account is part of the key so switching accounts still re-declares even if
    /// the two accounts happen to share a private relay set; the filter
    /// fingerprint (each filter's JSON) is part of it so that a caller widening its
    /// filter set — e.g. headway accepting a new shared board and adding its
    /// envelope filter — re-declares even when the relay set is unchanged.
    last: Option<(Pubkey, Vec<NormRelayUrl>, Vec<String>)>,
}

impl PrivateRelaySync {
    /// Create a private-sync coordinator for `app` (e.g. `"headway"`,
    /// `"notebook"`). `app` seeds a stable, app-unique scoped-sub owner/key.
    pub fn new(app: &'static str) -> Self {
        Self {
            app,
            owner: SubOwnerKey::new(format!("{app}/private-sync")),
            key: SubKey::new("private-sync"),
            last: None,
        }
    }

    /// Bring the inbound subscription in line with the selected account's
    /// private relays, declaring a live + full-history scoped sub for `filters`
    /// against them (or dropping it when none are marked). Returns the resolved
    /// private relays for use as outbound publish targets.
    ///
    /// `filters` is the full set to sync — a plaintext-app filter usually passes a
    /// single one, but a shared-note app passes one per channel (e.g. headway's
    /// own-board filter plus a kind-1081 envelope filter per accepted shared
    /// board). An empty set drops the subscription, same as no private relay.
    pub fn update(&mut self, ctx: &mut AppContext, filters: Vec<Filter>) -> Vec<RelayId> {
        let relays = ctx.accounts.selected_account_private_relays();
        let urls: Vec<NormRelayUrl> = relays
            .iter()
            .filter_map(|relay| match relay {
                RelayId::Websocket(url) => Some(url.clone()),
                RelayId::Multicast => None,
            })
            .collect();

        // Fingerprint the filter set (each filter's canonical JSON) so a widened
        // set re-declares even when the relay set is unchanged.
        let filter_fp: Vec<String> = filters.iter().filter_map(|f| f.json().ok()).collect();

        // Nothing to do unless the account, its private relay set, or the filter
        // set changed. set_sub/drop_owner each re-resolve the account's read
        // relays (a hot, log-emitting path), so calling them every frame spams the
        // logs and wastes work — dedup before touching the outbox at all.
        let pubkey = *ctx.accounts.selected_account_pubkey();
        if self.last.as_ref().is_some_and(|(pk, last_urls, last_fp)| {
            *pk == pubkey
                && last_urls.as_slice() == urls.as_slice()
                && last_fp.as_slice() == filter_fp.as_slice()
        }) {
            return relays;
        }
        self.log_change(ctx, &urls);
        self.last = Some((pubkey, urls.clone(), filter_fp));

        let mut scoped = ctx.remote.scoped_subs(ctx.accounts);
        if urls.is_empty() || filters.is_empty() {
            // No private relay marked (or nothing to sync): local-only. Drop any
            // prior declaration.
            scoped.drop_owner(self.owner);
            return relays;
        }

        let config = SubConfig::live(filters.clone())
            .explicit_relays(urls.into_iter().collect::<HashSet<_>>())
            .full_history(FullHistoryConfig::new(filters))
            .build();
        let _ = scoped.set_sub(ScopedSubIdentity::account(self.owner, self.key), config);

        relays
    }

    /// Log the private relay set (and the live connection status of each) — the
    /// diagnostic for "is the private set even resolving?". The caller only
    /// invokes this on an actual change, so this never spams a line every frame.
    fn log_change(&self, ctx: &AppContext, urls: &[NormRelayUrl]) {
        if urls.is_empty() {
            tracing::info!(
                app = self.app,
                "private-sync: no private relay marked — local-only"
            );
            return;
        }

        let inspect = ctx.remote.relay_inspect();
        let infos = inspect.relay_infos();
        let statuses: Vec<String> = urls
            .iter()
            .map(|url| {
                let status = infos
                    .iter()
                    .find(|info| info.relay_url == url)
                    .map(|info| format!("{:?}", info.status))
                    .unwrap_or_else(|| "NotConnected".to_string());
                format!("{url} ({status})")
            })
            .collect();
        tracing::info!(
            app = self.app,
            relays = %statuses.join(", "),
            "private-sync: syncing against private relays"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EguiWakeup, ExplicitPublishApi};
    use enostr::{FullKeypair, NormRelayUrl, OutboxPool, OutboxSessionHandler};
    use nostrdb::{Config, IngestMetadata, NoteBuilder};
    use tempfile::TempDir;

    /// Frame a signed note as the `["EVENT", {…}]` envelope `ingest` hands the
    /// publisher.
    fn event_frame() -> String {
        let kp = FullKeypair::generate();
        let note = NoteBuilder::new()
            .kind(1)
            .content("private-sync-test")
            .sign(&kp.secret_key.to_secret_bytes())
            .build()
            .expect("note");
        let event: serde_json::Value =
            serde_json::from_str(&note.json().expect("event json")).expect("event value");
        serde_json::json!(["EVENT", event]).to_string()
    }

    /// Drive `fan_out_event_frame` and return the relay set the outbox opened.
    fn relays_opened_for(frame: &str, relays: Vec<RelayId>) -> HashSet<NormRelayUrl> {
        let mut pool = OutboxPool::default();
        {
            let mut outbox =
                OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default()));
            let mut api = ExplicitPublishApi::new(&mut outbox);
            fan_out_event_frame(&mut api, frame, &relays);
        }
        pool.websocket_statuses()
            .keys()
            .map(|url| (*url).clone())
            .collect()
    }

    /// A well-formed frame is unwrapped and published to each target relay.
    #[tokio::test]
    async fn fan_out_publishes_inner_event_to_targets() {
        let relay = NormRelayUrl::new("wss://private.example.com").expect("relay");
        let opened = relays_opened_for(&event_frame(), vec![RelayId::Websocket(relay.clone())]);
        assert_eq!(opened, HashSet::from_iter([relay]));
    }

    /// An empty relay set is a no-op — no relay connection is opened.
    #[test]
    fn fan_out_empty_relays_is_noop() {
        assert!(relays_opened_for(&event_frame(), vec![]).is_empty());
    }

    /// A malformed frame (no inner event object) opens no relay; the local ingest
    /// has already happened, so there's nothing to forward.
    #[test]
    fn fan_out_malformed_frame_is_noop() {
        let relay = RelayId::Websocket(NormRelayUrl::new("wss://private.example.com").expect("r"));
        assert!(relays_opened_for("not json", vec![relay.clone()]).is_empty());
        assert!(relays_opened_for("[\"EVENT\"]", vec![relay]).is_empty());
    }

    // ===== fan_out_unseen_notes =====

    /// A temporary nostrdb for the seen-on fan-out tests.
    fn test_ndb() -> (TempDir, Ndb) {
        let tmp = TempDir::new().expect("tmp dir");
        let ndb = Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
        (tmp, ndb)
    }

    /// Ingest one signed kind-1 note, recording it as seen on `seen_on` — mirroring
    /// how a note reaches nostrdb from a relay (the embedded relay for CLI ingests,
    /// or a private relay for inbound-synced notes).
    fn ingest_seen_on(ndb: &Ndb, seen_on: &str) {
        let kp = FullKeypair::generate();
        let note = NoteBuilder::new()
            .kind(1)
            .content("fan-out-unseen-test")
            .sign(&kp.secret_key.to_secret_bytes())
            .build()
            .expect("note");
        let json = note.json().expect("note json");
        ndb.process_event_with(&json, IngestMetadata::new().relay(seen_on))
            .expect("ingest");
    }

    /// Drive `fan_out_unseen_notes` over `keys` and return the relay set the outbox
    /// opened (i.e. the relays each note was actually published to).
    fn relays_fanned_for(
        ndb: &Ndb,
        keys: &[NoteKey],
        relays: Vec<RelayId>,
    ) -> HashSet<NormRelayUrl> {
        let mut pool = OutboxPool::default();
        {
            let mut outbox =
                OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default()));
            let mut api = ExplicitPublishApi::new(&mut outbox);
            let txn = Transaction::new(ndb).expect("txn");
            fan_out_unseen_notes(&mut api, ndb, &txn, keys, &relays);
        }
        pool.websocket_statuses()
            .keys()
            .map(|url| (*url).clone())
            .collect()
    }

    /// A note ingested from the embedded relay (as a CLI publish arrives) has not
    /// been seen on the private relay, so it's fanned out there — the bug fix.
    #[tokio::test]
    async fn unseen_note_is_fanned_out_to_private_relay() {
        let (_tmp, ndb) = test_ndb();
        let sub = ndb
            .subscribe(&[Filter::new().kinds([1]).build()])
            .expect("sub");
        let waiter = ndb.wait_for_notes(sub, 1);
        ingest_seen_on(&ndb, "ws://127.0.0.1:6677");
        let keys = waiter.await.expect("await");

        let private = NormRelayUrl::new("wss://private.example.com").expect("relay");
        let opened = relays_fanned_for(&ndb, &keys, vec![RelayId::Websocket(private.clone())]);
        assert_eq!(opened, HashSet::from_iter([private]));
    }

    /// A note already seen on the private relay (e.g. pulled in by the inbound
    /// sync) is not echoed straight back out to it.
    #[tokio::test]
    async fn note_already_seen_on_private_relay_is_not_refanned() {
        let (_tmp, ndb) = test_ndb();
        let sub = ndb
            .subscribe(&[Filter::new().kinds([1]).build()])
            .expect("sub");
        let waiter = ndb.wait_for_notes(sub, 1);
        ingest_seen_on(&ndb, "wss://private.example.com");
        let keys = waiter.await.expect("await");

        let private = NormRelayUrl::new("wss://private.example.com").expect("relay");
        let opened = relays_fanned_for(&ndb, &keys, vec![RelayId::Websocket(private)]);
        assert!(opened.is_empty());
    }

    /// An unwrapped rumor — the plaintext inner note nostrdb produces from an SNS
    /// envelope — is never fanned out, even to a private relay it hasn't been seen
    /// on. The sealed envelope is the sync unit; leaking the cleartext rumor would
    /// defeat sealed sharing entirely.
    #[tokio::test]
    async fn unwrapped_rumor_is_not_fanned_out() {
        let (_tmp, ndb) = test_ndb();
        // Register the team root so ndb auto-unwraps the envelope on ingest.
        let mut root = [0u8; 32];
        root[0] = 0x11;
        root[31] = 0x22;
        assert!(ndb.add_team_root(&root));
        let keys = enostr::sns::derive_sns_keys(&root).expect("keys");
        let member = FullKeypair::generate();
        // The rumor must be a complete signed note — nostrdb re-parses it on the
        // seal peel and requires every field but the sig/pubkey (including the id),
        // which is exactly what the SNS publish path feeds wrap_rumor.
        let rumor = NoteBuilder::new()
            .kind(1)
            .content("secret")
            .created_at(1_700_000_000)
            .sign(&member.secret_key.secret_bytes())
            .build()
            .expect("rumor")
            .json()
            .expect("rumor json");
        let envelope =
            enostr::sns::wrap_rumor(&keys, &member, &rumor, 1_700_000_000).expect("envelope");

        // Ingest the envelope; ndb peels it to the rumor. No relay is attributed,
        // so the rumor's seen-on set is empty — it *would* be fanned to the private
        // relay if not for the is_rumor guard, which is exactly what this asserts.
        let event: serde_json::Value =
            serde_json::from_str(&envelope.json().expect("json")).expect("value");
        let frame = serde_json::json!(["EVENT", "team", event]).to_string();
        ndb.process_event(&frame).expect("ingest");

        // Ingest is async on a writer thread. Poll (bounded) for the unwrapped
        // rumor to commit, nudging the late-arrival peel each round, rather than
        // awaiting a subscription — keeps the test from hanging if the peel fails.
        let rumor_key = {
            let mut found = None;
            for _ in 0..100 {
                {
                    let txn = Transaction::new(&ndb).expect("txn");
                    ndb.process_sns(&txn);
                }
                let txn = Transaction::new(&ndb).expect("txn");
                if let Ok(res) = ndb.query(&txn, &[Filter::new().kinds([1]).build()], 1) {
                    if let Some(hit) = res.first() {
                        assert!(hit.note.is_rumor(), "unwrapped note should be a rumor");
                        found = Some(hit.note_key);
                        break;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            found.expect("SNS envelope should unwrap into the inner rumor")
        };

        let private = NormRelayUrl::new("wss://private.example.com").expect("relay");
        let opened = relays_fanned_for(&ndb, &[rumor_key], vec![RelayId::Websocket(private)]);
        assert!(
            opened.is_empty(),
            "a sealed rumor must not be fanned out in the clear"
        );
    }

    /// An empty private relay set is a no-op even with fresh notes to consider.
    #[tokio::test]
    async fn fan_out_unseen_empty_relays_is_noop() {
        let (_tmp, ndb) = test_ndb();
        let sub = ndb
            .subscribe(&[Filter::new().kinds([1]).build()])
            .expect("sub");
        let waiter = ndb.wait_for_notes(sub, 1);
        ingest_seen_on(&ndb, "ws://127.0.0.1:6677");
        let keys = waiter.await.expect("await");

        assert!(relays_fanned_for(&ndb, &keys, vec![]).is_empty());
    }
}
