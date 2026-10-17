//! The Transactions view. A placeholder until its card lands.

use super::{ViewCx, placeholder};
use crate::View;

/// Draw the Transactions view.
pub(crate) fn ui(cx: &mut ViewCx<'_>, ui: &mut egui::Ui) {
    placeholder(
        cx,
        ui,
        View::Transactions,
        "Every transaction, and an inbox for the ones not yet categorized.",
    );
}
