//! `ClientMessage`/`EventClientMessage` now live in `nostrdb_net`; enostr
//! re-exports them during the nostrdb-net convergence (phase 4) so the whole
//! workspace shares one type and consumers can repoint at their own pace with no
//! conversion glue. This shim goes away when enostr is deleted.
pub use nostrdb_net::{ClientMessage, EventClientMessage};
