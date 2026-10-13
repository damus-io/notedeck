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

use crate::cursor::{CursorMove, Vertical};
use crate::keys::{CardAction, PaneScroll};
use crate::ui::{
    BoardStep, BoardUiState, CardStep, DisplaySwitch, GroupStep, ListFold, ListLayout, OrderDir,
    Pick, SessionOpen, TabPick,
};
use headway::query::{BuiltinView, DisplayProp, GroupBy, SortBy};

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
/// `gt`/`gT` in the issue list: show the next/previous view tab, or a named
/// one.
pub(crate) const LIST_TAB: &str = "headway_list_tab";
/// `J` in the issue list, ordered by work order: move the cursor's card one
/// place later.
pub(crate) const LIST_MOVE_DOWN: &str = "headway_list_move_down";
/// `K` in the issue list, ordered by work order: move the cursor's card one
/// place earlier.
pub(crate) const LIST_MOVE_UP: &str = "headway_list_move_up";
/// The issue list's grouping, as its Display menu picks it.
pub(crate) const LIST_GROUP: &str = "headway_list_group";
/// The issue list's sub-grouping, as its Display menu picks it.
pub(crate) const LIST_SUBGROUP: &str = "headway_list_subgroup";
/// `go` in the issue list: the next ordering, or a named one.
pub(crate) const LIST_ORDER: &str = "headway_list_order";
/// The issue list's ordering direction, as the Display menu's arrow flips it.
pub(crate) const LIST_ORDER_DIR: &str = "headway_list_order_dir";
/// Show or hide one of the issue list's row properties.
pub(crate) const LIST_DISPLAY: &str = "headway_list_display";
/// `gs` in the issue list: name what it shows as a new saved view.
pub(crate) const LIST_SAVE_VIEW: &str = "headway_list_save_view";
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
    /// Change the issue list's tab, grouping, ordering or display.
    ListLayout(ListLayout),
    /// Move the issue list cursor's card a place later (`Down`) or earlier
    /// in work order.
    ListMove(Vertical),
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

/// Which tab `headway_list_tab` shows: a step, or a built-in view.
#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum TabTo {
    Next,
    Prev,
    Active,
    Backlog,
    All,
}

/// The values of [`TabTo`].
const TAB_TOS: &[&str] = &["next", "prev", "active", "backlog", "all"];

/// `headway_list_tab`'s arguments: a step or built-in view, or a saved view
/// by slug or name; not both.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct TabArgs {
    #[serde(default)]
    to: Option<TabTo>,
    #[serde(default)]
    view: Option<String>,
}

/// The arguments of `headway_list_group`, `headway_list_subgroup` and
/// `headway_list_order`: `next`, or a [`GroupBy`] or [`SortBy`] name.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ByArgs {
    #[serde(default)]
    by: Option<String>,
}

/// Which way `headway_list_order_dir` runs the ordering.
#[derive(Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum DirTo {
    #[default]
    Toggle,
    Asc,
    Desc,
}

/// The values of [`DirTo`].
const DIR_TOS: &[&str] = &["toggle", "asc", "desc"];

/// `headway_list_order_dir`'s arguments.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct OrderDirArgs {
    #[serde(default)]
    dir: DirTo,
}

/// `headway_list_display`'s `on`: `toggle`, or show or hide, as a word or a
/// JSON boolean.
#[derive(Deserialize, Clone, Copy)]
#[serde(untagged)]
enum SwitchArg {
    Bool(bool),
    Word(SwitchWord),
}

/// [`SwitchArg`]'s words.
#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum SwitchWord {
    Toggle,
    True,
    False,
}

/// The values of [`SwitchWord`].
const SWITCH_WORDS: &[&str] = &["toggle", "true", "false"];

/// `headway_list_display`'s arguments.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DisplayArgs {
    prop: String,
    #[serde(default)]
    on: Option<SwitchArg>,
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
    names_arg(name, values.to_vec(), doc)
}

/// An enum argument named `name` over `values`, defaulting to the first.
fn names_arg(name: &'static str, values: Vec<&'static str>, doc: &'static str) -> ToolArg {
    let first = values[0];
    ToolArg::new(name, ToolArgType::Enum(values), doc).default(json!(first))
}

/// `next`, then the names of `all`: a Display-menu pick's values.
fn pick_values<T: Copy>(all: &[T], name: fn(T) -> &'static str) -> Vec<&'static str> {
    std::iter::once("next")
        .chain(all.iter().map(|&x| name(x)))
        .collect()
}

/// Read a Display-menu pick: `next` (or nothing) for [`Pick::Next`], else a
/// name `T` parses.
fn parse_pick<T: std::str::FromStr>(by: Option<String>) -> Result<Pick<T>, String>
where
    T::Err: std::fmt::Display,
{
    match by.as_deref().map(str::trim) {
        None | Some("next") => Ok(Pick::Next),
        Some(name) => name.parse::<T>().map(Pick::To).map_err(|e| e.to_string()),
    }
}

impl HeadwayUiTool {
    /// The spec of every Headway UI tool, for [`notedeck::App::ui_tools`].
    pub(crate) fn specs() -> Vec<ToolSpec> {
        [
            card_spec(OPEN_CARD, "Open the current card's detail.", vec![]).titled("Open card"),
            card_spec(
                OPEN_EXPLAINER,
                "Open the explainer of the current card's review record in a browser.",
                vec![],
            )
            .titled("Open card's explainer"),
            card_spec(
                OPEN_PARENT,
                "Open the detail of the current card's parent, switching boards when the \
                 parent is on another. Only offered on a card that has a parent.",
                vec![],
            )
            .titled("Open parent card"),
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
            )
            .titled("Open card's agent session"),
            card_spec(
                OPEN_REVIEW,
                "Open the current card's review pane on its newest record.",
                vec![],
            )
            .titled("Review card's commit"),
            card_spec(
                SEND_BACK,
                "Send the current card back to In Progress, asking for a reason.",
                vec![],
            )
            .titled("Send card back"),
            card_spec(
                QUICK_COMMENT,
                "Ask for a one-line comment on the current card, post it, and send it to its \
                 review record's agentium session with any line comments drafted on the record.",
                vec![],
            )
            .titled("Comment on card"),
            card_spec(
                STEP_CARD,
                "Step to the next or previous card: the grid cursor's, the review queue's, \
                 or the card's column neighbour.",
                vec![enum_arg("dir", DIRS, "Which way to step.")],
            )
            .titled("Step to next or previous card"),
            ToolSpec::new(
                CURSOR_MOVE,
                "Move the board grid's or the issue list's cursor.",
                vec![enum_arg("to", CURSOR_TOS, "Where the cursor goes.")],
            )
            .ui()
            .titled("Move cursor"),
            ToolSpec::new(
                COMPOSE_CARD,
                "Open the add-card composer in the grid cursor's column, or in the issue \
                 list, the composer for a card in the cursor's group.",
                vec![],
            )
            .ui()
            .titled("New card"),
            ToolSpec::new(
                FOCUS_FILTER,
                "Focus the board grid's filter, or the issue list's query.",
                vec![],
            )
            .ui()
            .titled("Filter cards"),
            ToolSpec::new(
                OPEN_REVIEW_QUEUE,
                "Open the review queue: over the board's In Review column from the grid, \
                 over the card's In Review subissues from its detail.",
                vec![],
            )
            .ui()
            .titled("Open review queue"),
            ToolSpec::new(
                OPEN_LIST,
                "Open the issue list: every board's cards as one filterable list.",
                vec![],
            )
            .ui()
            .titled("Open issue list"),
            ToolSpec::new(
                NEXT_BOARD,
                "Switch to the next board, in the board switcher's order, wrapping around.",
                vec![],
            )
            .ui()
            .titled("Next board"),
            ToolSpec::new(
                PREV_BOARD,
                "Switch to the previous board, in the board switcher's order, wrapping around.",
                vec![],
            )
            .ui()
            .titled("Previous board"),
            ToolSpec::new(
                PICK_BOARD,
                "Open the board picker: every board, narrowed as you type, to switch to one.",
                vec![],
            )
            .ui()
            .titled("Switch board"),
            ToolSpec::new(CLEAR, "Drop the board grid's cursor.", vec![])
                .ui()
                .titled("Clear cursor"),
            ToolSpec::new(
                LIST_FOLD,
                "Fold the issue list's group headers: toggle, open or close the cursor's \
                 group, or close (all) or open (none) every group.",
                vec![enum_arg("to", FOLD_TOS, "What to fold.")],
            )
            .ui()
            .titled("Fold groups"),
            ToolSpec::new(
                LIST_STEP_GROUP,
                "Move the issue list's cursor to the next or previous group header.",
                vec![enum_arg("dir", DIRS, "Which way to step.")],
            )
            .ui()
            .titled("Step to next or previous group"),
            ToolSpec::new(
                LIST_MOVE_DOWN,
                "Move the issue list cursor's card one place later in its board's work \
                 order, past the next card in its group. Only while the list is ordered by \
                 work order, top first.",
                vec![],
            )
            .ui()
            .titled("Move card down in work order"),
            ToolSpec::new(
                LIST_MOVE_UP,
                "Move the issue list cursor's card one place earlier in its board's work \
                 order, past the card before it in its group. Only while the list is ordered \
                 by work order, top first.",
                vec![],
            )
            .ui()
            .titled("Move card up in work order"),
            ToolSpec::new(
                LIST_TAB,
                "Show one of the issue list's view tabs: the next or previous one, a built-in \
                 view (active, backlog, all), or a saved view by slug or name.",
                vec![
                    enum_arg("to", TAB_TOS, "A step or a built-in view."),
                    ToolArg::new(
                        "view",
                        ToolArgType::String,
                        "A saved view's slug or name, instead of `to`.",
                    ),
                ],
            )
            .ui()
            .titled("Switch list view"),
            ToolSpec::new(
                LIST_GROUP,
                "Group the issue list's rows: by status, priority, label, board or parent, \
                 none, or the next grouping in the Display menu's order.",
                vec![names_arg(
                    "by",
                    pick_values(&GroupBy::ALL, GroupBy::name),
                    "What to group by.",
                )],
            )
            .ui()
            .titled("Group list by"),
            ToolSpec::new(
                LIST_SUBGROUP,
                "Split each of the issue list's groups again: by status, priority, label, \
                 board or parent, none, or the next sub-grouping in the Display menu's order.",
                vec![names_arg(
                    "by",
                    pick_values(&GroupBy::ALL, GroupBy::name),
                    "What to sub-group by.",
                )],
            )
            .ui()
            .titled("Sub-group list by"),
            ToolSpec::new(
                LIST_ORDER,
                "Order the issue list's rows, run the ordering's natural way: manually, by \
                 priority, last updated, created, due date or work order, or the next ordering \
                 in the Display menu's order.",
                vec![names_arg(
                    "by",
                    pick_values(&SortBy::ALL, SortBy::name),
                    "What to order by.",
                )],
            )
            .ui()
            .titled("Order list by"),
            ToolSpec::new(
                LIST_ORDER_DIR,
                "Run the issue list's ordering the other way, or ascending or descending.",
                vec![enum_arg("dir", DIR_TOS, "Which way to run it.")],
            )
            .ui()
            .titled("Reverse list order"),
            ToolSpec::new(
                LIST_DISPLAY,
                "Show or hide one of the properties each issue-list row draws beside its \
                 title, as the Display menu's chips do.",
                vec![
                    names_arg(
                        "prop",
                        DisplayProp::ALL.iter().map(|p| p.name()).collect(),
                        "The property, by the name `headway list --show` reads.",
                    )
                    .required(true),
                    enum_arg(
                        "on",
                        SWITCH_WORDS,
                        "Show it (true), hide it (false), or flip it.",
                    ),
                ],
            )
            .ui()
            .titled("Show or hide row property"),
            ToolSpec::new(
                LIST_SAVE_VIEW,
                "Save what the issue list shows, its query and layout, as a new view: opens \
                 the view-name field.",
                vec![],
            )
            .ui()
            .titled("Save list view"),
            ToolSpec::new(
                SCROLL,
                "Scroll the review pane's diff or the card detail.",
                vec![enum_arg("to", SCROLL_TOS, "How far to scroll.")],
            )
            .ui()
            .titled("Scroll"),
            ToolSpec::new(
                STEP_FILE,
                "Scroll the review pane's diff to the next or previous file.",
                vec![enum_arg("dir", DIRS, "Which way to step.")],
            )
            .ui()
            .titled("Step to next or previous file"),
            ToolSpec::new(
                COMMENT,
                "Write a review comment on the picked diff lines, or on the commit.",
                vec![],
            )
            .ui()
            .titled("Write review comment"),
            ToolSpec::new(
                SEND_COMMENTS,
                "Send the review pane's drafted comments.",
                vec![],
            )
            .ui()
            .titled("Send review comments"),
            ToolSpec::new(
                BACK,
                "Leave the review queue, back a review pane out to its card, or leave the \
                 card detail or the issue list for the grid.",
                vec![],
            )
            .ui()
            .titled("Go back"),
        ]
        .into_iter()
        .map(|spec| spec.filed_under(crate::TOOL_CATEGORY))
        .collect()
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
            LIST_MOVE_DOWN => no_args(name, args, Self::ListMove(Vertical::Down))?,
            LIST_MOVE_UP => no_args(name, args, Self::ListMove(Vertical::Up))?,
            LIST_TAB => Self::ListLayout(ListLayout::Tab(parse_tab(name, args)?)),
            LIST_GROUP | LIST_SUBGROUP => {
                let ByArgs { by } = parse_args(name, args)?;
                let pick = parse_pick(by).map_err(|e| format!("'{name}': {e}"))?;
                Self::ListLayout(match name {
                    LIST_GROUP => ListLayout::Group(pick),
                    _ => ListLayout::SubGroup(pick),
                })
            }
            LIST_ORDER => {
                let ByArgs { by } = parse_args(name, args)?;
                let pick = parse_pick(by).map_err(|e| format!("'{name}': {e}"))?;
                Self::ListLayout(ListLayout::Order(pick))
            }
            LIST_ORDER_DIR => {
                let OrderDirArgs { dir } = parse_args(name, args)?;
                Self::ListLayout(ListLayout::OrderDir(match dir {
                    DirTo::Toggle => OrderDir::Toggle,
                    DirTo::Asc => OrderDir::Ascending,
                    DirTo::Desc => OrderDir::Descending,
                }))
            }
            LIST_DISPLAY => {
                let DisplayArgs { prop, on } = parse_args(name, args)?;
                let prop = prop.parse().map_err(|e| format!("'{name}': {e}"))?;
                let on = match on {
                    None | Some(SwitchArg::Word(SwitchWord::Toggle)) => None,
                    Some(SwitchArg::Bool(on)) => Some(on),
                    Some(SwitchArg::Word(SwitchWord::True)) => Some(true),
                    Some(SwitchArg::Word(SwitchWord::False)) => Some(false),
                };
                Self::ListLayout(ListLayout::Display(DisplaySwitch { prop, on }))
            }
            LIST_SAVE_VIEW => no_args(name, args, Self::ListLayout(ListLayout::SaveView))?,
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

/// `headway_list_tab`'s tab: `to`'s step or built-in, else `view`'s saved
/// view, else the next tab. Errors on both.
fn parse_tab(name: &str, args: &Value) -> Result<TabPick, String> {
    let TabArgs { to, view } = parse_args(name, args)?;
    Ok(match (to, view) {
        (Some(_), Some(_)) => {
            return Err(format!("'{name}' takes `to` or `view`, not both"));
        }
        (None, Some(view)) => TabPick::Saved(view),
        (None, None) | (Some(TabTo::Next), None) => TabPick::Next,
        (Some(TabTo::Prev), None) => TabPick::Prev,
        (Some(TabTo::Active), None) => TabPick::Builtin(BuiltinView::Active),
        (Some(TabTo::Backlog), None) => TabPick::Builtin(BuiltinView::Backlog),
        (Some(TabTo::All), None) => TabPick::Builtin(BuiltinView::All),
    })
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
/// The issue list's key scopes, its own first as the grid's. Its current card
/// is the cursor row's on whichever board that is
/// ([`BoardUiState::latch_current_ref`]), so the card keys act there.
const LIST_SCOPES: &[&str] = &["headway.list", "headway.card", "headway"];

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

    /// Make `card`, whose ref is `text`, the current card, or clear it with
    /// either missing: for the issue list, whose rows carry their refs ready
    /// made. Copies the ref into the buffer it keeps only when it changes.
    pub(crate) fn latch_ref(&mut self, card: Option<nostrdb_net::NoteId>, text: Option<&str>) {
        let (Some(card), Some(text)) = (card, text) else {
            self.card = None;
            return;
        };
        self.card = Some(card);
        if self.text != text {
            self.text.clear();
            self.text.push_str(text);
        }
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
        // the key answers rather than falling through. On a list row they
        // act on the row's board; `r`, `X` and `C` open its detail first.
        OPEN_EXPLAINER | OPEN_SESSION | OPEN_REVIEW | SEND_BACK | QUICK_COMMENT => card,
        // Only a subissue has a parent to open: on any other card the key
        // falls through, and a macro's step waits and aborts. Not from the
        // list, whose rows are already every card, parents and all.
        OPEN_PARENT => !list && current_card(state, at).is_some_and(|c| state.has_parent(c)),
        // The grid's step lands a missing cursor on the first card; the
        // queue's steps off a card that has left the board. The list's `j`/`k`
        // are its steps.
        STEP_CARD => (card && !list) || grid || at == KeyView::Queue,
        CURSOR_MOVE | FOCUS_FILTER => grid || list,
        COMPOSE_CARD => grid || (list && state.list_can_compose()),
        OPEN_LIST | PICK_BOARD => grid,
        LIST_FOLD | LIST_STEP_GROUP => list && state.list_has_groups(),
        // Only with a neighbour in its group to step past, so `J`/`K` fall
        // through otherwise.
        LIST_MOVE_DOWN => list && state.list_can_move(Vertical::Down),
        LIST_MOVE_UP => list && state.list_can_move(Vertical::Up),
        LIST_TAB | LIST_GROUP | LIST_SUBGROUP | LIST_ORDER | LIST_ORDER_DIR | LIST_DISPLAY
        | LIST_SAVE_VIEW => list,
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

    /// The arguments `spec` requires, each its first advertised value: the
    /// least a call of it can pass.
    fn required_args(spec: &ToolSpec) -> serde_json::Map<String, Value> {
        let schema = spec.json_schema();
        let required = schema["required"].as_array().expect("required");
        required
            .iter()
            .map(|arg| {
                let arg = arg.as_str().expect("an arg name");
                let first = schema["properties"][arg]["enum"][0].clone();
                assert!(
                    !first.is_null(),
                    "{}: required {arg} is no enum",
                    spec.name()
                );
                (arg.to_owned(), first)
            })
            .collect()
    }

    /// Every advertised spec is a UI tool that parses with no arguments but
    /// those it requires, and a tool that requires none takes `null` for
    /// none.
    #[test]
    fn every_spec_parses_with_its_defaults() {
        for spec in HeadwayUiTool::specs() {
            assert_eq!(spec.kind(), notedeck::ToolKind::Ui, "{}", spec.name());
            let least = required_args(&spec);
            HeadwayUiTool::parse(spec.name(), &Value::Object(least.clone()))
                .unwrap_or_else(|e| panic!("{} with no args: {e}", spec.name()));
            if least.is_empty() {
                HeadwayUiTool::parse(spec.name(), &Value::Null)
                    .unwrap_or_else(|e| panic!("{} with null args: {e}", spec.name()));
            } else {
                assert!(
                    HeadwayUiTool::parse(spec.name(), &json!({})).is_err(),
                    "{} without its required args",
                    spec.name()
                );
            }
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
                    let mut args = required_args(&spec);
                    args.insert(arg.clone(), value.clone());
                    HeadwayUiTool::parse(spec.name(), &Value::Object(args))
                        .unwrap_or_else(|e| panic!("{} {arg}={value}: {e}", spec.name()));
                }
            }
        }
    }

    /// The Display menu's tools advertise every grouping, ordering and row
    /// property `headway list` knows, by the names it reads, so the action
    /// bar's picker offers each.
    #[test]
    fn list_layout_tools_advertise_every_choice() {
        let values = |name: &str, arg: &str| -> Vec<String> {
            let spec = HeadwayUiTool::specs()
                .into_iter()
                .find(|s| s.name() == name)
                .expect("the spec");
            let schema = spec.json_schema();
            schema["properties"][arg]["enum"]
                .as_array()
                .expect("an enum")
                .iter()
                .map(|v| v.as_str().expect("a name").to_owned())
                .collect()
        };
        for by in GroupBy::ALL {
            assert!(values(LIST_GROUP, "by").contains(&by.name().to_owned()));
            assert!(values(LIST_SUBGROUP, "by").contains(&by.name().to_owned()));
        }
        for by in SortBy::ALL {
            assert!(values(LIST_ORDER, "by").contains(&by.name().to_owned()));
        }
        for prop in DisplayProp::ALL {
            assert!(values(LIST_DISPLAY, "prop").contains(&prop.name().to_owned()));
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
            (LIST_TAB, json!({ "to": "next", "view": "mine" })),
            (LIST_TAB, json!({ "to": "sideways" })),
            (LIST_GROUP, json!({ "by": "colour" })),
            (LIST_ORDER, json!({ "by": "alphabet" })),
            (LIST_ORDER_DIR, json!({ "dir": "up" })),
            (LIST_DISPLAY, json!({})),
            (LIST_DISPLAY, json!({ "prop": "nope" })),
            (LIST_DISPLAY, json!({ "prop": "due", "on": "maybe" })),
            (LIST_SAVE_VIEW, json!({ "name": "x" })),
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
        state.open_list(crate::ui::ListScope::All);
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
