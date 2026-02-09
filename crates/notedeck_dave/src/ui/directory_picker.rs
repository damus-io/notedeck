use crate::ui::keybind_hint::paint_keybind_hint;
use egui::{RichText, Vec2};
use std::path::{Path, PathBuf};

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

    /// Render the directory picker as a full-panel overlay
    /// `has_sessions` indicates whether there are existing sessions (enables cancel)
    pub fn overlay_ui(
        &mut self,
        ui: &mut egui::Ui,
        has_sessions: bool,
    ) -> Option<DirectoryPickerAction> {
        // Check for pending folder pick result first
        if let Some(path) = self.check_pending_pick() {
            return Some(DirectoryPickerAction::DirectorySelected(path));
        }

        let mut action = None;
        let is_narrow = notedeck::ui::is_narrow(ui.ctx());
        let ctrl_held = ui.input(|i| i.modifiers.ctrl);

        // Handle keyboard shortcuts for recent directories (Ctrl+1-9)
        // Only trigger when Ctrl is held to avoid intercepting TextEdit input
        if ctrl_held {
            for (idx, path) in self.recent_directories.iter().take(9).enumerate() {
                let key = match idx {
                    0 => egui::Key::Num1,
                    1 => egui::Key::Num2,
                    2 => egui::Key::Num3,
                    3 => egui::Key::Num4,
                    4 => egui::Key::Num5,
                    5 => egui::Key::Num6,
                    6 => egui::Key::Num7,
                    7 => egui::Key::Num8,
                    8 => egui::Key::Num9,
                    _ => continue,
                };
                if ui.input(|i| i.key_pressed(key)) {
                    return Some(DirectoryPickerAction::DirectorySelected(path.clone()));
                }
            }
        }

        // Handle Ctrl+B key for browse (track whether we need to trigger it)
        // Only trigger when Ctrl is held to avoid intercepting TextEdit input
        let trigger_browse = ctrl_held
            && ui.input(|i| i.key_pressed(egui::Key::B))
            && self.pending_folder_pick.is_none();

        // Full panel frame
        egui::Frame::new()
            .fill(ui.visuals().panel_fill)
            .inner_margin(egui::Margin::symmetric(if is_narrow { 16 } else { 40 }, 20))
            .show(ui, |ui| {
                // Header
                ui.horizontal(|ui| {
                    // Only show back button if there are existing sessions
                    if has_sessions {
                        if ui.button("< Back").clicked() {
                            action = Some(DirectoryPickerAction::Cancelled);
                        }
                        ui.add_space(16.0);
                    }
                    ui.heading("Select Working Directory");
                });

                ui.add_space(16.0);

                // Centered content (max width for desktop)
                let max_content_width = if is_narrow {
                    ui.available_width()
                } else {
                    500.0
                };
                let available_height = ui.available_height();

                ui.allocate_ui_with_layout(
                    egui::vec2(max_content_width, available_height),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        // Recent directories section
                        if !self.recent_directories.is_empty() {
                            ui.label(RichText::new("Recent Directories").strong());
                            ui.add_space(8.0);

                            // Use more vertical space on mobile
                            let scroll_height = if is_narrow {
                                (ui.available_height() - 150.0).max(100.0)
                            } else {
                                300.0
                            };

                            egui::ScrollArea::vertical()
                                .max_height(scroll_height)
                                .show(ui, |ui| {
                                    for (idx, path) in
                                        self.recent_directories.clone().iter().enumerate()
                                    {
                                        let display = abbreviate_path(path);

                                        // Full-width button style with larger touch targets on mobile
                                        let button_height = if is_narrow { 44.0 } else { 32.0 };
                                        let hint_width =
                                            if ctrl_held && idx < 9 { 24.0 } else { 0.0 };
                                        let button_width = ui.available_width() - hint_width - 4.0;

                                        ui.horizontal(|ui| {
                                            let button = egui::Button::new(
                                                RichText::new(&display).monospace(),
                                            )
                                            .min_size(Vec2::new(button_width, button_height))
                                            .fill(ui.visuals().widgets.inactive.weak_bg_fill);

                                            let response = ui.add(button);

                                            // Show keybind hint when Ctrl is held (for first 9 items)
                                            if ctrl_held && idx < 9 {
                                                let hint_text = format!("{}", idx + 1);
                                                let hint_center = response.rect.right_center()
                                                    + egui::vec2(hint_width / 2.0 + 2.0, 0.0);
                                                paint_keybind_hint(
                                                    ui,
                                                    hint_center,
                                                    &hint_text,
                                                    18.0,
                                                );
                                            }

                                            if response
                                                .on_hover_text(path.display().to_string())
                                                .clicked()
                                            {
                                                action =
                                                    Some(DirectoryPickerAction::DirectorySelected(
                                                        path.clone(),
                                                    ));
                                            }
                                        });

                                        ui.add_space(4.0);
                                    }
                                });

                            ui.add_space(16.0);
                            ui.separator();
                            ui.add_space(12.0);
                        }

                        // Browse button (larger touch target on mobile)
                        ui.horizontal(|ui| {
                            let browse_button =
                                egui::Button::new(RichText::new("Browse...").size(if is_narrow {
                                    16.0
                                } else {
                                    14.0
                                }))
                                .min_size(Vec2::new(
                                    if is_narrow {
                                        ui.available_width() - 28.0
                                    } else {
                                        120.0
                                    },
                                    if is_narrow { 48.0 } else { 32.0 },
                                ));

                            let response = ui.add(browse_button);

                            // Show keybind hint when Ctrl is held
                            if ctrl_held {
                                let hint_center =
                                    response.rect.right_center() + egui::vec2(14.0, 0.0);
                                paint_keybind_hint(ui, hint_center, "B", 18.0);
                            }

                            #[cfg(any(
                                target_os = "windows",
                                target_os = "macos",
                                target_os = "linux"
                            ))]
                            if response
                                .on_hover_text("Open folder picker dialog (B)")
                                .clicked()
                                || trigger_browse
                            {
                                // Spawn async folder picker
                                let (tx, rx) = std::sync::mpsc::channel();
                                let ctx_clone = ui.ctx().clone();
                                std::thread::spawn(move || {
                                    let result = rfd::FileDialog::new().pick_folder();
                                    let _ = tx.send(result);
                                    ctx_clone.request_repaint();
                                });
                                self.pending_folder_pick = Some(rx);
                            }

                            // On platforms without rfd (e.g., Android), just show the button disabled
                            #[cfg(not(any(
                                target_os = "windows",
                                target_os = "macos",
                                target_os = "linux"
                            )))]
                            {
                                let _ = response;
                                let _ = trigger_browse;
                            }
                        });

                        if self.pending_folder_pick.is_some() {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("Opening dialog...");
                            });
                        }

                        ui.add_space(16.0);

                        // Manual path input
                        ui.label("Or enter path:");
                        ui.add_space(4.0);

                        let response = ui.add(
                            egui::TextEdit::singleline(&mut self.path_input)
                                .hint_text("/path/to/project")
                                .desired_width(ui.available_width()),
                        );

                        ui.add_space(8.0);

                        let go_button = egui::Button::new("Go").min_size(Vec2::new(
                            if is_narrow {
                                ui.available_width()
                            } else {
                                50.0
                            },
                            if is_narrow { 44.0 } else { 28.0 },
                        ));

                        if ui.add(go_button).clicked()
                            || response.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            let path = PathBuf::from(&self.path_input);
                            if path.exists() && path.is_dir() {
                                action = Some(DirectoryPickerAction::DirectorySelected(path));
                            }
                        }
                    },
                );
            });

        // Handle Escape key (only if cancellation is allowed)
        if has_sessions && ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            action = Some(DirectoryPickerAction::Cancelled);
        }

        action
    }
}

/// Abbreviate a path for display (e.g., replace home dir with ~)
fn abbreviate_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(relative) = path.strip_prefix(&home) {
            return format!("~/{}", relative.display());
        }
    }
    path.display().to_string()
}
