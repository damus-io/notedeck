//! The Overview view. A placeholder until its card lands.

use super::{ViewCx, placeholder};
use crate::View;

/// Draw the Overview view.
pub(crate) fn ui(cx: &mut ViewCx<'_>, ui: &mut egui::Ui) {
    placeholder(
        cx,
        ui,
        View::Overview,
        "Net worth, this month's money in and out, and the headline numbers.",
    );
}
