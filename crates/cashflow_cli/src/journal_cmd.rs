//! The commands that read or write the journal itself: `journal ls`,
//! `journal status`, `journal verify`, `journal publish`, `journal rm` and
//! `import`, and the segment manifests they keep in the vault
//! ([`cashflow::manifest`]).

use std::collections::HashSet;

use cashflow::import::{self, ImportInput, Rows};
use cashflow::journal::{self, Listing, SegmentSummary};
use cashflow::manifest::{self, Manifest};
use cashflow::sync::Plan;
use cashflow::{CashflowDir, Config, Settings};
use ledgerdb::{Date, SegmentHash};
use serde_json::{Value, json};

use crate::args::{ImportArgs, Write};
use crate::books_cmd;
use crate::vault_cmd::Session;

/// Overrides the host name a manifest records, for a machine with more
/// than one data dir.
const HOST_ENV: &str = "CASHFLOW_HOST";

/// The host this run publishes manifests as: `$CASHFLOW_HOST`, else the
/// machine's hostname.
fn host() -> String {
    std::env::var(HOST_ENV)
        .ok()
        .filter(|h| !h.trim().is_empty())
        .unwrap_or_else(|| gethostname::gethostname().to_string_lossy().into_owned())
}

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
        "first": s.first.map(|d| d.to_string()),
        "last": s.last.map(|d| d.to_string()),
        "txns": s.txns,
        "prices": s.prices,
        "bytes": s.bytes,
        "errors": s.errors,
    })
}

/// A local segment as one `status`/`publish` line.
fn summary_line(s: &SegmentSummary) -> String {
    manifest_line(s.hash, &Manifest::of(s, ""))
}

/// Segment `hash` as one line from what `m` says of it:
/// `<hash>  <import day>  <layer>  <source>  <first>..<last>  <n> txns`.
fn manifest_line(hash: SegmentHash, m: &Manifest<'_>) -> String {
    let dates = match (m.first, m.last) {
        (Some(first), Some(last)) => format!("{first}..{last}"),
        _ => "no dates".to_owned(),
    };
    format!(
        "{hash}  {}  {}  {}  {dates}  {} txns",
        day_of(m.imported),
        m.layer,
        m.source,
        m.txns
    )
}

/// `n segment` or `n segments`.
fn segments(n: usize) -> String {
    if n == 1 {
        "1 segment".to_owned()
    } else {
        format!("{n} segments")
    }
}

/// The readable segments, after printing the unreadable ones to stderr.
fn listing(dir: &CashflowDir) -> Result<Listing, String> {
    let listing = journal::list(&journal::store(dir)).map_err(|e| e.to_string())?;
    for bad in &listing.bad {
        eprintln!("cashflow: {bad}");
    }
    Ok(listing)
}

/// `cashflow journal status`: what the journal lacks, what the vault lacks,
/// what is retired but still here, and a verify summary. Logged out, only
/// the verify summary.
pub fn status(dir: &CashflowDir, session: Option<&Session>, json: bool) -> Result<(), String> {
    let listing = listing(dir)?;
    // `list` read and parsed every segment already: that is a verify.
    let damaged = listing.segments.iter().filter(|s| s.errors > 0).count();
    let verify = json!({
        "ok": listing.segments.len() - damaged,
        "damaged": damaged,
        "bad": listing.bad.len(),
    });
    let verify_line = format!(
        "verify: {} ok, {damaged} damaged, {} bad",
        listing.segments.len() - damaged,
        listing.bad.len()
    );
    let Some(session) = session else {
        if json {
            println!("{}", json!({ "logged_in": false, "verify": verify }));
        } else {
            println!("not logged in: no vault manifests to compare (run `cashflow login`)");
            println!("{verify_line}");
        }
        return Ok(());
    };
    let txn = session.txn()?;
    let manifests = manifest::load(&session.ndb, &txn, session.vault.author());
    let plan = Plan::new(&manifests, &listing.segments);

    if json {
        let missing: Vec<_> = plan
            .missing_locally
            .iter()
            .map(|m| {
                let body = m.manifest;
                json!({
                    "hash": m.hash.to_string(),
                    "host": body.host,
                    "source": body.source,
                    "layer": body.layer,
                    "imported": body.imported,
                    "first": body.first.map(|d| d.to_string()),
                    "last": body.last.map(|d| d.to_string()),
                    "txns": body.txns,
                    "prices": body.prices,
                    "bytes": body.bytes,
                })
            })
            .collect();
        let local = |list: &[&SegmentSummary]| -> Vec<Value> {
            list.iter().map(|s| summary_json(s)).collect()
        };
        println!(
            "{}",
            json!({
                "logged_in": true,
                "in_sync": plan.is_in_sync(),
                "segments": listing.segments.len(),
                "manifests": manifests.len(),
                "missing_locally": missing,
                "unpublished": local(&plan.unpublished),
                "retired_here": local(&plan.retired_here),
                "verify": verify,
            })
        );
        return Ok(());
    }

    if plan.is_in_sync() {
        println!(
            "in sync: {} here, {} known to the vault",
            segments(listing.segments.len()),
            segments(manifests.len())
        );
    }
    if !plan.missing_locally.is_empty() {
        println!(
            "missing here: {} (every total leaves them out until they arrive)",
            segments(plan.missing_locally.len())
        );
        for group in plan.missing_by_host() {
            let txns: u64 = group.segments.iter().map(|m| m.manifest.txns).sum();
            println!(
                "  on {}: {}, {txns} txns",
                group.host,
                segments(group.segments.len())
            );
            for m in group.segments {
                println!("    {}", manifest_line(m.hash, m.manifest));
            }
        }
    }
    if !plan.unpublished.is_empty() {
        println!(
            "unpublished: {} the vault has no manifest of (run `cashflow journal publish`)",
            segments(plan.unpublished.len())
        );
        for s in &plan.unpublished {
            println!("    {}", summary_line(s));
        }
    }
    if !plan.retired_here.is_empty() {
        println!(
            "retired but still here: {}, already left out of every total (`cashflow journal rm <hash>` moves the file to the trash)",
            segments(plan.retired_here.len())
        );
        for s in &plan.retired_here {
            println!("    {}", summary_line(s));
        }
    }
    println!("{verify_line}");
    Ok(())
}

/// `cashflow journal publish`: write the manifest of every local segment
/// the vault has none of.
pub async fn publish(dir: &CashflowDir, session: &mut Session, write: Write) -> Result<(), String> {
    let listing = listing(dir)?;
    let host = host();
    let mut frames = Vec::new();
    let mut hashes = HashSet::new();
    {
        let txn = session.txn()?;
        let manifests = manifest::load(&session.ndb, &txn, session.vault.author());
        let plan = Plan::new(&manifests, &listing.segments);
        for summary in &plan.unpublished {
            if !write.dry_run {
                let body = Manifest::of(summary, &host);
                let written = manifest::write(&session.vault, &session.ndb, summary.hash, &body, 0)
                    .map_err(|e| e.to_string())?;
                frames.push(written.frame);
            }
            hashes.insert(summary.hash);
        }
    }
    session.publish(&frames).await?;
    let rows: Vec<&SegmentSummary> = listing
        .segments
        .iter()
        .filter(|s| hashes.contains(&s.hash))
        .collect();
    if write.json {
        let rows: Vec<_> = rows.iter().map(|s| summary_json(s)).collect();
        println!(
            "{}",
            json!({ "dry_run": write.dry_run, "host": host, "published": rows })
        );
        return Ok(());
    }
    let verb = if write.dry_run {
        "would publish"
    } else {
        "published"
    };
    for s in &rows {
        println!("{verb} {}", summary_line(s));
    }
    let tail = if write.dry_run {
        ""
    } else {
        session.offline_note()
    };
    println!(
        "{verb} {} as host {host}{tail}",
        segments(rows.len()).replace("segment", "manifest")
    );
    Ok(())
}

/// Publish the manifest of each segment in `hashes` the vault has no live
/// one of, as host [`host`]: what `import` and `prices fetch` run on the
/// segments they wrote. Logged out, says how to publish them later.
pub async fn publish_manifests(
    dir: &CashflowDir,
    session: Option<&mut Session>,
    hashes: &[SegmentHash],
) -> Result<(), String> {
    if hashes.is_empty() {
        return Ok(());
    }
    let Some(session) = session else {
        eprintln!(
            "cashflow: not logged in, so the vault has no manifest of this import yet: `cashflow journal publish` writes it after `cashflow login`"
        );
        return Ok(());
    };
    let store = journal::store(dir);
    let host = host();
    let mut frames = Vec::new();
    {
        let txn = session.txn()?;
        let manifests = manifest::load(&session.ndb, &txn, session.vault.author());
        for &hash in hashes {
            let written = manifest::publish_segment(
                &session.vault,
                &session.ndb,
                &manifests,
                &store,
                hash,
                &host,
            )
            .map_err(|e| e.to_string())?;
            frames.extend(written.map(|w| w.frame));
        }
    }
    session.publish(&frames).await
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

/// `cashflow journal rm <hash>`: retire one segment, named among the local
/// segments and (logged in) the vault's manifests: move its file to
/// `journal/.trash/` if it is here, and mark its manifest retired.
pub async fn rm(
    dir: &CashflowDir,
    session: Option<&mut Session>,
    prefix: &str,
    json: bool,
) -> Result<(), String> {
    let Some(session) = session else {
        let removed = journal::remove(&journal::store(dir), prefix).map_err(|e| e.to_string())?;
        if json {
            println!(
                "{}",
                json!({ "hash": removed.hash.to_string(), "to": removed.to, "retired": false })
            );
        } else {
            println!("removed {} -> {}", removed.hash, removed.to.display());
            println!("not logged in: other copies of the journal still load it");
        }
        return Ok(());
    };

    let store = journal::store(dir);
    let local = store.list().map_err(|e| e.to_string())?;
    let host = host();
    let (hash, frame) = {
        let txn = session.txn()?;
        let manifests = manifest::load(&session.ndb, &txn, session.vault.author());
        let known = manifests.iter().filter_map(manifest::hash_of);
        let hash = journal::resolve(local.iter().copied().chain(known), prefix)
            .map_err(|e| e.to_string())?;
        let stored = manifest::find(&manifests, hash);
        let frame = match stored {
            Some(m) if m.body.retired => None,
            Some(m) => Some(
                manifest::write(
                    &session.vault,
                    &session.ndb,
                    hash,
                    &m.body.retire(),
                    m.created_at,
                )
                .map_err(|e| e.to_string())?
                .frame,
            ),
            None => {
                // Here but never published: describe it before it moves.
                let summary = journal::summarize(&store, hash).map_err(|e| e.to_string())?;
                let body = Manifest::of(&summary, &host).retire();
                Some(
                    manifest::write(&session.vault, &session.ndb, hash, &body, 0)
                        .map_err(|e| e.to_string())?
                        .frame,
                )
            }
        };
        (hash, frame)
    };
    let to = if local.contains(&hash) {
        Some(store.trash(hash).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let wrote = frame.is_some();
    session
        .publish(&frame.into_iter().collect::<Vec<_>>())
        .await?;

    if json {
        println!(
            "{}",
            json!({ "hash": hash.to_string(), "to": to, "retired": true })
        );
        return Ok(());
    }
    match &to {
        Some(to) => println!("removed {hash} -> {}", to.display()),
        None => println!("{hash} isn't here"),
    }
    if wrote {
        println!(
            "retired {hash} in the vault: every copy of the journal leaves it out{}",
            session.offline_note()
        );
    } else {
        println!("{hash} was already retired in the vault");
    }
    Ok(())
}

/// How many skipped lines `import` prints before summing up the rest.
const SKIPPED_SHOWN: usize = 20;

/// `cashflow import <importer> [<path>...]`: logged in, the rows retired
/// segments hold count as new, and each segment written gets its manifest.
pub async fn import(
    dir: &CashflowDir,
    mut session: Option<Session>,
    args: &ImportArgs,
) -> Result<(), String> {
    let config = Config::load(dir).map_err(|e| e.to_string())?;
    let synced = match &session {
        Some(session) => {
            let txn = session.txn()?;
            import::Synced::from_settings(&Settings::load(
                &session.ndb,
                &txn,
                session.vault.author(),
            ))
        }
        None => import::Synced::default(),
    };
    let importer = import::importer(&args.importer, &config, &synced, dir).ok_or_else(|| {
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
    let retired = match &session {
        Some(session) => books_cmd::retired(session)?,
        None => HashSet::new(),
    };
    let written = import::write(
        &journal::store(dir),
        &retired,
        &*importer,
        &input,
        args.layer.as_deref(),
        None,
    )
    .map_err(|e| e.to_string())?;
    let hashes: Vec<SegmentHash> = written
        .other_layers
        .iter()
        .chain([&written])
        .filter_map(|w| w.hash)
        .collect();
    publish_manifests(dir, session.as_mut(), &hashes).await?;
    // A skipped line's number is in the one input file the importers that
    // skip (`ledger`) take.
    let file = args
        .paths
        .first()
        .map_or_else(String::new, |p| format!("{}:", p.display()));
    if args.json {
        let mut out = segment_json(&written);
        out["other_layers"] = written.other_layers.iter().map(segment_json).collect();
        out["notes"] = json!(written.notes);
        out["skipped"] = json!(
            written
                .skipped
                .iter()
                .map(|e| json!({
                    "line": e.line,
                    "col": e.col,
                    "error": e.message,
                }))
                .collect::<Vec<_>>()
        );
        out["left_out"] = json!(
            written
                .left_out
                .iter()
                .map(|l| json!({
                    "id": l.id,
                    "why": l.why,
                }))
                .collect::<Vec<_>>()
        );
        println!("{out}");
        return Ok(());
    }
    for error in written.skipped.iter().take(SKIPPED_SHOWN) {
        eprintln!("cashflow: skipped {file}{error}");
    }
    if let Some(more) = written.skipped.len().checked_sub(SKIPPED_SHOWN)
        && more > 0
    {
        eprintln!("cashflow: … and {more} more (see --json)");
    }
    for left in written.left_out.iter().take(SKIPPED_SHOWN) {
        eprintln!("cashflow: left out {} ({})", left.id, left.why);
    }
    if let Some(more) = written.left_out.len().checked_sub(SKIPPED_SHOWN)
        && more > 0
    {
        eprintln!("cashflow: … and {more} more left out (see --json)");
    }
    for note in &written.notes {
        eprintln!("cashflow: {note}");
    }
    let mut skipped = match written.skipped.len() {
        0 => String::new(),
        n => format!("  {n} skipped"),
    };
    if !written.left_out.is_empty() {
        skipped.push_str(&format!("  {} left out", written.left_out.len()));
    }
    for other in &written.other_layers {
        println!("{}", segment_line(other, ""));
    }
    println!("{}", segment_line(&written, &skipped));
    Ok(())
}

/// One segment `import` wrote, or would have, as JSON.
fn segment_json(written: &import::Written) -> Value {
    json!({
        "hash": written.hash.map(|h| h.to_string()),
        "new": written.new,
        "txn_rows": rows_json(written.txn_rows),
        "price_rows": rows_json(written.price_rows),
        "source": written.header.source,
        "layer": written.header.layer,
        "imported": written.header.imported,
        "txns": written.txns,
        "prices": written.prices,
    })
}

/// One segment `import` wrote, or would have, as its summary line, with
/// `tail` after it.
fn segment_line(written: &import::Written, tail: &str) -> String {
    let rows = format!(
        "txns {}  prices {}",
        rows_text(written.txn_rows),
        rows_text(written.price_rows)
    );
    let Some(hash) = written.hash else {
        return format!(
            "nothing new  {rows}  source {}  layer {}{tail}",
            written.header.source, written.header.layer
        );
    };
    let verb = if written.new { "imported" } else { "unchanged" };
    format!(
        "{verb} {hash}  {} txns  {} prices  ({rows})  source {}  layer {}{tail}",
        written.txns, written.prices, written.header.source, written.header.layer
    )
}

/// `{"new": 1, "changed": 0, "unchanged": 89}`.
fn rows_json(rows: Rows) -> Value {
    json!({ "new": rows.new, "changed": rows.changed, "unchanged": rows.unchanged })
}

/// `1 new/0 changed/89 unchanged`.
fn rows_text(rows: Rows) -> String {
    format!(
        "{} new/{} changed/{} unchanged",
        rows.new, rows.changed, rows.unchanged
    )
}

/// The UTC day of unix time `secs`.
fn day_of(secs: u64) -> Date {
    Date::from_days((secs / 86_400).min(u64::from(u32::MAX)) as u32)
}
