//! `cashflow sync`: move journal segments between this machine and the
//! remotes `config.toml` lists, through [`cashflow::sync::run`].

use cashflow::journal;
use cashflow::manifest;
use cashflow::sync::{self, Plan, SyncJob, SyncReport, TransportReport};
use cashflow::{CashflowDir, Config};
use ledgerdb::SegmentHash;
use serde_json::json;

use crate::args::{Run, Write};
use crate::journal_cmd;
use crate::vault_cmd::Session;

/// `cashflow sync [--transport <name>] [--dry-run] [--json]`: fetch what
/// the vault's manifests say this machine lacks, admit it, push each remote
/// what it lacks, and publish manifests for what arrived.
pub async fn sync(
    dir: &CashflowDir,
    run: &Run,
    only: Option<&str>,
    write: Write,
) -> Result<(), String> {
    // A config mistake fails before the relay is dialled.
    let config = Config::load(dir).map_err(|e| e.to_string())?;
    let transports =
        sync::transports(&config, dir, &journal_cmd::host(), only).map_err(|e| e.to_string())?;
    let mut session = Session::open(dir, run).await?;

    let store = journal::store(dir);
    let listing = journal::list(&store).map_err(|e| e.to_string())?;
    for bad in &listing.bad {
        eprintln!("cashflow: {bad}");
    }
    // Copy the plan out: it borrows the txn, which must not outlive this
    // block or be held across the transfer.
    let job = {
        let txn = session.txn()?;
        let manifests = manifest::load(&session.ndb, &txn, session.vault.author());
        SyncJob::new(&Plan::new(&manifests, &listing.segments), &listing.segments)
    };

    let staging = dir.staging();
    let dry_run = write.dry_run;
    let report = tokio::task::spawn_blocking(move || {
        sync::run(&transports, &store, &staging, &job, dry_run)
    })
    .await
    .map_err(|e| format!("sync stopped: {e}"))?;

    let admitted: Vec<SegmentHash> = report.admitted().collect();
    // What arrived came from manifests, so it has them; this only writes
    // one for a segment whose manifest went missing meanwhile.
    if !dry_run {
        journal_cmd::publish_manifests(dir, Some(&mut session), &admitted).await?;
    }

    if write.json {
        print_json(&report);
    } else {
        print_text(&report);
    }
    let failed = report
        .transports
        .iter()
        .filter(|t| !t.errors.is_empty() || !t.admit.rejected.is_empty())
        .count();
    match failed {
        0 => Ok(()),
        n => Err(format!(
            "{n} of {} transports failed or served a bad file",
            report.transports.len()
        )),
    }
}

/// `n segment` or `n segments`.
fn segments(n: usize) -> String {
    if n == 1 {
        "1 segment".to_owned()
    } else {
        format!("{n} segments")
    }
}

/// One line per transport, then what it moved, rejected and failed, then
/// what is still missing.
fn print_text(report: &SyncReport) {
    for t in &report.transports {
        if report.dry_run {
            println!(
                "{}: would offer {}, would fetch {}",
                t.name,
                segments(t.pushed.len()),
                segments(t.fetched.len())
            );
        } else {
            println!(
                "{}: pushed {}, fetched {}, admitted {}, rejected {}",
                t.name,
                segments(t.pushed.len()),
                segments(t.fetched.len()),
                t.admit.admitted.len(),
                t.admit.rejected.len()
            );
        }
        for hash in &t.pushed {
            let verb = if report.dry_run { "offer" } else { "pushed" };
            println!("    {verb} {hash}");
        }
        for hash in &t.admit.admitted {
            println!("    admitted {hash}");
        }
        for bad in &t.admit.rejected {
            println!("    rejected {bad} (left in staging/{}/)", t.name);
        }
        for err in &t.errors {
            println!("    error: {err}");
        }
    }
    if !report.still_missing.is_empty() {
        println!(
            "still missing: {} no transport delivered (every total leaves them out until they arrive)",
            segments(report.still_missing.len())
        );
        for hash in &report.still_missing {
            println!("    {hash}");
        }
    }
}

fn hashes(list: &[SegmentHash]) -> Vec<String> {
    list.iter().map(ToString::to_string).collect()
}

fn transport_json(t: &TransportReport) -> serde_json::Value {
    json!({
        "name": t.name,
        "fetched": hashes(&t.fetched),
        "admitted": hashes(&t.admit.admitted),
        "present": hashes(&t.admit.present),
        "rejected": t.admit.rejected.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "pushed": hashes(&t.pushed),
        "errors": t.errors.iter().map(ToString::to_string).collect::<Vec<_>>(),
    })
}

fn print_json(report: &SyncReport) {
    let transports: Vec<_> = report.transports.iter().map(transport_json).collect();
    println!(
        "{}",
        json!({
            "dry_run": report.dry_run,
            "transports": transports,
            "still_missing": hashes(&report.still_missing),
        })
    );
}
