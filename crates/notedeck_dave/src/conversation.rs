//! Kind-1988 conversation events: the shared per-account subscription, the
//! per-session note processing that keeps a remote session's chat in order,
//! and the remote permission request/response and mode/interrupt actions
//! they carry.

use crate::backend::BackendType;
use crate::publish::{ingest_live_event, pns_ingest};
use crate::{
    messages, session, session_events, session_loader, Dave, Message, PermissionResponse, SessionId,
};
use nostrdb::{NoteKey, Transaction};
use std::collections::HashMap;

/// A permission-mode change decoded from a remote command, to apply on the local
/// backend. See [`Dave::poll_remote_conversation_actions`].
pub(crate) struct ModeApply {
    pub(crate) backend_sid: String,
    pub(crate) backend_type: BackendType,
    pub(crate) mode: claude_agent_sdk_rs::PermissionMode,
}

/// An interrupt decoded from a remote command, to apply on the local backend by
/// aborting the session's in-flight turn. See
/// [`Dave::poll_remote_conversation_actions`].
pub(crate) struct InterruptApply {
    pub(crate) backend_sid: String,
    pub(crate) backend_type: BackendType,
}

/// The backend applications a poll of remote conversation actions produces:
/// permission-mode changes and interrupts destined for the local CLI backend.
#[derive(Default)]
pub(crate) struct RemoteActionApplies {
    pub(crate) mode_changes: Vec<ModeApply>,
    pub(crate) interrupts: Vec<InterruptApply>,
}

impl Dave {
    /// Poll for remote conversation actions arriving via nostr relays.
    ///
    /// Dispatches kind-1988 events by `role` tag:
    /// - `permission_response`: route through oneshot channel (first-response-wins)
    /// - `set_permission_mode`: apply mode change locally
    /// - `interrupt`: abort the session's in-flight turn on the local backend
    ///
    /// Returns the backend applications (mode changes and interrupts) that the
    /// caller forwards to the local CLI backend.
    pub(crate) fn poll_remote_conversation_actions(
        &mut self,
        ndb: &nostrdb::Ndb,
    ) -> RemoteActionApplies {
        let mut applies = RemoteActionApplies::default();
        let Some(account) = self.pns_local_state.as_ref().map(|state| state.account) else {
            return applies;
        };
        let Some(sub) = self.conversation_action_sub else {
            return applies;
        };

        let note_keys = ndb.poll_for_notes(sub, 256);
        if note_keys.is_empty() {
            return applies;
        }

        // Route each conversation event to its session by `d`-tag. Only local
        // sessions process remote actions, so the index excludes remote ones.
        let by_dtag = self.conversation_session_index(true);

        let txn = match Transaction::new(ndb) {
            Ok(txn) => txn,
            Err(_) => return applies,
        };

        for key in note_keys {
            let Ok(note) = ndb.get_note_by_key(&txn, key) else {
                continue;
            };
            if *note.pubkey() != *account.bytes() {
                continue;
            }
            let Some(session_id) = session_events::get_tag_value(&note, "d")
                .and_then(|dtag| by_dtag.get(dtag).copied())
            else {
                continue;
            };
            let Some(session) = self.session_manager.get_mut(session_id) else {
                continue;
            };
            let Some(agentic) = &mut session.agentic else {
                continue;
            };

            match session_events::get_tag_value(&note, "role") {
                Some("permission_response") => {
                    handle_remote_permission_response(&note, agentic, &mut session.chat);
                }
                Some("set_permission_mode") => {
                    let content = note.content();
                    let mode_str = match serde_json::from_str::<serde_json::Value>(content) {
                        Ok(v) => v
                            .get("mode")
                            .and_then(|m| m.as_str())
                            .unwrap_or("default")
                            .to_string(),
                        Err(_) => continue,
                    };

                    let new_mode = crate::session::permission_mode_from_str(&mode_str);
                    agentic.permission_mode = new_mode;
                    session.state_dirty = true;

                    applies.mode_changes.push(ModeApply {
                        backend_sid: format!("dave-session-{}", session_id),
                        backend_type: session.backend_type,
                        mode: new_mode,
                    });

                    tracing::info!(
                        "remote command: set permission mode to {:?} for session {}",
                        new_mode,
                        session_id,
                    );
                }
                Some("interrupt") => {
                    applies.interrupts.push(InterruptApply {
                        backend_sid: format!("dave-session-{}", session_id),
                        backend_type: session.backend_type,
                    });

                    tracing::info!("remote command: interrupt session {}", session_id);
                }
                _ => {}
            }
        }
        applies
    }

    /// Map each session's live-event `d`-tag (its `event_session_id`) to the
    /// session id, so a shared conversation subscription can route polled notes
    /// to the right session. `local_only` drops remote sessions (used by the
    /// action consumer, which only applies actions to local sessions).
    fn conversation_session_index(&self, local_only: bool) -> HashMap<String, SessionId> {
        let mut index = HashMap::new();
        for session_id in self.session_manager.session_ids() {
            let Some(session) = self.session_manager.get(session_id) else {
                continue;
            };
            if local_only && session.is_remote() {
                continue;
            }
            if let Some(agentic) = session.agentic.as_ref() {
                index.insert(agentic.event_session_id().to_string(), session_id);
            }
        }
        index
    }

    /// Poll for new kind-1988 conversation events.
    ///
    /// For remote sessions: process all roles (user, assistant, tool_call, etc.)
    /// to keep the phone UI in sync with the desktop's conversation.
    ///
    /// For local sessions: only process `role=user` messages arriving from
    /// remote clients (phone), collecting them for backend dispatch.
    /// Poll freshly-arrived conversation events and route them to their
    /// sessions.
    ///
    /// `fan_out_keys` is an out-param collecting the **wrapping PNS envelope**
    /// (kind-1080) note key of each account-authored conversation note seen this
    /// poll, resolved from the unwrapped rumor via
    /// [`rumor_giftwrap_id`](nostrdb::Note::rumor_giftwrap_id). The caller fans
    /// these out to the account's private relays: an event injected by the
    /// `agentium` CLI lands only on the local embedded relay (its default publish
    /// target), so without this re-broadcast the host renders it but the
    /// account's *other* devices never receive it. The envelope, not the
    /// plaintext rumor, is the sync unit (and the rumor would be dropped by
    /// [`fan_out_unseen_notes`](notedeck::fan_out_unseen_notes)'s `is_rumor`
    /// guard, never leaking cleartext).
    pub(crate) fn poll_remote_conversation_events(
        &mut self,
        ndb: &nostrdb::Ndb,
        secret_key: Option<&[u8; 32]>,
        fan_out_keys: &mut Vec<NoteKey>,
    ) -> Vec<(SessionId, String)> {
        let mut remote_user_messages: Vec<(SessionId, String)> = Vec::new();
        let mut rebuild_ids: Vec<SessionId> = Vec::new();
        let Some(account) = self.pns_local_state.as_ref().map(|state| state.account) else {
            return remote_user_messages;
        };
        let Some(sub) = self.conversation_sub else {
            return remote_user_messages;
        };

        let note_keys = ndb.poll_for_notes(sub, 256);
        if note_keys.is_empty() {
            return remote_user_messages;
        }

        // Route each polled conversation event to its session by `d`-tag. Both
        // local and remote sessions consume conversation events, so the index
        // keeps remote sessions too.
        let by_dtag = self.conversation_session_index(false);

        let txn = match Transaction::new(ndb) {
            Ok(txn) => txn,
            Err(_) => return remote_user_messages,
        };

        // Group polled notes by their target session, preserving arrival order
        // within each session so `process_conversation_notes` sees a coherent
        // batch.
        let mut by_session: HashMap<SessionId, Vec<nostrdb::NoteKey>> = HashMap::new();
        for key in note_keys {
            let Ok(note) = ndb.get_note_by_key(&txn, key) else {
                continue;
            };
            if *note.pubkey() != *account.bytes() {
                continue;
            }
            let session_id = session_events::get_tag_value(&note, "d")
                .and_then(|dtag| by_dtag.get(dtag).copied());

            // Collect the wrapping envelope for the caller's outbound fan-out
            // (see the doc on this fn), but only for a note we did not publish
            // ourselves. Notes dave builds ride `pending_relay_events` and are
            // marked in `seen_note_ids` at build time, so skipping them avoids a
            // second, redundant envelope on the relay; a note absent from the set
            // reached ndb some other way — chiefly an agentium-CLI injection into
            // the local embedded relay, which nothing else propagates. A note
            // whose session isn't materialized here can't be matched against a
            // dedup set, so fan it out (it isn't one of ours).
            let self_published = session_id
                .and_then(|sid| self.session_manager.get(sid))
                .and_then(|s| s.agentic.as_ref())
                .is_some_and(|a| a.seen_note_ids.contains(note.id()));
            if !self_published {
                if let Some(gw_id) = note.rumor_giftwrap_id() {
                    if let Ok(gw_key) = ndb.get_notekey_by_id(&txn, gw_id) {
                        fan_out_keys.push(gw_key);
                    }
                }
            }

            let Some(session_id) = session_id else {
                continue;
            };
            by_session.entry(session_id).or_default().push(key);
        }

        for (session_id, keys) in by_session {
            let Some(session) = self.session_manager.get_mut(session_id) else {
                continue;
            };
            let is_remote = session.is_remote();
            let notes: Vec<_> = keys
                .iter()
                .filter_map(|key| ndb.get_note_by_key(&txn, *key).ok())
                .collect();

            let result =
                process_conversation_notes(notes, session, session_id, is_remote, secret_key, ndb);
            remote_user_messages.extend(result.remote_user_messages);
            if result.rebuild_chat {
                rebuild_ids.push(session_id);
            }
        }

        // Drop the read txn before the rebuild pass, which opens its own fresh
        // transaction per session (avoids nested transactions).
        drop(txn);

        // A new displayable note landed for each of these remote sessions:
        // rebuild each chat from ndb in sorted order. This is the single display
        // path for remote sessions, so the result is independent of arrival/poll
        // order. Done after the poll loop so each rebuild uses a fresh
        // transaction (no nested txns).
        for session_id in rebuild_ids {
            let Ok(txn) = Transaction::new(ndb) else {
                continue;
            };
            let Some(session) = self.session_manager.get_mut(session_id) else {
                continue;
            };
            rebuild_remote_chat(session, ndb, &txn, &account);
            tracing::debug!(
                "rebuilt remote session {} chat from ndb ({} messages)",
                session_id,
                session.chat.len(),
            );
        }

        remote_user_messages
    }
}

/// Subscribe to every kind-1988 conversation event authored by `account`.
///
/// This is the shared, session-independent subscription that replaces the old
/// per-session (kind + author + `d`-tag) subscriptions: callers poll it once and
/// demux notes to the owning session by their `d`-tag. Returns `None` if nostrdb
/// refuses the subscription (e.g. cap reached), matching the old warn-and-skip
/// behavior.
pub(crate) fn subscribe_conversation_events(
    ndb: &nostrdb::Ndb,
    account: nostrdb_net::Pubkey,
) -> Option<nostrdb::Subscription> {
    let filter = nostrdb::Filter::new()
        .kinds([session_events::AI_CONVERSATION_KIND as u64])
        .authors([account.bytes()])
        .build();
    match ndb.subscribe(&[filter]) {
        Ok(sub) => Some(sub),
        Err(e) => {
            tracing::warn!("failed to subscribe for conversation events: {:?}", e);
            None
        }
    }
}

/// Result of processing a batch of conversation notes.
pub(crate) struct ProcessedNotes {
    /// User messages received from remote clients (for local sessions).
    pub remote_user_messages: Vec<(SessionId, String)>,
    /// True if this batch needs the caller to rebuild the remote session's chat
    /// from ndb (see [`rebuild_remote_chat`]) — set only on the slow path, when
    /// a new displayable note sorts at or before what's already shown. In-order
    /// notes are appended directly here and do NOT set this.
    pub rebuild_chat: bool,
}

/// Process a batch of kind-1988 notes for a single session.
///
/// Deduplicates via `seen_note_ids` and runs the side effects each note implies
/// (permission auto-accept + response tracking, compaction lifecycle,
/// proceed-after-compaction). Returns any remote user messages (for local
/// sessions) and events to publish.
///
/// For **remote** sessions display order must be a pure function of the
/// persisted event set. Two paths keep that guarantee:
/// - **fast path** — when every new displayable note in the batch sorts after
///   `agentic.tail_order` (what's already shown), they are appended in order via
///   [`render_conversation_note`](session_loader::render_conversation_note), the
///   *same* renderer the loader uses, so the result is byte-identical to a
///   rebuild. O(batch).
/// - **slow path** — any note at or before the tail (out-of-order relay
///   delivery / a fresh-machine backfill) sets `rebuild_chat`; the caller
///   reloads the whole chat from ndb sorted by
///   [`EventOrder`](session_loader::EventOrder), which reseeds `tail_order`.
///
/// `tail_order` is seeded from the loader on every rebuild; when `None` (never
/// loaded) the batch conservatively takes the slow path, so a missed seeding
/// can only cost an extra rebuild, never misorder.
///
/// For **local** sessions only incoming remote user messages are appended (the
/// live streaming path owns local display); those are never rebuilt from ndb.
pub(crate) fn process_conversation_notes<'a>(
    mut notes: Vec<nostrdb::Note<'a>>,
    session: &mut session::ChatSession,
    session_id: SessionId,
    is_remote: bool,
    secret_key: Option<&[u8; 32]>,
    ndb: &nostrdb::Ndb,
) -> ProcessedNotes {
    let mut remote_user_messages: Vec<(SessionId, String)> = Vec::new();
    let mut rebuild_chat = false;
    // Newest `created_at` of a displayable remote note in this batch, applied
    // to `last_activity` after the loop (can't call `session.mark_activity`
    // inside — `session.agentic` is mutably borrowed below).
    let mut latest_activity: Option<u64> = None;
    // Indices (into the sorted `notes`) of new displayable remote notes, decided
    // into an append or a rebuild after the side-effect pass below.
    let mut new_display_idxs: Vec<usize> = Vec::new();

    // Sort this batch by wall-clock time at millisecond resolution, keyed off
    // the same `EventOrder` the loader uses. For remote sessions display order
    // ultimately comes from the loader-driven rebuild, so this sort only matters
    // for the local-session user-message append below; for remote it keeps the
    // side-effect processing (compaction lifecycle) in a sensible order.
    notes.sort_by_key(|n| session_loader::EventOrder::from_note(n));

    for (idx, note) in notes.iter().enumerate() {
        // Skip events we've already processed (dedup)
        let note_id = *note.id();
        let dominated = session
            .agentic
            .as_mut()
            .map(|a| !a.seen_note_ids.insert(note_id))
            .unwrap_or(true);
        if dominated {
            continue;
        }

        let content = note.content();
        let role = session_events::get_tag_value(note, "role");

        // Local sessions: only process incoming user messages from remote clients
        if !is_remote {
            if role == Some("user") {
                tracing::info!("received remote user message for local session");
                session.chat.push(Message::User(content.to_string().into()));
                session.update_title_from_last_message();
                remote_user_messages.push((session_id, content.to_string()));
            }
            continue;
        }

        let Some(agentic) = &mut session.agentic else {
            continue;
        };

        // Collect newly-seen displayable notes; after the side-effect pass they
        // are either appended in order (fast path) or trigger a rebuild.
        let displayable = matches!(
            role,
            Some("user")
                | Some("assistant")
                | Some("tool_call")
                | Some("tool_result")
                | Some("permission_request")
                // A permission_response renders only when it carries a
                // user-authored reply (render_conversation_note drops the
                // rest); listing it here lets that reply append on the live
                // remote path instead of waiting for a full rebuild.
                | Some("permission_response")
                | Some("compaction_complete")
        );
        if displayable {
            let created_at = note.created_at();
            latest_activity = Some(latest_activity.map_or(created_at, |p| p.max(created_at)));
            new_display_idxs.push(idx);
        }

        // Side effects only — display is rebuilt from ndb by the caller. The
        // arms below run effects that a reload can't recover (publishing
        // responses, advancing compaction state) or that are order-neutral
        // in-place updates (marking a permission responded).
        match role {
            Some("permission_request") => {
                handle_remote_permission_request(note, content, agentic, secret_key, ndb);
            }
            Some("permission_response") => {
                // Track that this permission was responded to, and reflect it on
                // the existing chat message in place (order-neutral) so a lone
                // response with no displayable note in the batch still updates.
                if let Some(perm_id_str) = session_events::get_tag_value(note, "perm-id") {
                    if let Ok(perm_id) = uuid::Uuid::parse_str(perm_id_str) {
                        let decoded = session_events::decode_permission_response(content);
                        let response_type = decoded.response_type;
                        agentic.permissions.responded.insert(
                            perm_id,
                            crate::messages::PermissionDecision {
                                response: response_type,
                                auto_accepted: decoded.auto_accepted,
                            },
                        );
                        for msg in session.chat.iter_mut() {
                            if let Message::PermissionRequest(req) = msg {
                                if req.id == perm_id && req.response.is_none() {
                                    req.response = Some(response_type);
                                    req.auto_accepted = decoded.auto_accepted;
                                }
                            }
                        }
                    }
                }
            }
            Some("compaction_started") if agentic.compact_intent.is_none() => {
                agentic.compact_intent = Some(session::CompactIntent::Manual);
            }
            Some("compaction_complete") => {
                let pre_tokens = content.parse::<u64>().unwrap_or(0);
                agentic.last_compaction = Some(crate::messages::CompactionInfo { pre_tokens });

                // Advance compact-and-proceed: for remote sessions,
                // there's no stream-end to wait for, so go straight
                // to ReadyToProceed and consume immediately.
                match agentic.compact_intent {
                    Some(session::CompactIntent::ProceedAfterCompaction) => {
                        agentic.compact_intent = Some(session::CompactIntent::ReadyToProceed);
                    }
                    _ => {
                        agentic.compact_intent = None;
                    }
                }
            }
            _ => {
                // Skip progress, queue-operation, etc.
            }
        }

        // Handle proceed after compaction for remote sessions. Ingested locally;
        // the host's private-sync Session fans it out so the desktop backend
        // picks it up.
        if session.take_compact_and_proceed() {
            if let Some(sk) = secret_key {
                ingest_live_event(
                    session,
                    ndb,
                    sk,
                    "Proceed with implementing the plan.",
                    "user",
                    None,
                    None,
                );
            }
        }
    }

    // Reflect the new displayable notes. Fast path: if they all sort after
    // what's already shown (`tail_order`), append them in order using the same
    // renderer the loader uses — byte-identical to a rebuild, O(batch). Slow
    // path (any note at/before the tail, or an unseeded tail): flag a rebuild.
    if let (false, Some(agentic)) = (new_display_idxs.is_empty(), &mut session.agentic) {
        let min_new = session_loader::EventOrder::from_note(&notes[new_display_idxs[0]]);
        let appendable = matches!(agentic.tail_order, Some(tail) if min_new > tail);
        if appendable {
            for &i in &new_display_idxs {
                if let Some(msg) = session_loader::render_conversation_note(
                    &notes[i],
                    &agentic.permissions.responded,
                ) {
                    session.chat.push(msg);
                }
            }
            let last = *new_display_idxs.last().expect("non-empty");
            agentic.tail_order = Some(session_loader::EventOrder::from_note(&notes[last]));
        } else {
            rebuild_chat = true;
        }
    }

    // Remote sessions never hit the local `append_token` path, so drive the
    // status-bar "last activity" indicator off the newest ingested note.
    if let Some(ts) = latest_activity {
        session.mark_activity(ts);
    }

    ProcessedNotes {
        remote_user_messages,
        rebuild_chat,
    }
}

/// Rebuild a remote session's chat from ndb — the single source of truth for
/// remote conversation display order.
///
/// Loads every kind-1988 event for the session sorted by
/// [`EventOrder`](session_loader::EventOrder) and replaces `session.chat`, so
/// the displayed order is a pure, total function of the persisted event set,
/// independent of the order events arrived or were ingested (the fresh-machine
/// backfill case). Re-seeds the dedup set and permission state, then overlays
/// any in-memory permission decisions the loader couldn't know from ndb — an
/// auto-accept published this poll but not yet ingested back through the relay.
pub(crate) fn rebuild_remote_chat(
    session: &mut session::ChatSession,
    ndb: &nostrdb::Ndb,
    txn: &Transaction,
    author: &nostrdb_net::Pubkey,
) {
    let Some(claude_sid) = session
        .agentic
        .as_ref()
        .map(|a| a.event_session_id().to_string())
    else {
        return;
    };
    let loaded = session_loader::load_session_messages_for_author(ndb, txn, author, &claude_sid);
    session.chat = loaded.messages;

    let Some(agentic) = &mut session.agentic else {
        return;
    };
    agentic.seen_note_ids.extend(loaded.note_ids);
    // Seed the fast-path tail from the freshly loaded set: subsequent in-order
    // notes can then append instead of forcing another rebuild.
    agentic.tail_order = loaded.max_order;
    agentic.permissions.merge_loaded(loaded.permissions);

    // Overlay in-memory permission decisions onto the freshly loaded chat. The
    // loader only knows responses persisted in ndb, so an auto-accept recorded
    // this poll (its response event published but not yet ingested) would render
    // as pending — and collapsed — without this.
    for msg in session.chat.iter_mut() {
        let Message::PermissionRequest(req) = msg else {
            continue;
        };
        if let Some(&decision) = agentic.permissions.responded.get(&req.id) {
            if req.response.is_none() {
                req.response = Some(decision.response);
            }
            req.auto_accepted = decision.auto_accepted;
        }
    }
}

/// Handle a remote permission request from a kind-1988 conversation event.
///
/// Runs only the side effects — records the request note id and, if the runtime
/// allowlist auto-accepts, records the response and publishes it. The chat
/// message itself is rendered by the loader on the caller's rebuild (with the
/// in-memory `responded` overlay), so this never appends to chat.
fn handle_remote_permission_request(
    note: &nostrdb::Note,
    content: &str,
    agentic: &mut session::AgenticSessionData,
    secret_key: Option<&[u8; 32]>,
    ndb: &nostrdb::Ndb,
) {
    let Ok(content_json) = serde_json::from_str::<serde_json::Value>(content) else {
        return;
    };
    let tool_name = content_json["tool_name"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    let tool_input = content_json
        .get("tool_input")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let perm_id = session_events::get_tag_value(note, "perm-id")
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .unwrap_or_else(uuid::Uuid::new_v4);

    // Store the note ID for linking responses
    agentic
        .permissions
        .request_note_ids
        .insert(perm_id, *note.id());

    // Runtime allowlist auto-accept
    if !agentic.should_runtime_allow(&tool_name, &tool_input) {
        return;
    }

    tracing::info!(
        "runtime allow: auto-accepting remote '{}' for this session",
        tool_name,
    );
    // Record the decision in memory so the rebuild overlay renders it as allowed
    // (and expanded) even before the ingested response round-trips back through
    // the relay.
    agentic.permissions.responded.insert(
        perm_id,
        crate::messages::PermissionDecision {
            response: crate::messages::PermissionResponseType::Allowed,
            auto_accepted: true,
        },
    );
    if let Some(sk) = secret_key {
        let sid = agentic.event_session_id().to_string();
        if let Ok(evt) = session_events::build_permission_response_event(
            &perm_id,
            note.id(),
            true,
            None,
            false,
            true,
            &sid,
            &mut agentic.live_threading,
            sk,
        ) {
            // Ingest locally; the host's private-sync Session fans the envelope
            // out to the relay so the remote backend sees the auto-accept.
            pns_ingest(ndb, &evt.note_json, sk);
        }
    }
}

/// Handle a remote permission response from a kind-1988 event.
fn handle_remote_permission_response(
    note: &nostrdb::Note,
    agentic: &mut session::AgenticSessionData,
    chat: &mut [Message],
) {
    let Some(perm_id_str) = session_events::get_tag_value(note, "perm-id") else {
        tracing::warn!("permission_response event missing perm-id tag");
        return;
    };
    let Ok(perm_id) = uuid::Uuid::parse_str(perm_id_str) else {
        tracing::warn!("invalid perm-id UUID: {}", perm_id_str);
        return;
    };

    let decoded = session_events::decode_permission_response(note.content());
    let message = decoded.message;
    let cancel_turn = decoded.cancel_turn;
    let allowed = decoded.response_type == crate::messages::PermissionResponseType::Allowed;

    if let Some(sender) = agentic.permissions.pending.remove(&perm_id) {
        let response = if allowed {
            PermissionResponse::Allow { message }
        } else if cancel_turn {
            PermissionResponse::Cancel {
                reason: message.unwrap_or_else(|| messages::DEFAULT_REMOTE_EXIT_REASON.to_string()),
            }
        } else {
            PermissionResponse::Deny {
                reason: message.unwrap_or_else(|| messages::DEFAULT_REMOTE_DENY_REASON.to_string()),
            }
        };
        for msg in chat.iter_mut() {
            if let Message::PermissionRequest(req) = msg {
                if req.id == perm_id {
                    req.response = Some(decoded.response_type);
                    req.auto_accepted = decoded.auto_accepted;
                    break;
                }
            }
        }

        if sender.send(response).is_err() {
            tracing::warn!("failed to send remote permission response for {}", perm_id);
        } else {
            tracing::info!(
                "remote permission response for {}: {}",
                perm_id,
                if allowed { "allowed" } else { "denied" }
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiMode;
    use crate::session::SessionSource;
    use crate::session_events::{build_live_event, build_permission_request_event, ThreadingState};
    use crate::tests::{test_config, test_secret_key};
    use nostrdb::{IngestMetadata, Ndb, Transaction};

    use std::path::PathBuf;

    use tempfile::TempDir;

    /// Outbound fan-out primitive (headway:dave/light-hobby-upgrade): a
    /// conversation note injected by the `agentium` CLI reaches only the local
    /// embedded relay, so the host re-broadcasts its *wrapping* PNS envelope to
    /// the account's private relays. This guards the mechanism that makes that
    /// possible — an unwrapped rumor resolves to its kind-1080 envelope note key,
    /// and that envelope is a non-rumor note that
    /// [`notedeck::fan_out_unseen_notes`] will actually publish (it skips the
    /// plaintext rumor via its `is_rumor` guard, so the inner note must not be
    /// what we fan out).
    #[tokio::test]
    async fn conversation_rumor_resolves_to_fannable_envelope() {
        let sk = test_secret_key();
        let mut threading = ThreadingState::new();
        let user_evt = build_live_event(
            "hello from the CLI",
            "user",
            "envelope-fanout-test",
            None,
            None,
            None,
            &mut threading,
            &sk,
        )
        .unwrap();

        let tmp = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp.path().to_str().unwrap(), &test_config()).unwrap();
        // Register the account key so ndb's ingester unwraps the kind-1080
        // envelope into its inner rumor (mirrors how notedeck configures ndb).
        assert!(ndb.add_key(&sk), "ndb must accept the giftwrap key");

        // Ingest the PNS-wrapped note exactly as it arrives over a relay and wait
        // for the ingester to produce the inner rumor.
        let sub = ndb
            .subscribe(&[nostrdb::Filter::new()
                .kinds([session_events::AI_CONVERSATION_KIND as u64])
                .build()])
            .unwrap();
        pns_ingest(&ndb, &user_evt.note_json, &sk);
        let keys = ndb.wait_for_notes(sub, 1).await.unwrap();

        let txn = Transaction::new(&ndb).unwrap();
        let inner = ndb.get_note_by_key(&txn, keys[0]).unwrap();
        // The inner note is a rumor: fanning it out would leak plaintext, and
        // `fan_out_unseen_notes` deliberately skips it — which is exactly why the
        // fix fans out the envelope instead.
        assert!(inner.is_rumor(), "unwrapped conversation note is a rumor");
        assert_eq!(inner.content(), "hello from the CLI");

        // Resolve the wrapping envelope the fix collects and fans out.
        let gw_id = inner
            .rumor_giftwrap_id()
            .expect("rumor carries its giftwrap id");
        let gw_key = ndb
            .get_notekey_by_id(&txn, gw_id)
            .expect("wrapping envelope is in ndb");
        let envelope = ndb.get_note_by_key(&txn, gw_key).unwrap();
        assert_eq!(
            u64::from(envelope.kind()),
            nostrdb_net::pns::PNS_KIND as u64,
            "the resolved envelope is the kind-1080 PNS wrapper"
        );
        assert!(
            !envelope.is_rumor(),
            "the envelope is a real note fan_out_unseen_notes will publish"
        );
    }

    /// The selected account's pubkey alongside the author pubkeys of every
    /// note the shared conversation subscription matched.
    struct ConversationSubAuthors {
        account: [u8; 32],
        matched: Vec<[u8; 32]>,
    }

    async fn conversation_subscription_author_pubkeys() -> ConversationSubAuthors {
        let account = nostrdb_net::FullKeypair::generate();
        let other_account = nostrdb_net::FullKeypair::generate();
        let account_pubkey = *account.pubkey.bytes();
        let session_id_str = "same-d-live-scope";
        let mut account_threading = ThreadingState::new();
        let mut other_threading = ThreadingState::new();

        let account_event = build_live_event(
            "account event",
            "user",
            session_id_str,
            None,
            None,
            None,
            &mut account_threading,
            &account.secret_key.secret_bytes(),
        )
        .expect("account live event");
        let other_event = build_live_event(
            "other event",
            "user",
            session_id_str,
            None,
            None,
            None,
            &mut other_threading,
            &other_account.secret_key.secret_bytes(),
        )
        .expect("other live event");

        let tmp_dir = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp_dir.path().to_str().unwrap(), &test_config()).unwrap();
        let sub =
            subscribe_conversation_events(&ndb, account.pubkey).expect("conversation subscription");

        ndb.process_event_with(
            &other_event.to_event_json(),
            IngestMetadata::new().client(true),
        )
        .expect("ingest other event");
        ndb.process_event_with(
            &account_event.to_event_json(),
            IngestMetadata::new().client(true),
        )
        .expect("ingest account event");

        let mut keys = ndb
            .wait_for_notes(sub, 1)
            .await
            .expect("subscription notes");
        keys.extend(ndb.poll_for_notes(sub, 16));
        let txn = Transaction::new(&ndb).expect("txn");
        let pubkeys = keys
            .iter()
            .map(|key| *ndb.get_note_by_key(&txn, *key).expect("note").pubkey())
            .collect();
        ConversationSubAuthors {
            account: account_pubkey,
            matched: pubkeys,
        }
    }

    #[tokio::test]
    async fn conversation_subscription_filters_selected_account_author() {
        let authors = conversation_subscription_author_pubkeys().await;

        assert_eq!(
            authors.matched,
            vec![authors.account],
            "same-d events from another account must not match the conversation subscription"
        );
    }

    /// Every `Message::Assistant` body in a chat, in order — for asserting the
    /// rebuilt remote transcript's ordering by content.
    fn assistant_texts(chat: &[Message]) -> Vec<&str> {
        chat.iter()
            .filter_map(|m| match m {
                Message::Assistant(a) => Some(a.text()),
                _ => None,
            })
            .collect()
    }

    /// Integration test for the remote conversation display path: events
    /// ingested out of order into ndb produce a correctly ordered chat after
    /// the loader-driven rebuild (`rebuild_remote_chat`), the single ordering
    /// source `poll_remote_conversation_events` uses.
    #[tokio::test]
    async fn test_process_conversation_notes_ordering() {
        let sk = test_secret_key();
        let author = nostrdb_net::FullKeypair::from_secret_bytes(&sk)
            .unwrap()
            .pubkey;
        let mut threading = ThreadingState::new();
        let session_id_str = "poll-ordering-test";

        // Build events: tool_call (seq=0), permission_request (seq=1), tool_result (seq=2)
        let tool_call_evt = build_live_event(
            r#"{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}}"#,
            "tool_call",
            session_id_str,
            None,
            Some("toolu_1"),
            Some("Bash"),
            &mut threading,
            &sk,
        )
        .unwrap();

        let perm_id = uuid::Uuid::new_v4();
        let perm_evt = build_permission_request_event(
            &perm_id,
            "Bash",
            &serde_json::json!({"command": "rm -rf /tmp/test"}),
            session_id_str,
            &mut threading,
            &sk,
        )
        .unwrap();

        let tool_result_evt = build_live_event(
            "file1.txt\nfile2.txt",
            "tool_result",
            session_id_str,
            None,
            Some("toolu_1"),
            Some("Bash"),
            &mut threading,
            &sk,
        )
        .unwrap();

        // Set up ndb
        let tmp_dir = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp_dir.path().to_str().unwrap(), &test_config()).unwrap();

        let filter = nostrdb::Filter::new()
            .kinds([session_events::AI_CONVERSATION_KIND as u64])
            .build();

        // Ingest in REVERSED order to simulate out-of-order relay delivery
        for event in [&tool_result_evt, &perm_evt, &tool_call_evt] {
            let sub = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
            ndb.process_event_with(&event.to_event_json(), IngestMetadata::new().client(true))
                .expect("ingest failed");
            let _keys = ndb.wait_for_notes(sub, 1).await.unwrap();
        }

        // Create a remote agentic session whose event identity matches the
        // events' `d`-tag, so the rebuild loader can find them.
        let mut session = session::ChatSession::new(
            1,
            PathBuf::from("/tmp"),
            AiMode::Agentic,
            BackendType::Claude,
        );
        session.source = SessionSource::Remote;
        session.agentic.as_mut().unwrap().event_id = session_id_str.to_string();

        // First poll: process the batch (side effects + rebuild flag), then
        // rebuild the chat from ndb exactly as the caller does.
        {
            let txn = Transaction::new(&ndb).unwrap();
            let results = ndb.query(&txn, std::slice::from_ref(&filter), 128).unwrap();
            let notes: Vec<_> = results
                .iter()
                .filter_map(|qr| ndb.get_note_by_key(&txn, qr.note_key).ok())
                .collect();
            assert_eq!(notes.len(), 3, "should have 3 events in ndb");

            // Remote sessions never hit `append_token`, so last_activity must be
            // driven off the newest ingested note's wall-clock `created_at`.
            let newest_created_at = notes.iter().map(|n| n.created_at()).max();
            assert_eq!(session.last_activity, None, "starts unset");

            let result = process_conversation_notes(
                notes,
                &mut session,
                1,
                true, // is_remote
                Some(&sk),
                &ndb,
            );

            assert!(result.remote_user_messages.is_empty());
            assert!(
                result.rebuild_chat,
                "a batch of new displayable notes must request a rebuild"
            );
            assert_eq!(
                session.last_activity, newest_created_at,
                "last_activity should track the newest ingested note's created_at"
            );

            rebuild_remote_chat(&mut session, &ndb, &txn, &author);
        }

        // Assert correct ordering in the rebuilt chat
        assert_eq!(
            session.chat.len(),
            3,
            "should have 3 chat messages, got {}",
            session.chat.len()
        );
        assert!(
            matches!(&session.chat[0], Message::Assistant(_)),
            "chat[0] should be Assistant (tool_call)",
        );
        assert!(
            matches!(&session.chat[1], Message::PermissionRequest(_)),
            "chat[1] should be PermissionRequest",
        );
        assert!(
            matches!(&session.chat[2], Message::ToolResponse(_)),
            "chat[2] should be ToolResponse (tool_result)",
        );

        // Verify permission request has correct tool name
        if let Message::PermissionRequest(req) = &session.chat[1] {
            assert_eq!(req.tool_name, "Bash");
            assert_eq!(req.id, perm_id);
        }

        // Second poll of the same events: all already seen, so no rebuild is
        // requested and the chat is unchanged (dedup).
        {
            let txn = Transaction::new(&ndb).unwrap();
            let results = ndb.query(&txn, &[filter], 128).unwrap();
            let notes: Vec<_> = results
                .iter()
                .filter_map(|qr| ndb.get_note_by_key(&txn, qr.note_key).ok())
                .collect();

            let result = process_conversation_notes(notes, &mut session, 1, true, Some(&sk), &ndb);
            assert!(
                !result.rebuild_chat,
                "already-seen notes must not request a rebuild"
            );
        }
        assert_eq!(
            session.chat.len(),
            3,
            "dedup should prevent duplicate messages"
        );
    }

    /// Fresh-machine regression (dave#pledge-grief-close): on a machine that
    /// rebuilt ndb from a negentropy backfill, a session's events arrive in
    /// arbitrary order across polls. An early-order event that backfills *after*
    /// the initial load must still land in its correct position.
    ///
    /// The old path appended live notes incrementally and relied on a
    /// `max_seen_order` detector to trigger a rebuild on inversion — but that
    /// detector was never seeded from the initial load, so on a fresh machine
    /// the first backfilled event that belonged mid-list was appended at the end
    /// and never noticed, leaving the chat permanently misordered. The single
    /// loader-driven rebuild path (`rebuild_remote_chat`) is order-independent by
    /// construction: `process_conversation_notes` never appends display for
    /// remote sessions, it only flags that a rebuild is needed.
    #[tokio::test]
    async fn fresh_machine_backfill_rebuilds_in_order() {
        let sk = test_secret_key();
        let author = nostrdb_net::FullKeypair::from_secret_bytes(&sk)
            .unwrap()
            .pubkey;
        let mut threading = ThreadingState::new();
        let session_id_str = "backfill-test";

        // Authored order A < B < C (increasing seq, and non-decreasing ms).
        let mut mk = |text: &str| {
            build_live_event(
                text,
                "assistant",
                session_id_str,
                None,
                None,
                None,
                &mut threading,
                &sk,
            )
            .unwrap()
        };
        let a = mk("A");
        let b = mk("B");
        let c = mk("C");

        let tmp_dir = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp_dir.path().to_str().unwrap(), &test_config()).unwrap();
        let filter = nostrdb::Filter::new()
            .kinds([session_events::AI_CONVERSATION_KIND as u64])
            .build();

        let ingest = |ndb: &Ndb, evt: &session_events::BuiltEvent| {
            let sub = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
            ndb.process_event_with(&evt.to_event_json(), IngestMetadata::new().client(true))
                .expect("ingest failed");
            sub
        };

        // Initial backfill delivered only the middle and last events (A hasn't
        // arrived yet).
        for event in [&b, &c] {
            let sub = ingest(&ndb, event);
            let _ = ndb.wait_for_notes(sub, 1).await.unwrap();
        }

        let mut session = session::ChatSession::new(
            1,
            PathBuf::from("/tmp"),
            AiMode::Agentic,
            BackendType::Claude,
        );
        session.source = SessionSource::Remote;
        session.agentic.as_mut().unwrap().event_id = session_id_str.to_string();

        // Initial load populates the chat with what's present so far: [B, C].
        {
            let txn = Transaction::new(&ndb).unwrap();
            rebuild_remote_chat(&mut session, &ndb, &txn, &author);
        }
        assert_eq!(
            assistant_texts(&session.chat),
            vec!["B", "C"],
            "initial load has only the backfilled-so-far events"
        );

        // A backfills late; a subsequent poll delivers it on its own.
        {
            let sub = ingest(&ndb, &a);
            let _ = ndb.wait_for_notes(sub, 1).await.unwrap();
        }
        {
            let txn = Transaction::new(&ndb).unwrap();
            let a_batch: Vec<_> = ndb
                .query(&txn, std::slice::from_ref(&filter), 128)
                .unwrap()
                .iter()
                .filter_map(|qr| ndb.get_note_by_key(&txn, qr.note_key).ok())
                .filter(|n| n.content() == "A")
                .collect();
            assert_eq!(a_batch.len(), 1, "the poll batch is just A");

            let result =
                process_conversation_notes(a_batch, &mut session, 1, true, Some(&sk), &ndb);
            assert!(
                result.rebuild_chat,
                "an out-of-order backfill note (before the tail) must take the \
                 slow path and request a rebuild"
            );
            // The out-of-order note is not appended (that would misorder); the
            // chat is left for the rebuild to fix.
            assert_eq!(
                assistant_texts(&session.chat),
                vec!["B", "C"],
                "an out-of-order note must not be appended"
            );
            rebuild_remote_chat(&mut session, &ndb, &txn, &author);
        }

        // A lands in its correct position despite arriving last.
        assert_eq!(
            assistant_texts(&session.chat),
            vec!["A", "B", "C"],
            "a backfilled early event must sort into place, not append at the end"
        );
    }

    /// Fast path: when a polled note sorts after everything already displayed
    /// (in-order delivery, the common case), it is appended directly — no
    /// rebuild — and the result matches a from-scratch loader rebuild. This is
    /// the O(batch) optimization over always reloading the whole chat.
    #[tokio::test]
    async fn in_order_note_appends_without_rebuild() {
        let sk = test_secret_key();
        let author = nostrdb_net::FullKeypair::from_secret_bytes(&sk)
            .unwrap()
            .pubkey;
        let mut threading = ThreadingState::new();
        let session_id_str = "fast-path-test";

        let mut mk = |text: &str| {
            build_live_event(
                text,
                "assistant",
                session_id_str,
                None,
                None,
                None,
                &mut threading,
                &sk,
            )
            .unwrap()
        };
        let a = mk("A");
        let b = mk("B");

        let tmp_dir = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp_dir.path().to_str().unwrap(), &test_config()).unwrap();
        let filter = nostrdb::Filter::new()
            .kinds([session_events::AI_CONVERSATION_KIND as u64])
            .build();
        let ingest = |ndb: &Ndb, evt: &session_events::BuiltEvent| {
            let sub = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
            ndb.process_event_with(&evt.to_event_json(), IngestMetadata::new().client(true))
                .expect("ingest failed");
            sub
        };

        let mut session = session::ChatSession::new(
            1,
            PathBuf::from("/tmp"),
            AiMode::Agentic,
            BackendType::Claude,
        );
        session.source = SessionSource::Remote;
        session.agentic.as_mut().unwrap().event_id = session_id_str.to_string();

        // Initial load with A only (seeds tail_order at A).
        {
            let sub = ingest(&ndb, &a);
            let _ = ndb.wait_for_notes(sub, 1).await.unwrap();
            let txn = Transaction::new(&ndb).unwrap();
            rebuild_remote_chat(&mut session, &ndb, &txn, &author);
        }
        assert_eq!(assistant_texts(&session.chat), vec!["A"]);

        // B arrives in order; the poll appends it without asking for a rebuild.
        {
            let sub = ingest(&ndb, &b);
            let _ = ndb.wait_for_notes(sub, 1).await.unwrap();
            let txn = Transaction::new(&ndb).unwrap();
            let batch: Vec<_> = ndb
                .query(&txn, std::slice::from_ref(&filter), 128)
                .unwrap()
                .iter()
                .filter_map(|qr| ndb.get_note_by_key(&txn, qr.note_key).ok())
                .filter(|n| n.content() == "B")
                .collect();
            let result = process_conversation_notes(batch, &mut session, 1, true, Some(&sk), &ndb);
            assert!(
                !result.rebuild_chat,
                "an in-order note must be appended, not trigger a rebuild"
            );
        }

        // Appended directly, and identical to what a full rebuild would produce.
        assert_eq!(assistant_texts(&session.chat), vec!["A", "B"]);
        let txn = Transaction::new(&ndb).unwrap();
        let rebuilt =
            session_loader::load_session_messages_for_author(&ndb, &txn, &author, session_id_str);
        assert_eq!(
            assistant_texts(&session.chat),
            assistant_texts(&rebuilt.messages),
            "the fast-path append must match a from-scratch rebuild"
        );
    }

    /// A denied permission_response event must set PermissionResponseType::Denied
    /// on the matching chat PermissionRequest, not hardcode Allowed.
    ///
    /// This test processes events in two passes (simulating real polling):
    /// first the permission_request, then the permission_response. This
    /// ensures the response branch sees an existing pending request in chat.
    #[tokio::test]
    async fn test_permission_response_denied_is_decoded() {
        let sk = test_secret_key();
        let author = nostrdb_net::FullKeypair::from_secret_bytes(&sk)
            .unwrap()
            .pubkey;
        let mut threading = ThreadingState::new();
        let session_id_str = "perm-deny-test";
        let perm_id = uuid::Uuid::new_v4();

        // 1) Build a permission_request event.
        let perm_req_evt = build_permission_request_event(
            &perm_id,
            "Bash",
            &serde_json::json!({"command": "rm -rf /"}),
            session_id_str,
            &mut threading,
            &sk,
        )
        .unwrap();

        // 2) Build a permission_response event with allowed=false (deny).
        let perm_resp_evt = session_events::build_permission_response_event(
            &perm_id,
            &[0u8; 32], // dummy request note id
            false,      // DENIED
            Some("too dangerous"),
            false,
            false, // not auto-accepted
            session_id_str,
            &mut threading,
            &sk,
        )
        .unwrap();

        // Set up ndb
        let tmp_dir = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp_dir.path().to_str().unwrap(), &test_config()).unwrap();

        let filter = nostrdb::Filter::new()
            .kinds([session_events::AI_CONVERSATION_KIND as u64])
            .build();

        // Create a remote agentic session whose event identity matches the
        // events' `d`-tag so the rebuild loader can find them.
        let mut session = session::ChatSession::new(
            1,
            PathBuf::from("/tmp"),
            AiMode::Agentic,
            BackendType::Remote,
        );
        session.source = SessionSource::Remote;
        session.agentic.as_mut().unwrap().event_id = session_id_str.to_string();

        // Pass 1: ingest and process only the permission_request, then rebuild
        // the chat from ndb so it holds a pending PermissionRequest
        // (response=None). The response is not in ndb yet.
        {
            let sub = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
            ndb.process_event_with(
                &perm_req_evt.to_event_json(),
                IngestMetadata::new().client(true),
            )
            .expect("ingest failed");
            let _ = ndb.wait_for_notes(sub, 1).await.unwrap();

            let txn = Transaction::new(&ndb).unwrap();
            let results = ndb.query(&txn, std::slice::from_ref(&filter), 128).unwrap();
            let notes: Vec<_> = results
                .iter()
                .filter_map(|qr| ndb.get_note_by_key(&txn, qr.note_key).ok())
                .collect();
            assert_eq!(notes.len(), 1, "should have 1 permission_request");

            let result = process_conversation_notes(notes, &mut session, 1, true, Some(&sk), &ndb);
            assert!(
                result.rebuild_chat,
                "a permission_request must request a rebuild"
            );
            rebuild_remote_chat(&mut session, &ndb, &txn, &author);
        }

        // Verify the request is pending (response=None)
        let pending = session.chat.iter().find_map(|m| {
            if let Message::PermissionRequest(req) = m {
                Some(req.response)
            } else {
                None
            }
        });
        assert_eq!(
            pending,
            Some(None),
            "request should be pending before response"
        );

        // Pass 2: the denied response arrives on a later poll. It is
        // order-neutral, so `process_conversation_notes` marks the existing chat
        // request in place (no rebuild needed).
        {
            let sub = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
            ndb.process_event_with(
                &perm_resp_evt.to_event_json(),
                IngestMetadata::new().client(true),
            )
            .expect("ingest failed");
            let _ = ndb.wait_for_notes(sub, 1).await.unwrap();

            let txn = Transaction::new(&ndb).unwrap();
            let results = ndb.query(&txn, &[filter], 128).unwrap();
            let notes: Vec<_> = results
                .iter()
                .filter_map(|qr| ndb.get_note_by_key(&txn, qr.note_key).ok())
                .collect();

            let _result = process_conversation_notes(notes, &mut session, 1, true, Some(&sk), &ndb);
        }

        // Find the PermissionRequest in chat and verify it was marked Denied
        let perm_msg = session
            .chat
            .iter()
            .find_map(|m| {
                if let Message::PermissionRequest(req) = m {
                    if req.id == perm_id {
                        Some(req)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .expect("should have a PermissionRequest in chat");

        assert_eq!(
            perm_msg.response,
            Some(crate::messages::PermissionResponseType::Denied),
            "denied permission_response should set Denied, not Allowed"
        );
    }

    /// When both permission_request and permission_response arrive in the
    /// same batch, the response may sort before the request. The request
    /// handler checks `responded` — it must use the stored decision, not
    /// hardcode Allowed.
    #[tokio::test]
    async fn test_permission_denied_single_batch() {
        let sk = test_secret_key();
        let author = nostrdb_net::FullKeypair::from_secret_bytes(&sk)
            .unwrap()
            .pubkey;
        let mut threading = ThreadingState::new();
        let session_id_str = "perm-single-batch";
        let perm_id = uuid::Uuid::new_v4();

        let perm_req_evt = build_permission_request_event(
            &perm_id,
            "Bash",
            &serde_json::json!({"command": "rm -rf /"}),
            session_id_str,
            &mut threading,
            &sk,
        )
        .unwrap();

        let perm_resp_evt = session_events::build_permission_response_event(
            &perm_id,
            &[0u8; 32],
            false, // DENIED
            Some("too dangerous"),
            false,
            false, // not auto-accepted
            session_id_str,
            &mut threading,
            &sk,
        )
        .unwrap();

        let tmp_dir = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp_dir.path().to_str().unwrap(), &test_config()).unwrap();

        let filter = nostrdb::Filter::new()
            .kinds([session_events::AI_CONVERSATION_KIND as u64])
            .build();

        for event in [&perm_req_evt, &perm_resp_evt] {
            let sub = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
            ndb.process_event_with(&event.to_event_json(), IngestMetadata::new().client(true))
                .expect("ingest failed");
            let _keys = ndb.wait_for_notes(sub, 1).await.unwrap();
        }

        let mut session = session::ChatSession::new(
            1,
            PathBuf::from("/tmp"),
            AiMode::Agentic,
            BackendType::Remote,
        );
        session.source = SessionSource::Remote;
        session.agentic.as_mut().unwrap().event_id = session_id_str.to_string();

        // Process all events in one batch, then rebuild the chat from ndb.
        {
            let txn = Transaction::new(&ndb).unwrap();
            let results = ndb.query(&txn, &[filter], 128).unwrap();
            let notes: Vec<_> = results
                .iter()
                .filter_map(|qr| ndb.get_note_by_key(&txn, qr.note_key).ok())
                .collect();
            assert_eq!(notes.len(), 2);

            let _result = process_conversation_notes(notes, &mut session, 1, true, Some(&sk), &ndb);
            rebuild_remote_chat(&mut session, &ndb, &txn, &author);
        }

        // Find the PermissionRequest — regardless of processing order,
        // the denied response must be reflected.
        let perm_msg = session
            .chat
            .iter()
            .find_map(|m| {
                if let Message::PermissionRequest(req) = m {
                    if req.id == perm_id {
                        Some(req)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .expect("should have a PermissionRequest in chat");

        assert_eq!(
            perm_msg.response,
            Some(crate::messages::PermissionResponseType::Denied),
            "single-batch denied response should not be marked Allowed"
        );
    }

    /// Regression: an auto-accepted remote permission must reconstruct as
    /// `auto_accepted` from ndb alone. The remote chat is rebuilt purely from the
    /// persisted event set (`rebuild_remote_chat`), so if the `"auto"` provenance
    /// isn't carried on the response event and reconstructed by the loader, the
    /// responded row starts collapsed on a fresh machine — the regression this
    /// test guards. No in-memory decision is seeded here; the flag must come
    /// entirely from persisted events.
    #[tokio::test]
    async fn test_auto_accepted_permission_survives_ndb_rebuild() {
        let sk = test_secret_key();
        let author = nostrdb_net::FullKeypair::from_secret_bytes(&sk)
            .unwrap()
            .pubkey;
        let mut threading = ThreadingState::new();
        let session_id_str = "perm-auto-rebuild";
        let perm_id = uuid::Uuid::new_v4();

        let perm_req_evt = build_permission_request_event(
            &perm_id,
            "Bash",
            &serde_json::json!({"command": "cargo test --all"}),
            session_id_str,
            &mut threading,
            &sk,
        )
        .unwrap();

        // An auto-accepted (runtime allowlist) allow response: allowed=true,
        // auto_accepted=true.
        let perm_resp_evt = session_events::build_permission_response_event(
            &perm_id,
            &perm_req_evt.note_id,
            true,  // allowed
            None,  // no message
            false, // not a turn interrupt
            true,  // AUTO-ACCEPTED
            session_id_str,
            &mut threading,
            &sk,
        )
        .unwrap();

        let tmp_dir = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp_dir.path().to_str().unwrap(), &test_config()).unwrap();

        let filter = nostrdb::Filter::new()
            .kinds([session_events::AI_CONVERSATION_KIND as u64])
            .build();

        for event in [&perm_req_evt, &perm_resp_evt] {
            let sub = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
            ndb.process_event_with(&event.to_event_json(), IngestMetadata::new().client(true))
                .expect("ingest failed");
            let _keys = ndb.wait_for_notes(sub, 1).await.unwrap();
        }

        // Fresh remote session — nothing in memory, mimicking a newly-synced
        // machine whose chat is reconstructed purely from ndb.
        let mut session = session::ChatSession::new(
            1,
            PathBuf::from("/tmp"),
            AiMode::Agentic,
            BackendType::Remote,
        );
        session.source = SessionSource::Remote;
        session.agentic.as_mut().unwrap().event_id = session_id_str.to_string();

        {
            let txn = Transaction::new(&ndb).unwrap();
            rebuild_remote_chat(&mut session, &ndb, &txn, &author);
        }

        let perm_msg = session
            .chat
            .iter()
            .find_map(|m| match m {
                Message::PermissionRequest(req) if req.id == perm_id => Some(req),
                _ => None,
            })
            .expect("should have a PermissionRequest in chat");

        assert_eq!(
            perm_msg.response,
            Some(crate::messages::PermissionResponseType::Allowed),
            "auto-accepted response should reconstruct as Allowed"
        );
        assert!(
            perm_msg.auto_accepted,
            "auto-accept provenance must survive the ndb rebuild so the row \
             starts expanded on a fresh machine"
        );
    }
}
