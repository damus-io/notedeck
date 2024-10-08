use futures::channel::mpsc;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use uuid::Uuid;

use nostrdb::Filter;

use crate::Damus;

#[allow(dead_code)] // until InternalError is used
#[derive(Debug)]
pub enum DispatcherError {
    InternalError(String),
    NdbError(nostrdb::Error),
}

impl From<nostrdb::Error> for DispatcherError {
    fn from(err: nostrdb::Error) -> Self {
        DispatcherError::NdbError(err)
    }
}

impl fmt::Display for DispatcherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DispatcherError::InternalError(msg) => write!(f, "Internal error: {}", msg),
            DispatcherError::NdbError(err) => write!(f, "nostrdb error: {}", err),
        }
    }
}

impl Error for DispatcherError {}

pub type DispatcherResult<T> = Result<T, DispatcherError>;

#[derive(Debug)]
pub enum Event {
    NdbSubUpdate,
}

/// Used by the relay code to dispatch events to a waiting handlers
#[derive(Debug, Clone)]
pub struct EventSink {
    pub sender: mpsc::Sender<Event>,
}

/// Maps subscription id to handler for the subscription
pub type HandlerTable = HashMap<u64, EventSink>;

/// Used by async tasks to receive events
#[allow(dead_code)] // until id is read
#[derive(Debug)]
pub struct EventSource {
    pub ndbid: u64,
    pub poolid: String,
    pub receiver: mpsc::Receiver<Event>,
}

pub fn subscribe(
    damus: &mut Damus,
    filters: &[Filter],
    bufsz: usize,
) -> DispatcherResult<EventSource> {
    let (sender, receiver) = mpsc::channel::<Event>(bufsz);
    let ndbid = damus.ndb.subscribe(&filters)?.id();
    let poolid = Uuid::new_v4().to_string();
    damus.pool.subscribe(poolid.clone(), filters.into());
    damus.dispatch.insert(ndbid, EventSink { sender });
    Ok(EventSource {
        ndbid,
        poolid,
        receiver,
    })
}

pub fn _unsubscribe(_sub: EventSource) -> DispatcherResult<()> {
    unimplemented!()
}
