//! Slash-command autocomplete for the chat input.
//!
//! Typing `/` opens a list of the commands the session's backend knows,
//! filtered as you type; `/model ` opens a second list, the models it offers.
//! Dave adds no commands of its own: the list is the Claude CLI's
//! `initialize.commands[]`, and every command is sent as prompt text, which
//! the CLI runs itself in print mode (design: headway:dave/position-illegal-spare).
//!
//! The pieces:
//! - [`SlashCatalog`]: a backend's commands and models.
//! - [`SlashCatalogs`]: the catalogs Dave knows, probed once per cwd.
//! - [`SlashPopup`]: one session's popup — what it matches and which row is
//!   selected — reseeded when the input text changes, never per frame.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use poll_promise::Promise;
use serde_json::Value;

use crate::backend::BackendType;

/// The command whose argument is picked from [`SlashCatalog::models`].
pub const MODEL_COMMAND: &str = "model";

/// The command Dave keeps for itself: the CLI's `/clear` moves it to a new
/// session id, while Dave keys a session's events, d-tag and restore on the
/// old one, so Dave runs its own Clear instead.
pub const CLEAR_COMMAND: &str = "clear";

/// Rows the popup shows before it scrolls.
pub const MAX_VISIBLE_ROWS: usize = 8;

/// One command the backend runs, as its `initialize` response lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommand {
    /// The name, without the leading `/`.
    pub name: String,
    pub description: String,
    /// What the command takes, as the CLI writes it (`<model>`,
    /// `[low|medium|high]`). Empty for a command that takes nothing, which
    /// Enter runs at once.
    pub argument_hint: String,
}

/// One model the backend offers for `/model`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashModel {
    /// What `/model` takes: an alias (`sonnet`) or an id.
    pub value: String,
    /// `Sonnet 5.5`, `Default (recommended)`.
    pub display_name: String,
    pub description: String,
}

/// A backend's slash commands and models.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlashCatalog {
    /// The backend's name, for the model stage's label.
    pub source: &'static str,
    /// The command stage's group label: the name in small caps.
    pub group_label: &'static str,
    /// Built-ins first, in the CLI's order, then skills.
    pub commands: Vec<SlashCommand>,
    pub models: Vec<SlashModel>,
}

impl SlashCatalog {
    /// The catalog in a Claude CLI's `initialize` response (what
    /// `ClaudeClient::get_server_info` returns). Lenient: a missing or
    /// misshapen field comes out empty.
    pub fn from_server_info(info: &Value) -> Self {
        let field = |v: &Value, key: &str| {
            v.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let list = |key: &str| {
            info.get(key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        };

        let mut commands: Vec<SlashCommand> = list("commands")
            .filter_map(|c| {
                let name = field(c, "name");
                (!name.is_empty()).then(|| SlashCommand {
                    name,
                    description: field(c, "description"),
                    argument_hint: field(c, "argumentHint"),
                })
            })
            .collect();
        // A stable sort: built-ins keep the CLI's order, as do skills.
        commands.sort_by_key(|c| is_skill(&c.description));

        let models = list("models")
            .filter_map(|m| {
                let value = field(m, "value");
                (!value.is_empty()).then(|| SlashModel {
                    value,
                    display_name: field(m, "displayName"),
                    description: field(m, "description"),
                })
            })
            .collect();

        SlashCatalog {
            source: "Claude Code",
            group_label: "CLAUDE CODE",
            commands,
            models,
        }
    }

    /// Codex has no slash commands of its own, so its sessions get only
    /// `/model`, which Dave handles (see [`crate::Dave`]'s send path). Its
    /// model list is not fetched yet, so the value is typed.
    pub fn codex() -> Self {
        SlashCatalog {
            source: "Codex",
            group_label: "CODEX",
            commands: vec![SlashCommand {
                name: MODEL_COMMAND.to_owned(),
                description: "Switch this session's model".to_owned(),
                argument_hint: "<model>".to_owned(),
            }],
            models: Vec::new(),
        }
    }

    /// Whether `command` takes its argument from a list the popup can show.
    fn has_argument_list(&self, command: &str) -> bool {
        command == MODEL_COMMAND && !self.models.is_empty()
    }
}

/// Whether a command is a skill rather than a built-in. The CLI marks where
/// a skill was loaded from at the end of its description: `(user)`,
/// `(project)`, `(plugin:…)`. Skills bundled with the CLI carry no mark and
/// sort with the built-ins they ship beside.
fn is_skill(description: &str) -> bool {
    let d = description.trim_end();
    d.ends_with("(user)") || d.ends_with("(project)") || d.contains("(plugin:")
}

/// The command and argument a `/` input names, if it names one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlashInput<'a> {
    /// The command, without the `/`.
    pub command: &'a str,
    /// Everything after the first space, or `None` before one is typed.
    pub argument: Option<&'a str>,
}

impl<'a> SlashInput<'a> {
    /// Read a draft as a slash command: one line starting with `/`.
    pub fn parse(input: &'a str) -> Option<Self> {
        let rest = input.strip_prefix('/')?;
        if rest.contains('\n') {
            return None;
        }
        Some(match rest.split_once(' ') {
            Some((command, argument)) => SlashInput {
                command,
                argument: Some(argument),
            },
            None => SlashInput {
                command: rest,
                argument: None,
            },
        })
    }
}

/// One row the popup shows: an index into the catalog's commands (or models,
/// in the model stage) and the span of its text the query matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranked {
    pub index: usize,
    /// Byte range of the match in the row's name (or the model's display
    /// name); empty when the query is.
    pub span: Range<usize>,
    /// The model stage matched the model's `value`, not its display name.
    pub on_value: bool,
}

/// Which list the popup shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Commands, filtered by name.
    Command,
    /// `/model`'s argument: models, filtered by display name and value.
    Model,
}

/// What a key or click does to the popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashKey {
    Up,
    Down,
    /// Tab: complete the selected row into the input.
    Complete,
    /// Enter: run the selected row, or complete one that takes an argument.
    Enter,
    /// Esc: close the popup, or step back from the model stage.
    Back,
}

/// What a key did, for the caller to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashOutcome {
    /// Only the selection or the popup's visibility moved.
    Moved,
    /// The input was rewritten; the cursor belongs at its end.
    Edited,
    /// The input holds a whole command to send now.
    Run,
}

/// A session's catalog as the popup sees it. Cheap to make every frame: a
/// ready catalog is shared, not copied.
#[derive(Debug, Clone, Default)]
pub enum CatalogView {
    Ready(Rc<SlashCatalog>),
    /// Being probed: the popup shows a loading row.
    Loading,
    /// None to be had (a chat-mode session, or the probe failed): no popup.
    #[default]
    Unavailable,
}

impl CatalogView {
    /// The catalog, once there is one.
    pub fn ready(&self) -> Option<&SlashCatalog> {
        match self {
            CatalogView::Ready(catalog) => Some(catalog),
            CatalogView::Loading | CatalogView::Unavailable => None,
        }
    }

    /// Identifies the catalog, so a change of catalog reranks.
    fn key(&self) -> usize {
        match self {
            CatalogView::Ready(catalog) => Rc::as_ptr(catalog) as usize,
            CatalogView::Loading => 1,
            CatalogView::Unavailable => 0,
        }
    }
}

/// One session's slash popup.
///
/// The key step hands it the session's catalog each frame
/// ([`set_catalog`](Self::set_catalog)), so the chat view draws it from the
/// session alone. [`sync`](Self::sync) reranks only when the input text or
/// the catalog changed since the last call, reusing its buffers, so calling
/// it every frame allocates nothing.
#[derive(Debug, Default)]
pub struct SlashPopup {
    /// The commands and models the rows index into.
    catalog: CatalogView,
    /// The input text the matches were ranked for.
    seeded: String,
    /// The catalog they were ranked against ([`CatalogView::key`]), so a
    /// catalog that arrives while the popup is open reranks it.
    seeded_catalog: usize,
    /// Arrow keys moved the selection; the view scrolls it into sight once.
    scroll_to_selected: bool,
    /// The catalog is still loading.
    loading: bool,
    stage: Option<Stage>,
    matches: Vec<Ranked>,
    selected: usize,
    /// Esc closed the popup; it stays closed until the text changes.
    dismissed: bool,
}

impl SlashPopup {
    /// Use `catalog` from now on; the next [`sync`](Self::sync) reranks
    /// against it if it is a different one.
    pub fn set_catalog(&mut self, catalog: CatalogView) {
        if catalog.key() != self.catalog.key() {
            self.catalog = catalog;
        }
    }

    /// The catalog the rows index into, once it has loaded.
    pub fn catalog(&self) -> Option<&SlashCatalog> {
        self.catalog.ready()
    }

    /// Rerank for `input`, if it or the catalog changed since the last call.
    pub fn sync(&mut self, input: &str) {
        let catalog_key = self.catalog.key();
        let text_changed = self.seeded != input;
        if !text_changed && self.seeded_catalog == catalog_key {
            return;
        }
        if text_changed {
            self.seeded.clear();
            self.seeded.push_str(input);
            self.selected = 0;
            self.dismissed = false;
        }
        self.seeded_catalog = catalog_key;
        self.matches.clear();
        self.stage = None;
        self.loading = false;

        let Some(slash) = SlashInput::parse(input) else {
            return;
        };
        let catalog = match &self.catalog {
            CatalogView::Ready(catalog) => &**catalog,
            CatalogView::Unavailable => return,
            CatalogView::Loading => {
                // Show the loading row while a command is typed.
                if slash.argument.is_none() {
                    self.stage = Some(Stage::Command);
                    self.loading = true;
                }
                return;
            }
        };
        match slash.argument {
            None => {
                self.stage = Some(Stage::Command);
                rank_into(
                    &mut self.matches,
                    slash.command,
                    catalog.commands.iter().map(|c| c.name.as_str()),
                );
            }
            Some(arg) if catalog.has_argument_list(slash.command) => {
                self.stage = Some(Stage::Model);
                rank_models_into(&mut self.matches, arg, &catalog.models);
            }
            Some(_) => {}
        }
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
    }

    /// The list showing, if the popup is open.
    pub fn stage(&self) -> Option<Stage> {
        self.stage.filter(|_| !self.dismissed)
    }

    /// Whether the popup is showing: it has rows, or is loading them.
    pub fn is_open(&self) -> bool {
        self.stage().is_some() && (!self.matches.is_empty() || self.loading)
    }

    /// Whether the popup takes the keys it claims ([`take_key`]): it has
    /// rows to move through. A loading popup leaves Enter to send the text.
    pub fn claims_keys(&self) -> bool {
        self.stage().is_some() && !self.matches.is_empty()
    }

    /// Whether the catalog is still loading.
    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// The rows, best match first.
    pub fn matches(&self) -> &[Ranked] {
        &self.matches
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Point the selection at row `row` (hover does this).
    pub fn select(&mut self, row: usize) {
        if row < self.matches.len() {
            self.selected = row;
        }
    }

    /// Apply `key` to the popup and `input`. `None` when the popup has no
    /// rows, so the key is not the popup's (see [`claims_keys`](Self::claims_keys)).
    pub fn press(&mut self, key: SlashKey, input: &mut String) -> Option<SlashOutcome> {
        if !self.claims_keys() {
            return None;
        }
        let stage = self.stage()?;
        if key == SlashKey::Back {
            if stage == Stage::Model {
                input.clear();
                input.push('/');
                input.push_str(MODEL_COMMAND);
                return Some(SlashOutcome::Edited);
            }
            self.dismissed = true;
            return Some(SlashOutcome::Moved);
        }
        let rows = self.matches.len();
        match key {
            SlashKey::Up => self.selected = (self.selected + rows - 1) % rows,
            SlashKey::Down => self.selected = (self.selected + 1) % rows,
            SlashKey::Complete | SlashKey::Enter => {
                return Some(self.accept(stage, key == SlashKey::Enter, input))
            }
            SlashKey::Back => {}
        }
        self.scroll_to_selected = true;
        Some(SlashOutcome::Moved)
    }

    /// Whether the selection moved by key since the last call, so the view
    /// should scroll it into sight.
    pub fn take_scroll_to_selected(&mut self) -> bool {
        std::mem::take(&mut self.scroll_to_selected)
    }

    /// Write the selected row into `input`, and say whether it runs now.
    fn accept(&self, stage: Stage, enter: bool, input: &mut String) -> SlashOutcome {
        let Some(catalog) = self.catalog.ready() else {
            return SlashOutcome::Moved;
        };
        let index = self.matches[self.selected].index;
        input.clear();
        input.push('/');
        match stage {
            Stage::Command => {
                let command = &catalog.commands[index];
                input.push_str(&command.name);
                if enter && command.argument_hint.is_empty() {
                    return SlashOutcome::Run;
                }
                input.push(' ');
                SlashOutcome::Edited
            }
            Stage::Model => {
                input.push_str(MODEL_COMMAND);
                input.push(' ');
                input.push_str(&catalog.models[index].value);
                if enter {
                    SlashOutcome::Run
                } else {
                    SlashOutcome::Edited
                }
            }
        }
    }
}

/// Rank `names` against `query` into `out`: name-prefix matches first, then
/// substring matches, each in list order. ASCII case is ignored.
fn rank_into<'a>(out: &mut Vec<Ranked>, query: &str, names: impl Iterator<Item = &'a str> + Clone) {
    for prefix_pass in [true, false] {
        for (index, name) in names.clone().enumerate() {
            let Some(at) = find_ignore_ascii_case(name, query) else {
                continue;
            };
            if (at == 0) == prefix_pass {
                out.push(Ranked {
                    index,
                    span: at..at + query.len(),
                    on_value: false,
                });
            }
        }
    }
}

/// [`rank_into`] for models, matching the display name and then the value.
fn rank_models_into(out: &mut Vec<Ranked>, query: &str, models: &[SlashModel]) {
    for prefix_pass in [true, false] {
        for (index, model) in models.iter().enumerate() {
            let hit = find_ignore_ascii_case(&model.display_name, query)
                .map(|at| (at, false))
                .or_else(|| find_ignore_ascii_case(&model.value, query).map(|at| (at, true)));
            let Some((at, on_value)) = hit else {
                continue;
            };
            if (at == 0) == prefix_pass {
                out.push(Ranked {
                    index,
                    span: at..at + query.len(),
                    on_value,
                });
            }
        }
    }
}

/// The byte offset of the first match of `needle` in `hay`, ignoring ASCII
/// case. Offsets fall on char boundaries, since a match starts where an
/// equal (so equally long) char does.
fn find_ignore_ascii_case(hay: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    let (hay_b, needle_b) = (hay.as_bytes(), needle.as_bytes());
    if needle_b.len() > hay_b.len() {
        return None;
    }
    (0..=hay_b.len() - needle_b.len())
        .filter(|&at| hay.is_char_boundary(at))
        .find(|&at| hay_b[at..at + needle_b.len()].eq_ignore_ascii_case(needle_b))
}

/// A cwd's catalog: probing, probed, or failed to probe.
enum CatalogSlot {
    Loading(Promise<Option<SlashCatalog>>),
    Ready(Rc<SlashCatalog>),
    /// The probe failed; the popup stays closed rather than loading forever.
    Failed,
}

/// The slash catalogs Dave knows: one probed Claude catalog per cwd, and
/// Codex's fixed one.
///
/// A connected session's own catalog (from its CLI's `initialize`, see
/// [`DaveApiResponse::ServerInfo`](crate::messages::DaveApiResponse::ServerInfo))
/// replaces its cwd's, so a probe only fills the gap before the first turn.
pub struct SlashCatalogs {
    claude: HashMap<PathBuf, CatalogSlot>,
    codex: Rc<SlashCatalog>,
}

impl Default for SlashCatalogs {
    fn default() -> Self {
        SlashCatalogs {
            claude: HashMap::new(),
            codex: Rc::new(SlashCatalog::codex()),
        }
    }
}

impl SlashCatalogs {
    /// The catalog `session`'s popup shows: Codex's, the session's own CLI's,
    /// or its cwd's probed one. Chat-mode sessions get none.
    pub fn view(&self, session: &crate::session::ChatSession) -> CatalogView {
        let Some(agentic) = session.agentic.as_ref() else {
            return CatalogView::Unavailable;
        };
        match session.backend_type {
            BackendType::Codex => return CatalogView::Ready(self.codex.clone()),
            BackendType::OpenAI => return CatalogView::Unavailable,
            BackendType::Claude | BackendType::Remote => {}
        }
        if let Some(live) = &session.slash_catalog {
            return CatalogView::Ready(live.clone());
        }
        match self.claude.get(&agentic.cwd) {
            Some(CatalogSlot::Ready(catalog)) => CatalogView::Ready(catalog.clone()),
            Some(CatalogSlot::Failed) => CatalogView::Unavailable,
            Some(CatalogSlot::Loading(_)) | None => CatalogView::Loading,
        }
    }

    /// Probe `session`'s cwd once its draft starts a slash command and it has
    /// no catalog yet, and take a finished probe's result.
    pub fn poll(&mut self, session: &crate::session::ChatSession, repaint: &egui::Context) {
        let Some(agentic) = session.agentic.as_ref() else {
            return;
        };
        let wants = session.input.starts_with('/')
            && session.slash_catalog.is_none()
            && matches!(
                session.backend_type,
                BackendType::Claude | BackendType::Remote
            );
        if wants || self.claude.contains_key(&agentic.cwd) {
            self.poll_claude(&agentic.cwd, repaint);
        }
    }

    /// Start probing `cwd`'s Claude catalog unless it is cached or in
    /// flight; take a finished probe's result. `repaint` is asked to redraw
    /// when the probe finishes.
    fn poll_claude(&mut self, cwd: &Path, repaint: &egui::Context) {
        let slot = match self.claude.get_mut(cwd) {
            Some(slot) => slot,
            None => {
                let promise = probe_claude(cwd.to_path_buf(), repaint.clone());
                self.claude
                    .entry(cwd.to_path_buf())
                    .or_insert(CatalogSlot::Loading(promise))
            }
        };
        let CatalogSlot::Loading(promise) = slot else {
            return;
        };
        let Some(result) = promise.ready() else {
            return;
        };
        *slot = match result {
            Some(catalog) => CatalogSlot::Ready(Rc::new(catalog.clone())),
            None => CatalogSlot::Failed,
        };
    }
}

/// Ask a Claude CLI in `cwd` for its catalog: start it, take its
/// `initialize` response and stop it. No turn runs and no API call is made;
/// it took 0.3 s when the design measured it.
fn probe_claude(cwd: PathBuf, repaint: egui::Context) -> Promise<Option<SlashCatalog>> {
    Promise::spawn_async(async move {
        let mut options = claude_agent_sdk_rs::ClaudeAgentOptions::default();
        // A remote session's cwd may not exist here; probe from wherever
        // this process runs instead, which still lists the built-ins.
        if cwd.is_dir() {
            options.cwd = Some(cwd.clone());
        }
        let mut client = claude_agent_sdk_rs::ClaudeClient::new(options);
        let catalog = match client.connect().await {
            Ok(()) => client
                .get_server_info()
                .map(|info| SlashCatalog::from_server_info(&info)),
            Err(err) => {
                tracing::warn!("slash command probe in {} failed: {err}", cwd.display());
                None
            }
        };
        let _ = client.disconnect().await;
        repaint.request_repaint();
        catalog
    })
}

/// The popup's key step for one frame, run before the keybindings and the
/// input see the keys: rerank, then apply the key the popup claims, if the
/// input `input_id` has focus. A key that rewrote the input leaves the
/// cursor at its end; the caller sends the draft on [`SlashOutcome::Run`].
pub fn popup_keys(
    popup: &mut SlashPopup,
    input: &mut String,
    ctx: &egui::Context,
    input_id: egui::Id,
) -> Option<SlashOutcome> {
    popup.sync(input);
    if !popup.claims_keys() || !ctx.memory(|m| m.has_focus(input_id)) {
        return None;
    }
    let outcome = popup.press(take_key(ctx)?, input)?;
    if outcome == SlashOutcome::Edited {
        cursor_to_end(ctx, input_id, input);
    }
    Some(outcome)
}

/// Read the key the open popup claims this frame, consuming it so neither
/// the text input nor the keybindings see it.
fn take_key(ctx: &egui::Context) -> Option<SlashKey> {
    use egui::{Key, Modifiers};
    ctx.input_mut(|i| {
        // Shift+Enter is a newline, and Shift+arrows select text; egui's
        // `consume_key` would match them too, since it ignores extra Shift.
        if i.modifiers.shift {
            return None;
        }
        let mut take = |mods: Modifiers, key: Key| i.consume_key(mods, key);
        if take(Modifiers::NONE, Key::ArrowUp) || take(Modifiers::CTRL, Key::P) {
            Some(SlashKey::Up)
        } else if take(Modifiers::NONE, Key::ArrowDown) || take(Modifiers::CTRL, Key::N) {
            Some(SlashKey::Down)
        } else if take(Modifiers::NONE, Key::Tab) {
            Some(SlashKey::Complete)
        } else if take(Modifiers::NONE, Key::Enter) {
            Some(SlashKey::Enter)
        } else if take(Modifiers::NONE, Key::Escape) {
            Some(SlashKey::Back)
        } else {
            None
        }
    })
}

/// Move the text cursor of the input `id` to the end of `input`, after the
/// popup rewrote it.
pub fn cursor_to_end(ctx: &egui::Context, id: egui::Id, input: &str) {
    let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
        return;
    };
    let end = egui::text::CCursor::new(input.chars().count());
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::one(end)));
    state.store(ctx, id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A catalog shaped like a real `initialize` response, cut down.
    fn catalog() -> Rc<SlashCatalog> {
        Rc::new(SlashCatalog::from_server_info(&json!({
            "commands": [
                {"name": "headway", "description": "Read and edit a Headway board (user)", "argumentHint": ""},
                {"name": "code-review", "description": "Review the current diff", "argumentHint": "[low|medium|high]"},
                {"name": "clear", "description": "Clear conversation history", "argumentHint": ""},
                {"name": "compact", "description": "Summarize the conversation", "argumentHint": "<instructions>"},
                {"name": "model", "description": "Set the AI model", "argumentHint": "<model>"},
                {"name": "context", "description": "Show context usage", "argumentHint": ""},
                {"name": "", "description": "nameless, dropped"},
            ],
            "models": [
                {"value": "default", "displayName": "Default (recommended)", "description": "Opus 5.5"},
                {"value": "opus", "displayName": "Opus 5.5", "description": "complex work"},
                {"value": "sonnet", "displayName": "Sonnet 5.5", "description": "efficient"},
                {"value": "haiku", "displayName": "Haiku 5.5", "description": "fastest"},
            ],
        })))
    }

    /// A popup over `catalog`.
    fn popup_over(catalog: &Rc<SlashCatalog>) -> SlashPopup {
        let mut popup = SlashPopup::default();
        popup.set_catalog(CatalogView::Ready(catalog.clone()));
        popup
    }

    fn names(popup: &SlashPopup, catalog: &SlashCatalog) -> Vec<String> {
        popup
            .matches()
            .iter()
            .map(|r| match popup.stage() {
                Some(Stage::Model) => catalog.models[r.index].value.clone(),
                _ => catalog.commands[r.index].name.clone(),
            })
            .collect()
    }

    #[test]
    fn server_info_puts_built_ins_before_skills() {
        let catalog = catalog();
        let order: Vec<&str> = catalog.commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            order,
            [
                "code-review",
                "clear",
                "compact",
                "model",
                "context",
                "headway"
            ]
        );
        assert_eq!(catalog.models.len(), 4);
        assert_eq!(catalog.commands[0].argument_hint, "[low|medium|high]");
    }

    #[test]
    fn parse_reads_command_and_argument() {
        assert_eq!(SlashInput::parse("hello"), None);
        assert_eq!(SlashInput::parse("/a\nb"), None);
        assert_eq!(
            SlashInput::parse("/mo"),
            Some(SlashInput {
                command: "mo",
                argument: None
            })
        );
        assert_eq!(
            SlashInput::parse("/model son"),
            Some(SlashInput {
                command: "model",
                argument: Some("son")
            })
        );
    }

    #[test]
    fn prefix_matches_rank_before_substring_matches() {
        let catalog = catalog();
        let mut popup = popup_over(&catalog);
        popup.sync("/co");
        // `code-review`, `compact`, `context` start with "co"; nothing else
        // contains it.
        assert_eq!(
            names(&popup, &catalog),
            ["code-review", "compact", "context"]
        );

        popup.sync("/e");
        let ranked = names(&popup, &catalog);
        // No name starts with "e", so every row is a substring match, in
        // list order, and its span is where the "e" sits.
        assert_eq!(ranked[0], "code-review");
        assert_eq!(popup.matches()[0].span, 3..4);
    }

    #[test]
    fn empty_query_lists_everything_in_order() {
        let catalog = catalog();
        let mut popup = popup_over(&catalog);
        popup.sync("/");
        assert_eq!(popup.matches().len(), catalog.commands.len());
        assert!(popup.is_open());
    }

    #[test]
    fn model_stage_filters_by_display_name_then_value() {
        let catalog = catalog();
        let mut popup = popup_over(&catalog);
        popup.sync("/model ");
        assert_eq!(popup.stage(), Some(Stage::Model));
        assert_eq!(popup.matches().len(), 4);

        popup.sync("/model son");
        assert_eq!(names(&popup, &catalog), ["sonnet"]);

        // "5.5" is in three display names, never at the start.
        popup.sync("/model 5.5");
        assert_eq!(names(&popup, &catalog), ["opus", "sonnet", "haiku"]);
    }

    #[test]
    fn a_command_without_an_argument_list_closes_on_space() {
        let catalog = catalog();
        let mut popup = popup_over(&catalog);
        popup.sync("/compact ");
        assert!(!popup.is_open());
    }

    #[test]
    fn enter_runs_a_bare_command_and_completes_one_with_arguments() {
        let catalog = catalog();
        let mut popup = popup_over(&catalog);
        let mut input = "/cont".to_owned();
        popup.sync(&input);
        assert_eq!(
            popup.press(SlashKey::Enter, &mut input),
            Some(SlashOutcome::Run)
        );
        assert_eq!(input, "/context");

        let mut input = "/mod".to_owned();
        popup.sync(&input);
        assert_eq!(
            popup.press(SlashKey::Enter, &mut input),
            Some(SlashOutcome::Edited)
        );
        assert_eq!(input, "/model ");
        popup.sync(&input);
        assert_eq!(popup.stage(), Some(Stage::Model));
    }

    #[test]
    fn model_stage_picks_a_value() {
        let catalog = catalog();
        let mut popup = popup_over(&catalog);
        let mut input = "/model ".to_owned();
        popup.sync(&input);
        popup.press(SlashKey::Down, &mut input);
        popup.press(SlashKey::Down, &mut input);
        assert_eq!(
            popup.press(SlashKey::Complete, &mut input),
            Some(SlashOutcome::Edited)
        );
        assert_eq!(input, "/model sonnet");
        popup.sync(&input);
        assert_eq!(
            popup.press(SlashKey::Enter, &mut input),
            Some(SlashOutcome::Run)
        );
        assert_eq!(input, "/model sonnet");
    }

    #[test]
    fn selection_wraps() {
        let catalog = catalog();
        let mut popup = popup_over(&catalog);
        let mut input = "/co".to_owned();
        popup.sync(&input);
        popup.press(SlashKey::Up, &mut input);
        assert_eq!(popup.selected(), 2);
        popup.press(SlashKey::Down, &mut input);
        assert_eq!(popup.selected(), 0);
    }

    #[test]
    fn escape_closes_until_the_text_changes_and_steps_back_from_models() {
        let catalog = catalog();
        let mut popup = popup_over(&catalog);
        let mut input = "/model so".to_owned();
        popup.sync(&input);
        assert_eq!(
            popup.press(SlashKey::Back, &mut input),
            Some(SlashOutcome::Edited)
        );
        assert_eq!(input, "/model");
        popup.sync(&input);
        assert_eq!(popup.stage(), Some(Stage::Command));

        assert_eq!(
            popup.press(SlashKey::Back, &mut input),
            Some(SlashOutcome::Moved)
        );
        assert!(!popup.is_open());
        // A second Esc is not the popup's: normal mode takes it.
        assert_eq!(popup.press(SlashKey::Back, &mut input), None);
        popup.sync(&input);
        assert!(!popup.is_open(), "same text, still dismissed");
        input.push('x');
        popup.sync(&input);
        assert!(popup.stage().is_some(), "typing reopens it");
    }

    #[test]
    fn loading_opens_without_rows_and_reranks_when_the_catalog_lands() {
        let mut popup = SlashPopup::default();
        popup.set_catalog(CatalogView::Loading);
        popup.sync("/");
        assert!(popup.is_open());
        assert!(popup.matches().is_empty());
        let catalog = catalog();
        popup.set_catalog(CatalogView::Ready(catalog.clone()));
        popup.sync("/");
        assert_eq!(popup.matches().len(), catalog.commands.len());
    }
}
