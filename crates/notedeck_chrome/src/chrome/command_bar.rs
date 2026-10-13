//! The Cmd-K command bar: a query field over a filtered list of
//! [`CommandItem`]s, floating over every app.
//!
//! Cmd-K (Ctrl-K off macOS) opens it whatever has focus, and Cmd-K again or
//! Esc closes it, as does a click outside it. Typing filters the rows with
//! [`fuzzy_match`]; ↑/↓ or Ctrl-P/Ctrl-N move the selection and Enter runs
//! it, as a click on a row does. While it is open the keymap stands down, so
//! a key typed into the query never runs a binding.
//!
//! The bar only lists and runs rows. Where they come from is the caller's:
//! [`CommandBar::set_items`] hands it a list, which it filters each time the
//! query changes, never per frame. Each row's text is laid out once per
//! filter, so a frame with the bar open only draws.
//!
//! A chosen row can ask for more before it runs: a tool's argument (see
//! [`super::command_tools`]). The bar then steps into a new level, its prompt
//! drawn before the query: another list to pick from, or the query itself as
//! the argument's text. Backspace in an empty query steps back out.

use std::ops::Range;
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Galley, Key, Modifiers, Sense, Stroke, Vec2};
use notedeck::fuzzy::{contains_words, fuzzy_match};
use notedeck::theme::selection_fill;
use notedeck::tokens::{RADIUS_LG, RADIUS_MD, SPACING_LG, SPACING_MD, SPACING_SM, STROKE_THIN};
use notedeck::{
    tr, AppAction, AppContext, ColorTheme, CommandAction, CommandItem, Localization,
    ToolInvocation, ToolOutcome, ToolSpec,
};
use notedeck_ui::keybind_hint::{keycap_width, KeybindHint, KEYCAP};

use super::actions::chrome_handle_app_action;
use super::key_dispatch::tool_action;
use super::Chrome;

/// Opens the command bar. Bound to Cmd-K by the chrome itself, so it works
/// with a text field focused; the tool is for the keymap and for agents.
pub const OPEN_COMMAND_BAR: &str = "chrome_open_command_bar";

/// The command bar's UI tools, for the host's tool registry.
pub(super) fn command_bar_tools() -> [ToolSpec; 1] {
    [ToolSpec::new(
        OPEN_COMMAND_BAR,
        "Open the command bar, to find and run a command.",
        Vec::new(),
    )
    .titled("Open command bar")]
}

/// The widest the panel gets; a narrower window gets a panel
/// [`SPACING_LG`] in from each side.
const PANEL_WIDTH: f32 = 640.0;

/// The tallest the result list gets before it scrolls.
const LIST_HEIGHT: f32 = 420.0;

/// The height of one result row.
const ROW_HEIGHT: f32 = 36.0;

/// The height of a section heading.
const HEADING_HEIGHT: f32 = 28.0;

/// The query field's text size.
const QUERY_SIZE: f32 = 18.0;

/// A row title's text size.
const TITLE_SIZE: f32 = 14.0;

/// The size of a row's subtitle and a section heading.
const SMALL_SIZE: f32 = 12.0;

/// How dark the backdrop dims the window behind the bar.
const BACKDROP_ALPHA: u8 = 128;

/// How strongly the selected row is tinted with the selection colour: a
/// full-strength fill would drown the title's highlighted matches.
const SELECTED_TINT: f32 = 0.35;

/// The score of a row matched only by its [keywords](CommandItem::keywords):
/// below every row whose title matched.
const KEYWORD_ONLY: i32 = i32::MIN;

/// The query field's id, for focusing it.
fn field_id() -> egui::Id {
    egui::Id::unique("chrome-command-bar-query")
}

/// One row the query matched, in the order the bar lists them.
struct Row {
    /// Its index in [`CommandBar::items`].
    item: usize,
    /// How well it matched (see [`fuzzy_match`]).
    score: i32,
    /// Its matched byte offsets in its title, a range of
    /// [`CommandBar::positions`].
    matched: Range<usize>,
    /// Where its section sorts: the rank of the section's best row.
    section_rank: usize,
}

/// A row's text, laid out once per filter rather than per frame.
struct RowText {
    /// The section heading drawn above it, when it is its section's first
    /// row.
    heading: Option<Arc<Galley>>,
    /// Its title, matched characters highlighted, then its subtitle.
    line: Arc<Galley>,
}

/// What a level of the bar asks for.
#[derive(Default)]
enum Asking {
    /// One of its rows.
    #[default]
    Row,
    /// Text typed into the query, for one argument of a tool call.
    Text(TextArg),
}

/// A tool call waiting on one argument, typed into the query.
pub(super) struct TextArg {
    /// The call so far.
    pub invocation: ToolInvocation,
    /// The argument the query fills.
    pub arg: &'static str,
    /// Whether the text must be a number, and goes in as one.
    pub number: bool,
}

impl TextArg {
    /// The call with the argument set to `text`, or `None` when the text
    /// isn't a value it takes: empty, or not a number when it must be one.
    fn filled(&self, text: &str) -> Option<ToolInvocation> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let value = if self.number {
            let n: f64 = text.parse().ok()?;
            match n.fract() == 0.0 && n.abs() < i64::MAX as f64 {
                true => serde_json::Value::from(n as i64),
                false => serde_json::Value::from(n),
            }
        } else {
            serde_json::Value::from(text)
        };
        Some(with_arg(&self.invocation, self.arg, value))
    }
}

/// `invocation` with its argument `arg` set to `value`, its other args kept.
pub(super) fn with_arg(
    invocation: &ToolInvocation,
    arg: &str,
    value: serde_json::Value,
) -> ToolInvocation {
    let mut args = match &invocation.args {
        serde_json::Value::Object(args) => args.clone(),
        _ => serde_json::Map::new(),
    };
    args.insert(arg.to_owned(), value);
    ToolInvocation::new(invocation.name.clone(), serde_json::Value::Object(args))
}

/// A level the bar stepped into a deeper one from, kept to step back to.
struct Level {
    /// Its prompt, if it had one.
    prompt: Option<String>,
    /// Its rows.
    items: Vec<CommandItem>,
    /// What had been typed into it.
    query: String,
    /// What it asked for.
    asking: Asking,
    /// Its query field's placeholder.
    hint: String,
}

/// The prompt drawn before the query, laid out once per level.
struct PromptText {
    /// What it was laid out for.
    key: LayoutKey,
    /// The prompt.
    galley: Arc<Galley>,
}

/// What [`RowText`]s were laid out for: a change in any of them lays them
/// out again.
#[derive(Clone, Copy, PartialEq)]
struct LayoutKey {
    /// Dark or light mode, for the colours.
    dark_mode: bool,
    /// The scale, for the glyphs.
    pixels_per_point: f32,
    /// The panel's width, for where a long title is cut off.
    width: f32,
}

/// The command bar's state: whether it is open, its query, the rows it
/// was given, and which of them the query matches.
#[derive(Default)]
pub struct CommandBar {
    /// Whether it is showing.
    open: bool,
    /// Whether the query field has yet to take focus since it opened.
    focus_pending: bool,
    /// What had focus when it opened, to give it back on close.
    return_focus: Option<egui::Id>,
    /// What has been typed.
    query: String,
    /// Every row it can list.
    items: Vec<CommandItem>,
    /// Whether [`rows`](Self::rows) is out of date with the query or items.
    stale: bool,
    /// The rows the query matches, best section first, best row first
    /// within it.
    rows: Vec<Row>,
    /// Every row's matched byte offsets, end to end (see [`Row::matched`]).
    positions: Vec<usize>,
    /// [`fuzzy_match`]'s buffer, reused across rows.
    scratch: Vec<usize>,
    /// Each row's laid-out text, parallel to `rows`; empty until drawn.
    texts: Vec<RowText>,
    /// What `texts` was laid out for.
    laid_out_for: Option<LayoutKey>,
    /// The index in `rows` of the selected row.
    selected: usize,
    /// Whether the list should scroll the selected row into view, after a
    /// key moved it.
    reveal_selected: bool,
    /// The query field's placeholder, translated as the bar opens.
    hint: String,
    /// What it says when nothing matches, translated as the bar opens.
    no_results: String,
    /// What it says while the query is a tool argument's text, translated
    /// as the bar opens.
    enter_to_run: String,
    /// What this level asks for, drawn before the query; `None` at the top.
    prompt: Option<String>,
    /// [`prompt`](Self::prompt), laid out.
    prompt_text: Option<PromptText>,
    /// What this level asks for: a row, or text.
    asking: Asking,
    /// The levels it stepped in from, innermost last.
    back: Vec<Level>,
    /// The tools chosen from it lately, by name, latest first.
    recent: Vec<String>,
}

/// How many tools [`CommandBar::recent`] remembers.
const RECENT_TOOLS: usize = 5;

impl CommandBar {
    /// Whether it is showing.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// What has been typed into it.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Replace what has been typed, as if the user had typed it.
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.stale = true;
    }

    /// Replace the rows it can list; the query filters them.
    pub fn set_items(&mut self, items: Vec<CommandItem>) {
        self.items = items;
        self.stale = true;
    }

    /// The rows the query matches, in the order it lists them.
    pub fn matches(&mut self) -> impl Iterator<Item = &CommandItem> {
        self.refilter_if_stale();
        self.rows.iter().map(|row| &self.items[row.item])
    }

    /// The selected row, if anything matches.
    pub fn selected(&mut self) -> Option<&CommandItem> {
        self.refilter_if_stale();
        self.rows
            .get(self.selected)
            .map(|row| &self.items[row.item])
    }

    /// The prompt of the level it is at, if it stepped into one: the tool
    /// whose argument it is asking for.
    pub fn prompt(&self) -> Option<&str> {
        self.prompt.as_deref()
    }

    /// The tools chosen from it lately, by name, latest first.
    pub fn recent(&self) -> impl Iterator<Item = &str> {
        self.recent.iter().map(String::as_str)
    }

    /// Remember that the tool `name` was chosen, for listing it first next
    /// time.
    pub(super) fn remember(&mut self, name: &str) {
        self.recent.retain(|recent| recent != name);
        self.recent.insert(0, name.to_owned());
        self.recent.truncate(RECENT_TOOLS);
    }

    /// Step into a level that asks for one of `items`, under `prompt`, with
    /// `hint` as the query's placeholder. Backspace in its empty query steps
    /// back.
    pub(super) fn ask_row(&mut self, prompt: String, hint: String, items: Vec<CommandItem>) {
        self.step_in(prompt, hint, Asking::Row, items);
    }

    /// Step into a level that asks for `arg`'s text, typed into the query,
    /// under `prompt`, with `hint` as the query's placeholder.
    pub(super) fn ask_text(&mut self, prompt: String, hint: String, arg: TextArg) {
        self.step_in(prompt, hint, Asking::Text(arg), Vec::new());
    }

    /// Keep this level to step back to and start a new one.
    fn step_in(&mut self, prompt: String, hint: String, asking: Asking, items: Vec<CommandItem>) {
        self.back.push(Level {
            prompt: self.prompt.replace(prompt),
            items: std::mem::replace(&mut self.items, items),
            query: std::mem::take(&mut self.query),
            asking: std::mem::replace(&mut self.asking, asking),
            hint: std::mem::replace(&mut self.hint, hint),
        });
        self.prompt_text = None;
        self.stale = true;
    }

    /// Step back out to the level it stepped in from, its query as it was
    /// left. Returns false at the top.
    fn step_out(&mut self) -> bool {
        let Some(level) = self.back.pop() else {
            return false;
        };
        self.prompt = level.prompt;
        self.items = level.items;
        self.query = level.query;
        self.asking = level.asking;
        self.hint = level.hint;
        self.prompt_text = None;
        self.stale = true;
        true
    }

    /// Show it with an empty query and nothing to list yet; the query field
    /// takes focus the next time it is drawn.
    fn open(&mut self, i18n: &mut Localization) {
        self.open = true;
        self.focus_pending = true;
        self.query.clear();
        self.items.clear();
        self.back.clear();
        self.prompt = None;
        self.prompt_text = None;
        self.asking = Asking::Row;
        self.stale = true;
        self.hint = tr!(
            i18n,
            "Type a command or search…",
            "Placeholder in the command bar's query field"
        );
        self.no_results = tr!(
            i18n,
            "No results",
            "Shown in the command bar when nothing matches the query"
        );
        self.enter_to_run = tr!(
            i18n,
            "Press Enter to run it",
            "Shown in the command bar while the query is the text a command needs"
        );
    }

    /// Hide it and give focus back to what had it when it opened.
    pub(super) fn close(&mut self, ctx: &egui::Context) {
        self.open = false;
        self.focus_pending = false;
        self.items.clear();
        self.rows.clear();
        self.texts.clear();
        self.back.clear();
        self.prompt = None;
        self.prompt_text = None;
        self.asking = Asking::Row;
        let return_focus = self.return_focus.take();
        ctx.memory_mut(|m| match return_focus {
            Some(id) => m.request_focus(id),
            None => m.surrender_focus(field_id()),
        });
    }

    /// Move the selection `by` rows, stopping at either end.
    fn move_selection(&mut self, by: isize) {
        let last = self.rows.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(by).min(last);
        self.reveal_selected = true;
    }

    /// What choosing now does: the selected row's action, or, while the
    /// query is a tool argument's text, the call with it filled in. `None`
    /// when there is nothing to choose. Leaves the bar open: the caller
    /// closes it or steps it into the next argument.
    fn choose(&mut self) -> Option<CommandAction> {
        if let Asking::Text(arg) = &self.asking {
            return arg.filled(&self.query).map(CommandAction::RunTool);
        }
        self.refilter_if_stale();
        self.rows
            .get(self.selected)
            .map(|row| self.items[row.item].action.clone())
    }

    /// [`refilter`](Self::refilter), if the query or items changed since.
    fn refilter_if_stale(&mut self) {
        if self.stale {
            self.refilter();
        }
    }

    /// Match the query against every row's title, then its keywords, and
    /// order the matches: sections by their best row, rows by score within
    /// them, ties in the order they were given (so an empty query lists the
    /// rows as given, grouped by section). Selects the first. Allocates only
    /// to grow its buffers.
    fn refilter(&mut self) {
        self.stale = false;
        self.rows.clear();
        self.positions.clear();
        self.texts.clear();
        self.laid_out_for = None;
        self.selected = 0;
        self.reveal_selected = true;

        for (item, entry) in self.items.iter().enumerate() {
            let score = match fuzzy_match(&self.query, &entry.title, &mut self.scratch) {
                Some(score) => score,
                None if entry
                    .keywords
                    .as_deref()
                    .is_some_and(|keywords| contains_words(&self.query, keywords)) =>
                {
                    KEYWORD_ONLY
                }
                None => continue,
            };
            let start = self.positions.len();
            self.positions.extend_from_slice(&self.scratch);
            self.rows.push(Row {
                item,
                score,
                matched: start..self.positions.len(),
                section_rank: 0,
            });
        }
        self.rows.sort_by_key(|row| std::cmp::Reverse(row.score));

        // A section ranks where its best row does: the first row of it in
        // score order.
        for i in 0..self.rows.len() {
            let section = &self.items[self.rows[i].item].section;
            let first = self.rows[..i]
                .iter()
                .position(|row| self.items[row.item].section == *section)
                .unwrap_or(i);
            self.rows[i].section_rank = first;
        }
        self.rows.sort_by_key(|row| row.section_rank);
    }

    /// Lay out every row's text for `key`, unless it already is.
    fn lay_out(&mut self, ctx: &egui::Context, theme: &ColorTheme, key: LayoutKey) {
        if self.laid_out_for == Some(key) && self.texts.len() == self.rows.len() {
            return;
        }
        self.laid_out_for = Some(key);
        self.texts.clear();
        let mut section: Option<&str> = None;
        for row in &self.rows {
            let item = &self.items[row.item];
            let new_section = section != Some(item.section.as_ref());
            section = Some(item.section.as_ref());
            // An unnamed section, like a tool argument's values, has none.
            let heading = (new_section && !item.section.is_empty()).then(|| {
                let mut job = LayoutJob::single_section(
                    item.section.to_string(),
                    TextFormat::simple(FontId::proportional(SMALL_SIZE), theme.text_muted),
                );
                job.wrap.max_width = key.width;
                job.wrap.max_rows = 1;
                ctx.fonts_mut(|f| f.layout_job(job))
            });
            let keycap = item
                .keycap
                .as_deref()
                .map_or(0.0, |k| keycap_width(k) + SPACING_MD);
            let job = row_job(
                item,
                &self.positions[row.matched.clone()],
                theme,
                key.width - keycap,
            );
            let line = ctx.fonts_mut(|f| f.layout_job(job));
            self.texts.push(RowText { heading, line });
        }
    }

    /// Draw it over everything, if it is open, handing back what a clicked
    /// row does. A click outside it closes it.
    #[profiling::function]
    fn show(&mut self, ctx: &egui::Context) -> Option<CommandAction> {
        if !self.open {
            return None;
        }
        self.refilter_if_stale();
        if std::mem::take(&mut self.focus_pending) {
            self.return_focus = ctx.memory(|m| m.focused());
            ctx.memory_mut(|m| m.request_focus(field_id()));
        }

        let theme = ColorTheme::current(ctx);
        let screen = ctx.content_rect();
        let width = (screen.width() - 2.0 * SPACING_LG).min(PANEL_WIDTH);
        let top = screen.top() + (screen.height() * 0.15).max(SPACING_LG);
        let panel = egui::Rect::from_min_max(
            egui::pos2(screen.center().x - width / 2.0, top),
            egui::pos2(
                screen.center().x + width / 2.0,
                screen.bottom() - SPACING_LG,
            ),
        );
        let row_width = width - 2.0 * SPACING_SM;
        let layout = LayoutKey {
            dark_mode: ctx.global_style().visuals.dark_mode,
            pixels_per_point: ctx.pixels_per_point(),
            width: row_width - 2.0 * SPACING_MD,
        };

        let mut clicked = None;
        let mut close = false;
        let area = egui::Area::new(egui::Id::unique("chrome-command-bar"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .constrain(false)
            .show(ctx, |ui| {
                let backdrop = ui.allocate_rect(screen, Sense::click());
                ui.painter()
                    .rect_filled(screen, 0.0, Color32::from_black_alpha(BACKDROP_ALPHA));

                let drawn = ui
                    .scope_builder(egui::UiBuilder::new().max_rect(panel), |ui| {
                        egui::Frame::new()
                            .fill(theme.surface_elevated)
                            .stroke(Stroke::new(STROKE_THIN, theme.border_default))
                            .corner_radius(RADIUS_LG)
                            .shadow(ui.visuals().popup_shadow)
                            .show(ui, |ui| {
                                ui.set_width(width);
                                self.query_ui(ui, &theme, layout);
                                let y = ui.cursor().top();
                                ui.painter().hline(
                                    ui.max_rect().x_range(),
                                    y,
                                    Stroke::new(STROKE_THIN, theme.border_default),
                                );
                                clicked = self.results_ui(ui, &theme, row_width, layout);
                            })
                    })
                    .inner
                    .response;

                let outside = backdrop
                    .interact_pointer_pos()
                    .is_some_and(|pos| !drawn.rect.contains(pos));
                close = backdrop.clicked() && outside;
            });
        ctx.move_to_top(area.response.layer_id);

        if let Some(row) = clicked {
            self.selected = row;
            return self.choose();
        }
        if close {
            self.close(ctx);
        }
        None
    }

    /// The query field, after the level's prompt if it has one, refiltering
    /// when it changes. It keeps focus while the bar is open.
    fn query_ui(&mut self, ui: &mut egui::Ui, theme: &ColorTheme, layout: LayoutKey) {
        let Some(prompt) = self.prompt_galley(ui.ctx(), theme, layout) else {
            self.query_field_ui(ui);
            return;
        };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(SPACING_LG);
            let padding = egui::vec2(SPACING_SM, SPACING_SM / 2.0);
            let chip = prompt.size() + 2.0 * padding;
            // As tall as the query field, so the chip centres on its text.
            let field_height = ui.fonts_mut(|f| f.row_height(&FontId::proportional(QUERY_SIZE)))
                + 2.0 * SPACING_MD;
            let (slot, _) = ui.allocate_exact_size(
                egui::vec2(chip.x + SPACING_SM, field_height),
                Sense::hover(),
            );
            let rect = egui::Rect::from_min_size(
                egui::pos2(slot.left(), slot.center().y - chip.y / 2.0),
                chip,
            );
            ui.painter().rect_filled(
                rect,
                RADIUS_MD,
                selection_fill(ui.visuals()).gamma_multiply(SELECTED_TINT),
            );
            ui.painter()
                .galley(rect.min + padding, prompt, theme.text_primary);
            self.query_field_ui(ui);
        });
    }

    /// The level's prompt, laid out for `key`; laid out again only when the
    /// prompt or `key` changed. `None` at the top level.
    fn prompt_galley(
        &mut self,
        ctx: &egui::Context,
        theme: &ColorTheme,
        key: LayoutKey,
    ) -> Option<Arc<Galley>> {
        let prompt = self.prompt.as_deref()?;
        if let Some(text) = self.prompt_text.as_ref().filter(|text| text.key == key) {
            return Some(text.galley.clone());
        }
        let mut job = LayoutJob::single_section(
            prompt.to_owned(),
            TextFormat::simple(FontId::proportional(TITLE_SIZE), theme.text_primary),
        );
        job.wrap.max_width = key.width / 2.0;
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        let galley = ctx.fonts_mut(|f| f.layout_job(job));
        self.prompt_text = Some(PromptText {
            key,
            galley: galley.clone(),
        });
        Some(galley)
    }

    /// The query field itself.
    fn query_field_ui(&mut self, ui: &mut egui::Ui) {
        let field = egui::TextEdit::singleline(&mut self.query)
            .id(field_id())
            .frame(
                egui::Frame::NONE
                    .inner_margin(egui::Margin::symmetric(SPACING_LG as i8, SPACING_MD as i8)),
            )
            .hint_text(self.hint.as_str())
            .font(FontId::proportional(QUERY_SIZE))
            .desired_width(f32::INFINITY);
        let response = ui.add(field);
        if response.changed() {
            self.refilter();
        }
        if !response.has_focus() {
            response.request_focus();
        }
    }

    /// The matched rows under their section headings, or a line saying
    /// nothing matched. Returns the index of a clicked row. Lays the rows'
    /// text out only when the query, rows or layout changed; otherwise it
    /// only draws.
    fn results_ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &ColorTheme,
        row_width: f32,
        layout: LayoutKey,
    ) -> Option<usize> {
        // Here rather than before the query field, which refilters as it
        // changes.
        self.lay_out(ui.ctx(), theme, layout);
        let empty = match self.asking {
            Asking::Text(_) => Some(self.enter_to_run.as_str()),
            Asking::Row if self.rows.is_empty() => Some(self.no_results.as_str()),
            Asking::Row => None,
        };
        if let Some(empty) = empty {
            egui::Frame::new()
                .inner_margin(egui::Margin::same(SPACING_LG as i8))
                .show(ui, |ui| {
                    ui.colored_label(theme.text_muted, empty);
                });
            return None;
        }

        let Self {
            rows,
            items,
            texts,
            selected,
            reveal_selected,
            ..
        } = self;
        let pointer_moved = ui.input(|i| i.pointer.delta() != Vec2::ZERO);
        let selected_fill = selection_fill(ui.visuals()).gamma_multiply(SELECTED_TINT);
        let max_height = (ui.max_rect().bottom() - ui.cursor().top()).min(LIST_HEIGHT);
        let mut clicked = None;
        egui::Frame::new()
            .inner_margin(egui::Margin::same(SPACING_SM as i8))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("chrome-command-bar-results")
                    .max_height(max_height - 2.0 * SPACING_SM)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for (i, (row, text)) in rows.iter().zip(texts.iter()).enumerate() {
                            if let Some(heading) = &text.heading {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(row_width, HEADING_HEIGHT),
                                    Sense::hover(),
                                );
                                let pos = egui::pos2(
                                    rect.left() + SPACING_MD,
                                    rect.bottom() - heading.size().y - SPACING_SM / 2.0,
                                );
                                ui.painter().galley(pos, heading.clone(), theme.text_muted);
                            }

                            let (rect, response) = ui.allocate_exact_size(
                                egui::vec2(row_width, ROW_HEIGHT),
                                Sense::click(),
                            );
                            if response.hovered() && pointer_moved {
                                *selected = i;
                            }
                            if i == *selected {
                                ui.painter().rect_filled(rect, RADIUS_MD, selected_fill);
                                if std::mem::take(reveal_selected) {
                                    response.scroll_to_me(None);
                                }
                            }
                            let pos = egui::pos2(
                                rect.left() + SPACING_MD,
                                rect.center().y - text.line.size().y / 2.0,
                            );
                            ui.painter()
                                .galley(pos, text.line.clone(), theme.text_primary);
                            if let Some(keycap) = items[row.item].keycap.as_deref() {
                                let w = keycap_width(keycap);
                                KeybindHint::new(keycap).size(KEYCAP).width(w).paint_at(
                                    ui,
                                    egui::pos2(
                                        rect.right() - SPACING_MD - w / 2.0,
                                        rect.center().y,
                                    ),
                                );
                            }
                            if response.clicked() {
                                clicked = Some(i);
                            }
                        }
                    });
            });
        clicked
    }
}

/// A row's line: its title, the `matched` byte offsets in the accent colour,
/// then its subtitle, small and muted, all cut off with `…` at `width`.
fn row_job(item: &CommandItem, matched: &[usize], theme: &ColorTheme, width: f32) -> LayoutJob {
    let title = TextFormat::simple(FontId::proportional(TITLE_SIZE), theme.text_primary);
    let hit = TextFormat::simple(FontId::proportional(TITLE_SIZE), theme.accent);
    let mut job = LayoutJob::default();
    let mut at = 0;
    let mut matched = matched.iter().copied().peekable();
    while at < item.title.len() {
        let (end, format) = match matched.peek() {
            Some(&m) if m == at => {
                // A run of adjacent matched characters.
                let mut end = at;
                while let Some(&m) = matched.peek().filter(|&&m| m == end) {
                    end = m + item.title[m..].chars().next().map_or(1, char::len_utf8);
                    matched.next();
                }
                (end, &hit)
            }
            Some(&m) => (m, &title),
            None => (item.title.len(), &title),
        };
        job.append(&item.title[at..end], 0.0, format.clone());
        at = end;
    }
    if let Some(subtitle) = &item.subtitle {
        job.append(
            subtitle,
            SPACING_SM,
            TextFormat::simple(FontId::proportional(SMALL_SIZE), theme.text_muted),
        );
    }
    job.wrap.max_width = width.max(0.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job
}

impl Chrome {
    /// The command bar.
    pub fn command_bar(&self) -> &CommandBar {
        &self.command_bar
    }

    /// The command bar, to give it rows or a query.
    pub fn command_bar_mut(&mut self) -> &mut CommandBar {
        &mut self.command_bar
    }

    /// Open the command bar, or close it if it is open. Bound to Cmd-K.
    pub(super) fn toggle_command_bar(&mut self, app_ctx: &mut AppContext, ctx: &egui::Context) {
        if self.command_bar.open {
            self.command_bar.close(ctx);
        } else {
            self.command_bar.open(app_ctx.i18n);
            self.list_tools(app_ctx);
        }
    }

    /// The keys the open command bar takes before the app renders, so the
    /// app never sees them: Esc closes it, ↑/↓ and Ctrl-P/Ctrl-N move the
    /// selection, Enter runs it, and Backspace in an empty query steps back
    /// out of an argument. Typed text is left for its query field.
    pub(super) fn command_bar_keys(&mut self, app_ctx: &mut AppContext, ui: &mut egui::Ui) {
        if !self.command_bar.open {
            return;
        }
        let ctx = ui.ctx().clone();
        let (mut escape, mut up, mut down, mut enter) = (false, false, false, false);
        let mut back = false;
        let can_step_out = self.command_bar.query.is_empty() && !self.command_bar.back.is_empty();
        ctx.input_mut(|i| {
            escape = i.consume_key(Modifiers::NONE, Key::Escape);
            back = can_step_out && i.consume_key(Modifiers::NONE, Key::Backspace);
            up = i.consume_key(Modifiers::NONE, Key::ArrowUp)
                | consume_exact(i, Modifiers::CTRL, Key::P);
            down = i.consume_key(Modifiers::NONE, Key::ArrowDown)
                | consume_exact(i, Modifiers::CTRL, Key::N);
            enter = i.consume_key(Modifiers::NONE, Key::Enter);
        });
        if escape {
            self.command_bar.close(&ctx);
            return;
        }
        if back {
            self.command_bar.step_out();
        }
        if up {
            self.command_bar.move_selection(-1);
        }
        if down {
            self.command_bar.move_selection(1);
        }
        if !enter {
            return;
        }
        match self.command_bar.choose() {
            Some(action) => self.run_command(app_ctx, action, ui),
            // Nothing to run: an empty list closes, an argument's text not
            // typed yet waits for it.
            None if matches!(self.command_bar.asking, Asking::Row) => self.command_bar.close(&ctx),
            None => {}
        }
    }

    /// Draw the command bar over everything, if it is open, and run a row
    /// clicked in it.
    pub(super) fn command_bar_ui(&mut self, app_ctx: &mut AppContext, ui: &mut egui::Ui) {
        if let Some(action) = self.command_bar.show(ui.ctx()) {
            self.run_command(app_ctx, action, ui);
        }
    }

    /// Do what a chosen row does: step into the next argument its tool asks
    /// for, or close the bar and run the tool as a key bound to it would, or
    /// open its reference. Closing first lets a tool that opens a view land
    /// on it.
    fn run_command(&mut self, app_ctx: &mut AppContext, action: CommandAction, ui: &mut egui::Ui) {
        let action = match action {
            CommandAction::RunTool(invocation) => {
                if self.ask_next_arg(app_ctx, &invocation) {
                    return;
                }
                self.command_bar.close(ui.ctx());
                self.command_bar.remember(&invocation.name);
                let invocation = self.fill_current_args(app_ctx, invocation);
                tool_action(app_ctx, invocation)
            }
            CommandAction::Open(uri) => {
                self.command_bar.close(ui.ctx());
                Some(AppAction::Open(uri))
            }
        };
        if let Some(action) = action {
            chrome_handle_app_action(self, app_ctx, action, ui);
        }
    }

    /// Run the command bar's own UI tool `invocation`, or `None` when it
    /// isn't one.
    pub(super) fn call_command_bar_tool(
        &mut self,
        ctx: &mut AppContext,
        invocation: &ToolInvocation,
    ) -> Option<ToolOutcome> {
        if invocation.name != OPEN_COMMAND_BAR {
            return None;
        }
        if !self.command_bar.open {
            self.command_bar.open(ctx.i18n);
            self.list_tools(ctx);
        }
        Some(ToolOutcome::Data(serde_json::json!({ "open": true })))
    }

    /// Whether the command bar's own UI tool `name` would do something now,
    /// or `None` when `name` isn't one.
    pub(super) fn command_bar_tool_available(&self, name: &str) -> Option<bool> {
        (name == OPEN_COMMAND_BAR).then_some(!self.command_bar.open)
    }
}

/// Consume a press of `key` with exactly `modifiers` held: unlike
/// [`egui::InputState::consume_key`], an extra Shift or Alt doesn't match,
/// so Cmd-K leaves Cmd-Shift-K alone.
pub(super) fn consume_exact(input: &mut egui::InputState, modifiers: Modifiers, key: Key) -> bool {
    let before = input.events.len();
    input.events.retain(|event| {
        !matches!(
            event,
            egui::Event::Key { key: k, pressed: true, modifiers: m, .. }
                if *k == key && m.matches_exact(modifiers)
        )
    });
    input.events.len() != before
}
