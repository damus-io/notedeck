//! Board loading: the [`Roster`] of shared boards this account holds keys for,
//! and folding one board (or all of them) by whichever transport it uses.

use nostrdb::{Ndb, Transaction};
use nostrdb_net::Pubkey;

use headway::event::{self, BoardCoord, BoardView};
use headway::store;
use headway::teams;

/// The shared boards this account holds keys for, resolved once per run.
///
/// A shared board isn't a different kind of board, it's a different *transport*:
/// its edits are sealed into an SNS channel (kind-1081 envelopes under a team
/// key) instead of published as plaintext nostr events, and
/// [`event::fold_shared_board`] reads back only what was sealed. So every read
/// and every write here has to ask the roster which world a board lives in
/// first. Getting it wrong is silent both ways: folding a shared board the
/// plaintext way omits every co-member's edit, and writing plaintext to one
/// produces events no other front end will ever fold.
///
/// The roster also drives the envelope sync leg ([`sync_envelopes`](crate::sync::sync_envelopes)): a sealed
/// board's edits reach the relay only as kind-1081 envelopes signed by the team
/// keypair, which the account-scoped plaintext reconcile can't see, so the CLI
/// reconciles each channel's envelopes by its team pubkey. This is the one-shot
/// counterpart of the long-lived `relay::sync::Session` envelope pipe
/// (headway:notedeck/clap-matrix-machine): the CLI reconciles once per run and
/// exits rather than holding a live subscription, but both move the same
/// envelopes. It replaced an earlier assumption that the peeled rumors — stored
/// beside their envelopes and matching the plaintext [`event::headway_filter`] —
/// were enough to piggyback a sealed board onto the plaintext leg. They are not:
/// a fresh cache has no envelope to peel until this leg pulls it, and pushing a
/// rumor on the plaintext leg would leak it in the clear (see
/// [`plaintext_sync_filter`](crate::sync::plaintext_sync_filter)).
pub(crate) struct Roster {
    /// The channels *we* (the signer) hold keys for, each naming the board
    /// coordinate its key unlocks. Our membership, not the owner's: a member's
    /// roster lists the owner's boards it was shared, keyed to the owner.
    pub(crate) teams: Vec<teams::Team>,
    /// Whose boards we address — the owner half of a board coordinate
    /// (`--author`, else the signing key). One suffices because a run works one
    /// owner's boards; it differs from the signer when we are a member.
    author: Pubkey,
}

impl Roster {
    /// Derive the roster from nostrdb and register every `team_root` with it.
    ///
    /// Registration is what makes the channel readable *and writable*: our own
    /// sealed edits are ingested as envelopes and only become board events once
    /// nostrdb peels them, so an unregistered root means a write we can't even
    /// read back ourselves.
    ///
    /// `me` is the signer, whose received key-shares make up the roster; `author`
    /// is the board owner whose coordinates [`Self::channel`] and friends look up.
    /// They are the same key on an own board and differ for a member, whose
    /// key-shares are addressed to it but name the owner's board.
    pub(crate) fn load(
        ndb: &Ndb,
        me: &Pubkey,
        author: &Pubkey,
        registry: &mut teams::RootRegistry,
    ) -> Self {
        let teams = teams::teams_from_ndb(ndb, me);
        registry.register(ndb, &teams);
        Self {
            teams,
            author: *author,
        }
    }

    /// The channel new edits to `board_id` seal into — its *primary* channel (see
    /// [`teams::board_channels`]) — or `None` for a private board. Reads gather
    /// every channel instead; see [`Self::channel_pubkeys`].
    pub(crate) fn channel(&self, board_id: &str) -> Option<store::SnsChannel> {
        let addr = event::board_address(&self.author, board_id);
        let team = *teams::board_channels(&self.teams, &addr).first()?;
        Some(store::SnsChannel {
            keys: team.sns_keys()?,
        })
    }

    /// Every channel `board_id`'s content may be sealed into, primary first — what
    /// a read folds the union of, because a board's history can be split across
    /// channels that cannot be merged back together.
    fn channel_pubkeys(&self, board_id: &str) -> Vec<Pubkey> {
        let addr = event::board_address(&self.author, board_id);
        teams::board_channel_pubkeys(&self.teams, &addr)
    }

    /// The same roster, addressing `author`'s boards instead — for when the
    /// owner is only known once the roster itself has been read (see
    /// [`Self::owners_of`]). The channels don't change: they are *our*
    /// membership, whoever's board we then look up in it.
    pub(crate) fn for_author(self, author: Pubkey) -> Self {
        Self { author, ..self }
    }

    /// Every board in the roster that someone *other* than `me` owns — the boards
    /// shared with us — one per coordinate, sorted by slug then owner.
    ///
    /// A board can back several channels (one per key-share generation), so the
    /// coordinates are deduplicated; our own boards are left out because the
    /// author fold in [`list_boards`] already lists them.
    pub(crate) fn foreign_boards(&self, me: &Pubkey) -> Vec<BoardCoord> {
        let mut coords: Vec<BoardCoord> = self
            .teams
            .iter()
            .filter_map(|t| BoardCoord::parse(&t.board_addr))
            .filter(|c| &c.owner != me.bytes())
            .collect();
        coords.sort_by(|a, b| (&a.slug, a.owner).cmp(&(&b.slug, b.owner)));
        coords.dedup();
        coords
    }

    /// The owners of every board shared with `me` under the slug `board_id`.
    ///
    /// A card ref (`headway:<board>/<word-id>`) names a slug but not its owner, so
    /// this is what lets a member say `--board shared` instead of carrying the
    /// owner's hex around: exactly one owner resolves it, several make it
    /// ambiguous. Slugs are only unique per owner, so more than one is possible.
    pub(crate) fn owners_of(&self, me: &Pubkey, board_id: &str) -> Vec<Pubkey> {
        self.foreign_boards(me)
            .into_iter()
            .filter(|c| c.slug == board_id)
            .map(|c| Pubkey::new(c.owner))
            .collect()
    }

    /// The raw `team_root` behind `board_id`'s primary channel — what a re-seal
    /// needs in order to keep sealing a shared board into the channel it already
    /// has, rather than minting a second one (see the `migrate` command).
    pub(crate) fn team_root(&self, board_id: &str) -> Option<[u8; 32]> {
        let addr = event::board_address(&self.author, board_id);
        teams::board_channels(&self.teams, &addr)
            .first()?
            .root_bytes()
    }
}

/// Fold one board, by whichever transport it uses: a joined shared board folds
/// by *coordinate* (gathering every member's sealed edits), a private one folds
/// from `author`'s own plaintext events.
pub(crate) fn load_board(
    ndb: &Ndb,
    roster: &Roster,
    author: &Pubkey,
    board_id: &str,
) -> Option<BoardView> {
    let txn = Transaction::new(ndb).ok()?;
    let channels = roster.channel_pubkeys(board_id);
    if channels.is_empty() {
        return event::load_board(ndb, &txn, author, board_id);
    }
    event::load_shared_board(
        ndb,
        &txn,
        &event::board_address(author, board_id),
        &channels,
    )
}

/// All of `author`'s boards in the cache, sorted by id for a stable listing.
/// Each shared board is re-folded by coordinate so its card counts match what
/// the app draws, rather than the plaintext-only subset the author fold sees.
pub(crate) fn list_boards(ndb: &Ndb, roster: &Roster, author: &Pubkey) -> Vec<BoardView> {
    let Ok(txn) = Transaction::new(ndb) else {
        return Vec::new();
    };
    let mut boards = event::fold_board(ndb, &txn, author)
        .map(|r| r.finalize())
        .unwrap_or_default();
    for board in &mut boards {
        // Only the shared ones: the author fold above already produced every
        // private board, and re-folding those would pay a whole extra walk each.
        if roster.channel_pubkeys(&board.id).is_empty() {
            continue;
        }
        if let Some(shared) = load_board(ndb, roster, author, &board.id) {
            *board = shared;
        }
    }
    boards.sort_by(|a, b| a.id.cmp(&b.id));
    boards
}

/// Every board shared with `me` — owned by someone else, joined through a
/// key-share — folded by coordinate, sorted by slug then owner.
///
/// Folded with the shared fold, gathering every channel of each board, so a
/// board's card count includes every member's edits. A board we hold a key for
/// but whose envelopes haven't arrived yet doesn't fold, and is left out rather
/// than listed empty.
pub(crate) fn list_shared_with_me(ndb: &Ndb, roster: &Roster, me: &Pubkey) -> Vec<BoardView> {
    let Ok(txn) = Transaction::new(ndb) else {
        return Vec::new();
    };
    roster
        .foreign_boards(me)
        .iter()
        .filter_map(|coord| {
            let addr = coord.coordinate();
            let channels = teams::board_channel_pubkeys(&roster.teams, &addr);
            event::load_shared_board(ndb, &txn, &addr, &channels)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A board is shared iff the roster holds a key for its full *coordinate*.
    /// Matching on the slug alone would be wrong in both directions: it would
    /// seal edits to our own private board because a *co-member's* board happens
    /// to share its slug, and — the case that actually bites — it would let a
    /// board we own but never shared fold through the team-key path, which
    /// ingests only sealed rumors and would show it empty.
    #[test]
    fn a_board_is_shared_only_at_its_own_coordinate() {
        let owner = nostrdb_net::FullKeypair::generate().pubkey;
        let other = nostrdb_net::FullKeypair::generate().pubkey;
        let mut root = [0u8; 32];
        root[0] = 0x11;
        root[31] = 0x42;
        let roster = Roster {
            teams: vec![
                // A board we own and shared.
                teams::Team {
                    team_root: hex::encode(root),
                    board_addr: event::board_address(&owner, "shared"),
                    epoch: None,
                    shared_at: 0,
                },
                // Someone else's board that happens to use a slug we also use.
                teams::Team {
                    team_root: hex::encode(root),
                    board_addr: event::board_address(&other, "private"),
                    epoch: None,
                    shared_at: 0,
                },
            ],
            author: owner,
        };

        assert!(
            roster.channel("shared").is_some(),
            "our own shared board must resolve to its channel"
        );
        assert!(
            roster.channel("private").is_none(),
            "a slug match under a different owner is not our board"
        );
        assert!(roster.channel("never-shared").is_none());
    }

    /// The boards shared with us are the roster's *other* owners' boards, one per
    /// coordinate however many channels back it, and a slug names a shared board
    /// only through the owners who shared one under it.
    #[test]
    fn foreign_boards_are_other_owners_boards_once_each() {
        let me = nostrdb_net::FullKeypair::generate().pubkey;
        let alice = nostrdb_net::FullKeypair::generate().pubkey;
        let bob = nostrdb_net::FullKeypair::generate().pubkey;
        let team = |owner: &Pubkey, slug: &str, epoch| teams::Team {
            team_root: hex::encode([0x11; 32]),
            board_addr: event::board_address(owner, slug),
            epoch,
            shared_at: 0,
        };
        let roster = Roster {
            teams: vec![
                team(&me, "shared", None),
                team(&alice, "shared", None),
                // A second channel of the same board (a later key generation).
                team(&alice, "shared", Some(1)),
                team(&bob, "shared", None),
                team(&alice, "roadmap", None),
            ],
            author: me,
        };

        let foreign: Vec<(String, [u8; 32])> = roster
            .foreign_boards(&me)
            .into_iter()
            .map(|c| (c.slug, c.owner))
            .collect();
        assert_eq!(foreign.len(), 3, "{foreign:?}");
        assert!(foreign.iter().all(|(_, owner)| owner != me.bytes()));
        assert_eq!(foreign[0].0, "roadmap", "sorted by slug");

        let mut owners = roster.owners_of(&me, "shared");
        owners.sort();
        let mut expected = vec![alice, bob];
        expected.sort();
        assert_eq!(owners, expected);
        assert_eq!(roster.owners_of(&me, "roadmap"), vec![alice]);
        assert!(roster.owners_of(&me, "nope").is_empty());
    }
}
