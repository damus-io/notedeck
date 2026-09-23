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
    /// Interrupt/stop the current AI operation
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

/// Seconds a chord waits for its next key before it lapses. Re-armed by every
/// key the chord accepts, so a run of `j`s never times out mid-stride.
const CHORD_TIMEOUT: f64 = 2.0;

/// Progress through a multi-key chord opened by the leader key.
///
/// Owned by [`Dave`](crate::Dave) and passed `&mut` into [`check_keybindings`]
/// each frame. While a chord is pending the chat input's focus is set aside, so
/// the bare keys that follow (`j`, `z`, `g`, …) are read as commands rather than
/// typed; when the chord ends, focus goes back to whatever held it.
///
/// A chord stays open after a command so motions repeat — `<leader> j j j za` —
/// and ends on Esc, `q`, a key it does not know, or [`CHORD_TIMEOUT`] of quiet.
#[derive(Default)]
pub struct ChordState {
    pending: Option<Pending>,
    /// `ctx.input(|i| i.time)` past which the chord lapses.
    expires_at: f64,
    /// Whatever held keyboard focus when the leader fired, handed back when the
    /// chord ends.
    restore_focus: Option<egui::Id>,
}

/// How far into a chord we are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    /// The leader fired; waiting for a command or a prefix.
    Leader,
    /// `<leader> z`: waiting for `a` / `o` / `c` / `R` / `M`.
    LeaderZ,
    /// `<leader> g`: waiting for the second `g`.
    LeaderG,
}

/// What the chord machine made of this frame's input.
enum ChordStep {
    /// No chord is pending (or it just ended on a modified key): the regular
    /// bindings should look at this frame.
    FallThrough,
    /// The chord owned this frame's keys, and maybe produced an action.
    Consumed(Option<KeyAction>),
}

impl ChordState {
    /// Open a chord: set the focused widget aside so bare keys reach us.
    fn open(&mut self, ctx: &egui::Context, now: f64) {
        self.restore_focus = ctx.memory_mut(|m| {
            let focused = m.focused();
            if let Some(id) = focused {
                m.surrender_focus(id);
            }
            focused
        });
        self.advance(Pending::Leader, now);
    }

    /// Move to `next` and re-arm the timeout.
    fn advance(&mut self, next: Pending, now: f64) {
        self.pending = Some(next);
        self.expires_at = now + CHORD_TIMEOUT;
    }

    /// End the chord and give focus back to whatever held it before.
    fn end(&mut self, ctx: &egui::Context) {
        self.pending = None;
        if let Some(id) = self.restore_focus.take() {
            ctx.memory_mut(|m| m.request_focus(id));
        }
    }
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
fn check_chord(ctx: &egui::Context, chord: &mut ChordState) -> ChordStep {
    let Some(pending) = chord.pending else {
        return ChordStep::FallThrough;
    };

    let now = ctx.input(|i| i.time);
    if now >= chord.expires_at {
        chord.end(ctx);
        return ChordStep::FallThrough;
    }

    // Esc cancels the chord and nothing else: it must not also interrupt.
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
        chord.end(ctx);
        return ChordStep::Consumed(None);
    }

    let Some(press) = ctx.input(first_key_press) else {
        // Nothing typed yet: wake up in time to let the chord lapse.
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(chord.expires_at - now));
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

    let (next, action) = match (pending, press.key, press.shift) {
        (Pending::Leader, Key::J, false) => {
            (Some(Pending::Leader), Some(KeyAction::BlockCursorDown))
        }
        (Pending::Leader, Key::K, false) => (Some(Pending::Leader), Some(KeyAction::BlockCursorUp)),
        (Pending::Leader, Key::G, true) => {
            (Some(Pending::Leader), Some(KeyAction::BlockCursorLast))
        }
        (Pending::Leader, Key::G, false) => (Some(Pending::LeaderG), None),
        (Pending::Leader, Key::Z, false) => (Some(Pending::LeaderZ), None),
        (Pending::Leader, Key::O, false) => (Some(Pending::Leader), Some(KeyAction::BlockToggle)),
        (Pending::Leader, Key::Q, false) => (None, Some(KeyAction::BlockCursorClear)),
        (Pending::LeaderG, Key::G, false) => {
            (Some(Pending::Leader), Some(KeyAction::BlockCursorFirst))
        }
        (Pending::LeaderZ, Key::A, false) => (Some(Pending::Leader), Some(KeyAction::BlockToggle)),
        (Pending::LeaderZ, Key::O, false) => (Some(Pending::Leader), Some(KeyAction::BlockOpen)),
        (Pending::LeaderZ, Key::C, false) => (Some(Pending::Leader), Some(KeyAction::BlockClose)),
        (Pending::LeaderZ, Key::R, true) => {
            (Some(Pending::Leader), Some(KeyAction::BlockExpandAll))
        }
        (Pending::LeaderZ, Key::M, true) => {
            (Some(Pending::Leader), Some(KeyAction::BlockCollapseAll))
        }
        // Anything else cancels the chord; the stray key is dropped.
        _ => (None, None),
    };

    match next {
        Some(next) => chord.advance(next, now),
        None => chord.end(ctx),
    }
    ChordStep::Consumed(action)
}

/// Check for keybinding actions.
/// Most keybindings use Ctrl modifier to avoid conflicts with text input.
/// Exception: 1/2 for permission responses work without Ctrl but only when no text input has focus.
/// In Chat mode, agentic-specific keybindings (scene view, plan mode, focus queue) are disabled.
///
/// `chord` carries a leader chord across frames: while one is pending it owns
/// the keyboard (see [`ChordState`]). `leader` is the key that opens one.
pub fn check_keybindings(
    ctx: &egui::Context,
    chord: &mut ChordState,
    leader: Leader,
    has_pending_permission: bool,
    has_pending_question: bool,
    in_tentative_state: bool,
    ai_mode: AiMode,
) -> Option<KeyAction> {
    let is_agentic = ai_mode == AiMode::Agentic;

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

    // Escape otherwise works to interrupt AI (even when text input has focus)
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
        return Some(KeyAction::Interrupt);
    }

    let ctrl = egui::Modifiers::CTRL;
    let ctrl_shift = egui::Modifiers::CTRL | egui::Modifiers::SHIFT;

    // The leader opens a chord; the keys that follow land next frame.
    if ctx.input(|i| leader.pressed(i)) {
        chord.open(ctx, ctx.input(|i| i.time));
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
    if !ctx.wants_keyboard_input() && ctx.input(|i| i.key_pressed(Key::Delete)) {
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
    if is_agentic && has_pending_permission && !has_pending_question && !ctx.wants_keyboard_input()
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

    /// Press `modifiers`+`key` in a headless egui frame and return whatever
    /// `check_keybindings` detects (agentic mode, no pending prompts).
    fn detect(modifiers: Modifiers, key: Key) -> Option<KeyAction> {
        detect_sequence(&[(modifiers, key)])
    }

    /// Press each `(modifiers, key)` in turn, threading one [`ChordState`]
    /// through every frame the way `Dave` does, and return the last action
    /// `check_keybindings` detected along the way.
    fn detect_sequence(presses: &[(Modifiers, Key)]) -> Option<KeyAction> {
        detect_sequence_with(Leader::DEFAULT, presses)
    }

    /// [`detect_sequence`] with `leader` bound in place of the default.
    fn detect_sequence_with(leader: Leader, presses: &[(Modifiers, Key)]) -> Option<KeyAction> {
        // Accumulate: `press_key_modifiers` runs the key-down frame internally
        // and then a key-up frame, so we must not clobber the detection with the
        // later (keys-released) frame's `None`.
        let mut harness = Harness::new_ui_state(
            |ui, (chord, action): &mut (ChordState, Option<KeyAction>)| {
                if let Some(a) = check_keybindings(
                    ui.ctx(),
                    chord,
                    leader,
                    false,
                    false,
                    false,
                    AiMode::Agentic,
                ) {
                    *action = Some(a);
                }
            },
            (ChordState::default(), None),
        );
        harness.run();
        for (modifiers, key) in presses {
            harness.press_key_modifiers(*modifiers, *key);
        }
        harness.state().1.clone()
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
    fn escape_with_a_chord_pending_only_cancels_the_chord() {
        assert_eq!(detect_sequence(&[LEADER, (NONE, Key::Escape)]), None);
    }

    #[test]
    fn escape_with_no_chord_still_interrupts() {
        assert_eq!(detect(NONE, Key::Escape), Some(KeyAction::Interrupt));
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
    fn a_chord_lapses_after_its_timeout() {
        let leader = Leader::DEFAULT;
        let mut harness = Harness::new_ui_state(
            |ui, (chord, action): &mut (ChordState, Option<KeyAction>)| {
                if let Some(a) = check_keybindings(
                    ui.ctx(),
                    chord,
                    leader,
                    false,
                    false,
                    false,
                    AiMode::Agentic,
                ) {
                    *action = Some(a);
                }
            },
            (ChordState::default(), None),
        );
        harness.run();
        harness.press_key_modifiers(LEADER.0, LEADER.1);
        // Each step is a quarter second of simulated time.
        for _ in 0..(CHORD_TIMEOUT * 4.0) as usize + 1 {
            harness.step();
        }
        harness.press_key_modifiers(NONE, Key::J);
        assert_eq!(harness.state().1, None, "j after the timeout is just a j");
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
        let input_id = egui::Id::new("chat_input");
        // A real text field: egui drops focus from an id no widget claims.
        let mut harness = Harness::new_ui_state(
            |ui, (chord, text): &mut (ChordState, String)| {
                check_keybindings(
                    ui.ctx(),
                    chord,
                    Leader::DEFAULT,
                    false,
                    false,
                    false,
                    AiMode::Agentic,
                );
                ui.add(egui::TextEdit::singleline(text).id(input_id));
            },
            (ChordState::default(), String::new()),
        );
        // A real keypress carries its text alongside the key event.
        let type_key = |harness: &mut Harness<'_, (ChordState, String)>, key: Key, text: &str| {
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
        let input_id = egui::Id::new("chat_input");
        let mut harness = Harness::new_ui_state(
            |ui, (chord, text): &mut (ChordState, String)| {
                check_keybindings(
                    ui.ctx(),
                    chord,
                    Leader::DEFAULT,
                    false,
                    false,
                    false,
                    AiMode::Agentic,
                );
                ui.add(egui::TextEdit::singleline(text).id(input_id));
            },
            (ChordState::default(), String::new()),
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
