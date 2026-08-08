//! The `query_replaceable` helpers now live in `nostrdb_net`; enostr re-exports
//! them during the nostrdb-net convergence (phase 4) so every consumer shares one
//! implementation. This shim goes away when enostr is deleted.
pub use nostrdb_net::{query_replaceable, query_replaceable_filtered};
