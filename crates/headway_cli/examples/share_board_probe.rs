//! Share one of your sealed boards with another pubkey — the spike behind a
//! future `headway share <board> <npub>` (headway:headway/plunge-guide-despair).
//!
//! A sealed board's channel key is `derive_board_root(owner secret, slug)`, so
//! the owner can re-derive it from the slug alone and gift-wrap it to a new
//! member as a kind-1082 key-share ([`store::share_board`]). The member's
//! nostrdb unwraps that into the roster, registers the root, and from then on
//! peels every kind-1081 envelope of the board — all existing history included,
//! with no re-seal.
//!
//! Deliberately takes the signing key **only** from `HEADWAY_NSEC` and a cache
//! only from `--db`: it never falls back to the key or cache `headway login`
//! stored, so a probe run can't sign as (or pollute the cache of) the account
//! that is logged in.
//!
//! ```text
//! HEADWAY_NSEC=nsec1… cargo run -p headway_cli --example share_board_probe -- \
//!     --db /tmp/owner-db --board jex0-spike --to npub1…
//! ```
//!
//! Every run wraps with a fresh ephemeral key, so it is not idempotent on the
//! wire: re-running publishes another (harmless, deduplicated-by-roster) share.

use headway::store::{self, Publisher};
use headway::{event, teams};
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync;

/// Only used to name the (ignored) default cache path; `--db` is required.
const APP: &str = "headway-cli";

/// A [`Publisher`] that collects the `["EVENT", {...}]` frames so they can be
/// sent to the relay after the local ingest succeeded.
#[derive(Default)]
struct Collect(Vec<String>);

impl Publisher for Collect {
    fn publish(&mut self, frame: &str) {
        self.0.push(frame.to_string());
    }
}

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(1);
}

#[tokio::main]
async fn main() {
    enostr::install_crypto();

    let mut db: Option<String> = None;
    let mut board: Option<String> = None;
    let mut to: Option<String> = None;
    let mut relay_url = sync::DEFAULT_RELAY.to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--db" => db = args.next(),
            "--board" => board = args.next(),
            "--to" => to = args.next(),
            "--relay" => relay_url = args.next().unwrap_or(relay_url),
            other => die(format!("unknown argument: {other}")),
        }
    }
    let db = db.unwrap_or_else(|| die("--db <dir> is required (never the shared CLI cache)"));
    let slug = board.unwrap_or_else(|| die("--board <slug> is required"));
    let to = to.unwrap_or_else(|| die("--to <npub|hex> is required"));
    let recipient = Pubkey::parse(&to).unwrap_or_else(|e| die(format!("bad --to: {e}")));

    let nsec = std::env::var("HEADWAY_NSEC")
        .unwrap_or_else(|_| die("HEADWAY_NSEC is required (the stored login is never used)"));
    let (secret, owner) = sync::parse_nsec(&nsec).unwrap_or_else(|e| die(e));
    let owner = Pubkey::new(*owner.bytes());

    let ndb = sync::open_ndb(Some(&db), APP).unwrap_or_else(|e| die(e));
    ndb.add_key(&secret);

    let root = nostrdb_net::sns::derive_board_root(&secret, &slug);
    let addr = event::board_address(&owner, &slug);
    let team_pk = nostrdb_net::sns::derive_sns_keys(&root)
        .map(|k| k.team_keypair.pubkey.hex())
        .unwrap_or_else(|| die("derived root has no usable keys"));

    // Refuse to hand out a key for a board the owner hasn't sealed under the
    // derived root: the recipient would join a channel with nothing in it.
    let own = teams::teams_from_ndb(&ndb, &owner);
    if !own
        .iter()
        .any(|t| t.board_addr == addr && t.root_bytes() == Some(root))
    {
        die(format!(
            "'{slug}' is not a sealed board under its derived root in {db} — \
             run `headway --db {db} --board {slug} seed` first"
        ));
    }

    let mut sink = Collect::default();
    if !store::share_board(&ndb, &secret, &recipient, &addr, &root, &mut sink) {
        die("failed to wrap the key-share");
    }
    let mut relay = sync::Relay::connect(&relay_url)
        .await
        .unwrap_or_else(|e| die(format!("couldn't connect to {relay_url}: {e}")));
    relay
        .publish(&sink.0)
        .await
        .unwrap_or_else(|e| die(format!("publish failed: {e}")));
    println!("shared {addr}");
    println!("  with      {}", recipient.hex());
    println!("  team pk   {team_pk}");
    println!("  relay     {relay_url} ({} frame)", sink.0.len());
}
