//! The Trends view: progress over time.
//!
//! Three charts over the months of one [`Span`] (12 months, 3 years or all
//! time):
//! - **Income vs expenses:** each month's income and expenses as bars, with
//!   the net as a line over them.
//! - **Account trends:** any accounts picked from a searchable list, each
//!   one's monthly flow as a line, up to [`SERIES`] at once. An account keeps
//!   its color while it is picked.
//! - **Net worth:** everything under `assets` and `liabilities` at each
//!   month's end, other commodities (on-chain BTC) valued at that day's
//!   price, with a chip to leave them out.
//!
//! Every series comes from per-month rollups ([`model`]) and is recomputed
//! only when the books, the span or today change, or an account is picked;
//! a frame draws the cached values and their pre-formatted text (CLAUDE.md
//! rule 18). The chart's axis labels are the one exception: the chart asks
//! for them as `String`s each frame.

pub mod model;

use egui::{Color32, RichText};
use ledgerdb::Date;
use notedeck_ui::charts::{
    HoverPoint, Series, TimeSeries, TimeSeriesStyle, area_chart, bar_line_chart, line_chart,
};

use super::{LedgerCx, ViewCx};
use crate::{
    View,
    money::compact,
    ui::palette::{Palette, SERIES},
};
use model::{Line, Monthly, NetWorth, Span, Trend, Window};

/// Height of each chart in the wide layout.
const CHART_HEIGHT: f32 = 220.0;

/// Height of each chart on a phone.
const NARROW_CHART_HEIGHT: f32 = 180.0;

/// How many matching accounts the picker lists under its search field.
const MATCHES: usize = 8;

/// The Trends view's state: the span, the accounts picked, and every
/// series cached.
#[derive(Default)]
pub(crate) struct TrendsState {
    /// How far back the charts look.
    span: Span,
    /// What the cached series were computed from; a frame with a different
    /// key recomputes them all.
    key: Option<Key>,
    /// The months the charts cover, `None` for an empty journal.
    window: Option<Window>,
    /// Each month's income, expenses and net.
    monthly: Monthly,
    /// Net worth at each month's end.
    net_worth: NetWorth,
    /// Whether net worth leaves out holdings in other commodities.
    home_only: bool,
    /// Whether [`Self::home_only`] changed since net worth was computed.
    net_worth_stale: bool,
    /// The home commodity's decimals, for the axis labels.
    precision: u8,
    /// The subtitle naming the home commodity: `Amounts in CAD`.
    home: String,
    /// The accounts compared and the picker that chooses them.
    compare: Compare,
}

/// The account trends chart's picks and its picker.
#[derive(Default)]
struct Compare {
    /// Every account's full name, sorted: what the picker searches.
    accounts: Vec<String>,
    /// What is typed into the picker.
    search: String,
    /// The accounts compared, in the order they were picked.
    picked: Vec<Picked>,
}

/// What every cached series depends on besides the picks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Key {
    generation: u64,
    span: Span,
    today: Date,
}

/// An account on the trends chart.
struct Picked {
    /// The account's full name. Kept by name, so a reload of the books
    /// keeps the pick.
    name: String,
    /// Which [`Palette::series`] color it is drawn in; it keeps it until it
    /// is removed, whatever else is picked or removed.
    slot: usize,
    /// Its monthly flow, `None` until computed or when the reloaded books
    /// no longer have the account.
    trend: Option<Trend>,
    /// Whether [`Self::trend`] needs computing.
    stale: bool,
}

impl TrendsState {
    /// Recompute what is stale from `ledger`.
    #[profiling::function]
    fn refresh(&mut self, ledger: LedgerCx<'_>, today: Date) {
        let books = ledger.books;
        let Some(home) = books.home() else { return };
        let journal = books.ledger().journal();
        let key = Key {
            generation: ledger.generation,
            span: self.span,
            today,
        };
        if self.key != Some(key) {
            if self.key.map(|k| k.generation) != Some(ledger.generation) {
                self.compare.accounts = journal
                    .accounts
                    .ids()
                    .map(|a| journal.accounts.full_name(a).to_owned())
                    .collect();
                self.compare.accounts.sort_unstable();
            }
            self.key = Some(key);
            self.precision = journal.commodities.precision(home);
            self.home = format!("Amounts in {}", journal.commodities.name(home));
            self.window = model::window(self.span, today, books.query().span());
            self.monthly = match &self.window {
                Some(window) => model::monthly(books, home, window),
                None => Monthly::default(),
            };
            self.net_worth_stale = true;
            self.compare.picked.iter_mut().for_each(|p| p.stale = true);
        }

        let Some(window) = &self.window else {
            self.net_worth = NetWorth::default();
            self.compare.picked.iter_mut().for_each(|p| p.trend = None);
            return;
        };
        if self.net_worth_stale {
            self.net_worth_stale = false;
            self.net_worth = model::net_worth(books, home, window, self.home_only);
        }
        for pick in self.compare.picked.iter_mut().filter(|p| p.stale) {
            pick.stale = false;
            pick.trend = journal
                .accounts
                .lookup(&pick.name)
                .map(|account| model::trend(books, home, account, window));
        }
    }
}

impl Compare {
    /// Compare `name` too, in the first color no other pick has.
    fn pick(&mut self, name: String) {
        if self.picked.len() >= SERIES || self.picked.iter().any(|p| p.name == name) {
            return;
        }
        let slot = (0..SERIES)
            .find(|slot| self.picked.iter().all(|p| p.slot != *slot))
            .unwrap_or(0);
        self.picked.push(Picked {
            name,
            slot,
            trend: None,
            stale: true,
        });
    }
}

/// Draw the Trends view.
#[profiling::function]
pub(crate) fn ui(cx: &mut ViewCx<'_>, ui: &mut egui::Ui) {
    let palette = cx.palette;
    let state = &mut cx.states.trends;
    let narrow = notedeck::ui::is_narrow(ui.ctx());
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(View::Trends.label())
                .heading()
                .color(palette.text),
        );
        ui.add_space(16.0);
        span_picker(ui, palette, &mut state.span);
    });
    ui.add_space(12.0);

    let Some(ledger) = cx.ledger else {
        muted(ui, palette, "No journal loaded yet.");
        return;
    };
    if ledger.books.home().is_none() {
        muted(
            ui,
            palette,
            "The journal has no postings in the home commodity, so nothing can be totalled.",
        );
        return;
    }
    state.refresh(ledger, cx.today);
    let TrendsState {
        window,
        monthly,
        net_worth,
        home_only,
        net_worth_stale,
        precision,
        home,
        compare,
        ..
    } = state;
    let Some(window) = window else {
        muted(ui, palette, "Nothing in the journal yet.");
        return;
    };

    let height = if narrow {
        NARROW_CHART_HEIGHT
    } else {
        CHART_HEIGHT
    };
    let precision = *precision;
    let format_value = move |v: f64| compact(v, precision);
    let bucket_label = |i: usize| window.labels.get(i).cloned().unwrap_or_default();
    let charts = Charts {
        palette,
        narrow,
        height,
        bucket_label: &bucket_label,
        format_value: &format_value,
    };

    let mut action = None;
    egui::ScrollArea::vertical()
        .id_salt("cashflow_trends")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.label(
                RichText::new(home.as_str())
                    .small()
                    .color(palette.text_muted),
            );
            ui.add_space(4.0);
            monthly_ui(ui, &charts, monthly);
            ui.add_space(28.0);
            action = trends_ui(ui, &charts, compare);
            ui.add_space(28.0);
            if net_worth_ui(ui, &charts, net_worth, *home_only) {
                action = Some(Action::ToggleHomeOnly);
            }
        });

    match action {
        Some(Action::Pick(name)) => {
            compare.pick(name);
            compare.search.clear();
        }
        Some(Action::Unpick(at)) => {
            compare.picked.remove(at);
        }
        Some(Action::ToggleHomeOnly) => {
            *home_only = !*home_only;
            *net_worth_stale = true;
        }
        None => {}
    }
}

/// A click this frame that changes what the charts show.
enum Action {
    /// Compare this account too.
    Pick(String),
    /// Stop comparing the pick at this index.
    Unpick(usize),
    /// Flip whether net worth leaves out other commodities.
    ToggleHomeOnly,
}

/// What every chart in the view shares.
struct Charts<'a> {
    palette: &'a Palette,
    /// Whether the view is laid out for a phone.
    narrow: bool,
    height: f32,
    bucket_label: &'a dyn Fn(usize) -> String,
    format_value: &'a dyn Fn(f64) -> String,
}

impl Charts<'_> {
    /// The chart style: lines in the palette's colors, values below zero in
    /// its expense color.
    fn style(&self) -> TimeSeriesStyle {
        TimeSeriesStyle {
            negative_color: Some(self.palette.expense),
            ..TimeSeriesStyle::default()
        }
    }

    /// `series` over the view's months.
    fn data<'s>(&'s self, series: &'s [Series<'s>]) -> TimeSeries<'s> {
        TimeSeries {
            series,
            bucket_label: self.bucket_label,
            format_value: self.format_value,
        }
    }

    /// The chart's size: the width it is given, the view's height.
    fn size(&self, ui: &egui::Ui) -> egui::Vec2 {
        egui::vec2(ui.available_width(), self.height)
    }
}

/// Income and expenses as bars per month, the net as a line over them.
fn monthly_ui(ui: &mut egui::Ui, charts: &Charts<'_>, monthly: &Monthly) {
    let palette = charts.palette;
    section_heading(ui, palette, "Income vs expenses");
    let figures = [
        (palette.income, "Income", &monthly.income_total),
        (palette.expense, "Expenses", &monthly.expenses_total),
        (palette.text, "Net", &monthly.net_total),
    ];
    if charts.narrow {
        // A row each, so no entry breaks between its name and its amount.
        for (color, name, total) in figures {
            ui.horizontal(|ui| figure(ui, palette, color, name, total));
        }
    } else {
        ui.horizontal(|ui| {
            for (color, name, total) in figures {
                figure(ui, palette, color, name, total);
            }
        });
    }
    ui.add_space(6.0);

    let lines = [
        (&monthly.income, palette.income),
        (&monthly.expenses, palette.expense),
        (&monthly.net, palette.text),
    ];
    let labels = ["Income", "Expenses", "Net"];
    let series = [0, 1, 2].map(|i| Series {
        label: labels[i],
        values: &lines[i].0.values,
        color: lines[i].1,
    });
    let data = charts.data(&series);
    let chart = bar_line_chart(
        ui,
        charts.size(ui),
        &data,
        2,
        &charts.style(),
        |ui, hover| tooltip(ui, palette, hover, &lines.map(|(line, _)| line), None),
    );
    label_chart(&chart.response, "Income vs expenses chart");
    unconverted_note(ui, palette, &monthly.unconverted);
}

/// The picker, a chip per account picked, and their lines. Returns a pick
/// or an unpick clicked this frame.
fn trends_ui(ui: &mut egui::Ui, charts: &Charts<'_>, compare: &mut Compare) -> Option<Action> {
    let palette = charts.palette;
    section_heading(ui, palette, "Account trends");
    let mut action = picker(ui, palette, compare);

    ui.horizontal_wrapped(|ui| {
        for (at, pick) in compare.picked.iter().enumerate() {
            let total = pick
                .trend
                .as_ref()
                .map_or("not in these books", |t| &t.total);
            if chip(ui, palette, palette.series[pick.slot], &pick.name, total) {
                action = Some(Action::Unpick(at));
            }
        }
    });
    ui.add_space(6.0);

    if compare.picked.is_empty() {
        muted(
            ui,
            palette,
            "Search for an account above to chart its monthly flow. Pick several to compare them.",
        );
        return action;
    }

    // A fixed array of every slot, so a frame builds no list of series.
    let empty = Series {
        label: "",
        values: &[],
        color: Color32::TRANSPARENT,
    };
    let mut series = [empty; SERIES];
    let mut lines: [Option<&Line>; SERIES] = [None; SERIES];
    let mut count = 0;
    for pick in &compare.picked {
        let Some(trend) = &pick.trend else { continue };
        series[count] = Series {
            label: &pick.name,
            values: &trend.line.values,
            color: palette.series[pick.slot],
        };
        lines[count] = Some(&trend.line);
        count += 1;
    }
    let data = charts.data(&series[..count]);
    let chart = line_chart(ui, charts.size(ui), &data, &charts.style(), |ui, hover| {
        tooltip_rows(ui, palette, hover, |i| lines.get(i).copied().flatten());
    });
    label_chart(&chart.response, "Account trends chart");
    action
}

/// Net worth at each month's end, with the chip that leaves out other
/// commodities. Returns whether the chip was clicked.
fn net_worth_ui(
    ui: &mut egui::Ui,
    charts: &Charts<'_>,
    net_worth: &NetWorth,
    home_only: bool,
) -> bool {
    let palette = charts.palette;
    let mut toggled = false;
    ui.horizontal_wrapped(|ui| {
        section_heading(ui, palette, "Net worth");
        ui.add_space(8.0);
        ui.label(
            RichText::new(&net_worth.latest)
                .size(18.0)
                .color(palette.text),
        );
        if !net_worth.others.is_empty() {
            ui.add_space(8.0);
            let text = RichText::new("Home currency only").color(if home_only {
                palette.text
            } else {
                palette.text_muted
            });
            toggled = ui
                .add(egui::Button::selectable(home_only, text))
                .on_hover_text("Leave out holdings in other commodities, such as on-chain BTC.")
                .clicked();
        }
    });
    ui.add_space(6.0);

    let series = [Series {
        label: "Net worth",
        values: &net_worth.line.values,
        color: palette.accent,
    }];
    let data = charts.data(&series);
    let note = price_note(net_worth, home_only);
    let chart = area_chart(ui, charts.size(ui), &data, &charts.style(), |ui, hover| {
        tooltip(ui, palette, hover, &[&net_worth.line], note)
    });
    label_chart(&chart.response, "Net worth chart");
    if let Some(note) = note {
        ui.add_space(4.0);
        ui.label(RichText::new(note).small().color(palette.text_muted));
    }
    unconverted_note(ui, palette, &net_worth.unconverted);
    toggled
}

/// What the net worth line says about prices: that it moves with them,
/// when it holds another commodity and is not leaving it out.
fn price_note(net_worth: &NetWorth, home_only: bool) -> Option<&'static str> {
    if net_worth.others.is_empty() || home_only {
        return None;
    }
    Some(
        "Holdings in other commodities are valued at each month-end's price, so the line moves with their price too.",
    )
}

/// A chart's tooltip: the month, each series' swatch, name and value, and
/// an optional note under them.
fn tooltip(
    ui: &mut egui::Ui,
    palette: &Palette,
    hover: &HoverPoint<'_>,
    lines: &[&Line],
    note: Option<&str>,
) {
    tooltip_rows(ui, palette, hover, |i| lines.get(i).copied());
    if let Some(note) = note {
        ui.add_space(4.0);
        ui.set_max_width(260.0);
        ui.label(RichText::new(note).small().color(palette.text_muted));
    }
}

/// The month, then a row per series: its swatch, name and the value's text
/// from `line(i)`, the line series `i` was drawn from.
fn tooltip_rows<'l>(
    ui: &mut egui::Ui,
    palette: &Palette,
    hover: &HoverPoint<'_>,
    line: impl Fn(usize) -> Option<&'l Line>,
) {
    let bucket = hover.bucket;
    ui.label(
        RichText::new((hover.data.bucket_label)(bucket))
            .strong()
            .color(palette.text),
    );
    for (i, series) in hover.data.series.iter().enumerate() {
        let Some(text) = line(i).and_then(|l| l.texts.get(bucket)) else {
            continue;
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new("■").color(series.color));
            ui.label(RichText::new(series.label).color(palette.text_muted));
            ui.label(RichText::new(text).color(palette.text));
        });
    }
}

/// The chips choosing how far back the charts look.
fn span_picker(ui: &mut egui::Ui, palette: &Palette, span: &mut Span) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for choice in Span::ALL {
            let selected = choice == *span;
            let color = if selected {
                palette.text
            } else {
                palette.text_muted
            };
            let chip =
                egui::Button::selectable(selected, RichText::new(choice.label()).color(color));
            if ui.add(chip).clicked() {
                *span = choice;
            }
        }
    });
}

/// The account search field and, while something is typed, the accounts
/// it matches. Returns the account clicked.
fn picker(ui: &mut egui::Ui, palette: &Palette, compare: &mut Compare) -> Option<Action> {
    let full = compare.picked.len() >= SERIES;
    let hint = if full {
        "Remove an account to add another"
    } else {
        "Add an account…"
    };
    ui.add_enabled(
        !full,
        egui::TextEdit::singleline(&mut compare.search)
            .hint_text(hint)
            .desired_width(280.0),
    );
    let search = compare.search.trim();
    if full || search.is_empty() {
        return None;
    }

    let mut picked = None;
    let mut any = false;
    let matches = compare
        .accounts
        .iter()
        .filter(|name| contains_ignore_ascii_case(name, search))
        .filter(|name| compare.picked.iter().all(|p| &p.name != *name))
        .take(MATCHES);
    for name in matches {
        any = true;
        let row = egui::Button::new(RichText::new(name).color(palette.text)).frame(false);
        if ui.add(row).clicked() {
            picked = Some(Action::Pick(name.clone()));
        }
    }
    if !any {
        muted(ui, palette, "No account matches.");
    }
    picked
}

/// Whether `haystack` contains `needle`, ignoring ASCII case, without
/// lowercasing either into a new string.
fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    let (haystack, needle) = (haystack.as_bytes(), needle.as_bytes());
    needle.is_empty()
        || haystack
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
}

/// A picked account: its swatch, name and total, and a button that removes
/// it. Returns whether that was clicked.
fn chip(ui: &mut egui::Ui, palette: &Palette, color: Color32, name: &str, total: &str) -> bool {
    let mut removed = false;
    egui::Frame::new()
        .fill(palette.surface)
        .corner_radius(12.0)
        .inner_margin(egui::Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(RichText::new("■").color(color));
                ui.label(RichText::new(name).color(palette.text));
                ui.label(RichText::new(total).small().color(palette.text_muted));
                let close =
                    egui::Button::new(RichText::new("×").color(palette.text_muted)).frame(false);
                removed = ui.add(close).on_hover_text("Remove").clicked();
            });
        });
    removed
}

/// Name a chart for screen readers (and the tests that find it).
fn label_chart(response: &egui::Response, name: &'static str) {
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, name));
}

/// A section's heading.
fn section_heading(ui: &mut egui::Ui, palette: &Palette, title: &'static str) {
    ui.label(RichText::new(title).size(18.0).strong().color(palette.text));
}

/// A legend entry with its total: swatch, name, amount.
fn figure(ui: &mut egui::Ui, palette: &Palette, color: Color32, name: &str, total: &str) {
    ui.label(RichText::new("■").color(color));
    ui.label(RichText::new(name).color(palette.text_muted));
    ui.label(RichText::new(total).color(palette.text));
    ui.add_space(12.0);
}

/// A note under a chart about amounts left out, when there are any.
fn unconverted_note(ui: &mut egui::Ui, palette: &Palette, note: &Option<String>) {
    let Some(note) = note else { return };
    ui.add_space(4.0);
    ui.label(RichText::new(note).small().color(palette.warning));
}

/// A line of muted text: an empty state or a hint.
fn muted(ui: &mut egui::Ui, palette: &Palette, text: &'static str) {
    ui.label(RichText::new(text).color(palette.text_muted));
}
