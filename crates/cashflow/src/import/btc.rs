//! The `btc` importer: on-chain bitcoin wallets, read from bitcoind.
//!
//! With no paths it asks bitcoind over JSON-RPC: `listwallets` (or the
//! wallets [`BtcConfig`] names), then for each wallet `listtransactions "*"
//! <count> <skip>`, paged until a page comes back short, so a wallet of any
//! size is read whole. Given paths it reads the tab-separated listing the old
//! `btc-txs-raw` script printed instead, one entry per line:
//!
//! ```text
//! wallet  label  address  category  amount  blocktime  confirmations  txid  vout  [fee]
//! ```
//!
//! (the optional tenth column, a send's fee, is one the old script never
//! printed). That is for offline use and tests.
//!
//! **Where bitcoind's RPC can't be reached**, from a laptop away from the
//! machine it runs on, say, the same calls go through the `corerpc` plugin
//! (jb55/cln-core-rpc) of the CLN node `[cln]` names, over commando with the
//! `cln-rune` secret: `[btc] via` (see [`BtcVia`]). By default a run tries
//! HTTP first and falls back only when the first call can't reach bitcoind
//! or there are no credentials for it, and then reads the whole run through
//! corerpc, so a run never mixes the two.
//!
//! **Amounts are exact.** bitcoind writes an amount as a decimal number of
//! bitcoin, read from the JSON text itself (never through an `f64`) into
//! integer msat, the commodity amounts are written in. Through corerpc they
//! pass through a double twice, corerpc's JavaScript and lnsocket's parsed
//! `Value`, so they are exact to 15 significant digits, which is every
//! amount under 10M BTC to the sat (see [`Corerpc`]); over HTTP they stay
//! exact from bitcoind's own text. An entry not yet in a
//! block, or one whose transaction lost to a conflicting one, has no date to
//! file it under: it is left out (see [`super::LeftOut`]) and a later import
//! takes it once it confirms.
//!
//! **One transaction per txid.** bitcoind lists a transaction once per
//! output, per wallet. The entries of one txid across every wallet read
//! become one transaction, `btc:<txid>`, whenever any of them is a `send`:
//! a send's output that another wallet read as a `receive` is a transfer
//! between your own wallets, `assets:btc:<a>` to `assets:btc:<b>`, rather
//! than an expense in one and an income in the other. Any other output of a
//! send goes to `expenses:unknown`, and its fee, once per sending wallet, to
//! `expenses:fees:btc` (unless the wallet paid only some of the inputs; see
//! `Entry::fee`), so each wallet's account moves exactly as bitcoind's
//! listing sums. A txid with no send (a payment in, or a mined coin)
//! is one transaction per output, `btc:<txid>:<vout>`, against
//! `income:unknown`.
//!
//! What each came from is kept as `raw-txid`, `raw-wallet`, `raw-category`,
//! `raw-label` and `raw-address` metadata, so a rule can file mining payouts
//! or a donor's address; none of that is in this code. Its segments go in the
//! `btc-onchain` layer, which a reader can switch off as a whole.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::Path;

use base64::Engine;
use ledgerdb::Date;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::value::RawValue;
use serde_json::{Value, json};

use super::cln::{self, NoCommando, Node, NodeError};
use super::{
    Batch, ImportError, ImportInput, Importer, LeftOut, RawAmount, RawMeta, RawPosting, RawTxn,
};
use crate::CashflowDir;
use crate::config::{BtcConfig, BtcVia, ClnConfig, secret_env_var};
use crate::prices::MSAT;

/// The secret holding the password of [`BtcConfig::user`].
pub const PASSWORD_SECRET: &str = "bitcoind-rpc-password";

/// bitcoind's RPC endpoint on mainnet when [`BtcConfig::url`] is unset.
const DEFAULT_URL: &str = "http://127.0.0.1:8332";

/// How many entries one `listtransactions` call asks for.
const PAGE: usize = 1000;

/// A send's outputs to someone else.
const SPENT: &str = "expenses:unknown";

/// What came in from outside.
const RECEIVED: &str = "income:unknown";

/// Where a send's fee goes.
const FEES: &str = "expenses:fees:btc";

/// msat in one bitcoin: 10^8 sat of 10^3 msat each.
const MSAT_DIGITS: u32 = 11;

/// A bitcoind JSON-RPC endpoint, so the importer can be tested without one
/// and read through more than one transport.
pub trait Rpc {
    /// Call `method` with `params` (a JSON array) on the node (`wallet`
    /// `None`) or on the named wallet, and return the answer's `result` as
    /// JSON text, so an amount in it can still be read exactly.
    fn call(
        &self,
        wallet: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Box<RawValue>, RpcError>;
}

/// Why an [`Rpc`] call has no result. The kinds are kept apart so a caller
/// can tell "bitcoind could not be reached" from "bitcoind said no".
#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    /// The request never reached bitcoind: the connection was refused or
    /// timed out, or the host did not resolve. Over HTTP a malformed URL or
    /// a TLS failure reads as this too (see [`BitcoindHttp`]).
    #[error("{0}")]
    Unreachable(String),
    /// There is nothing to sign in with: no password for the configured
    /// user, or no cookie file to read.
    #[error("{0}")]
    NoCredentials(String),
    /// bitcoind turned the credentials down.
    #[error("{0}")]
    Unauthorized(String),
    /// bitcoind answered with a JSON-RPC error, such as -18 for a wallet
    /// that is not loaded.
    #[error("{message} (code {code})")]
    Rpc { code: i64, message: String },
    /// Anything else: an answer that isn't JSON-RPC, one with no result, or
    /// one cut off mid-body.
    #[error("{0}")]
    Other(String),
}

/// [`Rpc`] over `ehttp`, with HTTP basic auth.
pub struct BitcoindHttp {
    url: String,
    /// `Basic <base64 of user:password>`.
    authorization: String,
}

impl BitcoindHttp {
    /// An endpoint at `url` that signs in with `credentials`, which is
    /// `user:password`, as bitcoind's cookie file holds it.
    pub fn new(url: &str, credentials: &str) -> Self {
        let encoded = base64::engine::general_purpose::STANDARD.encode(credentials);
        Self {
            url: url.trim_end_matches('/').to_owned(),
            authorization: format!("Basic {encoded}"),
        }
    }
}

impl Rpc for BitcoindHttp {
    fn call(
        &self,
        wallet: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Box<RawValue>, RpcError> {
        let path = wallet.map_or_else(String::new, |name| format!("/wallet/{}", encode(name)));
        let body = json!({"jsonrpc": "1.0", "id": "cashflow", "method": method, "params": params});
        let mut request =
            ehttp::Request::post(format!("{}{path}", self.url), body.to_string().into_bytes());
        request.headers.insert("Authorization", &self.authorization);
        request.headers.insert("Content-Type", "application/json");
        // ehttp hands back only the text of a failure. Any `Err` is one where
        // no answer arrived: refused, timed out, a host that did not resolve,
        // but also a malformed URL or a TLS failure, which its text alone
        // can't tell apart from those. None of them reached bitcoind, so all
        // read as unreachable, except a body cut off after it answered.
        let response = ehttp::fetch_blocking(&request).map_err(|error| {
            if error.starts_with("Failed to read response body") {
                RpcError::Other(error)
            } else {
                RpcError::Unreachable(error)
            }
        })?;
        if response.status == 401 {
            return Err(RpcError::Unauthorized(
                "401 Unauthorized: check [btc] user and the bitcoind-rpc-password secret, or the cookie".to_owned(),
            ));
        }
        // bitcoind reports an RPC error as JSON under an HTTP error status,
        // so the body is read whatever the status.
        result_of(&response.bytes)
    }
}

/// The `result` of a JSON-RPC answer, or its error.
fn result_of(answer: &[u8]) -> Result<Box<RawValue>, RpcError> {
    let parsed: Answer = serde_json::from_slice(answer).map_err(|e| {
        let text = String::from_utf8_lossy(answer);
        RpcError::Other(format!(
            "{e}: {}",
            text.chars().take(200).collect::<String>()
        ))
    })?;
    if let Some(error) = parsed.error {
        return Err(RpcError::Rpc {
            code: error.code,
            message: error.message,
        });
    }
    parsed
        .result
        .ok_or_else(|| RpcError::Other("no result in the answer".to_owned()))
}

/// [`Rpc`] through a CLN node's `corerpc` plugin: `corerpc <wallet>
/// <method> [args…]`, the empty wallet being bitcoind's default.
///
/// corerpc is JavaScript, so bitcoind's numbers reach it as doubles, and
/// lnsocket hands back a parsed [`Value`]: the answer is that value written
/// back out as text. A double's shortest form reads back as the same
/// decimal for every amount of at most 15 significant digits, which
/// [`btc_to_msat`] then reads exactly.
pub struct Corerpc<'a> {
    node: &'a dyn Node,
}

impl<'a> Corerpc<'a> {
    /// bitcoind, through `node`'s corerpc.
    pub fn new(node: &'a dyn Node) -> Self {
        Self { node }
    }
}

impl Rpc for Corerpc<'_> {
    fn call(
        &self,
        wallet: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Box<RawValue>, RpcError> {
        let Value::Array(args) = params else {
            return Err(RpcError::Other(format!(
                "corerpc takes its params as an array, not {params}"
            )));
        };
        let mut corerpc = vec![json!(wallet.unwrap_or("")), json!(method)];
        corerpc.extend(args);
        let answer =
            self.node
                .call("corerpc", Value::Array(corerpc))
                .map_err(|error| match error {
                    NodeError::Rpc {
                        code: cln::RUNE_DENIED,
                        message,
                    } => RpcError::Rpc {
                        code: cln::RUNE_DENIED,
                        message: format!("{message}; {}", cln::mint_rune()),
                    },
                    NodeError::Rpc { code, message } => RpcError::Rpc { code, message },
                    NodeError::Other(error) => RpcError::Unreachable(error),
                })?;
        RawValue::from_string(answer.to_string()).map_err(|e| RpcError::Other(e.to_string()))
    }
}

/// Reads on-chain bitcoin wallets from bitcoind, or from `btc-txs-raw`
/// listings.
pub struct BtcImporter {
    config: BtcConfig,
    /// The CLN node a run reads through when bitcoind can't be reached.
    cln: ClnConfig,
    /// Where the RPC password and rune secrets are read from, when the
    /// import runs.
    dir: CashflowDir,
    /// The endpoint to use instead of connecting with `config`, for tests.
    rpc: Option<Box<dyn Rpc>>,
    /// The CLN node to use instead of connecting with `cln`, for tests.
    node: Option<Box<dyn Node>>,
}

impl BtcImporter {
    /// The importer as `config.toml`'s `[btc]` table sets it up, reading
    /// through the node `[cln]` names when it has to, and its secrets from
    /// `dir` when it runs.
    pub fn new(config: Option<&BtcConfig>, cln: Option<&ClnConfig>, dir: &CashflowDir) -> Self {
        Self {
            config: config.cloned().unwrap_or_default(),
            cln: cln.cloned().unwrap_or_default(),
            dir: dir.clone(),
            rpc: None,
            node: None,
        }
    }

    /// The importer reading from `rpc` rather than connecting over HTTP.
    pub fn with_rpc(config: Option<&BtcConfig>, rpc: Box<dyn Rpc>) -> Self {
        Self {
            rpc: Some(rpc),
            ..Self::new(config, None, &CashflowDir::new(""))
        }
    }

    /// The importer reading through `node`'s corerpc rather than connecting
    /// to the node `[cln]` names.
    pub fn with_node(self, node: Box<dyn Node>) -> Self {
        Self {
            node: Some(node),
            ..self
        }
    }

    /// bitcoind's endpoint, as notes and errors name it.
    fn url(&self) -> &str {
        self.config.url.as_deref().unwrap_or(DEFAULT_URL)
    }

    /// Every entry, through the backend `[btc] via` picks.
    fn read(&self) -> Result<Read, ImportError> {
        match self.config.via.unwrap_or_default() {
            BtcVia::Http => {
                let entries = self.over_http(|rpc| self.fetch(rpc)).map_err(usage)??;
                Ok(Read {
                    entries,
                    note: None,
                })
            }
            BtcVia::Corerpc => {
                let entries = self
                    .through_corerpc(|rpc| self.fetch(rpc))
                    .map_err(|missing| ImportError::Usage(missing.to_string()))??;
                Ok(Read {
                    entries,
                    note: None,
                })
            }
            BtcVia::Auto => self.read_auto(),
        }
    }

    /// HTTP, unless its first call can't reach bitcoind or there are no
    /// credentials for it; then corerpc for the whole run, when `[cln]`
    /// can be reached, with a note saying why.
    fn read_auto(&self) -> Result<Read, ImportError> {
        let (why, failed) = match self.over_http(|rpc| {
            let counted = Counted::new(rpc);
            let fetched = self.fetch(&counted);
            (fetched, counted.answered.get())
        }) {
            Err(RpcError::NoCredentials(error)) => {
                ("has no credentials".to_owned(), ImportError::Usage(error))
            }
            Err(error) => return Err(usage(error)),
            Ok((Ok(entries), _)) => {
                return Ok(Read {
                    entries,
                    note: None,
                });
            }
            Ok((
                Err(FetchError::Call(CallFailed {
                    from,
                    error: RpcError::Unreachable(error),
                })),
                0,
            )) => (
                format!("couldn't be reached: {error}"),
                ImportError::Fetch { from, error },
            ),
            Ok((Err(error), _)) => return Err(error.into()),
        };
        let entries = match self.through_corerpc(|rpc| self.fetch(rpc)) {
            Ok(fetched) => fetched?,
            Err(missing) => return Err(no_fallback(failed, &missing)),
        };
        Ok(Read {
            entries,
            note: Some(format!(
                "bitcoind's RPC at {} {why}, so wallets were read through corerpc over commando",
                self.url()
            )),
        })
    }

    /// `work` over HTTP: the endpoint given in its place, else the one the
    /// config names.
    fn over_http<T>(&self, work: impl FnOnce(&dyn Rpc) -> T) -> Result<T, RpcError> {
        if let Some(rpc) = &self.rpc {
            return Ok(work(&**rpc));
        }
        Ok(work(&self.connect()?))
    }

    /// `work` through corerpc: on the node given in its place, else the one
    /// `[cln]` names.
    fn through_corerpc<T>(&self, work: impl FnOnce(&dyn Rpc) -> T) -> Result<T, NoCommando> {
        if let Some(node) = &self.node {
            return Ok(work(&Corerpc::new(&**node)));
        }
        let commando = cln::commando(&self.cln, &self.dir)?;
        Ok(work(&Corerpc::new(&commando)))
    }

    /// The endpoint the config names, signed in with its credentials.
    /// Credentials that aren't there, a password secret or a cookie file,
    /// are [`RpcError::NoCredentials`]; a secrets store that can't be read is
    /// [`RpcError::Other`].
    fn connect(&self) -> Result<BitcoindHttp, RpcError> {
        let url = self.url();
        let credentials = match &self.config.user {
            Some(user) => {
                let password = self
                    .dir
                    .secret(PASSWORD_SECRET)
                    .map_err(|e| RpcError::Other(e.to_string()))?
                    .ok_or_else(|| {
                        RpcError::NoCredentials(format!(
                            "[btc] user is set but its password is not: store it with \
                             `cashflow secret set {PASSWORD_SECRET}` (or set ${})",
                            secret_env_var(PASSWORD_SECRET),
                        ))
                    })?;
                format!("{user}:{}", password.expose())
            }
            None => {
                let cookie =
                    self.config.cookie.clone().or_else(|| {
                        dirs::home_dir().map(|home| home.join(".bitcoin").join(".cookie"))
                    });
                let Some(cookie) = cookie else {
                    return Err(RpcError::NoCredentials(
                        "no bitcoind credentials: run `cashflow config set btc.cookie <path>`, \
                         or `cashflow config set btc.user <rpcuser>` and `cashflow secret set \
                         bitcoind-rpc-password`"
                            .to_owned(),
                    ));
                };
                std::fs::read_to_string(&cookie)
                    .map_err(|source| {
                        RpcError::NoCredentials(format!("reading {}: {source}", cookie.display()))
                    })?
                    .trim_end()
                    .to_owned()
            }
        };
        Ok(BitcoindHttp::new(url, &credentials))
    }

    /// Every entry of every wallet the config names, over `rpc`.
    fn fetch(&self, rpc: &dyn Rpc) -> Result<Vec<Entry>, FetchError> {
        let mut wallets: Vec<Wallet> = match &self.config.wallets {
            Some(names) => names.iter().map(|name| Wallet::own(name)).collect(),
            None => call::<Vec<String>>(rpc, None, "listwallets", json!([]))?
                .iter()
                .map(|name| Wallet::own(name))
                .collect(),
        };
        for name in &self.config.external_wallets {
            match wallets.iter_mut().find(|w| &w.name == name) {
                Some(wallet) => wallet.watch_only = true,
                None => wallets.push(Wallet {
                    name: name.clone(),
                    watch_only: true,
                }),
            }
        }

        // One call at a time, which through corerpc is one commando handshake
        // a page: a handful for a few wallets. `Node::call_each` could batch
        // a wallet's pages over one connection if that ever matters.
        let mut entries = Vec::new();
        for wallet in &wallets {
            for skip in (0..).step_by(PAGE) {
                let params = json!(["*", PAGE, skip, wallet.watch_only]);
                let page: Vec<Listed> = call(rpc, Some(&wallet.name), "listtransactions", params)?;
                let short = page.len() < PAGE;
                for listed in page {
                    entries.push(listed.entry(&wallet.name)?);
                }
                if short {
                    break;
                }
            }
        }
        Ok(entries)
    }
}

impl Importer for BtcImporter {
    fn name(&self) -> &'static str {
        "btc"
    }

    fn layer(&self) -> &'static str {
        "btc-onchain"
    }

    fn import(&self, input: &ImportInput) -> Result<Batch, ImportError> {
        let mut note = None;
        let entries = if !input.paths.is_empty() {
            let mut entries = Vec::new();
            for path in &input.paths {
                entries.extend(read_listing(path)?);
            }
            entries
        } else {
            let read = self.read()?;
            note = read.note;
            read.entries
        };
        let Txns { txns, left_out } = txns(&input.source, entries);
        Ok(Batch {
            left_out,
            notes: note.into_iter().collect(),
            ..Batch::from_raw(&txns, &[])?
        })
    }
}

/// What a run read over the RPC, and why it read through corerpc when it
/// fell back to it.
struct Read {
    entries: Vec<Entry>,
    note: Option<String>,
}

/// An [`Rpc`] that counts the calls answered, so a fallback can tell a
/// bitcoind that never answered from one that stopped mid-run.
struct Counted<'a> {
    rpc: &'a dyn Rpc,
    answered: Cell<usize>,
}

impl<'a> Counted<'a> {
    fn new(rpc: &'a dyn Rpc) -> Self {
        Self {
            rpc,
            answered: Cell::new(0),
        }
    }
}

impl Rpc for Counted<'_> {
    fn call(
        &self,
        wallet: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Box<RawValue>, RpcError> {
        let result = self.rpc.call(wallet, method, params)?;
        self.answered.set(self.answered.get() + 1);
        Ok(result)
    }
}

/// An [`RpcError`] from setting up a call, as the run reports it.
fn usage(error: RpcError) -> ImportError {
    ImportError::Usage(error.to_string())
}

/// `failed`, which a fallback to corerpc would have taken over, saying what
/// the fallback needs that is `missing`.
fn no_fallback(failed: ImportError, missing: &NoCommando) -> ImportError {
    let instead = format!(
        "to read the wallets through the CLN node's corerpc over commando instead: {missing}"
    );
    match failed {
        ImportError::Fetch { from, error } => ImportError::Fetch {
            from,
            error: format!("{error}; {instead}"),
        },
        ImportError::Usage(error) => ImportError::Usage(format!("{error}; {instead}")),
        other => other,
    }
}

/// A wallet to read, and whether to ask for its watch-only addresses.
struct Wallet {
    name: String,
    watch_only: bool,
}

impl Wallet {
    /// A wallet read as bitcoind lists it by default.
    fn own(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            watch_only: false,
        }
    }
}

/// Why [`BtcImporter::fetch`] stopped: a call that failed, kept typed so a
/// caller can tell an unreachable bitcoind from one that said no, or an entry
/// it could not read.
#[derive(Debug, thiserror::Error)]
enum FetchError {
    #[error(transparent)]
    Call(#[from] CallFailed),
    #[error(transparent)]
    Entry(#[from] ImportError),
}

impl From<FetchError> for ImportError {
    fn from(error: FetchError) -> Self {
        match error {
            FetchError::Call(failed) => failed.into(),
            FetchError::Entry(error) => error,
        }
    }
}

/// An [`Rpc`] call that failed, and which call it was.
#[derive(Debug, thiserror::Error)]
#[error("{from}: {error}")]
struct CallFailed {
    /// The call, such as `bitcoind listtransactions (wallet "hot")`.
    from: String,
    error: RpcError,
}

impl From<CallFailed> for ImportError {
    fn from(failed: CallFailed) -> Self {
        ImportError::Fetch {
            from: failed.from,
            error: failed.error.to_string(),
        }
    }
}

/// Call `method` with `params` on the node (`wallet` `None`) or a wallet,
/// and read its result as `T`.
fn call<T: DeserializeOwned>(
    rpc: &dyn Rpc,
    wallet: Option<&str>,
    method: &str,
    params: Value,
) -> Result<T, CallFailed> {
    let fail = |error: RpcError| CallFailed {
        from: match wallet {
            Some(name) => format!("bitcoind {method} (wallet {name:?})"),
            None => format!("bitcoind {method}"),
        },
        error,
    };
    let result = rpc.call(wallet, method, params).map_err(fail)?;
    serde_json::from_str(result.get()).map_err(|e| {
        fail(RpcError::Other(format!(
            "{e}: {}",
            result.get().chars().take(200).collect::<String>()
        )))
    })
}

/// A JSON-RPC answer, its result left as text.
#[derive(Deserialize)]
struct Answer {
    result: Option<Box<RawValue>>,
    error: Option<AnswerError>,
}

#[derive(Deserialize)]
struct AnswerError {
    code: i64,
    message: String,
}

/// One entry of `listtransactions`, as bitcoind writes it. The amounts stay
/// JSON text so they are read exactly.
#[derive(Deserialize)]
struct Listed {
    #[serde(default)]
    address: Option<String>,
    category: String,
    amount: Box<RawValue>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    vout: u32,
    #[serde(default)]
    fee: Option<Box<RawValue>>,
    #[serde(default)]
    confirmations: i64,
    #[serde(default)]
    blocktime: Option<u64>,
    txid: String,
    #[serde(default)]
    abandoned: bool,
}

impl Listed {
    /// The entry, read in `wallet`.
    fn entry(self, wallet: &str) -> Result<Entry, ImportError> {
        let amount = |raw: &RawValue| {
            btc_to_msat(raw.get()).ok_or_else(|| ImportError::Fetch {
                from: format!("bitcoind listtransactions (wallet {wallet:?})"),
                error: format!("txid {}: bad amount {}", self.txid, raw.get()),
            })
        };
        Ok(Entry {
            msat: amount(&self.amount)?,
            fee: self.fee.as_deref().map(amount).transpose()?,
            wallet: wallet.to_owned(),
            label: self.label.unwrap_or_default(),
            address: self.address.unwrap_or_default(),
            category: self.category,
            blocktime: self.blocktime.filter(|&t| t > 0),
            confirmations: self.confirmations,
            abandoned: self.abandoned,
            txid: self.txid,
            vout: self.vout,
        })
    }
}

/// One output of one transaction as one wallet sees it, from either source.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    wallet: String,
    label: String,
    address: String,
    /// `send`, `receive`, `generate`, `immature` or `orphan`.
    category: String,
    /// The change to the wallet: negative for a send.
    msat: i64,
    /// A send's fee as bitcoind reports it, negative, and repeated on every
    /// send entry of the transaction. It is positive when others paid some
    /// of the inputs (a dual-funded channel open, say): bitcoind computes it
    /// as what the wallet put in less every output, so then it is the part
    /// of the listed sends the wallet didn't pay for.
    fee: Option<i64>,
    /// When its block was mined; `None` when it is in none.
    blocktime: Option<u64>,
    /// Negative when the transaction lost to a conflicting one.
    confirmations: i64,
    abandoned: bool,
    txid: String,
    vout: u32,
}

impl Entry {
    /// Why it can't be imported (yet), if it can't.
    fn left_out(&self) -> Option<&'static str> {
        if self.abandoned {
            Some("abandoned")
        } else if self.confirmations < 0 {
            Some("conflicted")
        } else if self.category == "orphan" {
            Some("orphaned coinbase")
        } else if self.blocktime.is_none() || self.confirmations == 0 {
            Some("unconfirmed")
        } else {
            None
        }
    }

    fn is_send(&self) -> bool {
        self.category == "send"
    }
}

/// Read one `btc-txs-raw` listing.
fn read_listing(path: &Path) -> Result<Vec<Entry>, ImportError> {
    let text = std::fs::read_to_string(path).map_err(|source| ImportError::Read {
        path: path.to_owned(),
        source,
    })?;
    let mut entries = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry = listing_entry(line).map_err(|message| ImportError::Csv {
            path: path.to_owned(),
            row: index + 1,
            message,
        })?;
        entries.push(entry);
    }
    Ok(entries)
}

/// One line of a listing: jq's `@tsv`, so a field's tab, line break or
/// backslash is escaped and a null is empty.
fn listing_entry(line: &str) -> Result<Entry, String> {
    let fields: Vec<String> = line.split('\t').map(unescape_tsv).collect();
    if !(9..=10).contains(&fields.len()) {
        return Err(format!(
            "{} fields, not the 9 (or 10, with a fee) of wallet, label, address, category, \
             amount, blocktime, confirmations, txid, vout",
            fields.len()
        ));
    }
    let number = |at: usize, name: &str| -> Result<Option<i64>, String> {
        let field = fields[at].trim();
        if field.is_empty() {
            return Ok(None);
        }
        field
            .parse()
            .map(Some)
            .map_err(|_| format!("bad {name} {field:?}"))
    };
    let amount = |at: usize| btc_to_msat(fields[at].trim());
    let txid = fields[7].trim().to_owned();
    if txid.is_empty() {
        return Err("no txid".to_owned());
    }
    let fee = match fields.get(9).map(|f| f.trim()) {
        None | Some("") => None,
        Some(_) => Some(amount(9).ok_or_else(|| format!("bad fee {:?}", fields[9]))?),
    };
    Ok(Entry {
        msat: amount(4).ok_or_else(|| format!("bad amount {:?}", fields[4]))?,
        fee,
        blocktime: number(5, "blocktime")?.filter(|&t| t > 0).map(|t| t as u64),
        confirmations: number(6, "confirmations")?.unwrap_or(0),
        vout: number(8, "vout")?
            .unwrap_or(0)
            .try_into()
            .map_err(|_| format!("bad vout {:?}", fields[8]))?,
        wallet: fields[0].clone(),
        label: fields[1].clone(),
        address: fields[2].clone(),
        category: fields[3].trim().to_owned(),
        abandoned: false,
        txid,
    })
}

/// Undo jq's `@tsv` escapes: `\t`, `\n`, `\r` and `\\`.
fn unescape_tsv(field: &str) -> String {
    if !field.contains('\\') {
        return field.to_owned();
    }
    let mut out = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// A decimal number of bitcoin, such as `-0.00012300`, `1e-08` or `21e6`,
/// as exact msat. `None` when it isn't a number, needs a fraction of a msat,
/// or doesn't fit.
fn btc_to_msat(text: &str) -> Option<i64> {
    let (negative, rest) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let (number, exponent) = match rest.find(['e', 'E']) {
        Some(at) => (&rest[..at], rest[at + 1..].parse::<i32>().ok()?),
        None => (rest, 0),
    };
    let (whole, frac) = number.split_once('.').unwrap_or((number, ""));
    if whole.is_empty() && frac.is_empty() {
        return None;
    }
    if !whole
        .chars()
        .chain(frac.chars())
        .all(|c| c.is_ascii_digit())
    {
        return None;
    }
    // The digits as one integer, scaled by 10^shift to msat.
    let digits = format!("{whole}{frac}");
    let digits = digits.trim_start_matches('0');
    let shift = MSAT_DIGITS as i64 + exponent as i64 - frac.len() as i64;
    let mut msat: i128 = if digits.is_empty() {
        0
    } else {
        if digits.len() > 30 {
            return None;
        }
        digits.parse().ok()?
    };
    if shift >= 0 {
        for _ in 0..shift {
            msat = msat.checked_mul(10)?;
            if msat > i64::MAX as i128 {
                return None;
            }
        }
    } else {
        for _ in 0..-shift {
            if msat % 10 != 0 {
                return None;
            }
            msat /= 10;
        }
    }
    let msat = i64::try_from(msat).ok()?;
    Some(if negative { -msat } else { msat })
}

/// Percent-encode a wallet name for its `/wallet/<name>` path.
fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The ledger account of `wallet`: `assets:btc:<wallet>`, with bitcoind's
/// unnamed default wallet as `default` and a `:` or whitespace, which would
/// split or break the name, as `-`.
fn wallet_account(wallet: &str) -> String {
    let name: String = wallet
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
        .replace(':', "-");
    let name = if name.is_empty() { "default" } else { &name };
    format!("assets:btc:{name}")
}

/// [`txns`]'s result.
struct Txns {
    txns: Vec<RawTxn>,
    left_out: Vec<LeftOut>,
}

/// Turn every wallet's entries into transactions, as the module docs say,
/// in block order.
fn txns(source: &str, entries: Vec<Entry>) -> Txns {
    // Overlapping pages, or a listing read twice, repeat an entry; keep one.
    let mut by_txid: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
    for entry in entries {
        let group = by_txid.entry(entry.txid.clone()).or_default();
        let repeat = group.iter().any(|e| {
            e.wallet == entry.wallet && e.category == entry.category && e.vout == entry.vout
        });
        if !repeat {
            group.push(entry);
        }
    }

    let mut timed: Vec<Timed> = Vec::new();
    let mut left_out = Vec::new();
    for (txid, group) in by_txid {
        if let Some(why) = group.iter().find_map(Entry::left_out) {
            left_out.push(LeftOut {
                id: format!("{source}:{txid}"),
                why,
            });
            continue;
        }
        let blocktime = group.iter().filter_map(|e| e.blocktime).min().unwrap_or(0);
        let made = if group.iter().any(Entry::is_send) {
            vec![spend(source, &txid, &group)]
        } else {
            // Two wallets watching one address both list its output; the
            // coins are counted once, in the first.
            let mut vouts = Vec::new();
            group
                .iter()
                .filter(|e| {
                    let first = !vouts.contains(&e.vout);
                    vouts.push(e.vout);
                    first
                })
                .map(|e| receipt(source, e))
                .collect()
        };
        timed.extend(
            made.into_iter()
                .flatten()
                .map(|txn| Timed { blocktime, txn }),
        );
    }
    timed.sort_by(|a, b| (a.blocktime, &a.txn.id).cmp(&(b.blocktime, &b.txn.id)));
    Txns {
        txns: timed.into_iter().map(|t| t.txn).collect(),
        left_out,
    }
}

/// A transaction and the time of its block, to sort by.
struct Timed {
    blocktime: u64,
    txn: RawTxn,
}

/// The one transaction of a txid some read wallet sent: transfers between
/// the read wallets, other outputs, and each sending wallet's fee. `None`
/// when it moves nothing, a zero-fee send to itself.
fn spend(source: &str, txid: &str, group: &[Entry]) -> Option<RawTxn> {
    let mut legs = Legs::default();
    let mut matched = vec![false; group.len()];
    let mut feed: Vec<&str> = Vec::new();
    let mut transfers = false;
    let mut external = false;
    for send in group.iter().filter(|e| e.is_send()) {
        let to = group.iter().enumerate().position(|(at, e)| {
            !matched[at] && !e.is_send() && e.vout == send.vout && e.msat == -send.msat
        });
        let from = wallet_account(&send.wallet);
        match to {
            Some(at) => {
                matched[at] = true;
                if group[at].wallet != send.wallet {
                    transfers = true;
                    legs.add(&from, send.msat);
                    legs.add(&wallet_account(&group[at].wallet), group[at].msat);
                }
            }
            None => {
                external = true;
                legs.add(&from, send.msat);
                legs.add(SPENT, -send.msat);
            }
        }
        if let Some(fee) = send.fee.filter(|_| !feed.contains(&send.wallet.as_str())) {
            feed.push(&send.wallet);
            legs.add(&from, fee);
            // A positive "fee" is not one: it takes back the share of the
            // sends that someone else's inputs paid for (see [`Entry::fee`]).
            legs.add(if fee < 0 { FEES } else { SPENT }, -fee);
        }
    }
    for (at, entry) in group.iter().enumerate() {
        if entry.is_send() || matched[at] {
            continue;
        }
        external = true;
        legs.add(&wallet_account(&entry.wallet), entry.msat);
        legs.add(RECEIVED, -entry.msat);
    }
    let kind = match (transfers, external) {
        (true, false) => "transfer",
        _ => "send",
    };
    let postings = legs.postings();
    if postings.len() < 2 {
        return None;
    }
    Some(RawTxn {
        date: date_of(group),
        cleared: true,
        payee: payee(group, kind),
        id: format!("{source}:{txid}"),
        meta: meta(txid, group.iter()),
        postings,
    })
}

/// The transaction of one output received from outside: a payment in, or
/// a mined coin.
fn receipt(source: &str, entry: &Entry) -> Option<RawTxn> {
    if entry.msat == 0 {
        return None;
    }
    let kind = match entry.category.as_str() {
        "generate" | "immature" => "mined",
        _ => "receive",
    };
    let group = std::slice::from_ref(entry);
    let mut legs = Legs::default();
    legs.add(&wallet_account(&entry.wallet), entry.msat);
    legs.add(RECEIVED, -entry.msat);
    Some(RawTxn {
        date: date_of(group),
        cleared: true,
        payee: payee(group, kind),
        id: format!("{source}:{}:{}", entry.txid, entry.vout),
        meta: meta(&entry.txid, group.iter()),
        postings: legs.postings(),
    })
}

/// The UTC day of the group's block.
fn date_of(group: &[Entry]) -> Date {
    let secs = group.iter().filter_map(|e| e.blocktime).min().unwrap_or(0);
    Date::from_days((secs / 86_400).min(u64::from(u32::MAX)) as u32)
}

/// The first label in the group, or `bitcoin <kind>` when none has one.
fn payee(group: &[Entry], kind: &str) -> String {
    group
        .iter()
        .map(|e| e.label.trim())
        .find(|label| !label.is_empty())
        .map_or_else(|| format!("bitcoin {kind}"), str::to_owned)
}

/// The `raw-*` lines of a transaction made from `entries`: each field's
/// distinct values, joined by `, ` when there are several.
fn meta<'a>(txid: &str, entries: impl Iterator<Item = &'a Entry> + Clone) -> Vec<RawMeta> {
    let line = |key: &str, value: String| RawMeta {
        key: key.to_owned(),
        value,
    };
    let joined = |field: fn(&Entry) -> &str| {
        let mut seen: Vec<&str> = Vec::new();
        for value in entries.clone().map(field) {
            if !value.trim().is_empty() && !seen.contains(&value) {
                seen.push(value);
            }
        }
        seen.join(", ")
    };
    let mut meta = vec![line("raw-txid", txid.to_owned())];
    for (key, field) in [
        ("raw-wallet", (|e| &e.wallet) as fn(&Entry) -> &str),
        ("raw-category", |e| &e.category),
        ("raw-label", |e| &e.label),
        ("raw-address", |e| &e.address),
    ] {
        let value = joined(field);
        if !value.is_empty() {
            meta.push(line(key, value));
        }
    }
    meta
}

/// A transaction's postings, summed per account in the order each account
/// first appears.
#[derive(Default)]
struct Legs(Vec<Leg>);

struct Leg {
    account: String,
    msat: i64,
}

impl Legs {
    fn add(&mut self, account: &str, msat: i64) {
        match self.0.iter_mut().find(|leg| leg.account == account) {
            Some(leg) => leg.msat += msat,
            None => self.0.push(Leg {
                account: account.to_owned(),
                msat,
            }),
        }
    }

    /// The postings, leaving out an account that nets to nothing.
    fn postings(self) -> Vec<RawPosting> {
        self.0
            .into_iter()
            .filter(|leg| leg.msat != 0)
            .map(|leg| RawPosting {
                account: leg.account,
                amount: Some(RawAmount {
                    qty: leg.msat,
                    precision: 0,
                    commodity: MSAT.to_owned(),
                }),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::{self, Rows};
    use ledgerdb::{JournalStore, LayerFilter, Ledger, WriteOptions};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A bitcoind holding `wallets`, each a list of `listtransactions`
    /// entries oldest first, as JSON text so amounts stay exact. It answers
    /// `listtransactions` pages as bitcoind does: `skip` counts back from
    /// the newest, and a page is in time order.
    struct FakeNode {
        wallets: Vec<(String, Vec<String>)>,
        /// Every path and method called, in order.
        calls: Rc<RefCell<Vec<String>>>,
    }

    impl Rpc for FakeNode {
        fn call(
            &self,
            wallet: Option<&str>,
            method: &str,
            params: Value,
        ) -> Result<Box<RawValue>, RpcError> {
            let path = wallet.map_or_else(String::new, |name| format!("/wallet/{}", encode(name)));
            self.calls.borrow_mut().push(format!("{path} {method}"));
            let result = match (wallet, method) {
                (None, "listwallets") => {
                    let names: Vec<_> = self.wallets.iter().map(|(n, _)| n.clone()).collect();
                    serde_json::to_string(&names).unwrap()
                }
                (Some(wallet), "listtransactions") => {
                    let Some((_, listed)) = self.wallets.iter().find(|(name, _)| name == wallet)
                    else {
                        return Err(RpcError::Rpc {
                            code: -18,
                            message: "Requested wallet does not exist or is not loaded".to_owned(),
                        });
                    };
                    let count = params[1].as_u64().unwrap() as usize;
                    let skip = params[2].as_u64().unwrap() as usize;
                    let end = listed.len().saturating_sub(skip);
                    let start = end.saturating_sub(count);
                    format!("[{}]", listed[start..end].join(","))
                }
                _ => panic!("unexpected call {path} {method}"),
            };
            Ok(RawValue::from_string(result).unwrap())
        }
    }

    /// An endpoint that fails every call with what `fail` makes.
    struct Failing(fn() -> RpcError);

    impl Rpc for Failing {
        fn call(&self, _: Option<&str>, _: &str, _: Value) -> Result<Box<RawValue>, RpcError> {
            Err((self.0)())
        }
    }

    /// A CLN node whose corerpc answers from `bitcoind`, its numbers
    /// passed through a double as corerpc's JavaScript and lnsocket's
    /// parsed `Value` pass them. Every corerpc params array is logged.
    struct CoreNode {
        bitcoind: FakeNode,
        params: Rc<RefCell<Vec<Value>>>,
    }

    impl CoreNode {
        fn new(bitcoind: FakeNode) -> Self {
            Self {
                bitcoind,
                params: Rc::default(),
            }
        }
    }

    impl Node for CoreNode {
        fn call(&self, method: &str, params: Value) -> Result<Value, NodeError> {
            assert_eq!(method, "corerpc");
            self.params.borrow_mut().push(params.clone());
            let Value::Array(mut args) = params else {
                panic!("corerpc params not an array: {params}");
            };
            let rest = args.split_off(2);
            let wallet = args[0].as_str().unwrap();
            let method = args[1].as_str().unwrap();
            let wallet = (!wallet.is_empty()).then_some(wallet);
            match self.bitcoind.call(wallet, method, Value::Array(rest)) {
                Ok(raw) => Ok(through_double(serde_json::from_str(raw.get()).unwrap())),
                Err(RpcError::Rpc { code, message }) => Err(NodeError::Rpc { code, message }),
                Err(error) => Err(NodeError::Other(error.to_string())),
            }
        }
    }

    /// `value` with every number made a double and written as JavaScript
    /// writes one (a whole number without a fraction), then read as a
    /// `Value` without `arbitrary_precision` reads it.
    fn through_double(value: Value) -> Value {
        match value {
            Value::Number(n) => {
                let double = n.as_f64().unwrap();
                if double.fract() == 0.0 && double.abs() < 1e15 {
                    json!(double as i64)
                } else {
                    json!(double)
                }
            }
            Value::Array(items) => Value::Array(items.into_iter().map(through_double).collect()),
            Value::Object(fields) => Value::Object(
                fields
                    .into_iter()
                    .map(|(k, v)| (k, through_double(v)))
                    .collect(),
            ),
            other => other,
        }
    }

    /// A node that fails every call with `error`.
    struct DownNode(NodeError);

    impl Node for DownNode {
        fn call(&self, _: &str, _: Value) -> Result<Value, NodeError> {
            Err(self.0.clone())
        }
    }

    /// A node a test expects never to be called.
    struct Untouched;

    impl Node for Untouched {
        fn call(&self, method: &str, params: Value) -> Result<Value, NodeError> {
            panic!("fell back to corerpc: {method} {params}");
        }
    }

    /// `hot` paying `cold` with a fee, and a payment in: the wallets the
    /// backend tests read both ways.
    fn two_wallets() -> FakeNode {
        let fee = Some("-0.00012345");
        let hot = vec![
            listed("receive", "1.00000000", None, "funding", 1, DAY1),
            listed("send", "-0.50000000", fee, "move", 0, DAY2),
            listed("send", "-0.20000000", fee, "move", 1, DAY2),
        ];
        let cold = vec![listed("receive", "0.50000000", None, "move", 0, DAY2)];
        fake_node(&[("hot", hot), ("cold", cold)])
    }

    /// `config` with `[btc] via` set.
    fn via(via: BtcVia) -> BtcConfig {
        BtcConfig {
            via: Some(via),
            ..BtcConfig::default()
        }
    }

    /// One `listtransactions` entry, mined at `blocktime` (unconfirmed when
    /// 0). `amount` and `fee` are written into the JSON as given.
    fn listed(
        category: &str,
        amount: &str,
        fee: Option<&str>,
        txid: &str,
        vout: u32,
        blocktime: u64,
    ) -> String {
        let fee = fee.map_or_else(String::new, |fee| format!(r#""fee":{fee},"#));
        let (confirmations, blocktime) = match blocktime {
            0 => (0, String::new()),
            t => (6, format!(r#""blocktime":{t},"#)),
        };
        format!(
            r#"{{"address":"bc1qsynthetic{vout}","category":"{category}","amount":{amount},{fee}"label":"","vout":{vout},"confirmations":{confirmations},{blocktime}"txid":"{txid}","time":1}}"#
        )
    }

    /// A node holding `wallets`, with an empty call log.
    fn fake_node(wallets: &[(&str, Vec<String>)]) -> FakeNode {
        FakeNode {
            wallets: wallets
                .iter()
                .map(|(name, listed)| (name.to_string(), listed.clone()))
                .collect(),
            calls: Rc::default(),
        }
    }

    fn input() -> ImportInput {
        ImportInput {
            source: "btc".to_owned(),
            paths: Vec::new(),
        }
    }

    /// 2024-03-05 00:00 UTC, and the next day.
    const DAY1: u64 = 1_709_596_800;
    const DAY2: u64 = DAY1 + 86_400;

    fn ledger_text(batch: &Batch) -> String {
        batch.journal.to_ledger(WriteOptions::default())
    }

    /// A `user` with no password secret says how to store one.
    #[test]
    fn a_user_without_its_password_names_the_command() {
        if std::env::var(secret_env_var(PASSWORD_SECRET)).is_ok() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let config = BtcConfig {
            user: Some("rpcuser".to_owned()),
            ..BtcConfig::default()
        };
        let importer = BtcImporter::new(Some(&config), None, &CashflowDir::new(tmp.path()));
        let Some(RpcError::NoCredentials(message)) = importer.connect().err() else {
            panic!("expected missing credentials");
        };
        assert!(
            message.contains("cashflow secret set bitcoind-rpc-password"),
            "{message}"
        );
        let Err(ImportError::Usage(shown)) = importer.import(&input()) else {
            panic!("expected a usage error");
        };
        // With no [cln] to fall back to, the error says what one needs.
        assert_eq!(
            shown,
            format!(
                "{message}; to read the wallets through the CLN node's corerpc over commando \
                 instead: no `node_id` under [cln] in config.toml: run `cashflow config set \
                 cln.node_id <66-hex node id>`"
            )
        );

        let http = BtcConfig {
            via: Some(BtcVia::Http),
            ..config
        };
        let importer = BtcImporter::new(Some(&http), None, &CashflowDir::new(tmp.path()));
        let Err(ImportError::Usage(shown)) = importer.import(&input()) else {
            panic!("expected a usage error");
        };
        assert_eq!(shown, message);
    }

    /// A cookie file that isn't there is missing credentials, not a failed
    /// read, so a fallback can take over.
    #[test]
    fn a_missing_cookie_is_no_credentials() {
        let tmp = tempfile::tempdir().unwrap();
        let cookie = tmp.path().join(".cookie");
        let config = BtcConfig {
            cookie: Some(cookie.clone()),
            ..BtcConfig::default()
        };
        let importer = BtcImporter::new(Some(&config), None, &CashflowDir::new(tmp.path()));
        let Some(RpcError::NoCredentials(message)) = importer.connect().err() else {
            panic!("expected missing credentials");
        };
        assert!(
            message.starts_with(&format!("reading {}: ", cookie.display())),
            "{message}"
        );
    }

    /// Each kind of failure keeps its kind through `call` and names the
    /// call in what the user sees.
    #[test]
    fn call_keeps_each_failure_kind() {
        let kinds: [fn() -> RpcError; 5] = [
            || RpcError::Unreachable("Connection refused".to_owned()),
            || RpcError::NoCredentials("no cookie".to_owned()),
            || RpcError::Unauthorized("401 Unauthorized".to_owned()),
            || RpcError::Rpc {
                code: -18,
                message: "not loaded".to_owned(),
            },
            || RpcError::Other("no result in the answer".to_owned()),
        ];
        for make in kinds {
            let failed = call::<Vec<String>>(&Failing(make), Some("hot"), "listwallets", json!([]))
                .unwrap_err();
            assert_eq!(failed.from, "bitcoind listwallets (wallet \"hot\")");
            assert_eq!(
                std::mem::discriminant(&failed.error),
                std::mem::discriminant(&make()),
                "{:?}",
                failed.error
            );
            assert_eq!(
                ImportError::from(failed).to_string(),
                format!("bitcoind listwallets (wallet \"hot\"): {}", make())
            );
        }
        assert_eq!(
            RpcError::Rpc {
                code: -18,
                message: "not loaded".to_owned()
            }
            .to_string(),
            "not loaded (code -18)"
        );

        // A result of the wrong shape is reported with its text.
        let node = fake_node(&[]);
        let failed = call::<u32>(&node, None, "listwallets", json!([])).unwrap_err();
        let RpcError::Other(message) = failed.error else {
            panic!("expected other");
        };
        assert!(message.ends_with(": []"), "{message}");
    }

    /// A port nothing listens on is unreachable, which a fallback takes as
    /// leave to try another way.
    #[test]
    fn a_closed_port_is_unreachable() {
        let http = BitcoindHttp::new("http://127.0.0.1:1", "user:password");
        let error = http.call(None, "listwallets", json!([])).unwrap_err();
        assert!(matches!(error, RpcError::Unreachable(_)), "{error:?}");
    }

    /// bitcoind's answer: an error under any status, a result kept as its
    /// own text so amounts stay exact, and anything else as other.
    #[test]
    fn an_answer_is_its_result_or_its_error() {
        let result =
            result_of(br#"{"result":[{"amount":0.10000000}],"error":null,"id":"cashflow"}"#)
                .unwrap();
        assert_eq!(result.get(), r#"[{"amount":0.10000000}]"#);

        let error = result_of(
            br#"{"result":null,"error":{"code":-18,"message":"Requested wallet does not exist or is not loaded"},"id":"cashflow"}"#,
        )
        .unwrap_err();
        let RpcError::Rpc { code, message } = error else {
            panic!("expected an rpc error, got {error:?}");
        };
        assert_eq!(code, -18);
        assert_eq!(message, "Requested wallet does not exist or is not loaded");

        let error = result_of(br#"{"result":null,"error":null,"id":"cashflow"}"#).unwrap_err();
        assert!(
            matches!(&error, RpcError::Other(m) if m == "no result in the answer"),
            "{error:?}"
        );

        let error = result_of(b"<html>Not Found</html>").unwrap_err();
        assert!(
            matches!(&error, RpcError::Other(m) if m.ends_with(": <html>Not Found</html>")),
            "{error:?}"
        );
    }

    #[test]
    fn decimal_amounts_are_exact_msat() {
        assert_eq!(btc_to_msat("0.00000001"), Some(1_000));
        assert_eq!(btc_to_msat("1e-08"), Some(1_000));
        assert_eq!(btc_to_msat("1E-8"), Some(1_000));
        assert_eq!(btc_to_msat("-0.10000000"), Some(-10_000_000_000));
        assert_eq!(btc_to_msat("21e6"), Some(2_100_000_000_000_000_000));
        assert_eq!(
            btc_to_msat("21000000.00000000"),
            Some(2_100_000_000_000_000_000)
        );
        // Where `btc * 1e11` in an f64 goes wrong.
        assert_eq!(
            btc_to_msat("20999999.99999999"),
            Some(2_099_999_999_999_999_000)
        );
        assert_eq!(btc_to_msat("0.29"), Some(29_000_000_000));
        assert_eq!(btc_to_msat("0.000000001"), Some(100));
        assert_eq!(btc_to_msat("0"), Some(0));
        assert_eq!(btc_to_msat("-0.0"), Some(0));
        // A fraction of a msat, a number too big, and not numbers.
        assert_eq!(btc_to_msat("0.000000000001"), None);
        assert_eq!(btc_to_msat("1e9"), None);
        for bad in ["", ".", "-", "abc", "1.2.3", "1e", "0x10", "1 0"] {
            assert_eq!(btc_to_msat(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_wallet_is_paged_until_exhausted() {
        // One more entry than a page (and one unconfirmed), so it takes a
        // second call, which comes back short.
        let mut many: Vec<String> = (0..PAGE as u32 + 1)
            .map(|n| {
                listed(
                    "receive",
                    "0.00000001",
                    None,
                    &format!("t{n:04}"),
                    0,
                    DAY1 + u64::from(n),
                )
            })
            .collect();
        many.push(listed("receive", "0.00000002", None, "unconfirmed", 0, 0));
        let node = fake_node(&[("big", many)]);
        let calls = node.calls.clone();
        let batch = BtcImporter::with_rpc(None, Box::new(node))
            .import(&input())
            .unwrap();

        assert_eq!(batch.txn_count(), PAGE + 1);
        assert_eq!(
            *calls.borrow(),
            [
                " listwallets",
                "/wallet/big listtransactions",
                "/wallet/big listtransactions",
            ]
        );
        assert_eq!(
            batch.left_out,
            [LeftOut {
                id: "btc:unconfirmed".to_owned(),
                why: "unconfirmed"
            }]
        );
    }

    #[test]
    fn a_move_between_wallets_is_one_transfer_with_its_fee() {
        // `hot` pays 0.5 to `cold` and 0.2 to someone else in one
        // transaction; bitcoind repeats the fee on both send entries.
        let fee = Some("-0.00012345");
        let hot = vec![
            listed("receive", "1.00000000", None, "funding", 1, DAY1),
            listed("send", "-0.50000000", fee, "move", 0, DAY2),
            listed("send", "-0.20000000", fee, "move", 1, DAY2),
        ];
        let cold = vec![listed("receive", "0.50000000", None, "move", 0, DAY2)];
        let node = fake_node(&[("hot", hot), ("cold", cold)]);
        let batch = BtcImporter::with_rpc(None, Box::new(node))
            .import(&input())
            .unwrap();

        assert_eq!(
            ledger_text(&batch),
            "2024-03-05 * bitcoin receive\n\
             \t; id: btc:funding:1\n\
             \t; raw-txid: funding\n\
             \t; raw-wallet: hot\n\
             \t; raw-category: receive\n\
             \t; raw-address: bc1qsynthetic1\n\
             \tassets:btc:hot\t100000000000 msat\n\
             \tincome:unknown\t-100000000000 msat\n\n\
             2024-03-06 * bitcoin send\n\
             \t; id: btc:move\n\
             \t; raw-txid: move\n\
             \t; raw-wallet: hot, cold\n\
             \t; raw-category: send, receive\n\
             \t; raw-address: bc1qsynthetic0, bc1qsynthetic1\n\
             \tassets:btc:hot\t-70012345000 msat\n\
             \tassets:btc:cold\t50000000000 msat\n\
             \texpenses:fees:btc\t12345000 msat\n\
             \texpenses:unknown\t20000000000 msat\n"
        );
    }

    #[test]
    fn a_pure_transfer_has_no_unknown_leg() {
        let hot = vec![listed("send", "-0.5", Some("-0.0001"), "move", 0, DAY1)];
        let cold = vec![listed("receive", "0.5", None, "move", 0, DAY1)];
        let node = fake_node(&[("hot", hot), ("cold wallet", cold)]);
        let batch = BtcImporter::with_rpc(None, Box::new(node))
            .import(&input())
            .unwrap();
        let text = ledger_text(&batch);
        assert!(
            text.starts_with("2024-03-05 * bitcoin transfer\n"),
            "{text}"
        );
        assert!(
            text.contains("\tassets:btc:cold-wallet\t50000000000 msat\n"),
            "{text}"
        );
        assert!(!text.contains("unknown"), "{text}");
    }

    #[test]
    fn a_send_to_itself_is_only_its_fee() {
        let hot = vec![
            listed("send", "-0.5", Some("-0.0001"), "self", 0, DAY1),
            listed("receive", "0.5", None, "self", 0, DAY1),
        ];
        let node = fake_node(&[("hot", hot)]);
        let batch = BtcImporter::with_rpc(None, Box::new(node))
            .import(&input())
            .unwrap();
        let text = ledger_text(&batch);
        assert!(
            text.ends_with(
                "\tassets:btc:hot\t-10000000 msat\n\texpenses:fees:btc\t10000000 msat\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn a_send_partly_funded_by_others_is_not_a_fee() {
        // The wallet put in 0.3 of a 0.5 output; bitcoind lists the send at
        // -0.5 with a "fee" of +0.2.
        let hot = vec![listed("send", "-0.5", Some("0.2"), "dual", 0, DAY1)];
        let node = fake_node(&[("hot", hot)]);
        let batch = BtcImporter::with_rpc(None, Box::new(node))
            .import(&input())
            .unwrap();
        let text = ledger_text(&batch);
        assert!(
            text.ends_with(
                "\tassets:btc:hot\t-30000000000 msat\n\texpenses:unknown\t30000000000 msat\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn named_wallets_skip_listwallets_and_an_rpc_error_says_which() {
        let node = fake_node(&[(
            "hot",
            vec![listed("generate", "6.25", None, "coinbase", 0, DAY1)],
        )]);
        let calls = node.calls.clone();
        let config = BtcConfig {
            wallets: Some(vec!["hot".to_owned()]),
            ..BtcConfig::default()
        };
        let batch = BtcImporter::with_rpc(Some(&config), Box::new(node))
            .import(&input())
            .unwrap();
        assert!(
            ledger_text(&batch).starts_with("2024-03-05 * bitcoin mined\n\t; id: btc:coinbase:0\n")
        );
        assert_eq!(*calls.borrow(), ["/wallet/hot listtransactions"]);

        let node = fake_node(&[]);
        let config = BtcConfig {
            wallets: Some(vec!["gone".to_owned()]),
            ..BtcConfig::default()
        };
        let err = BtcImporter::with_rpc(Some(&config), Box::new(node))
            .import(&input())
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "bitcoind listtransactions (wallet \"gone\"): Requested wallet does not exist or is not loaded (code -18)"
        );
    }

    #[test]
    fn conflicted_and_abandoned_are_left_out() {
        let conflicted = listed("receive", "1", None, "lost", 0, DAY1)
            .replace(r#""confirmations":6"#, r#""confirmations":-3"#);
        let abandoned = listed("send", "-1", Some("-0.001"), "dropped", 0, 0)
            .replace(r#""time":1"#, r#""time":1,"abandoned":true"#);
        let node = fake_node(&[(
            "hot",
            vec![
                conflicted,
                abandoned,
                listed("receive", "1", None, "kept", 0, DAY1),
            ],
        )]);
        let batch = BtcImporter::with_rpc(None, Box::new(node))
            .import(&input())
            .unwrap();
        assert_eq!(batch.txn_count(), 1);
        let why: Vec<_> = batch
            .left_out
            .iter()
            .map(|l| (l.id.as_str(), l.why))
            .collect();
        assert_eq!(
            why,
            [("btc:dropped", "abandoned"), ("btc:lost", "conflicted")]
        );
    }

    #[test]
    fn a_tsv_listing_reads_like_the_rpc() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("txs.tsv");
        std::fs::write(
            &file,
            format!(
                "hot\tsome\\tlabel\tbc1qa\treceive\t1e-08\t{DAY1}\t10\tt1\t0\n\
                 \n\
                 hot\t\tbc1qb\tsend\t-0.5\t{DAY2}\t9\tt2\t1\t-0.0001\n\
                 cold\t\tbc1qb\treceive\t0.5\t{DAY2}\t9\tt2\t1\n\
                 hot\t\tbc1qc\treceive\t2\t\t0\tt3\t0\n"
            ),
        )
        .unwrap();
        let importer = BtcImporter::new(None, None, &CashflowDir::new(tmp.path()));
        let batch = importer
            .import(&ImportInput {
                source: "btc".to_owned(),
                paths: vec![file.clone()],
            })
            .unwrap();
        let text = ledger_text(&batch);
        assert!(
            text.contains("2024-03-05 * some label\n\t; id: btc:t1:0\n"),
            "{text}"
        );
        assert!(text.contains("\tassets:btc:hot\t1000 msat\n"), "{text}");
        assert!(
            text.contains("2024-03-06 * bitcoin transfer\n\t; id: btc:t2\n"),
            "{text}"
        );
        assert!(
            text.contains("\texpenses:fees:btc\t10000000 msat\n"),
            "{text}"
        );
        assert_eq!(batch.left_out.len(), 1);

        std::fs::write(&file, "hot\tx\n").unwrap();
        let err = importer
            .import(&ImportInput {
                source: "btc".to_owned(),
                paths: vec![file],
            })
            .unwrap_err();
        assert!(err.to_string().contains("row 1: 2 fields"), "{err}");
    }

    #[test]
    fn a_reimport_writes_nothing_and_reports_what_it_left_out() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        let wallets = [(
            "hot",
            vec![
                listed("receive", "0.1", None, "in", 0, DAY1),
                listed("receive", "0.2", None, "pending", 0, 0),
            ],
        )];
        let run = |imported| {
            let node = fake_node(&wallets);
            let importer = BtcImporter::with_rpc(None, Box::new(node));
            import::write(
                &store,
                &Default::default(),
                &importer,
                &input(),
                None,
                Some(imported),
            )
            .unwrap()
        };

        let first = run(7);
        assert!(first.new);
        assert_eq!(first.header.layer, "btc-onchain");
        assert_eq!(first.left_out.len(), 1);
        let again = run(8);
        assert_eq!(again.hash, None);
        assert_eq!(
            again.txn_rows,
            Rows {
                new: 0,
                changed: 0,
                unchanged: 1
            }
        );
        let ledger = Ledger::load(&store, LayerFilter::All).unwrap().ledger;
        assert!(ledger.find("btc:in:0").is_some());
    }

    #[test]
    fn wallet_names_make_valid_accounts_and_paths() {
        assert_eq!(wallet_account(""), "assets:btc:default");
        assert_eq!(wallet_account("cold wallet"), "assets:btc:cold-wallet");
        assert_eq!(wallet_account("a:b"), "assets:btc:a-b");
        assert_eq!(encode("my wallet/1"), "my%20wallet%2F1");
        assert_eq!(encode(""), "");
    }

    /// The same wallets read through corerpc as over HTTP give the same
    /// records, and corerpc's first param is the wallet.
    #[test]
    fn corerpc_reads_what_http_reads() {
        let http = BtcImporter::with_rpc(Some(&via(BtcVia::Http)), Box::new(two_wallets()))
            .import(&input())
            .unwrap();

        let node = CoreNode::new(two_wallets());
        let params = node.params.clone();
        let corerpc = BtcImporter::with_rpc(
            Some(&via(BtcVia::Corerpc)),
            Box::new(Failing(|| panic!("called over HTTP"))),
        )
        .with_node(Box::new(node))
        .import(&input())
        .unwrap();

        assert_eq!(ledger_text(&corerpc), ledger_text(&http));
        assert!(corerpc.notes.is_empty(), "{:?}", corerpc.notes);
        assert_eq!(
            *params.borrow(),
            [
                json!(["", "listwallets"]),
                json!(["hot", "listtransactions", "*", PAGE, 0, false]),
                json!(["cold", "listtransactions", "*", PAGE, 0, false]),
            ]
        );
    }

    /// Auto falls back when bitcoind can't be reached, reads the whole run
    /// through corerpc, and says so.
    #[test]
    fn auto_falls_back_when_bitcoind_is_unreachable() {
        let http = BtcImporter::with_rpc(None, Box::new(two_wallets()))
            .import(&input())
            .unwrap();
        let batch = BtcImporter::with_rpc(
            None,
            Box::new(Failing(|| {
                RpcError::Unreachable("Connection refused".to_owned())
            })),
        )
        .with_node(Box::new(CoreNode::new(two_wallets())))
        .import(&input())
        .unwrap();
        assert_eq!(ledger_text(&batch), ledger_text(&http));
        assert_eq!(
            batch.notes,
            [
                "bitcoind's RPC at http://127.0.0.1:8332 couldn't be reached: Connection refused, \
                 so wallets were read through corerpc over commando"
            ]
        );
    }

    /// Auto falls back when there is nothing to sign in to bitcoind with.
    #[test]
    fn auto_falls_back_without_credentials() {
        let tmp = tempfile::tempdir().unwrap();
        let config = BtcConfig {
            url: Some("http://monad:8332".to_owned()),
            cookie: Some(tmp.path().join(".cookie")),
            ..BtcConfig::default()
        };
        let batch = BtcImporter::new(Some(&config), None, &CashflowDir::new(tmp.path()))
            .with_node(Box::new(CoreNode::new(two_wallets())))
            .import(&input())
            .unwrap();
        assert_eq!(batch.txn_count(), 2);
        assert_eq!(
            batch.notes,
            [
                "bitcoind's RPC at http://monad:8332 has no credentials, so wallets were read \
                 through corerpc over commando"
            ]
        );
    }

    /// Wrong credentials and an error bitcoind answers with are real
    /// errors, not leave to try corerpc; nor is bitcoind going away after
    /// it has answered, since a run never mixes backends.
    #[test]
    fn auto_does_not_fall_back_once_bitcoind_answers() {
        let kinds: [fn() -> RpcError; 2] = [
            || RpcError::Unauthorized("401 Unauthorized".to_owned()),
            || RpcError::Rpc {
                code: -18,
                message: "not loaded".to_owned(),
            },
        ];
        for make in kinds {
            let error = BtcImporter::with_rpc(None, Box::new(Failing(make)))
                .with_node(Box::new(Untouched))
                .import(&input())
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("bitcoind listwallets: {}", make())
            );
        }

        /// A bitcoind that lists its wallets, then goes away.
        struct Vanishing(FakeNode);
        impl Rpc for Vanishing {
            fn call(
                &self,
                wallet: Option<&str>,
                method: &str,
                params: Value,
            ) -> Result<Box<RawValue>, RpcError> {
                match method {
                    "listwallets" => self.0.call(wallet, method, params),
                    _ => Err(RpcError::Unreachable("Connection reset".to_owned())),
                }
            }
        }
        let error = BtcImporter::with_rpc(None, Box::new(Vanishing(two_wallets())))
            .with_node(Box::new(Untouched))
            .import(&input())
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "bitcoind listtransactions (wallet \"hot\"): Connection reset"
        );
    }

    /// An unreachable bitcoind with no [cln] to fall back to fails as it
    /// did, naming what the fallback needs.
    #[test]
    fn no_cln_names_what_the_fallback_needs() {
        let error = BtcImporter::with_rpc(
            None,
            Box::new(Failing(|| {
                RpcError::Unreachable("Connection refused".to_owned())
            })),
        )
        .import(&input())
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "bitcoind listwallets: Connection refused; to read the wallets through the CLN \
             node's corerpc over commando instead: no `node_id` under [cln] in config.toml: run \
             `cashflow config set cln.node_id <66-hex node id>`"
        );
    }

    /// A rune that doesn't permit the call says how to mint one that does;
    /// a node that can't be reached is unreachable.
    #[test]
    fn corerpc_failures_keep_their_kind() {
        let denied = DownNode(NodeError::Rpc {
            code: cln::RUNE_DENIED,
            message: "Not authorized: method not permitted".to_owned(),
        });
        let error = Corerpc::new(&denied)
            .call(None, "listwallets", json!([]))
            .unwrap_err();
        let RpcError::Rpc { code, message } = &error else {
            panic!("expected an rpc error, got {error:?}");
        };
        assert_eq!(*code, cln::RUNE_DENIED);
        assert!(message.contains("lightning-cli createrune"), "{message}");

        let error = BtcImporter::with_rpc(Some(&via(BtcVia::Corerpc)), Box::new(two_wallets()))
            .with_node(Box::new(denied))
            .import(&input())
            .unwrap_err();
        assert!(error.to_string().contains("createrune"), "{error}");

        let down = DownNode(NodeError::Other(
            "connecting to monad:9735: refused".to_owned(),
        ));
        let error = Corerpc::new(&down)
            .call(Some("hot"), "listtransactions", json!([]))
            .unwrap_err();
        assert!(matches!(error, RpcError::Unreachable(_)), "{error:?}");

        let error = Corerpc::new(&down)
            .call(None, "listwallets", json!({}))
            .unwrap_err();
        assert!(matches!(error, RpcError::Other(_)), "{error:?}");
    }

    /// An amount through a double, written back out as its shortest form,
    /// reads as the same msat as bitcoind's own text: corerpc's reads are
    /// exact to 15 significant digits.
    #[test]
    fn amounts_through_a_double_are_exact() {
        for text in [
            "0.00000001",
            "0.00000546",
            "-0.00012300",
            "1.23456789",
            "20999999.97690000",
            "-0.00001410",
            "21000000.00000000",
            "0.29",
        ] {
            let exact = btc_to_msat(text).unwrap();
            let double = json!(text.parse::<f64>().unwrap()).to_string();
            assert_eq!(btc_to_msat(&double), Some(exact), "{text} as {double}");
            let parsed = through_double(serde_json::from_str(text).unwrap()).to_string();
            assert_eq!(btc_to_msat(&parsed), Some(exact), "{text} as {parsed}");
        }
    }
}
