use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use nostr::{PublicKey, UnsignedEvent};
use nostr_double_ratchet::{
    AppKeysManager, DeviceEntry, FileStorageAdapter, SessionManager, SessionState,
};

/// Commands sent from the UI thread to the double-ratchet worker thread.
pub(crate) enum Command {
    /// Process an event received from the relay pool.
    ProcessEvent(nostr::Event),
    /// Subscribe to discovery material for the given owner pubkey.
    SetupUser(PublicKey),
    /// Check whether an active session can currently send to the given owner pubkey.
    ProbeSendReady(PublicKey),
    /// Encrypt and publish an "inner rumor" event to the given recipient.
    SendEvent {
        recipient: PublicKey,
        event: UnsignedEvent,
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

        let storage = Arc::new(FileStorageAdapter::new(storage_dir)?);

        std::thread::Builder::new()
            .name("double-ratchet-worker".to_string())
            .spawn(move || {
                // SessionManager drives sessions, invites, and decryption.
                let manager = SessionManager::new(
                    owner_pubkey,
                    owner_identity_key,
                    device_id,
                    owner_pubkey,
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
                let _ = app_keys.add_device(DeviceEntry::new(owner_pubkey, now));

                // Publish AppKeys and our Invite material.
                //
                // NOTE: publishing is handled by the UI thread (signing + relay publish).
                let _ = app_keys.publish(owner_pubkey);
                let _ = manager.init();

                while let Ok(cmd) = cmd_rx.recv() {
                    match cmd {
                        Command::ProcessEvent(event) => {
                            manager.process_received_event(event);
                            publish_send_ready_peers(&manager, owner_pubkey, &send_ready_tx);
                        }
                        Command::SetupUser(recipient) => {
                            manager.setup_user(recipient);
                            publish_send_ready_peers(&manager, owner_pubkey, &send_ready_tx);
                        }
                        Command::ProbeSendReady(recipient) => {
                            publish_send_ready_peer(
                                &manager,
                                owner_pubkey,
                                recipient,
                                &send_ready_tx,
                            );
                        }
                        Command::SendEvent { recipient, event } => {
                            let _ = manager.send_event(recipient, event);
                            publish_send_ready_peers(&manager, owner_pubkey, &send_ready_tx);
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
        let _ = self.cmd_tx.send(Command::ProcessEvent(event));
    }

    /// Ask the worker to subscribe to peer AppKeys and known device invites.
    pub(crate) fn setup_user(&self, recipient: PublicKey) {
        let _ = self.cmd_tx.send(Command::SetupUser(recipient));
    }

    /// Ask the worker whether the selected account can send to `recipient` right now.
    pub(crate) fn probe_send_ready(&self, recipient: PublicKey) {
        let _ = self.cmd_tx.send(Command::ProbeSendReady(recipient));
    }

    /// Ask the worker to encrypt and publish the given rumor to `recipient`.
    pub(crate) fn send_event(&self, recipient: PublicKey, event: UnsignedEvent) {
        let _ = self.cmd_tx.send(Command::SendEvent { recipient, event });
    }

    /// Request a graceful worker shutdown.
    pub(crate) fn shutdown(&self) {
        let _ = self.cmd_tx.send(Command::Shutdown);
    }
}

fn publish_send_ready_peers(
    manager: &SessionManager,
    owner_pubkey: PublicKey,
    send_ready_tx: &Sender<PublicKey>,
) {
    for (peer, _, state) in manager.export_active_sessions() {
        if peer == owner_pubkey || !session_can_send(&state) {
            continue;
        }
        let _ = send_ready_tx.send(peer);
    }
}

fn publish_send_ready_peer(
    manager: &SessionManager,
    owner_pubkey: PublicKey,
    recipient: PublicKey,
    send_ready_tx: &Sender<PublicKey>,
) {
    if recipient == owner_pubkey {
        return;
    }

    for (peer, _, state) in manager.export_active_sessions() {
        if peer == recipient && session_can_send(&state) {
            let _ = send_ready_tx.send(peer);
            return;
        }
    }
}

fn session_can_send(state: &SessionState) -> bool {
    state.their_next_nostr_public_key.is_some() && state.our_current_nostr_key.is_some()
}
