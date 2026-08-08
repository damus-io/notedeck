//! The notebook's inline [`KindRenderer`]s, contributed to notedeck's registry so
//! a reference to notebook-owned content draws inline wherever it's mentioned:
//!
//! - [`LongformKindRenderer`] for longform notes (kind 30023) — a `nostr:naddr…`
//!   reference, inline in a text node or as a note-embed node, previews the note.
//!   Kind-1 (`nevent`/`note`) references are already covered by the columns app's
//!   `NoteKindRenderer`; longform is the notebook's primary document type.
//! - [`NotebookNodeRenderer`] for canvas nodes (kind 1606) — a `notebook:<word-id>`
//!   or `nostr:` reference draws a live chip/card of the node's *current* content,
//!   folded through the shared [`NotebookCache`](crate::NotebookCache).

use std::cell::RefCell;
use std::rc::Rc;

use enostr::Pubkey;
use nostrdb::{Ndb, Note, Transaction};
use notedeck::{
    ColorTheme, KindRenderRequest, KindRenderResponse, KindRenderer, NoteContext, RenderContext,
};

use crate::NotebookCache;
use crate::editor::document_icon;
use crate::event::{self, KIND_LONGFORM, KIND_NODE, NodeEvent, NotebookEvent};

/// How many bytes of content to preview when a longform note has no `summary`
/// tag. A borrowed prefix (cut back to a char boundary) — never an owned
/// truncation — so this stays allocation-free on the per-frame render path.
const CONTENT_PREVIEW_BYTES: usize = 280;

/// Title shown for a longform note that carries no `title` tag.
const UNTITLED: &str = "Untitled";

/// Renders a kind-30023 longform note referenced from elsewhere. Registered into
/// [`notedeck::KindRendererRegistry`] at app startup via
/// [`Notebook::kind_renderers`](crate::Notebook).
///
/// Honors [`req.context`](KindRenderRequest::context): a compact one-line **chip**
/// (a document glyph + title) when the reference flows inline in a text node's
/// prose, and a fuller **block** (title heading + body preview) when a surface
/// gives the reference its own box — a note-embed node, say.
pub struct LongformKindRenderer;

impl KindRenderer for LongformKindRenderer {
    fn id(&self) -> &'static str {
        "notebook.longform"
    }

    fn name(&self) -> &'static str {
        "Longform"
    }

    fn kinds(&self) -> &'static [u32] {
        &[KIND_LONGFORM]
    }

    fn render(
        &self,
        ui: &mut egui::Ui,
        note_context: &mut NoteContext,
        req: &KindRenderRequest,
    ) -> KindRenderResponse {
        let theme = ColorTheme::current(ui.ctx());
        let response = match req.context {
            notedeck::RenderContext::Inline => longform_chip_ui(ui, &theme, req.note),
            _ => longform_block_ui(ui, &theme, note_context, req),
        };
        KindRenderResponse::new(response)
    }
}

/// A compact, single-line inline reference to a longform note: a document glyph
/// followed by the note's title, in a small rounded pill — the in-prose shape
/// ([`notedeck::RenderContext::Inline`]), versus the full [`longform_block_ui`].
fn longform_chip_ui(ui: &mut egui::Ui, theme: &ColorTheme, note: &Note) -> egui::Response {
    let title = tag_value(note, "title").unwrap_or(UNTITLED);
    notedeck_ui::inline_chip(ui, theme, title, |ui, size| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
        document_icon(ui.painter(), rect.center(), size, theme.text_muted);
    })
}

/// The block embed of a longform note ([`notedeck::RenderContext::Embed`], and
/// the default for any non-inline surface): its title heading and a short body
/// preview — the `summary` tag when present, otherwise the head of the content.
fn longform_block_ui(
    ui: &mut egui::Ui,
    _theme: &ColorTheme,
    note_context: &mut NoteContext,
    req: &KindRenderRequest,
) -> egui::Response {
    ui.vertical(|ui| {
        ui.label(
            egui::RichText::new(tag_value(req.note, "title").unwrap_or(UNTITLED))
                .heading()
                .strong(),
        );
        // Prefer the note's own summary; fall back to the head of the body. Both
        // are borrowed from the note, so no per-frame allocation.
        let (preview, truncated) = match tag_value(req.note, "summary") {
            Some(summary) => (summary, false),
            None => head(req.note.content(), CONTENT_PREVIEW_BYTES),
        };
        notedeck_ui::markdown::render_markdown_with_refs(ui, note_context, req.txn, preview);
        if truncated {
            ui.weak("…");
        }
    })
    .response
}

/// The value of the first tag named `name` (e.g. `title`, `summary`), borrowed
/// from the note and non-empty, or `None`. Allocation-free — scans the note's
/// tags each call rather than folding a [`LongformNote`](crate::event::LongformNote).
fn tag_value<'a>(note: &'a Note, name: &str) -> Option<&'a str> {
    for tag in note.tags() {
        if tag.get_str(0) == Some(name)
            && let Some(value) = tag.get_str(1)
            && !value.is_empty()
        {
            return Some(value);
        }
    }
    None
}

/// Title shown for a node with no text (a freshly-created, still-empty box).
const UNTITLED_NODE: &str = "Untitled node";

/// A node's *current* content, folded live from the shared canvas cache — what the
/// chip/embed shows instead of the kind-1606 creation snapshot (which never carries
/// later edits). Owned so the cache borrow drops before drawing.
struct LiveNode {
    /// The node's current text body (markdown).
    text: String,
    /// The title of the canvas the node currently lives on.
    canvas_title: String,
}

/// Renders a notebook node (kind 1606) referenced inline, e.g. by a
/// `notebook:<word-id>` word id ([`crate::reference`]) or a `nostr:` event
/// reference. Registered into [`notedeck::KindRendererRegistry`] at app startup via
/// [`Notebook::kind_renderers`](crate::Notebook).
///
/// The kind-1606 note is only the node's *creation-time* snapshot; its current text
/// (and which canvas it sits on) come from folding the owning canvas's later edits.
/// So we fold that canvas through the app's shared [`NotebookCache`] — the same
/// realtime-pumped fold the foreground canvas reads, so a live edit shows on the
/// chip — and fall back to the raw snapshot when the canvas isn't folded locally.
///
/// Honors [`req.context`](KindRenderRequest::context): a compact one-line **chip**
/// (a node glyph + the node's title) inline in prose, and a fuller **card** (title
/// + content preview + which canvas) as a block embed.
pub struct NotebookNodeRenderer {
    /// Shared with the app's foreground UI and its reference parser (see
    /// [`Notebook::cache`](crate::Notebook)), so a node drawn inline folds the same
    /// cached canvas the open surface does — once per account, not once per surface.
    cache: Rc<RefCell<NotebookCache>>,
}

impl NotebookNodeRenderer {
    /// Build the renderer over the app's shared canvas cache.
    pub(crate) fn new(cache: Rc<RefCell<NotebookCache>>) -> Self {
        Self { cache }
    }
}

impl KindRenderer for NotebookNodeRenderer {
    fn id(&self) -> &'static str {
        "notebook.node"
    }

    fn name(&self) -> &'static str {
        "Notebook node"
    }

    fn kinds(&self) -> &'static [u32] {
        &[KIND_NODE]
    }

    #[profiling::function]
    fn render(
        &self,
        ui: &mut egui::Ui,
        note_context: &mut NoteContext,
        req: &KindRenderRequest,
    ) -> KindRenderResponse {
        let note = req.note;
        let theme = ColorTheme::current(ui.ctx());
        let Some(NotebookEvent::Node(node)) = event::parse(note) else {
            return KindRenderResponse::new(ui.weak("invalid notebook node"));
        };

        // Fold the owning canvas (shared cache) for the node's *current* text and
        // canvas, falling back to the creation snapshot when it isn't folded locally
        // (a not-yet-synced canvas, or another account's node). The borrow drops
        // with `live` (owned) before we draw.
        let live = live_node(&self.cache, note_context.ndb, req.txn, &node);
        let text = live
            .as_ref()
            .map(|l| l.text.as_str())
            .unwrap_or(&node.content.text);

        let response = match req.context {
            RenderContext::Inline => node_chip_ui(ui, &theme, node_title(text)),
            _ => node_embed_ui(ui, &theme, text, live.as_ref()),
        };
        // Clicking the chip/card opens the node in Notebook (shared helper).
        notedeck::open_on_click(ui, response, note)
    }
}

/// The node's current text + owning-canvas title, folded live from the shared
/// [`NotebookCache`]. `None` when the node's canvas isn't folded locally — the
/// caller falls back to the creation snapshot. Reads through the memoized
/// [`with_canvases`](crate::NotebookCache) so a chip reuses this frame's fold.
fn live_node(
    cache: &Rc<RefCell<NotebookCache>>,
    ndb: &Ndb,
    txn: &Transaction,
    node: &NodeEvent,
) -> Option<LiveNode> {
    let author = Pubkey::new(node.canvas_author);
    cache
        .borrow_mut()
        .with_canvases(ndb, txn, &author, |canvases| {
            let canvas = event::find_canvas(canvases, &author, &node.canvas_id)?;
            let view = canvas
                .nodes
                .iter()
                .chain(canvas.pending.iter())
                .find(|n| n.id.bytes() == &node.id)?;
            Some(LiveNode {
                text: view.content.text.clone(),
                canvas_title: canvas.title.clone(),
            })
        })
        .flatten()
}

/// The chip/heading title for a node: the first non-blank line of its text, or
/// [`UNTITLED_NODE`] for an empty node. Borrows `text` (or a `'static` fallback), so
/// the per-frame render path stays allocation-free.
fn node_title(text: &str) -> &str {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.is_empty() { UNTITLED_NODE } else { line }
}

/// A compact, single-line inline reference to a node: a canvas-node glyph followed
/// by the node's title, in a small rounded pill — the in-prose shape
/// ([`notedeck::RenderContext::Inline`]), versus the fuller [`node_embed_ui`].
fn node_chip_ui(ui: &mut egui::Ui, theme: &ColorTheme, title: &str) -> egui::Response {
    notedeck_ui::inline_chip(ui, theme, title, |ui, size| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
        node_glyph(ui.painter(), rect.center(), size, theme.text_muted);
    })
}

/// The block embed of a node ([`notedeck::RenderContext::Embed`]): its title, a
/// short body preview, and — when the owning canvas is folded — which canvas it
/// lives on. Preview text is borrowed (no owned truncation) to keep the path light.
fn node_embed_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    text: &str,
    live: Option<&LiveNode>,
) -> egui::Response {
    ui.vertical(|ui| {
        ui.label(egui::RichText::new(node_title(text)).heading().strong());
        let (preview, truncated) = head(text, CONTENT_PREVIEW_BYTES);
        ui.weak(preview);
        if truncated {
            ui.weak("…");
        }
        // The canvas the node lives on, when it's folded (glyph + name).
        if let Some(live) = live {
            ui.horizontal(|ui| {
                let size = ui.text_style_height(&egui::TextStyle::Small);
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
                node_glyph(ui.painter(), rect.center(), size, theme.text_muted);
                ui.weak(&live.canvas_title);
            });
        }
    })
    .response
}

/// A simple canvas-node glyph — a rounded box with one ruled line — drawn into a
/// square of `size`. Distinct from the longform [`document_icon`] (a taller page):
/// a node is a box on the canvas.
fn node_glyph(painter: &egui::Painter, center: egui::Pos2, size: f32, color: egui::Color32) {
    let stroke = egui::Stroke::new((size * 0.08).max(1.0), color);
    let boxed = egui::Rect::from_center_size(center, egui::vec2(size * 0.82, size * 0.6));
    painter.rect_stroke(
        boxed,
        egui::CornerRadius::same((size * 0.16) as u8),
        stroke,
        egui::StrokeKind::Inside,
    );
    // One ruled line inside the box, suggesting the node's text.
    let x0 = boxed.left() + size * 0.16;
    let x1 = boxed.right() - size * 0.16;
    painter.line_segment([egui::pos2(x0, center.y), egui::pos2(x1, center.y)], stroke);
}

/// `s` cut to at most `max` bytes on a char boundary, paired with whether it was
/// actually cut. Borrows `s` (no owned truncation), keeping the render path
/// allocation-free.
fn head(s: &str, max: usize) -> (&str, bool) {
    if s.len() <= max {
        return (s, false);
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (&s[..end], true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_returns_whole_short_string_untruncated() {
        assert_eq!(head("short", 280), ("short", false));
    }

    #[test]
    fn head_cuts_long_ascii_at_the_limit() {
        let s = "a".repeat(300);
        let (preview, truncated) = head(&s, 280);
        assert_eq!(preview.len(), 280);
        assert!(truncated);
    }

    #[test]
    fn head_backs_off_to_a_char_boundary() {
        // "é" is two bytes; a cut landing mid-char must back off, never panic.
        let s = "é".repeat(200); // 400 bytes
        let (preview, truncated) = head(&s, 281);
        assert!(truncated);
        assert!(s.is_char_boundary(preview.len()));
        assert!(preview.len() <= 281);
    }

    #[test]
    fn node_title_is_the_first_non_blank_line() {
        assert_eq!(node_title("hello world"), "hello world");
        // Leading blank lines are skipped; the title is trimmed.
        assert_eq!(node_title("\n\n  # Heading  \nbody"), "# Heading");
        // An empty / whitespace-only node falls back to the placeholder.
        assert_eq!(node_title(""), UNTITLED_NODE);
        assert_eq!(node_title("   \n  "), UNTITLED_NODE);
    }

    /// The renderer shows a node's *current* text, not its kind-1606 creation
    /// snapshot: after a content edit folds into the shared cache, [`live_node`]
    /// resolves the edited body (and the owning canvas's title). This is the whole
    /// point of folding through the cache rather than reading the creation note.
    #[test]
    fn live_node_reflects_the_current_content_not_the_snapshot() {
        use crate::store::{self, CANVAS_ID, CanvasAction, NoPublish};
        use enostr::FullKeypair;
        use futures_util::StreamExt;
        use nostrdb::{Config, SubscriptionStream};

        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
        let kp = FullKeypair::generate();
        let secret = kp.secret_key.secret_bytes();
        let sub = ndb
            .subscribe(&[event::notebook_filter(&kp.pubkey)])
            .unwrap();
        let mut stream = SubscriptionStream::new(ndb.clone(), sub).notes_per_await(64);
        let mut await_notes = |n: usize| {
            pollster::block_on(async {
                let mut seen = 0;
                while seen < n {
                    seen += stream.next().await.expect("subscription open").len();
                }
            });
        };

        let cache = Rc::new(RefCell::new(NotebookCache::default()));
        let poll_canvas = |cache: &Rc<RefCell<NotebookCache>>| -> Option<crate::event::CanvasView> {
            let txn = Transaction::new(&ndb).unwrap();
            cache.borrow_mut().poll(&ndb, &txn, &kp.pubkey);
            cache.borrow_mut().canvas(&ndb, &txn, &kp.pubkey, CANVAS_ID)
        };

        store::seed_canvas(
            &ndb,
            &kp.pubkey,
            &secret,
            CANVAS_ID,
            "My canvas",
            &mut NoPublish,
        );
        await_notes(1);
        let view = loop {
            if let Some(v) = poll_canvas(&cache) {
                break v;
            }
        };

        // Add a node, fold it in, and capture its creation snapshot.
        store::apply(
            &ndb,
            CANVAS_ID,
            &view,
            &kp.pubkey,
            &secret,
            CanvasAction::AddNode {
                kind: event::NodeKind::Text,
                geo: event::Geometry {
                    x: 0,
                    y: 0,
                    w: 200,
                    h: 80,
                },
                content: event::NodeContent {
                    text: "original".to_string(),
                    ..Default::default()
                },
            },
            &mut NoPublish,
        );
        await_notes(2);
        let (node_id, view) = loop {
            if let Some(v) = poll_canvas(&cache)
                && let Some(node) = v.nodes.first()
            {
                break (node.id, v);
            }
        };

        // The creation snapshot the renderer parses from the kind-1606 note.
        let snapshot = NodeEvent {
            id: *node_id.bytes(),
            author: *kp.pubkey.bytes(),
            canvas_author: *kp.pubkey.bytes(),
            canvas_id: CANVAS_ID.to_string(),
            kind: event::NodeKind::Text,
            geo: event::Geometry::default(),
            content: event::NodeContent {
                text: "original".to_string(),
                ..Default::default()
            },
            created_at: 0,
        };

        // Before any edit, the live fold agrees with the snapshot and knows the
        // canvas title.
        let live = live_node(&cache, &ndb, &Transaction::new(&ndb).unwrap(), &snapshot)
            .expect("node folded");
        assert_eq!(live.text, "original");
        assert_eq!(live.canvas_title, "My canvas");

        // Edit the node's content; the snapshot is now stale but the live fold
        // tracks the new body.
        store::apply(
            &ndb,
            CANVAS_ID,
            &view,
            &kp.pubkey,
            &secret,
            CanvasAction::EditContent {
                node: node_id,
                content: event::NodeContent {
                    text: "edited".to_string(),
                    ..Default::default()
                },
            },
            &mut NoPublish,
        );
        await_notes(1);
        let live = loop {
            poll_canvas(&cache);
            let txn = Transaction::new(&ndb).unwrap();
            if let Some(l) = live_node(&cache, &ndb, &txn, &snapshot)
                && l.text == "edited"
            {
                break l;
            }
        };
        assert_eq!(live.text, "edited", "live fold beats the creation snapshot");

        // A node whose canvas isn't folded (a different account) resolves to None,
        // so the renderer falls back to the snapshot.
        let other = FullKeypair::generate();
        let foreign = NodeEvent {
            canvas_author: *other.pubkey.bytes(),
            ..snapshot.clone()
        };
        let txn = Transaction::new(&ndb).unwrap();
        assert!(live_node(&cache, &ndb, &txn, &foreign).is_none());
    }
}
