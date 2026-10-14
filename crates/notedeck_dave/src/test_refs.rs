//! Stand-ins for headway's card reference parser and renderer, which Dave
//! doesn't load, for tests that need a session's `headway:` ref to resolve to
//! a card with a title.

/// Stands in for headway's `headway:` parser, which Dave doesn't load:
/// the one card ref it knows resolves to its seeded kind-1621 note.
pub(crate) struct StubCardParser {
    pub card: std::rc::Rc<std::cell::RefCell<Option<CardNote>>>,
}

/// The card [`StubCardParser`] resolves: its ref and its note.
pub(crate) struct CardNote {
    pub card_ref: String,
    pub note: nostrdb_net::NoteId,
}

impl notedeck::ReferenceParser for StubCardParser {
    fn id(&self) -> &'static str {
        "headway"
    }

    fn find(&self, text: &str) -> Option<std::ops::Range<usize>> {
        headway::wordid::find_ref(text)
    }

    fn resolve(
        &self,
        matched: &str,
        _ctx: &notedeck::ReferenceResolveCtx,
    ) -> Option<notedeck::ResolvedRef> {
        let card = self.card.borrow();
        let card = card.as_ref().filter(|c| c.card_ref == matched)?;
        Some(notedeck::ResolvedRef::note(card.note))
    }
}

/// Stands in for headway's card renderer: the note's content is the
/// card's title, which its chip shows beside a status glyph.
pub(crate) struct StubCardRenderer;

impl notedeck::KindRenderer for StubCardRenderer {
    fn id(&self) -> &'static str {
        "test.card"
    }

    fn name(&self) -> &'static str {
        "Test card"
    }

    fn kinds(&self) -> &'static [u32] {
        &[1621]
    }

    fn render(
        &self,
        ui: &mut egui::Ui,
        _note_context: &mut notedeck::NoteContext,
        req: &notedeck::KindRenderRequest,
    ) -> notedeck::KindRenderResponse {
        let theme = notedeck::ColorTheme::current(ui.ctx());
        let response =
            notedeck_ui::inline_chip(ui, &theme, req.note.content(), req.chrome, |ui, size| {
                ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
            });
        notedeck::open_on_click(ui, response, req)
    }

    fn title(
        &self,
        _ndb: &nostrdb::Ndb,
        _txn: &nostrdb::Transaction,
        note: &nostrdb::Note,
    ) -> Option<String> {
        Some(note.content().to_owned())
    }
}

/// Sign a kind-1621 card note titled `title` with `secret` and ingest it into
/// `ndb` as a client write, returning what [`StubCardParser`] needs to
/// resolve `card_ref` to it. Ingest is async: the caller waits for it.
pub(crate) fn ingest_card(
    ndb: &nostrdb::Ndb,
    secret: &[u8; 32],
    card_ref: &str,
    title: &str,
) -> CardNote {
    let note = nostrdb::NoteBuilder::new()
        .kind(1621)
        .content(title)
        .created_at(1_700_000_000)
        .sign(secret)
        .build()
        .expect("note");
    let frame = nostrdb_net::ClientMessage::event(&note)
        .expect("event")
        .to_json()
        .expect("json");
    ndb.process_event_with(&frame, nostrdb::IngestMetadata::new().client(true))
        .expect("ingest");
    CardNote {
        card_ref: card_ref.to_owned(),
        note: nostrdb_net::NoteId::new(*note.id()),
    }
}
