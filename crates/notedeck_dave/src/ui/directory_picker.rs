use egui::{Align, Color32, Layout, RichText, Vec2};
use std::path::PathBuf;

/// Maximum number of recent directories to store
const MAX_RECENT_DIRECTORIES: usize = 10;

/// Actions that can be triggered from the directory picker
#[derive(Debug, Clone)]
pub enum DirectoryPickerAction {
    /// User selected a directory
    DirectorySelected(PathBuf),
    /// User cancelled the picker
    Cancelled,
    /// User requested to browse for a directory (opens native dialog)
    BrowseRequested,
}

/// State for the directory picker modal
pub struct DirectoryPicker {
    /// List of recently used directories
    pub recent_directories: Vec<PathBuf>,
    /// Whether the picker is currently open
    pub is_open: bool,
    /// Text input for manual path entry
    path_input: String,
    /// Pending async folder picker result
    pending_folder_pick: Option<std::sync::mpsc::Receiver<Option<PathBuf>>>,
}

impl Default for DirectoryPicker {
    fn default() -> Self {
        Self::new()
    }
}

impl DirectoryPicker {
    pub fn new() -> Self {
        Self {
            recent_directories: Vec::new(),
            is_open: false,
            path_input: String::new(),
            pending_folder_pick: None,
        }
    }

    /// Open the picker
    pub fn open(&mut self) {
        self.is_open = true;
        self.path_input.clear();
    }

    /// Close the picker
    pub fn close(&mut self) {
        self.is_open = false;
        self.pending_folder_pick = None;
    }

    /// Add a directory to the recent list
    pub fn add_recent(&mut self, path: PathBuf) {
        // Remove if already exists (we'll re-add at front)
        self.recent_directories.retain(|p| p != &path);
        // Add to front
        self.recent_directories.insert(0, path);
        // Trim to max size
        self.recent_directories.truncate(MAX_RECENT_DIRECTORIES);
    }

    /// Check for pending folder picker result
    fn check_pending_pick(&mut self) -> Option<PathBuf> {
        if let Some(rx) = &self.pending_folder_pick {
            match rx.try_recv() {
                Ok(Some(path)) => {
                    self.pending_folder_pick = None;
                    return Some(path);
                }
                Ok(None) => {
                    // User cancelled the dialog
                    self.pending_folder_pick = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.pending_folder_pick = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // Still waiting
                }
            }
        }
        None
    }

    /// Render the directory picker UI
    /// Returns an action if one was triggered
    pub fn ui(&mut self, ctx: &egui::Context) -> Option<DirectoryPickerAction> {
        if !self.is_open {
            return None;
        }

        // Check for pending folder pick result first
        if let Some(path) = self.check_pending_pick() {
            self.close();
            return Some(DirectoryPickerAction::DirectorySelected(path));
        }

        let mut action = None;

        egui::Window::new("Select Working Directory")
            .collapsible(false)
            .resizable(true)
            .default_width(400.0)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.add_space(8.0);

                // Recent directories section
                if !self.recent_directories.is_empty() {
                    ui.label(RichText::new("Recent Directories").strong());
                    ui.add_space(4.0);

                    egui::ScrollArea::vertical()
                        .max_height(200.0)
                        .show(ui, |ui| {
                            for path in &self.recent_directories.clone() {
                                let display = abbreviate_path(path);
                                let response = ui.add(
                                    egui::Button::new(RichText::new(&display).monospace())
                                        .min_size(Vec2::new(ui.available_width(), 28.0))
                                        .fill(Color32::TRANSPARENT),
                                );

                                if response
                                    .on_hover_text(path.display().to_string())
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .clicked()
                                {
                                    action = Some(DirectoryPickerAction::DirectorySelected(
                                        path.clone(),
                                    ));
                                }
                            }
                        });

                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(8.0);
                }

                // Browse button
                ui.horizontal(|ui| {
                    if ui
                        .button(RichText::new("Browse...").size(14.0))
                        .on_hover_text("Open folder picker dialog")
                        .clicked()
                    {
                        // Spawn async folder picker
                        let (tx, rx) = std::sync::mpsc::channel();
                        let ctx_clone = ctx.clone();
                        std::thread::spawn(move || {
                            let result = rfd::FileDialog::new().pick_folder();
                            let _ = tx.send(result);
                            ctx_clone.request_repaint();
                        });
                        self.pending_folder_pick = Some(rx);
                    }

                    if self.pending_folder_pick.is_some() {
                        ui.spinner();
                        ui.label("Opening dialog...");
                    }
                });

                ui.add_space(8.0);

                // Manual path input
                ui.label("Or enter path:");
                ui.horizontal(|ui| {
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut self.path_input)
                            .hint_text("/path/to/project")
                            .desired_width(ui.available_width() - 50.0),
                    );

                    if ui.button("Go").clicked()
                        || response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        let path = PathBuf::from(&self.path_input);
                        if path.exists() && path.is_dir() {
                            action = Some(DirectoryPickerAction::DirectorySelected(path));
                        }
                    }
                });

                ui.add_space(12.0);

                // Cancel button
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        action = Some(DirectoryPickerAction::Cancelled);
                    }
                });
            });

        // Handle Escape key to cancel
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            action = Some(DirectoryPickerAction::Cancelled);
        }

        // Close picker if action taken
        if action.is_some() {
            self.close();
        }

        action
    }
}

/// Abbreviate a path for display (e.g., replace home dir with ~)
fn abbreviate_path(path: &PathBuf) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(relative) = path.strip_prefix(&home) {
            return format!("~/{}", relative.display());
        }
    }
    path.display().to_string()
}
