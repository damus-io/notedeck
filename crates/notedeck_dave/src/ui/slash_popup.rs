//! The slash-command popup above Dave's chat input: the rows of a
//! [`SlashPopup`], drawn in a floating area as wide as the input.
//!
//! It draws only theme visuals (`window_fill`, `window_stroke`,
//! `popup_shadow`, the selection hue), so it follows the dark and light
//! themes with no colours of its own.

use egui::text::{LayoutJob, TextWrapping};
use egui::{Align2, Color32, FontId, Rect, Sense, TextFormat};
use notedeck::{tr, Localization};

use crate::slash::{
    Ranked, SlashCatalog, SlashKey, SlashOutcome, SlashPopup, Stage, MAX_VISIBLE_ROWS,
};

/// A row's height.
const ROW_HEIGHT: f32 = 26.0;
/// The gap between the popup's bottom edge and the input.
const GAP_ABOVE_INPUT: f32 = 6.0;
/// A row's text inset from its left edge.
const ROW_PAD: f32 = 8.0;
/// Where the description column starts, from the row's left edge, unless the
/// name runs past it.
const COMMAND_DESCRIPTION_AT: f32 = 250.0;
const MODEL_DESCRIPTION_AT: f32 = 200.0;
/// The least room between a name and its description.
const NAME_GAP: f32 = 14.0;
/// The text size of the name, hint and description.
const ROW_TEXT: f32 = 13.0;
/// The text size of the group label and the footer.
const SMALL_TEXT: f32 = 11.0;

/// Draw `popup` above `input_rect` if it is open, and apply a click on one
/// of its rows to `input`. Returns what the click did.
pub fn slash_popup_ui(
    popup: &mut SlashPopup,
    input: &mut String,
    current_model: &str,
    input_rect: Rect,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) -> Option<SlashOutcome> {
    if !popup.is_open() {
        return None;
    }
    let visuals = ui.visuals().clone();
    let frame = egui::Frame::NONE
        .fill(visuals.window_fill)
        .stroke(visuals.window_stroke)
        .shadow(visuals.popup_shadow)
        .corner_radius(notedeck::tokens::RADIUS_MD)
        .inner_margin(egui::Margin::same(6));
    let inner_width = input_rect.width() - 12.0 - 2.0 * visuals.window_stroke.width;

    egui::Area::new(egui::Id::unique("dave_slash_popup"))
        .order(egui::Order::Foreground)
        .fixed_pos(input_rect.left_top() - egui::vec2(0.0, GAP_ABOVE_INPUT))
        .pivot(Align2::LEFT_BOTTOM)
        .show(ui.ctx(), |ui| {
            frame
                .show(ui, |ui| {
                    ui.set_width(inner_width);
                    ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                    popup_body(popup, input, current_model, i18n, ui)
                })
                .inner
        })
        .inner
}

/// The popup's group label, rows and footer.
fn popup_body(
    popup: &mut SlashPopup,
    input: &mut String,
    current_model: &str,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) -> Option<SlashOutcome> {
    let stage = popup.stage()?;
    group_label(popup.catalog(), stage, ui);

    if popup.is_loading() {
        let text = tr!(
            i18n,
            "loading commands…",
            "Dave slash-command popup row shown while the list of commands loads"
        );
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_HEIGHT), Sense::hover());
        ui.painter().text(
            rect.left_center() + egui::vec2(ROW_PAD, 0.0),
            Align2::LEFT_CENTER,
            text,
            FontId::proportional(ROW_TEXT),
            ui.visuals().weak_text_color(),
        );
        return None;
    }

    let scroll_to_selected = popup.take_scroll_to_selected();
    let event = egui::ScrollArea::vertical()
        .max_height(ROW_HEIGHT * MAX_VISIBLE_ROWS as f32)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            let catalog = popup.catalog()?;
            let pointer_moved = ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO);
            let mut event = None;
            for (row, ranked) in popup.matches().iter().enumerate() {
                let selected = row == popup.selected();
                let response = match stage {
                    Stage::Command => command_row(catalog, ranked, selected, ui),
                    Stage::Model => model_row(catalog, ranked, selected, current_model, ui),
                };
                if selected && scroll_to_selected {
                    response.scroll_to_me(None);
                }
                if response.clicked() {
                    event = Some(RowEvent::Click(row));
                } else if response.hovered() && pointer_moved && !selected {
                    // Only a moving pointer takes the selection, so one
                    // resting over the popup doesn't fight the arrow keys.
                    event = Some(RowEvent::Hover(row));
                }
            }
            event
        })
        .inner;

    footer(popup, stage, i18n, ui);

    match event? {
        RowEvent::Hover(row) => {
            popup.select(row);
            None
        }
        RowEvent::Click(row) => {
            popup.select(row);
            popup.press(SlashKey::Enter, input)
        }
    }
}

/// What the pointer did to a row this frame.
enum RowEvent {
    Hover(usize),
    Click(usize),
}

/// The small-caps label over the rows: the backend's name, or in the model
/// stage `MODEL · <backend>`.
fn group_label(catalog: Option<&SlashCatalog>, stage: Stage, ui: &mut egui::Ui) {
    let weak = ui.visuals().weak_text_color();
    let font = FontId::proportional(SMALL_TEXT);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), Sense::hover());
    let at = rect.left_center() + egui::vec2(ROW_PAD, 0.0);
    let source = catalog.map_or("", |c| c.source);
    match stage {
        Stage::Command => {
            let label = catalog.map_or("", |c| c.group_label);
            ui.painter()
                .text(at, Align2::LEFT_CENTER, label, font, weak);
        }
        Stage::Model => {
            let model = ui
                .painter()
                .text(at, Align2::LEFT_CENTER, "MODEL", font.clone(), weak);
            ui.painter().text(
                egui::pos2(model.right() + 6.0, at.y),
                Align2::LEFT_CENTER,
                "·  ",
                font.clone(),
                weak,
            );
            ui.painter().text(
                egui::pos2(model.right() + 16.0, at.y),
                Align2::LEFT_CENTER,
                source,
                font,
                weak,
            );
        }
    }
}

/// One command: `/name` and its argument hint in monospace, then its
/// description in a column.
fn command_row(
    catalog: &SlashCatalog,
    ranked: &Ranked,
    selected: bool,
    ui: &mut egui::Ui,
) -> egui::Response {
    let command = &catalog.commands[ranked.index];
    let response = row_rect(selected, ui);
    let rect = response.rect;
    let visuals = ui.visuals();
    let mono = FontId::monospace(ROW_TEXT);

    let mut name = LayoutJob::default();
    append(&mut name, "/", &mono, visuals.strong_text_color(), false);
    append_matched(
        &mut name,
        &command.name,
        &ranked.span,
        &mono,
        visuals.strong_text_color(),
        visuals.hyperlink_color,
    );
    if !command.argument_hint.is_empty() {
        append(&mut name, " ", &mono, visuals.weak_text_color(), false);
        append(
            &mut name,
            &command.argument_hint,
            &mono,
            visuals.weak_text_color(),
            false,
        );
    }
    let name_end = paint_job(name, rect, ROW_PAD, ui);
    paint_description(
        &command.description,
        rect,
        COMMAND_DESCRIPTION_AT.max(name_end + NAME_GAP),
        rect.right(),
        ui,
    );
    response
}

/// One model: its display name, then its value (the alias `/model` takes)
/// in weak monospace, its description in a column, and a tick when it is the
/// session's model.
fn model_row(
    catalog: &SlashCatalog,
    ranked: &Ranked,
    selected: bool,
    current_model: &str,
    ui: &mut egui::Ui,
) -> egui::Response {
    let model = &catalog.models[ranked.index];
    let response = row_rect(selected, ui);
    let rect = response.rect;
    let visuals = ui.visuals();
    let accent = visuals.hyperlink_color;
    let no_span = 0..0;
    let (name_span, value_span) = if ranked.on_value {
        (&no_span, &ranked.span)
    } else {
        (&ranked.span, &no_span)
    };

    let mut name = LayoutJob::default();
    append_matched(
        &mut name,
        &model.display_name,
        name_span,
        &FontId::proportional(ROW_TEXT),
        visuals.strong_text_color(),
        accent,
    );
    // The value says what to type; a display name that already is it (an
    // id with no alias) shows once.
    if model.value != model.display_name {
        let mono = FontId::monospace(ROW_TEXT);
        append(&mut name, "  ", &mono, visuals.weak_text_color(), false);
        append_matched(
            &mut name,
            &model.value,
            value_span,
            &mono,
            visuals.weak_text_color(),
            accent,
        );
    }
    let name_end = paint_job(name, rect, ROW_PAD, ui);

    let tick_width = 24.0;
    let is_current = model.value.eq_ignore_ascii_case(current_model);
    if is_current {
        ui.painter().text(
            rect.right_center() - egui::vec2(ROW_PAD, 0.0),
            Align2::RIGHT_CENTER,
            "✔",
            FontId::proportional(ROW_TEXT),
            notedeck::tokens::STATUS_IN_REVIEW,
        );
    }
    paint_description(
        &model.description,
        rect,
        MODEL_DESCRIPTION_AT.max(name_end + NAME_GAP),
        rect.right() - tick_width,
        ui,
    );
    response
}

/// Allocate a row, filled when it is the selected one.
fn row_rect(selected: bool, ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
    if selected {
        ui.painter().rect_filled(
            rect,
            notedeck::tokens::RADIUS_SM,
            notedeck::theme::selection_fill(ui.visuals()),
        );
    }
    response
}

/// Paint `job` on one line at `inset` into `rect`, returning the x its text
/// ends at, relative to the row's left edge.
fn paint_job(job: LayoutJob, rect: Rect, inset: f32, ui: &egui::Ui) -> f32 {
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let width = galley.size().x;
    let pos = egui::pos2(rect.left() + inset, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(pos, galley, Color32::PLACEHOLDER);
    inset + width
}

/// Paint `text` weak, on one line from `from` (relative to the row's left
/// edge) to `right`, cut with an ellipsis where it doesn't fit.
fn paint_description(text: &str, rect: Rect, from: f32, right: f32, ui: &egui::Ui) {
    let width = right - (rect.left() + from) - ROW_PAD;
    if width <= 0.0 || text.is_empty() {
        return;
    }
    let mut job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat::simple(
            FontId::proportional(ROW_TEXT),
            ui.visuals().weak_text_color(),
        ),
    );
    job.wrap = TextWrapping::truncate_at_width(width);
    paint_job(job, rect, from, ui);
}

/// Append `text` to `job` in `font` and `color`, underlined when `underline`.
fn append(job: &mut LayoutJob, text: &str, font: &FontId, color: Color32, underline: bool) {
    let mut format = TextFormat::simple(font.clone(), color);
    if underline {
        format.underline = egui::Stroke::new(1.0, color);
    }
    job.append(text, 0.0, format);
}

/// Append `text` with its `span` in `accent` and underlined: the part of it
/// the query matched.
fn append_matched(
    job: &mut LayoutJob,
    text: &str,
    span: &std::ops::Range<usize>,
    font: &FontId,
    color: Color32,
    accent: Color32,
) {
    let Some(matched) = text.get(span.clone()).filter(|m| !m.is_empty()) else {
        append(job, text, font, color, false);
        return;
    };
    append(job, &text[..span.start], font, color, false);
    append(job, matched, font, accent, true);
    append(job, &text[span.end..], font, color, false);
}

/// The key hints, and how many rows sit below the fold.
fn footer(popup: &SlashPopup, stage: Stage, i18n: &mut Localization, ui: &mut egui::Ui) {
    let weak = ui.visuals().weak_text_color();
    let font = FontId::proportional(SMALL_TEXT);
    let hints = match stage {
        Stage::Command => tr!(
            i18n,
            "↑↓ select · Tab complete · Enter run · Esc close",
            "Key hints under Dave's slash-command list"
        ),
        Stage::Model => tr!(
            i18n,
            "↑↓ select · Enter switch · Esc back to commands",
            "Key hints under Dave's /model list of models"
        ),
    };
    ui.add_space(4.0);
    let line_y = ui.cursor().top();
    ui.painter().hline(
        ui.max_rect().x_range(),
        line_y,
        ui.visuals().widgets.noninteractive.bg_stroke,
    );
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), Sense::hover());
    ui.painter().text(
        rect.left_center() + egui::vec2(ROW_PAD, 0.0),
        Align2::LEFT_CENTER,
        hints,
        font.clone(),
        weak,
    );
    let hidden = popup.matches().len().saturating_sub(MAX_VISIBLE_ROWS);
    if hidden > 0 {
        let more = tr!(
            i18n,
            "+{count} more",
            "How many more rows Dave's slash-command list has below the visible ones",
            count = hidden
        );
        ui.painter().text(
            rect.right_center() - egui::vec2(ROW_PAD, 0.0),
            Align2::RIGHT_CENTER,
            more,
            font,
            weak,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::slash_popup_ui;
    use crate::slash::{CatalogView, SlashCatalog, SlashPopup};
    use crate::ui::dave::input_fill;
    use crate::ui::{InputMode, InputboxLayout};
    use egui_kittest::Harness;
    use notedeck::test_harness::PressKey;
    use notedeck::Localization;
    use std::rc::Rc;

    /// A catalog shaped like the Claude CLI's `initialize` response, cut
    /// down to fit a render: a few built-ins, one skill, and the models.
    fn catalog() -> Rc<SlashCatalog> {
        Rc::new(SlashCatalog::from_server_info(&serde_json::json!({
            "commands": [
                {"name": "headway", "description": "Read and edit a Headway kanban board (user)", "argumentHint": ""},
                {"name": "code-review", "description": "Review the current diff for correctness bugs", "argumentHint": "[low|medium|high]"},
                {"name": "clear", "description": "Clear conversation history and free up context", "argumentHint": ""},
                {"name": "compact", "description": "Summarize the conversation to free context", "argumentHint": "<instructions>"},
                {"name": "context", "description": "Show current context usage", "argumentHint": ""},
                {"name": "effort", "description": "Set effort level for model usage", "argumentHint": "<low|medium|high|max>"},
                {"name": "model", "description": "Set the AI model for Claude Code", "argumentHint": "<model>"},
                {"name": "simplify", "description": "Review changed code for reuse and simplification", "argumentHint": "[<target>]"},
                {"name": "usage", "description": "Show plan usage limits", "argumentHint": ""},
                {"name": "mcp", "description": "Manage MCP servers", "argumentHint": ""},
            ],
            "models": [
                {"value": "default", "displayName": "Default (recommended)", "description": "Opus 5.5 · Best for everyday, complex tasks"},
                {"value": "opus", "displayName": "Opus 5.5", "description": "For complex work and everyday tasks"},
                {"value": "fable", "displayName": "Fable 5.1", "description": "For your toughest challenges"},
                {"value": "sonnet", "displayName": "Sonnet 5.5", "description": "Most efficient for simpler tasks"},
                {"value": "haiku", "displayName": "Haiku 5.5", "description": "Fastest for quick answers"},
            ],
        })))
    }

    /// Render the input holding `text` at the bottom of a chat-sized panel,
    /// with its popup open above it, under `visuals`. The harness runs in
    /// the matching theme, since the popup is an `Area` and takes the
    /// context's visuals, not the input's `Ui`'s.
    fn render(name: &str, text: &str, visuals: egui::Visuals) {
        let mut popup = SlashPopup::default();
        popup.set_catalog(CatalogView::Ready(catalog()));
        let mut text = text.to_owned();
        let theme = if visuals.dark_mode {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        };
        let mut harness = notedeck::test_harness::with_notedeck_style(
            Harness::builder()
                .with_size(egui::Vec2::new(640.0, 440.0))
                .with_theme(theme)
                .renderer(notedeck::software_renderer())
                .build_ui_state(
                    move |ui, i18n: &mut Localization| {
                        // Fonts set during a pass load at the start of the next,
                        // so the first pass only installs notedeck's: egui's own
                        // lack the footer's arrows, and drawing them would panic.
                        if ui.ctx().cumulative_pass_nr() == 0 {
                            notedeck::fonts::setup_fonts(ui.ctx());
                            return;
                        }
                        ui.style_mut().visuals = visuals.clone();
                        ui.painter()
                            .rect_filled(ui.max_rect(), 0.0, visuals.panel_fill);
                        popup.sync(&text);
                        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                            let input = egui::Frame::new()
                                .inner_margin(egui::Margin::same(
                                    notedeck::tokens::SPACING_SM as i8,
                                ))
                                .fill(input_fill(ui.visuals(), false))
                                .corner_radius(notedeck::tokens::RADIUS_LG)
                                .show(ui, |ui| {
                                    InputboxLayout::new(&mut text, i18n, InputMode::Insert)
                                        .claim_popup_keys(true)
                                        .show(ui)
                                });
                            slash_popup_ui(
                                &mut popup,
                                &mut text,
                                "default",
                                input.response.rect,
                                i18n,
                                ui,
                            );
                        });
                    },
                    Localization::default(),
                ),
        );
        harness.run();
        harness.snapshot(name);
    }

    // The popup's four states, the design's renders 1–4 (headway:dave/position-illegal-spare).

    /// Every command: the built-ins, then the skill.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_slash_popup_commands() {
        render("slash_popup_commands", "/", notedeck::theme::dark_mode());
    }

    /// Filtered: prefix matches first, the matched span underlined.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_slash_popup_filtered() {
        render("slash_popup_filtered", "/co", notedeck::theme::dark_mode());
    }

    /// `/model`'s list, the current model ticked.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_slash_popup_models() {
        render(
            "slash_popup_models",
            "/model ",
            notedeck::theme::dark_mode(),
        );
    }

    /// That list filtered by alias, in the light theme.
    #[test]
    #[ignore] // requires lavapipe — run via scripts/snapshot-test
    fn snapshot_slash_popup_models_filtered_light() {
        render(
            "slash_popup_models_filtered_light",
            "/model son",
            notedeck::theme::light_mode(),
        );
    }

    /// What the key-flow harness holds between frames.
    struct KeyFlow {
        popup: SlashPopup,
        text: String,
        /// Every outcome the key step returned, in order.
        outcomes: Vec<crate::slash::SlashOutcome>,
    }

    const INPUT: &str = "slash_key_flow_input";

    /// A real `InputboxLayout` with the popup's key step run ahead of it each
    /// frame, the way `Dave::render` runs it ahead of the keybindings.
    fn key_flow(text: &str) -> Harness<'static, KeyFlow> {
        let mut popup = SlashPopup::default();
        popup.set_catalog(CatalogView::Ready(catalog()));
        let state = KeyFlow {
            popup,
            text: text.to_owned(),
            outcomes: Vec::new(),
        };
        let mut harness = Harness::new_ui_state(
            |ui, state: &mut KeyFlow| {
                let id = egui::Id::unique(INPUT);
                if let Some(outcome) =
                    crate::slash::popup_keys(&mut state.popup, &mut state.text, ui.ctx(), id)
                {
                    state.outcomes.push(outcome);
                }
                InputboxLayout::new_default(&mut state.text)
                    .claim_popup_keys(state.popup.claims_keys())
                    .id(id)
                    .show(ui);
            },
            state,
        );
        harness
            .ctx
            .memory_mut(|m| m.request_focus(egui::Id::unique(INPUT)));
        harness.run();
        harness
    }

    fn focused(harness: &Harness<'_, KeyFlow>) -> bool {
        harness.ctx.memory(|m| m.has_focus(egui::Id::unique(INPUT)))
    }

    /// Tab completes a command and keeps focus in the input (egui would
    /// otherwise move focus on Tab before any handler runs); the model stage
    /// follows, the arrows move through it, and Enter runs `/model <value>`.
    #[test]
    fn tab_completes_and_enter_runs_through_the_real_input() {
        use crate::slash::SlashOutcome;
        use egui::{Key, Modifiers};

        let mut harness = key_flow("/mod");
        harness.press_key_modifiers(Modifiers::NONE, Key::Tab);
        harness.run();
        assert_eq!(harness.state().text, "/model ");
        assert!(focused(&harness), "Tab must not move focus off the input");

        // Default, Opus, Fable, then Sonnet.
        for _ in 0..3 {
            harness.press_key_modifiers(Modifiers::NONE, Key::ArrowDown);
        }
        harness.press_key_modifiers(Modifiers::NONE, Key::Enter);
        harness.run();
        let state = harness.state();
        assert_eq!(state.text, "/model sonnet");
        assert_eq!(state.outcomes.last(), Some(&SlashOutcome::Run));
    }

    /// The first Esc closes the popup and keeps focus and text; the second is
    /// not the popup's, so egui drops focus (which is how Dave enters normal
    /// mode).
    #[test]
    fn escape_closes_the_popup_then_leaves_the_input() {
        use egui::{Key, Modifiers};

        let mut harness = key_flow("/co");
        assert!(harness.state().popup.is_open());
        harness.press_key_modifiers(Modifiers::NONE, Key::Escape);
        harness.run();
        assert!(!harness.state().popup.is_open());
        assert_eq!(harness.state().text, "/co");
        assert!(focused(&harness), "the first Esc only closes the popup");

        harness.press_key_modifiers(Modifiers::NONE, Key::Escape);
        harness.run();
        assert!(!focused(&harness), "the second Esc leaves the input");
    }

    /// Shift+Enter is a newline even with the popup open.
    #[test]
    fn shift_enter_is_not_the_popups() {
        use egui::{Key, Modifiers};

        let mut harness = key_flow("/co");
        harness.press_key_modifiers(Modifiers::SHIFT, Key::Enter);
        harness.run();
        assert!(harness.state().outcomes.is_empty());
        assert!(harness.state().text.contains('\n'));
    }
}
