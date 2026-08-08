//! A registry of pluggable renderers for nostr events, keyed by event kind.
//!
//! A `nostr:` reference embedded in some text (a notebook note, a chat message,
//! …) resolves to a single nostr event — an `nevent`/`note` points straight at
//! one, and an `naddr` resolves to the canonical replaceable event for its
//! coordinate. The surface that wants to display it doesn't know how to render
//! every kind, so it looks one up here: each [`KindRenderer`] declares the
//! [`kinds`](KindRenderer::kinds) it handles and draws the resolved note inline.
//!
//! There can be **several** renderers for the same kind (e.g. two different
//! takes on a kind-1 note). Each renderer carries a stable [`id`](KindRenderer::id)
//! so a per-kind default can be remembered in settings; without a stored choice
//! the first-registered renderer for the kind wins. Renderer impls live in the
//! crates that own the data (e.g. headway renderers in `notedeck_headway`) and
//! are registered at app startup, keeping this crate free of those dependencies.

use std::collections::HashMap;

use enostr::NoteId;
use nostrdb::{Filter, Ndb, Note, Transaction};

use crate::NoteContext;

/// Resolve a bech32 `nostr:` entity to a concrete note: `nevent`/`note` point
/// straight at one, `naddr` resolves to the latest replaceable event for its
/// coordinate (see this module's docs). Returns `None` for an unparseable ref,
/// an unhandled prefix, or an entity not yet in `ndb`.
///
/// The shared resolver behind every inline `nostr:` reference — text surfaces
/// (the notebook, chat, …) call this and then hand the note to the
/// [`KindRenderer`] for its kind.
pub fn resolve_ref<'a>(ndb: &Ndb, txn: &'a Transaction, bech: &str) -> Option<Note<'a>> {
    if bech.starts_with("nevent1") {
        let id = NoteId::from_nevent_bech(bech)?;
        ndb.get_note_by_id(txn, id.bytes()).ok()
    } else if bech.starts_with("note1") {
        let id = NoteId::from_bech(bech)?;
        ndb.get_note_by_id(txn, id.bytes()).ok()
    } else if bech.starts_with("naddr1") {
        use nostr::nips::nip19::FromBech32;
        let coord = nostr::nips::nip01::Coordinate::from_bech32(bech).ok()?;
        let author = coord.public_key.to_bytes();
        let filter = Filter::new()
            .authors([&author])
            .kinds([coord.kind.as_u16() as u64])
            .tags([coord.identifier.as_str()], 'd')
            .limit(1)
            .build();
        ndb.query(txn, &[filter], 1)
            .ok()?
            .into_iter()
            .next()
            .map(|r| r.note)
    } else {
        None
    }
}

/// What a [`KindRenderer`] produced this frame: the drawn [`egui::Response`] and
/// an optional [`AppAction`](crate::AppAction) for the shell to route.
///
/// A renderer draws a *clickable* inline widget, so beyond the response it can
/// signal intent — most commonly "open this entity in the app that owns it" when
/// clicked (see [`with_action`](Self::with_action)). The caller pushes any action
/// onto [`AppContext::app_actions`](crate::AppContext::app_actions).
pub struct KindRenderResponse {
    /// The egui response covering what the renderer drew.
    pub response: egui::Response,
    /// An action raised this frame (e.g. the widget was clicked), or `None`.
    pub action: Option<crate::AppAction>,
}

impl KindRenderResponse {
    /// A response that only drew, raising no action.
    pub fn new(response: egui::Response) -> Self {
        Self {
            response,
            action: None,
        }
    }

    /// A response carrying an action to route (e.g. raised on click).
    pub fn with_action(response: egui::Response, action: Option<crate::AppAction>) -> Self {
        Self { response, action }
    }
}

/// Wrap an inline widget's [`egui::Response`] so clicking it opens the drawn entity
/// in the app that owns it — the shared click-to-open behaviour every inline
/// [`KindRenderer`] wants, so no impl re-derives it.
///
/// Senses clicks over the drawn area, shows a pointer cursor on hover, and on a
/// click emits [`AppAction::Note`](crate::AppAction::Note) carrying `note`'s id. The
/// shell routes that action to the owning app (it sniffs the note's kind to pick
/// the destination), just as it does for a clicked note anywhere else.
pub fn open_on_click(ui: &egui::Ui, response: egui::Response, note: &Note) -> KindRenderResponse {
    let response = response.interact(egui::Sense::click());
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let action = response
        .clicked()
        .then(|| crate::AppAction::Note(crate::NoteAction::note(NoteId::new(*note.id()))));
    KindRenderResponse::with_action(response, action)
}

/// Where an inline reference is being drawn, so a [`KindRenderer`] can pick a
/// shape for the *same* entity — a compact chip in flowing prose versus a full
/// card as a block embed.
///
/// Advisory: a renderer that doesn't specialize a given context just draws its
/// default shape. `#[non_exhaustive]` so new contexts (a dense list row, a
/// hover tooltip, …) can be added without breaking existing impls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum RenderContext {
    /// Inline within a run of flowing text: draw a compact, single-line
    /// representation that doesn't break the paragraph. The **default**, so a
    /// reference is unobtrusive unless a surface asks for more.
    #[default]
    Inline,
    /// A block embed that owns its own line/box: draw the richest useful preview
    /// (e.g. a full card).
    Embed,
    /// The surface is dedicated entirely to this one entity: draw its *complete*
    /// content (a full document render), not a preview. Distinct from
    /// [`Embed`](Self::Embed), which previews a reference sitting inside another
    /// document's flow — here the reference *is* the whole surface (a canvas
    /// note-embed node, say), so there is nothing to truncate for.
    Full,
}

/// The inputs for one [`KindRenderer::render`] call: the resolved note, a
/// transaction to read it (and any events folded off it) within, and the
/// [`RenderContext`] hinting which shape to draw.
///
/// Bundled into a struct so the render signature stays stable as inputs grow
/// (rather than churning every impl each time one is added).
pub struct KindRenderRequest<'a, 'txn> {
    /// A live read transaction the note was resolved from.
    pub txn: &'a Transaction,
    /// The resolved note to draw. Addressable references (`naddr`) arrive as the
    /// canonical replaceable event for their coordinate, so a renderer can
    /// recover author/`d`-tag from the note itself (and fold further events off
    /// it if it needs to, e.g. a board).
    pub note: &'a Note<'txn>,
    /// The surrounding context, hinting which shape to draw. Advisory.
    pub context: RenderContext,
}

/// Renders a nostr entity of one or more kinds inline.
pub trait KindRenderer {
    /// Stable identifier used to persist the per-kind default choice in settings.
    /// Must be unique across registered renderers (e.g. `"headway.issue"`).
    fn id(&self) -> &'static str;

    /// Human-readable label shown in the renderer picker.
    fn name(&self) -> &'static str;

    /// The event kinds this renderer can draw.
    fn kinds(&self) -> &'static [u32];

    /// Draw the resolved note described by `req`, returning the
    /// [`KindRenderResponse`] covering what was drawn plus any action raised
    /// (e.g. "open me in my host app" on click).
    ///
    /// The [`NoteContext`] carries the dependencies a full renderer needs (ndb,
    /// caches, accounts, i18n, …) so a renderer can reuse rich widgets like
    /// notedeck_ui's `NoteView`; `note_context.ndb` is the db the note was
    /// resolved from. Honor [`req.context`](KindRenderRequest::context) to pick a
    /// shape, or ignore it to always draw the same one.
    fn render(
        &self,
        ui: &mut egui::Ui,
        note_context: &mut NoteContext,
        req: &KindRenderRequest,
    ) -> KindRenderResponse;
}

/// A set of [`KindRenderer`]s indexed by the kinds they handle.
#[derive(Default)]
pub struct KindRendererRegistry {
    /// Registration order is the fallback order within a kind.
    renderers: Vec<Box<dyn KindRenderer>>,
    /// kind -> indices into `renderers`, in registration order. Many per kind.
    by_kind: HashMap<u32, Vec<usize>>,
}

impl KindRendererRegistry {
    /// Register a renderer, indexing it under every kind it declares.
    pub fn register(&mut self, renderer: Box<dyn KindRenderer>) {
        let idx = self.renderers.len();
        for &kind in renderer.kinds() {
            self.by_kind.entry(kind).or_default().push(idx);
        }
        self.renderers.push(renderer);
    }

    /// All renderers registered for `kind`, in registration order.
    pub fn renderers_for(&self, kind: u32) -> impl Iterator<Item = &dyn KindRenderer> {
        self.by_kind
            .get(&kind)
            .into_iter()
            .flatten()
            .map(|&i| self.renderers[i].as_ref())
    }

    /// Look up a renderer by its stable [`id`](KindRenderer::id).
    pub fn by_id(&self, id: &str) -> Option<&dyn KindRenderer> {
        self.renderers
            .iter()
            .map(|r| r.as_ref())
            .find(|r| r.id() == id)
    }

    /// Pick the renderer to use for `kind`: the `chosen_id` default if it is set
    /// and registered for this kind, otherwise the first one registered for the
    /// kind. `None` if nothing handles the kind.
    pub fn default_for(&self, kind: u32, chosen_id: Option<&str>) -> Option<&dyn KindRenderer> {
        if let Some(id) = chosen_id {
            if let Some(r) = self.renderers_for(kind).find(|r| r.id() == id) {
                return Some(r);
            }
        }
        self.renderers_for(kind).next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stub {
        id: &'static str,
        kinds: &'static [u32],
    }

    impl KindRenderer for Stub {
        fn id(&self) -> &'static str {
            self.id
        }
        fn name(&self) -> &'static str {
            self.id
        }
        fn kinds(&self) -> &'static [u32] {
            self.kinds
        }
        fn render(
            &self,
            ui: &mut egui::Ui,
            _note_context: &mut NoteContext,
            _req: &KindRenderRequest,
        ) -> KindRenderResponse {
            KindRenderResponse::new(ui.label("stub"))
        }
    }

    #[test]
    fn default_and_fallback() {
        let mut reg = KindRendererRegistry::default();
        reg.register(Box::new(Stub {
            id: "a",
            kinds: &[1, 1621],
        }));
        reg.register(Box::new(Stub {
            id: "b",
            kinds: &[1],
        }));

        // Two renderers registered for kind 1, one for 1621.
        assert_eq!(reg.renderers_for(1).count(), 2);
        assert_eq!(reg.renderers_for(1621).count(), 1);
        assert_eq!(reg.renderers_for(9999).count(), 0);

        // No choice -> first registered for the kind.
        assert_eq!(reg.default_for(1, None).unwrap().id(), "a");
        // Valid choice honored.
        assert_eq!(reg.default_for(1, Some("b")).unwrap().id(), "b");
        // Choice not registered for this kind -> fall back to first.
        assert_eq!(reg.default_for(1, Some("nope")).unwrap().id(), "a");
        // Unknown kind -> nothing.
        assert!(reg.default_for(9999, None).is_none());
    }
}
