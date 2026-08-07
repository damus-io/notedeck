use egui::{RichText, Vec2};

/// Actions from the new-session-kind chooser overlay.
#[derive(Debug, Clone)]
pub enum SessionKindAction {
    /// Start a lightweight local chat session.
    Chat,
    /// Start an agentic session (routes on to the host picker to spawn on a
    /// remote host).
    Agentic,
    /// User cancelled.
    Cancelled,
}

/// Render the new-session-kind chooser as a full-panel overlay.
///
/// Shown when remote agentic sessions already exist but this device has no
/// local agentic backend (e.g. Android), so the user can choose between a local
/// chat and spawning a remote agentic session rather than the client silently
/// assuming one based on local capability.
pub fn session_kind_picker_overlay_ui(
    ui: &mut egui::Ui,
    has_sessions: bool,
) -> Option<SessionKindAction> {
    let mut action = None;
    let is_narrow = notedeck::ui::is_narrow(ui.ctx());

    egui::Frame::new()
        .fill(ui.visuals().panel_fill)
        .inner_margin(egui::Margin::symmetric(if is_narrow { 16 } else { 40 }, 20))
        .show(ui, |ui| {
            // Header
            ui.horizontal(|ui| {
                if has_sessions {
                    if ui.button("< Back").clicked() {
                        action = Some(SessionKindAction::Cancelled);
                    }
                    ui.add_space(16.0);
                }
                ui.heading("New Session");
            });

            ui.add_space(16.0);

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
                    let button_height = if is_narrow { 44.0 } else { 32.0 };
                    let button_width = ui.available_width() - 4.0;

                    // Local chat option
                    let chat = egui::Button::new(RichText::new("💬  Chat").monospace())
                        .min_size(Vec2::new(button_width, button_height))
                        .fill(ui.visuals().widgets.inactive.weak_bg_fill);
                    if ui
                        .add(chat)
                        .on_hover_text("Start a local chat with the AI")
                        .clicked()
                    {
                        action = Some(SessionKindAction::Chat);
                    }

                    ui.add_space(8.0);

                    // Remote agentic session option
                    let agentic = egui::Button::new(RichText::new("🖥  Remote session").monospace())
                        .min_size(Vec2::new(button_width, button_height))
                        .fill(ui.visuals().widgets.inactive.weak_bg_fill);
                    if ui
                        .add(agentic)
                        .on_hover_text("Spawn an agentic session on a remote host")
                        .clicked()
                    {
                        action = Some(SessionKindAction::Agentic);
                    }
                },
            );
        });

    // Escape to cancel
    if has_sessions
        && ui
            .ctx()
            .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        action = Some(SessionKindAction::Cancelled);
    }

    action
}
