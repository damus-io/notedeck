//! The Projects view. A placeholder until its card lands.

use super::{ViewCx, placeholder};
use crate::View;

/// Draw the Projects view.
pub(crate) fn ui(cx: &mut ViewCx<'_>, ui: &mut egui::Ui) {
    placeholder(cx, ui, View::Projects, "Revenue and cost per project.");
}
