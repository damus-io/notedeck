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
    /// Toggle plan mode for the active session (Ctrl+M)
    TogglePlanMode,
    /// Delete the active session
    DeleteActiveSession,
    /// Navigate to next item in focus queue (Ctrl+N)
    FocusQueueNext,
    /// Navigate to previous item in focus queue (Ctrl+P)
    FocusQueuePrev,
    /// Dismiss current item from focus queue (Ctrl+D)
    FocusQueueDismiss,
}

/// Check for keybinding actions.
/// Most keybindings use Ctrl modifier to avoid conflicts with text input.
/// Exception: 1/2 for permission responses work without Ctrl but only when no text input has focus.
pub fn check_keybindings(
    ctx: &egui::Context,
    has_pending_permission: bool,
    has_pending_question: bool,
    in_tentative_state: bool,
) -> Option<KeyAction> {
    // Escape in tentative state cancels the tentative mode
    if in_tentative_state && ctx.input(|i| i.key_pressed(Key::Escape)) {
        return Some(KeyAction::CancelTentative);
    }

    // Escape otherwise works to interrupt AI (even when text input has focus)
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        return Some(KeyAction::Interrupt);
    }

    let ctrl = egui::Modifiers::CTRL;
    let ctrl_shift = egui::Modifiers::CTRL | egui::Modifiers::SHIFT;

    // Ctrl+Tab / Ctrl+Shift+Tab for cycling through agents
    // Works even with text input focus since Ctrl modifier makes it unambiguous
    // IMPORTANT: Check Ctrl+Shift+Tab first because consume_key uses matches_logically
    // which ignores extra Shift, so Ctrl+Tab would consume Ctrl+Shift+Tab otherwise
    if let Some(action) = ctx.input_mut(|i| {
        if i.consume_key(ctrl_shift, Key::Tab) {
            Some(KeyAction::PreviousAgent)
        } else if i.consume_key(ctrl, Key::Tab) {
            Some(KeyAction::NextAgent)
        } else {
            None
        }
    }) {
        return Some(action);
    }

    // Ctrl+N for higher priority (toward NeedsInput)
    if ctx.input(|i| i.modifiers.matches_exact(ctrl) && i.key_pressed(Key::N)) {
        return Some(KeyAction::FocusQueuePrev);
    }

    // Ctrl+P for lower priority (toward Done)
    if ctx.input(|i| i.modifiers.matches_exact(ctrl) && i.key_pressed(Key::P)) {
        return Some(KeyAction::FocusQueueNext);
    }

    // Ctrl+T to spawn a new agent
    if ctx.input(|i| i.modifiers.matches_exact(ctrl) && i.key_pressed(Key::T)) {
        return Some(KeyAction::NewAgent);
    }

    // Ctrl+G to toggle between scene view and classic view
    if ctx.input(|i| i.modifiers.matches_exact(ctrl) && i.key_pressed(Key::G)) {
        return Some(KeyAction::ToggleView);
    }

    // Ctrl+M to toggle plan mode
    if ctx.input(|i| i.modifiers.matches_exact(ctrl) && i.key_pressed(Key::M)) {
        return Some(KeyAction::TogglePlanMode);
    }

    // Ctrl+D to dismiss current item from focus queue
    if ctx.input(|i| i.modifiers.matches_exact(ctrl) && i.key_pressed(Key::D)) {
        return Some(KeyAction::FocusQueueDismiss);
    }

    // Delete key to delete active session (only when no text input has focus)
    if !ctx.wants_keyboard_input() && ctx.input(|i| i.key_pressed(Key::Delete)) {
        return Some(KeyAction::DeleteActiveSession);
    }

    // Ctrl+1-9 for switching agents (works even with text input focus)
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

    // When there's a pending permission (but NOT an AskUserQuestion):
    // - 1 = accept, 2 = deny (no modifiers)
    // - Shift+1 = tentative accept, Shift+2 = tentative deny (for adding message)
    // This is checked AFTER Ctrl+number so Ctrl bindings take precedence
    // IMPORTANT: Only handle these when no text input has focus, to avoid
    // capturing keypresses when user is typing a message in tentative state
    // AskUserQuestion uses number keys for option selection, so we skip these bindings
    if has_pending_permission && !has_pending_question && !ctx.wants_keyboard_input() {
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
            None
        }) {
            return Some(action);
        }

        // Bare keypresses (no modifiers) for immediate accept/deny
        if let Some(action) = ctx.input(|i| {
            if !i.modifiers.any() {
                if i.key_pressed(Key::Num1) {
                    return Some(KeyAction::AcceptPermission);
                } else if i.key_pressed(Key::Num2) {
                    return Some(KeyAction::DenyPermission);
                }
            }
            None
        }) {
            return Some(action);
        }
    }

    None
}
