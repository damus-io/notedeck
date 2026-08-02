use enostr::NormRelayUrl;
use hashbrown::HashSet;
use nostrdb::{Filter, SendFilter};

use crate::remote_data::{
    RemoteAdvertisedFetchCoverage, RemoteFetchCommand, RemoteIntent, RemoteIntentBatchBuilder,
};

/// Additional remote-advertised relay coverage for a transient selected-account read fetch.
pub(crate) struct OneshotRemoteAdvertisedCoverage {
    relays: HashSet<NormRelayUrl>,
    filters: Vec<Filter>,
}

impl OneshotRemoteAdvertisedCoverage {
    pub(crate) fn new(
        relays: impl IntoIterator<Item = NormRelayUrl>,
        filters: Vec<Filter>,
    ) -> Self {
        Self {
            relays: relays.into_iter().collect(),
            filters,
        }
    }
}

/// App-facing one-shot relay API.
///
/// This keeps transient read requests (REQ/EOSE) separate from durable
/// scoped subscriptions.
pub struct OneshotApi<'o> {
    batch: &'o mut RemoteIntentBatchBuilder,
}

impl<'o> OneshotApi<'o> {
    pub(crate) fn new(batch: &'o mut RemoteIntentBatchBuilder) -> Self {
        Self { batch }
    }

    /// Send a one-shot request to the selected account's read relay set.
    pub fn oneshot(&mut self, filters: Vec<Filter>) {
        self.oneshot_with_remote_advertised_coverage(filters, Vec::new());
    }

    /// Send a selected-account read request with additive remote-advertised relay coverage.
    pub(crate) fn oneshot_with_remote_advertised_coverage(
        &mut self,
        filters: Vec<Filter>,
        remote_advertised: Vec<OneshotRemoteAdvertisedCoverage>,
    ) {
        let filters = match send_filters_prune_empty(filters) {
            Ok(filters) => filters,
            Err(_) => {
                tracing::warn!("failed to send one-shot request: filter is not sendable");
                return;
            }
        };
        if filters.is_empty() {
            return;
        }

        let remote_advertised = remote_advertised
            .into_iter()
            .filter_map(|coverage| {
                if coverage.relays.is_empty() {
                    return None;
                }

                let filters = match send_filters_prune_empty(coverage.filters) {
                    Ok(filters) => filters,
                    Err(_) => {
                        tracing::warn!(
                            "failed to send remote-advertised one-shot coverage: filter is not sendable"
                        );
                        return None;
                    }
                };
                (!filters.is_empty())
                    .then(|| RemoteAdvertisedFetchCoverage::new(coverage.relays, filters))
            })
            .collect();

        self.batch.push(RemoteIntent::Fetch(
            RemoteFetchCommand::SelectedAccountRead {
                filters,
                remote_advertised,
            },
        ));
    }
}

fn send_filters_prune_empty(filters: Vec<Filter>) -> Result<Vec<SendFilter>, ()> {
    filters
        .into_iter()
        .filter(|filter| filter.num_elements() != 0)
        .map(|filter| SendFilter::try_from_filter(filter).map_err(|_| ()))
        .collect()
}
