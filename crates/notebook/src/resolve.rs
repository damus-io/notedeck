//! Resolving the selectors a person types — a `notebook:<word-id>` ref, a
//! `nostr:naddr`/coordinate, a full id, or a unique prefix — against the folded
//! vault. One set of resolvers serves the `notebook` CLI and the app's tools
//! (which `nd notebook …` calls), so a ref resolves the same way in both.
//!
//! Every function answers a `String` error naming what didn't match, ready to
//! print.

use nostrdb_net::{NoteId, Pubkey};

use crate::event::{
    self, CanvasView, EdgeView, LongformNote, NodeView, NoteAddress, NotebookTarget,
};
use crate::wordid;

/// Every node on `view`, surfaced and pending alike: an edit can address a
/// pending proposal as readily as a surfaced node.
pub fn all_nodes(view: &CanvasView) -> impl Iterator<Item = &NodeView> {
    view.nodes.iter().chain(view.pending.iter())
}

/// Resolve a node selector to its id; see [`find_node`].
pub fn resolve_node(view: &CanvasView, sel: &str) -> Result<NoteId, String> {
    find_node(view, sel).map(|n| n.id)
}

/// Resolve a node selector, accepting (in order): a full 64-char hex id; a
/// `notebook:<word-id>` reference (the `notebook:` scheme is required — a bare
/// word-id is not a reference); or a unique hex prefix. Pending nodes count.
pub fn find_node<'a>(view: &'a CanvasView, sel: &str) -> Result<&'a NodeView, String> {
    if let Ok(id) = NoteId::from_hex(sel) {
        return all_nodes(view)
            .find(|n| n.id == id)
            .ok_or_else(|| format!("no node matching '{sel}'"));
    }

    // A `notebook:<word-id>` reference: match by re-encoding each node — exactly
    // how a git short hash is resolved.
    if let Some(words) = wordid::parse_ref(sel)
        && let Some(n) = all_nodes(view).find(|n| wordid::encode(n.id.bytes()) == words)
    {
        return Ok(n);
    }

    let sel = sel.to_lowercase();
    let mut hits = all_nodes(view).filter(|n| n.id.hex().starts_with(&sel));
    match (hits.next(), hits.next()) {
        (Some(n), None) => Ok(n),
        (Some(_), Some(_)) => Err(format!("ambiguous node prefix '{sel}'")),
        _ => Err(format!("no node matching '{sel}'")),
    }
}

/// Resolve an edge selector by full id or unique id prefix.
pub fn resolve_edge<'a>(view: &'a CanvasView, sel: &str) -> Result<&'a EdgeView, String> {
    if let Some(e) = view.edges.iter().find(|e| e.id == sel) {
        return Ok(e);
    }
    let mut hits = view.edges.iter().filter(|e| e.id.starts_with(sel));
    match (hits.next(), hits.next()) {
        (Some(e), None) => Ok(e),
        (Some(_), Some(_)) => Err(format!("ambiguous edge prefix '{sel}'")),
        _ => Err(format!("no edge matching '{sel}'")),
    }
}

/// Resolve a `show` selector to a [`NotebookTarget`] across the whole notebook.
///
/// The flat `notebook:<word-id>` namespace ([`event::resolve_ref`]) is layered
/// *between* the explicit forms and the loose fallbacks — deliberately, so the
/// namespace stays authoritative: an explicit `naddr`/coordinate wins first (it
/// names one exact kind), a well-formed `notebook:` ref that resolves to nothing
/// **fails here** rather than falling through to a prefix match (a mistyped
/// word-id can't silently resolve to some other item), and only a bare
/// (scheme-less) selector reaches the raw-hex/`d`-prefix fallbacks.
pub fn resolve_target(
    sel: &str,
    canvases: &[CanvasView],
    notes: &[LongformNote],
) -> Result<NotebookTarget, String> {
    // 1. An explicit `nostr:naddr` / bare coordinate — each kind-discriminated, so
    //    it names exactly one document type.
    if let Some(NoteAddress { author, d }) = event::parse_canvas_naddr(sel) {
        return canvases
            .iter()
            .find(|c| c.id == d && c.author == *author.bytes())
            .map(|c| NotebookTarget::Canvas {
                author,
                d: c.id.clone(),
            })
            .ok_or_else(|| format!("no canvas matching '{sel}'"));
    }
    if let Some(NoteAddress { author, d }) = event::parse_longform_naddr(sel) {
        return notes
            .iter()
            .find(|n| n.d == d && n.author == *author.bytes())
            .map(|n| NotebookTarget::Note {
                author,
                d: n.d.clone(),
            })
            .ok_or_else(|| format!("no vault note matching '{sel}'"));
    }

    // 2. A `notebook:<word-id>` reference — resolved across the flat namespace
    //    (documents first, then nodes). Ref-shaped-but-unmatched fails.
    if let Some(word_id) = wordid::parse_ref(sel) {
        return event::resolve_ref(word_id, canvases, notes)
            .ok_or_else(|| format!("no notebook item matching '{sel}'"));
    }

    // 3. A full 64-char hex node id.
    if let Ok(id) = NoteId::from_hex(sel) {
        return canvases
            .iter()
            .flat_map(all_nodes)
            .find(|n| n.id == id)
            .map(|n| NotebookTarget::Node { id: n.id })
            .ok_or_else(|| format!("no node matching '{sel}'"));
    }

    // 4. Fallback: a unique hex-prefix node id, or a unique `d` prefix across notes
    //    and canvases — ambiguity across the union is rejected.
    prefix_target(sel, canvases, notes)
}

/// The loose-prefix arm of [`resolve_target`]: match a bare (scheme-less)
/// selector as a node id hex prefix or a note/canvas `d` prefix, requiring
/// exactly one hit across the whole notebook so an ambiguous prefix fails rather
/// than silently picking one.
fn prefix_target(
    sel: &str,
    canvases: &[CanvasView],
    notes: &[LongformNote],
) -> Result<NotebookTarget, String> {
    let hex = sel.to_lowercase();
    let nodes = canvases
        .iter()
        .flat_map(all_nodes)
        .filter(|n| n.id.hex().starts_with(&hex))
        .map(|n| NotebookTarget::Node { id: n.id });
    let docs = canvases
        .iter()
        .filter(|c| c.id.starts_with(sel))
        .map(|c| NotebookTarget::Canvas {
            author: Pubkey::new(c.author),
            d: c.id.clone(),
        });
    let longform = notes
        .iter()
        .filter(|n| n.d.starts_with(sel))
        .map(|n| NotebookTarget::Note {
            author: Pubkey::new(n.author),
            d: n.d.clone(),
        });
    let mut hits = nodes.chain(docs).chain(longform);
    match (hits.next(), hits.next()) {
        (Some(target), None) => Ok(target),
        (None, _) => Err(format!("no notebook item matching '{sel}'")),
        _ => Err(format!("ambiguous selector '{sel}'")),
    }
}

/// Resolve the canvas an edit targets to its id, against `author`'s folded
/// canvases: an explicit selector (a `nostr:naddr`/coordinate, a
/// `notebook:<word-id>` ref, or a literal/unique-prefix `d`), or — when omitted
/// — the sole canvas. There is no hard-coded default id: with several canvases
/// and no selector the caller must say which; with none it must seed first.
/// `canvases` is already scoped to one author by the fold, so a coordinate or
/// ref for another author's canvas simply won't match.
pub fn resolve_canvas_id(
    selector: Option<&str>,
    canvases: &[CanvasView],
) -> Result<String, String> {
    let Some(sel) = selector else {
        return match canvases {
            [only] => Ok(only.id.clone()),
            [] => Err("no canvas yet — run `notebook seed` to create one".into()),
            _ => Err(
                "several canvases — pass --canvas <ref|d> to pick one (see `notebook show`)".into(),
            ),
        };
    };

    // A `nostr:naddr` / bare coordinate naming a canvas.
    if let Some(NoteAddress { author, d }) = event::parse_canvas_naddr(sel) {
        return canvases
            .iter()
            .find(|c| c.id == d && c.author == *author.bytes())
            .map(|c| c.id.clone())
            .ok_or_else(|| format!("no canvas matching '{sel}'"));
    }

    // A `notebook:<word-id>` ref — must resolve to a *canvas* (a note/node ref
    // given as the canvas fails rather than falling through to a `d` prefix).
    if let Some(word_id) = wordid::parse_ref(sel) {
        return match event::resolve_ref(word_id, canvases, &[]) {
            Some(NotebookTarget::Canvas { d, .. }) => Ok(d),
            _ => Err(format!("no canvas matching '{sel}'")),
        };
    }

    // A literal or unique-prefix `d`.
    let mut hits = canvases.iter().filter(|c| c.id.starts_with(sel));
    match (hits.next(), hits.next()) {
        (Some(c), None) => Ok(c.id.clone()),
        (Some(_), Some(_)) => Err(format!("ambiguous canvas prefix '{sel}'")),
        _ => Err(format!("no canvas matching '{sel}'")),
    }
}

/// Resolve a vault-note selector against `notes`, accepting (in order): a
/// `nostr:naddr…`/coordinate reference; a `notebook:<word-id>` reference
/// (matched against each note's canonical ref); or a full `d` or unique `d`
/// prefix.
pub fn find_longform<'a>(notes: &'a [LongformNote], sel: &str) -> Result<&'a LongformNote, String> {
    // A nostr:naddr / bare coordinate → match by (author, d).
    if let Some(NoteAddress { author, d }) = event::parse_longform_naddr(sel) {
        return notes
            .iter()
            .find(|n| n.author == *author.bytes() && n.d == d)
            .ok_or_else(|| format!("no vault note matching '{sel}'"));
    }

    // A `notebook:<word-id>` reference → the note whose canonical ref equals it.
    // Ref-shaped but unmatched fails here rather than falling through to a `d`
    // prefix, so a mistyped word-id can't silently resolve to some other note.
    if wordid::parse_ref(sel).is_some() {
        return notes
            .iter()
            .find(|n| event::longform_ref(&Pubkey::new(n.author), &n.d) == sel)
            .ok_or_else(|| format!("no vault note matching '{sel}'"));
    }

    // Otherwise a full `d` or a unique `d` prefix.
    let mut hits = notes.iter().filter(|n| n.d.starts_with(sel));
    match (hits.next(), hits.next()) {
        (Some(n), None) => Ok(n),
        (Some(_), Some(_)) => Err(format!("ambiguous vault selector '{sel}'")),
        _ => Err(format!("no vault note matching '{sel}'")),
    }
}

/// A resolved `show` target, looked up and owned: what it renders and
/// serializes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shown {
    Canvas(CanvasView),
    Note(LongformNote),
    Node(NodeView),
}

impl Shown {
    /// Its machine form: a canvas, note or node JSON object.
    pub fn json(&self) -> serde_json::Value {
        match self {
            Self::Canvas(view) => event::canvas_json(view),
            Self::Note(note) => event::longform_json(note),
            Self::Node(node) => event::node_json(node),
        }
    }
}

/// Resolve `sel` with [`resolve_target`] and look up what it names.
pub fn show(sel: &str, canvases: &[CanvasView], notes: &[LongformNote]) -> Result<Shown, String> {
    // The lookups re-find a resolved target in the same slices it was matched
    // against, so a miss is a guard rather than a reachable path.
    let gone = || format!("'{sel}' went missing while resolving it");
    Ok(match resolve_target(sel, canvases, notes)? {
        NotebookTarget::Canvas { author, d } => Shown::Canvas(
            event::find_canvas(canvases, &author, &d)
                .ok_or_else(gone)?
                .clone(),
        ),
        NotebookTarget::Note { author, d } => Shown::Note(
            notes
                .iter()
                .find(|n| n.d == d && n.author == *author.bytes())
                .ok_or_else(gone)?
                .clone(),
        ),
        NotebookTarget::Node { id } => Shown::Node(
            canvases
                .iter()
                .flat_map(all_nodes)
                .find(|n| n.id == id)
                .ok_or_else(gone)?
                .clone(),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(author: [u8; 32], d: &str, title: &str) -> LongformNote {
        LongformNote {
            author,
            d: d.to_string(),
            title: title.to_string(),
            summary: None,
            content: String::new(),
            published_at: None,
            hashtags: Vec::new(),
            created_at: 0,
            deleted: false,
        }
    }

    /// A minimal folded canvas (no nodes/edges) for the resolver tests.
    fn canvas(author: [u8; 32], id: &str, title: &str) -> CanvasView {
        CanvasView {
            id: id.to_string(),
            author,
            title: title.to_string(),
            members: Vec::new(),
            open: false,
            created_at: 0,
            nodes: Vec::new(),
            edges: Vec::new(),
            pending: Vec::new(),
        }
    }

    #[test]
    fn find_longform_by_prefix_ref_and_naddr() {
        let a = [7u8; 32];
        let notes = vec![note(a, "abcdef", "One"), note(a, "abff00", "Two")];

        // A unique `d` prefix resolves; an ambiguous one is rejected.
        assert_eq!(find_longform(&notes, "abcd").unwrap().d, "abcdef");
        assert!(find_longform(&notes, "ab").is_err());
        assert!(find_longform(&notes, "zz").is_err());

        // The canonical `notebook:<word-id>` ref resolves to its own note.
        let reference = event::longform_ref(&Pubkey::new(a), "abcdef");
        assert_eq!(find_longform(&notes, &reference).unwrap().d, "abcdef");

        // A nostr:naddr for the note resolves too.
        let naddr = event::longform_naddr(&Pubkey::new(a), "abff00").unwrap();
        assert_eq!(find_longform(&notes, &naddr).unwrap().d, "abff00");

        // A well-formed ref that matches no note fails rather than falling through
        // to a `d`-prefix match.
        let missing = event::longform_ref(&Pubkey::new(a), "nope");
        assert!(find_longform(&notes, &missing).is_err());
    }

    #[test]
    fn resolve_target_dispatches_note_and_canvas() {
        let a = [7u8; 32];
        let pk = Pubkey::new(a);
        let canvases = vec![canvas(a, "c0ffee", "A Canvas")];
        let notes = vec![note(a, "abcdef", "An Article")];

        // A note's naddr and its `notebook:` ref both resolve to the note.
        let n_naddr = event::longform_naddr(&pk, "abcdef").unwrap();
        assert_eq!(
            resolve_target(&n_naddr, &canvases, &notes).unwrap(),
            NotebookTarget::Note {
                author: pk,
                d: "abcdef".to_string()
            }
        );
        let n_ref = event::longform_ref(&pk, "abcdef");
        assert!(matches!(
            resolve_target(&n_ref, &canvases, &notes).unwrap(),
            NotebookTarget::Note { .. }
        ));

        // A canvas's naddr and its `notebook:` ref both resolve to the canvas.
        let c_naddr = event::canvas_naddr(&pk, "c0ffee").unwrap();
        assert_eq!(
            resolve_target(&c_naddr, &canvases, &notes).unwrap(),
            NotebookTarget::Canvas {
                author: pk,
                d: "c0ffee".to_string()
            }
        );
        let c_ref = event::canvas_ref(&pk, "c0ffee");
        assert!(matches!(
            resolve_target(&c_ref, &canvases, &notes).unwrap(),
            NotebookTarget::Canvas { .. }
        ));

        // A well-formed `notebook:` ref matching nothing fails rather than falling
        // through to the loose prefix arm.
        let missing = event::canvas_ref(&pk, "nope");
        assert!(resolve_target(&missing, &canvases, &notes).is_err());

        // A bare `d` prefix resolves through the fallback arm (note vs. canvas).
        assert!(matches!(
            resolve_target("abc", &canvases, &notes).unwrap(),
            NotebookTarget::Note { .. }
        ));
        assert!(matches!(
            resolve_target("c0f", &canvases, &notes).unwrap(),
            NotebookTarget::Canvas { .. }
        ));
        assert!(resolve_target("zz", &canvases, &notes).is_err());
    }

    #[test]
    fn resolve_canvas_id_infers_sole_and_requires_selection() {
        let a = [7u8; 32];
        let pk = Pubkey::new(a);

        // No selector: infer the sole canvas, but require one when there are none or
        // several (no hard-coded default id).
        assert!(resolve_canvas_id(None, &[]).is_err());
        let one = vec![canvas(a, "c0ffee", "One")];
        assert_eq!(resolve_canvas_id(None, &one).unwrap(), "c0ffee");
        let two = vec![canvas(a, "c0ffee", "One"), canvas(a, "d00d00", "Two")];
        assert!(resolve_canvas_id(None, &two).is_err());

        // A selector picks a specific canvas: a `notebook:` ref, a naddr, or a `d`
        // prefix. A note ref (or an unmatched one) given as the canvas fails.
        let c_ref = event::canvas_ref(&pk, "d00d00");
        assert_eq!(resolve_canvas_id(Some(&c_ref), &two).unwrap(), "d00d00");
        let c_naddr = event::canvas_naddr(&pk, "c0ffee").unwrap();
        assert_eq!(resolve_canvas_id(Some(&c_naddr), &two).unwrap(), "c0ffee");
        assert_eq!(resolve_canvas_id(Some("d00"), &two).unwrap(), "d00d00");
        let note_ref = event::longform_ref(&pk, "c0ffee");
        assert!(resolve_canvas_id(Some(&note_ref), &two).is_err());
    }
}
