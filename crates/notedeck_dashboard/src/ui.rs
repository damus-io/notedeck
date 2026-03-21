use egui::FontId;
use egui::RichText;

use std::time::Instant;

use nostrdb::Transaction;
use notedeck::{
    AppContext, abbrev::floor_char_boundary, name::get_display_name, profile::get_profile_url,
    theme::ColorTheme, tokens,
};
use notedeck_ui::ProfilePic;

use crate::Dashboard;
use crate::FxHashMap;
use crate::Period;
use crate::RollingCache;
use crate::chart::Bar;
use crate::chart::BarChartStyle;
use crate::chart::horizontal_bar_chart;
use crate::chart::palette;
use crate::top_kind1_authors_over;
use crate::top_kinds_over;
use crate::top_new_contact_list_clients_over;

pub fn period_picker_ui(ui: &mut egui::Ui, period: &mut Period) {
    let theme = ColorTheme::current(ui.ctx());

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;

        for (i, p) in Period::ALL.iter().enumerate() {
            let selected = *period == *p;

            let (bg, text_color) = if selected {
                (theme.accent, egui::Color32::WHITE)
            } else {
                (egui::Color32::TRANSPARENT, theme.text_secondary)
            };

            let rounding = match i {
                0 => egui::CornerRadius {
                    nw: tokens::RADIUS_SM as u8,
                    sw: tokens::RADIUS_SM as u8,
                    ne: 0,
                    se: 0,
                },
                2 => egui::CornerRadius {
                    nw: 0,
                    sw: 0,
                    ne: tokens::RADIUS_SM as u8,
                    se: tokens::RADIUS_SM as u8,
                },
                _ => egui::CornerRadius::ZERO,
            };

            let btn = egui::Button::new(RichText::new(p.label()).small().color(text_color))
                .fill(bg)
                .corner_radius(rounding)
                .stroke(egui::Stroke::new(tokens::STROKE_THIN, theme.border_default));

            if ui.add(btn).clicked() {
                *period = *p;
            }
        }
    });
}

pub fn dashboard_controls_ui(d: &mut Dashboard, ui: &mut egui::Ui) {
    let theme = ColorTheme::current(ui.ctx());

    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Dashboard")
                .font(FontId::proportional(20.0))
                .color(theme.text_primary)
                .strong(),
        );

        ui.add_space(tokens::SPACING_LG);

        period_picker_ui(ui, &mut d.period);

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Status on the right
            status_text(ui, d);

            ui.add_space(tokens::SPACING_MD);

            let refresh_btn = if d.running {
                egui::Button::new(RichText::new("Refreshing…").small().color(theme.text_muted))
            } else {
                egui::Button::new(RichText::new("⟳ Refresh").small())
            };

            if ui.add_enabled(!d.running, refresh_btn).clicked() {
                d.force_refresh();
            }
        });
    });
}

fn status_text(ui: &mut egui::Ui, d: &Dashboard) {
    let theme = ColorTheme::current(ui.ctx());

    if let Some(e) = &d.last_error {
        ui.label(RichText::new(e).color(theme.destructive).small());
        return;
    }

    let mut parts: Vec<String> = Vec::new();
    if d.running {
        parts.push("updating…".to_owned());
    }
    if let Some(t) = d.last_snapshot {
        parts.push(format!(
            "updated {:.1?} ago",
            Instant::now().duration_since(t)
        ));
    }
    if let Some(dur) = d.last_duration {
        let ms = dur.as_secs_f64() * 1000.0;
        parts.push(format!("{ms:.0} ms"));
    }
    if parts.is_empty() {
        parts.push("—".to_owned());
    }

    ui.label(
        RichText::new(parts.join(" · "))
            .small()
            .color(theme.text_muted),
    );
}

fn card_header_ui(ui: &mut egui::Ui, title: &str) {
    let theme = ColorTheme::current(ui.ctx());
    ui.label(RichText::new(title).small().color(theme.text_muted));
}

pub fn card_ui(
    ui: &mut egui::Ui,
    min_card: f32,
    content: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let theme = ColorTheme::current(ui.ctx());

    egui::Frame::group(ui.style())
        .fill(theme.surface_secondary)
        .corner_radius(egui::CornerRadius::same(tokens::RADIUS_MD as u8))
        .inner_margin(egui::Margin::same(tokens::SPACING_LG as i8))
        .stroke(egui::Stroke::new(tokens::STROKE_THIN, theme.border_default))
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
    card_header_ui(ui, "KINDS");
    ui.add_space(tokens::SPACING_SM);

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
        let theme = ColorTheme::current(ui.ctx());
        ui.label(
            RichText::new("…")
                .font(FontId::proportional(24.0))
                .color(theme.text_muted),
        );
    } else {
        horizontal_bar_chart(ui, None, &bars, BarChartStyle::default());
    }
}

pub fn totals_ui(dashboard: &Dashboard, ui: &mut egui::Ui) {
    let theme = ColorTheme::current(ui.ctx());
    card_header_ui(ui, "TOTAL EVENTS");
    ui.add_space(tokens::SPACING_SM);

    let count: u64 = match dashboard.period {
        Period::Daily => total_over(&dashboard.state.daily),
        Period::Weekly => total_over(&dashboard.state.weekly),
        Period::Monthly => total_over(&dashboard.state.monthly),
    };

    ui.label(
        RichText::new(count.to_string())
            .font(FontId::proportional(36.0))
            .color(theme.text_primary)
            .strong(),
    );
}

pub fn posts_per_period_ui(dashboard: &mut Dashboard, ui: &mut egui::Ui) {
    card_header_ui(
        ui,
        &format!(
            "KIND 1 POSTS PER {}",
            dashboard.period.label().to_uppercase()
        ),
    );
    ui.add_space(tokens::SPACING_SM);

    let cache = dashboard.selected_cache();
    let bars = series_bars_for_kind(dashboard.period, cache, 1);

    if bars.is_empty() && dashboard.state.total.total == 0 && dashboard.last_error.is_none() {
        let theme = ColorTheme::current(ui.ctx());
        ui.label(
            RichText::new("…")
                .font(FontId::proportional(24.0))
                .color(theme.text_muted),
        );
    } else if bars.is_empty() {
        ui.label("No data");
    } else {
        horizontal_bar_chart(ui, None, &bars, BarChartStyle::default());
    }
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
    const NAMES: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let name = NAMES[(month.saturating_sub(1)) as usize];
    let yy = (year % 100).abs();
    format!("{name} \u{2019}{yy:02}")
}

fn bucket_label(period: Period, start_ts: i64, end_ts: i64) -> String {
    use chrono::{Datelike, TimeZone, Utc};

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

pub fn dashboard_ui(dashboard: &mut Dashboard, ui: &mut egui::Ui, ctx: &mut AppContext<'_>) {
    egui::Frame::new()
        .inner_margin(egui::Margin::same(tokens::SPACING_XL as i8))
        .show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                dashboard_ui_inner(dashboard, ui, ctx);
            });
        });
}

fn dashboard_ui_inner(dashboard: &mut Dashboard, ui: &mut egui::Ui, ctx: &mut AppContext<'_>) {
    let min_card = 260.0;

    dashboard_controls_ui(dashboard, ui);
    ui.add_space(tokens::SPACING_LG);

    ui.with_layout(
        egui::Layout::left_to_right(egui::Align::TOP).with_main_wrap(true),
        |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(tokens::SPACING_SM, tokens::SPACING_SM);
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
            ui.add_sized(size, |ui: &mut egui::Ui| {
                card_ui(ui, min_card, |ui| clients_stack_ui(dashboard, ui))
            });
            ui.add_sized(size, |ui: &mut egui::Ui| {
                card_ui(ui, min_card, |ui| clients_trends_ui(dashboard, ui))
            });
            ui.add_sized(size, |ui: &mut egui::Ui| {
                card_ui(ui, min_card, |ui| new_contact_lists_ui(dashboard, ui))
            });
            ui.add_sized(size, |ui: &mut egui::Ui| {
                card_ui(ui, min_card, |ui| top_posters_ui(dashboard, ui, ctx))
            });
        },
    );
}

fn client_series(cache: &RollingCache, client: &str) -> Vec<f32> {
    let n = cache.buckets.len();
    let mut out = Vec::with_capacity(n);
    for i in (0..n).rev() {
        let v = cache.buckets[i]
            .client_pubkeys
            .get(client)
            .map(|s| s.len() as f32)
            .unwrap_or(0.0);
        out.push(v);
    }
    out
}

pub fn clients_trends_ui(dashboard: &mut Dashboard, ui: &mut egui::Ui) {
    card_header_ui(ui, "CLIENTS (TREND)");
    ui.add_space(tokens::SPACING_SM);

    let limit = 10;

    let cache = match dashboard.period {
        Period::Daily => &dashboard.state.daily,
        Period::Weekly => &dashboard.state.weekly,
        Period::Monthly => &dashboard.state.monthly,
    };

    let top = top_clients_over(cache, limit);
    if top.is_empty() && dashboard.last_error.is_none() {
        let theme = ColorTheme::current(ui.ctx());
        ui.label(
            RichText::new("…")
                .font(FontId::proportional(24.0))
                .color(theme.text_muted),
        );
        return;
    }
    if top.is_empty() {
        ui.label("No client tags");
        return;
    }

    let spark_w = (ui.available_width() - 140.0).max(80.0);

    for (row_i, cs) in top.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(&cs.name).small());
            ui.add_space(tokens::SPACING_SM);

            let series = client_series(cache, &cs.name);

            let resp = crate::sparkline::sparkline(
                ui,
                egui::vec2(spark_w, tokens::SPARKLINE_HEIGHT),
                &series,
                palette(row_i),
                crate::sparkline::SparkStyle::default(),
            );

            if resp.hovered() {
                let last = series.last().copied().unwrap_or(0.0);
                resp.on_hover_text(format!(
                    "{} users / {} events\nlatest bucket: {:.0}",
                    cs.unique_pubkeys, cs.events, last
                ));
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{} / {}", cs.unique_pubkeys, cs.events))
                        .small()
                        .strong(),
                );
            });
        });
        ui.add_space(tokens::SPACING_XS);
    }
}

fn stacked_clients_over_time(
    cache: &RollingCache,
    top: &[ClientStats],
) -> Vec<Vec<(egui::Color32, f32)>> {
    let n = cache.buckets.len();
    let mut out = Vec::with_capacity(n);

    for i in (0..n).rev() {
        let mut segs = Vec::with_capacity(top.len());
        for (idx, cs) in top.iter().enumerate() {
            let v = *cache.buckets[i].clients.get(&cs.name).unwrap_or(&0) as f32;
            segs.push((palette(idx), v));
        }
        out.push(segs);
    }
    out
}

pub fn clients_stack_ui(dashboard: &mut Dashboard, ui: &mut egui::Ui) {
    card_header_ui(ui, "CLIENTS (STACKED)");
    ui.add_space(tokens::SPACING_SM);

    let limit = 6;

    let cache = dashboard.selected_cache();
    let top = top_clients_over(cache, limit);

    if top.is_empty() && dashboard.last_error.is_none() {
        let theme = ColorTheme::current(ui.ctx());
        ui.label(
            RichText::new("…")
                .font(FontId::proportional(24.0))
                .color(theme.text_muted),
        );
    } else if top.is_empty() {
        ui.label("No client tags");
    } else {
        let buckets = stacked_clients_over_time(cache, &top);
        let w = ui.available_width().max(120.0);
        let h = 70.0;

        let resp = crate::chart::stacked_bars(ui, egui::vec2(w, h), &buckets);

        // legend
        ui.add_space(tokens::SPACING_SM);
        ui.horizontal_wrapped(|ui| {
            for (i, cs) in top.iter().enumerate() {
                ui.label(RichText::new("■").color(palette(i)));
                ui.label(RichText::new(&cs.name).small());
                ui.add_space(tokens::SPACING_MD);
            }
        });

        let _ = resp;
    }
}

struct ClientStats {
    pub name: String,
    pub events: u64,
    pub unique_pubkeys: usize,
}

fn top_clients_over(cache: &RollingCache, limit: usize) -> Vec<ClientStats> {
    let mut event_agg: FxHashMap<String, u64> = FxHashMap::default();
    let mut pubkey_agg: FxHashMap<String, rustc_hash::FxHashSet<enostr::Pubkey>> =
        FxHashMap::default();

    for b in &cache.buckets {
        for (client, count) in &b.clients {
            *event_agg.entry(client.clone()).or_default() += *count as u64;
        }
        for (client, pubkeys) in &b.client_pubkeys {
            pubkey_agg
                .entry(client.clone())
                .or_default()
                .extend(pubkeys);
        }
    }

    let mut out: Vec<ClientStats> = event_agg
        .into_iter()
        .map(|(name, events)| {
            let unique_pubkeys = pubkey_agg.get(&name).map(|s| s.len()).unwrap_or(0);
            ClientStats {
                name,
                events,
                unique_pubkeys,
            }
        })
        .collect();

    out.sort_by(|a, b| {
        b.unique_pubkeys
            .cmp(&a.unique_pubkeys)
            .then_with(|| b.events.cmp(&a.events))
            .then_with(|| a.name.cmp(&b.name))
    });

    out.truncate(limit);
    out
}

fn new_contact_list_series(cache: &RollingCache, client: &str) -> Vec<f32> {
    let n = cache.buckets.len();
    let mut out = Vec::with_capacity(n);
    for i in (0..n).rev() {
        let v = cache.buckets[i]
            .new_contact_list_clients
            .get(client)
            .copied()
            .unwrap_or(0) as f32;
        out.push(v);
    }
    out
}

pub fn new_contact_lists_ui(dashboard: &mut Dashboard, ui: &mut egui::Ui) {
    card_header_ui(ui, "NEW CONTACT LISTS BY CLIENT");
    ui.add_space(tokens::SPACING_SM);

    let limit = 10;
    let cache = dashboard.selected_cache();
    let top = top_new_contact_list_clients_over(cache, limit);

    if top.is_empty() && dashboard.last_error.is_none() {
        let theme = ColorTheme::current(ui.ctx());
        ui.label(
            RichText::new("…")
                .font(FontId::proportional(24.0))
                .color(theme.text_muted),
        );
        return;
    }
    if top.is_empty() {
        ui.label("No data");
        return;
    }

    let spark_w = (ui.available_width() - 140.0).max(80.0);

    for (row_i, (client, total)) in top.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(client).small());
            ui.add_space(tokens::SPACING_SM);

            let series = new_contact_list_series(cache, client);
            let resp = crate::sparkline::sparkline(
                ui,
                egui::vec2(spark_w, tokens::SPARKLINE_HEIGHT),
                &series,
                palette(row_i),
                crate::sparkline::SparkStyle::default(),
            );

            if resp.hovered() {
                let last = series.last().copied().unwrap_or(0.0);
                resp.on_hover_text(format!(
                    "{} new contact lists\nlatest bucket: {:.0}",
                    total, last
                ));
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(total.to_string()).small().strong());
            });
        });
        ui.add_space(tokens::SPACING_XS);
    }
}

pub fn top_posters_ui(dashboard: &mut Dashboard, ui: &mut egui::Ui, ctx: &mut AppContext<'_>) {
    let cache = dashboard.selected_cache();
    let n = cache.buckets.len();
    let unit = dashboard.period.label();
    let header = format!("TOP POSTERS ({n} {unit}s)");
    card_header_ui(ui, &header);
    ui.add_space(tokens::SPACING_SM);

    let limit = 10;
    let top = top_kind1_authors_over(cache, limit);

    if top.is_empty() && dashboard.last_error.is_none() {
        let theme = ColorTheme::current(ui.ctx());
        ui.label(
            RichText::new("…")
                .font(FontId::proportional(24.0))
                .color(theme.text_muted),
        );
        return;
    }

    let txn = match Transaction::new(ctx.ndb) {
        Ok(t) => t,
        Err(_) => {
            ui.label("DB error");
            return;
        }
    };

    let pfp_size = ProfilePic::small_size() as f32;

    for (pubkey, count) in &top {
        let profile = ctx.ndb.get_profile_by_pubkey(&txn, pubkey.bytes()).ok();
        let name = get_display_name(profile.as_ref());
        let pfp_url = get_profile_url(profile.as_ref());

        ui.horizontal(|ui| {
            ui.add(
                &mut ProfilePic::new(ctx.img_cache, ctx.media_jobs.sender(), pfp_url)
                    .size(pfp_size),
            );
            ui.add_space(tokens::SPACING_SM);

            let display = name.name();
            let truncated = if display.len() > 16 {
                let end = floor_char_boundary(display, 16);
                format!("{}...", &display[..end])
            } else {
                display.to_string()
            };
            ui.label(RichText::new(truncated).small());

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(count.to_string()).small().strong());
            });
        });
        ui.add_space(tokens::SPACING_XS);
    }
}
