//! `egui_kittest` helpers that keep the behaviour notedeck's test suites were
//! written against on egui 0.31.
//!
//! ## Key presses
//!
//! Since egui 0.36, `Harness::key_press` and `key_press_modifiers` only queue
//! events, and `Harness::step` then runs one frame per queued event: a single
//! press with modifiers becomes four frames (modifiers down, key down, key up,
//! modifiers up), each `step_dt` of simulated time apart. The chord and
//! keybinding tests count frames and time, so they keep egui 0.31's timing
//! through this trait instead.

use egui::{Event, Key, Modifiers};
use egui_kittest::{Harness, HarnessBuilder};

/// Run `add_contents` in a `Ui` covering the whole window, as an app's root
/// `Ui` does.
///
/// `Harness::build_ui_state` hands its closure a `Ui` inset by an 8pt margin.
/// A snapshot of a whole app is taken edge to edge, as it was with egui 0.31's
/// `build_state`, which drew straight onto the `Context`.
pub fn full_window<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let rect = ui.ctx().content_rect();
    let mut window = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    window.set_clip_rect(rect);
    add_contents(&mut window)
}

/// A harness builder with egui 0.31's kittest defaults: a glyph no installed
/// font has draws as tofu instead of panicking, and an input widget with no
/// accessible name passes.
///
/// egui 0.36 made both of those test failures. Suites whose harnesses skip
/// notedeck's fonts (so symbols like `▼` or `⇧` are missing from egui's
/// defaults) or that render product inputs not yet given accessible names use
/// this until they are ported to the stricter checks.
pub fn lenient_builder<S>() -> HarnessBuilder<S> {
    Harness::<S>::builder()
        .allow_missing_glyphs()
        .with_accessibility_check(false)
}

/// egui 0.31's `Harness::press_key` and `Harness::press_key_modifiers`.
pub trait PressKey {
    /// Put `key`'s press and release into the next frame's input, without
    /// running a frame.
    fn press_key(&mut self, key: Key);

    /// Hold `modifiers`, run the frame that sees `key` go down, then put its
    /// release into the next frame's input and let go of `modifiers`.
    fn press_key_modifiers(&mut self, modifiers: Modifiers, key: Key);
}

/// The modifiers the next frame will see: the latest queued change, or else
/// what the last frame ended with.
fn next_frame_modifiers<S>(harness: &Harness<'_, S>) -> Modifiers {
    harness
        .input()
        .events
        .iter()
        .rev()
        .find_map(|event| match event {
            Event::ModifiersChanged(modifiers) => Some(*modifiers),
            _ => None,
        })
        .unwrap_or_else(|| harness.ctx.input(|i| i.modifiers))
}

fn key_event(key: Key, pressed: bool, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        pressed,
        modifiers,
        repeat: false,
        physical_key: None,
    }
}

impl<S> PressKey for Harness<'_, S> {
    fn press_key(&mut self, key: Key) {
        let modifiers = next_frame_modifiers(self);
        let events = &mut self.input_mut().events;
        events.push(key_event(key, true, modifiers));
        events.push(key_event(key, false, modifiers));
    }

    fn press_key_modifiers(&mut self, modifiers: Modifiers, key: Key) {
        // Modifiers are input state, set by events since egui 0.36.
        let previous = next_frame_modifiers(self);
        let events = &mut self.input_mut().events;
        events.push(Event::ModifiersChanged(previous | modifiers));
        events.push(key_event(key, true, modifiers));
        self.step();
        let events = &mut self.input_mut().events;
        events.push(key_event(key, false, modifiers));
        events.push(Event::ModifiersChanged(previous));
    }
}
