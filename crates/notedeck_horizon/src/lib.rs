//! Horizon — a timeblocking nostr calendar app for Notedeck.
//!
//! Time blocks are modelled as NIP-52 calendar events stored in nostrdb —
//! time-based (kind `31923`) and date-based / all-day (kind `31922`) — and
//! rendered on a day/week timeline with a live "now" indicator.

use chrono::{DateTime, Datelike, Local, TimeZone, Timelike};
use nostrdb::{Ndb, Subscription, Transaction};
use notedeck::{AppContext, AppResponse};

use block::Block;

mod block;
mod timeline;

/// Which span of time the timeline shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Day,
    Week,
}

pub struct Horizon {
    view: View,
    /// The date the timeline is focused on.
    focus: DateTime<Local>,
    /// Time blocks materialized from NIP-52 calendar events.
    blocks: Vec<Block>,
    /// nostrdb subscription for live calendar-event updates.
    sub: Option<Subscription>,
}

impl Default for Horizon {
    fn default() -> Self {
        Self {
            view: View::Day,
            focus: Local::now(),
            blocks: Vec::new(),
            sub: None,
        }
    }
}

impl notedeck::App for Horizon {
    fn update(&mut self, ctx: &mut AppContext<'_>, _egui_ctx: &egui::Context) {
        // Subscribe once, then backfill the calendar events already in the db —
        // a subscription only reports notes indexed *after* it's created.
        if self.sub.is_none() {
            match ctx.ndb.subscribe(&block::calendar_filters()) {
                Ok(sub) => {
                    self.sub = Some(sub);
                    self.reload(ctx.ndb);
                }
                Err(err) => tracing::error!("horizon: failed to subscribe: {err}"),
            }
        }

        // Re-read whenever new calendar notes have been indexed.
        if let Some(sub) = self.sub
            && !ctx.ndb.poll_for_notes(sub, 256).is_empty()
        {
            self.reload(ctx.ndb);
        }
    }

    fn render(&mut self, _ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        self.show(ui);
        AppResponse::none()
    }
}

impl Horizon {
    /// Re-read all calendar events from nostrdb into [`Self::blocks`].
    fn reload(&mut self, ndb: &Ndb) {
        let txn = match Transaction::new(ndb) {
            Ok(txn) => txn,
            Err(err) => {
                tracing::error!("horizon: txn failed: {err}");
                return;
            }
        };

        let filters = block::calendar_filters();
        let results = ndb.query(&txn, &filters, 5000).unwrap_or_default();

        self.blocks = results
            .iter()
            .filter_map(|r| ndb.get_note_by_key(&txn, r.note_key).ok())
            .filter_map(|note| block::from_note(&note))
            .collect();
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        self.header(ui);
        ui.separator();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| match self.view {
                View::Day => timeline::day(ui, self.focus, &self.blocks),
                View::Week => timeline::week(ui, self.focus, &self.blocks),
            });
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Horizon");
            ui.separator();

            if ui.button("◀").on_hover_text("Previous").clicked() {
                self.shift(-1);
            }
            if ui.button("Today").clicked() {
                self.focus = Local::now();
            }
            if ui.button("▶").on_hover_text("Next").clicked() {
                self.shift(1);
            }

            let label = match self.view {
                View::Day => self.focus.format("%A, %B %-d, %Y").to_string(),
                View::Week => {
                    let start = start_of_week(self.focus);
                    let end = start + chrono::Duration::days(6);
                    format!("{} – {}", start.format("%b %-d"), end.format("%b %-d, %Y"))
                }
            };
            ui.label(label);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.selectable_value(&mut self.view, View::Week, "Week");
                ui.selectable_value(&mut self.view, View::Day, "Day");
            });
        });
    }

    /// Move the focused range forward/backward by one unit of the current view.
    fn shift(&mut self, units: i64) {
        let days = match self.view {
            View::Day => units,
            View::Week => units * 7,
        };
        self.focus += chrono::Duration::days(days);
    }
}

/// The local midnight that starts the (Monday-based) week containing `dt`.
pub(crate) fn start_of_week(dt: DateTime<Local>) -> DateTime<Local> {
    let back = dt.weekday().num_days_from_monday() as i64;
    let monday = dt.date_naive() - chrono::Duration::days(back);
    Local
        .from_local_datetime(&monday.and_hms_opt(0, 0, 0).unwrap())
        .single()
        .unwrap_or(dt)
}

/// Fraction (0.0..1.0) of the way through the day that `dt`'s local time is.
pub(crate) fn day_fraction(dt: DateTime<Local>) -> f32 {
    dt.num_seconds_from_midnight() as f32 / 86_400.0
}
