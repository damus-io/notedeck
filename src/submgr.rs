#![allow(unused)]

use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use enostr::Filter;
use nostrdb;

/// The Subscription Manager
///
/// NOTE - This interface wishes it was called Subscriptions but there
/// already is one.  Using a lame (but short) placeholder name instead
/// for now ...
///
/// ```ignore
/// use std::error::Error;
///
/// use notedeck::submgr::{SubMgr, SubSpecBuilder, SubError};
/// use enostr::Filter;
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn Error>> {
///     let mut submgr = SubMgr::new();
///
///     let filter = Filter::new().kinds(vec![1, 2, 3]).build();
///     let ep = submgr.subscribe(SubSpecBuilder::new(vec![filter]).build())?;
///     loop {
///         match ep.next().await {
///             Ok(nks) => {
///                 // process the note keys
///             },
///             Err(SubError::ReevaluateState) => {
///                 // not really an error, break out of loop and reevaluate state
///                 break;
///             },
///             Err(err) => {
///                 // something bad happened
///                 eprintln!("Error: {:?}", err);
///                 break;
///             },
///         }
///     }
///     submgr.unsubscribe(ep)?;
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

pub struct SubId(nostrdb::Subscription);

#[derive(Debug)]
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

pub struct SubSpec {
    rmtid: String,
    filters: Vec<Filter>,
    constraints: Vec<SubConstraint>,
    allowed_relays: Vec<String>,
    blocked_relays: Vec<String>,
    is_oneshot: bool,
}

pub struct SubMgr {
    subs: HashMap<SubId, (SubSpec, SubEndpoint)>,
}

impl SubMgr {
    pub fn new() -> Self {
        SubMgr {
            subs: HashMap::new(),
        }
    }

    pub fn subscribe(&mut self, sub: SubSpec) -> SubResult<SubEndpoint> {
        unimplemented!();
    }

    pub fn unsubscribe(&mut self, ep: SubEndpoint) -> SubResult<()> {
        unimplemented!();
    }
}

pub struct SubEndpoint {
    id: SubId,
    // internals omitted ...
}

impl SubEndpoint {
    pub async fn next(&self) -> SubResult<Vec<nostrdb::NoteKey>> {
        unimplemented!();
    }
}
