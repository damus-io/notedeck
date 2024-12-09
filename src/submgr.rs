#![allow(unused)]

use std::collections::HashMap;

use enostr::Filter;
use nostrdb;

/// The Subscription Manager
///
/// NOTE - This interface wishes it was called Subscriptions but there
/// already is one.  Using a lame (but short) placeholder name instead
/// for now ...
///
/// ```
/// use crate::SubMgr;
///
/// let mut submgr = SubMgr::new();
///
/// let filter = Filter::new().kinds(vec![1, 2, 3]).build();
/// let ep = submgr.subscribe(SubSpecBuilder::new(vec![filter]).build())?;
/// loop {
///     match ep.next().await {
///         Ok(nks) => {
///             // process the note keys
///         },
///         Err(ReevaluateState) => {
///             // not really an error, break out of loop and reevaluate state
///         },
///         Err(err) => {
///             // something bad happened
///         },
///     }
/// }
/// submgr.unsubscribe(ep)?;
/// ```

pub enum SubError {
    ReevaluateState,
    InternalError(String),
    NdbError(nostrdb::Error),
}

pub type SubResult<T> = Result<T, SubError>;

pub struct SubId(nostrdb::Subscription);

pub enum SubConstraint {
    OneShot,
    AllowedRelays(Vec<String>),
    ProhibitedRelays(Vec<String>),
    // other constraints as we think of them ...
}

pub struct SubSpecBuilder {
    rmtid: Option<String>,
    filters: Vec<Filter>,
    allowed_relays: Vec<String>,
    prohibited_relays: Vec<String>,
    is_oneshot: bool,
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
    prohibited_relays: Vec<String>,
    is_oneshot: bool,
}

pub struct SubMgr {
    subs: HashMap<SubId, (SubSpec, SubEndpoint)>,
}

impl SubMgr {
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
