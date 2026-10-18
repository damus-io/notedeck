//! Universal search in the Cmd-K bar through real chrome frames: the fake
//! app's hits are listed under its name and Enter opens one, the query is
//! searched once per change rather than per frame, a pending answer is asked
//! again until it settles, an app's name switches to it, and a typed
//! reference opens.

mod common;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use common::{bar_key, Fixture};
use egui::{Event, Key, Modifiers};
use nostrdb::{Filter, IngestMetadata, NoteBuilder};
use notedeck::{App, AppContext, AppResponse, OpenUri, SearchHit};
use notedeck_chrome::NotedeckApp;

/// No bindings: these tests only type into the bar.
const KEYMAP: &str = "";

/// Press the bar's key, Cmd-K on macOS and Ctrl-Space elsewhere, the way
/// this platform's keyboard reports it.
fn cmd_k(f: &mut Fixture) {
    let open = bar_key(&f.egui_ctx);
    f.press(open.modifiers, open.logical_key, None);
}

/// Type `text` into whatever has focus, one frame.
fn type_text(f: &mut Fixture, text: &str) {
    f.frame(vec![Event::Text(text.to_owned())]);
}

/// The titles the bar lists, in order.
fn listed(f: &mut Fixture) -> Vec<String> {
    f.chrome
        .command_bar_mut()
        .matches()
        .map(|item| item.title.clone())
        .collect()
}

/// Whether the last frame painted `text` within some run of text: a row's
/// title and subtitle are one.
fn drew_in(f: &Fixture, text: &str) -> bool {
    f.drawn().iter().any(|drawn| drawn.contains(text))
}

/// A hit of the fake app's titled `title`, opening `fake:<uri>`.
fn hit(title: &str, uri: &str) -> SearchHit {
    SearchHit::new(title, OpenUri::new(format!("fake:{uri}")), 0)
}

#[test]
fn an_apps_hits_list_under_its_name_and_enter_opens_one() {
    let mut f = Fixture::new(KEYMAP);
    f.state.borrow_mut().results = vec![hit("Fix the parser", "fix-parser").kind("Card")];
    cmd_k(&mut f);
    type_text(&mut f, "parser");
    f.frame(Vec::new());

    assert_eq!(listed(&mut f), ["Fix the parser"]);
    assert!(f.drew("fake"), "under the app's name");
    assert!(drew_in(&f, "Fix the parser"), "the hit is drawn");
    assert!(drew_in(&f, "Card"), "with its kind");

    f.key(Key::Enter);
    assert!(
        !f.chrome.command_bar().is_open(),
        "opening it closes the bar"
    );
    assert_eq!(
        f.chrome.pending_open(),
        Some(&OpenUri::new("fake:fix-parser")),
        "Enter raised AppAction::Open on the hit's uri"
    );
}

#[test]
fn the_query_is_searched_once_per_change_not_per_frame() {
    let mut f = Fixture::new(KEYMAP);
    cmd_k(&mut f);
    f.idle(0.2);
    assert!(f.searched().is_empty(), "an empty query searches nothing");

    type_text(&mut f, "pa");
    f.idle(0.5);
    assert_eq!(f.searched(), ["pa"]);

    type_text(&mut f, "r ");
    f.idle(0.5);
    assert_eq!(f.searched(), ["pa", "par"], "searched trimmed");
}

#[test]
fn a_hit_is_kept_whatever_its_title_matches() {
    let mut f = Fixture::new(KEYMAP);
    f.state.borrow_mut().results = vec![hit("Unrelated title", "body-match")];
    cmd_k(&mut f);
    type_text(&mut f, "zzq");
    f.frame(Vec::new());
    assert_eq!(listed(&mut f), ["Unrelated title"], "its provider found it");
}

#[test]
fn a_pending_answer_is_asked_again_until_it_settles() {
    let mut f = Fixture::new(KEYMAP);
    f.state.borrow_mut().search_pending = true;
    cmd_k(&mut f);
    type_text(&mut f, "slow");
    assert_eq!(f.searched().len(), 1);

    f.idle(0.35);
    let asked = f.searched().len();
    assert!(asked >= 3, "asked again while pending: {asked}");
    assert!(asked <= 5, "but not every frame: {asked}");

    {
        let mut state = f.state.borrow_mut();
        state.search_pending = false;
        state.results = vec![hit("Slow answer", "slow")];
    }
    f.idle(0.2);
    assert_eq!(listed(&mut f), ["Slow answer"], "the refined answer");
    let settled = f.searched().len();
    f.idle(0.5);
    assert_eq!(f.searched().len(), settled, "a settled answer isn't asked");
}

#[test]
fn no_search_runs_while_the_bar_asks_for_an_argument() {
    let mut f = Fixture::new(KEYMAP);
    cmd_k(&mut f);
    type_text(&mut f, "name it");
    f.frame(Vec::new());
    f.key(Key::Enter);
    assert_eq!(f.chrome.command_bar().prompt(), Some("Name it"));
    let before = f.searched();

    type_text(&mut f, "Bob");
    f.idle(0.2);
    assert_eq!(f.searched(), before, "the query is a value, not a search");
}

/// A second app to switch away from and back.
struct Spare;

impl App for Spare {
    fn render(&mut self, _ctx: &mut AppContext<'_>, _ui: &mut egui::Ui) -> AppResponse {
        AppResponse::none()
    }
}

#[test]
fn an_apps_name_switches_to_it() {
    let mut f = Fixture::behind(
        KEYMAP,
        [NotedeckApp::Other("Spare".to_owned(), Box::new(Spare))],
    );
    assert!(f.state.borrow().area.is_none(), "the fake app is behind");
    cmd_k(&mut f);
    type_text(&mut f, "fak");
    f.frame(Vec::new());
    assert_eq!(listed(&mut f).first().map(String::as_str), Some("fake"));
    assert!(f.drew("Apps"));
    assert!(drew_in(&f, "Switch to app"));

    f.key(Key::Enter);
    assert!(!f.chrome.command_bar().is_open());
    f.frame(Vec::new());
    assert!(f.state.borrow().area.is_some(), "the fake app is in front");
}

#[tokio::test]
async fn a_typed_reference_is_one_open_row() {
    let mut f = Fixture::new(KEYMAP);
    let note = NoteBuilder::new()
        .kind(1)
        .content("a note to open from the bar")
        .created_at(1_000)
        .sign(&[7u8; 32])
        .build()
        .expect("note");
    let id = nostrdb_net::NoteId::new(*note.id());
    let frame = nostrdb_net::ClientMessage::event(&note)
        .expect("event")
        .to_json()
        .expect("json");
    {
        let ndb = f.notedeck.app_context().ndb.clone();
        let sub = ndb
            .subscribe(&[Filter::new().ids([id.bytes()]).build()])
            .expect("sub");
        ndb.process_event_with(&frame, IngestMetadata::new().client(true))
            .expect("ingest");
        ndb.wait_for_all_notes_within(sub, 1, Duration::from_secs(5))
            .await
            .expect("committed");
    }
    let reference = format!("nostr:{}", id.to_bech().expect("bech"));

    cmd_k(&mut f);
    type_text(&mut f, &reference);
    f.frame(Vec::new());
    let first = f
        .chrome
        .command_bar_mut()
        .matches()
        .next()
        .cloned()
        .expect("a row");
    assert_eq!(first.title, reference);
    assert_eq!(first.subtitle.as_deref(), Some("Open"));
    assert_eq!(
        first.action,
        notedeck::CommandAction::Open(OpenUri::new(reference.clone()))
    );

    type_text(&mut f, "x");
    f.frame(Vec::new());
    assert!(
        !listed(&mut f)
            .iter()
            .any(|title| title.starts_with("nostr:")),
        "a reference that doesn't resolve isn't offered"
    );
}

#[test]
fn asking_a_pending_provider_again_keeps_the_selection() {
    let mut f = Fixture::new(KEYMAP);
    {
        let mut state = f.state.borrow_mut();
        state.search_pending = true;
        state.results = vec![hit("First hit", "one"), hit("Second hit", "two")];
    }
    cmd_k(&mut f);
    type_text(&mut f, "hit");
    f.frame(Vec::new());
    f.key(Key::ArrowDown);
    let selected = |f: &mut Fixture| {
        f.chrome
            .command_bar_mut()
            .selected()
            .map(|item| item.title.clone())
    };
    assert_eq!(selected(&mut f).as_deref(), Some("Second hit"));

    f.idle(0.35);
    assert!(f.searched().len() >= 3, "it was asked again");
    assert_eq!(
        selected(&mut f).as_deref(),
        Some("Second hit"),
        "the same answer leaves the selection where the key moved it"
    );
}

/// How a [`StubRenderer`] was last asked to draw, and where it drew.
#[derive(Clone, Copy)]
struct StubDrawn {
    context: notedeck::RenderContext,
    chrome: notedeck::InlineChrome,
    interaction: notedeck::Interaction,
    rect: egui::Rect,
}

/// Draws a kind-1 note as a button, a widget that takes clicks whatever it
/// is asked, and records how it was asked.
struct StubRenderer {
    drawn: Rc<RefCell<Option<StubDrawn>>>,
}

impl notedeck::KindRenderer for StubRenderer {
    fn id(&self) -> &'static str {
        "test.stub"
    }

    fn name(&self) -> &'static str {
        "Stub"
    }

    fn kinds(&self) -> &'static [u32] {
        &[1]
    }

    fn render(
        &self,
        ui: &mut egui::Ui,
        _note_context: &mut notedeck::NoteContext,
        req: &notedeck::KindRenderRequest,
    ) -> notedeck::KindRenderResponse {
        let response = ui.button("stub row");
        *self.drawn.borrow_mut() = Some(StubDrawn {
            context: req.context,
            chrome: req.chrome,
            interaction: req.interaction,
            rect: response.rect,
        });
        notedeck::KindRenderResponse::new(response)
    }
}

/// An app whose only hit is `hit`, and which draws kind-1 notes with a
/// [`StubRenderer`] sharing `drawn`.
struct Renders {
    hit: SearchHit,
    drawn: Rc<RefCell<Option<StubDrawn>>>,
}

impl App for Renders {
    fn render(&mut self, _ctx: &mut AppContext<'_>, _ui: &mut egui::Ui) -> AppResponse {
        AppResponse::none()
    }

    fn search(
        &mut self,
        _ctx: &mut AppContext<'_>,
        _query: &notedeck::SearchQuery<'_>,
        out: &mut notedeck::SearchHits,
    ) {
        out.push(self.hit.clone());
    }

    fn kind_renderers(&self) -> Vec<Box<dyn notedeck::KindRenderer>> {
        vec![Box::new(StubRenderer {
            drawn: Rc::clone(&self.drawn),
        })]
    }
}

#[tokio::test]
async fn a_hit_standing_for_a_note_is_drawn_by_its_renderer_and_its_click_opens_the_hit() {
    let note = NoteBuilder::new()
        .kind(1)
        .content("a note drawn in the bar")
        .created_at(1_000)
        .sign(&[9u8; 32])
        .build()
        .expect("note");
    let id = nostrdb_net::NoteId::new(*note.id());
    let drawn = Rc::new(RefCell::new(None));
    let renders = Renders {
        hit: hit("A rendered note", "rendered")
            .kind("Note")
            .previewed(notedeck::HitPreview::Note(id)),
        drawn: Rc::clone(&drawn),
    };
    let mut f = Fixture::behind(
        KEYMAP,
        [NotedeckApp::Other("Renders".to_owned(), Box::new(renders))],
    );
    {
        let ndb = f.notedeck.app_context().ndb.clone();
        let sub = ndb
            .subscribe(&[Filter::new().ids([id.bytes()]).build()])
            .expect("sub");
        let frame = nostrdb_net::ClientMessage::event(&note)
            .expect("event")
            .to_json()
            .expect("json");
        ndb.process_event_with(&frame, IngestMetadata::new().client(true))
            .expect("ingest");
        ndb.wait_for_all_notes_within(sub, 1, Duration::from_secs(5))
            .await
            .expect("committed");
    }

    cmd_k(&mut f);
    type_text(&mut f, "rendered");
    f.frame(Vec::new());
    let seen = drawn.borrow().expect("the renderer drew the hit");
    assert_eq!(seen.context, notedeck::RenderContext::Row);
    assert_eq!(seen.chrome, notedeck::InlineChrome::Bare);
    assert_eq!(seen.interaction, notedeck::Interaction::Inert);
    assert!(f.drew("stub row"));
    assert!(
        !drew_in(&f, "A rendered note"),
        "the renderer draws the note, not the hit's title"
    );
    assert!(drew_in(&f, "Note"), "its kind is drawn beside it");

    // Clicked on the button the renderer drew, the row still takes it.
    let at = seen.rect.center();
    f.frame(vec![Event::PointerMoved(at)]);
    let click = |pressed| Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    f.frame(vec![click(true)]);
    f.frame(vec![click(false)]);
    assert!(!f.chrome.command_bar().is_open(), "the click chose the row");
    assert_eq!(
        f.chrome.pending_open(),
        Some(&OpenUri::new("fake:rendered")),
        "and opened the hit"
    );
}

#[test]
fn a_hit_standing_for_a_note_the_db_lacks_is_drawn_as_text() {
    let mut f = Fixture::new(KEYMAP);
    f.state.borrow_mut().results =
        vec![hit("A missing note", "missing")
            .kind("Note")
            .previewed(notedeck::HitPreview::Note(nostrdb_net::NoteId::new(
                [3; 32],
            )))];
    cmd_k(&mut f);
    type_text(&mut f, "missing");
    f.frame(Vec::new());
    assert!(drew_in(&f, "A missing note"), "its title, as any row's");
}
