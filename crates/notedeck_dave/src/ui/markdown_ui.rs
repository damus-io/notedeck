//! Markdown rendering for assistant messages.
//!
//! The implementation now lives in `notedeck_ui::markdown` so other crates can
//! reuse it; this module re-exports it for the existing dave call sites.

pub use notedeck_ui::markdown::*;
