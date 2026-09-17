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

use enostr::{NormRelayUrl, RelayId};
use hashbrown::HashSet;
use nostrdb::{Filter, Ndb, NoteKey, Subscription, Transaction};
use nostrdb_net::relay::sync::Session;
use nostrdb_net::Pubkey;

use crate::{
    AppContext, ExplicitPublishApi, FullHistoryConfig, ScopedSubIdentity, SubConfig, SubKey,
    SubOwnerKey, SubRelayPolicy,
};

/// Errors from [`write_private_note`], the host-owned outbound write path.
#[derive(Debug, thiserror::Error)]
pub enum PrivateWriteError {
    /// PNS encryption or 1080-envelope signing failed (see [`nostrdb_net::pns::wrap`]).
    #[error("PNS wrap failed")]
    Wrap,
    /// Serializing the signed 1080 envelope back to JSON failed.
    #[error("envelope serialization failed: {0}")]
    Serialize(String),
    /// nostrdb rejected the envelope ingest frame.
    #[error("ndb ingest failed: {0}")]
    Ingest(String),
}

/// Author a private note for the selected account: PNS-wrap a signed inner event
/// into a kind-1080 envelope and ingest it into the local nostrdb.
///
/// This is the host-owned outbound write path. Apps author an inner event (a
/// fully signed nostr note JSON) and hand it here; **no app wraps 1080 envelopes
/// or runs a publish queue itself**. The single local ingest does double duty:
///
/// - nostrdb's `process_pns` unwraps the envelope, making the inner event
///   immediately queryable on this device (a local read reflects the write at
///   once, exactly as an inbound relay envelope would); and
/// - [`HostPrivateSync`] picks the freshly-ingested envelope up off its local
///   subscription poll and fans it out to the account's private relays
///   ([`HostPrivateSync::fan_out_local_envelopes`]), so the user's other devices
///   see it.
///
/// `secret_key` is the account's 32-byte device secret; its PNS keypair is
/// derived here ([`nostrdb_net::pns::derive_pns_keys`]). With no private relay marked
/// the fan-out is a no-op and the note simply stays local.
///
/// The 3-element `["EVENT","_pns",{…}]` relay frame drives nostrdb's PNS-unwrap
/// ingest path; the `"_pns"` subid is a local marker (the envelope carries no
/// seen-on relay, so the fan-out publishes it to every private relay).
///
/// Designed as the convergence point for every app's private write: dave's
/// session events today, notebook/headway private documents later.
pub fn write_private_note(
    ndb: &Ndb,
    secret_key: &[u8; 32],
    inner_json: &str,
) -> Result<(), PrivateWriteError> {
    let pns_keys = nostrdb_net::pns::derive_pns_keys(secret_key);
    let envelope = nostrdb_net::pns::wrap(&pns_keys, inner_json, crate::time::unix_time_secs())
        .ok_or(PrivateWriteError::Wrap)?;
    let envelope_json = envelope
        .json()
        .map_err(|e| PrivateWriteError::Serialize(e.to_string()))?;
    ndb.process_event(&format!("[\"EVENT\",\"_pns\",{envelope_json}]"))
        .map_err(|e| PrivateWriteError::Ingest(e.to_string()))
}

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
        // sealed 1080/1081 *envelope* is the note that actually gets fanned (by the
        // host's [`HostPrivateSync::fan_out_local_envelopes`] over its envelope
        // subs); the app-outbox caller only ever carries plaintext app events. The
        // guard makes both paths safe against a rumor key slipping in either way.
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

/// The stable subscription id the host's [`Session`] declares on every private
/// relay. It is a single logical subscription (one filter set) fanned across the
/// relay set, so dropping it closes the account's private `REQ` on all of them.
/// The one filter set carries the account's PNS 1080 stream *and* every joined
/// SNS channel's 1081/1082 streams, so one id closes everything.
const HOST_PRIVATE_SUB_ID: &str = "host/private";

/// The unlinkable pubkey that signs (and thus authors) the account's kind-1080
/// PNS envelopes, HKDF-derived from the account secret
/// (`nostrdb_net::pns::derive_pns_keys`). Every device for the same account derives the
/// same pubkey, so this names the account's private-note stream. A PNS envelope
/// wraps an account-private inner note — notebook longform, a dave session state,
/// a headway board-pref — NIP-44 encrypted to that keypair; relays only ever see
/// the opaque envelope, and nostrdb (seeded with the account key at sign-in)
/// auto-unwraps it on ingest so the inner note becomes queryable by its own kind.
fn pns_author(account_secret: &[u8; 32]) -> Pubkey {
    nostrdb_net::pns::derive_pns_keys(account_secret)
        .keypair
        .pubkey
}

/// Filter for the account's kind-1080 PNS envelope stream, authored by
/// [`pns_author`]. Full-history: no `since`/time window — the negentropy backfill
/// only transfers the envelopes this device lacks, so bounding the window would
/// just risk dropping older private notes for no bandwidth saving. Because the
/// derived author is a pure function of the account secret, this single filter
/// pulls back *every* app's private notes for the account.
fn pns_envelope_filter(pns_pubkey: &Pubkey) -> Filter {
    Filter::new()
        .kinds([nostrdb_net::pns::PNS_KIND as u64])
        .authors([pns_pubkey.bytes()])
        .build()
}

// ===== SNS (sealed shared session) roster + channel sync =====
//
// An SNS `team_root` is a shared channel secret: possession is membership in a
// shared board/session, and registering the root with [`Ndb::add_team_root`]
// makes nostrdb auto-unwrap that channel's kind-1081 envelopes. A member is added
// by gift-wrapping them a kind-1082 key-share, which nostrdb unwraps into a
// durable, queryable `1082` rumor — so the roster already lives in the db and
// rides the account's NIP-59 inbox across devices, with no disk config. Registered
// roots are ephemeral in nostrdb (they don't survive a restart), so the roster
// must be re-registered each boot/account-switch, mirroring `add_key`.
//
// These are the *sync mechanism*, generalized out of `notedeck_headway`'s `teams`
// module so the host can register roots and subscribe to their envelopes for
// *every* SNS app centrally. App-level *policy* — which board coordinate a root
// unlocks, its epoch/rotation, folding a board's edits — stays in the owning app.
// For sync we only need the root itself, so unlike headway's board-bearing `Team`
// roster these keep every key-shared root, including ones that name no board
// (harmless to register).

/// Filter for unwrapped SNS key-share rumors (kind-1082) — the shares nostrdb has
/// peeled out of gift-wraps addressed to one of our account keys. The roster is
/// derived from these (see [`registered_roots`]); a live subscription on the same
/// filter surfaces new joins from the account's other devices, and the same stream
/// scopes the outbound gift-wrap leg ([`own_selfshare_giftwraps`]).
///
/// This is a **local** stream: a 1082 only ever exists after nostrdb peels a
/// kind-1059 gift-wrap, so it is a local subscription/query filter, never a
/// meaningful remote `REQ`.
fn keyshare_filter() -> Filter {
    Filter::new()
        .kinds([nostrdb_net::sns::KEYSHARE_KIND as u64])
        .limit(500)
        .build()
}

/// Filter for a shared channel's kind-1081 envelopes, authored by `team_pubkey`
/// (the team keypair whose pubkey *is* the channel). Every sealed edit is one such
/// envelope, so this is the inbound sync stream for that channel.
fn team_envelope_filter(team_pubkey: &Pubkey) -> Filter {
    Filter::new()
        .kinds([nostrdb_net::sns::SNS_ENVELOPE_KIND as u64])
        .authors([team_pubkey.bytes()])
        .limit(5000)
        .build()
}

/// The kind-1081 envelope stream of *every* roster channel at once, authored by
/// any of `team_pubkeys`. One local subscription over this drives the outbound
/// fan-out of freshly-authored sealed envelopes across all channels (see
/// [`HostPrivateSync::fan_out_local_envelopes`]); a `limit` is inert for a
/// subscription, so it is left unbounded.
fn team_envelopes_filter(team_pubkeys: &[Pubkey]) -> Filter {
    Filter::new()
        .kinds([nostrdb_net::sns::SNS_ENVELOPE_KIND as u64])
        .authors(team_pubkeys.iter().map(|k| k.bytes()))
        .build()
}

/// The team keypair pubkey that seals (and thus authors) `root`'s kind-1081
/// envelopes — the channel to subscribe to. `None` if the root is unusable.
fn team_pubkey(root: &[u8; 32]) -> Option<Pubkey> {
    Some(nostrdb_net::sns::derive_sns_keys(root)?.team_keypair.pubkey)
}

/// Every SNS `team_root` `author` has been key-shared, reconstructed from nostrdb.
///
/// Reads the unwrapped kind-1082 rumors (see [`keyshare_filter`]), keeps the ones
/// gift-wrapped to `author` (nostrdb records the recipient on the rumor), and
/// returns each share's raw 32-byte `team_root`, de-duplicated. Membership thus
/// survives restarts and rides the NIP-59 inbox across devices with no config file.
///
/// Unlike headway's board-bearing roster this keeps shares that name no board: for
/// sync we only need the root to register + subscribe, and registering an extra
/// root is harmless. App-level board semantics filter the roster themselves.
fn registered_roots(ndb: &Ndb, author: &Pubkey) -> Vec<[u8; 32]> {
    let Ok(txn) = Transaction::new(ndb) else {
        return Vec::new();
    };
    let Ok(results) = ndb.query(&txn, &[keyshare_filter()], 500) else {
        return Vec::new();
    };
    let mut roots: Vec<[u8; 32]> = Vec::new();
    for res in results {
        // A `1082` unwrapped for another local account isn't ours.
        if res.note.rumor_receiver_pubkey() != Some(author.bytes()) {
            continue;
        }
        let Some(share) = nostrdb_net::sns::parse_keyshare(&res.note) else {
            continue;
        };
        if !roots.contains(&share.team_root) {
            roots.push(share.team_root);
        }
    }
    roots
}

/// Map kind-1082 key-share rumors to the **outer kind-1059 gift-wraps** that carry
/// them, keeping only the *self-shares this account minted* — a share `account`
/// authored and addressed to `account` itself, i.e. the key to one of our own
/// team-of-one boards.
///
/// This is the key half of the account's private surface: a board's *content*
/// rides its kind-1081 envelopes, but another device can only **join** the channel
/// from the kind-1059 carrying its key-share. Every other sync leg here is
/// author-keyed, and a gift-wrap's author is a throwaway ephemeral key by NIP-59
/// design, so no author-keyed filter can ever select one — which is why a board
/// sealed on one device used to be invisible on every other.
///
/// The scoping is deliberate. The only wire-visible handle on a gift-wrap is its
/// `p` tag, but fanning *everything* `#p`-tagged to us would make notedeck a
/// write-amplifier into our own private relay, and the seen-on check would not stop
/// it: a wrap pulled in from a *public* accounts-read relay has seen-on = that
/// public relay, so it looks unsent to the private one and gets forwarded. Anyone
/// able to write a 1059 at any relay we read would then have unbounded write access
/// to our private relay. So we scope on the **peeled rumor** instead — the one
/// thing the ephemeral outer key cannot hide from us locally. A stranger's wrap
/// peels to a rumor *they* authored and is dropped here, leaving no spam surface;
/// a co-member's genuine invite is likewise not ours to republish.
///
/// The returned keys name the stored 1059s, which are forwarded **verbatim** by
/// [`fan_keys_to_relays`]. Forwarding, not re-wrapping: a re-wrap mints a fresh
/// ephemeral key and a new event id every run, so it is not idempotent, whereas
/// forwarding the stored wrap is (and the seen-on check then actually works).
fn own_selfshare_giftwraps(
    ndb: &Ndb,
    txn: &Transaction,
    account: &Pubkey,
    rumor_keys: impl IntoIterator<Item = NoteKey>,
) -> Vec<NoteKey> {
    let mut wraps: Vec<NoteKey> = Vec::new();
    for key in rumor_keys {
        let Ok(rumor) = ndb.get_note_by_key(txn, key) else {
            continue;
        };
        // Minted by us (the rumor is signed by the sharer, so its author is real
        // even though the wrap's author is not) *and* addressed to us.
        if rumor.pubkey() != account.bytes()
            || rumor.rumor_receiver_pubkey() != Some(account.bytes())
        {
            continue;
        }
        let Some(wrap_id) = rumor.rumor_giftwrap_id() else {
            continue;
        };
        let Some(wrap_key) = ndb
            .get_note_by_id(txn, wrap_id)
            .ok()
            .and_then(|wrap| wrap.key())
        else {
            continue;
        };
        if !wraps.contains(&wrap_key) {
            wraps.push(wrap_key);
        }
    }
    wraps
}

/// The SNS team roots this session has already handed to nostrdb, so each one is
/// registered exactly once.
///
/// Registering the same root twice is **not** free. nostrdb keeps its registered
/// roots in a fixed-size per-ingester-thread array (`MAX_INGESTER_KEYS`, 128) and
/// `ndb_add_team_root` appends unconditionally — it does not dedup. Since
/// [`HostPrivateSync::update`] re-registers the *whole* root set every time the set
/// changes, a long-running session with a couple of dozen boards fills that array
/// with duplicates within a few roster changes. Once it is full, every genuinely
/// new root is silently dropped: `ndb_add_team_root` reports whether the *dispatch*
/// succeeded, not whether the key was accepted, so nothing here can see it happen.
/// The board is then listed from its key-share but its kind-1081 envelopes are
/// never peeled, and its shared fold stays empty until the app is restarted (the
/// registration is process-lifetime, so this set matches it exactly).
#[derive(Default)]
struct RegisteredRoots(HashSet<[u8; 32]>);

impl RegisteredRoots {
    /// Register every not-yet-registered `root` with nostrdb so it auto-unwraps
    /// that channel's kind-1081 envelopes, then run one [`Ndb::process_sns`]
    /// catch-up peel for envelopes that were ingested before the root was
    /// registered. Idempotent — call on boot and after every account switch
    /// (mirrors `add_key`). The catch-up walk is only paid when a root was actually
    /// new, which is now what the flag means: with the duplicate calls gone, the
    /// walk runs on a real join rather than on every root-set change.
    fn register(&mut self, ndb: &Ndb, roots: &[[u8; 32]]) {
        let mut registered = false;
        for root in roots {
            if !self.0.insert(*root) {
                continue;
            }
            registered |= ndb.add_team_root(root);
        }
        if !registered {
            return;
        }
        let Ok(txn) = Transaction::new(ndb) else {
            return;
        };
        ndb.process_sns(&txn);
    }
}

/// The inputs the host's private [`Session`] subscription was last declared for.
/// Re-declaration is deduped on this value: only a new account, a changed private
/// relay set, or a grown/shrunk SNS roster warrants tearing down and reopening the
/// `REQ`. The roster roots are held sorted so equality is order-independent.
#[derive(PartialEq, Eq)]
struct DeclaredSync {
    account: Pubkey,
    relays: Vec<NormRelayUrl>,
    roots: Vec<[u8; 32]>,
}

/// The SNS team roots apps have asked the host to sync this session — the seam
/// [`AppContext::register_team_root`](crate::AppContext::register_team_root)
/// writes into and [`HostPrivateSync`] reads from its off-foreground pump.
///
/// It exists because some SNS channels have no kind-1082 key-share to discover
/// them from: the notebook's vault is a *derived* team-of-one channel
/// (`derive_board_root(secret, "notebook")`), so an app that owns such a channel
/// hands the host its root directly instead of via the roster. Roots accumulate
/// (registering an extra one is harmless — see [`registered_roots`]); an app
/// re-registers its root each frame and the set is simply deduped.
///
/// Held by [`Notedeck`](crate::Notedeck) and handed to each frame's `AppContext`
/// as a `&mut` field (like [`AppActionQueue`](crate::AppActionQueue)): the app
/// registers through it during `update`/`render`, and the host reads the
/// accumulated set from its pump — a different, non-overlapping borrow — so a
/// plain owned set suffices with no interior mutability.
#[derive(Default)]
pub struct PrivateChannels {
    roots: HashSet<[u8; 32]>,
}

impl PrivateChannels {
    /// Register an SNS `team_root` for the host to register with nostrdb, sync
    /// inbound, and fan outbound. Idempotent; call each frame the channel is live.
    pub fn register_team_root(&mut self, root: [u8; 32]) {
        self.roots.insert(root);
    }

    /// A snapshot of the registered roots, for the host's pump to union into its
    /// roster.
    pub fn roots(&self) -> Vec<[u8; 32]> {
        self.roots.iter().copied().collect()
    }
}

/// The host's account-wide private-note sync, run from [`Notedeck`](crate::Notedeck)
/// independent of whichever app is foregrounded.
///
/// The host owns a long-lived [`Session`] over its own [`RelayPool`] — a small,
/// dedicated pool for the account's 1–2 private-sync relays, separate from the
/// app read/write outbox — and feeds it one filter set covering the account's
/// whole private surface:
///
/// - the account's kind-1080 PNS envelope stream ([`pns_envelope_filter`]);
/// - one kind-1081 envelope stream per SNS channel in the account's roster
///   ([`team_envelope_filter`]), so a co-member's sealed shared-board edits land
///   off-foreground. The roster is the union of the channels key-shared to the
///   account (from nostrdb — see [`registered_roots`]) and the roots apps
///   registered this session ([`PrivateChannels`]), e.g. the notebook's derived
///   team-of-one vault, which has no key-share of its own;
/// - the account's kind-1082 key-share stream ([`keyshare_filter`]), so a new
///   join accepted on *another* of the account's devices arrives here and grows
///   the roster (the closed loop: relay → 1082 in ndb → roster poll → re-derive →
///   new 1081 filter).
///
/// That covers *both* directions for the account's private notes at once:
///
/// - **inbound** — a live `REQ` plus a NIP-77 negentropy backfill pull the
///   account's envelopes into the local nostrdb, where nostrdb auto-unwraps them
///   (PNS via the seeded account key, SNS once the root is registered); apps then
///   read the inner notes with plain local queries.
/// - **outbound** — local subscriptions over the account's PNS 1080 stream *and*
///   every roster channel's SNS 1081 stream drive an
///   [`is_rumor`](nostrdb::Note::is_rumor)-guarded fan-out of freshly-authored
///   envelopes (e.g. a notebook longform, or a headway shared-board edit made on
///   this device) out to the private relays via [`Session::publish`]. The 1082
///   key-share stream drives a third outbound leg: the kind-1059 gift-wrap of each
///   self-share *we* minted is forwarded too ([`own_selfshare_giftwraps`]), because
///   a channel's 1081 envelopes carry its content but only the gift-wrap carries
///   the key another device needs to **join** it. The host owns the whole private
///   wire — every kind, both directions — so an SNS app never publishes its own
///   sealed envelopes or key-shares.
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
    /// Local subscription over the account's kind-1082 key-share stream, re-created
    /// when the selected account changes. Polled each frame for two jobs: as a cheap
    /// roster change-detector (a fresh 1082 means a channel was joined on this or
    /// another device, so re-derive the SNS roster from ndb rather than querying it
    /// every frame), and as the scope for the outbound gift-wrap leg — a fresh 1082
    /// of our own naming a kind-1059 to forward ([`own_selfshare_giftwraps`]).
    roster_sub: Option<Subscription>,
    /// Local subscription over *every* roster channel's kind-1081 envelope stream
    /// (all team pubkeys at once), re-created when the roster changes. Polled each
    /// frame to drive the outbound fan-out of freshly-authored SNS envelopes — the
    /// 1081 twin of [`local_sub`](Self::local_sub)'s 1080 PNS fan-out, so an app
    /// (notebook, headway) never fans its own sealed envelopes.
    local_sns_sub: Option<Subscription>,
    /// The SNS roster derived from the account's kind-1082 key-shares
    /// ([`registered_roots`]), cached between the roster-change signals so the ndb
    /// walk is only paid when the key-share sub reports a join or the account
    /// switches. The app-registered roots ([`update`](Self::update)'s `app_roots`)
    /// are unioned onto this each frame; the union is what gets registered,
    /// declared inbound, and fanned outbound.
    roster_roots: Vec<[u8; 32]>,
    /// What the remote subscription was last declared for, so we only re-declare
    /// (and re-arm the settle watcher) on a real change — a new account, a changed
    /// private relay set, or a grown/shrunk roster — rather than every frame.
    declared: Option<DeclaredSync>,
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
    /// The roots already registered with nostrdb this session, so the re-registration
    /// on each roster change doesn't fill nostrdb's fixed root table with duplicates
    /// and start dropping new boards ([`RegisteredRoots`]).
    registered: RegisteredRoots,
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
            roster_sub: None,
            local_sns_sub: None,
            roster_roots: Vec::new(),
            declared: None,
            // Nothing declared yet ⇒ nothing to reconcile ⇒ settled.
            settled: Arc::new(AtomicBool::new(true)),
            settle_gen: Arc::new(AtomicU64::new(0)),
            registered: RegisteredRoots::default(),
        }
    }

    /// Whether the current private declaration's history backfill has settled.
    ///
    /// `true` when local-only (no private relay) or once the backfill has
    /// reconciled; `false` in the window between (re)declaring the subscription
    /// and its settle. Apps gate work that must not act on a mid-sync view (e.g.
    /// dave's deleted-session litter avoidance) on this via
    /// [`AppContext::private_sync_settled`](crate::AppContext).
    pub fn settled(&self) -> bool {
        self.settled.load(Ordering::Acquire)
    }

    /// Bring the host sync in line with the selected account: (re)declare the
    /// private subscription over `private_urls` — the account's PNS 1080 stream, its
    /// joined SNS channels' 1081 streams, and its 1082 key-share stream — and fan
    /// any freshly-authored local PNS/SNS envelopes, and the gift-wraps of our own
    /// key-shares, out to them. Cheap to call every frame — the remote declaration
    /// is deduped on `(account, urls, roster)` and only the small fan-out +
    /// roster-change polls run otherwise.
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
        app_roots: &[[u8; 32]],
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

        // Re-create the account-keyed local subs when the account changes: the PNS
        // sub's filter is keyed on the account-derived PNS author (a switch renames
        // the stream), and the roster change-detector must observe only the new
        // account's incoming key-shares.
        let account_changed = self.declared.as_ref().map(|d| &d.account) != Some(account);
        if account_changed {
            if let Some(old) = self.local_sub.take() {
                let _ = ndb.unsubscribe(old);
            }
            self.local_sub = ndb.subscribe(&[pns_envelope_filter(&pns_pubkey)]).ok();
            if let Some(old) = self.roster_sub.take() {
                let _ = ndb.unsubscribe(old);
            }
            self.roster_sub = ndb.subscribe(&[keyshare_filter()]).ok();
        }

        // Refresh the base SNS roster (the account's kind-1082 key-shares). Walking
        // ndb for it is only paid when the account changed or the key-share sub
        // reports a fresh 1082 (a channel joined here or on another device);
        // otherwise the cached roster is reused.
        // The polled keys are kept, not just counted: a fresh 1082 of our own also
        // means a gift-wrap to fan outbound (see [`own_selfshare_giftwraps`]).
        let keyshare_keys = self
            .roster_sub
            .map(|sub| ndb.poll_for_notes(sub, 64))
            .unwrap_or_default();
        let roster_dirty = account_changed || !keyshare_keys.is_empty();
        if roster_dirty {
            self.roster_roots = registered_roots(ndb, account);
        }

        // The channels to sync are the key-shared roster *plus* the roots apps
        // registered this session ([`AppContext::register_team_root`]) — e.g. the
        // notebook's derived team-of-one vault, which has no key-share of its own.
        // Registering an extra root is harmless, so the union is just deduped, not
        // account-scoped.
        let mut roots = self.roster_roots.clone();
        for root in app_roots {
            if !roots.contains(root) {
                roots.push(*root);
            }
        }
        roots.sort_unstable();

        // On any change to the effective root set, register the roots (so nostrdb
        // auto-unwraps their 1081 envelopes even off-foreground) and re-open the
        // local 1081 sub that drives the outbound fan over *all* channels at once.
        let roots_changed = self.declared.as_ref().map(|d| d.roots.as_slice()) != Some(&roots);
        if roots_changed {
            self.registered.register(ndb, &roots);
            if let Some(old) = self.local_sns_sub.take() {
                let _ = ndb.unsubscribe(old);
            }
            let team_pubkeys: Vec<Pubkey> = roots.iter().filter_map(team_pubkey).collect();
            self.local_sns_sub = (!team_pubkeys.is_empty())
                .then(|| ndb.subscribe(&[team_envelopes_filter(&team_pubkeys)]).ok())
                .flatten();
        }

        // (Re)declare the remote subscription on any account / relay-set / roster
        // change. Folding the sorted roster into the dedup key makes a joined
        // channel re-declare (adding its 1081 filter) even when account + relays held.
        let next = DeclaredSync {
            account: *account,
            relays: private_urls.to_vec(),
            roots,
        };
        if self.declared.as_ref() != Some(&next) {
            self.redeclare(&session, &pns_pubkey, private_urls, &next.roots);
            // Catch up the outbound envelope leg. Keyed off the declaration change
            // rather than the root-set change above for the same reason as the
            // gift-wrap leg below: a board sealed before any private relay was
            // marked changes the roots while `private_urls` is still empty, and a
            // catchup run then has nowhere to publish. Gated on the roots alone it
            // would never run again — the roots do not change a second time — and
            // the channel's envelopes would stay on this device forever.
            let team_pubkeys: Vec<Pubkey> = next.roots.iter().filter_map(team_pubkey).collect();
            self.fan_out_channel_catchup(ndb, &session, private_urls, &team_pubkeys);
            // Catch up the outbound gift-wrap leg on the same (rare) trigger. The
            // live poll below only reports 1082s committed *after* the sub opened,
            // so every board sealed before this boot — the whole existing roster,
            // and anything the CLI sealed while the app was closed — would never
            // have its key fanned. Keyed off the declaration change so it also runs
            // the moment the private relay set first resolves (the account-change
            // pump usually sees no relays yet), and re-runs cost only the query
            // thanks to the seen-on check.
            self.fan_out_selfshare_catchup(ndb, &session, private_urls, account);
            self.declared = Some(next);
        }

        self.fan_out_local_envelopes(ndb, &session, private_urls, account, &keyshare_keys);
    }

    /// Replace the private declaration: close the prior `REQ` on every relay and,
    /// when the account still has private relays, open a fresh live + backfilling
    /// subscription over the current set, arming a generation-guarded settle
    /// watcher. With no private relays this is the teardown to local-only.
    ///
    /// The filter set is the account's whole private surface: the PNS 1080 stream,
    /// one 1081 envelope stream per registered SNS `root`, and the 1082 key-share
    /// stream. They ride one logical sub per relay under [`HOST_PRIVATE_SUB_ID`], so
    /// the single drop above closes all of them.
    fn redeclare(
        &mut self,
        session: &Arc<Session>,
        pns_pubkey: &Pubkey,
        urls: &[NormRelayUrl],
        roots: &[[u8; 32]],
    ) {
        session.drop_subscription(HOST_PRIVATE_SUB_ID);
        if urls.is_empty() {
            // Local-only: nothing to reconcile, so the view is trivially settled.
            self.settled.store(true, Ordering::Release);
            return;
        }

        // One logical subscription (same id + filter set) fanned across the relay
        // set; the same filters drive the live `REQ` and the history backfill.
        let mut filters = vec![pns_envelope_filter(pns_pubkey)];
        for root in roots {
            if let Some(team_pk) = team_pubkey(root) {
                filters.push(team_envelope_filter(&team_pk));
            }
        }
        // The 1082 key-share stream: a join accepted on another of the account's
        // devices arrives here and (via the roster poll) grows the roster.
        filters.push(keyshare_filter());
        for url in urls {
            session.set_subscription(
                HOST_PRIVATE_SUB_ID,
                url.to_string(),
                filters.clone(),
                filters.clone(),
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

    /// Poll the local envelope subscriptions — the account's kind-1080 PNS stream
    /// *and* every roster channel's kind-1081 SNS stream — and fan freshly-authored
    /// envelopes out to the private relays they have not been seen on yet, via
    /// [`Session::publish`]. `keyshare_keys` are the kind-1082 rumors this frame's
    /// roster poll reported, whose own gift-wraps are fanned alongside them.
    ///
    /// The seen-on check ([`fan_out_unseen_notes_with`]) keeps an envelope pulled
    /// *in* by the inbound leg from being echoed straight back out, and the
    /// `is_rumor` guard keeps a sealed rumor from ever leaking in the clear. Even
    /// with no private relay we still drain the polls so a later-marked relay does
    /// not receive an unbounded backlog dump in one frame. Fanning all three streams
    /// here is what lets an SNS app (notebook, headway) never publish its own
    /// sealed envelopes or key-shares — the host owns the whole private wire, every
    /// kind, both directions.
    fn fan_out_local_envelopes(
        &self,
        ndb: &Ndb,
        session: &Session,
        urls: &[NormRelayUrl],
        account: &Pubkey,
        keyshare_keys: &[NoteKey],
    ) {
        let mut keys = self
            .local_sub
            .map(|sub| ndb.poll_for_notes(sub, 64))
            .unwrap_or_default();
        if let Some(sns_sub) = self.local_sns_sub {
            keys.extend(ndb.poll_for_notes(sns_sub, 64));
        }
        if (keys.is_empty() && keyshare_keys.is_empty()) || urls.is_empty() {
            return;
        }
        let Ok(txn) = Transaction::new(ndb) else {
            return;
        };
        // A fresh key-share rumor of our own means a board was just sealed here (a
        // GUI create) or arrived from our CLI through the embedded relay: fan the
        // outer 1059 that carries it, so the *key* to the board reaches our other
        // devices and not just its content.
        keys.extend(own_selfshare_giftwraps(
            ndb,
            &txn,
            account,
            keyshare_keys.iter().copied(),
        ));
        if keys.is_empty() {
            return;
        }
        fan_keys_to_relays(session, ndb, &txn, &keys, urls);
    }

    /// Fan the outer kind-1059 gift-wrap of every self-share already in ndb that the
    /// private relays haven't seen. The gift-wrap twin of
    /// [`fan_out_channel_catchup`](Self::fan_out_channel_catchup): the live
    /// [`roster_sub`](Self::roster_sub) only reports 1082s committed *after* it was
    /// opened, so a board sealed on a previous run — or by the `headway`/`notebook`
    /// CLI while the app was closed, which lands the wrap in ndb via the embedded
    /// relay — predates it and would never have its key fanned. Without this, only
    /// boards created while the app happened to be running with a private relay
    /// marked would ever become joinable elsewhere.
    ///
    /// Run only on a declaration change (account / relay set / roster), and the
    /// seen-on check ([`fan_out_unseen_notes_with`]) skips wraps the relays already
    /// hold, so a re-run costs only the query.
    fn fan_out_selfshare_catchup(
        &self,
        ndb: &Ndb,
        session: &Session,
        urls: &[NormRelayUrl],
        account: &Pubkey,
    ) {
        if urls.is_empty() {
            return;
        }
        let Ok(txn) = Transaction::new(ndb) else {
            return;
        };
        let Ok(results) = ndb.query(&txn, &[keyshare_filter()], 500) else {
            return;
        };
        let keys =
            own_selfshare_giftwraps(ndb, &txn, account, results.iter().map(|res| res.note_key));
        if keys.is_empty() {
            return;
        }
        fan_keys_to_relays(session, ndb, &txn, &keys, urls);
    }

    /// Fan every kind-1081 envelope currently in ndb for `team_pubkeys`' channels
    /// that the private relays haven't been seen on yet. Complements the live
    /// [`local_sns_sub`](Self::local_sns_sub), which only reports envelopes
    /// committed *after* it was opened: an envelope sealed and locally-ingested
    /// before its root was registered (a board definition, the notebook's first
    /// canvas) predates the sub and would never be fanned otherwise. Run only on a
    /// declaration change (account / relay set / roster), and the seen-on check
    /// ([`fan_out_unseen_notes_with`]) skips envelopes the relays already hold, so a
    /// co-member's inbound edits are not echoed back and a re-run costs only the
    /// query.
    fn fan_out_channel_catchup(
        &self,
        ndb: &Ndb,
        session: &Session,
        urls: &[NormRelayUrl],
        team_pubkeys: &[Pubkey],
    ) {
        if urls.is_empty() || team_pubkeys.is_empty() {
            return;
        }
        let Ok(txn) = Transaction::new(ndb) else {
            return;
        };
        let Ok(results) = ndb.query(&txn, &[team_envelopes_filter(team_pubkeys)], 5000) else {
            return;
        };
        let keys: Vec<NoteKey> = results.iter().map(|r| r.note_key).collect();
        if keys.is_empty() {
            return;
        }
        fan_keys_to_relays(session, ndb, &txn, &keys, urls);
    }
}

/// Publish each note in `keys` the private relays haven't seen to the ones missing
/// it, over the host [`Session`]. The shared tail of both host fan paths (the live
/// [`HostPrivateSync::fan_out_local_envelopes`] poll and the roster-change
/// [`HostPrivateSync::fan_out_channel_catchup`]); only the key source differs.
fn fan_keys_to_relays(
    session: &Session,
    ndb: &Ndb,
    txn: &Transaction,
    keys: &[NoteKey],
    urls: &[NormRelayUrl],
) {
    let relays: Vec<RelayId> = urls.iter().cloned().map(RelayId::Websocket).collect();
    fan_out_unseen_notes_with(ndb, txn, keys, &relays, |json, targets| {
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

        let config = SubConfig::builder(filters.clone())
            .explicit(
                urls.into_iter().collect::<HashSet<_>>(),
                SubRelayPolicy::accounts_read_important(),
            )
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
        let statuses: Vec<String> = urls
            .iter()
            .map(|url| {
                let status = inspect
                    .relay_infos()
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
    use crate::test_util::test_config;
    use crate::{
        remote_data::{RemoteIntent, RemoteIntentBatchBuilder, RemotePublishCommand},
        ExplicitPublishApi,
    };
    use enostr::NormRelayUrl;
    use nostrdb::{Config, IngestMetadata, NoteBuilder};
    use nostrdb_net::FullKeypair;
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

    /// Drive `fan_out_event_frame` and return the explicit publish relay set.
    fn explicit_publish_relays_for(frame: &str, relays: Vec<RelayId>) -> Option<Vec<RelayId>> {
        let mut batch = RemoteIntentBatchBuilder::new();
        {
            let mut api = ExplicitPublishApi::new(&mut batch);
            fan_out_event_frame(&mut api, frame, &relays);
        }

        let batch = batch.take()?;
        let mut publish_relays = None;
        for section in batch.sections() {
            for intent in section.intents() {
                let RemoteIntent::Publish(RemotePublishCommand::Explicit { relays, .. }) = intent
                else {
                    panic!("unexpected private-sync intent");
                };
                assert!(
                    publish_relays.is_none(),
                    "fan_out_event_frame should emit one explicit publish"
                );
                publish_relays = Some(relays.clone());
            }
        }
        publish_relays
    }

    /// A well-formed frame is unwrapped and published to each target relay.
    #[tokio::test]
    async fn fan_out_publishes_inner_event_to_targets() {
        let relay = NormRelayUrl::new("wss://private.example.com").expect("relay");
        let publish_relays =
            explicit_publish_relays_for(&event_frame(), vec![RelayId::Websocket(relay.clone())]);
        assert_eq!(publish_relays, Some(vec![RelayId::Websocket(relay)]));
    }

    /// An empty relay set is a no-op — no relay connection is opened.
    #[test]
    fn fan_out_empty_relays_is_noop() {
        assert!(explicit_publish_relays_for(&event_frame(), vec![]).is_none());
    }

    /// A malformed frame (no inner event object) opens no relay; the local ingest
    /// has already happened, so there's nothing to forward.
    #[test]
    fn fan_out_malformed_frame_is_noop() {
        let relay = RelayId::Websocket(NormRelayUrl::new("wss://private.example.com").expect("r"));
        assert!(explicit_publish_relays_for("not json", vec![relay.clone()]).is_none());
        assert!(explicit_publish_relays_for("[\"EVENT\"]", vec![relay]).is_none());
    }

    // ===== fan_out_unseen_notes =====

    /// A temporary nostrdb for the seen-on fan-out tests.
    fn test_ndb() -> (TempDir, Ndb) {
        let tmp = TempDir::new().expect("tmp dir");
        let ndb = Ndb::new(tmp.path().to_str().expect("path"), &test_config()).expect("ndb");
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

    /// Drive `fan_out_unseen_notes` over `keys` and return the explicit relay
    /// targets staged through the production remote-intent boundary.
    fn relays_fanned_for(
        ndb: &Ndb,
        keys: &[NoteKey],
        relays: Vec<RelayId>,
    ) -> HashSet<NormRelayUrl> {
        let mut batch = RemoteIntentBatchBuilder::new();
        {
            let mut api = ExplicitPublishApi::new(&mut batch);
            let txn = Transaction::new(ndb).expect("txn");
            fan_out_unseen_notes(&mut api, ndb, &txn, keys, &relays);
        }

        let mut published = HashSet::new();
        let Some(batch) = batch.take() else {
            return published;
        };
        for section in batch.sections() {
            for intent in section.intents() {
                let RemoteIntent::Publish(RemotePublishCommand::Explicit { relays, .. }) = intent
                else {
                    panic!("unexpected private-sync intent");
                };
                published.extend(relays.iter().filter_map(|relay| match relay {
                    RelayId::Websocket(relay) => Some(relay.clone()),
                    RelayId::Multicast => None,
                }));
            }
        }
        published
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
        let keys = nostrdb_net::sns::derive_sns_keys(&root).expect("keys");
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
            nostrdb_net::sns::wrap_rumor(&keys, &member, &rumor, 1_700_000_000).expect("envelope");

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
        let pns_keys = nostrdb_net::pns::derive_pns_keys(secret);
        let inner = NoteBuilder::new()
            .kind(1)
            .content("private longform body")
            .created_at(HOST_TEST_TS)
            .sign(secret)
            .build()
            .expect("inner note");
        let inner_id = *inner.id();
        let envelope =
            nostrdb_net::pns::wrap(&pns_keys, &inner.json().expect("inner json"), HOST_TEST_TS)
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
        host_a.update(&mut ndb_a, &account.pubkey, &secret, relays, &[]);
        // A 2-element client frame (`["EVENT",{…}]`) is a locally-authored event,
        // so ingest it as one — `process_event` expects the 3-element relay form.
        ndb_a
            .process_client_event(&frame)
            .expect("ingest envelope on A");

        // Pump A until the relay has stored the fanned-out envelope.
        let mut fanned = false;
        for _ in 0..500 {
            host_a.update(&mut ndb_a, &account.pubkey, &secret, relays, &[]);
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
            host_b.update(&mut ndb_b, &account.pubkey, &secret, relays, &[]);
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
        host.update(&mut ndb, &account.pubkey, &secret, &[], &[]);
        assert!(
            host.settled(),
            "no private relay ⇒ nothing to reconcile ⇒ still settled"
        );
    }

    // ===== SNS roster =====

    /// A distinct 32-byte team root per `seed`.
    fn test_root(seed: u8) -> [u8; 32] {
        let mut root = [0u8; 32];
        root[0] = 0x11;
        root[31] = seed;
        root
    }

    /// NIP-59 seal + gift-wrap a kind-1082 key-share to `recipient` and return the
    /// kind-1059 giftwrap JSON — what a sharer publishes and nostrdb unwraps back
    /// into a queryable `1082` rumor. `board_addr = None` builds a (board-less)
    /// share that headway would drop but the sync roster keeps.
    fn gift_wrapped_keyshare(
        sender: &FullKeypair,
        recipient: &Pubkey,
        root: &[u8; 32],
        board_addr: Option<&str>,
    ) -> String {
        nostrdb_net::sns::wrap_keyshare(
            sender,
            recipient,
            root,
            board_addr.unwrap_or(""),
            None,
            HOST_TEST_TS,
        )
        .expect("keyshare giftwrap")
        .json()
        .expect("giftwrap json")
    }

    /// Ingest a kind-1059 giftwrap as if it arrived from a relay; nostrdb unwraps it
    /// into the durable kind-1082 rumor when the recipient key is seeded.
    fn ingest_giftwrap(ndb: &Ndb, giftwrap_json: &str) {
        ndb.process_event(&format!("[\"EVENT\",\"_gw\",{giftwrap_json}]"))
            .expect("ingest giftwrap");
    }

    /// Poll the (async-ingested) roster until it has at least `n` roots, or time out.
    async fn wait_roots(ndb: &Ndb, author: &Pubkey, n: usize) -> Vec<[u8; 32]> {
        for _ in 0..250 {
            let roots = registered_roots(ndb, author);
            if roots.len() >= n {
                return roots;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("roster never reached {n} root(s)");
    }

    /// The roster reads a share gift-wrapped to the account (and never one wrapped
    /// to a different account), including a board-less share the sync path keeps.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn roster_reads_shares_for_recipient_only() {
        let (_dir, ndb) = test_ndb();
        let author = FullKeypair::generate();
        let other = FullKeypair::generate();
        let sender = FullKeypair::generate();
        ndb.add_key(&author.secret_key.secret_bytes());

        let with_board = test_root(0x22);
        let board_less = test_root(0x33);
        ingest_giftwrap(
            &ndb,
            &gift_wrapped_keyshare(&sender, &author.pubkey, &with_board, Some("30619:owner:x")),
        );
        // A board-less share headway drops — the sync roster still registers it.
        ingest_giftwrap(
            &ndb,
            &gift_wrapped_keyshare(&sender, &author.pubkey, &board_less, None),
        );

        let roots = wait_roots(&ndb, &author.pubkey, 2).await;
        assert!(roots.contains(&with_board));
        assert!(roots.contains(&board_less));
        // Wrapped to `author`; never counts as `other`'s.
        assert!(registered_roots(&ndb, &other.pubkey).is_empty());
    }

    /// End-to-end host sync of a *sealed shared-board* (SNS) edit over a shared
    /// private relay: device A's headway authors a kind-1081 envelope and publishes
    /// it (modelled by seeding the relay directly, since the host's outbound leg is
    /// PNS-only); device B's host derives the channel from its key-share roster,
    /// backfills the envelope, and nostrdb auto-unwraps the inner rumor — all with
    /// headway backgrounded (we just pump `update`, no app). This is the inbound SNS
    /// leg the host takes over from headway in Stage 3.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn host_sync_delivers_sns_envelope_between_devices() {
        use nostrdb_net::relay::server;
        use std::time::Duration;

        // The shared board root and its channel keypair (the 1081 author).
        let root = test_root(0x55);
        let sns_keys = nostrdb_net::sns::derive_sns_keys(&root).expect("sns keys");

        // A shared private relay over its own opaque db — never seeded with the root,
        // so it only ever holds the opaque 1081 envelope, never the inner rumor.
        let (_relay_dir, relay_ndb) = test_ndb();
        let relay = server::spawn(relay_ndb.clone(), "127.0.0.1:0".parse().expect("addr"))
            .expect("spawn relay");
        let url = NormRelayUrl::new(&relay.url()).expect("relay url");
        let relays = std::slice::from_ref(&url);

        // Device A seals a board edit and its envelope reaches the relay. This test
        // isolates the inbound leg, so we model A's publish by ingesting the
        // envelope straight into the relay's db (the host's own 1081 fan-out is
        // exercised separately in `host_fans_app_registered_channel_between_devices`).
        let author = FullKeypair::generate();
        let secret = author.secret_key.secret_bytes();
        let member = FullKeypair::generate();
        let rumor = NoteBuilder::new()
            .kind(1)
            .content("sealed shared-board edit")
            .created_at(HOST_TEST_TS)
            .sign(&member.secret_key.secret_bytes())
            .build()
            .expect("rumor");
        let inner_id = *rumor.id();
        let envelope = nostrdb_net::sns::wrap_rumor(
            &sns_keys,
            &member,
            &rumor.json().expect("rumor json"),
            HOST_TEST_TS,
        )
        .expect("sns envelope");
        relay_ndb
            .process_event(&format!(
                "[\"EVENT\",\"a\",{}]",
                envelope.json().expect("envelope json")
            ))
            .expect("seed relay with envelope");

        // Device B: seed the account key and a key-share for the root (as its NIP-59
        // inbox would carry), so the host derives the channel from the roster,
        // registers the root, backfills the 1081, and auto-unwraps the inner rumor.
        let (_b_dir, mut ndb_b) = test_ndb();
        ndb_b.add_key(&secret);
        let sharer = FullKeypair::generate();
        ingest_giftwrap(
            &ndb_b,
            &gift_wrapped_keyshare(&sharer, &author.pubkey, &root, Some("30619:owner:board")),
        );

        let mut host_b = HostPrivateSync::new();
        let mut delivered = false;
        for _ in 0..500 {
            host_b.update(&mut ndb_b, &author.pubkey, &secret, relays, &[]);
            if ndb_has(&ndb_b, &inner_id) {
                delivered = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            delivered,
            "device B's host should backfill the 1081 envelope and auto-unwrap it"
        );

        relay.shutdown();
    }

    /// The app-registered-root path end to end: a channel with **no key-share**
    /// (a derived team-of-one vault, registered via `app_roots` — the seam
    /// [`AppContext::register_team_root`] feeds), synced both ways by the host.
    /// Device A registers the root, seals an edit, and *its host* fans the 1081
    /// envelope out (no app publishes it); device B registers the same derived root
    /// and backfills + auto-unwraps it. Proves both new host halves at once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn host_fans_app_registered_channel_between_devices() {
        use nostrdb_net::relay::server;
        use std::time::Duration;

        // A derived-style team-of-one root apps register directly — never key-shared.
        let root = test_root(0x66);
        let sns_keys = nostrdb_net::sns::derive_sns_keys(&root).expect("sns keys");
        let app_roots = std::slice::from_ref(&root);

        let (_relay_dir, relay_ndb) = test_ndb();
        let relay = server::spawn(relay_ndb.clone(), "127.0.0.1:0".parse().expect("addr"))
            .expect("spawn relay");
        let url = NormRelayUrl::new(&relay.url()).expect("relay url");
        let relays = std::slice::from_ref(&url);

        let account = FullKeypair::generate();
        let secret = account.secret_key.secret_bytes();

        // Device A: register the root (opens the local 1081 sub), then locally
        // ingest a sealed envelope exactly as the notebook store's `ingest_sealed`
        // does — the host, not the app, fans it out on the next pump.
        let (_a_dir, mut ndb_a) = test_ndb();
        ndb_a.add_key(&secret);
        let mut host_a = HostPrivateSync::new();
        host_a.update(&mut ndb_a, &account.pubkey, &secret, relays, app_roots);

        let rumor = NoteBuilder::new()
            .kind(1)
            .content("app-registered channel edit")
            .created_at(HOST_TEST_TS)
            .sign(&secret)
            .build()
            .expect("rumor");
        let inner_id = *rumor.id();
        let envelope = nostrdb_net::sns::wrap_rumor(
            &sns_keys,
            &account,
            &rumor.json().expect("json"),
            HOST_TEST_TS,
        )
        .expect("sns envelope");
        ndb_a
            .process_event(&format!(
                "[\"EVENT\",\"_local\",{}]",
                envelope.json().expect("envelope json")
            ))
            .expect("local envelope ingest");

        // Device B: register the SAME derived root (no key-share) and backfill.
        let (_b_dir, mut ndb_b) = test_ndb();
        ndb_b.add_key(&secret);
        let mut host_b = HostPrivateSync::new();

        let mut delivered = false;
        for _ in 0..500 {
            // A's host fans its locally-ingested envelope; B's host backfills it.
            host_a.update(&mut ndb_a, &account.pubkey, &secret, relays, app_roots);
            host_b.update(&mut ndb_b, &account.pubkey, &secret, relays, app_roots);
            if ndb_has(&ndb_b, &inner_id) {
                delivered = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            delivered,
            "an app-registered channel's 1081 envelope should fan from A's host and \
             auto-unwrap on B's"
        );

        relay.shutdown();
    }

    /// The catch-up path: an envelope sealed and locally-ingested *before* its root
    /// is registered still fans out. This is the shape of a board definition or the
    /// notebook's first canvas — authored, then the app registers the channel — and
    /// the live outbound sub (future-commits-only) misses it, so without
    /// [`HostPrivateSync::fan_out_channel_catchup`] the very first sealed write of
    /// every channel would silently never leave the device.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn host_catches_up_envelope_sealed_before_registration() {
        use nostrdb_net::relay::server;
        use std::time::Duration;

        let root = test_root(0x77);
        let sns_keys = nostrdb_net::sns::derive_sns_keys(&root).expect("sns keys");
        let app_roots = std::slice::from_ref(&root);

        let (_relay_dir, relay_ndb) = test_ndb();
        let relay = server::spawn(relay_ndb.clone(), "127.0.0.1:0".parse().expect("addr"))
            .expect("spawn relay");
        let url = NormRelayUrl::new(&relay.url()).expect("relay url");
        let relays = std::slice::from_ref(&url);

        let account = FullKeypair::generate();
        let secret = account.secret_key.secret_bytes();

        // Device A: seal + locally ingest an envelope FIRST, register the root only
        // afterwards (the app-registers-after-write ordering). add_team_root up front
        // just lets nostrdb unwrap it locally; the outbound fan must still catch it.
        let (_a_dir, mut ndb_a) = test_ndb();
        ndb_a.add_key(&secret);
        assert!(ndb_a.add_team_root(&root));
        let rumor = NoteBuilder::new()
            .kind(1)
            .content("definition sealed before registration")
            .created_at(HOST_TEST_TS)
            .sign(&secret)
            .build()
            .expect("rumor");
        let inner_id = *rumor.id();
        let envelope = nostrdb_net::sns::wrap_rumor(
            &sns_keys,
            &account,
            &rumor.json().expect("json"),
            HOST_TEST_TS,
        )
        .expect("sns envelope");
        ndb_a
            .process_event(&format!(
                "[\"EVENT\",\"_local\",{}]",
                envelope.json().expect("envelope json")
            ))
            .expect("local envelope ingest");
        // Only now does the app register the channel with the host — the envelope
        // already predates the outbound sub this opens.
        let mut host_a = HostPrivateSync::new();

        let (_b_dir, mut ndb_b) = test_ndb();
        ndb_b.add_key(&secret);
        let mut host_b = HostPrivateSync::new();

        let mut delivered = false;
        for _ in 0..500 {
            host_a.update(&mut ndb_a, &account.pubkey, &secret, relays, app_roots);
            host_b.update(&mut ndb_b, &account.pubkey, &secret, relays, app_roots);
            if ndb_has(&ndb_b, &inner_id) {
                delivered = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            delivered,
            "an envelope sealed before its root was registered should still catch up \
             and fan to the other device"
        );

        relay.shutdown();
    }

    // ===== outbound gift-wrap (self-share) leg =====

    /// The kind-1059 gift-wrap's event id, read off its wire JSON.
    fn giftwrap_id(giftwrap_json: &str) -> [u8; 32] {
        let wrap: serde_json::Value = serde_json::from_str(giftwrap_json).expect("giftwrap json");
        let mut id = [0u8; 32];
        hex::decode_to_slice(wrap["id"].as_str().expect("giftwrap id"), &mut id)
            .expect("hex giftwrap id");
        id
    }

    /// The outbound gift-wrap leg end to end: a board sealed on device A becomes
    /// **joinable** on a fresh device B that only ever talks to the private relay A
    /// fans out to.
    ///
    /// Device A mints a self-share (as `headway::store::create_shared_board` does)
    /// and seals the board definition into the channel; its host fans out both the
    /// kind-1059 gift-wrap carrying the *key* and the kind-1081 envelope carrying
    /// the *content*. Device B starts with nothing but the account key. Its inbound
    /// gift-wrap pull lives at the account level (`accounts.rs`'s `#p`-keyed
    /// `giftwrap_live_filter`), so that one sub is modelled here; everything after
    /// it is B's own host — nostrdb peels the key-share, the roster grows, the
    /// channel is derived and registered, the 1081 backfills and unseals.
    ///
    /// Before this leg existed nothing ever carried a 1059 to a private relay, so
    /// B's roster stayed empty forever and the board was invisible off-device.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn host_fans_selfshare_so_sealed_board_joins_on_another_device() {
        use nostrdb_net::relay::server;
        use std::time::Duration;

        let root = test_root(0x88);
        let sns_keys = nostrdb_net::sns::derive_sns_keys(&root).expect("sns keys");

        // The shared private relay over its own opaque db: never seeded with the
        // account key or the root, so it only ever holds the gift-wrap and the
        // envelope, never anything it could read out of them.
        let (_relay_dir, relay_ndb) = test_ndb();
        let relay = server::spawn(relay_ndb.clone(), "127.0.0.1:0".parse().expect("addr"))
            .expect("spawn relay");
        let url = NormRelayUrl::new(&relay.url()).expect("relay url");
        let relays = std::slice::from_ref(&url);

        let account = FullKeypair::generate();
        let secret = account.secret_key.secret_bytes();

        // Device A: mint the board's self-share — a key-share we author, addressed
        // to ourselves — and let nostrdb peel it into the roster. This happens
        // *before* the host exists, the real ordering for a board sealed on a
        // previous run or by the CLI, so only the catch-up leg can fan it.
        let (_a_dir, mut ndb_a) = test_ndb();
        ndb_a.add_key(&secret);
        ingest_giftwrap(
            &ndb_a,
            &gift_wrapped_keyshare(&account, &account.pubkey, &root, Some("30619:owner:board")),
        );
        wait_roots(&ndb_a, &account.pubkey, 1).await;

        // One pump registers the root off the roster so nostrdb unseals the channel.
        let mut host_a = HostPrivateSync::new();
        host_a.update(&mut ndb_a, &account.pubkey, &secret, relays, &[]);

        // Seal the board definition into the channel, as `create_shared_board` does.
        let definition = NoteBuilder::new()
            .kind(1)
            .content("sealed board definition")
            .created_at(HOST_TEST_TS)
            .sign(&secret)
            .build()
            .expect("definition rumor");
        let definition_id = *definition.id();
        let envelope = nostrdb_net::sns::wrap_rumor(
            &sns_keys,
            &account,
            &definition.json().expect("json"),
            HOST_TEST_TS,
        )
        .expect("sns envelope");
        ndb_a
            .process_event(&format!(
                "[\"EVENT\",\"_local\",{}]",
                envelope.json().expect("envelope json")
            ))
            .expect("local envelope ingest");

        // Device B: a fresh cache holding only the account key, plus the
        // account-level gift-wrap inbox sub against the same private relay.
        let (_b_dir, mut ndb_b) = test_ndb();
        ndb_b.add_key(&secret);
        let giftwrap_filter = Filter::new()
            .kinds([1059])
            .pubkeys([account.pubkey.bytes()])
            .build();
        let inbox_b = Session::new(ndb_b.clone());
        inbox_b.set_subscription(
            "test/giftwrap-inbox",
            url.to_string(),
            vec![giftwrap_filter.clone()],
            vec![giftwrap_filter],
        );
        let mut host_b = HostPrivateSync::new();

        let mut joined = false;
        for _ in 0..500 {
            host_a.update(&mut ndb_a, &account.pubkey, &secret, relays, &[]);
            host_b.update(&mut ndb_b, &account.pubkey, &secret, relays, &[]);
            if ndb_has(&ndb_b, &definition_id) {
                joined = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            joined,
            "A should fan its self-share gift-wrap so B joins the channel and \
             unseals the board definition"
        );
        assert!(
            registered_roots(&ndb_b, &account.pubkey).contains(&root),
            "B's roster should have grown from the fanned-out gift-wrap"
        );

        inbox_b.drop_subscription("test/giftwrap-inbox");
        relay.shutdown();
    }

    /// The scoping guard: of two gift-wraps addressed to us, only the one whose
    /// peeled key-share *we* authored is fanned.
    ///
    /// The stranger's wrap is both a genuine co-member invite (not ours to
    /// republish) and the shape a spammer would use. Fanning everything `#p`-tagged
    /// to us would make notedeck a write-amplifier into our own private relay, and
    /// the seen-on check would not catch it: a wrap pulled from a *public*
    /// accounts-read relay has seen-on = that public relay, so it looks unsent to
    /// the private one. Scoping on the peeled rumor's author closes that off.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn foreign_giftwrap_addressed_to_us_is_not_fanned() {
        let (_dir, ndb) = test_ndb();
        let account = FullKeypair::generate();
        let stranger = FullKeypair::generate();
        ndb.add_key(&account.secret_key.secret_bytes());

        let ours = gift_wrapped_keyshare(
            &account,
            &account.pubkey,
            &test_root(0x88),
            Some("30619:us:mine"),
        );
        let theirs = gift_wrapped_keyshare(
            &stranger,
            &account.pubkey,
            &test_root(0x99),
            Some("30619:them:theirs"),
        );
        ingest_giftwrap(&ndb, &ours);
        ingest_giftwrap(&ndb, &theirs);
        // Both peel into the roster — the roster keeps a co-member's share, the
        // outbound leg does not.
        wait_roots(&ndb, &account.pubkey, 2).await;

        let txn = Transaction::new(&ndb).expect("txn");
        let rumors = ndb.query(&txn, &[keyshare_filter()], 500).expect("query");
        let fanned_ids: Vec<[u8; 32]> = own_selfshare_giftwraps(
            &ndb,
            &txn,
            &account.pubkey,
            rumors.iter().map(|res| res.note_key),
        )
        .iter()
        .map(|key| *ndb.get_note_by_key(&txn, *key).expect("wrap note").id())
        .collect();

        assert_eq!(
            fanned_ids,
            vec![giftwrap_id(&ours)],
            "only the gift-wrap of the self-share we minted is fanned"
        );
        assert!(
            !fanned_ids.contains(&giftwrap_id(&theirs)),
            "a stranger's gift-wrap must never be amplified into our private relay"
        );
    }

    /// A registered root derives a stable team pubkey and a well-formed envelope
    /// filter; registration is idempotent.
    #[test]
    fn registers_roots_and_derives_channel() {
        let (_dir, ndb) = test_ndb();
        let root = test_root(0x44);
        // First registration reaches nostrdb; a repeat is dropped before it does,
        // so nostrdb's fixed root table never sees the same root twice.
        let mut registered = RegisteredRoots::default();
        registered.register(&ndb, &[root]);
        registered.register(&ndb, &[root]);
        assert_eq!(registered.0.len(), 1, "the root is recorded once");

        let pk = team_pubkey(&root).expect("team pubkey");
        let filter = team_envelope_filter(&pk);
        // The filter targets the channel's 1081 stream authored by the team pubkey.
        assert!(filter.json().expect("filter json").contains("1081"));
    }

    /// A roster that grows one root at a time re-declares — and so re-registers —
    /// the *whole* set each time. Those repeats must not cost anything in nostrdb's
    /// fixed root table, or a board joined late in a long session never peels.
    ///
    /// nostrdb holds registered roots in a 128-slot per-ingester-thread array and
    /// `ndb_add_team_root` appends without deduping, so twenty roots arriving one at
    /// a time used to spend 1+2+…+20 = 210 of those slots. Every root after the
    /// array filled was dropped *silently* — `add_team_root` reports the dispatch,
    /// not the acceptance — leaving the board listed from its key-share with a
    /// shared fold that never fills ("Loading shared board…" until a restart).
    ///
    /// The last channel's envelope is ingested *before* its root is registered, so
    /// this exercises the [`Ndb::process_sns`] catch-up peel rather than the
    /// ingest-time one — the order a board sealed on another device arrives in.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn late_root_peels_after_a_roster_grew_one_root_at_a_time() {
        use std::time::Duration;

        let (_dir, mut ndb) = test_ndb();
        let account = FullKeypair::generate();
        let secret = account.secret_key.secret_bytes();
        ndb.add_key(&secret);
        let mut host = HostPrivateSync::new();

        // Roots trickle in one at a time, as apps register them and key-shares land.
        // Twenty is well past where re-registering the whole set each time overruns
        // nostrdb's table: measured on this tree, 120 cumulative registrations still
        // peel and 136 do not.
        let mut roots: Vec<[u8; 32]> = Vec::new();
        for i in 0..16u8 {
            roots.push(test_root(i));
            host.update(&mut ndb, &account.pubkey, &secret, &[], &roots);
        }

        // A genuinely new channel whose envelope is already in ndb — an envelope
        // pushed in through the embedded relay before its root was known.
        let root = test_root(200);
        let sns_keys = nostrdb_net::sns::derive_sns_keys(&root).expect("sns keys");
        let rumor = NoteBuilder::new()
            .kind(1)
            .content("late board definition")
            .created_at(HOST_TEST_TS)
            .sign(&secret)
            .build()
            .expect("rumor");
        let inner_id = *rumor.id();
        let envelope = nostrdb_net::sns::wrap_rumor(
            &sns_keys,
            &account,
            &rumor.json().expect("rumor json"),
            HOST_TEST_TS,
        )
        .expect("sns envelope");
        let envelope_id = *envelope.id();
        ndb.process_event(&format!(
            "[\"EVENT\",\"e\",{}]",
            envelope.json().expect("envelope json")
        ))
        .expect("ingest envelope");
        // Wait out the async ingest so the envelope is durably committed *before*
        // the root is registered — the catch-up peel is what must find it.
        for _ in 0..250 {
            if ndb_has(&ndb, &envelope_id) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(ndb_has(&ndb, &envelope_id), "envelope never landed");

        roots.push(root);
        let mut peeled = false;
        for _ in 0..250 {
            host.update(&mut ndb, &account.pubkey, &secret, &[], &roots);
            if ndb_has(&ndb, &inner_id) {
                peeled = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            peeled,
            "a root joined after the roster grew never peeled its envelope: \
             nostrdb's root table was full of duplicate registrations"
        );
    }
}
