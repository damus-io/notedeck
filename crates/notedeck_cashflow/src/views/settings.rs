//! The Settings view: which layers load, the home currency, how the journal
//! stands, and how it syncs.
//!
//! - **Layers** are toggled here as on the Overview's chips; the switch
//!   follows the account through the vault (see [`crate::layers`]).
//! - **Home currency** is read-only for now: `cashflow settings set home`
//!   changes it.
//! - **Journal** mirrors `cashflow journal status`: segments and the last
//!   import per source, a warning for a source not imported in
//!   [`STALE_AFTER_DAYS`], what the vault lists that the journal lacks, what
//!   it retires, and what the last load couldn't read.
//! - **Sync** lists `config.toml`'s transports with how each last ran, and
//!   the Sync now button. A transport whose program isn't installed says so
//!   before anyone presses it.
//!
//! Every line built from the books is written into a [`Model`] when they
//! change, so a frame only draws.

use cashflow::Books;
use egui::RichText;
use ledgerdb::Date;

use super::ViewCx;
use crate::journal::JournalSource;
use crate::ui::journal_status;
use crate::ui::palette::Palette;

/// How many days since a source's last import before it is flagged stale:
/// a monthly statement, plus a few days' grace.
pub const STALE_AFTER_DAYS: u32 = 35;

/// Seconds in a day, to date a segment header's unix `imported` time.
const DAY_SECS: u64 = 86_400;

/// The Settings view's cached journal status.
#[derive(Default)]
pub(crate) struct SettingsState {
    /// What [`Self::model`] was computed from.
    key: Option<Key>,
    /// The lines drawn from the books.
    model: Model,
}

/// What the cached model depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Key {
    generation: u64,
    today: Date,
}

/// What the view draws from the books, written once per load.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Model {
    /// Every layer the journal has, with how many segments are in it.
    pub layers: Vec<LayerRow>,
    /// Every source the journal has, by name.
    pub sources: Vec<SourceRow>,
    /// "The last load read 3 segments: 1 damaged, 0 unreadable", or empty
    /// when it read all of them cleanly.
    pub problems: String,
}

/// One layer to switch.
#[derive(Debug, PartialEq, Eq)]
pub struct LayerRow {
    /// The layer, as segment headers name it.
    pub name: String,
    /// The toggle's label: "cln-chain · 4 segments".
    pub label: String,
}

/// One source the journal has segments from.
#[derive(Debug, PartialEq, Eq)]
pub struct SourceRow {
    /// The source, as segment headers name it.
    pub source: String,
    /// "4 segments · last import 2024-10-02".
    pub detail: String,
    /// "amex not imported since 2024-08-01", when it is stale.
    pub stale: Option<String>,
}

/// Work out what the view draws from `books`, as of `today`.
#[profiling::function]
pub fn model(books: &Books, today: Date) -> Model {
    let ledger = books.ledger();
    let segments = || ledger.segments().iter().chain(ledger.excluded());

    let layers = crate::layers::known(books)
        .into_iter()
        .map(|name| {
            let count = segments().filter(|s| s.header.layer == name).count();
            LayerRow {
                label: format!("{name} · {}", segment_count(count)),
                name,
            }
        })
        .collect();

    let mut names: Vec<&str> = segments().map(|s| s.header.source.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    let sources = names
        .into_iter()
        .map(|source| {
            let of_source = || segments().filter(|s| s.header.source == source);
            let imported = of_source().map(|s| s.header.imported).max().unwrap_or(0);
            let last = Date::from_days((imported / DAY_SECS).min(u32::MAX as u64) as u32);
            let stale = today.days().saturating_sub(last.days()) > STALE_AFTER_DAYS;
            SourceRow {
                source: source.to_owned(),
                detail: format!(
                    "{} · last import {last}",
                    segment_count(of_source().count())
                ),
                stale: stale.then(|| format!("{source} not imported since {last}")),
            }
        })
        .collect();

    let report = &books.report;
    let bad = report.damaged.len() + report.unreadable.len() + report.unmergeable.len();
    let problems = if bad == 0 {
        String::new()
    } else {
        format!(
            "The last load read {}: {} damaged, {} unreadable, {} that wouldn't merge.",
            segment_count(report.loaded.len() + report.unreadable.len()),
            report.damaged.len(),
            report.unreadable.len(),
            report.unmergeable.len()
        )
    };
    Model {
        layers,
        sources,
        problems,
    }
}

/// `1 segment` or `n segments`.
fn segment_count(n: usize) -> String {
    if n == 1 {
        "1 segment".to_owned()
    } else {
        format!("{n} segments")
    }
}

/// Draw the Settings view.
#[profiling::function]
pub(crate) fn ui(cx: &mut ViewCx<'_>, ui: &mut egui::Ui) {
    let palette = cx.palette;
    if let Some(ledger) = cx.ledger {
        let key = Key {
            generation: ledger.generation,
            today: cx.today,
        };
        let state = &mut cx.states.settings;
        if state.key != Some(key) {
            state.key = Some(key);
            state.model = model(ledger.books, cx.today);
        }
    }
    let model = &cx.states.settings.model;

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.label(RichText::new("Settings").heading().color(palette.text));

        section(ui, palette, "Layers");
        muted(
            ui,
            palette,
            "Which journal layers the books load. Switches follow your account to every machine.",
        );
        if model.layers.is_empty() {
            muted(ui, palette, "No layers until the journal has segments.");
        }
        for row in &model.layers {
            let mut on = cx.layers.is_on(&row.name);
            let text = RichText::new(row.label.as_str()).color(palette.text);
            if ui.checkbox(&mut on, text).changed() {
                cx.layers.toggle(&row.name);
            }
        }

        section(ui, palette, "Home currency");
        let home = cx
            .journal
            .as_deref()
            .map_or(cashflow::settings::DEFAULT_HOME_COMMODITY, JournalSource::home);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(home).strong().color(palette.text));
            muted(
                ui,
                palette,
                "Every amount is converted into it. `cashflow settings set home <symbol>` changes it.",
            );
        });

        section(ui, palette, "Journal");
        if !cx.summary.is_empty() {
            ui.label(RichText::new(cx.summary).color(palette.text_muted));
        }
        ui.add_space(4.0);
        for row in &model.sources {
            source_row(ui, palette, row);
        }
        if let Some(journal) = cx.journal.as_deref()
            && !journal.retired_here().is_empty()
        {
            ui.label(RichText::new(journal.retired_here()).color(palette.text_muted));
        }
        if !model.problems.is_empty() {
            ui.label(RichText::new(model.problems.as_str()).color(palette.warning));
        }

        section(ui, palette, "Sync");
        match cx.journal.as_deref_mut() {
            Some(journal) => sync_ui(ui, palette, journal),
            None => muted(
                ui,
                palette,
                "This app was handed its books: there is no journal to sync.",
            ),
        }
    });
}

/// One source: its name, its segments and last import, and the stale
/// warning when it has one.
fn source_row(ui: &mut egui::Ui, palette: &Palette, row: &SourceRow) {
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(row.source.as_str())
                .strong()
                .color(palette.text),
        );
        ui.label(RichText::new(row.detail.as_str()).color(palette.text_muted));
        if let Some(stale) = &row.stale {
            ui.label(RichText::new(stale.as_str()).color(palette.warning));
        }
    });
}

/// The configured transports and how each last ran, then what the vault
/// lists that the journal lacks, the Sync now button and the last sync.
fn sync_ui(ui: &mut egui::Ui, palette: &Palette, journal: &mut JournalSource) {
    match journal.transports() {
        Err(err) => {
            ui.label(RichText::new(err).color(palette.warning));
        }
        Ok(transports) => {
            for status in transports {
                let info = &status.info;
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(info.name.as_str())
                            .strong()
                            .color(palette.text),
                    );
                    ui.label(RichText::new(info.kind.as_str()).color(palette.text_muted));
                    ui.label(
                        RichText::new(info.remote.as_str())
                            .monospace()
                            .color(palette.text),
                    );
                });
                if let Some(err) = &info.unavailable {
                    ui.label(RichText::new(err.as_str()).color(palette.warning));
                }
                let Some(run) = &status.last else {
                    continue;
                };
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(run.at.as_str()).color(palette.text_muted));
                    ui.label(RichText::new(run.counts.as_str()).color(palette.text));
                    if let Some(err) = &run.error {
                        ui.label(RichText::new(err.as_str()).color(palette.warning));
                    }
                });
            }
        }
    }
    ui.add_space(8.0);
    journal_status::sync_row(ui, palette, journal);
}

/// A section's heading, with room above it.
fn section(ui: &mut egui::Ui, palette: &Palette, title: &'static str) {
    ui.add_space(20.0);
    ui.label(RichText::new(title).size(18.0).strong().color(palette.text));
    ui.add_space(4.0);
}

/// A line of muted text.
fn muted(ui: &mut egui::Ui, palette: &Palette, text: &'static str) {
    ui.label(RichText::new(text).color(palette.text_muted));
}
