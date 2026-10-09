//! The Usage card: how full the context window is and with what, what the
//! session has cost, and the context turn by turn with its compactions.
//!
//! Everything it draws comes from [`UsageHistory`]; the strings are built by
//! [`UsageLabels`] memos and the sparkline's points when the history changes,
//! so a frame only lays them out.

use crate::inspector::{LabelMemo, SessionInspector, Sparkline, Tokens, UsageHistory, UsageLabels};
use crate::ui::dave::format_tokens;
use notedeck::{tr, tr_plural, Localization};

use super::{heading_ui, key_ui, value_ui, weak_ui};

/// Height of the stacked context bar.
const BAR_HEIGHT: f32 = 10.0;

/// Height of the context-per-turn sparkline.
const SPARKLINE_HEIGHT: f32 = 48.0;

/// Side of a legend swatch.
const SWATCH: f32 = 8.0;

/// Most compactions the card lists, newest first.
const COMPACTIONS_SHOWN: usize = 3;

/// The kinds of token on the bar, in the order they stack, and output after
/// them in the legend only (it isn't in the context).
#[derive(Clone, Copy)]
enum Kind {
    CacheRead,
    CacheWrite,
    Input,
    Output,
}

impl Kind {
    const ALL: [Kind; 4] = [Kind::CacheRead, Kind::CacheWrite, Kind::Input, Kind::Output];

    fn tokens(self, tokens: &Tokens) -> u64 {
        match self {
            Kind::CacheRead => tokens.cache_read,
            Kind::CacheWrite => tokens.cache_creation,
            Kind::Input => tokens.input,
            Kind::Output => tokens.output,
        }
    }

    fn color(self) -> egui::Color32 {
        use notedeck::tokens::PALETTE;
        match self {
            Kind::CacheRead => PALETTE[9],
            Kind::CacheWrite => PALETTE[3],
            Kind::Input => PALETTE[1],
            Kind::Output => PALETTE[2],
        }
    }

    fn memo(self, labels: &mut UsageLabels) -> &mut LabelMemo<u64> {
        &mut labels.breakdown[self as usize]
    }

    fn label(self, count: u64, i18n: &mut Localization) -> String {
        let count = format_tokens(count);
        match self {
            Kind::CacheRead => tr!(
                i18n,
                "cache read {count}",
                "Dave session inspector usage: context tokens read from the prompt cache",
                count = count.as_str()
            ),
            Kind::CacheWrite => tr!(
                i18n,
                "cache write {count}",
                "Dave session inspector usage: context tokens written to the prompt cache",
                count = count.as_str()
            ),
            Kind::Input => tr!(
                i18n,
                "input {count}",
                "Dave session inspector usage: uncached context tokens",
                count = count.as_str()
            ),
            Kind::Output => tr!(
                i18n,
                "output {count}",
                "Dave session inspector usage: tokens the model wrote in its last reply",
                count = count.as_str()
            ),
        }
    }
}

/// The Usage card's body.
pub fn usage_ui(
    inspector: &mut SessionInspector,
    context_window: u64,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) {
    let usage = &inspector.usage;
    let labels = &mut inspector.usage_labels;
    let Some(current) = usage.current() else {
        weak_ui(
            ui,
            &tr!(
                i18n,
                "No usage reported yet",
                "Dave session inspector usage: shown before the agent's first request finishes"
            ),
        );
        return;
    };

    let context = current.context();
    ui.horizontal(|ui| {
        key_ui(
            ui,
            &tr!(i18n, "Context", "Dave session inspector usage row"),
        );
        let text = labels
            .context
            .get((context, context_window), i18n, context_text);
        value_ui(ui, text);
    });
    context_bar_ui(&current, context_window, ui);
    legend_ui(&current, labels, i18n, ui);

    if usage.turns_recorded() == 0 {
        return;
    }
    ui.add_space(notedeck::tokens::SPACING_XS);
    ui.horizontal(|ui| {
        key_ui(
            ui,
            &tr!(
                i18n,
                "Session",
                "Dave session inspector usage row: cost and turns so far"
            ),
        );
        let text = labels
            .totals
            .get(usage.generation(), i18n, |_, i18n| totals_text(usage, i18n));
        value_ui(ui, text);
    });
    sparkline_ui(usage, context_window, i18n, ui);
    compactions_ui(usage, labels, i18n, ui);
}

/// The context as one bar the width of the card: cache read, cache write
/// and uncached input stacked against the window.
fn context_bar_ui(tokens: &Tokens, context_window: u64, ui: &mut egui::Ui) {
    let size = egui::vec2(ui.available_width(), BAR_HEIGHT);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let radius = notedeck::tokens::RADIUS_SM;
    painter.rect_filled(rect, radius, ui.visuals().faint_bg_color);

    let per_token = rect.width() / context_window.max(1) as f32;
    let mut x = rect.left();
    for kind in [Kind::CacheRead, Kind::CacheWrite, Kind::Input] {
        let width = (kind.tokens(tokens) as f32 * per_token).min(rect.right() - x);
        if width <= 0.0 {
            continue;
        }
        let segment =
            egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(width, BAR_HEIGHT));
        painter.rect_filled(segment, 0.0, kind.color());
        x += width;
    }
    painter.rect_stroke(
        rect,
        radius,
        ui.visuals().widgets.noninteractive.bg_stroke,
        egui::StrokeKind::Inside,
    );
}

/// A swatch and a count for each kind of token.
fn legend_ui(
    tokens: &Tokens,
    labels: &mut UsageLabels,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) {
    ui.horizontal_wrapped(|ui| {
        for kind in Kind::ALL {
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(SWATCH, SWATCH), egui::Sense::hover());
            ui.painter().rect_filled(rect, 2.0, kind.color());
            let text = kind
                .memo(labels)
                .get(kind.tokens(tokens), i18n, |n, i18n| kind.label(n, i18n));
            weak_ui(ui, text);
        }
    });
}

/// Context at the end of each turn against the window, with a mark where
/// the conversation was compacted. Hovering a point shows its turn.
fn sparkline_ui(
    usage: &UsageHistory,
    context_window: u64,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) {
    let spark = usage.sparkline();
    if spark.values.is_empty() {
        return;
    }
    ui.add_space(notedeck::tokens::SPACING_XS);
    let size = egui::vec2(ui.available_width(), SPARKLINE_HEIGHT);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    painter.rect_filled(rect, notedeck::tokens::RADIUS_SM, visuals.faint_bg_color);

    let plot = rect.shrink(2.0);
    let scale = spark.max.max(context_window as f32).max(1.0);
    let window_y = y_at(plot, context_window as f32, scale);
    painter.hline(
        plot.x_range(),
        window_y,
        egui::Stroke::new(notedeck::tokens::STROKE_THIN, visuals.weak_text_color()),
    );

    let compaction =
        egui::Stroke::new(notedeck::tokens::STROKE_THICK, notedeck::tokens::PALETTE[2]);
    for &marker in &spark.markers {
        painter.vline(x_at(plot, marker, spark), plot.y_range(), compaction);
    }

    let line = egui::Stroke::new(notedeck::tokens::STROKE_MEDIUM, Kind::CacheRead.color());
    let point = |i: usize| {
        egui::pos2(
            x_at(plot, i as f32, spark),
            y_at(plot, spark.values[i], scale),
        )
    };
    if spark.values.len() == 1 {
        painter.circle_filled(point(0), 2.0, line.color);
    }
    for i in 1..spark.values.len() {
        painter.line_segment([point(i - 1), point(i)], line);
    }

    let Some(pos) = resp.hover_pos() else {
        return;
    };
    let i = nearest_index(plot, pos.x, spark);
    painter.circle_filled(point(i), 3.0, line.color);
    resp.on_hover_text_at_pointer(turn_text(usage, i, i18n));
}

/// Where value index `i` (fractional for a marker) sits across the plot.
/// A single turn sits in the middle.
fn x_at(plot: egui::Rect, i: f32, spark: &Sparkline) -> f32 {
    let last = spark.values.len().saturating_sub(1);
    if last == 0 {
        return plot.center().x;
    }
    let t = (i / last as f32).clamp(0.0, 1.0);
    plot.left() + t * plot.width()
}

fn y_at(plot: egui::Rect, value: f32, scale: f32) -> f32 {
    plot.bottom() - (value / scale).clamp(0.0, 1.0) * plot.height()
}

/// The turn nearest an x position on the plot.
fn nearest_index(plot: egui::Rect, x: f32, spark: &Sparkline) -> usize {
    let last = spark.values.len().saturating_sub(1);
    if last == 0 || plot.width() <= 0.0 {
        return 0;
    }
    let t = ((x - plot.left()) / plot.width()).clamp(0.0, 1.0);
    (t * last as f32).round() as usize
}

/// The newest compactions, one line each.
fn compactions_ui(
    usage: &UsageHistory,
    labels: &mut UsageLabels,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) {
    if usage.compactions().is_empty() {
        return;
    }
    let heading = labels
        .compactions_heading
        .get(usage.compactions().len(), i18n, |n, i18n| {
            tr_plural!(
                i18n,
                "{count} compaction",
                "{count} compactions",
                "Dave session inspector usage: heading over the list of conversation compactions",
                n as i64,
            )
        });
    heading_ui(ui, heading);
    let lines = labels.compactions.get(usage.generation(), i18n, |_, i18n| {
        compaction_lines(usage, i18n)
    });
    weak_ui(ui, lines);
}

/// "123K / 200K · 62%": numbers only, like the status bar's meter, so
/// nothing to translate.
fn context_text((context, window): (u64, u64), _i18n: &mut Localization) -> String {
    let used = format_tokens(context);
    let window_text = format_tokens(window);
    let percent = context * 100 / window.max(1);
    format!("{used} / {window_text} · {percent}%")
}

/// "$1.23 · 14 turns · 57 requests"
fn totals_text(usage: &UsageHistory, i18n: &mut Localization) -> String {
    let mut parts = Vec::with_capacity(3);
    if let Some(cost) = usage.total_cost() {
        parts.push(format!("${cost:.2}"));
    }
    parts.push(tr_plural!(
        i18n,
        "{count} turn",
        "{count} turns",
        "Dave session inspector usage: turns the session has finished",
        usage.turns_recorded() as i64,
    ));
    parts.push(tr_plural!(
        i18n,
        "{count} request",
        "{count} requests",
        "Dave session inspector usage: model requests across the session's turns",
        usage.requests() as i64,
    ));
    parts.join(" · ")
}

/// "after turn 12 · 180K → 24K", newest first. Numbered like the
/// sparkline's hover, so a line finds its marker.
fn compaction_lines(usage: &UsageHistory, i18n: &mut Localization) -> String {
    let mut lines = Vec::with_capacity(COMPACTIONS_SHOWN);
    for compaction in usage.compactions().iter().rev().take(COMPACTIONS_SHOWN) {
        let turn = compaction.turn.to_string();
        let before = format_tokens(compaction.pre_tokens);
        lines.push(match compaction.post_tokens {
            Some(after) => {
                let after = format_tokens(after);
                tr!(
                    i18n,
                    "after turn {turn} · {before} → {after}",
                    "Dave session inspector usage: a compaction, the turn it followed and the context before and after",
                    turn = turn.as_str(),
                    before = before.as_str(),
                    after = after.as_str()
                )
            }
            None => tr!(
                i18n,
                "after turn {turn} · from {before}",
                "Dave session inspector usage: a compaction whose resulting context isn't known yet",
                turn = turn.as_str(),
                before = before.as_str()
            ),
        });
    }
    lines.join("\n")
}

/// The hover line for sparkline point `i`: "Turn 12 · 120K · $0.14".
fn turn_text(usage: &UsageHistory, i: usize, i18n: &mut Localization) -> String {
    let Some(turn) = usage.turns().get(i) else {
        return String::new();
    };
    let number = usage.turns_recorded() - usage.turns().len() as u64 + i as u64 + 1;
    let number = number.to_string();
    let context = format_tokens(turn.tokens.context());
    let line = tr!(
        i18n,
        "Turn {number} · {context}",
        "Dave session inspector usage: hover over one turn on the context sparkline",
        number = number.as_str(),
        context = context.as_str()
    );
    match turn.cost_usd {
        Some(cost) => format!("{line} · ${cost:.2}"),
        None => line,
    }
}
