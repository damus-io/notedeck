//! Run configurations (kind-31991) and the processes the Run button launches
//! from them: loading, syncing and publishing configs, and killing and
//! reaping their process trees.

use crate::publish::ingest_built_event;
use crate::{session_events, session_loader, ui, Dave, RunConfig, SessionId};
use std::collections::{HashMap, HashSet};

/// Kill a spawned process and all of its descendants.
///
/// On Unix, we use the process group created at spawn time (via `process_group(0)`),
/// sending SIGKILL to the entire group so that grandchildren like `cargo`, `rustc`,
/// or a compiled binary are all terminated.
///
/// On non-Unix platforms we fall back to killing only the immediate child.
pub(crate) fn kill_process_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // The child's PID is also its PGID because we called process_group(0) at spawn.
        // A negative PID in kill(2) targets the entire process group.
        let pgid = child.id() as libc::pid_t;
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
}

impl Dave {
    pub(crate) fn kill_session_run_processes(&mut self, id: SessionId) {
        if let Some(mut procs) = self.run_processes.remove(&id) {
            for (_, mut child) in procs.drain() {
                kill_process_tree(&mut child);
                self.pending_reap.push(child);
            }
        }
        self.running_session_ids.remove(&id);
    }

    /// Reap finished run processes and update `self.running_session_ids` in one pass.
    /// Called once per frame from `update()`.
    pub(crate) fn reap_run_processes(&mut self) {
        let mut still_running: HashMap<SessionId, HashSet<String>> = HashMap::new();
        for (session_id, procs) in self.run_processes.iter_mut() {
            procs.retain(|cfg_id, child| match child.try_wait() {
                Ok(None) => {
                    still_running
                        .entry(*session_id)
                        .or_default()
                        .insert(cfg_id.clone());
                    true
                }
                Ok(Some(status)) => {
                    tracing::trace!(
                        "run process [{cfg_id}] for session {session_id} exited: {status}"
                    );
                    false
                }
                Err(e) => {
                    tracing::warn!(
                        "run process [{cfg_id}] for session {session_id} try_wait error: {e}"
                    );
                    false
                }
            });
        }
        self.run_processes.retain(|_, procs| !procs.is_empty());
        self.running_session_ids = still_running;
    }

    /// Reap killed child processes without blocking; removes entries that have exited.
    pub(crate) fn poll_pending_reap(&mut self) {
        self.pending_reap
            .retain_mut(|child| child.try_wait().ok().flatten().is_none());
    }

    /// Poll ndb for new kind-31991 run-config events and upsert into `self.run_configs`.
    ///
    /// Each event is one config (d-tag = config UUID). Live events may be
    /// upserts (name/command changed) or tombstones (deleted tag present).
    pub(crate) fn poll_run_config_events(&mut self, ndb: &nostrdb::Ndb) {
        let Some(sub) = self.run_config_sub else {
            return;
        };
        let Some(account) = self.pns_local_state.as_ref().map(|state| state.account) else {
            return;
        };
        let note_keys = ndb.poll_for_notes(sub, 1);
        if note_keys.is_empty() {
            return;
        }
        let Ok(txn) = nostrdb::Transaction::new(ndb) else {
            return;
        };
        for key in note_keys {
            let Ok(note) = ndb.get_note_by_key(&txn, key) else {
                continue;
            };
            if note.kind() != crate::config::AI_RUN_CONFIG_KIND {
                continue;
            }
            if *note.pubkey() != *account.bytes() {
                continue;
            }
            if session_events::get_tag_value(&note, "hostname") != Some(self.hostname.as_str()) {
                continue;
            }
            if session_events::is_run_config_deleted(&note) {
                // Tombstone: remove config by d-tag ID, only if newer
                let ts = note.created_at();
                if let Some(config_id) = session_events::run_config_event_id(&note) {
                    let mut removed = false;
                    for configs in self.run_configs.values_mut() {
                        let before = configs.len();
                        configs.retain(|c| c.id != config_id || c.updated_at > ts);
                        if configs.len() < before {
                            removed = true;
                        }
                    }
                    if removed {
                        self.kill_run_config_processes(&config_id);
                    }
                    self.run_configs.retain(|_, v| !v.is_empty());
                }
            } else if let Some((cwd, config)) = session_events::parse_run_config_event(&note) {
                // Upsert: update existing or insert new, only if newer
                let configs = self.run_configs.entry(cwd).or_default();
                if let Some(existing) = configs.iter_mut().find(|c| c.id == config.id) {
                    if config.updated_at >= existing.updated_at {
                        existing.name = config.name;
                        existing.command = config.command;
                        existing.updated_at = config.updated_at;
                    }
                } else {
                    configs.push(config);
                }
                RunConfig::sort_by_name(configs);
            }
        }
    }

    /// Kill a running process for the given session and config ID.
    pub(crate) fn kill_run_process(&mut self, session_id: &SessionId, config_id: &str) {
        if let Some(procs) = self.run_processes.get_mut(session_id) {
            if let Some(mut child) = procs.remove(config_id) {
                kill_process_tree(&mut child);
                self.pending_reap.push(child);
            }
            if procs.is_empty() {
                self.run_processes.remove(session_id);
            }
        }
        if let Some(ids) = self.running_session_ids.get_mut(session_id) {
            ids.remove(config_id);
            if ids.is_empty() {
                self.running_session_ids.remove(session_id);
            }
        }
    }

    /// Kill all running processes for a given config ID across all sessions.
    pub(crate) fn kill_run_config_processes(&mut self, config_id: &str) {
        let session_ids: Vec<_> = self.run_processes.keys().copied().collect();
        for sid in session_ids {
            self.kill_run_process(&sid, config_id);
        }
    }

    /// Collect all existing run configs as editor suggestions.
    pub(crate) fn collect_run_config_suggestions(
        &self,
        exclude_id: Option<&str>,
    ) -> Vec<RunConfig> {
        ui::run_config_editor::collect_run_config_suggestions(&self.run_configs, exclude_id)
    }

    /// Build and queue a kind-31991 event for a single run config.
    pub(crate) fn publish_run_config(
        &mut self,
        config: &RunConfig,
        cwd: &std::path::Path,
        ndb: &nostrdb::Ndb,
        sk: &[u8; 32],
    ) {
        ingest_built_event(
            session_events::build_run_config_event(
                config,
                &cwd.to_string_lossy(),
                &self.hostname,
                sk,
            ),
            "run-config",
            ndb,
            sk,
        );
    }

    /// Build and queue a tombstone kind-31991 event to delete a config.
    pub(crate) fn publish_run_config_delete(
        &mut self,
        config_id: &str,
        cwd: &std::path::Path,
        ndb: &nostrdb::Ndb,
        sk: &[u8; 32],
    ) {
        ingest_built_event(
            session_events::build_run_config_delete_event(
                config_id,
                &cwd.to_string_lossy(),
                &self.hostname,
                sk,
            ),
            "run-config-delete",
            ndb,
            sk,
        );
    }

    pub(crate) fn subscribe_pns_run_configs(
        &mut self,
        ndb: &nostrdb::Ndb,
        account: nostrdb_net::Pubkey,
    ) {
        let rc_filter = nostrdb::Filter::new()
            .kinds([crate::config::AI_RUN_CONFIG_KIND as u64])
            .authors([account.bytes()])
            .build();
        match ndb.subscribe(&[rc_filter]) {
            Ok(sub) => {
                self.run_config_sub = Some(sub);
                tracing::info!("subscribed for run config events in ndb");
            }
            Err(e) => {
                tracing::warn!("failed to subscribe for run config events: {:?}", e);
            }
        }
    }

    pub(crate) fn load_run_configs(&mut self, ndb: &nostrdb::Ndb, account: nostrdb_net::Pubkey) {
        let txn = match nostrdb::Transaction::new(ndb) {
            Ok(txn) => txn,
            Err(err) => {
                tracing::warn!("failed to open txn for run config restore: {err:?}");
                return;
            }
        };
        self.run_configs =
            session_loader::load_run_configs_from_ndb(ndb, &txn, &account, &self.hostname);
        tracing::info!("loaded {} run config CWDs from ndb", self.run_configs.len());
    }
}
