use futures::{channel::mpsc, FutureExt, StreamExt};
use std::collections::BTreeMap;
use std::fmt;
use std::{cell::RefCell, cmp::Ordering, rc::Rc};
use thiserror::Error;
use tracing::{debug, error, info, trace, warn};
use uuid::Uuid;

use enostr::{ClientMessage, Filter, PoolRelay, RelayEvent, RelayMessage, RelayPool};
use nostrdb::{self, Ndb, Subscription, SubscriptionStream};

/// The Subscription Manager
///
/// ```no_run
/// use std::error::Error;
///
/// use nostrdb::{Config, Ndb};
/// use enostr::{Filter, RelayPool};
/// use notedeck::subman::{SubConstraint, SubMan, SubSpecBuilder, SubError};
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn Error>> {
///     let mut ndb = Ndb::new("the/db/path/", &Config::new())?;
///     let mut subman = SubMan::new(ndb.clone(), RelayPool::new());
///
///     // Define a filter and build the subscription specification
///     let filter = Filter::new().kinds(vec![1, 2, 3]).build();
///     let spec = SubSpecBuilder::new()
///         .filters(vec![filter])
///         .constraint(SubConstraint::OnlyLocal)
///         .build();
///
///     // Subscribe and obtain a SubReceiver
///     let mut receiver = subman.subscribe(spec)?;
///
///     // Process incoming note keys
///     loop {
///         match receiver.next().await {
///             Ok(note_keys) => {
///                 // Process the note keys
///                 println!("Received note keys: {:?}", note_keys);
///             },
///             Err(SubError::StreamEnded) => {
///                 // Not really an error; we should clean up
///                 break;
///             },
///             Err(err) => {
///                 // Handle other errors
///                 eprintln!("Error: {:?}", err);
///                 break;
///             },
///         }
///     }
///
///     // Unsubscribe when the subscription is no longer needed
///     subman.unsubscribe_local_id(&receiver.local_id().unwrap())?;
///
///     Ok(())
/// }
/// ```
///
/// Supported Operational Modes:
///
/// | mode            | Constraints        | lcl | rmt | end mechanism       |
/// |-----------------+--------------------+-----+-----+---------------------|
/// | normal          |                    | sub | sub | client-closes       |
/// | local           | OnlyLocal          | sub |     | client-closes       |
/// | normal one-shot | OneShot            | sub | sub | EOSE -> StreamEnded |
/// | local one-shot  | OneShot+OnlyLocal  | qry |     | query,  StreamEnded |
/// | "prefetch"      | OneShot+OnlyRemote |     | sub | EOSE -> StreamEnded |

#[derive(Debug, Error)]
pub enum SubError {
    #[error("Stream ended")]
    StreamEnded,

    #[error("Internal error: {0}")]
    InternalError(String),

    #[error("nostrdb error: {0}")]
    NdbError(#[from] nostrdb::Error),
}

pub type SubResult<T> = Result<T, SubError>;

#[derive(Debug, Clone, Copy)]
pub struct LocalId(nostrdb::Subscription);

impl fmt::Display for LocalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.id())
    }
}

impl From<Subscription> for LocalId {
    fn from(subscription: Subscription) -> Self {
        LocalId(subscription)
    }
}

impl Ord for LocalId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.id().cmp(&other.0.id())
    }
}

impl PartialOrd for LocalId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for LocalId {
    fn eq(&self, other: &Self) -> bool {
        self.0.id() == other.0.id()
    }
}

impl Eq for LocalId {}

// nostr remote sub id
pub type RemoteId = String;

#[derive(Debug, Clone)]
pub enum SubConstraint {
    OneShot,                    // terminate subscription after initial query and EOSE
    OnlyLocal,                  // only query the local db, no remote subs
    OnlyRemote,                 // prefetch from remote, nothing returned
    OutboxRelays(Vec<String>),  // ensure one of these is in the active relay set
    AllowedRelays(Vec<String>), // if not empty, only use these relays
    BlockedRelays(Vec<String>), // if not empty, don't use these relays
}

#[derive(Debug, Default, Clone)]
pub struct SubSpecBuilder {
    remote_id: Option<String>,
    filters: Vec<Filter>,
    constraints: Vec<SubConstraint>,
}

impl SubSpecBuilder {
    pub fn new() -> Self {
        SubSpecBuilder::default()
    }
    pub fn remote_id(mut self, id: String) -> Self {
        self.remote_id = Some(id);
        self
    }
    pub fn filters(mut self, filters: Vec<Filter>) -> Self {
        self.filters.extend(filters);
        self
    }
    pub fn constraint(mut self, constraint: SubConstraint) -> Self {
        self.constraints.push(constraint);
        self
    }
    pub fn build(self) -> SubSpec {
        let mut outbox_relays = Vec::new();
        let mut allowed_relays = Vec::new();
        let mut blocked_relays = Vec::new();
        let mut is_oneshot = false;
        let mut is_onlylocal = false;
        let mut is_onlyremote = false;

        for constraint in self.constraints {
            match constraint {
                SubConstraint::OneShot => is_oneshot = true,
                SubConstraint::OnlyLocal => is_onlylocal = true,
                SubConstraint::OnlyRemote => is_onlyremote = true,
                SubConstraint::OutboxRelays(relays) => outbox_relays.extend(relays),
                SubConstraint::AllowedRelays(relays) => allowed_relays.extend(relays),
                SubConstraint::BlockedRelays(relays) => blocked_relays.extend(relays),
            }
        }

        let remote_id = self.remote_id.unwrap_or_else(|| Uuid::new_v4().to_string());

        SubSpec {
            remote_id,
            filters: self.filters,
            outbox_relays,
            allowed_relays,
            blocked_relays,
            is_oneshot,
            is_onlylocal,
            is_onlyremote,
        }
    }
}

#[derive(Clone)]
pub struct SubSpec {
    pub remote_id: String, // unused if is_onlylocal
    pub filters: Vec<Filter>,
    pub outbox_relays: Vec<String>,
    pub allowed_relays: Vec<String>,
    pub blocked_relays: Vec<String>,
    pub is_oneshot: bool,
    pub is_onlylocal: bool,
    pub is_onlyremote: bool,
}

impl fmt::Debug for SubSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Convert each Filter to its JSON representation.
        let filters_json: Vec<_> = self
            .filters
            .iter()
            .map(|filter| filter.json().unwrap())
            .collect();
        f.debug_struct("SubSpec")
            .field("remote_id", &self.remote_id)
            .field("filters", &filters_json)
            .field("outbox_relays", &self.outbox_relays)
            .field("allowed_relays", &self.allowed_relays)
            .field("blocked_relays", &self.blocked_relays)
            .field("is_oneshot", &self.is_oneshot)
            .field("is_onlylocal", &self.is_onlylocal)
            .field("is_onlyremote", &self.is_onlyremote)
            .finish()
    }
}

// State for a local subscription
#[derive(Debug)]
struct LocalSubState {
    local_id: LocalId,
}

// State of a remote subscription on a specific relay
#[allow(unused)]
#[derive(Default, Debug, Clone, Eq, PartialEq)]
enum RelaySubState {
    #[default]
    Pending, // before relay open or subscribed
    Syncing,       // before EOSE
    Current,       // after EOSE
    Error(String), // went wrong
    Closed,        // closed
}

// State for a remote subscription
#[derive(Debug)]
struct RemoteSubState {
    remote_id: RemoteId,
    relays: BTreeMap<String, RelaySubState>,
    tx_ended: mpsc::Sender<()>, // send StreamEnded to receiver
}

impl RemoteSubState {
    pub fn update_rss(&mut self, relay: &str, newstate: RelaySubState) {
        let rss = self.relays.get_mut(relay).expect("couldn't find relay");
        debug!(
            "RemoteSubState update_rss {} {}: {:?} -> {:?}",
            self.remote_id, relay, *rss, newstate
        );
        *rss = newstate;
    }

    // if this is a one-shot and there are no relays left syncing we are done
    pub fn consider_finished(&mut self, is_oneshot: bool) -> bool {
        let still_syncing: Vec<String> = self
            .relays
            .iter()
            .filter(|(_k, v)| **v == RelaySubState::Syncing)
            .map(|(k, _v)| k.clone())
            .collect();

        if still_syncing.is_empty() {
            if is_oneshot {
                debug!(
                    "handle_eose {}: all relays done syncing, sending one-shot ending",
                    self.remote_id
                );
                self.tx_ended.try_send(()).ok();
                true
            } else {
                debug!("handle_eose {}: all relays done syncing", self.remote_id);
                false
            }
        } else {
            debug!(
                "handle_eose {}: still_syncing {:?}",
                self.remote_id, still_syncing
            );
            false
        }
    }
}

// State of a subscription
#[allow(unused)]
#[derive(Debug)]
pub struct SubState {
    spec: SubSpec,
    local: Option<LocalSubState>,
    remote: Option<RemoteSubState>,
}
pub type SubStateRef = Rc<RefCell<SubState>>;

impl Drop for SubState {
    fn drop(&mut self) {
        debug!("dropping SubState for {}", self.spec.remote_id);
    }
}

pub struct SubMan {
    ndb: Ndb,
    pool: RelayPool,
    local: BTreeMap<LocalId, SubStateRef>,
    remote: BTreeMap<RemoteId, SubStateRef>,
}

impl SubMan {
    pub fn new(ndb: Ndb, pool: RelayPool) -> Self {
        SubMan {
            ndb,
            pool,
            local: BTreeMap::new(),
            remote: BTreeMap::new(),
        }
    }

    pub fn ndb(&self) -> Ndb {
        self.ndb.clone()
    }

    // deprecated, use SubMan directly instead
    pub fn pool(&mut self) -> &mut RelayPool {
        &mut self.pool
    }

    pub fn subscribe(&mut self, spec: SubSpec) -> SubResult<SubReceiver> {
        let (substate, subrcvr) = self.make_subscription(&spec)?;
        let state = Rc::new(RefCell::new(substate));
        if let Some(local_id) = subrcvr.local_id() {
            self.local.insert(local_id, Rc::clone(&state));
        }
        if let Some(remote_id) = subrcvr.remote_id() {
            self.remote.insert(remote_id, Rc::clone(&state));
        }
        Ok(subrcvr)
    }

    pub fn unsubscribe_local_id(&mut self, local_id: &LocalId) -> SubResult<()> {
        // find the substate and delegate to unsubscribe_substate
        let ssref = match self.local.get(local_id) {
            None => {
                return Err(SubError::InternalError(format!(
                    "substate for {} not found",
                    local_id
                )))
            }
            Some(ssref) => ssref.clone(), // clone to drop the borrow on the map
        };
        self.unsubscribe_substate(&ssref)
    }

    pub fn unsubscribe_remote_id(&mut self, remote_id: &RemoteId) -> SubResult<()> {
        // find the substate and delegate to unsubscribe_substate
        let ssref = match self.remote.get(remote_id) {
            None => {
                return Err(SubError::InternalError(format!(
                    "substate for {} not found",
                    remote_id
                )))
            }
            Some(ssref) => ssref.clone(), // clone to drop the borrow on the map
        };
        self.unsubscribe_substate(&ssref)
    }

    fn unsubscribe_substate(&mut self, ssref: &SubStateRef) -> SubResult<()> {
        let mut substate = ssref.borrow_mut();
        if let Some(&mut ref mut remotesubstate) = substate.remote.as_mut() {
            let remote_id = remotesubstate.remote_id.clone();
            // unsubscribe from all remote relays
            for (url, rss) in remotesubstate.relays.iter() {
                match rss {
                    RelaySubState::Syncing | RelaySubState::Current => {
                        SubMan::close_relay_sub(&mut self.pool, &remote_id, url);
                        // not worth marking as closed because we drop below
                    }
                    _ => {}
                }
            }
            // send StreamEnded to the receiver
            remotesubstate.tx_ended.try_send(()).ok();
            // remove from the SubMan index
            self.remote
                .remove(&remote_id)
                .expect("removed from remote index");
        }
        if let Some(localsubstate) = &substate.local {
            let local_id = &localsubstate.local_id;
            // remove from the SubMan index
            self.local
                .remove(local_id)
                .expect("removed from local index");
        }
        Ok(())
    }

    pub fn remove_substate_remote_id(&mut self, remote_id: &RemoteId) -> SubResult<()> {
        // remove from the local sub index if needed
        if let Some(ssref) = self.remote.get(remote_id) {
            let substate = ssref.borrow();
            if let Some(localsubstate) = &substate.local {
                self.local.remove(&localsubstate.local_id);
            }
        }
        // remove from the remote sub index
        match self.remote.remove(remote_id) {
            Some(_) => Ok(()),
            None => Err(SubError::InternalError(format!(
                "substate for {} not found",
                remote_id
            ))),
        }
    }

    fn make_subscription(&mut self, spec: &SubSpec) -> SubResult<(SubState, SubReceiver)> {
        // Setup local ndb subscription state
        let (maybe_localstate, localsub) = if spec.is_onlyremote {
            (None, None)
        } else {
            let subscription = self.ndb.subscribe(&spec.filters)?;
            let localstrm = subscription.stream(&self.ndb).notes_per_await(1);
            let local_id = subscription.into();
            (
                Some(LocalSubState { local_id }),
                Some(LocalSub {
                    ndb: self.ndb.clone(),
                    local_id,
                    localstrm,
                }),
            )
        };

        // Setup remote nostr relay subscription state
        let (maybe_remotestate, remotesub) = if spec.is_onlylocal {
            (None, None)
        } else {
            let (tx_ended, rx_ended) = mpsc::channel(1);
            // FIXME - need to choose specific relays!
            let relays: BTreeMap<String, RelaySubState> = self
                .pool
                .relays
                .iter()
                .map(|s| match s.url() {
                    "multicast" => (s.url().to_string(), RelaySubState::Current),
                    _ => (s.url().to_string(), RelaySubState::Syncing),
                })
                .collect();
            let remote_id = spec.remote_id.clone();
            self.pool
                .subscribe(spec.remote_id.clone(), spec.filters.clone());
            (
                Some(RemoteSubState {
                    remote_id: remote_id.clone(),
                    relays,
                    tx_ended,
                }),
                Some(RemoteSub {
                    remote_id,
                    rx_ended,
                }),
            )
        };

        Ok((
            SubState {
                spec: spec.clone(),
                local: maybe_localstate,
                remote: maybe_remotestate,
            },
            SubReceiver {
                localsub,
                remotesub,
            },
        ))
    }

    fn close_relay_sub(pool: &mut RelayPool, sid: &str, url: &str) {
        debug!("closing relay sub {} {}", sid, url);
        if let Some(relay) = pool.relays.iter_mut().find(|r| r.url() == url) {
            let cmd = ClientMessage::close(sid.to_string());
            if let Err(err) = relay.send(&cmd) {
                error!("trouble closing relay sub: {} {}: {:?}", sid, url, err);
            }
        }
    }

    pub fn process_relays<H: LegacyRelayHandler>(
        &mut self,
        legacy_relay_handler: &mut H,
    ) -> SubResult<()> {
        let wakeup = move || {
            // ignore
        };
        self.pool.keepalive_ping(wakeup);

        // NOTE: we don't use the while let loop due to borrow issues
        #[allow(clippy::while_let_loop)]
        loop {
            let ev = if let Some(ev) = self.pool.try_recv() {
                ev.into_owned()
            } else {
                break;
            };

            let relay = RelayPool::canonicalize_url(ev.relay.clone());

            match (&ev.event).into() {
                RelayEvent::Opened => {
                    debug!("handle_opened {}", relay);

                    // handle legacy subscriptions
                    legacy_relay_handler.handle_opened(&mut self.ndb, &mut self.pool, &relay);

                    // send our remote subscriptions for this relay
                    for ssr in self.remote.values_mut() {
                        let mut substate = ssr.borrow_mut();
                        let remote_id = substate.spec.remote_id.clone();
                        let filters = substate.spec.filters.clone();
                        if let Some(remotesubstate) = &mut substate.remote {
                            if let Some(rss) = &remotesubstate.relays.get(&relay) {
                                match rss {
                                    RelaySubState::Pending => {
                                        debug!(
                                            "SubMan handle_opened: sending sub {} {}: {:?}",
                                            remote_id,
                                            relay,
                                            filters
                                                .iter()
                                                .map(|f| f.json().unwrap_or_default())
                                                .collect::<Vec<_>>(),
                                        );
                                        self.pool.send_to(
                                            &ClientMessage::req(remote_id, filters),
                                            &relay,
                                        );
                                        remotesubstate.update_rss(&relay, RelaySubState::Syncing);
                                    }
                                    _ => {
                                        debug!(
                                            "SubMan handle_opened: {} {} ignored in state {:?}",
                                            remote_id, relay, rss
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                // TODO: handle reconnects
                RelayEvent::Closed => warn!("{} connection closed", &relay),
                RelayEvent::Error(e) => error!("{}: {}", &relay, e),
                RelayEvent::Other(msg) => trace!("other event {:?}", &msg),
                RelayEvent::Message(msg) => {
                    self.process_message(legacy_relay_handler, &relay, &msg);
                }
            }
        }
        Ok(())
    }

    pub fn process_message<H: LegacyRelayHandler>(
        &mut self,
        legacy_relay_handler: &mut H,
        relay: &str,
        msg: &RelayMessage,
    ) {
        match msg {
            RelayMessage::Event(_subid, ev) => {
                let relay = if let Some(relay) = self.pool.relays.iter().find(|r| r.url() == relay)
                {
                    relay
                } else {
                    error!("couldn't find relay {} for note processing!?", relay);
                    return;
                };

                match relay {
                    PoolRelay::Websocket(_) => {
                        //info!("processing event {}", event);
                        if let Err(err) = self.ndb.process_event_with(
                            ev,
                            nostrdb::IngestMetadata::new()
                                .client(false)
                                .relay(relay.url()),
                        ) {
                            error!("error processing event {ev}: {err}");
                        }
                    }
                    PoolRelay::Multicast(_) => {
                        // multicast events are client events
                        if let Err(err) = self.ndb.process_event_with(
                            ev,
                            nostrdb::IngestMetadata::new()
                                .client(true)
                                .relay(relay.url()),
                        ) {
                            error!("error processing multicast event {ev}: {err}");
                        }
                    }
                }
            }
            RelayMessage::Notice(msg) => warn!("Notice from {}: {}", relay, msg),
            RelayMessage::OK(cr) => info!("OK {:?}", cr),
            RelayMessage::Eose(sid) => {
                debug!("SubMan process_message Eose {} {}", sid, relay);
                let mut substate_finished = false;
                // do we have this sub in the subman remote subscriptions?
                if let Some(ss) = self.remote.get_mut(*sid) {
                    let is_oneshot = ss.borrow().spec.is_oneshot;
                    let mut substate = ss.borrow_mut();
                    if let Some(remotesubstate) = &mut substate.remote {
                        remotesubstate.update_rss(relay, RelaySubState::Current);

                        if is_oneshot {
                            SubMan::close_relay_sub(&mut self.pool, sid, relay);
                            remotesubstate.update_rss(relay, RelaySubState::Closed);
                        }

                        // any relays left syncing?
                        substate_finished = remotesubstate.consider_finished(is_oneshot);
                    }
                } else {
                    // we didn't find it in the subman, delegate to the legacy code
                    legacy_relay_handler.handle_eose(&mut self.ndb, &mut self.pool, sid, relay);
                }
                if substate_finished {
                    if let Err(err) = self.remove_substate_remote_id(&sid.to_string()) {
                        error!("trouble removing substate for {}: {:?}", sid, err);
                    }
                }
            }
        }
    }
}

pub trait LegacyRelayHandler {
    fn handle_opened(&mut self, ndb: &mut Ndb, pool: &mut RelayPool, relay: &str);
    fn handle_eose(&mut self, ndb: &mut Ndb, pool: &mut RelayPool, id: &str, relay: &str);
}

struct LocalSub {
    ndb: Ndb,
    local_id: LocalId, // ndb id
    localstrm: SubscriptionStream,
}

#[allow(unused)]
struct RemoteSub {
    remote_id: RemoteId,          // remote nostr sub id
    rx_ended: mpsc::Receiver<()>, // end-of-stream
}

pub struct SubReceiver {
    localsub: Option<LocalSub>,
    remotesub: Option<RemoteSub>,
}

impl Drop for SubReceiver {
    fn drop(&mut self) {
        debug!("dropping Receiver for {}", self.idstr());
    }
}

impl SubReceiver {
    pub fn idstr(&self) -> String {
        let mut idstr = "".to_string();
        if let Some(lsub) = &self.localsub {
            idstr.push_str(&format!("local:{}", lsub.local_id));
        }
        if let Some(rsub) = &self.remotesub {
            if !idstr.is_empty() {
                idstr.push_str(", ");
            }
            idstr.push_str(&format!("remote:{}", rsub.remote_id));
        }
        if idstr.is_empty() {
            "query".to_string()
        } else {
            idstr
        }
    }

    pub fn local_id(&self) -> Option<LocalId> {
        self.localsub.as_ref().map(|lsub| lsub.local_id)
    }

    pub fn remote_id(&self) -> Option<RemoteId> {
        self.remotesub.as_ref().map(|rsub| rsub.remote_id.clone())
    }

    pub async fn next(&mut self) -> SubResult<Vec<nostrdb::NoteKey>> {
        if let (Some(lsub), Some(rsub)) = (&mut self.localsub, &mut self.remotesub) {
            // local and remote subs
            futures::select! {
                notes = lsub.localstrm.next().fuse() => {
                    match notes {
                        Some(notes) => Ok(notes),
                        None => Err(SubError::StreamEnded),
                    }
                },
                _ = rsub.rx_ended.next().fuse() => {
                    Err(SubError::StreamEnded)
                }
            }
        } else if let Some(lsub) = &mut self.localsub {
            // only local sub
            lsub.localstrm.next().await.ok_or(SubError::StreamEnded)
        } else if let Some(rsub) = &mut self.remotesub {
            // only remote sub (prefetch only, values not returned)
            match rsub.rx_ended.next().await {
                // in both cases the stream has ended
                Some(_) => Err(SubError::StreamEnded), // an EOSE was observed
                None => Err(SubError::StreamEnded),    // the subscription was closed
            }
        } else {
            // query case
            Err(SubError::InternalError("unimplmented".to_string()))
        }
    }

    pub fn poll(&mut self, max_notes: u32) -> Vec<nostrdb::NoteKey> {
        assert!(self.localsub.is_some()); // FIXME - local only
        let localsub = self.localsub.as_mut().unwrap();
        localsub.ndb.poll_for_notes(localsub.local_id.0, max_notes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testdbs_path_async;
    use crate::util::test_util::{raw_msg, test_keypair, ManagedNdb};
    use nostrdb::{NoteKey, Transaction};

    const RELAY_URL: &str = "test_url";

    // test basic subscription functionality
    #[tokio::test]
    async fn test_subman_sub() -> Result<(), Box<dyn std::error::Error>> {
        // setup an ndb and subman to test
        let (_mndb, ndb) = ManagedNdb::setup(&testdbs_path_async!());
        let mut subman = SubMan::new(ndb.clone(), RelayPool::new());

        // subscribe to some stuff
        let mut receiver = subman.subscribe(
            SubSpecBuilder::new()
                .filters(vec![Filter::new().kinds(vec![1]).build()])
                .constraint(SubConstraint::OnlyLocal)
                .build(),
        )?;
        let local_id = receiver.local_id().unwrap();

        // nothing should be available yet
        assert_eq!(receiver.poll(1), vec![]);

        // process a test event that matches the subscription
        let keys1 = test_keypair(1);
        let kind = 1;
        let content = "abc";
        ndb.process_event_with(
            &raw_msg("subid", &keys1, kind, content),
            nostrdb::IngestMetadata::new()
                .client(false)
                .relay(RELAY_URL),
        )?;

        // receiver should now see the msg
        let nks = receiver.next().await?;
        assert_eq!(nks.len(), 1);
        let txn = Transaction::new(&ndb)?;
        let note = ndb.get_note_by_key(&txn, nks[0])?;
        assert_eq!(note.pubkey(), keys1.pubkey.bytes());
        assert_eq!(note.kind(), kind);
        assert_eq!(note.content(), content);

        // now nothing should be available again
        assert_eq!(receiver.poll(1), vec![]);

        subman.unsubscribe_local_id(&local_id)?;
        Ok(())
    }

    // ensure that the subscription works when it is waiting before the event
    #[tokio::test]
    async fn test_subman_sub_with_waiting_thread() -> Result<(), Box<dyn std::error::Error>> {
        // setup an ndb and subman to test
        let (_mndb, ndb) = ManagedNdb::setup(&testdbs_path_async!());
        let mut subman = SubMan::new(ndb.clone(), RelayPool::new());

        // subscribe to some stuff
        let mut receiver = subman.subscribe(
            SubSpecBuilder::new()
                .filters(vec![Filter::new().kinds(vec![1]).build()])
                .constraint(SubConstraint::OnlyLocal)
                .build(),
        )?;
        let local_id = receiver.local_id().unwrap();

        // spawn a task to wait for the next message
        let handle = tokio::spawn(async move {
            let nks = receiver.next().await.unwrap();
            assert_eq!(nks.len(), 1); // Ensure one message is received
            (receiver, nks) // return the receiver as well
        });

        // process a test event that matches the subscription
        let keys1 = test_keypair(1);
        let kind = 1;
        let content = "abc";
        ndb.process_event_with(
            &raw_msg("subid", &keys1, kind, content),
            nostrdb::IngestMetadata::new()
                .client(false)
                .relay(RELAY_URL),
        )?;

        // await the spawned task to ensure it completes
        let (mut receiver, nks) = handle.await?;

        // validate the received message
        let txn = Transaction::new(&ndb)?;
        let note = ndb.get_note_by_key(&txn, nks[0])?;
        assert_eq!(note.pubkey(), keys1.pubkey.bytes());
        assert_eq!(note.kind(), kind);
        assert_eq!(note.content(), content);

        // ensure no additional messages are available
        assert_eq!(receiver.poll(1), vec![]);

        subman.unsubscribe_local_id(&local_id)?;
        Ok(())
    }

    // test subscription poll and next interaction
    #[tokio::test]
    async fn test_subman_poll_and_next() -> Result<(), Box<dyn std::error::Error>> {
        // setup an ndb and subman to test
        let (_mndb, ndb) = ManagedNdb::setup(&testdbs_path_async!());
        let mut subman = SubMan::new(ndb.clone(), RelayPool::new());

        // subscribe to some stuff
        let mut receiver = subman.subscribe(
            SubSpecBuilder::new()
                .filters(vec![Filter::new().kinds(vec![1]).build()])
                .constraint(SubConstraint::OnlyLocal)
                .build(),
        )?;
        let local_id = receiver.local_id().unwrap();

        // nothing should be available yet
        assert_eq!(receiver.poll(1), vec![]);

        // process a test event that matches the subscription
        let keys1 = test_keypair(1);
        let kind = 1;
        let content = "abc";
        ndb.process_event_with(
            &raw_msg("subid", &keys1, kind, content),
            nostrdb::IngestMetadata::new()
                .client(false)
                .relay(RELAY_URL),
        )?;
        std::thread::sleep(std::time::Duration::from_millis(150));

        // now poll should consume the note
        assert_eq!(receiver.poll(1), vec![NoteKey::new(1)]);

        // nothing more available
        assert_eq!(receiver.poll(1), vec![]);

        // process a second event
        let content = "def";
        ndb.process_event_with(
            &raw_msg("subid", &keys1, kind, content),
            nostrdb::IngestMetadata::new()
                .client(false)
                .relay(RELAY_URL),
        )?;

        // now receiver should now see the second note
        assert_eq!(receiver.next().await?, vec![NoteKey::new(2)]);

        subman.unsubscribe_local_id(&local_id)?;
        Ok(())
    }
}
