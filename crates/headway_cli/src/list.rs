//! `headway list` — the cards a query keeps, one row each, across every board.
//!
//! The query is [`headway::query`]'s, the same one the GUI board filter reads,
//! so `p:high label:ui is:done` means one thing everywhere. The listing, the
//! views and both renderings are [`headway::list`]'s, which the app's
//! `headway_list` tool shares, so `nd headway list` prints what this does.
//! Output follows `grep`: paged and colored on a terminal, plain when piped,
//! `--json` for scripts.

use std::io::IsTerminal;

use cli_term::{ColorWhen, PagerMode};
use nostrdb::Ndb;
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync::Result;

use headway::event::BoardView;
use headway::list::{self, Listing, NO_CARDS};
use headway::query::{ListQuery, ViewSpec};
use headway::views::SavedView;

use crate::grep::PAGER_VAR;

pub(crate) use headway::list::compose_view;

/// How `list`'s rows are written: `--json`, or text under the `--color` and
/// `--pager`/`--no-pager` modes, at most `limit` of them (`-n`). What each
/// row shows, how rows are gathered and their order are the query's.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ListOutput {
    pub(crate) json: bool,
    pub(crate) color: ColorWhen,
    pub(crate) pager: PagerMode,
    pub(crate) limit: Option<usize>,
}

/// `headway list [query...]` — print every live card on `boards` that `query`
/// keeps, one row per card (see [`Listing::new`]).
pub(crate) fn cmd_list(boards: &[BoardView], query: &ListQuery, out: ListOutput) -> Result<()> {
    let listing = Listing::new(boards, query, out.limit);
    if out.json {
        println!("{}", serde_json::to_string_pretty(&listing.json())?);
        return Ok(());
    }
    let stdout_tty = std::io::stdout().is_terminal();
    let use_pager = out.pager.enabled(stdout_tty);
    let color = out.color.enabled(stdout_tty || use_pager);
    let output = listing.text(color);
    if output.is_empty() {
        println!("{NO_CARDS}");
        return Ok(());
    }
    cli_term::emit(&output, use_pager, PAGER_VAR);
    Ok(())
}

/// `headway list --save-view <name>`: save `spec` as the signer's (`me`) view
/// `name` (see [`list::save_named_view`]), and say so.
pub(crate) fn save_named_view(
    ndb: &Ndb,
    me: &Pubkey,
    secret: &[u8; 32],
    saved: &[SavedView],
    name: &str,
    spec: ViewSpec,
    json: bool,
) -> Result<()> {
    let view = list::save_named_view(ndb, me, secret, saved, name, spec)?;
    if json {
        let value = list::view_json(&view.spec, Some(&view.slug));
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("{}", list::saved_view_line(&view));
    }
    Ok(())
}

/// `headway list --delete-view <name>`: delete the signer's (`me`) saved view
/// `name`, and say so.
pub(crate) fn delete_named_view(
    ndb: &Ndb,
    me: &Pubkey,
    secret: &[u8; 32],
    saved: &[SavedView],
    name: &str,
) -> Result<()> {
    let view = list::delete_named_view(ndb, me, secret, saved, name)?;
    println!("{}", list::deleted_view_line(&view));
    Ok(())
}

/// `headway list --views`: the built-in views, then `saved`, as text or JSON
/// (see [`list::views_text`]).
pub(crate) fn print_views(saved: &[SavedView], layout: &ViewSpec, json: bool) -> Result<()> {
    if json {
        let value = list::views_json(saved, layout);
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        print!("{}", list::views_text(saved, layout));
    }
    Ok(())
}
