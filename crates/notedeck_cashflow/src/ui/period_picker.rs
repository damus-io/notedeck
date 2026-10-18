//! The period picker: one chip per [`Period`], the shared period selected.

use egui::RichText;

use super::palette::Palette;
use crate::Period;

/// Draw a chip per period into `ui`, set `period` to the one clicked, and
/// return whether it changed.
pub fn show(ui: &mut egui::Ui, palette: &Palette, period: &mut Period) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for choice in Period::ALL {
            let selected = choice == *period;
            let color = if selected {
                palette.text
            } else {
                palette.text_muted
            };
            let chip =
                egui::Button::selectable(selected, RichText::new(choice.label()).color(color));
            if ui.add(chip).clicked() && !selected {
                *period = choice;
                changed = true;
            }
        }
    });
    changed
}
