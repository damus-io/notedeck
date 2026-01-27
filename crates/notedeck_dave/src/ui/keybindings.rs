use egui::Key;

/// Keybinding actions that can be triggered globally
#[derive(Debug, Clone, PartialEq)]
pub enum KeyAction {
    /// Accept/Allow a pending permission request
    AcceptPermission,
    /// Deny a pending permission request
    DenyPermission,
    /// Switch to agent by number (0-indexed)
    SwitchToAgent(usize),
    /// Cycle to next agent
    NextAgent,
    /// Cycle to previous agent
    PreviousAgent,
    /// Spawn a new agent
    NewAgent,
    /// Interrupt/stop the current AI operation
    Interrupt,
}

/// Check for keybinding actions.
/// All keybindings use Ctrl modifier to avoid conflicts with text input.
pub fn check_keybindings(ctx: &egui::Context, has_pending_permission: bool) -> Option<KeyAction> {
    // Escape works even when text input has focus (to interrupt AI)
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        return Some(KeyAction::Interrupt);
    }

    let ctrl = egui::Modifiers::CTRL;
    let ctrl_shift = egui::Modifiers::CTRL | egui::Modifiers::SHIFT;

    // Ctrl+Tab / Ctrl+Shift+Tab for cycling through agents
    // Works even with text input focus since Ctrl modifier makes it unambiguous
    if let Some(action) = ctx.input_mut(|i| {
        if i.consume_key(ctrl, Key::Tab) {
            Some(KeyAction::NextAgent)
        } else if i.consume_key(ctrl_shift, Key::Tab) {
            Some(KeyAction::PreviousAgent)
        } else {
            None
        }
    }) {
        return Some(action);
    }

    // Ctrl+N to spawn a new agent (works even with text input focus)
    if ctx.input(|i| i.modifiers.matches_exact(ctrl) && i.key_pressed(Key::N)) {
        return Some(KeyAction::NewAgent);
    }

    // Ctrl+1-9 for switching agents (works even with text input focus)
    // When there's a pending permission, Ctrl+1 = accept, Ctrl+2 = deny
    if let Some(action) = ctx.input(|i| {
        if !i.modifiers.matches_exact(ctrl) {
            return None;
        }

        if has_pending_permission {
            if i.key_pressed(Key::Num1) {
                return Some(KeyAction::AcceptPermission);
            }
            if i.key_pressed(Key::Num2) {
                return Some(KeyAction::DenyPermission);
            }
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

    None
}
