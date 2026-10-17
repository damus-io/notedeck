//! The Categories view. A placeholder until its card lands.

use super::{ViewCx, placeholder};
use crate::View;

/// Draw the Categories view.
pub(crate) fn ui(cx: &mut ViewCx<'_>, ui: &mut egui::Ui) {
    placeholder(
        cx,
        ui,
        View::Categories,
        "Where money comes from and goes, by category.",
    );
}
