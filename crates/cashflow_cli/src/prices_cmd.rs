//! `cashflow prices fetch`: daily exchange rates into the journal.

use std::time::{SystemTime, UNIX_EPOCH};

use cashflow::CashflowDir;
use cashflow::journal;
use cashflow::prices::{self, Ehttp, Fetched, SOURCES, Source};
use ledgerdb::{Date, LayerFilter, Ledger};
use serde_json::json;

use crate::args::PricesArgs;

/// Fetch each asked-for source and print what it wrote. A source that fails
/// is reported and the rest still run; the command fails if any did.
pub fn fetch(dir: &CashflowDir, args: &PricesArgs) -> Result<(), String> {
    let store = journal::store(dir);
    let sources: &[Source] = if args.sources.is_empty() {
        SOURCES
    } else {
        &args.sources
    };
    // Resuming needs the newest rate per source and the oldest transaction.
    let ledger = match args.since {
        Some(_) => None,
        None => Some(
            Ledger::load(&store, LayerFilter::All)
                .map_err(|e| e.to_string())?
                .ledger,
        ),
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());

    // Every start day before any request, so a usage error writes nothing.
    let starts = sources
        .iter()
        .map(|&source| Ok((source, since_for(source, args.since, ledger.as_ref())?)))
        .collect::<Result<Vec<_>, String>>()?;

    let mut rows = Vec::new();
    let mut failed = Vec::new();
    for (source, since) in starts {
        match prices::fetch_into(&store, &Ehttp, source, since, now) {
            Ok(fetched) => {
                if args.json {
                    rows.push(fetched_json(&fetched));
                } else {
                    print_fetched(&fetched);
                }
            }
            Err(err) => {
                if args.json {
                    rows.push(json!({ "source": source.name(), "error": err.to_string() }));
                }
                eprintln!("cashflow: {source}: {err}");
                failed.push(source.name());
            }
        }
    }
    if args.json {
        println!("{}", serde_json::Value::Array(rows));
    }
    if failed.is_empty() {
        return Ok(());
    }
    Err(format!("fetching {} failed", failed.join(", ")))
}

/// The first day to fetch `source` from: `--since`, else where
/// [`prices::resume_from`] says. [`Source::Units`] has no range.
fn since_for(source: Source, since: Option<Date>, ledger: Option<&Ledger>) -> Result<Date, String> {
    if source == Source::Units {
        return Ok(Date::EPOCH);
    }
    if let Some(since) = since {
        return Ok(since);
    }
    ledger
        .and_then(|ledger| prices::resume_from(ledger, source))
        .ok_or_else(|| format!("{source}: the journal is empty, so pass --since <date>"))
}

fn print_fetched(fetched: &Fetched) {
    if fetched.filed.is_empty() {
        println!(
            "up to date  source {}  nothing since {}",
            fetched.source, fetched.since
        );
    }
    for filed in &fetched.filed {
        let verb = if filed.new { "imported" } else { "unchanged" };
        println!(
            "{verb} {}  {} prices  source {}  {}..{}",
            filed.hash, filed.prices, fetched.source, filed.first, filed.last
        );
    }
}

fn fetched_json(fetched: &Fetched) -> serde_json::Value {
    let segments: Vec<_> = fetched
        .filed
        .iter()
        .map(|f| {
            json!({
                "hash": f.hash.to_string(),
                "new": f.new,
                "imported": f.header.imported,
                "prices": f.prices,
                "first": f.first.to_string(),
                "last": f.last.to_string(),
            })
        })
        .collect();
    json!({
        "source": fetched.source.name(),
        "since": fetched.since.to_string(),
        "segments": segments,
    })
}
