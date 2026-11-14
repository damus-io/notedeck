//! `headway diff`: find the commit a card's review record names on this host —
//! fetching it from the host that recorded it when it isn't here — and print
//! it `git show`-style. The no-UI exercise of [`headway::git::resolve`], over
//! [`headway::diff`], which the app's `headway_diff` tool shares.
//!
//! The resolution line (where the commit was found) goes to stderr and the
//! commit to stdout, so `headway diff <card> | git apply` works. `--note`
//! prints one of the commit's explain notes instead (see
//! [`headway::git::fetch_notes`]).

use std::path::Path;

use headway::diff::DiffPlan;
use headway::event::BoardView;
use headway::git::NoteKind;

use nostrdb_net::relay::sync::Result;

/// Resolve card `sel`'s newest review record (or the one whose commit starts
/// with `record`) to a commit and print it, or with `note` that explain note of
/// it. A card with no records falls back to the `Headway:` trailer search in
/// this checkout. `cache_root` is where the headway-owned bare cache repos
/// live.
pub(crate) fn print_diff(
    view: &BoardView,
    sel: &str,
    record: Option<&str>,
    note: Option<NoteKind>,
    cache_root: &Path,
) -> Result<()> {
    let plan = DiffPlan::new(view, sel, record, note, Some(Path::new(".")))?;
    let out = plan.run(cache_root)?;
    eprintln!("{}", nostrdb_net::relay::sync::dim(&out.notice()));
    print!("{}", out.text());
    Ok(())
}
