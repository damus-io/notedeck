//! `Pubkey` now lives in `nostrdb_net`; enostr re-exports it during the
//! nostrdb-net convergence (phase 4) so the whole workspace shares one type
//! and consumers can repoint `enostr::Pubkey` → `nostrdb_net::Pubkey` at their
//! own pace with no conversion glue. This shim goes away when enostr is deleted.
pub use nostrdb_net::{Pubkey, PubkeyRef};
