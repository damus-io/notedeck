use futures::channel::mpsc;
use std::collections::HashMap;

use enostr::PoolEvent;

#[derive(Debug)]
pub struct SubscriptionHandler {
    _sender: mpsc::Sender<PoolEvent>,
}

/// Maps subscription id to handler for the subscription
pub type HandlerTable = HashMap<String, SubscriptionHandler>;
