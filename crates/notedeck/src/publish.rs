use enostr::{EventClientMessage, RelayId};
use nostrdb::Note;

use crate::remote_data::{RemoteIntent, RemoteIntentBatchBuilder, RemotePublishCommand};

/// Explicit-relay publishing API that does not depend on account state.
pub struct ExplicitPublishApi<'o> {
    batch: &'o mut RemoteIntentBatchBuilder,
}

impl<'o> ExplicitPublishApi<'o> {
    pub(crate) fn new(batch: &'o mut RemoteIntentBatchBuilder) -> Self {
        Self { batch }
    }

    /// Publish a note to an explicit relay target set.
    pub fn publish_note(&mut self, note: &Note, relays: Vec<RelayId>) {
        let note_json = match note.json() {
            Ok(note_json) => note_json,
            Err(err) => {
                tracing::error!("failed to serialize note for publish: {err}");
                return;
            }
        };
        self.publish_event_json(note_json, relays);
    }

    /// Publish an already-built event JSON to an explicit relay target set.
    pub fn publish_event_json(&mut self, note_json: String, relays: Vec<RelayId>) {
        self.batch
            .push(RemoteIntent::Publish(RemotePublishCommand::Explicit {
                msg: EventClientMessage { note_json },
                relays,
            }));
    }
}

/// Selected-account write-relay publishing API.
pub struct AccountsPublishApi<'o> {
    batch: &'o mut RemoteIntentBatchBuilder,
}

impl<'o> AccountsPublishApi<'o> {
    pub(crate) fn new(batch: &'o mut RemoteIntentBatchBuilder) -> Self {
        Self { batch }
    }

    /// Publish a note to the selected account's write relay set.
    pub fn publish_note(&mut self, note: &Note) {
        let note_json = match note.json() {
            Ok(note_json) => note_json,
            Err(err) => {
                tracing::error!("failed to serialize note for publish: {err}");
                return;
            }
        };
        self.batch.push(RemoteIntent::Publish(
            RemotePublishCommand::SelectedAccountWrite {
                msg: EventClientMessage { note_json },
            },
        ));
    }
}

/// Compatibility wrapper over typed publishing APIs.
pub struct PublishApi<'o> {
    batch: &'o mut RemoteIntentBatchBuilder,
}

impl<'o> PublishApi<'o> {
    pub(crate) fn new(batch: &'o mut RemoteIntentBatchBuilder) -> Self {
        Self { batch }
    }

    pub fn explicit(&mut self) -> ExplicitPublishApi<'_> {
        ExplicitPublishApi::new(self.batch)
    }

    pub fn accounts_write(&mut self) -> AccountsPublishApi<'_> {
        AccountsPublishApi::new(self.batch)
    }
}
