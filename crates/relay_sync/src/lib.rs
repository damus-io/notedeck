//! Shared sync plumbing for CLIs that drive a running notedeck over its embedded
//! relay (see [`headway_cli`] and [`notebook_cli`]).
//!
//! A CLI keeps its **own** nostrdb as a cache. Each run it reconciles that cache
//! against the relay with NIP-77 negentropy — pulling the events the relay has
//! that it lacks and pushing the ones it holds that the relay lacks — then folds
//! its document locally with a domain reducer. Edits forward the events they
//! produce back to the relay so the running app sees the change.
//!
//! This crate is domain-agnostic: it deals in event kinds, nostrdb filters and
//! `["EVENT", {...}]` frames, not boards or canvases. Each CLI supplies its kinds,
//! a filter, and a predicate naming which kinds are addressable (latest-wins,
//! keyed per `(kind, d-tag)`) so stale revisions aren't re-pushed forever.

mod relay;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use enostr::Pubkey;
use negentropy::{Id, NegentropyStorageVector};
use nostrdb::{Config, Filter, Ndb, Note, Transaction};
use serde_json::json;

pub use relay::{Diff, Relay};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Default URL of notedeck's embedded relay (see `--relay-bind`, default
/// `127.0.0.1:6677`).
pub const DEFAULT_RELAY: &str = "ws://127.0.0.1:6677";

/// How many event ids to request per `REQ` when pulling reconciled events down.
/// The relay caps a single `REQ`'s stored replay, so we fetch in chunks under
/// that cap.
const ID_FETCH_CHUNK: usize = 300;

/// Open (creating if needed) a CLI's own nostrdb cache under `<data-dir>/<app>`
/// (e.g. `~/.local/share/headway-cli` on Linux), or at `db` if given.
pub fn open_ndb(db: Option<&str>, app: &str) -> Result<Ndb> {
    let path = match db {
        Some(p) => std::path::PathBuf::from(p),
        None => dirs::data_dir()
            .ok_or("no data dir; pass --db <path>")?
            .join(app),
    };
    std::fs::create_dir_all(&path)?;
    let path = path.to_str().ok_or("db path is not valid utf-8")?;
    Ok(Ndb::new(path, &Config::new())?)
}

/// Reconcile the local cache against the relay both ways, so the cache and the
/// app converge regardless of which side an edit happened on.
///
/// `kinds` is the event kinds to sync (used for the wire filter); `filter` is the
/// matching nostrdb filter (used for the local fold). `is_addressable(kind)` names
/// the kinds the caller treats as latest-wins per `(kind, d-tag)`, so [`frames_where`]
/// pushes only the winning revision of each.
///
/// Best-effort: a relay that doesn't speak NIP-77 falls back to a full NIP-01
/// sync, and a failed flush is warned rather than fatal. A failed *pull* does
/// propagate, matching the original behaviour.
pub async fn reconcile_sync(
    relay: &mut Relay,
    ndb: &Ndb,
    author: &Pubkey,
    kinds: &[u32],
    filter: &Filter,
    is_addressable: &dyn Fn(u32) -> bool,
) -> Result<()> {
    let wire = json!({ "kinds": kinds, "authors": [author.hex()] });
    match relay
        .reconcile(&wire.to_string(), local_set(ndb, filter)?)
        .await
    {
        Ok(diff) => {
            // Pull missing events by id. The relay caps a single `REQ`'s stored
            // replay, so fetch in chunks rather than one oversized filter.
            for chunk in diff.need.chunks(ID_FETCH_CHUNK) {
                let ids: Vec<String> = chunk.iter().map(hex::encode).collect();
                let received = relay
                    .sync_into(ndb, &json!({ "ids": ids }).to_string())
                    .await?;
                await_ingest(ndb, &received).await;
            }

            // Push events the relay is missing (e.g. edits made offline).
            // Best-effort: a rejected flush (or a dropped connection mid-push)
            // mustn't abort the command.
            let have: HashSet<[u8; 32]> = diff.have.iter().copied().collect();
            let pending = frames_where(ndb, filter, is_addressable, |id| have.contains(id));
            if !pending.is_empty() {
                match relay.publish(&pending).await {
                    Ok(()) => eprintln!("flushed {} local event(s) to the relay", pending.len()),
                    Err(e) => eprintln!("warning: couldn't flush local events: {e}"),
                }
            }
        }
        // Sync is best-effort. A relay that doesn't speak NIP-77 (an older
        // notedeck, or a plain NIP-01 relay) can't reconcile — fall back to a
        // full NIP-01 sync rather than failing or, worse, hanging. If even that
        // fails, warn and carry on against the cache.
        Err(e) => {
            eprintln!("warning: negentropy reconcile unavailable: {e}");
            if let Err(e) = fallback_sync(relay, ndb, kinds, author, filter, is_addressable).await {
                eprintln!("warning: fallback sync failed: {e}");
            }
        }
    }
    Ok(())
}

/// Connect to `relay_url` and reconcile the local cache against it, returning the
/// live relay — or `None` if nothing was reachable, in which case the CLI works
/// offline against the cache. The relay is best-effort: it's how fresh events sync
/// in and edits fan back out to the running app, but the cache is the source of
/// truth the CLI folds from, so an unreachable relay falls back to the cache.
pub async fn connect_and_sync(
    relay_url: &str,
    ndb: &Ndb,
    author: &Pubkey,
    kinds: &[u32],
    filter: &Filter,
    is_addressable: &dyn Fn(u32) -> bool,
) -> Result<Option<Relay>> {
    let relay = match Relay::connect(relay_url).await {
        // The app being closed is the common case, not an error worth warning
        // about — fall back to the local cache quietly.
        Ok(mut relay) => {
            reconcile_sync(&mut relay, ndb, author, kinds, filter, is_addressable).await?;
            Some(relay)
        },
        Err(e) => {
            eprintln!("warning: {e}");
            eprintln!("working offline against the local cache (--relay to point elsewhere)");
            None
        }
    };
    Ok(relay)
}

/// nostrdb ingests on background threads, so a freshly-synced event isn't
/// queryable immediately. Poll until every received id is present (ids already in
/// the cache resolve at once; new ones once the ingester commits them), or a
/// short deadline elapses.
pub async fn await_ingest(ndb: &Ndb, ids: &[[u8; 32]]) {
    if ids.is_empty() {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !all_present(ndb, ids) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn all_present(ndb: &Ndb, ids: &[[u8; 32]]) -> bool {
    let Ok(txn) = Transaction::new(ndb) else {
        return false;
    };
    ids.iter().all(|id| ndb.get_note_by_id(&txn, id).is_ok())
}

/// The sealed negentropy set of the cached events matching `filter`, keyed by
/// `(created_at, id)`. This is the local side handed to [`Relay::reconcile`].
fn local_set(ndb: &Ndb, filter: &Filter) -> Result<NegentropyStorageVector> {
    let txn = Transaction::new(ndb)?;
    let mut storage = NegentropyStorageVector::new();
    ndb.fold(
        &txn,
        std::slice::from_ref(filter),
        &mut storage,
        |acc, note| {
            // insert only fails on a bad id length, which can't happen for a stored
            // note; ignore the Result to keep the fold infallible.
            let _ = acc.insert(note.created_at(), Id::from_byte_array(*note.id()));
            acc
        },
    )?;
    storage.seal()?;
    Ok(storage)
}

/// The `["EVENT", {...}]` frames for the cached events matching `filter` whose id
/// satisfies `keep` — the events to push so the relay (and app) catch up. `keep`
/// selects which side of a reconcile to forward (the ids we hold that the relay
/// lacks).
///
/// Addressable events (those `is_addressable` accepts) are deduplicated to their
/// latest revision per `(kind, d-tag)` right in the query; immutable events pass
/// through untouched. The local cache is append-only and keeps every old
/// revision, but a relay holds only the latest and rejects the rest as
/// "replaced" — pushing stale revisions is pointless and would keep the reconcile
/// from ever converging (the dropped id can never land, so it re-flushes every
/// run). The winner follows NIP-33 resolution: newest `created_at`, ties broken
/// by the lexically lowest id, matching what the relay and app keep.
pub fn frames_where(
    ndb: &Ndb,
    filter: &Filter,
    is_addressable: &dyn Fn(u32) -> bool,
    keep: impl Fn(&[u8; 32]) -> bool,
) -> Vec<String> {
    let Ok(txn) = Transaction::new(ndb) else {
        return Vec::new();
    };

    // Threaded accumulator: the winning revision per addressable coordinate, plus
    // every immutable event in arrival order.
    type Latest = HashMap<(u32, String), (u64, [u8; 32], String)>;
    let (latest, plain) = ndb
        .fold(
            &txn,
            std::slice::from_ref(filter),
            (Latest::new(), Vec::<([u8; 32], String)>::new()),
            |(mut latest, mut plain), note| {
                let id = *note.id();
                let Ok(msg) = enostr::ClientMessage::event(&note) else {
                    return (latest, plain);
                };
                let Ok(frame) = msg.to_json() else {
                    return (latest, plain);
                };

                let kind = note.kind();
                if is_addressable(kind)
                    && let Some(d) = d_tag(&note)
                {
                    let at = note.created_at();
                    let win = latest
                        .get(&(kind, d.clone()))
                        .is_none_or(|(t, i, _)| at > *t || (at == *t && id < *i));
                    if win {
                        latest.insert((kind, d), (at, id, frame));
                    }
                } else {
                    plain.push((id, frame));
                }
                (latest, plain)
            },
        )
        .unwrap_or_default();

    latest
        .into_values()
        .map(|(_, id, frame)| (id, frame))
        .chain(plain)
        .filter(|(id, _)| keep(id))
        .map(|(_, frame)| frame)
        .collect()
}

/// The value of a note's `d` tag, if any.
fn d_tag(note: &Note) -> Option<String> {
    note.tags().iter().find_map(|tag| {
        (tag.get_str(0) == Some("d"))
            .then(|| tag.get_str(1).map(str::to_owned))
            .flatten()
    })
}

/// Degraded sync for relays that don't speak NIP-77: `REQ` the whole document in,
/// ingest it, then push any local event the relay didn't return. O(document) on
/// the wire instead of O(difference), but it keeps the CLI working against plain
/// NIP-01 relays (or a notedeck whose relay predates negentropy).
async fn fallback_sync(
    relay: &mut Relay,
    ndb: &Ndb,
    kinds: &[u32],
    author: &Pubkey,
    filter: &Filter,
    is_addressable: &dyn Fn(u32) -> bool,
) -> Result<()> {
    let wire = json!({ "kinds": kinds, "authors": [author.hex()] });
    let received = relay.sync_into(ndb, &wire.to_string()).await?;
    await_ingest(ndb, &received).await;

    let on_relay: HashSet<[u8; 32]> = received.into_iter().collect();
    let pending = frames_where(ndb, filter, is_addressable, |id| !on_relay.contains(id));
    if !pending.is_empty() {
        relay.publish(&pending).await?;
        eprintln!("flushed {} local event(s) to the relay", pending.len());
    }
    Ok(())
}

/// Forward an edit's collected frames to the relay if one is connected. With no
/// relay the events are already in the local cache; they simply won't reach the
/// running app until it's reachable, so this is a no-op.
pub async fn publish(relay: &mut Option<Relay>, frames: &[String]) -> Result<()> {
    if let Some(relay) = relay {
        relay.publish(frames).await?;
    }
    Ok(())
}

/// A trailing note for command output, flagging when a change landed only in the
/// local cache because no relay was reachable.
pub fn offline_note(relay: &Option<Relay>) -> &'static str {
    if relay.is_some() {
        ""
    } else {
        " — offline, not forwarded to the app"
    }
}

/// Mute `s` with an ANSI color, but only when stdout is a terminal — so ids
/// read as muted beside titles interactively, while a piped or redirected listing
/// stays plain text for scripts to parse.
///
/// Uses the "bright black" foreground (SGR 90), not the dim attribute (SGR 2):
/// dim is widely unimplemented — urxvt, among others, ignores it and renders
/// the id at full strength — whereas the bright-black color is part of the
/// standard 16-color palette every terminal honors.
pub fn dim(s: &str) -> String {
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        format!("\x1b[90m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

// ---------------------------------------------------------------------------
// stored signing key
// ---------------------------------------------------------------------------

/// Where the signing key lives when stored via `login`: a single `nsec...` line
/// in `<data-dir>/<app>/nsec` (e.g. `~/.local/share/headway-cli/nsec` on Linux),
/// alongside the cache. It lets a key be set once so later runs — and the agents
/// driving them — never have to pass `--nsec` or export the env var.
pub fn nsec_config_path(app: &str) -> Result<std::path::PathBuf> {
    Ok(dirs::data_dir()
        .ok_or("no data dir; set the nsec env var or pass --nsec")?
        .join(app)
        .join("nsec"))
}

/// Read the stored signing key for `app`, if any. Missing file, unreadable file,
/// or an empty one all read as "no stored key" — the caller falls back to the env
/// var or `--nsec`.
pub fn stored_nsec(app: &str) -> Option<String> {
    let contents = std::fs::read_to_string(nsec_config_path(app).ok()?).ok()?;
    let trimmed = contents.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Validate `nsec` and store it for later runs of `app`. We derive the pubkey
/// first so a malformed key is rejected before it's written, and lock the file to
/// the owner since it holds a secret.
pub fn login(nsec: &str, app: &str) -> Result<()> {
    let (_, pubkey) = parse_nsec(nsec)?;
    let path = nsec_config_path(app)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, format!("{nsec}\n"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    println!(
        "stored signing key for {} in {}",
        pubkey.hex(),
        path.display()
    );
    Ok(())
}

/// Forget the stored signing key for `app`. Removing a key that isn't there is
/// not an error.
pub fn logout(app: &str) -> Result<()> {
    let path = nsec_config_path(app)?;
    match std::fs::remove_file(&path) {
        Ok(()) => println!("removed stored signing key at {}", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("no stored signing key"),
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

/// Decode an `nsec...` into raw secret bytes and its derived pubkey.
pub fn parse_nsec(nsec: &str) -> Result<([u8; 32], Pubkey)> {
    let (hrp, data) = bech32::decode(nsec).map_err(|_| "invalid nsec (not bech32)")?;
    if hrp.as_str() != "nsec" {
        return Err(format!("expected an nsec, got '{}' key", hrp.as_str()).into());
    }
    let secret: [u8; 32] = data
        .try_into()
        .map_err(|_| "nsec did not decode to 32 bytes")?;
    let keypair = enostr::Keypair::from_secret(enostr::SecretKey::from_slice(&secret)?);
    Ok((secret, keypair.pubkey))
}
