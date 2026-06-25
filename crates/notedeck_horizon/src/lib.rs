//! Horizon — a timeblocking nostr calendar app for Notedeck.
//!
//! The plan is to model time blocks as NIP-52 time-based calendar events
//! (kind `31923`) stored in nostrdb, and render them on a day/week timeline.
//! For now this is a scaffold: it draws an empty timeline grid with a "now"
//! indicator so the app shows up in the chrome and we have a surface to build
//! the block UI on top of.

use chrono::{DateTime, Datelike, Local, TimeZone, Timelike};
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
    /// Time blocks to render. Seeded with demo data until NIP-52 reads land.
    blocks: Vec<Block>,
}

impl Default for Horizon {
    fn default() -> Self {
        Self {
            view: View::Day,
            focus: Local::now(),
            blocks: block::demo(Local::now()),
        }
    }
}

impl notedeck::App for Horizon {
    fn update(&mut self, _ctx: &mut AppContext<'_>, _egui_ctx: &egui::Context) {
        // TODO: subscribe to NIP-52 calendar events (kinds 31922-31925) and
        // materialize them into blocks for the focused range.
    }

    fn render(&mut self, _ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        self.show(ui);
        AppResponse::none()
    }
}

impl Horizon {
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
