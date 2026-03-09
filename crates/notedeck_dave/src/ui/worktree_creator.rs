use std::path::PathBuf;
use std::sync::mpsc;

use egui_nav::{Nav, NavUiType, RouteResponse};

use crate::backend::BackendType;
use crate::session::SessionId;
use crate::worktree;

/// The single Nav route for the worktree creator overlay.
#[derive(Clone, PartialEq, Debug)]
pub enum WorktreeRoute {
    Settings,
}

pub enum WorktreeCreatorAction {
    Cancelled,
    Created {
        worktree_path: PathBuf,
        branch: String,
        is_new_branch: bool,
        backend_type: BackendType,
    },
}

#[derive(Clone, PartialEq, Debug)]
pub enum BranchMode {
    New,
    Existing,
}

pub struct WorktreeCreator {
    pub from_session_id: SessionId,
    pub from_cwd: PathBuf,

    pub parent_dir: PathBuf,
    dir_input: String,
    /// The initial dir_input value before any user edits.
    initial_dir_input: String,
    /// Async native folder-picker (same pattern as DirectoryPicker)
    pending_browse: Option<mpsc::Receiver<Option<PathBuf>>>,
    /// Async git repo root detection
    pending_root: Option<mpsc::Receiver<Option<PathBuf>>>,

    pub folder_name: String,

    pub branch_mode: BranchMode,
    pub new_branch_name: String,
    pub existing_branches: Vec<String>,
    pub selected_branch_idx: Option<usize>,
    /// Async branch list fetch
    pending_branches: Option<mpsc::Receiver<Vec<String>>>,

    /// The backend that will be used for the new worktree session.
    pub selected_backend: BackendType,

    /// Error message from the last failed create attempt.
    pub error: Option<String>,
}

impl WorktreeCreator {
    pub fn new(
        from_session_id: SessionId,
        from_cwd: PathBuf,
        parent_backend_type: BackendType,
    ) -> Self {
        // Use from_cwd's parent as immediate fallback; git root detection runs async.
        let default_parent = from_cwd
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| from_cwd.clone());
        let dir_input = default_parent.to_string_lossy().into_owned();

        let (root_tx, root_rx) = mpsc::channel();
        let cwd_for_root = from_cwd.clone();
        std::thread::spawn(move || {
            let _ = root_tx.send(worktree::git_repo_root(&cwd_for_root));
        });

        let (branch_tx, branch_rx) = mpsc::channel();
        let cwd_for_branches = from_cwd.clone();
        std::thread::spawn(move || {
            let _ = branch_tx.send(worktree::list_branches(&cwd_for_branches));
        });

        Self {
            from_session_id,
            from_cwd,
            parent_dir: default_parent,
            initial_dir_input: dir_input.clone(),
            dir_input,
            pending_browse: None,
            pending_root: Some(root_rx),
            folder_name: String::new(),
            branch_mode: BranchMode::New,
            new_branch_name: String::new(),
            existing_branches: Vec::new(),
            selected_branch_idx: None,
            pending_branches: Some(branch_rx),
            selected_backend: parent_backend_type,
            error: None,
        }
    }

    /// Poll all pending async operations. Returns true if any completed (repaint needed).
    fn poll_async(&mut self) -> bool {
        let mut changed = false;

        if let Some(rx) = &self.pending_browse {
            match rx.try_recv() {
                Ok(Some(path)) => {
                    self.dir_input = path.to_string_lossy().into_owned();
                    self.parent_dir = path;
                    self.pending_browse = None;
                    changed = true;
                }
                Ok(None) | Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending_browse = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }

        if let Some(rx) = &self.pending_root {
            match rx.try_recv() {
                Ok(root) => {
                    if let Some(parent) = root.and_then(|r| r.parent().map(PathBuf::from)) {
                        // Only apply if the user hasn't edited dir_input
                        if self.dir_input == self.initial_dir_input {
                            self.dir_input = parent.to_string_lossy().into_owned();
                            self.initial_dir_input = self.dir_input.clone();
                        }
                        self.parent_dir = parent;
                        changed = true;
                    }
                    self.pending_root = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending_root = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }

        if let Some(rx) = &self.pending_branches {
            match rx.try_recv() {
                Ok(branches) => {
                    self.existing_branches = branches;
                    self.pending_branches = None;
                    changed = true;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending_branches = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }

        changed
    }

    pub fn overlay_ui(
        &mut self,
        ui: &mut egui::Ui,
        available_backends: &[BackendType],
    ) -> Option<WorktreeCreatorAction> {
        if self.poll_async() {
            ui.ctx().request_repaint();
        }

        let mut action: Option<WorktreeCreatorAction> = None;

        Nav::new(&[WorktreeRoute::Settings])
            .id_source(egui::Id::new("worktree_creator_nav"))
            .show_mut(ui, |ui, render_type, _nav| match render_type {
                NavUiType::Title => {
                    ui.label(egui::RichText::new("New Worktree").strong().size(16.0));
                    RouteResponse {
                        response: (),
                        can_take_drag_from: vec![],
                    }
                }
                NavUiType::Body => {
                    action = self.settings_ui(ui, available_backends);
                    RouteResponse {
                        response: (),
                        can_take_drag_from: vec![],
                    }
                }
            });

        action
    }

    fn settings_ui(
        &mut self,
        ui: &mut egui::Ui,
        available_backends: &[BackendType],
    ) -> Option<WorktreeCreatorAction> {
        ui.add_space(12.0);
        self.parent_dir_ui(ui);
        ui.add_space(12.0);
        self.folder_name_ui(ui);
        ui.add_space(12.0);
        self.branch_ui(ui);
        ui.add_space(12.0);
        self.backend_ui(ui, available_backends);
        if let Some(err) = &self.error {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(err.as_str())
                    .color(ui.visuals().error_fg_color)
                    .size(11.0),
            );
        }
        ui.add_space(16.0);
        self.action_buttons_ui(ui)
    }

    fn backend_ui(&mut self, ui: &mut egui::Ui, available_backends: &[BackendType]) {
        ui.label("Agent:");
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            for &bt in available_backends {
                ui.selectable_value(&mut self.selected_backend, bt, bt.display_name());
            }
        });
    }

    fn parent_dir_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Parent directory:");
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let edit = egui::TextEdit::singleline(&mut self.dir_input)
                .desired_width(ui.available_width() - 74.0)
                .hint_text("/path/to/parent");
            if ui.add(edit).changed() {
                self.parent_dir = PathBuf::from(&self.dir_input);
            }
            self.browse_button(ui);
        });
    }

    fn folder_name_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Folder name:");
        ui.add_space(4.0);
        ui.add(
            egui::TextEdit::singleline(&mut self.folder_name)
                .desired_width(ui.available_width())
                .hint_text("e.g. feature-xyz"),
        );
        if !self.folder_name.is_empty() {
            let preview = self
                .parent_dir
                .join(&self.folder_name)
                .to_string_lossy()
                .into_owned();
            ui.label(
                egui::RichText::new(format!("→ {preview}"))
                    .weak()
                    .size(11.0),
            );
        }
    }

    fn branch_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Branch:");
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.branch_mode, BranchMode::New, "New branch");
            ui.selectable_value(
                &mut self.branch_mode,
                BranchMode::Existing,
                "Existing branch",
            );
        });
        ui.add_space(6.0);
        match &self.branch_mode {
            BranchMode::New => self.new_branch_ui(ui),
            BranchMode::Existing => self.existing_branch_ui(ui),
        }
    }

    fn new_branch_ui(&mut self, ui: &mut egui::Ui) {
        if ui
            .add(
                egui::TextEdit::singleline(&mut self.new_branch_name)
                    .desired_width(ui.available_width())
                    .hint_text("branch name (defaults to folder name)"),
            )
            .changed()
        {
            self.error = None;
        }
    }

    fn existing_branch_ui(&mut self, ui: &mut egui::Ui) {
        if self.existing_branches.is_empty() {
            ui.label(egui::RichText::new("No local branches found.").weak());
            return;
        }
        egui::ScrollArea::vertical()
            .max_height(120.0)
            .id_salt("worktree_branches")
            .show(ui, |ui| {
                for (i, branch) in self.existing_branches.iter().enumerate() {
                    let selected = self.selected_branch_idx == Some(i);
                    if ui.selectable_label(selected, branch.as_str()).clicked() {
                        self.selected_branch_idx = Some(i);
                    }
                }
            });
    }

    fn action_buttons_ui(&mut self, ui: &mut egui::Ui) -> Option<WorktreeCreatorAction> {
        let mut result = None;

        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                result = Some(WorktreeCreatorAction::Cancelled);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let can_create = !self.folder_name.is_empty()
                    && match &self.branch_mode {
                        BranchMode::New => true,
                        BranchMode::Existing => self.selected_branch_idx.is_some(),
                    };
                if ui
                    .add_enabled(can_create, egui::Button::new("Create Worktree"))
                    .clicked()
                {
                    result = Some(self.build_create_action());
                }
            });
        });

        result
    }

    fn build_create_action(&self) -> WorktreeCreatorAction {
        let (branch, is_new_branch) = match &self.branch_mode {
            BranchMode::New => {
                let name = if self.new_branch_name.is_empty() {
                    &self.folder_name
                } else {
                    &self.new_branch_name
                };
                (name.clone(), true)
            }
            BranchMode::Existing => {
                let idx = self.selected_branch_idx.expect("validated before calling");
                (self.existing_branches[idx].clone(), false)
            }
        };
        WorktreeCreatorAction::Created {
            worktree_path: self.parent_dir.join(&self.folder_name),
            branch,
            is_new_branch,
            backend_type: self.selected_backend,
        }
    }

    fn browse_button(&mut self, ui: &mut egui::Ui) {
        #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
        if ui.button("Browse…").clicked() && self.pending_browse.is_none() {
            let (tx, rx) = mpsc::channel();
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                let picked = rfd::FileDialog::new().pick_folder();
                let _ = tx.send(picked);
                ctx.request_repaint();
            });
            self.pending_browse = Some(rx);
        }
    }
}
