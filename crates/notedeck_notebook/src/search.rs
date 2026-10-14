//! The Notebook's answer to the chrome's universal search (the Cmd-K bar, see
//! [`notedeck::search`]): the nodes on every one of the account's canvases.
//!
//! The canvases are the shared canvas fold's views (the
//! [`NotebookCache`](crate::NotebookCache) the open canvas and the inline
//! `notebook:` chips read), so a node edited a moment ago is found by its new
//! text. Pending nodes (proposals hidden from a closed canvas) are left out:
//! there is nothing on the canvas to open. A node matches:
//!
//! - **by reference**: the query is its `notebook:<word-id>`, or the bare
//!   word-id;
//! - **by title**: the shared [`fuzzy_match`] on its title, the first
//!   non-blank line of its text as its inline chip shows it (a file, link or
//!   group node with no text is titled by its label, file or url);
//! - **by body**: every word of the query appears somewhere in its text,
//!   label, file or url, ignoring case. That scores zero, below any title
//!   match, as the bar expects of a body-only match.
//!
//! Hits rank by that score, then by when the node was last edited, newest
//! first. Each opens through its `notebook:<word-id>` reference, which the
//! app's [`NotebookRefParser`](crate::reference::NotebookRefParser) resolves to
//! its creation note and the chrome routes to the Notebook, which switches to
//! the node's canvas, selects it and pans it into view.
//!
//! Nothing is indexed ahead of the query: the bar asks only when its query
//! changes, and a scan of a few thousand nodes' text is well under a frame.

use notedeck::fuzzy::fuzzy_match;
use notedeck::{OpenUri, SearchHit, SearchHits, SearchQuery};

use crate::event::{CanvasView, NodeView};
use crate::render::{UNTITLED_NODE, node_title};
use crate::wordid;

/// What a node hit is marked as in the bar.
const KIND_NODE: &str = "Node";

/// The score of a query that names the node by its word-id: above any title
/// match, so the node it names lists first.
const EXACT: i32 = 1 << 20;

/// One node that matched, before the cut to the provider's cap.
struct Ranked<'a> {
    /// The canvas it sits on.
    canvas: &'a CanvasView,
    /// The node.
    node: &'a NodeView,
    /// How well it matched (see the module docs).
    score: i32,
}

/// Answer `query` from `canvases`, the account's folded canvases, into `out`,
/// best first, up to its cap.
#[profiling::function]
pub(crate) fn search(canvases: &[CanvasView], query: &SearchQuery, out: &mut SearchHits) {
    let text = query.text;
    // A pasted `notebook:<word-id>`, or a bare one, names one node.
    let words = wordid::parse_ref(text).or_else(|| {
        (wordid::three_words_end(text.as_bytes(), 0) == Some(text.len())).then_some(text)
    });
    let mut positions = Vec::new();
    let mut ranked: Vec<Ranked> = Vec::new();
    for canvas in canvases {
        for node in &canvas.nodes {
            let Some(score) = score(node, text, words, &mut positions) else {
                continue;
            };
            ranked.push(Ranked {
                canvas,
                node,
                score,
            });
        }
    }

    ranked.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(b.node.edited_at.cmp(&a.node.edited_at))
    });
    for r in ranked {
        if out.is_full() {
            break;
        }
        out.push(hit(&r));
    }
}

/// How well `text` matches `node`, the best of its ways to match (see the
/// module docs), or `None` when it doesn't. `words` is the word-id the query
/// names, if it names one.
fn score(
    node: &NodeView,
    text: &str,
    words: Option<&str>,
    positions: &mut Vec<usize>,
) -> Option<i32> {
    if let Some(words) = words
        && wordid::encode(node.id.bytes()) == words
    {
        return Some(EXACT);
    }
    if let Some(score) = fuzzy_match(text, title(node), positions) {
        return Some(score);
    }
    let content = &node.content;
    let fields = [
        Some(content.text.as_str()),
        content.label.as_deref(),
        content.file.as_deref(),
        content.url.as_deref(),
    ];
    let found = |word: &str| {
        fields
            .iter()
            .flatten()
            .any(|field| contains_ignoring_case(field, word))
    };
    text.split_whitespace().all(found).then_some(0)
}

/// What the bar lists `node` as: the title its inline chip shows, or, for a
/// node with no text, its label, file or url.
fn title(node: &NodeView) -> &str {
    let content = &node.content;
    let titled = node_title(&content.text);
    if titled != UNTITLED_NODE {
        return titled;
    }
    [&content.label, &content.file, &content.url]
        .into_iter()
        .flatten()
        .map(|s| s.trim())
        .find(|s| !s.is_empty())
        .unwrap_or(UNTITLED_NODE)
}

/// Whether `needle` appears in `haystack`, comparing each character
/// lowercased. Allocates nothing.
fn contains_ignoring_case(haystack: &str, needle: &str) -> bool {
    haystack.char_indices().any(|(at, _)| {
        let mut rest = lowercased(&haystack[at..]);
        lowercased(needle).all(|c| rest.next() == Some(c))
    })
}

/// The characters of `s`, each lowercased.
fn lowercased(s: &str) -> impl Iterator<Item = char> + '_ {
    s.chars().flat_map(char::to_lowercase)
}

/// The bar's row for `r`: the node's title, marked as a node, with the canvas
/// it sits on, opening its `notebook:` reference.
fn hit(r: &Ranked) -> SearchHit {
    let uri = OpenUri::new(wordid::node_ref(r.node.id.bytes()));
    SearchHit::new(title(r.node), uri, r.score)
        .kind(KIND_NODE)
        .subtitled(r.canvas.title.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Geometry, NodeContent, NodeKind};
    use nostrdb_net::NoteId;
    use notedeck::search::HITS_PER_PROVIDER;

    /// A text node `id` reading `text`, last edited at `edited_at`.
    fn node(id: u8, text: &str, edited_at: u64) -> NodeView {
        NodeView {
            id: NoteId::new([id; 32]),
            author: [0; 32],
            kind: NodeKind::Text,
            geo: Geometry {
                x: 0,
                y: 0,
                w: 200,
                h: 80,
            },
            z: String::new(),
            color: None,
            content: NodeContent {
                text: text.to_owned(),
                ..NodeContent::default()
            },
            created_at: 0,
            placed_at: 0,
            edited_at,
        }
    }

    /// A canvas titled `title` holding `nodes`.
    fn canvas(title: &str, nodes: Vec<NodeView>) -> CanvasView {
        CanvasView {
            id: title.to_lowercase(),
            author: [0; 32],
            title: title.to_owned(),
            members: Vec::new(),
            open: false,
            created_at: 0,
            nodes,
            edges: Vec::new(),
            pending: Vec::new(),
        }
    }

    /// The hits `query` finds in `canvases`, best first.
    fn found(canvases: &[CanvasView], query: &str) -> SearchHits {
        let mut out = SearchHits::new(HITS_PER_PROVIDER);
        search(canvases, &SearchQuery { text: query }, &mut out);
        out
    }

    /// The titles of `hits`, best first.
    fn titles(hits: &SearchHits) -> Vec<&str> {
        hits.hits().iter().map(|h| h.title.as_str()).collect()
    }

    /// A title query finds its node as a hit that opens its `notebook:`
    /// reference, subtitled with its canvas, and misses the nodes it doesn't
    /// match.
    #[test]
    fn a_title_query_finds_its_node_and_opens_its_reference() {
        let canvases = [
            canvas("Ideas", vec![node(1, "Relay reconnect\nbackoff", 10)]),
            canvas("Trips", vec![node(2, "Packing list", 20)]),
        ];
        let out = found(&canvases, "relay reconnect");
        let [hit] = out.hits() else {
            panic!("one hit: {:?}", out.hits());
        };
        assert_eq!(hit.title, "Relay reconnect");
        assert_eq!(hit.subtitle.as_deref(), Some("Ideas"));
        assert_eq!(hit.kind.as_deref(), Some(KIND_NODE));
        assert_eq!(hit.uri.reference, wordid::node_ref(&[1; 32]));
    }

    /// A word only in a node's body finds it, ignoring case, below a node
    /// whose title matches; every word of the query must appear.
    #[test]
    fn a_body_word_finds_its_node_below_a_title_match() {
        let canvases = [canvas(
            "Ideas",
            vec![
                node(1, "Groceries\nbuy ZEBRA crossing paint", 30),
                node(2, "zebra", 10),
                node(3, "Unrelated", 40),
            ],
        )];
        let out = found(&canvases, "zebra");
        assert_eq!(titles(&out), ["zebra", "Groceries"]);
        assert_eq!(out.hits()[1].score, 0);

        assert_eq!(titles(&found(&canvases, "crossing paint")), ["Groceries"]);
        assert!(found(&canvases, "crossing glue").hits().is_empty());
    }

    /// A pasted `notebook:<word-id>`, or the bare word-id, finds exactly its
    /// node, first.
    #[test]
    fn a_word_id_names_its_node() {
        let canvases = [canvas(
            "Ideas",
            vec![node(1, "First", 10), node(2, "Second", 20)],
        )];
        let words = wordid::encode(&[2; 32]);
        for query in [wordid::node_ref(&[2; 32]), words] {
            let out = found(&canvases, &query);
            assert_eq!(titles(&out).first(), Some(&"Second"), "{query}");
            assert_eq!(out.hits()[0].score, EXACT);
        }
    }

    /// A node with no text is titled by its label, file or url, and found by
    /// it; pending nodes are never found.
    #[test]
    fn a_textless_node_is_titled_by_its_label_and_pending_nodes_are_skipped() {
        let mut group = node(1, "", 10);
        group.content.label = Some("Reading group".to_owned());
        let mut ideas = canvas("Ideas", vec![group]);
        ideas.pending.push(node(2, "Reading proposal", 20));

        let out = found(&[ideas], "reading");
        assert_eq!(titles(&out), ["Reading group"]);
    }

    /// Ties in score break by the newest edit, and the answer stops at the
    /// provider's cap.
    #[test]
    fn ties_break_newest_first_and_stop_at_the_cap() {
        let nodes = (0..20).map(|i| node(i, "note", u64::from(i))).collect();
        let out = found(&[canvas("Ideas", nodes)], "note");
        assert_eq!(out.hits().len(), HITS_PER_PROVIDER);
        let first = &out.hits()[0];
        assert_eq!(first.uri.reference, wordid::node_ref(&[19; 32]));
    }

    /// The case-folding compare finds a needle anywhere, across scripts, and
    /// not a needle that isn't there.
    #[test]
    fn contains_ignoring_case_folds_both_sides() {
        assert!(contains_ignoring_case("Buy ZEBRA paint", "zebra"));
        assert!(contains_ignoring_case("ÉCOLE", "école"));
        assert!(contains_ignoring_case("abc", ""));
        assert!(!contains_ignoring_case("zebr", "zebra"));
    }
}
