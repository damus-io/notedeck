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

use nostrdb::{Note, NoteKey};

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

    /// Look up what `reference` names: resolve it, and `read` the note it
    /// resolves to (its title, say), versioned so the caller can keep the
    /// result and check it with [`reference_revision`](Self::reference_revision)
    /// instead of reading again.
    ///
    /// Each revision is taken before what it versions is read, so a fold that
    /// moves in between leaves the kept revision older than the read, and the
    /// next check looks again. Taken after, a note folding in between would
    /// leave the old answer under the new revision, and it would never be
    /// looked up again.
    pub fn lookup_reference<'t, T>(
        &self,
        reference: &str,
        ctx: &ReferenceResolveCtx<'t>,
        read: impl FnOnce(&Note<'t>) -> T,
    ) -> ReferenceLookup<T> {
        let resolve = self.reference_parsers.revision_exact(reference, ctx);
        let Some(note) = self.referenced_note(reference, ctx) else {
            return ReferenceLookup {
                value: None,
                note: None,
                revision: resolve.map(|resolve| ReferenceRevision {
                    resolve,
                    read: None,
                }),
            };
        };
        let read_revision = self.kind_renderers.revision(ctx.ndb, ctx.txn, &note, None);
        let revision = resolve
            .zip(read_revision)
            .map(|(resolve, read)| ReferenceRevision {
                resolve,
                read: Some(read),
            });
        ReferenceLookup {
            value: Some(read(&note)),
            note: note.key(),
            revision,
        }
    }

    /// Which revision of its data a lookup of `reference` reads: resolving it
    /// ([`ReferenceParser::revision`](crate::ReferenceParser::revision)) and,
    /// when it resolved to the note at `resolved`, that note's title and
    /// outline ([`KindRenderer::revision`](crate::KindRenderer::revision)).
    ///
    /// A caller that keeps a lookup's result (Dave keeps card titles) keeps
    /// this beside it, with the key of the note it resolved to, and looks
    /// again only once it differs. `None` when either half can't tell, so the
    /// caller falls back to looking again on its own schedule.
    pub fn reference_revision(
        &self,
        reference: &str,
        ctx: &ReferenceResolveCtx,
        resolved: Option<NoteKey>,
    ) -> Option<ReferenceRevision> {
        let resolve = self.reference_parsers.revision_exact(reference, ctx)?;
        let read = match resolved.map(|key| ctx.ndb.get_note_by_key(ctx.txn, key)) {
            None => None,
            Some(Ok(note)) => Some(
                self.kind_renderers
                    .revision(ctx.ndb, ctx.txn, &note, None)?,
            ),
            // The note it resolved to is gone, which no kept revision matches.
            Some(Err(_)) => None,
        };
        Some(ReferenceRevision { resolve, read })
    }
}

/// What [`AppRegistries::lookup_reference`] found.
pub struct ReferenceLookup<T> {
    /// What its `read` made of the note the reference resolved to; `None`
    /// when it resolved to nothing.
    pub value: Option<T>,
    /// The key of that note, for a later
    /// [`reference_revision`](AppRegistries::reference_revision) to check.
    pub note: Option<NoteKey>,
    /// The revision of what the lookup read; `None` when the parser or the
    /// renderer can't tell.
    pub revision: Option<ReferenceRevision>,
}

/// The revisions a lookup of a reference read (see
/// [`AppRegistries::reference_revision`]). Two are equal when nothing the
/// lookup read has changed between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferenceRevision {
    /// What resolving the reference read.
    pub resolve: u64,
    /// What the title and outline of the note it resolved to read; `None`
    /// when it resolved to nothing.
    pub read: Option<u64>,
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

    /// Resolves `@card` to `note` once it is set, reporting `revision` as
    /// what resolving read: a stand-in for a parser over a fold cache.
    struct VersionedParser {
        note: std::rc::Rc<std::cell::Cell<Option<NoteId>>>,
        revision: std::rc::Rc<std::cell::Cell<u64>>,
    }

    impl ReferenceParser for VersionedParser {
        fn id(&self) -> &'static str {
            "card"
        }
        fn find(&self, text: &str) -> Option<Range<usize>> {
            let at = text.find("@card")?;
            Some(at..at + "@card".len())
        }
        fn resolve(&self, _matched: &str, _ctx: &ReferenceResolveCtx) -> Option<ResolvedRef> {
            self.note.get().map(ResolvedRef::note)
        }
        fn revision(&self, _matched: &str, _ctx: &ReferenceResolveCtx) -> Option<u64> {
            Some(self.revision.get())
        }
    }

    /// [`ContentTitle`], reporting `revision` as what its title read.
    struct VersionedTitle {
        revision: std::rc::Rc<std::cell::Cell<u64>>,
    }

    impl KindRenderer for VersionedTitle {
        fn id(&self) -> &'static str {
            "test.versioned"
        }
        fn name(&self) -> &'static str {
            "Versioned"
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
        fn revision(&self, _ndb: &nostrdb::Ndb, _txn: &Transaction, _note: &Note) -> Option<u64> {
            Some(self.revision.get())
        }
    }

    /// A versioned lookup keeps the revision of both halves it read, and a
    /// later [`AppRegistries::reference_revision`] matches it until either
    /// half moves. Without a renderer revision the lookup is unversioned.
    #[tokio::test]
    async fn a_lookup_is_current_until_what_it_read_moves() {
        use std::cell::Cell;
        use std::rc::Rc;

        let TempNdb { ndb, dir: _dir } = TempNdb::new();
        let kp = FullKeypair::generate();
        let note = NoteBuilder::new()
            .kind(1)
            .content("Card title")
            .created_at(1_700_000_000)
            .sign(&kp.secret_key.secret_bytes())
            .build()
            .expect("note");
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

        let resolved = Rc::new(Cell::new(None));
        let resolve_rev = Rc::new(Cell::new(1));
        let read_rev = Rc::new(Cell::new(10));
        let mut registries = AppRegistries::default();
        registries
            .reference_parsers
            .register(Box::new(VersionedParser {
                note: resolved.clone(),
                revision: resolve_rev.clone(),
            }));
        registries.kind_renderers.register(Box::new(VersionedTitle {
            revision: read_rev.clone(),
        }));

        let txn = Transaction::new(&ndb).expect("txn");
        let ctx = ReferenceResolveCtx {
            ndb: &ndb,
            txn: &txn,
            selected_account: None,
        };
        let title = |note: &Note| note.content().to_owned();

        // A miss is versioned by the parser alone.
        let miss = registries.lookup_reference("@card", &ctx, title);
        assert!(miss.value.is_none());
        let kept = miss.revision.expect("the parser versions it");
        assert_eq!(kept.read, None);
        assert_eq!(
            registries.reference_revision("@card", &ctx, None),
            Some(kept)
        );

        // The card arrives: the parser's fold moves, so the miss is stale.
        resolved.set(Some(NoteId::new(*note.id())));
        resolve_rev.set(2);
        assert_ne!(
            registries.reference_revision("@card", &ctx, None),
            Some(kept)
        );

        let hit = registries.lookup_reference("@card", &ctx, title);
        assert_eq!(hit.value.as_deref(), Some("Card title"));
        let kept = hit.revision.expect("both halves version it");
        assert_eq!(kept.read, Some(10));
        let current = || registries.reference_revision("@card", &ctx, hit.note);
        assert_eq!(current(), Some(kept), "nothing moved");

        // The renderer's fold moves (the card was renamed): stale.
        read_rev.set(11);
        assert_ne!(current(), Some(kept));
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
