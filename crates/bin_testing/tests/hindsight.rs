//! `docs/hindsight/` holds what agents wanted and worked around: a file per
//! want, a `# title`, a blank line, a header, a blank line, then one
//! `- <card ref> — <what it cost>` line per card that wanted it. The header is
//! an optional `Card: <card ref>` naming the card that builds the want, then
//! one or two lines saying what the want is and what going without it costs.
//! A want's count is its number of entries, so
//! this holds every line to that shape: a wrapped or prose line would inflate
//! the count, and a card named twice in one file would count itself twice.
//!
//! It lives in `bin_testing` because that crate is std-only, so the check
//! builds and runs in seconds: `cargo test -p bin_testing --test hindsight`.

use std::collections::HashSet;
use std::path::Path;

/// Whether `id` is a headway word-id: lowercase words joined by hyphens.
fn word_id(id: &str) -> bool {
    let words: Vec<_> = id.split('-').collect();
    words.len() >= 2
        && words
            .iter()
            .all(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase()))
}

/// Whether `card` (the part after `headway:`) is `<board>/<word-id>`.
fn card_ref(card: &str) -> Result<&str, &'static str> {
    let (board, id) = card.split_once('/').ok_or("a card ref without its board")?;
    if board.is_empty() || !word_id(id) {
        return Err("not a headway:<board>/<word-id> ref");
    }
    Ok(card)
}

/// The card an entry line names, or why the line is not an entry.
fn entry(line: &str) -> Result<&str, &'static str> {
    let rest = line
        .strip_prefix("- headway:")
        .ok_or("not `- headway:<board>/<word-id> — …`")?;
    let (card, said) = rest
        .split_once(" — ")
        .ok_or("no ` — ` after the card ref")?;
    if said.trim().is_empty() {
        return Err("nothing said about what it cost");
    }
    card_ref(card)
}

/// Why the header between the title and the entries is not an optional
/// `Card: headway:<board>/<word-id>` then one or two lines of what it is.
fn header(lines: &[&str]) -> Result<(), &'static str> {
    if lines.iter().any(|l| l.starts_with("- ")) {
        return Err("no blank line between the header and the entries");
    }
    if lines.iter().any(|l| l.chars().count() > 84) {
        return Err("a header line past 84 columns");
    }
    let said = match lines.first().and_then(|l| l.strip_prefix("Card: headway:")) {
        Some(card) => card_ref(card).map(|_| &lines[1..])?,
        None => lines,
    };
    if said.first().is_some_and(|l| l.starts_with("TLDR:")) {
        return Err("no `TLDR:` label; the line under the title says it");
    }
    if said.is_empty() || said.len() > 2 || said.iter().any(|l| l.starts_with("Card:")) {
        return Err("a header is one or two lines of what the want is and costs");
    }
    Ok(())
}

#[test]
fn every_want_is_a_title_and_one_line_per_card_that_wanted_it() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/hindsight");
    for file in std::fs::read_dir(&dir).expect("a hindsight directory") {
        let path = file.expect("a readable entry").path();
        let name = path.display();
        // Keeps the directory in git once the last want is built.
        if path.file_name().is_some_and(|f| f == ".gitkeep") {
            continue;
        }
        assert!(
            path.extension().is_some_and(|e| e == "md"),
            "{name}: only wants, as .md"
        );
        let text = std::fs::read_to_string(&path).expect("a readable want");
        let mut lines = text.lines();
        let title = lines
            .next()
            .and_then(|l| l.strip_prefix("# "))
            .unwrap_or("");
        assert!(
            !title.trim().is_empty(),
            "{name}: the first line is `# <the want>`"
        );
        assert_eq!(
            lines.next(),
            Some(""),
            "{name}: a blank line after the title"
        );
        let head: Vec<_> = lines.by_ref().take_while(|l| !l.is_empty()).collect();
        header(&head).unwrap_or_else(|why| panic!("{name}: header: {why}"));
        let first = head.len() + 4;
        let mut cards = HashSet::new();
        for (n, line) in lines.enumerate() {
            let card = entry(line).unwrap_or_else(|why| panic!("{name}:{}: {why}", n + first));
            assert!(cards.insert(card), "{name}: {card} wants it twice");
        }
        assert!(!cards.is_empty(), "{name}: a want nobody has wanted");
    }
}
