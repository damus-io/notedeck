//! The Projects view: revenue per project, over the shared [`Period`].
//!
//! - **The list**: a tile per project with its revenue, its share of all
//!   revenue, its change from the period before, its net when the project
//!   tags expense accounts too, a sparkline of its last twelve months, and
//!   its revenue mix by source as a stacked bar. Income no project claims
//!   is the last tile, with a link to the triage inbox.
//! - **A project's detail** (click its tile): revenue per month as a line
//!   chart (expenses beside it when the project has them) and its sources,
//!   largest first, each a link into the Categories view drilled into it.
//!
//! Which account is in which project comes from the chart of accounts
//! ([`cashflow::projects::Projects`], handed in with
//! [`crate::CashflowApp::set_projects`]). The figures are worked out by
//! [`model::projects`] only when the books, the attribution, the period or
//! today change, and drawn from there each frame.

pub mod model;

use egui::{Color32, RichText, Sense};
use ledgerdb::Date;
use notedeck_ui::charts::{
    HoverPoint, Series, SparkStyle, TimeSeries, TimeSeriesStyle, line_chart, sparkline,
};

use super::categories::{self, model::Flow};
use super::{LedgerCx, ViewCx};
use crate::{Period, View, money::compact, ui::palette::Palette};
use model::{Inputs, ProjectRow, Report};

/// Height of the detail's revenue chart in the wide layout.
const CHART_HEIGHT: f32 = 220.0;

/// Height of the detail's revenue chart on a phone.
const NARROW_CHART_HEIGHT: f32 = 160.0;

/// Size of a tile's sparkline.
const SPARK_SIZE: egui::Vec2 = egui::vec2(96.0, 22.0);

/// Height of a tile's revenue mix bar.
const MIX_HEIGHT: f32 = 8.0;

/// Gap between tiles.
const GAP: f32 = 12.0;

/// The Projects view's state: its figures, cached, and the project open.
#[derive(Default)]
pub(crate) struct ProjectsState {
    /// What [`Self::report`] was computed from; a frame with a different
    /// key recomputes it.
    key: Option<Key>,
    /// The figures, once there are books in the home commodity.
    report: Option<Report>,
    /// The project whose detail is showing, if one is.
    open: Option<Opened>,
}

/// A project whose detail is showing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Opened {
    /// The project, `None` for the unattributed income.
    project: Option<String>,
}

/// What the cached figures depend on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Key {
    generation: u64,
    period: Period,
    today: Date,
}

impl ProjectsState {
    /// Recompute the figures if `ledger`, `period` or `today` moved.
    #[profiling::function]
    fn refresh(
        &mut self,
        ledger: LedgerCx<'_>,
        projects: &cashflow::projects::Projects,
        period: Period,
        today: Date,
    ) {
        let key = Key {
            generation: ledger.generation,
            period,
            today,
        };
        if self.key == Some(key) {
            return;
        }
        self.key = Some(key);
        self.report = ledger.books.home().map(|home| {
            model::projects(Inputs {
                books: ledger.books,
                home,
                projects,
                period,
                today,
            })
        });
    }
}

/// A click this frame that changes what shows.
enum Action {
    /// Show the detail of the row at this index of [`Report::rows`].
    Open(usize),
    /// Back from a detail to the list.
    Back,
    /// Show the triage inbox.
    Triage,
    /// Show the Categories view drilled into the source at this index of
    /// the open project's [`ProjectRow::sources`].
    Source(usize),
}

/// Draw the Projects view.
#[profiling::function]
pub(crate) fn ui(cx: &mut ViewCx<'_>, ui: &mut egui::Ui) {
    let palette = cx.palette;
    let narrow = notedeck::ui::is_narrow(ui.ctx());
    crate::ui::period_picker::heading(ui, palette, View::Projects, cx.period, narrow);
    ui.add_space(12.0);

    let Some(ledger) = cx.ledger else {
        muted(ui, palette, "No journal loaded yet.");
        return;
    };
    let period = *cx.period;
    let states = &mut *cx.states;
    let state = &mut states.projects;
    state.refresh(ledger, cx.projects, period, cx.today);
    let Some(report) = &state.report else {
        muted(
            ui,
            palette,
            "The journal has no postings in the home commodity, so nothing can be totalled.",
        );
        return;
    };

    let open = state
        .open
        .as_ref()
        .and_then(|o| report.row(o.project.as_deref()));
    let mut action = None;
    egui::ScrollArea::vertical()
        .id_salt("cashflow_projects")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            action = match open {
                Some(row) => detail_ui(ui, palette, report, row, period, narrow),
                None => list_ui(ui, palette, report, period),
            };
        });

    match action {
        Some(Action::Open(at)) => {
            state.open = report.rows.get(at).map(|row| Opened {
                project: row.project.clone(),
            });
        }
        Some(Action::Back) => state.open = None,
        Some(Action::Triage) => cx.open = Some(View::Transactions),
        Some(Action::Source(at)) => {
            if let Some(source) = open.and_then(|row| row.sources.get(at)) {
                states.categories.open(Flow::Earning, &source.detail);
                cx.open = Some(View::Categories);
            }
        }
        None => {}
    }
}

/// The total, then a tile per project. Returns a tile or link clicked.
fn list_ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    report: &Report,
    period: Period,
) -> Option<Action> {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Revenue").color(palette.text_muted));
        ui.label(
            RichText::new(&report.total_text)
                .size(18.0)
                .strong()
                .color(palette.text),
        );
    });
    if !report.attributed {
        muted(
            ui,
            palette,
            "No account is in a project yet. Put one in with `cashflow account set <account> --project <name>`.",
        );
    }
    ui.add_space(GAP);
    if report.rows.is_empty() {
        muted(ui, palette, "No revenue in the journal yet.");
        return None;
    }
    let mut action = None;
    for (at, row) in report.rows.iter().enumerate() {
        if tile(ui, palette, row, period).clicked() {
            action = Some(Action::Open(at));
        }
        if row.project.is_none() && triage_link(ui, palette).clicked() {
            action = Some(Action::Triage);
        }
        ui.add_space(GAP);
    }
    action
}

/// One project's tile: its name and share, then its sparkline and revenue
/// at the right; its changes under them; then its revenue mix. The whole tile is a button into the
/// project's detail.
fn tile(ui: &mut egui::Ui, palette: &Palette, row: &ProjectRow, period: Period) -> egui::Response {
    let inner = egui::Frame::new()
        .fill(palette.surface)
        .corner_radius(8.0)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(&row.label)
                        .size(16.0)
                        .strong()
                        .color(palette.text),
                );
                ui.label(RichText::new(&row.share_text).color(palette.text_muted));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(&row.revenue_text)
                            .size(16.0)
                            .strong()
                            .color(palette.text),
                    );
                    ui.add_space(8.0);
                    sparkline(
                        ui,
                        SPARK_SIZE,
                        &row.spark,
                        palette.income,
                        SparkStyle::default(),
                    )
                    .on_hover_text("Revenue per month, last 12 months");
                });
            });
            ui.horizontal_wrapped(|ui| changes_ui(ui, palette, row, period));
            ui.add_space(6.0);
            mix_ui(ui, palette, row);
        });
    let response = inner.response.interact(Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &row.label));
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        ui.painter().rect_stroke(
            response.rect,
            8.0,
            egui::Stroke::new(1.0, palette.border),
            egui::StrokeKind::Inside,
        );
    }
    response
}

/// A row's change from the period before, and its spending and net when
/// it has them, each phrase kept whole so a narrow tile wraps between them.
fn changes_ui(ui: &mut egui::Ui, palette: &Palette, row: &ProjectRow, period: Period) {
    if let (Some(delta), Some(versus)) = (&row.vs_previous, period.previous_label()) {
        let color = match delta.amount.signum() {
            1 => palette.income,
            -1 => palette.expense,
            _ => palette.text_muted,
        };
        small(ui, &delta.text, color);
        small(ui, versus, palette.text_muted);
        ui.add_space(8.0);
    }
    if let Some(net) = &row.net {
        small(ui, "spent", palette.text_muted);
        small(ui, &net.expenses_text, palette.text);
        ui.add_space(4.0);
        let color = if net.net < 0 {
            palette.expense
        } else {
            palette.income
        };
        small(ui, "net", palette.text_muted);
        small(ui, &net.net_text, color);
    }
}

/// A small label that never wraps inside itself.
fn small(ui: &mut egui::Ui, text: &str, color: Color32) {
    ui.add(
        egui::Label::new(RichText::new(text).small().color(color))
            .wrap_mode(egui::TextWrapMode::Extend),
    );
}

/// The revenue mix as one stacked bar, a color per source, and a legend
/// under it naming each with its share.
fn mix_ui(ui: &mut egui::Ui, palette: &Palette, row: &ProjectRow) {
    if row.mix.is_empty() {
        return;
    }
    let size = egui::vec2(ui.available_width(), MIX_HEIGHT);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    let radius = MIX_HEIGHT / 2.0;
    painter.rect_filled(rect, radius, palette.border);
    let mut left = rect.left();
    for (at, part) in row.mix.iter().enumerate() {
        let width = rect.width() * part.share;
        let slice = egui::Rect::from_min_size(
            egui::pos2(left, rect.top()),
            egui::vec2(width, rect.height()),
        );
        painter.rect_filled(slice, 0.0, mix_color(palette, at));
        left += width;
    }
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        for (at, part) in row.mix.iter().enumerate() {
            ui.label(RichText::new("●").small().color(mix_color(palette, at)));
            ui.label(RichText::new(&part.label).small().color(palette.text));
            ui.label(
                RichText::new(&part.share_text)
                    .small()
                    .color(palette.text_muted),
            );
            ui.add_space(6.0);
        }
    });
}

/// The color of the mix slice at `at`: a series color per source, muted
/// for "Other".
fn mix_color(palette: &Palette, at: usize) -> Color32 {
    if at >= model::MIX {
        return palette.text_muted;
    }
    palette.series[at % palette.series.len()]
}

/// The unattributed income's link into the triage inbox.
fn triage_link(ui: &mut egui::Ui, palette: &Palette) -> egui::Response {
    let text = RichText::new("Give it a project in the triage inbox →")
        .small()
        .color(palette.accent);
    let response = ui.add(egui::Button::new(text).frame(false));
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// One project's detail: back to the list, its figures, its revenue per
/// month, and its sources. Returns a link clicked.
fn detail_ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    report: &Report,
    row: &ProjectRow,
    period: Period,
    narrow: bool,
) -> Option<Action> {
    let mut action = None;
    let back =
        egui::Button::new(RichText::new("← All projects").color(palette.text_muted)).frame(false);
    if ui.add(back).clicked() {
        action = Some(Action::Back);
    }
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(&row.label)
                .size(22.0)
                .strong()
                .color(palette.text),
        );
        ui.label(RichText::new(&row.share_text).color(palette.text_muted));
        ui.label(RichText::new("of revenue").color(palette.text_muted));
    });
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(&row.revenue_text)
                .size(16.0)
                .strong()
                .color(palette.text),
        );
        ui.add_space(8.0);
        changes_ui(ui, palette, row, period);
    });
    if row.project.is_none() && triage_link(ui, palette).clicked() {
        action = Some(Action::Triage);
    }
    ui.add_space(16.0);

    section_heading(ui, palette, "Revenue per month, last 12 months");
    ui.add_space(6.0);
    chart_ui(ui, palette, report, row, narrow);
    ui.add_space(24.0);

    section_heading(ui, palette, "Sources");
    ui.add_space(4.0);
    if row.sources.is_empty() {
        muted(ui, palette, "Nothing earned in this period.");
        return action;
    }
    let max = row
        .sources
        .iter()
        .map(|r| r.amount.unsigned_abs())
        .max()
        .unwrap_or(0);
    for (at, source) in row.sources.iter().enumerate() {
        let response = categories::bar_row(ui, palette, Flow::Earning, source, max)
            .on_hover_ui(|ui| categories::tooltip(ui, palette, source, period));
        if response.clicked() {
            action = Some(Action::Source(at));
        }
    }
    action
}

/// Revenue per month as a line, and expenses beside it when the project
/// has them.
fn chart_ui(ui: &mut egui::Ui, palette: &Palette, report: &Report, row: &ProjectRow, narrow: bool) {
    let Some(window) = &report.window else {
        muted(ui, palette, "Nothing in the journal yet.");
        return;
    };
    let precision = report.precision;
    let format_value = move |v: f64| compact(v, precision);
    let bucket_label = |i: usize| window.labels.get(i).cloned().unwrap_or_default();
    let revenue = Series {
        label: "Revenue",
        values: &row.revenue_line.values,
        color: palette.income,
    };
    let both;
    let series: &[Series<'_>] = match &row.expenses_line {
        Some(expenses) => {
            both = [
                revenue,
                Series {
                    label: "Expenses",
                    values: &expenses.values,
                    color: palette.expense,
                },
            ];
            &both
        }
        None => std::slice::from_ref(&revenue),
    };
    let data = TimeSeries {
        series,
        bucket_label: &bucket_label,
        format_value: &format_value,
    };
    let height = if narrow {
        NARROW_CHART_HEIGHT
    } else {
        CHART_HEIGHT
    };
    let size = egui::vec2(ui.available_width(), height);
    let chart = line_chart(ui, size, &data, &TimeSeriesStyle::default(), |ui, hover| {
        tooltip(ui, palette, hover, row)
    });
    chart.response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Image, true, "Project revenue chart")
    });
}

/// The chart's tooltip: the month, its revenue, and its expenses when the
/// project has them.
fn tooltip(ui: &mut egui::Ui, palette: &Palette, hover: &HoverPoint<'_>, row: &ProjectRow) {
    let bucket = hover.bucket;
    ui.label(
        RichText::new((hover.data.bucket_label)(bucket))
            .strong()
            .color(palette.text),
    );
    let lines = [
        ("Revenue", Some(&row.revenue_line)),
        ("Expenses", row.expenses_line.as_ref()),
    ];
    for (name, line) in lines {
        let Some(text) = line.and_then(|l| l.texts.get(bucket)) else {
            continue;
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(name).color(palette.text_muted));
            ui.label(RichText::new(text).color(palette.text));
        });
    }
}

/// A section's heading.
fn section_heading(ui: &mut egui::Ui, palette: &Palette, title: &'static str) {
    ui.label(RichText::new(title).size(18.0).strong().color(palette.text));
}

/// A line of muted text: an empty state or a hint.
fn muted(ui: &mut egui::Ui, palette: &Palette, text: &'static str) {
    ui.label(RichText::new(text).color(palette.text_muted));
}
