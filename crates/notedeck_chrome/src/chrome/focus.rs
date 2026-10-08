//! Per-app keyboard focus across app switches.
//!
//! egui drops a widget's focus once it isn't rendered for a frame, so
//! switching apps (Ctrl+Tab, a tab click, a global back) would otherwise lose
//! focus until the user clicks back in. [`AppFocus`] remembers what each app
//! had focused when it last rendered and puts it back when the app becomes
//! active again. It's generic: no per-app code needed.

use std::collections::HashMap;

/// What each app had focused when it last rendered, keyed by app index.
#[derive(Default)]
pub(super) struct AppFocus {
    /// `Some(None)` is a remembered "nothing focused", e.g. a field the user
    /// left with Esc, which is different from never having seen the app
    /// (no entry), where the app's autofocus widget may claim focus instead.
    remembered: HashMap<usize, Option<egui::Id>>,
}

impl AppFocus {
    /// Put back `app`'s remembered focus on the frame it becomes active,
    /// before it renders. An app left with nothing focused gets nothing, so
    /// keys reach its keymap rather than a field the user already left. An
    /// app never seen before asks its autofocus widget (if any) to grab
    /// focus, like browser autofocus.
    pub(super) fn restore(&self, ctx: &egui::Context, app: usize) {
        match self.remembered.get(&app) {
            Some(Some(id)) => ctx.memory_mut(|m| m.request_focus(*id)),
            // Clear the outgoing app's focus too: it can still be drawn
            // (sliding out) and would keep the keys.
            Some(None) => ctx.memory_mut(|m| m.stop_text_input()),
            None => {
                ctx.memory_mut(|m| m.stop_text_input());
                notedeck_ui::request_autofocus(ctx);
            }
        }
    }

    /// Note what `app` has focused as a frame it rendered ends, including
    /// nothing: a field the user left (Esc, Enter, a click elsewhere) stays
    /// left when they come back.
    ///
    /// `app` must be the app that rendered this frame, which isn't always the
    /// active one: an open or a back raised mid-render switches the active
    /// app after the outgoing one has drawn, and its focus is the outgoing
    /// app's.
    pub(super) fn remember(&mut self, ctx: &egui::Context, app: usize) {
        self.remembered.insert(app, ctx.memory(|m| m.focused()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Key, Modifiers, RawInput};

    const APP: usize = 1;

    fn field_id() -> egui::Id {
        egui::Id::unique("focus_test_field")
    }

    /// Run one pass with `events` that draws the test field when `draw` is
    /// set, then let `after` look at the frame before it ends.
    fn pass(
        ctx: &egui::Context,
        events: Vec<Event>,
        draw: bool,
        mut after: impl FnMut(&egui::Context),
    ) {
        let input = RawInput {
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            if draw {
                let mut text = String::new();
                ui.add(egui::TextEdit::singleline(&mut text).id(field_id()));
            }
            after(ui.ctx());
        });
        // Nothing paints these passes; drop the font atlas upload unapplied.
        output.textures_delta.clear();
    }

    fn key(key: Key) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    /// A context whose test field holds focus, remembered for `APP`.
    fn focused_field(focus: &mut AppFocus) -> egui::Context {
        let ctx = egui::Context::default();
        ctx.memory_mut(|m| m.request_focus(field_id()));
        pass(&ctx, vec![], true, |_| {});
        pass(&ctx, vec![], true, |ctx| focus.remember(ctx, APP));
        assert_eq!(ctx.memory(|m| m.focused()), Some(field_id()));
        ctx
    }

    /// Switch away (the field stops drawing) and back, restoring `APP`.
    fn switch_back(ctx: &egui::Context, focus: &AppFocus) -> Option<egui::Id> {
        pass(ctx, vec![], false, |_| {});
        pass(ctx, vec![], false, |_| {});
        pass(ctx, vec![], true, |ctx| {
            focus.restore(ctx, APP);
        });
        ctx.memory(|m| m.focused())
    }

    #[test]
    fn a_focused_field_comes_back() {
        let mut focus = AppFocus::default();
        let ctx = focused_field(&mut focus);
        assert_eq!(switch_back(&ctx, &focus), Some(field_id()));
    }

    #[test]
    fn a_field_left_with_esc_stays_left() {
        let mut focus = AppFocus::default();
        let ctx = focused_field(&mut focus);
        pass(&ctx, vec![key(Key::Escape)], true, |ctx| {
            focus.remember(ctx, APP)
        });
        assert_eq!(switch_back(&ctx, &focus), None);
    }

    #[test]
    fn an_app_never_seen_gets_nothing() {
        let mut focus = AppFocus::default();
        let ctx = focused_field(&mut focus);
        pass(&ctx, vec![], true, |ctx| focus.restore(ctx, APP + 1));
        assert_eq!(ctx.memory(|m| m.focused()), None);
    }
}
