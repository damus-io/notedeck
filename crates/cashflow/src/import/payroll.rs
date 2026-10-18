//! The `payroll` importer: a payroll file of bitcoin payouts, the input
//! `payroll.awk` used to read.
//!
//! Each payout is a line starting with `*`, seven fields separated by tabs
//! or spaces:
//!
//! ```text
//! *  <invoice>  <address>  <date>  <cad>CAD  <sats>  <txid>
//! *  ??         bc1q…      2024-03-01  4000CAD  5000000  9f2c…
//! *  7          bc1q…      2024-04-01  4000.50CAD  4800000  1ab3…
//! ```
//!
//! The invoice number is `??` when the file doesn't give one, the CAD amount
//! is what the invoice was for (its `CAD` suffix optional), the sats are what
//! was paid for it, and the txid is the on-chain send that paid it. Every
//! other line (a heading, a PGP signature around the list) is not a payout
//! and is passed over. A `*` line that doesn't read is reported in
//! [`Batch::skipped`] with its line number, and the rest of the file still
//! imports.
//!
//! **The send is already in the journal.** The txid is a send from a wallet
//! the `btc` importer reads, which files it as `btc:<txid>` with the wallet
//! leg and a counter leg to `expenses:unknown`. So a payout is not posted
//! against a wallet a second time: by default its record moves the sats from
//! `expenses:unknown` to `expenses:payroll`, leaving the wallet to the `btc`
//! importer. [`PayrollConfig::counter`] can point it at a wallet account
//! instead, for payouts from a wallet that is not imported.
//!
//! Each payout becomes, in msat like every bitcoin amount:
//!
//! ```text
//! 2024-03-01 * <payee> invoice 1
//!     ; id: payroll:<txid>
//!     ; raw-txid: <txid>
//!     ; raw-address: <address>
//!     ; raw-cad: 4000.00
//!     expenses:payroll       5000000000 msat
//!     expenses:unknown      -5000000000 msat
//! ```
//!
//! The payee is [`PayrollConfig::payee`] (never a name in this code) and
//! `invoice <n>`, `n` being the line's own invoice number when it has one
//! and otherwise its place among the file's `*` lines, which is how
//! `payroll.awk` numbered them. The record id is `<source>:<txid>`, with
//! `:2`, `:3`, … for further invoices paid by the same send. `raw-txid` is
//! the key the `btc` importer uses too, so one rule can match both records.
//!
//! **The CAD amount is not a `P` directive.** It is the rate the payout was
//! made at, but a `sat → CAD` price would be a one-hop edge in the price
//! graph, and [`ledgerdb::PriceDb`] takes the shortest path, so every
//! bitcoin amount after a payday would convert at that payday's rate instead
//! of the daily `msat → sat → btc → USD → CAD` chain. It is kept as
//! `raw-cad` metadata instead.

use std::collections::HashMap;

use ledgerdb::Date;

use super::csv_rows::{parse_cents, read};
use super::{
    Batch, ImportError, ImportInput, Importer, RawAmount, RawMeta, RawPosting, RawTxn, Skipped,
};
use crate::config::PayrollConfig;
use crate::prices::MSAT;

/// The payee name when [`PayrollConfig::payee`] is unset.
const PAYEE: &str = "payroll";

/// The expense account when [`PayrollConfig::account`] is unset.
const ACCOUNT: &str = "expenses:payroll";

/// The counter leg when [`PayrollConfig::counter`] is unset: where the `btc`
/// importer files the send that paid the invoice.
const COUNTER: &str = "expenses:unknown";

/// msat in a sat.
const MSAT_PER_SAT: i64 = 1000;

/// Reads a payroll file of bitcoin payouts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayrollImporter {
    /// The name in each payee line, before `invoice <n>`.
    pub payee: String,
    /// The expense account the pay goes to.
    pub account: String,
    /// The counter leg's account.
    pub counter: String,
}

impl PayrollImporter {
    /// The importer as `config.toml`'s `[payroll]` table sets it up, each
    /// key unset taking its default.
    pub fn new(config: Option<&PayrollConfig>) -> Self {
        let get = |key: fn(&PayrollConfig) -> &Option<String>, default: &str| {
            config
                .and_then(|c| key(c).clone())
                .unwrap_or_else(|| default.to_owned())
        };
        Self {
            payee: get(|c| &c.payee, PAYEE),
            account: get(|c| &c.account, ACCOUNT),
            counter: get(|c| &c.counter, COUNTER),
        }
    }
}

impl Importer for PayrollImporter {
    fn name(&self) -> &'static str {
        "payroll"
    }

    fn layer(&self) -> &'static str {
        "payroll"
    }

    fn import(&self, input: &ImportInput) -> Result<Batch, ImportError> {
        let [path] = input.paths.as_slice() else {
            return Err(ImportError::Usage(
                "`import payroll` takes exactly one <file>".to_owned(),
            ));
        };
        let text = read(path)?;
        let Lines { payouts, skipped } = read_lines(&text);
        let mut sends: HashMap<&str, u32> = HashMap::new();
        let txns: Vec<RawTxn> = payouts
            .iter()
            .map(|payout| {
                let nth = sends.entry(payout.txid).or_default();
                *nth += 1;
                let id = match *nth {
                    1 => format!("{}:{}", input.source, payout.txid),
                    n => format!("{}:{}:{n}", input.source, payout.txid),
                };
                self.raw_txn(id, payout)
            })
            .collect();
        Ok(Batch {
            skipped,
            ..Batch::from_raw(&txns, &[])?
        })
    }
}

/// One `*` line, as read.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Payout<'a> {
    /// The invoice number the payee line gives: the line's own, or its
    /// place among the `*` lines when it has none.
    invoice: u32,
    /// Whether [`Self::invoice`] is the line's own.
    numbered: bool,
    address: &'a str,
    date: Date,
    /// The invoice's CAD amount, in cents.
    cad_cents: i64,
    sats: i64,
    txid: &'a str,
}

/// A file's payouts in file order, and the `*` lines that didn't read.
struct Lines<'a> {
    payouts: Vec<Payout<'a>>,
    skipped: Vec<Skipped>,
}

/// Read every `*` line of `text`, passing over the rest.
fn read_lines(text: &str) -> Lines<'_> {
    let mut lines = Lines {
        payouts: Vec::new(),
        skipped: Vec::new(),
    };
    let mut place = 0u32;
    for (index, line) in text.lines().enumerate() {
        if !line.starts_with('*') {
            continue;
        }
        place += 1;
        match read_payout(line, place) {
            Ok(payout) => lines.payouts.push(payout),
            Err(Bad { at, message }) => lines.skipped.push(Skipped {
                line: index as u32 + 1,
                col: column(line, at),
                message,
            }),
        }
    }
    lines
}

/// What is wrong with a `*` line, and the field (a slice of the line) it
/// is in.
struct Bad<'a> {
    at: &'a str,
    message: String,
}

/// Read one `*` line, the `place`th in its file.
fn read_payout(line: &str, place: u32) -> Result<Payout<'_>, Bad<'_>> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    let [star, invoice, address, date, cad, sats, txid] = fields[..] else {
        return Err(Bad {
            at: line,
            message: format!(
                "a payout has 7 fields (* invoice address date cad sats txid), this line {}",
                fields.len()
            ),
        });
    };
    let bad = |at, what: &str| Bad {
        at,
        message: format!("bad {what} {at:?}"),
    };
    if star != "*" {
        return Err(bad(star, "marker"));
    }
    let own = invoice.parse::<u32>().ok();
    if own.is_none() && invoice != "??" {
        return Err(bad(invoice, "invoice number (a number or ??)"));
    }
    let date = Date::parse(date).map_err(|_| bad(date, "date"))?;
    let cad_cents = parse_cents(cad.strip_suffix("CAD").unwrap_or(cad))
        .filter(|cents| *cents > 0)
        .ok_or_else(|| bad(cad, "CAD amount"))?;
    let sats = sats
        .parse::<i64>()
        .ok()
        .filter(|sats| *sats > 0 && sats.checked_mul(MSAT_PER_SAT).is_some())
        .ok_or_else(|| bad(sats, "sat amount"))?;
    if txid.len() != 64 || !txid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad(txid, "txid (64 hex digits)"));
    }
    Ok(Payout {
        invoice: own.unwrap_or(place),
        numbered: own.is_some(),
        address,
        date,
        cad_cents,
        sats,
        txid,
    })
}

/// The 1-based byte column `field`, a slice of `line`, starts at.
fn column(line: &str, field: &str) -> u32 {
    (field.as_ptr() as usize - line.as_ptr() as usize) as u32 + 1
}

impl PayrollImporter {
    /// `payout` as a raw transaction with record id `id`.
    fn raw_txn(&self, id: String, payout: &Payout) -> RawTxn {
        let msat = payout.sats * MSAT_PER_SAT;
        let amount = |qty| {
            Some(RawAmount {
                qty,
                precision: 0,
                commodity: MSAT.to_owned(),
            })
        };
        let meta = |key: &str, value: String| RawMeta {
            key: key.to_owned(),
            value,
        };
        let mut metas = vec![
            meta("raw-txid", payout.txid.to_owned()),
            meta("raw-address", payout.address.to_owned()),
            meta(
                "raw-cad",
                format!("{}.{:02}", payout.cad_cents / 100, payout.cad_cents % 100),
            ),
        ];
        if payout.numbered {
            metas.push(meta("raw-invoice", payout.invoice.to_string()));
        }
        RawTxn {
            date: payout.date,
            cleared: true,
            payee: format!("{} invoice {}", self.payee, payout.invoice),
            id,
            meta: metas,
            postings: vec![
                RawPosting {
                    account: self.account.clone(),
                    amount: amount(msat),
                },
                RawPosting {
                    account: self.counter.clone(),
                    amount: amount(-msat),
                },
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::{Rows, write};
    use ledgerdb::{JournalStore, WriteOptions};
    use std::path::{Path, PathBuf};

    /// A synthetic 64-hex txid made of `c`.
    fn txid(c: char) -> String {
        c.to_string().repeat(64)
    }

    fn file(dir: &Path, text: &str) -> PathBuf {
        let path = dir.join("payroll.txt");
        std::fs::write(&path, text).unwrap();
        path
    }

    fn input(paths: Vec<PathBuf>) -> ImportInput {
        ImportInput {
            source: "payroll".to_owned(),
            paths,
        }
    }

    /// A file shaped like a signed payroll list: a heading and a signature
    /// around the `*` lines, tabs and runs of spaces between fields.
    fn signed(lines: &str) -> String {
        format!(
            "Payouts\n=======\n\n{lines}\n-----BEGIN PGP SIGNATURE-----\nabc\n-----END PGP SIGNATURE-----\n"
        )
    }

    #[test]
    fn payouts_become_raw_transactions() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (txid('a'), txid('b'));
        let text = signed(&format!(
            "*\t??\tbc1qone\t2024-03-01\t4000CAD\t5000000\t{a}\n\
             *  7   bc1qtwo  2024-04-01  4000.50  4800000  {b}\n"
        ));
        let importer = PayrollImporter::new(Some(&PayrollConfig {
            payee: Some("Ada".to_owned()),
            ..PayrollConfig::default()
        }));
        let batch = importer
            .import(&input(vec![file(tmp.path(), &text)]))
            .unwrap();
        assert!(batch.skipped.is_empty(), "{:?}", batch.skipped);
        assert_eq!(batch.price_count(), 0, "the CAD rate is not a P directive");
        assert_eq!(
            batch.journal.to_ledger(WriteOptions::default()),
            format!(
                "2024-03-01 * Ada invoice 1\n\
                 \t; id: payroll:{a}\n\
                 \t; raw-txid: {a}\n\
                 \t; raw-address: bc1qone\n\
                 \t; raw-cad: 4000.00\n\
                 \texpenses:payroll\t5000000000 msat\n\
                 \texpenses:unknown\t-5000000000 msat\n\n\
                 2024-04-01 * Ada invoice 7\n\
                 \t; id: payroll:{b}\n\
                 \t; raw-txid: {b}\n\
                 \t; raw-address: bc1qtwo\n\
                 \t; raw-cad: 4000.50\n\
                 \t; raw-invoice: 7\n\
                 \texpenses:payroll\t4800000000 msat\n\
                 \texpenses:unknown\t-4800000000 msat\n"
            )
        );
    }

    #[test]
    fn defaults_and_a_configured_counter() {
        let defaults = PayrollImporter::new(None);
        assert_eq!(
            (
                defaults.payee.as_str(),
                defaults.account.as_str(),
                defaults.counter.as_str()
            ),
            ("payroll", "expenses:payroll", "expenses:unknown")
        );

        let tmp = tempfile::tempdir().unwrap();
        let path = file(
            tmp.path(),
            &format!("* ?? bc1q 2024-03-01 10CAD 100 {}\n", txid('c')),
        );
        let importer = PayrollImporter::new(Some(&PayrollConfig {
            account: Some("expenses:payroll:ada".to_owned()),
            counter: Some("assets:btc".to_owned()),
            ..PayrollConfig::default()
        }));
        let text = importer
            .import(&input(vec![path]))
            .unwrap()
            .journal
            .to_ledger(WriteOptions::default());
        assert!(
            text.starts_with("2024-03-01 * payroll invoice 1\n"),
            "{text}"
        );
        assert!(
            text.contains("\texpenses:payroll:ada\t100000 msat\n\tassets:btc\t-100000 msat\n"),
            "{text}"
        );
    }

    /// Two invoices paid by one send keep a record each.
    #[test]
    fn a_send_paying_two_invoices() {
        let tmp = tempfile::tempdir().unwrap();
        let a = txid('d');
        let path = file(
            tmp.path(),
            &format!("* 1 bc1q 2024-03-01 10CAD 100 {a}\n* 2 bc1q 2024-03-01 10CAD 100 {a}\n"),
        );
        let batch = PayrollImporter::new(None)
            .import(&input(vec![path]))
            .unwrap();
        let ids: Vec<_> = batch
            .journal
            .txns
            .iter()
            .map(|t| batch.journal.strings.get(t.id.unwrap()))
            .collect();
        assert_eq!(ids, [format!("payroll:{a}"), format!("payroll:{a}:2")]);
    }

    /// A line that doesn't read is reported by line and column, and every
    /// other payout still imports. Malformed lines keep their place, so the
    /// invoice numbers after them don't shift once they're fixed.
    #[test]
    fn a_malformed_line_is_skipped_not_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let t = txid('e');
        let text = format!(
            "* ?? bc1q 2024-03-01 10CAD 100 {t}\n\
             * ?? bc1q 2024-03-01 10CAD 100\n\
             * ?? bc1q 2024-02-30 10CAD 100 {t}\n\
             * ?? bc1q 2024-03-01 tenCAD 100 {t}\n\
             * ?? bc1q 2024-03-01 10CAD -5 {t}\n\
             * x1 bc1q 2024-03-01 10CAD 100 {t}\n\
             * ?? bc1q 2024-03-01 10CAD 100 nothex\n\
             * ?? bc1q 2024-03-08 10CAD 100 {}\n",
            txid('f')
        );
        let batch = PayrollImporter::new(None)
            .import(&input(vec![file(tmp.path(), &text)]))
            .unwrap();
        let skipped: Vec<_> = batch.skipped.iter().map(ToString::to_string).collect();
        assert_eq!(
            skipped,
            [
                "2:1: a payout has 7 fields (* invoice address date cad sats txid), this line 6",
                "3:11: bad date \"2024-02-30\"",
                "4:22: bad CAD amount \"tenCAD\"",
                "5:28: bad sat amount \"-5\"",
                "6:3: bad invoice number (a number or ??) \"x1\"",
                "7:32: bad txid (64 hex digits) \"nothex\"",
            ]
        );
        assert_eq!(batch.txn_count(), 2);
        let text = batch.journal.to_ledger(WriteOptions::default());
        assert!(text.contains("payroll invoice 1\n"), "{text}");
        assert!(text.contains("payroll invoice 8\n"), "{text}");
    }

    #[test]
    fn a_file_with_no_payouts_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        let path = file(tmp.path(), &signed(""));
        let err = write(
            &store,
            &PayrollImporter::new(None),
            &input(vec![path]),
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ImportError::Empty { .. }), "{err}");
        assert!(matches!(
            PayrollImporter::new(None).import(&input(vec![])),
            Err(ImportError::Usage(_))
        ));
    }

    /// The file grows by a line each payday: a re-import writes only the new
    /// payout, and one with nothing new writes nothing.
    #[test]
    fn a_reimport_writes_only_new_payouts() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        let first = format!("* ?? bc1q 2024-03-01 10CAD 100 {}\n", txid('a'));
        let run = |text: &str, at| {
            let path = file(tmp.path(), text);
            write(
                &store,
                &PayrollImporter::new(None),
                &input(vec![path]),
                None,
                Some(at),
            )
            .unwrap()
        };
        let one = run(&first, 1);
        assert_eq!((one.txns, one.header.layer.as_str()), (1, "payroll"));

        let grown = format!("{first}* ?? bc1q 2024-04-01 10CAD 90 {}\n", txid('b'));
        let two = run(&grown, 2);
        assert_eq!(
            two.txn_rows,
            Rows {
                new: 1,
                changed: 0,
                unchanged: 1
            }
        );
        let again = run(&grown, 3);
        assert_eq!(again.hash, None);
        assert_eq!(store.list().unwrap().len(), 2);
    }
}
