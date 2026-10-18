//! What the shell draws about the journal itself: the panes that stand in
//! for every view until there are books (loading, empty, error), the banner
//! above a view when the journal is missing segments, and the one-line
//! summary of what is loaded.
//!
//! Every line that depends on data is written by [`crate::journal`] or
//! [`summary`] when the data changes, so drawing one allocates nothing.

use std::fmt::Write;
use std::time::Duration;

use cashflow::Books;
use egui::RichText;

use super::palette::Palette;
use crate::journal::JournalSource;

/// The commands that put something in an empty journal, from
/// `cashflow import --help`.
const IMPORT_COMMANDS: &[&str] = &[
    "cashflow import amex ~/Downloads/activity.csv",
    "cashflow import rbc ~/Downloads/csv*.csv",
    "cashflow import cln",
    "cashflow import btc",
    "cashflow import ledger books.ledger",
];

/// "1,234 transactions · 2019-01-02 to 2025-10-16 · loaded in 1.2 s": what
/// the books hold and, when known, how long they took. Written once per
/// load.
pub fn summary(books: &Books, load_time: Option<Duration>) -> String {
    let txns = books.ledger().txns();
    let mut text = String::new();
    let _ = write!(text, "{} transactions", thousands(txns.len()));
    if let (Some(first), Some(last)) = (txns.first(), txns.last()) {
        let _ = write!(text, " · {} to {}", first.date, last.date);
    }
    if let Some(took) = load_time {
        let _ = write!(text, " · loaded in {}", duration(took));
    }
    text
}

/// `n` with a comma every three digits.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut text = String::with_capacity(digits.len() + digits.len() / 3);
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    text
}

/// `850 ms` under a second, else `1.2 s`.
fn duration(took: Duration) -> String {
    if took < Duration::from_secs(1) {
        format!("{} ms", took.as_millis())
    } else {
        format!("{:.1} s", took.as_secs_f64())
    }
}

/// The pane shown before the first load finishes.
pub fn loading_ui(ui: &mut egui::Ui, palette: &Palette) {
    ui.horizontal(|ui| {
        ui.add(egui::Spinner::new().color(palette.accent));
        ui.label(RichText::new("Loading the journal…").color(palette.text_muted));
    });
}

/// The pane shown when the journal can't be listed.
pub fn error_ui(ui: &mut egui::Ui, palette: &Palette, source: &JournalSource) {
    ui.label(
        RichText::new("The journal couldn't be read")
            .heading()
            .color(palette.text),
    );
    ui.add_space(8.0);
    if let Some(err) = source.error() {
        ui.label(RichText::new(err).color(palette.warning));
    }
}

/// The pane shown when the journal has no segments: where it looks, how to
/// import into it, and a sync when one is configured.
pub fn empty_ui(ui: &mut egui::Ui, palette: &Palette, source: &mut JournalSource) {
    ui.label(
        RichText::new("No transactions yet")
            .heading()
            .color(palette.text),
    );
    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("The journal at").color(palette.text_muted));
        ui.label(
            RichText::new(source.journal_path())
                .monospace()
                .color(palette.text),
        );
        ui.label(RichText::new("is empty. Import a statement with:").color(palette.text_muted));
    });
    ui.add_space(8.0);
    for command in IMPORT_COMMANDS {
        ui.label(RichText::new(*command).monospace().color(palette.text));
    }
    if source.can_sync() || !source.missing().hashes.is_empty() {
        ui.add_space(16.0);
        sync_row(ui, palette, source);
    }
}

/// The banner above a view while the journal lacks segments the vault
/// lists, or after a sync, saying how it went. Draws nothing otherwise.
pub fn banner_ui(ui: &mut egui::Ui, palette: &Palette, source: &mut JournalSource) {
    let missing = !source.missing().hashes.is_empty();
    if !missing && source.last_sync().is_none() && !source.is_syncing() {
        return;
    }
    egui::Frame::new()
        .fill(palette.surface)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            sync_row(ui, palette, source);
        });
    ui.add_space(12.0);
}

/// What is missing, the Sync now button (or why there is none), and how the
/// running or last sync went.
pub(crate) fn sync_row(ui: &mut egui::Ui, palette: &Palette, source: &mut JournalSource) {
    let missing = &source.missing().summary;
    if !missing.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new("Missing segments:")
                    .strong()
                    .color(palette.warning),
            );
            ui.label(RichText::new(missing.as_str()).color(palette.text));
            ui.label(
                RichText::new("Totals leave them out until they arrive.").color(palette.text_muted),
            );
        });
    }
    ui.horizontal_wrapped(|ui| {
        if source.is_syncing() {
            ui.add(egui::Spinner::new().color(palette.accent));
            ui.label(RichText::new("Syncing…").color(palette.text_muted));
        } else if source.can_sync() {
            if ui.button("Sync now").clicked() {
                source.start_sync();
            }
        } else {
            ui.label(
                RichText::new("Add a [[sync.transport]] to config.toml to fetch them.")
                    .color(palette.text_muted),
            );
        }
        if let Some(outcome) = source.last_sync() {
            let color = if outcome.failed {
                palette.warning
            } else {
                palette.text_muted
            };
            ui.label(RichText::new(outcome.summary.as_str()).color(color));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands_groups_by_three() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(333_012), "333,012");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn duration_reads_in_ms_then_seconds() {
        assert_eq!(duration(Duration::from_millis(850)), "850 ms");
        assert_eq!(duration(Duration::from_millis(1_240)), "1.2 s");
    }
}
