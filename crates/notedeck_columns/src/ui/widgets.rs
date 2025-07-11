use egui::Widget;
use notedeck_ui::widgets::styled_button_toggleable;

/// Sized and styled to match the figma design
pub fn styled_button(text: &str, fill_color: egui::Color32) -> impl Widget + '_ {
    styled_button_toggleable(text, fill_color, true)
}
