//! UI component for selecting resumable Claude sessions.

use crate::path_utils::abbreviate_path;
use crate::session_discovery::{discover_sessions, format_relative_time, ResumableSession};
use crate::ui::keybind_hint::paint_keybind_hint;
use egui::{RichText, Vec2};
use std::path::{Path, PathBuf};

/// Maximum number of sessions to display
const MAX_SESSIONS_DISPLAYED: usize = 10;

/// Actions that can be triggered from the session picker
#[derive(Debug, Clone)]
pub enum SessionPickerAction {
    /// User selected a session to resume
    ResumeSession {
        cwd: PathBuf,
        session_id: String,
        title: String,
        /// Path to the JSONL file for archive conversion
        file_path: PathBuf,
    },
    /// User wants to start a new session (no resume)
    NewSession { cwd: PathBuf },
    /// User cancelled and wants to go back to directory picker
    BackToDirectoryPicker,
}

/// State for the session picker modal
pub struct SessionPicker {
    /// The working directory we're showing sessions for
    cwd: Option<PathBuf>,
    /// Cached list of resumable sessions
    sessions: Vec<ResumableSession>,
    /// Whether the picker is currently open
    pub is_open: bool,
}

impl Default for SessionPicker {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionPicker {
    pub fn new() -> Self {
        Self {
            cwd: None,
            sessions: Vec::new(),
            is_open: false,
        }
    }

    /// Open the picker for a specific working directory
    pub fn open(&mut self, cwd: PathBuf) {
        self.sessions = discover_sessions(&cwd);
        self.cwd = Some(cwd);
        self.is_open = true;
    }

    /// Close the picker
    pub fn close(&mut self) {
        self.is_open = false;
        self.cwd = None;
        self.sessions.clear();
    }

    /// Check if there are sessions available to resume
    pub fn has_sessions(&self) -> bool {
        !self.sessions.is_empty()
    }

    /// Get the current working directory
    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// Render the session picker as a full-panel overlay
    pub fn overlay_ui(&mut self, ui: &mut egui::Ui) -> Option<SessionPickerAction> {
        let cwd = self.cwd.clone()?;

        let mut action = None;
        let is_narrow = notedeck::ui::is_narrow(ui.ctx());
        let ctrl_held = ui.input(|i| i.modifiers.ctrl);

        // Handle keyboard shortcuts for sessions (Ctrl+1-9)
        // Only trigger when Ctrl is held to avoid intercepting TextEdit input
        if ctrl_held {
            for (idx, session) in self.sessions.iter().take(9).enumerate() {
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
                    return Some(SessionPickerAction::ResumeSession {
                        cwd,
                        session_id: session.session_id.clone(),
                        title: session.summary.clone(),
                        file_path: session.file_path.clone(),
                    });
                }
            }
        }

        // Handle Ctrl+N key for new session
        // Only trigger when Ctrl is held to avoid intercepting TextEdit input
        if ctrl_held && ui.input(|i| i.key_pressed(egui::Key::N)) {
            return Some(SessionPickerAction::NewSession { cwd });
        }

        // Handle Escape key or Ctrl+B to go back
        // B key requires Ctrl to avoid intercepting TextEdit input
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            || (ctrl_held && ui.input(|i| i.key_pressed(egui::Key::B)))
        {
            return Some(SessionPickerAction::BackToDirectoryPicker);
        }

        // Full panel frame
        egui::Frame::new()
            .fill(ui.visuals().panel_fill)
            .inner_margin(egui::Margin::symmetric(if is_narrow { 16 } else { 40 }, 20))
            .show(ui, |ui| {
                // Header
                ui.horizontal(|ui| {
                    if ui.button("< Back").clicked() {
                        action = Some(SessionPickerAction::BackToDirectoryPicker);
                    }
                    ui.add_space(16.0);
                    ui.heading("Resume Session");
                });

                ui.add_space(8.0);

                // Show the cwd
                ui.label(RichText::new(abbreviate_path(&cwd)).monospace().weak());

                ui.add_space(16.0);

                // Centered content
                let max_content_width = if is_narrow {
                    ui.available_width()
                } else {
                    600.0
                };
                let available_height = ui.available_height();

                ui.allocate_ui_with_layout(
                    egui::vec2(max_content_width, available_height),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        // New session button at top
                        ui.horizontal(|ui| {
                            let new_button = egui::Button::new(
                                RichText::new("+ New Session").size(if is_narrow {
                                    16.0
                                } else {
                                    14.0
                                }),
                            )
                            .min_size(Vec2::new(
                                if is_narrow {
                                    ui.available_width() - 28.0
                                } else {
                                    150.0
                                },
                                if is_narrow { 48.0 } else { 36.0 },
                            ));

                            let response = ui.add(new_button);

                            // Show keybind hint when Ctrl is held
                            if ctrl_held {
                                let hint_center =
                                    response.rect.right_center() + egui::vec2(14.0, 0.0);
                                paint_keybind_hint(ui, hint_center, "N", 18.0);
                            }

                            if response
                                .on_hover_text("Start a new conversation (N)")
                                .clicked()
                            {
                                action = Some(SessionPickerAction::NewSession { cwd: cwd.clone() });
                            }
                        });

                        ui.add_space(16.0);
                        ui.separator();
                        ui.add_space(12.0);

                        // Sessions list
                        if self.sessions.is_empty() {
                            ui.label(
                                RichText::new("No previous sessions found for this directory.")
                                    .weak(),
                            );
                        } else {
                            ui.label(RichText::new("Recent Sessions").strong());
                            ui.add_space(8.0);

                            let scroll_height = if is_narrow {
                                (ui.available_height() - 80.0).max(100.0)
                            } else {
                                400.0
                            };

                            egui::ScrollArea::vertical()
                                .max_height(scroll_height)
                                .show(ui, |ui| {
                                    for (idx, session) in self
                                        .sessions
                                        .iter()
                                        .take(MAX_SESSIONS_DISPLAYED)
                                        .enumerate()
                                    {
                                        let button_height = if is_narrow { 64.0 } else { 50.0 };
                                        let hint_width =
                                            if ctrl_held && idx < 9 { 24.0 } else { 0.0 };
                                        let button_width = ui.available_width() - hint_width - 4.0;

                                        ui.horizontal(|ui| {
                                            // Create a frame for the session button
                                            let response = ui.add(
                                                egui::Button::new("")
                                                    .min_size(Vec2::new(
                                                        button_width,
                                                        button_height,
                                                    ))
                                                    .fill(
                                                        ui.visuals().widgets.inactive.weak_bg_fill,
                                                    ),
                                            );

                                            // Draw the content over the button
                                            let rect = response.rect;
                                            let painter = ui.painter();

                                            // Summary text (truncated)
                                            let summary_text = &session.summary;
                                            let text_color = ui.visuals().text_color();
                                            painter.text(
                                                rect.left_top() + egui::vec2(8.0, 8.0),
                                                egui::Align2::LEFT_TOP,
                                                summary_text,
                                                egui::FontId::proportional(13.0),
                                                text_color,
                                            );

                                            // Metadata line (time + message count)
                                            let meta_text = format!(
                                                "{} • {} messages",
                                                format_relative_time(&session.last_timestamp),
                                                session.message_count
                                            );
                                            painter.text(
                                                rect.left_bottom() + egui::vec2(8.0, -8.0),
                                                egui::Align2::LEFT_BOTTOM,
                                                meta_text,
                                                egui::FontId::proportional(11.0),
                                                ui.visuals().weak_text_color(),
                                            );

                                            // Show keybind hint when Ctrl is held
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

                                            if response.clicked() {
                                                action = Some(SessionPickerAction::ResumeSession {
                                                    cwd: cwd.clone(),
                                                    session_id: session.session_id.clone(),
                                                    title: session.summary.clone(),
                                                    file_path: session.file_path.clone(),
                                                });
                                            }
                                        });

                                        ui.add_space(4.0);
                                    }

                                    if self.sessions.len() > MAX_SESSIONS_DISPLAYED {
                                        ui.add_space(8.0);
                                        ui.label(
                                            RichText::new(format!(
                                                "... and {} more sessions",
                                                self.sessions.len() - MAX_SESSIONS_DISPLAYED
                                            ))
                                            .weak(),
                                        );
                                    }
                                });
                        }
                    },
                );
            });

        action
    }
}
