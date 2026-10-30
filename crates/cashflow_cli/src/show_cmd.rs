//! `cashflow show <record-id>`: one transaction's facts and what decided
//! its account, read from [`cashflow::detail`], the same model the app's
//! detail view draws.

use cashflow::CashflowDir;
use cashflow::chart::Chart;
use cashflow::detail::{self, Decision, TxnDetail, Written};
use cashflow::projects::Projects;
use ledgerdb::{Amount, Commodities, TxnFlag};
use serde_json::{Value, json};

use crate::args::{BooksArgs, Run};
use crate::books_cmd::loaded_books;
use crate::journal_cmd::day_of;
use crate::vault_cmd::Session;

/// `cashflow show <record-id>`: print the record's detail, or with `--json`
/// the same as one object. A record the loaded books don't hold is an
/// error.
pub async fn show(
    dir: &CashflowDir,
    run: &Run,
    record: &str,
    books: &BooksArgs,
    json: bool,
) -> Result<(), String> {
    let session = Session::open_if_logged_in(dir, run).await?;
    let loaded = loaded_books(dir, session.as_ref(), books)?;
    let projects = match &session {
        Some(session) => {
            let txn = session.txn()?;
            Chart::load(&session.ndb, &txn, session.vault.author()).projects()
        }
        None => Projects::default(),
    };
    let Some(detail) = detail::of(&loaded.books, &loaded.inputs, &projects, record) else {
        return Err(format!(
            "no transaction with id '{record}' in the loaded journal (is its layer off, or its segment not synced here?)"
        ));
    };
    let commodities = &loaded.books.ledger().journal().commodities;
    if json {
        println!("{}", detail_json(&detail, commodities));
    } else {
        print!("{}", detail_text(&detail, commodities));
    }
    Ok(())
}

/// The ledger-cli mark of `flag`, with the space after it.
fn mark(flag: TxnFlag) -> &'static str {
    match flag {
        TxnFlag::Uncleared => "",
        TxnFlag::Cleared => "* ",
        TxnFlag::Pending => "! ",
    }
}

/// `written` as `<day> by <agentium ref>`.
fn written_text(written: &Written) -> String {
    match &written.agentium {
        Some(by) => format!("{} by {by}", day_of(written.at)),
        None => day_of(written.at).to_string(),
    }
}

/// The detail as the text `show` prints.
fn detail_text(d: &TxnDetail, commodities: &Commodities) -> String {
    let amount = |a: Amount| a.display(commodities).to_string();
    let code = d.code.as_ref().map_or(String::new(), |c| format!("({c}) "));
    let mut out = format!("{} {}{code}{}\n", d.date, mark(d.flag), d.payee);
    out += &format!("  id       {}\n", d.record);
    out += &format!(
        "  source   {}  layer {}  imported {}\n",
        d.source,
        d.layer,
        day_of(d.imported)
    );
    out += &format!("  segment  {}\n\n", d.segment);

    let width = d
        .postings
        .iter()
        .map(|p| p.account.len())
        .max()
        .unwrap_or(0);
    for p in &d.postings {
        out += &format!("  {:<width$}  {:>14}", p.account, amount(p.amount));
        if let Some(home) = p.home.filter(|h| h.commodity != p.amount.commodity) {
            out += &format!("  = {}", amount(home));
        }
        if let Some(project) = &p.project {
            out += &format!("  [{}", project.project);
            if project.root != p.account {
                out += &format!(" from {}", project.root);
            }
            out += "]";
        }
        out += "\n";
    }

    out += &format!("\nwhy: {}\n", decision_text(&d.decided, commodities));
    if let Some(written) = d.decided.written() {
        out += &format!("  written {}\n", written_text(written));
    }
    for rejected in &d.rejected {
        out += &format!("  {rejected}\n");
    }
    if !d.meta.is_empty() {
        out += "\nmeta:\n";
        for m in &d.meta {
            out += &format!("  {}: {}\n", m.key, m.value);
        }
    }
    if !d.comments.is_empty() {
        out += "\ncomments:\n";
        for c in &d.comments {
            out += &format!("  {c}\n");
        }
    }
    out
}

/// One line on what decided the counter leg.
fn decision_text(decided: &Decision, commodities: &Commodities) -> String {
    match decided {
        Decision::AsImported => "as imported: it has no counter leg to categorize".to_owned(),
        Decision::Unknown { default } => {
            format!("unknown: nothing has categorized it, so it stays on {default}")
        }
        Decision::Rule {
            id,
            account,
            patterns,
            ..
        } => {
            let from = id
                .as_ref()
                .map_or("from the rules file".to_owned(), |id| id.clone());
            let quoted: Vec<String> = patterns.iter().map(|p| format!("{p:?}")).collect();
            let matching = if quoted.is_empty() {
                String::new()
            } else {
                format!(", matching {}", quoted.join(" or "))
            };
            format!("rule {from} sends it to {account}{matching}")
        }
        Decision::Override { account, .. } => format!("override to {account}"),
        Decision::Split { legs, .. } => {
            let legs: Vec<String> = legs
                .iter()
                .map(|l| format!("{} {}", l.account, l.amount.display(commodities)))
                .collect();
            format!("split into {}", legs.join(", "))
        }
        Decision::Transfer { other, .. } => format!("transfer linked with {other}"),
    }
}

/// The detail as `show --json` prints it.
fn detail_json(d: &TxnDetail, commodities: &Commodities) -> Value {
    let amount = |a: Amount| a.display(commodities).to_string();
    json!({
        "record_id": d.record,
        "date": d.date.to_string(),
        "payee": d.payee,
        "code": d.code,
        "flag": match d.flag {
            TxnFlag::Uncleared => "uncleared",
            TxnFlag::Cleared => "cleared",
            TxnFlag::Pending => "pending",
        },
        "source": d.source,
        "layer": d.layer,
        "segment": d.segment.to_string(),
        "imported": d.imported,
        "postings": d.postings.iter().map(|p| json!({
            "account": p.account,
            "amount": amount(p.amount),
            "qty": p.amount.qty,
            "home": p.home.map(amount),
            "own": p.own,
            "project": p.project.as_ref().map(|f| json!({"project": f.project, "root": f.root})),
        })).collect::<Vec<_>>(),
        "meta": d.meta.iter().map(|m| json!({"key": m.key, "value": m.value})).collect::<Vec<_>>(),
        "comments": d.comments,
        "decided": decision_json(&d.decided, commodities),
        "rejected": d.rejected,
    })
}

/// [`Decision`] as an object whose `by` names the tier, in `categorize`'s
/// words (`untouched`, `default`, `rule`, `override`, `split`, `transfer`).
fn decision_json(decided: &Decision, commodities: &Commodities) -> Value {
    let written = decided
        .written()
        .map(|w| json!({"at": w.at, "agentium": w.agentium}));
    match decided {
        Decision::AsImported => json!({"by": "untouched"}),
        Decision::Unknown { default } => json!({"by": "default", "account": default}),
        Decision::Rule {
            id,
            account,
            patterns,
            ..
        } => json!({
            "by": "rule", "rule_id": id, "account": account, "patterns": patterns,
            "written": written,
        }),
        Decision::Override { account, .. } => {
            json!({"by": "override", "account": account, "written": written})
        }
        Decision::Split { legs, .. } => json!({
            "by": "split",
            "legs": legs.iter().map(|l| json!({
                "account": l.account,
                "amount": l.amount.display(commodities).to_string(),
                "qty": l.amount.qty,
            })).collect::<Vec<_>>(),
            "written": written,
        }),
        Decision::Transfer { other, .. } => {
            json!({"by": "transfer", "other": other, "written": written})
        }
    }
}
