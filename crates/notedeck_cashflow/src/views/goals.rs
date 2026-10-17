//! The Goals view. A placeholder until its card lands.

use super::{ViewCx, placeholder};
use crate::View;

/// Draw the Goals view.
pub(crate) fn ui(cx: &mut ViewCx<'_>, ui: &mut egui::Ui) {
    placeholder(
        cx,
        ui,
        View::Goals,
        "Savings and spending targets, tracked as Headway cards.",
    );
}
