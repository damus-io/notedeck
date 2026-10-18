//! A full chrome driven frame by frame, for tests: the host's own frame
//! ([`Notedeck::tick_app`]) around a [`Chrome`] the test owns, so a test can
//! reach into the chrome between frames without its frame drifting from the
//! shipped one.
//!
//! [`chrome_frame`] is the frame alone, for a harness that brings its own
//! loop (egui_kittest); [`ChromeDriver`] adds the loop: input time, a fixed
//! window, key presses, the text each frame painted and the URLs it opened.
//!
//! Ungated, so chrome's own integration tests can use it without enabling a
//! feature of their own crate. The driver doesn't own the notedeck's data
//! directory: a test keeps its `TempDir` alive beside it.

use egui::{Event, Key, Modifiers};
use notedeck::Notedeck;

use crate::Chrome;

/// The window a [`ChromeDriver`] draws into.
const SCREEN: egui::Vec2 = egui::vec2(1200.0, 800.0);

/// The input time between two [`ChromeDriver`] frames, as at the app's 60 Hz.
const FRAME_DT: f64 = 1.0 / 60.0;

/// How long [`ChromeDriver::frames_until`] lets background threads (nostrdb
/// ingest, a relay) run between two frames.
const FRAME_GAP: std::time::Duration = std::time::Duration::from_millis(5);

/// One chrome frame, as the host runs it: the background work, the tool
/// registry following the running apps, the chrome's `update`, the main panel
/// and the chrome's `render`.
pub fn chrome_frame(notedeck: &mut Notedeck, chrome: &mut Chrome, ui: &mut egui::Ui) {
    notedeck.tick_app(chrome, ui);
}

/// A headless chrome driven through [`chrome_frame`], one frame per call,
/// recording the text each frame painted.
pub struct ChromeDriver {
    pub egui_ctx: egui::Context,
    pub notedeck: Notedeck,
    pub chrome: Chrome,
    /// The input time of the next frame.
    time: f64,
    /// Every piece of text the last frame painted.
    drawn: Vec<String>,
    /// Every URL a frame asked the platform to open, in order.
    opened: Vec<String>,
}

impl ChromeDriver {
    /// Drive `chrome` on `notedeck`, both built on `egui_ctx`. Installs
    /// notedeck's fonts and theme and runs two frames: fonts apply on the
    /// pass after they're set, and apps set themselves up on their first
    /// update.
    pub fn new(egui_ctx: egui::Context, notedeck: Notedeck, chrome: Chrome) -> Self {
        notedeck.setup(&egui_ctx);
        let mut driver = Self {
            egui_ctx,
            notedeck,
            chrome,
            time: 0.0,
            drawn: Vec::new(),
            opened: Vec::new(),
        };
        driver.frame(Vec::new());
        driver.frame(Vec::new());
        driver
    }

    /// One frame with `events` as its input, a sixtieth of a second after
    /// the last.
    pub fn frame(&mut self, events: Vec<Event>) {
        let input = egui::RawInput {
            time: Some(self.time),
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
            events,
            ..Default::default()
        };
        self.time += FRAME_DT;

        let Self {
            egui_ctx,
            notedeck,
            chrome,
            drawn,
            opened,
            ..
        } = self;
        let output = egui_ctx.run_ui(input, |ui| chrome_frame(notedeck, chrome, ui));
        drawn.clear();
        for clipped in &output.shapes {
            collect_text(&clipped.shape, drawn);
        }
        for command in &output.platform_output.commands {
            if let egui::OutputCommand::OpenUrl(open) = command {
                opened.push(open.url.clone());
            }
        }
        output.drop_without_applying_deltas();
    }

    /// One frame with `key` pressed (typing `text`, if any) and released.
    pub fn press(&mut self, modifiers: Modifiers, key: Key, text: Option<&str>) {
        let event = |pressed| Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        };
        let mut events = vec![event(true)];
        if let Some(text) = text {
            events.push(Event::Text(text.to_owned()));
        }
        events.push(event(false));
        self.frame(events);
    }

    /// Press a bare key.
    pub fn key(&mut self, key: Key) {
        self.press(Modifiers::NONE, key, None);
    }

    /// Run idle frames until `secs` of input time have passed.
    pub fn idle(&mut self, secs: f64) {
        let until = self.time + secs;
        while self.time < until {
            self.frame(Vec::new());
        }
    }

    /// The input time of the next frame.
    pub fn time(&self) -> f64 {
        self.time
    }

    /// Every piece of text the last frame painted.
    pub fn drawn(&self) -> &[String] {
        &self.drawn
    }

    /// Every URL a frame asked the platform to open so far, in order.
    pub fn opened(&self) -> &[String] {
        &self.opened
    }

    /// Whether the last frame painted exactly `s` somewhere.
    pub fn drew(&self, s: &str) -> bool {
        self.drawn.iter().any(|drawn| drawn == s)
    }

    /// Run frames until `done` holds, at most `max` of them, or panic naming
    /// `what` and what the last frame drew. Checks before each frame, so
    /// it runs none when `done` already holds, and sleeps a moment between
    /// frames for work landing from other threads.
    pub fn frames_until(&mut self, what: &str, max: usize, done: impl Fn(&Self) -> bool) {
        for _ in 0..max {
            if done(self) {
                return;
            }
            self.frame(Vec::new());
            std::thread::sleep(FRAME_GAP);
        }
        assert!(
            done(self),
            "{what} not after {max} frames; the last drew {:?}",
            self.drawn
        );
    }
}

/// Push the text of every text shape in `shape` onto `out`.
pub fn collect_text(shape: &egui::Shape, out: &mut Vec<String>) {
    match shape {
        egui::Shape::Text(text) => out.push(text.galley.text().to_owned()),
        egui::Shape::Vec(shapes) => {
            for shape in shapes {
                collect_text(shape, out);
            }
        }
        _ => {}
    }
}
