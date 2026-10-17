//! The view nav: a rail down the left on wide screens, a scrolling strip
//! across the top on narrow ones.

use egui::{RichText, Sense};

use super::{palette::Palette, panel_frame};
use crate::View;

/// Width of the nav rail on wide screens.
const RAIL_WIDTH: f32 = 180.0;

/// Height of one entry in the nav rail.
const RAIL_ROW_HEIGHT: f32 = 32.0;

/// Gap between a rail row's left edge and its label.
const ROW_TEXT_INSET: f32 = 12.0;

/// Draw the nav as a panel of `ui` and return the view the user picked this
/// frame, if any. `current` is drawn selected.
#[profiling::function]
pub fn show(ui: &mut egui::Ui, palette: &Palette, current: View, narrow: bool) -> Option<View> {
    if narrow {
        return egui::Panel::top("cashflow_nav")
            .frame(panel_frame(palette).inner_margin(egui::Margin::symmetric(8, 6)))
            .show(ui, |ui| strip(ui, palette, current))
            .inner;
    }

    egui::Panel::left("cashflow_nav")
        .resizable(false)
        .exact_size(RAIL_WIDTH)
        .frame(
            egui::Frame::new()
                .fill(palette.surface)
                .inner_margin(egui::Margin::symmetric(8, 16)),
        )
        .show(ui, |ui| rail(ui, palette, current))
        .inner
}

/// The wide layout: the app's name, then one full-width row per view.
fn rail(ui: &mut egui::Ui, palette: &Palette, current: View) -> Option<View> {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        // Line the name up with the row labels below it.
        ui.add_space(ROW_TEXT_INSET);
        ui.label(RichText::new("Cashflow").heading().color(palette.text));
    });
    ui.add_space(12.0);

    let mut picked = None;
    for view in View::ALL {
        if rail_row(ui, palette, view, view == current).clicked() {
            picked = Some(view);
        }
    }
    picked
}

/// One rail entry: a rounded highlight under the selected or hovered view.
fn rail_row(ui: &mut egui::Ui, palette: &Palette, view: View, selected: bool) -> egui::Response {
    let size = egui::vec2(ui.available_width(), RAIL_ROW_HEIGHT);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::Button, true, selected, view.label())
    });

    let painter = ui.painter_at(rect);
    if selected {
        painter.rect_filled(rect, 6.0, palette.accent.gamma_multiply(0.25));
    } else if resp.hovered() {
        painter.rect_filled(rect, 6.0, palette.border);
    }

    let color = if selected {
        palette.text
    } else {
        palette.text_muted
    };
    painter.text(
        rect.left_center() + egui::vec2(ROW_TEXT_INSET, 0.0),
        egui::Align2::LEFT_CENTER,
        view.label(),
        egui::TextStyle::Button.resolve(ui.style()),
        color,
    );
    resp
}

/// The narrow layout: every view as a tab in one horizontally scrolling row.
fn strip(ui: &mut egui::Ui, palette: &Palette, current: View) -> Option<View> {
    let mut picked = None;
    egui::ScrollArea::horizontal()
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for view in View::ALL {
                    let selected = view == current;
                    let color = if selected {
                        palette.text
                    } else {
                        palette.text_muted
                    };
                    let tab = egui::Button::selectable(
                        selected,
                        RichText::new(view.label()).color(color),
                    );
                    if ui.add(tab).clicked() {
                        picked = Some(view);
                    }
                }
            });
        });
    picked
}
