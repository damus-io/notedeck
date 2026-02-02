mod identity;
mod limits;
pub mod message;
mod multicast;
pub mod pool;
pub mod subs_debug;
mod websocket;

pub use identity::{
    NormRelayUrl, OutboxSubId, RelayId, RelayReqId, RelayReqStatus, RelayType, RelayUrlPkgs,
};
pub use limits::{
    RelayCoordinatorLimits, RelayLimitations, SubPass, SubPassGuardian, SubPassRevocation,
};
pub use multicast::MulticastRelay;
use nostrdb::Filter;
pub use websocket::{WebsocketConn, WebsocketRelay};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum RelayStatus {
    Connected,
    Connecting,
    Disconnected,
}

enum UnownedRelay<'a> {
    Websocket(&'a mut WebsocketRelay),
    Multicast(&'a mut MulticastRelay),
}

/// RawEventData is the event raw data from a relay
pub struct RawEventData<'a> {
    pub url: &'a str,
    pub event_json: &'a str,
    pub relay_type: RelayImplType,
}

/// RelayImplType identifies whether an event came from a websocket or multicast relay.
pub enum RelayImplType {
    Websocket,
    Multicast,
}

pub enum RelayTask {
    Unsubscribe,
    Subscribe,
}

pub struct FilterMetadata {
    filter_json_size: usize,
    last_seen: Option<u64>,
}

pub struct MetadataFilters {
    filters: Vec<Filter>,
    meta: Vec<FilterMetadata>,
}

impl MetadataFilters {
    pub fn new(filters: Vec<Filter>) -> Self {
        let meta = filters
            .iter()
            .map(|f| FilterMetadata {
                filter_json_size: f.json().ok().map(|j| j.len()).unwrap_or(0),
                last_seen: None,
            })
            .collect();
        Self { filters, meta }
    }

    pub fn json_size_sum(&self) -> usize {
        self.meta.iter().map(|f| f.filter_json_size).sum()
    }

    pub fn since_optimize(&mut self) {
        for (filter, meta) in self.filters.iter_mut().zip(self.meta.iter()) {
            let Some(last_seen) = meta.last_seen else {
                continue;
            };

            *filter = filter.clone().since_mut(last_seen);
        }
    }

    pub fn get_filters(&self) -> &Vec<Filter> {
        &self.filters
    }

    pub fn iter(&self) -> MetadataFiltersIter<'_> {
        MetadataFiltersIter {
            filters: self.filters.iter(),
            meta: self.meta.iter(),
        }
    }

    pub fn iter_mut(&mut self) -> MetadataFiltersIterMut<'_> {
        MetadataFiltersIterMut {
            filters: self.filters.iter_mut(),
            meta: self.meta.iter_mut(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.filters.iter().all(|f| f.num_elements() == 0)
    }
}

pub struct MetadataFiltersIter<'a> {
    filters: std::slice::Iter<'a, Filter>,
    meta: std::slice::Iter<'a, FilterMetadata>,
}

impl<'a> Iterator for MetadataFiltersIter<'a> {
    type Item = (&'a Filter, &'a FilterMetadata);

    fn next(&mut self) -> Option<Self::Item> {
        Some((self.filters.next()?, self.meta.next()?))
    }
}

pub struct MetadataFiltersIterMut<'a> {
    filters: std::slice::IterMut<'a, Filter>,
    meta: std::slice::IterMut<'a, FilterMetadata>,
}

impl<'a> Iterator for MetadataFiltersIterMut<'a> {
    type Item = (&'a mut Filter, &'a mut FilterMetadata);

    fn next(&mut self) -> Option<Self::Item> {
        Some((self.filters.next()?, self.meta.next()?))
    }
}
