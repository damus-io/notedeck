mod broadcast;
mod compaction;
mod coordinator;
mod identity;
mod limits;
pub mod message;
mod multicast;
mod outbox;
pub mod pool;
mod queue;
pub mod subs_debug;
mod subscription;
mod transparent;
mod websocket;

pub use broadcast::{BroadcastCache, BroadcastRelay};
pub use identity::{
    NormRelayUrl, OutboxSubId, RelayId, RelayReqId, RelayReqStatus, RelayType, RelayUrlPkgs,
};
pub use limits::{
    RelayCoordinatorLimits, RelayLimitations, SubPass, SubPassGuardian, SubPassRevocation,
};
pub use multicast::{MulticastRelay, MulticastRelayCache};
use nostrdb::Filter;
pub use outbox::{OutboxPool, OutboxSession, OutboxSessionHandler};
pub use queue::QueuedTasks;
pub use subscription::{
    FullModificationTask, ModifyFiltersTask, ModifyRelaysTask, ModifyTask, OutboxSubscriptions,
    OutboxTask, SubscribeTask,
};
pub use websocket::{WebsocketConn, WebsocketRelay};

#[cfg(test)]
pub mod test_utils;

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

    #[allow(dead_code)]
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

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.filters.iter().all(|f| f.num_elements() == 0)
    }
}

#[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;

    fn filter_has_since(filter: &Filter, expected: u64) -> bool {
        let json = filter.json().expect("filter json");
        json.contains(&format!("\"since\":{}", expected))
    }

    #[test]
    fn since_optimize_applies_last_seen_to_filter() {
        let filter = Filter::new().kinds(vec![1]).build();
        let mut metadata_filters = MetadataFilters::new(vec![filter]);

        // Initially no since
        let json_before = metadata_filters.get_filters()[0]
            .json()
            .expect("filter json");
        assert!(
            !json_before.contains("\"since\""),
            "filter should not have since initially"
        );

        // Set last_seen on metadata
        metadata_filters.meta[0].last_seen = Some(12345);

        // Call since_optimize
        metadata_filters.since_optimize();

        // Now filter should have since
        assert!(
            filter_has_since(&metadata_filters.get_filters()[0], 12345),
            "filter should have since:12345 after optimization"
        );
    }

    #[test]
    fn since_optimize_skips_filters_without_last_seen() {
        let filter1 = Filter::new().kinds(vec![1]).build();
        let filter2 = Filter::new().kinds(vec![2]).build();
        let mut metadata_filters = MetadataFilters::new(vec![filter1, filter2]);

        // Only set last_seen on first filter
        metadata_filters.meta[0].last_seen = Some(99999);

        metadata_filters.since_optimize();

        // First filter should have since
        assert!(
            filter_has_since(&metadata_filters.get_filters()[0], 99999),
            "first filter should have since"
        );

        // Second filter should NOT have since
        let json_second = metadata_filters.get_filters()[1]
            .json()
            .expect("filter json");
        assert!(
            !json_second.contains("\"since\""),
            "second filter should not have since"
        );
    }

    #[test]
    fn since_optimize_overwrites_existing_since() {
        // Create filter with initial since value
        let filter = Filter::new().kinds(vec![1]).since(100).build();
        let mut metadata_filters = MetadataFilters::new(vec![filter]);

        // Verify initial since
        assert!(
            filter_has_since(&metadata_filters.get_filters()[0], 100),
            "filter should have initial since:100"
        );

        // Set different last_seen
        metadata_filters.meta[0].last_seen = Some(200);
        metadata_filters.since_optimize();

        // Since should be updated to new value
        assert!(
            filter_has_since(&metadata_filters.get_filters()[0], 200),
            "filter should have updated since:200"
        );
    }
}
