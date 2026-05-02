//! Iris-compatible double-ratchet messaging support.

mod compat;
mod service;
mod util;
mod worker;

pub(crate) use service::RatchetService;
