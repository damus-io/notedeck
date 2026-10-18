use std::cell::RefCell;
use std::path::PathBuf;

use egui::{Align, Color32, Layout, Pos2, Sense};
use nostrdb::Transaction;
use notedeck::NoteContext;
use notedeck_ui::app_images;

use crate::agent_status::{paint_status_bar, stale_color, AgentStatus, STATUS_BAR_WIDTH};
use crate::card_titles::CardLookup;
use crate::collapse_state::CollapseState;
use crate::config::AiMode;
use crate::focus_queue::{FocusPriority, FocusQueue};
use crate::session::{now_unix, ChatSession, Nesting, SessionId, SessionManager};
use crate::ui::keybind_hint::{keybind_hint, paint_keybind_hint, KeybindHint};
use crate::ui::SquareLoadingSpinner;

/// Actions that can be triggered from the session list UI
#[derive(Debug, Clone)]
pub enum SessionListAction {
    NewSession,
    SwitchTo(SessionId),
    Delete(SessionId),
    Rename(SessionId, String),
    DismissDone(SessionId),
    Duplicate(SessionId),
    Reset(SessionId),
    NewWorktree(SessionId),
    DeleteWorktree(SessionId),
    ToggleHostCollapse(String),
    ToggleProjectCollapse(String, PathBuf),
    ToggleCwdCollapse(String, PathBuf),
    NewSessionInCwd(String, PathBuf),
}

/// The sessions Ctrl+J / Ctrl+K would cycle to, relative to the active one.
/// `None` everywhere unless ctrl is held and there's more than one session.
#[derive(Clone, Copy, Default)]
struct CycleHints {
    next: Option<SessionId>,
    prev: Option<SessionId>,
}

impl CycleHints {
    fn compute(
        session_manager: &mut SessionManager,
        collapse: &CollapseState,
        active_id: Option<SessionId>,
    ) -> Self {
        let order = session_manager.visual_order(collapse);
        // Cycling to self is meaningless with a single session.
        if order.len() < 2 {
            return Self::default();
        }
        let Some(pos) = active_id.and_then(|a| order.iter().position(|&id| id == a)) else {
            return Self::default();
        };
        let len = order.len();
        Self {
            next: Some(order[(pos + 1) % len]),
            prev: Some(order[if pos == 0 { len - 1 } else { pos - 1 }]),
        }
    }

    /// The hint label ("J"/"K") to paint on `id`'s row, if it's adjacent to the
    /// active session.
    fn hint_for(&self, id: SessionId) -> Option<&'static str> {
        if self.next == Some(id) {
            Some("J")
        } else if self.prev == Some(id) {
            Some("K")
        } else {
            None
        }
    }
}

/// What a session row needs to draw the inline references in its title — a
/// session titled `headway:<board>/<word-id>` shows the card's chip, the same
/// one the chat draws for that ref: the note context the registered
/// [`ReferenceParser`](notedeck::ReferenceParser)s and
/// [`KindRenderer`](notedeck::KindRenderer)s work against, and the transaction
/// every row reads its resolved note in.
///
/// Behind a `RefCell` in [`SessionListUi`] because the row renderers borrow the
/// list immutably (each row's session is borrowed out of its `SessionManager`)
/// while a kind renderer draws with `&mut NoteContext`.
pub struct TitleRefs<'a, 'n> {
    pub note: &'a mut NoteContext<'n>,
    pub txn: &'a Transaction,
}

/// UI component for displaying the session list sidebar
pub struct SessionListUi<'a, 'n> {
    session_manager: &'a mut SessionManager,
    focus_queue: &'a FocusQueue,
    collapse_state: &'a CollapseState,
    ctrl_held: bool,
    /// Normal mode is walking the list: wash the active row like the
    /// chat's block cursor, so it's clear where `j` / `k` are.
    chord_cursor: bool,
    /// The active session changed since the list last drew: scroll its row
    /// into view this frame. Set by `sessions_list_ui`.
    scroll_to_active: bool,
    /// Draws the references in a title as their inline widgets. `None` (the
    /// ctx-less test harnesses) paints every title as plain text.
    title_refs: Option<RefCell<TitleRefs<'a, 'n>>>,
}

impl<'a, 'n> SessionListUi<'a, 'n> {
    pub fn new(
        session_manager: &'a mut SessionManager,
        focus_queue: &'a FocusQueue,
        collapse_state: &'a CollapseState,
        ctrl_held: bool,
    ) -> Self {
        SessionListUi {
            session_manager,
            focus_queue,
            collapse_state,
            ctrl_held,
            chord_cursor: false,
            scroll_to_active: false,
            title_refs: None,
        }
    }

    /// Draw references in session titles (e.g. `headway:<board>/<word-id>`)
    /// as their registered inline widgets instead of plain text.
    pub fn title_refs(mut self, note: &'a mut NoteContext<'n>, txn: &'a Transaction) -> Self {
        self.title_refs = Some(RefCell::new(TitleRefs { note, txn }));
        self
    }

    /// Mark the active row as normal mode's cursor in the sessions pane.
    pub fn chord_cursor(mut self, chord_cursor: bool) -> Self {
        self.chord_cursor = chord_cursor;
        self
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<SessionListAction> {
        let mut action: Option<SessionListAction> = None;

        ui.vertical(|ui| {
            // Header with New Agent button
            action = self.header_ui(ui);

            ui.add_space(8.0);

            // Scrollable list of sessions
            egui::ScrollArea::vertical()
                .id_salt("session_list_scroll")
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    if let Some(session_action) = self.sessions_list_ui(ui) {
                        action = Some(session_action);
                    }
                });
        });

        action
    }

    fn header_ui(&self, ui: &mut egui::Ui) -> Option<SessionListAction> {
        let mut action = None;

        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Sessions").size(18.0).strong());

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let icon = app_images::new_message_image()
                    .max_height(20.0)
                    .sense(Sense::click());

                if ui
                    .add(icon)
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text("New Chat")
                    .clicked()
                {
                    action = Some(SessionListAction::NewSession);
                }
            });
        });

        action
    }

    fn sessions_list_ui(&mut self, ui: &mut egui::Ui) -> Option<SessionListAction> {
        let mut action = None;
        self.refresh_card_titles(ui.input(|i| i.time));
        let active_id = self.session_manager.active_id();
        // Follow the active session when something other than a click moves
        // it (normal mode's gg / G / j / k, dd, Ctrl+J / K, the focus queue),
        // so it never lands off-screen. A clicked row is already visible, and
        // scroll_to_me(None) leaves a visible row where it is.
        let last_active_id = ui.make_persistent_id("session_list_last_active");
        let last_active = ui.data(|d| d.get_temp::<Option<SessionId>>(last_active_id));
        self.scroll_to_active = last_active != Some(active_id);
        if self.scroll_to_active {
            ui.data_mut(|d| d.insert_temp(last_active_id, active_id));
        }
        // Sessions Ctrl+J / Ctrl+K (cycle next/previous) would jump to, used to
        // paint a J/K hint on the rows adjacent to the active one while ctrl is
        // held. Mirrors the wrap-around order in update::cycle_agent.
        let cycle = if self.ctrl_held {
            CycleHints::compute(self.session_manager, self.collapse_state, active_id)
        } else {
            CycleHints::default()
        };
        let mut visual_index: usize = 0;
        let host_groups = self.session_manager.host_groups().to_vec();

        // Agents grouped by host → project → cwd (deterministically ordered)
        for host_group in &host_groups {
            if let Some(a) =
                host_section_ui(ui, self, host_group, &mut visual_index, active_id, cycle)
            {
                action = Some(a);
            }
        }

        // Chats section (pre-computed IDs)
        let chat_ids = self.session_manager.chat_ids().to_vec();
        if !chat_ids.is_empty() {
            ui.label(
                egui::RichText::new("Chats")
                    .size(12.0)
                    .color(ui.visuals().weak_text_color()),
            );
            ui.add_space(4.0);
            for id in chat_ids {
                if let Some(session) = self.session_manager.get(id) {
                    if let Some(a) = self.render_session_item(
                        ui,
                        session,
                        visual_index,
                        active_id,
                        RowPlace {
                            cwd: None,
                            own_bar: true,
                        },
                        cycle,
                    ) {
                        action = Some(a);
                    }
                    visual_index += 1;
                }
            }
        }

        action
    }

    fn render_session_item(
        &self,
        ui: &mut egui::Ui,
        session: &crate::session::ChatSession,
        index: usize,
        active_id: Option<SessionId>,
        place: RowPlace<'_>,
        cycle: CycleHints,
    ) -> Option<SessionListAction> {
        let is_active = Some(session.id) == active_id;
        let cycle_hint = cycle.hint_for(session.id);
        let shortcut_hint = if self.ctrl_held && index < 9 {
            Some(index + 1)
        } else {
            None
        };
        let queue_priority = self.focus_queue.get_session_priority(session.id);

        let is_renaming = renaming(ui.ctx()) == Some(session.id);

        let display_title = if is_renaming {
            ""
        } else {
            session.details.display_title()
        };
        let RowResponse {
            response,
            action: dot_action,
        } = match session.ai_mode {
            AiMode::Agentic => self.agent_row_ui(
                ui,
                AgentRow {
                    session_id: session.id,
                    title: display_title,
                    cwd_display: place.cwd,
                    is_active,
                    shortcut_hint,
                    cycle_hint,
                    status: session.status(),
                    last_activity: session.last_activity,
                    queue_priority,
                    // Only a row with no folder header above it shows its
                    // worktree's +/-; under a header, the header does.
                    git_lines: place.cwd.and_then(|_| git_lines(session)),
                    own_bar: place.own_bar,
                },
            ),
            _ => self.chat_row_ui(
                ui,
                session.id,
                display_title,
                is_active,
                shortcut_hint,
                cycle_hint,
                queue_priority,
            ),
        };

        let mut action = dot_action;

        if is_active && self.scroll_to_active {
            response.scroll_to_me(None);
        }

        if is_renaming {
            let renamed = rename_ui(ui, session.id, |ui, buf| {
                inline_rename_field(ui, response.rect, buf)
            });
            if let Some(title) = renamed {
                action = Some(SessionListAction::Rename(session.id, title));
            }
        } else if response.clicked() {
            action = Some(SessionListAction::SwitchTo(session.id));
        }

        // Long-press to rename (mobile)
        if !is_renaming {
            let press_id = egui::Id::unique("session_long_press");
            if response.is_pointer_button_down_on() {
                let now = ui.input(|i| i.time);
                let start: Option<PressStart> = ui.data(|d| d.get_temp(press_id));
                if start.is_none() {
                    ui.data_mut(|d| d.insert_temp(press_id, PressStart(now)));
                } else if let Some(s) = start {
                    if now - s.0 > 0.5 {
                        start_rename(ui.ctx(), session.id, session.details.display_title());
                        ui.data_mut(|d| d.remove_by_type::<PressStart>());
                    }
                }
            } else {
                ui.data_mut(|d| d.remove_by_type::<PressStart>());
            }
        }

        let ctrl_held = self.ctrl_held;
        let is_agentic = session.ai_mode == AiMode::Agentic;
        let confirm_id = egui::Id::unique("confirm_delete_worktree").with(session.id);

        notedeck_ui::context_menu::context_menu(&response, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Rename").clicked() {
                    start_rename(ui.ctx(), session.id, session.details.display_title());
                    ui.close();
                }
                if is_active && ctrl_held {
                    keybind_hint(ui, "⌃⇧R");
                }
            });
            if is_agentic {
                ui.horizontal(|ui| {
                    if ui.button("Duplicate").clicked() {
                        action = Some(SessionListAction::Duplicate(session.id));
                        ui.close();
                    }
                    if is_active && ctrl_held {
                        keybind_hint(ui, "⌃⇧T");
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("Clear").clicked() {
                        action = Some(SessionListAction::Reset(session.id));
                        ui.close();
                    }
                    if is_active && ctrl_held {
                        keybind_hint(ui, "⌃⇧K");
                    }
                });
            }
            ui.horizontal(|ui| {
                if ui.button("Delete").clicked() {
                    action = Some(SessionListAction::Delete(session.id));
                    ui.close();
                }
                if ctrl_held {
                    keybind_hint(ui, "Del");
                }
            });
            // A remote session's tree is on its host, out of reach of a
            // worktree made here.
            let is_git_repo = !session.is_remote()
                && session
                    .agentic
                    .as_ref()
                    .and_then(|a| a.git_status.current())
                    .is_some_and(|status| status.work_tree().is_some());
            if is_git_repo && ui.button("New worktree from this session").clicked() {
                action = Some(SessionListAction::NewWorktree(session.id));
                ui.close();
            }
            let is_worktree = is_git_repo
                && session
                    .cwd()
                    .map(|p| crate::worktree::is_linked_worktree(p))
                    .unwrap_or(false);
            if is_worktree {
                if let Some(a) = delete_worktree_menu_item(ui, session.id, confirm_id) {
                    action = Some(a);
                }
            }
        });

        // Reset confirmation state when the menu is closed.
        if !response.context_menu_opened() {
            ui.ctx().data_mut(|d| d.insert_temp(confirm_id, false));
        }

        action
    }

    /// Render an agentic session row: the status bar, the title on one line
    /// (see [`title_ui`](Self::title_ui); a session titled with its card's
    /// ref draws the card's inline chip) and the focus dot.
    ///
    /// When `cwd_display` is `Some` (a session with no folder header above
    /// it) the row is two lines: the cwd path under the title, the worktree's
    /// git +/- at its bottom right and the focus dot moved to the top right.
    /// A row under a folder header draws no git +/-: the header shows its
    /// worktree's once.
    fn agent_row_ui(&self, ui: &mut egui::Ui, row: AgentRow<'_>) -> RowResponse {
        let height = if row.cwd_display.is_some() {
            48.0
        } else {
            32.0
        };
        let desired_size = egui::vec2(ui.available_width(), height);
        let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click());
        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);

        paint_row_background(ui, rect, row.is_active, self.chord_cursor, &response);

        // Status color indicator (left edge vertical bar).
        let status_color = row_status_color(row.status, row.last_activity);
        if row.own_bar {
            let status_bar_rect = egui::Rect::from_min_size(
                rect.left_top() + egui::vec2(BAR_LEFT, BAR_INSET),
                egui::vec2(STATUS_BAR_WIDTH, rect.height() - 2.0 * BAR_INSET),
            );
            paint_status_bar(ui.painter(), status_bar_rect, status_color);
        }
        let mut text_start_x = TEXT_LEFT;

        // A pending placeholder (a remote spawn/resume in flight) shows a small
        // loading spinner before its "Connecting…" title so the row reads as
        // in-progress. Reuses SquareLoadingSpinner (interval-based repaint) over
        // egui::Spinner (which repaints every frame). Drawn in a detached child ui
        // so it paints at an absolute rect without advancing the row's layout.
        if row.status == AgentStatus::Pending {
            let spin_size = 12.0;
            let spin_rect = egui::Rect::from_min_size(
                egui::pos2(
                    rect.left() + text_start_x,
                    rect.center().y - spin_size / 2.0,
                ),
                egui::vec2(PENDING_SPINNER_SLOT, spin_size),
            );
            let mut spinner_ui = ui.new_child(egui::UiBuilder::new().max_rect(spin_rect));
            spinner_ui.add(
                SquareLoadingSpinner::new()
                    .size(spin_size)
                    .color(status_color),
            );
            text_start_x += PENDING_SPINNER_SLOT + 2.0;
        }

        let hints: &[(&str, &str)] = &[("⇧T", "Duplicate"), ("⇧K", "Clear"), ("⇧R", "Rename")];
        // Standalone (cwd) rows get the worktree treatment: status dot moves to
        // the top-right and the git +/- line stats sit on the bottom-right next
        // to the cwd path. Grouped rows keep the centered layout.
        let AgentRightSide {
            title_right,
            cwd_right,
            action: dot_action,
        } = if row.cwd_display.is_some() {
            render_agent_row_right_side(
                ui,
                rect,
                row.session_id,
                row.is_active,
                self.ctrl_held,
                row.shortcut_hint,
                row.cycle_hint,
                row.queue_priority,
                row.git_lines,
                hints,
            )
        } else {
            let RightSide { width, action } = render_row_right_side(
                ui,
                rect,
                row.session_id,
                row.is_active,
                self.ctrl_held,
                row.shortcut_hint,
                row.cycle_hint,
                row.queue_priority,
                hints,
            );
            AgentRightSide {
                title_right: width,
                cwd_right: width,
                action,
            }
        };

        let title_max_width = rect.width() - text_start_x - title_right;
        let cwd_max_width = rect.width() - text_start_x - cwd_right;
        let title_height = row_height(ui, &title_font());
        let title_top = if row.cwd_display.is_some() {
            rect.top() + ROW_PAD
        } else {
            rect.center().y - title_height / 2.0
        };
        self.title_ui(
            ui,
            row.title,
            egui::pos2(rect.left() + text_start_x, title_top),
            title_max_width,
        );

        if let Some(cwd) = row.cwd_display {
            let cwd_pos = egui::pos2(rect.left() + text_start_x, title_top + title_height + 1.0);
            cwd_inline_ui(ui, cwd, cwd_pos, cwd_max_width);
        }

        RowResponse {
            response,
            action: dot_action,
        }
    }

    /// Render a chat session row (no status bar, no cwd).
    #[allow(clippy::too_many_arguments)]
    fn chat_row_ui(
        &self,
        ui: &mut egui::Ui,
        session_id: SessionId,
        title: &str,
        is_active: bool,
        shortcut_hint: Option<usize>,
        cycle_hint: Option<&str>,
        queue_priority: Option<FocusPriority>,
    ) -> RowResponse {
        let desired_size = egui::vec2(ui.available_width(), 32.0);
        let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click());
        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);

        paint_row_background(ui, rect, is_active, self.chord_cursor, &response);

        let text_start_x = 8.0;
        let hints: &[(&str, &str)] = &[("⇧R", "Rename")];
        let RightSide {
            width: right_used,
            action: dot_action,
        } = render_row_right_side(
            ui,
            rect,
            session_id,
            is_active,
            self.ctrl_held,
            shortcut_hint,
            cycle_hint,
            queue_priority,
            hints,
        );

        let max_text_width = rect.width() - text_start_x - right_used;
        let font_id = egui::FontId::proportional(14.0);
        let title_height = ui
            .painter()
            .layout_no_wrap(title.to_string(), font_id, ui.visuals().text_color())
            .size()
            .y;
        let title_top = rect.center().y - title_height / 2.0;
        self.title_ui(
            ui,
            title,
            egui::pos2(rect.left() + text_start_x, title_top),
            max_text_width,
        );

        RowResponse {
            response,
            action: dot_action,
        }
    }

    /// Refresh the kept card titles through [`title_refs`](Self::title_refs)
    /// (see [`refresh_card_titles`]). Without them there is nothing to look
    /// up through, and the rows draw what was kept.
    fn refresh_card_titles(&mut self, now: f64) {
        let Some(refs) = &self.title_refs else {
            return;
        };
        refresh_card_titles(self.session_manager, &refs.borrow(), now);
    }

    /// Draw a row's `title` with its top-left at `pos`, clipped to `max_width`.
    ///
    /// A title holding a reference the registered parsers recognize — a
    /// session titled with its card's `headway:<board>/<word-id>` — is laid out
    /// as text runs and the references' inline widgets (see
    /// [`title_with_refs_ui`]). One with none, or a list drawn without
    /// [`title_refs`](Self::title_refs), is painted as plain text.
    fn title_ui(&self, ui: &mut egui::Ui, title: &str, pos: Pos2, max_width: f32) {
        let Some(refs) = &self.title_refs else {
            render_title(ui, title, pos, max_width);
            return;
        };
        let mut refs = refs.borrow_mut();
        if refs
            .note
            .registries
            .reference_parsers
            .find_next(title)
            .is_none()
        {
            render_title(ui, title, pos, max_width);
            return;
        }
        title_with_refs_ui(
            ui,
            &mut refs,
            title,
            pos,
            max_width,
            &egui::FontId::proportional(14.0),
            // The row's click switches to its session; the chip mustn't take
            // it.
            notedeck::Interaction::Inert,
        );
    }
}

/// Look up the current title and outline of each card a session is on, and
/// of each such card's parent, unless the lookup kept for it still holds
/// (nothing it read has changed, see [`CardTitles`](crate::card_titles::CardTitles)),
/// so the list and the scene draw from text kept on the [`SessionManager`]
/// instead of folding a board per session per frame. A lookup that changed
/// what was kept regroups the list, since card parents link successor chains
/// and the parent's title and progress head them.
pub fn refresh_card_titles(
    session_manager: &mut SessionManager,
    refs: &TitleRefs<'_, '_>,
    now: f64,
) {
    let note = &*refs.note;
    let registries = note.registries;
    let ctx = notedeck::ReferenceResolveCtx {
        ndb: note.ndb,
        txn: refs.txn,
        selected_account: Some(*note.accounts.selected_account_pubkey()),
    };
    let mut titles = std::mem::take(&mut session_manager.card_titles);
    let mut changed = false;
    for session in session_manager.iter() {
        let Some(card_ref) = session.details.card_ref() else {
            continue;
        };
        changed |= titles.refresh(registries, &ctx, card_ref, now);
        let Some(parent) = titles.parent(card_ref) else {
            continue;
        };
        if !titles.is_current_in(registries, &ctx, parent, now) {
            let parent = parent.to_owned();
            changed |= titles.store(&parent, CardLookup::of(registries, &ctx, &parent), now);
        }
    }
    session_manager.card_titles = titles;
    if changed {
        session_manager.lineage_changed();
    }
}

/// Where a session's row sits in the list. Every row is one session.
#[derive(Clone, Copy)]
struct RowPlace<'a> {
    /// The path shown in the row when the session is alone in its cwd, with
    /// no folder header above it.
    cwd: Option<&'a str>,
    /// Whether the row paints its own status bar. A chain link doesn't:
    /// [`chain_ui`] paints one bar down the whole chain.
    own_bar: bool,
}

/// Lines a session's worktree has added and deleted against HEAD.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct GitLines {
    pub added: usize,
    pub deleted: usize,
}

/// `session`'s +/- lines against HEAD, if its repo has tracked changes. A
/// remote session's come from its host's inspector snapshot.
pub(crate) fn git_lines(session: &ChatSession) -> Option<GitLines> {
    let diff = session
        .agentic
        .as_ref()?
        .git_status
        .current()?
        .work_tree()?;
    let lines = GitLines {
        added: diff.added_lines,
        deleted: diff.deleted_lines,
    };
    (lines.added > 0 || lines.deleted > 0).then_some(lines)
}

/// What [`SessionListUi::agent_row_ui`] draws.
struct AgentRow<'s> {
    session_id: SessionId,
    /// The session's title: a card's ref draws as the card's inline chip.
    title: &'s str,
    /// The cwd path, for a session alone in its cwd.
    cwd_display: Option<&'s str>,
    is_active: bool,
    /// The Ctrl+<n> number to show, while ctrl is held.
    shortcut_hint: Option<usize>,
    /// "J" / "K" on the rows Ctrl+J / K would move to.
    cycle_hint: Option<&'static str>,
    status: AgentStatus,
    /// Unix time of the session's last activity.
    last_activity: Option<u64>,
    queue_priority: Option<FocusPriority>,
    /// The worktree's +/- lines, drawn on a row with a `cwd_display` only.
    git_lines: Option<GitLines>,
    /// Whether to paint the row's status bar (see [`RowPlace::own_bar`]).
    own_bar: bool,
}

/// Left padding of a row's text, past the status bar.
const TEXT_LEFT: f32 = 12.0;
/// The status bar's offset from a row's left edge.
const BAR_LEFT: f32 = 2.0;
/// How far a row's status bar stops short of its top and bottom.
const BAR_INSET: f32 = 4.0;

/// A row's status colour, faded toward a desaturated version of itself as
/// the session goes stale (no activity since `last_activity`).
fn row_status_color(status: AgentStatus, last_activity: Option<u64>) -> egui::Color32 {
    let age = last_activity
        .map(|t| now_unix().saturating_sub(t))
        .unwrap_or(0);
    stale_color(status.color(), age)
}

/// Width of the "Connecting…" spinner before a pending row's title.
const PENDING_SPINNER_SLOT: f32 = 22.0;
/// Space above and below a row's text.
const ROW_PAD: f32 = 6.0;

/// A row title's font.
fn title_font() -> egui::FontId {
    egui::FontId::proportional(14.0)
}

/// The font of a row's cwd line and of git +/- counts.
fn cwd_font() -> egui::FontId {
    egui::FontId::monospace(10.0)
}

/// Height of one line of `font`.
fn row_height(ui: &egui::Ui, font: &egui::FontId) -> f32 {
    ui.fonts_mut(|f| f.row_height(font))
}

/// Write `secs` as a short age: `now`, `12m`, `3h`, `2d`.
pub(crate) fn age_text(secs: u64, out: &mut String) {
    use std::fmt::Write;
    let _ = match secs {
        0..60 => write!(out, "now"),
        60..3600 => write!(out, "{}m", secs / 60),
        3600..86400 => write!(out, "{}h", secs / 3600),
        _ => write!(out, "{}d", secs / 86400),
    };
}

/// Lay `title` out left to right in a child ui at `pos`, `max_width` wide:
/// its text runs painted in `font_id` (a row's title is
/// [`render_title`]'s 14px, the chat header's 13px), and each reference in it drawn
/// by the [`KindRenderer`](notedeck::KindRenderer) for the kind it resolves to
/// (a headway card's chip) with [`InlineChrome::Bare`](notedeck::InlineChrome::Bare):
/// the chat's widget for that ref without its pill, since the row already
/// frames the title.
///
/// `interaction` says whether the widget takes clicks. A row draws it
/// [`Inert`](notedeck::Interaction::Inert): a click on the title is a click
/// on the row, switching to the session, not one that opens the referenced
/// card. The chat header draws it [`Open`](notedeck::Interaction::Open), and
/// the action a click raises goes onto
/// [`app_actions`](notedeck::AppContext::app_actions) for the host to route
/// to the app that owns the entity (headway, for a card), as a chip in the
/// chat's markdown does. A reference that doesn't resolve — its card isn't in
/// the local db yet — stays text, so a loose parser match never shows as a
/// broken chip. Whatever overflows `max_width` is clipped, like a long plain
/// title.
pub(crate) fn title_with_refs_ui(
    ui: &mut egui::Ui,
    refs: &mut TitleRefs<'_, '_>,
    title: &str,
    pos: Pos2,
    max_width: f32,
    font_id: &egui::FontId,
    interaction: notedeck::Interaction,
) {
    let row_height = ui.fonts_mut(|f| f.row_height(font_id));
    // Leave room on both sides of the title's centre line, so `Align::Center`
    // centres a widget taller than the text row on the line the plain title
    // would sit on rather than letting it hang off the bottom of a
    // text-row-high rect. A bare chip is one row tall, but a renderer that
    // ignores the hint still draws its outlined pill.
    let center = pos + egui::vec2(max_width / 2.0, row_height / 2.0);
    let rect = egui::Rect::from_center_size(center, egui::vec2(max_width, row_height + 8.0));
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    // Clip sideways only, so such an outline isn't shaved off.
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    child.spacing_mut().item_spacing.x = 0.0;
    // An inline widget sizes itself to the surrounding prose, which it takes
    // to be `TextStyle::Body`. Here the prose is the title, so the chip
    // matches the plain titles above and below it instead of standing a size
    // larger.
    child
        .style_mut()
        .text_styles
        .insert(egui::TextStyle::Body, font_id.clone());

    let mut rest = title;
    while let Some(m) = refs.note.registries.reference_parsers.find_next(rest) {
        title_run_ui(&mut child, &rest[..m.range.start], font_id);
        let matched = &rest[m.range.clone()];
        let drawn = notedeck_ui::markdown::reference_ui(
            &mut child,
            refs.note,
            refs.txn,
            matched,
            notedeck::RenderContext::Inline,
            // The row already frames its title; a pill inside it is a box in
            // a box.
            notedeck::InlineChrome::Bare,
            interaction,
        );
        match drawn {
            Some(drawn) => {
                if let Some(action) = drawn.action {
                    refs.note.app_actions.push(action);
                }
            }
            None => title_run_ui(&mut child, matched, font_id),
        }
        rest = &rest[m.range.end..];
    }
    title_run_ui(&mut child, rest, font_id);
}

/// Paint one plain-text run of a title at `ui`'s cursor and advance past it.
///
/// Painted rather than added as a `Label`: a label is a widget, and one lying
/// over the row would take the row's hover (and, if selectable, its click).
fn title_run_ui(ui: &mut egui::Ui, text: &str, font_id: &egui::FontId) {
    if text.is_empty() {
        return;
    }
    let color = ui.visuals().text_color();
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font_id.clone(), color);
    let (_, rect) = ui.allocate_space(galley.size());
    ui.painter().galley(rect.min, galley, color);
}

#[derive(Clone, Copy)]
struct PressStart(f64);

/// Where the open rename keeps the session it renames and the title typed
/// so far.
fn rename_state_id() -> egui::Id {
    egui::Id::unique("session_rename_state")
}

/// Open the rename of session `id`, its field starting at `title`. The
/// session list draws the field over the session's row, the scene view in
/// its toolbar (see [`rename_ui`]).
pub(crate) fn start_rename(ctx: &egui::Context, id: SessionId, title: &str) {
    let state = (id, title.to_owned());
    ctx.data_mut(|d| d.insert_temp(rename_state_id(), state));
}

/// The session being renamed, if a rename is open.
pub(crate) fn renaming(ctx: &egui::Context) -> Option<SessionId> {
    ctx.data(|d| d.get_temp::<(SessionId, String)>(rename_state_id()))
        .map(|(id, _)| id)
}

/// The inline session rename field's id. Keybindings check it so Esc cancels
/// the rename instead of entering normal mode.
pub(crate) fn rename_edit_id() -> egui::Id {
    egui::Id::unique("session_rename_edit")
}

/// Draw the open rename of session `id`, if it is the one being renamed:
/// `field` draws the text field, with [`rename_edit_id`], over the title
/// typed so far. Returns the new title once Enter, or a click away,
/// confirms it; Esc cancels it. Either closes the rename.
pub(crate) fn rename_ui(
    ui: &mut egui::Ui,
    id: SessionId,
    field: impl FnOnce(&mut egui::Ui, &mut String) -> egui::Response,
) -> Option<String> {
    let state_id = rename_state_id();
    let (renamed, mut buf) = ui.data(|d| d.get_temp::<(SessionId, String)>(state_id))?;
    if renamed != id {
        return None;
    }
    let edit = field(ui, &mut buf);
    if !edit.has_focus() && !edit.lost_focus() {
        edit.request_focus();
    }

    let confirmed = edit.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if !confirmed && !edit.lost_focus() {
        ui.data_mut(|d| d.insert_temp(state_id, (id, buf)));
        return None;
    }
    ui.data_mut(|d| d.remove::<(SessionId, String)>(state_id));
    let cancelled =
        !confirmed && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    (!cancelled).then_some(buf)
}

/// The session list's rename field, floating over the row at `row`.
fn inline_rename_field(ui: &mut egui::Ui, row: egui::Rect, buf: &mut String) -> egui::Response {
    let edit_rect = row.shrink2(egui::vec2(8.0, 4.0));
    egui::Area::new(egui::Id::unique("rename_textedit"))
        .fixed_pos(edit_rect.min)
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            ui.set_width(edit_rect.width());
            ui.add(
                egui::TextEdit::singleline(buf)
                    .id(rename_edit_id())
                    .font(egui::FontId::proportional(14.0))
                    .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(4, 2))),
            )
        })
        .inner
}

/// Truncate text from the start, showing "…" + the longest suffix that fits.
/// Uses binary search over character offsets for O(log n) performance.
pub(crate) fn truncate_start(
    ui: &egui::Ui,
    text: &str,
    font: &egui::FontId,
    max_width: f32,
) -> String {
    if text.is_empty() {
        return String::new();
    }
    let measure = |s: String| -> f32 {
        ui.painter()
            .layout_no_wrap(s, font.clone(), Color32::WHITE)
            .size()
            .x
    };
    if measure(text.to_string()) <= max_width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut lo = 1usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let candidate: String = std::iter::once('…')
            .chain(chars[mid..].iter().copied())
            .collect();
        if measure(candidate) <= max_width {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    if lo >= chars.len() {
        "…".to_string()
    } else {
        std::iter::once('…')
            .chain(chars[lo..].iter().copied())
            .collect()
    }
}

/// Truncate a `prefix + path` string to fit within `max_width`.
///
/// Three tiers:
/// 1. Everything fits — return as-is.
/// 2. Path fits alone — truncate prefix from start to fill remaining space.
/// 3. Path overflows — drop prefix, truncate path from start.
pub(crate) fn truncate_host_and_path(
    ui: &egui::Ui,
    prefix: &str,
    path: &str,
    max_width: f32,
) -> String {
    let font = egui::FontId::monospace(10.0);
    let weak_color = ui.visuals().weak_text_color();

    let prefix_width = if prefix.is_empty() {
        0.0
    } else {
        ui.painter()
            .layout_no_wrap(prefix.to_string(), font.clone(), weak_color)
            .size()
            .x
    };

    let path_width = ui
        .painter()
        .layout_no_wrap(path.to_string(), font.clone(), weak_color)
        .size()
        .x;

    if path_width <= max_width - prefix_width {
        format!("{}{}", prefix, path)
    } else if path_width <= max_width {
        let host_budget = max_width - path_width;
        let truncated_prefix = truncate_start(ui, prefix, &font, host_budget);
        format!("{}{}", truncated_prefix, path)
    } else {
        truncate_start(ui, path, &font, max_width)
    }
}

/// Fill a session row's background: the active row, a hovered row, and on top
/// of the active row the chord cursor's wash while normal mode walks the list.
fn paint_row_background(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    is_active: bool,
    chord_cursor: bool,
    response: &egui::Response,
) {
    let fill = if is_active {
        ui.visuals().widgets.active.bg_fill
    } else if response.hovered() {
        ui.visuals().widgets.hovered.weak_bg_fill
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 8.0, fill);
    if is_active && chord_cursor {
        ui.painter()
            .rect_filled(rect, 8.0, super::dave::block_cursor_fill(ui));
    }
}

/// What a session row drew: its response, and what clicking its focus dot
/// asks, if it was clicked.
struct RowResponse {
    /// The row's click response.
    response: egui::Response,
    /// What clicking its focus dot asks, if it was clicked.
    action: Option<SessionListAction>,
}

/// What a session row's right side took, as [`render_row_right_side`] draws it.
struct RightSide {
    /// The total width it consumed.
    width: f32,
    /// What clicking its focus dot asks, if it was clicked.
    action: Option<SessionListAction>,
}

/// Render the right side of a session row: shortcut number, ctrl+shift hints, and focus dot.
#[allow(clippy::too_many_arguments)]
fn render_row_right_side(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    session_id: SessionId,
    is_active: bool,
    ctrl_held: bool,
    shortcut_hint: Option<usize>,
    cycle_hint: Option<&str>,
    queue_priority: Option<FocusPriority>,
    hints: &[(&str, &str)],
) -> RightSide {
    let mut right_offset = 8.0;
    let mut dot_action = None;

    if let Some(num) = shortcut_hint {
        let hint_size = 18.0;
        let hint_text = format!("{}", num);
        let hint_center = rect.right_center() - egui::vec2(8.0 + hint_size / 2.0, 0.0);
        paint_keybind_hint(ui, hint_center, &hint_text, hint_size);
        right_offset = 8.0 + hint_size + 6.0;
    }

    // Ctrl+J / Ctrl+K hint on the rows adjacent to the active session.
    if let Some(cycle) = cycle_hint {
        let hint_size = 18.0;
        let hint_center = rect.right_center() - egui::vec2(right_offset + hint_size / 2.0, 0.0);
        paint_keybind_hint(ui, hint_center, cycle, hint_size);
        right_offset += hint_size + 6.0;
    }

    if is_active && ctrl_held {
        let hint_size = 16.0;
        let hint_width = 26.0;
        let gap = 3.0;

        for (hint_text, tooltip) in hints {
            let center = rect.right_center() - egui::vec2(right_offset + hint_width / 2.0, 0.0);
            KeybindHint::new(hint_text)
                .size(hint_size)
                .width(hint_width)
                .paint_at(ui, center);
            let hint_rect = egui::Rect::from_center_size(center, egui::vec2(hint_width, hint_size));
            ui.interact(
                hint_rect,
                ui.scope_id().with(("keybind_tip", *hint_text)),
                Sense::hover(),
            )
            .on_hover_text(*tooltip);
            right_offset += hint_width + gap;
        }
    }

    if let Some(priority) = queue_priority {
        let dot_radius = 5.0;
        let dot_center = rect.right_center() - egui::vec2(right_offset + dot_radius + 4.0, 0.0);
        ui.painter()
            .circle_filled(dot_center, dot_radius, priority.color());

        if priority == FocusPriority::Done {
            let dot_rect = egui::Rect::from_center_size(
                dot_center,
                egui::vec2(dot_radius * 4.0, dot_radius * 4.0),
            );
            let dot_response = ui.interact(
                dot_rect,
                ui.scope_id().with(("dismiss_dot", session_id)),
                egui::Sense::click(),
            );
            if dot_response.clicked() {
                dot_action = Some(SessionListAction::DismissDone(session_id));
            }
            if dot_response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }

        right_offset += dot_radius * 2.0 + 8.0;
    }

    RightSide {
        width: right_offset,
        action: dot_action,
    }
}

/// What a standalone agent row's right side took, as
/// [`render_agent_row_right_side`] draws it.
struct AgentRightSide {
    /// The width to reserve on the title (top) line.
    title_right: f32,
    /// The width to reserve on the cwd (bottom) line.
    cwd_right: f32,
    /// What clicking its focus dot asks, if it was clicked.
    action: Option<SessionListAction>,
}

/// Render the right side of a standalone (cwd) agent row: the shortcut
/// number and ctrl+shift hints centered, the focus dot in the top-right
/// corner and the worktree's git +/- in the bottom-right one.
#[allow(clippy::too_many_arguments)]
fn render_agent_row_right_side(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    session_id: SessionId,
    is_active: bool,
    ctrl_held: bool,
    shortcut_hint: Option<usize>,
    cycle_hint: Option<&str>,
    queue_priority: Option<FocusPriority>,
    git_lines: Option<GitLines>,
    hints: &[(&str, &str)],
) -> AgentRightSide {
    // The hints, centered, take what a dotless row's right side would.
    let RightSide {
        width: center_offset,
        ..
    } = render_row_right_side(
        ui,
        rect,
        session_id,
        is_active,
        ctrl_held,
        shortcut_hint,
        cycle_hint,
        None,
        hints,
    );

    // Status dot, top-right corner.
    let mut dot_action = None;
    let mut dot_width = 0.0;
    if let Some(priority) = queue_priority {
        let dot_radius = 5.0;
        let dot_center = egui::pos2(rect.right() - 4.0 - dot_radius, rect.top() + 9.0);
        ui.painter()
            .circle_filled(dot_center, dot_radius, priority.color());

        if priority == FocusPriority::Done {
            let dot_rect = egui::Rect::from_center_size(
                dot_center,
                egui::vec2(dot_radius * 4.0, dot_radius * 4.0),
            );
            let dot_response = ui.interact(
                dot_rect,
                ui.scope_id().with(("dismiss_dot", session_id)),
                egui::Sense::click(),
            );
            if dot_response.clicked() {
                dot_action = Some(SessionListAction::DismissDone(session_id));
            }
            if dot_response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }

        dot_width = dot_radius * 2.0 + 8.0;
    }

    // Git +/- line stats, bottom-right corner.
    let git_width = git_lines.map_or(0.0, |lines| {
        let right = egui::pos2(rect.right() - 6.0, rect.bottom() - 9.0);
        paint_git_lines(ui, lines, right) + 10.0
    });

    AgentRightSide {
        title_right: center_offset.max(dot_width),
        cwd_right: center_offset.max(git_width),
        action: dot_action,
    }
}

/// Paint a worktree's +/- lines against HEAD, e.g. `+88 -4`, right-aligned
/// so they end at `right_center` and centered on its y. Returns the width
/// they took.
fn paint_git_lines(ui: &egui::Ui, lines: GitLines, right_center: Pos2) -> f32 {
    let painter = ui.painter();
    let mut x = right_center.x;
    // Laid out right to left, so the deleted count goes first.
    let mut paint = |text: String, color: Color32| {
        if x < right_center.x {
            x -= 5.0;
        }
        let galley = painter.layout_no_wrap(text, cwd_font(), color);
        x -= galley.size().x;
        let pos = egui::pos2(x, right_center.y - galley.size().y / 2.0);
        painter.galley(pos, galley, color);
    };
    if lines.deleted > 0 {
        paint(format!("-{}", lines.deleted), GIT_DELETED_COLOR);
    }
    if lines.added > 0 {
        paint(format!("+{}", lines.added), GIT_ADDED_COLOR);
    }
    right_center.x - x
}

/// The colour of a worktree's added-lines count.
pub(crate) const GIT_ADDED_COLOR: egui::Color32 = egui::Color32::from_rgb(60, 180, 60);
/// The colour of a worktree's deleted-lines count.
pub(crate) const GIT_DELETED_COLOR: egui::Color32 = egui::Color32::from_rgb(200, 60, 60);

/// Render a title string at the given position, clipping if it exceeds max_width.
fn render_title(ui: &mut egui::Ui, title: &str, title_pos: Pos2, max_width: f32) {
    let font_id = egui::FontId::proportional(14.0);
    let text_color = ui.visuals().text_color();
    let galley = ui
        .painter()
        .layout_no_wrap(title.to_string(), font_id.clone(), text_color);
    let title_height = galley.size().y;

    if galley.size().x > max_width {
        let clip_rect = egui::Rect::from_min_size(title_pos, egui::vec2(max_width, title_height));
        ui.painter()
            .with_clip_rect(clip_rect)
            .galley(title_pos, galley, text_color);
    } else {
        ui.painter().text(
            title_pos,
            egui::Align2::LEFT_TOP,
            title,
            font_id,
            text_color,
        );
    }
}

/// Render one host's collapsible section, including its nested cwd sections.
///
/// Like the project and workspace sections, the header is built by hand via the
/// shared [`collapsible_header_ui`] so a host icon can sit on the left and the
/// disclosure arrow on the right. Open/closed is forced from the external
/// `CollapseState` each frame (keeping a single source of truth for keyboard
/// navigation and persistence); `CollapsingState` is kept only for the animated
/// body reveal and the arrow's `openness`.
fn host_section_ui(
    ui: &mut egui::Ui,
    list_ui: &SessionListUi<'_, '_>,
    host_group: &crate::session::HostGroup,
    visual_index: &mut usize,
    active_id: Option<SessionId>,
    cycle: CycleHints,
) -> Option<SessionListAction> {
    let mut action = None;
    let host_label = if host_group.hostname.is_empty() {
        "Local"
    } else {
        host_group.hostname.as_str()
    };
    let host_collapsed = list_ui
        .collapse_state
        .is_host_collapsed(&host_group.hostname);
    let host_id = ui.make_persistent_id(("dave_host_collapse", host_group.hostname.as_str()));
    let mut host_state =
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), host_id, true);
    host_state.set_open(!host_collapsed);
    let openness = host_state.openness(ui.ctx());

    let header = collapsible_header_ui(
        ui,
        HeaderParams {
            text: host_label,
            font: egui::FontId::proportional(14.0),
            color: HeaderColor::Weak,
            icon: HeaderIcon::Host,
            height: 24.0,
            truncate_from_start: false,
            git_lines: None,
        },
        openness,
    );

    host_state.show_body_unindented(ui, |ui| {
        for project in &host_group.project_groups {
            // Single-workspace projects render flush (no slug header) so the
            // common single-checkout case stays uncluttered — "flat". Only
            // multi-worktree projects get a project header grouping their
            // workspaces, which is exactly where per-cwd scatter was the problem.
            let rendered = if project.is_flat() {
                cwd_section_ui(
                    ui,
                    list_ui,
                    &host_group.hostname,
                    &project.cwd_groups[0],
                    visual_index,
                    active_id,
                    cycle,
                )
            } else {
                project_section_ui(
                    ui,
                    list_ui,
                    &host_group.hostname,
                    project,
                    visual_index,
                    active_id,
                    cycle,
                )
            };
            if let Some(a) = rendered {
                action = Some(a);
            }
        }
    });
    host_state.store(ui.ctx());

    // The whole row toggles collapse; the label sits on top and captures clicks
    // over its own rect, so check both.
    if header.clicked() {
        action = Some(SessionListAction::ToggleHostCollapse(
            host_group.hostname.clone(),
        ));
    }

    ui.add_space(6.0);
    action
}

/// Render one multi-workspace project's collapsible section: a slug header with
/// each of its workspaces (worktrees/dirs) beneath. Single-workspace projects
/// never reach here — they render flush (see `host_section_ui`).
///
/// The header is built by hand rather than via `CollapsingState::show_header`
/// so the disclosure arrow can sit on the *right* (egui always paints its
/// triangle on the left). Open/closed state is driven manually from the
/// external `CollapseState`; `CollapsingState` is kept only for the animated
/// body reveal and the arrow's `openness`.
fn project_section_ui(
    ui: &mut egui::Ui,
    list_ui: &SessionListUi<'_, '_>,
    hostname: &str,
    project: &crate::session::ProjectGroup,
    visual_index: &mut usize,
    active_id: Option<SessionId>,
    cycle: CycleHints,
) -> Option<SessionListAction> {
    let mut action = None;
    let collapsed = list_ui
        .collapse_state
        .is_project_collapsed(hostname, &project.root);
    let project_id =
        ui.make_persistent_id(("dave_project_collapse", hostname, project.root.as_path()));
    let mut project_state = egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(),
        project_id,
        true,
    );
    project_state.set_open(!collapsed);
    let openness = project_state.openness(ui.ctx());

    let header = collapsible_header_ui(
        ui,
        HeaderParams {
            text: &project.slug,
            font: egui::FontId::proportional(13.0),
            color: HeaderColor::StrongOnHover,
            icon: HeaderIcon::Folder,
            height: 22.0,
            truncate_from_start: false,
            git_lines: None,
        },
        openness,
    );

    project_state.show_body_unindented(ui, |ui| {
        // Indent the whole workspace level under the project header so the
        // hierarchy (project → workspace) reads clearly. Each workspace then
        // applies the same inline-single / folder-multi rule as a flat project
        // (a lone session renders inline with its cwd path; multiple sessions
        // get a collapsible folder), so `cwd_section_ui` is reused verbatim and
        // `visual_order` mirrors it.
        egui::Frame::new()
            .inner_margin(egui::Margin {
                left: 12,
                ..Default::default()
            })
            .show(ui, |ui| {
                for cwd_group in &project.cwd_groups {
                    if let Some(a) = cwd_section_ui(
                        ui,
                        list_ui,
                        hostname,
                        cwd_group,
                        visual_index,
                        active_id,
                        cycle,
                    ) {
                        action = Some(a);
                    }
                }
            });
    });
    project_state.store(ui.ctx());

    // The whole row toggles collapse; the label sits on top and captures clicks
    // over its own rect, so check both.
    if header.clicked() {
        action = Some(SessionListAction::ToggleProjectCollapse(
            hostname.to_string(),
            project.root.clone(),
        ));
    }

    ui.add_space(4.0);
    action
}

/// Which glyph a collapsible section header paints at its left edge.
///
/// Each variant owns both where its icon sits ([`HeaderIcon::icon_rect`]) and
/// how it's drawn ([`HeaderIcon::paint`]), so [`collapsible_header_ui`] stays
/// agnostic to the specific glyph.
#[derive(Clone, Copy)]
enum HeaderIcon {
    /// A monitor silhouette for a host/machine section.
    Host,
    /// A folder silhouette for a project section.
    Folder,
    /// A git-branch glyph for a workspace (worktree/cwd) section.
    Worktree,
}

impl HeaderIcon {
    /// Left-edge icon rect for this glyph within the header row `rect`.
    fn icon_rect(self, rect: egui::Rect) -> egui::Rect {
        let left = rect.left() + 2.0;
        match self {
            HeaderIcon::Host => {
                let (w, h) = (13.0, 11.0);
                egui::Rect::from_min_size(
                    egui::pos2(left, rect.center().y - h / 2.0),
                    egui::vec2(w, h),
                )
            }
            HeaderIcon::Folder => {
                let s = 13.0;
                egui::Rect::from_min_size(
                    egui::pos2(left, rect.center().y - s * 0.4),
                    egui::vec2(s, s * 0.8),
                )
            }
            HeaderIcon::Worktree => {
                let s = 12.0;
                egui::Rect::from_min_size(
                    egui::pos2(left, rect.center().y - s / 2.0),
                    egui::vec2(s, s),
                )
            }
        }
    }

    /// Paint this glyph inside `rect` using `color`.
    fn paint(self, painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
        match self {
            HeaderIcon::Host => paint_host_icon(painter, rect, color),
            HeaderIcon::Folder => paint_folder_icon(painter, rect, color),
            HeaderIcon::Worktree => paint_worktree_icon(painter, rect, color),
        }
    }
}

/// How a collapsible section header colors its text and icon.
#[derive(Clone, Copy)]
enum HeaderColor {
    /// Weak (dim) text that doesn't change on hover — host and workspace.
    Weak,
    /// Normal text that strengthens on hover — project headers.
    StrongOnHover,
}

impl HeaderColor {
    fn resolve(self, visuals: &egui::Visuals, hovered: bool) -> egui::Color32 {
        match self {
            HeaderColor::Weak => visuals.weak_text_color(),
            HeaderColor::StrongOnHover if hovered => visuals.strong_text_color(),
            HeaderColor::StrongOnHover => visuals.text_color(),
        }
    }
}

/// Parameters for [`collapsible_header_ui`], the one header renderer shared by
/// the host, project, and workspace sections.
struct HeaderParams<'a> {
    /// Header text (host label, project slug, or cwd path).
    text: &'a str,
    /// Font + size for the text.
    font: egui::FontId,
    /// Text/icon color treatment.
    color: HeaderColor,
    /// Glyph painted at the left edge.
    icon: HeaderIcon,
    /// Row height.
    height: f32,
    /// Truncate the text from the *start* (keep the tail) via
    /// [`truncate_host_and_path`], as the workspace cwd path does; otherwise
    /// truncate normally from the end with an ellipsis.
    truncate_from_start: bool,
    /// A worktree's +/- lines, drawn right-aligned before the disclosure
    /// arrow: a cwd folder's, so its rows needn't each repeat them.
    git_lines: Option<GitLines>,
}

/// What [`collapsible_header_ui`] drew.
struct HeaderResponse {
    /// The full-width click row.
    row: egui::Response,
    /// The label on top of it, which captures clicks over its own rect.
    label: egui::Response,
}

impl HeaderResponse {
    /// Whether the header was clicked, on the row or its label.
    fn clicked(&self) -> bool {
        self.row.clicked() || self.label.clicked()
    }
}

/// The one collapsible section header, shared by the host, project, and
/// workspace sections (previously three near-identical hand-built renderers).
///
/// Allocates a full-width click row, paints `params.icon` at the left edge and
/// a disclosure arrow (driven by `openness`) at the right edge, and places
/// `params.text` between them as a real accessible [`egui::Label`] (left-
/// aligned) — so every header is keyboard- and right-click-friendly, not just
/// the workspace one. egui always paints its own triangle on the left, which is
/// why the arrow is drawn by hand on the right and the caller drives open/closed
/// from the external `CollapseState`.
///
/// Callers toggle collapse when [`HeaderResponse::clicked`]: the label sits on
/// top of the row and captures clicks over its own rect.
fn collapsible_header_ui(
    ui: &mut egui::Ui,
    params: HeaderParams<'_>,
    openness: f32,
) -> HeaderResponse {
    let (rect, row_response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), params.height),
        Sense::click(),
    );
    let row_response = row_response.on_hover_cursor(egui::CursorIcon::PointingHand);

    let color = params.color.resolve(ui.visuals(), row_response.hovered());

    // Icon, left edge.
    let icon_rect = params.icon.icon_rect(rect);
    params.icon.paint(ui.painter(), icon_rect, color);

    // Disclosure arrow, right edge (rotates from ▶ closed to ▼ open).
    let arrow_box = 16.0;
    let arrow_center = egui::pos2(rect.right() - 2.0 - arrow_box / 2.0, rect.center().y);
    paint_disclosure_arrow(ui.painter(), arrow_center, openness, color);

    // Git +/- just left of the arrow.
    let mut text_right = arrow_center.x - arrow_box / 2.0 - 4.0;
    if let Some(lines) = params.git_lines {
        let right = egui::pos2(text_right, rect.center().y);
        text_right -= paint_git_lines(ui, lines, right) + 6.0;
    }

    // Header text between the icon and the git +/- (or the arrow), as a real
    // accessible widget so it can be found and right-clicked.
    let text_left = icon_rect.right() + 6.0;
    let label_rect = egui::Rect::from_min_max(
        egui::pos2(text_left, rect.top()),
        egui::pos2(text_right, rect.bottom()),
    );
    let max_text_width = label_rect.width().max(0.0);

    // Workspace paths keep their tail (truncate from the start); host/project
    // text truncates normally from the end with an ellipsis.
    let label = if params.truncate_from_start {
        let text = truncate_host_and_path(ui, "", params.text, max_text_width);
        egui::Label::new(egui::RichText::new(text).font(params.font).color(color))
            .sense(Sense::click())
    } else {
        egui::Label::new(
            egui::RichText::new(params.text)
                .font(params.font)
                .color(color),
        )
        .truncate()
        .sense(Sense::click())
    };

    // Left-align the text within its rect (a plain `ui.put` would center it).
    let mut label_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(label_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let label_response = label_ui
        .add(label)
        .on_hover_cursor(egui::CursorIcon::PointingHand);

    HeaderResponse {
        row: row_response,
        label: label_response,
    }
}

/// Paint a small monitor silhouette inside `rect` (a screen over a stand),
/// reading as a "host / machine" rather than a folder or worktree.
fn paint_host_icon(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.3, color);

    // Screen: a rounded rect outline occupying the top portion of the icon.
    let screen = egui::Rect::from_min_max(
        rect.left_top(),
        egui::pos2(rect.right(), rect.top() + rect.height() * 0.72),
    );
    painter.rect_stroke(screen, 1.5, stroke, egui::StrokeKind::Inside);

    // Stand: a short neck down from the screen to a wider base foot.
    let cx = rect.center().x;
    let base_y = rect.bottom();
    painter.line_segment(
        [egui::pos2(cx, screen.bottom()), egui::pos2(cx, base_y)],
        stroke,
    );
    let foot_half = rect.width() * 0.28;
    painter.line_segment(
        [
            egui::pos2(cx - foot_half, base_y),
            egui::pos2(cx + foot_half, base_y),
        ],
        stroke,
    );
}

/// Paint a small filled folder silhouette (a tab over a body) inside `rect`.
fn paint_folder_icon(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let rounding = 1.5;
    let tab_h = rect.height() * 0.28;
    let tab =
        egui::Rect::from_min_size(rect.left_top(), egui::vec2(rect.width() * 0.5, tab_h * 2.0));
    let body = egui::Rect::from_min_max(
        egui::pos2(rect.left(), rect.top() + tab_h),
        rect.right_bottom(),
    );
    painter.rect_filled(tab, rounding, color);
    painter.rect_filled(body, rounding, color);
}

/// Paint a disclosure triangle centered at `center`, rotating from pointing
/// right (`openness` 0, collapsed) to pointing down (`openness` 1, expanded).
fn paint_disclosure_arrow(painter: &egui::Painter, center: Pos2, openness: f32, color: Color32) {
    let r = 4.0;
    let angle = openness * std::f32::consts::FRAC_PI_2;
    let (sin, cos) = angle.sin_cos();
    let rot =
        |x: f32, y: f32| egui::pos2(center.x + x * cos - y * sin, center.y + x * sin + y * cos);
    let points = vec![rot(r, 0.0), rot(-r * 0.7, -r * 0.9), rot(-r * 0.7, r * 0.9)];
    painter.add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
}

/// Render one (host, cwd) section, including its session rows.
///
/// When the cwd has a single session, the folder header is suppressed and the
/// session is rendered inline with its cwd path embedded in the row. When the
/// cwd has multiple sessions, a collapsible folder wraps them.
fn cwd_section_ui(
    ui: &mut egui::Ui,
    list_ui: &SessionListUi<'_, '_>,
    hostname: &str,
    cwd_group: &crate::session::CwdGroup,
    visual_index: &mut usize,
    active_id: Option<SessionId>,
    cycle: CycleHints,
) -> Option<SessionListAction> {
    let mut action = None;

    // Single-session cwd: render the row inline with the cwd path, no folder.
    if cwd_group.session_ids.len() == 1 {
        let id = cwd_group.session_ids[0];
        if let Some(session) = list_ui.session_manager.get(id) {
            if let Some(a) = list_ui.render_session_item(
                ui,
                session,
                *visual_index,
                active_id,
                RowPlace {
                    cwd: Some(cwd_group.display_cwd.as_str()),
                    own_bar: true,
                },
                cycle,
            ) {
                action = Some(a);
            }
            *visual_index += 1;
        }
        return action;
    }

    // Multi-session cwd: the folder sits flush with the rows around it, like
    // a successor chain; only a spawned child's row is nested
    // (`NEST_INDENT`).
    action = cwd_folder_ui(
        ui,
        list_ui,
        hostname,
        cwd_group,
        visual_index,
        active_id,
        cycle,
    );

    ui.add_space(2.0);
    action
}

/// The +/- lines of `group`'s worktree, for its folder header. Every session
/// in the folder runs in that worktree, so any one's git status is its
/// status; a session running elsewhere ([`ListRow::elsewhere`](crate::session::ListRow::elsewhere))
/// is in another worktree, and keeps its own +/- on its row.
fn folder_git_lines(
    session_manager: &SessionManager,
    group: &crate::session::CwdGroup,
) -> Option<GitLines> {
    group
        .session_ids
        .iter()
        .zip(&group.rows)
        .filter(|(_, row)| row.elsewhere.is_none())
        .find_map(|(&id, _)| git_lines(session_manager.get(id)?))
}

/// Render the collapsible folder for a multi-session cwd: header, body
/// containing each session row, and the right-click "New Session" menu.
///
/// Like `project_section_ui`, the header is built by hand rather than via
/// `CollapsingState::show_header` so the disclosure arrow can sit on the
/// *right* (egui always paints its triangle on the left) with a worktree icon
/// on the left. Open/closed state is driven manually from the external
/// `CollapseState`; `CollapsingState` is kept only for the animated body reveal
/// and the arrow's `openness`.
fn cwd_folder_ui(
    ui: &mut egui::Ui,
    list_ui: &SessionListUi<'_, '_>,
    hostname: &str,
    cwd_group: &crate::session::CwdGroup,
    visual_index: &mut usize,
    active_id: Option<SessionId>,
    cycle: CycleHints,
) -> Option<SessionListAction> {
    let mut action = None;
    let cwd_collapsed = list_ui
        .collapse_state
        .is_cwd_collapsed(hostname, &cwd_group.cwd);
    let cwd_id = ui.make_persistent_id(("dave_cwd_collapse", hostname, cwd_group.cwd.as_path()));
    let mut cwd_state =
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), cwd_id, true);
    cwd_state.set_open(!cwd_collapsed);
    let openness = cwd_state.openness(ui.ctx());

    let header = collapsible_header_ui(
        ui,
        HeaderParams {
            text: &cwd_group.display_cwd,
            font: egui::FontId::monospace(10.0),
            color: HeaderColor::Weak,
            icon: HeaderIcon::Worktree,
            height: 20.0,
            truncate_from_start: true,
            git_lines: folder_git_lines(list_ui.session_manager, cwd_group),
        },
        openness,
    );

    cwd_state.show_body_unindented(ui, |ui| {
        ui.add_space(2.0);
        if let Some(a) = workspace_rows_ui(
            ui,
            list_ui,
            cwd_group,
            &mut RowWalk {
                visual_index,
                active_id,
                cycle,
            },
        ) {
            action = Some(a);
        }
    });
    cwd_state.store(ui.ctx());

    // The whole row toggles collapse; the label sits on top and captures clicks
    // over its own rect, so check both.
    if header.clicked() {
        action = Some(SessionListAction::ToggleCwdCollapse(
            hostname.to_string(),
            cwd_group.cwd.clone(),
        ));
    }

    notedeck_ui::context_menu::context_menu(&header.label, |ui| {
        if ui.button("New Session").clicked() {
            action = Some(SessionListAction::NewSessionInCwd(
                hostname.to_string(),
                cwd_group.cwd.clone(),
            ));
            ui.close();
        }
    });

    action
}

/// What every row of one folder is drawn with besides its session, carried
/// down the lineage walk.
struct RowWalk<'v> {
    /// The next drawn session's place in visual order (its Ctrl+<n> hint).
    visual_index: &'v mut usize,
    active_id: Option<SessionId>,
    cycle: CycleHints,
}

/// Left indent of each lineage level of a nested child row.
const NEST_INDENT: f32 = 14.0;

/// Draw a workspace's rows in [`CwdGroup::session_ids`](crate::session::CwdGroup::session_ids)
/// order, which `visual_order` and Ctrl+J / K walk, so the drawn order and the
/// keyboard's cannot disagree: a successor chain as its block (see
/// [`chain_ui`]), any other session as its own row, followed by its
/// children nested under it (see [`lineage_row_ui`]).
fn workspace_rows_ui(
    ui: &mut egui::Ui,
    list_ui: &SessionListUi<'_, '_>,
    group: &crate::session::CwdGroup,
    walk: &mut RowWalk<'_>,
) -> Option<SessionListAction> {
    let mut action = None;
    let mut start = 0;
    while start < group.session_ids.len() {
        if let Some(chain) = group.chain_at(start) {
            let a = chain_ui(ui, list_ui, group, chain, walk);
            action = a.or(action);
            start = chain.end;
            continue;
        }
        let a = lineage_row_ui(
            ui,
            list_ui,
            group.session_ids[start],
            &group.rows[start],
            walk,
        );
        action = a.or(action);
        start += 1;
    }
    action
}

/// Draw session `id` where `row` places it: a child indented under the
/// session that spawned it, and a chain link that has handed its work on dimmed. A
/// session running outside the folder's workspace says where on its row.
fn lineage_row_ui(
    ui: &mut egui::Ui,
    list_ui: &SessionListUi<'_, '_>,
    id: SessionId,
    row: &crate::session::ListRow,
    walk: &mut RowWalk<'_>,
) -> Option<SessionListAction> {
    let session = list_ui.session_manager.get(id)?;
    let place = RowPlace {
        cwd: row.elsewhere.as_deref(),
        own_bar: !matches!(row.place.nesting, Nesting::Link { .. }),
    };
    let indent = NEST_INDENT * f32::from(row.place.depth);
    let dim = row.place.nesting == (Nesting::Link { handed_on: true });
    let index = *walk.visual_index;
    *walk.visual_index += 1;
    let RowWalk {
        active_id, cycle, ..
    } = *walk;
    let draw = |ui: &mut egui::Ui| {
        if dim {
            ui.multiply_opacity(0.55);
        }
        list_ui.render_session_item(ui, session, index, active_id, place, cycle)
    };
    if indent == 0.0 && !dim {
        return draw(ui);
    }
    egui::Frame::new()
        .inner_margin(egui::Margin {
            left: indent.min(i8::MAX as f32) as i8,
            ..Default::default()
        })
        .show(ui, draw)
        .inner
}

/// Draw a successor chain: its links in order, flush with the rows around
/// them and sharing one status bar (see [`ChainBar`]), which marks them as
/// one block, each with what nests under it. Links that handed their work on
/// are dimmed; their stretch of the bar too, unless the link still wants
/// attention (see [`AgentStatus::is_quiet`]).
fn chain_ui(
    ui: &mut egui::Ui,
    list_ui: &SessionListUi<'_, '_>,
    group: &crate::session::CwdGroup,
    chain: &crate::session::ChainSpan,
    walk: &mut RowWalk<'_>,
) -> Option<SessionListAction> {
    let mut action = None;
    let x = ui.cursor().left() + BAR_LEFT;
    let gap = ui.spacing().item_spacing.y;
    let mut bar = ChainBar {
        x,
        open: None,
        pending: None,
    };
    for (&id, row) in group.session_ids[chain.start..chain.end]
        .iter()
        .zip(&group.rows[chain.start..chain.end])
    {
        let top = ui.cursor().top();
        if let Nesting::Link { handed_on } = row.place.nesting {
            if let Some(session) = list_ui.session_manager.get(id) {
                let status = session.status();
                let color = row_status_color(status, session.last_activity);
                // A handed-on link still asking for attention keeps its color.
                let color = if handed_on && status.is_quiet() {
                    color.gamma_multiply(0.55)
                } else {
                    color
                };
                bar.link(ui.painter(), top - gap / 2.0, color);
            }
        }
        if let Some(a) = lineage_row_ui(ui, list_ui, id, row, walk) {
            action = Some(a);
        }
    }
    bar.close(ui.painter(), ui.cursor().top() - gap);
    ui.add_space(2.0);
    action
}

/// How tall the blend between two links' colours is on a chain's merged
/// status bar, centred on the boundary between their rows.
const CHAIN_BAR_BLEND: f32 = 10.0;

/// A chain's links' status bars, painted as one bar at `x` while the links
/// are drawn: each link's colour along its own stretch, with a
/// [`CHAIN_BAR_BLEND`]-tall blend where one link's stretch meets the next.
///
/// Each boundary is painted one link late, once the row under it has been
/// drawn, so that row's hover or active background can't cover the blend.
struct ChainBar {
    /// The bar's left edge.
    x: f32,
    /// The stretch still being drawn: where it starts and its link's colour.
    open: Option<BarStretch>,
    /// The latest link's start, painted when the next link starts or the
    /// bar closes.
    pending: Option<BarStretch>,
}

/// One link's stretch of a [`ChainBar`], from `top` in `color`.
#[derive(Clone, Copy)]
struct BarStretch {
    top: f32,
    color: egui::Color32,
}

impl ChainBar {
    /// Start the next link's stretch at `edge`, the boundary between its row
    /// and the one above. Call it before drawing the link's row.
    fn link(&mut self, painter: &egui::Painter, edge: f32, color: egui::Color32) {
        self.flush(painter);
        self.pending = Some(BarStretch { top: edge, color });
    }

    /// Paint up to the pending link's start, now that its row is drawn: the
    /// first link opens the bar with a rounded end, a later one ends the
    /// stretch above it in a blend.
    fn flush(&mut self, painter: &egui::Painter) {
        let Some(BarStretch { top: edge, color }) = self.pending.take() else {
            return;
        };
        let Some(open) = self.open else {
            let top = edge + BAR_INSET;
            self.cap(painter, top, color, CapEnd::Top);
            self.open = Some(BarStretch {
                top: top + STATUS_BAR_WIDTH / 2.0,
                color,
            });
            return;
        };
        let half = CHAIN_BAR_BLEND / 2.0;
        self.band(painter, open.top, open.color, edge - half, open.color);
        self.band(painter, edge - half, open.color, edge + half, color);
        self.open = Some(BarStretch {
            top: edge + half,
            color,
        });
    }

    /// End the bar at `bottom`, the last row's bottom edge.
    fn close(&mut self, painter: &egui::Painter, bottom: f32) {
        self.flush(painter);
        let Some(open) = self.open.take() else {
            return;
        };
        let end = bottom - BAR_INSET - STATUS_BAR_WIDTH / 2.0;
        self.band(painter, open.top, open.color, end, open.color);
        self.cap(painter, end, open.color, CapEnd::Bottom);
    }

    /// Paint the bar from `y0` in `c0` to `y1` in `c1`, blending between.
    fn band(
        &self,
        painter: &egui::Painter,
        y0: f32,
        c0: egui::Color32,
        y1: f32,
        c1: egui::Color32,
    ) {
        if y1 <= y0 {
            return;
        }
        let right = self.x + STATUS_BAR_WIDTH;
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(egui::pos2(self.x, y0), c0);
        mesh.colored_vertex(egui::pos2(right, y0), c0);
        mesh.colored_vertex(egui::pos2(self.x, y1), c1);
        mesh.colored_vertex(egui::pos2(right, y1), c1);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(1, 3, 2);
        painter.add(mesh);
    }

    /// Paint the bar's rounded `end`, half a bar width tall, from `y`.
    fn cap(&self, painter: &egui::Painter, y: f32, color: egui::Color32, end: CapEnd) {
        let r = STATUS_BAR_WIDTH / 2.0;
        let round = (r.round() as u8).max(1);
        let corners = match end {
            CapEnd::Top => egui::CornerRadius {
                nw: round,
                ne: round,
                ..Default::default()
            },
            CapEnd::Bottom => egui::CornerRadius {
                sw: round,
                se: round,
                ..Default::default()
            },
        };
        let rect =
            egui::Rect::from_min_size(egui::pos2(self.x, y), egui::vec2(STATUS_BAR_WIDTH, r));
        painter.rect_filled(rect, corners, color);
    }
}

/// Which end of a [`ChainBar`] a cap rounds.
#[derive(Clone, Copy)]
enum CapEnd {
    Top,
    Bottom,
}

/// Paint a small git-branch (worktree) glyph inside `rect`: a trunk with two
/// nodes and a third node forked off to the right, reading as a branch/worktree
/// rather than a plain folder.
fn paint_worktree_icon(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let node_r = rect.height() * 0.16;
    let stroke = egui::Stroke::new(1.3, color);

    // Trunk on the left: a node at top and bottom joined by a vertical line.
    let trunk_x = rect.left() + node_r + 1.0;
    let top = egui::pos2(trunk_x, rect.top() + node_r);
    let bottom = egui::pos2(trunk_x, rect.bottom() - node_r);
    painter.line_segment([top, bottom], stroke);

    // Branch node forked off to the upper right, connected back to the trunk.
    let branch = egui::pos2(rect.right() - node_r - 1.0, rect.top() + node_r);
    let fork = egui::pos2(trunk_x, rect.center().y);
    painter.line_segment([branch, fork], stroke);

    painter.circle_filled(top, node_r, color);
    painter.circle_filled(bottom, node_r, color);
    painter.circle_filled(branch, node_r, color);
}

/// Draw the cwd path inline within an agent row (monospace 10pt, weak color,
/// truncated from the start), for sessions that are alone in their cwd and so
/// don't get a surrounding folder header.
fn cwd_inline_ui(ui: &mut egui::Ui, cwd_display: &str, pos: Pos2, max_width: f32) {
    let text = truncate_host_and_path(ui, "", cwd_display, max_width);
    let cwd_font = cwd_font();
    let cwd_color = ui.visuals().weak_text_color();
    ui.painter()
        .text(pos, egui::Align2::LEFT_TOP, &text, cwd_font, cwd_color);
}

/// Renders the "Delete worktree" context-menu item with an inline confirmation step.
///
/// First click shows "Delete this worktree? / Cancel / Delete".
/// Confirmation state is stored in egui temp storage keyed per session.
fn delete_worktree_menu_item(
    ui: &mut egui::Ui,
    session_id: SessionId,
    confirm_id: egui::Id,
) -> Option<SessionListAction> {
    let confirming: bool = ui.ctx().data(|d| d.get_temp(confirm_id).unwrap_or(false));

    let mut action = None;

    if confirming {
        ui.separator();
        ui.label("Delete this worktree?");
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                ui.ctx().data_mut(|d| d.insert_temp(confirm_id, false));
                ui.close();
            }
            if ui.button("Delete").clicked() {
                action = Some(SessionListAction::DeleteWorktree(session_id));
                ui.ctx().data_mut(|d| d.insert_temp(confirm_id, false));
                ui.close();
            }
        });
    } else if ui.button("Delete worktree").clicked() {
        ui.ctx().data_mut(|d| d.insert_temp(confirm_id, true));
    }

    action
}

#[cfg(test)]
mod tests {
    use super::{folder_git_lines, AgentRow, GitLines, SessionListAction, SessionListUi};
    use crate::agent_status::AgentStatus;
    use crate::backend::BackendType;
    use crate::collapse_state::CollapseState;
    use crate::config::AiMode;
    use crate::focus_queue::{FocusPriority, FocusQueue};
    use crate::session::SessionId;
    use crate::session::SessionManager;
    use crate::test_refs::{ingest_card, StubCardParser, StubCardRenderer};
    use egui::Event;
    use egui_kittest::{
        kittest::{NodeT, Queryable},
        Harness,
    };
    use std::path::PathBuf;

    struct SessionListHarnessState {
        ui: SessionListUiOwnedState,
        action: Option<SessionListAction>,
    }

    struct SessionListUiOwnedState {
        session_manager: SessionManager,
        focus_queue: FocusQueue,
        collapse_state: CollapseState,
        ctrl_held: bool,
        cwd_label: String,
        cwd_path: PathBuf,
    }

    impl SessionListUiOwnedState {
        fn new() -> Self {
            let cwd_path = PathBuf::from("/tmp/project");
            let mut session_manager = SessionManager::new();
            // Two sessions in the same cwd so the folder header is rendered
            // (single-session cwds skip the header and inline the cwd path).
            for title in ["Workspace agent A", "Workspace agent B"] {
                let session_id = session_manager.new_session(
                    cwd_path.clone(),
                    AiMode::Agentic,
                    BackendType::Claude,
                );
                let session = session_manager
                    .get_mut(session_id)
                    .expect("session should exist");
                session.details.hostname = "host-a".to_string();
                session.details.title = title.to_string();
                session.details.custom_title = None;
                session.details.home_dir = String::new();
            }
            session_manager.rebuild_groups();

            let cwd_label = session_manager.host_groups()[0].project_groups[0].cwd_groups[0]
                .display_cwd
                .clone();

            Self {
                session_manager,
                focus_queue: FocusQueue::new(),
                collapse_state: CollapseState::new(),
                ctrl_held: false,
                cwd_label,
                cwd_path,
            }
        }
    }

    #[test]
    fn right_clicking_cwd_header_can_start_a_new_session_in_that_cwd() {
        let mut harness = Harness::new_ui_state(
            |ui, state: &mut SessionListHarnessState| {
                let session_list = SessionListUi::new(
                    &mut state.ui.session_manager,
                    &state.ui.focus_queue,
                    &state.ui.collapse_state,
                    state.ui.ctrl_held,
                );
                let mut session_list = session_list;
                if let Some(action) = session_list.ui(ui) {
                    state.action = Some(action);
                }
            },
            SessionListHarnessState {
                ui: SessionListUiOwnedState::new(),
                action: None,
            },
        );

        harness.run();

        let header = harness.get_by_label(harness.state().ui.cwd_label.as_str());
        let bounds = header
            .accesskit_node()
            .raw_bounds()
            .expect("cwd header bounds");
        let center = egui::pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        );
        harness.input_mut().events.push(Event::PointerMoved(center));
        harness.input_mut().events.push(Event::PointerButton {
            pos: center,
            button: egui::PointerButton::Secondary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        harness.input_mut().events.push(Event::PointerButton {
            pos: center,
            button: egui::PointerButton::Secondary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        // The menu's first frame is an invisible sizing pass; settle so its
        // items are laid out and reachable.
        harness.run();

        harness.get_by_label("New Session").click_accesskit();
        harness.run();

        match harness.state().action.as_ref() {
            Some(SessionListAction::NewSessionInCwd(host, cwd)) => {
                assert_eq!(host, "host-a");
                assert_eq!(cwd, &harness.state().ui.cwd_path);
            }
            other => panic!("expected NewSessionInCwd action, got {:?}", other),
        }
    }

    /// A keyboard move (normal mode's G, Ctrl+J/K, dd's neighbour) can select
    /// a session scrolled out of the list; the list follows it there. The rows
    /// are painted, not labelled, so the cwd folder headers between them stand
    /// in as markers of where the list has scrolled.
    #[test]
    fn the_list_scrolls_to_a_session_selected_off_screen() {
        const HEIGHT: f32 = 300.0;
        let mut session_manager = SessionManager::new();
        // Two sessions per cwd, so each cwd gets a labelled folder header.
        for n in 0..12 {
            let cwd = PathBuf::from(format!("/tmp/scroll-{n:02}"));
            for _ in 0..2 {
                let id =
                    session_manager.new_session(cwd.clone(), AiMode::Agentic, BackendType::Claude);
                let session = session_manager.get_mut(id).unwrap();
                session.details.hostname = "host-a".to_string();
                session.details.home_dir = String::new();
            }
        }
        session_manager.rebuild_groups();
        let headers: Vec<String> = session_manager.host_groups()[0]
            .project_groups
            .iter()
            .flat_map(|project| project.cwd_groups.iter())
            .map(|cwd| cwd.display_cwd.clone())
            .collect();
        let collapse_state = CollapseState::new();
        let rows = session_manager.visual_order(&collapse_state);
        session_manager.switch_to(rows[0]);

        let mut harness = Harness::builder()
            .with_size(egui::vec2(300.0, HEIGHT))
            .build_ui_state(
                |ui, state: &mut SessionListUiOwnedState| {
                    SessionListUi::new(
                        &mut state.session_manager,
                        &state.focus_queue,
                        &state.collapse_state,
                        false,
                    )
                    .ui(ui);
                },
                SessionListUiOwnedState {
                    session_manager,
                    focus_queue: FocusQueue::new(),
                    collapse_state,
                    ctrl_held: false,
                    cwd_label: String::new(),
                    cwd_path: PathBuf::new(),
                },
            );
        let top_of = |harness: &Harness<'_, SessionListUiOwnedState>, label: &str| {
            let bounds = harness
                .get_by_label(label)
                .accesskit_node()
                .raw_bounds()
                .expect("header bounds");
            bounds.y0 as f32
        };
        let last_header = headers.last().unwrap().as_str();

        harness.run();
        assert!(
            top_of(&harness, last_header) > HEIGHT,
            "the last folder starts out below the fold"
        );

        let last_row = *rows.last().unwrap();
        harness.state_mut().session_manager.switch_to(last_row);
        harness.run();
        let top = top_of(&harness, last_header);
        assert!(
            (0.0..HEIGHT).contains(&top),
            "the list scrolled the selected row into view (header at {top})"
        );

        // And back up, as gg does.
        let first_header = headers.first().unwrap().as_str();
        assert!(
            top_of(&harness, first_header) < 0.0,
            "scrolled past the top"
        );
        harness.state_mut().session_manager.switch_to(rows[0]);
        harness.run();
        let top = top_of(&harness, first_header);
        assert!(
            (0.0..HEIGHT).contains(&top),
            "the list scrolled back up to the first row (header at {top})"
        );
    }

    /// A cwd folder's header carries its worktree's +/- lines, taken from
    /// any session in it, and a session running elsewhere doesn't lend its
    /// own.
    #[test]
    fn a_folder_header_shows_its_worktrees_git_lines() {
        let mut sm = SessionManager::new();
        let mut add = |cwd: &str, lines: Option<GitLines>| {
            let id = sm.new_session(PathBuf::from(cwd), AiMode::Agentic, BackendType::Claude);
            if let Some(l) = lines {
                let s = sm.get_mut(id).expect("session");
                let git = &mut s.agentic.as_mut().expect("agentic").git_status;
                git.set_lines(l.added, l.deleted);
            }
            id
        };
        let lines = GitLines {
            added: 9,
            deleted: 2,
        };
        // The first session's status hasn't been fetched yet.
        add("/tmp/wt", None);
        add("/tmp/wt", Some(lines));
        sm.rebuild_groups();
        let group = sm
            .host_groups()
            .iter()
            .flat_map(|h| &h.project_groups)
            .flat_map(|p| &p.cwd_groups)
            .find(|g| g.session_ids.len() == 2)
            .expect("a two-session folder")
            .clone();
        assert!(folder_git_lines(&sm, &group) == Some(lines));
    }

    /// Render a single standalone (cwd) agentic row in isolation so the
    /// worktree layout — status dot top-right, git +/- bottom-right — can be
    /// verified visually via snapshot.
    fn agent_row_harness(
        priority: Option<FocusPriority>,
        git_lines: Option<GitLines>,
    ) -> Harness<'static> {
        notedeck::test_harness::with_notedeck_style(
            Harness::builder()
                .with_size(egui::Vec2::new(260.0, 56.0))
                .renderer(notedeck::software_renderer())
                .build_ui(move |ui| {
                    let mut session_manager = SessionManager::new();
                    let id = session_manager.new_session(
                        PathBuf::from("/tmp/project"),
                        AiMode::Agentic,
                        BackendType::Claude,
                    );
                    let focus_queue = FocusQueue::new();
                    let collapse_state = CollapseState::new();
                    let list = SessionListUi::new(
                        &mut session_manager,
                        &focus_queue,
                        &collapse_state,
                        false,
                    );
                    list.agent_row_ui(
                        ui,
                        AgentRow {
                            cwd_display: Some("~/src/notedeck-wt"),
                            queue_priority: priority,
                            git_lines,
                            ..test_row(id, "refactor auth module", AgentStatus::NeedsInput)
                        },
                    );
                }),
        )
    }

    /// An [`AgentRow`] for `id` titled `title`, with nothing else to show.
    fn test_row(id: SessionId, title: &str, status: AgentStatus) -> AgentRow<'_> {
        AgentRow {
            session_id: id,
            title,
            cwd_display: None,
            is_active: false,
            shortcut_hint: None,
            cycle_hint: None,
            status,
            last_activity: None,
            queue_priority: None,
            git_lines: None,
            own_bar: true,
        }
    }

    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_agent_row_git_status_and_dot() {
        let mut harness = agent_row_harness(
            Some(FocusPriority::NeedsInput),
            Some(GitLines {
                added: 142,
                deleted: 37,
            }),
        );
        harness.run();
        harness.snapshot("agent_row_git_status_and_dot");
    }

    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_agent_row_git_status_additions_only() {
        let mut harness = agent_row_harness(
            None,
            Some(GitLines {
                added: 88,
                deleted: 0,
            }),
        );
        harness.run();
        harness.snapshot("agent_row_git_status_additions_only");
    }

    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_agent_row_clean() {
        let mut harness = agent_row_harness(None, None);
        harness.run();
        harness.snapshot("agent_row_clean");
    }

    /// Render a grouped agentic row with a given status/title in isolation, so the
    /// remote-resume "Connecting…" placeholder (spinner + muted title) can be
    /// compared against the live row it upgrades into via snapshot.
    fn status_row_harness(status: AgentStatus, title: &'static str) -> Harness<'static> {
        notedeck::test_harness::with_notedeck_style(
            Harness::builder()
                .with_size(egui::Vec2::new(260.0, 48.0))
                .renderer(notedeck::software_renderer())
                .build_ui(move |ui| {
                    let mut session_manager = SessionManager::new();
                    let id = session_manager.new_session(
                        PathBuf::from("/tmp/project"),
                        AiMode::Agentic,
                        BackendType::Claude,
                    );
                    let focus_queue = FocusQueue::new();
                    let collapse_state = CollapseState::new();
                    let list = SessionListUi::new(
                        &mut session_manager,
                        &focus_queue,
                        &collapse_state,
                        false,
                    );
                    list.agent_row_ui(ui, test_row(id, title, status));
                }),
        )
    }

    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_connecting_placeholder_row() {
        // A single step (not run()) since the spinner self-schedules a repaint
        // every interval and run() would loop to its step cap.
        let mut harness = status_row_harness(AgentStatus::Pending, "Connecting...");
        harness.step();
        harness.snapshot("session_row_connecting");
    }

    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_revived_session_row() {
        let mut harness = status_row_harness(AgentStatus::Idle, "Dead Session");
        harness.run();
        harness.snapshot("session_row_revived");
    }

    /// Render the whole sidebar with a flat single-checkout project and a
    /// multi-worktree project side by side, so the new project grouping — one
    /// "notedeck" header wrapping its worktrees, versus the flush single project
    /// — can be verified visually.
    fn project_grouping_harness() -> Harness<'static> {
        notedeck::test_harness::with_notedeck_style(
            Harness::builder()
                .with_size(egui::Vec2::new(300.0, 420.0))
                .renderer(notedeck::software_renderer())
                .build_ui(move |ui| {
                    let mut sm = SessionManager::new();

                    // A flat, single-checkout project (renders flush, no header).
                    let solo = sm.new_session(
                        PathBuf::from("/home/dev/scratch"),
                        AiMode::Agentic,
                        BackendType::Claude,
                    );

                    // A multi-worktree "notedeck" project: main checkout + two
                    // worktrees, all sharing one repo root. The dave worktree,
                    // which holds two sessions, is dirty: its folder header shows
                    // the +/- once, and its rows show none.
                    let wt = |sm: &mut SessionManager, cwd: &str, title: &str, lines: GitLines| {
                        let id = sm.new_session(
                            PathBuf::from(cwd),
                            AiMode::Agentic,
                            BackendType::Claude,
                        );
                        let s = sm.get_mut(id).expect("session");
                        s.details.title = title.to_string();
                        s.details.home_dir = "/home/dev".to_string();
                        s.details.project_root = Some(PathBuf::from("/home/dev/notedeck"));
                        s.details.project_slug = Some("notedeck".to_string());
                        let git = &mut s.agentic.as_mut().expect("agentic").git_status;
                        git.set_lines(lines.added, lines.deleted);
                        id
                    };
                    let clean = GitLines {
                        added: 0,
                        deleted: 0,
                    };
                    let dave = GitLines {
                        added: 142,
                        deleted: 37,
                    };
                    wt(&mut sm, "/home/dev/notedeck", "fix streaming bug", clean);
                    wt(&mut sm, "/home/dev/notedeck-dave", "project grouping", dave);
                    wt(
                        &mut sm,
                        "/home/dev/notedeck-dave",
                        "render tool results",
                        dave,
                    );
                    let feature = GitLines {
                        added: 12,
                        deleted: 0,
                    };
                    wt(
                        &mut sm,
                        "/home/dev/notedeck-feature",
                        "responsive layout",
                        feature,
                    );

                    if let Some(s) = sm.get_mut(solo) {
                        s.details.title = "one-off".to_string();
                        s.details.home_dir = "/home/dev".to_string();
                    }
                    sm.rebuild_groups();

                    let focus_queue = FocusQueue::new();
                    let collapse_state = CollapseState::new();
                    let mut list =
                        SessionListUi::new(&mut sm, &focus_queue, &collapse_state, false);
                    list.ui(ui);
                }),
        )
    }

    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_project_grouping() {
        let mut harness = project_grouping_harness();
        harness.run();
        harness.snapshot("session_list_project_grouping");
    }

    /// Card refs and their titles for the card-row list: long gamedev titles
    /// in the `CODE: headline — detail` shape, one without a code.
    const CARDS: [(&str, &str); 5] = [
        (
            "headway:game/able-bench-coil",
            "UT7c: the key list, bindings, Escape menu, settings and console drawn from the atlas — crews-keys-2, bindings-1",
        ),
        (
            "headway:game/crane-drift-oak",
            "UT7e: the talent window drawn from the atlas — crews-talents-2",
        ),
        (
            "headway:game/dune-ember-fig",
            "QA2: the Crownfolk's town and givers stood — stamps on homeland roles, quest givers placed",
        ),
        (
            "headway:game/gale-hush-iris",
            "Crag 1.6: the cliff relief modelled — a row of ZONES carries its levels",
        ),
        (
            "headway:game/jade-knot-lark",
            "a morphing prop's foot worked out once: the capital films ~10x cheaper",
        ),
    ];

    /// A [`SessionManager`] with a session per [`CARDS`] card in one cwd,
    /// titled with its ref, the QA2 card with a second session, and its card
    /// titles looked up already.
    fn card_sessions() -> SessionManager {
        let now = crate::session::now_unix();
        let mut sm = SessionManager::new();
        let mut add = |card_ref: &str, status: AgentStatus, mins: u64| {
            let id = sm.new_session(
                PathBuf::from("/home/dev/crownfolk"),
                AiMode::Agentic,
                BackendType::Claude,
            );
            let s = sm.get_mut(id).expect("session");
            s.details.title = card_ref.to_string();
            s.details.home_dir = "/home/dev".to_string();
            s.last_activity = Some(now - mins * 60);
            // Observed remotely, so the status is the one its state reports.
            s.source = crate::session::SessionSource::Remote;
            s.agentic.as_mut().expect("agentic").remote_status = Some(status);
            s.update_status();
            id
        };
        add(CARDS[0].0, AgentStatus::NeedsInput, 3);
        add(CARDS[1].0, AgentStatus::Working, 12);
        add(CARDS[2].0, AgentStatus::Working, 8);
        add(CARDS[2].0, AgentStatus::Idle, 65);
        add(CARDS[3].0, AgentStatus::Idle, 120);
        add(CARDS[4].0, AgentStatus::Idle, 180);
        for (card_ref, title) in CARDS {
            sm.card_titles
                .store(card_ref, crate::card_titles::CardLookup::titled(title), 0.0);
        }
        sm.rebuild_groups();
        sm
    }

    /// The whole list as card-titled rows, the two QA2 sessions on a row
    /// each. Drawn without title refs, so each row is its card's raw ref.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_card_titles() {
        let mut harness = notedeck::test_harness::with_notedeck_style(
            Harness::builder()
                .with_size(egui::Vec2::new(300.0, 520.0))
                .renderer(notedeck::software_renderer())
                .build_ui_state(
                    |ui, sm: &mut SessionManager| {
                        let focus_queue = FocusQueue::new();
                        let collapse_state = CollapseState::new();
                        SessionListUi::new(sm, &focus_queue, &collapse_state, false).ui(ui);
                    },
                    card_sessions(),
                ),
        );
        harness.run();
        harness.snapshot("session_list_card_titles");
    }

    /// Two sessions on one card sort next to each other, and each still has
    /// a row of its own: no row stands for more than one session, so a click
    /// on either QA2 row switches to that row's session.
    #[test]
    fn sessions_on_one_card_each_get_their_own_row() {
        let mut sm = card_sessions();
        let collapse_state = CollapseState::new();
        let order = sm.visual_order(&collapse_state);
        let cards: Vec<Option<&str>> = order
            .iter()
            .map(|&id| sm.get(id).unwrap().details.card_ref())
            .collect();
        assert_eq!(
            cards,
            [0, 1, 2, 2, 3, 4].map(|n| Some(CARDS[n].0)),
            "sessions sort by card, the QA2 pair together"
        );
        // The second QA2 session is the active one, so a row standing for
        // both (which switches to its active session) would reach it before
        // the first and break the order below.
        assert!(sm.switch_to(order[3]));

        let mut harness = Harness::builder()
            .with_size(egui::Vec2::new(300.0, 520.0))
            .build_ui_state(
                |ui, state: &mut SessionListHarnessState| {
                    let mut list = SessionListUi::new(
                        &mut state.ui.session_manager,
                        &state.ui.focus_queue,
                        &state.ui.collapse_state,
                        false,
                    );
                    if let Some(action) = list.ui(ui) {
                        state.action = Some(action);
                    }
                },
                SessionListHarnessState {
                    ui: SessionListUiOwnedState {
                        session_manager: sm,
                        focus_queue: FocusQueue::new(),
                        collapse_state,
                        ctrl_held: false,
                        cwd_label: String::new(),
                        cwd_path: PathBuf::new(),
                    },
                    action: None,
                },
            );
        harness.run();

        // Click down the list and note which session each click switches
        // to. A row standing for several sessions would leave all but one of
        // them unreached, so the rows clicked through, in order, must be
        // exactly the visual order, the QA2 pair as two rows.
        let mut reached: Vec<SessionId> = Vec::new();
        let mut y = 40.0;
        while y < 520.0 {
            harness.state_mut().action = None;
            for pressed in [true, false] {
                harness.input_mut().events.push(Event::PointerButton {
                    pos: egui::pos2(150.0, y),
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            harness.run();
            if let Some(SessionListAction::SwitchTo(id)) = harness.state().action {
                if reached.last() != Some(&id) {
                    reached.push(id);
                }
            }
            y += 4.0;
        }
        assert_eq!(reached, order, "each session has a row of its own");
    }

    /// The epic a [`lineage_sessions`] chain's cards are subissues of.
    const EPIC: &str = "headway:dave/subway-unusual-any";

    /// The chain's four cards, all subissues of [`EPIC`].
    const CHAIN: [(&str, &str); 4] = [
        (
            "headway:dave/width-useless-choose",
            "SM1: from-session tag on messages sent between sessions",
        ),
        (
            "headway:dave/name-tree-bulk",
            "SM2: spawn lineage — parent-session and relation on spawn",
        ),
        (
            "headway:dave/tone-grief-hour",
            "SM3: code column and two-line headline rows",
        ),
        (
            "headway:dave/throw-draw-draw",
            "SM4: nest subagents and child sessions; group successor chains",
        ),
    ];

    /// Ids of the sessions in [`lineage_sessions`].
    struct Lineage {
        parent: SessionId,
        subagents: [SessionId; 2],
        child: SessionId,
        links: [SessionId; 4],
    }

    /// One workspace holding a session with a foreground and a background
    /// subagent and a child session, and a four-link successor chain on
    /// [`EPIC`]'s cards: two links spawned with `--successor`, one inferred
    /// from its sibling card. The first link ran in another worktree, so the
    /// list keeps it there and the chain in this one has three links.
    fn lineage_sessions() -> LineageFixture {
        use agentium_core::session_events::SessionRelation;
        use agentium_core::wordid::session_ref;
        let now = crate::session::now_unix();
        let mut sm = SessionManager::new();
        let mut add = |cwd: &str, title: &str, status: AgentStatus, mins: u64| {
            let id = sm.new_session(PathBuf::from(cwd), AiMode::Agentic, BackendType::Claude);
            let s = sm.get_mut(id).expect("session");
            s.details.title = title.to_string();
            s.details.home_dir = "/home/dev".to_string();
            s.details.project_root = Some(PathBuf::from("/home/dev/notedeck"));
            s.details.project_slug = Some("notedeck".to_string());
            s.last_activity = Some(now - mins * 60);
            s.source = crate::session::SessionSource::Remote;
            s.agentic.as_mut().expect("agentic").remote_status = Some(status);
            s.update_status();
            id
        };
        let lane = "/home/dev/notedeck-scene";
        let parent = add(lane, "refactor the session list", AgentStatus::Working, 2);
        let fg = add(
            lane,
            "Explore: find the grouping code",
            AgentStatus::Done,
            2,
        );
        let bg = add(
            lane,
            "Bash: cargo test -p notedeck_dave",
            AgentStatus::Idle,
            1,
        );
        let child = add(lane, "which crate owns the scene?", AgentStatus::Idle, 4);
        let links = [
            add("/home/dev/notedeck", CHAIN[0].0, AgentStatus::Done, 200),
            add(lane, CHAIN[1].0, AgentStatus::Done, 150),
            add(lane, CHAIN[2].0, AgentStatus::Done, 60),
            add(lane, CHAIN[3].0, AgentStatus::Working, 1),
        ];
        let ref_of = |sm: &SessionManager, id: SessionId| {
            session_ref(
                sm.get(id)
                    .unwrap()
                    .agentic
                    .as_ref()
                    .unwrap()
                    .event_session_id(),
            )
        };
        for (id, background) in [(fg, false), (bg, true)] {
            let status = sm.get(id).unwrap().status();
            sm.get_mut(id).unwrap().subagent_of = Some(crate::session::SubagentOf {
                parent,
                task_id: format!("task-{id}"),
                agent_id: None,
                status,
                background,
            });
        }
        let spawn = |sm: &mut SessionManager, id, by, relation| {
            let by = ref_of(sm, by);
            let s = sm.get_mut(id).unwrap();
            s.details.parent_session = Some(by);
            s.details.relation = relation;
        };
        spawn(&mut sm, child, parent, SessionRelation::Child);
        spawn(&mut sm, links[1], links[0], SessionRelation::Successor);
        spawn(&mut sm, links[2], links[1], SessionRelation::None);
        spawn(&mut sm, links[3], links[2], SessionRelation::Successor);

        for (card_ref, title) in CHAIN {
            let lookup = crate::card_titles::CardLookup {
                title: Some(title.to_owned()),
                outline: Some(notedeck::Outline {
                    parent: Some(EPIC.to_owned()),
                    parts: None,
                }),
                ..Default::default()
            };
            sm.card_titles.store(card_ref, lookup, 0.0);
        }
        sm.rebuild_groups();
        let ids = Lineage {
            parent,
            subagents: [fg, bg],
            child,
            links,
        };
        LineageFixture { sm, ids }
    }

    /// What [`lineage_sessions`] builds.
    struct LineageFixture {
        sm: SessionManager,
        ids: Lineage,
    }

    /// A child session nests under its parent and the parent's subagents
    /// get no row at all, and the chain's links follow one another in one
    /// run; Ctrl+J / K walks them in that order. The first link ran in
    /// another worktree, so it stays in that worktree's folder rather than
    /// joining the chain.
    #[test]
    fn lineage_nests_in_visual_order() {
        let LineageFixture { mut sm, ids } = lineage_sessions();
        let collapse = CollapseState::new();
        let chain = &ids.links;
        let expanded = [
            chain[0], chain[1], chain[2], chain[3], ids.parent, ids.child,
        ];
        assert_eq!(sm.visual_order(&collapse), expanded);
        for sub in ids.subagents {
            assert!(sm.get(sub).is_some(), "the subagent's session still exists");
        }

        let groups = sm.host_groups()[0].project_groups[0].cwd_groups.clone();
        assert_eq!(groups[0].display_cwd, "~/notedeck");
        assert_eq!(
            groups[0].session_ids,
            [chain[0]],
            "the first link stays home"
        );
        assert!(groups[0].chains.is_empty());
        let group = &groups[1];
        assert_eq!(group.display_cwd, "~/notedeck-scene");
        let span = &group.chains[0];
        assert_eq!((span.start, span.end), (0, 3));
        assert!(group.rows.iter().all(|r| r.elsewhere.is_none()));
    }

    /// The lineage list: a parent with a child session under it (its two
    /// subagents draw no rows), and a three-link chain, its handed-on links
    /// dimmed and flush with the rows around them on one status bar that
    /// changes colour link to link. The chain's first
    /// link ran in another worktree and is listed in that folder.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_lineage() {
        let mut harness = notedeck::test_harness::with_notedeck_style(
            Harness::builder()
                .with_size(egui::Vec2::new(300.0, 560.0))
                .renderer(notedeck::software_renderer())
                .build_ui_state(
                    |ui, sm: &mut SessionManager| {
                        let focus_queue = FocusQueue::new();
                        let collapse_state = CollapseState::new();
                        SessionListUi::new(sm, &focus_queue, &collapse_state, false).ui(ui);
                    },
                    lineage_sessions().sm,
                ),
        );
        harness.run();
        harness.snapshot("session_list_lineage");
    }

    /// State for [`a_ref_title_draws_its_chip_and_a_chip_click_switches_to_the_row`]: a
    /// real `Notedeck` so the registries and ndb a chip resolves through exist.
    struct RefTitleState {
        host: notedeck_testing::TestHost,
        account: nostrdb_net::FullKeypair,
        session_manager: SessionManager,
        focus_queue: FocusQueue,
        collapse_state: CollapseState,
        action: Option<SessionListAction>,
        /// App actions drained so far (an inert chip raises none).
        app_actions: usize,
        /// Draw the chat header of the first session instead of the list.
        header: bool,
    }

    fn ref_title_ui(ui: &mut egui::Ui, state: &mut RefTitleState) {
        if state.host.setup_once(ui.ctx()) {
            return;
        }
        let mut app_ctx = state.host.app_context();
        let mut note_ctx = app_ctx.note_context();
        let txn = nostrdb::Transaction::new(note_ctx.ndb).expect("txn");
        if state.header {
            let id = state.session_manager.session_ids()[0];
            let details = &state.session_manager.get(id).expect("session").details;
            let mut refs = super::TitleRefs {
                note: &mut note_ctx,
                txn: &txn,
            };
            crate::ui::dave::session_header_ui(
                ui,
                details,
                BackendType::Claude,
                false,
                Some(&mut refs),
                None,
            );
            state.app_actions += app_ctx.app_actions.take().len();
            return;
        }
        let action = SessionListUi::new(
            &mut state.session_manager,
            &state.focus_queue,
            &state.collapse_state,
            false,
        )
        .title_refs(&mut note_ctx, &txn)
        .ui(ui);
        if action.is_some() {
            state.action = action;
        }
        state.app_actions += app_ctx.app_actions.take().len();
    }

    /// Seed the kind-31988 state of session `session_id`, titled `title`.
    fn seed_session_state(ndb: &nostrdb::Ndb, secret: &[u8; 32], session_id: &str, title: &str) {
        let note = nostrdb::NoteBuilder::new()
            .kind(agentium_core::session_events::AI_SESSION_STATE_KIND)
            .content("")
            .created_at(1_700_000_000)
            .start_tag()
            .tag_str("d")
            .tag_str(session_id)
            .start_tag()
            .tag_str("title")
            .tag_str(title)
            .start_tag()
            .tag_str("status")
            .tag_str("working")
            .sign(secret)
            .build()
            .expect("note");
        let frame = nostrdb_net::ClientMessage::event(&note)
            .expect("event")
            .to_json()
            .expect("json");
        ndb.process_event_with(&frame, nostrdb::IngestMetadata::new().client(true))
            .expect("ingest");
    }

    /// A harness drawing one session titled with another session's `agentium:`
    /// ref — the list when `header` is false, its chat header when true — and
    /// waits until that ref's chip shows the referenced session's title,
    /// which it returns beside the harness and the titled session's id.
    fn ref_title_harness(header: bool) -> RefTitleHarness {
        use std::cell::RefCell;
        use std::rc::Rc;
        use std::time::{Duration, Instant};

        let account = nostrdb_net::FullKeypair::generate();
        let mut host = notedeck_testing::TestHost::with_account(&account);
        let notedeck = host.notedeck();
        let cache = Rc::new(RefCell::new(
            crate::session_cache::AgentiumSessionCache::default(),
        ));
        notedeck.register_reference_parser(Box::new(crate::reference::AgentiumRefParser::new(
            cache.clone(),
        )));
        notedeck
            .register_kind_renderer(Box::new(crate::render::AgentiumSessionRenderer::new(cache)));

        let referenced = "claude-session-referenced";
        let mut session_manager = SessionManager::new();
        let id = session_manager.new_session(
            PathBuf::from("/tmp/ref-title"),
            AiMode::Agentic,
            BackendType::Claude,
        );
        let session = session_manager.get_mut(id).expect("session");
        session.details.hostname = "host-a".to_string();
        session.details.title = agentium_core::wordid::session_ref(referenced);
        session.details.custom_title = None;
        session_manager.rebuild_groups();

        let state = RefTitleState {
            host,
            account,
            session_manager,
            focus_queue: FocusQueue::new(),
            collapse_state: CollapseState::new(),
            action: None,
            app_actions: 0,
            header,
        };
        let mut harness = Harness::builder()
            .with_size(egui::vec2(320.0, 300.0))
            .build_ui_state(ref_title_ui, state);
        harness.run_steps(2);

        let secret = harness.state().account.secret_key.secret_bytes();
        let chip_title = "Refactor the session parser";
        seed_session_state(
            harness.state_mut().host.app_context().ndb,
            &secret,
            referenced,
            chip_title,
        );

        // The chip shows the referenced session's title once its state folds.
        let deadline = Instant::now() + Duration::from_secs(5);
        while harness.query_by_label(chip_title).is_none() {
            assert!(Instant::now() < deadline, "the title's chip never resolved");
            std::thread::sleep(Duration::from_millis(25));
            harness.run_ok();
        }
        RefTitleHarness {
            harness,
            id,
            chip_title,
        }
    }

    /// What [`ref_title_harness`] hands back.
    struct RefTitleHarness {
        harness: Harness<'static, RefTitleState>,
        id: SessionId,
        chip_title: &'static str,
    }

    /// A session titled with a reference — here another session's
    /// `agentium:` ref, the way autowork titles a session with its card's
    /// `headway:` ref — draws the referenced entity's chip in its row rather
    /// than the raw ref, and a click on the chip is a click on the row: it
    /// switches to the row's session rather than opening the referenced one.
    #[test]
    fn a_ref_title_draws_its_chip_and_a_chip_click_switches_to_the_row() {
        let RefTitleHarness {
            mut harness,
            id,
            chip_title,
        } = ref_title_harness(false);

        click_label(&mut harness, chip_title);

        assert_eq!(
            harness.state().app_actions,
            0,
            "the chip is inert in the row, so it shouldn't raise its open action"
        );
        assert!(
            matches!(harness.state().action, Some(SessionListAction::SwitchTo(s)) if s == id),
            "the row under the chip takes the click: got {:?}",
            harness.state().action
        );
    }

    /// Click the middle of the widget labelled `label`, and let two frames
    /// take the press and release.
    fn click_label(harness: &mut Harness<'static, RefTitleState>, label: &str) {
        let bounds = harness
            .get_by_label(label)
            .accesskit_node()
            .raw_bounds()
            .expect("label bounds");
        let center = egui::pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        );
        harness.input_mut().events.push(Event::PointerMoved(center));
        for pressed in [true, false] {
            harness.input_mut().events.push(Event::PointerButton {
                pos: center,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        harness.run_steps(2);
    }

    /// The chat header draws a ref-titled session's title the way its row
    /// does: the referenced entity's chip, not the raw ref. Unlike the row's,
    /// the header's chip takes a click: it raises the chip's open action for
    /// the host to route to the app that owns the entity.
    #[test]
    fn the_chat_header_draws_a_ref_title_as_a_chip_that_opens_it() {
        let RefTitleHarness {
            mut harness,
            id,
            chip_title,
        } = ref_title_harness(true);
        let raw = &harness
            .state()
            .session_manager
            .get(id)
            .expect("session")
            .details
            .title;
        assert!(
            harness.query_by_label(raw).is_none(),
            "the header shouldn't draw the raw ref {raw} once its chip resolves"
        );

        click_label(&mut harness, chip_title);

        assert_eq!(
            harness.state().app_actions,
            1,
            "a click on the header's chip raises its open action"
        );
    }

    /// A session titled with its card's ref draws the card's inline chip on
    /// its row, showing the card's title, not the raw ref. The regression: the
    /// row painted the title as bare text and drew no chip.
    #[test]
    fn a_card_titled_row_leads_with_the_cards_chip() {
        use std::cell::RefCell;
        use std::rc::Rc;
        use std::time::{Duration, Instant};

        let card_ref = "headway:dave/able-badge-brick";
        let title = "Session list title chip steals the row click from the session";

        let account = nostrdb_net::FullKeypair::generate();
        let mut host = notedeck_testing::TestHost::with_account(&account);
        let notedeck = host.notedeck();
        let card = Rc::new(RefCell::new(None));
        notedeck.register_reference_parser(Box::new(StubCardParser { card: card.clone() }));
        notedeck.register_kind_renderer(Box::new(StubCardRenderer));

        let mut session_manager = SessionManager::new();
        let id = session_manager.new_session(
            PathBuf::from("/tmp/card-title"),
            AiMode::Agentic,
            BackendType::Claude,
        );
        let session = session_manager.get_mut(id).expect("session");
        session.details.title = card_ref.to_string();
        session.details.custom_title = None;
        session_manager.rebuild_groups();

        let state = RefTitleState {
            host,
            account,
            session_manager,
            focus_queue: FocusQueue::new(),
            collapse_state: CollapseState::new(),
            action: None,
            app_actions: 0,
            header: false,
        };
        let mut harness = Harness::builder()
            .with_size(egui::vec2(300.0, 200.0))
            .build_ui_state(ref_title_ui, state);
        harness.run_steps(2);

        let secret = harness.state().account.secret_key.secret_bytes();
        {
            // Committed before the next frame looks it up, so that lookup
            // finds it rather than keeping a miss for a refresh window.
            let ctx = harness.state_mut().host.app_context();
            let sub = ctx
                .ndb
                .subscribe(&[nostrdb::Filter::new().kinds([1621]).build()])
                .expect("subscribe");
            *card.borrow_mut() = Some(ingest_card(ctx.ndb, &secret, card_ref, title));
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("runtime")
                .block_on(
                    ctx.ndb
                        .wait_for_all_notes_within(sub, 1, Duration::from_secs(5)),
                )
                .expect("the card's note is ingested");
        }

        // The card's title is looked up for the list, and the row's chip
        // draws it.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            harness.run_ok();
            let looked_up =
                harness.state().session_manager.card_titles.get(card_ref) == Some(title);
            if looked_up && harness.query_by_label(title).is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the card-titled row never drew its card's chip (looked up: {looked_up})"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            harness.query_by_label(card_ref).is_none(),
            "the row shows the card, not its raw ref"
        );
    }
}
