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
}

impl fmt::Display for DispatcherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DispatcherError::InternalError(msg) => write!(f, "Internal error: {}", msg),
        }
    }
}

impl Error for DispatcherError {}

pub type DispatcherResult<T> = Result<T, DispatcherError>;

#[derive(Debug)]
pub enum Event {
    Pool,
}

/// Used by the relay code to dispatch events to a waiting handlers
#[derive(Debug, Clone)]
pub struct SubscriptionHandler {
    pub sender: mpsc::Sender<Event>,
}

/// Maps subscription id to handler for the subscription
pub type HandlerTable = HashMap<String, SubscriptionHandler>;

/// Used by async tasks to receive events
#[allow(dead_code)] // until id is read
#[derive(Debug)]
pub struct Subscription {
    pub id: String,
    pub receiver: mpsc::Receiver<Event>,
}

pub fn subscribe(
    damus: &mut Damus,
    filters: &[Filter],
    bufsz: usize,
) -> DispatcherResult<Subscription> {
    let (sender, receiver) = mpsc::channel::<Event>(bufsz);
    let id = Uuid::new_v4().to_string();
    damus
        .dispatch
        .insert(id.clone(), SubscriptionHandler { sender });
    damus.pool.subscribe(id.clone(), filters.into());
    Ok(Subscription { id, receiver })
}

pub fn _unsubscribe(_sub: Subscription) -> DispatcherResult<()> {
    unimplemented!()
}
