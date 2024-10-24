use std::collections::HashMap;

use enostr::{Filter, RelayPool};
use nostrdb::{Ndb, Transaction};
use tracing::{debug, info, warn};

use crate::{
    multi_subscriber::MultiSubscriber, note::NoteRef, notecache::NoteCache, timeline::TimelineTab,
    Error, Result,
};

