//! The Adventure view. A placeholder until its card lands.

use super::{ViewCx, placeholder};
use crate::View;

/// Draw the Adventure view.
pub(crate) fn ui(cx: &mut ViewCx<'_>, ui: &mut egui::Ui) {
    placeholder(
        cx,
        ui,
        View::Adventure,
        "Skills and XP earned from better cashflow.",
    );
}
