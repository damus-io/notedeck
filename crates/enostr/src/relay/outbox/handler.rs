use hashbrown::HashSet;
use nostrdb::{Filter, Note};

use crate::relay::outbox::OutboxPool;
use crate::relay::{NormRelayUrl, OutboxSubId, RelayId, RelayUrlPkgs};
use crate::{relay::outbox::OutboxSession, Wakeup};

/// OutboxSessionHandler is the RAII wrapper apps use to stage subscription
/// updates; dropping it flushes the recorded operations into the OutboxPool.
pub struct OutboxSessionHandler<'a, W>
where
    W: Wakeup,
{
    pub outbox: &'a mut OutboxPool,
    pub(crate) session: OutboxSession,
    pub(crate) wakeup: W,
}

impl<'a, W> Drop for OutboxSessionHandler<'a, W>
where
    W: Wakeup,
{
    fn drop(&mut self) {
        let session = std::mem::take(&mut self.session);
        self.outbox.ingest_session(session, &self.wakeup);
    }
}

impl<'a, W> OutboxSessionHandler<'a, W>
where
    W: Wakeup,
{
    pub fn new(outbox: &'a mut OutboxPool, wakeup: W) -> Self {
        Self {
            outbox,
            session: OutboxSession::default(),
            wakeup,
        }
    }

    pub fn subscribe(&mut self, filters: Vec<Filter>, urls: RelayUrlPkgs) -> OutboxSubId {
        let new_id = self.outbox.registry.next();
        self.session.subscribe(new_id, filters, urls);
        new_id
    }

    pub fn oneshot(&mut self, filters: Vec<Filter>, urls: RelayUrlPkgs) {
        let new_id = self.outbox.registry.next();
        self.session.oneshot(new_id, filters, urls);
    }

    pub fn modify_filters(&mut self, id: OutboxSubId, filters: Vec<Filter>) {
        self.session.new_filters(id, filters);
    }

    pub fn modify_relays(&mut self, id: OutboxSubId, relays: HashSet<NormRelayUrl>) {
        self.session.new_relays(id, relays);
    }

    pub fn unsubscribe(&mut self, id: OutboxSubId) {
        self.session.unsubscribe(id);
    }

    pub fn broadcast_note(&mut self, note: &Note, relays: Vec<RelayId>) {
        self.outbox.broadcast_note(note, relays, &self.wakeup);
    }

    /// Eject the session from the handler.
    /// This is only necessary between initialization of the app and the first frame
    pub fn export(mut self) -> OutboxSession {
        let session = std::mem::take(&mut self.session);
        drop(self);
        session
    }

    pub fn import(outbox: &'a mut OutboxPool, session: OutboxSession, wakeup: W) -> Self {
        Self {
            outbox,
            session,
            wakeup,
        }
    }
}
