//! Shared-board membership: the SNS `team_root`s the account has joined.
//!
//! Possession of a `team_root` is membership in a shared board's channel (see
//! `docs/nip-sns-sealed-shared-storage.md`). nostrdb is only the *mechanism* —
//! register a root with [`Ndb::add_team_root`] and it auto-unwraps that channel's
//! kind-1081 envelopes — while *joining* (which roots to register, and re-register
//! on boot) is **policy**, which lives here.
//!
//! The policy is shared, not per-front-end: the egui app and the CLI must agree
//! on which boards are shared, or one of them folds a board the other seals and
//! they diverge (the CLI writing plaintext to a sealed board is invisible in the
//! app, since [`crate::event::fold_shared_board`] ingests only sealed rumors).
//! So this lives beside the fold it feeds rather than in either front end.
//!
//! The roster is not stored on disk: it *already* lives in nostrdb. A member is
//! added by gift-wrapping them a kind-1082 key-share, and nostrdb unwraps the
//! `1059` into a durable, queryable `1082` rumor. [`teams_from_ndb`] reads those
//! rumors back — so joined boards survive a restart (the rumors persist) and ride
//! the account's NIP-59 inbox across devices, with no per-device config file. The
//! `team_root`s themselves are ephemeral in nostrdb (registered keys don't survive
//! a restart), so [`register_teams`] re-registers the derived roster each boot,
//! mirroring `add_key` for account keys.

use enostr::Pubkey;
use nostrdb::{Filter, Ndb, Transaction};

/// One shared board the account has joined: the channel secret plus which board
/// coordinate it unlocks. Derived from the account's kind-1082 key-share rumors
/// (see [`teams_from_ndb`]) and re-registered with nostrdb on boot.
#[derive(Clone, Debug, PartialEq)]
pub struct Team {
    /// Hex of the 32-byte `team_root` secret — the shared channel key every
    /// member holds.
    pub team_root: String,
    /// The board coordinate this root unlocks: `30619:<owner-hex>:<board-id>`.
    pub board_addr: String,
    /// Rotation generation, if the key-share named one (see *Rotation* in the SNS
    /// doc). `None` for a first-generation share.
    pub epoch: Option<u32>,
    /// `created_at` of the key-share that joined this channel. Orders a board's
    /// channels when it has more than one (see [`board_channels`]).
    pub shared_at: u64,
}

impl Team {
    /// Decode [`team_root`](Self::team_root) into the raw 32-byte secret, or
    /// `None` if it isn't 32 bytes of hex.
    pub fn root_bytes(&self) -> Option<[u8; 32]> {
        let bytes = hex::decode(&self.team_root).ok()?;
        <[u8; 32]>::try_from(bytes.as_slice()).ok()
    }

    /// The board's slug (its `d`-tag id) — the trailing segment of
    /// [`board_addr`](Self::board_addr) (`30619:<owner>:<slug>`). Used to match a
    /// joined shared board against the app's active-board selection.
    pub fn board_slug(&self) -> Option<&str> {
        self.board_addr.rsplit(':').next().filter(|s| !s.is_empty())
    }

    /// Derive this team's SNS channel keys from its root, or `None` if the root is
    /// unusable. The keys are what seal an edit ([`enostr::sns::wrap_rumor`]) and
    /// name the channel to subscribe to (the team keypair's pubkey).
    pub fn sns_keys(&self) -> Option<enostr::sns::SnsKeys> {
        enostr::sns::derive_sns_keys(&self.root_bytes()?)
    }
}

/// Filter for unwrapped SNS key-share rumors (kind-1082) — the shares nostrdb has
/// peeled out of gift-wraps addressed to one of our account keys. The roster is
/// derived from these (see [`teams_from_ndb`]) and the app subscribes to the same
/// filter to notice new joins live.
pub fn keyshare_filter() -> Filter {
    Filter::new()
        .kinds([enostr::sns::KEYSHARE_KIND as u64])
        .limit(500)
        .build()
}

/// Filter for the kind-1081 SNS envelopes of one or more channels — every sealed
/// edit to a board, addressed to (and signed by) each team keypair. Takes a set
/// because one board can span several channels (see [`board_channels`]), and its
/// change signal has to cover all of them.
///
/// The envelope, not the rumor inside it, is the sync unit: nostrdb stores both,
/// but only the envelope carries the sealed bytes a *non*-keyholder relay can
/// pass along. Every shared-board edit is exactly one envelope, so a subscription
/// on this doubles as the board's change signal — new envelope means re-fold.
///
/// Deliberately unbounded: a `limit` is inert for subscription matching, and a
/// walk that a `limit` truncates would silently under-report the channel.
pub fn envelope_filter(team_pubkeys: &[Pubkey]) -> Filter {
    Filter::new()
        .kinds([enostr::sns::SNS_ENVELOPE_KIND as u64])
        .authors(team_pubkeys.iter().map(|k| k.bytes()))
        .build()
}

/// Filter for the kind-1059 gift-wraps addressed to `author` — the wire form of a
/// key-share, and the only way a *new* shared board can reach a cache.
///
/// [`keyshare_filter`] matches the 1082 rumor nostrdb peeled out of one of these,
/// which is a purely local artefact: it never travels, so a client that syncs only
/// the 1082 kind pulls nothing and derives an empty roster — and an empty roster
/// means every shared board silently looks private. A client that reads shared
/// boards must sync *this* and let nostrdb unwrap (its account key registered via
/// `Ndb::add_key`).
///
/// Unbounded, like [`envelope_filter`]: the roster is not a feed to page through,
/// and a truncated walk drops channels.
pub fn giftwrap_filter(author: &Pubkey) -> Filter {
    Filter::new()
        .kinds([1059])
        .pubkeys([author.bytes()])
        .build()
}

/// The shared boards `author` has joined, reconstructed from nostrdb.
///
/// The roster lives in the db, not on disk: every joined board arrived as a
/// kind-1082 key-share gift-wrapped to the account, and nostrdb unwrapped it into
/// a durable `1082` rumor. We query those rumors, keep the ones addressed to
/// `author` (nostrdb records the gift-wrap recipient on the rumor), and read each
/// one's `team_root` / `a` / `epoch` tags via [`enostr::sns::parse_keyshare`] — the
/// same tag parser the app's live accept path uses, so both agree on the 1082 wire
/// format (notably that a 64-hex `team_root` is an id element, not a string). Thus
/// membership survives restarts and rides the account's NIP-59 inbox across devices
/// with no config file. Shares that name no board are dropped (unfoldable); exact
/// `(team_root, board_addr)` duplicates are collapsed.
///
/// **Accept policy.** Returning a share here *is* accepting it. Today that's
/// auto-accept: any `1082` nostrdb unwrapped for us joins its board (the SNS doc
/// lists auto-accept as a valid policy, and there is no join UI yet). A future
/// user-prompt / spam-guard policy (headway#fox-allow-violin) filters exactly
/// here; everything downstream (registration, re-register-on-boot, sealing) is
/// unchanged whether the accept was automatic or confirmed.
pub fn teams_from_ndb(ndb: &Ndb, author: &Pubkey) -> Vec<Team> {
    let Ok(txn) = Transaction::new(ndb) else {
        return Vec::new();
    };
    let Ok(results) = ndb.query(&txn, &[keyshare_filter()], 500) else {
        return Vec::new();
    };
    let mut teams: Vec<Team> = Vec::new();
    for res in results {
        // A `1082` unwrapped for another local account isn't ours to join.
        if res.note.rumor_receiver_pubkey() != Some(author.bytes()) {
            continue;
        }
        let Some(share) = enostr::sns::parse_keyshare(&res.note) else {
            continue;
        };
        // A share must name the board its root unlocks, or we can't fold it later.
        let Some(board_addr) = share.board_addr else {
            continue;
        };
        let team = Team {
            team_root: hex::encode(share.team_root),
            board_addr,
            epoch: share.epoch,
            shared_at: res.note.created_at(),
        };
        if !teams
            .iter()
            .any(|t| t.team_root == team.team_root && t.board_addr == team.board_addr)
        {
            teams.push(team);
        }
    }
    teams
}

/// Every channel the roster holds for the board at `board_addr`, ordered
/// **primary first**: highest rotation [`epoch`](Team::epoch), then the earliest
/// key-share.
///
/// A board usually has exactly one channel, but it can end up with several and
/// they cannot be merged — a note is promoted to a sealed rumor only while it is
/// still plaintext, so once sealed under one root it can never move to another.
/// Rotation creates this deliberately (a new epoch is a new root); a re-seal that
/// ran under a fresh root creates it by accident. Either way both halves of the
/// board are real and readable, so a *read* must fold the union of all of them
/// (see [`crate::event::fold_shared_board`]) — picking one would truncate the
/// board, or lose it outright if the chosen channel lacks the board definition.
///
/// A *write* seals into the first entry: the newest epoch is where rotation says
/// new edits belong, and the earliest-share tie-break keeps an accidental second
/// channel from capturing them.
pub fn board_channels<'a>(teams: &'a [Team], board_addr: &str) -> Vec<&'a Team> {
    let mut matching: Vec<&Team> = teams
        .iter()
        .filter(|t| t.board_addr == board_addr)
        .collect();
    matching.sort_by_key(|t| (std::cmp::Reverse(t.epoch.unwrap_or(0)), t.shared_at));
    matching
}

/// The team public keys of [`board_channels`], in the same order — what
/// [`crate::event::fold_shared_board`] takes to gather every channel of a board.
/// Entries whose root doesn't derive usable keys are dropped.
pub fn board_channel_pubkeys(teams: &[Team], board_addr: &str) -> Vec<Pubkey> {
    board_channels(teams, board_addr)
        .into_iter()
        .filter_map(|t| Some(t.sns_keys()?.team_keypair.pubkey))
        .collect()
}

/// Register every joined `team_root` with nostrdb so it auto-unwraps that
/// channel's kind-1081 envelopes, then run one [`Ndb::process_sns`] catch-up peel
/// for envelopes that were ingested before the root was registered. Idempotent —
/// call on boot and after every account switch (mirrors `add_key`).
pub fn register_teams(ndb: &Ndb, teams: &[Team]) {
    let mut registered = false;
    for team in teams {
        if let Some(root) = team.root_bytes() {
            registered |= ndb.add_team_root(&root);
        }
    }
    // Only pay the catch-up walk when a root was actually (re-)registered.
    if registered && let Ok(txn) = Transaction::new(ndb) {
        ndb.process_sns(&txn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use enostr::FullKeypair;
    use nostr::key::PublicKey;
    use nostr::nips::nip44;
    use nostr::secp256k1::rand::rngs::OsRng;
    use nostrdb::{Config, NoteBuilder};
    use std::time::{Duration, Instant};

    fn test_root(seed: u8) -> [u8; 32] {
        let mut root = [0u8; 32];
        root[0] = 0x11;
        root[31] = seed;
        root
    }

    fn ndb() -> (tempfile::TempDir, Ndb) {
        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
        (dir, ndb)
    }

    /// Build a kind-1082 key-share rumor (all notes via nostrdb [`NoteBuilder`]),
    /// NIP-59 seal + gift-wrap it to `recipient`, and return the kind-1059 giftwrap
    /// JSON — what a sharer publishes and nostrdb unwraps back into a queryable
    /// `1082` rumor. Mirrors the app's real inbound path so the receiver filter and
    /// tag parsing are exercised end to end. Pass `board_addr = None` for a
    /// (malformed) share that names no board.
    fn gift_wrapped_keyshare(
        sender: &FullKeypair,
        recipient: &Pubkey,
        root: &[u8; 32],
        board_addr: Option<&str>,
        epoch: Option<u32>,
    ) -> String {
        let mut rumor = NoteBuilder::new()
            .kind(enostr::sns::KEYSHARE_KIND)
            .content("")
            .start_tag()
            .tag_str("team_root")
            .tag_str(&hex::encode(root));
        if let Some(addr) = board_addr {
            rumor = rumor.start_tag().tag_str("a").tag_str(addr);
        }
        if let Some(epoch) = epoch {
            rumor = rumor
                .start_tag()
                .tag_str("epoch")
                .tag_str(&epoch.to_string());
        }
        let rumor_json = rumor
            .sign(&sender.secret_key.secret_bytes())
            .build()
            .expect("rumor")
            .json()
            .expect("rumor json");

        let recipient_pk = PublicKey::from_slice(recipient.bytes()).expect("recipient pk");
        let mut rng = OsRng;
        // NIP-44 seals the rumor to the recipient, then a random wrap key seals the
        // kind-13 seal into the 1059 (encryption only — the notes are NoteBuilder).
        let encrypted_rumor = nip44::encrypt_with_rng(
            &mut rng,
            &sender.secret_key,
            &recipient_pk,
            &rumor_json,
            nip44::Version::V2,
        )
        .expect("encrypt rumor");
        let seal_json = NoteBuilder::new()
            .kind(13)
            .content(&encrypted_rumor)
            .sign(&sender.secret_key.secret_bytes())
            .build()
            .expect("seal")
            .json()
            .expect("seal json");
        let wrap_keys = FullKeypair::generate();
        let encrypted_seal = nip44::encrypt_with_rng(
            &mut rng,
            &wrap_keys.secret_key,
            &recipient_pk,
            &seal_json,
            nip44::Version::V2,
        )
        .expect("encrypt seal");
        NoteBuilder::new()
            .kind(1059)
            .content(&encrypted_seal)
            .start_tag()
            .tag_str("p")
            .tag_str(&recipient.hex())
            .sign(&wrap_keys.secret_key.secret_bytes())
            .build()
            .expect("giftwrap")
            .json()
            .expect("giftwrap json")
    }

    fn ingest(ndb: &Ndb, giftwrap_json: &str) {
        ndb.process_event(&format!("[\"EVENT\",\"_gw\",{giftwrap_json}]"))
            .expect("ingest giftwrap");
    }

    /// Poll the (async) roster until it has at least `n` teams, or time out.
    fn wait_teams(ndb: &Ndb, author: &Pubkey, n: usize) -> Vec<Team> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let teams = teams_from_ndb(ndb, author);
            if teams.len() >= n {
                return teams;
            }
            assert!(
                Instant::now() < deadline,
                "roster never reached {n} team(s)"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn reads_gift_wrapped_share_for_recipient_only() {
        let (_dir, ndb) = ndb();
        let author = FullKeypair::generate();
        let other = FullKeypair::generate();
        let sender = FullKeypair::generate();
        // nostrdb only unwraps a 1059 addressed to a *registered* account key.
        ndb.add_key(&author.secret_key.secret_bytes());

        let root = test_root(0x22);
        let board_addr = "30619:owner:headway";
        ingest(
            &ndb,
            &gift_wrapped_keyshare(&sender, &author.pubkey, &root, Some(board_addr), Some(2)),
        );

        // The roster is reconstructed from the unwrapped 1082 rumor — no disk.
        let teams = wait_teams(&ndb, &author.pubkey, 1);
        assert_eq!(teams.len(), 1);
        assert_eq!(teams[0].board_addr, board_addr);
        assert_eq!(teams[0].root_bytes(), Some(root));
        assert_eq!(teams[0].epoch, Some(2));

        // The share was gift-wrapped to `author`; it never counts as `other`'s.
        assert!(teams_from_ndb(&ndb, &other.pubkey).is_empty());
    }

    #[test]
    fn requires_a_board_and_lists_each_joined_board() {
        let (_dir, ndb) = ndb();
        let author = FullKeypair::generate();
        let sender = FullKeypair::generate();
        ndb.add_key(&author.secret_key.secret_bytes());

        // A share naming no board can't be folded later, so it's dropped.
        ingest(
            &ndb,
            &gift_wrapped_keyshare(&sender, &author.pubkey, &test_root(0x01), None, None),
        );
        // Two distinct boards each join.
        ingest(
            &ndb,
            &gift_wrapped_keyshare(
                &sender,
                &author.pubkey,
                &test_root(0x02),
                Some("30619:owner:alpha"),
                None,
            ),
        );
        ingest(
            &ndb,
            &gift_wrapped_keyshare(
                &sender,
                &author.pubkey,
                &test_root(0x03),
                Some("30619:owner:beta"),
                None,
            ),
        );

        let teams = wait_teams(&ndb, &author.pubkey, 2);
        // Exactly the two board-bearing shares — the board-less one is excluded.
        assert_eq!(teams.len(), 2);
        let mut boards: Vec<&str> = teams.iter().map(|t| t.board_addr.as_str()).collect();
        boards.sort();
        assert_eq!(boards, vec!["30619:owner:alpha", "30619:owner:beta"]);
    }
}
