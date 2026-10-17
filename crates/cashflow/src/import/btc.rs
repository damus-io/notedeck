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
//! **Amounts are exact.** bitcoind writes an amount as a decimal number of
//! bitcoin, read from the JSON text itself (never through an `f64`) into
//! integer msat, the commodity amounts are written in. An entry not yet in a
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

use std::collections::BTreeMap;
use std::path::Path;

use base64::Engine;
use ledgerdb::Date;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::value::RawValue;
use serde_json::{Value, json};

use super::{
    Batch, ImportError, ImportInput, Importer, LeftOut, RawAmount, RawMeta, RawPosting, RawTxn,
};
use crate::CashflowDir;
use crate::config::{BtcConfig, secret_env_var};
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

/// A bitcoind JSON-RPC endpoint, so the importer can be tested without one.
pub trait Rpc {
    /// POST `body` (a JSON-RPC request) to `path` under the endpoint, `""`
    /// for the node or `/wallet/<name>` for a wallet, and return the body of
    /// the answer, whatever its status: bitcoind reports an RPC error as JSON
    /// under an HTTP error.
    fn post(&self, path: &str, body: &str) -> Result<Vec<u8>, String>;
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
    fn post(&self, path: &str, body: &str) -> Result<Vec<u8>, String> {
        let url = format!("{}{path}", self.url);
        let mut request = ehttp::Request::post(url, body.as_bytes().to_vec());
        request.headers.insert("Authorization", &self.authorization);
        request.headers.insert("Content-Type", "application/json");
        let response = ehttp::fetch_blocking(&request)?;
        if response.status == 401 {
            return Err("401 Unauthorized: check [btc] user and the bitcoind-rpc-password secret, or the cookie".to_owned());
        }
        Ok(response.bytes)
    }
}

/// Reads on-chain bitcoin wallets from bitcoind, or from `btc-txs-raw`
/// listings.
pub struct BtcImporter {
    config: BtcConfig,
    /// Where the RPC password secret is read from, when the import runs.
    dir: CashflowDir,
    /// The endpoint to use instead of connecting with `config`, for tests.
    rpc: Option<Box<dyn Rpc>>,
}

impl BtcImporter {
    /// The importer as `config.toml`'s `[btc]` table sets it up, reading its
    /// secret from `dir` when it runs.
    pub fn new(config: Option<&BtcConfig>, dir: &CashflowDir) -> Self {
        Self {
            config: config.cloned().unwrap_or_default(),
            dir: dir.clone(),
            rpc: None,
        }
    }

    /// The importer reading from `rpc` rather than connecting.
    pub fn with_rpc(config: Option<&BtcConfig>, rpc: Box<dyn Rpc>) -> Self {
        Self {
            rpc: Some(rpc),
            ..Self::new(config, &CashflowDir::new(""))
        }
    }

    /// The endpoint the config names, signed in with its credentials.
    fn connect(&self) -> Result<BitcoindHttp, ImportError> {
        let url = self.config.url.as_deref().unwrap_or(DEFAULT_URL);
        let credentials = match &self.config.user {
            Some(user) => {
                let password = self
                    .dir
                    .secret(PASSWORD_SECRET)
                    .map_err(|e| ImportError::Usage(e.to_string()))?
                    .ok_or_else(|| {
                        ImportError::Usage(format!(
                            "[btc] user is set but its password is not: set ${} or write \
                             it to {}",
                            secret_env_var(PASSWORD_SECRET),
                            self.dir.secrets().join(PASSWORD_SECRET).display()
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
                    return Err(ImportError::Usage(
                        "no bitcoind credentials: set [btc] user or cookie in config.toml"
                            .to_owned(),
                    ));
                };
                std::fs::read_to_string(&cookie)
                    .map_err(|source| ImportError::Read {
                        path: cookie,
                        source,
                    })?
                    .trim_end()
                    .to_owned()
            }
        };
        Ok(BitcoindHttp::new(url, &credentials))
    }

    /// Every entry of every wallet the config names, over `rpc`.
    fn fetch(&self, rpc: &dyn Rpc) -> Result<Vec<Entry>, ImportError> {
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
        let entries = if !input.paths.is_empty() {
            let mut entries = Vec::new();
            for path in &input.paths {
                entries.extend(read_listing(path)?);
            }
            entries
        } else if let Some(rpc) = &self.rpc {
            self.fetch(&**rpc)?
        } else {
            self.fetch(&self.connect()?)?
        };
        let Txns { txns, left_out } = txns(&input.source, entries);
        Ok(Batch {
            left_out,
            ..Batch::from_raw(&txns, &[])?
        })
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

/// Call `method` with `params` on the node (`wallet` `None`) or a wallet,
/// and read its result as `T`.
fn call<T: DeserializeOwned>(
    rpc: &dyn Rpc,
    wallet: Option<&str>,
    method: &str,
    params: Value,
) -> Result<T, ImportError> {
    let from = match wallet {
        Some(name) => format!("bitcoind {method} (wallet {name:?})"),
        None => format!("bitcoind {method}"),
    };
    let fail = |error: String| ImportError::Fetch {
        from: from.clone(),
        error,
    };
    let path = wallet.map_or_else(String::new, |name| format!("/wallet/{}", encode(name)));
    let body = json!({"jsonrpc": "1.0", "id": "cashflow", "method": method, "params": params});
    let answer = rpc.post(&path, &body.to_string()).map_err(fail)?;
    let answer: Answer<T> = serde_json::from_slice(&answer).map_err(|e| {
        let text = String::from_utf8_lossy(&answer);
        fail(format!(
            "{e}: {}",
            text.chars().take(200).collect::<String>()
        ))
    })?;
    if let Some(error) = answer.error {
        return Err(fail(format!("{} (code {})", error.message, error.code)));
    }
    answer
        .result
        .ok_or_else(|| fail("no result in the answer".to_owned()))
}

/// A JSON-RPC answer.
#[derive(Deserialize)]
struct Answer<T> {
    result: Option<T>,
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
        fn post(&self, path: &str, body: &str) -> Result<Vec<u8>, String> {
            let request: Value = serde_json::from_str(body).unwrap();
            let method = request["method"].as_str().unwrap();
            self.calls.borrow_mut().push(format!("{path} {method}"));
            let result = match (path, method) {
                ("", "listwallets") => {
                    let names: Vec<_> = self.wallets.iter().map(|(n, _)| n.clone()).collect();
                    serde_json::to_string(&names).unwrap()
                }
                (path, "listtransactions") => {
                    let Some((_, listed)) = self
                        .wallets
                        .iter()
                        .find(|(name, _)| format!("/wallet/{}", encode(name)) == path)
                    else {
                        return Ok(br#"{"result":null,"error":{"code":-18,"message":"Requested wallet does not exist or is not loaded"}}"#.to_vec());
                    };
                    let count = request["params"][1].as_u64().unwrap() as usize;
                    let skip = request["params"][2].as_u64().unwrap() as usize;
                    let end = listed.len().saturating_sub(skip);
                    let start = end.saturating_sub(count);
                    format!("[{}]", listed[start..end].join(","))
                }
                _ => panic!("unexpected call {path} {method}"),
            };
            Ok(format!(r#"{{"result":{result},"error":null,"id":"cashflow"}}"#).into_bytes())
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
        let importer = BtcImporter::new(None, &CashflowDir::new(tmp.path()));
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
            import::write(&store, &importer, &input(), None, Some(imported)).unwrap()
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
}
