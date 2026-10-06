//! Columns' keyboard input through the real chrome: it is read only while
//! Columns is on screen. Opened apps all get `update` every frame whether or
//! not they are in front, so input read there is stolen from whichever app
//! is. Escape is the one that shows: Columns consumes it to go back a route,
//! out from under the app the user is actually typing at.
//!
//! Release only, like `headway_nav_tests`: Columns' own per-column nav still
//! slides a pushed route in, drawing two layers whose widget ids repeat, which
//! egui debug-asserts against. The chrome's app-switch slide is off, so a
//! switch lands on the frame it's made.

use nostrdb_net::FullKeypair;
use notedeck::{App, NavStack, Notedeck};
use notedeck_columns::Route;

use super::Chrome;

/// How many frames a slide gets to land: two seconds of them at 60 Hz.
const FRAME_CAP: usize = 120;

/// A chrome built the way the app builds one, with Columns opened and one
/// route deep, so it has somewhere to go back to.
struct Fixture {
    _dir: tempfile::TempDir,
    egui_ctx: egui::Context,
    notedeck: Notedeck,
    chrome: Chrome,
    time: f64,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::TempDir::new().expect("tmp dir");
        let kp = FullKeypair::generate();
        let args: Vec<String> = [
            "notedeck-test",
            "--testrunner",
            "--nsec",
            &kp.secret_key.to_secret_hex(),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let egui_ctx = egui::Context::default();
        let mut notedeck = Notedeck::init(&egui_ctx, dir.path(), &args);
        {
            let app_ctx = notedeck.app_context();
            app_ctx.settings.complete_welcome();
            app_ctx.settings.get_settings_mut().animate_nav_transitions = false;
        }
        notedeck.setup(&egui_ctx);
        let chrome = Chrome::new_headless(&args, &mut notedeck).expect("chrome");

        let mut fixture = Self {
            _dir: dir,
            egui_ctx,
            notedeck,
            chrome,
            time: 0.0,
        };
        // Fonts are applied on the next pass, and Columns initializes its
        // columns on its first update; Settings then gives it a route to
        // leave.
        fixture.frame(Vec::new());
        fixture.frame(Vec::new());
        fixture.columns_router().route_to(Route::Settings);
        fixture.settle();
        fixture
    }

    /// One real chrome frame — `update` then `render`, as the host runs them
    /// — with `events` as its input.
    fn frame(&mut self, events: Vec<egui::Event>) {
        let input = egui::RawInput {
            time: Some(self.time),
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            events,
            ..Default::default()
        };
        self.time += 1.0 / 60.0;

        let Self {
            egui_ctx,
            notedeck,
            chrome,
            ..
        } = self;
        let mut app_ctx = notedeck.app_context();
        egui_ctx
            .run_ui(input, |ui| {
                chrome.update(&mut app_ctx);
                egui::CentralPanel::default().show(ui, |ui| {
                    chrome.render(&mut app_ctx, ui);
                });
            })
            .drop_without_applying_deltas();
    }

    /// Run frames until Columns' selected column has finished sliding.
    fn settle(&mut self) {
        for _ in 0..FRAME_CAP {
            let router = self.columns_router();
            if !router.navigating() && !router.returning() {
                return;
            }
            self.frame(Vec::new());
        }
        panic!("columns' nav never landed");
    }

    /// One frame with Escape pressed and released.
    fn press_escape(&mut self) {
        let key = |pressed| egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        self.frame(vec![key(true), key(false)]);
    }

    /// Columns' selected column's router.
    fn columns_router(&mut self) -> &mut NavStack<Route> {
        let app_ctx = self.notedeck.app_context();
        self.chrome
            .get_columns_app()
            .expect("columns in the roster")
            .columns_mut(app_ctx.i18n, app_ctx.accounts)
            .get_selected_router()
    }

    /// Whether Columns' selected column is still on its pushed route, with no
    /// back started.
    fn columns_still_deep(&mut self) -> bool {
        let router = self.columns_router();
        router.routes().len() > 1 && !router.returning()
    }
}

/// With another app in front, Escape is that app's: Columns, opened but
/// behind it, leaves its route alone.
#[tokio::test]
async fn a_backgrounded_columns_leaves_escape_alone() {
    let mut fx = Fixture::new();
    let headway = fx.chrome.headway_slot().expect("headway in the roster");
    fx.chrome.set_active(headway as i32);
    fx.frame(Vec::new());
    assert!(
        fx.chrome.is_opened(0),
        "columns stays opened behind headway"
    );

    fx.press_escape();

    assert!(
        fx.columns_still_deep(),
        "a columns nobody is looking at went back on Escape"
    );
}

/// The other half: in front, Columns still takes Escape to go back.
#[tokio::test]
async fn columns_in_front_goes_back_on_escape() {
    let mut fx = Fixture::new();
    assert!(fx.columns_still_deep(), "columns starts one route deep");

    fx.press_escape();

    assert!(
        !fx.columns_still_deep(),
        "columns in front went back on Escape"
    );
}
