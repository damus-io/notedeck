//! Charts shared across notedeck apps: horizontal and stacked bars,
//! sparklines, and time-series line/area/bar charts with hover tooltips.

mod bar;
mod sparkline;
mod time_series;

pub use bar::{horizontal_bar_chart, palette, stacked_bars, Bar, BarChartStyle, Segment};
pub use sparkline::{sparkline, SparkStyle};
pub use time_series::{
    area_chart, axis_ticks, bar_line_chart, default_tooltip, line_chart, line_chart_marked,
    AxisTicks, HoverPoint, HoverValue, Level, Marker, Marks, Series, TimeSeries,
    TimeSeriesResponse, TimeSeriesStyle, Trail,
};
