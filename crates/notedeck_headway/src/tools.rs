//! Agent tools exposing this app's headway boards to AI backends over the
//! shared per-app tool interface ([`notedeck::AppTool`]).
//!
//! Each tool is a read-only nostrdb query that reuses headway core's board
//! folding ([`event::fold_board`]) and JSON serializers ([`event::board_json`] /
//! [`event::card_json`]), so a backend (Dave, `notedeck --mcp`) sees exactly the
//! same curated schema the CLI emits with `--json`. Boards are folded for the
//! currently-selected account — the pubkey that authors this user's boards.

use headway::event::{self, BoardView, Priority, resolve_card};
use headway::list::{self, LayoutGiven, ListViews, Listing};
use headway::query::{DisplayProps, GroupBy, Grouping, ListQuery, SortBy, Sorting, ViewSpec};
use headway::store::{self, BoardAction, NewCard};
use headway::views::{self, SavedView};
use headway::{boards, edit, render, wordid};
use nostrdb::{Ndb, Transaction};
use nostrdb_net::{NoteId, Pubkey};
use notedeck::{
    AppTool, ExplicitPublishApi, PrivateRelayTargets, RegisteredTool, ToolArg, ToolArgType,
    ToolContext, ToolSpec, fan_out_routed_event_frame,
};
use serde::{Deserialize, Serialize, Serializer};

use crate::cursor::{self, MoveTarget, Side, Vertical};
use crate::ui::attach::{AttachTarget, referenced};

mod attach;
mod board;
mod review;
use crate::teams;
use crate::ui::ViewFilter;
use headway::query::IssueFilter;

/// Every agent tool the Headway app contributes, ready to register.
pub fn tools() -> Vec<RegisteredTool> {
    vec![
        // Read.
        RegisteredTool::new(board::ListBoards),
        RegisteredTool::new(ShowBoard),
        RegisteredTool::new(ShowCard),
        RegisteredTool::new(ReadyItems),
        RegisteredTool::new(Grep),
        RegisteredTool::new(List),
        RegisteredTool::slow(review::Diff),
        RegisteredTool::slow(attach::Attach::default()),
        // Write.
        RegisteredTool::new(AddCard),
        RegisteredTool::new(MoveCard),
        RegisteredTool::new(SetTitle),
        RegisteredTool::new(SetDescription),
        RegisteredTool::new(SetLabels),
        RegisteredTool::new(SetPriority),
        RegisteredTool::new(SetDue),
        RegisteredTool::new(SetEstimate),
        RegisteredTool::new(SetParent),
        RegisteredTool::new(EdgeTool(Edge::Block)),
        RegisteredTool::new(EdgeTool(Edge::Unblock)),
        RegisteredTool::new(EdgeTool(Edge::Relate)),
        RegisteredTool::new(EdgeTool(Edge::Unrelate)),
        RegisteredTool::new(Seq),
        RegisteredTool::new(SetTerminal),
        RegisteredTool::new(CommentCard),
        RegisteredTool::new(review::Review),
        RegisteredTool::new(ArchiveCard),
        RegisteredTool::new(RestoreCard),
        RegisteredTool::new(DeleteCard),
        RegisteredTool::new(RenameBoard),
        RegisteredTool::new(board::SeedBoard),
        RegisteredTool::new(board::TombstoneTool(board::Tombstone::Delete)),
        RegisteredTool::new(board::TombstoneTool(board::Tombstone::Restore)),
        RegisteredTool::new(board::CrossBoardTool(board::CrossBoard::Link)),
        RegisteredTool::new(board::CrossBoardTool(board::CrossBoard::Move)),
        RegisteredTool::new(board::MigrateBoard),
        RegisteredTool::new(board::ShareBoard),
        RegisteredTool::new(SaveView),
        RegisteredTool::new(DeleteView),
    ]
}

/// The board `board_id` as the edit tools see it ([`resolve_target`]): an
/// own plaintext board folded by its author, a sealed or joined board by its
/// coordinate with every member's and comment key's events in it. The read
/// tools answer from it, so a card read back shows what an edit wrote, an
/// agent's comment on a sealed board included, as the CLI's `show` does.
fn board_view(cx: &ToolContext, board_id: &str) -> Result<BoardView, String> {
    let author = *cx.accounts.selected_account_pubkey();
    let teams = teams::teams_from_ndb(cx.ndb, &author);
    resolve_target(cx.ndb, &author, board_id, &teams).map(|target| target.view)
}

/// The variable a command line falls back to for a tool's `board`, as the
/// `headway` CLI reads it: the caller's board, which the app can't see.
const BOARD_ENV: &str = "HEADWAY_BOARD";

/// `headway_show_board`: a board's columns and cards.
struct ShowBoard;

/// Arguments for [`ShowBoard`].
#[derive(Deserialize)]
struct ShowBoardArgs {
    /// The board id (slug) to show; the primary board when omitted.
    #[serde(default)]
    board: Option<String>,
    /// List archived cards in full rather than as a count.
    #[serde(default)]
    archived: bool,
    /// Every board this account can read, not just `board`.
    #[serde(default)]
    all: bool,
}

/// [`ShowBoard`]'s answer: one folded board, which goes out as
/// [`event::board_json`] and renders as [`render::board`], or with `all`
/// every readable board, as `nd headway show --all` prints them.
#[derive(Debug)]
enum ShownBoard {
    One {
        view: BoardView,
        archived: bool,
    },
    All {
        boards: Vec<BoardView>,
        /// Whose boards are "own": the rest are labelled with their owner.
        me: Pubkey,
        archived: bool,
    },
}

impl Serialize for ShownBoard {
    /// One board as [`event::board_json`]; every board as an array of them,
    /// each carrying its `owner` hex, since slugs are only unique per owner.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::One { view, .. } => event::board_json(view).serialize(serializer),
            Self::All { boards, .. } => serializer.collect_seq(boards.iter().map(|view| {
                let mut board = event::board_json(view);
                board["owner"] = serde_json::json!(hex::encode(view.author));
                board
            })),
        }
    }
}

impl AppTool for ShowBoard {
    type Args = ShowBoardArgs;
    type Output = ShownBoard;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_show_board",
            "Show a headway board's columns and their cards as JSON. Omit `board` to show the primary board; `all` shows every board this account can read, as an array of boards each carrying its `owner`.",
            vec![
                ToolArg::new(
                    "board",
                    ToolArgType::String,
                    "The board id (slug) to show. Defaults to the primary board when omitted.",
                )
                .env(BOARD_ENV),
                ToolArg::new(
                    "archived",
                    ToolArgType::Boolean,
                    "List archived cards in full rather than as a count.",
                ),
                ToolArg::new(
                    "all",
                    ToolArgType::Boolean,
                    "Show every board this account can read (its own and those shared with it), not just one.",
                ),
            ],
        )
.titled("Show board")
.read_only()
.command(&["headway", "show"])
.filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        if args.all {
            let me = *cx.accounts.selected_account_pubkey();
            let roster = boards::Roster::read(cx.ndb, boards::Identity::own(me));
            let mut all = boards::list_boards(cx.ndb, &roster);
            all.extend(boards::list_shared_with_me(cx.ndb, &roster));
            return Ok(ShownBoard::All {
                boards: all,
                me,
                archived: args.archived,
            });
        }
        let board_id = args.board.as_deref().unwrap_or(store::BOARD_ID);
        Ok(ShownBoard::One {
            view: board_view(cx, board_id)?,
            archived: args.archived,
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(match out {
            ShownBoard::One { view, archived } => render::board(view, *archived, true),
            ShownBoard::All {
                boards,
                me,
                archived,
            } => render::all_boards(boards, me.bytes(), *archived, true),
        })
    }
}

/// `headway_show_card`: one card in full detail.
struct ShowCard;

/// Arguments for [`ShowCard`].
#[derive(Deserialize)]
struct ShowCardArgs {
    /// The card ref: `headway:<board>/<word-id>` (or `<board>/<word-id>`) or a hex
    /// id prefix.
    card: String,
    /// The name of a review source on the card to answer with, instead of
    /// the card.
    #[serde(default)]
    source: Option<String>,
    /// The board to resolve the card on (see [`Route::board`]).
    #[serde(flatten)]
    route: Route,
}

/// [`ShowCard`]'s answer: the card's board and its id. It goes out as
/// `nd headway show <card> --json` prints it ([`render::cards_json`], an array of
/// the one card with its column) and renders as [`render::card_detail`].
/// With `--source` it is that source's text instead, as a JSON string and as
/// the text exactly as stored.
#[derive(Debug)]
struct ShownCard {
    view: BoardView,
    id: NoteId,
    source: Option<String>,
}

impl ShownCard {
    /// The card, with where it sits; [`ShowCard::call`] only builds a
    /// `ShownCard` for a card it found.
    fn placed(&self) -> Option<render::PlacedCard<'_>> {
        render::find_card(&self.view, &self.id)
    }
}

impl Serialize for ShownCard {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if let Some(text) = &self.source {
            return text.serialize(serializer);
        }
        let cards: Vec<render::PlacedCard> = self.placed().into_iter().collect();
        render::cards_json(&self.view, &cards).serialize(serializer)
    }
}

impl AppTool for ShowCard {
    type Args = ShowCardArgs;
    type Output = ShownCard;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_show_card",
            "Show a single headway card in full, as a one-element array of the card (with the `column` it sits in), the shape `nd headway show <card> --json` prints — description, labels, priority, comments, activity, subissues, its blocking edges (`blocked` = held back by an unfinished prerequisite; `blocked_by` = the cards it depends on; `blocks` = the cards depending on it, each edge flagged `done` when cleared), and its `related` edges (undirected \"see also\" links — purely informational context, never a prerequisite or work-order) — as JSON. Address the card by its `headway:<board>/<word-id>` ref (or the scheme-less `<board>/<word-id>`) or a hex id prefix.",
            vec![
                ToolArg::new(
                    "card",
                    ToolArgType::String,
                    "The card ref: `headway:<board>/<word-id>` (or `<board>/<word-id>`) or a hex id prefix.",
                )
                .required(true)
                .positional(),
                ToolArg::new(
                    "source",
                    ToolArgType::String,
                    "Answer with the text of the review source of this file name instead, exactly as stored, from the newest review record carrying one.",
                ),
                card_board_arg(),
                default_board_arg(),
            ],
        )
.titled("Show card")
.read_only()
.command(&["headway", "show"])
.filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let board = args.route.board([args.card.as_str()])?;
        let view = board_view(cx, &board)?;
        let id = resolve_card(&view, &args.card)?;
        let Some(placed) = render::find_card(&view, &id) else {
            return Err(format!("no card matching '{}'", args.card));
        };
        let source = match args.source.as_deref() {
            Some(name) => Some(render::review_source(placed.card, name)?.to_owned()),
            None => None,
        };
        Ok(ShownCard { view, id, source })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        if let Some(text) = &out.source {
            return Some(text.clone());
        }
        let placed = out.placed()?;
        Some(render::card_detail(
            &out.view,
            placed.card,
            placed.column,
            true,
        ))
    }
}

/// `headway_ready_items`: what to work on next, from a container's ready
/// frontier — `nd headway next`.
struct ReadyItems;

/// Arguments for [`ReadyItems`].
#[derive(Deserialize)]
struct ReadyItemsArgs {
    /// Scope the frontier to a container: a card ref (its subtree) or the board id
    /// (the whole board). The board root when omitted.
    #[serde(default)]
    container: Option<String>,
    /// The whole frontier rather than the single next card.
    #[serde(default)]
    ready: bool,
    /// Cap the number of items returned.
    #[serde(default)]
    limit: Option<usize>,
    /// Keep only cards carrying every one of these labels.
    #[serde(default, deserialize_with = "list_or_csv")]
    labels: Vec<String>,
    #[serde(flatten)]
    route: Route,
}

impl AppTool for ReadyItems {
    type Args = ReadyItemsArgs;
    type Output = Vec<render::ReadyCard>;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_ready_items",
            "What to work on next on a headway board, as `nd headway next` answers it: the cards workable right now — not done, not already In Progress (someone has it), not blocked by an unfinished prerequisite, and not a parent whose real work is its subissues — in work-order (seq-ranked, then creation order), each as `{ref, id, title}`. By default only the single next card; `ready` gives the whole frontier, the parallel-dispatch set a pool of agents can fan out over, and `limit` caps either. Omit `container` for the whole board, or pass a card ref to scope to that card's subtree. `labels` keeps only cards carrying every one of them; it never changes what counts as blocked or done.",
            vec![
                ToolArg::new(
                    "container",
                    ToolArgType::String,
                    "Scope the frontier to a container: a card ref (`headway:<board>/<word-id>`) for its subtree, or the board id for the whole board. Defaults to the board root.",
                )
                .long("in"),
                ToolArg::new(
                    "ready",
                    ToolArgType::Boolean,
                    "The whole ready frontier, not just the single next card.",
                ),
                ToolArg::new(
                    "limit",
                    ToolArgType::Integer,
                    "Cap the number of items returned.",
                )
                .short('n'),
                ToolArg::new(
                    "labels",
                    ToolArgType::List,
                    "Keep only cards carrying every one of these labels.",
                )
                .short('l')
                .long("label"),
                card_board_arg(),
                default_board_arg(),
            ],
        )
.titled("List ready cards")
.read_only()
.command(&["headway", "next"])
.filed_under(crate::TOOL_CATEGORY)
    }

    /// Unlike the CLI's `next`, this never refuses an implicit board: the
    /// CLI's refusal guards its persisted current board, which another
    /// session can switch between two commands, and a tool has none. Its
    /// fallbacks, the caller's `default_board` and the primary board, can't
    /// move under it.
    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let board = args.route.board(args.container.as_deref())?;
        let view = board_view(cx, &board)?;
        let container = edit::container_or_root(&view, args.container.as_deref())?;
        Ok(render::next_cards(
            &view,
            &container,
            &args.labels,
            args.ready,
            args.limit,
        ))
    }

    /// One ref and title a line, as `nd headway next` prints them; nothing for an
    /// empty frontier, so `$(nd headway next)` is empty when nothing is ready.
    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(render::next_lines(out, true))
    }
}

/// The boards a cross-board read (`grep`, `list`) reads, and the `--in` card
/// whose subtree it narrows to.
struct ReadScope {
    boards: Vec<BoardView>,
    /// `--in <card>`: that card and its descendants; `None` for a whole board
    /// (no `--in`, or `--in <board-slug>`).
    subtree: Option<[u8; 32]>,
}

/// Gather a [`ReadScope`], as the `headway` CLI's `read_scope` does: with no
/// `board`, every board this account can read (its own and those shared
/// with it, as `show --all` lists them); otherwise the one board `board`
/// ([`board_view`]), narrowed to `container`'s subtree when it names a card.
/// Each board is folded once.
fn read_scope(
    cx: &ToolContext,
    board: Option<&str>,
    container: Option<&str>,
) -> Result<ReadScope, String> {
    let Some(board) = board else {
        if container.is_some() {
            return Err("--in reads one card's subtree on its own board — drop --all".into());
        }
        let me = *cx.accounts.selected_account_pubkey();
        let roster = boards::Roster::read(cx.ndb, boards::Identity::own(me));
        let mut all = boards::list_boards(cx.ndb, &roster);
        all.extend(boards::list_shared_with_me(cx.ndb, &roster));
        return Ok(ReadScope {
            boards: all,
            subtree: None,
        });
    };
    let view = board_view(cx, board)?;
    // `--in <board-slug>` is the whole board, the same as no `--in`.
    let subtree = match container.map(|sel| edit::resolve_container(&view, sel)) {
        Some(Ok(event::Container::Card(id))) => Some(id),
        Some(Err(e)) => return Err(e),
        _ => None,
    };
    Ok(ReadScope {
        boards: vec![view],
        subtree,
    })
}

/// Whether `container` is a self-routing card ref, one that names its own
/// board (`headway:<board>/<word-id>`), which the CLI counts as naming the
/// board outright.
fn names_board(container: Option<&str>) -> bool {
    container.and_then(wordid::parse_ref).is_some()
}

/// `headway_grep`: search card text — `nd headway grep`.
struct Grep;

/// Arguments for [`Grep`].
#[derive(Deserialize)]
struct GrepArgs {
    /// The regular expression to search for, smart-case unless told.
    pattern: String,
    /// Keep only this card and its descendants (`--in`).
    #[serde(default)]
    container: Option<String>,
    /// Every board. The default already, unless `board` or `container`
    /// narrows the search; with `board`, it widens it again.
    #[serde(default)]
    all: bool,
    /// `-i`: always fold case.
    #[serde(default)]
    ignore_case: bool,
    /// `-s`: never fold case.
    #[serde(default)]
    case_sensitive: bool,
    #[serde(flatten)]
    route: Route,
}

impl GrepArgs {
    /// The pattern compiled under its case mode, smart-case by default.
    fn compile(&self) -> Result<headway::grep::Regex, String> {
        let case = match (self.ignore_case, self.case_sensitive) {
            (true, true) => return Err("pass one of --ignore-case and --case-sensitive".into()),
            (true, false) => headway::grep::CaseMode::Insensitive,
            (false, true) => headway::grep::CaseMode::Sensitive,
            (false, false) => headway::grep::CaseMode::Smart,
        };
        headway::grep::compile_pattern(&self.pattern, case)
    }

    /// The board the search is narrowed to, or `None` for every board: the
    /// CLI searches every board unless `--board` or `--in` narrows it, and
    /// the caller's own board (`$HEADWAY_BOARD`) never does.
    fn board(&self) -> Result<Option<String>, String> {
        let container = self.container.as_deref();
        if self.all || (self.route.board.is_none() && container.is_none()) {
            return Ok(None);
        }
        self.route.board(container).map(Some)
    }
}

/// [`Grep`]'s answer: the boards searched and the pattern, which go out as
/// [`headway::grep::hits_json`] and render as [`headway::grep::text`].
struct Grepped {
    scope: ReadScope,
    pattern: headway::grep::Regex,
}

impl Grepped {
    /// The cards that matched, most recent first.
    fn hits(&self) -> Vec<headway::grep::GrepHit<'_>> {
        headway::grep::search(&self.scope.boards, self.scope.subtree, &self.pattern)
    }
}

impl Serialize for Grepped {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        headway::grep::hits_json(&self.hits()).serialize(serializer)
    }
}

impl AppTool for Grep {
    type Args = GrepArgs;
    type Output = Grepped;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_grep",
            "Search headway card text — titles, descriptions, comments and review comments — as `nd headway grep` does: every board this account can read, archived cards included, unless `board` or `container` narrows it. Smart-case: a pattern with an uppercase letter is case-sensitive. Answers an array of cards, most recent match first, each `{ref, id, board, title, column, matches: [{field, text, created_at}]}`, one row per matching line.",
            vec![
                ToolArg::new(
                    "pattern",
                    ToolArgType::String,
                    "The regular expression to search for; quote it if it has spaces.",
                )
                .required(true)
                .positional(),
                ToolArg::new(
                    "container",
                    ToolArgType::String,
                    "Search only this card (`headway:<board>/<word-id>`) and its descendants, on its own board.",
                )
                .long("in"),
                ToolArg::new(
                    "all",
                    ToolArgType::Boolean,
                    "Search every board even when `board` names one. Every board is the default already.",
                ),
                ToolArg::new(
                    "archived",
                    ToolArgType::Boolean,
                    "Accepted for the CLI's sake: archived cards are always searched.",
                ),
                ToolArg::new(
                    "ignore_case",
                    ToolArgType::Boolean,
                    "Always fold case.",
                )
                .short('i'),
                ToolArg::new(
                    "case_sensitive",
                    ToolArgType::Boolean,
                    "Never fold case.",
                )
                .short('s'),
                ToolArg::new(
                    "board",
                    ToolArgType::String,
                    "Search only this board (slug). Defaults to every board.",
                ),
                default_board_arg(),
            ],
        )
        .titled("Search cards")
        .read_only()
        .command(&["headway", "grep"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let pattern = args.compile()?;
        let board = args.board()?;
        let scope = read_scope(cx, board.as_deref(), args.container.as_deref())?;
        Ok(Grepped { scope, pattern })
    }

    /// Each card's header and its matching lines, as `nd headway grep` prints
    /// them, or `no matches`.
    fn render(&self, out: &Self::Output) -> Option<String> {
        let text = headway::grep::text(&out.hits(), &out.pattern, true);
        if text.is_empty() {
            return Some(format!("{}\n", headway::grep::NO_MATCHES));
        }
        Some(text)
    }
}

/// The query and layout arguments `list` and `--save-view` share: what a
/// `nd headway list` command line says beyond the saved-view flags.
#[derive(Deserialize)]
struct ListLayoutArgs {
    /// The query's terms, as one string.
    #[serde(default)]
    query: Option<String>,
    /// Keep only this card and its descendants (`--in`).
    #[serde(default)]
    container: Option<String>,
    /// Done cards too, when the query says nothing about state.
    #[serde(default)]
    all: bool,
    /// `--show`: the properties a row shows, a comma list.
    #[serde(default)]
    show: Option<String>,
    /// `--hide`: properties taken from what a row shows, a comma list.
    #[serde(default)]
    hide: Option<String>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    sub_group: Option<String>,
    /// `--empty`: groups no card is in, too.
    #[serde(default)]
    empty: bool,
    #[serde(default)]
    sort: Option<String>,
    #[serde(default)]
    descending: bool,
    #[serde(default)]
    ascending: bool,
    #[serde(default)]
    completed_by_recency: bool,
    /// `--view`: lay the list out as this saved or built-in view.
    #[serde(default)]
    view: Option<String>,
    #[serde(flatten)]
    route: Route,
}

/// A `list` run read off [`ListLayoutArgs`]: its own view and the board it
/// names outright.
struct ListRun {
    /// The query words and layout flags, as a view.
    spec: ViewSpec,
    /// `--view`, and which layout flag groups were passed over it.
    views: ListViews,
    /// The board `board` or a self-routing `container` names, which narrows
    /// the list to it; `None` reads every board.
    explicit: Option<String>,
}

impl ListLayoutArgs {
    /// The run's own view, as the `headway` CLI reads its flags.
    fn run(&self) -> Result<ListRun, String> {
        let mut given = LayoutGiven::default();
        let parse_props = |list: &str| DisplayProps::parse_list(list).map_err(|e| e.to_string());
        let show = self.show.as_deref().map(parse_props).transpose()?;
        let hide = match self.hide.as_deref() {
            Some(list) => parse_props(list)?,
            None => DisplayProps::NONE,
        };
        given.display = self.show.is_some() || self.hide.is_some();

        let mut grouping = Grouping::default();
        if let Some(by) = &self.group {
            grouping.by = by.parse::<GroupBy>().map_err(|e| e.to_string())?;
        }
        if let Some(sub) = &self.sub_group {
            grouping.sub = sub.parse::<GroupBy>().map_err(|e| e.to_string())?;
        }
        grouping.show_empty = self.empty;
        given.grouping = self.group.is_some() || self.sub_group.is_some() || self.empty;

        let by = match &self.sort {
            Some(by) => by.parse::<SortBy>().map_err(|e| e.to_string())?,
            None => SortBy::default(),
        };
        let descending = match (self.descending, self.ascending) {
            (true, true) => return Err("pass one of --descending and --ascending".into()),
            (true, false) => true,
            (false, true) => false,
            (false, false) => by.natural_descending(),
        };
        let sorting = Sorting {
            descending,
            completed_by_recency: self.completed_by_recency,
            ..Sorting::by(by)
        };
        given.sorting =
            self.sort.is_some() || self.descending || self.ascending || self.completed_by_recency;

        let container = self.container.as_deref();
        let explicit = match self.route.board.is_some() || names_board(container) {
            true => Some(self.route.board(container)?),
            false => None,
        };
        Ok(ListRun {
            spec: ViewSpec {
                name: String::new(),
                query: self.query.clone().unwrap_or_default(),
                grouping,
                sorting,
                display: DisplayProps::shown_hidden(show, hide),
                hide_subissues: false,
                board: explicit.clone(),
            },
            views: ListViews {
                apply: self.view.clone(),
                given,
                ..ListViews::default()
            },
            explicit,
        })
    }

    /// The view the run shows: its own over `--view`'s (see
    /// [`list::compose_view`]), `--all` adding done cards.
    fn compose(&self, saved: &[SavedView]) -> Result<ViewSpec, String> {
        let run = self.run()?;
        list::compose_view(saved, &run.views, run.spec, self.all)
    }
}

/// The arguments [`ListLayoutArgs`] reads, in the order `nd headway list
/// --help` lists them: the query words, then the flags.
fn list_layout_args() -> Vec<ToolArg> {
    vec![
        ToolArg::new(
            "query",
            ToolArgType::String,
            "The query, as the board filter reads it: terms AND, e.g. `p:high label:ui is:done col:backlog due:any`, or a pasted card ref. With no `is:` term, only open cards.",
        )
        .rest(),
        ToolArg::new(
            "container",
            ToolArgType::String,
            "List only this card (`headway:<board>/<word-id>`) and its descendants, on its own board.",
        )
        .long("in"),
        ToolArg::new(
            "all",
            ToolArgType::Boolean,
            "Done cards too, when the query says nothing about state (`is:any`).",
        ),
        ToolArg::new(
            "show",
            ToolArgType::String,
            "The properties a row shows, a comma list: priority, id, status, subissues, labels, estimate, blockers, due, created, updated, board.",
        ),
        ToolArg::new(
            "hide",
            ToolArgType::String,
            "Properties to take from what a row shows, a comma list.",
        ),
        ToolArg::new(
            "group",
            ToolArgType::String,
            "Group the rows: none, status, priority, label, board or parent.",
        ),
        ToolArg::new(
            "sub_group",
            ToolArgType::String,
            "Group each group's rows again, by the same names as `group`.",
        ),
        ToolArg::new(
            "empty",
            ToolArgType::Boolean,
            "Show the groups no card is in too.",
        ),
        ToolArg::new(
            "sort",
            ToolArgType::String,
            "Order the rows: manual (board order, the default), priority, updated, created, due or work-order (which marks the ready frontier).",
        ),
        ToolArg::new(
            "descending",
            ToolArgType::Boolean,
            "Sort descending, over the order's natural direction.",
        ),
        ToolArg::new(
            "ascending",
            ToolArgType::Boolean,
            "Sort ascending, over the order's natural direction.",
        ),
        ToolArg::new(
            "completed_by_recency",
            ToolArgType::Boolean,
            "Order done cards by when they were finished.",
        ),
        ToolArg::new(
            "view",
            ToolArgType::String,
            "Lay the list out as this saved or built-in view (Active, Backlog, All issues); the query narrows it and a layout flag passed replaces its own.",
        ),
        ToolArg::new(
            "board",
            ToolArgType::String,
            "List only this board (slug). Defaults to every board, or a view's own board.",
        ),
        default_board_arg(),
    ]
}

/// `headway_list`: the cards a query keeps, or the views — `nd headway list`.
struct List;

/// Arguments for [`List`].
#[derive(Deserialize)]
struct ListArgs {
    #[serde(flatten)]
    layout: ListLayoutArgs,
    /// `-n`: keep the first rows.
    #[serde(default)]
    limit: Option<usize>,
    /// `--views`: list the views instead of the cards.
    #[serde(default)]
    views: bool,
}

/// [`List`]'s answer: the cards, which go out as [`Listing::json`] and
/// render as [`Listing::text`], or with `views` the views.
enum Listed {
    Cards {
        scope: ReadScope,
        query: ListQuery,
        limit: Option<usize>,
    },
    Views {
        saved: Vec<SavedView>,
        /// The list's own layout, which a built-in view takes.
        layout: ViewSpec,
    },
}

impl Serialize for Listed {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Listed::Cards {
                scope,
                query,
                limit,
            } => Listing::new(&scope.boards, query, *limit)
                .json()
                .serialize(serializer),
            Listed::Views { saved, layout } => {
                list::views_json(saved, layout).serialize(serializer)
            }
        }
    }
}

impl AppTool for List {
    type Args = ListArgs;
    type Output = Listed;

    fn spec(&self) -> ToolSpec {
        let mut args = list_layout_args();
        args.extend([
            ToolArg::new(
                "limit",
                ToolArgType::Integer,
                "Keep the first rows of the sorted list.",
            )
            .short('n'),
            ToolArg::new(
                "views",
                ToolArgType::Boolean,
                "List the views — built-in, then saved — instead of the cards.",
            ),
        ]);
        ToolSpec::new(
            "headway_list",
            "List the headway cards a query keeps, as `nd headway list` does: every board this account can read unless `board` or `container` narrows it, open cards unless the query or `all` says otherwise. Answers an array of cards as `show <card> --json` renders them plus their `board`, `column` and, sorted by work-order, `ready`; grouped, an array of `{group, count, rows, subgroups}`. `views` lists the saved and built-in views instead.",
            args,
        )
        .titled("List cards")
        .read_only()
        .command(&["headway", "list"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    /// As the CLI reads it: a view scoped to a board reads that board,
    /// unless the run named a board or a container itself.
    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let me = *cx.accounts.selected_account_pubkey();
        let saved = views::load_views(cx.ndb, &me);
        let layout = args.layout;
        if args.views {
            let layout = layout.run()?.spec;
            return Ok(Listed::Views { saved, layout });
        }
        let explicit = layout.run()?.explicit;
        let spec = layout.compose(&saved)?;
        let container = layout.container.as_deref();
        let view_board = spec
            .board
            .clone()
            .filter(|_| explicit.is_none() && container.is_none());
        let board = match (&view_board, &explicit, container) {
            (Some(board), _, _) => Some(board.clone()),
            (None, None, None) => None,
            (None, _, _) => Some(layout.route.board(container)?),
        };
        let scope = read_scope(cx, board.as_deref(), container)?;
        let query = ListQuery {
            filter: spec.filter(),
            within: scope.subtree,
            board: view_board,
            display: spec.display,
            grouping: spec.grouping,
            sorting: spec.sorting,
        };
        Ok(Listed::Cards {
            scope,
            query,
            limit: args.limit,
        })
    }

    /// One row per card, or the groups' headers and rows, as `nd headway list`
    /// prints them, or `no cards`; with `views`, one view a line.
    fn render(&self, out: &Self::Output) -> Option<String> {
        match out {
            Listed::Views { saved, layout } => Some(list::views_text(saved, layout)),
            Listed::Cards {
                scope,
                query,
                limit,
            } => {
                let text = Listing::new(&scope.boards, query, *limit).text(true);
                match text.is_empty() {
                    true => Some(format!("{}\n", list::NO_CARDS)),
                    false => Some(text),
                }
            }
        }
    }
}

// ===== write tools =====

/// A [`store::Publisher`] that fans locally-ingested board events out to the
/// selected account's private relays through a [`ToolContext`]'s publisher —
/// the tool-side analogue of the Headway app's own `PrivateRelayPublisher` —
/// counting what it sends for the edit's [`EditAck`].
///
/// A write is a plaintext event on an own board and a kind-1081 envelope on a
/// shared one, so each frame is routed by its kind: an encrypted-only private
/// relay gets the envelopes and never the plaintext.
///
/// Each note it is told was ingested goes in the context's
/// [`written`](ToolContext::written), so `nd` answers once the edit reads
/// back rather than while nostrdb is still unwrapping it.
struct RelayFanout<'x> {
    api: ExplicitPublishApi<'x>,
    targets: PrivateRelayTargets,
    /// The frames published so far.
    sent: usize,
    /// The context's [`written`](ToolContext::written).
    written: &'x mut Vec<NoteId>,
}

impl store::Publisher for RelayFanout<'_> {
    fn publish(&mut self, event_frame: &str) {
        fan_out_routed_event_frame(&mut self.api, event_frame, &self.targets);
        self.sent += 1;
    }

    fn ingested(&mut self, id: &NoteId) {
        self.written.push(*id);
    }
}

/// A [`RelayFanout`] to the selected account's private relays through `cx`'s
/// publisher, or an error in a read-only context, so a write is never
/// silently dropped.
fn publisher<'x>(cx: &'x mut ToolContext<'_>) -> Result<RelayFanout<'x>, String> {
    let targets = cx.accounts.selected_account_private_relay_set().targets();
    let api = cx
        .remote
        .as_mut()
        .ok_or("publishing is unavailable in this context")?
        .publisher();
    Ok(RelayFanout {
        api,
        targets,
        sent: 0,
        written: &mut cx.written,
    })
}

/// What an edit tool answers: the ack `headway <edit> --json` prints,
/// `{ok, events}`, plus the new card's `card` and `ref` after an add, or
/// `noop` when the card was already as asked and nothing was written. The
/// change ingests and publishes asynchronously, so re-query with
/// `headway_show_board` / `headway_show_card` to see the settled state.
#[derive(Serialize, Debug)]
struct EditAck {
    ok: bool,
    /// How many events the edit signed and published.
    events: usize,
    /// The hex id of the card an add created.
    #[serde(skip_serializing_if = "Option::is_none")]
    card: Option<String>,
    /// That card's `headway:<board>/<word-id>` ref.
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    card_ref: Option<String>,
    /// Why nothing was written: the card was already in the asked-for state.
    #[serde(skip_serializing_if = "Option::is_none")]
    noop: Option<String>,
}

impl EditAck {
    /// The ack as the CLI prints it: `ok (2 events)`, then the new card's
    /// ref (muted) after an add, or why nothing was written.
    fn render(&self) -> String {
        match (&self.noop, &self.card_ref) {
            (Some(why), _) => format!("ok (0 events) — {why}\n"),
            (None, Some(card_ref)) => format!(
                "ok ({} events) — {}\n",
                self.events,
                render::dim(true, card_ref)
            ),
            (None, None) => format!("ok ({} events)\n", self.events),
        }
    }
}

/// A list argument as a model or a key binding may send it: a JSON array of
/// strings, or one comma-separated string. Either way each item is trimmed
/// and blanks dropped, so `["a", "b,c"]` and `"a, b,c"` are the same three.
fn list_or_csv<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Labels {
        List(Vec<String>),
        Csv(String),
    }
    let items = match Labels::deserialize(de)? {
        Labels::List(items) => items,
        Labels::Csv(csv) => vec![csv],
    };
    Ok(items
        .iter()
        .flat_map(|item| item.split(','))
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

/// An estimate as a model may send it, a number or the text a command line
/// types (`3`, `none`), parsed as `nd headway estimate` parses it.
#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum Points {
    Number(u32),
    Text(String),
}

impl Points {
    /// The estimate to set, `None` to clear it.
    fn parse(&self) -> Result<Option<u32>, String> {
        match self {
            Self::Number(n) => Ok(Some(*n)),
            Self::Text(text) => edit::parse_estimate(text),
        }
    }
}

/// A resolved edit target: the folded board view and the SNS channel to seal its
/// edits into — `None` for an own plaintext board, `Some` for a shared board.
struct ResolvedTarget {
    view: BoardView,
    channel: Option<store::SnsChannel>,
}

/// Resolve a mutation's target board and the SNS channel to seal its edits into,
/// mirroring the app's [`render`](crate) coordinate-aware routing.
///
/// A board is shared iff its coordinate is in `teams`, matched on the full
/// coordinate — so a board you *own* and shared (self-shared team-of-one, whose
/// coordinate is your own) folds by coordinate (every member's cards) and seals
/// every edit, instead of folding author-scoped and leaking a plaintext rumor.
/// That own-coordinate case is checked first; a private own board (not in the
/// roster) then wins the author-scoped fold on a bare-slug collision with a joined
/// board; otherwise a joined shared board of that slug is folded and sealed. A
/// shared board whose channel can't be derived, or that hasn't synced yet, is
/// *refused* rather than edited unsealed.
fn resolve_target(
    ndb: &Ndb,
    author: &Pubkey,
    board_id: &str,
    teams: &[teams::Team],
) -> Result<ResolvedTarget, String> {
    let txn = Transaction::new(ndb).map_err(|e| format!("failed to open db: {e}"))?;

    // Your own board, matched by coordinate against the roster first: if shared,
    // fold + seal it rather than leak plaintext via the author-scoped fold below.
    let own_coord = event::board_address(author, board_id);
    if let Some(team) = teams.iter().find(|t| t.board_addr == own_coord) {
        return resolve_shared(ndb, &txn, board_id, team, teams);
    }

    // Own plaintext board wins on a bare-slug collision with a joined board.
    if let Some(view) = event::load_board(ndb, &txn, author, board_id) {
        return Ok(ResolvedTarget {
            view,
            channel: None,
        });
    }

    // Not ours: only a joined shared board of that slug is editable, and only sealed.
    let team = teams
        .iter()
        .find(|t| t.board_slug() == Some(board_id))
        .ok_or_else(|| format!("no board '{board_id}'"))?;
    resolve_shared(ndb, &txn, board_id, team, teams)
}

/// Fold a shared board by coordinate and derive its sealing channel. Refuses
/// (rather than editing unsealed) when the channel key is unusable or the board
/// hasn't synced yet.
fn resolve_shared(
    ndb: &Ndb,
    txn: &Transaction,
    board_id: &str,
    team: &teams::Team,
    roster: &[teams::Team],
) -> Result<ResolvedTarget, String> {
    // Seal into the board's *primary* channel, but fold every channel it has —
    // see `teams::board_channels`.
    let primary = teams::board_channels(roster, &team.board_addr)
        .first()
        .copied()
        .unwrap_or(team);
    let channel = primary
        .sns_keys()
        .map(|keys| store::SnsChannel { keys })
        .ok_or_else(|| {
            format!(
                "shared board '{board_id}' has an unusable team key; refusing to publish unsealed"
            )
        })?;
    let channels = teams::board_channel_pubkeys(roster, &team.board_addr);
    let view = event::load_shared_board(ndb, txn, &team.board_addr, &channels)
        .ok_or_else(|| format!("shared board '{board_id}' has not synced yet"))?;
    Ok(ResolvedTarget {
        view,
        channel: Some(channel),
    })
}

/// Apply a board mutation for the selected account, fan the resulting events
/// out to its private relays, and answer with the CLI's ack.
///
/// Resolves `board_id` (own or joined-shared, see [`resolve_target`]) so `build`
/// can resolve card/column refs against the live view, then signs, ingests, and
/// publishes the action — sealing into the board's SNS channel when it's shared.
/// A comment is signed by the context's comment key when it carries one, and
/// refused on a plaintext board, where it would never show (see
/// [`store::signing_key`]). Errors when the account is watch-only or the
/// context has no publisher (a read-only context), so a change is never
/// silently dropped.
///
/// An edge or parent edit [`store::apply_outcome`] declines is refused with
/// its reason, unless the card was already as asked: that is an `ok` with
/// nothing written, as the CLI answers it.
fn mutate(
    cx: &mut ToolContext,
    board_id: &str,
    build: impl FnOnce(&BoardView) -> Result<BoardAction, String>,
) -> Result<EditAck, String> {
    let author = *cx.accounts.selected_account_pubkey();

    // Resolve the board and its channel first, so a bad card/column ref reports the
    // specific resolution error rather than a generic capability error. The roster
    // is read straight from nostrdb (see [`teams::teams_from_ndb`]).
    let teams = teams::teams_from_ndb(cx.ndb, &author);
    let ResolvedTarget { view, channel } = resolve_target(cx.ndb, &author, board_id, &teams)?;
    if view.deleted {
        return Err(render::deleted_board_error(board_id));
    }
    let action = build(&view)?;

    let secret = cx
        .accounts
        .selected_filled()
        .map(|f| f.secret_key.secret_bytes())
        .ok_or("the selected account is watch-only and cannot sign changes")?;
    // A comment from a caller with its own comment key (an agent, through
    // `nd`) is signed by that key, as the CLI signs it; it reaches only a tool
    // that accepts one.
    let comment_secret = cx.comment_signer.map(|key| key.secret_key.secret_bytes());
    let signing_key = store::signing_key(
        &action,
        &secret,
        comment_secret.as_ref(),
        channel.as_ref(),
        board_id,
    )?;

    let ndb = cx.ndb;
    let mut publisher = publisher(cx)?;
    let store::ApplyOutcome { declined, created } = store::apply_outcome(
        ndb,
        board_id,
        &view,
        &author,
        &store::Signer::new(signing_key, channel.as_ref()),
        action,
        &mut publisher,
    );
    if let Some(declined) = declined {
        let why = render::declined_message(&view, &declined);
        if !declined.reason.is_noop() {
            return Err(format!("refused: {why}"));
        }
        return Ok(EditAck {
            ok: true,
            events: 0,
            card: None,
            card_ref: None,
            noop: Some(why),
        });
    }
    if publisher.sent == 0 {
        return Err("the edit produced no events (unknown card or column?)".to_owned());
    }
    Ok(EditAck {
        ok: true,
        events: publisher.sent,
        card: created.map(|id| id.hex()),
        card_ref: created.map(|id| render::plain_ref(&view, &id)),
        noop: None,
    })
}

/// Which board a card tool runs on, from the two arguments every card tool
/// takes ([`card_board_arg`] and [`default_board_arg`]) and the card refs it
/// was given.
#[derive(Deserialize, Default, Debug)]
struct Route {
    /// The board named outright: `--board` on a command line.
    #[serde(default)]
    board: Option<String>,
    /// The caller's board when nothing names one: `$HEADWAY_BOARD` on a
    /// command line.
    #[serde(default)]
    default_board: Option<String>,
}

impl Route {
    /// The board, by the `headway` CLI's rule: `board` and the board any
    /// self-routing ref in `refs` names (`headway:<board>/<word-id>`) must
    /// agree, and whichever is given names it; else `default_board`; else
    /// the primary board. So a ref pasted from `show` works wherever it goes,
    /// and a caller's `$HEADWAY_BOARD` never sends it to the wrong board.
    /// Bare word-ids and hex prefixes route nowhere, and resolve on the
    /// board picked.
    fn board<'r>(&self, refs: impl IntoIterator<Item = &'r str>) -> Result<String, String> {
        let mut named: Option<String> = None;
        for slug in refs
            .into_iter()
            .filter_map(wordid::parse_ref)
            .map(|r| r.board.to_lowercase())
        {
            match &named {
                Some(cur) if *cur != slug => {
                    return Err(format!(
                        "card refs name different boards ('{cur}' and '{slug}')"
                    ));
                }
                _ => named = Some(slug),
            }
        }
        match (&self.board, named) {
            (Some(board), Some(named)) if *board != named => Err(format!(
                "board '{board}' conflicts with card ref board '{named}'"
            )),
            (Some(board), _) => Ok(board.clone()),
            (None, Some(named)) => Ok(named),
            (None, None) => Ok(self
                .default_board
                .clone()
                .unwrap_or_else(|| store::BOARD_ID.to_owned())),
        }
    }
}

/// `headway_add_card`: create a card on a board.
struct AddCard;

/// Arguments for [`AddCard`].
#[derive(Deserialize)]
struct AddCardArgs {
    title: String,
    /// Target column id or name; the first column when omitted.
    #[serde(default)]
    column: Option<String>,
    /// Labels to tag the new card with.
    #[serde(default, deserialize_with = "list_or_csv")]
    labels: Vec<String>,
    /// A card ref to parent the new card under (create it as a subissue).
    #[serde(default)]
    parent: Option<String>,
    /// Its starting description.
    #[serde(default)]
    description: Option<String>,
    /// Its starting priority.
    #[serde(default)]
    priority: Option<String>,
    /// Its starting estimate.
    #[serde(default)]
    estimate: Option<Points>,
    /// Its starting due date, `YYYY-MM-DD`.
    #[serde(default)]
    due: Option<String>,
    #[serde(flatten)]
    route: Route,
}

impl AppTool for AddCard {
    type Args = AddCardArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_add_card",
            "Create a card on a headway board, by default in the first column of the primary board. Answers `{ok, events, card, ref}`: `ref` is the new card's `headway:<board>/<word-id>` ref.",
            vec![
                ToolArg::new("title", ToolArgType::String, "The card title.")
                    .required(true)
                    .rest(),
                ToolArg::new(
                    "column",
                    ToolArgType::String,
                    "Target column id or name. Defaults to the first column.",
                )
                .long("col"),
                ToolArg::new(
                    "labels",
                    ToolArgType::List,
                    "Labels to tag the card with.",
                )
                .short('l')
                .long("label"),
                ToolArg::new(
                    "parent",
                    ToolArgType::String,
                    "Optional card ref (`headway:<board>/<word-id>` or `<board>/<word-id>`) to parent the new card under (make it a subissue).",
                ),
                ToolArg::new(
                    "description",
                    ToolArgType::String,
                    "Its starting description (the cover note).",
                )
                .long("desc")
                .from_file("desc-file"),
                priority_arg("Its starting priority."),
                estimate_arg("Its starting estimate: a number of points."),
                due_arg("Its starting due date, `YYYY-MM-DD`."),
                board_arg(),
                default_board_arg(),
            ],
        )
        .titled("Add card")
        .command(&["headway", "add"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let board = args.route.board(args.parent.as_deref())?;
        // Read the clearable fields before resolving anything, so a typo in
        // one fails before the board is folded.
        let estimate = args.estimate.as_ref().map(Points::parse).transpose()?;
        let due = args.due.as_deref().map(edit::parse_due).transpose()?;
        mutate(cx, &board, |view| {
            let col = match args.column.as_deref() {
                Some(sel) => edit::resolve_col(view, sel)?,
                None => 0,
            };
            let parent = match args.parent.as_deref() {
                Some(p) => Some(resolve_card(view, p)?),
                None => None,
            };
            Ok(BoardAction::AddCard(NewCard {
                col,
                title: args.title,
                description: args.description.unwrap_or_default(),
                labels: args.labels,
                parent,
                priority: args
                    .priority
                    .as_deref()
                    .map(Priority::parse)
                    .unwrap_or_default(),
                estimate: estimate.flatten(),
                due: due.flatten(),
            }))
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// `headway_move_card`: move a card to another column (appended to its end),
/// or one step on the board (Headway's `H L J K`).
struct MoveCard;

/// Arguments for [`MoveCard`]: exactly one of `column` and `to`.
#[derive(Deserialize)]
struct MoveCardArgs {
    card: String,
    /// Target column id or name.
    #[serde(default)]
    column: Option<String>,
    /// The row in `column` to land on (0 is the top); its end when omitted.
    #[serde(default)]
    row: Option<usize>,
    /// One step from where the card is.
    #[serde(default)]
    to: Option<MoveStep>,
    #[serde(flatten)]
    route: Route,
}

/// A one-step card move, the board edit behind Headway's `H L J K`.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum MoveStep {
    /// To the column on the left, at the same height.
    Left,
    /// To the column on the right, at the same height.
    Right,
    /// Past the card above it in its column.
    Up,
    /// Past the card below it in its column.
    Down,
}

/// The values of [`MoveStep`], as the spec advertises them.
const MOVE_STEPS: &[&str] = &["left", "right", "up", "down"];

impl MoveStep {
    /// Where this step drops `card` on `view`; an error at the edge of the board or of its column.
    ///
    /// The steps the grid's `H L J K` always made ([`cursor::move_across`],
    /// [`cursor::move_within`]), over the whole board: a tool sees no grid
    /// filter, so with one on, `J` hops the next card even if it's hidden.
    fn target(self, view: &BoardView, card: NoteId) -> Result<MoveTarget, String> {
        let everything = IssueFilter::default();
        let filter = ViewFilter {
            view,
            filter: &everything,
        };
        let target = match self {
            Self::Left => cursor::move_across(view, &filter, card, Side::Left),
            Self::Right => cursor::move_across(view, &filter, card, Side::Right),
            Self::Up => cursor::move_within(view, &filter, card, Vertical::Up),
            Self::Down => cursor::move_within(view, &filter, card, Vertical::Down),
        };
        target.ok_or_else(|| {
            let edge = match self {
                Self::Left => "already in the first column",
                Self::Right => "already in the last column",
                Self::Up => "already at the top of its column",
                Self::Down => "already at the bottom of its column",
            };
            format!("the card is {edge}")
        })
    }
}

impl AppTool for MoveCard {
    type Args = MoveCardArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_move_card",
            "Move a headway card: to another column (at `row`, else the end of that column), \
             or one step with `to` (left/right: the neighbouring column at the same height; \
             up/down: past the neighbouring card in its column). Give exactly one of \
             `column` and `to`.",
            vec![
                card_arg(),
                ToolArg::new("column", ToolArgType::String, "Target column id or name.")
                    .long("col"),
                ToolArg::new(
                    "row",
                    ToolArgType::Integer,
                    "The position in `column` to land at, 0 being the top. Defaults to its end.",
                ),
                ToolArg::new(
                    "to",
                    ToolArgType::Enum(MOVE_STEPS.to_vec()),
                    "One step from where the card is, instead of a column.",
                ),
                card_board_arg(),
                default_board_arg(),
            ],
        )
        .titled("Move card")
        .command(&["headway", "move"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let board = args.route.board([args.card.as_str()])?;
        mutate(cx, &board, |view| Self::edit(view, &args))
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

impl MoveCard {
    /// The move `args` asks for on `view`.
    fn edit(view: &BoardView, args: &MoveCardArgs) -> Result<BoardAction, String> {
        let card = resolve_card(view, &args.card)?;
        let MoveTarget { to_col, to_row } = match (&args.column, args.to) {
            (Some(column), None) => {
                let to_col = edit::resolve_col(view, column)?;
                MoveTarget {
                    to_col,
                    to_row: args.row.unwrap_or(view.columns[to_col].cards.len()),
                }
            }
            (None, Some(_)) if args.row.is_some() => {
                return Err("'row' goes with 'column', not 'to'".to_owned());
            }
            (None, Some(step)) => step.target(view, card)?,
            _ => return Err("give exactly one of 'column' and 'to'".to_owned()),
        };
        Ok(BoardAction::MoveCard {
            card,
            to_col,
            to_row,
        })
    }
}

/// `headway_set_title`: replace a card's title.
struct SetTitle;

/// Arguments for [`SetTitle`].
#[derive(Deserialize)]
struct SetTitleArgs {
    card: String,
    title: String,
    #[serde(flatten)]
    route: Route,
}

impl AppTool for SetTitle {
    type Args = SetTitleArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_set_title",
            "Replace a headway card's title.",
            vec![
                card_arg(),
                ToolArg::new("title", ToolArgType::String, "The new title.")
                    .required(true)
                    .rest(),
                card_board_arg(),
                default_board_arg(),
            ],
        )
        .titled("Set card title")
        .command(&["headway", "title"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let board = args.route.board([args.card.as_str()])?;
        mutate(cx, &board, |view| {
            Ok(BoardAction::EditTitle {
                card: resolve_card(view, &args.card)?,
                title: args.title,
            })
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// `headway_set_description`: replace a card's description.
struct SetDescription;

/// Arguments for [`SetDescription`].
#[derive(Deserialize)]
struct SetDescriptionArgs {
    card: String,
    description: String,
    /// Files for a command line to upload first (see [`attach_arg`]).
    #[serde(default)]
    attach: Vec<String>,
    /// Uploaded images to add, as [`attach::Attach`] answers them.
    #[serde(default)]
    attachments: Vec<String>,
    #[serde(flatten)]
    route: Route,
}

impl AppTool for SetDescription {
    type Args = SetDescriptionArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_set_description",
            "Replace a headway card's description (its body / cover note).",
            vec![
                card_arg(),
                ToolArg::new("description", ToolArgType::String, "The new description.")
                    .required(true)
                    .rest()
                    .from_file("desc-file"),
                attach_arg(),
                attachments_arg("Uploaded images, as `headway_attach` answers them: each appended to the description as a markdown image, its key on the event."),
                card_board_arg(),
                default_board_arg(),
            ],
        )
        .titled("Set card description")
        .command(&["headway", "desc"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        if !args.attach.is_empty() {
            return Err(attach::ATTACH_NEEDS_UPLOAD.to_owned());
        }
        let added = attach::parse_attachments(&args.attachments)?;
        let description =
            attach::with_references(args.description, &added, AttachTarget::Description);
        let board = args.route.board([args.card.as_str()])?;
        mutate(cx, &board, |view| {
            let card = resolve_card(view, &args.card)?;
            // The new cover note is the whole description: keep the card's
            // attachments its text still points at, and add the new ones.
            let kept = render::find_card(view, &card)
                .map(|found| found.card.description_attachments.as_slice())
                .unwrap_or_default();
            let attachments = referenced(kept, &description)
                .cloned()
                .chain(added)
                .collect();
            Ok(BoardAction::EditDescription {
                card,
                description,
                attachments,
            })
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// `headway_set_labels`: set a card's labels, replacing its set.
struct SetLabels;

/// Arguments for [`SetLabels`].
#[derive(Deserialize)]
struct SetLabelsArgs {
    card: String,
    /// The card's new label set; none clears it.
    #[serde(default, deserialize_with = "list_or_csv")]
    labels: Vec<String>,
    #[serde(flatten)]
    route: Route,
}

impl AppTool for SetLabels {
    type Args = SetLabelsArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_set_labels",
            "Set a headway card's labels to exactly `labels`, replacing its set: to add one, pass the card's labels with it. Naming none clears them.",
            vec![
                card_arg(),
                ToolArg::new(
                    "labels",
                    ToolArgType::List,
                    "The card's labels. Omit or leave empty to clear them.",
                )
                .rest()
                .short('l')
                .long("label"),
                card_board_arg(),
                default_board_arg(),
            ],
        )
        .titled("Set card labels")
        .command(&["headway", "label"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let board = args.route.board([args.card.as_str()])?;
        mutate(cx, &board, |view| {
            Ok(BoardAction::SetLabels {
                card: resolve_card(view, &args.card)?,
                labels: args.labels,
            })
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// `headway_set_priority`: set a card's priority.
struct SetPriority;

/// Arguments for [`SetPriority`].
#[derive(Deserialize)]
struct SetPriorityArgs {
    card: String,
    /// One of `none`, `low`, `medium`, `high`, `urgent`.
    priority: String,
    #[serde(flatten)]
    route: Route,
}

impl AppTool for SetPriority {
    type Args = SetPriorityArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_set_priority",
            "Set a headway card's priority. `none` clears it.",
            vec![
                card_arg(),
                priority_arg("The priority level.")
                    .required(true)
                    .positional(),
                card_board_arg(),
                default_board_arg(),
            ],
        )
        .titled("Set card priority")
        .command(&["headway", "priority"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let board = args.route.board([args.card.as_str()])?;
        mutate(cx, &board, |view| {
            Ok(BoardAction::SetPriority {
                card: resolve_card(view, &args.card)?,
                priority: Priority::parse(&args.priority),
            })
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// `headway_set_due`: set or clear a card's due date.
struct SetDue;

/// Arguments for [`SetDue`].
#[derive(Deserialize)]
struct SetDueArgs {
    card: String,
    /// `YYYY-MM-DD`, or `none` to clear it.
    due: String,
    #[serde(flatten)]
    route: Route,
}

impl AppTool for SetDue {
    type Args = SetDueArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_set_due",
            "Set a headway card's due date. `none` clears it.",
            vec![
                card_arg(),
                due_arg("The due date, `YYYY-MM-DD`, or `none` to clear it.")
                    .required(true)
                    .positional(),
                card_board_arg(),
                default_board_arg(),
            ],
        )
        .titled("Set card due date")
        .command(&["headway", "due"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let board = args.route.board([args.card.as_str()])?;
        let due = edit::parse_due(&args.due)?;
        mutate(cx, &board, |view| {
            Ok(BoardAction::SetDue {
                card: resolve_card(view, &args.card)?,
                due,
            })
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// `headway_set_estimate`: set or clear a card's estimate.
struct SetEstimate;

/// Arguments for [`SetEstimate`].
#[derive(Deserialize)]
struct SetEstimateArgs {
    card: String,
    /// A number of points, or `none` to clear it.
    estimate: Points,
    #[serde(flatten)]
    route: Route,
}

impl AppTool for SetEstimate {
    type Args = SetEstimateArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_set_estimate",
            "Set a headway card's estimate in points. `none` clears it.",
            vec![
                card_arg(),
                estimate_arg("The estimate: a number of points, or `none` to clear it.")
                    .required(true)
                    .positional(),
                card_board_arg(),
                default_board_arg(),
            ],
        )
        .titled("Set card estimate")
        .command(&["headway", "estimate"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let board = args.route.board([args.card.as_str()])?;
        let estimate = args.estimate.parse()?;
        mutate(cx, &board, |view| {
            Ok(BoardAction::SetEstimate {
                card: resolve_card(view, &args.card)?,
                estimate,
            })
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// `headway_set_parent`: make a card a subissue of another, or detach it.
struct SetParent;

/// Arguments for [`SetParent`].
#[derive(Deserialize)]
struct SetParentArgs {
    card: String,
    /// The parent card ref; omit to detach `card` from its parent.
    #[serde(default)]
    parent: Option<String>,
    #[serde(flatten)]
    route: Route,
}

impl AppTool for SetParent {
    type Args = SetParentArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_set_parent",
            "Make a headway card a subissue of another card, or omit `parent` to detach it. Refused if it would create a parent cycle.",
            vec![
                card_arg(),
                ToolArg::new(
                    "parent",
                    ToolArgType::String,
                    "The parent card ref (`headway:<board>/<word-id>` or `<board>/<word-id>`). Omit to detach the card from its parent.",
                )
                .positional(),
                card_board_arg(),
                default_board_arg(),
            ],
        )
        .titled("Set card parent")
        .command(&["headway", "parent"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let refs = std::iter::once(args.card.as_str()).chain(args.parent.as_deref());
        let board = args.route.board(refs)?;
        mutate(cx, &board, |view| {
            let card = resolve_card(view, &args.card)?;
            let parent = match args.parent.as_deref() {
                Some(p) => Some(resolve_card(view, p)?),
                None => None,
            };
            Ok(BoardAction::SetParent { card, parent })
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// Which edge between two cards an [`EdgeTool`] adds or removes.
#[derive(Clone, Copy, Debug)]
enum Edge {
    /// `card` is blocked by `other`: a dependency.
    Block,
    Unblock,
    /// `card` and `other` are related: an undirected "see also".
    Relate,
    Unrelate,
}

/// `headway_block` / `headway_unblock` / `headway_relate` /
/// `headway_unrelate`: add or remove an edge between two cards. The edge may
/// cross boards; a block that would close a cycle is refused.
struct EdgeTool(Edge);

/// Arguments for an [`EdgeTool`]. The other card is `on` for a block (the
/// blocker) and `other` for a relate; each is that tool's name for it.
#[derive(Deserialize)]
struct EdgeArgs {
    card: String,
    #[serde(alias = "on")]
    other: String,
    #[serde(flatten)]
    route: Route,
}

impl AppTool for EdgeTool {
    type Args = EdgeArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        let (name, description, title, path): (_, _, _, &'static [&'static str]) = match self.0 {
            Edge::Block => (
                "headway_block",
                "Mark a headway card as blocked by another: a dependency edge, independent of the parent axis. A blocked card drops out of `headway_ready_items` until its blocker is done (in a terminal column, or a parent whose live subissues are all done). The edge may cross boards (name the other card by its full hex id); a cycle is refused.",
                "Block card",
                &["headway", "block"],
            ),
            Edge::Unblock => (
                "headway_unblock",
                "Remove a headway card's blocked-by edge.",
                "Unblock card",
                &["headway", "unblock"],
            ),
            Edge::Relate => (
                "headway_relate",
                "Relate two headway cards: an undirected \"see also\" edge, shown on both. It carries no ordering or readiness meaning and never affects what is ready. May cross boards.",
                "Relate cards",
                &["headway", "relate"],
            ),
            Edge::Unrelate => (
                "headway_unrelate",
                "Remove the relates edge between two headway cards. The edge is symmetric, so either card can remove it.",
                "Unrelate cards",
                &["headway", "unrelate"],
            ),
        };
        let other = match self.0 {
            Edge::Block | Edge::Unblock => ToolArg::new(
                "on",
                ToolArgType::String,
                "The blocker's card ref, or its full hex id when it is on another board.",
            )
            .long("on"),
            Edge::Relate | Edge::Unrelate => ToolArg::new(
                "other",
                ToolArgType::String,
                "The other card's ref, or its full hex id when it is on another board.",
            )
            .long("to"),
        };
        ToolSpec::new(
            name,
            description,
            vec![
                card_arg(),
                other.required(true).positional(),
                card_board_arg(),
                default_board_arg(),
            ],
        )
        .titled(title)
        .command(path)
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let board = args
            .route
            .board([args.card.as_str(), args.other.as_str()])?;
        let edge = self.0;
        mutate(cx, &board, |view| {
            let card = resolve_card(view, &args.card)?;
            let other = resolve_card(view, &args.other)?;
            Ok(match edge {
                Edge::Block => BoardAction::Block { card, on: other },
                Edge::Unblock => BoardAction::Unblock { card, on: other },
                Edge::Relate => BoardAction::Relate { card, other },
                Edge::Unrelate => BoardAction::Unrelate { card, other },
            })
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// `headway_seq`: where a card sits in a container's work-order, or, with a
/// position, move it there — `nd headway seq`.
struct Seq;

/// Arguments for [`Seq`]: at most one of `first`, `last`, `after` and
/// `before`; none reads the card's place.
#[derive(Deserialize)]
struct SeqArgs {
    card: String,
    #[serde(default)]
    first: bool,
    #[serde(default)]
    last: bool,
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    before: Option<String>,
    /// The container: a card ref (its subissues) or the board slug.
    #[serde(default)]
    container: Option<String>,
    #[serde(flatten)]
    route: Route,
}

impl SeqArgs {
    /// The move asked for, or `None` to read the card's place. More than one
    /// position is an error.
    fn position(&self) -> Result<Option<SeqSpec<'_>>, String> {
        let given = [
            self.first.then_some(SeqSpec::First),
            self.last.then_some(SeqSpec::Last),
            self.after.as_deref().map(SeqSpec::After),
            self.before.as_deref().map(SeqSpec::Before),
        ];
        let mut given = given.into_iter().flatten();
        let position = given.next();
        if given.next().is_some() {
            return Err("give at most one of 'first', 'last', 'after' and 'before'".to_owned());
        }
        Ok(position)
    }
}

/// A `seq` move, its anchor still a raw card ref.
#[derive(Clone, Copy)]
enum SeqSpec<'a> {
    First,
    Last,
    After(&'a str),
    Before(&'a str),
}

impl SeqSpec<'_> {
    /// The position on `view`, its anchor resolved.
    fn resolve(self, view: &BoardView) -> Result<store::SeqPosition, String> {
        Ok(match self {
            Self::First => store::SeqPosition::First,
            Self::Last => store::SeqPosition::Last,
            Self::After(a) => store::SeqPosition::After(resolve_card(view, a)?),
            Self::Before(b) => store::SeqPosition::Before(resolve_card(view, b)?),
        })
    }
}

/// What [`Seq`] answers: the card's place when asked for none, else the
/// move's ack.
#[derive(Serialize, Debug)]
#[serde(untagged)]
enum SeqAnswer {
    Place(render::SeqPlace),
    Moved(EditAck),
}

impl AppTool for Seq {
    type Args = SeqArgs;
    type Output = SeqAnswer;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_seq",
            "Show or set where a headway card sits in a container's work-order, the order `headway_ready_items` walks. With no position it only reads, answering `{ref, container, index, of, sequenced, before, after, ready_ahead, ready, not_ready_why}`: its place (\"31 of 40\"), the cards just before and after it, whether it has a rank of its own, how many ready cards come ahead of it, and why it isn't ready itself. With one of `first`, `last`, `after` or `before` it moves the card there instead and answers the edit's ack. An `after`/`before` anchor must be sequenced already. A card the container's walk never reaches is an error.",
            vec![
                card_arg(),
                ToolArg::new("first", ToolArgType::Boolean, "Move it to the front of the order."),
                ToolArg::new("last", ToolArgType::Boolean, "Move it to the back of the order."),
                ToolArg::new(
                    "after",
                    ToolArgType::String,
                    "Move it immediately after this card.",
                ),
                ToolArg::new(
                    "before",
                    ToolArgType::String,
                    "Move it immediately before this card.",
                ),
                ToolArg::new(
                    "container",
                    ToolArgType::String,
                    "The container: a card ref (its subissues) or the board slug (the board root). Defaults to the board root.",
                )
                .long("in"),
                card_board_arg(),
                default_board_arg(),
            ],
        )
        .titled("Card work-order")
        .command(&["headway", "seq"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let refs = [
            Some(args.card.as_str()),
            args.after.as_deref(),
            args.before.as_deref(),
            args.container.as_deref(),
        ];
        let board = args.route.board(refs.into_iter().flatten())?;
        let Some(spec) = args.position()? else {
            let view = board_view(cx, &board)?;
            let card = resolve_card(&view, &args.card)?;
            let container = edit::container_or_root(&view, args.container.as_deref())?;
            return render::seq_place(&view, &container, card).map(SeqAnswer::Place);
        };
        mutate(cx, &board, |view| {
            let card = resolve_card(view, &args.card)?;
            let container = edit::container_or_root(view, args.container.as_deref())?;
            let rank = store::seq_rank(view, &container, card, &spec.resolve(view)?)?;
            Ok(BoardAction::SetSequence {
                card,
                container,
                rank,
            })
        })
        .map(SeqAnswer::Moved)
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(match out {
            SeqAnswer::Place(place) => place.text(true),
            SeqAnswer::Moved(ack) => ack.render(),
        })
    }
}

/// `headway_set_terminal`: mark a column terminal (a "done" column), or
/// clear the flag — `nd headway terminal`.
struct SetTerminal;

/// Arguments for [`SetTerminal`].
#[derive(Deserialize)]
struct SetTerminalArgs {
    column: String,
    /// `on` (the default) or `off`.
    #[serde(default)]
    state: Option<String>,
    #[serde(flatten)]
    route: Route,
}

impl SetTerminalArgs {
    /// Whether the column ends up terminal: `state` read as `nd headway
    /// terminal` reads it, `on` when it is left out.
    fn terminal(&self) -> Result<bool, String> {
        match self.state.as_deref() {
            None | Some("on" | "true" | "yes") => Ok(true),
            Some("off" | "false" | "no") => Ok(false),
            Some(other) => Err(format!("terminal state must be on/off, got '{other}'")),
        }
    }
}

impl AppTool for SetTerminal {
    type Args = SetTerminalArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_set_terminal",
            "Mark a headway column terminal (a \"done\" column), or clear the flag with `state: off`. A card in a terminal column clears its dependents and drops out of `headway_ready_items`. Republishes the board definition.",
            vec![
                ToolArg::new("column", ToolArgType::String, "The column id or name.")
                    .required(true)
                    .positional(),
                ToolArg::new(
                    "state",
                    ToolArgType::Enum(vec!["on", "off"]),
                    "`on` to mark it terminal, `off` to clear the flag. Defaults to `on`.",
                )
                .positional(),
                board_arg(),
                default_board_arg(),
            ],
        )
        .titled("Set terminal column")
        .command(&["headway", "terminal"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let board = args.route.board([])?;
        let terminal = args.terminal()?;
        mutate(cx, &board, |view| {
            Ok(BoardAction::SetColumnTerminal {
                col: edit::resolve_col(view, &args.column)?,
                terminal,
            })
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// `headway_comment_card`: post a comment on a card, a reply, or an inline
/// comment on one of its review records.
struct CommentCard;

/// Arguments for [`CommentCard`].
#[derive(Deserialize)]
struct CommentCardArgs {
    card: String,
    body: String,
    /// The comment or review comment this answers.
    #[serde(default)]
    reply_to: Option<String>,
    /// The file in a review record's commit this comments on.
    #[serde(default)]
    path: Option<String>,
    /// The line, or `a-b` range, in `path`.
    #[serde(default)]
    line: Option<String>,
    /// The lines are on the old (deleted) side.
    #[serde(default)]
    old: bool,
    /// The review record whose commit starts with this, rather than the newest.
    #[serde(default)]
    record: Option<String>,
    /// Files for a command line to upload first (see [`attach_arg`]).
    #[serde(default)]
    attach: Vec<String>,
    /// Uploaded images to carry, as [`attach::Attach`] answers them.
    #[serde(default)]
    attachments: Vec<String>,
    #[serde(flatten)]
    route: Route,
}

impl AppTool for CommentCard {
    type Args = CommentCardArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_comment_card",
            "Post a comment on a headway card. `reply_to` threads it under a comment or review comment on the card (a reply to a review comment goes on that comment's record, under the same lines). `path` and `line` leave a new inline comment on a review record's commit (the newest record, or the one `record` picks); `record` alone comments on the whole commit.",
            vec![
                card_arg(),
                ToolArg::new("body", ToolArgType::String, "The comment text.")
                    .required(true)
                    .rest()
                    .from_file("body-file"),
                ToolArg::new(
                    "reply_to",
                    ToolArgType::String,
                    "The comment or review comment on the same card this answers: its id, a hex prefix, or its word-id.",
                ),
                ToolArg::new(
                    "path",
                    ToolArgType::String,
                    "The file in a review record's commit to comment on, as the diff names it. Needs `line`.",
                ),
                ToolArg::new(
                    "line",
                    ToolArgType::String,
                    "The line, or inclusive range `a-b` (1-based), the comment points at. Needs `path`.",
                ),
                ToolArg::new(
                    "old",
                    ToolArgType::Boolean,
                    "The lines are on the old (deleted) side.",
                ),
                ToolArg::new(
                    "record",
                    ToolArgType::String,
                    "The review record whose commit starts with this. Defaults to the newest.",
                ),
                attach_arg(),
                attachments_arg("Uploaded images, as `headway_attach` answers them: each appended to the comment as its URL on a line of its own, its key on the event. Card comments only, not inline review comments."),
                card_board_arg(),
                default_board_arg(),
            ],
        )
        .titled("Post comment")
        .command(&["headway", "comment"])
        .filed_under(crate::TOOL_CATEGORY)
        .accepts_comment_signer()
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let board = args.route.board([args.card.as_str()])?;
        let flags = edit::ReviewCommentFlags {
            path: args.path,
            lines: args.line.as_deref().map(edit::parse_lines).transpose()?,
            old: args.old,
            record: args.record,
        };
        if !args.attach.is_empty() {
            return Err(attach::ATTACH_NEEDS_UPLOAD.to_owned());
        }
        let added = attach::parse_attachments(&args.attachments)?;
        let body = attach::with_references(args.body, &added, AttachTarget::Comment);
        mutate(cx, &board, |view| {
            let mut action = edit::comment_action(
                view,
                resolve_card(view, &args.card)?,
                body,
                args.reply_to.as_deref(),
                &flags,
            )?;
            if added.is_empty() {
                return Ok(action);
            }
            match &mut action {
                BoardAction::AddComment { attachments, .. } => *attachments = added,
                _ => {
                    return Err(
                        "attachments go on card comments, not inline review comments".to_owned(),
                    );
                }
            }
            Ok(action)
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// `headway_archive_card`: take a card off the board, keeping it recoverable.
struct ArchiveCard;

/// Arguments for a card-only mutation (archive / restore / delete).
#[derive(Deserialize)]
struct CardArgs {
    card: String,
    #[serde(flatten)]
    route: Route,
}

impl AppTool for ArchiveCard {
    type Args = CardArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_archive_card",
            "Archive a headway card: take it off the board but keep it recoverable.",
            vec![card_arg(), card_board_arg(), default_board_arg()],
        )
        .titled("Archive card")
        .command(&["headway", "archive"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let board = args.route.board([args.card.as_str()])?;
        mutate(cx, &board, |view| Self::edit(view, &args))
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

impl ArchiveCard {
    /// The archive `args` asks for on `view`.
    fn edit(view: &BoardView, args: &CardArgs) -> Result<BoardAction, String> {
        Ok(BoardAction::ArchiveCard {
            card: resolve_card(view, &args.card)?,
        })
    }
}

/// The board edit the data tool `name` makes on `view` with `args`, built as
/// the tool builds it but neither signed nor published: what the keymap
/// tests check the keys bound to Headway's board edits against (`a`, `D`,
/// `H L J K`).
#[cfg(test)]
pub(crate) fn board_edit(
    view: &BoardView,
    name: &str,
    args: &serde_json::Value,
) -> Result<BoardAction, String> {
    fn parse<T: serde::de::DeserializeOwned>(args: &serde_json::Value) -> Result<T, String> {
        serde_json::from_value(args.clone()).map_err(|e| e.to_string())
    }
    match name {
        "headway_archive_card" => ArchiveCard::edit(view, &parse(args)?),
        "headway_move_card" => MoveCard::edit(view, &parse(args)?),
        _ => Err(format!("'{name}' makes no board edit the keys make")),
    }
}

/// `headway_restore_card`: restore an archived card to its origin column.
struct RestoreCard;

impl AppTool for RestoreCard {
    type Args = CardArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_restore_card",
            "Restore an archived headway card to the column it was archived from.",
            vec![card_arg(), card_board_arg(), default_board_arg()],
        )
        .titled("Restore card")
        .command(&["headway", "restore"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let board = args.route.board([args.card.as_str()])?;
        mutate(cx, &board, |view| {
            Ok(BoardAction::RestoreCard {
                card: resolve_card(view, &args.card)?,
            })
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// `headway_delete_card`: remove a card from the board.
struct DeleteCard;

impl AppTool for DeleteCard {
    type Args = CardArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_delete_card",
            "Remove a headway card from the board (tombstone its placement).",
            vec![card_arg(), card_board_arg(), default_board_arg()],
        )
        .titled("Delete card")
        .command(&["headway", "delete"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let board = args.route.board([args.card.as_str()])?;
        mutate(cx, &board, |view| {
            Ok(BoardAction::DeleteCard {
                card: resolve_card(view, &args.card)?,
            })
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// `headway_rename_board`: change the board's display title (slug unchanged).
struct RenameBoard;

/// Arguments for [`RenameBoard`].
#[derive(Deserialize)]
struct RenameBoardArgs {
    title: String,
    #[serde(flatten)]
    route: Route,
}

impl AppTool for RenameBoard {
    type Args = RenameBoardArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_rename_board",
            "Rename a headway board's display title. Its slug is unchanged, so existing card refs keep working.",
            vec![
                ToolArg::new("title", ToolArgType::String, "The new board title.")
                    .required(true)
                    .rest(),
                board_arg(),
                default_board_arg(),
            ],
        )
        .titled("Rename board")
        .command(&["headway", "rename"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let board = args.route.board([])?;
        mutate(cx, &board, |_view| {
            Ok(BoardAction::RenameBoard {
                title: args.title.clone(),
            })
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// The selected account's signing key, for a write that isn't a board edit.
fn account_secret(cx: &ToolContext) -> Result<[u8; 32], String> {
    cx.accounts
        .selected_filled()
        .map(|f| f.secret_key.secret_bytes())
        .ok_or_else(|| "the selected account is watch-only and cannot sign changes".to_owned())
}

/// A saved view as `list --save-view --json` prints it
/// ([`list::view_json`]), and the line `--save-view` or `--delete-view`
/// says.
struct ViewAnswer {
    view: SavedView,
    line: String,
}

impl Serialize for ViewAnswer {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        list::view_json(&self.view.spec, Some(&self.view.slug)).serialize(serializer)
    }
}

/// `headway_save_view`: save a list's query and layout as a named view —
/// `nd headway list --save-view <name>`. Personal and local, like the app's own
/// view tabs: PNS-wrapped and signed by the account, never a board edit.
struct SaveView;

/// Arguments for [`SaveView`].
#[derive(Deserialize)]
struct SaveViewArgs {
    /// The view's name; saving over an existing name replaces that view.
    save_view: String,
    #[serde(flatten)]
    layout: ListLayoutArgs,
}

impl AppTool for SaveView {
    type Args = SaveViewArgs;
    type Output = ViewAnswer;

    fn spec(&self) -> ToolSpec {
        let mut args = vec![
            ToolArg::new(
                "save_view",
                ToolArgType::String,
                "The view's name. Saving over a view of that name replaces it.",
            )
            .required(true),
        ];
        args.extend(list_layout_args());
        ToolSpec::new(
            "headway_save_view",
            "Save a headway list's query and layout as a named view, as `nd headway list --save-view <name>` does: the same arguments as `headway_list`, saved rather than listed (over `view` when one is applied). The view is this account's own, kept encrypted; answers it as `{name, query, display, grouping, sorting, board, slug, builtin}`.",
            args,
        )
        .titled("Save list view")
        .command(&["headway", "list"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let me = *cx.accounts.selected_account_pubkey();
        let secret = account_secret(cx)?;
        let saved = views::load_views(cx.ndb, &me);
        let spec = args.layout.compose(&saved)?;
        let view = list::save_named_view(cx.ndb, &me, &secret, &saved, &args.save_view, spec)?;
        let line = list::saved_view_line(&view);
        Ok(ViewAnswer { view, line })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(format!("{}\n", out.line))
    }
}

/// `headway_delete_view`: delete a saved list view — `nd headway list
/// --delete-view <name>`.
struct DeleteView;

/// Arguments for [`DeleteView`].
#[derive(Deserialize)]
struct DeleteViewArgs {
    /// The view's name or slug.
    delete_view: String,
}

impl AppTool for DeleteView {
    type Args = DeleteViewArgs;
    type Output = ViewAnswer;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_delete_view",
            "Delete one of this account's saved headway list views, by name or slug, as `nd headway list --delete-view <name>` does. Answers the view deleted, as `headway_save_view` answers one.",
            vec![
                ToolArg::new(
                    "delete_view",
                    ToolArgType::String,
                    "The saved view's name or slug.",
                )
                .required(true),
            ],
        )
        .titled("Delete list view")
        .command(&["headway", "list"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let me = *cx.accounts.selected_account_pubkey();
        let secret = account_secret(cx)?;
        let saved = views::load_views(cx.ndb, &me);
        let view = list::delete_named_view(cx.ndb, &me, &secret, &saved, &args.delete_view)?;
        let line = list::deleted_view_line(&view);
        Ok(ViewAnswer { view, line })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(format!("{}\n", out.line))
    }
}

/// The shared `card` argument: a card ref accepted by [`resolve_card`], the
/// first bare word on a command line.
fn card_arg() -> ToolArg {
    ToolArg::new(
        "card",
        ToolArgType::String,
        "The card ref, as printed by list/show tools: `headway:<board>/<word-id>` \
         (or the scheme-less `<board>/<word-id>`), or a word-id or hex id prefix \
         on the board picked.",
    )
    .required(true)
    .from_current()
    .positional()
}

/// A card tool's optional `board` argument (see [`Route::board`]).
fn card_board_arg() -> ToolArg {
    ToolArg::new(
        "board",
        ToolArgType::String,
        "The board id (slug). Defaults to the board a `headway:<board>/<word-id>` \
         card ref names, which it must agree with, else `default_board`.",
    )
}

/// The `attach` argument of the tools that can carry images: files a command
/// line uploads with [`attach::Attach`] first, passing what it answers as
/// `attachments` (see [`ToolArg::via`]). The tool itself refuses it.
fn attach_arg() -> ToolArg {
    ToolArg::new(
        "attach",
        ToolArgType::Paths,
        "Image files to attach. A command line (`nd … --attach <path>`) uploads them first; any other caller calls `headway_attach` with them and passes its answer as `attachments`.",
    )
    .via("headway_attach", "paths")
}

/// The `attachments` argument: uploaded images, as `headway_attach` answers
/// them, and where they go in the text (`placed`).
fn attachments_arg(placed: &'static str) -> ToolArg {
    ToolArg::new("attachments", ToolArgType::Strings, placed)
}

/// The caller's board, used when nothing else names one (see
/// [`Route::board`]): a command line fills it from `$HEADWAY_BOARD`.
fn default_board_arg() -> ToolArg {
    ToolArg::new(
        "default_board",
        ToolArgType::String,
        "The board to use when neither `board` nor a card ref names one. Defaults to the primary board.",
    )
    .env(BOARD_ENV)
}

/// The shared optional `board` argument, defaulting to the primary board.
fn board_arg() -> ToolArg {
    ToolArg::new(
        "board",
        ToolArgType::String,
        "The board id (slug). Defaults to the primary board.",
    )
}

/// A card's priority, as `add` and `priority` take it.
fn priority_arg(description: &'static str) -> ToolArg {
    ToolArg::new(
        "priority",
        ToolArgType::Enum(vec!["none", "low", "medium", "high", "urgent"]),
        description,
    )
}

/// A card's estimate, as `add` and `estimate` take it: points or `none`.
fn estimate_arg(description: &'static str) -> ToolArg {
    ToolArg::new("estimate", ToolArgType::String, description)
}

/// A card's due date, as `add` and `due` take it: `YYYY-MM-DD` or `none`.
fn due_arg(description: &'static str) -> ToolArg {
    ToolArg::new("due", ToolArgType::String, description)
}

#[cfg(test)]
mod tests {
    mod agent_toolset;
    mod attach;
    mod board;
    mod edits;
    mod live;
    mod review;
    mod search;
    mod structure;

    use super::*;
    use headway::wordid;
    use nostrdb::{Ndb, SubscriptionStream};
    use nostrdb_net::FullKeypair;
    use notedeck::{Accounts, NoteCache, UnknownIds};
    use notedeck_testing::fixtures::test_config;
    use serde_json::json;
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    /// A seeded demo board plus an `Accounts` whose selected pubkey authored it,
    /// so the tools fold the right board. The demo board lands 3 / 2 / 1 / 0 / 1
    /// cards across its five columns (7 total). Owned pieces are returned so they
    /// outlive the borrows a `ToolContext` takes.
    fn seeded_env() -> SeededEnv {
        let dir = TempDir::new().expect("tmp dir");
        let mut ndb = Ndb::new(dir.path().to_str().expect("path"), &test_config()).expect("ndb");
        let kp = FullKeypair::generate();
        let secret = kp.secret_key.secret_bytes();

        // Ingest is async, so subscribe *before* seeding: every event the seed
        // writes from here on wakes the stream.
        let sub = ndb
            .subscribe(&[event::headway_filter(&kp.pubkey)])
            .expect("subscribe");
        let mut stream = SubscriptionStream::new(ndb.clone(), sub);
        let seeded = store::seed_demo_board(
            &ndb,
            &kp.pubkey,
            &secret,
            store::BOARD_ID,
            1_700_000_000,
            &mut store::NoPublish,
        );

        // Wait on the seed's own event count rather than on the board's shape.
        // The seed lands in phases — board/columns/cards, then the dependency
        // edges, then the post-creation history (subject edits, labels, a
        // placement, priorities) — so a "7 cards are present" predicate is
        // satisfied at the end of phase one, with the blockers and priorities
        // these tests assert on still in flight. The count is reported by the
        // seeder itself, so it can't drift as seed events are added or removed,
        // and the wait is bounded: a count that can never be reached fails the
        // test with the running total instead of hanging.
        notedeck_testing::await_notes(&mut stream, seeded);

        let txn = Transaction::new(&ndb).expect("txn");
        let mut unknown_ids = UnknownIds::default();
        // `fallback` is the selected account when no keystore is given, so the
        // tools fold this keypair's board.
        let accounts = Accounts::new(
            None,
            Vec::new(),
            Vec::new(),
            kp.pubkey,
            &mut ndb,
            &txn,
            &mut unknown_ids,
        );
        drop(txn);
        SeededEnv {
            dir,
            ndb,
            accounts,
            note_cache: NoteCache::default(),
        }
    }

    /// What [`seeded_env`] hands back. Bind `dir` for the whole test: the
    /// database lives in it.
    struct SeededEnv {
        dir: TempDir,
        ndb: Ndb,
        accounts: Accounts,
        note_cache: NoteCache,
    }

    fn tool_context<'a>(
        ndb: &'a Ndb,
        note_cache: &'a mut NoteCache,
        accounts: &'a Accounts,
    ) -> ToolContext<'a> {
        ToolContext {
            ndb,
            note_cache,
            accounts,
            remote: None,
            comment_signer: None,
            path: None,
            written: Vec::new(),
        }
    }

    /// A shared board resolves *with* a sealing channel — the anti-leak contract:
    /// an AI-tool edit to a shared board seals into its SNS channel rather than
    /// publishing a plaintext rumor. Covers both the joined path (a board this
    /// account doesn't own) and the owner's own shared board (its coordinate is in
    /// the roster, so it seals too instead of folding author-scoped).
    #[test]
    fn resolve_target_seals_a_shared_board() {
        let dir = TempDir::new().expect("tmp dir");
        let ndb = Ndb::new(dir.path().to_str().expect("path"), &test_config()).expect("ndb");
        let owner = FullKeypair::generate();
        let member = FullKeypair::generate();

        // The owner shares a board over an SNS channel. A shared board's definition
        // travels sealed (the member's coordinate fold gathers only team-sealed
        // rumors), and the owner's own author-scoped fold sees the same unwrapped
        // rumor — so one sealed definition serves both resolution paths.
        let mut root = [0u8; 32];
        root[0] = 0x11;
        root[31] = 0x22;
        assert!(ndb.add_team_root(&root));
        let channel = store::SnsChannel {
            keys: nostrdb_net::sns::derive_sns_keys(&root).expect("keys"),
        };
        let board_addr = event::board_address(&owner.pubkey, store::BOARD_ID);
        let cols = vec![
            event::ColumnDef::new("backlog", "Backlog"),
            event::ColumnDef::new("todo", "Todo"),
            event::ColumnDef::new("done", "Done"),
        ];
        store::ingest_signed(
            &ndb,
            event::build_board(store::BOARD_ID, "Headway", "", &cols),
            &store::Signer::shared(&owner.secret_key.secret_bytes(), &channel),
            &mut store::NoPublish,
        );

        // Wait for the sealed definition to unwrap and fold on both paths.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let txn = Transaction::new(&ndb).expect("txn");
            let ready = event::load_shared_board(
                &ndb,
                &txn,
                &board_addr,
                std::slice::from_ref(&channel.keys.team_keypair.pubkey),
            )
            .is_some()
                && event::load_board(&ndb, &txn, &owner.pubkey, store::BOARD_ID).is_some();
            drop(txn);
            if ready {
                break;
            }
            assert!(Instant::now() < deadline, "sealed board never materialised");
            std::thread::sleep(Duration::from_millis(20));
        }

        let team = teams::Team {
            team_root: hex::encode(root),
            board_addr,
            epoch: None,
            shared_at: 0,
        };
        let teams = [team];

        // The member doesn't own the board, so it folds by coordinate and seals.
        let ResolvedTarget { view, channel } =
            resolve_target(&ndb, &member.pubkey, store::BOARD_ID, &teams).expect("resolves");
        assert_eq!(view.id, store::BOARD_ID);
        assert!(
            channel.is_some(),
            "a joined shared board must resolve a sealing channel, never plaintext"
        );

        // The owner editing their OWN shared board also seals (breakage #1 + the
        // plaintext leak): its coordinate is in the roster, so it folds by
        // coordinate — surfacing co-members' cards — and seals every edit, rather
        // than folding author-scoped and publishing a plaintext rumor.
        let ResolvedTarget {
            channel: own_channel,
            ..
        } = resolve_target(&ndb, &owner.pubkey, store::BOARD_ID, &teams).expect("resolves");
        assert!(
            own_channel.is_some(),
            "an owner's own shared board must seal too, not leak plaintext"
        );
    }

    /// A board that is neither owned nor a joined shared board is a clean error,
    /// not a plaintext edit.
    #[test]
    fn resolve_target_unknown_board_errors() {
        let SeededEnv {
            dir: _dir,
            ndb,
            accounts,
            ..
        } = seeded_env();
        let author = *accounts.selected_account_pubkey();
        // `SnsChannel` isn't `Debug`, so match the error rather than `unwrap_err`.
        let Err(err) = resolve_target(&ndb, &author, "nope", &[]) else {
            panic!("an unknown board must not resolve");
        };
        assert!(err.contains("no board 'nope'"), "{err}");
    }

    #[test]
    fn list_boards_reports_the_seeded_board() {
        let SeededEnv {
            dir: _dir,
            ndb,
            accounts,
            mut note_cache,
        } = seeded_env();
        let mut cx = tool_context(&ndb, &mut note_cache, &accounts);

        let out = call_registered(&mut cx, "headway_list_boards", json!({})).unwrap();
        assert_eq!(
            out,
            json!([{ "id": store::BOARD_ID, "title": "Headway", "card_count": 7 }])
        );
    }

    #[test]
    fn show_board_returns_five_columns() {
        let SeededEnv {
            dir: _dir,
            ndb,
            accounts,
            mut note_cache,
        } = seeded_env();
        let mut cx = tool_context(&ndb, &mut note_cache, &accounts);

        let out = ShowBoard
            .call(&mut cx, serde_json::from_value(json!({})).unwrap())
            .unwrap();
        let out = serde_json::to_value(&out).unwrap();
        assert_eq!(out["id"], store::BOARD_ID);
        assert_eq!(out["columns"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn show_board_errors_on_unknown_board() {
        let SeededEnv {
            dir: _dir,
            ndb,
            accounts,
            mut note_cache,
        } = seeded_env();
        let mut cx = tool_context(&ndb, &mut note_cache, &accounts);

        let err = ShowBoard
            .call(
                &mut cx,
                serde_json::from_value(json!({ "board": "nope" })).unwrap(),
            )
            .unwrap_err();
        assert!(err.contains("no board 'nope'"));
    }

    #[test]
    fn show_card_resolves_by_word_id() {
        let SeededEnv {
            dir: _dir,
            ndb,
            accounts,
            mut note_cache,
        } = seeded_env();

        // Pull a real card's word-id out of the folded board.
        let words = {
            let txn = Transaction::new(&ndb).expect("txn");
            let author = *accounts.selected_account_pubkey();
            let view = event::load_board(&ndb, &txn, &author, store::BOARD_ID).expect("board");
            let card = view
                .columns
                .iter()
                .flat_map(|c| &c.cards)
                .next()
                .expect("card");
            wordid::encode(card.id.bytes())
        };

        // Address the card by a full `headway:<board>/<word-id>` reference — a
        // bare word-id is no longer accepted as a card ref.
        let card_ref = format!("{}:{}/{}", wordid::SCHEME, store::BOARD_ID, words);
        let mut cx = tool_context(&ndb, &mut note_cache, &accounts);
        let out = ShowCard
            .call(
                &mut cx,
                ShowCardArgs {
                    card: card_ref.clone(),
                    source: None,
                    route: Route::default(),
                },
            )
            .unwrap();
        let json = serde_json::to_value(&out).unwrap();
        // `nd headway show <card> --json`'s shape: one card, with its column.
        assert_eq!(json.as_array().map(Vec::len), Some(1), "{json}");
        assert_eq!(json[0]["ref"], json!(card_ref));
        assert!(json[0]["title"].is_string());
        assert!(json[0]["column"].is_string());

        let text = ShowCard.render(&out).expect("a card renders");
        assert!(
            plain(&text).starts_with(&format!("card    {card_ref}\n")),
            "{text}"
        );
    }

    /// `s` without its ANSI color, as nd prints it into a pipe.
    fn plain(s: &str) -> String {
        s.replace("\x1b[90m", "").replace("\x1b[0m", "")
    }

    /// The tools' text is the CLI's: `nd headway show` and `nd headway show`
    /// render one board with one function, so the tool's text, its color
    /// stripped, is byte for byte what the CLI prints into a pipe, and its
    /// value is what `nd headway show --json` prints.
    #[test]
    fn show_board_renders_what_the_cli_prints() {
        let SeededEnv {
            dir: _dir,
            ndb,
            accounts,
            mut note_cache,
        } = seeded_env();
        let mut cx = tool_context(&ndb, &mut note_cache, &accounts);
        let out = ShowBoard
            .call(&mut cx, serde_json::from_value(json!({})).unwrap())
            .unwrap();

        let txn = Transaction::new(&ndb).expect("txn");
        let author = *accounts.selected_account_pubkey();
        let view = event::load_board(&ndb, &txn, &author, store::BOARD_ID).expect("board");
        let text = ShowBoard.render(&out).expect("a board renders");
        assert!(text.contains("\x1b[90m"), "refs go out colored: {text:?}");
        assert_eq!(plain(&text), render::board(&view, false, false));
        assert_eq!(
            serde_json::to_value(&out).unwrap(),
            event::board_json(&view)
        );
    }

    #[test]
    fn show_card_errors_on_unknown_card() {
        let SeededEnv {
            dir: _dir,
            ndb,
            accounts,
            mut note_cache,
        } = seeded_env();
        let mut cx = tool_context(&ndb, &mut note_cache, &accounts);

        let err = ShowCard
            .call(
                &mut cx,
                ShowCardArgs {
                    card: "no-such-card".to_string(),
                    source: None,
                    route: Route::default(),
                },
            )
            .unwrap_err();
        assert!(err.contains("no card matching"));
    }

    /// The full `headway:<board>/<word-id>` reference for the board's first card —
    /// what a tool's `card` arg accepts (a bare word-id is no longer a ref).
    fn first_card_ref(ndb: &Ndb, accounts: &Accounts) -> String {
        let txn = Transaction::new(ndb).expect("txn");
        let author = *accounts.selected_account_pubkey();
        let view = event::load_board(ndb, &txn, &author, store::BOARD_ID).expect("board");
        let card = view
            .columns
            .iter()
            .flat_map(|c| &c.cards)
            .next()
            .expect("card");
        wordid::card_ref(store::BOARD_ID, card.id.bytes())
    }

    #[test]
    fn ready_items_excludes_blocked_cards() {
        // The demo board wires a dependency chain: "Sync cards across relays" is
        // blocked by the still-open "Nostr event model", and "Card detail /
        // comments view" is blocked by sync. Both must be held out of the ready
        // frontier so an agent fanning out over it respects the prerequisites.
        let SeededEnv {
            dir: _dir,
            ndb,
            accounts,
            note_cache: mut nc,
        } = seeded_env();
        let mut cx = tool_context(&ndb, &mut nc, &accounts);

        let out = ReadyItems
            .call(
                &mut cx,
                serde_json::from_value(json!({ "ready": true })).unwrap(),
            )
            .unwrap();

        let titles: Vec<&str> = out.iter().map(|i| i.title.as_str()).collect();
        assert!(!out.is_empty(), "the seeded board has workable cards");
        assert!(
            !titles.contains(&"Sync cards across relays"),
            "a blocked card must not be ready: {titles:?}"
        );
        assert!(
            !titles.contains(&"Card detail / comments view"),
            "a blocked card must not be ready: {titles:?}"
        );
        // Every item carries a full `headway:<board>/…` ref and a 64-char hex id.
        assert!(
            out.iter()
                .all(|i| i.card_ref.starts_with("headway:") && i.id.len() == 64),
            "{out:?}"
        );
    }

    #[test]
    fn ready_items_limit_caps_the_frontier() {
        let SeededEnv {
            dir: _dir,
            ndb,
            accounts,
            note_cache: mut nc,
        } = seeded_env();
        let mut cx = tool_context(&ndb, &mut nc, &accounts);

        let full = ReadyItems
            .call(
                &mut cx,
                serde_json::from_value(json!({ "ready": true })).unwrap(),
            )
            .unwrap();
        assert!(full.len() > 1, "need a multi-card frontier to cap");

        let capped = ReadyItems
            .call(
                &mut cx,
                serde_json::from_value(json!({ "limit": 1 })).unwrap(),
            )
            .unwrap();
        assert_eq!(capped.len(), 1);
        // `limit` caps the head of the same work-order, so it's the first item.
        assert_eq!(capped[0].id, full[0].id);

        // `nd headway next`'s lines: the plain ref, then the title.
        let text = ReadyItems.render(&capped).expect("next renders");
        assert_eq!(
            plain(&text),
            format!("{}  {}\n", capped[0].card_ref, capped[0].title)
        );
        assert_eq!(ReadyItems.render(&Vec::new()).as_deref(), Some(""));
    }

    #[test]
    fn ready_items_errors_on_unknown_board() {
        let SeededEnv {
            dir: _dir,
            ndb,
            accounts,
            note_cache: mut nc,
        } = seeded_env();
        let mut cx = tool_context(&ndb, &mut nc, &accounts);

        let err = ReadyItems
            .call(
                &mut cx,
                serde_json::from_value(json!({ "ready": true, "board": "nope" })).unwrap(),
            )
            .unwrap_err();
        assert!(err.contains("no board 'nope'"), "{err}");
    }

    #[test]
    fn ready_items_errors_on_unknown_container() {
        let SeededEnv {
            dir: _dir,
            ndb,
            accounts,
            note_cache: mut nc,
        } = seeded_env();
        let mut cx = tool_context(&ndb, &mut nc, &accounts);

        let err = ReadyItems
            .call(
                &mut cx,
                serde_json::from_value(json!({ "container": "no-such-card", "ready": true }))
                    .unwrap(),
            )
            .unwrap_err();
        assert!(err.contains("no card matching"), "{err}");
    }

    #[test]
    fn resolve_col_by_id_and_name() {
        let SeededEnv {
            dir: _dir,
            ndb,
            accounts,
            ..
        } = seeded_env();
        let txn = Transaction::new(&ndb).expect("txn");
        let author = *accounts.selected_account_pubkey();
        let view = event::load_board(&ndb, &txn, &author, store::BOARD_ID).expect("board");

        // The default board's first column is Backlog.
        assert_eq!(edit::resolve_col(&view, "backlog").unwrap(), 0);
        assert_eq!(edit::resolve_col(&view, "BACKLOG").unwrap(), 0);
        assert_eq!(edit::resolve_col(&view, &view.columns[0].id).unwrap(), 0);
        assert!(edit::resolve_col(&view, "nope").is_err());
    }

    /// Labels arrive as an array (a command line's `-l a -l b,c`) or, from a
    /// model or an older binding, one comma string; both trim and drop
    /// blanks, and none at all is an empty set, which clears.
    #[test]
    fn labels_take_a_list_or_a_comma_string() {
        let labels = |args: serde_json::Value| -> Vec<String> {
            let mut args = args;
            args["card"] = json!("c");
            serde_json::from_value::<SetLabelsArgs>(args)
                .expect("parses")
                .labels
        };
        assert_eq!(labels(json!({"labels": "a, b ,,c "})), ["a", "b", "c"]);
        assert_eq!(
            labels(json!({"labels": ["a", "b,c", " "]})),
            ["a", "b", "c"]
        );
        assert!(labels(json!({"labels": "  ,  "})).is_empty());
        assert!(labels(json!({})).is_empty());
    }

    #[test]
    fn move_card_unknown_column_errors() {
        // The card resolves, so the error is specifically the column miss.
        let SeededEnv {
            dir: _dir,
            ndb,
            accounts,
            note_cache: mut nc,
        } = seeded_env();
        let card = first_card_ref(&ndb, &accounts);
        let mut cx = tool_context(&ndb, &mut nc, &accounts);

        let err = MoveCard
            .call(
                &mut cx,
                MoveCardArgs {
                    to: None,
                    card,
                    column: Some("nope".to_string()),
                    row: None,
                    route: Route::default(),
                },
            )
            .unwrap_err();
        assert!(err.contains("no column matching"), "{err}");
    }

    /// A card tool's board, by the CLI's rule: `board` and a card ref's
    /// board must agree and either names it, then `default_board` (the
    /// caller's `$HEADWAY_BOARD`), then the primary board. A bare word-id or
    /// hex prefix names no board.
    #[test]
    fn a_card_ref_routes_to_its_board() {
        let route = |board: Option<&str>, default_board: Option<&str>| Route {
            board: board.map(str::to_owned),
            default_board: default_board.map(str::to_owned),
        };
        let routed = |board, default_board, refs: &[&str]| {
            route(board, default_board).board(refs.iter().copied())
        };
        let card = "headway:notedeck/maple-river-canyon";
        assert_eq!(routed(None, None, &[card]).unwrap(), "notedeck");
        assert_eq!(
            routed(None, None, &["Notedeck/maple-river-canyon"]).unwrap(),
            "notedeck"
        );
        // A ref beats the caller's default; an agreeing `board` is fine.
        assert_eq!(routed(None, Some("env"), &[card]).unwrap(), "notedeck");
        assert_eq!(routed(Some("notedeck"), None, &[card]).unwrap(), "notedeck");
        let err = routed(Some("other"), None, &[card]).unwrap_err();
        assert!(
            err.contains("conflicts with card ref board 'notedeck'"),
            "{err}"
        );
        let err = routed(None, None, &[card, "headway:other/maple-river-canyon"]).unwrap_err();
        assert!(err.contains("different boards"), "{err}");
        // Nothing routes: the default, then the primary.
        assert_eq!(
            routed(None, Some("env"), &["maple-river-canyon"]).unwrap(),
            "env"
        );
        assert_eq!(routed(Some("b"), Some("env"), &["0123abcd"]).unwrap(), "b");
        assert_eq!(routed(None, None, &["0123abcd"]).unwrap(), store::BOARD_ID);
        assert_eq!(routed(None, None, &[]).unwrap(), store::BOARD_ID);
    }

    /// `to` steps a card as the grid's `H L J K` did, refusing at an edge,
    /// and takes the place of `column`, never alongside it.
    #[test]
    fn move_card_steps() {
        use crate::cursor::tests::{grid, id};
        let view = grid();
        let card = |n| wordid::card_ref(&view.id, id(n).bytes());
        let edit = |n, column: Option<&str>, to: Option<MoveStep>| {
            let args = MoveCardArgs {
                card: card(n),
                column: column.map(str::to_owned),
                row: None,
                to,
                route: Route::default(),
            };
            match MoveCard::edit(&view, &args) {
                Ok(BoardAction::MoveCard {
                    card,
                    to_col,
                    to_row,
                }) => Ok((card, to_col, to_row)),
                Ok(_) => panic!("a move"),
                Err(err) => Err(err),
            }
        };
        // `a` holds 1,2,3; `b` 4; `c` 5,6.
        assert_eq!(edit(2, None, Some(MoveStep::Right)), Ok((id(2), 1, 1)));
        assert_eq!(edit(2, None, Some(MoveStep::Down)), Ok((id(2), 0, 3)));
        assert_eq!(edit(2, None, Some(MoveStep::Up)), Ok((id(2), 0, 0)));
        assert_eq!(edit(5, None, Some(MoveStep::Left)), Ok((id(5), 1, 0)));
        assert_eq!(edit(2, Some("c"), None), Ok((id(2), 2, 2)));
        for (n, step) in [
            (1, MoveStep::Left),
            (5, MoveStep::Right),
            (1, MoveStep::Up),
            (3, MoveStep::Down),
        ] {
            let err = edit(n, None, Some(step)).unwrap_err();
            assert!(err.starts_with("the card is already"), "{step:?}: {err}");
        }
        assert!(edit(2, Some("c"), Some(MoveStep::Up)).is_err(), "both");
        assert!(edit(2, None, None).is_err(), "neither");
    }

    #[test]
    fn set_priority_unknown_card_errors() {
        let SeededEnv {
            dir: _dir,
            ndb,
            accounts,
            note_cache: mut nc,
        } = seeded_env();
        let mut cx = tool_context(&ndb, &mut nc, &accounts);

        let err = SetPriority
            .call(
                &mut cx,
                SetPriorityArgs {
                    card: "no-such".to_string(),
                    priority: "high".to_string(),
                    route: Route::default(),
                },
            )
            .unwrap_err();
        assert!(err.contains("no card matching"), "{err}");
    }

    #[test]
    fn write_tool_rejects_watch_only_account() {
        // The seeded account is pubkey-only. A valid mutation folds and resolves
        // but must refuse to sign rather than silently drop the change.
        let SeededEnv {
            dir: _dir,
            ndb,
            accounts,
            note_cache: mut nc,
        } = seeded_env();
        let card = first_card_ref(&ndb, &accounts);
        let mut cx = tool_context(&ndb, &mut nc, &accounts);

        let err = SetPriority
            .call(
                &mut cx,
                SetPriorityArgs {
                    card,
                    priority: "high".to_string(),
                    route: Route::default(),
                },
            )
            .unwrap_err();
        assert!(err.contains("watch-only"), "{err}");
    }

    /// The view `author`'s board `board_id` resolves to, as a write tool
    /// resolves it (sealed boards by their channel), once `ready` holds of
    /// it. Panics after a bounded wait: ingest and unwrapping are async.
    fn settled_view(
        ndb: &Ndb,
        author: &Pubkey,
        board_id: &str,
        what: &str,
        ready: impl Fn(&BoardView) -> bool,
    ) -> BoardView {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let teams = teams::teams_from_ndb(ndb, author);
            if let Ok(ResolvedTarget { view, .. }) = resolve_target(ndb, author, board_id, &teams)
                && ready(&view)
            {
                return view;
            }
            assert!(Instant::now() < deadline, "never settled: {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Run the registered tool `name` (so the registry's comment-signer
    /// gate applies) and hand back its data or its error.
    fn call_registered(
        cx: &mut ToolContext,
        name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let tool = tools()
            .into_iter()
            .find(|tool| tool.name() == name)
            .unwrap_or_else(|| panic!("{name} is registered"));
        match tool.call(cx, &args) {
            notedeck::ToolOutcome::Data(value) => Ok(value),
            notedeck::ToolOutcome::Error(err) => Err(err),
            notedeck::ToolOutcome::Intent(_) => panic!("{name} answered with an intent"),
        }
    }

    /// The caller's comment key signs a comment on a sealed board, as the
    /// CLI's `HEADWAY_COMMENT_NSEC` does; on a plaintext board, where a
    /// comment by another key would never show, the call is refused and a
    /// comment without one is the account's own.
    #[tokio::test]
    async fn a_comment_signer_signs_on_sealed_boards_and_is_refused_on_plaintext() {
        const SEALED: &str = "sealed";
        let dir = TempDir::new().expect("tmp dir");
        let account = FullKeypair::generate();
        let agent = FullKeypair::generate();
        let secret = account.secret_key.secret_bytes();
        let args: Vec<String> = [
            "notedeck-test",
            "--testrunner",
            "--nsec",
            &account.secret_key.to_secret_hex(),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let mut notedeck = notedeck::Notedeck::init(&egui::Context::default(), dir.path(), &args);
        let mut app = notedeck.app_context();
        let ndb = app.ndb.clone();

        // A plaintext board with cards, and a sealed board born the way
        // `nd headway seed` makes one.
        store::seed_demo_board(
            &ndb,
            &account.pubkey,
            &secret,
            store::BOARD_ID,
            1_700_000_000,
            &mut store::NoPublish,
        );
        let root = nostrdb_net::sns::derive_board_root(&secret, SEALED);
        assert!(store::create_shared_board(
            &ndb,
            &account.pubkey,
            &secret,
            SEALED,
            "Sealed",
            &root,
            &mut store::NoPublish,
        ));
        settled_view(&ndb, &account.pubkey, SEALED, "the sealed board", |_| true);
        {
            let mut cx = app.tool_context();
            call_registered(
                &mut cx,
                "headway_add_card",
                json!({ "title": "Sealed card", "board": SEALED }),
            )
            .expect("adds a card");
        }
        let view = settled_view(&ndb, &account.pubkey, SEALED, "the sealed card", |view| {
            view.columns.iter().any(|col| !col.cards.is_empty())
        });
        let sealed_card = view.columns.iter().flat_map(|c| &c.cards).next().unwrap();
        let sealed_ref = wordid::card_ref(SEALED, sealed_card.id.bytes());
        settled_view(
            &ndb,
            &account.pubkey,
            store::BOARD_ID,
            "the demo board",
            |view| view.columns.iter().any(|col| !col.cards.is_empty()),
        );
        let plain_ref = first_card_ref(&ndb, app.accounts);

        let mut cx = app.tool_context();
        cx.comment_signer = Some(&agent);
        call_registered(
            &mut cx,
            "headway_comment_card",
            json!({ "card": sealed_ref, "body": "from the agent" }),
        )
        .expect("a sealed board takes the agent's comment");
        let err = call_registered(
            &mut cx,
            "headway_comment_card",
            json!({ "card": plain_ref, "body": "never shown" }),
        )
        .unwrap_err();
        assert!(err.contains("plaintext board"), "{err}");

        let view = settled_view(
            &ndb,
            &account.pubkey,
            SEALED,
            "the agent's comment",
            |view| {
                view.columns
                    .iter()
                    .flat_map(|c| &c.cards)
                    .any(|card| !card.comments.is_empty())
            },
        );
        let comment = view
            .columns
            .iter()
            .flat_map(|c| &c.cards)
            .flat_map(|card| &card.comments)
            .next()
            .unwrap();
        assert_eq!(comment.body, "from the agent");
        assert_eq!(
            comment.author,
            *agent.pubkey.bytes(),
            "signed by the comment key"
        );

        // `show` reads the sealed board by its coordinate, so the agent's
        // comment is there to read back, not only the account's own events.
        let mut cx = app.tool_context();
        let shown = call_registered(&mut cx, "headway_show_card", json!({ "card": sealed_ref }))
            .expect("shows the sealed card");
        assert_eq!(shown[0]["comments"][0]["body"], "from the agent");
        assert_eq!(shown[0]["comments"][0]["author"], agent.pubkey.hex());

        // So do the reads across every board: each sealed board is folded
        // by its coordinate there too, not only by the account's own events.
        let found = call_registered(
            &mut cx,
            "headway_grep",
            json!({ "pattern": "from the agent" }),
        )
        .expect("greps every board");
        assert_eq!(found[0]["ref"], sealed_ref.as_str(), "{found}");
        let all = call_registered(&mut cx, "headway_show_board", json!({ "all": true }))
            .expect("shows every board");
        let sealed = all
            .as_array()
            .unwrap()
            .iter()
            .find(|board| board["id"] == SEALED)
            .expect("the sealed board");
        assert_eq!(
            sealed["columns"][0]["cards"][0]["comments"][0]["author"],
            agent.pubkey.hex()
        );
    }
}
