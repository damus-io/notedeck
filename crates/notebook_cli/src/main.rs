//! `notebook` — a CLI for reading and mutating a notebook canvas against a
//! running notedeck's embedded relay.
//!
//! A sibling to [`headway_cli`]: the cache/sync/relay plumbing — keeping the
//! CLI's own nostrdb, reconciling it against the app's relay with NIP-77
//! negentropy, and the stored signing key — lives in `nostrdb_net`'s
//! `relay::sync` module. This file is just the canvas's command surface: parsing,
//! resolving node and edge arguments against the folded canvas, and rendering.
//! The canvas itself
//! is folded by the same reducer the egui app uses ([`notedeck_notebook::event`]),
//! and edits are produced by the same store ([`notedeck_notebook::store`]).

use std::env;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use enostr::{NoteId, Pubkey};
use nostrdb::{Ndb, Transaction};

use notedeck_notebook::event::{
    self, CanvasView, EdgeView, Geometry, NodeContent, NodeKind, NodeView, NotebookTarget,
    VaultDoc, VaultDocKind,
};
use notedeck_notebook::store::{self, CanvasAction, Publisher};
use notedeck_notebook::wordid;

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

    // Register the signer's key so nostrdb transparently unwraps our PNS-wrapped
    // longform (kind 1080 → 30023) for the vault reader below, mirroring what the
    // app does when the account is added. Without it the vault reads empty.
    if let Some((sk, _)) = &cli.secret {
        ndb.add_key(sk);
    }

    // The vault is a local read of the PNS-unwrapped longform store. Longform
    // never travels over the relay — pushing the plaintext kind-30023 would leak
    // the article (cross-device sync fans the 1080 wrapper instead; see
    // notedeck_notebook and headway:notebook/merry-patch-boost) — so a `vault`
    // command skips the canvas reconcile entirely and works fully offline.
    if let Command::Vault { notes } = &cli.command {
        return run_vault(&ndb, &author, notes, cli.json);
    }

    // Reconcile the local cache against the relay both ways so the cache and the
    // app converge regardless of which side an edit happened on. Best-effort: an
    // unreachable relay leaves us working offline against the cache.
    let filter = event::notebook_filter(&author);
    // `connect_and_sync` speaks `nostrdb_net::Pubkey`; convert our enostr author
    // across the boundary (both are `[u8; 32]` newtypes).
    let author_nn = nostrdb_net::Pubkey::new(*author.bytes());
    let mut relay = nostrdb_net::relay::sync::connect_and_sync(
        &cli.relay,
        &ndb,
        &author_nn,
        &event::NOTEBOOK_KINDS,
        &filter,
        &event::is_addressable,
    )
    .await?;

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
                let canvases = fold_all_canvases(&ndb, &txn, &author);
                let notes = event::list_longform(&ndb, &txn, &author);
                // Resolve every selector first so one bad ref fails the whole command
                // rather than printing a partial result (as `print_nodes` did).
                let resolved: Vec<NotebookTarget> = targets
                    .iter()
                    .map(|sel| resolve_target(sel, &canvases, &notes))
                    .collect::<Result<_>>()?;
                show_targets(&resolved, &canvases, &notes, as_json)?;
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
                if fold_all_canvases(&ndb, &txn, &author)
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
                let canvases = fold_all_canvases(&ndb, &txn, &author);
                let canvas_id = resolve_canvas_id(canvas.as_deref(), &canvases)?;
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

/// Fold *all* of `author`'s canvases into their finalized views — the input the
/// unified resolver and the whole-vault `show` need. Unlike [`load_canvas`] (which
/// picks one canvas by id), this surfaces the multi-canvas set: deleted canvases
/// are already dropped by the reducer's `finalize`.
fn fold_all_canvases(ndb: &Ndb, txn: &Transaction, author: &Pubkey) -> Vec<CanvasView> {
    event::fold_canvas(ndb, txn, author)
        .map(|reducer| reducer.finalize())
        .unwrap_or_default()
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

/// Resolve a node argument: a full 64-char hex id, a `notebook:<word-id>`
/// reference, or a unique hex prefix matched against every node on the canvas
/// (pending ones included).
fn resolve_node(view: &CanvasView, sel: &str) -> Result<NoteId> {
    find_node(view, sel).map(|n| n.id)
}

/// Resolve a node selector, accepting (in order): a full 64-char hex id; a
/// `notebook:<word-id>` reference (the `notebook:` scheme is required — a bare
/// word-id is not a reference); or a unique hex prefix.
fn find_node<'a>(view: &'a CanvasView, sel: &str) -> Result<&'a NodeView> {
    if let Ok(id) = NoteId::from_hex(sel) {
        return all_nodes(view)
            .find(|n| n.id == id)
            .ok_or_else(|| format!("no node matching '{sel}'").into());
    }

    // A `notebook:<word-id>` reference: match by re-encoding each node — exactly
    // how a git short hash is resolved.
    if let Some(words) = wordid::parse_ref(sel)
        && let Some(n) = all_nodes(view).find(|n| wordid::encode(n.id.bytes()) == words)
    {
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

/// Resolve a `show` selector to a [`NotebookTarget`] across the whole notebook,
/// unifying the two context-specific matchers `show` used to layer by hand (node
/// lookup vs. the `vault` command's note lookup).
///
/// The flat `notebook:<word-id>` namespace ([`event::resolve_ref`]) is layered
/// *between* the explicit forms and the loose fallbacks — deliberately, so the
/// namespace stays authoritative: an explicit `naddr`/coordinate wins first (it
/// names one exact kind), a well-formed `notebook:` ref that resolves to nothing
/// **fails here** rather than falling through to a prefix match (a mistyped word-id
/// can't silently resolve to some other item — the care [`find_longform`] already
/// took), and only a bare (scheme-less) selector reaches the raw-hex/`d`-prefix
/// fallbacks.
fn resolve_target(
    sel: &str,
    canvases: &[CanvasView],
    notes: &[event::LongformNote],
) -> Result<NotebookTarget> {
    // 1. An explicit `nostr:naddr` / bare coordinate — each kind-discriminated, so
    //    it names exactly one document type.
    if let Some((author, d)) = event::parse_canvas_naddr(sel) {
        return canvases
            .iter()
            .find(|c| c.id == d && c.author == *author.bytes())
            .map(|c| NotebookTarget::Canvas {
                author,
                d: c.id.clone(),
            })
            .ok_or_else(|| format!("no canvas matching '{sel}'").into());
    }
    if let Some((author, d)) = event::parse_longform_naddr(sel) {
        return notes
            .iter()
            .find(|n| n.d == d && n.author == *author.bytes())
            .map(|n| NotebookTarget::Note {
                author,
                d: n.d.clone(),
            })
            .ok_or_else(|| format!("no vault note matching '{sel}'").into());
    }

    // 2. A `notebook:<word-id>` reference — resolved across the flat namespace
    //    (documents first, then nodes). Ref-shaped-but-unmatched fails.
    if let Some(word_id) = wordid::parse_ref(sel) {
        return event::resolve_ref(word_id, canvases, notes)
            .ok_or_else(|| format!("no notebook item matching '{sel}'").into());
    }

    // 3. A full 64-char hex node id.
    if let Ok(id) = NoteId::from_hex(sel) {
        return canvases
            .iter()
            .flat_map(all_nodes)
            .find(|n| n.id == id)
            .map(|n| NotebookTarget::Node { id: n.id })
            .ok_or_else(|| format!("no node matching '{sel}'").into());
    }

    // 4. Fallback: a unique hex-prefix node id, or a unique `d` prefix across notes
    //    and canvases — ambiguity across the union is rejected.
    prefix_target(sel, canvases, notes)
}

/// The loose-prefix arm of [`resolve_target`]: match a bare (scheme-less) selector
/// as a node id hex prefix or a note/canvas `d` prefix, requiring exactly one hit
/// across the whole notebook so an ambiguous prefix fails rather than silently
/// picking one.
fn prefix_target(
    sel: &str,
    canvases: &[CanvasView],
    notes: &[event::LongformNote],
) -> Result<NotebookTarget> {
    let hex = sel.to_lowercase();
    let mut hits: Vec<NotebookTarget> = Vec::new();
    for c in canvases {
        for n in all_nodes(c) {
            if n.id.hex().starts_with(&hex) {
                hits.push(NotebookTarget::Node { id: n.id });
            }
        }
        if c.id.starts_with(sel) {
            hits.push(NotebookTarget::Canvas {
                author: Pubkey::new(c.author),
                d: c.id.clone(),
            });
        }
    }
    for n in notes {
        if n.d.starts_with(sel) {
            hits.push(NotebookTarget::Note {
                author: Pubkey::new(n.author),
                d: n.d.clone(),
            });
        }
    }
    match hits.len() {
        1 => Ok(hits.pop().unwrap()),
        0 => Err(format!("no notebook item matching '{sel}'").into()),
        _ => Err(format!("ambiguous selector '{sel}'").into()),
    }
}

/// Resolve `--canvas <ref|d>` to a concrete canvas id against `author`'s folded
/// canvases: an explicit selector (a `nostr:naddr`/coordinate, a
/// `notebook:<word-id>` ref, or a literal/unique-prefix `d`), or — when omitted —
/// the sole canvas. There is no hard-coded default id (the retired
/// `store::CANVAS_ID`): with several canvases and no selector the caller must say
/// which; with none it must `seed` first. `canvases` is already scoped to one
/// author (the reader's) by the fold, so a coordinate/ref for another author's
/// canvas simply won't match.
fn resolve_canvas_id(selector: Option<&str>, canvases: &[CanvasView]) -> Result<String> {
    let Some(sel) = selector else {
        return match canvases {
            [only] => Ok(only.id.clone()),
            [] => Err("no canvas yet — run `notebook seed` to create one".into()),
            _ => Err(
                "several canvases — pass --canvas <ref|d> to pick one (see `notebook show`)".into(),
            ),
        };
    };

    // A `nostr:naddr` / bare coordinate naming a canvas.
    if let Some((a, d)) = event::parse_canvas_naddr(sel) {
        return canvases
            .iter()
            .find(|c| c.id == d && c.author == *a.bytes())
            .map(|c| c.id.clone())
            .ok_or_else(|| format!("no canvas matching '{sel}'").into());
    }

    // A `notebook:<word-id>` ref — must resolve to a *canvas* (a note/node ref given
    // to `--canvas` fails rather than falling through to a `d` prefix).
    if let Some(word_id) = wordid::parse_ref(sel) {
        return match event::resolve_ref(word_id, canvases, &[]) {
            Some(NotebookTarget::Canvas { d, .. }) => Ok(d),
            _ => Err(format!("no canvas matching '{sel}'").into()),
        };
    }

    // A literal or unique-prefix `d`.
    let mut hits = canvases.iter().filter(|c| c.id.starts_with(sel));
    match (hits.next(), hits.next()) {
        (Some(c), None) => Ok(c.id.clone()),
        (Some(_), Some(_)) => Err(format!("ambiguous canvas prefix '{sel}'").into()),
        _ => Err(format!("no canvas matching '{sel}'").into()),
    }
}

// ---------------------------------------------------------------------------
// output
// ---------------------------------------------------------------------------

/// A node's human-friendly reference for display/addressing: `notebook:<word-id>`,
/// e.g. `notebook:maple-river-canyon`, muted. This is what a human quotes; it
/// resolves back via [`find_node`].
fn word_ref(id: &NoteId) -> String {
    nostrdb_net::relay::sync::dim(&wordid::node_ref(id.bytes()))
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
        print_node_line(n);
    }
    if !view.edges.is_empty() {
        println!("\nEdges ({})", view.edges.len());
        for e in &view.edges {
            println!(
                "  {} → {}  {}",
                word_ref(&e.from),
                word_ref(&e.to),
                nostrdb_net::relay::sync::dim(&e.id),
            );
        }
    }
    if !view.pending.is_empty() {
        println!(
            "\nPending ({}) — proposals on a closed canvas",
            view.pending.len()
        );
        for n in &view.pending {
            print_node_line(n);
        }
    }
}

fn print_node_line(n: &NodeView) {
    let geo = nostrdb_net::relay::sync::dim(&format!(
        "({},{} {}×{})",
        n.geo.x, n.geo.y, n.geo.w, n.geo.h
    ));
    println!(
        "  {}  {}  {}",
        one_line(&n.content.text),
        geo,
        word_ref(&n.id),
    );
}

/// Render already-resolved `show` targets, dispatching each on its type. A single
/// target prints in its natural shape — a whole canvas, a note's raw markdown body
/// (so `notebook show <note> > note.md` round-trips), or a node line, the machine
/// form being a lone object. Several targets print in sequence (JSON as one array),
/// so a mixed selection stays one navigable document.
fn show_targets(
    targets: &[NotebookTarget],
    canvases: &[CanvasView],
    notes: &[event::LongformNote],
    as_json: bool,
) -> Result<()> {
    if as_json {
        let vals = targets
            .iter()
            .map(|t| target_json(t, canvases, notes))
            .collect::<Result<Vec<_>>>()?;
        let rendered = match vals.as_slice() {
            [one] => serde_json::to_string_pretty(one),
            _ => serde_json::to_string_pretty(&vals),
        };
        println!("{}", rendered.unwrap_or_else(|_| "null".into()));
        return Ok(());
    }

    for target in targets {
        match target {
            NotebookTarget::Canvas { author, d } => {
                print_canvas(find_target_canvas(canvases, author, d)?, false);
            }
            NotebookTarget::Note { author, d } => {
                print_vault_bodies(&[find_target_note(notes, author, d)?], false);
            }
            NotebookTarget::Node { id } => print_node_line(find_target_node(canvases, *id)?),
        }
    }
    Ok(())
}

/// The machine form of a resolved target: a canvas / note / node JSON object,
/// dispatched on its type.
fn target_json(
    target: &NotebookTarget,
    canvases: &[CanvasView],
    notes: &[event::LongformNote],
) -> Result<serde_json::Value> {
    Ok(match target {
        NotebookTarget::Canvas { author, d } => {
            event::canvas_json(find_target_canvas(canvases, author, d)?)
        }
        NotebookTarget::Note { author, d } => {
            event::longform_json(find_target_note(notes, author, d)?)
        }
        NotebookTarget::Node { id } => event::node_json(find_target_node(canvases, *id)?),
    })
}

// The three lookups below re-find a resolved target's concrete view in the same
// slices `resolve_target` matched it against, so they never realistically miss; the
// graceful error is a belt-and-braces guard rather than a reachable path.

fn find_target_canvas<'a>(
    canvases: &'a [CanvasView],
    author: &Pubkey,
    d: &str,
) -> Result<&'a CanvasView> {
    event::find_canvas(canvases, author, d)
        .ok_or_else(|| format!("canvas '{d}' went missing during render").into())
}

fn find_target_note<'a>(
    notes: &'a [event::LongformNote],
    author: &Pubkey,
    d: &str,
) -> Result<&'a event::LongformNote> {
    notes
        .iter()
        .find(|n| n.d == d && n.author == *author.bytes())
        .ok_or_else(|| format!("note '{d}' went missing during render").into())
}

fn find_target_node(canvases: &[CanvasView], id: NoteId) -> Result<&NodeView> {
    canvases
        .iter()
        .flat_map(all_nodes)
        .find(|n| n.id == id)
        .ok_or_else(|| format!("node '{}' went missing during render", id.hex()).into())
}

// ---------------------------------------------------------------------------
// vault (longform notes)
// ---------------------------------------------------------------------------

/// List `author`'s longform vault, or print the markdown bodies of the notes named
/// by `sels`. A purely local read of the PNS-unwrapped longform store — see the
/// short-circuit in [`run`] for why longform never syncs over the relay.
fn run_vault(ndb: &Ndb, author: &Pubkey, sels: &[String], as_json: bool) -> Result<()> {
    let txn = Transaction::new(ndb).map_err(|e| format!("opening a db transaction: {e}"))?;
    let notes = event::list_longform(ndb, &txn, author);

    if sels.is_empty() {
        print_vault(&notes, as_json);
        return Ok(());
    }

    // Resolve every selector first so one bad ref fails the whole command rather
    // than printing a partial result (mirrors `print_nodes`).
    let picked: Vec<&event::LongformNote> = sels
        .iter()
        .map(|sel| find_longform(&notes, sel))
        .collect::<Result<_>>()?;
    print_vault_bodies(&picked, as_json);
    Ok(())
}

/// Resolve a vault-note selector against the current list, accepting (in order): a
/// `nostr:naddr…`/coordinate reference; a `notebook:<word-id>` reference (matched
/// against each note's canonical ref); or a full `d` or unique `d` prefix.
fn find_longform<'a>(
    notes: &'a [event::LongformNote],
    sel: &str,
) -> Result<&'a event::LongformNote> {
    // A nostr:naddr / bare coordinate → match by (author, d).
    if let Some((author, d)) = event::parse_longform_naddr(sel) {
        return notes
            .iter()
            .find(|n| n.author == *author.bytes() && n.d == d)
            .ok_or_else(|| format!("no vault note matching '{sel}'").into());
    }

    // A `notebook:<word-id>` reference → the note whose canonical ref equals it.
    // Ref-shaped but unmatched fails here rather than falling through to a `d`
    // prefix, so a mistyped word-id can't silently resolve to some other note.
    if wordid::parse_ref(sel).is_some() {
        return notes
            .iter()
            .find(|n| event::longform_ref(&Pubkey::new(n.author), &n.d) == sel)
            .ok_or_else(|| format!("no vault note matching '{sel}'").into());
    }

    // Otherwise a full `d` or a unique `d` prefix.
    let mut hits = notes.iter().filter(|n| n.d.starts_with(sel));
    match (hits.next(), hits.next()) {
        (Some(n), None) => Ok(n),
        (Some(_), Some(_)) => Err(format!("ambiguous vault selector '{sel}'").into()),
        _ => Err(format!("no vault note matching '{sel}'").into()),
    }
}

/// A vault note's one-line title for listings, falling back when the note has no
/// title tag yet (a freshly-created draft).
fn note_title(n: &event::LongformNote) -> &str {
    let t = n.title.trim();
    if t.is_empty() { "(untitled)" } else { t }
}

/// Print the vault list: one row per note (title, edited-age, ref), with the
/// summary on a muted second line when present.
fn print_vault(notes: &[event::LongformNote], as_json: bool) {
    if as_json {
        let out: Vec<_> = notes.iter().map(event::longform_json).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&out).unwrap_or_else(|_| "null".into())
        );
        return;
    }

    let now = now_secs();
    println!(
        "Vault ({} note{})",
        notes.len(),
        if notes.len() == 1 { "" } else { "s" }
    );
    for n in notes {
        let author = Pubkey::new(n.author);
        let when = nostrdb_net::relay::sync::dim(&format!("edited {}", ago(n.created_at, now)));
        let reference = nostrdb_net::relay::sync::dim(&event::longform_ref(&author, &n.d));
        println!("  {}  {}  {}", note_title(n), when, reference);
        if let Some(summary) = n
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            println!("    {}", nostrdb_net::relay::sync::dim(&one_line(summary)));
        }
    }
}

/// Print the markdown bodies of the resolved notes. A single note prints its raw
/// body only, so `notebook vault <note> > note.md` round-trips; more than one is
/// separated by a muted ref header so the concatenation stays navigable.
fn print_vault_bodies(notes: &[&event::LongformNote], as_json: bool) {
    if as_json {
        let out: Vec<_> = notes.iter().map(|n| event::longform_json(n)).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&out).unwrap_or_else(|_| "null".into())
        );
        return;
    }

    for (i, n) in notes.iter().enumerate() {
        if notes.len() > 1 {
            if i > 0 {
                println!();
            }
            let reference = event::longform_ref(&Pubkey::new(n.author), &n.d);
            println!("{}", nostrdb_net::relay::sync::dim(&reference));
        }
        print!("{}", n.content);
        // Ensure a trailing newline so the shell prompt isn't glued to the body.
        if !n.content.ends_with('\n') {
            println!();
        }
    }
}

// ---------------------------------------------------------------------------
// vault documents (unified note + canvas listing)
// ---------------------------------------------------------------------------

/// A vault document's kind as a short label for listings and `--json`.
fn doc_kind_label(kind: VaultDocKind) -> &'static str {
    match kind {
        VaultDocKind::Note => "note",
        VaultDocKind::Canvas => "canvas",
    }
}

/// A vault document's one-line title for listings, with the "(untitled)" fallback
/// the note listing already uses for a fresh draft.
fn doc_title(title: &str) -> &str {
    let t = title.trim();
    if t.is_empty() { "(untitled)" } else { t }
}

/// A vault document's human `notebook:<word-id>` reference, dispatched on its kind:
/// a note's coordinate ref ([`event::longform_ref`]) or a canvas's
/// ([`event::canvas_ref`]).
fn doc_ref(doc: &VaultDoc) -> String {
    match doc.kind {
        VaultDocKind::Note => event::longform_ref(&doc.author, &doc.d),
        VaultDocKind::Canvas => event::canvas_ref(&doc.author, &doc.d),
    }
}

/// Print the whole vault: one typed row per document (kind, title, edited-age, ref),
/// newest-edited first. The mixed-document counterpart of [`print_vault`]; a canvas
/// row has no summary subtitle (only notes carry one, and [`VaultDoc`] doesn't
/// project it — a note's body/summary is reached by addressing it: `show <ref>`).
fn print_vault_docs(docs: &[VaultDoc], as_json: bool) {
    if as_json {
        let out: Vec<_> = docs.iter().map(vault_doc_json).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&out).unwrap_or_else(|_| "null".into())
        );
        return;
    }

    let now = now_secs();
    println!(
        "Vault ({} item{})",
        docs.len(),
        if docs.len() == 1 { "" } else { "s" }
    );
    for doc in docs {
        let kind = nostrdb_net::relay::sync::dim(&format!("[{}]", doc_kind_label(doc.kind)));
        let when = nostrdb_net::relay::sync::dim(&format!("edited {}", ago(doc.edited_at, now)));
        let reference = nostrdb_net::relay::sync::dim(&doc_ref(doc));
        println!(
            "  {}  {}  {}  {}",
            kind,
            doc_title(&doc.title),
            when,
            reference
        );
    }
}

/// The machine form of a vault row: the typed projection plus its human `ref`, so a
/// consumer can address the document without re-deriving it (mirrors
/// [`event::longform_json`]).
fn vault_doc_json(doc: &VaultDoc) -> serde_json::Value {
    serde_json::json!({
        "kind": doc_kind_label(doc.kind),
        "author": doc.author.hex(),
        "d": doc.d,
        "title": doc.title,
        "edited_at": doc.edited_at,
        "ref": doc_ref(doc),
    })
}

/// Wall-clock seconds since the Unix epoch (0 if the clock somehow predates it).
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A compact relative age ("just now", "5m ago", "2h ago", "3d ago") for a note's
/// last edit. Deliberately dependency-light: the CLI isn't a localized surface,
/// unlike the in-app vault sidebar's `edited_subtitle`.
fn ago(then: u64, now: u64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        0..=1 => "just now".to_string(),
        2..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
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
        // (and the `notedeck_notebook` store/event layer) speaks `enostr::Pubkey`.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn note(author: [u8; 32], d: &str, title: &str) -> event::LongformNote {
        event::LongformNote {
            author,
            d: d.to_string(),
            title: title.to_string(),
            summary: None,
            content: String::new(),
            published_at: None,
            hashtags: Vec::new(),
            created_at: 0,
            deleted: false,
        }
    }

    #[test]
    fn ago_buckets_by_magnitude() {
        assert_eq!(ago(100, 100), "just now");
        assert_eq!(ago(100, 101), "just now");
        assert_eq!(ago(0, 30), "30s ago");
        assert_eq!(ago(0, 120), "2m ago");
        assert_eq!(ago(0, 7200), "2h ago");
        assert_eq!(ago(0, 3 * 86_400), "3d ago");
        // A clock that ran backwards clamps to "just now" rather than underflowing.
        assert_eq!(ago(500, 100), "just now");
    }

    #[test]
    fn find_longform_by_prefix_ref_and_naddr() {
        let a = [7u8; 32];
        let notes = vec![note(a, "abcdef", "One"), note(a, "abff00", "Two")];

        // A unique `d` prefix resolves; an ambiguous one is rejected.
        assert_eq!(find_longform(&notes, "abcd").unwrap().d, "abcdef");
        assert!(find_longform(&notes, "ab").is_err());
        assert!(find_longform(&notes, "zz").is_err());

        // The canonical `notebook:<word-id>` ref resolves to its own note.
        let reference = event::longform_ref(&Pubkey::new(a), "abcdef");
        assert_eq!(find_longform(&notes, &reference).unwrap().d, "abcdef");

        // A nostr:naddr for the note resolves too.
        let naddr = event::longform_naddr(&Pubkey::new(a), "abff00").unwrap();
        assert_eq!(find_longform(&notes, &naddr).unwrap().d, "abff00");

        // A well-formed ref that matches no note fails rather than falling through
        // to a `d`-prefix match.
        let missing = event::longform_ref(&Pubkey::new(a), "nope");
        assert!(find_longform(&notes, &missing).is_err());
    }

    #[test]
    fn note_title_falls_back_when_blank() {
        assert_eq!(note_title(&note([0u8; 32], "d", "  Hi ")), "Hi");
        assert_eq!(note_title(&note([0u8; 32], "d", "   ")), "(untitled)");
    }

    /// A minimal folded canvas (no nodes/edges) for the resolver tests.
    fn canvas(author: [u8; 32], id: &str, title: &str) -> CanvasView {
        CanvasView {
            id: id.to_string(),
            author,
            title: title.to_string(),
            members: Vec::new(),
            open: false,
            created_at: 0,
            nodes: Vec::new(),
            edges: Vec::new(),
            pending: Vec::new(),
        }
    }

    #[test]
    fn resolve_target_dispatches_note_and_canvas() {
        let a = [7u8; 32];
        let pk = Pubkey::new(a);
        let canvases = vec![canvas(a, "c0ffee", "A Canvas")];
        let notes = vec![note(a, "abcdef", "An Article")];

        // A note's naddr and its `notebook:` ref both resolve to the note.
        let n_naddr = event::longform_naddr(&pk, "abcdef").unwrap();
        assert_eq!(
            resolve_target(&n_naddr, &canvases, &notes).unwrap(),
            NotebookTarget::Note {
                author: pk,
                d: "abcdef".to_string()
            }
        );
        let n_ref = event::longform_ref(&pk, "abcdef");
        assert!(matches!(
            resolve_target(&n_ref, &canvases, &notes).unwrap(),
            NotebookTarget::Note { .. }
        ));

        // A canvas's naddr and its `notebook:` ref both resolve to the canvas.
        let c_naddr = event::canvas_naddr(&pk, "c0ffee").unwrap();
        assert_eq!(
            resolve_target(&c_naddr, &canvases, &notes).unwrap(),
            NotebookTarget::Canvas {
                author: pk,
                d: "c0ffee".to_string()
            }
        );
        let c_ref = event::canvas_ref(&pk, "c0ffee");
        assert!(matches!(
            resolve_target(&c_ref, &canvases, &notes).unwrap(),
            NotebookTarget::Canvas { .. }
        ));

        // A well-formed `notebook:` ref matching nothing fails rather than falling
        // through to the loose prefix arm.
        let missing = event::canvas_ref(&pk, "nope");
        assert!(resolve_target(&missing, &canvases, &notes).is_err());

        // A bare `d` prefix resolves through the fallback arm (note vs. canvas).
        assert!(matches!(
            resolve_target("abc", &canvases, &notes).unwrap(),
            NotebookTarget::Note { .. }
        ));
        assert!(matches!(
            resolve_target("c0f", &canvases, &notes).unwrap(),
            NotebookTarget::Canvas { .. }
        ));
        assert!(resolve_target("zz", &canvases, &notes).is_err());
    }

    #[test]
    fn resolve_canvas_id_infers_sole_and_requires_selection() {
        let a = [7u8; 32];
        let pk = Pubkey::new(a);

        // No selector: infer the sole canvas, but require one when there are none or
        // several (no hard-coded default id).
        assert!(resolve_canvas_id(None, &[]).is_err());
        let one = vec![canvas(a, "c0ffee", "One")];
        assert_eq!(resolve_canvas_id(None, &one).unwrap(), "c0ffee");
        let two = vec![canvas(a, "c0ffee", "One"), canvas(a, "d00d00", "Two")];
        assert!(resolve_canvas_id(None, &two).is_err());

        // A selector picks a specific canvas: a `notebook:` ref, a naddr, or a `d`
        // prefix. A note ref (or an unmatched one) given to --canvas fails.
        let c_ref = event::canvas_ref(&pk, "d00d00");
        assert_eq!(resolve_canvas_id(Some(&c_ref), &two).unwrap(), "d00d00");
        let c_naddr = event::canvas_naddr(&pk, "c0ffee").unwrap();
        assert_eq!(resolve_canvas_id(Some(&c_naddr), &two).unwrap(), "c0ffee");
        assert_eq!(resolve_canvas_id(Some("d00"), &two).unwrap(), "d00d00");
        let note_ref = event::longform_ref(&pk, "c0ffee");
        assert!(resolve_canvas_id(Some(&note_ref), &two).is_err());
    }
}
