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

use nostrdb::{Ndb, Note, Transaction};

/// Renders a nostr entity of one or more kinds inline.
///
/// The note is already resolved from the db by the caller; addressable
/// references (`naddr`) arrive here as the canonical replaceable event for their
/// coordinate, so a renderer can recover author/`d`-tag from the note itself
/// (and fold further events off it if it needs to, e.g. a board).
pub trait KindRenderer {
    /// Stable identifier used to persist the per-kind default choice in settings.
    /// Must be unique across registered renderers (e.g. `"headway.issue"`).
    fn id(&self) -> &'static str;

    /// Human-readable label shown in the renderer picker.
    fn name(&self) -> &'static str;

    /// The event kinds this renderer can draw.
    fn kinds(&self) -> &'static [u32];

    /// Draw the resolved note, returning the response covering what was drawn.
    fn render(
        &self,
        ui: &mut egui::Ui,
        ndb: &Ndb,
        txn: &Transaction,
        note: &Note,
    ) -> egui::Response;
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
            _ndb: &Ndb,
            _txn: &Transaction,
            _note: &Note,
        ) -> egui::Response {
            ui.label("stub")
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
