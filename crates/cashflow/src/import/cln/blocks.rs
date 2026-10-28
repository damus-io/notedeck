//! Block times for the `cln` importer's on-chain records, from the node's
//! `corerpc` plugin.
//!
//! Bookkeeper stamps an event with when it logged it, which for coins it
//! backfilled can be years after they confirmed, and carries only the
//! block's height. jb55/cln-core-rpc's `corerpc <wallet> <method> [args…]`
//! passes a call through to bitcoind, so the height's time is two calls
//! over the same commando connection: `getblockhash`, then
//! `getblockheader`. Each batch is one [`Node::call_each`], one call at a
//! time ([`IN_FLIGHT`]).
//!
//! corerpc hands bitcoind's errors back as a *successful* result (`{"code":
//! -8}` for a height past the tip), so every answer is checked for its
//! shape: a hash is 64 hex, a header an object whose `height` is the one
//! asked about and whose `time` is a number. Anything else leaves that
//! height without a time, and its records are dated by bookkeeper's
//! timestamp.
//!
//! **Held times come first.** A record the journal holds carries its
//! `raw-blockheight` and `raw-blocktime`, and a height a held record has a
//! time for is never asked about again. So an incremental run asks only
//! about new heights, and a machine whose rune can't reach corerpc keeps a
//! record's block date rather than reverting it.

use std::collections::{BTreeSet, HashMap};

use ledgerdb::Ledger;
use serde_json::{Value, json};

use super::{Node, NodeError};

/// The plugin's method.
const CORERPC: &str = "corerpc";

/// The bitcoind wallet corerpc's first parameter names. Neither call reads
/// a wallet, so the default one does.
const WALLET: &str = "";

/// The metadata key a record carries its block height under, `-` when it
/// is unknown.
pub(super) const HEIGHT_META: &str = "raw-blockheight";

/// The metadata key a record carries its block's time under, when it has
/// one.
pub(super) const TIME_META: &str = "raw-blocktime";

/// How many corerpc calls wait on the node at once: one. clightningjs, the
/// library the plugin is built on, reads every request into one `msg` and
/// answers each after its `await` under `msg.id`, which by then is the
/// newest request's, so of calls made side by side all but one are never
/// answered, and a batch would wait out the call timeout
/// (headway:cashflow/avoid-build-trick).
const IN_FLIGHT: usize = 1;

/// What a record's `raw-blockheight` says when its height is unknown.
pub(super) const UNKNOWN: &str = "-";

/// The unix time of each block height a run knows.
#[derive(Default)]
pub(super) struct BlockTimes {
    times: HashMap<u32, u64>,
}

impl BlockTimes {
    /// The times `held`'s records from `source` carry. Only that source's,
    /// since a node on another network numbers its blocks alike.
    pub(super) fn held(held: &Ledger, source: &str) -> Self {
        let journal = held.journal();
        let times = super::sourced(held, source)
            .filter_map(|txn| {
                let height = journal.txn_meta(txn, HEIGHT_META)?.parse().ok()?;
                let time = journal.txn_meta(txn, TIME_META)?.parse().ok()?;
                Some((height, time))
            })
            .collect();
        Self { times }
    }

    /// The time of block `height`, if known.
    pub(super) fn time(&self, height: u32) -> Option<u64> {
        self.times.get(&height).copied()
    }

    /// Ask `node`'s corerpc for each of `heights` not already known. A
    /// height that gets no time keeps none, and the note says how many and
    /// why. With no node (saved listings), nothing is asked.
    pub(super) fn fetch(
        &mut self,
        node: Option<&dyn Node>,
        heights: impl IntoIterator<Item = u32>,
    ) -> Option<String> {
        let missing: Vec<u32> = heights
            .into_iter()
            .filter(|height| !self.times.contains_key(height))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if missing.is_empty() {
            return None;
        }
        let why = match node {
            Some(node) => match self.ask(node, &missing) {
                Ok(()) => return None,
                Err(why) => why,
            },
            None => "reading saved listings, with no node to ask".to_owned(),
        };
        let unknown = missing
            .iter()
            .filter(|h| !self.times.contains_key(h))
            .count();
        Some(format!(
            "{unknown} of {} block heights have no time, so their on-chain records are dated \
             by when bookkeeper logged them ({why})",
            missing.len()
        ))
    }

    /// Ask for `heights`' hashes and then those hashes' headers, keeping
    /// each time that checks out. `Err` names the first failure when any
    /// height got no time.
    fn ask(&mut self, node: &dyn Node, heights: &[u32]) -> Result<(), String> {
        let mut failure: Option<String> = None;
        let mut fail = |why: String| {
            failure.get_or_insert(why);
        };

        let params = heights
            .iter()
            .map(|&height| json!([WALLET, "getblockhash", height]))
            .collect();
        let hashes = node
            .call_each(CORERPC, params, IN_FLIGHT)
            .map_err(|error| format!("{CORERPC}: {error}"))?;
        let mut found = Vec::new();
        for (&height, answer) in heights.iter().zip(hashes) {
            match answer.map_err(rpc_failure).and_then(block_hash) {
                Ok(hash) => found.push(Hashed { height, hash }),
                Err(why) => fail(format!("{CORERPC} getblockhash: {why}")),
            }
        }

        let params = found
            .iter()
            .map(|hashed| json!([WALLET, "getblockheader", hashed.hash]))
            .collect();
        let headers = if found.is_empty() {
            Vec::new()
        } else {
            node.call_each(CORERPC, params, IN_FLIGHT)
                .map_err(|error| format!("{CORERPC}: {error}"))?
        };
        for (hashed, answer) in found.iter().zip(headers) {
            match answer
                .map_err(rpc_failure)
                .and_then(|header| block_time(&header, hashed.height))
            {
                Ok(time) => {
                    self.times.insert(hashed.height, time);
                }
                Err(why) => fail(format!("{CORERPC} getblockheader: {why}")),
            }
        }
        match failure {
            Some(why) => Err(why),
            None => Ok(()),
        }
    }
}

/// Whether `node`'s corerpc gives a block hash, asking for the genesis
/// block's, which every chain has. `Err` says why not.
pub(super) fn probe(node: &dyn Node) -> Result<(), String> {
    node.call(CORERPC, json!([WALLET, "getblockhash", 0]))
        .map_err(rpc_failure)
        .and_then(block_hash)
        .map(drop)
        .map_err(|why| format!("{CORERPC} getblockhash: {why}"))
}

/// A height and the hash corerpc gave for it.
struct Hashed {
    height: u32,
    hash: String,
}

/// A call's own error, as text.
fn rpc_failure(error: NodeError) -> String {
    error.to_string()
}

/// A `getblockhash` answer: 64 hex, or why it isn't one.
fn block_hash(answer: Value) -> Result<String, String> {
    match answer {
        Value::String(hash) if hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => {
            Ok(hash)
        }
        other => Err(format!("answered {other}")),
    }
}

/// A `getblockheader` answer's time, when it is the header of `height`.
fn block_time(header: &Value, height: u32) -> Result<u64, String> {
    let matches = header.get("height").and_then(Value::as_u64) == Some(u64::from(height));
    match header.get("time").and_then(Value::as_u64) {
        Some(time) if matches => Ok(time),
        _ => Err(format!("answered {header}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_checked_for_their_shape() {
        let hash = "ab".repeat(32);
        assert_eq!(block_hash(json!(hash)), Ok(hash));
        for bad in [json!({ "code": -8 }), json!("abc"), json!("zz".repeat(32))] {
            assert!(block_hash(bad).is_err());
        }

        let header = json!({ "height": 7, "time": 1_500_000_000 });
        assert_eq!(block_time(&header, 7), Ok(1_500_000_000));
        assert!(block_time(&header, 8).is_err(), "another block's header");
        assert!(block_time(&json!({ "code": -5 }), 7).is_err());
        assert!(block_time(&json!({ "height": 7, "time": "noon" }), 7).is_err());
    }
}
