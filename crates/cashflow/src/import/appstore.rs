//! The `appstore` importer: App Store Connect financial reports, the
//! proceeds Apple actually pays out, for every app and in-app purchase.
//!
//! Given files it reads reports downloaded from App Store Connect
//! (*Payments and Financial Reports*), plain or gzipped; a directory reads its
//! `*.txt`, `*.tsv` and `*.gz` files in name order. With no files it fetches
//! them from the App Store Connect API, `GET /v1/financeReports` with
//! `reportType=FINANCIAL` and `regionCode=ZZ` (every region), for each month
//! named on the command line (`2026-03`, or a range `2025-10..2026-03`) or,
//! with none, the twelve months before this one. A month Apple has no report
//! for yet is left out (see [`super::LeftOut`]), and a later import takes it.
//!
//! A report is tab-separated, its columns found by their header names. Apple
//! spells some of them two ways (`Sales or Return`, `Sale or Return`) and
//! closes the file with `Total_Rows`, `Total_Amount` and `Total_Units` lines;
//! an all-regions report may add a summary section headed `Country Of Sale`,
//! and a header can repeat. The reader passes over all of that:
//!
//! ```text
//! Start Date  End Date  UPC  ISRC/ISBN  Vendor Identifier  Quantity  Partner Share  Extended Partner Share  Partner Share Currency  Sales or Return  Apple Identifier  …  Title  …  Product Type Identifier  …  Country Of Sale  …
//! 03/29/2026  05/02/2026      com.example.pro  com.example.pro  3  2.10  6.30  USD  S  1000000001  …  Example Pro  …  IAY  …  US  …
//! Total_Rows  1
//! ```
//!
//! **One transaction per (period, product, currency, product type).** A
//! report has a row per country and price; those are summed, so a month of a
//! subscription sold in thirty USD countries is one transaction of its
//! `Extended Partner Share`, the proceeds after Apple's commission and taxes.
//! It posts to `assets:apple` (proceeds Apple owes until it pays them out)
//! and `income:unknown`, in the report's currency; turning that into CAD is
//! the price db's job. Which app a product belongs to is a rule on
//! `raw-app-id`, never this code. A return row (`R`) counts against the
//! sales, whichever way Apple signed it.
//!
//! **Periods are Apple's fiscal months**, which neither start nor end with
//! the calendar month, so a transaction is dated the period's `End Date` and
//! its id names that date: `<source>:<vendor>:<end>:<apple-id>:<currency>:
//! <product-type>`. That is the same whether the report was downloaded by
//! hand or fetched, and whatever month label it was asked for under.
//!
//! The API signs in with an ES256 JWT made from the `appstore.p8` secret,
//! `[appstore] key_id` and `issuer`; the token is never written anywhere.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::PathBuf;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ledgerdb::Date;
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair};
use serde_json::json;

use super::csv_rows::{Header, RowError, files_with, parse_cents, parse_mdy, read_bytes, squeeze};
use super::{
    Batch, ImportError, ImportInput, Importer, LeftOut, RawAmount, RawMeta, RawPosting, RawTxn,
};
use crate::CashflowDir;
use crate::config::{AppStoreConfig, secret_env_var};

/// The secret holding the App Store Connect API key, the `.p8` file Apple
/// lets you download once.
pub const KEY_SECRET: &str = "appstore.p8";

/// The finance reports endpoint.
const API: &str = "https://api.appstoreconnect.apple.com/v1/financeReports";

/// Where proceeds go when [`AppStoreConfig::account`] is unset.
const DEFAULT_ACCOUNT: &str = "assets:apple";

/// The counter leg.
const RECEIVED: &str = "income:unknown";

/// Every region in one report.
const DEFAULT_REGION: &str = "ZZ";

/// How many months a run with no months named fetches, ending last month.
const DEFAULT_MONTHS: u32 = 12;

/// How long a token is good for: twenty minutes, the most Apple accepts.
const TOKEN_LIFETIME: u64 = 20 * 60;

/// The extensions a directory's report files have.
const REPORT_EXTENSIONS: &[&str] = &["txt", "tsv", "gz"];

/// Where finance reports come from, so the importer can be tested without
/// App Store Connect.
pub trait ReportSource {
    /// The financial report for fiscal month `month` (`YYYY-MM`) as Apple
    /// sends it (usually gzipped), or `None` when Apple has none for it.
    fn report(&self, month: &str) -> Result<Option<Vec<u8>>, String>;
}

/// [`ReportSource`] over `ehttp`, signed in with a token.
pub struct AppStoreHttp {
    vendor: String,
    region: String,
    /// `Bearer <jwt>`.
    authorization: String,
}

impl ReportSource for AppStoreHttp {
    fn report(&self, month: &str) -> Result<Option<Vec<u8>>, String> {
        let url = format!(
            "{API}?filter%5BregionCode%5D={}&filter%5BreportDate%5D={month}\
             &filter%5BreportType%5D=FINANCIAL&filter%5BvendorNumber%5D={}",
            self.region, self.vendor
        );
        let mut request = ehttp::Request::get(url);
        request.headers.insert("Authorization", &self.authorization);
        request.headers.insert("Accept", "application/a-gzip");
        let response = ehttp::fetch_blocking(&request)?;
        match response.status {
            200 => Ok(Some(response.bytes)),
            404 => Ok(None),
            status => Err(format!(
                "{status} {}: {}",
                response.status_text,
                api_error(&response.bytes)
            )),
        }
    }
}

/// The `detail` of the first error in an App Store Connect error answer, or
/// the start of the body when it isn't one.
fn api_error(body: &[u8]) -> String {
    let detail = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["errors"][0]["detail"].as_str().map(str::to_owned));
    detail.unwrap_or_else(|| String::from_utf8_lossy(body).chars().take(200).collect())
}

/// What a token is made of, besides the key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenClaims<'a> {
    /// The key's id, `kid` in the header.
    pub key_id: &'a str,
    /// The issuer id, `iss`.
    pub issuer: &'a str,
    /// Unix seconds, `iat`; the token expires [`TOKEN_LIFETIME`] later.
    pub now: u64,
}

/// An App Store Connect API token: an ES256 JWT signed with the PKCS#8 key
/// in `key_pem` (a `.p8` file's text).
pub fn token(key_pem: &str, claims: &TokenClaims) -> Result<String, String> {
    let der = pem_body(key_pem)?;
    let rng = SystemRandom::new();
    let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &der, &rng)
        .map_err(|e| format!("the {KEY_SECRET} key is not a P-256 PKCS#8 key: {e}"))?;
    let header = json!({"alg": "ES256", "kid": claims.key_id, "typ": "JWT"});
    let payload = json!({
        "iss": claims.issuer,
        "iat": claims.now,
        "exp": claims.now + TOKEN_LIFETIME,
        "aud": "appstoreconnect-v1",
    });
    let signed = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string()),
        URL_SAFE_NO_PAD.encode(payload.to_string())
    );
    let signature = key
        .sign(&rng, signed.as_bytes())
        .map_err(|_| "signing the token failed".to_owned())?;
    Ok(format!("{signed}.{}", URL_SAFE_NO_PAD.encode(signature)))
}

/// The DER bytes of a PEM file: its base64 lines between the `-----` ones.
fn pem_body(pem: &str) -> Result<Vec<u8>, String> {
    let body: String = pem
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("-----"))
        .collect();
    STANDARD
        .decode(body)
        .map_err(|e| format!("the {KEY_SECRET} key is not PEM: {e}"))
}

/// Reads App Store Connect financial reports, from files or the API.
pub struct AppStoreImporter {
    config: AppStoreConfig,
    /// Where the key secret is read from, when the import fetches.
    dir: CashflowDir,
    /// The reports to read instead of connecting, for tests.
    source: Option<Box<dyn ReportSource>>,
    /// Today, for the default months, or the clock's when `None`.
    today: Option<Date>,
}

impl AppStoreImporter {
    /// The importer as `config.toml`'s `[appstore]` table sets it up,
    /// reading its key from `dir` when it fetches.
    pub fn new(config: Option<&AppStoreConfig>, dir: &CashflowDir) -> Self {
        Self {
            config: config.cloned().unwrap_or_default(),
            dir: dir.clone(),
            source: None,
            today: None,
        }
    }

    /// The importer fetching from `source` rather than connecting, as if
    /// it were `today`.
    pub fn with_source(
        config: Option<&AppStoreConfig>,
        source: Box<dyn ReportSource>,
        today: Date,
    ) -> Self {
        Self {
            source: Some(source),
            today: Some(today),
            ..Self::new(config, &CashflowDir::new(""))
        }
    }

    /// The vendor number, which every record id carries.
    fn vendor(&self) -> Result<&str, ImportError> {
        self.config.vendor.as_deref().ok_or_else(|| {
            ImportError::Usage(
                "no `vendor` under [appstore] in config.toml: run `cashflow config set \
                 appstore.vendor <number>` with your vendor number, from App Store Connect's \
                 Payments and Financial Reports"
                    .to_owned(),
            )
        })
    }

    /// The API, signed in with the config's key.
    fn connect(&self, vendor: &str) -> Result<AppStoreHttp, ImportError> {
        let need = |key: &str| {
            ImportError::Usage(format!(
                "fetching reports needs `{key}` under [appstore] in config.toml: run \
                 `cashflow config set appstore.{key} <id>` (or give report files instead)"
            ))
        };
        let key_id = self
            .config
            .key_id
            .as_deref()
            .ok_or_else(|| need("key_id"))?;
        let issuer = self
            .config
            .issuer
            .as_deref()
            .ok_or_else(|| need("issuer"))?;
        let key = self
            .dir
            .secret(KEY_SECRET)
            .map_err(|e| ImportError::Usage(e.to_string()))?
            .ok_or_else(|| {
                ImportError::Usage(format!(
                    "no App Store Connect key: store the .p8 file with `cashflow secret set \
                     {KEY_SECRET} < AuthKey_<key id>.p8` (or set ${})",
                    secret_env_var(KEY_SECRET),
                ))
            })?;
        let claims = TokenClaims {
            key_id,
            issuer,
            now: super::now(),
        };
        let jwt = token(key.expose(), &claims).map_err(ImportError::Usage)?;
        Ok(AppStoreHttp {
            vendor: vendor.to_owned(),
            region: self
                .config
                .region
                .clone()
                .unwrap_or_else(|| DEFAULT_REGION.to_owned()),
            authorization: format!("Bearer {jwt}"),
        })
    }
}

impl Importer for AppStoreImporter {
    fn name(&self) -> &'static str {
        "appstore"
    }

    fn layer(&self) -> &'static str {
        "appstore"
    }

    fn import(&self, input: &ImportInput) -> Result<Batch, ImportError> {
        let vendor = self.vendor()?;
        let Args { files, months } = split_args(&input.paths)?;
        if !files.is_empty() && !months.is_empty() {
            return Err(ImportError::Usage(
                "give report files or months to fetch, not both".to_owned(),
            ));
        }
        let mut reports = Reports::default();
        for path in files_with(&files, REPORT_EXTENSIONS)? {
            let text = report_text(read_bytes(&path)?)
                .map_err(|message| RowError { row: 1, message }.in_file(&path))?;
            let rows = read_report(&text).map_err(|error| error.in_file(&path))?;
            reports.add(text, rows);
        }
        if files.is_empty() {
            let months = if months.is_empty() {
                default_months(self.today.unwrap_or_else(today))
            } else {
                months
            };
            let prefix = format!("{}:{vendor}", input.source);
            match &self.source {
                Some(source) => fetch(&**source, &months, &prefix, &mut reports)?,
                None => fetch(&self.connect(vendor)?, &months, &prefix, &mut reports)?,
            }
        }
        let account = self.config.account.as_deref().unwrap_or(DEFAULT_ACCOUNT);
        let mut left_out = reports.left_out;
        let mut txns = Vec::new();
        for (key, group) in reports.groups {
            let id = key.id(&input.source, vendor);
            if group.cents == 0 {
                left_out.push(LeftOut {
                    id,
                    why: "no proceeds",
                });
                continue;
            }
            txns.push(raw_txn(id, account, key, group));
        }
        Ok(Batch {
            left_out,
            ..Batch::from_raw(&txns, &[])?
        })
    }
}

/// Add every month's report from `source` to `reports`, and leave out
/// each month it has none for, as `<prefix>:<month>`.
fn fetch(
    source: &dyn ReportSource,
    months: &[String],
    prefix: &str,
    reports: &mut Reports,
) -> Result<(), ImportError> {
    for month in months {
        let fail = |error| ImportError::Fetch {
            from: format!("App Store Connect financeReports {month}"),
            error,
        };
        let Some(body) = source.report(month).map_err(fail)? else {
            reports.left_out.push(LeftOut {
                id: format!("{prefix}:{month}"),
                why: "no report",
            });
            continue;
        };
        let text = report_text(body).map_err(fail)?;
        let rows = read_report(&text).map_err(|e| fail(format!("row {}: {}", e.row, e.message)))?;
        reports.add(text, rows);
    }
    Ok(())
}

/// The command line's paths, split into report files and months to fetch.
struct Args {
    files: Vec<PathBuf>,
    /// `YYYY-MM` each, in order.
    months: Vec<String>,
}

/// Each of `paths` that exists is a file or directory of reports; any other
/// must be a month, `YYYY-MM`, or an inclusive range of them,
/// `YYYY-MM..YYYY-MM`.
fn split_args(paths: &[PathBuf]) -> Result<Args, ImportError> {
    let mut args = Args {
        files: Vec::new(),
        months: Vec::new(),
    };
    for path in paths {
        if path.exists() {
            args.files.push(path.clone());
            continue;
        }
        let text = path.to_string_lossy();
        let range = match text.split_once("..") {
            Some((from, to)) => month_index(from).zip(month_index(to)),
            None => month_index(&text).map(|m| (m, m)),
        };
        let Some((from, to)) = range.filter(|(from, to)| from <= to) else {
            return Err(ImportError::Read {
                path: path.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no such file, and not a month (YYYY-MM) or range (YYYY-MM..YYYY-MM)",
                ),
            });
        };
        args.months.extend((from..=to).map(month_label));
    }
    Ok(args)
}

/// `YYYY-MM` as a [`Date::month_index`].
fn month_index(text: &str) -> Option<u32> {
    let (year, month) = text.split_once('-')?;
    if year.len() != 4 || month.len() != 2 {
        return None;
    }
    let year: u32 = year.parse().ok()?;
    let month: u32 = month.parse().ok()?;
    (1..=12).contains(&month).then_some(year * 12 + month - 1)
}

/// A [`Date::month_index`] as `YYYY-MM`.
fn month_label(index: u32) -> String {
    format!("{:04}-{:02}", index / 12, index % 12 + 1)
}

/// The [`DEFAULT_MONTHS`] months before `today`'s.
fn default_months(today: Date) -> Vec<String> {
    let this = today.month_index();
    (this - DEFAULT_MONTHS..this).map(month_label).collect()
}

/// Today, by the clock.
fn today() -> Date {
    Date::from_days((super::now() / 86_400) as u32)
}

/// A report's text: gunzipped when it is gzip, without a byte-order mark.
fn report_text(bytes: Vec<u8>) -> Result<String, String> {
    let bytes = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::MultiGzDecoder::new(&bytes[..])
            .read_to_end(&mut out)
            .map_err(|e| format!("not a gzip file: {e}"))?;
        out
    } else {
        bytes
    };
    let text = String::from_utf8(bytes).map_err(|e| format!("not UTF-8 text: {e}"))?;
    Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned())
}

/// The reports read so far, summed into groups.
#[derive(Default)]
struct Reports {
    /// Each report's text, so the same report given twice counts once.
    seen: BTreeSet<String>,
    groups: BTreeMap<Key, Group>,
    /// The months fetched that had no report.
    left_out: Vec<LeftOut>,
}

impl Reports {
    /// Sum `rows`, read from report `text`, into the groups, unless the
    /// same report was added already.
    fn add(&mut self, text: String, rows: Vec<Row>) {
        if !self.seen.insert(text) {
            return;
        }
        for row in rows {
            let key = Key {
                end: row.end,
                apple_id: row.apple_id,
                currency: row.currency,
                product_type: row.product_type,
            };
            let group = self.groups.entry(key).or_insert_with(|| Group {
                start: row.start,
                sku: row.sku,
                title: row.title,
                cents: 0,
                units: 0,
                countries: BTreeSet::new(),
            });
            group.start = group.start.min(row.start);
            group.cents += row.cents;
            group.units += row.units;
            if !row.country.is_empty() {
                group.countries.insert(row.country);
            }
        }
    }
}

/// What one transaction sums: the rows sharing these.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    /// The period's last day, the transaction's date.
    end: Date,
    apple_id: String,
    currency: String,
    product_type: String,
}

impl Key {
    /// The record id under `source` for `vendor`.
    fn id(&self, source: &str, vendor: &str) -> String {
        format!(
            "{source}:{vendor}:{}:{}:{}:{}",
            self.end, self.apple_id, self.currency, self.product_type
        )
    }
}

/// The rows of one [`Key`], summed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Group {
    /// The period's first day.
    start: Date,
    /// `Vendor Identifier`: the app's SKU or the in-app purchase's product
    /// id.
    sku: String,
    title: String,
    /// Proceeds, in hundredths of the currency.
    cents: i64,
    /// Units sold less units returned.
    units: i64,
    countries: BTreeSet<String>,
}

/// `group` as a raw transaction with record id `id`, posting to `account`.
fn raw_txn(id: String, account: &str, key: Key, group: Group) -> RawTxn {
    let amount = |qty| {
        Some(RawAmount {
            qty,
            precision: 2,
            commodity: key.currency.clone(),
        })
    };
    let meta = [
        ("raw-app-id", key.apple_id.clone()),
        ("raw-sku", group.sku),
        ("raw-title", group.title.clone()),
        ("raw-product-type", key.product_type.clone()),
        ("raw-country", Vec::from_iter(group.countries).join(",")),
        ("raw-units", group.units.to_string()),
        ("raw-period", format!("{}..{}", group.start, key.end)),
    ]
    .into_iter()
    .filter(|(_, value)| !value.is_empty())
    .map(|(key, value)| RawMeta {
        key: key.to_owned(),
        value,
    })
    .collect();
    RawTxn {
        date: key.end,
        cleared: true,
        payee: group.title,
        id,
        meta,
        postings: vec![
            RawPosting {
                account: account.to_owned(),
                amount: amount(group.cents),
            },
            RawPosting {
                account: RECEIVED.to_owned(),
                amount: amount(-group.cents),
            },
        ],
    }
}

/// One data row of a report, as read.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    start: Date,
    end: Date,
    sku: String,
    /// Signed: a return's is negative.
    units: i64,
    /// `Extended Partner Share` in hundredths, signed like `units`.
    cents: i64,
    currency: String,
    apple_id: String,
    title: String,
    product_type: String,
    country: String,
}

/// Where each column the importer reads sits in a row.
struct Columns {
    start: usize,
    end: usize,
    sku: usize,
    quantity: usize,
    extended: usize,
    currency: usize,
    sale_or_return: usize,
    apple_id: usize,
    title: usize,
    product_type: usize,
    country: usize,
}

/// The column names the importer reads, for the error when one is missing.
const EXPECTED: &str = "Start Date, End Date, Vendor Identifier, Quantity, Extended Partner \
                        Share, Partner Share Currency, Sales or Return, Apple Identifier, Title, \
                        Product Type Identifier, Country Of Sale";

impl Columns {
    /// Find the columns by name in the `header` row, on line `line`.
    fn find(header: &Header, line: usize) -> Result<Self, RowError> {
        let at_line = |mut error: RowError| {
            error.row = line;
            error
        };
        let need = |name| header.need(name).map_err(at_line);
        Ok(Self {
            start: need("Start Date")?,
            end: need("End Date")?,
            sku: need("Vendor Identifier")?,
            quantity: need("Quantity")?,
            extended: need("Extended Partner Share")?,
            currency: need("Partner Share Currency")?,
            sale_or_return: header
                .at("Sale or Return")
                .map_or_else(|| need("Sales or Return"), Ok)?,
            apple_id: need("Apple Identifier")?,
            title: need("Title")?,
            product_type: need("Product Type Identifier")?,
            country: need("Country Of Sale")?,
        })
    }
}

/// Whether the trimmed `fields` are a report's column header.
fn is_header(fields: &[&str]) -> bool {
    let has = |name: &str| fields.iter().any(|f| f.eq_ignore_ascii_case(name));
    has("Extended Partner Share") && has("Apple Identifier")
}

/// Every data row of a report, in its order. `row` in an error is the line.
///
/// Blank lines, the `Total_*` footer and any `Country Of Sale` summary
/// section are passed over; any other line must read, so nothing is
/// silently dropped.
fn read_report(text: &str) -> Result<Vec<Row>, RowError> {
    let mut columns: Option<Columns> = None;
    let mut in_summary = false;
    let mut rows = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line_no = index + 1;
        let fields: Vec<&str> = line.split('\t').map(str::trim).collect();
        let first = fields[0];
        if fields.iter().all(|f| f.is_empty()) || first.starts_with("Total_") {
            continue;
        }
        if is_header(&fields) {
            let header = Header::new(fields.iter().map(|f| f.to_string()).collect(), EXPECTED);
            columns = Some(Columns::find(&header, line_no)?);
            in_summary = false;
            continue;
        }
        if first.eq_ignore_ascii_case("Country Of Sale") {
            in_summary = true;
        }
        if in_summary {
            continue;
        }
        let error = |message: String| RowError {
            row: line_no,
            message,
        };
        let Some(columns) = &columns else {
            return Err(error(format!(
                "a row before any header; expected a header with {EXPECTED}"
            )));
        };
        rows.push(read_row(&fields, columns).map_err(error)?);
    }
    Ok(rows)
}

/// One data row's `fields`.
fn read_row(fields: &[&str], columns: &Columns) -> Result<Row, String> {
    let field = |at: usize| fields.get(at).copied().unwrap_or("");
    let date = |at: usize| parse_mdy(field(at)).ok_or_else(|| format!("bad date {:?}", field(at)));
    let units: i64 = field(columns.quantity)
        .replace(',', "")
        .parse()
        .map_err(|_| format!("bad quantity {:?}", field(columns.quantity)))?;
    let cents = parse_cents(field(columns.extended))
        .ok_or_else(|| format!("bad amount {:?}", field(columns.extended)))?;
    // A return counts against the sales whether or not Apple signed it.
    let returned = match field(columns.sale_or_return) {
        "S" => false,
        "R" => true,
        other => return Err(format!("sales or return {other:?}, not S or R")),
    };
    let signed = |n: i64| if returned { -n.abs() } else { n };
    let currency = field(columns.currency);
    if currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(format!("bad currency {currency:?}"));
    }
    let apple_id = field(columns.apple_id);
    if apple_id.is_empty() {
        return Err("no Apple Identifier".to_owned());
    }
    let product_type = field(columns.product_type);
    if product_type.is_empty() || product_type.contains(char::is_whitespace) {
        return Err(format!("bad product type {product_type:?}"));
    }
    Ok(Row {
        start: date(columns.start)?,
        end: date(columns.end)?,
        sku: squeeze(field(columns.sku)),
        units: signed(units),
        cents: signed(cents),
        currency: currency.to_owned(),
        apple_id: apple_id.to_owned(),
        title: squeeze(field(columns.title)),
        product_type: product_type.to_owned(),
        country: field(columns.country).to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::Write;
    use std::path::Path;

    use ledgerdb::WriteOptions;
    use ring::signature::{ECDSA_P256_SHA256_FIXED, KeyPair, UnparsedPublicKey};

    /// A synthetic vendor number.
    const VENDOR: &str = "80000001";

    /// The header as Apple writes it, `Sales or Return` spelling and all.
    const HEADER: &str = "Start Date\tEnd Date\tUPC\tISRC/ISBN\tVendor Identifier\tQuantity\t\
                          Partner Share\tExtended Partner Share\tPartner Share Currency\t\
                          Sales or Return\tApple Identifier\tArtist/Show/Developer/Author\tTitle\t\
                          Label/Studio/Network/Developer/Publisher\tGrid\t\
                          Product Type Identifier\tISAN/Other Identifier\tCountry Of Sale\t\
                          Pre-order Flag\tPromo Code\tCustomer Price\tCustomer Currency\n";

    /// One data row's fields, written by [`Line::tsv`]. Struct-update from
    /// [`PRO`] to vary a few.
    struct Line {
        apple_id: &'static str,
        sku: &'static str,
        qty: &'static str,
        ext: &'static str,
        currency: &'static str,
        sale_or_return: &'static str,
        product_type: &'static str,
        country: &'static str,
    }

    /// A synthetic subscription sold once in the US.
    const PRO: Line = Line {
        apple_id: "1000000001",
        sku: "com.example.pro",
        qty: "1",
        ext: "2.10",
        currency: "USD",
        sale_or_return: "S",
        product_type: "IAY",
        country: "US",
    };

    impl Line {
        /// The row as a report line, under [`HEADER`].
        fn tsv(&self) -> String {
            let Line {
                apple_id: id,
                sku,
                qty,
                ext,
                currency: cur,
                sale_or_return: sr,
                product_type: ptype,
                country,
            } = self;
            format!(
                "03/29/2026\t05/02/2026\t\t{sku}\t{sku}\t{qty}\t2.10\t{ext}\t{cur}\t{sr}\t{id}\t\
                 Example Dev\tTitle {id}\t\t\t{ptype}\t\t{country}\t\t\t2.99\t{cur}\n"
            )
        }
    }

    /// A month's all-regions report: one subscription sold in two USD
    /// countries with a refund, the same subscription in EUR, a paid app,
    /// a free row with no proceeds, then the summary section and footer.
    fn report() -> String {
        let app = Line {
            apple_id: "1000000002",
            sku: "example.app",
            ext: "0.70",
            product_type: "1F",
            ..PRO
        };
        [
            HEADER.to_owned(),
            Line {
                qty: "3",
                ext: "6.30",
                ..PRO
            }
            .tsv(),
            Line {
                qty: "2",
                ext: "4.20",
                country: "EC",
                ..PRO
            }
            .tsv(),
            Line {
                qty: "-1",
                ext: "-2.10",
                sale_or_return: "R",
                ..PRO
            }
            .tsv(),
            Line {
                qty: "4",
                ext: "7.60",
                currency: "EUR",
                country: "DE",
                ..PRO
            }
            .tsv(),
            app.tsv(),
            Line {
                apple_id: "1000000003",
                sku: "example.free",
                qty: "5",
                ext: "0.00",
                ..app
            }
            .tsv(),
            "\n".to_owned(),
            "Country Of Sale\tPartner Share Currency\tQuantity\tExtended Partner Share\n"
                .to_owned(),
            "US\tUSD\t8\t4.90\n".to_owned(),
            "Total_Rows\t6\n".to_owned(),
            "Total_Amount\t16.70\n".to_owned(),
            "Total_Units\t14\n".to_owned(),
        ]
        .concat()
    }

    /// [`report`] as the importer writes it.
    const LEDGER: &str = "\
2026-05-02 * Title 1000000001
\t; id: appstore:80000001:2026-05-02:1000000001:EUR:IAY
\t; raw-app-id: 1000000001
\t; raw-sku: com.example.pro
\t; raw-title: Title 1000000001
\t; raw-product-type: IAY
\t; raw-country: DE
\t; raw-units: 4
\t; raw-period: 2026-03-29..2026-05-02
\tassets:apple\t7.60 EUR
\tincome:unknown\t-7.60 EUR

2026-05-02 * Title 1000000001
\t; id: appstore:80000001:2026-05-02:1000000001:USD:IAY
\t; raw-app-id: 1000000001
\t; raw-sku: com.example.pro
\t; raw-title: Title 1000000001
\t; raw-product-type: IAY
\t; raw-country: EC,US
\t; raw-units: 4
\t; raw-period: 2026-03-29..2026-05-02
\tassets:apple\t8.40 USD
\tincome:unknown\t-8.40 USD

2026-05-02 * Title 1000000002
\t; id: appstore:80000001:2026-05-02:1000000002:USD:1F
\t; raw-app-id: 1000000002
\t; raw-sku: example.app
\t; raw-title: Title 1000000002
\t; raw-product-type: 1F
\t; raw-country: US
\t; raw-units: 1
\t; raw-period: 2026-03-29..2026-05-02
\tassets:apple\t0.70 USD
\tincome:unknown\t-0.70 USD
";

    fn config() -> AppStoreConfig {
        AppStoreConfig {
            vendor: Some(VENDOR.to_owned()),
            ..AppStoreConfig::default()
        }
    }

    fn input(paths: Vec<PathBuf>) -> ImportInput {
        ImportInput {
            source: "appstore".to_owned(),
            paths,
        }
    }

    fn gzip(text: &str) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(text.as_bytes()).unwrap();
        encoder.finish().unwrap()
    }

    fn file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn import_files(paths: Vec<PathBuf>) -> Result<Batch, ImportError> {
        AppStoreImporter::new(Some(&config()), &CashflowDir::new("unused")).import(&input(paths))
    }

    fn ledger(batch: &Batch) -> String {
        batch.journal.to_ledger(WriteOptions::default())
    }

    fn date(text: &str) -> Date {
        Date::parse(text).unwrap()
    }

    #[test]
    fn rows_are_summed_per_product_currency_and_type() {
        let tmp = tempfile::tempdir().unwrap();
        let batch = import_files(vec![file(tmp.path(), "r.txt", report().as_bytes())]).unwrap();
        assert_eq!(ledger(&batch), LEDGER);
        assert_eq!(
            batch.left_out,
            [LeftOut {
                id: "appstore:80000001:2026-05-02:1000000003:USD:1F".to_owned(),
                why: "no proceeds",
            }]
        );
    }

    /// The same report gzipped, with a byte-order mark and the other
    /// spelling of `Sale or Return`, reads the same, and given twice
    /// counts once.
    #[test]
    fn gzip_bom_and_header_spellings_read_the_same() {
        let tmp = tempfile::tempdir().unwrap();
        let other = format!(
            "\u{feff}{}",
            report().replace("Sales or Return", "Sale or Return")
        );
        let gz = file(tmp.path(), "r.txt.gz", &gzip(&other));
        assert_eq!(ledger(&import_files(vec![gz.clone()]).unwrap()), LEDGER);

        let plain = file(tmp.path(), "copy.txt", other.as_bytes());
        assert_eq!(ledger(&import_files(vec![gz, plain]).unwrap()), LEDGER);
    }

    /// A month split into per-region files sums to what the all-regions
    /// report has, under the same ids, and a directory reads its reports.
    #[test]
    fn region_files_sum_to_the_whole_month() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("reports");
        std::fs::create_dir(&dir).unwrap();
        let whole = report();
        let lines: Vec<&str> = whole.lines().collect();
        let us = format!(
            "{HEADER}{}\n{}\n{}\nTotal_Rows\t3\n",
            lines[1], lines[3], lines[5]
        );
        let rest = format!("{HEADER}{}\n{}\n{}\n", lines[2], lines[4], lines[6]);
        file(&dir, "80000001_0426_US.txt", us.as_bytes());
        file(&dir, "80000001_0426_WW.txt.gz", &gzip(&rest));
        file(&dir, "notes.md", b"not a report");
        assert_eq!(ledger(&import_files(vec![dir]).unwrap()), LEDGER);
    }

    #[test]
    fn reports_that_do_not_read_fail_naming_the_line() {
        let tmp = tempfile::tempdir().unwrap();
        let run = |name: &str, text: String| {
            import_files(vec![file(tmp.path(), name, text.as_bytes())])
                .unwrap_err()
                .to_string()
        };
        let good = PRO.tsv();
        let err = run("a.txt", good.clone());
        assert!(err.ends_with("a.txt: row 1: a row before any header; expected a header with Start Date, End Date, Vendor Identifier, Quantity, Extended Partner Share, Partner Share Currency, Sales or Return, Apple Identifier, Title, Product Type Identifier, Country Of Sale"), "{err}");

        let bad = Line {
            sale_or_return: "X",
            ..PRO
        }
        .tsv();
        let err = run("b.txt", format!("{HEADER}{good}{bad}"));
        assert!(
            err.ends_with("b.txt: row 3: sales or return \"X\", not S or R"),
            "{err}"
        );

        let fine = Line {
            ext: "1.005",
            ..PRO
        }
        .tsv();
        let err = run("c.txt", format!("{HEADER}{fine}"));
        assert!(err.ends_with("c.txt: row 2: bad amount \"1.005\""), "{err}");

        let header = HEADER.replace("\tTitle\t", "\tName\t");
        let err = run("d.txt", format!("{header}{good}"));
        assert!(
            err.contains("d.txt: row 1: no \"Title\" column; the header is"),
            "{err}"
        );
    }

    #[test]
    fn a_vendor_number_is_required() {
        let importer = AppStoreImporter::new(None, &CashflowDir::new("unused"));
        let err = importer.import(&input(vec![])).unwrap_err();
        assert!(matches!(&err, ImportError::Usage(m) if m.contains("`vendor` under [appstore]")));
    }

    #[test]
    fn months_and_ranges() {
        let args = |list: &[&str]| split_args(&list.iter().map(PathBuf::from).collect::<Vec<_>>());
        assert_eq!(
            args(&["2025-11..2026-02", "2026-04"]).unwrap().months,
            ["2025-11", "2025-12", "2026-01", "2026-02", "2026-04"]
        );
        for bad in ["2026-13", "2026-1", "2026-03..2026-01", "nope.txt"] {
            assert!(
                matches!(args(&[bad]), Err(ImportError::Read { .. })),
                "{bad}"
            );
        }
        let months = default_months(date("2026-03-15"));
        assert_eq!(months.len(), 12);
        assert_eq!(
            (months[0].as_str(), months[11].as_str()),
            ("2025-03", "2026-02")
        );
    }

    /// Reports by month, recording the months asked for.
    struct FakeApi {
        reports: BTreeMap<&'static str, Vec<u8>>,
        asked: RefCell<Vec<String>>,
    }

    impl ReportSource for std::rc::Rc<FakeApi> {
        fn report(&self, month: &str) -> Result<Option<Vec<u8>>, String> {
            self.asked.borrow_mut().push(month.to_owned());
            Ok(self.reports.get(month).cloned())
        }
    }

    #[test]
    fn fetched_months_read_like_files_and_a_missing_one_waits() {
        let api = std::rc::Rc::new(FakeApi {
            reports: BTreeMap::from([("2026-04", gzip(&report()))]),
            asked: RefCell::new(Vec::new()),
        });
        let importer = AppStoreImporter::with_source(
            Some(&config()),
            Box::new(api.clone()),
            date("2026-06-10"),
        );
        let batch = importer
            .import(&input(vec![PathBuf::from("2026-04..2026-05")]))
            .unwrap();
        assert_eq!(ledger(&batch), LEDGER);
        assert_eq!(*api.asked.borrow(), ["2026-04", "2026-05"]);
        assert_eq!(
            batch.left_out[0],
            LeftOut {
                id: "appstore:80000001:2026-05".to_owned(),
                why: "no report",
            }
        );

        // With no months it asks for the twelve before this one.
        api.asked.borrow_mut().clear();
        importer.import(&input(vec![])).unwrap();
        let asked = api.asked.borrow();
        assert_eq!(
            (asked.len(), asked[0].as_str(), asked[11].as_str()),
            (12, "2025-06", "2026-05")
        );
    }

    #[test]
    fn fetching_without_the_key_ids_says_which_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let importer = AppStoreImporter::new(Some(&config()), &CashflowDir::new(tmp.path()));
        let err = importer.import(&input(vec![])).unwrap_err();
        assert!(
            matches!(&err, ImportError::Usage(m)
                if m.contains("`key_id`") && m.contains("cashflow config set appstore.key_id")),
            "{err}"
        );

        let ids = AppStoreConfig {
            key_id: Some("KEY123".to_owned()),
            issuer: Some("issuer-uuid".to_owned()),
            ..config()
        };
        let importer = AppStoreImporter::new(Some(&ids), &CashflowDir::new(tmp.path()));
        let err = importer.import(&input(vec![])).unwrap_err();
        assert!(
            matches!(&err, ImportError::Usage(m)
                if m.contains("no App Store Connect key")
                    && m.contains("cashflow secret set appstore.p8")),
            "{err}"
        );
    }

    /// The token's header and claims are what App Store Connect asks for,
    /// and its signature verifies against the key's public half.
    #[test]
    fn tokens_carry_the_claims_and_verify() {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            STANDARD.encode(pkcs8.as_ref())
        );
        let claims = TokenClaims {
            key_id: "KEY123",
            issuer: "issuer-uuid",
            now: 1_780_000_000,
        };
        let jwt = token(&pem, &claims).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let decode = |part: &str| -> serde_json::Value {
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
        };
        assert_eq!(
            decode(parts[0]),
            json!({"alg": "ES256", "kid": "KEY123", "typ": "JWT"})
        );
        assert_eq!(
            decode(parts[1]),
            json!({
                "iss": "issuer-uuid",
                "iat": 1_780_000_000u64,
                "exp": 1_780_001_200u64,
                "aud": "appstoreconnect-v1",
            })
        );

        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let signature = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        assert_eq!(signature.len(), 64, "ES256 is r||s, not DER");
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, key.public_key().as_ref())
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
            .unwrap();

        assert!(token("not a key", &claims).unwrap_err().contains("not PEM"));
    }
}
