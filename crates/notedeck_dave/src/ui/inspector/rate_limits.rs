//! The Rate limits card: how much of each of the Claude account's
//! subscription windows is used, and when each resets.
//!
//! The windows belong to the account, not the session, so a local session
//! draws the account-wide value `Dave` keeps (see `Dave::rate_limit`). Its
//! strings are built by [`RateLimitLabels`] memos when a window moves or its
//! reset passes, so a frame only lays them out.

use crate::inspector::{RateLimitLabels, SessionInspector, RATE_LIMIT_WINDOWS};
use crate::messages::{RateLimitInfo, RateLimitStatus, RateLimitWindow, RateLimitWindowKind};
use crate::ui::dave::fill_color;
use notedeck::{tr, Localization};

use super::{key_ui, value_ui, weak_ui};

/// Height of a window's usage bar.
const BAR_HEIGHT: f32 = 8.0;

/// The Rate limits card's body. `info` is the account's latest report, or
/// `None` before one arrives.
pub fn rate_limits_ui(
    inspector: &mut SessionInspector,
    info: Option<&RateLimitInfo>,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) {
    let labels = &mut inspector.rate_limit_labels;
    let Some(info) = info else {
        weak_ui(
            ui,
            &tr!(
                i18n,
                "No rate-limit report yet",
                "Dave session inspector rate limits: shown before the CLI reports the account's windows"
            ),
        );
        account_note_ui(i18n, ui);
        return;
    };

    ui.horizontal(|ui| {
        key_ui(
            ui,
            &tr!(i18n, "Status", "Dave session inspector rate limits row"),
        );
        let text = labels.status.get(info.status, i18n, status_text);
        ui.label(
            egui::RichText::new(text)
                .font(egui::FontId::proportional(super::VALUE_SIZE))
                .color(status_color(info.status, ui)),
        );
    });

    let now = crate::session::now_unix();
    for (i, shown) in info.windows().take(RATE_LIMIT_WINDOWS).enumerate() {
        window_ui(labels, i, shown.kind, shown.window, now, i18n, ui);
    }
    account_note_ui(i18n, ui);
}

/// One window: its name and "82% · resets 3:42 PM" over a bar coloured by
/// how full it is. A window whose reset has passed is drawn faded, since the
/// next report will start it over.
fn window_ui(
    labels: &mut RateLimitLabels,
    i: usize,
    kind: RateLimitWindowKind,
    window: RateLimitWindow,
    now: u64,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) {
    let passed = window.resets_at <= now;
    ui.add_space(notedeck::tokens::SPACING_XS);
    ui.horizontal(|ui| {
        let name = labels.names[i].get(kind, i18n, window_name);
        value_ui(ui, name);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let text = labels.windows[i].get((kind, window, passed), i18n, window_text);
            weak_ui(ui, text);
        });
    });

    let size = egui::vec2(ui.available_width(), BAR_HEIGHT);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let radius = notedeck::tokens::RADIUS_SM;
    painter.rect_filled(rect, radius, ui.visuals().faint_bg_color);
    let fraction = window.utilization.clamp(0.0, 1.0) as f32;
    if fraction > 0.0 {
        let mut color = fill_color(fraction);
        if passed {
            color = color.gamma_multiply(notedeck::tokens::OPACITY_DISABLED);
        }
        let fill =
            egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * fraction, BAR_HEIGHT));
        painter.rect_filled(fill, radius, color);
    }
    painter.rect_stroke(
        rect,
        radius,
        ui.visuals().widgets.noninteractive.bg_stroke,
        egui::StrokeKind::Inside,
    );
}

/// The reminder that these windows are the account's, under every card.
fn account_note_ui(i18n: &mut Localization, ui: &mut egui::Ui) {
    ui.add_space(notedeck::tokens::SPACING_XS);
    ui.label(
        egui::RichText::new(tr!(
            i18n,
            "Account-wide: every session on this Claude account shares these windows.",
            "Dave session inspector rate limits: note that the limits are per account, not per session"
        ))
        .small()
        .weak(),
    );
}

/// The colour a status reads in: plain when allowed, warning near the
/// limit, error over it.
fn status_color(status: RateLimitStatus, ui: &egui::Ui) -> egui::Color32 {
    match status {
        RateLimitStatus::Allowed => ui.visuals().text_color(),
        RateLimitStatus::AllowedWarning => ui.visuals().warn_fg_color,
        RateLimitStatus::Rejected => ui.visuals().error_fg_color,
    }
}

fn status_text(status: RateLimitStatus, i18n: &mut Localization) -> String {
    match status {
        RateLimitStatus::Allowed => tr!(
            i18n,
            "Within limits",
            "Dave session inspector rate limits status: under every limit"
        ),
        RateLimitStatus::AllowedWarning => tr!(
            i18n,
            "Near a limit",
            "Dave session inspector rate limits status: close to a rate limit"
        ),
        RateLimitStatus::Rejected => tr!(
            i18n,
            "Limited until reset",
            "Dave session inspector rate limits status: over a limit, requests refused until it resets"
        ),
    }
}

fn window_name(kind: RateLimitWindowKind, i18n: &mut Localization) -> String {
    match kind {
        RateLimitWindowKind::FiveHour => tr!(
            i18n,
            "5-hour",
            "Dave session inspector rate limits: the five-hour session window"
        ),
        RateLimitWindowKind::SevenDay => tr!(
            i18n,
            "7-day",
            "Dave session inspector rate limits: the weekly window"
        ),
        RateLimitWindowKind::SevenDayOpus => tr!(
            i18n,
            "7-day Opus",
            "Dave session inspector rate limits: the weekly window for Opus"
        ),
        RateLimitWindowKind::SevenDaySonnet => tr!(
            i18n,
            "7-day Sonnet",
            "Dave session inspector rate limits: the weekly window for Sonnet"
        ),
        RateLimitWindowKind::SevenDayOverageIncluded => tr!(
            i18n,
            "7-day with overage",
            "Dave session inspector rate limits: the weekly window including paid overage"
        ),
        RateLimitWindowKind::Overage => tr!(
            i18n,
            "Overage",
            "Dave session inspector rate limits: paid usage past the subscription limits"
        ),
    }
}

/// "82% · resets 3:42 PM", or "reset at 3:42 PM" once it has. A weekly
/// window's reset names the day too.
fn window_text(
    (kind, window, passed): (RateLimitWindowKind, RateLimitWindow, bool),
    i18n: &mut Localization,
) -> String {
    let at = reset_time(window.resets_at, kind.is_weekly());
    if passed {
        return tr!(
            i18n,
            "reset at {time}",
            "Dave session inspector rate limits: a window whose reset time has passed",
            time = at.as_str()
        );
    }
    let percent = percent(window.utilization).to_string();
    tr!(
        i18n,
        "{percent}% · resets {time}",
        "Dave session inspector rate limits: how much of a window is used and when it resets",
        percent = percent.as_str(),
        time = at.as_str()
    )
}

/// A Unix time as local wall-clock time, with the weekday when `with_day`:
/// "3:42 PM", "Tue 3:42 PM".
fn reset_time(unix: u64, with_day: bool) -> String {
    if !with_day {
        return super::clock_time(unix);
    }
    chrono::DateTime::from_timestamp(unix as i64, 0)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%a %-I:%M %p")
                .to_string()
        })
        .unwrap_or_default()
}

/// A utilization fraction as a whole percentage.
fn percent(utilization: f64) -> u32 {
    (utilization * 100.0).round().max(0.0) as u32
}

/// The status bar's pill for an account near or over a limit, which opens
/// the Rate limits card. Draws nothing while the account is within limits.
pub fn rate_limit_pill_ui(
    info: &RateLimitInfo,
    labels: &mut RateLimitLabels,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) -> Option<egui::Response> {
    use crate::ui::badge::{BadgeVariant, StatusBadge};

    if !info.needs_attention() {
        return None;
    }
    let percent = info.utilization.map(percent).unwrap_or(0);
    let text = labels.pill.get((info.status, percent), i18n, pill_text);
    let variant = if info.status == RateLimitStatus::Rejected {
        BadgeVariant::Destructive
    } else {
        BadgeVariant::Warning
    };
    Some(
        StatusBadge::new(text)
            .variant(variant)
            .show(ui)
            .on_hover_cursor(egui::CursorIcon::PointingHand),
    )
}

fn pill_text((status, percent): (RateLimitStatus, u32), i18n: &mut Localization) -> String {
    if status == RateLimitStatus::Rejected {
        return tr!(
            i18n,
            "Rate limited",
            "Dave status bar pill: the Claude account is over a rate limit"
        );
    }
    let percent = percent.to_string();
    tr!(
        i18n,
        "Limit {percent}%",
        "Dave status bar pill: the Claude account is near a rate limit, with how much of it is used",
        percent = percent.as_str()
    )
}
