//! The commands that read or write the journal itself: `journal ls`,
//! `journal verify` and `import`.

use cashflow::CashflowDir;
use cashflow::import::{self, ImportInput};
use cashflow::journal::{self, SegmentSummary};
use ledgerdb::Date;
use serde_json::json;

use crate::args::ImportArgs;

/// `cashflow journal ls`: one line per segment, oldest import first.
pub fn ls(dir: &CashflowDir, json: bool) -> Result<(), String> {
    let listing = journal::list(&journal::store(dir)).map_err(|e| e.to_string())?;
    for bad in &listing.bad {
        eprintln!("cashflow: {bad}");
    }
    if json {
        let rows: Vec<_> = listing.segments.iter().map(summary_json).collect();
        println!("{}", serde_json::Value::Array(rows));
        return Ok(());
    }
    for s in &listing.segments {
        let errors = match s.errors {
            0 => String::new(),
            n => format!("  {n} parse errors"),
        };
        println!(
            "{}  {}  {}  {}  {} txns  {} prices{errors}",
            s.hash,
            day_of(s.header.imported),
            s.header.layer,
            s.header.source,
            s.txns,
            s.prices
        );
    }
    Ok(())
}

fn summary_json(s: &SegmentSummary) -> serde_json::Value {
    json!({
        "hash": s.hash.to_string(),
        "source": s.header.source,
        "layer": s.header.layer,
        "imported": s.header.imported,
        "txns": s.txns,
        "prices": s.prices,
        "errors": s.errors,
    })
}

/// `cashflow journal verify`: every segment's hash, header and body.
pub fn verify(dir: &CashflowDir, json: bool) -> Result<(), String> {
    let verified = journal::verify(&journal::store(dir)).map_err(|e| e.to_string())?;
    let total = verified.ok.len() + verified.damaged.len() + verified.bad.len();
    if json {
        let damaged: Vec<_> = verified
            .damaged
            .iter()
            .map(|d| {
                json!({
                    "hash": d.hash.to_string(),
                    "errors": d.errors.iter().map(ToString::to_string).collect::<Vec<_>>(),
                })
            })
            .collect();
        let bad: Vec<String> = verified.bad.iter().map(ToString::to_string).collect();
        println!(
            "{}",
            json!({ "ok": verified.ok.len(), "damaged": damaged, "bad": bad })
        );
    } else {
        for d in &verified.damaged {
            let first = &d.errors[0];
            println!(
                "damaged {}: {} lines don't parse, first {first}",
                d.hash,
                d.errors.len()
            );
        }
        for bad in &verified.bad {
            println!("bad {bad}");
        }
        if verified.is_clean() {
            println!("ok: {total} segments");
        }
    }
    if verified.is_clean() {
        return Ok(());
    }
    Err(format!(
        "{} of {total} segments failed verification",
        verified.damaged.len() + verified.bad.len()
    ))
}

/// `cashflow import <importer> [<path>...]`.
pub fn import(dir: &CashflowDir, args: &ImportArgs) -> Result<(), String> {
    let importer = import::importer(&args.importer).ok_or_else(|| {
        format!(
            "no importer named '{}' (importers: {})",
            args.importer,
            import::NAMES.join(", ")
        )
    })?;
    let input = ImportInput {
        source: args
            .source
            .clone()
            .unwrap_or_else(|| importer.name().to_owned()),
        paths: args.paths.clone(),
    };
    let written = import::write(
        &journal::store(dir),
        &*importer,
        &input,
        args.layer.as_deref(),
        None,
    )
    .map_err(|e| e.to_string())?;
    if args.json {
        println!(
            "{}",
            json!({
                "hash": written.hash.to_string(),
                "new": written.new,
                "source": written.header.source,
                "layer": written.header.layer,
                "imported": written.header.imported,
                "txns": written.txns,
                "prices": written.prices,
            })
        );
        return Ok(());
    }
    let verb = if written.new { "imported" } else { "unchanged" };
    println!(
        "{verb} {}  {} txns  {} prices  source {}  layer {}",
        written.hash, written.txns, written.prices, written.header.source, written.header.layer
    );
    Ok(())
}

/// The UTC day of unix time `secs`.
fn day_of(secs: u64) -> Date {
    Date::from_days((secs / 86_400).min(u64::from(u32::MAX)) as u32)
}
