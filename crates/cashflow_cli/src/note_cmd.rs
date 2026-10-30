//! `cashflow note <record-id> [<text>...]`: the notes left on one
//! transaction, or a new one, through [`cashflow::notes`], the same thread
//! the app's detail view shows. `show` prints them with [`notes_text`].

use cashflow::notes::{self, Note};
use nostrdb_net::Pubkey;
use serde_json::{Value, json};

use crate::journal_cmd::day_of;
use crate::vault_cmd::Session;

/// List `record`'s notes, or with `text` add one and publish it.
pub async fn note(
    session: &mut Session,
    record: &str,
    text: Option<&str>,
    json: bool,
) -> Result<(), String> {
    let Some(text) = text else {
        let txn = session.txn()?;
        let thread = notes::notes_of(&session.ndb, &txn, session.vault.author(), record);
        let me = session.vault.author();
        if json {
            println!("{}", notes_json(&thread, me));
        } else if thread.is_empty() {
            println!("no notes on {record}");
        } else {
            print!("{}", notes_text(&thread, me));
        }
        return Ok(());
    };
    let written =
        notes::post(&session.vault, &session.ndb, record, text, 0).map_err(|e| e.to_string())?;
    session.publish(&[written.frame]).await?;
    if json {
        println!(
            "{}",
            json!({"record_id": record, "created_at": written.created_at, "text": text.trim()})
        );
    } else {
        println!("noted on {record}{}", session.offline_note());
    }
    Ok(())
}

/// `secs` as `YYYY-MM-DD HH:MM UTC`.
fn when(secs: u64) -> String {
    let of_day = secs % 86_400;
    format!(
        "{} {:02}:{:02} UTC",
        day_of(secs),
        of_day / 3600,
        of_day % 3600 / 60
    )
}

/// Who wrote a note: "you" for `me`, else the start of their key.
fn who(author: &Pubkey, me: &Pubkey) -> String {
    if author == me {
        "you".to_owned()
    } else {
        author.hex()[..8].to_owned()
    }
}

/// `thread` as text, a line per note: when, who, what. Continuation lines
/// of a multi-line note are indented under it.
pub fn notes_text(thread: &[Note], me: &Pubkey) -> String {
    let mut out = String::new();
    for note in thread {
        let mut lines = note.text.lines();
        out += &format!(
            "  {}  {}  {}\n",
            when(note.created_at),
            who(&note.author, me),
            lines.next().unwrap_or("")
        );
        for line in lines {
            out += &format!("      {line}\n");
        }
    }
    out
}

/// `thread` as `note --json` and `show --json` print it.
pub fn notes_json(thread: &[Note], me: &Pubkey) -> Value {
    thread
        .iter()
        .map(|n| {
            json!({
                "author": n.author.hex(),
                "mine": &n.author == me,
                "created_at": n.created_at,
                "text": n.text,
            })
        })
        .collect()
}
