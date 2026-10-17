//! Time-series line and area charts.
//!
//! The x axis is a run of date buckets (bucket `0` is the oldest), the y axis
//! is an amount. Any number of [`Series`] share one zero baseline and one set
//! of nicely rounded y ticks. Values below zero are drawn below the baseline
//! in the negative color (the theme's `destructive` unless the style says
//! otherwise). Hovering the plot draws a vertical crosshair and a tooltip of
//! every series' value at the hovered bucket; the tooltip's contents come from
//! a caller-supplied closure, so a view can restyle it.
//!
//! Every input is borrowed: the series are slices the caller already holds,
//! so a chart adds nothing of its own to a frame beyond the shapes egui needs
//! to paint it (one line per same-sign run, one mesh per filled series) and
//! the axis label strings.

use egui::epaint::Mesh;
use egui::{
    pos2, Align2, Color32, FontId, Frame, Order, Pos2, Rect, Response, RichText, Sense, Shape,
    Stroke, Ui, Vec2,
};

/// One named line of a [`TimeSeries`].
#[derive(Clone, Copy)]
pub struct Series<'a> {
    /// Shown in the tooltip beside the value.
    pub label: &'a str,
    /// One finite value per bucket, oldest first. A series shorter than the
    /// chart's longest simply ends early.
    pub values: &'a [f64],
    /// Line and fill color for values at or above zero.
    pub color: Color32,
}

/// Everything a time-series chart draws, borrowed from the caller.
#[derive(Clone, Copy)]
pub struct TimeSeries<'a> {
    /// The series, drawn in order (later ones on top).
    pub series: &'a [Series<'a>],
    /// Names bucket `i`, for the x axis and the tooltip header.
    pub bucket_label: &'a dyn Fn(usize) -> String,
    /// Formats an amount, for the y axis and the tooltip.
    pub format_value: &'a dyn Fn(f64) -> String,
}

impl TimeSeries<'_> {
    /// Number of x buckets: the length of the longest series.
    pub fn buckets(&self) -> usize {
        self.series
            .iter()
            .map(|s| s.values.len())
            .max()
            .unwrap_or(0)
    }

    /// The smallest and largest value across every series, widened to
    /// include zero so the baseline is always on the chart.
    fn value_span(&self) -> ValueSpan {
        let mut span = ValueSpan { min: 0.0, max: 0.0 };
        for &v in self.series.iter().flat_map(|s| s.values) {
            span.min = span.min.min(v);
            span.max = span.max.max(v);
        }
        span
    }
}

/// A closed range of values.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ValueSpan {
    min: f64,
    max: f64,
}

/// Styling for [`line_chart`] and [`area_chart`].
#[derive(Clone, Copy)]
pub struct TimeSeriesStyle {
    /// Width of each series' line.
    pub stroke_width: f32,
    /// Opacity, `0..=1`, of the area under a series in an [`area_chart`].
    pub fill_opacity: f32,
    /// Roughly how many y-axis intervals to draw; the real count follows the
    /// nice step [`axis_ticks`] picks.
    pub y_ticks: usize,
    /// Minimum horizontal distance between two x-axis labels; buckets in
    /// between go unlabelled.
    pub min_x_label_spacing: f32,
    /// Color for values below zero. `None` takes the theme's `destructive`.
    pub negative_color: Option<Color32>,
}

impl Default for TimeSeriesStyle {
    fn default() -> Self {
        Self {
            stroke_width: notedeck::tokens::STROKE_THICK,
            fill_opacity: 0.25,
            y_ticks: 4,
            min_x_label_spacing: 64.0,
            negative_color: None,
        }
    }
}

/// The bucket under the pointer, handed to the tooltip closure.
pub struct HoverPoint<'a> {
    /// Index of the hovered bucket.
    pub bucket: usize,
    /// The chart's data, for its labels, formatter and values.
    pub data: &'a TimeSeries<'a>,
}

/// One series' value at a [`HoverPoint`].
pub struct HoverValue<'a> {
    /// The series this value belongs to.
    pub series: &'a Series<'a>,
    /// Its value at the hovered bucket, `None` when the series ended earlier.
    pub value: Option<f64>,
}

impl<'a> HoverPoint<'a> {
    /// Each series' value at the hovered bucket, in series order.
    pub fn values(&self) -> impl Iterator<Item = HoverValue<'a>> + 'a {
        let bucket = self.bucket;
        self.data.series.iter().map(move |series| HoverValue {
            series,
            value: series.values.get(bucket).copied(),
        })
    }
}

/// What a time-series chart reports back.
pub struct TimeSeriesResponse {
    /// The response for the whole chart, axes included.
    pub response: Response,
    /// The area inside the axes where the series are drawn.
    pub plot_rect: Rect,
    /// The bucket under the pointer, if any.
    pub hovered: Option<usize>,
    /// Where the tooltip was shown this frame, if it was.
    pub tooltip_rect: Option<Rect>,
}

/// Y-axis ticks: every multiple of `step` from `lo` to `hi`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AxisTicks {
    /// The lowest tick, at or below the data.
    pub lo: f64,
    /// The highest tick, at or above the data.
    pub hi: f64,
    /// Distance between ticks: 1, 2 or 5 times a power of ten.
    pub step: f64,
}

impl AxisTicks {
    /// Each tick value from `lo` to `hi`, computed from its index so floating
    /// point error doesn't accumulate across ticks.
    pub fn values(&self) -> impl Iterator<Item = f64> {
        let AxisTicks { lo, step, .. } = *self;
        let count = ((self.hi - self.lo) / step).round() as usize;
        // `+ 0.0` turns a `-0.0` tick into `0.0`, so it never prints as "-0".
        (0..=count).map(move |k| lo + k as f64 * step + 0.0)
    }
}

/// Nicely rounded ticks covering `min..=max` in about `target` intervals.
///
/// An empty or degenerate range (`min == max`) gets a unit range above it.
pub fn axis_ticks(min: f64, max: f64, target: usize) -> AxisTicks {
    let (min, max) = if max > min {
        (min, max)
    } else {
        (min, min + 1.0)
    };
    let step = nice_step((max - min) / target.max(1) as f64);
    AxisTicks {
        lo: (min / step).floor() * step,
        hi: (max / step).ceil() * step,
        step,
    }
}

/// The smallest of 1, 2, 5 or 10 times a power of ten that is at least `raw`.
fn nice_step(raw: f64) -> f64 {
    if !raw.is_finite() || raw <= 0.0 {
        return 1.0;
    }
    let magnitude = 10f64.powf(raw.log10().floor());
    let normalized = raw / magnitude;
    let nice = if normalized <= 1.0 {
        1.0
    } else if normalized <= 2.0 {
        2.0
    } else if normalized <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * magnitude
}

/// The default tooltip: the bucket's label, then one swatch, label and
/// formatted value per series that has a value there.
pub fn default_tooltip(ui: &mut Ui, hover: &HoverPoint<'_>) {
    ui.label(RichText::new((hover.data.bucket_label)(hover.bucket)).strong());
    for point in hover.values() {
        let Some(value) = point.value else {
            continue;
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new("■").color(point.series.color));
            ui.label(point.series.label);
            ui.label((hover.data.format_value)(value));
        });
    }
}

/// Draws `data` as lines, one per series, filling `size`.
///
/// `tooltip` renders the hover tooltip's contents; pass [`default_tooltip`]
/// for the plain one.
pub fn line_chart(
    ui: &mut Ui,
    size: Vec2,
    data: &TimeSeries<'_>,
    style: &TimeSeriesStyle,
    tooltip: impl FnOnce(&mut Ui, &HoverPoint<'_>),
) -> TimeSeriesResponse {
    time_series_chart(ui, size, data, style, SeriesFill::Line, tooltip)
}

/// Draws `data` as lines with the area between each line and the zero
/// baseline filled, filling `size`.
///
/// `tooltip` renders the hover tooltip's contents; pass [`default_tooltip`]
/// for the plain one.
pub fn area_chart(
    ui: &mut Ui,
    size: Vec2,
    data: &TimeSeries<'_>,
    style: &TimeSeriesStyle,
    tooltip: impl FnOnce(&mut Ui, &HoverPoint<'_>),
) -> TimeSeriesResponse {
    time_series_chart(ui, size, data, style, SeriesFill::Area, tooltip)
}

/// Whether the area under each series is filled.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SeriesFill {
    Line,
    Area,
}

/// Font of the axis labels.
fn axis_font() -> FontId {
    FontId::proportional(11.0)
}

#[profiling::function]
fn time_series_chart(
    ui: &mut Ui,
    size: Vec2,
    data: &TimeSeries<'_>,
    style: &TimeSeriesStyle,
    fill: SeriesFill,
    tooltip: impl FnOnce(&mut Ui, &HoverPoint<'_>),
) -> TimeSeriesResponse {
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(
        rect,
        notedeck::tokens::RADIUS_MD,
        ui.visuals().faint_bg_color,
    );

    let buckets = data.buckets();
    if buckets == 0 {
        return TimeSeriesResponse {
            response,
            plot_rect: rect,
            hovered: None,
            tooltip_rect: None,
        };
    }

    let span = data.value_span();
    let ticks = axis_ticks(span.min, span.max, style.y_ticks);
    let label_color = ui.visuals().weak_text_color();
    let negative = style
        .negative_color
        .unwrap_or_else(|| notedeck::ColorTheme::current(ui.ctx()).destructive);

    let y_label_width = ticks
        .values()
        .map(|t| {
            painter
                .layout_no_wrap((data.format_value)(t), axis_font(), label_color)
                .size()
                .x
        })
        .fold(0.0, f32::max);
    let font_height = ui.fonts_mut(|f| f.row_height(&axis_font()));
    let pad = notedeck::tokens::SPACING_SM;

    let plot = Plot {
        rect: Rect::from_min_max(
            pos2(rect.left() + pad + y_label_width + pad, rect.top() + pad),
            pos2(
                rect.right() - pad,
                rect.bottom() - pad - font_height - notedeck::tokens::SPACING_XS,
            ),
        ),
        buckets,
        ticks,
    };

    paint_y_axis(&painter, &plot, data, label_color, ui.visuals());
    paint_x_axis(&painter, &plot, data, style, label_color, rect);

    for series in data.series {
        if fill == SeriesFill::Area {
            paint_area(&painter, &plot, series, style, negative);
        }
        paint_line(&painter, &plot, series, style, negative);
    }

    let hovered = response
        .hover_pos()
        .filter(|pos| pos.x >= plot.rect.left() - pad && pos.x <= plot.rect.right() + pad)
        .map(|pos| plot.bucket_at(pos.x));

    let tooltip_rect = hovered.map(|bucket| {
        paint_crosshair(&painter, &plot, data, bucket, style, negative, ui.visuals());
        show_tooltip(ui, &response, &plot, data, bucket, tooltip)
    });

    TimeSeriesResponse {
        response,
        plot_rect: plot.rect,
        hovered,
        tooltip_rect,
    }
}

/// Maps buckets and values into the plot's screen space.
struct Plot {
    rect: Rect,
    buckets: usize,
    ticks: AxisTicks,
}

impl Plot {
    /// Screen x of bucket `i`. A single bucket sits in the middle.
    fn x(&self, i: usize) -> f32 {
        if self.buckets < 2 {
            return self.rect.center().x;
        }
        self.rect.left() + i as f32 * self.bucket_width()
    }

    /// Horizontal distance between two neighbouring buckets.
    fn bucket_width(&self) -> f32 {
        self.rect.width() / (self.buckets.saturating_sub(1) as f32).max(1.0)
    }

    /// Screen y of value `v`.
    fn y(&self, v: f64) -> f32 {
        let t = (v - self.ticks.lo) / (self.ticks.hi - self.ticks.lo);
        self.rect.bottom() - t as f32 * self.rect.height()
    }

    /// Screen position of value `v` at bucket `i`.
    fn point(&self, i: usize, v: f64) -> Pos2 {
        pos2(self.x(i), self.y(v))
    }

    /// The bucket nearest screen x.
    fn bucket_at(&self, x: f32) -> usize {
        if self.buckets < 2 {
            return 0;
        }
        let i = ((x - self.rect.left()) / self.bucket_width()).round();
        (i.max(0.0) as usize).min(self.buckets - 1)
    }

    /// Where the segment from `a` at bucket `i` to `b` at bucket `i + 1`
    /// crosses zero. Only meaningful when `a` and `b` differ in sign.
    fn zero_crossing(&self, i: usize, a: f64, b: f64) -> Pos2 {
        let t = (a / (a - b)) as f32;
        let x0 = self.x(i);
        pos2(x0 + t * (self.x(i + 1) - x0), self.y(0.0))
    }
}

/// Gridlines and labels at each y tick, and a stronger zero baseline.
fn paint_y_axis(
    painter: &egui::Painter,
    plot: &Plot,
    data: &TimeSeries<'_>,
    label_color: Color32,
    visuals: &egui::Visuals,
) {
    let grid = Stroke::new(
        notedeck::tokens::STROKE_THIN,
        visuals.widgets.noninteractive.bg_stroke.color,
    );
    for tick in plot.ticks.values() {
        let y = plot.y(tick);
        painter.hline(plot.rect.x_range(), y, grid);
        painter.text(
            pos2(plot.rect.left() - notedeck::tokens::SPACING_SM, y),
            Align2::RIGHT_CENTER,
            (data.format_value)(tick),
            axis_font(),
            label_color,
        );
    }

    let baseline = Stroke::new(notedeck::tokens::STROKE_THIN, label_color);
    painter.hline(plot.rect.x_range(), plot.y(0.0), baseline);
}

/// Bucket labels under the plot, skipping buckets so labels stay at least
/// `style.min_x_label_spacing` apart and inside the chart.
fn paint_x_axis(
    painter: &egui::Painter,
    plot: &Plot,
    data: &TimeSeries<'_>,
    style: &TimeSeriesStyle,
    label_color: Color32,
    chart: Rect,
) {
    let every = (style.min_x_label_spacing / plot.bucket_width())
        .ceil()
        .max(1.0) as usize;
    let y = plot.rect.bottom() + notedeck::tokens::SPACING_XS;
    for i in (0..plot.buckets).step_by(every) {
        let galley = painter.layout_no_wrap((data.bucket_label)(i), axis_font(), label_color);
        let half = galley.size().x / 2.0;
        let x = plot.x(i).clamp(
            chart.left() + half,
            (chart.right() - half).max(chart.left() + half),
        );
        painter.galley(pos2(x - half, y), galley, label_color);
    }
}

/// One series as a polyline, split where it crosses zero so the part below
/// the baseline takes the negative color.
fn paint_line(
    painter: &egui::Painter,
    plot: &Plot,
    series: &Series<'_>,
    style: &TimeSeriesStyle,
    negative: Color32,
) {
    let color_for = |below: bool| if below { negative } else { series.color };
    let values = series.values;

    if let [only] = values {
        painter.circle_filled(
            plot.point(0, *only),
            style.stroke_width,
            color_for(*only < 0.0),
        );
        return;
    }

    let mut run: Vec<Pos2> = Vec::with_capacity(values.len() + 1);
    let mut run_below = false;
    for (i, &v) in values.iter().enumerate() {
        let below = v < 0.0;
        if i > 0 && below != run_below {
            let crossing = plot.zero_crossing(i - 1, values[i - 1], v);
            run.push(crossing);
            let done = std::mem::replace(&mut run, Vec::with_capacity(values.len() - i + 2));
            painter.add(Shape::line(
                done,
                Stroke::new(style.stroke_width, color_for(run_below)),
            ));
            run.push(crossing);
        }
        run_below = below;
        run.push(plot.point(i, v));
    }
    painter.add(Shape::line(
        run,
        Stroke::new(style.stroke_width, color_for(run_below)),
    ));
}

/// The area between one series and the zero baseline, as one mesh: a quad
/// per segment, or two triangles where the segment crosses zero so each side
/// takes its own color.
fn paint_area(
    painter: &egui::Painter,
    plot: &Plot,
    series: &Series<'_>,
    style: &TimeSeriesStyle,
    negative: Color32,
) {
    let values = series.values;
    if values.len() < 2 {
        return;
    }

    let above = series.color.gamma_multiply(style.fill_opacity);
    let below = negative.gamma_multiply(style.fill_opacity);
    let base_y = plot.y(0.0);
    let segments = values.len() - 1;

    let mut mesh = Mesh::default();
    mesh.reserve_vertices(segments * 6);
    mesh.reserve_triangles(segments * 2);

    for (i, pair) in values.windows(2).enumerate() {
        let [a, b] = [pair[0], pair[1]];
        let pa = plot.point(i, a);
        let pb = plot.point(i + 1, b);
        let ba = pos2(pa.x, base_y);
        let bb = pos2(pb.x, base_y);

        if (a < 0.0) == (b < 0.0) || a == 0.0 || b == 0.0 {
            let color = if a < 0.0 || b < 0.0 { below } else { above };
            let first = mesh.vertices.len() as u32;
            for p in [pa, pb, bb, ba] {
                mesh.colored_vertex(p, color);
            }
            mesh.add_triangle(first, first + 1, first + 2);
            mesh.add_triangle(first, first + 2, first + 3);
            continue;
        }

        let crossing = plot.zero_crossing(i, a, b);
        let [color_a, color_b] = if a < 0.0 {
            [below, above]
        } else {
            [above, below]
        };
        let first = mesh.vertices.len() as u32;
        for p in [pa, crossing, ba] {
            mesh.colored_vertex(p, color_a);
        }
        for p in [crossing, pb, bb] {
            mesh.colored_vertex(p, color_b);
        }
        mesh.add_triangle(first, first + 1, first + 2);
        mesh.add_triangle(first + 3, first + 4, first + 5);
    }

    painter.add(Shape::mesh(mesh));
}

/// The vertical hover line at `bucket`, and a dot on each series there.
fn paint_crosshair(
    painter: &egui::Painter,
    plot: &Plot,
    data: &TimeSeries<'_>,
    bucket: usize,
    style: &TimeSeriesStyle,
    negative: Color32,
    visuals: &egui::Visuals,
) {
    let x = plot.x(bucket);
    painter.vline(
        x,
        plot.rect.y_range(),
        Stroke::new(notedeck::tokens::STROKE_THIN, visuals.text_color()),
    );
    for series in data.series {
        let Some(&v) = series.values.get(bucket) else {
            continue;
        };
        let color = if v < 0.0 { negative } else { series.color };
        painter.circle(
            plot.point(bucket, v),
            style.stroke_width + 2.0,
            color,
            Stroke::new(notedeck::tokens::STROKE_THIN, visuals.extreme_bg_color),
        );
    }
}

/// Shows the tooltip beside the crosshair at `bucket`: to its right while the
/// crosshair is in the plot's left half, to its left otherwise, so it never
/// covers the bucket it describes. Returns where it was drawn.
fn show_tooltip(
    ui: &Ui,
    response: &Response,
    plot: &Plot,
    data: &TimeSeries<'_>,
    bucket: usize,
    tooltip: impl FnOnce(&mut Ui, &HoverPoint<'_>),
) -> Rect {
    let x = plot.x(bucket);
    let gap = notedeck::tokens::SPACING_SM;
    let (pivot, anchor_x) = if x > plot.rect.center().x {
        (Align2::RIGHT_TOP, x - gap)
    } else {
        (Align2::LEFT_TOP, x + gap)
    };

    egui::Area::new(response.id.with("time_series_tooltip"))
        .order(Order::Tooltip)
        .interactable(false)
        .pivot(pivot)
        .fixed_pos(pos2(anchor_x, plot.rect.top()))
        .show(ui.ctx(), |ui| {
            Frame::popup(ui.style()).show(ui, |ui| {
                tooltip(ui, &HoverPoint { bucket, data });
            });
        })
        .response
        .rect
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_steps_are_one_two_or_five_times_a_power_of_ten() {
        assert_eq!(nice_step(0.7), 1.0);
        assert_eq!(nice_step(1.3), 2.0);
        assert_eq!(nice_step(3.0), 5.0);
        assert_eq!(nice_step(7.0), 10.0);
        assert_eq!(nice_step(230.0), 500.0);
        assert_eq!(nice_step(0.0), 1.0);
    }

    #[test]
    fn ticks_cover_the_data_and_include_zero_when_it_spans_it() {
        let ticks = axis_ticks(-1_200.0, 4_800.0, 4);
        assert_eq!(
            ticks,
            AxisTicks {
                lo: -2_000.0,
                hi: 6_000.0,
                step: 2_000.0
            }
        );
        let values: Vec<f64> = ticks.values().collect();
        assert_eq!(values, [-2_000.0, 0.0, 2_000.0, 4_000.0, 6_000.0]);
        assert!(values.iter().all(|v| v.is_sign_positive() || *v < 0.0));
    }

    #[test]
    fn a_flat_range_still_gets_ticks() {
        let ticks = axis_ticks(0.0, 0.0, 4);
        assert!(ticks.hi > ticks.lo);
        assert!(ticks.values().count() >= 2);
    }

    fn plot(buckets: usize) -> Plot {
        Plot {
            rect: Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0)),
            buckets,
            ticks: AxisTicks {
                lo: -10.0,
                hi: 10.0,
                step: 5.0,
            },
        }
    }

    #[test]
    fn buckets_map_to_evenly_spaced_x_and_back() {
        let p = plot(5);
        assert_eq!(p.x(0), 0.0);
        assert_eq!(p.x(4), 100.0);
        assert_eq!(p.bucket_at(26.0), 1);
        assert_eq!(p.bucket_at(-50.0), 0);
        assert_eq!(p.bucket_at(500.0), 4);
        assert_eq!(plot(1).x(0), 50.0);
    }

    #[test]
    fn zero_sits_mid_plot_and_crossings_land_on_it() {
        let p = plot(2);
        assert_eq!(p.y(0.0), 50.0);
        let c = p.zero_crossing(0, 5.0, -5.0);
        assert_eq!(c, pos2(50.0, 50.0));
    }
}
