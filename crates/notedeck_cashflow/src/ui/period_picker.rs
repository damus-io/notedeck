//! The period picker: one chip per [`Period`], the shared period selected.

use egui::RichText;

use super::palette::Palette;
use crate::{Period, View};

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

/// A view's heading with the period chips beside it, or, on a phone, under
/// it on a row of their own that scrolls rather than widening the page.
/// Returns whether the period changed.
pub fn heading(
    ui: &mut egui::Ui,
    palette: &Palette,
    view: View,
    period: &mut Period,
    narrow: bool,
) -> bool {
    let heading = RichText::new(view.label()).heading().color(palette.text);
    if !narrow {
        return ui
            .horizontal(|ui| {
                ui.label(heading);
                ui.add_space(16.0);
                show(ui, palette, period)
            })
            .inner;
    }
    ui.label(heading);
    ui.add_space(4.0);
    egui::ScrollArea::horizontal()
        .id_salt("cashflow_period_picker")
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .show(ui, |ui| show(ui, palette, period))
        .inner
}
