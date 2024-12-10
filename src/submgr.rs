#![allow(unused)]

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::{Arc, Mutex};

use enostr::Filter;
use nostrdb;

/// The Subscription Manager
///
/// NOTE - This interface wishes it was called Subscriptions but there
/// already is one.  Using a lame (but short) placeholder name instead
/// for now ...
///
/// ```no_run
/// use std::error::Error;
/// use std::sync::{Arc, Mutex};
///
/// use notedeck::submgr::{SubMgr, SubSpecBuilder, SubError};
/// use enostr::Filter;
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn Error>> {
///     let submgr = SubMgr::new();
///
///     // Define a filter and build the subscription specification
///     let filter = Filter::new().kinds(vec![1, 2, 3]).build();
///     let spec = SubSpecBuilder::new(vec![filter]).build();
///
///     // Subscribe and obtain a SubReceiver
///     let receiver = SubMgr::subscribe(submgr.clone(), spec)?;
///
///     // Process incoming note keys
///     loop {
///         match receiver.next().await {
///             Ok(note_keys) => {
///                 // Process the note keys
///                 println!("Received note keys: {:?}", note_keys);
///             },
///             Err(SubError::ReevaluateState) => {
///                 // Not really an error; break out to reevaluate the state
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
///     // The subscription will automatically be cleaned up when the receiver goes out of scope
///     Ok(())
/// }
/// ```

#[derive(Debug)]
pub enum SubError {
    ReevaluateState,
    InternalError(String),
    NdbError(nostrdb::Error),
}

impl From<nostrdb::Error> for SubError {
    fn from(err: nostrdb::Error) -> Self {
        SubError::NdbError(err)
    }
}

impl fmt::Display for SubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SubError::ReevaluateState => write!(f, "ReevaluateState"),
            SubError::InternalError(msg) => write!(f, "Internal error: {}", msg),
            SubError::NdbError(err) => write!(f, "nostrdb error: {}", err),
        }
    }
}

impl Error for SubError {}

pub type SubResult<T> = Result<T, SubError>;

#[derive(Debug, Clone, Copy)]
pub struct SubId(nostrdb::Subscription);

impl Ord for SubId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.id().cmp(&other.0.id()) // Access the inner `u64` and compare
    }
}

impl PartialOrd for SubId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other)) // Delegate to `cmp`
    }
}

impl PartialEq for SubId {
    fn eq(&self, other: &Self) -> bool {
        self.0.id() == other.0.id() // Compare the inner `u64`
    }
}

impl Eq for SubId {}

#[derive(Debug, Clone)]
pub enum SubConstraint {
    OneShot,                    // terminate subscription after initial query
    Local,                      // only query the local db, no remote subs
    OutboxRelays(Vec<String>),  // ensure one of these is in the active relay set
    AllowedRelays(Vec<String>), // if not empty, only use these relays
    BlockedRelays(Vec<String>), // if not empty, don't use these relays
                                // other constraints as we think of them ...
}

pub struct SubSpecBuilder {
    rmtid: Option<String>,
    filters: Vec<Filter>,
    is_oneshot: bool,
    is_local: bool,
    outbox_relays: Vec<String>,
    allowed_relays: Vec<String>,
    blocked_relays: Vec<String>,
}

impl SubSpecBuilder {
    pub fn new(filters: Vec<Filter>) -> Self {
        unimplemented!();
    }
    pub fn rmtid(mut self, id: String) -> Self {
        unimplemented!();
    }
    // ... more here ...
    pub fn build(self) -> SubSpec {
        unimplemented!();
    }
}

#[derive(Debug, Clone)]
pub struct SubSpec {
    rmtid: String,
    filters: Vec<Filter>,
    constraints: Vec<SubConstraint>,
    allowed_relays: Vec<String>,
    blocked_relays: Vec<String>,
    is_oneshot: bool,
}

pub struct SubMgr {
    subs: BTreeMap<SubId, (SubSpec, SubSender)>,
}

impl SubMgr {
    pub fn new() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(SubMgr {
            subs: BTreeMap::new(),
        }))
    }

    pub fn subscribe(sub_mgr: Arc<Mutex<SubMgr>>, spec: SubSpec) -> SubResult<SubReceiver> {
        let mut mgr = sub_mgr.lock().unwrap();
        let (id, sender, receiver) = mgr.make_subscription(&spec)?;
        mgr.subs.insert(id, (spec, sender));
        Ok(SubReceiver {
            id,
            sub_mgr: sub_mgr.clone(),
        })
    }

    pub fn unsubscribe(sub_mgr: Arc<Mutex<SubMgr>>, id: SubId) -> SubResult<()> {
        let mut mgr = sub_mgr.lock().unwrap();
        mgr.subs.remove(&id);
        Ok(())
    }

    fn make_subscription(&mut self, sub: &SubSpec) -> SubResult<(SubId, SubSender, SubReceiver)> {
        unimplemented!();
    }
}

pub struct SubSender {
    // internals omitted ...
}

pub struct SubReceiver {
    sub_mgr: Arc<Mutex<SubMgr>>,
    id: SubId,
    // internals omitted ...
}

impl SubReceiver {
    pub fn new(id: SubId, sub_mgr: Arc<Mutex<SubMgr>>) -> Self {
        SubReceiver { id, sub_mgr }
    }

    pub async fn next(&self) -> SubResult<Vec<nostrdb::NoteKey>> {
        unimplemented!();
    }
}

impl Drop for SubReceiver {
    fn drop(&mut self) {
        if let Err(err) = SubMgr::unsubscribe(self.sub_mgr.clone(), self.id.clone()) {
            eprintln!("Failed to unsubscribe: {:?}", err);
        }
    }
}
