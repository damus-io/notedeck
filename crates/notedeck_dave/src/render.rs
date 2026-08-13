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

use std::cell::RefCell;
use std::rc::Rc;

use enostr::Pubkey;
use nostrdb::{Ndb, Note, Transaction};
use notedeck::{
    ColorTheme, KindRenderRequest, KindRenderResponse, KindRenderer, NoteContext, RenderContext,
};

use crate::agent_status::AgentStatus;
use crate::session_cache::AgentiumSessionCache;
use agentium_core::session_events::AI_SESSION_STATE_KIND;
use agentium_core::session_loader::{SessionState, DELETED_STATUS};

/// The kinds this renderer draws: the parameterized-replaceable session-state
/// event. A `'static` slice so [`KindRenderer::kinds`] can hand it out by
/// reference.
const SESSION_KINDS: &[u32] = &[AI_SESSION_STATE_KIND];

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
}

impl LiveSession {
    /// Build from a folded [`SessionState`] (the live cache read) or the note's own
    /// snapshot (the fallback).
    fn from_state(state: &SessionState) -> Self {
        Self {
            title: state.display_title().to_string(),
            status: state.status.clone(),
            cwd: state.cwd.clone(),
        }
    }
}

/// Renders an agent session (kind 31988) referenced inline, e.g. by an
/// `agentium:<word-id>` word id ([`crate::reference`]) or a `nostr:` event
/// reference. Registered into [`notedeck::KindRendererRegistry`] at app startup
/// via [`Dave::kind_renderers`](crate::Dave).
///
/// Reads the SAME `Rc<RefCell<AgentiumSessionCache>>` as the parser and Dave's
/// session UI (cloned in at registration), so a status update shows on the chip in
/// realtime. Honors [`req.context`](KindRenderRequest::context): a compact
/// one-line **chip** (a live status dot + the session's title) inline in prose,
/// and a fuller **card** (title + status + working directory) as a block embed.
pub struct AgentiumSessionRenderer {
    /// Shared with the app's foreground UI and its reference parser (see
    /// [`Dave::session_cache`](crate::Dave)), so a session drawn inline folds the
    /// same cached state the open surface does — once per account, not once per
    /// surface.
    cache: Rc<RefCell<AgentiumSessionCache>>,
}

impl AgentiumSessionRenderer {
    /// Build the renderer over the app's shared session cache.
    pub fn new(cache: Rc<RefCell<AgentiumSessionCache>>) -> Self {
        Self { cache }
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
            RenderContext::Inline => session_chip_ui(ui, &theme, &live),
            _ => session_card_ui(ui, &theme, &live),
        };
        // Clicking the chip/card opens the session in Dave (shared helper).
        notedeck::open_on_click(ui, response, note)
    }
}

/// The session's current title/status/cwd, folded live from the shared
/// [`AgentiumSessionCache`], falling back to `snapshot` (the note's own tags) when
/// the session isn't folded locally. Reads through the memoized
/// [`session`](AgentiumSessionCache::session), keyed by the note's author + its
/// `claude_session_id` d-tag, so a chip reuses this frame's fold.
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
        .map(|v| LiveSession::from_state(&v.state))
        .unwrap_or_else(|| LiveSession::from_state(snapshot))
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

/// The color of the live status dot: the mapped [`AgentStatus`] color for a known
/// status, else a muted dot for an unrecognised one.
fn status_color(theme: &ColorTheme, status: &str) -> egui::Color32 {
    AgentStatus::from_status_str(status)
        .map(|s| s.color())
        .unwrap_or(theme.text_muted)
}

/// A compact, single-line inline reference to a session: a live status dot
/// followed by the session's title, in a small rounded pill — the in-prose shape
/// ([`notedeck::RenderContext::Inline`]), versus the fuller [`session_card_ui`].
fn session_chip_ui(ui: &mut egui::Ui, theme: &ColorTheme, live: &LiveSession) -> egui::Response {
    let color = status_color(theme, &live.status);
    // A tombstoned session reads as resumable: a hollow ring instead of a filled
    // dot, and a hover hint. Clicking it reopens (revives + resumes) the session
    // via Dave's pending-open path — the inline resume affordance.
    let deleted = live.status == DELETED_STATUS;
    let response = notedeck_ui::inline_chip(ui, theme, session_title(live), move |ui, size| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
        if deleted {
            status_ring(ui.painter(), rect.center(), size, color);
        } else {
            status_dot(ui.painter(), rect.center(), size, color);
        }
    });
    if deleted {
        response.on_hover_text("Deleted — click to resume")
    } else {
        response
    }
}

/// The block embed of a session ([`notedeck::RenderContext::Embed`]): its title, a
/// status line (dot + label), and its working directory. Borrowed text keeps the
/// path light.
fn session_card_ui(ui: &mut egui::Ui, theme: &ColorTheme, live: &LiveSession) -> egui::Response {
    ui.vertical(|ui| {
        ui.label(egui::RichText::new(session_title(live)).heading().strong());
        ui.horizontal(|ui| {
            let size = ui.text_style_height(&egui::TextStyle::Small);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
            status_dot(
                ui.painter(),
                rect.center(),
                size,
                status_color(theme, &live.status),
            );
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

/// A filled status dot centred in a square of `size` — the live status indicator,
/// the agentium analog of headway's column [`StatusIcon`].
fn status_dot(painter: &egui::Painter, center: egui::Pos2, size: f32, color: egui::Color32) {
    painter.circle_filled(center, size * 0.32, color);
}

/// A hollow status ring — the tombstoned/resumable indicator, distinguishing a
/// deleted session's chip from a live one's filled [`status_dot`].
fn status_ring(painter: &egui::Painter, center: egui::Pos2, size: f32, color: egui::Color32) {
    painter.circle_stroke(center, size * 0.30, egui::Stroke::new(size * 0.12, color));
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
        };
        assert_eq!(session_title(&blank), UNTITLED_SESSION);

        let named = LiveSession {
            title: "Fix the parser".into(),
            status: "idle".into(),
            cwd: String::new(),
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
