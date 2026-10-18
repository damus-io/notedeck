//! Time-series line, area and bar charts.
//!
//! The x axis is a run of date buckets (bucket `0` is the oldest), the y axis
//! is an amount. Any number of [`Series`] share one zero baseline and one set
//! of nicely rounded y ticks. Values below zero are drawn below the baseline
//! in the negative color (the theme's `destructive` unless the style says
//! otherwise). Hovering the plot draws a vertical crosshair and a tooltip of
//! every series' value at the hovered bucket; the tooltip's contents come from
//! a caller-supplied closure, so a view can restyle it.
//!
//! [`bar_line_chart`] draws some series as bars, side by side in each bucket,
//! and the rest as lines over them, on the same axis: a month's income and
//! expenses as bars with the net as a line, say. Each bucket there is a slot
//! with its bar group in the middle, and hovering shades the slot.
//!
//! [`line_chart_marked`] draws a line chart with [`Marks`] over it: dashed
//! [`Trail`]s that carry a solid series on past where it ends (a
//! projection), horizontal [`Level`]s (a target) and vertical [`Marker`]s (a
//! deadline). Marks widen the axes to fit, but the tooltip and crosshair
//! stay the series' own.
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

/// What [`line_chart_marked`] draws over its series, borrowed from the
/// caller like the series are.
#[derive(Clone, Copy, Default)]
pub struct Marks<'a> {
    /// Dashed lines, each starting partway along the x axis.
    pub trails: &'a [Trail<'a>],
    /// Horizontal lines at a value.
    pub levels: &'a [Level<'a>],
    /// Vertical lines at a bucket.
    pub markers: &'a [Marker<'a>],
}

impl Marks<'_> {
    /// No marks: what [`line_chart`] draws with.
    pub const NONE: Marks<'static> = Marks {
        trails: &[],
        levels: &[],
        markers: &[],
    };

    /// Number of x buckets the trails reach.
    fn buckets(&self) -> usize {
        self.trails
            .iter()
            .map(|t| t.start + t.values.len())
            .max()
            .unwrap_or(0)
    }

    /// `span` widened to every value the trails and levels draw.
    fn widen(&self, mut span: ValueSpan) -> ValueSpan {
        let trails = self
            .trails
            .iter()
            .flat_map(|t| t.join.iter().chain(t.values));
        let levels = self.levels.iter().map(|l| &l.value);
        for &v in trails.chain(levels) {
            span.min = span.min.min(v);
            span.max = span.max.max(v);
        }
        span
    }
}

/// A dashed line whose first value is at bucket [`Self::start`]: a
/// projection carrying on from where a solid series ends.
#[derive(Clone, Copy)]
pub struct Trail<'a> {
    /// The bucket of [`Self::values`]' first value.
    pub start: usize,
    /// Where the line starts from at [`Self::start`] before its first
    /// value, also at `start`: the solid series' last value, so the two
    /// join even when the trail's first value differs from it.
    pub join: Option<f64>,
    /// One finite value per bucket from [`Self::start`] on.
    pub values: &'a [f64],
    /// The line's color.
    pub color: Color32,
}

/// A dashed horizontal line across the plot at a value, labelled at its
/// right end: a target.
#[derive(Clone, Copy)]
pub struct Level<'a> {
    /// The value it is drawn at.
    pub value: f64,
    /// Drawn above the line at the plot's right edge.
    pub label: &'a str,
    /// The line's and label's color.
    pub color: Color32,
}

/// A vertical line at a bucket, labelled at its top: a deadline.
#[derive(Clone, Copy)]
pub struct Marker<'a> {
    /// The bucket it is drawn at.
    pub bucket: usize,
    /// Drawn beside the line's top.
    pub label: &'a str,
    /// The line's and label's color.
    pub color: Color32,
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
    /// The panel the hover tooltip is drawn in. `None` takes egui's popup
    /// frame; a themed tooltip (a game HUD's, say) brings its own.
    pub tooltip_frame: Option<Frame>,
}

impl Default for TimeSeriesStyle {
    fn default() -> Self {
        Self {
            stroke_width: notedeck::tokens::STROKE_THICK,
            fill_opacity: 0.25,
            y_ticks: 4,
            min_x_label_spacing: 64.0,
            negative_color: None,
            tooltip_frame: None,
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
    time_series_chart(
        ui,
        size,
        data,
        &Marks::NONE,
        style,
        SeriesFill::Line,
        tooltip,
    )
}

/// Draws `data` as lines, one per series, with `marks` over them, filling
/// `size`. The axes cover the marks as well as the series.
///
/// `tooltip` renders the hover tooltip's contents; pass [`default_tooltip`]
/// for the plain one.
pub fn line_chart_marked(
    ui: &mut Ui,
    size: Vec2,
    data: &TimeSeries<'_>,
    marks: &Marks<'_>,
    style: &TimeSeriesStyle,
    tooltip: impl FnOnce(&mut Ui, &HoverPoint<'_>),
) -> TimeSeriesResponse {
    time_series_chart(ui, size, data, marks, style, SeriesFill::Line, tooltip)
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
    time_series_chart(
        ui,
        size,
        data,
        &Marks::NONE,
        style,
        SeriesFill::Area,
        tooltip,
    )
}

/// Draws the first `bars` series of `data` as bars, grouped side by side in
/// each bucket from the zero baseline, and the rest as lines over them, all
/// on one y axis, filling `size`.
///
/// A bar keeps its series' color below zero too; only the lines take the
/// negative color. `tooltip` renders the hover tooltip's contents; pass
/// [`default_tooltip`] for the plain one.
pub fn bar_line_chart(
    ui: &mut Ui,
    size: Vec2,
    data: &TimeSeries<'_>,
    bars: usize,
    style: &TimeSeriesStyle,
    tooltip: impl FnOnce(&mut Ui, &HoverPoint<'_>),
) -> TimeSeriesResponse {
    let bars = bars.min(data.series.len());
    time_series_chart(
        ui,
        size,
        data,
        &Marks::NONE,
        style,
        SeriesFill::Bars(bars),
        tooltip,
    )
}

/// How each series is drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SeriesFill {
    /// Every series a line.
    Line,
    /// Every series a line over the area it encloses with the baseline.
    Area,
    /// This many series as bars, the rest as lines.
    Bars(usize),
}

impl SeriesFill {
    /// How many leading series are bars.
    fn bars(self) -> usize {
        match self {
            SeriesFill::Bars(n) => n,
            SeriesFill::Line | SeriesFill::Area => 0,
        }
    }
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
    marks: &Marks<'_>,
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

    let buckets = data.buckets().max(marks.buckets());
    if buckets == 0 {
        return TimeSeriesResponse {
            response,
            plot_rect: rect,
            hovered: None,
            tooltip_rect: None,
        };
    }

    let span = marks.widen(data.value_span());
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
        slots: matches!(fill, SeriesFill::Bars(_)),
    };

    paint_y_axis(&painter, &plot, data, label_color, ui.visuals());
    paint_x_axis(&painter, &plot, data, style, label_color, rect);

    let hovered = response
        .hover_pos()
        .filter(|pos| pos.x >= plot.rect.left() - pad && pos.x <= plot.rect.right() + pad)
        .map(|pos| plot.bucket_at(pos.x));

    let (bars, lines) = data.series.split_at(fill.bars());
    if !bars.is_empty() {
        if let Some(bucket) = hovered {
            paint_slot(&painter, &plot, bucket, ui.visuals());
        }
        paint_bars(&painter, &plot, bars);
    }
    for series in lines {
        if fill == SeriesFill::Area {
            paint_area(&painter, &plot, series, style, negative);
        }
        paint_line(&painter, &plot, series, style, negative);
    }
    paint_marks(&painter, &plot, marks, style);

    let tooltip_rect = hovered.map(|bucket| {
        paint_crosshair(
            &painter,
            &plot,
            lines,
            bucket,
            style,
            negative,
            ui.visuals(),
        );
        show_tooltip(
            ui,
            &response,
            &plot,
            data,
            bucket,
            style.tooltip_frame,
            tooltip,
        )
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
    /// Whether each bucket is a slot of the plot's width with its point in
    /// the middle, as bars need, rather than a point with the first and last
    /// on the plot's edges.
    slots: bool,
}

impl Plot {
    /// Screen x of bucket `i`. A single point sits in the middle.
    fn x(&self, i: usize) -> f32 {
        if self.slots {
            return self.rect.left() + (i as f32 + 0.5) * self.bucket_width();
        }
        if self.buckets < 2 {
            return self.rect.center().x;
        }
        self.rect.left() + i as f32 * self.bucket_width()
    }

    /// Horizontal distance between two neighbouring buckets.
    fn bucket_width(&self) -> f32 {
        if self.slots {
            return self.rect.width() / (self.buckets as f32).max(1.0);
        }
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

    /// The bucket nearest screen x, or in slots the one whose slot holds
    /// it.
    fn bucket_at(&self, x: f32) -> usize {
        if self.buckets < 2 {
            return 0;
        }
        let at = (x - self.rect.left()) / self.bucket_width();
        let i = if self.slots { at.floor() } else { at.round() };
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

/// Length of a dash in a mark, and of the gap after it.
const DASH: f32 = 6.0;

/// `marks` over the series: the levels and markers, then the trails on top.
fn paint_marks(painter: &egui::Painter, plot: &Plot, marks: &Marks<'_>, style: &TimeSeriesStyle) {
    let pad = notedeck::tokens::SPACING_XS;
    for level in marks.levels {
        let y = plot.y(level.value);
        let stroke = Stroke::new(notedeck::tokens::STROKE_THIN, level.color);
        let ends = [pos2(plot.rect.left(), y), pos2(plot.rect.right(), y)];
        painter.extend(Shape::dashed_line(&ends, stroke, DASH, DASH / 2.0));
        painter.text(
            pos2(plot.rect.right(), y - pad),
            Align2::RIGHT_BOTTOM,
            level.label,
            axis_font(),
            level.color,
        );
    }
    for marker in marks.markers.iter().filter(|m| m.bucket < plot.buckets) {
        let x = plot.x(marker.bucket);
        let stroke = Stroke::new(notedeck::tokens::STROKE_THIN, marker.color);
        let ends = [pos2(x, plot.rect.top()), pos2(x, plot.rect.bottom())];
        painter.extend(Shape::dashed_line(&ends, stroke, DASH / 2.0, DASH / 2.0));
        let (anchor, x) = if x > plot.rect.center().x {
            (Align2::RIGHT_TOP, x - pad)
        } else {
            (Align2::LEFT_TOP, x + pad)
        };
        painter.text(
            pos2(x, plot.rect.top()),
            anchor,
            marker.label,
            axis_font(),
            marker.color,
        );
    }
    for trail in marks.trails {
        let joined = trail.join.map(|v| plot.point(trail.start, v));
        let points: Vec<Pos2> = joined
            .into_iter()
            .chain(
                (trail.start..)
                    .zip(trail.values)
                    .map(|(i, &v)| plot.point(i, v)),
            )
            .collect();
        let stroke = Stroke::new(style.stroke_width, trail.color);
        if let [only] = points[..] {
            painter.circle_filled(only, style.stroke_width, trail.color);
            continue;
        }
        painter.extend(Shape::dashed_line(&points, stroke, DASH, DASH / 2.0));
    }
}

/// Fraction of a slot its bar group spans; the rest is the gap between
/// groups.
const BAR_GROUP_WIDTH: f32 = 0.72;

/// Gap between two bars of a group, in points.
const BAR_GAP: f32 = 2.0;

/// The hovered bucket's slot, shaded behind its bars.
fn paint_slot(painter: &egui::Painter, plot: &Plot, bucket: usize, visuals: &egui::Visuals) {
    let half = plot.bucket_width() / 2.0;
    let x = plot.x(bucket);
    let slot = Rect::from_x_y_ranges(x - half..=x + half, plot.rect.y_range());
    painter.rect_filled(
        slot,
        notedeck::tokens::RADIUS_SM,
        visuals.widgets.hovered.weak_bg_fill,
    );
}

/// `bars` as a group per bucket: side by side in series order, each from
/// the baseline to its value, rounded at the value's end.
fn paint_bars(painter: &egui::Painter, plot: &Plot, bars: &[Series<'_>]) {
    let group = plot.bucket_width() * BAR_GROUP_WIDTH;
    let width = ((group - BAR_GAP * (bars.len() as f32 - 1.0)) / bars.len() as f32).max(1.0);
    let base = plot.y(0.0);
    let radius = (width / 2.0).min(4.0) as u8;
    for bucket in 0..plot.buckets {
        let left = plot.x(bucket) - group / 2.0;
        for (k, series) in bars.iter().enumerate() {
            let Some(&v) = series.values.get(bucket) else {
                continue;
            };
            if v == 0.0 {
                continue;
            }
            let x = left + k as f32 * (width + BAR_GAP);
            let y = plot.y(v);
            let bar = Rect::from_x_y_ranges(x..=x + width, y.min(base)..=y.max(base));
            let corners = if v > 0.0 {
                egui::CornerRadius {
                    nw: radius,
                    ne: radius,
                    sw: 0,
                    se: 0,
                }
            } else {
                egui::CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: radius,
                    se: radius,
                }
            };
            painter.rect_filled(bar, corners, series.color);
        }
    }
}

/// The hover mark at `bucket`: a vertical line (unless the buckets are
/// slots, which [`paint_slot`] shades instead) and a dot on each of `lines`
/// there.
fn paint_crosshair(
    painter: &egui::Painter,
    plot: &Plot,
    lines: &[Series<'_>],
    bucket: usize,
    style: &TimeSeriesStyle,
    negative: Color32,
    visuals: &egui::Visuals,
) {
    let x = plot.x(bucket);
    if !plot.slots {
        painter.vline(
            x,
            plot.rect.y_range(),
            Stroke::new(notedeck::tokens::STROKE_THIN, visuals.text_color()),
        );
    }
    for series in lines {
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
    frame: Option<Frame>,
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
            let frame = frame.unwrap_or_else(|| Frame::popup(ui.style()));
            frame.show(ui, |ui| {
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
            slots: false,
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
    fn slots_center_each_bucket_and_cover_their_width() {
        let p = Plot {
            slots: true,
            ..plot(4)
        };
        assert_eq!(p.bucket_width(), 25.0);
        assert_eq!(p.x(0), 12.5);
        assert_eq!(p.x(3), 87.5);
        assert_eq!(p.bucket_at(24.0), 0);
        assert_eq!(p.bucket_at(26.0), 1);
        assert_eq!(p.bucket_at(99.0), 3);
        let one = Plot {
            slots: true,
            ..plot(1)
        };
        assert_eq!(one.x(0), 50.0);
    }

    #[test]
    fn zero_sits_mid_plot_and_crossings_land_on_it() {
        let p = plot(2);
        assert_eq!(p.y(0.0), 50.0);
        let c = p.zero_crossing(0, 5.0, -5.0);
        assert_eq!(c, pos2(50.0, 50.0));
    }

    /// A trail reaches past the series and a level above them, so the
    /// axes cover both; a trail's join value counts too.
    #[test]
    fn marks_widen_the_buckets_and_the_span() {
        let trail = Trail {
            start: 3,
            join: Some(-4.0),
            values: &[5.0, 6.0],
            color: Color32::WHITE,
        };
        let level = Level {
            value: 12.0,
            label: "target",
            color: Color32::WHITE,
        };
        let marks = Marks {
            trails: &[trail],
            levels: &[level],
            markers: &[],
        };
        assert_eq!(marks.buckets(), 5);
        assert_eq!(Marks::NONE.buckets(), 0);
        let span = marks.widen(ValueSpan { min: 0.0, max: 3.0 });
        assert_eq!(
            span,
            ValueSpan {
                min: -4.0,
                max: 12.0
            }
        );
    }
}
