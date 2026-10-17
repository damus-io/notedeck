//! The Settings view. A placeholder until its card lands.

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
}
