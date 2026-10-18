//! Columns' keyboard input through the real chrome: it is read only while
//! Columns is on screen. Opened apps all get `update` every frame whether or
//! not they are in front, so input read there is stolen from whichever app
//! is. Escape is the one that shows: Columns consumes it to go back a route,
//! out from under the app the user is actually typing at.
//!
//! The chrome's app-switch slide is off, so a switch lands on the frame it's
//! made.

use nostrdb_net::FullKeypair;
use notedeck::{NavStack, Notedeck};
use notedeck_columns::Route;

use super::Chrome;
use crate::testing::ChromeDriver;

/// How many frames a slide gets to land: two seconds of them at 60 Hz.
const FRAME_CAP: usize = 120;

/// A chrome built the way the app builds one, with Columns opened and one
/// route deep, so it has somewhere to go back to.
struct Fixture {
    _dir: tempfile::TempDir,
    driver: ChromeDriver,
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
        let chrome = Chrome::new_headless(&args, &mut notedeck).expect("chrome");

        // The driver's two warm-up frames apply the fonts and let Columns
        // initialize its columns on its first update; Settings then gives it
        // a route to leave.
        let mut fixture = Self {
            _dir: dir,
            driver: ChromeDriver::new(egui_ctx, notedeck, chrome),
        };
        fixture.columns_router().route_to(Route::Settings);
        fixture.settle();
        fixture
    }

    /// Run frames until Columns' selected column has finished sliding.
    fn settle(&mut self) {
        for _ in 0..FRAME_CAP {
            let router = self.columns_router();
            if !router.navigating() && !router.returning() {
                return;
            }
            self.driver.frame(Vec::new());
        }
        panic!("columns' nav never landed");
    }

    /// Columns' selected column's router.
    fn columns_router(&mut self) -> &mut NavStack<Route> {
        let ChromeDriver {
            notedeck, chrome, ..
        } = &mut self.driver;
        let app_ctx = notedeck.app_context();
        chrome
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
    let headway = fx
        .driver
        .chrome
        .headway_slot()
        .expect("headway in the roster");
    fx.driver.chrome.set_active(headway as i32);
    fx.driver.frame(Vec::new());
    assert!(
        fx.driver.chrome.is_opened(0),
        "columns stays opened behind headway"
    );

    fx.driver.key(egui::Key::Escape);

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

    fx.driver.key(egui::Key::Escape);

    assert!(
        !fx.columns_still_deep(),
        "columns in front went back on Escape"
    );
}
