//! The keymap engine runs every frame, before the active app renders, so its
//! frames with no key press must not allocate (AGENTS.md rule 18). A frame
//! with a key press may; that's a few a second at most.

use egui::{Event, Key, Modifiers};
use notedeck_chrome::keymap::{self, KeyEngine};
use notedeck_testing::alloc::{measure, CountingAllocator};

/// Process-wide and one per binary, so it lives in this test rather than in
/// `notedeck_testing`.
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Enough bindings that a lookup has scopes and sorted bindings to search.
const KEYMAP: &str = r#"
[[bind]]
scope = "app.pane"
keys = "j"
tool = "down"

[[bind]]
scope = "app"
keys = "g g"
tool = "first"

[[bind]]
scope = "app"
keys = "?"
tool = "help"
"#;

/// The scopes an app would report.
const SCOPES: &[&str] = &["app.pane", "app"];

/// Allocations on this thread by [`KeyEngine::resolve`] alone, in a frame at
/// `time` whose input is `events`.
fn resolve_allocs(
    engine: &mut KeyEngine,
    ctx: &egui::Context,
    time: f64,
    events: Vec<Event>,
) -> u64 {
    let input = egui::RawInput {
        time: Some(time),
        events,
        ..Default::default()
    };
    let mut allocs = 0;
    ctx.run_ui(input, |ui| {
        let ctx = ui.ctx();
        let (_, counts) = measure(|| {
            engine
                .resolve(ctx, keymap::Scopes::new(SCOPES), |_| true)
                .is_some()
        });
        allocs = counts.thread.allocs;
    })
    .drop_without_applying_deltas();
    allocs
}

/// One pressed `key`.
fn press(key: Key) -> Vec<Event> {
    vec![Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }]
}

#[test]
fn a_frame_with_no_key_press_does_not_allocate() {
    let ctx = egui::Context::default();
    let mut engine = KeyEngine::new(keymap::parse(KEYMAP).expect("keymap"));
    // Warm egui's per-pass state.
    for i in 0..5 {
        resolve_allocs(&mut engine, &ctx, i as f64 / 60.0, Vec::new());
    }

    let idle = resolve_allocs(&mut engine, &ctx, 0.1, Vec::new());
    assert_eq!(idle, 0, "an idle frame allocated {idle} times");

    // Key presses may allocate; they're here to leave a chord pending.
    resolve_allocs(&mut engine, &ctx, 0.2, press(Key::G));
    assert!(engine.pending().is_some(), "g starts a chord");

    // The chord's first waiting frame is egui's first `request_repaint_after`
    // on this context, which sets up its bookkeeping once; measure the next.
    resolve_allocs(&mut engine, &ctx, 0.3, Vec::new());
    let waiting = resolve_allocs(&mut engine, &ctx, 0.4, Vec::new());
    assert_eq!(
        waiting, 0,
        "a frame waiting on a chord allocated {waiting} times"
    );
}

/// The which-key strip walks the keymap every frame it shows; listing it, the
/// full table or a chord's continuations, doesn't allocate (the drawing does).
#[test]
fn listing_the_which_key_strip_does_not_allocate() {
    let keymap = keymap::parse(KEYMAP).expect("keymap");
    let scopes = keymap::Scopes::new(SCOPES);
    let pending = keymap::KeySeq::parse("g").ok();

    let (shown, counts) = measure(|| {
        let full = keymap.hints(scopes, None, |_| true).count();
        let chord = keymap.hints(scopes, pending, |_| true).count();
        (full, chord)
    });
    assert_eq!(shown, (3, 1));
    assert_eq!(counts.thread.allocs, 0, "listing allocated");
}
