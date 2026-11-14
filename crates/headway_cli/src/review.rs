//! `headway review`: gather a review record for a commit from git, the host
//! and the environment, so an agent's done step is one line. The gathering
//! is [`headway::review`]'s, shared with the app's `headway_review` tool.
//!
//! Gathering runs at parse time (like `--desc-file`'s read), before any relay
//! work, so a bad rev or a directory outside a repo fails fast rather than after
//! a full sync.

use headway::event::{ReviewFields, ReviewSource};
pub(crate) use headway::review::{ReviewFlags, SourceFlag};

use nostrdb_net::relay::sync::Result;

/// Read the source files `flags` name (see [`headway::review::read_sources`]).
pub(crate) fn read_sources(flags: &[SourceFlag]) -> Result<Vec<ReviewSource>> {
    Ok(headway::review::read_sources(flags)?)
}

/// Resolve `flags` into the record `review` writes (see
/// [`headway::review::gather`]), falling back to `$AGENTIUM_SESSION` for the
/// session when `--agentium` isn't given.
pub(crate) fn gather(mut flags: ReviewFlags) -> Result<ReviewFields> {
    flags.default_agentium = std::env::var("AGENTIUM_SESSION").ok();
    Ok(headway::review::gather(flags)?)
}

/// Print what a `review` recorded, one field per line, so the transcript of the
/// agent that ran it shows exactly what was captured.
pub(crate) fn print_recorded(review: &ReviewFields, sources: &[ReviewSource]) {
    print!("{}", headway::review::recorded_text(review, sources));
}
