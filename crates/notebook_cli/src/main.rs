//! `notebook` — a CLI for reading and mutating a notebook canvas against a
//! running notedeck's embedded relay.
//!
//! A sibling to [`headway_cli`]: the cache/sync/relay plumbing — keeping the
//! CLI's own nostrdb, reconciling it against the app's relay with NIP-77
//! negentropy, and the stored signing key — lives in `nostrdb_net`'s
//! `relay::sync` module. This file is just the canvas's command surface: parsing,
//! resolving node and edge arguments against the folded canvas, and rendering.
//! The canvas itself
//! is folded by the same reducer the egui app uses ([`notebook::event`]),
//! and edits are produced by the same store ([`notebook::store`]).

use std::env;
use std::process::ExitCode;

use nostrdb::{Ndb, Transaction};
use nostrdb_net::Pubkey;

use notebook::event::{self, CanvasView, Geometry, NodeContent, NodeKind, VaultDoc};
use notebook::render;
use notebook::resolve::{self, Shown};
use notebook::store::{self, CanvasAction, Publisher};

use nostrdb_net::relay::sync::Result;

/// The CLI's cache/key directory under the platform data dir (e.g.
/// `~/.local/share/notebook-cli` on Linux).
const APP: &str = "notebook-cli";

/// Default size of a freshly-created text node, in canvas pixels (mirrors the
/// app's `NEW_NODE_SIZE`).
const NEW_W: u64 = 250;
const NEW_H: u64 = 120;

#[tokio::main]
async fn main() -> ExitCode {
    // Terminate quietly on a closed pipe (`notebook show | head`) instead of
    // panicking in println! on EPIPE.
    nostrdb_net::relay::sync::reset_sigpipe();
    // Select the rustls CryptoProvider before any wss:// relay handshake; the
    // standalone CLIs never run notedeck's startup init that does this.
    enostr::install_crypto();
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
        /// Optional document/node selectors. Empty → list the whole vault (typed
        /// note|canvas rows). Non-empty → resolve each selector to a canvas, note,
        /// or node and print it, dispatching the render on the resolved type.
        targets: Vec<String>,
    },
    Vault {
        /// Optional note selectors. Empty → list the vault; otherwise print each
        /// named note's markdown body.
        notes: Vec<String>,
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
        Command::Login { nsec } => return nostrdb_net::relay::sync::login(nsec, APP),
        Command::Logout => return nostrdb_net::relay::sync::logout(APP),
        _ => {}
    }

    // The author whose canvas we read/write: an explicit override, else the
    // signing key's own pubkey.
    let author = match (&cli.author, &cli.secret) {
        (Some(pk), _) => *pk,
        (None, Some((_, pk))) => *pk,
        (None, None) => return Err("need --nsec to sign, or --author to read a canvas".into()),
    };

    let ndb = nostrdb_net::relay::sync::open_ndb(cli.db.as_deref(), APP)?;

    // The whole vault (canvases + longform) is sealed into the account's team-of-one
    // SNS workspace. Register its derived root so nostrdb transparently unwraps the
    // kind-1081 envelopes to the inner canvas/longform rumors the readers below
    // fold. The root is derived from the signing key, so a read-only `--author` CLI
    // can't open the private vault — it works against whatever is already cached.
    if let Some((sk, _)) = &cli.secret {
        store::register_workspace(&ndb, sk);
    }

    // `vault` is a local read of the unwrapped store; it skips the relay reconcile
    // and works fully offline against the cache.
    if let Command::Vault { notes } = &cli.command {
        return run_vault(&ndb, &author, notes, cli.json);
    }

    // Reconcile the sealed vault against the relay both ways so the cache and the
    // app converge regardless of which side an edit happened on. The wire unit is
    // the kind-1081 SNS envelope, authored by the workspace's team key; nostrdb
    // unwraps it locally (root registered above). Immutable per-edit, so nothing is
    // addressable. Reconciling needs the signing key to derive the channel — a
    // read-only CLI has nothing sealed to sync, so it works offline against the
    // cache. Best-effort: an unreachable relay also falls back to the cache.
    let mut relay = if let Some((sk, _)) = &cli.secret {
        let team_pubkey =
            store::workspace_team_pubkey(sk).ok_or("could not derive the vault's SNS channel")?;
        let filter = nostrdb::Filter::new()
            .kinds([nostrdb_net::sns::SNS_ENVELOPE_KIND as u64])
            .authors([team_pubkey.bytes()])
            .build();
        // `connect_and_sync` speaks `nostrdb_net::Pubkey`; convert the team key
        // across the boundary (both are `[u8; 32]` newtypes).
        let team_pubkey_nn = nostrdb_net::Pubkey::new(*team_pubkey.bytes());
        nostrdb_net::relay::sync::connect_and_sync(
            &cli.relay,
            &ndb,
            &team_pubkey_nn,
            &[nostrdb_net::sns::SNS_ENVELOPE_KIND],
            &filter,
            &|_| false,
        )
        .await?
    } else {
        None
    };

    let canvas = cli.canvas;
    let as_json = cli.json;
    let secret = cli.secret.map(|(s, _)| s);

    match cli.command {
        // Both `show` forms read the just-reconciled cache: the no-arg listing and
        // any canvas/node dispatch need the canvas fold that the reconcile above
        // populates (canvases travel over the relay; only longform is local-only,
        // which is why the `vault` command alone keeps the offline fast path).
        Command::Show { targets } => {
            let txn = open_txn(&ndb)?;
            if targets.is_empty() {
                print_vault_docs(&event::list_vault(&ndb, &txn, &author), as_json);
            } else {
                let canvases = event::fold_canvases(&ndb, &txn, &author);
                let notes = event::list_longform(&ndb, &txn, &author);
                // Resolve every selector first so one bad ref fails the whole command
                // rather than printing a partial result.
                let shown: Vec<Shown> = targets
                    .iter()
                    .map(|sel| resolve::show(sel, &canvases, &notes))
                    .collect::<std::result::Result<_, _>>()?;
                print_shown(&shown, as_json);
            }
        }

        Command::Seed { title } => {
            let secret = secret.ok_or("seed needs --nsec to sign")?;
            // `--canvas` names the new canvas's literal `d` (it can't be a ref — the
            // canvas doesn't exist yet); absent, mint a fresh opaque `d`. Either way
            // there is no well-known default id anymore.
            let canvas_id = canvas.clone().unwrap_or_else(store::mint_d);
            {
                let txn = open_txn(&ndb)?;
                if event::fold_canvases(&ndb, &txn, &author)
                    .iter()
                    .any(|c| c.id == canvas_id)
                {
                    return Err(format!("canvas '{canvas_id}' already exists").into());
                }
            }
            let mut sink = Collect::default();
            store::seed_canvas(&ndb, &author, &secret, &canvas_id, &title, &mut sink);
            let n = sink.0.len();
            nostrdb_net::relay::sync::publish(&mut relay, &sink.0).await?;
            println!(
                "seeded canvas '{canvas_id}' ({n} events){}",
                nostrdb_net::relay::sync::offline_note(&relay)
            );
        }

        edit => {
            let secret = secret.ok_or("this command needs --nsec to sign")?;
            // Resolve which canvas to edit from `--canvas <ref|d>` (or the sole
            // canvas when omitted), then take an owned snapshot so the read `txn`
            // doesn't straddle the async publish below.
            let (canvas_id, view) = {
                let txn = open_txn(&ndb)?;
                let canvases = event::fold_canvases(&ndb, &txn, &author);
                let canvas_id = resolve::resolve_canvas_id(canvas.as_deref(), &canvases)?;
                let view = event::find_canvas(&canvases, &author, &canvas_id)
                    .cloned()
                    .ok_or_else(|| format!("no canvas '{canvas_id}' — run `notebook seed`"))?;
                (canvas_id, view)
            };
            let action = build_action(&view, edit)?;

            let mut sink = Collect::default();
            store::apply(&ndb, &canvas_id, &view, &author, &secret, action, &mut sink);
            if sink.0.is_empty() {
                return Err("action produced no events (unknown node or edge?)".into());
            }
            let n = sink.0.len();
            nostrdb_net::relay::sync::publish(&mut relay, &sink.0).await?;
            println!(
                "ok ({n} events){}",
                nostrdb_net::relay::sync::offline_note(&relay)
            );
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
            let node = resolve::find_node(view, &node)?;
            CanvasAction::SetGeometry {
                node: node.id,
                geo: geo.resolve(node.geo),
            }
        }
        Command::Edit { node, text } => CanvasAction::EditContent {
            node: resolve::resolve_node(view, &node)?,
            content: text_content(text),
        },
        Command::Color { node, color } => CanvasAction::Recolor {
            node: resolve::resolve_node(view, &node)?,
            color,
        },
        Command::Restack { node, to_index } => CanvasAction::Restack {
            node: resolve::resolve_node(view, &node)?,
            to_index,
        },
        Command::Delete { node } => CanvasAction::DeleteNode {
            node: resolve::resolve_node(view, &node)?,
        },
        Command::Connect {
            from,
            to,
            from_side,
            to_side,
        } => {
            let from = resolve::resolve_node(view, &from)?;
            let to = resolve::resolve_node(view, &to)?;
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
            let e = resolve::resolve_edge(view, &edge)?;
            CanvasAction::DeleteEdge {
                edge_id: e.id.clone(),
                from: e.from,
                to: e.to,
            }
        }
        Command::Rename { title } => CanvasAction::Rename { title },
        Command::Show { .. }
        | Command::Vault { .. }
        | Command::Seed { .. }
        | Command::Login { .. }
        | Command::Logout => {
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

/// Open a read transaction, mapping the nostrdb error into the CLI's error type.
fn open_txn(ndb: &Ndb) -> Result<Transaction> {
    Transaction::new(ndb).map_err(|e| format!("opening a db transaction: {e}").into())
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
// output
// ---------------------------------------------------------------------------

/// Whether the muted parts of the text (refs, ages, geometry) are colored:
/// only when stdout is a terminal.
fn color() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

/// Print `value` as pretty JSON.
fn print_json(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".into())
    );
}

/// Print the whole vault: one typed row per document, newest-edited first, or
/// the rows' JSON.
fn print_vault_docs(docs: &[VaultDoc], as_json: bool) {
    if as_json {
        print_json(&docs.iter().map(event::vault_doc_json).collect());
        return;
    }
    print!("{}", render::vault_docs(docs, render::now_secs(), color()));
}

/// Print already-looked-up `show` targets, each in its natural shape. A single
/// target's JSON is a lone object; several are one array.
fn print_shown(items: &[Shown], as_json: bool) {
    if !as_json {
        print!("{}", render::shown(items, color()));
        return;
    }
    match items {
        [one] => print_json(&one.json()),
        _ => print_json(&items.iter().map(Shown::json).collect()),
    }
}

// ---------------------------------------------------------------------------
// vault (longform notes)
// ---------------------------------------------------------------------------

/// List `author`'s longform vault, or print the markdown bodies of the notes named
/// by `sels`. A purely local read of the unwrapped longform store — see the
/// short-circuit in [`run`] for why it skips the relay.
fn run_vault(ndb: &Ndb, author: &Pubkey, sels: &[String], as_json: bool) -> Result<()> {
    let txn = open_txn(ndb)?;
    let notes = event::list_longform(ndb, &txn, author);

    if sels.is_empty() {
        if as_json {
            print_json(&notes.iter().map(event::longform_json).collect());
        } else {
            print!("{}", render::vault(&notes, render::now_secs(), color()));
        }
        return Ok(());
    }

    // Resolve every selector first so one bad ref fails the whole command rather
    // than printing a partial result.
    let picked: Vec<&event::LongformNote> = sels
        .iter()
        .map(|sel| resolve::find_longform(&notes, sel))
        .collect::<std::result::Result<_, _>>()?;
    if as_json {
        print_json(&picked.iter().map(|n| event::longform_json(n)).collect());
    } else {
        print!("{}", render::vault_bodies(picked.into_iter(), color()));
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
    /// The `--canvas <ref|d>` selector, or `None`. There is no default id (the
    /// retired `store::CANVAS_ID`): `show` lists rather than opens, and the
    /// mutation commands resolve this against the folded canvases (falling back to
    /// the sole canvas when omitted).
    canvas: Option<String>,
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
            .or_else(|| nostrdb_net::relay::sync::stored_nsec(APP));
        let mut relay = env::var("NOTEBOOK_RELAY")
            .ok()
            .unwrap_or_else(|| nostrdb_net::relay::sync::DEFAULT_RELAY.to_string());
        let mut db = None;
        let mut canvas = None;
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
                "--canvas" => canvas = Some(value("--canvas")?),
                "--author" => author = Some(Pubkey::parse(&value("--author")?)?),
                "--title" => title = Some(value("--title")?),
                "--color" => color = Some(value("--color")?),
                "--from-side" => from_side = Some(value("--from-side")?),
                "--to-side" => to_side = Some(value("--to-side")?),
                "-x" | "--x" => geo.x = Some(num_i("--x")?),
                "-y" | "--y" => geo.y = Some(num_i("--y")?),
                "-w" | "--w" => geo.w = Some(num_i("--w")?.max(0) as u64),
                // No `-h` short: clap-style, `-h` is help. Height is `--height` only.
                "--height" => geo.h = Some(num_i("--height")?.max(0) as u64),
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
        // `parse_nsec` hands back a `nostrdb_net::Pubkey`; the rest of the CLI
        // (and the `notebook` store/event layer) speaks `nostrdb_net::Pubkey`.
        // Both are `[u8; 32]` newtypes, so bridge at this boundary and keep
        // everything downstream in enostr terms.
        let secret = match (&command, nsec) {
            (Command::Login { .. } | Command::Logout, _) => None,
            (_, Some(nsec)) => {
                let (sk, pk) = nostrdb_net::relay::sync::parse_nsec(&nsec)?;
                Some((sk, Pubkey::new(*pk.bytes())))
            }
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
            targets: rest.to_vec(),
        },
        "vault" => Command::Vault {
            notes: rest.to_vec(),
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
    show [refs...]            List the whole vault (notes + canvases, typed rows
                              with their notebook: refs), or resolve each given ref
                              and print it: a canvas, a note's body, or a node
                              (--json for machine output)
    vault [notes...]          List your longform vault, or print the named notes'
                              markdown bodies (--json for machine output). A local,
                              fully-offline notes-only view — `show` is the unified
                              surface; needs your --nsec/login to decrypt the vault.
    seed [title...]           Create a new canvas (mints a fresh id, or --canvas
                              <d> to name it; default title \"Notebook\")
    add <text...>             Add a text node (-x -y -w --height to place/size it)
    move <node> -x <n> -y <n> Move/resize a node (-w --height to resize)
    edit <node> <text...>     Replace a node's text
    color <node> <color>      Recolor a node (none/- clears)
    restack <node> <index>    Restack a node to a display index (0 = bottom)
    delete <node>             Remove a node (reversible tombstone)
    connect <from> <to>       Draw an edge (--from-side/--to-side for anchors)
    disconnect <edge>         Remove an edge (id from `show`)
    rename <title...>         Rename the canvas
    login <nsec>              Store a signing key for later runs
    logout                    Forget the stored signing key

    A <ref> is a notebook:word-id reference, a nostr:naddr/coordinate, a full id,
    or a unique short prefix. `show` resolves it across the whole vault (note,
    canvas, or node); `<node>` args to the edit commands name a node on the
    target canvas.

OPTIONS:
    --nsec <nsec>     Signing key for this run. Normally unnecessary — run
                      `notebook login` once and it's reused. ($NOTEBOOK_NSEC,
                      if set, takes precedence over the stored key.)
    --author <pk>     Canvas author to read (defaults to the signer)
    --relay <url>     Relay URL (or $NOTEBOOK_RELAY) [default: {DEFAULT_RELAY}]
    --canvas <ref|d>  Which canvas the edit commands target (a notebook: ref or a
                      canvas d). No default — with one canvas it's inferred; with
                      several, name it. `show` lists rather than opens, so it needs
                      no --canvas.
    --db <path>       nostrdb cache dir [default: <data-dir>/notebook-cli]
    -x, -y, -w        Node geometry for `add`/`move`
    --height <n>      Node height for `add`/`move` (no `-h`; that's --help)
    --color <c>       Color for `color`
    --json            Machine-readable output (show)
    -h, --help        Print this help",
        DEFAULT_RELAY = nostrdb_net::relay::sync::DEFAULT_RELAY,
    );
}
