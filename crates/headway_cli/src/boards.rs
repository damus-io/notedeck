//! Board loading: the [`Roster`] of shared boards this account holds keys for,
//! and folding one board (or all of them) by whichever transport it uses.

use nostrdb::{Ndb, Transaction};
use nostrdb_net::Pubkey;

use headway::event::{self, BoardView};
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
    /// Joined channels, each naming the board coordinate its key unlocks.
    pub(crate) teams: Vec<teams::Team>,
    /// Whose boards we address — the owner half of a board coordinate. The CLI
    /// is single-author (`--author`, else the signing key), so one suffices.
    author: Pubkey,
}

impl Roster {
    /// Derive the roster from nostrdb and register every `team_root` with it.
    ///
    /// Registration is what makes the channel readable *and writable*: our own
    /// sealed edits are ingested as envelopes and only become board events once
    /// nostrdb peels them, so an unregistered root means a write we can't even
    /// read back ourselves.
    pub(crate) fn load(ndb: &Ndb, author: &Pubkey, registry: &mut teams::RootRegistry) -> Self {
        let teams = teams::teams_from_ndb(ndb, author);
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
}
