//! The Overview: the landing view.
//!
//! Over the shared [`Period`] (this month by default):
//! - **Net cashflow** (income less expenses), with how it compares with
//!   the period before and with the trailing twelve months' average.
//! - **Income** and **expenses**, and the **savings rate** (net / income).
//!   Routing fees are a figure of their own beside them, never income (see
//!   [`cashflow::net`]).
//! - **Net per month** over the last twelve, as an area chart.
//! - The **top five** expense and income categories, each a link into the
//!   Categories view drilled into it.
//! - **What needs attention**: postings with no price, transactions nothing
//!   has categorized (a link to the Transactions view's triage inbox), and
//!   segments the journal is missing (a link to Settings).
//! - A chip per **journal layer**; switching one asks for the books to be
//!   reloaded without it ([`crate::Layers`]).
//!
//! The figures are worked out by [`model::overview`] only when the books,
//! the period or today change, and drawn from there each frame.

pub mod model;

use egui::{Color32, RichText};
use ledgerdb::Date;
use notedeck_ui::charts::{HoverPoint, Series, TimeSeries, TimeSeriesStyle, area_chart};

use super::categories::{self, model::Flow};
use super::{LedgerCx, ViewCx};
use crate::{Period, View, money::compact, ui::palette::Palette};
use model::{Delta, Inputs, Overview};

/// Height of the net cashflow chart in the wide layout.
const CHART_HEIGHT: f32 = 200.0;

/// Height of the net cashflow chart on a phone.
const NARROW_CHART_HEIGHT: f32 = 160.0;

/// Height every figure tile is at least, so a row of them lines up.
const TILE_HEIGHT: f32 = 96.0;

/// Gap between tiles, and between the two top lists.
const GAP: f32 = 12.0;

/// The Overview's state: its figures, cached.
#[derive(Default)]
pub(crate) struct OverviewState {
    /// What [`Self::overview`] was computed from; a frame with a different
    /// key recomputes it.
    key: Option<Key>,
    /// The figures, once there are books in the home commodity.
    overview: Option<Overview>,
    /// The count [`Self::missing_text`] was written for.
    missing: usize,
    /// "N missing segments", when the journal lacks any.
    missing_text: Option<String>,
}

/// What the cached figures depend on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Key {
    generation: u64,
    period: Period,
    today: Date,
}

impl OverviewState {
    /// Recompute what is stale from `ledger`.
    #[profiling::function]
    fn refresh(&mut self, ledger: LedgerCx<'_>, period: Period, today: Date, missing: usize) {
        if self.missing != missing {
            self.missing = missing;
            self.missing_text = (missing > 0).then(|| {
                let s = if missing == 1 { "" } else { "s" };
                format!("{missing} missing segment{s}")
            });
        }
        let key = Key {
            generation: ledger.generation,
            period,
            today,
        };
        if self.key == Some(key) {
            return;
        }
        self.key = Some(key);
        self.overview = ledger.books.home().map(|home| {
            model::overview(Inputs {
                books: ledger.books,
                home,
                period,
                today,
            })
        });
    }
}

/// A click this frame that changes something outside the figures.
enum Action {
    /// Switch the layer at this index of [`Overview::layers`].
    ToggleLayer(usize),
    /// Show this view.
    Open(View),
    /// Show the Categories view drilled into the row at this index of a
    /// top list.
    Category(Flow, usize),
}

/// Draw the Overview.
#[profiling::function]
pub(crate) fn ui(cx: &mut ViewCx<'_>, ui: &mut egui::Ui) {
    let palette = cx.palette;
    let narrow = notedeck::ui::is_narrow(ui.ctx());
    crate::ui::period_picker::heading(ui, palette, View::Overview, cx.period, narrow);
    ui.add_space(12.0);

    let Some(ledger) = cx.ledger else {
        muted(ui, palette, "No journal loaded yet.");
        return;
    };
    let period = *cx.period;
    let states = &mut *cx.states;
    let state = &mut states.overview;
    state.refresh(ledger, period, cx.today, cx.missing_segments);
    let Some(overview) = &state.overview else {
        muted(
            ui,
            palette,
            "The journal has no postings in the home commodity, so nothing can be totalled.",
        );
        return;
    };

    let layers = &*cx.layers;
    let missing = state.missing_text.as_deref();
    let mut action = None;
    egui::ScrollArea::vertical()
        .id_salt("cashflow_overview")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            action = status_ui(ui, palette, overview, layers, missing);
            ui.add_space(12.0);
            figures_ui(ui, palette, overview, period, narrow);
            ui.add_space(24.0);
            trend_ui(ui, palette, overview, narrow);
            ui.add_space(24.0);
            if let Some(clicked) = top_ui(ui, palette, overview, period, narrow) {
                action = Some(clicked);
            }
        });

    match action {
        Some(Action::ToggleLayer(at)) => {
            if let Some(layer) = overview.layers.get(at) {
                cx.layers.toggle(layer);
            }
        }
        Some(Action::Open(view)) => cx.open = Some(view),
        Some(Action::Category(flow, at)) => {
            let rows = match flow {
                Flow::Spending => &overview.top_expenses,
                Flow::Earning => &overview.top_income,
            };
            if let Some(row) = rows.get(at) {
                states.categories.open(flow, &row.detail);
                cx.open = Some(View::Categories);
            }
        }
        None => {}
    }
}

/// The layer chips, then a badge for each thing that needs attention.
/// Returns a chip or a badge clicked.
fn status_ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    overview: &Overview,
    layers: &crate::Layers,
    missing: Option<&str>,
) -> Option<Action> {
    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        if !overview.layers.is_empty() {
            ui.label(RichText::new("Layers").small().color(palette.text_muted));
        }
        for (at, layer) in overview.layers.iter().enumerate() {
            let on = layers.is_on(layer);
            let color = if on {
                palette.text
            } else {
                palette.text_muted
            };
            let chip = egui::Button::selectable(on, RichText::new(layer).color(color));
            if ui
                .add(chip)
                .on_hover_text("Switch this journal layer on or off.")
                .clicked()
            {
                action = Some(Action::ToggleLayer(at));
            }
        }
        ui.add_space(12.0);

        let badges = [
            (
                overview.unknown.as_deref(),
                Some(View::Transactions),
                "Nothing has categorized these yet. Open the triage inbox.",
            ),
            (
                overview.unpriced.as_deref(),
                None,
                "The price db has no rate into the home commodity on their dates, so they are left out of every figure.",
            ),
            (
                missing,
                Some(View::Settings),
                "The vault lists segments this journal does not have, so every figure is partial until they sync.",
            ),
        ];
        for (text, link, hint) in badges {
            let Some(text) = text else { continue };
            if badge(ui, palette, text, link.is_some()).on_hover_text(hint).clicked() {
                action = link.map(Action::Open);
            }
        }
    });
    action
}

/// A warning badge: `text` on a raised chip, a link when `links`.
fn badge(ui: &mut egui::Ui, palette: &Palette, text: &str, links: bool) -> egui::Response {
    let label = RichText::new(text).small().color(palette.warning);
    let button = egui::Button::new(label)
        .fill(palette.surface)
        .stroke(egui::Stroke::new(1.0, palette.warning.gamma_multiply(0.5)))
        .corner_radius(10.0);
    let response = ui.add(button);
    if links && response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// The four figure tiles (net, income, expenses, savings rate) in a row,
/// or two rows on a phone, then the routing figure when there is one.
fn figures_ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    overview: &Overview,
    period: Period,
    narrow: bool,
) {
    let net_color = if overview.flow.net() < 0 {
        palette.expense
    } else {
        palette.income
    };
    let net = |ui: &mut egui::Ui| {
        tile(
            ui,
            palette,
            "Net cashflow",
            &overview.net_text,
            net_color,
            |ui| {
                if let (Some(delta), Some(versus)) =
                    (&overview.vs_previous, period.previous_label())
                {
                    delta_ui(ui, palette, delta, versus);
                }
                if let Some(delta) = &overview.vs_average {
                    delta_ui(ui, palette, delta, "vs the 12-month average");
                }
            },
        )
    };
    let income = |ui: &mut egui::Ui| {
        tile(
            ui,
            palette,
            "Income",
            &overview.income_text,
            palette.text,
            |_| {},
        )
    };
    let expenses = |ui: &mut egui::Ui| {
        tile(
            ui,
            palette,
            "Expenses",
            &overview.expenses_text,
            palette.text,
            |_| {},
        )
    };
    let savings = |ui: &mut egui::Ui| {
        tile(
            ui,
            palette,
            "Savings rate",
            &overview.savings_text,
            palette.text,
            |ui| {
                ui.label(
                    RichText::new("of income kept")
                        .small()
                        .color(palette.text_muted),
                );
            },
        )
    };

    ui.spacing_mut().item_spacing.x = GAP;
    if narrow {
        net(ui);
        ui.add_space(GAP);
        ui.columns_const(|[left, right]| {
            income(left);
            expenses(right);
        });
        ui.add_space(GAP);
        savings(ui);
    } else {
        ui.columns_const(|[a, b, c, d]| {
            net(a);
            income(b);
            expenses(c);
            savings(d);
        });
    }

    if let Some(routed) = &overview.routed_text {
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Routing").color(palette.text_muted));
            ui.label(RichText::new(routed).color(palette.text));
            ui.label(
                RichText::new("earned by the node, kept out of income")
                    .small()
                    .color(palette.text_muted),
            );
        });
    }
}

/// One figure on a raised tile: its name, its value, and whatever `below`
/// adds under it.
fn tile(
    ui: &mut egui::Ui,
    palette: &Palette,
    name: &'static str,
    value: &str,
    color: Color32,
    below: impl FnOnce(&mut egui::Ui),
) {
    egui::Frame::new()
        .fill(palette.surface)
        .corner_radius(8.0)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_min_height(TILE_HEIGHT);
            ui.label(RichText::new(name).color(palette.text_muted));
            ui.label(RichText::new(value).size(22.0).strong().color(color));
            below(ui);
        });
}

/// A change in net and what it is measured against, colored by which way
/// it went.
fn delta_ui(ui: &mut egui::Ui, palette: &Palette, delta: &Delta, versus: &'static str) {
    let color = match delta.amount.signum() {
        1 => palette.income,
        -1 => palette.expense,
        _ => palette.text_muted,
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(&delta.text).small().color(color));
        ui.label(RichText::new(versus).small().color(palette.text_muted));
    });
}

/// Net per month over the last twelve, as an area chart, values below zero
/// in the expense color.
fn trend_ui(ui: &mut egui::Ui, palette: &Palette, overview: &Overview, narrow: bool) {
    section_heading(ui, palette, "Net cashflow, last 12 months");
    ui.add_space(6.0);
    let Some(trend) = &overview.trend else {
        muted(ui, palette, "Nothing in the journal yet.");
        return;
    };
    let precision = overview.precision;
    let format_value = move |v: f64| compact(v, precision);
    let bucket_label = |i: usize| trend.window.labels.get(i).cloned().unwrap_or_default();
    let series = [Series {
        label: "Net",
        values: &trend.net.values,
        color: palette.accent,
    }];
    let data = TimeSeries {
        series: &series,
        bucket_label: &bucket_label,
        format_value: &format_value,
    };
    let style = TimeSeriesStyle {
        negative_color: Some(palette.expense),
        ..TimeSeriesStyle::default()
    };
    let height = if narrow {
        NARROW_CHART_HEIGHT
    } else {
        CHART_HEIGHT
    };
    let size = egui::vec2(ui.available_width(), height);
    let chart = area_chart(ui, size, &data, &style, |ui, hover| {
        tooltip(ui, palette, hover, &trend.net.texts)
    });
    chart
        .response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, "Net cashflow chart"));
}

/// The chart's tooltip: the month and its net.
fn tooltip(ui: &mut egui::Ui, palette: &Palette, hover: &HoverPoint<'_>, texts: &[String]) {
    let bucket = hover.bucket;
    ui.label(
        RichText::new((hover.data.bucket_label)(bucket))
            .strong()
            .color(palette.text),
    );
    let Some(text) = texts.get(bucket) else {
        return;
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new("Net").color(palette.text_muted));
        ui.label(RichText::new(text).color(palette.text));
    });
}

/// The top expense and income categories side by side, or stacked on a
/// phone. Returns the row clicked.
fn top_ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    overview: &Overview,
    period: Period,
    narrow: bool,
) -> Option<Action> {
    let lists = [
        (Flow::Spending, "Top expenses", &overview.top_expenses),
        (Flow::Earning, "Top income", &overview.top_income),
    ];
    let mut action = None;
    if narrow {
        for (flow, title, rows) in lists {
            if let Some(clicked) = top_list(ui, palette, flow, title, rows, period) {
                action = Some(clicked);
            }
            ui.add_space(GAP);
        }
        return action;
    }
    let [(spending, s_title, s_rows), (earning, e_title, e_rows)] = lists;
    ui.spacing_mut().item_spacing.x = GAP * 2.0;
    ui.columns_const(|[left, right]| {
        let picked = [
            top_list(left, palette, spending, s_title, s_rows, period),
            top_list(right, palette, earning, e_title, e_rows, period),
        ];
        action = picked.into_iter().flatten().next();
    });
    action
}

/// One top list: its heading and a bar per category, each a link into the
/// Categories view. Returns the row clicked.
fn top_list(
    ui: &mut egui::Ui,
    palette: &Palette,
    flow: Flow,
    title: &'static str,
    rows: &[categories::model::Row],
    period: Period,
) -> Option<Action> {
    section_heading(ui, palette, title);
    ui.add_space(4.0);
    if rows.is_empty() {
        muted(ui, palette, "Nothing in this period.");
        return None;
    }
    let max = rows
        .iter()
        .map(|r| r.amount.unsigned_abs())
        .max()
        .unwrap_or(0);
    let mut action = None;
    for (at, row) in rows.iter().enumerate() {
        let response = categories::bar_row(ui, palette, flow, row, max)
            .on_hover_ui(|ui| categories::tooltip(ui, palette, row, period));
        if response.clicked() {
            action = Some(Action::Category(flow, at));
        }
    }
    action
}

/// A section's heading.
fn section_heading(ui: &mut egui::Ui, palette: &Palette, title: &'static str) {
    ui.label(RichText::new(title).size(18.0).strong().color(palette.text));
}

/// A line of muted text: an empty state or a hint.
fn muted(ui: &mut egui::Ui, palette: &Palette, text: &'static str) {
    ui.label(RichText::new(text).color(palette.text_muted));
}
