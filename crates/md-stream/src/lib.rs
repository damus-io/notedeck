//! Incremental markdown parser for streaming LLM output.
//!
//! Designed for chat interfaces where markdown arrives token-by-token
//! and needs to be rendered progressively.

mod element;
mod inline;
mod parser;
mod partial;

pub use element::{CodeBlock, InlineElement, InlineStyle, ListItem, MdElement};
pub use inline::{parse_inline, InlineState};
pub use parser::StreamParser;
pub use partial::{LinkState, Partial, PartialKind};

#[cfg(test)]
mod tests;
