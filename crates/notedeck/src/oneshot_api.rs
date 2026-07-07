use nostrdb::{Filter, SendFilter};

use crate::remote_data::{RemoteFetchCommand, RemoteIntent, RemoteIntentBatchBuilder};

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

        self.batch.push(RemoteIntent::Fetch(
            RemoteFetchCommand::SelectedAccountRead { filters },
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
