// `ProfileState` now lives in `nostrdb_net`; re-export it so there's a single
// profile-metadata type workspace-wide (collapse-via-reexport).
pub use nostrdb_net::ProfileState;
