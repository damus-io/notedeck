use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use nostr::PublicKey;
use nostr44::{PublicKey as NdrPublicKey, UnsignedEvent as NdrUnsignedEvent};
use nostr_double_ratchet::{
    AppKeysManager, DeviceEntry, FileStorageAdapter, SessionManager, SessionState,
};

use super::compat::{app_event_to_ndr, app_unsigned_to_ndr, to_app_pubkey, to_ndr_pubkey};

struct DirectMessageSubscription {
    subid: String,
    authors: Vec<NdrPublicKey>,
}

/// Commands sent from the UI thread to the double-ratchet worker thread.
pub(crate) enum Command {
    /// Process an event received from the relay pool.
    ProcessEvent(nostr44::Event),
    /// Subscribe to discovery material for the given owner pubkey.
    SetupUser(NdrPublicKey),
    /// Check whether an active session can currently send to the given owner pubkey.
    ProbeSendReady(NdrPublicKey),
    /// Encrypt and publish an "inner rumor" event to the given recipient.
    SendEvent {
        recipient: NdrPublicKey,
        event: NdrUnsignedEvent,
    },
    /// Shut down the worker thread.
    Shutdown,
}

/// Handle for the double-ratchet worker.
///
/// The worker owns the [`SessionManager`] and storage adapter and runs on a background thread so
/// the egui render loop never blocks on crypto or file I/O.
pub(crate) struct WorkerHandle {
    cmd_tx: Sender<Command>,
}

impl Clone for WorkerHandle {
    fn clone(&self) -> Self {
        Self {
            cmd_tx: self.cmd_tx.clone(),
        }
    }
}

impl WorkerHandle {
    /// Spawn a worker thread and return a handle plus the [`SessionManagerEvent`] receiver.
    pub(crate) fn spawn(
        storage_dir: PathBuf,
        owner_pubkey: PublicKey,
        owner_identity_key: [u8; 32],
        device_id: String,
    ) -> nostr_double_ratchet::Result<(
        Self,
        Receiver<nostr_double_ratchet::SessionManagerEvent>,
        Receiver<PublicKey>,
    )> {
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (send_ready_tx, send_ready_rx) = crossbeam_channel::unbounded();
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let owner_ndr_pubkey = to_ndr_pubkey(owner_pubkey)?;

        let storage = Arc::new(FileStorageAdapter::new(storage_dir)?);

        std::thread::Builder::new()
            .name("double-ratchet-worker".to_string())
            .spawn(move || {
                // SessionManager drives sessions, invites, and decryption.
                let manager = SessionManager::new(
                    owner_ndr_pubkey,
                    owner_identity_key,
                    device_id,
                    owner_ndr_pubkey,
                    event_tx.clone(),
                    Some(storage.clone()),
                    None,
                );

                // AppKeys is required for other clients to discover which device identities are
                // authorized for this owner pubkey.
                let pubsub: Arc<dyn nostr_double_ratchet::NostrPubSub> = Arc::new(event_tx.clone());
                let mut app_keys = AppKeysManager::new(pubsub, Some(storage.clone()));

                if let Err(err) = app_keys.init() {
                    tracing::warn!("double-ratchet app keys init failed: {err}");
                }

                // Ensure our current device is present.
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or_default();
                if let Err(err) = app_keys.add_device(DeviceEntry::new(owner_ndr_pubkey, now)) {
                    tracing::warn!("double-ratchet app_keys.add_device failed: {err}");
                }

                // Publish AppKeys and our Invite material.
                //
                // NOTE: publishing is handled by the UI thread (signing + relay publish).
                if let Err(err) = app_keys.publish(owner_ndr_pubkey) {
                    tracing::warn!("double-ratchet app_keys.publish failed: {err}");
                }
                if let Err(err) = manager.init() {
                    tracing::warn!("double-ratchet manager.init failed: {err}");
                }
                let mut message_subscription = None;
                sync_direct_message_subscription(&manager, &event_tx, &mut message_subscription);

                while let Ok(cmd) = cmd_rx.recv() {
                    match cmd {
                        Command::ProcessEvent(event) => {
                            manager.process_received_event(event);
                            sync_direct_message_subscription(
                                &manager,
                                &event_tx,
                                &mut message_subscription,
                            );
                            publish_send_ready_peers(&manager, owner_ndr_pubkey, &send_ready_tx);
                        }
                        Command::SetupUser(recipient) => {
                            manager.setup_user(recipient);
                            sync_direct_message_subscription(
                                &manager,
                                &event_tx,
                                &mut message_subscription,
                            );
                            publish_send_ready_peers(&manager, owner_ndr_pubkey, &send_ready_tx);
                        }
                        Command::ProbeSendReady(recipient) => {
                            publish_send_ready_peer(
                                &manager,
                                owner_ndr_pubkey,
                                recipient,
                                &send_ready_tx,
                            );
                        }
                        Command::SendEvent { recipient, event } => {
                            let _ = manager.send_event(recipient, event);
                            sync_direct_message_subscription(
                                &manager,
                                &event_tx,
                                &mut message_subscription,
                            );
                            publish_send_ready_peers(&manager, owner_ndr_pubkey, &send_ready_tx);
                        }
                        Command::Shutdown => break,
                    }
                }
            })
            .map_err(|e| nostr_double_ratchet::Error::Storage(e.to_string()))?;

        Ok((Self { cmd_tx }, event_rx, send_ready_rx))
    }

    /// Forward an inbound relay event to the worker for session processing.
    pub(crate) fn process_event(&self, event: nostr::Event) {
        match app_event_to_ndr(&event) {
            Ok(event) => {
                let _ = self.cmd_tx.send(Command::ProcessEvent(event));
            }
            Err(err) => tracing::warn!("double-ratchet: failed to convert inbound event: {err}"),
        }
    }

    /// Ask the worker to subscribe to peer AppKeys and known device invites.
    pub(crate) fn setup_user(&self, recipient: PublicKey) {
        match to_ndr_pubkey(recipient) {
            Ok(recipient) => {
                let _ = self.cmd_tx.send(Command::SetupUser(recipient));
            }
            Err(err) => tracing::warn!("double-ratchet: failed to convert setup pubkey: {err}"),
        }
    }

    /// Ask the worker whether the selected account can send to `recipient` right now.
    pub(crate) fn probe_send_ready(&self, recipient: PublicKey) {
        match to_ndr_pubkey(recipient) {
            Ok(recipient) => {
                let _ = self.cmd_tx.send(Command::ProbeSendReady(recipient));
            }
            Err(err) => tracing::warn!("double-ratchet: failed to convert probe pubkey: {err}"),
        }
    }

    /// Ask the worker to encrypt and publish the given rumor to `recipient`.
    pub(crate) fn send_event(&self, recipient: PublicKey, event: nostr::UnsignedEvent) {
        match (to_ndr_pubkey(recipient), app_unsigned_to_ndr(&event)) {
            (Ok(recipient), Ok(event)) => {
                let _ = self.cmd_tx.send(Command::SendEvent { recipient, event });
            }
            (Err(err), _) | (_, Err(err)) => {
                tracing::warn!("double-ratchet: failed to convert outbound event: {err}");
            }
        }
    }

    /// Request a graceful worker shutdown.
    pub(crate) fn shutdown(&self) {
        let _ = self.cmd_tx.send(Command::Shutdown);
    }
}

fn sync_direct_message_subscription(
    manager: &SessionManager,
    event_tx: &Sender<nostr_double_ratchet::SessionManagerEvent>,
    current: &mut Option<DirectMessageSubscription>,
) {
    let mut authors = manager.get_all_message_push_author_pubkeys();
    authors.sort_by_key(|author| author.to_hex());

    if current
        .as_ref()
        .is_some_and(|subscription| subscription.authors == authors)
    {
        return;
    }

    if let Some(subscription) = current.take() {
        let _ = event_tx.send(nostr_double_ratchet::SessionManagerEvent::Unsubscribe(
            subscription.subid,
        ));
    }

    if authors.is_empty() {
        return;
    }

    let filter = nostr44::Filter::new()
        .kind(nostr44::Kind::Custom(
            nostr_double_ratchet::MESSAGE_EVENT_KIND as u16,
        ))
        .authors(authors.clone());
    let Ok(filter_json) = serde_json::to_string(&filter) else {
        tracing::warn!("double-ratchet: failed to encode message subscription filter");
        return;
    };

    let subid = format!("ndr-runtime-messages-notedeck-{}", uuid::Uuid::new_v4());
    let _ = event_tx.send(nostr_double_ratchet::SessionManagerEvent::Subscribe {
        subid: subid.clone(),
        filter_json,
    });
    *current = Some(DirectMessageSubscription { subid, authors });
}

fn publish_send_ready_peers(
    manager: &SessionManager,
    owner_pubkey: NdrPublicKey,
    send_ready_tx: &Sender<PublicKey>,
) {
    for (peer, _, state) in manager.export_active_sessions() {
        if peer == owner_pubkey || !session_can_send(&state) {
            continue;
        }
        if let Ok(peer) = to_app_pubkey(peer) {
            let _ = send_ready_tx.send(peer);
        }
    }
}

fn publish_send_ready_peer(
    manager: &SessionManager,
    owner_pubkey: NdrPublicKey,
    recipient: NdrPublicKey,
    send_ready_tx: &Sender<PublicKey>,
) {
    if recipient == owner_pubkey {
        return;
    }

    for (peer, _, state) in manager.export_active_sessions() {
        if peer == recipient && session_can_send(&state) {
            if let Ok(peer) = to_app_pubkey(peer) {
                let _ = send_ready_tx.send(peer);
            }
            return;
        }
    }
}

fn session_can_send(state: &SessionState) -> bool {
    state.their_next_nostr_public_key.is_some() && state.our_current_nostr_key.is_some()
}
