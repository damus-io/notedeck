//! The CLI's relay legs beyond the plaintext reconcile: the kind-1059
//! gift-wrap pull, the per-channel kind-1081 envelope sync, the own-board
//! self-share flush (and its per-cache marker), and joining an own sealed board
//! by deriving its channel.

use nostrdb::{Ndb, Transaction};
use nostrdb_net::Pubkey;
use serde_json::json;

use headway::event;
use headway::store;
use headway::teams;

use crate::{APP, Collect, Roster};

/// Upper bound for a windowed reconcile's `created_at` search: `u32::MAX` (unix
/// second `4294967295` ≈ year 2106) — past any real event time, yet within the
/// 32-bit range nostrdb's filter `until` accepts (a larger value fails
/// `Filter::from_json` with `BufferOverflow`). Windows over the relay's per-sync
/// cap hone in by bisection, so an over-wide upper bound costs only a few cheap
/// empty-range reconciles.
const RECONCILE_UNTIL: u64 = u32::MAX as u64;

/// The plaintext reconcile filter: every headway event authored by us that is
/// *not* an unwrapped rumor.
///
/// [`event::headway_filter`] also matches a sealed board's locally-unwrapped
/// rumors — a rumor keeps its original author, so an account-authored sealed
/// edit still matches `authors:[account]`. Those rumors live only in this cache
/// (the relay holds their kind-1081 envelopes instead), so feeding them to the
/// bidirectional reconcile would flush them to the relay *as plaintext*: a leak
/// that also never converges, since the relay keeps the envelope, not the
/// plaintext, so the reconcile re-opens the same diff every run. The custom
/// `!is_rumor()` element drops them from both the negentropy "have" set and the
/// push frames (`local_set` and `frames_where` both fold with this filter),
/// mirroring notedeck's `fan_out_unseen_notes` `is_rumor` guard.
///
/// Write-side only, by design: the wire pull filter is built from kinds+author
/// independently of this one, so inbound plaintext sync is unaffected, and reads
/// never route through this filter (they fold via `load_board`). A
/// genuinely-plaintext board's events aren't rumors, so they still reconcile
/// both ways.
pub(crate) fn plaintext_sync_filter(author: &Pubkey) -> nostrdb::Filter {
    nostrdb::Filter::new()
        .authors([author.bytes()])
        .kinds(event::HEADWAY_KINDS.iter().map(|k| *k as u64))
        .custom(|note| !note.is_rumor())
        .build()
}

/// Reconcile every joined board's kind-1081 SNS envelopes with the relay, both
/// ways, keyed by each channel's team pubkey.
///
/// A sealed board's edits travel as kind-1081 envelopes signed by the channel's
/// *team* keypair, not the account — so they match neither the account-scoped
/// plaintext [`event::headway_filter`] reconcile nor the kind-1059 gift-wrap
/// pull ([`pull_giftwraps`]). Without this leg a fresh cache never pulls a sealed
/// board down, and a seal made offline never flushes up. Each channel is
/// reconciled under its team pubkey: the pull brings a co-member's (or another
/// device's) envelopes down, the push flushes envelopes we sealed while offline.
/// nostrdb auto-unwraps every pulled envelope against the roots [`Roster::load`]
/// registered, so the peeled rumors become queryable and the board folds.
///
/// Bidirectional like the plaintext reconcile, but per channel: an envelope is
/// immutable (nothing is addressable, so there is no revision to dedup) and
/// authored by exactly one team key, so each distinct channel pubkey is its own
/// single-author reconcile. Best-effort: an unreachable or non-NIP-77 relay just
/// leaves us with the channels already in the cache.
pub(crate) async fn sync_envelopes(
    relay: &mut nostrdb_net::relay::sync::Relay,
    ndb: &Ndb,
    roster: &Roster,
) {
    // Every distinct channel pubkey across all joined boards — a board can span
    // several channels, and one channel can back several boards, so dedup.
    let mut pubkeys: Vec<Pubkey> = Vec::new();
    for team in &roster.teams {
        let Some(pk) = team.sns_keys().map(|keys| keys.team_keypair.pubkey) else {
            continue;
        };
        if !pubkeys.contains(&pk) {
            pubkeys.push(pk);
        }
    }
    if pubkeys.is_empty() {
        return;
    }

    let envelope_kinds = [nostrdb_net::sns::SNS_ENVELOPE_KIND];
    let before = count_matching(ndb, &teams::envelope_filter(&pubkeys));
    for pk in &pubkeys {
        let filter = teams::envelope_filter(std::slice::from_ref(pk));
        let author = nostrdb_net::Pubkey::new(*pk.bytes());
        if let Err(e) = nostrdb_net::relay::sync::reconcile_sync(
            relay,
            ndb,
            &author,
            &envelope_kinds,
            &filter,
            &|_| false,
        )
        .await
        {
            eprintln!("warning: couldn't sync sealed-board envelopes: {e}");
        }
    }

    // Peel any envelopes that just arrived. Auto-unwrap fires on ingest for an
    // already-registered root, but a `process_sns` catch-up covers an envelope
    // that landed before its root was registered — the envelope counterpart of
    // `pull_giftwraps`' peel. Gated on new arrivals so the whole-store walk isn't
    // paid on a command that pulled nothing.
    if count_matching(ndb, &teams::envelope_filter(&pubkeys)) > before
        && let Ok(txn) = Transaction::new(ndb)
    {
        ndb.process_sns(&txn);
    }
}

/// Pull the kind-1059 gift-wraps addressed to `author` into the local cache, so
/// nostrdb can peel out the key-shares the roster is derived from.
///
/// Pull-only and best-effort: gift-wraps are addressed *to* us, so there is
/// nothing of ours to push back, and a relay that can't serve them just leaves us
/// with the boards we already knew about.
pub(crate) async fn pull_giftwraps(
    relay: &mut nostrdb_net::relay::sync::Relay,
    ndb: &Ndb,
    author: &Pubkey,
) {
    let filter = teams::giftwrap_filter(author);
    let wire = json!({ "kinds": [1059], "#p": [author.hex()] }).to_string();
    let before = count_matching(ndb, &filter);
    // Windowed, not a plain `pull_reconcile`: an active account's kind-1059 inbox
    // can exceed the relay's per-sync negentropy cap, and the un-windowed pull's
    // capped-`REQ` fallback would then see only the newest slice and re-fetch it
    // every run. `pull_reconcile_windowed` bisects `created_at` so each sub-sync
    // stays under the cap. `RECONCILE_UNTIL` bounds the search past any real event
    // time yet within nostrdb's 32-bit `until`.
    if let Err(e) =
        nostrdb_net::relay::sync::pull_reconcile_windowed(relay, ndb, &wire, RECONCILE_UNTIL).await
    {
        eprintln!("warning: couldn't sync shared-board key-shares: {e}");
    }
    // Peel what just arrived, but only if something did. A gift-wrap is unwrapped
    // against the registered account keys as it is ingested; one that landed in an
    // earlier run — before this key was registered, or before the peel existed —
    // stays sealed and invisible to the roster until this catch-up pass, the
    // giftwrap counterpart of `register_teams`' `process_sns`. It walks *every*
    // stored wrap and attempts a decrypt per wrap, so running it unconditionally
    // would re-try thousands of other people's wraps on every command, printing a
    // failure line for each. Gated on new arrivals it runs at most once per sync
    // that actually brought something.
    if count_matching(ndb, &filter) > before
        && let Ok(txn) = Transaction::new(ndb)
    {
        ndb.process_giftwraps(&txn);
    }
}

/// Push half of the giftwrap leg (see headway:headway/basic-owner-torch): for
/// every shared board we OWN, re-publish its kind-1059 self-share so another of
/// the account's devices — or a co-member — can join it.
///
/// [`pull_giftwraps`] is pull-only, and neither the plaintext reconcile nor
/// [`sync_envelopes`] carries a kind-1059: a board sealed while offline (or by a
/// front end that never fanned its self-share) flushes its *content* up but not
/// the *key* to read it, so a fresh cache pulls the envelopes and folds nothing.
/// This closes that gap. A self-share's kind-1059 is authored by an ephemeral
/// gift-wrap key, so we can't pick our own out of the stored wraps to forward
/// them — instead we regenerate each from the roster's `team_root` via
/// [`store::share_board`] (recipient = ourselves), exactly as `seed`/`migrate` do.
///
/// Scoped tightly, because re-wrapping is not idempotent (a fresh ephemeral key
/// each run) so an ungated flush would spam the account's inbox every command:
/// - **Own boards only** — never re-broadcast a co-member's inbound share.
/// - **Once per cache** — a per-db marker records the roots already flushed
///   ([`flushed_marker_path`]); a fresh cache re-flushes, a repeat run is a no-op.
/// - **Real headway channels only** — skip a root whose coordinate doesn't fold a
///   headway board. A slug can collide with another app's derived root (the
///   notebook canvas also derives `derive_board_root(secret, "notebook")`), and
///   re-advertising that root here would cross-wire this coordinate onto the
///   foreign channel.
pub(crate) async fn flush_own_selfshares(
    relay: &mut nostrdb_net::relay::sync::Relay,
    ndb: &Ndb,
    roster: &Roster,
    author: &Pubkey,
    secret: &[u8; 32],
    db: Option<&str>,
) {
    // A board coordinate is `30619:<owner>:<slug>`; ours start with this prefix.
    let owner_prefix = format!("{}:{}:", event::KIND_BOARD as u64, author.hex());
    let mut flushed = read_flushed_selfshares(db);
    let mut sink = Collect::default();
    let mut newly: Vec<String> = Vec::new();
    for team in &roster.teams {
        if !team.board_addr.starts_with(&owner_prefix) || flushed.contains(&team.team_root) {
            continue;
        }
        let Some(root) = team.root_bytes() else {
            continue;
        };
        if !folds_headway_board(ndb, team) {
            continue;
        }
        if store::share_board(ndb, secret, author, &team.board_addr, &root, &mut sink) {
            newly.push(team.team_root.clone());
        }
    }
    if sink.0.is_empty() {
        return;
    }
    match relay.publish(&sink.0).await {
        Ok(()) => {
            eprintln!("flushed {} own self-share(s) to the relay", sink.0.len());
            flushed.extend(newly);
            if let Err(e) = write_flushed_selfshares(db, &flushed) {
                eprintln!("warning: couldn't record flushed self-shares: {e}");
            }
        }
        Err(e) => eprintln!("warning: couldn't flush self-shares: {e}"),
    }
}

/// Join an own sealed board by *deriving* its channel from its slug, for the case
/// where no kind-1059 key-share for it is in this cache.
///
/// `seed`/`migrate` derive a board's root as
/// `derive_board_root(account secret, slug)` and never mint one randomly, exactly
/// so the same slug converges to the same channel on every device. That makes the
/// key-share redundant *for a board we own and can name*: the root is
/// recomputable, so a device that never received the self-share can still
/// register the channel, pull its envelopes and fold the board. The 1059 is still
/// what *enumerates* boards — a slug nobody told us cannot be derived, so this
/// only ever runs for a board named on the command line.
///
/// Returns whether the board now folds. On success it also self-shares the root,
/// which both persists the join (the next run finds it in the roster with no
/// derivation) and puts the missing 1059 back on the relay for the account's
/// other devices. The self-share is deliberately emitted *after* the fold check,
/// so a mistyped slug reports "no board" rather than minting a key-share and
/// registering a phantom channel.
pub(crate) async fn recover_derived_board(
    relay: &mut nostrdb_net::relay::sync::Relay,
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    board_id: &str,
) -> bool {
    let root = nostrdb_net::sns::derive_board_root(secret, board_id);
    let Some(keys) = nostrdb_net::sns::derive_sns_keys(&root) else {
        return false;
    };
    let team_pk = keys.team_keypair.pubkey;
    // Register before pulling so arriving envelopes auto-unwrap; `process_sns`
    // below still covers any that were already cached from an earlier run.
    ndb.add_team_root(&root);

    let filter = teams::envelope_filter(std::slice::from_ref(&team_pk));
    let before = count_matching(ndb, &filter);
    let wire_author = nostrdb_net::Pubkey::new(*team_pk.bytes());
    if let Err(e) = nostrdb_net::relay::sync::reconcile_sync(
        relay,
        ndb,
        &wire_author,
        &[nostrdb_net::sns::SNS_ENVELOPE_KIND],
        &filter,
        &|_| false,
    )
    .await
    {
        eprintln!("warning: couldn't sync derived-board envelopes: {e}");
    }
    if count_matching(ndb, &filter) > before
        && let Ok(txn) = Transaction::new(ndb)
    {
        ndb.process_sns(&txn);
    }

    let addr = event::board_address(author, board_id);
    {
        let Ok(txn) = Transaction::new(ndb) else {
            return false;
        };
        if event::load_shared_board(ndb, &txn, &addr, &[team_pk]).is_none() {
            return false;
        }
    }

    // The board is real. Self-share the root so this device keeps the join and
    // the account's other devices finally get the key-share they never saw.
    let mut sink = Collect::default();
    if store::share_board(ndb, secret, author, &addr, &root, &mut sink)
        && let Err(e) = relay.publish(&sink.0).await
    {
        eprintln!("warning: couldn't publish the recovered self-share: {e}");
    }
    eprintln!("joined '{board_id}' by deriving its channel (no key-share was cached)");
    true
}

/// Whether `team`'s coordinate folds an actual headway board (its sealed
/// definition is present), distinguishing a genuine shared board from a root that
/// only collides with a foreign app's channel (which carries no headway events).
fn folds_headway_board(ndb: &Ndb, team: &teams::Team) -> bool {
    let Some(keys) = team.sns_keys() else {
        return false;
    };
    let Ok(txn) = Transaction::new(ndb) else {
        return false;
    };
    event::load_shared_board(ndb, &txn, &team.board_addr, &[keys.team_keypair.pubkey]).is_some()
}

/// Path of the per-cache marker listing the `team_root`s whose self-share this
/// cache has already flushed up (see [`flush_own_selfshares`]). It lives in the db
/// directory: per-cache, so a fresh cache re-flushes, and — with `--db` — isolated
/// from the account's real cache, so a test never touches the developer's marker.
/// Mirrors `open_ndb`'s path logic (`--db` verbatim, else the platform data dir).
fn flushed_marker_path(db: Option<&str>) -> Option<std::path::PathBuf> {
    match db {
        Some(p) => Some(std::path::PathBuf::from(p).join("flushed_selfshares")),
        None => nostrdb_net::relay::sync::config_path(APP, "flushed_selfshares").ok(),
    }
}

/// The set of `team_root`s (hex) this cache has already flushed a self-share for.
fn read_flushed_selfshares(db: Option<&str>) -> std::collections::HashSet<String> {
    let Some(path) = flushed_marker_path(db) else {
        return std::collections::HashSet::new();
    };
    std::fs::read_to_string(path)
        .map(|s| {
            s.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Persist the flushed-root set (see [`read_flushed_selfshares`]).
fn write_flushed_selfshares(
    db: Option<&str>,
    roots: &std::collections::HashSet<String>,
) -> std::io::Result<()> {
    let Some(path) = flushed_marker_path(db) else {
        return Ok(());
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut sorted: Vec<&str> = roots.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    std::fs::write(path, sorted.join("\n"))
}

/// How many stored notes match `filter`, counted through the index walk rather
/// than by materialising them.
fn count_matching(ndb: &Ndb, filter: &nostrdb::Filter) -> usize {
    let Ok(txn) = Transaction::new(ndb) else {
        return 0;
    };
    ndb.fold(&txn, std::slice::from_ref(filter), 0usize, |n, _note| n + 1)
        .unwrap_or(0)
}
