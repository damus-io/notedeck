//! A generic key-chord (key-prefix) state machine.
//!
//! Apps that bind multi-key sequences — vim's `gg`, `za`, a leader chord — share
//! the mechanics here: reading this frame's key press, timing a pending chord
//! out, and swallowing the keys a chord consumed. What each key *means* stays in
//! the app: it owns a `P` enum naming how far into a chord it is and matches on
//! `(pending, press)` itself.
//!
//! The state lives in [`ChordState`], owned by the app and passed in by `&mut`
//! each frame, never in a global.

use egui::{Event, Key, Modifiers};

/// How long a pending chord waits for its next key before it lapses, in
/// seconds. Re-armed by every [`ChordState::begin`], so a run of keys the chord
/// accepts never times out mid-stride.
pub const CHORD_TIMEOUT: f64 = 2.0;

/// One key press read off this frame's input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyPress {
    /// The key that went down.
    pub key: Key,
    /// The modifiers held with it.
    pub modifiers: Modifiers,
}

impl KeyPress {
    /// No Ctrl/Alt/Cmd — Shift is allowed (it selects `G` vs `g`). A non-bare
    /// press belongs to app/chrome shortcuts and must fall through.
    pub fn is_bare(&self) -> bool {
        let m = self.modifiers;
        !(m.ctrl || m.alt || m.command || m.mac_cmd)
    }
}

/// The first `Event::Key { pressed: true, .. }` this frame (repeats included,
/// so held j keeps moving). Releases are ignored.
pub fn first_key_press(input: &egui::InputState) -> Option<KeyPress> {
    input.events.iter().find_map(|event| match event {
        Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => Some(KeyPress {
            key: *key,
            modifiers: *modifiers,
        }),
        _ => None,
    })
}

/// Drop this frame's pressed-Key and Text events so a key the app handled
/// can't also type into a TextEdit that takes focus later this same frame
/// (e.g. `a` opening a composer, `/` focusing a filter). Releases are kept.
///
/// Trimming `i.events` does not desync [`egui::InputState::keys_down`]: that is
/// computed in `begin_pass` from the raw events, before any widget runs.
pub fn swallow_key_events(ctx: &egui::Context) {
    ctx.input_mut(|i| {
        i.events
            .retain(|e| !matches!(e, Event::Key { pressed: true, .. } | Event::Text(_)))
    });
}

/// Progress through a multi-key chord. `P` is the app's own "what's pending"
/// enum. Owned by the app state — no globals (AGENTS.md rule 6).
///
/// Each frame the app calls [`tick`](Self::tick) first, then reads
/// [`first_key_press`] and, depending on what is pending, either
/// [`begin`](Self::begin)s the next step or [`clear`](Self::clear)s the chord.
pub struct ChordState<P> {
    /// How far into a chord we are, or `None` when no chord is pending.
    pending: Option<P>,
    /// `ctx.input(|i| i.time)` past which the pending chord lapses.
    expires_at: f64,
}

/// Implemented by hand so `ChordState<P>` doesn't require `P: Default`.
impl<P> Default for ChordState<P> {
    fn default() -> Self {
        Self {
            pending: None,
            expires_at: 0.0,
        }
    }
}

impl<P: Copy> ChordState<P> {
    /// How far into a chord we are, or `None` when no chord is pending.
    pub fn pending(&self) -> Option<P> {
        self.pending
    }

    /// Start or advance a chord, re-arming the timeout from `now`.
    pub fn begin(&mut self, pending: P, now: f64) {
        self.pending = Some(pending);
        self.expires_at = now + CHORD_TIMEOUT;
    }

    /// End the chord, whether it completed or was cancelled.
    pub fn clear(&mut self) {
        self.pending = None;
    }

    /// Call once per frame before reading keys. Clears a lapsed chord and
    /// returns `None`; otherwise schedules `request_repaint_after` for the
    /// remaining time (so the timeout fires with no input) and returns the
    /// still-pending step.
    pub fn tick(&mut self, ctx: &egui::Context) -> Option<P> {
        let pending = self.pending?;

        let now = ctx.input(|i| i.time);
        let remaining = self.expires_at - now;
        if remaining <= 0.0 {
            self.clear();
            return None;
        }

        ctx.request_repaint_after(std::time::Duration::from_secs_f64(remaining));
        Some(pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use notedeck::test_harness::PressKey;

    /// A one-step chord, standing in for an app's pending enum.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Pending {
        /// `g` was pressed; waiting for the second `g`.
        G,
    }

    /// What a chord-driving test frame leaves behind.
    #[derive(Default)]
    struct ChordHarness {
        chord: ChordState<Pending>,
        /// What the most recent frame's `tick` returned.
        ticked: Option<Pending>,
    }

    /// A harness that ticks the chord every frame and (re)begins it on `g`.
    fn chord_harness() -> Harness<'static, ChordHarness> {
        let mut harness = Harness::new_ui_state(
            |ui, state: &mut ChordHarness| {
                let ctx = ui.ctx();
                state.ticked = state.chord.tick(ctx);
                let press = ctx.input(first_key_press);
                if press.is_some_and(|p| p.key == Key::G) {
                    let now = ctx.input(|i| i.time);
                    state.chord.begin(Pending::G, now);
                }
            },
            ChordHarness::default(),
        );
        harness.run();
        harness
    }

    #[test]
    fn first_key_press_reads_the_down_frame_only() {
        // Accumulate per frame: `press_key_modifiers` runs the key-down frame and
        // queues the release for the next step, so a single slot would be
        // clobbered by the up-frame's `None`.
        let mut harness = Harness::new_ui_state(
            |ui, presses: &mut Vec<Option<KeyPress>>| {
                presses.push(ui.ctx().input(first_key_press));
            },
            Vec::new(),
        );
        harness.run();
        harness.state_mut().clear();

        harness.press_key_modifiers(Modifiers::NONE, Key::J);
        harness.step();

        let j = KeyPress {
            key: Key::J,
            modifiers: Modifiers::NONE,
        };
        assert_eq!(harness.state().as_slice(), &[Some(j), None]);
    }

    #[test]
    fn is_bare_allows_shift_but_not_ctrl_alt_or_cmd() {
        let press = |modifiers, key| KeyPress { key, modifiers };
        assert!(press(Modifiers::NONE, Key::J).is_bare());
        assert!(press(Modifiers::SHIFT, Key::G).is_bare());
        assert!(!press(Modifiers::CTRL, Key::J).is_bare());
        assert!(!press(Modifiers::ALT, Key::H).is_bare());
        assert!(!press(Modifiers::COMMAND, Key::W).is_bare());
        assert!(!press(Modifiers::MAC_CMD, Key::W).is_bare());
    }

    #[test]
    fn a_chord_lapses_after_its_timeout() {
        let mut harness = chord_harness();
        harness.press_key_modifiers(Modifiers::NONE, Key::G);
        assert_eq!(harness.state().chord.pending(), Some(Pending::G));

        // Each step is a quarter second of simulated time.
        for _ in 0..(CHORD_TIMEOUT * 4.0) as usize + 1 {
            harness.step();
        }
        assert_eq!(harness.state().ticked, None);
        assert_eq!(harness.state().chord.pending(), None);
    }

    #[test]
    fn a_second_begin_rearms_the_timeout() {
        let mut harness = chord_harness();
        let first = harness.ctx.input(|i| i.time);
        harness.press_key_modifiers(Modifiers::NONE, Key::G);

        // Halfway to the first deadline, press `g` again.
        for _ in 0..(CHORD_TIMEOUT * 2.0) as usize {
            harness.step();
        }
        harness.press_key_modifiers(Modifiers::NONE, Key::G);

        // Walk past the first deadline, but not the second.
        while harness.ctx.input(|i| i.time) <= first + CHORD_TIMEOUT + 0.25 {
            harness.step();
        }
        assert_eq!(harness.state().ticked, Some(Pending::G));
        assert_eq!(harness.state().chord.pending(), Some(Pending::G));
    }

    /// Feed `a` (its key press and its text) to a focused text field, calling
    /// [`swallow_key_events`] before the field lays out when `swallow` is set,
    /// and return what the field ends up holding.
    fn type_a_into_focused_field(swallow: bool) -> String {
        let input_id = egui::Id::unique("chord_test_input");
        let mut harness = Harness::new_ui_state(
            |ui, text: &mut String| {
                if swallow {
                    swallow_key_events(ui.ctx());
                }
                ui.add(egui::TextEdit::singleline(text).id(input_id))
                    .accessible_name("test field");
            },
            String::new(),
        );
        harness.ctx.memory_mut(|m| m.request_focus(input_id));
        harness.run();

        // A real keypress carries its text alongside the key event.
        harness.input_mut().events.push(Event::Text("a".to_owned()));
        harness.press_key_modifiers(Modifiers::NONE, Key::A);
        harness.step();
        harness.state().clone()
    }

    #[test]
    fn swallow_key_events_keeps_a_handled_key_out_of_a_text_field() {
        assert_eq!(
            type_a_into_focused_field(false),
            "a",
            "unswallowed, it types"
        );
        assert_eq!(type_a_into_focused_field(true), "", "swallowed, it doesn't");
    }
}
