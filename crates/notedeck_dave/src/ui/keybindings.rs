use crate::config::AiMode;
use egui::Key;

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
    /// Stop the active session's running turn (normal s), the same
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
    /// Move the block cursor to the next collapsible block (normal j, Ctrl+Shift+N)
    BlockCursorDown,
    /// Move the block cursor to the previous collapsible block (normal k, Ctrl+Shift+P)
    BlockCursorUp,
    /// Move the block cursor to the first collapsible block (normal gg)
    BlockCursorFirst,
    /// Move the block cursor to the last collapsible block (normal G)
    BlockCursorLast,
    /// Flip the block under the cursor (normal za / o, Ctrl+Shift+O)
    BlockToggle,
    /// Expand the block under the cursor (normal zo)
    BlockOpen,
    /// Collapse the block under the cursor (normal zc)
    BlockClose,
    /// Expand every collapsible block (normal zR, Ctrl+Shift+E)
    BlockExpandAll,
    /// Collapse every collapsible block (normal zM, Ctrl+Shift+M)
    BlockCollapseAll,
    /// Drop the block cursor, so the transcript follows new output again (normal q)
    BlockCursorClear,
    /// Point normal mode's motions at the session list (normal h)
    FocusSessionsPane,
    /// Point normal mode's motions back at the chat (normal l, or Enter from the session list)
    FocusChatPane,
    /// Switch to the next session without focusing its input (normal h j)
    SessionPaneNext,
    /// Switch to the previous session without focusing its input (normal h k)
    SessionPanePrev,
    /// Switch to the first session in the list (normal h gg)
    SessionPaneFirst,
    /// Switch to the last session in the list (normal h G)
    SessionPaneLast,
    /// Leave normal mode for the chat input (`i` / `a`)
    InsertMode,
}

impl KeyAction {
    /// Actions that only mean something for agentic sessions. The Ctrl ladder
    /// gates their bindings on `is_agentic`, and normal mode follows suit.
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
}

/// Dave's vim-style normal mode, and how far into a chord it is.
///
/// Owned by [`Dave`](crate::Dave) and passed `&mut` into [`check_keybindings`]
/// each frame. Insert mode is the default: the chat input has focus and you
/// type. Esc enters normal mode, which sets the input's focus aside so bare
/// keys (`j`, `z`, `g`, …) are read as commands rather than typed.
///
/// Normal mode stays on after a command so motions repeat — `j j j za` — and
/// never times out; the which-key strip shows it is on. A key it doesn't know
/// is swallowed, and a modified key (a Ctrl binding) goes to the regular
/// bindings, without leaving the mode either way. It ends on `i` / `a` / `q`
/// (focus back to the input), on a tentative permission answer (the input
/// takes its message), on `n` / `r` (the picker or rename field takes the
/// keyboard), when you click into a text field, or when an overlay opens.
/// Esc cancels a half-typed prefix; at the root it cancels a waiting
/// tentative answer, and otherwise is left for chrome, which toggles the side
/// menu. A held Esc's auto-repeats are swallowed there, so holding it doesn't
/// flicker the menu.
///
/// `h` / `l` point the motions at the session list or the chat ([`Pane`]):
/// `j` / `k` walk blocks in the chat and sessions in the list.
#[derive(Default)]
pub struct NormalMode {
    pending: Option<Pending>,
    /// Which pane the motions move through. Back to [`Pane::Chat`] each time
    /// normal mode is entered.
    pane: Pane,
    /// Whether the session list is on screen, as of the last frame. `h` is a
    /// no-op without it.
    sessions_shown: bool,
    /// Whether the active session is agentic, as of the last frame.
    agentic: bool,
    /// Whether the active session has a running turn to stop, as of the last
    /// frame.
    interruptible: bool,
    /// Normal mode ended wanting the active input focused; taken by
    /// [`Self::take_input_focus`].
    input_focus_due: bool,
}

/// How far into a chord normal mode is. Read by the which-key strip through
/// [`NormalMode::view`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pending {
    /// Normal mode's root: waiting for a command or a prefix.
    Root,
    /// `z`: waiting for `a` / `o` / `c` / `R` / `M`.
    Z,
    /// `g`: waiting for the second `g`.
    G,
    /// `d`: waiting for the second `d`.
    D,
    /// `]`: waiting for `q`.
    CloseBracket,
    /// `[`: waiting for `q`.
    OpenBracket,
}

/// Which part of Dave normal mode's motions move through.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pane {
    /// The session list: `j` / `k` switch sessions.
    Sessions,
    /// The chat transcript: `j` / `k` walk its collapsible blocks.
    #[default]
    Chat,
}

/// Normal mode as the UI sees it: the which-key strip reads its hints, and the
/// session list marks its row while the motions are in [`Pane::Sessions`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NormalView {
    pub pending: Pending,
    pub pane: Pane,
    /// `h` can reach the session list.
    pub sessions_shown: bool,
    /// The agentic-only keys (`c`, `v`, `m`, `]q`, `[q`) apply.
    pub agentic: bool,
    /// `s` has a running turn to stop.
    pub interruptible: bool,
}

/// One continuation normal mode accepts, as the which-key strip shows it.
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

/// Session keys, from either pane: normal mode's names for Ctrl bindings.
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

/// Back to insert mode, from either pane.
const INSERT: &[ChordHint] = &[
    hint("i", KeyAction::InsertMode),
    hint("a", KeyAction::InsertMode),
];

/// Focus-queue keys, from either pane (vim's quickfix `]q` / `[q`).
const FOCUS_QUEUE: &[ChordHint] = &[
    hint("]q", KeyAction::FocusQueueNext),
    hint("[q", KeyAction::FocusQueuePrev),
];

/// What normal mode's root accepts in the chat pane.
const CHAT_ROOT_HINTS: &[&[ChordHint]] = &[
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
    // Both leave normal mode, so they share a group.
    &[
        hint("q", KeyAction::BlockCursorClear),
        hint("i", KeyAction::InsertMode),
        hint("a", KeyAction::InsertMode),
    ],
];

/// The keycap for Enter.
const ENTER: &str = "\u{21b5}";

/// What normal mode's root accepts in the sessions pane.
const SESSIONS_ROOT_HINTS: &[&[ChordHint]] = &[
    SESSION_MOTIONS,
    &[
        hint("l", KeyAction::FocusChatPane),
        hint(ENTER, KeyAction::FocusChatPane),
    ],
    INSERT,
];

/// What normal mode's root accepts in either pane besides the pane's own keys: the
/// strip gives them a row of their own.
const SESSION_KEYS: &[&[ChordHint]] = &[SESSION_LIFECYCLE, SESSION_VIEW, SESSION_TURN, FOCUS_QUEUE];

/// What `z` accepts.
const Z_HINTS: &[&[ChordHint]] = &[
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

/// What `g` accepts in the chat pane.
const CHAT_G_HINTS: &[&[ChordHint]] = &[&[hint("g", KeyAction::BlockCursorFirst)]];

/// What `g` accepts in the sessions pane.
const SESSIONS_G_HINTS: &[&[ChordHint]] = &[&[hint("g", KeyAction::SessionPaneFirst)]];

/// What `d` accepts.
const D_HINTS: &[&[ChordHint]] = &[&[hint("d", KeyAction::DeleteActiveSession)]];

/// What `]` accepts.
const CLOSE_BRACKET_HINTS: &[&[ChordHint]] = &[&[hint("q", KeyAction::FocusQueueNext)]];

/// What `[` accepts.
const OPEN_BRACKET_HINTS: &[&[ChordHint]] = &[&[hint("q", KeyAction::FocusQueuePrev)]];

impl NormalView {
    /// The pane's keys this state could accept, in groups the strip spaces
    /// apart; [`Self::session_keys`] has the rest. Filter through
    /// [`Self::offers`]: a group may hold keys that do nothing this frame.
    ///
    /// Mirrors the match in `check_normal_mode`; `every_hint_does_what_it_says`
    /// keeps the two from drifting.
    pub fn hints(self) -> &'static [&'static [ChordHint]] {
        match (self.pane, self.pending) {
            (Pane::Chat, Pending::Root) => CHAT_ROOT_HINTS,
            (Pane::Sessions, Pending::Root) => SESSIONS_ROOT_HINTS,
            (_, Pending::Z) => Z_HINTS,
            (Pane::Chat, Pending::G) => CHAT_G_HINTS,
            (Pane::Sessions, Pending::G) => SESSIONS_G_HINTS,
            (_, Pending::D) => D_HINTS,
            (_, Pending::CloseBracket) => CLOSE_BRACKET_HINTS,
            (_, Pending::OpenBracket) => OPEN_BRACKET_HINTS,
        }
    }

    /// The session keys this state accepts, from either pane: empty past the
    /// root.
    pub fn session_keys(self) -> &'static [&'static [ChordHint]] {
        match self.pending {
            Pending::Root => SESSION_KEYS,
            _ => &[],
        }
    }

    /// Whether `action` does anything this frame: `h` needs the session list
    /// on screen, `s` a running turn, and the agentic-only keys an agentic
    /// session. A key that doesn't is swallowed without leaving normal mode, and
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

/// What normal mode made of this frame's input.
enum NormalStep {
    /// Normal mode is off, or this frame's key is a modified one or a bare key
    /// the regular bindings own: they should look at this frame.
    FallThrough,
    /// Normal mode owned this frame's keys, and maybe produced an action.
    Consumed(Option<KeyAction>),
    /// Esc at normal mode's root: nothing in Dave touches it, so it reaches
    /// chrome's fallback (the side menu) and normal mode stays on.
    LeaveForChrome,
}

impl NormalMode {
    /// Normal mode as the UI sees it, or `None` in insert mode.
    pub fn view(&self) -> Option<NormalView> {
        self.pending.map(|pending| self.view_at(pending))
    }

    /// Normal mode's view, at `pending`.
    fn view_at(&self, pending: Pending) -> NormalView {
        NormalView {
            pending,
            pane: self.pane,
            sessions_shown: self.sessions_shown,
            agentic: self.agentic,
            interruptible: self.interruptible,
        }
    }

    /// Whether normal mode just ended wanting the active session's input
    /// focused. Reading it clears it, so it is acted on once.
    pub fn take_input_focus(&mut self) -> bool {
        std::mem::take(&mut self.input_focus_due)
    }

    /// A regular binding ran while normal mode was on: a Ctrl binding, or a
    /// bare key normal mode let through (a permission answer). It stays on,
    /// like vim, unless the binding opened something you type into: those
    /// leave it the way their normal-mode keys do. A tentative permission
    /// answer (`!`, Shift+2, Shift+3) waits for a message typed into the
    /// input, so it hands the input focus.
    fn after_modified(&mut self, action: &KeyAction) {
        match action {
            KeyAction::NewAgent | KeyAction::RenameAgent => self.release(),
            KeyAction::OpenExternalEditor
            | KeyAction::TentativeAccept
            | KeyAction::TentativeDeny
            | KeyAction::TentativeAllowAlways => self.end(),
            _ => {}
        }
    }

    /// Record what this frame offers normal mode. With the session list off
    /// screen (a narrow layout, the scene view) it falls back to the chat.
    fn observe(&mut self, sessions_shown: bool, agentic: bool, interruptible: bool) {
        self.sessions_shown = sessions_shown;
        self.agentic = agentic;
        self.interruptible = interruptible;
        if !sessions_shown {
            self.pane = Pane::Chat;
        }
    }

    /// Whether normal mode is on.
    fn is_on(&self) -> bool {
        self.pending.is_some()
    }

    /// Enter normal mode. egui has already dropped focus on the Esc that got
    /// us here; anything still focused is set aside so bare keys reach us.
    fn open(&mut self, ctx: &egui::Context) {
        ctx.memory_mut(|m| {
            if let Some(id) = m.focused() {
                m.surrender_focus(id);
            }
        });
        self.pane = Pane::Chat;
        self.pending = Some(Pending::Root);
    }

    /// Back to insert mode: focus the active session's input. There is no
    /// earlier focus to hand back, since the Esc that entered normal mode
    /// dropped it.
    fn end(&mut self) {
        self.pending = None;
        self.input_focus_due = true;
    }

    /// Leave normal mode and leave focus alone: the action opens something
    /// that takes typing (a rename field, the new-agent picker) and claims
    /// focus itself, or the user clicked into a text field.
    fn release(&mut self) {
        self.pending = None;
    }
}

/// Where normal mode goes after a key.
enum Then {
    /// Stay open, waiting in this state.
    Continue(Pending),
    /// End, handing focus back.
    End,
    /// End without touching focus (see [`NormalMode::release`]).
    Release,
}

/// A key press, as normal mode reads it.
#[derive(Clone, Copy)]
struct KeyPress {
    key: Key,
    shift: bool,
    /// Ctrl / Alt / Cmd held — never part of a chord, so the regular
    /// bindings get it.
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

/// Bare keys Dave already reads while the chat input is unfocused, which is
/// all of normal mode: the permission answers (`1` / `2` / `3`, `!`), a
/// question's options (`1`-`9`, Enter) and delete-session. Normal mode lets
/// them through rather than swallowing them as unknown.
fn owned_bare_key(key: Key) -> bool {
    matches!(
        key,
        Key::Num0
            | Key::Num1
            | Key::Num2
            | Key::Num3
            | Key::Num4
            | Key::Num5
            | Key::Num6
            | Key::Num7
            | Key::Num8
            | Key::Num9
            | Key::Exclamationmark
            | Key::Enter
            | Key::Delete
    )
}

/// Whether this frame's Esc presses are all auto-repeats of a held Esc.
fn escape_is_repeat(input: &egui::InputState) -> bool {
    input.events.iter().all(|event| match event {
        egui::Event::Key {
            key: Key::Escape,
            pressed: true,
            repeat,
            ..
        } => *repeat,
        _ => true,
    })
}

/// Feed this frame's input to normal mode, if it is on. `tentative`: a
/// tentative permission answer is waiting for its message, and Esc at the
/// root cancels it.
fn check_normal_mode(ctx: &egui::Context, mode: &mut NormalMode, tentative: bool) -> NormalStep {
    let Some(pending) = mode.pending else {
        return NormalStep::FallThrough;
    };

    // Normal mode set focus aside and holds focus requests until it ends, so
    // a focused widget means the user clicked into a text field: they are
    // typing now.
    if ctx.memory(|m| m.focused()).is_some() {
        mode.release();
        return NormalStep::FallThrough;
    }

    // Esc cancels a half-typed prefix. At the root it cancels a tentative
    // answer (the regular bindings' Esc), or else it's chrome's (the side
    // menu); normal mode stays on either way.
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        if pending == Pending::Root && tentative {
            return NormalStep::FallThrough;
        }
        // Chrome toggles its menu on every Esc it sees, auto-repeats
        // included, so only a fresh press reaches it.
        if pending == Pending::Root && !ctx.input(escape_is_repeat) {
            return NormalStep::LeaveForChrome;
        }
        ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
        mode.pending = Some(Pending::Root);
        return NormalStep::Consumed(None);
    }

    let Some(press) = ctx.input(first_key_press) else {
        return NormalStep::Consumed(None);
    };

    // A modified key is somebody else's binding (a Ctrl alias): the regular
    // ladder has it, and normal mode stays on.
    if press.modified {
        return NormalStep::FallThrough;
    }

    use KeyAction as A;
    use Then::{Continue, End, Release};
    let (then, action) = match (mode.pane, pending, press.key, press.shift) {
        // Motions: the same keys walk blocks in the chat, sessions in the list.
        (Pane::Chat, Pending::Root, Key::J, false) => {
            (Continue(Pending::Root), Some(A::BlockCursorDown))
        }
        (Pane::Chat, Pending::Root, Key::K, false) => {
            (Continue(Pending::Root), Some(A::BlockCursorUp))
        }
        (Pane::Chat, Pending::Root, Key::G, true) => {
            (Continue(Pending::Root), Some(A::BlockCursorLast))
        }
        (Pane::Chat, Pending::G, Key::G, false) => {
            (Continue(Pending::Root), Some(A::BlockCursorFirst))
        }
        (Pane::Sessions, Pending::Root, Key::J, false) => {
            (Continue(Pending::Root), Some(A::SessionPaneNext))
        }
        (Pane::Sessions, Pending::Root, Key::K, false) => {
            (Continue(Pending::Root), Some(A::SessionPanePrev))
        }
        (Pane::Sessions, Pending::Root, Key::G, true) => {
            (Continue(Pending::Root), Some(A::SessionPaneLast))
        }
        (Pane::Sessions, Pending::G, Key::G, false) => {
            (Continue(Pending::Root), Some(A::SessionPaneFirst))
        }
        (_, Pending::Root, Key::G, false) => (Continue(Pending::G), None),

        // Panes. Enter from the list goes back to the chat and hands it the
        // keyboard: you've picked the session you wanted.
        (_, Pending::Root, Key::H, false) => (Continue(Pending::Root), Some(A::FocusSessionsPane)),
        (_, Pending::Root, Key::L, false) => (Continue(Pending::Root), Some(A::FocusChatPane)),
        (Pane::Sessions, Pending::Root, Key::Enter, false) => (End, Some(A::FocusChatPane)),

        // Folds, in the chat only.
        (Pane::Chat, Pending::Root, Key::Z, false) => (Continue(Pending::Z), None),
        (Pane::Chat, Pending::Root, Key::O, false) => {
            (Continue(Pending::Root), Some(A::BlockToggle))
        }
        (Pane::Chat, Pending::Root, Key::Q, false) => (End, Some(A::BlockCursorClear)),

        // Back to insert mode, from either pane.
        (_, Pending::Root, Key::I | Key::A, false) => (End, Some(A::InsertMode)),
        (_, Pending::Z, Key::A, false) => (Continue(Pending::Root), Some(A::BlockToggle)),
        (_, Pending::Z, Key::O, false) => (Continue(Pending::Root), Some(A::BlockOpen)),
        (_, Pending::Z, Key::C, false) => (Continue(Pending::Root), Some(A::BlockClose)),
        (_, Pending::Z, Key::R, true) => (Continue(Pending::Root), Some(A::BlockExpandAll)),
        (_, Pending::Z, Key::M, true) => (Continue(Pending::Root), Some(A::BlockCollapseAll)),

        // Session keys, from either pane. New-agent and rename open something
        // you type into, so they leave normal mode; so does the external editor.
        (_, Pending::Root, Key::N, false) => (Release, Some(A::NewAgent)),
        (_, Pending::Root, Key::C, false) => (Continue(Pending::Root), Some(A::CloneAgent)),
        (_, Pending::Root, Key::R, false) => (Release, Some(A::RenameAgent)),
        (_, Pending::Root, Key::D, false) => (Continue(Pending::D), None),
        (_, Pending::D, Key::D, false) => (Continue(Pending::Root), Some(A::DeleteActiveSession)),
        (_, Pending::Root, Key::V, false) => (Continue(Pending::Root), Some(A::ToggleView)),
        (_, Pending::Root, Key::M, false) => {
            (Continue(Pending::Root), Some(A::CyclePermissionMode))
        }
        (_, Pending::Root, Key::E, false) => (End, Some(A::OpenExternalEditor)),
        (_, Pending::Root, Key::S, false) => (Continue(Pending::Root), Some(A::Interrupt)),
        (_, Pending::Root, Key::CloseBracket, false) => (Continue(Pending::CloseBracket), None),
        (_, Pending::Root, Key::OpenBracket, false) => (Continue(Pending::OpenBracket), None),
        (_, Pending::CloseBracket, Key::Q, false) => {
            (Continue(Pending::Root), Some(A::FocusQueueNext))
        }
        (_, Pending::OpenBracket, Key::Q, false) => {
            (Continue(Pending::Root), Some(A::FocusQueuePrev))
        }

        // Keys the regular bindings read while the input is unfocused.
        (_, _, key, _) if owned_bare_key(key) => {
            mode.pending = Some(Pending::Root);
            return NormalStep::FallThrough;
        }

        // Anything else is swallowed, and drops a half-typed prefix.
        _ => (Continue(Pending::Root), None),
    };

    // Swallow this frame's keys and text so a bare key never leaks into the
    // input or anything else reading keys.
    ctx.input_mut(|i| {
        i.events
            .retain(|e| !matches!(e, egui::Event::Key { .. } | egui::Event::Text(_)))
    });

    // A key that does nothing here (`h` with no session list, `v` in a chat
    // session) is swallowed and normal mode carries on.
    let view = mode.view_at(pending);
    let action = action.filter(|action| view.offers(action));

    match &action {
        Some(A::FocusSessionsPane) => mode.pane = Pane::Sessions,
        Some(A::FocusChatPane) => mode.pane = Pane::Chat,
        _ => {}
    }

    match then {
        Continue(next) => mode.pending = Some(next),
        End => mode.end(),
        Release => mode.release(),
    }
    NormalStep::Consumed(action)
}

/// What a frame offers the keybindings besides the keys themselves, gathered
/// by `Dave::process_keybindings` from the active session and the layout.
#[derive(Clone, Copy, Debug)]
pub struct KeyContext {
    /// The active session's mode; agentic-only bindings need
    /// [`AiMode::Agentic`].
    pub ai_mode: AiMode,
    /// The session list is on screen, for normal mode's `h`.
    pub sessions_shown: bool,
    /// The active session has a running turn, for normal mode's `s`.
    pub interruptible: bool,
    /// A permission request is waiting, for the bare `1` / `2` / `3` keys.
    pub has_pending_permission: bool,
    /// The waiting request is a question set, which takes the number keys
    /// itself.
    pub has_pending_question: bool,
    /// A tentative accept/deny is waiting for its message; Esc cancels it.
    pub in_tentative_state: bool,
    /// An overlay (settings, a picker) covers the chat. It owns Esc, and
    /// normal mode, whose keys are the chat's, ends.
    pub overlay_open: bool,
    /// The inline session rename field has, or just lost, focus: its Esc
    /// cancels the rename rather than entering normal mode.
    pub renaming: bool,
}

/// Check for keybinding actions.
/// Most keybindings use Ctrl modifier to avoid conflicts with text input.
/// Exceptions: the permission answers (1/2/3, Shift+1/2/3), a question's
/// options and Delete are bare keys, read only while no text input has focus —
/// in normal mode, which sets the input's focus aside.
/// In Chat mode, agentic-specific keybindings (scene view, plan mode, focus queue) are disabled.
///
/// `mode` carries normal mode across frames: while it is on it owns the bare
/// keys (see [`NormalMode`]). Esc enters it from insert mode unless Esc is
/// someone else's. `keys` is what the frame offers the bindings (see
/// [`KeyContext`]).
pub fn check_keybindings(
    ctx: &egui::Context,
    mode: &mut NormalMode,
    keys: KeyContext,
) -> Option<KeyAction> {
    let KeyContext {
        ai_mode,
        sessions_shown,
        interruptible,
        in_tentative_state,
        overlay_open,
        renaming,
        ..
    } = keys;
    let is_agentic = ai_mode == AiMode::Agentic;
    mode.observe(sessions_shown, is_agentic, interruptible);

    // An overlay is modal: normal mode's keys are the chat's.
    if overlay_open && mode.is_on() {
        mode.release();
    }

    // Normal mode reads bare keys, and its Esc outranks every other Esc but
    // a tentative answer's.
    match check_normal_mode(ctx, mode, is_agentic && in_tentative_state) {
        NormalStep::Consumed(action) => return action,
        NormalStep::LeaveForChrome => return None,
        NormalStep::FallThrough => {}
    }

    // Escape in tentative state cancels the tentative mode (agentic only)
    if is_agentic
        && in_tentative_state
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape))
    {
        return Some(KeyAction::CancelTentative);
    }

    // Esc in insert mode enters normal mode, unless an overlay or the rename
    // field owns it. The keys that follow land next frame.
    if !mode.is_on()
        && !overlay_open
        && !renaming
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape))
    {
        mode.open(ctx);
        return None;
    }

    let action = check_regular_bindings(ctx, keys);
    if let Some(action) = &action {
        if mode.is_on() {
            mode.after_modified(action);
        }
    }
    action
}

/// The Ctrl ladder, and the bare keys Dave reads while the chat input is
/// unfocused.
fn check_regular_bindings(ctx: &egui::Context, keys: KeyContext) -> Option<KeyAction> {
    let KeyContext {
        ai_mode,
        has_pending_permission,
        has_pending_question,
        ..
    } = keys;
    let is_agentic = ai_mode == AiMode::Agentic;
    let ctrl = egui::Modifiers::CTRL;
    let ctrl_shift = egui::Modifiers::CTRL | egui::Modifiers::SHIFT;

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

    // Ctrl+Shift aliases for the block cursor, usable mid-typing without normal mode.
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
        detect_sequence_in(FRAME, presses)
    }

    /// The session list on screen and a turn running, agentic, with no
    /// pending prompts: every normal-mode key applies.
    const FRAME: KeyContext = KeyContext {
        ai_mode: AiMode::Agentic,
        sessions_shown: true,
        interruptible: true,
        has_pending_permission: false,
        has_pending_question: false,
        in_tentative_state: false,
        overlay_open: false,
        renaming: false,
    };

    /// `check_keybindings` as these tests drive it.
    fn check(ctx: &egui::Context, mode: &mut NormalMode, keys: KeyContext) -> Option<KeyAction> {
        check_keybindings(ctx, mode, keys)
    }

    /// [`detect_sequence`] in `frame`.
    fn detect_sequence_in(frame: KeyContext, presses: &[(Modifiers, Key)]) -> Option<KeyAction> {
        // Accumulate: each press runs one frame, and a later press that
        // detects nothing must not clobber an earlier detection with `None`.
        // (Its key-up is queued for the next frame, not run.)
        let mut harness = Harness::new_ui_state(
            |ui, (mode, action): &mut (NormalMode, Option<KeyAction>)| {
                if let Some(a) = check(ui.ctx(), mode, frame) {
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

    /// Press each `(modifiers, key)` in turn and return how far into normal
    /// mode that leaves us: what the which-key strip is handed next frame.
    fn pending_after(presses: &[(Modifiers, Key)]) -> Option<Pending> {
        pending_after_in(FRAME, presses)
    }

    /// [`pending_after`] in `frame`.
    fn pending_after_in(frame: KeyContext, presses: &[(Modifiers, Key)]) -> Option<Pending> {
        let mut harness = Harness::new_ui_state(
            |ui, mode: &mut NormalMode| {
                check(ui.ctx(), mode, frame);
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

    const ESC: (Modifiers, Key) = (Modifiers::NONE, Key::Escape);
    const NONE: Modifiers = Modifiers::NONE;
    const SHIFT: Modifiers = Modifiers::SHIFT;
    const CTRL_SHIFT: Modifiers = Modifiers::CTRL.plus(Modifiers::SHIFT);

    #[test]
    fn normal_j_moves_the_cursor_down() {
        assert_eq!(
            detect_sequence(&[ESC, (NONE, Key::J)]),
            Some(KeyAction::BlockCursorDown),
        );
    }

    #[test]
    fn normal_z_shift_r_expands_all() {
        assert_eq!(
            detect_sequence(&[ESC, (NONE, Key::Z), (SHIFT, Key::R)]),
            Some(KeyAction::BlockExpandAll),
        );
    }

    #[test]
    fn normal_g_g_moves_to_the_first_block() {
        assert_eq!(
            detect_sequence(&[ESC, (NONE, Key::G), (NONE, Key::G)]),
            Some(KeyAction::BlockCursorFirst),
        );
    }

    #[test]
    fn the_chord_stays_open_so_motions_repeat() {
        assert_eq!(
            detect_sequence(&[ESC, (NONE, Key::J), (NONE, Key::K)]),
            Some(KeyAction::BlockCursorUp),
        );
    }

    #[test]
    fn pending_tracks_the_chord() {
        assert_eq!(pending_after(&[]), None);
        assert_eq!(pending_after(&[ESC]), Some(Pending::Root));
        assert_eq!(pending_after(&[ESC, (NONE, Key::Z)]), Some(Pending::Z));
        assert_eq!(pending_after(&[ESC, (NONE, Key::G)]), Some(Pending::G));
        assert_eq!(
            pending_after(&[ESC, (NONE, Key::Z), (NONE, Key::A)]),
            Some(Pending::Root),
            "a finished command drops back to the root, so the strip stays up"
        );
        assert_eq!(pending_after(&[ESC, (NONE, Key::Q)]), None);
        assert_eq!(pending_after(&[ESC, (NONE, Key::I)]), None);
        assert_eq!(
            pending_after(&[ESC, (NONE, Key::X)]),
            Some(Pending::Root),
            "an unknown key is swallowed and normal mode stays on"
        );
        assert_eq!(
            pending_after(&[ESC, (NONE, Key::Z), (NONE, Key::X)]),
            Some(Pending::Root),
            "and drops a half-typed prefix"
        );
    }

    /// The which-key strip must never advertise a key normal mode would reject,
    /// or promise the wrong action.
    #[test]
    fn every_hint_does_what_it_says() {
        const H: (Modifiers, Key) = (NONE, Key::H);
        type Presses = &'static [(Modifiers, Key)];
        let prefixes: [(Pane, Pending, Presses); 11] = [
            (Pane::Chat, Pending::Root, &[ESC]),
            (Pane::Chat, Pending::Z, &[ESC, (NONE, Key::Z)]),
            (Pane::Chat, Pending::G, &[ESC, (NONE, Key::G)]),
            (Pane::Chat, Pending::D, &[ESC, (NONE, Key::D)]),
            (
                Pane::Chat,
                Pending::CloseBracket,
                &[ESC, (NONE, Key::CloseBracket)],
            ),
            (
                Pane::Chat,
                Pending::OpenBracket,
                &[ESC, (NONE, Key::OpenBracket)],
            ),
            (Pane::Sessions, Pending::Root, &[ESC, H]),
            (Pane::Sessions, Pending::G, &[ESC, H, (NONE, Key::G)]),
            (Pane::Sessions, Pending::D, &[ESC, H, (NONE, Key::D)]),
            (
                Pane::Sessions,
                Pending::CloseBracket,
                &[ESC, H, (NONE, Key::CloseBracket)],
            ),
            (
                Pane::Sessions,
                Pending::OpenBracket,
                &[ESC, H, (NONE, Key::OpenBracket)],
            ),
        ];
        for (pane, pending, prefix) in prefixes {
            let view = NormalView {
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
    fn normal_h_j_switches_sessions_instead_of_moving_the_cursor() {
        assert_eq!(
            detect_sequence(&[ESC, (NONE, Key::H), (NONE, Key::J)]),
            Some(KeyAction::SessionPaneNext),
        );
        assert_eq!(
            detect_sequence(&[ESC, (NONE, Key::H), (SHIFT, Key::G)]),
            Some(KeyAction::SessionPaneLast),
        );
    }

    #[test]
    fn normal_h_l_j_is_back_in_the_chat() {
        assert_eq!(
            detect_sequence(&[ESC, (NONE, Key::H), (NONE, Key::L), (NONE, Key::J)]),
            Some(KeyAction::BlockCursorDown),
        );
    }

    #[test]
    fn every_entry_starts_in_the_chat() {
        assert_eq!(
            detect_sequence(&[ESC, (NONE, Key::H), (NONE, Key::I), ESC, (NONE, Key::J)]),
            Some(KeyAction::BlockCursorDown),
        );
        assert_eq!(
            detect_sequence(&[ESC, (NONE, Key::H), ESC, (NONE, Key::J)]),
            Some(KeyAction::SessionPaneNext),
            "Esc at the root leaves normal mode on, so the pane stays",
        );
    }

    #[test]
    fn enter_from_the_session_list_ends_the_chord() {
        assert_eq!(
            detect_sequence(&[ESC, (NONE, Key::H), (NONE, Key::Enter)]),
            Some(KeyAction::FocusChatPane),
        );
        assert_eq!(
            pending_after(&[ESC, (NONE, Key::H), (NONE, Key::Enter)]),
            None
        );
    }

    #[test]
    fn h_without_a_session_list_does_nothing() {
        let presses = [ESC, (NONE, Key::H), (NONE, Key::J)];
        assert_eq!(
            detect_sequence_in(
                KeyContext {
                    sessions_shown: false,
                    ..FRAME
                },
                &presses
            ),
            Some(KeyAction::BlockCursorDown),
            "h is swallowed, normal mode stays on and in the chat",
        );
    }

    #[test]
    fn rename_and_new_agent_end_the_chord() {
        assert_eq!(pending_after(&[ESC, (NONE, Key::R)]), None);
        assert_eq!(pending_after(&[ESC, (NONE, Key::N)]), None);
        assert_eq!(
            pending_after(&[ESC, (NONE, Key::D), (NONE, Key::D)]),
            Some(Pending::Root),
            "dd keeps normal mode on, so you can walk on to the next session",
        );
    }

    #[test]
    fn normal_s_stops_the_running_turn_from_either_pane() {
        assert_eq!(
            detect_sequence(&[ESC, (NONE, Key::S)]),
            Some(KeyAction::Interrupt),
        );
        assert_eq!(
            detect_sequence(&[ESC, (NONE, Key::H), (NONE, Key::S)]),
            Some(KeyAction::Interrupt),
        );
        assert_eq!(
            pending_after(&[ESC, (NONE, Key::S)]),
            Some(Pending::Root),
            "s keeps normal mode on, like the other session keys",
        );
    }

    #[test]
    fn normal_s_with_nothing_running_is_swallowed() {
        let idle = KeyContext {
            interruptible: false,
            ..FRAME
        };
        assert_eq!(detect_sequence_in(idle, &[ESC, (NONE, Key::S)]), None);
        assert_eq!(
            pending_after_in(idle, &[ESC, (NONE, Key::S)]),
            Some(Pending::Root),
            "normal mode stays on",
        );
    }

    #[test]
    fn escape_enters_normal_mode_and_does_not_interrupt() {
        assert_eq!(detect(NONE, Key::Escape), None);
        assert_eq!(pending_after(&[ESC]), Some(Pending::Root));
    }

    #[test]
    fn escape_in_the_tentative_state_still_cancels_it() {
        let tentative = KeyContext {
            has_pending_permission: true,
            in_tentative_state: true,
            ..FRAME
        };
        assert_eq!(
            detect_sequence_in(tentative, &[ESC]),
            Some(KeyAction::CancelTentative)
        );
        assert_eq!(
            pending_after_in(tentative, &[ESC]),
            None,
            "and does not enter normal mode"
        );
    }

    #[test]
    fn insert_mode_stops_reading_bare_keys() {
        for insert in [Key::I, Key::A] {
            assert_eq!(
                detect_sequence(&[ESC, (NONE, insert)]),
                Some(KeyAction::InsertMode),
            );
            assert_eq!(
                detect_sequence(&[ESC, (NONE, insert), (NONE, Key::J)]),
                Some(KeyAction::InsertMode),
                "{insert:?}: j after it is not a motion",
            );
            assert_eq!(pending_after(&[ESC, (NONE, insert), (NONE, Key::J)]), None);
        }
    }

    #[test]
    fn a_chord_stays_open_while_idle() {
        let mut harness = Harness::new_ui_state(
            |ui, (mode, action): &mut (NormalMode, Option<KeyAction>)| {
                if let Some(a) = check(ui.ctx(), mode, FRAME) {
                    *action = Some(a);
                }
            },
            (NormalMode::default(), None),
        );
        harness.run();
        harness.press_key_modifiers(ESC.0, ESC.1);
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

    /// A real keypress carries its text alongside the key event.
    fn type_key<S>(harness: &mut Harness<'_, S>, key: Key, text: &str) {
        harness
            .input_mut()
            .events
            .push(egui::Event::Text(text.to_owned()));
        harness.press_key_modifiers(NONE, key);
    }

    /// A frame with the keybindings in front of a real text field, which
    /// starts out focused like the chat input: egui drops focus from an id
    /// no widget claims. Normal mode's request to focus the input is honoured
    /// the way `settle_normal_mode_focus` does it.
    fn input_harness(
        input_id: egui::Id,
        keys: KeyContext,
    ) -> Harness<'static, (NormalMode, String)> {
        let mut harness = Harness::new_ui_state(
            move |ui, (mode, text): &mut (NormalMode, String)| {
                check(ui.ctx(), mode, keys);
                if mode.take_input_focus() {
                    ui.memory_mut(|m| m.request_focus(input_id));
                }
                ui.add(egui::TextEdit::singleline(text).id(input_id))
                    .accessible_name("test field");
            },
            (NormalMode::default(), String::new()),
        );
        harness.ctx.memory_mut(|m| m.request_focus(input_id));
        harness.run();
        harness
    }

    #[test]
    fn normal_mode_hands_focus_back_on_i() {
        let input_id = egui::Id::unique("chat_input");
        let mut harness = input_harness(input_id, FRAME);

        harness.press_key_modifiers(ESC.0, ESC.1);
        assert_eq!(
            harness.ctx.memory(|m| m.focused()),
            None,
            "Esc sets the input's focus aside"
        );

        type_key(&mut harness, Key::J, "j");
        assert_eq!(
            harness.state().1,
            "",
            "a normal-mode key is a command, not text"
        );

        type_key(&mut harness, Key::W, "w");
        assert_eq!(harness.ctx.memory(|m| m.focused()), None);
        assert_eq!(harness.state().1, "", "an unknown key is swallowed");
        assert!(harness.state().0.is_on(), "and normal mode stays on");

        // `i` ends normal mode and hands focus back in the same frame, so its
        // own text must not follow the focus into the input.
        type_key(&mut harness, Key::I, "i");
        assert_eq!(harness.ctx.memory(|m| m.focused()), Some(input_id));
        assert_eq!(harness.state().1, "", "the i is a command");

        type_key(&mut harness, Key::H, "h");
        assert_eq!(harness.state().1, "h", "typing resumes in insert mode");
    }

    #[test]
    fn escape_from_the_input_enters_normal_mode() {
        let input_id = egui::Id::unique("chat_input");
        let mut harness = input_harness(input_id, FRAME);

        type_key(&mut harness, Key::Escape, "");
        assert!(harness.state().0.is_on());
        assert_eq!(harness.ctx.memory(|m| m.focused()), None);

        type_key(&mut harness, Key::J, "j");
        assert_eq!(
            harness.state().1,
            "",
            "j moves the cursor instead of typing"
        );
    }

    #[test]
    fn escape_cancels_a_prefix_and_stays_in_normal_mode() {
        assert_eq!(
            pending_after(&[ESC, (NONE, Key::Z), ESC]),
            Some(Pending::Root)
        );
        assert_eq!(
            detect_sequence(&[ESC, (NONE, Key::Z), ESC, (NONE, Key::J)]),
            Some(KeyAction::BlockCursorDown),
        );
    }

    /// Press `presses` and report whether the last frame's Esc was still in
    /// the input when `check_keybindings` returned, and whether normal mode
    /// is on afterwards.
    fn escape_left_over(keys: KeyContext, presses: &[(Modifiers, Key)]) -> (bool, bool) {
        key_left_over(keys, presses, Key::Escape)
    }

    /// [`escape_left_over`] for any `key`: whether the last frame's `key` was
    /// left for whoever reads keys after the bindings (a question's options,
    /// chrome), and whether normal mode is on afterwards.
    fn key_left_over(keys: KeyContext, presses: &[(Modifiers, Key)], key: Key) -> (bool, bool) {
        let mut harness = Harness::new_ui_state(
            move |ui, (mode, left): &mut (NormalMode, bool)| {
                check(ui.ctx(), mode, keys);
                *left |= ui.input(|i| i.key_pressed(key));
            },
            (NormalMode::default(), false),
        );
        harness.run();
        let Some((last, earlier)) = presses.split_last() else {
            panic!("no presses");
        };
        for (modifiers, key) in earlier {
            harness.press_key_modifiers(*modifiers, *key);
        }
        harness.state_mut().1 = false;
        harness.press_key_modifiers(last.0, last.1);
        (harness.state().1, harness.state().0.is_on())
    }

    #[test]
    fn escape_at_the_root_is_left_for_chrome() {
        assert_eq!(
            escape_left_over(FRAME, &[ESC, ESC]),
            (true, true),
            "the second Esc is chrome's (the side menu), and normal mode stays on"
        );
        assert_eq!(
            escape_left_over(FRAME, &[ESC]),
            (false, true),
            "the first Esc is Dave's: it enters normal mode"
        );
    }

    #[test]
    fn escape_cancelling_a_prefix_is_consumed() {
        assert_eq!(
            escape_left_over(FRAME, &[ESC, (NONE, Key::Z), ESC]),
            (false, true),
            "the Esc only drops the z, so chrome never sees it"
        );
    }

    /// A held Esc: one press, then `repeats` more with no key-up between,
    /// one a frame. Returns whether any Esc was left for chrome, and whether
    /// normal mode is on. egui marks a press a repeat itself, from whether
    /// the key is already down, so a key-up (`press_key_modifiers` queues
    /// one) would make the next press fresh.
    fn hold_escape(repeats: usize) -> (bool, bool) {
        let mut harness = Harness::new_ui_state(
            |ui, (mode, left): &mut (NormalMode, bool)| {
                check(ui.ctx(), mode, FRAME);
                *left |= ui.input(|i| i.key_pressed(Key::Escape));
            },
            (NormalMode::default(), false),
        );
        harness.run();
        for _ in 0..=repeats {
            harness.input_mut().events.push(egui::Event::Key {
                key: Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: NONE,
            });
            harness.step();
        }
        (harness.state().1, harness.state().0.is_on())
    }

    #[test]
    fn holding_escape_does_not_reach_chrome() {
        assert_eq!(
            hold_escape(5),
            (false, true),
            "the first press enters normal mode and its repeats are swallowed, \
             so the side menu doesn't flicker"
        );
    }

    #[test]
    fn a_tentative_answer_ends_normal_mode() {
        let pending = KeyContext {
            has_pending_permission: true,
            ..FRAME
        };
        for (press, action) in [
            ((NONE, Key::Exclamationmark), KeyAction::TentativeAccept),
            ((SHIFT, Key::Num2), KeyAction::TentativeDeny),
            ((SHIFT, Key::Num3), KeyAction::TentativeAllowAlways),
        ] {
            assert_eq!(
                detect_sequence_in(pending, &[ESC, press]),
                Some(action.clone())
            );
            assert_eq!(
                pending_after_in(pending, &[ESC, press]),
                None,
                "{action:?}: the input takes the message, so its letters type"
            );
        }
    }

    #[test]
    fn escape_at_the_root_cancels_a_waiting_tentative_answer() {
        let mut harness =
            Harness::new_ui_state(
                |ui,
                 (mode, keys, action, left): &mut (
                    NormalMode,
                    KeyContext,
                    Option<KeyAction>,
                    bool,
                )| {
                    *action = check(ui.ctx(), mode, *keys);
                    *left |= ui.input(|i| i.key_pressed(Key::Escape));
                },
                (NormalMode::default(), FRAME, None, false),
            );
        harness.run();
        harness.press_key_modifiers(ESC.0, ESC.1);
        assert!(harness.state().0.is_on());

        // The answer went tentative some other way (a click) while normal
        // mode stayed on.
        harness.state_mut().1 = KeyContext {
            has_pending_permission: true,
            in_tentative_state: true,
            ..FRAME
        };
        harness.state_mut().3 = false;
        harness.press_key_modifiers(ESC.0, ESC.1);
        assert_eq!(harness.state().2, Some(KeyAction::CancelTentative));
        assert!(!harness.state().3, "the Esc is not chrome's");
        assert!(harness.state().0.is_on(), "and normal mode stays on");
    }

    #[test]
    fn ctrl_g_and_e_end_normal_mode_for_the_external_editor() {
        for press in [(Modifiers::CTRL, Key::G), (NONE, Key::E)] {
            assert_eq!(
                detect_sequence(&[ESC, press]),
                Some(KeyAction::OpenExternalEditor),
            );
            assert_eq!(pending_after(&[ESC, press]), None, "{press:?}");
        }
    }

    #[test]
    fn bare_keys_the_regular_bindings_own_pass_through_normal_mode() {
        assert_eq!(
            detect_sequence(&[ESC, (NONE, Key::Delete)]),
            Some(KeyAction::DeleteActiveSession),
        );
        assert_eq!(
            pending_after(&[ESC, (NONE, Key::Delete)]),
            Some(Pending::Root)
        );
        assert_eq!(
            key_left_over(FRAME, &[ESC, (NONE, Key::Enter)], Key::Enter),
            (true, true),
            "Enter is left for a question's submit"
        );
        let question = KeyContext {
            has_pending_permission: true,
            has_pending_question: true,
            ..FRAME
        };
        assert_eq!(
            key_left_over(question, &[ESC, (NONE, Key::Num4)], Key::Num4),
            (true, true),
            "a digit is left for the question's options"
        );
    }

    #[test]
    fn escape_under_an_overlay_is_the_overlays() {
        let overlay = KeyContext {
            overlay_open: true,
            ..FRAME
        };
        assert_eq!(escape_left_over(overlay, &[ESC]), (true, false));
    }

    #[test]
    fn an_overlay_opening_ends_normal_mode_and_keeps_its_esc() {
        let mut harness = Harness::new_ui_state(
            |ui, (mode, overlay, left): &mut (NormalMode, bool, bool)| {
                let keys = KeyContext {
                    overlay_open: *overlay,
                    ..FRAME
                };
                check(ui.ctx(), mode, keys);
                *left |= ui.input(|i| i.key_pressed(Key::Escape));
            },
            (NormalMode::default(), false, false),
        );
        harness.run();
        harness.press_key_modifiers(ESC.0, ESC.1);
        assert!(harness.state().0.is_on());

        harness.state_mut().1 = true;
        harness.state_mut().2 = false;
        harness.press_key_modifiers(ESC.0, ESC.1);
        assert!(!harness.state().0.is_on(), "the overlay ends normal mode");
        assert!(harness.state().2, "and its Esc stays the overlay's");
    }

    #[test]
    fn escape_while_renaming_cancels_the_rename() {
        let renaming = KeyContext {
            renaming: true,
            ..FRAME
        };
        assert_eq!(escape_left_over(renaming, &[ESC]), (true, false));
    }

    #[test]
    fn ctrl_keys_work_without_leaving_normal_mode() {
        let ctrl_j = (Modifiers::CTRL, Key::J);
        assert_eq!(detect_sequence(&[ESC, ctrl_j]), Some(KeyAction::NextAgent));
        assert_eq!(pending_after(&[ESC, ctrl_j]), Some(Pending::Root));
        assert_eq!(
            detect_sequence(&[ESC, ctrl_j, (NONE, Key::J)]),
            Some(KeyAction::BlockCursorDown),
        );
    }

    #[test]
    fn ctrl_keys_that_take_typing_leave_normal_mode() {
        assert_eq!(
            detect_sequence(&[ESC, (Modifiers::CTRL, Key::T)]),
            Some(KeyAction::NewAgent)
        );
        assert_eq!(pending_after(&[ESC, (Modifiers::CTRL, Key::T)]), None);
        assert_eq!(pending_after(&[ESC, (CTRL_SHIFT, Key::R)]), None);
    }

    #[test]
    fn permission_keys_reach_the_permission_bindings_in_normal_mode() {
        let pending = KeyContext {
            has_pending_permission: true,
            ..FRAME
        };
        assert_eq!(
            detect_sequence_in(pending, &[ESC, (NONE, Key::Num1)]),
            Some(KeyAction::AcceptPermission),
        );
        assert_eq!(
            pending_after_in(pending, &[ESC, (NONE, Key::Num1)]),
            Some(Pending::Root),
        );
    }

    #[test]
    fn clicking_into_a_text_field_leaves_normal_mode() {
        let input_id = egui::Id::unique("chat_input");
        let other_id = egui::Id::unique("some_other_field");
        let mut harness = Harness::new_ui_state(
            move |ui, (mode, text, other): &mut (NormalMode, String, String)| {
                check(ui.ctx(), mode, FRAME);
                ui.add(egui::TextEdit::singleline(text).id(input_id))
                    .accessible_name("test field");
                ui.add(egui::TextEdit::singleline(other).id(other_id))
                    .accessible_name("other field");
            },
            (NormalMode::default(), String::new(), String::new()),
        );
        harness.ctx.memory_mut(|m| m.request_focus(input_id));
        harness.run();

        type_key(&mut harness, Key::Escape, "");
        assert!(harness.state().0.is_on());

        // A click into the other field focuses it.
        harness.ctx.memory_mut(|m| m.request_focus(other_id));
        harness.run();
        assert!(!harness.state().0.is_on(), "normal mode ends");
        assert_eq!(
            harness.ctx.memory(|m| m.focused()),
            Some(other_id),
            "without handing focus back to the input"
        );
        assert!(!harness.state_mut().0.take_input_focus());

        type_key(&mut harness, Key::J, "j");
        assert_eq!(harness.state().2, "j", "typing lands in the clicked field");
    }

    #[test]
    fn ctrl_m_cycles_permission_mode() {
        assert_eq!(
            detect(Modifiers::CTRL, Key::M),
            Some(KeyAction::CyclePermissionMode),
        );
    }
}
