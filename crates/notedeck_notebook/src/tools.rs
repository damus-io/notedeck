//! Data tools exposing the notebook vault — canvases, their nodes and edges,
//! and longform notes — over the shared per-app tool interface
//! ([`notedeck::AppTool`]), one per `notebook` CLI command, so `nd notebook …`
//! is that CLI.
//!
//! Each tool answers from nostrdb alone, never from state the app fills in
//! [`update`](notedeck::App::update): the shell lists a stopped app's tools and
//! runs a call before the app it starts has updated once. Reads fold with the
//! notebook core crate ([`event`]), resolve refs with [`resolve`] and render
//! with [`render`], the same code the CLI uses. Writes go through
//! [`store`]'s sealed path, as the app's own edits do: each event is sealed
//! into the account's team-of-one SNS workspace and ingested locally, and the
//! notedeck host fans the kind-1081 envelope out once the app hands it the
//! workspace root.

use nostrdb::Transaction;
use nostrdb_net::{NoteId, Pubkey};
use notebook::event::{
    self, CanvasView, EdgeEnds, Geometry, LongformNote, NodeContent, NodeKind, VaultDoc,
};
use notebook::render;
use notebook::resolve::{self, Shown};
use notebook::store::{self, CanvasAction};
use notebook::wordid;
use notedeck::{AppTool, RegisteredTool, ToolArg, ToolArgType, ToolContext, ToolSpec};
use serde::{Deserialize, Serialize, Serializer};

/// What the notebook's tools are filed under in a command bar.
const TOOL_CATEGORY: &str = "Notebook";

/// Default size of a node `add` creates, in canvas pixels (the app's
/// `NEW_NODE_SIZE`).
const NEW_W: u64 = 250;
const NEW_H: u64 = 120;

/// The canvas sides an edge can anchor to (JSON Canvas's).
const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];

/// Every data tool the Notebook app contributes, ready to register.
pub fn tools() -> Vec<RegisteredTool> {
    vec![
        // Read.
        RegisteredTool::new(Show),
        RegisteredTool::new(Vault),
        // Write.
        RegisteredTool::new(Seed),
        RegisteredTool::new(AddNode),
        RegisteredTool::new(MoveNode),
        RegisteredTool::new(EditNode),
        RegisteredTool::new(ColorNode),
        RegisteredTool::new(RestackNode),
        RegisteredTool::new(DeleteNode),
        RegisteredTool::new(Connect),
        RegisteredTool::new(Disconnect),
        RegisteredTool::new(RenameCanvas),
    ]
}

// ---------------------------------------------------------------------------
// the account
// ---------------------------------------------------------------------------

/// The selected account as the vault sees it: whose documents to fold, and the
/// secret that signs edits, `None` for a watch-only account.
struct Account {
    author: Pubkey,
    secret: Option<[u8; 32]>,
}

/// The selected account, with its vault's workspace root registered with
/// nostrdb so the sealed documents unwrap.
///
/// The app hands the root to the host every update, but a call can run before
/// the app has updated once, and an edit's envelope only unwraps (so reads
/// back) once its root is registered. Registering is idempotent and cheap once
/// done. A watch-only account has no secret to derive the root from, so it
/// reads whatever is unwrapped already.
fn account(cx: &ToolContext) -> Account {
    let author = *cx.accounts.selected_account_pubkey();
    let secret = cx
        .accounts
        .selected_filled()
        .map(|f| f.secret_key.secret_bytes());
    if let Some(secret) = &secret {
        store::register_workspace(cx.ndb, secret);
    }
    Account { author, secret }
}

/// The selected account's signing secret, or an error a person can act on.
/// A read-only context (no publisher) refuses too, so a write is never made
/// where the caller asked for none.
fn signer(cx: &ToolContext, account: &Account) -> Result<[u8; 32], String> {
    if cx.publish.is_none() {
        return Err("publishing is unavailable in this context".to_owned());
    }
    account
        .secret
        .ok_or_else(|| "the selected account is watch-only and cannot sign changes".to_owned())
}

/// A read transaction, its error worded for the caller.
fn open_txn(cx: &ToolContext) -> Result<Transaction, String> {
    Transaction::new(cx.ndb).map_err(|e| format!("opening a db transaction: {e}"))
}

/// The canvas argument every edit takes.
fn canvas_arg() -> ToolArg {
    ToolArg::new(
        "canvas",
        ToolArgType::String,
        "Which canvas to edit: a `notebook:<word-id>` ref, a `nostr:naddr`, or its `d` (a unique prefix will do). Inferred when there is only one canvas.",
    )
}

// ---------------------------------------------------------------------------
// reads
// ---------------------------------------------------------------------------

/// `notebook_show`: the whole vault, or what each ref names.
struct Show;

/// Arguments for [`Show`].
#[derive(Deserialize)]
struct ShowArgs {
    /// Refs to show; the vault listing when empty.
    #[serde(default)]
    refs: Vec<String>,
}

/// [`Show`]'s answer.
#[derive(Debug)]
enum Shows {
    /// Every vault document, newest-edited first.
    Vault(Vec<VaultDoc>),
    /// What each ref named, in order.
    Items(Vec<Shown>),
}

impl Serialize for Shows {
    /// The vault as an array of rows ([`event::vault_doc_json`]); one item as
    /// its lone object, several as an array, as `notebook show --json` prints.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Vault(docs) => serializer.collect_seq(docs.iter().map(event::vault_doc_json)),
            Self::Items(items) => match items.as_slice() {
                [one] => one.json().serialize(serializer),
                _ => serializer.collect_seq(items.iter().map(Shown::json)),
            },
        }
    }
}

impl AppTool for Show {
    type Args = ShowArgs;
    type Output = Shows;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "notebook_show",
            "Show the notebook vault. With no `refs`, every document (longform notes and canvases), newest-edited first, as rows `{kind, author, d, title, edited_at, ref}`. With `refs`, resolve each — a `notebook:<word-id>` ref, a `nostr:naddr`, a full id or a unique prefix — and answer what it names: a canvas `{id, title, nodes, edges, pending, …}`, a note `{d, ref, title, content, …}` or a node `{id, ref, x, y, w, h, text, …}`; one ref answers its object, several an array.",
            vec![
                ToolArg::new(
                    "refs",
                    ToolArgType::List,
                    "Refs to show: `notebook:<word-id>`, a `nostr:naddr`, a full id or a unique prefix. Omit to list the vault.",
                )
                .rest(),
            ],
        )
        .titled("Show notebook")
        .read_only()
        .command(&["notebook", "show"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let Account { author, .. } = account(cx);
        let txn = open_txn(cx)?;
        if args.refs.is_empty() {
            return Ok(Shows::Vault(event::list_vault(cx.ndb, &txn, &author)));
        }
        let canvases = event::fold_canvases(cx.ndb, &txn, &author);
        let notes = event::list_longform(cx.ndb, &txn, &author);
        // Every ref resolves before anything answers, so one bad ref fails
        // the call rather than answering part of it.
        args.refs
            .iter()
            .map(|sel| resolve::show(sel, &canvases, &notes))
            .collect::<Result<_, _>>()
            .map(Shows::Items)
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(match out {
            Shows::Vault(docs) => render::vault_docs(docs, render::now_secs(), true),
            Shows::Items(items) => render::shown(items, true),
        })
    }
}

/// `notebook_vault`: the longform notes, or the named notes' bodies.
struct Vault;

/// Arguments for [`Vault`].
#[derive(Deserialize)]
struct VaultArgs {
    /// Notes whose bodies to answer; the note listing when empty.
    #[serde(default)]
    notes: Vec<String>,
}

/// [`Vault`]'s answer: always an array of notes, as `notebook vault --json`
/// prints them.
#[derive(Debug)]
enum Notes {
    /// Every note, listed.
    Listed(Vec<LongformNote>),
    /// The notes asked for, whose bodies render.
    Picked(Vec<LongformNote>),
}

impl Notes {
    fn notes(&self) -> &[LongformNote] {
        match self {
            Self::Listed(notes) | Self::Picked(notes) => notes,
        }
    }
}

impl Serialize for Notes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.notes().iter().map(event::longform_json))
    }
}

impl AppTool for Vault {
    type Args = VaultArgs;
    type Output = Notes;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "notebook_vault",
            "List the notebook's longform notes, newest-edited first, or with `notes` answer just those notes (their markdown `content` included). Always an array of `{d, ref, naddr, author, title, summary, published_at, hashtags, created_at, content}`.",
            vec![
                ToolArg::new(
                    "notes",
                    ToolArgType::List,
                    "Notes to answer: a `notebook:<word-id>` ref, a `nostr:naddr`, or a `d` (a unique prefix will do). Omit to list them all.",
                )
                .rest(),
            ],
        )
        .titled("Notebook vault")
        .read_only()
        .command(&["notebook", "vault"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let Account { author, .. } = account(cx);
        let txn = open_txn(cx)?;
        let notes = event::list_longform(cx.ndb, &txn, &author);
        if args.notes.is_empty() {
            return Ok(Notes::Listed(notes));
        }
        args.notes
            .iter()
            .map(|sel| resolve::find_longform(&notes, sel).cloned())
            .collect::<Result<_, _>>()
            .map(Notes::Picked)
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(match out {
            Notes::Listed(notes) => render::vault(notes, render::now_secs(), true),
            Notes::Picked(notes) => render::vault_bodies(notes.iter(), true),
        })
    }
}

// ---------------------------------------------------------------------------
// writes
// ---------------------------------------------------------------------------

/// Counts the sealed envelopes an edit ingests. Nothing is published from
/// here: the host fans the vault's envelopes out (see the module docs).
#[derive(Default)]
struct Count(usize);

impl store::Publisher for Count {
    fn publish(&mut self, _event_frame: &str) {
        self.0 += 1;
    }
}

/// What an edit answers: `{ok, events}`, plus the new node's `node` and `ref`
/// after an add. The change ingests asynchronously, so re-query with
/// `notebook_show` to see the settled state.
#[derive(Serialize, Debug)]
struct EditAck {
    ok: bool,
    /// How many events the edit signed and sealed.
    events: usize,
    /// The hex id of the node an add created.
    #[serde(skip_serializing_if = "Option::is_none")]
    node: Option<String>,
    /// That node's `notebook:<word-id>` ref.
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    node_ref: Option<String>,
}

impl EditAck {
    /// The ack as the CLI prints it, `ok (2 events)`, then the new node's ref
    /// (muted) after an add.
    fn render(&self) -> String {
        match &self.node_ref {
            Some(node_ref) => format!(
                "ok ({} events) — {}\n",
                self.events,
                render::dim(true, node_ref)
            ),
            None => format!("ok ({} events)\n", self.events),
        }
    }
}

/// Apply the action `build` makes of the canvas `canvas` names (the sole
/// canvas when `None`), as the selected account.
fn edit(
    cx: &mut ToolContext,
    canvas: Option<&str>,
    build: impl FnOnce(&CanvasView) -> Result<CanvasAction, String>,
) -> Result<EditAck, String> {
    let account = account(cx);
    let secret = signer(cx, &account)?;
    let author = account.author;
    let canvases = event::fold_canvases(cx.ndb, &open_txn(cx)?, &author);
    let canvas_id = resolve::resolve_canvas_id(canvas, &canvases)?;
    let view = event::find_canvas(&canvases, &author, &canvas_id)
        .ok_or_else(|| format!("no canvas '{canvas_id}' — run `notebook seed`"))?;
    let action = build(view)?;

    let mut count = Count::default();
    let created = store::apply(
        cx.ndb, &canvas_id, view, &author, &secret, action, &mut count,
    );
    if count.0 == 0 {
        return Err("the edit produced no events (unknown node or edge?)".to_owned());
    }
    Ok(EditAck {
        ok: true,
        events: count.0,
        node: created.map(|id| id.hex()),
        node_ref: created.map(|id| wordid::node_ref(id.bytes())),
    })
}

/// A node's text content.
fn text_content(text: String) -> NodeContent {
    NodeContent {
        text,
        ..Default::default()
    }
}

/// Geometry an edit was given, any part of which may be left out: `add`
/// fills the rest from defaults, `move` from where the node is.
#[derive(Deserialize, Default)]
struct GeoArgs {
    #[serde(default)]
    x: Option<i64>,
    #[serde(default)]
    y: Option<i64>,
    #[serde(default)]
    w: Option<i64>,
    #[serde(default)]
    height: Option<i64>,
}

impl GeoArgs {
    /// `base` with each given part in place of its own; a negative size is 0.
    fn resolve(&self, base: Geometry) -> Geometry {
        let size = |n: Option<i64>, base: u64| n.map_or(base, |n| n.max(0) as u64);
        Geometry {
            x: self.x.unwrap_or(base.x),
            y: self.y.unwrap_or(base.y),
            w: size(self.w, base.w),
            h: size(self.height, base.h),
        }
    }
}

/// The geometry arguments `add` and `move` take. Height has no `-h`, which is
/// help.
fn geo_args() -> [ToolArg; 4] {
    [
        ToolArg::new(
            "x",
            ToolArgType::Integer,
            "The node's left edge, in canvas pixels.",
        )
        .short('x'),
        ToolArg::new(
            "y",
            ToolArgType::Integer,
            "The node's top edge, in canvas pixels.",
        )
        .short('y'),
        ToolArg::new(
            "w",
            ToolArgType::Integer,
            "The node's width, in canvas pixels.",
        )
        .short('w'),
        ToolArg::new(
            "height",
            ToolArgType::Integer,
            "The node's height, in canvas pixels.",
        ),
    ]
}

/// The `node` argument the node edits take, first on the command line.
fn node_arg(description: &'static str) -> ToolArg {
    ToolArg::new("node", ToolArgType::String, description)
        .required(true)
        .positional()
}

/// What a node selector is, for the node arguments' descriptions.
const NODE_SELECTOR: &str =
    "The node: its `notebook:<word-id>` ref, its full id, or a unique id prefix.";

/// `notebook_seed`: create a canvas.
struct Seed;

/// Arguments for [`Seed`].
#[derive(Deserialize)]
struct SeedArgs {
    #[serde(default)]
    title: Option<String>,
    /// The new canvas's `d`; minted when omitted.
    #[serde(default)]
    canvas: Option<String>,
}

/// [`Seed`]'s answer: the edit ack plus the new canvas.
#[derive(Serialize, Debug)]
struct Seeded {
    ok: bool,
    events: usize,
    /// The new canvas's `d`.
    canvas: String,
    /// Its `notebook:<word-id>` ref.
    #[serde(rename = "ref")]
    canvas_ref: String,
}

impl AppTool for Seed {
    type Args = SeedArgs;
    type Output = Seeded;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "notebook_seed",
            "Create a new, empty canvas in the notebook. Answers `{ok, events, canvas, ref}`: `canvas` is its `d`, `ref` its `notebook:<word-id>` ref.",
            vec![
                ToolArg::new("title", ToolArgType::String, "The canvas title.")
                    .default(serde_json::json!("Notebook"))
                    .rest()
                    .long("title"),
                ToolArg::new(
                    "canvas",
                    ToolArgType::String,
                    "The new canvas's `d`. A fresh one is minted when omitted.",
                ),
            ],
        )
        .titled("New canvas")
        .command(&["notebook", "seed"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let account = account(cx);
        let secret = signer(cx, &account)?;
        let author = account.author;
        // `canvas` names the new canvas's literal `d`: it can't be a ref, since
        // the canvas doesn't exist yet.
        let canvas_id = args.canvas.unwrap_or_else(store::mint_d);
        let canvases = event::fold_canvases(cx.ndb, &open_txn(cx)?, &author);
        if canvases.iter().any(|c| c.id == canvas_id) {
            return Err(format!("canvas '{canvas_id}' already exists"));
        }
        let title = args.title.unwrap_or_else(|| "Notebook".to_owned());
        let mut count = Count::default();
        store::seed_canvas(cx.ndb, &author, &secret, &canvas_id, &title, &mut count);
        if count.0 == 0 {
            return Err("seeding the canvas produced no events".to_owned());
        }
        Ok(Seeded {
            ok: true,
            events: count.0,
            canvas_ref: event::canvas_ref(&author, &canvas_id),
            canvas: canvas_id,
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(format!(
            "seeded canvas '{}' ({} events) — {}\n",
            out.canvas,
            out.events,
            render::dim(true, &out.canvas_ref)
        ))
    }
}

/// `notebook_add_node`: add a text node.
struct AddNode;

/// Arguments for [`AddNode`].
#[derive(Deserialize)]
struct AddNodeArgs {
    text: String,
    #[serde(flatten)]
    geo: GeoArgs,
    #[serde(default)]
    canvas: Option<String>,
}

impl AppTool for AddNode {
    type Args = AddNodeArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        let mut args = vec![
            ToolArg::new("text", ToolArgType::String, "The node's text.")
                .required(true)
                .rest(),
        ];
        args.extend(geo_args());
        args.push(canvas_arg());
        ToolSpec::new(
            "notebook_add_node",
            "Add a text node to a notebook canvas, on top of the others, at (0,0) and 250×120 unless placed. Answers `{ok, events, node, ref}`: `ref` is the new node's `notebook:<word-id>` ref.",
            args,
        )
        .titled("Add node")
        .command(&["notebook", "add"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let AddNodeArgs { text, geo, canvas } = args;
        edit(cx, canvas.as_deref(), |_| {
            Ok(CanvasAction::AddNode {
                kind: NodeKind::Text,
                geo: geo.resolve(Geometry {
                    x: 0,
                    y: 0,
                    w: NEW_W,
                    h: NEW_H,
                }),
                content: text_content(text),
            })
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// `notebook_move_node`: move or resize a node.
struct MoveNode;

/// Arguments for [`MoveNode`].
#[derive(Deserialize)]
struct MoveNodeArgs {
    node: String,
    #[serde(flatten)]
    geo: GeoArgs,
    #[serde(default)]
    canvas: Option<String>,
}

impl AppTool for MoveNode {
    type Args = MoveNodeArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        let mut args = vec![node_arg(NODE_SELECTOR)];
        args.extend(geo_args());
        args.push(canvas_arg());
        ToolSpec::new(
            "notebook_move_node",
            "Move or resize a node on a notebook canvas: each of `x`, `y`, `w` and `height` given replaces the node's own. Answers `{ok, events}`.",
            args,
        )
        .titled("Move node")
        .command(&["notebook", "move"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        edit(cx, args.canvas.as_deref(), |view| {
            let node = resolve::find_node(view, &args.node)?;
            Ok(CanvasAction::SetGeometry {
                node: node.id,
                geo: args.geo.resolve(node.geo),
            })
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// `notebook_edit_node`: replace a node's text.
struct EditNode;

/// Arguments for [`EditNode`].
#[derive(Deserialize)]
struct EditNodeArgs {
    node: String,
    text: String,
    #[serde(default)]
    canvas: Option<String>,
}

impl AppTool for EditNode {
    type Args = EditNodeArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "notebook_edit_node",
            "Replace a node's text on a notebook canvas. Answers `{ok, events}`.",
            vec![
                node_arg(NODE_SELECTOR),
                ToolArg::new("text", ToolArgType::String, "The node's new text.")
                    .required(true)
                    .rest(),
                canvas_arg(),
            ],
        )
        .titled("Edit node")
        .command(&["notebook", "edit"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let EditNodeArgs { node, text, canvas } = args;
        edit(cx, canvas.as_deref(), |view| {
            Ok(CanvasAction::EditContent {
                node: resolve::resolve_node(view, &node)?,
                content: text_content(text),
            })
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// `notebook_color_node`: recolor a node, or clear its color.
struct ColorNode;

/// Arguments for [`ColorNode`].
#[derive(Deserialize)]
struct ColorNodeArgs {
    node: String,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    canvas: Option<String>,
}

impl AppTool for ColorNode {
    type Args = ColorNodeArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "notebook_color_node",
            "Recolor a node on a notebook canvas: a JSON Canvas color, a preset `1`–`6` or a hex `#rrggbb`. Omit `color`, or give `none` or `-`, to clear it. Answers `{ok, events}`.",
            vec![
                node_arg(NODE_SELECTOR),
                ToolArg::new(
                    "color",
                    ToolArgType::String,
                    "The color: a preset `1`–`6` or `#rrggbb`; `none`, `-` or omitted clears it.",
                )
                // Positional only: `--color` is nd's own flag, for the text.
                .positional(),
                canvas_arg(),
            ],
        )
        .titled("Color node")
        .command(&["notebook", "color"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let ColorNodeArgs {
            node,
            color,
            canvas,
        } = args;
        // "none", "-" or "" mean "clear", as on the CLI.
        let color = color.filter(|c| !matches!(c.as_str(), "none" | "-" | ""));
        edit(cx, canvas.as_deref(), |view| {
            Ok(CanvasAction::Recolor {
                node: resolve::resolve_node(view, &node)?,
                color,
            })
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// `notebook_restack_node`: move a node to another place in the stack.
struct RestackNode;

/// Arguments for [`RestackNode`].
#[derive(Deserialize)]
struct RestackNodeArgs {
    node: String,
    index: usize,
    #[serde(default)]
    canvas: Option<String>,
}

impl AppTool for RestackNode {
    type Args = RestackNodeArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "notebook_restack_node",
            "Restack a node on a notebook canvas to a display index, 0 being the bottom. Answers `{ok, events}`.",
            vec![
                node_arg(NODE_SELECTOR),
                ToolArg::new(
                    "index",
                    ToolArgType::Integer,
                    "The display index to move it to; 0 is the bottom.",
                )
                .required(true)
                .positional(),
                canvas_arg(),
            ],
        )
        .titled("Restack node")
        .command(&["notebook", "restack"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        edit(cx, args.canvas.as_deref(), |view| {
            Ok(CanvasAction::Restack {
                node: resolve::resolve_node(view, &args.node)?,
                to_index: args.index,
            })
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// `notebook_delete_node`: remove a node.
struct DeleteNode;

/// Arguments for [`DeleteNode`].
#[derive(Deserialize)]
struct NodeArgs {
    node: String,
    #[serde(default)]
    canvas: Option<String>,
}

impl AppTool for DeleteNode {
    type Args = NodeArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "notebook_delete_node",
            "Remove a node from a notebook canvas (a reversible tombstone). Answers `{ok, events}`.",
            vec![node_arg(NODE_SELECTOR), canvas_arg()],
        )
        .titled("Delete node")
        .command(&["notebook", "delete"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        edit(cx, args.canvas.as_deref(), |view| {
            Ok(CanvasAction::DeleteNode {
                node: resolve::resolve_node(view, &args.node)?,
            })
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// `notebook_connect`: draw an edge between two nodes.
struct Connect;

/// Arguments for [`Connect`].
#[derive(Deserialize)]
struct ConnectArgs {
    from: String,
    to: String,
    #[serde(default)]
    from_side: Option<String>,
    #[serde(default)]
    to_side: Option<String>,
    #[serde(default)]
    canvas: Option<String>,
}

impl AppTool for Connect {
    type Args = ConnectArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "notebook_connect",
            "Draw an arrow from one node to another on a notebook canvas. One edge per ordered pair: connecting the same two again updates that edge. Answers `{ok, events}`.",
            vec![
                ToolArg::new("from", ToolArgType::String, NODE_SELECTOR)
                    .required(true)
                    .positional(),
                ToolArg::new("to", ToolArgType::String, NODE_SELECTOR)
                    .required(true)
                    .positional(),
                ToolArg::new(
                    "from_side",
                    ToolArgType::Enum(SIDES.to_vec()),
                    "The side of `from` the edge leaves.",
                ),
                ToolArg::new(
                    "to_side",
                    ToolArgType::Enum(SIDES.to_vec()),
                    "The side of `to` the edge arrives at.",
                ),
                canvas_arg(),
            ],
        )
        .titled("Connect nodes")
        .command(&["notebook", "connect"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let ConnectArgs {
            from,
            to,
            from_side,
            to_side,
            canvas,
        } = args;
        edit(cx, canvas.as_deref(), |view| {
            let from: NoteId = resolve::resolve_node(view, &from)?;
            let to: NoteId = resolve::resolve_node(view, &to)?;
            // Edge ids are stable per ordered pair, so re-drawing the same
            // connection updates that edge (latest-wins) rather than stacking
            // duplicates, as the app's `intent_to_action` does.
            Ok(CanvasAction::SetEdge {
                edge_id: format!("{}-{}", from.hex(), to.hex()),
                from,
                to,
                ends: EdgeEnds {
                    from_side,
                    to_side,
                    to_end: Some("arrow".to_string()),
                    ..Default::default()
                },
            })
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// `notebook_disconnect`: remove an edge.
struct Disconnect;

/// Arguments for [`Disconnect`].
#[derive(Deserialize)]
struct DisconnectArgs {
    edge: String,
    #[serde(default)]
    canvas: Option<String>,
}

impl AppTool for Disconnect {
    type Args = DisconnectArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "notebook_disconnect",
            "Remove an edge from a notebook canvas. Answers `{ok, events}`.",
            vec![
                ToolArg::new(
                    "edge",
                    ToolArgType::String,
                    "The edge: its id as `notebook_show` lists it, or a unique prefix.",
                )
                .required(true)
                .positional(),
                canvas_arg(),
            ],
        )
        .titled("Disconnect nodes")
        .command(&["notebook", "disconnect"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        edit(cx, args.canvas.as_deref(), |view| {
            let edge = resolve::resolve_edge(view, &args.edge)?;
            Ok(CanvasAction::DeleteEdge {
                edge_id: edge.id.clone(),
                from: edge.from,
                to: edge.to,
            })
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// `notebook_rename_canvas`: retitle a canvas.
struct RenameCanvas;

/// Arguments for [`RenameCanvas`].
#[derive(Deserialize)]
struct RenameArgs {
    title: String,
    #[serde(default)]
    canvas: Option<String>,
}

impl AppTool for RenameCanvas {
    type Args = RenameArgs;
    type Output = EditAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "notebook_rename_canvas",
            "Rename a notebook canvas. Answers `{ok, events}`.",
            vec![
                ToolArg::new("title", ToolArgType::String, "The canvas's new title.")
                    .required(true)
                    .rest()
                    .long("title"),
                canvas_arg(),
            ],
        )
        .titled("Rename canvas")
        .command(&["notebook", "rename"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let RenameArgs { title, canvas } = args;
        edit(cx, canvas.as_deref(), |_| {
            Ok(CanvasAction::Rename { title })
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

#[cfg(test)]
mod tests;
