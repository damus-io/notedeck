//! Horizon — a timeblocking nostr calendar app for Notedeck.
//!
//! Time blocks are modelled as NIP-52 calendar events stored in nostrdb —
//! time-based (kind `31923`) and date-based / all-day (kind `31922`) — and
//! rendered in a three-pane calendar: a mini-month + agenda sidebar, a styled
//! day/week timeline, and an inspector for the selected event.

use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone, Timelike};
use egui::{RichText, Stroke};
use nostrdb::{Ndb, Subscription, Transaction};
use notedeck::{AppContext, AppResponse};

use block::Block;

mod block;
mod inspector;
mod sidebar;
mod theme;
mod timeline;

/// Which span of time the timeline shows. Only [`View::Day`] and [`View::Week`]
/// have full timelines today; the wider spans are placeholders for now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

impl View {
    const ALL: [View; 5] = [
        View::Day,
        View::Week,
        View::Month,
        View::Quarter,
        View::Year,
    ];

    fn label(self) -> &'static str {
        match self {
            View::Day => "Day",
            View::Week => "Week",
            View::Month => "Month",
            View::Quarter => "Quarter",
            View::Year => "Year",
        }
    }
}

pub struct Horizon {
    view: View,
    /// The date the timeline is focused on.
    focus: DateTime<Local>,
    /// First-of-month date shown by the sidebar's mini calendar.
    cal_month: NaiveDate,
    /// Index into [`Self::blocks`] of the inspected event, if any.
    selected: Option<usize>,
    /// Current search query (not yet wired to filtering).
    search: String,
    /// Time blocks materialized from NIP-52 calendar events.
    blocks: Vec<Block>,
    /// nostrdb subscription for live calendar-event updates.
    sub: Option<Subscription>,
}

impl Default for Horizon {
    fn default() -> Self {
        let now = Local::now();
        Self {
            view: View::Day,
            focus: now,
            cal_month: sidebar::month_of(now),
            selected: None,
            search: String::new(),
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
        self.blocks.sort_by_key(|b| b.start);
        // A reload can invalidate the old index, so clear the selection.
        self.selected = None;
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        apply_theme(ui);

        egui::TopBottomPanel::top("horizon_toolbar")
            .frame(panel_frame().inner_margin(egui::Margin::symmetric(12, 8)))
            .show_inside(ui, |ui| self.toolbar(ui));

        egui::SidePanel::left("horizon_sidebar")
            .resizable(true)
            .default_width(300.0)
            .frame(panel_frame().inner_margin(egui::Margin::symmetric(12, 4)))
            .show_inside(ui, |ui| {
                let action =
                    sidebar::show(ui, self.focus, self.cal_month, &self.blocks, self.selected);
                self.apply_sidebar(action);
            });

        egui::SidePanel::right("horizon_inspector")
            .resizable(true)
            .default_width(320.0)
            .frame(panel_frame().inner_margin(egui::Margin::symmetric(16, 4)))
            .show_inside(ui, |ui| {
                inspector::show(ui, &self.blocks, self.selected);
            });

        egui::CentralPanel::default()
            .frame(panel_frame().inner_margin(egui::Margin::symmetric(8, 4)))
            .show_inside(ui, |ui| self.center(ui));
    }

    fn center(&mut self, ui: &mut egui::Ui) {
        match self.view {
            View::Day => {
                if let Some(i) = timeline::center_day(ui, self.focus, &self.blocks, self.selected) {
                    self.selected = Some(i);
                }
            }
            View::Week => {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| timeline::week(ui, self.focus, &self.blocks));
            }
            other => {
                ui.vertical_centered(|ui| {
                    ui.add_space(60.0);
                    ui.label(
                        RichText::new(format!("{} view coming soon", other.label()))
                            .size(16.0)
                            .color(theme::TEXT_WEAK),
                    );
                });
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if nav_button(ui, "‹") {
                self.shift(-1);
            }
            if ui
                .add(egui::Button::new(RichText::new("Today").color(theme::TEXT)))
                .clicked()
            {
                self.go_today();
            }
            if nav_button(ui, "›") {
                self.shift(1);
            }

            // Red "today" day-of-month badge.
            let today = Local::now().day();
            let (badge, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
            ui.painter().circle_filled(badge.center(), 11.0, theme::NOW);
            ui.painter().text(
                badge.center(),
                egui::Align2::CENTER_CENTER,
                today.to_string(),
                egui::FontId::proportional(11.0),
                theme::TEXT,
            );

            ui.add_space(16.0);

            // View segmented control.
            for v in View::ALL {
                if tab(ui, v.label(), self.view == v) {
                    self.view = v;
                }
            }

            // Search at the far right.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("Search")
                        .desired_width(200.0),
                );
            });
        });
    }

    fn apply_sidebar(&mut self, action: sidebar::SidebarAction) {
        if action.month_step != 0 {
            self.cal_month = sidebar::step_month(self.cal_month, action.month_step);
        }
        if let Some(date) = action.focus {
            self.focus = at_local_midnight(date).unwrap_or(self.focus);
            self.cal_month = date.with_day(1).unwrap_or(self.cal_month);
        }
        if let Some(i) = action.selected {
            self.selected = Some(i);
        }
    }

    fn go_today(&mut self) {
        self.focus = Local::now();
        self.cal_month = sidebar::month_of(self.focus);
    }

    /// Move the focused range forward/backward by one unit of the current view.
    fn shift(&mut self, units: i64) {
        let days = match self.view {
            View::Week => units * 7,
            _ => units,
        };
        self.focus += chrono::Duration::days(days);
        self.cal_month = sidebar::month_of(self.focus);
    }
}

/// Push Horizon's dark palette into the ambient egui style for this frame.
fn apply_theme(ui: &mut egui::Ui) {
    let v = &mut ui.style_mut().visuals;
    v.panel_fill = theme::BG;
    v.window_fill = theme::BG;
    v.extreme_bg_color = theme::SURFACE;
    v.override_text_color = Some(theme::TEXT);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, theme::GRID);
    v.widgets.inactive.bg_fill = theme::SURFACE;
    v.widgets.hovered.bg_fill = theme::GRID;
}

fn panel_frame() -> egui::Frame {
    egui::Frame::new().fill(theme::BG)
}

fn nav_button(ui: &mut egui::Ui, glyph: &str) -> bool {
    ui.add(egui::Button::new(
        RichText::new(glyph).size(18.0).color(theme::TEXT_WEAK),
    ))
    .clicked()
}

/// A segmented-control tab; highlighted when `active`.
fn tab(ui: &mut egui::Ui, label: &str, active: bool) -> bool {
    let color = if active {
        theme::TEXT
    } else {
        theme::TEXT_WEAK
    };
    let mut btn = egui::Button::new(RichText::new(label).color(color)).frame(false);
    if active {
        btn = btn.fill(theme::SURFACE);
    }
    ui.add(btn).clicked()
}

/// The local midnight that starts the (Monday-based) week containing `dt`.
pub(crate) fn start_of_week(dt: DateTime<Local>) -> DateTime<Local> {
    let back = dt.weekday().num_days_from_monday() as i64;
    let monday = dt.date_naive() - chrono::Duration::days(back);
    at_local_midnight(monday).unwrap_or(dt)
}

/// Resolve a `NaiveDate` to local midnight, handling DST gaps gracefully.
fn at_local_midnight(date: NaiveDate) -> Option<DateTime<Local>> {
    Local
        .from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
        .single()
}

/// Fraction (0.0..1.0) of the way through the day that `dt`'s local time is.
pub(crate) fn day_fraction(dt: DateTime<Local>) -> f32 {
    dt.num_seconds_from_midnight() as f32 / 86_400.0
}
