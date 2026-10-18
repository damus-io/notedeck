//! The Goals view: the financial goals on Headway cards, measured against
//! the books.
//!
//! - **The list**: a tile per goal with its card's title (a link that opens
//!   the card in notedeck), a status pill, a progress bar of the value so
//!   far against the target, the pace the rest of the window needs beside
//!   the pace it is on, and when it gets there. A goal whose block doesn't
//!   read, or can't be measured against these books, shows why in its tile.
//! - **A goal's detail** ("Chart"): its history as a line, the projection
//!   carrying on from it dashed, the target as a level and the deadline
//!   marked.
//!
//! The goals come from [`crate::goal_source::GoalSource`], or
//! [`crate::CashflowApp::set_goals`]. They are measured by [`model::report`]
//! only when the books, the goals or today change; the Overview's strip
//! reads the same [`GoalsState`].

pub mod model;

use cashflow::goals::{GOAL_LABEL, Status};
use egui::{Color32, RichText, Sense};
use ledgerdb::Date;
use notedeck_ui::charts::{
    HoverPoint, Level, Marker, Marks, Series, TimeSeries, TimeSeriesStyle, Trail, line_chart_marked,
};

use super::{GoalsCx, LedgerCx, ViewCx};
use crate::{View, ui::palette::Palette};
use model::{Chart, GoalRow, Inputs, Measured, Report};

/// Height of the detail's chart in the wide layout.
const CHART_HEIGHT: f32 = 240.0;

/// Height of the detail's chart on a phone.
const NARROW_CHART_HEIGHT: f32 = 170.0;

/// Height of a tile's progress bar.
const BAR_HEIGHT: f32 = 8.0;

/// Gap between tiles.
const GAP: f32 = 12.0;

/// The Goals view's state: the goals measured, cached, and the one open.
#[derive(Default)]
pub(crate) struct GoalsState {
    /// What [`Self::report`] was computed from; a frame with a different
    /// key recomputes it.
    key: Option<Key>,
    /// The goals measured, once there are books.
    report: Option<Report>,
    /// The card ref of the goal whose detail is showing, if one is.
    open: Option<String>,
}

/// What the cached figures depend on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Key {
    books: u64,
    goals: u64,
    today: Date,
}

impl GoalsState {
    /// Measure the goals again if the books, the goals or today moved.
    #[profiling::function]
    pub(crate) fn refresh(
        &mut self,
        ledger: LedgerCx<'_>,
        goals: GoalsCx<'_>,
        projects: &cashflow::projects::Projects,
        today: Date,
    ) {
        let key = Key {
            books: ledger.generation,
            goals: goals.generation,
            today,
        };
        if self.key == Some(key) {
            return;
        }
        self.key = Some(key);
        self.report = Some(model::report(Inputs {
            books: ledger.books,
            projects,
            today,
            goals: goals.goals,
        }));
    }

    /// The goals measured at the last refresh.
    pub(crate) fn report(&self) -> Option<&Report> {
        self.report.as_ref()
    }
}

/// A click this frame.
enum Action {
    /// Show the detail of the row at this index of [`Report::rows`].
    Open(usize),
    /// Back from a detail to the list.
    Back,
    /// Open the card of the row at this index in notedeck.
    Card(usize),
    /// Open the card of the goal whose detail is showing.
    OpenCard,
}

/// Draw the Goals view.
#[profiling::function]
pub(crate) fn ui(cx: &mut ViewCx<'_>, ui: &mut egui::Ui) {
    let palette = cx.palette;
    let narrow = notedeck::ui::is_narrow(ui.ctx());
    ui.label(
        RichText::new(View::Goals.label())
            .heading()
            .color(palette.text),
    );
    ui.add_space(12.0);

    let Some(ledger) = cx.ledger else {
        muted(ui, palette, "No journal loaded yet.");
        return;
    };
    let goals = cx.goals;
    let state = &mut cx.states.goals;
    state.refresh(ledger, goals, cx.projects, cx.today);
    let Some(report) = &state.report else {
        return;
    };
    if report.rows.is_empty() {
        empty_ui(ui, palette, goals);
        return;
    }

    let open = state.open.as_deref().and_then(|r| report.row(r));
    let mut action = None;
    egui::ScrollArea::vertical()
        .id_salt("cashflow_goals")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            action = match open {
                Some(row) => detail_ui(ui, palette, row, narrow),
                None => list_ui(ui, palette, report),
            };
        });

    match action {
        Some(Action::Open(at)) => state.open = report.rows.get(at).map(|r| r.card_ref.clone()),
        Some(Action::Back) => state.open = None,
        Some(Action::Card(at)) => cx.open_card = report.rows.get(at).map(|r| r.card_ref.clone()),
        Some(Action::OpenCard) => cx.open_card = open.map(|r| r.card_ref.clone()),
        None => {}
    }
}

/// No goals on the boards: how to make one.
fn empty_ui(ui: &mut egui::Ui, palette: &Palette, goals: GoalsCx<'_>) {
    muted(ui, palette, "No goals yet.");
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        muted(ui, palette, "Label a Headway card");
        code(ui, palette, GOAL_LABEL);
        muted(ui, palette, "on");
        for board in goals.boards {
            code(ui, palette, board);
        }
        muted(
            ui,
            palette,
            "and give it a ```cashflow-goal block (see `cashflow goals --help`).",
        );
    });
}

/// A tile per goal. Returns a link clicked.
fn list_ui(ui: &mut egui::Ui, palette: &Palette, report: &Report) -> Option<Action> {
    let mut action = None;
    for (at, row) in report.rows.iter().enumerate() {
        if let Some(clicked) = tile(ui, palette, row) {
            action = Some(match clicked {
                TileClick::Card => Action::Card(at),
                TileClick::Chart => Action::Open(at),
            });
        }
        ui.add_space(GAP);
    }
    action
}

/// What was clicked in a tile.
enum TileClick {
    /// The title: open the card.
    Card,
    /// "Chart": show the goal's detail.
    Chart,
}

/// One goal's tile: its title and status, what it asks for, then its
/// progress, or why it can't be measured.
fn tile(ui: &mut egui::Ui, palette: &Palette, row: &GoalRow) -> Option<TileClick> {
    let mut clicked = None;
    egui::Frame::new()
        .fill(palette.surface)
        .corner_radius(8.0)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                if title_link(ui, palette, &row.title, 16.0).clicked() {
                    clicked = Some(TileClick::Card);
                }
                pill_ui(ui, palette, row);
                if row.outcome.is_ok() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let chart = RichText::new("Chart →").color(palette.accent);
                        let response = ui.add(egui::Button::new(chart).frame(false));
                        if response.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if response.clicked() {
                            clicked = Some(TileClick::Chart);
                        }
                    });
                }
            });
            ui.horizontal_wrapped(|ui| {
                small(ui, &row.card_ref, palette.text_muted);
                small(ui, "·", palette.text_muted);
                small(ui, &row.column, palette.text_muted);
                if !row.spec_text.is_empty() {
                    small(ui, "·", palette.text_muted);
                    small(ui, &row.spec_text, palette.text_muted);
                }
            });
            ui.add_space(8.0);
            match &row.outcome {
                Ok(measured) => progress_ui(ui, palette, measured),
                Err(err) => {
                    ui.label(RichText::new(err).color(palette.expense));
                }
            }
        });
    clicked
}

/// The value so far against the target as a bar, then the paces and the
/// ETA.
fn progress_ui(ui: &mut egui::Ui, palette: &Palette, m: &Measured) {
    let color = status_color(palette, m.status);
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(&m.current_text)
                .size(16.0)
                .strong()
                .color(palette.text),
        );
        ui.label(RichText::new("of").color(palette.text_muted));
        ui.label(RichText::new(&m.target_text).color(palette.text));
        ui.label(RichText::new(&m.window_text).color(palette.text_muted));
    });
    ui.add_space(4.0);
    bar(ui, palette, m.fraction, color);
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        if let Some(required) = &m.required_text {
            small(ui, required, palette.text);
            small(ui, "·", palette.text_muted);
        }
        small(ui, &m.pace_text, palette.text);
        small(ui, "·", palette.text_muted);
        small(ui, "projected", palette.text_muted);
        small(ui, &m.projected_text, palette.text);
        if let Some(eta) = &m.eta_text {
            small(ui, "·", palette.text_muted);
            small(ui, eta, color);
        }
    });
    if let Some(unconverted) = &m.unconverted_text {
        small(ui, unconverted, palette.warning);
    }
}

/// A full-width progress bar, `fraction` of it filled in `color`.
fn bar(ui: &mut egui::Ui, palette: &Palette, fraction: f32, color: Color32) {
    let size = egui::vec2(ui.available_width(), BAR_HEIGHT);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    let radius = BAR_HEIGHT / 2.0;
    painter.rect_filled(rect, radius, palette.border);
    if fraction > 0.0 {
        let mut filled = rect;
        filled.set_width((rect.width() * fraction).max(BAR_HEIGHT));
        painter.rect_filled(filled, radius, color);
    }
}

/// The status pill: achieved, on track, at risk, behind, or that the goal
/// needs fixing.
fn pill_ui(ui: &mut egui::Ui, palette: &Palette, row: &GoalRow) {
    let (text, color) = match &row.outcome {
        Ok(m) => (m.status.name(), status_color(palette, m.status)),
        Err(_) => ("needs fixing", palette.warning),
    };
    egui::Frame::new()
        .fill(color.gamma_multiply(0.18))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().strong().color(color));
        });
}

/// The color a status is drawn in.
pub(crate) fn status_color(palette: &Palette, status: Status) -> Color32 {
    match status {
        Status::Achieved => palette.accent,
        Status::OnTrack => palette.income,
        Status::AtRisk => palette.warning,
        Status::Behind => palette.expense,
    }
}

/// A goal's title as a link to its card.
fn title_link(ui: &mut egui::Ui, palette: &Palette, title: &str, size: f32) -> egui::Response {
    let text = RichText::new(title).size(size).strong().color(palette.text);
    let response = ui
        .add(egui::Button::new(text).frame(false))
        .on_hover_text("Open the card in Headway");
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        let rect = response.rect;
        ui.painter().hline(
            rect.x_range(),
            rect.bottom(),
            egui::Stroke::new(1.0, palette.text_muted),
        );
    }
    response
}

/// One goal's detail: back to the list, its title, figures and chart.
fn detail_ui(ui: &mut egui::Ui, palette: &Palette, row: &GoalRow, narrow: bool) -> Option<Action> {
    let mut action = None;
    let back =
        egui::Button::new(RichText::new("← All goals").color(palette.text_muted)).frame(false);
    if ui.add(back).clicked() {
        action = Some(Action::Back);
    }
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        if title_link(ui, palette, &row.title, 22.0).clicked() {
            action = Some(Action::OpenCard);
        }
        pill_ui(ui, palette, row);
    });
    ui.horizontal_wrapped(|ui| {
        small(ui, &row.card_ref, palette.text_muted);
        small(ui, "·", palette.text_muted);
        small(ui, &row.spec_text, palette.text_muted);
    });
    ui.add_space(12.0);
    let Ok(measured) = &row.outcome else {
        return action;
    };
    progress_ui(ui, palette, measured);
    ui.add_space(20.0);
    ui.label(
        RichText::new(measured.chart.caption)
            .size(18.0)
            .strong()
            .color(palette.text),
    );
    ui.add_space(6.0);
    chart_ui(ui, palette, measured, narrow);
    action
}

/// The history as a line, the projection dashed on from it, the target and
/// the deadline.
fn chart_ui(ui: &mut egui::Ui, palette: &Palette, m: &Measured, narrow: bool) {
    let chart = &m.chart;
    let color = palette.series[0];
    let format_value = |v: f64| chart.axis_text(v);
    let bucket_label = |i: usize| chart.labels.get(i).cloned().unwrap_or_default();
    let actual = [Series {
        label: "Actual",
        values: &chart.actual,
        color,
    }];
    let data = TimeSeries {
        series: &actual,
        bucket_label: &bucket_label,
        format_value: &format_value,
    };
    let trails = [Trail {
        start: chart.projection_start,
        join: chart.join,
        values: &chart.projection,
        color,
    }];
    let levels = [Level {
        value: chart.target,
        label: "target",
        color: status_color(palette, m.status),
    }];
    let deadline = chart.deadline.map(|bucket| Marker {
        bucket,
        label: &chart.deadline_label,
        color: palette.text_muted,
    });
    let marks = Marks {
        trails: &trails,
        levels: &levels,
        markers: deadline.as_slice(),
    };
    let height = if narrow {
        NARROW_CHART_HEIGHT
    } else {
        CHART_HEIGHT
    };
    let size = egui::vec2(ui.available_width(), height);
    let response = line_chart_marked(
        ui,
        size,
        &data,
        &marks,
        &TimeSeriesStyle::default(),
        |ui, hover| tooltip(ui, palette, hover, m),
    );
    response
        .response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, "Goal progress chart"));
}

/// The chart's tooltip: the month, its actual and projected values where
/// it has them, and the target.
fn tooltip(ui: &mut egui::Ui, palette: &Palette, hover: &HoverPoint<'_>, m: &Measured) {
    let chart: &Chart = &m.chart;
    let bucket = hover.bucket;
    ui.label(
        RichText::new((hover.data.bucket_label)(bucket))
            .strong()
            .color(palette.text),
    );
    let projected = bucket
        .checked_sub(chart.projection_start)
        .and_then(|i| chart.projection_texts.get(i));
    let lines = [
        ("Actual", chart.actual_texts.get(bucket)),
        ("Projected", projected),
        ("Target", Some(&m.target_text)),
    ];
    for (name, text) in lines {
        let Some(text) = text else {
            continue;
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(name).color(palette.text_muted));
            ui.label(RichText::new(text).color(palette.text));
        });
    }
}

/// The Overview's strip: how many goals are on track, at risk and behind,
/// as a link into the Goals view. Draws nothing with no goals. Returns
/// whether it was clicked.
pub(crate) fn strip_ui(ui: &mut egui::Ui, palette: &Palette, report: &Report) -> bool {
    let summary = &report.summary;
    if summary.text.is_empty() {
        return false;
    }
    let color = if summary.errors + summary.behind > 0 {
        palette.expense
    } else if summary.at_risk > 0 {
        palette.warning
    } else {
        palette.income
    };
    let mut clicked = false;
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Goals").small().color(palette.text_muted));
        let text = RichText::new(&summary.text).small().color(color);
        let button = egui::Button::new(text)
            .fill(palette.surface)
            .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.5)))
            .corner_radius(10.0);
        let response = ui.add(button).on_hover_text("Open the Goals view.");
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        clicked = response.clicked();
    });
    clicked
}

/// A small label that never wraps inside itself.
fn small(ui: &mut egui::Ui, text: &str, color: Color32) {
    ui.add(
        egui::Label::new(RichText::new(text).small().color(color))
            .wrap_mode(egui::TextWrapMode::Extend),
    );
}

/// A name set as code.
fn code(ui: &mut egui::Ui, palette: &Palette, text: &str) {
    ui.label(RichText::new(text).code().color(palette.text));
}

/// A line of muted text: an empty state or a hint.
fn muted(ui: &mut egui::Ui, palette: &Palette, text: &'static str) {
    ui.label(RichText::new(text).color(palette.text_muted));
}
