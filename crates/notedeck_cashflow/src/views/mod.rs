//! One module per [`View`], and the dispatch from a view to its module.
//!
//! Every view has the same shape — `pub(crate) fn ui(cx: &mut ViewCx<'_>, ui)`
//! — so the router is one `match` and a new view is a new module plus an arm
//! here.

use egui::RichText;

use crate::{View, ui::palette::Palette};

mod adventure;
mod categories;
mod goals;
mod overview;
mod projects;
mod settings;
mod transactions;
mod trends;

/// What a view draws from. Every view takes this one struct, so handing views
/// something new (the ledger, the host's `AppContext`) is a field here, not a
/// new parameter on every view.
pub struct ViewCx<'a> {
    /// The colors to draw with.
    pub palette: &'a Palette,
}

/// Draw `view` into `ui`.
#[profiling::function]
pub fn show(view: View, cx: &mut ViewCx<'_>, ui: &mut egui::Ui) {
    match view {
        View::Overview => overview::ui(cx, ui),
        View::Categories => categories::ui(cx, ui),
        View::Trends => trends::ui(cx, ui),
        View::Transactions => transactions::ui(cx, ui),
        View::Projects => projects::ui(cx, ui),
        View::Goals => goals::ui(cx, ui),
        View::Adventure => adventure::ui(cx, ui),
        View::Settings => settings::ui(cx, ui),
    }
}

/// A view that has no content yet: its heading and one line on what it will
/// show.
fn placeholder(cx: &ViewCx<'_>, ui: &mut egui::Ui, view: View, what: &'static str) {
    ui.label(RichText::new(view.label()).heading().color(cx.palette.text));
    ui.add_space(8.0);
    ui.label(RichText::new(what).color(cx.palette.text_muted));
}
