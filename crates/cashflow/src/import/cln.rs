//! The `cln` importer: Core Lightning bookkeeper events, from
//! `bkpr-listaccountevents`.
//!
//! With no paths it asks the node over commando (a lightning peer
//! connection, so no RPC socket or HTTP server has to be exposed), with the
//! node id and host [`ClnConfig`] names and the `cln-rune` secret. Given
//! paths it reads saved listings instead, each the JSON `lightning-cli
//! bkpr-listaccountevents > events.json` prints, which is also the way to
//! import on the node itself.
//!
//! **It reads only what the journal lacks.** A bookkeeper patched to take
//! `start_time` is asked for the events after the newest payment the journal
//! already holds from this source, less [`OVERLAP_SECS`]: `start_time` is
//! exclusive and many events share a second, so the overlap is re-read and
//! [`super::write`] drops it as unchanged. The cursor is read from the
//! loaded journal ([`cursor`]), not kept anywhere else, so a second machine
//! the journal syncs to resumes where the first left off. A node that
//! doesn't take `start_time` answers with `-32602 unknown parameter`, and is
//! then read whole, which the run's notes say.
//!
//! An event whose description was edited later (`bkpr-editdescriptionby*`)
//! is not read again once it is older than the cursor, so the journal keeps
//! the description it was imported with.
//!
//! **Two layers.** Payments (`invoice` and `routed` events) go in the `cln`
//! layer, as `cln.ledger` had them. Every other event (deposits,
//! withdrawals, channel opens and closes, on-chain fees, journal entries, …)
//! goes in `cln-chain`, which a reader can switch off as a whole (`--except
//! cln-chain`), as the dashboard is to by default: it is bookkeeper's own
//! view of the node's on-chain side, and overlaps the `btc` importer's where
//! a wallet of one paid the other. An event of zero msat each way records
//! nothing and is passed over.
//!
//! **Record ids.** A patched bookkeeper prints each chain and channel event's
//! `created_index`, lightningd's own move index, unique per type, and each
//! on-chain fee's `update_count`; those make `cln:chain:<n>`,
//! `cln:channel:<n>` and `cln:fee:<account>:<txid>:<count>`. An unpatched one
//! prints no id at all, and its fields don't single an event out either
//! (the two legs of a routed payment share a `payment_id`, and the same
//! invoice can be paid twice over one channel in a second), so an event is
//! then `cln:h:<hash>`, the first 16 hex of a sha256 over every field but its
//! editable description, with `:2`, `:3`, … for identical events. The
//! timestamp is in the hash, so identical events fall in one second, which a
//! read from a cursor takes whole. An event the journal already holds under
//! that id keeps it once the node is patched, so switching costs no
//! duplicates.
//!
//! **Raw facts, structural defaults.** Each payment posts its msat to
//! `assets:cln` against a counter leg:
//!
//! - `routed`: `routed:in` or `routed:out`, payee `Routed`.
//! - a zap (the description is a kind-9734 zap request): `income:zap`, with
//!   the recipient (`p`), the zapped event (`e`) and the sender as
//!   `raw-zap-p`, `raw-zap-e` and `raw-zap-sender`. Whose zap it was is a
//!   rule on `raw-zap-p`.
//! - an LNURL payment (the description is LNURL metadata): `income:lnurl`,
//!   with `raw-lnurl-address` and `raw-lnurl-text`.
//! - anything else: `income:unknown` or `expenses:unknown` by sign, with the
//!   description as payee and `raw-description`.
//!
//! An outgoing zap or LNURL payment goes to `expenses:unknown`. Every record
//! also carries `raw-cln-tag`, `raw-timestamp`, `raw-direction` (`credit` or
//! `debit`, for a rule on one side only) and, when the event has them,
//! `raw-payment-id`, `raw-fees-msat` and `raw-rebalance`.
//!
//! A `cln-chain` event posts to its bookkeeper account (`assets:cln:wallet`
//! for the on-chain wallet, `assets:cln` for a channel) against
//! `income:unknown` or `expenses:unknown`, with `raw-account`,
//! `raw-outpoint`, `raw-txid` and `raw-origin`. Two kinds don't move the
//! owner's money and post against `equity:cln` instead: an event on
//! bookkeeper's `external` account (`equity:cln:external`), the far side of
//! a move whose near side is a wallet or channel event, and an on-chain fee
//! (`equity:cln:fees`), which the wallet and channel events already net.
//!
//! **Dates** are the event's day in the synced
//! [`Setting::Timezone`](crate::settings::Setting::Timezone), so every
//! machine files an event under the same day and a second machine's import
//! rewrites nothing. [`ClnConfig::timezone`] overrides it on one machine.
//! With neither set it is the system's own zone, as `index.ts` dated them,
//! and the run says so.

use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use ledgerdb::{LayerFilter, Ledger};
use lnsocket::bitcoin::secp256k1::{PublicKey, SecretKey, rand};
use lnsocket::commando::{CommandoConfig, RetryPolicy};
use lnsocket::{CommandoClient, LNSocket};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::csv_rows::read;
use super::{
    Batch, ImportError, ImportInput, Importer, OtherLayer, RawAmount, RawMeta, RawPosting, RawTxn,
    Skipped, Synced,
};
use crate::CashflowDir;
use crate::config::{ClnConfig, Secret, secret_env_var};
use crate::prices::MSAT;
use crate::zone::Zone;

/// The secret holding the rune commando calls are made with.
pub const RUNE_SECRET: &str = "cln-rune";

/// The layer payments go in.
pub const LAYER: &str = "cln";

/// The layer every other event goes in.
pub const CHAIN_LAYER: &str = "cln-chain";

/// How far before the newest payment it holds a run starts reading again.
/// Events share whole seconds, and a routed payment's two legs can be a
/// second apart, so a day of overlap is cheap insurance; the rows it reads
/// again are dropped as unchanged.
pub const OVERLAP_SECS: u64 = 86_400;

/// The bookkeeper call.
const METHOD: &str = "bkpr-listaccountevents";

/// The port a host without one is reached on.
const DEFAULT_PORT: u16 = 9735;

/// How long one call may take. A whole unpatched listing is hundreds of MB.
const CALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// JSON-RPC's code for a parameter the method doesn't take.
const UNKNOWN_PARAMETER: i64 = -32602;

/// The node's channels, where payments land.
const CHANNELS: &str = "assets:cln";
/// The node's on-chain wallet.
const WALLET: &str = "assets:cln:wallet";
/// Bookkeeper's `external` account.
const EXTERNAL: &str = "equity:cln:external";
/// On-chain fees, as bookkeeper accounts them.
const FEES: &str = "equity:cln:fees";
/// What the `external` and fee records post against.
const MIRROR: &str = "equity:cln";
const ROUTED_IN: &str = "routed:in";
const ROUTED_OUT: &str = "routed:out";
const SPENT: &str = "expenses:unknown";
const RECEIVED: &str = "income:unknown";
const ZAPS: &str = "income:zap";
const LNURL: &str = "income:lnurl";

/// The nostr kind of a zap request.
const ZAP_REQUEST: u64 = 9734;

/// A Core Lightning node to call, so the importer can be tested without
/// one.
pub trait Node {
    /// Call `method` with named `params` and return its result.
    fn call(&self, method: &str, params: Value) -> Result<Value, NodeError>;
}

/// Why a [`Node`] call failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NodeError {
    /// The node answered with a JSON-RPC error.
    #[error("{message} ({code})")]
    Rpc { code: i64, message: String },
    /// The node could not be reached or its answer not read.
    #[error("{0}")]
    Other(String),
}

/// [`Node`] over commando: each call connects, makes the one call and
/// hangs up.
pub struct Commando {
    node_id: PublicKey,
    /// `host:port`.
    host: String,
    rune: Secret,
}

impl Commando {
    /// The node `node_id` (hex) at `host` (`host[:port]`), called with
    /// `rune`.
    pub fn new(node_id: &str, host: &str, rune: Secret) -> Result<Self, String> {
        let node_id =
            PublicKey::from_str(node_id).map_err(|e| format!("node id {node_id:?}: {e}"))?;
        let host = if host.contains(':') {
            host.to_owned()
        } else {
            format!("{host}:{DEFAULT_PORT}")
        };
        Ok(Self {
            node_id,
            host,
            rune,
        })
    }

    /// Make the call on a runtime of its own, which a thread of its own
    /// keeps clear of any runtime the caller is on.
    fn call_on_own_runtime(&self, method: &str, params: Value) -> Result<Value, NodeError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| NodeError::Other(format!("starting a runtime: {e}")))?;
        runtime.block_on(async {
            let key = SecretKey::new(&mut rand::thread_rng());
            let socket = LNSocket::connect_and_init(key, self.node_id, &self.host)
                .await
                .map_err(|e| NodeError::Other(format!("connecting to {}: {e}", self.host)))?;
            let config = CommandoConfig::new()
                .timeout(Some(CALL_TIMEOUT))
                .retry_policy(RetryPolicy::Never)
                .no_reconnect();
            let client = CommandoClient::spawn_with_config(socket, self.rune.expose(), config);
            client.call(method, params).await.map_err(|e| match e {
                lnsocket::Error::Rpc(rpc) => NodeError::Rpc {
                    code: rpc.code,
                    message: rpc.message,
                },
                other => NodeError::Other(other.to_string()),
            })
        })
    }
}

impl Node for Commando {
    fn call(&self, method: &str, params: Value) -> Result<Value, NodeError> {
        std::thread::scope(|scope| {
            scope
                .spawn(|| self.call_on_own_runtime(method, params))
                .join()
                .unwrap_or_else(|_| Err(NodeError::Other("the commando call panicked".to_owned())))
        })
    }
}

/// The zone an import dates events in, and a note for the user when it
/// isn't the one every machine shares.
struct Dating {
    zone: Zone,
    note: Option<String>,
}

/// Reads Core Lightning bookkeeper events, from the node or saved listings.
pub struct ClnImporter {
    config: ClnConfig,
    /// The synced timezone setting, when the run read the vault.
    synced_timezone: Option<String>,
    /// Where the rune is read from, when the import runs.
    dir: CashflowDir,
    /// The node to call instead of connecting with `config`, for tests.
    node: Option<Box<dyn Node>>,
}

impl ClnImporter {
    /// The importer as `config.toml`'s `[cln]` table sets it up, reading its
    /// rune from `dir` when it runs.
    pub fn new(config: Option<&ClnConfig>, dir: &CashflowDir) -> Self {
        Self {
            config: config.cloned().unwrap_or_default(),
            synced_timezone: None,
            dir: dir.clone(),
            node: None,
        }
    }

    /// The importer calling `node` rather than connecting.
    pub fn with_node(config: Option<&ClnConfig>, node: Box<dyn Node>) -> Self {
        Self {
            node: Some(node),
            ..Self::new(config, &CashflowDir::new(""))
        }
    }

    /// The importer dating events in `synced`'s timezone, unless
    /// `[cln] timezone` overrides it.
    pub fn synced(self, synced: &Synced) -> Self {
        Self {
            synced_timezone: synced.timezone.clone(),
            ..self
        }
    }

    /// The zone events are dated in: `[cln] timezone`, then the synced
    /// setting, then the system's own. Either fallback the user may not
    /// expect comes with a note saying so.
    fn dating(&self) -> Result<Dating, ImportError> {
        let synced = self.synced_timezone.as_deref();
        if let Some(local) = self.config.timezone.as_deref() {
            let zone = Zone::parse(local)
                .map_err(|e| ImportError::Usage(format!("[cln] timezone: {e}")))?;
            let note = synced
                .filter(|s| Zone::parse(s).ok() != Some(zone))
                .map(|s| {
                    format!(
                        "dating events in [cln] timezone {local}, not the synced timezone {s}: \
                         another machine dating in {s} rewrites events near midnight"
                    )
                });
            return Ok(Dating { zone, note });
        }
        if let Some(synced) = synced {
            let zone = Zone::parse(synced)
                .map_err(|e| ImportError::Usage(format!("the timezone setting: {e}")))?;
            return Ok(Dating { zone, note: None });
        }
        Ok(Dating {
            zone: Zone::Local,
            note: Some(
                "dating events in this machine's time zone: set one every machine shares \
                 (`cashflow settings set timezone America/Vancouver`), or two machines \
                 importing rewrite each other's events near midnight"
                    .to_owned(),
            ),
        })
    }

    /// The node the config names, called with the rune secret.
    fn connect(&self) -> Result<Commando, ImportError> {
        let need = |key: &str, example: &str| {
            ImportError::Usage(format!(
                "no `{key}` under [cln] in config.toml: run `cashflow config set cln.{key} \
                 {example}` to fetch from the node (or give `bkpr-listaccountevents` output \
                 files)"
            ))
        };
        let node_id = self
            .config
            .node_id
            .as_deref()
            .ok_or_else(|| need("node_id", "<66-hex node id>"))?;
        let host = self
            .config
            .host
            .as_deref()
            .ok_or_else(|| need("host", "<host[:port]>"))?;
        let rune = self
            .dir
            .secret(RUNE_SECRET)
            .map_err(|e| ImportError::Usage(e.to_string()))?
            .ok_or_else(|| {
                ImportError::Usage(format!(
                    "no rune: mint one on the node with lightning-cli createrune \
                     restrictions='[[\"method={METHOD}\"]]', then store it with `cashflow \
                     secret set {RUNE_SECRET}` (or set ${})",
                    secret_env_var(RUNE_SECRET),
                ))
            })?;
        Commando::new(node_id, host, rune).map_err(ImportError::Usage)
    }
}

impl Importer for ClnImporter {
    fn name(&self) -> &'static str {
        "cln"
    }

    fn layer(&self) -> &'static str {
        LAYER
    }

    fn import(&self, input: &ImportInput) -> Result<Batch, ImportError> {
        self.import_held(input, &Ledger::new(LayerFilter::All))
    }

    fn import_held(&self, input: &ImportInput, held: &Ledger) -> Result<Batch, ImportError> {
        let Dating { zone, note } = self.dating()?;
        let mut notes: Vec<String> = note.into_iter().collect();
        let listings = if input.paths.is_empty() {
            let connected;
            let node: &dyn Node = match &self.node {
                Some(node) => &**node,
                None => {
                    connected = self.connect()?;
                    &connected
                }
            };
            let Fetched { listing, note } = fetch(node, cursor(held, &input.source))?;
            notes.extend(note);
            vec![read_value(listing)?]
        } else {
            input
                .paths
                .iter()
                .map(|path| read_file(path))
                .collect::<Result<_, _>>()?
        };

        let convert = Convert {
            source: &input.source,
            zone,
            held,
        };
        let mut payments = Vec::new();
        let mut chain = Vec::new();
        let mut skipped = Vec::new();
        for listing in listings {
            skipped.extend(listing.skipped);
            let mut seen: HashMap<String, u32> = HashMap::new();
            for event in &listing.events {
                if event.credit_msat == 0 && event.debit_msat == 0 {
                    continue;
                }
                let hash = convert.hash_id(event);
                let nth = seen.entry(hash.clone()).or_default();
                *nth += 1;
                let fallback = match *nth {
                    1 => hash,
                    n => format!("{hash}:{n}"),
                };
                let id = convert.record_id(event, fallback);
                if event.is_payment() {
                    payments.push(convert.payment(event, id));
                } else {
                    chain.push(convert.chain(event, id));
                }
            }
        }
        let chain = Batch::from_raw(&chain, &[])?.journal;
        Ok(Batch {
            skipped,
            notes,
            other_layers: vec![OtherLayer {
                layer: CHAIN_LAYER.to_owned(),
                journal: chain,
            }],
            ..Batch::from_raw(&payments, &[])?
        })
    }
}

/// What [`fetch`] read.
struct Fetched {
    /// The `bkpr-listaccountevents` result.
    listing: Value,
    /// Why it read more than it was asked to, if it did.
    note: Option<String>,
}

/// Ask `node` for the events after `cursor`, or every event when there is
/// no cursor or the node can't take one.
fn fetch(node: &dyn Node, cursor: Option<u64>) -> Result<Fetched, ImportError> {
    let call = |params: Value| {
        node.call(METHOD, params)
            .map_err(|error| ImportError::Fetch {
                from: METHOD.to_owned(),
                error: error.to_string(),
            })
    };
    let Some(start) = cursor else {
        return Ok(Fetched {
            listing: call(json!({}))?,
            note: None,
        });
    };
    match node.call(METHOD, json!({ "start_time": start })) {
        Ok(listing) => Ok(Fetched {
            listing,
            note: None,
        }),
        Err(NodeError::Rpc { code, message })
            if code == UNKNOWN_PARAMETER && message.contains("start_time") =>
        {
            Ok(Fetched {
                listing: call(json!({}))?,
                note: Some(format!(
                    "the node's bookkeeper doesn't take start_time, so every event was read \
                     and only the new ones kept ({METHOD}: {message})"
                )),
            })
        }
        Err(error) => Err(ImportError::Fetch {
            from: METHOD.to_owned(),
            error: error.to_string(),
        }),
    }
}

/// Where a run from `source` starts reading: [`OVERLAP_SECS`] before the
/// newest payment `held` has from it, or `None` when it has none.
///
/// Only payments count: they are written last (see [`super::write`]), so a
/// run that wrote its `cln-chain` segment but failed on its payments resumes
/// from before both.
fn cursor(held: &Ledger, source: &str) -> Option<u64> {
    let journal = held.journal();
    let prefix = format!("{source}:");
    held.txns()
        .iter()
        .filter(|txn| {
            txn.id
                .is_some_and(|id| journal.strings.get(id).starts_with(&prefix))
        })
        .filter(|txn| {
            journal
                .txn_meta(txn, "raw-cln-tag")
                .is_some_and(is_payment_tag)
        })
        .filter_map(|txn| journal.txn_meta(txn, "raw-timestamp")?.parse::<u64>().ok())
        .max()
        .map(|newest| newest.saturating_sub(OVERLAP_SECS))
}

fn is_payment_tag(tag: &str) -> bool {
    tag == "invoice" || tag == "routed"
}

/// The events of one listing, and the ones that didn't read.
struct Listing {
    events: Vec<Event>,
    skipped: Vec<Skipped>,
}

/// A saved listing.
fn read_file(path: &Path) -> Result<Listing, ImportError> {
    #[derive(Deserialize)]
    struct Saved<'a> {
        #[serde(borrow)]
        events: Vec<&'a RawValue>,
    }
    let text = read(path)?;
    let saved: Saved = serde_json::from_str(&text).map_err(|e| ImportError::Fetch {
        from: path.display().to_string(),
        error: format!("not {METHOD} output: {e}"),
    })?;
    Ok(read_events(
        saved
            .events
            .into_iter()
            .map(|raw| serde_json::from_str::<Event>(raw.get())),
    ))
}

/// A listing the node answered with.
fn read_value(mut listing: Value) -> Result<Listing, ImportError> {
    let Value::Array(events) = listing["events"].take() else {
        return Err(ImportError::Fetch {
            from: METHOD.to_owned(),
            error: "the answer has no `events` list".to_owned(),
        });
    };
    Ok(read_events(events.into_iter().map(serde_json::from_value)))
}

/// Read each event, keeping the ones that don't read as [`Skipped`] at their
/// place in the listing, counting from 1.
fn read_events(events: impl Iterator<Item = Result<Event, serde_json::Error>>) -> Listing {
    let mut listing = Listing {
        events: Vec::new(),
        skipped: Vec::new(),
    };
    for (at, event) in events.enumerate() {
        match event {
            Ok(event) => listing.events.push(event),
            Err(error) => listing.skipped.push(Skipped {
                line: at as u32 + 1,
                col: 1,
                message: format!("event {}: {error}", at + 1),
            }),
        }
    }
    listing
}

/// One bookkeeper event, as `bkpr-listaccountevents` prints it.
#[derive(Clone, Debug, Deserialize)]
struct Event {
    /// A channel id, `wallet` or `external`.
    account: String,
    /// `chain`, `channel` or `onchain_fee`.
    #[serde(rename = "type")]
    kind: String,
    tag: String,
    #[serde(deserialize_with = "msat")]
    credit_msat: u64,
    #[serde(deserialize_with = "msat")]
    debit_msat: u64,
    timestamp: u64,
    description: Option<String>,
    outpoint: Option<String>,
    txid: Option<String>,
    origin: Option<String>,
    payment_id: Option<String>,
    part_id: Option<u64>,
    #[serde(default, deserialize_with = "opt_msat")]
    fees_msat: Option<u64>,
    #[serde(default)]
    is_rebalance: bool,
    /// The patched bookkeeper's id for a chain or channel event.
    created_index: Option<u64>,
    /// The patched bookkeeper's id for an on-chain fee, with its account and
    /// txid.
    update_count: Option<u32>,
}

impl Event {
    /// Whether it goes in the `cln` layer rather than `cln-chain`.
    fn is_payment(&self) -> bool {
        self.kind == "channel" && is_payment_tag(&self.tag)
    }

    /// What it moved into its account: credit less debit.
    fn net(&self) -> i64 {
        self.credit_msat as i64 - self.debit_msat as i64
    }
}

/// An msat amount, as a number or, from an older node, `"1000msat"`.
fn msat<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Msat {
        Number(u64),
        Text(String),
    }
    match Msat::deserialize(deserializer)? {
        Msat::Number(n) => Ok(n),
        Msat::Text(text) => text
            .strip_suffix("msat")
            .unwrap_or(&text)
            .parse()
            .map_err(|_| serde::de::Error::custom(format!("not an msat amount: {text:?}"))),
    }
}

fn opt_msat<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    msat(deserializer).map(Some)
}

/// Turns events into raw transactions.
struct Convert<'a> {
    source: &'a str,
    zone: Zone,
    held: &'a Ledger,
}

impl Convert<'_> {
    /// `<source>:h:<16 hex>`: the event's fields but its description, which
    /// can be edited, hashed.
    fn hash_id(&self, event: &Event) -> String {
        let mut hasher = Sha256::new();
        let fields = [
            event.kind.as_str(),
            &event.tag,
            &event.account,
            &event.credit_msat.to_string(),
            &event.debit_msat.to_string(),
            &event.timestamp.to_string(),
            event.payment_id.as_deref().unwrap_or(""),
            &event.part_id.map_or_else(String::new, |p| p.to_string()),
            event.outpoint.as_deref().unwrap_or(""),
            event.txid.as_deref().unwrap_or(""),
            event.origin.as_deref().unwrap_or(""),
        ];
        for field in fields {
            hasher.update(field.as_bytes());
            hasher.update([0x1f]);
        }
        let digest = hasher.finalize();
        let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        format!("{}:h:{hex}", self.source)
    }

    /// The patched bookkeeper's id for `event`, unless the journal already
    /// holds it under `fallback` (or there is no such id).
    fn record_id(&self, event: &Event, fallback: String) -> String {
        let native = match (event.kind.as_str(), event.created_index, &event.txid) {
            ("chain" | "channel", Some(n), _) => format!("{}:{}:{n}", self.source, event.kind),
            ("onchain_fee", _, Some(txid)) => match event.update_count {
                Some(n) => format!("{}:fee:{}:{txid}:{n}", self.source, event.account),
                None => return fallback,
            },
            _ => return fallback,
        };
        if self.held.find(&fallback).is_some() {
            fallback
        } else {
            native
        }
    }

    /// What every record carries.
    fn base_meta(&self, event: &Event) -> Vec<RawMeta> {
        let direction = if event.net() > 0 { "credit" } else { "debit" };
        let mut meta = vec![
            meta("raw-cln-tag", &event.tag),
            meta("raw-timestamp", &event.timestamp.to_string()),
            meta("raw-direction", direction),
        ];
        if let Some(payment_id) = &event.payment_id {
            meta.push(self::meta("raw-payment-id", payment_id));
        }
        meta
    }

    /// An `invoice` or `routed` event, against `assets:cln`.
    fn payment(&self, event: &Event, id: String) -> RawTxn {
        let net = event.net();
        let mut meta = self.base_meta(event);
        if let Some(fees) = event.fees_msat.filter(|&f| f > 0) {
            meta.push(self::meta("raw-fees-msat", &fees.to_string()));
        }
        if event.is_rebalance {
            meta.push(self::meta("raw-rebalance", "true"));
        }
        let by_sign = |received: &str| if net > 0 { received } else { SPENT }.to_owned();
        let (payee, counter) = if event.tag == "routed" {
            let counter = if net > 0 { ROUTED_IN } else { ROUTED_OUT };
            ("Routed".to_owned(), counter.to_owned())
        } else {
            match describe(event.description.as_deref().unwrap_or("")) {
                Description::Zap { p, e, sender } => {
                    let fields = [
                        ("raw-zap-p", p),
                        ("raw-zap-e", e),
                        ("raw-zap-sender", sender),
                    ];
                    for (key, value) in fields {
                        if let Some(value) = value {
                            meta.push(self::meta(key, &value));
                        }
                    }
                    ("Zap".to_owned(), by_sign(ZAPS))
                }
                Description::Lnurl { address, text } => {
                    if let Some(address) = &address {
                        meta.push(self::meta("raw-lnurl-address", address));
                    }
                    if let Some(text) = &text {
                        meta.push(self::meta("raw-lnurl-text", text));
                    }
                    let payee = text
                        .or(address)
                        .unwrap_or_else(|| "LNURL payment".to_owned());
                    (payee, by_sign(LNURL))
                }
                Description::Plain(text) if text.trim().is_empty() => {
                    ("Lightning payment".to_owned(), by_sign(RECEIVED))
                }
                Description::Plain(text) => {
                    meta.push(self::meta("raw-description", text));
                    (text.to_owned(), by_sign(RECEIVED))
                }
            }
        };
        RawTxn {
            date: self.zone.date(event.timestamp),
            cleared: true,
            payee,
            id,
            meta,
            postings: vec![posting(&counter, -net), posting(CHANNELS, net)],
        }
    }

    /// Any other event, for the `cln-chain` layer.
    fn chain(&self, event: &Event, id: String) -> RawTxn {
        let net = event.net();
        let mut meta = self.base_meta(event);
        meta.push(self::meta("raw-account", &event.account));
        let fields = [
            ("raw-outpoint", &event.outpoint),
            ("raw-txid", &event.txid),
            ("raw-origin", &event.origin),
        ];
        for (key, value) in fields {
            if let Some(value) = value {
                meta.push(self::meta(key, value));
            }
        }
        let postings = if event.kind == "onchain_fee" {
            vec![posting(FEES, net), posting(MIRROR, -net)]
        } else if event.account == "external" {
            vec![posting(EXTERNAL, net), posting(MIRROR, -net)]
        } else {
            let own = if event.account == "wallet" {
                WALLET
            } else {
                CHANNELS
            };
            let counter = if net > 0 { RECEIVED } else { SPENT };
            vec![posting(counter, -net), posting(own, net)]
        };
        RawTxn {
            date: self.zone.date(event.timestamp),
            cleared: true,
            payee: event.tag.replace('_', " "),
            id,
            meta,
            postings,
        }
    }
}

fn meta(key: &str, value: &str) -> RawMeta {
    RawMeta {
        key: key.to_owned(),
        value: value.to_owned(),
    }
}

fn posting(account: &str, msat: i64) -> RawPosting {
    RawPosting {
        account: account.to_owned(),
        amount: Some(RawAmount {
            qty: msat,
            precision: 0,
            commodity: MSAT.to_owned(),
        }),
    }
}

/// What an invoice's description says it was.
#[derive(Debug, PartialEq, Eq)]
enum Description<'a> {
    /// A kind-9734 zap request. A tag it lacks is `None`.
    Zap {
        p: Option<String>,
        e: Option<String>,
        sender: Option<String>,
    },
    /// LNURL metadata, `[["text/plain", …], ["text/identifier", …], …]`.
    Lnurl {
        address: Option<String>,
        text: Option<String>,
    },
    /// Anything else, as it is.
    Plain(&'a str),
}

/// Read an invoice description. JSON that isn't a zap request or LNURL
/// metadata, or doesn't parse, is plain text.
fn describe(text: &str) -> Description<'_> {
    let trimmed = text.trim_start();
    if !(trimmed.starts_with('{') || trimmed.starts_with("[[")) {
        return Description::Plain(text);
    }
    let Ok(json) = serde_json::from_str::<Value>(trimmed) else {
        return Description::Plain(text);
    };
    match &json {
        Value::Object(zap) if zap.get("kind").and_then(Value::as_u64) == Some(ZAP_REQUEST) => {
            let tags = zap.get("tags").and_then(Value::as_array);
            let tag = |name: &str| tag_value(tags.map(Vec::as_slice).unwrap_or(&[]), name);
            Description::Zap {
                p: tag("p"),
                e: tag("e"),
                sender: zap.get("pubkey").and_then(Value::as_str).map(str::to_owned),
            }
        }
        Value::Array(entries) if !entries.is_empty() => Description::Lnurl {
            address: tag_value(entries, "text/identifier"),
            text: tag_value(entries, "text/plain"),
        },
        _ => Description::Plain(text),
    }
}

/// The second element of the first `[name, value, …]` entry of `entries`
/// that has a string there.
fn tag_value(entries: &[Value], name: &str) -> Option<String> {
    entries.iter().find_map(|entry| {
        let entry = entry.as_array()?;
        if entry.first()?.as_str()? != name {
            return None;
        }
        entry.get(1)?.as_str().map(str::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use ledgerdb::{JournalStore, LayerFilter, Ledger};

    use super::*;
    use crate::import;

    /// A node answering from a fixed list of events, recording each call's
    /// params. An unpatched one refuses `start_time` as lightningd does.
    struct FakeNode {
        events: Vec<Value>,
        patched: bool,
        calls: Rc<RefCell<Vec<Value>>>,
    }

    impl Node for FakeNode {
        fn call(&self, method: &str, params: Value) -> Result<Value, NodeError> {
            assert_eq!(method, METHOD);
            self.calls.borrow_mut().push(params.clone());
            let start = params.get("start_time").and_then(Value::as_u64);
            if start.is_some() && !self.patched {
                return Err(NodeError::Rpc {
                    code: UNKNOWN_PARAMETER,
                    message: "unknown parameter: start_time, this may be caused by a failure to \
                              autodetect key=value-style parameters."
                        .to_owned(),
                });
            }
            let events: Vec<Value> = self
                .events
                .iter()
                .filter(|e| start.is_none_or(|s| e["timestamp"].as_u64().unwrap() > s))
                .map(|e| {
                    let mut e = e.clone();
                    if !self.patched {
                        let event = e.as_object_mut().unwrap();
                        event.remove("created_index");
                        event.remove("update_count");
                    }
                    e
                })
                .collect();
            Ok(json!({ "events": events }))
        }
    }

    /// 2024-03-01 12:00 UTC.
    const T: u64 = 1_709_294_400;

    fn invoice(credit: u64, debit: u64, description: &str, index: u64) -> Value {
        json!({
            "account": "c0ffee", "type": "channel", "tag": "invoice",
            "credit_msat": credit, "debit_msat": debit, "currency": "bc",
            "payment_id": format!("{index:064x}"), "part_id": 0,
            "timestamp": T + index, "description": description,
            "is_rebalance": false, "created_index": index,
        })
    }

    fn zap_request(tags: Value) -> String {
        json!({ "kind": 9734, "pubkey": "aa".repeat(32), "content": "", "tags": tags }).to_string()
    }

    fn utc() -> ClnConfig {
        ClnConfig {
            timezone: Some("utc".to_owned()),
            ..ClnConfig::default()
        }
    }

    fn input() -> ImportInput {
        ImportInput {
            source: "cln".to_owned(),
            paths: vec![],
        }
    }

    /// An importer over a [`FakeNode`], and the calls it gets.
    struct Rig {
        cln: ClnImporter,
        calls: Rc<RefCell<Vec<Value>>>,
    }

    fn importer(events: Vec<Value>, patched: bool) -> Rig {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let node = FakeNode {
            events,
            patched,
            calls: calls.clone(),
        };
        Rig {
            cln: ClnImporter::with_node(Some(&utc()), Box::new(node)),
            calls,
        }
    }

    /// The batch's transactions, read back from its journal.
    fn read_batch(journal: &ledgerdb::Journal) -> Vec<Txn> {
        journal
            .txns
            .iter()
            .map(|txn| Txn {
                id: journal.strings.get(txn.id.unwrap()).to_owned(),
                date: txn.date.to_string(),
                payee: journal.strings.get(txn.payee).to_owned(),
                postings: journal
                    .postings_of(txn)
                    .iter()
                    .map(|p| Leg {
                        account: journal.accounts.full_name(p.account).to_owned(),
                        msat: p.qty,
                    })
                    .collect(),
                meta: journal
                    .meta_of(txn)
                    .iter()
                    .map(|m| {
                        format!(
                            "{}={}",
                            journal.strings.get(m.key),
                            journal.strings.get(m.value)
                        )
                    })
                    .collect(),
            })
            .collect()
    }

    #[derive(Debug, PartialEq)]
    struct Txn {
        id: String,
        date: String,
        payee: String,
        postings: Vec<Leg>,
        meta: Vec<String>,
    }

    #[derive(Debug, PartialEq)]
    struct Leg {
        account: String,
        msat: i64,
    }

    fn leg(account: &str, msat: i64) -> Leg {
        Leg {
            account: account.to_owned(),
            msat,
        }
    }

    fn has(txn: &Txn, meta: &str) -> bool {
        txn.meta.iter().any(|m| m == meta)
    }

    #[test]
    fn invoices_post_to_their_defaults_by_description() {
        let events = vec![
            invoice(5000, 0, "Thanks for the coffee", 1),
            invoice(0, 2000, "Pay a friend", 2),
            invoice(
                21_000,
                0,
                &zap_request(json!([["p", "bb".repeat(32)], ["e", "cc".repeat(32)]])),
                3,
            ),
            // A zap with no `p` tag doesn't crash, it just has no recipient.
            invoice(1000, 0, &zap_request(json!([["relays", "wss://x"]])), 4),
            invoice(
                9000,
                0,
                &json!([
                    ["text/plain", "Sats for someone"],
                    ["text/identifier", "someone@example.com"]
                ])
                .to_string(),
                5,
            ),
            // Broken JSON is a plain description.
            invoice(7000, 0, "{\"kind\": 9734, oops", 6),
            invoice(3000, 0, "", 7),
        ];
        let Rig { cln, .. } = importer(events, true);
        let batch = cln.import(&input()).unwrap();
        let txns = read_batch(&batch.journal);
        assert_eq!(txns.len(), 7);

        assert_eq!(txns[0].id, "cln:channel:1");
        assert_eq!(txns[0].date, "2024-03-01");
        assert_eq!(txns[0].payee, "Thanks for the coffee");
        assert_eq!(
            txns[0].postings,
            [leg("income:unknown", -5000), leg("assets:cln", 5000)]
        );
        assert!(has(&txns[0], "raw-description=Thanks for the coffee"));
        assert!(has(&txns[0], "raw-cln-tag=invoice"));
        assert!(has(&txns[0], "raw-direction=credit"));
        assert!(has(&txns[0], &format!("raw-timestamp={}", T + 1)));

        assert_eq!(
            txns[1].postings,
            [leg("expenses:unknown", 2000), leg("assets:cln", -2000)]
        );
        assert!(has(&txns[1], "raw-direction=debit"));

        assert_eq!(txns[2].payee, "Zap");
        assert_eq!(txns[2].postings[0], leg("income:zap", -21_000));
        assert!(has(&txns[2], &format!("raw-zap-p={}", "bb".repeat(32))));
        assert!(has(&txns[2], &format!("raw-zap-e={}", "cc".repeat(32))));
        assert!(has(
            &txns[2],
            &format!("raw-zap-sender={}", "aa".repeat(32))
        ));

        assert_eq!(txns[3].postings[0], leg("income:zap", -1000));
        assert!(!txns[3].meta.iter().any(|m| m.starts_with("raw-zap-p")));

        assert_eq!(txns[4].payee, "Sats for someone");
        assert_eq!(txns[4].postings[0], leg("income:lnurl", -9000));
        assert!(has(&txns[4], "raw-lnurl-address=someone@example.com"));
        assert!(has(&txns[4], "raw-lnurl-text=Sats for someone"));

        assert_eq!(txns[5].postings[0], leg("income:unknown", -7000));
        assert!(has(&txns[5], "raw-description={\"kind\": 9734, oops"));

        assert_eq!(txns[6].payee, "Lightning payment");
        assert!(batch.notes.is_empty());
    }

    #[test]
    fn routed_legs_are_two_records_with_a_payee() {
        let leg_event = |account: &str, credit: u64, debit: u64, index: u64| {
            json!({
                "account": account, "type": "channel", "tag": "routed",
                "credit_msat": credit, "debit_msat": debit, "fees_msat": 90,
                "currency": "bc", "payment_id": "dd".repeat(32), "part_id": 0,
                "timestamp": T, "is_rebalance": false,
            })
            .as_object()
            .cloned()
            .map(|mut e| {
                e.insert("created_index".to_owned(), json!(index));
                Value::Object(e)
            })
            .unwrap()
        };
        let events = vec![
            leg_event("out", 0, 36_000, 10),
            leg_event("in", 36_090, 0, 11),
        ];
        for patched in [true, false] {
            let Rig { cln, .. } = importer(events.clone(), patched);
            let txns = read_batch(&cln.import(&input()).unwrap().journal);
            assert_eq!(txns.len(), 2);
            assert_ne!(txns[0].id, txns[1].id, "the legs share a payment_id");
            assert!(txns.iter().all(|t| t.payee == "Routed"));
            assert_eq!(txns[0].postings[0], leg("routed:out", 36_000));
            assert_eq!(txns[1].postings[0], leg("routed:in", -36_090));
            assert!(has(&txns[1], "raw-fees-msat=90"));
        }
    }

    #[test]
    fn other_events_go_in_the_chain_layer() {
        let events = vec![
            json!({
                "account": "wallet", "type": "chain", "tag": "deposit",
                "credit_msat": 50_000_000, "debit_msat": 0, "currency": "bc",
                "outpoint": format!("{}:1", "ee".repeat(32)), "timestamp": T,
                "blockheight": 800_000, "created_index": 3,
            }),
            json!({
                "account": "external", "type": "chain", "tag": "to_them",
                "credit_msat": 1_000_000, "debit_msat": 0, "currency": "bc",
                "outpoint": format!("{}:0", "ee".repeat(32)), "origin": "c0ffee",
                "timestamp": T, "blockheight": 800_000, "created_index": 4,
            }),
            json!({
                "account": "c0ffee", "type": "onchain_fee", "tag": "onchain_fee",
                "credit_msat": 204_000, "debit_msat": 0, "currency": "bc",
                "txid": "ff".repeat(32), "timestamp": T, "update_count": 2,
            }),
            // Zero each way: a peer's channel open, nothing of ours moved.
            json!({
                "account": "c0ffee", "type": "chain", "tag": "channel_open",
                "credit_msat": 0, "debit_msat": 0, "currency": "bc",
                "outpoint": format!("{}:0", "ee".repeat(32)), "timestamp": T,
                "blockheight": 0, "created_index": 5,
            }),
            json!({
                "account": "c0ffee", "type": "channel", "tag": "journal_entry",
                "credit_msat": 0, "debit_msat": 1500, "currency": "bc",
                "timestamp": T, "is_rebalance": false, "created_index": 6,
            }),
        ];
        let Rig { cln, .. } = importer(events, true);
        let batch = cln.import(&input()).unwrap();
        assert!(batch.journal.txns.is_empty());
        let [other] = batch.other_layers.as_slice() else {
            panic!("one other layer");
        };
        assert_eq!(other.layer, "cln-chain");
        let txns = read_batch(&other.journal);
        let ids: Vec<_> = txns.iter().map(|t| t.id.as_str()).collect();
        let fee_id = format!("cln:fee:c0ffee:{}:2", "ff".repeat(32));
        assert_eq!(
            ids,
            ["cln:chain:3", "cln:chain:4", &fee_id, "cln:channel:6"]
        );
        assert_eq!(txns[0].payee, "deposit");
        assert_eq!(
            txns[0].postings,
            [
                leg("income:unknown", -50_000_000),
                leg("assets:cln:wallet", 50_000_000)
            ]
        );
        assert!(has(&txns[0], "raw-account=wallet"));
        assert!(has(
            &txns[0],
            &format!("raw-outpoint={}:1", "ee".repeat(32))
        ));
        assert_eq!(
            txns[1].postings,
            [
                leg("equity:cln:external", 1_000_000),
                leg("equity:cln", -1_000_000)
            ]
        );
        assert!(has(&txns[1], "raw-origin=c0ffee"));
        assert_eq!(
            txns[2].postings,
            [leg("equity:cln:fees", 204_000), leg("equity:cln", -204_000)]
        );
        assert_eq!(
            txns[3].postings,
            [leg("expenses:unknown", 1500), leg("assets:cln", -1500)]
        );
    }

    #[test]
    fn an_unpatched_node_gets_hash_ids_that_count_identical_events() {
        let twice = invoice(25_000, 0, "Same invoice", 1);
        let events = vec![twice.clone(), twice, invoice(1000, 0, "other", 2)];
        let Rig { cln, .. } = importer(events, false);
        let txns = read_batch(&cln.import(&input()).unwrap().journal);
        assert!(txns[0].id.starts_with("cln:h:"), "{}", txns[0].id);
        assert_eq!(txns[0].id.len(), "cln:h:".len() + 16);
        assert_eq!(txns[1].id, format!("{}:2", txns[0].id));
        assert_ne!(txns[2].id, txns[0].id);
    }

    #[test]
    fn the_description_is_not_part_of_a_hash_id() {
        let ids = |description| {
            let Rig { cln, .. } = importer(vec![invoice(25_000, 0, description, 1)], false);
            read_batch(&cln.import(&input()).unwrap().journal)[0]
                .id
                .clone()
        };
        assert_eq!(ids("before an edit"), ids("after an edit"));
    }

    /// Import `events` from a node into `store`, at `imported`.
    fn pull(
        store: &JournalStore,
        events: &[Value],
        patched: bool,
        imported: u64,
    ) -> import::Written {
        let Rig { cln, .. } = importer(events.to_vec(), patched);
        import::write(
            store,
            &Default::default(),
            &cln,
            &input(),
            None,
            Some(imported),
        )
        .unwrap()
    }

    #[test]
    fn a_patched_node_is_read_from_the_cursor_and_keeps_older_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        let deposit = json!({
            "account": "wallet", "type": "chain", "tag": "deposit",
            "credit_msat": 7000, "debit_msat": 0, "currency": "bc",
            "outpoint": format!("{}:1", "ee".repeat(32)), "timestamp": T,
            "blockheight": 1, "created_index": 1,
        });
        let first = vec![
            invoice(1000, 0, "one", 1),
            invoice(2000, 0, "two", 2),
            deposit,
        ];

        // Imported first from an unpatched node: hash ids, two layers.
        let written = pull(&store, &first, false, 10);
        assert_eq!(written.header.layer, "cln");
        assert_eq!(written.txns, 2);
        let [chain] = written.other_layers.as_slice() else {
            panic!("a cln-chain segment");
        };
        assert_eq!((chain.header.layer.as_str(), chain.txns), ("cln-chain", 1));
        assert_eq!(store.list().unwrap().len(), 2);

        let held = Ledger::load(&store, LayerFilter::All).unwrap().ledger;
        assert_eq!(cursor(&held, "cln"), Some(T + 2 - OVERLAP_SECS));
        assert_eq!(cursor(&held, "other"), None);

        // The node is patched and has a new event: it is asked from the
        // cursor, the two it holds keep their hash ids, the new one gets
        // its created_index.
        let mut later = first.clone();
        later.push(invoice(3000, 0, "three", 3));
        let Rig { cln, calls } = importer(later.clone(), true);
        let written =
            import::write(&store, &Default::default(), &cln, &input(), None, Some(11)).unwrap();
        assert_eq!(
            calls.borrow().as_slice(),
            [json!({ "start_time": T + 2 - OVERLAP_SECS })]
        );
        assert_eq!((written.txn_rows.new, written.txn_rows.unchanged), (1, 2));
        assert_eq!(written.other_layers[0].txn_rows.unchanged, 1);
        let ledger = Ledger::load(&store, LayerFilter::All).unwrap().ledger;
        assert_eq!(ledger.txns().len(), 4);
        assert!(ledger.find("cln:channel:3").is_some());
        assert!(ledger.find("cln:channel:1").is_none());

        // Once more: nothing new, nothing written.
        let again = pull(&store, &later, true, 12);
        assert_eq!(again.hash, None);
        assert_eq!(again.txn_rows.unchanged, 3);
        assert_eq!(store.list().unwrap().len(), 3);
    }

    #[test]
    fn an_unpatched_node_is_read_whole_and_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        let events = vec![invoice(1000, 0, "one", 1)];
        pull(&store, &events, false, 10);

        let Rig { cln, calls } = importer(events, false);
        let written =
            import::write(&store, &Default::default(), &cln, &input(), None, Some(11)).unwrap();
        assert_eq!(
            calls.borrow().as_slice(),
            [json!({ "start_time": T + 1 - OVERLAP_SECS }), json!({})]
        );
        assert_eq!(written.notes.len(), 1);
        assert!(
            written.notes[0].contains("start_time"),
            "{}",
            written.notes[0]
        );
        assert_eq!(written.hash, None);
    }

    #[test]
    fn any_other_node_error_fails_the_import() {
        struct Down;
        impl Node for Down {
            fn call(&self, _: &str, _: Value) -> Result<Value, NodeError> {
                Err(NodeError::Rpc {
                    code: 1502,
                    message: "Not authorized: method not permitted".to_owned(),
                })
            }
        }
        let cln = ClnImporter::with_node(Some(&utc()), Box::new(Down));
        let err = cln.import(&input()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "bkpr-listaccountevents: Not authorized: method not permitted (1502)"
        );
    }

    #[test]
    fn a_saved_listing_reads_and_skips_what_does_not() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.json");
        let mut old_style = invoice(0, 0, "old node", 2);
        old_style["credit_msat"] = json!("4000msat");
        old_style["debit_msat"] = json!("0msat");
        let listing = json!({ "events": [
            invoice(1000, 0, "one", 1),
            { "account": "c0ffee", "type": "channel" },
            old_style,
        ]});
        std::fs::write(&path, listing.to_string()).unwrap();
        let cln = ClnImporter::new(Some(&utc()), &CashflowDir::new("unused"));
        let batch = cln
            .import(&ImportInput {
                source: "cln".to_owned(),
                paths: vec![path],
            })
            .unwrap();
        let txns = read_batch(&batch.journal);
        assert_eq!(txns.len(), 2);
        assert_eq!(txns[1].postings[1], leg("assets:cln", 4000));
        assert_eq!(batch.skipped.len(), 1);
        assert_eq!(batch.skipped[0].line, 2);
        assert!(batch.skipped[0].message.contains("missing field"));

        std::fs::write(tmp.path().join("bad.json"), "[1, 2]").unwrap();
        let err = cln
            .import(&ImportInput {
                source: "cln".to_owned(),
                paths: vec![tmp.path().join("bad.json")],
            })
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("not bkpr-listaccountevents output"),
            "{err}"
        );
    }

    #[test]
    fn fetching_needs_the_node_and_a_rune() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = CashflowDir::new(tmp.path());
        let usage = |config: &ClnConfig| match ClnImporter::new(Some(config), &dir).import(&input())
        {
            Err(ImportError::Usage(message)) => message,
            other => panic!("{other:?}"),
        };
        let message = usage(&utc());
        assert!(message.contains("`node_id` under [cln]"), "{message}");
        assert!(
            message.contains("cashflow config set cln.node_id"),
            "{message}"
        );
        let config = ClnConfig {
            node_id: Some(format!("02{}", "11".repeat(32))),
            host: Some("127.0.0.1".to_owned()),
            ..utc()
        };
        // Unless the environment happens to carry one.
        if std::env::var(secret_env_var(RUNE_SECRET)).is_err() {
            let message = usage(&config);
            assert!(message.contains("createrune"), "{message}");
            assert!(
                message.contains("cashflow secret set cln-rune"),
                "{message}"
            );
        }
        let config = ClnConfig {
            timezone: Some("mars".to_owned()),
            ..config
        };
        assert!(usage(&config).contains("[cln] timezone"));
    }

    /// Two machines dating in their own zones file an event near midnight
    /// under different days, so each import rewrites the other's. Once the
    /// zone comes from the synced setting they agree, and the second
    /// machine's import writes nothing.
    #[test]
    fn a_synced_zone_keeps_two_machines_from_rewriting_each_other() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = CashflowDir::new(tmp.path());
        let store = JournalStore::new(tmp.path().join("journal"));
        let path = tmp.path().join("events.json");
        let mut late = invoice(1000, 0, "late", 1);
        // 2024-03-02 02:30 UTC: March 1st anywhere west of UTC-2:30.
        late["timestamp"] = json!(1_709_346_600);
        std::fs::write(&path, json!({ "events": [late] }).to_string()).unwrap();
        let input = ImportInput {
            source: "cln".to_owned(),
            paths: vec![path],
        };
        let run = |cln: &ClnImporter, imported| {
            import::write(
                &store,
                &Default::default(),
                cln,
                &input,
                None,
                Some(imported),
            )
            .unwrap()
        };
        let own_zone = |zone: &str| {
            let config = ClnConfig {
                timezone: Some(zone.to_owned()),
                ..ClnConfig::default()
            };
            ClnImporter::new(Some(&config), &dir)
        };
        let date = |id: &str| {
            let ledger = Ledger::load(&store, LayerFilter::All).unwrap().ledger;
            ledger.txns()[ledger.find(id).unwrap().index()]
                .date
                .to_string()
        };

        run(&own_zone("utc"), 10);
        assert_eq!(date("cln:channel:1"), "2024-03-02");
        let flipped = run(&own_zone("-07:00"), 11);
        assert_eq!(flipped.txn_rows.changed, 1);
        assert_eq!(date("cln:channel:1"), "2024-03-01");

        let synced = Synced {
            timezone: Some("Asia/Tokyo".to_owned()),
        };
        let laptop = run(&ClnImporter::new(None, &dir).synced(&synced), 12);
        assert_eq!(laptop.txn_rows.changed, 1);
        assert!(laptop.notes.is_empty(), "{:?}", laptop.notes);
        let desktop = run(&ClnImporter::new(None, &dir).synced(&synced), 13);
        assert_eq!(desktop.hash, None);
        assert_eq!(desktop.txn_rows.unchanged, 1);
        assert_eq!(date("cln:channel:1"), "2024-03-02");
    }

    /// `[cln] timezone` beats the synced setting, and says so when they
    /// differ; with neither set it is the machine's own zone, and says so.
    #[test]
    fn the_local_override_and_the_fallback_are_noted() {
        let dir = CashflowDir::new("unused");
        let dating = |config: Option<&str>, synced: Option<&str>| {
            let config = ClnConfig {
                timezone: config.map(str::to_owned),
                ..ClnConfig::default()
            };
            let synced = Synced {
                timezone: synced.map(str::to_owned),
            };
            ClnImporter::new(Some(&config), &dir)
                .synced(&synced)
                .dating()
        };

        let shared = dating(None, Some("America/Vancouver")).unwrap();
        assert!(matches!(shared.zone, Zone::Named(_)));
        assert_eq!(shared.note, None);

        let overridden = dating(Some("utc"), Some("America/Vancouver")).unwrap();
        assert_eq!(overridden.zone, Zone::Fixed(0));
        let note = overridden.note.unwrap();
        assert!(
            note.contains("not the synced timezone America/Vancouver"),
            "{note}"
        );
        assert_eq!(dating(Some("UTC"), Some("utc")).unwrap().note, None);

        let fallback = dating(None, None).unwrap();
        assert_eq!(fallback.zone, Zone::Local);
        assert!(fallback.note.unwrap().contains("settings set timezone"));

        let Err(ImportError::Usage(message)) = dating(None, Some("Mars/Olympus")) else {
            panic!("a bad synced zone is a usage error");
        };
        assert!(message.contains("timezone setting"), "{message}");
    }

    #[test]
    fn describe_reads_each_shape() {
        assert_eq!(describe("plain"), Description::Plain("plain"));
        assert_eq!(describe("[1, 2]"), Description::Plain("[1, 2]"));
        assert_eq!(
            describe("{\"kind\": 1}"),
            Description::Plain("{\"kind\": 1}")
        );
        assert_eq!(
            describe("[[]]"),
            Description::Lnurl {
                address: None,
                text: None
            }
        );
        assert_eq!(
            describe("{\"kind\": 9734, \"tags\": \"nope\"}"),
            Description::Zap {
                p: None,
                e: None,
                sender: None
            }
        );
    }
}
