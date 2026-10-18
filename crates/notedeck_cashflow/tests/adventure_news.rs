//! The Adventure crossing into the rest of the app: level-up and
//! achievement toasts over any view, checked against the vault's seen
//! note so the same news never toasts twice, the first opening's summary
//! of what the history already unlocked, and the triage inbox's floating
//! XP.
//!
//! Every test runs under a plain `cargo test`, over the made-up journals
//! in `common`.

mod common;

use std::time::Duration;

use cashflow::adventure::seen::Seen;
use cashflow::kinds;
use common::{
    CashflowTestState, DESKTOP, JournalFixture, harness, loaded_harness, step_until,
    synthetic_years_app, triage_app, wait_vault, watch_vault,
};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use notedeck_cashflow::View;
use notedeck_cashflow::news::Announced;

/// A window tall enough for every group of the triage journal.
const TALL: egui::Vec2 = egui::Vec2::new(1200.0, 1100.0);

/// The bodies of the toasts waiting, the one showing first.
fn toasts(harness: &Harness<'static, CashflowTestState>) -> Vec<String> {
    let news = harness.state().app.news();
    news.toasts().queued().map(|t| t.body.clone()).collect()
}

/// What the app last took the seen note to say, once it says something.
fn seen(harness: &Harness<'static, CashflowTestState>) -> Seen {
    match harness.state().app.news().announced() {
        Announced::Seen(seen) => seen.clone(),
        other => panic!("nothing announced yet: {other:?}"),
    }
}

/// Open the Adventure on the years journal with no seen note, as the
/// first opening ever, and return the harness on it.
fn first_opening() -> Harness<'static, CashflowTestState> {
    let mut app = synthetic_years_app();
    app.set_announced(Announced::Never);
    let mut harness = harness(app, DESKTOP);
    harness.state_mut().app.set_view(View::Adventure);
    harness.run();
    harness
}

/// With no seen note, nothing toasts and nothing folds outside the
/// Adventure; its first opening sums the history up in one toast and
/// writes the note.
#[test]
fn the_first_opening_sums_up_the_history() {
    let mut app = synthetic_years_app();
    app.set_announced(Announced::Never);
    let mut harness = harness(app, DESKTOP);
    assert!(toasts(&harness).is_empty(), "the dashboard isn't a game");
    assert_eq!(harness.state().app.news().announced(), &Announced::Never);

    let sub = watch_vault(harness.state_mut());
    harness.state_mut().app.set_view(View::Adventure);
    harness.run();

    let bodies = toasts(&harness);
    assert_eq!(bodies.len(), 1, "one summary, not a toast each: {bodies:?}");
    assert!(
        bodies[0].starts_with("You unlocked ") && bodies[0].ends_with(" from your history!"),
        "{bodies:?}"
    );
    harness.get_by_label("ACHIEVEMENTS UNLOCKED");
    let note = seen(&harness);
    assert!(!note.achievements.is_empty() && !note.levels.is_empty());

    let written = wait_vault(harness.state_mut(), sub, 1, kinds::ADVENTURE);
    assert_eq!(written.len(), 1);
    assert_eq!(written[0].d, "adv:seen");
}

/// Once the seen note holds what the fold stands at, nothing toasts, in
/// the Adventure or out of it: the note round-tripped, the news is old.
#[test]
fn no_toast_once_the_note_has_it() {
    let note = seen(&first_opening());

    let mut app = synthetic_years_app();
    app.set_announced(Announced::Seen(note.clone()));
    let mut harness = harness(app, DESKTOP);
    harness.state_mut().app.set_view(View::Adventure);
    harness.run();
    assert!(toasts(&harness).is_empty(), "{:?}", toasts(&harness));
    assert_eq!(
        harness.state().app.news().announced(),
        &Announced::Seen(note)
    );
}

/// A level and an achievement the note lacks toast over any view, here
/// the Overview, and the note takes them so they never toast again.
#[test]
fn news_toasts_over_any_view() {
    let full = seen(&first_opening());
    let mut behind = full.clone();
    let earning = behind
        .levels
        .iter_mut()
        .find(|l| l.key == "earning")
        .expect("an Earning level");
    earning.level -= 1;
    let level = earning.level + 1;
    let gone = behind.achievements.pop().expect("an achievement");
    let name = cashflow::adventure::achievements::get(&gone).unwrap().name;

    let mut app = synthetic_years_app();
    app.set_announced(Announced::Seen(behind));
    let harness = harness(app, DESKTOP);
    assert_eq!(harness.state().app.view(), View::Overview);
    assert_eq!(
        toasts(&harness),
        [format!("Earning is now level {level}"), name.to_owned()]
    );
    harness.get_by_label("LEVEL UP");
    assert_eq!(seen(&harness), full);
}

/// Through a real vault: the first opening writes the seen note, and an
/// app that starts over the same vault (another desk, once the note has
/// synced) reads it and toasts nothing.
#[test]
fn the_seen_note_follows_the_account() {
    let fixture = JournalFixture::synthetic();
    let mut harness = loaded_harness(fixture.app(), DESKTOP, false);
    step_until(&mut harness, "the vault is read", |s| {
        s.app.news().announced() != &Announced::Unknown
    });
    assert_eq!(harness.state().app.news().announced(), &Announced::Never);

    let sub = watch_vault(harness.state_mut());
    harness.state_mut().app.set_view(View::Adventure);
    harness.step();
    harness.step();
    wait_vault(harness.state_mut(), sub, 1, kinds::ADVENTURE);
    let note = seen(&harness);

    harness.state_mut().app = fixture.app();
    harness.state_mut().app.set_view(View::Adventure);
    step_until(&mut harness, "the books load and the note is read", |s| {
        s.app.books().is_some() && matches!(s.app.news().announced(), Announced::Seen(_))
    });
    harness.step();
    assert!(toasts(&harness).is_empty(), "{:?}", toasts(&harness));
    assert_eq!(seen(&harness), note);
}

/// Pick a new account for the last (smallest) group in the inbox:
/// "Categorize all…" on the three coffees.
fn categorize_the_coffees(harness: &mut Harness<'static, CashflowTestState>) {
    harness.get_by_label("Inbox").click();
    harness.run();
    harness
        .get_all_by_label("Categorize all…")
        .last()
        .unwrap()
        .click();
    harness.run();
    harness
        .get_by_role(Role::TextInput)
        .type_text("expenses:food:coffee");
    harness.run();
    harness
        .get_by_label("+ New account expenses:food:coffee")
        .click();
    harness.run();
}

/// Categorizing a group floats the Bookkeeping XP its rule earns, on the
/// fold's rates, and once the text has faded the inbox asks for no more
/// frames.
#[test]
fn categorizing_floats_the_xp_then_settles() {
    let mut harness = harness(triage_app(), TALL);
    categorize_the_coffees(&mut harness);

    let tuning = cashflow::adventure::XpTuning::default();
    let floating: Vec<&str> = harness.state().app.floating_xp().collect();
    assert_eq!(
        floating,
        [format!("+{} Bookkeeping XP", 3 * tuning.rule_xp)]
    );

    // Each step is 1/4 s: well past the text's life.
    harness.run_steps(8);
    assert_eq!(harness.state().app.floating_xp().count(), 0);
    let delay = harness.output().viewport_output[&egui::ViewportId::ROOT].repaint_delay;
    assert!(
        delay > Duration::from_secs(1),
        "still repainting: {delay:?}"
    );
}

/// With `adventure.feedback_in_dashboard` off, the inbox floats nothing.
#[test]
fn no_floating_xp_with_feedback_off() {
    let mut app = triage_app();
    app.set_adventure_feedback(false);
    let mut harness = harness(app, TALL);
    categorize_the_coffees(&mut harness);
    assert_eq!(harness.state().app.floating_xp().count(), 0);
}
