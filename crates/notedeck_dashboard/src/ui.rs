use egui::FontId;
use egui::RichText;

use std::time::Duration;
use std::time::Instant;

use crate::Dashboard;
use crate::chart::Bar;
use crate::chart::BarChartStyle;
use crate::chart::horizontal_bar_chart;
use crate::chart::palette;

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

pub fn card_ui(ui: &mut egui::Ui, min_card: f32, content: impl FnOnce(&mut egui::Ui)) {
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
            ui.vertical(|ui| content(ui));
        });
}

pub fn kinds_ui(dashboard: &Dashboard, ui: &mut egui::Ui) {
    card_header_ui(ui, "Kinds");
    ui.add_space(8.0);

    let bars = kinds_to_bars(&dashboard.state.top_kinds);
    if bars.is_empty() && dashboard.state.total_count == 0 && dashboard.last_error.is_none() {
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

    ui.horizontal(|ui| {
        ui.label(
            RichText::new(dashboard.state.total_count.to_string())
                .font(FontId::proportional(34.0))
                .strong(),
        );

        ui.add_space(10.0);
    });
}

pub fn posts_per_month_ui(dashboard: &Dashboard, ui: &mut egui::Ui) {
    card_header_ui(ui, "Posts per month (last 6 months)");
    ui.add_space(8.0);

    let bars = posts_per_month_to_bars(&dashboard.state.posts_per_month);
    if bars.is_empty() && dashboard.state.total_count == 0 && dashboard.last_error.is_none() {
        ui.label(RichText::new("…").font(FontId::proportional(24.0)).weak());
    } else if bars.is_empty() {
        ui.label("No data");
    } else {
        let mut style = BarChartStyle::default();
        style.value_precision = 0;
        style.show_values = true;
        horizontal_bar_chart(ui, None, &bars, style);
    }

    footer_status_ui(
        ui,
        dashboard.running,
        dashboard.last_error.as_deref(),
        dashboard.last_snapshot,
        dashboard.last_duration,
    );
}

fn kinds_to_bars(top_kinds: &[(u32, u64)]) -> Vec<Bar> {
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

fn posts_per_month_to_bars(items: &[(String, u64)]) -> Vec<Bar> {
    items
        .iter()
        .enumerate()
        .map(|(i, (label, count))| Bar {
            label: label.clone(),
            value: *count as f32,
            color: palette(i),
        })
        .collect()
}
