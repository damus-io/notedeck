//! The chrome's settle-nav mode through real chrome frames, under a debug
//! build: an app that pushes and backs out of a route while drawing one
//! widget id on every route trips egui's "changed layer_id" assert when the
//! slide is drawn, and lands both without it when nav settles.

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use notedeck::{App, AppContext, AppResponse, Notedeck};
use notedeck_chrome::testing::ChromeDriver;
use notedeck_chrome::{Chrome, NotedeckApp};

/// What the test asks the app to do on its next render, and what it saw.
#[derive(Default)]
struct Script {
    /// Push a route on the next render.
    push: bool,
    /// Go back on the next render.
    back: bool,
    /// The routes handed to the app's `cleanup_nav`, by their number.
    cleaned: Vec<u32>,
}

/// An app whose every route draws the same `TextEdit`, by one fixed id, and
/// that pushes route `1` or goes back when its [`Script`] says to.
struct NavApp {
    script: Rc<RefCell<Script>>,
    text: String,
}

impl App for NavApp {
    fn render(&mut self, _ctx: &mut AppContext<'_>, _ui: &mut egui::Ui) -> AppResponse {
        AppResponse::none()
    }

    fn render_nav(
        &mut self,
        ctx: &mut AppContext<'_>,
        ui: &mut egui::Ui,
        _token: &Rc<dyn Any>,
    ) -> AppResponse {
        ui.add(egui::TextEdit::singleline(&mut self.text).id(egui::Id::unique("settle_nav_field")));
        let mut script = self.script.borrow_mut();
        if std::mem::take(&mut script.push) {
            ctx.navigator.push_route(1u32);
        }
        if std::mem::take(&mut script.back) {
            ctx.navigator.back();
        }
        AppResponse::none()
    }

    fn cleanup_nav(&mut self, _ctx: &mut AppContext<'_>, token: &Rc<dyn Any>) {
        if let Some(route) = token.downcast_ref::<u32>() {
            self.script.borrow_mut().cleaned.push(*route);
        }
    }
}

/// A headless chrome running only a [`NavApp`], with nav transitions
/// animated or not. Keeps the notedeck's data directory alive beside it.
fn driver(animate: bool) -> (tempfile::TempDir, ChromeDriver, Rc<RefCell<Script>>) {
    let dir = tempfile::TempDir::new().expect("tmp dir");
    let args = ["notedeck-test", "--testrunner"].map(str::to_owned);
    let egui_ctx = egui::Context::default();
    let mut notedeck = Notedeck::init(&egui_ctx, dir.path(), &args);
    {
        let app_ctx = notedeck.app_context();
        app_ctx.settings.complete_welcome();
        app_ctx.settings.get_settings_mut().animate_nav_transitions = animate;
    }
    let script = Rc::new(RefCell::new(Script::default()));
    let app = NavApp {
        script: Rc::clone(&script),
        text: String::new(),
    };
    let chrome = Chrome::new_headless_with_apps(
        &mut notedeck,
        [NotedeckApp::Other("nav".to_owned(), Box::new(app))],
    );
    let driver = ChromeDriver::new(egui_ctx, notedeck, chrome);
    (dir, driver, script)
}

/// How deep the active app's history is.
fn depth(driver: &ChromeDriver) -> usize {
    driver.chrome.history().expect("an active app").len()
}

/// Set in the child [`a_drawn_push_trips_the_layer_assert`] runs itself in.
const DRAW_THE_PUSH: &str = "NOTEDECK_SETTLE_NAV_DRAW_THE_PUSH";

/// Without settle-nav, the frame after a push draws the root and the pushed
/// route on two layers, animations off or not, and egui asserts on the first
/// widget both draw. This is the frame the mode exists to skip.
///
/// Runs the frames in a child process of this test binary: a second panic,
/// in a `Ui`'s destructor while the first unwinds, aborts the process, so
/// `#[should_panic]` can't catch it.
#[cfg(debug_assertions)]
#[test]
fn a_drawn_push_trips_the_layer_assert() {
    if std::env::var_os(DRAW_THE_PUSH).is_some() {
        let (_dir, mut driver, script) = driver(false);
        script.borrow_mut().push = true;
        driver.frame(Vec::new());
        driver.frame(Vec::new());
        return;
    }

    let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "a_drawn_push_trips_the_layer_assert",
            "--nocapture",
        ])
        .env(DRAW_THE_PUSH, "1")
        .env("RUST_BACKTRACE", "0")
        .output()
        .expect("run the test binary");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "the drawn push ran clean: {stderr}");
    assert!(
        stderr.contains("changed layer_id"),
        "the drawn push failed, but not on the layer assert: {stderr}"
    );
}

/// With settle-nav, a push and a back each land by the end of the frame that
/// asked for them, the back's route reaches `cleanup_nav` once, and no frame
/// draws a slide, with nav animations on or off.
#[test]
fn settled_push_and_back_land_without_a_slide() {
    for animate in [false, true] {
        let (_dir, driver, script) = driver(animate);
        let mut driver = driver.settle_nav();

        script.borrow_mut().push = true;
        driver.frame(Vec::new());
        assert_eq!(depth(&driver), 2, "the push landed (animate {animate})");
        driver.frame(Vec::new());

        script.borrow_mut().back = true;
        driver.frame(Vec::new());
        assert_eq!(depth(&driver), 1, "the back landed (animate {animate})");
        driver.frame(Vec::new());

        assert_eq!(script.borrow().cleaned, [1], "animate {animate}");
    }
}

/// With settle-nav, the chrome's own back (Alt-←), flagged before the app's
/// pass rather than after it, lands before that pass draws.
#[test]
fn settled_chrome_back_lands_before_the_draw() {
    let (_dir, driver, script) = driver(false);
    let mut driver = driver.settle_nav();

    script.borrow_mut().push = true;
    driver.frame(Vec::new());
    assert_eq!(depth(&driver), 2, "the push landed");

    driver.press(egui::Modifiers::ALT, egui::Key::ArrowLeft, None);
    assert_eq!(depth(&driver), 1, "Alt-← landed");
    assert_eq!(script.borrow().cleaned, [1]);
}
