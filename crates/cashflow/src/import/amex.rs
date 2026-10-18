//! The `amex` importer: American Express activity CSV exports, ported from
//! amex2ledger.
//!
//! An export looks like this, with the columns found by their header names:
//!
//! ```text
//! Date,Date Processed,Description,Amount,Flexible
//! 15 Apr 2026,16 Apr 2026,CORNER BISTRO           Springfield,123.16,FS
//! ```
//!
//! A positive amount is a charge on the card and a negative one a payment or
//! refund. Each row becomes a transaction with the card leg
//! ([`AmexConfig::card`], `liabilities:amex` by default) and a counter leg to
//! `expenses:unknown` taking the other side, so a refund lowers expenses and
//! a payment lands there too until a rule (say, `PAYMENT RECEIVED` to
//! `assets:chequing`) moves it. The description is split into
//! `; raw-merchant:` and `; raw-city:` (see [`split_description`]) and kept
//! whole as `; raw-description:`; the merchant is the payee.
//!
//! **Record ids.** The export has no reference column, so a row's id is
//! the occurrence id `<source>:<date>:<cents>:<merchant>:<n>` (see
//! [`super::csv_rows`]), the merchant normalized by [`id_key`]. So for each
//! (date, merchant, cents) the journal keeps the most rows any one export
//! had, never the sum: amex2ledger's dedup, done by the record ids rather
//! than a pass over the files. The batch is sorted by id, so a re-pull of
//! the same rows makes the same batch and [`super::write`] finds nothing
//! changed.
//!
//! An Amex activity export only lists posted charges, and every row has a
//! processed date, which marks it cleared. A row without one would be
//! written uncleared and, keeping its id, replaced once it posts, as long as
//! its amount doesn't change on the way.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use ledgerdb::{Date, Ymd};

use super::csv_rows::{
    Header, Identified, RowError, csv_files, id_key, number, parse_cents, read, squeeze,
};
use super::{Batch, ImportError, ImportInput, Importer, RawAmount, RawMeta, RawPosting, RawTxn};
use crate::config::AmexConfig;
use crate::csv;

/// The card account when `[amex] card` doesn't name one.
pub const DEFAULT_CARD: &str = "liabilities:amex";

/// The commodity when `[amex] currency` doesn't name one.
pub const DEFAULT_CURRENCY: &str = "CAD";

/// The counter leg's account: the structural default the derive step
/// recategorizes (see [`crate::rules::DEFAULT_COUNTER_ACCOUNTS`]).
const COUNTER: &str = "expenses:unknown";

/// Amex pads the merchant name to this many characters and appends the city.
const MERCHANT_WIDTH: usize = 24;

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// Reads Amex activity CSV exports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AmexImporter {
    /// The card's liability account.
    pub card: String,
    /// The commodity amounts are written in.
    pub currency: String,
    /// Title-case the payee (see [`AmexConfig::title_case`]).
    pub title_case: bool,
}

impl AmexImporter {
    /// The importer as `config.toml`'s `[amex]` table sets it up, `None`
    /// being all defaults.
    pub fn new(config: Option<&AmexConfig>) -> Self {
        let config = config.cloned().unwrap_or_default();
        Self {
            card: config.card.unwrap_or_else(|| DEFAULT_CARD.to_owned()),
            currency: config
                .currency
                .unwrap_or_else(|| DEFAULT_CURRENCY.to_owned()),
            title_case: config.title_case,
        }
    }
}

impl Importer for AmexImporter {
    fn name(&self) -> &'static str {
        "amex"
    }

    fn layer(&self) -> &'static str {
        "amex"
    }

    fn import(&self, input: &ImportInput) -> Result<Batch, ImportError> {
        if input.paths.is_empty() {
            return Err(ImportError::Usage(
                "`import amex` takes one or more CSV files or directories".to_owned(),
            ));
        }
        let mut rows: BTreeMap<String, Row> = BTreeMap::new();
        for path in csv_files(&input.paths)? {
            let file = read_export(&read(&path)?).map_err(|error| error.in_file(&path))?;
            for Identified { id, row } in give_ids(&input.source, file) {
                keep(&mut rows, id, row);
            }
        }
        let txns: Vec<RawTxn> = rows
            .into_iter()
            .map(|(id, row)| self.raw_txn(id, row))
            .collect();
        Ok(Batch::from_raw(&txns, &[])?)
    }
}

impl AmexImporter {
    /// `row` as a raw transaction with record id `id`.
    fn raw_txn(&self, id: String, row: Row) -> RawTxn {
        let payee = if self.title_case {
            title_case(&row.merchant)
        } else {
            row.merchant.clone()
        };
        let amount = |qty| {
            Some(RawAmount {
                qty,
                precision: 2,
                commodity: self.currency.clone(),
            })
        };
        let mut meta = vec![RawMeta {
            key: "raw-merchant".to_owned(),
            value: row.merchant,
        }];
        if !row.city.is_empty() {
            meta.push(RawMeta {
                key: "raw-city".to_owned(),
                value: row.city,
            });
        }
        meta.push(RawMeta {
            key: "raw-description".to_owned(),
            value: row.description,
        });
        if let Some(processed) = row.processed {
            meta.push(RawMeta {
                key: "raw-processed".to_owned(),
                value: processed.to_string(),
            });
        }
        RawTxn {
            date: row.date,
            cleared: row.processed.is_some(),
            payee,
            id,
            meta,
            postings: vec![
                RawPosting {
                    account: COUNTER.to_owned(),
                    amount: amount(row.cents),
                },
                RawPosting {
                    account: self.card.clone(),
                    amount: amount(-row.cents),
                },
            ],
        }
    }
}

/// One export row, as read.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Row {
    date: Date,
    /// Signed cents: positive is a charge on the card.
    cents: i64,
    /// [`id_key`] of the merchant, the id's merchant part. It sorts
    /// before the merchant itself, so rows sharing an id key sort together.
    key: String,
    /// The description's first [`MERCHANT_WIDTH`] characters, or all of it,
    /// with whitespace squeezed.
    merchant: String,
    /// What follows the merchant, squeezed; empty when there is none.
    city: String,
    /// The whole description, squeezed.
    description: String,
    /// `Date Processed`, `None` when the column is missing or blank.
    processed: Option<Date>,
}

/// Where each column the importer reads sits in a row.
struct Columns {
    date: usize,
    processed: Option<usize>,
    description: usize,
    amount: usize,
}

impl Columns {
    /// Find the columns by name in the `header` row.
    fn find(header: &Header) -> Result<Self, RowError> {
        Ok(Self {
            date: header.need("Date")?,
            processed: header.at("Date Processed"),
            description: header.need("Description")?,
            amount: header.need("Amount")?,
        })
    }
}

/// Every row of one export, in its order. The first record is the header;
/// a row that doesn't read is an error, so no charge is silently dropped.
fn read_export(text: &str) -> Result<Vec<Row>, RowError> {
    let mut records = csv::Reader::new(text);
    let Some(header) = records.next() else {
        return Ok(Vec::new());
    };
    let header = Header::new(
        header.map(|field| field.into_owned()).collect(),
        "Date, Date Processed, Description, Amount",
    );
    let columns = Columns::find(&header)?;

    let mut rows = Vec::new();
    for (index, record) in records.enumerate() {
        let row = index + 2;
        let fields: Vec<_> = record.collect();
        let field = |at: usize| fields.get(at).map_or("", |f| f.as_ref()).trim();
        let error = |message: String| RowError { row, message };

        let date = parse_date(field(columns.date))
            .ok_or_else(|| error(format!("bad date {:?}", field(columns.date))))?;
        let processed = match columns.processed.map(field).filter(|p| !p.is_empty()) {
            Some(text) => Some(
                parse_date(text).ok_or_else(|| error(format!("bad processed date {text:?}")))?,
            ),
            None => None,
        };
        let cents = parse_cents(field(columns.amount))
            .ok_or_else(|| error(format!("bad amount {:?}", field(columns.amount))))?;
        let raw = field(columns.description);
        let Description { merchant, city } = split_description(raw);
        if merchant.is_empty() {
            return Err(error("empty description".to_owned()));
        }
        rows.push(Row {
            date,
            cents,
            key: id_key(&merchant),
            merchant,
            city,
            description: squeeze(raw),
            processed,
        });
    }
    Ok(rows)
}

/// Give each row of one export its occurrence id under `source`, the stem
/// being (date, cents, merchant key), which [`Row`] sorts by first.
fn give_ids(source: &str, rows: Vec<Row>) -> Vec<Identified<Row>> {
    number(rows, |row| {
        format!("{source}:{}:{}:{}", row.date, row.cents, row.key)
    })
}

/// Keep `row` under `id` unless an earlier export already gave one: the
/// first export's wins, except that a posted row replaces a pending one.
fn keep(rows: &mut BTreeMap<String, Row>, id: String, row: Row) {
    match rows.entry(id) {
        Entry::Vacant(slot) => {
            slot.insert(row);
        }
        Entry::Occupied(mut slot) => {
            if slot.get().processed.is_none() && row.processed.is_some() {
                slot.insert(row);
            }
        }
    }
}

/// `"CORNER BISTRO           Springfield"` to `("CORNER BISTRO",
/// "Springfield")`.
///
/// The merchant is blank-padded to [`MERCHANT_WIDTH`], so there is a city
/// only when the description is longer than that *and* its last padding
/// column is blank. That keeps a plain `"PAYMENT RECEIVED - THANK YOU"`
/// whole.
fn split_description(description: &str) -> Description {
    let mut chars = description.char_indices().skip(MERCHANT_WIDTH - 1);
    let last_pad = chars.next().map(|(_, c)| c);
    let city_at = chars.next().map(|(at, _)| at);
    let Some(at) = city_at.filter(|_| last_pad.is_some_and(char::is_whitespace)) else {
        return Description {
            merchant: squeeze(description),
            city: String::new(),
        };
    };
    Description {
        merchant: squeeze(&description[..at]),
        city: squeeze(&description[at..]),
    }
}

/// A description split by [`split_description`], each part squeezed.
#[derive(Debug, PartialEq, Eq)]
struct Description {
    merchant: String,
    /// Empty when there is none.
    city: String,
}

/// `"15 Apr 2026"` (or `"15 April 2026"`) to 2026-04-15.
fn parse_date(text: &str) -> Option<Date> {
    let mut parts = text.split_whitespace();
    let day: u8 = parts.next()?.parse().ok()?;
    let month = parts.next()?.to_ascii_lowercase();
    let year: u16 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let month = MONTHS.iter().position(|m| month.starts_with(m))? as u8 + 1;
    Date::from_ymd(Ymd { year, month, day }).ok()
}

/// `"CORNER BISTRO"` to `"Corner Bistro"`, leaving words that carry digits
/// or punctuation (order ids such as `STORE*AB12CD`) as they are.
fn title_case(text: &str) -> String {
    let words: Vec<String> = text
        .split(' ')
        .map(|word| {
            if !word.chars().all(|c| c.is_alphabetic() || c == '\'') {
                return word.to_owned();
            }
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first
                    .to_uppercase()
                    .chain(chars.as_str().to_lowercase().chars())
                    .collect(),
                None => String::new(),
            }
        })
        .collect();
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::{Rows, write};
    use ledgerdb::{JournalStore, LayerFilter, Ledger, TxnId, WriteOptions};
    use std::path::{Path, PathBuf};

    const HEADER: &str = "Date,Date Processed,Description,Amount,Flexible\n";

    fn date(text: &str) -> Date {
        Date::parse(text).unwrap()
    }

    /// An importer with the defaults.
    fn amex() -> AmexImporter {
        AmexImporter::new(None)
    }

    /// Write `text` to `name` in `dir` and return its path.
    fn file(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    fn input(paths: Vec<PathBuf>) -> ImportInput {
        ImportInput {
            source: "amex".to_owned(),
            paths,
        }
    }

    /// Import `paths` into `store` as one run stamped `imported`.
    fn import(store: &JournalStore, paths: Vec<PathBuf>, imported: u64) -> super::super::Written {
        write(
            store,
            &Default::default(),
            &amex(),
            &input(paths),
            None,
            Some(imported),
        )
        .unwrap()
    }

    /// Every transaction in the journal as canonical text, sorted, so two
    /// journals holding the same records compare equal however they got
    /// there.
    fn books(store: &JournalStore) -> Vec<String> {
        let ledger = Ledger::load(store, LayerFilter::All).unwrap().ledger;
        let mut txns: Vec<String> = (0..ledger.txns().len() as u32)
            .map(|at| {
                let mut text = String::new();
                ledger
                    .journal()
                    .write_txns([TxnId(at)], &mut text, WriteOptions::default())
                    .unwrap();
                text
            })
            .collect();
        txns.sort();
        txns
    }

    #[test]
    fn dates() {
        assert_eq!(parse_date("15 Apr 2026"), Some(date("2026-04-15")));
        assert_eq!(parse_date("05 aug 2026"), Some(date("2026-08-05")));
        assert_eq!(parse_date("1 September 2025"), Some(date("2025-09-01")));
        assert_eq!(parse_date("Date"), None);
        assert_eq!(parse_date("30 Feb 2026"), None);
        assert_eq!(parse_date("15 Apr 2026 x"), None);
    }

    #[test]
    fn description_splitting() {
        let split = |text| {
            let Description { merchant, city } = split_description(text);
            [merchant, city]
        };
        assert_eq!(
            split("CORNER BISTRO           Springfield"),
            ["CORNER BISTRO".to_owned(), "Springfield".to_owned()]
        );
        // A 23-character merchant with one column of padding.
        assert_eq!(
            split("PIZZA PLACE # 021 SPRIN SPRINGFIELD"),
            [
                "PIZZA PLACE # 021 SPRIN".to_owned(),
                "SPRINGFIELD".to_owned()
            ]
        );
        // Longer than the padding but not padded: kept whole.
        assert_eq!(
            split("PAYMENT RECEIVED - THANK YOU"),
            ["PAYMENT RECEIVED - THANK YOU".to_owned(), String::new()]
        );
        // Exactly 24 characters ending in a space has no city.
        assert_eq!(
            split("SHORT NAME              "),
            ["SHORT NAME".to_owned(), String::new()]
        );
        assert_eq!(
            split("  AIR  LINE "),
            ["AIR LINE".to_owned(), String::new()]
        );
        // Characters, not bytes: an accented merchant splits at 24 too.
        assert_eq!(
            split("CAFÉ ÉTOILE             Montréal"),
            ["CAFÉ ÉTOILE".to_owned(), "Montréal".to_owned()]
        );
    }

    #[test]
    fn title_casing_leaves_ids_alone() {
        assert_eq!(title_case("CORNER BISTRO"), "Corner Bistro");
        assert_eq!(title_case("SHOP MKTP CA*AB12CD"), "Shop Mktp CA*AB12CD");
        assert_eq!(title_case("JOE'S DINER"), "Joe's Diner");
    }

    #[test]
    fn rows_read_by_header_name() {
        // Columns in another order, a BOM, CRLF, a quoted comma and a
        // trailing empty field.
        let text = "\u{feff}Amount,Description,Date,Date Processed,Flexible\r\n\
                    -50.00,\"FERRY, NORTH DOCK\",14 Aug 2026,15 Aug 2026,\r\n\
                    \r\n\
                    7.00,\"SAY \"\"HI\"\"\",1 Jan 2026,,\r\n";
        let rows = read_export(text).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].date, date("2026-08-14"));
        assert_eq!(rows[0].processed, Some(date("2026-08-15")));
        assert_eq!(rows[0].cents, -5000);
        assert_eq!(rows[0].merchant, "FERRY, NORTH DOCK");
        assert_eq!(rows[1].cents, 700);
        assert_eq!(rows[1].merchant, "SAY \"HI\"");
        assert_eq!(rows[1].processed, None);
        assert!(read_export("").unwrap().is_empty());
        assert!(read_export(HEADER).unwrap().is_empty());
    }

    #[test]
    fn a_row_that_does_not_read_is_an_error() {
        let err = read_export("Date,Description,Total\n").unwrap_err();
        assert_eq!(err.row, 1);
        assert!(err.message.contains("\"Amount\""), "{}", err.message);

        let text = format!("{HEADER}1 Jan 2026,1 Jan 2026,SHOP,1.00,\n1 Jan 2026,,SHOP,lots,\n");
        let err = read_export(&text).unwrap_err();
        assert_eq!(err.row, 3);
        assert!(err.message.contains("lots"), "{}", err.message);

        let text = format!("{HEADER}Smarch 1,,SHOP,1.00,\n");
        assert_eq!(read_export(&text).unwrap_err().row, 2);
        let text = format!("{HEADER}1 Jan 2026,,  ,1.00,\n");
        assert!(read_export(&text).unwrap_err().message.contains("empty"));
    }

    #[test]
    fn identical_rows_are_numbered_in_any_order() {
        let a = format!(
            "{HEADER}2 Jan 2026,3 Jan 2026,RIDE CO,13.83,\n\
             1 Jan 2026,2 Jan 2026,SHOP,5.00,\n\
             2 Jan 2026,4 Jan 2026,RIDE CO,13.83,\n"
        );
        let b = format!(
            "{HEADER}2 Jan 2026,4 Jan 2026,RIDE CO,13.83,\n\
             2 Jan 2026,3 Jan 2026,Ride  Co,13.83,\n\
             1 Jan 2026,2 Jan 2026,SHOP,5.00,\n"
        );
        let ids = |text: &str| -> Vec<(String, Option<Date>)> {
            give_ids("amex", read_export(text).unwrap())
                .into_iter()
                .map(|Identified { id, row }| (id, row.processed))
                .collect()
        };
        let got = ids(&a);
        assert_eq!(
            got,
            [
                (
                    "amex:2026-01-01:500:shop:1".to_owned(),
                    Some(date("2026-01-02"))
                ),
                (
                    "amex:2026-01-02:1383:ride-co:1".to_owned(),
                    Some(date("2026-01-03"))
                ),
                (
                    "amex:2026-01-02:1383:ride-co:2".to_owned(),
                    Some(date("2026-01-04"))
                ),
            ]
        );
        // Same ids whatever the order, and whatever the merchant's case.
        let got_b: Vec<String> = ids(&b).into_iter().map(|(id, _)| id).collect();
        let got_a: Vec<String> = got.into_iter().map(|(id, _)| id).collect();
        assert_eq!(got_b, got_a);
    }

    #[test]
    fn a_row_becomes_a_raw_charge() {
        let tmp = tempfile::tempdir().unwrap();
        let text = format!(
            "{HEADER}15 Apr 2026,16 Apr 2026,CORNER BISTRO           Springfield,123.16,FS\n\
             12 Mar 2026,12 Mar 2026,PAYMENT RECEIVED - THANK YOU,\"-8,803.95\",\n"
        );
        let path = file(tmp.path(), "a.csv", &text);
        let batch = amex().import(&input(vec![path])).unwrap();
        assert_eq!(
            batch.journal.to_ledger(WriteOptions::default()),
            "2026-03-12 * PAYMENT RECEIVED - THANK YOU\n\
             \t; id: amex:2026-03-12:-880395:payment-received-thank-you:1\n\
             \t; raw-merchant: PAYMENT RECEIVED - THANK YOU\n\
             \t; raw-description: PAYMENT RECEIVED - THANK YOU\n\
             \t; raw-processed: 2026-03-12\n\
             \texpenses:unknown\t-8803.95 CAD\n\
             \tliabilities:amex\t8803.95 CAD\n\n\
             2026-04-15 * CORNER BISTRO\n\
             \t; id: amex:2026-04-15:12316:corner-bistro:1\n\
             \t; raw-merchant: CORNER BISTRO\n\
             \t; raw-city: Springfield\n\
             \t; raw-description: CORNER BISTRO Springfield\n\
             \t; raw-processed: 2026-04-16\n\
             \texpenses:unknown\t123.16 CAD\n\
             \tliabilities:amex\t-123.16 CAD\n"
        );
    }

    #[test]
    fn config_sets_card_currency_and_title_case() {
        let tmp = tempfile::tempdir().unwrap();
        let path = file(
            tmp.path(),
            "a.csv",
            &format!("{HEADER}15 Apr 2026,,CORNER BISTRO,1.00,\n"),
        );
        let importer = AmexImporter::new(Some(&AmexConfig {
            card: Some("liabilities:card".to_owned()),
            currency: Some("USD".to_owned()),
            title_case: true,
        }));
        let text = importer
            .import(&input(vec![path]))
            .unwrap()
            .journal
            .to_ledger(WriteOptions::default());
        // Uncleared without a processed date; the id and raw-merchant keep
        // the export's spelling.
        assert!(text.starts_with("2026-04-15 Corner Bistro\n"), "{text}");
        assert!(text.contains("; id: amex:2026-04-15:100:corner-bistro:1\n"));
        assert!(text.contains("; raw-merchant: CORNER BISTRO\n"));
        assert!(text.contains("\tliabilities:card\t-1.00 USD\n"), "{text}");
    }

    #[test]
    fn a_directory_reads_its_csv_files_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("amex");
        std::fs::create_dir(&dir).unwrap();
        file(&dir, "b.CSV", &format!("{HEADER}2 Jan 2026,,TWO,2.00,\n"));
        file(&dir, "a.csv", &format!("{HEADER}1 Jan 2026,,ONE,1.00,\n"));
        let batch = amex().import(&input(vec![dir])).unwrap();
        assert_eq!(batch.txn_count(), 2);
    }

    #[test]
    fn errors_name_the_file_and_row() {
        let tmp = tempfile::tempdir().unwrap();
        let path = file(tmp.path(), "a.csv", &format!("{HEADER}1 Jan 2026,,X,?,\n"));
        let err = amex().import(&input(vec![path.clone()])).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("{}: row 2: bad amount \"?\"", path.display())
        );
        assert!(matches!(
            amex().import(&input(vec![])),
            Err(ImportError::Usage(_))
        ));
        assert!(matches!(
            amex().import(&input(vec![tmp.path().join("missing.csv")])),
            Err(ImportError::Read { .. })
        ));
    }

    /// Two exports that overlap, the second also re-listing a day of five
    /// identical rides as four (a ride the first export had is not in the
    /// second's range), and their union with each row at its largest count.
    const FIRST: &str = "\
1 Jan 2026,1 Jan 2026,SHOP,5.00,
2 Jan 2026,2 Jan 2026,RIDE CO,13.83,
2 Jan 2026,2 Jan 2026,RIDE CO,13.83,
2 Jan 2026,2 Jan 2026,RIDE CO,13.83,
2 Jan 2026,2 Jan 2026,RIDE CO,13.83,
2 Jan 2026,2 Jan 2026,RIDE CO,13.83,
3 Jan 2026,3 Jan 2026,GROCER  SPRINGFIELD,40.00,
";
    const SECOND: &str = "\
2 Jan 2026,2 Jan 2026,RIDE CO,13.83,
2 Jan 2026,2 Jan 2026,RIDE CO,13.83,
2 Jan 2026,2 Jan 2026,RIDE CO,13.83,
2 Jan 2026,2 Jan 2026,RIDE CO,13.83,
3 Jan 2026,3 Jan 2026,GROCER  SPRINGFIELD,40.00,
3 Jan 2026,3 Jan 2026,GROCER  SPRINGFIELD,40.00,
4 Jan 2026,4 Jan 2026,PAYMENT RECEIVED - THANK YOU,-100.00,
";
    const UNION: &str = "\
1 Jan 2026,1 Jan 2026,SHOP,5.00,
2 Jan 2026,2 Jan 2026,RIDE CO,13.83,
2 Jan 2026,2 Jan 2026,RIDE CO,13.83,
2 Jan 2026,2 Jan 2026,RIDE CO,13.83,
2 Jan 2026,2 Jan 2026,RIDE CO,13.83,
2 Jan 2026,2 Jan 2026,RIDE CO,13.83,
3 Jan 2026,3 Jan 2026,GROCER  SPRINGFIELD,40.00,
3 Jan 2026,3 Jan 2026,GROCER  SPRINGFIELD,40.00,
4 Jan 2026,4 Jan 2026,PAYMENT RECEIVED - THANK YOU,-100.00,
";

    #[test]
    fn overlapping_exports_import_as_their_union() {
        let tmp = tempfile::tempdir().unwrap();
        let first = file(tmp.path(), "1.csv", &format!("{HEADER}{FIRST}"));
        let second = file(tmp.path(), "2.csv", &format!("{HEADER}{SECOND}"));
        let union = file(tmp.path(), "u.csv", &format!("{HEADER}{UNION}"));

        let one_by_one = JournalStore::new(tmp.path().join("a"));
        import(&one_by_one, vec![first.clone()], 1);
        let later = import(&one_by_one, vec![second.clone()], 2);
        assert_eq!(
            later.txn_rows,
            Rows {
                new: 2,
                changed: 0,
                unchanged: 5
            }
        );

        let together = JournalStore::new(tmp.path().join("b"));
        let both = import(&together, vec![first, second], 1);
        assert_eq!(both.txns, 9);

        let at_once = JournalStore::new(tmp.path().join("c"));
        import(&at_once, vec![union.clone()], 1);

        let want = books(&at_once);
        assert_eq!(want.len(), 9);
        assert_eq!(books(&one_by_one), want);
        assert_eq!(books(&together), want);

        // And the union again has nothing to add.
        let again = import(&at_once, vec![union], 3);
        assert_eq!(again.hash, None);
    }

    #[test]
    fn rules_categorize_through_derive() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        import(
            &store,
            vec![file(tmp.path(), "u.csv", &format!("{HEADER}{UNION}"))],
            1,
        );
        // An amex.toml-shaped rules file: case-insensitive merchant
        // substrings, first match wins.
        let edits = crate::rules::parse(
            r#"
card = "liabilities:amex"
unknown = "expenses:unknown"
currency = "CAD"

[[rule]]
account = "assets:chequing"
match = ["payment received"]

[[rule]]
account = "expenses:groceries"
match = ["grocer"]

[[rule]]
account = "expenses:food"
match = ["grocer", "shop"]
"#,
            Path::new("amex.toml"),
        )
        .unwrap();
        let books =
            crate::Books::load(&store, LayerFilter::All, &Default::default(), &edits, "CAD")
                .unwrap();
        let accounts = &books.ledger().journal().accounts;
        let q = books.query();
        let at = date("2026-12-31");
        let balance = |name| {
            let account = accounts.lookup(name).unwrap();
            q.balance(account, at, books.home()).totals[0].qty
        };
        assert_eq!(balance("expenses:groceries"), 8000);
        assert_eq!(balance("expenses:food"), 500);
        assert_eq!(balance("assets:chequing"), -10000);
        assert_eq!(balance("expenses:unknown"), 5 * 1383);
        assert_eq!(
            balance("liabilities:amex"),
            -(8000 + 500 - 10000 + 5 * 1383)
        );
    }
}
