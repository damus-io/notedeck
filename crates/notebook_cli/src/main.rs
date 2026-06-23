//! `notebook` — a CLI for reading and mutating a notebook canvas against a
//! running notedeck's embedded relay.
//!
//! A sibling to [`headway_cli`]: the cache/sync/relay plumbing — keeping the
//! CLI's own nostrdb, reconciling it against the app's relay with NIP-77
//! negentropy, and the stored signing key — lives in the shared [`relay_sync`]
//! crate. This file is just the canvas's command surface: parsing, resolving node
//! and edge arguments against the folded canvas, and rendering. The canvas itself
//! is folded by the same reducer the egui app uses ([`notedeck_notebook::event`]),
//! and edits are produced by the same store ([`notedeck_notebook::store`]).

use std::env;
use std::process::ExitCode;

use enostr::{NoteId, Pubkey};
use nostrdb::{Ndb, Transaction};

use notedeck_notebook::event::{
    self, CanvasView, EdgeView, Geometry, NodeContent, NodeKind, NodeView,
};
use notedeck_notebook::store::{self, CanvasAction, Publisher};
use notedeck_notebook::wordid;

use relay_sync::Result;

/// The CLI's cache/key directory under the platform data dir (e.g.
/// `~/.local/share/notebook-cli` on Linux).
const APP: &str = "notebook-cli";

/// Default size of a freshly-created text node, in canvas pixels (mirrors the
/// app's `NEW_NODE_SIZE`).
const NEW_W: u64 = 250;
const NEW_H: u64 = 120;

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(e) = run().await {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// A parsed command. Node/edge arguments are still raw strings here; they're
/// resolved against the canvas once it's folded.
enum Command {
    Show {
        /// Optional node selectors. When non-empty, `show` prints only these
        /// nodes rather than the whole canvas.
        nodes: Vec<String>,
    },
    Seed {
        title: String,
    },
    Add {
        text: String,
        geo: PartialGeo,
    },
    Move {
        node: String,
        geo: PartialGeo,
    },
    Edit {
        node: String,
        text: String,
    },
    Color {
        node: String,
        color: Option<String>,
    },
    Restack {
        node: String,
        to_index: usize,
    },
    Delete {
        node: String,
    },
    Connect {
        from: String,
        to: String,
        from_side: Option<String>,
        to_side: Option<String>,
    },
    Disconnect {
        edge: String,
    },
    Rename {
        title: String,
    },
    Login {
        nsec: String,
    },
    Logout,
}

/// Geometry pieces supplied on the command line. `move` fills the unset fields
/// from the node's current geometry; `add` fills them from defaults.
#[derive(Default)]
struct PartialGeo {
    x: Option<i64>,
    y: Option<i64>,
    w: Option<u64>,
    h: Option<u64>,
}

impl PartialGeo {
    /// Resolve against a base geometry, taking each supplied field and falling
    /// back to `base` otherwise.
    fn resolve(&self, base: Geometry) -> Geometry {
        Geometry {
            x: self.x.unwrap_or(base.x),
            y: self.y.unwrap_or(base.y),
            w: self.w.unwrap_or(base.w),
            h: self.h.unwrap_or(base.h),
        }
    }
}

async fn run() -> Result<()> {
    let cli = match Cli::parse(env::args().skip(1))? {
        Some(cli) => cli,
        None => {
            print_usage();
            return Ok(());
        }
    };

    // `login`/`logout` manage the stored key and touch neither the cache nor a
    // relay, so handle them before any of that machinery spins up.
    match &cli.command {
        Command::Login { nsec } => return relay_sync::login(nsec, APP),
        Command::Logout => return relay_sync::logout(APP),
        _ => {}
    }

    // The author whose canvas we read/write: an explicit override, else the
    // signing key's own pubkey.
    let author = match (&cli.author, &cli.secret) {
        (Some(pk), _) => *pk,
        (None, Some((_, pk))) => *pk,
        (None, None) => return Err("need --nsec to sign, or --author to read a canvas".into()),
    };

    let ndb = relay_sync::open_ndb(cli.db.as_deref(), APP)?;

    // Reconcile the local cache against the relay both ways so the cache and the
    // app converge regardless of which side an edit happened on. Best-effort: an
    // unreachable relay leaves us working offline against the cache.
    let filter = event::notebook_filter(&author);
    let mut relay = relay_sync::connect_and_sync(
        &cli.relay,
        &ndb,
        &author,
        &event::NOTEBOOK_KINDS,
        &filter,
        &event::is_addressable,
    )
    .await?;

    let canvas = cli.canvas;
    let as_json = cli.json;
    let secret = cli.secret.map(|(s, _)| s);

    match cli.command {
        Command::Show { nodes } => match load_canvas(&ndb, &author, &canvas) {
            Some(view) if nodes.is_empty() => print_canvas(&view, as_json),
            Some(view) => print_nodes(&view, &nodes, as_json)?,
            None if as_json => println!("null"),
            None => println!(
                "no canvas '{}' for {} — run `notebook seed`",
                canvas,
                author.hex()
            ),
        },

        Command::Seed { title } => {
            let secret = secret.ok_or("seed needs --nsec to sign")?;
            if load_canvas(&ndb, &author, &canvas).is_some() {
                return Err(format!("canvas '{canvas}' already exists").into());
            }
            let mut sink = Collect::default();
            store::seed_canvas(&ndb, &author, &secret, &canvas, &title, &mut sink);
            let n = sink.0.len();
            relay_sync::publish(&mut relay, &sink.0).await?;
            println!(
                "seeded canvas '{canvas}' ({n} events){}",
                relay_sync::offline_note(&relay)
            );
        }

        edit => {
            let secret = secret.ok_or("this command needs --nsec to sign")?;
            let view = load_canvas(&ndb, &author, &canvas)
                .ok_or_else(|| format!("no canvas '{canvas}' — run `notebook seed`"))?;
            let action = build_action(&view, edit)?;

            let mut sink = Collect::default();
            store::apply(&ndb, &canvas, &view, &author, &secret, action, &mut sink);
            if sink.0.is_empty() {
                return Err("action produced no events (unknown node or edge?)".into());
            }
            let n = sink.0.len();
            relay_sync::publish(&mut relay, &sink.0).await?;
            println!("ok ({n} events){}", relay_sync::offline_note(&relay));
        }
    }

    Ok(())
}

/// Translate a resolved [`Command`] into a [`CanvasAction`], resolving node and
/// edge arguments against `view`.
fn build_action(view: &CanvasView, command: Command) -> Result<CanvasAction> {
    Ok(match command {
        Command::Add { text, geo } => CanvasAction::AddNode {
            kind: NodeKind::Text,
            geo: geo.resolve(Geometry {
                x: 0,
                y: 0,
                w: NEW_W,
                h: NEW_H,
            }),
            content: text_content(text),
        },
        Command::Move { node, geo } => {
            let node = find_node(view, &node)?;
            CanvasAction::SetGeometry {
                node: node.id,
                geo: geo.resolve(node.geo),
            }
        }
        Command::Edit { node, text } => CanvasAction::EditContent {
            node: resolve_node(view, &node)?,
            content: text_content(text),
        },
        Command::Color { node, color } => CanvasAction::Recolor {
            node: resolve_node(view, &node)?,
            color,
        },
        Command::Restack { node, to_index } => CanvasAction::Restack {
            node: resolve_node(view, &node)?,
            to_index,
        },
        Command::Delete { node } => CanvasAction::DeleteNode {
            node: resolve_node(view, &node)?,
        },
        Command::Connect {
            from,
            to,
            from_side,
            to_side,
        } => {
            let from = resolve_node(view, &from)?;
            let to = resolve_node(view, &to)?;
            // Edge ids are stable per ordered pair, so re-drawing the same
            // connection updates that edge (latest-wins) rather than stacking
            // duplicates — matching the app's `intent_to_action`.
            CanvasAction::SetEdge {
                edge_id: format!("{}-{}", from.hex(), to.hex()),
                from,
                to,
                ends: event::EdgeEnds {
                    from_side,
                    to_side,
                    to_end: Some("arrow".to_string()),
                    ..Default::default()
                },
            }
        }
        Command::Disconnect { edge } => {
            let e = resolve_edge(view, &edge)?;
            CanvasAction::DeleteEdge {
                edge_id: e.id.clone(),
                from: e.from,
                to: e.to,
            }
        }
        Command::Rename { title } => CanvasAction::Rename { title },
        Command::Show { .. } | Command::Seed { .. } | Command::Login { .. } | Command::Logout => {
            unreachable!("handled before build_action")
        }
    })
}

fn text_content(text: String) -> NodeContent {
    NodeContent {
        text,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// canvas loading
// ---------------------------------------------------------------------------

fn load_canvas(ndb: &Ndb, author: &Pubkey, canvas_id: &str) -> Option<CanvasView> {
    let txn = Transaction::new(ndb).ok()?;
    event::load_canvas(ndb, &txn, author, canvas_id)
}

/// Collects the `["EVENT", {...}]` frames an edit produces so they can be
/// forwarded to the relay after `apply` returns.
#[derive(Default)]
struct Collect(Vec<String>);

impl Publisher for Collect {
    fn publish(&mut self, frame: &str) {
        self.0.push(frame.to_string());
    }
}

// ---------------------------------------------------------------------------
// argument resolution
// ---------------------------------------------------------------------------

fn all_nodes(view: &CanvasView) -> impl Iterator<Item = &NodeView> {
    view.nodes.iter().chain(view.pending.iter())
}

/// Resolve a node argument: a full 64-char hex id, a word-id, or a unique hex
/// prefix matched against every node on the canvas (pending ones included).
fn resolve_node(view: &CanvasView, sel: &str) -> Result<NoteId> {
    find_node(view, sel).map(|n| n.id)
}

/// Resolve a node selector, accepting (in order): a full 64-char hex id; a
/// word-id like `notebook@maple-river-canyon` (the `<canvas>@` prefix is
/// optional, and a bare leading `@` is fine too); or a unique hex prefix.
fn find_node<'a>(view: &'a CanvasView, sel: &str) -> Result<&'a NodeView> {
    if let Ok(id) = NoteId::from_hex(sel) {
        return all_nodes(view)
            .find(|n| n.id == id)
            .ok_or_else(|| format!("no node matching '{sel}'").into());
    }

    // Word-id: drop an optional `<canvas>@` prefix (or a bare leading `@`), then
    // match by re-encoding each node — exactly how a git short hash is resolved.
    let words = sel
        .strip_prefix(&format!("{}@", view.id.to_lowercase()))
        .or_else(|| sel.strip_prefix('@'))
        .unwrap_or(sel);
    if let Some(n) = all_nodes(view).find(|n| wordid::encode(n.id.bytes()) == words) {
        return Ok(n);
    }

    let sel = sel.to_lowercase();
    let mut hits = all_nodes(view).filter(|n| n.id.hex().starts_with(&sel));
    match (hits.next(), hits.next()) {
        (Some(n), None) => Ok(n),
        (Some(_), Some(_)) => Err(format!("ambiguous node prefix '{sel}'").into()),
        _ => Err(format!("no node matching '{sel}'").into()),
    }
}

/// Resolve an edge argument by full id or unique id prefix.
fn resolve_edge<'a>(view: &'a CanvasView, sel: &str) -> Result<&'a EdgeView> {
    if let Some(e) = view.edges.iter().find(|e| e.id == sel) {
        return Ok(e);
    }
    let mut hits = view.edges.iter().filter(|e| e.id.starts_with(sel));
    match (hits.next(), hits.next()) {
        (Some(e), None) => Ok(e),
        (Some(_), Some(_)) => Err(format!("ambiguous edge prefix '{sel}'").into()),
        _ => Err(format!("no edge matching '{sel}'").into()),
    }
}

// ---------------------------------------------------------------------------
// output
// ---------------------------------------------------------------------------

/// A node's human-friendly reference for display/addressing: the canvas id, an
/// `@`, then the node's word-id, e.g. `notebook@maple-river-canyon`, muted. This
/// is what a human quotes; it resolves back via [`find_node`].
fn word_ref(canvas: &str, id: &NoteId) -> String {
    relay_sync::dim(&format!("{canvas}@{}", wordid::encode(id.bytes())))
}

/// The first line of a node's text, trimmed and truncated, for one-line listings.
fn one_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() > 60 {
        let cut: String = line.chars().take(57).collect();
        format!("{cut}…")
    } else if line.is_empty() {
        "(empty)".to_string()
    } else {
        line.to_string()
    }
}

fn print_canvas(view: &CanvasView, as_json: bool) {
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&event::canvas_json(view))
                .unwrap_or_else(|_| "null".into())
        );
        return;
    }

    println!("{}{}", view.title, if view.open { "  [open]" } else { "" });

    println!("\nNodes ({})", view.nodes.len());
    for n in &view.nodes {
        print_node_line(&view.id, n);
    }
    if !view.edges.is_empty() {
        println!("\nEdges ({})", view.edges.len());
        for e in &view.edges {
            println!(
                "  {} → {}  {}",
                word_ref(&view.id, &e.from),
                word_ref(&view.id, &e.to),
                relay_sync::dim(&e.id),
            );
        }
    }
    if !view.pending.is_empty() {
        println!(
            "\nPending ({}) — proposals on a closed canvas",
            view.pending.len()
        );
        for n in &view.pending {
            print_node_line(&view.id, n);
        }
    }
}

fn print_node_line(canvas: &str, n: &NodeView) {
    let geo = relay_sync::dim(&format!(
        "({},{} {}×{})",
        n.geo.x, n.geo.y, n.geo.w, n.geo.h
    ));
    println!(
        "  {}  {}  {}",
        one_line(&n.content.text),
        geo,
        word_ref(canvas, &n.id),
    );
}

/// Print only the nodes named by `sels` (each a node id or unique short prefix).
fn print_nodes(view: &CanvasView, sels: &[String], as_json: bool) -> Result<()> {
    // Resolve every selector first so a bad id fails the whole command rather
    // than printing a partial result.
    let nodes: Vec<&NodeView> = sels
        .iter()
        .map(|sel| find_node(view, sel))
        .collect::<Result<_>>()?;

    if as_json {
        let out: Vec<_> = nodes.iter().map(|n| event::node_json(n)).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&out).unwrap_or_else(|_| "null".into())
        );
    } else {
        for n in &nodes {
            print_node_line(&view.id, n);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// argument parsing
// ---------------------------------------------------------------------------

struct Cli {
    secret: Option<([u8; 32], Pubkey)>,
    author: Option<Pubkey>,
    relay: String,
    db: Option<String>,
    canvas: String,
    json: bool,
    command: Command,
}

impl Cli {
    /// Parse args (without the program name). Returns `Ok(None)` when usage
    /// should be printed (no command, `-h`/`--help`).
    fn parse(args: impl Iterator<Item = String>) -> Result<Option<Self>> {
        // Precedence: `--nsec` overrides the `NOTEBOOK_NSEC` env var, which
        // overrides the key stored by `login`.
        let mut nsec = env::var("NOTEBOOK_NSEC")
            .ok()
            .or_else(|| relay_sync::stored_nsec(APP));
        let mut relay = env::var("NOTEBOOK_RELAY")
            .ok()
            .unwrap_or_else(|| relay_sync::DEFAULT_RELAY.to_string());
        let mut db = None;
        let mut canvas = store::CANVAS_ID.to_string();
        let mut author = None;
        let mut json = false;
        let mut geo = PartialGeo::default();
        let mut title: Option<String> = None;
        let mut color: Option<String> = None;
        let mut from_side = None;
        let mut to_side = None;
        let mut positionals: Vec<String> = Vec::new();

        let mut args = args;
        while let Some(arg) = args.next() {
            let mut value = |flag: &str| {
                args.next()
                    .ok_or_else(|| format!("{flag} needs a value").into())
                    as Result<String>
            };
            let mut num_i = |flag: &str| -> Result<i64> {
                value(flag)?
                    .parse()
                    .map_err(|_| format!("{flag} must be a number").into())
            };
            match arg.as_str() {
                "-h" | "--help" => return Ok(None),
                "--nsec" => nsec = Some(value("--nsec")?),
                "--relay" => relay = value("--relay")?,
                "--db" => db = Some(value("--db")?),
                "--canvas" => canvas = value("--canvas")?,
                "--author" => author = Some(Pubkey::parse(&value("--author")?)?),
                "--title" => title = Some(value("--title")?),
                "--color" => color = Some(value("--color")?),
                "--from-side" => from_side = Some(value("--from-side")?),
                "--to-side" => to_side = Some(value("--to-side")?),
                "-x" | "--x" => geo.x = Some(num_i("--x")?),
                "-y" | "--y" => geo.y = Some(num_i("--y")?),
                "-w" | "--w" => geo.w = Some(num_i("--w")?.max(0) as u64),
                "--height" | "--h" => geo.h = Some(num_i("--h")?.max(0) as u64),
                "--json" => json = true,
                other if other.starts_with("--") => {
                    return Err(format!("unknown flag '{other}'").into());
                }
                _ => positionals.push(arg),
            }
        }

        let Some((name, rest)) = positionals.split_first() else {
            return Ok(None);
        };
        let command = parse_command(name, rest, geo, title, color, from_side, to_side)?;

        // `login`/`logout` manage the stored key themselves, so don't parse (and
        // potentially reject on) whatever key is currently configured.
        let secret = match (&command, nsec) {
            (Command::Login { .. } | Command::Logout, _) => None,
            (_, Some(nsec)) => Some(relay_sync::parse_nsec(&nsec)?),
            (_, None) => None,
        };

        Ok(Some(Cli {
            secret,
            author,
            relay,
            db,
            canvas,
            json,
            command,
        }))
    }
}

fn parse_command(
    name: &str,
    rest: &[String],
    geo: PartialGeo,
    title: Option<String>,
    color: Option<String>,
    from_side: Option<String>,
    to_side: Option<String>,
) -> Result<Command> {
    let node = || -> Result<String> { arg(rest, 0, name) };
    Ok(match name {
        "show" => Command::Show {
            nodes: rest.to_vec(),
        },
        "seed" => Command::Seed {
            // `seed [title...]`, or --title, defaulting to "Notebook".
            title: title
                .or_else(|| (!rest.is_empty()).then(|| rest.join(" ")))
                .unwrap_or_else(|| "Notebook".to_string()),
        },
        "add" => Command::Add {
            text: joined(rest, 0, name)?,
            geo,
        },
        "move" => Command::Move { node: node()?, geo },
        "edit" => Command::Edit {
            node: node()?,
            text: joined(rest, 1, name)?,
        },
        "color" => Command::Color {
            node: node()?,
            // A second positional sets the color; `--color` works too. "none",
            // "-" or "" clears it.
            color: clear_color(rest.get(1).cloned().or(color)),
        },
        "restack" => Command::Restack {
            node: node()?,
            to_index: arg(rest, 1, name)?
                .parse()
                .map_err(|_| "restack index must be a number")?,
        },
        "delete" => Command::Delete { node: node()? },
        "connect" => Command::Connect {
            from: arg(rest, 0, name)?,
            to: arg(rest, 1, name)?,
            from_side,
            to_side,
        },
        "disconnect" => Command::Disconnect { edge: node()? },
        "rename" => Command::Rename {
            title: title.map(Ok).unwrap_or_else(|| joined(rest, 0, name))?,
        },
        "login" => Command::Login {
            nsec: arg(rest, 0, name)?,
        },
        "logout" => Command::Logout,
        other => return Err(format!("unknown command '{other}' (try `notebook --help`)").into()),
    })
}

/// Map a color argument that means "clear" (`none`/`-`/empty) to `None`, else
/// keep the color. `None` input also stays `None`.
fn clear_color(color: Option<String>) -> Option<String> {
    color.filter(|c| !matches!(c.as_str(), "none" | "-" | ""))
}

/// The `idx`th positional argument to a command, or an error naming the command.
fn arg(rest: &[String], idx: usize, cmd: &str) -> Result<String> {
    rest.get(idx)
        .cloned()
        .ok_or_else(|| format!("`{cmd}` is missing an argument").into())
}

/// Everything from `idx` onward, space-joined — for free-text bodies/titles.
fn joined(rest: &[String], idx: usize, cmd: &str) -> Result<String> {
    let parts = rest.get(idx..).unwrap_or_default();
    if parts.is_empty() {
        return Err(format!("`{cmd}` is missing text").into());
    }
    Ok(parts.join(" "))
}

fn print_usage() {
    eprintln!(
        "\
notebook — interact with a notebook canvas over a running notedeck's relay

USAGE:
    notebook [OPTIONS] <COMMAND>

COMMANDS:
    show [nodes...]            Print the canvas, or just the given nodes
                              (--json for machine output)
    seed [title...]           Seed the canvas if none exists (default \"Notebook\")
    add <text...>             Add a text node (-x -y -w -h to place/size it)
    move <node> -x <n> -y <n> Move/resize a node (-w -h to resize)
    edit <node> <text...>     Replace a node's text
    color <node> <color>      Recolor a node (none/- clears)
    restack <node> <index>    Restack a node to a display index (0 = bottom)
    delete <node>             Remove a node (reversible tombstone)
    connect <from> <to>       Draw an edge (--from-side/--to-side for anchors)
    disconnect <edge>         Remove an edge (id from `show`)
    rename <title...>         Rename the canvas
    login <nsec>              Store a signing key for later runs
    logout                    Forget the stored signing key

    <node> is a node id, a word-id like notebook@maple-river-canyon, or a
    unique short prefix (see `show`).

OPTIONS:
    --nsec <nsec>     Signing key for this run. Normally unnecessary — run
                      `notebook login` once and it's reused. ($NOTEBOOK_NSEC,
                      if set, takes precedence over the stored key.)
    --author <pk>     Canvas author to read (defaults to the signer)
    --relay <url>     Relay URL (or $NOTEBOOK_RELAY) [default: {DEFAULT_RELAY}]
    --canvas <id>     Canvas id [default: {canvas}]
    --db <path>       nostrdb cache dir [default: <data-dir>/notebook-cli]
    -x, -y, -w, -h    Node geometry for `add`/`move`
    --color <c>       Color for `color`
    --json            Machine-readable output (show)
    -h, --help        Print this help",
        DEFAULT_RELAY = relay_sync::DEFAULT_RELAY,
        canvas = store::CANVAS_ID,
    );
}
