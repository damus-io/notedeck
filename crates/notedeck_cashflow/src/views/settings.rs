//! The Settings view. A placeholder until its card lands, showing what the
//! journal holds.

use egui::RichText;

use super::{ViewCx, placeholder};
use crate::View;

/// Draw the Settings view.
pub(crate) fn ui(cx: &mut ViewCx<'_>, ui: &mut egui::Ui) {
    placeholder(
        cx,
        ui,
        View::Settings,
        "Layers, home currency, and journal and sync status.",
    );
    if cx.summary.is_empty() {
        return;
    }
    ui.add_space(16.0);
    ui.label(RichText::new("Journal").strong().color(cx.palette.text));
    ui.label(RichText::new(cx.summary).color(cx.palette.text_muted));
}
