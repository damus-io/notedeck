//! The Trends view. A placeholder until its card lands.

use super::{ViewCx, placeholder};
use crate::View;

/// Draw the Trends view.
pub(crate) fn ui(cx: &mut ViewCx<'_>, ui: &mut egui::Ui) {
    placeholder(cx, ui, View::Trends, "Categories and balances over time.");
}
