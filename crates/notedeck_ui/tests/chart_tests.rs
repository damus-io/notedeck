//! Time-series chart tests: snapshots of line, area and negative series, and
//! where the hover tooltip lands relative to the crosshair.

use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use notedeck_ui::charts::{
    area_chart, default_tooltip, line_chart, Series, TimeSeries, TimeSeriesResponse,
    TimeSeriesStyle,
};

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

const INCOME: [f64; 12] = [
    5200.0, 5100.0, 6400.0, 5900.0, 7200.0, 6800.0, 7100.0, 7600.0, 6900.0, 8200.0, 7800.0, 9100.0,
];

const EXPENSES: [f64; 12] = [
    4100.0, 4600.0, 4300.0, 5200.0, 4800.0, 6100.0, 5300.0, 5000.0, 5600.0, 5400.0, 6200.0, 7000.0,
];

/// Income minus expenses some months, but dipping below zero in others.
const NET: [f64; 12] = [
    1100.0, -900.0, 2100.0, -1300.0, 2400.0, 700.0, -400.0, 2600.0, 1300.0, -2200.0, 1600.0, 2100.0,
];

/// Which chart a test draws.
#[derive(Clone, Copy)]
enum Kind {
    Line,
    Area,
}

/// Harness state: what to draw, and the chart's last response.
struct ChartState {
    kind: Kind,
    series: Vec<Series<'static>>,
    last: Option<TimeSeriesResponse>,
}

fn month(i: usize) -> String {
    MONTHS[i % MONTHS.len()].to_owned()
}

fn dollars(v: f64) -> String {
    if v < 0.0 {
        format!("-${:.0}", -v)
    } else {
        format!("${v:.0}")
    }
}

fn draw(ui: &mut egui::Ui, state: &mut ChartState) {
    ui.ctx().set_visuals(notedeck::theme::dark_mode(false));
    let data = TimeSeries {
        series: &state.series,
        bucket_label: &month,
        format_value: &dollars,
    };
    let style = TimeSeriesStyle {
        negative_color: Some(egui::Color32::from_rgb(0xE5, 0x48, 0x4D)),
        ..TimeSeriesStyle::default()
    };
    let size = ui.available_size();
    let resp = match state.kind {
        Kind::Line => line_chart(ui, size, &data, &style, default_tooltip),
        Kind::Area => area_chart(ui, size, &data, &style, default_tooltip),
    };
    state.last = Some(resp);
}

fn income_vs_expenses() -> Vec<Series<'static>> {
    vec![
        Series {
            label: "Income",
            values: &INCOME,
            color: egui::Color32::from_rgb(0x46, 0xA7, 0x58),
        },
        Series {
            label: "Expenses",
            values: &EXPENSES,
            color: egui::Color32::from_rgb(0x3E, 0x63, 0xDD),
        },
    ]
}

fn net() -> Vec<Series<'static>> {
    vec![Series {
        label: "Net",
        values: &NET,
        color: egui::Color32::from_rgb(0x46, 0xA7, 0x58),
    }]
}

fn state(kind: Kind, series: Vec<Series<'static>>) -> ChartState {
    ChartState {
        kind,
        series,
        last: None,
    }
}

/// A harness that lays out and hit-tests but never rasterizes.
fn harness(kind: Kind, series: Vec<Series<'static>>) -> Harness<'static, ChartState> {
    Harness::builder()
        .with_size(egui::vec2(640.0, 320.0))
        .build_ui_state(draw, state(kind, series))
}

/// A harness that renders through lavapipe, for snapshots.
fn render_harness(kind: Kind, series: Vec<Series<'static>>) -> Harness<'static, ChartState> {
    Harness::builder()
        .with_size(egui::vec2(640.0, 320.0))
        .renderer(notedeck::software_renderer())
        .build_ui_state(draw, state(kind, series))
}

/// Hovers bucket `bucket` and returns the chart's response once the tooltip
/// has settled (an egui Area spends its first frame measuring itself).
fn hover_bucket(harness: &mut Harness<'static, ChartState>, bucket: usize) -> TimeSeriesResponse {
    harness.run();
    let plot = harness.state().last.as_ref().unwrap().plot_rect;
    let x = plot.left() + plot.width() * bucket as f32 / (MONTHS.len() - 1) as f32;
    harness.hover_at(egui::pos2(x, plot.center().y));
    harness.run_steps(3);
    harness.state_mut().last.take().unwrap()
}

#[test]
fn hover_reports_the_bucket_under_the_pointer() {
    let mut harness = harness(Kind::Line, income_vs_expenses());
    let resp = hover_bucket(&mut harness, 4);
    assert_eq!(resp.hovered, Some(4));
    assert!(resp.tooltip_rect.is_some());

    // The default tooltip names the bucket and every series.
    harness.get_by_label("May");
    harness.get_by_label("Income");
    harness.get_by_label("$7200");
    harness.get_by_label("Expenses");
}

#[test]
fn tooltip_sits_right_of_a_crosshair_in_the_left_half() {
    let mut harness = harness(Kind::Line, income_vs_expenses());
    let resp = hover_bucket(&mut harness, 2);
    let plot = resp.plot_rect;
    let crosshair_x = plot.left() + plot.width() * 2.0 / 11.0;
    let tip = resp.tooltip_rect.expect("tooltip shown");
    assert!(
        tip.left() > crosshair_x,
        "tooltip {tip:?} should start right of the crosshair at x={crosshair_x}"
    );
    assert!(tip.top() >= plot.top() - 1.0);
}

#[test]
fn tooltip_flips_left_of_a_crosshair_in_the_right_half() {
    let mut harness = harness(Kind::Line, income_vs_expenses());
    let resp = hover_bucket(&mut harness, 10);
    let plot = resp.plot_rect;
    let crosshair_x = plot.left() + plot.width() * 10.0 / 11.0;
    let tip = resp.tooltip_rect.expect("tooltip shown");
    assert!(
        tip.right() < crosshair_x,
        "tooltip {tip:?} should end left of the crosshair at x={crosshair_x}"
    );
}

#[test]
fn no_hover_no_tooltip() {
    let mut harness = harness(Kind::Area, net());
    harness.run();
    let resp = harness.state_mut().last.take().unwrap();
    assert_eq!(resp.hovered, None);
    assert_eq!(resp.tooltip_rect, None);
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_line_chart() {
    let mut harness = render_harness(Kind::Line, income_vs_expenses());
    harness.run();
    harness.snapshot("time_series_line");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_area_chart() {
    let mut harness = render_harness(Kind::Area, income_vs_expenses());
    harness.run();
    harness.snapshot("time_series_area");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_negative_series() {
    let mut harness = render_harness(Kind::Area, net());
    harness.run();
    harness.snapshot("time_series_negative");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_hover_tooltip() {
    let mut harness = render_harness(Kind::Line, income_vs_expenses());
    hover_bucket(&mut harness, 8);
    harness.snapshot("time_series_hover");
}
