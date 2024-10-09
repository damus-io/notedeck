use futures::channel::mpsc;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use enostr::PoolEvent;

use nostrdb::Filter;

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

/// Used by the relay code to dispatch events to a waiting handlers
#[derive(Debug)]
pub struct SubscriptionHandler {
    _sender: mpsc::Sender<PoolEvent>,
}

/// Maps subscription id to handler for the subscription
pub type HandlerTable = HashMap<String, SubscriptionHandler>;

/// Used by tasks to receive events
#[derive(Debug)]
pub struct Subscription {
    id: String,
    _receiver: mpsc::Receiver<PoolEvent>,
}

pub async fn fetch(_filter: Filter) -> DispatcherResult<PoolEvent> {
    unimplemented!()
}

pub fn subscribe(_filter: Filter) -> DispatcherResult<Subscription> {
    unimplemented!()
}

pub fn unsubscribe(_sub: Subscription) -> DispatcherResult<()> {
    unimplemented!()
}
