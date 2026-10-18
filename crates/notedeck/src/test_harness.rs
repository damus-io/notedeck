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
//!
//! ## An app's frame
//!
//! [`app_frame`] and [`app_builder`] hand a snapshot the `Ui` notedeck's
//! chrome hands an app: notedeck's fonts, `ColorTheme` visuals and text
//! styles, the root panel's fill and the chrome's zero horizontal item gap.
//! A harness on egui's defaults draws something the app never shows.

use crate::NotedeckOptions;
use egui::{Event, Key, Modifiers, ThemePreference};
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

/// Run `add_contents` the way notedeck's chrome hands an app its `Ui`:
/// notedeck's fonts, `ColorTheme` visuals and text styles; the root panel's
/// `panel_fill` edge to edge; the chrome's zero horizontal item gap.
///
/// The first frame on a context only installs that setup and asks for a
/// repaint, returning `None`: kittest's `build_ui` runs that frame before a
/// test can touch the context, and fonts set during a frame apply from the
/// next one. Every later frame draws and returns `Some`. The setup follows the
/// harness's theme (`HarnessBuilder::with_theme`).
///
/// An app adds its own root setup inside `add_contents`, as Dave does with its
/// item gap.
pub fn app_frame<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> Option<R> {
    let ctx = ui.ctx().clone();
    let installed = egui::Id::unique("notedeck_test_app_setup");
    if !ctx.data(|d| d.get_temp::<bool>(installed).unwrap_or(false)) {
        // What `Notedeck::setup` does for the real app.
        crate::setup::setup_egui_context(
            &ctx,
            NotedeckOptions::empty(),
            ThemePreference::from(ctx.theme()),
            1.0,
        );
        ctx.data_mut(|d| d.insert_temp(installed, true));
        ctx.request_repaint();
        return None;
    }

    Some(full_window(ui, |ui| {
        // The root panel's fill, as `main_panel` and the chrome's nav body
        // paint it under every app.
        let fill = ui.visuals().panel_fill;
        ui.painter().rect_filled(ui.max_rect(), 0.0, fill);
        // `Chrome::show` zeroes the gap for every app.
        ui.spacing_mut().item_spacing.x = 0.0;
        add_contents(ui)
    }))
}

/// A strict harness builder for [`app_frame`] harnesses: with notedeck's fonts
/// installed a missing glyph is a real bug, so it panics as egui 0.36 does by
/// default, and inputs must carry accessible names.
pub fn app_builder<S>() -> HarnessBuilder<S> {
    Harness::<S>::builder()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// After the setup frame, the app's `Ui` has notedeck's fonts, the dark
    /// `ColorTheme` panel fill and the chrome's zero gap, and a strict harness
    /// draws a glyph only notedeck's fonts carry.
    #[test]
    fn app_frame_sets_up_the_ui_like_the_chrome() {
        #[derive(Default)]
        struct Seen {
            frames: usize,
            item_gap: Option<f32>,
            panel_fill: Option<egui::Color32>,
        }

        let mut harness = app_builder().with_theme(egui::Theme::Dark).build_ui_state(
            |ui, seen: &mut Seen| {
                seen.frames += 1;
                app_frame(ui, |ui| {
                    seen.item_gap = Some(ui.spacing().item_spacing.x);
                    seen.panel_fill = Some(ui.visuals().panel_fill);
                    // ⇧ is in DejaVuSans, not egui's default fonts.
                    ui.label("⇧ shift");
                });
            },
            Seen::default(),
        );
        harness.run();

        let seen = harness.state();
        assert!(seen.frames >= 2, "the setup frame asks for a drawing frame");
        assert_eq!(seen.item_gap, Some(0.0));
        let dark = crate::theme::create_themed_visuals(
            crate::theme::dark_color_theme(),
            egui::Visuals::dark(),
        );
        assert_eq!(seen.panel_fill, Some(dark.panel_fill));

        let proportional = harness
            .ctx
            .fonts(|f| f.definitions().families[&egui::FontFamily::Proportional].clone());
        assert_eq!(proportional.first().map(String::as_str), Some("Onest"));
    }
}
