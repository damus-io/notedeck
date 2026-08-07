//! Shared-board membership: the SNS `team_root`s the account has joined.
//!
//! Possession of a `team_root` is membership in a shared board's channel (see
//! `docs/nip-sns-sealed-shared-storage.md`). nostrdb is only the *mechanism* —
//! register a root with [`Ndb::add_team_root`] and it auto-unwraps that channel's
//! kind-1081 envelopes — while *joining* (which roots to register, persisting
//! them, and re-registering on boot) is app **policy**, which lives here.
//!
//! Roots are persisted per account (registered keys don't survive a restart, so
//! the app re-registers them each boot, mirroring `add_key` for account keys) and
//! accepted from incoming kind-1082 key-shares. A new member is added by
//! gift-wrapping them a `1082`; nostrdb unwraps the `1059` to the `1082` rumor and
//! the app reads it with [`enostr::sns::parse_keyshare`], then — per the current
//! auto-accept policy — registers and persists the root.

use std::collections::HashMap;
use std::path::PathBuf;

use enostr::Pubkey;
use enostr::sns::KeyShare;
use nostrdb::{Ndb, Transaction};
use notedeck::{DataPath, DataPathType};
use serde::{Deserialize, Serialize};

/// One shared board the account has joined: the channel secret plus which board
/// coordinate it unlocks. Persisted in `headway-teams.json` and re-registered
/// with nostrdb on boot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Team {
    /// Hex of the 32-byte `team_root` secret — the shared channel key every
    /// member holds.
    pub team_root: String,
    /// The board coordinate this root unlocks: `30619:<owner-hex>:<board-id>`.
    pub board_addr: String,
    /// Rotation generation, if the key-share named one (see *Rotation* in the SNS
    /// doc). `None` for a first-generation share.
    #[serde(default)]
    pub epoch: Option<u32>,
}

impl Team {
    /// Decode [`team_root`](Self::team_root) into the raw 32-byte secret, or
    /// `None` if it isn't 32 bytes of hex.
    pub fn root_bytes(&self) -> Option<[u8; 32]> {
        let bytes = hex::decode(&self.team_root).ok()?;
        <[u8; 32]>::try_from(bytes.as_slice()).ok()
    }
}

/// The file holding each account's joined shared boards: a JSON map of pubkey-hex
/// → the account's [`Team`]s, under the app's settings dir. One file for all
/// accounts, mirroring `headway-boards.json`.
fn teams_path(path: &DataPath) -> PathBuf {
    path.path(DataPathType::Setting).join("headway-teams.json")
}

/// The shared boards `author` has joined, if any were saved.
pub fn load_teams(path: &DataPath, author: &Pubkey) -> Vec<Team> {
    let Ok(data) = std::fs::read_to_string(teams_path(path)) else {
        return Vec::new();
    };
    let map: HashMap<String, Vec<Team>> = serde_json::from_str(&data).unwrap_or_default();
    map.get(&author.hex()).cloned().unwrap_or_default()
}

/// Persist `author`'s joined boards, merging into the existing map so other
/// accounts' memberships are preserved. Best-effort: a write failure is non-fatal.
fn save_teams(path: &DataPath, author: &Pubkey, teams: &[Team]) {
    let file = teams_path(path);
    let mut map: HashMap<String, Vec<Team>> = std::fs::read_to_string(&file)
        .ok()
        .and_then(|d| serde_json::from_str(&d).ok())
        .unwrap_or_default();
    map.insert(author.hex(), teams.to_vec());
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(&map) {
        let _ = std::fs::write(&file, json);
    }
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
    if registered {
        if let Ok(txn) = Transaction::new(ndb) {
            ndb.process_sns(&txn);
        }
    }
}

/// Accept an incoming kind-1082 key-share under the current policy, returning the
/// [`Team`] if it was newly joined (`None` if it names no board, is already
/// joined, or its root is unusable).
///
/// **Policy.** This auto-accepts: any `1082` nostrdb unwrapped for us (it was
/// gift-wrapped to our key) registers and persists its root with no prompt. The
/// SNS doc lists auto-accept as a valid app policy, and there is no join UI yet.
/// A future update will gate acceptance on a user prompt ("join shared board from
/// <sharer>?"); that decision belongs exactly here, before [`Ndb::add_team_root`]
/// and [`save_teams`] — everything downstream (registration, persistence,
/// re-register-on-boot) is unchanged whether the accept was automatic or
/// confirmed.
pub fn accept_keyshare(
    path: &DataPath,
    author: &Pubkey,
    ndb: &Ndb,
    share: &KeyShare,
) -> Option<Team> {
    // A share must name the board its root unlocks, or we can't fold it later.
    let board_addr = share.board_addr.clone()?;
    let team = Team {
        team_root: hex::encode(share.team_root),
        board_addr,
        epoch: share.epoch,
    };

    let mut teams = load_teams(path, author);
    if teams
        .iter()
        .any(|t| t.team_root == team.team_root && t.board_addr == team.board_addr)
    {
        return None; // already joined
    }

    // (Future accept-policy prompt gates here — see the doc comment.)
    ndb.add_team_root(&share.team_root);
    if let Ok(txn) = Transaction::new(ndb) {
        ndb.process_sns(&txn);
    }
    teams.push(team.clone());
    save_teams(path, author, &teams);
    Some(team)
}

#[cfg(test)]
mod tests {
    use super::*;
    use enostr::FullKeypair;
    use enostr::sns::KeyShare;
    use nostrdb::Config;

    fn tmp_datapath() -> (tempfile::TempDir, DataPath) {
        let dir = tempfile::TempDir::new().unwrap();
        let path = DataPath::new(dir.path());
        (dir, path)
    }

    fn test_root() -> [u8; 32] {
        let mut root = [0u8; 32];
        root[0] = 0x11;
        root[31] = 0x22;
        root
    }

    #[test]
    fn accept_persists_and_dedupes() {
        let (_dir, path) = tmp_datapath();
        let ndb_dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(ndb_dir.path().to_str().unwrap(), &Config::new()).unwrap();
        let author = FullKeypair::generate().pubkey;

        let share = KeyShare {
            team_root: test_root(),
            board_addr: Some("30619:owner:headway".to_string()),
            epoch: Some(1),
        };

        // First accept joins and persists.
        let joined = accept_keyshare(&path, &author, &ndb, &share).expect("joined");
        assert_eq!(joined.board_addr, "30619:owner:headway");
        assert_eq!(joined.root_bytes(), Some(test_root()));
        let teams = load_teams(&path, &author);
        assert_eq!(teams.len(), 1);
        assert_eq!(teams[0], joined);

        // Re-accepting the same share is a no-op (already joined).
        assert!(accept_keyshare(&path, &author, &ndb, &share).is_none());
        assert_eq!(load_teams(&path, &author).len(), 1);
    }

    #[test]
    fn accept_requires_a_board() {
        let (_dir, path) = tmp_datapath();
        let ndb_dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(ndb_dir.path().to_str().unwrap(), &Config::new()).unwrap();
        let author = FullKeypair::generate().pubkey;

        // A share that names no board can't be folded later, so it isn't joined.
        let share = KeyShare {
            team_root: test_root(),
            board_addr: None,
            epoch: None,
        };
        assert!(accept_keyshare(&path, &author, &ndb, &share).is_none());
        assert!(load_teams(&path, &author).is_empty());
    }

    #[test]
    fn teams_are_per_account() {
        let (_dir, path) = tmp_datapath();
        let ndb_dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(ndb_dir.path().to_str().unwrap(), &Config::new()).unwrap();
        let alice = FullKeypair::generate().pubkey;
        let bob = FullKeypair::generate().pubkey;

        let share = KeyShare {
            team_root: test_root(),
            board_addr: Some("30619:owner:headway".to_string()),
            epoch: None,
        };
        accept_keyshare(&path, &alice, &ndb, &share).expect("alice joins");

        // Bob's membership list is independent and empty.
        assert_eq!(load_teams(&path, &alice).len(), 1);
        assert!(load_teams(&path, &bob).is_empty());
    }
}
