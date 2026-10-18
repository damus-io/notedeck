//! `cashflow goals`: the financial goals on Headway cards (see
//! [`cashflow::goals`]), each with its card's full `headway:<board>/<word-id>`
//! ref.
//!
//! The cards come from the `headway` CLI's own nostrdb cache, not this CLI's:
//! that is where `headway` reconciles boards with the relay (plaintext
//! events, key-share gift-wraps and sealed envelopes), and this command only
//! reads. Pulling boards here would mean a second copy of that sync.

use cashflow::board_progress;
use cashflow::goals::{self, Goal, GoalSpec, Scope, Target};
use cashflow::{CashflowDir, Config};
use headway::boards::{Identity, Roster};
use headway::event::BoardView;
use headway::teams::RootRegistry;
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync;
use serde_json::{Value, json};

use crate::args::GoalsArgs;
use crate::vault_cmd;

/// The `headway` CLI's cache directory under the platform data dir (its
/// `APP`), which `--headway-db` overrides.
const HEADWAY_APP: &str = "headway-cli";

/// The goal boards `args` names, folded.
pub struct GoalBoards {
    /// Whose boards they are.
    pub me: Pubkey,
    /// The slugs looked on.
    pub boards: Vec<String>,
    /// Each board of those slugs, owned or shared.
    pub views: Vec<BoardView>,
}

impl GoalBoards {
    /// Fold `--board`'s boards (else the config's) from the headway cache.
    pub fn load(dir: &CashflowDir, args: &GoalsArgs) -> Result<Self, String> {
        let config = Config::load(dir).map_err(|e| e.to_string())?;
        let boards = if args.boards.is_empty() {
            config.goal_boards()
        } else {
            args.boards.clone()
        };
        let me = match &args.author {
            Some(author) => Pubkey::parse(author).map_err(|e| format!("--author: {e}"))?,
            None => vault_cmd::account_pubkey(dir)
                .map_err(|e| format!("{e} (or pass --author <npub|hex>)"))?,
        };
        let db = args.headway_db.as_ref().map(|p| p.to_string_lossy());
        let ndb = sync::open_ndb(db.as_deref(), HEADWAY_APP).map_err(|e| e.to_string())?;
        let mut registry = RootRegistry::default();
        let roster = Roster::load(&ndb, Identity::own(me), &mut registry);
        let views = board_progress::named_boards(&ndb, &roster, &boards);
        Ok(GoalBoards { me, boards, views })
    }

    /// The goals on the boards, in board, column and card order.
    pub fn goals(&self) -> Vec<Goal> {
        self.views.iter().flat_map(goals::board_goals).collect()
    }

    /// The board `goal` is on.
    pub fn view_of(&self, goal: &Goal) -> Option<&BoardView> {
        self.views
            .iter()
            .find(|v| v.id == goal.board && v.author == goal.owner)
    }
}

/// `cashflow goals`: list the goals on `--board`'s boards (else the config's).
pub fn ls(dir: &CashflowDir, args: &GoalsArgs) -> Result<(), String> {
    let GoalBoards { me, boards, views } = GoalBoards::load(dir, args)?;
    let found: Vec<Goal> = views.iter().flat_map(goals::board_goals).collect();

    if args.json {
        let rows: Vec<Value> = found.iter().map(goal_json).collect();
        println!("{}", Value::Array(rows));
        return Ok(());
    }
    if found.is_empty() {
        eprintln!(
            "no goals on {} (label a card `{}` and give it a ```{} block; see `cashflow goals --help`)",
            boards.join(", "),
            goals::GOAL_LABEL,
            goals::FENCE_INFO,
        );
        return Ok(());
    }
    for goal in &found {
        print_goal(goal, &me);
    }
    Ok(())
}

/// Two lines: the card, then its goal or the error in its block.
fn print_goal(goal: &Goal, me: &Pubkey) {
    let shared = if &goal.owner == me.bytes() {
        String::new()
    } else {
        format!(", shared by {}", hex(&goal.owner))
    };
    println!(
        "{}  {}  ({}{shared})",
        goal.card_ref, goal.title, goal.column
    );
    match &goal.spec {
        Ok(spec) => println!("    {spec}"),
        Err(err) => println!("    error: {err}"),
    }
}

/// A goal as JSON: the card, and either `goal` or `error`.
fn goal_json(goal: &Goal) -> Value {
    let mut row = json!({
        "card": goal.card_ref,
        "board": goal.board,
        "owner": hex(&goal.owner),
        "title": goal.title,
        "column": goal.column,
        "done": goal.done,
    });
    match &goal.spec {
        Ok(spec) => row["goal"] = spec_json(spec),
        Err(err) => {
            row["error"] = json!({
                "line": err.line,
                "message": err.kind.to_string(),
            })
        }
    }
    row
}

/// A spec's fields as JSON, the target as written.
fn spec_json(spec: &GoalSpec) -> Value {
    let (account, project) = match &spec.scope {
        Some(Scope::Account(account)) => (Some(account), None),
        Some(Scope::Project(project)) => (None, Some(project)),
        None => (None, None),
    };
    let commodity = match &spec.target {
        Target::Amount { commodity, .. } => commodity.as_deref(),
        Target::Percent(_) => None,
    };
    json!({
        "metric": spec.metric.name(),
        "direction": match spec.direction() {
            goals::Direction::AtLeast => "at_least",
            goals::Direction::AtMost => "at_most",
        },
        "account": account,
        "project": project,
        "target": spec.target.to_string(),
        "percent": matches!(spec.target, Target::Percent(_)),
        "commodity": commodity,
        "per": spec.per.name(),
        "by": spec.by.map(|by| by.to_string()),
        "summary": spec.to_string(),
    })
}

/// A 32-byte key as lower-case hex.
fn hex(bytes: &[u8; 32]) -> String {
    Pubkey::new(*bytes).hex()
}
