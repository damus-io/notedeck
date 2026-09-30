//! Dave's per-frame UI: the scene, desktop and narrow layouts, and the
//! dispatch of the UI and keybinding actions they return.

use crate::backend::{BackendType, Model};
use crate::ui::keybindings::{KeyAction, KeyContext};
use crate::{
    check_keybindings, focus_queue, get_backend, secret_key_bytes, ui, update, worktree, Dave,
    DaveAction, DaveOverlay, DaveResponse, KeyActionResult, OverlayResult, PendingWorktreeRemoval,
    RunConfigEditor, SceneViewAction, SendActionResult, SessionListAction, UiActionResult,
};
use notedeck::{ui::is_narrow, AppAction, AppContext};

impl Dave {
    pub(crate) fn ui(&mut self, app_ctx: &mut AppContext, ui: &mut egui::Ui) -> DaveResponse {
        // Check overlays first — take ownership so we can call &mut self
        // methods freely. Put the variant back if the overlay stays open.
        let overlay = std::mem::take(&mut self.active_overlay);
        match overlay {
            DaveOverlay::Settings => {
                match ui::settings_overlay_ui(
                    &mut self.settings_panel,
                    &self.settings,
                    app_ctx.i18n,
                    ui,
                ) {
                    OverlayResult::ApplySettings(new_settings) => {
                        self.apply_settings(new_settings.clone());
                        return DaveResponse::new(DaveAction::UpdateSettings(new_settings));
                    }
                    OverlayResult::Close => {}
                    _ => {
                        self.active_overlay = DaveOverlay::Settings;
                    }
                }
                return DaveResponse::default();
            }
            DaveOverlay::NewSessionKind => {
                let has_sessions = !self.session_manager.is_empty();
                match ui::session_kind_picker_overlay_ui(ui, has_sessions) {
                    OverlayResult::NewSessionChat => {
                        let cwd = std::env::current_dir().unwrap_or_default();
                        self.create_session_with_cwd(
                            cwd,
                            self.model_config.backend,
                            Model::Default,
                        );
                        self.active_overlay = DaveOverlay::None;
                    }
                    OverlayResult::NewSessionAgentic => {
                        self.active_overlay = DaveOverlay::HostPicker;
                    }
                    OverlayResult::Close => {}
                    _ => {
                        self.active_overlay = DaveOverlay::NewSessionKind;
                    }
                }
                return DaveResponse::default();
            }
            DaveOverlay::HostPicker => {
                let has_sessions = !self.session_manager.is_empty();
                let known_hosts = self.known_remote_hosts();
                match ui::host_picker_overlay_ui(&self.hostname, &known_hosts, has_sessions, ui) {
                    OverlayResult::HostSelected(host) => {
                        self.directory_picker.target_host = host;
                        self.active_overlay = DaveOverlay::DirectoryPicker;
                    }
                    OverlayResult::Close => {}
                    _ => {
                        self.active_overlay = DaveOverlay::HostPicker;
                    }
                }
                return DaveResponse::default();
            }
            DaveOverlay::DirectoryPicker => {
                let has_sessions = !self.session_manager.is_empty();
                match ui::directory_picker_overlay_ui(&mut self.directory_picker, has_sessions, ui)
                {
                    OverlayResult::DirectorySelected(path) => {
                        if let Some(target_host) = self.directory_picker.target_host.take() {
                            tracing::info!(
                                "remote directory selected: {:?} on {}",
                                path,
                                target_host
                            );
                            self.queue_spawn_command(
                                &target_host,
                                &path,
                                self.model_config.backend,
                            );
                        } else {
                            tracing::info!("directory selected: {:?}", path);
                            self.create_or_pick_backend(path, None);
                        }
                    }
                    OverlayResult::Close => {
                        self.directory_picker.target_host = None;
                    }
                    _ => {
                        self.active_overlay = DaveOverlay::DirectoryPicker;
                    }
                }
                return DaveResponse::default();
            }
            DaveOverlay::SessionPicker { backend, model } => {
                match ui::session_picker_overlay_ui(&mut self.session_picker, ui) {
                    OverlayResult::ResumeSession {
                        cwd,
                        session_id,
                        title,
                        file_path,
                    } => {
                        // Resumed sessions are always Claude (discovered from JSONL)
                        let claude_session_id = session_id.clone();
                        let sid = self.create_resumed_session_with_cwd(
                            cwd,
                            session_id,
                            title,
                            BackendType::Claude,
                        );
                        self.pending_archive_convert = Some((file_path, sid, claude_session_id));
                        self.session_picker.close();
                    }
                    OverlayResult::NewSession { cwd } => {
                        tracing::info!(
                            "new session from session picker: {:?} (backend: {:?})",
                            cwd,
                            backend
                        );
                        self.session_picker.close();
                        self.create_session_with_cwd(cwd, backend, model.clone());
                    }
                    OverlayResult::BackToDirectoryPicker => {
                        self.session_picker.close();
                        self.active_overlay = DaveOverlay::DirectoryPicker;
                    }
                    _ => {
                        self.active_overlay = DaveOverlay::SessionPicker { backend, model };
                    }
                }
                return DaveResponse::default();
            }
            DaveOverlay::BackendPicker {
                cwd,
                target_host,
                mut selected_models,
            } => {
                if let Some((bt, model)) = ui::backend_picker_overlay_ui(
                    &self.available_backends,
                    &mut selected_models,
                    ui,
                ) {
                    tracing::info!("backend selected: {:?}, model: {:?}", bt, model);
                    if let Some(host) = target_host {
                        self.queue_spawn_command(&host, &cwd, bt);
                    } else {
                        self.create_or_resume_session(cwd, bt, model);
                    }
                } else {
                    self.active_overlay = DaveOverlay::BackendPicker {
                        cwd,
                        target_host,
                        selected_models,
                    };
                }
                return DaveResponse::default();
            }
            DaveOverlay::WorktreeCreator(mut creator) => {
                match ui::worktree_creator_overlay_ui(&mut creator, ui, &self.available_backends) {
                    Some(ui::WorktreeCreatorAction::Created {
                        worktree_path,
                        branch,
                        is_new_branch,
                        backend_type,
                    }) => {
                        match worktree::create_git_worktree(
                            &creator.from_cwd,
                            &worktree_path,
                            &branch,
                            is_new_branch,
                        ) {
                            Ok(()) => {
                                self.create_session_with_cwd(
                                    worktree_path,
                                    backend_type,
                                    Model::Default,
                                );
                            }
                            Err(msg) => {
                                creator.error = Some(msg);
                                self.active_overlay = DaveOverlay::WorktreeCreator(creator);
                            }
                        }
                    }
                    Some(ui::WorktreeCreatorAction::Cancelled) => { /* overlay closes */ }
                    None => {
                        self.active_overlay = DaveOverlay::WorktreeCreator(creator);
                    }
                }

                return DaveResponse::default();
            }
            DaveOverlay::RunConfigEditor(mut editor) => {
                match ui::run_config_editor_overlay_ui(&mut editor, ui) {
                    Some(editor_action) => {
                        let change = editor_action.process(&mut self.run_configs);
                        if let ui::RunConfigChange::Deleted { ref config_id, .. } = change {
                            self.kill_run_config_processes(config_id);
                        }
                        if let Some(sk) =
                            secret_key_bytes(app_ctx.accounts.get_selected_account().keypair())
                        {
                            match change {
                                ui::RunConfigChange::Saved { cwd, config } => {
                                    self.publish_run_config(&config, &cwd, app_ctx.ndb, &sk);
                                }
                                ui::RunConfigChange::Deleted { cwd, config_id } => {
                                    self.publish_run_config_delete(
                                        &config_id,
                                        &cwd,
                                        app_ctx.ndb,
                                        &sk,
                                    );
                                }
                                ui::RunConfigChange::None => {}
                            }
                        }
                    }
                    None => {
                        self.active_overlay = DaveOverlay::RunConfigEditor(editor);
                    }
                }
                return DaveResponse::default();
            }
            DaveOverlay::None => {}
        }

        // Normal routing
        if is_narrow(ui.ctx()) {
            self.narrow_ui(app_ctx, ui)
        } else if self.show_scene {
            self.scene_ui(app_ctx, ui)
        } else {
            self.desktop_ui(app_ctx, ui)
        }
    }

    /// Scene view with RTS-style agent visualization and chat side panel
    fn scene_ui(&mut self, app_ctx: &mut AppContext, ui: &mut egui::Ui) -> DaveResponse {
        let (dave_response, view_action) = ui::scene_ui(
            &mut self.session_manager,
            &mut self.scene,
            &mut self.focus_queue,
            &self.model_config,
            self.auto_steal.is_enabled(),
            self.normal_mode.view(),
            &self.run_configs,
            &self.running_session_ids,
            app_ctx,
            ui,
        );

        // Handle view actions
        match view_action {
            SceneViewAction::ToggleToListView => {
                self.show_scene = false;
            }
            SceneViewAction::SpawnAgent => {
                return DaveResponse::new(DaveAction::NewChat);
            }
            SceneViewAction::DeleteSelected(ids) => {
                for id in ids {
                    self.delete_session(id);
                }
                if let Some(session) = self.session_manager.sessions_ordered().first() {
                    self.scene.select(session.id);
                } else {
                    self.scene.clear_selection();
                }
            }
            SceneViewAction::SelectedSession(id) => {
                self.anchor_focus(id);
            }
            SceneViewAction::None => {}
        }

        dave_response
    }

    /// Desktop layout with sidebar for session list
    fn desktop_ui(&mut self, app_ctx: &mut AppContext, ui: &mut egui::Ui) -> DaveResponse {
        let (chat_response, session_action, toggle_scene) = ui::desktop_ui(
            &mut self.session_manager,
            &self.focus_queue,
            &self.collapse_state,
            &self.model_config,
            self.auto_steal.is_enabled(),
            self.normal_mode.view(),
            &self.run_configs,
            &self.running_session_ids,
            app_ctx,
            ui,
        );

        if toggle_scene {
            self.show_scene = true;
        }

        if let Some(action) = session_action {
            match action {
                SessionListAction::NewSession => return DaveResponse::new(DaveAction::NewChat),
                SessionListAction::SwitchTo(id) => {
                    self.session_manager.switch_to(id);
                    self.focus_queue.dequeue(id);
                    self.anchor_focus(id);
                }
                SessionListAction::Delete(id) => {
                    self.delete_session(id);
                }
                SessionListAction::Rename(id, new_title) => {
                    self.rename_session(id, new_title);
                }
                SessionListAction::DismissDone(id) => {
                    self.focus_queue.dequeue_done(id);
                    if let Some(session) = self.session_manager.get_mut(id) {
                        if session.indicator == Some(focus_queue::FocusPriority::Done) {
                            session.indicator = None;
                            session.state_dirty = true;
                        }
                    }
                }
                SessionListAction::Duplicate(id) => {
                    self.duplicate_session(id);
                }
                SessionListAction::Reset(id) => {
                    self.clear_session(id);
                }
                SessionListAction::NewWorktree(session_id) => {
                    if let Some((cwd, backend_type)) = self
                        .session_manager
                        .get(session_id)
                        .and_then(|s| s.cwd().cloned().map(|c| (c, s.backend_type)))
                    {
                        self.active_overlay = DaveOverlay::WorktreeCreator(Box::new(
                            ui::WorktreeCreator::new(session_id, cwd, backend_type),
                        ));
                    }
                }
                SessionListAction::DeleteWorktree(session_id) => {
                    if let Some(cwd) = self
                        .session_manager
                        .get(session_id)
                        .and_then(|s| s.cwd().cloned())
                    {
                        self.pending_worktree_removals
                            .push(PendingWorktreeRemoval::spawn(session_id, cwd));
                    }
                }
                SessionListAction::ToggleHostCollapse(hostname) => {
                    self.toggle_host_collapse(&hostname);
                }
                SessionListAction::ToggleProjectCollapse(hostname, root) => {
                    self.toggle_project_collapse(&hostname, &root);
                }
                SessionListAction::ToggleCwdCollapse(hostname, cwd) => {
                    self.toggle_cwd_collapse(&hostname, &cwd);
                }
                SessionListAction::NewSessionInCwd(hostname, cwd) => {
                    let target_host = if hostname.is_empty() {
                        None
                    } else {
                        Some(hostname)
                    };
                    self.create_or_pick_backend(cwd, target_host);
                }
            }
        }

        chat_response
    }

    /// Narrow/mobile layout - shows either session list or chat
    fn narrow_ui(&mut self, app_ctx: &mut AppContext, ui: &mut egui::Ui) -> DaveResponse {
        let (dave_response, session_action) = ui::narrow_ui(
            &mut self.session_manager,
            &self.focus_queue,
            &self.collapse_state,
            &self.model_config,
            self.auto_steal.is_enabled(),
            self.normal_mode.view(),
            &self.run_configs,
            &self.running_session_ids,
            self.show_session_list,
            app_ctx,
            ui,
        );

        if let Some(action) = session_action {
            match action {
                SessionListAction::NewSession => {
                    self.handle_new_chat();
                    self.show_session_list = false;
                }
                SessionListAction::SwitchTo(id) => {
                    self.session_manager.switch_to(id);
                    self.focus_queue.dequeue(id);
                    self.anchor_focus(id);
                    self.show_session_list = false;
                }
                SessionListAction::Delete(id) => {
                    self.delete_session(id);
                }
                SessionListAction::Rename(id, new_title) => {
                    self.rename_session(id, new_title);
                }
                SessionListAction::DismissDone(id) => {
                    self.focus_queue.dequeue_done(id);
                    if let Some(session) = self.session_manager.get_mut(id) {
                        if session.indicator == Some(focus_queue::FocusPriority::Done) {
                            session.indicator = None;
                            session.state_dirty = true;
                        }
                    }
                }
                SessionListAction::Duplicate(id) => {
                    self.duplicate_session(id);
                    self.show_session_list = false;
                }
                SessionListAction::Reset(id) => {
                    self.clear_session(id);
                    self.show_session_list = false;
                }
                SessionListAction::NewWorktree(session_id) => {
                    if let Some((cwd, backend_type)) = self
                        .session_manager
                        .get(session_id)
                        .and_then(|s| s.cwd().cloned().map(|c| (c, s.backend_type)))
                    {
                        self.active_overlay = DaveOverlay::WorktreeCreator(Box::new(
                            ui::WorktreeCreator::new(session_id, cwd, backend_type),
                        ));
                        self.show_session_list = false;
                    }
                }
                SessionListAction::DeleteWorktree(session_id) => {
                    if let Some(cwd) = self
                        .session_manager
                        .get(session_id)
                        .and_then(|s| s.cwd().cloned())
                    {
                        self.pending_worktree_removals
                            .push(PendingWorktreeRemoval::spawn(session_id, cwd));
                    }
                }
                SessionListAction::ToggleHostCollapse(hostname) => {
                    self.toggle_host_collapse(&hostname);
                }
                SessionListAction::ToggleProjectCollapse(hostname, root) => {
                    self.toggle_project_collapse(&hostname, &root);
                }
                SessionListAction::ToggleCwdCollapse(hostname, cwd) => {
                    self.toggle_cwd_collapse(&hostname, &cwd);
                }
                SessionListAction::NewSessionInCwd(hostname, cwd) => {
                    let target_host = if hostname.is_empty() {
                        None
                    } else {
                        Some(hostname)
                    };
                    self.create_or_pick_backend(cwd, target_host);
                    self.show_session_list = false;
                }
            }
        }

        dave_response
    }

    /// Get the first pending permission request ID for the active session
    fn first_pending_permission(&self) -> Option<uuid::Uuid> {
        update::first_pending_permission(&self.session_manager)
    }

    /// Check if the first pending permission is a shared question-set prompt
    fn has_pending_question(&self) -> bool {
        update::has_pending_question(&self.session_manager)
    }

    /// Check and dispatch keybindings. Called from render() so that
    /// key consumption only happens when Dave is the active app.
    pub(crate) fn process_keybindings(&mut self, egui_ctx: &egui::Context) {
        // While the settings panel records a new leader, it owns the keyboard.
        if self.settings_panel.is_capturing_leader() {
            return;
        }

        let has_pending_permission = self.first_pending_permission().is_some();
        let has_pending_question = self.has_pending_question();
        let in_tentative_state = self
            .session_manager
            .get_active()
            .and_then(|s| s.agentic.as_ref())
            .map(|a| a.permission_message_state != crate::session::PermissionMessageState::None)
            .unwrap_or(false);
        let active_ai_mode = self
            .session_manager
            .get_active()
            .map(|s| s.ai_mode)
            .unwrap_or(self.ai_mode);
        // The chord's `h` needs the session list on screen: the desktop layout,
        // with no overlay covering it.
        let sessions_shown = !is_narrow(egui_ctx)
            && !self.show_scene
            && matches!(self.active_overlay, DaveOverlay::None);
        // The chord's `s` needs a running turn to stop.
        let interruptible = self
            .session_manager
            .get_active()
            .is_some_and(update::session_is_interruptible);
        let keys = KeyContext {
            leader: self.leader,
            ai_mode: active_ai_mode,
            sessions_shown,
            interruptible,
            has_pending_permission,
            has_pending_question,
            in_tentative_state,
        };
        if let Some(key_action) = check_keybindings(egui_ctx, &mut self.normal_mode, keys) {
            self.handle_key_action(key_action, egui_ctx);
        }
        ui::settle_chord_focus(&mut self.normal_mode, &mut self.session_manager);
    }

    /// Handle a keybinding action
    fn handle_key_action(&mut self, key_action: KeyAction, egui_ctx: &egui::Context) {
        let bt = self
            .session_manager
            .get_active()
            .map(|s| s.backend_type)
            .unwrap_or(BackendType::Remote);
        match ui::handle_key_action(
            key_action,
            &mut self.session_manager,
            &mut self.scene,
            &mut self.focus_queue,
            &self.collapse_state,
            get_backend(&self.backends, bt),
            self.show_scene,
            self.auto_steal.is_enabled(),
            &mut self.home_session,
            egui_ctx,
        ) {
            KeyActionResult::ToggleView => {
                self.show_scene = !self.show_scene;
            }
            KeyActionResult::PublishInterruptCommand(cmd) => {
                self.pending_interrupt_commands.push(cmd);
            }
            KeyActionResult::CloneAgent => {
                self.clone_active_agent();
            }
            KeyActionResult::NewAgent => {
                self.handle_new_chat();
            }
            KeyActionResult::DeleteSession(id) => {
                self.delete_session(id);
            }
            KeyActionResult::ClearAgent => {
                if let Some(id) = self.session_manager.active_id() {
                    self.clear_session(id);
                }
            }
            KeyActionResult::SetAutoSteal(new_state) => {
                self.auto_steal = if new_state {
                    focus_queue::AutoStealState::Pending
                } else {
                    focus_queue::AutoStealState::Disabled
                };
            }
            KeyActionResult::PublishPermissionResponse(publish) => {
                self.pending_perm_responses.push(publish);
            }
            KeyActionResult::PublishModeCommand(cmd) => {
                self.pending_mode_commands.push(cmd);
            }
            KeyActionResult::None => {}
        }
    }

    /// Handle the Send action, including tentative permission states
    fn handle_send_action(&mut self, ctx: &AppContext, ui: &egui::Ui) {
        let bt = self
            .session_manager
            .get_active()
            .map(|s| s.backend_type)
            .unwrap_or(BackendType::Remote);
        match ui::handle_send_action(
            &mut self.session_manager,
            get_backend(&self.backends, bt),
            ui.ctx(),
        ) {
            SendActionResult::SendMessage => {
                self.handle_user_send(ctx);
            }
            SendActionResult::NeedsRelayPublish(publish) => {
                self.pending_perm_responses.push(publish);
            }
            SendActionResult::Handled => {}
        }
    }

    /// Handle a UI action from DaveUi
    pub(crate) fn handle_ui_action(
        &mut self,
        action: DaveAction,
        ctx: &AppContext,
        ui: &egui::Ui,
    ) -> Option<AppAction> {
        // Intercept NewChat to handle chat vs agentic mode
        if matches!(action, DaveAction::NewChat) {
            self.handle_new_chat();
            return None;
        }

        // Intercept run-app actions — handled here, not in ui::handle_ui_action
        if let DaveAction::Run(run_action) = action {
            use ui::RunAction;
            match run_action {
                RunAction::Launch { config_id } => {
                    if let Some(session) = self.session_manager.get_active() {
                        let session_id = session.id;
                        let cwd = session.cwd().cloned();
                        let cmd = cwd
                            .as_deref()
                            .and_then(|p| self.run_configs.get(p))
                            .and_then(|cfgs| cfgs.iter().find(|rc| rc.id == config_id))
                            .map(|rc| rc.command.clone());
                        match (cwd, cmd) {
                            (Some(cwd), Some(cmd)) => {
                                tracing::trace!(
                                    "RunAction::Launch: spawning `{cmd}` in {}",
                                    cwd.display()
                                );
                                #[cfg(unix)]
                                let mut command = std::process::Command::new("sh");
                                #[cfg(windows)]
                                let mut command = std::process::Command::new("cmd");
                                #[cfg(unix)]
                                command.arg("-c").arg(&cmd);
                                #[cfg(windows)]
                                command.arg("/C").arg(&cmd);
                                command
                                    .current_dir(&cwd)
                                    .stdin(std::process::Stdio::null())
                                    .stdout(std::process::Stdio::inherit())
                                    .stderr(std::process::Stdio::inherit());
                                #[cfg(unix)]
                                {
                                    use std::os::unix::process::CommandExt;
                                    command.process_group(0);
                                }
                                match command.spawn() {
                                    Ok(child) => {
                                        tracing::info!(
                                            "RunAction::Launch: spawned pid {}",
                                            child.id()
                                        );
                                        self.run_processes
                                            .entry(session_id)
                                            .or_default()
                                            .insert(config_id, child);
                                    }
                                    Err(e) => {
                                        tracing::error!("failed to spawn run command `{cmd}`: {e}");
                                    }
                                }
                            }
                            (cwd, cmd) => {
                                tracing::warn!(
                                    "RunAction::Launch: missing cwd or command (cwd={:?}, has_cmd={})",
                                    cwd,
                                    cmd.is_some()
                                );
                            }
                        }
                    }
                }
                RunAction::Stop { config_id } => {
                    if let Some(session_id) = self.session_manager.active_id() {
                        self.kill_run_process(&session_id, &config_id);
                    }
                }
                RunAction::OpenNew { cwd } => {
                    let suggestions = self.collect_run_config_suggestions(None);
                    self.active_overlay = DaveOverlay::RunConfigEditor(Box::new(
                        RunConfigEditor::new_config(cwd, suggestions),
                    ));
                }
                RunAction::OpenEdit { cwd, config_id } => {
                    let existing = self
                        .run_configs
                        .get(&cwd)
                        .and_then(|cfgs| cfgs.iter().find(|c| c.id == config_id))
                        .cloned();
                    if let Some(config) = existing {
                        let suggestions = self.collect_run_config_suggestions(Some(&config_id));
                        self.active_overlay = DaveOverlay::RunConfigEditor(Box::new(
                            RunConfigEditor::edit_config(cwd, config, suggestions),
                        ));
                    }
                }
            }
            return None;
        }

        let bt = self
            .session_manager
            .get_active()
            .map(|s| s.backend_type)
            .unwrap_or(BackendType::Remote);
        match ui::handle_ui_action(
            action,
            &mut self.session_manager,
            get_backend(&self.backends, bt),
            &mut self.active_overlay,
            &mut self.show_session_list,
            ui.ctx(),
        ) {
            UiActionResult::AppAction(app_action) => Some(app_action),
            UiActionResult::SendAction => {
                self.handle_send_action(ctx, ui);
                None
            }
            UiActionResult::PublishPermissionResponse(publish) => {
                self.pending_perm_responses.push(publish);
                None
            }
            UiActionResult::PublishModeCommand(cmd) => {
                self.pending_mode_commands.push(cmd);
                None
            }
            UiActionResult::PublishInterruptCommand(cmd) => {
                self.pending_interrupt_commands.push(cmd);
                None
            }
            UiActionResult::ToggleAutoSteal => {
                let new_state = crate::update::toggle_auto_steal(
                    &mut self.session_manager,
                    &mut self.scene,
                    self.show_scene,
                    self.auto_steal.is_enabled(),
                    &mut self.home_session,
                );
                self.auto_steal = if new_state {
                    focus_queue::AutoStealState::Pending
                } else {
                    focus_queue::AutoStealState::Disabled
                };
                None
            }
            UiActionResult::NewChat => {
                self.handle_new_chat();
                None
            }
            UiActionResult::FocusQueueNext => {
                crate::update::focus_queue_next(
                    &mut self.session_manager,
                    &mut self.focus_queue,
                    &self.collapse_state,
                    &mut self.scene,
                    self.show_scene,
                );
                None
            }
            UiActionResult::Compact => {
                self.dispatch_compact(bt, ui);
                None
            }
            UiActionResult::Handled => None,
        }
    }
}
