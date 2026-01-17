use egui::FontId;
use egui::RichText;

use std::time::Duration;
use std::time::Instant;

use crate::Dashboard;
use crate::Period;
use crate::RollingCache;
use crate::chart::Bar;
use crate::chart::BarChartStyle;
use crate::chart::horizontal_bar_chart;
use crate::chart::palette;
use crate::top_kinds_over;

pub fn period_picker_ui(ui: &mut egui::Ui, period: &mut Period) {
    ui.horizontal(|ui| {
        for p in Period::ALL {
            let selected = *period == p;
            if ui.selectable_label(selected, p.label()).clicked() {
                *period = p;
            }
        }
    });
}

pub fn dashboard_controls_ui(d: &mut Dashboard, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Range").small().weak());
        period_picker_ui(ui, &mut d.period);

        ui.add_space(12.0);
    });
}

pub fn footer_status_ui(
    ui: &mut egui::Ui,
    running: bool,
    err: Option<&str>,
    last_snapshot: Option<Instant>,
    last_duration: Option<Duration>,
) {
    ui.add_space(8.0);

    if let Some(e) = err {
        ui.label(RichText::new(e).color(ui.visuals().error_fg_color).small());
        return;
    }

    let mut parts: Vec<String> = Vec::new();
    if running {
        parts.push("updating…".to_owned());
    }

    if let Some(t) = last_snapshot {
        parts.push(format!(
            "updated {:.1?} ago",
            Instant::now().duration_since(t)
        ));
    }

    if let Some(d) = last_duration {
        let ms = d.as_secs_f64() * 1000.0;
        parts.push(format!("{ms:.0} ms"));
    }

    if parts.is_empty() {
        parts.push("—".to_owned());
    }

    ui.label(RichText::new(parts.join(" · ")).small().weak());
}

fn card_header_ui(ui: &mut egui::Ui, title: &str) {
    ui.horizontal(|ui| {
        let weak = ui.visuals().weak_text_color();
        ui.add(
            egui::Label::new(egui::RichText::new(title).small().color(weak))
                .wrap_mode(egui::TextWrapMode::Wrap),
        );
    });
}

pub fn card_ui(
    ui: &mut egui::Ui,
    min_card: f32,
    content: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let visuals = ui.visuals().clone();
    egui::Frame::group(ui.style())
        .fill(visuals.extreme_bg_color)
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::same(12))
        .stroke(egui::Stroke::new(
            1.0,
            visuals.widgets.noninteractive.bg_stroke.color,
        ))
        .show(ui, |ui| {
            ui.set_min_width(min_card);
            ui.set_min_height(min_card * 0.5);
            ui.vertical(|ui| {
                content(ui);
            });
        })
        .response
}

pub fn kinds_ui(dashboard: &mut Dashboard, ui: &mut egui::Ui) {
    card_header_ui(ui, "Kinds");
    ui.add_space(8.0);

    // top kind limit, don't show more then this
    let limit = 10;

    let window_total = match dashboard.period {
        Period::Daily => total_over(&dashboard.state.daily),
        Period::Weekly => total_over(&dashboard.state.weekly),
        Period::Monthly => total_over(&dashboard.state.monthly),
    };

    let top = match dashboard.period {
        Period::Daily => top_kinds_over(&dashboard.state.daily, limit),
        Period::Weekly => top_kinds_over(&dashboard.state.weekly, limit),
        Period::Monthly => top_kinds_over(&dashboard.state.monthly, limit),
    };

    let bars = kinds_to_bars(&top);

    if bars.is_empty() && window_total == 0 && dashboard.last_error.is_none() {
        // still show something (no loading screen)
        ui.label(RichText::new("…").font(FontId::proportional(24.0)).weak());
    } else {
        horizontal_bar_chart(ui, None, &bars, BarChartStyle::default());
    }

    footer_status_ui(
        ui,
        dashboard.running,
        dashboard.last_error.as_deref(),
        dashboard.last_snapshot,
        dashboard.last_duration,
    );
}

pub fn totals_ui(dashboard: &Dashboard, ui: &mut egui::Ui) {
    card_header_ui(ui, "All notes");
    ui.add_space(8.0);

    let count: u64 = match dashboard.period {
        Period::Daily => total_over(&dashboard.state.daily),
        Period::Weekly => total_over(&dashboard.state.weekly),
        Period::Monthly => total_over(&dashboard.state.monthly),
    };

    ui.horizontal(|ui| {
        ui.label(
            RichText::new(count.to_string())
                .font(FontId::proportional(34.0))
                .strong(),
        );

        ui.add_space(10.0);
    });
}

pub fn posts_per_period_ui(dashboard: &Dashboard, ui: &mut egui::Ui) {
    card_header_ui(
        ui,
        &format!("Kind 1 posts per {}", dashboard.period.label()),
    );
    ui.add_space(8.0);

    let cache = dashboard.selected_cache();
    let bars = series_bars_for_kind(dashboard.period, cache, 1);

    if bars.is_empty() && dashboard.state.total.total == 0 && dashboard.last_error.is_none() {
        ui.label(RichText::new("…").font(FontId::proportional(24.0)).weak());
    } else if bars.is_empty() {
        ui.label("No data");
    } else {
        horizontal_bar_chart(ui, None, &bars, BarChartStyle::default());
    }

    footer_status_ui(
        ui,
        dashboard.running,
        dashboard.last_error.as_deref(),
        dashboard.last_snapshot,
        dashboard.last_duration,
    );
}

fn kinds_to_bars(top_kinds: &[(u64, u64)]) -> Vec<Bar> {
    top_kinds
        .iter()
        .enumerate()
        .map(|(i, (k, c))| Bar {
            label: format!("{k}"),
            value: *c as f32,
            color: palette(i),
        })
        .collect()
}

fn month_label(year: i32, month: u32) -> String {
    // e.g. "Jan ’26" when year differs, otherwise just "Jan" would be
    // ambiguous across years We'll always include the year suffix to
    // keep it clear when the range crosses years.
    const NAMES: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let name = NAMES[(month.saturating_sub(1)) as usize];
    let yy = (year % 100).abs();
    format!("{name} \u{2019}{yy:02}")
}

fn bucket_label(period: Period, start_ts: i64, end_ts: i64) -> String {
    use chrono::{Datelike, TimeZone, Utc};

    // end-1 keeps labels stable at boundaries
    let default_label = "—";
    let Some(end_dt) = Utc.timestamp_opt(end_ts.saturating_sub(1), 0).single() else {
        return default_label.to_owned();
    };

    match period {
        Period::Daily => end_dt.format("%b %d").to_string(),
        Period::Weekly => match Utc.timestamp_opt(start_ts, 0).single() {
            Some(s) => format!("{}-{}", s.format("%b %d"), end_dt.format("%b %d")),
            None => default_label.to_owned(),
        },
        Period::Monthly => month_label(end_dt.year(), end_dt.month()),
    }
}

fn series_bars_for_kind(period: Period, cache: &RollingCache, kind: u64) -> Vec<Bar> {
    let n = cache.buckets.len();
    let mut out = Vec::with_capacity(n);

    for i in (0..n).rev() {
        let end_ts = cache.bucket_end_ts(i);
        let start_ts = cache.bucket_start_ts(i);

        let label = bucket_label(period, start_ts, end_ts);

        let count = *cache.buckets[i].kinds.get(&kind).unwrap_or(&0) as f32;

        out.push(Bar {
            label,
            value: count,
            color: palette(out.len()),
        });
    }

    out
}

// Count totals
fn total_over(cache: &RollingCache) -> u64 {
    cache.buckets.iter().map(|b| b.total).sum()
}

pub fn dashboard_ui(dashboard: &mut Dashboard, ui: &mut egui::Ui) {
    egui::Frame::new()
        .inner_margin(egui::Margin::same(20))
        .show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                dashboard_ui_inner(dashboard, ui);
            });
        });
}

fn dashboard_ui_inner(dashboard: &mut Dashboard, ui: &mut egui::Ui) {
    let min_card = 240.0;
    let gap = 8.0;

    dashboard_controls_ui(dashboard, ui);

    ui.with_layout(
        egui::Layout::left_to_right(egui::Align::TOP).with_main_wrap(true),
        |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
            let size = [min_card, min_card];
            ui.add_sized(size, |ui: &mut egui::Ui| {
                card_ui(ui, min_card, |ui| totals_ui(dashboard, ui))
            });
            ui.add_sized(size, |ui: &mut egui::Ui| {
                card_ui(ui, min_card, |ui| posts_per_period_ui(dashboard, ui))
            });
            ui.add_sized(size, |ui: &mut egui::Ui| {
                card_ui(ui, min_card, |ui| kinds_ui(dashboard, ui))
            });
        },
    );
}
