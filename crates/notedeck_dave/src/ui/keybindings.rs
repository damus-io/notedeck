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

/// Check for keybinding actions when no text input has focus
/// If `has_pending_permission` is true, keys 1/2 are used for permission responses
/// instead of agent switching.
pub fn check_keybindings(ctx: &egui::Context, has_pending_permission: bool) -> Option<KeyAction> {
    // Escape works even when text input has focus (to interrupt AI)
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        return Some(KeyAction::Interrupt);
    }

    // Only process other keys when no text input has focus
    if ctx.wants_keyboard_input() {
        return None;
    }

    ctx.input(|i| {
        // When there's a pending permission, 1 = accept, 2 = deny
        if has_pending_permission {
            if i.key_pressed(Key::Num1) {
                return Some(KeyAction::AcceptPermission);
            }
            if i.key_pressed(Key::Num2) {
                return Some(KeyAction::DenyPermission);
            }
        }

        // Number keys 1-9 for switching agents (when no pending permission)
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

        // Tab / Shift+Tab for cycling through agents
        if i.key_pressed(Key::Tab) {
            if i.modifiers.shift {
                return Some(KeyAction::PreviousAgent);
            } else {
                return Some(KeyAction::NextAgent);
            }
        }

        None
    })
}
