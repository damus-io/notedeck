use crate::config::{AiMode, LeaderKey};
use egui::{Key, Modifiers};

/// Keybinding actions that can be triggered globally
#[derive(Debug, Clone, PartialEq)]
pub enum KeyAction {
    /// Accept/Allow a pending permission request
    AcceptPermission,
    /// Deny a pending permission request
    DenyPermission,
    /// Tentatively accept, waiting for message (Shift+1)
    TentativeAccept,
    /// Tentatively deny, waiting for message (Shift+2)
    TentativeDeny,
    /// Allow always — add to session allowlist and accept (3)
    AllowAlways,
    /// Tentatively allow always, waiting for message (Shift+3)
    TentativeAllowAlways,
    /// Cancel tentative state (Escape when tentative)
    CancelTentative,
    /// Switch to agent by number (0-indexed)
    SwitchToAgent(usize),
    /// Cycle to next agent
    NextAgent,
    /// Cycle to previous agent
    PreviousAgent,
    /// Spawn a new agent (Ctrl+T)
    NewAgent,
    /// Stop the active session's running turn (`s` in the chord), the same
    /// thing the Stop button does
    Interrupt,
    /// Toggle between scene view and classic view
    ToggleView,
    /// Cycle permission mode: Manual → Plan → Accept Edits → Auto (Ctrl+M)
    CyclePermissionMode,
    /// Delete the active session
    DeleteActiveSession,
    /// Navigate to next item in focus queue (Ctrl+N)
    FocusQueueNext,
    /// Navigate to previous item in focus queue (Ctrl+P)
    FocusQueuePrev,
    /// Toggle Done status for current focus queue item (Ctrl+D)
    FocusQueueToggleDone,
    /// Toggle auto-steal focus mode (Ctrl+\)
    ToggleAutoSteal,
    /// Open external editor for composing input (Ctrl+G)
    OpenExternalEditor,
    /// Open a new terminal window (Ctrl+`)
    OpenTerminal,
    /// Clone the active agent with the same working directory (Ctrl+Shift+T)
    CloneAgent,
    /// Clear the active agent (Ctrl+Shift+C)
    ClearAgent,
    /// Rename the active agent (Ctrl+Shift+R)
    RenameAgent,
    /// Move the block cursor to the next collapsible block (<leader> j, Ctrl+Shift+N)
    BlockCursorDown,
    /// Move the block cursor to the previous collapsible block (<leader> k, Ctrl+Shift+P)
    BlockCursorUp,
    /// Move the block cursor to the first collapsible block (<leader> gg)
    BlockCursorFirst,
    /// Move the block cursor to the last collapsible block (<leader> G)
    BlockCursorLast,
    /// Flip the block under the cursor (<leader> za / o, Ctrl+Shift+O)
    BlockToggle,
    /// Expand the block under the cursor (<leader> zo)
    BlockOpen,
    /// Collapse the block under the cursor (<leader> zc)
    BlockClose,
    /// Expand every collapsible block (<leader> zR, Ctrl+Shift+E)
    BlockExpandAll,
    /// Collapse every collapsible block (<leader> zM, Ctrl+Shift+M)
    BlockCollapseAll,
    /// Drop the block cursor, so the transcript follows new output again (<leader> q)
    BlockCursorClear,
    /// Point the chord at the session list (<leader> h)
    FocusSessionsPane,
    /// Point the chord back at the chat (<leader> l, or Enter from the session list)
    FocusChatPane,
    /// Switch to the next session without focusing its input (<leader> h j)
    SessionPaneNext,
    /// Switch to the previous session without focusing its input (<leader> h k)
    SessionPanePrev,
    /// Switch to the first session in the list (<leader> h gg)
    SessionPaneFirst,
    /// Switch to the last session in the list (<leader> h G)
    SessionPaneLast,
}

impl KeyAction {
    /// Actions that only mean something for agentic sessions. The Ctrl ladder
    /// gates their bindings on `is_agentic`, and the chord follows suit.
    fn agentic_only(&self) -> bool {
        matches!(
            self,
            KeyAction::CloneAgent
                | KeyAction::ToggleView
                | KeyAction::CyclePermissionMode
                | KeyAction::FocusQueueNext
                | KeyAction::FocusQueuePrev
        )
    }

    /// Actions that can leave a different session active. A chord that ran one
    /// can't hand focus back to the id it saved: that was the old session's
    /// input, which no longer renders.
    fn changes_session(&self) -> bool {
        matches!(
            self,
            KeyAction::SessionPaneNext
                | KeyAction::SessionPanePrev
                | KeyAction::SessionPaneFirst
                | KeyAction::SessionPaneLast
                | KeyAction::CloneAgent
                | KeyAction::DeleteActiveSession
                | KeyAction::FocusQueueNext
                | KeyAction::FocusQueuePrev
        )
    }
}

/// The key that opens a chord, resolved from the persisted [`LeaderKey`].
///
/// Built once when settings load or change, so the per-frame match never
/// parses a key name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Leader {
    modifiers: Modifiers,
    key: Key,
}

impl Leader {
    /// Ctrl+;, the same binding as [`LeaderKey::default`].
    pub const DEFAULT: Leader = Leader {
        modifiers: Modifiers::CTRL,
        key: Key::Semicolon,
    };

    /// Resolve a persisted leader. A key name egui does not know (a
    /// hand-edited or newer settings file) falls back to [`Self::DEFAULT`].
    pub fn resolve(leader: &LeaderKey) -> Self {
        let Some(key) = leader.resolve() else {
            tracing::warn!("unknown leader key {:?}, using Ctrl+;", leader.key);
            return Self::DEFAULT;
        };
        Leader {
            modifiers: leader.modifiers(),
            key,
        }
    }

    /// Whether this frame pressed the leader.
    fn pressed(self, input: &egui::InputState) -> bool {
        input.modifiers.matches_exact(self.modifiers) && input.key_pressed(self.key)
    }
}

/// Progress through a multi-key chord opened by the leader key.
///
/// Owned by [`Dave`](crate::Dave) and passed `&mut` into [`check_keybindings`]
/// each frame. While a chord is pending the chat input's focus is set aside, so
/// the bare keys that follow (`j`, `z`, `g`, …) are read as commands rather than
/// typed; when the chord ends, focus goes back to whatever held it.
///
/// A chord is a latch: it stays open after a command so motions repeat —
/// `<leader> j j j za` — and ends only on Esc, `q`, a key it does not know, or
/// a modified key. It never times out, so pausing to read a block does not drop
/// you back into the input mid-stride; the which-key strip shows it is open.
///
/// `h` / `l` point the chord at the session list or the chat ([`Pane`]), and
/// the motions follow: `j` / `k` walk blocks in the chat and sessions in the
/// list.
#[derive(Default)]
pub struct NormalMode {
    pending: Option<Pending>,
    /// Which pane the motions move through. Back to [`Pane::Chat`] on every
    /// leader.
    pane: Pane,
    /// Whatever held keyboard focus when the leader fired, handed back when the
    /// chord ends.
    restore_focus: Option<egui::Id>,
    /// Whether the session list is on screen, as of the last frame. `h` is a
    /// no-op without it.
    sessions_shown: bool,
    /// Whether the active session is agentic, as of the last frame.
    agentic: bool,
    /// Whether the active session has a running turn to stop, as of the last
    /// frame.
    interruptible: bool,
    /// When the chord ends, focus the active session's input instead of
    /// `restore_focus`: the chord switched sessions, or an action asked for
    /// the input while the chord held the keyboard.
    focus_input_on_end: bool,
    /// The chord ended with `focus_input_on_end` set; taken by
    /// [`Self::take_input_focus`].
    input_focus_due: bool,
}

/// How far into a chord we are. Read by the which-key strip through
/// [`NormalMode::view`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pending {
    /// The leader fired; waiting for a command or a prefix.
    Leader,
    /// `<leader> z`: waiting for `a` / `o` / `c` / `R` / `M`.
    LeaderZ,
    /// `<leader> g`: waiting for the second `g`.
    LeaderG,
    /// `<leader> d`: waiting for the second `d`.
    LeaderD,
    /// `<leader> ]`: waiting for `q`.
    LeaderCloseBracket,
    /// `<leader> [`: waiting for `q`.
    LeaderOpenBracket,
}

/// Which part of Dave a chord's motions move through.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pane {
    /// The session list: `j` / `k` switch sessions.
    Sessions,
    /// The chat transcript: `j` / `k` walk its collapsible blocks.
    #[default]
    Chat,
}

/// A pending chord as the UI sees it: the which-key strip reads its hints, and
/// the session list marks its row while the chord is in [`Pane::Sessions`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChordView {
    pub pending: Pending,
    pub pane: Pane,
    /// `h` can reach the session list.
    pub sessions_shown: bool,
    /// The agentic-only keys (`c`, `v`, `m`, `]q`, `[q`) apply.
    pub agentic: bool,
    /// `s` has a running turn to stop.
    pub interruptible: bool,
}

/// One continuation a pending chord accepts, as the which-key strip shows it.
pub struct ChordHint {
    /// The keys to type from here, as printed on the keycap (`"za"`, `"G"`).
    pub keys: &'static str,
    /// What typing them does.
    pub action: KeyAction,
}

/// Shorthand for the hint tables below.
const fn hint(keys: &'static str, action: KeyAction) -> ChordHint {
    ChordHint { keys, action }
}

/// Chat-pane motions: walk the transcript's collapsible blocks.
const BLOCK_MOTIONS: &[ChordHint] = &[
    hint("j", KeyAction::BlockCursorDown),
    hint("k", KeyAction::BlockCursorUp),
    hint("gg", KeyAction::BlockCursorFirst),
    hint("G", KeyAction::BlockCursorLast),
];

/// Sessions-pane motions: walk the session list.
const SESSION_MOTIONS: &[ChordHint] = &[
    hint("j", KeyAction::SessionPaneNext),
    hint("k", KeyAction::SessionPanePrev),
    hint("gg", KeyAction::SessionPaneFirst),
    hint("G", KeyAction::SessionPaneLast),
];

/// Session keys, from either pane: the chord's names for Ctrl bindings.
const SESSION_LIFECYCLE: &[ChordHint] = &[
    hint("n", KeyAction::NewAgent),
    hint("c", KeyAction::CloneAgent),
    hint("r", KeyAction::RenameAgent),
    hint("dd", KeyAction::DeleteActiveSession),
];

/// View keys, from either pane.
const SESSION_VIEW: &[ChordHint] = &[
    hint("v", KeyAction::ToggleView),
    hint("m", KeyAction::CyclePermissionMode),
    hint("e", KeyAction::OpenExternalEditor),
];

/// Turn keys, from either pane.
const SESSION_TURN: &[ChordHint] = &[hint("s", KeyAction::Interrupt)];

/// Focus-queue keys, from either pane (vim's quickfix `]q` / `[q`).
const FOCUS_QUEUE: &[ChordHint] = &[
    hint("]q", KeyAction::FocusQueueNext),
    hint("[q", KeyAction::FocusQueuePrev),
];

/// What `<leader>` accepts in the chat pane.
const CHAT_LEADER_HINTS: &[&[ChordHint]] = &[
    BLOCK_MOTIONS,
    &[
        hint("za", KeyAction::BlockToggle),
        hint("o", KeyAction::BlockToggle),
        hint("zo", KeyAction::BlockOpen),
        hint("zc", KeyAction::BlockClose),
    ],
    &[
        hint("zR", KeyAction::BlockExpandAll),
        hint("zM", KeyAction::BlockCollapseAll),
    ],
    &[hint("h", KeyAction::FocusSessionsPane)],
    &[hint("q", KeyAction::BlockCursorClear)],
];

/// The keycap for Enter.
const ENTER: &str = "\u{21b5}";

/// What `<leader>` accepts in the sessions pane.
const SESSIONS_LEADER_HINTS: &[&[ChordHint]] = &[
    SESSION_MOTIONS,
    &[
        hint("l", KeyAction::FocusChatPane),
        hint(ENTER, KeyAction::FocusChatPane),
    ],
];

/// What `<leader>` accepts in either pane besides the pane's own keys: the
/// strip gives them a row of their own.
const SESSION_KEYS: &[&[ChordHint]] = &[SESSION_LIFECYCLE, SESSION_VIEW, SESSION_TURN, FOCUS_QUEUE];

/// What `<leader> z` accepts.
const LEADER_Z_HINTS: &[&[ChordHint]] = &[
    &[
        hint("a", KeyAction::BlockToggle),
        hint("o", KeyAction::BlockOpen),
        hint("c", KeyAction::BlockClose),
    ],
    &[
        hint("R", KeyAction::BlockExpandAll),
        hint("M", KeyAction::BlockCollapseAll),
    ],
];

/// What `<leader> g` accepts in the chat pane.
const CHAT_LEADER_G_HINTS: &[&[ChordHint]] = &[&[hint("g", KeyAction::BlockCursorFirst)]];

/// What `<leader> g` accepts in the sessions pane.
const SESSIONS_LEADER_G_HINTS: &[&[ChordHint]] = &[&[hint("g", KeyAction::SessionPaneFirst)]];

/// What `<leader> d` accepts.
const LEADER_D_HINTS: &[&[ChordHint]] = &[&[hint("d", KeyAction::DeleteActiveSession)]];

/// What `<leader> ]` accepts.
const LEADER_CLOSE_BRACKET_HINTS: &[&[ChordHint]] = &[&[hint("q", KeyAction::FocusQueueNext)]];

/// What `<leader> [` accepts.
const LEADER_OPEN_BRACKET_HINTS: &[&[ChordHint]] = &[&[hint("q", KeyAction::FocusQueuePrev)]];

impl ChordView {
    /// The pane's keys this state could accept, in groups the strip spaces
    /// apart; [`Self::session_keys`] has the rest. Filter through
    /// [`Self::offers`]: a group may hold keys that do nothing this frame.
    ///
    /// Mirrors the match in `check_chord`; `every_hint_does_what_it_says`
    /// keeps the two from drifting.
    pub fn hints(self) -> &'static [&'static [ChordHint]] {
        match (self.pane, self.pending) {
            (Pane::Chat, Pending::Leader) => CHAT_LEADER_HINTS,
            (Pane::Sessions, Pending::Leader) => SESSIONS_LEADER_HINTS,
            (_, Pending::LeaderZ) => LEADER_Z_HINTS,
            (Pane::Chat, Pending::LeaderG) => CHAT_LEADER_G_HINTS,
            (Pane::Sessions, Pending::LeaderG) => SESSIONS_LEADER_G_HINTS,
            (_, Pending::LeaderD) => LEADER_D_HINTS,
            (_, Pending::LeaderCloseBracket) => LEADER_CLOSE_BRACKET_HINTS,
            (_, Pending::LeaderOpenBracket) => LEADER_OPEN_BRACKET_HINTS,
        }
    }

    /// The session keys this state accepts, from either pane: empty past the
    /// leader.
    pub fn session_keys(self) -> &'static [&'static [ChordHint]] {
        match self.pending {
            Pending::Leader => SESSION_KEYS,
            _ => &[],
        }
    }

    /// Whether `action` does anything this frame: `h` needs the session list
    /// on screen, `s` a running turn, and the agentic-only keys an agentic
    /// session. A key that doesn't is swallowed without ending the chord, and
    /// the strip leaves it out.
    pub fn offers(self, action: &KeyAction) -> bool {
        match action {
            KeyAction::FocusSessionsPane => self.sessions_shown,
            KeyAction::Interrupt => self.interruptible,
            action if action.agentic_only() => self.agentic,
            _ => true,
        }
    }
}

/// What the chord machine made of this frame's input.
enum ChordStep {
    /// No chord is pending (or it just ended on a modified key): the regular
    /// bindings should look at this frame.
    FallThrough,
    /// The chord owned this frame's keys, and maybe produced an action.
    Consumed(Option<KeyAction>),
}

impl NormalMode {
    /// The pending chord as the UI sees it, or `None` when no chord is pending.
    pub fn view(&self) -> Option<ChordView> {
        self.pending.map(|pending| self.view_at(pending))
    }

    /// This chord's view, at `pending`.
    fn view_at(&self, pending: Pending) -> ChordView {
        ChordView {
            pending,
            pane: self.pane,
            sessions_shown: self.sessions_shown,
            agentic: self.agentic,
            interruptible: self.interruptible,
        }
    }

    /// Whether a chord that just ended wants the active session's input
    /// focused. Reading it clears it, so it is acted on once.
    pub fn take_input_focus(&mut self) -> bool {
        std::mem::take(&mut self.input_focus_due)
    }

    /// Hold an action's request to focus the input until the chord ends.
    /// Taking focus mid-chord would put the input back under the bare keys.
    pub fn defer_input_focus(&mut self) {
        self.focus_input_on_end = true;
    }

    /// Record what this frame offers the chord. With the session list off
    /// screen (a narrow layout, the scene view) it falls back to the chat.
    fn observe(&mut self, sessions_shown: bool, agentic: bool, interruptible: bool) {
        self.sessions_shown = sessions_shown;
        self.agentic = agentic;
        self.interruptible = interruptible;
        if !sessions_shown {
            self.pane = Pane::Chat;
        }
    }

    /// Open a chord: set the focused widget aside so bare keys reach us.
    fn open(&mut self, ctx: &egui::Context) {
        self.restore_focus = ctx.memory_mut(|m| {
            let focused = m.focused();
            if let Some(id) = focused {
                m.surrender_focus(id);
            }
            focused
        });
        self.pane = Pane::Chat;
        self.focus_input_on_end = false;
        self.pending = Some(Pending::Leader);
    }

    /// End the chord and give focus back to whatever held it before, or to
    /// the active session's input if the chord moved between sessions.
    fn end(&mut self, ctx: &egui::Context) {
        self.pending = None;
        let restore = self.restore_focus.take();
        if std::mem::take(&mut self.focus_input_on_end) {
            self.input_focus_due = true;
        } else if let Some(id) = restore {
            ctx.memory_mut(|m| m.request_focus(id));
        }
    }

    /// End the chord and leave focus alone: the action opens something that
    /// takes typing (a rename field, the new-agent picker) and claims focus
    /// itself.
    fn release(&mut self) {
        self.pending = None;
        self.restore_focus = None;
        self.focus_input_on_end = false;
    }
}

/// Where a chord goes after a key.
enum Then {
    /// Stay open, waiting in this state.
    Continue(Pending),
    /// End, handing focus back.
    End,
    /// End without touching focus (see [`NormalMode::release`]).
    Release,
}

/// A key press, as the chord machine reads it.
#[derive(Clone, Copy)]
struct KeyPress {
    key: Key,
    shift: bool,
    /// Ctrl / Alt / Cmd held — never part of a chord, so it ends one.
    modified: bool,
}

/// The first key press in this frame's events, repeats included so a held `j`
/// keeps walking.
fn first_key_press(input: &egui::InputState) -> Option<KeyPress> {
    input.events.iter().find_map(|event| match event {
        egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => Some(KeyPress {
            key: *key,
            shift: modifiers.shift,
            modified: modifiers.ctrl || modifiers.alt || modifiers.command || modifiers.mac_cmd,
        }),
        _ => None,
    })
}

/// Feed this frame's input to a pending chord.
fn check_chord(ctx: &egui::Context, chord: &mut NormalMode) -> ChordStep {
    let Some(pending) = chord.pending else {
        return ChordStep::FallThrough;
    };

    // Esc cancels the chord and nothing else.
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
        chord.end(ctx);
        return ChordStep::Consumed(None);
    }

    let Some(press) = ctx.input(first_key_press) else {
        return ChordStep::Consumed(None);
    };

    // A modified key is somebody else's binding (a Ctrl alias, the leader
    // again): end the chord and let the regular ladder have it.
    if press.modified {
        chord.end(ctx);
        return ChordStep::FallThrough;
    }

    // Swallow this frame's keys and text so a bare key never leaks into the
    // input, the permission bindings, or Delete-to-delete-session.
    ctx.input_mut(|i| {
        i.events
            .retain(|e| !matches!(e, egui::Event::Key { .. } | egui::Event::Text(_)))
    });

    use KeyAction as A;
    use Then::{Continue, End, Release};
    let (then, action) = match (chord.pane, pending, press.key, press.shift) {
        // Motions: the same keys walk blocks in the chat, sessions in the list.
        (Pane::Chat, Pending::Leader, Key::J, false) => {
            (Continue(Pending::Leader), Some(A::BlockCursorDown))
        }
        (Pane::Chat, Pending::Leader, Key::K, false) => {
            (Continue(Pending::Leader), Some(A::BlockCursorUp))
        }
        (Pane::Chat, Pending::Leader, Key::G, true) => {
            (Continue(Pending::Leader), Some(A::BlockCursorLast))
        }
        (Pane::Chat, Pending::LeaderG, Key::G, false) => {
            (Continue(Pending::Leader), Some(A::BlockCursorFirst))
        }
        (Pane::Sessions, Pending::Leader, Key::J, false) => {
            (Continue(Pending::Leader), Some(A::SessionPaneNext))
        }
        (Pane::Sessions, Pending::Leader, Key::K, false) => {
            (Continue(Pending::Leader), Some(A::SessionPanePrev))
        }
        (Pane::Sessions, Pending::Leader, Key::G, true) => {
            (Continue(Pending::Leader), Some(A::SessionPaneLast))
        }
        (Pane::Sessions, Pending::LeaderG, Key::G, false) => {
            (Continue(Pending::Leader), Some(A::SessionPaneFirst))
        }
        (_, Pending::Leader, Key::G, false) => (Continue(Pending::LeaderG), None),

        // Panes. Enter from the list goes back to the chat and hands it the
        // keyboard: you've picked the session you wanted.
        (_, Pending::Leader, Key::H, false) => {
            (Continue(Pending::Leader), Some(A::FocusSessionsPane))
        }
        (_, Pending::Leader, Key::L, false) => (Continue(Pending::Leader), Some(A::FocusChatPane)),
        (Pane::Sessions, Pending::Leader, Key::Enter, false) => (End, Some(A::FocusChatPane)),

        // Folds, in the chat only.
        (Pane::Chat, Pending::Leader, Key::Z, false) => (Continue(Pending::LeaderZ), None),
        (Pane::Chat, Pending::Leader, Key::O, false) => {
            (Continue(Pending::Leader), Some(A::BlockToggle))
        }
        (Pane::Chat, Pending::Leader, Key::Q, false) => (End, Some(A::BlockCursorClear)),
        (_, Pending::LeaderZ, Key::A, false) => (Continue(Pending::Leader), Some(A::BlockToggle)),
        (_, Pending::LeaderZ, Key::O, false) => (Continue(Pending::Leader), Some(A::BlockOpen)),
        (_, Pending::LeaderZ, Key::C, false) => (Continue(Pending::Leader), Some(A::BlockClose)),
        (_, Pending::LeaderZ, Key::R, true) => (Continue(Pending::Leader), Some(A::BlockExpandAll)),
        (_, Pending::LeaderZ, Key::M, true) => {
            (Continue(Pending::Leader), Some(A::BlockCollapseAll))
        }

        // Session keys, from either pane. New-agent and rename open something
        // you type into, so they end the chord; so does the external editor.
        (_, Pending::Leader, Key::N, false) => (Release, Some(A::NewAgent)),
        (_, Pending::Leader, Key::C, false) => (Continue(Pending::Leader), Some(A::CloneAgent)),
        (_, Pending::Leader, Key::R, false) => (Release, Some(A::RenameAgent)),
        (_, Pending::Leader, Key::D, false) => (Continue(Pending::LeaderD), None),
        (_, Pending::LeaderD, Key::D, false) => {
            (Continue(Pending::Leader), Some(A::DeleteActiveSession))
        }
        (_, Pending::Leader, Key::V, false) => (Continue(Pending::Leader), Some(A::ToggleView)),
        (_, Pending::Leader, Key::M, false) => {
            (Continue(Pending::Leader), Some(A::CyclePermissionMode))
        }
        (_, Pending::Leader, Key::E, false) => (End, Some(A::OpenExternalEditor)),
        (_, Pending::Leader, Key::S, false) => (Continue(Pending::Leader), Some(A::Interrupt)),
        (_, Pending::Leader, Key::CloseBracket, false) => {
            (Continue(Pending::LeaderCloseBracket), None)
        }
        (_, Pending::Leader, Key::OpenBracket, false) => {
            (Continue(Pending::LeaderOpenBracket), None)
        }
        (_, Pending::LeaderCloseBracket, Key::Q, false) => {
            (Continue(Pending::Leader), Some(A::FocusQueueNext))
        }
        (_, Pending::LeaderOpenBracket, Key::Q, false) => {
            (Continue(Pending::Leader), Some(A::FocusQueuePrev))
        }

        // Anything else cancels the chord; the stray key is dropped.
        _ => (End, None),
    };

    // A key that does nothing here (`h` with no session list, `v` in a chat
    // session) is swallowed and the chord carries on.
    let view = chord.view_at(pending);
    let action = action.filter(|action| view.offers(action));

    match &action {
        Some(A::FocusSessionsPane) => chord.pane = Pane::Sessions,
        Some(A::FocusChatPane) => chord.pane = Pane::Chat,
        Some(action) if action.changes_session() => chord.focus_input_on_end = true,
        _ => {}
    }

    match then {
        Continue(next) => chord.pending = Some(next),
        End => chord.end(ctx),
        Release => chord.release(),
    }
    ChordStep::Consumed(action)
}

/// What a frame offers the keybindings besides the keys themselves, gathered
/// by `Dave::process_keybindings` from the active session and the layout.
#[derive(Clone, Copy, Debug)]
pub struct KeyContext {
    /// The key that opens a chord.
    pub leader: Leader,
    /// The active session's mode; agentic-only bindings need
    /// [`AiMode::Agentic`].
    pub ai_mode: AiMode,
    /// The session list is on screen, for the chord's `h`.
    pub sessions_shown: bool,
    /// The active session has a running turn, for the chord's `s`.
    pub interruptible: bool,
    /// A permission request is waiting, for the bare `1` / `2` / `3` keys.
    pub has_pending_permission: bool,
    /// The waiting request is a question set, which takes the number keys
    /// itself.
    pub has_pending_question: bool,
    /// A tentative accept/deny is waiting for its message; Esc cancels it.
    pub in_tentative_state: bool,
}

/// Check for keybinding actions.
/// Most keybindings use Ctrl modifier to avoid conflicts with text input.
/// Exception: 1/2 for permission responses work without Ctrl but only when no text input has focus.
/// In Chat mode, agentic-specific keybindings (scene view, plan mode, focus queue) are disabled.
///
/// `chord` carries a leader chord across frames: while one is pending it owns
/// the keyboard (see [`NormalMode`]). `keys` is what the frame offers the
/// bindings (see [`KeyContext`]).
pub fn check_keybindings(
    ctx: &egui::Context,
    chord: &mut NormalMode,
    keys: KeyContext,
) -> Option<KeyAction> {
    let KeyContext {
        leader,
        ai_mode,
        sessions_shown,
        interruptible,
        has_pending_permission,
        has_pending_question,
        in_tentative_state,
    } = keys;
    let is_agentic = ai_mode == AiMode::Agentic;
    chord.observe(sessions_shown, is_agentic, interruptible);

    // A pending chord reads bare keys, and its Esc outranks every other Esc.
    if let ChordStep::Consumed(action) = check_chord(ctx, chord) {
        return action;
    }

    // Escape in tentative state cancels the tentative mode (agentic only)
    if is_agentic
        && in_tentative_state
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape))
    {
        return Some(KeyAction::CancelTentative);
    }

    let ctrl = egui::Modifiers::CTRL;
    let ctrl_shift = egui::Modifiers::CTRL | egui::Modifiers::SHIFT;

    // The leader opens a chord; the keys that follow land next frame.
    if ctx.input(|i| leader.pressed(i)) {
        chord.open(ctx);
        return None;
    }

    // Ctrl+J / Ctrl+K for cycling through agents/chats.
    // Previously Ctrl+Tab / Ctrl+Shift+Tab, but the chrome now consumes those
    // for app-level tab switching (see notedeck_chrome::chrome::cycle_app), so
    // they never reach dave. Works even with text input focus since the Ctrl
    // modifier makes them unambiguous.
    // Use matches_exact so Ctrl+Shift+K (ClearAgent) isn't swallowed here.
    if ctx.input(|i| i.modifiers.matches_exact(ctrl) && i.key_pressed(Key::J)) {
        return Some(KeyAction::NextAgent);
    }
    if ctx.input(|i| i.modifiers.matches_exact(ctrl) && i.key_pressed(Key::K)) {
        return Some(KeyAction::PreviousAgent);
    }

    // Focus queue navigation - agentic only
    if is_agentic {
        // Ctrl+N for higher priority (toward NeedsInput)
        if ctx.input(|i| i.modifiers.matches_exact(ctrl) && i.key_pressed(Key::N)) {
            return Some(KeyAction::FocusQueueNext);
        }

        // Ctrl+P for lower priority (toward Done)
        if ctx.input(|i| i.modifiers.matches_exact(ctrl) && i.key_pressed(Key::P)) {
            return Some(KeyAction::FocusQueuePrev);
        }
    }

    // Ctrl+Shift+T to clone the active agent (check before Ctrl+T) - agentic only
    if is_agentic && ctx.input(|i| i.modifiers.matches_exact(ctrl_shift) && i.key_pressed(Key::T)) {
        return Some(KeyAction::CloneAgent);
    }

    // Ctrl+Shift+K to clear the active agent - agentic only
    if is_agentic && ctx.input(|i| i.modifiers.matches_exact(ctrl_shift) && i.key_pressed(Key::K)) {
        return Some(KeyAction::ClearAgent);
    }

    // Ctrl+Shift+R to rename the active agent
    if ctx.input(|i| i.modifiers.matches_exact(ctrl_shift) && i.key_pressed(Key::R)) {
        return Some(KeyAction::RenameAgent);
    }

    // Ctrl+Shift aliases for the block cursor, usable mid-typing without a chord.
    if let Some(action) = ctx.input(|i| {
        if !i.modifiers.matches_exact(ctrl_shift) {
            return None;
        }
        [
            (Key::N, KeyAction::BlockCursorDown),
            (Key::P, KeyAction::BlockCursorUp),
            (Key::O, KeyAction::BlockToggle),
            (Key::E, KeyAction::BlockExpandAll),
            (Key::M, KeyAction::BlockCollapseAll),
        ]
        .into_iter()
        .find_map(|(key, action)| i.key_pressed(key).then_some(action))
    }) {
        return Some(action);
    }

    // Ctrl+T to spawn a new agent/chat
    if ctx.input(|i| i.modifiers.matches_exact(ctrl) && i.key_pressed(Key::T)) {
        return Some(KeyAction::NewAgent);
    }

    // Ctrl+L to toggle between scene view and list view - agentic only
    if is_agentic && ctx.input(|i| i.modifiers.matches_exact(ctrl) && i.key_pressed(Key::L)) {
        return Some(KeyAction::ToggleView);
    }

    // Ctrl+G to open external editor for composing input
    if ctx.input(|i| i.modifiers.matches_exact(ctrl) && i.key_pressed(Key::G)) {
        return Some(KeyAction::OpenExternalEditor);
    }

    // Ctrl+` to open a new terminal window
    if ctx.input(|i| i.modifiers.matches_exact(ctrl) && i.key_pressed(Key::Backtick)) {
        return Some(KeyAction::OpenTerminal);
    }

    // Ctrl+M to cycle permission mode - agentic only
    if is_agentic && ctx.input(|i| i.modifiers.matches_exact(ctrl) && i.key_pressed(Key::M)) {
        return Some(KeyAction::CyclePermissionMode);
    }

    // Ctrl+D to toggle Done status for current focus queue item - agentic only
    if is_agentic && ctx.input(|i| i.modifiers.matches_exact(ctrl) && i.key_pressed(Key::D)) {
        return Some(KeyAction::FocusQueueToggleDone);
    }

    // Ctrl+\ to toggle auto-steal focus mode (Ctrl+Space conflicts with macOS input source switching) - agentic only
    if is_agentic && ctx.input(|i| i.modifiers.matches_exact(ctrl) && i.key_pressed(Key::Backslash))
    {
        return Some(KeyAction::ToggleAutoSteal);
    }

    // Delete key to delete active session (only when no text input has focus)
    if !ctx.egui_wants_keyboard_input() && ctx.input(|i| i.key_pressed(Key::Delete)) {
        return Some(KeyAction::DeleteActiveSession);
    }

    // Ctrl+1-9 for switching agents/chats (works even with text input focus)
    // Check this BEFORE permission bindings so Ctrl+number always switches agents
    if let Some(action) = ctx.input(|i| {
        if !i.modifiers.matches_exact(ctrl) {
            return None;
        }

        for (idx, key) in [
            Key::Num1,
            Key::Num2,
            Key::Num3,
            Key::Num4,
            Key::Num5,
            Key::Num6,
            Key::Num7,
            Key::Num8,
            Key::Num9,
        ]
        .iter()
        .enumerate()
        {
            if i.key_pressed(*key) {
                return Some(KeyAction::SwitchToAgent(idx));
            }
        }

        None
    }) {
        return Some(action);
    }

    // Permission keybindings - agentic only
    // When there's a pending permission (but NOT an AskUserQuestion):
    // - 1 = accept, 2 = deny (no modifiers)
    // - Shift+1 = tentative accept, Shift+2 = tentative deny (for adding message)
    // This is checked AFTER Ctrl+number so Ctrl bindings take precedence
    // IMPORTANT: Only handle these when no text input has focus, to avoid
    // capturing keypresses when user is typing a message in tentative state
    // AskUserQuestion uses number keys for option selection, so we skip these bindings
    if is_agentic
        && has_pending_permission
        && !has_pending_question
        && !ctx.egui_wants_keyboard_input()
    {
        // Shift+1 = tentative accept, Shift+2 = tentative deny
        // Note: egui may report shifted keys as their symbol (e.g., Shift+1 as Exclamationmark)
        // We check for both the symbol key and Shift+Num key to handle different behaviors
        if let Some(action) = ctx.input_mut(|i| {
            // Shift+1: check for '!' (Exclamationmark) which egui reports on some systems
            if i.key_pressed(Key::Exclamationmark) {
                return Some(KeyAction::TentativeAccept);
            }
            // Shift+2: check with shift modifier (egui may report Num2 with shift held)
            if i.modifiers.shift && i.key_pressed(Key::Num2) {
                return Some(KeyAction::TentativeDeny);
            }
            // Shift+3: tentative allow always
            if i.modifiers.shift && i.key_pressed(Key::Num3) {
                return Some(KeyAction::TentativeAllowAlways);
            }
            None
        }) {
            return Some(action);
        }

        // Bare keypresses (no modifiers) for immediate accept/deny/always
        if let Some(action) = ctx.input(|i| {
            if !i.modifiers.any() {
                if i.key_pressed(Key::Num1) {
                    return Some(KeyAction::AcceptPermission);
                } else if i.key_pressed(Key::Num2) {
                    return Some(KeyAction::DenyPermission);
                } else if i.key_pressed(Key::Num3) {
                    return Some(KeyAction::AllowAlways);
                }
            }
            None
        }) {
            return Some(action);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiMode;
    use egui::{Key, Modifiers};
    use egui_kittest::Harness;
    use notedeck::test_harness::PressKey;

    /// Press `modifiers`+`key` in a headless egui frame and return whatever
    /// `check_keybindings` detects (agentic mode, no pending prompts).
    fn detect(modifiers: Modifiers, key: Key) -> Option<KeyAction> {
        detect_sequence(&[(modifiers, key)])
    }

    /// Press each `(modifiers, key)` in turn, threading one [`NormalMode`]
    /// through every frame the way `Dave` does, and return the last action
    /// `check_keybindings` detected along the way.
    fn detect_sequence(presses: &[(Modifiers, Key)]) -> Option<KeyAction> {
        detect_sequence_with(Leader::DEFAULT, presses)
    }

    /// The session list on screen and a turn running, agentic, with no
    /// pending prompts: every chord key applies.
    const FRAME: KeyContext = KeyContext {
        leader: Leader::DEFAULT,
        ai_mode: AiMode::Agentic,
        sessions_shown: true,
        interruptible: true,
        has_pending_permission: false,
        has_pending_question: false,
        in_tentative_state: false,
    };

    /// `check_keybindings` as these tests drive it.
    fn check(ctx: &egui::Context, chord: &mut NormalMode, keys: KeyContext) -> Option<KeyAction> {
        check_keybindings(ctx, chord, keys)
    }

    /// [`detect_sequence`] with `leader` bound in place of the default.
    fn detect_sequence_with(leader: Leader, presses: &[(Modifiers, Key)]) -> Option<KeyAction> {
        detect_sequence_in(KeyContext { leader, ..FRAME }, presses)
    }

    /// [`detect_sequence`] in `frame`.
    fn detect_sequence_in(frame: KeyContext, presses: &[(Modifiers, Key)]) -> Option<KeyAction> {
        // Accumulate: `press_key_modifiers` runs the key-down frame internally
        // and then a key-up frame, so we must not clobber the detection with the
        // later (keys-released) frame's `None`.
        let mut harness = Harness::new_ui_state(
            |ui, (chord, action): &mut (NormalMode, Option<KeyAction>)| {
                if let Some(a) = check(ui.ctx(), chord, frame) {
                    *action = Some(a);
                }
            },
            (NormalMode::default(), None),
        );
        harness.run();
        for (modifiers, key) in presses {
            harness.press_key_modifiers(*modifiers, *key);
        }
        harness.state().1.clone()
    }

    /// Press each `(modifiers, key)` in turn and return how far into a chord
    /// that leaves us: what the which-key strip is handed next frame.
    fn pending_after(presses: &[(Modifiers, Key)]) -> Option<Pending> {
        pending_after_in(FRAME, presses)
    }

    /// [`pending_after`] in `frame`.
    fn pending_after_in(frame: KeyContext, presses: &[(Modifiers, Key)]) -> Option<Pending> {
        let mut harness = Harness::new_ui_state(
            |ui, chord: &mut NormalMode| {
                check(ui.ctx(), chord, frame);
            },
            NormalMode::default(),
        );
        harness.run();
        for (modifiers, key) in presses {
            harness.press_key_modifiers(*modifiers, *key);
        }
        harness.state().view().map(|view| view.pending)
    }

    /// The presses that type a hint's keycap: lowercase is bare, uppercase is
    /// shifted, and the Enter keycap is Enter.
    fn hint_presses(keys: &str) -> Vec<(Modifiers, Key)> {
        if keys == ENTER {
            return vec![(NONE, Key::Enter)];
        }
        keys.chars()
            .map(|c| {
                let key = Key::from_name(&c.to_ascii_uppercase().to_string())
                    .unwrap_or_else(|| panic!("no egui key for {c:?}"));
                let modifiers = if c.is_ascii_uppercase() { SHIFT } else { NONE };
                (modifiers, key)
            })
            .collect()
    }

    const LEADER: (Modifiers, Key) = (Leader::DEFAULT.modifiers, Leader::DEFAULT.key);
    const NONE: Modifiers = Modifiers::NONE;
    const SHIFT: Modifiers = Modifiers::SHIFT;
    const CTRL_SHIFT: Modifiers = Modifiers::CTRL.plus(Modifiers::SHIFT);

    #[test]
    fn leader_j_moves_the_cursor_down() {
        assert_eq!(
            detect_sequence(&[LEADER, (NONE, Key::J)]),
            Some(KeyAction::BlockCursorDown),
        );
    }

    #[test]
    fn leader_z_shift_r_expands_all() {
        assert_eq!(
            detect_sequence(&[LEADER, (NONE, Key::Z), (SHIFT, Key::R)]),
            Some(KeyAction::BlockExpandAll),
        );
    }

    #[test]
    fn leader_g_g_moves_to_the_first_block() {
        assert_eq!(
            detect_sequence(&[LEADER, (NONE, Key::G), (NONE, Key::G)]),
            Some(KeyAction::BlockCursorFirst),
        );
    }

    #[test]
    fn the_chord_stays_open_so_motions_repeat() {
        assert_eq!(
            detect_sequence(&[LEADER, (NONE, Key::J), (NONE, Key::K)]),
            Some(KeyAction::BlockCursorUp),
        );
    }

    #[test]
    fn pending_tracks_the_chord() {
        assert_eq!(pending_after(&[]), None);
        assert_eq!(pending_after(&[LEADER]), Some(Pending::Leader));
        assert_eq!(
            pending_after(&[LEADER, (NONE, Key::Z)]),
            Some(Pending::LeaderZ)
        );
        assert_eq!(
            pending_after(&[LEADER, (NONE, Key::G)]),
            Some(Pending::LeaderG)
        );
        assert_eq!(
            pending_after(&[LEADER, (NONE, Key::Z), (NONE, Key::A)]),
            Some(Pending::Leader),
            "a finished command drops back to the leader, so the strip stays up"
        );
        assert_eq!(pending_after(&[LEADER, (NONE, Key::Q)]), None);
        assert_eq!(pending_after(&[LEADER, (NONE, Key::X)]), None);
    }

    /// The which-key strip must never advertise a key the chord would reject,
    /// or promise the wrong action.
    #[test]
    fn every_hint_does_what_it_says() {
        const H: (Modifiers, Key) = (NONE, Key::H);
        type Presses = &'static [(Modifiers, Key)];
        let prefixes: [(Pane, Pending, Presses); 11] = [
            (Pane::Chat, Pending::Leader, &[LEADER]),
            (Pane::Chat, Pending::LeaderZ, &[LEADER, (NONE, Key::Z)]),
            (Pane::Chat, Pending::LeaderG, &[LEADER, (NONE, Key::G)]),
            (Pane::Chat, Pending::LeaderD, &[LEADER, (NONE, Key::D)]),
            (
                Pane::Chat,
                Pending::LeaderCloseBracket,
                &[LEADER, (NONE, Key::CloseBracket)],
            ),
            (
                Pane::Chat,
                Pending::LeaderOpenBracket,
                &[LEADER, (NONE, Key::OpenBracket)],
            ),
            (Pane::Sessions, Pending::Leader, &[LEADER, H]),
            (
                Pane::Sessions,
                Pending::LeaderG,
                &[LEADER, H, (NONE, Key::G)],
            ),
            (
                Pane::Sessions,
                Pending::LeaderD,
                &[LEADER, H, (NONE, Key::D)],
            ),
            (
                Pane::Sessions,
                Pending::LeaderCloseBracket,
                &[LEADER, H, (NONE, Key::CloseBracket)],
            ),
            (
                Pane::Sessions,
                Pending::LeaderOpenBracket,
                &[LEADER, H, (NONE, Key::OpenBracket)],
            ),
        ];
        for (pane, pending, prefix) in prefixes {
            let view = ChordView {
                pending,
                pane,
                sessions_shown: true,
                agentic: true,
                interruptible: true,
            };
            assert_eq!(pending_after(prefix), Some(pending), "{pane:?} {prefix:?}");
            let groups = view.hints().iter().chain(view.session_keys());
            for hint in groups.flat_map(|group| group.iter()) {
                let mut presses = prefix.to_vec();
                presses.extend(hint_presses(hint.keys));
                assert_eq!(
                    detect_sequence(&presses),
                    Some(hint.action.clone()),
                    "{pane:?} {pending:?} hint {:?}",
                    hint.keys
                );
            }
        }
    }

    #[test]
    fn leader_h_j_switches_sessions_instead_of_moving_the_cursor() {
        assert_eq!(
            detect_sequence(&[LEADER, (NONE, Key::H), (NONE, Key::J)]),
            Some(KeyAction::SessionPaneNext),
        );
        assert_eq!(
            detect_sequence(&[LEADER, (NONE, Key::H), (SHIFT, Key::G)]),
            Some(KeyAction::SessionPaneLast),
        );
    }

    #[test]
    fn leader_h_l_j_is_back_in_the_chat() {
        assert_eq!(
            detect_sequence(&[LEADER, (NONE, Key::H), (NONE, Key::L), (NONE, Key::J)]),
            Some(KeyAction::BlockCursorDown),
        );
    }

    #[test]
    fn every_leader_starts_in_the_chat() {
        assert_eq!(
            detect_sequence(&[
                LEADER,
                (NONE, Key::H),
                (NONE, Key::Escape),
                LEADER,
                (NONE, Key::J)
            ]),
            Some(KeyAction::BlockCursorDown),
        );
        assert_eq!(
            detect_sequence(&[LEADER, (NONE, Key::H), LEADER, (NONE, Key::J)]),
            Some(KeyAction::BlockCursorDown),
            "the leader mid-chord starts a fresh one",
        );
    }

    #[test]
    fn enter_from_the_session_list_ends_the_chord() {
        assert_eq!(
            detect_sequence(&[LEADER, (NONE, Key::H), (NONE, Key::Enter)]),
            Some(KeyAction::FocusChatPane),
        );
        assert_eq!(
            pending_after(&[LEADER, (NONE, Key::H), (NONE, Key::Enter)]),
            None
        );
    }

    #[test]
    fn h_without_a_session_list_does_nothing() {
        let presses = [LEADER, (NONE, Key::H), (NONE, Key::J)];
        assert_eq!(
            detect_sequence_in(
                KeyContext {
                    sessions_shown: false,
                    ..FRAME
                },
                &presses
            ),
            Some(KeyAction::BlockCursorDown),
            "h is swallowed, the chord stays open and in the chat",
        );
    }

    #[test]
    fn rename_and_new_agent_end_the_chord() {
        assert_eq!(pending_after(&[LEADER, (NONE, Key::R)]), None);
        assert_eq!(pending_after(&[LEADER, (NONE, Key::N)]), None);
        assert_eq!(
            pending_after(&[LEADER, (NONE, Key::D), (NONE, Key::D)]),
            Some(Pending::Leader),
            "dd keeps the chord, so you can walk on to the next session",
        );
    }

    #[test]
    fn escape_with_a_chord_pending_only_cancels_the_chord() {
        assert_eq!(detect_sequence(&[LEADER, (NONE, Key::Escape)]), None);
    }

    #[test]
    fn leader_s_stops_the_running_turn_from_either_pane() {
        assert_eq!(
            detect_sequence(&[LEADER, (NONE, Key::S)]),
            Some(KeyAction::Interrupt),
        );
        assert_eq!(
            detect_sequence(&[LEADER, (NONE, Key::H), (NONE, Key::S)]),
            Some(KeyAction::Interrupt),
        );
        assert_eq!(
            pending_after(&[LEADER, (NONE, Key::S)]),
            Some(Pending::Leader),
            "s keeps the chord open, like the other session keys",
        );
    }

    #[test]
    fn leader_s_with_nothing_running_is_swallowed() {
        let idle = KeyContext {
            interruptible: false,
            ..FRAME
        };
        assert_eq!(detect_sequence_in(idle, &[LEADER, (NONE, Key::S)]), None);
        assert_eq!(
            pending_after_in(idle, &[LEADER, (NONE, Key::S)]),
            Some(Pending::Leader),
            "the chord stays open",
        );
    }

    #[test]
    fn escape_with_no_chord_does_not_interrupt() {
        assert_eq!(detect(NONE, Key::Escape), None);
    }

    #[test]
    fn escape_in_the_tentative_state_still_cancels_it() {
        let mut harness = Harness::new_ui_state(
            |ui, action: &mut Option<KeyAction>| {
                let mut chord = NormalMode::default();
                let keys = KeyContext {
                    has_pending_permission: true,
                    in_tentative_state: true,
                    ..FRAME
                };
                if let Some(a) = check_keybindings(ui.ctx(), &mut chord, keys) {
                    *action = Some(a);
                }
            },
            None,
        );
        harness.run();
        harness.press_key_modifiers(NONE, Key::Escape);
        assert_eq!(harness.state(), &Some(KeyAction::CancelTentative));
    }

    #[test]
    fn a_cancelled_chord_stops_reading_bare_keys() {
        assert_eq!(
            detect_sequence(&[LEADER, (NONE, Key::Escape), (NONE, Key::J)]),
            None,
        );
        assert_eq!(
            detect_sequence(&[LEADER, (NONE, Key::X), (NONE, Key::J)]),
            None,
            "an unknown key cancels the chord too",
        );
    }

    #[test]
    fn a_chord_stays_open_while_idle() {
        let mut harness = Harness::new_ui_state(
            |ui, (chord, action): &mut (NormalMode, Option<KeyAction>)| {
                if let Some(a) = check(ui.ctx(), chord, FRAME) {
                    *action = Some(a);
                }
            },
            (NormalMode::default(), None),
        );
        harness.run();
        harness.press_key_modifiers(LEADER.0, LEADER.1);
        // Each step is a quarter second of simulated time: a long pause to
        // read a block.
        for _ in 0..40 {
            harness.step();
        }
        harness.press_key_modifiers(NONE, Key::J);
        assert_eq!(
            harness.state().1,
            Some(KeyAction::BlockCursorDown),
            "j after a pause still moves the cursor",
        );
    }

    #[test]
    fn ctrl_shift_n_moves_the_cursor_down() {
        assert_eq!(detect(CTRL_SHIFT, Key::N), Some(KeyAction::BlockCursorDown));
    }

    #[test]
    fn bare_j_with_no_chord_does_nothing() {
        assert_eq!(detect(NONE, Key::J), None);
    }

    #[test]
    fn the_chord_hands_focus_back_when_it_ends() {
        let input_id = egui::Id::unique("chat_input");
        // A real text field: egui drops focus from an id no widget claims.
        let mut harness = Harness::new_ui_state(
            |ui, (chord, text): &mut (NormalMode, String)| {
                check(ui.ctx(), chord, FRAME);
                ui.add(egui::TextEdit::singleline(text).id(input_id))
                    .accessible_name("test field");
            },
            (NormalMode::default(), String::new()),
        );
        // A real keypress carries its text alongside the key event.
        let type_key = |harness: &mut Harness<'_, (NormalMode, String)>, key: Key, text: &str| {
            harness
                .input_mut()
                .events
                .push(egui::Event::Text(text.to_owned()));
            harness.press_key_modifiers(NONE, key);
        };
        harness.ctx.memory_mut(|m| m.request_focus(input_id));
        harness.run();

        harness.press_key_modifiers(LEADER.0, LEADER.1);
        assert_eq!(
            harness.ctx.memory(|m| m.focused()),
            None,
            "the leader sets the input's focus aside"
        );

        type_key(&mut harness, Key::J, "j");
        assert_eq!(harness.state().1, "", "a chord key is a command, not text");

        // An unknown key ends the chord and hands focus back in the same frame,
        // so its own text must not follow the focus into the input.
        type_key(&mut harness, Key::X, "x");
        assert_eq!(harness.ctx.memory(|m| m.focused()), Some(input_id));
        assert_eq!(harness.state().1, "", "the cancelling key is dropped");

        type_key(&mut harness, Key::H, "h");
        assert_eq!(harness.state().1, "h", "typing resumes after the chord");
    }

    #[test]
    fn escape_ends_the_chord_and_hands_focus_back() {
        let input_id = egui::Id::unique("chat_input");
        let mut harness = Harness::new_ui_state(
            |ui, (chord, text): &mut (NormalMode, String)| {
                check(ui.ctx(), chord, FRAME);
                ui.add(egui::TextEdit::singleline(text).id(input_id))
                    .accessible_name("test field");
            },
            (NormalMode::default(), String::new()),
        );
        harness.ctx.memory_mut(|m| m.request_focus(input_id));
        harness.run();

        harness.press_key_modifiers(LEADER.0, LEADER.1);
        harness.press_key_modifiers(NONE, Key::Escape);
        assert_eq!(harness.ctx.memory(|m| m.focused()), Some(input_id));
    }

    #[test]
    fn the_default_leader_is_the_persisted_default() {
        assert_eq!(Leader::resolve(&LeaderKey::default()), Leader::DEFAULT);
    }

    #[test]
    fn a_rebound_leader_opens_the_chord_and_the_old_one_does_not() {
        let alt_x = Leader::resolve(&LeaderKey::from_press(Modifiers::ALT, Key::X));
        assert_eq!(
            detect_sequence_with(alt_x, &[(Modifiers::ALT, Key::X), (NONE, Key::J)]),
            Some(KeyAction::BlockCursorDown),
        );
        assert_eq!(detect_sequence_with(alt_x, &[LEADER, (NONE, Key::J)]), None);
    }

    #[test]
    fn a_leader_key_round_trips_through_its_name() {
        let leader = LeaderKey::from_press(Modifiers::CTRL | Modifiers::SHIFT, Key::OpenBracket);
        let json = serde_json::to_string(&leader).unwrap();
        let back: LeaderKey = serde_json::from_str(&json).unwrap();
        assert_eq!(back.to_string(), "Ctrl+Shift+[");
        assert_eq!(
            Leader::resolve(&back),
            Leader {
                modifiers: Modifiers::CTRL | Modifiers::SHIFT,
                key: Key::OpenBracket,
            }
        );
    }

    #[test]
    fn an_unknown_leader_name_falls_back_to_the_default() {
        let bogus = LeaderKey {
            key: "NotAKey".to_owned(),
            ..LeaderKey::default()
        };
        assert_eq!(Leader::resolve(&bogus), Leader::DEFAULT);
    }

    #[test]
    fn ctrl_m_cycles_permission_mode() {
        assert_eq!(
            detect(Modifiers::CTRL, Key::M),
            Some(KeyAction::CyclePermissionMode),
        );
    }
}
