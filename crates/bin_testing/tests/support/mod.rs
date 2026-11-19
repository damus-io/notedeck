//! Helpers shared by bin_testing's test targets. Each target that needs one
//! pulls this in with `mod support;`; cargo doesn't build `tests/support/`
//! as a target of its own.

pub mod graph;
