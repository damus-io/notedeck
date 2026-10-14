//! Dave's inline [`KindRenderer`] for an agent session (kind 31988), contributed
//! to notedeck's registry so an `agentium:<word-id>` (or `nostr:`) reference to a
//! session draws inline wherever it's mentioned — a note, a comment, a Dave chat
//! message.
//!
//! The kind-31988 note the parser resolved to is only a *snapshot* of the session
//! at one revision; the session's live title and status come from folding its
//! current state through the app's shared
//! [`AgentiumSessionCache`](crate::session_cache::AgentiumSessionCache) — the same
//! realtime-pumped fold the foreground and the reference parser read, so a status
//! update shows on the chip in realtime — falling back to the note snapshot when
//! the session isn't folded locally (a not-yet-synced session, or one Dave never
//! opened). A click opens the session in Dave (via the shared
//! [`notedeck::open_on_click`]).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use nostrdb::{Ndb, Note, Transaction};
use nostrdb_net::Pubkey;
use notedeck::{
    ColorTheme, InlineChrome, KindRenderRequest, KindRenderResponse, KindRenderer, NoteContext,
    RenderContext,
};

use crate::agent_status::{paint_status_bar, stale_color, AgentStatus, STATUS_BAR_WIDTH};
use crate::session_cache::AgentiumSessionCache;
use agentium_core::session_events::{now_secs, AI_SESSION_STATE_KIND};
use agentium_core::session_loader::{SessionState, DELETED_STATUS};

/// The kinds this renderer draws: the parameterized-replaceable session-state
/// event. A `'static` slice so [`KindRenderer::kinds`] can hand it out by
/// reference.
const SESSION_KINDS: &[u32] = &[AI_SESSION_STATE_KIND];

/// How far a chip's status bar stops short of the top and bottom of its text
/// row.
const STATUS_BAR_INSET: f32 = 2.0;

/// Title shown for a session whose state carries no `title` tag.
const UNTITLED_SESSION: &str = "Untitled session";

/// A session's *current* display fields, folded live from the shared cache — what
/// the chip/card shows instead of the kind-31988 snapshot (which never carries
/// later status/title updates). Owned so the cache borrow drops before drawing.
struct LiveSession {
    /// The session's current display title.
    title: String,
    /// The session's current status tag (e.g. `working`/`idle`/`done`).
    status: String,
    /// The working directory the session runs in (for the block card).
    cwd: String,
    /// Unix seconds of the session's last activity, used to fade the status color as
    /// the session goes stale. For a folded session this is
    /// [`SessionView::last_activity`](crate::session_cache::SessionView::last_activity)
    /// — the newest of its state and conversation-message times, memoized in the
    /// shared cache so no ndb query runs in the per-frame render path. For the
    /// unfolded fallback it's the note snapshot's own `created_at`.
    last_activity: u64,
}

impl LiveSession {
    /// Build from a folded [`SessionState`] (the live cache read) or the note's own
    /// snapshot (the fallback), with `last_activity` supplied by the caller: the
    /// cache's cross-kind timestamp for a folded session, or the snapshot's
    /// `created_at` for one not folded locally.
    fn from_state(state: &SessionState, last_activity: u64) -> Self {
        Self {
            title: state.display_title().to_string(),
            status: state.status.clone(),
            cwd: state.cwd.clone(),
            last_activity,
        }
    }
}

/// Seconds since a session last did anything, `0` when its timestamp is in the
/// future (clock skew) — the age fed to [`stale_color`].
fn session_age(live: &LiveSession) -> u64 {
    now_secs().saturating_sub(live.last_activity)
}

/// Renders an agent session (kind 31988) referenced inline, e.g. by an
/// `agentium:<word-id>` word id ([`crate::reference`]) or a `nostr:` event
/// reference. Registered into [`notedeck::KindRendererRegistry`] at app startup
/// via [`Dave::kind_renderers`](crate::Dave).
///
/// Reads the SAME `Rc<RefCell<AgentiumSessionCache>>` as the parser and Dave's
/// session UI (cloned in at registration), so a status update shows on the chip in
/// realtime. Honors [`req.context`](KindRenderRequest::context): a compact
/// one-line **chip** (the session list's status bar + the session's title) inline
/// in prose, and a fuller **card** (title + status + working directory) as a
/// block embed.
///
/// A chip whose session is titled with a reference and nothing else — autowork
/// titles a session with its card's `headway:<board>/<word-id>` — draws that
/// entity's own chip as its title (see [`chip_ui`](Self::chip_ui)), not the raw
/// ref.
pub struct AgentiumSessionRenderer {
    /// Shared with the app's foreground UI and its reference parser (see
    /// [`Dave::session_cache`](crate::Dave)), so a session drawn inline folds the
    /// same cached state the open surface does — once per account, not once per
    /// surface.
    cache: Rc<RefCell<AgentiumSessionCache>>,
    /// Set while a chip draws a referenced entity's chip as its title. A
    /// session's chip drawn inside another's — a title that refers to a session
    /// — sees it set and shows its own title as text, so two sessions titled
    /// with each other's refs can't nest without end.
    nesting: Cell<bool>,
}

impl AgentiumSessionRenderer {
    /// Build the renderer over the app's shared session cache.
    pub fn new(cache: Rc<RefCell<AgentiumSessionCache>>) -> Self {
        Self {
            cache,
            nesting: Cell::new(false),
        }
    }

    /// The inline chip for `live`: its status bar and the session's title.
    ///
    /// A title that is wholly one reference draws the referenced entity's chip
    /// through the registry, without its pill (it sits in this one's) and
    /// [`Inert`](notedeck::Interaction::Inert) (a click on it is a click on
    /// this chip, which opens the session). A reference that doesn't resolve
    /// falls back to its text, as does any title drawn while already nesting.
    fn chip_ui(
        &self,
        ui: &mut egui::Ui,
        note_context: &mut NoteContext,
        req: &KindRenderRequest,
        theme: &ColorTheme,
        live: &LiveSession,
        session_id: &str,
    ) -> egui::Response {
        let title = session_title(live);
        let whole_ref = note_context
            .registries
            .reference_parsers
            .find_next(title)
            .is_some_and(|m| m.range == (0..title.len()));
        if self.nesting.get() || !whole_ref {
            return session_chip_ui(ui, theme, live, req.chrome);
        }

        self.nesting.set(true);
        let response = notedeck_ui::inline_chip_with(
            ui,
            theme,
            ("agentium-chip", session_id),
            req.chrome,
            session_glyph(theme, live),
            |ui| {
                let drawn = notedeck_ui::markdown::reference_ui(
                    ui,
                    note_context,
                    req.txn,
                    title,
                    RenderContext::Inline,
                    InlineChrome::Bare,
                    notedeck::Interaction::Inert,
                );
                if drawn.is_none() {
                    title_label_ui(ui, theme, title);
                }
            },
        );
        self.nesting.set(false);
        with_resume_hint(response, live)
    }
}

impl KindRenderer for AgentiumSessionRenderer {
    fn id(&self) -> &'static str {
        "agentium.session"
    }

    fn name(&self) -> &'static str {
        "Agentium session"
    }

    fn kinds(&self) -> &'static [u32] {
        SESSION_KINDS
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
        let Some(snapshot) = SessionState::from_note(note, None) else {
            return KindRenderResponse::new(ui.weak("invalid agentium session"));
        };

        // Fold the session's *current* state (shared cache), falling back to the
        // note's own snapshot when it isn't folded locally (a not-yet-synced
        // session, or another account's). The borrow drops with `live` (owned)
        // before we draw.
        let live = live_session(&self.cache, note_context.ndb, req.txn, note, &snapshot);

        let response = match req.context {
            RenderContext::Inline | RenderContext::Row => self.chip_ui(
                ui,
                note_context,
                req,
                &theme,
                &live,
                &snapshot.claude_session_id,
            ),
            _ => session_card_ui(ui, &theme, &live),
        };
        // Clicking the chip/card opens the session in Dave (shared helper).
        notedeck::open_on_click(ui, response, req)
    }
}

/// The session's current title/status/cwd, folded live from the shared
/// [`AgentiumSessionCache`], falling back to `snapshot` (the note's own tags) when
/// the session isn't folded locally. Reads through the memoized
/// [`session`](AgentiumSessionCache::session), keyed by the note's author + its
/// `claude_session_id` d-tag, so a chip reuses this frame's fold.
///
/// `last_activity` (which fades a stale session's status color) comes straight
/// from the folded [`SessionView`](crate::session_cache::SessionView): the cache
/// maintains it across both the session's state events and its conversation
/// messages, so this path does **no** per-frame ndb query — the fix for the
/// per-chip query storm that dominated the render of a message with many chips.
#[profiling::function]
fn live_session(
    cache: &Rc<RefCell<AgentiumSessionCache>>,
    ndb: &Ndb,
    txn: &Transaction,
    note: &Note,
    snapshot: &SessionState,
) -> LiveSession {
    let author = Pubkey::new(*note.pubkey());
    cache
        .borrow_mut()
        .session(ndb, txn, &author, &snapshot.claude_session_id)
        .map(|v| LiveSession::from_state(&v.state, v.last_activity))
        .unwrap_or_else(|| LiveSession::from_state(snapshot, snapshot.created_at))
}

/// The chip/heading title for a session: its display title, or
/// [`UNTITLED_SESSION`] when blank.
fn session_title(live: &LiveSession) -> &str {
    if live.title.is_empty() {
        UNTITLED_SESSION
    } else {
        &live.title
    }
}

/// The base status color: the mapped [`AgentStatus`] color for a known status,
/// else a muted color for an unrecognised one.
fn base_status_color(theme: &ColorTheme, status: &str) -> egui::Color32 {
    AgentStatus::from_status_str(status)
        .map(|s| s.color())
        .unwrap_or(theme.text_muted)
}

/// A status color that fades with age.
fn status_color(theme: &ColorTheme, session: &LiveSession) -> egui::Color32 {
    let age = session_age(session);
    stale_color(base_status_color(theme, &session.status), age)
}

/// A compact, single-line inline reference to a session: its status bar
/// ([`session_glyph`]) followed by the session's title, in a small rounded pill —
/// the in-prose shape ([`notedeck::RenderContext::Inline`]), versus the fuller
/// [`session_card_ui`].
/// `chrome` drops the pill when the surface asks for it.
#[profiling::function]
fn session_chip_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    live: &LiveSession,
    chrome: InlineChrome,
) -> egui::Response {
    let response = notedeck_ui::inline_chip(
        ui,
        theme,
        session_title(live),
        chrome,
        session_glyph(theme, live),
    );
    with_resume_hint(response, live)
}

/// The chip's leading glyph for `live`, for [`notedeck_ui::inline_chip`]'s icon
/// slot: the session list's status bar ([`paint_status_bar`]), in the same
/// status color fading the same way as a session row's, so the chip reads as a
/// session even when its title is another entity's chip. A tombstoned session,
/// which reads as resumable, adds a small "resume" play triangle after it.
///
/// A live session's bar is narrower than the square the slot is measured at,
/// so the chip draws a little narrower than it planned for and breaks the row
/// early by that much at worst — never late. A deleted one fills the square.
fn session_glyph(theme: &ColorTheme, live: &LiveSession) -> impl FnOnce(&mut egui::Ui, f32) {
    let color = status_color(theme, live);
    let deleted = live.status == DELETED_STATUS;
    move |ui, size| {
        // As tall as the slot (one text row) less a little at each end, the
        // way a row's bar is inset from the row's top and bottom.
        let height = ui.available_height() - 2.0 * STATUS_BAR_INSET;
        let width = if deleted { size } else { STATUS_BAR_WIDTH };
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
        let bar = egui::Rect::from_min_size(rect.min, egui::vec2(STATUS_BAR_WIDTH, height));
        paint_status_bar(ui.painter(), bar, color);
        if deleted {
            // The rest of the square, past the bar and a gap the bar's width.
            let rest = egui::Rect::from_min_max(
                egui::pos2(bar.max.x + STATUS_BAR_WIDTH, rect.min.y),
                rect.max,
            );
            resume_glyph(ui.painter(), rest.center(), size, color);
        }
    }
}

/// A tombstoned session's chip carries a hover hint: clicking it reopens
/// (revives + resumes) the session via Dave's pending-open path — the inline
/// resume affordance.
fn with_resume_hint(response: egui::Response, live: &LiveSession) -> egui::Response {
    if live.status == DELETED_STATUS {
        response.on_hover_text("Deleted — click to resume")
    } else {
        response
    }
}

/// A session title as the chip's one-line text, ellipsized when it runs out of
/// room — what [`notedeck_ui::inline_chip`] draws, for a chip whose title slot
/// is drawn by hand.
fn title_label_ui(ui: &mut egui::Ui, theme: &ColorTheme, title: &str) {
    ui.add(
        egui::Label::new(egui::RichText::new(title).color(theme.text_primary))
            .wrap_mode(egui::TextWrapMode::Truncate)
            .selectable(false),
    );
}

/// The block embed of a session ([`notedeck::RenderContext::Embed`]): its title, a
/// status line (dot + label), and its working directory. Borrowed text keeps the
/// path light.
#[profiling::function]
fn session_card_ui(ui: &mut egui::Ui, theme: &ColorTheme, live: &LiveSession) -> egui::Response {
    ui.vertical(|ui| {
        ui.label(egui::RichText::new(session_title(live)).heading().strong());
        ui.horizontal(|ui| {
            let size = ui.text_style_height(&egui::TextStyle::Small);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
            status_dot(ui.painter(), rect.center(), size, status_color(theme, live));
            ui.weak(status_label(&live.status));
        });
        if !live.cwd.is_empty() {
            ui.weak(&live.cwd);
        }
    })
    .response
}

/// The human-readable label for a status tag: the mapped [`AgentStatus`] label, or
/// the raw tag when unrecognised.
fn status_label(status: &str) -> &str {
    AgentStatus::from_status_str(status)
        .map(|s| s.label())
        .unwrap_or(status)
}

/// A filled status dot centred in a square of `size` — the block card's live
/// status indicator.
fn status_dot(painter: &egui::Painter, center: egui::Pos2, size: f32, color: egui::Color32) {
    painter.circle_filled(center, size * 0.32, color);
}

/// A small right-pointing "resume" play triangle — the tombstoned/resumable
/// indicator, drawn after a deleted session's status bar. A hollow circle is
/// headway's TODO glyph, so it is a play triangle that reads as click-to-resume
/// without colliding with it.
fn resume_glyph(painter: &egui::Painter, center: egui::Pos2, size: f32, color: egui::Color32) {
    let r = size * 0.30;
    // A right-pointing triangle centred on `center`, nudged left so it looks
    // optically centred (a triangle's visual mass sits toward its base).
    let pts = vec![
        egui::pos2(center.x - r * 0.7, center.y - r),
        egui::pos2(center.x - r * 0.7, center.y + r),
        egui::pos2(center.x + r, center.y),
    ];
    painter.add(egui::Shape::convex_polygon(pts, color, egui::Stroke::NONE));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_title_falls_back_when_blank() {
        let blank = LiveSession {
            title: String::new(),
            status: "working".into(),
            cwd: String::new(),
            last_activity: 0,
        };
        assert_eq!(session_title(&blank), UNTITLED_SESSION);

        let named = LiveSession {
            title: "Fix the parser".into(),
            status: "idle".into(),
            cwd: String::new(),
            last_activity: 0,
        };
        assert_eq!(session_title(&named), "Fix the parser");
    }

    #[test]
    fn status_label_maps_known_and_passes_through_unknown() {
        assert_eq!(status_label("needs_input"), "Needs Input");
        assert_eq!(status_label("working"), "Working");
        // An unrecognised status tag renders verbatim.
        assert_eq!(status_label("compacting"), "compacting");
    }
}
