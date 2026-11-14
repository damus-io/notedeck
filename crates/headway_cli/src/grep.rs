//! `headway grep` — search card text across a board (or every board).
//!
//! Modelled on `agentium grep` (crates/agentium_cli/src/grep.rs): each matching
//! line printed under its owner's full, pasteable ref, smart-case by default,
//! paged and colored the same way, and `--json` grouped per owner rather than
//! one flat row per hit. The search and both renderings are
//! [`headway::grep`]'s, which the app's `headway_grep` tool shares, so `nd
//! headway grep` prints what this does.

use std::io::IsTerminal;

use cli_term::{ColorWhen, PagerMode};
use nostrdb_net::relay::sync::Result;
use regex::Regex;

use headway::event::BoardView;
use headway::grep::{self, NO_MATCHES};

pub(crate) use headway::grep::PAGER_VAR;

/// `headway grep <pattern>` — print every line of card text in `boards` that
/// `pattern` matches, grouped under each card's `headway:<board>/<word-id>` ref,
/// each line with how long ago its text was written, most recent card first
/// (see [`grep::search`]). `subtree` (`--in <card>`) keeps only that card and
/// its descendants.
///
/// The text output goes through a pager when stdout is a terminal, like
/// `git grep` and `agentium grep` (`--pager`/`--no-pager` force it either
/// way), and is colored for the effective sink unless `--color` says otherwise
/// — `--color always` keeps the highlight when piping into your own `less -R`.
pub(crate) fn cmd_grep(
    boards: &[BoardView],
    subtree: Option<[u8; 32]>,
    pattern: &Regex,
    out: GrepOutput,
) -> Result<()> {
    let stdout_tty = std::io::stdout().is_terminal();
    let use_pager = !out.json && out.pager.enabled(stdout_tty);
    let color = out.color.enabled(stdout_tty || use_pager);

    let hits = grep::search(boards, subtree, pattern);
    if out.json {
        println!("{}", serde_json::to_string_pretty(&grep::hits_json(&hits))?);
        return Ok(());
    }
    let output = grep::text(&hits, pattern, color);
    if output.is_empty() {
        println!("{NO_MATCHES}");
        return Ok(());
    }
    cli_term::emit(&output, use_pager, PAGER_VAR);
    Ok(())
}

/// How `grep`'s results are written: `--json`, or text under the `--color`
/// and `--pager`/`--no-pager` modes. JSON is never paged or colored.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GrepOutput {
    pub(crate) json: bool,
    pub(crate) color: ColorWhen,
    pub(crate) pager: PagerMode,
}
