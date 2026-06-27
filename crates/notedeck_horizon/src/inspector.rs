//! The right pane: details for the currently selected event.

use crate::block::Block;
use crate::theme;
use egui::{RichText, vec2};

pub(crate) fn show(ui: &mut egui::Ui, blocks: &[Block], selected: Option<usize>) {
    ui.add_space(10.0);
    let Some(block) = selected.and_then(|i| blocks.get(i)) else {
        ui.vertical_centered(|ui| {
            ui.add_space(40.0);
            ui.label(RichText::new("No event selected").color(theme::TEXT_WEAK));
        });
        return;
    };

    ui.label(
        RichText::new(&block.title)
            .size(20.0)
            .strong()
            .color(theme::TEXT),
    );
    ui.add_space(2.0);
    ui.label(RichText::new("Add Location").color(theme::TEXT_WEAK));
    ui.add_space(12.0);

    let tz = block.start.format("%Z").to_string();

    field(ui, "all-day", if block.all_day { "Yes" } else { "No" });
    if block.all_day {
        field(ui, "date", &block.start.format("%Y-%m-%d").to_string());
    } else {
        field(
            ui,
            "starts",
            &block.start.format("%Y-%m-%d  %-I:%M %p").to_string(),
        );
        field(
            ui,
            "ends",
            &block.end.format("%Y-%m-%d  %-I:%M %p").to_string(),
        );
        field(ui, "time zone", &tz);
    }
    field_swatch(ui, "calendar", "Personal", block.color);
    field(ui, "color", "None");
    field(ui, "repeat", "Never");
    field(ui, "alert", "None");
    field(ui, "show as", "Busy");
}

/// A right-aligned dim field name with its value, matching the reference layout.
fn field(ui: &mut egui::Ui, name: &str, value: &str) {
    ui.horizontal(|ui| {
        label_col(ui, name);
        ui.label(RichText::new(value).color(theme::TEXT));
    });
    ui.add_space(7.0);
}

/// A field whose value is preceded by a small calendar color swatch.
fn field_swatch(ui: &mut egui::Ui, name: &str, value: &str, color: egui::Color32) {
    ui.horizontal(|ui| {
        label_col(ui, name);
        let (dot, _) = ui.allocate_exact_size(vec2(12.0, 12.0), egui::Sense::hover());
        ui.painter().circle_filled(dot.center(), 5.0, color);
        ui.label(RichText::new(value).color(theme::TEXT));
    });
    ui.add_space(7.0);
}

/// Fixed-width right-aligned column for the field name.
fn label_col(ui: &mut egui::Ui, name: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(78.0, 16.0), egui::Sense::hover());
    ui.painter().text(
        egui::pos2(rect.right(), rect.center().y),
        egui::Align2::RIGHT_CENTER,
        name,
        egui::FontId::proportional(13.0),
        theme::TEXT_WEAK,
    );
}
