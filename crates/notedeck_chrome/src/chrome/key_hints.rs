//! The which-key strip: a row of keycaps along the bottom of the app area,
//! generated from the keymap for the active app's key scopes (see
//! [`Keymap::hints`](crate::keymap::Keymap::hints)).
//!
//! It shows itself while a chord is pending, listing what can finish it, and
//! otherwise the whole table once `?` ([`TOGGLE_KEY_HINTS`]) pins it, per app.
//! Both are the chrome's own UI tools, owned by the shell rather than an app.

use super::{Chrome, MAX_APPS};
use crate::keymap::{Hint, Scopes};
use notedeck::{App, AppContext, ToolInvocation, ToolOutcome, ToolRegistry, ToolSpec};
use oot_bitset::{bitset_clear, bitset_get, bitset_set};

/// Pins or unpins the active app's which-key strip. Bound to `?`.
pub const TOGGLE_KEY_HINTS: &str = "chrome_toggle_key_hints";

/// Unpins the active app's which-key strip; only available while it is pinned,
/// so its key (Esc) falls through otherwise.
pub const HIDE_KEY_HINTS: &str = "chrome_hide_key_hints";

/// Whether the which-key strip's tool `name` would do something now, or
/// `None` for another tool: `?` only in an app that reports key scopes
/// (`has_scopes`), so any other app keeps the key, and the strip's Esc only
/// while `?` has `pinned` it, so Esc falls through otherwise. The chrome's
/// rule, public so a harness standing in for the chrome answers the same
/// (see [`KeyTools`](crate::keymap::KeyTools)).
pub fn key_hints_tool_available(name: &str, has_scopes: bool, pinned: bool) -> Option<bool> {
    match name {
        TOGGLE_KEY_HINTS => Some(has_scopes),
        HIDE_KEY_HINTS => Some(pinned),
        _ => None,
    }
}

/// The which-key strip's UI tools, for the host's tool registry; the rest of
/// the chrome's own are the keymap editor's.
pub(super) fn chrome_ui_tools() -> [ToolSpec; 2] {
    [
        ToolSpec::new(
            TOGGLE_KEY_HINTS,
            "Show or hide the which-key strip listing the active app's key bindings.",
            Vec::new(),
        )
        .titled("Toggle key hints"),
        ToolSpec::new(
            HIDE_KEY_HINTS,
            "Hide the which-key strip, if it is showing.",
            Vec::new(),
        )
        .titled("Hide key hints"),
    ]
}

impl Chrome {
    /// Run the chrome's own UI tool `invocation`, or `None` when it isn't
    /// one: the which-key strip's here, the keymap editor's in
    /// [`call_keymap_editor_tool`](Self::call_keymap_editor_tool), the jump
    /// list's in [`call_jump_tool`](Self::call_jump_tool), the nav stack's in
    /// [`call_nav_tool`](Self::call_nav_tool), the command bar's in
    /// [`call_command_bar_tool`](Self::call_command_bar_tool).
    pub(super) fn call_chrome_tool(
        &mut self,
        ctx: &mut AppContext,
        invocation: &ToolInvocation,
    ) -> Option<ToolOutcome> {
        if let Some(outcome) = self.call_keymap_editor_tool(ctx, invocation) {
            return Some(outcome);
        }
        if let Some(outcome) = self.call_jump_tool(ctx, invocation) {
            return Some(outcome);
        }
        if let Some(outcome) = self.call_nav_tool(ctx, invocation) {
            return Some(outcome);
        }
        if let Some(outcome) = self.call_command_bar_tool(ctx, invocation) {
            return Some(outcome);
        }
        if let Some(outcome) = self.call_open_tool(ctx, invocation) {
            return Some(outcome);
        }
        let pinned = self.key_hints_pinned();
        let pin = match invocation.name.as_str() {
            TOGGLE_KEY_HINTS => !pinned,
            HIDE_KEY_HINTS => false,
            _ => return None,
        };
        let Some(slot) = self.active_slot() else {
            return Some(ToolOutcome::Error("no app is active".to_owned()));
        };
        if pin {
            bitset_set(&mut self.key_hints, slot as u16);
        } else {
            bitset_clear(&mut self.key_hints, slot as u16);
        }
        Some(ToolOutcome::Data(serde_json::json!({ "shown": pin })))
    }

    /// Whether the chrome's own UI tool `name` would do something now, or
    /// `None` when `name` isn't one. The strip only lists the keymap's
    /// bindings, so `?` is only taken in an app that reports key scopes; any
    /// other app keeps the key.
    pub(super) fn chrome_tool_available(&self, name: &str) -> Option<bool> {
        if let Some(available) = self.keymap_editor_tool_available(name) {
            return Some(available);
        }
        if let Some(available) = self.jump_tool_available(name) {
            return Some(available);
        }
        if let Some(available) = self.nav_tool_available(name) {
            return Some(available);
        }
        if let Some(available) = self.command_bar_tool_available(name) {
            return Some(available);
        }
        match name {
            // Any time: whether the reference opens is the call's to say.
            super::OPEN => Some(true),
            // Matched first so asking after an app's tool never asks the
            // app for its scopes.
            TOGGLE_KEY_HINTS | HIDE_KEY_HINTS => key_hints_tool_available(
                name,
                self.apps
                    .get(self.active as usize)
                    .is_some_and(|app| !app.key_scopes().is_empty()),
                self.key_hints_pinned(),
            ),
            _ => None,
        }
    }

    /// Whether `?` has pinned the active app's which-key strip open.
    pub fn key_hints_pinned(&self) -> bool {
        self.active_slot()
            .is_some_and(|slot| bitset_get(&self.key_hints, slot as u16))
    }

    /// The active app's slot, if it is one the bitsets can track.
    pub(super) fn active_slot(&self) -> Option<usize> {
        usize::try_from(self.active)
            .ok()
            .filter(|&slot| slot < MAX_APPS)
    }

    /// What the which-key strip shows this frame, or `None` when it's
    /// hidden: the keys that finish a pending chord, else, if `?` pinned it,
    /// every key the active app's scopes bind to an available tool. `tools` is
    /// the host's registry, for whether an app's tool is available. Lazy;
    /// allocates nothing.
    pub fn key_hints<'a>(
        &'a self,
        tools: &'a ToolRegistry,
    ) -> Option<impl Iterator<Item = Hint<'a>> + Clone + 'a> {
        let pending = self.keys.pending();
        if pending.is_none() && !self.key_hints_pinned() {
            return None;
        }
        let app = self.apps.get(self.active as usize)?;
        let scopes = Scopes::new(app.key_scopes()).with_current(app.current_ref());
        let available = move |name: &str| self.key_tool_available(tools, name);
        Some(self.keys.keymap().hints(scopes, pending, available))
    }

    /// Draw the which-key strip ([`key_hints`](Self::key_hints)) floating
    /// along the bottom of `rect`, the app's area, over the app rather than
    /// taking room from it, so a chord's strip coming and going never moves
    /// the app (see [`key_hints_overlay`]). Allocates nothing when there is
    /// nothing to show.
    ///
    /// [`key_hints_overlay`]: notedeck_ui::keybind_hint::key_hints_overlay
    #[profiling::function]
    pub(super) fn key_hints_ui(&self, ctx: &AppContext, ui: &egui::Ui, rect: egui::Rect) {
        let hints = self
            .key_hints(&ctx.registries.tools)
            .filter(|hints| hints.clone().next().is_some())
            .map(|hints| hints.map(|hint| (hint.keycap_iter(), hint.label)));
        let muted = notedeck::ColorTheme::current(ui.ctx()).text_muted;
        notedeck_ui::keybind_hint::key_hints_overlay(
            ui.ctx(),
            egui::Id::unique("chrome-key-hints"),
            rect,
            muted,
            hints,
        );
    }
}
