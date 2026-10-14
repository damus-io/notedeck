//! A single home for the read-only, app-contributed registries handed to every
//! frame through [`AppContext`](crate::AppContext).
//!
//! Apps populate these — [`kind_renderers`](AppRegistries::kind_renderers) at
//! startup, [`tools`](AppRegistries::tools) per-frame from the running app set —
//! and the browser looks entries up while drawing. Grouping them here keeps
//! `AppContext` from sprawling one field per registry as more registries are
//! added.
//!
//! The whole struct is exposed as a single shared `&AppRegistries`, so a caller
//! can copy that reference out before taking a disjoint `&mut self` reborrow
//! (e.g. [`note_context`](crate::AppContext::note_context) /
//! [`tool_context`](crate::AppContext::tool_context)) — the same borrow trick
//! each individual registry relied on before they shared a home.

use nostrdb::Note;

use crate::{
    kind_renderer::KindRendererRegistry,
    reference::{ReferenceParserRegistry, ReferenceResolveCtx},
    tool::ToolRegistry,
};

/// The read-only registries an [`AppContext`](crate::AppContext) exposes to a
/// frame, gathered into one shared handle. See the [module docs](self).
#[derive(Default)]
pub struct AppRegistries {
    /// Renderers for nostr events embedded inline, keyed by kind (e.g. a headway
    /// issue referenced from a notebook note). Populated at app startup.
    pub kind_renderers: KindRendererRegistry,
    /// Parsers that turn a `scheme:token` text reference into a resolvable nostr
    /// entity (the front half of an inline reference). Seeded with the built-in
    /// `nostr:` parser; apps add their own at startup. See [`reference`](crate::reference).
    pub reference_parsers: ReferenceParserRegistry,
    /// App-contributed agent tools for AI backends. Reset per-frame from the
    /// running apps.
    pub tools: ToolRegistry,
}

impl AppRegistries {
    /// The note `reference` names: exactly one reference (see
    /// [`ReferenceParserRegistry::resolve_exact`]), resolved through the
    /// registered parsers and read from `ctx`'s transaction. `None` when no
    /// parser recognizes it or what it names isn't in the local db yet.
    pub fn referenced_note<'t>(
        &self,
        reference: &str,
        ctx: &ReferenceResolveCtx<'t>,
    ) -> Option<Note<'t>> {
        let resolved = self.reference_parsers.resolve_exact(reference, ctx)?;
        ctx.ndb
            .get_note_by_id(ctx.txn, resolved.note_id.bytes())
            .ok()
    }

    /// The current title of what `reference` names (a card's title for its
    /// `headway:<board>/<word-id>`), from the [`referenced_note`]'s kind
    /// renderer (see [`KindRendererRegistry::title`]), with neither the caller
    /// nor this knowing the reference's scheme. `None` when it doesn't resolve
    /// or its kind offers no title.
    ///
    /// Resolving can fold events and the title is owned, so a caller that
    /// shows it often keeps it (as Dave's session list does) rather than
    /// asking again per frame.
    ///
    /// [`referenced_note`]: Self::referenced_note
    pub fn reference_title(&self, reference: &str, ctx: &ReferenceResolveCtx) -> Option<String> {
        let note = self.referenced_note(reference, ctx)?;
        self.kind_renderers.title(ctx.ndb, ctx.txn, &note, None)
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;
    use std::time::Duration;

    use nostrdb::{Filter, IngestMetadata, NoteBuilder, Transaction};
    use nostrdb_net::{ClientMessage, FullKeypair, NoteId};

    use super::*;
    use crate::reference::{ReferenceParser, ResolvedRef};
    use crate::test_util::TempNdb;
    use crate::{KindRenderRequest, KindRenderResponse, KindRenderer, NoteContext};

    /// Resolves the one reference `@card` to `note`.
    struct CardParser {
        note: NoteId,
    }

    impl ReferenceParser for CardParser {
        fn id(&self) -> &'static str {
            "card"
        }
        fn find(&self, text: &str) -> Option<Range<usize>> {
            let at = text.find("@card")?;
            Some(at..at + "@card".len())
        }
        fn resolve(&self, _matched: &str, _ctx: &ReferenceResolveCtx) -> Option<ResolvedRef> {
            Some(ResolvedRef::note(self.note))
        }
    }

    /// Titles a kind-1 note with its content.
    struct ContentTitle;

    impl KindRenderer for ContentTitle {
        fn id(&self) -> &'static str {
            "test.content"
        }
        fn name(&self) -> &'static str {
            "Content"
        }
        fn kinds(&self) -> &'static [u32] {
            &[1]
        }
        fn render(
            &self,
            ui: &mut egui::Ui,
            _note_context: &mut NoteContext,
            _req: &KindRenderRequest,
        ) -> KindRenderResponse {
            KindRenderResponse::new(ui.label("card"))
        }
        fn title(&self, _ndb: &nostrdb::Ndb, _txn: &Transaction, note: &Note) -> Option<String> {
            Some(note.content().to_owned())
        }
    }

    /// A reference's title comes from the kind renderer of the note its
    /// parser resolves it to; prose around it, or a reference to a note not
    /// in the db, has none.
    #[tokio::test]
    async fn a_reference_is_titled_by_the_renderer_of_the_note_it_names() {
        let TempNdb { ndb, dir: _dir } = TempNdb::new();
        let kp = FullKeypair::generate();
        let note = NoteBuilder::new()
            .kind(1)
            .content("Find sessions by their card's title")
            .created_at(1_700_000_000)
            .sign(&kp.secret_key.secret_bytes())
            .build()
            .expect("note");
        let note_id = NoteId::new(*note.id());

        let mut registries = AppRegistries::default();
        registries
            .reference_parsers
            .register(Box::new(CardParser { note: note_id }));
        registries.kind_renderers.register(Box::new(ContentTitle));

        let title = |ndb: &nostrdb::Ndb, reference: &str| {
            let txn = Transaction::new(ndb).expect("txn");
            let ctx = ReferenceResolveCtx {
                ndb,
                txn: &txn,
                selected_account: None,
            };
            registries.reference_title(reference, &ctx)
        };
        assert_eq!(title(&ndb, "@card"), None, "not in the db yet");

        let sub = ndb
            .subscribe(&[Filter::new().ids([note.id()]).build()])
            .expect("subscribe");
        let frame = ClientMessage::event(&note)
            .expect("event")
            .to_json()
            .expect("json");
        ndb.process_event_with(&frame, IngestMetadata::new().client(true))
            .expect("ingest");
        tokio::time::timeout(Duration::from_secs(5), ndb.wait_for_notes(sub, 1))
            .await
            .expect("ingested within 5s")
            .expect("ingest notified the subscription");

        assert_eq!(
            title(&ndb, " @card ").as_deref(),
            Some("Find sessions by their card's title")
        );
        assert_eq!(title(&ndb, "see @card"), None, "prose is not a reference");
    }
}
