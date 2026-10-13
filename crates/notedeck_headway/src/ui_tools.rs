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
//! UI picks their result up as the edit folds in, like any other. Chrome's
//! keymap binds the keys that make them (`a`, `D`, `H`/`L`/`J`/`K`) to those
//! data tools on the [current card](CurrentRef); nothing here shadows their
//! names (`no_ui_tool_shadows_a_data_tool`).
//!
//! This module also answers, from state alone and without allocating, which
//! tools are [available](available) and which [key scopes](key_scopes) are
//! active.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use notedeck::{ToolArg, ToolArgType, ToolSpec};

use crate::cursor::CursorMove;
use crate::keys::{CardAction, PaneScroll};
use crate::ui::{BoardStep, BoardUiState, CardStep, GroupStep, ListFold, SessionOpen};

/// `Enter`/`o`: open the current card's detail.
pub(crate) const OPEN_CARD: &str = "headway_open_card";
/// `e`: open the explainer of the current card's record.
pub(crate) const OPEN_EXPLAINER: &str = "headway_open_explainer";
/// `g p`: open the current card's parent. Offered only on a card that has
/// one, so a macro step on it waits, then aborts, on a card that hasn't.
pub(crate) const OPEN_PARENT: &str = "headway_open_parent";
/// `s`/`S`: open the agentium session of the current card's record.
pub(crate) const OPEN_SESSION: &str = "headway_open_session";
/// `r`: open the current card's review pane.
pub(crate) const OPEN_REVIEW: &str = "headway_open_review";
/// `X`: send the current card back, asking for a reason.
pub(crate) const SEND_BACK: &str = "headway_send_back";
/// `C`: comment on the current card, telling its record's session.
pub(crate) const QUICK_COMMENT: &str = "headway_quick_comment";
/// `n`/`p`: step to the next/previous card.
pub(crate) const STEP_CARD: &str = "headway_step_card";
/// `j`/`k`/`h`/`l`, `gg`/`G`: move the grid's cursor.
pub(crate) const CURSOR_MOVE: &str = "headway_cursor_move";
/// `c` on the grid: open the add-card composer; in the issue list, the
/// composer for a card in the cursor's group. Creating the card is the
/// composer's (or the `headway_add_card` data tool's).
pub(crate) const COMPOSE_CARD: &str = "headway_compose_card";
/// `/`: focus the grid's filter.
pub(crate) const FOCUS_FILTER: &str = "headway_focus_filter";
/// `R`: the review queue over the board's In Review, or the detail card's.
pub(crate) const OPEN_REVIEW_QUEUE: &str = "headway_open_review_queue";
/// `gi` on the grid: open the cross-board issue list.
pub(crate) const OPEN_LIST: &str = "headway_open_list";
/// `]b` on the grid: switch to the next board, in the switcher's order.
pub(crate) const NEXT_BOARD: &str = "headway_next_board";
/// `[b` on the grid: switch to the previous board, in the switcher's order.
pub(crate) const PREV_BOARD: &str = "headway_prev_board";
/// `gb` on the grid: open the board picker.
pub(crate) const PICK_BOARD: &str = "headway_pick_board";
/// The grid's `Esc`: drop the cursor.
pub(crate) const CLEAR: &str = "headway_clear";
/// `za`/`zc`/`zo`/`zM`/`zR` in the issue list: fold its group headers.
pub(crate) const LIST_FOLD: &str = "headway_list_fold";
/// `]g`/`[g` in the issue list: step the cursor to the next/previous group
/// header.
pub(crate) const LIST_STEP_GROUP: &str = "headway_list_step_group";
/// `j`/`k`, `Space`, `^d`/`^u`, `gg`/`G` over a pane: scroll it.
pub(crate) const SCROLL: &str = "headway_scroll";
/// `]`/`[`: the next/previous file of a review pane's diff.
pub(crate) const STEP_FILE: &str = "headway_step_file";
/// `c` in a review pane: comment on the picked lines, or the commit.
pub(crate) const COMMENT: &str = "headway_comment";
/// The review pane's "Send N comments": send the drafted comments. No key:
/// `C`'s comment takes them along.
pub(crate) const SEND_COMMENTS: &str = "headway_send_comments";
/// `q`/`Esc` over a pane: leave the queue, back a review pane out to its
/// card, or leave the detail for the grid.
pub(crate) const BACK: &str = "headway_back";

/// What a Headway key or UI tool does. Card tools carry the [`CardAction`]
/// their key maps to; the rest name what they do in the view that has them,
/// and do nothing in a view that doesn't.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum HeadwayUiTool {
    /// A card action, on the current card or the one `card` names (a
    /// `headway:<board>/<word-id>` reference, checked by [`parse`](Self::parse)).
    Card {
        action: CardAction,
        card: Option<String>,
    },
    /// Move the grid's or the issue list's cursor.
    Cursor(CursorMove),
    /// Open the add-card composer in the grid cursor's column, or the issue
    /// list's in the cursor's group.
    AddCard,
    /// Focus the grid's filter field.
    FocusFilter,
    /// The review queue: the board's on the grid, the card's in its detail.
    ReviewQueue,
    /// Open the cross-board issue list from the grid.
    OpenList,
    /// Switch to the next or previous board, from the grid.
    StepBoard(BoardStep),
    /// Open the board picker over the grid.
    PickBoard,
    /// The grid's `Esc`.
    Clear,
    /// Fold the issue list's group headers.
    ListFold(ListFold),
    /// Step the issue list's cursor to the next or previous group header.
    ListStepGroup(GroupStep),
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

/// Next or previous, for `headway_step_card`, `headway_step_file` and
/// `headway_list_step_group`.
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

/// The arguments of `headway_step_file` and `headway_list_step_group`: a
/// direction.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct DirArgs {
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

/// What `headway_list_fold` folds.
#[derive(Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum FoldTo {
    #[default]
    Toggle,
    Open,
    Close,
    All,
    None,
}

/// The values of [`FoldTo`].
const FOLD_TOS: &[&str] = &["toggle", "open", "close", "all", "none"];

/// `headway_list_fold`'s arguments.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FoldArgs {
    #[serde(default)]
    to: FoldTo,
}

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
                OPEN_PARENT,
                "Open the detail of the current card's parent, switching boards when the \
                 parent is on another. Only offered on a card that has a parent.",
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
                QUICK_COMMENT,
                "Ask for a one-line comment on the current card, post it, and send it to its \
                 review record's agentium session with any line comments drafted on the record.",
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
                "Move the board grid's or the issue list's cursor.",
                vec![enum_arg("to", CURSOR_TOS, "Where the cursor goes.")],
            )
            .ui(),
            ToolSpec::new(
                COMPOSE_CARD,
                "Open the add-card composer in the grid cursor's column, or in the issue \
                 list, the composer for a card in the cursor's group.",
                vec![],
            )
            .ui(),
            ToolSpec::new(
                FOCUS_FILTER,
                "Focus the board grid's filter, or the issue list's query.",
                vec![],
            )
            .ui(),
            ToolSpec::new(
                OPEN_REVIEW_QUEUE,
                "Open the review queue: over the board's In Review column from the grid, \
                 over the card's In Review subissues from its detail.",
                vec![],
            )
            .ui(),
            ToolSpec::new(
                OPEN_LIST,
                "Open the issue list: every board's cards as one filterable list.",
                vec![],
            )
            .ui(),
            ToolSpec::new(
                NEXT_BOARD,
                "Switch to the next board, in the board switcher's order, wrapping around.",
                vec![],
            )
            .ui(),
            ToolSpec::new(
                PREV_BOARD,
                "Switch to the previous board, in the board switcher's order, wrapping around.",
                vec![],
            )
            .ui(),
            ToolSpec::new(
                PICK_BOARD,
                "Open the board picker: every board, narrowed as you type, to switch to one.",
                vec![],
            )
            .ui(),
            ToolSpec::new(CLEAR, "Drop the board grid's cursor.", vec![]).ui(),
            ToolSpec::new(
                LIST_FOLD,
                "Fold the issue list's group headers: toggle, open or close the cursor's \
                 group, or close (all) or open (none) every group.",
                vec![enum_arg("to", FOLD_TOS, "What to fold.")],
            )
            .ui(),
            ToolSpec::new(
                LIST_STEP_GROUP,
                "Move the issue list's cursor to the next or previous group header.",
                vec![enum_arg("dir", DIRS, "Which way to step.")],
            )
            .ui(),
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
                 card detail or the issue list for the grid.",
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
            OPEN_PARENT => card_tool(name, args, CardAction::Parent)?,
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
            QUICK_COMMENT => card_tool(name, args, CardAction::Comment)?,
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
            OPEN_LIST => no_args(name, args, Self::OpenList)?,
            NEXT_BOARD => no_args(name, args, Self::StepBoard(BoardStep::Next))?,
            PREV_BOARD => no_args(name, args, Self::StepBoard(BoardStep::Prev))?,
            PICK_BOARD => no_args(name, args, Self::PickBoard)?,
            CLEAR => no_args(name, args, Self::Clear)?,
            LIST_FOLD => {
                let FoldArgs { to } = parse_args(name, args)?;
                Self::ListFold(match to {
                    FoldTo::Toggle => ListFold::Toggle,
                    FoldTo::Open => ListFold::Open,
                    FoldTo::Close => ListFold::Close,
                    FoldTo::All => ListFold::CloseAll,
                    FoldTo::None => ListFold::OpenAll,
                })
            }
            LIST_STEP_GROUP => {
                let DirArgs { dir } = parse_args(name, args)?;
                Self::ListStepGroup(match dir {
                    Dir::Next => GroupStep::Next,
                    Dir::Prev => GroupStep::Prev,
                })
            }
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
                let DirArgs { dir } = parse_args(name, args)?;
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
    /// The cross-board issue list.
    List,
}

/// The view whose keys and tools are live: nothing while the `X`/`C`
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
    if state.list_open() {
        return Some(KeyView::List);
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
/// The issue list's key scopes. Not `headway.card`: its board edits act on a
/// card of the board showing, and a list row may be on another.
const LIST_SCOPES: &[&str] = &["headway.list", "headway"];

/// The key scopes of the [view in front](key_view), most specific first, or
/// none while no view takes keys.
pub(crate) fn key_scopes(state: &BoardUiState) -> &'static [&'static str] {
    match key_view(state) {
        None => &[],
        Some(KeyView::Queue) => QUEUE_SCOPES,
        Some(KeyView::Pane) => PANE_SCOPES,
        Some(KeyView::Detail) => DETAIL_SCOPES,
        Some(KeyView::Grid) => GRID_SCOPES,
        Some(KeyView::List) => LIST_SCOPES,
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
        KeyView::List => state.list_cursor(),
    }
}

/// The current card as a `headway:<board>/<word-id>` ref, for chrome's
/// keymap to run board edits on (`a` is `headway_archive_card` with
/// `card = "{current}"`; see [`notedeck::App::current_ref`]). Latched as each
/// pass ends ([`BoardUiState::latch_current_ref`]) and kept between passes, so
/// reading it never allocates; the ref is rebuilt only when the card changes.
#[derive(Default)]
pub(crate) struct CurrentRef {
    /// The card, while there is one.
    card: Option<nostrdb_net::NoteId>,
    /// Its ref, meaningful only while `card` is set.
    text: String,
}

impl CurrentRef {
    /// Make `card`, on the board with slug `board`, the current card, or
    /// clear it.
    pub(crate) fn latch(&mut self, board: &str, card: Option<nostrdb_net::NoteId>) {
        let Some(card) = card else {
            self.card = None;
            return;
        };
        let same_board = headway::wordid::parse_ref(&self.text).is_some_and(|r| r.board == board);
        if self.card == Some(card) && same_board {
            return;
        }
        self.card = Some(card);
        self.text = headway::wordid::card_ref(board, card.bytes());
    }

    /// The current card's ref, if there is one.
    pub(crate) fn get(&self) -> Option<&str> {
        self.card.map(|_| self.text.as_str())
    }
}

/// Whether the UI tool `name` does anything in the view in front: a card tool
/// needs a current card, a grid tool the grid, a pane tool a pane. `headway_clear` is
/// available only with a cursor to drop, so an `Esc` with nothing to clear
/// falls through to chrome, and `]b`/`[b` only with another board to step
/// to. Unknown names aren't. Doesn't allocate.
pub(crate) fn available(state: &BoardUiState, name: &str) -> bool {
    let Some(at) = key_view(state) else {
        return false;
    };
    let card = current_card(state, at).is_some();
    let grid = at == KeyView::Grid;
    let list = at == KeyView::List;
    let review = matches!(at, KeyView::Queue | KeyView::Pane);
    match name {
        // The detail has nothing to open: its card is open. The list's
        // Enter on a group header folds it.
        OPEN_CARD => {
            (card && at != KeyView::Detail) || (list && state.list_cursor_spot().is_some())
        }
        // Say there's no explainer, or no session, when there's none, so
        // the key answers rather than falling through. The list's card may
        // be on another board, so these wait for its detail.
        OPEN_EXPLAINER | OPEN_SESSION | OPEN_REVIEW | SEND_BACK | QUICK_COMMENT => card && !list,
        // Only a subissue has a parent to open: on any other card the key
        // falls through, and a macro's step waits and aborts.
        OPEN_PARENT => !list && current_card(state, at).is_some_and(|c| state.has_parent(c)),
        // The grid's step lands a missing cursor on the first card; the
        // queue's steps off a card that has left the board.
        STEP_CARD => (card && !list) || grid || at == KeyView::Queue,
        CURSOR_MOVE | FOCUS_FILTER => grid || list,
        COMPOSE_CARD => grid || (list && state.list_can_compose()),
        OPEN_LIST | PICK_BOARD => grid,
        LIST_FOLD | LIST_STEP_GROUP => list && state.list_has_groups(),
        NEXT_BOARD | PREV_BOARD => grid && state.other_boards(),
        OPEN_REVIEW_QUEUE => grid || at == KeyView::Detail,
        CLEAR => grid && card,
        SCROLL => !grid && !list,
        // An Esc that only left a detail field mustn't leave the card too,
        // nor one that closes the list's `+` composer the list.
        BACK => {
            !grid
                && !(at == KeyView::Detail && state.detail_field_focused())
                && !(list && state.list_composing())
        }
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

    /// `headway_open_parent` is a card tool, on the current card or a named
    /// one.
    #[test]
    fn open_parent_parses_with_and_without_a_card() {
        let parse = |args| HeadwayUiTool::parse(OPEN_PARENT, &args).unwrap();
        assert_eq!(
            parse(json!({})),
            HeadwayUiTool::Card {
                action: CardAction::Parent,
                card: None,
            }
        );
        assert_eq!(
            parse(json!({ "card": "headway:b/maple-river-canyon" })),
            HeadwayUiTool::Card {
                action: CardAction::Parent,
                card: Some("headway:b/maple-river-canyon".to_owned()),
            }
        );
        assert!(HeadwayUiTool::parse(OPEN_PARENT, &json!({ "card": "maple" })).is_err());
    }

    /// `headway_open_parent` is offered on a subissue in the grid and the
    /// detail, as each pass latches it, and not on a card with no parent,
    /// nor in the issue list.
    #[test]
    fn open_parent_is_offered_only_on_a_subissue() {
        use crate::cursor::tests::{grid, id};
        use crate::ui::{ViewFilter, parse_board_filter};

        let mut view = grid();
        // Card 5 is a subissue of card 4; card 6 has no parent.
        view.columns[2].cards[0].parent = Some(id(4));
        let everything = parse_board_filter("", &view, false);
        let filter = ViewFilter {
            view: &view,
            filter: &everything,
        };
        // As a grid pass ends.
        let pass = |state: &mut BoardUiState| {
            state.latch_cursor_slot(&view, &filter);
            state.latch_current_ref(&view);
        };

        let mut state = BoardUiState::default();
        state.set_cursor(id(5));
        assert!(!available(&state, OPEN_PARENT), "not latched yet");
        pass(&mut state);
        assert!(available(&state, OPEN_PARENT), "a subissue on the grid");

        state.set_cursor(id(6));
        assert!(!available(&state, OPEN_PARENT), "the latch is 5's");
        pass(&mut state);
        assert!(!available(&state, OPEN_PARENT), "no parent on the grid");

        state.set_selected(Some(id(5)));
        pass(&mut state);
        assert!(available(&state, OPEN_PARENT), "a subissue's detail");
        state.set_selected(Some(id(6)));
        pass(&mut state);
        assert!(!available(&state, OPEN_PARENT), "no parent in the detail");

        state.set_selected(None);
        state.set_cursor(id(5));
        pass(&mut state);
        state.open_list();
        pass(&mut state);
        assert!(!available(&state, OPEN_PARENT), "the issue list");
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
