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

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use enostr::{NormRelayUrl, Pubkey, RelayId};
use hashbrown::HashSet;
use nostrdb::{Filter, Ndb, NoteKey, Subscription, Transaction};
use nostrdb_net::relay::sync::Session;

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
    fan_out_unseen_notes_with(ndb, txn, keys, relays, |json, targets| {
        api.publish_event_json(json, targets)
    });
}

/// The seen-on/`is_rumor` fan-out logic shared by the outbox path
/// ([`fan_out_unseen_notes`]) and the host [`Session`] path
/// ([`HostPrivateSync`]). For each key it resolves the note, skips unwrapped
/// rumors, computes the private relays it hasn't been seen on, and hands the
/// note's JSON plus those targets to `publish` — the only difference between the
/// two callers being which transport `publish` writes to.
fn fan_out_unseen_notes_with(
    ndb: &Ndb,
    txn: &Transaction,
    keys: &[NoteKey],
    relays: &[RelayId],
    mut publish: impl FnMut(String, Vec<RelayId>),
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
        publish(json, targets);
    }
}

/// The stable subscription id the host's PNS [`Session`] declares on every
/// private relay. It is a single logical subscription (one filter set) fanned
/// across the relay set, so dropping it closes the PNS `REQ` on all of them.
const HOST_PNS_SUB_ID: &str = "host/pns";

/// The unlinkable pubkey that signs (and thus authors) the account's kind-1080
/// PNS envelopes, HKDF-derived from the account secret
/// (`enostr::pns::derive_pns_keys`). Every device for the same account derives the
/// same pubkey, so this names the account's private-note stream. A PNS envelope
/// wraps an account-private inner note — notebook longform, a dave session state,
/// a headway board-pref — NIP-44 encrypted to that keypair; relays only ever see
/// the opaque envelope, and nostrdb (seeded with the account key at sign-in)
/// auto-unwraps it on ingest so the inner note becomes queryable by its own kind.
fn pns_author(account_secret: &[u8; 32]) -> Pubkey {
    enostr::pns::derive_pns_keys(account_secret).keypair.pubkey
}

/// Filter for the account's kind-1080 PNS envelope stream, authored by
/// [`pns_author`]. Full-history: no `since`/time window — the negentropy backfill
/// only transfers the envelopes this device lacks, so bounding the window would
/// just risk dropping older private notes for no bandwidth saving. Because the
/// derived author is a pure function of the account secret, this single filter
/// pulls back *every* app's private notes for the account.
fn pns_envelope_filter(pns_pubkey: &Pubkey) -> Filter {
    Filter::new()
        .kinds([enostr::pns::PNS_KIND as u64])
        .authors([pns_pubkey.bytes()])
        .build()
}

/// The host's account-wide private-note sync, run from [`Notedeck`](crate::Notedeck)
/// independent of whichever app is foregrounded.
///
/// The host owns a long-lived [`Session`] over its own [`RelayPool`] — a small,
/// dedicated pool for the account's 1–2 private-sync relays, separate from the
/// app read/write outbox — and feeds it the account's kind-1080 PNS envelope
/// filter ([`pns_envelope_filter`]). That covers *both* directions for every
/// app's private notes at once:
///
/// - **inbound** — a live `REQ` plus a NIP-77 negentropy backfill pull the
///   account's envelopes into the local nostrdb, where nostrdb auto-unwraps them;
///   apps then read the inner notes with plain local queries.
/// - **outbound** — a local subscription over the same envelope stream drives an
///   [`is_rumor`](nostrdb::Note::is_rumor)-guarded fan-out of freshly-authored
///   envelopes (e.g. a notebook longform created on this device) out to the
///   private relays via [`Session::publish`].
///
/// Which filters to sync and the fan-out guard are host policy and live here; the
/// [`Session`] itself is kind-agnostic. With no private relay marked the relay set
/// is empty and both directions are no-ops, so the account stays purely local.
pub struct HostPrivateSync {
    /// The long-lived sync loop over the account's private relays. Lazily spawned
    /// on the first [`update`](Self::update) where a Tokio runtime exists (it is
    /// absent under the test harness, which keeps the host inert there). Held
    /// behind an `Arc` so a settle watcher can be spawned onto the runtime with
    /// its own handle.
    session: Option<Arc<Session>>,
    /// Local subscription over the account's kind-1080 envelope stream, re-created
    /// when the selected account changes. Polled each frame to drive the outbound
    /// fan-out.
    local_sub: Option<Subscription>,
    /// The `(account, sorted private relay urls)` the remote subscription was last
    /// declared for, so we only re-declare (and re-arm the settle watcher) on a
    /// real change rather than every frame.
    declared: Option<(Pubkey, Vec<NormRelayUrl>)>,
    /// Whether the history backfill for the current declaration has settled — read
    /// by apps via [`AppContext::private_sync_settled`](crate::AppContext). Starts
    /// `true` (nothing declared ⇒ nothing pending), flips `false` on each
    /// (re)declaration, and latches back `true` when that declaration's backfill
    /// completes. Shared with the settle watcher task.
    settled: Arc<AtomicBool>,
    /// Monotonic declaration generation. Each (re)declaration bumps it and spawns a
    /// watcher capturing the new value; a watcher only latches [`settled`](Self::settled)
    /// if its generation is still current, so a stale watcher from a superseded
    /// declaration (e.g. after an account switch) can't mark a fresh sync settled.
    settle_gen: Arc<AtomicU64>,
}

impl Default for HostPrivateSync {
    fn default() -> Self {
        Self::new()
    }
}

impl HostPrivateSync {
    /// A host sync that has not yet declared anything: inert until the first
    /// [`update`](Self::update) resolves an account and (optionally) its relays.
    pub fn new() -> Self {
        Self {
            session: None,
            local_sub: None,
            declared: None,
            // Nothing declared yet ⇒ nothing to reconcile ⇒ settled.
            settled: Arc::new(AtomicBool::new(true)),
            settle_gen: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Whether the current PNS declaration's history backfill has settled.
    ///
    /// `true` when local-only (no private relay) or once the backfill has
    /// reconciled; `false` in the window between (re)declaring the subscription
    /// and its settle. Apps gate work that must not act on a mid-sync view (e.g.
    /// dave's deleted-session litter avoidance) on this via
    /// [`AppContext::private_sync_settled`](crate::AppContext).
    pub fn settled(&self) -> bool {
        self.settled.load(Ordering::Acquire)
    }

    /// Bring the host sync in line with the selected account: (re)declare the PNS
    /// envelope subscription over `private_urls` and fan any freshly-authored local
    /// envelopes out to them. Cheap to call every frame — the remote declaration is
    /// deduped on `(account, urls)` and only the small fan-out poll runs otherwise.
    ///
    /// `account`/`account_secret` are the selected account's pubkey and secret;
    /// `private_urls` its marked private-sync relays (empty ⇒ local-only). Must be
    /// called from within a Tokio runtime for the first, session-spawning call to
    /// take effect; without one it is a no-op (the test harness runs no runtime).
    pub fn update(
        &mut self,
        ndb: &mut Ndb,
        account: &Pubkey,
        account_secret: &[u8; 32],
        private_urls: &[NormRelayUrl],
    ) {
        // Lazily spawn the session loop. `Session::new` `tokio::spawn`s, so it
        // needs a runtime; under the test harness there is none, so the host stays
        // inert (and `settled` stays `true`, i.e. never blocks an app).
        if self.session.is_none() {
            if tokio::runtime::Handle::try_current().is_err() {
                return;
            }
            self.session = Some(Arc::new(Session::new(ndb.clone())));
        }
        let session = self.session.clone().expect("session just ensured");
        let pns_pubkey = pns_author(account_secret);

        // Re-create the local envelope sub when the account changes: its filter is
        // keyed on the account-derived PNS author, so a switch renames the stream.
        let account_changed = self.declared.as_ref().map(|(a, _)| a) != Some(account);
        if account_changed {
            if let Some(old) = self.local_sub.take() {
                let _ = ndb.unsubscribe(old);
            }
            self.local_sub = ndb.subscribe(&[pns_envelope_filter(&pns_pubkey)]).ok();
        }

        // (Re)declare the remote subscription on any account/relay-set change.
        if self.declared.as_ref() != Some(&(*account, private_urls.to_vec())) {
            self.redeclare(&session, &pns_pubkey, private_urls);
            self.declared = Some((*account, private_urls.to_vec()));
        }

        self.fan_out_local_envelopes(ndb, &session, private_urls);
    }

    /// Replace the PNS declaration: close the prior `REQ` on every relay and, when
    /// the account still has private relays, open a fresh live + backfilling
    /// subscription over the current set, arming a generation-guarded settle
    /// watcher. With no private relays this is the teardown to local-only.
    fn redeclare(&mut self, session: &Arc<Session>, pns_pubkey: &Pubkey, urls: &[NormRelayUrl]) {
        session.drop_subscription(HOST_PNS_SUB_ID);
        if urls.is_empty() {
            // Local-only: nothing to reconcile, so the view is trivially settled.
            self.settled.store(true, Ordering::Release);
            return;
        }

        // One logical subscription (same id + filter) fanned across the relay set;
        // the same filter drives the live `REQ` and the history backfill.
        let filter = pns_envelope_filter(pns_pubkey);
        for url in urls {
            session.set_subscription(
                HOST_PNS_SUB_ID,
                url.to_string(),
                vec![filter.clone()],
                vec![filter.clone()],
            );
        }

        // Mid-sync until the backfill settles. Bump the generation and spawn a
        // watcher that latches `settled` only if it is still the current
        // declaration when the backfill completes.
        self.settled.store(false, Ordering::Release);
        let gen = self.settle_gen.fetch_add(1, Ordering::AcqRel) + 1;
        let session = session.clone();
        let settled = self.settled.clone();
        let settle_gen = self.settle_gen.clone();
        tokio::spawn(async move {
            session.wait_for_sync().await;
            if settle_gen.load(Ordering::Acquire) == gen {
                settled.store(true, Ordering::Release);
            }
        });
    }

    /// Poll the local envelope subscription and fan freshly-authored envelopes out
    /// to the private relays they have not been seen on yet, via [`Session::publish`].
    ///
    /// The seen-on check ([`fan_out_unseen_notes_with`]) keeps an envelope pulled
    /// *in* by the inbound leg from being echoed straight back out, and the
    /// `is_rumor` guard keeps a sealed rumor from ever leaking in the clear. Even
    /// with no private relay we still drain the poll so a later-marked relay does
    /// not receive an unbounded backlog dump in one frame.
    fn fan_out_local_envelopes(&self, ndb: &Ndb, session: &Session, urls: &[NormRelayUrl]) {
        let Some(sub) = self.local_sub else {
            return;
        };
        let keys = ndb.poll_for_notes(sub, 64);
        if keys.is_empty() || urls.is_empty() {
            return;
        }
        let relays: Vec<RelayId> = urls.iter().cloned().map(RelayId::Websocket).collect();
        let Ok(txn) = Transaction::new(ndb) else {
            return;
        };
        fan_out_unseen_notes_with(ndb, &txn, &keys, &relays, |json, targets| {
            let target_urls: Vec<String> = targets
                .into_iter()
                .filter_map(|relay| match relay {
                    RelayId::Websocket(url) => Some(url.to_string()),
                    RelayId::Multicast => None,
                })
                .collect();
            if !target_urls.is_empty() {
                session.publish(json, target_urls);
            }
        });
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

    // ===== HostPrivateSync =====

    /// Fixed timestamp for the test envelope so its id is deterministic and no
    /// wall clock is read.
    const HOST_TEST_TS: u64 = 1_700_000_000;

    /// Build the kind-1080 PNS envelope wrapping a signed inner kind-1 note for
    /// `secret`'s account, returning the `["EVENT", {…}]` ingest frame, the
    /// envelope id, and the inner note id.
    fn pns_envelope_frame(secret: &[u8; 32]) -> (String, [u8; 32], [u8; 32]) {
        let pns_keys = enostr::pns::derive_pns_keys(secret);
        let inner = NoteBuilder::new()
            .kind(1)
            .content("private longform body")
            .created_at(HOST_TEST_TS)
            .sign(secret)
            .build()
            .expect("inner note");
        let inner_id = *inner.id();
        let envelope =
            enostr::pns::wrap(&pns_keys, &inner.json().expect("inner json"), HOST_TEST_TS)
                .expect("pns envelope");
        let envelope_id = *envelope.id();
        let frame = format!("[\"EVENT\",{}]", envelope.json().expect("envelope json"));
        (frame, envelope_id, inner_id)
    }

    /// Whether `ndb` holds note `id` yet (queryable == committed).
    fn ndb_has(ndb: &Ndb, id: &[u8; 32]) -> bool {
        Transaction::new(ndb)
            .ok()
            .is_some_and(|txn| ndb.get_note_by_id(&txn, id).is_ok())
    }

    /// End-to-end host sync over a shared private relay: device A fans a freshly
    /// authored PNS envelope out to the relay, device B backfills it and
    /// auto-unwraps the inner note — the notebook-longform cross-device path,
    /// exercised at the `HostPrivateSync` level with the source device backgrounded
    /// (we just pump `update`, no app).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn host_sync_delivers_pns_envelope_between_devices() {
        use nostrdb_net::relay::server;
        use std::time::Duration;

        // A shared private relay backed by its own opaque db (never seeded with the
        // account key, so it only ever holds the envelope, never the inner note).
        let (_relay_dir, relay_ndb) = test_ndb();
        let relay = server::spawn(relay_ndb.clone(), "127.0.0.1:0".parse().expect("addr"))
            .expect("spawn relay");
        let url = NormRelayUrl::new(&relay.url()).expect("relay url");
        let relays = std::slice::from_ref(&url);

        let account = FullKeypair::generate();
        let secret = account.secret_key.secret_bytes();
        let (frame, envelope_id, inner_id) = pns_envelope_frame(&secret);

        // Device A: declare the host sub first (so its fan-out poll observes the
        // envelope), then author the envelope into the local db.
        let (_a_dir, mut ndb_a) = test_ndb();
        ndb_a.add_key(&secret);
        let mut host_a = HostPrivateSync::new();
        host_a.update(&mut ndb_a, &account.pubkey, &secret, relays);
        // A 2-element client frame (`["EVENT",{…}]`) is a locally-authored event,
        // so ingest it as one — `process_event` expects the 3-element relay form.
        ndb_a
            .process_client_event(&frame)
            .expect("ingest envelope on A");

        // Pump A until the relay has stored the fanned-out envelope.
        let mut fanned = false;
        for _ in 0..500 {
            host_a.update(&mut ndb_a, &account.pubkey, &secret, relays);
            if ndb_has(&relay_ndb, &envelope_id) {
                fanned = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            fanned,
            "device A should fan its PNS envelope out to the private relay"
        );

        // Device B: backfill the account's private stream; nostrdb auto-unwraps the
        // envelope (B is seeded with the account key) so the inner note is queryable.
        let (_b_dir, mut ndb_b) = test_ndb();
        ndb_b.add_key(&secret);
        let mut host_b = HostPrivateSync::new();
        let mut delivered = false;
        for _ in 0..500 {
            host_b.update(&mut ndb_b, &account.pubkey, &secret, relays);
            if ndb_has(&ndb_b, &inner_id) {
                delivered = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            delivered,
            "device B should backfill the envelope and auto-unwrap the inner note"
        );

        relay.shutdown();
    }

    /// With no private relay marked the host stays local-only and immediately
    /// reports settled — an app gating on it is never blocked by a sync that isn't
    /// running.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn host_sync_local_only_is_settled() {
        let account = FullKeypair::generate();
        let secret = account.secret_key.secret_bytes();
        let (_dir, mut ndb) = test_ndb();

        let mut host = HostPrivateSync::new();
        assert!(host.settled(), "a fresh host has nothing pending");
        host.update(&mut ndb, &account.pubkey, &secret, &[]);
        assert!(
            host.settled(),
            "no private relay ⇒ nothing to reconcile ⇒ still settled"
        );
    }
}
