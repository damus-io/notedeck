//! Diagnose — and optionally remediate — a sealed board's kind-1059 self-share.
//!
//! A team-of-one board's content travels as kind-1081 envelopes signed by the
//! channel's team key, but another device can only *join* the channel from the
//! kind-1059 gift-wrap carrying the kind-1082 key-share. When that 1059 never
//! reaches a shared relay the board is invisible everywhere but the cache that
//! created it (headway:headway/pizza-stamp-marble, headway:headway/basic-owner-torch).
//!
//! Read-only by default: prints each joined channel's board coordinate, team
//! root, and team pubkey (the author to count kind-1081 envelopes by). With
//! `--publish` it re-wraps one board's self-share and publishes it to `--relay`,
//! which is the one-off remediation for a board whose 1059 never made it up.
//!
//! `--derive` works on a board this cache has *not* joined: a board root is
//! `derive_board_root(account secret, slug)`, so the coordinate and team pubkey
//! are recomputable from the slug alone, with no key-share present. That is what
//! lets a second device diagnose — and join — a board whose self-share never
//! reached a relay it reads.
//!
//! ```text
//! cargo run -p headway_cli --example selfshare_doctor
//! cargo run -p headway_cli --example selfshare_doctor -- --board noteguard --derive
//! cargo run -p headway_cli --example selfshare_doctor -- \
//!     --board damus-website --relay ws://relay.jb55.com --publish
//! ```
//!
//! Re-wrapping picks a fresh ephemeral key every run, so a `--publish` is *not*
//! idempotent: run it once per board that needs it, never in a loop.

use headway::store::Publisher;
use headway::{event, store, teams};
use nostrdb::{Ndb, Transaction};
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync;

/// The headway CLI's cache/key directory — this example reads the same cache and
/// stored signing key the CLI does, so it sees exactly the roster `headway` sees.
const APP: &str = "headway-cli";

/// A [`Publisher`] that collects the `["EVENT", {...}]` frames instead of
/// sending them, so the caller decides whether they ever reach a relay.
#[derive(Default)]
struct Collect(Vec<String>);

impl Publisher for Collect {
    fn publish(&mut self, frame: &str) {
        self.0.push(frame.to_string());
    }
}

#[tokio::main]
async fn main() {
    enostr::install_crypto();

    let mut board: Option<String> = None;
    let mut relay_url = sync::DEFAULT_RELAY.to_string();
    let mut do_publish = false;
    let mut derive = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--board" => board = args.next(),
            "--relay" => relay_url = args.next().unwrap_or(relay_url),
            "--publish" => do_publish = true,
            "--derive" => derive = true,
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }

    let Some(nsec) = sync::stored_nsec(APP) else {
        eprintln!("no stored key — run `headway login <nsec>` first");
        std::process::exit(1);
    };
    let (secret, author) = match sync::parse_nsec(&nsec) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    let author = Pubkey::new(*author.bytes());

    let ndb = match sync::open_ndb(None, APP) {
        Ok(ndb) => ndb,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    ndb.add_key(&secret);

    // `--derive` skips the roster entirely: recompute the root from the slug and
    // report (or self-share) that channel even though no key-share for it is in
    // this cache. `--publish` here both puts the 1059 on `--relay` and — because
    // `share_board` ingests locally — joins the board on this device.
    if derive {
        let Some(slug) = board.as_deref() else {
            eprintln!("--derive needs --board <slug>");
            std::process::exit(2);
        };
        let root = nostrdb_net::sns::derive_board_root(&secret, slug);
        let addr = event::board_address(&author, slug);
        let team_pk = nostrdb_net::sns::derive_sns_keys(&root)
            .map(|k| k.team_keypair.pubkey.hex())
            .unwrap_or_else(|| "<no keys>".to_string());
        println!("board       {slug} (derived)");
        println!("  addr      {addr}");
        println!("  root      {}", hex::encode(root));
        println!("  team pk   {team_pk}");
        if !do_publish {
            return;
        }
        let mut sink = Collect::default();
        if !store::share_board(&ndb, &secret, &author, &addr, &root, &mut sink) {
            eprintln!("failed to wrap the self-share for '{slug}'");
            std::process::exit(1);
        }
        publish_to(&relay_url, &sink.0, slug).await;
        return;
    }

    let teams = teams::teams_from_ndb(&ndb, &author);
    teams::RootRegistry::default().register(&ndb, &teams);

    let owner_prefix = format!("{}:{}:", event::KIND_BOARD as u64, author.hex());
    for team in &teams {
        let slug = team.board_slug().unwrap_or("<none>");
        if board.as_deref().is_some_and(|b| b != slug) {
            continue;
        }
        let team_pk = team
            .sns_keys()
            .map(|k| k.team_keypair.pubkey.hex())
            .unwrap_or_else(|| "<no keys>".to_string());
        let ours = team.board_addr.starts_with(&owner_prefix);
        println!("board       {slug}");
        println!("  addr      {}", team.board_addr);
        println!("  root      {}", team.team_root);
        println!("  team pk   {team_pk}");
        println!("  ours      {ours}");
        println!("  folds     {}", folds_headway_board(&ndb, team));
    }

    if !do_publish {
        return;
    }
    let Some(slug) = board.as_deref() else {
        eprintln!("--publish needs --board <slug> (never re-wrap every board at once)");
        std::process::exit(2);
    };
    let Some(team) = teams
        .iter()
        .find(|t| t.board_slug() == Some(slug) && t.board_addr.starts_with(&owner_prefix))
    else {
        eprintln!("no channel we own for board '{slug}'");
        std::process::exit(1);
    };
    let Some(root) = team.root_bytes() else {
        eprintln!("board '{slug}' has no usable team root");
        std::process::exit(1);
    };

    let mut sink = Collect::default();
    if !store::share_board(&ndb, &secret, &author, &team.board_addr, &root, &mut sink) {
        eprintln!("failed to re-wrap the self-share for '{slug}'");
        std::process::exit(1);
    }
    publish_to(&relay_url, &sink.0, slug).await;
}

/// Send `frames` to `relay_url`, reporting what landed. Exits non-zero rather
/// than returning on failure: a self-share that did not reach the relay is the
/// whole bug this example exists to fix, so it must never look like success.
async fn publish_to(relay_url: &str, frames: &[String], slug: &str) {
    let mut relay = match sync::Relay::connect(relay_url).await {
        Ok(relay) => relay,
        Err(e) => {
            eprintln!("error: couldn't connect to {relay_url}: {e}");
            std::process::exit(1);
        }
    };
    match relay.publish(frames).await {
        Ok(()) => println!(
            "published {} self-share(s) for '{slug}' to {relay_url}",
            frames.len()
        ),
        Err(e) => {
            eprintln!("error: publish failed: {e}");
            std::process::exit(1);
        }
    }
}

/// Whether `team`'s coordinate folds an actual headway board, distinguishing a
/// genuine shared board from a root that only collides with another app's
/// channel. Mirrors the CLI's own guard of the same name.
fn folds_headway_board(ndb: &Ndb, team: &teams::Team) -> bool {
    let Some(keys) = team.sns_keys() else {
        return false;
    };
    let Ok(txn) = Transaction::new(ndb) else {
        return false;
    };
    event::load_shared_board(ndb, &txn, &team.board_addr, &[keys.team_keypair.pubkey]).is_some()
}
