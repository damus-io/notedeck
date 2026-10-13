use crate::config::AiMode;
use crate::ui::scene::SceneDir;
use egui::Key;
use notedeck_ui::chord;

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
    /// Stop the active session's running turn (normal S), the same
    /// thing the Stop button does
    Interrupt,
    /// Open the issue the active session works — its `issue_url`, e.g. a
    /// headway card — through chrome (normal s), the reverse of headway's
    /// `s` from a card to its session
    OpenIssue,
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
    /// Expand every collapsible block (normal zr, Ctrl+Shift+E)
    BlockExpandAll,
    /// Collapse every collapsible block (normal zm, Ctrl+Shift+M)
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
    /// Step the scene's selection to the nearest node that way, switching to
    /// it without focusing its input (normal h, then h / j / k / l on the
    /// map)
    SceneStep(SceneDir),
    /// Leave normal mode for the chat input (`i` / `a`)
    InsertMode,
    /// Show the active session's Details tab (normal gd)
    ShowDetails,
    /// Show the active session's chat tab (normal gc)
    ShowChat,
}

/// Dave's vim-style normal mode.
///
/// Owned by [`Dave`](crate::Dave) and passed `&mut` into [`check_keybindings`]
/// each frame. Insert mode is the default: the chat input has focus and you
/// type. Esc enters normal mode, which sets the input's focus aside so bare
/// keys are commands rather than typing.
///
/// The commands themselves are chrome's: normal mode reports Dave's key
/// scopes ([`crate::ui_tools::key_scopes`]), and chrome's keymap
/// (`default_keymap.toml`, scopes `dave.chat`, `dave.sessions`, `dave.normal`)
/// resolves each press, chords like `gg` and `zr` included, to one of Dave's
/// UI tools before Dave renders. Chrome's which-key strip lists them. What
/// stays here is the mode itself:
///
/// - Esc in the input enters it. Esc at its root is left for chrome, which
///   toggles the side menu, and normal mode stays on; a held Esc's
///   auto-repeats are swallowed, so holding it doesn't flicker the menu.
/// - A bare key the keymap didn't take is swallowed, so it reaches no widget,
///   except the ones Dave's Ctrl ladder reads unfocused ([`owned_bare_key`]).
///   A modified key goes to the Ctrl ladder. Neither leaves the mode.
/// - It ends as [`Self::after_action`] says (`i` / `a`, `q`, `n`, `r`, …),
///   when you click into a text field, or when an overlay opens.
///
/// `h` / `l` point the motions at the session list or the chat ([`Pane`]),
/// which picks the key scope: `j` / `k` walk blocks in the chat and sessions
/// in the list. In the scene view the map stands in for the list: `h` steps
/// onto it, and there `h` / `j` / `k` / `l` step between its nodes, `l` off
/// its right edge back to the chat beside it.
#[derive(Default)]
pub struct NormalMode {
    /// Whether normal mode is on.
    on: bool,
    /// Which pane the motions move through. Back to [`Pane::Chat`] each time
    /// normal mode is entered.
    pane: Pane,
    /// Whether the session list is on screen, as of the last frame. `h` is a
    /// no-op without it.
    sessions_shown: bool,
    /// Whether the scene view's map stands in for the session list, as of
    /// the last frame.
    scene: bool,
    /// Normal mode ended wanting the active input focused; taken by
    /// [`Self::take_input_focus`].
    input_focus_due: bool,
}

/// Which part of Dave normal mode's motions move through.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pane {
    /// The session list: `j` / `k` switch sessions. In the scene view, the
    /// map: `h` / `j` / `k` / `l` step between its nodes.
    Sessions,
    /// The chat transcript: `j` / `k` walk its collapsible blocks.
    #[default]
    Chat,
}

/// What the active session offers normal mode's keys, read live from it
/// whenever asked, so a UI tool called before Dave renders sees the session
/// it acts on.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ActiveOffers {
    /// The active session's mode; agentic-only keys need
    /// [`AiMode::Agentic`].
    pub ai_mode: AiMode,
    /// The active session has a running turn, for `S`.
    pub interruptible: bool,
    /// The active session has an issue to open, for `s`.
    pub has_issue: bool,
}

/// Normal mode as the UI and Dave's UI tools see it: the session list marks
/// its row while the motions are in [`Pane::Sessions`], and
/// [`crate::ui_tools::available`] reads what the keys can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NormalView {
    pub pane: Pane,
    /// `h` can reach the session list.
    pub sessions_shown: bool,
    /// The scene view's map stands in for the session list.
    pub scene: bool,
    /// The agentic-only keys (`c`, `v`, `m`, `]q`, `[q`) apply.
    pub agentic: bool,
    /// `S` has a running turn to stop.
    pub interruptible: bool,
    /// `s` has an issue to open.
    pub has_issue: bool,
}

impl NormalMode {
    /// Normal mode as the UI sees it with the active session's `offers`, or
    /// `None` in insert mode.
    pub(crate) fn view(&self, offers: ActiveOffers) -> Option<NormalView> {
        self.on.then_some(NormalView {
            pane: self.pane,
            sessions_shown: self.sessions_shown,
            scene: self.scene,
            agentic: offers.ai_mode == AiMode::Agentic,
            interruptible: offers.interruptible,
            has_issue: offers.has_issue,
        })
    }

    /// Whether normal mode just ended wanting the active session's input
    /// focused. Reading it clears it, so it is acted on once.
    pub fn take_input_focus(&mut self) -> bool {
        std::mem::take(&mut self.input_focus_due)
    }

    /// `action` ran — its key through chrome's keymap, its UI tool called by
    /// name (`crate::ui_tools`), or a Ctrl binding — and this is what it does
    /// to normal mode itself, the same whichever way it came:
    ///
    /// - `h` / `l` point the motions at the session list or the chat.
    /// - While normal mode is on, an action that opens something you type
    ///   into leaves it the way its normal-mode key does: `i` / `a`, `q`, the
    ///   external editor and a tentative permission answer (`!`, Shift+2,
    ///   Shift+3, which waits for a message typed into the input) hand the
    ///   input focus; `n` and `r` leave focus to the picker or the rename
    ///   field they open. Anything else keeps normal mode on, like vim.
    pub(crate) fn after_action(&mut self, action: &KeyAction) {
        match action {
            KeyAction::FocusSessionsPane => self.pane = Pane::Sessions,
            KeyAction::FocusChatPane => self.pane = Pane::Chat,
            _ => {}
        }
        if !self.is_normal() {
            return;
        }
        match action {
            KeyAction::NewAgent | KeyAction::RenameAgent => self.release(),
            KeyAction::InsertMode
            | KeyAction::BlockCursorClear
            | KeyAction::OpenExternalEditor
            | KeyAction::TentativeAccept
            | KeyAction::TentativeDeny
            | KeyAction::TentativeAllowAlways => self.end(),
            _ => {}
        }
    }

    /// Record what this frame offers normal mode. With the session list off
    /// screen (a narrow layout, an overlay) it falls back to the chat.
    fn observe(&mut self, keys: &KeyContext) {
        self.sessions_shown = keys.sessions_shown;
        self.scene = keys.scene;
        if !keys.sessions_shown {
            self.pane = Pane::Chat;
        }
    }

    /// Whether normal mode is on: bare keys are commands, not typing. Dave's
    /// key scopes and UI tools are live only then.
    pub fn is_normal(&self) -> bool {
        self.on
    }

    /// Which pane the motions move through.
    pub fn pane(&self) -> Pane {
        self.pane
    }

    /// Point the motions at `pane`, as a jump list restore does. The
    /// session list falls back to the chat if the next frame finds it off
    /// screen.
    pub(crate) fn set_pane(&mut self, pane: Pane) {
        self.pane = pane;
    }

    /// Whether the session list was on screen last frame, so `h` can reach it.
    pub fn sessions_shown(&self) -> bool {
        self.sessions_shown
    }

    /// Enter normal mode. egui has already dropped focus on the Esc that got
    /// us here; anything still focused is set aside so bare keys reach
    /// chrome's keymap, which stands aside while a widget has focus.
    pub(crate) fn open(&mut self, ctx: &egui::Context) {
        ctx.memory_mut(|m| {
            if let Some(id) = m.focused() {
                m.surrender_focus(id);
            }
        });
        self.pane = Pane::Chat;
        self.on = true;
    }

    /// Back to insert mode: focus the active session's input. There is no
    /// earlier focus to hand back, since the Esc that entered normal mode
    /// dropped it.
    fn end(&mut self) {
        self.on = false;
        self.input_focus_due = true;
    }

    /// Leave normal mode and leave focus alone: the action opens something
    /// that takes typing (a rename field, the new-agent picker) and claims
    /// focus itself, or the user clicked into a text field.
    fn release(&mut self) {
        self.on = false;
    }
}

/// Bare keys Dave's Ctrl ladder reads while the chat input is unfocused,
/// which is all of normal mode: the permission answers (`1` / `2` / `3`, `!`),
/// a question's options (`1`-`9`, Enter) and delete-session. Normal mode
/// lets them through rather than swallowing them.
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

/// What normal mode made of this frame's input.
enum NormalStep {
    /// Normal mode is off, or this frame's key is a modified one or a bare key
    /// the Ctrl ladder owns: it should look at this frame.
    FallThrough,
    /// Normal mode swallowed this frame's keys, or left its Esc for chrome.
    Done,
}

/// Feed this frame's input to normal mode, if it is on. Its commands were
/// chrome's keymap's to take before Dave renders, so a bare key left here is
/// unbound: it's swallowed. `tentative`: a tentative permission answer is
/// waiting for its message, and Esc cancels it.
fn check_normal_mode(ctx: &egui::Context, mode: &mut NormalMode, tentative: bool) -> NormalStep {
    if !mode.is_normal() {
        return NormalStep::FallThrough;
    }

    // Normal mode set focus aside and holds focus requests until it ends, so
    // a focused widget means the user clicked into a text field: they are
    // typing now.
    if ctx.memory(|m| m.focused()).is_some() {
        mode.release();
        return NormalStep::FallThrough;
    }

    // Esc at the root is chrome's (the side menu), unless it cancels a
    // tentative answer; normal mode stays on either way. Chrome toggles its
    // menu on every Esc it sees, auto-repeats included, so only a fresh
    // press reaches it.
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        if tentative {
            return NormalStep::FallThrough;
        }
        if ctx.input(escape_is_repeat) {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
        }
        return NormalStep::Done;
    }

    let Some(press) = ctx.input(chord::first_key_press) else {
        return NormalStep::Done;
    };
    // A modified key is somebody else's binding (a Ctrl alias), and the
    // permission keys and Delete are the Ctrl ladder's: normal mode stays on.
    if !press.is_bare() || owned_bare_key(press.key) {
        return NormalStep::FallThrough;
    }

    // Any other key the keymap left is unbound here: swallow it and its
    // text, so it never reaches the input or anything else reading keys.
    chord::swallow_key_events(ctx);
    NormalStep::Done
}

/// What a frame offers the keybindings besides the keys themselves, gathered
/// by `Dave::process_keybindings` from the active session and the layout.
#[derive(Clone, Copy, Debug)]
pub struct KeyContext {
    /// The active session's mode; agentic-only bindings need
    /// [`AiMode::Agentic`].
    pub ai_mode: AiMode,
    /// The session list, or the scene's map standing in for it, is on
    /// screen, for normal mode's `h`.
    pub sessions_shown: bool,
    /// The scene view is up: the session pane is its map.
    pub scene: bool,
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
/// `mode` carries normal mode across frames (see [`NormalMode`]); its
/// commands come through chrome's keymap, not here. Esc enters it from
/// insert mode unless Esc is someone else's. `keys` is what the frame offers
/// the bindings (see [`KeyContext`]).
pub fn check_keybindings(
    ctx: &egui::Context,
    mode: &mut NormalMode,
    keys: KeyContext,
) -> Option<KeyAction> {
    mode.observe(&keys);
    let KeyContext {
        ai_mode,
        in_tentative_state,
        overlay_open,
        renaming,
        ..
    } = keys;
    let is_agentic = ai_mode == AiMode::Agentic;

    // An overlay is modal: normal mode's keys are the chat's.
    if overlay_open && mode.is_normal() {
        mode.release();
    }

    // Normal mode swallows the bare keys the keymap left, and leaves its Esc
    // for chrome unless a tentative answer takes it.
    if let NormalStep::Done = check_normal_mode(ctx, mode, is_agentic && in_tentative_state) {
        return None;
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
    if !mode.is_normal()
        && !overlay_open
        && !renaming
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape))
    {
        mode.open(ctx);
        return None;
    }

    let action = check_regular_bindings(ctx, keys);
    if let Some(action) = &action {
        mode.after_action(action);
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
    use crate::ui_tools::chrome_keys::ChromeKeys;
    use egui::{Key, Modifiers};
    use egui_kittest::Harness;
    use notedeck::test_harness::PressKey;
    use notedeck::ToolInvocation;
    use notedeck_chrome::keymap::{KeyStroke, Mods};
    use notedeck_ui::chord::KeyPress;

    /// A frame as these tests run it: what the keybindings see, and what the
    /// active session offers chrome's keymap.
    #[derive(Clone, Copy)]
    struct Frame {
        keys: KeyContext,
        offers: ActiveOffers,
    }

    /// The session list on screen, agentic, a turn running and an issue
    /// linked, with no pending prompts: every normal-mode key applies.
    const FRAME: Frame = Frame {
        keys: KeyContext {
            ai_mode: AiMode::Agentic,
            sessions_shown: true,
            scene: false,
            has_pending_permission: false,
            has_pending_question: false,
            in_tentative_state: false,
            overlay_open: false,
            renaming: false,
        },
        offers: ActiveOffers {
            ai_mode: AiMode::Agentic,
            interruptible: true,
            has_issue: true,
        },
    };

    impl Frame {
        /// This frame with `keys` changed.
        fn keys(self, keys: impl FnOnce(&mut KeyContext)) -> Self {
            let mut frame = self;
            keys(&mut frame.keys);
            frame
        }

        /// This frame with `offers` changed.
        fn offers(self, offers: impl FnOnce(&mut ActiveOffers)) -> Self {
            let mut frame = self;
            offers(&mut frame.offers);
            frame
        }
    }

    /// Normal mode and chrome's keymap in front of it, and the last action
    /// either produced.
    struct Keys {
        mode: NormalMode,
        chrome: ChromeKeys,
        action: Option<KeyAction>,
    }

    impl Keys {
        fn new() -> Self {
            Keys {
                mode: NormalMode::default(),
                chrome: ChromeKeys::new(),
                action: None,
            }
        }

        /// One Dave frame: chrome's keymap, then `check_keybindings`.
        /// Accumulates: a later frame that detects nothing (a key-up) doesn't
        /// clobber an earlier detection with `None`.
        fn frame(&mut self, ctx: &egui::Context, frame: Frame) {
            let overlay = frame.keys.overlay_open;
            if let Some(action) = self
                .chrome
                .frame(ctx, &mut self.mode, frame.offers, overlay)
            {
                self.action = Some(action);
            }
            if let Some(action) = check_keybindings(ctx, &mut self.mode, frame.keys) {
                self.action = Some(action);
            }
        }

        /// The tool the keymap last called.
        fn last_call(&self) -> Option<&ToolInvocation> {
            self.chrome.calls.last()
        }
    }

    /// Press each of `presses` in turn in `frame`, one Dave frame each.
    fn keys_after_in(frame: Frame, presses: &[KeyPress]) -> Harness<'static, Keys> {
        let mut harness = Harness::new_ui_state(
            move |ui, keys: &mut Keys| keys.frame(ui.ctx(), frame),
            Keys::new(),
        );
        harness.run();
        for KeyPress { modifiers, key } in presses {
            harness.press_key_modifiers(*modifiers, *key);
        }
        harness
    }

    /// The last action `presses` produced, in [`FRAME`].
    fn detect_sequence(presses: &[KeyPress]) -> Option<KeyAction> {
        detect_sequence_in(FRAME, presses)
    }

    /// [`detect_sequence`] in `frame`.
    fn detect_sequence_in(frame: Frame, presses: &[KeyPress]) -> Option<KeyAction> {
        keys_after_in(frame, presses).state().action.clone()
    }

    /// `modifiers`+`key` alone.
    fn detect(modifiers: Modifiers, key: Key) -> Option<KeyAction> {
        detect_sequence(&[press(modifiers, key)])
    }

    /// Whether normal mode is on after `presses`, in [`FRAME`].
    fn normal_after(presses: &[KeyPress]) -> bool {
        normal_after_in(FRAME, presses)
    }

    /// [`normal_after`] in `frame`.
    fn normal_after_in(frame: Frame, presses: &[KeyPress]) -> bool {
        keys_after_in(frame, presses).state().mode.is_normal()
    }

    /// A press of `key` with `modifiers` held.
    const fn press(modifiers: Modifiers, key: Key) -> KeyPress {
        KeyPress { key, modifiers }
    }

    /// The press that types `stroke`.
    fn stroke_press(stroke: &KeyStroke) -> KeyPress {
        let mut modifiers = Modifiers::NONE;
        if stroke.mods.contains(Mods::SHIFT) {
            modifiers |= Modifiers::SHIFT;
        }
        if stroke.mods.contains(Mods::CTRL) {
            modifiers |= Modifiers::CTRL;
        }
        press(modifiers, stroke.key)
    }

    const ESC: KeyPress = press(Modifiers::NONE, Key::Escape);
    const H: KeyPress = press(Modifiers::NONE, Key::H);
    const NONE: Modifiers = Modifiers::NONE;
    const SHIFT: Modifiers = Modifiers::SHIFT;
    const CTRL_SHIFT: Modifiers = Modifiers::CTRL.plus(Modifiers::SHIFT);

    #[test]
    fn normal_j_moves_the_cursor_down() {
        assert_eq!(
            detect_sequence(&[ESC, press(NONE, Key::J)]),
            Some(KeyAction::BlockCursorDown),
        );
    }

    #[test]
    fn normal_z_shift_r_expands_all() {
        assert_eq!(
            detect_sequence(&[ESC, press(NONE, Key::Z), press(SHIFT, Key::R)]),
            Some(KeyAction::BlockExpandAll),
        );
    }

    #[test]
    fn normal_g_g_moves_to_the_first_block() {
        assert_eq!(
            detect_sequence(&[ESC, press(NONE, Key::G), press(NONE, Key::G)]),
            Some(KeyAction::BlockCursorFirst),
        );
    }

    #[test]
    fn normal_g_d_and_g_c_switch_the_sessions_tab_from_either_pane() {
        for prefix in [vec![ESC], vec![ESC, H]] {
            let mut presses = prefix.clone();
            presses.extend([press(NONE, Key::G), press(NONE, Key::D)]);
            assert_eq!(detect_sequence(&presses), Some(KeyAction::ShowDetails));
            presses.extend([press(NONE, Key::G), press(NONE, Key::C)]);
            assert_eq!(detect_sequence(&presses), Some(KeyAction::ShowChat));
            assert!(normal_after(&presses), "the tabs keep normal mode on");
        }
    }

    #[test]
    fn motions_repeat_because_normal_mode_stays_on() {
        assert_eq!(
            detect_sequence(&[ESC, press(NONE, Key::J), press(NONE, Key::K)]),
            Some(KeyAction::BlockCursorUp),
        );
    }

    #[test]
    fn commands_keep_normal_mode_on_until_one_leaves_it() {
        assert!(!normal_after(&[]));
        assert!(normal_after(&[ESC]));
        let h = keys_after_in(FRAME, &[ESC, press(NONE, Key::Z)]);
        assert!(h.state().mode.is_normal());
        assert!(
            h.state().chrome.engine().pending().is_some(),
            "z is a prefix, pending in chrome"
        );
        assert!(
            normal_after(&[ESC, press(NONE, Key::Z), press(NONE, Key::A)]),
            "a finished command leaves normal mode on"
        );
        assert!(!normal_after(&[ESC, press(NONE, Key::Q)]));
        assert!(!normal_after(&[ESC, press(NONE, Key::I)]));
        assert!(
            normal_after(&[ESC, press(NONE, Key::X)]),
            "an unbound key is swallowed and normal mode stays on"
        );
        let h = keys_after_in(FRAME, &[ESC, press(NONE, Key::Z), press(NONE, Key::X)]);
        assert!(h.state().mode.is_normal());
        assert!(
            h.state().chrome.engine().pending().is_none(),
            "and drops a half-typed prefix"
        );
    }

    /// Every `dave.*` binding in the shipped keymap, pressed from normal mode
    /// in each pane whose scopes hold it, calls exactly its own tool with its
    /// own args, and Dave takes the call: no binding is shadowed by another
    /// scope's prefix, unavailable with everything on offer, or broken. The
    /// which-key strip lists the bindings the keys would run (chrome's
    /// `Keymap::hints`), so this is what keeps it honest. The one shadowing
    /// by design is the scene's map: its `h j k l` steps stand over the
    /// session list's `j` / `k` and the panes' `h` / `l`.
    #[test]
    fn every_dave_binding_calls_its_tool_from_its_pane() {
        let keymap_keys = ChromeKeys::new();
        let keymap = keymap_keys.engine().keymap();
        let panes = [
            (Pane::Chat, false, vec![ESC]),
            (Pane::Sessions, false, vec![ESC, H]),
            (Pane::Sessions, true, vec![ESC, H]),
        ];
        let mut checked = 0;
        for (pane, scene, prefix) in panes {
            let view = NormalView {
                pane,
                sessions_shown: true,
                scene,
                agentic: true,
                interruptible: true,
                has_issue: true,
            };
            let frame = FRAME.keys(|k| k.scene = scene);
            let scene_steps = keymap.bindings("dave.scene");
            for scope in crate::ui_tools::key_scopes(Some(view)) {
                for binding in keymap.bindings(scope) {
                    let notedeck_chrome::keymap::BindAction::Tool(want) = &binding.action else {
                        continue;
                    };
                    let stepped_over = scene
                        && *scope != "dave.scene"
                        && scene_steps.iter().any(|step| step.keys == binding.keys);
                    if stepped_over {
                        continue;
                    }
                    let mut presses = prefix.clone();
                    presses.extend(binding.keys.as_slice().iter().map(stroke_press));
                    let h = keys_after_in(frame, &presses);
                    assert_eq!(
                        h.state().last_call(),
                        Some(want),
                        "{pane:?}: {scope} {} ",
                        binding.keys
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 40, "Dave's keymap is in: {checked} bindings");
    }

    #[test]
    fn normal_h_j_switches_sessions_instead_of_moving_the_cursor() {
        assert_eq!(
            detect_sequence(&[ESC, H, press(NONE, Key::J)]),
            Some(KeyAction::SessionPaneNext),
        );
        assert_eq!(
            detect_sequence(&[ESC, H, press(SHIFT, Key::G)]),
            Some(KeyAction::SessionPaneLast),
        );
    }

    /// In the scene view `h` steps onto the map, where `h j k l` step
    /// between its nodes rather than switching panes or sessions in order.
    #[test]
    fn normal_h_on_the_scene_steps_across_the_map() {
        let scene = FRAME.keys(|k| k.scene = true);
        let steps = [
            (Key::H, SceneDir::Left),
            (Key::J, SceneDir::Down),
            (Key::K, SceneDir::Up),
            (Key::L, SceneDir::Right),
        ];
        for (key, dir) in steps {
            assert_eq!(
                detect_sequence_in(scene, &[ESC, H, press(NONE, key)]),
                Some(KeyAction::SceneStep(dir)),
                "{key:?}"
            );
        }
        assert_eq!(
            detect_sequence_in(scene, &[ESC, H, press(SHIFT, Key::G)]),
            Some(KeyAction::SessionPaneLast),
            "the list's keys the map doesn't take still apply"
        );
        assert_eq!(
            detect_sequence_in(scene, &[ESC, press(NONE, Key::J)]),
            Some(KeyAction::BlockCursorDown),
            "Esc lands in the chat, as it does beside the list"
        );
    }

    #[test]
    fn normal_h_l_j_is_back_in_the_chat() {
        assert_eq!(
            detect_sequence(&[ESC, H, press(NONE, Key::L), press(NONE, Key::J)]),
            Some(KeyAction::BlockCursorDown),
        );
    }

    #[test]
    fn every_entry_starts_in_the_chat() {
        assert_eq!(
            detect_sequence(&[ESC, H, press(NONE, Key::I), ESC, press(NONE, Key::J)]),
            Some(KeyAction::BlockCursorDown),
        );
        assert_eq!(
            detect_sequence(&[ESC, H, ESC, press(NONE, Key::J)]),
            Some(KeyAction::SessionPaneNext),
            "Esc at the root leaves normal mode on, so the pane stays",
        );
    }

    /// Enter on the session list is `dave_insert_mode`: the session you're
    /// on takes the keyboard.
    #[test]
    fn enter_from_the_session_list_leaves_normal_mode() {
        let presses = [ESC, H, press(NONE, Key::Enter)];
        assert_eq!(detect_sequence(&presses), Some(KeyAction::InsertMode));
        assert!(!normal_after(&presses));
    }

    #[test]
    fn h_without_a_session_list_does_nothing() {
        let no_list = FRAME.keys(|k| k.sessions_shown = false);
        assert_eq!(
            detect_sequence_in(no_list, &[ESC, H, press(NONE, Key::J)]),
            Some(KeyAction::BlockCursorDown),
            "h does nothing, normal mode stays on and in the chat",
        );
    }

    #[test]
    fn rename_and_new_agent_leave_normal_mode() {
        assert!(!normal_after(&[ESC, press(NONE, Key::R)]));
        assert!(!normal_after(&[ESC, press(NONE, Key::N)]));
        assert!(
            normal_after(&[ESC, press(NONE, Key::D), press(NONE, Key::D)]),
            "dd keeps normal mode on, so you can walk on to the next session",
        );
    }

    #[test]
    fn normal_shift_s_stops_the_running_turn_from_either_pane() {
        assert_eq!(
            detect_sequence(&[ESC, press(SHIFT, Key::S)]),
            Some(KeyAction::Interrupt),
        );
        assert_eq!(
            detect_sequence(&[ESC, H, press(SHIFT, Key::S)]),
            Some(KeyAction::Interrupt),
        );
        assert!(
            normal_after(&[ESC, press(SHIFT, Key::S)]),
            "S keeps normal mode on, like the other session keys",
        );
    }

    #[test]
    fn normal_shift_s_with_nothing_running_is_swallowed() {
        let idle = FRAME.offers(|o| o.interruptible = false);
        assert_eq!(detect_sequence_in(idle, &[ESC, press(SHIFT, Key::S)]), None);
        assert!(normal_after_in(idle, &[ESC, press(SHIFT, Key::S)]));
    }

    /// `s` jumps to the session's issue from either pane — and never stops the
    /// turn, which moved to `S` so the two can't be confused.
    #[test]
    fn normal_s_opens_the_sessions_issue_from_either_pane() {
        assert_eq!(
            detect_sequence(&[ESC, press(NONE, Key::S)]),
            Some(KeyAction::OpenIssue),
        );
        assert_eq!(
            detect_sequence(&[ESC, H, press(NONE, Key::S)]),
            Some(KeyAction::OpenIssue),
        );
        assert!(
            normal_after(&[ESC, press(NONE, Key::S)]),
            "s keeps normal mode on, like the other session keys",
        );
    }

    #[test]
    fn normal_s_without_an_issue_is_swallowed() {
        let unlinked = FRAME.offers(|o| o.has_issue = false);
        assert_eq!(
            detect_sequence_in(unlinked, &[ESC, press(NONE, Key::S)]),
            None
        );
        assert!(normal_after_in(unlinked, &[ESC, press(NONE, Key::S)]));
    }

    #[test]
    fn escape_enters_normal_mode_and_does_not_interrupt() {
        assert_eq!(detect(NONE, Key::Escape), None);
        assert!(normal_after(&[ESC]));
    }

    #[test]
    fn escape_in_the_tentative_state_still_cancels_it() {
        let tentative = FRAME.keys(|k| {
            k.has_pending_permission = true;
            k.in_tentative_state = true;
        });
        assert_eq!(
            detect_sequence_in(tentative, &[ESC]),
            Some(KeyAction::CancelTentative)
        );
        assert!(
            !normal_after_in(tentative, &[ESC]),
            "and does not enter normal mode"
        );
    }

    #[test]
    fn insert_mode_stops_reading_bare_keys() {
        for insert in [Key::I, Key::A] {
            assert_eq!(
                detect_sequence(&[ESC, press(NONE, insert)]),
                Some(KeyAction::InsertMode),
            );
            assert_eq!(
                detect_sequence(&[ESC, press(NONE, insert), press(NONE, Key::J)]),
                Some(KeyAction::InsertMode),
                "{insert:?}: j after it is not a motion",
            );
            assert!(!normal_after(&[
                ESC,
                press(NONE, insert),
                press(NONE, Key::J)
            ]));
        }
    }

    /// Normal mode itself never times out: a long pause to read a block
    /// leaves it on.
    #[test]
    fn normal_mode_stays_on_while_idle() {
        let mut harness = keys_after_in(FRAME, &[ESC]);
        // Each step is a quarter second of simulated time.
        for _ in 0..40 {
            harness.step();
        }
        harness.press_key_modifiers(NONE, Key::J);
        assert_eq!(
            harness.state().action,
            Some(KeyAction::BlockCursorDown),
            "j after a pause still moves the cursor",
        );
    }

    /// A half-typed prefix is chrome's, and lapses after its timeout: `z`,
    /// a pause, then `o` is `o` (toggle) rather than `zo` (expand).
    #[test]
    fn a_half_typed_prefix_lapses_after_chromes_timeout() {
        let mut harness = keys_after_in(FRAME, &[ESC, press(NONE, Key::Z)]);
        for _ in 0..12 {
            harness.step();
        }
        harness.press_key_modifiers(NONE, Key::O);
        assert_eq!(harness.state().action, Some(KeyAction::BlockToggle));
        assert!(harness.state().mode.is_normal());
    }

    #[test]
    fn ctrl_shift_n_moves_the_cursor_down() {
        assert_eq!(detect(CTRL_SHIFT, Key::N), Some(KeyAction::BlockCursorDown));
    }

    #[test]
    fn bare_j_in_insert_mode_does_nothing() {
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

    /// The keys in front of a text field, as [`input_harness`] drives them.
    struct Typing {
        /// The keybindings.
        keys: Keys,
        /// What the field holds.
        text: String,
    }

    /// A frame with the keys in front of a real text field, which starts out
    /// focused like the chat input: egui drops focus from an id no widget
    /// claims. Normal mode's request to focus the input is honoured the way
    /// `settle_normal_mode_focus` does it.
    fn input_harness(input_id: egui::Id) -> Harness<'static, Typing> {
        let mut harness = Harness::new_ui_state(
            move |ui, Typing { keys, text }: &mut Typing| {
                keys.frame(ui.ctx(), FRAME);
                if keys.mode.take_input_focus() {
                    ui.memory_mut(|m| m.request_focus(input_id));
                }
                ui.add(egui::TextEdit::singleline(text).id(input_id))
                    .accessible_name("test field");
            },
            Typing {
                keys: Keys::new(),
                text: String::new(),
            },
        );
        harness.ctx.memory_mut(|m| m.request_focus(input_id));
        harness.run();
        harness
    }

    #[test]
    fn normal_mode_hands_focus_back_on_i() {
        let input_id = egui::Id::unique("chat_input");
        let mut harness = input_harness(input_id);

        harness.press_key_modifiers(ESC.modifiers, ESC.key);
        assert_eq!(
            harness.ctx.memory(|m| m.focused()),
            None,
            "Esc sets the input's focus aside"
        );

        type_key(&mut harness, Key::J, "j");
        assert_eq!(
            harness.state().text,
            "",
            "a normal-mode key is a command, not text"
        );

        type_key(&mut harness, Key::W, "w");
        assert_eq!(harness.ctx.memory(|m| m.focused()), None);
        assert_eq!(harness.state().text, "", "an unbound key is swallowed");
        assert!(
            harness.state().keys.mode.is_normal(),
            "and normal mode stays on"
        );

        // `i` ends normal mode and hands focus back in the same frame, so its
        // own text must not follow the focus into the input.
        type_key(&mut harness, Key::I, "i");
        assert_eq!(harness.ctx.memory(|m| m.focused()), Some(input_id));
        assert_eq!(harness.state().text, "", "the i is a command");

        type_key(&mut harness, Key::H, "h");
        assert_eq!(harness.state().text, "h", "typing resumes in insert mode");
    }

    #[test]
    fn escape_from_the_input_enters_normal_mode() {
        let input_id = egui::Id::unique("chat_input");
        let mut harness = input_harness(input_id);

        type_key(&mut harness, Key::Escape, "");
        assert!(harness.state().keys.mode.is_normal());
        assert_eq!(harness.ctx.memory(|m| m.focused()), None);

        type_key(&mut harness, Key::J, "j");
        assert_eq!(
            harness.state().text,
            "",
            "j moves the cursor instead of typing"
        );
    }

    #[test]
    fn escape_cancels_a_prefix_and_stays_in_normal_mode() {
        assert!(normal_after(&[ESC, press(NONE, Key::Z), ESC]));
        assert_eq!(
            detect_sequence(&[ESC, press(NONE, Key::Z), ESC, press(NONE, Key::J)]),
            Some(KeyAction::BlockCursorDown),
        );
    }

    /// What a key left behind, as [`key_left_over`] and [`hold_escape`]
    /// report it.
    #[derive(Debug, PartialEq, Eq)]
    struct LeftOver {
        /// Whether the key was left for whoever reads keys after Dave (a
        /// question's options, chrome's side menu).
        left: bool,
        /// Whether normal mode is on afterwards.
        normal: bool,
    }

    impl LeftOver {
        /// What `harness` holds now.
        fn of(harness: &Harness<'_, (Keys, bool)>) -> Self {
            Self {
                left: harness.state().1,
                normal: harness.state().0.mode.is_normal(),
            }
        }
    }

    /// Press `presses` in `frame` and report whether the last frame's `key`
    /// was left for whoever reads keys after Dave, and whether normal mode is
    /// on afterwards.
    fn key_left_over(frame: Frame, presses: &[KeyPress], key: Key) -> LeftOver {
        let mut harness = Harness::new_ui_state(
            move |ui, (keys, left): &mut (Keys, bool)| {
                keys.frame(ui.ctx(), frame);
                *left |= ui.input(|i| i.key_pressed(key));
            },
            (Keys::new(), false),
        );
        harness.run();
        let Some((last, earlier)) = presses.split_last() else {
            panic!("no presses");
        };
        for KeyPress { modifiers, key } in earlier {
            harness.press_key_modifiers(*modifiers, *key);
        }
        harness.state_mut().1 = false;
        harness.press_key_modifiers(last.modifiers, last.key);
        LeftOver::of(&harness)
    }

    /// [`key_left_over`] for Esc.
    fn escape_left_over(frame: Frame, presses: &[KeyPress]) -> LeftOver {
        key_left_over(frame, presses, Key::Escape)
    }

    #[test]
    fn escape_at_the_root_is_left_for_chrome() {
        assert_eq!(
            escape_left_over(FRAME, &[ESC, ESC]),
            LeftOver {
                left: true,
                normal: true
            },
            "the second Esc is chrome's (the side menu), and normal mode stays on"
        );
        assert_eq!(
            escape_left_over(FRAME, &[ESC]),
            LeftOver {
                left: false,
                normal: true
            },
            "the first Esc is Dave's: it enters normal mode"
        );
    }

    #[test]
    fn escape_cancelling_a_prefix_is_consumed() {
        assert_eq!(
            escape_left_over(FRAME, &[ESC, press(NONE, Key::Z), ESC]),
            LeftOver {
                left: false,
                normal: true
            },
            "the Esc only drops the z, so the side menu never sees it"
        );
    }

    /// A held Esc: one press, then `repeats` more with no key-up between,
    /// one a frame. Returns whether any Esc was left for chrome, and whether
    /// normal mode is on. egui marks a press a repeat itself, from whether
    /// the key is already down, so a key-up (`press_key_modifiers` queues
    /// one) would make the next press fresh.
    fn hold_escape(repeats: usize) -> LeftOver {
        let mut harness = Harness::new_ui_state(
            |ui, (keys, left): &mut (Keys, bool)| {
                keys.frame(ui.ctx(), FRAME);
                *left |= ui.input(|i| i.key_pressed(Key::Escape));
            },
            (Keys::new(), false),
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
        LeftOver::of(&harness)
    }

    #[test]
    fn holding_escape_does_not_reach_chrome() {
        assert_eq!(
            hold_escape(5),
            LeftOver {
                left: false,
                normal: true
            },
            "the first press enters normal mode and its repeats are swallowed, \
             so the side menu doesn't flicker"
        );
    }

    #[test]
    fn a_tentative_answer_ends_normal_mode() {
        let pending = FRAME.keys(|k| k.has_pending_permission = true);
        for (press, action) in [
            (
                press(NONE, Key::Exclamationmark),
                KeyAction::TentativeAccept,
            ),
            (press(SHIFT, Key::Num2), KeyAction::TentativeDeny),
            (press(SHIFT, Key::Num3), KeyAction::TentativeAllowAlways),
        ] {
            assert_eq!(
                detect_sequence_in(pending, &[ESC, press]),
                Some(action.clone())
            );
            assert!(
                !normal_after_in(pending, &[ESC, press]),
                "{action:?}: the input takes the message, so its letters type"
            );
        }
    }

    #[test]
    fn escape_at_the_root_cancels_a_waiting_tentative_answer() {
        let mut harness = Harness::new_ui_state(
            |ui, (keys, frame, left): &mut (Keys, Frame, bool)| {
                keys.frame(ui.ctx(), *frame);
                *left |= ui.input(|i| i.key_pressed(Key::Escape));
            },
            (Keys::new(), FRAME, false),
        );
        harness.run();
        harness.press_key_modifiers(ESC.modifiers, ESC.key);
        assert!(harness.state().0.mode.is_normal());

        // The answer went tentative some other way (a click) while normal
        // mode stayed on.
        harness.state_mut().1 = FRAME.keys(|k| {
            k.has_pending_permission = true;
            k.in_tentative_state = true;
        });
        harness.state_mut().2 = false;
        harness.press_key_modifiers(ESC.modifiers, ESC.key);
        assert_eq!(harness.state().0.action, Some(KeyAction::CancelTentative));
        assert!(!harness.state().2, "the Esc is not chrome's");
        assert!(
            harness.state().0.mode.is_normal(),
            "and normal mode stays on"
        );
    }

    #[test]
    fn ctrl_g_and_e_end_normal_mode_for_the_external_editor() {
        for press in [press(Modifiers::CTRL, Key::G), press(NONE, Key::E)] {
            assert_eq!(
                detect_sequence(&[ESC, press]),
                Some(KeyAction::OpenExternalEditor),
            );
            assert!(!normal_after(&[ESC, press]), "{press:?}");
        }
    }

    #[test]
    fn bare_keys_the_regular_bindings_own_pass_through_normal_mode() {
        assert_eq!(
            detect_sequence(&[ESC, press(NONE, Key::Delete)]),
            Some(KeyAction::DeleteActiveSession),
        );
        assert!(normal_after(&[ESC, press(NONE, Key::Delete)]));
        assert_eq!(
            key_left_over(FRAME, &[ESC, press(NONE, Key::Enter)], Key::Enter),
            LeftOver {
                left: true,
                normal: true
            },
            "Enter in the chat is left for a question's submit"
        );
        let question = FRAME.keys(|k| {
            k.has_pending_permission = true;
            k.has_pending_question = true;
        });
        assert_eq!(
            key_left_over(question, &[ESC, press(NONE, Key::Num4)], Key::Num4),
            LeftOver {
                left: true,
                normal: true
            },
            "a digit is left for the question's options"
        );
    }

    /// Normal mode swallows a key the keymap left, so it reaches no widget.
    #[test]
    fn an_unbound_key_is_swallowed() {
        assert_eq!(
            key_left_over(FRAME, &[ESC, press(NONE, Key::X)], Key::X),
            LeftOver {
                left: false,
                normal: true
            }
        );
        assert_eq!(
            key_left_over(FRAME, &[press(NONE, Key::X)], Key::X),
            LeftOver {
                left: true,
                normal: false
            },
            "in insert mode it's typing"
        );
    }

    #[test]
    fn escape_under_an_overlay_is_the_overlays() {
        let overlay = FRAME.keys(|k| k.overlay_open = true);
        assert_eq!(
            escape_left_over(overlay, &[ESC]),
            LeftOver {
                left: true,
                normal: false
            }
        );
    }

    #[test]
    fn an_overlay_opening_ends_normal_mode_and_keeps_its_esc() {
        let mut harness = Harness::new_ui_state(
            |ui, (keys, overlay, left): &mut (Keys, bool, bool)| {
                let overlay = *overlay;
                keys.frame(ui.ctx(), FRAME.keys(|k| k.overlay_open = overlay));
                *left |= ui.input(|i| i.key_pressed(Key::Escape));
            },
            (Keys::new(), false, false),
        );
        harness.run();
        harness.press_key_modifiers(ESC.modifiers, ESC.key);
        assert!(harness.state().0.mode.is_normal());

        harness.state_mut().1 = true;
        harness.state_mut().2 = false;
        harness.press_key_modifiers(ESC.modifiers, ESC.key);
        assert!(
            !harness.state().0.mode.is_normal(),
            "the overlay ends normal mode"
        );
        assert!(harness.state().2, "and its Esc stays the overlay's");
    }

    #[test]
    fn escape_while_renaming_cancels_the_rename() {
        let renaming = FRAME.keys(|k| k.renaming = true);
        assert_eq!(
            escape_left_over(renaming, &[ESC]),
            LeftOver {
                left: true,
                normal: false
            }
        );
    }

    #[test]
    fn ctrl_keys_work_without_leaving_normal_mode() {
        let ctrl_j = press(Modifiers::CTRL, Key::J);
        assert_eq!(detect_sequence(&[ESC, ctrl_j]), Some(KeyAction::NextAgent));
        assert!(normal_after(&[ESC, ctrl_j]));
        assert_eq!(
            detect_sequence(&[ESC, ctrl_j, press(NONE, Key::J)]),
            Some(KeyAction::BlockCursorDown),
        );
    }

    #[test]
    fn ctrl_keys_that_take_typing_leave_normal_mode() {
        assert_eq!(
            detect_sequence(&[ESC, press(Modifiers::CTRL, Key::T)]),
            Some(KeyAction::NewAgent)
        );
        assert!(!normal_after(&[ESC, press(Modifiers::CTRL, Key::T)]));
        assert!(!normal_after(&[ESC, press(CTRL_SHIFT, Key::R)]));
    }

    #[test]
    fn permission_keys_reach_the_permission_bindings_in_normal_mode() {
        let pending = FRAME.keys(|k| k.has_pending_permission = true);
        assert_eq!(
            detect_sequence_in(pending, &[ESC, press(NONE, Key::Num1)]),
            Some(KeyAction::AcceptPermission),
        );
        assert!(normal_after_in(pending, &[ESC, press(NONE, Key::Num1)]));
    }

    #[test]
    fn clicking_into_a_text_field_leaves_normal_mode() {
        let input_id = egui::Id::unique("chat_input");
        let other_id = egui::Id::unique("some_other_field");
        let mut harness = Harness::new_ui_state(
            move |ui, (keys, text, other): &mut (Keys, String, String)| {
                keys.frame(ui.ctx(), FRAME);
                ui.add(egui::TextEdit::singleline(text).id(input_id))
                    .accessible_name("test field");
                ui.add(egui::TextEdit::singleline(other).id(other_id))
                    .accessible_name("other field");
            },
            (Keys::new(), String::new(), String::new()),
        );
        harness.ctx.memory_mut(|m| m.request_focus(input_id));
        harness.run();

        type_key(&mut harness, Key::Escape, "");
        assert!(harness.state().0.mode.is_normal());

        // A click into the other field focuses it.
        harness.ctx.memory_mut(|m| m.request_focus(other_id));
        harness.run();
        assert!(!harness.state().0.mode.is_normal(), "normal mode ends");
        assert_eq!(
            harness.ctx.memory(|m| m.focused()),
            Some(other_id),
            "without handing focus back to the input"
        );
        assert!(!harness.state_mut().0.mode.take_input_focus());

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
