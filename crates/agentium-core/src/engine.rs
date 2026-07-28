//! The standalone agentium engine: an owned [`Ndb`] plus (in later slices) a
//! NIP-77 relay-sync loop that implements the [`Transport`](crate::Transport)
//! boundary, so the same engine drops into a desktop host or a standalone/iOS
//! process with no host application context.
//!
//! # Database ownership
//!
//! The engine must be agnostic to whether it *owns* or *shares* its nostrdb.
//! [`Ndb`] is `#[derive(Clone)]` over an `Arc`-backed handle: a clone is a cheap
//! reference to the *same* database, and the database is torn down only when the
//! last clone drops. So the engine holds an `Ndb` **by value** — no lifetime
//! parameter, no borrowed/owned split — and the two constructors converge on
//! that one stored type:
//!
//! - [`Engine::open`] — standalone/iOS: the engine creates its own database and
//!   holds it.
//! - [`Engine::with_ndb`] — an embedding host passes a *clone* of its own
//!   database; both point at the same db, and the engine dropping won't tear
//!   down the host's db because the host still holds a clone.
//!
//! Holding by value is also *required*, not merely convenient: the reconcile
//! loop runs in a `tokio::spawn`ed task and needs a `'static` handle, so a
//! borrowed `&Ndb` couldn't move into the task — a cloned `Ndb` is exactly
//! right.

use nostrdb::Ndb;

/// The standalone agentium engine.
///
/// This slice establishes db ownership (see the module docs); the relay-sync
/// loop and [`Transport`](crate::Transport) implementation land in subsequent
/// slices, reusing [`nostrdb_net`]'s NIP-77 sync and websocket relay.
pub struct Engine {
    ndb: Ndb,
}

impl Engine {
    /// Open a standalone engine over its own nostrdb at `path` (created if
    /// absent). Use this on a host that has no existing database of its own.
    pub fn open(path: &str) -> Result<Self, nostrdb::Error> {
        let ndb = Ndb::new(path, &nostrdb::Config::new())?;
        Ok(Self { ndb })
    }

    /// Build an engine over an existing database, taking a cheap [`Ndb`] clone.
    /// Use this on an embedding host: pass `host_ndb.clone()` so the engine and
    /// host share one database and neither's drop tears it down while the other
    /// still holds a handle.
    pub fn with_ndb(ndb: Ndb) -> Self {
        Self { ndb }
    }

    /// The engine's database handle.
    pub fn ndb(&self) -> &Ndb {
        &self.ndb
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostrdb::Transaction;
    use tempfile::TempDir;

    #[test]
    fn open_creates_a_usable_db() {
        let tmp = TempDir::new().expect("tmp dir");
        let engine = Engine::open(tmp.path().to_str().expect("path")).expect("open");
        // A fresh db opens a transaction without error.
        Transaction::new(engine.ndb()).expect("txn");
    }

    #[test]
    fn with_ndb_shares_one_database() {
        let tmp = TempDir::new().expect("tmp dir");
        let host = Ndb::new(tmp.path().to_str().expect("path"), &nostrdb::Config::new())
            .expect("host ndb");

        // The host hands the engine a clone; dropping the engine must not tear
        // down the shared db, so the host can still use it afterwards.
        let engine = Engine::with_ndb(host.clone());
        drop(engine);
        Transaction::new(&host).expect("host db still usable after engine drop");
    }
}
