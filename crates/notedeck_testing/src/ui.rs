//! Generic UI interaction helpers for E2E tests.
//!
//! The text lookups here (`*_by_text*`, [`wait_for_text`]) differ from
//! kittest's `*_by_label` in three ways that each cost earlier cards a guess:
//!
//! - **They read both `label` and `value`.** A `Role::Label` keeps its text in
//!   `value`, which kittest's label filter knows, but a value-bearing node of
//!   another role (a text input, the note renderer's text) is invisible to it,
//!   and a hand-rolled `.accesskit_node().label()` is `None` on a Label.
//! - **They ignore Fluent's bidi isolates.** Fluent wraps every placeable in
//!   U+2068 … U+2069 (`"\u{2068}4\u{2069} turns"`), but only once the string's
//!   FTL entry is exported, so a needle that spells them in breaks before the
//!   export and one that leaves them out breaks after it. Both sides are
//!   compared with the isolates stripped.
//! - **A miss prints the tree.** The panic carries every node's role, label
//!   and value ([`tree_text`]), so a wrong needle shows what was on screen.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::kittest::{AccessKitNode, NodeT, Queryable};
use egui_kittest::{Harness, Node};

/// Waits until a labeled UI node appears on the given harness, printing the
/// accesskit tree if it never does.
pub fn wait_for_label<S>(harness: &mut Harness<'_, S>, label: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;

    loop {
        harness.step();
        if harness.query_all_by_label(label).next().is_some() {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for UI label {label:?}; on screen:\n{}",
            tree_text(harness)
        );

        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Clicks the enabled UI node matching the given label.
pub fn click_enabled_label<S>(harness: &Harness<'_, S>, label: &str) {
    let node = harness
        .query_all_by_label(label)
        .find(|node| !node.accesskit_node().is_disabled())
        .unwrap_or_else(|| panic!("no enabled UI node found for label {label:?}"));
    node.click_accesskit();
}

/// Whether `ch` is one of the Unicode bidi isolates (U+2066–U+2069) that
/// Fluent wraps placeables in.
fn is_isolate(ch: char) -> bool {
    matches!(ch, '\u{2066}'..='\u{2069}')
}

/// `text` without its bidi isolates.
pub fn strip_isolates(text: &str) -> String {
    text.chars().filter(|&ch| !is_isolate(ch)).collect()
}

/// Whether `shown` reads as exactly `needle`, ignoring bidi isolates on both.
pub fn text_eq(shown: &str, needle: &str) -> bool {
    shown
        .chars()
        .filter(|&ch| !is_isolate(ch))
        .eq(needle.chars().filter(|&ch| !is_isolate(ch)))
}

/// Whether `shown` contains `needle`, ignoring bidi isolates on both.
pub fn text_contains(shown: &str, needle: &str) -> bool {
    strip_isolates(shown).contains(&strip_isolates(needle))
}

/// Whether `node` is a text run: egui hangs one under every widget that
/// draws text, carrying its parent's text again, so lookups and the dump
/// skip it rather than find each text twice.
fn is_text_run(node: &AccessKitNode<'_>) -> bool {
    node.role() == Role::TextRun
}

/// Whether `node`'s label or value passes `matches`.
fn node_text_matches(node: &AccessKitNode<'_>, matches: impl Fn(&str) -> bool) -> bool {
    !is_text_run(node) && node.label().is_some_and(|label| matches(&label))
        || !is_text_run(node) && node.value().is_some_and(|value| matches(&value))
}

/// Every node under the harness root whose label or value passes `matches`,
/// leaving out a node that only labels another (the labelled widget matches
/// in its place), as kittest's label queries do.
fn query_all_matching<'t, S>(
    harness: &'t Harness<'_, S>,
    matches: impl Fn(&str) -> bool + 't,
) -> impl Iterator<Item = Node<'t>> + 't {
    let root = harness.root();
    let labels: HashSet<_> = root
        .children_recursive()
        .flat_map(|node| {
            node.accesskit_node()
                .labelled_by()
                .map(|label| label.id())
                .collect::<Vec<_>>()
        })
        .collect();
    root.children_recursive().filter(move |node| {
        let node = node.accesskit_node();
        !labels.contains(&node.id()) && node_text_matches(&node, &matches)
    })
}

/// Every node whose label or value reads exactly `text`, bidi isolates aside.
pub fn query_all_by_text<'t, S>(
    harness: &'t Harness<'_, S>,
    text: &'t str,
) -> impl Iterator<Item = Node<'t>> + 't {
    query_all_matching(harness, move |shown| text_eq(shown, text))
}

/// Every node whose label or value contains `text`, bidi isolates aside.
pub fn query_all_by_text_contains<'t, S>(
    harness: &'t Harness<'_, S>,
    text: &'t str,
) -> impl Iterator<Item = Node<'t>> + 't {
    query_all_matching(harness, move |shown| text_contains(shown, text))
}

/// The one node whose label or value reads exactly `text`. Panics with the
/// tree when there is none or more than one.
pub fn get_by_text<'t, S>(harness: &'t Harness<'_, S>, text: &'t str) -> Node<'t> {
    only_match(harness, query_all_by_text(harness, text), "reading", text)
}

/// The one node whose label or value contains `text`. Panics with the tree
/// when there is none or more than one.
pub fn get_by_text_contains<'t, S>(harness: &'t Harness<'_, S>, text: &'t str) -> Node<'t> {
    only_match(
        harness,
        query_all_by_text_contains(harness, text),
        "containing",
        text,
    )
}

/// The single node of `found`, or a panic naming `how` and `text` and
/// printing the tree.
fn only_match<'t, S>(
    harness: &Harness<'_, S>,
    mut found: impl Iterator<Item = Node<'t>>,
    how: &str,
    text: &str,
) -> Node<'t> {
    let Some(first) = found.next() else {
        panic!(
            "no UI node {how} {text:?}; on screen:\n{}",
            tree_text(harness)
        );
    };
    let more = found.count();
    assert!(
        more == 0,
        "{} UI nodes {how} {text:?}, expected one; on screen:\n{}",
        more + 1,
        tree_text(harness)
    );
    first
}

/// Steps the harness until a node whose label or value reads exactly `text`
/// appears, or panics with the tree after `timeout`.
pub fn wait_for_text<S>(harness: &mut Harness<'_, S>, text: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;

    loop {
        harness.step();
        if query_all_by_text(harness, text).next().is_some() {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for UI text {text:?}; on screen:\n{}",
            tree_text(harness)
        );

        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The harness's accesskit tree, one line per node that carries text: its
/// role, label and value, indented by depth. Nodes without text and text
/// runs are left out, so the dump is what a text lookup could have matched. Text is debug-quoted, which spells the invisible
/// isolates out as `\u{2068}`/`\u{2069}`.
pub fn tree_text<S>(harness: &Harness<'_, S>) -> String {
    let mut out = String::new();
    write_tree(&mut out, &harness.root(), 0);
    if out.is_empty() {
        out.push_str("  (no node carries a label or value)\n");
    }
    out
}

/// Appends `node`'s children, recursively, to `out` at `depth`.
fn write_tree(out: &mut String, node: &Node<'_>, depth: usize) {
    for child in node.children() {
        let ak = child.accesskit_node();
        if is_text_run(&ak) {
            continue;
        }
        let label = ak.label();
        let value = ak.value();
        if label.is_some() || value.is_some() {
            let _ = write!(out, "{:indent$}{:?}", "", ak.role(), indent = 2 + depth * 2);
            if let Some(label) = label {
                let _ = write!(out, " label={label:?}");
            }
            if let Some(value) = value {
                let _ = write!(out, " value={value:?}");
            }
            out.push('\n');
        }
        write_tree(out, &child, depth + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame with a plain label, a Fluent-style isolated count, and a text
    /// input whose text sits in its value under a different label.
    fn harness() -> Harness<'static> {
        let mut harness = Harness::new_ui(|ui| {
            // Fonts set during a pass load at the start of the next one, and
            // a bare harness has none, so the first pass only installs them.
            if ui.ctx().cumulative_pass_nr() == 0 {
                notedeck::fonts::setup_fonts(ui.ctx());
                return;
            }
            ui.label("Board");
            ui.label("\u{2068}4\u{2069} turns");
            let mut text = String::from("draft body");
            ui.add(egui::TextEdit::singleline(&mut text).hint_text("Comment"));
        });
        harness.run();
        harness
    }

    #[test]
    fn isolates_are_ignored_on_both_sides() {
        assert!(text_eq("\u{2068}4\u{2069} turns", "4 turns"));
        assert!(text_eq("4 turns", "\u{2068}4\u{2069} turns"));
        assert!(!text_eq("4 turns", "4 turn"));
        assert!(text_contains("worked \u{2068}12m\u{2069} over", "12m over"));
    }

    #[test]
    fn matches_a_label_with_or_without_its_isolates() {
        let harness = harness();
        get_by_text(&harness, "Board");
        get_by_text(&harness, "4 turns");
        get_by_text(&harness, "\u{2068}4\u{2069} turns");
        get_by_text_contains(&harness, "turns");
    }

    #[test]
    fn matches_a_value_off_a_non_label_role() {
        let harness = harness();
        assert!(
            harness.query_by_label("draft body").is_none(),
            "kittest's label lookup cannot see a text input's value"
        );
        let node = get_by_text(&harness, "draft body");
        assert_eq!(
            node.accesskit_node().role(),
            egui::accesskit::Role::TextInput
        );
    }

    #[test]
    fn the_tree_dump_shows_role_label_and_value() {
        let tree = tree_text(&harness());
        assert!(tree.contains("Label"), "{tree}");
        assert!(
            tree.contains(r#"value="\u{2068}4\u{2069} turns""#),
            "{tree}"
        );
        assert!(tree.contains(r#"value="draft body""#), "{tree}");
    }

    #[test]
    fn a_miss_prints_the_tree() {
        let harness = harness();
        let miss = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            get_by_text(&harness, "Boards");
        }))
        .expect_err("no node reads Boards");
        let msg = miss.downcast_ref::<String>().expect("formatted panic");
        assert!(msg.contains(r#"no UI node reading "Boards""#), "{msg}");
        assert!(msg.contains(r#"value="Board""#), "{msg}");
    }

    #[test]
    fn wait_for_text_returns_once_the_text_is_drawn() {
        let mut harness = harness();
        wait_for_text(&mut harness, "4 turns", Duration::from_secs(5));
    }
}
