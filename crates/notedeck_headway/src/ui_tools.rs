//! Headway's **UI tools**: every action behind its keys, by name, so chrome's
//! keymap, its macros and (opted-in) agents can run them (see
//! [`notedeck::App::ui_tools`]).
//!
//! Chrome's keymap (`default_keymap.toml` in `notedeck_chrome`) binds
//! Headway's keys, in the [key scopes](key_scopes) of the view in front, to
//! these tools. A tool is parsed here into a [`HeadwayUiTool`] and queued on
//! [`BoardUiState`]; [`crate::keys`] drains the queue as the board renders,
//! through its `apply_*` helpers, because the board view a tool acts on only
//! exists inside the render pass. So a key and its tool called by name have
//! one effect, in one order.
//!
//! **Card tools** act on *the current card*: the open detail's, the queue's
//! or a review pane's card, else the grid's cursor. An optional
//! `card: "headway:<board>/<word-id>"` names one explicitly: on the grid the
//! cursor moves to it first; over a pane it has to be the card showing.
//!
//! **Board edits are not UI tools.** Archiving, moving a card (`D` is a move
//! to Done) and creating one are Headway's data tools ([`crate::tools`]:
//! `headway_archive_card`, `headway_move_card`, `headway_add_card`), and the
//! UI picks their result up as the edit folds in, like any other. The keys
//! that make those edits (`a`, `D`, `H`/`L`/`J`/`K`) still run through
//! [`crate::keys`] as before; nothing here shadows the data tools' names
//! (`no_ui_tool_shadows_a_data_tool`).
//!
//! This module also answers, from state alone and without allocating, which
//! tools are [available](available) and which [key scopes](key_scopes) are
//! active.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use notedeck::{ToolArg, ToolArgType, ToolSpec};

use crate::cursor::CursorMove;
use crate::keys::{CardAction, CardMove, PaneScroll};
use crate::ui::{BoardUiState, CardStep, SessionOpen};

/// `Enter`/`o`: open the current card's detail.
pub(crate) const OPEN_CARD: &str = "headway_open_card";
/// `e`: open the explainer of the current card's record.
pub(crate) const OPEN_EXPLAINER: &str = "headway_open_explainer";
/// `s`/`S`: open the agentium session of the current card's record.
pub(crate) const OPEN_SESSION: &str = "headway_open_session";
/// `r`: open the current card's review pane.
pub(crate) const OPEN_REVIEW: &str = "headway_open_review";
/// `X`: send the current card back, asking for a reason.
pub(crate) const SEND_BACK: &str = "headway_send_back";
/// `n`/`p`: step to the next/previous card.
pub(crate) const STEP_CARD: &str = "headway_step_card";
/// `j`/`k`/`h`/`l`, `gg`/`G`: move the grid's cursor.
pub(crate) const CURSOR_MOVE: &str = "headway_cursor_move";
/// `c` on the grid: open the add-card composer. Creating the card is the
/// composer's (or the `headway_add_card` data tool's).
pub(crate) const COMPOSE_CARD: &str = "headway_compose_card";
/// `/`: focus the grid's filter.
pub(crate) const FOCUS_FILTER: &str = "headway_focus_filter";
/// `R`: the review queue over the board's In Review, or the detail card's.
pub(crate) const OPEN_REVIEW_QUEUE: &str = "headway_open_review_queue";
/// The grid's `Esc`: drop the cursor.
pub(crate) const CLEAR: &str = "headway_clear";
/// `j`/`k`, `Space`, `^d`/`^u`, `gg`/`G` over a pane: scroll it.
pub(crate) const SCROLL: &str = "headway_scroll";
/// `]`/`[`: the next/previous file of a review pane's diff.
pub(crate) const STEP_FILE: &str = "headway_step_file";
/// `c` in a review pane: comment on the picked lines, or the commit.
pub(crate) const COMMENT: &str = "headway_comment";
/// `C` in a review pane: send the drafted comments.
pub(crate) const SEND_COMMENTS: &str = "headway_send_comments";
/// `q`/`Esc` over a pane: leave the queue, back a review pane out to its
/// card, or leave the detail for the grid.
pub(crate) const BACK: &str = "headway_back";

/// What a Headway key or UI tool does. Card tools carry the [`CardAction`]
/// their key maps to; the rest name what they do in the view that has them,
/// and do nothing in a view that doesn't.
///
/// The keys' board edits are here too, so every key goes through one
/// `apply_*` path: [`CardAction::Archive`], [`CardAction::Done`] and
/// [`MoveCard`](Self::MoveCard). [`parse`](Self::parse) never produces them:
/// called by name, those edits are data tools (see the module docs).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum HeadwayUiTool {
    /// A card action, on the current card or the one `card` names (a
    /// `headway:<board>/<word-id>` reference, checked by [`parse`](Self::parse)).
    Card {
        action: CardAction,
        card: Option<String>,
    },
    /// Move the grid's cursor.
    Cursor(CursorMove),
    /// Move the grid's cursor card (`H`/`L`/`J`/`K`). Keys only: a board edit.
    MoveCard(CardMove),
    /// Open the add-card composer in the cursor's column.
    AddCard,
    /// Focus the grid's filter field.
    FocusFilter,
    /// The review queue: the board's on the grid, the card's in its detail.
    ReviewQueue,
    /// The grid's `Esc`.
    Clear,
    /// Scroll the review pane's diff or the detail.
    Scroll(PaneScroll),
    /// Write a comment in a review pane.
    Comment,
    /// Send a review pane's drafted comments.
    SendComments,
    /// Back out of the queue, a review pane or the detail.
    Back,
}

/// The arguments of a card tool with nothing to choose.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CardArgs {
    #[serde(default)]
    card: Option<String>,
}

/// `headway_open_session`'s arguments.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SessionArgs {
    #[serde(default)]
    card: Option<String>,
    #[serde(default)]
    code_review: bool,
}

/// Next or previous, for `headway_step_card` and `headway_step_file`.
#[derive(Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Dir {
    #[default]
    Next,
    Prev,
}

/// The values of [`Dir`], as the specs advertise them.
const DIRS: &[&str] = &["next", "prev"];

/// `headway_step_card`'s arguments.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct StepArgs {
    #[serde(default)]
    card: Option<String>,
    #[serde(default)]
    dir: Dir,
}

/// `headway_step_file`'s arguments.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileArgs {
    #[serde(default)]
    dir: Dir,
}

/// Where `headway_cursor_move` takes the cursor.
#[derive(Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum CursorTo {
    #[default]
    Down,
    Up,
    Left,
    Right,
    First,
    Last,
}

/// The values of [`CursorTo`].
const CURSOR_TOS: &[&str] = &["down", "up", "left", "right", "first", "last"];

/// `headway_cursor_move`'s arguments.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CursorArgs {
    #[serde(default)]
    to: CursorTo,
}

/// How far `headway_scroll` scrolls: a line, a page or half of one either
/// way, or to an end.
#[derive(Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum ScrollTo {
    #[default]
    Down,
    Up,
    PageDown,
    PageUp,
    HalfDown,
    HalfUp,
    Top,
    Bottom,
}

/// The values of [`ScrollTo`].
const SCROLL_TOS: &[&str] = &[
    "down",
    "up",
    "page_down",
    "page_up",
    "half_down",
    "half_up",
    "top",
    "bottom",
];

/// `headway_scroll`'s arguments.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ScrollArgs {
    #[serde(default)]
    to: ScrollTo,
}

/// An argument-free tool's arguments: none.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

/// The optional `card` argument every card tool takes.
fn card_arg() -> ToolArg {
    ToolArg::new(
        "card",
        ToolArgType::String,
        "The card to act on, as headway:<board>/<word-id>; it must be on the board showing. \
         Omitted: the current card (the open detail's, else the grid cursor's).",
    )
}

/// A card tool's spec: its description and the `card` argument, plus `more`.
fn card_spec(name: &'static str, description: &'static str, more: Vec<ToolArg>) -> ToolSpec {
    let mut args = vec![card_arg()];
    args.extend(more);
    ToolSpec::new(name, description, args).ui()
}

/// An enum argument named `name` over `values`, defaulting to the first.
fn enum_arg(name: &'static str, values: &'static [&'static str], doc: &'static str) -> ToolArg {
    ToolArg::new(name, ToolArgType::Enum(values.to_vec()), doc).default(json!(values[0]))
}

impl HeadwayUiTool {
    /// The spec of every Headway UI tool, for [`notedeck::App::ui_tools`].
    pub(crate) fn specs() -> Vec<ToolSpec> {
        vec![
            card_spec(OPEN_CARD, "Open the current card's detail.", vec![]),
            card_spec(
                OPEN_EXPLAINER,
                "Open the explainer of the current card's review record in a browser.",
                vec![],
            ),
            card_spec(
                OPEN_SESSION,
                "Open the agentium session of the current card's review record.",
                vec![
                    ToolArg::new(
                        "code_review",
                        ToolArgType::Boolean,
                        "Ask the session for a code review of its work.",
                    )
                    .default(json!(false)),
                ],
            ),
            card_spec(
                OPEN_REVIEW,
                "Open the current card's review pane on its newest record.",
                vec![],
            ),
            card_spec(
                SEND_BACK,
                "Send the current card back to In Progress, asking for a reason.",
                vec![],
            ),
            card_spec(
                STEP_CARD,
                "Step to the next or previous card: the grid cursor's, the review queue's, \
                 or the card's column neighbour.",
                vec![enum_arg("dir", DIRS, "Which way to step.")],
            ),
            ToolSpec::new(
                CURSOR_MOVE,
                "Move the board grid's cursor.",
                vec![enum_arg("to", CURSOR_TOS, "Where the cursor goes.")],
            )
            .ui(),
            ToolSpec::new(
                COMPOSE_CARD,
                "Open the add-card composer in the grid cursor's column.",
                vec![],
            )
            .ui(),
            ToolSpec::new(FOCUS_FILTER, "Focus the board grid's filter.", vec![]).ui(),
            ToolSpec::new(
                OPEN_REVIEW_QUEUE,
                "Open the review queue: over the board's In Review column from the grid, \
                 over the card's In Review subissues from its detail.",
                vec![],
            )
            .ui(),
            ToolSpec::new(CLEAR, "Drop the board grid's cursor.", vec![]).ui(),
            ToolSpec::new(
                SCROLL,
                "Scroll the review pane's diff or the card detail.",
                vec![enum_arg("to", SCROLL_TOS, "How far to scroll.")],
            )
            .ui(),
            ToolSpec::new(
                STEP_FILE,
                "Scroll the review pane's diff to the next or previous file.",
                vec![enum_arg("dir", DIRS, "Which way to step.")],
            )
            .ui(),
            ToolSpec::new(
                COMMENT,
                "Write a review comment on the picked diff lines, or on the commit.",
                vec![],
            )
            .ui(),
            ToolSpec::new(
                SEND_COMMENTS,
                "Send the review pane's drafted comments.",
                vec![],
            )
            .ui(),
            ToolSpec::new(
                BACK,
                "Leave the review queue, back a review pane out to its card, or leave the \
                 card detail for the grid.",
                vec![],
            )
            .ui(),
        ]
    }

    /// Parse a call of the tool `name` with `args` (an object, or `null` for
    /// none). Errors on an unknown tool, arguments that aren't an object, an
    /// unknown argument, a value outside
    /// an argument's enum, or a `card` that isn't a card reference.
    pub(crate) fn parse(name: &str, args: &Value) -> Result<Self, String> {
        let none = json!({});
        let args = if args.is_null() { &none } else { args };
        if !args.is_object() {
            return Err(format!("'{name}' arguments must be an object, not {args}"));
        }
        Ok(match name {
            OPEN_CARD => card_tool(name, args, CardAction::Open)?,
            OPEN_EXPLAINER => card_tool(name, args, CardAction::Explainer(None))?,
            OPEN_SESSION => {
                let SessionArgs { card, code_review } = parse_args(name, args)?;
                let how = if code_review {
                    SessionOpen::CodeReview
                } else {
                    SessionOpen::Plain
                };
                card_target(CardAction::Session(how, None), card)?
            }
            OPEN_REVIEW => card_tool(name, args, CardAction::Review(None))?,
            SEND_BACK => card_tool(name, args, CardAction::SendBack)?,
            STEP_CARD => {
                let StepArgs { card, dir } = parse_args(name, args)?;
                let step = match dir {
                    Dir::Next => CardStep::Next,
                    Dir::Prev => CardStep::Prev,
                };
                card_target(CardAction::Step(step), card)?
            }
            CURSOR_MOVE => {
                let CursorArgs { to } = parse_args(name, args)?;
                Self::Cursor(match to {
                    CursorTo::Down => CursorMove::Down,
                    CursorTo::Up => CursorMove::Up,
                    CursorTo::Left => CursorMove::Left,
                    CursorTo::Right => CursorMove::Right,
                    CursorTo::First => CursorMove::First,
                    CursorTo::Last => CursorMove::Last,
                })
            }
            COMPOSE_CARD => no_args(name, args, Self::AddCard)?,
            FOCUS_FILTER => no_args(name, args, Self::FocusFilter)?,
            OPEN_REVIEW_QUEUE => no_args(name, args, Self::ReviewQueue)?,
            CLEAR => no_args(name, args, Self::Clear)?,
            SCROLL => {
                let ScrollArgs { to } = parse_args(name, args)?;
                Self::Scroll(match to {
                    ScrollTo::Down => PaneScroll::Rows(1),
                    ScrollTo::Up => PaneScroll::Rows(-1),
                    ScrollTo::PageDown => PaneScroll::Pages(1.0),
                    ScrollTo::PageUp => PaneScroll::Pages(-1.0),
                    ScrollTo::HalfDown => PaneScroll::Pages(0.5),
                    ScrollTo::HalfUp => PaneScroll::Pages(-0.5),
                    ScrollTo::Top => PaneScroll::Top,
                    ScrollTo::Bottom => PaneScroll::Bottom,
                })
            }
            STEP_FILE => {
                let FileArgs { dir } = parse_args(name, args)?;
                Self::Scroll(match dir {
                    Dir::Next => PaneScroll::NextFile,
                    Dir::Prev => PaneScroll::PrevFile,
                })
            }
            COMMENT => no_args(name, args, Self::Comment)?,
            SEND_COMMENTS => no_args(name, args, Self::SendComments)?,
            BACK => no_args(name, args, Self::Back)?,
            _ => return Err(format!("unknown tool '{name}'")),
        })
    }
}

/// Deserialize `args` for the tool `name`.
fn parse_args<T: DeserializeOwned>(name: &str, args: &Value) -> Result<T, String> {
    T::deserialize(args).map_err(|e| format!("failed to parse '{name}' arguments: {e}"))
}

/// A card tool with nothing to choose but its card.
fn card_tool(name: &str, args: &Value, action: CardAction) -> Result<HeadwayUiTool, String> {
    let CardArgs { card } = parse_args(name, args)?;
    card_target(action, card)
}

/// `action` on `card`, once `card` is checked to be a card reference.
fn card_target(action: CardAction, card: Option<String>) -> Result<HeadwayUiTool, String> {
    if let Some(card) = &card
        && headway::wordid::parse_ref(card).is_none()
    {
        return Err(format!(
            "'{card}' is not a card reference (headway:<board>/<word-id>)"
        ));
    }
    Ok(HeadwayUiTool::Card { action, card })
}

/// `tool`, once `args` is checked to hold nothing.
fn no_args(name: &str, args: &Value, tool: HeadwayUiTool) -> Result<HeadwayUiTool, String> {
    let NoArgs {} = parse_args(name, args)?;
    Ok(tool)
}

/// Which of Headway's keyed views is in front, as far as its keys and tools
/// go. `None` from [`key_view`] when none takes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyView {
    /// The board grid.
    Grid,
    /// The review queue.
    Queue,
    /// A review pane opened from a card.
    Pane,
    /// The card detail.
    Detail,
}

/// The view whose keys and tools are live: nothing while the `X`
/// composer is open (its Enter/Esc are its field's) or the dependency graph
/// is; else the queue, a review pane, the detail; else the grid, unless an
/// inline editor, the archived sheet or one of the grid's menus has the
/// keyboard.
pub(crate) fn key_view(state: &BoardUiState) -> Option<KeyView> {
    if state.rejecting() || state.graph_epic().is_some() {
        return None;
    }
    if state.queue_open() {
        return Some(KeyView::Queue);
    }
    if state.review_card().is_some() {
        return Some(KeyView::Pane);
    }
    if state.selected().is_some() {
        return Some(KeyView::Detail);
    }
    if state.keys_blocked() || state.grid_menu_open() {
        return None;
    }
    Some(KeyView::Grid)
}

/// The queue's key scopes. Bound in chrome's `default_keymap.toml`. No two
/// scopes of one view's stack bind the same keys, so the order only sets the
/// which-key strip's: the card actions first, then the view's own keys, as
/// Headway's strips have always read; the grid lists its own first.
const QUEUE_SCOPES: &[&str] = &[
    "headway.card",
    "headway.scroll",
    "headway.diff",
    "headway.queue",
    "headway",
];
/// A plain review pane's key scopes: the queue's, bar how it's left.
const PANE_SCOPES: &[&str] = &[
    "headway.card",
    "headway.scroll",
    "headway.diff",
    "headway.pane",
    "headway",
];
/// The detail's key scopes.
const DETAIL_SCOPES: &[&str] = &[
    "headway.card",
    "headway.scroll",
    "headway.detail",
    "headway",
];
/// The grid's key scopes.
const GRID_SCOPES: &[&str] = &["headway.grid", "headway.card", "headway"];

/// The key scopes of the [view in front](key_view), most specific first, or
/// none while no view takes keys.
pub(crate) fn key_scopes(state: &BoardUiState) -> &'static [&'static str] {
    match key_view(state) {
        None => &[],
        Some(KeyView::Queue) => QUEUE_SCOPES,
        Some(KeyView::Pane) => PANE_SCOPES,
        Some(KeyView::Detail) => DETAIL_SCOPES,
        Some(KeyView::Grid) => GRID_SCOPES,
    }
}

/// The current card of `at`: the queue's, the review pane's, the detail's,
/// or the grid's cursor card.
pub(crate) fn current_card(state: &BoardUiState, at: KeyView) -> Option<nostrdb_net::NoteId> {
    match at {
        KeyView::Grid => state.cursor(),
        KeyView::Queue => state.queue_card(),
        KeyView::Pane => state.review_card(),
        KeyView::Detail => state.selected(),
    }
}

/// Whether the UI tool `name` does anything in the view in front: a card tool
/// needs a current card, a grid tool the grid, a pane tool a pane. `headway_clear` is
/// available only with a cursor to drop, so an `Esc` with nothing to clear
/// falls through to chrome. Unknown names aren't. Doesn't allocate.
pub(crate) fn available(state: &BoardUiState, name: &str) -> bool {
    let Some(at) = key_view(state) else {
        return false;
    };
    let card = current_card(state, at).is_some();
    let grid = at == KeyView::Grid;
    let review = matches!(at, KeyView::Queue | KeyView::Pane);
    match name {
        // The detail has nothing to open: its card is open.
        OPEN_CARD => card && at != KeyView::Detail,
        // Say there's no explainer, or no session, when there's none, so
        // the key answers rather than falling through.
        OPEN_EXPLAINER | OPEN_SESSION | OPEN_REVIEW | SEND_BACK => card,
        // The grid's step lands a missing cursor on the first card; the
        // queue's steps off a card that has left the board.
        STEP_CARD => card || grid || at == KeyView::Queue,
        CURSOR_MOVE | COMPOSE_CARD | FOCUS_FILTER => grid,
        OPEN_REVIEW_QUEUE => grid || at == KeyView::Detail,
        CLEAR => grid && card,
        SCROLL => !grid,
        // An Esc that only left a detail field mustn't leave the card too.
        BACK => !grid && !(at == KeyView::Detail && state.detail_field_focused()),
        STEP_FILE => review,
        COMMENT | SEND_COMMENTS => review && card,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every advertised spec is a UI tool that parses with no arguments, and
    /// `null` arguments mean none.
    #[test]
    fn every_spec_parses_with_its_defaults() {
        for spec in HeadwayUiTool::specs() {
            assert_eq!(spec.kind(), notedeck::ToolKind::Ui, "{}", spec.name());
            HeadwayUiTool::parse(spec.name(), &json!({}))
                .unwrap_or_else(|e| panic!("{} with no args: {e}", spec.name()));
            HeadwayUiTool::parse(spec.name(), &Value::Null)
                .unwrap_or_else(|e| panic!("{} with null args: {e}", spec.name()));
        }
    }

    /// Every value an enum argument advertises parses, so a spec can't offer
    /// a value `parse` doesn't know.
    #[test]
    fn every_advertised_enum_value_parses() {
        for spec in HeadwayUiTool::specs() {
            let schema = spec.json_schema();
            let props = schema["properties"].as_object().expect("properties");
            for (arg, prop) in props {
                for value in prop["enum"].as_array().into_iter().flatten() {
                    HeadwayUiTool::parse(spec.name(), &json!({ arg: value }))
                        .unwrap_or_else(|e| panic!("{} {arg}={value}: {e}", spec.name()));
                }
            }
        }
    }

    /// Every tool `parse` knows is advertised, and no name twice.
    #[test]
    fn specs_name_every_tool_once() {
        let names: Vec<_> = HeadwayUiTool::specs().iter().map(|s| s.name()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "duplicate names in {names:?}");
        assert!(HeadwayUiTool::parse("headway_nope", &json!({})).is_err());
    }

    #[test]
    fn parse_rejects_bad_args() {
        let bad = [
            (CURSOR_MOVE, json!({ "to": "sideways" })),
            (STEP_CARD, json!({ "dir": 3 })),
            (OPEN_SESSION, json!({ "code_review": "yes" })),
            (OPEN_CARD, json!({ "card": "maple-river-canyon" })),
            (OPEN_CARD, json!({ "bogus": 1 })),
            (
                COMPOSE_CARD,
                json!({ "card": "headway:b/maple-river-canyon" }),
            ),
            (SCROLL, json!({ "to": "left" })),
            (BACK, json!([])),
        ];
        for (name, args) in bad {
            assert!(
                HeadwayUiTool::parse(name, &args).is_err(),
                "{name} accepted {args}"
            );
        }
    }

    #[test]
    fn parse_reads_args_into_the_keys_actions() {
        let parse = |name, args| HeadwayUiTool::parse(name, &args).unwrap();
        assert_eq!(
            parse(OPEN_SESSION, json!({ "code_review": true })),
            HeadwayUiTool::Card {
                action: CardAction::Session(SessionOpen::CodeReview, None),
                card: None,
            }
        );
        assert_eq!(
            parse(
                STEP_CARD,
                json!({ "dir": "prev", "card": "headway:b/maple-river-canyon" })
            ),
            HeadwayUiTool::Card {
                action: CardAction::Step(CardStep::Prev),
                card: Some("headway:b/maple-river-canyon".to_owned()),
            }
        );
        assert_eq!(
            parse(SCROLL, json!({ "to": "half_up" })),
            HeadwayUiTool::Scroll(PaneScroll::Pages(-0.5))
        );
        assert_eq!(
            parse(STEP_FILE, json!({ "dir": "prev" })),
            HeadwayUiTool::Scroll(PaneScroll::PrevFile)
        );
    }

    /// No UI tool takes a data tool's name. The tool registry keeps the first
    /// of a name and registers data tools first, so a UI tool that shadowed
    /// one would be dropped and never reach Headway: the bug this replaced,
    /// when `headway_archive_card`, `headway_move_card` and `headway_add_card`
    /// were UI tools too.
    #[test]
    fn no_ui_tool_shadows_a_data_tool() {
        let data: Vec<_> = crate::tools::tools().iter().map(|t| t.name()).collect();
        for spec in HeadwayUiTool::specs() {
            assert!(
                !data.contains(&spec.name()),
                "UI tool {} shadows a data tool",
                spec.name()
            );
        }
    }

    /// Board edits are data tools: called as UI tools they're unknown.
    #[test]
    fn board_edits_are_not_ui_tools() {
        for name in [
            "headway_archive_card",
            "headway_move_card",
            "headway_add_card",
            "headway_done_card",
        ] {
            assert!(HeadwayUiTool::parse(name, &json!({})).is_err(), "{name}");
        }
    }
}
