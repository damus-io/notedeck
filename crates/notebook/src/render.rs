//! The human text view of a notebook: the vault listing, a canvas, a node, a
//! longform note's body, and an edit's ack.
//!
//! Every function returns its text rather than printing it, so the `notebook`
//! CLI and the app's tools (which `nd notebook …` prints) show one rendering.
//! `color` says whether the muted parts (refs, ages, geometry) carry an ANSI
//! bright-black: the CLI passes whether its stdout is a terminal, and a tool
//! answering over the socket renders with color on and leaves stripping it to
//! the client, which is the one that knows where the text is going.

use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use nostrdb_net::Pubkey;

use crate::event::{self, CanvasView, LongformNote, NodeView, VaultDoc};
use crate::resolve::Shown;
use crate::wordid;

/// `s` in ANSI bright-black when `color`, else as is. Bright-black rather than
/// the SGR dim attribute because many terminals ignore dim, while the 16-color
/// palette is honored everywhere.
pub fn dim(color: bool, s: &str) -> String {
    if color {
        format!("\x1b[90m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

/// Wall-clock seconds since the Unix epoch (0 if the clock somehow predates it),
/// the `now` the listings measure ages against.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A compact relative age ("just now", "5m ago", "2h ago", "3d ago") for a
/// document's last edit. Deliberately dependency-light: the command line isn't
/// a localized surface, unlike the in-app vault sidebar's `edited_subtitle`.
pub fn ago(then: u64, now: u64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        0..=1 => "just now".to_string(),
        2..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

/// The first line of `text`, trimmed and truncated, for one-line listings.
pub fn one_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() > 60 {
        let cut: String = line.chars().take(57).collect();
        format!("{cut}…")
    } else if line.is_empty() {
        "(empty)".to_string()
    } else {
        line.to_string()
    }
}

/// A document title for listings, falling back when it has none yet (a
/// freshly-created draft).
pub fn title(title: &str) -> &str {
    let t = title.trim();
    if t.is_empty() { "(untitled)" } else { t }
}

/// A node's `notebook:<word-id>` ref, muted: what a person quotes to address it.
fn node_ref(color: bool, node: &NodeView) -> String {
    dim(color, &wordid::node_ref(node.id.bytes()))
}

/// A node's geometry, `(x,y w×h)`, muted.
fn geometry(color: bool, node: &NodeView) -> String {
    let g = &node.geo;
    dim(color, &format!("({},{} {}×{})", g.x, g.y, g.w, g.h))
}

/// Append `body`, ending it with a newline so a shell prompt isn't glued to it.
fn push_body(out: &mut String, body: &str) {
    out.push_str(body);
    if !body.ends_with('\n') {
        out.push('\n');
    }
}

/// The whole vault: a count, then one typed row per document (kind, title,
/// edited-age, ref), in the order given (newest-edited first from
/// [`event::list_vault`]).
pub fn vault_docs(docs: &[VaultDoc], now: u64, color: bool) -> String {
    let mut out = format!(
        "Vault ({} item{})\n",
        docs.len(),
        if docs.len() == 1 { "" } else { "s" }
    );
    for doc in docs {
        let _ = writeln!(
            out,
            "  {}  {}  {}  {}",
            dim(color, &format!("[{}]", doc.kind.label())),
            title(&doc.title),
            dim(color, &format!("edited {}", ago(doc.edited_at, now))),
            dim(color, &event::vault_doc_ref(doc)),
        );
    }
    out
}

/// The longform vault: a count, then one row per note (title, edited-age, ref),
/// with its summary on a muted second line when it has one.
pub fn vault(notes: &[LongformNote], now: u64, color: bool) -> String {
    let mut out = format!(
        "Vault ({} note{})\n",
        notes.len(),
        if notes.len() == 1 { "" } else { "s" }
    );
    for n in notes {
        let author = Pubkey::new(n.author);
        let _ = writeln!(
            out,
            "  {}  {}  {}",
            title(&n.title),
            dim(color, &format!("edited {}", ago(n.created_at, now))),
            dim(color, &event::longform_ref(&author, &n.d)),
        );
        if let Some(summary) = n
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let _ = writeln!(out, "    {}", dim(color, &one_line(summary)));
        }
    }
    out
}

/// The markdown bodies of `notes`. A single note is its raw body only, so
/// `notebook vault <note> > note.md` round-trips; more than one is separated by
/// a muted ref header so the concatenation stays navigable.
pub fn vault_bodies<'a>(
    notes: impl ExactSizeIterator<Item = &'a LongformNote>,
    color: bool,
) -> String {
    let headed = notes.len() > 1;
    let mut out = String::new();
    for (i, n) in notes.enumerate() {
        if headed {
            if i > 0 {
                out.push('\n');
            }
            let reference = event::longform_ref(&Pubkey::new(n.author), &n.d);
            let _ = writeln!(out, "{}", dim(color, &reference));
        }
        push_body(&mut out, &n.content);
    }
    out
}

/// A canvas: its title, its nodes one line each, its edges, and the pending
/// proposals a closed canvas holds back.
pub fn canvas(view: &CanvasView, color: bool) -> String {
    let mut out = format!(
        "{}{}\n",
        view.title,
        if view.open { "  [open]" } else { "" }
    );

    let _ = writeln!(out, "\nNodes ({})", view.nodes.len());
    for n in &view.nodes {
        node_line(&mut out, n, color);
    }
    if !view.edges.is_empty() {
        let _ = writeln!(out, "\nEdges ({})", view.edges.len());
        for e in &view.edges {
            let _ = writeln!(
                out,
                "  {} → {}  {}",
                dim(color, &wordid::node_ref(e.from.bytes())),
                dim(color, &wordid::node_ref(e.to.bytes())),
                dim(color, &e.id),
            );
        }
    }
    if !view.pending.is_empty() {
        let _ = writeln!(
            out,
            "\nPending ({}) — proposals on a closed canvas",
            view.pending.len()
        );
        for n in &view.pending {
            node_line(&mut out, n, color);
        }
    }
    out
}

/// A node as one listing line: its first line of text, geometry and ref.
fn node_line(out: &mut String, n: &NodeView, color: bool) {
    let _ = writeln!(
        out,
        "  {}  {}  {}",
        one_line(&n.content.text),
        geometry(color, n),
        node_ref(color, n),
    );
}

/// A node shown on its own: a muted ref and geometry line, then its whole text
/// (not the truncated line [`canvas`] lists it with).
pub fn node_body(n: &NodeView, color: bool) -> String {
    let mut out = format!("{}  {}\n", node_ref(color, n), geometry(color, n));
    push_body(&mut out, &n.content.text);
    out
}

/// What `show <refs...>` prints: each target in its natural shape, in order —
/// a whole canvas, a note's raw markdown body, or a node's body.
pub fn shown(items: &[Shown], color: bool) -> String {
    let mut out = String::new();
    for item in items {
        match item {
            Shown::Canvas(view) => out.push_str(&canvas(view, color)),
            Shown::Note(note) => out.push_str(&vault_bodies(std::iter::once(note), color)),
            Shown::Node(node) => out.push_str(&node_body(node, color)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ago_buckets_by_magnitude() {
        assert_eq!(ago(100, 100), "just now");
        assert_eq!(ago(100, 101), "just now");
        assert_eq!(ago(0, 30), "30s ago");
        assert_eq!(ago(0, 120), "2m ago");
        assert_eq!(ago(0, 7200), "2h ago");
        assert_eq!(ago(0, 3 * 86_400), "3d ago");
        // A clock that ran backwards clamps to "just now" rather than underflowing.
        assert_eq!(ago(500, 100), "just now");
    }

    #[test]
    fn title_falls_back_when_blank() {
        assert_eq!(title("  Hi "), "Hi");
        assert_eq!(title("   "), "(untitled)");
    }
}
